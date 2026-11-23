//! NBD 클라이언트·서버가 QEMU 도구와 맞는지(계획 68 설계 §14 "NBD"): 우리 클라이언트 ↔ `qemu-nbd`(`base:allocation`·
//! 비트맵 문맥), `qemu-img convert`·`map`·`qemu-io` ↔ 우리 서버(저장소 백업 내보내기), QEMU NBD 블록 드라이버의
//! `reconnect-delay`로 서버가 죽었다 같은 소켓으로 다시 뜨면 읽기가 이어지는지(§4 탐침).
//! `cargo test -p opl-store --features storage-it --test nbd_qemu` (qemu-img·qemu-io·qemu-nbd 필요)
#![cfg(feature = "storage-it")]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use opl_store::backend::Backend;
use opl_store::backend::mem::MemBackend;
use opl_store::nbd::export::{ChunkCache, StoreExport};
use opl_store::nbd::serve::bind;
use opl_store::nbd::{
    BASE_ALLOCATION, Extent, NbdClient, NbdServer, STATE_DIRTY, STATE_HOLE, STATE_ZERO,
    dirty_bitmap_context,
};
use opl_store::store::Store;
use opl_store::testing::write_backup;
use opl_types::backup_store::CHUNK_BYTES;
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn short_tempdir() -> tempfile::TempDir {
    // macOS의 기본 TMPDIR은 소켓 경로 한도를 넘기므로 /tmp를 쓴다.
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

fn run(cmd: &str, args: &[&str]) -> String {
    let out = Command::new(cmd).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{cmd} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 끝날 때 죽이는 자식 프로세스
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_socket(path: &Path) {
    for _ in 0..200 {
        if NbdClient::list(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{} did not answer", path.display());
}

/// `qemu-nbd`(macOS Homebrew 포함)가 유닉스 소켓으로 qcow2를 내보내고, 우리 클라이언트가 `base:allocation`과
/// `qemu:dirty-bitmap:b0` 문맥으로 구멍·쓴 곳·바뀐 곳을 본다(꺼진 VM 읽기 — 설계 §8.2, 비트맵은 §16의 나중 일).
#[tokio::test]
async fn our_client_reads_qemu_nbd_with_allocation_and_bitmap_contexts() {
    let dir = short_tempdir();
    let img = dir.path().join("a.qcow2");
    let img_s = img.to_str().unwrap();
    run("qemu-img", &["create", "-q", "-f", "qcow2", img_s, "16M"]);
    run("qemu-io", &["-c", "write -P 0x11 0 1M", img_s]);
    run("qemu-img", &["bitmap", "--add", img_s, "b0"]);
    run("qemu-io", &["-c", "write -P 0x22 5M 64k", img_s]);
    let sock = dir.path().join("n.sock");
    let _nbd = Child(
        Command::new("qemu-nbd")
            .args([
                "--read-only",
                "--persistent",
                "--shared=0",
                "-B",
                "b0",
                &format!("--socket={}", sock.display()),
                "--image-opts",
                &format!("driver=qcow2,file.driver=file,file.filename={img_s},file.locking=off"),
            ])
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    wait_socket(&sock).await;
    let bitmap = dirty_bitmap_context("b0");
    let mut c = NbdClient::connect(&sock, "", &[BASE_ALLOCATION, &bitmap])
        .await
        .unwrap();
    assert_eq!(c.size(), 16 * MIB);
    assert!(c.read_only());
    assert!(c.has_context(BASE_ALLOCATION) && c.has_context(&bitmap));
    let alloc = c.extents(BASE_ALLOCATION, 0, c.size()).await.unwrap();
    // 쓴 곳(0..1M, 5M..5M+64k)만 데이터, 나머지는 구멍·0
    let data: Vec<(u64, u64)> = spans(&alloc, |f| f & STATE_ZERO == 0);
    assert_eq!(data, vec![(0, MIB), (5 * MIB, 5 * MIB + 65536)]);
    assert!(
        alloc
            .iter()
            .filter(|e| e.flags & STATE_ZERO != 0)
            .all(|e| e.flags & STATE_HOLE != 0)
    );
    let dirty = c.extents(&bitmap, 0, c.size()).await.unwrap();
    assert_eq!(
        spans(&dirty, |f| f & STATE_DIRTY != 0),
        vec![(5 * MIB, 5 * MIB + 65536)]
    );
    assert_eq!(c.read(0, 4096).await.unwrap(), vec![0x11; 4096]);
    assert_eq!(c.read(5 * MIB, 65536).await.unwrap(), vec![0x22; 65536]);
    assert_eq!(c.read(8 * MIB, 4096).await.unwrap(), vec![0; 4096]);
    // 여러 연결(--shared=0)
    let mut c2 = NbdClient::connect(&sock, "", &[]).await.unwrap();
    assert_eq!(c2.read(MIB - 1, 2).await.unwrap(), vec![0x11, 0]);
    c.disconnect().await.unwrap();
}

/// 범위들 가운데 `keep`인 것의 (시작, 끝)
fn spans(ext: &[Extent], keep: impl Fn(u32) -> bool) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut at = 0;
    for e in ext {
        if keep(e.flags) {
            match out.last_mut() {
                Some(last) if last.1 == at => last.1 += e.length,
                _ => out.push((at, at + e.length)),
            }
        }
        at += e.length;
    }
    out
}

/// 저장소 백업 하나를 쓰고 그 디스크들을 내보내는 서버
async fn store_server() -> (Arc<NbdServer>, Vec<Vec<u8>>) {
    let backend: Arc<dyn Backend> = Arc::new(MemBackend::new());
    let store = Arc::new(Store::init(backend, Uuid::now_v7(), None, 0).await.unwrap());
    let cb = CHUNK_BYTES as usize;
    // 청크마다 한 바이트 무늬: 0x5a, 0(0 청크), 0xa5, 짧은 마지막 청크 0x33
    let mut d0 = vec![0x5a; cb];
    d0.extend(vec![0; cb]);
    d0.extend(vec![0xa5; cb]);
    d0.extend(vec![0x33; cb / 2]);
    let images = vec![d0, vec![0x77; MIB as usize]];
    let (vm, backup) = (Uuid::now_v7(), Uuid::now_v7());
    write_backup(&store, vm, backup, &images).await.unwrap();
    let m = store.read_manifest(vm, backup).await.unwrap();
    let cache = Arc::new(ChunkCache::new(64 * MIB, 4));
    let mut s = NbdServer::new();
    for d in &m.disks {
        let ix = store.read_index(&m, d).await.unwrap();
        s.add(
            d.name.clone(),
            Arc::new(StoreExport::new(store.clone(), ix, cache.clone())),
        );
    }
    (Arc::new(s), images)
}

/// 따로 실행기를 둔 서버(멈추면 열린 연결까지 모두 끊긴다 — 헬퍼 프로세스가 죽은 것처럼)
struct Serving {
    rt: Option<tokio::runtime::Runtime>,
}

impl Serving {
    fn start(server: Arc<NbdServer>, sock: &Path) -> Serving {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let sock = sock.to_path_buf();
        rt.spawn(async move {
            let listener = bind(&sock).unwrap();
            server.serve(listener, std::future::pending()).await;
        });
        Serving { rt: Some(rt) }
    }

    fn kill(mut self) {
        self.rt.take().unwrap().shutdown_background();
    }
}

fn nbd_json(sock: &Path, export: &str) -> String {
    format!(
        r#"json:{{"driver":"nbd","server":{{"type":"unix","path":"{}"}},"export":"{export}"}}"#,
        sock.display()
    )
}

/// `qemu-img convert`(보통 복원의 길 — 설계 §8.6)가 우리 서버에서 디스크를 그대로 받고, `qemu-img map`이 0 청크를
/// 0으로 본다. `qemu-io`의 무늬 읽기도 맞는다.
#[tokio::test(flavor = "multi_thread")]
async fn qemu_img_and_qemu_io_read_our_server() {
    let dir = short_tempdir();
    let sock = dir.path().join("s.sock");
    let (server, images) = store_server().await;
    let serving = Serving::start(server, &sock);
    wait_socket(&sock).await;
    for (i, img) in images.iter().enumerate() {
        let out = dir.path().join(format!("disk{i}.raw"));
        let src = nbd_json(&sock, &format!("disk{i}"));
        run(
            "qemu-img",
            &["convert", "-O", "raw", &src, out.to_str().unwrap()],
        );
        assert!(std::fs::read(&out).unwrap() == *img, "disk{i} differs");
    }
    let map = run(
        "qemu-img",
        &["map", "--output=json", &nbd_json(&sock, "disk0")],
    );
    let map: serde_json::Value = serde_json::from_str(&map).unwrap();
    let cb = CHUNK_BYTES;
    let zero: Vec<(u64, u64)> = map
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["zero"] == true)
        .map(|e| (e["start"].as_u64().unwrap(), e["length"].as_u64().unwrap()))
        .collect();
    assert_eq!(zero, vec![(cb, cb)], "{map}");
    let opts = format!(
        "driver=nbd,server.type=unix,server.path={},export=disk0",
        sock.display()
    );
    let out = run(
        "qemu-io",
        &[
            "-r",
            "--image-opts",
            &opts,
            "-c",
            "read -P 0x5a 0 64k",
            "-c",
            &format!("read -P 0 {cb} 64k"),
            "-c",
            &format!("read -P 0xa5 {} 1M", 3 * cb - MIB),
            "-c",
            &format!("read -P 0x33 {} 64k", 3 * cb + 4096),
        ],
    );
    assert!(!out.contains("fail"), "{out}");
    // 쓰기는 거절된다(읽기 전용 내보내기).
    let w = Command::new("qemu-io")
        .args(["--image-opts", &opts, "-c", "write -P 1 0 4k"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&w.stdout),
        String::from_utf8_lossy(&w.stderr)
    );
    assert!(
        !w.status.success() || text.contains("fail") || text.contains("read-only"),
        "{text}"
    );
    serving.kill();
}

/// QEMU NBD 블록 드라이버의 `reconnect-delay`(설계 §4·§11.4 탐침): 읽기 사이에 서버(헬퍼)가 죽고 같은 소켓으로 다시
/// 뜨면, 그동안의 읽기는 붙잡혔다가 오류 없이 이어진다. 기한을 넘기면 읽기 오류다.
#[tokio::test(flavor = "multi_thread")]
async fn qemu_nbd_driver_reconnects_to_a_restarted_server() {
    let dir = short_tempdir();
    let sock = dir.path().join("r.sock");
    let (server, _) = store_server().await;
    for (delay, down_ms, survives) in [(15u32, 2500u64, true), (1, 4000, false)] {
        let serving = Serving::start(server.clone(), &sock);
        wait_socket(&sock).await;
        let opts = format!(
            "driver=nbd,server.type=unix,server.path={},export=disk0,reconnect-delay={delay}",
            sock.display()
        );
        // 첫 읽기 → 1.5초 쉼(그 사이 서버가 죽음) → 둘째 읽기(서버가 없는 동안 보냄)
        let io = Command::new("qemu-io")
            .args([
                "-r",
                "--image-opts",
                &opts,
                "-c",
                "read -P 0x5a 0 64k",
                "-c",
                "sleep 1500",
                "-c",
                &format!("read -P 0xa5 {} 64k", 2 * CHUNK_BYTES),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        serving.kill();
        let _ = std::fs::remove_file(&sock);
        tokio::time::sleep(Duration::from_millis(down_ms)).await;
        let serving = Serving::start(server.clone(), &sock);
        let started = std::time::Instant::now();
        let out = tokio::task::spawn_blocking(move || io.wait_with_output().unwrap())
            .await
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        eprintln!(
            "PROBE reconnect-delay={delay}s down={down_ms}ms: finished {:?} after restart; {}",
            started.elapsed(),
            text.replace('\n', " | ")
        );
        let reads_ok = text.matches("read 65536/65536").count();
        if survives {
            assert_eq!(reads_ok, 2, "{text}");
            assert!(!text.contains("fail"), "{text}");
        } else {
            assert_eq!(reads_ok, 1, "{text}");
            assert!(text.contains("fail"), "{text}");
        }
        serving.kill();
    }
}
