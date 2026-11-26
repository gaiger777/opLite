//! `orbivisorctl alert …`(경보 규칙, 활성 경보, 보낸 기록)와 `notify target …`(알림 대상, 시험 보내기). 계획 60 §8·§9.
//!
//! 비밀(SMTP 비밀번호, webhook 토큰·헤더 값, `--url -`의 URL)은 인자로 받지 않고 터미널에서 묻거나(보이지 않게) 표준
//! 입력에서 한 줄씩 읽는다. `notify target set`은 주지 않은 비밀을 보내지 않아 서버가 저장된 값을 지키게 한다(webhook
//! URL은 응답의 줄인 URL을 그대로, 헤더는 빈 값으로 보낸다).

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::client::{ApiError, Client, seg};
use crate::metrics::{duration_ms, now_ms};
use crate::output::{cell, json as print_json, table};

// ---- 공통 ----

/// 기간 인자(`--for`·`--repeat`): 초(단위 없음)나 `30s`·`5m`·`4h`·`1d`
fn parse_secs(s: &str) -> Result<u32, String> {
    let ms = match s.parse::<i64>() {
        Ok(secs) => secs.checked_mul(1000),
        Err(_) => duration_ms(s),
    }
    .ok_or_else(|| format!("{s:?} is not a duration (300, 30s, 5m, 4h, 1d)"))?;
    u32::try_from(ms / 1000).map_err(|_| format!("{s:?} is out of range"))
}

/// ValueEnum 값의 API 이름(`mem-percent` → `mem_percent`)
fn api_name(v: impl ValueEnum) -> String {
    v.to_possible_value()
        .map(|p| p.get_name().replace('-', "_"))
        .unwrap_or_default()
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn ms_time(v: &Value) -> String {
    let Some(ms) = v.as_i64() else {
        return "-".into();
    };
    match time::OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000)) {
        Ok(t) => format!(
            "{}T{:02}:{:02}:{:02}Z",
            t.date(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => ms.to_string(),
    }
}

/// 숫자 칸(소수 둘째 자리까지, 끝의 0은 뗀다)
fn number(v: &Value) -> String {
    match v.as_f64() {
        Some(n) => {
            let s = format!("{n:.2}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
        None => "-".into(),
    }
}

/// 이름 목록 칸(없으면 `-`)
fn names(v: &Value) -> String {
    let list: Vec<&str> = v
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if list.is_empty() {
        "-".into()
    } else {
        list.join(",")
    }
}

/// 응답의 노드 이름 목록(`unreachable` 따위)을 `err`에 알린다.
fn node_warnings(resp: &Value, err: &mut dyn Write, notes: &[(&str, &str)]) -> std::io::Result<()> {
    for (key, what) in notes {
        let list = names(&resp[*key]);
        if list != "-" {
            writeln!(err, "{what}: {}", list.replace(',', ", "))?;
        }
    }
    Ok(())
}

/// 비밀 하나를 묻는다: 터미널이면 보이지 않게 묻고, 아니면 표준 입력에서 한 줄 읽는다. 빈 값은 오류다.
fn ask_secret(what: &str) -> anyhow::Result<String> {
    let value = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("{what}: "))?
    } else {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("cannot read standard input")?;
        line.trim_end_matches(['\r', '\n']).to_string()
    };
    if value.is_empty() {
        bail!("{what} is empty");
    }
    Ok(value)
}

/// 비밀을 얻는 함수(시험은 가짜를 준다)
type Ask<'a> = &'a mut dyn FnMut(&str) -> anyhow::Result<String>;

// ---- 경보 ----

#[derive(Subcommand)]
pub(crate) enum AlertCmd {
    /// 활성 경보(리더 경보와 모든 노드의 노드 경보). firing 먼저, 심각도 높은 것 먼저
    List,
    /// 보낸 알림 기록(모든 노드, 새 것부터)
    History(AlertHistoryArgs),
    /// 경보 규칙(모두 읽고 admin만 바꾼다)
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Args)]
pub(crate) struct AlertHistoryArgs {
    /// 이 시각부터: `30m`·`1h`·`7d`(지금에서 뺌), 유닉스 밀리초나 RFC 3339. 기본은 `--until`의 24시간 전
    #[arg(long)]
    since: Option<String>,
    /// 이 시각까지: `--since`와 같은 형식. 기본은 지금
    #[arg(long)]
    until: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    #[arg(long)]
    limit: Option<u32>,
}

#[derive(Subcommand)]
pub(crate) enum RuleCmd {
    List,
    Get {
        rule: String,
    },
    /// 규칙을 만든다. 예: `alert rule create cpu-hot --node-metric cpu --gt 90 --for 5m --target ops-mail`
    Create(RuleCreate),
    /// 준 것만 바꾼다. `--gt`·`--lt`만 주면 지금 지표 조건의 문턱만 바꾼다.
    Set(RuleSet),
    /// 규칙을 지운다. 그 규칙의 활성 경보는 알림 없이 사라진다.
    Delete {
        rule: String,
    },
    /// 권하는 SDN 경보 규칙 여덟(`sdn-quorum-lost` …)을 한 번에 만든다. 같은 이름의 규칙은 그대로 둔다.
    AddSdnDefaults {
        /// 알림 대상 이름(여러 번 줄 수 있다)
        #[arg(long = "target", value_name = "NAME", required = true)]
        targets: Vec<String>,
    },
    /// 권하는 링크 경보 규칙 둘(`node-link-down`·`bond-degraded`)을 한 번에 만든다. 같은 이름의 규칙은 그대로 둔다.
    AddLinkDefaults {
        /// 알림 대상 이름(여러 번 줄 수 있다)
        #[arg(long = "target", value_name = "NAME", required = true)]
        targets: Vec<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum NodeMetric {
    Cpu,
    MemPercent,
    FsPercent,
    DiskRead,
    DiskWrite,
    NetRx,
    NetTx,
}

#[derive(Clone, Copy, ValueEnum)]
enum VmMetric {
    Cpu,
    MemRss,
    DiskRead,
    DiskWrite,
    DiskReadIops,
    DiskWriteIops,
    NetRx,
    NetTx,
}

#[derive(Clone, Copy, ValueEnum)]
enum SeverityArg {
    Info,
    Warning,
    Critical,
}

/// 조건 하나. 지표 조건은 `--gt`나 `--lt`와 함께 준다(단위: CPU·사용률 %, 메모리 MiB, 디스크·네트워크 바이트/초).
#[derive(Args, Default)]
#[command(group(ArgGroup::new("condition").args([
    "node_metric", "vm_metric", "pool_usage", "node_offline", "vm_failed", "task_failed", "ha_recovery",
    "backup_job_failed", "sdn",
])))]
struct ConditionArgs {
    /// 노드 지표 임계값
    #[arg(long, value_enum)]
    node_metric: Option<NodeMetric>,
    /// VM 지표 임계값(그 VM이 도는 노드가 본다)
    #[arg(long, value_enum)]
    vm_metric: Option<VmMetric>,
    /// 지표가 이 값보다 크면
    #[arg(long, allow_negative_numbers = true, conflicts_with = "lt")]
    gt: Option<f64>,
    /// 지표가 이 값보다 작으면
    #[arg(long, allow_negative_numbers = true)]
    lt: Option<f64>,
    /// 풀 사용률(%)이 이 값보다 크면
    #[arg(long, value_name = "PERCENT")]
    pool_usage: Option<f64>,
    /// 노드가 오프라인이면(리더가 본다)
    #[arg(long)]
    node_offline: bool,
    /// VM이 failed면
    #[arg(long)]
    vm_failed: bool,
    /// Task가 실패하면(이벤트, `--op`로 종류를 고른다)
    #[arg(long)]
    task_failed: bool,
    /// `--task-failed`의 Task 종류(`vm-start` 따위, 여러 번 줄 수 있다. 없으면 모두)
    #[arg(long = "op", value_name = "OP")]
    ops: Vec<String>,
    /// HA 복구가 일어나면(이벤트)
    #[arg(long)]
    ha_recovery: bool,
    /// 백업 작업이 실패하면(이벤트, 리더가 본다)
    #[arg(long)]
    backup_job_failed: bool,
    /// 리더가 보는 조건: 백업(계획 68) backup_verify_failed, backup_store_unavailable, SDN(계획 67) sdn_quorum_lost,
    /// sdn_central_down, sdn_chassis_down, sdn_gateway_unavailable, sdn_gateway_failover(이벤트), sdn_sync_failed,
    /// sdn_physnet_not_ready, sdn_lb_member_down
    #[arg(long = "condition", value_name = "KIND", value_parser = parse_sdn_condition)]
    sdn: Option<String>,
}

/// SDN·백업 저장소 경보 조건의 `kind`(`--condition`)
const SDN_CONDITIONS: &[&str] = &[
    "backup_verify_failed",
    "backup_store_unavailable",
    "sdn_quorum_lost",
    "sdn_central_down",
    "sdn_chassis_down",
    "sdn_gateway_unavailable",
    "sdn_gateway_failover",
    "sdn_sync_failed",
    "sdn_physnet_not_ready",
    "sdn_lb_member_down",
];

fn parse_sdn_condition(s: &str) -> Result<String, String> {
    if SDN_CONDITIONS.contains(&s) {
        Ok(s.to_string())
    } else {
        Err(format!("use one of {}", SDN_CONDITIONS.join(", ")))
    }
}

impl ConditionArgs {
    /// 비교(`gt`·`lt`)와 문턱
    fn compare(&self) -> Option<(&'static str, f64)> {
        match (self.gt, self.lt) {
            (Some(v), _) => Some(("gt", v)),
            (_, Some(v)) => Some(("lt", v)),
            _ => None,
        }
    }

    /// 준 조건(없으면 None)
    fn condition(&self) -> anyhow::Result<Option<Value>> {
        let metric = |kind: &str, metric: String| -> anyhow::Result<Value> {
            let Some((op, value)) = self.compare() else {
                bail!("--{} needs --gt or --lt", kind.replace('_', "-"));
            };
            Ok(json!({ "kind": kind, "metric": metric, "op": op, "value": value }))
        };
        let cond = if let Some(m) = self.node_metric {
            metric("node_metric", api_name(m))?
        } else if let Some(m) = self.vm_metric {
            metric("vm_metric", api_name(m))?
        } else if let Some(percent) = self.pool_usage {
            json!({ "kind": "pool_usage", "percent": percent })
        } else if self.node_offline {
            json!({ "kind": "node_offline" })
        } else if self.vm_failed {
            json!({ "kind": "vm_failed" })
        } else if self.task_failed {
            let mut c = json!({ "kind": "task_failed" });
            if !self.ops.is_empty() {
                c["ops"] = json!(self.ops);
            }
            c
        } else if self.ha_recovery {
            json!({ "kind": "ha_recovery" })
        } else if self.backup_job_failed {
            json!({ "kind": "backup_job_failed" })
        } else if let Some(kind) = &self.sdn {
            json!({ "kind": kind })
        } else {
            return Ok(None);
        };
        if !self.ops.is_empty() && !self.task_failed {
            bail!("--op goes with --task-failed");
        }
        if self.compare().is_some() && cond["metric"].is_null() {
            bail!("--gt and --lt go with --node-metric or --vm-metric");
        }
        Ok(Some(cond))
    }
}

/// 만들기와 바꾸기에 같은 규칙 설정
#[derive(Args, Default)]
struct RuleOpts {
    /// 조건이 이만큼 이어지면 알린다(`0`, `30s`, `5m`; 이벤트는 0)
    #[arg(long = "for", value_name = "DURATION", value_parser = parse_secs)]
    for_secs: Option<u32>,
    #[arg(long, value_enum)]
    severity: Option<SeverityArg>,
    /// 알림 대상 이름(여러 번 줄 수 있다. 바꾸기는 목록을 통째로 바꾼다)
    #[arg(long = "target", value_name = "NAME")]
    targets: Vec<String>,
    /// 계속 firing이면 이 간격으로 다시 알린다(`4h`, `0`이면 다시 알리지 않는다. 기본 4h)
    #[arg(long, value_name = "DURATION", value_parser = parse_secs)]
    repeat: Option<u32>,
    /// 해제(resolved)도 알린다(기본 true)
    #[arg(long, value_name = "true|false")]
    send_resolved: Option<bool>,
    /// 이 노드들만 본다(여러 번 줄 수 있다. 바꾸기는 목록을 통째로 바꾼다)
    #[arg(long = "node", value_name = "NAME")]
    nodes: Vec<String>,
    /// 이 VM들만 본다(여러 번 줄 수 있다. 바꾸기는 목록을 통째로 바꾼다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
}

impl RuleOpts {
    /// spec에 준 값을 넣는다.
    fn apply(&self, spec: &mut Value) {
        if let Some(secs) = self.for_secs {
            spec["for_secs"] = json!(secs);
        }
        if let Some(s) = self.severity {
            spec["severity"] = json!(api_name(s));
        }
        if !self.targets.is_empty() {
            spec["targets"] = json!(self.targets);
        }
        if let Some(secs) = self.repeat {
            spec["repeat_secs"] = json!(secs);
        }
        if let Some(on) = self.send_resolved {
            spec["send_resolved"] = json!(on);
        }
        if !self.nodes.is_empty() {
            spec["nodes"] = json!(self.nodes);
        }
        if !self.vms.is_empty() {
            spec["vms"] = json!(self.vms);
        }
    }
}

#[derive(Args)]
pub(crate) struct RuleCreate {
    name: String,
    #[command(flatten)]
    condition: ConditionArgs,
    #[command(flatten)]
    opts: RuleOpts,
    /// 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
pub(crate) struct RuleSet {
    rule: String,
    #[command(flatten)]
    condition: ConditionArgs,
    #[command(flatten)]
    opts: RuleOpts,
    #[arg(long, conflicts_with = "disable")]
    enable: bool,
    #[arg(long)]
    disable: bool,
    /// 노드 고르기를 지운다(모든 노드).
    #[arg(long, conflicts_with = "nodes")]
    all_nodes: bool,
    /// VM 고르기를 지운다(모든 VM).
    #[arg(long, conflicts_with = "vms")]
    all_vms: bool,
}

/// `alert rule create`의 본문(`POST /alert-rules`). 심각도 기본은 warning이다.
fn rule_create_body(c: &RuleCreate) -> anyhow::Result<Value> {
    let Some(condition) = c.condition.condition()? else {
        bail!(
            "give a condition: --node-metric, --vm-metric, --pool-usage, --node-offline, --vm-failed, \
             --task-failed, --ha-recovery or --backup-job-failed"
        );
    };
    if c.opts.targets.is_empty() {
        bail!("give at least one --target");
    }
    let mut spec = json!({ "condition": condition, "severity": "warning" });
    c.opts.apply(&mut spec);
    if c.disabled {
        spec["enabled"] = json!(false);
    }
    Ok(json!({ "name": c.name, "spec": spec }))
}

/// `alert rule set`: 읽은 규칙의 spec에 준 것만 바꾼다.
fn rule_set_spec(current: &Value, s: &RuleSet) -> anyhow::Result<Value> {
    let mut spec = current["spec"].clone();
    match s.condition.condition()? {
        Some(cond) => spec["condition"] = cond,
        None => {
            if let Some((op, value)) = s.condition.compare() {
                if spec["condition"]["metric"].is_null() {
                    bail!(
                        "the rule's condition is {}; --gt and --lt change only metric conditions",
                        cell(&spec["condition"]["kind"])
                    );
                }
                spec["condition"]["op"] = json!(op);
                spec["condition"]["value"] = json!(value);
            }
            if !s.condition.ops.is_empty() {
                if spec["condition"]["kind"] != "task_failed" {
                    bail!("--op goes with a task_failed condition");
                }
                spec["condition"]["ops"] = json!(s.condition.ops);
            }
        }
    }
    s.opts.apply(&mut spec);
    if s.enable || s.disable {
        spec["enabled"] = json!(s.enable);
    }
    if let Some(spec) = spec.as_object_mut() {
        if s.all_nodes {
            spec.remove("nodes");
        }
        if s.all_vms {
            spec.remove("vms");
        }
    }
    Ok(spec)
}

/// 조건을 짧은 글로: `node_metric cpu > 90`, `pool_usage > 85%`, `task_failed vm-start,vm-stop`
fn condition_text(c: &Value) -> String {
    let kind = cell(&c["kind"]);
    let op = if c["op"] == "lt" { "<" } else { ">" };
    match kind.as_str() {
        "node_metric" | "vm_metric" => {
            format!("{kind} {} {op} {}", cell(&c["metric"]), number(&c["value"]))
        }
        "pool_usage" => format!("pool_usage > {}%", number(&c["percent"])),
        "task_failed" if c["ops"].is_array() => format!("task_failed {}", names(&c["ops"])),
        _ => kind,
    }
}

const RULE_HEADERS: &[&str] = &["NAME", "ENABLED", "SEVERITY", "CONDITION", "FOR", "TARGETS"];

fn rule_row(r: &Value) -> Vec<String> {
    let spec = &r["spec"];
    vec![
        cell(&r["name"]),
        (spec["enabled"] != false).to_string(),
        cell(&spec["severity"]),
        condition_text(&spec["condition"]),
        format!("{}s", spec["for_secs"].as_u64().unwrap_or(0)),
        names(&spec["targets"]),
    ]
}

/// 경보 대상 칸: `node/node-a`, `vm/web@node-a`(노드가 아닌 대상은 그 노드를 붙인다)
fn subject_text(s: &Value) -> String {
    let mut text = format!("{}/{}", cell(&s["kind"]), cell(&s["name"]));
    // 링크 경보(계획 75)는 포트마다: `node/svc01/eno1`
    if let Some(port) = s["port"].as_str() {
        text.push('/');
        text.push_str(port);
    }
    if let Some(node) = s["node"].as_str()
        && s["kind"] != "node"
    {
        text.push('@');
        text.push_str(node);
    }
    text
}

const ALERT_HEADERS: &[&str] = &[
    "STATE",
    "SEVERITY",
    "RULE",
    "SUBJECT",
    "VALUE",
    "THRESHOLD",
    "SINCE",
];

fn alert_row(a: &Value) -> Vec<String> {
    // 조건이 거짓이 돼 해제를 기다리는 경보
    let state = if a["clear_since"].is_null() {
        cell(&a["state"])
    } else {
        "clearing".into()
    };
    vec![
        state,
        cell(&a["severity"]),
        cell(&a["rule_name"]),
        subject_text(&a["subject"]),
        number(&a["value"]),
        number(&a["threshold"]),
        ms_time(&a["since"]),
    ]
}

const HISTORY_HEADERS: &[&str] = &[
    "TIME", "NODE", "STATE", "RULE", "SUBJECT", "TARGET", "RESULT",
];

fn history_row(r: &Value) -> Vec<String> {
    // admin이 아니면 서버가 대상 이름과 오류 문구를 빼고 보낸다.
    let result = match (r["ok"] == true, r["error"].as_str()) {
        (true, _) => "ok".to_string(),
        (false, Some(e)) => format!("failed: {e}"),
        (false, None) => "failed".to_string(),
    };
    vec![
        ms_time(&r["at"]),
        cell(&r["node"]),
        cell(&r["state"]),
        cell(&r["rule"]),
        cell(&r["subject"]),
        cell(&r["target"]),
        result,
    ]
}

/// `alert history`의 질의. 기간이면 지금에서 뺀 유닉스 밀리초로, 아니면 그대로(서버가 RFC 3339도 푼다)
fn history_query(args: &AlertHistoryArgs, now_ms: i64) -> Vec<(&'static str, String)> {
    let time = |s: &String| match duration_ms(s) {
        Some(ms) => (now_ms - ms).to_string(),
        None => s.clone(),
    };
    [
        ("since", args.since.as_ref().map(time)),
        ("until", args.until.as_ref().map(time)),
        ("limit", args.limit.map(|n| n.to_string())),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect()
}

/// 바꾸는 PUT을 `If-Match`로 보내고, 읽은 뒤 status만 바뀌었으면(`revision_mismatch`) 세 번까지 다시 읽어 한다.
async fn put_rule(
    client: &Client,
    path: &str,
    edit: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<Value> {
    let mut tries = 0;
    loop {
        let current = client.get(path).await?;
        let labels = current.get("labels").cloned().unwrap_or_else(|| json!({}));
        let body = json!({ "labels": labels, "spec": edit(&current)? });
        let r = client
            .request(Method::PUT, path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body);
        match client.send(r).await {
            Err(e)
                if tries < 3
                    && e.downcast_ref::<ApiError>()
                        .is_some_and(|a| a.code == "revision_mismatch") =>
            {
                tries += 1;
            }
            other => return other,
        }
    }
}

pub(crate) async fn alert(
    client: &Client,
    cmd: AlertCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    match cmd {
        AlertCmd::List => {
            let resp = client.get("/alerts").await?;
            if json_out {
                print_json(out, &resp)?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = resp["alerts"]
                .as_array()
                .map(|a| a.iter().map(alert_row).collect())
                .unwrap_or_default();
            table(out, ALERT_HEADERS, &rows)?;
            node_warnings(
                &resp,
                err,
                &[
                    ("unreachable", "warning: no alerts from"),
                    (
                        "unsupported",
                        "note: these nodes do not evaluate alerts yet",
                    ),
                    ("overflowing", "warning: too many active alerts on"),
                ],
            )?;
        }
        AlertCmd::History(args) => {
            let q = history_query(&args, now_ms());
            let mut r = client.request(Method::GET, "/alerts/history");
            if !q.is_empty() {
                r = r.query(&q);
            }
            let resp = client.send(r).await?;
            if json_out {
                print_json(out, &resp)?;
                return Ok(());
            }
            let rows: Vec<Vec<String>> = resp["records"]
                .as_array()
                .map(|a| a.iter().map(history_row).collect())
                .unwrap_or_default();
            table(out, HISTORY_HEADERS, &rows)?;
            node_warnings(
                &resp,
                err,
                &[
                    ("unreachable", "warning: no records from"),
                    (
                        "unsupported",
                        "note: these nodes do not send notifications yet",
                    ),
                ],
            )?;
            if resp["truncated"] == true {
                writeln!(
                    err,
                    "note: more records match; narrow the range or raise --limit"
                )?;
            }
        }
        AlertCmd::Rule { cmd } => match cmd {
            RuleCmd::List => {
                let list = client.get("/alert-rules").await?;
                let items = list["items"].as_array().cloned().unwrap_or_default();
                if json_out {
                    print_json(out, &Value::Array(items))?;
                } else {
                    let rows: Vec<_> = items.iter().map(rule_row).collect();
                    table(out, RULE_HEADERS, &rows)?;
                }
            }
            RuleCmd::Get { rule } => {
                print_json(
                    out,
                    &client.get(&format!("/alert-rules/{}", seg(&rule))).await?,
                )?;
            }
            RuleCmd::Create(c) => {
                client.post("/alert-rules", &rule_create_body(&c)?).await?;
                writeln!(out, "alert rule {} created", c.name)?;
            }
            RuleCmd::Set(s) => {
                let path = format!("/alert-rules/{}", seg(&s.rule));
                put_rule(client, &path, |current| rule_set_spec(current, &s)).await?;
                writeln!(out, "alert rule {} updated", s.rule)?;
            }
            RuleCmd::Delete { rule } => {
                client
                    .delete(&format!("/alert-rules/{}", seg(&rule)))
                    .await?;
                writeln!(out, "alert rule {rule} deleted")?;
            }
            RuleCmd::AddSdnDefaults { targets } => {
                add_default_rules(client, "/alert-rules/sdn-defaults", targets, json_out, out)
                    .await?;
            }
            RuleCmd::AddLinkDefaults { targets } => {
                add_default_rules(client, "/alert-rules/link-defaults", targets, json_out, out)
                    .await?;
            }
        },
    }
    Ok(())
}

/// 권하는 묶음(`sdn-defaults`·`link-defaults`)을 대상과 함께 한 번 보내고 만든 것·그대로 둔 것을 줄마다 알린다.
async fn add_default_rules(
    client: &Client,
    path: &str,
    targets: Vec<String>,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let resp = client.post(path, &json!({ "targets": targets })).await?;
    if json_out {
        print_json(out, &resp)?;
    } else {
        for r in resp["created"].as_array().into_iter().flatten() {
            writeln!(out, "alert rule {} created", cell(&r["name"]))?;
        }
        for name in resp["existing"].as_array().into_iter().flatten() {
            writeln!(out, "alert rule {} already exists; kept", cell(name))?;
        }
    }
    Ok(())
}

// ---- 알림 대상 ----

#[derive(Subcommand)]
pub(crate) enum NotifyCmd {
    /// 알림 대상(SMTP·webhook, admin만)
    Target {
        #[command(subcommand)]
        cmd: TargetCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum TargetCmd {
    List,
    /// 설정을 보인다(비밀은 있는지만).
    Get {
        target: String,
    },
    /// 대상을 만든다. 비밀은 묻거나 표준 입력에서 읽는다(`--password`, `--token`, 값 없는 `--header NAME`, `--url -`).
    Create(TargetCreate),
    /// 준 것만 바꾼다. 주지 않은 비밀(비밀번호·토큰·헤더 값·URL)은 서버가 저장된 값을 지킨다(SMTP 서버·계정이나 webhook
    /// 출처·CA가 바뀌면 다시 줘야 한다).
    Set(TargetSet),
    /// 대상을 지운다. 쓰는 규칙이 있으면 거절한다.
    Delete {
        target: String,
    },
    /// 시험 알림을 한 번 보낸다(꺼진 대상도). 실패하면 오류로 끝난다.
    Test {
        target: String,
    },
}

#[derive(Args)]
pub(crate) struct TargetCreate {
    name: String,
    #[command(subcommand)]
    kind: TargetKind,
}

#[derive(Subcommand)]
enum TargetKind {
    /// 메일(SMTP)
    Smtp(SmtpArgs),
    /// HTTP POST(일반 JSON, Slack, Teams, ntfy, Gotify)
    Webhook(WebhookArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum TlsArg {
    Starttls,
    Tls,
    None,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Json,
    Slack,
    Teams,
    Ntfy,
    Gotify,
}

#[derive(Args)]
struct SmtpArgs {
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 587)]
    port: u16,
    #[arg(long, value_enum, default_value = "starttls")]
    tls: TlsArg,
    /// 로그인 사용자(없으면 인증하지 않는다)
    #[arg(long)]
    user: Option<String>,
    /// 비밀번호를 묻는다(터미널) 또는 표준 입력에서 한 줄 읽는다.
    #[arg(long, requires = "user")]
    password: bool,
    /// 보내는 주소
    #[arg(long)]
    from: String,
    /// 받는 주소(여러 번 줄 수 있다, 20개까지)
    #[arg(long = "to", value_name = "ADDRESS", required = true)]
    to: Vec<String>,
    /// 꺼 둔 채로 만든다(시험 보내기는 된다).
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
struct WebhookArgs {
    /// 보낼 URL. `-`면 묻는다(Slack·Teams URL처럼 URL이 비밀일 때)
    #[arg(long)]
    url: String,
    #[arg(long, value_enum, default_value = "json")]
    format: FormatArg,
    /// 보낼 헤더 `NAME=VALUE`. `NAME`만 주면 값을 묻는다(여러 번 줄 수 있다).
    #[arg(long = "header", value_name = "NAME[=VALUE]")]
    headers: Vec<String>,
    /// 토큰을 묻는다(ntfy·gotify)
    #[arg(long)]
    token: bool,
    /// 서버 인증서를 확인할 CA(PEM 파일). 없으면 OS 신뢰 저장소
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// 꺼 둔 채로 만든다(시험 보내기는 된다).
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
pub(crate) struct TargetSet {
    target: String,
    #[arg(long, conflicts_with = "disable")]
    enable: bool,
    #[arg(long)]
    disable: bool,
    /// SMTP 서버
    #[arg(long, help_heading = "SMTP")]
    host: Option<String>,
    #[arg(long, help_heading = "SMTP")]
    port: Option<u16>,
    #[arg(long, value_enum, help_heading = "SMTP")]
    tls: Option<TlsArg>,
    #[arg(long, help_heading = "SMTP", conflicts_with = "no_user")]
    user: Option<String>,
    /// 로그인하지 않는다(비밀번호도 지운다).
    #[arg(long, help_heading = "SMTP")]
    no_user: bool,
    /// 새 비밀번호를 묻는다.
    #[arg(long, help_heading = "SMTP", conflicts_with = "no_password")]
    password: bool,
    /// 저장된 비밀번호를 지운다.
    #[arg(long, help_heading = "SMTP")]
    no_password: bool,
    #[arg(long, help_heading = "SMTP")]
    from: Option<String>,
    /// 받는 주소(목록을 통째로 바꾼다)
    #[arg(long = "to", value_name = "ADDRESS", help_heading = "SMTP")]
    to: Vec<String>,
    /// 새 URL(`-`면 묻는다)
    #[arg(long, help_heading = "Webhook")]
    url: Option<String>,
    #[arg(long, value_enum, help_heading = "Webhook")]
    format: Option<FormatArg>,
    /// 헤더를 더하거나 바꾼다(`NAME=VALUE`, `NAME`만 주면 값을 묻는다).
    #[arg(long = "header", value_name = "NAME[=VALUE]", help_heading = "Webhook")]
    headers: Vec<String>,
    /// 헤더를 지운다.
    #[arg(long = "rm-header", value_name = "NAME", help_heading = "Webhook")]
    remove_headers: Vec<String>,
    /// 새 토큰을 묻는다.
    #[arg(long, help_heading = "Webhook", conflicts_with = "no_token")]
    token: bool,
    /// 저장된 토큰을 지운다.
    #[arg(long, help_heading = "Webhook")]
    no_token: bool,
    /// CA(PEM 파일)를 바꾼다.
    #[arg(long, help_heading = "Webhook", conflicts_with = "no_ca")]
    ca_file: Option<PathBuf>,
    /// CA를 지운다(OS 신뢰 저장소).
    #[arg(long, help_heading = "Webhook")]
    no_ca: bool,
}

impl TargetSet {
    fn smtp_given(&self) -> bool {
        self.host.is_some()
            || self.port.is_some()
            || self.tls.is_some()
            || self.user.is_some()
            || self.no_user
            || self.password
            || self.no_password
            || self.from.is_some()
            || !self.to.is_empty()
    }

    fn webhook_given(&self) -> bool {
        self.url.is_some()
            || self.format.is_some()
            || !self.headers.is_empty()
            || !self.remove_headers.is_empty()
            || self.token
            || self.no_token
            || self.ca_file.is_some()
            || self.no_ca
    }
}

/// `--url`: `-`면 묻는다.
fn url_arg(url: &str, ask: Ask) -> anyhow::Result<String> {
    if url == "-" {
        ask("webhook url")
    } else {
        Ok(url.to_string())
    }
}

/// `--header NAME=VALUE`(또는 `NAME`이면 값을 묻는다)들을 `headers`에 넣는다.
fn put_headers(headers: &mut Map<String, Value>, given: &[String], ask: Ask) -> anyhow::Result<()> {
    for h in given {
        let (name, value) = match h.split_once('=') {
            Some((name, value)) => (name.trim(), value.to_string()),
            None => (h.trim(), ask(&format!("value of header {}", h.trim()))?),
        };
        if name.is_empty() {
            bail!("--header {h:?} has no name");
        }
        headers.insert(name.to_string(), json!(value));
    }
    Ok(())
}

fn read_ca(path: &PathBuf) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
}

/// `notify target create`의 본문(`POST /notification-targets`)
fn target_create_body(c: &TargetCreate, ask: Ask) -> anyhow::Result<Value> {
    let (target, disabled) = match &c.kind {
        TargetKind::Smtp(s) => {
            let mut t = json!({
                "kind": "smtp", "host": s.host, "port": s.port, "tls": api_name(s.tls),
                "from": s.from, "to": s.to,
            });
            if let Some(user) = &s.user {
                t["user"] = json!(user);
            }
            if s.password {
                t["password"] = json!(ask("SMTP password")?);
            }
            (t, s.disabled)
        }
        TargetKind::Webhook(w) => {
            let mut t = json!({
                "kind": "webhook", "url": url_arg(&w.url, ask)?, "format": api_name(w.format),
            });
            let mut headers = Map::new();
            put_headers(&mut headers, &w.headers, ask)?;
            if !headers.is_empty() {
                t["headers"] = Value::Object(headers);
            }
            if w.token {
                t["token"] = json!(ask("webhook token")?);
            }
            if let Some(path) = &w.ca_file {
                t["ca_pem"] = json!(read_ca(path)?);
            }
            (t, w.disabled)
        }
    };
    let mut body = json!({ "name": c.name, "target": target });
    if disabled {
        body["enabled"] = json!(false);
    }
    Ok(body)
}

/// `notify target set`의 본문(`PUT /notification-targets/{key}`): 읽은 대상(비밀 없는 보기)에 준 것만 바꾼다. 주지 않은
/// 비밀은 보내지 않는다(SMTP `password`·webhook `token`을 빼고, 헤더 값은 빈 글, URL은 줄인 URL 그대로).
fn target_set_body(view: &Value, s: &TargetSet, ask: Ask) -> anyhow::Result<Value> {
    let v = &view["target"];
    let kind = cell(&v["kind"]);
    let target = match kind.as_str() {
        "smtp" => {
            if s.webhook_given() {
                bail!(
                    "target {} is smtp; webhook options do not apply",
                    cell(&view["name"])
                );
            }
            let mut t = json!({
                "kind": "smtp",
                "host": s.host.clone().map_or_else(|| v["host"].clone(), Value::from),
                "port": s.port.map_or_else(|| v["port"].clone(), Value::from),
                "tls": s.tls.map_or_else(|| v["tls"].clone(), |t| json!(api_name(t))),
                "from": s.from.clone().map_or_else(|| v["from"].clone(), Value::from),
                "to": if s.to.is_empty() { v["to"].clone() } else { json!(s.to) },
            });
            let user = match (&s.user, s.no_user) {
                (Some(user), _) => json!(user),
                (None, true) => Value::Null,
                (None, false) => v["user"].clone(),
            };
            if !user.is_null() {
                t["user"] = user;
            }
            if s.password {
                t["password"] = json!(ask("SMTP password")?);
            } else if s.no_password || s.no_user {
                t["password"] = json!("");
            }
            t
        }
        "webhook" => {
            if s.smtp_given() {
                bail!(
                    "target {} is a webhook; smtp options do not apply",
                    cell(&view["name"])
                );
            }
            let url = match &s.url {
                Some(url) => json!(url_arg(url, ask)?),
                // 줄인 URL을 그대로 주면 서버가 저장된 URL을 지킨다.
                None => v["url"].clone(),
            };
            let mut t = json!({
                "kind": "webhook",
                "url": url,
                "format": s.format.map_or_else(|| v["format"].clone(), |f| json!(api_name(f))),
            });
            // 빈 값은 저장된 값을 지킨다.
            let mut headers = Map::new();
            for name in v["header_names"].as_array().into_iter().flatten() {
                if let Some(name) = name.as_str()
                    && !s
                        .remove_headers
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(name))
                {
                    headers.insert(name.to_string(), json!(""));
                }
            }
            put_headers(&mut headers, &s.headers, ask)?;
            if !headers.is_empty() {
                t["headers"] = Value::Object(headers);
            }
            if s.token {
                t["token"] = json!(ask("webhook token")?);
            } else if s.no_token {
                t["token"] = json!("");
            }
            let ca = match (&s.ca_file, s.no_ca) {
                (Some(path), _) => json!(read_ca(path)?),
                (None, true) => Value::Null,
                (None, false) => v["ca_pem"].clone(),
            };
            if !ca.is_null() {
                t["ca_pem"] = ca;
            }
            t
        }
        other => bail!("unknown target kind {other:?}"),
    };
    let enabled = if s.enable || s.disable {
        s.enable
    } else {
        view["enabled"] != false
    };
    Ok(json!({ "name": view["name"], "enabled": enabled, "target": target }))
}

const TARGET_HEADERS: &[&str] = &["NAME", "KIND", "ENABLED", "DESTINATION"];

fn target_row(t: &Value) -> Vec<String> {
    let v = &t["target"];
    let destination = match v["kind"].as_str() {
        Some("smtp") => format!(
            "{}:{} -> {}",
            cell(&v["host"]),
            cell(&v["port"]),
            names(&v["to"])
        ),
        _ => format!("{} {}", cell(&v["format"]), cell(&v["url"])),
    };
    vec![
        cell(&t["name"]),
        cell(&v["kind"]),
        (t["enabled"] != false).to_string(),
        destination,
    ]
}

pub(crate) async fn notify(
    client: &Client,
    cmd: NotifyCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let NotifyCmd::Target { cmd } = cmd;
    target(client, cmd, json_out, out, &mut |what| ask_secret(what)).await
}

async fn target(
    client: &Client,
    cmd: TargetCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    ask: Ask<'_>,
) -> anyhow::Result<()> {
    match cmd {
        TargetCmd::List => {
            let list = client.get("/notification-targets").await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &Value::Array(items))?;
            } else {
                let rows: Vec<_> = items.iter().map(target_row).collect();
                table(out, TARGET_HEADERS, &rows)?;
            }
        }
        TargetCmd::Get { target } => print_json(
            out,
            &client
                .get(&format!("/notification-targets/{}", seg(&target)))
                .await?,
        )?,
        TargetCmd::Create(c) => {
            client
                .post("/notification-targets", &target_create_body(&c, ask)?)
                .await?;
            writeln!(out, "notification target {} created", c.name)?;
        }
        TargetCmd::Set(s) => {
            let path = format!("/notification-targets/{}", seg(&s.target));
            let view = client.get(&path).await?;
            let body = target_set_body(&view, &s, ask)?;
            client
                .send(
                    client
                        .request(Method::PUT, &path)
                        .header(reqwest::header::IF_MATCH, cell(&view["revision"]))
                        .json(&body),
                )
                .await?;
            writeln!(out, "notification target {} updated", s.target)?;
        }
        TargetCmd::Delete { target } => {
            client
                .delete(&format!("/notification-targets/{}", seg(&target)))
                .await?;
            writeln!(out, "notification target {target} deleted")?;
        }
        TargetCmd::Test { target } => {
            let resp = client
                .send(client.request(
                    Method::POST,
                    &format!("/notification-targets/{}/test", seg(&target)),
                ))
                .await?;
            if json_out {
                print_json(out, &resp)?;
            }
            if resp["ok"] != true {
                bail!(
                    "test notification to {target} failed: {}",
                    resp["error"].as_str().unwrap_or("unknown error")
                );
            }
            if !json_out {
                writeln!(out, "notification target {target}: test sent")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::fake_api::fake_api;

    /// 시험용 명령줄(`alert`·`notify`만)
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: Top,
    }

    #[derive(Subcommand)]
    enum Top {
        Alert {
            #[command(subcommand)]
            cmd: AlertCmd,
        },
        Notify {
            #[command(subcommand)]
            cmd: NotifyCmd,
        },
    }

    fn alert_cmd(args: &[&str]) -> AlertCmd {
        let mut argv = vec!["t", "alert"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).unwrap().cmd {
            Top::Alert { cmd } => cmd,
            Top::Notify { .. } => unreachable!(),
        }
    }

    fn target_cmd(args: &[&str]) -> TargetCmd {
        let mut argv = vec!["t", "notify", "target"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).unwrap().cmd {
            Top::Notify {
                cmd: NotifyCmd::Target { cmd },
            } => cmd,
            Top::Alert { .. } => unreachable!(),
        }
    }

    fn rule_create(args: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["rule", "create"];
        argv.extend_from_slice(args);
        let AlertCmd::Rule {
            cmd: RuleCmd::Create(c),
        } = alert_cmd(&argv)
        else {
            unreachable!()
        };
        rule_create_body(&c)
    }

    /// 조건 인자마다 API 조건이 되고, 빠진 것·맞지 않는 것은 오류다.
    #[test]
    fn rule_arguments_become_the_spec() {
        assert_eq!(
            rule_create(&[
                "cpu-hot",
                "--node-metric",
                "cpu",
                "--gt",
                "90",
                "--for",
                "5m",
                "--target",
                "ops",
                "--severity",
                "critical",
            ])
            .unwrap(),
            json!({ "name": "cpu-hot", "spec": {
                "condition": { "kind": "node_metric", "metric": "cpu", "op": "gt", "value": 90.0 },
                "severity": "critical", "for_secs": 300, "targets": ["ops"],
            } })
        );
        assert_eq!(
            rule_create(&[
                "rss",
                "--vm-metric",
                "mem-rss",
                "--lt",
                "-1",
                "--target",
                "a",
                "--target",
                "b",
                "--repeat",
                "0",
                "--send-resolved",
                "false",
                "--vm",
                "web",
                "--disabled",
            ])
            .unwrap()["spec"],
            json!({
                "condition": { "kind": "vm_metric", "metric": "mem_rss", "op": "lt", "value": -1.0 },
                "severity": "warning", "targets": ["a", "b"], "repeat_secs": 0,
                "send_resolved": false, "vms": ["web"], "enabled": false,
            })
        );
        assert_eq!(
            rule_create(&[
                "full",
                "--pool-usage",
                "85",
                "--target",
                "a",
                "--node",
                "n1"
            ])
            .unwrap()["spec"]["condition"],
            json!({ "kind": "pool_usage", "percent": 85.0 })
        );
        assert_eq!(
            rule_create(&["t", "--task-failed", "--op", "vm-start", "--target", "a"]).unwrap()["spec"]
                ["condition"],
            json!({ "kind": "task_failed", "ops": ["vm-start"] })
        );
        for (flag, kind) in [
            ("--node-offline", "node_offline"),
            ("--vm-failed", "vm_failed"),
            ("--ha-recovery", "ha_recovery"),
            ("--backup-job-failed", "backup_job_failed"),
        ] {
            assert_eq!(
                rule_create(&["r", flag, "--target", "a"]).unwrap()["spec"]["condition"],
                json!({ "kind": kind })
            );
        }
        // SDN 조건(계획 67)
        assert_eq!(
            rule_create(&[
                "r",
                "--condition",
                "sdn_chassis_down",
                "--for",
                "60",
                "--node",
                "n1",
                "--target",
                "a"
            ])
            .unwrap()["spec"],
            json!({ "condition": { "kind": "sdn_chassis_down" }, "severity": "warning", "for_secs": 60,
                "targets": ["a"], "nodes": ["n1"] })
        );
        assert!(
            Cli::try_parse_from([
                "t",
                "alert",
                "rule",
                "create",
                "r",
                "--condition",
                "sdn_nope",
                "--target",
                "a"
            ])
            .is_err()
        );
        let err = |args: &[&str]| rule_create(args).unwrap_err().to_string();
        assert!(err(&["r", "--node-metric", "cpu", "--target", "a"]).contains("--gt or --lt"));
        assert!(err(&["r", "--target", "a"]).contains("give a condition"));
        assert!(err(&["r", "--node-offline"]).contains("--target"));
        assert!(err(&["r", "--node-offline", "--gt", "1", "--target", "a"]).contains("--gt"));
        assert!(err(&["r", "--vm-failed", "--op", "x", "--target", "a"]).contains("--op"));
        // 조건은 하나만
        assert!(
            Cli::try_parse_from([
                "t",
                "alert",
                "rule",
                "create",
                "r",
                "--node-offline",
                "--vm-failed"
            ])
            .is_err()
        );
        assert_eq!(parse_secs("90"), Ok(90));
        assert_eq!(parse_secs("4h"), Ok(14_400));
        assert!(parse_secs("soon").is_err());
    }

    /// `alert rule set`은 읽은 spec에 준 것만 바꾸고 `If-Match`로 쓰며, `--gt`만 주면 문턱만 바꾼다.
    /// 읽은 뒤 status가 바뀌어 `revision_mismatch`면 다시 읽어 쓴다.
    #[tokio::test]
    async fn rule_set_edits_the_spec_and_retries_a_stale_revision() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let puts = std::sync::Arc::new(AtomicUsize::new(0));
        let p = puts.clone();
        let (client, seen) = fake_api(move |method, _| {
            if method == "PUT" && p.fetch_add(1, Ordering::SeqCst) == 0 {
                return (
                    412,
                    json!({ "code": "revision_mismatch", "message": "stale" }),
                );
            }
            (
                200,
                json!({
                    "id": "r1", "name": "cpu-hot", "revision": 7, "labels": { "team": "ops" },
                    "spec": {
                        "condition": { "kind": "node_metric", "metric": "cpu", "op": "gt", "value": 90.0 },
                        "severity": "warning", "targets": ["ops"], "nodes": ["n1"]
                    },
                    "status": {}
                }),
            )
        })
        .await;
        let cmd = alert_cmd(&[
            "rule",
            "set",
            "cpu-hot",
            "--gt",
            "1000",
            "--disable",
            "--all-nodes",
            "--target",
            "a",
        ]);
        let mut out = Vec::new();
        alert(&client, cmd, false, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "alert rule cpu-hot updated\n"
        );
        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|s| s.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/alert-rules/cpu-hot HTTP/1.1",
                "PUT /api/v1/alert-rules/cpu-hot HTTP/1.1",
                "GET /api/v1/alert-rules/cpu-hot HTTP/1.1",
                "PUT /api/v1/alert-rules/cpu-hot HTTP/1.1",
            ]
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("7"));
        assert_eq!(
            seen[3].body,
            json!({ "labels": { "team": "ops" }, "spec": {
                "condition": { "kind": "node_metric", "metric": "cpu", "op": "gt", "value": 1000.0 },
                "severity": "warning", "targets": ["a"], "enabled": false
            } })
        );

        // 지표 조건이 아니면 문턱만 바꿀 수 없다.
        let current = json!({ "spec": { "condition": { "kind": "node_offline" } } });
        let AlertCmd::Rule {
            cmd: RuleCmd::Set(s),
        } = alert_cmd(&["rule", "set", "x", "--lt", "3"])
        else {
            unreachable!()
        };
        let err = rule_set_spec(&current, &s).unwrap_err().to_string();
        assert!(err.contains("node_offline"), "{err}");
    }

    /// `alert rule add-sdn-defaults`는 대상과 함께 한 번 보내고 만든 것·그대로 둔 것을 줄마다 알린다(계획 67 §11).
    #[tokio::test]
    async fn sdn_defaults_are_one_request() {
        let (client, seen) = fake_api(|_, _| {
            (
                200,
                json!({ "created": [{ "name": "sdn-quorum-lost" }, { "name": "sdn-chassis-down" }],
                        "existing": ["sdn-sync-failed"] }),
            )
        })
        .await;
        assert!(Cli::try_parse_from(["t", "alert", "rule", "add-sdn-defaults"]).is_err());
        let cmd = alert_cmd(&[
            "rule",
            "add-sdn-defaults",
            "--target",
            "ops",
            "--target",
            "pager",
        ]);
        let mut out = Vec::new();
        alert(&client, cmd, false, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "alert rule sdn-quorum-lost created\nalert rule sdn-chassis-down created\n\
             alert rule sdn-sync-failed already exists; kept\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].line,
            "POST /api/v1/alert-rules/sdn-defaults HTTP/1.1"
        );
        assert_eq!(seen[0].body, json!({ "targets": ["ops", "pager"] }));
    }

    /// `alert rule add-link-defaults`도 같은 모양으로 `link-defaults`에 한 번 보낸다(계획 75 §3.4).
    #[tokio::test]
    async fn link_defaults_are_one_request() {
        let (client, seen) = fake_api(|_, _| {
            (
                200,
                json!({ "created": [{ "name": "node-link-down" }], "existing": ["bond-degraded"] }),
            )
        })
        .await;
        assert!(Cli::try_parse_from(["t", "alert", "rule", "add-link-defaults"]).is_err());
        let cmd = alert_cmd(&["rule", "add-link-defaults", "--target", "ops"]);
        let mut out = Vec::new();
        alert(&client, cmd, false, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "alert rule node-link-down created\nalert rule bond-degraded already exists; kept\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].line,
            "POST /api/v1/alert-rules/link-defaults HTTP/1.1"
        );
        assert_eq!(seen[0].body, json!({ "targets": ["ops"] }));
    }

    /// 활성 경보와 보낸 기록은 표로 보이고 빠진 노드·잘림은 stderr에 알린다. `--since 1h`는 밀리초로 바꿔 보낸다.
    #[tokio::test]
    async fn alerts_and_their_history_are_tables() {
        let (client, seen) = fake_api(|_, path| {
            if path.starts_with("/api/v1/alerts/history") {
                (
                    200,
                    json!({
                        "records": [
                            { "at": 1_762_387_260_000i64, "node": "n1", "rule": "cpu-hot", "subject": "n1",
                              "state": "resolved", "target": "ops", "ok": true },
                            { "at": 1_762_387_200_000i64, "node": "n1", "state": "test",
                              "target": "ops", "ok": false, "error": "http://127.0.0.1:9/ answered 500" }
                        ],
                        "unreachable": [], "unsupported": ["n3"], "truncated": true
                    }),
                )
            } else {
                (
                    200,
                    json!({
                        "alerts": [
                            { "rule": "r1", "rule_name": "cpu-hot", "severity": "critical",
                              "condition": "node_metric",
                              "subject": { "kind": "node", "id": "i1", "name": "n1" },
                              "state": "firing", "since": 1_762_387_200_000i64, "value": 97.256,
                              "threshold": 90.0 },
                            { "rule": "r2", "rule_name": "rss", "severity": "info", "condition": "vm_metric",
                              "subject": { "kind": "vm", "id": "i2", "name": "web", "node": "n2" },
                              "state": "firing", "since": 1_762_387_200_000i64,
                              "clear_since": 1_762_387_205_000i64 }
                        ],
                        "unreachable": ["n2"], "unsupported": []
                    }),
                )
            }
        })
        .await;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        alert(&client, alert_cmd(&["list"]), false, &mut out, &mut err)
            .await
            .unwrap();
        alert(
            &client,
            alert_cmd(&["history", "--since", "1h", "--limit", "2"]),
            false,
            &mut out,
            &mut err,
        )
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "STATE     SEVERITY  RULE     SUBJECT    VALUE  THRESHOLD  SINCE\n\
             firing    critical  cpu-hot  node/n1    97.26  90         2025-11-06T00:00:00Z\n\
             clearing  info      rss      vm/web@n2  -      -          2025-11-06T00:00:00Z\n\
             TIME                  NODE  STATE     RULE     SUBJECT  TARGET  RESULT\n\
             2025-11-06T00:01:00Z  n1    resolved  cpu-hot  n1       ops     ok\n\
             2025-11-06T00:00:00Z  n1    test      -        -        ops     failed: http://127.0.0.1:9/ answered 500\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no alerts from: n2\n\
             note: these nodes do not send notifications yet: n3\n\
             note: more records match; narrow the range or raise --limit\n"
        );
        let line = seen.lock().unwrap()[1].line.clone();
        let since: i64 = line
            .split("since=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!((now_ms() - 3_600_000 - since).abs() < 60_000, "{line}");
        assert!(line.ends_with("&limit=2 HTTP/1.1"), "{line}");
    }

    fn create_body(args: &[&str], answers: &[&str]) -> (anyhow::Result<Value>, Vec<String>) {
        let mut argv = vec!["create"];
        argv.extend_from_slice(args);
        let TargetCmd::Create(c) = target_cmd(&argv) else {
            unreachable!()
        };
        let mut asked = Vec::new();
        let mut answers = answers.iter();
        let body = target_create_body(&c, &mut |what| {
            asked.push(what.to_string());
            Ok(answers.next().unwrap().to_string())
        });
        (body, asked)
    }

    /// 대상 만들기: 비밀은 인자가 아니라 묻는 값으로 들어간다.
    #[test]
    fn target_create_asks_for_secrets() {
        let (body, asked) = create_body(
            &[
                "ops-mail",
                "smtp",
                "--host",
                "mail.example",
                "--user",
                "bot",
                "--password",
                "--from",
                "orbivisor@example",
                "--to",
                "a@example",
                "--to",
                "b@example",
            ],
            &["s3cret"],
        );
        assert_eq!(asked, ["SMTP password"]);
        assert_eq!(
            body.unwrap(),
            json!({ "name": "ops-mail", "target": {
                "kind": "smtp", "host": "mail.example", "port": 587, "tls": "starttls", "user": "bot",
                "password": "s3cret", "from": "orbivisor@example", "to": ["a@example", "b@example"]
            } })
        );
        let (body, asked) = create_body(
            &[
                "push",
                "webhook",
                "--url",
                "-",
                "--format",
                "ntfy",
                "--token",
                "--header",
                "X-Plain=1",
                "--header",
                "Authorization",
                "--disabled",
            ],
            &["https://ntfy.example/topic", "Bearer abc", "tk"],
        );
        assert_eq!(
            asked,
            [
                "webhook url",
                "value of header Authorization",
                "webhook token"
            ]
        );
        assert_eq!(
            body.unwrap(),
            json!({ "name": "push", "enabled": false, "target": {
                "kind": "webhook", "url": "https://ntfy.example/topic", "format": "ntfy",
                "headers": { "X-Plain": "1", "Authorization": "Bearer abc" }, "token": "tk"
            } })
        );
        // 비밀번호는 인자로 받지 않는다.
        assert!(
            Cli::try_parse_from([
                "t",
                "notify",
                "target",
                "create",
                "m",
                "smtp",
                "--host",
                "h",
                "--user",
                "u",
                "--password",
                "pw",
                "--from",
                "f",
                "--to",
                "t",
            ])
            .is_err()
        );
    }

    fn set_body(view: &Value, args: &[&str], answers: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["set"];
        argv.extend_from_slice(args);
        let TargetCmd::Set(s) = target_cmd(&argv) else {
            unreachable!()
        };
        let mut answers = answers.iter();
        target_set_body(view, &s, &mut |_| Ok(answers.next().unwrap().to_string()))
    }

    /// 대상 바꾸기는 주지 않은 비밀을 보내지 않는다: SMTP 비밀번호는 빼고, webhook URL은 줄인 URL 그대로, 헤더는 빈 값.
    #[test]
    fn target_set_keeps_the_stored_secrets() {
        let smtp = json!({ "id": "t1", "name": "ops-mail", "revision": 3, "enabled": true, "target": {
            "kind": "smtp", "host": "mail.example", "port": 587, "tls": "starttls", "user": "bot",
            "has_password": true, "from": "orbivisor@example", "to": ["a@example"]
        } });
        assert_eq!(
            set_body(&smtp, &["ops-mail", "--to", "c@example", "--disable"], &[]).unwrap(),
            json!({ "name": "ops-mail", "enabled": false, "target": {
                "kind": "smtp", "host": "mail.example", "port": 587, "tls": "starttls", "user": "bot",
                "from": "orbivisor@example", "to": ["c@example"]
            } })
        );
        assert_eq!(
            set_body(&smtp, &["ops-mail", "--password"], &["new"]).unwrap()["target"]["password"],
            "new"
        );
        let cleared = set_body(&smtp, &["ops-mail", "--no-user"], &[]).unwrap();
        assert_eq!(cleared["target"]["user"], Value::Null);
        assert_eq!(cleared["target"]["password"], "");
        assert!(set_body(&smtp, &["ops-mail", "--token"], &[]).is_err());

        let hook = json!({ "id": "t2", "name": "hook", "revision": 4, "enabled": true, "target": {
            "kind": "webhook", "url": "https://hooks.example/…", "format": "json",
            "header_names": ["Authorization", "X-Old"], "has_token": false, "ca_pem": "PEM"
        } });
        assert_eq!(
            set_body(
                &hook,
                &["hook", "--rm-header", "x-old", "--header", "X-New=2"],
                &[]
            )
            .unwrap(),
            json!({ "name": "hook", "enabled": true, "target": {
                "kind": "webhook", "url": "https://hooks.example/…", "format": "json",
                "headers": { "Authorization": "", "X-New": "2" }, "ca_pem": "PEM"
            } })
        );
        let changed = set_body(
            &hook,
            &[
                "hook", "--url", "-", "--format", "gotify", "--token", "--no-ca",
            ],
            &["https://other.example/x", "tk"],
        )
        .unwrap();
        assert_eq!(changed["target"]["url"], "https://other.example/x");
        assert_eq!(changed["target"]["token"], "tk");
        assert_eq!(changed["target"]["ca_pem"], Value::Null);
        assert!(set_body(&hook, &["hook", "--host", "h"], &[]).is_err());
    }

    /// 대상 명령이 부르는 API. 시험 보내기가 실패하면 오류로 끝난다.
    #[tokio::test]
    async fn target_commands_call_the_api() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("GET", "/api/v1/notification-targets") => (
                200,
                json!({ "items": [
                    { "id": "t1", "name": "ops-mail", "revision": 1, "enabled": true, "target": {
                        "kind": "smtp", "host": "mail.example", "port": 587, "tls": "starttls",
                        "has_password": false, "from": "f@example", "to": ["a@example", "b@example"] } },
                    { "id": "t2", "name": "hook", "revision": 1, "enabled": false, "target": {
                        "kind": "webhook", "url": "https://hooks.example/…", "format": "slack",
                        "header_names": [], "has_token": false } }
                ] }),
            ),
            ("POST", "/api/v1/notification-targets/hook/test") => (
                200,
                json!({ "ok": false, "error": "https://hooks.example/… answered 404" }),
            ),
            ("POST", _) => (200, json!({ "ok": true })),
            ("DELETE", _) => (204, Value::Null),
            _ => (200, json!({})),
        })
        .await;
        let mut out = Vec::new();
        let mut no_ask = |_: &str| -> anyhow::Result<String> { panic!("asked for a secret") };
        for args in [
            &["list"][..],
            &["test", "ops-mail"][..],
            &["delete", "hook"][..],
        ] {
            target(&client, target_cmd(args), false, &mut out, &mut no_ask)
                .await
                .unwrap();
        }
        let err = target(
            &client,
            target_cmd(&["test", "hook"]),
            false,
            &mut out,
            &mut no_ask,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "test notification to hook failed: https://hooks.example/… answered 404"
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME      KIND     ENABLED  DESTINATION\n\
             ops-mail  smtp     true     mail.example:587 -> a@example,b@example\n\
             hook      webhook  false    slack https://hooks.example/…\n\
             notification target ops-mail: test sent\n\
             notification target hook deleted\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/notification-targets HTTP/1.1",
                "POST /api/v1/notification-targets/ops-mail/test HTTP/1.1",
                "DELETE /api/v1/notification-targets/hook HTTP/1.1",
                "POST /api/v1/notification-targets/hook/test HTTP/1.1",
            ]
        );
    }

    /// 링크 경보(계획 75)는 대상에 포트를 단다.
    #[test]
    fn link_alert_subjects_name_the_port() {
        assert_eq!(
            subject_text(
                &json!({ "kind": "node", "id": "i1", "name": "n1", "node": "n1", "port": "eno1" })
            ),
            "node/n1/eno1"
        );
        assert_eq!(
            subject_text(&json!({ "kind": "node", "id": "i1", "name": "n1", "node": "n1" })),
            "node/n1"
        );
    }
}
