//! 복제본 기록 파일(계획 55): 이 노드가 가진 VM 디스크의 복제본(`replica`)과, 이 노드가 원본으로 복제를 보낸
//! VM 디스크(`primary`). `{replica_dir}/{vm}.json`(0600, 임시 파일에 써서 fsync한 뒤 이름을 바꾼다). 이 노드에서
//! 복제본 파일을 지우는 유일한 근거다: 복제본은 Raft 리소스가 아니고(원본과 같은 볼륨 id), Raft만 보고 지우면
//! 잘못 설정한 풀에서 남의 디스크를 지울 수 있다. 받기·끝내기·합치기·청소·승격은 그 VM의 잠금([`lock_vm`])을 쥐고,
//! 기록 파일을 읽고 쓰는 동안 기록 디렉터리의 잠금([`lock`])을 쥔다(VM 잠금 다음에). 오래 걸리는 일(겹층 합치기,
//! 받는 QEMU 띄우기)을 하는 받기 준비와 승격은 VM 잠금만 쥐고 그 VM의 기록을 다룬다([`vm_records`]) — 기록 파일은
//! VM마다 하나이고 그 파일을 쓰는 쪽은 모두 그 VM의 잠금을 쥔다.
//!
//! 증분 회차는 복제본 대신 겹층(`{replica_dir}/overlays/{run}-{volume}.qcow2`, backing = 복제본)에 받는다. 끝까지
//! 받은 겹층(`received`)은 끝난 회차라 합치기([`settle`])가 끊겨도 다시 하면 같아지고, 받지 못한 겹층은 버린다.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use opl_cluster::Cluster;
use opl_driver::{DriverError, Storage};
use opl_types::runtime::ImageFormat;
use opl_types::storage::StoragePoolSpec;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 기록 파일 하나: VM 하나에서 이 노드가 가진 디스크들
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaRecord {
    pub vm: Uuid,
    pub role: Role,
    /// 이 노드가 끝까지 받은 마지막 회차(복제본만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<Uuid>,
    /// 그 회차의 시점(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<u64>,
    pub volumes: Vec<RecordedReplica>,
}

/// 이 노드가 그 VM에서 맡은 쪽
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// VM이 도는 원본 노드: 기록의 볼륨은 볼륨 리소스의 진짜 디스크다(HA 복구 뒤 돌아온 옛 원본을 알아보는 근거).
    Primary,
    /// 복제 대상 노드: 기록의 볼륨은 복제본이다.
    Replica,
}

/// 이 노드의 볼륨 파일 하나(풀 안에서 id로 찾는다)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedReplica {
    pub id: Uuid,
    pub pool: String,
    pub format: ImageFormat,
    /// 복제본의 가상 크기
    pub size_bytes: u64,
    /// 이 노드가 이 볼륨을 만들었다(`create_volume`이 성공한 뒤 다시 적는다). 만들기 전에 적은 항목은 남의 파일일 수
    /// 있으므로 지우지 않는다.
    #[serde(default)]
    pub created: bool,
    pub state: ReplicaState,
    /// 증분 회차를 받는(받은) 겹층
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
}

/// 복제본 볼륨 하나의 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplicaState {
    /// 기록의 회차 시점으로 온전하다.
    Ready,
    /// 전체 복사가 복제본 위에 쓰는 중이다(쓸 수 없다).
    Syncing,
    /// HA 복구 뒤 남은 옛 원본이다(전체 복사가 필요하다).
    Stale,
}

/// 증분 회차의 겹층 파일
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlay {
    /// 그 회차(원본의 `VmReplicate` Task id)
    pub run: Uuid,
    /// 겹층 파일(절대 경로)
    pub path: PathBuf,
    /// 받는 QEMU가 끝까지 받아 내보냈다(끝난 회차). 아니면 버린다.
    #[serde(default)]
    pub received: bool,
}

/// 기록 디렉터리의 잠금([`lock`])이나 VM 잠금([`vm_records`])을 쥔 동안. 그 디렉터리를 알려 준다.
pub struct Records {
    dir: PathBuf,
    _guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Records {
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// 기록 디렉터리 `dir`의 잠금을 기다려 쥔다. 디렉터리마다 하나이므로 받기·끝내기·버리기·청소·승격이 함께 쓴다
/// (청소가 막 만든 복제본을 지우지 않게, 합치기와 받기가 같은 복제본에 겹치지 않게).
pub async fn lock(dir: &Path) -> Records {
    static LOCKS: LazyLock<std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(Default::default);
    let lock = LOCKS
        .lock()
        .unwrap()
        .entry(dir.to_path_buf())
        .or_default()
        .clone();
    Records {
        dir: dir.to_path_buf(),
        _guard: Some(lock.lock_owned().await),
    }
}

/// VM 잠금([`lock_vm`]의 `_vm`)만 쥔 동안 그 VM의 기록을 다루는 손잡이. 기록 디렉터리 잠금을 쥐지 않으므로 오래 걸리는
/// 일(겹층 합치기, 받는 QEMU 띄우기) 동안 다른 VM의 받기·끝내기·청소를 막지 않는다. 그 VM의 기록과 복제본만 다뤄야
/// 한다(기록 파일은 임시 파일에 써서 바꿔 넣고, 같은 파일을 쓰는 쪽은 모두 그 VM의 잠금을 쥔다).
pub fn vm_records(dir: &Path, _vm: &tokio::sync::OwnedMutexGuard<()>) -> Records {
    Records {
        dir: dir.to_path_buf(),
        _guard: None,
    }
}

/// 기록 디렉터리 `dir`에서 VM `vm`의 잠금을 기다려 쥔다. 그 VM의 기록과 복제본 파일을 다루는 일(받기·끝내기·겹층
/// 합치기·청소·승격)이 함께 쓰고 다른 VM의 일은 막지 않는다. 오래 걸리는 합치기(`qemu-img commit`)는 이 잠금만 쥔다.
/// 기록 디렉터리 잠금([`lock`])보다 먼저 쥔다.
pub async fn lock_vm(dir: &Path, vm: Uuid) -> tokio::sync::OwnedMutexGuard<()> {
    type VmLocks = HashMap<(PathBuf, Uuid), Arc<tokio::sync::Mutex<()>>>;
    static LOCKS: LazyLock<std::sync::Mutex<VmLocks>> = LazyLock::new(Default::default);
    let lock = LOCKS
        .lock()
        .unwrap()
        .entry((dir.to_path_buf(), vm))
        .or_default()
        .clone();
    lock.lock_owned().await
}

fn record_path(dir: &Path, vm: Uuid) -> PathBuf {
    dir.join(format!("{vm}.json"))
}

/// 겹층 파일을 두는 디렉터리
pub fn overlay_dir(dir: &Path) -> PathBuf {
    dir.join("overlays")
}

/// 회차 `run`에서 볼륨 `volume`의 겹층 파일
pub fn overlay_path(dir: &Path, run: Uuid, volume: Uuid) -> PathBuf {
    overlay_dir(dir).join(format!("{run}-{volume}.qcow2"))
}

/// VM `vm`의 기록. 없으면 `None`이다.
pub fn read(dir: &Path, vm: Uuid) -> std::io::Result<Option<ReplicaRecord>> {
    match std::fs::read(record_path(dir, vm)) {
        Ok(data) => Ok(Some(serde_json::from_slice(&data)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 기록을 임시 파일에 써서 디스크에 내린 뒤 바꿔 넣는다(디렉터리도 내린다).
pub fn write(dir: &Path, record: &ReplicaRecord) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let path = record_path(dir, record.vm);
    let tmp = dir.join(format!(".{}.json.tmp", record.vm));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec(record)?)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    std::fs::File::open(dir)?.sync_all()
}

/// VM `vm`의 기록을 지운다. 없으면 할 일이 없다.
pub fn remove(dir: &Path, vm: Uuid) -> std::io::Result<()> {
    match std::fs::remove_file(record_path(dir, vm)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 이 노드의 기록들. 읽지 못한 파일은 경고만 남기고 건너뛴다.
pub fn list(dir: &Path) -> Vec<ReplicaRecord> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot read replica records");
            return Vec::new();
        }
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_record = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".json") && !n.starts_with('.'));
        if !is_record || !path.is_file() {
            continue;
        }
        let read = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|data| serde_json::from_slice(&data).map_err(|e| e.to_string()));
        match read {
            Ok(record) => records.push(record),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read a replica record")
            }
        }
    }
    records
}

/// 겹층 파일을 지운다. 없으면 지운 것으로 본다.
pub fn discard_overlay(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 기록 `record`의 겹층을 정리한다: 끝까지 받은 겹층은 복제본에 합치고(`qemu-img commit`), 받지 못한 겹층은
/// 회차가 `live`에 없으면 버린다. 겹층마다 정리한 뒤 기록을 다시 쓴다. 부르는 쪽이 잠금(`records`)을 쥐고, 그
/// 복제본을 쓰는 QEMU(받는 QEMU)가 없어야 한다. 합치지 못하면 겹층을 남긴 채 오류다(다시 하면 같아진다).
/// 정리한 기록을 돌려준다.
pub async fn settle(
    cluster: &Cluster,
    storage: &dyn Storage,
    records: &Records,
    mut record: ReplicaRecord,
    live: &[Uuid],
) -> Result<ReplicaRecord, String> {
    let dir = records.dir();
    for i in 0..record.volumes.len() {
        let v = &record.volumes[i];
        let Some(overlay) = &v.overlay else {
            continue;
        };
        if overlay.received {
            let pool = cluster
                .get_by_name::<StoragePoolSpec>(&v.pool)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("storage pool {:?} not found", v.pool))?;
            tracing::info!(vm = %record.vm, volume = %v.id, run = %overlay.run, "merging a received replica increment");
            storage
                .commit_overlay(&pool, v.id, v.format, &overlay.path)
                .await
                .map_err(|e| format!("cannot merge the increment into replica {}: {e}", v.id))?;
        } else if live.contains(&overlay.run) {
            continue;
        } else {
            tracing::info!(vm = %record.vm, volume = %v.id, run = %overlay.run, "discarding an unfinished replica increment");
            discard_overlay(&overlay.path).map_err(|e| {
                format!(
                    "cannot remove the replica increment {}: {e}",
                    overlay.path.display()
                )
            })?;
        }
        record.volumes[i].overlay = None;
        write(dir, &record).map_err(|e| format!("cannot write the replica record: {e}"))?;
    }
    Ok(record)
}

/// VM `vm`의 기록에서 끝까지 받은 겹층(`received`)을 복제본에 합친다. 부르는 쪽이 그 VM의 잠금([`lock_vm`])을 쥐고,
/// 합치는 동안에는 기록 디렉터리 잠금을 쥐지 않는다(기록을 읽고 쓰는 동안만 쥔다 — 다른 VM의 받기를 막지 않게).
/// 합치지 못하면 겹층을 남긴 채 오류다(다음 받기·청소·승격이 [`settle`]로 다시 한다).
pub async fn merge_received(
    cluster: &Cluster,
    storage: &dyn Storage,
    dir: &Path,
    vm: Uuid,
) -> Result<(), String> {
    let record = {
        let _records = lock(dir).await;
        read(dir, vm).map_err(|e| format!("cannot read the replica record: {e}"))?
    };
    for v in record.iter().flat_map(|r| &r.volumes) {
        let Some(overlay) = v.overlay.as_ref().filter(|o| o.received) else {
            continue;
        };
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(&v.pool)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("storage pool {:?} not found", v.pool))?;
        tracing::info!(vm = %vm, volume = %v.id, run = %overlay.run, "merging a received replica increment");
        storage
            .commit_overlay(&pool, v.id, v.format, &overlay.path)
            .await
            .map_err(|e| format!("cannot merge the increment into replica {}: {e}", v.id))?;
        let records = lock(dir).await;
        let current =
            read(records.dir(), vm).map_err(|e| format!("cannot read the replica record: {e}"))?;
        if let Some(mut current) = current
            && let Some(entry) = current
                .volumes
                .iter_mut()
                .find(|r| r.id == v.id && r.overlay.as_ref() == Some(overlay))
        {
            entry.overlay = None;
            write(records.dir(), &current)
                .map_err(|e| format!("cannot write the replica record: {e}"))?;
        }
    }
    Ok(())
}

/// 이 노드의 복제본 볼륨 파일 하나를 끄고 지운다(그 겹층도). 만들었다고 적지 않은 항목은 지우지 않는다(남의 파일일
/// 수 있다). 없는 것은 지운 것으로 본다. 지웠거나 지울 것이 없으면 참이다.
pub async fn delete_replica(
    cluster: &Cluster,
    storage: &dyn Storage,
    vm: Uuid,
    v: &RecordedReplica,
) -> bool {
    if let Some(overlay) = &v.overlay
        && let Err(e) = discard_overlay(&overlay.path)
    {
        tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot remove a replica increment");
        return false;
    }
    if !v.created {
        tracing::warn!(vm = %vm, volume = %v.id, pool = %v.pool, "this node did not create this replica; forgetting it without deleting");
        return true;
    }
    let pool = match cluster.get_by_name::<StoragePoolSpec>(&v.pool) {
        Ok(Some(pool)) => pool,
        // 풀이 없으면 파일도 찾을 수 없다.
        Ok(None) => return true,
        Err(e) => {
            tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot read storage pool");
            return false;
        }
    };
    if let Err(e) = storage.deactivate_volume(&pool, v.id, v.format).await {
        tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot deactivate volume");
    }
    match storage.delete_volume(&pool, v.id, v.format).await {
        Ok(()) | Err(DriverError::VolumeNotFound(_)) => true,
        Err(e) => {
            tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot delete a replica");
            false
        }
    }
}

/// `dir`이 있는 파일 시스템의 남은 바이트(일반 사용자가 쓸 수 있는 만큼)
pub fn free_bytes(dir: &Path) -> std::io::Result<u64> {
    let st = rustix::fs::statvfs(dir)?;
    Ok(st.f_bavail.saturating_mul(st.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(vm: Uuid) -> ReplicaRecord {
        ReplicaRecord {
            vm,
            role: Role::Replica,
            generation: Some(Uuid::now_v7()),
            synced_at: Some(1700),
            volumes: vec![RecordedReplica {
                id: Uuid::now_v7(),
                pool: "b".into(),
                format: ImageFormat::Qcow2,
                size_bytes: 1 << 30,
                created: true,
                state: ReplicaState::Ready,
                overlay: Some(Overlay {
                    run: Uuid::now_v7(),
                    path: "/var/lib/orbivisor/replicas/overlays/x.qcow2".into(),
                    received: false,
                }),
            }],
        }
    }

    #[test]
    fn records_round_trip_through_the_directory() {
        let root = tempfile::TempDir::new().unwrap();
        let dir = root.path().join("replicas");
        assert!(list(&dir).is_empty());
        let (a, b) = (record(Uuid::now_v7()), record(Uuid::now_v7()));
        assert_eq!(read(&dir, a.vm).unwrap(), None);
        write(&dir, &a).unwrap();
        write(&dir, &b).unwrap();
        let mode = std::fs::metadata(record_path(&dir, a.vm))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(record_path(&dir, a.vm)).unwrap()).unwrap();
        assert_eq!(json["role"], "replica");
        assert_eq!(json["volumes"][0]["state"], "ready");
        assert_eq!(json["volumes"][0]["format"], "qcow2");
        assert_eq!(json["volumes"][0]["overlay"]["received"], false);
        assert_eq!(read(&dir, a.vm).unwrap(), Some(a.clone()));
        let mut got = list(&dir);
        got.sort_by_key(|r| r.vm);
        assert_eq!(got, [a.clone(), b.clone()]);
        // 읽지 못하는 파일, 임시 파일, 겹층 디렉터리는 건너뛴다.
        std::fs::write(dir.join("bad.json"), "{").unwrap();
        std::fs::write(dir.join(".x.json.tmp"), "{").unwrap();
        std::fs::create_dir_all(overlay_dir(&dir)).unwrap();
        std::fs::write(overlay_path(&dir, Uuid::now_v7(), Uuid::now_v7()), "").unwrap();
        remove(&dir, a.vm).unwrap();
        remove(&dir, a.vm).unwrap();
        assert_eq!(list(&dir), [b]);
    }

    #[test]
    fn a_primary_record_omits_what_it_does_not_have() {
        let vm = Uuid::now_v7();
        let mut r = record(vm);
        r.role = Role::Primary;
        r.generation = None;
        r.synced_at = None;
        r.volumes[0].overlay = None;
        r.volumes[0].created = false;
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "vm": vm,
                "role": "primary",
                "volumes": [{
                    "id": r.volumes[0].id,
                    "pool": "b",
                    "format": "qcow2",
                    "size_bytes": 1u64 << 30,
                    "created": false,
                    "state": "ready"
                }]
            })
        );
        assert_eq!(serde_json::from_value::<ReplicaRecord>(json).unwrap(), r);
    }

    #[test]
    fn overlays_live_under_the_record_directory() {
        let (run, volume) = (Uuid::now_v7(), Uuid::now_v7());
        assert_eq!(
            overlay_path(Path::new("/d/replicas"), run, volume),
            PathBuf::from(format!("/d/replicas/overlays/{run}-{volume}.qcow2"))
        );
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("o.qcow2");
        discard_overlay(&path).unwrap();
        std::fs::write(&path, "").unwrap();
        discard_overlay(&path).unwrap();
        assert!(!path.exists());
        assert!(free_bytes(root.path()).unwrap() > 0);
    }

    #[tokio::test]
    async fn the_lock_is_one_per_directory() {
        let root = tempfile::TempDir::new().unwrap();
        let (a, b) = (root.path().join("a"), root.path().join("b"));
        let held = lock(&a).await;
        assert_eq!(held.dir(), a);
        drop(lock(&b).await);
        let waiting = tokio::spawn({
            let a = a.clone();
            async move { lock(&a).await.dir().to_path_buf() }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!waiting.is_finished());
        drop(held);
        assert_eq!(waiting.await.unwrap(), a);
    }
}
