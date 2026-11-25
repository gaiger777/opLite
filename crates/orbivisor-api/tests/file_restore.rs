//! 파일 복원 세션 API(계획 69 스테이지 5, 가짜 하이퍼바이저의 도우미 — 에이전트 몸통 + 메모리 나무): 세션을 열고(다시 쓰기),
//! 볼륨·디렉터리를 보고, 파일 하나·zip·tar.gz를 링크 두 걸음으로 내려받고, 닫는다. 권한(`backup.file_restore`·남의 세션),
//! 기능 게이트, 노드 고르기(이미지 없음·꽉 참), 도우미 실패, 유휴 닫힘, 백업 지우기(`backup_in_use`·`force`)와 보존, 감사.
//! 노드 사이 넘김은 `two_nodes.rs`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::file_restore::{
    HvGuest, backup_pool, node_state, pool_backup, report, reported, sessions,
};
use common::{Client, Env, FakeStorage, WAIT, vm_request};
use orbivisor_api::FileRestoreSessions;
use orbivisor_driver::{DriverError, GuestAnswer, MockGuestFs};
use orbivisor_types::backup::{Backup, BackupSpec};
use orbivisor_types::node::{FILE_RESTORE, NodeSpec};
use orbivisor_types::task::{TaskSpec, TaskState};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fr {
    env: Env,
    sessions: Arc<FileRestoreSessions>,
    backup: Backup,
    _dir: TempDir,
}

/// 이미지가 있는 노드 하나(세션 상한 `max`, 유휴 `idle`)와 옛 풀 백업 `web-1`
async fn start(max_sessions: u32, idle: Duration) -> Fr {
    let dir = TempDir::new().unwrap();
    let state = node_state(dir.path(), max_sessions, idle, Duration::from_secs(3600));
    let reg = sessions(dir.path(), Some(state));
    let guest = Arc::new(HvGuest::default());
    let mut env = {
        let (reg, guest) = (reg.clone(), guest.clone());
        Env::start_with_state(move |s| {
            s.file_restore = reg;
            s.guest = Some(guest);
        })
        .await
    };
    let _ = guest.0.set(env.hv.clone());
    // 가짜 하이퍼바이저의 도우미는 백업 파일을 읽지 않는다 — 백업 파일 확인(qemu-img)도 가짜 저장소로.
    env.stop_runner().await;
    env.start_runner_with(Arc::new(FakeStorage::default()))
        .await;
    report(&env.cluster, env.node, Some(reported(0, max_sessions))).await;
    backup_pool(&env.cluster, env.dir.path(), "bk", vec![]).await;
    env.pool_ready("bk").await;
    let backup = pool_backup(&env.cluster, env.dir.path(), "bk", "web-1", None).await;
    Fr {
        env,
        sessions: reg,
        backup,
        _dir: dir,
    }
}

async fn open(c: &Client, backup: &str) -> (StatusCode, Value) {
    c.post(&format!("/backups/{backup}/file-restore"), Value::Null)
        .await
}

/// 세션이 `phase`가 될 때까지 기다린다.
async fn wait_phase(c: &Client, id: &str, phase: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let (status, s) = c.get(&format!("/file-restore/sessions/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{s}");
        if s["phase"] == phase {
            return s;
        }
        assert!(
            Instant::now() < deadline,
            "session never became {phase}: {s}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 세션을 열고 `ready`까지 기다린다.
async fn ready(c: &Client, backup: &str) -> Value {
    let (status, body) = open(c, backup).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["session"]["phase"], "booting", "{body}");
    let id = body["session"]["id"].as_str().unwrap().to_string();
    wait_phase(c, &id, "ready").await
}

fn names(page: &Value) -> Vec<String> {
    page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

async fn link(c: &Client, id: &str, body: Value) -> String {
    let (status, l) = c
        .post(&format!("/file-restore/sessions/{id}/download-link"), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{l}");
    assert_eq!(l["expires_in"], 60);
    l["url"].as_str().unwrap().to_string()
}

/// 내려받기(본문 바이트와 머리)
async fn fetch(c: &Client, url: &str) -> (StatusCode, reqwest::header::HeaderMap, Vec<u8>) {
    let resp = c
        .http
        .get(format!("{}/api/v1{url}", c.base))
        .bearer_auth(c.token.as_deref().unwrap())
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    (status, headers, resp.bytes().await.unwrap().to_vec())
}

/// zip의 중앙 디렉터리에서 이름들
fn zip_names(zip: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 46 <= zip.len() {
        if zip[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
            let n = u16::from_le_bytes([zip[i + 28], zip[i + 29]]) as usize;
            out.push(String::from_utf8_lossy(&zip[i + 46..i + 46 + n]).into_owned());
            i += 46 + n;
        } else {
            i += 1;
        }
    }
    out
}

async fn task_of(env: &Env, session: &str) -> orbivisor_types::task::Task {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks
        .into_iter()
        .find(|t| t.spec.target.to_string() == session)
        .unwrap()
}

async fn wait_task_done(env: &Env, session: &str) -> orbivisor_types::task::Task {
    let deadline = Instant::now() + WAIT;
    loop {
        let t = task_of(env, session).await;
        if t.status.state.is_finished() {
            return t;
        }
        assert!(Instant::now() < deadline, "session task did not end: {t:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_session_browses_downloads_and_closes() {
    let fr = start(2, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    let s = ready(&admin, "web-1").await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["backup"], fr.backup.id.to_string());
    assert_eq!(s["backup_name"], "web-1");
    assert_eq!(s["node"], env.node.to_string());
    let vols: Vec<&str> = s["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert_eq!(vols, ["disk0p1", "disk0p2", "lvm/vg0/data", "disk1"]);
    assert_eq!(s["volumes"][0]["state"], "unmounted");
    assert!(s["expires_at"].as_u64().unwrap() > s["opened_at"].as_u64().unwrap());
    // 도우미는 백업 디스크 하나(옛 풀 백업의 qcow2)로 떴다.
    let launches = env.hv.restore_launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].disks.len(), 1);
    assert_eq!(launches[0].disks[0].size, 8 << 30);

    // 같은 사용자가 다시 열면 같은 세션(200)
    let (status, again) = open(&admin, &fr.backup.id.to_string()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["session"]["id"], id.as_str());
    assert_eq!(env.hv.restore_launches().len(), 1);

    // 나열(처음이면 붙임)·쪽 나눔·항목
    let base = format!("/file-restore/sessions/{id}/volumes");
    let (status, root) = admin.get(&format!("{base}/disk0p1/entries")).await;
    assert_eq!(status, StatusCode::OK, "{root}");
    assert_eq!(names(&root), ["etc", "home", "var"]);
    let (_, s) = admin.get(&format!("/file-restore/sessions/{id}")).await;
    assert_eq!(s["volumes"][0]["state"], "mounted");
    let (_, page) = admin
        .get(&format!("{base}/disk0p1/entries?path=/etc&limit=2"))
        .await;
    assert_eq!(names(&page), ["hostname", "localtime"]);
    let cursor = page["next"].as_str().unwrap();
    let (_, page) = admin
        .get(&format!("{base}/disk0p1/entries?path=/etc&cursor={cursor}"))
        .await;
    assert_eq!(names(&page), ["nginx"]);
    assert!(page.get("next").is_none(), "{page}");
    let (_, link_entry) = admin
        .get(&format!("{base}/disk0p1/stat?path=/etc/localtime"))
        .await;
    assert_eq!(link_entry["kind"], "symlink");
    assert_eq!(link_entry["link_target"], "/usr/share/zoneinfo/UTC");
    let (status, e) = admin
        .get(&format!("{base}/disk0p1/stat?path=/etc/hostname"))
        .await;
    assert_eq!(status, StatusCode::OK, "{e}");
    assert_eq!(
        (e["kind"].as_str(), e["size"].as_u64()),
        (Some("file"), Some(7))
    );
    // LVM 논리 볼륨(이름에 `/` — 경로 조각으로 부호화)
    let (status, data) = admin.get(&format!("{base}/lvm%2Fvg0%2Fdata/entries")).await;
    assert_eq!(status, StatusCode::OK, "{data}");
    assert_eq!(names(&data), ["srv"]);

    // 오류
    let cases = [
        (
            format!("{base}/disk0p2/entries"),
            StatusCode::CONFLICT,
            "volume_not_mountable",
        ),
        (
            format!("{base}/disk9/entries"),
            StatusCode::NOT_FOUND,
            "volume_not_found",
        ),
        (
            format!("{base}/..%2Fx/entries"),
            StatusCode::NOT_FOUND,
            "volume_not_found",
        ),
        (
            format!("{base}/disk0p1/entries?path=/nope"),
            StatusCode::NOT_FOUND,
            "entry_not_found",
        ),
        (
            format!("{base}/disk0p1/entries?path=/etc/hostname"),
            StatusCode::BAD_REQUEST,
            "invalid_path",
        ),
        (
            format!("{base}/disk0p1/entries?path=/etc/../x"),
            StatusCode::BAD_REQUEST,
            "invalid_path",
        ),
        (
            format!("{base}/disk0p1/entries?cursor=***"),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
    ];
    for (path, status, code) in cases {
        let (got, body) = admin.get(&path).await;
        assert_eq!(
            (got, body["code"].as_str()),
            (status, Some(code)),
            "{path}: {body}"
        );
    }

    // 파일 하나(auto → raw): Content-Length·이름, 링크는 한 번만
    let url = link(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc/hostname"] }),
    )
    .await;
    let (status, headers, body) = fetch(&admin, &url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"web-01\n");
    assert_eq!(headers["content-length"], "7");
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("filename=\"hostname\""),
        "{headers:?}"
    );
    let (status, _, body) = fetch(&admin, &url).await;
    assert_eq!(
        status,
        StatusCode::GONE,
        "{}",
        String::from_utf8_lossy(&body)
    );

    // 큰 파일 그대로(3 MiB — 여러 데이터 틀)
    let url = link(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/var/log/app.log"], "format": "raw" }),
    )
    .await;
    let (_, _, body) = fetch(&admin, &url).await;
    assert_eq!(body.len(), 3 << 20);
    assert!(body.chunks(16).all(|c| c == b"0123456789abcdef"));

    // 디렉터리 → zip(링크는 넣지 않고 보고서)
    let url = link(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc"] }),
    )
    .await;
    let (status, headers, body) = fetch(&admin, &url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/zip");
    assert!(headers.get("content-length").is_none());
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("web-1-etc.zip"),
        "{headers:?}"
    );
    let mut got = zip_names(&body);
    got.sort();
    assert_eq!(
        got,
        [
            "ORBIVISOR-RESTORE-REPORT.txt",
            "etc/",
            "etc/hostname",
            "etc/nginx/",
            "etc/nginx/nginx.conf"
        ]
    );

    // 여러 항목 → tar.gz(이름을 줌)
    let url = link(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc", "/home"], "format": "tar.gz", "name": "picked.tar.gz" }),
    )
    .await;
    let (status, headers, body) = fetch(&admin, &url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/gzip");
    assert_eq!(&body[..2], [0x1f, 0x8b]);
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("picked.tar.gz")
    );

    // 링크 요청 검사
    let base_link = format!("/file-restore/sessions/{id}/download-link");
    for (body, code) in [
        (json!({ "volume": "disk0p1", "paths": [] }), "invalid_path"),
        (
            json!({ "volume": "disk0p1", "paths": ["/a", "/b"], "format": "raw" }),
            "invalid_path",
        ),
        (
            json!({ "volume": "disk0p1", "paths": ["/etc"], "name": "../x" }),
            "invalid_request",
        ),
        (
            json!({ "volume": "disk1", "paths": ["/"] }),
            "volume_not_mountable",
        ),
    ] {
        let (status, got) = admin.post(&base_link, body.clone()).await;
        assert_eq!(got["code"], code, "{body}: {status} {got}");
    }
    let url = link(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc"], "format": "raw" }),
    )
    .await;
    let (status, _, body) = fetch(&admin, &url).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "{}",
        String::from_utf8_lossy(&body)
    );

    // 다른 사용자는 남의 링크를 쓰지 못한다(403 — 링크는 남음).
    let other = env.user("other-admin", "admin").await;
    let url = link(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc/hostname"] }),
    )
    .await;
    let (status, _, _) = fetch(&other, &url).await;
    // admin은 남의 세션을 보지만 링크는 만든 사용자의 것이다.
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = fetch(&admin, &url).await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"web-01\n".as_slice())
    );

    // 감사: 열기·링크·내려받기(바이트·항목 수)
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, audit) = admin.get("/audit?limit=500").await;
        let text = audit.to_string();
        if text.contains("backup file-restore download web-1 disk0p1:/etc (zip, 4 entries")
            && text.contains("backup file-restore open web-1")
            && text.contains("backup file-restore download-link web-1 disk0p1:/etc/hostname (auto)")
        {
            break;
        }
        assert!(Instant::now() < deadline, "audit entries missing: {text}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 닫기: Task가 성공으로 끝나고 등록부·세션 디렉터리가 비고 더 쓰지 못한다.
    let (status, _) = admin.delete(&format!("/file-restore/sessions/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let t = wait_task_done(env, &id).await;
    assert_eq!(t.status.state, TaskState::Succeeded, "{t:?}");
    assert_eq!(t.status.error, None);
    let s = wait_phase(&admin, &id, "closed").await;
    assert!(s["volumes"].as_array().unwrap().is_empty());
    assert!(fr.sessions.is_empty());
    assert!(!fr._dir.path().join("run/file-restore").join(&id).exists());
    assert!(!fr._dir.path().join("state/file-restore").join(&id).exists());
    let (status, body) = admin.get(&format!("{base}/disk0p1/entries")).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("session_closed"))
    );
    let (status, list) = admin.get("/file-restore/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]));
    // 닫은 뒤 다시 열면 새 세션이다.
    let s2 = ready(&admin, "web-1").await;
    assert_ne!(s2["id"], id.as_str());
}

#[tokio::test]
async fn sessions_need_the_privilege_and_stay_with_their_owner() {
    let fr = start(4, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    // operator에는 `backup.file_restore`가 없다(계획 62 규칙).
    let operator = env.user("op", "operator").await;
    let (status, body) = open(&operator, "web-1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 파일만 꺼내는 헬프데스크 역할(이제 만들 수 있다)
    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "helpdesk", "privileges": ["system.read", "backup.file_restore"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": "hd", "password": "password-1", "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let (status, grant) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": user["id"] },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "root" },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{grant}");
    let hd = env.login("hd", "password-1").await;
    let s = ready(&hd, "web-1").await;
    let id = s["id"].as_str().unwrap();
    assert_eq!(s["opened_by"], user["id"]);
    let (status, _) = hd
        .get(&format!(
            "/file-restore/sessions/{id}/volumes/disk0p1/entries"
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    // 남의 세션은 보이지 않는다(admin은 목록 `all`에서 보고 닫을 수 있다).
    let (status, body) = operator.get(&format!("/file-restore/sessions/{id}")).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("session_not_found"))
    );
    let (status, _) = operator.get("/file-restore/sessions?all=true").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, mine) = admin.get("/file-restore/sessions").await;
    assert_eq!(mine, json!([]));
    let (_, all) = admin.get("/file-restore/sessions?all=true").await;
    assert_eq!(all.as_array().unwrap().len(), 1, "{all}");
    assert_eq!(all[0]["id"], id);
    let (_, by_backup) = hd.get("/file-restore/sessions?backup=web-1").await;
    assert_eq!(by_backup[0]["id"], id);
    // admin이 다시 열면 admin의 새 세션(다시 쓰기는 같은 사용자만)
    let (status, body) = open(&admin, "web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // 권한을 빼면 다음 요청부터 403(세션은 남아 유휴로 닫힌다).
    let grant = grant["id"].as_str().unwrap().to_string();
    let (status, _) = admin.delete(&format!("/grants/{grant}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = hd
        .get(&format!(
            "/file-restore/sessions/{id}/volumes/disk0p1/entries"
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // 리뷰 1: 목록도 권한을 다시 본다(볼륨·백업 이름이 보이지 않음). 없는 백업은 404가 아니라 빈 목록이다.
    let (status, list) = hd.get("/file-restore/sessions").await;
    assert_eq!((status, list), (StatusCode::OK, json!([])));
    let (_, list) = hd.get("/file-restore/sessions?backup=web-1").await;
    assert_eq!(list, json!([]));
    let (status, list) = hd.get("/file-restore/sessions?backup=no-such-backup").await;
    assert_eq!((status, list), (StatusCode::OK, json!([])));
    let (status, _) = admin.delete(&format!("/file-restore/sessions/{id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 옛 노드가 있으면 모든 경로가 409 `cluster_upgrading`
    let mut status_ = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status_.features.retain(|f| f != FILE_RESTORE);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status_)
        .await
        .unwrap();
    let (status, body) = open(&admin, "web-1").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("cluster_upgrading"))
    );
    let (status, body) = admin.get("/file-restore/sessions").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("cluster_upgrading"))
    );
    let (status, body) = admin.get(&format!("/file-restore/sessions/{id}")).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("cluster_upgrading"))
    );
}

#[tokio::test]
async fn sessions_need_a_node_with_the_image_and_a_free_slot() {
    let fr = start(1, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    // 이미지를 알리지 않는 노드
    report(&env.cluster, env.node, None).await;
    let (status, body) = open(&admin, "web-1").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("file_restore_unavailable"))
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("orbivisor-restore-image"),
        "{body}"
    );
    // 모두 꽉 참
    report(&env.cluster, env.node, Some(reported(1, 1))).await;
    let (status, body) = open(&admin, "web-1").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("file_restore_busy"))
    );
    // 백업을 읽을 노드가 이미지 없는 다른 노드(파일을 쓴 노드)
    report(&env.cluster, env.node, Some(reported(0, 1))).await;
    let elsewhere = pool_backup(
        &env.cluster,
        env.dir.path(),
        "bk",
        "web-2",
        Some(uuid::Uuid::now_v7()),
    )
    .await;
    let (status, body) = open(&admin, &elsewhere.name).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("file_restore_unavailable"))
    );
    // 준비되지 않은 백업
    let mut b = env
        .cluster
        .get::<BackupSpec>(elsewhere.id)
        .unwrap()
        .unwrap();
    b.status.phase = orbivisor_types::backup::BackupPhase::Pending;
    env.cluster
        .update_status::<BackupSpec>(b.id, &b.status)
        .await
        .unwrap();
    let (status, body) = open(&admin, "web-2").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("backup_not_ready"))
    );

    // 노드 상태가 늦어도 노드의 자리 상한을 넘기면 Task가 실패한다.
    let _s = ready(&admin, "web-1").await;
    let other = env.user("second", "admin").await;
    let (status, body) = open(&other, "web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["session"]["id"].as_str().unwrap();
    let t = wait_task_done(env, id).await;
    assert_eq!(t.status.state, TaskState::Failed);
    assert!(
        t.status
            .error
            .unwrap()
            .contains("maximum number of file restore sessions")
    );
    let s = wait_phase(&other, id, "failed").await;
    assert!(s["error"].as_str().unwrap().contains("maximum"), "{s}");
}

#[tokio::test]
async fn a_helper_that_fails_or_idles_ends_the_session() {
    let fr = start(4, Duration::from_millis(400)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    env.hv.fail_next_restore_helper(DriverError::FileRestore(
        "the helper VM exited before it answered: kernel panic".into(),
    ));
    let (status, body) = open(&admin, "web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["session"]["id"].as_str().unwrap().to_string();
    let s = wait_phase(&admin, &id, "failed").await;
    assert!(s["error"].as_str().unwrap().contains("kernel panic"), "{s}");
    assert!(fr.sessions.is_empty());

    // 쓰지 않으면 유휴 시간 뒤 닫힌다(까닭은 Task 경고). 그동안 백업은 지울 수 없다.
    let s = ready(&admin, "web-1").await;
    let id = s["id"].as_str().unwrap().to_string();
    let (status, body) = admin.delete("/backups/web-1").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("backup_in_use")),
        "{body}"
    );
    let t = wait_task_done(env, &id).await;
    assert_eq!(t.status.state, TaskState::Succeeded);
    assert!(
        t.status.error.as_deref().unwrap().contains("without use"),
        "{t:?}"
    );
    let (status, body) = admin.delete("/backups/web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn force_deleting_a_backup_closes_its_sessions() {
    let fr = start(4, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    let s = ready(&admin, "web-1").await;
    let id = s["id"].as_str().unwrap().to_string();

    // 리뷰 1: 거절할 지우기(증분의 부모)는 세션을 닫지 않는다.
    let child = pool_backup(&env.cluster, env.dir.path(), "bk", "web-2", None).await;
    let mut st = child.status.clone();
    st.parent = Some(fr.backup.id);
    env.cluster
        .update_status::<BackupSpec>(child.id, &st)
        .await
        .unwrap();
    let (status, body) = admin.delete("/backups/web-1?force=true").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("backup_in_use")),
        "{body}"
    );
    let (_, s) = admin.get(&format!("/file-restore/sessions/{id}")).await;
    assert_eq!(s["phase"], "ready", "{s}");
    env.cluster
        .delete::<BackupSpec>(child.id, None)
        .await
        .unwrap();

    let (status, body) = admin.delete("/backups/web-1?force=true").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let t = wait_task_done(env, &id).await;
    assert_eq!(t.status.state, TaskState::Succeeded, "{t:?}");
    let task = admin.wait_task(&body).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(
        env.cluster
            .get::<BackupSpec>(fr.backup.id)
            .unwrap()
            .is_none()
    );
}

/// 실행 중이고 게스트 에이전트가 답하는 VM `name`(가짜 게스트 파일시스템 `fs`)
async fn guest_vm(env: &Env, admin: &Client, name: &str, fs: MockGuestFs) -> uuid::Uuid {
    let mut req = vm_request(name);
    req["spec"]["guest_agent"] = json!({});
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let id: uuid::Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    env.hv.set_guest_agent(id, GuestAnswer::Ok);
    env.hv.set_guest_fs(id, fs);
    let t = admin.act(name, "start").await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");
    id
}

/// 되돌려 쓰기를 요청하고 Task가 끝날 때까지 기다린다.
async fn restore_into(c: &Client, id: &str, body: Value) -> Value {
    let (status, task) = c
        .post(&format!("/file-restore/sessions/{id}/restore-to-vm"), body)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"]["kind"], "file-restore-to-guest",
        "{task}"
    );
    wait_any(c, &task).await
}

/// 성공·실패 어느 쪽이든 끝난 Task
async fn wait_any(c: &Client, task: &Value) -> Value {
    let id = task["id"].as_str().unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, t) = c.get(&format!("/tasks/{id}")).await;
        if t["status"]["state"] == "succeeded" || t["status"]["state"] == "failed" {
            return t;
        }
        assert!(Instant::now() < deadline, "task did not end: {t}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn text(fs: &MockGuestFs, path: &str) -> String {
    String::from_utf8(fs.files.get(path).cloned().unwrap_or_default()).unwrap()
}

/// 게스트로 되돌려 쓰기(§12): 디렉터리째(keep — 있는 파일은 `.restored-…`로), 덮어쓰기(임시 파일 → mv), 건너뛰기, 큰 파일은
/// 1 MiB 쓰기마다 따로(에이전트 잠금을 쓰기마다), 심볼릭 링크는 건너뜀, 쓰기 실패는 다 쓰지 못한 파일을 지우고 Task 실패.
#[tokio::test]
async fn files_are_restored_into_a_running_guest() {
    let fr = start(2, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    let mut fs = MockGuestFs::linux(&["/srv", "/srv/etc"]);
    fs.files
        .insert("/srv/etc/hostname".into(), b"old\n".to_vec());
    let vm = guest_vm(env, &admin, "web", fs).await;
    let s = ready(&admin, "web-1").await;
    let id = s["id"].as_str().unwrap().to_string();

    let (status, caps) = admin.get("/vms/web/guest/restore-caps").await;
    assert_eq!(status, StatusCode::OK, "{caps}");
    assert_eq!(
        caps,
        json!({ "os": "linux", "file_write": true, "exec": true })
    );

    // 디렉터리째, keep(기본)
    let t = restore_into(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc"], "vm": "web", "target_dir": "/srv" }),
    )
    .await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");
    let warning = t["status"]["error"].as_str().unwrap();
    assert!(warning.contains("restored 2 file(s)"), "{warning}");
    assert!(warning.contains("kept existing files"), "{warning}");
    assert!(
        warning.contains("etc/localtime (symbolic links are not restored)"),
        "{warning}"
    );
    assert!(
        warning.contains("owners, modes and times were not restored"),
        "{warning}"
    );
    let g = env.hv.guest_fs(vm).unwrap();
    assert_eq!(text(&g, "/srv/etc/hostname"), "old\n");
    let kept: Vec<&String> = g
        .files
        .keys()
        .filter(|k| k.starts_with("/srv/etc/hostname.restored-"))
        .collect();
    assert_eq!(kept.len(), 1, "{:?}", g.files.keys());
    assert_eq!(text(&g, kept[0]), "web-01\n");
    assert_eq!(
        text(&g, "/srv/etc/nginx/nginx.conf"),
        "worker_processes auto;\n"
    );
    assert!(g.dirs.contains("/srv/etc/nginx"));
    assert!(!g.files.contains_key("/srv/etc/localtime"));
    assert_eq!(g.open_handles(), 0);

    // 덮어쓰기: 임시 이름에 다 쓴 뒤 mv -f -T
    let t = restore_into(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc/hostname"], "vm": "web",
            "target_dir": "/srv/etc", "conflict": "overwrite" }),
    )
    .await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");
    let g = env.hv.guest_fs(vm).unwrap();
    assert_eq!(text(&g, "/srv/etc/hostname"), "web-01\n");
    assert!(
        !g.files.keys().any(|k| k.contains(".orbivisor-")),
        "{:?}",
        g.files.keys()
    );
    assert!(
        g.ops
            .iter()
            .any(|o| o.starts_with("exec /bin/mv -f -T -- /srv/etc/hostname.orbivisor-")),
        "{:?}",
        g.ops
    );

    // 건너뛰기
    let t = restore_into(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/etc/hostname"], "vm": "web",
            "target_dir": "/srv/etc", "conflict": "skip" }),
    )
    .await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");
    assert!(
        t["status"]["error"]
            .as_str()
            .unwrap()
            .contains("skipped hostname (exists)")
    );

    // 3 MiB 파일은 1 MiB 쓰기 셋(일마다 따로 — 잠금을 쓰기마다 놓는다)
    let before = env.hv.guest_fs(vm).unwrap().ops.len();
    let t = restore_into(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/var/log/app.log"], "vm": "web", "target_dir": "/srv/log" }),
    )
    .await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");
    let g = env.hv.guest_fs(vm).unwrap();
    assert_eq!(g.files["/srv/log/app.log"].len(), 3 << 20);
    assert_eq!(
        g.ops[before..]
            .iter()
            .filter(|o| o.starts_with("write "))
            .count(),
        3
    );

    // 쓰기가 거절되면 다 쓰지 못한 파일을 지우고 Task는 실패(쓴 것은 결과에)
    let mut broken = env.hv.guest_fs(vm).unwrap();
    broken.fail_writes_after = Some(
        broken
            .ops
            .iter()
            .filter(|o| o.starts_with("write "))
            .count()
            + 1,
    );
    env.hv.set_guest_fs(vm, broken);
    let t = restore_into(
        &admin,
        &id,
        json!({ "volume": "disk0p1", "paths": ["/var/log/app.log"], "vm": "web", "target_dir": "/srv/log2" }),
    )
    .await;
    assert_eq!(t["status"]["state"], "failed", "{t}");
    let err = t["status"]["error"].as_str().unwrap();
    assert!(err.contains("1 item(s) could not be restored"), "{err}");
    assert!(err.contains("No space left on device"), "{err}");
    let g = env.hv.guest_fs(vm).unwrap();
    assert!(
        !g.files.contains_key("/srv/log2/app.log"),
        "{:?}",
        g.files.keys()
    );
    assert_eq!(g.open_handles(), 0);

    // 감사
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, audit) = admin.get("/audit?limit=500").await;
        let text = audit.to_string();
        if text.contains("backup file-restore to-vm web-1 disk0p1:/etc → vm web:/srv (keep)") {
            break;
        }
        assert!(Instant::now() < deadline, "audit entry missing: {text}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 에이전트가 막은 것과 요청 검사: `guest-exec`가 없으면 디렉터리·덮어쓰기는 400, `guest-file-*`가 없으면 409
/// `guest_agent_restricted`, 대상 디렉터리는 절대 경로, 꺼진 VM은 409, 대상 VM의 `vm.config`가 없으면 403.
#[tokio::test]
async fn restoring_into_a_guest_checks_the_agent_and_the_request() {
    let fr = start(2, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    let mut fs = MockGuestFs::linux(&["/srv"]);
    fs.exec = false;
    let vm = guest_vm(env, &admin, "web", fs).await;
    let s = ready(&admin, "web-1").await;
    let id = s["id"].as_str().unwrap().to_string();
    let url = format!("/file-restore/sessions/{id}/restore-to-vm");
    let (_, caps) = admin.get("/vms/web/guest/restore-caps").await;
    assert_eq!(caps["exec"], false, "{caps}");
    assert!(
        caps["reason"].as_str().unwrap().contains("guest-exec"),
        "{caps}"
    );
    let body = |paths: Value, dir: &str, conflict: &str| json!({ "volume": "disk0p1", "paths": paths, "vm": "web", "target_dir": dir, "conflict": conflict });
    for (req, code) in [
        (
            body(json!(["/etc"]), "/srv", "keep"),
            "guest_mkdir_unavailable",
        ),
        (
            body(json!(["/"]), "/srv", "keep"),
            "guest_mkdir_unavailable",
        ),
        (
            body(json!(["/etc/hostname"]), "/srv", "overwrite"),
            "guest_overwrite_unavailable",
        ),
        (
            body(json!(["/etc/hostname"]), "srv", "keep"),
            "invalid_target_dir",
        ),
        (
            body(json!(["/etc/hostname"]), "/srv/../etc", "keep"),
            "invalid_target_dir",
        ),
        (body(json!([]), "/srv", "keep"), "invalid_path"),
    ] {
        let (status, err) = admin.post(&url, req.clone()).await;
        assert_eq!(
            (status, err["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some(code)),
            "{req} {err}"
        );
    }
    // 파일 하나는 exec 없이도 된다(있는 디렉터리 바로 아래).
    let t = restore_into(&admin, &id, body(json!(["/etc/hostname"]), "/srv", "keep")).await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");
    assert_eq!(
        text(&env.hv.guest_fs(vm).unwrap(), "/srv/hostname"),
        "web-01\n"
    );

    // guest-file-*를 막은 에이전트(RHEL·Rocky 기본)
    let mut fs = env.hv.guest_fs(vm).unwrap();
    fs.file_write = false;
    env.hv.set_guest_fs(vm, fs);
    let (status, err) = admin
        .post(&url, body(json!(["/etc/hostname"]), "/srv", "keep"))
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("guest_agent_restricted")),
        "{err}"
    );
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("/etc/sysconfig/qemu-ga"),
        "{err}"
    );

    // 꺼진 VM
    let mut req = vm_request("off");
    req["spec"]["guest_agent"] = json!({});
    let (status, _) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED);
    let mut off = body(json!(["/etc/hostname"]), "/srv", "keep");
    off["vm"] = json!("off");
    let (status, err) = admin.post(&url, off).await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("vm_not_running")),
        "{err}"
    );
    let (status, err) = admin.get("/vms/off/guest/restore-caps").await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("vm_not_running"))
    );

    // 파일만 꺼내는 역할(backup.file_restore)은 VM에 쓰지 못한다(vm.config가 없음).
    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "helpdesk", "privileges": ["system.read", "backup.file_restore"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (_, user) = admin
        .post(
            "/users",
            json!({ "name": "hd", "password": "password-1", "role": "viewer" }),
        )
        .await;
    let (status, _) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": user["id"] },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "root" },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let hd = env.login("hd", "password-1").await;
    let s = ready(&hd, "web-1").await;
    let hd_id = s["id"].as_str().unwrap();
    let (status, err) = hd
        .post(
            &format!("/file-restore/sessions/{hd_id}/restore-to-vm"),
            body(json!(["/etc/hostname"]), "/srv", "keep"),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{err}");
    // viewer도 능력은 본다(system.read).
    let (status, caps) = hd.get("/vms/web/guest/restore-caps").await;
    assert_eq!(status, StatusCode::OK, "{caps}");
    assert_eq!(caps["file_write"], false, "{caps}");
}

/// 리뷰 1: 있는지 알 수 없는 대상(읽기로 열 수 없음 — SELinux, Windows 공유 잠금)은 없는 것으로 보지 않는다: 그 파일은
/// 실패로 적고 덮어쓰지 않는다.
#[tokio::test]
async fn files_whose_existence_is_unknown_are_not_overwritten() {
    let fr = start(2, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    let mut fs = MockGuestFs::linux(&["/srv", "/srv/etc"]);
    fs.files
        .insert("/srv/etc/hostname".into(), b"old\n".to_vec());
    fs.unreadable.insert("/srv/etc/hostname".into());
    let vm = guest_vm(env, &admin, "web", fs).await;
    let s = ready(&admin, "web-1").await;
    let id = s["id"].as_str().unwrap().to_string();
    for conflict in ["overwrite", "keep", "skip"] {
        let t = restore_into(
            &admin,
            &id,
            json!({ "volume": "disk0p1", "paths": ["/etc/hostname"], "vm": "web",
                    "target_dir": "/srv/etc", "conflict": conflict }),
        )
        .await;
        assert_eq!(t["status"]["state"], "failed", "{conflict}: {t}");
        let err = t["status"]["error"].as_str().unwrap();
        assert!(
            err.contains("cannot tell whether it exists"),
            "{conflict}: {err}"
        );
        let g = env.hv.guest_fs(vm).unwrap();
        assert_eq!(text(&g, "/srv/etc/hostname"), "old\n");
        assert!(!g.ops.iter().any(|o| o.starts_with("open ")), "{:?}", g.ops);
    }
}

/// 리뷰 1: 폴더에만 권한이 있는 사용자(계획 62)도 자기 세션 Task와 되돌려 쓰기 Task를 따라간다 — 세션 Task는 백업이,
/// 되돌려 쓰기 Task는 VM이 주인이다(클러스터 Task가 아님).
#[tokio::test]
async fn folder_scoped_users_follow_their_file_restore_tasks() {
    let fr = start(2, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    let (status, folder) = admin
        .post("/folders", json!({ "name": "team", "kind": "vm" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{folder}");
    let team: uuid::Uuid = folder["id"].as_str().unwrap().parse().unwrap();
    // 백업한 때의 폴더가 team인 백업과 team 안의 VM
    let mut backup = env
        .cluster
        .get::<BackupSpec>(fr.backup.id)
        .unwrap()
        .unwrap();
    backup.spec.vm_spec.folder = Some(team);
    env.cluster
        .update_spec(backup.id, None, backup.labels.clone(), &backup.spec)
        .await
        .unwrap();
    let mut req = vm_request("web");
    req["spec"]["guest_agent"] = json!({});
    req["spec"]["folder"] = json!(team);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let vm_id: uuid::Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    env.hv.set_guest_agent(vm_id, GuestAnswer::Ok);
    env.hv.set_guest_fs(vm_id, MockGuestFs::linux(&["/srv"]));
    let t = admin.act("web", "start").await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");

    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "team-restore", "privileges": ["system.read", "vm.config", "backup.file_restore"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": "fu", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let (status, grant) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": user["id"] },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "folder", "id": team },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{grant}");
    let fu = env.login("fu", "password-1").await;

    let (status, opened) = open(&fu, "web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{opened}");
    let task = opened["task"].as_str().unwrap();
    let (status, t) = fu.get(&format!("/tasks/{task}")).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["spec"]["op"]["kind"], "file-restore-session");
    let id = opened["session"]["id"].as_str().unwrap().to_string();
    wait_phase(&fu, &id, "ready").await;

    let (status, t) = fu
        .post(
            &format!("/file-restore/sessions/{id}/restore-to-vm"),
            json!({ "volume": "disk0p1", "paths": ["/etc/hostname"], "vm": "web", "target_dir": "/srv" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{t}");
    let task = t["id"].as_str().unwrap();
    let t = wait_any(&fu, &t).await;
    assert_eq!(t["status"]["state"], "succeeded", "{t}");
    let (status, got) = fu.get(&format!("/tasks/{task}")).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["op"]["kind"], "file-restore-to-guest");
    // 폴더 밖 사용자는 보지 못한다.
    let other = env.user("other", "none").await;
    let (status, _) = other.get(&format!("/tasks/{task}")).await;
    assert_ne!(status, StatusCode::OK);
}

/// 노드 비우기는 그 노드의 세션을 닫고(설계 §13), 유지보수 노드에는 새 세션을 열지 않는다.
#[tokio::test]
async fn draining_the_node_closes_its_sessions() {
    let fr = start(2, Duration::from_secs(600)).await;
    let env = &fr.env;
    let admin = env.admin().await;
    let s = ready(&admin, "web-1").await;
    let id = s["id"].as_str().unwrap().to_string();
    let (status, task) = admin
        .post(&format!("/nodes/{}/drain", env.node), Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let t = wait_task_done(env, &id).await;
    assert_eq!(t.status.state, TaskState::Succeeded, "{t:?}");
    assert!(fr.sessions.is_empty());
    let (status, body) = open(&admin, "web-1").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("file_restore_unavailable")),
        "{body}"
    );
}
