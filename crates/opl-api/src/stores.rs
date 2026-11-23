//! 백업 저장소(계획 68) 열기와 노드 고르기. Task 실행기(백업·복원·지우기·옮기기·GC·검증), 리더의 스케줄러, opld의
//! 닿음 보고가 함께 쓴다.
//!
//! - 백업 풀 안 저장소는 `{풀 루트}/store/{저장소 id}/`(설계 §7.6). 공유가 아닌 dir 풀이면 만든 노드(`status.node`)에만
//!   있다. 풀은 `backups`가 켜져 있고 그 노드에서 준비돼 있어야 한다.
//! - S3 저장소는 위트니스가 아닌 모든 노드가 쓴다(닿는지는 노드 보고 `NodeStatus.backup_stores`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use opl_cluster::Cluster;
use opl_driver::check_pool_root;
use opl_store::backend::local::LocalBackend;
use opl_store::backend::s3::S3Backend;
use opl_store::crypto::MasterKey;
use opl_store::{Backend, Store, StoreError};
use opl_types::backup_store::{BackupStore, BackupStoreSpec, StoreBackend};
use opl_types::node::{Node, NodeSpec};
use opl_types::storage::{StoragePool, StoragePoolSpec, is_shared, pool_error_on, pool_usable_on};
use opl_types::task::{TaskOp, TaskSpec};
use uuid::Uuid;

/// 저장소 `store.json`의 id: 만든 저장소는 리소스 id, 붙인 저장소는 초기화가 읽은 남의 id
pub fn store_id(store: &BackupStore) -> Uuid {
    store.status.store_id.unwrap_or(store.id)
}

/// 백업 풀 루트 `root` 안 저장소 디렉터리
pub fn store_dir(root: &Path, store: &BackupStore) -> PathBuf {
    root.join("store").join(store_id(store).to_string())
}

/// 저장소의 주 키(평문 저장소면 없음)
pub fn master_key(store: &BackupStore) -> Result<Option<MasterKey>, String> {
    store
        .spec
        .encryption
        .as_ref()
        .map(|e| MasterKey::from_base64(&e.key))
        .transpose()
        .map_err(|e| format!("backup store {:?}: invalid key: {e}", store.name))
}

/// 저장소 오류의 문구(사용자에게 보이는 Task 오류)
pub fn store_error(store: &str, e: StoreError) -> String {
    format!("backup store {store:?}: {e}")
}

/// 노드 `node`가 저장소를 쓸 수 있는지(온라인 여부는 보지 않는다). 백업 풀 안 저장소는 풀이 그 노드에서 준비돼 있고
/// 공유 풀이거나 저장소를 만든 노드여야 한다. S3는 위트니스가 아닌 노드다.
pub fn usable_on(store: &BackupStore, pool: Option<&StoragePool>, node: &Node) -> bool {
    match &store.spec.backend {
        StoreBackend::Pool(_) => pool.is_some_and(|p| {
            p.spec.backups
                && pool_usable_on(p, node)
                && (is_shared(p) || store.status.node.is_none_or(|n| n == node.id))
        }),
        StoreBackend::S3(_) => !node.spec.witness,
    }
}

/// 백업 풀 안 저장소의 풀(S3면 없음)
pub fn backend_pool(cluster: &Cluster, store: &BackupStore) -> Result<Option<StoragePool>, String> {
    match &store.spec.backend {
        StoreBackend::Pool(name) => cluster
            .get_by_name::<StoragePoolSpec>(name)
            .map_err(|e| e.to_string())?
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "storage pool {name:?} of backup store {:?} not found",
                    store.name
                )
            }),
        StoreBackend::S3(_) => Ok(None),
    }
}

/// 노드 `node`(이 노드)에서 저장소의 백엔드를 만든다. 쓸 수 없으면 그 까닭이다. `mount_root`는 NFS 풀의 마운트
/// 디렉터리다.
pub fn open_backend(
    cluster: &Cluster,
    store: &BackupStore,
    node: Uuid,
    mount_root: &Path,
) -> Result<Arc<dyn Backend>, String> {
    match &store.spec.backend {
        StoreBackend::Pool(name) => {
            let pool = backend_pool(cluster, store)?.expect("pool backend");
            let me = cluster
                .get::<NodeSpec>(node)
                .map_err(|e| e.to_string())?
                .ok_or("this node is not registered")?;
            if !pool.spec.backups {
                return Err(format!("storage pool {name:?} does not store backups"));
            }
            if !pool_usable_on(&pool, &me) {
                return Err(format!(
                    "storage pool {name:?} is not ready on this node: {}",
                    pool_error_on(&pool, &me)
                ));
            }
            if let Some(owner) = store
                .status
                .node
                .filter(|n| *n != node && !is_shared(&pool))
            {
                return Err(format!(
                    "backup store {:?} is on node {owner}, not on this node",
                    store.name
                ));
            }
            let root = check_pool_root(&pool, mount_root)
                .map_err(|e| format!("storage pool {name:?}: {e}"))?;
            Ok(Arc::new(LocalBackend::new(store_dir(&root, store))))
        }
        StoreBackend::S3(target) => S3Backend::new(target, store.spec.max_streams)
            .map(|b| Arc::new(b) as Arc<dyn Backend>)
            .map_err(|e| store_error(&store.name, e)),
    }
}

/// `backup-serve` 헬퍼(라이브 복원 §11.1)에 줄 저장소 백엔드: 백업 풀 안 저장소는 이 노드의 디렉터리, S3는 그 대상.
/// 쓸 수 없는 까닭은 [`open_backend`]와 같다.
pub fn serve_backend(
    cluster: &Cluster,
    store: &BackupStore,
    node: Uuid,
    mount_root: &Path,
) -> Result<opl_store::nbd::serve::ServeBackend, String> {
    use opl_store::nbd::serve::ServeBackend;
    match &store.spec.backend {
        StoreBackend::Pool(name) => {
            // 풀·노드 확인은 open_backend와 같다.
            open_backend(cluster, store, node, mount_root)?;
            let pool = backend_pool(cluster, store)?.expect("pool backend");
            let root = check_pool_root(&pool, mount_root)
                .map_err(|e| format!("storage pool {name:?}: {e}"))?;
            Ok(ServeBackend::Local {
                root: store_dir(&root, store),
            })
        }
        StoreBackend::S3(target) => Ok(ServeBackend::S3 {
            target: target.clone(),
            max_streams: store.spec.max_streams,
        }),
    }
}

/// 이 노드에서 저장소를 연다(키 지문 확인).
pub async fn open(
    cluster: &Cluster,
    store: &BackupStore,
    node: Uuid,
    mount_root: &Path,
) -> Result<Store, String> {
    let backend = open_backend(cluster, store, node, mount_root)?;
    let key = master_key(store)?;
    Store::open(backend, key.as_ref())
        .await
        .map_err(|e| store_error(&store.name, e))
}

/// 이름으로 저장소 리소스
pub fn get(cluster: &Cluster, name: &str) -> Result<BackupStore, String> {
    cluster
        .get_by_name::<BackupStoreSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("backup store {name:?} not found"))
}

/// 저장소 Task(GC·검증·초기화·scan)·저장소를 쓰는 Task를 돌릴 노드: 저장소를 쓸 수 있는 온라인 노드 가운데 — 공유가
/// 아닌 풀의 저장소는 만든 노드뿐 — 이 저장소에 닿는다고 보고한 노드를 먼저, 그다음 끝나지 않은 백업·저장소 Task가 적은
/// 노드, 이름 차례. `prefer`가 그런 노드면 그 노드다. 없으면 `None`.
pub fn task_node(
    cluster: &Cluster,
    store: &BackupStore,
    prefer: Option<Uuid>,
) -> Result<Option<Uuid>, String> {
    let pool = backend_pool(cluster, store)?;
    let (_, nodes) = cluster.list::<NodeSpec>().map_err(|e| e.to_string())?;
    let (_, tasks) = cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    let mut usable: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.status.online && usable_on(store, pool.as_ref(), n))
        .collect();
    if let Some(p) = prefer.filter(|p| usable.iter().any(|n| n.id == *p)) {
        return Ok(Some(p));
    }
    let busy = |node: Uuid| {
        tasks
            .iter()
            .filter(|t| t.spec.node == node && !t.status.state.is_finished())
            .filter(|t| {
                matches!(
                    t.spec.op,
                    TaskOp::VmBackup { .. } | TaskOp::BackupReplicate { .. }
                ) || t.spec.op.needs_backup_store()
            })
            .count()
    };
    let reports_ok = |n: &Node| {
        n.status
            .backup_stores
            .iter()
            .any(|s| s.store == store.id && s.ok)
    };
    usable.sort_by_key(|n| (!reports_ok(n), busy(n.id), n.name.clone()));
    Ok(usable.first().map(|n| n.id))
}

/// 노드 `node`가 저장소를 쓸 수 있는지와 아니면 그 까닭(풀 준비·저장소를 만든 노드). 노드가 온라인인지는 보지 않는다.
pub fn usable_on_node(cluster: &Cluster, store: &BackupStore, node: Uuid) -> Result<(), String> {
    let pool = backend_pool(cluster, store)?;
    let n = cluster
        .get::<NodeSpec>(node)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("node {node} not found"))?;
    if usable_on(store, pool.as_ref(), &n) {
        return Ok(());
    }
    Err(match (&pool, store.status.node) {
        (Some(p), Some(owner)) if !is_shared(p) && owner != node => {
            format!("backup store {:?} is on another node ({owner})", store.name)
        }
        (Some(p), _) if !p.spec.backups => {
            format!("storage pool {:?} does not store backups", p.name)
        }
        (Some(p), _) => format!(
            "storage pool {:?} is not ready on node {:?}: {}",
            p.name,
            n.name,
            pool_error_on(p, &n)
        ),
        (None, _) => format!("witness node {:?} does not use backup stores", n.name),
    })
}
