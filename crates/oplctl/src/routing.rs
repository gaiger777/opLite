//! `orbivisorctl router …`·`fip …`·`node sdn …`(계획 66 §13): SDN 라우터, 유동 IP, 노드의 physnet·게이트웨이. 바꾸는 명령은
//! 리소스를 읽어 고친 뒤 리비전 확인(`If-Match`)으로 쓴다.

use std::io::Write;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, seg};
use crate::output::{cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum RouterCmd {
    List,
    /// 라우터와 게이트웨이 표(노드·우선순위·지금 주인), 물리 라우터에 둘 경로
    Get {
        router: String,
    },
    /// 라우터를 만든다(네트워크 폴더의 network.config).
    Create {
        name: String,
        /// 외부 게이트웨이 `NET[,ip=A[+B]][,snat=on|off]`(공급자 네트워크, 주소는 없으면 서버가 고른다)
        #[arg(long, value_name = "NET[,ip=A[+B]][,snat=on|off]", value_parser = parse_external)]
        external: Option<Value>,
        /// 잇는 오버레이 SDN 네트워크(여러 번)
        #[arg(long = "interface", value_name = "NET")]
        interfaces: Vec<String>,
        #[arg(long)]
        description: Option<String>,
        /// 넣을 네트워크 폴더(경로나 id)
        #[arg(long)]
        folder: Option<String>,
    },
    /// 라우터를 지운다(인터페이스가 없어야 한다).
    Delete {
        router: String,
    },
    /// 내 네트워크를 그 네트워크가 인터페이스인 라우터에서 뗀다(네트워크의 network.config — 라우터를 읽을 수 없어도)
    Detach {
        network: String,
    },
    /// 인터페이스(오버레이 SDN 네트워크 — 그 서브넷 게이트웨이가 라우터 주소)
    Interface {
        #[command(subcommand)]
        cmd: InterfaceCmd,
    },
    /// 정적 경로
    Route {
        #[command(subcommand)]
        cmd: RouteCmd,
    },
    /// 외부 게이트웨이(공급자 네트워크)
    External {
        #[command(subcommand)]
        cmd: ExternalCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum InterfaceCmd {
    Add { router: String, network: String },
    Remove { router: String, network: String },
}

#[derive(Subcommand)]
pub(crate) enum RouteCmd {
    Add {
        router: String,
        destination: String,
        nexthop: String,
    },
    Remove {
        router: String,
        destination: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum ExternalCmd {
    /// 외부 게이트웨이를 정한다. 같은 네트워크에 ip를 주지 않으면 지금 주소를 그대로 쓴다.
    Set {
        router: String,
        #[arg(value_name = "NET[,ip=A[+B]][,snat=on|off]", value_parser = parse_external)]
        external: Value,
    },
    /// 외부 게이트웨이를 뺀다.
    Clear { router: String },
}

#[derive(Subcommand)]
pub(crate) enum FipCmd {
    List,
    Get {
        fip: String,
    },
    /// 공급자 네트워크에서 유동 IP를 받는다(주소는 없으면 유동 IP 풀에서).
    Create {
        #[arg(long)]
        network: String,
        #[arg(long = "ip")]
        address: Option<String>,
        /// 이름(없으면 주소의 점을 -로 바꾼 것)
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        folder: Option<String>,
        /// 곧바로 붙일 VM NIC `VM[:NIC]`(NIC는 순번 0부터나 MAC, 없으면 0)
        #[arg(long, value_name = "VM[:NIC]")]
        attach: Option<String>,
    },
    /// 유동 IP를 VM NIC에 붙인다(NIC의 네트워크가 외부 네트워크로 나가는 라우터의 인터페이스여야 한다).
    Attach {
        fip: String,
        #[arg(value_name = "VM[:NIC]")]
        target: String,
    },
    Detach {
        fip: String,
    },
    Delete {
        fip: String,
    },
}

/// `node sdn NODE …`
#[derive(Args)]
pub(crate) struct NodeSdnArgs {
    pub(crate) node: String,
    /// physnet NAME을 이 노드의 Linux 브리지 BRIDGE에 잇는다(여러 번).
    #[arg(long = "physnet", value_name = "NAME=BRIDGE")]
    physnets: Vec<String>,
    /// physnet을 뺀다(여러 번).
    #[arg(long = "no-physnet", value_name = "NAME")]
    no_physnets: Vec<String>,
    /// 게이트웨이 노드 후보로(우선순위 1-1000, 클수록 먼저, 기본 100)
    #[arg(long, value_name = "PRIORITY", num_args = 0..=1, default_missing_value = "100",
        conflicts_with = "no_gateway")]
    gateway: Option<u16>,
    #[arg(long)]
    no_gateway: bool,
}

impl NodeSdnArgs {
    fn given(&self) -> bool {
        !self.physnets.is_empty()
            || !self.no_physnets.is_empty()
            || self.gateway.is_some()
            || self.no_gateway
    }
}

/// `NET[,ip=A[+B]][,snat=on|off]`의 `RouterExternal`(검증은 서버가 한다)
pub(crate) fn parse_external(s: &str) -> Result<Value, String> {
    let bad =
        |why: &str| format!("invalid external {s:?}: {why}; use NET[,ip=A[+B]][,snat=on|off]");
    let mut parts = s.split(',');
    let network = parts.next().unwrap_or_default();
    if network.is_empty() {
        return Err(bad("no network"));
    }
    let mut v = json!({ "network": network });
    for part in parts {
        match part.split_once('=').ok_or_else(|| bad("missing '='"))? {
            ("ip", ips) => v["ips"] = json!(ips.split('+').collect::<Vec<_>>()),
            ("snat", "on" | "true") => v["snat"] = json!(true),
            ("snat", "off" | "false") => v["snat"] = json!(false),
            ("snat", _) => return Err(bad("snat is on or off")),
            (k, _) => return Err(bad(&format!("unknown option {k:?}"))),
        }
    }
    Ok(v)
}

fn arr(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// 응답 헤더 `orbivisor-warning`을 알린다.
fn warn(err: &mut (dyn Write + Send), headers: &reqwest::header::HeaderMap) -> anyhow::Result<()> {
    if let Some(w) = headers
        .get("orbivisor-warning")
        .and_then(|v| v.to_str().ok())
    {
        writeln!(err, "warning: {w}")?;
    }
    Ok(())
}

const ROUTER_HEADERS: &[&str] = &["NAME", "EXTERNAL", "SNAT", "INTERFACES", "GATEWAY"];
const GATEWAY_HEADERS: &[&str] = &["NODE", "PRIORITY", "NODE-PRIORITY", "ACTIVE"];
const FIP_HEADERS: &[&str] = &["NAME", "ADDRESS", "NETWORK", "VM", "STATE"];

fn router_row(r: &Value) -> Vec<String> {
    let s = &r["spec"];
    let ext = &s["external"];
    let interfaces: Vec<String> = arr(&s["interfaces"])
        .iter()
        .map(|i| cell(&i["network"]))
        .collect();
    vec![
        cell(&r["name"]),
        if ext.is_object() {
            let ips: Vec<String> = arr(&ext["ips"]).iter().map(cell).collect();
            format!("{} {}", cell(&ext["network"]), ips.join(","))
        } else {
            "-".into()
        },
        match ext.is_object() {
            false => "-".into(),
            true if ext["snat"] == false => "off".into(),
            true => "on".into(),
        },
        if interfaces.is_empty() {
            "-".into()
        } else {
            interfaces.join(",")
        },
        match (r["hosting"].as_str(), r["gateway_error"].as_str()) {
            (Some(h), _) => h.to_string(),
            (None, Some(_)) => "none".into(),
            (None, None) => "-".into(),
        },
    ]
}

/// `fip list`의 STATE 칸(`distributed n2`·`centralized`·`unrouted`·`unattached`)
fn fip_state(f: &Value) -> String {
    let s = &f["state"];
    match s["kind"].as_str() {
        Some("distributed") => format!("distributed {}", cell(&s["node"])),
        Some(k) => k.to_string(),
        None => "-".into(),
    }
}

fn fip_row(f: &Value) -> Vec<String> {
    let t = &f["spec"]["target"];
    vec![
        cell(&f["name"]),
        cell(&f["spec"]["address"]),
        cell(&f["spec"]["network"]),
        if t.is_object() {
            match f["vm_name"].as_str() {
                Some(n) => format!("{n}/{}", cell(&t["mac"])),
                None => format!("{}/{}", cell(&t["vm"]), cell(&t["mac"])),
            }
        } else {
            "-".into()
        },
        fip_state(f),
    ]
}

/// 라우터를 읽어 `change`로 스펙을 고치고 `PATCH`로 쓴다(바뀐 칸만, 리비전 확인).
async fn patch_router(
    client: &Client,
    router: &str,
    change: impl FnOnce(&Value) -> anyhow::Result<Value>,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/routers/{}", seg(router));
    let current = client.get(&format!("{path}?live=false")).await?;
    let body = change(&current["spec"])?;
    let r = client
        .request(Method::PATCH, &path)
        .header("if-match", cell(&current["revision"]))
        .json(&body);
    let (headers, _) = client.send_with_headers(r).await?;
    warn(err, &headers)
}

pub(crate) async fn router(
    client: &Client,
    cmd: RouterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        RouterCmd::List => {
            let list = client.get("/routers").await?;
            if json_out {
                return Ok(print_json(out, &list["items"])?);
            }
            if let Some(e) = arr(&list["items"])
                .iter()
                .find_map(|r| r["live_error"].as_str().map(str::to_string))
            {
                writeln!(err, "warning: cannot read the gateway owners from OVN: {e}")?;
            }
            let rows: Vec<_> = arr(&list["items"]).iter().map(router_row).collect();
            table(out, ROUTER_HEADERS, &rows)?;
        }
        RouterCmd::Get { router } => {
            let r = client.get(&format!("/routers/{}", seg(&router))).await?;
            if json_out {
                return Ok(print_json(out, &r)?);
            }
            print_router(out, err, &r)?;
        }
        RouterCmd::Create {
            name,
            external,
            interfaces,
            description,
            folder,
        } => {
            let mut spec = json!({});
            if let Some(e) = external {
                spec["external"] = e;
            }
            if !interfaces.is_empty() {
                spec["interfaces"] = json!(
                    interfaces
                        .iter()
                        .map(|n| json!({ "network": n }))
                        .collect::<Vec<_>>()
                );
            }
            if let Some(d) = description {
                spec["description"] = json!(d);
            }
            if let Some(f) = &folder
                && let Some(id) = crate::access::resolve_folder(client, "network", f).await?
            {
                spec["folder"] = json!(id);
            }
            let r = client
                .request(Method::POST, "/routers")
                .json(&json!({ "name": name, "spec": spec }));
            let (headers, created) = client.send_with_headers(r).await?;
            warn(err, &headers)?;
            let ips: Vec<String> = arr(&created["spec"]["external"]["ips"])
                .iter()
                .map(cell)
                .collect();
            if ips.is_empty() {
                writeln!(out, "router {name} created")?;
            } else {
                writeln!(out, "router {name} created (external {})", ips.join(", "))?;
            }
        }
        RouterCmd::Delete { router } => {
            client.delete(&format!("/routers/{}", seg(&router))).await?;
            writeln!(out, "router {router} deleted")?;
        }
        RouterCmd::Detach { network } => {
            client
                .delete(&format!("/networks/{}/router", seg(&network)))
                .await?;
            writeln!(out, "network {network} detached from its router")?;
        }
        RouterCmd::Interface { cmd } => {
            let (router, network, add) = match cmd {
                InterfaceCmd::Add { router, network } => (router, network, true),
                InterfaceCmd::Remove { router, network } => (router, network, false),
            };
            patch_router(
                client,
                &router,
                |spec| {
                    let mut list = arr(&spec["interfaces"]);
                    let pos = list.iter().position(|i| i["network"] == network.as_str());
                    match (add, pos) {
                        (true, Some(_)) => bail!("{network} is already an interface of {router}"),
                        (true, None) => list.push(json!({ "network": network })),
                        (false, None) => bail!("{network} is not an interface of {router}"),
                        (false, Some(i)) => {
                            list.remove(i);
                        }
                    }
                    Ok(json!({ "interfaces": list }))
                },
                err,
            )
            .await?;
            let what = if add { "added to" } else { "removed from" };
            writeln!(out, "interface {network} {what} router {router}")?;
        }
        RouterCmd::Route { cmd } => {
            let (router, destination, nexthop) = match cmd {
                RouteCmd::Add {
                    router,
                    destination,
                    nexthop,
                } => (router, destination, Some(nexthop)),
                RouteCmd::Remove {
                    router,
                    destination,
                } => (router, destination, None),
            };
            let add = nexthop.is_some();
            patch_router(
                client,
                &router,
                |spec| {
                    let mut list = arr(&spec["routes"]);
                    let pos = list
                        .iter()
                        .position(|r| r["destination"] == destination.as_str());
                    match (nexthop, pos) {
                        (Some(_), Some(_)) => {
                            bail!("{router} already has a route to {destination}")
                        }
                        (Some(hop), None) => {
                            list.push(json!({ "destination": destination, "nexthop": hop }))
                        }
                        (None, None) => bail!("{router} has no route to {destination}"),
                        (None, Some(i)) => {
                            list.remove(i);
                        }
                    }
                    Ok(json!({ "routes": list }))
                },
                err,
            )
            .await?;
            let what = if add { "added to" } else { "removed from" };
            writeln!(out, "route {destination} {what} router {router}")?;
        }
        RouterCmd::External { cmd } => {
            let (router, external) = match cmd {
                ExternalCmd::Set { router, external } => (router, external),
                ExternalCmd::Clear { router } => (router, Value::Null),
            };
            let cleared = external.is_null();
            let network = cell(&external["network"]);
            patch_router(
                client,
                &router,
                |_| Ok(json!({ "external": external })),
                err,
            )
            .await?;
            if cleared {
                writeln!(out, "router {router}: external gateway removed")?;
            } else {
                writeln!(out, "router {router}: external gateway {network}")?;
            }
        }
    }
    Ok(())
}

/// `router get`의 사람용 출력
fn print_router(
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
    r: &Value,
) -> anyhow::Result<()> {
    let s = &r["spec"];
    writeln!(out, "router {}", cell(&r["name"]))?;
    let interfaces: Vec<String> = arr(&s["interfaces"])
        .iter()
        .map(|i| cell(&i["network"]))
        .collect();
    writeln!(
        out,
        "interfaces: {}",
        if interfaces.is_empty() {
            "-".into()
        } else {
            interfaces.join(", ")
        }
    )?;
    let ext = &s["external"];
    if ext.is_object() {
        let ips: Vec<String> = arr(&ext["ips"]).iter().map(cell).collect();
        let snat = if ext["snat"] == false { "off" } else { "on" };
        writeln!(
            out,
            "external: {} {} (snat {snat})",
            cell(&ext["network"]),
            ips.join(", ")
        )?;
    } else {
        writeln!(out, "external: -")?;
    }
    for route in arr(&s["routes"]) {
        writeln!(
            out,
            "route: {} via {}",
            cell(&route["destination"]),
            cell(&route["nexthop"])
        )?;
    }
    if let Some(e) = r["gateway_error"].as_str() {
        writeln!(err, "warning: {e}; the external gateway is off")?;
    }
    if let Some(e) = r["live_error"].as_str() {
        writeln!(err, "warning: cannot read the gateway owner from OVN: {e}")?;
    }
    let gateways: Vec<Vec<String>> = arr(&r["gateways"])
        .iter()
        .map(|g| {
            vec![
                cell(&g["node"]),
                cell(&g["priority"]),
                cell(&g["node_priority"]),
                if g["active"] == true { "yes" } else { "-" }.to_string(),
            ]
        })
        .collect();
    if !gateways.is_empty() {
        writeln!(out)?;
        table(out, GATEWAY_HEADERS, &gateways)?;
    }
    let upstream: Vec<String> = arr(&r["upstream_routes"]).iter().map(cell).collect();
    if !upstream.is_empty() {
        writeln!(out)?;
        writeln!(out, "routes the physical router needs (no NAT):")?;
        for u in upstream {
            writeln!(out, "  {u}")?;
        }
    }
    Ok(())
}

/// `VM[:NIC]`의 대상 `{vm, mac}`: NIC는 순번(0부터)이나 MAC, 없으면 0
async fn fip_target(client: &Client, s: &str) -> anyhow::Result<Value> {
    // VM 이름에는 `:`가 없다: 첫 `:` 뒤 전체가 NIC(MAC에도 `:`가 있다).
    let (vm, nic) = s.split_once(':').unwrap_or((s, "0"));
    let found = client.get(&format!("/vms/{}", seg(vm))).await?;
    pick_nic(&found, nic)
}

/// VM의 NIC(순번이나 MAC)를 `{vm, mac}`로
fn pick_nic(vm: &Value, nic: &str) -> anyhow::Result<Value> {
    let nics = arr(&vm["spec"]["nics"]);
    let found = match nic.parse::<usize>() {
        Ok(i) => nics.get(i),
        Err(_) => nics.iter().find(|n| {
            n["mac"]
                .as_str()
                .is_some_and(|m| m.eq_ignore_ascii_case(nic))
        }),
    };
    let found = found.with_context(|| format!("VM {} has no NIC {nic}", cell(&vm["name"])))?;
    Ok(json!({ "vm": vm["id"], "mac": found["mac"] }))
}

pub(crate) async fn fip(
    client: &Client,
    cmd: FipCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        FipCmd::List => {
            let list = client.get("/floating-ips").await?;
            if json_out {
                return Ok(print_json(out, &list["items"])?);
            }
            let rows: Vec<_> = arr(&list["items"]).iter().map(fip_row).collect();
            table(out, FIP_HEADERS, &rows)?;
        }
        FipCmd::Get { fip } => {
            let f = client.get(&format!("/floating-ips/{}", seg(&fip))).await?;
            if json_out {
                return Ok(print_json(out, &f)?);
            }
            table(out, FIP_HEADERS, &[fip_row(&f)])?;
            if let Some(reason) = f["state"]["reason"].as_str() {
                writeln!(out, "{}: {reason}", fip_state(&f))?;
            }
        }
        FipCmd::Create {
            network,
            address,
            name,
            description,
            folder,
            attach,
        } => {
            let mut spec = json!({ "network": network });
            if let Some(a) = address {
                spec["address"] = json!(a);
            }
            if let Some(d) = description {
                spec["description"] = json!(d);
            }
            if let Some(f) = &folder
                && let Some(id) = crate::access::resolve_folder(client, "network", f).await?
            {
                spec["folder"] = json!(id);
            }
            if let Some(t) = &attach {
                spec["target"] = fip_target(client, t).await?;
            }
            let mut body = json!({ "spec": spec });
            if let Some(n) = name {
                body["name"] = json!(n);
            }
            let created = client.post("/floating-ips", &body).await?;
            writeln!(
                out,
                "floating IP {} created ({})",
                cell(&created["name"]),
                cell(&created["spec"]["address"])
            )?;
        }
        FipCmd::Attach { fip, target } => {
            let target = fip_target(client, &target).await?;
            client
                .patch(
                    &format!("/floating-ips/{}", seg(&fip)),
                    &json!({ "target": target }),
                )
                .await?;
            writeln!(out, "floating IP {fip} attached")?;
        }
        FipCmd::Detach { fip } => {
            client
                .patch(
                    &format!("/floating-ips/{}", seg(&fip)),
                    &json!({ "target": null }),
                )
                .await?;
            writeln!(out, "floating IP {fip} detached")?;
        }
        FipCmd::Delete { fip } => {
            client
                .delete(&format!("/floating-ips/{}", seg(&fip)))
                .await?;
            writeln!(out, "floating IP {fip} deleted")?;
        }
    }
    Ok(())
}

/// 지금 `sdn`(없으면 빈 것)에 인자를 적용한 노드 SDN 설정(비면 `{}` — 서버가 지운다)
fn node_sdn_after(current: &Value, args: &NodeSdnArgs) -> anyhow::Result<Value> {
    let mut sdn = if current.is_object() {
        current.clone()
    } else {
        json!({})
    };
    let mut physnets = sdn["physnets"].as_object().cloned().unwrap_or_default();
    for p in &args.physnets {
        let (name, bridge) = p
            .split_once('=')
            .with_context(|| format!("invalid --physnet {p:?}; use NAME=BRIDGE"))?;
        physnets.insert(name.to_string(), json!(bridge));
    }
    for name in &args.no_physnets {
        if physnets.remove(name).is_none() {
            bail!("the node has no physnet {name}");
        }
    }
    let obj = sdn.as_object_mut().context("node sdn is not an object")?;
    if physnets.is_empty() {
        obj.remove("physnets");
    } else {
        obj.insert("physnets".into(), Value::Object(physnets));
    }
    if let Some(g) = args.gateway {
        obj.insert("gateway".into(), json!(g));
    }
    if args.no_gateway {
        obj.remove("gateway");
    }
    Ok(sdn)
}

/// 노드 SDN 설정과 상태 한 줄(`node get`·`node sdn`)
pub(crate) fn node_sdn_line(node: &Value) -> Option<String> {
    let spec = &node["spec"]["sdn"];
    let status = &node["status"]["sdn"];
    let physnets: Vec<String> = arr(&status["physnets"])
        .iter()
        .map(|p| {
            let state = if p["ready"] == true {
                "ready".to_string()
            } else {
                format!("not ready: {}", cell(&p["error"]))
            };
            format!("{}={} ({state})", cell(&p["name"]), cell(&p["bridge"]))
        })
        .collect();
    let physnets = if physnets.is_empty() {
        spec["physnets"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect()
    } else {
        physnets
    };
    if physnets.is_empty() && spec["gateway"].is_null() {
        return None;
    }
    Some(format!(
        "sdn: physnets {}; gateway {}",
        if physnets.is_empty() {
            "-".into()
        } else {
            physnets.join(", ")
        },
        match spec["gateway"].as_u64() {
            Some(p) => format!("priority {p}"),
            None => "no".into(),
        }
    ))
}

pub(crate) async fn node_sdn(
    client: &Client,
    args: &NodeSdnArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/nodes/{}", seg(&args.node));
    let current = client.get(&path).await?;
    let mut node = current.clone();
    if args.given() {
        let mut spec = current["spec"].clone();
        spec["sdn"] = node_sdn_after(&current["spec"]["sdn"], args)?;
        // GET은 빈 레이블을 빼므로 그대로 옮기면 `null`이다.
        let labels = current.get("labels").cloned().unwrap_or_else(|| json!({}));
        let req = client
            .request(Method::PUT, &path)
            .header("if-match", current["revision"].to_string())
            .json(&json!({ "spec": spec, "labels": labels }));
        let (headers, updated) = client.send_with_headers(req).await?;
        warn(err, &headers)?;
        node = updated;
    }
    if json_out {
        return Ok(print_json(out, &node["spec"]["sdn"])?);
    }
    writeln!(
        out,
        "node {}: {}",
        cell(&node["name"]),
        node_sdn_line(&node).unwrap_or_else(|| "sdn: no physnets, not a gateway".into())
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        args: NodeSdnArgs,
    }

    #[test]
    fn external_flags_parse() {
        assert_eq!(parse_external("ext").unwrap(), json!({ "network": "ext" }));
        assert_eq!(
            parse_external("ext,ip=203.0.113.5+2001:db8::5,snat=off").unwrap(),
            json!({ "network": "ext", "ips": ["203.0.113.5", "2001:db8::5"], "snat": false })
        );
        assert!(parse_external("ext,snat=maybe").is_err());
        assert!(parse_external("ext,foo=1").is_err());
        assert!(parse_external(",ip=1.2.3.4").is_err());
    }

    #[test]
    fn node_sdn_flags_change_only_what_is_given() {
        let parse = |argv: &[&str]| {
            let mut all = vec!["t", "n1"];
            all.extend_from_slice(argv);
            T::try_parse_from(all).unwrap().args
        };
        let now = json!({ "physnets": { "ext": "vmbr1" }, "gateway": 200 });
        assert_eq!(
            node_sdn_after(&now, &parse(&["--physnet", "dmz=vmbr2"])).unwrap(),
            json!({ "physnets": { "ext": "vmbr1", "dmz": "vmbr2" }, "gateway": 200 })
        );
        assert_eq!(
            node_sdn_after(&now, &parse(&["--no-physnet", "ext", "--no-gateway"])).unwrap(),
            json!({})
        );
        // `--gateway`만 주면 100
        assert_eq!(
            node_sdn_after(&Value::Null, &parse(&["--gateway"])).unwrap(),
            json!({ "gateway": 100 })
        );
        assert!(node_sdn_after(&now, &parse(&["--no-physnet", "lab"])).is_err());
        assert!(node_sdn_after(&now, &parse(&["--physnet", "ext"])).is_err());
        assert!(!parse(&[]).given());
    }

    #[test]
    fn nics_are_picked_by_index_or_mac() {
        let vm = json!({ "id": "v1", "name": "web", "spec": { "nics": [
            { "mac": "52:54:00:00:00:01" }, { "mac": "52:54:00:00:00:02" }
        ] } });
        assert_eq!(
            pick_nic(&vm, "1").unwrap(),
            json!({ "vm": "v1", "mac": "52:54:00:00:00:02" })
        );
        assert_eq!(
            pick_nic(&vm, "52:54:00:00:00:01").unwrap(),
            json!({ "vm": "v1", "mac": "52:54:00:00:00:01" })
        );
        assert!(pick_nic(&vm, "2").is_err());
    }

    #[test]
    fn rows_show_gateways_and_states() {
        let r = json!({ "name": "r1", "hosting": "n2", "spec": {
            "external": { "network": "ext", "ips": ["203.0.113.50"], "snat": false },
            "interfaces": [{ "network": "t1" }, { "network": "t2" }]
        } });
        assert_eq!(
            router_row(&r),
            ["r1", "ext 203.0.113.50", "off", "t1,t2", "n2"]
        );
        let lonely = json!({ "name": "r2", "gateway_error": "no gateway", "spec": {
            "external": { "network": "ext" }
        } });
        assert_eq!(router_row(&lonely), ["r2", "ext ", "on", "-", "none"]);
        let f = json!({ "name": "203-0-113-120", "vm_name": "web",
            "spec": { "network": "ext", "address": "203.0.113.120",
                      "target": { "vm": "v1", "mac": "52:54:00:00:00:01" } },
            "state": { "kind": "distributed", "node": "n1" } });
        assert_eq!(
            fip_row(&f),
            [
                "203-0-113-120",
                "203.0.113.120",
                "ext",
                "web/52:54:00:00:00:01",
                "distributed n1"
            ]
        );
        let line = node_sdn_line(&json!({
            "spec": { "sdn": { "physnets": { "ext": "vmbr1" }, "gateway": 100 } },
            "status": { "sdn": { "physnets": [{ "name": "ext", "bridge": "vmbr1", "ready": true }] } }
        }))
        .unwrap();
        assert_eq!(
            line,
            "sdn: physnets ext=vmbr1 (ready); gateway priority 100"
        );
        assert_eq!(node_sdn_line(&json!({ "spec": {}, "status": {} })), None);
    }
}
