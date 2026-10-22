//! M5 완료 기준: CLI로 VM을 만들어 콘솔에 붙고, opld를 kill -9 해도 VM이 계속 돌다가 다시 제어된다.
//! API는 HTTPS, 콘솔은 WSS다.
//! `cargo test -p opld --features qemu-it --test e2e`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::time::Duration;

use common::{Daemon, free_port};
use rustix::process::{Pid, test_kill_process};
use tokio::io::AsyncReadExt;

fn alive(pid: u64) -> bool {
    test_kill_process(Pid::from_raw(pid as i32).unwrap()).is_ok()
}

#[tokio::test]
async fn cli_creates_vm_opens_console_and_survives_kill_9() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("e2e").await;
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
    // 이미지 가져오기: oplctl이 파일을 스트리밍으로 올린다.
    let image = d.path("data.raw");
    std::fs::write(&image, vec![1u8; 4 << 20]).unwrap();
    let image_s = image.to_str().unwrap();
    d.ctl(&[
        "volume",
        "import",
        "data",
        "--pool",
        "local",
        "--file",
        image_s,
        "--source-format",
        "raw",
    ])
    .await
    .unwrap();
    let data = d.ctl_json(&["volume", "get", "data"]).await;
    assert_eq!(data["spec"]["size_bytes"], 4 << 20);
    assert_eq!(data["status"]["phase"], "ready");
    let out = d
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--disk", "root", "--nic", "user", "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    let pid = vm["status"]["pid"].as_u64().unwrap();
    assert!(alive(pid));

    // 콘솔: oplctl이 연 로컬 포트로 VNC 서버의 인사말이 온다. 블록을 나가면 콘솔을 닫는다.
    {
        let port = free_port();
        let port_s = port.to_string();
        let args = ["vm", "console", "web", "--port", &port_s];
        let console = d.ctl(&args);
        tokio::pin!(console);
        let greeting = async {
            let mut tcp = loop {
                match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    Ok(tcp) => break tcp,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            let mut buf = [0u8; 12];
            tcp.read_exact(&mut buf).await.unwrap();
            buf
        };
        tokio::select! {
            r = &mut console => panic!("console exited: {r:?}"),
            buf = tokio::time::timeout(Duration::from_secs(10), greeting) => {
                assert_eq!(&buf.unwrap(), b"RFB 003.008\n");
            }
        }
    }

    // opld가 죽어도 VM은 그대로 돈다.
    d.kill();
    assert!(alive(pid));
    d.start().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    assert_eq!(vm["status"]["pid"].as_u64(), Some(pid));

    // 다시 붙은 VM을 끌 수 있다.
    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    assert!(!alive(pid));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.ctl(&["volume", "delete", "data"]).await.unwrap();
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
    d.stop();
}
