//! 노드 전원 끄기 펜싱의 실행기(설계 §8.1). `NodeFence`의 방법(IPMI·Redfish·명령)으로 노드를 끄고 전원을 읽는다.
//! 리더가 리스를 잃은 노드를 끌 때([`power_off`])와 `fence-test`·이웃 확인([`check`])이 쓴다. 오류는 모든 뷰어가 읽는
//! `fence_check`·`node-power-off` Task로 가므로 일반 이유만 담는다(BMC 주소·호스트·명령 출력 없이). 자세한 것은 경고
//! 로그에 남긴다.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_types::fence::{
    CommandFence, FenceKind, FenceMethod, FenceMethodStatus, IpmiFence, NodeFenceSpec, Power,
    RedfishFence,
};
use opl_types::node::{Node, NodeSpec};
use serde_json::{Value, json};
use uuid::Uuid;

/// 명령 하나·HTTP 요청 하나를 기다리는 최대 시간
pub const FENCE_CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// 끄기 뒤 꺼진 것을 확인하기까지 기다리는 최대 시간
pub const FENCE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);
/// 끄기 뒤 전원을 다시 읽는 간격
const CONFIRM_INTERVAL: Duration = Duration::from_secs(1);
/// 경고 로그에 남기는 실패한 명령 출력의 최대 길이
const OUTPUT_LIMIT: usize = 400;
/// 명령 출력을 받아 두는 최대 크기
const OUTPUT_CAPTURE: usize = 64 * 1024;
/// 명령이 끝난 뒤 출력의 끝(EOF)을 기다리는 시간. 출력 파이프를 쥔 채 남은 백그라운드 자식이 있으면 그때까지
/// 받은 것만 쓴다.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);
/// orbivisord에 `PATH`가 없을 때 명령 방법에 주는 `PATH`
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// 방법 하나로 노드를 끄고 전원을 읽는다.
#[async_trait]
pub trait PowerFence: Send + Sync {
    /// 끄기를 요청한다(꺼졌는지는 [`status`](Self::status)로 본다).
    async fn off(&self, node: &Node, method: &FenceMethod) -> Result<(), String>;
    async fn status(&self, node: &Node, method: &FenceMethod) -> Result<Power, String>;
}

/// 실제 실행기: `ipmitool`, Redfish(HTTP), `/bin/sh -c`
pub struct Fencer {
    /// `ipmitool` 실행 파일(opld `[ha] ipmitool`)
    pub ipmitool: PathBuf,
    /// 명령 하나·HTTP 요청 하나의 제한 시간
    pub timeout: Duration,
}

impl Default for Fencer {
    fn default() -> Self {
        Self {
            ipmitool: "ipmitool".into(),
            timeout: FENCE_CALL_TIMEOUT,
        }
    }
}

/// 끄기의 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerOff {
    /// 이 방법으로 꺼진 것을 확인했다.
    Off { method: FenceKind },
    /// 모든 방법이 실패했다(방법마다 `"<종류>: <이유>"`).
    Failed(Vec<String>),
}

/// `spec`의 방법을 차례로 해 본다: 끄기를 요청하고 1초마다 전원을 읽어 [`FENCE_CONFIRM_TIMEOUT`] 안에 꺼지면
/// 끝, 아니면 다음 방법.
pub async fn power_off(fence: &dyn PowerFence, node: &Node, spec: &NodeFenceSpec) -> PowerOff {
    power_off_within(fence, node, spec, FENCE_CONFIRM_TIMEOUT).await
}

/// [`power_off`]와 같되 꺼진 것을 `confirm`까지 기다린다(시험이 짧게 쓴다). 끄기 요청이 실패해도 전원을 한 번
/// 읽어 꺼져 있으면 그 방법으로 꺼진 것으로 본다(이미 꺼진 노드에 409를 주는 BMC, 꺼지며 끊긴 호스트).
pub async fn power_off_within(
    fence: &dyn PowerFence,
    node: &Node,
    spec: &NodeFenceSpec,
    confirm: Duration,
) -> PowerOff {
    let mut errors = Vec::new();
    for method in &spec.methods {
        let kind = method.kind().as_str();
        if let Err(e) = fence.off(node, method).await {
            if fence.status(node, method).await == Ok(Power::Off) {
                return PowerOff::Off {
                    method: method.kind(),
                };
            }
            errors.push(format!("{kind}: {e}"));
            continue;
        }
        match confirm_off(fence, node, method, confirm).await {
            Ok(()) => {
                return PowerOff::Off {
                    method: method.kind(),
                };
            }
            Err(e) => errors.push(format!("{kind}: {e}")),
        }
    }
    PowerOff::Failed(errors)
}

/// 꺼질 때까지 전원을 읽는다. 마지막으로 읽은 것을 오류에 담는다.
async fn confirm_off(
    fence: &dyn PowerFence,
    node: &Node,
    method: &FenceMethod,
    confirm: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + confirm;
    loop {
        let last = match fence.status(node, method).await {
            Ok(Power::Off) => return Ok(()),
            Ok(power) => format!("power is {}", power_str(power)),
            Err(e) => e,
        };
        if tokio::time::Instant::now() + CONFIRM_INTERVAL > deadline {
            return Err(format!(
                "power off not confirmed within {}s ({last})",
                confirm.as_secs()
            ));
        }
        tokio::time::sleep(CONFIRM_INTERVAL).await;
    }
}

fn power_str(power: Power) -> &'static str {
    match power {
        Power::On => "on",
        Power::Off => "off",
        Power::Unknown => "unknown",
    }
}

/// 방법마다 전원을 읽는다(함께, 방법 순서로). 전원은 건드리지 않는다.
pub async fn check(
    fence: &dyn PowerFence,
    node: &Node,
    spec: &NodeFenceSpec,
) -> Vec<FenceMethodStatus> {
    futures_util::future::join_all(spec.methods.iter().map(|method| async move {
        match fence.status(node, method).await {
            Ok(power) => FenceMethodStatus {
                kind: method.kind(),
                power,
                error: None,
            },
            Err(e) => FenceMethodStatus {
                kind: method.kind(),
                power: Power::Unknown,
                error: Some(e),
            },
        }
    }))
    .await
}

/// 다른 노드가 노드 사이 RPC(`FenceStatus`)로 부탁한 전원 읽기. 이 노드 저장소의 `NodeFence`를 쓴다.
pub struct FenceStatusSource {
    pub cluster: Arc<Cluster>,
    pub fence: Arc<dyn PowerFence>,
}

#[async_trait]
impl opl_peer::FenceSource for FenceStatusSource {
    async fn status(&self, node: Uuid) -> Result<Vec<FenceMethodStatus>, String> {
        let target = self
            .cluster
            .get::<NodeSpec>(node)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("node {node} not found"))?;
        let spec = self
            .cluster
            .get::<NodeFenceSpec>(node)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("node {:?} has no power fencing", target.name))?;
        Ok(check(self.fence.as_ref(), &target, &spec.spec).await)
    }
}

#[async_trait]
impl PowerFence for Fencer {
    async fn off(&self, node: &Node, method: &FenceMethod) -> Result<(), String> {
        let call = async {
            match method {
                FenceMethod::Ipmi(ipmi) => self.ipmi(ipmi, "off").await.map(drop),
                FenceMethod::Redfish(redfish) => self.redfish_off(redfish).await,
                FenceMethod::Command(command) => run_shell(node, &command.off).await.map(drop),
            }
        };
        self.limited(call).await
    }

    async fn status(&self, node: &Node, method: &FenceMethod) -> Result<Power, String> {
        let call = async {
            match method {
                FenceMethod::Ipmi(ipmi) => {
                    self.ipmi(ipmi, "status").await.map(|out| ipmi_power(&out))
                }
                FenceMethod::Redfish(redfish) => self.redfish_status(redfish).await,
                FenceMethod::Command(CommandFence { status, .. }) => {
                    run_shell(node, status).await.map(|out| command_power(&out))
                }
            }
        };
        self.limited(call).await
    }
}

impl Fencer {
    /// `call`을 제한 시간까지만 기다린다. 넘으면 버린다(명령은 `kill_on_drop`으로 끝난다).
    async fn limited<T>(&self, call: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        tokio::time::timeout(self.timeout, call)
            .await
            .unwrap_or_else(|_| Err(format!("timed out after {}s", self.timeout.as_secs())))
    }

    /// `ipmitool … chassis power <action>`의 표준 출력. 비밀번호는 `IPMI_PASSWORD` 환경 변수로 넘긴다.
    async fn ipmi(&self, ipmi: &IpmiFence, action: &str) -> Result<String, String> {
        let mut command = tokio::process::Command::new(&self.ipmitool);
        command.args(ipmi_args(ipmi, action)).env("LANG", "C");
        match &ipmi.password {
            Some(password) => command.env("IPMI_PASSWORD", password),
            None => command.env_remove("IPMI_PASSWORD"),
        };
        output(command, "ipmitool").await
    }

    /// Redfish 요청용 HTTP 클라이언트. `ca_pem`이 있으면 그 인증서만 루트로 믿고 `insecure`면 확인하지 않는다.
    /// 리다이렉트를 따르지 않는다(Basic 인증을 다른 곳으로 보내지 않게). BMC는 관리망에 있으므로 프록시 환경
    /// 변수(`HTTP(S)_PROXY`)를 따르지 않는다.
    fn redfish_client(&self, redfish: &RedfishFence) -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(pem) = &redfish.ca_pem {
            let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
                .map_err(|e| format!("ca_pem: {}", crate::telemetry::chain(&e)))?;
            builder = builder.tls_certs_only(certs);
        }
        if redfish.insecure {
            builder = builder.tls_danger_accept_invalid_certs(true);
        }
        builder.build().map_err(|e| crate::telemetry::chain(&e))
    }

    /// 시스템 리소스의 URL. `system_id`가 없으면 `Systems`의 멤버가 하나일 때 그것이다. 멤버는 `url`과 같은 출처
    /// (scheme·호스트·포트)여야 한다(Basic 인증을 다른 곳으로 보내지 않게).
    async fn redfish_system(
        &self,
        client: &reqwest::Client,
        redfish: &RedfishFence,
    ) -> Result<reqwest::Url, String> {
        let base = redfish.url.trim_end_matches('/');
        if let Some(id) = &redfish.system_id {
            return parse_url(&format!("{base}/redfish/v1/Systems/{id}"));
        }
        let systems = parse_url(&format!("{base}/redfish/v1/Systems"))?;
        let list =
            redfish_json(client, redfish, reqwest::Method::GET, systems.clone(), None).await?;
        let members = list["Members"].as_array().cloned().unwrap_or_default();
        let host = host_port(&systems);
        let [member] = members.as_slice() else {
            return Err(format!(
                "the BMC lists {} systems; set system_id",
                members.len()
            ));
        };
        let path = member["@odata.id"]
            .as_str()
            .ok_or_else(|| "the BMC lists a system without @odata.id".to_string())?;
        let system = systems.join(path).map_err(|e| {
            tracing::warn!(bmc = %host, path, error = %e, "the BMC lists a bad system path");
            "the BMC lists a bad system path".to_string()
        })?;
        if system.origin() != systems.origin() {
            tracing::warn!(bmc = %host, path, "the BMC lists a system on another host");
            return Err("the BMC lists a system on another host".into());
        }
        Ok(system)
    }

    async fn redfish_off(&self, redfish: &RedfishFence) -> Result<(), String> {
        let client = self.redfish_client(redfish)?;
        let system = self.redfish_system(&client, redfish).await?;
        let reset = parse_url(&format!(
            "{}/Actions/ComputerSystem.Reset",
            system.as_str().trim_end_matches('/')
        ))?;
        redfish_json(
            &client,
            redfish,
            reqwest::Method::POST,
            reset,
            Some(json!({ "ResetType": "ForceOff" })),
        )
        .await
        .map(drop)
    }

    async fn redfish_status(&self, redfish: &RedfishFence) -> Result<Power, String> {
        let client = self.redfish_client(redfish)?;
        let system = self.redfish_system(&client, redfish).await?;
        let body = redfish_json(&client, redfish, reqwest::Method::GET, system, None).await?;
        Ok(match body["PowerState"].as_str() {
            Some("Off") => Power::Off,
            Some("On" | "PoweringOn" | "PoweringOff" | "Paused") => Power::On,
            _ => Power::Unknown,
        })
    }
}

/// `ipmitool`의 인자(비밀번호는 넣지 않는다 — `-E`가 환경 변수에서 읽는다)
pub fn ipmi_args(ipmi: &IpmiFence, action: &str) -> Vec<String> {
    let mut args = Vec::new();
    if ipmi.lanplus {
        args.extend(["-I".into(), "lanplus".into()]);
    }
    args.extend([
        "-H".into(),
        ipmi.address.clone(),
        "-p".into(),
        ipmi.port.to_string(),
        "-U".into(),
        ipmi.user.clone(),
    ]);
    if ipmi.password.is_some() {
        args.push("-E".into());
    }
    args.extend(["chassis".into(), "power".into(), action.into()]);
    args
}

/// `ipmitool chassis power status`의 출력(`Chassis Power is on|off`)
fn ipmi_power(out: &str) -> Power {
    let out = out.to_ascii_lowercase();
    if out.contains("chassis power is off") {
        Power::Off
    } else if out.contains("chassis power is on") {
        Power::On
    } else {
        Power::Unknown
    }
}

/// `status` 명령 출력의 첫 줄(공백을 떼고 소문자)
fn command_power(out: &str) -> Power {
    match out
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "off" => Power::Off,
        "on" => Power::On,
        _ => Power::Unknown,
    }
}

/// `/bin/sh -c <script>`의 표준 출력. 환경 변수로 끌 노드를 알린다. orbivisord의 환경은 넘기지 않는다(`PATH`와
/// `ORBIVISOR_NODE_*`만). 스크립트가 비밀을 찍을 수 있으므로 실패의 출력은 오류에 담지 않고 로그에만 남긴다.
async fn run_shell(node: &Node, script: &str) -> Result<String, String> {
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| DEFAULT_PATH.into()),
        )
        .env("ORBIVISOR_NODE_NAME", &node.name)
        .env("ORBIVISOR_NODE_ID", node.id.to_string())
        .env("ORBIVISOR_NODE_ADDRESS", host_of(&node.spec.address));
    output(command, "command").await
}

/// 명령을 돌려 표준 출력을 돌려준다. 실패하면 종료 상태가 오류이고 stderr(없으면 stdout) 앞부분은 경고 로그에만
/// 남긴다(BMC 주소나 스크립트가 찍은 비밀을 담을 수 있다). 명령은 자기 프로세스 그룹에서 돌고, 끝나기 전에 버려지면(제한 시간)
/// 그룹을 통째로 죽인다(명령이 띄운 손자까지). 끝남은 명령 자신의 종료로 보고, 출력은 그 뒤
/// [`OUTPUT_GRACE`]까지만 기다린다(출력 파이프를 쥔 백그라운드 자식을 기다리지 않는다).
async fn output(mut command: tokio::process::Command, name: &str) -> Result<String, String> {
    use std::process::Stdio;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 제한 시간이 지나 버리면 명령도 끝낸다.
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    let mut group = GroupKill(child.id());
    let (stdout, stderr) = (Captured::default(), Captured::default());
    // 버려지면 읽기도 멈춘다.
    let mut readers = tokio::task::JoinSet::new();
    if let Some(pipe) = child.stdout.take() {
        readers.spawn(stdout.clone().drain(pipe));
    }
    if let Some(pipe) = child.stderr.take() {
        readers.spawn(stderr.clone().drain(pipe));
    }
    let status = child
        .wait()
        .await
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    // 명령은 끝났다. 남은 백그라운드 자식은 그대로 둔다.
    group.0 = None;
    let _ = tokio::time::timeout(OUTPUT_GRACE, async {
        while readers.join_next().await.is_some() {}
    })
    .await;
    let stdout = stdout.text();
    if !status.success() {
        let stderr = stderr.text();
        let text = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        let text: String = text.chars().take(OUTPUT_LIMIT).collect();
        tracing::warn!(command = name, %status, output = %text, "power fencing command failed");
        return Err(format!("{name} failed ({status})"));
    }
    Ok(stdout)
}

/// 명령의 프로세스 그룹. 버려질 때 그룹이 남아 있으면(`Some`) 통째로 `SIGKILL`한다.
struct GroupKill(Option<u32>);

impl Drop for GroupKill {
    fn drop(&mut self) {
        if let Some(pgid) = self.0.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
            // SAFETY: 메모리를 건드리지 않는 시스템 호출이다. 그룹이 이미 없으면 ESRCH로 끝난다.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

/// 명령 출력을 [`OUTPUT_CAPTURE`]까지 받아 둔다(그 뒤는 읽어 버린다).
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    async fn drain(self, mut pipe: impl tokio::io::AsyncRead + Unpin) {
        use tokio::io::AsyncReadExt;

        let mut chunk = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let mut buf = self.0.lock().unwrap();
            let room = OUTPUT_CAPTURE.saturating_sub(buf.len()).min(n);
            buf.extend_from_slice(&chunk[..room]);
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// `host:port`·`[v6]:port`의 호스트
fn host_of(address: &str) -> &str {
    if let Some(rest) = address.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match address.rsplit_once(':') {
        Some((host, _)) if !host.contains(':') => host,
        _ => address,
    }
}

fn parse_url(url: &str) -> Result<reqwest::Url, String> {
    reqwest::Url::parse(url).map_err(|e| format!("invalid url: {e}"))
}

/// 경고 로그에 쓰는 URL의 `호스트[:포트]`(경로·사용자 정보는 빼고)
fn host_port(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or("");
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

/// Redfish 요청 하나(Basic 인증). 2xx가 아니면 오류. 본문이 비었거나 JSON이 아니면 null이다. 오류에는 HTTP 상태나
/// "connection failed"·"timed out"만 담고 호스트와 자세한 이유는 경고 로그에 남긴다.
async fn redfish_json(
    client: &reqwest::Client,
    redfish: &RedfishFence,
    method: reqwest::Method,
    url: reqwest::Url,
    body: Option<Value>,
) -> Result<Value, String> {
    let mut request = client
        .request(method.clone(), url.clone())
        .basic_auth(&redfish.user, redfish.password.as_deref())
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.json(&body);
    }
    let host = host_port(&url);
    let failed = |e: reqwest::Error| {
        let why = if e.is_timeout() {
            "timed out"
        } else {
            "connection failed"
        };
        tracing::warn!(bmc = %host, %method, error = %crate::telemetry::chain(&e.without_url()), "redfish request failed");
        format!("{method} {why}")
    };
    let response = request.send().await.map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(bmc = %host, %method, %status, "redfish request refused");
        return Err(format!("{method} returned {status}"));
    }
    let bytes = response.bytes().await.map_err(failed)?;
    Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipmi(password: Option<&str>, lanplus: bool) -> IpmiFence {
        IpmiFence {
            address: "10.0.0.9".into(),
            port: 6230,
            user: "admin".into(),
            password: password.map(Into::into),
            lanplus,
        }
    }

    #[test]
    fn ipmi_args_never_carry_the_password() {
        assert_eq!(
            ipmi_args(&ipmi(Some("s3cret"), true), "off"),
            [
                "-I", "lanplus", "-H", "10.0.0.9", "-p", "6230", "-U", "admin", "-E", "chassis",
                "power", "off"
            ]
        );
        // IPMI 1.5에 비밀번호 없음
        assert_eq!(
            ipmi_args(&ipmi(None, false), "status"),
            [
                "-H", "10.0.0.9", "-p", "6230", "-U", "admin", "chassis", "power", "status"
            ]
        );
    }

    #[test]
    fn reads_power_from_outputs() {
        assert_eq!(ipmi_power("Chassis Power is off\n"), Power::Off);
        assert_eq!(ipmi_power("Chassis Power is on\n"), Power::On);
        assert_eq!(
            ipmi_power("Error: Unable to establish IPMI v2 session"),
            Power::Unknown
        );
        assert_eq!(command_power(" OFF \nmore"), Power::Off);
        assert_eq!(command_power("on"), Power::On);
        assert_eq!(command_power("standby"), Power::Unknown);
        assert_eq!(command_power(""), Power::Unknown);
    }

    #[test]
    fn host_of_strips_the_port() {
        assert_eq!(host_of("10.0.0.1:8006"), "10.0.0.1");
        assert_eq!(host_of("node-a:8006"), "node-a");
        assert_eq!(host_of("[fd00::1]:8006"), "fd00::1");
        assert_eq!(host_of("node-a"), "node-a");
        assert_eq!(host_of("fd00::1"), "fd00::1");
    }
}
