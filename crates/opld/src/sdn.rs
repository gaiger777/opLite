//! 노드 SDN 에이전트(계획 65 §7.2). 위트니스가 아닌 Linux 노드에서 돌며 `Sdn` 리소스가 있을 때만 일한다(없으면 꾸러미를
//! 찾지도 않는다 — 배포만 한 노드는 지금과 같다).
//!
//! - 이 노드가 이 클러스터의 chassis면(OVS DB `external_ids:orbivisor-cluster`가 내 태그) 노드 TLS 사본을 다시 쓰고(인증서를
//!   다시 서명했으면), `ovn-remote`를 중앙 목록에, `ovn-encap-ip`를 underlay에 맞추고, 장치가 없는 이 클러스터의 OVS
//!   포트(재부팅 뒤 남은 것)를 지운다.
//! - 이 노드가 이 클러스터의 chassis면 physnet 연결(veth·OVS 브리지·`ovn-bridge-mappings`)과 게이트웨이 표시
//!   (`ovn-cms-options`)를 `NodeSpec.sdn`에 맞춘다(계획 66 §7 — 관리자 Linux 브리지에 veth만 매단다).
//! - 이 노드가 중앙이면 Orbivisor 단위를 맞추고(northd의 중앙 목록) 멈췄으면 켜며, 꾸러미를 올려 스키마가 새로워졌으면
//!   리더에서 변환한다. 이 노드에서 중앙 Task가 도는 동안(`SdnStatus.pending`)은 단위를 건드리지 않는다.
//! - 읽은 상태(`NodeSdn`)는 [`SdnReport`]에 두고 검사 루프(`checks.rs`)가 저장된 값과 다를 때만, 모든 노드가 `sdn`을 알릴
//!   때만 노드 상태에 쓴다(계획 63·64와 같은 까닭).
//!
//! 언제: 시작, `Sdn` 리소스나 이 노드의 underlay·`NodeSpec.sdn`이 바뀔 때(1초 모아 한 번), 그리고 `refresh`마다(기본 60초).

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_driver::ovn::{Chassis, ChassisProbe, Db, OvnHost};
use opl_types::network::tap_name;
use opl_types::node::{NodeSpec, SDN, SDN_ROUTING};
use opl_types::resource::Kind;
use opl_types::sdn::{
    ChassisState, NodeSdn, NodeSdnConfig, PhysnetState, SDN_NAME, SdnCentral, SdnPending, SdnPhase,
    SdnSpec,
};
use opl_types::vm::VmSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// `Sdn`·`Node`가 바뀌면 이만큼 모아 한 번에 맞춘다.
const DEBOUNCE: Duration = Duration::from_secs(1);

/// 에이전트가 마지막으로 읽은 이 노드의 SDN 상태. 검사 루프가 노드 상태에 쓴다.
#[derive(Debug, Clone, Default)]
pub struct SdnReport(Arc<Mutex<Option<NodeSdn>>>);

impl SdnReport {
    pub fn get(&self) -> Option<NodeSdn> {
        self.0.lock().unwrap().clone()
    }

    pub(crate) fn set(&self, value: Option<NodeSdn>) {
        *self.0.lock().unwrap() = value;
    }
}

/// 모든 노드가 `sdn`을 알 때만 `NodeStatus.sdn`을 쓴다(옛 리더가 모르는 칸을 지우고 다시 쓰기를 되풀이하지 않게).
pub fn sdn_ready(cluster: &Cluster, me: &str) -> bool {
    match opl_api::cluster::missing_feature(cluster, SDN) {
        Ok(old) => old.iter().all(|n| n == me),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the node features");
            false
        }
    }
}

/// 모든 노드가 `sdn-routing`을 알 때만 `NodeSdn.physnets`·`gateway`를 쓴다(계획 66 §7 — 옛 리더가 모르는 칸을 지우고
/// 다시 쓰기를 되풀이하지 않게).
pub fn routing_ready(cluster: &Cluster, me: &str) -> bool {
    match opl_api::cluster::missing_feature(cluster, SDN_ROUTING) {
        Ok(old) => old.iter().all(|n| n == me),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the node features");
            false
        }
    }
}

/// 노드 SDN 에이전트 작업
pub struct SdnAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    host: Arc<OvnHost>,
}

impl SdnAgent {
    /// OVN을 세울 수 있는 노드(Linux)에서만 띄운다.
    pub fn spawn(
        cluster: Arc<Cluster>,
        host: Arc<OvnHost>,
        report: SdnReport,
        refresh: Duration,
    ) -> Option<Self> {
        host.check_platform().ok()?;
        let NodeId(node) = cluster.id();
        let agent = Agent {
            cluster,
            host: host.clone(),
            node,
            report,
        };
        let (shutdown, rx) = oneshot::channel();
        Some(Self {
            shutdown,
            task: tokio::spawn(run(agent, refresh, rx)),
            host,
        })
    }

    /// 멈춘다. 클러스터에서 빠졌으면(`left`) chassis 구성의 내 칸(`orbivisor-cluster`·`ovn-remote`)만 지우고
    /// ovn-controller를 멈춘다(꾸러미·DB는 남김). 지우지 못하면 경고만 남긴다.
    pub async fn shutdown(self, left: bool) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
        if !left {
            return;
        }
        match self.host.unconfigure_chassis().await {
            Ok(true) => tracing::info!("removed this node's OVN chassis settings"),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "cannot remove the OVN chassis settings"),
        }
    }
}

/// 맞추기를 다시 할지 정하는 입력: 중앙 목록, 자리, 이 노드의 underlay와 SDN 설정(physnet·게이트웨이). 이것이 바뀌지 않은
/// `Node` 변경(검사 결과 등)으로는 다시 맞추지 않는다.
type Inputs = Option<(
    Vec<SdnCentral>,
    Option<SdnPending>,
    Option<IpAddr>,
    Option<NodeSdnConfig>,
)>;

struct Agent {
    cluster: Arc<Cluster>,
    host: Arc<OvnHost>,
    node: Uuid,
    report: SdnReport,
}

impl Agent {
    fn inputs(&self) -> Inputs {
        let sdn = self.cluster.get_by_name::<SdnSpec>(SDN_NAME).ok()??;
        let me = self.cluster.get::<NodeSpec>(self.node).ok()??;
        Some((
            sdn.status.centrals,
            sdn.status.pending,
            me.status.underlay,
            me.spec.sdn,
        ))
    }

    /// 한 번 맞추고 읽는다. `Sdn` 리소스가 없거나 이 노드가 위트니스면 아무것도 하지 않는다.
    async fn tick(&self) {
        let sdn = match self.cluster.get_by_name::<SdnSpec>(SDN_NAME) {
            Ok(Some(sdn)) => sdn,
            Ok(None) => return self.report.set(None),
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the SDN");
                return;
            }
        };
        let me = match self.cluster.get::<NodeSpec>(self.node) {
            Ok(Some(me)) if !me.spec.witness => me,
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read this node");
                return;
            }
        };
        let host = &*self.host;
        let mut errors: Vec<String> = Vec::new();
        let versions = match host.versions().await {
            Ok(Some(v)) => v,
            // OVN이 깔리지 않은 노드(chassis가 아님)
            Ok(None) => {
                return self.report.set(Some(NodeSdn::default()));
            }
            Err(e) => {
                self.report.set(Some(NodeSdn {
                    error: Some(e.to_string()),
                    ..NodeSdn::default()
                }));
                return;
            }
        };
        let centrals: Vec<IpAddr> = sdn.status.centrals.iter().map(|c| c.address).collect();
        let mine = sdn.status.centrals.iter().find(|c| c.node == self.node);
        // 이 노드에서 중앙 Task가 돈다(그 Task가 단위·DB를 다룬다).
        let busy = sdn
            .status
            .pending
            .as_ref()
            .is_some_and(|p| p.node == self.node);

        let configured = matches!(
            host.external_ids().await,
            Ok(Some(ids)) if ids.get(opl_driver::ovn::TAG_KEY).map(String::as_str) == Some(host.tag())
        );
        if configured || mine.is_some() {
            match host.write_tls() {
                Ok(true) => tracing::info!("updated the OVN TLS copy"),
                Ok(false) => {}
                Err(e) => errors.push(format!("cannot copy the node TLS: {e}")),
            }
        }

        let central = match mine {
            Some(c) if !busy => {
                self.keep_central(c.address, &centrals, &mut errors).await;
                Some(host.central_health().await)
            }
            Some(_) => Some(host.central_health().await),
            // 중앙 목록에 없지만 이 SDN의 DB가 있다: 끝나지 않은 중앙 더하기가 남긴 멤버일 수 있다. 알려서 중앙 빼기로
            // 떠나게 한다(리뷰 1 I-7 — 꺼진 채 지우면 다른 중앙이 내쫓는다, 리뷰 2 N-2). 단위는 맞추지 않는다. 세우는 중
            // (첫 세우기 노드의 DB는 아직 목록에 없다)과 이 노드에서 중앙 Task가 도는 동안(합류 중)은 알리지 않는다.
            None if sdn.status.phase == SdnPhase::Ready && !busy => {
                let known = opl_driver::ovn::KnownCids {
                    nb: sdn.status.nb_cid,
                    sb: sdn.status.sb_cid,
                };
                if host.has_cluster_db(known).await {
                    Some(host.central_health().await)
                } else {
                    None
                }
            }
            None => None,
        };

        // physnet 연결과 게이트웨이 표시(계획 66 §7): 이 클러스터의 chassis일 때만
        let wanted = me.spec.sdn.as_ref();
        let (physnets, gateway) = if configured {
            match Box::pin(host.reconcile_physnets(wanted)).await {
                Ok(report) => {
                    errors.extend(report.errors);
                    (report.physnets, report.gateway)
                }
                Err(e) => {
                    let e = format!("cannot connect the physnets: {e}");
                    errors.push(e.clone());
                    (not_ready(wanted, &e), false)
                }
            }
        } else {
            (
                not_ready(wanted, "this node is not an SDN (OVN) chassis"),
                false,
            )
        };

        if configured {
            match me.status.underlay {
                Some(encap_ip) if !centrals.is_empty() => {
                    let chassis = Chassis {
                        node: self.node,
                        centrals: &centrals,
                        encap_ip,
                    };
                    match host.reconcile_chassis(&chassis).await {
                        Ok(true) => tracing::info!("updated the OVN chassis settings"),
                        Ok(false) => {}
                        Err(e) => errors.push(format!("cannot update the OVN chassis: {e}")),
                    }
                }
                Some(_) => {}
                None => errors.push("this node has no underlay address".into()),
            }
            match host.remove_stale_ports(&self.running_taps()).await {
                Ok(removed) if !removed.is_empty() => {
                    tracing::info!(ports = ?removed, "removed stale OVS ports")
                }
                Ok(_) => {}
                Err(e) => errors.push(format!("cannot clean up OVS ports: {e}")),
            }
        }

        let (chassis, encap_ip) = match host.chassis_probe(self.node, &centrals).await {
            ChassisProbe::NotConfigured => (ChassisState::Missing, None),
            ChassisProbe::Configured {
                connected,
                registered,
                encap_ip,
            } => {
                let state = match (connected, registered) {
                    (false, _) => ChassisState::Down,
                    (true, Some(false)) => ChassisState::Configuring,
                    (true, _) => ChassisState::Ready,
                };
                (state, encap_ip)
            }
        };
        if !errors.is_empty() {
            tracing::warn!(errors = ?errors, "SDN agent");
        }
        self.report.set(Some(NodeSdn {
            physnets,
            gateway,
            chassis,
            ovn_version: Some(versions.ovn),
            ovs_version: Some(versions.ovs),
            encap_ip,
            central,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        }));
    }

    /// 중앙 노드의 단위를 맞춘다: 파일(northd의 중앙 목록), 바뀐 단위 다시 시작, 멈춘 단위 켜기, 스키마 변환
    async fn keep_central(&self, address: IpAddr, centrals: &[IpAddr], errors: &mut Vec<String>) {
        let host = &*self.host;
        match host.write_units(address, centrals).await {
            Ok(changed) if !changed.is_empty() => {
                tracing::info!(units = ?changed, "updated the OVN central units");
                if let Err(e) = host.start_central(&changed).await {
                    errors.push(format!("cannot restart the OVN central units: {e}"));
                }
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("cannot write the OVN central units: {e}")),
        }
        match host.ensure_central_running().await {
            Ok(started) if !started.is_empty() => {
                tracing::warn!(units = ?started, "started stopped OVN central units")
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("cannot start the OVN central units: {e}")),
        }
        for db in Db::ALL {
            match host.convert_if_needed(db).await {
                Ok(true) => tracing::info!(db = db.name(), "converted the OVN database schema"),
                Ok(false) => {}
                // 서버가 막 켜지는 중일 수 있다. 다음 차례에 다시 본다.
                Err(e) => {
                    tracing::debug!(db = db.name(), error = %e, "cannot check the OVN schema")
                }
            }
        }
    }

    /// 이 노드에 배정된 VM의 tap 이름(청소에서 지우지 않는다)
    fn running_taps(&self) -> BTreeSet<String> {
        let Ok((_, vms)) = self.cluster.list::<VmSpec>() else {
            return BTreeSet::new();
        };
        vms.iter()
            .filter(|vm| vm.status.node == Some(self.node))
            .flat_map(|vm| (0..vm.spec.nics.len()).map(|i| tap_name(vm.id, i)))
            .collect()
    }
}

/// 원하는 physnet마다(이름 순) 준비되지 않음과 그 까닭
fn not_ready(wanted: Option<&NodeSdnConfig>, error: &str) -> Vec<PhysnetState> {
    wanted
        .into_iter()
        .flat_map(|c| &c.physnets)
        .map(|(name, bridge)| PhysnetState {
            name: name.clone(),
            bridge: bridge.clone(),
            ready: false,
            error: Some(error.to_string()),
        })
        .collect()
}

async fn run(agent: Agent, refresh: Duration, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(refresh);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watch: Option<Watch> = None;
    let mut last: Inputs = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
            () = changed(&mut watch) => {
                tokio::select! {
                    _ = &mut shutdown => return,
                    () = tokio::time::sleep(DEBOUNCE) => {}
                }
                watch = None;
                // 검사 결과처럼 상관없는 노드 상태 변경은 건너뛴다(주기 차례가 따로 있다).
                if agent.inputs() == last {
                    resubscribe(&agent, &mut watch);
                    continue;
                }
            }
        }
        resubscribe(&agent, &mut watch);
        last = agent.inputs();
        // 하던 맞추기는 끊지 않는다. 종료는 차례 사이에서만 본다.
        agent.tick().await;
    }
}

/// 이번에 읽을 상태 뒤의 변경을 놓치지 않게 먼저 구독한다.
fn resubscribe(agent: &Agent, watch: &mut Option<Watch>) {
    if watch.is_none() {
        *watch = agent
            .cluster
            .list::<SdnSpec>()
            .ok()
            .and_then(|(revision, _)| agent.cluster.watch(revision).ok());
    }
}

/// `Sdn`이나 `Node`가 바뀔 때까지 기다린다. 구독이 끊기면 버리고 돌아간다. 구독이 없으면 기다리기만 한다.
async fn changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return std::future::pending().await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Sdn | Kind::Node,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Sdn | Kind::Node,
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
    use std::sync::Arc;

    use async_trait::async_trait;
    use opl_driver::DriverError;
    use opl_driver::ovn::{Cmd, CmdOutput, CommandRunner, FakeRunner, OvnOptions, OvnPaths};
    use opl_types::resource::Resource;
    use opl_types::sdn::{SdnPhase, SdnStatus};

    use super::*;
    use crate::bootstrap::tests::single_node;

    const TAG: &str = "abcdef012345";
    const OVN_VERSION: &str =
        include_str!("../../opl-driver/src/ovn/fixtures/ovn-controller-version.txt");
    const OVS_VERSION: &str =
        include_str!("../../opl-driver/src/ovn/fixtures/ovs-vsctl-version.txt");
    const NB_LEADER: &str =
        include_str!("../../opl-driver/src/ovn/fixtures/cluster-status-nb-leader.txt");

    /// `missing`이 참이면 OVN 프로그램이 없는 노드다.
    struct Runner {
        fake: FakeRunner,
        missing: bool,
    }

    #[async_trait]
    impl CommandRunner for Runner {
        async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
            if self.missing {
                return Err(DriverError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "missing",
                )));
            }
            self.fake.run(cmd).await
        }
    }

    struct Setup {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
        cluster: Arc<Cluster>,
        runner: Arc<Runner>,
        agent: Agent,
    }

    /// 이 노드(Node 리소스, 기능 `sdn`)가 하나인 클러스터
    async fn node_cluster() -> (tempfile::TempDir, Arc<Cluster>) {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(node)) = single_node(&dir.path().join("cluster")).await;
        let mut me: opl_types::node::Node = Resource::new(
            "n1",
            NodeSpec {
                sdn: None,
                address: "10.0.0.1:8006".into(),
                maintenance: false,
                witness: false,
                roles: None,
                host_resources: None,
            },
        );
        me.id = node;
        me.status.features = vec![SDN.into()];
        cluster.create(&me).await.unwrap();
        (dir, Arc::new(cluster))
    }

    async fn setup(missing: bool) -> Setup {
        let (dir, cluster) = node_cluster().await;
        let root = dir.path().join("ovn");
        let paths = OvnPaths {
            etc: root.join("etc"),
            var_lib: root.join("var"),
            run: root.join("run"),
            log: root.join("log"),
            systemd: root.join("systemd"),
            pki: root.join("pki"),
            schemas: root.join("share"),
            distro_nb_db: root.join("distro/ovnnb_db.db"),
            ovs_system_id: root.join("system-id.conf"),
        };
        std::fs::create_dir_all(&paths.pki).unwrap();
        for f in ["ca.crt", "node.crt", "node.key"] {
            std::fs::write(paths.pki.join(f), f).unwrap();
        }
        let runner = Arc::new(Runner {
            fake: FakeRunner::new(),
            missing,
        });
        let options = OvnOptions {
            any_os: true,
            ..OvnOptions::default()
        };
        let host = Arc::new(OvnHost::new(runner.clone(), paths, options, TAG.into()));
        let NodeId(node) = cluster.id();
        let agent = Agent {
            cluster: cluster.clone(),
            host,
            node,
            report: SdnReport::default(),
        };
        Setup {
            _dir: dir,
            root,
            cluster,
            runner,
            agent,
        }
    }

    async fn create_sdn(cluster: &Cluster, status: SdnStatus) {
        let mut sdn: opl_types::sdn::Sdn = Resource::new(SDN_NAME, SdnSpec {});
        sdn.status = status;
        cluster.create(&sdn).await.unwrap();
    }

    async fn set_underlay(cluster: &Cluster, node: Uuid, ip: &str) {
        let me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
        let mut status = me.status;
        status.underlay = Some(ip.parse().unwrap());
        cluster
            .update_status::<NodeSpec>(node, &status)
            .await
            .unwrap();
    }

    fn ids(pairs: &[(&str, &str)]) -> CmdOutput {
        let map: Vec<serde_json::Value> = pairs
            .iter()
            .map(|(k, v)| serde_json::json!([k, v]))
            .collect();
        CmdOutput::ok(
            serde_json::json!({ "data": [[["map", map]]], "headings": ["external_ids"] })
                .to_string(),
        )
    }

    /// `Sdn` 리소스가 없으면 명령을 하나도 부르지 않는다(배포만 한 노드). OVN이 없는 노드는 `missing`이다.
    #[tokio::test]
    async fn does_nothing_before_sdn_init() {
        let s = setup(false).await;
        s.agent.tick().await;
        assert!(s.runner.fake.calls().is_empty());
        assert_eq!(s.agent.report.get(), None);

        let s = setup(true).await;
        create_sdn(&s.cluster, SdnStatus::default()).await;
        s.agent.tick().await;
        assert_eq!(s.agent.report.get(), Some(NodeSdn::default()));
    }

    /// 중앙이자 chassis: TLS 사본, 단위 파일, 멈춘 단위 켜기, chassis 맞추기, 청소, 상태 읽기
    #[tokio::test]
    async fn keeps_a_central_chassis() {
        let s = setup(false).await;
        let node = s.agent.node;
        set_underlay(&s.cluster, node, "10.1.0.5").await;
        create_sdn(
            &s.cluster,
            SdnStatus {
                phase: SdnPhase::Ready,
                centrals: vec![
                    SdnCentral {
                        node,
                        address: "10.0.0.1".parse().unwrap(),
                    },
                    SdnCentral {
                        node: Uuid::nil(),
                        address: "10.0.0.2".parse().unwrap(),
                    },
                ],
                ..SdnStatus::default()
            },
        )
        .await;
        let f = &s.runner.fake;
        f.reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        f.reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
        f.reply(
            "ovs-vsctl --format=json --columns=external_ids",
            ids(&[
                (opl_driver::ovn::TAG_KEY, TAG),
                ("ovn-remote", "ssl:10.0.0.1:6642"),
                ("ovn-encap-ip", "10.1.0.5"),
            ]),
        );
        f.reply(
            "systemctl is-active opl-ovn-sb.service",
            CmdOutput::fail(3, ""),
        );
        f.reply("ovs-appctl", CmdOutput::ok(NB_LEADER));
        f.reply("ovsdb-client needs-conversion", CmdOutput::ok("no\n"));
        f.reply(
            "ovn-appctl --timeout 5 -t ovn-controller",
            CmdOutput::ok("connected\n"),
        );
        f.reply(
            "ovs-vsctl --format=json --columns=name,ofport",
            CmdOutput::ok(
                r#"{"data":[["opstale",-1,["map",[["orbivisor-cluster","abcdef012345"]]]]],"headings":["name","ofport","external_ids"]}"#,
            ),
        );
        f.reply(
            "ovn-sbctl",
            CmdOutput::ok(format!(r#"{{"data":[["{node}"]],"headings":["name"]}}"#)),
        );
        f.reply(
            "ovs-vsctl --format=json --columns=name,external_ids find Bridge",
            CmdOutput::ok(r#"{"data":[],"headings":["name","external_ids"]}"#),
        );
        s.agent.tick().await;

        let report = s.agent.report.get().unwrap();
        assert_eq!(report.chassis, ChassisState::Ready);
        // physnet이 없으면 매핑·옵션을 건드리지 않는다.
        assert!(report.physnets.is_empty() && !report.gateway);
        assert!(f.calls().iter().all(|c| !c.contains("ovn-bridge-mappings")
            && !c.contains("ovn-cms-options")
            && !c.starts_with("ip ")));
        assert_eq!(report.ovn_version.as_deref(), Some("24.03.8"));
        assert_eq!(report.ovs_version.as_deref(), Some("3.3.9"));
        assert_eq!(report.encap_ip, Some("10.1.0.5".parse().unwrap()));
        let central = report.central.unwrap();
        assert_eq!(central.nb, opl_types::sdn::RaftRole::Leader);
        assert_eq!(report.error, None);

        let calls = f.calls();
        assert!(
            calls.contains(&"systemctl start opl-ovn-sb.service".to_string()),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("ovs-vsctl set Open_vSwitch")
                    && c.contains(
                        "external_ids:ovn-remote=\"ssl:10.0.0.1:6642,ssl:10.0.0.2:6642\""
                    )),
            "{calls:?}"
        );
        assert!(calls.contains(&"ovs-vsctl --if-exists del-port br-int opstale".to_string()));
        let northd =
            std::fs::read_to_string(s.root.join("systemd/opl-ovn-northd.service")).unwrap();
        assert!(northd.contains("--ovnnb-db=ssl:10.0.0.1:6641,ssl:10.0.0.2:6641 "));
        assert_eq!(
            std::fs::read_to_string(s.root.join("etc/node.key")).unwrap(),
            "node.key"
        );

        // 이 노드에서 중앙 Task가 돌면 단위를 건드리지 않는다.
        let sdn = s.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        let mut status = sdn.status;
        status.pending = Some(SdnPending {
            op: opl_types::task::TaskOp::SdnCentralRemove,
            node,
            task: Uuid::now_v7(),
        });
        s.cluster
            .update_status::<SdnSpec>(sdn.id, &status)
            .await
            .unwrap();
        let before = f.calls().len();
        s.agent.tick().await;
        let after: Vec<String> = f.calls()[before..].to_vec();
        assert!(
            !after.iter().any(|c| c.starts_with("systemctl")),
            "{after:?}"
        );
    }

    /// 이 클러스터의 chassis가 아닌 노드: 맞추지 않고 `missing`
    #[tokio::test]
    async fn a_node_that_is_not_a_chassis() {
        let s = setup(false).await;
        create_sdn(
            &s.cluster,
            SdnStatus {
                phase: SdnPhase::Ready,
                centrals: vec![SdnCentral {
                    node: Uuid::nil(),
                    address: "10.0.0.2".parse().unwrap(),
                }],
                ..SdnStatus::default()
            },
        )
        .await;
        let f = &s.runner.fake;
        f.reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        f.reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
        f.reply(
            "ovs-vsctl --format=json --columns=external_ids",
            ids(&[("system-id", "x")]),
        );
        s.agent.tick().await;
        let report = s.agent.report.get().unwrap();
        assert_eq!(report.chassis, ChassisState::Missing);
        assert_eq!(report.central, None);
        assert!(f.calls().iter().all(|c| !c.starts_with("ovs-vsctl set")
            && !c.starts_with("systemctl")
            && !c.starts_with("ovs-vsctl --if-exists")));
        assert!(!s.root.join("etc/node.key").exists());
    }

    /// 중앙 목록에 없지만 이 SDN의 DB를 가진 노드(끝나지 않은 중앙 더하기)는 중앙 상태를 알린다(리뷰 1 I-7 — API가
    /// 노드 지우기를 막고 중앙 빼기를 받는다). 단위는 맞추지 않는다.
    #[tokio::test]
    async fn a_half_joined_member_reports_its_databases() {
        let s = setup(false).await;
        let cid = Uuid::now_v7();
        create_sdn(
            &s.cluster,
            SdnStatus {
                phase: SdnPhase::Ready,
                centrals: vec![SdnCentral {
                    node: Uuid::nil(),
                    address: "10.0.0.2".parse().unwrap(),
                }],
                nb_cid: Some(cid),
                ..SdnStatus::default()
            },
        )
        .await;
        let f = &s.runner.fake;
        f.reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        f.reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
        f.reply(
            "ovs-vsctl --format=json --columns=external_ids",
            ids(&[("system-id", "x")]),
        );
        s.agent.tick().await;
        assert_eq!(s.agent.report.get().unwrap().central, None);

        std::fs::create_dir_all(s.root.join("var")).unwrap();
        std::fs::write(s.root.join("var/nb.db"), "nb").unwrap();
        f.reply("ovsdb-tool db-cid", CmdOutput::ok(format!("{cid}\n")));
        f.reply("ovs-appctl", CmdOutput::ok(NB_LEADER));
        s.agent.tick().await;
        assert!(s.agent.report.get().unwrap().central.is_some());
        assert!(f.calls().iter().all(|c| !c.starts_with("systemctl")));
        // 세우는 중이거나 이 노드에서 중앙 Task가 돌면(합류 중) 알리지 않는다(리뷰 2 N-2).
        let sdn = s.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        for status in [
            SdnStatus {
                phase: SdnPhase::Bootstrapping,
                ..sdn.status.clone()
            },
            SdnStatus {
                pending: Some(SdnPending {
                    op: opl_types::task::TaskOp::SdnCentralAdd,
                    node: s.agent.node,
                    task: Uuid::now_v7(),
                }),
                ..sdn.status.clone()
            },
        ] {
            s.cluster
                .update_status::<SdnSpec>(sdn.id, &status)
                .await
                .unwrap();
            s.agent.tick().await;
            assert_eq!(s.agent.report.get().unwrap().central, None);
        }
        s.cluster
            .update_status::<SdnSpec>(sdn.id, &sdn.status)
            .await
            .unwrap();
        // 다른 클러스터의 DB면 아니다.
        f.reply(
            "ovsdb-tool db-cid",
            CmdOutput::ok(format!("{}\n", Uuid::now_v7())),
        );
        s.agent.tick().await;
        assert_eq!(s.agent.report.get().unwrap().central, None);
    }

    async fn set_node_sdn(cluster: &Cluster, node: Uuid, sdn: Option<NodeSdnConfig>) {
        let me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
        let mut spec = me.spec;
        spec.sdn = sdn;
        cluster
            .update_spec(node, None, me.labels, &spec)
            .await
            .unwrap();
    }

    /// 계획 66 §7: 이 클러스터의 chassis면 `NodeSpec.sdn`의 physnet을 잇고 게이트웨이를 표시해 알린다. chassis가 아니면
    /// 아무것도 만들지 않고 준비되지 않음으로 알린다. `NodeSpec.sdn`이 바뀌면 맞추기 입력이 바뀐다.
    #[tokio::test]
    async fn connects_physnets_on_a_chassis() {
        let s = setup(false).await;
        let node = s.agent.node;
        set_underlay(&s.cluster, node, "10.1.0.5").await;
        create_sdn(
            &s.cluster,
            SdnStatus {
                phase: SdnPhase::Ready,
                centrals: vec![SdnCentral {
                    node: Uuid::nil(),
                    address: "10.0.0.2".parse().unwrap(),
                }],
                ..SdnStatus::default()
            },
        )
        .await;
        let before = s.agent.inputs();
        let config = NodeSdnConfig {
            physnets: [("ext".to_string(), "vmbr1".to_string())].into(),
            gateway: Some(100),
        };
        set_node_sdn(&s.cluster, node, Some(config)).await;
        assert_ne!(s.agent.inputs(), before);

        let f = &s.runner.fake;
        f.reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        f.reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
        // chassis가 아님
        f.reply(
            "ovs-vsctl --format=json --columns=external_ids",
            ids(&[("system-id", "x")]),
        );
        s.agent.tick().await;
        let report = s.agent.report.get().unwrap();
        assert_eq!(
            report.physnets,
            [PhysnetState {
                name: "ext".into(),
                bridge: "vmbr1".into(),
                ready: false,
                error: Some("this node is not an SDN (OVN) chassis".into()),
            }]
        );
        assert!(!report.gateway);
        assert!(f.calls().iter().all(|c| !c.starts_with("ip link")));

        f.reply(
            "ovs-vsctl --format=json --columns=external_ids",
            ids(&[(opl_driver::ovn::TAG_KEY, TAG)]),
        );
        let empty = |headings: serde_json::Value| {
            CmdOutput::ok(serde_json::json!({ "data": [], "headings": headings }).to_string())
        };
        f.reply(
            "ovs-vsctl --format=json --columns=name,external_ids find Bridge",
            empty(serde_json::json!(["name", "external_ids"])),
        );
        f.reply(
            "ovs-vsctl --format=json --columns=name,ofport,external_ids",
            empty(serde_json::json!(["name", "ofport", "external_ids"])),
        );
        f.reply(
            "ip -j link show",
            CmdOutput::ok(r#"[{"ifname":"vmbr1","mtu":1500,"flags":["UP"]}]"#),
        );
        f.reply(
            "ip -j -d link show type bridge",
            CmdOutput::ok(
                r#"[{"ifname":"vmbr1","mtu":1500,"linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":0}}}]"#,
            ),
        );
        s.agent.tick().await;
        let report = s.agent.report.get().unwrap();
        assert_eq!(report.physnets.len(), 1);
        assert!(report.physnets[0].ready, "{report:?}");
        assert!(report.gateway);
        let calls = f.calls();
        let (_, veth, bridge) = opl_types::sdn::physnet_ifnames(TAG, "ext");
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("ip link add") && c.contains(&veth)),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.contains(&format!(
                "external_ids:ovn-bridge-mappings=\"ext:{bridge}\""
            ))),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.contains("external_ids:ovn-cms-options=\"enable-chassis-as-gw\"")),
            "{calls:?}"
        );
    }

    /// 기능 `sdn`을 모르는 노드가 있으면 쓰지 않는다.
    #[tokio::test]
    async fn the_report_waits_for_the_feature() {
        let (_dir, cluster) = node_cluster().await;
        let NodeId(node) = cluster.id();
        let me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
        let name = me.name.clone();
        let mut status = me.status;
        status.features.retain(|f| f != SDN);
        cluster
            .update_status::<NodeSpec>(node, &status)
            .await
            .unwrap();
        assert!(sdn_ready(&cluster, &name));
        let mut other: opl_types::node::Node = Resource::new(
            "old",
            NodeSpec {
                sdn: None,
                address: "10.0.0.9:8006".into(),
                maintenance: false,
                witness: false,
                roles: None,
                host_resources: None,
            },
        );
        other.status.features.clear();
        cluster.create(&other).await.unwrap();
        assert!(!sdn_ready(&cluster, &name));
        let mut status = other.status.clone();
        status.features.push(SDN.into());
        cluster
            .update_status::<NodeSpec>(other.id, &status)
            .await
            .unwrap();
        assert!(sdn_ready(&cluster, &name));
    }
}
