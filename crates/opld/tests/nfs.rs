//! 실제 NFS 풀: opld가 `mount -t nfs`로 localhost의 내보내기를 마운트하고, 그 풀에 볼륨을 만들고
//! qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면 언마운트한다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_NFS_EXPORT=/srv/opl-nfs cargo test -p opld --features nfs-it --test nfs`
//! Linux에서 root, NFS 클라이언트(`mount.nfs`), qemu-img가 필요하다. `OPL_TEST_NFS_EXPORT`는
//! 127.0.0.1에 `rw,no_root_squash`로 내보낸 디렉터리다(서버 쪽 경로로 파일을 확인한다).
#![cfg(all(feature = "nfs-it", target_os = "linux"))]

mod common;

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Daemon, eventually};
use oplctl::Client;
use serde_json::Value;

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
fn client(d: &Daemon) -> Client {
    let config: Value =
        serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
    Client::new(
        config["server"].as_str().unwrap(),
        config["token"].as_str().map(str::to_string),
        config["ca_pem"].as_str().map(str::to_string),
    )
    .unwrap()
}

/// NFS 마운트 지점들(`/proc/self/mountinfo`의 마운트 지점과 종류)
fn nfs_mount_points() -> Vec<PathBuf> {
    let info = std::fs::read_to_string(opl_driver::mounts::MOUNTINFO).unwrap();
    opl_driver::mounts::nfs_mount_points(&info)
        .into_iter()
        .collect()
}

/// 그 경로에 NFS 마운트가 있는지
fn nfs_mounted(dir: &Path) -> bool {
    nfs_mount_points().iter().any(|p| p == dir)
}

/// 테스트가 중간에 실패해도 데몬의 임시 디렉터리를 지우기 전에 opld를 멈추고 `{data_dir}/pools`
/// 아래의 NFS 마운트를 푼다. 마운트된 채로 지우면 내보내기의 파일까지 지운다. 풀지 못하면 임시
/// 디렉터리를 지우지 않고 남긴다.
struct Unmounting(Daemon);

impl Deref for Unmounting {
    type Target = Daemon;

    fn deref(&self) -> &Daemon {
        &self.0
    }
}

impl DerefMut for Unmounting {
    fn deref_mut(&mut self) -> &mut Daemon {
        &mut self.0
    }
}

impl Drop for Unmounting {
    fn drop(&mut self) {
        // opld가 다시 마운트하지 않게 먼저 멈춘다.
        if self.0.is_running() {
            self.0.kill();
        }
        let Ok(pools) = std::fs::canonicalize(self.0.data_dir().join("pools")) else {
            return;
        };
        let mut left = false;
        for point in nfs_mount_points().iter().filter(|p| p.starts_with(&pools)) {
            let unmounted = Command::new("umount")
                .arg(point)
                .status()
                .is_ok_and(|s| s.success());
            if !unmounted {
                eprintln!("cannot unmount {}", point.display());
                left = true;
            }
        }
        if left {
            let dir = std::mem::replace(&mut self.0.dir, tempfile::TempDir::new().unwrap());
            eprintln!(
                "left {} in place because nfs pools are still mounted under it",
                dir.keep().display()
            );
        }
    }
}

/// 볼륨 파일에 든 qcow2 스냅샷 태그
fn tags(file: &Path) -> Vec<String> {
    let out = Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(file)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    let info: Value = serde_json::from_slice(&out.stdout).unwrap();
    info["snapshots"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn nfs_pool_mounts_holds_volumes_and_unmounts() {
    let export = PathBuf::from(
        std::env::var("OPL_TEST_NFS_EXPORT")
            .expect("OPL_TEST_NFS_EXPORT must name a directory exported to 127.0.0.1"),
    );
    let mut d = Unmounting(Daemon::new("node1", ""));
    d.start().await;
    d.init("nfs").await;
    d.login().await;
    let c = client(&d);

    // oplctl로 만든 NFS 풀은 shared를 적지 않아도 공유다. 만들면 곧바로 마운트한다.
    let source = format!("127.0.0.1:{}", export.display());
    d.ctl(&["pool", "create", "nas", "--nfs", &source])
        .await
        .unwrap();
    d.wait_pool_ready("nas").await;
    let pool = c.get("/storage-pools/nas").await.unwrap();
    assert_eq!(pool["spec"]["shared"], true, "{pool}");
    let mount_point = std::fs::canonicalize(d.data_dir())
        .unwrap()
        .join("pools")
        .join(pool["id"].as_str().unwrap());
    assert!(nfs_mounted(&mount_point), "{}", mount_point.display());
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["nas"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");

    // 볼륨 파일은 마운트를 거쳐 서버의 내보내기 디렉터리에 생긴다.
    d.ctl(&["volume", "create", "root", "--pool", "nas", "--size", "64M"])
        .await
        .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let file_name = format!("{}.qcow2", volume["id"].as_str().unwrap());
    let file = export.join(&file_name);
    assert!(file.exists(), "{}", file.display());
    assert!(mount_point.join(&file_name).exists());

    // 꺼진 VM의 스냅샷은 NFS 위 qcow2의 내부 스냅샷이다.
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    d.ctl(&["vm", "snapshot", "create", "web", "before"])
        .await
        .unwrap();
    let snapshot = c.get("/vms/web/snapshots/before").await.unwrap();
    assert_eq!(snapshot["status"]["phase"], "ready", "{snapshot}");
    assert_eq!(
        tags(&file),
        vec![snapshot["id"].as_str().unwrap().to_string()]
    );
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert!(tags(&file).is_empty());
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(!file.exists(), "{}", file.display());

    // 풀을 지우면 언마운트하고 마운트 지점도 지운다.
    d.ctl(&["pool", "delete", "nas"]).await.unwrap();
    eventually("nfs pool unmounted", || async {
        !nfs_mounted(&mount_point) && !mount_point.exists()
    })
    .await;
    d.stop();
}
