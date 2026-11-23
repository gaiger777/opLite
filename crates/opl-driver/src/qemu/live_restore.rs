//! 라이브 복원(계획 68 설계 §11.2): 배선 파일(`{VM 상태 디렉터리}/live-restore.json`)이 있으면 `start`가 디스크마다 백업
//! (`disk{i}-lrsrc`) → 오버레이(`disk{i}-lr`)를 두고 장치를 오버레이에 붙여 켠다. 켜진 뒤 디스크마다 mirror(`opl-lr-{i}`)가
//! 오버레이를 대상 볼륨 노드(`disk{i}`)로 옮기고, 모두 `ready`면 `job-complete`로 장치가 대상으로 갈아탄다. 배선은 opld가
//! 다시 떠도 남아 QEMU가 죽은 뒤 다시 켜도 같은 배선이다.

use super::*;
use crate::hypervisor::LiveRestoreJob;
use opl_types::runtime::LiveRestoreDisk;

/// VM 상태 디렉터리 안 배선 파일
pub(super) const LIVE_RESTORE_FILE: &str = "live-restore.json";
/// 디스크마다의 mirror 작업 id 접두사(`opl-lr-{i}`)
pub(super) const LIVE_RESTORE_JOB_PREFIX: &str = "opl-lr-";
/// `job-complete` 뒤 mirror가 끝나기를 기다리는 한도(이 시점까지 맞추는 데 드는 시간)
const PIVOT_WAIT: Duration = Duration::from_secs(60);

/// 상태 디렉터리 `dir`의 배선(없으면 `None`)
pub(super) fn read_wiring(dir: &Path) -> Result<Option<Vec<LiveRestoreDisk>>, DriverError> {
    match std::fs::read(dir.join(LIVE_RESTORE_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| DriverError::LiveRestore(format!("invalid live restore wiring: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// 장치에 붙은 맨 위 노드들(`query-block`)
async fn attached(qmp: &QmpClient) -> Result<HashSet<String>, DriverError> {
    let roots = qmp_call(qmp.query_block_roots(), "QMP query-block").await?;
    Ok(roots.into_iter().collect())
}

/// 블록 작업마다의 `actively-synced`(QEMU 8.2 전은 None)
async fn synced_jobs(qmp: &QmpClient) -> Result<Vec<(String, Option<bool>)>, DriverError> {
    qmp_call(qmp.query_block_jobs_synced(), "QMP query-block-jobs").await
}

/// `ready`인 미러 `job`이 대상과 맞춰졌는지(리뷰 1 I3): `actively-synced`가 false면 아직 아니다 — 그 뒤 쓰기는 대상에도
/// 바로 가지만 앞선 더러운 영역이 남았을 수 있다. 그 칸이 없는 옛 QEMU는 `ready`를 믿는다(처음부터 write-blocking인
/// 미러는 `ready`가 될 때 맞춰진다).
fn in_sync(synced: &[(String, Option<bool>)], job: &str) -> bool {
    synced
        .iter()
        .find(|(id, _)| id == job)
        .and_then(|(_, s)| *s)
        != Some(false)
}

/// 배선에서 디스크 `disks`를 갈아탄 것으로 적는다(이미 적혀 있으면 쓰지 않는다).
fn mark_pivoted(dir: &Path, disks: &[usize]) -> Result<(), DriverError> {
    let Some(mut wiring) = read_wiring(dir)? else {
        return Ok(());
    };
    let mut changed = false;
    for &i in disks {
        if let Some(d) = wiring.get_mut(i).filter(|d| !d.pivoted) {
            d.pivoted = true;
            changed = true;
        }
    }
    if changed {
        write_wiring(dir, &wiring)?;
    }
    Ok(())
}

/// 배선 파일을 바꾼다: 임시 파일에 쓰고 fsync한 뒤 rename, 디렉터리도 fsync(리뷰 2 N4 — 전원이 나간 뒤 빈 파일이나
/// `pivoted`를 잃은 옛 배선이 남으면 갈아탄 디스크를 옛 오버레이로 덮는다).
fn write_wiring(dir: &Path, disks: &[LiveRestoreDisk]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!("{LIVE_RESTORE_FILE}.tmp"));
    let json = serde_json::to_vec(disks).expect("wiring serializes");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&json)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, dir.join(LIVE_RESTORE_FILE))?;
    std::fs::File::open(dir)?.sync_all()
}

/// 이름 있는 블록 노드들
async fn node_names(qmp: &QmpClient) -> Result<HashSet<String>, DriverError> {
    let nodes = qmp_call(qmp.query_block_nodes(), "QMP query-named-block-nodes").await?;
    Ok(nodes.into_iter().map(|n| n.node_name).collect())
}

impl QemuHypervisor {
    /// [`Hypervisor::set_live_restore`]
    pub(super) fn write_live_restore(
        &self,
        id: Uuid,
        disks: Option<&[LiveRestoreDisk]>,
    ) -> Result<(), DriverError> {
        let dir = self.state_dir(id);
        let file = dir.join(LIVE_RESTORE_FILE);
        match disks {
            Some(disks) => {
                std::fs::create_dir_all(&dir)?;
                write_wiring(&dir, disks)?;
            }
            None => {
                if let Some(old) = read_wiring(&dir)? {
                    for d in old {
                        remove_if_exists(&d.overlay)?;
                    }
                }
                remove_if_exists(&file)?;
            }
        }
        Ok(())
    }

    /// [`Hypervisor::start_live_restore`]
    pub(super) async fn live_restore_jobs(
        &self,
        id: Uuid,
    ) -> Result<Vec<LiveRestoreJob>, DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let dir = self.state_dir(id);
        let wiring = read_wiring(&dir)?.ok_or_else(|| {
            DriverError::LiveRestore(format!("VM {id} has no live restore wiring"))
        })?;
        let jobs = query_jobs(qmp).await?;
        let names = node_names(qmp).await?;
        let roots = attached(qmp).await?;
        let synced = synced_jobs(qmp).await?;
        let mut out = Vec::with_capacity(wiring.len());
        let mut switched = Vec::new();
        for (i, disk) in wiring.iter().enumerate() {
            let job_id = format!("{LIVE_RESTORE_JOB_PREFIX}{i}");
            let overlay = format!("disk{i}-lr");
            let mut job = LiveRestoreJob {
                disk: i,
                ready: false,
                done_bytes: 0,
                total_bytes: 0,
                error: None,
            };
            match jobs.iter().find(|j| j.id == job_id) {
                // 이미 갈아탔다(배선에 적었거나 오버레이 노드가 없다).
                _ if disk.pivoted => job.ready = true,
                None if !names.contains(&overlay) => job.ready = true,
                // 장치가 오버레이에 붙어 있지 않다: 앞선 갈아타기가 끝낸 뒤 opld가 죽었다 — 대상이 게스트의 디스크다.
                // 오버레이를 다시 옮기면 갈아탄 뒤의 쓰기를 덮으므로 옮기지 않는다(리뷰 1 I3).
                None if !roots.contains(&overlay) => {
                    switched.push(i);
                    job.ready = true;
                }
                None => {
                    let target = format!("disk{i}");
                    let mirror = qmp.blockdev_mirror_active(&job_id, &overlay, &target);
                    qmp_call(mirror, "QMP blockdev-mirror").await?;
                }
                Some(j) if j.is_concluded() => {
                    qmp_call(qmp.job_dismiss(&job_id), "QMP job-dismiss").await?;
                    if j.error.is_none() && !roots.contains(&overlay) {
                        // 갈아타기가 끝났다(opld가 그 사이 죽었다).
                        switched.push(i);
                        job.ready = true;
                    } else {
                        // 오류로 끝났다(대상 쓰기 실패 등) — 장치는 오버레이에 있다. 다음 부름이 처음부터 다시 건다.
                        job.error = Some(format!(
                            "the copy of disk {i} failed: {}",
                            j.error.as_deref().unwrap_or("it ended early")
                        ));
                    }
                }
                Some(j) if j.status == "paused" || j.status == "standby" => {
                    // 백업을 읽지 못해 멈췄다(`on-source-error: stop`). 잇는다 — 아직 읽지 못하면 다시 멈춘다.
                    qmp_call(qmp.job_resume(&job_id), "QMP job-resume").await?;
                    job.done_bytes = j.current_progress;
                    job.total_bytes = j.total_progress;
                    job.error = Some(format!(
                        "the copy of disk {i} stopped because the backup could not be read; resumed"
                    ));
                }
                Some(j) => {
                    job.ready = j.is_ready() && in_sync(&synced, &job_id);
                    job.done_bytes = j.current_progress;
                    job.total_bytes = j.total_progress;
                }
            }
            out.push(job);
        }
        mark_pivoted(&dir, &switched)?;
        Ok(out)
    }

    /// [`Hypervisor::pivot_live_restore`]
    pub(super) async fn pivot(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let dir = self.state_dir(id);
        let wiring = read_wiring(&dir)?.ok_or_else(|| {
            DriverError::LiveRestore(format!("VM {id} has no live restore wiring"))
        })?;
        let disks = wiring.len();
        let jobs = query_jobs(qmp).await?;
        let names = node_names(qmp).await?;
        let roots = attached(qmp).await?;
        let synced = synced_jobs(qmp).await?;
        let mut ids = Vec::new();
        // 한 디스크의 `job-complete`가 실패해도 이미 건 다른 디스크의 갈아타기는 끝까지 보고 배선에 적는다.
        let mut refused = None;
        for (i, disk) in wiring.iter().enumerate() {
            let job_id = format!("{LIVE_RESTORE_JOB_PREFIX}{i}");
            match jobs.iter().find(|j| j.id == job_id) {
                _ if disk.pivoted => {}
                Some(j) if j.is_ready() && in_sync(&synced, &job_id) => {
                    match qmp_call(qmp.job_complete(&job_id), "QMP job-complete").await {
                        Ok(()) => ids.push(job_id),
                        // 갈아타기 실패(리뷰 2 N6): `LiveRestore`로 알려 Task가 `Copying`으로 돌아가 다시 옮기게 한다.
                        Err(e) => {
                            refused = Some(DriverError::LiveRestore(format!(
                                "disk {i}: job-complete failed: {e}"
                            )));
                            break;
                        }
                    }
                }
                // 앞선 갈아타기가 끝낸 작업(opld가 그 사이 죽었다)
                Some(j) if j.is_concluded() => ids.push(job_id),
                Some(j) => {
                    refused = Some(DriverError::LiveRestore(format!(
                        "disk {i} is not copied yet ({})",
                        j.status
                    )));
                    break;
                }
                // 장치가 아직 오버레이에 있는데 옮기는 작업이 없다(갈아타기가 실패했다) — 다시 옮겨야 한다.
                None if roots.contains(&format!("disk{i}-lr")) => {
                    refused = Some(DriverError::LiveRestore(format!(
                        "disk {i} has no copy running"
                    )));
                    break;
                }
                None => {}
            }
        }
        if ids.is_empty()
            && let Some(e) = refused
        {
            return Err(e);
        }
        // 모두 끝나기를 기다린다(하나가 실패해도 다른 것을 취소하지 않는다 — 끝낸 디스크는 이미 대상에 있다).
        let deadline = tokio::time::Instant::now() + PIVOT_WAIT;
        let done = loop {
            let jobs = query_jobs(qmp).await?;
            let mine: Vec<&JobInfo> = jobs.iter().filter(|j| ids.contains(&j.id)).collect();
            if mine.iter().all(|j| j.is_concluded()) {
                break jobs;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DriverError::Timeout("live restore mirrors to complete"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        // 갈아탄 디스크(장치가 더는 오버레이에 붙지 않은 것)를 배선에 먼저 적는다(리뷰 1 I3): 실패한 디스크를 다시 옮기는
        // 동안 QEMU가 다시 떠도 그 디스크는 대상에 붙는다. 그다음 작업을 치운다.
        let roots = attached(qmp).await?;
        let switched: Vec<usize> = (0..disks)
            .filter(|i| !roots.contains(&format!("disk{i}-lr")))
            .collect();
        // 장치가 아직 오버레이에 있는 디스크는 갈아타지 못했다(오류 없이 끝났어도 — 그 오버레이를 지우면 안 된다).
        let failed: Vec<String> = (0..disks)
            .filter(|i| !switched.contains(i))
            .map(|i| {
                let id = format!("{LIVE_RESTORE_JOB_PREFIX}{i}");
                let why = done
                    .iter()
                    .find(|j| j.id == id)
                    .and_then(|j| j.error.clone())
                    .unwrap_or_else(|| "it ended without switching".into());
                format!("{id}: {why}")
            })
            .collect();
        mark_pivoted(&dir, &switched)?;
        for id in &ids {
            if done.iter().any(|j| &j.id == id) {
                qmp_call(qmp.job_dismiss(id), "QMP job-dismiss").await?;
            }
        }
        // 갈아탄 디스크의 오버레이·백업 노드를 지운다(장치는 이제 `disk{i}`).
        for i in switched {
            for node in [format!("disk{i}-lr"), format!("disk{i}-lrsrc")] {
                if names.contains(&node) {
                    qmp_call(qmp.blockdev_del(&node), "QMP blockdev-del").await?;
                }
            }
        }
        if let Some(e) = refused {
            return Err(e);
        }
        if !failed.is_empty() {
            return Err(DriverError::LiveRestore(format!(
                "switching to the restored disks failed: {}",
                failed.join("; ")
            )));
        }
        Ok(())
    }
}
