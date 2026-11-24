//! 파일 복원 세션 API(계획 69 스테이지 5, 가짜 하이퍼바이저의 도우미 — 에이전트 몸통 + 메모리 나무): 세션을 열고(다시 쓰기),
//! 볼륨·디렉터리를 보고, 파일 하나·zip·tar.gz를 링크 두 걸음으로 내려받고, 닫는다. 권한(`backup.file_restore`·남의 세션),
//! 기능 게이트, 노드 고르기(이미지 없음·꽉 참), 도우미 실패, 유휴 닫힘, 백업 지우기(`backup_in_use`·`force`)와 보존, 감사.
//! 노드 사이 넘김은 `two_nodes.rs`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::file_restore::{backup_pool, node_state, pool_backup, report, reported, sessions};
use common::{Client, Env, FakeStorage, WAIT};
use opl_api::FileRestoreSessions;
use opl_driver::DriverError;
use opl_types::backup::{Backup, BackupSpec};
use opl_types::node::{FILE_RESTORE, NodeSpec};
use opl_types::task::{TaskSpec, TaskState};
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
    let mut env = {
        let reg = reg.clone();
        Env::start_with_state(move |s| s.file_restore = reg).await
    };
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

async fn task_of(env: &Env, session: &str) -> opl_types::task::Task {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    tasks
        .into_iter()
        .find(|t| t.spec.target.to_string() == session)
        .unwrap()
}

async fn wait_task_done(env: &Env, session: &str) -> opl_types::task::Task {
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
    b.status.phase = opl_types::backup::BackupPhase::Pending;
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
