//! SDN(OVN, 계획 65 §7)의 설치·구성 Task: 첫 중앙 세우기(`sdn-init`), 중앙 더하기·빼기(`sdn-central-add`·`-remove`),
//! chassis 설치(`sdn-chassis-install`). 노드에서 하는 일은 [`OvnHost`]의 단계 함수가 하고(모두 멱등), 여기서는 Raft에서
//! 필요한 것을 읽어 단계를 차례로 부르고, NB·SB가 실제로 선 것(리더·멤버)을 확인한 뒤에만 `Sdn` 리소스의 상태에 그 노드를
//! 더한다. 중앙 Task는 `SdnStatus.pending`(API가 리비전 확인 쓰기로 잡음)을 가진 것만 돈다 — 클러스터 전체에서 한 번에
//! 하나다. 단계마다 진행률 stage를 단계 이름으로 쓴다(바이트 0/0). 실패한 Task는 같은 요청을 다시 하면 이어서 한다.
//!
//! OVN 접근 잠금(`sdn-lockdown`, 계획 67 §10.4)은 같은 자리를 잡지만 노드에서 OVN을 부르지 않는다: `SdnSpec.lockdown`을
//! 쓰고, 중앙마다의 SDN 에이전트가 하나씩(정족수를 지키며) SB를 바꿔 `SdnStatus.lockdown.applied`에 적고 모든 노드가
//! 허용 목록을 맞출 때까지 기다린다. 에이전트가 Raft 상태만 보고 이어서 하므로 Task가 죽어도(다시 해도) 잠금은 끝까지
//! 간다. 중앙 세우기·더하기는 `SdnSpec.lockdown`을 따라 처음부터 잠근 단위로 뜨고 `applied`에 든다.

use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_driver::MirrorProgress;
use orbivisor_driver::ovn::{Chassis, Db, KnownCids, OvnHost, OvnVersions, major_minor};
use orbivisor_types::node::{Node, NodeSpec};
use orbivisor_types::sdn::{
    ChassisState, LOCKDOWN_RAFT_CUT, MAX_CENTRALS, SDN_NAME, Sdn, SdnCentral, SdnPhase, SdnSpec,
    SdnStatus, central_address,
};
use orbivisor_types::task::{Task, TaskOp};
use orbivisor_types::task_failure::{TaskFailure, code};
use uuid::Uuid;

use super::Ctx;
use super::migrate::write_progress;

/// SDN Task가 설 줄: `Sdn` 리소스(없으면 Task의 대상). chassis 설치의 대상은 노드지만 같은 줄에 서서 한 노드 안에서
/// 중앙 Task와 차례로 돈다(같은 노드의 apt·OVS DB를 함께 만지지 않게).
pub(super) fn queue(ctx: &Ctx, task: &Task) -> Uuid {
    match ctx.cluster.get_by_name::<SdnSpec>(SDN_NAME) {
        Ok(Some(sdn)) => sdn.id,
        _ => task.spec.target,
    }
}

/// SDN Task를 돌린다. 성공해도 알릴 것(`br-int`의 남의 포트, 짝수 중앙)은 경고로 남긴다.
pub(super) async fn perform(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    if let TaskOp::SdnLockdown { enabled } = task.spec.op {
        return lockdown(ctx, task.id, enabled, LockdownLimits::default()).await;
    }
    let host = ctx
        .config
        .sdn
        .as_deref()
        .ok_or("this node cannot run SDN (OVN) tasks")?;
    let sdn = ctx
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .map_err(|e| e.to_string())?
        .ok_or("SDN (OVN) is not set up")?;
    let me = ctx
        .cluster
        .get::<NodeSpec>(ctx.config.node)
        .map_err(|e| e.to_string())?
        .ok_or("this node is not registered")?;
    if me.spec.witness {
        return Err("a witness node cannot run OVN".into());
    }
    let run = Run {
        ctx,
        task: task.id,
        host,
        sdn,
        me,
    };
    match &task.spec.op {
        TaskOp::SdnInit { address } => run.init(*address).await,
        TaskOp::SdnCentralAdd => run.add_central().await,
        TaskOp::SdnCentralRemove => run.remove_central().await,
        TaskOp::SdnCentralKick { node, address } => run.kick(*node, *address).await,
        TaskOp::SdnChassisInstall => run.install_chassis().await,
        op => Err(format!("{op:?} is not an SDN task")),
    }
}

/// 끝난 SDN Task(`error`면 실패)의 뒤처리: 이 Task가 잡은 자리(`pending`)를 놓고, 실패한 첫 세우기면 그 이유를 `Sdn`
/// 리소스에 남긴다(phase는 그대로 — 다시 하면 이어서 한다).
pub(super) async fn finished(ctx: &Ctx, task: &Task, error: Option<&str>) {
    let result = update_sdn(&ctx.cluster, |s| {
        let mut changed = false;
        if s.pending.as_ref().is_some_and(|p| p.task == task.id) {
            s.pending = None;
            changed = true;
        }
        if matches!(task.spec.op, TaskOp::SdnLockdown { .. })
            && let Some(state) = s.lockdown.as_mut()
            && (state.pending || state.error.as_deref() != error)
        {
            state.pending = false;
            state.error = error.map(str::to_string);
            changed = true;
        }
        if matches!(task.spec.op, TaskOp::SdnInit { .. })
            && s.init_node == Some(task.spec.node)
            && let Some(error) = error
            && s.last_error.as_deref() != Some(error)
        {
            s.last_error = Some(error.to_string());
            changed = true;
        }
        changed
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot record the end of an SDN task");
    }
}

/// `Sdn` 리소스의 상태를 `change`로 바꾼다(읽은 revision 확인 — 다른 쓰기와 부딪치면 다시 읽어 바꾼다). `change`가
/// 거짓이면 쓰지 않는다. 리소스가 없으면 아무것도 하지 않는다.
pub async fn update_sdn(
    cluster: &Cluster,
    mut change: impl FnMut(&mut SdnStatus) -> bool,
) -> Result<(), String> {
    loop {
        let Some(current) = cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let mut status = current.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<SdnSpec>(current.id, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Raft가 아는 OVN 클러스터 id
fn known_cids(s: &SdnStatus) -> KnownCids {
    KnownCids {
        nb: s.nb_cid,
        sb: s.sb_cid,
    }
}

/// 잠긴 중앙(`SdnStatus.lockdown.applied`)의 주소
fn locked_addresses(s: &SdnStatus) -> Vec<IpAddr> {
    let applied = s
        .lockdown
        .as_ref()
        .map(|l| l.applied.as_slice())
        .unwrap_or_default();
    s.centrals
        .iter()
        .filter(|c| applied.contains(&c.node))
        .map(|c| c.address)
        .collect()
}

/// 중앙 `node`를 잠긴 중앙(`applied`)에 더한다(이미 있으면 거짓).
fn mark_locked(s: &mut SdnStatus, node: Uuid) -> bool {
    let state = s.lockdown.get_or_insert_with(Default::default);
    if state.applied.contains(&node) {
        return false;
    }
    state.applied.push(node);
    true
}

/// 떠난 중앙 `node`를 잠긴 중앙에서 뺀다(있었으면 참).
fn unmark_locked(s: &mut SdnStatus, node: Uuid) -> bool {
    let Some(state) = s.lockdown.as_mut() else {
        return false;
    };
    let before = state.applied.len();
    state.applied.retain(|n| *n != node);
    state.applied.len() != before
}

/// `sdn-lockdown`이 기다리는 시간
#[derive(Debug, Clone, Copy)]
struct LockdownLimits {
    /// 잠긴 중앙이 하나도 늘거나 줄지 않은 채 이만큼 지나면 실패다(중앙 하나가 SB를 다시 띄워 따라잡는 데 몇 초).
    stall: Duration,
    /// 모든 노드가 허용 목록을 맞추기까지(넘으면 경고 — 꺼진 노드는 다시 켜질 때 맞춘다)
    firewall: Duration,
    /// 다시 보는 간격
    step: Duration,
}

impl Default for LockdownLimits {
    fn default() -> Self {
        Self {
            stall: Duration::from_secs(300),
            firewall: Duration::from_secs(120),
            step: Duration::from_secs(1),
        }
    }
}

/// `sdn-lockdown`(계획 67 §10.4): `SdnSpec.lockdown`을 `enabled`로 쓰고, 중앙마다의 에이전트가 SB를 바꿔 `applied`가
/// 원하는 대로 될 때까지(중앙 하나씩 — 에이전트가 정족수를 본다), 그 뒤 chassis·중앙 노드가 허용 목록을 맞출 때까지
/// 기다린다. 다른 요청이 그새 반대로 바꾸면 실패한다. 진행이 멈추면 기다리는 중앙과 그 노드가 알린 오류로 실패한다.
async fn lockdown(
    ctx: &Ctx,
    task: Uuid,
    enabled: bool,
    limits: LockdownLimits,
) -> Result<Option<String>, String> {
    let cluster = &ctx.cluster;
    write_progress(ctx, task, "writing-spec", MirrorProgress::default()).await;
    loop {
        let sdn = cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .map_err(|e| e.to_string())?
            .ok_or("SDN (OVN) is not set up")?;
        if sdn.status.phase != SdnPhase::Ready {
            return Err("SDN (OVN) is not ready yet".into());
        }
        if sdn.spec.lockdown == enabled {
            break;
        }
        let mut spec = sdn.spec.clone();
        spec.lockdown = enabled;
        match cluster
            .update_spec(sdn.id, Some(sdn.revision), sdn.labels.clone(), &spec)
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let result = lockdown_steps(ctx, task, enabled, limits).await;
    // 켜기가 실패하면 반쯤 걸린 채 두지 않는다(리뷰 2 I2): `SdnSpec.lockdown`을 다시 끈다. 그러면 에이전트가 중앙의 SB를
    // 하나씩 되돌리고 허용 목록을 걷는다(지우기가 실패하면 차례마다 다시 한다). Task가 끝난 뒤나 opld를 다시 띄운 뒤에도
    // 다시 걸지 않는다 — 원하는 잠금이 꺼짐이다. 실패 까닭은 `SdnStatus.lockdown.error`에 남는다(`finished`).
    match result {
        Err(e) if enabled => match turn_off_again(cluster).await {
            Ok(true) => Err(format!("{e}; turned the OVN lockdown off again")),
            Ok(false) => Err(e),
            Err(w) => Err(format!("{e}; cannot turn the OVN lockdown off again: {w}")),
        },
        other => other,
    }
}

/// 실패한 잠금 켜기 뒤 `SdnSpec.lockdown`을 끈다(켜져 있었으면 참). 다른 쓰기와 부딪치면 다시 읽는다.
async fn turn_off_again(cluster: &Cluster) -> Result<bool, String> {
    loop {
        let Some(sdn) = cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .map_err(|e| e.to_string())?
        else {
            return Ok(false);
        };
        if !sdn.spec.lockdown {
            return Ok(false);
        }
        let mut spec = sdn.spec.clone();
        spec.lockdown = false;
        match cluster
            .update_spec(sdn.id, Some(sdn.revision), sdn.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(true),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// `sdn-lockdown`의 스펙을 쓴 뒤의 단계: 중앙 바꾸기 → 허용 목록 기다리기
async fn lockdown_steps(
    ctx: &Ctx,
    task: Uuid,
    enabled: bool,
    limits: LockdownLimits,
) -> Result<Option<String>, String> {
    let cluster = &ctx.cluster;
    update_sdn(cluster, |s| {
        let state = s.lockdown.get_or_insert_with(Default::default);
        if state.pending && state.error.is_none() {
            return false;
        }
        state.pending = true;
        state.error = None;
        true
    })
    .await?;

    write_progress(ctx, task, "switching-centrals", MirrorProgress::default()).await;
    let mut seen: Option<Vec<Uuid>> = None;
    let mut deadline = tokio::time::Instant::now() + limits.stall;
    loop {
        let sdn = cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .map_err(|e| e.to_string())?
            .ok_or("SDN (OVN) was removed")?;
        if sdn.spec.lockdown != enabled {
            return Err("another request changed the OVN lockdown; this task stops".into());
        }
        let applied = sdn
            .status
            .lockdown
            .as_ref()
            .map(|l| l.applied.clone())
            .unwrap_or_default();
        let waiting = sdn
            .status
            .centrals
            .iter()
            .find(|c| applied.contains(&c.node) != enabled);
        let Some(waiting) = waiting else {
            break;
        };
        if seen.as_ref() != Some(&applied) {
            seen = Some(applied);
            deadline = tokio::time::Instant::now() + limits.stall;
        }
        if tokio::time::Instant::now() >= deadline {
            let node = cluster.get::<NodeSpec>(waiting.node).ok().flatten();
            let name = node
                .as_ref()
                .map_or_else(|| waiting.node.to_string(), |n| n.name.clone());
            let why = node
                .and_then(|n| n.status.sdn?.error)
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            return Err(format!(
                "the OVN central {name:?} did not switch its lockdown in {}s (is it online, with the other centrals in quorum?){why}",
                limits.stall.as_secs()
            ));
        }
        tokio::time::sleep(limits.step).await;
    }

    write_progress(ctx, task, "applying-firewall", MirrorProgress::default()).await;
    let deadline = tokio::time::Instant::now() + limits.firewall;
    loop {
        let (_, nodes) = cluster.list::<NodeSpec>().map_err(|e| e.to_string())?;
        let sdn = cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .map_err(|e| e.to_string())?
            .ok_or("SDN (OVN) was removed")?;
        let central = |id: Uuid| sdn.status.centrals.iter().any(|c| c.node == id);
        let pending: Vec<&Node> = nodes
            .iter()
            .filter(|n| !n.spec.witness)
            .filter(|n| {
                let Some(s) = &n.status.sdn else {
                    return false;
                };
                if s.chassis == ChassisState::Missing && !central(n.id) {
                    return false;
                }
                let on = s.lockdown.as_ref().is_some_and(|l| l.firewall);
                on != enabled
            })
            .collect();
        if pending.is_empty() {
            return Ok(None);
        }
        // 중앙이 표를 건 뒤 Raft가 끊겨 다시 걷었으면 성공으로 끝내지 않는다(리뷰 1 I5).
        if enabled
            && let Some((name, e)) = pending.iter().filter(|n| central(n.id)).find_map(|n| {
                let e = n.status.sdn.as_ref()?.lockdown.as_ref()?.error.clone()?;
                e.starts_with(LOCKDOWN_RAFT_CUT)
                    .then(|| (n.name.clone(), e))
            })
        {
            return Err(format!(
                "the OVN central {name:?} could not keep its address allow-list: {e}"
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            let names: Vec<String> = pending
                .iter()
                .map(|n| {
                    let state = if n.status.online { "" } else { " (offline)" };
                    let error = n
                        .status
                        .sdn
                        .as_ref()
                        .and_then(|s| s.lockdown.as_ref()?.error.clone())
                        .map(|e| format!(": {e}"))
                        .unwrap_or_default();
                    format!("{}{state}{error}", n.name)
                })
                .collect();
            return Ok(Some(format!(
                "the OVN address allow-list is not {} on {} yet; the SDN agent there keeps trying",
                if enabled { "applied" } else { "removed" },
                names.join(", ")
            )));
        }
        tokio::time::sleep(limits.step).await;
    }
}

/// 노드 `node`의 OVN 판 `have`가 클러스터의 판(`SdnStatus.release`, `24.03.8`)과 주·부 판이 같은지. 클러스터 판을 아직
/// 모르면 통과다.
pub(crate) fn check_release(node: &str, have: &str, cluster: Option<&str>) -> Result<(), String> {
    let Some(cluster) = cluster else {
        return Ok(());
    };
    if major_minor(have) == major_minor(cluster) {
        return Ok(());
    }
    Err(format!(
        "node {node:?} has OVN {have} but the SDN runs OVN {cluster}; install the same OVN release first"
    ))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 도는 SDN Task 하나
struct Run<'a> {
    ctx: &'a Ctx,
    task: Uuid,
    host: &'a OvnHost,
    sdn: Sdn,
    /// 이 노드
    me: Node,
}

impl Run<'_> {
    fn status(&self) -> &SdnStatus {
        &self.sdn.status
    }

    fn name(&self) -> &str {
        &self.me.name
    }

    /// 진행률 stage를 쓴다(바이트 0/0).
    async fn stage(&self, stage: &str) {
        write_progress(self.ctx, self.task, stage, MirrorProgress::default()).await;
    }

    /// 이 첫 세우기 Task가 아직 이 SDN의 첫 세우기인지: 첫 세우기 노드가 이 노드이고, 자리(`pending`)가 다른 Task의 것이
    /// 아니다.
    fn owns_init(&self, s: &SdnStatus) -> bool {
        s.init_node == Some(self.me.id) && s.pending.as_ref().is_none_or(|p| p.task == self.task)
    }

    fn moved(&self) -> String {
        format!(
            "the SDN (OVN) setup was moved off node {:?}; this task stops",
            self.name()
        )
    }

    /// 첫 세우기 Task의 상태 바꾸기: [`update_sdn`]과 같되 이 Task가 첫 세우기가 아니게 됐으면 쓰지 않고 실패한다.
    async fn update_init(
        &self,
        mut change: impl FnMut(&mut SdnStatus) -> bool,
    ) -> Result<(), String> {
        let mut lost = false;
        update_sdn(&self.ctx.cluster, |s| {
            if !self.owns_init(s) {
                lost = true;
                return false;
            }
            change(s)
        })
        .await?;
        if lost {
            return Err(self.moved());
        }
        Ok(())
    }

    /// 중앙 더하기·빼기는 SDN이 준비되었고 이 Task가 자리(`pending`)를 가졌을 때만 돈다.
    fn require_slot(&self) -> Result<(), String> {
        self.require_ready()?;
        if self
            .status()
            .pending
            .as_ref()
            .is_none_or(|p| p.task != self.task)
        {
            return Err(
                "this task does not hold the SDN central slot (another central task runs?)".into(),
            );
        }
        Ok(())
    }

    fn require_ready(&self) -> Result<(), String> {
        if self.status().phase != SdnPhase::Ready || self.status().centrals.is_empty() {
            return Err(super::failure::coded(
                TaskFailure::new(code::SDN_NOT_READY),
                "SDN (OVN) is not ready yet",
            ));
        }
        Ok(())
    }

    /// 이 노드의 Geneve 주소(`NodeStatus.underlay`)
    fn encap_ip(&self) -> Result<IpAddr, String> {
        self.me
            .status
            .underlay
            .ok_or_else(|| format!("node {:?} has no underlay address", self.name()))
    }

    /// 이 노드의 중앙 주소: 이미 중앙이면 그 주소, 아니면 `given`, 그것도 없으면 노드 주소의 IP나 underlay
    fn central_address(&self, given: Option<IpAddr>) -> Result<IpAddr, String> {
        if let Some(c) = self.status().centrals.iter().find(|c| c.node == self.me.id) {
            return Ok(c.address);
        }
        given.or_else(|| central_address(&self.me)).ok_or_else(|| {
            format!(
                "node {:?} has no IP address for the OVN central (its address is {:?}); give one",
                self.name(),
                self.me.spec.address
            )
        })
    }

    /// 지금 중앙 주소들(Raft 차례)에 `me`(이 노드의 주소)를 더한 것
    fn centrals_with(&self, me: IpAddr) -> Vec<IpAddr> {
        let mut list: Vec<IpAddr> = self
            .status()
            .centrals
            .iter()
            .filter(|c| c.node != self.me.id)
            .map(|c| c.address)
            .collect();
        list.push(me);
        list
    }

    /// `checking`: Linux인지, 이 노드에 남의 OVN이 없는지. 경고(`br-int`의 남의 포트)를 돌려준다.
    async fn check(&self, joining: bool) -> Result<Vec<String>, String> {
        self.stage("checking").await;
        self.host.check_platform().map_err(|e| e.to_string())?;
        self.host
            .check_foreign(self.name(), known_cids(self.status()), joining)
            .await
            .map_err(|e| e.to_string())
    }

    /// `installing-packages`: 꾸러미를 깔고 판이 SDN과 같은지 본다. 중앙이면 꾸러미가 만든 배포판 DB도 다시 본다(이미
    /// 깔려 있던 `ovn-central`의 DB).
    async fn install(&self, central: bool) -> Result<OvnVersions, String> {
        self.stage("installing-packages").await;
        let options = self.host.options();
        let packages = if central {
            &options.central_packages
        } else {
            &options.host_packages
        };
        let versions = self.host.ensure_packages(packages).await.map_err(|e| {
            super::failure::coded(
                TaskFailure::new(code::PACKAGE_INSTALL_FAILED).param("node", self.name()),
                e.to_string(),
            )
        })?;
        check_release(self.name(), &versions.ovn, self.status().release.as_deref())?;
        if central {
            self.host
                .check_distro_db(self.name())
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(versions)
    }

    /// `writing-tls`: 노드 TLS 사본. 중앙이면 배포판 중앙 단위도 끄고 가린다.
    async fn write_tls(&self, central: bool) -> Result<(), String> {
        self.stage("writing-tls").await;
        self.host.write_tls().map_err(|e| e.to_string())?;
        if central {
            let units = self
                .host
                .disable_distro_units()
                .await
                .map_err(|e| e.to_string())?;
            if !units.is_empty() {
                tracing::info!(units = ?units, "disabled the distribution OVN central units");
            }
        }
        Ok(())
    }

    /// `starting`: Orbivisor 단위를 쓰고 켠다. 잠금(`SdnSpec.lockdown`, 계획 67 §10.4)이면 이 노드의 SB도 처음부터
    /// 잠근 모양으로 뜬다(다른 중앙은 `applied`대로).
    async fn start(&self, address: IpAddr, centrals: &[IpAddr]) -> Result<(), String> {
        self.stage("starting").await;
        let mut locked = locked_addresses(self.status());
        locked.retain(|a| *a != address);
        if self.sdn.spec.lockdown {
            locked.push(address);
        }
        let changed = self
            .host
            .write_units(address, centrals, &locked)
            .await
            .map_err(|e| e.to_string())?;
        self.host
            .start_central(&changed)
            .await
            .map_err(|e| e.to_string())
    }

    /// `configuring-chassis`: 이 노드를 chassis로 구성한다(SB는 `centrals`).
    async fn chassis(&self, centrals: &[IpAddr]) -> Result<(), String> {
        self.stage("configuring-chassis").await;
        let chassis = Chassis {
            node: self.me.id,
            centrals,
            encap_ip: self.encap_ip()?,
        };
        self.host
            .configure_chassis(&chassis)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `sdn-init`: 이 노드에 NB·SB 클러스터와 northd를 세우고 chassis로 구성해 SDN을 `ready`로 한다.
    async fn init(&self, given: Option<IpAddr>) -> Result<Option<String>, String> {
        if !self.owns_init(self.status()) {
            return Err(self.moved());
        }
        // 첫 세우기 노드는 chassis이기도 하다(underlay가 없으면 꾸러미를 깔기 전에 멈춘다).
        self.encap_ip()?;
        let address = self.central_address(given)?;
        let warnings = self.check(false).await?;
        let versions = self.install(true).await?;
        self.update_init(|s| {
            if s.release.as_deref() == Some(versions.ovn.as_str()) {
                return false;
            }
            s.release = Some(versions.ovn.clone());
            true
        })
        .await?;
        self.write_tls(true).await?;

        self.stage("creating-cluster").await;
        // 준비된 SDN(또는 중앙이 이미 있음)에서는 새 클러스터를 만들지 않는다(리뷰 1 I-7 — DB를 치운 뒤 다시 하면 다른
        // 중앙과 갈라진 새 NB·SB가 선다). 세우는 중이면 DB는 아직 아무도 쓰지 않았으므로 Raft 주소가 다르면(주소를 바꿔
        // 다시 함) 이름만 바꿔 치우고 새로 만든다.
        let established =
            self.status().phase == SdnPhase::Ready || !self.status().centrals.is_empty();
        let mut stopped = false;
        for db in Db::ALL {
            if established && !self.host.paths().db_file(db).exists() {
                return Err(format!(
                    "the OVN {} database of node {:?} is missing; a ready SDN cannot get a second OVN cluster here — \
                     restore the file, or remove this central with `sdn central remove` and add it again",
                    db.name(),
                    self.name()
                ));
            }
            if let Some(old) = self
                .host
                .db_address_differs(db, address)
                .await
                .map_err(|e| e.to_string())?
            {
                if established {
                    return Err(format!(
                        "the OVN {} database of node {:?} uses Raft address {old}, not {address}; retry without address",
                        db.name(),
                        self.name()
                    ));
                }
                // 지난 시도가 켠 서버가 아직 그 파일을 쥐었으면 압축(임시 파일을 제자리로 이름 바꾸기)이 새 DB를 덮을 수
                // 있다(리뷰 2 m-3). 치우기 전에 멈춘다 — 아래 `start`가 새 주소로 다시 켠다.
                if !stopped {
                    self.host.stop_central().await.map_err(|e| e.to_string())?;
                    stopped = true;
                }
                let kept = self
                    .host
                    .retire_db(db, unix_now())
                    .map_err(|e| e.to_string())?;
                tracing::info!(db = db.name(), old = %old, kept = ?kept, "the setup address changed; recreating the unused OVN database");
            }
        }
        for db in Db::ALL {
            // 새 DB는 임시 파일에 만들고, 그 클러스터 id를 Raft에 적은 뒤에야 제자리에 둔다(§7.4 검사가 늘 맞게).
            if let Some(cid) = self
                .host
                .create_cluster(db, address)
                .await
                .map_err(|e| e.to_string())?
            {
                self.update_init(|s| {
                    let slot = match db {
                        Db::Nb => &mut s.nb_cid,
                        Db::Sb => &mut s.sb_cid,
                    };
                    if *slot == Some(cid) {
                        return false;
                    }
                    *slot = Some(cid);
                    true
                })
                .await?;
                self.host.commit_db(db).map_err(|e| e.to_string())?;
            }
        }
        let centrals = self.centrals_with(address);
        self.start(address, &centrals).await?;

        self.stage("waiting-for-leader").await;
        // 이미 준비된 SDN에서 다시 하면(중앙이 여럿일 수 있다) 리더가 아니라 멤버면 된다.
        let ready = self.status().phase == SdnPhase::Ready;
        let known = self
            .ctx
            .cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .ok()
            .flatten()
            .map(|s| known_cids(&s.status))
            .unwrap_or_default();
        for (db, cid) in [(Db::Nb, known.nb), (Db::Sb, known.sb)] {
            let status = if ready {
                self.host.wait_member(db, cid).await
            } else {
                self.host.wait_leader(db).await
            }
            .map_err(|e| e.to_string())?;
            if cid.is_some() && status.cid != cid {
                return Err(format!(
                    "the {} database runs cluster {:?}, not the SDN's {cid:?}",
                    db.name(),
                    status.cid
                ));
            }
            tracing::info!(db = db.name(), cid = ?status.cid, "the OVN database is up");
        }
        let me = self.me.id;
        let lockdown = self.sdn.spec.lockdown;
        self.update_init(|s| {
            let mut changed = false;
            if !s.centrals.iter().any(|c| c.node == me) {
                s.centrals.push(SdnCentral { node: me, address });
                changed = true;
            }
            if lockdown {
                changed |= mark_locked(s, me);
            }
            if s.phase != SdnPhase::Ready || s.last_error.is_some() {
                s.phase = SdnPhase::Ready;
                s.last_error = None;
                changed = true;
            }
            changed
        })
        .await?;
        self.chassis(&centrals).await?;
        Ok(join_warnings(warnings))
    }

    /// `sdn-central-add`: 기존 NB·SB 클러스터에 이 노드를 더하고 chassis로 구성한다.
    async fn add_central(&self) -> Result<Option<String>, String> {
        self.require_slot()?;
        let centrals = &self.status().centrals;
        let already = centrals.iter().any(|c| c.node == self.me.id);
        if !already && centrals.len() >= MAX_CENTRALS {
            return Err(format!(
                "SDN (OVN) can have at most {MAX_CENTRALS} central nodes"
            ));
        }
        self.encap_ip()?;
        let address = self.central_address(None)?;
        let mut warnings = self.check(true).await?;
        self.install(true).await?;
        self.write_tls(true).await?;

        let all = self.centrals_with(address);
        if let Err(e) = self.join(address).await {
            // 합류가 Raft에 적혔을 수 있다(멤버인데 중앙 목록에 없음 — 그 노드를 잃으면 정족수가 깨진다). 떠날 수 있으면
            // 떠나 치우고, 아니면 그대로 두어 다시 하거나 중앙 빼기로 떠나게 한다(리뷰 1 I-7).
            if already {
                return Err(e);
            }
            return Err(match self.leave_and_retire().await {
                Ok(()) => format!("{e}; left the OVN cluster again — retry the central add"),
                Err(why) => format!(
                    "{e}; cannot leave the OVN cluster again ({why}) — retry the central add, or remove the node with \
                     `sdn central remove`"
                ),
            });
        }
        self.chassis(&all).await?;
        if all.len().is_multiple_of(2) {
            warnings.push(format!(
                "SDN (OVN) now has {} central nodes; an even number tolerates no more failures than one \
                 fewer — add or remove one",
                all.len()
            ));
        }
        Ok(join_warnings(warnings))
    }

    /// `joining-cluster`·`starting`·`waiting-for-leader`: 기존 중앙에 합류하고 단위를 켜 멤버가 되면 중앙 목록에 더한다.
    async fn join(&self, address: IpAddr) -> Result<(), String> {
        self.stage("joining-cluster").await;
        let others: Vec<IpAddr> = self
            .status()
            .centrals
            .iter()
            .filter(|c| c.node != self.me.id)
            .map(|c| c.address)
            .collect();
        for db in Db::ALL {
            self.host
                .join_cluster(db, address, &others)
                .await
                .map_err(|e| e.to_string())?;
        }
        let all = self.centrals_with(address);
        self.start(address, &all).await?;

        self.stage("waiting-for-leader").await;
        let known = known_cids(self.status());
        for (db, cid) in [(Db::Nb, known.nb), (Db::Sb, known.sb)] {
            self.host
                .wait_member(db, cid)
                .await
                .map_err(|e| e.to_string())?;
        }
        let me = self.me.id;
        let lockdown = self.sdn.spec.lockdown;
        update_sdn(&self.ctx.cluster, |s| {
            let mut changed = false;
            if !s.centrals.iter().any(|c| c.node == me) {
                s.centrals.push(SdnCentral { node: me, address });
                changed = true;
            }
            if lockdown {
                changed |= mark_locked(s, me);
            }
            changed
        })
        .await
    }

    /// `leaving-cluster`·`stopping`: 이 노드의 NB·SB가 클러스터를 떠나고(떠나지 못하면 단위를 멈추지 않고 실패 —
    /// 멈춘 멤버는 정족수를 깎는다), 단위를 멈추고, DB 파일은 이름만 바꾼다.
    async fn leave_and_retire(&self) -> Result<(), String> {
        self.stage("leaving-cluster").await;
        for db in Db::ALL {
            if self.host.paths().db_file(db).exists() {
                self.host
                    .leave_cluster(db)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        self.stage("stopping").await;
        self.host.stop_central().await.map_err(|e| e.to_string())?;
        let moved = self
            .host
            .retire_dbs(unix_now())
            .map_err(|e| e.to_string())?;
        if !moved.is_empty() {
            tracing::info!(files = ?moved, "kept the OVN databases this node left");
        }
        Ok(())
    }

    /// `sdn-central-remove`: 이 노드가 NB·SB 클러스터를 떠나고, 단위를 멈추고, DB 파일은 이름만 바꾼 뒤 중앙 목록에서
    /// 뺀다. chassis 구성은 그대로다(SB 주소는 에이전트가 맞춘다). 중앙 목록에 없는데 DB가 남은 노드(끝나지 않은 중앙
    /// 더하기가 남긴 멤버)도 같이 떠난다.
    async fn remove_central(&self) -> Result<Option<String>, String> {
        self.require_slot()?;
        let centrals = &self.status().centrals;
        let listed = centrals.iter().any(|c| c.node == self.me.id);
        let has_db = Db::ALL
            .iter()
            .any(|db| self.host.paths().db_file(*db).exists());
        if !listed && !has_db {
            // 앞 Task가 이미 뺐다.
            return Ok(None);
        }
        if listed && centrals.len() <= 1 {
            return Err("the last OVN central node cannot be removed".into());
        }
        self.stage("checking").await;
        self.host.check_platform().map_err(|e| e.to_string())?;
        self.leave_and_retire().await?;
        if !listed {
            return Ok(Some(format!(
                "node {:?} was not a listed OVN central; it left the OVN cluster it had half joined",
                self.name()
            )));
        }
        let me = self.me.id;
        update_sdn(&self.ctx.cluster, |s| {
            let before = s.centrals.len();
            s.centrals.retain(|c| c.node != me);
            let unlocked = unmark_locked(s, me);
            s.centrals.len() != before || unlocked
        })
        .await?;
        Ok(None)
    }

    /// `sdn-central-kick`: 이 노드(살아 있는 중앙)가 꺼졌거나 지운 노드 `node`(Raft 주소 `address`)를 NB·SB 클러스터에서
    /// 내쫓고(`cluster/kick`, 목록에서 빠질 때까지 기다림) 중앙 목록과 지운 노드 기록(`departed`)에서 뺀다(리뷰 2 N-1·N-2).
    /// 마지막 중앙은 내쫓지 않는다. 멤버가 아니었으면(합류를 마치지 못함) 내쫓을 것 없이 기록만 지우고 알린다.
    async fn kick(&self, node: Uuid, address: IpAddr) -> Result<Option<String>, String> {
        self.require_slot()?;
        let centrals = &self.status().centrals;
        let listed = centrals.iter().any(|c| c.node == node);
        if listed && centrals.len() <= 1 {
            return Err("the last OVN central node cannot be removed".into());
        }
        if !centrals.iter().any(|c| c.node == self.me.id) {
            return Err(format!(
                "node {:?} is not an OVN central and cannot kick another server",
                self.name()
            ));
        }
        self.stage("kicking-server").await;
        let mut kicked = Vec::new();
        for db in Db::ALL {
            if self
                .host
                .kick(db, address)
                .await
                .map_err(|e| e.to_string())?
            {
                kicked.push(db.name());
            }
        }
        update_sdn(&self.ctx.cluster, |s| {
            let before = (s.centrals.len(), s.departed.len());
            s.centrals.retain(|c| c.node != node);
            s.departed.retain(|d| d.node != node);
            let unlocked = unmark_locked(s, node);
            (s.centrals.len(), s.departed.len()) != before || unlocked
        })
        .await?;
        if kicked.is_empty() {
            return Ok(Some(format!(
                "{address} was not a member of the OVN clusters; nothing to kick out"
            )));
        }
        tracing::info!(%node, %address, dbs = ?kicked, "kicked a dead server out of the OVN clusters");
        Ok(None)
    }

    /// `sdn-chassis-install`: chassis 꾸러미를 깔고 이 노드를 chassis로 구성한다.
    async fn install_chassis(&self) -> Result<Option<String>, String> {
        self.require_ready()?;
        self.encap_ip()?;
        let central = self.status().centrals.iter().any(|c| c.node == self.me.id);
        let warnings = self.check(central).await?;
        self.install(central).await?;
        self.write_tls(false).await?;
        let centrals: Vec<IpAddr> = self.status().centrals.iter().map(|c| c.address).collect();
        self.chassis(&centrals).await?;
        Ok(join_warnings(warnings))
    }
}

fn join_warnings(warnings: Vec<String>) -> Option<String> {
    (!warnings.is_empty()).then(|| warnings.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_must_match_by_major_and_minor() {
        check_release("n1", "24.03.8", None).unwrap();
        check_release("n1", "24.03.8", Some("24.03.2")).unwrap();
        let err = check_release("n1", "24.09.1", Some("24.03.8")).unwrap_err();
        assert!(
            err.contains("has OVN 24.09.1 but the SDN runs OVN 24.03.8"),
            "{err}"
        );
    }

    #[test]
    fn warnings_are_joined() {
        assert_eq!(join_warnings(Vec::new()), None);
        assert_eq!(
            join_warnings(vec!["a".into(), "b".into()]).as_deref(),
            Some("a; b")
        );
    }
}
