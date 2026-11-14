//! 마이그레이션 디스크 기록 파일(설계 §3.6): 로컬 디스크를 복사하는 마이그레이션에서 이 노드가 만들었거나
//! (대상, `target`) 확정 뒤 지울(원본, `source`) 볼륨. `{data_dir}/migration-disks/{task}.json`(0600,
//! 쓰고 fsync). 이 노드에서 볼륨 파일을 지우는 유일한 근거다: Raft 기록만 보고 지우면 잘못 설정한
//! 풀(`shared = false`인데 실제로 같은 경로)이나 한 호스트의 두 노드에서 남의 디스크를 지울 수 있다.
//! 기록을 정리하거나 받는 쪽이 볼륨을 만드는 동안은 이 노드의 잠금([`lock`])을 쥔다.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_types::runtime::ImageFormat;
use opl_types::storage::{StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{Vm, VmSpec};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 기록 파일 하나: 마이그레이션 Task 하나에서 이 노드가 맡은 볼륨들
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DiskRecord {
    pub vm: Uuid,
    pub task: Uuid,
    pub role: Role,
    pub volumes: Vec<RecordedVolume>,
}

/// 이 노드가 마이그레이션에서 맡은 쪽
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Role {
    /// 보내는 노드: 확정 뒤 지울 원본 볼륨
    Source,
    /// 받는 노드: 새로 만든 볼륨
    Target,
}

/// 이 노드의 볼륨 파일 하나(풀 안에서 id로 찾는다)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecordedVolume {
    pub id: Uuid,
    pub pool: String,
    pub format: ImageFormat,
    /// 받는 쪽(`target`)이 이 볼륨을 만들었다(`create_volume`이 성공한 뒤 다시 적는다). 만들기 전에 적은
    /// 항목은 이미 있던 남의 파일일 수 있으므로 지우지 않는다. 원본 쪽(`source`)은 원래 있던 볼륨이라
    /// 보지 않는다.
    #[serde(default)]
    pub created: bool,
}

impl RecordedVolume {
    /// 정리·되돌리기가 이 항목의 파일을 지울 수 있는지
    fn deletable(&self, role: Role) -> bool {
        role == Role::Source || self.created
    }
}

/// 이 노드의 기록 파일을 다루는 동안 쥔 잠금([`lock`]). 그 디렉터리를 알려 준다.
pub(crate) struct Records {
    dir: PathBuf,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl Records {
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }
}

/// 기록 디렉터리 `dir`의 잠금을 기다려 쥔다. 디렉터리마다 하나이므로 같은 디렉터리를 쓰는 받는 쪽과 Task
/// 실행기(정리, 원본의 마무리)가 함께 쓴다: 받기는 남은 기록 정리부터 받는 QEMU를 띄울 때까지, 취소는
/// 끝날 때까지, 정리는 QEMU 확인부터 지우기까지 쥔다(정리가 막 만든 같은 id의 볼륨을 지우지 않게).
pub(crate) async fn lock(dir: &Path) -> Records {
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
        _guard: lock.lock_owned().await,
    }
}

fn record_path(dir: &Path, task: Uuid) -> PathBuf {
    dir.join(format!("{task}.json"))
}

/// Task `task`의 기록. 없거나 읽지 못하면 `None`이다.
pub(crate) fn load(dir: &Path, task: Uuid) -> Option<DiskRecord> {
    let data = std::fs::read(record_path(dir, task)).ok()?;
    serde_json::from_slice(&data).ok()
}

/// 기록을 임시 파일에 써서 디스크에 내린 뒤 바꿔 넣는다(디렉터리도 내린다).
pub(crate) fn write(dir: &Path, record: &DiskRecord) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let path = record_path(dir, record.task);
    let tmp = dir.join(format!(".{}.json.tmp", record.task));
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

/// Task `task`의 기록을 지운다. 없으면 할 일이 없다.
pub(crate) fn remove(dir: &Path, task: Uuid) -> std::io::Result<()> {
    match std::fs::remove_file(record_path(dir, task)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 이 노드의 기록들. 읽지 못한 파일은 경고만 남기고 건너뛴다.
pub(crate) fn list(dir: &Path) -> Vec<DiskRecord> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot read migration disk records");
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
        if !is_record {
            continue;
        }
        let read = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|data| serde_json::from_slice(&data).map_err(|e| e.to_string()));
        match read {
            Ok(record) => records.push(record),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read a migration disk record")
            }
        }
    }
    records
}

/// 기록 하나를 어떻게 정리할지([`decide`])
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Cleanup {
    /// 그 마이그레이션이 아직 끝나지 않았다.
    Keep,
    /// 기록의 볼륨이 이 노드의 진짜 디스크다(되돌려진 원본, 확정된 대상): 기록만 지운다.
    Forget,
    /// 이 볼륨들을 지운 뒤 기록을 지운다(나머지는 이 노드의 진짜 디스크다). 그 VM의 QEMU가 이 노드에
    /// 있으면 다음에 본다.
    Delete(Vec<RecordedVolume>),
    /// VM이 마이그레이션 중이 아닌 채 이 노드에 있고 이 볼륨들은 볼륨 기록의 지금 위치가 아니다: 도는
    /// VM이 쓸 수 없는 위치이므로 QEMU를 보지 않고 지운다.
    DeleteUnused(Vec<RecordedVolume>),
}

/// 정리 규칙(설계 §3.6). VM에 이 기록의 마이그레이션이 끝나지 않았으면(그 Task가 `abandoned`가
/// 아니면 — 원본이 버린다고 알린 마이그레이션은 이 노드에 표시를 지운 기록이 아직 오지 않았어도 끝난
/// 것이다) 그대로 둔다. 아니면 볼륨 기록의 위치가 (이 노드, 기록의 풀)인 항목은 이 노드의 진짜 디스크이고,
/// 다른 항목은 남은 사본이다: 원본이면 다른 노드로 확정됐고, 대상이면 되돌려졌거나 확정 전에 어느 쪽이
/// 죽었다. VM이 이 노드에 있어도 다른 풀로 옮겨 왔으면 그렇다(VM이 지워졌거나 볼륨 리소스가 없으면
/// 고아다). 남은 사본을 지운다. 다만 VM이 이 노드에 있고 다른 마이그레이션 중이면 끝난 뒤에 본다.
/// VM이 마이그레이션 중이 아닌 채 이 노드에 있고 지울 항목의 볼륨 기록이 모두 있으면(다른 위치를 가리킨다)
/// QEMU를 보지 않고 지운다([`Cleanup::DeleteUnused`], VM이 도는 동안에도).
/// 받는 쪽이 만들었다고 적지 않은 항목(`created`가 아님)은 남의 파일일 수 있으므로 지우지 않고 기록에서
/// 뺀다(새는 쪽을 고른다).
pub(crate) fn decide(
    record: &DiskRecord,
    me: Uuid,
    vm: Option<&Vm>,
    volume: impl Fn(Uuid) -> Option<Volume>,
    abandoned: Option<Uuid>,
) -> Cleanup {
    let migrating = vm
        .and_then(|vm| vm.status.migration.as_ref())
        .filter(|m| abandoned != Some(m.task));
    if migrating.is_some_and(|m| m.task == record.task) {
        return Cleanup::Keep;
    }
    let here = |v: &RecordedVolume| {
        volume(v.id).is_some_and(|r| r.status.node == Some(me) && r.spec.pool == v.pool)
    };
    let (delete, foreign): (Vec<_>, Vec<_>) = record
        .volumes
        .iter()
        .filter(|v| !here(v))
        .cloned()
        .partition(|v| v.deletable(record.role));
    let vm_here = vm.is_some_and(|vm| vm.spec.node == Some(me));
    if !delete.is_empty() && vm_here && migrating.is_some() {
        return Cleanup::Keep;
    }
    for v in foreign {
        tracing::warn!(vm = %record.vm, volume = %v.id, pool = %v.pool, "a migration did not create this volume here; forgetting it without deleting");
    }
    if delete.is_empty() {
        Cleanup::Forget
    } else if vm_here && delete.iter().all(|v| volume(v.id).is_some()) {
        Cleanup::DeleteUnused(delete)
    } else {
        Cleanup::Delete(delete)
    }
}

/// 이 노드의 기록(`vm`이 있으면 그 VM의 것만)을 [`decide`]대로 정리한다(`abandoned`는 원본이 버린
/// 마이그레이션 Task). 부르는 쪽이 잠금(`records`)을 쥔다. 지울 볼륨이 있는데 그 VM의
/// QEMU가 이 노드에 아직 있으면(끄지 못한 원본, 받는 QEMU) 다음에 다시 본다(쓰일 수 없는 위치의
/// 사본([`Cleanup::DeleteUnused`])은 보지 않는다). 지우기 직전에 기록 파일과
/// 그 항목이 아직 있는지 다시 본다. 볼륨을 다 지웠을 때만
/// 기록을 지운다. 실패는 경고만 남긴다(다음 정리가 다시 한다).
pub(crate) async fn clean_up(
    cluster: &Cluster,
    storage: &dyn Storage,
    hv: &dyn Hypervisor,
    me: Uuid,
    records: &Records,
    vm: Option<Uuid>,
    abandoned: Option<Uuid>,
) {
    let dir = records.dir();
    for record in list(dir) {
        if vm.is_some_and(|vm| vm != record.vm) {
            continue;
        }
        let read = || -> Result<(Option<Vm>, Vec<Volume>), ClusterError> {
            let current = cluster.get::<VmSpec>(record.vm)?;
            let mut volumes = Vec::new();
            for v in &record.volumes {
                volumes.extend(cluster.get::<VolumeSpec>(v.id)?);
            }
            Ok((current, volumes))
        };
        let (current, volumes) = match read() {
            Ok(read) => read,
            Err(e) => {
                tracing::warn!(vm = %record.vm, error = %e, "cannot read VM");
                continue;
            }
        };
        let volume = |id| volumes.iter().find(|v| v.id == id).cloned();
        let (mut delete, unused) = match decide(&record, me, current.as_ref(), volume, abandoned) {
            Cleanup::Keep => continue,
            Cleanup::Forget => (Vec::new(), false),
            Cleanup::Delete(delete) => (delete, false),
            Cleanup::DeleteUnused(delete) => (delete, true),
        };
        if !delete.is_empty() {
            // 쓰일 수 없는 위치의 사본이면 QEMU를 보지 않는다.
            let qemu = if unused {
                Ok(None)
            } else {
                hv.inspect(record.vm).await
            };
            match qemu {
                Ok(None) => {}
                Ok(Some(_)) => {
                    tracing::info!(vm = %record.vm, "a QEMU of the VM still runs; deleting its copied disks later");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(vm = %record.vm, error = %e, "cannot inspect VM");
                    continue;
                }
            }
            // 그 사이 기록이 바뀌었으면(받기가 다시 적었거나 지웠다) 남은 항목만 지운다.
            let Some(now) = load(dir, record.task) else {
                continue;
            };
            delete = still_recorded(&now, delete);
            tracing::info!(vm = %record.vm, task = %record.task, "deleting the disk copies of a migration");
        }
        if delete_volumes(cluster, storage, record.vm, &delete).await
            && let Err(e) = remove(dir, record.task)
        {
            tracing::warn!(vm = %record.vm, error = %e, "cannot remove a migration disk record");
        }
    }
}

/// `delete` 가운데 기록 `now`에 아직 (지울 수 있게) 적힌 항목
fn still_recorded(now: &DiskRecord, delete: Vec<RecordedVolume>) -> Vec<RecordedVolume> {
    delete
        .into_iter()
        .filter(|v| {
            now.volumes
                .iter()
                .any(|n| n.id == v.id && n.pool == v.pool && n.deletable(now.role))
        })
        .collect()
}

/// 이 노드의 볼륨 파일들을 끄고 지운다. 없는 것은 지운 것으로 본다. 모두 지웠으면 참이다.
pub(crate) async fn delete_volumes(
    cluster: &Cluster,
    storage: &dyn Storage,
    vm: Uuid,
    volumes: &[RecordedVolume],
) -> bool {
    let mut done = true;
    for v in volumes {
        let pool = match cluster.get_by_name::<StoragePoolSpec>(&v.pool) {
            Ok(Some(pool)) => pool,
            // 풀이 없으면 파일도 찾을 수 없다.
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot read storage pool");
                done = false;
                continue;
            }
        };
        if let Err(e) = storage.deactivate_volume(&pool, v.id, v.format).await {
            tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot deactivate volume");
        }
        match storage.delete_volume(&pool, v.id, v.format).await {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => {
                tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot delete a disk copy");
                done = false;
            }
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use opl_types::resource::Resource;
    use opl_types::vm::{CpuSpec, MachineType, MigrationStatus, PowerState, VmSpec};

    use super::*;

    fn vm(node: Uuid) -> Vm {
        Resource::new(
            "web",
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 1,
                    model: Default::default(),
                },
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics: vec![],
                power: PowerState::Running,
                restart: Default::default(),
                node: Some(node),
                snapshot_schedule: None,
                ha: None,
                placement: None,
            },
        )
    }

    fn volume(pool: &str, node: Uuid) -> Volume {
        let mut v: Volume = Resource::new(
            "root",
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1 << 30,
                format: ImageFormat::Qcow2,
            },
        );
        v.status.node = Some(node);
        v
    }

    fn record(role: Role, volume: &Volume) -> DiskRecord {
        DiskRecord {
            vm: Uuid::now_v7(),
            task: Uuid::now_v7(),
            role,
            volumes: vec![RecordedVolume {
                id: volume.id,
                pool: "local".into(),
                format: ImageFormat::Qcow2,
                created: role == Role::Target,
            }],
        }
    }

    #[test]
    fn records_round_trip_through_the_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let dir = dir.path().join("migration-disks");
        assert!(list(&dir).is_empty());
        let a = record(Role::Source, &volume("local", Uuid::now_v7()));
        let b = record(Role::Target, &volume("local", Uuid::now_v7()));
        write(&dir, &a).unwrap();
        write(&dir, &b).unwrap();
        let mode = std::fs::metadata(record_path(&dir, a.task))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(record_path(&dir, b.task)).unwrap()).unwrap();
        assert_eq!(json["role"], "target");
        assert_eq!(json["volumes"][0]["format"], "qcow2");
        let mut got = list(&dir);
        got.sort_by_key(|r| r.task);
        assert_eq!(got, [a.clone(), b.clone()]);
        // 읽지 못하는 파일과 임시 파일은 건너뛴다.
        std::fs::write(dir.join("bad.json"), "{").unwrap();
        std::fs::write(dir.join(".x.json.tmp"), "{").unwrap();
        remove(&dir, a.task).unwrap();
        remove(&dir, a.task).unwrap();
        assert_eq!(list(&dir), [b]);
    }

    #[test]
    fn unfinished_migrations_are_kept() {
        let (me, other) = (Uuid::now_v7(), Uuid::now_v7());
        for role in [Role::Source, Role::Target] {
            let v = volume("local", other);
            let r = record(role, &v);
            let mut current = vm(other);
            current.status.migration = Some(MigrationStatus {
                node: me,
                task: r.task,
                disks: vec![],
            });
            assert_eq!(
                decide(&r, me, Some(&current), |_| Some(v.clone()), None),
                Cleanup::Keep
            );
            // 원본이 버린다고 알렸으면 이 노드의 기록이 늦어도 끝난 것이다.
            assert_eq!(
                decide(&r, me, Some(&current), |_| Some(v.clone()), Some(r.task)),
                Cleanup::Delete(r.volumes.clone())
            );
            // 같은 VM의 다른(새) 마이그레이션은 이 기록의 것이 아니다.
            current.status.migration.as_mut().unwrap().task = Uuid::now_v7();
            assert_eq!(
                decide(&r, me, Some(&current), |_| Some(v.clone()), None),
                Cleanup::Delete(r.volumes.clone())
            );
        }
    }

    #[test]
    fn target_copies_are_deleted_unless_the_vm_came_here() {
        let (me, source) = (Uuid::now_v7(), Uuid::now_v7());
        // 되돌려졌다: VM과 볼륨이 원본에 있다.
        let v = volume("local", source);
        let r = record(Role::Target, &v);
        assert_eq!(
            decide(&r, me, Some(&vm(source)), |_| Some(v.clone()), None),
            Cleanup::Delete(r.volumes.clone())
        );
        // 확정됐고 볼륨 기록도 이 노드의 그 풀이다.
        let here = volume("local", me);
        assert_eq!(
            decide(&r, me, Some(&vm(me)), |_| Some(here.clone()), None),
            Cleanup::Forget
        );
        // VM이 지워졌지만 볼륨은 이 노드의 그 풀에 있다(확정 뒤 VM만 지움).
        assert_eq!(
            decide(&r, me, None, |_| Some(here.clone()), None),
            Cleanup::Forget
        );
        // 볼륨은 이 노드에 있지만 다른 풀이다: 기록의 파일은 진짜 디스크가 아니다.
        let elsewhere = volume("fast", me);
        assert_eq!(
            decide(&r, me, None, |_| Some(elsewhere.clone()), None),
            Cleanup::Delete(r.volumes.clone())
        );
        // VM도 볼륨도 없다(고아).
        assert_eq!(
            decide(&r, me, None, |_| None, None),
            Cleanup::Delete(r.volumes.clone())
        );
    }

    #[test]
    fn source_copies_are_deleted_once_the_vm_left() {
        let (me, target) = (Uuid::now_v7(), Uuid::now_v7());
        // 되돌려졌다: VM이 이 노드에 있다.
        let v = volume("local", me);
        let r = record(Role::Source, &v);
        assert_eq!(
            decide(&r, me, Some(&vm(me)), |_| Some(v.clone()), None),
            Cleanup::Forget
        );
        // 다른 노드로 확정됐고 볼륨 기록도 옮겨졌다.
        let moved = volume("local", target);
        assert_eq!(
            decide(&r, me, Some(&vm(target)), |_| Some(moved.clone()), None),
            Cleanup::Delete(r.volumes.clone())
        );
        // 확정됐는데 볼륨 기록이 아직 이 노드다: 지우지 않는다.
        assert_eq!(
            decide(&r, me, Some(&vm(target)), |_| Some(v.clone()), None),
            Cleanup::Forget
        );
    }

    #[test]
    fn copies_left_in_another_pool_are_deleted_when_the_vm_is_here() {
        let me = Uuid::now_v7();
        // 이 노드로 다른 풀(`fast`)에 옮겨 왔다: 기록의 `local` 사본은 남은 것이고 도는 VM이 쓸 수 없다.
        let v = volume("fast", me);
        for role in [Role::Source, Role::Target] {
            let r = record(role, &v);
            assert_eq!(
                decide(&r, me, Some(&vm(me)), |_| Some(v.clone()), None),
                Cleanup::DeleteUnused(r.volumes.clone())
            );
            // 볼륨 기록이 없으면 지금 위치를 모르므로 QEMU를 본 뒤에 지운다.
            assert_eq!(
                decide(&r, me, Some(&vm(me)), |_| None, None),
                Cleanup::Delete(r.volumes.clone())
            );
            // 다른 마이그레이션이 끝나지 않았으면 끝난 뒤에 본다.
            let mut moving = vm(me);
            moving.status.migration = Some(MigrationStatus {
                node: Uuid::now_v7(),
                task: Uuid::now_v7(),
                disks: vec![],
            });
            assert_eq!(
                decide(&r, me, Some(&moving), |_| Some(v.clone()), None),
                Cleanup::Keep
            );
        }
    }

    #[test]
    fn target_entries_not_created_here_are_never_deleted() {
        let (me, source) = (Uuid::now_v7(), Uuid::now_v7());
        let v = volume("local", source);
        let mut r = record(Role::Target, &v);
        // 만들기 전에 적었다(만들다 죽었거나 이미 있었다).
        r.volumes[0].created = false;
        for current in [Some(vm(source)), Some(vm(me)), None] {
            assert_eq!(
                decide(&r, me, current.as_ref(), |_| Some(v.clone()), None),
                Cleanup::Forget
            );
        }
        // 만든 항목만 지운다.
        let other = volume("local", source);
        r.volumes.push(RecordedVolume {
            id: other.id,
            pool: "local".into(),
            format: ImageFormat::Qcow2,
            created: true,
        });
        assert_eq!(
            decide(&r, me, Some(&vm(source)), |_| Some(v.clone()), None),
            Cleanup::Delete(vec![r.volumes[1].clone()])
        );
        // 옛 기록(`created` 없음)의 항목은 만들지 않은 것으로 읽는다.
        let json = serde_json::json!({ "id": v.id, "pool": "local", "format": "qcow2" });
        let old: RecordedVolume = serde_json::from_value(json).unwrap();
        assert!(!old.created);
    }

    #[test]
    fn only_entries_still_recorded_are_deleted() {
        let v = volume("local", Uuid::now_v7());
        let r = record(Role::Target, &v);
        assert_eq!(still_recorded(&r, r.volumes.clone()), r.volumes);
        let mut now = r.clone();
        now.volumes[0].created = false;
        assert!(still_recorded(&now, r.volumes.clone()).is_empty());
        now.volumes.clear();
        assert!(still_recorded(&now, r.volumes.clone()).is_empty());
    }

    #[tokio::test]
    async fn the_lock_is_one_per_directory() {
        let root = tempfile::TempDir::new().unwrap();
        let (a, b) = (root.path().join("a"), root.path().join("b"));
        let held = lock(&a).await;
        assert_eq!(held.dir(), a);
        // 다른 디렉터리(다른 노드)는 기다리지 않는다.
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
