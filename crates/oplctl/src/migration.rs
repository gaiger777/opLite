//! `orbivisorctl network migrate …`·`network migration …`(계획 67 §9·§14): 단순 네트워크(브리지·VLAN·VXLAN)의 VM NIC를
//! SDN 네트워크로 옮긴다. 미리 보기 계획(`--dry-run`), 시작·멈춤·이어 하기·끝내기·되돌리기는 `network-migrate` Task이고
//! 기본으로 끝날 때까지 기다린다.

use std::io::Write;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::client::{Client, MIGRATE_WAIT, seg};
use crate::output::{cell, json as print_json, table};

/// `network migrate SOURCE --to TARGET …`
#[derive(Args)]
#[command(group(clap::ArgGroup::new("bridging").args(["no_bridge", "bridge_node"])))]
pub(crate) struct MigrateArgs {
    /// 옮길 VM NIC가 있는 단순 네트워크
    source: String,
    /// 옮겨 갈 SDN 네트워크(서브넷이 있음)
    #[arg(long = "to", value_name = "TARGET")]
    target: String,
    /// 옮기기 이름(없으면 서버가 `SOURCE-to-TARGET`, 쓰였으면 `-2` …을 고른다)
    #[arg(long)]
    name: Option<String>,
    /// 옮길 VM(여러 번, 없으면 원본에 NIC가 있는 모든 VM)
    #[arg(long = "vm", value_name = "VM")]
    vms: Vec<String>,
    /// VM마다 방식(auto는 꺼짐 → offline, 켜짐 → live, 안 되면 restart)
    #[arg(long, value_enum, default_value = "auto")]
    mode: MigrationMode,
    /// 옮겨 갈 주소 `VM[:NIC]=A[+B]`(NIC는 순번 0부터, 없으면 원본 네트워크의 첫 NIC)
    #[arg(long = "ip", value_name = "VM[:NIC]=A[+B]")]
    ips: Vec<String>,
    /// 옮기는 동안 두 L2를 잇는 다리를 놓지 않는다.
    #[arg(long)]
    no_bridge: bool,
    /// 다리를 놓을 노드(원본 브리지가 있는 chassis)
    #[arg(long, value_name = "NODE")]
    bridge_node: Option<String>,
    /// 게스트가 볼 MTU가 줄어도 라이브로 옮긴다.
    #[arg(long)]
    allow_mtu_shrink: bool,
    /// 한 번에 옮기는 VM 수(1-8, 기본 1)
    #[arg(long, value_name = "N")]
    parallel: Option<u8>,
    /// 만들지 않고 계획만 보인다.
    #[arg(long)]
    dry_run: bool,
    /// 시작한 Task를 기다리지 않는다.
    #[arg(long)]
    no_wait: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum MigrationMode {
    Auto,
    Offline,
    Live,
    Restart,
}

impl MigrationMode {
    fn as_str(self) -> &'static str {
        match self {
            MigrationMode::Auto => "auto",
            MigrationMode::Offline => "offline",
            MigrationMode::Live => "live",
            MigrationMode::Restart => "restart",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum MigrationCmd {
    List,
    /// 옮기기와 VM NIC마다의 상태
    Get {
        migration: String,
    },
    /// 계획한 옮기기를 시작한다.
    Start {
        migration: String,
        /// 다리 노드를 바꾼다(다리 노드를 지워 멈춘 옮기기).
        #[arg(long, value_name = "NODE")]
        bridge_node: Option<String>,
        #[arg(long)]
        no_wait: bool,
    },
    /// 멈췄거나 실패한 옮기기를 이어 한다.
    Resume {
        migration: String,
        #[arg(long, value_name = "NODE")]
        bridge_node: Option<String>,
        #[arg(long)]
        no_wait: bool,
    },
    /// 지금 옮기는 VM이 끝난 뒤 멈춘다.
    Pause {
        migration: String,
    },
    /// 다리를 걷고 끝낸다. `--router`면 대상 네트워크를 그 라우터 인터페이스로 붙인다.
    Finish {
        migration: String,
        #[arg(long)]
        router: Option<String>,
        #[arg(long)]
        no_wait: bool,
    },
    /// 옮긴 VM을 원본 네트워크로 되돌린다(VM을 주지 않으면 모두).
    Rollback {
        migration: String,
        vms: Vec<String>,
        #[arg(long)]
        no_wait: bool,
    },
    Delete {
        migration: String,
    },
}

fn arr(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

const PLAN_HEADERS: &[&str] = &["VM", "NIC", "MODE", "ADDRESS", "SOURCE", "BLOCKER/WARNING"];
const LIST_HEADERS: &[&str] = &["NAME", "SOURCE", "TARGET", "PHASE", "VMS"];
const MOVE_HEADERS: &[&str] = &["VM", "NIC", "MODE", "ADDRESS", "SOURCE", "STATE", "ERROR"];

fn ips_text(v: &Value) -> String {
    let ips: Vec<String> = arr(v).iter().map(cell).collect();
    if ips.is_empty() {
        "-".into()
    } else {
        ips.join(",")
    }
}

/// 계획 표의 행(막는 까닭이 있으면 그것, 없으면 경고)
fn plan_rows(plan: &Value) -> Vec<Vec<String>> {
    arr(&plan["nics"])
        .iter()
        .map(|n| {
            let blockers: Vec<String> = arr(&n["blockers"]).iter().map(cell).collect();
            let warnings: Vec<String> = arr(&n["warnings"]).iter().map(cell).collect();
            let note = if !blockers.is_empty() {
                format!("blocked: {}", blockers.join("; "))
            } else if !warnings.is_empty() {
                warnings.join("; ")
            } else {
                "-".into()
            };
            vec![
                cell(&n["name"]),
                cell(&n["nic"]),
                cell(&n["mode"]),
                ips_text(&n["ips"]),
                cell(&n["address_source"]),
                note,
            ]
        })
        .collect()
}

/// 다리 줄(`bridge: on node n1`·`bridge: not needed`·`bridge: error …`)
fn bridge_line(plan: &Value, nodes: &[Value]) -> String {
    let b = &plan["bridge"];
    let mut line = if let Some(e) = b["error"].as_str() {
        format!("bridge: cannot lay it: {e}")
    } else if b["needed"] == true {
        match b["node"].as_str() {
            Some(_) => format!("bridge: on node {}", crate::cli::name_of(nodes, &b["node"])),
            None => "bridge: needed".into(),
        }
    } else {
        "bridge: not needed".into()
    };
    for w in arr(&b["warnings"]) {
        line.push_str(&format!("\nwarning: {}", cell(&w)));
    }
    line
}

fn print_plan(out: &mut (dyn Write + Send), plan: &Value, nodes: &[Value]) -> anyhow::Result<()> {
    table(out, PLAN_HEADERS, &plan_rows(plan))?;
    writeln!(out, "{}", bridge_line(plan, nodes))?;
    if let Some(n) = plan["hidden"].as_u64().filter(|n| *n > 0) {
        writeln!(
            out,
            "{n} VM(s) you cannot read are on the source network; they are not moved"
        )?;
    }
    Ok(())
}

fn list_row(m: &Value) -> Vec<String> {
    let vms = arr(&m["status"]["vms"]);
    let moved = vms.iter().filter(|v| v["state"] == "moved").count();
    vec![
        cell(&m["name"]),
        cell(&m["spec"]["source"]),
        cell(&m["spec"]["target"]),
        cell(&m["status"]["phase"]),
        format!("{moved}/{}", vms.len()),
    ]
}

fn move_rows(m: &Value, vms: &[Value]) -> Vec<Vec<String>> {
    arr(&m["status"]["vms"])
        .iter()
        .map(|v| {
            vec![
                crate::cli::name_of(vms, &v["vm"]),
                cell(&v["nic"]),
                cell(&v["mode"]),
                ips_text(&v["ips"]),
                cell(&v["address_source"]),
                cell(&v["state"]),
                cell(&v["error"]),
            ]
        })
        .collect()
}

/// VM 인자(이름이나 id)를 읽는다.
async fn get_vm(client: &Client, vm: &str) -> anyhow::Result<Value> {
    client.get(&format!("/vms/{}", seg(vm))).await
}

/// `VM[:NIC]=A[+B]`의 (VM, NIC 순번, 주소들)
fn parse_ip_arg(s: &str) -> anyhow::Result<(&str, Option<usize>, Vec<String>)> {
    let (target, ips) = s
        .split_once('=')
        .with_context(|| format!("invalid --ip {s:?}; use VM[:NIC]=A[+B]"))?;
    let (vm, nic) = match target.split_once(':') {
        Some((vm, nic)) => (
            vm,
            Some(
                nic.parse::<usize>()
                    .with_context(|| format!("invalid NIC in --ip {s:?}"))?,
            ),
        ),
        None => (target, None),
    };
    let ips: Vec<String> = ips.split('+').map(str::to_string).collect();
    if vm.is_empty() || ips.iter().any(String::is_empty) {
        bail!("invalid --ip {s:?}; use VM[:NIC]=A[+B]");
    }
    Ok((vm, nic, ips))
}

/// 노드 이름이나 id의 id
async fn node_id(client: &Client, node: &str) -> anyhow::Result<Value> {
    Ok(client.get(&format!("/nodes/{}", seg(node))).await?["id"].clone())
}

/// 인자로 옮기기 스펙을 만든다(VM 이름·NIC 순번·노드 이름을 id·MAC으로).
async fn migration_spec(client: &Client, a: &MigrateArgs) -> anyhow::Result<Value> {
    let mut spec = json!({
        "source": a.source,
        "target": a.target,
        "mode": a.mode.as_str(),
        "allow_mtu_shrink": a.allow_mtu_shrink,
    });
    let mut vms = Vec::new();
    for v in &a.vms {
        vms.push(get_vm(client, v).await?["id"].clone());
    }
    if !vms.is_empty() {
        spec["vms"] = json!(vms);
    }
    let mut addresses = Vec::new();
    for s in &a.ips {
        let (vm, nic, ips) = parse_ip_arg(s)?;
        let found = get_vm(client, vm).await?;
        let nics = arr(&found["spec"]["nics"]);
        let picked = match nic {
            Some(i) => nics.get(i),
            None => nics.iter().find(|n| n["network"] == a.source.as_str()),
        }
        .with_context(|| match nic {
            Some(i) => format!("VM {vm} has no NIC {i}"),
            None => format!("VM {vm} has no NIC on network {}", a.source),
        })?;
        addresses.push(json!({ "vm": found["id"], "mac": picked["mac"], "ip": ips }));
    }
    if !addresses.is_empty() {
        spec["addresses"] = json!(addresses);
    }
    if a.no_bridge {
        spec["bridge"] = json!(false);
    }
    if let Some(n) = &a.bridge_node {
        spec["bridge_node"] = node_id(client, n).await?;
    }
    if let Some(p) = a.parallel {
        spec["parallel"] = json!(p);
    }
    Ok(spec)
}

/// 걸음 Task를 기다린다(`--no-wait`면 알리기만). 끝나면 경고를 알린다.
async fn step(
    client: &Client,
    name: &str,
    what: &str,
    task: &Value,
    no_wait: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let id = task["id"].as_str().context("response has no task id")?;
    if no_wait {
        writeln!(
            out,
            "network migration {name}: {what} task {id} started; follow it with `orbivisorctl task get {id}`"
        )?;
        return Ok(());
    }
    let done = client.wait_task_for(task, MIGRATE_WAIT).await?;
    if let Some(w) = done["status"]["warning"].as_str() {
        writeln!(err, "warning: {w}")?;
    }
    let m = client
        .get(&format!("/network-migrations/{}", seg(name)))
        .await?;
    writeln!(
        out,
        "network migration {name}: {what} done ({})",
        cell(&m["status"]["phase"])
    )?;
    Ok(())
}

pub(crate) async fn migrate(
    client: &Client,
    a: MigrateArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let spec = migration_spec(client, &a).await?;
    // 이름을 주지 않으면 서버가 `SOURCE-to-TARGET`(쓰였으면 `-2` …)을 고른다(끝난 기록이 이름을 쥐어도 다시 옮길 수 있게).
    let body = match &a.name {
        Some(name) => json!({ "name": name, "spec": spec }),
        None => json!({ "spec": spec }),
    };
    if a.dry_run {
        let plan = client
            .post("/network-migrations?dry_run=true", &body)
            .await?;
        if json_out {
            return Ok(print_json(out, &plan)?);
        }
        let nodes = client
            .get("/nodes")
            .await
            .map(|v| crate::cli::items(&v))
            .unwrap_or_default();
        return print_plan(out, &plan, &nodes);
    }
    let created = client.post("/network-migrations", &body).await?;
    let name = created["name"]
        .as_str()
        .map(str::to_string)
        .or(a.name.clone())
        .unwrap_or_default();
    if json_out {
        print_json(out, &created)?;
    } else {
        let nodes = client
            .get("/nodes")
            .await
            .map(|v| crate::cli::items(&v))
            .unwrap_or_default();
        print_plan(out, &created["plan"], &nodes)?;
    }
    let task = client
        .post(
            &format!("/network-migrations/{}/start", seg(&name)),
            &json!({}),
        )
        .await?;
    step(client, &name, "start", &task, a.no_wait, out, err).await
}

pub(crate) async fn migration(
    client: &Client,
    cmd: MigrationCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = |m: &str, action: &str| format!("/network-migrations/{}/{action}", seg(m));
    match cmd {
        MigrationCmd::List => {
            let list = client.get("/network-migrations").await?;
            if json_out {
                return Ok(print_json(out, &list["items"])?);
            }
            let rows: Vec<_> = arr(&list["items"]).iter().map(list_row).collect();
            table(out, LIST_HEADERS, &rows)?;
        }
        MigrationCmd::Get { migration } => {
            let m = client
                .get(&format!("/network-migrations/{}", seg(&migration)))
                .await?;
            if json_out {
                return Ok(print_json(out, &m)?);
            }
            let s = &m["status"];
            writeln!(
                out,
                "network migration {}: {} → {} ({})",
                cell(&m["name"]),
                cell(&m["spec"]["source"]),
                cell(&m["spec"]["target"]),
                cell(&s["phase"])
            )?;
            if s["bridge"].is_object() {
                let nodes = client
                    .get("/nodes")
                    .await
                    .map(|v| crate::cli::items(&v))
                    .unwrap_or_default();
                writeln!(
                    out,
                    "bridge: {} on node {}",
                    cell(&s["bridge"]["physnet"]),
                    crate::cli::name_of(&nodes, &s["bridge"]["node"])
                )?;
            }
            if let Some(e) = s["error"].as_str() {
                writeln!(out, "error: {e}")?;
            }
            let vms = crate::cli::items(&client.get("/vms").await?);
            writeln!(out)?;
            table(out, MOVE_HEADERS, &move_rows(&m, &vms))?;
        }
        MigrationCmd::Start {
            migration,
            bridge_node,
            no_wait,
        } => {
            let mut body = json!({});
            if let Some(n) = bridge_node {
                body["bridge_node"] = json!(n);
            }
            let task = client.post(&path(&migration, "start"), &body).await?;
            step(client, &migration, "start", &task, no_wait, out, err).await?;
        }
        MigrationCmd::Resume {
            migration,
            bridge_node,
            no_wait,
        } => {
            let mut body = json!({});
            if let Some(n) = bridge_node {
                body["bridge_node"] = json!(n);
            }
            let task = client.post(&path(&migration, "resume"), &body).await?;
            step(client, &migration, "resume", &task, no_wait, out, err).await?;
        }
        MigrationCmd::Pause { migration } => {
            client.post(&path(&migration, "pause"), &json!({})).await?;
            writeln!(
                out,
                "network migration {migration}: pausing after the VM being moved"
            )?;
        }
        MigrationCmd::Finish {
            migration,
            router,
            no_wait,
        } => {
            let mut body = json!({});
            if let Some(r) = router {
                body["router"] = json!(r);
            }
            let task = client.post(&path(&migration, "finish"), &body).await?;
            step(client, &migration, "finish", &task, no_wait, out, err).await?;
        }
        MigrationCmd::Rollback {
            migration,
            vms,
            no_wait,
        } => {
            let mut body = json!({});
            if !vms.is_empty() {
                body["vms"] = json!(vms);
            }
            let task = client.post(&path(&migration, "rollback"), &body).await?;
            step(client, &migration, "rollback", &task, no_wait, out, err).await?;
        }
        MigrationCmd::Delete { migration } => {
            client
                .delete(&format!("/network-migrations/{}", seg(&migration)))
                .await?;
            writeln!(out, "network migration {migration} deleted")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        args: MigrateArgs,
    }

    #[test]
    fn ip_arguments_parse() {
        assert_eq!(
            parse_ip_arg("web:1=10.0.0.5+fd00::5").unwrap(),
            ("web", Some(1), vec!["10.0.0.5".into(), "fd00::5".into()])
        );
        assert_eq!(
            parse_ip_arg("web=10.0.0.5").unwrap(),
            ("web", None, vec!["10.0.0.5".into()])
        );
        assert!(parse_ip_arg("web").is_err());
        assert!(parse_ip_arg("web:x=1.2.3.4").is_err());
        assert!(
            T::try_parse_from([
                "t",
                "lan",
                "--to",
                "t",
                "--no-bridge",
                "--bridge-node",
                "n1"
            ])
            .is_err()
        );
    }

    #[test]
    fn plan_rows_prefer_blockers() {
        let plan = json!({ "nics": [
            { "vm": "v1", "name": "web", "nic": 0, "mode": "live", "ips": ["10.0.0.5"],
              "address_source": "guest-agent", "blockers": [], "warnings": ["MTU shrinks"] },
            { "vm": "v2", "name": "db", "nic": 1, "mode": "blocked", "ips": [],
              "address_source": "ipam", "blockers": ["a template VM"], "warnings": ["x"] }
        ], "bridge": { "needed": true, "node": "n1id", "warnings": [] } });
        let rows = plan_rows(&plan);
        assert_eq!(
            rows[0],
            ["web", "0", "live", "10.0.0.5", "guest-agent", "MTU shrinks"]
        );
        assert_eq!(rows[1][5], "blocked: a template VM");
        let nodes = vec![json!({ "id": "n1id", "name": "n1" })];
        assert_eq!(bridge_line(&plan, &nodes), "bridge: on node n1");
        // 읽을 수 없는 VM은 수만(리뷰 1 I3)
        let mut text = Vec::new();
        let mut hidden = plan.clone();
        hidden["hidden"] = json!(2);
        print_plan(&mut text, &hidden, &nodes).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(
            text.contains("2 VM(s) you cannot read are on the source network"),
            "{text}"
        );
        let row = list_row(
            &json!({ "name": "m", "spec": { "source": "lan", "target": "t" },
            "status": { "phase": "running", "vms": [{ "state": "moved" }, { "state": "pending" }] } }),
        );
        assert_eq!(row, ["m", "lan", "t", "running", "1/2"]);
    }

    #[tokio::test]
    async fn dry_run_sends_the_resolved_spec() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("GET", "/api/v1/vms/web") => (
                200,
                json!({ "id": "v1", "name": "web", "spec": { "nics": [
                    { "network": "user", "mac": "52:54:00:00:00:09" },
                    { "network": "lan", "mac": "52:54:00:00:00:01" }] } }),
            ),
            ("GET", "/api/v1/nodes/n2") => (200, json!({ "id": "n2id", "name": "n2" })),
            ("GET", "/api/v1/nodes") => (200, json!({ "items": [{ "id": "n2id", "name": "n2" }] })),
            ("POST", "/api/v1/network-migrations?dry_run=true") => (
                200,
                json!({ "nics": [], "bridge": { "needed": true, "node": "n2id", "warnings": [] } }),
            ),
            _ => (200, json!({})),
        })
        .await;
        let args = T::try_parse_from([
            "t",
            "lan",
            "--to",
            "t-lan",
            "--vm",
            "web",
            "--ip",
            "web=10.0.0.5",
            "--mode",
            "live",
            "--bridge-node",
            "n2",
            "--parallel",
            "2",
            "--dry-run",
        ])
        .unwrap()
        .args;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        migrate(&client, args, false, &mut out, &mut err)
            .await
            .unwrap();
        let seen = seen.lock().unwrap().clone();
        let post = seen.iter().find(|s| s.line.starts_with("POST")).unwrap();
        assert_eq!(
            post.body,
            json!({ "spec": {
                "source": "lan", "target": "t-lan", "mode": "live", "allow_mtu_shrink": false,
                "vms": ["v1"],
                "addresses": [{ "vm": "v1", "mac": "52:54:00:00:00:01", "ip": ["10.0.0.5"] }],
                "bridge_node": "n2id", "parallel": 2 } })
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("bridge: on node n2"), "{text}");
    }
}
