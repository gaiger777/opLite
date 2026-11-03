//! 오버레이 에이전트. 클러스터의 VXLAN 네트워크마다 이 노드에 브리지 `oplv<vni>`와 VXLAN 장치
//! `oplx<vni>`를 만들고, 다른 노드의 underlay를 FDB에 둔다. 장치를 만들거나 지우면 곧바로 브리지
//! 목록을 다시 기록해 스케줄러가 새 네트워크를 쓸 수 있게 한다.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_driver::{HostNet, Overlay};
use opl_types::network::{Network, NetworkSpec};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Kind;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::host::Host;

/// 바뀐 것이 없어도 장치를 다시 맞추는 간격(누가 장치를 지웠거나 바꿨을 때)
const OVERLAY_REFRESH: Duration = Duration::from_secs(60);
/// Network·Node가 바뀌면 이만큼 모아 한 번에 맞춘다.
const DEBOUNCE: Duration = Duration::from_secs(1);
/// 클러스터 태그의 길이(16진수 글자 수)
const TAG_LEN: usize = 12;

/// 이 클러스터가 만든 장치의 표시(`alias opl:<태그>`). 클러스터 CA 인증서 지문(16진수)의 앞 12자다.
pub fn cluster_tag(ca_pem: &str) -> anyhow::Result<String> {
    let fingerprint = opl_peer::pki::fingerprint(ca_pem)?;
    // `sha256:` 머리를 떼고 16진수 글자만 쓴다.
    let hex = fingerprint.rsplit(':').next().unwrap_or_default();
    Ok(hex
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .take(TAG_LEN)
        .collect())
}

/// 이 노드(`me`)에 있어야 할 VXLAN 네트워크. 내 Node가 없거나, 내가 위트니스거나, 내 underlay가
/// 없으면 정할 수 없어 None이다(장치를 건드리지 않는다). 피어는 나 아닌, 위트니스가 아니고 underlay가
/// 있는 노드의 underlay다(내 것과 같은 주소는 뺀다). VNI 순이고, 같은 VNI가 둘이면(동시에 만든 경우)
/// 이름이 앞선 것만 쓴다.
pub fn desired_overlays(networks: &[Network], nodes: &[Node], me: Uuid) -> Option<Vec<Overlay>> {
    let local = nodes
        .iter()
        .find(|n| n.id == me && !n.spec.witness)
        .and_then(|n| n.status.underlay)?;
    let mut peers: Vec<_> = nodes
        .iter()
        .filter(|n| n.id != me && !n.spec.witness)
        .filter_map(|n| n.status.underlay)
        .filter(|&ip| ip != local)
        .collect();
    peers.sort();
    peers.dedup();
    let mut vxlans: Vec<_> = networks
        .iter()
        .filter_map(|n| Some((n.spec.vni()?, &n.name, n.spec.mtu()?)))
        .collect();
    vxlans.sort();
    vxlans.dedup_by_key(|(vni, ..)| *vni);
    let overlays = vxlans
        .into_iter()
        .map(|(vni, _, mtu)| Overlay {
            vni,
            mtu,
            local,
            peers: peers.clone(),
        })
        .collect();
    Some(overlays)
}

/// 오버레이 에이전트 작업
pub struct OverlayAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    /// 클러스터에서 빠질 때 장치를 지우는 데 쓴다.
    net: Arc<dyn HostNet>,
    tag: String,
}

impl OverlayAgent {
    /// 위트니스가 아니고 호스트 네트워크가 있을 때(Linux)만 띄운다.
    pub fn spawn(cluster: Arc<Cluster>, config: &Config, host: &Host, tag: String) -> Option<Self> {
        let net = host.net.clone()?;
        if config.witness {
            return None;
        }
        let NodeId(node) = cluster.id();
        let overlays = Overlays {
            cluster,
            host: host.clone(),
            net: net.clone(),
            node,
            tag: tag.clone(),
            skipping: AtomicBool::new(false),
        };
        let (shutdown, rx) = oneshot::channel();
        Some(Self {
            shutdown,
            task: tokio::spawn(run_overlays(overlays, OVERLAY_REFRESH, rx)),
            net,
            tag,
        })
    }

    /// 멈춘다. 장치는 그대로 둔다(VM이 계속 쓴다). 다만 클러스터에서 빠졌으면(`left`, 이 노드에 배정된
    /// VM이 없다) 이 노드의 VXLAN 장치를 지운다. 지우지 못하면 경고만 남긴다.
    pub async fn shutdown(self, left: bool) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
        if !left {
            return;
        }
        if let Err(e) = self.net.sync_overlays(&self.tag, &[]).await {
            tracing::warn!(error = %e, "cannot remove VXLAN devices after leaving the cluster");
        }
    }
}

/// 한 번 맞추는 데 필요한 것
struct Overlays {
    cluster: Arc<Cluster>,
    /// 브리지 목록을 다시 기록할 때 쓴다.
    host: Host,
    net: Arc<dyn HostNet>,
    node: Uuid,
    tag: String,
    /// 내 underlay를 몰라 건너뛰는 중이다(경고를 한 번만 남긴다).
    skipping: AtomicBool,
}

impl Overlays {
    /// 바라는 장치를 맞추고, 장치가 바뀌었으면 브리지 목록을 기록한다. sync 오류는 여기서 경고로
    /// 남기고, 목록 읽기·브리지 기록 오류를 돌려준다.
    async fn tick(&self) -> anyhow::Result<()> {
        const READ: &str = "cannot read cluster state for VXLAN networks";
        let (_, networks) = self.cluster.list::<NetworkSpec>().context(READ)?;
        let (_, nodes) = self.cluster.list::<NodeSpec>().context(READ)?;
        let Some(desired) = desired_overlays(&networks, &nodes, self.node) else {
            // 잠깐 underlay를 잃었다고 VM이 쓰는 장치를 지우지 않는다.
            if !nodes.iter().any(|n| n.id == self.node) {
                // 첫 부팅에서 Node 리소스를 아직 기록하지 않았다.
                tracing::debug!("this node is not registered yet; skipping VXLAN sync");
            } else if !self.skipping.swap(true, Ordering::Relaxed) {
                tracing::warn!("this node has no underlay; leaving VXLAN devices as they are");
            } else {
                tracing::debug!("this node has no underlay; skipping VXLAN sync");
            }
            return Ok(());
        };
        self.skipping.store(false, Ordering::Relaxed);
        let result = self.net.sync_overlays(&self.tag, &desired).await;
        // 브리지 목록 기록이 실패해도 sync 오류를 먼저 남긴다.
        if let Err(e) = &result {
            tracing::warn!(error = %e, "cannot sync VXLAN networks");
        }
        // 실패해도 다른 네트워크의 장치는 바뀌었을 수 있으므로 브리지 목록을 다시 기록한다.
        if result.as_ref().map_or(true, |c| c.devices_changed) {
            bootstrap::refresh_bridges(&self.cluster, &self.host)
                .await
                .context("cannot record bridges")?;
        }
        Ok(())
    }
}

async fn run_overlays(overlays: Overlays, refresh: Duration, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(refresh);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watch = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
            () = overlay_changed(&mut watch) => {
                // 잇따른 변경을 모은다. 구독이 끊긴 때(Reset·오류)도 같이 기다려 다시 구독하기를 몰아치지
                // 않는다. 모은 변경은 구독을 새로 해 버린다(맞출 때 목록을 다시 읽는다).
                tokio::select! {
                    _ = &mut shutdown => return,
                    () = tokio::time::sleep(DEBOUNCE) => {}
                }
                watch = None;
            }
        }
        // 이번에 읽을 목록 뒤의 변경을 놓치지 않게 먼저 구독한다.
        if watch.is_none() {
            watch = overlays
                .cluster
                .list::<NetworkSpec>()
                .ok()
                .and_then(|(revision, _)| overlays.cluster.watch(revision).ok());
        }
        // 하던 sync는 끊지 않는다(장치를 반쯤 만든 채 두지 않게). 종료는 차례 사이에서만 본다.
        // 목록 읽기나 브리지 기록의 실패(sync 실패는 tick이 남긴다). 메시지는 그 문맥이다.
        if let Err(e) = overlays.tick().await {
            tracing::warn!(error = %e.root_cause(), "{e}");
        }
    }
}

/// Network나 Node 리소스가 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을 버리고
/// 돌아가 맞춘 뒤 다시 구독하게 한다. 구독이 없으면 기다리기만 한다.
async fn overlay_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return std::future::pending().await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Network | Kind::Node,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Network | Kind::Node,
                    ..
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

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use async_trait::async_trait;
    use opl_driver::{DriverError, OverlayChanges, Tap};
    use opl_peer::pki::ClusterCa;
    use opl_types::network::{Vxlan, vxlan_bridge};
    use opl_types::node::{BridgeInfo, NodeStatus};
    use opl_types::resource::Resource;

    use crate::bootstrap::tests::single_node;

    /// 받은 `desired`를 기록하고, 바라는 VXLAN 네트워크의 브리지가 있는 것처럼 브리지 목록을 바꾼다.
    /// `fail`이면 장치는 바꾸되 오류를 낸다(일부 네트워크만 실패한 것처럼). `hold`면 `release`가 올
    /// 때까지 끝내지 않는다.
    #[derive(Debug, Default)]
    struct RecordingNet {
        calls: Mutex<Vec<Vec<Overlay>>>,
        bridges: Mutex<Vec<BridgeInfo>>,
        fail: AtomicBool,
        hold: AtomicBool,
        release: tokio::sync::Notify,
        finished: AtomicUsize,
    }

    #[async_trait]
    impl HostNet for RecordingNet {
        async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
            Ok(self.bridges.lock().unwrap().clone())
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
            desired: &[Overlay],
        ) -> Result<OverlayChanges, DriverError> {
            self.calls.lock().unwrap().push(desired.to_vec());
            if self.hold.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            self.finished.fetch_add(1, Ordering::SeqCst);
            let bridges: Vec<_> = desired
                .iter()
                .map(|o| BridgeInfo {
                    name: vxlan_bridge(o.vni),
                    vlan_filtering: false,
                })
                .collect();
            let mut current = self.bridges.lock().unwrap();
            let devices_changed = *current != bridges;
            *current = bridges;
            if self.fail.load(Ordering::SeqCst) {
                return Err(DriverError::Overlay("boom".into()));
            }
            Ok(OverlayChanges { devices_changed })
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn node(name: &str, witness: bool, underlay: Option<&str>) -> Node {
        let mut node = Resource::new(
            name,
            NodeSpec {
                address: format!("{name}:8006"),
                maintenance: false,
                witness,
            },
        );
        node.status = NodeStatus {
            underlay: underlay.map(ip),
            ..NodeStatus::default()
        };
        node
    }

    fn vxlan(name: &str, vni: u32, mtu: Option<u16>) -> Network {
        Resource::new(
            name,
            NetworkSpec {
                bridge: None,
                vlan: None,
                vxlan: Some(Vxlan { vni }),
                mtu,
            },
        )
    }

    #[test]
    fn the_cluster_tag_is_the_start_of_the_ca_fingerprint() {
        let ca = ClusterCa::generate("c").unwrap();
        let tag = cluster_tag(&ca.cert_pem).unwrap();
        let fingerprint = opl_peer::pki::fingerprint(&ca.cert_pem).unwrap();
        assert_eq!(tag.len(), 12);
        assert!(
            tag.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{tag}"
        );
        assert_eq!(format!("sha256:{tag}"), fingerprint[..19]);
        assert_ne!(
            tag,
            cluster_tag(&ClusterCa::generate("c").unwrap().cert_pem).unwrap()
        );
    }

    #[test]
    fn desired_overlays_cover_vxlan_networks_with_the_other_nodes_as_peers() {
        let me = node("me", false, Some("10.0.0.1"));
        let nodes = vec![
            me.clone(),
            node("b", false, Some("10.0.0.3")),
            node("c", false, Some("10.0.0.2")),
            // 같은 underlay는 한 번만
            node("d", false, Some("10.0.0.3")),
            // 위트니스와 underlay 없는 노드는 피어가 아니다.
            node("w", true, Some("10.0.0.9")),
            node("e", false, None),
            // 내 underlay와 같은 주소는 피어가 아니다.
            node("f", false, Some("10.0.0.1")),
        ];
        let bridge = Resource::new(
            "lan",
            NetworkSpec {
                bridge: Some("vmbr0".into()),
                vlan: None,
                vxlan: None,
                mtu: None,
            },
        );
        let networks = vec![
            vxlan("b200", 200, None),
            bridge,
            vxlan("a100", 100, Some(9000)),
            // 같은 VNI는 이름이 앞선 것만
            vxlan("z100", 100, Some(1400)),
        ];
        let peers = vec![ip("10.0.0.2"), ip("10.0.0.3")];
        assert_eq!(
            desired_overlays(&networks, &nodes, me.id),
            Some(vec![
                Overlay {
                    vni: 100,
                    mtu: 9000,
                    local: ip("10.0.0.1"),
                    peers: peers.clone(),
                },
                Overlay {
                    vni: 200,
                    mtu: 1450,
                    local: ip("10.0.0.1"),
                    peers,
                },
            ])
        );

        // 내 underlay가 없거나, 내가 위트니스거나, 내 Node가 없으면 정할 수 없다(장치를 건드리지
        // 않는다). VXLAN 네트워크가 없으면 빈 목록이다(내 장치를 정리한다).
        let mut no_underlay = nodes.clone();
        no_underlay[0].status.underlay = None;
        assert_eq!(desired_overlays(&networks, &no_underlay, me.id), None);
        let mut witness = nodes.clone();
        witness[0].spec.witness = true;
        assert_eq!(desired_overlays(&networks, &witness, me.id), None);
        assert_eq!(desired_overlays(&networks, &nodes[1..], me.id), None);
        assert_eq!(desired_overlays(&[], &nodes, me.id), Some(vec![]));
        // 혼자면 피어가 없다.
        assert_eq!(
            desired_overlays(&networks[..1], std::slice::from_ref(&me), me.id),
            Some(vec![Overlay {
                vni: 200,
                mtu: 1450,
                local: ip("10.0.0.1"),
                peers: vec![],
            }])
        );
    }

    fn test_config(witness: bool) -> Config {
        toml::from_str(&format!(
            "name = \"n\"\naddress = \"h\"\nwitness = {witness}\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false"
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn runs_only_on_a_non_witness_node_with_host_networking() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let config = test_config(false);
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        assert!(OverlayAgent::spawn(cluster.clone(), &config, &host, "t".into()).is_none());
        host.net = Some(Arc::new(RecordingNet::default()));
        assert!(
            OverlayAgent::spawn(cluster.clone(), &test_config(true), &host, "t".into()).is_none()
        );
        let agent = OverlayAgent::spawn(cluster.clone(), &config, &host, "t".into()).unwrap();
        agent.shutdown(false).await;
        cluster.shutdown().await.unwrap();
    }

    /// 클러스터에서 빠질 때만 이 노드의 VXLAN 장치를 모두 지운다(빈 목록으로 맞춘다).
    #[tokio::test]
    async fn leaving_removes_the_devices() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let config = test_config(false);
        let net = Arc::new(RecordingNet::default());
        let host = Host {
            net: Some(net.clone()),
            ..Host::from_config(&config, "t").unwrap()
        };
        // 내 Node가 없으므로 에이전트는 맞추지 않는다.
        let agent = OverlayAgent::spawn(cluster.clone(), &config, &host, "t".into()).unwrap();
        agent.shutdown(false).await;
        assert!(net.calls.lock().unwrap().is_empty());
        let agent = OverlayAgent::spawn(cluster.clone(), &config, &host, "t".into()).unwrap();
        agent.shutdown(true).await;
        assert_eq!(*net.calls.lock().unwrap(), [Vec::<Overlay>::new()]);
        cluster.shutdown().await.unwrap();
    }

    /// 이 노드의 Node 리소스를 만든다.
    async fn create_me(cluster: &Cluster, me: Uuid, underlay: Option<&str>) {
        let mut mine = node("me", false, underlay);
        mine.id = me;
        cluster.create(&mine).await.unwrap();
        cluster
            .update_status::<NodeSpec>(me, &mine.status)
            .await
            .unwrap();
    }

    /// 조건이 맞을 때까지 기다린다.
    async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(tokio::time::Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 네트워크를 만들거나 지우고 노드가 더해지면 주기를 기다리지 않고 다시 맞추고, 장치가 바뀌면
    /// (일부 실패해도) 브리지 목록을 기록한다.
    #[tokio::test]
    async fn syncs_on_network_and_node_changes_and_records_the_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(me)) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        create_me(&cluster, me, Some("10.0.0.1")).await;
        let config = test_config(false);
        let net = Arc::new(RecordingNet::default());
        let host = Host {
            net: Some(net.clone()),
            ..Host::from_config(&config, "t").unwrap()
        };
        let overlays = Overlays {
            cluster: cluster.clone(),
            host: host.clone(),
            net: net.clone(),
            node: me,
            tag: "abc".into(),
            skipping: AtomicBool::new(false),
        };
        let (stop, rx) = oneshot::channel();
        // 주기는 길게 둔다. 시작할 때 한 번, 그 뒤로는 바뀔 때마다 맞춘다.
        let task = tokio::spawn(run_overlays(overlays, Duration::from_secs(3600), rx));
        let last = || net.calls.lock().unwrap().last().cloned();
        let recorded = || -> Vec<String> {
            let node = cluster.get::<NodeSpec>(me).unwrap().unwrap();
            node.status.bridges.into_iter().map(|b| b.name).collect()
        };

        wait_for("first sync", || last() == Some(vec![])).await;
        let first = vxlan("first", 1000, None);
        cluster.create(&first).await.unwrap();
        wait_for("sync after the network is created", || {
            last().is_some_and(|d| d.len() == 1 && d[0].vni == 1000 && d[0].peers.is_empty())
        })
        .await;
        wait_for("bridges recorded", || recorded() == ["oplv1000"]).await;

        cluster
            .create(&node("peer", false, Some("10.0.0.2")))
            .await
            .unwrap();
        // 새 노드가 underlay를 기록하면 피어가 된다.
        let (_, nodes) = cluster.list::<NodeSpec>().unwrap();
        let peer = nodes.iter().find(|n| n.name == "peer").unwrap();
        let status = NodeStatus {
            underlay: Some(ip("10.0.0.2")),
            ..NodeStatus::default()
        };
        cluster
            .update_status::<NodeSpec>(peer.id, &status)
            .await
            .unwrap();
        wait_for("sync after the node is added", || {
            last().is_some_and(|d| d.len() == 1 && d[0].peers == [ip("10.0.0.2")])
        })
        .await;

        // 일부 네트워크가 실패해도 바뀐 장치는 기록한다.
        net.fail.store(true, Ordering::SeqCst);
        cluster.create(&vxlan("second", 2000, None)).await.unwrap();
        wait_for("bridges recorded after a failed sync", || {
            recorded() == ["oplv1000", "oplv2000"]
        })
        .await;
        net.fail.store(false, Ordering::SeqCst);

        let (_, networks) = cluster.list::<NetworkSpec>().unwrap();
        for network in networks {
            cluster
                .delete::<NetworkSpec>(network.id, None)
                .await
                .unwrap();
        }
        wait_for("sync after the networks are deleted", || {
            last() == Some(vec![])
        })
        .await;
        wait_for("bridges removed", || recorded().is_empty()).await;
        stop.send(()).unwrap();
        task.await.unwrap();
        cluster.shutdown().await.unwrap();
    }

    /// 내 underlay를 모르면(잠깐 잃은 경우 포함) 맞추지 않는다. 빈 목록으로 맞추면 VM이 쓰는 장치를
    /// 지운다.
    #[tokio::test]
    async fn skips_sync_without_my_underlay() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(me)) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        cluster.create(&vxlan("ov", 1000, None)).await.unwrap();
        let config = test_config(false);
        let net = Arc::new(RecordingNet::default());
        let overlays = Overlays {
            cluster: cluster.clone(),
            host: Host {
                net: Some(net.clone()),
                ..Host::from_config(&config, "t").unwrap()
            },
            net: net.clone(),
            node: me,
            tag: "abc".into(),
            skipping: AtomicBool::new(false),
        };
        // 내 Node가 아직 없으면(첫 부팅) 맞추지 않고, underlay가 없다는 경고도 아직 남기지 않는다.
        overlays.tick().await.unwrap();
        assert!(!overlays.skipping.load(Ordering::Relaxed));
        // Node는 있는데 underlay가 없으면 한 번 경고하고 맞추지 않는다.
        create_me(&cluster, me, None).await;
        overlays.tick().await.unwrap();
        assert!(overlays.skipping.load(Ordering::Relaxed));
        overlays.tick().await.unwrap();
        assert!(net.calls.lock().unwrap().is_empty());

        // underlay를 기록하면 그때부터 맞춘다.
        let status = NodeStatus {
            underlay: Some(ip("10.0.0.1")),
            ..NodeStatus::default()
        };
        cluster
            .update_status::<NodeSpec>(me, &status)
            .await
            .unwrap();
        overlays.tick().await.unwrap();
        let calls = net.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 1);
        assert_eq!(calls[0][0].vni, 1000);
        cluster.shutdown().await.unwrap();
    }

    /// 종료 신호가 와도 하던 sync는 끝까지 한다(장치를 반쯤 만든 채 멈추지 않게).
    #[tokio::test]
    async fn shutdown_waits_for_the_sync_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(me)) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        create_me(&cluster, me, Some("10.0.0.1")).await;
        let config = test_config(false);
        let net = Arc::new(RecordingNet::default());
        net.hold.store(true, Ordering::SeqCst);
        let overlays = Overlays {
            cluster: cluster.clone(),
            host: Host {
                net: Some(net.clone()),
                ..Host::from_config(&config, "t").unwrap()
            },
            net: net.clone(),
            node: me,
            tag: "abc".into(),
            skipping: AtomicBool::new(false),
        };
        let (stop, rx) = oneshot::channel();
        let task = tokio::spawn(run_overlays(overlays, Duration::from_secs(3600), rx));
        wait_for("sync started", || net.calls.lock().unwrap().len() == 1).await;
        stop.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!task.is_finished());
        assert_eq!(net.finished.load(Ordering::SeqCst), 0);
        net.release.notify_one();
        task.await.unwrap();
        assert_eq!(net.finished.load(Ordering::SeqCst), 1);
        assert_eq!(net.calls.lock().unwrap().len(), 1);
        cluster.shutdown().await.unwrap();
    }
}
