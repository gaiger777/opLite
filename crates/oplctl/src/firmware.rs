//! vTPM·Secure Boot·게스트 에이전트(계획 59): `vm create|set`의 `--tpm`·`--secure-boot`·`--guest-agent`·`--preset`,
//! `vm firmware reset`, `vm guest [password]`, `vm get`·`node get`의 요약 줄.

use std::io::{BufRead, Write};

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::cli::{Switch, ago, items, name_of, now_secs, unix_time};
use crate::client::{Client, seg};
use crate::output::{bytes, cell, json as print_json, table};

/// 게스트 에이전트의 종료 방법(`guest_agent.shutdown`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentShutdownArg {
    /// 에이전트의 `guest-shutdown`(답하지 않으면 ACPI)
    Agent,
    /// 늘 ACPI 전원 단추
    Acpi,
}

/// `vm create|set`의 펌웨어·게스트 에이전트 값. 주지 않은 값은 지금 값(새 VM은 끔)이다.
#[derive(Debug, Default, Clone, PartialEq, Eq, Args)]
pub(crate) struct FirmwareArgs {
    /// TPM 2.0(노드의 swtpm). UEFI VM만 되고, 실행 중이면 다음 재시작 때 적용한다. 꺼도 TPM 상태는 남는다.
    #[arg(long, value_enum, value_name = "on|off")]
    pub(crate) tpm: Option<Switch>,
    /// Secure Boot(Microsoft 키를 넣은 UEFI 변수). UEFI VM만 되고 VM이 꺼져 있어야 한다. 이미 UEFI 변수가 있는
    /// VM은 `vm set`의 `--reset-uefi-vars`와 함께 준다.
    #[arg(long, value_enum, value_name = "on|off")]
    pub(crate) secure_boot: Option<Switch>,
    /// QEMU 게스트 에이전트 채널(게스트에 qemu-guest-agent가 있어야 함): 게스트 정보, 에이전트로 끄기, 스냅샷·백업의
    /// fs-freeze. 실행 중이면 채널은 다음 재시작 때 붙는다.
    #[arg(long, value_enum, value_name = "on|off")]
    pub(crate) guest_agent: Option<Switch>,
    /// 디스크 스냅샷·백업·복제 때 게스트 파일 시스템을 얼린다(기본 on).
    #[arg(long, value_enum, value_name = "on|off")]
    pub(crate) agent_freeze: Option<Switch>,
    /// VM을 끌 때 쓰는 길(기본 agent: 에이전트가 답하지 않으면 ACPI).
    #[arg(long, value_enum)]
    pub(crate) agent_shutdown: Option<AgentShutdownArg>,
}

impl FirmwareArgs {
    /// Secure Boot 말고 바꾸는 것이 있는지
    pub(crate) fn changes_other(&self) -> bool {
        self.tpm.is_some()
            || self.guest_agent.is_some()
            || self.agent_freeze.is_some()
            || self.agent_shutdown.is_some()
    }
}

/// `vm create --preset`
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Preset {
    /// q35·UEFI·Secure Boot·TPM 2.0·게스트 에이전트, vCPU 2, 4096 MiB, SATA 디스크, e1000e NIC(드라이버 없이 설치)
    Windows11,
}

/// Windows 11 미리 채우기의 vCPU(소켓 1 × 코어 2)와 메모리(MiB)
pub(crate) const WINDOWS11_CORES: u16 = 2;
pub(crate) const WINDOWS11_MEMORY: u32 = 4096;

/// `--preset windows11`을 만든 VM에 알릴 줄(stderr)
pub(crate) const WINDOWS11_NOTE: &str = "note: the windows11 preset uses SATA disks and an e1000e NIC so Windows \
     installs without drivers; switch them to VirtIO after installing the VirtIO drivers";

/// `--preset`과 함께 주면 안 되는 머신·펌웨어인지 본다(Windows 11은 q35 UEFI만).
pub(crate) fn check_preset(
    preset: Preset,
    q35: Option<bool>,
    uefi: Option<bool>,
) -> anyhow::Result<()> {
    let Preset::Windows11 = preset;
    if q35 == Some(false) {
        bail!("--preset windows11 needs --machine q35");
    }
    if uefi == Some(false) {
        bail!("--preset windows11 needs UEFI firmware");
    }
    Ok(())
}

/// 미리 채우기를 새 VM 스펙에 넣는다: 디스크는 SATA, NIC는 e1000e, TPM·Secure Boot·게스트 에이전트를 켠다. 그 뒤
/// [`firmware_update`]가 준 값(`--tpm off` 등)으로 덮는다. 머신·펌웨어·CPU·메모리는 부르는 쪽이 정한다.
pub(crate) fn apply_preset(spec: &mut Value, preset: Preset) {
    let Preset::Windows11 = preset;
    for disk in spec["disks"].as_array_mut().into_iter().flatten() {
        disk["bus"] = json!("sata");
    }
    for nic in spec["nics"].as_array_mut().into_iter().flatten() {
        nic["model"] = json!("e1000e");
    }
    spec["tpm"] = json!(true);
    spec["secure_boot"] = json!(true);
    spec["guest_agent"] = json!({});
}

/// 스펙(`spec`, VM의 `spec` 객체)에 준 값만 넣는다. `skip_secure_boot`면 Secure Boot는 두고(초기화 요청이 바꾼다).
/// 끄는 값은 필드를 뺀다(서버 기본값).
pub(crate) fn firmware_update(
    spec: &mut Value,
    args: &FirmwareArgs,
    skip_secure_boot: bool,
) -> anyhow::Result<()> {
    let set = |spec: &mut Value, field: &str, on: Switch| match on {
        Switch::On => spec[field] = json!(true),
        Switch::Off => {
            if let Some(spec) = spec.as_object_mut() {
                spec.remove(field);
            }
        }
    };
    if let Some(on) = args.tpm {
        set(spec, "tpm", on);
    }
    if let Some(on) = args.secure_boot
        && !skip_secure_boot
    {
        set(spec, "secure_boot", on);
    }
    match args.guest_agent {
        Some(Switch::Off) => {
            if args.agent_freeze.is_some() || args.agent_shutdown.is_some() {
                bail!(
                    "--agent-freeze and --agent-shutdown need the guest agent; drop --guest-agent off"
                );
            }
            if let Some(spec) = spec.as_object_mut() {
                spec.remove("guest_agent");
            }
        }
        Some(Switch::On) if !spec["guest_agent"].is_object() => spec["guest_agent"] = json!({}),
        _ => {}
    }
    if args.agent_freeze.is_some() || args.agent_shutdown.is_some() {
        let agent = &mut spec["guest_agent"];
        if !agent.is_object() {
            bail!("the VM has no guest agent; add --guest-agent on");
        }
        if let Some(on) = args.agent_freeze {
            agent["freeze_fs"] = json!(on == Switch::On);
        }
        if let Some(how) = args.agent_shutdown {
            agent["shutdown"] = json!(match how {
                AgentShutdownArg::Agent => "agent",
                AgentShutdownArg::Acpi => "acpi",
            });
        }
    }
    Ok(())
}

/// `vm set`이 알릴 펌웨어 변경: `tpm on, secure boot off, guest agent on`. 바꾼 것이 없으면 None이다.
pub(crate) fn firmware_done(args: &FirmwareArgs, skip_secure_boot: bool) -> Option<String> {
    let word = |on: Switch| if on == Switch::On { "on" } else { "off" };
    let mut parts = Vec::new();
    if let Some(on) = args.tpm {
        parts.push(format!("tpm {}", word(on)));
    }
    if let Some(on) = args.secure_boot
        && !skip_secure_boot
    {
        parts.push(format!("secure boot {}", word(on)));
    }
    match args.guest_agent {
        Some(on) => parts.push(format!("guest agent {}", word(on))),
        None if args.agent_freeze.is_some() || args.agent_shutdown.is_some() => {
            parts.push("guest agent updated".into())
        }
        None => {}
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// `vm set --secure-boot on|off --reset-uefi-vars`: 변수 초기화 Task를 만들며 Secure Boot를 바꾸고(같은 요청) 끝나기를
/// 기다린다. 부팅 항목이 지워진다.
pub(crate) async fn reset_with_secure_boot(
    client: &Client,
    vm: &str,
    on: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let body = json!({ "uefi_vars": true, "secure_boot": on });
    let task = client
        .post(&format!("/vms/{}/firmware/reset", seg(vm)), &body)
        .await?;
    client.wait_task(&task).await?;
    let state = if on { "on" } else { "off" };
    writeln!(out, "vm {vm}: UEFI variables reset, Secure Boot {state}")?;
    Ok(())
}

/// `vm firmware`
#[derive(Subcommand)]
pub(crate) enum FirmwareCmd {
    /// 꺼진 VM의 UEFI 변수(부팅 항목)나 TPM 상태를 지운다. 다음 시작 때 새로 만든다. TPM을 지우면 TPM에 봉인한
    /// 비밀(BitLocker 키 등)을 잃는다.
    #[command(group(ArgGroup::new("what").required(true).multiple(true).args(["uefi_vars", "tpm"])))]
    Reset {
        vm: String,
        /// UEFI 변수를 지운다(부팅 항목을 잃는다).
        #[arg(long)]
        uefi_vars: bool,
        /// TPM 상태를 지운다(새 TPM이 된다).
        #[arg(long)]
        tpm: bool,
        /// `--tpm`을 묻지 않고 한다(터미널이 아니면 필요하다).
        #[arg(long)]
        yes: bool,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
}

/// TPM 초기화를 묻는 말
fn tpm_question(vm: &str) -> String {
    format!(
        "resetting the TPM of VM {vm} gives it a new TPM: secrets sealed to the old one (BitLocker keys, \
         disk encryption, credentials) are lost and the guest may ask for a recovery key.\n\
         type the VM name to confirm: "
    )
}

/// 친 답이 VM 이름인지
fn confirmed(vm: &str, answer: &str) -> bool {
    answer.trim() == vm
}

/// 초기화한 것: `uefi vars, tpm`
fn reset_parts(uefi_vars: bool, tpm: bool) -> String {
    [(uefi_vars, "uefi vars"), (tpm, "tpm")]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) async fn firmware(
    client: &Client,
    cmd: FirmwareCmd,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let FirmwareCmd::Reset {
        vm,
        uefi_vars,
        tpm,
        yes,
        no_wait,
    } = cmd;
    if tpm && !yes {
        use std::io::IsTerminal;
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            bail!(
                "--tpm gives VM {vm} a new TPM and loses the secrets sealed to it; pass --yes to confirm it"
            );
        }
        write!(err, "{}", tpm_question(&vm))?;
        err.flush()?;
        let mut answer = String::new();
        stdin.lock().read_line(&mut answer)?;
        if !confirmed(&vm, &answer) {
            bail!("cancelled");
        }
    }
    let body = json!({ "uefi_vars": uefi_vars, "tpm": tpm });
    let task = client
        .post(&format!("/vms/{}/firmware/reset", seg(&vm)), &body)
        .await?;
    let parts = reset_parts(uefi_vars, tpm);
    if no_wait {
        let id = task["id"].as_str().context("response has no task id")?;
        writeln!(out, "vm {vm}: firmware reset task {id} started ({parts})")?;
        return Ok(());
    }
    client.wait_task(&task).await?;
    writeln!(out, "vm {vm}: firmware reset ({parts})")?;
    Ok(())
}

/// `vm guest`: 게스트 에이전트가 알린 정보, 또는 `vm guest password`
#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub(crate) struct GuestArgs {
    #[command(subcommand)]
    pub(crate) cmd: Option<GuestCmd>,
    /// VM 이름이나 id
    #[arg(required = true)]
    pub(crate) vm: Option<String>,
    /// 모아 둔 정보(60초마다) 대신 지금 묻는다(VM마다 5초에 한 번).
    #[arg(long)]
    pub(crate) refresh: bool,
}

#[derive(Subcommand)]
pub(crate) enum GuestCmd {
    /// 게스트 사용자의 비밀번호를 게스트 에이전트로 바꾼다. 비밀번호는 표준 입력(터미널이면 두 번 묻기)으로 받는다.
    Password {
        vm: String,
        /// 게스트 사용자 이름(Windows는 `Administrator` 등)
        #[arg(long)]
        user: String,
    },
}

/// `state`가 `answered`가 아닐 때의 줄
fn guest_state_line(vm: &str, state: &str) -> String {
    let why = match state {
        "no_agent" => {
            "the guest agent has not answered (turn on --guest-agent and install qemu-guest-agent in the guest)"
        }
        "not_running" => "the VM is not running",
        "unreachable" => "the node running the VM did not answer",
        "unsupported" => "the node running the VM cannot read the guest agent (upgrade it)",
        other => other,
    };
    format!("vm {vm}: {why}")
}

/// `vm guest`의 요약 줄: OS, 호스트 이름, 에이전트 버전, 모은 시각
fn guest_lines(info: &Value, now: u64) -> Vec<String> {
    let os = &info["os"];
    let mut lines = Vec::new();
    if !os.is_null() {
        let name = os["name"]
            .as_str()
            .or(os["id"].as_str())
            .unwrap_or("-")
            .to_string();
        let mut line = format!("os: {name}");
        if let Some(kernel) = os["kernel"].as_str() {
            line.push_str(&format!(" (kernel {kernel})"));
        }
        lines.push(line);
    }
    lines.push(format!("hostname: {}", cell(&info["hostname"])));
    lines.push(format!("agent: {}", cell(&info["agent_version"])));
    if let Some(at) = info["collected_at"].as_u64() {
        lines.push(format!(
            "collected: {} ({} ago)",
            unix_time(at as i64),
            ago(now.saturating_sub(at))
        ));
    }
    lines
}

/// NIC 표의 열
const GUEST_NIC_HEADERS: &[&str] = &["NIC", "MAC", "ADDRESSES"];

fn guest_nic_rows(info: &Value) -> Vec<Vec<String>> {
    items_of(&info["interfaces"])
        .iter()
        .map(|n| {
            let addresses: Vec<String> = items_of(&n["addresses"])
                .iter()
                .map(|a| format!("{}/{}", cell(&a["ip"]), cell(&a["prefix"])))
                .collect();
            vec![
                cell(&n["name"]),
                cell(&n["mac"]),
                if addresses.is_empty() {
                    "-".into()
                } else {
                    addresses.join(", ")
                },
            ]
        })
        .collect()
}

/// 파일 시스템 표의 열
const GUEST_FS_HEADERS: &[&str] = &["MOUNT", "TYPE", "USED", "SIZE"];

fn guest_fs_rows(info: &Value) -> Vec<Vec<String>> {
    items_of(&info["filesystems"])
        .iter()
        .map(|f| {
            vec![
                cell(&f["mountpoint"]),
                cell(&f["fstype"]),
                bytes(&f["used_bytes"]),
                bytes(&f["total_bytes"]),
            ]
        })
        .collect()
}

fn items_of(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// 비밀번호를 읽는다: 터미널이면 두 번 묻고(같아야 함), 아니면 표준 입력의 첫 줄이다. 화면에 보이지 않는다.
fn read_guest_password(user: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let first = rpassword::prompt_password(format!("new password for {user}: "))?;
        let again = rpassword::prompt_password("again: ")?;
        if first != again {
            bail!("the passwords do not match");
        }
        return Ok(first);
    }
    first_line(&mut stdin.lock())
}

/// 표준 입력의 첫 줄(줄 끝은 뺌)
fn first_line(input: &mut dyn BufRead) -> anyhow::Result<String> {
    let mut line = String::new();
    input
        .read_line(&mut line)
        .context("cannot read the password from standard input")?;
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    if line.is_empty() {
        bail!("no password on standard input");
    }
    Ok(line)
}

pub(crate) async fn guest(
    client: &Client,
    args: GuestArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if let Some(GuestCmd::Password { vm, user }) = args.cmd {
        let password = read_guest_password(&user)?;
        return guest_password(client, &vm, &user, password, out).await;
    }
    let vm = args.vm.context("give a VM")?;
    let path = if args.refresh {
        format!("/vms/{}/guest?refresh=true", seg(&vm))
    } else {
        format!("/vms/{}/guest", seg(&vm))
    };
    let resp = client.get(&path).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let state = resp["state"].as_str().unwrap_or("-");
    if state != "answered" {
        writeln!(out, "{}", guest_state_line(&vm, state))?;
        return Ok(());
    }
    let info = &resp["info"];
    for line in guest_lines(info, now_secs()) {
        writeln!(out, "{line}")?;
    }
    let nics = guest_nic_rows(info);
    if !nics.is_empty() {
        writeln!(out)?;
        table(out, GUEST_NIC_HEADERS, &nics)?;
    }
    let fs = guest_fs_rows(info);
    if !fs.is_empty() {
        writeln!(out)?;
        table(out, GUEST_FS_HEADERS, &fs)?;
    }
    Ok(())
}

/// 비밀번호 바꾸기 요청. 비밀번호는 본문으로만 보내고 출력하지 않는다.
pub(crate) async fn guest_password(
    client: &Client,
    vm: &str,
    user: &str,
    password: String,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let body = json!({ "user": user, "password": password });
    client
        .post(&format!("/vms/{}/guest/password", seg(vm)), &body)
        .await?;
    writeln!(out, "vm {vm}: password of guest user {user} set")?;
    Ok(())
}

/// `vm get`의 펌웨어·게스트 에이전트 줄(stderr): `tpm: on (state on node a, copies: b)`, `secure boot: on`,
/// `guest agent: on (freeze fs, shutdown agent)`. `nodes`는 `/nodes`의 항목이다.
pub(crate) fn firmware_lines(vm: &Value, nodes: &[Value]) -> Vec<String> {
    let (spec, status) = (&vm["spec"], &vm["status"]);
    let mut lines = Vec::new();
    let tpm = spec["tpm"] == true;
    let state = status["tpm_sha256"].is_string();
    if tpm || state {
        let mut line = format!("tpm: {}", if tpm { "on" } else { "off" });
        if state {
            let holder = if status["firmware_node"].is_null() {
                "-".to_string()
            } else {
                name_of(nodes, &status["firmware_node"])
            };
            let copies: Vec<String> = items_of(&status["firmware_copies"])
                .iter()
                .map(|n| name_of(nodes, n))
                .collect();
            let kept = if tpm { "state" } else { "state kept" };
            line.push_str(&format!(" ({kept} on node {holder}"));
            if !copies.is_empty() {
                line.push_str(&format!(", copies: {}", copies.join(", ")));
            }
            line.push(')');
        } else {
            line.push_str(" (no state yet; made on first start)");
        }
        lines.push(line);
    }
    if spec["secure_boot"] == true {
        lines.push("secure boot: on".into());
    }
    let agent = &spec["guest_agent"];
    if agent.is_object() {
        let freeze = if agent["freeze_fs"] == false {
            "no fs freeze"
        } else {
            "freeze fs"
        };
        let shutdown = agent["shutdown"].as_str().unwrap_or("agent");
        lines.push(format!("guest agent: on ({freeze}, shutdown {shutdown})"));
    }
    lines
}

/// `vm get`의 줄에 노드 이름이 필요한지(TPM 상태가 있음)
pub(crate) fn needs_nodes(vm: &Value) -> bool {
    vm["status"]["tpm_sha256"].is_string()
}

/// `node get`의 줄(stderr): `firmware: tpm yes, secure boot no`. 옛 노드·위트니스는 없다.
pub(crate) fn node_firmware_line(node: &Value) -> Option<String> {
    let caps = &node["status"]["firmware_caps"];
    if !caps.is_object() {
        return None;
    }
    let yes = |v: &Value| if *v == true { "yes" } else { "no" };
    Some(format!(
        "firmware: tpm {}, secure boot {}",
        yes(&caps["tpm"]),
        yes(&caps["secure_boot"])
    ))
}

/// `node list`의 TPM·SB 열(능력을 알리지 않은 노드는 `-`)
pub(crate) fn caps_cell(node: &Value, cap: &str) -> String {
    cell(&node["status"]["firmware_caps"][cap])
}

/// `vm get` 줄을 쓴다. 노드 이름은 TPM 상태가 있을 때만 읽는다(읽지 못하면 id).
pub(crate) async fn write_firmware_lines(
    client: &Client,
    vm: &Value,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let nodes = if needs_nodes(vm) {
        match client.get("/nodes").await {
            Ok(list) => items(&list),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };
    for line in firmware_lines(vm, &nodes) {
        writeln!(err, "{line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(f: impl FnOnce(&mut FirmwareArgs)) -> FirmwareArgs {
        let mut a = FirmwareArgs::default();
        f(&mut a);
        a
    }

    #[test]
    fn firmware_update_sets_only_given_values() {
        let mut spec = json!({ "memory_mib": 256 });
        firmware_update(
            &mut spec,
            &args(|a| {
                a.tpm = Some(Switch::On);
                a.secure_boot = Some(Switch::On);
                a.guest_agent = Some(Switch::On);
            }),
            false,
        )
        .unwrap();
        assert_eq!(
            spec,
            json!({ "memory_mib": 256, "tpm": true, "secure_boot": true, "guest_agent": {} })
        );
        // 이미 있는 에이전트 설정은 `--guest-agent on`이 지우지 않는다.
        spec["guest_agent"] = json!({ "freeze_fs": false });
        firmware_update(
            &mut spec,
            &args(|a| a.guest_agent = Some(Switch::On)),
            false,
        )
        .unwrap();
        assert_eq!(spec["guest_agent"], json!({ "freeze_fs": false }));
        firmware_update(
            &mut spec,
            &args(|a| {
                a.agent_freeze = Some(Switch::On);
                a.agent_shutdown = Some(AgentShutdownArg::Acpi);
            }),
            false,
        )
        .unwrap();
        assert_eq!(
            spec["guest_agent"],
            json!({ "freeze_fs": true, "shutdown": "acpi" })
        );
        // 끄면 필드를 뺀다. Secure Boot는 초기화 요청이 바꿀 때 둔다.
        firmware_update(
            &mut spec,
            &args(|a| {
                a.tpm = Some(Switch::Off);
                a.secure_boot = Some(Switch::Off);
                a.guest_agent = Some(Switch::Off);
            }),
            true,
        )
        .unwrap();
        assert_eq!(spec, json!({ "memory_mib": 256, "secure_boot": true }));
    }

    #[test]
    fn firmware_done_lists_the_changes() {
        let a = args(|a| {
            a.tpm = Some(Switch::On);
            a.secure_boot = Some(Switch::Off);
            a.guest_agent = Some(Switch::On);
        });
        assert_eq!(
            firmware_done(&a, false).unwrap(),
            "tpm on, secure boot off, guest agent on"
        );
        assert_eq!(firmware_done(&a, true).unwrap(), "tpm on, guest agent on");
        let a = args(|a| a.agent_freeze = Some(Switch::Off));
        assert_eq!(firmware_done(&a, false).unwrap(), "guest agent updated");
        assert_eq!(firmware_done(&FirmwareArgs::default(), false), None);
    }

    #[test]
    fn agent_options_need_the_agent() {
        let mut spec = json!({});
        let e = firmware_update(
            &mut spec,
            &args(|a| a.agent_freeze = Some(Switch::Off)),
            false,
        )
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "the VM has no guest agent; add --guest-agent on"
        );
        let e = firmware_update(
            &mut spec,
            &args(|a| {
                a.guest_agent = Some(Switch::Off);
                a.agent_shutdown = Some(AgentShutdownArg::Agent);
            }),
            false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("need the guest agent"), "{e}");
        // 켜면서 주면 된다.
        firmware_update(
            &mut spec,
            &args(|a| {
                a.guest_agent = Some(Switch::On);
                a.agent_freeze = Some(Switch::Off);
            }),
            false,
        )
        .unwrap();
        assert_eq!(spec, json!({ "guest_agent": { "freeze_fs": false } }));
    }

    #[test]
    fn windows11_preset_fills_the_spec() {
        let mut spec = json!({
            "disks": [{ "volume": "win", "bus": "virtio-blk", "boot_order": 1 }],
            "nics": [{ "network": "lan", "model": "virtio" }],
        });
        apply_preset(&mut spec, Preset::Windows11);
        assert_eq!(
            spec,
            json!({
                "disks": [{ "volume": "win", "bus": "sata", "boot_order": 1 }],
                "nics": [{ "network": "lan", "model": "e1000e" }],
                "tpm": true, "secure_boot": true, "guest_agent": {},
            })
        );
        // 준 값이 미리 채운 값을 덮는다.
        firmware_update(&mut spec, &args(|a| a.tpm = Some(Switch::Off)), false).unwrap();
        assert_eq!(spec["tpm"], Value::Null);
        check_preset(Preset::Windows11, None, None).unwrap();
        check_preset(Preset::Windows11, Some(true), Some(true)).unwrap();
        assert_eq!(
            check_preset(Preset::Windows11, Some(false), None)
                .unwrap_err()
                .to_string(),
            "--preset windows11 needs --machine q35"
        );
        assert!(check_preset(Preset::Windows11, None, Some(false)).is_err());
    }

    #[test]
    fn firmware_lines_show_tpm_secure_boot_and_agent() {
        let nodes = vec![
            json!({ "id": "id-a", "name": "a" }),
            json!({ "id": "id-b", "name": "b" }),
        ];
        let vm = json!({
            "spec": { "tpm": true, "secure_boot": true, "guest_agent": {} },
            "status": { "tpm_sha256": "ab", "firmware_node": "id-a", "firmware_copies": ["id-b"] },
        });
        assert!(needs_nodes(&vm));
        assert_eq!(
            firmware_lines(&vm, &nodes),
            [
                "tpm: on (state on node a, copies: b)",
                "secure boot: on",
                "guest agent: on (freeze fs, shutdown agent)",
            ]
        );
        let vm = json!({
            "spec": { "tpm": true, "guest_agent": { "freeze_fs": false, "shutdown": "acpi" } },
            "status": {},
        });
        assert!(!needs_nodes(&vm));
        assert_eq!(
            firmware_lines(&vm, &[]),
            [
                "tpm: on (no state yet; made on first start)",
                "guest agent: on (no fs freeze, shutdown acpi)",
            ]
        );
        // TPM을 뺐어도 상태는 남는다.
        let vm = json!({ "spec": {}, "status": { "tpm_sha256": "ab", "firmware_node": "id-a" } });
        assert_eq!(
            firmware_lines(&vm, &nodes),
            ["tpm: off (state kept on node a)"]
        );
        assert!(firmware_lines(&json!({ "spec": {}, "status": {} }), &[]).is_empty());
    }

    #[test]
    fn node_firmware_caps() {
        let n = json!({ "status": { "firmware_caps": { "tpm": true, "secure_boot": false } } });
        assert_eq!(
            node_firmware_line(&n).unwrap(),
            "firmware: tpm yes, secure boot no"
        );
        assert_eq!(caps_cell(&n, "tpm"), "true");
        assert_eq!(caps_cell(&n, "secure_boot"), "false");
        let old = json!({ "status": {} });
        assert_eq!(node_firmware_line(&old), None);
        assert_eq!(caps_cell(&old, "tpm"), "-");
    }

    #[test]
    fn reset_confirmation_and_parts() {
        assert!(confirmed("win", "win\n"));
        assert!(confirmed("win", "  win "));
        for bad in ["", "\n", "y", "yes", "win2"] {
            assert!(!confirmed("win", bad), "{bad:?}");
        }
        assert!(tpm_question("win").ends_with("type the VM name to confirm: "));
        assert_eq!(reset_parts(true, true), "uefi vars, tpm");
        assert_eq!(reset_parts(false, true), "tpm");
        assert_eq!(reset_parts(true, false), "uefi vars");
    }

    #[test]
    fn guest_info_is_shown_as_lines_and_tables() {
        let info = json!({
            "collected_at": 1000,
            "agent_version": "8.2.2",
            "os": { "id": "ubuntu", "name": "Ubuntu 24.04.1 LTS", "kernel": "6.8.0-45-generic" },
            "hostname": "web",
            "interfaces": [
                { "name": "lo", "addresses": [{ "ip": "127.0.0.1", "prefix": 8 }] },
                { "name": "eth0", "mac": "52:54:00:12:34:56", "addresses": [
                    { "ip": "10.0.2.15", "prefix": 24 }, { "ip": "fec0::5054:ff:fe12:3456", "prefix": 64 },
                ] },
                { "name": "eth1", "addresses": [] },
            ],
            "filesystems": [{ "mountpoint": "/", "fstype": "ext4", "used_bytes": 1073741824u64,
                              "total_bytes": 4294967296u64 }],
        });
        assert_eq!(
            guest_lines(&info, 1045),
            [
                "os: Ubuntu 24.04.1 LTS (kernel 6.8.0-45-generic)",
                "hostname: web",
                "agent: 8.2.2",
                "collected: 1970-01-01 00:16 UTC (45s ago)",
            ]
        );
        assert_eq!(
            guest_nic_rows(&info),
            [
                vec!["lo", "-", "127.0.0.1/8"],
                vec![
                    "eth0",
                    "52:54:00:12:34:56",
                    "10.0.2.15/24, fec0::5054:ff:fe12:3456/64"
                ],
                vec!["eth1", "-", "-"],
            ]
        );
        assert_eq!(
            guest_fs_rows(&info),
            [vec!["/", "ext4", "1.0GiB", "4.0GiB"]]
        );
        assert_eq!(
            guest_lines(&json!({ "collected_at": 5 }), 5),
            [
                "hostname: -",
                "agent: -",
                "collected: 1970-01-01 00:00 UTC (0s ago)"
            ]
        );
        assert_eq!(
            guest_state_line("web", "not_running"),
            "vm web: the VM is not running"
        );
    }

    #[test]
    fn the_password_is_the_first_line_of_stdin() {
        let mut input = std::io::Cursor::new(b"s3cret pass\r\nnext\n".to_vec());
        assert_eq!(first_line(&mut input).unwrap(), "s3cret pass");
        let mut empty = std::io::Cursor::new(b"\n".to_vec());
        assert!(first_line(&mut empty).is_err());
    }
}
