//! systemd 알림: opld가 `NOTIFY_SOCKET`으로 시작이 끝나면 `READY=1`을, 실행기 확인이 되면 `WATCHDOG=1`을
//! 보내는지 본다(설계 §3.3).

mod common;

use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

use common::Daemon;

/// `want`이 올 때까지 받는다.
fn wait_for(socket: &UnixDatagram, want: &str) {
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let until = Instant::now() + common::WAIT;
    let mut buf = [0u8; 256];
    while Instant::now() < until {
        if let Ok(n) = socket.recv(&mut buf)
            && &buf[..n] == want.as_bytes()
        {
            return;
        }
    }
    panic!("{want} did not arrive");
}

#[tokio::test]
async fn notifies_systemd_when_ready_and_while_runtimes_answer() {
    let mut d = Daemon::new("node1", "");
    let path = d.path("notify");
    let socket = UnixDatagram::bind(&path).unwrap();
    d.notify_socket = Some(path);
    // 미초기화 노드도 제어 소켓을 열면 시작이 끝난 것이다.
    d.start().await;
    wait_for(&socket, "READY=1");
    wait_for(&socket, "WATCHDOG=1");
    // 멤버가 되면(API가 듣기 시작하면) 다시 알린다.
    d.init("prod").await;
    wait_for(&socket, "READY=1");
    // 다시 띄운 멤버는 API가 들은 뒤 알린다.
    d.stop();
    d.start().await;
    wait_for(&socket, "READY=1");
}

/// 정족수 없이 다시 켠 노드는 고아 정리(지우기 쓰기마다 10초까지 기다림)가 끝나기 전에 `READY=1`을
/// 보낸다. 고아가 많아도 systemd의 시작 제한(90초)에 걸려 다시 띄워지지 않는다.
#[tokio::test]
async fn ready_comes_before_the_orphan_sweep_without_quorum() {
    let mut a = Daemon::new("node1", "");
    let mut b = Daemon::new("node2", "");
    a.start().await;
    b.start().await;
    a.init("prod").await;
    a.login().await;
    b.join(a.join_url.as_deref().unwrap()).await.unwrap();
    common::eventually("two voters", || async {
        let status = a.ctl_json(&["cluster", "status"]).await;
        status["members"]
            .as_array()
            .is_some_and(|m| m.iter().filter(|m| m["voter"] == true).count() == 2)
    })
    .await;

    // node1에 끝나지 않은 업로드(만드는 중인 볼륨)를 남기고 kill -9 한다.
    let pool = a.path("pool");
    std::fs::create_dir_all(&pool).unwrap();
    a.ctl(&["pool", "create", "pool1", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(a.path("oplctl.json")).unwrap()).unwrap();
    let token = config["token"].as_str().unwrap().to_string();
    let url = format!(
        "{}/api/v1/volumes/import?name=orphan&pool=pool1&source_format=raw",
        a.server()
    );
    let body = futures_util::stream::once(async { Ok::<_, std::io::Error>(vec![0u8; 1024]) })
        .chain(futures_util::stream::pending());
    let upload = tokio::spawn(
        reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .unwrap()
            .post(url)
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_LENGTH, 1 << 20)
            .body(reqwest::Body::wrap_stream(body))
            .send(),
    );
    common::eventually("the uploading volume", || async {
        let volumes = a.ctl_json(&["volume", "list"]).await;
        volumes
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v["name"] == "orphan"))
    })
    .await;
    a.kill();
    upload.abort();
    b.stop();

    let path = a.path("notify");
    let socket = UnixDatagram::bind(&path).unwrap();
    a.notify_socket = Some(path);
    let ready = std::thread::spawn(move || {
        wait_for(&socket, "READY=1");
        Instant::now()
    });
    a.start().await;
    let api = Instant::now();
    let ready = ready.join().unwrap();
    assert!(
        api.saturating_duration_since(ready) >= Duration::from_secs(5),
        "READY=1 came {:?} before the API",
        api.saturating_duration_since(ready)
    );
}
