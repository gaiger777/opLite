//! VMware 이전 Task(계획 72 설계 §9·§10.3). 대상은 `vmware-vm` 항목이고 항목의 줄에서 차례로 돈다.
//!
//! - `vmware-sync`: 회차 하나(§9.2). 따뜻한 이전은 "CBT 켜기 → S(n+1) 만들기 → S(n) 지우기(합치기 — 기반 flat 파일이 S(n+1)
//!   시점으로 언다) → 디스크마다 `QueryChangedDiskAreas(S(n+1), changeId_n)`의 구역만 기반에서 읽어 대상 볼륨에 그 자리 쓰기".
//!   첫 회차는 `"*"`(할당된 구역 전체)다. 차가운 이전(꺼진 VM)은 스냅샷·CBT 없이 기반 전체를 읽는다. 걸음마다 Raft에 먼저
//!   적으므로(스냅샷 moref, 디스크별 체크포인트 `copied_bytes`, 회차가 끝난 디스크의 `rounds`) 다시 뜬 opld가 이어 간다 — 한
//!   회차 동안 기반은 얼어 있어 같은 구역을 다시 읽으면 같은 바이트다.
//! - `vmware-cleanup`: 취소(대상 볼륨도 지움 → `cancelled`)·마무리(→ `finalized`). 우리 스냅샷(Raft에 적힌 moref만)·CBT
//!   (우리가 켰을 때만)을 치운다. 취소는 바꾼 원본 설정을 되돌리고, 마무리는 원본 보호(NIC `startConnected=false`·설명)를
//!   그대로 둔다(§10.3). 원본 VM·디스크·사용자 스냅샷은 건드리지 않는다.
//! - `vmware-cutover`·`vmware-revert`: 전환 상태 기계와 켠 뒤 되돌리기([`cutover`] 모듈, 설계 §10).
//!
//! 쓰기: 대상 볼륨을 `qemu-nbd`로 쓰기로 열고(`Storage::open_target`, 연결 하나) 0 조각은 `WRITE_ZEROES`(씬 유지), 체크포인트
//! 전과 끝에 `FLUSH`한다. 읽기는 디스크마다 `streams_per_disk`개의 `/folder` 범위 읽개(VDDK 길은 nbdkit 하나)로 나란히 하고
//! 쓰기는 차례대로 한다.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{DriverError, MirrorProgress};
use opl_store::nbd::NbdClient;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::Task;
use opl_types::vmware::{
    MAX_SYNC_FAILURES, VmwareApiKind, VmwareMigration, VmwareMigrationMode, VmwareMigrationPhase,
    VmwareMigrationSpec, VmwareMigrationStatus, VmwareSnapshotRef, VmwareSource, VmwareSourceSpec,
    VmwareSyncDisk, VmwareTransport, VmwareVm, VmwareVmSpec, VmwareVmState, VmwareVmStatus,
    snapshot_name,
};
use uuid::Uuid;

use super::backup::ready_pool;
use super::backup_store::socket_path;
use super::migrate::write_progress;
use super::{Ctx, set_volume_size};
use crate::backups::now;
use crate::vmware::VsphereError;
use crate::vmware::cbt::{self, ALL_AREAS, CHUNK};
use crate::vmware::client::Session;
use crate::vmware::inventory::{self, InvVm, Inventory};
use crate::vmware::reader::{DiskReader, FolderReader, HostRoute, NbdkitReader, VddkParams};
use crate::vmware::soap::MoRef;
use crate::vmware_migrations::{counts, plan};
use crate::volumes::default_format;

/// 디스크별 체크포인트(`copied_bytes`)를 적는 간격(바이트). 시험은 `TaskRunnerConfig::vmware_checkpoint_bytes`로 줄인다.
pub const CHECKPOINT_BYTES: u64 = 512 << 20;
/// 체크포인트를 적는 간격(시간)
const CHECKPOINT_EVERY: Duration = Duration::from_secs(30);
/// 사이가 이만큼 이하인 구역은 이어 읽는다([`cbt::coalesce`]).
const COALESCE_GAP: u64 = 256 << 10;
/// 읽기·쓰기 한 조각이 이만큼 끝나지 않으면(바이트가 늘지 않으면) 회차가 실패한다(설계 §9.5).
pub const STALL: Duration = Duration::from_secs(600);
/// 전환이 만든 VM이 `running`으로 보이기를 기다리는 시간(설계 §10.1)
pub const START_TIMEOUT: Duration = Duration::from_secs(300);

mod cutover;
pub(super) use cutover::{cutover, revert};

/// 동기화가 멈춘 까닭
#[derive(Debug)]
struct SyncError {
    message: String,
    /// 다시 해도 같다 — 항목을 곧 `failed`로 둔다.
    permanent: bool,
    /// 원본이 무료 ESXi(API 읽기 전용)다 — 원본 연결 상태에 적는다.
    restricted: bool,
    /// 회차 중 우리 것이 아닌 스냅샷이 보였다 — 따뜻한 동기화는 이번 회차를 건너뛴다(실패로 세지 않음).
    foreign: bool,
}

impl SyncError {
    fn retry(message: impl Into<String>) -> Self {
        SyncError {
            message: message.into(),
            permanent: false,
            restricted: false,
            foreign: false,
        }
    }

    fn fatal(message: impl Into<String>) -> Self {
        SyncError {
            message: message.into(),
            permanent: true,
            restricted: false,
            foreign: false,
        }
    }
}

/// 우리 것이 아닌 스냅샷이 있다는 말(이름들)
fn foreign_message(names: &[&str]) -> String {
    format!(
        "the VM has snapshots that are not Orbivisor's ({}); consolidate or delete them",
        names.join(", ")
    )
}

impl From<VsphereError> for SyncError {
    fn from(e: VsphereError) -> Self {
        let restricted = matches!(e, VsphereError::Restricted(_));
        SyncError {
            message: e.to_string(),
            // 권한·인증·무료 ESXi는 다시 해도 같다.
            permanent: restricted
                || matches!(
                    e,
                    VsphereError::Auth { .. }
                        | VsphereError::NoPermission { .. }
                        | VsphereError::Fingerprint { .. }
                ),
            restricted,
            foreign: false,
        }
    }
}

impl From<String> for SyncError {
    fn from(message: String) -> Self {
        SyncError::retry(message)
    }
}

impl From<ClusterError> for SyncError {
    fn from(e: ClusterError) -> Self {
        SyncError::retry(e.to_string())
    }
}

/// 사람이 읽는 크기
fn size_text(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ---- 항목 읽기·쓰기 ----

fn get_item(ctx: &Ctx, id: Uuid) -> Result<VmwareVm, String> {
    ctx.cluster
        .get::<VmwareVmSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "vmware-vm was deleted".to_string())
}

/// 항목 상태를 `change`로 고쳐 쓴다(읽은 revision 확인 — 다른 쓰기와 부딪치면 다시 읽어 다시 고친다). 바뀐 것이 없으면
/// 쓰지 않는다. 고친 항목을 돌려준다.
async fn update_item(
    ctx: &Ctx,
    id: Uuid,
    change: impl Fn(&mut VmwareVmStatus),
) -> Result<VmwareVm, String> {
    loop {
        let mut item = get_item(ctx, id)?;
        let mut status = item.status.clone();
        change(&mut status);
        if status == item.status {
            return Ok(item);
        }
        match ctx
            .cluster
            .update_status_checked::<VmwareVmSpec>(id, item.revision, &status)
            .await
        {
            Ok(revision) => {
                item.status = status;
                item.revision = revision;
                return Ok(item);
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(format!("cannot record the migration state: {e}")),
        }
    }
}

/// 계획의 항목 상태별 수를 다시 센다(요약만 — 실패는 넘어간다).
async fn recount(ctx: &Ctx, migration: Uuid) {
    for _ in 0..5 {
        let Ok(Some(m)) = ctx.cluster.get::<VmwareMigrationSpec>(migration) else {
            return;
        };
        let Ok((_, items)) = ctx.cluster.list::<VmwareVmSpec>() else {
            return;
        };
        let items: Vec<VmwareVm> = items
            .into_iter()
            .filter(|i| i.spec.migration == migration)
            .collect();
        let status = VmwareMigrationStatus {
            counts: counts(&items),
            ..m.status.clone()
        };
        if status == m.status {
            return;
        }
        match ctx
            .cluster
            .update_status_checked::<VmwareMigrationSpec>(migration, m.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => {
                tracing::warn!(migration = %migration, error = %e, "cannot recount the migration");
                return;
            }
            Ok(_) => return,
        }
    }
}

/// 따뜻한 이전의 델타 주기(초): 계획의 `sync.interval_min`, 시험은 실행기 설정이 덮어쓴다.
pub(super) fn sync_every(ctx: &Ctx, m: &VmwareMigration) -> u64 {
    ctx.config
        .vmware_sync_every
        .map_or(u64::from(m.spec.sync.interval_min) * 60, |d| {
            d.as_secs().max(1)
        })
}

fn get_migration(ctx: &Ctx, id: Uuid) -> Result<VmwareMigration, SyncError> {
    ctx.cluster
        .get::<VmwareMigrationSpec>(id)?
        .ok_or_else(|| SyncError::fatal("the vmware-migration of this VM was deleted"))
}

fn get_source(ctx: &Ctx, m: &VmwareMigration) -> Result<VmwareSource, SyncError> {
    ctx.cluster
        .get::<VmwareSourceSpec>(m.spec.source)?
        .ok_or_else(|| SyncError::fatal("the vmware-source of this migration no longer exists"))
}

/// 원본의 세션: API와 같은 노드 캐시(세션 5분)를 쓰고, 없으면(실행기만 띄운 시험) 새로 로그인한다.
async fn session_for(ctx: &Ctx, source: &VmwareSource) -> Result<Arc<Session>, VsphereError> {
    match &ctx.config.api {
        Some(api) => api.vmware_cache.session(source).await,
        None => Session::login(&source.spec).await.map(Arc::new),
    }
}

async fn inventory_for(
    ctx: &Ctx,
    source: &VmwareSource,
    session: &Session,
    refresh: bool,
) -> Result<Arc<Inventory>, VsphereError> {
    match &ctx.config.api {
        Some(api) => api
            .vmware_cache
            .inventory(source, refresh)
            .await
            .map(|(inv, _)| inv),
        None => inventory::inventory(session).await.map(Arc::new),
    }
}

/// 원본 연결에 무료 ESXi임을 적는다(미리 확인이 따뜻한 이전을 막는다).
async fn mark_restricted(ctx: &Ctx, source: Uuid) {
    let Ok(Some(s)) = ctx.cluster.get::<VmwareSourceSpec>(source) else {
        return;
    };
    if s.status.restricted {
        return;
    }
    let mut status = s.status.clone();
    status.restricted = true;
    if let Err(e) = ctx
        .cluster
        .update_status::<VmwareSourceSpec>(source, &status)
        .await
    {
        tracing::warn!(source = %source, error = %e, "cannot record that the source is restricted");
    }
}

/// 한 번의 동기화에 드는 것
struct Run<'a> {
    ctx: &'a Ctx,
    task: &'a Task,
    item: Uuid,
    migration: VmwareMigration,
    source: VmwareSource,
    session: Arc<Session>,
    inv: Arc<Inventory>,
    vm: InvVm,
    warnings: Mutex<Vec<String>>,
}

impl Run<'_> {
    fn warn(&self, w: impl Into<String>) {
        let w = w.into();
        let mut ws = self.warnings.lock().unwrap_or_else(|e| e.into_inner());
        if !ws.contains(&w) {
            ws.push(w);
        }
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn vm_ref(&self) -> MoRef {
        MoRef::new("VirtualMachine", &self.vm.moref)
    }

    fn warm(&self) -> bool {
        self.migration.spec.mode == VmwareMigrationMode::Warm
    }
}

/// 항목 하나를 다룰 [`Run`]을 연다: 원본 세션, 인벤토리(노드 캐시 — 없는 VM이면 새로), VM 하나를 새로 읽고 같은 VM인지
/// (instanceUuid) 본다.
async fn open_run<'a>(
    ctx: &'a Ctx,
    task: &'a Task,
    item: &VmwareVm,
    migration: VmwareMigration,
) -> Result<Run<'a>, SyncError> {
    let source = get_source(ctx, &migration)?;
    let session = session_for(ctx, &source).await?;
    let mut inv = inventory_for(ctx, &source, &session, false).await?;
    if inv.vm(&item.spec.source_ref).is_none() {
        inv = inventory_for(ctx, &source, &session, true).await?;
    }
    let vm = match inventory::refresh_vm(&session, &inv, &item.spec.source_ref).await {
        Ok(vm) => vm,
        Err(VsphereError::NotFound { .. }) => {
            return Err(SyncError::fatal(format!(
                "vmware_vm_gone: the VM {} no longer exists on the source",
                item.spec.source_ref
            )));
        }
        Err(e) => return Err(e.into()),
    };
    if vm.instance_uuid.as_deref() != Some(item.spec.instance_uuid.as_str()) {
        return Err(SyncError::fatal(format!(
            "vmware_vm_replaced: {} is now another VM (instance UUID {})",
            item.spec.source_ref,
            vm.instance_uuid.as_deref().unwrap_or("-")
        )));
    }
    Ok(Run {
        ctx,
        task,
        item: item.id,
        migration,
        source,
        session,
        inv,
        vm,
        warnings: Mutex::default(),
    })
}

// ---- vmware-sync ----

/// `vmware-sync` Task: 회차 하나. 실패하면 항목에 이유를 적고, 다시 해도 같은 실패이거나 연달아
/// [`MAX_SYNC_FAILURES`]번이면 항목을 `failed`로 둔다(아니면 스케줄러가 다음 틱에 다시 — 체크포인트부터).
pub(super) async fn sync(ctx: &Ctx, task: &Task, full: bool) -> Result<Option<String>, String> {
    let id = task.spec.target;
    match Box::pin(run_sync(ctx, task, full)).await {
        Ok(warning) => Ok(warning),
        Err(e) => {
            if e.restricted
                && let Ok(item) = get_item(ctx, id)
                && let Ok(Some(m)) = ctx.cluster.get::<VmwareMigrationSpec>(item.spec.migration)
            {
                mark_restricted(ctx, m.spec.source).await;
            }
            record_failure(ctx, id, &e).await;
            Err(e.message)
        }
    }
}

/// 실패를 항목에 적는다(끝난·취소된 항목은 그대로).
async fn record_failure(ctx: &Ctx, id: Uuid, e: &SyncError) {
    let at = now();
    let updated = update_item(ctx, id, |s| {
        if !matches!(
            s.state,
            VmwareVmState::Pending
                | VmwareVmState::Copying
                | VmwareVmState::Syncing
                | VmwareVmState::Ready
                | VmwareVmState::RolledBack
        ) {
            return;
        }
        s.error = Some(e.message.clone());
        s.sync_failures += 1;
        if e.permanent || s.sync_failures >= MAX_SYNC_FAILURES {
            s.state = VmwareVmState::Failed;
            s.push_history(at, format!("sync failed: {}", e.message));
        } else {
            s.push_history(
                at,
                format!(
                    "sync attempt {} of {MAX_SYNC_FAILURES} failed",
                    s.sync_failures
                ),
            );
        }
    })
    .await;
    match updated {
        Ok(item) => recount(ctx, item.spec.migration).await,
        Err(err) => tracing::warn!(item = %id, error = %err, "cannot record a failed VMware sync"),
    }
}

async fn run_sync(ctx: &Ctx, task: &Task, full: bool) -> Result<Option<String>, SyncError> {
    let id = task.spec.target;
    let item = get_item(ctx, id)?;
    if !matches!(
        item.status.state,
        VmwareVmState::Pending
            | VmwareVmState::Copying
            | VmwareVmState::Syncing
            | VmwareVmState::RolledBack
    ) {
        return Ok(Some(format!(
            "skipped: vmware-vm {:?} is {}",
            item.name,
            item.status.state.as_str()
        )));
    }
    let migration = get_migration(ctx, item.spec.migration)?;
    if migration.status.phase == VmwareMigrationPhase::Cancelled {
        return Ok(Some("skipped: the migration was cancelled".into()));
    }
    let mut run = Box::pin(open_run(ctx, task, &item, migration)).await?;
    if let Some(sent) = item.status.shutdown_sent_at {
        Box::pin(cutover::recheck_shutdown(&mut run, sent)).await?;
    }
    let run = run;

    // 우리 스냅샷(Raft의 moref)과 이번 회차에 만들 이름이 아닌 스냅샷은 사용자 것이다(백업 제품 등).
    let n = item.status.rounds + 1;
    let expected = snapshot_name(id, n);
    let ours: Vec<&str> = [&item.status.snapshot, &item.status.pending_snapshot]
        .into_iter()
        .flatten()
        .map(|s| s.moref.as_str())
        .collect();
    let foreign: Vec<&str> = run
        .vm
        .snapshots
        .iter()
        .filter(|s| !ours.contains(&s.moref.as_str()) && s.name != expected)
        .map(|s| s.name.as_str())
        .collect();
    if !foreign.is_empty() {
        let message = foreign_message(&foreign);
        if !run.warm() {
            return Err(SyncError::fatal(format!("vmware_snapshots: {message}")));
        }
        return Box::pin(skip_round(&run, &message)).await;
    }
    // `stop_after_days`가 지나도록 전환하지 않았다: 우리 스냅샷을 지우고 동기화를 멈춘다(§9.3 — changeId는 남겨 다시
    // 시작하면 `"*"` 대신 이어 간다).
    let stop_days = u64::from(run.migration.spec.sync.stop_after_days);
    if run.warm()
        && item.status.rounds > 0
        && stop_days > 0
        && item
            .status
            .started_at
            .is_some_and(|t| now() >= t + stop_days * 86_400)
    {
        return Box::pin(stop_syncing(&run, stop_days)).await;
    }
    if !run.warm() && run.vm.power != Some(opl_types::vmware::VmwarePower::PoweredOff) {
        return Err(SyncError::fatal(
            "vmware_powered_on: the VM is powered on; power it off or use a warm migration",
        ));
    }

    // 시작을 적는다(원본 정보도 새로).
    let at = now();
    let fresh = run.vm.to_source();
    let item = update_item(ctx, id, |s| {
        if s.state == VmwareVmState::Pending {
            s.state = VmwareVmState::Copying;
            s.push_history(at, "copy started");
        }
        s.started_at.get_or_insert(at);
        s.source = Some(fresh.clone());
    })
    .await?;
    recount(ctx, item.spec.migration).await;

    let targets = Box::pin(ensure_disks(&run)).await?;
    let started = Instant::now();
    let copied = if run.warm() {
        match Box::pin(warm_round(&run, &targets, full)).await {
            Err(e) if e.foreign => {
                let message = e.message.trim_start_matches("vmware_snapshots: ");
                return Box::pin(skip_round(&run, message)).await;
            }
            copied => copied?,
        }
    } else {
        Box::pin(cold_round(&run, &targets)).await?
    };
    let state = if run.warm() {
        VmwareVmState::Syncing
    } else {
        VmwareVmState::Ready
    };
    finish_round(&run, copied, started.elapsed(), Some(state)).await
}

/// 따뜻한 이전: 우리 것이 아닌 스냅샷 때문에 이번 회차를 건너뛰고 다음 주기에 다시 본다(설계 §9.2).
async fn skip_round(run: &Run<'_>, message: &str) -> Result<Option<String>, SyncError> {
    let at = now();
    let every = sync_every(run.ctx, &run.migration);
    let warning = format!("{message}; skipping this round until only Orbivisor snapshots remain");
    let item = update_item(run.ctx, run.item, |s| {
        s.next_sync_at = Some(at + every);
        if !s.warnings.contains(&warning) {
            s.warnings.push(warning.clone());
        }
        s.push_history(at, "round skipped: foreign snapshots");
    })
    .await?;
    recount(run.ctx, item.spec.migration).await;
    Ok(Some(warning))
}

/// 오래 전환하지 않은 따뜻한 항목의 동기화를 멈춘다: 우리 스냅샷을 지우고 다음 동기화를 지운다(`retry`가 다시 시작한다).
async fn stop_syncing(run: &Run<'_>, days: u64) -> Result<Option<String>, SyncError> {
    let item = get_item(run.ctx, run.item)?;
    for snap in [&item.status.pending_snapshot, &item.status.snapshot]
        .into_iter()
        .flatten()
    {
        remove_ours(&run.session, snap).await?;
        let moref = snap.moref.clone();
        update_item(run.ctx, run.item, |s| {
            if s.pending_snapshot
                .as_ref()
                .is_some_and(|p| p.moref == moref)
            {
                s.pending_snapshot = None;
            }
            if s.snapshot.as_ref().is_some_and(|p| p.moref == moref) {
                s.snapshot = None;
            }
        })
        .await?;
    }
    let at = now();
    let warning = format!(
        "synchronization stopped: not cut over within {days} days; the Orbivisor snapshot was removed \
         (retry to continue from the last changed block tracking position)"
    );
    let item = update_item(run.ctx, run.item, |s| {
        s.next_sync_at = None;
        s.estimate_s = None;
        if !s.warnings.contains(&warning) {
            s.warnings.push(warning.clone());
        }
        s.push_history(at, "synchronization stopped");
    })
    .await?;
    recount(run.ctx, item.spec.migration).await;
    Ok(Some(warning))
}

/// 회차를 끝낸다: 회차 수·시각·다음 동기화·예상 내릴 시간, 상태(`state` — 없으면 그대로, 전환 중의 회차), 오류·실패 수를
/// 지운다.
async fn finish_round(
    run: &Run<'_>,
    copied: u64,
    took: Duration,
    state: Option<VmwareVmState>,
) -> Result<Option<String>, SyncError> {
    let at = now();
    let warm = run.warm();
    let every = sync_every(run.ctx, &run.migration);
    let warnings = run.warnings();
    let secs = took.as_secs_f64().max(0.001);
    let item = update_item(run.ctx, run.item, |s| {
        let n = s.rounds + 1;
        s.rounds = n;
        s.last_sync_at = Some(at);
        s.next_sync_at = warm.then_some(at + every);
        // 예상 마지막 델타 시간 = 이번 델타 바이트 ÷ 잰 속도(첫 전체 복사는 델타가 아니다)
        s.estimate_s = (warm && n > 1).then(|| {
            let delta: u64 = s.disks.iter().map(|d| d.last_delta_bytes).sum();
            let speed = copied as f64 / secs;
            if speed > 0.0 {
                (delta as f64 / speed).ceil() as u64
            } else {
                0
            }
        });
        if let Some(state) = state {
            s.state = state;
            s.error = None;
        }
        s.sync_failures = 0;
        for w in &warnings {
            if !s.warnings.contains(w) {
                s.warnings.push(w.clone());
            }
        }
        let what = if n == 1 { "full copy" } else { "delta" };
        s.push_history(
            at,
            format!(
                "round {n} ({what}): {} in {}s",
                size_text(copied),
                took.as_secs()
            ),
        );
    })
    .await?;
    recount(run.ctx, item.spec.migration).await;
    Ok((!warnings.is_empty()).then(|| warnings.join("; ")))
}

/// 대상 디스크 하나: 볼륨과 그 풀
struct Target {
    volume: Volume,
    pool: StoragePool,
    /// 이 Task가 방금 만든 볼륨(비어 있다 — 전체 복사가 빈 곳을 0으로 쓸 필요가 없다)
    fresh: bool,
}

/// 디스크 표를 채우고(첫 동기화 — 볼륨 id를 먼저 적음) 대상 볼륨을 만든다(있으면 그대로). 원본 디스크가 바뀌었으면(수·키)
/// 실패, 커졌으면 볼륨을 늘리고 그 디스크를 전체로 다시, 대상 파일이 없어졌으면 그 디스크를 전체로 다시 복사한다.
async fn ensure_disks(run: &Run<'_>) -> Result<Vec<Target>, SyncError> {
    let ctx = run.ctx;
    let item = get_item(ctx, run.item)?;
    let format = |pool: &str| {
        ctx.cluster
            .get_by_name::<StoragePoolSpec>(pool)
            .ok()
            .flatten()
            .map(|p| default_format(&p))
            .unwrap_or(ImageFormat::Qcow2)
    };
    let requests = plan::disk_volumes(
        &run.migration.spec.rules,
        item.id,
        &item.spec,
        &run.vm,
        &format,
    )
    .map_err(|problems| {
        SyncError::fatal(
            problems
                .iter()
                .map(|p| format!("{}: {}", p.code, p.message))
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    if item.status.disks.is_empty() {
        let disks: Vec<VmwareSyncDisk> = run
            .vm
            .disks
            .iter()
            .map(|d| VmwareSyncDisk {
                key: d.key,
                file: d.file.clone(),
                capacity_bytes: d.capacity,
                volume: Some(Uuid::now_v7()),
                change_id: None,
                copied_bytes: 0,
                rounds: 0,
                last_delta_bytes: 0,
                source_mtime: None,
            })
            .collect();
        // 만들기 전에 적는다: 만들다 끊겨도 다음 시도와 정리가 볼륨을 찾는다.
        update_item(ctx, run.item, |s| {
            if s.disks.is_empty() {
                s.disks = disks.clone();
            }
        })
        .await?;
    }
    let item = get_item(ctx, run.item)?;
    let keys: Vec<i32> = item.status.disks.iter().map(|d| d.key).collect();
    let now_keys: Vec<i32> = run.vm.disks.iter().map(|d| d.key).collect();
    if keys != now_keys {
        return Err(SyncError::fatal(
            "the VM's disks changed; cancel and start this VM again",
        ));
    }
    let mut out = Vec::new();
    for (i, (disk, request)) in item.status.disks.iter().zip(&requests).enumerate() {
        let vol_id = disk
            .volume
            .ok_or_else(|| SyncError::fatal(format!("disk {i} has no target volume")))?;
        let capacity = run.vm.disks[i].capacity;
        if capacity < disk.capacity_bytes {
            return Err(SyncError::fatal(format!(
                "disk {i} of the VM shrank; cancel and start this VM again"
            )));
        }
        let volume = match ctx.cluster.get::<VolumeSpec>(vol_id)? {
            Some(v) => v,
            None => {
                let mut v: Volume = Resource::new(request.name.clone(), request.spec.clone());
                v.id = vol_id;
                v.labels = request.labels.clone();
                v.spec.size_bytes = disk.capacity_bytes;
                v.status.node = Some(ctx.config.node);
                match ctx.cluster.create(&v).await {
                    Ok(revision) => v.revision = revision,
                    Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => {
                        return Err(SyncError::fatal(format!(
                            "volume name {:?} is already taken; rename or delete that volume",
                            request.name
                        )));
                    }
                    Err(e) => return Err(e.into()),
                }
                v
            }
        };
        if volume.status.phase != VolumePhase::Pending {
            return Err(SyncError::fatal(format!(
                "target volume {:?} is no longer pending",
                volume.name
            )));
        }
        if volume.status.node.is_some_and(|n| n != ctx.config.node)
            && !opl_types::storage::is_shared(&request_pool(ctx, &volume)?)
        {
            return Err(SyncError::fatal(format!(
                "target volume {:?} is on another node's local pool",
                volume.name
            )));
        }
        let pool = ready_pool(ctx, &volume.spec.pool)?;
        let made = match ctx
            .storage
            .create_volume(&pool, vol_id, disk.capacity_bytes, volume.spec.format)
            .await
        {
            Ok(_) => true,
            Err(DriverError::VolumeExists(_)) => false,
            Err(e) => {
                return Err(SyncError::retry(format!(
                    "cannot create volume {:?}: {e}",
                    volume.name
                )));
            }
        };
        if made && (disk.rounds > 0 || disk.copied_bytes > 0 || disk.change_id.is_some()) {
            run.warn(format!(
                "the target volume {:?} was missing; copying disk {i} again in full",
                volume.name
            ));
            restart_disk(ctx, run.item, i).await?;
        }
        if capacity > disk.capacity_bytes {
            ctx.storage
                .resize_volume(&pool, vol_id, volume.spec.format, capacity)
                .await
                .map_err(|e| SyncError::retry(format!("cannot grow {:?}: {e}", volume.name)))?;
            set_volume_size(ctx, vol_id, capacity).await?;
            run.warn(format!(
                "disk {i} grew to {}; copying it again in full",
                size_text(capacity)
            ));
            update_item(ctx, run.item, |s| {
                if let Some(d) = s.disks.get_mut(i) {
                    d.capacity_bytes = capacity;
                }
            })
            .await?;
            restart_disk(ctx, run.item, i).await?;
        }
        out.push(Target {
            volume,
            pool,
            fresh: made,
        });
    }
    Ok(out)
}

fn request_pool(ctx: &Ctx, volume: &Volume) -> Result<StoragePool, SyncError> {
    ctx.cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
        .ok_or_else(|| SyncError::fatal(format!("storage pool {:?} not found", volume.spec.pool)))
}

/// 디스크 `i`를 처음부터 다시 복사하게 둔다(`"*"`, 체크포인트·회차 표시를 지움).
async fn restart_disk(ctx: &Ctx, item: Uuid, i: usize) -> Result<(), String> {
    update_item(ctx, item, |s| {
        if let Some(d) = s.disks.get_mut(i) {
            d.change_id = None;
            d.copied_bytes = 0;
            d.rounds = 0;
        }
    })
    .await
    .map(|_| ())
}

/// 우리 스냅샷 하나를 지운다(합침). 이미 없으면 된 것이다.
async fn remove_ours(session: &Session, snap: &VmwareSnapshotRef) -> Result<(), VsphereError> {
    match session
        .remove_snapshot(&MoRef::new("VirtualMachineSnapshot", &snap.moref))
        .await
    {
        Ok(()) | Err(VsphereError::NotFound { .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// 따뜻한 회차(§9.2): CBT 켜기 → S(n+1) → S(n) 지우기 → 디스크마다 바뀐 구역. 복사한 바이트를 돌려준다.
async fn warm_round(run: &Run<'_>, targets: &[Target], full: bool) -> Result<u64, SyncError> {
    let ctx = run.ctx;
    let vm_ref = run.vm_ref();
    if !run.vm.cbt {
        // 바꾸기 전에 적는다(취소·마무리가 끈다).
        update_item(ctx, run.item, |s| s.cbt_enabled_by_us = true).await?;
        run.session.set_change_tracking(&vm_ref, true).await?;
    }
    let item = get_item(ctx, run.item)?;
    // 앞 시도가 새 스냅샷을 만들고 옛 것을 지우지 못했다: 옛 것을 먼저 지운다.
    if let Some(pending) = item.status.pending_snapshot.clone() {
        if let Some(old) = &item.status.snapshot {
            remove_ours(&run.session, old).await.map_err(|e| {
                SyncError::fatal(format!(
                    "cannot remove snapshot {}: {e}; the VM keeps two Orbivisor snapshots",
                    old.name
                ))
            })?;
        }
        update_item(ctx, run.item, |s| {
            s.snapshot = Some(pending.clone());
            s.pending_snapshot = None;
        })
        .await?;
    }
    let item = get_item(ctx, run.item)?;
    let n = item.status.rounds + 1;
    let expected = snapshot_name(run.item, n);
    let in_progress = item
        .status
        .snapshot
        .as_ref()
        .is_some_and(|s| s.name == expected);
    if !in_progress {
        // 새 회차: 체크포인트를 지우고(전체면 changeId도) 스냅샷을 만든다.
        update_item(ctx, run.item, |s| {
            for d in &mut s.disks {
                d.copied_bytes = 0;
                if full {
                    d.change_id = None;
                }
            }
        })
        .await?;
        let made = match run.vm.snapshots.iter().find(|s| s.name == expected) {
            // 앞 시도가 만들고 적지 못한 것
            Some(s) => MoRef::new("VirtualMachineSnapshot", &s.moref),
            None => {
                let description = format!(
                    "Orbivisor migration {} (round {n}); removed by the next round",
                    run.migration.name
                );
                run.session
                    .create_snapshot(&vm_ref, &expected, &description)
                    .await?
            }
        };
        let new = VmwareSnapshotRef {
            moref: made.value.clone(),
            name: expected.clone(),
            created_at: now(),
        };
        match item.status.snapshot.clone() {
            Some(old) => {
                update_item(ctx, run.item, |s| s.pending_snapshot = Some(new.clone())).await?;
                remove_ours(&run.session, &old).await.map_err(|e| {
                    SyncError::fatal(format!(
                        "cannot remove snapshot {}: {e}; the VM keeps two Orbivisor snapshots",
                        old.name
                    ))
                })?;
                update_item(ctx, run.item, |s| {
                    s.snapshot = Some(new.clone());
                    s.pending_snapshot = None;
                })
                .await?;
            }
            None => {
                update_item(ctx, run.item, |s| s.snapshot = Some(new.clone())).await?;
            }
        }
    }
    let item = get_item(ctx, run.item)?;
    let snap = item
        .status
        .snapshot
        .clone()
        .ok_or_else(|| SyncError::retry("the round has no snapshot"))?;
    // 기반 파일 = S(n+1)은 그 사이에 다른 스냅샷이 없을 때만이다: 검사 뒤 생긴 사용자 스냅샷(백업 제품 등)이 있으면 기반은
    // 그 시점이라 바뀐 구역을 옛 내용으로 읽는다(리뷰 1). 읽기 전에 스냅샷 나무를 다시 보고 우리 것만 있어야 한다.
    let now_vm = inventory::refresh_vm(&run.session, &run.inv, &run.vm.moref).await?;
    let foreign: Vec<&str> = now_vm
        .snapshots
        .iter()
        .filter(|s| s.moref != snap.moref)
        .map(|s| s.name.as_str())
        .collect();
    if !foreign.is_empty() {
        return Err(SyncError {
            message: format!("vmware_snapshots: {}", foreign_message(&foreign)),
            permanent: false,
            restricted: false,
            foreign: true,
        });
    }
    let snap_ref = MoRef::new("VirtualMachineSnapshot", &snap.moref);
    let snap_disks = run.session.snapshot_disks(&snap_ref).await?;
    let ids: BTreeMap<i32, Option<String>> = snap_disks
        .into_iter()
        .map(|d| (d.key, d.change_id))
        .collect();
    let mut copied = 0;
    for (i, target) in targets.iter().enumerate() {
        let disk = get_item(ctx, run.item)?.status.disks[i].clone();
        if disk.rounds >= n {
            continue;
        }
        let new_id = ids.get(&disk.key).cloned().flatten().ok_or_else(|| {
            SyncError::retry(format!(
                "snapshot {} has no change ID for disk {i}; changed block tracking is not active yet",
                snap.name
            ))
        })?;
        let since = disk.change_id.clone();
        let mut whole = since.is_none();
        let areas = match cbt::changed_areas(
            &run.session,
            &vm_ref,
            &snap_ref,
            disk.key,
            disk.capacity_bytes,
            since.as_deref().unwrap_or(ALL_AREAS),
        )
        .await
        {
            Ok(a) => a,
            // 옛 changeId가 무효다(CBT 초기화·Storage vMotion) — 그 디스크만 전체로(§9.2).
            Err(VsphereError::FileFault { .. }) if since.is_some() => {
                run.warn(format!(
                    "changed block tracking was reset on disk {i}; copying it again in full"
                ));
                update_item(ctx, run.item, |s| {
                    if let Some(d) = s.disks.get_mut(i) {
                        d.change_id = None;
                        d.copied_bytes = 0;
                    }
                })
                .await?;
                whole = true;
                cbt::changed_areas(
                    &run.session,
                    &vm_ref,
                    &snap_ref,
                    disk.key,
                    disk.capacity_bytes,
                    ALL_AREAS,
                )
                .await?
            }
            Err(e) => return Err(e.into()),
        };
        let disk = get_item(ctx, run.item)?.status.disks[i].clone();
        // `"*"`은 쓰인 구역만 준다: 앞 회차의 내용이 있는 볼륨이면 그 밖(원본이 그새 비운 곳)을 0으로 쓴다(리뷰 1).
        let zero_gaps = whole && !target.fresh;
        copied += Box::pin(copy_disk(
            run,
            i,
            &disk,
            target,
            &areas,
            Some(&snap_ref),
            zero_gaps,
        ))
        .await?;
        let total = cbt::total(&areas);
        update_item(ctx, run.item, |s| {
            if let Some(d) = s.disks.get_mut(i) {
                d.change_id = Some(new_id.clone());
                d.copied_bytes = 0;
                d.rounds = n;
                d.last_delta_bytes = total;
            }
        })
        .await?;
    }
    Ok(copied)
}

/// 차가운 회차(§9.2): 꺼진 VM의 기반 전체(0 조각은 `WRITE_ZEROES`). 디스크마다 시작 때 flat 파일의 수정 시각을 적고, 끝에
/// 원본이 아직 꺼져 있는지 본다.
async fn cold_round(run: &Run<'_>, targets: &[Target]) -> Result<u64, SyncError> {
    let ctx = run.ctx;
    let n = get_item(ctx, run.item)?.status.rounds + 1;
    let mut copied = 0;
    for (i, target) in targets.iter().enumerate() {
        let disk = get_item(ctx, run.item)?.status.disks[i].clone();
        if disk.rounds >= n {
            continue;
        }
        if disk.copied_bytes == 0 || disk.source_mtime.is_none() {
            let mtime = flat_mtime(run, &disk).await;
            update_item(ctx, run.item, |s| {
                if let Some(d) = s.disks.get_mut(i) {
                    d.source_mtime = mtime.clone();
                }
            })
            .await?;
        }
        let areas = [(0, disk.capacity_bytes)];
        copied += Box::pin(copy_disk(run, i, &disk, target, &areas, None, false)).await?;
        update_item(ctx, run.item, |s| {
            if let Some(d) = s.disks.get_mut(i) {
                d.copied_bytes = 0;
                d.rounds = n;
                d.last_delta_bytes = d.capacity_bytes;
            }
        })
        .await?;
    }
    if run.session.power_state(&run.vm_ref()).await? != opl_types::vmware::VmwarePower::PoweredOff {
        return Err(SyncError::fatal(
            "vmware_powered_on: the VM was powered on during the copy; power it off and retry",
        ));
    }
    Ok(copied)
}

/// 디스크 flat 파일의 수정 시각(`SearchDatastore_Task`). 알 수 없으면 경고만 남기고 없음이다.
async fn flat_mtime(run: &Run<'_>, disk: &VmwareSyncDisk) -> Option<String> {
    let found = async {
        let dc = run.vm.datacenter.clone().unwrap_or_default();
        let reader = FolderReader::open(run.session.clone(), &dc, &disk.file, None).await?;
        let flat = reader.flat_path();
        let (ds, path) = crate::vmware::client::split_datastore_path(&flat)
            .ok_or_else(|| VsphereError::Invalid(format!("invalid datastore path {flat:?}")))?;
        let browser = run
            .inv
            .datastore_named(ds)
            .and_then(|d| d.browser.clone())
            .ok_or_else(|| VsphereError::Invalid(format!("datastore {ds} has no browser")))?;
        let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
        let files = run
            .session
            .search_datastore(
                &MoRef::new("HostDatastoreBrowser", &browser),
                &format!("[{ds}] {dir}"),
                file,
            )
            .await?;
        Ok::<_, VsphereError>(
            files
                .into_iter()
                .find(|f| f.path == file)
                .and_then(|f| f.modification),
        )
    }
    .await;
    match found {
        Ok(Some(m)) => Some(m),
        Ok(None) => {
            run.warn(format!(
                "cannot read the modification time of {}",
                disk.file
            ));
            None
        }
        Err(e) => {
            run.warn(format!(
                "cannot read the modification time of {}: {e}",
                disk.file
            ));
            None
        }
    }
}

/// 디스크를 읽는 길: `/folder` 범위 읽개 `streams`개(vCenter면 VM의 호스트로 바로), VDDK 길이면 nbdkit 하나(스냅샷 시점).
async fn open_readers(
    run: &Run<'_>,
    disk: &VmwareSyncDisk,
    snapshot: Option<&MoRef>,
) -> Result<Vec<Box<dyn DiskReader>>, SyncError> {
    let streams = usize::from(run.migration.spec.limits.streams_per_disk.max(1));
    match &run.source.spec.transport {
        VmwareTransport::Folder => {
            let dc = run
                .vm
                .datacenter
                .clone()
                .ok_or_else(|| SyncError::fatal("the VM has no datacenter"))?;
            let route = (run.session.kind() == VmwareApiKind::Vcenter)
                .then(|| {
                    let host = run.inv.host(run.vm.host.as_deref()?)?;
                    Some(HostRoute {
                        name: host.name.clone(),
                        fingerprint: host.certificate_sha256.clone()?,
                    })
                })
                .flatten();
            let mut out: Vec<Box<dyn DiskReader>> = Vec::with_capacity(streams);
            for _ in 0..streams {
                out.push(Box::new(
                    FolderReader::open(run.session.clone(), &dc, &disk.file, route.clone()).await?,
                ));
            }
            Ok(out)
        }
        VmwareTransport::Vddk(v) => {
            let node = run.ctx.config.api.as_ref().map(|a| a.vmware.clone());
            let node = node.unwrap_or_default();
            let nbdkit = node.nbdkit.clone().ok_or_else(|| {
                SyncError::fatal("this node has no nbdkit for the VDDK transport ([vmware] nbdkit)")
            })?;
            let libdir = node.libdir_for(v.libdir.as_deref()).ok_or_else(|| {
                SyncError::fatal("this node has no VDDK library ([vmware] vddk_libdir)")
            })?;
            let params = VddkParams {
                libdir,
                server: run.source.spec.host.clone(),
                port: run.source.spec.port,
                user: run.source.spec.user.clone(),
                password: run.source.spec.password.clone(),
                thumbprint: v.thumbprint_sha1.clone(),
                vm: run.vm.moref.clone(),
                snapshot: snapshot.map(|s| s.value.clone()),
                file: disk.file.clone(),
                transports: None,
            };
            let reader =
                NbdkitReader::open_vddk(&nbdkit, &run.ctx.config.backup_run_dir, &params).await?;
            Ok(vec![Box::new(reader)])
        }
    }
}

/// 구역들을 원본에서 읽어 대상 볼륨에 쓴다(체크포인트 `disk.copied_bytes`부터 — 구역 목록 안 위치). `zero_gaps`면 먼저
/// (디스크를 처음부터 쓸 때) 구역 밖을 `WRITE_ZEROES`로 비운다. 복사한 바이트를 돌려준다.
async fn copy_disk(
    run: &Run<'_>,
    i: usize,
    disk: &VmwareSyncDisk,
    target: &Target,
    areas: &[(u64, u64)],
    snapshot: Option<&MoRef>,
    zero_gaps: bool,
) -> Result<u64, SyncError> {
    let ctx = run.ctx;
    let total = cbt::total(areas);
    let zero_gaps = zero_gaps && disk.copied_bytes == 0;
    if total == 0 && !zero_gaps {
        return Ok(0);
    }
    let (volume, pool) = (&target.volume, &target.pool);
    let src = ctx
        .storage
        .volume_source(pool, volume.id, volume.spec.format, None)
        .await
        .map_err(|e| SyncError::retry(format!("cannot open volume {:?}: {e}", volume.name)))?;
    let socket = socket_path(ctx, &format!("vmw{i}"), run.item);
    let result = async {
        let nbd = ctx
            .storage
            .open_target(&ctx.config.qemu_nbd, &src, &socket)
            .await
            .map_err(|e| SyncError::retry(format!("cannot write volume {:?}: {e}", volume.name)))?;
        let connected = NbdClient::connect(&nbd.socket, &nbd.export, &[]).await;
        let mut client = match connected {
            Ok(c) => c,
            Err(e) => {
                nbd.close().await;
                return Err(SyncError::retry(format!(
                    "cannot write volume {:?}: {e}",
                    volume.name
                )));
            }
        };
        let pumped = if client.size() < disk.capacity_bytes {
            Err(SyncError::retry(format!(
                "volume {:?} is smaller than the source disk",
                volume.name
            )))
        } else if zero_gaps
            && let Err(e) = write_gaps(&mut client, areas, disk.capacity_bytes).await
        {
            Err(SyncError::retry(format!("cannot write disk {i}: {e}")))
        } else {
            match open_readers(run, disk, snapshot).await {
                Ok(readers) => Box::pin(pump(run, i, disk, &mut client, readers, areas)).await,
                Err(e) => Err(e),
            }
        };
        let flushed = client.flush().await;
        let _ = client.disconnect().await;
        nbd.close().await;
        let copied = pumped?;
        flushed.map_err(|e| SyncError::retry(format!("cannot flush {:?}: {e}", volume.name)))?;
        Ok(copied)
    }
    .await;
    if let Err(e) = ctx
        .storage
        .release_source(pool, volume.id, volume.spec.format, None)
        .await
    {
        tracing::warn!(volume = %volume.name, error = %e, "cannot release the target volume");
    }
    result
}

/// 구역 밖(`[0, capacity)`에서 `areas`가 덮지 않는 곳)을 0으로 쓴다(얇은 대상에서는 싸다).
async fn write_gaps(
    client: &mut NbdClient,
    areas: &[(u64, u64)],
    capacity: u64,
) -> Result<(), opl_store::nbd::NbdError> {
    let mut sorted = areas.to_vec();
    sorted.sort_unstable();
    let mut at = 0u64;
    let mut gaps = Vec::new();
    for (offset, len) in sorted {
        if offset > at {
            gaps.push((at, offset - at));
        }
        at = at.max(offset + len);
    }
    if capacity > at {
        gaps.push((at, capacity - at));
    }
    for (offset, len) in gaps {
        client.write_zeroes(offset, len).await?;
    }
    Ok(())
}

/// 읽개 묶음으로 나란히 읽고 차례대로 쓴다. 체크포인트(바이트·시간)마다 `FLUSH`한 뒤 Raft에 위치를 적고 Task 진행률을
/// 쓴다. 대역 한도가 있으면 이 Task의 몫(한도 ÷ 동시 수)을 넘지 않게 쉰다.
async fn pump(
    run: &Run<'_>,
    i: usize,
    disk: &VmwareSyncDisk,
    client: &mut NbdClient,
    readers: Vec<Box<dyn DiskReader>>,
    areas: &[(u64, u64)],
) -> Result<u64, SyncError> {
    let ctx = run.ctx;
    let stall = ctx.config.vmware_stall;
    let every_bytes = ctx.config.vmware_checkpoint_bytes.max(1);
    // 가까운 구역은 이어 읽는다(체크포인트 위치는 이 이은 목록 안의 위치 — 늘 같은 목록이 나온다).
    let areas = &cbt::coalesce(areas, COALESCE_GAP);
    let total = cbt::total(areas);
    let start = disk.copied_bytes.min(total);
    let streams = readers.len().max(1);
    // 체크포인트는 늘 조각 경계에 있다(조각을 다 쓴 뒤에만 적는다).
    let mut at = 0u64;
    let todo: Vec<(u64, u32)> = cbt::chunks(areas, CHUNK)
        .into_iter()
        .filter(|(_, len)| {
            let keep = at >= start;
            at += u64::from(*len);
            keep
        })
        .collect();
    let rate = run.migration.spec.limits.bandwidth_mib_s.map(|mib| {
        let share =
            u64::from(mib) * (1 << 20) / u64::from(run.migration.spec.limits.max_parallel.max(1));
        share.max(1) as f64
    });
    let pool: Arc<Mutex<Vec<Box<dyn DiskReader>>>> = Arc::new(Mutex::new(readers));
    let mut reads = futures_util::stream::iter(todo)
        .map(|(offset, len)| {
            let pool = pool.clone();
            async move {
                let mut reader = pool
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pop()
                    .expect("a reader per stream");
                let read = tokio::time::timeout(stall, reader.read(offset, len)).await;
                pool.lock().unwrap_or_else(|e| e.into_inner()).push(reader);
                match read {
                    Ok(Ok(data)) if data.len() == len as usize => Ok((offset, data)),
                    Ok(Ok(data)) => Err(SyncError::retry(format!(
                        "short read of disk {i} at {offset}: {} of {len} bytes",
                        data.len()
                    ))),
                    Ok(Err(e)) => Err(SyncError::from(e)),
                    Err(_) => Err(SyncError::retry(format!(
                        "no progress reading disk {i} for {}s",
                        stall.as_secs()
                    ))),
                }
            }
        })
        .buffered(streams);
    let began = Instant::now();
    let mut done = 0u64;
    let mut saved = 0u64;
    let mut saved_at = Instant::now();
    let stage = format!("copying disk {i}");
    while let Some(read) = reads.next().await {
        let (offset, data) = read?;
        let len = data.len() as u64;
        let write = async {
            if cbt::is_zero(&data) {
                client.write_zeroes(offset, len).await
            } else {
                client.write(offset, &data).await
            }
        };
        match tokio::time::timeout(stall, write).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(SyncError::retry(format!("cannot write disk {i}: {e}"))),
            Err(_) => {
                return Err(SyncError::retry(format!(
                    "no progress writing disk {i} for {}s",
                    stall.as_secs()
                )));
            }
        }
        done += len;
        if let Some(rate) = rate {
            let due = Duration::from_secs_f64(done as f64 / rate);
            let spent = began.elapsed();
            if due > spent {
                tokio::time::sleep(due - spent).await;
            }
        }
        if done - saved >= every_bytes || saved_at.elapsed() >= CHECKPOINT_EVERY {
            client
                .flush()
                .await
                .map_err(|e| SyncError::retry(format!("cannot flush disk {i}: {e}")))?;
            let position = start + done;
            update_item(ctx, run.item, |s| {
                if let Some(d) = s.disks.get_mut(i) {
                    d.copied_bytes = position;
                }
            })
            .await?;
            write_progress(
                ctx,
                run.task.id,
                &stage,
                MirrorProgress {
                    done_bytes: position,
                    total_bytes: total,
                },
            )
            .await;
            saved = done;
            saved_at = Instant::now();
        }
    }
    drop(reads);
    let readers = std::mem::take(&mut *pool.lock().unwrap_or_else(|e| e.into_inner()));
    for r in &readers {
        for w in r.warnings() {
            run.warn(w);
        }
    }
    Ok(done)
}

// ---- vmware-cleanup ----

/// `vmware-cleanup` Task(설계 §10.3): 원본의 우리 스냅샷(Raft에 적힌 moref만)을 지우고, 우리가 켠 CBT를 끄고, 바꾼 원본
/// 설정(NIC `startConnected`·설명)을 되돌린다. `delete_target`(취소)이면 대상 볼륨도 지우고 `cancelled`, 아니면(마무리)
/// `finalized`다. 원본 VM의 전원·디스크·사용자 스냅샷은 그대로다. 취소가 실패하면 항목을 `failed`로 두고 이유를 적는다(다시
/// 취소하면 남은 것부터 한다 — 실패 수는 `cleanup_failures`). 마무리가 실패하면 `done`에 오류만 남긴다(다시 마무리한다).
/// 취소는 전환한 항목(`done`·`cutting-over`, 또는 되돌림 실패가 아닌데 대상 VM이 있음)을 건드리지 않는다(리뷰 1).
pub(super) async fn cleanup(
    ctx: &Ctx,
    task: &Task,
    delete_target: bool,
) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let item = get_item(ctx, id)?;
    if item.status.state.is_final() {
        return Ok(None);
    }
    let s = &item.status;
    let cut_over = matches!(s.state, VmwareVmState::Done | VmwareVmState::CuttingOver)
        || (s.target_vm.is_some() && s.cutover_step.is_none());
    if delete_target && cut_over {
        return Ok(Some(format!(
            "skipped: vmware-vm {:?} is {} (cut over VMs are finalized or reverted, not cancelled)",
            item.name,
            s.state.as_str()
        )));
    }
    if !delete_target && s.state != VmwareVmState::Done {
        return Ok(Some(format!(
            "skipped: vmware-vm {:?} is {}",
            item.name,
            s.state.as_str()
        )));
    }
    match Box::pin(run_cleanup(ctx, &item, delete_target)).await {
        Ok(()) => Ok(None),
        Err(e) => {
            let at = now();
            let message = format!("cleanup: {e}");
            match update_item(ctx, id, |s| {
                s.error = Some(message.clone());
                if delete_target {
                    s.cleanup_failures += 1;
                    s.state = VmwareVmState::Failed;
                    s.push_history(at, "cleanup failed");
                } else {
                    // 전환한 VM은 그대로 돈다: `done`에 남겨 다시 마무리하게 한다.
                    s.push_history(at, "finalize failed");
                }
            })
            .await
            {
                Ok(item) => recount(ctx, item.spec.migration).await,
                Err(err) => {
                    tracing::warn!(item = %id, error = %err, "cannot record a failed VMware cleanup")
                }
            }
            Err(message)
        }
    }
}

async fn run_cleanup(ctx: &Ctx, item: &VmwareVm, delete_target: bool) -> Result<(), String> {
    let id = item.id;
    let s = &item.status;
    let on_source = s.snapshot.is_some()
        || s.pending_snapshot.is_some()
        || s.cbt_enabled_by_us
        || !s.source_changes.is_empty();
    if on_source {
        let migration = get_migration(ctx, item.spec.migration).map_err(|e| e.message)?;
        let source = get_source(ctx, &migration).map_err(|e| e.message)?;
        let session = session_for(ctx, &source).await.map_err(|e| e.to_string())?;
        let vm_ref = MoRef::new("VirtualMachine", &item.spec.source_ref);
        for snap in [&s.pending_snapshot, &s.snapshot].into_iter().flatten() {
            remove_ours(&session, snap)
                .await
                .map_err(|e| format!("cannot remove snapshot {}: {e}", snap.name))?;
            let moref = snap.moref.clone();
            update_item(ctx, id, |s| {
                if s.pending_snapshot
                    .as_ref()
                    .is_some_and(|p| p.moref == moref)
                {
                    s.pending_snapshot = None;
                }
                if s.snapshot.as_ref().is_some_and(|p| p.moref == moref) {
                    s.snapshot = None;
                }
            })
            .await?;
        }
        // 취소는 바꾼 원본 설정(NIC·설명·우리가 끈 전원)을 되돌리고, 마무리는 원본 보호를 그대로 둔다(실수로 켜도 같은
        // MAC·IP가 붙지 않게 — §10.3).
        if delete_target {
            cutover::remove_target_vm(ctx, id).await?;
            cutover::restore_source(ctx, &session, &vm_ref, id).await?;
        }
        if s.cbt_enabled_by_us {
            match session.set_change_tracking(&vm_ref, false).await {
                Ok(()) | Err(VsphereError::NotFound { .. }) => {}
                Err(e) => return Err(format!("cannot turn changed block tracking off: {e}")),
            }
            update_item(ctx, id, |s| s.cbt_enabled_by_us = false).await?;
        }
    }
    if delete_target {
        // 되돌림이 실패한 항목은 대상 VM이 남아 있을 수 있다(볼륨을 쓰므로 먼저 지운다).
        cutover::remove_target_vm(ctx, id).await?;
        for (i, disk) in s.disks.iter().enumerate() {
            let Some(volume) = disk.volume else { continue };
            // 다른 노드의 로컬 풀에 있는 볼륨은 여기서 지울 수 없다: 기록을 남겨 그 노드가 시작할 때 지우게 한다(항목이
            // 끝나면 그 노드의 고아 정리가 지운다 — 기록을 지우면 파일이 남는다).
            if let Some(owner) = elsewhere(ctx, volume)? {
                let at = now();
                update_item(ctx, id, |s| {
                    s.push_history(
                        at,
                        format!(
                            "the target volume of disk {i} is on node {owner}'s local pool; that node removes it"
                        ),
                    );
                })
                .await?;
                continue;
            }
            super::remove_volume(ctx, volume)
                .await
                .map_err(|e| format!("cannot delete the target volume of disk {i}: {e}"))?;
            update_item(ctx, id, |s| {
                if let Some(d) = s.disks.get_mut(i) {
                    d.volume = None;
                }
            })
            .await?;
        }
    }
    let at = now();
    let item = update_item(ctx, id, |s| {
        s.state = if delete_target {
            VmwareVmState::Cancelled
        } else {
            VmwareVmState::Finalized
        };
        s.error = None;
        s.sync_failures = 0;
        s.cleanup_failures = 0;
        s.next_sync_at = None;
        s.push_history(
            at,
            if delete_target {
                "cancelled; Orbivisor snapshots and target volumes removed"
            } else {
                "finalized; Orbivisor snapshots removed"
            },
        );
    })
    .await?;
    recount(ctx, item.spec.migration).await;
    Ok(())
}

/// 볼륨이 다른 노드의 로컬(공유 아닌) 풀에 있으면 그 노드 이름(없으면 id)
fn elsewhere(ctx: &Ctx, volume: Uuid) -> Result<Option<String>, String> {
    let Some(v) = ctx
        .cluster
        .get::<VolumeSpec>(volume)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let Some(node) = v.status.node.filter(|n| *n != ctx.config.node) else {
        return Ok(None);
    };
    let shared = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&v.spec.pool)
        .map_err(|e| e.to_string())?
        .is_some_and(|p| opl_types::storage::is_shared(&p));
    if shared {
        return Ok(None);
    }
    let name = ctx
        .cluster
        .get::<opl_types::node::NodeSpec>(node)
        .ok()
        .flatten()
        .map_or_else(|| node.to_string(), |n| n.name);
    Ok(Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_well() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(3 << 20), "3.0 MiB");
        assert_eq!(size_text(5 << 30), "5.0 GiB");
    }

    #[test]
    fn vsphere_errors_split_into_retry_and_permanent() {
        let e = SyncError::from(VsphereError::Restricted("free".into()));
        assert!(e.permanent && e.restricted);
        let e = SyncError::from(VsphereError::Connect("refused".into()));
        assert!(!e.permanent && !e.restricted);
        let e = SyncError::from(VsphereError::NoPermission {
            privilege: Some("VirtualMachine.State.CreateSnapshot".into()),
            message: "no".into(),
        });
        assert!(e.permanent);
    }
}
