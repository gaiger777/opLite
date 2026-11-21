//! `orbivisorctl sdn …`(OVN 설치·구성과 상태, 계획 65)와 SDN 네트워크의 `network subnet …`·`network ips`. 바꾸는
//! `sdn` 명령은 Task를 만들고 기본으로 끝날 때까지 기다린다.

use std::collections::BTreeSet;
use std::io::Write;
use std::net::IpAddr;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use opl_types::network::Subnet;
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{CEPH_WAIT, Client, seg};
use crate::output::{cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum SdnCmd {
    /// 노드 NODE에 OVN 꾸러미를 apt로 설치하고 NB·SB ovsdb Raft 클러스터와 northd(첫 중앙)를 세우고 chassis로 구성한다.
    /// 실패하면 같은 노드로 다시 부르면 이어서 한다.
    Init {
        node: String,
        /// NB·SB가 들을 주소(노드 인증서의 주소 가운데 하나). 없으면 노드 주소의 IP
        #[arg(long)]
        address: Option<IpAddr>,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
    /// OVN 중앙 노드(NB·SB·northd, 3이나 5개 권장)
    Central {
        #[command(subcommand)]
        cmd: CentralCmd,
    },
    /// OVN chassis(ovn-controller와 OVS — SDN NIC가 있는 VM을 돌리는 노드)
    Chassis {
        #[command(subcommand)]
        cmd: ChassisCmd,
    },
    /// 중앙과 Raft 역할·northd, 동기화, 노드마다의 chassis 상태
    Status,
    /// OVN 접근 잠금(SB 역할 포트·northd 전용 포트·주소 허용 목록)을 켜거나 끈다. 중앙을 하나씩 바꾼다.
    Lockdown {
        #[arg(value_enum)]
        state: OnOff,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
}

/// `sdn lockdown`의 켜기·끄기
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OnOff {
    On,
    Off,
}

#[derive(Subcommand)]
pub(crate) enum CentralCmd {
    /// 노드를 OVN 중앙으로 더한다(5개까지).
    Add {
        node: String,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
    /// 노드를 OVN 중앙에서 뺀다(노드가 온라인이어야 한다, 마지막 중앙은 뺄 수 없다).
    Remove {
        node: String,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ChassisCmd {
    /// 노드들(`--all`이면 아직 chassis가 아닌 모든 온라인 Linux 노드)에 OVN chassis를 설치한다. 새로 합류한 노드에는
    /// 저절로 깔지 않는다.
    Install {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        nodes: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
}

/// JSON 배열의 항목(배열이 아니면 없음)
fn arr(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// `sdn status`의 중앙 표 열
const CENTRAL_HEADERS: &[&str] = &["CENTRAL", "ADDRESS", "NB", "SB", "NORTHD"];
/// `sdn status`의 chassis 표 열
const CHASSIS_HEADERS: &[&str] = &["NODE", "CHASSIS", "ENCAP-IP", "OVN", "ERROR"];
/// `network ips`의 열
const IP_HEADERS: &[&str] = &["VM", "NIC", "MAC", "ADDRESSES", "UP", "NODE"];
/// `network ips`의 서브넷 표 열
const SUBNET_HEADERS: &[&str] = &["SUBNET", "SIZE", "USED", "FREE"];

pub(crate) async fn sdn(
    client: &Client,
    cmd: SdnCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SdnCmd::Init {
            node,
            address,
            no_wait,
        } => {
            let mut body = json!({ "node": node });
            if let Some(a) = address {
                body["address"] = json!(a);
            }
            let task = client.post("/sdn", &body).await?;
            if no_wait {
                return started(out, &task);
            }
            wait(client, &task, &node, err).await?;
            writeln!(out, "sdn: OVN is ready (central and chassis on {node})")?;
        }
        SdnCmd::Central {
            cmd: CentralCmd::Add { node, no_wait },
        } => {
            let (task, warning) = post_with_warning(
                client,
                Method::POST,
                "/sdn/centrals",
                Some(json!({ "node": node })),
            )
            .await?;
            if let Some(w) = warning {
                writeln!(err, "warning: {w}")?;
            }
            if no_wait {
                return started(out, &task);
            }
            wait(client, &task, &node, err).await?;
            writeln!(out, "sdn: {node} joined the OVN central cluster")?;
        }
        SdnCmd::Central {
            cmd: CentralCmd::Remove { node, no_wait },
        } => {
            let path = format!("/sdn/centrals/{}", seg(&node));
            let (task, warning) = post_with_warning(client, Method::DELETE, &path, None).await?;
            if let Some(w) = warning {
                writeln!(err, "warning: {w}")?;
            }
            if no_wait {
                return started(out, &task);
            }
            wait(client, &task, &node, err).await?;
            writeln!(out, "sdn: {node} left the OVN central cluster")?;
        }
        SdnCmd::Chassis {
            cmd:
                ChassisCmd::Install {
                    nodes,
                    all,
                    no_wait,
                },
        } => {
            let body = if all {
                json!({ "all": true })
            } else {
                json!({ "nodes": nodes })
            };
            let (tasks, warning) =
                post_with_warning(client, Method::POST, "/sdn/chassis", Some(body)).await?;
            if let Some(w) = warning {
                writeln!(err, "warning: {w}")?;
            }
            let list = crate::cli::items(&client.get("/nodes").await?);
            for task in tasks.as_array().cloned().unwrap_or_default() {
                let node = crate::cli::name_of(&list, &task["spec"]["node"]);
                if no_wait {
                    started(out, &task)?;
                    continue;
                }
                wait(client, &task, &node, err).await?;
                writeln!(out, "sdn: chassis installed on {node}")?;
            }
        }
        SdnCmd::Lockdown { state, no_wait } => {
            let enabled = state == OnOff::On;
            let task = client
                .post("/sdn/lockdown", &json!({ "enabled": enabled }))
                .await?;
            if no_wait {
                return started(out, &task);
            }
            wait(client, &task, "the OVN centrals", err).await?;
            writeln!(
                out,
                "sdn: OVN access lockdown is {}",
                if enabled { "on" } else { "off" }
            )?;
        }
        SdnCmd::Status => {
            let view = client.get("/sdn").await?;
            if json_out {
                return Ok(print_json(out, &view)?);
            }
            print_status(out, &view)?;
        }
    }
    Ok(())
}

/// 요청을 보내고 (본문, 응답 헤더 `orbivisor-warning`)
async fn post_with_warning(
    client: &Client,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<(Value, Option<String>)> {
    let mut r = client.request(method, path);
    if let Some(body) = &body {
        r = r.json(body);
    }
    let (headers, value) = client.send_with_headers(r).await?;
    let warning = headers
        .get("orbivisor-warning")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Ok((value, warning))
}

/// `--no-wait`: 만든 Task를 알린다.
fn started(out: &mut (dyn Write + Send), task: &Value) -> anyhow::Result<()> {
    let id = task["id"].as_str().context("response has no task id")?;
    writeln!(
        out,
        "sdn: task {id} started; follow it with `orbivisorctl task get {id}`"
    )?;
    Ok(())
}

/// SDN Task가 끝날 때까지 기다리며 진행 단계가 바뀔 때마다 한 줄을 알린다(apt 설치가 있어 오래 기다린다).
async fn wait(
    client: &Client,
    task: &Value,
    node: &str,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<Value> {
    let mut last: Option<String> = None;
    let task = client
        .wait_task_watch(task, CEPH_WAIT, |t| {
            if let Some(stage) = t["status"]["progress"]["stage"].as_str()
                && last.as_deref() != Some(stage)
            {
                last = Some(stage.to_string());
                let _ = writeln!(err, "sdn: {stage} on {node}");
            }
            Ok(())
        })
        .await?;
    if let Some(w) = task["status"]["warning"].as_str() {
        let _ = writeln!(err, "warning: {w}");
    }
    Ok(task)
}

/// `sdn status`의 글
fn print_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    let Some(phase) = view["phase"].as_str() else {
        writeln!(
            out,
            "sdn: not set up (run `orbivisorctl sdn init NODE` to install OVN)"
        )?;
        return Ok(());
    };
    writeln!(out, "phase: {phase}")?;
    if let Some(release) = view["release"].as_str() {
        writeln!(out, "OVN: {release}")?;
    }
    if let Some(e) = view["last_error"].as_str() {
        writeln!(out, "last error: {e}")?;
    }
    let sync = &view["sync"];
    if sync.is_object() {
        let caught_up = sync["hv_cfg"].as_i64() >= sync["nb_cfg"].as_i64();
        writeln!(
            out,
            "sync: term {} index {} at {} (nb_cfg {}, chassis {}){}",
            cell(&sync["term"]),
            cell(&sync["index"]),
            cell(&sync["at"]),
            cell(&sync["nb_cfg"]),
            if caught_up { "caught up" } else { "behind" },
            sync["error"]
                .as_str()
                .map(|e| format!(" — error: {e}"))
                .unwrap_or_default()
        )?;
    }
    let tasks = view["tasks"].as_array().map_or(0, Vec::len);
    if tasks > 0 {
        writeln!(out, "running SDN tasks: {tasks}")?;
    }
    writeln!(out)?;
    table(out, CENTRAL_HEADERS, &central_rows(view))?;
    writeln!(out)?;
    table(out, CHASSIS_HEADERS, &chassis_rows(view))?;
    Ok(())
}

fn central_rows(view: &Value) -> Vec<Vec<String>> {
    arr(&view["centrals"])
        .iter()
        .map(|c| {
            let h = &c["health"];
            vec![
                cell(&c["name"]),
                cell(&c["address"]),
                cell(&h["nb"]),
                cell(&h["sb"]),
                match h["northd_active"].as_bool() {
                    Some(true) => "active".into(),
                    Some(false) => "standby".into(),
                    None => "-".into(),
                },
            ]
        })
        .collect()
}

fn chassis_rows(view: &Value) -> Vec<Vec<String>> {
    arr(&view["chassis"])
        .iter()
        .map(|c| {
            let s = &c["sdn"];
            vec![
                cell(&c["name"]),
                s["chassis"].as_str().unwrap_or("missing").to_string(),
                cell(&s["encap_ip"]),
                cell(&s["ovn_version"]),
                cell(&s["error"]),
            ]
        })
        .collect()
}

/// `--subnet CIDR[,gateway=IP][,dhcp=on|off][,dns=IP+IP][,pool=A-B+C-D][,floating=A-B+C-D][,ipv6-mode=MODE]
/// [,description=TEXT]`의 서브넷(검증은 서버가 한다). `floating`은 공급자 네트워크의 유동 IP 풀, `ipv6-mode`는 `slaac`·
/// `dhcpv6-stateful`·`dhcpv6-stateless`(계획 66)다.
pub(crate) fn parse_subnet(s: &str) -> Result<Subnet, String> {
    let bad = |why: &str| {
        format!(
            "invalid subnet {s:?}: {why}; use CIDR[,gateway=IP][,dhcp=on|off][,dns=IP+IP][,pool=A-B+C-D]\
             [,floating=A-B+C-D][,ipv6-mode=slaac|dhcpv6-stateful|dhcpv6-stateless]"
        )
    };
    let ranges = |value: &str, what: &str| -> Result<Vec<Value>, String> {
        value
            .split('+')
            .map(|p| {
                let (a, b) = p
                    .split_once('-')
                    .ok_or_else(|| bad(&format!("{what} is START-END")))?;
                Ok(json!({ "start": a, "end": b }))
            })
            .collect()
    };
    let mut parts = s.split(',');
    let cidr = parts.next().unwrap_or_default();
    let mut v = json!({ "cidr": cidr });
    for part in parts {
        let (key, value) = part.split_once('=').ok_or_else(|| bad("missing '='"))?;
        match key {
            "gateway" => v["gateway"] = json!(value),
            "dhcp" => {
                v["dhcp"] = json!(match value {
                    "on" | "true" => true,
                    "off" | "false" => false,
                    _ => return Err(bad("dhcp is on or off")),
                })
            }
            "dns" => v["dns_servers"] = json!(value.split('+').collect::<Vec<_>>()),
            "pool" => v["pools"] = Value::Array(ranges(value, "pool")?),
            "floating" => v["floating_pools"] = Value::Array(ranges(value, "floating")?),
            "ipv6-mode" => {
                v["ipv6_mode"] = json!(match value {
                    "slaac" | "dhcpv6-stateful" | "dhcpv6-stateless" => value,
                    _ =>
                        return Err(bad(
                            "ipv6-mode is slaac, dhcpv6-stateful or dhcpv6-stateless"
                        )),
                })
            }
            "description" => v["description"] = json!(value),
            _ => return Err(bad(&format!("unknown option {key:?}"))),
        }
    }
    serde_json::from_value(v).map_err(|e| bad(&e.to_string()))
}

#[derive(Subcommand)]
pub(crate) enum SubnetCmd {
    /// SDN 네트워크에 서브넷을 더한다(주소 체계마다 하나). 주소가 하나도 없는 NIC(켜진 VM 포함)는 서버가 새 서브넷의
    /// 주소를 준다(모두 줄 수 없으면 409 `ip_exhausted`). 주소가 있는 NIC에 빠진 체계는 `network ips NAME --fill`로 채운다.
    Add {
        network: String,
        #[arg(value_name = "CIDR[,gateway=IP][,dhcp=on|off][,dns=IP+IP][,pool=A-B]", value_parser = parse_subnet)]
        subnet: Subnet,
    },
    /// 서브넷의 DHCP·DNS·풀·설명을 바꾼다(CIDR·게이트웨이는 바꿀 수 없다). 준 칸만 바꾼다.
    Set {
        network: String,
        #[arg(value_name = "CIDR[,dhcp=on|off][,dns=IP+IP][,pool=A-B][,description=TEXT]", value_parser = parse_subnet)]
        subnet: Subnet,
    },
    /// 서브넷을 뺀다. 그 주소를 쓰는 NIC가 없어야 한다.
    Remove { network: String, cidr: String },
}

/// `network subnet …`: 네트워크를 읽어 서브넷 목록을 바꾸고 `PATCH /networks/{key}`로 쓴다(리비전 확인).
pub(crate) async fn network_subnet(
    client: &Client,
    cmd: SubnetCmd,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let network = match &cmd {
        SubnetCmd::Add { network, .. }
        | SubnetCmd::Set { network, .. }
        | SubnetCmd::Remove { network, .. } => network.clone(),
    };
    let path = format!("/networks/{}", seg(&network));
    let current = client.get(&path).await?;
    let subnets = subnets_after(&current, &cmd)?;
    let r = client
        .request(Method::PATCH, &path)
        .header("if-match", cell(&current["revision"]))
        .json(&json!({ "subnets": subnets }));
    client.send(r).await?;
    let what = match &cmd {
        SubnetCmd::Add { subnet, .. } => format!("subnet {} added to", subnet.cidr),
        SubnetCmd::Set { subnet, .. } => {
            format!("subnet {} of", subnet.cidr).replace(" of", " changed on")
        }
        SubnetCmd::Remove { cidr, .. } => format!("subnet {cidr} removed from"),
    };
    writeln!(out, "{what} network {network}")?;
    Ok(())
}

/// 지금 네트워크 `current`에 `cmd`를 적용한 서브넷 목록(JSON)
fn subnets_after(current: &Value, cmd: &SubnetCmd) -> anyhow::Result<Vec<Value>> {
    let mut list = arr(&current["spec"]["subnets"]);
    let pos = |list: &[Value], cidr: &str| list.iter().position(|s| s["cidr"] == cidr);
    match cmd {
        SubnetCmd::Add { subnet, .. } => {
            let cidr = subnet.cidr.to_string();
            if pos(&list, &cidr).is_some() {
                bail!(
                    "network {} already has subnet {cidr}",
                    cell(&current["name"])
                );
            }
            list.push(serde_json::to_value(subnet)?);
        }
        SubnetCmd::Set { subnet, .. } => {
            let cidr = subnet.cidr.to_string();
            let i = pos(&list, &cidr).with_context(|| {
                format!("network {} has no subnet {cidr}", cell(&current["name"]))
            })?;
            let given = serde_json::to_value(subnet)?;
            for (k, v) in given.as_object().into_iter().flatten() {
                if k != "cidr" {
                    list[i][k] = v.clone();
                }
            }
        }
        SubnetCmd::Remove { cidr, .. } => {
            let i = pos(&list, cidr).with_context(|| {
                format!("network {} has no subnet {cidr}", cell(&current["name"]))
            })?;
            list.remove(i);
        }
    }
    Ok(list)
}

#[derive(Args)]
pub(crate) struct NetworkIps {
    pub(crate) network: String,
    /// 서브넷 주소가 없는 NIC(서브넷을 나중에 더한 네트워크)에 빈 주소를 골라 VM 스펙에 적는다(꺼진 VM만 — 켜진 VM의 주소는
    /// 바꿀 수 없다).
    #[arg(long)]
    pub(crate) fill: bool,
}

/// `network ips`: 서브넷 사용과 NIC마다의 주소·OVN 포트 상태. `--fill`이면 먼저 빈 주소를 채운다.
pub(crate) async fn network_ips(
    client: &Client,
    args: &NetworkIps,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/networks/{}/ips", seg(&args.network));
    if args.fill {
        fill(client, &args.network, out, err).await?;
    }
    let view = client.get(&path).await?;
    if json_out {
        return Ok(print_json(out, &view)?);
    }
    if let Some(e) = view["live_error"].as_str() {
        writeln!(err, "warning: cannot read OVN port state: {e}")?;
    }
    if let Some(n) = view["hidden"].as_u64().filter(|n| *n > 0) {
        writeln!(
            err,
            "note: {n} NIC(s) of VMs you cannot read are counted in USED but not listed"
        )?;
    }
    let subnets: Vec<Vec<String>> = arr(&view["subnets"])
        .iter()
        .map(|s| {
            vec![
                cell(&s["cidr"]),
                cell(&s["size"]),
                cell(&s["used"]),
                cell(&s["free"]),
            ]
        })
        .collect();
    table(out, SUBNET_HEADERS, &subnets)?;
    writeln!(out)?;
    table(out, IP_HEADERS, &ip_rows(&view))?;
    Ok(())
}

fn ip_rows(view: &Value) -> Vec<Vec<String>> {
    arr(&view["allocations"])
        .iter()
        .map(|a| {
            let ips: Vec<String> = arr(&a["ips"]).iter().map(cell).collect();
            vec![
                cell(&a["vm_name"]),
                cell(&a["nic"]),
                cell(&a["mac"]),
                if ips.is_empty() {
                    "-".into()
                } else {
                    ips.join(",")
                },
                match a["live"]["up"].as_bool() {
                    Some(true) => "yes".into(),
                    Some(false) => "no".into(),
                    None => "-".into(),
                },
                cell(&a["live"]["node"]),
            ]
        })
        .collect()
}

/// 네트워크 `network`의 SDN NIC 가운데 서브넷 주소 체계의 주소가 없는 것에 빈 주소를 골라 VM 스펙에 적는다(API는 주소가
/// 있던 NIC의 빠진 체계를 저절로 채우지 않는다 — 설계 §9). 다른 요청이 그새 같은 주소를 잡으면(409 `ip_in_use`) 다시 고른다. 켜진 VM은
/// 주소가 하나도 없는 NIC만 채우고(리뷰 2 N-4 — 그 포트는 꺼져 있었다), 주소가 있는 NIC에 빠진 체계는 건너뛰고 알린다.
async fn fill(
    client: &Client,
    network: &str,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let net = client.get(&format!("/networks/{}", seg(network))).await?;
    let subnets: Vec<Subnet> = serde_json::from_value(net["spec"]["subnets"].clone())
        .context("unexpected network subnets")?;
    let name = cell(&net["name"]);
    let mut filled = 0;
    for round in 0..5 {
        let vms = crate::cli::items(&client.get("/vms").await?);
        let mut used: BTreeSet<IpAddr> = vms
            .iter()
            .filter(|v| v["spec"]["template"] != true)
            .flat_map(|v| arr(&v["spec"]["nics"]))
            .filter(|n| n["network"] == name.as_str())
            .flat_map(|n| arr(&n["ips"]))
            .filter_map(|ip| ip.as_str()?.parse().ok())
            .collect();
        let mut retry = false;
        for vm in vms.iter().filter(|v| v["spec"]["template"] != true) {
            let vm_name = cell(&vm["name"]);
            let running = vm["status"]["phase"]
                .as_str()
                .is_some_and(|p| p != "stopped");
            let (spec, skipped) = fill_spec(vm, &name, &subnets, &mut used, running)?;
            if skipped && round == 0 {
                writeln!(
                    err,
                    "warning: VM {vm_name} is not stopped; stop it and run --fill again to add the missing address family"
                )?;
            }
            let Some(spec) = spec else {
                continue;
            };
            let r = client
                .request(Method::PUT, &format!("/vms/{}", seg(&vm_name)))
                .header("if-match", cell(&vm["revision"]))
                .json(&json!({ "spec": spec, "labels": vm["labels"] }));
            match client.send(r).await {
                Ok(_) => {
                    filled += 1;
                    writeln!(out, "VM {vm_name}: addresses filled")?;
                }
                Err(e)
                    if e.downcast_ref::<crate::client::ApiError>()
                        .is_some_and(|a| {
                            a.code == "ip_in_use" || a.code == "revision_mismatch"
                        }) =>
                {
                    retry = true;
                }
                Err(e) => return Err(e),
            }
        }
        if !retry {
            if filled == 0 {
                writeln!(out, "every NIC on network {name} already has its addresses")?;
            }
            return Ok(());
        }
    }
    bail!("addresses on network {name} kept changing; run --fill again")
}

/// VM `vm`의 네트워크 `network` NIC에 빠진 주소 체계마다 서브넷의 빈 주소를 넣은 새 스펙(바꿀 것이 없으면 None).
/// 고른 주소는 `used`에 넣는다. `running`이면 주소가 하나도 없는 NIC만 채우고(켜진 VM의 있던 주소는 바꾸지 않는다), 빠진
/// 체계가 있어 건너뛴 NIC가 있으면 둘째 값이 참이다.
fn fill_spec(
    vm: &Value,
    network: &str,
    subnets: &[Subnet],
    used: &mut BTreeSet<IpAddr>,
    running: bool,
) -> anyhow::Result<(Option<Value>, bool)> {
    let mut spec = vm["spec"].clone();
    let mut changed = false;
    let mut skipped = false;
    let Some(nics) = spec["nics"].as_array_mut() else {
        return Ok((None, false));
    };
    for nic in nics.iter_mut().filter(|n| n["network"] == network) {
        let mut ips: Vec<IpAddr> = arr(&nic["ips"])
            .iter()
            .filter_map(|ip| ip.as_str()?.parse().ok())
            .collect();
        if running && !ips.is_empty() {
            skipped |= subnets
                .iter()
                .any(|s| !ips.iter().any(|ip| ip.is_ipv4() == s.is_ipv4()));
            continue;
        }
        for subnet in subnets {
            if ips.iter().any(|ip| ip.is_ipv4() == subnet.is_ipv4()) {
                continue;
            }
            let ip = opl_types::ipam::allocate(subnet, used, None)
                .map_err(|e| anyhow::anyhow!("network {network}: {e}"))?;
            used.insert(ip);
            ips.push(ip);
            changed = true;
        }
        ips.sort_by_key(|ip| !ip.is_ipv4());
        nic["ips"] = json!(ips);
    }
    Ok((changed.then_some(spec), skipped))
}

/// `--ip [nicN=]A[+B]`: NIC 순번(없으면 준 순서)과 주소(체계마다 하나까지)
pub(crate) fn parse_nic_ips(s: &str) -> Result<(Option<usize>, Vec<IpAddr>), String> {
    let bad = || format!("invalid --ip {s:?}: use [nicN=]ADDRESS[+ADDRESS]");
    let (index, list) = match s.split_once('=') {
        Some((key, value)) => (
            Some(
                key.strip_prefix("nic")
                    .and_then(|n| n.parse::<usize>().ok())
                    .ok_or_else(bad)?,
            ),
            value,
        ),
        None => (None, s),
    };
    let ips: Vec<IpAddr> = list
        .split('+')
        .map(|a| a.parse().map_err(|_| bad()))
        .collect::<Result<_, _>>()?;
    Ok((index, ips))
}

/// `--ip`들을 NIC 순번별 주소 목록으로(빈 자리는 `[]` — 자동)
pub(crate) fn ips_by_index(list: &[(Option<usize>, Vec<IpAddr>)]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    for (k, (index, ips)) in list.iter().enumerate() {
        let i = index.unwrap_or(k);
        if out.len() <= i {
            out.resize(i + 1, json!([]));
        }
        out[i] = json!(ips);
    }
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnets_parse_with_options() {
        let s = parse_subnet("10.0.0.0/24,gateway=10.0.0.254,dhcp=off,dns=1.1.1.1+8.8.8.8,pool=10.0.0.10-10.0.0.20+10.0.0.30-10.0.0.40").unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            json!({
                "cidr": "10.0.0.0/24", "gateway": "10.0.0.254", "dhcp": false,
                "dns_servers": ["1.1.1.1", "8.8.8.8"],
                "pools": [{ "start": "10.0.0.10", "end": "10.0.0.20" }, { "start": "10.0.0.30", "end": "10.0.0.40" }]
            })
        );
        assert_eq!(
            serde_json::to_value(parse_subnet("fd00::/64").unwrap()).unwrap(),
            json!({ "cidr": "fd00::/64" })
        );
        for bad in [
            "10.0.0.0/24,dhcp=maybe",
            "10.0.0.0/24,x=1",
            "nope",
            "10.0.0.0/24,pool=1",
        ] {
            assert!(parse_subnet(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn subnet_edits_change_the_list() {
        let net = json!({ "name": "t", "spec": { "subnets": [
            { "cidr": "10.0.0.0/24", "dns_servers": ["1.1.1.1"] }
        ] } });
        let add = SubnetCmd::Add {
            network: "t".into(),
            subnet: parse_subnet("fd00::/64,dhcp=off").unwrap(),
        };
        assert_eq!(subnets_after(&net, &add).unwrap().len(), 2);
        let set = SubnetCmd::Set {
            network: "t".into(),
            subnet: parse_subnet("10.0.0.0/24,dhcp=off").unwrap(),
        };
        assert_eq!(
            subnets_after(&net, &set).unwrap(),
            [json!({ "cidr": "10.0.0.0/24", "dns_servers": ["1.1.1.1"], "dhcp": false })]
        );
        let remove = SubnetCmd::Remove {
            network: "t".into(),
            cidr: "10.0.0.0/24".into(),
        };
        assert!(subnets_after(&net, &remove).unwrap().is_empty());
        let missing = SubnetCmd::Remove {
            network: "t".into(),
            cidr: "10.9.0.0/24".into(),
        };
        assert!(subnets_after(&net, &missing).is_err());
        assert!(
            subnets_after(
                &net,
                &SubnetCmd::Add {
                    network: "t".into(),
                    subnet: parse_subnet("10.0.0.0/24").unwrap(),
                }
            )
            .is_err()
        );
    }

    #[test]
    fn fill_gives_missing_families_the_lowest_free_address() {
        let subnets = vec![
            parse_subnet("10.0.0.0/29").unwrap(),
            parse_subnet("fd00::/64,dhcp=off").unwrap(),
        ];
        let vm = json!({ "name": "a", "spec": { "nics": [
            { "network": "t", "model": "virtio", "ips": ["10.0.0.2"] },
            { "network": "other", "model": "virtio" }
        ] } });
        let mut used: BTreeSet<IpAddr> = ["10.0.0.2".parse().unwrap()].into();
        // 켜진 VM이면 주소가 있는 NIC는 건너뛰고 알린다(리뷰 2 N-4).
        let (none, skipped) = fill_spec(&vm, "t", &subnets, &mut used, true).unwrap();
        assert!(none.is_none() && skipped);
        let (spec, _) = fill_spec(&vm, "t", &subnets, &mut used, false).unwrap();
        let spec = spec.unwrap();
        assert_eq!(spec["nics"][0]["ips"], json!(["10.0.0.2", "fd00::2"]));
        assert!(spec["nics"][1].get("ips").is_none());
        // 다 있으면 바꾸지 않는다.
        let done = json!({ "spec": spec });
        assert!(
            fill_spec(&done, "t", &subnets, &mut used, false)
                .unwrap()
                .0
                .is_none()
        );
        // 켜진 VM의 주소 없는 NIC는 채운다.
        let bare = json!({ "spec": { "nics": [{ "network": "t", "model": "virtio" }] } });
        let (spec, skipped) = fill_spec(&bare, "t", &subnets, &mut used, true).unwrap();
        assert!(!skipped);
        assert_eq!(
            spec.unwrap()["nics"][0]["ips"],
            json!(["10.0.0.3", "fd00::3"])
        );
    }

    #[test]
    fn nic_ips_parse_and_index() {
        let a = parse_nic_ips("nic1=10.0.0.5+fd00::5").unwrap();
        assert_eq!(a.0, Some(1));
        assert_eq!(a.1.len(), 2);
        assert_eq!(parse_nic_ips("10.0.0.6").unwrap().0, None);
        assert!(parse_nic_ips("nicx=10.0.0.5").is_err());
        assert!(parse_nic_ips("10.0.0").is_err());
        assert_eq!(ips_by_index(&[a]), json!([[], ["10.0.0.5", "fd00::5"]]));
    }

    #[test]
    fn status_rows() {
        let view = json!({
            "phase": "ready",
            "centrals": [{ "name": "n1", "address": "10.0.0.1",
                "health": { "nb": "leader", "sb": "follower", "northd_active": true } }],
            "chassis": [{ "name": "n1", "sdn": { "chassis": "ready", "encap_ip": "10.0.0.1", "ovn_version": "24.03.8" } },
                        { "name": "n2" }],
            "tasks": []
        });
        assert_eq!(
            central_rows(&view),
            [vec!["n1", "10.0.0.1", "leader", "follower", "active"]]
        );
        assert_eq!(chassis_rows(&view)[1], ["n2", "missing", "-", "-", "-"]);
        let mut out = Vec::new();
        print_status(
            &mut out,
            &json!({ "centrals": [], "chassis": [], "tasks": [] }),
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("not set up"));
    }
}
