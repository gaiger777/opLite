//! 파일 복원(계획 69) 시험 도우미: 이미지를 검사한 것으로 둔 노드 상태와 세션 등록부, 노드 상태의 `file_restore`, 옛 풀
//! 백업(매니페스트와 빈 디스크 파일 — 가짜 하이퍼바이저의 도우미는 파일을 읽지 않는다).

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use opl_driver::{DriverError, Hypervisor, MockHypervisor};
use opl_peer::{GuestAgentError, GuestAgentSource};
use opl_types::file_restore::{GuestFileAnswer, GuestFileOp};
use opl_types::guest::{GuestInfoResponse, GuestInfoState};

use opl_api::FileRestoreSessions;
use opl_cluster::Cluster;
use opl_driver::file_restore::{CheckedImage, FileRestoreState, HelperConfig, SessionLimits};
use opl_qemu::HostProfile;
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupPhase, BackupSpec, MANIFEST_FILE, MANIFEST_VERSION,
    backup_dir,
};
use opl_types::file_restore::FileRestoreNode;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::version::BUILD;
use opl_types::vm::{Arch, VmSpec};
use uuid::Uuid;

/// 시험 이미지의 판(`orbivisor <BUILD> lock-…`)
pub fn image_version() -> String {
    format!("orbivisor {BUILD} lock-000000000000")
}

/// 이미지를 검사한 것으로 둔 노드의 파일 복원 상태(세션 상한 `max_sessions`, 유휴 `idle`, 최대 `max`)
pub fn node_state(
    dir: &Path,
    max_sessions: u32,
    idle: Duration,
    max: Duration,
) -> Arc<FileRestoreState> {
    let state = Arc::new(FileRestoreState::new(
        dir.join("image"),
        &HostProfile::homebrew_hvf(Path::new("/nonexistent")),
        BUILD,
        HelperConfig::default(),
        SessionLimits {
            max_sessions,
            idle,
            max,
            ..SessionLimits::default()
        },
    ));
    state.set_checked_image(CheckedImage {
        arch: Arch::Aarch64,
        kernel: "/nonexistent/Image".into(),
        initrd: "/nonexistent/initrd.img".into(),
        image_version: image_version(),
        kernel_release: "6.12.0-test".into(),
    });
    state
}

/// 세션 등록부(세션 디렉터리는 `dir` 아래)
pub fn sessions(dir: &Path, state: Option<Arc<FileRestoreState>>) -> Arc<FileRestoreSessions> {
    Arc::new(FileRestoreSessions::new(
        state,
        dir.join("run"),
        dir.join("state"),
    ))
}

/// 노드 상태의 `file_restore`를 쓴다(opld의 알림 루프 대신). `None`이면 지운다.
pub async fn report(cluster: &Cluster, node: Uuid, value: Option<FileRestoreNode>) {
    let mut status = cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
    status.file_restore = value;
    cluster
        .update_status::<NodeSpec>(node, &status)
        .await
        .unwrap();
}

/// 이미지가 있는 노드의 알림 값
pub fn reported(sessions: u32, max_sessions: u32) -> FileRestoreNode {
    FileRestoreNode {
        image_version: image_version(),
        arch: Arch::Aarch64,
        accel: "hvf".into(),
        sessions,
        max_sessions,
    }
}

/// 백업 풀 `pool`(디렉터리 `root/pool`, `nodes`가 비면 모든 노드)
pub async fn backup_pool(cluster: &Cluster, root: &Path, pool: &str, nodes: Vec<Uuid>) {
    let path = root.join(pool);
    std::fs::create_dir_all(&path).unwrap();
    let spec = StoragePoolSpec {
        backend: PoolBackend::Dir { path },
        nodes,
        shared: false,
        backups: true,
        folder: None,
    };
    cluster.create(&Resource::new(pool, spec)).await.unwrap();
}

/// 풀 `pool`(디렉터리 `root/pool`)에 `ready` 전체 백업 `name`(디스크 하나, 파일을 쓴 노드 `node`)을 만든다.
pub async fn pool_backup(
    cluster: &Cluster,
    root: &Path,
    pool: &str,
    name: &str,
    node: Option<Uuid>,
) -> Backup {
    let vm_spec: VmSpec = serde_json::from_value(serde_json::json!({
        "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "nics": [],
    }))
    .unwrap();
    let vm = Uuid::now_v7();
    let mut backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm,
            vm_name: "web".into(),
            pool: pool.into(),
            vm_spec: vm_spec.clone(),
            job: None,
            origin: None,
            incremental: false,
            max_chain: None,
            source: None,
            store: None,
        },
    );
    let dir = backup_dir(&root.join(pool), backup.id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("disk0.qcow2"), b"not read by the fake helper").unwrap();
    let manifest = BackupManifest {
        version: MANIFEST_VERSION,
        backup: backup.id,
        parent: None,
        vm,
        vm_name: "web".into(),
        vm_spec,
        disks: vec![BackupDisk {
            name: "disk0".into(),
            file: "disk0.qcow2".into(),
            virtual_bytes: 8 << 30,
            stored_bytes: 27,
            sha256: None,
        }],
        uefi_vars: false,
        created_at: 0,
        tpm_state: false,
    };
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    backup.status.phase = BackupPhase::Ready;
    backup.status.node = node;
    cluster.create(&backup).await.unwrap();
    cluster
        .update_status::<BackupSpec>(backup.id, &backup.status)
        .await
        .unwrap();
    cluster.get::<BackupSpec>(backup.id).unwrap().unwrap()
}

/// 가짜 하이퍼바이저의 게스트 에이전트(파일 일만 — opld의 게스트 정보 캐시 대신). 하이퍼바이저는 `Env`가 뜬 뒤 넣는다.
#[derive(Default)]
pub struct HvGuest(pub OnceLock<Arc<MockHypervisor>>);

#[async_trait]
impl GuestAgentSource for HvGuest {
    async fn info(&self, _vm: Uuid, _refresh: bool) -> GuestInfoResponse {
        GuestInfoResponse {
            state: GuestInfoState::NoAgent,
            info: None,
        }
    }

    async fn set_password(&self, _vm: Uuid, _u: &str, _p: &str) -> Result<(), GuestAgentError> {
        Err(GuestAgentError::Unsupported)
    }

    async fn file(&self, vm: Uuid, op: GuestFileOp) -> Result<GuestFileAnswer, GuestAgentError> {
        let hv = self.0.get().ok_or(GuestAgentError::Unsupported)?;
        hv.guest_file(vm, &op).await.map_err(|e| match e {
            DriverError::NotRunning(_) => GuestAgentError::NotRunning,
            DriverError::InvalidSpec(_) => GuestAgentError::NoAgent,
            e => GuestAgentError::Unavailable(e.to_string()),
        })
    }
}
