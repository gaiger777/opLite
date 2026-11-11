//! 굶김 시험(설계 §3.4): opld 셋(`[ha] fencing = "process"`)으로 만든 클러스터에서 리더의 `api` 실행기의
//! 작업자를 모두 15초 동안 동기 sleep으로 막는다(시험 빌드의 제어 소켓 `POST /v1/test/block-api`). 그동안
//! 리더의 API는 답하지 않지만 `core`의 리더는 그대로이고 임기도 같으며, 세 노드의 HA 리스 나이가 3초를 넘지
//! 않고, 다른 노드로 한 쓰기가 커밋된다. 리더의 systemd 쓰다듬기(`WATCHDOG=1`)는 막힌 동안 멈췄다가 풀리면
//! 돌아온다. 막는 시간은 실행기 확인(10초 간격, 2초 기한)이 적어도 한 번 막힌 동안에 들도록 15초로 둔다.
//! `cargo test -p opld --features starve-it --test starve`
#![cfg(feature = "starve-it")]

mod common;

use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use common::{Daemon, eventually};
use serde_json::Value;

const BLOCK: Duration = Duration::from_secs(15);

/// 리스 갱신 간격 1초(`fence_after_secs`의 10분의 1)
const CONFIG: &str =
    "[ha]\nfencing = \"process\"\nfence_after_secs = 10\n[test]\nblock_api_ms = 15000\n";

/// 제어 소켓에 요청한다.
async fn control(d: &Daemon, post: bool, path: &str) -> reqwest::Response {
    let http = reqwest::Client::builder()
        .unix_socket(d.path("opld.sock"))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("http://opld{path}");
    let req = if post { http.post(url) } else { http.get(url) };
    req.send().await.unwrap()
}

/// 이 노드가 아는 Raft 리더·임기와 리스 나이(`GET /v1/test/raft`)
async fn raft(d: &Daemon) -> Value {
    control(d, false, "/v1/test/raft")
        .await
        .json()
        .await
        .unwrap()
}

/// 받은 데이터그램들(`within` 동안 기다린다)
fn received(socket: &UnixDatagram, within: Duration) -> Vec<String> {
    socket.set_read_timeout(Some(within)).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while let Ok(n) = socket.recv(&mut buf) {
        got.push(String::from_utf8_lossy(&buf[..n]).into_owned());
        socket.set_nonblocking(true).unwrap();
    }
    socket.set_nonblocking(false).unwrap();
    got
}

#[tokio::test]
async fn a_blocked_api_runtime_does_not_starve_raft_or_ha_leases() {
    let mut nodes = vec![
        Daemon::new("node1", CONFIG),
        Daemon::new("node2", CONFIG),
        Daemon::new("node3", CONFIG),
    ];
    let notify = nodes[0].path("notify");
    let socket = UnixDatagram::bind(&notify).unwrap();
    nodes[0].notify_socket = Some(notify);
    for d in &mut nodes {
        d.start().await;
    }
    nodes[0].init("prod").await;
    nodes[0].login().await;
    let url = nodes[0].join_url.clone().unwrap();
    nodes[1].join(&url).await.unwrap();
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    nodes[2].join(url.trim()).await.unwrap();
    let (password, fp) = (
        nodes[0].admin_password.clone().unwrap(),
        nodes[0].ca_fingerprint.clone().unwrap(),
    );
    nodes[1].login_with(&password, &fp).await;

    // node1이 리더이고 세 노드의 리스를 받고 있다.
    eventually("node1 leads and holds three leases", || async {
        let r = raft(&nodes[0]).await;
        r["leader"] == r["node"] && r["lease_age_ms"].as_object().is_some_and(|l| l.len() == 3)
    })
    .await;
    let before = raft(&nodes[0]).await;
    assert!(
        received(&socket, Duration::from_secs(12))
            .iter()
            .any(|s| s == "WATCHDOG=1"),
        "no WATCHDOG=1 before blocking"
    );

    let resp = control(&nodes[0], true, "/v1/test/block-api").await;
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    let blocked = Instant::now();

    // 리더의 API는 답하지 않는다(정말 막혔다).
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let api = http
        .get(format!("{}/api/v1/auth/whoami", nodes[0].server()))
        .send()
        .await;
    assert!(api.is_err(), "the API answered while blocked: {api:?}");

    // 다른 노드의 API로 한 쓰기는 리더(core)를 거쳐 커밋된다.
    let pool = nodes[1].path("pool");
    std::fs::create_dir_all(&pool).unwrap();
    nodes[1]
        .ctl(&["pool", "create", "p", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();

    // 막힌 동안(끝나기 1초 전까지) 리더·임기가 그대로이고 리스 나이가 3초 미만이다.
    // 막기 전에 시작한 실행기 확인(기한 2초)이 보낸 것은 버리고, 그 뒤로는 WATCHDOG=1이 오지 않는다.
    while blocked.elapsed() < BLOCK - Duration::from_secs(1) {
        let r = raft(&nodes[0]).await;
        assert_eq!(
            (&r["leader"], &r["term"]),
            (&before["leader"], &before["term"]),
            "{r}"
        );
        let ages = r["lease_age_ms"].as_object().unwrap();
        assert_eq!(ages.len(), 3, "{r}");
        assert!(ages.values().all(|a| a.as_u64().unwrap() < 3000), "{r}");
        let got = received(&socket, Duration::from_millis(250));
        if blocked.elapsed() > Duration::from_millis(2500) {
            assert!(
                !got.iter().any(|s| s == "WATCHDOG=1"),
                "petted while blocked: {got:?}"
            );
        }
    }

    // 풀리면 API와 쓰다듬기가 돌아온다.
    eventually("the API answers again", || async {
        http.get(format!("{}/api/v1/auth/whoami", nodes[0].server()))
            .send()
            .await
            .is_ok()
    })
    .await;
    assert!(
        received(&socket, Duration::from_secs(12))
            .iter()
            .any(|s| s == "WATCHDOG=1"),
        "no WATCHDOG=1 after unblocking"
    );
    let log = nodes[0].log();
    assert!(
        log.contains("not petting the systemd watchdog") && log.contains("\"api\""),
        "{log}"
    );
    assert_eq!(raft(&nodes[0]).await["term"], before["term"]);
}
