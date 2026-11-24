//! 실제 도우미 이미지로 파일 복원 도우미 VM을 띄우는 시험(계획 69 설계 §14, 스테이지 4 탐침).
//! `OPL_RESTORE_IMAGE_DIR=<이미지 디렉터리> OPL_RESTORE_FIXTURES=<고정물> cargo test -p opl-driver --features restore-it
//! --test restore_helper -- --nocapture --test-threads 1` (탐침 값은 "PROBE" 줄). 이미지는 `packaging/restore-image/build.sh`,
//! 고정물은 `crates/opl-restore/tests/fixtures/make-disks.sh`(LVM은 root). 고르는 것: `OPL_RESTORE_QEMU`·
//! `OPL_RESTORE_QEMU_IMG`(바이너리), `OPL_RESTORE_ACCEL=tcg`(가속 강제), `OPL_RESTORE_BIG_MIB`(흘리기 크기, 기본 256 —
//! 이미지는 고정물 디렉터리에 `big-<MiB>.img`로 남겨 다시 쓴다. mke2fs가 없는 macOS는 Linux에서 만든 것을 옮겨 쓴다).
//! 권한 내림(`-run-with user=nobody`)·sandbox는 Linux에서 root로 돌릴 때만 본다(`OPL_RESTORE_SCOPE=1`이면 systemd scope도).
//! - 고정물: 탐지 결과, 파일마다 SHA-256(NBD 원본 ext4·qcow2 원본 나머지), 더러운 ext4 저널이 덮개 위에서 재생됨, 같은 UUID
//!   XFS 둘(`nouuid`), MBR 논리·GPT, LUKS는 "not supported", LVM 선형·씬, 있으면 XFS 더러운 로그(`xfs-dirty.img`)
//! - 흘리기 MB/s, 덮개 한도, QEMU `kill -9`, opld가 다시 뜬 것처럼 남은 것 치우기
#![cfg(feature = "restore-it")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opl_driver::file_restore::{
    CheckedImage, HelperConfig, HelperLaunch, HelperPaths, HelperSource, RestoreHelper, StreamItem,
    check_image, stop_helper,
};
use opl_driver::{Hypervisor, QemuConfig, QemuHypervisor};
use opl_qemu::{Accel, HostProfile, LiveRestoreSource};
use opl_restore::agent::fs::path_of;
use opl_restore::proto::{Reply, Volume};
use opl_store::nbd::NbdServer;
use opl_store::nbd::export::MemoryExport;
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from)
}

fn short_tempdir() -> tempfile::TempDir {
    // macOS 유닉스 소켓 경로 한도
    tempfile::Builder::new()
        .prefix("p69")
        .tempdir_in("/tmp")
        .unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    hv: QemuHypervisor,
    config: QemuConfig,
    image: CheckedImage,
    fixtures: PathBuf,
    qemu_img: PathBuf,
}

fn host() -> HostProfile {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    let mut host = HostProfile::native(Path::new(&share));
    if let Some(q) = env_path("OPL_RESTORE_QEMU") {
        host.qemu_binary = q;
    }
    if std::env::var("OPL_RESTORE_ACCEL").as_deref() == Ok("tcg") {
        host.accel = Accel::Tcg;
    }
    host
}

async fn env() -> Option<Env> {
    let (Some(images), Some(fixtures)) = (
        env_path("OPL_RESTORE_IMAGE_DIR"),
        env_path("OPL_RESTORE_FIXTURES"),
    ) else {
        eprintln!("skipping: set OPL_RESTORE_IMAGE_DIR and OPL_RESTORE_FIXTURES");
        return None;
    };
    let host = host();
    let image = check_image(&images, host.arch, opl_types::version::BUILD).unwrap();
    let dir = short_tempdir();
    let root = dir.path().to_path_buf();
    let config = QemuConfig {
        host,
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: None,
        // `OPL_RESTORE_SCOPE=1`(Linux root·systemd): `opl-fr-<id>.scope`로 감싼다
        systemd_scope: std::env::var_os("OPL_RESTORE_SCOPE").is_some(),
        swtpm: Default::default(),
    };
    let hv = QemuHypervisor::new(config.clone()).await.unwrap();
    let qemu_img = env_path("OPL_RESTORE_QEMU_IMG").unwrap_or_else(|| "qemu-img".into());
    Some(Env {
        _dir: dir,
        root,
        hv,
        config,
        image,
        fixtures,
        qemu_img,
    })
}

impl Env {
    fn helper_config(&self) -> HelperConfig {
        HelperConfig {
            qemu_img: self.qemu_img.clone(),
            ..HelperConfig::default()
        }
    }

    /// 고정물 날 이미지를 qcow2 백업처럼(옛 풀 백업 원본) 바꾼다
    fn qcow2(&self, name: &str) -> Option<HelperSource> {
        let raw = self.fixtures.join(name);
        if !raw.exists() {
            eprintln!("no fixture {name}");
            return None;
        }
        let out = self.root.join(format!("{name}.qcow2"));
        if !out.exists() {
            let st = std::process::Command::new(&self.qemu_img)
                .args(["convert", "-q", "-f", "raw", "-O", "qcow2"])
                .arg(&raw)
                .arg(&out)
                .status()
                .unwrap();
            assert!(st.success());
        }
        Some(HelperSource {
            source: LiveRestoreSource::Qcow2 { chain: vec![out] },
            size: std::fs::metadata(&raw).unwrap().len(),
        })
    }

    async fn start(&self, id: Uuid, disks: Vec<HelperSource>, cfg: &HelperConfig) -> RestoreHelper {
        let launch = HelperLaunch {
            id,
            image: self.image.clone(),
            disks,
        };
        let started = Instant::now();
        let helper = Box::pin(self.hv.start_restore_helper(cfg, &launch))
            .await
            .unwrap();
        println!(
            "PROBE boot: accel={} disks={} start→hello {:.2}s (returned after {:.2}s), kernel {}",
            self.config.host.accel.as_str(),
            launch.disks.len(),
            helper.boot_time.as_secs_f64(),
            started.elapsed().as_secs_f64(),
            helper.hello.kernel
        );
        helper
    }
}

/// opld NBD 서버처럼 날 이미지 하나를 `disk0`으로 내보낸다(저장소 백업 길)
async fn serve_nbd(socket: &Path, raw: &Path) -> tokio::sync::oneshot::Sender<()> {
    let mut server = NbdServer::new();
    server.add(
        "disk0",
        Arc::new(MemoryExport::new(
            std::fs::read(raw).unwrap(),
            BTreeMap::new(),
        )),
    );
    let _ = std::fs::remove_file(socket);
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(Arc::new(server).serve(listener, async {
        let _ = rx.await;
    }));
    tx
}

async fn read_file(h: &RestoreHelper, volume: &str, path: &str) -> Vec<u8> {
    let (size, mut s) = h
        .client
        .read(volume, path_of(path))
        .await
        .unwrap_or_else(|e| panic!("read {volume}:{path}: {e}"));
    let mut out = Vec::with_capacity(size as usize);
    while let Some(item) = s.next().await.unwrap() {
        match item {
            StreamItem::Data(d) => out.extend(d),
            StreamItem::Reply(r) => panic!("{r:?}"),
        }
    }
    assert_eq!(out.len() as u64, size);
    out
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// `expected.sha256`(`<해시>  <상대 경로>`)
fn expected(fixtures: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(fixtures.join("expected.sha256"))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once("  "))
        .map(|(h, p)| (h.to_string(), p.to_string()))
        .collect()
}

async fn check_tree(h: &RestoreHelper, volume: &str, files: &[(String, String)], skip: &[&str]) {
    let note = h.client.mount(volume).await.unwrap();
    for (hash, path) in files {
        if skip.iter().any(|s| path.contains(s)) {
            continue;
        }
        let data = read_file(h, volume, path).await;
        assert_eq!(&sha(&data), hash, "{volume}:{path} (mount note {note:?})");
    }
}

fn find<'a>(vols: &'a [Volume], id: &str) -> &'a Volume {
    vols.iter()
        .find(|v| v.id == id)
        .unwrap_or_else(|| panic!("no volume {id}: {vols:#?}"))
}

#[cfg(target_os = "linux")]
fn proc_status(pid: u32, key: &str) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
}

/// 고정물 디스크를 모두 붙여 탐지·마운트·읽기를 견준다. root면 권한 내림·sandbox도 본다.
#[tokio::test(flavor = "multi_thread")]
async fn fixtures_are_browsed_through_the_helper() {
    let Some(env) = env().await else { return };
    let files = expected(&env.fixtures);
    let id = Uuid::now_v7();
    let paths = HelperPaths::new(&env.config.run_dir, &env.config.state_dir, id);
    paths.prepare().unwrap();
    // disk0: NBD(저장소 백업 길), 나머지: qcow2
    let stop_nbd = serve_nbd(&paths.nbd_socket(), &env.fixtures.join("ext4.img")).await;
    let mut disks = vec![HelperSource {
        source: LiveRestoreSource::Nbd {
            socket: paths.nbd_socket(),
            export: "disk0".into(),
        },
        size: std::fs::metadata(env.fixtures.join("ext4.img"))
            .unwrap()
            .len(),
    }];
    let mut names = vec!["ext4.img(nbd)".to_string()];
    for name in [
        "ext4-dirty.img",
        "xfs.img",
        "xfs.img",
        "mbr.img",
        "gpt.img",
        "luks.img",
        "lvm.img",
        "xfs-dirty.img",
        "vfat.img",
        "exfat.img",
        "btrfs.img",
        "ntfs.img",
    ] {
        if let Some(d) = env.qcow2(name) {
            disks.push(d);
            names.push(name.to_string());
        }
    }
    let disk_of = |name: &str| names.iter().position(|n| n == name);
    let cfg = env.helper_config();
    let h = env.start(id, disks, &cfg).await;
    let t = Instant::now();
    let vols = h.client.probe().await.unwrap();
    println!(
        "PROBE probe: {:.2}s, {} volumes",
        t.elapsed().as_secs_f64(),
        vols.len()
    );
    for v in &vols {
        println!(
            "PROBE volume {:<22} disk{:<2} {:?} fs={:?} label={:?} mountable={} note={:?} ({})",
            v.id,
            v.disk,
            v.kind,
            v.fs,
            v.label,
            v.mountable,
            v.note,
            names.get(v.disk as usize).map_or("?", String::as_str)
        );
    }

    if env.config.systemd_scope {
        let out = std::process::Command::new("systemctl")
            .args([
                "show",
                "-p",
                "MemoryMax,TasksMax,CPUQuotaPerSecUSec,ActiveState",
            ])
            .arg(format!("opl-fr-{id}.scope"))
            .output()
            .unwrap();
        let show = String::from_utf8_lossy(&out.stdout).replace('\n', " ");
        println!("PROBE scope: {show}");
        assert!(show.contains("ActiveState=active"), "{show}");
    }
    #[cfg(target_os = "linux")]
    if rustix::process::geteuid().is_root() {
        let pid = h.pid.unwrap();
        let uid = proc_status(pid, "Uid:").unwrap();
        println!(
            "PROBE privileges: Uid {uid}, Seccomp {:?}",
            proc_status(pid, "Seccomp:")
        );
        assert!(
            !uid.starts_with("0\t") && !uid.starts_with("0 "),
            "QEMU still runs as root: {uid}"
        );
    }

    // ext4 over NBD: 나무 전체
    let t = Instant::now();
    check_tree(&h, "disk0", &files, &[]).await;
    println!(
        "PROBE ext4(nbd) tree read: {:.2}s",
        t.elapsed().as_secs_f64()
    );
    let (root, _) = h.client.list("disk0", vec![], None, 1000).await.unwrap();
    let root: Vec<String> = root.iter().map(|e| e.name.lossy()).collect();
    for want in [
        "hello.txt",
        "dir",
        "link",
        "한글-이름.txt",
        "big.bin",
        "empty",
    ] {
        assert!(root.iter().any(|n| n == want), "{want} in {root:?}");
    }
    let link = h.client.stat("disk0", path_of("link")).await.unwrap();
    assert_eq!(link.link_target.unwrap().as_bytes(), b"hello.txt");
    let mut walk = h
        .client
        .walk("disk0", vec![path_of("dir")], false)
        .await
        .unwrap();
    let mut items = Vec::new();
    while let Some(i) = walk.next().await.unwrap() {
        if let StreamItem::Reply(Reply::Item(i)) = i {
            items.push(opl_restore::proto::display_path(&i.path));
        }
    }
    assert_eq!(items, ["/dir", "/dir/nested", "/dir/nested/deep.txt"]);

    // 더러운 저널: 덮개 위에서 재생된다
    if let Some(i) = disk_of("ext4-dirty.img") {
        let vol = format!("disk{i}");
        let note = h.client.mount(&vol).await.unwrap();
        let hello = read_file(&h, &vol, "hello.txt").await;
        println!(
            "PROBE ext4-dirty: note={note:?} hello starts {:?}",
            String::from_utf8_lossy(&hello[..hello.len().min(17)])
        );
        assert!(
            hello.starts_with(b"journal replayed"),
            "journal not replayed (note {note:?})"
        );
        assert_eq!(note, None);
    }
    // 같은 UUID XFS 둘(nouuid)
    let xfs: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| *n == "xfs.img")
        .map(|(i, _)| i)
        .collect();
    for i in &xfs {
        check_tree(&h, &format!("disk{i}"), &files, &[]).await;
    }
    if xfs.len() == 2 {
        println!("PROBE xfs: two filesystems with the same UUID both mounted (nouuid)");
    }
    if let Some(i) = disk_of("xfs-dirty.img") {
        let vol = format!("disk{i}");
        let note = h.client.mount(&vol).await.unwrap();
        let replayed = read_file(&h, &vol, "replay.txt").await;
        println!(
            "PROBE xfs-dirty: note={note:?} replay.txt={:?}",
            String::from_utf8_lossy(&replayed)
        );
        assert_eq!(replayed, b"replayed\n");
    }
    if let Some(i) = disk_of("mbr.img") {
        check_tree(&h, &format!("disk{i}p1"), &files, &[]).await;
        assert_eq!(
            find(&vols, &format!("disk{i}p5")).fs.as_deref(),
            Some("vfat")
        );
        let hello = read_file(&h, &format!("disk{i}p5"), "hello.txt").await;
        assert_eq!(hello, b"hello from orbivisor\n");
    }
    if let Some(i) = disk_of("gpt.img") {
        check_tree(&h, &format!("disk{i}p1"), &files, &[]).await;
        let esp = find(&vols, &format!("disk{i}p2"));
        assert_eq!(esp.fs.as_deref(), Some("vfat"));
    }
    if let Some(i) = disk_of("luks.img") {
        let v = find(&vols, &format!("disk{i}"));
        assert!(!v.mountable);
        assert!(v.note.as_deref().unwrap_or("").contains("LUKS"), "{v:?}");
    }
    if disk_of("lvm.img").is_some() {
        check_tree(&h, "lvm/oplfixvg/lin", &files, &[]).await;
        check_tree(&h, "lvm/oplfixvg/thin", &files, &[]).await;
        println!("PROBE lvm: linear and thin LVs read");
    }
    if let Some(i) = disk_of("vfat.img") {
        let hello = read_file(&h, &format!("disk{i}"), "hello.txt").await;
        assert_eq!(hello, b"hello from orbivisor\n");
    }
    if let Some(i) = disk_of("btrfs.img") {
        check_tree(&h, &format!("disk{i}"), &files, &[]).await;
    }
    let overlay: u64 = (0..names.len())
        .filter_map(|i| std::fs::metadata(paths.overlay(i)).ok())
        .map(|m| m.len())
        .sum();
    println!("PROBE overlays after browsing: {} KiB", overlay >> 10);

    let pid = h.pid.unwrap();
    let t = Instant::now();
    stop_helper(h).await;
    println!("PROBE stop: {:.2}s", t.elapsed().as_secs_f64());
    let _ = stop_nbd.send(());
    assert!(!paths.run.exists() && !paths.state.exists());
    if env.config.systemd_scope {
        let out = std::process::Command::new("systemctl")
            .args(["is-active", &format!("opl-fr-{id}.scope")])
            .output()
            .unwrap();
        assert!(!out.status.success(), "the scope is still there");
    }
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid as i32).unwrap())
            .is_err()
    );
}

/// 큰 파일 하나를 흘려 받는 속도(§4 탐침)
#[tokio::test(flavor = "multi_thread")]
async fn streaming_throughput() {
    let Some(env) = env().await else { return };
    let mib: u64 = std::env::var("OPL_RESTORE_BIG_MIB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);
    // 자리마다 다른 내용(압축·0 건너뛰기에 속지 않게)
    let mut data = vec![0u8; (mib << 20) as usize];
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for chunk in data.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
    }
    let want = sha(&data);
    // 만든 이미지는 고정물 디렉터리에 둔다(mke2fs가 없는 macOS는 Linux에서 만든 것을 옮겨 쓴다).
    let img = env.fixtures.join(format!("big-{mib}.img"));
    if !img.exists() {
        let mke2fs = [
            "mke2fs",
            "/opt/homebrew/opt/e2fsprogs/sbin/mke2fs",
            "/usr/sbin/mke2fs",
        ]
        .into_iter()
        .find(|p| std::process::Command::new(p).arg("-V").output().is_ok());
        let Some(mke2fs) = mke2fs else {
            eprintln!("skipping: no mke2fs and no {}", img.display());
            return;
        };
        let tree = env.root.join("big-tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("big.bin"), &data).unwrap();
        let st = std::process::Command::new(mke2fs)
            .args(["-q", "-F", "-t", "ext4", "-d"])
            .arg(&tree)
            .arg(&img)
            .arg(format!("{}M", mib + mib / 8 + 32))
            .status()
            .unwrap();
        assert!(st.success());
        std::fs::remove_dir_all(&tree).unwrap();
    }
    drop(data);
    let out = env.root.join("big.qcow2");
    let st = std::process::Command::new(&env.qemu_img)
        .args(["convert", "-q", "-f", "raw", "-O", "qcow2"])
        .arg(&img)
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success());
    let size = std::fs::metadata(&img).unwrap().len();
    let h = env
        .start(
            Uuid::now_v7(),
            vec![HelperSource {
                source: LiveRestoreSource::Qcow2 { chain: vec![out] },
                size,
            }],
            &env.helper_config(),
        )
        .await;
    h.client.probe().await.unwrap();
    h.client.mount("disk0").await.unwrap();
    let t = Instant::now();
    let (n, mut s) = h.client.read("disk0", path_of("big.bin")).await.unwrap();
    // 받는 동안은 해시하지 않는다(디버그 빌드의 SHA-256이 재는 값을 끌어내리지 않게).
    let mut chunks = Vec::new();
    let mut got = 0u64;
    while let Some(StreamItem::Data(d)) = s.next().await.unwrap() {
        got += d.len() as u64;
        chunks.push(d);
    }
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(got, n);
    let mut hasher = Sha256::new();
    for c in &chunks {
        hasher.update(c);
    }
    assert_eq!(hex::encode(hasher.finalize()), want);
    println!(
        "PROBE stream: {mib} MiB in {secs:.2}s = {:.0} MB/s (accel {})",
        (n as f64 / 1e6) / secs,
        env.config.host.accel.as_str()
    );
    stop_helper(h).await;
}

/// 덮개 한도를 넘으면 지킴이가 세션을 닫고, QEMU가 `kill -9`로 죽으면 세션이 끝나 치워진다.
#[tokio::test(flavor = "multi_thread")]
async fn guard_and_kill_end_the_session() {
    let Some(env) = env().await else { return };
    let Some(disk) = env.qcow2("ext4-dirty.img") else {
        return;
    };
    // 덮개 1바이트: 첫 지킴이 회차(10초)에 닫힌다.
    let tiny = HelperConfig {
        overlay_max: 1,
        ..env.helper_config()
    };
    let id = Uuid::now_v7();
    let h = env.start(id, vec![disk.clone()], &tiny).await;
    let reason = tokio::time::timeout(Duration::from_secs(30), h.ended())
        .await
        .unwrap();
    assert!(
        reason.contains("journal replay wrote more than 1 MiB"),
        "{reason}"
    );
    let err = h.client.probe().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("the file restore session is closed: {reason}")
    );
    println!("PROBE overlay guard: {reason} / {err}");
    let paths = HelperPaths::new(&env.config.run_dir, &env.config.state_dir, id);
    stop_helper(h).await;
    assert!(!paths.run.exists() && !paths.state.exists());

    let id = Uuid::now_v7();
    let h = env
        .start(id, vec![disk.clone()], &env.helper_config())
        .await;
    let pid = h.pid.unwrap();
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid as i32).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(10), h.ended())
        .await
        .unwrap();
    println!("PROBE kill -9: {reason}");
    assert!(reason.contains("exited"), "{reason}");
    assert!(h.client.closed_reason().is_some());
    stop_helper(h).await;
    let paths = HelperPaths::new(&env.config.run_dir, &env.config.state_dir, id);
    assert!(!paths.run.exists() && !paths.state.exists());

    // opld가 죽은 것처럼 도우미를 버려 두고, 새 드라이버의 치우기가 QEMU·파일을 지운다.
    let id = Uuid::now_v7();
    let h = env.start(id, vec![disk], &env.helper_config()).await;
    let pid = h.pid.unwrap();
    std::mem::forget(h);
    let again = QemuHypervisor::new(env.config.clone()).await.unwrap();
    assert_eq!(again.sweep_restore_helpers().await, 1);
    let paths = HelperPaths::new(&env.config.run_dir, &env.config.state_dir, id);
    assert!(!paths.run.exists() && !paths.state.exists());
    let gone = (0..250).any(|_| {
        std::thread::sleep(Duration::from_millis(20));
        opl_driver_cmdline_gone(pid)
    });
    assert!(gone, "QEMU {pid} still runs");
}

/// pid가 끝났거나 좀비(이 시험 프로세스의 자식이라 아직 거두지 않음)
fn opl_driver_cmdline_gone(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    stat.trim().is_empty() || stat.trim().starts_with('Z')
}
