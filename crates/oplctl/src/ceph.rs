//! `orbivisorctl ceph …`와 `node disks`: Orbivisor가 노드에 설치·구성하는 Ceph(부트스트랩, mon·mgr·OSD 더하기,
//! 풀 만들기와 등록, 클라이언트 꾸러미, 상태). 바꾸는 명령은 Task를 만들고 기본으로 끝날 때까지 기다린다.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use opl_types::ceph::{CephReport, CephStatusView, DiskState, NodeDisk};
use serde_json::{Value, json};

use crate::client::{CEPH_WAIT, Client, seg};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum CephCmd {
    /// 노드 NODE에 Ceph 꾸러미를 apt로 설치하고 첫 모니터·매니저를 세운다. 실패하면 같은 노드로 다시 부르면 이어서
    /// 한다.
    Init {
        node: String,
        /// 모니터와 클라이언트의 망(CIDR). 없으면 노드 주소가 든 인터페이스의 망
        #[arg(long)]
        public_network: Option<String>,
        /// OSD 복제 트래픽 망(CIDR)
        #[arg(long)]
        cluster_network: Option<String>,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
    /// Ceph 상태(health와 검사, 모니터 정족수, 매니저, OSD, PG, 사용량, 풀)
    Status,
    /// 모니터(3이나 5개 권장)
    Mon {
        #[command(subcommand)]
        cmd: MonCmd,
    },
    /// 매니저(하나가 active, 나머지는 standby)
    Mgr {
        #[command(subcommand)]
        cmd: MgrCmd,
    },
    /// OSD(노드의 디스크 하나에 하나, ceph-volume LVM·BlueStore)
    Osd {
        #[command(subcommand)]
        cmd: OsdCmd,
    },
    /// Ceph 풀을 만들어 Orbivisor RBD 스토리지 풀로 등록한다.
    Pool {
        #[command(subcommand)]
        cmd: CephPoolCmd,
    },
    /// RBD 풀만 쓰는 노드(데몬이 없는 노드)의 Ceph 클라이언트 꾸러미
    Client {
        #[command(subcommand)]
        cmd: CephClientCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum MonCmd {
    /// 노드에 모니터를 더한다(5개까지).
    Add {
        node: String,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum MgrCmd {
    /// 노드에 (standby) 매니저를 더한다.
    Add {
        node: String,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum OsdCmd {
    /// 노드의 디스크(`/dev/sdb`나 `/dev/disk/by-id/…`) 전체에 OSD를 만든다. 쓸 수 있는지는 `node disks NODE`로 본다.
    Add(OsdAdd),
    /// OSD 표(호스트, 장치, 클래스, 크기, 사용량, up·in)
    List,
}

#[derive(Args)]
pub(crate) struct OsdAdd {
    pub(crate) node: String,
    pub(crate) device: String,
    /// 디스크의 파티션 표·파일 시스템·LVM을 지우고 쓴다.
    #[arg(long)]
    pub(crate) wipe: bool,
    /// `--wipe`를 묻지 않고 한다(터미널이 아니면 필요하다).
    #[arg(long)]
    pub(crate) yes: bool,
    /// Task를 기다리지 않는다.
    #[arg(long)]
    pub(crate) no_wait: bool,
}

#[derive(Subcommand)]
pub(crate) enum CephPoolCmd {
    /// Ceph 풀을 만들고 Orbivisor RBD 풀 NAME(사용자 `orbivisor`, 관리 conf)으로 등록한다.
    Create {
        name: String,
        /// Ceph 풀 이름(기본: NAME)
        #[arg(long)]
        ceph_pool: Option<String>,
        /// 복제 수 1~3(기본: min(3, OSD가 있는 호스트 수)). 1은 사본이 하나뿐이다.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=3))]
        size: Option<u8>,
        /// Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum CephClientCmd {
    /// 노드들(`--all`이면 위트니스가 아닌 모든 노드)에 Ceph 클라이언트 꾸러미(`ceph-common`·`qemu-block-extra`)를
    /// 설치한다.
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

/// mon이 짝수 개일 때의 경고
const EVEN_MONS: &str = "an odd number of monitors (3 or 5) keeps quorum best";

/// size 2의 경고
const SIZE_TWO: &str = "size 2 can lose data when one copy is down; 3 is recommended";

/// `ceph status`의 풀 표 열
const POOL_HEADERS: &[&str] = &["POOL", "CEPH-POOL", "SIZE", "STORED", "AVAIL"];

/// `ceph osd list`의 열
const OSD_HEADERS: &[&str] = &["ID", "HOST", "DEVICE", "CLASS", "SIZE", "USED", "UP", "IN"];

/// `node disks`의 열
const DISK_HEADERS: &[&str] = &["DEVICE", "BY-ID", "SIZE", "TYPE", "MODEL", "STATE"];

pub(crate) async fn ceph(
    client: &Client,
    cmd: CephCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        CephCmd::Init {
            node,
            public_network,
            cluster_network,
            no_wait,
        } => {
            let mut body = json!({ "node": node });
            if let Some(net) = public_network {
                body["public_network"] = json!(net);
            }
            if let Some(net) = cluster_network {
                body["cluster_network"] = json!(net);
            }
            let task = client.post("/ceph", &body).await?;
            if no_wait {
                return started(out, &task);
            }
            wait(client, &task, &node, err).await?;
            let view = client.get("/ceph").await?;
            writeln!(
                out,
                "ceph: cluster {} ready (monitor and manager on {node})",
                cell(&view["fsid"])
            )?;
        }
        CephCmd::Status => {
            let view = client.get("/ceph/status").await?;
            if json_out {
                return Ok(print_json(out, &view)?);
            }
            let view: CephStatusView =
                serde_json::from_value(view).context("unexpected Ceph status")?;
            if let Some(warning) = stale_warning(&view, now_secs()) {
                writeln!(err, "{warning}")?;
            }
            for line in status_lines(&view.report) {
                writeln!(out, "{line}")?;
            }
            if !view.report.pools.is_empty() {
                writeln!(out)?;
                table(out, POOL_HEADERS, &pool_rows(&view.report))?;
            }
        }
        CephCmd::Mon {
            cmd: MonCmd::Add { node, no_wait },
        } => {
            // 더한 뒤의 mon 수(읽지 못하면 경고만 건너뛴다 — POST가 오류를 알린다)
            let count = client
                .get("/ceph")
                .await
                .ok()
                .and_then(|v| v["mons"].as_array().map(|m| m.len() + 1));
            let task = client.post("/ceph/mons", &json!({ "node": node })).await?;
            if let Some(warning) = count.and_then(mon_warning) {
                writeln!(err, "warning: {warning}")?;
            }
            if no_wait {
                return started(out, &task);
            }
            wait(client, &task, &node, err).await?;
            writeln!(out, "ceph: monitor on {node} is in quorum")?;
        }
        CephCmd::Mgr {
            cmd: MgrCmd::Add { node, no_wait },
        } => {
            let task = client.post("/ceph/mgrs", &json!({ "node": node })).await?;
            if no_wait {
                return started(out, &task);
            }
            wait(client, &task, &node, err).await?;
            writeln!(out, "ceph: manager on {node} is running")?;
        }
        CephCmd::Osd {
            cmd: OsdCmd::Add(args),
        } => osd_add(client, &args, out, err).await?,
        CephCmd::Osd { cmd: OsdCmd::List } => {
            let view = client.get("/ceph/status").await?;
            if json_out {
                return Ok(print_json(out, &view["osds"])?);
            }
            let view: CephStatusView =
                serde_json::from_value(view).context("unexpected Ceph status")?;
            if let Some(warning) = stale_warning(&view, now_secs()) {
                writeln!(err, "{warning}")?;
            }
            table(out, OSD_HEADERS, &osd_rows(&view.report))?;
        }
        CephCmd::Pool {
            cmd:
                CephPoolCmd::Create {
                    name,
                    ceph_pool,
                    size,
                    no_wait,
                },
        } => {
            let mut body = json!({ "name": name });
            if let Some(pool) = &ceph_pool {
                body["pool"] = json!(pool);
            }
            if let Some(size) = size {
                body["size"] = json!(size);
            }
            let task = client.post("/ceph/pools", &body).await?;
            // 서버가 고른 size(주지 않았으면 min(3, 호스트 수))
            let op = &task["spec"]["op"];
            if op["size"] == 2 {
                writeln!(err, "warning: {SIZE_TWO}")?;
            }
            if no_wait {
                return started(out, &task);
            }
            let nodes = crate::cli::items(&client.get("/nodes").await?);
            let runs_on = crate::cli::name_of(&nodes, &task["spec"]["node"]);
            wait(client, &task, &runs_on, err).await?;
            writeln!(
                out,
                "ceph: storage pool {name} (Ceph pool {}, size {}) is ready",
                cell(&op["pool"]),
                cell(&op["size"])
            )?;
        }
        CephCmd::Client {
            cmd:
                CephClientCmd::Install {
                    nodes,
                    all,
                    no_wait,
                },
        } => {
            let nodes = if all {
                let list = crate::cli::items(&client.get("/nodes").await?);
                client_nodes(&list)
            } else {
                nodes
            };
            if nodes.is_empty() {
                bail!("no node to install the Ceph client packages on");
            }
            // 노드마다 Task를 먼저 모두 만든다(노드들이 함께 설치한다).
            let mut tasks = Vec::new();
            for node in &nodes {
                let task = client
                    .post("/ceph/clients", &json!({ "node": node }))
                    .await?;
                tasks.push((node, task));
            }
            for (node, task) in tasks {
                if no_wait {
                    started(out, &task)?;
                    continue;
                }
                wait(client, &task, node, err).await?;
                writeln!(out, "ceph: client packages installed on {node}")?;
            }
        }
    }
    Ok(())
}

/// `ceph osd add`: 디스크를 확인하고(`--wipe`면 장치 이름을 다시 쳐서) OSD Task를 만들고 기다린 뒤 새 OSD를 알린다.
async fn osd_add(
    client: &Client,
    args: &OsdAdd,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let (node, device) = (args.node.as_str(), args.device.as_str());
    // 노드의 디스크 목록에서 그 디스크(없으면 서버가 400으로 알린다)
    let disks: Vec<NodeDisk> =
        serde_json::from_value(client.get(&format!("/nodes/{}/disks", seg(node))).await?)
            .context("unexpected disk list")?;
    let disk = disks
        .into_iter()
        .find(|d| d.path == device || d.by_id.as_deref() == Some(device));
    if args.wipe && !args.yes {
        use std::io::IsTerminal;
        let disk = wipe_target(node, device, disk.as_ref())?;
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            bail!("--wipe erases all data on {node}:{device}; pass --yes to confirm it");
        }
        write!(err, "{}", wipe_question(node, disk))?;
        err.flush()?;
        let mut answer = String::new();
        stdin.lock().read_line(&mut answer)?;
        if !confirm_wipe(device, disk, &answer) {
            bail!("cancelled");
        }
    }
    let task = client
        .post(
            "/ceph/osds",
            &json!({ "node": node, "device": request_device(device, disk.as_ref()), "wipe": args.wipe }),
        )
        .await?;
    if args.no_wait {
        return started(out, &task);
    }
    wait(client, &task, node, err).await?;
    let path = disk.map_or_else(|| device.to_string(), |d| d.path);
    // 새 OSD는 Ceph에서 읽는다(읽지 못하면 만들었다고만 알린다).
    let report = match client.get("/ceph/status?refresh=true").await {
        Ok(v) => serde_json::from_value::<CephStatusView>(v)
            .ok()
            .map(|v| v.report),
        Err(_) => None,
    };
    let osd = report.as_ref().and_then(|r| new_osd(r, node, &path));
    match osd {
        Some((id, true)) => writeln!(out, "ceph: osd.{id} on {node} {path} is up")?,
        Some((id, false)) => writeln!(out, "ceph: osd.{id} on {node} {path} created (not up yet)")?,
        None => writeln!(out, "ceph: OSD on {node} {path} created")?,
    }
    Ok(())
}

/// `node disks`: 노드의 디스크마다 OSD로 쓸 수 있는지
pub(crate) async fn node_disks(
    client: &Client,
    node: &str,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let list = client.get(&format!("/nodes/{}/disks", seg(node))).await?;
    if json_out {
        return Ok(print_json(out, &list)?);
    }
    let disks: Vec<NodeDisk> = serde_json::from_value(list).context("unexpected disk list")?;
    table(out, DISK_HEADERS, &disk_rows(&disks))?;
    Ok(())
}

/// `--no-wait`: 만든 Task를 알린다.
fn started(out: &mut (dyn Write + Send), task: &Value) -> anyhow::Result<()> {
    let id = task["id"].as_str().context("response has no task id")?;
    writeln!(
        out,
        "ceph: task {id} started; follow it with `orbivisorctl task get {id}`"
    )?;
    Ok(())
}

/// Ceph Task가 끝날 때까지 기다리며 진행 단계가 바뀔 때마다 한 줄을 알린다.
async fn wait(
    client: &Client,
    task: &Value,
    node: &str,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<Value> {
    let mut last = None;
    client
        .wait_task_watch(task, CEPH_WAIT, |t| {
            if let Some(line) = stage_line(&t["status"]["progress"], node, &mut last) {
                // 진행 줄을 쓰지 못해도(stderr가 닫힘) 계속 기다린다.
                let _ = writeln!(err, "{line}");
            }
            Ok(())
        })
        .await
}

/// Task 진행 단계가 지난번과 다르면 알릴 줄(`ceph: installing-packages on node1`)
fn stage_line(progress: &Value, node: &str, last: &mut Option<String>) -> Option<String> {
    let stage = progress["stage"].as_str()?;
    if last.as_deref() == Some(stage) {
        return None;
    }
    *last = Some(stage.to_string());
    Some(format!("ceph: {stage} on {node}"))
}

/// mon이 `count`개가 될 때의 경고(짝수)
fn mon_warning(count: usize) -> Option<&'static str> {
    count.is_multiple_of(2).then_some(EVEN_MONS)
}

/// 지금 읽지 못해 마지막 보고서를 보일 때의 경고
fn stale_warning(view: &CephStatusView, now: u64) -> Option<String> {
    view.stale.then(|| {
        format!(
            "warning: Ceph status from {}s ago: {}",
            now.saturating_sub(view.report.read_at),
            view.error.as_deref().unwrap_or("unknown error")
        )
    })
}

/// 지금 시각(유닉스 초)
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// health 검사의 심각도 표시(`[WRN]`·`[ERR]`)
fn severity_tag(severity: &str) -> &str {
    match severity {
        "HEALTH_WARN" => "WRN",
        "HEALTH_ERR" => "ERR",
        other => other.strip_prefix("HEALTH_").unwrap_or(other),
    }
}

/// `ceph status`의 줄들(풀 표 앞)
fn status_lines(r: &CephReport) -> Vec<String> {
    let mut lines = vec![format!("health: {}", r.health.status)];
    for c in &r.health.checks {
        lines.push(format!(
            "  [{}] {}: {}{}",
            severity_tag(&c.severity),
            c.code,
            c.summary,
            if c.muted { " (muted)" } else { "" }
        ));
    }
    lines.push(format!(
        "mon: {} (quorum {})",
        r.mons.len(),
        if r.quorum.is_empty() {
            "none".to_string()
        } else {
            r.quorum.join(",")
        }
    ));
    let mut mgr = match &r.mgr.active {
        Some(active) => format!("mgr: {active} (active)"),
        None => "mgr: no active manager".to_string(),
    };
    if !r.mgr.standbys.is_empty() {
        mgr.push_str(&format!(", standbys: {}", r.mgr.standbys.join(",")));
    }
    lines.push(mgr);
    let hosts: BTreeSet<&str> = r.osds.iter().map(|o| o.host.as_str()).collect();
    lines.push(format!(
        "osd: {} up, {} in, {} host{}",
        r.osds.iter().filter(|o| o.up).count(),
        r.osds.iter().filter(|o| o.in_).count(),
        hosts.len(),
        if hosts.len() == 1 { "" } else { "s" }
    ));
    let pgs = if r.pgs.states.is_empty() {
        r.pgs.total.to_string()
    } else {
        r.pgs
            .states
            .iter()
            .map(|(state, n)| format!("{n} {state}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    lines.push(format!("pgs: {pgs}"));
    lines.push(format!(
        "usage: {} used, {} / {} available",
        bytes(&r.usage.used_bytes.into()),
        bytes(&r.usage.available_bytes.into()),
        bytes(&r.usage.total_bytes.into())
    ));
    lines
}

/// `ceph status`의 풀 표
fn pool_rows(r: &CephReport) -> Vec<Vec<String>> {
    r.pools
        .iter()
        .map(|p| {
            vec![
                p.orbivisor_pool.clone().unwrap_or_else(|| "-".into()),
                p.name.clone(),
                p.size.map_or_else(|| "-".into(), |s| s.to_string()),
                bytes(&p.stored_bytes.into()),
                bytes(&p.max_available_bytes.into()),
            ]
        })
        .collect()
}

/// `ceph osd list`의 표
fn osd_rows(r: &CephReport) -> Vec<Vec<String>> {
    r.osds
        .iter()
        .map(|o| {
            vec![
                o.id.to_string(),
                o.host.clone(),
                o.device.clone().unwrap_or_else(|| "-".into()),
                o.class.clone().unwrap_or_else(|| "-".into()),
                bytes(&o.total_bytes.into()),
                bytes(&o.used_bytes.into()),
                if o.up { "up" } else { "down" }.into(),
                if o.in_ { "in" } else { "out" }.into(),
            ]
        })
        .collect()
}

/// 디스크 상태 칸(`available`, `wipeable: gpt partition table, ext4 filesystem`, `in use: mounted at /`)
fn disk_state(state: &DiskState) -> String {
    match state {
        DiskState::Available => "available".into(),
        DiskState::Wipeable { reasons } => format!("wipeable: {}", reasons.join(", ")),
        DiskState::InUse { reason } => format!("in use: {reason}"),
    }
}

/// `node disks`의 표
fn disk_rows(disks: &[NodeDisk]) -> Vec<Vec<String>> {
    disks
        .iter()
        .map(|d| {
            vec![
                d.path.clone(),
                d.by_id.as_deref().map_or_else(
                    || "-".into(),
                    |b| b.strip_prefix("/dev/disk/by-id/").unwrap_or(b).to_string(),
                ),
                bytes(&d.size_bytes.into()),
                if d.rotational { "hdd" } else { "ssd" }.into(),
                d.model.clone().unwrap_or_else(|| "-".into()),
                disk_state(&d.state),
            ]
        })
        .collect()
}

/// `--wipe`의 확인 질문
fn wipe_question(node: &str, disk: &NodeDisk) -> String {
    let what = match &disk.state {
        DiskState::Wipeable { reasons } => format!(" ({})", reasons.join(", ")),
        _ => String::new(),
    };
    format!(
        "erase all data on {node}:{}{what}? type the device name to confirm: ",
        disk.path
    )
}

/// 지우기 확인에 친 이름이 그 디스크인지: 준 장치 이름, 디스크 경로(`/dev/sdb`)나 그 이름(`sdb`), by-id 경로
fn confirm_wipe(device: &str, disk: &NodeDisk, input: &str) -> bool {
    let input = input.trim();
    !input.is_empty()
        && (input == device
            || input == disk.path
            || disk.path.strip_prefix("/dev/") == Some(input)
            || disk.by_id.as_deref() == Some(input))
}

/// 지우고 쓸 디스크(묻기 전에): 노드의 디스크 목록에 없으면 무엇을 지울지 보일 수 없으므로 그만둔다.
fn wipe_target<'a>(
    node: &str,
    device: &str,
    disk: Option<&'a NodeDisk>,
) -> anyhow::Result<&'a NodeDisk> {
    disk.ok_or_else(|| {
        anyhow::anyhow!("node {node} has no disk {device}; see `orbivisorctl node disks {node}`")
    })
}

/// 보낼 장치: 목록에서 찾은(확인한) 디스크면 그 by-id 경로(없으면 장치 경로) — 그사이 이름이 바뀌어도 같은 디스크다.
fn request_device(device: &str, disk: Option<&NodeDisk>) -> String {
    disk.map_or_else(
        || device.to_string(),
        |d| d.by_id.clone().unwrap_or_else(|| d.path.clone()),
    )
}

/// 노드 `node`의 장치 `path`에 있는 OSD 가운데 id가 가장 큰 것(새로 만든 것)과 up인지. CRUSH 호스트 이름이 노드
/// 이름과 같은 OSD를 먼저 보고, 그런 것이 없으면(호스트 이름이 다른 노드) 장치만으로 고른다.
fn new_osd(r: &CephReport, node: &str, path: &str) -> Option<(u32, bool)> {
    let on_device: Vec<_> = r
        .osds
        .iter()
        .filter(|o| o.device.as_deref() == Some(path))
        .collect();
    let on_host: Vec<_> = on_device.iter().filter(|o| o.host == node).collect();
    let newest = if on_host.is_empty() {
        on_device.into_iter().max_by_key(|o| o.id)
    } else {
        on_host.into_iter().max_by_key(|o| o.id).copied()
    };
    newest.map(|o| (o.id, o.up))
}

/// `client install --all`의 노드: 위트니스가 아닌 모든 노드(이름)
fn client_nodes(nodes: &[Value]) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| n["spec"]["witness"] != true)
        .filter_map(|n| n["name"].as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use opl_types::ceph::{
        CephHealth, CephHealthCheck, MgrReport, MonReport, OsdReport, PgReport, PoolReport, Usage,
    };

    use super::*;

    fn report() -> CephReport {
        CephReport {
            health: CephHealth {
                status: "HEALTH_WARN".into(),
                checks: vec![
                    CephHealthCheck {
                        code: "TOO_FEW_OSDS".into(),
                        severity: "HEALTH_WARN".into(),
                        summary: "OSD count 1 < osd_pool_default_size 3".into(),
                        count: 1,
                        muted: false,
                    },
                    CephHealthCheck {
                        code: "MON_DOWN".into(),
                        severity: "HEALTH_ERR".into(),
                        summary: "1/3 mons down, quorum node1,node2".into(),
                        count: 1,
                        muted: true,
                    },
                ],
            },
            mons: ["node1", "node2", "node3"]
                .iter()
                .map(|n| MonReport {
                    name: n.to_string(),
                    addr: "10.0.0.1:3300".into(),
                    in_quorum: true,
                })
                .collect(),
            quorum: vec!["node1".into(), "node2".into(), "node3".into()],
            mgr: MgrReport {
                active: Some("node1".into()),
                standbys: vec!["node2".into()],
                available: true,
            },
            osds: vec![
                osd(0, "host-a", true, true, Some("/dev/sdb")),
                osd(1, "host-a", true, true, Some("/dev/sdc")),
                osd(2, "host-b", false, true, None),
                osd(3, "host-b", true, false, Some("/dev/sdb")),
            ],
            osd_hosts_up: 2,
            pgs: PgReport {
                total: 129,
                states: vec![("active+clean".into(), 120), ("peering".into(), 9)],
            },
            usage: Usage {
                total_bytes: 40 << 30,
                used_bytes: 1288490189,
                available_bytes: 38 << 30,
            },
            pools: vec![
                PoolReport {
                    name: ".mgr".into(),
                    id: 1,
                    size: Some(3),
                    stored_bytes: 590368,
                    max_available_bytes: 12 << 30,
                    percent_used: 0.0,
                    orbivisor_pool: None,
                },
                PoolReport {
                    name: "vmpool".into(),
                    id: 2,
                    size: Some(3),
                    stored_bytes: 1 << 30,
                    max_available_bytes: 12 << 30,
                    percent_used: 0.08,
                    orbivisor_pool: Some("vms".into()),
                },
            ],
            read_at: 1_000,
            via: uuid::Uuid::nil(),
        }
    }

    fn osd(id: u32, host: &str, up: bool, in_: bool, device: Option<&str>) -> OsdReport {
        OsdReport {
            id,
            host: host.into(),
            up,
            in_,
            class: Some("hdd".into()),
            weight: 0.0059,
            device: device.map(str::to_string),
            total_bytes: 10 << 30,
            used_bytes: 1 << 20,
        }
    }

    fn disk(path: &str, by_id: Option<&str>, state: DiskState) -> NodeDisk {
        NodeDisk {
            path: path.into(),
            by_id: by_id.map(str::to_string),
            size_bytes: 100 << 30,
            rotational: true,
            model: Some("QEMU HARDDISK".into()),
            serial: None,
            state,
        }
    }

    fn rendered(headers: &[&str], rows: &[Vec<String>]) -> String {
        let mut out = Vec::new();
        table(&mut out, headers, rows).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn status_lines_show_checks_daemons_pgs_and_usage() {
        assert_eq!(
            status_lines(&report()),
            [
                "health: HEALTH_WARN",
                "  [WRN] TOO_FEW_OSDS: OSD count 1 < osd_pool_default_size 3",
                "  [ERR] MON_DOWN: 1/3 mons down, quorum node1,node2 (muted)",
                "mon: 3 (quorum node1,node2,node3)",
                "mgr: node1 (active), standbys: node2",
                "osd: 3 up, 3 in, 2 hosts",
                "pgs: 120 active+clean, 9 peering",
                "usage: 1.2GiB used, 38.0GiB / 40.0GiB available",
            ]
        );
        // 빈 클러스터(mgr 없음, OSD 없음)
        let empty = CephReport {
            health: CephHealth {
                status: "HEALTH_OK".into(),
                checks: Vec::new(),
            },
            ..CephReport::default()
        };
        assert_eq!(
            status_lines(&empty),
            [
                "health: HEALTH_OK",
                "mon: 0 (quorum none)",
                "mgr: no active manager",
                "osd: 0 up, 0 in, 0 hosts",
                "pgs: 0",
                "usage: 0B used, 0B / 0B available",
            ]
        );
    }

    #[test]
    fn stale_status_warns_with_age_and_error() {
        let mut view = CephStatusView {
            report: report(),
            stale: false,
            error: None,
        };
        assert_eq!(stale_warning(&view, 1_030), None);
        view.stale = true;
        view.error = Some("no monitor node is online".into());
        assert_eq!(
            stale_warning(&view, 1_030).as_deref(),
            Some("warning: Ceph status from 30s ago: no monitor node is online")
        );
    }

    #[test]
    fn pool_and_osd_tables() {
        let r = report();
        assert_eq!(
            rendered(POOL_HEADERS, &pool_rows(&r)),
            "POOL  CEPH-POOL  SIZE  STORED    AVAIL\n\
             -     .mgr       3     576.5KiB  12.0GiB\n\
             vms   vmpool     3     1.0GiB    12.0GiB\n"
        );
        assert_eq!(
            rendered(OSD_HEADERS, &osd_rows(&r)),
            "ID  HOST    DEVICE    CLASS  SIZE     USED    UP    IN\n\
             0   host-a  /dev/sdb  hdd    10.0GiB  1.0MiB  up    in\n\
             1   host-a  /dev/sdc  hdd    10.0GiB  1.0MiB  up    in\n\
             2   host-b  -         hdd    10.0GiB  1.0MiB  down  in\n\
             3   host-b  /dev/sdb  hdd    10.0GiB  1.0MiB  up    out\n"
        );
        // 새 OSD는 그 노드(CRUSH 호스트)·장치의 id가 가장 큰 것이다. 호스트 이름이 노드 이름과 다르면 장치만으로.
        assert_eq!(new_osd(&r, "host-a", "/dev/sdb"), Some((0, true)));
        assert_eq!(new_osd(&r, "host-b", "/dev/sdb"), Some((3, true)));
        assert_eq!(new_osd(&r, "node9", "/dev/sdb"), Some((3, true)));
        assert_eq!(new_osd(&r, "host-a", "/dev/sdz"), None);
    }

    #[test]
    fn disk_table_shows_state_and_reasons() {
        let disks = [
            disk(
                "/dev/sdb",
                Some("/dev/disk/by-id/wwn-0x5000c500a1b2c3d4"),
                DiskState::Available,
            ),
            disk(
                "/dev/sdc",
                None,
                DiskState::Wipeable {
                    reasons: vec!["gpt partition table".into(), "ext4 filesystem".into()],
                },
            ),
            NodeDisk {
                rotational: false,
                model: None,
                ..disk(
                    "/dev/nvme0n1",
                    None,
                    DiskState::InUse {
                        reason: "system disk (/ is on it)".into(),
                    },
                )
            },
        ];
        assert_eq!(
            rendered(DISK_HEADERS, &disk_rows(&disks)),
            "DEVICE        BY-ID                   SIZE      TYPE  MODEL          STATE\n\
             /dev/sdb      wwn-0x5000c500a1b2c3d4  100.0GiB  hdd   QEMU HARDDISK  available\n\
             /dev/sdc      -                       100.0GiB  hdd   QEMU HARDDISK  wipeable: gpt partition table, ext4 filesystem\n\
             /dev/nvme0n1  -                       100.0GiB  ssd   -              in use: system disk (/ is on it)\n"
        );
    }

    #[test]
    fn wipe_needs_the_device_name_typed_again() {
        let d = disk(
            "/dev/sdb",
            Some("/dev/disk/by-id/wwn-0x5000c500a1b2c3d4"),
            DiskState::Wipeable {
                reasons: vec!["gpt partition table".into(), "ext4 filesystem".into()],
            },
        );
        assert_eq!(
            wipe_question("node2", &d),
            "erase all data on node2:/dev/sdb (gpt partition table, ext4 filesystem)? type the device name to confirm: "
        );
        for ok in ["/dev/sdb\n", "sdb\n", "  sdb  "] {
            assert!(confirm_wipe("/dev/sdb", &d, ok), "{ok:?}");
        }
        // by-id로 준 장치는 그 이름으로도 받는다.
        assert!(confirm_wipe(
            "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4",
            &d,
            "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4"
        ));
        for bad in ["", "\n", "y", "yes", "/dev/sdc", "sdb1", "/dev/sd"] {
            assert!(!confirm_wipe("/dev/sdb", &d, bad), "{bad:?}");
        }
    }

    /// `--wipe`는 목록에 있는 디스크만 묻는다. 보내는 장치는 확인한 디스크의 by-id(없으면 장치) 경로다.
    #[test]
    fn wipe_targets_a_listed_disk_and_sends_its_by_id_path() {
        let d = disk(
            "/dev/sdb",
            Some("/dev/disk/by-id/wwn-0x5000c500a1b2c3d4"),
            DiskState::Available,
        );
        assert_eq!(wipe_target("node2", "/dev/sdb", Some(&d)).unwrap(), &d);
        assert_eq!(
            wipe_target("node2", "/dev/sdz", None)
                .unwrap_err()
                .to_string(),
            "node node2 has no disk /dev/sdz; see `orbivisorctl node disks node2`"
        );
        assert_eq!(
            request_device("/dev/sdb", Some(&d)),
            "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4"
        );
        let plain = disk("/dev/loop2", None, DiskState::Available);
        assert_eq!(request_device("loop2", Some(&plain)), "/dev/loop2");
        assert_eq!(request_device("/dev/sdz", None), "/dev/sdz");
    }

    #[test]
    fn progress_lines_once_per_stage() {
        let mut last = None;
        let p = |stage: &str| json!({ "stage": stage, "done_bytes": 0, "total_bytes": 0 });
        assert_eq!(
            stage_line(&p("installing-packages"), "node1", &mut last).as_deref(),
            Some("ceph: installing-packages on node1")
        );
        assert_eq!(
            stage_line(&p("installing-packages"), "node1", &mut last),
            None
        );
        assert_eq!(stage_line(&Value::Null, "node1", &mut last), None);
        assert_eq!(
            stage_line(&p("creating-monitor"), "node1", &mut last).as_deref(),
            Some("ceph: creating-monitor on node1")
        );
    }

    #[test]
    fn even_monitor_counts_warn() {
        assert_eq!(mon_warning(1), None);
        assert_eq!(mon_warning(2), Some(EVEN_MONS));
        assert_eq!(mon_warning(3), None);
        assert_eq!(mon_warning(4), Some(EVEN_MONS));
    }

    #[test]
    fn client_install_all_skips_witnesses() {
        let nodes = [
            json!({ "name": "node1", "spec": { "witness": false } }),
            json!({ "name": "w", "spec": { "witness": true } }),
            json!({ "name": "node2", "spec": {} }),
        ];
        assert_eq!(client_nodes(&nodes), ["node1", "node2"]);
    }
}
