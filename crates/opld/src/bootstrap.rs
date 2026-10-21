//! 노드가 시작할 때 한 번 하는 일: 노드 id, 1인 클러스터 초기화, 첫 관리자, Node 리소스.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use openraft::{BasicNode, ServerState};
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use uuid::Uuid;

use crate::config::Config;

pub const NODE_ID_FILE: &str = "node-id";
const NODE_ID_TMP_FILE: &str = "node-id.tmp";
pub const ADMIN_PASSWORD_FILE: &str = "initial-admin-password";

/// 처음 시작할 때 만든 노드 id를 계속 쓴다. Raft 노드 id이자 Node 리소스 id다.
pub fn node_id(data_dir: &Path) -> anyhow::Result<Uuid> {
    let path = data_dir.join(NODE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => Uuid::try_parse(s.trim()).with_context(|| format!("bad {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::now_v7();
            // 임시 파일에 쓰고 바꿔 넣어, 도중에 죽어도 반쯤 쓴 id가 남지 않게 한다.
            let tmp = data_dir.join(NODE_ID_TMP_FILE);
            let mut file = std::fs::File::create(&tmp)?;
            writeln!(file, "{id}")?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            Ok(id)
        }
        Err(e) => Err(e.into()),
    }
}

/// 아직 멤버십이 없으면 이 노드 하나로 클러스터를 시작하고, 리더가 될 때까지 기다린다.
/// 다른 노드의 합류는 계획 6에서 다룬다.
pub async fn start_cluster(cluster: &Cluster, address: &str) -> anyhow::Result<()> {
    let id = cluster.id();
    if !cluster.raft().is_initialized().await? {
        tracing::info!(node = %id, "initializing a new single-node cluster");
        cluster
            .initialize(BTreeMap::from([(id, BasicNode::new(address))]))
            .await?;
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| m.state == ServerState::Leader && m.current_leader == Some(id),
            "become leader",
        )
        .await?;
    Ok(())
}

/// 사용자가 하나도 없으면 `admin`을 만들고 비밀번호를 `{data_dir}/initial-admin-password`(0600)에 쓴다.
pub async fn ensure_admin(cluster: &Cluster, data_dir: &Path) -> anyhow::Result<()> {
    if !cluster.list::<UserSpec>()?.1.is_empty() {
        return Ok(());
    }
    let password = Uuid::new_v4().simple().to_string();
    let path = data_dir.join(ADMIN_PASSWORD_FILE);
    // 사용자를 만들기 전에 파일을 쓴다. 그 사이 죽으면 다음 시작에서 새 비밀번호로 덮어쓴다.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    writeln!(file, "{password}")?;
    file.sync_all()?;
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password)
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
        },
    );
    cluster.create(&admin).await?;
    tracing::info!(path = %path.display(), "created user admin; its password is in this file");
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량을 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address: config.listen.clone(),
                    maintenance: false,
                },
            );
            node.id = id;
            cluster.create(&node).await?;
        }
        Some(node) if node.spec.address != config.listen => {
            let spec = NodeSpec {
                address: config.listen.clone(),
                ..node.spec
            };
            cluster.update_spec(id, None, node.labels, &spec).await?;
        }
        Some(_) => {}
    }
    let status = NodeStatus {
        online: true,
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        memory_mib: crate::host::memory_mib(),
        hypervisor: "qemu-hvf".into(),
    };
    cluster.update_status::<NodeSpec>(id, &status).await?;
    Ok(())
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let NodeId(id) = cluster.id();
    let Ok(Some(node)) = cluster.get::<NodeSpec>(id) else {
        return;
    };
    let status = NodeStatus {
        online: false,
        ..node.status
    };
    if let Err(e) = cluster.update_status::<NodeSpec>(id, &status).await {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }
}
