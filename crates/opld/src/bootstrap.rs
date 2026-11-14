//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError, ZONE_LABEL};
use opl_peer::pki::{ClusterCa, NodeTls, cert_names};
use opl_reconciler::HaGuard;
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::host::same_host;
use opl_types::node::{FEATURES, NodeHa, NodeShutdown, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use opl_types::vm::Arch;
use uuid::Uuid;

use crate::config::Config;
use crate::host::Host;
use crate::member::pki_dir;

pub const NODE_ID_FILE: &str = "node-id";
const NODE_ID_TMP_FILE: &str = "node-id.tmp";
/// 키운 HA 값 기록([`record_raised_ha`]) 한 번의 제한 시간과 다시 해 보는 간격
const HA_RECORD_TIMEOUT: Duration = Duration::from_secs(2);
const HA_RECORD_RETRY: Duration = Duration::from_millis(500);

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
            realm: None,
            subject: None,
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

/// 이 노드의 Node 리소스가 없으면 만들고(역할은 설정의 `witness`), 있으면 주소와 zone 레이블(`[node] zone`이
/// 있을 때만)을 맞춘다.
async fn ensure_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    // 앞뒤 공백은 잘라 적는다(`" rack-a"`와 `"rack-a"`를 다른 zone으로 보지 않게).
    let zone = config.node.zone.as_deref().map(str::trim);
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                    witness: config.witness,
                    roles: None,
                },
            );
            node.id = id;
            if let Some(zone) = zone {
                node.labels.insert(ZONE_LABEL.into(), zone.to_string());
            }
            cluster.create(&node).await?;
        }
        Some(node)
            if node.spec.address != address
                || zone.is_some_and(|z| {
                    node.labels.get(ZONE_LABEL).map(String::as_str) != Some(z)
                }) =>
        {
            let mut labels = node.labels;
            if let Some(zone) = zone {
                labels.insert(ZONE_LABEL.into(), zone.to_string());
            }
            let spec = NodeSpec {
                address,
                ..node.spec
            };
            cluster.update_spec(id, None, labels, &spec).await?;
        }
        Some(_) => {}
    }
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부, 버전, 업데이트 확인 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let underlay = config.underlay().await;
    record_node(cluster, config, host, underlay).await
}

/// 이 노드에 설정한 HA 값(`configured`, [`Config::node_ha`])이 기록된 값(`stored`)보다 크다: 리더가 등록 전까지
/// 기록된 값으로 일찍 복구할 수 있다. 펜싱을 새로 켰거나 `recover_after`(watchdog 포함)·`fence_after` 가운데
/// 하나라도 커졌으면 true다. 펜싱을 껐거나 같거나 작아졌으면 false다.
pub fn ha_raised(stored: Option<&NodeHa>, configured: Option<&NodeHa>) -> bool {
    let Some(configured) = configured else {
        return false;
    };
    let Some(stored) = stored else {
        return true;
    };
    configured.recover_after_secs > stored.recover_after_secs
        || configured.fence_after_secs.unwrap_or(0) > stored.fence_after_secs.unwrap_or(0)
}

/// 키운 HA 값(`status.ha`)만 먼저 기록하고 `guard`의 붙들기([`HaGuard::hold_until_recorded`])를 푼다. 전체 등록
/// ([`register_node`])은 이름 풀기·브리지 읽기로 오래 걸릴 수 있고, 그동안 `arm`하지 못하면 이전 프로세스가 켜 둔
/// watchdog을 아무도 쓰다듬지 않아 호스트가 재시작된다. 그래서 `status.ha` 하나만 짧게(시도마다
/// [`HA_RECORD_TIMEOUT`], [`HA_RECORD_RETRY`]마다) 쓸 때까지 다시 한다.
pub async fn record_raised_ha(cluster: &Cluster, config: &Config, guard: &HaGuard) {
    let ha = config.node_ha();
    loop {
        let write = update_node_status(cluster, |status| {
            if status.ha == ha {
                return false;
            }
            status.ha = ha;
            true
        });
        match tokio::time::timeout(HA_RECORD_TIMEOUT, write).await {
            Ok(Ok(())) => break,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "cannot record the longer HA timings; will retry")
            }
            Err(_) => tracing::warn!("recording the longer HA timings timed out; will retry"),
        }
        tokio::time::sleep(HA_RECORD_RETRY).await;
    }
    guard.recorded();
}

/// `register_node`의 기록. `underlay`는 정한 underlay다(None이면 풀지 못한 것).
async fn record_node(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    underlay: Option<IpAddr>,
) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    end_shutdown_maintenance(cluster).await?;
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
            ha: config.node_ha(),
            // 이름을 풀지 못하면(설정·IP 리터럴이 아님) 기록된 값을 지킨다(잠깐 DNS가 안 된다고
            // VXLAN 네트워크를 잃지 않게).
            underlay: underlay.or(status.underlay),
            firewall: host.firewall.is_some(),
            version: Some(BUILD.to_string()),
            // 업데이트 확인이 쓴 결과는 지우지 않는다. 꺼진 노드는 None이다(다시 확인을 받지 않는다).
            updates: config
                .updates
                .enabled()
                .then(|| status.updates.take().unwrap_or_default()),
            features: features(),
            cluster_addresses: announced_links(config),
            // 다시 시작했으니 지난 호스트 종료 표시는 지운다(얼린 노드가 풀린다).
            shutdown: None,
            // 이웃 확인 결과는 `FenceWatch`가 쓴다. 지우지 않는다.
            fence_check: status.fence_check.take(),
            // 인터페이스와 HA 검사 결과는 노드의 검사기가 쓴다. 지우지 않는다.
            interfaces: std::mem::take(&mut status.interfaces),
            checks: std::mem::take(&mut status.checks),
        };
        true
    })
    .await
}

/// 지난 호스트 종료 때 정책(`migrate`)이 켠 유지보수를 끈다. 관리자가 켠 유지보수(표시의 `maintenance`가
/// false)는 그대로 둔다. 표시는 등록이 지운다.
async fn end_shutdown_maintenance(cluster: &Cluster) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    loop {
        let Some(node) = cluster.get::<NodeSpec>(id)? else {
            return Ok(());
        };
        if !node.spec.maintenance || !node.status.shutdown.is_some_and(|s| s.maintenance) {
            return Ok(());
        }
        tracing::info!("ending the maintenance the last host shutdown turned on");
        let spec = NodeSpec {
            maintenance: false,
            ..node.spec
        };
        match cluster
            .update_spec(id, Some(node.revision), node.labels, &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// 호스트 종료 표시를 쓴다(과반수가 없으면 쓰기 제한 시간 뒤 실패).
pub async fn mark_shutdown(cluster: &Cluster, shutdown: NodeShutdown) -> anyhow::Result<()> {
    update_node_status(cluster, |status| {
        status.shutdown = Some(shutdown);
        true
    })
    .await
}

/// 이 빌드가 아는 기능([`FEATURES`]). 위트니스도 적는다(새 기능의 API는 모든 노드를 본다).
fn features() -> Vec<String> {
    FEATURES.iter().map(|f| f.to_string()).collect()
}

/// 알릴 클러스터 링크: 설정의 링크 가운데 호스트가 지금 노드 인증서의 이름에 있는 것. 다시 서명하지 못해 이름이
/// 빠진 링크로는 다른 노드가 TLS로 닿지 못하므로 알리지 않고 경고한다(빠진 링크가 바뀔 때만, `refresh_features`가
/// 주기적으로 부른다). 인증서를 읽지 못하면 모두 알린다.
pub(crate) fn announced_links(config: &Config) -> Vec<String> {
    let links = config.cluster_links();
    let names = match NodeTls::load(&pki_dir(config)) {
        Ok(Some(tls)) => cert_names(&tls.cert_pem).ok(),
        _ => None,
    };
    let Some(names) = names else {
        return links;
    };
    let (named, missing): (Vec<_>, Vec<_>) = config
        .cluster_hosts()
        .into_iter()
        .zip(links)
        .partition(|(host, _)| names.iter().any(|n| same_host(n, host)));
    let missing: Vec<_> = missing.into_iter().map(|(_, link)| link).collect();
    if newly_missing(&MISSING_LINKS, &missing) {
        tracing::warn!(
            ?missing,
            "the node certificate does not name these cluster links; not announcing them"
        );
    }
    named.into_iter().map(|(_, link)| link).collect()
}

/// 마지막으로 본 알리지 않는 링크([`announced_links`])
static MISSING_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 알리지 않는 링크 `missing`이 있고 `last`(지난번에 본 것)와 다르면 true다. `last`를 `missing`으로 바꾼다.
fn newly_missing(last: &Mutex<Vec<String>>, missing: &[String]) -> bool {
    let mut last = last.lock().unwrap();
    let changed = *last != missing;
    if changed {
        *last = missing.to_vec();
    }
    changed && !missing.is_empty()
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
            features: features(),
            cluster_addresses: announced_links(config),
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

/// 기능 표시와 클러스터 링크가 이 빌드의 목록·설정과 다르면 고쳐 쓴다. 롤링 업그레이드 중 옛 버전 리더가 이
/// 노드의 status를 다시 쓰면(모르는 `features`·`cluster_addresses`를 빼고) 지워지므로 주기적으로 되살린다. 같으면
/// 쓰지 않는다.
pub async fn refresh_features(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let features = features();
    let links = announced_links(config);
    update_node_status(cluster, |status| {
        if status.features == features && status.cluster_addresses == links {
            return false;
        }
        tracing::info!(?features, ?links, "restoring the recorded features");
        status.features = features.clone();
        status.cluster_addresses = links.clone();
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
pub(crate) async fn update_node_status(
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
    use openraft::ServerState;
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_cluster::types::OrbNode;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, NodeUpdates, PoolState, ShutdownAction};

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
        let members = BTreeMap::from([(id, OrbNode::single(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
        cluster
            .raft()
            .unwrap()
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

    /// 설정의 클러스터 링크(없으면 `address` 하나)를 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_cluster_links() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let links = || {
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            node.status.cluster_addresses
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(links(), ["10.0.0.1:8007"]);
        config.cluster_addresses = vec!["10.1.0.1".into(), "10.0.0.1".into()];
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(links(), ["10.1.0.1:8007", "10.0.0.1:8007"]);
        cluster.shutdown().await.unwrap();
    }

    /// 노드 인증서에 이름이 없는 링크(다시 서명 실패)는 알리지 않는다. 이름은 값으로 비교한다.
    #[tokio::test]
    async fn registration_announces_only_links_named_in_the_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        config.data_dir = dir.path().join("data");
        config.cluster_addresses = vec!["fd00::1".into(), "10.1.0.1".into(), "10.0.0.1".into()];
        let ca = ClusterCa::generate("t").unwrap();
        let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
        let names = ["10.0.0.1".to_string(), "fd00:0::1".into()];
        NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id.0, &names).unwrap(),
            key_pem,
        }
        .save(&pki_dir(&config))
        .unwrap();
        assert_eq!(
            announced_links(&config),
            ["[fd00::1]:8007", "10.0.0.1:8007"]
        );

        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.cluster_addresses,
            ["[fd00::1]:8007", "10.0.0.1:8007"]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 알리지 않는 링크의 경고는 그 집합이 바뀔 때만 한다(같은 것을 주기적으로 되풀이하지 않음).
    #[test]
    fn missing_links_are_warned_only_when_they_change() {
        let last = Mutex::new(Vec::new());
        let a = ["10.1.0.1:8007".to_string()];
        let ab = ["10.1.0.1:8007".to_string(), "10.2.0.1:8007".into()];
        assert!(!newly_missing(&last, &[]));
        assert!(newly_missing(&last, &a));
        assert!(!newly_missing(&last, &a));
        assert!(newly_missing(&last, &ab));
        assert!(!newly_missing(&last, &ab));
        assert!(!newly_missing(&last, &[]));
        assert!(newly_missing(&last, &a));
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
        config.ha.fence_after_secs = Some(2);
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.ha,
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 4,
                fence_after_secs: Some(2),
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

    /// `[node] zone`을 노드 레이블 `zone`으로 적는다. 다른 레이블은 그대로 두고, zone이 없으면 레이블을
    /// 건드리지 않는다(API로 둔 zone).
    #[tokio::test]
    async fn registration_records_the_zone_label() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let labels = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap().labels;
        config.node.zone = Some("rack-a".into());
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(labels().get("zone").map(String::as_str), Some("rack-a"));
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        let mut other = node.labels.clone();
        other.insert("owner".into(), "ops".into());
        cluster
            .update_spec(id.0, None, other, &node.spec)
            .await
            .unwrap();
        // 앞뒤 공백은 잘라 적는다.
        config.node.zone = Some(" rack-b ".into());
        register_node(&cluster, &config, &host).await.unwrap();
        let got = labels();
        assert_eq!(got.get("zone").map(String::as_str), Some("rack-b"));
        assert_eq!(got.get("owner").map(String::as_str), Some("ops"));
        config.node.zone = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(labels(), got);
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

    /// 등록할 때 이 빌드의 버전과 기능을 쓴다(옛 버전으로 적힌 값도 바꾼다).
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
        let features = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .features
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer",
                "multi-link",
                "ha-placement",
                "fast-ha",
                "ha-checks",
                "placement"
            ]
        );
        update_node_status(&cluster, |status| {
            status.version = Some("0.0.1".into());
            status.features.clear();
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer",
                "multi-link",
                "ha-placement",
                "fast-ha",
                "ha-checks",
                "placement"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 옛 버전 리더가 status를 다시 쓰며 지운 기능 표시와 클러스터 링크를 주기 갱신이 되살린다. 같으면 쓰지 않는다.
    #[tokio::test]
    async fn refresh_restores_features_erased_by_an_old_leader() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let node = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        let revision = node().revision;
        refresh_features(&cluster, &config).await.unwrap();
        assert_eq!(node().revision, revision);
        // 옛 리더는 모르는 `features`·`cluster_addresses`를 빼고 status를 다시 쓴다.
        update_node_status(&cluster, |status| {
            status.features.clear();
            status.cluster_addresses.clear();
            true
        })
        .await
        .unwrap();
        assert!(node().status.features.is_empty());
        refresh_features(&cluster, &config).await.unwrap();
        assert_eq!(node().status.cluster_addresses, config.cluster_links());
        assert!(!node().status.cluster_addresses.is_empty());
        assert_eq!(
            node().status.features,
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer",
                "multi-link",
                "ha-placement",
                "fast-ha",
                "ha-checks",
                "placement"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    #[test]
    fn ha_raised_only_when_the_configured_timings_grow() {
        use opl_types::node::Fencing;
        let ha = |fencing, fence_after: Option<u64>, recover_after| NodeHa {
            fencing,
            recover_after_secs: recover_after,
            fence_after_secs: fence_after,
        };
        let fast = ha(Fencing::Watchdog, Some(10), 22);
        let safe = ha(Fencing::Watchdog, Some(30), 62);
        // fast → safe, 펜싱을 새로 켬, 계획 51 전 기록(fence_after 없음)
        assert!(ha_raised(Some(&fast), Some(&safe)));
        assert!(ha_raised(None, Some(&fast)));
        assert!(ha_raised(
            Some(&ha(Fencing::Watchdog, None, 22)),
            Some(&fast)
        ));
        // process → watchdog(recover_after만 커짐)
        assert!(ha_raised(
            Some(&ha(Fencing::Process, Some(10), 12)),
            Some(&fast)
        ));
        // 같거나 작아짐, 펜싱을 끔
        assert!(!ha_raised(Some(&fast), Some(&fast)));
        assert!(!ha_raised(Some(&safe), Some(&fast)));
        assert!(!ha_raised(Some(&fast), None));
        assert!(!ha_raised(None, None));
    }

    /// 키운 HA 값은 `status.ha` 하나만 써서 붙들기를 푼다: 전체 등록(이름 풀기·브리지 읽기·온라인 표시)을
    /// 기다리지 않는다.
    #[tokio::test]
    async fn raised_ha_timings_are_recorded_alone_and_release_the_guard() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let config: Config = toml::from_str(
            "name = \"n\"\naddress = \"10.0.0.1\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false\n[ha]\nfencing = \"process\"",
        )
        .unwrap();
        ensure_node(&cluster, &config).await.unwrap();
        let guard = HaGuard::spawn(cluster.clone(), config.ha.guard_config().unwrap());
        guard.subscribe().wait_for(|s| s.fresh).await.unwrap();
        guard.hold_until_recorded();
        assert!(matches!(
            guard.arm(),
            Err(opl_reconciler::ha::ArmError::Unrecorded)
        ));
        record_raised_ha(&cluster, &config, &guard).await;
        guard.arm().unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.ha, config.node_ha());
        assert!(config.node_ha().is_some());
        // 등록의 다른 기록은 아직 없다.
        assert!(!node.status.online);
        assert_eq!(node.status.version, None);
        guard.close();
        cluster.shutdown().await.unwrap();
    }

    /// 다시 등록해도 이웃 확인 결과(`fence_check`, `FenceWatch`가 쓴다)와 인터페이스·HA 검사 결과(검사기가 쓴다)를
    /// 지우지 않는다.
    #[tokio::test]
    async fn registration_keeps_the_fence_check_interfaces_and_checks() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let check = opl_types::node::FenceCheck {
            at: 7,
            by: "n2".into(),
            error: Some("ipmi: timed out".into()),
        };
        let interfaces = vec![opl_types::node::NodeInterface {
            name: "eno1".into(),
            kind: opl_types::node::InterfaceKind::Ethernet,
            state: "up".into(),
            carrier: Some(true),
            speed_mbps: None,
            mac: None,
            master: None,
            addresses: vec!["10.0.0.5/24".into()],
            bond: None,
        }];
        let checks = vec![opl_types::ha_check::CheckReport::new(
            "stor",
            opl_types::ha_check::CheckState::Failing,
            Some("no carrier".into()),
            Some(5),
        )];
        update_node_status(&cluster, |status| {
            status.fence_check = Some(check.clone());
            status.interfaces = interfaces.clone();
            status.checks = checks.clone();
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.fence_check, Some(check));
        assert_eq!(node.status.interfaces, interfaces);
        assert_eq!(node.status.checks, checks);
        cluster.shutdown().await.unwrap();
    }

    /// 주기 갱신도 노드 인증서에 이름이 있는 링크만 되살린다.
    #[tokio::test]
    async fn refresh_restores_only_links_named_in_the_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        config.data_dir = dir.path().join("data");
        config.cluster_addresses = vec!["10.0.0.1".into(), "10.1.0.1".into()];
        let ca = ClusterCa::generate("t").unwrap();
        let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
        NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id.0, &["10.0.0.1".into()]).unwrap(),
            key_pem,
        }
        .save(&pki_dir(&config))
        .unwrap();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        update_node_status(&cluster, |status| {
            status.cluster_addresses = vec!["10.0.0.1:8007".into(), "10.1.0.1:8007".into()];
            true
        })
        .await
        .unwrap();
        refresh_features(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.cluster_addresses, ["10.0.0.1:8007"]);
        cluster.shutdown().await.unwrap();
    }

    /// 업데이트가 켜진 노드는 `updates`를 기록하고(처음엔 빈 값) 확인 결과를 다시 등록해도 지키며,
    /// 꺼지면 지운다.
    #[tokio::test]
    async fn registration_records_whether_updates_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let updates = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .updates
        };
        config.updates.enabled = Some(true);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(NodeUpdates::default()));
        let checked = NodeUpdates {
            checked_at: Some(1_793_000_000),
            error: Some("apt-get update -q failed".into()),
            ..NodeUpdates::default()
        };
        update_node_status(&cluster, |status| {
            status.updates = Some(checked.clone());
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(checked));
        config.updates.enabled = Some(false);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), None);
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
    #[tokio::test]
    async fn witness_registration_records_only_the_role_and_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        // underlay와 업데이트를 설정해도 기록하지 않는다.
        let config: Config = toml::from_str(
            "name = \"w\"\naddress = \"10.0.0.9\"\nwitness = true\n[network]\nunderlay = \"10.1.0.9\"\n\
             [updates]\nenabled = true",
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
                features: vec![
                    "node-drain".into(),
                    "incremental-backup".into(),
                    "incremental-replication".into(),
                    "storage-migration".into(),
                    "metrics".into(),
                    "node-roles".into(),
                    "shared-sessions".into(),
                    "vip".into(),
                    "lease-liveness".into(),
                    "voter-limit".into(),
                    "observer".into(),
                    "multi-link".into(),
                    "ha-placement".into(),
                    "fast-ha".into(),
                    "ha-checks".into(),
                    "placement".into(),
                ],
                cluster_addresses: vec!["10.0.0.9:8007".into()],
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

    /// 다시 등록하면 호스트 종료 표시를 지우고, 표시가 켠 유지보수만 끈다(관리자가 켠 것은 둔다).
    #[tokio::test]
    async fn registration_clears_the_shutdown_mark_and_its_maintenance() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let set_maintenance = |on: bool| {
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            let spec = NodeSpec {
                maintenance: on,
                ..node.spec
            };
            let cluster = &cluster;
            async move {
                cluster
                    .update_spec(id.0, None, node.labels, &spec)
                    .await
                    .unwrap();
            }
        };
        let node = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        for (by_policy, kept) in [(true, false), (false, true)] {
            set_maintenance(true).await;
            mark_shutdown(
                &cluster,
                NodeShutdown {
                    action: ShutdownAction::Migrate,
                    maintenance: by_policy,
                    at: 7,
                },
            )
            .await
            .unwrap();
            mark_offline(&cluster).await;
            assert_eq!(node().status.shutdown.unwrap().at, 7);
            register_node(&cluster, &config, &host).await.unwrap();
            let after = node();
            assert_eq!(after.status.shutdown, None);
            assert!(after.status.online);
            assert_eq!(after.spec.maintenance, kept, "set by policy: {by_policy}");
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
