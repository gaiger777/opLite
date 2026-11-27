//! 복구 계획의 보호 상태(계획 76 설계 §3.1): 리더가 1분마다, 그리고 계획·백업·원격이 바뀌면 곧 계획마다
//! [`orbivisor_types::dr::protection`]을 다시 세어 바뀌었을 때만 `status.protection`에 쓴다(Raft 부하). 나이(`age_secs`)만
//! 바뀐 것은 바뀜으로 보지 않는다 — 저장된 나이는 `checked_at` 때의 것이고, 지금 나이는 보고(`/report`)가 다시 센다.
//! RPO를 넘는 순간은 `ok`가 바뀌므로 1분 안에 적힌다. 원격의 CA 지문은 저장된 원격(`RemoteSpec.ca_fingerprint`)에서
//! 읽으므로 원격에 닿지 않아도 센다.

use std::sync::Arc;
use std::time::{Duration, Instant};

use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError, Typed};
use orbivisor_types::backup::{Backup, BackupSpec};
use orbivisor_types::dr::{RecoveryPlan, RecoveryPlanSpec, RecoveryPlanStatus, VmProtection};
use orbivisor_types::remote::{Remote, RemoteSpec};
use orbivisor_types::resource::ResourceSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::backups::now;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct DrProtectionConfig {
    /// 계획·백업·원격이 바뀌었는지 보는 간격
    pub tick: Duration,
    /// 바뀐 것이 없어도 다시 세는 간격(나이가 RPO를 넘는 것을 잡는다)
    pub interval: Duration,
}

impl Default for DrProtectionConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(5),
            interval: Duration::from_secs(60),
        }
    }
}

/// 보호 상태 반복 작업. 모든 노드에서 띄우고 리더인 노드에서만 일한다.
pub struct DrProtection {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl DrProtection {
    pub fn spawn(state: Arc<AppState>, config: DrProtectionConfig) -> Self {
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

async fn run(
    state: Arc<AppState>,
    config: DrProtectionConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 마지막으로 센 때와 그때의 입력 표시. 리더가 아니게 되면 잊는다(다시 리더가 되면 곧 센다).
    let mut last: Option<(Instant, Signature)> = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        let cluster = &state.cluster;
        if cluster.members().leader != Some(cluster.id()) {
            last = None;
            continue;
        }
        let Some(sig) = signature(cluster) else {
            continue;
        };
        if last
            .as_ref()
            .is_some_and(|(at, s)| *s == sig && at.elapsed() < config.interval)
        {
            continue;
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        let done = tokio::select! {
            _ = &mut shutdown => return,
            done = refresh_all(cluster, now()) => done,
        };
        // 쓰기가 모두 됐을 때만 기억한다(못 쓴 계획은 다음 tick에 다시 본다). 쓴 계획의 revision이 바뀌었으므로 입력
        // 표시는 다시 읽는다.
        last = done
            .then(|| signature(cluster).map(|s| (Instant::now(), s)))
            .flatten();
    }
}

/// 입력(계획·백업·원격)이 바뀌었는지 싸게 알아보는 표시: 종류마다 (수, revision 합, 가장 큰 revision)
type Signature = [(usize, u64, u64); 3];

fn sig_of<S: ResourceSpec>(items: &[Typed<S>]) -> (usize, u64, u64) {
    items
        .iter()
        .fold((items.len(), 0u64, 0u64), |(n, sum, max), r| {
            (n, sum.wrapping_add(r.revision), max.max(r.revision))
        })
}

fn signature(cluster: &Cluster) -> Option<Signature> {
    Some([
        sig_of(&cluster.list::<RecoveryPlanSpec>().ok()?.1),
        sig_of(&cluster.list::<BackupSpec>().ok()?.1),
        sig_of(&cluster.list::<RemoteSpec>().ok()?.1),
    ])
}

/// 계획 `plan`의 원격(`source`)의 CA 지문. 원격이 없으면 `None`(모든 VM이 복구 지점 없음).
pub(crate) fn source_ca<'a>(plan: &RecoveryPlanSpec, remotes: &'a [Remote]) -> Option<&'a str> {
    remotes
        .iter()
        .find(|r| r.name == plan.source)
        .map(|r| r.spec.ca_fingerprint.as_str())
}

/// 나이를 빼고 같은지
fn same_ignoring_age(a: &[VmProtection], b: &[VmProtection]) -> bool {
    let strip = |v: &[VmProtection]| -> Vec<VmProtection> {
        v.iter()
            .cloned()
            .map(|mut p| {
                p.age_secs = None;
                p
            })
            .collect()
    };
    strip(a) == strip(b)
}

/// 계획 하나의 새 보호 상태. 바뀐 것이 없으면(나이 빼고) `None`.
pub(crate) fn recount(
    plan: &RecoveryPlan,
    backups: &[Backup],
    remotes: &[Remote],
    now: u64,
) -> Option<RecoveryPlanStatus> {
    let protection =
        orbivisor_types::dr::protection(&plan.spec, backups, source_ca(&plan.spec, remotes), now);
    if plan.status.checked_at.is_some() && same_ignoring_age(&plan.status.protection, &protection) {
        return None;
    }
    Some(RecoveryPlanStatus {
        protection,
        checked_at: Some(now),
        ..plan.status.clone()
    })
}

/// 모든 계획을 다시 센다. 모두 읽고 쓸 수 있었으면 참.
pub(crate) async fn refresh_all(cluster: &Cluster, now: u64) -> bool {
    let read = || -> Result<_, ClusterError> {
        Ok((
            cluster.list::<RecoveryPlanSpec>()?.1,
            cluster.list::<BackupSpec>()?.1,
            cluster.list::<RemoteSpec>()?.1,
        ))
    };
    let (plans, backups, remotes) = match read() {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "cannot read recovery plans");
            return false;
        }
    };
    let mut ok = true;
    for plan in plans {
        let Some(status) = recount(&plan, &backups, &remotes, now) else {
            continue;
        };
        match cluster
            .update_status_checked::<RecoveryPlanSpec>(plan.id, plan.revision, &status)
            .await
        {
            Ok(_) => {}
            // 그사이 계획이 바뀌었거나 지워졌다: 다음 tick에 다시 본다.
            Err(ClusterError::Rejected(
                Rejection::RevisionMismatch { .. } | Rejection::NotFound { .. },
            )) => ok = false,
            Err(e) => {
                tracing::warn!(plan = %plan.name, error = %e, "cannot store the DR protection state");
                ok = false;
            }
        }
    }
    ok
}
