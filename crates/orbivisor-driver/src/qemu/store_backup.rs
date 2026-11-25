//! 저장소 백업의 읽기(계획 68 설계 §8.1·§8.5): 복제 회차와 같은 플리싱(copy-before-write 필터 + snapshot-access)으로
//! 디스크를 한 시점에 고정하고 VM 상태 디렉터리의 유닉스 소켓 NBD 서버에 읽기 전용으로 내보낸다. opld의 백업 Task가
//! 읽고(스테이지 4), 끝나면 [`QemuHypervisor::end_store_read`]가 뗀다. 플리싱 이미지 자리가 차지 않게 지킴이가 지켜본다.

use super::*;
use crate::hypervisor::{FleeceDisk, FleeceExport, FleeceRequest};

/// 디스크마다의 NBD 내보내기 id 접두사(`opl-bk-{i}`)
pub(super) const STORE_EXPORT_PREFIX: &str = "opl-bk-";
/// 부모 비트맵의 "그 시점까지"를 얼려 두는 임시 비트맵(디스크 노드마다, 저장하지 않고 꺼져 있음 — 내보내기에 단다)
pub(super) const POINT_BITMAP: &str = "opl-fl-point";
/// VM 상태 디렉터리 안 NBD 서버 소켓
pub(super) const BACKUP_SOCKET: &str = "backup.sock";
/// 지킴이가 남은 공간을 보는 간격
const GUARD_TICK: Duration = Duration::from_secs(1);
/// 내보내기를 닫은 뒤 QEMU가 그것을 다 치우기를 기다리는 한도
const EXPORT_GONE_TIMEOUT: Duration = Duration::from_secs(10);

/// 열린 저장소 백업 읽기 하나
pub(super) struct StoreRead {
    /// 붙인 플리싱(지킴이와 끝내기가 함께 쓴다)
    fleeces: Arc<tokio::sync::Mutex<Vec<Fleece>>>,
    /// 지킴이를 멈추라는 신호와 지킴이
    stop: watch::Sender<bool>,
    guard: JoinHandle<()>,
    /// 지킴이가 뗐으면 그 까닭
    tripped: Arc<Mutex<Option<String>>>,
}

/// 디렉터리 `dir`이 있는 파일 시스템의 남은 공간(바이트)
fn free_bytes(dir: &Path) -> Result<u64, String> {
    let st = rustix::fs::statvfs(dir)
        .map_err(|e| format!("cannot read the free space of {}: {e}", dir.display()))?;
    Ok(st.f_bavail.saturating_mul(st.f_frsize))
}

/// 남은 NBD 내보내기 id들(`query-block-exports`)
async fn export_ids(qmp: &QmpClient) -> Result<Vec<String>, DriverError> {
    let r = qmp_call(
        qmp.execute("query-block-exports", None),
        "QMP query-block-exports",
    )
    .await?;
    Ok(r.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["id"].as_str().map(str::to_string))
        .collect())
}

/// 저장소 백업의 내보내기(`opl-bk-*`)를 닫고 다 사라지기를 기다린 뒤, 그 NBD 서버를 멈추고(우리 소켓이 있었거나 내보내기가
/// 있었으면) 소켓 파일과 임시 비트맵([`POINT_BITMAP`])을 지운다. 플리싱 노드는 건드리지 않는다.
pub(super) async fn clear_store_exports(qmp: &QmpClient, dir: &Path) -> Result<(), DriverError> {
    let ours: Vec<String> = export_ids(qmp)
        .await?
        .into_iter()
        .filter(|id| id.starts_with(STORE_EXPORT_PREFIX))
        .collect();
    for id in &ours {
        qmp_call(qmp.block_export_del(id), "QMP block-export-del").await?;
    }
    // 닫힌 내보내기는 붙은 클라이언트가 떨어진 뒤에 사라진다(그때까지 노드와 비트맵을 쥔다).
    let deadline = tokio::time::Instant::now() + EXPORT_GONE_TIMEOUT;
    loop {
        let left = export_ids(qmp).await?;
        if !left.iter().any(|id| id.starts_with(STORE_EXPORT_PREFIX)) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(DriverError::Backup(format!(
                "the backup exports did not close: {left:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let socket = dir.join(BACKUP_SOCKET);
    if !ours.is_empty() || socket.exists() {
        // 서버가 없으면 오류다 — 이미 멈췄다.
        if let Err(e) = qmp_call(qmp.nbd_server_stop(), "QMP nbd-server-stop").await {
            tracing::debug!(error = %e, "no NBD server to stop");
        }
        remove_if_exists(&socket)?;
    }
    remove_bitmaps_where(qmp, |name| name == POINT_BITMAP).await
}

/// 시작 전 남은 공간 확인([`FleeceRequest::reserve_min`]; 디스크 크기 합의 10%는 [`start_fleecing`]이 본다)
fn check_reserve(dir: &Path, need: u64) -> Result<(), DriverError> {
    let available = free_bytes(dir).map_err(DriverError::Backup)?;
    if available < need {
        return Err(DriverError::Backup(format!(
            "not enough scratch space for fleecing in {}: need {need}, available {available}",
            dir.display()
        )));
    }
    Ok(())
}

/// 플리싱을 붙이고(새 비트맵·임시 비트맵은 그 transaction 안에서) 내보낸다. 붙인 것은 `fleeces`에 적는다.
#[allow(clippy::too_many_arguments)]
async fn open_exports(
    qmp: &QmpClient,
    disks: &[BlockNode],
    req: &FleeceRequest,
    (dir, halt, freezer): (&Path, &FleeceHalt<'_>, &mut Freezer),
    fleeces: &mut Vec<Fleece>,
) -> Result<FleeceExport, DriverError> {
    let devices = block_devices(qmp).await?;
    let mut actions: Vec<TransactionAction> =
        disks.iter().map(|d| bitmap_add(d, &req.bitmap)).collect();
    if let Some(parent) = &req.parent {
        for d in disks {
            actions.push(TransactionAction::BitmapAddDisabled {
                node: d.node_name.clone(),
                name: POINT_BITMAP.into(),
            });
            actions.push(TransactionAction::BitmapMerge {
                node: d.node_name.clone(),
                target: POINT_BITMAP.into(),
                bitmaps: vec![(d.node_name.clone(), parent.clone())],
            });
        }
    }
    let indexes: Vec<usize> = (0..disks.len()).collect();
    start_fleecing(
        qmp,
        disks,
        &indexes,
        &actions,
        (dir, halt, freezer),
        DriverError::Backup,
        fleeces,
    )
    .await?;
    let socket = dir.join(BACKUP_SOCKET);
    remove_if_exists(&socket)?;
    qmp_call(qmp.nbd_server_start_unix(&socket), "QMP nbd-server-start").await?;
    let mut out = Vec::new();
    for (i, d) in disks.iter().enumerate() {
        let (_, _, access) = fleece_nodes(i);
        let name = format!("disk{i}");
        let bitmaps = match &req.parent {
            Some(_) => vec![(d.node_name.clone(), POINT_BITMAP.to_string())],
            None => Vec::new(),
        };
        let export_id = format!("{STORE_EXPORT_PREFIX}{i}");
        let add = qmp.block_export_add_nbd_ro(&export_id, &access, &name, &bitmaps);
        qmp_call(add, "QMP block-export-add").await?;
        let size = devices
            .iter()
            .find(|(root, _, _)| *root == d.node_name)
            .map_or(0, |(_, _, size)| *size);
        out.push(FleeceDisk {
            export: name,
            size,
            dirty_context: req
                .parent
                .as_ref()
                .map(|_| orbivisor_store::nbd::dirty_bitmap_context(POINT_BITMAP)),
        });
    }
    Ok(FleeceExport {
        socket,
        disks: out,
        quiesced: Quiesced::No,
    })
}

/// 지킴이: `dir`의 남은 공간이 `min_free`보다 적어지면 내보내기와 플리싱을 뗀다(게스트 쓰기가 플리싱 이미지로 가지
/// 않게). 멈추라는 신호를 받으면 끝난다.
fn spawn_guard(
    handle: Arc<VmHandle>,
    dir: PathBuf,
    min_free: u64,
    fleeces: Arc<tokio::sync::Mutex<Vec<Fleece>>>,
    tripped: Arc<Mutex<Option<String>>>,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                () = tokio::time::sleep(GUARD_TICK) => {}
            }
            let available = match free_bytes(&dir) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(error = %e, "fleece guard cannot read the free space");
                    continue;
                }
            };
            if available >= min_free {
                continue;
            }
            let reason = format!(
                "the backup was stopped: only {available} bytes left in {} for the fleecing images (keeping {min_free})",
                dir.display()
            );
            tracing::warn!("{reason}");
            *tripped.lock().unwrap() = Some(reason);
            let qmp = &handle.qmp;
            if let Err(e) = clear_store_exports(qmp, &dir).await {
                tracing::warn!(error = %e, "fleece guard cannot close the backup exports");
            }
            if let Err(e) = stop_fleecing(qmp, &mut *fleeces.lock().await).await {
                tracing::warn!(error = %e, "fleece guard cannot remove the fleecing");
            }
            return;
        }
    })
}

impl QemuHypervisor {
    pub(super) async fn begin_store_read(
        &self,
        id: Uuid,
        req: &FleeceRequest,
    ) -> Result<FleeceExport, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let dir = self.state_dir(id);
        // 같은 VM의 앞선 읽기가 닫히지 않았으면(Task가 사라짐) 지킴이를 멈추고 뗀다. 그 비트맵은 그대로 둔다.
        let earlier = self.store_reads.lock().unwrap().remove(&id);
        if let Some(earlier) = earlier {
            tracing::warn!(%id, "closing a backup read that was left open");
            close_read(qmp, &dir, earlier).await?;
        }
        clear_backup_leftovers(qmp).await?;
        clear_fleecing(qmp, &dir).await?;
        let disks = query_disks(qmp).await?;
        if disks.is_empty() {
            return Err(DriverError::Backup(format!("VM {id} has no disks")));
        }
        if let Some(parent) = &req.parent
            && let Some(d) = disks.iter().find(|d| {
                !d.bitmaps
                    .iter()
                    .any(|b| &b.name == parent && !b.inconsistent)
            })
        {
            return Err(DriverError::Backup(format!(
                "{} has no usable dirty bitmap {parent}",
                d.node_name
            )));
        }
        std::fs::create_dir_all(&dir)?;
        check_reserve(&dir, req.reserve_min)?;
        let halt = self.fleece_halt(id, &handle);
        let mut freezer = Freezer::new(req.quiesce, &handle.paths, &handle.agent);
        let mut fleeces = Vec::new();
        let opened =
            open_exports(qmp, &disks, req, (&dir, &halt, &mut freezer), &mut fleeces).await;
        let export = match opened {
            Ok(mut export) => {
                export.quiesced = freezer.outcome();
                export
            }
            Err(e) => {
                tracing::warn!(%id, error = %e, "cannot open the backup read; removing what was attached");
                if let Err(clear) = clear_store_exports(qmp, &dir).await {
                    tracing::warn!(%id, error = %clear, "cannot close the backup exports");
                }
                if let Err(clear) = stop_fleecing(qmp, &mut fleeces).await {
                    tracing::warn!(%id, error = %clear, "cannot remove the fleecing");
                }
                // 새 비트맵은 없을 수 있다(transaction 전에 실패).
                drop_bitmap(qmp, &req.bitmap).await;
                return Err(e);
            }
        };
        let fleeces = Arc::new(tokio::sync::Mutex::new(fleeces));
        let tripped = Arc::new(Mutex::new(None));
        let (stop, stopped) = watch::channel(false);
        let guard = spawn_guard(
            handle.clone(),
            dir,
            req.min_free,
            fleeces.clone(),
            tripped.clone(),
            stopped,
        );
        self.store_reads.lock().unwrap().insert(
            id,
            StoreRead {
                fleeces,
                stop,
                guard,
                tripped,
            },
        );
        Ok(export)
    }

    pub(super) async fn end_store_read(
        &self,
        id: Uuid,
        req: &FleeceRequest,
        success: bool,
    ) -> Result<(), DriverError> {
        let read = self.store_reads.lock().unwrap().remove(&id);
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let dir = self.state_dir(id);
        let mut result: Result<Option<String>, DriverError> = match read {
            Some(read) => close_read(qmp, &dir, read).await,
            // opld가 다시 떴다: VM에 남은 것을 치운다.
            None => {
                async {
                    clear_store_exports(qmp, &dir).await?;
                    clear_fleecing(qmp, &dir).await?;
                    Ok(None)
                }
                .await
            }
        };
        if success && matches!(result, Ok(None)) {
            if let Some(parent) = &req.parent {
                drop_bitmap(qmp, parent).await;
            }
        } else {
            drop_bitmap(qmp, &req.bitmap).await;
        }
        if let Ok(Some(reason)) = &result {
            result = Err(DriverError::Backup(reason.clone()));
        }
        result.map(drop)
    }
}

/// 읽기를 닫는다: 지킴이를 멈추고(떼는 중이면 끝나기를 기다림) 내보내기·플리싱을 뗀다. 지킴이가 뗐으면 그 까닭.
async fn close_read(
    qmp: &QmpClient,
    dir: &Path,
    read: StoreRead,
) -> Result<Option<String>, DriverError> {
    let _ = read.stop.send(true);
    if let Err(e) = read.guard.await {
        tracing::warn!(error = %e, "fleece guard failed");
    }
    let tripped = read.tripped.lock().unwrap().clone();
    let mut result = clear_store_exports(qmp, dir).await;
    let mut fleeces = read.fleeces.lock().await;
    if let Err(e) = stop_fleecing(qmp, &mut fleeces).await {
        result = result.and(Err(e));
    }
    // 떼지 못한 것이 있으면 남은 것 치우기로 한 번 더(장치가 필터를 쓰면 되돌림).
    if result.is_err()
        && let Err(e) = clear_fleecing(qmp, dir).await
    {
        tracing::warn!(error = %e, "cannot remove the fleecing left by the backup read");
    }
    result.map(|()| tripped)
}

/// 모든 디스크에서 비트맵 `name`을 지운다(없으면 넘어감, 실패는 경고만).
async fn drop_bitmap(qmp: &QmpClient, name: &str) {
    let disks = match query_disks(qmp).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(bitmap = name, error = %e, "cannot read dirty bitmaps");
            return;
        }
    };
    for d in disks
        .iter()
        .filter(|d| d.bitmaps.iter().any(|b| b.name == name))
    {
        let remove = qmp.block_dirty_bitmap_remove(&d.node_name, name);
        if let Err(e) = qmp_call(remove, "QMP block-dirty-bitmap-remove").await {
            tracing::warn!(node = %d.node_name, bitmap = name, error = %e, "cannot remove dirty bitmap");
        }
    }
}
