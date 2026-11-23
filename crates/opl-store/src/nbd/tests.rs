//! NBD 클라이언트 ↔ 서버, 저장소 내보내기, 헬퍼 몸통 시험(QEMU 없이).

use std::sync::Arc;

use async_trait::async_trait;
use opl_types::backup_store::CHUNK_BYTES;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use super::client::{Extent, NbdClient, NbdError};
use super::export::{ChunkCache, MemoryExport, StoreExport, allocation};
use super::serve::{ServeBackend, ServeConfig};
use super::server::{ExportSource, NbdServer, SourceError};
use super::*;
use crate::backend::Backend;
use crate::backend::local::LocalBackend;
use crate::backend::mem::MemBackend;
use crate::crypto::MasterKey;
use crate::format::{STORE_FILE, StoreInfo};
use crate::store::Store;
use crate::testing::write_backup;

const MIB: usize = 1 << 20;

/// 메모리 원본: 바이트와 할당 범위(0으로 채운 곳 = 구멍·0)
struct MemSource {
    data: Vec<u8>,
    fail_at: Option<u64>,
}

#[async_trait]
impl ExportSource for MemSource {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    async fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, SourceError> {
        if self
            .fail_at
            .is_some_and(|f| f >= offset && f < offset + len as u64)
        {
            return Err(SourceError::io("injected failure"));
        }
        Ok(self.data[offset as usize..offset as usize + len as usize].to_vec())
    }

    async fn allocation(&self, offset: u64, len: u32) -> Result<Vec<Extent>, SourceError> {
        // 64 KiB마다 모두 0이면 구멍
        let mut out: Vec<Extent> = Vec::new();
        let mut at = offset;
        let end = offset + len as u64;
        while at < end {
            let stop = ((at / 65536 + 1) * 65536).min(end);
            let zero = self.data[at as usize..stop as usize]
                .iter()
                .all(|b| *b == 0);
            let flags = if zero { STATE_HOLE | STATE_ZERO } else { 0 };
            match out.last_mut() {
                Some(e) if e.flags == flags => e.length += stop - at,
                _ => out.push(Extent {
                    length: stop - at,
                    flags,
                }),
            }
            at = stop;
        }
        Ok(out)
    }
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) | 1)
        .collect()
}

/// 1 MiB: 앞 256 KiB 무늬, 다음 256 KiB 0, 나머지 무늬
fn image() -> Vec<u8> {
    let mut d = pattern(MIB, 3);
    d[256 << 10..512 << 10].fill(0);
    d
}

fn server_with(data: Vec<u8>, fail_at: Option<u64>) -> Arc<NbdServer> {
    let mut s = NbdServer::new();
    s.add("disk0", Arc::new(MemSource { data, fail_at }));
    s.add(
        "disk1",
        Arc::new(MemSource {
            data: vec![9; 4096],
            fail_at: None,
        }),
    );
    Arc::new(s)
}

async fn connect(
    server: &Arc<NbdServer>,
    export: &str,
    contexts: &[&str],
) -> Result<NbdClient<tokio::io::DuplexStream>, NbdError> {
    let (a, b) = tokio::io::duplex(1 << 16);
    tokio::spawn(server.clone().handle(b));
    NbdClient::handshake(a, export, contexts).await
}

#[tokio::test]
async fn client_reads_what_the_server_exports_and_sees_holes() {
    let data = image();
    let server = server_with(data.clone(), None);
    let mut c = connect(&server, "disk0", &[BASE_ALLOCATION]).await.unwrap();
    assert_eq!(c.size(), MIB as u64);
    assert!(c.read_only());
    assert!(c.has_context(BASE_ALLOCATION));
    assert_eq!(c.read(0, MIB as u32).await.unwrap(), data);
    assert_eq!(
        c.read(100_000, 300_000).await.unwrap(),
        data[100_000..400_000]
    );
    assert_eq!(c.read(0, 0).await.unwrap(), Vec::<u8>::new());
    let ext = c.extents(BASE_ALLOCATION, 0, MIB as u64).await.unwrap();
    assert_eq!(
        ext,
        vec![
            Extent {
                length: 256 << 10,
                flags: 0
            },
            Extent {
                length: 256 << 10,
                flags: STATE_HOLE | STATE_ZERO
            },
            Extent {
                length: 512 << 10,
                flags: 0
            },
        ]
    );
    // 범위 밖은 클라이언트가 막는다.
    assert!(c.read(MIB as u64 - 10, 20).await.is_err());
    c.disconnect().await.unwrap();
}

#[tokio::test]
async fn unknown_export_and_missing_context() {
    let server = server_with(image(), None);
    let e = connect(&server, "nope", &[]).await.unwrap_err();
    assert!(
        matches!(&e, NbdError::Export(m) if m.contains("unknown")),
        "{e}"
    );
    // 서버가 모르는 문맥은 빠진다.
    let mut c = connect(&server, "disk1", &[BASE_ALLOCATION, "qemu:dirty-bitmap:x"])
        .await
        .unwrap();
    assert!(c.has_context(BASE_ALLOCATION));
    assert!(!c.has_context("qemu:dirty-bitmap:x"));
    assert!(c.block_status("qemu:dirty-bitmap:x", 0, 10).await.is_err());
    assert_eq!(c.read(0, 4096).await.unwrap(), vec![9; 4096]);
}

#[tokio::test]
async fn source_errors_come_back_as_server_errors_and_the_connection_survives() {
    let server = server_with(image(), Some(700_000));
    let mut c = connect(&server, "disk0", &[]).await.unwrap();
    let e = c.read(650_000, 100_000).await.unwrap_err();
    assert!(
        matches!(&e, NbdError::Server { code, message } if *code == EIO && message.contains("injected")),
        "{e}"
    );
    assert_eq!(c.read(0, 10).await.unwrap(), image()[..10]);
}

/// 날 바이트로: 쓰기는 EPERM, 모르는 명령은 EINVAL, 여러 요청을 한꺼번에 보내도 모두 답한다.
#[tokio::test]
async fn writes_are_refused_and_pipelined_requests_all_get_answers() {
    let server = server_with(image(), None);
    let (a, b) = tokio::io::duplex(1 << 20);
    tokio::spawn(server.clone().handle(b));
    let c = NbdClient::handshake(a, "disk0", &[]).await.unwrap();
    // 협상이 끝난 스트림을 꺼낼 길이 없으므로 같은 순서를 손으로 다시 한다.
    drop(c);
    let (mut a, b) = tokio::io::duplex(1 << 22);
    tokio::spawn(server.clone().handle(b));
    let mut head = [0u8; 18];
    a.read_exact(&mut head).await.unwrap();
    a.write_u32(FLAG_C_FIXED_NEWSTYLE | FLAG_C_NO_ZEROES)
        .await
        .unwrap();
    // 옛 OPT_EXPORT_NAME
    a.write_u64(IHAVEOPT).await.unwrap();
    a.write_u32(OPT_EXPORT_NAME).await.unwrap();
    a.write_u32(5).await.unwrap();
    a.write_all(b"disk0").await.unwrap();
    assert_eq!(a.read_u64().await.unwrap(), MIB as u64);
    let flags = a.read_u16().await.unwrap();
    assert_ne!(flags & TFLAG_READ_ONLY, 0);
    let req = |cmd: u16, cookie: u64, off: u64, len: u32| {
        let mut f = Vec::new();
        f.extend_from_slice(&REQUEST_MAGIC.to_be_bytes());
        f.extend_from_slice(&0u16.to_be_bytes());
        f.extend_from_slice(&cmd.to_be_bytes());
        f.extend_from_slice(&cookie.to_be_bytes());
        f.extend_from_slice(&off.to_be_bytes());
        f.extend_from_slice(&len.to_be_bytes());
        f
    };
    let mut burst = Vec::new();
    burst.extend(req(CMD_WRITE, 1, 0, 3));
    burst.extend_from_slice(b"abc");
    for i in 0..20u64 {
        burst.extend(req(CMD_READ, 100 + i, i * 4096, 4096));
    }
    burst.extend(req(42, 2, 0, 0));
    burst.extend(req(CMD_FLUSH, 3, 0, 0));
    a.write_all(&burst).await.unwrap();
    let mut seen = std::collections::BTreeMap::new();
    for _ in 0..23 {
        assert_eq!(a.read_u32().await.unwrap(), SIMPLE_REPLY_MAGIC);
        let err = a.read_u32().await.unwrap();
        let cookie = a.read_u64().await.unwrap();
        if cookie >= 100 && err == 0 {
            let mut d = vec![0; 4096];
            a.read_exact(&mut d).await.unwrap();
            let off = (cookie - 100) as usize * 4096;
            assert_eq!(d, image()[off..off + 4096]);
        }
        seen.insert(cookie, err);
    }
    assert_eq!(seen[&1], EPERM);
    assert_eq!(seen[&2], EINVAL);
    assert_eq!(seen[&3], 0);
    assert_eq!(seen.len(), 23);
    a.write_all(&req(CMD_DISC, 4, 0, 0)).await.unwrap();
    let mut rest = Vec::new();
    a.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty());
}

#[test]
fn allocation_marks_zero_chunks_as_holes_and_clips() {
    let id = [1u8; 32];
    let ids = vec![id, opl_types::backup_store::ZERO_CHUNK, id];
    let ext = allocation(&ids, 100, 250, 50, 200);
    assert_eq!(
        ext,
        vec![
            Extent {
                length: 50,
                flags: 0
            },
            Extent {
                length: 100,
                flags: STATE_HOLE | STATE_ZERO
            },
            Extent {
                length: 50,
                flags: 0
            },
        ]
    );
    assert_eq!(allocation(&ids, 100, 250, 240, 100).len(), 1);
}

/// 디스크 둘: 0번은 4 MiB 청크 셋(가운데 0, 마지막은 짧음), 1번은 1 MiB
fn disks() -> Vec<Vec<u8>> {
    let cb = CHUNK_BYTES as usize;
    let mut d0 = pattern(2 * cb + MIB / 2, 5);
    d0[cb..2 * cb].fill(0);
    vec![d0, pattern(MIB, 77)]
}

async fn mem_store(key: Option<&MasterKey>) -> Arc<Store> {
    let b: Arc<dyn Backend> = Arc::new(MemBackend::new());
    Arc::new(Store::init(b, Uuid::now_v7(), key, 0).await.unwrap())
}

async fn store_server(store: &Arc<Store>, vm: Uuid, backup: Uuid) -> Arc<NbdServer> {
    let m = store.read_manifest(vm, backup).await.unwrap();
    let cache = Arc::new(ChunkCache::new(64 << 20, 4));
    let mut s = NbdServer::new();
    for d in &m.disks {
        let ix = store.read_index(&m, d).await.unwrap();
        s.add(
            d.name.clone(),
            Arc::new(StoreExport::new(store.clone(), ix, cache.clone())),
        );
    }
    Arc::new(s)
}

#[tokio::test]
async fn store_export_serves_the_backup_with_zero_chunks_as_holes() {
    for key in [None, Some(MasterKey::generate())] {
        let store = mem_store(key.as_ref()).await;
        let (vm, backup) = (Uuid::now_v7(), Uuid::now_v7());
        let images = disks();
        write_backup(&store, vm, backup, &images).await.unwrap();
        let server = store_server(&store, vm, backup).await;
        for (i, img) in images.iter().enumerate() {
            let mut c = connect(&server, &format!("disk{i}"), &[BASE_ALLOCATION])
                .await
                .unwrap();
            assert_eq!(c.size(), img.len() as u64);
            let mut got = Vec::new();
            let step = 3 * MIB as u64 + 12345; // 청크 경계를 가로지르는 읽기
            let mut at = 0;
            while at < c.size() {
                let n = step.min(c.size() - at) as u32;
                got.extend(c.read(at, n).await.unwrap());
                at += n as u64;
            }
            assert!(got == *img, "disk{i} differs");
        }
        let mut c = connect(&server, "disk0", &[BASE_ALLOCATION]).await.unwrap();
        let cb = CHUNK_BYTES;
        let ext = c.extents(BASE_ALLOCATION, 0, c.size()).await.unwrap();
        assert_eq!(
            ext,
            vec![
                Extent {
                    length: cb,
                    flags: 0
                },
                Extent {
                    length: cb,
                    flags: STATE_HOLE | STATE_ZERO
                },
                Extent {
                    length: MIB as u64 / 2,
                    flags: 0
                },
            ]
        );
    }
}

#[tokio::test]
async fn chunk_cache_fetches_each_chunk_once_and_evicts_by_size() {
    let mem = Arc::new(MemBackend::new());
    let b: Arc<dyn Backend> = mem.clone();
    let store = Store::init(b, Uuid::now_v7(), None, 0).await.unwrap();
    let (vm, backup) = (Uuid::now_v7(), Uuid::now_v7());
    let cb = CHUNK_BYTES as usize;
    let image: Vec<u8> = (0..3).flat_map(|n| pattern(cb, 10 + n)).collect();
    write_backup(&store, vm, backup, std::slice::from_ref(&image))
        .await
        .unwrap();
    let m = store.read_manifest(vm, backup).await.unwrap();
    let ix = store.read_index(&m, &m.disks[0]).await.unwrap();
    // 두 청크만 담는 캐시
    let cache = ChunkCache::new(2 * CHUNK_BYTES, 4);
    let got =
        futures_util::future::join_all((0..8).map(|_| cache.get(&store, &ix.ids[0], cb))).await;
    assert!(got.iter().all(|r| r.as_ref().unwrap()[..] == image[..cb]));
    assert_eq!(cache.len(), 1);
    // 담긴 청크는 저장소를 다시 읽지 않는다(망가뜨려도 그대로).
    assert!(mem.tamper(&crate::format::chunk_key(&ix.ids[0]), |d| d[20] ^= 1));
    assert_eq!(
        cache.get(&store, &ix.ids[0], cb).await.unwrap()[..],
        image[..cb]
    );
    for id in &ix.ids[1..3] {
        cache.get(&store, id, cb).await.unwrap();
    }
    assert_eq!(cache.len(), 2);
    // 밀려난 청크는 다시 읽는다 — 망가뜨렸으니 오류이고 캐시에 남지 않는다.
    let e = cache.get(&store, &ix.ids[0], cb).await.unwrap_err();
    assert_eq!(e.code, EIO);
    assert_eq!(cache.len(), 2);
}

/// 헬퍼 몸통: 로컬 백엔드의 암호화 저장소를 설정(JSON 왕복)으로 열어 유닉스 소켓에 내보내고, 소켓은 0600이다.
#[tokio::test]
async fn serve_runs_from_a_json_config_on_a_private_socket() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().join("st");
    let backend: Arc<dyn Backend> = Arc::new(LocalBackend::new(root.clone()));
    let key = MasterKey::generate();
    let id = Uuid::now_v7();
    // prepare(디렉터리 65,536개)를 건너뛰고 store.json만 쓴다 — 로컬 put은 디렉터리를 만든다.
    let fp = crate::crypto::StoreKeys::new(&key)
        .fingerprint()
        .to_string();
    let info = StoreInfo::new(id, Some(&fp), 0);
    backend
        .put(STORE_FILE, serde_json::to_vec(&info).unwrap())
        .await
        .unwrap();
    let store = Store::open(backend, Some(&key)).await.unwrap();
    let (vm, backup) = (Uuid::now_v7(), Uuid::now_v7());
    let images = disks();
    write_backup(&store, vm, backup, &images).await.unwrap();

    let cfg = ServeConfig {
        socket: dir.path().join("run/serve.sock"),
        backend: ServeBackend::Local { root },
        key: Some(key.to_base64()),
        vm,
        backup,
        cache_mib: 16,
        pid_file: Some(dir.path().join("run/serve.pid")),
    };
    assert!(!format!("{cfg:?}").contains(&key.to_base64()));
    let cfg: ServeConfig = serde_json::from_slice(&serde_json::to_vec(&cfg).unwrap()).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let run = {
        let cfg = cfg.clone();
        tokio::spawn(async move {
            super::serve::run(&cfg, async {
                let _ = stopped.await;
            })
            .await
        })
    };
    let mut names = None;
    for _ in 0..100 {
        if let Ok(n) = NbdClient::list(&cfg.socket).await {
            names = Some(n);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(names.unwrap(), vec!["disk0", "disk1"]);
    let mode = std::fs::metadata(&cfg.socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let pid = std::fs::read_to_string(cfg.pid_file.as_ref().unwrap()).unwrap();
    assert_eq!(pid.trim(), std::process::id().to_string());
    let mut c = NbdClient::connect(&cfg.socket, "disk1", &[]).await.unwrap();
    assert_eq!(c.read(0, MIB as u32).await.unwrap(), images[1]);
    stop.send(()).unwrap();
    run.await.unwrap().unwrap();
    assert!(!cfg.socket.exists());
    assert!(!cfg.pid_file.as_ref().unwrap().exists());

    // 틀린 키: 열지 못한다.
    let mut bad = cfg.clone();
    bad.key = Some(MasterKey::generate().to_base64());
    assert!(super::serve::exports(&bad).await.is_err());
    // 없는 백업
    let mut bad = cfg;
    bad.backup = Uuid::now_v7();
    assert!(super::serve::exports(&bad).await.is_err());
}

/// 원본이 주는 문맥(비트맵)을 `base:allocation`과 함께 고르고 한 요청에 둘 다 받는다. 나열은 접두사로 거른다.
#[tokio::test]
async fn extra_contexts_are_negotiated_and_answered_together() {
    let mut data = vec![0u8; MIB];
    data[..65536].fill(1);
    let ctx = dirty_bitmap_context("opl-fl-point");
    let bitmaps = [(ctx.clone(), vec![(MIB as u64 / 2, 4096)])]
        .into_iter()
        .collect();
    let mut s = NbdServer::new();
    s.add("disk0", Arc::new(MemoryExport::new(data.clone(), bitmaps)));
    let server = Arc::new(s);
    let mut c = connect(&server, "disk0", &[&ctx, BASE_ALLOCATION, "qemu:nope"])
        .await
        .unwrap();
    assert!(c.has_context(&ctx) && c.has_context(BASE_ALLOCATION));
    let dirty = c.extents(&ctx, 0, MIB as u64).await.unwrap();
    assert_eq!(
        dirty,
        vec![
            Extent {
                length: MIB as u64 / 2,
                flags: 0
            },
            Extent {
                length: 4096,
                flags: STATE_DIRTY
            },
            Extent {
                length: MIB as u64 / 2 - 4096,
                flags: 0
            },
        ]
    );
    let alloc = c.extents(BASE_ALLOCATION, 0, MIB as u64).await.unwrap();
    assert_eq!(
        alloc[0],
        Extent {
            length: 65536,
            flags: 0
        }
    );
    assert_eq!(c.read(0, 4).await.unwrap(), vec![1; 4]);
    // 문맥 없이 붙은 연결의 BLOCK_STATUS는 거절된다.
    let mut plain = connect(&server, "disk0", &[]).await.unwrap();
    assert!(plain.block_status(BASE_ALLOCATION, 0, 10).await.is_err());
}

/// `BLOCK_STATUS`는 읽기 한도(32 MiB)보다 긴 범위도 받는다(`qemu-img convert`가 64 MiB 디스크에 한 번에 묻는다).
#[tokio::test]
async fn block_status_accepts_ranges_longer_than_the_read_limit() {
    let size = 40 * MIB;
    let mut data = vec![0u8; size];
    data[35 * MIB] = 1;
    let server = server_with(data, None);
    let mut c = connect(&server, "disk0", &[BASE_ALLOCATION]).await.unwrap();
    let got = c
        .block_status(BASE_ALLOCATION, 0, size as u32)
        .await
        .unwrap();
    assert_eq!(got[0].flags, STATE_HOLE | STATE_ZERO);
    assert!(got.iter().map(|e| e.length).sum::<u64>() <= size as u64);
    // 읽기는 그대로 한도를 지킨다
    assert!(
        c.read(0, (MAX_REQUEST as usize + 4096) as u32)
            .await
            .is_err()
    );
}
