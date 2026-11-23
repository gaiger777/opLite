//! 백업 저장소 Task(계획 68 설계 §10·§12.6): 초기화(`BackupStoreInit`), GC(`BackupStoreGc` — 표시·기다림·쓸기 잠금·
//! 늦은 표시·쓸기), 검증(`BackupStoreVerify`), 읽기 전용 저장소의 가져오기(`BackupStoreScan`). 대상은 저장소 리소스이고
//! 저장소의 줄에 서서 같은 저장소의 GC·검증은 차례로 돈다(백업과는 나란히).

use std::time::Instant;

use opl_store::format::OwnerFile;
use opl_store::format::manifest::{CONFIG_BLOB, ConfigBlob};
use opl_store::gc::{self, Marker};
use opl_store::store::OwnerClaim;
use opl_store::verify::{ExpectedDisk, Verifier};
use opl_store::{Store, StoreError};
use opl_types::backup::{
    Backup, BackupOrigin, BackupPhase, BackupSpec, BackupStatus, BackupTarget, BackupVerify,
    VerifyState, default_backup_name,
};
use opl_types::backup_job::next_run;
use opl_types::backup_store::{
    BackupStore, BackupStoreSpec, GcPhase, GcResult, GcRun, StoreOwner, StorePhase, StoreUsage,
    VerifyMode, VerifyResult, verify::due,
};
use opl_types::cluster::ClusterSpec;
use opl_types::resource::Resource;
use opl_types::storage::is_shared;
use opl_types::task::Task;
use uuid::Uuid;

use super::Ctx;
use super::backup_store::{Step, update_store};
use super::snapshot::now_secs;
use crate::stores;

fn get_store(ctx: &Ctx, id: Uuid) -> Result<BackupStore, String> {
    ctx.cluster
        .get::<BackupStoreSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "backup store was deleted".into())
}

/// 이 클러스터의 이름과 CA 지문(`owner.json`)
fn this_cluster(ctx: &Ctx) -> Result<(String, String), String> {
    let info = ctx
        .cluster
        .list::<ClusterSpec>()
        .map_err(|e| e.to_string())?
        .1
        .into_iter()
        .next()
        .ok_or("the cluster has not been initialized")?;
    let fp = opl_peer::pki::fingerprint(&info.spec.ca_cert_pem).map_err(|e| e.to_string())?;
    Ok((info.name, fp))
}

fn owner_file(ctx: &Ctx, store: &BackupStore, now: u64) -> Result<OwnerFile, String> {
    let (cluster, ca_fingerprint) = this_cluster(ctx)?;
    Ok(OwnerFile {
        cluster,
        ca_fingerprint,
        store_resource: store.id,
        heartbeat: now,
    })
}

// ---- 초기화 ----

/// 저장소를 만들거나(쓰기 저장소: `store.json`·주인 표식) 붙인다(읽기 전용: `store.json`을 읽어 키를 확인하고 주인을
/// 읽음). `ready`와 `store_id`·`owner`·다음 GC·검증 시각을 적는다. 공유가 아닌 dir 풀의 저장소는 이 노드에 묶는다.
/// 이미 준비된 저장소면 할 일이 없다. 다른 클러스터가 주인인 곳에 쓰기 저장소를 만들면 실패한다.
pub(super) async fn init(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let mut res = get_store(ctx, task.spec.target)?;
    if res.status.phase == StorePhase::Ready {
        return Ok(None);
    }
    let fail = |e: StoreError| stores::store_error(&res.name, e);
    // 공유가 아닌 풀의 저장소는 여기서 만든다 — 이 노드에 묶는다.
    let pool = stores::backend_pool(&ctx.cluster, &res)?;
    let node = pool
        .as_ref()
        .filter(|p| !is_shared(p))
        .map(|_| ctx.config.node);
    if node.is_some() {
        res.status.node = node;
    }
    let backend =
        stores::open_backend(&ctx.cluster, &res, ctx.config.node, &ctx.config.mount_root)?;
    let key = stores::master_key(&res)?;
    let now = now_secs();
    let (store, owner) = if res.spec.read_only {
        let store = Store::open(backend, key.as_ref()).await.map_err(fail)?;
        let owner = store.read_owner().await.map_err(fail)?;
        (store, owner)
    } else {
        let store = Store::init(backend, res.id, key.as_ref(), now)
            .await
            .map_err(fail)?;
        let me = owner_file(ctx, &res, now)?;
        let owner = match store.claim_owner(&me).await.map_err(fail)? {
            OwnerClaim::Claimed | OwnerClaim::Ours => me,
            OwnerClaim::Other(o) => {
                return Err(format!(
                    "the store location is owned by cluster {:?}; attach it read-only or take it over",
                    o.cluster
                ));
            }
        };
        (store, Some(owner))
    };
    let store_id = store.id();
    let (gc_at, verify_at) = (
        (!res.spec.read_only).then(|| next_run(&res.spec.gc, now)),
        res.spec.verify.as_ref().map(|v| next_run(&v.schedule, now)),
    );
    update_store(ctx, res.id, |s| {
        s.phase = StorePhase::Ready;
        s.store_id = Some(store_id);
        s.node = node.or(s.node);
        s.owner = owner.as_ref().map(|o| StoreOwner {
            cluster: o.cluster.clone(),
            ca_fingerprint: o.ca_fingerprint.clone(),
        });
        s.next_gc = s.next_gc.or(gc_at);
        s.next_verify = s.next_verify.or(verify_at);
        s.error = None;
        Ok(Step::Write)
    })
    .await?;
    Ok(None)
}

/// 실패한 초기화: 저장소를 `failed`와 그 까닭으로(이미 준비된 저장소는 건드리지 않는다)
pub(super) async fn init_failed(ctx: &Ctx, task: &Task, error: &str) {
    let r = update_store(ctx, task.spec.target, |s| {
        if s.phase == StorePhase::Ready {
            return Ok(Step::Skip);
        }
        s.phase = StorePhase::Failed;
        s.error = Some(error.to_string());
        Ok(Step::Write)
    })
    .await;
    if let Err(e) = r {
        tracing::warn!(store = %task.spec.target, error = %e, "cannot record the failed store init");
    }
}

/// 준비된 저장소와 이 노드에서 연 것. 쓰기가 필요하면(`write`) 읽기 전용 저장소를 거절한다.
async fn ready_store(ctx: &Ctx, id: Uuid, write: bool) -> Result<(BackupStore, Store), String> {
    let res = get_store(ctx, id)?;
    if res.status.phase != StorePhase::Ready {
        return Err(format!("backup store {:?} is not ready", res.name));
    }
    if write && res.spec.read_only {
        return Err(format!("backup store {:?} is read-only", res.name));
    }
    let store = stores::open(&ctx.cluster, &res, ctx.config.node, &ctx.config.mount_root).await?;
    Ok((res, store))
}

// ---- GC(§10.2) ----

/// 기다림 집합 W의 상태: 이 저장소에 아직 쓰는 중인 백업(만드는 중·받는 중)
fn writing(b: &Backup, store: &str) -> bool {
    b.spec.target() == Ok(BackupTarget::Store(store))
        && matches!(
            b.status.phase,
            BackupPhase::Pending | BackupPhase::Receiving
        )
}

/// 저장소 GC(§10.2): 표식과 T0 → Raft에 `gc`(L) → 기다림 집합 W → 모든 인덱스 표시 → W가 끝나기를 기다림(그새 생긴
/// 인덱스도 표시) → `committing`이 빌 때 쓸기 잠금(`sweeping`, `sweep_epoch` + 1) → 늦은 표시 → 쓸기 → 결과·사용량·주인
/// 하트비트. 주인이 이 클러스터가 아니면(넘겨받음) 저장소를 `failed`로 멈춘다.
pub(super) async fn gc(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let (res, store) = ready_store(ctx, task.spec.target, true).await?;
    let fail = |e: StoreError| stores::store_error(&res.name, e);
    let (cluster, fp) = this_cluster(ctx)?;
    if let Some(o) = store.read_owner().await.map_err(fail)?
        && (o.ca_fingerprint != fp || o.store_resource != res.id)
    {
        let error = format!("store was taken over by cluster {:?}", o.cluster);
        let _ = update_store(ctx, res.id, |s| {
            s.phase = StorePhase::Failed;
            s.error = Some(error.clone());
            Ok(Step::Write)
        })
        .await;
        return Err(error);
    }
    let start = gc::start(&store, task.id).await.map_err(fail)?;
    let started_at = now_secs();
    // L
    update_store(ctx, res.id, |s| match &s.gc {
        Some(g) if g.task == task.id => Ok(Step::Skip),
        Some(g) => Err(format!("another GC (task {}) is running", g.task)),
        None => {
            s.gc = Some(GcRun {
                task: task.id,
                started_at,
                marker_at: start.t0,
                phase: GcPhase::Mark,
            });
            Ok(Step::Write)
        }
    })
    .await?;
    // W: L이 이 노드에 적용된 뒤의 목록(쓰기는 적용을 기다린다)
    let (_, backups) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    let waiting: Vec<Uuid> = backups
        .iter()
        .filter(|b| writing(b, &res.name))
        .map(|b| b.id)
        .collect();
    let capacity = gc::capacity(&store, res.status.usage.map(|u| u.chunks))
        .await
        .map_err(fail)?;
    let mut marker = Marker::new(capacity, gc::seed_for(task.id));
    marker.mark(&store).await.map_err(fail)?;
    let set_phase = |phase: GcPhase| {
        move |s: &mut opl_types::backup_store::BackupStoreStatus| match &mut s.gc {
            Some(g) if g.task == task.id => {
                if g.phase == phase {
                    return Ok(Step::Skip);
                }
                g.phase = phase;
                Ok(Step::Write)
            }
            _ => Err("the GC record was cleared".to_string()),
        }
    };
    update_store(ctx, res.id, set_phase(GcPhase::Wait)).await?;
    let deadline = Instant::now() + ctx.config.gc_wait_max;
    loop {
        let (_, backups) = ctx
            .cluster
            .list::<BackupSpec>()
            .map_err(|e| e.to_string())?;
        let still: Vec<&Backup> = backups
            .iter()
            .filter(|b| waiting.contains(&b.id) && writing(b, &res.name))
            .collect();
        if still.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            let names: Vec<&str> = still.iter().map(|b| b.name.as_str()).collect();
            return Err(format!(
                "backups {} did not finish in time; nothing was removed",
                names.join(", ")
            ));
        }
        tokio::time::sleep(ctx.config.store_poll).await;
        marker.mark(&store).await.map_err(fail)?;
    }
    // 쓸기 잠금: 커밋 구간이 비었을 때만
    update_store(ctx, res.id, |s| {
        match &s.gc {
            Some(g) if g.task == task.id => {}
            _ => return Err("the GC record was cleared".into()),
        }
        if s.sweeping {
            return Ok(Step::Skip);
        }
        if !s.committing.is_empty() {
            return Ok(Step::Wait);
        }
        s.sweeping = true;
        s.sweep_epoch += 1;
        if let Some(g) = &mut s.gc {
            g.phase = GcPhase::Sweep;
        }
        Ok(Step::Write)
    })
    .await?;
    // 늦은 표시: 표시 뒤에 생긴 인덱스
    marker.mark(&store).await.map_err(fail)?;
    let stats = gc::sweep(&store, &marker, &start).await.map_err(fail)?;
    let finished_at = now_secs();
    let owner = OwnerFile {
        cluster,
        ca_fingerprint: fp,
        store_resource: res.id,
        heartbeat: finished_at,
    };
    if let Err(e) = store.write_owner(&owner).await {
        tracing::warn!(store = %res.name, error = %e, "cannot write the owner heartbeat");
    }
    if let Err(e) = gc::finish(&store, &start).await {
        tracing::warn!(store = %res.name, error = %e, "cannot remove the GC marker");
    }
    let usage = StoreUsage {
        snapshots: marker.snapshots,
        chunks: stats.chunks,
        stored_bytes: stats.stored_bytes,
        logical_bytes: marker.logical_bytes,
        measured_at: finished_at,
    };
    let result = GcResult {
        started_at,
        finished_at,
        removed_chunks: stats.removed_chunks,
        removed_bytes: stats.removed_bytes,
        kept_young: stats.kept_young,
        error: None,
    };
    update_store(ctx, res.id, |s| {
        if s.gc.as_ref().is_some_and(|g| g.task == task.id) {
            s.gc = None;
            s.sweeping = false;
        }
        s.last_gc = Some(result.clone());
        s.usage = Some(usage);
        Ok(Step::Write)
    })
    .await?;
    tracing::info!(store = %res.name, removed = stats.removed_chunks, bytes = stats.removed_bytes, "backup store GC finished");
    Ok(None)
}

/// 실패한 GC: 이 Task의 `gc`·쓸기 잠금을 풀고 `last_gc`에 오류를 남긴다.
pub(super) async fn gc_failed(ctx: &Ctx, task: &Task, error: &str) {
    let now = now_secs();
    let r = update_store(ctx, task.spec.target, |s| {
        let Some(g) = s.gc.clone().filter(|g| g.task == task.id) else {
            return Ok(Step::Skip);
        };
        s.gc = None;
        s.sweeping = false;
        s.last_gc = Some(GcResult {
            started_at: g.started_at,
            finished_at: now,
            removed_chunks: 0,
            removed_bytes: 0,
            kept_young: 0,
            error: Some(error.to_string()),
        });
        Ok(Step::Write)
    })
    .await;
    if let Err(e) = r {
        tracing::warn!(store = %task.spec.target, error = %e, "cannot release the GC lock of a failed GC");
    }
}

// ---- 검증(§10.5) ----

/// 백업 상태의 `verify`를 읽은 revision에서만 바꾼다.
async fn record_verify(ctx: &Ctx, id: Uuid, verify: &BackupVerify) -> Result<(), String> {
    loop {
        let Some(b) = ctx
            .cluster
            .get::<BackupSpec>(id)
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let status = BackupStatus {
            verify: Some(verify.clone()),
            ..b.status
        };
        match ctx
            .cluster
            .update_status_checked::<BackupSpec>(id, b.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(opl_cluster::ClusterError::Rejected(
                opl_cluster::types::Rejection::RevisionMismatch { .. },
            )) => {}
            Err(opl_cluster::ClusterError::Rejected(opl_cluster::types::Rejection::NotFound {
                ..
            })) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 저장소 검증(§10.5): `backups`(비면 일정대로 [`due`])의 매니페스트·블롭·인덱스·청크를 확인하고 백업마다
/// `status.verify`, 저장소에 `last_verify`·`bad_chunks`를 적는다. 깨진 청크는 `bad/`로 옮긴다(읽기 전용이면 옮기지
/// 않음). 실패한 백업이 있으면 경고로 돌려준다.
pub(super) async fn verify(
    ctx: &Ctx,
    task: &Task,
    backups: &[Uuid],
) -> Result<Option<String>, String> {
    let (res, store) = ready_store(ctx, task.spec.target, false).await?;
    let started_at = now_secs();
    let mode = res
        .spec
        .verify
        .as_ref()
        .map_or(VerifyMode::Full, |p| p.mode);
    let (_, all) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    let ids = if backups.is_empty() {
        res.spec
            .verify
            .as_ref()
            .map(|p| due(&all, &res.name, p, started_at))
            .unwrap_or_default()
    } else {
        backups.to_vec()
    };
    let mut verifier = Verifier::new(&store, mode, !res.spec.read_only, started_at);
    let (mut snapshots, mut failed) = (0u64, Vec::new());
    for id in ids {
        let Some(b) = all.iter().find(|b| b.id == id) else {
            continue;
        };
        if b.spec.target() != Ok(BackupTarget::Store(&res.name))
            || b.status.phase != BackupPhase::Ready
        {
            continue;
        }
        let expected: Vec<ExpectedDisk> = b
            .status
            .disks
            .iter()
            .map(|d| ExpectedDisk {
                name: d.name.clone(),
                virtual_bytes: d.virtual_bytes,
            })
            .collect();
        let report = verifier
            .verify_snapshot(b.spec.vm, b.id, Some(&expected))
            .await
            .map_err(|e| stores::store_error(&res.name, e))?;
        snapshots += 1;
        let verify = BackupVerify {
            at: now_secs(),
            state: if report.ok() {
                VerifyState::Ok
            } else {
                VerifyState::Failed
            },
            bad_chunks: report.bad_chunks.len() as u64,
            error: report.error.clone(),
        };
        if !report.ok() {
            failed.push(b.name.clone());
        }
        record_verify(ctx, b.id, &verify).await?;
    }
    let result = VerifyResult {
        started_at,
        finished_at: now_secs(),
        snapshots,
        chunks: verifier.chunks_checked(),
        bad: verifier.bad_chunks().len() as u64,
        error: None,
    };
    let moved = verifier.moved;
    update_store(ctx, res.id, |s| {
        s.last_verify = Some(result.clone());
        s.bad_chunks += moved;
        Ok(Step::Write)
    })
    .await?;
    Ok((!failed.is_empty()).then(|| {
        format!(
            "{} backup(s) failed verification: {}",
            failed.len(),
            failed.join(", ")
        )
    }))
}

/// 실패한 검증(저장소에 닿지 못함 따위): 저장소 `last_verify`에 오류를 남긴다.
pub(super) async fn verify_failed(ctx: &Ctx, task: &Task, error: &str) {
    let now = now_secs();
    let started_at = task.id.get_timestamp().map_or(now, |t| t.to_unix().0);
    let r = update_store(ctx, task.spec.target, |s| {
        s.last_verify = Some(VerifyResult {
            started_at,
            finished_at: now,
            snapshots: 0,
            chunks: 0,
            bad: 0,
            error: Some(error.to_string()),
        });
        Ok(Step::Write)
    })
    .await;
    if let Err(e) = r {
        tracing::warn!(store = %task.spec.target, error = %e, "cannot record the failed verification");
    }
}

// ---- scan(§12.6) ----

/// 읽기 전용으로 붙인 저장소의 매니페스트마다 `ready` Backup을 만든다(리소스 id = 저장소의 백업 id, `origin` = 주인
/// 클러스터). 다시 하면 새것만 더하고, 저장소에서 사라진 백업은 `failed`("snapshot is gone from the store")로 둔다.
/// 읽지 못한 스냅샷은 건너뛰고 경고로 알린다.
pub(super) async fn scan(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let (res, store) = ready_store(ctx, task.spec.target, false).await?;
    if !res.spec.read_only {
        return Err(format!(
            "backup store {:?} belongs to this cluster; scan reads stores attached read-only",
            res.name
        ));
    }
    let fail = |e: StoreError| stores::store_error(&res.name, e);
    let owner = match store.read_owner().await.map_err(fail)? {
        Some(o) => StoreOwner {
            cluster: o.cluster,
            ca_fingerprint: o.ca_fingerprint,
        },
        None => res
            .status
            .owner
            .clone()
            .ok_or("the store has no owner.json")?,
    };
    let snaps = store.list_snapshots().await.map_err(fail)?;
    let (_, mut all) = ctx
        .cluster
        .list::<BackupSpec>()
        .map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    for s in snaps.iter().filter(|s| s.complete) {
        if all.iter().any(|b| b.id == s.backup) {
            continue;
        }
        let read = async {
            let m = store.read_manifest(s.vm, s.backup).await?;
            let raw = store.get_blob(&m, CONFIG_BLOB).await?;
            let config: ConfigBlob = serde_json::from_slice(&raw)
                .map_err(|e| StoreError::Corrupt(format!("config blob: {e}")))?;
            Ok::<_, StoreError>((m, config))
        };
        let (m, config) = match read.await {
            Ok(r) => r,
            Err(e @ (StoreError::Unavailable(_) | StoreError::Denied(_))) => return Err(fail(e)),
            Err(e) => {
                warnings.push(format!("snapshot {}: {e}", s.backup));
                continue;
            }
        };
        let base = default_backup_name(&config.vm_name, m.created_at);
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{}-{n}", &base[..base.len().min(60)])
                }
            })
            .find(|n| !all.iter().any(|b| &b.name == n))
            .expect("a free name");
        let mut vm_spec = config.vm_spec.clone();
        // 다른 클러스터의 폴더는 여기에 없다(받은 사본과 같다 — 루트).
        vm_spec.folder = None;
        let mut b: Backup = Resource::new(
            name,
            BackupSpec {
                vm: s.vm,
                vm_name: config.vm_name.clone(),
                pool: String::new(),
                store: Some(res.name.clone()),
                source: None,
                vm_spec,
                job: None,
                origin: Some(BackupOrigin {
                    cluster: owner.cluster.clone(),
                    ca_fingerprint: owner.ca_fingerprint.clone(),
                    backup: s.backup,
                    job: None,
                }),
                incremental: false,
                max_chain: None,
            },
        );
        b.id = s.backup;
        b.status = BackupStatus {
            phase: BackupPhase::Ready,
            created_at: Some(m.created_at),
            disks: m
                .disks
                .iter()
                .map(|d| opl_types::backup::BackupDisk {
                    name: d.name.clone(),
                    file: d.index.clone(),
                    virtual_bytes: d.virtual_bytes,
                    stored_bytes: 0,
                    sha256: None,
                })
                .collect(),
            uefi_vars: config.uefi_vars,
            tpm_state: config.tpm_state,
            ..Default::default()
        };
        match ctx.cluster.create(&b).await {
            Ok(_) => all.push(b),
            Err(e) => warnings.push(format!("snapshot {}: {e}", s.backup)),
        }
    }
    for b in all.iter().filter(|b| {
        b.spec.target() == Ok(BackupTarget::Store(&res.name))
            && b.status.phase == BackupPhase::Ready
            && !snaps.iter().any(|s| s.complete && s.backup == b.id)
    }) {
        let status = BackupStatus {
            phase: BackupPhase::Failed,
            error: Some("snapshot is gone from the store".into()),
            ..b.status.clone()
        };
        if let Err(e) = ctx
            .cluster
            .update_status_checked::<BackupSpec>(b.id, b.revision, &status)
            .await
        {
            warnings.push(format!("backup {:?}: {e}", b.name));
        }
    }
    Ok((!warnings.is_empty()).then(|| warnings.join("; ")))
}
