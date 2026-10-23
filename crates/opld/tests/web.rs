//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 VM을 만들고 콘솔을 본 뒤 지운다.
//! 클러스터와 볼륨은 여기서 oplctl로 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
//! QEMU(`qemu-it`와 같다), pnpm, `pnpm -C web exec playwright install chromium`이 필요하다.
#![cfg(feature = "web-it")]

mod common;

use std::path::Path;
use std::process::Command;

use common::Daemon;

#[tokio::test]
async fn browser_creates_vm_opens_console_and_deletes_it() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    assert!(
        web.join("dist/index.html").exists(),
        "build the web UI first: pnpm -C web build"
    );
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("web-e2e").await;
    d.login().await;
    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();

    let out = tokio::task::spawn_blocking({
        let (url, password) = (d.server(), d.admin_password.clone().unwrap());
        move || {
            Command::new("pnpm")
                .args(["exec", "playwright", "test"])
                .current_dir(&web)
                .env("OPL_URL", url)
                .env("OPL_USER", "admin")
                .env("OPL_PASSWORD", password)
                .env("OPL_VOLUME", "root")
                .output()
                .expect("run pnpm (is it installed?)")
        }
    })
    .await
    .unwrap();
    if !out.status.success() {
        // 브라우저가 VM을 지우지 못했으면 QEMU가 남지 않게 끈다(실패는 무시한다).
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
        let _ = d.ctl(&["vm", "delete", "web"]).await;
    }
    assert!(
        out.status.success(),
        "playwright failed:\n{}\n{}\nopld log:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        d.log()
    );
    // 브라우저가 지운 VM은 없고, 볼륨은 남는다.
    let vms = d.ctl_json(&["vm", "list"]).await;
    assert_eq!(vms.as_array().map(Vec::len), Some(0), "{vms}");
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}
