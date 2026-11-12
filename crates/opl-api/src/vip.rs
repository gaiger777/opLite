//! 내장 VIP(설계 §4). 리더의 [`VipManager`]가 VIP마다 그것을 가질 노드를 골라 Raft(`VipHolder`)에 적고,
//! 각 노드의 [`VipAgent`]가 헬스 체크가 200일 때 리더에게 VIP 리스를 갱신하며(`RenewVip`) 자기가 주인인
//! VIP를 인터페이스에 붙인다.
//!
//! 안전 규칙(HA 리스와 같은 모양):
//! - 노드는 갱신을 보내기 전에 시각을 재고, 성공하면 그 시각에서 `lease - margin`까지만 VIP를 쥔다. 갱신이
//!   끊기면(리더와 끊김 포함) 그 전에 스스로 뗀다.
//! - 리더는 주인의 마지막 갱신(리더 시계)에서 `lease`가 지나야 다른 노드로 옮긴다. 리더가 된 뒤 처음
//!   `lease` 동안은 갱신을 받지 못한 주인도 아직 쥐었다고 본다(앞 리더가 갱신해 주었을 수 있다).
//! - 옮기기 전에 옛 주인의 VIP 갱신을 막고(펜싱) 리스를 다시 본다. 쓰기의 결과를 안 뒤에 풀므로, 그 뒤의
//!   갱신이 돌려주는 인덱스에 옮긴 결과가 들어 있어 옛 주인은 다시 쥐지 않는다.
//! - 주인이 살아 있으면 옮기지 않는다(리더가 바뀌거나 노드가 더해져도).
//! - 모든 노드(위트니스 포함)가 [`VIP_FEATURE`]를 알 때만 `VipHolder`를 쓴다(옛 노드는 그 로그 항목을 풀지
//!   못해 Raft 적용이 멈춘다).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{VipAddress, VipNet, VipRecord};
use opl_types::resource::Resource;
use opl_types::vip::{VipHolder, VipHolderSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::health::{self, HealthStatus};
use crate::state::AppState;

/// VIP 리스 길이(리더 시계)
pub const VIP_LEASE: Duration = Duration::from_secs(3);
/// 노드가 VIP 리스를 갱신하는 간격
pub const VIP_RENEW_INTERVAL: Duration = Duration::from_secs(1);
/// 노드가 리스가 끝나기 이만큼 전에 VIP를 뗀다(`ip addr del`에 걸리는 시간)
pub const VIP_RELEASE_MARGIN: Duration = Duration::from_millis(500);
/// gratuitous ARP를 보내는 명령을 기다리는 최대 시간
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(5);
/// 갱신 이만큼에 한 번 쥔 VIP를 다시 붙인다(멱등). 밖에서(`ifreload`, 브리지 다시 시작, networkd 다시 읽기)
/// 지워진 주소를 되살린다.
const REASSERT_EVERY: u32 = 5;
/// 모든 노드가 이 기능을 알면 VIP를 쓴다(옛 노드는 `vip-holder` 종류를 풀지 못한다).
pub const VIP_FEATURE: &str = "vip";

/// 모든 노드가 [`VIP_FEATURE`]를 안다. 읽지 못하면 모르는 것으로 본다.
fn every_node_knows_vips(cluster: &Cluster) -> bool {
    crate::cluster::missing_feature(cluster, VIP_FEATURE).is_ok_and(|missing| missing.is_empty())
}

/// 주인 `holder`가 VIP `vip`를 아직 쥐고 있을 수 있다: 리스가 살아 있고 그 VIP를 가질 수 있다고 알렸거나,
/// 리더가 된 지 리스 길이가 안 되어 아직 갱신을 받지 못했다(`settling`).
fn keeps(
    vip: &str,
    holder: Uuid,
    fresh: &BTreeMap<Uuid, BTreeSet<String>>,
    settling: bool,
) -> bool {
    match fresh.get(&holder) {
        Some(vips) => vips.contains(vip),
        None => settling,
    }
}

/// 리더가 할 일: 새 주인이 필요한 VIP와 그 노드. `holders`는 지금 주인(VIP 주소 → 노드), `fresh`는 리스가
/// 살아 있는 노드와 그 노드가 가질 수 있는 VIP다. 쥐고 있을 수 있는 주인은 그대로 두고, 나머지 VIP는 그
/// VIP를 가질 수 있는 노드 가운데 가진 VIP가 가장 적은 노드(같으면 id가 작은 노드)에 준다. 그래서 노드가
/// 넉넉하면 VIP마다 다른 노드가 된다.
pub fn plan(
    holders: &BTreeMap<String, Uuid>,
    fresh: &BTreeMap<Uuid, BTreeSet<String>>,
    settling: bool,
) -> Vec<(String, Uuid)> {
    let vips: BTreeSet<&String> = fresh.values().flatten().collect();
    let mut load: BTreeMap<Uuid, usize> = fresh.keys().map(|n| (*n, 0)).collect();
    let mut free = Vec::new();
    for vip in vips {
        match holders.get(vip) {
            Some(h) if keeps(vip, *h, fresh, settling) => *load.entry(*h).or_default() += 1,
            _ => free.push(vip),
        }
    }
    let mut moves = Vec::new();
    for vip in free {
        let best = fresh
            .iter()
            .filter(|(_, vips)| vips.contains(vip))
            .map(|(n, _)| (load[n], *n))
            .min();
        if let Some((_, node)) = best {
            *load.entry(node).or_default() += 1;
            moves.push((vip.clone(), node));
        }
    }
    moves
}

/// 노드가 지금 붙여야 할 VIP: 리스(`deadline`, 노드 시계)가 살아 있는 동안 자기가 주인인 것
pub fn wanted(
    now: Instant,
    deadline: Option<Instant>,
    me: Uuid,
    addresses: &[VipAddress],
    holders: &BTreeMap<String, Uuid>,
) -> BTreeSet<VipAddress> {
    if deadline.is_none_or(|d| now >= d) {
        return BTreeSet::new();
    }
    addresses
        .iter()
        .filter(|v| holders.get(&v.ip.to_string()) == Some(&me))
        .copied()
        .collect()
}

/// 지금 주인(VIP 주소 → 리소스)
fn holders(cluster: &Cluster) -> Result<BTreeMap<String, VipHolder>, ClusterError> {
    Ok(cluster
        .list::<VipHolderSpec>()?
        .1
        .into_iter()
        .map(|h| (h.name.clone(), h))
        .collect())
}

#[derive(Debug, Clone)]
pub struct VipManagerConfig {
    /// 리스를 확인하는 간격
    pub tick: Duration,
    pub lease: Duration,
}

impl Default for VipManagerConfig {
    fn default() -> Self {
        Self {
            tick: VIP_RENEW_INTERVAL,
            lease: VIP_LEASE,
        }
    }
}

/// 리더의 VIP 배정 반복 작업. 모든 노드(위트니스 빼고)에서 띄우고, 리더인 노드에서만 일한다.
pub struct VipManager {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl VipManager {
    pub fn spawn(cluster: Arc<Cluster>, config: VipManagerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(manage(cluster, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 결과를 모르는 옮기기 쓰기(`Timeout` 등). 같은 임기에서는 그 쓰기가 보이거나 임기가 바뀔 때까지 옛 주인의
/// 펜싱을 풀지 않는다(나중에 커밋될 수 있다).
struct Pending {
    term: u64,
    vip: String,
    from: Uuid,
    to: Uuid,
}

/// 리더로 본 임기와 그 임기에서 리더인 것을 처음 본 시각
type Base = Option<(u64, Instant)>;

async fn manage(
    cluster: Arc<Cluster>,
    config: VipManagerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut base: Base = None;
    let mut pending: Vec<Pending> = Vec::new();
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tick(&cluster, &config, &mut base, &mut pending) => {}
        }
    }
    // 결과를 모르는 쓰기의 펜싱은 남긴다(그 쓰기가 나중에 커밋될 수 있다). 이 노드는 멈추므로 더 받지 않는다.
}

/// `from`의 펜싱을 푼다. 결과를 모르는 다른 쓰기가 남았으면 두고 다음에 푼다.
fn end_fence(cluster: &Cluster, pending: &[Pending], from: Uuid) {
    if !pending.iter().any(|p| p.from == from) {
        cluster.end_vip_fence(NodeId(from));
    }
}

/// 쓰기의 결과를 안다(펜싱을 풀어도 된다). 이유는 HA 복구의 `settled`와 같다.
fn settled(result: &Result<u64, ClusterError>) -> bool {
    matches!(
        result,
        Ok(_) | Err(ClusterError::Rejected(_) | ClusterError::NotLeader { .. })
    )
}

async fn tick(
    cluster: &Cluster,
    config: &VipManagerConfig,
    base: &mut Base,
    pending: &mut Vec<Pending>,
) {
    let term = cluster.vip_leases().term;
    // 임기가 바뀌었으면 새 리더가 커밋한 인덱스가 살아남은 옛 쓰기를 포함하므로 펜싱을 푼다.
    let (old, kept): (Vec<_>, Vec<_>) = pending.drain(..).partition(|p| p.term != term);
    *pending = kept;
    for p in old {
        end_fence(cluster, pending, p.from);
    }
    if cluster.members().leader != Some(cluster.id()) {
        *base = None;
        return;
    }
    // VIP를 쓰는 노드가 없으면(갱신이 없으면) 장벽(과반수 확인)도 하지 않는다. 기준점은 첫 갱신을 본 때로
    // 늦어질 뿐이라(옮기기를 더 기다림) 안전하다.
    if pending.is_empty() && cluster.vip_leases().renewed.is_empty() {
        return;
    }
    // 옛 노드가 남았으면 `VipHolder`를 쓰지 않는다(그 노드의 적용이 멈춘다).
    if !every_node_knows_vips(cluster) {
        tracing::debug!("some nodes do not know VIPs yet; not assigning");
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let leases = cluster.vip_leases();
    let now = Instant::now();
    let b = match *base {
        Some((t, b)) if t == leases.term => b,
        _ => {
            *base = Some((leases.term, now));
            now
        }
    };
    let Ok(current) = holders(cluster) else {
        return;
    };
    // 결과를 모르던 쓰기가 보이면 펜싱을 푼다.
    let (done, kept): (Vec<_>, Vec<_>) = pending
        .drain(..)
        .partition(|p| current.get(&p.vip).is_some_and(|h| h.spec.node == p.to));
    *pending = kept;
    for p in done {
        end_fence(cluster, pending, p.from);
    }
    let fresh =
        |leases: &opl_cluster::VipLeases, now: Instant| -> BTreeMap<Uuid, BTreeSet<String>> {
            leases
                .renewed
                .iter()
                .filter(|(_, r)| now.saturating_duration_since(r.at.max(b)) < config.lease)
                .map(|(n, r)| (n.0, r.vips.clone()))
                .collect()
        };
    let settling = now.saturating_duration_since(b) < config.lease;
    let owners: BTreeMap<String, Uuid> = current
        .iter()
        .map(|(vip, h)| (vip.clone(), h.spec.node))
        .collect();
    for (vip, to) in plan(&owners, &fresh(&leases, now), settling) {
        if pending.iter().any(|p| p.vip == vip) {
            continue;
        }
        let holder = current.get(&vip);
        let from = holder.map(|h| h.spec.node).filter(|from| *from != to);
        if let Some(from) = from {
            cluster.begin_vip_fence(NodeId(from));
            // 표시하기 직전에 받은 갱신이 있을 수 있으므로 표시한 뒤 다시 본다.
            let again = cluster.vip_leases();
            let now = Instant::now();
            let settling = now.saturating_duration_since(b) < config.lease;
            if again.term != leases.term || keeps(&vip, from, &fresh(&again, now), settling) {
                end_fence(cluster, pending, from);
                continue;
            }
        }
        let spec = VipHolderSpec { node: to };
        let result = match holder {
            Some(h) => {
                cluster
                    .update_spec(h.id, Some(h.revision), h.labels.clone(), &spec)
                    .await
            }
            None => cluster.create(&Resource::new(vip.as_str(), spec)).await,
        };
        match &result {
            Ok(_) => tracing::info!(vip = %vip, node = %to, from = ?from, "VIP assigned"),
            Err(e) => tracing::warn!(vip = %vip, node = %to, error = %e, "cannot assign the VIP"),
        }
        let Some(from) = from else {
            continue;
        };
        if settled(&result) {
            end_fence(cluster, pending, from);
        } else {
            pending.push(Pending {
                term: leases.term,
                vip,
                from,
                to,
            });
        }
    }
}

#[derive(Debug, Clone)]
pub struct VipAgentConfig {
    /// VIP를 붙일 인터페이스
    pub interface: String,
    pub addresses: Vec<VipAddress>,
    pub interval: Duration,
    pub lease: Duration,
    pub margin: Duration,
    /// 있으면 붙인 VIP를 여기 적는다(프로세스가 죽어 남은 VIP를 다음 시작과 `--release-vips`가 뗀다).
    pub record: Option<VipRecord>,
}

impl VipAgentConfig {
    pub fn new(interface: impl Into<String>, addresses: Vec<VipAddress>) -> Self {
        Self {
            interface: interface.into(),
            addresses,
            interval: VIP_RENEW_INTERVAL,
            lease: VIP_LEASE,
            margin: VIP_RELEASE_MARGIN,
            record: None,
        }
    }
}

/// 갱신을 기다릴 최대 시간: 리스가 남았으면 남은 시간까지(`interval`을 넘지 않게), 리스가 없으면
/// `interval`. 남은 시간이 없으면 None이다(갱신하지 않는다 — 기다리는 동안 끝난 리스로 VIP를 쥐게 된다).
pub fn renew_limit(
    now: Instant,
    deadline: Option<Instant>,
    interval: Duration,
) -> Option<Duration> {
    match deadline {
        Some(d) => Some(d.saturating_duration_since(now).min(interval)).filter(|l| !l.is_zero()),
        None => Some(interval),
    }
}

/// 이 노드의 VIP 리스를 갱신하고 자기가 주인인 VIP를 붙이고 뗀다(`[vip]`이 있는 노드만).
pub struct VipAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl VipAgent {
    pub fn spawn(state: Arc<AppState>, net: Arc<dyn VipNet>, config: VipAgentConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(agent(state, net, config, rx)),
        }
    }

    /// 멈추고 붙인 VIP를 모두 뗀다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 이 노드가 지금 VIP를 가져도 되는지: 헬스 체크(`?ready`)가 200이다.
fn healthy(state: &AppState) -> bool {
    health::check(state, false).is_ok_and(|h| h.status == HealthStatus::Ok)
}

async fn agent(
    state: Arc<AppState>,
    net: Arc<dyn VipNet>,
    config: VipAgentConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let iface = config.interface.as_str();
    let record = config.record.as_ref();
    // 지난 프로세스가 떼지 못하고 죽었으면 VIP가 남아 있다. 리스를 받기 전에는 쥐면 안 되므로 뗀다(기록한
    // VIP는 opld가 시작할 때 이미 뗐다).
    for vip in &config.addresses {
        if let Err(e) = net.remove(iface, *vip).await {
            tracing::warn!(vip = %vip, interface = iface, error = %e, "cannot remove a leftover VIP");
        }
    }
    let ips: Vec<String> = config.addresses.iter().map(|v| v.ip.to_string()).collect();
    // 쥔 VIP와 그 gratuitous ARP 작업(떼면 멈춘다)
    let mut held: BTreeMap<VipAddress, JoinHandle<()>> = BTreeMap::new();
    let mut renewals: u32 = 0;
    let mut deadline: Option<Instant> = None;
    let mut next = Instant::now();
    let mut last_error: Option<String> = None;
    let mut upgrading = false;
    loop {
        let now = Instant::now();
        let mut reassert = false;
        // 늦게 깼으면(리스가 이미 끝남) 갱신을 기다리기 전에 먼저 뗀다.
        if deadline.is_some_and(|d| d <= now) {
            deadline = None;
            sync(&net, iface, record, &mut held, &BTreeSet::new()).await;
        }
        if now >= next {
            next = now + config.interval;
            renewals = renewals.wrapping_add(1);
            reassert = renewals.is_multiple_of(REASSERT_EVERY);
            let knows = every_node_knows_vips(&state.cluster);
            if knows == upgrading {
                upgrading = !knows;
                if upgrading {
                    tracing::warn!(
                        "some nodes do not know VIPs yet; not taking a VIP until they are upgraded"
                    );
                }
            }
            // 리스가 끝나기 전에 답이 없으면 그만 기다리고 뗀다.
            let limit = renew_limit(Instant::now(), deadline, config.interval);
            if !healthy(&state) {
                // 준비되지 않은 노드는 VIP를 갖지 않는다(리더는 리스가 끝나면 옮긴다).
                deadline = None;
            } else if let (true, Some(limit)) = (knows, limit) {
                let sent = Instant::now();
                match state.cluster.renew_vip(ips.clone(), limit).await {
                    Ok(()) => {
                        deadline = Some(sent + config.lease - config.margin);
                        if last_error.take().is_some() {
                            tracing::info!("VIP lease renewed again");
                        }
                    }
                    Err(e) => {
                        let e = e.to_string();
                        if last_error.as_deref() != Some(e.as_str()) {
                            tracing::warn!(error = %e, "cannot renew the VIP lease");
                        }
                        last_error = Some(e);
                    }
                }
            }
        }
        let owners: BTreeMap<String, Uuid> = match holders(&state.cluster) {
            Ok(h) => h.into_iter().map(|(vip, h)| (vip, h.spec.node)).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "cannot read VIP holders");
                BTreeMap::new()
            }
        };
        let want = wanted(
            Instant::now(),
            deadline,
            state.node,
            &config.addresses,
            &owners,
        );
        sync(&net, iface, record, &mut held, &want).await;
        if reassert {
            restore(&net, iface, &mut held).await;
        }
        let wake = match deadline {
            Some(d) if d > Instant::now() => d.min(next),
            _ => next,
        };
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep_until(wake) => {}
        }
    }
    sync(&net, iface, record, &mut held, &BTreeSet::new()).await;
}

/// 붙인 VIP(`held`)를 `want`에 맞춘다. 떼지 못한 것은 `held`에 남겨 다음에 다시 뗀다. `record`가 있으면
/// 붙이기 전에 적고(적지 못하면 붙이지 않는다) 뗀 뒤 지운다. 떼면 아직 도는 알림도 멈춘다.
async fn sync(
    net: &Arc<dyn VipNet>,
    iface: &str,
    record: Option<&VipRecord>,
    held: &mut BTreeMap<VipAddress, JoinHandle<()>>,
    want: &BTreeSet<VipAddress>,
) {
    let release: Vec<VipAddress> = held.keys().filter(|v| !want.contains(v)).copied().collect();
    for vip in release {
        match net.remove(iface, vip).await {
            Ok(()) => {
                if let Some(announce) = held.remove(&vip) {
                    announce.abort();
                }
                tracing::info!(vip = %vip, interface = iface, "VIP released");
                if let Some(Err(e)) = record.map(|r| r.remove(iface, vip)) {
                    tracing::warn!(vip = %vip, error = %e, "cannot update the VIP record");
                }
            }
            Err(e) => {
                tracing::error!(vip = %vip, interface = iface, error = %e, "cannot remove the VIP")
            }
        }
    }
    let take: Vec<VipAddress> = want
        .iter()
        .filter(|v| !held.contains_key(v))
        .copied()
        .collect();
    for vip in take {
        if let Some(Err(e)) = record.map(|r| r.add(iface, vip)) {
            tracing::error!(vip = %vip, error = %e, "cannot record the VIP; not adding it");
            continue;
        }
        match net.add(iface, vip).await {
            Ok(_) => {
                held.insert(vip, announce(net, iface, vip));
                tracing::info!(vip = %vip, interface = iface, "VIP taken");
            }
            Err(e) => {
                tracing::warn!(vip = %vip, interface = iface, error = %e, "cannot add the VIP")
            }
        }
    }
}

/// 쥔 VIP를 다시 붙인다(멱등). 밖에서 지워져 다시 붙였으면 다시 알린다.
async fn restore(
    net: &Arc<dyn VipNet>,
    iface: &str,
    held: &mut BTreeMap<VipAddress, JoinHandle<()>>,
) {
    for (vip, task) in held.iter_mut() {
        match net.add(iface, *vip).await {
            Ok(true) => {
                tracing::warn!(vip = %vip, interface = iface, "the VIP was removed from the interface; added it again");
                task.abort();
                *task = announce(net, iface, *vip);
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(vip = %vip, interface = iface, error = %e, "cannot add the VIP again")
            }
        }
    }
}

/// gratuitous ARP를 보낸다. 기다리지 않는다(리스 확인과 떼기가 늦지 않게).
fn announce(net: &Arc<dyn VipNet>, iface: &str, vip: VipAddress) -> JoinHandle<()> {
    let (net, iface) = (net.clone(), iface.to_string());
    tokio::spawn(async move {
        let result = tokio::time::timeout(ANNOUNCE_TIMEOUT, net.announce(&iface, vip))
            .await
            .map_err(|_| "timed out".to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        if let Err(e) = result {
            tracing::warn!(vip = %vip, error = %e, "cannot send gratuitous ARP");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(i: u128) -> Uuid {
        Uuid::from_u128(i)
    }

    fn set(vips: &[&str]) -> BTreeSet<String> {
        vips.iter().map(|v| v.to_string()).collect()
    }

    fn fresh(list: &[(u128, &[&str])]) -> BTreeMap<Uuid, BTreeSet<String>> {
        list.iter().map(|(i, v)| (n(*i), set(v))).collect()
    }

    fn holders(list: &[(&str, u128)]) -> BTreeMap<String, Uuid> {
        list.iter().map(|(v, i)| (v.to_string(), n(*i))).collect()
    }

    const A: &str = "10.0.0.1";
    const B: &str = "10.0.0.2";

    #[test]
    fn spreads_vips_over_nodes() {
        let f = fresh(&[(1, &[A, B]), (2, &[A, B])]);
        assert_eq!(
            plan(&holders(&[]), &f, false),
            vec![(A.to_string(), n(1)), (B.to_string(), n(2))]
        );
        // 이미 node2가 A를 가졌으면 B는 node1로 간다.
        assert_eq!(
            plan(&holders(&[(A, 2)]), &f, false),
            vec![(B.to_string(), n(1))]
        );
        // 노드가 하나면 둘 다 그 노드다.
        assert_eq!(
            plan(&holders(&[]), &fresh(&[(3, &[A, B])]), false),
            vec![(A.to_string(), n(3)), (B.to_string(), n(3))]
        );
    }

    #[test]
    fn keeps_a_live_holder_and_moves_an_expired_one() {
        let f = fresh(&[(1, &[A]), (2, &[A])]);
        // 살아 있는 주인은 짐이 많아도 그대로다.
        assert!(plan(&holders(&[(A, 2)]), &f, false).is_empty());
        // 리스가 끝난 주인(fresh에 없음)의 VIP는 옮긴다.
        assert_eq!(
            plan(&holders(&[(A, 9)]), &f, false),
            vec![(A.to_string(), n(1))]
        );
        // 리더가 된 지 얼마 안 되면 갱신을 아직 받지 못한 주인도 쥐었다고 본다.
        assert!(plan(&holders(&[(A, 9)]), &f, true).is_empty());
        // 살아 있지만 그 VIP를 가질 수 없다고 알린 주인(설정이 바뀜)은 쥐지 않는다.
        let f = fresh(&[(1, &[A]), (2, &[B])]);
        assert_eq!(
            plan(&holders(&[(A, 2)]), &f, true),
            vec![(A.to_string(), n(1)), (B.to_string(), n(2))]
        );
        // 가질 수 있는 노드가 없으면 그대로 둔다.
        assert!(
            plan(&holders(&[(A, 9)]), &fresh(&[(1, &[B])]), false)
                .iter()
                .all(|(vip, _)| vip != A)
        );
    }

    #[test]
    fn a_node_holds_its_vips_only_until_its_deadline() {
        let now = Instant::now();
        let a: VipAddress = "10.0.0.1/24".parse().unwrap();
        let b: VipAddress = "10.0.0.2/24".parse().unwrap();
        let h = holders(&[(A, 1), (B, 2)]);
        let later = Some(now + Duration::from_secs(1));
        assert_eq!(wanted(now, later, n(1), &[a, b], &h), BTreeSet::from([a]));
        assert_eq!(wanted(now, later, n(2), &[a, b], &h), BTreeSet::from([b]));
        // 리스가 없거나 끝났으면 아무것도 쥐지 않는다.
        assert!(wanted(now, None, n(1), &[a, b], &h).is_empty());
        assert!(wanted(now, Some(now), n(1), &[a, b], &h).is_empty());
        // 설정에 없는 VIP는 주인이어도 붙이지 않는다.
        assert!(wanted(now, later, n(1), &[b], &h).is_empty());
    }

    #[test]
    fn renewals_wait_at_most_until_the_deadline() {
        let now = Instant::now();
        let second = Duration::from_secs(1);
        assert_eq!(renew_limit(now, None, second), Some(second));
        let soon = now + Duration::from_millis(300);
        assert_eq!(
            renew_limit(now, Some(soon), second),
            Some(Duration::from_millis(300))
        );
        assert_eq!(
            renew_limit(now, Some(now + Duration::from_secs(5)), second),
            Some(second)
        );
        // 남은 시간이 없으면 갱신하지 않는다.
        assert_eq!(renew_limit(now, Some(now), second), None);
        assert_eq!(renew_limit(soon, Some(now), second), None);
    }
}
