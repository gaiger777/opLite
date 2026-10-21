use std::path::PathBuf;
use std::sync::Arc;

use opl_cluster::Cluster;
use opl_driver::Hypervisor;
use uuid::Uuid;

use crate::auth::{Authorizer, RoleAuthorizer, Sessions};

/// 모든 핸들러가 공유하는 노드의 상태.
pub struct AppState {
    pub cluster: Arc<Cluster>,
    /// 이 노드의 id
    pub node: Uuid,
    pub hv: Arc<dyn Hypervisor>,
    pub authorizer: Arc<dyn Authorizer>,
    /// 올린 이미지를 가져오기 전에 잠시 두는 디렉터리. 볼륨 풀과 같은 파일시스템이면 좋다.
    pub upload_dir: PathBuf,
    pub(crate) sessions: Sessions,
    /// 이 노드에서 VM spec의 검증과 쓰기를 한 번에 하나씩 한다(MAC·볼륨 중복 확인이 경합하지 않게).
    /// 여러 노드에 걸친 경합은 나중에 클러스터 수준의 확인이 필요하다.
    pub(crate) vm_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new(
        cluster: Arc<Cluster>,
        node: Uuid,
        hv: Arc<dyn Hypervisor>,
        upload_dir: PathBuf,
    ) -> Self {
        Self {
            cluster,
            node,
            hv,
            authorizer: Arc::new(RoleAuthorizer),
            upload_dir,
            sessions: Sessions::default(),
            vm_lock: tokio::sync::Mutex::new(()),
        }
    }
}
