//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, 스냅샷을 만들고 되돌리고, 그 스냅샷에서 복제한 VM을 시작했다가 지우고, 스냅샷을
//! 지우고, 볼륨을 늘리고, 예약 스냅샷이 생기고 보관 개수대로 지워지는지 본 뒤, VM(스냅샷과 함께)·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
//! QEMU(`qemu-it`와 같다), pnpm, `pnpm -C web exec playwright install chromium`이 필요하다.
#![cfg(feature = "web-it")]

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use common::Daemon;
use oplctl::Client;
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

#[tokio::test]
async fn browser_uploads_image_runs_vm_and_cleans_up() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    assert!(
        web.join("dist/index.html").exists(),
        "build the web UI first: pnpm -C web build"
    );
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
    let image = d.path("image.raw");
    std::fs::File::create(&image)
        .unwrap()
        .set_len(IMAGE_BYTES)
        .unwrap();

    let out = tokio::task::spawn_blocking({
        let (url, password) = (d.server(), d.admin_password.clone().unwrap());
        let (pool, image) = (pool.clone(), image.clone());
        move || {
            Command::new("pnpm")
                .args(["exec", "playwright", "test"])
                .current_dir(&web)
                .env("OPL_URL", url)
                .env("OPL_USER", "admin")
                .env("OPL_PASSWORD", password)
                .env("OPL_POOL_PATH", pool)
                .env("OPL_IMAGE", image)
                .output()
                .expect("run pnpm (is it installed?)")
        }
    })
    .await
    .unwrap();
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
    d.stop();
}
