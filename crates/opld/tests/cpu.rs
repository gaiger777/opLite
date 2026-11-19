//! 실제 QEMU로 게스트 CPU 모델·플래그를 본다(계획 63): 노드 탐침이 올린 카탈로그, 숨긴 플래그가 vCPU에서 꺼졌는지(QMP
//! `qom-get`), 실행 중 붙인 vCPU도 꺼졌는지, 숨김 목록을 바꾸면 검사 루프가 보류 `cpu`를 쓰고 지우는지, 마이그레이션
//! 받는 쪽이 기록된 CPU 그대로인지(받는 때의 숨김 목록과 상관없이), 노드가 못 주는 기능이 있는 모델은 켜지 않는지(QEMU
//! `enforce`와 Orbivisor의 카탈로그 확인).
//! `cargo test -p opld --features cpu-it --test cpu`
//!
//! 모델·플래그와 숨김 목록은 CLI(`vm set --cpu-model/--cpu-flags`, `settings set cpu.mask=…`)로 바꾸고, vCPU 속성과
//! 카탈로그는 제어 소켓의 시험 고리(`/v1/test/vcpu-props`·`cpu-catalog`)로 읽는다. 숨길 플래그는 러너 CPU를 모르므로 `host`로 켠 vCPU에
//! 있는 것을 고른다(x86_64: `avx512vbmi` → `avx2` → `sse4.2`, aarch64: `sve` → `pauth`). 이 Mac(aarch64 HVF)은 TCG로 돌리고
//! (HVF는 명명 모델을 못 켜고 마이그레이션도 막혀 있다) 명명 모델 `neoverse-n1`을 켠다. vCPU 핫플러그와 `enforce`는 x86_64만이다.
#![cfg(feature = "cpu-it")]

mod common;

use std::time::Duration;

use common::{Daemon, eventually};
use oplctl::Client;
use serde_json::{Value, json};

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
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

/// `[hypervisor]`: 한 호스트의 두 노드라 스코프를 끈다. 이 Mac은 TCG다.
fn hypervisor() -> &'static str {
    if cfg!(target_os = "macos") {
        "systemd_scope = false\naccel = \"tcg\""
    } else {
        "systemd_scope = false"
    }
}

fn daemon(name: &str, ports: &str) -> Daemon {
    Daemon::with_hypervisor(
        name,
        &format!("[reconciler]\nacpi_timeout_secs = 5\n[migration]\nports = \"{ports}\"\n"),
        hypervisor(),
    )
}

/// 제어 소켓의 시험 고리
async fn hook(d: &Daemon, path: &str, body: Value) -> Value {
    let http = reqwest::Client::builder()
        .unix_socket(d.path("opld.sock"))
        .build()
        .unwrap();
    let resp = http
        .post(format!("http://opld/v1/test/{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert!(status.is_success(), "{path}: {status} {text}");
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap()
    }
}

/// 꺼진 VM `web`의 CPU 모델·플래그를 CLI로 바꾼다(`vm set --cpu-model --cpu-flags`).
async fn set_cpu(d: &Daemon, model: &str, flags: &[&str]) {
    let flags = if flags.is_empty() {
        "--no-cpu-flags".to_string()
    } else {
        format!("--cpu-flags={}", flags.join(","))
    };
    d.ctl(&["vm", "set", "web", "--cpu-model", model, &flags])
        .await
        .unwrap();
}

/// 클러스터 숨김 목록을 CLI로 바꾼다(`settings set cpu.mask=…`).
async fn set_mask(d: &Daemon, mask: &[&str]) {
    let value = format!("cpu.mask={}", mask.join(","));
    d.ctl(&["settings", "set", &value]).await.unwrap();
}

/// 도는 VM `web`의 vCPU마다 속성 `names`
async fn props(d: &Daemon, names: &[&str]) -> Vec<serde_json::Map<String, Value>> {
    let v = hook(d, "vcpu-props", json!({ "vm": "web", "names": names })).await;
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_object().unwrap().clone())
        .collect()
}

/// 숨길 플래그 후보(이 노드 아키텍처)
fn candidates() -> &'static [&'static str] {
    if cfg!(target_arch = "x86_64") {
        &["avx512vbmi", "avx2", "sse4.2"]
    } else {
        &["sve", "pauth"]
    }
}

/// 노드가 카탈로그를 올릴 때까지 기다린다.
async fn wait_catalog(c: &Client, node: &str) {
    eventually("CPU catalog published", || async {
        let n = c.get(&format!("/nodes/{node}")).await.unwrap();
        n["status"]["cpu"]["catalog"]
            .as_str()
            .is_some_and(|h| !h.is_empty())
    })
    .await;
}

async fn vm(c: &Client) -> Value {
    c.get("/vms/web").await.unwrap()
}

fn resolved(vm: &Value) -> &Value {
    &vm["status"]["hardware"]["cpu_resolved"]
}

/// `host`로 켠 vCPU에 켜져 있는 첫 후보와, 그다음 후보(없으면 None)
async fn pick_flags(d: &Daemon) -> (String, Option<String>) {
    let all = props(d, candidates()).await;
    let on: Vec<String> = candidates()
        .iter()
        .filter(|f| all[0].get(**f) == Some(&Value::Bool(true)))
        .map(|f| f.to_string())
        .collect();
    assert!(!on.is_empty(), "no candidate flag on this host: {all:?}");
    (on[0].clone(), on.get(1).cloned())
}

async fn restart(d: &Daemon) {
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
}

/// 노드 하나: 숨긴 플래그, 실행 중 붙인 vCPU, 숨김 목록과 보류, 명명 모델.
#[tokio::test]
async fn masked_flags_are_off_in_every_vcpu() {
    let mut d = daemon("node1", "60300-60309");
    d.start().await;
    d.init("cpu").await;
    d.login().await;
    let c = client(&d);
    wait_catalog(&c, "node1").await;

    d.ctl(&[
        "vm",
        "create",
        "web",
        "--memory",
        "256",
        "--max-vcpus",
        "2",
        "--start",
    ])
    .await
    .unwrap();
    let (flag, other) = pick_flags(&d).await;
    let v = vm(&c).await;
    // `host`는 TCG(이 Mac, KVM 없는 러너)에서 `max`다.
    assert!(
        ["host", "max"].contains(&resolved(&v)["model"].as_str().unwrap()),
        "{v}"
    );

    // VM 플래그로 숨긴다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    set_cpu(&d, "host", &[&format!("-{flag}")]).await;
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let v = vm(&c).await;
    assert_eq!(resolved(&v)["flags"], json!({ &flag: false }), "{v}");
    for p in props(&d, &[&flag]).await {
        assert_eq!(p.get(&flag), Some(&Value::Bool(false)), "{p:?}");
    }

    // 실행 중 붙인 vCPU도 같은 CPU 형식의 전역 속성이라 꺼져 있다(x86_64만).
    if cfg!(target_arch = "x86_64") {
        let out = d.ctl(&["vm", "set", "web", "--cores", "2"]).await.unwrap();
        assert_eq!(
            out,
            "vm web: hardware updated\nvm web: applied while running\n"
        );
        let all = props(&d, &[&flag]).await;
        assert_eq!(all.len(), 2, "{all:?}");
        for p in all {
            assert_eq!(p.get(&flag), Some(&Value::Bool(false)), "{p:?}");
        }
    }

    // 숨김 목록을 더하면 검사 루프가 보류 `cpu`를 쓰고(실행 중 CPU는 그대로), 빼면 지운다.
    if let Some(other) = &other {
        set_mask(&d, &[other.as_str()]).await;
        eventually("pending cpu change", || async {
            let v = vm(&c).await;
            v["status"]["hardware"]["pending"]
                .as_array()
                .is_some_and(|p| p.iter().any(|p| p["field"] == "cpu"))
        })
        .await;
        let v = vm(&c).await;
        let p = &v["status"]["hardware"]["pending"][0];
        assert_eq!(p["reason"], "restart", "{v}");
        assert!(
            p["wanted"].as_str().unwrap().contains(&format!("-{other}")),
            "{v}"
        );
        assert_eq!(
            props(&d, &[other]).await[0].get(other),
            Some(&Value::Bool(true))
        );
        // 전원을 껐다 켜면 적용된다.
        restart(&d).await;
        let v = vm(&c).await;
        assert_eq!(v["status"]["hardware"]["pending"], Value::Null, "{v}");
        assert_eq!(
            props(&d, &[other]).await[0].get(other),
            Some(&Value::Bool(false))
        );
        set_mask(&d, &[]).await;
        eventually("pending cpu change", || async {
            vm(&c).await["status"]["hardware"]["pending"]
                .as_array()
                .is_some_and(|p| p.iter().any(|p| p["field"] == "cpu"))
        })
        .await;
    }

    // 명명 모델(버전 고정, x86_64는 카탈로그가 켤 수 있다는 것, aarch64 TCG는 `neoverse-n1`)
    let named = named_model(&d).await;
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    set_cpu(&d, &named, &[]).await;
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let v = vm(&c).await;
    assert_eq!(resolved(&v)["model"], named, "{v}");
    assert_eq!(
        resolved(&v)["enforce"].as_bool().unwrap_or(false),
        cfg!(target_arch = "x86_64"),
        "{v}"
    );

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.stop();
}

/// 이 노드 카탈로그(시험 고리)
async fn catalog(d: &Daemon) -> Value {
    hook(d, "cpu-catalog", json!({})).await
}

/// 카탈로그의 플래그 이름(비트 순번 → 이름)
fn names(catalog: &Value, bits: &Value) -> Vec<String> {
    let flags: Vec<&str> = catalog["flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    let bits = bits.as_str().unwrap_or("");
    let bytes = base64url(bits);
    let mut out = Vec::new();
    for (i, byte) in bytes.iter().enumerate() {
        for b in 0..8 {
            if byte & (1 << b) != 0 {
                out.push(flags[i * 8 + b].to_string());
            }
        }
    }
    out
}

fn base64url(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'-' => 62,
        _ => 63,
    };
    let mut out = Vec::new();
    let (mut acc, mut n) = (0u32, 0);
    for c in s.bytes() {
        acc = (acc << 6) | u32::from(val(c));
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
            acc &= (1 << n) - 1;
        }
    }
    out
}

/// 이 노드가 켤 수 있는 명명 모델. x86_64는 카탈로그에서 빠진 것이 없는 버전 모델(뒤에서부터), aarch64는 `neoverse-n1`.
async fn named_model(d: &Daemon) -> String {
    if cfg!(target_arch = "aarch64") {
        return "neoverse-n1".into();
    }
    let cat = catalog(d).await;
    cat["models"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| {
            m["alias_of"].is_null()
                && names(&cat, &m["missing"]).is_empty()
                && m["missing_other"].as_array().is_none_or(Vec::is_empty)
                && m["name"].as_str().unwrap().contains("-v")
        })
        .map(|m| m["name"].as_str().unwrap().to_string())
        .expect("no usable named CPU model")
}

/// 노드가 못 주는 기능이 있는 모델: QEMU는 `enforce`로 켜지 않고 그 기능을 끄면 켠다. Orbivisor는 카탈로그로 먼저
/// 거절한다(x86_64만 — aarch64 QEMU에는 `enforce`가 없다).
#[cfg(target_arch = "x86_64")]
#[tokio::test]
async fn enforce_refuses_a_model_with_missing_features() {
    let mut d = daemon("node1", "60310-60319");
    d.start().await;
    d.init("cpu-enforce").await;
    d.login().await;
    let c = client(&d);
    wait_catalog(&c, "node1").await;
    let cat = catalog(&d).await;
    let (model, missing) = cat["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            m["alias_of"].is_null() && m["missing_other"].as_array().is_none_or(Vec::is_empty)
        })
        .map(|m| {
            (
                m["name"].as_str().unwrap().to_string(),
                names(&cat, &m["missing"]),
            )
        })
        .find(|(_, missing)| !missing.is_empty())
        .expect("every CPU model is usable on this host");
    eprintln!("{model} lacks {missing:?}");

    // QEMU 그대로: enforce면 켜지 않고, 빠진 기능을 끄면 켠다.
    let accel = cat["accel"].as_str().unwrap();
    let qemu = |cpu: String| {
        std::process::Command::new("qemu-system-x86_64")
            .args(["-machine", &format!("q35,accel={accel}"), "-cpu", &cpu])
            .args(["-nodefaults", "-display", "none", "-S", "-qmp", "stdio"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let run = |cpu: String| {
        use std::io::Write;
        let mut child = qemu(cpu);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"execute\":\"qmp_capabilities\"}\n{\"execute\":\"quit\"}\n")
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let out = run(format!("{model},enforce"));
    assert!(!out.status.success(), "{model},enforce started");
    let off: String = missing.iter().map(|f| format!(",{f}=off")).collect();
    let out = run(format!("{model},enforce{off}"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Orbivisor: 스케줄러가 카탈로그로 먼저 거절하고(409 `no_node`) 노드마다 까닭을 붙인다. 빠진 기능을 끄면 켠다.
    d.ctl(&["vm", "create", "web", "--memory", "256"])
        .await
        .unwrap();
    set_cpu(&d, &model, &[]).await;
    let err = d.ctl(&["vm", "start", "web"]).await.unwrap_err();
    assert!(err.to_string().contains("no node can run this VM"), "{err}");
    assert!(
        err.to_string()
            .contains(&format!("cannot run CPU model {model} (missing")),
        "{err}"
    );
    let off: Vec<String> = missing.iter().map(|f| format!("-{f}")).collect();
    let off: Vec<&str> = off.iter().map(String::as_str).collect();
    set_cpu(&d, &model, &off).await;
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let v = vm(&c).await;
    assert_eq!(resolved(&v)["enforce"], true, "{v}");
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.stop();
}

/// 마이그레이션 받는 쪽은 기록된 CPU 그대로 켠다: 켠 뒤 숨김 목록을 바꿔도 대상의 vCPU는 원본과 같다.
#[tokio::test]
async fn the_receiving_side_keeps_the_recorded_cpu() {
    let mut d = [
        daemon("node1", "60320-60329"),
        daemon("node2", "60330-60339"),
    ];
    for d in &mut d {
        d.start().await;
    }
    d[0].init("cpu-mig").await;
    d[0].login().await;
    let url = d[0].join_url.clone().unwrap();
    d[1].join(&url).await.unwrap();
    let c = client(&d[0]);
    eventually("both nodes online", || async {
        let nodes = c.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap();
        nodes.len() == 2 && nodes.iter().all(|n| n["status"]["online"] == true)
    })
    .await;
    wait_catalog(&c, "node1").await;
    wait_catalog(&c, "node2").await;

    d[0].ctl(&[
        "vm", "create", "web", "--memory", "256", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let (flag, other) = pick_flags(&d[0]).await;
    d[0].ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    set_cpu(&d[0], "host", &[&format!("-{flag}")]).await;
    d[0].ctl(&["vm", "start", "web"]).await.unwrap();
    let before = resolved(&vm(&c).await).clone();
    // 켠 뒤 숨김 목록을 바꾼다(받는 쪽이 새로 해석하면 달라진다).
    let names: Vec<&str> = std::iter::once(flag.as_str())
        .chain(other.as_deref())
        .collect();
    if let Some(other) = &other {
        set_mask(&d[0], &[other.as_str()]).await;
    }

    let task = c
        .post("/vms/web/migrate", &json!({ "node": "node2" }))
        .await
        .unwrap();
    c.wait_task(&task).await.unwrap();
    eventually("running on node2", || async {
        let v = vm(&c).await;
        v["status"]["phase"] == "running"
            && v["status"]["pid"].is_u64()
            && v["status"]["node"] == c.get("/nodes/node2").await.unwrap()["id"]
    })
    .await;
    let v = vm(&c).await;
    assert_eq!(resolved(&v), &before, "{v}");
    let after = props(&d[1], &names).await;
    assert_eq!(after[0].get(&flag), Some(&Value::Bool(false)), "{after:?}");
    if let Some(other) = &other {
        assert_eq!(after[0].get(other), Some(&Value::Bool(true)), "{after:?}");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    d[0].ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d[0].ctl(&["vm", "delete", "web"]).await.unwrap();
    for d in &mut d {
        d.stop();
    }
}
