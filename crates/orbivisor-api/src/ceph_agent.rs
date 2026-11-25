//! Ceph 클라이언트 설정 에이전트(계획 56): 노드마다 `Ceph` 리소스를 지켜보며 관리 Ceph에 붙는 클라이언트
//! 설정(`{client_dir}/ceph.conf`·`ceph.client.orbivisor.keyring`)을 쓰고, 이 노드가 데몬 노드(mon·mgr·OSD)면
//! Orbivisor가 쓴 `/etc/ceph/ceph.conf`의 `mon_host`도 맞춘다. 리소스가 생기거나 바뀌거나 지워질 때마다 다시
//! 맞추고, 내용이 같으면 쓰지 않는다. 리소스가 없거나 mon이 아직 없으면 아무것도 하지 않는다(이미 쓴 파일은 남김).

use std::sync::Arc;
use std::time::Duration;

use orbivisor_cluster::Cluster;
use orbivisor_cluster::watch::{Watch, WatchEvent};
use orbivisor_driver::ceph::{CephHost, is_managed, render_client_conf, render_conf};
use orbivisor_types::ceph::{CEPH_NAME, CephSpec};
use orbivisor_types::node::NodeSpec;
use orbivisor_types::resource::Kind;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// 구독이 끊긴 뒤 다시 구독하기까지 기다리는 시간
const RESUBSCRIBE: Duration = Duration::from_secs(10);

/// 이 노드의 Ceph 설정 파일을 `Ceph` 리소스에 맞추는 에이전트. 위트니스가 아닌 노드에서 돈다.
pub struct CephAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl CephAgent {
    pub fn spawn(cluster: Arc<Cluster>, node: Uuid, host: Arc<CephHost>) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, node, host, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    cluster: Arc<Cluster>,
    node: Uuid,
    host: Arc<CephHost>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut watch: Option<Watch> = None;
    loop {
        // 이번에 읽을 리소스 뒤의 변경을 놓치지 않게 먼저 구독한다.
        if watch.is_none() {
            watch = cluster
                .list::<CephSpec>()
                .ok()
                .and_then(|(revision, _)| cluster.watch(revision).ok());
        }
        // 파일 읽기·쓰기·fsync는 막는 I/O라 블로킹 스레드에서 한다.
        let (c, h) = (cluster.clone(), host.clone());
        if let Err(e) = tokio::task::spawn_blocking(move || sync(&c, node, &h)).await {
            tracing::warn!(error = %e, "the Ceph config sync panicked");
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = ceph_changed(&mut watch) => {}
        }
    }
}

/// `Ceph` 리소스가 바뀔 때까지 기다린다. 구독이 끊기면 버리고 돌아가 다시 맞추게 한다. 구독이 없으면 잠시 뒤
/// 돌아가 다시 구독하게 한다.
async fn ceph_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return tokio::time::sleep(RESUBSCRIBE).await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Ceph, ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Ceph, ..
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

/// 이 노드의 파일을 지금 리소스에 맞춘다. 실패는 로그만 남긴다(다음 변경 때 다시 한다).
fn sync(cluster: &Cluster, node: Uuid, host: &CephHost) {
    // Ceph는 Linux 노드에만 세운다.
    if host.check_platform().is_err() {
        return;
    }
    let ceph = match cluster.get_by_name::<CephSpec>(CEPH_NAME) {
        Ok(Some(ceph)) => ceph,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the Ceph cluster");
            return;
        }
    };
    let status = &ceph.status;
    if status.mons.is_empty() {
        return;
    }
    match cluster.get::<NodeSpec>(node) {
        Ok(Some(me)) if !me.spec.witness => {}
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read this node");
            return;
        }
    }
    let keyring = host.client_keyring().to_string_lossy().into_owned();
    let client = render_client_conf(&ceph.spec, &status.mons, &keyring);
    match host.write_client(&client, &ceph.spec.keys.client) {
        Ok(true) => {
            tracing::info!(path = %host.client_conf().display(), "wrote the Ceph client config")
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "cannot write the Ceph client config"),
    }
    let daemon = status.mons.iter().any(|m| m.node == node)
        || status.mgrs.contains(&node)
        || status.osd_nodes.contains(&node);
    if !daemon {
        return;
    }
    // 데몬 설정은 Orbivisor가 쓴 파일만 다시 쓴다(처음 쓰는 것은 Task다). 이 노드에 mon을 더하는 Task가
    // 도는 동안(이 노드가 이미 mgr·OSD 노드면) 여기서 쓰는 conf에는 아직 정족수에 들지 않은 그 mon이 없다. mon
    // 저장소는 monmap을 따로 가지므로 상관없고, Task가 끝나 `mons`에 오르면 다시 맞춘다.
    let path = host.paths().etc_ceph.join("ceph.conf");
    match std::fs::read_to_string(&path) {
        Ok(text) if is_managed(&text) => {}
        Ok(_) => {
            tracing::warn!(
                path = %path.display(),
                "not updating a Ceph config that Orbivisor did not write"
            );
            return;
        }
        Err(_) => return,
    }
    match host.write_conf(&render_conf(&ceph.spec, &status.mons, None)) {
        Ok(true) => tracing::info!(path = %path.display(), "updated the Ceph monitors"),
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "cannot write the Ceph config"),
    }
}
