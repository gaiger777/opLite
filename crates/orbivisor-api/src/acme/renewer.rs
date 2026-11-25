//! 리더 `AcmeRenewer`(계획 73 §11.2): 틱마다(60초) 인증서마다 할 일을 순수 함수 [`due`]로 정하고 Task `acme-order`를
//! 만든다(인증서·노드마다 하나, 클러스터 전체 동시 [`AcmeRenewerConfig::max_orders`]개). 할 일은 Raft status로만 정하므로
//! 리더가 바뀌어도 다음 틱에 잇는다. 하루 한 번 ARI 갱신 창을 다시 묻고, 지난 HTTP-01 답을 치운다.

use std::sync::Arc;
use std::time::Duration;

use orbivisor_cluster::ClusterError;
use orbivisor_types::acme::{
    AcmeAccountSpec, Certificate, CertificateScope, CertificateSpec, ChallengeKind, IssuedCert,
    RenewPolicy,
};
use orbivisor_types::node::{ACME, Node, NodeSpec};
use orbivisor_types::task::{Task, TaskOp, TaskSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{ari, covered_nodes, node_address, order};
use crate::cluster::missing_feature;
use crate::state::AppState;
use crate::tasks::create_task_with_id;

/// ARI 창을 다시 묻는 간격
const ARI_EVERY: i64 = 86_400;
/// ARI를 묻지 못했을 때 다시 묻기까지
const ARI_RETRY: i64 = 3600;
/// ARI 묻기(디렉터리·창) 하나의 한도
const ARI_TIMEOUT: Duration = Duration::from_secs(10);

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

/// 인증서 하나의 할 일. 물러서는 중이면 없다(공유는 `next_attempt_at`, 노드마다는 그 노드의 `node_failures` — 한 노드의
/// 물러서기가 다른 노드를 막지 않는다). 노드마다면 온라인이고 `acme`를 아는 덮는 노드마다(오프라인 노드는 건너뛰고 돌아오면
/// 잇는다).
pub fn due(cert: &Certificate, nodes: &[Node], now: i64) -> Vec<Due> {
    match cert.spec.scope {
        CertificateScope::Shared if cert.status.backing_off(None, now) => Vec::new(),
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
            .filter(|n| !cert.status.backing_off(Some(n.id), now))
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

/// 주문이 쓸 DNS-01 TXT 이름(`_acme-challenge.` 앞의 이름 — 와일드카드는 기본 이름과 같다)과 DNS 공급자. HTTP-01이면
/// None. CNAME은 따라가지 않는다(리더는 DNS를 묻지 않는 값싼 어림이다).
fn txt_names<'a>(
    cert: &'a Certificate,
    node: Option<Uuid>,
    nodes: &[Node],
) -> Option<(&'a str, Vec<String>)> {
    let ChallengeKind::Dns01 { dns } = &cert.spec.challenge else {
        return None;
    };
    let names = match node {
        None => cert.spec.names.clone(),
        Some(id) => {
            let n = nodes.iter().find(|n| n.id == id)?;
            cert.spec.names_for(&n.name, &node_address(n))
        }
    };
    let names = names
        .iter()
        .map(|n| {
            n.trim_start_matches("*.")
                .trim_end_matches('.')
                .to_ascii_lowercase()
        })
        .collect();
    Some((dns.as_str(), names))
}

/// 만들 Task들: (인증서, 대상 노드, 돌 노드). 같은 DNS 공급자의 같은 TXT 이름을 쓰는 주문은 함께 돌리지 않는다(리뷰 2 —
/// PowerDNS의 읽고 고쳐 쓰기는 노드 사이에서 원자적이지 않아 한 값을 잃는다). 지금 갱신이 만든 Task끼리는 막지 않는다.
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
    // 돌고 있는(또는 이번에 만들) 주문의 TXT 이름
    let mut busy: Vec<(&str, Vec<String>)> = open
        .iter()
        .filter_map(|(c, n)| txt_names(certs.iter().find(|x| x.id == *c)?, *n, nodes))
        .collect();
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
            let txt = txt_names(cert, d.node, nodes);
            if let Some((dns, names)) = &txt
                && busy
                    .iter()
                    .any(|(d2, n2)| d2 == dns && n2.iter().any(|n| names.contains(n)))
            {
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
            busy.extend(txt);
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
        // 범위가 바뀌었으면 다른 범위의 실패를 지운다(리뷰 2 — 요약·물러서기·경보가 이어지지 않게).
        let shared = cert.spec.scope == CertificateScope::Shared;
        if cert.status.clone().fit_scope(shared) {
            if let Err(e) = order::modify_status(cluster, cert.id, |s| {
                s.fit_scope(shared);
            })
            .await
            {
                tracing::debug!(error = %e, "cannot reset the failures of the old scope");
            }
            continue;
        }
        // 뺀 노드·더는 덮지 않는 노드의 실패를 지운다(요약·경보에 남지 않게). 노드마다 인증서만(공유의 물러서기는 그대로).
        let covered: Vec<Uuid> = covered_nodes(&cert.spec, &nodes)
            .iter()
            .map(|n| n.id)
            .collect();
        if !shared
            && cert
                .status
                .node_failures
                .keys()
                .any(|n| !covered.contains(n))
            && let Err(e) = order::modify_status(cluster, cert.id, |s| {
                s.retain_nodes(|n| covered.contains(n))
            })
            .await
        {
            tracing::debug!(error = %e, "cannot drop failures of uncovered nodes");
        }
    }
    for (certificate, node, run_on) in plan(
        &certs,
        &nodes,
        &tasks,
        cluster.id().0,
        config.max_orders,
        now,
    ) {
        // 칸을 먼저 잡는다(리뷰 2 — 그 사이 지금 갱신이 잡았으면 그것에 맡긴다).
        let id = Uuid::now_v7();
        let key_slot = super::slot(node);
        match order::claim(cluster, certificate, &[(key_slot.clone(), id)], |_| {}).await {
            Ok(None) => {}
            Ok(Some(_)) => continue,
            Err(e) => {
                tracing::debug!(error = %e, "cannot claim the ACME order slot");
                continue;
            }
        }
        if let Err(e) = create_task_with_id(
            cluster,
            id,
            TaskOp::AcmeOrder { certificate, node },
            certificate,
            run_on,
        )
        .await
        {
            tracing::warn!(error = %e.message, "cannot create the ACME order task");
            let _ = order::modify_status(cluster, certificate, |s| {
                if s.running.get(&key_slot) == Some(&id) {
                    s.running.remove(&key_slot);
                }
            })
            .await;
        }
    }
    // ARI 창 다시 묻기는 주문을 만든 뒤에(CA가 늦거나 멈춰도 주문이 밀리지 않게)
    for cert in &certs {
        Box::pin(refresh_ari(state, cert, now)).await;
    }
}

/// ARI 창을 하루 한 번 다시 묻는다. 창이 바뀌면 `renew_at`도 다시 정한다. AKI가 없는(인증서 id가 없는) 인증서는 묻지 않고,
/// CA가 창을 주지 않으면(`renewalInfo`가 없는 CA) `ari_checked_at`에 적어 하루 뒤에 다시 묻는다. CA가 닿지 않으면 한 시간 뒤.
/// 묻기마다 [`ARI_TIMEOUT`] 한도다.
async fn refresh_ari(state: &AppState, cert: &Certificate, now: i64) {
    if cert.spec.renew != RenewPolicy::Ari {
        return;
    }
    let mut slots: Vec<(Option<Uuid>, IssuedCert, String)> = cert
        .status
        .nodes
        .iter()
        .map(|(n, i)| (Some(*n), i))
        .chain(cert.status.shared.iter().map(|s| (None, s)))
        .filter(|(_, i)| ari_stale(i, now))
        .filter_map(|(n, i)| {
            let id = ari::leaf(&i.chain_pem).ok()?.cert_id?;
            Some((n, i.clone(), id))
        })
        .collect();
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
    // 창 없이 물은 것: (슬롯, 일련번호, 다시 물을 수 있게 적을 때)
    let mut asked: Vec<(Option<Uuid>, String, i64)> = Vec::new();
    let connected = tokio::time::timeout(
        ARI_TIMEOUT,
        super::client::AcmeClient::connect(url, account.spec.ca_bundle.as_deref(), key),
    )
    .await;
    let Ok(Ok(client)) = connected else {
        for (node, issued, _) in slots.drain(..) {
            asked.push((node, issued.serial, now - ARI_EVERY + ARI_RETRY));
        }
        mark_asked(state, cert, asked).await;
        return;
    };
    for (node, issued, id) in slots {
        let info = match tokio::time::timeout(ARI_TIMEOUT, client.renewal_info(&id)).await {
            Ok(Ok(Some(info))) => info,
            Ok(Ok(None)) => {
                asked.push((node, issued.serial, now));
                continue;
            }
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "cannot read the ARI renewal window");
                asked.push((node, issued.serial, now - ARI_EVERY + ARI_RETRY));
                continue;
            }
            Err(_) => {
                tracing::debug!("the ARI renewal window request timed out");
                asked.push((node, issued.serial, now - ARI_EVERY + ARI_RETRY));
                continue;
            }
        };
        let checked_at = now + info.retry_after.map_or(0, |r| r as i64 - ARI_EVERY).max(0);
        let changed = issued
            .ari
            .is_none_or(|w| (w.start, w.end) != (info.start, info.end));
        let window = orbivisor_types::acme::AriWindow {
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
    mark_asked(state, cert, asked).await;
}

/// 다시 물어야 하나: 창의 `checked_at`과 `ari_checked_at`(창 없이·실패로 물은 때) 가운데 늦은 것에서 하루가 지났나
/// (리뷰 2 — 오래된 창이 있어도 다시 묻기 표시를 지킨다)
fn ari_stale(i: &IssuedCert, now: i64) -> bool {
    let checked = i.ari.map(|w| w.checked_at).max(i.ari_checked_at);
    checked.is_none_or(|t| t + ARI_EVERY <= now)
}

/// 창 없이 물은 슬롯에 `ari_checked_at`을 적는다(같은 인증서일 때만 — 그 사이 새로 받았으면 그대로).
async fn mark_asked(state: &AppState, cert: &Certificate, asked: Vec<(Option<Uuid>, String, i64)>) {
    if asked.is_empty() {
        return;
    }
    let result = order::modify_status(&state.cluster, cert.id, |s| {
        for (node, serial, at) in &asked {
            let slot = match node {
                Some(n) => s.nodes.get_mut(n),
                None => s.shared.as_mut(),
            };
            if let Some(i) = slot.filter(|i| &i.serial == serial) {
                i.ari_checked_at = Some(*at);
            }
        }
    })
    .await;
    if let Err(e) = result {
        tracing::debug!(error = %e, "cannot note the ARI check");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbivisor_types::acme::{CertificateKey, CertificateStatus, ChallengeKind};
    use orbivisor_types::node::{NodeSpec, NodeStatus};
    use orbivisor_types::resource::Resource;
    use orbivisor_types::task::TaskStatus;

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
            ari_checked_at: None,
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
        // 물러서는 중이면 없다(노드마다).
        for n in [1, 2] {
            c.status
                .record_failure(Some(Uuid::from_u128(n)), "x".into(), 0, |_| 20);
        }
        assert!(due(&c, &nodes, 10).is_empty());
        assert_eq!(due(&c, &nodes, 20).len(), 2);
        // 고른 노드만
        c.status.node_failures.clear();
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

    #[test]
    fn ari_is_asked_once_a_day_even_without_a_window() {
        let mut i = issued(&["a"], 500);
        assert!(ari_stale(&i, 0));
        i.ari_checked_at = Some(100);
        assert!(!ari_stale(&i, 100 + ARI_EVERY - 1));
        assert!(ari_stale(&i, 100 + ARI_EVERY));
        i.ari = Some(orbivisor_types::acme::AriWindow {
            start: 1,
            end: 2,
            checked_at: 1000,
        });
        assert!(!ari_stale(&i, 1000 + ARI_EVERY - 1));
        // 리뷰 2: 오래된 창이 있어도 실패·창 없음으로 적은 다시 묻기 표시를 지킨다.
        i.ari_checked_at = Some(5000);
        assert!(!ari_stale(&i, 1000 + ARI_EVERY));
        assert!(ari_stale(&i, 5000 + ARI_EVERY));
    }

    /// 리뷰 2: 같은 DNS 공급자의 같은 TXT 이름을 쓰는 주문은 함께 돌리지 않는다.
    #[test]
    fn orders_sharing_a_txt_name_run_one_at_a_time() {
        let nodes = [node(1, true), node(2, true)];
        let mut c = cert(CertificateScope::PerNode { nodes: vec![] });
        c.spec.challenge = ChallengeKind::Dns01 { dns: "pdns".into() };
        c.spec.names = vec!["{node}.example.com".into(), "*.pve.example.com".into()];
        let ids: Vec<_> = plan(
            std::slice::from_ref(&c),
            &nodes,
            &[],
            Uuid::from_u128(1),
            2,
            0,
        )
        .into_iter()
        .map(|(_, n, _)| n)
        .collect();
        assert_eq!(ids, [Some(Uuid::from_u128(1))], "the shared name waits");
        // 돌고 있는 주문이 이름을 쓰면 다음 틱에
        let t: Task = Resource::new(
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
        assert!(
            plan(
                std::slice::from_ref(&c),
                &nodes,
                &[t],
                Uuid::from_u128(1),
                3,
                0
            )
            .is_empty()
        );
        // 노드 이름만 다르면 함께
        c.spec.names = vec!["{node}.example.com".into()];
        assert_eq!(
            plan(
                std::slice::from_ref(&c),
                &nodes,
                &[],
                Uuid::from_u128(1),
                2,
                0
            )
            .len(),
            2
        );
        // 다른 공급자면 막지 않는다.
        let mut other = cert(CertificateScope::Shared);
        other.id = Uuid::from_u128(101);
        other.spec.challenge = ChallengeKind::Dns01 { dns: "r53".into() };
        other.spec.names = vec!["n1.example.com".into()];
        c.spec.names = vec!["n1.example.com".into()];
        c.spec.scope = CertificateScope::Shared;
        assert_eq!(
            plan(&[c, other], &nodes, &[], Uuid::from_u128(1), 4, 0).len(),
            2
        );
    }

    /// 리뷰 1: 한 노드의 물러서기는 다른 노드(새 노드·갱신할 노드)를 막지 않는다.
    #[test]
    fn one_node_backing_off_does_not_block_others() {
        let nodes = [node(1, true), node(2, true)];
        let mut c = cert(CertificateScope::PerNode { nodes: vec![] });
        c.status
            .record_failure(Some(Uuid::from_u128(1)), "n1: x".into(), 0, |_| 3600);
        assert_eq!(
            due(&c, &nodes, 10),
            [Due {
                node: Some(Uuid::from_u128(2)),
                reason: "not issued yet"
            }]
        );
        assert_eq!(due(&c, &nodes, 3600).len(), 2);
    }
}
