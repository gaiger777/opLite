//! 백업 저장소를 실제 S3 호환 저장소(CI: SeaweedFS)로(계획 68 §14 GitHub `backup-s3` 작업): orbivisorctl로 S3 저장소를 만들고 실제
//! QEMU VM을 백업·복원·라이브 복원한다. GC와 백업 동시, 사용자 CA의 HTTPS, S3 저장소 멈춤(`docker pause`) 도중의 백업과 라이브
//! 복원(그사이 헬퍼 `kill -9`·opld 다시 시작), 두 클러스터 DR(읽기 전용 붙이기·escrow·scan·라이브 복원·넘겨받기).
//! `cargo test -p opld --features s3-it --test backup_s3 -- --nocapture --test-threads=1`
//!
//! 환경 변수(CI가 S3 호환 저장소(SeaweedFS) 컨테이너 둘을 띄워 준다): `OPL_S3_ENDPOINT`(http), `OPL_S3_TLS_ENDPOINT`(https), `OPL_S3_CA`(https
//! 끝점을 서명한 CA PEM 파일), `OPL_S3_ACCESS_KEY`, `OPL_S3_SECRET_KEY`, `OPL_S3_BUCKET`(두 끝점에 있는 버킷),
//! `OPL_S3_CONTAINER`(http 끝점 S3 저장소의 docker 컨테이너 이름 — 멈춤 시험).
#![cfg(feature = "s3-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::Daemon;
use serde_json::Value;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set (see tests/backup_s3.rs)"))
}

/// 접두사 하나(시험마다 다른 곳)
fn prefix(what: &str) -> String {
    format!("{what}-{}", uuid::Uuid::now_v7().simple())
}

fn docker(args: &[&str]) {
    let ok = Command::new("docker")
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "docker {args:?}");
}

fn qemu_io(file: &Path, format: &str, cmd: &str) {
    let ok = Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(file)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "qemu-io {cmd}");
}

fn same(a: &Path, fa: &str, b: &Path, fb: &str) -> bool {
    Command::new("qemu-img")
        .args(["compare", "-q", "-f", fa, "-F", fb])
        .arg(a)
        .arg(b)
        .status()
        .unwrap()
        .success()
}

/// 데몬 하나와 dir 풀 `local`
async fn node(name: &str, cluster: &str) -> Daemon {
    let mut d = Daemon::new(name, "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init(cluster).await;
    d.login().await;
    let dir = d.path("local");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d
}

/// 볼륨 `name`(크기, 형식)을 만들고 그 파일 경로
async fn volume(d: &Daemon, name: &str, size: &str, format: &str) -> PathBuf {
    d.ctl(&[
        "volume", "create", name, "--pool", "local", "--size", size, "--format", format,
    ])
    .await
    .unwrap();
    let v = d.ctl_json(&["volume", "get", name]).await;
    d.path("local")
        .join(format!("{}.{format}", v["id"].as_str().unwrap()))
}

fn volume_file(d: &Daemon, v: &Value, format: &str) -> PathBuf {
    d.path("local")
        .join(format!("{}.{format}", v["id"].as_str().unwrap()))
}

/// `backup-store create`의 S3 인자(http 끝점이면 `--insecure-http`)
fn s3_args(d: &Daemon, endpoint: &str, prefix: &str) -> Vec<String> {
    let secret = d.path("s3-secret");
    std::fs::write(&secret, env("OPL_S3_SECRET_KEY")).unwrap();
    let mut a: Vec<String> = [
        "--s3-endpoint",
        endpoint,
        "--bucket",
        &env("OPL_S3_BUCKET"),
        "--prefix",
        prefix,
        "--path-style",
        "--access-key-id",
        &env("OPL_S3_ACCESS_KEY"),
        "--secret-key-file",
        secret.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if endpoint.starts_with("http://") {
        a.push("--insecure-http".into());
    }
    a
}

async fn ctl(d: &Daemon, args: &[&str], more: &[String]) -> anyhow::Result<String> {
    let mut all: Vec<&str> = args.to_vec();
    all.extend(more.iter().map(String::as_str));
    d.ctl(&all).await
}

/// 조건이 될 때까지(최대 `limit`) VM `vm`을 읽는다.
async fn wait_vm(
    d: &Daemon,
    vm: &str,
    limit: Duration,
    what: &str,
    ok: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(out) = d.ctl(&["-o", "json", "vm", "get", vm]).await {
            let v: Value = serde_json::from_str(&out).unwrap();
            if ok(&v) {
                return v;
            }
            assert!(Instant::now() < deadline, "{what}: {v}");
        } else {
            assert!(Instant::now() < deadline, "{what}: VM {vm} not found");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// HTTP 끝점의 암호화 저장소: 실행 중 VM을 백업 다섯 번 하는 동안 GC를 세 번 돌리고(동시), 모든 백업이 검증을 통과하며,
/// 마지막 백업을 복원하면 원본과 같다. 저장소의 어떤 객체에도 디스크에 쓴 표식이 평문으로 없다. 처리량을 적는다.
#[tokio::test]
async fn s3_store_backups_survive_concurrent_gc() {
    let d = node("s3a", "s3a").await;
    let root = volume(&d, "root", "256M", "raw").await;
    // 표식(평문으로 저장소에 남으면 안 된다)과 데이터
    let marker = b"OPL-PLAINTEXT-MARKER-6c2f";
    {
        use std::io::{Seek, Write};
        let mut f = std::fs::OpenOptions::new().write(true).open(&root).unwrap();
        for i in 0..64u64 {
            f.seek(std::io::SeekFrom::Start(i * (4 << 20) + 123))
                .unwrap();
            f.write_all(marker).unwrap();
            f.write_all(&[i as u8; 4096]).unwrap();
        }
    }
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    let pass = d.path("escrow");
    std::fs::write(&pass, "correct horse battery staple").unwrap();
    let p = prefix("gc");
    let out = ctl(
        &d,
        &[
            "backup-store",
            "create",
            "s3",
            "--encrypt",
            "--escrow-passphrase-file",
            pass.to_str().unwrap(),
        ],
        &s3_args(&d, &env("OPL_S3_ENDPOINT"), &p),
    )
    .await
    .unwrap();
    assert!(out.contains("recovery key: OPK1-"), "{out}");

    let started = Instant::now();
    d.ctl(&["backup", "create", "web", "--store", "s3", "--name", "b1"])
        .await
        .unwrap();
    let took = started.elapsed();
    println!(
        "PROBE first full backup of 256 MiB to S3 (encrypted): {:?} ({:.1} MB/s)",
        took,
        256.0 / took.as_secs_f64()
    );
    let backups = async {
        for i in 2..=5 {
            let name = format!("b{i}");
            d.ctl(&[
                "backup",
                "create",
                "web",
                "--store",
                "s3",
                "--name",
                &name,
                "--incremental",
            ])
            .await
            .unwrap();
        }
    };
    let gcs = async {
        for _ in 0..3 {
            d.ctl(&["backup-store", "gc", "s3", "--wait"])
                .await
                .unwrap();
        }
    };
    tokio::join!(backups, gcs);
    d.ctl(&["backup-store", "gc", "s3", "--wait"])
        .await
        .unwrap();
    for i in 1..=5 {
        let out = d
            .ctl(&["backup", "verify", &format!("b{i}"), "--wait"])
            .await
            .unwrap();
        assert!(out.contains("verified: ok"), "{out}");
    }
    let list = d.ctl(&["backup-store", "list"]).await.unwrap();
    println!("{list}");

    // 평문 표식이 저장소 객체에 없다.
    let target: opl_types::backup_store::S3Target = serde_json::from_value(serde_json::json!({
        "endpoint": env("OPL_S3_ENDPOINT"), "bucket": env("OPL_S3_BUCKET"), "prefix": p,
        "path_style": true, "access_key_id": env("OPL_S3_ACCESS_KEY"),
        "secret_access_key": env("OPL_S3_SECRET_KEY"), "insecure_http": true
    }))
    .unwrap();
    let backend = opl_store::backend::s3::S3Backend::new(&target, 8).unwrap();
    use opl_store::backend::Backend;
    let objects = opl_store::backend::list_all(&backend, "").await.unwrap();
    assert!(objects.len() > 10, "{}", objects.len());
    for o in &objects {
        let body = backend.get(&o.key).await.unwrap();
        assert!(
            !body.windows(marker.len()).any(|w| w == marker),
            "{} holds the plaintext marker",
            o.key
        );
    }

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let started = Instant::now();
    d.ctl(&[
        "backup", "restore", "b5", "--name", "copy", "--pool", "local",
    ])
    .await
    .unwrap();
    println!("PROBE restore of 256 MiB from S3: {:?}", started.elapsed());
    let copy = d.ctl_json(&["volume", "get", "copy-disk0"]).await;
    assert!(same(&volume_file(&d, &copy, "raw"), "raw", &root, "raw"));
}

/// 사용자 CA로 서명한 HTTPS 끝점: CA 없이는 초기화가 실패하고(시스템 루트가 모름), `--ca-file`이면 백업·복원이 된다.
#[tokio::test]
async fn s3_https_with_a_user_ca() {
    let d = node("s3b", "s3b").await;
    let root = volume(&d, "root", "64M", "qcow2").await;
    qemu_io(&root, "qcow2", "write -P 0x3c 1M 8M");
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    let endpoint = env("OPL_S3_TLS_ENDPOINT");
    let e = ctl(
        &d,
        &["backup-store", "create", "nocab"],
        &s3_args(&d, &endpoint, &prefix("noca")),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(e.contains("not initialized"), "{e}");
    let ca = env("OPL_S3_CA");
    ctl(
        &d,
        &["backup-store", "create", "tls", "--ca-file", &ca],
        &s3_args(&d, &endpoint, &prefix("tls")),
    )
    .await
    .unwrap();
    let out = d.ctl(&["backup-store", "check", "tls"]).await.unwrap();
    assert!(out.contains("this node: ok"), "{out}");
    d.ctl(&["backup", "create", "web", "--store", "tls", "--name", "t1"])
        .await
        .unwrap();
    d.ctl(&[
        "backup", "restore", "t1", "--name", "web2", "--pool", "local",
    ])
    .await
    .unwrap();
    let copy = d.ctl_json(&["volume", "get", "web2-disk0"]).await;
    assert!(same(
        &volume_file(&d, &copy, "qcow2"),
        "qcow2",
        &root,
        "qcow2"
    ));
}

/// S3 호환 저장소(CI: SeaweedFS) 컨테이너를 멈춘다(`docker pause`). 놓으면 푼다.
struct Paused(String);

impl Paused {
    fn new() -> Self {
        let c = env("OPL_S3_CONTAINER");
        docker(&["pause", &c]);
        Paused(c)
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        docker(&["unpause", &self.0]);
    }
}

/// S3 저장소가 멈춰도(다시 하기 60초 안) 백업은 이어 가고, 라이브 복원 VM은 켜진 채 남는다. 라이브 복원 도중(S3 저장소 멈춤으로
/// 붙잡아 둔 채) 헬퍼를 `kill -9`하고 opld를 다시 시작해도 Task가 헬퍼를 다시 띄워 끝내고, 대상 디스크는 원본과 같다. 90초
/// 멈춤(다시 하기보다 김)도 풀리면 끝난다(그사이 단계는 적어 둔다).
#[tokio::test]
async fn s3_pauses_and_kills_during_backup_and_live_restore() {
    let mut d = node("s3c", "s3c").await;
    let root = volume(&d, "big", "512M", "qcow2").await;
    for i in 0..8u32 {
        qemu_io(
            &root,
            "qcow2",
            &format!("write -P {} {}M 60M", 0x10 + i, i * 64),
        );
    }
    d.ctl(&[
        "vm", "create", "big", "--memory", "256", "--disk", "big", "--start",
    ])
    .await
    .unwrap();
    ctl(
        &d,
        &["backup-store", "create", "s3"],
        &s3_args(&d, &env("OPL_S3_ENDPOINT"), &prefix("pause")),
    )
    .await
    .unwrap();
    // 백업 도중 20초 멈춤
    let backup = d.ctl(&["backup", "create", "big", "--store", "s3", "--name", "b1"]);
    let pause = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let p = Paused::new();
        tokio::time::sleep(Duration::from_secs(20)).await;
        drop(p);
    };
    let (out, ()) = tokio::join!(backup, pause);
    let out = out.unwrap();
    assert!(out.contains("backup b1 of vm big created"), "{out}");
    d.ctl(&["vm", "stop", "big", "--force"]).await.unwrap();

    // 라이브 복원: 켜진 뒤 멈춤 → 헬퍼 kill -9 → opld 다시 시작 → 풂 → 끝남
    let started = Instant::now();
    d.ctl(&[
        "backup", "restore", "b1", "--name", "big2", "--pool", "local", "--live",
    ])
    .await
    .unwrap();
    let vm = wait_vm(
        &d,
        "big2",
        Duration::from_secs(300),
        "live restore VM running",
        |v| v["status"]["phase"] == "running",
    )
    .await;
    println!(
        "PROBE live-restored VM running after {:?}",
        started.elapsed()
    );
    let id: uuid::Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    {
        let _p = Paused::new();
        tokio::time::sleep(Duration::from_secs(3)).await;
        let pid_file = d
            .data_dir()
            .join("backup-run")
            .join(format!("lr-{}.pid", id.simple()));
        if let Ok(pid) = std::fs::read_to_string(&pid_file) {
            let pid: i32 = pid.trim().parse().unwrap();
            let _ = rustix::process::kill_process(
                rustix::process::Pid::from_raw(pid).unwrap(),
                rustix::process::Signal::KILL,
            );
            println!("PROBE killed the backup-serve helper {pid} while S3 was paused");
        } else {
            println!("PROBE the live restore had finished before the pause (pid file gone)");
        }
        d.stop();
        d.start().await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let v = d.ctl_json(&["vm", "get", "big2"]).await;
        println!(
            "PROBE during a 20 s pause: phase {} live_restore {}",
            v["status"]["phase"], v["status"]["live_restore"]
        );
        assert_eq!(v["status"]["phase"], "running", "{v}");
    }
    wait_vm(
        &d,
        "big2",
        Duration::from_secs(900),
        "live restore finished",
        |v| v["status"].get("live_restore").is_none() && v["status"]["phase"] == "running",
    )
    .await;
    println!(
        "PROBE live restore of 512 MiB with a pause, a helper kill and an opld restart: {:?}",
        started.elapsed()
    );
    d.ctl(&["vm", "stop", "big2", "--force"]).await.unwrap();
    let copy = d.ctl_json(&["volume", "get", "big2-disk0"]).await;
    assert!(same(
        &volume_file(&d, &copy, "qcow2"),
        "qcow2",
        &root,
        "qcow2"
    ));

    // 90초 멈춤(헬퍼의 다시 하기 60초보다 길다)
    d.ctl(&[
        "backup", "restore", "b1", "--name", "big3", "--pool", "local", "--live",
    ])
    .await
    .unwrap();
    wait_vm(
        &d,
        "big3",
        Duration::from_secs(300),
        "second live restore running",
        |v| v["status"]["phase"] == "running",
    )
    .await;
    {
        let _p = Paused::new();
        for _ in 0..9 {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let v = d.ctl_json(&["vm", "get", "big3"]).await;
            println!(
                "PROBE 90 s pause: phase {} live_restore {}",
                v["status"]["phase"], v["status"]["live_restore"]
            );
        }
    }
    wait_vm(
        &d,
        "big3",
        Duration::from_secs(900),
        "live restore finished after a long pause",
        |v| v["status"].get("live_restore").is_none(),
    )
    .await;
    d.ctl(&["vm", "stop", "big3", "--force"]).await.unwrap();
    let copy = d.ctl_json(&["volume", "get", "big3-disk0"]).await;
    assert!(same(
        &volume_file(&d, &copy, "qcow2"),
        "qcow2",
        &root,
        "qcow2"
    ));
    d.stop();
}

/// 두 클러스터 DR(§12.6): prod가 S3에 암호화 저장소(escrow)로 백업하고, dr이 같은 곳을 읽기 전용으로 붙여 escrow 암호
/// 문구로 열고 scan·라이브 복원한다(원본과 같음). prod가 살아 있으면 넘겨받기는 거절되고 `--force`면 된다. 그 뒤 prod의
/// GC는 저장소가 넘어간 것을 보고 멈춘다.
#[tokio::test]
async fn s3_two_cluster_disaster_recovery() {
    let prod = node("prod1", "prod").await;
    let dr = node("dr1", "dr").await;
    let root = volume(&prod, "root", "64M", "qcow2").await;
    qemu_io(&root, "qcow2", "write -P 0x77 2M 20M");
    prod.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    let pass = prod.path("escrow");
    std::fs::write(&pass, "correct horse battery staple").unwrap();
    let p = prefix("dr");
    ctl(
        &prod,
        &[
            "backup-store",
            "create",
            "s3",
            "--encrypt",
            "--escrow-passphrase-file",
            pass.to_str().unwrap(),
        ],
        &s3_args(&prod, &env("OPL_S3_ENDPOINT"), &p),
    )
    .await
    .unwrap();
    prod.ctl(&["backup", "create", "web", "--store", "s3", "--name", "w1"])
        .await
        .unwrap();
    // prod의 GC가 주인 하트비트를 쓴다.
    prod.ctl(&["backup-store", "gc", "s3", "--wait"])
        .await
        .unwrap();

    let dpass = dr.path("escrow");
    std::fs::write(&dpass, "correct horse battery staple").unwrap();
    ctl(
        &dr,
        &[
            "backup-store",
            "create",
            "s3",
            "--read-only",
            "--escrow-unlock-file",
            dpass.to_str().unwrap(),
        ],
        &s3_args(&dr, &env("OPL_S3_ENDPOINT"), &p),
    )
    .await
    .unwrap();
    dr.ctl(&["backup-store", "scan", "s3", "--wait"])
        .await
        .unwrap();
    let list = dr.ctl_json(&["backup", "list"]).await;
    let items = list.as_array().unwrap();
    assert_eq!(items.len(), 1, "{list}");
    let name = items[0]["name"].as_str().unwrap().to_string();
    assert_eq!(items[0]["spec"]["origin"]["cluster"], "prod", "{list}");
    let started = Instant::now();
    dr.ctl(&[
        "backup", "restore", &name, "--name", "web-dr", "--pool", "local", "--live", "--wait",
    ])
    .await
    .unwrap();
    println!("PROBE DR live restore of 64 MiB: {:?}", started.elapsed());
    dr.ctl(&["vm", "stop", "web-dr", "--force"]).await.unwrap();
    let copy = dr.ctl_json(&["volume", "get", "web-dr-disk0"]).await;
    assert!(same(
        &volume_file(&dr, &copy, "qcow2"),
        "qcow2",
        &root,
        "qcow2"
    ));

    // 읽기 전용: 백업은 거절
    let e = dr
        .ctl(&["backup", "create", "web-dr", "--store", "s3"])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("read-only"), "{e}");
    // 넘겨받기: prod의 하트비트가 새로우면 거절, --force면 된다.
    let e = dr
        .ctl(&["backup-store", "take-over", "s3"])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("heartbeat"), "{e}");
    dr.ctl(&["backup-store", "take-over", "s3", "--force"])
        .await
        .unwrap();
    dr.ctl(&["backup-store", "gc", "s3", "--wait"])
        .await
        .unwrap();
    let e = prod
        .ctl(&["backup-store", "gc", "s3", "--wait"])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("taken over"), "{e}");
}
