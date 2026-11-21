//! `orbivisorctl lb …`(계획 67 §7·§14): OVN 부하 분산기. VIP 네트워크는 라우터 인터페이스인 오버레이 SDN 네트워크이고,
//! 멤버는 VM NIC나 주소다. 바꾸는 명령은 부하 분산기를 읽어 고친 뒤 리비전 확인(`If-Match`)으로 `PATCH`한다.

use std::io::Write;

use anyhow::{Context, bail};
use clap::Subcommand;
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, seg};
use crate::output::{cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum LbCmd {
    List,
    /// 부하 분산기와 리스너, 멤버 표(주소·포트·건강·경고)
    Get {
        lb: String,
    },
    /// 부하 분산기를 만든다(네트워크 폴더의 network.config, VIP 네트워크의 network.assign). 멤버는 `lb member add`로 더한다.
    Create {
        name: String,
        /// VIP 네트워크(라우터 인터페이스인 오버레이 SDN 네트워크)
        #[arg(long)]
        network: String,
        /// VIP(IPv4). 없으면 서버가 그 서브넷 풀에서 고른다.
        #[arg(long)]
        vip: Option<String>,
        /// 바깥 VIP `NET[,ip=A]`(VIP 네트워크 라우터의 바깥 공급자 네트워크)
        #[arg(long, value_name = "NET[,ip=A]", value_parser = parse_external)]
        external: Option<Value>,
        /// 리스너 `tcp/80`·`udp/53`(여러 번, 하나 이상)
        #[arg(long = "listener", value_name = "PROTO/PORT", required = true, value_parser = parse_listener)]
        listeners: Vec<(String, u16)>,
        /// 같은 클라이언트를 이 시간(초) 동안 같은 멤버로
        #[arg(long, value_name = "SECS")]
        affinity: Option<u32>,
        #[arg(long)]
        description: Option<String>,
        /// 넣을 네트워크 폴더(경로나 id)
        #[arg(long)]
        folder: Option<String>,
    },
    Delete {
        lb: String,
    },
    /// 리스너의 멤버(VM NIC `VM[:NIC]`나 주소 `IP`, 뒤에 `:PORT` — 없으면 리스너 포트)
    Member {
        #[command(subcommand)]
        cmd: MemberCmd,
    },
    /// 리스너의 건강 검사(OVN 서비스 모니터 — NIC 멤버만 검사한다)
    Health {
        #[command(subcommand)]
        cmd: HealthCmd,
    },
    /// 바깥 VIP(라우터 바깥 공급자 네트워크의 유동 IP 범위에서)
    External {
        #[command(subcommand)]
        cmd: LbExternalCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum MemberCmd {
    /// 멤버를 더한다. VM NIC 멤버는 그 VM의 vm.config가 있어야 한다.
    Add {
        lb: String,
        #[arg(value_name = "PROTO/PORT", value_parser = parse_listener)]
        listener: (String, u16),
        /// `VM[:NIC][@PORT]`(NIC는 순번 0부터나 MAC, 없으면 0)나 `IP[:PORT]`
        #[arg(value_name = "VM[:NIC][@PORT]|IP[:PORT]")]
        member: String,
    },
    Remove {
        lb: String,
        #[arg(value_name = "PROTO/PORT", value_parser = parse_listener)]
        listener: (String, u16),
        #[arg(value_name = "VM[:NIC][@PORT]|IP[:PORT]")]
        member: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum HealthCmd {
    /// 건강 검사를 켜거나 바꾼다(주지 않은 값은 지금 값, 없으면 기본 5초·3초·2·3). `--off`는 끈다.
    Set {
        lb: String,
        #[arg(value_name = "PROTO/PORT", value_parser = parse_listener)]
        listener: (String, u16),
        #[arg(long, value_name = "SECS")]
        interval: Option<u16>,
        #[arg(long, value_name = "SECS")]
        timeout: Option<u16>,
        #[arg(long, value_name = "N")]
        rise: Option<u8>,
        #[arg(long, value_name = "N")]
        fall: Option<u8>,
        #[arg(long, conflicts_with_all = ["interval", "timeout", "rise", "fall"])]
        off: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum LbExternalCmd {
    /// 바깥 VIP를 정한다(ip가 없으면 서버가 고른다).
    Set {
        lb: String,
        #[arg(value_name = "NET[,ip=A]", value_parser = parse_external)]
        external: Value,
    },
    Clear {
        lb: String,
    },
}

/// `tcp/80`의 (프로토콜, 포트)
pub(crate) fn parse_listener(s: &str) -> Result<(String, u16), String> {
    let bad = || format!("invalid listener {s:?}: use tcp/PORT or udp/PORT");
    let (proto, port) = s.split_once('/').ok_or_else(bad)?;
    let proto = proto.to_ascii_lowercase();
    if proto != "tcp" && proto != "udp" {
        return Err(bad());
    }
    let port: u16 = port.parse().map_err(|_| bad())?;
    if port == 0 {
        return Err(bad());
    }
    Ok((proto, port))
}

/// `NET[,ip=A]`의 `LbExternal`
pub(crate) fn parse_external(s: &str) -> Result<Value, String> {
    let bad = |why: &str| format!("invalid external {s:?}: {why}; use NET[,ip=A]");
    let mut parts = s.split(',');
    let network = parts.next().unwrap_or_default();
    if network.is_empty() {
        return Err(bad("no network"));
    }
    let mut v = json!({ "network": network });
    for part in parts {
        match part.split_once('=').ok_or_else(|| bad("missing '='"))? {
            ("ip", ip) => v["ip"] = json!(ip),
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
    if let Some(w) = crate::client::warning_header(headers) {
        writeln!(err, "warning: {w}")?;
    }
    Ok(())
}

const LB_HEADERS: &[&str] = &["NAME", "NETWORK", "VIP", "EXTERNAL", "LISTENERS", "MEMBERS"];
const MEMBER_HEADERS: &[&str] = &["LISTENER", "MEMBER", "ADDRESS", "PORT", "HEALTH", "WARNING"];

/// VIP 상태(`ready`가 아니면 그 까닭과 함께)
fn state_text(s: &Value) -> Option<String> {
    match s["state"].as_str()? {
        "ready" => None,
        other => Some(match s["reason"].as_str() {
            Some(r) => format!("{other}: {r}"),
            None => other.to_string(),
        }),
    }
}

fn listener_names(spec: &Value) -> Vec<String> {
    arr(&spec["listeners"])
        .iter()
        .map(|l| format!("{}/{}", cell(&l["protocol"]), cell(&l["port"])))
        .collect()
}

fn lb_row(l: &Value) -> Vec<String> {
    let s = &l["spec"];
    let members = arr(&l["members"]);
    let up = members.iter().filter(|m| m["health"] == "online").count();
    let checked = members
        .iter()
        .filter(|m| m["health"] != "unchecked")
        .count();
    let mut vip = cell(&s["vip"]);
    if state_text(&l["vip_state"]).is_some() {
        vip.push_str(" (unrouted)");
    }
    vec![
        cell(&l["name"]),
        cell(&s["network"]),
        vip,
        if s["external"].is_object() {
            format!(
                "{} {}",
                cell(&s["external"]["network"]),
                cell(&s["external"]["ip"])
            )
        } else {
            "-".into()
        },
        listener_names(s).join(","),
        if checked == 0 {
            members.len().to_string()
        } else {
            format!("{} ({up}/{checked} up)", members.len())
        },
    ]
}

fn member_row(m: &Value) -> Vec<String> {
    let who = match (m["vm_name"].as_str(), m["vm"].as_str()) {
        (Some(n), _) => format!("{n}/{}", cell(&m["mac"])),
        (None, Some(id)) => format!("{id}/{}", cell(&m["mac"])),
        _ => "address".into(),
    };
    let mut health = cell(&m["health"]);
    if let Some(r) = m["reason"].as_str() {
        health.push_str(&format!(" ({r})"));
    }
    vec![
        cell(&m["listener"]),
        who,
        cell(&m["ip"]),
        cell(&m["port"]),
        health,
        cell(&m["warning"]),
    ]
}

/// `lb get`의 사람용 출력
fn print_lb(
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
    l: &Value,
) -> anyhow::Result<()> {
    let s = &l["spec"];
    writeln!(out, "load balancer {}", cell(&l["name"]))?;
    writeln!(out, "vip: {} on {}", cell(&s["vip"]), cell(&s["network"]))?;
    if let Some(t) = state_text(&l["vip_state"]) {
        writeln!(err, "warning: the VIP is {t}")?;
    }
    if s["external"].is_object() {
        writeln!(
            out,
            "external: {} on {}",
            cell(&s["external"]["ip"]),
            cell(&s["external"]["network"])
        )?;
        if let Some(t) = l.get("external_state").and_then(state_text) {
            writeln!(err, "warning: the external VIP is {t}")?;
        }
    }
    if let Some(a) = s["affinity_secs"].as_u64() {
        writeln!(out, "affinity: {a}s")?;
    }
    for li in arr(&s["listeners"]) {
        let h = &li["health"];
        let health = if h.is_object() {
            format!(
                "health every {}s, timeout {}s, rise {}, fall {}",
                cell(&h["interval_secs"]),
                cell(&h["timeout_secs"]),
                cell(&h["rise"]),
                cell(&h["fall"])
            )
        } else {
            "no health check".into()
        };
        writeln!(
            out,
            "listener {}/{}: {} members, {health}",
            cell(&li["protocol"]),
            cell(&li["port"]),
            arr(&li["members"]).len()
        )?;
    }
    if let Some(e) = l["live_error"].as_str() {
        writeln!(err, "warning: cannot read member health from OVN: {e}")?;
    }
    let rows: Vec<_> = arr(&l["members"]).iter().map(member_row).collect();
    if !rows.is_empty() {
        writeln!(out)?;
        table(out, MEMBER_HEADERS, &rows)?;
    }
    Ok(())
}

/// 부하 분산기를 읽어 `change`(지금 스펙 → `PATCH` 본문)로 고쳐 쓴다(리비전 확인).
async fn patch_lb(
    client: &Client,
    lb: &str,
    change: impl FnOnce(&Value) -> anyhow::Result<Value>,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/load-balancers/{}", seg(lb));
    let current = client.get(&format!("{path}?live=false")).await?;
    let body = change(&current["spec"])?;
    let r = client
        .request(Method::PATCH, &path)
        .header("if-match", cell(&current["revision"]))
        .json(&body);
    let (headers, _) = client.send_with_headers(r).await?;
    warn(err, &headers)
}

/// 리스너 목록에서 (프로토콜, 포트)의 순번
fn listener_index(
    listeners: &[Value],
    lb: &str,
    (proto, port): &(String, u16),
) -> anyhow::Result<usize> {
    listeners
        .iter()
        .position(|l| l["protocol"] == proto.as_str() && l["port"] == *port)
        .with_context(|| format!("load balancer {lb} has no listener {proto}/{port}"))
}

/// 멤버 인자를 스펙의 멤버로: 주소(`IP[:PORT]`)면 그대로, 아니면 `VM[:NIC][@PORT]`의 VM을 읽어 NIC MAC을 찾는다.
async fn member_value(client: &Client, s: &str, default_port: u16) -> anyhow::Result<Value> {
    if let Some((ip, port)) = parse_address_member(s)? {
        return Ok(json!({ "ip": ip, "port": port.unwrap_or(default_port) }));
    }
    let (target, port) = match s.rsplit_once('@') {
        Some((t, p)) => (
            t,
            p.parse::<u16>()
                .with_context(|| format!("invalid member port in {s:?}"))?,
        ),
        None => (s, default_port),
    };
    let (vm, nic) = target.split_once(':').unwrap_or((target, "0"));
    let found = client.get(&format!("/vms/{}", seg(vm))).await?;
    let nics = arr(&found["spec"]["nics"]);
    let picked = match nic.parse::<usize>() {
        Ok(i) => nics.get(i),
        Err(_) => nics.iter().find(|n| {
            n["mac"]
                .as_str()
                .is_some_and(|m| m.eq_ignore_ascii_case(nic))
        }),
    }
    .with_context(|| format!("VM {vm} has no NIC {nic}"))?;
    Ok(json!({ "vm": found["id"], "mac": picked["mac"], "port": port }))
}

/// `IP[:PORT]`(IPv4)면 (주소, 포트)
fn parse_address_member(s: &str) -> anyhow::Result<Option<(std::net::Ipv4Addr, Option<u16>)>> {
    let (ip, port) = s.split_once(':').unwrap_or((s, ""));
    let Ok(ip) = ip.parse::<std::net::Ipv4Addr>() else {
        return Ok(None);
    };
    let port = if port.is_empty() {
        None
    } else {
        Some(
            port.parse::<u16>()
                .with_context(|| format!("invalid member port in {s:?}"))?,
        )
    };
    Ok(Some((ip, port)))
}

/// 같은 멤버인지(VM NIC는 vm·mac, 주소는 ip — 포트도 같아야)
fn same_member(a: &Value, b: &Value) -> bool {
    a["vm"] == b["vm"] && a["mac"] == b["mac"] && a["ip"] == b["ip"] && a["port"] == b["port"]
}

pub(crate) async fn lb(
    client: &Client,
    cmd: LbCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        LbCmd::List => {
            let list = client.get("/load-balancers").await?;
            if json_out {
                return Ok(print_json(out, &list["items"])?);
            }
            if let Some(e) = arr(&list["items"])
                .iter()
                .find_map(|l| l["live_error"].as_str().map(str::to_string))
            {
                writeln!(err, "warning: cannot read member health from OVN: {e}")?;
            }
            let rows: Vec<_> = arr(&list["items"]).iter().map(lb_row).collect();
            table(out, LB_HEADERS, &rows)?;
        }
        LbCmd::Get { lb } => {
            let l = client.get(&format!("/load-balancers/{}", seg(&lb))).await?;
            if json_out {
                return Ok(print_json(out, &l)?);
            }
            print_lb(out, err, &l)?;
        }
        LbCmd::Create {
            name,
            network,
            vip,
            external,
            listeners,
            affinity,
            description,
            folder,
        } => {
            let mut spec = json!({
                "network": network,
                "listeners": listeners
                    .iter()
                    .map(|(p, port)| json!({ "protocol": p, "port": port }))
                    .collect::<Vec<_>>(),
            });
            if let Some(v) = vip {
                spec["vip"] = json!(v);
            }
            if let Some(e) = external {
                spec["external"] = e;
            }
            if let Some(a) = affinity {
                spec["affinity_secs"] = json!(a);
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
                .request(Method::POST, "/load-balancers")
                .json(&json!({ "name": name, "spec": spec }));
            let (headers, created) = client.send_with_headers(r).await?;
            warn(err, &headers)?;
            let mut what = format!("vip {}", cell(&created["spec"]["vip"]));
            if let Some(ip) = created["spec"]["external"]["ip"].as_str() {
                what.push_str(&format!(", external {ip}"));
            }
            writeln!(out, "load balancer {name} created ({what})")?;
        }
        LbCmd::Delete { lb } => {
            client
                .delete(&format!("/load-balancers/{}", seg(&lb)))
                .await?;
            writeln!(out, "load balancer {lb} deleted")?;
        }
        LbCmd::Member { cmd } => {
            let (lb, listener, member, add) = match cmd {
                MemberCmd::Add {
                    lb,
                    listener,
                    member,
                } => (lb, listener, member, true),
                MemberCmd::Remove {
                    lb,
                    listener,
                    member,
                } => (lb, listener, member, false),
            };
            let value = member_value(client, &member, listener.1).await?;
            patch_lb(
                client,
                &lb,
                |spec| {
                    let mut listeners = arr(&spec["listeners"]);
                    let i = listener_index(&listeners, &lb, &listener)?;
                    let mut members = arr(&listeners[i]["members"]);
                    let pos = members.iter().position(|m| same_member(m, &value));
                    match (add, pos) {
                        (true, Some(_)) => bail!(
                            "{member} is already a member of {}/{}",
                            listener.0,
                            listener.1
                        ),
                        (true, None) => members.push(value.clone()),
                        (false, None) => {
                            bail!("{member} is not a member of {}/{}", listener.0, listener.1)
                        }
                        (false, Some(p)) => {
                            members.remove(p);
                        }
                    }
                    listeners[i]["members"] = Value::Array(members);
                    Ok(json!({ "listeners": listeners }))
                },
                err,
            )
            .await?;
            let what = if add { "added to" } else { "removed from" };
            writeln!(
                out,
                "member {member} {what} load balancer {lb} {}/{}",
                listener.0, listener.1
            )?;
        }
        LbCmd::Health {
            cmd:
                HealthCmd::Set {
                    lb,
                    listener,
                    interval,
                    timeout,
                    rise,
                    fall,
                    off,
                },
        } => {
            patch_lb(
                client,
                &lb,
                |spec| {
                    let mut listeners = arr(&spec["listeners"]);
                    let i = listener_index(&listeners, &lb, &listener)?;
                    if off {
                        if let Some(o) = listeners[i].as_object_mut() {
                            o.remove("health");
                        }
                    } else {
                        let now = &listeners[i]["health"];
                        let pick = |v: Option<u64>, key: &str, default: u64| {
                            v.or_else(|| now[key].as_u64()).unwrap_or(default)
                        };
                        let health = json!({
                            "interval_secs": pick(interval.map(u64::from), "interval_secs", 5),
                            "timeout_secs": pick(timeout.map(u64::from), "timeout_secs", 3),
                            "rise": pick(rise.map(u64::from), "rise", 2),
                            "fall": pick(fall.map(u64::from), "fall", 3),
                        });
                        listeners[i]["health"] = health;
                    }
                    Ok(json!({ "listeners": listeners }))
                },
                err,
            )
            .await?;
            let state = if off { "off" } else { "on" };
            writeln!(
                out,
                "load balancer {lb} {}/{}: health check {state}",
                listener.0, listener.1
            )?;
        }
        LbCmd::External { cmd } => {
            let (lb, external) = match cmd {
                LbExternalCmd::Set { lb, external } => (lb, external),
                LbExternalCmd::Clear { lb } => (lb, Value::Null),
            };
            let cleared = external.is_null();
            patch_lb(client, &lb, |_| Ok(json!({ "external": external })), err).await?;
            if cleared {
                writeln!(out, "load balancer {lb}: external VIP removed")?;
            } else {
                let l = client
                    .get(&format!("/load-balancers/{}?live=false", seg(&lb)))
                    .await?;
                writeln!(
                    out,
                    "load balancer {lb}: external VIP {} on {}",
                    cell(&l["spec"]["external"]["ip"]),
                    cell(&l["spec"]["external"]["network"])
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;

    #[test]
    fn listeners_and_externals_parse() {
        assert_eq!(parse_listener("TCP/80").unwrap(), ("tcp".into(), 80));
        assert!(parse_listener("sctp/80").is_err());
        assert!(parse_listener("tcp/0").is_err());
        assert!(parse_listener("80").is_err());
        assert_eq!(
            parse_external("ext,ip=203.0.113.130").unwrap(),
            json!({ "network": "ext", "ip": "203.0.113.130" })
        );
        assert!(parse_external("ext,foo=1").is_err());
        assert_eq!(
            parse_address_member("10.0.0.9:8080").unwrap(),
            Some(("10.0.0.9".parse().unwrap(), Some(8080)))
        );
        assert_eq!(parse_address_member("web:1").unwrap(), None);
    }

    #[test]
    fn rows_show_health_and_reasons() {
        let l = json!({ "name": "web-lb", "spec": {
            "network": "t1", "vip": "10.10.0.50",
            "external": { "network": "ext", "ip": "203.0.113.130" },
            "listeners": [{ "protocol": "tcp", "port": 80 }] },
            "vip_state": { "state": "ready" },
            "members": [
                { "listener": "tcp/80", "vm": "v1", "vm_name": "web1", "mac": "52:54:00:00:00:01",
                  "ip": "10.10.0.11", "port": 80, "health": "online" },
                { "listener": "tcp/80", "ip": "10.10.0.12", "port": 80, "health": "missing",
                  "reason": "VM gone", "warning": "blocked by groups" }
            ] });
        assert_eq!(
            lb_row(&l),
            [
                "web-lb",
                "t1",
                "10.10.0.50",
                "ext 203.0.113.130",
                "tcp/80",
                "2 (1/2 up)"
            ]
        );
        assert_eq!(
            member_row(&arr(&l["members"])[1]),
            [
                "tcp/80",
                "address",
                "10.10.0.12",
                "80",
                "missing (VM gone)",
                "blocked by groups"
            ]
        );
    }

    #[tokio::test]
    async fn create_and_member_add_send_the_spec() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("GET", "/api/v1/vms/web1") => (
                200,
                json!({ "id": "v1", "name": "web1", "spec": { "nics": [{ "mac": "52:54:00:00:00:01" }] } }),
            ),
            ("GET", "/api/v1/load-balancers/web-lb?live=false") => (
                200,
                json!({ "name": "web-lb", "revision": 7, "spec": { "network": "t1",
                    "listeners": [{ "protocol": "tcp", "port": 80 }] } }),
            ),
            ("POST", "/api/v1/load-balancers") => (
                201,
                json!({ "name": "web-lb", "spec": { "vip": "10.10.0.50" } }),
            ),
            _ => (200, json!({})),
        })
        .await;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        lb(
            &client,
            LbCmd::Create {
                name: "web-lb".into(),
                network: "t1".into(),
                vip: None,
                external: None,
                listeners: vec![("tcp".into(), 80)],
                affinity: Some(60),
                description: None,
                folder: None,
            },
            false,
            &mut out,
            &mut err,
        )
        .await
        .unwrap();
        lb(
            &client,
            LbCmd::Member {
                cmd: MemberCmd::Add {
                    lb: "web-lb".into(),
                    listener: ("tcp".into(), 80),
                    member: "web1@8080".into(),
                },
            },
            false,
            &mut out,
            &mut err,
        )
        .await
        .unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen[0].body,
            json!({ "name": "web-lb", "spec": { "network": "t1", "affinity_secs": 60,
                "listeners": [{ "protocol": "tcp", "port": 80 }] } })
        );
        let patch = seen.iter().find(|s| s.line.starts_with("PATCH")).unwrap();
        assert_eq!(patch.if_match.as_deref(), Some("7"));
        assert_eq!(
            patch.body,
            json!({ "listeners": [{ "protocol": "tcp", "port": 80, "members": [
                { "vm": "v1", "mac": "52:54:00:00:00:01", "port": 8080 }] }] })
        );
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("load balancer web-lb created (vip 10.10.0.50)"),
            "{text}"
        );
    }
}
