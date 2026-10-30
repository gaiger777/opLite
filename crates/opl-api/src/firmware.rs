//! 다른 노드에서 UEFI 변수 파일 받아 오기(설계 §5). 변수 파일을 가진 노드(`firmware_node`)에 노드
//! 사이 RPC `UefiVars`로 받아 이 노드의 변수 파일을 바꾼다. opld가 reconciler에 넣는다.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_driver::uefi_vars_path;
use opl_peer::Peers;
use opl_reconciler::{FirmwareError, FirmwareFetcher};
use opl_types::node::NodeSpec;
use uuid::Uuid;

pub struct UefiVarsFetcher {
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    /// 이 노드의 드라이버 `state_dir`(받은 파일을 여기 둔다)
    state_dir: PathBuf,
    /// 이 노드의 UEFI 변수 템플릿. 받은 파일의 크기가 이와 다르면(다른 펌웨어 빌드) 쓰지 않는다.
    template: PathBuf,
}

impl UefiVarsFetcher {
    pub fn new(
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        state_dir: impl Into<PathBuf>,
        template: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cluster,
            peers,
            state_dir: state_dir.into(),
            template: template.into(),
        }
    }
}

#[async_trait]
impl FirmwareFetcher for UefiVarsFetcher {
    /// 오프라인으로 기록된 노드에는 연결해 보지 않는다. 노드마다 펌웨어 빌드가 달라 변수 파일의 크기가
    /// 이 노드의 템플릿과 다르면 받은 것을 버린다(이 노드의 펌웨어가 그 파일로 켜지지 않는다).
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
        match self.cluster.get::<NodeSpec>(node) {
            Ok(Some(n)) if !n.status.online => {
                return Err(FirmwareError(format!("node {:?} is offline", n.name)));
            }
            Ok(_) => {}
            Err(e) => return Err(FirmwareError(format!("cannot read node {node}: {e}"))),
        }
        let addr = self
            .cluster
            .members()
            .addr(NodeId(node))
            .map(str::to_string)
            .ok_or_else(|| FirmwareError(format!("node {node} is not a cluster member")))?;
        let len = tokio::fs::metadata(&self.template)
            .await
            .map_err(|e| {
                FirmwareError(format!(
                    "cannot read the UEFI variables template {}: {e}",
                    self.template.display()
                ))
            })?
            .len();
        self.peers
            .uefi_vars(&addr, vm, &uefi_vars_path(&self.state_dir, vm), Some(len))
            .await
            .map(|_| ())
            .map_err(|e| FirmwareError(e.to_string()))
    }
}
