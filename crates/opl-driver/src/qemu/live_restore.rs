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
                let tmp = dir.join(format!("{LIVE_RESTORE_FILE}.tmp"));
                let json = serde_json::to_vec(disks).expect("wiring serializes");
                std::fs::write(&tmp, json)?;
                std::fs::rename(&tmp, &file)?;
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
        let disks = read_wiring(&self.state_dir(id))?
            .ok_or_else(|| DriverError::LiveRestore(format!("VM {id} has no live restore wiring")))?
            .len();
        let jobs = query_jobs(qmp).await?;
        let names = node_names(qmp).await?;
        let mut out = Vec::with_capacity(disks);
        for i in 0..disks {
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
                // 이미 갈아탔다(오버레이 노드가 없다).
                None if !names.contains(&overlay) => job.ready = true,
                None => {
                    let target = format!("disk{i}");
                    let mirror = qmp.blockdev_mirror_active(&job_id, &overlay, &target);
                    qmp_call(mirror, "QMP blockdev-mirror").await?;
                }
                Some(j) if j.is_concluded() => {
                    // 오류로 끝났다(대상 쓰기 실패 등). 치우고 다음 부름이 처음부터 다시 건다.
                    qmp_call(qmp.job_dismiss(&job_id), "QMP job-dismiss").await?;
                    job.error = Some(format!(
                        "the copy of disk {i} failed: {}",
                        j.error.as_deref().unwrap_or("it ended early")
                    ));
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
                    job.ready = j.is_ready();
                    job.done_bytes = j.current_progress;
                    job.total_bytes = j.total_progress;
                }
            }
            out.push(job);
        }
        Ok(out)
    }

    /// [`Hypervisor::pivot_live_restore`]
    pub(super) async fn pivot(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        let disks = read_wiring(&self.state_dir(id))?
            .ok_or_else(|| DriverError::LiveRestore(format!("VM {id} has no live restore wiring")))?
            .len();
        let jobs = query_jobs(qmp).await?;
        let names = node_names(qmp).await?;
        let mut ids = Vec::new();
        for i in 0..disks {
            let job_id = format!("{LIVE_RESTORE_JOB_PREFIX}{i}");
            match jobs.iter().find(|j| j.id == job_id) {
                Some(j) if j.is_ready() => {
                    qmp_call(qmp.job_complete(&job_id), "QMP job-complete").await?;
                    ids.push(job_id);
                }
                // 앞선 갈아타기가 끝낸 작업(opld가 그 사이 죽었다)
                Some(j) if j.is_concluded() => ids.push(job_id),
                Some(j) => {
                    return Err(DriverError::LiveRestore(format!(
                        "disk {i} is not copied yet ({})",
                        j.status
                    )));
                }
                None if names.contains(&format!("disk{i}-lr")) => {
                    return Err(DriverError::LiveRestore(format!(
                        "disk {i} has no copy running"
                    )));
                }
                None => {}
            }
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
        let mut failed = Vec::new();
        for id in &ids {
            let job = done.iter().find(|j| &j.id == id);
            if let Some(error) = job.and_then(|j| j.error.clone()) {
                failed.push(format!("{id}: {error}"));
            }
            if job.is_some() {
                qmp_call(qmp.job_dismiss(id), "QMP job-dismiss").await?;
            }
        }
        // 갈아탄 디스크의 오버레이·백업 노드를 지운다(장치는 이제 `disk{i}`).
        let names = node_names(qmp).await?;
        for i in 0..disks {
            let job_id = format!("{LIVE_RESTORE_JOB_PREFIX}{i}");
            if failed.iter().any(|f| f.starts_with(&format!("{job_id}:"))) {
                continue;
            }
            for node in [format!("disk{i}-lr"), format!("disk{i}-lrsrc")] {
                if names.contains(&node) {
                    qmp_call(qmp.blockdev_del(&node), "QMP blockdev-del").await?;
                }
            }
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
