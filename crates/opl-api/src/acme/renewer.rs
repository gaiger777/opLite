//! 리더 `AcmeRenewer`(계획 73 §11.2): 틱마다(60초) 인증서마다 할 일을 순수 함수 [`due`]로 정하고 Task `acme-order`를
//! 만든다(인증서·노드마다 하나, 클러스터 전체 동시 [`AcmeRenewerConfig::max_orders`]개). 할 일은 Raft status로만 정하므로
//! 리더가 바뀌어도 다음 틱에 잇는다. 하루 한 번 ARI 갱신 창을 다시 묻고, 지난 HTTP-01 답을 치운다.

use std::sync::Arc;
use std::time::Duration;

use opl_cluster::ClusterError;
use opl_types::acme::{
    AcmeAccountSpec, Certificate, CertificateScope, CertificateSpec, IssuedCert, RenewPolicy,
};
use opl_types::node::{ACME, Node, NodeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{ari, covered_nodes, node_address, order};
use crate::cluster::missing_feature;
use crate::state::AppState;
use crate::tasks::create_task;

/// ARI 창을 다시 묻는 간격
const ARI_EVERY: i64 = 86_400;

#[derive(Debug, Clone)]
pub struct AcmeRenewerConfig {
    pub tick: Duration,
    /// 클러스터 전체 동시 주문 수
    pub max_orders: usize,
}

impl Default for AcmeRenewerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            max_orders: 2,
        }
    }
}

/// 받아야 할 것 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    /// 노드마다 인증서의 노드(공유면 None)
    pub node: Option<Uuid>,
    pub reason: &'static str,
}

fn need(
    issued: Option<&IssuedCert>,
    names: &[String],
    cert: &CertificateSpec,
    now: i64,
) -> Option<&'static str> {
    let Some(i) = issued else {
        return Some("not issued yet");
    };
    let mut want = names.to_vec();
    let mut have = i.names.clone();
    want.sort();
    have.sort();
    if want != have {
        return Some("names changed");
    }
    if i.key != cert.key {
        return Some("key type changed");
    }
    if now >= i.renew_at {
        return Some("renewal due");
    }
    None
}

/// 인증서 하나의 할 일. 물러서는 중(`next_attempt_at`)이면 없다. 노드마다면 온라인이고 `acme`를 아는 덮는 노드마다
/// (오프라인 노드는 건너뛰고 돌아오면 잇는다).
pub fn due(cert: &Certificate, nodes: &[Node], now: i64) -> Vec<Due> {
    if cert.status.next_attempt_at.is_some_and(|t| t > now) {
        return Vec::new();
    }
    match cert.spec.scope {
        CertificateScope::Shared => need(
            cert.status.shared.as_ref(),
            &cert.spec.names,
            &cert.spec,
            now,
        )
        .map(|reason| Due { node: None, reason })
        .into_iter()
        .collect(),
        CertificateScope::PerNode { .. } => covered_nodes(&cert.spec, nodes)
            .into_iter()
            .filter(|n| n.status.online && n.status.features.iter().any(|f| f == ACME))
            .filter_map(|n| {
                let names = cert.spec.names_for(&n.name, &node_address(n));
                need(cert.status.nodes.get(&n.id), &names, &cert.spec, now).map(|reason| Due {
                    node: Some(n.id),
                    reason,
                })
            })
            .collect(),
    }
}

/// 공유 인증서를 받을 노드: 리더(위트니스가 아니면), 아니면 `acme`를 아는 온라인 노드 가운데 id가 가장 작은 것
pub fn shared_runner(nodes: &[Node], leader: Uuid) -> Option<Uuid> {
    let ok = |n: &&Node| {
        !n.spec.witness && n.status.online && n.status.features.iter().any(|f| f == ACME)
    };
    nodes
        .iter()
        .filter(ok)
        .find(|n| n.id == leader)
        .or_else(|| nodes.iter().filter(ok).min_by_key(|n| n.id))
        .map(|n| n.id)
}

/// 만들 Task들: (인증서, 대상 노드, 돌 노드)
pub fn plan(
    certs: &[Certificate],
    nodes: &[Node],
    tasks: &[Task],
    leader: Uuid,
    max_orders: usize,
    now: i64,
) -> Vec<(Uuid, Option<Uuid>, Uuid)> {
    let open: Vec<(Uuid, Option<Uuid>)> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .filter_map(|t| match t.spec.op {
            TaskOp::AcmeOrder { certificate, node } => Some((certificate, node)),
            _ => None,
        })
        .collect();
    let mut budget = max_orders.saturating_sub(open.len());
    let mut out = Vec::new();
    for cert in certs {
        for d in due(cert, nodes, now) {
            if budget == 0 {
                return out;
            }
            // 같은 인증서의 Task가 돌면(다른 노드라도 — 공유는 하나뿐) 그 노드 것은 다음 틱
            if open.iter().any(|(c, n)| *c == cert.id && *n == d.node) {
                continue;
            }
            let run_on = match d.node {
                Some(n) => n,
                None => match shared_runner(nodes, leader) {
                    Some(n) => n,
                    None => continue,
                },
            };
            tracing::info!(certificate = %cert.name, node = ?d.node, reason = d.reason, "ordering an ACME certificate");
            out.push((cert.id, d.node, run_on));
            budget -= 1;
        }
    }
    out
}

/// 리더 반복 작업. 모든 노드에서 띄우고 리더에서만 일한다.
pub struct AcmeRenewer {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl AcmeRenewer {
    pub fn spawn(state: Arc<AppState>, config: AcmeRenewerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(state: Arc<AppState>, config: AcmeRenewerConfig, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            _ = Box::pin(tick(&state, &config)) => {}
        }
    }
}

/// 한 번(시험이 직접 부른다)
pub async fn tick(state: &AppState, config: &AcmeRenewerConfig) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    if cluster.read_barrier().await.is_err() {
        return;
    }
    if !missing_feature(cluster, ACME).is_ok_and(|old| old.is_empty()) {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<CertificateSpec>()?.1,
            cluster.list::<NodeSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
        ))
    })();
    let Ok((certs, nodes, tasks)) = lists else {
        return;
    };
    if certs.is_empty() {
        return;
    }
    let now = crate::backups::now() as i64;
    for cert in &certs {
        // 지난 HTTP-01 답을 치운다.
        if cert.status.challenges.iter().any(|a| a.until <= now)
            && let Err(e) =
                order::modify_status(cluster, cert.id, |s| s.challenges.retain(|a| a.until > now))
                    .await
        {
            tracing::debug!(error = %e, "cannot drop expired HTTP-01 answers");
        }
        Box::pin(refresh_ari(state, cert, now)).await;
    }
    for (certificate, node, run_on) in plan(
        &certs,
        &nodes,
        &tasks,
        cluster.id().0,
        config.max_orders,
        now,
    ) {
        if let Err(e) = create_task(
            cluster,
            TaskOp::AcmeOrder { certificate, node },
            certificate,
            run_on,
        )
        .await
        {
            tracing::warn!(error = %e.message, "cannot create the ACME order task");
        }
    }
}

/// ARI 창을 하루 한 번 다시 묻는다. 창이 바뀌면 `renew_at`도 다시 정한다.
async fn refresh_ari(state: &AppState, cert: &Certificate, now: i64) {
    if cert.spec.renew != RenewPolicy::Ari {
        return;
    }
    let stale = |i: &IssuedCert| i.ari.is_none_or(|w| w.checked_at + ARI_EVERY <= now);
    let mut slots: Vec<(Option<Uuid>, IssuedCert)> = cert
        .status
        .nodes
        .iter()
        .filter(|(_, i)| stale(i))
        .map(|(n, i)| (Some(*n), i.clone()))
        .collect();
    if let Some(s) = cert.status.shared.as_ref().filter(|s| stale(s)) {
        slots.push((None, s.clone()));
    }
    if slots.is_empty() {
        return;
    }
    let Ok(Some(account)) = state
        .cluster
        .get_by_name::<AcmeAccountSpec>(&cert.spec.account)
    else {
        return;
    };
    let (Some(url), Ok(key)) = (
        account.spec.directory_url(),
        super::jws::AccountKey::from_pem(account.spec.key.expose()),
    ) else {
        return;
    };
    let Ok(client) =
        super::client::AcmeClient::connect(url, account.spec.ca_bundle.as_deref(), key).await
    else {
        return;
    };
    for (node, issued) in slots {
        let Some(id) = ari::leaf(&issued.chain_pem).ok().and_then(|l| l.cert_id) else {
            continue;
        };
        let info = match client.renewal_info(&id).await {
            Ok(Some(info)) => info,
            Ok(None) => continue,
            Err(e) => {
                tracing::debug!(error = %e, "cannot read the ARI renewal window");
                continue;
            }
        };
        let checked_at = now + info.retry_after.map_or(0, |r| r as i64 - ARI_EVERY).max(0);
        let changed = issued
            .ari
            .is_none_or(|w| (w.start, w.end) != (info.start, info.end));
        let window = opl_types::acme::AriWindow {
            start: info.start,
            end: info.end,
            checked_at,
        };
        let renew_at = if changed {
            let jitter = if crate::settings::current(&state.cluster)
                .map(|s| s.spec.acme().renew_jitter)
                .unwrap_or(true)
            {
                order::random_unit()
            } else {
                0.0
            };
            ari::renew_at(
                cert.spec.renew,
                issued.not_before,
                issued.not_after,
                Some(&window),
                jitter,
            )
        } else {
            issued.renew_at
        };
        let serial = issued.serial.clone();
        let result = order::modify_status(&state.cluster, cert.id, |s| {
            let slot = match node {
                Some(n) => s.nodes.get_mut(&n),
                None => s.shared.as_mut(),
            };
            if let Some(i) = slot.filter(|i| i.serial == serial) {
                i.ari = Some(window);
                i.renew_at = renew_at;
            }
        })
        .await;
        if let Err(e) = result {
            tracing::debug!(error = %e, "cannot store the ARI renewal window");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::acme::{CertificateKey, CertificateStatus, ChallengeKind};
    use opl_types::node::{NodeSpec, NodeStatus};
    use opl_types::resource::Resource;
    use opl_types::task::TaskStatus;

    fn node(n: u128, online: bool) -> Node {
        let mut node = Resource::new(
            format!("n{n}"),
            NodeSpec {
                address: format!("10.0.0.{n}:8007"),
                maintenance: false,
                witness: false,
                roles: None,
                host_resources: None,
                sdn: None,
            },
        );
        node.id = Uuid::from_u128(n);
        node.status = NodeStatus {
            online,
            features: vec![ACME.into()],
            ..serde_json::from_value(serde_json::json!({"online": online, "cpus": 1, "memory_mib": 1, "hypervisor": "x"})).unwrap()
        };
        node
    }

    fn cert(scope: CertificateScope) -> Certificate {
        let mut c = Resource::new(
            "c",
            CertificateSpec {
                account: "le".into(),
                names: vec!["{node}.example.com".into()],
                scope,
                challenge: ChallengeKind::Http01,
                key: CertificateKey::EcdsaP256,
                profile: None,
                renew: RenewPolicy::Ari,
                preferred_chain: None,
                install: Default::default(),
                precheck: true,
            },
        );
        c.id = Uuid::from_u128(100);
        c
    }

    fn issued(names: &[&str], renew_at: i64) -> IssuedCert {
        IssuedCert {
            serial: "1".into(),
            not_before: 0,
            not_after: 1000,
            names: names.iter().map(|s| s.to_string()).collect(),
            chain_pem: String::new(),
            ari: None,
            renew_at,
            key: CertificateKey::EcdsaP256,
            error: None,
        }
    }

    #[test]
    fn due_per_node_and_shared() {
        let nodes = [node(1, true), node(2, true), node(3, false)];
        let mut c = cert(CertificateScope::PerNode { nodes: vec![] });
        let d = due(&c, &nodes, 10);
        assert_eq!(
            d,
            [
                Due {
                    node: Some(Uuid::from_u128(1)),
                    reason: "not issued yet"
                },
                Due {
                    node: Some(Uuid::from_u128(2)),
                    reason: "not issued yet"
                },
            ]
        );
        c.status = CertificateStatus {
            nodes: [
                (Uuid::from_u128(1), issued(&["n1.example.com"], 500)),
                (Uuid::from_u128(2), issued(&["old.example.com"], 500)),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(
            due(&c, &nodes, 10),
            [Due {
                node: Some(Uuid::from_u128(2)),
                reason: "names changed"
            }]
        );
        assert_eq!(due(&c, &nodes, 600).len(), 2);
        c.spec.key = CertificateKey::EcdsaP384;
        assert_eq!(due(&c, &nodes, 10)[0].reason, "key type changed");
        // 물러서는 중이면 없다.
        c.status.next_attempt_at = Some(20);
        assert!(due(&c, &nodes, 10).is_empty());
        assert_eq!(due(&c, &nodes, 20).len(), 2);
        // 고른 노드만
        c.status.next_attempt_at = None;
        c.spec.scope = CertificateScope::PerNode {
            nodes: vec![Uuid::from_u128(2)],
        };
        assert_eq!(due(&c, &nodes, 10).len(), 1);
        // 공유: 이름은 그대로, 노드 하나
        let mut s = cert(CertificateScope::Shared);
        s.spec.names = vec!["api.example.com".into()];
        assert_eq!(
            due(&s, &nodes, 0),
            [Due {
                node: None,
                reason: "not issued yet"
            }]
        );
        s.status.shared = Some(issued(&["api.example.com"], 500));
        assert!(due(&s, &nodes, 0).is_empty());
        assert_eq!(
            shared_runner(&nodes, Uuid::from_u128(2)),
            Some(Uuid::from_u128(2))
        );
        assert_eq!(
            shared_runner(&nodes, Uuid::from_u128(3)),
            Some(Uuid::from_u128(1))
        );
    }

    #[test]
    fn plan_respects_open_tasks_and_the_cluster_limit() {
        let nodes = [node(1, true), node(2, true)];
        let c = cert(CertificateScope::PerNode { nodes: vec![] });
        let mut t: Task = Resource::new(
            "t",
            TaskSpec {
                op: TaskOp::AcmeOrder {
                    certificate: c.id,
                    node: Some(Uuid::from_u128(1)),
                },
                target: c.id,
                node: Uuid::from_u128(1),
            },
        );
        t.status = TaskStatus::default();
        assert_eq!(
            plan(
                std::slice::from_ref(&c),
                &nodes,
                &[],
                Uuid::from_u128(1),
                2,
                0
            ),
            [
                (c.id, Some(Uuid::from_u128(1)), Uuid::from_u128(1)),
                (c.id, Some(Uuid::from_u128(2)), Uuid::from_u128(2)),
            ]
        );
        // 돌고 있는 Task: 그 노드는 건너뛰고 한도에 센다.
        assert_eq!(
            plan(
                std::slice::from_ref(&c),
                &nodes,
                std::slice::from_ref(&t),
                Uuid::from_u128(1),
                2,
                0
            ),
            [(c.id, Some(Uuid::from_u128(2)), Uuid::from_u128(2))]
        );
        assert!(
            plan(
                std::slice::from_ref(&c),
                &nodes,
                std::slice::from_ref(&t),
                Uuid::from_u128(1),
                1,
                0
            )
            .is_empty()
        );
    }
}
