//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, VM·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
//! QEMU(`qemu-it`와 같다), pnpm, `pnpm -C web exec playwright install chromium`이 필요하다.
#![cfg(feature = "web-it")]

mod common;

use std::path::Path;
use std::process::Command;

use common::Daemon;

/// 브라우저가 올릴 디스크 이미지(raw, 0으로 채운 sparse 파일)의 크기
const IMAGE_BYTES: u64 = 16 << 20;

#[tokio::test]
async fn browser_uploads_image_runs_vm_and_cleans_up() {
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
        // 브라우저가 지우지 못한 것을 지운다(QEMU가 남지 않게). 실패는 무시한다.
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
        let _ = d.ctl(&["vm", "delete", "web"]).await;
        let _ = d.ctl(&["volume", "delete", "root"]).await;
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
    d.stop();
}
