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
//! - OVN 접근 잠금(계획 67 §10): 중앙은 자기 차례(`SdnSpec.lockdown`과 다른 첫 중앙이고 다른 중앙의 NB·SB가 살아 있음)에
//!   SB 설정 파일·단위를 바꾸고 SB를 다시 띄워 따라잡은 뒤 `SdnStatus.lockdown.applied`에 적는다(한 번에 중앙 하나 —
//!   정족수 유지). 모든 중앙이 잠기면 chassis·중앙이 nftables 주소 허용 목록(`inet opl_ovn_<태그>`)을 맞춘다. 노드가
//!   빠지거나 들면 1초 모으기를 기다리지 않고 곧바로 집합을 바꾼다.
//! - 이 노드가 중앙이고 자기 SB가 Raft 리더면 클러스터에 없는 노드의 SB `Chassis`·`Chassis_Private`를 로컬 소켓으로
//!   지운다(계획 67 §10.2 — 리더 OvnSync에서 옮겨 옴, 모든 노드가 `sdn-services`를 알 때만). NB·SB에 붙은 상대 주소를
//!   읽어 알린다(잠금 미리 확인).
//! - 이 노드가 단순 → SDN 옮기기의 다리 노드면(끝나지 않은 `NetworkMigration`의 `status.bridge.node`, 계획 67 §9.5) 그 임시
//!   physnet `mig<h8>`을 원본 네트워크의 Linux 브리지에 같은 physnet 길로 잇고 알린다(관리자 설정 `NodeSpec.sdn`은 그대로).
//! - 읽은 상태(`NodeSdn`)는 [`SdnReport`]에 두고 검사 루프(`checks.rs`)가 저장된 값과 다를 때만, 모든 노드가 `sdn`을 알릴
//!   때만 노드 상태에 쓴다(계획 63·64와 같은 까닭).
//!
//! 언제: 시작, `Sdn` 리소스나 노드(이 노드의 underlay·`NodeSpec.sdn`, 허용 집합, 중앙의 상태)가 바뀔 때(1초 모아 한 번),
//! 그리고 `refresh`마다(기본 60초).

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_driver::ovn::lockdown::{LockdownRole, ruleset};
use opl_driver::ovn::{Chassis, ChassisProbe, Db, OvnHost};
use opl_types::network::{NetworkSpec, tap_name};
use opl_types::network_migration::NetworkMigrationSpec;
use opl_types::node::{Node, NodeSpec, SDN, SDN_ROUTING, SDN_SERVICES};
use opl_types::resource::Kind;
use opl_types::sdn::{
    AllowSets, CentralHealth, ChassisState, LOCKDOWN_RAFT_CUT, LockdownNodeState, NodeSdn,
    NodeSdnConfig, PhysnetState, RaftRole, SDN_NAME, Sdn, SdnCentral, SdnPending, SdnPhase,
    SdnSpec, allow_sets,
};
use opl_types::task::TaskOp;
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

/// 모든 노드가 `sdn-services`를 알 때만 계획 67의 노드 상태 칸(`NodeStatus.qos` 따위)을 쓴다(옛 리더가 모르는 칸을
/// 지우고 다시 쓰기를 되풀이하지 않게 — 계획 66 `routing_ready`와 같다).
pub fn services_ready(cluster: &Cluster, me: &str) -> bool {
    match opl_api::cluster::missing_feature(cluster, SDN_SERVICES) {
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
            firewall: Mutex::new(None),
            refused: Mutex::new(None),
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

/// 맞추기를 다시 할지 정하는 입력: 중앙 목록, 자리, 이 노드의 underlay와 SDN 설정(physnet·게이트웨이), 잠금(원하는 것,
/// 잠긴 중앙, 허용 집합, 노드 목록, 중앙들의 온라인·Raft 역할 — 계획 67). 이것이 바뀌지 않은 `Node` 변경(검사 결과
/// 등)으로는 다시 맞추지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Inputs {
    centrals: Vec<SdnCentral>,
    pending: Option<SdnPending>,
    underlay: Option<IpAddr>,
    config: Option<NodeSdnConfig>,
    lockdown: bool,
    applied: Vec<Uuid>,
    sets: AllowSets,
    nodes: Vec<Uuid>,
    health: Vec<(Uuid, bool, Option<CentralHealth>)>,
    /// 이 노드가 놓을 옮기기 다리(physnet → 원본 Linux 브리지)
    bridges: BTreeMap<String, String>,
}

/// 이 노드 `node`가 다리 노드인 끝나지 않은 옮기기의 physnet → 원본 네트워크의 Linux 브리지(계획 67 §9.5)
fn migration_physnets(cluster: &Cluster, node: Uuid) -> BTreeMap<String, String> {
    let (Ok((_, migrations)), Ok((_, networks))) = (
        cluster.list::<NetworkMigrationSpec>(),
        cluster.list::<NetworkSpec>(),
    ) else {
        return BTreeMap::new();
    };
    migrations
        .iter()
        .filter(|m| !m.status.is_done())
        .filter_map(|m| {
            let b = m.status.bridge.as_ref().filter(|b| b.node == node)?;
            let source = networks
                .iter()
                .find(|n| n.name == m.spec.source && !n.spec.is_sdn())?;
            Some((b.physnet.clone(), source.spec.bridge_name()))
        })
        .collect()
}

/// 관리자 physnet 설정 `config`에 옮기기 다리 `bridges`를 더한 것(다리가 없으면 `config` 그대로)
fn with_bridges(
    config: Option<&NodeSdnConfig>,
    bridges: &BTreeMap<String, String>,
) -> Option<NodeSdnConfig> {
    if bridges.is_empty() {
        return config.cloned();
    }
    let mut c = config.cloned().unwrap_or_default();
    for (physnet, bridge) in bridges {
        c.physnets
            .entry(physnet.clone())
            .or_insert_with(|| bridge.clone());
    }
    Some(c)
}

/// 마지막으로 넣은 허용 목록(빠른 길이 쓴다): 글과 이 노드의 몫
#[derive(Debug, Clone, PartialEq, Eq)]
struct Firewall {
    text: String,
    role: LockdownRole,
}

struct Agent {
    cluster: Arc<Cluster>,
    host: Arc<OvnHost>,
    node: Uuid,
    report: SdnReport,
    /// 마지막으로 넣은 허용 목록(없으면 넣지 않았거나 지웠다)
    firewall: Mutex<Option<Firewall>>,
    /// Raft를 끊어 걷은 허용 목록(리뷰 1 I5): 그 글, 그때의 잠금 Task, 까닭. 같은 글은 새 잠금 Task가 오기 전에 다시 넣지
    /// 않는다(넣고 걷기를 되풀이하지 않게).
    refused: Mutex<Option<(String, Option<Uuid>, String)>>,
}

/// 잠금을 바꾸지 않는 Task가 자리를 잡았는지(중앙 더하기·빼기 따위 — 그동안 중앙의 잠금은 그대로 둔다)
fn other_task(pending: Option<&SdnPending>) -> bool {
    pending.is_some_and(|p| !matches!(p.op, TaskOp::SdnLockdown { .. }))
}

/// 모든 중앙의 SB가 잠겼다(허용 목록을 걸 때 — 바뀌지 않은 northd가 6645에 막히는 순간이 없게, 계획 67 §10.4).
fn all_locked(sdn: &Sdn) -> bool {
    let applied = sdn
        .status
        .lockdown
        .as_ref()
        .map(|l| l.applied.as_slice())
        .unwrap_or_default();
    !sdn.status.centrals.is_empty()
        && sdn
            .status
            .centrals
            .iter()
            .all(|c| applied.contains(&c.node))
}

/// 목록에서 나(`me`)보다 앞선 중앙 가운데 아직 허용 목록을 걸었다고 알리지 않은 것의 이름(중앙은 하나씩 건다 — 리뷰 1 I5)
fn earlier_central_without_firewall(sdn: &Sdn, nodes: &[Node], me: Uuid) -> Option<String> {
    for c in sdn.status.centrals.iter().take_while(|c| c.node != me) {
        let node = nodes.iter().find(|n| n.id == c.node);
        let on = node
            .and_then(|n| n.status.sdn.as_ref())
            .and_then(|s| s.lockdown.as_ref())
            .is_some_and(|l| l.firewall);
        if !on {
            return Some(node.map_or_else(|| c.node.to_string(), |n| n.name.clone()));
        }
    }
    None
}

/// 잠긴 중앙(`SdnStatus.lockdown.applied`)의 주소
fn locked_addresses(sdn: &Sdn) -> Vec<IpAddr> {
    let applied = sdn
        .status
        .lockdown
        .as_ref()
        .map(|l| l.applied.clone())
        .unwrap_or_default();
    sdn.status
        .centrals
        .iter()
        .filter(|c| applied.contains(&c.node))
        .map(|c| c.address)
        .collect()
}

/// 이 노드 `me`(중앙)의 잠금을 바꿀 차례인지(계획 67 §10.4): 다른 중앙 Task가 없고, SDN이 준비됐고, 원하는 잠금과 다른
/// 첫 중앙(목록 차례)이 나이고, 다른 중앙이 모두 온라인이며 NB·SB가 리더나 팔로어라고 알렸다(내 SB를 다시 띄워도
/// 정족수가 남는다). 아니면 기다리는 까닭
fn lockdown_turn(sdn: &Sdn, nodes: &[Node], me: Uuid) -> Result<(), String> {
    if other_task(sdn.status.pending.as_ref()) {
        return Err("another SDN central task is running".into());
    }
    if sdn.status.phase != SdnPhase::Ready {
        return Err("SDN (OVN) is not ready".into());
    }
    let want = sdn.spec.lockdown;
    let applied = sdn
        .status
        .lockdown
        .as_ref()
        .map(|l| l.applied.as_slice())
        .unwrap_or_default();
    let first = sdn
        .status
        .centrals
        .iter()
        .find(|c| applied.contains(&c.node) != want);
    match first {
        Some(c) if c.node == me => {}
        Some(c) => return Err(format!("waiting for central {}", c.node)),
        None => return Err("nothing to change".into()),
    }
    let live = |role: RaftRole| matches!(role, RaftRole::Leader | RaftRole::Follower);
    for c in sdn.status.centrals.iter().filter(|c| c.node != me) {
        let node = nodes.iter().find(|n| n.id == c.node);
        let healthy = node.is_some_and(|n| {
            n.status.online
                && n.status
                    .sdn
                    .as_ref()
                    .and_then(|s| s.central)
                    .is_some_and(|h| live(h.nb) && live(h.sb))
        });
        if !healthy {
            let name = node.map_or_else(|| c.node.to_string(), |n| n.name.clone());
            return Err(format!(
                "central {name:?} is offline or its OVN databases are not in the cluster"
            ));
        }
    }
    Ok(())
}

impl Agent {
    fn inputs(&self) -> Option<Inputs> {
        let sdn = self.cluster.get_by_name::<SdnSpec>(SDN_NAME).ok()??;
        let me = self.cluster.get::<NodeSpec>(self.node).ok()??;
        let (_, nodes) = self.cluster.list::<NodeSpec>().ok()?;
        let health = sdn
            .status
            .centrals
            .iter()
            .map(|c| {
                let n = nodes.iter().find(|n| n.id == c.node);
                (
                    c.node,
                    n.is_some_and(|n| n.status.online),
                    n.and_then(|n| n.status.sdn.as_ref()?.central),
                )
            })
            .collect();
        Some(Inputs {
            sets: allow_sets(&nodes, &sdn),
            nodes: nodes.iter().map(|n| n.id).collect(),
            health,
            lockdown: sdn.spec.lockdown,
            applied: sdn.status.lockdown.map(|l| l.applied).unwrap_or_default(),
            centrals: sdn.status.centrals,
            pending: sdn.status.pending,
            underlay: me.status.underlay,
            config: me.spec.sdn,
            bridges: migration_physnets(&self.cluster, self.node),
        })
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
        // 노드 목록을 읽지 못하면 허용 목록·청소를 건너뛴다(빈 목록으로 막거나 지우지 않는다 — §10.3).
        let nodes = match self.cluster.list::<NodeSpec>() {
            Ok((_, nodes)) => Some(nodes),
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the nodes");
                None
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
        // 이 노드에서 중앙 Task가 돈다(그 Task가 단위·DB를 다룬다). 잠금 Task는 중앙을 만지지 않는다.
        let busy = sdn
            .status
            .pending
            .as_ref()
            .is_some_and(|p| p.node == self.node && other_task(Some(p)));

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
                self.keep_central(&sdn, nodes.as_deref(), c.address, &centrals, &mut errors)
                    .await;
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
        let wanted = with_bridges(
            me.spec.sdn.as_ref(),
            &migration_physnets(&self.cluster, self.node),
        );
        let wanted = wanted.as_ref();
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

        // 주소 허용 목록(계획 67 §10.3)
        let role = LockdownRole {
            central: mine.is_some(),
            chassis: configured,
        };
        let lockdown = match &nodes {
            Some(nodes) => self.firewall(&sdn, nodes, role).await,
            None => None,
        };
        // 지운 노드의 SB chassis 청소(계획 67 §10.2): 이 중앙의 SB가 리더일 때만
        if mine.is_some()
            && let Some(nodes) = &nodes
            && services_ready(&self.cluster, &me.name)
            && host
                .cluster_status(Db::Sb)
                .await
                .is_ok_and(|s| s.is_leader())
        {
            let keep = nodes.iter().map(|n| n.id).collect();
            match host.clean_chassis(&keep).await {
                Ok(removed) if !removed.is_empty() => {
                    tracing::info!(chassis = ?removed, "removed the OVN chassis of removed nodes")
                }
                Ok(_) => {}
                Err(e) => errors.push(format!(
                    "cannot remove the OVN chassis of removed nodes: {e}"
                )),
            }
        }
        let (peers, raft_peers) = if mine.is_some() {
            let peers = host.central_peers().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "cannot read the OVN clients");
                Vec::new()
            });
            // Raft 상대(리뷰 1 I5 — 잠금 미리 확인이 중앙 집합과 견준다)
            let raft = host.central_raft_peers().await.unwrap_or_else(|e| {
                tracing::debug!(error = %e, "cannot read the OVN Raft peers");
                Vec::new()
            });
            (peers, raft)
        } else {
            (Vec::new(), Vec::new())
        };

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
            lockdown,
            peers,
            raft_peers,
            chassis,
            ovn_version: Some(versions.ovn),
            ovs_version: Some(versions.ovs),
            encap_ip,
            central,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        }));
    }

    /// 중앙 노드의 단위를 맞춘다: 잠금 차례면 SB를 잠금대로 바꾸고(§10.4), 아니면 파일(northd의 중앙 목록, 잠긴 중앙),
    /// 바뀐 단위 다시 시작, 멈춘 단위 켜기, 스키마 변환
    async fn keep_central(
        &self,
        sdn: &Sdn,
        nodes: Option<&[Node]>,
        address: IpAddr,
        centrals: &[IpAddr],
        errors: &mut Vec<String>,
    ) {
        let host = &*self.host;
        let mut locked = locked_addresses(sdn);
        let applied_me = locked.contains(&address);
        let turn = match nodes {
            Some(nodes) if sdn.spec.lockdown != applied_me => {
                match lockdown_turn(sdn, nodes, self.node) {
                    Ok(()) => true,
                    Err(why) => {
                        tracing::info!(reason = %why, "the OVN lockdown of this central waits");
                        false
                    }
                }
            }
            _ => false,
        };
        if turn {
            return self
                .switch_lockdown(sdn, address, centrals, locked, errors)
                .await;
        }
        // 잠금은 내 SB가 실제로 바뀐 뒤 적힌 `applied`를 따른다(차례가 아니면 바꾸지 않는다).
        locked.dedup();
        match host.write_units(address, centrals, &locked).await {
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

    /// 이 중앙의 SB를 `SdnSpec.lockdown`대로 바꾼다(§10.4 (2)·(3)·(4)): 설정 파일·단위를 쓰고, 돌고 있는 SB가 아직 옛
    /// 모양이면(파일을 쓴 뒤 멈췄다 다시 와도) 다시 띄우고, 클러스터에 다시 붙을 때까지 기다린 뒤 `applied`에 적는다.
    async fn switch_lockdown(
        &self,
        sdn: &Sdn,
        address: IpAddr,
        centrals: &[IpAddr],
        mut locked: Vec<IpAddr>,
        errors: &mut Vec<String>,
    ) {
        let host = &*self.host;
        let want = sdn.spec.lockdown;
        locked.retain(|a| *a != address);
        if want {
            locked.push(address);
        }
        tracing::info!(
            lockdown = want,
            "switching the OVN lockdown of this central"
        );
        let mut restart = match host.write_units(address, centrals, &locked).await {
            Ok(changed) => changed,
            Err(e) => {
                return errors.push(format!("cannot write the OVN central units: {e}"));
            }
        };
        let sb = Db::Sb.unit();
        if host.sb_running_locked().await != Some(want) && !restart.contains(&sb) {
            restart.push(sb);
        }
        if let Err(e) = host.start_central(&restart).await {
            return errors.push(format!("cannot restart the OVN central units: {e}"));
        }
        if let Err(e) = host.wait_member(Db::Sb, sdn.status.sb_cid).await {
            return errors.push(format!(
                "the OVN southbound database did not rejoin its cluster after the lockdown change: {e}"
            ));
        }
        if host.sb_running_locked().await != Some(want) {
            return errors.push(
                "the OVN southbound database does not listen as the lockdown wants after a restart"
                    .into(),
            );
        }
        let me = self.node;
        let result = opl_api::update_sdn_status(&self.cluster, |s| {
            let state = s.lockdown.get_or_insert_with(Default::default);
            let has = state.applied.contains(&me);
            if has == want {
                return false;
            }
            if want {
                state.applied.push(me);
            } else {
                state.applied.retain(|n| *n != me);
            }
            true
        })
        .await;
        match result {
            Ok(()) => tracing::info!(lockdown = want, "switched the OVN lockdown of this central"),
            Err(e) => errors.push(format!("cannot record the OVN lockdown: {e}")),
        }
    }

    /// 주소 허용 목록을 맞추고 알릴 상태를 돌려준다(잠금이 꺼져 있으면 None). 잠금을 원하고 모든 중앙이 잠겼으면 이
    /// 노드의 몫(중앙·chassis)대로 표를 넣고, 아니면 있던 표를 지운다(잠금을 켠 적이 없으면 nft를 부르지 않는다).
    async fn firewall(
        &self,
        sdn: &Sdn,
        nodes: &[Node],
        role: LockdownRole,
    ) -> Option<LockdownNodeState> {
        let host = &*self.host;
        let sets = allow_sets(nodes, sdn);
        let wanted = sdn.spec.lockdown && all_locked(sdn) && (role.central || role.chassis);
        if !wanted {
            let had = self.firewall.lock().unwrap().take().is_some();
            // chassis도 중앙도 아닌 노드는 표를 걸지 않으므로 알리지도 않는다(nft가 없을 수 있다).
            if !had && !role.central && !role.chassis {
                return None;
            }
            if had || sdn.status.lockdown.is_some() {
                match host.remove_lockdown().await {
                    Ok(true) => tracing::info!("removed the OVN address allow-list"),
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot remove the OVN address allow-list");
                        return sdn.spec.lockdown.then_some(LockdownNodeState {
                            firewall: false,
                            allowed: sets.members.len(),
                            error: Some(e.to_string()),
                        });
                    }
                }
            }
            return sdn.spec.lockdown.then_some(LockdownNodeState {
                firewall: false,
                allowed: sets.members.len(),
                error: None,
            });
        }
        let text = ruleset(host.tag(), &sets, role);
        let same = self
            .firewall
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|f| f.text == text);
        // 같은 글을 넣었어도 표가 사라졌으면(누가 `nft flush ruleset`) 다시 넣는다.
        let present = same && host.lockdown_present().await.unwrap_or(false);
        if !present && role.central {
            let waiting = |error: Option<String>| LockdownNodeState {
                firewall: false,
                allowed: sets.members.len(),
                error,
            };
            // 중앙은 하나씩(리뷰 1 I5): 목록에서 앞선 중앙이 모두 표를 걸었다고 알린 뒤에 건다.
            if let Some(name) = earlier_central_without_firewall(sdn, nodes, self.node) {
                tracing::debug!(central = %name, "waiting for an earlier central to apply the OVN allow-list");
                return Some(waiting(None));
            }
            // Raft를 끊어 걷은 글은 새 잠금 Task가 올 때까지 다시 넣지 않는다.
            let task = sdn.status.pending.as_ref().map(|p| p.task);
            let refused = self.refused.lock().unwrap().clone();
            if let Some((t, at, why)) = refused {
                if t == text && at == task {
                    return Some(waiting(Some(why)));
                }
                *self.refused.lock().unwrap() = None;
            }
        }
        let error = if present {
            None
        } else {
            match host.apply_lockdown(&text).await {
                Ok(()) => {
                    tracing::info!(
                        members = sets.members.len(),
                        centrals = sets.centrals.len(),
                        "applied the OVN address allow-list"
                    );
                    // 중앙은 건 뒤 자기 NB·SB Raft가 그대로인지 본다. 끊겼으면 표를 걷는다(정족수를 잃은 채 성공으로
                    // 알리지 않게 — 리뷰 1 I5).
                    let cut = if role.central {
                        host.raft_still_intact().await.err()
                    } else {
                        None
                    };
                    match cut {
                        None => {
                            *self.firewall.lock().unwrap() = Some(Firewall { text, role });
                            None
                        }
                        Some(why) => {
                            let removed = host.remove_lockdown().await;
                            *self.firewall.lock().unwrap() = None;
                            let msg = format!(
                                "{LOCKDOWN_RAFT_CUT} ({why}); removed it again{} — check `sdn lockdown on --dry-run` for Raft peers outside the central set",
                                match &removed {
                                    Err(e) => format!(" but the removal failed: {e}"),
                                    Ok(_) => String::new(),
                                }
                            );
                            tracing::warn!(error = %msg, "OVN lockdown");
                            let task = sdn.status.pending.as_ref().map(|p| p.task);
                            *self.refused.lock().unwrap() = Some((text, task, msg.clone()));
                            Some(msg)
                        }
                    }
                }
                Err(e) => {
                    *self.firewall.lock().unwrap() = None;
                    Some(e.to_string())
                }
            }
        };
        Some(LockdownNodeState {
            firewall: error.is_none(),
            allowed: sets.members.len(),
            error,
        })
    }

    /// 빠른 길(§10.3 "1초 안"): 노드가 빠지거나 들어 허용 집합이 바뀌었으면 1초 모으기와 다른 맞추기를 기다리지 않고
    /// 지금 넣은 표의 집합만 새로 넣는다. 표를 넣은 적이 없으면 아무것도 하지 않는다(다음 차례가 한다).
    async fn quick_firewall(&self) {
        let Some(last) = self.firewall.lock().unwrap().clone() else {
            return;
        };
        let (Ok(Some(sdn)), Ok((_, nodes))) = (
            self.cluster.get_by_name::<SdnSpec>(SDN_NAME),
            self.cluster.list::<NodeSpec>(),
        ) else {
            return;
        };
        if !(sdn.spec.lockdown && all_locked(&sdn)) {
            return;
        }
        let text = ruleset(self.host.tag(), &allow_sets(&nodes, &sdn), last.role);
        if text == last.text {
            return;
        }
        match self.host.apply_lockdown(&text).await {
            Ok(()) => {
                tracing::info!("updated the OVN address allow-list");
                *self.firewall.lock().unwrap() = Some(Firewall {
                    text,
                    role: last.role,
                });
            }
            Err(e) => tracing::warn!(error = %e, "cannot update the OVN address allow-list"),
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
    let mut last: Option<Inputs> = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
            () = changed(&mut watch) => {
                // 지운 노드를 곧바로 끊는다(§10.3).
                agent.quick_firewall().await;
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
                    kind: Kind::Sdn | Kind::Node | Kind::NetworkMigration,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Sdn | Kind::Node | Kind::NetworkMigration,
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
            wait_step: Duration::from_millis(20),
            ..OvnOptions::default()
        };
        let host = Arc::new(OvnHost::new(runner.clone(), paths, options, TAG.into()));
        let NodeId(node) = cluster.id();
        let agent = Agent {
            cluster: cluster.clone(),
            host,
            node,
            report: SdnReport::default(),
            firewall: Mutex::new(None),
            refused: Mutex::new(None),
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
        let mut sdn: opl_types::sdn::Sdn = Resource::new(SDN_NAME, SdnSpec::default());
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
        f.reply(
            "ovsdb-client --timeout=10 transact",
            CmdOutput::ok(r#"[{"rows":[]},{"rows":[]}]"#),
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

    /// 계획 67 §9.5: 다리 노드는 옮기기의 physnet을 원본 Linux 브리지에 같은 physnet 길로 잇고 알린다(관리자 설정에 더함).
    /// 다른 노드의 다리는 놓지 않는다.
    #[tokio::test]
    async fn lays_the_migration_bridge_on_its_node() {
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
        let lan = Resource::new(
            "lan",
            serde_json::from_value::<NetworkSpec>(serde_json::json!({ "bridge": "oplmig0" }))
                .unwrap(),
        );
        s.cluster.create(&lan).await.unwrap();
        let mut m: opl_types::network_migration::NetworkMigration = Resource::new(
            "m",
            serde_json::from_value(serde_json::json!({ "source": "lan", "target": "t-lan" }))
                .unwrap(),
        );
        let physnet = opl_types::sdn::migration_physnet(TAG, m.id);
        // 다른 노드의 다리는 이 노드가 놓지 않는다.
        m.status.bridge = Some(opl_types::network_migration::MigrationBridge {
            node: Uuid::now_v7(),
            physnet: physnet.clone(),
        });
        s.cluster.create(&m).await.unwrap();
        assert!(s.agent.inputs().unwrap().bridges.is_empty());
        let mut status = m.status.clone();
        status.bridge.as_mut().unwrap().node = node;
        s.cluster
            .update_status::<NetworkMigrationSpec>(m.id, &status)
            .await
            .unwrap();
        assert_eq!(
            s.agent.inputs().unwrap().bridges,
            [(physnet.clone(), "oplmig0".to_string())].into()
        );
        // 관리자 설정에 다리를 더한다(게이트웨이는 그대로, 다리가 없으면 설정 그대로).
        let admin = NodeSdnConfig {
            physnets: [("ext".to_string(), "vmbr1".to_string())].into(),
            gateway: Some(10),
        };
        let both = with_bridges(Some(&admin), &s.agent.inputs().unwrap().bridges).unwrap();
        assert_eq!(both.physnets.len(), 2);
        assert_eq!(both.gateway, Some(10));
        assert_eq!(with_bridges(Some(&admin), &BTreeMap::new()), Some(admin));
        assert_eq!(with_bridges(None, &BTreeMap::new()), None);

        let f = &s.runner.fake;
        f.reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        f.reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
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
            CmdOutput::ok(r#"[{"ifname":"oplmig0","mtu":1500,"flags":["UP"]}]"#),
        );
        f.reply(
            "ip -j -d link show type bridge",
            CmdOutput::ok(
                r#"[{"ifname":"oplmig0","mtu":1500,"linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":0}}}]"#,
            ),
        );
        s.agent.tick().await;
        let report = s.agent.report.get().unwrap();
        assert_eq!(report.physnets.len(), 1, "{report:?}");
        assert_eq!(report.physnets[0].name, physnet);
        assert!(report.physnets[0].ready, "{report:?}");
        let (_, veth, bridge) = opl_types::sdn::physnet_ifnames(TAG, &physnet);
        let calls = f.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("ip link add") && c.contains(&veth)),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.contains(&format!(
                "external_ids:ovn-bridge-mappings=\"{physnet}:{bridge}\""
            ))),
            "{calls:?}"
        );
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

    /// 다른 노드 하나(중앙이면 NB·SB 역할을 알린다)
    async fn add_node(
        cluster: &Cluster,
        name: &str,
        address: &str,
        online: bool,
        central: Option<RaftRole>,
    ) -> Uuid {
        let mut n: opl_types::node::Node = Resource::new(
            name,
            NodeSpec {
                sdn: None,
                address: format!("{address}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
                host_resources: None,
            },
        );
        n.status.online = online;
        n.status.features = vec![SDN.into(), SDN_SERVICES.into()];
        n.status.sdn = central.map(|role| NodeSdn {
            central: Some(CentralHealth {
                nb: role,
                sb: role,
                northd_active: false,
            }),
            ..NodeSdn::default()
        });
        cluster.create(&n).await.unwrap();
        n.id
    }

    async fn set_features(cluster: &Cluster, node: Uuid, features: &[&str]) {
        let me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
        let mut status = me.status;
        status.features = features.iter().map(|f| f.to_string()).collect();
        status.online = true;
        cluster
            .update_status::<NodeSpec>(node, &status)
            .await
            .unwrap();
    }

    fn central(node: Uuid, ip: &str) -> SdnCentral {
        SdnCentral {
            node,
            address: ip.parse().unwrap(),
        }
    }

    /// 중앙이자 chassis인 노드의 가짜 답(`list-remotes`는 [`central_setup`]이 먼저 단다)
    fn central_replies(f: &FakeRunner) {
        f.reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        f.reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
        f.reply(
            "ovs-vsctl --format=json --columns=external_ids",
            ids(&[(opl_driver::ovn::TAG_KEY, TAG)]),
        );
        f.reply(
            "ovs-vsctl --format=json --columns=name,external_ids find Bridge",
            CmdOutput::ok(r#"{"data":[],"headings":["name","external_ids"]}"#),
        );
        f.reply(
            "ovs-vsctl --format=json --columns=name,ofport",
            CmdOutput::ok(r#"{"data":[],"headings":["name","ofport","external_ids"]}"#),
        );
        f.reply("ovs-appctl", CmdOutput::ok(NB_LEADER));
        f.reply("ovsdb-client needs-conversion", CmdOutput::ok("no\n"));
        f.reply(
            "ovsdb-client --timeout=10 transact",
            CmdOutput::ok(r#"[{"rows":[]},{"rows":[]}]"#),
        );
        f.reply(
            "ss -tnH",
            CmdOutput::ok("0 0 10.0.0.1:6642 10.0.0.2:4000\n"),
        );
    }

    fn remotes_rule(s: &Setup) -> String {
        format!(
            "ovs-appctl --timeout 5 -t {} ovsdb-server/list-remotes",
            s.root.join("run/sb.ctl").display()
        )
    }

    fn listing(locked: bool) -> CmdOutput {
        if locked {
            CmdOutput::ok("pssl:6642:10.0.0.1\npssl:6645:10.0.0.1\npunix:/x/sb.sock\n")
        } else {
            CmdOutput::ok("pssl:6642:10.0.0.1\npunix:/x/sb.sock\n")
        }
    }

    /// 중앙의 가짜 답. `list-remotes`(더 긴 앞부분)를 일반 `ovs-appctl`보다 먼저 단다(처음 맞는 규칙이 답한다).
    fn central_setup(s: &Setup, remotes: Vec<CmdOutput>) {
        let f = &s.runner.fake;
        f.reply_seq(&remotes_rule(s), remotes);
        central_replies(f);
    }

    /// 잠금 차례(계획 67 §10.4): 다른 중앙 Task가 없고, 바꿀 첫 중앙이 나이고, 다른 중앙이 온라인이며 NB·SB가 리더·팔로어
    #[tokio::test]
    async fn the_lockdown_turn_goes_one_central_at_a_time() {
        let (_dir, cluster) = node_cluster().await;
        let NodeId(me) = cluster.id();
        let mut status = cluster.get::<NodeSpec>(me).unwrap().unwrap().status;
        status.online = true;
        status.sdn = Some(NodeSdn {
            central: Some(CentralHealth {
                nb: RaftRole::Follower,
                sb: RaftRole::Follower,
                northd_active: true,
            }),
            ..NodeSdn::default()
        });
        cluster
            .update_status::<NodeSpec>(me, &status)
            .await
            .unwrap();
        let a = add_node(&cluster, "a", "10.0.0.2", true, Some(RaftRole::Follower)).await;
        let b = add_node(&cluster, "b", "10.0.0.3", true, Some(RaftRole::Leader)).await;
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec { lockdown: true });
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![
            central(a, "10.0.0.2"),
            central(me, "10.0.0.1"),
            central(b, "10.0.0.3"),
        ];
        let nodes = || cluster.list::<NodeSpec>().unwrap().1;
        // 차례는 목록의 첫 중앙(a)부터
        assert!(
            lockdown_turn(&sdn, &nodes(), me)
                .unwrap_err()
                .contains("waiting for central")
        );
        assert_eq!(lockdown_turn(&sdn, &nodes(), a), Ok(()));
        sdn.status.lockdown = Some(opl_types::sdn::LockdownState {
            applied: vec![a],
            ..Default::default()
        });
        assert_eq!(lockdown_turn(&sdn, &nodes(), me), Ok(()));
        // 다른 중앙이 오프라인이거나 Raft 밖이면 기다린다.
        let mut down = cluster.get::<NodeSpec>(b).unwrap().unwrap().status;
        down.sdn.as_mut().unwrap().central.as_mut().unwrap().sb = RaftRole::Candidate;
        cluster.update_status::<NodeSpec>(b, &down).await.unwrap();
        assert!(
            lockdown_turn(&sdn, &nodes(), me)
                .unwrap_err()
                .contains("\"b\"")
        );
        let mut status = cluster.get::<NodeSpec>(b).unwrap().unwrap().status;
        status.sdn.as_mut().unwrap().central.as_mut().unwrap().sb = RaftRole::Follower;
        status.online = false;
        cluster.update_status::<NodeSpec>(b, &status).await.unwrap();
        assert!(lockdown_turn(&sdn, &nodes(), me).is_err());
        status.online = true;
        cluster.update_status::<NodeSpec>(b, &status).await.unwrap();
        assert_eq!(lockdown_turn(&sdn, &nodes(), me), Ok(()));
        // 중앙 더하기 Task가 자리를 잡았으면 기다리고, 잠금 Task면 간다.
        sdn.status.pending = Some(SdnPending {
            op: TaskOp::SdnCentralAdd,
            node: b,
            task: Uuid::now_v7(),
        });
        assert!(lockdown_turn(&sdn, &nodes(), me).is_err());
        sdn.status.pending.as_mut().unwrap().op = TaskOp::SdnLockdown { enabled: true };
        assert_eq!(lockdown_turn(&sdn, &nodes(), me), Ok(()));
        // 끄기는 거꾸로: 잠긴 첫 중앙부터
        sdn.spec.lockdown = false;
        assert!(lockdown_turn(&sdn, &nodes(), me).is_err());
        assert_eq!(lockdown_turn(&sdn, &nodes(), a), Ok(()));
    }

    /// 리뷰 1 I5: 중앙이 표를 건 뒤 자기 Raft가 끊겼으면(리더가 팔로어에게서 오래 듣지 못함) 표를 걷고 오류를 알린다.
    /// 같은 잠금 차례에는 같은 표를 다시 넣지 않는다.
    #[tokio::test]
    async fn a_firewall_that_cuts_raft_is_removed() {
        let s = setup(false).await;
        let node = s.agent.node;
        set_underlay(&s.cluster, node, "10.0.0.1").await;
        set_features(&s.cluster, node, &[SDN, SDN_SERVICES]).await;
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec { lockdown: true });
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![central(node, "10.0.0.1")];
        s.cluster.create(&sdn).await.unwrap();
        let f = &s.runner.fake;
        let cut = include_str!("../../opl-driver/src/ovn/fixtures/cluster-status-leader-two.txt")
            .replace("last msg 331 ms ago", "last msg 9000 ms ago");
        for db in [opl_driver::ovn::Db::Nb, opl_driver::ovn::Db::Sb] {
            f.reply(
                &format!(
                    "ovs-appctl --timeout 5 -t {} cluster/status",
                    s.agent.host.paths().db_ctl(db).display()
                ),
                CmdOutput::ok(cut.as_str()),
            );
        }
        central_setup(&s, vec![listing(false), listing(true)]);
        s.agent.tick().await;
        let sdn = s.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        assert_eq!(sdn.status.lockdown.as_ref().unwrap().applied, [node]);

        let nft = |f: &FakeRunner| {
            f.commands()
                .into_iter()
                .filter(|c| c.program == "nft" && c.args == ["-f", "-"])
                .count()
        };
        s.agent.tick().await;
        // 넣고 곧바로 걷었다.
        assert_eq!(nft(f), 2, "{:?}", f.calls());
        let report = s.agent.report.get().unwrap();
        let lockdown = report.lockdown.unwrap();
        assert!(!lockdown.firewall, "{lockdown:?}");
        let error = lockdown.error.unwrap();
        assert!(error.starts_with(LOCKDOWN_RAFT_CUT), "{error}");
        assert!(error.contains("ssl:127.0.0.2:6643"), "{error}");
        // 같은 차례에는 다시 넣지 않는다.
        s.agent.tick().await;
        assert_eq!(nft(f), 2, "{:?}", f.calls());
        assert!(
            s.agent
                .report
                .get()
                .unwrap()
                .lockdown
                .unwrap()
                .error
                .is_some()
        );
    }

    /// 중앙 하나: 잠금을 원하면 설정 파일·단위를 쓰고 SB를 다시 띄워 따라잡은 뒤 `applied`에 적는다. 다음 차례에 모든
    /// 중앙이 잠겼으므로 허용 목록을 넣는다. 잠근 뒤 다시 와도 SB를 다시 띄우지 않는다. 끄면 표를 지우고 되돌린다.
    #[tokio::test]
    async fn a_single_central_locks_down_and_back() {
        let s = setup(false).await;
        let node = s.agent.node;
        set_underlay(&s.cluster, node, "10.0.0.1").await;
        set_features(&s.cluster, node, &[SDN, SDN_SERVICES]).await;
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec { lockdown: true });
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![central(node, "10.0.0.1")];
        s.cluster.create(&sdn).await.unwrap();
        central_setup(&s, vec![listing(false), listing(true)]);
        let f = &s.runner.fake;

        s.agent.tick().await;
        let calls = f.calls();
        assert!(
            calls.contains(&"systemctl restart opl-ovn-sb.service".to_string()),
            "{calls:?}"
        );
        assert!(s.root.join("etc/sb.json").exists());
        let unit = std::fs::read_to_string(s.root.join("systemd/opl-ovn-sb.service")).unwrap();
        assert!(unit.contains("--config-file="), "{unit}");
        let sdn = s.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        assert_eq!(sdn.status.lockdown.as_ref().unwrap().applied, [node]);
        // 이 차례에는 아직 표를 넣지 않았다(모든 중앙이 잠긴 것을 본 다음 차례).
        let report = s.agent.report.get().unwrap();
        assert!(!report.lockdown.as_ref().unwrap().firewall, "{report:?}");
        assert_eq!(report.peers, ["10.0.0.2".parse::<IpAddr>().unwrap()]);
        assert_eq!(report.raft_peers, ["10.0.0.2".parse::<IpAddr>().unwrap()]);

        let before = f.calls().len();
        s.agent.tick().await;
        let after: Vec<String> = f.calls()[before..].to_vec();
        assert!(!after.iter().any(|c| c.contains("restart")), "{after:?}");
        assert!(after.contains(&"nft -f -".to_string()), "{after:?}");
        let nft = f
            .commands()
            .into_iter()
            .rev()
            .find(|c| c.program == "nft" && c.args == ["-f", "-"])
            .unwrap();
        let text = String::from_utf8(nft.stdin.unwrap()).unwrap();
        assert!(
            text.contains(&format!("table inet opl_ovn_{TAG} {{\n")),
            "{text}"
        );
        assert!(text.contains("elements = { 10.0.0.1 }"), "{text}");
        assert!(text.contains("tcp dport { 6643, 6644, 6645 }"), "{text}");
        let report = s.agent.report.get().unwrap();
        let lockdown = report.lockdown.unwrap();
        assert!(
            lockdown.firewall && lockdown.error.is_none(),
            "{lockdown:?}"
        );
        assert_eq!(lockdown.allowed, 1);

        // 끄기: 표를 지우고 SB를 계획 66 모양으로 되돌린다.
        let mut sdn = s.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        sdn.spec.lockdown = false;
        s.cluster
            .update_spec(sdn.id, None, sdn.labels.clone(), &sdn.spec)
            .await
            .unwrap();
        f.reply(
            &format!("nft list table inet opl_ovn_{TAG}"),
            CmdOutput::ok("table"),
        );
        f.reply_seq(&remotes_rule(&s), vec![listing(true), listing(false)]);
        s.agent.tick().await;
        assert!(!s.root.join("etc/sb.json").exists());
        let sdn = s.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        assert!(sdn.status.lockdown.unwrap().applied.is_empty());
        let deleted = f
            .commands()
            .into_iter()
            .rfind(|c| c.program == "nft" && c.args == ["-f", "-"])
            .unwrap();
        assert_eq!(
            String::from_utf8(deleted.stdin.unwrap()).unwrap(),
            format!("table inet opl_ovn_{TAG} {{}}\ndelete table inet opl_ovn_{TAG}\n")
        );
        assert_eq!(s.agent.report.get().unwrap().lockdown, None);
    }

    /// 단위 파일을 쓴 뒤 SB를 다시 띄우기 전에 멈췄다가 다시 와도(파일은 같음) 돌고 있는 SB가 옛 모양이면 다시 띄운다.
    /// 다른 중앙이 Raft 밖이면 차례가 오지 않아 아무것도 바꾸지 않는다.
    #[tokio::test]
    async fn a_lockdown_switch_resumes_after_a_crash() {
        let s = setup(false).await;
        let node = s.agent.node;
        set_underlay(&s.cluster, node, "10.0.0.1").await;
        let other = add_node(&s.cluster, "b", "10.0.0.2", true, Some(RaftRole::Candidate)).await;
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec { lockdown: true });
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![central(node, "10.0.0.1"), central(other, "10.0.0.2")];
        s.cluster.create(&sdn).await.unwrap();
        central_setup(&s, vec![listing(false)]);
        let f = &s.runner.fake;
        s.agent.tick().await;
        assert!(!s.root.join("etc/sb.json").exists());
        assert!(
            !f.calls().iter().any(|c| c.contains("list-remotes")),
            "{:?}",
            f.calls()
        );

        // 다른 중앙이 돌아왔다. 파일은 이미 잠근 모양(앞 차례가 쓰고 멈춤)이지만 SB는 옛 모양으로 돈다.
        let host = &s.agent.host;
        host.write_units(
            "10.0.0.1".parse().unwrap(),
            &["10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()],
            &["10.0.0.1".parse().unwrap()],
        )
        .await
        .unwrap();
        let mut status = s.cluster.get::<NodeSpec>(other).unwrap().unwrap().status;
        status.sdn.as_mut().unwrap().central.as_mut().unwrap().sb = RaftRole::Follower;
        status.sdn.as_mut().unwrap().central.as_mut().unwrap().nb = RaftRole::Follower;
        s.cluster
            .update_status::<NodeSpec>(other, &status)
            .await
            .unwrap();
        f.reply_seq(&remotes_rule(&s), vec![listing(false), listing(true)]);
        let before = f.calls().len();
        s.agent.tick().await;
        let after: Vec<String> = f.calls()[before..].to_vec();
        assert!(
            after.contains(&"systemctl restart opl-ovn-sb.service".to_string()),
            "{after:?}"
        );
        let sdn = s.cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        assert_eq!(sdn.status.lockdown.unwrap().applied, [node]);
        // 아직 다른 중앙이 잠기지 않아 표는 걸지 않는다.
        assert!(!after.contains(&"nft -f -".to_string()), "{after:?}");
    }

    /// 빠른 길: 넣은 표가 있으면 노드가 빠질 때 모으기 없이 그 주소를 뺀 집합을 곧바로 넣는다.
    #[tokio::test]
    async fn a_removed_node_leaves_the_allow_list_at_once() {
        let s = setup(false).await;
        let node = s.agent.node;
        set_underlay(&s.cluster, node, "10.0.0.1").await;
        let gone = add_node(&s.cluster, "gone", "10.0.0.7", true, None).await;
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec { lockdown: true });
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![central(node, "10.0.0.1")];
        sdn.status.lockdown = Some(opl_types::sdn::LockdownState {
            applied: vec![node],
            ..Default::default()
        });
        s.cluster.create(&sdn).await.unwrap();
        central_setup(&s, vec![listing(true)]);
        s.agent.tick().await;
        let last = || {
            s.runner
                .fake
                .commands()
                .into_iter()
                .rfind(|c| c.program == "nft" && c.args == ["-f", "-"])
                .map(|c| String::from_utf8(c.stdin.unwrap()).unwrap())
                .unwrap()
        };
        assert!(last().contains("10.0.0.7"), "{}", last());
        // 같으면 넣지 않는다.
        let n = s.runner.fake.calls().len();
        s.agent.quick_firewall().await;
        assert_eq!(s.runner.fake.calls().len(), n);
        s.cluster.delete::<NodeSpec>(gone, None).await.unwrap();
        s.agent.quick_firewall().await;
        assert!(!last().contains("10.0.0.7"), "{}", last());
        assert_eq!(s.runner.fake.calls().len(), n + 1);
    }

    /// 지운 노드의 SB chassis 청소: 모든 노드가 `sdn-services`를 알고 내 SB가 리더일 때만(계획 67 §10.2)
    #[tokio::test]
    async fn the_central_agent_cleans_chassis_when_its_sb_leads() {
        let s = setup(false).await;
        let node = s.agent.node;
        set_underlay(&s.cluster, node, "10.0.0.1").await;
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec::default());
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![central(node, "10.0.0.1")];
        s.cluster.create(&sdn).await.unwrap();
        central_setup(&s, vec![listing(false)]);
        let f = &s.runner.fake;
        let gone = Uuid::now_v7();
        f.reply(
            "ovsdb-client --timeout=10 transact",
            CmdOutput::ok(format!(
                r#"[{{"rows":[{{"name":"{gone}"}},{{"name":"{node}"}}]}},{{"rows":[]}}]"#
            )),
        );
        // 옛 노드(`sdn-services`를 모름)가 있으면 리더 OvnSync가 한다.
        let old = add_node(&s.cluster, "old", "10.0.0.9", true, None).await;
        set_features(&s.cluster, old, &[SDN]).await;
        s.agent.tick().await;
        assert!(
            f.calls()
                .iter()
                .all(|c| !c.starts_with("ovsdb-client --timeout=10"))
        );
        // 잠금을 켠 적이 없으면 nft를 부르지 않는다.
        assert!(f.calls().iter().all(|c| !c.starts_with("nft")));
        set_features(&s.cluster, old, &[SDN, SDN_SERVICES]).await;
        s.agent.tick().await;
        let deletes: Vec<String> = f
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("ovsdb-client --timeout=10") && c.contains("delete"))
            .collect();
        assert_eq!(deletes.len(), 1, "{deletes:?}");
        assert!(deletes[0].contains(&gone.to_string()) && !deletes[0].contains(&node.to_string()));
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
