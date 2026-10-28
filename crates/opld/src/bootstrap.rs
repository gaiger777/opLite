//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_peer::pki::ClusterCa;
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use uuid::Uuid;

use crate::config::Config;
use crate::host::Host;

pub const NODE_ID_FILE: &str = "node-id";
const NODE_ID_TMP_FILE: &str = "node-id.tmp";

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

/// 새 클러스터의 첫 기록: 클러스터 정보(이름, CA), 이 노드, `admin` 사용자. 돌려준 비밀번호는
/// 저장하지 않는다(해시만 남는다).
pub async fn create_cluster(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    name: &str,
    ca: &ClusterCa,
) -> anyhow::Result<String> {
    let info = Resource::new(
        name,
        ClusterSpec {
            ca_cert_pem: ca.cert_pem.clone(),
            ca_key_pem: ca.key_pem.clone(),
        },
    );
    cluster.create(&info).await?;
    register_node(cluster, config, host).await?;
    let password = Uuid::new_v4().simple().to_string();
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password.clone())
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
            session_epoch: 0,
        },
    );
    cluster.create(&admin).await?;
    Ok(password)
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                },
            );
            node.id = id;
            cluster.create(&node).await?;
        }
        Some(node) if node.spec.address != address => {
            let spec = NodeSpec {
                address,
                ..node.spec
            };
            cluster.update_spec(id, None, node.labels, &spec).await?;
        }
        Some(_) => {}
    }
    let bridges = host.bridges().await;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let memory_mib = crate::host::memory_mib();
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            cpus,
            memory_mib,
            hypervisor: host.hypervisor_name(),
            // 브리지 목록을 얻지 못하면 기록된 목록을 그대로 둔다.
            bridges: bridges.clone().unwrap_or_else(|| status.bridges.clone()),
            arch: Some(host.profile.arch),
            // 풀 상태는 풀 새로 고침이 쓴다. 이미 적힌 것을 지우지 않는다.
            pools: std::mem::take(&mut status.pools),
        };
        true
    })
    .await
}

/// 브리지가 바뀌었으면(관리자가 만들거나 지움) 기록한다. 바뀌지 않았거나 목록을 얻지 못하면
/// 쓰지 않는다.
pub async fn refresh_bridges(cluster: &Cluster, host: &Host) -> anyhow::Result<()> {
    let Some(bridges) = host.bridges().await else {
        return Ok(());
    };
    update_node_status(cluster, |status| {
        if status.bridges == bridges {
            return false;
        }
        tracing::info!(?bridges, "bridges changed");
        status.bridges = bridges.clone();
        true
    })
    .await
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let result = update_node_status(cluster, |status| {
        status.online = false;
        true
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

/// 이 노드의 status를 `change`로 고쳐 쓴다(`false`면 쓰지 않는다). status의 다른 필드는 다른
/// 작업(풀 새로 고침)도 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 다시 고친다.
async fn update_node_status(
    cluster: &Cluster,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    loop {
        let Some(node) = cluster.get::<NodeSpec>(id)? else {
            return Ok(());
        };
        let mut status = node.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(id, node.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use openraft::{BasicNode, ServerState};
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, HostNet, Tap};
    use opl_types::node::{BridgeInfo, PoolState};
    use opl_types::vm::Arch;

    use super::*;

    /// 정해 둔 브리지를 돌려준다. `None`이면 `ip`가 실패한 것처럼 오류를 낸다.
    #[derive(Debug)]
    struct FakeNet(Option<Vec<BridgeInfo>>);

    #[async_trait]
    impl HostNet for FakeNet {
        async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
            self.0.clone().ok_or(DriverError::ToolFailed {
                tool: "ip",
                stderr: "boom".into(),
            })
        }
        async fn create_tap(&self, _: &Tap) -> Result<(), DriverError> {
            unimplemented!()
        }
        async fn delete_vm_taps(&self, _: Uuid) -> Result<(), DriverError> {
            unimplemented!()
        }
    }

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }

    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    pub(crate) async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, cluster.raft().clone());
        let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        (cluster, id)
    }

    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
            net: Some(Arc::new(FakeNet(bridges))),
            ..Host::from_config(&config).unwrap()
        };
        let br0 = vec![BridgeInfo {
            name: "br0".into(),
            vlan_filtering: false,
        }];
        let (ok, failing) = (host(Some(br0.clone())), host(None));
        let recorded = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .bridges
        };

        // 새 노드는 목록을 얻지 못하면 브리지가 없다.
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), vec![]);
        register_node(&cluster, &config, &ok).await.unwrap();
        assert_eq!(recorded(), br0);
        // 이미 기록된 브리지는 목록을 얻지 못해도 지우지 않는다.
        refresh_bridges(&cluster, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        refresh_bridges(&cluster, &host(Some(vec![])))
            .await
            .unwrap();
        assert_eq!(recorded(), vec![]);
        cluster.shutdown().await.unwrap();
    }

    /// 풀 새로 고침이 쓴 `pools`를 브리지 기록과 오프라인 표시가 지우지 않는다. 읽은 뒤 쓰기 전에 다른
    /// 쪽이 status를 써도 그 값을 지키며 다시 쓴다.
    #[tokio::test(flavor = "multi_thread")]
    async fn node_status_writes_keep_pools_written_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let pools = BTreeMap::from([("local".to_string(), PoolState::default())]);
        // 처음 고칠 때 풀 새로 고침이 끼어든다.
        let mut first = true;
        update_node_status(&cluster, |status| {
            if std::mem::take(&mut first) {
                let mut meanwhile = status.clone();
                meanwhile.pools = pools.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(cluster.update_status::<NodeSpec>(id.0, &meanwhile))
                })
                .unwrap();
            }
            status.bridges = vec![BridgeInfo {
                name: "br0".into(),
                vlan_filtering: false,
            }];
            true
        })
        .await
        .unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.pools, pools);
        assert_eq!(node.status.bridges.len(), 1);

        mark_offline(&cluster).await;
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(!node.status.online);
        assert_eq!(node.status.pools, pools);

        // 다시 시작해 등록해도 풀 새로 고침이 먼저 쓴 pools를 지우지 않는다.
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.status.online);
        assert_eq!(node.status.pools, pools);
        cluster.shutdown().await.unwrap();
    }
}
