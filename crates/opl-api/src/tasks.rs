//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

pub(crate) mod backup;
pub(crate) mod backup_store;
mod ceph;
mod clone;
mod download;
mod drain;
pub(crate) mod file_restore;
mod firmware;
mod hotplug;
mod import;
pub(crate) mod live_restore;
mod migrate;
pub(crate) mod network_migrate;
mod replicate;
pub(crate) mod sdn;
mod snapshot;
mod store_tasks;
mod template;
mod vm_replicate;

pub(crate) use clone::{LINKED_ONLY_FROM_TEMPLATES, unlinkable, volume_name};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::ceph::CephHost;
use opl_driver::ovn::OvnHost;
use opl_driver::{DriverError, Hypervisor, ISO_HEADER_LEN, Storage, is_iso_image};
use opl_peer::Peers;
use opl_reconciler::{FirewallGate, FirmwareFetcher, ReconcilerHandle};
use opl_types::backup::{BackupPhase, BackupSpec};
use opl_types::resource::{Kind, Resource};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
    is_shared, pool_listed_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::state::AppState;
use crate::updates::Updates;
use crate::vm_imports::ImportCredentials;
use crate::vms::{is_stopped, vm_uses_volume};
use crate::volumes::{block_pool_kind, usable_node};

/// Task를 만든 API 요청의 사용자 id를 담는 레이블(요청 밖에서 만든 Task — 스케줄러·HA·정리 작업 — 에는 없다)
pub const REQUESTED_BY: &str = "requested-by";

/// 새 Task를 저장소에 만든다. 이름은 id 문자열이다. API 요청 안이면 요청한 사용자를 [`REQUESTED_BY`] 레이블에 둔다.
pub(crate) async fn create_task(
    cluster: &Cluster,
    op: TaskOp,
    target: Uuid,
    node: Uuid,
) -> Result<Task, ApiError> {
    // 템플릿 Task와 새 칸은 옛 노드가 풀지 못한다(계획 58). 경로마다 검증 전에 보지만, 빠뜨린 길이 없게 여기서도 본다.
    if op.needs_templates() {
        crate::cluster::require_feature(cluster, opl_types::node::TEMPLATES)?;
    }
    // 펌웨어 초기화 Task는 옛 노드가 풀지 못한다(계획 59).
    if op.needs_vtpm() {
        crate::cluster::require_feature(cluster, opl_types::node::VTPM)?;
    }
    // 잠금·옮기기 Task도 옛 노드가 풀지 못한다(계획 67).
    if op.needs_sdn_services() {
        crate::cluster::require_feature(cluster, opl_types::node::SDN_SERVICES)?;
    }
    // 파일 복원 Task도 옛 노드가 풀지 못한다(계획 69).
    if op.needs_file_restore() {
        crate::cluster::require_feature(cluster, opl_types::node::FILE_RESTORE)?;
    }
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    // 요청한 사용자(리뷰 2 N7). 레이블이라 옛 노드도 그대로 읽는다(`TaskSpec`은 모르는 칸을 거절한다).
    if let Some(user) = crate::audit::requester() {
        task.labels
            .insert(REQUESTED_BY.to_string(), user.to_string());
    }
    task.revision = cluster.create(&task).await?;
    Ok(task)
}

#[derive(Clone)]
pub struct TaskRunnerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 Task만 실행한다.
    pub node: Uuid,
    /// Task 하나가 이보다 오래 걸리면 실패로 끝낸다.
    pub timeout: Duration,
    /// 저장소 쓰기가 실패했을 때 다시 시도하는 간격, VM 상태를 다시 확인하는 간격
    pub poll_interval: Duration,
    /// API가 올린 이미지를 받아 두는 디렉터리. 시작할 때 Task가 없는 파일을 지운다.
    pub upload_dir: Option<PathBuf>,
    /// VM별 상태 디렉터리의 뿌리(`QemuConfig::state_dir`). 스냅샷의 UEFI 변수 사본을 둔다.
    /// 없으면 변수를 복사하거나 되돌리지 않는다.
    pub state_dir: Option<PathBuf>,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 백업 Task가 백업 풀의 파일 경로를 정한다.
    pub mount_root: PathBuf,
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨, 시간을 넘긴 블록 스냅샷이 남긴 태그를
    /// 다시 지워 보는 간격
    pub orphan_retry: Duration,
    /// 블록 스냅샷이 남긴 태그, 이 노드에서 켜진 채 남은 볼륨을 다시 찾아 정리하는 간격(시작할 때
    /// 한 번 찾고 그 뒤로). 확정되지 않았거나 확정 뒤 남은 마이그레이션 QEMU도 이 간격으로 정리한다.
    pub tag_sweep: Duration,
    /// 라이브 마이그레이션을 보낼 때 대상 노드를 부르는 노드 간 클라이언트. 없으면 마이그레이션 Task는
    /// 실패한다.
    pub peers: Option<Arc<Peers>>,
    /// 마이그레이션을 보내는 대역폭 상한(바이트/초). `None`이면 QEMU 기본값
    pub migration_bandwidth: Option<u64>,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다. 마이그레이션 Task는 `timeout` 대신 이것을 쓴다.
    pub migration_timeout: Duration,
    /// 로컬 디스크를 복사하는 마이그레이션에서 디스크 미러가 모두 따라잡기(`ready`)까지의 한도
    /// (`[migration] disk_timeout`). 메모리 단계는 `migration_timeout`이다.
    pub migration_disk_timeout: Duration,
    /// 마이그레이션 디스크 기록 파일의 디렉터리(opld의 `{data_dir}/migration-disks`). 받는 쪽
    /// (`IncomingConfig::migration_disks`)과 같다. 없으면 로컬 디스크를 복사하는 마이그레이션은 실패한다.
    pub migration_disks: Option<PathBuf>,
    /// 스냅샷·복제가 UEFI 변수를 복사하기 전에, 변수 파일이 다른 노드에 있으면 받아 온다. 없으면
    /// 이 노드의 파일을 그대로 쓴다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
    /// 이 노드가 받은 ESXi 가져오기의 자격 증명. 가져오기 Task가 쓰고, 끝나거나 지우면 지운다. API
    /// (`AppState::import_credentials`)와 같은 것을 넘긴다.
    pub import_credentials: Arc<ImportCredentials>,
    /// 이 노드의 업데이트 확인. 없으면(업데이트가 꺼진 노드) 업데이트 확인 Task는 실패한다.
    pub updates: Option<Arc<Updates>>,
    /// 이 노드의 API 상태. 노드 비우기 Task가 마이그레이션 API와 같은 확인으로 Task를 만들 때 쓴다
    /// (이 노드의 API와 `vm_lock`을 함께 쓴다). 없으면 비우기 Task는 실패한다.
    pub api: Option<Arc<AppState>>,
    /// 핫플러그 Task가 떼기·줄이기에서 게스트가 내려 주기를 기다리는 한도(기본 60초)
    pub hotplug_timeout: Duration,
    /// 핫플러그 Task가 보안 그룹을 건 NIC를 붙이기 전에 이 노드의 방화벽 규칙을 넣는다(reconciler가 켤 때와 같음).
    /// 없으면 그런 NIC는 붙이지 않는다.
    pub firewall: Option<Arc<dyn FirewallGate>>,
    /// 복제 Task가 보내는 대역폭 상한(바이트/초, 디스크 수로 나눈다). `None`이면 제한 없음(`[replication] bandwidth_mib`)
    pub replication_bandwidth: Option<u64>,
    /// 복제 Task가 보낸 양의 합이 이 시간 동안 늘지 않으면 실패한다(끊긴 링크). 복제 Task는 시간 제한이 없다.
    pub replication_stall: Duration,
    /// 복제본 기록 파일의 디렉터리(opld의 `{data_dir}/replicas`, 받는 쪽·reconciler와 같다). 없으면 복제 Task는
    /// 실패한다.
    pub replica_dir: Option<PathBuf>,
    /// 이 노드에서 Ceph를 세우는 단계들(opld `[ceph]`). 없으면 Ceph Task는 실패한다.
    pub ceph: Option<Arc<CephHost>>,
    /// 이 노드에서 OVN을 세우는 단계들(opld `[sdn]`, 계획 65). 없으면 SDN Task는 실패한다.
    pub sdn: Option<Arc<OvnHost>>,
    /// URL에서 받기(`volume-download`)가 루프백·링크 로컬·사설망 주소에서도 받는지(opld `[download] allow_private`,
    /// 기본 false — 노드 안쪽이나 클라우드 메타데이터에 닿지 않게)
    pub download_allow_private: bool,
    /// 지운 VM의 상태 디렉터리(`{state_dir}/vm/{id}`)는 마지막으로 바뀐 지 이만큼 지나야 지운다(계획 59 §10, 기본
    /// 1시간). 정리는 `tag_sweep`마다 돈다.
    pub state_dir_min_age: Duration,
    /// 이 노드의 CPU 다시 읽기(계획 63). 없으면 `node-cpu-probe` Task는 실패한다.
    pub cpu: Option<Arc<dyn crate::cpu::CpuRefresher>>,
    /// 네트워크 옮기기(계획 67)가 VM 하나의 끄기·켜기·핫플러그를 기다리는 한도(기본 15분 — 시험은 줄인다)
    pub migration_vm_wait: Duration,
    /// 꺼진 VM·옛 백업을 저장소로 읽는 `qemu-nbd`(계획 68 §8.2)
    pub qemu_nbd: PathBuf,
    /// 저장소 백업·복원의 유닉스 소켓 디렉터리(경로 길이 한도 안 — opld는 `{data_dir}/run`)
    pub backup_run_dir: PathBuf,
    /// fleecing 시작 전 남아 있어야 할 공간의 최소(디스크 크기 합의 10%와 큰 쪽, `[backup] fleece_reserve_min_mib`)
    pub fleece_reserve_min: u64,
    /// fleecing 지킴이가 읽기를 끊는 남은 공간(`[backup] fleece_min_free_mib`)
    pub fleece_min_free: u64,
    /// 저장소 복원 NBD 서버의 풀린 청크 캐시(MiB, `[backup] serve_cache_mib`)
    pub serve_cache_mib: u64,
    /// 저장소 쓰기 잠금·GC 기다림을 다시 보는 간격(기본 5초)
    pub store_poll: Duration,
    /// GC가 시작 때 진행 중이던 백업을 기다리는 한도(기본 24시간 — 넘으면 아무것도 지우지 않고 실패)
    pub gc_wait_max: Duration,
    /// 라이브 복원 오버레이를 만드는 `qemu-img`(계획 68 §11.2)
    pub qemu_img: PathBuf,
    /// 라이브 복원이 저장소 백업을 내보내는 `backup-serve` 헬퍼의 실행 파일(opld 자신). 없으면 opld 안에서 내보낸다
    /// (시험 — opld가 다시 뜨면 Task가 다시 띄운다).
    pub backup_serve_exe: Option<PathBuf>,
    /// 헬퍼를 systemd scope(`opl-bkserve-<id>`)로 감싸 opld가 다시 떠도 살게 한다(systemd가 있는 Linux).
    pub backup_serve_scope: bool,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
            mount_root: PathBuf::new(),
            orphan_retry: Duration::from_secs(60),
            tag_sweep: Duration::from_secs(30 * 60),
            peers: None,
            migration_bandwidth: None,
            migration_timeout: Duration::from_secs(30 * 60),
            migration_disk_timeout: Duration::from_secs(12 * 3600),
            migration_disks: None,
            firmware: None,
            import_credentials: Arc::default(),
            updates: None,
            api: None,
            hotplug_timeout: Duration::from_secs(60),
            firewall: None,
            replication_bandwidth: None,
            replication_stall: migrate::DISK_STALL,
            replica_dir: None,
            ceph: None,
            sdn: None,
            download_allow_private: false,
            state_dir_min_age: firmware::STATE_DIR_MIN_AGE,
            cpu: None,
            migration_vm_wait: network_migrate::VM_WAIT,
            qemu_nbd: PathBuf::from("qemu-nbd"),
            backup_run_dir: std::env::temp_dir(),
            fleece_reserve_min: 2 << 30,
            fleece_min_free: 1 << 30,
            serve_cache_mib: opl_store::nbd::serve::DEFAULT_CACHE_MIB,
            store_poll: Duration::from_secs(5),
            gc_wait_max: Duration::from_secs(24 * 3600),
            qemu_img: PathBuf::from("qemu-img"),
            backup_serve_exe: None,
            backup_serve_scope: false,
        }
    }
}

pub struct TaskRunner {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Ctx {
    cluster: Arc<Cluster>,
    storage: Arc<dyn Storage>,
    hv: Arc<dyn Hypervisor>,
    reconciler: ReconcilerHandle,
    config: TaskRunnerConfig,
    /// 실행 중인 복제 Task가 만든 것(Task id별). 실패하면 정리가 지운다.
    cloned: std::sync::Mutex<HashMap<Uuid, clone::Made>>,
    /// 라이브 복원이 opld 안에서 내보내는 저장소 백업(VM별 — `backup-serve` 헬퍼를 쓰지 않을 때, 계획 68)
    live_serving: std::sync::Mutex<HashMap<Uuid, backup_store::Serving>>,
    /// 실행기가 멈췄는지. 뒤에서 도는 일(남은 태그 지우기)이 보고 함께 멈춘다.
    stopped: watch::Sender<bool>,
}

impl TaskRunner {
    /// 고아(Task 없는 Pending 볼륨·스냅샷, 올린 파일)를 지운 뒤 실행 루프를 띄우고 돌아온다.
    /// API가 막 만든 볼륨이나 올린 파일을 고아로 지우지 않도록 API 서버는 이것을 기다린 뒤 띄운다.
    /// `hv`는 실행 중인 VM의 스냅샷을 다룬다(이 노드의 reconciler와 같은 하이퍼바이저).
    pub async fn spawn(
        cluster: Arc<Cluster>,
        storage: Arc<dyn Storage>,
        hv: Arc<dyn Hypervisor>,
        reconciler: ReconcilerHandle,
        config: TaskRunnerConfig,
    ) -> Self {
        let ctx = Arc::new(Ctx {
            cluster,
            storage,
            hv,
            reconciler,
            config,
            cloned: Default::default(),
            live_serving: Default::default(),
            stopped: watch::channel(false).0,
        });
        let (revision, tasks) = loop {
            match ctx.cluster.list::<TaskSpec>() {
                Ok(list) => break list,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot list tasks; will retry");
                    tokio::time::sleep(ctx.config.poll_interval).await;
                }
            }
        };
        let unmounted = sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx, unmounted)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// `unmounted`는 풀이 마운트되지 않아 지우지 못한 고아 볼륨이다. 따로 다시 지워 본다.
async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>, unmounted: Vec<Uuid>) {
    let retry = tokio::spawn(retry_orphans(ctx.clone(), unmounted));
    let tags = tokio::spawn(sweep_block_tags_every(ctx.clone()));
    let idle = tokio::spawn(deactivate_idle_volumes_every(ctx.clone()));
    let migrations = tokio::spawn(migrate::sweep_every(ctx.clone()));
    let state_dirs = tokio::spawn(firmware::sweep_state_dirs_every(ctx.clone()));
    // 실행을 시작한 Task. 목록을 다시 읽을 때 같은 Task를 두 번 시작하지 않게 한다.
    let mut started: HashSet<Uuid> = HashSet::new();
    // 실행 중인 Task. 실행기가 멈출 때 함께 멈춘다.
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 줄(볼륨, VM)별 마지막 Task. 같은 줄의 Task는 앞의 것이 끝난 뒤에 시작한다(`queue`).
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
    let mut first = true;
    'sync: loop {
        let (revision, tasks) = match ctx.cluster.list::<TaskSpec>() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list tasks; will retry");
                tokio::select! {
                    _ = &mut shutdown => break,
                    () = tokio::time::sleep(ctx.config.poll_interval) => continue,
                }
            }
        };
        running.retain(|_, h| !h.is_finished());
        by_target.retain(|_, h| !h.is_finished());
        started.retain(|id| {
            tasks
                .iter()
                .any(|t| t.id == *id && !t.status.state.is_finished())
        });
        for task in tasks {
            if task.spec.node != ctx.config.node || started.contains(&task.id) {
                continue;
            }
            match task.status.state {
                TaskState::Pending => start(
                    &ctx,
                    &mut started,
                    &mut running,
                    &mut by_target,
                    task,
                    false,
                ),
                // 이전 opld가 실행하다 내려갔다.
                TaskState::Running if first => {
                    start(&ctx, &mut started, &mut running, &mut by_target, task, true)
                }
                _ => {}
            }
        }
        first = false;
        let mut watch = match ctx.cluster.watch(revision) {
            Ok(w) => w,
            Err(_) => continue,
        };
        loop {
            tokio::select! {
                _ = &mut shutdown => break 'sync,
                ev = watch.next() => match ev {
                    Ok(WatchEvent::Put { kind: Kind::Task, resource }) => {
                        let Ok(task) = serde_json::to_value(resource).and_then(serde_json::from_value::<Task>)
                        else {
                            continue;
                        };
                        if task.spec.node == ctx.config.node
                            && task.status.state == TaskState::Pending
                            && !started.contains(&task.id)
                        {
                            start(&ctx, &mut started, &mut running, &mut by_target, task, false);
                        }
                    }
                    Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                    Ok(_) => {}
                },
            }
        }
    }
    for handle in running.values() {
        handle.abort();
    }
    retry.abort();
    tags.abort();
    idle.abort();
    migrations.abort();
    state_dirs.abort();
    ctx.stopped.send_replace(true);
}

/// 공유 풀에서 남은 태그로 보고 지우려면 스냅샷이 만들어진 뒤 지나야 하는 시간. 다른 노드의 스냅샷
/// Task가 막 찍은 태그를 그 리소스보다 먼저 보고 지우지 않게 한다.
const SHARED_TAG_MIN_AGE: Duration = Duration::from_secs(30 * 60);

/// 남은 블록 스냅샷 태그를 시작할 때와 그 뒤 `tag_sweep`마다 지운다([`sweep_block_tags`]). 실행기가
/// 멈추면 함께 멈춘다.
async fn sweep_block_tags_every(ctx: Arc<Ctx>) {
    loop {
        sweep_block_tags(ctx.clone()).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 시간을 넘긴 블록 스냅샷이 남긴 태그를 지운다: 이 노드가 맡는 블록 풀(LVM-thin, ZFS, RBD) 볼륨의
/// 태그 중 UUID 모양인데 그 id의 스냅샷 리소스가 없는 것. 스냅샷 Task는 리소스를 만든 뒤 태그를
/// 찍고 태그를 지운 뒤 리소스를 지우므로 그런 태그는 남은 것이다. 백업의 임시 스냅샷(백업 id의 simple
/// 문자열)도 같은 모양이므로, 백업 Task가 끝나지 않은 `pending` 백업의 태그는 둔다. Orbivisor 밖에서 만든 태그는 두고,
/// 스토리지 명령이 느려도 실행기 시작을 막지 않게 뒤에서 돈다.
///
/// 노드 로컬 풀의 볼륨은 그 볼륨을 만든 노드가 맡는다. 공유 풀(RBD)의 볼륨은 어느 노드에서나
/// 보이므로 노드마다 같은 답이 나오게 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드가
/// 맡는다(그런 노드가 없으면 만든 노드). 공유 풀에서는 다른 노드의 Task가 막 찍은 태그일 수 있으므로
/// 만든 지 [`SHARED_TAG_MIN_AGE`]가 지난 태그만 지운다.
async fn sweep_block_tags(ctx: Arc<Ctx>) {
    let volumes = match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => volumes,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list volumes");
            return;
        }
    };
    for volume in volumes {
        if volume.status.phase != VolumePhase::Ready {
            continue;
        }
        let Ok(Some(pool)) = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        else {
            continue;
        };
        if !uses_block_snapshots(&pool) {
            continue;
        }
        let shared = is_shared(&pool);
        let owner = if shared {
            usable_node(&ctx.cluster, &pool, None)
                .ok()
                .flatten()
                .or(volume.status.node)
        } else {
            volume.status.node
        };
        if owner.is_some_and(|n| n != ctx.config.node) {
            continue;
        }
        let format = volume.spec.format;
        let tags = if shared {
            ctx.storage
                .snapshot_tags_older_than(&pool, volume.id, format, SHARED_TAG_MIN_AGE)
                .await
        } else {
            ctx.storage.snapshot_tags(&pool, volume.id, format).await
        };
        let tags = match tags {
            Ok(tags) => tags,
            Err(e) => {
                tracing::debug!(volume = %volume.name, error = %e, "cannot read block snapshots");
                continue;
            }
        };
        for tag in tags {
            let Ok(id) = tag.parse::<Uuid>() else {
                continue;
            };
            if !matches!(ctx.cluster.get::<SnapshotSpec>(id), Ok(None)) {
                continue;
            }
            match ctx.cluster.get::<BackupSpec>(id) {
                Ok(Some(b)) if b.status.phase != BackupPhase::Pending => {}
                // 채울 Task가 없는 pending 백업(Task가 끝났거나 지워졌다)의 태그도 남은 것이다.
                Ok(Some(_)) if !backing_up(&ctx, id) => {}
                Ok(None) => {}
                _ => continue,
            }
            tracing::warn!(volume = %volume.name, tag, "removing a block snapshot left without its snapshot");
            if let Err(e) = ctx
                .storage
                .snapshot_delete(&pool, volume.id, format, &tag)
                .await
            {
                tracing::warn!(volume = %volume.name, tag, error = %e, "cannot remove block snapshot");
            }
        }
    }
}

/// 백업 `id`를 채우는 백업 Task가 끝나지 않았는지. 목록을 읽지 못하면 그렇다고 본다.
fn backing_up(ctx: &Ctx, id: Uuid) -> bool {
    let Ok((_, tasks)) = ctx.cluster.list::<TaskSpec>() else {
        return true;
    };
    tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VmBackup { backup } if backup == id)
    })
}

/// 이 노드에서 켜진 채 남은 볼륨을 시작할 때와 그 뒤 `tag_sweep`마다 끈다
/// ([`deactivate_idle_volumes`]). 실행기가 멈추면 함께 멈춘다.
async fn deactivate_idle_volumes_every(ctx: Arc<Ctx>) {
    loop {
        deactivate_idle_volumes(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 이 노드에서 켜진 채 남은 볼륨(공유 LVM의 배타 활성화)을 끈다. VM이 멈춘 뒤 reconciler가 끄지
/// 못했거나 opld가 그 전에 내려갔을 때 남는다. 이 노드에 놓여 실행 중이거나 켜려는 VM(하이퍼바이저에서
/// 도는 VM 포함)의 디스크, 이 노드에서 끝나지 않은 Task의 볼륨(VM Task면 그 VM의 디스크), 아직
/// 만드는 중인(pending) 볼륨은 그대로 둔다. 리소스가 없는 볼륨은 끈다.
///
/// 켜진 볼륨을 먼저 읽고 그 뒤에 VM·Task를 읽는다: 볼륨을 켜는 VM·Task는 켜기 전에 이미 저장소에
/// 있으므로 쓰는 중인 볼륨을 끄지 않는다(그 사이 켠 볼륨은 이번에 보지 않는다). 이미 켜져 있던 볼륨을
/// 그 사이 시작한 VM이 그대로 쓸 수도 있으므로 VM·Task는 볼륨마다 끄기 직전에 다시 읽는다.
async fn deactivate_idle_volumes(ctx: &Ctx) {
    let me = ctx.config.node;
    let pools = match ctx.cluster.list::<StoragePoolSpec>() {
        Ok((_, pools)) => pools,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list storage pools");
            return;
        }
    };
    let mut active = Vec::new();
    for pool in pools.into_iter().filter(|p| pool_listed_on(p, me)) {
        match ctx.storage.active_volumes(&pool).await {
            Ok(ids) if !ids.is_empty() => active.push((pool, ids)),
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(pool = %pool.name, error = %e, "cannot list active volumes")
            }
        }
    }
    for (pool, ids) in active {
        for id in ids {
            let (in_use, formats) = match volumes_in_use(ctx).await {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read which volumes are in use");
                    return;
                }
            };
            if in_use.contains(&id) {
                continue;
            }
            let format = formats.get(&id).copied().unwrap_or(ImageFormat::Raw);
            tracing::info!(pool = %pool.name, volume = %id, "deactivating a volume left active");
            if let Err(e) = ctx.storage.deactivate_volume(&pool, id, format).await {
                tracing::warn!(pool = %pool.name, volume = %id, error = %e, "cannot deactivate volume");
            }
        }
    }
}

/// 이 노드가 쓰는 볼륨([`deactivate_idle_volumes`]이 그대로 두는 볼륨)과 볼륨별 형식. 이 노드로
/// 마이그레이션하는 중인 VM의 디스크도 쓰는 중이다(받는 QEMU를 띄우기 전에 켠다). 이 노드에서 보내는
/// 중인 VM은 원하는 상태가 running이고 마이그레이션 Task가 있다.
async fn volumes_in_use(ctx: &Ctx) -> Result<(HashSet<Uuid>, HashMap<Uuid, ImageFormat>), String> {
    let me = ctx.config.node;
    let running = ctx
        .hv
        .list()
        .await
        .map_err(|e| format!("cannot list running VMs: {e}"))?;
    let (volumes, tasks, vms) = ctx
        .cluster
        .list::<VolumeSpec>()
        .and_then(|(_, volumes)| {
            Ok((
                volumes,
                ctx.cluster.list::<TaskSpec>()?.1,
                ctx.cluster.list::<VmSpec>()?.1,
            ))
        })
        .map_err(|e| format!("cannot list volumes, tasks or VMs: {e}"))?;
    let mut in_use: HashSet<Uuid> = volumes
        .iter()
        .filter(|v| v.status.phase != VolumePhase::Ready)
        .map(|v| v.id)
        .collect();
    let mut task_vms = HashSet::new();
    for task in tasks
        .iter()
        .filter(|t| t.spec.node == me && !t.status.state.is_finished())
    {
        in_use.insert(task.spec.target);
        task_vms.insert(task.spec.target);
    }
    let by_name: HashMap<&str, Uuid> = volumes.iter().map(|v| (v.name.as_str(), v.id)).collect();
    for vm in &vms {
        let used = task_vms.contains(&vm.id)
            || running.iter().any(|r| r.id == vm.id)
            || (vm.spec.node == Some(me) && vm.spec.power == PowerState::Running)
            || vm.status.migration.as_ref().is_some_and(|m| m.node == me);
        if used {
            // 떼는 중인 디스크(도는 QEMU에 아직 붙음)도 쓰는 중이다.
            let held = vm.status.hardware.iter().flat_map(|hw| &hw.disks);
            in_use.extend(
                vm.spec
                    .disks
                    .iter()
                    .chain(held)
                    .filter_map(|d| by_name.get(d.volume.as_str())),
            );
        }
    }
    let formats = volumes.iter().map(|v| (v.id, v.spec.format)).collect();
    Ok((in_use, formats))
}

/// 풀이 마운트되지 않았거나 응답하지 않아 지우지 못한 고아 볼륨을 `orphan_retry`마다 다시 지워 본다. 지웠거나 볼륨이
/// 없어졌으면 목록에서 뺀다. 마운트 말고 다른 이유로 실패하면 시작할 때처럼 기록만 하고 뺀다.
async fn retry_orphans(ctx: Arc<Ctx>, mut volumes: Vec<Uuid>) {
    while !volumes.is_empty() {
        tokio::time::sleep(ctx.config.orphan_retry).await;
        let mut left = Vec::new();
        for id in volumes {
            match remove_volume(&ctx, id).await {
                Ok(()) => tracing::info!(volume = %id, "removed a volume left unfinished"),
                Err(RemoveError::Storage(
                    DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                )) => left.push(id),
                Err(e) => tracing::warn!(volume = %id, error = %e, "cannot remove volume"),
            }
        }
        volumes = left;
    }
}

fn is_volume_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VolumeCreate
            | TaskOp::VolumeImport { .. }
            | TaskOp::VolumeDownload { .. }
            | TaskOp::VolumeDelete
    )
}

fn is_snapshot_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::SnapshotDelete { .. }
    )
}

/// Task가 설 줄. 볼륨 Task는 그 볼륨의 줄에 선다(`Storage`의 요구). 스냅샷 Task는 VM의 줄에
/// 선다(부모·현재 위치가 차례로 바뀐다). 복제 Task도 원본 VM의 줄에 선다. 복사하는 동안 그 스냅샷을
/// 지우거나 디스크를 되돌리면 안 된다. 볼륨 크기 늘리기는 그 볼륨을 쓰는 VM의 줄에 선다. 스냅샷
/// 만들기·되돌리기가 같은 디스크를 다루고 되돌리기는 크기도 되돌리므로 섞이면 안 된다. 지금 쓰는
/// VM이 없으면 그 볼륨을 담은 스냅샷의 VM(되돌리면 다시 붙는다), 그것도 없으면 볼륨의 줄이다.
/// VM 전원 Task는 기다리기만 하므로 줄을 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
/// 마이그레이션 Task는 VM의 줄에 선다(앞의 스냅샷·복제가 끝난 뒤 옮긴다). 백업 Task도 VM의 줄에
/// 선다(임시 스냅샷을 찍고 디스크를 읽는다). 핫플러그 Task도 VM의 줄에 서서 스냅샷·복제·백업·마이그레이션과
/// 차례로 돈다(장치가 바뀌는 중에 디스크를 담거나 옮기면 안 된다). 백업 지우기와 복원은 그 백업의 줄에 서서 겹치지 않는다
/// (복원 Task의 대상은 새 VM이다). 복제도 그 백업의 줄에 서서 보내는 동안 지우기가 파일을 지우지
/// 않는다. VM 가져오기와 가져오기 지우기도 그 가져오기의 줄에 선다(가져오기 Task의 대상은 새 VM이다).
/// 노드 비우기는 그 노드의 줄에 서서 한 노드를 한 번에 하나만 비운다. 로컬 디스크 복제도 VM의 줄에 서서 백업·스냅샷·
/// 핫플러그·마이그레이션과 차례로 돈다(디스크를 읽는 중에 장치가 바뀌거나 VM이 옮겨 가면 안 된다). Ceph Task는
/// `Ceph` 리소스의 줄에 서서 한 노드 안에서 차례로 돈다(같은 노드의 apt·`/etc/ceph`를 함께 만지지 않게). 평탄화(계획 58)는
/// 크기 늘리기처럼 그 볼륨을 쓰는 VM의 줄에 서서 스냅샷·복제·백업과 섞이지 않는다. 템플릿 되돌리기는 템플릿 VM의 줄에
/// 서서 앞서 받은 배포(복제) Task가 끝난 뒤 남은 연결 복제를 본다. 펌웨어 초기화(계획 59)도 VM의 줄에 서서 스냅샷·백업·복제가
/// 펌웨어 파일을 담는 중에 지우지 않는다. SDN Task(계획 65)는 chassis 설치까지 `Sdn` 리소스의 줄에 서서 한 노드 안에서
/// 차례로 돈다(Ceph와 같은 까닭 — 같은 노드의 apt·OVS DB).
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    if task.spec.op.is_sdn() {
        return Some(sdn::queue(ctx, task));
    }
    match task.spec.op {
        // 라이브 복원도 백업의 줄에 서서 끝날 때까지 그 백업을 지우지 않게 한다(헬퍼·QEMU가 읽는다).
        TaskOp::BackupRestore { backup, .. } | TaskOp::BackupLiveRestore { backup, .. } => {
            return Some(backup);
        }
        // 저장소 Task는 저장소의 줄(같은 저장소의 GC·검증은 이 노드 안에서 차례로 — 계획 68 §10.2. 다른 노드에서는 함께
        // 돌 수 있다: GC는 가리키지 않는 청크만, 검증은 가리키는 청크만 건드리고 잠금은 Raft의 저장소 상태다 — 리뷰 1 M5)
        TaskOp::BackupStoreInit
        | TaskOp::BackupStoreGc
        | TaskOp::BackupStoreVerify { .. }
        | TaskOp::BackupStoreScan => return Some(target),
        TaskOp::VmImport { import, .. } => return Some(import),
        // 옮기기의 걸음은 그 옮기기의 줄에서 차례로(계획 67)
        TaskOp::NetworkMigrate { .. } => return Some(target),
        _ => {}
    }
    if !matches!(
        task.spec.op,
        TaskOp::VolumeResize { .. } | TaskOp::VolumeFlatten
    ) {
        let vm_queue = matches!(
            task.spec.op,
            TaskOp::VmClone { .. }
                | TaskOp::VmMigrate { .. }
                | TaskOp::VmBackup { .. }
                | TaskOp::VmHotplug
                | TaskOp::VmReplicate { .. }
                | TaskOp::VmUntemplate
                | TaskOp::VmFirmwareReset { .. }
        );
        let backup_queue = matches!(
            task.spec.op,
            TaskOp::BackupDelete
                | TaskOp::BackupReplicate { .. }
                | TaskOp::BackupConvert { .. }
                | TaskOp::BackupCopy { .. }
                | TaskOp::VmImportDelete
        );
        let node_queue = task.spec.op == TaskOp::NodeDrain;
        return (is_volume_op(&task.spec.op)
            || is_snapshot_op(&task.spec.op)
            || vm_queue
            || backup_queue
            || node_queue
            || task.spec.op.is_ceph())
        .then_some(target);
    }
    let Ok(Some(volume)) = ctx.cluster.get::<VolumeSpec>(target) else {
        return Some(target);
    };
    let uses = |spec: &VmSpec| spec.disks.iter().any(|d| d.volume == volume.name);
    if let Ok((_, vms)) = ctx.cluster.list::<VmSpec>()
        && let Some(vm) = vms.iter().find(|vm| vm_uses_volume(vm, &volume.name))
    {
        return Some(vm.id);
    }
    if let Ok((_, snapshots)) = ctx.cluster.list::<SnapshotSpec>()
        && let Some(s) = snapshots.iter().find(|s| uses(&s.spec.vm_spec))
    {
        return Some(s.spec.vm);
    }
    Some(target)
}

/// Task를 실행한다. 줄(`queue`)이 있으면 그 줄의 앞 Task가 끝난 뒤에 시작한다.
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다. 업그레이드 Task는 opld 밖의
/// 유닛에서 돌고 opld를 다시 시작하게 하므로 실패로 기록하지 않고 그 유닛을 이어서 지켜본다.
fn start(
    ctx: &Arc<Ctx>,
    started: &mut HashSet<Uuid>,
    running: &mut HashMap<Uuid, AbortHandle>,
    by_target: &mut HashMap<Uuid, JoinHandle<()>>,
    task: Task,
    interrupted: bool,
) {
    started.insert(task.id);
    let id = task.id;
    let queue = queue(ctx, &task);
    let previous = queue.and_then(|q| by_target.remove(&q));
    let ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        // 옮기기 Task(계획 67 §9.4)는 VM마다 상태를 리소스에 적으므로 다시 뜬 opld가 이어서 한다.
        let resumes = matches!(
            task.spec.op,
            TaskOp::NodeUpgrade | TaskOp::NetworkMigrate { .. } | TaskOp::BackupLiveRestore { .. }
        );
        if interrupted && !resumes {
            let error = "interrupted: orbivisord restarted while the task was running".to_string();
            fail(&ctx, &task, error, true).await;
        } else {
            execute(&ctx, task, interrupted).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
}

/// `resume`이면 이전 opld가 실행하던 Task를 이어서 한다(업그레이드만).
async fn execute(ctx: &Ctx, task: Task, resume: bool) {
    if !begin(ctx, task.id).await {
        return;
    }
    // 시간을 넘겨 버린 작업은 끝나지 않았다(명령이 아직 돌 수 있다). 마이그레이션은 단계마다
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    // 백업과 복원, 복제, VM 가져오기는 디스크 크기만큼 오래 걸릴 수 있으므로 시간 제한을 두지 않는다
    // (설계 §2.3, 복제는 원격 클라이언트가 조각 사이 60초를 본다).
    // 업그레이드도 오래 걸릴 수 있고 apt를 중간에 끊으면 더 나쁘다. 노드 비우기는 마이그레이션을 여럿
    // 하므로 VM마다 기다리는 시간만 둔다(`migration_timeout`). 로컬 디스크 복제는 전체 회차가 디스크 크기만큼
    // 걸리므로 보낸 양이 늘지 않는 시간만 본다(`replication_stall`). Ceph Task는 꾸러미 설치·디스크 지우기처럼
    // 오래 걸리는 단계마다 한도가 따로 있다(SDN Task도 같다 — apt, Raft 기다리기). URL 받기는 이미지 크기만큼 걸리므로 연결·읽기 쉼 한도만 둔다. 평탄화는
    // 기반 데이터를 디스크 크기만큼 복사한다(`qemu-img rebase`, `zfs send`).
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. }
            | TaskOp::VmBackup { .. }
            | TaskOp::BackupRestore { .. }
            | TaskOp::BackupReplicate { .. }
            | TaskOp::VmImport { .. }
            | TaskOp::NodeUpgrade
            | TaskOp::NodeDrain
            | TaskOp::VmReplicate { .. }
            | TaskOp::VolumeDownload { .. }
            | TaskOp::VolumeFlatten
    ) || task.spec.op.is_ceph()
        || task.spec.op.is_sdn()
        // 저장소 Task(계획 68)는 저장소 크기만큼 읽고 쓴다(S3는 다시 하기 60초를 품는다).
        || matches!(
            task.spec.op,
            TaskOp::BackupStoreInit
                | TaskOp::BackupStoreGc
                | TaskOp::BackupStoreVerify { .. }
                | TaskOp::BackupStoreScan
                | TaskOp::BackupConvert { .. }
                | TaskOp::BackupCopy { .. }
                | TaskOp::BackupLiveRestore { .. }
        )
        || matches!(task.spec.op, TaskOp::NetworkMigrate { .. })
        // 파일 복원 세션은 스스로 유휴·최대 시간으로 끝난다(계획 69 §10.3).
        || matches!(task.spec.op, TaskOp::FileRestoreSession { .. });
    let (result, unfinished) = if unlimited {
        (perform(ctx, &task, resume).await, false)
    } else {
        match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task, resume)).await {
            Ok(r) => (r, false),
            Err(_) => (
                Err(format!("timed out after {:?}", ctx.config.timeout)),
                true,
            ),
        }
    };
    match result {
        Ok(warning) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            if task.spec.op.is_ceph() {
                ceph::finished(ctx, &task, None).await;
            }
            if task.spec.op.is_sdn() {
                sdn::finished(ctx, &task, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, warning).await
        }
        Err(e) => fail(ctx, &task, e, unfinished).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다. `unfinished`면 작업이 끝나지 않았다(시간 초과, 재시작으로 중단).
async fn fail(ctx: &Ctx, task: &Task, error: String, unfinished: bool) {
    let error = if unfinished {
        unfinished_error(ctx, task, error).await
    } else {
        error
    };
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task, &error, unfinished).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 끝나지 않은(시간 초과, 재시작으로 중단) Task의 오류에 남은 일을 붙인다. 템플릿 표시를 지운 뒤 끊긴 되돌리기는 다시 보기를
/// 하지 못했으므로 여기서 연결 복제를 다시 본다(남았으면 그 이름, 표시는 되돌리지 않는다). 가져오기를 마친 받기 → 템플릿은
/// 받은 볼륨을 남기므로([`keeps_downloaded`]) 그것으로 손수 만드는 길을 알린다(템플릿을 이미 만들었으면 알리지 않는다).
async fn unfinished_error(ctx: &Ctx, task: &Task, error: String) -> String {
    if task.spec.op == TaskOp::VmUntemplate
        && let Err(e) = template::recheck_unfinished(ctx, task.spec.target).await
    {
        return format!("{error}; {e}");
    }
    if let TaskOp::VolumeDownload {
        template: Some(draft),
        ..
    } = &task.spec.op
        && keeps_downloaded(ctx, task)
        && let Ok(Some(volume)) = ctx.cluster.get::<VolumeSpec>(task.spec.target)
        && !matches!(
            ctx.cluster.get_by_name::<VmSpec>(&draft.name),
            Ok(Some(vm)) if vm.spec.template
        )
    {
        return download::keep_hint(error, &volume.name);
    }
    error
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제·복원·VM 가져오기의 흔적을 지워 이름을 다시 쓸 수 있게
/// 한다. 실패한 백업은 디렉터리를 지우고 `failed`와 `error`를 남긴다. 실패한 복제는 원본의 사본 기록을
/// `failed`와 `error`로 바꾼다(체인 앞 백업의 copying 기록도). 실패한 VM 가져오기는 `ready`로 돌아가
/// `error`를 남긴다. 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
/// 블록 디스크의 태그는 뒤에서 다시 지운다([`snapshot::remove_unfinished_tags`]). 실패한 로컬 디스크 복제는 그
/// 대상의 상태에 `error`를 남기고 원본 QEMU에 남은 플리싱을 치운 뒤 대상 노드에 회차를 버리게 한다. 실패한
/// Ceph 부트스트랩은 `Ceph` 리소스에 `last_error`를 남기고(다시 하면 이어서 한다), Ceph Task는 잡아 둔 자리
/// (`pending`)를 놓는다. SDN Task도 같다(첫 세우기의 실패는 `Sdn` 리소스의 `last_error`).
async fn cleanup(ctx: &Ctx, task: &Task, error: &str, unfinished: bool) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op {
        if unfinished {
            snapshot::remove_unfinished_tags(ctx, snapshot);
        }
        if let Err(e) = snapshot::discard_pending(ctx, snapshot).await {
            tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
        }
    }
    if let TaskOp::VolumeImport { source, .. } = &task.spec.op {
        let _ = tokio::fs::remove_file(source).await;
    }
    if let (TaskOp::VolumeDownload { .. }, Some(dir)) = (&task.spec.op, &ctx.config.upload_dir) {
        let _ = tokio::fs::remove_file(download::path(dir, task.spec.target)).await;
    }
    if let TaskOp::VmClone { .. } | TaskOp::BackupRestore { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
    }
    if let TaskOp::BackupLiveRestore { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
        live_restore::failed(ctx, task, error).await;
    }
    if let TaskOp::VmBackup { backup } = task.spec.op {
        backup::discard(ctx, task, backup, error, unfinished).await;
    }
    if let TaskOp::BackupReplicate { remote, pool } = &task.spec.op {
        replicate::discard_chain(ctx, task.spec.target, remote, pool, error).await;
    }
    match &task.spec.op {
        TaskOp::BackupConvert { store } => {
            backup_store::discard_convert(ctx, task, store, error).await;
        }
        TaskOp::BackupCopy { store } => {
            backup_store::discard_copy(ctx, task, store, error).await;
        }
        TaskOp::BackupStoreInit => store_tasks::init_failed(ctx, task, error).await,
        TaskOp::BackupStoreGc => store_tasks::gc_failed(ctx, task, error).await,
        TaskOp::BackupStoreVerify { .. } => store_tasks::verify_failed(ctx, task, error).await,
        _ => {}
    }
    if let TaskOp::VmImport { import, .. } = task.spec.op {
        import::discard(ctx, task, import, error).await;
    }
    if let TaskOp::VmReplicate { .. } = task.spec.op {
        vm_replicate::discard(ctx, task, error).await;
    }
    if task.spec.op.is_ceph() {
        ceph::finished(ctx, task, Some(error)).await;
    }
    if task.spec.op.is_sdn() {
        sdn::finished(ctx, task, Some(error)).await;
    }
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. } | TaskOp::VolumeDownload { .. }
    ) && !keeps_downloaded(ctx, task)
        && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// 템플릿 초안이 있는 받기가 가져오기를 마친 뒤(볼륨 Ready) 실패했으면 받은 볼륨을 남긴다 — 템플릿 만들기만 실패했다
/// (계획 58 §8). 그 볼륨으로 손수 템플릿을 만들 수 있다.
fn keeps_downloaded(ctx: &Ctx, task: &Task) -> bool {
    matches!(
        &task.spec.op,
        TaskOp::VolumeDownload {
            template: Some(_),
            ..
        }
    ) && matches!(
        ctx.cluster.get::<VolumeSpec>(task.spec.target),
        Ok(Some(v)) if v.status.phase == VolumePhase::Ready
    )
}

/// `pending`을 `running`으로 읽은 revision에서만 바꾼다. 그사이 다른 쪽(리더의 정리 작업)이
/// 실패로 끝냈거나 Task가 지워졌으면 `false`로, 실행하지 않는다. `running`으로 읽히면 응답을 잃은
/// 앞의 쓰기가 적용된 것이다(`running`은 이 노드만 쓴다). 기록될 때까지 다시 시도한다.
async fn begin(ctx: &Ctx, id: Uuid) -> bool {
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
        progress: None,
    };
    loop {
        let current = match ctx.cluster.get::<TaskSpec>(id) {
            Ok(Some(task)) => task,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot read task; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        match current.status.state {
            TaskState::Pending => {}
            TaskState::Running => return true,
            TaskState::Succeeded | TaskState::Failed => return false,
        }
        match ctx
            .cluster
            .update_status_checked::<TaskSpec>(id, current.revision, &running)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return false,
            // 바뀐 Task가 이 노드에 적용되면 다시 읽어 판단한다.
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task start; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus {
        state,
        error,
        progress: None,
    };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
            // 정리 작업이 지운 Task는 끝난 것이다(`begin`과 같다).
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task status; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 성공한 Task가 남길 경고(`error`)가 있으면 `Ok(Some)`이다(지우지 못한 원격 사본, 얼리지 못한 파일 시스템).
async fn perform(ctx: &Ctx, task: &Task, resume: bool) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let done = match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
        TaskOp::VmStop => wait_vm(ctx, id, stopped).await,
        TaskOp::VmForceStop => {
            ctx.reconciler.force_stop(id);
            wait_vm(ctx, id, stopped).await
        }
        TaskOp::VmReboot => reboot(ctx, task).await,
        TaskOp::VolumeCreate => create_volume(ctx, id).await,
        TaskOp::VolumeImport {
            source,
            source_format,
        } => import_volume(ctx, id, source, *source_format).await,
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
        // 게스트 에이전트가 파일 시스템을 얼리지 못했으면 경고로 남긴다(계획 59 §9).
        TaskOp::SnapshotCreate { snapshot } => return snapshot::create(ctx, id, *snapshot).await,
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot, .. } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        // 연결하지 못해 전체 복사한 디스크, 고정 IP·호스트 이름, 켜지 못한 이유는 경고로 남긴다.
        TaskOp::VmClone {
            name,
            snapshot,
            pool,
            linked,
            overrides,
        } => {
            return clone::clone(
                ctx,
                task,
                name,
                *snapshot,
                pool.as_deref(),
                *linked,
                overrides.as_deref(),
            )
            .await;
        }
        TaskOp::VmMigrate { node, pool } => {
            migrate::migrate(ctx, task, *node, pool.as_deref()).await
        }
        // 기록 Task는 끝난 상태로 만들어지므로 실행되지 않는다.
        TaskOp::VmHaRecover { .. } => Err("an HA recovery task is a record and is not run".into()),
        TaskOp::NodePowerOff { .. } => {
            Err("a node power-off task is a record and is not run".into())
        }
        TaskOp::HaCheck { .. } => Err("an HA check task is a record and is not run".into()),
        TaskOp::BondDegraded { .. } => Err("a bond warning task is a record and is not run".into()),
        TaskOp::CpuBaseline { .. } => Err("a CPU baseline task is a record and is not run".into()),
        TaskOp::VmBackup { backup } => return backup::backup(ctx, task, *backup).await,
        TaskOp::BackupDelete => return backup::delete(ctx, id).await,
        // 비운 CD-ROM 드라이브는 경고로 남긴다.
        TaskOp::BackupRestore {
            backup,
            name,
            pool,
            folder,
        } => {
            return backup::restore(ctx, task, *backup, name, pool, *folder, false).await;
        }
        TaskOp::BackupReplicate { remote, pool } => {
            replicate::replicate(ctx, id, remote, pool).await
        }
        TaskOp::VmImport { import, run } => import::import(ctx, task, *import, run).await,
        TaskOp::VmImportDelete => import::delete(ctx, id).await,
        TaskOp::NodeUpdateCheck => match &ctx.config.updates {
            Some(updates) => updates.refresh_waiting(&ctx.cluster, ctx.config.node).await,
            None => Err("updates are not enabled on this node".into()),
        },
        TaskOp::NodeUpgrade => match &ctx.config.updates {
            Some(updates) => {
                updates
                    .upgrade(&ctx.cluster, ctx.config.node, task.id, resume)
                    .await
            }
            None => Err("updates are not enabled on this node".into()),
        },
        TaskOp::NodeDrain => drain::drain(ctx, id).await,
        TaskOp::NodeCpuProbe => match &ctx.config.cpu {
            Some(cpu) => cpu.refresh().await,
            None => Err("this node does not read CPU models (a witness)".into()),
        },
        // 성공해도 다음 재시작 때 적용할 변경이 있으면 경고로 남긴다.
        TaskOp::VmHotplug => return hotplug::hotplug(ctx, task).await,
        // VM이 이 노드에서 돌지 않아 건너뛴 회차는 경고로 남긴다.
        TaskOp::VmReplicate { .. } => return vm_replicate::replicate(ctx, task).await,
        TaskOp::CephInit
        | TaskOp::CephMonAdd
        | TaskOp::CephMgrAdd
        | TaskOp::CephOsdAdd { .. }
        | TaskOp::CephPoolCreate { .. }
        | TaskOp::CephClientInstall => ceph::perform(ctx, task).await,
        // SDN 설치·구성(계획 65). `br-int`의 남의 포트·짝수 중앙은 경고로 남긴다.
        TaskOp::SdnInit { .. }
        | TaskOp::SdnCentralAdd
        | TaskOp::SdnCentralRemove
        | TaskOp::SdnCentralKick { .. }
        | TaskOp::SdnChassisInstall
        | TaskOp::SdnLockdown { .. } => return sdn::perform(ctx, task).await,
        // 단순 → SDN 옮기기(계획 67 §9). 원본에 남은 NIC·걷지 못한 다리는 경고로 남긴다.
        TaskOp::NetworkMigrate { .. } => {
            return Box::pin(network_migrate::perform(ctx, task)).await;
        }
        TaskOp::VolumeDownload {
            url,
            sha256,
            source_format,
            sha512,
            checksum_url,
            template,
        } => {
            let expected = download::Expected {
                sha256: sha256.as_deref(),
                sha512: sha512.as_deref(),
                checksum_url: checksum_url.as_deref(),
            };
            download::download(
                ctx,
                task,
                url,
                expected,
                *source_format,
                template.as_deref(),
            )
            .await
        }
        TaskOp::VolumeFlatten => template::flatten(ctx, id).await,
        // SDN 주소를 다시 받지 못한 NIC는 Task 경고로 알린다(리뷰 2 N-4).
        TaskOp::VmUntemplate => return template::untemplate(ctx, task).await,
        TaskOp::VmFirmwareReset {
            uefi_vars,
            tpm,
            secure_boot,
        } => firmware::reset(ctx, task, (*uefi_vars, *tpm), *secure_boot).await,
        // 백업 저장소 Task(계획 68 §10·§12). 깨진 백업·읽지 못한 스냅샷은 경고로 남긴다.
        TaskOp::BackupStoreInit => return store_tasks::init(ctx, task).await,
        TaskOp::BackupStoreGc => return Box::pin(store_tasks::gc(ctx, task)).await,
        TaskOp::BackupStoreVerify { backups } => {
            return Box::pin(store_tasks::verify(ctx, task, backups)).await;
        }
        TaskOp::BackupStoreScan => return Box::pin(store_tasks::scan(ctx, task)).await,
        TaskOp::BackupConvert { store } => {
            return Box::pin(backup_store::convert(ctx, task, store)).await;
        }
        // 디스크를 다 받기 전에 켜고 뒤에서 옮긴다(계획 68 §11). opld가 다시 떠도 잇는다.
        TaskOp::BackupLiveRestore {
            backup,
            name,
            pool,
            folder,
        } => {
            return Box::pin(live_restore::run(
                ctx, task, *backup, name, pool, *folder, resume,
            ))
            .await;
        }
        TaskOp::BackupCopy { store } => {
            return Box::pin(backup_store::copy(ctx, task, store)).await;
        }
        // 파일 복원 세션(계획 69 §10): 유휴·최대 시간·닫기로 끝나면 그 까닭을 경고로 남긴다.
        TaskOp::FileRestoreSession { backup } => {
            return Box::pin(file_restore::run(ctx, task, *backup)).await;
        }
        // 게스트로 되돌려 쓰기는 스테이지 6이 더한다. 그 전에는 API가 만들지 않는다.
        TaskOp::FileRestoreToGuest { .. } => Err("file restore tasks are not available yet".into()),
    };
    done.map(|()| None)
}

/// VM을 켰다: 원하는 상태와 실제 상태가 모두 running. reconciler가 포기해 원하는 상태를
/// stopped로 바꿨으면 실패다. 이전 실행의 Failed status는 원하는 상태가 running인 동안 무시한다.
fn started(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, VmPhase::Running) => Some(Ok(())),
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Err(vm
            .status
            .last_error
            .clone()
            .unwrap_or_else(|| "VM was stopped".into()))),
        _ => None,
    }
}

fn stopped(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        (PowerState::Running, _) => Some(Err("VM was started again".into())),
        _ => None,
    }
}

/// VM이 조건을 만족할 때까지 로컬 저장소를 다시 읽는다.
async fn wait_vm(
    ctx: &Ctx,
    id: Uuid,
    done: impl Fn(&Vm) -> Option<Result<(), String>>,
) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if let Some(result) = done(&vm) {
            return result;
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

/// 원하는 상태를 바꾼다. reconciler의 status 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_power(ctx: &Ctx, id: Uuid, power: PowerState) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if vm.spec.power == power {
            return Ok(());
        }
        let mut spec = vm.spec.clone();
        spec.power = power;
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// ACPI로 끈 뒤 다시 켠다. 게스트가 ACPI를 무시하면 reconciler가 `acpi_timeout` 뒤 강제로 끈다.
/// 그 사이 같은 VM에 대한 요청(시작, 정지, 강제 종료)이 새로 들어오면 그 요청을 따르고 다시 켜지 않는다.
async fn reboot(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    // 뒤의 요청이 이미 끝났을 수 있다(켜진 VM의 start는 곧바로 끝난다). 그 뒤에 끄면 안 된다.
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Stopped).await?;
    wait_vm(ctx, id, |vm| match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, _) | (_, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        _ => None,
    })
    .await?;
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Running).await?;
    wait_vm(ctx, id, started).await
}

/// 이 Task 뒤에 같은 VM의 전원을 바꾸는 Task(동작, 스냅샷 되돌리기)가 만들어졌는지.
/// 스냅샷 만들기·삭제는 전원을 바꾸지 않으므로 세지 않는다. Task id는 UUIDv7이라 만든 순서대로 커진다.
fn superseded(ctx: &Ctx, task: &Task) -> Result<bool, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == task.spec.target
            && t.id > task.id
            && matches!(
                t.spec.op,
                TaskOp::VmStart
                    | TaskOp::VmStop
                    | TaskOp::VmForceStop
                    | TaskOp::VmReboot
                    | TaskOp::SnapshotRevert { .. }
            )
    }))
}

fn volume_and_pool(ctx: &Ctx, id: Uuid) -> Result<(Volume, StoragePool), String> {
    let volume = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or("volume was deleted")?;
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok((volume, pool))
}

/// 볼륨 스펙의 크기를 바꾼다. 다른 쓰기(라벨 등)를 덮지 않게 다시 읽은 revision으로 쓰고,
/// 그 사이 바뀌었으면 다시 읽어 쓴다.
async fn set_volume_size(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    loop {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("volume was deleted")?;
        if volume.spec.size_bytes == size_bytes {
            return Ok(());
        }
        let spec = VolumeSpec {
            size_bytes,
            ..volume.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(volume.revision), volume.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 파일이 준비된 볼륨을 Ready로 기록한다. 실제 크기(qemu-img가 512바이트 단위로 올림하고, 가져온
/// 이미지는 원본 크기다)와 할당 크기도 남기는데, 이것은 실패해도 넘어간다. Ready는 기록될 때까지
/// 다시 시도한다. 실패로 끝내면 정리하면서 다 만든 파일을 지우게 된다.
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            None
        }
    };
    if let Some(info) = info
        && volume.spec.size_bytes != info.virtual_bytes
    {
        let mut spec = volume.spec.clone();
        spec.size_bytes = info.virtual_bytes;
        if let Err(e) = ctx
            .cluster
            .update_spec(volume.id, None, volume.labels.clone(), &spec)
            .await
        {
            tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
        }
    }
    let status = VolumeStatus {
        phase: VolumePhase::Ready,
        allocated_bytes: info.map(|i| i.allocated_bytes),
        ..volume.status.clone()
    };
    while let Err(e) = ctx
        .cluster
        .update_status::<VolumeSpec>(volume.id, &status)
        .await
    {
        tracing::warn!(volume = %volume.id, error = %e, "cannot mark volume ready; will retry");
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

async fn create_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    match ctx
        .storage
        .create_volume(&pool, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn import_volume(
    ctx: &Ctx,
    id: Uuid,
    source: &Path,
    source_format: SourceFormat,
) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    if !volume.spec.content.is_disk() {
        check_iso_source(source).await?;
    }
    ctx.storage
        .check_import_source(source, source_format)
        .await
        .map_err(|e| e.to_string())?;
    ctx.storage
        .import_volume(&pool, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

/// ISO 볼륨의 원본이 설치 매체(ISO 9660·UDF)인지 앞부분으로 본다.
async fn check_iso_source(source: &Path) -> Result<(), String> {
    use tokio::io::AsyncReadExt;

    let mut header = Vec::with_capacity(ISO_HEADER_LEN);
    tokio::fs::File::open(source)
        .await
        .map_err(|e| e.to_string())?
        .take(ISO_HEADER_LEN as u64)
        .read_to_end(&mut header)
        .await
        .map_err(|e| e.to_string())?;
    if !is_iso_image(&header) {
        return Err("not an ISO 9660 or UDF image".into());
    }
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    // 연결 복제의 기반과 템플릿 디스크는 지우지 않는다(계획 58). API가 확인한 뒤 다른 노드의 복제 Task가 그새 기록을
    // 만들었거나 다른 노드의 요청이 템플릿으로 바꿨을 수 있다.
    if let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
    {
        let (_, volumes) = ctx
            .cluster
            .list::<VolumeSpec>()
            .map_err(|e| e.to_string())?;
        let clones: Vec<&str> = volumes
            .iter()
            .filter(|v| v.spec.base == Some(id))
            .map(|v| v.name.as_str())
            .collect();
        if !clones.is_empty() {
            return Err(crate::volumes::has_clones_message(&volume.name, &clones));
        }
        // 그새 템플릿이 된 VM의 디스크도 지우지 않는다(다음 배포의 기반이다).
        let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        if let Some(template) = crate::volumes::template_of(&vms, &volume.name) {
            return Err(format!(
                "volume {:?} is a disk of template {:?}",
                volume.name, template.name
            ));
        }
    }
    // ISO는 여러 VM이 함께 쓰므로 API가 확인한 뒤 다른 노드의 요청이 CD에 넣었거나 그 VM의 스냅샷을 찍었을 수 있다.
    // 지우기 전에 다시 본다.
    if let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .filter(|v| !v.spec.content.is_disk())
    {
        let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        let users: Vec<&Vm> = vms
            .iter()
            .filter(|vm| vm_uses_volume(vm, &volume.name))
            .collect();
        if !users.is_empty() {
            return Err(crate::volumes::in_cdrom_message(&volume.name, &users));
        }
        let (_, snapshots) = ctx
            .cluster
            .list::<SnapshotSpec>()
            .map_err(|e| e.to_string())?;
        if let Some(s) = snapshots
            .iter()
            .find(|s| crate::volumes::spec_refers_to(&s.spec.vm_spec, &volume.name))
        {
            return Err(crate::volumes::in_snapshot_message(
                &volume.name,
                &s.spec.name,
                s.spec.vm,
            ));
        }
    }
    remove_volume(ctx, id).await.map_err(|e| e.to_string())
}

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD, 공유 LVM)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
///
/// 폴더 할당량(계획 62 리뷰 1): 저장소를 건드리기 전에 요청한 크기를 스펙에 먼저 써서 Raft의 할당량 검사로 몫을
/// 잡는다(두 노드가 동시에 늘려도 하나만 잡는다). 늘린 뒤의 실제 크기(LVM extent·ZFS 단위 올림)는 상태
/// `disk_bytes`에 적은 뒤 스펙에 기록해 할당량에 막히지 않는다(사용량을 적게 세지 않는다). 늘리지 못하면 디스크의
/// 실제 크기로 되돌린다.
async fn resize_volume(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err("volume is not ready".into());
    }
    if size_bytes <= volume.spec.size_bytes {
        return Err(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        ));
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms.iter().find(|vm| {
        vm_uses_volume(vm, &volume.name) && !vm.spec.disks.iter().any(|d| d.volume == volume.name)
    }) {
        return Err(format!(
            "volume {:?} is still being released by VM {:?}; run `vm hotplug` or restart the VM",
            volume.name, vm.name
        ));
    }
    let user = vms.iter().find_map(|vm| {
        let disk = vm.spec.disks.iter().position(|d| d.volume == volume.name)?;
        Some((vm, disk))
    });
    // 꺼진 VM의 경로(저장소로 늘린다)는 원하는 상태도 꺼짐일 때만 탄다. 켜는 중에 늘리면 그동안
    // 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
    if let Some((vm, _)) = user
        && vm.status.phase != VmPhase::Running
        && !is_stopped(vm)
    {
        return Err(format!(
            "VM {:?} is starting or stopping; try again when it has settled",
            vm.name
        ));
    }
    // 앞 Task가 지우지 못한 허가(기록 실패·노드 죽음)가 남아 있으면 먼저 지운다(리뷰 2 N8): 남은 허가는 그 크기까지의
    // 기록을 할당량 없이 통과시킨다.
    clear_disk_bytes(ctx, id).await?;
    // 할당량을 미리 본 뒤(경로가 든 메시지) 스펙에 써서 Raft가 몫을 잡는다. 거절되면 디스크는 그대로다.
    crate::quota::precheck(
        &ctx.cluster,
        crate::quota::Change::Volume {
            name: &volume.name,
            size: size_bytes,
        },
    )
    .map_err(|e| e.message)?;
    set_volume_size(ctx, id, size_bytes).await?;
    if let Err(e) = grow_storage(ctx, &volume, &pool, user, size_bytes).await {
        // 늘리지 못했다: 잡아 둔 크기를 디스크의 실제 크기로 되돌린다(읽지 못하면 크게 센 채로 둔다).
        if let Ok(info) = ctx.storage.volume_info(&pool, id, volume.spec.format).await
            && let Err(undo) = record_grown_size(ctx, id, info.virtual_bytes).await
        {
            tracing::warn!(volume = %volume.name, error = %undo, "cannot record the volume size after a failed resize");
        }
        return Err(e);
    }
    let info = ctx
        .storage
        .volume_info(&pool, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    record_grown_size(ctx, id, info.virtual_bytes).await
}

/// 저장소(실행 중이면 QEMU도)로 볼륨을 늘린다.
async fn grow_storage(
    ctx: &Ctx,
    volume: &Volume,
    pool: &StoragePool,
    user: Option<(&Vm, usize)>,
    size_bytes: u64,
) -> Result<(), String> {
    let id = volume.id;
    match user {
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다. 블록 볼륨은
        // QEMU가 늘릴 수 없으므로 저장소로 먼저 늘린 뒤 QEMU에 볼륨의 실제 크기(LVM은 extent,
        // ZFS는 1 MiB 단위로 올린다)를 알린다. 그래야 게스트가 보는 크기가 기록하는 크기와 같다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => {
            let mut size_bytes = size_bytes;
            if block_pool_kind(pool).is_some() {
                ctx.storage
                    .resize_volume(pool, id, volume.spec.format, size_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                size_bytes = ctx
                    .storage
                    .volume_info(pool, id, volume.spec.format)
                    .await
                    .map_err(|e| format!("cannot read the new size: {e}"))?
                    .virtual_bytes;
            }
            ctx.hv
                .block_resize(vm.id, disk, size_bytes)
                .await
                .map_err(|e| e.to_string())
        }
        _ => ctx
            .storage
            .resize_volume(pool, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string()),
    }
}

/// 늘린 디스크의 실제 크기를 기록한다. 상태 `disk_bytes`를 먼저 적어 스펙 기록이 폴더 할당량에 막히지 않게 하고
/// (Raft 쪽 `quota_check`) 기록한 뒤 지운다. 기록하지 못하면 오류다(Task가 실패로 알린다).
async fn record_grown_size(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    let volume = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or("volume was deleted")?;
    if volume.status.disk_bytes != Some(size_bytes) {
        let status = VolumeStatus {
            disk_bytes: Some(size_bytes),
            ..volume.status.clone()
        };
        ctx.cluster
            .update_status::<VolumeSpec>(id, &status)
            .await
            .map_err(|e| e.to_string())?;
    }
    set_volume_size(ctx, id, size_bytes).await.map_err(|e| {
        format!("the disk now has {size_bytes} bytes but the size was not recorded: {e}")
    })?;
    // 허가는 한 번만 쓴다: 남겨 두면 스냅샷 되돌리기로 작아진 뒤 그 크기까지 다시 늘리는 것이 할당량을 건너뛴다.
    // 지우지 못하면 다음 늘리기·되돌리기가 지운다.
    if let Err(e) = clear_disk_bytes(ctx, id).await {
        tracing::warn!(volume = %id, error = %e, "cannot clear the recorded disk size");
    }
    Ok(())
}

/// 볼륨 상태의 `disk_bytes`(늘리기 허가)를 지운다. 쓰기가 실패하면 세 번까지 다시 한다. 볼륨이 없으면 할 일이 없다.
async fn clear_disk_bytes(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..3 {
        let Some(volume) = ctx
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        if volume.status.disk_bytes.is_none() {
            return Ok(());
        }
        let status = VolumeStatus {
            disk_bytes: None,
            ..volume.status.clone()
        };
        match ctx.cluster.update_status::<VolumeSpec>(id, &status).await {
            Ok(_) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("cannot clear the recorded disk size: {last}"))
}

/// 볼륨을 지우지 못한 이유
#[derive(Debug, thiserror::Error)]
enum RemoveError {
    #[error(transparent)]
    Storage(DriverError),
    #[error("{0}")]
    Other(String),
}

impl From<String> for RemoveError {
    fn from(e: String) -> Self {
        RemoveError::Other(e)
    }
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), RemoveError> {
    let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms.iter().find(|vm| vm_uses_volume(vm, &volume.name)) {
        return Err(format!("volume is attached to VM {:?}", vm.name).into());
    }
    if let Some(pool) = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
    {
        match ctx
            .storage
            .delete_volume(&pool, id, volume.spec.format)
            .await
        {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(RemoveError::Storage(e)),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string().into()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일, VM 가져오기의 남은 파일([`crate::vm_imports::sweep`]).
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
/// 풀이 아직 마운트되지 않아(opld가 막 시작했다) 지우지 못한 볼륨을 돌려준다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) -> Vec<Uuid> {
    let mut unmounted = Vec::new();
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .collect();
    match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => {
            for volume in volumes {
                // Task 목록보다 나중에 만든 볼륨은 그 목록으로 판단할 수 없다(다른 노드의 API가
                // 방금 만들었을 수 있다).
                if volume.revision > revision
                    || volume.status.node.is_some_and(|n| n != ctx.config.node)
                {
                    continue;
                }
                let has_task = unfinished.iter().any(|t| t.spec.target == volume.id);
                if volume.status.phase == VolumePhase::Pending && !has_task {
                    tracing::warn!(volume = %volume.name, "removing a volume left unfinished");
                    match remove_volume(ctx, volume.id).await {
                        Ok(()) => {}
                        Err(
                            e @ RemoveError::Storage(
                                DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                            ),
                        ) => {
                            tracing::info!(volume = %volume.name, error = %e, "will remove the volume when its pool is mounted");
                            unmounted.push(volume.id);
                        }
                        Err(e) => {
                            tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume")
                        }
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list volumes"),
    }
    match ctx.cluster.list::<SnapshotSpec>() {
        Ok((_, snapshots)) => {
            for s in snapshots {
                if s.revision > revision || s.status.phase != SnapshotPhase::Pending {
                    continue;
                }
                let creates = |t: &&Task| matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id);
                if unfinished.iter().any(creates) {
                    continue;
                }
                // 만들던 노드: 끝난 Task가 있으면 그 노드, 없으면(Task를 만들기 전에 API가 죽었다)
                // API가 Task를 배정했을 노드(`vm_placement`, 공유 풀이면 노드마다 같은 답이 나오게
                // 이 노드를 앞세우지 않는다). 그것도 없으면 디스크가 없는 VM이라 태그가 없으니 어느
                // 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, None, &vm)));
                if owner.is_some_and(|n| n != ctx.config.node) {
                    continue;
                }
                tracing::warn!(snapshot = %s.id, vm = %s.spec.vm, "removing a snapshot left unfinished");
                if let Err(e) = snapshot::discard(ctx, &s).await {
                    tracing::warn!(snapshot = %s.id, error = %e, "cannot remove snapshot");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list snapshots"),
    }
    let Some(dir) = &ctx.config.upload_dir else {
        return unmounted;
    };
    crate::vm_imports::sweep(&ctx.cluster, ctx.config.node, dir, revision, tasks).await;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return unmounted;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 하위 디렉터리(`imports`)는 가져오기의 정리가 맡는다.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let wanted = unfinished.iter().any(|t| match &t.spec.op {
            TaskOp::VolumeImport { source, .. } => *source == path,
            TaskOp::VolumeDownload { .. } => download::path(dir, t.spec.target) == path,
            _ => false,
        });
        if !wanted {
            tracing::warn!(path = %path.display(), "removing an upload without a task");
            let _ = std::fs::remove_file(&path);
        }
    }
    unmounted
}
