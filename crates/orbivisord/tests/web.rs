//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, 스냅샷을 만들고 되돌리고, 그 스냅샷에서 복제한 VM을 시작했다가 지우고, 스냅샷을
//! 지우고, 볼륨을 늘리고, 예약 스냅샷이 생기고 보관 개수대로 지워지는지 본 뒤, VM(스냅샷과 함께)·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! 계획 62: 전역 역할이 없는 폴더 사용자로 로그인하면 자기 폴더만 보이고 2단계 인증으로 다시 로그인한다
//! (`permissions.spec.ts`, QEMU 없음).
//! 계획 69: 고정물 ext4 디스크 VM의 저장소 백업을 브라우저로 찾아보고 파일 하나·폴더(zip)를 내려받은 뒤 닫는다
//! (`file-restore.spec.ts`, 실제 도우미 이미지 — `OPL_RESTORE_IMAGE_DIR`·`OPL_RESTORE_FIXTURES`가 없으면 건너뜀).
//! 계획 70: VM을 켠 뒤 인프라 › 노드의 보기 "지도"에서 노드 블록·VM 타일을 보고 키보드·누르기로 서랍을 열고 목록 보기·휴대폰 폭을 본다.
//! 계획 71: 지도에서 `m`(한 노드라 옮길 곳 없음 알림과 Esc), 보기 "빌더"로 마우스·키보드·휴대폰 폭에서 VM을 만든다
//! (`builder.spec.ts`, QEMU를 켜지 않음).
//! 계획 72: 시험 안에 띄운 가짜 vSphere로 원본 더하기(지문) → 마법사 → 따뜻한 동기화 → 전환 → VM 링크, 그 VM을 OVA로
//! 내보내 내려받기(`vmware.spec.ts`, QEMU를 켜지 않음).
//! 계획 73: 웹에서 VM 메모리를 바꾸고 ⋯ "설정 기록…" 서랍의 전후 표를 본 뒤 만든 줄로 되돌리고, 인증서 보기를 연다
//! (`history.spec.ts`, QEMU를 켜지 않음).
//! `pnpm -C web build && cargo test -p orbivisord --features web-it --test web`
//! QEMU(`qemu-it`와 같다), pnpm, `pnpm -C web exec playwright install chromium`이 필요하다.
#![cfg(feature = "web-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use common::Daemon;
use orbivisorctl::Client;
use serde_json::Value;

/// 브라우저가 올릴 디스크 이미지(raw, 0으로 채운 sparse 파일)의 크기
const IMAGE_BYTES: u64 = 16 << 20;

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다(스냅샷 전체 목록은 oplctl에 없다).
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

/// 브라우저 시험은 하나씩 돈다(Playwright의 결과 디렉터리와 브라우저·QEMU 부하를 나누지 않게).
static BROWSER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn web_dir() -> PathBuf {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    assert!(
        web.join("dist/index.html").exists(),
        "build the web UI first: pnpm -C web build"
    );
    web
}

/// `spec` 하나만 Playwright로 돌린다(다른 spec은 다른 준비가 필요하다).
async fn playwright(spec: &str, env: Vec<(&'static str, String)>) -> Output {
    let web = web_dir();
    let spec = spec.to_string();
    tokio::task::spawn_blocking(move || {
        Command::new("pnpm")
            .args(["exec", "playwright", "test", &spec])
            .current_dir(&web)
            .envs(env)
            .output()
            .expect("run pnpm (is it installed?)")
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn browser_uploads_image_runs_vm_and_cleans_up() {
    let _one = BROWSER.lock().await;
    web_dir();
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다. 예약 스냅샷은 한 시간을 2초로.
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[schedule]\ntick_secs = 1\nhour_secs = 2\n",
    );
    d.start().await;
    d.init("web-e2e").await;
    d.login().await;
    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    // 백업 저장소 줄기(계획 68)의 백업 풀. 저장소 리소스를 지워도 저장소 데이터(`store/<id>`)는 남는다(설계 원칙).
    let backup_pool = d.path("bkpool");
    std::fs::create_dir(&backup_pool).unwrap();
    let image = d.path("image.raw");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(IMAGE_BYTES)
        .unwrap();

    let out = playwright(
        "e2e/vm.spec.ts",
        vec![
            ("OPL_URL", d.server()),
            ("OPL_USER", "admin".into()),
            ("OPL_PASSWORD", d.admin_password.clone().unwrap()),
            ("OPL_POOL_PATH", pool.to_str().unwrap().into()),
            ("OPL_BACKUP_POOL_PATH", backup_pool.to_str().unwrap().into()),
            ("OPL_IMAGE", image.to_str().unwrap().into()),
        ],
    )
    .await;
    if !out.status.success() {
        // 브라우저가 지우지 못한 VM을 끄고 지운다(QEMU가 남지 않게). 볼륨 삭제는 Task를 오래 기다릴 수
        // 있어 잠깐만 기다리고, 풀과 남은 파일은 데몬의 임시 디렉터리와 함께 사라진다. 실패는 무시한다.
        // 예약이 새 스냅샷을 만들지 않게 먼저 끈다(CLI가 vm_busy면 30초까지 다시 시도한다).
        let _ = tokio::time::timeout(
            Duration::from_secs(40),
            d.ctl(&["vm", "snapshot", "schedule", "web", "--off"]),
        )
        .await;
        let _ = tokio::time::timeout(
            Duration::from_secs(30),
            d.ctl(&["vm", "stop", "copy", "--force"]),
        )
        .await;
        let _ = tokio::time::timeout(
            Duration::from_secs(60),
            d.ctl(&["vm", "delete", "copy", "--with-snapshots"]),
        )
        .await;
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(60),
            d.ctl(&["vm", "delete", "web", "--with-snapshots"]),
        )
        .await;
        for volume in ["root", "copy-root"] {
            let _ = tokio::time::timeout(
                Duration::from_secs(30),
                d.ctl(&["volume", "delete", volume]),
            )
            .await;
        }
    }
    assert!(
        out.status.success(),
        "playwright failed:\n{}\n{}\nopld log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        d.log()
    );
    // 브라우저가 만든 것은 모두 지웠다.
    for kind in ["vm", "volume", "pool"] {
        let list = d.ctl_json(&[kind, "list"]).await;
        assert_eq!(list.as_array().map(Vec::len), Some(0), "{kind}: {list}");
    }
    let snapshots: Value = client(&d).get("/snapshots").await.unwrap();
    assert_eq!(
        snapshots["items"].as_array().map(Vec::len),
        Some(0),
        "snapshots: {snapshots}"
    );
    // 볼륨 파일도 지웠다. dir 풀의 디렉터리에는 볼륨 파일과 만드는 동안의 임시 파일만 생긴다.
    let left: Vec<_> = std::fs::read_dir(&pool)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        left.is_empty(),
        "pool directory {}: {left:?}",
        pool.display()
    );
    let stores = d.ctl_json(&["backup-store", "list"]).await;
    assert_eq!(stores.as_array().map(Vec::len), Some(0), "stores: {stores}");
    assert!(
        backup_pool.join("store").is_dir(),
        "the deleted store's data stays in the backup pool"
    );
    d.stop();
}

/// 계획 62: admin이 `/teamA`·`/teamB`와 꺼진 VM(디스크 없음)을 만들고, 그룹 `team-a`(멤버 alice, 전역 역할 `none`)에
/// `/teamA` operator를 준다. 브라우저로 alice가 로그인하면 VM 목록·폴더 트리·팔레트에 teamA만 있고, + 만들기가
/// `/teamA`를 골라 둔 채 VM을 만들며(노드 목록 없이), 인증 앱을 켠 뒤에는 둘째 단계로 로그인한다.
#[tokio::test]
async fn browser_shows_a_folder_user_only_their_folder() {
    let _one = BROWSER.lock().await;
    web_dir();
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("web-perm").await;
    d.login().await;
    for f in ["/teamA", "/teamB"] {
        d.ctl(&["folder", "create", f]).await.unwrap();
    }
    for (vm, folder) in [("a1", Some("/teamA")), ("b1", Some("/teamB")), ("r1", None)] {
        let mut args = vec!["vm", "create", vm, "--memory", "256"];
        if let Some(f) = folder {
            args.extend(["--folder", f]);
        }
        d.ctl(&args).await.unwrap();
    }
    let password = "alice-password-1";
    let file = d.path("alice-password");
    std::fs::write(&file, password).unwrap();
    d.ctl(&[
        "user",
        "create",
        "alice",
        "--role",
        "none",
        "--password-file",
        file.to_str().unwrap(),
    ])
    .await
    .unwrap();
    d.ctl(&["group", "create", "team-a", "--member", "alice"])
        .await
        .unwrap();
    d.ctl(&[
        "grant", "add", "--group", "team-a", "--role", "operator", "--on", "/teamA",
    ])
    .await
    .unwrap();

    let out = playwright(
        "e2e/permissions.spec.ts",
        vec![
            ("OPL_URL", d.server()),
            ("OPL_USER", "alice".into()),
            ("OPL_PASSWORD", password.into()),
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "playwright failed:\n{}\n{}\nopld log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        d.log()
    );
    // 브라우저가 만든 VM은 /teamA에 있고(머신은 서버가 골랐다), alice에게 2단계 인증이 켜졌다.
    let mut names: Vec<String> = d
        .ctl_json(&["vm", "list", "--folder", "/teamA"])
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["a1", "a2"]);
    let a2 = d.ctl_json(&["vm", "get", "a2"]).await;
    let machine = if std::env::consts::ARCH == "aarch64" {
        "virt"
    } else {
        "q35"
    };
    assert_eq!(a2["spec"]["machine"], machine, "{a2}");
    let users: Value = client(&d).get("/users").await.unwrap();
    let alice = users["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["name"] == "alice")
        .unwrap();
    assert_eq!(alice["two_factor"], true, "{alice}");
    d.stop();
}

/// 계획 71: 보기 "빌더"로 꺼진 VM 셋(b1 마우스·b2 키보드·b3 휴대폰 폭)을 만든다. 볼륨 `b1-disk0`(이름 겹침 — 브라우저
/// 오류)과 vCPU 할당량 1인 폴더 `/q`(서버 미리 확인의 할당량 오류)를 미리 만든다. b2·b3은 보통 크기(vCPU 2·메모리
/// 4 GiB)·새 디스크 하나·NAT NIC 하나, b1은 `/q`에 작게(vCPU 1·1 GiB)와 살피기에서 바꾼 이름·크기의 디스크다.
#[tokio::test]
async fn browser_builds_vms_with_the_builder() {
    let _one = BROWSER.lock().await;
    web_dir();
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("web-builder").await;
    d.login().await;
    let dir = d.path("local");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&[
        "volume", "create", "b1-disk0", "--pool", "local", "--size", "1M",
    ])
    .await
    .unwrap();
    d.ctl(&["folder", "create", "/q"]).await.unwrap();
    d.ctl(&["folder", "quota", "set", "/q", "--vcpus", "1"])
        .await
        .unwrap();

    let out = playwright(
        "e2e/builder.spec.ts",
        vec![
            ("OPL_URL", d.server()),
            ("OPL_USER", "admin".into()),
            ("OPL_PASSWORD", d.admin_password.clone().unwrap()),
            ("OPL_POOL", "local".into()),
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "playwright failed:\n{}\n{}\nopld log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        d.log()
    );
    for (vm, vcpus, memory, volume, size) in [
        ("b1", 1, 1024, "b1-root", 1u64 << 30),
        ("b2", 2, 4096, "b2-disk0", 20 << 30),
        ("b3", 2, 4096, "b3-disk0", 20 << 30),
    ] {
        let v = d.ctl_json(&["vm", "get", vm]).await;
        let spec = &v["spec"];
        assert_eq!(spec["cpu"]["cores"], vcpus, "{v}");
        assert_eq!(spec["memory_mib"], memory, "{v}");
        assert_eq!(spec["power"], "stopped", "{v}");
        assert_eq!(spec["disks"].as_array().map(Vec::len), Some(1), "{v}");
        assert_eq!(spec["nics"].as_array().map(Vec::len), Some(1), "{v}");
        assert_eq!(spec["nics"][0]["network"], "user", "{v}");
        assert_eq!(spec["disks"][0]["volume"], volume, "{v}");
        let vol = d.ctl_json(&["volume", "get", volume]).await;
        assert_eq!(vol["spec"]["size_bytes"], size, "{vol}");
    }
    // b1은 할당량을 단 폴더 /q에 있다.
    let names: Vec<Value> = d
        .ctl_json(&["vm", "list", "--folder", "/q"])
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].clone())
        .collect();
    assert_eq!(names, ["b1"]);
    // 미리 만든 볼륨은 그대로 남고 어디에도 붙지 않았다.
    let taken = d.ctl_json(&["volume", "get", "b1-disk0"]).await;
    assert!(taken["status"]["attached_to"].is_null(), "{taken}");
    d.stop();
}

/// 계획 69: 도우미 이미지를 켠 노드에서 고정물 ext4 디스크(`make-disks.sh`의 `ext4.img`)를 단 꺼진 VM을 저장소에
/// 백업하고, 브라우저가 백업 표의 "Browse files"로 세션을 열어 디렉터리에 들어가고 `hello.txt`(raw)와 볼륨 루트(zip)를
/// 내려받은 뒤 닫는다. 닫은 세션은 목록에 없다. `OPL_RESTORE_IMAGE_DIR=<이미지> OPL_RESTORE_FIXTURES=<고정물>`이
/// 없으면 건너뛴다(`file_restore.rs`와 같은 입력).
#[tokio::test]
async fn browser_browses_backup_files() {
    let (Some(image_dir), Some(fixtures)) = (
        std::env::var_os("OPL_RESTORE_IMAGE_DIR"),
        std::env::var_os("OPL_RESTORE_FIXTURES"),
    ) else {
        eprintln!("skipping: set OPL_RESTORE_IMAGE_DIR and OPL_RESTORE_FIXTURES");
        return;
    };
    let _one = BROWSER.lock().await;
    web_dir();
    let mut d = Daemon::new(
        "node1",
        &format!(
            "[file_restore]\nimage_dir = \"{}\"\nrun_as = \"\"\n",
            Path::new(&image_dir).display()
        ),
    );
    d.start().await;
    d.init("web-fr").await;
    d.login().await;
    for (name, extra) in [("local", None), ("bk", Some("--backups"))] {
        let dir = d.path(name);
        std::fs::create_dir(&dir).unwrap();
        let mut args = vec!["pool", "create", name, "--path", dir.to_str().unwrap()];
        args.extend(extra);
        d.ctl(&args).await.unwrap();
        d.wait_pool_ready(name).await;
    }
    common::eventually("the node reports file restore", || async {
        let nodes = d.ctl_json(&["node", "list"]).await;
        nodes[0]["status"]["file_restore"]["image_version"].is_string()
    })
    .await;
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M", "--format", "qcow2",
    ])
    .await
    .unwrap();
    let v = d.ctl_json(&["volume", "get", "root"]).await;
    let file = d
        .path("local")
        .join(format!("{}.qcow2", v["id"].as_str().unwrap()));
    let image = Path::new(&fixtures).join("ext4.img");
    let out = Command::new("qemu-img")
        .args(["convert", "-n", "-f", "raw", "-O", "qcow2"])
        .arg(&image)
        .arg(&file)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "qemu-img convert: {out:?}");
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    d.ctl(&["backup-store", "create", "st", "--pool", "bk"])
        .await
        .unwrap();
    d.ctl(&["backup", "create", "web", "--store", "st", "--name", "fr1"])
        .await
        .unwrap();

    let out = playwright(
        "e2e/file-restore.spec.ts",
        vec![
            ("OPL_URL", d.server()),
            ("OPL_USER", "admin".into()),
            ("OPL_PASSWORD", d.admin_password.clone().unwrap()),
            ("OPL_BACKUP", "fr1".into()),
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "playwright failed:\n{}\n{}\nopld log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        d.log()
    );
    // 브라우저가 닫은 세션은 열린 목록에 없고, 도우미 디렉터리도 치워진다.
    common::eventually("the browser's session is closed", || async {
        let open = d.ctl_json(&["backup", "files", "sessions"]).await;
        open.as_array().is_some_and(Vec::is_empty)
            && !d
                .path("run")
                .join("file-restore")
                .read_dir()
                .is_ok_and(|mut e| e.next().is_some())
    })
    .await;
    d.stop();
}

/// 계획 72: 가짜 vSphere(기본 세계 — 켜진 `web01`, 4 MiB 디스크 둘)를 띄우고 브라우저가 원본을 더해 마법사로 따뜻한 이전을
/// 만들고 전환한 뒤 만든 VM을 OVA로 내보내 내려받는다. 끝나면 원본은 꺼지고 NIC가 "켤 때 연결"에서 빠졌으며, 대상 VM은 꺼진
/// 채 있고 볼륨 바이트가 원본과 같고, 내보내기는 준비됐다. 스케줄러 틱 0.5초, 델타 주기 3초.
#[tokio::test(flavor = "multi_thread")]
async fn browser_migrates_a_vmware_vm_and_exports_it() {
    use orbivisor_api::vmware::fake::{FakeConfig, FakeVsphere};
    use orbivisor_types::vmware::VmwarePower;

    let _one = BROWSER.lock().await;
    web_dir();
    let mut d = Daemon::new(
        "node1",
        "[test]\nvmware_tick_ms = 500\nvmware_sync_secs = 3\n",
    );
    d.start().await;
    d.init("web-vmware").await;
    d.login().await;
    let dir = d.path("local");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&["network", "create", "lan", "--bridge", "br0"])
        .await
        .unwrap();
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let spec = fake.source_spec();

    let out = playwright(
        "e2e/vmware.spec.ts",
        vec![
            ("OPL_URL", d.server()),
            ("OPL_USER", "admin".into()),
            ("OPL_PASSWORD", d.admin_password.clone().unwrap()),
            ("OPL_POOL", "local".into()),
            ("OPL_NETWORK", "lan".into()),
            ("OPL_VSPHERE_HOST", spec.host.clone()),
            ("OPL_VSPHERE_PORT", spec.port.to_string()),
            ("OPL_VSPHERE_USER", spec.user.clone()),
            ("OPL_VSPHERE_PASSWORD", spec.password.expose().to_string()),
            ("OPL_VSPHERE_FINGERPRINT", spec.fingerprint.clone()),
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "playwright failed:\n{}\n{}\nopld log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        d.log()
    );
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    assert_eq!(fake.nics_start_connected("web01"), vec![false, false]);
    let vm = d.ctl_json(&["vm", "get", "web01"]).await;
    assert_eq!(vm["spec"]["power"], "stopped", "{vm}");
    for (i, disk) in vm["spec"]["disks"].as_array().unwrap().iter().enumerate() {
        let volume = d
            .ctl_json(&["volume", "get", disk["volume"].as_str().unwrap()])
            .await;
        let format = volume["spec"]["format"].as_str().unwrap();
        let file = dir.join(format!("{}.{format}", volume["id"].as_str().unwrap()));
        let raw = d.path("disk.raw");
        let out = Command::new("qemu-img")
            .args(["convert", "-O", "raw"])
            .arg(&file)
            .arg(&raw)
            .output()
            .expect("run qemu-img");
        assert!(out.status.success(), "qemu-img convert: {out:?}");
        assert!(
            std::fs::read(&raw).unwrap() == fake.disk_bytes("web01", i),
            "disk {i} differs from the source"
        );
    }
    let export = d.ctl_json(&["export", "get", "web01-ova"]).await;
    assert_eq!(export["status"]["phase"], "ready", "{export}");
    d.stop();
}

/// 계획 73: CLI로 꺼진 VM `hist`(메모리 256 MiB, 디스크 없음)를 만들고, 브라우저가 웹 편집으로 메모리를 512로 바꾼 뒤 ⋯ "설정
/// 기록…" 서랍에서 두 줄과 전후 표를 보고 만든 줄로 되돌린다(`history.spec.ts`, QEMU를 켜지 않음). 메모리가 256으로 돌아오고
/// 이력의 맨 위 줄이 되돌린 줄(출처 web, `restored_from`)이다.
#[tokio::test]
async fn browser_restores_a_vm_from_its_config_history() {
    let _one = BROWSER.lock().await;
    web_dir();
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("web-history").await;
    d.login().await;
    d.ctl(&["vm", "create", "hist", "--memory", "256", "--nic", "user"])
        .await
        .unwrap();

    let out = playwright(
        "e2e/history.spec.ts",
        vec![
            ("OPL_URL", d.server()),
            ("OPL_USER", "admin".into()),
            ("OPL_PASSWORD", d.admin_password.clone().unwrap()),
            ("OPL_VM", "hist".into()),
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "playwright failed:\n{}\n{}\nopld log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        d.log()
    );
    let vm = d.ctl_json(&["vm", "get", "hist"]).await;
    assert_eq!(vm["spec"]["memory_mib"], 256, "{vm}");
    let rows = d.ctl_json(&["history", "vm", "hist"]).await;
    let rows = rows["items"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    // 되돌리기는 요청한 쪽의 출처(웹)를 지킨다 — 적용 Task가 썼어도 `apply`가 아니다.
    assert_eq!(rows[0]["source"], "web", "{rows:?}");
    assert_eq!(rows[0]["restored_from"], rows[2]["revision"], "{rows:?}");
    assert_eq!(rows[1]["source"], "web", "{rows:?}");
    d.stop();
}
