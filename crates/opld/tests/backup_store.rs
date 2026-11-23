//! 백업 저장소(계획 68 스테이지 3): `orbivisord backup-serve` 헬퍼(표준 입력 설정, pid 파일, 죽이고 다시 띄우기,
//! 멈추기)와 `backup-it` 빌드의 제어 소켓 고리. QEMU는 쓰지 않는다.
//! `cargo test -p opld --test backup_store` (고리는 `--features backup-it`)

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use opl_driver::{backup_serve_alive, start_backup_serve, stop_backup_serve};
use opl_store::backend::Backend;
use opl_store::backend::local::LocalBackend;
use opl_store::crypto::{MasterKey, StoreKeys};
use opl_store::format::{STORE_FILE, StoreInfo};
use opl_store::nbd::NbdClient;
use opl_store::nbd::serve::{ServeBackend, ServeConfig};
use opl_store::store::Store;
use opl_store::testing::write_backup;
use uuid::Uuid;

const MIB: usize = 1 << 20;

/// 로컬 백엔드의 암호화 저장소에 디스크 둘짜리 백업 하나를 쓴다(prepare의 디렉터리 65,536개는 건너뜀).
async fn local_backup(root: &Path, key: &MasterKey) -> (Uuid, Uuid, Vec<Vec<u8>>) {
    let backend: Arc<dyn Backend> = Arc::new(LocalBackend::new(root));
    let fp = StoreKeys::new(key).fingerprint().to_string();
    let info = StoreInfo::new(Uuid::now_v7(), Some(&fp), 0);
    backend
        .put(STORE_FILE, serde_json::to_vec(&info).unwrap())
        .await
        .unwrap();
    let store = Store::open(backend, Some(key)).await.unwrap();
    let mut d0: Vec<u8> = (0..5 * MIB).map(|i| (i % 251) as u8).collect();
    d0[MIB..2 * MIB].fill(0);
    let images = vec![d0, vec![0x42; MIB]];
    let (vm, backup) = (Uuid::now_v7(), Uuid::now_v7());
    write_backup(&store, vm, backup, &images).await.unwrap();
    (vm, backup, images)
}

async fn read_all(socket: &Path, export: &str) -> Vec<u8> {
    let mut c = NbdClient::connect(socket, export, &[]).await.unwrap();
    let mut out = Vec::new();
    let mut at = 0;
    while at < c.size() {
        let n = (c.size() - at).min(3 << 20) as u32;
        out.extend(c.read(at, n).await.unwrap());
        at += n as u64;
    }
    out
}

/// 헬퍼는 표준 입력의 설정(키 포함 — argv에 없음)으로 저장소 백업의 디스크를 내보내고 pid 파일을 쓴다. `kill -9` 뒤
/// 같은 소켓으로 다시 띄우면 다시 읽히고, 멈추면 소켓과 pid 파일이 사라진다. 틀린 키는 로그의 까닭과 함께 실패한다.
#[tokio::test]
async fn backup_serve_helper_exports_a_store_backup_and_restarts() {
    let dir = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let key = MasterKey::generate();
    let root = dir.path().join("store");
    let (vm, backup, images) = local_backup(&root, &key).await;
    let exe = Path::new(env!("CARGO_BIN_EXE_orbivisord"));
    let cfg = ServeConfig {
        socket: dir.path().join("run/bk.sock"),
        backend: ServeBackend::Local { root },
        key: Some(key.to_base64()),
        vm,
        backup,
        cache_mib: 32,
        pid_file: Some(dir.path().join("run/bk.pid")),
    };
    let pid_file = cfg.pid_file.clone().unwrap();
    let log = dir.path().join("run/bk.log");
    let task = Uuid::now_v7();
    let helper = start_backup_serve(exe, false, task, &cfg, &log)
        .await
        .unwrap();
    assert_eq!(backup_serve_alive(&pid_file), Some(helper.pid));
    let cmdline = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &helper.pid.to_string()])
        .output()
        .unwrap();
    let cmdline = String::from_utf8_lossy(&cmdline.stdout);
    assert!(cmdline.contains("backup-serve"), "{cmdline}");
    assert!(!cmdline.contains(&key.to_base64()));
    assert_eq!(
        NbdClient::list(&cfg.socket).await.unwrap(),
        ["disk0", "disk1"]
    );
    assert!(read_all(&cfg.socket, "disk0").await == images[0]);

    // 헬퍼가 죽었다(설계 §11.4) → 같은 소켓으로 다시 띄운다.
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(helper.pid as i32).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    let mut gone = false;
    for _ in 0..100 {
        if backup_serve_alive(&pid_file).is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone);
    let again = start_backup_serve(exe, false, task, &cfg, &log)
        .await
        .unwrap();
    assert_ne!(again.pid, helper.pid);
    assert!(read_all(&cfg.socket, "disk1").await == images[1]);
    stop_backup_serve(&pid_file, &cfg.socket).await;
    assert!(backup_serve_alive(&pid_file).is_none());
    assert!(!cfg.socket.exists());
    assert!(!pid_file.exists());

    // 틀린 키: 저장소를 열지 못하고 끝난다.
    let mut bad = cfg.clone();
    bad.key = Some(MasterKey::generate().to_base64());
    let e = start_backup_serve(exe, false, task, &bad, &log)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("backup-serve exited"), "{e}");
    assert!(e.contains("key") || e.contains("fingerprint"), "{e}");
}

/// `backup-it` 고리: 저장소·작업 리소스를 이름으로 만들고 바꾸고 지운다. 같은 백업 풀에 저장소 둘은 Raft의 겹침
/// 키(`backup-store-target`)가 막는다.
#[cfg(feature = "backup-it")]
#[tokio::test]
async fn backup_hook_writes_stores_and_jobs() {
    use common::Daemon;
    use serde_json::{Value, json};

    async fn put(d: &Daemon, kind: &str, name: &str, spec: Value) -> (bool, String) {
        let http = reqwest::Client::builder()
            .unix_socket(d.path("opld.sock"))
            .build()
            .unwrap();
        let resp = http
            .post("http://opld/v1/test/put")
            .json(&json!({ "kind": kind, "name": name, "spec": spec }))
            .send()
            .await
            .unwrap();
        (resp.status().is_success(), resp.text().await.unwrap())
    }

    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("c1").await;
    let (ok, id) = put(
        &d,
        "backup-store",
        "st",
        json!({ "backend": { "pool": "bk" } }),
    )
    .await;
    assert!(ok, "{id}");
    let id: Uuid = serde_json::from_str(&id).unwrap();
    let (ok, again) = put(
        &d,
        "backup-store",
        "st",
        json!({ "backend": { "pool": "bk" }, "max_streams": 4 }),
    )
    .await;
    assert!(ok, "{again}");
    assert_eq!(serde_json::from_str::<Uuid>(&again).unwrap(), id);
    let (ok, why) = put(
        &d,
        "backup-store",
        "st2",
        json!({ "backend": { "pool": "bk" } }),
    )
    .await;
    assert!(!ok, "a second store on the same pool: {why}");
    let (ok, why) = put(
        &d,
        "backup-store",
        "st3",
        json!({ "backend": { "pool": "bk" }, "max_streams": 0 }),
    )
    .await;
    assert!(!ok && why.contains("max_streams"), "{why}");
    let job = json!({ "vms": { "all": true }, "store": "st", "schedule": { "at": "03:00" } });
    let (ok, why) = put(&d, "backup-job", "nightly", job).await;
    assert!(ok, "{why}");
    let (ok, gone) = put(&d, "backup-store", "st", Value::Null).await;
    assert!(ok && gone == "null", "{gone}");
    let (ok, why) = put(&d, "nope", "x", Value::Null).await;
    assert!(!ok, "{why}");
    d.stop();
}
