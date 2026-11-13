//! 전원 끄기 펜싱 실행기([`Fencer`])와 [`power_off`]: 가짜 `ipmitool` 셸 스크립트, 프로세스 안의 가짜 Redfish
//! 서버(127.0.0.1), 명령 방법.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use opl_api::power_fence::{Fencer, PowerFence, PowerOff, power_off, power_off_within};
use opl_types::fence::{
    CommandFence, FenceKind, FenceMethod, IpmiFence, NodeFenceSpec, Power, RedfishFence,
};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

fn node() -> Node {
    let mut n = Resource::new(
        "node-b",
        NodeSpec {
            address: "10.1.2.3:8006".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    n.id = Uuid::from_u128(0xb);
    n
}

/// 가짜 `ipmitool`: 부를 때마다 `<인자> pw=<IPMI_PASSWORD>`를 `calls`에 더한다. `fail`이 있으면 실패하고,
/// `power off`면 `stuck`이 없을 때 `power`를 `off`로 바꾼다. `power status`는 `Chassis Power is <power>`다.
const IPMITOOL: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$* pw=${IPMI_PASSWORD-unset}" >> "$d/calls"
if [ -f "$d/fail" ]; then
    echo "Error: Unable to establish IPMI v2 / RMCP+ session ($*)" >&2
    exit 1
fi
case "$*" in
*"power off") [ -f "$d/stuck" ] || echo off > "$d/power"; echo "Chassis Power Control: Down/Off" ;;
*"power status") echo "Chassis Power is $(cat "$d/power")" ;;
esac
"#;

struct FakeIpmi {
    dir: TempDir,
}

impl FakeIpmi {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ipmitool");
        std::fs::write(&path, IPMITOOL).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        std::fs::write(dir.path().join("power"), "on\n").unwrap();
        Self { dir }
    }

    fn fencer(&self) -> Fencer {
        Fencer {
            ipmitool: self.dir.path().join("ipmitool"),
            timeout: Duration::from_secs(5),
        }
    }

    fn touch(&self, name: &str) {
        std::fs::write(self.dir.path().join(name), "").unwrap();
    }

    fn calls(&self) -> Vec<String> {
        read_lines(&self.dir.path().join("calls"))
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn ipmi(password: Option<&str>) -> FenceMethod {
    FenceMethod::Ipmi(IpmiFence {
        address: "10.9.9.9".into(),
        port: 623,
        user: "admin".into(),
        password: password.map(Into::into),
        lanplus: true,
    })
}

#[tokio::test]
async fn ipmi_turns_the_node_off_with_the_password_in_the_environment() {
    let fake = FakeIpmi::new();
    let fencer = fake.fencer();
    let m = ipmi(Some("s3cret"));
    assert_eq!(fencer.status(&node(), &m).await, Ok(Power::On));
    fencer.off(&node(), &m).await.unwrap();
    assert_eq!(fencer.status(&node(), &m).await, Ok(Power::Off));
    let base = "-I lanplus -H 10.9.9.9 -p 623 -U admin -E chassis power";
    assert_eq!(
        fake.calls(),
        [
            format!("{base} status pw=s3cret"),
            format!("{base} off pw=s3cret"),
            format!("{base} status pw=s3cret"),
        ]
    );

    // 비밀번호가 없으면 `-E`도 환경 변수도 없다.
    std::fs::remove_file(fake.dir.path().join("calls")).unwrap();
    fencer.status(&node(), &ipmi(None)).await.unwrap();
    assert_eq!(
        fake.calls(),
        ["-I lanplus -H 10.9.9.9 -p 623 -U admin chassis power status pw=unset"]
    );

    // 0이 아닌 종료는 실패이고 종료 상태만 담는다(stderr의 BMC 주소는 모든 뷰어가 읽는 오류에 넣지 않는다).
    fake.touch("fail");
    let err = fencer.off(&node(), &m).await.unwrap_err();
    assert_eq!(err, "ipmitool failed (exit status: 1)");
}

#[tokio::test]
async fn power_off_confirms_and_gives_up_when_the_power_stays_on() {
    let fake = FakeIpmi::new();
    let fencer = fake.fencer();
    let spec = NodeFenceSpec {
        methods: vec![ipmi(Some("pw"))],
    };
    assert_eq!(
        power_off(&fencer, &node(), &spec).await,
        PowerOff::Off {
            method: FenceKind::Ipmi
        }
    );

    // 끄기는 성공해도 전원이 계속 켜져 있으면 확인 제한 시간 뒤 실패다.
    std::fs::write(fake.dir.path().join("power"), "on\n").unwrap();
    fake.touch("stuck");
    let started = std::time::Instant::now();
    let PowerOff::Failed(errors) =
        power_off_within(&fencer, &node(), &spec, Duration::from_secs(2)).await
    else {
        panic!("the power never went off");
    };
    assert!(started.elapsed() >= Duration::from_secs(1), "{errors:?}");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with("ipmi: power off not confirmed"),
        "{errors:?}"
    );
    assert!(errors[0].contains("power is on"), "{errors:?}");
}

/// 끌 때 `off-mark`를 만들고 받은 환경 변수를 `env`에 적는 명령 방법
fn command(dir: &Path) -> FenceMethod {
    let d = dir.display();
    FenceMethod::Command(CommandFence {
        off: format!(
            "echo \"$ORBIVISOR_NODE_NAME $ORBIVISOR_NODE_ID $ORBIVISOR_NODE_ADDRESS\" > {d}/env; touch {d}/off-mark"
        ),
        status: format!("if [ -f {d}/off-mark ]; then echo OFF; else echo ' on'; fi"),
    })
}

#[tokio::test]
async fn command_gets_the_node_in_its_environment() {
    let dir = TempDir::new().unwrap();
    let fencer = Fencer::default();
    let m = command(dir.path());
    assert_eq!(fencer.status(&node(), &m).await, Ok(Power::On));
    fencer.off(&node(), &m).await.unwrap();
    assert_eq!(fencer.status(&node(), &m).await, Ok(Power::Off));
    assert_eq!(
        read_lines(&dir.path().join("env")),
        [format!("node-b {} 10.1.2.3", Uuid::from_u128(0xb))]
    );

    // 첫 줄이 `on`·`off`가 아니면 모름, 0이 아닌 종료는 실패다.
    let garbage = FenceMethod::Command(CommandFence {
        off: "exit 3".into(),
        status: "echo standby; echo off".into(),
    });
    assert_eq!(fencer.status(&node(), &garbage).await, Ok(Power::Unknown));
    let err = fencer.off(&node(), &garbage).await.unwrap_err();
    assert!(err.contains("exit status: 3"), "{err}");
    // 명령의 출력은 비밀을 담을 수 있어 오류(Task·`fence_check`로 간다)에 담지 않는다.
    let err = fencer
        .off(
            &node(),
            &FenceMethod::Command(CommandFence {
                off: "echo nope >&2; false".into(),
                status: "echo on".into(),
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(err, "command failed (exit status: 1)");

    // 제한 시간을 넘긴 명령은 버린다.
    let slow = Fencer {
        timeout: Duration::from_millis(300),
        ..Fencer::default()
    };
    let hang = FenceMethod::Command(CommandFence {
        off: "sleep 10".into(),
        status: "sleep 10".into(),
    });
    let started = std::time::Instant::now();
    let err = slow.status(&node(), &hang).await.unwrap_err();
    assert!(err.starts_with("timed out"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// 명령에는 orbivisord의 환경을 넘기지 않는다: `PATH`와 `ORBIVISOR_NODE_*`(그리고 셸이 스스로 두는 것)만 있다.
#[tokio::test]
async fn a_command_gets_only_path_and_the_node_variables() {
    let dir = TempDir::new().unwrap();
    let d = dir.path().display();
    let m = FenceMethod::Command(CommandFence {
        off: "true".into(),
        status: format!("env > {d}/env; echo on"),
    });
    assert_eq!(Fencer::default().status(&node(), &m).await, Ok(Power::On));
    let names: Vec<String> = read_lines(&dir.path().join("env"))
        .iter()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_string()))
        .collect();
    assert!(names.iter().any(|n| n == "PATH"), "{names:?}");
    assert!(
        names.iter().any(|n| n == "ORBIVISOR_NODE_NAME"),
        "{names:?}"
    );
    // 셸이 두는 것 말고는 없다(시험 프로세스의 `HOME`·`CARGO_*` 등이 새지 않는다).
    let allowed = [
        "PATH",
        "ORBIVISOR_NODE_NAME",
        "ORBIVISOR_NODE_ID",
        "ORBIVISOR_NODE_ADDRESS",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
    ];
    assert!(
        names.iter().all(|n| allowed.contains(&n.as_str())),
        "{names:?}"
    );
}

#[tokio::test]
async fn power_off_falls_back_to_the_next_method() {
    let fake = FakeIpmi::new();
    fake.touch("fail");
    let fencer = fake.fencer();
    let dir = TempDir::new().unwrap();
    let spec = NodeFenceSpec {
        methods: vec![ipmi(Some("pw")), command(dir.path())],
    };
    assert_eq!(
        power_off(&fencer, &node(), &spec).await,
        PowerOff::Off {
            method: FenceKind::Command
        }
    );
    assert!(dir.path().join("off-mark").exists());

    // 모두 실패하면 방법마다의 이유
    let spec = NodeFenceSpec {
        methods: vec![
            ipmi(Some("pw")),
            FenceMethod::Command(CommandFence {
                off: "echo broken >&2; exit 1".into(),
                status: "echo on".into(),
            }),
        ],
    };
    let PowerOff::Failed(errors) = power_off(&fencer, &node(), &spec).await else {
        panic!("a method succeeded");
    };
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].starts_with("ipmi: "), "{errors:?}");
    assert!(errors[1].starts_with("command: "), "{errors:?}");
    assert!(!errors[1].contains("broken"), "{errors:?}");
}

/// 명령이 끝나면 출력 파이프를 쥔 채 남은 백그라운드 자식을 기다리지 않는다.
#[tokio::test]
async fn a_command_leaving_a_background_child_ends_with_the_command() {
    let fencer = Fencer {
        timeout: Duration::from_secs(4),
        ..Fencer::default()
    };
    let m = FenceMethod::Command(CommandFence {
        off: "true".into(),
        status: "sleep 6 & echo on".into(),
    });
    let started = std::time::Instant::now();
    assert_eq!(fencer.status(&node(), &m).await, Ok(Power::On));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

/// 제한 시간을 넘긴 명령은 그 명령이 띄운 손자 프로세스까지 끝낸다.
#[tokio::test]
async fn a_timed_out_command_kills_its_grandchildren() {
    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("pid");
    let fencer = Fencer {
        timeout: Duration::from_millis(500),
        ..Fencer::default()
    };
    let m = FenceMethod::Command(CommandFence {
        off: format!("sleep 30 & echo $! > {}; wait", pid_file.display()),
        status: "echo on".into(),
    });
    let err = fencer.off(&node(), &m).await.unwrap_err();
    assert!(err.starts_with("timed out"), "{err}");
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let pid = pid.trim();
    let alive = || {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while alive() {
        assert!(
            std::time::Instant::now() < deadline,
            "the grandchild {pid} is still running"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 프로세스 안의 가짜 Redfish BMC. `admin:pw`만 받고(아니면 401) `Reset`의 본문을 적는다.
#[derive(Clone, Default)]
struct Bmc {
    systems: Arc<AtomicUsize>,
    off: Arc<AtomicBool>,
    resets: Arc<Mutex<Vec<(String, Value)>>>,
    slow: Arc<AtomicBool>,
    /// `Reset`에 409로 답하고 끄지 않는다(이미 꺼진 노드에 409를 주는 BMC).
    refuse: Arc<AtomicBool>,
    /// `Systems`의 멤버가 다른 출처(포트)를 가리킨다.
    foreign: Arc<AtomicBool>,
}

impl Bmc {
    async fn start(systems: usize) -> (Bmc, String) {
        let bmc = Bmc::default();
        bmc.systems.store(systems, Ordering::SeqCst);
        let app = axum::Router::new()
            .route("/redfish/v1/Systems", get(list))
            .route("/redfish/v1/Systems/{id}", get(system))
            .route(
                "/redfish/v1/Systems/{id}/Actions/ComputerSystem.Reset",
                post(reset),
            )
            .with_state(bmc.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (bmc, url)
    }

    async fn check(&self, headers: &HeaderMap) -> Result<(), StatusCode> {
        if self.slow.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("admin:pw")
        );
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&expected) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(())
    }
}

async fn list(State(bmc): State<Bmc>, headers: HeaderMap) -> Response {
    if let Err(s) = bmc.check(&headers).await {
        return s.into_response();
    }
    let prefix = if bmc.foreign.load(Ordering::SeqCst) {
        "http://127.0.0.1:9"
    } else {
        ""
    };
    let members: Vec<Value> = (1..=bmc.systems.load(Ordering::SeqCst))
        .map(|i| json!({ "@odata.id": format!("{prefix}/redfish/v1/Systems/{i}") }))
        .collect();
    axum::Json(json!({ "Members": members })).into_response()
}

async fn system(
    State(bmc): State<Bmc>,
    UrlPath(_id): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(s) = bmc.check(&headers).await {
        return s.into_response();
    }
    let state = if bmc.off.load(Ordering::SeqCst) {
        "Off"
    } else {
        "On"
    };
    axum::Json(json!({ "PowerState": state })).into_response()
}

async fn reset(
    State(bmc): State<Bmc>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if let Err(s) = bmc.check(&headers).await {
        return s.into_response();
    }
    if bmc.refuse.load(Ordering::SeqCst) {
        return StatusCode::CONFLICT.into_response();
    }
    bmc.resets.lock().unwrap().push((id, body));
    bmc.off.store(true, Ordering::SeqCst);
    StatusCode::NO_CONTENT.into_response()
}

fn redfish(url: &str, password: &str, system_id: Option<&str>) -> FenceMethod {
    FenceMethod::Redfish(RedfishFence {
        url: format!("{url}/"),
        user: "admin".into(),
        password: Some(password.into()),
        system_id: system_id.map(Into::into),
        ca_pem: None,
        insecure: false,
    })
}

#[tokio::test]
async fn redfish_finds_the_only_system_resets_it_and_reads_the_power() {
    let (bmc, url) = Bmc::start(1).await;
    let fencer = Fencer::default();
    let m = redfish(&url, "pw", None);
    assert_eq!(fencer.status(&node(), &m).await, Ok(Power::On));
    let spec = NodeFenceSpec {
        methods: vec![m.clone()],
    };
    assert_eq!(
        power_off(&fencer, &node(), &spec).await,
        PowerOff::Off {
            method: FenceKind::Redfish
        }
    );
    assert_eq!(
        *bmc.resets.lock().unwrap(),
        [("1".to_string(), json!({ "ResetType": "ForceOff" }))]
    );
    assert_eq!(fencer.status(&node(), &m).await, Ok(Power::Off));

    // 틀린 비밀번호는 401이다. 오류에는 HTTP 상태만 담는다(BMC 주소 없이).
    let err = fencer
        .status(&node(), &redfish(&url, "wrong", None))
        .await
        .unwrap_err();
    assert_eq!(err, "GET returned 401 Unauthorized");
}

/// 닿지 않는 BMC는 "connection failed"만 남긴다(주소는 경고 로그에만).
#[tokio::test]
async fn an_unreachable_redfish_fails_without_its_address() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let err = Fencer::default()
        .status(&node(), &redfish(&url, "pw", Some("1")))
        .await
        .unwrap_err();
    assert_eq!(err, "GET connection failed");
}

/// 끄기 요청이 실패해도 전원이 이미 꺼져 있으면 그 방법으로 꺼진 것이다. 켜져 있으면 끄기의 오류로 실패한다.
#[tokio::test]
async fn a_refused_reset_of_a_node_that_is_already_off_counts_as_off() {
    let (bmc, url) = Bmc::start(1).await;
    bmc.refuse.store(true, Ordering::SeqCst);
    let fencer = Fencer::default();
    let spec = NodeFenceSpec {
        methods: vec![redfish(&url, "pw", Some("1"))],
    };
    let PowerOff::Failed(errors) =
        power_off_within(&fencer, &node(), &spec, Duration::from_secs(1)).await
    else {
        panic!("the power was on");
    };
    assert_eq!(errors, ["redfish: POST returned 409 Conflict"]);
    bmc.off.store(true, Ordering::SeqCst);
    assert_eq!(
        power_off(&fencer, &node(), &spec).await,
        PowerOff::Off {
            method: FenceKind::Redfish
        }
    );
    assert!(bmc.resets.lock().unwrap().is_empty());
}

/// `Systems`의 멤버가 `url`과 다른 출처면 따라가지 않는다(Basic 인증을 다른 곳으로 보내지 않게).
#[tokio::test]
async fn redfish_does_not_follow_a_system_on_another_host() {
    let (bmc, url) = Bmc::start(1).await;
    bmc.foreign.store(true, Ordering::SeqCst);
    let err = Fencer::default()
        .off(&node(), &redfish(&url, "pw", None))
        .await
        .unwrap_err();
    assert_eq!(err, "the BMC lists a system on another host");
    assert!(bmc.resets.lock().unwrap().is_empty());
}

#[tokio::test]
async fn redfish_needs_a_system_id_when_there_are_several_systems() {
    let (bmc, url) = Bmc::start(2).await;
    let fencer = Fencer::default();
    let err = fencer
        .off(&node(), &redfish(&url, "pw", None))
        .await
        .unwrap_err();
    assert_eq!(err, "the BMC lists 2 systems; set system_id");
    fencer
        .off(&node(), &redfish(&url, "pw", Some("2")))
        .await
        .unwrap();
    assert_eq!(
        *bmc.resets.lock().unwrap(),
        [("2".to_string(), json!({ "ResetType": "ForceOff" }))]
    );
}

#[tokio::test]
async fn a_slow_redfish_times_out() {
    let (bmc, url) = Bmc::start(1).await;
    bmc.slow.store(true, Ordering::SeqCst);
    let fencer = Fencer {
        timeout: Duration::from_millis(300),
        ..Fencer::default()
    };
    let started = std::time::Instant::now();
    let err = fencer
        .status(&node(), &redfish(&url, "pw", Some("1")))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3), "{err}");
    assert!(
        err.contains("timed out") || err.contains("timeout"),
        "{err}"
    );
}
