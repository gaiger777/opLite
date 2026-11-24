//! 파일 복원 도우미 이미지 검사와 노드 알림(계획 69 §8.4·§10.3). 시작할 때와 [`CHECK_EVERY`]마다 `image_dir`의
//! `manifest.json`이 바뀌었으면 이미지를 다시 검사하고([`FileRestoreState::refresh_image`]), 세션 수가 바뀌면 곧바로
//! `NodeStatus.file_restore`를 맞춘다. 이미지가 없거나 검사를 지나지 못하면 쓰지 않는다(None — 경고 한 줄).
//!
//! 모든 노드가 기능 `file-restore`를 알릴 때까지는 쓰지 않는다([`file_restore_ready`]): 옛 노드는 이 칸을 몰라 노드
//! 상태를 다시 쓰며 지우고, 그러면 회차마다 다시 쓰게 된다.

use std::sync::Arc;
use std::time::Duration;

use opl_cluster::Cluster;
use opl_driver::file_restore::FileRestoreState;
use opl_types::file_restore::FileRestoreNode;
use opl_types::node::FILE_RESTORE;
use tokio::task::JoinHandle;

/// manifest 바뀜을 보는 간격
pub const CHECK_EVERY: Duration = Duration::from_secs(60);

/// 이 노드 밖의 모든 노드가 기능 `file-restore`를 알린다(이 노드는 이 빌드라 안다). 읽지 못하면 false다.
pub fn file_restore_ready(cluster: &Cluster, me: &str) -> bool {
    match opl_api::cluster::missing_feature(cluster, FILE_RESTORE) {
        Ok(old) => old.iter().all(|n| n == me),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the node features");
            false
        }
    }
}

/// `NodeStatus.file_restore`에 쓸 값: 기능이 모든 노드에 있고 이미지가 검사를 지났을 때만
pub fn node_value(
    cluster: &Cluster,
    me: &str,
    state: Option<&FileRestoreState>,
) -> Option<FileRestoreNode> {
    state
        .filter(|_| file_restore_ready(cluster, me))
        .and_then(FileRestoreState::node)
}

/// 노드 상태를 지금 값으로 맞춘다(같으면 쓰지 않는다)
pub async fn record(cluster: &Cluster, me: &str, state: &FileRestoreState) -> anyhow::Result<()> {
    let value = node_value(cluster, me, Some(state));
    crate::bootstrap::update_node_status(cluster, |status| {
        if status.file_restore == value {
            return false;
        }
        status.file_restore = value.clone();
        true
    })
    .await
}

/// 이미지를 검사하고 노드 상태를 맞추는 루프: 세션 수가 바뀌면 곧바로, 아니면 [`CHECK_EVERY`]마다(manifest가 바뀌었을
/// 때만 다시 해시를 맞춘다). 처음 한 번은 곧바로 한다.
pub fn spawn(
    cluster: Arc<Cluster>,
    state: Arc<FileRestoreState>,
    me: String,
    every: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sessions = state.subscribe();
        loop {
            state.refresh_image().await;
            if let Err(e) = record(&cluster, &me, &state).await {
                tracing::warn!(error = %e, "cannot record the file restore state");
            }
            tokio::select! {
                changed = sessions.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(every) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use opl_driver::file_restore::{HelperConfig, SessionLimits};
    use opl_qemu::{Accel, HostProfile};
    use opl_types::node::NodeSpec;
    use opl_types::vm::Arch;

    use super::*;
    use crate::bootstrap::tests::single_node;

    fn write_image(dir: &std::path::Path) {
        let d = dir.join("aarch64");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("Image"), b"kernel").unwrap();
        std::fs::write(d.join("initrd.img"), b"initrd").unwrap();
        let sha = |b: &[u8]| {
            use sha2::Digest;
            hex::encode(sha2::Sha256::digest(b))
        };
        let manifest = serde_json::json!({
            "arch": "aarch64",
            "files": { "Image": sha(b"kernel"), "initrd.img": sha(b"initrd") },
            "image_version": format!("orbivisor {} lock-233004539824", opl_types::version::BUILD),
            "initrd": "initrd.img",
            "kernel": "Image",
            "kernel_release": "6.12",
            "proto": 2,
        });
        std::fs::write(d.join("manifest.json"), manifest.to_string()).unwrap();
    }

    /// 이미지가 생기면 알리고, 세션 수가 바뀌면 곧바로 다시 쓴다. 이미지가 사라지면 지운다.
    #[tokio::test]
    async fn the_reporter_follows_the_image_and_the_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        crate::bootstrap::register_node(
            &cluster,
            &crate::bootstrap::tests::test_config(),
            &crate::host::Host::from_config(&crate::bootstrap::tests::test_config(), "t").unwrap(),
        )
        .await
        .unwrap();
        let images = dir.path().join("images");
        let state = Arc::new(FileRestoreState::new(
            images.clone(),
            &HostProfile::debian(Arch::Aarch64, Accel::Tcg),
            opl_types::version::BUILD,
            HelperConfig::default(),
            SessionLimits::default(),
        ));
        let me = crate::bootstrap::tests::test_config().name;
        let reporter = spawn(
            cluster.clone(),
            state.clone(),
            me,
            Duration::from_millis(50),
        );
        let recorded = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .file_restore
        };
        let wait = |want: Option<u32>| {
            let recorded = &recorded;
            async move {
                for _ in 0..200 {
                    if recorded().map(|n| n.sessions) == want {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                panic!("file_restore is {:?}, want sessions {want:?}", recorded());
            }
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(recorded(), None);
        write_image(&images);
        wait(Some(0)).await;
        let node = recorded().unwrap();
        assert_eq!(
            (node.arch, node.accel.as_str(), node.max_sessions),
            (Arch::Aarch64, "tcg", 4)
        );
        let slot = state.try_open().unwrap();
        wait(Some(1)).await;
        drop(slot);
        wait(Some(0)).await;
        std::fs::remove_dir_all(&images).unwrap();
        wait(None).await;
        reporter.abort();
        cluster.shutdown().await.unwrap();
    }
}
