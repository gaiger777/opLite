//! 이 노드가 백업 저장소(계획 68)에 닿는지 보고한다(설계 §6 `NodeStatus.backup_stores`). 60초마다 이 노드가 쓸 수
//! 있는 준비된 저장소마다 `store.json`을 읽어 본다(백업 풀 안 저장소는 풀이 이 노드에서 준비돼 있고 공유 풀이거나 저장소를
//! 만든 노드일 때, S3는 위트니스가 아닐 때). 결과(닿음·까닭)가 바뀌었을 때만 노드 상태에 쓴다. 리더의 경보
//! `backup_store_unavailable`과 저장소 Task의 노드 고르기가 읽는다.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::backup_store::{BackupStore, BackupStoreSpec, NodeStoreState, StorePhase};
use opl_types::node::{NodeSpec, NodeStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// 확인 간격
pub const CHECK_EVERY: Duration = Duration::from_secs(60);
/// 저장소 하나를 읽는 한도(S3는 다시 하기 60초를 품는다 — 멈춘 NFS도 여기서 끊는다)
const CHECK_TIMEOUT: Duration = Duration::from_secs(75);

pub struct StoreWatch {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl StoreWatch {
    pub fn spawn(cluster: Arc<Cluster>, node: Uuid, mount_root: PathBuf, every: Duration) -> Self {
        let (shutdown, mut rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(every);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = &mut rx => return,
                    _ = interval.tick() => {}
                }
                tokio::select! {
                    _ = &mut rx => return,
                    r = check_once(&cluster, node, &mount_root) => if let Err(e) = r {
                        tracing::warn!(error = %e, "cannot report backup store reachability");
                    }
                }
            }
        });
        Self { shutdown, task }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 저장소 하나의 `store.json`을 읽어 이 저장소인지 본다.
async fn reach(
    cluster: &Cluster,
    store: &BackupStore,
    node: Uuid,
    mount_root: &std::path::Path,
) -> Result<(), String> {
    let backend = opl_api::stores::open_backend(cluster, store, node, mount_root)?;
    match opl_store::store::read_info(backend.as_ref()).await {
        Ok(Some(info)) if info.id == opl_api::stores::store_id(store) => Ok(()),
        Ok(Some(info)) => Err(format!(
            "the location holds another backup store ({})",
            info.id
        )),
        Ok(None) => Err("store.json is missing".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// 이 클러스터가 주인이고 하트비트가 [`opl_api::stores::HEARTBEAT_EVERY`]보다 오래됐으면 새로 쓴다. 썼으면 true.
async fn heartbeat(
    cluster: &Cluster,
    store: &BackupStore,
    node: Uuid,
    mount_root: &std::path::Path,
) -> Result<bool, String> {
    let (_, fp) = opl_api::stores::this_cluster(cluster)?;
    let opened = opl_api::stores::open(cluster, store, node, mount_root).await?;
    opened
        .refresh_heartbeat(&fp, store.id, now(), opl_api::stores::HEARTBEAT_EVERY)
        .await
        .map_err(|e| e.to_string())
}

/// 한 번 확인하고 바뀌었으면 쓴다.
pub async fn check_once(
    cluster: &Cluster,
    node: Uuid,
    mount_root: &std::path::Path,
) -> Result<(), ClusterError> {
    let Some(me) = cluster.get::<NodeSpec>(node)? else {
        return Ok(());
    };
    let (_, stores) = cluster.list::<BackupStoreSpec>()?;
    let mut found = Vec::new();
    for store in stores
        .iter()
        .filter(|s| s.status.phase == StorePhase::Ready)
    {
        let pool = match opl_api::stores::backend_pool(cluster, store) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !opl_api::stores::usable_on(store, pool.as_ref(), &me) {
            continue;
        }
        let r = match tokio::time::timeout(CHECK_TIMEOUT, reach(cluster, store, node, mount_root))
            .await
        {
            Ok(r) => r,
            Err(_) => Err(format!("no answer in {}s", CHECK_TIMEOUT.as_secs())),
        };
        // 리더는 이 클러스터가 주인인 쓰기 저장소의 하트비트를 새로 쓴다(리뷰 1 I4 — GC가 드물거나 실패해도 산 주인이
        // `force` 없이 넘겨받히지 않는다).
        if r.is_ok() && !store.spec.read_only && cluster.leader_term().is_some() {
            let beat = heartbeat(cluster, store, node, mount_root);
            match tokio::time::timeout(CHECK_TIMEOUT, beat).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(store = %store.name, error = %e, "cannot write the owner heartbeat")
                }
                Err(_) => {
                    tracing::warn!(store = %store.name, "writing the owner heartbeat timed out")
                }
            }
        }
        found.push(NodeStoreState {
            store: store.id,
            ok: r.is_ok(),
            error: r.err(),
            checked_at: now(),
        });
    }
    found.sort_by_key(|s| s.store);
    write(cluster, node, found).await
}

/// 닿음·까닭이 바뀐 저장소가 있을 때만 쓴다(확인 시각만 바뀌면 앞 기록을 둔다). 그사이 다른 쪽이 상태를 썼으면 다시
/// 읽고 쓴다.
async fn write(
    cluster: &Cluster,
    node: Uuid,
    found: Vec<NodeStoreState>,
) -> Result<(), ClusterError> {
    let same = |a: &[NodeStoreState], b: &[NodeStoreState]| {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| x.store == y.store && x.ok == y.ok && x.error == y.error)
    };
    loop {
        let Some(n) = cluster.get::<NodeSpec>(node)? else {
            return Ok(());
        };
        if same(&n.status.backup_stores, &found) {
            return Ok(());
        }
        let status = NodeStatus {
            backup_stores: found.clone(),
            ..n.status
        };
        match cluster
            .update_status_checked::<NodeSpec>(node, n.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::tests::single_node;
    use opl_cluster::types::NodeId;
    use opl_types::node::PoolState;
    use opl_types::resource::Resource;
    use opl_types::storage::{PoolBackend, StoragePoolSpec};

    #[tokio::test]
    async fn reports_reachable_and_missing_stores_only_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(node)) = single_node(&dir.path().join("cluster")).await;
        let pool_dir = dir.path().join("bk");
        std::fs::create_dir(&pool_dir).unwrap();
        let pool = Resource::new(
            "bk",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: pool_dir.clone(),
                },
                nodes: vec![],
                shared: false,
                backups: true,
                folder: None,
            },
        );
        cluster.create(&pool).await.unwrap();
        let mut me: opl_types::node::Node = Resource::new(
            "n1",
            NodeSpec {
                sdn: None,
                host_resources: None,
                address: "h:8006".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        me.id = node;
        me.status.online = true;
        me.status.pools.insert(
            "bk".into(),
            PoolState {
                pool_id: pool.id,
                ready: true,
                error: None,
                total_bytes: 1,
                available_bytes: 1,
            },
        );
        cluster.create(&me).await.unwrap();
        let spec: BackupStoreSpec =
            serde_json::from_value(serde_json::json!({"backend": {"pool": "bk"}})).unwrap();
        let mut store: BackupStore = Resource::new("st", spec);
        store.status.phase = StorePhase::Ready;
        store.status.node = Some(node);
        cluster.create(&store).await.unwrap();
        // 아직 준비 중인 저장소는 보지 않는다
        let mut init: BackupStore = Resource::new(
            "new",
            serde_json::from_value(serde_json::json!({"backend": {"s3": {
                "endpoint": "https://s3.example.com", "bucket": "bkt",
                "access_key_id": "a", "secret_access_key": "b"}}}))
            .unwrap(),
        );
        init.status.phase = StorePhase::Initializing;
        cluster.create(&init).await.unwrap();

        let states = || {
            cluster
                .get::<NodeSpec>(node)
                .unwrap()
                .unwrap()
                .status
                .backup_stores
        };
        check_once(&cluster, node, dir.path()).await.unwrap();
        let s = states();
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].store, s[0].ok), (store.id, false));
        assert!(
            s[0].error.as_deref().unwrap().contains("store.json"),
            "{s:?}"
        );
        // store.json을 두면 닿는다
        let sdir = pool_dir.join("store").join(store.id.to_string());
        std::fs::create_dir_all(&sdir).unwrap();
        let info = opl_store::format::StoreInfo::new(store.id, None, 1);
        std::fs::write(sdir.join("store.json"), serde_json::to_vec(&info).unwrap()).unwrap();
        check_once(&cluster, node, dir.path()).await.unwrap();
        let s = states();
        assert!(s[0].ok && s[0].error.is_none(), "{s:?}");
        // 바뀐 것이 없으면 쓰지 않는다(revision 그대로)
        let rev = cluster.get::<NodeSpec>(node).unwrap().unwrap().revision;
        check_once(&cluster, node, dir.path()).await.unwrap();
        assert_eq!(
            cluster.get::<NodeSpec>(node).unwrap().unwrap().revision,
            rev
        );
    }
}
