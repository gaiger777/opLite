//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::net::IpAddr;
use std::path::Path;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_peer::pki::ClusterCa;
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use opl_types::vm::Arch;
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

/// 다시 시작한 노드의 역할(합류할 때 정해진 Node `spec.witness`)이 설정의 `witness`와 같은지 본다.
/// 로컬 저장소만 읽는다. Node 리소스가 아직 없으면 그대로 둔다.
pub fn check_role(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let Some(node) = cluster.get::<NodeSpec>(id)? else {
        return Ok(());
    };
    match (node.spec.witness, config.witness) {
        (true, false) => anyhow::bail!(
            "this node joined as a witness but opld.toml says witness = false; leave the cluster \
             and join again to change the role"
        ),
        (false, true) => anyhow::bail!(
            "this node joined as a non-witness node but opld.toml says witness = true; leave the \
             cluster and join again to change the role"
        ),
        _ => Ok(()),
    }
}

/// 이 노드의 Node 리소스가 없으면 만들고(역할은 설정의 `witness`), 있으면 주소만 맞춘다.
async fn ensure_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                    witness: config.witness,
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
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let underlay = config.underlay().await;
    record_node(cluster, config, host, underlay).await
}

/// `register_node`의 기록. `underlay`는 정한 underlay다(None이면 풀지 못한 것).
async fn record_node(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    underlay: Option<IpAddr>,
) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
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
            // 펜싱이 꺼져 있으면 None이라 리더가 이 노드의 HA VM을 옮기지 않는다.
            ha: config.ha.node_ha(),
            // 이름을 풀지 못하면(설정·IP 리터럴이 아님) 기록된 값을 지킨다(잠깐 DNS가 안 된다고
            // VXLAN 네트워크를 잃지 않게).
            underlay: underlay.or(status.underlay),
            firewall: host.firewall.is_some(),
            version: Some(BUILD.to_string()),
        };
        true
    })
    .await
}

/// 위트니스의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 아키텍처만 기록한다. VM을 돌리지
/// 않으므로 용량·하이퍼바이저·브리지·풀은 비운다.
pub async fn register_witness(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            arch: Some(Arch::native()),
            version: Some(BUILD.to_string()),
            ..NodeStatus::default()
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

    use super::*;
    use async_trait::async_trait;
    use openraft::{BasicNode, ServerState};
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, PoolState};

    use crate::config::FencingSetting;

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
        async fn sync_overlays(
            &self,
            _: &str,
            _: &[Overlay],
        ) -> Result<OverlayChanges, DriverError> {
            unimplemented!()
        }
    }

    #[derive(Debug)]
    struct FakeFirewall;

    #[async_trait]
    impl Firewall for FakeFirewall {
        async fn apply(&self, _: &str) -> Result<(), DriverError> {
            Ok(())
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
        router.register(id, &cluster);
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

    /// address는 IP 리터럴이다(등록이 이름을 풀지 않게).
    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"10.0.0.1\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    /// `[ha]`의 펜싱을 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다. `off`면 없다.
    #[tokio::test]
    async fn registration_records_the_ha_fencing() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        config.ha.fencing = FencingSetting::Process;
        config.ha.fence_after_secs = 2;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.ha,
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        config.ha.fencing = FencingSetting::Off;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.ha, None);
        cluster.shutdown().await.unwrap();
    }

    /// `[network] underlay`를, 없으면 `address`의 IP를 기록한다. 이름을 풀지 못하면(None) 기록된 값을
    /// 그대로 둔다(잠깐 DNS가 안 된다고 VM이 쓰는 VXLAN 장치를 지우지 않게).
    #[tokio::test]
    async fn registration_records_the_underlay() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let underlay = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .underlay
        };
        // 이름을 풀지 못한 새 노드에는 없다.
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), None);
        config.network.underlay = Some("10.1.0.5".parse().unwrap());
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("10.1.0.5".parse().unwrap()));
        config.network.underlay = None;
        config.address = "fd00::5".into();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        cluster.shutdown().await.unwrap();
    }

    /// 방화벽(nftables)이 있는지 기록하고, 다시 등록하면 그때의 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_firewall() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let firewall = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .firewall
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        host.firewall = Some(Arc::new(FakeFirewall));
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(firewall());
        host.firewall = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        cluster.shutdown().await.unwrap();
    }

    /// 등록할 때 이 빌드의 버전을 쓴다(옛 버전으로 적힌 값도 바꾼다).
    #[tokio::test]
    async fn registration_records_the_build_version() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let version = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .version
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        update_node_status(&cluster, |status| {
            status.version = Some("0.0.1".into());
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
    #[tokio::test]
    async fn witness_registration_records_only_the_role_and_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        // underlay를 설정해도 기록하지 않는다.
        let config: Config = toml::from_str(
            "name = \"w\"\naddress = \"10.0.0.9\"\nwitness = true\n[network]\nunderlay = \"10.1.0.9\"",
        )
        .unwrap();
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.name, "w");
        assert!(node.spec.witness);
        assert_eq!(node.spec.address, "10.0.0.9:8006");
        assert_eq!(
            node.status,
            NodeStatus {
                online: true,
                arch: Some(Arch::native()),
                version: Some(BUILD.to_string()),
                ..NodeStatus::default()
            }
        );

        mark_offline(&cluster).await;
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.spec.witness);
        assert!(node.status.online);
        cluster.shutdown().await.unwrap();
    }

    /// 합류할 때 정한 역할과 설정의 `witness`가 다르면 시작하지 않는다. Node 리소스가 없으면 본다.
    #[tokio::test]
    async fn a_role_change_in_the_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let witness: Config =
            toml::from_str("name = \"w\"\naddress = \"h\"\nwitness = true").unwrap();
        let normal = test_config();
        check_role(&cluster, &witness).unwrap();
        check_role(&cluster, &normal).unwrap();

        register_witness(&cluster, &witness).await.unwrap();
        check_role(&cluster, &witness).unwrap();
        let err = check_role(&cluster, &normal).unwrap_err().to_string();
        assert!(
            err.contains("joined as a witness but opld.toml says witness = false"),
            "{err}"
        );

        let NodeId(id) = cluster.id();
        let node = cluster.get::<NodeSpec>(id).unwrap().unwrap();
        let spec = NodeSpec {
            witness: false,
            ..node.spec
        };
        cluster
            .update_spec(id, None, node.labels, &spec)
            .await
            .unwrap();
        check_role(&cluster, &normal).unwrap();
        let err = check_role(&cluster, &witness).unwrap_err().to_string();
        assert!(
            err.contains("joined as a non-witness node but opld.toml says witness = true"),
            "{err}"
        );
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
            net: Some(Arc::new(FakeNet(bridges))),
            ..Host::from_config(&config, "t").unwrap()
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
        let mut host = Host::from_config(&config, "t").unwrap();
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
