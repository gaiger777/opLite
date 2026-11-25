//! 계획 73 선언형 운영 실제 시험(설계 §14): 노드 둘과 진짜 QEMU로 `examples/declarative/web/`(폴더·SDN 없는 네트워크·볼륨
//! 둘·켜진 VM 둘)을 `orbivisorctl apply` → `diff` 0 → 메모리 바꾼 파일 `plan`(restart 표시) → `apply` 뒤 다시 켬 → 다른 노드의
//! API로 세션이 vCPU를 바꿈(출처 web) → `drift`가 그 칸과 바꾼 사람 → `restore vm --revision` → 드리프트 없음 → `export`를 같은
//! 묶음으로 `diff` 0(왕복) → `--minimal` 내보내기를 다른 이름 접두로 다시 apply → `restore --at`(폴더 범위) →
//! `apply --prune --prune-data`로 둘 다 지움(풀 디렉터리가 빈다). ACME는 여기 넣지 않는다(Pebble 작업 `acme`).
//! `cargo test -p opld --features qemu-it --test declarative`
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::Daemon;
use serde_json::{Value, json};

/// 드리프트 검사는 리더 루프가 10초마다 바뀜을 몰아 본다 — 기다리는 한도는 넉넉히.
const DRIFT_WAIT: Duration = Duration::from_secs(90);

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/declarative/web")
}

/// 예 디렉터리를 `to`에 베끼며 파일마다 글을 바꾼다(`edit(파일 이름, 내용)`).
fn copy_dir(from: &Path, to: &Path, edit: impl Fn(&str, String) -> String) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        let text = std::fs::read_to_string(&p).unwrap();
        std::fs::write(to.join(&name), edit(&name, text)).unwrap();
    }
}

/// oplctl을 돌려 끝 코드(`oplctl::Exit` — diff·drift의 0/1)와 출력을 돌려준다. 다른 오류는 실패.
async fn ctl_code(d: &Daemon, args: &[&str]) -> (u8, String) {
    let config = d.path("oplctl.json");
    let mut argv = vec!["oplctl", "--config", config.to_str().unwrap()];
    argv.extend_from_slice(args);
    let mut out = Vec::new();
    let code = match Box::pin(oplctl::run(argv, &mut out)).await {
        Ok(()) => 0,
        Err(e) => match e.downcast_ref::<oplctl::Exit>() {
            Some(x) => x.code,
            None => panic!("oplctl {args:?}: {e:#}\n{}", String::from_utf8_lossy(&out)),
        },
    };
    (code, String::from_utf8(out).unwrap())
}

/// 웹처럼 로그인 세션(쿠키 `opl_session`)으로 VM 하나의 spec을 바꾼다 — `oplctl login`은 API 토큰이라 출처가 `api`다.
/// 클라이언트 머리(`Orbivisor-Client`)가 없는 세션 요청은 출처 `web`으로 기록된다.
async fn web_edit_vm(d: &Daemon, password: &str, vm: &str, edit: impl FnOnce(&mut Value)) {
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let base = format!("{}/api/v1", d.server());
    let resp = http
        .post(format!("{base}/auth/login"))
        .json(&json!({ "username": "admin", "password": password }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "login: {}", resp.status());
    let cookie = resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let live: Value = http
        .get(format!("{base}/vms/{vm}"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut spec = live["spec"].clone();
    edit(&mut spec);
    let resp = http
        .put(format!("{base}/vms/{vm}"))
        .header("cookie", &cookie)
        .json(&json!({ "labels": live["labels"], "spec": spec }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "PUT /vms/{vm}: {}",
        resp.status()
    );
}

async fn wait_phase(d: &Daemon, vm: &str, phase: &str) {
    // TCG(svc01 aarch64)에서는 두 VM이 함께 켜지는 데 30초를 넘길 수 있다 — VMware 실제 시험처럼 넉넉히 두고,
    // 시간이 지나면 마지막 상태를 보인다.
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let got = d.ctl_json(&["vm", "get", vm]).await;
        if got["status"]["phase"] == phase {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for vm {vm} {phase}: {}",
            got["status"]
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 묶음 `set`의 드리프트 보고가 `ok`를 만족할 때까지 기다린다(보고를 돌려준다).
async fn wait_drift(d: &Daemon, set: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + DRIFT_WAIT;
    loop {
        let (_, out) = ctl_code(d, &["-o", "json", "drift", set]).await;
        let report: Value = serde_json::from_str(&out).unwrap();
        let drift = report[0]["drift"].clone();
        if ok(&drift) {
            return drift;
        }
        assert!(Instant::now() < deadline, "drift of {set}: {drift}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn items(drift: &Value) -> Vec<Value> {
    drift["items"].as_array().cloned().unwrap_or_default()
}

/// 리소스 하나의 이력(가장 새 것 먼저)
async fn history(d: &Daemon, kind: &str, name: &str) -> Vec<Value> {
    d.ctl_json(&["history", kind, name]).await["items"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn manifests_apply_drift_restore_export_and_prune_on_two_nodes() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut a = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    let mut b = Daemon::new("node2", "[reconciler]\nacpi_timeout_secs = 5\n");
    a.start().await;
    b.start().await;
    a.init("declarative").await;
    a.login().await;
    let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
    b.join(url.trim()).await.unwrap();
    b.login_with(
        a.admin_password.as_deref().unwrap(),
        a.ca_fingerprint.as_deref().unwrap(),
    )
    .await;

    // 두 노드가 함께 쓰는 디렉터리 풀 `local`(예 매니페스트의 풀 이름)
    let shared = tempfile::Builder::new()
        .prefix("opldecl")
        .tempdir_in("/tmp")
        .unwrap();
    let pool = shared.path().to_str().unwrap();
    a.ctl(&["pool", "create", "local", "--path", pool, "--shared"])
        .await
        .unwrap();
    a.wait_pool_ready("local").await;
    b.wait_pool_ready("local").await;

    // 예 그대로 — 다만 이 Mac에는 브리지 vmbr0이 없으니 VM NIC만 사용자 모드(`user`)로 바꾼다. 네트워크 web-net은 만든다.
    let work = a.path("manifests");
    let web = work.join("web");
    copy_dir(&examples(), &web, |name, text| {
        if name == "80-vms.yaml" {
            assert!(text.contains("network: web-net"), "{text}");
            text.replace("network: web-net", "network: user")
        } else {
            text
        }
    });
    let web_s = web.to_str().unwrap();

    // 1. 적용: 폴더 → 네트워크 → 볼륨 → VM 차례, VM 둘이 진짜로 켜진다.
    let out = a
        .ctl(&["apply", "-f", web_s, "--set", "web", "--yes"])
        .await
        .unwrap();
    assert!(out.contains("+ Vm web01"), "{out}");
    for vm in ["web01", "web02"] {
        wait_phase(&a, vm, "running").await;
    }
    let vm = a.ctl_json(&["vm", "get", "web01"]).await;
    assert_eq!(vm["spec"]["memory_mib"], 256, "{vm}");
    assert_eq!(vm["labels"]["orbivisor.io/apply-set"], "web", "{vm}");
    assert_eq!(vm["labels"]["tier"], "web", "{vm}");
    let names: Vec<Value> = a
        .ctl_json(&["vm", "list", "--folder", "/demo/web"])
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].clone())
        .collect();
    assert_eq!(names, ["web01", "web02"]);
    let net = a.ctl_json(&["network", "get", "web-net"]).await;
    assert_eq!(net["spec"]["bridge"], "vmbr0", "{net}");
    let rows = history(&a, "vm", "web01").await;
    assert_eq!(rows[0]["source"], "apply", "{rows:?}");
    assert_eq!(rows[0]["actor"]["user"], "admin", "{rows:?}");
    // 이 시점(폴더 시점 되돌리기의 목표)
    let before_memory = a.ctl_json(&["history", "--limit", "1"]).await["items"][0]["revision"]
        .as_u64()
        .unwrap();

    // 2. 다시 견주면 같다(끝 코드 0). 묶음 드리프트도 없다(드리프트 검사는 차이가 없으면 보고를 쓰지 않는다 — `null`).
    let (code, out) = ctl_code(&a, &["diff", "-f", web_s, "--set", "web"]).await;
    assert_eq!(code, 0, "{out}");
    let (code, out) = ctl_code(&a, &["drift", "web"]).await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("No drift."), "{out}");

    // 3. web01 메모리를 바꾼 파일: diff 1, plan이 켜진 VM의 재시작을 알린다, 적용 뒤 다시 켜면 512 MiB로 돈다.
    let bigger = work.join("bigger");
    copy_dir(&web, &bigger, |name, text| {
        if name == "80-vms.yaml" {
            text.replacen("memory_mib: 256Mi", "memory_mib: 512Mi", 1)
        } else {
            text
        }
    });
    let bigger_s = bigger.to_str().unwrap();
    let (code, out) = ctl_code(&a, &["diff", "-f", bigger_s, "--set", "web"]).await;
    assert_eq!(code, 1, "{out}");
    let out = a
        .ctl(&["plan", "-f", bigger_s, "--set", "web"])
        .await
        .unwrap();
    assert!(out.contains("~ Vm web01"), "{out}");
    assert!(out.contains("memory_mib: 256 -> 512"), "{out}");
    assert!(out.contains("next restart"), "{out}");
    assert!(!out.contains("web02"), "only web01 changes:\n{out}");
    a.ctl(&[
        "apply",
        "-f",
        bigger_s,
        "--set",
        "web",
        "--reason",
        "more memory",
        "--yes",
    ])
    .await
    .unwrap();
    let vm = a.ctl_json(&["vm", "get", "web01"]).await;
    assert_eq!(vm["spec"]["memory_mib"], 512, "{vm}");
    a.ctl(&["vm", "stop", "web01", "--force"]).await.unwrap();
    a.ctl(&["vm", "start", "web01"]).await.unwrap();
    wait_phase(&a, "web01", "running").await;
    // 끄고 켠 것은 CLI 토큰의 줄(출처 api, `power`)이고, 그 앞이 적용 줄(사유와 함께)이다.
    let rows = history(&a, "vm", "web01").await;
    assert_eq!(rows[0]["source"], "api", "{rows:?}");
    assert_eq!(rows[0]["changed"], json!(["power"]), "{rows:?}");
    let row = rows.iter().find(|r| r["source"] == "apply").unwrap();
    assert_eq!(row["changed"], json!(["memory_mib"]), "{rows:?}");
    assert_eq!(row["reason"], "more memory", "{rows:?}");
    let applied = row["revision"].as_u64().unwrap();

    // 4. 다른 노드의 API로 로그인 세션이 vCPU를 바꾼다(웹과 같은 길 — 쿠키, 클라이언트 머리 없음) → 드리프트 Modified + 누가.
    web_edit_vm(&b, a.admin_password.as_deref().unwrap(), "web01", |spec| {
        spec["cpu"]["cores"] = json!(2);
    })
    .await;
    let rows = history(&a, "vm", "web01").await;
    assert_eq!(rows[0]["source"], "web", "{rows:?}");
    let drift = wait_drift(&a, "web", |d| !items(d).is_empty()).await;
    let item = &items(&drift)[0];
    assert_eq!(item["kind"], "vm", "{drift}");
    assert_eq!(item["name"], "web01", "{drift}");
    assert_eq!(item["state"], "modified", "{drift}");
    assert!(
        item["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().is_some_and(|p| p.starts_with("cpu"))),
        "{drift}"
    );
    assert_eq!(item["by"]["source"], "web", "{drift}");
    assert_eq!(item["by"]["actor"]["user"], "admin", "{drift}");
    let (code, out) = ctl_code(&a, &["drift", "web"]).await;
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("web01"), "{out}");

    // 5. 이력 줄 하나로 되돌린다(적용한 모양 — vCPU 1·메모리 512) → 드리프트가 사라진다.
    let out = a
        .ctl(&[
            "restore",
            "vm",
            "web01",
            "--revision",
            &applied.to_string(),
            "--reason",
            "undo the web change",
            "--yes",
        ])
        .await
        .unwrap();
    assert!(out.contains("Vm web01"), "{out}");
    let vm = a.ctl_json(&["vm", "get", "web01"]).await;
    assert_eq!(vm["spec"]["cpu"]["cores"], 1, "{vm}");
    assert_eq!(vm["spec"]["memory_mib"], 512, "{vm}");
    let rows = history(&a, "vm", "web01").await;
    assert_eq!(rows[0]["restored_from"], applied, "{rows:?}");
    assert_eq!(rows[0]["source"], "cli", "{rows:?}");
    assert_eq!(rows[0]["reason"], "undo the web change", "{rows:?}");
    wait_drift(&a, "web", |d| items(d).is_empty()).await;

    // 6. 내보낸 파일을 같은 묶음으로 견주면 같다(왕복).
    let exported = work.join("exported");
    a.ctl(&[
        "export",
        "--set",
        "web",
        "--dir",
        exported.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let mut files: Vec<String> = std::fs::read_dir(&exported)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "10-folders.yaml",
            "30-networks.yaml",
            "70-volumes.yaml",
            "80-vms.yaml"
        ]
    );
    let (code, out) = ctl_code(
        &a,
        &["diff", "-f", exported.to_str().unwrap(), "--set", "web"],
    )
    .await;
    assert_eq!(code, 0, "{out}");

    // 7. 사람이 읽을 내보내기(`--minimal` — MAC 없음)를 이름 접두 app·폴더 /copy/app으로 바꿔 다른 묶음으로 적용한다(꺼진
    // 채). 폴더 /demo를 함께 쓰면 다른 묶음이 관리하는 객체라 막힌다(`managed_elsewhere`).
    let minimal = work.join("minimal");
    a.ctl(&[
        "export",
        "--set",
        "web",
        "--minimal",
        "--dir",
        minimal.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let app = work.join("app");
    copy_dir(&minimal, &app, |_, text| {
        assert!(!text.contains("mac:"), "{text}");
        text.replace("web", "app")
            .replace("demo", "copy")
            .replace("state: running", "state: stopped")
    });
    let app_s = app.to_str().unwrap();
    let out = a
        .ctl(&["apply", "-f", app_s, "--set", "app", "--yes"])
        .await
        .unwrap();
    assert!(out.contains("+ Vm app01"), "{out}");
    let vm = a.ctl_json(&["vm", "get", "app01"]).await;
    assert_eq!(vm["spec"]["memory_mib"], 512, "{vm}");
    assert_eq!(vm["spec"]["disks"][0]["volume"], "app01-root", "{vm}");
    assert_eq!(vm["spec"]["power"], "stopped", "{vm}");
    let (code, out) = ctl_code(&a, &["diff", "-f", app_s, "--set", "app"]).await;
    assert_eq!(code, 0, "{out}");

    // 8. 폴더 /demo/web를 메모리를 바꾸기 전 시점으로 → web01이 256 MiB 선언으로(app 묶음은 범위 밖이라 그대로).
    let out = a
        .ctl(&[
            "restore",
            "--at",
            &before_memory.to_string(),
            "--folder",
            "/demo/web",
            "--yes",
        ])
        .await
        .unwrap();
    assert!(out.contains("Vm web01"), "{out}");
    assert!(!out.contains("app01"), "{out}");
    let vm = a.ctl_json(&["vm", "get", "web01"]).await;
    assert_eq!(vm["spec"]["memory_mib"], 256, "{vm}");
    let vm = a.ctl_json(&["vm", "get", "app01"]).await;
    assert_eq!(vm["spec"]["memory_mib"], 512, "{vm}");

    // 9. 폴더만 남긴 파일 + 가지치기: 데이터 종류는 --prune-data가 있어야 지운다.
    for (set, dir) in [("web", &web), ("app", &app)] {
        let only = work.join(format!("{set}-folders"));
        std::fs::create_dir_all(&only).unwrap();
        std::fs::copy(dir.join("10-folders.yaml"), only.join("10-folders.yaml")).unwrap();
        let only_s = only.to_str().unwrap();
        let out = a
            .ctl(&["plan", "-f", only_s, "--set", set, "--prune"])
            .await
            .unwrap();
        assert!(out.contains("prune-data"), "{out}");
        a.ctl(&[
            "apply",
            "-f",
            only_s,
            "--set",
            set,
            "--prune",
            "--prune-data",
            "--yes",
        ])
        .await
        .unwrap();
    }
    for vm in ["web01", "web02", "app01", "app02"] {
        assert!(a.ctl(&["vm", "get", vm]).await.is_err(), "{vm} is left");
    }
    for net in ["web-net", "app-net"] {
        assert!(
            a.ctl(&["network", "get", net]).await.is_err(),
            "{net} is left"
        );
    }
    common::eventually("pool directory empty", || async {
        std::fs::read_dir(shared.path()).unwrap().count() == 0
    })
    .await;
    // 지운 것도 이력에 남는다.
    let rows = history(&a, "vm", "web02").await;
    assert_eq!(rows[0]["op"], "delete", "{rows:?}");
    b.stop();
    a.stop();
}
