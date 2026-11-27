//! `orbivisorctl dr …`(계획 76 설계 §4): 복구 계획 만들기·보기·목록·바꾸기·지우기(YAML·JSON 파일 `-f`), 시험 장애 조치·
//! 장애 조치(planned·disaster)·뒷정리·취소, 실행 기록과 실행 하나(`--watch`), RPO/RTO 보고, 되돌리기용 백업 작업(reprotect).
//!
//! 응답은 `Value`로 읽는다(새 서버가 더한 칸은 지나친다). 끝났는지 가르는 실행 상태만 [`RunState`]로 읽고, 이 빌드가
//! 모르는 값은 `Unknown`이다.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::{Day, KeepArgs, days_value, keep_value, unix_time};
use crate::client::{ApiError, BACKUP_WAIT, Client, NotLoggedIn, seg};
use crate::output::{cell, json as print_json, table};

/// 실행을 기다리며 다시 읽는 간격(리더의 DR 스케줄러 주기와 같다)
const POLL_EVERY: Duration = Duration::from_secs(5);

#[derive(Subcommand)]
pub(crate) enum DrCmd {
    /// 복구 계획(`cluster.admin`이 바꾼다): 만들기·보기·목록·바꾸기·지우기
    Plan {
        #[command(subcommand)]
        cmd: PlanCmd,
    },
    /// 시험 장애 조치: 운영 쪽을 건드리지 않고 시험 VM(`<vm>-drt`)을 격리망에서 그룹 차례로 되살린다. 끝나면 `dr cleanup`.
    Test {
        plan: String,
        /// 실행이 끝날 때까지 기다린다(단계가 바뀔 때마다 stderr에 한 줄, 끝에 실행 표).
        #[arg(long)]
        wait: bool,
        #[command(flatten)]
        points: PointArgs,
        /// 시험망이 라우터·업링크가 있는(밖과 닿을 수 있는) 네트워크여도 진행한다(시험 VM은 운영 주소·이름으로 뜬다).
        #[arg(long)]
        allow_routed_test_networks: bool,
    },
    /// 실제 장애 조치: 원본 이름으로 VM을 되살린다. `planned`는 운영 쪽 VM을 끄고 마지막 백업을 받아 넘기고, `disaster`는
    /// 지금 있는 가장 새 사본으로 곧장 한다.
    Failover {
        plan: String,
        #[arg(long, value_enum)]
        mode: FailoverMode,
        /// 확인: 계획 이름을 그대로 다시 친다.
        #[arg(long, value_name = "PLAN")]
        confirm: String,
        #[arg(long)]
        wait: bool,
        /// (disaster만)
        #[command(flatten)]
        points: PointArgs,
    },
    /// 지난 시험이 남긴 시험 VM(끄고 지움)과 거품 네트워크를 지운다. 장애 조치로 만든 VM은 건드리지 않는다.
    Cleanup {
        plan: String,
        #[arg(long)]
        wait: bool,
    },
    /// 도는 실행을 지금 단계 뒤에 멈춘다(진행 중인 Task는 끝까지, 시험이면 그 뒤 뒷정리).
    Cancel { plan: String },
    /// 계획의 실행 기록(새것부터, 최근 20개)
    Runs { plan: String },
    /// 실행 하나(이름 또는 id): 그룹·VM·복구 지점·단계·새 VM·Task·오류 표
    Run {
        run: String,
        /// 끝날 때까지 바뀔 때마다 다시 찍는다(`-o json`이면 바뀔 때마다 한 줄). 끝이 `succeeded`가 아니면 끝 코드 1.
        #[arg(long)]
        watch: bool,
    },
    /// RPO/RTO 보고: VM마다 복구 지점과 나이, 최근 시험 셋, 지난 장애 조치, 도는 실행
    Report {
        plan: String,
        /// 이 시각(RFC 3339) 이하의 사본으로 보호 상태·장애 조치 준비를 센다(`--point-in-time`으로 돌리기 전에 볼 때)
        #[arg(long, value_name = "TIME", value_parser = crate::vmware::parse_time)]
        point_in_time: Option<u64>,
    },
    /// 되돌리기 준비: 장애 조치로 되살린 VM에 원격(되살아난 운영 클러스터)으로 복제하는 백업 작업
    /// `dr-reprotect-<plan>`을 만들거나 바꾼다.
    Reprotect(ReprotectArgs),
    /// 재해 장애 조치 뒤 돌아온 운영 쪽에서 이곳에 되살린 VM을 강제로 끄고 원하는 상태를 stopped로 둔다(두 벌 막기). 장애
    /// 조치 때의 운영 쪽 VM(id)만 끄고 같은 이름의 다른 VM은 건너뛴다. 멈춘 VM은 계획의 `source_fence_pending` 목록에서 뺀다.
    FenceSource { plan: String },
    /// 두 벌 막기 목록(`source_fence_pending`)을 운영 쪽에 아무것도 하지 않고 비운다 — 운영 쪽 옛 VM이 멈췄거나 없음을
    /// 스스로 확인했을 때(되돌리기 앞에 지움, 원격을 새 클러스터로 바꿈 등).
    ClearFence { plan: String },
}

/// 복구 지점 고르기(시험·재해 장애 조치)
#[derive(Args)]
pub(crate) struct PointArgs {
    /// 복구 지점이 없으면 이 VM을 건너뛰고 진행한다(여러 번 줄 수 있다 — 여기 없는 VM에 복구 지점이 없으면 거절).
    #[arg(long = "skip-vm", value_name = "VM")]
    skip_vms: Vec<String>,
    /// 복구 지점이 없는 VM은 모두 건너뛰고 진행한다(`--skip-vm`으로 고르는 편이 안전하다).
    #[arg(long)]
    allow_missing: bool,
    /// 이 시각(RFC 3339) 이전의 가장 새 복구 지점으로(랜섬웨어·논리 손상이 복제된 경우)
    #[arg(long, value_name = "TIME", value_parser = crate::vmware::parse_time)]
    point_in_time: Option<u64>,
}

impl PointArgs {
    fn fill(&self, body: &mut Value) {
        if self.allow_missing {
            body["allow_missing"] = json!(true);
        }
        if !self.skip_vms.is_empty() {
            body["skip_vms"] = json!(self.skip_vms);
        }
        if let Some(t) = self.point_in_time {
            body["point_in_time"] = json!(t);
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum FailoverMode {
    /// 운영 쪽이 살아 있음: 운영 VM을 끄고 마지막 백업을 이 클러스터로 보낸 뒤 넘긴다.
    Planned,
    /// 운영 쪽이 없음: 지금 있는 가장 새 사본으로 곧장
    Disaster,
}

#[derive(Subcommand)]
pub(crate) enum PlanCmd {
    /// 계획을 만든다. `-f`는 계획 파일(YAML·JSON, `-`면 표준 입력 — spec 그대로나 `{labels, spec}`, `plan get -o json`의
    /// 출력도 된다). 파일 없이는 `--source --pool --group`으로 기본값 계획을 만든다.
    Create(CreateArgs),
    /// 계획(머리 줄과 그룹·VM 표, `-o json`이면 리소스 그대로)
    Get { plan: String },
    /// `NAME SOURCE VMS RPO-OK LAST-TEST-RTO STATE`
    List,
    /// 계획을 파일로 통째로 바꾼다(`If-Match` — 파일에 `revision`이 있으면 그것, 없으면 지금 계획을 읽은 것). 파일에
    /// `labels`가 없으면 지금 라벨을 그대로 둔다. 도는 실행은 시작한 때의 계획으로 간다.
    Set {
        plan: String,
        #[arg(short = 'f', long = "file", value_name = "FILE")]
        file: PathBuf,
    },
    /// 계획과 그 실행 기록을 지운다(실행이 만든 VM·네트워크는 그대로).
    Delete { plan: String },
}

#[derive(Args)]
pub(crate) struct CreateArgs {
    name: String,
    /// 계획 파일(YAML·JSON, `-`면 표준 입력)
    #[arg(short = 'f', long = "file", value_name = "FILE",
        conflicts_with_all = ["source", "pool", "groups", "folder"])]
    file: Option<PathBuf>,
    /// 운영 클러스터(원격 이름)
    #[arg(long, required_unless_present = "file")]
    source: Option<String>,
    /// 되살린 VM의 기본 풀(이 클러스터)
    #[arg(long, required_unless_present = "file")]
    pool: Option<String>,
    /// 부팅 그룹 하나(준 차례대로, 기다림 `running`·제한 600초): `NAME=VM,VM…`
    #[arg(long = "group", value_name = "NAME=VM,VM", value_parser = parse_group,
        required_unless_present = "file")]
    groups: Vec<(String, Vec<String>)>,
    /// 장애 조치로 만든 VM의 폴더(경로)
    #[arg(long)]
    folder: Option<String>,
}

#[derive(Args)]
pub(crate) struct ReprotectArgs {
    plan: String,
    /// 복제를 받을 원격(보통 되살아난 운영 클러스터)
    #[arg(long)]
    remote: String,
    /// 그 원격의 백업 풀
    #[arg(long)]
    pool: String,
    /// 이 클러스터에서 백업을 쓸 백업 풀(없으면 이미 있는 작업의 풀, 그것도 없으면 계획의 `pool`)
    #[arg(long)]
    backup_pool: Option<String>,
    /// 실행 시각(UTC). 작업이 이미 있으면 생략해도 된다(그대로) — 보존 규칙도 주지 않으면 그대로다.
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
}

/// 실행 상태(`status.state`). 이 빌드가 모르는 값(뒤 버전이 더한 상태)은 `Unknown`이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RunState {
    Running,
    Succeeded,
    Partial,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

impl RunState {
    fn of(run: &Value) -> Self {
        Self::deserialize(&run["status"]["state"]).unwrap_or(Self::Unknown)
    }
}

/// 실행이 끝났는지. 모르는 상태는 끝난 시각(`finished_at`)이 있을 때만 끝으로 본다.
fn finished(run: &Value) -> bool {
    match RunState::of(run) {
        RunState::Running => false,
        RunState::Unknown => !run["status"]["finished_at"].is_null(),
        _ => true,
    }
}

/// `--group NAME=VM,VM`
fn parse_group(s: &str) -> Result<(String, Vec<String>), String> {
    let bad = || format!("invalid group {s:?}: use NAME=VM,VM");
    let (name, vms) = s.split_once('=').ok_or_else(bad)?;
    let vms: Vec<String> = vms
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
        .collect();
    if name.is_empty() || vms.is_empty() {
        return Err(bad());
    }
    Ok((name.to_string(), vms))
}

/// 계획 파일(`-`면 표준 입력): JSON이면 그대로, 아니면 YAML
fn read_doc(path: &Path) -> anyhow::Result<Value> {
    let text = if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .context("cannot read standard input")?;
        s
    } else {
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?
    };
    parse_doc(&text).with_context(|| format!("{} is not a JSON or YAML plan", path.display()))
}

fn parse_doc(text: &str) -> anyhow::Result<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
    let v: Value = serde_saphyr::from_str(text)?;
    anyhow::ensure!(v.is_object(), "the plan must be a mapping");
    Ok(v)
}

/// 파일의 (라벨, spec, revision): `{spec: …}`이면 리소스 모양(`plan get -o json`), 아니면 spec 그대로
fn plan_parts(doc: Value) -> (Option<Value>, Value, Option<Value>) {
    if doc["spec"].is_object() {
        let labels = doc.get("labels").filter(|l| !l.is_null()).cloned();
        let revision = doc.get("revision").filter(|r| !r.is_null()).cloned();
        (labels, doc["spec"].clone(), revision)
    } else {
        (None, doc, None)
    }
}

/// 플래그로 만든 계획의 spec(그 밖은 서버 기본)
fn flag_spec(a: &CreateArgs) -> Value {
    let groups: Vec<Value> = a
        .groups
        .iter()
        .map(|(name, vms)| {
            json!({ "name": name, "vms": vms.iter().map(|v| json!({ "vm": v })).collect::<Vec<_>>() })
        })
        .collect();
    let mut spec = json!({ "source": a.source, "pool": a.pool, "groups": groups });
    if let Some(f) = &a.folder {
        spec["folder"] = json!(f);
    }
    spec
}

/// 초를 짧게(`45s`, `12m`, `34h`, `3d`)
fn dur(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s}s"),
        s if s < 2 * 3600 => format!("{}m", s / 60),
        s if s < 48 * 3600 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn dur_cell(v: &Value) -> String {
    v.as_u64().map_or_else(|| "-".into(), dur)
}

fn time_cell(v: &Value) -> String {
    v.as_i64().map_or_else(|| "-".into(), unix_time)
}

fn vm_count(spec: &Value) -> usize {
    spec["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| g["vms"].as_array().map_or(0, Vec::len))
        .sum()
}

const PLAN_HEADERS: &[&str] = &["NAME", "SOURCE", "VMS", "RPO-OK", "LAST-TEST-RTO", "STATE"];

fn plan_row(p: &Value) -> Vec<String> {
    let st = &p["status"];
    let protection = st["protection"].as_array().cloned().unwrap_or_default();
    let rpo = if protection.is_empty() {
        "-".into()
    } else {
        let ok = protection.iter().filter(|v| v["ok"] == json!(true)).count();
        format!("{ok}/{}", protection.len())
    };
    let last = &st["last_test"];
    let test = if last.is_null() {
        "-".into()
    } else {
        let rto = dur_cell(&last["rto_secs"]);
        match last["result"].as_str() {
            Some("succeeded") | None => rto,
            Some(r) => format!("{rto} ({r})"),
        }
    };
    vec![
        cell(&p["name"]),
        cell(&p["spec"]["source"]),
        vm_count(&p["spec"]).to_string(),
        rpo,
        test,
        if st["run"].is_null() {
            "idle"
        } else {
            "running"
        }
        .into(),
    ]
}

const GROUP_HEADERS: &[&str] = &[
    "GROUP", "WAIT", "TIMEOUT", "DELAY", "VM", "POOL", "NODE", "IP",
];

/// IP 규칙 칸(`0=10.0.0.5/24 gw 10.0.0.1; 1=dhcp`)
fn ip_cell(rules: &Value) -> String {
    let rules: Vec<String> = rules
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let nic = cell(&r["nic"]);
            if r["dhcp"] == json!(true) {
                return format!("{nic}=dhcp");
            }
            let mut s = format!("{nic}={}", cell(&r["address"]));
            if let Some(gw) = r["gateway"].as_str() {
                s.push_str(&format!(" gw {gw}"));
            }
            if let Some(dns) = r["dns"].as_array().filter(|d| !d.is_empty()) {
                let dns: Vec<String> = dns.iter().map(cell).collect();
                s.push_str(&format!(" dns {}", dns.join(",")));
            }
            s
        })
        .collect();
    if rules.is_empty() {
        "-".into()
    } else {
        rules.join("; ")
    }
}

/// 그룹·VM 표의 행(그룹 칸은 그룹의 첫 VM 행에만)
fn group_rows(spec: &Value) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for g in spec["groups"].as_array().into_iter().flatten() {
        for (i, v) in g["vms"].as_array().into_iter().flatten().enumerate() {
            let group = if i == 0 {
                vec![
                    cell(&g["name"]),
                    g["wait"].as_str().unwrap_or("running").to_string(),
                    dur(g["timeout_secs"].as_u64().unwrap_or(600)),
                    dur(g["delay_secs"].as_u64().unwrap_or(0)),
                ]
            } else {
                vec![String::new(); 4]
            };
            rows.push(
                group
                    .into_iter()
                    .chain([
                        cell(&v["vm"]),
                        cell(&v["pool"]),
                        cell(&v["node"]),
                        ip_cell(&v["ip"]),
                    ])
                    .collect(),
            );
        }
    }
    rows
}

/// 맵 줄(`networks: lan=dr-lan, db=dr-db`). 비었으면 없다.
fn map_line(out: &mut dyn Write, label: &str, map: &Value) -> std::io::Result<()> {
    let Some(m) = map.as_object().filter(|m| !m.is_empty()) else {
        return Ok(());
    };
    let pairs: Vec<String> = m.iter().map(|(k, v)| format!("{k}={}", cell(v))).collect();
    writeln!(out, "{label}: {}", pairs.join(", "))
}

fn plan_view(out: &mut dyn Write, p: &Value) -> std::io::Result<()> {
    let spec = &p["spec"];
    let rpo = match spec["rpo_target_secs"].as_u64() {
        Some(0) => "off".into(),
        Some(s) => dur(s),
        None => "-".into(),
    };
    writeln!(
        out,
        "dr-plan {}: source {}, pool {}, RPO target {rpo}, on timeout {}",
        cell(&p["name"]),
        cell(&spec["source"]),
        cell(&spec["pool"]),
        spec["on_timeout"].as_str().unwrap_or("continue"),
    )?;
    if let Some(f) = spec["folder"].as_str() {
        writeln!(out, "folder: {f}")?;
    }
    map_line(out, "networks", &spec["networks"])?;
    map_line(out, "security groups", &spec["security_groups"])?;
    map_line(out, "test networks", &spec["test_networks"])?;
    table(out, GROUP_HEADERS, &group_rows(spec))
}

const RUNS_HEADERS: &[&str] = &["NAME", "KIND", "STATE", "STARTED", "RTO", "READY", "ERROR"];

fn run_row(r: &Value) -> Vec<String> {
    let st = &r["status"];
    let steps = st["steps"].as_array().cloned().unwrap_or_default();
    let done = steps
        .iter()
        .filter(|s| matches!(s["phase"].as_str(), Some("ready" | "deleted")))
        .count();
    vec![
        cell(&r["name"]),
        cell(&r["spec"]["kind"]),
        cell(&st["state"]),
        time_cell(&st["started_at"]),
        rto_cell(st),
        format!("{done}/{}", steps.len()),
        st["error"].as_str().unwrap_or("").to_string(),
    ]
}

/// RTO: 성공하지 않은 실행은 되살린 VM만으로 센 값이라 실제보다 짧다(`>=`).
fn rto_cell(st: &Value) -> String {
    match (st["rto_secs"].is_null(), st["state"].as_str()) {
        (true, _) => "-".into(),
        (false, Some("succeeded" | "running")) => dur_cell(&st["rto_secs"]),
        (false, _) => format!(">={}", dur_cell(&st["rto_secs"])),
    }
}

const STEP_HEADERS: &[&str] = &[
    "GROUP", "VM", "BACKUP", "PHASE", "NEW-VM", "NODE", "TASK", "ERROR",
];

fn step_row(s: &Value) -> Vec<String> {
    let mut phase = cell(&s["phase"]);
    if let Some(stage) = s["prepare"]["stage"].as_str() {
        phase = format!("{phase} ({stage})");
    }
    vec![
        if s["group"].as_str().is_some_and(|g| !g.is_empty()) {
            cell(&s["group"])
        } else {
            "-".into()
        },
        cell(&s["vm"]),
        cell(&s["backup"]),
        phase,
        cell(&s["new_vm"]),
        cell(&s["node"]),
        cell(&s["task"]),
        s["error"].as_str().unwrap_or("").to_string(),
    ]
}

/// 실행 하나: 머리 줄, 오류·취소, 거품 네트워크, 단계 표
fn run_view(out: &mut dyn Write, r: &Value) -> std::io::Result<()> {
    let st = &r["status"];
    let mut head = format!(
        "dr-run {} ({} of plan {}): {}, started {}",
        cell(&r["name"]),
        cell(&r["spec"]["kind"]),
        cell(&r["spec"]["plan_name"]),
        cell(&st["state"]),
        time_cell(&st["started_at"]),
    );
    if !st["finished_at"].is_null() {
        head.push_str(&format!(", finished {}", time_cell(&st["finished_at"])));
    }
    if !st["rto_secs"].is_null() {
        head.push_str(&format!(", RTO {}", rto_cell(st)));
    }
    if !st["prepare_secs"].is_null() {
        head.push_str(&format!(", prepare {}", dur_cell(&st["prepare_secs"])));
    }
    writeln!(out, "{head}")?;
    if let Some(e) = st["error"].as_str() {
        writeln!(out, "error: {e}")?;
    }
    if st["cancel_requested"] == json!(true) && !finished(r) {
        writeln!(out, "cancel requested")?;
    }
    if st["preempted"] == json!(true) {
        writeln!(
            out,
            "stopped for a failover (its cleanup starts after that failover)"
        )?;
    }
    if let Some(t) = st["preempted_test"].as_str() {
        writeln!(out, "stopped the running test {t}")?;
    }
    for b in st["bubbles"].as_array().into_iter().flatten() {
        let mut line = format!("network {}", cell(&b["name"]));
        if let Some(src) = b["source"].as_str().filter(|s| !s.is_empty()) {
            line.push_str(&format!(" (for {src})"));
        }
        line.push_str(&format!(": {}", cell(&b["state"])));
        if let Some(e) = b["error"].as_str() {
            line.push_str(&format!(": {e}"));
        }
        writeln!(out, "{line}")?;
    }
    let rows: Vec<Vec<String>> = st["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(step_row)
        .collect();
    table(out, STEP_HEADERS, &rows)?;
    // 단계마다 다시 해 보는 일시 오류와 기록(다른 복구 지점, 운영 쪽 VM 다시 켬, 라이브 복원 복사 실패)
    for s in st["steps"].as_array().into_iter().flatten() {
        if let Some(e) = s["last_error"].as_str() {
            writeln!(
                out,
                "{}: retrying since {}: {e}",
                cell(&s["vm"]),
                time_cell(&s["retrying_since"])
            )?;
        }
        for n in s["notes"].as_array().into_iter().flatten() {
            writeln!(out, "{}: {}", cell(&s["vm"]), cell(n))?;
        }
        // 리뷰 2: 데이터를 더 잃음·디스크를 다 받지 못함·운영 쪽에서 꺼진 채 남음
        if let Some(lost) = s["lost_secs"].as_u64() {
            writeln!(
                out,
                "{}: warning: restored from an older recovery point ({} older than the first choice)",
                cell(&s["vm"]),
                dur_cell(&json!(lost))
            )?;
        }
        if let Some(e) = s["copy_error"].as_str() {
            writeln!(
                out,
                "{}: warning: the live restore did not finish copying the disks: {e}",
                cell(&s["vm"])
            )?;
        }
        if let Some(e) = s["prepare"]["restart_error"].as_str() {
            writeln!(
                out,
                "{}: warning: source VM left stopped ({e}) — start it on the source cluster",
                cell(&s["vm"])
            )?;
        }
    }
    if let Some(by) = st["preempted_by"].as_str() {
        writeln!(out, "stopped by failover {by}")?;
    }
    readiness_lines(out, &st["failover_readiness"])
}

/// 두 벌 막기 목록의 VM 이름(새 모양 `{vm, source_id}`, 옛 서버의 이름 목록도)
fn fence_names(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|f| f["vm"].as_str().map_or_else(|| cell(f), str::to_string))
        .collect()
}

/// 장애 조치 준비 문제(VM마다 한 줄)
fn readiness_lines(out: &mut dyn Write, list: &Value) -> std::io::Result<()> {
    for r in list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["ok"] != json!(true))
    {
        let issues: Vec<String> = r["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| match i["name"].as_str() {
                Some(n) => format!("{} {n} missing", cell(&i["kind"])),
                None => format!("no {}", cell(&i["kind"])),
            })
            .collect();
        writeln!(
            out,
            "failover not ready: {}: {}",
            cell(&r["vm"]),
            issues.join(", ")
        )?;
    }
    Ok(())
}

const PROTECTION_HEADERS: &[&str] = &["VM", "BACKUP", "POINT", "AGE", "STATUS"];

fn protection_row(p: &Value) -> Vec<String> {
    let status = if p["ok"] == json!(true) {
        "ok".to_string()
    } else {
        match p["issue"].as_str() {
            Some("no-recovery-point") => "no recovery point".into(),
            Some("over-rpo") => "over RPO".into(),
            Some("verify-failed") => "newest copy failed verification".into(),
            Some(other) => other.to_string(),
            None => "not ok".into(),
        }
    };
    vec![
        cell(&p["vm"]),
        cell(&p["backup"]),
        time_cell(&p["point_at"]),
        dur_cell(&p["age_secs"]),
        status,
    ]
}

const TEST_HEADERS: &[&str] = &["RUN", "STATE", "STARTED", "RTO", "FAILED"];

fn summary_row(s: &Value) -> Vec<String> {
    let failed: Vec<String> = s["failed_vms"]
        .as_array()
        .into_iter()
        .flatten()
        .map(cell)
        .collect();
    vec![
        cell(&s["name"]),
        cell(&s["state"]),
        time_cell(&s["started_at"]),
        match s["state"].as_str() {
            Some("succeeded") | None => dur_cell(&s["rto_secs"]),
            _ if s["rto_secs"].is_null() => "-".into(),
            _ => format!(">={}", dur_cell(&s["rto_secs"])),
        },
        if failed.is_empty() {
            "-".into()
        } else {
            failed.join(", ")
        },
    ]
}

fn report_view(out: &mut dyn Write, r: &Value) -> std::io::Result<()> {
    let rpo = match r["rpo_target_secs"].as_u64() {
        Some(0) => "off".into(),
        Some(s) => dur(s),
        None => "-".into(),
    };
    writeln!(
        out,
        "dr-plan {} (source {}): {} of {} VMs within RPO target {rpo}, at {}",
        cell(&r["plan"]),
        cell(&r["source"]),
        cell(&r["protected"]),
        cell(&r["total"]),
        time_cell(&r["at"]),
    )?;
    if r["source_registered"] == json!(false) {
        writeln!(
            out,
            "warning: remote {} is not registered, so no recovery points are matched",
            cell(&r["source"])
        )?;
    }
    let rows: Vec<Vec<String>> = r["protection"]
        .as_array()
        .into_iter()
        .flatten()
        .map(protection_row)
        .collect();
    table(out, PROTECTION_HEADERS, &rows)?;
    readiness_lines(out, &r["failover_readiness"])?;
    let pending = fence_names(&r["source_fence_pending"]);
    if !pending.is_empty() {
        writeln!(
            out,
            "warning: recovered after a disaster and not yet stopped on the source cluster: {} — run `orbivisorctl dr fence-source {}` when it is back (or `dr clear-fence` once you checked yourself)",
            pending.join(", "),
            cell(&r["plan"])
        )?;
    }
    let tests: Vec<Vec<String>> = r["tests"]
        .as_array()
        .into_iter()
        .flatten()
        .map(summary_row)
        .collect();
    if tests.is_empty() {
        writeln!(out, "\ntests: none")?;
    } else {
        writeln!(out, "\ntests:")?;
        table(out, TEST_HEADERS, &tests)?;
    }
    for (label, s) in [
        ("last failover", &r["last_failover"]),
        ("running", &r["running"]),
    ] {
        if s.is_null() {
            continue;
        }
        let mut line = format!(
            "{label}: {} ({}) {}",
            cell(&s["name"]),
            cell(&s["kind"]),
            cell(&s["state"])
        );
        if !s["rto_secs"].is_null() {
            line.push_str(&format!(", RTO {}", dur_cell(&s["rto_secs"])));
        }
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// 기다림을 끝내는 오류(401·403·404 — 다시 읽어도 같다). 나머지(닿지 않음·리더 바뀜)는 알리고 다시 읽는다.
fn ends_wait(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NotLoggedIn>().is_some()
        || e.downcast_ref::<ApiError>().is_some_and(|api| {
            matches!(
                api.code.as_str(),
                "unauthorized" | "forbidden" | "not_found"
            )
        })
}

/// 실행이 끝날 때까지 `every`마다 다시 읽어 `seen`(읽은 실행, stderr)을 부른다. 끝난 실행을 돌려준다.
async fn follow(
    client: &Client,
    run: &str,
    every: Duration,
    err: &mut (dyn Write + Send),
    mut seen: impl FnMut(&Value, &mut (dyn Write + Send)) -> anyhow::Result<()>,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + BACKUP_WAIT;
    let path = format!("/dr-runs/{}", seg(run));
    loop {
        match client.get(&path).await {
            Ok(r) => {
                seen(&r, err)?;
                if finished(&r) {
                    return Ok(r);
                }
            }
            Err(e) if !ends_wait(&e) => writeln!(err, "error: {e:#}")?,
            Err(e) => return Err(e),
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "stopped waiting for dr-run {run} after {} hours; it may still be running, see `orbivisorctl dr run {run}`",
            BACKUP_WAIT.as_secs() / 3600
        );
        tokio::time::sleep(every).await;
    }
}

/// 끝난 실행이 `succeeded`가 아니면 끝 코드 1
fn check_result(run: &Value) -> anyhow::Result<()> {
    match RunState::of(run) {
        RunState::Succeeded => Ok(()),
        _ => anyhow::bail!(
            "dr-run {} ended {}",
            cell(&run["name"]),
            cell(&run["status"]["state"])
        ),
    }
}

/// 실행 끝점 부름 하나
struct Action<'a> {
    plan: &'a str,
    /// `test`·`failover`·`cleanup`·`cancel`
    name: &'static str,
    body: Value,
    wait: bool,
}

/// 실행 끝점을 부르고(`--wait`면 끝까지 따라가며 단계가 바뀔 때마다 stderr에 한 줄) 결과를 알린다.
async fn act(
    client: &Client,
    a: Action<'_>,
    every: Duration,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let (action, wait) = (a.name, a.wait);
    let run = client
        .post(
            &format!("/dr-plans/{}/actions/{action}", seg(a.plan)),
            &a.body,
        )
        .await?;
    let name = cell(&run["name"]);
    if !wait {
        if json_out {
            return Ok(print_json(out, &run)?);
        }
        if action == "cancel" {
            writeln!(out, "dr-run {name}: cancel requested")?;
        } else {
            writeln!(
                out,
                "dr-run {name} started ({}); follow it with `orbivisorctl dr run {name} --watch`",
                cell(&run["spec"]["kind"])
            )?;
        }
        return Ok(());
    }
    writeln!(
        err,
        "dr-run {name} started ({})",
        cell(&run["spec"]["kind"])
    )?;
    let mut last: Vec<(String, String)> = Vec::new();
    let done = follow(client, &cell(&run["id"]), every, err, |r, err| {
        let now: Vec<(String, String)> = r["status"]["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| (cell(&s["vm"]), step_row(s)[3].clone()))
            .collect();
        for step in now.iter().filter(|s| !last.contains(s)) {
            // 진행 줄을 쓰지 못해도(stderr가 닫힘) 계속 기다린다.
            let _ = writeln!(err, "dr-run {name}: {} {}", step.0, step.1);
        }
        last = now;
        Ok(())
    })
    .await?;
    if json_out {
        print_json(out, &done)?;
    } else {
        run_view(out, &done)?;
    }
    check_result(&done)
}

/// `dr run <run> --watch`: 바뀔 때마다 실행 표를 다시 찍는다(`-o json`이면 한 줄씩).
async fn watch(
    client: &Client,
    run: &str,
    every: Duration,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut last = String::new();
    let mut first = true;
    let done = follow(client, run, every, err, |r, _| {
        let mut text = Vec::new();
        if json_out {
            writeln!(text, "{}", serde_json::to_string(r)?)?;
        } else {
            run_view(&mut text, r)?;
        }
        let text = String::from_utf8(text)?;
        if text != last {
            if !first && !json_out {
                writeln!(out)?;
            }
            out.write_all(text.as_bytes())?;
            out.flush()?;
            first = false;
            last = text;
        }
        Ok(())
    })
    .await?;
    check_result(&done)
}

pub(crate) async fn dr(
    client: &Client,
    cmd: DrCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    run_cmd(client, cmd, POLL_EVERY, json_out, out, err).await
}

async fn run_cmd(
    client: &Client,
    cmd: DrCmd,
    every: Duration,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let plan_path = |p: &str| format!("/dr-plans/{}", seg(p));
    match cmd {
        DrCmd::Plan { cmd } => return plan(client, cmd, json_out, out).await,
        DrCmd::Test {
            plan,
            wait,
            points,
            allow_routed_test_networks,
        } => {
            let mut body = json!({});
            points.fill(&mut body);
            if allow_routed_test_networks {
                body["allow_routed_test_networks"] = json!(true);
            }
            let a = Action {
                plan: &plan,
                name: "test",
                body,
                wait,
            };
            act(client, a, every, json_out, out, err).await?
        }
        DrCmd::Failover {
            plan,
            mode,
            confirm,
            wait,
            points,
        } => {
            let mode = match mode {
                FailoverMode::Planned => "planned",
                FailoverMode::Disaster => "disaster",
            };
            let mut body = json!({ "mode": mode, "confirm": confirm });
            points.fill(&mut body);
            let a = Action {
                plan: &plan,
                name: "failover",
                body,
                wait,
            };
            act(client, a, every, json_out, out, err).await?
        }
        DrCmd::Cleanup { plan, wait } => {
            let a = Action {
                plan: &plan,
                name: "cleanup",
                body: json!({}),
                wait,
            };
            act(client, a, every, json_out, out, err).await?
        }
        DrCmd::Cancel { plan } => {
            let a = Action {
                plan: &plan,
                name: "cancel",
                body: json!({}),
                wait: false,
            };
            act(client, a, every, json_out, out, err).await?
        }
        DrCmd::Runs { plan } => {
            let list = client.get(&format!("{}/runs", plan_path(&plan))).await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                return Ok(print_json(out, &Value::Array(items))?);
            }
            let rows: Vec<Vec<String>> = items.iter().map(run_row).collect();
            table(out, RUNS_HEADERS, &rows)?;
        }
        DrCmd::Run { run, watch: true } => watch(client, &run, every, json_out, out, err).await?,
        DrCmd::Run { run, watch: false } => {
            let r = client.get(&format!("/dr-runs/{}", seg(&run))).await?;
            if json_out {
                print_json(out, &r)?;
            } else {
                run_view(out, &r)?;
            }
        }
        DrCmd::Report {
            plan,
            point_in_time,
        } => {
            let at = point_in_time.map_or_else(String::new, |t| format!("?point_in_time={t}"));
            let r = client
                .get(&format!("{}/report{at}", plan_path(&plan)))
                .await?;
            if json_out {
                print_json(out, &r)?;
            } else {
                report_view(out, &r)?;
            }
        }
        DrCmd::Reprotect(a) => {
            let mut body = json!({ "remote": a.remote, "pool": a.pool });
            if let Some(at) = &a.at {
                body["schedule"] = json!({ "at": at, "days": days_value(&a.days) });
            }
            if a.keep.given() {
                body["keep"] = keep_value(&a.keep, &Value::Null);
            }
            if let Some(p) = &a.backup_pool {
                body["backup_pool"] = json!(p);
            }
            let job = client
                .post(&format!("{}/actions/reprotect", plan_path(&a.plan)), &body)
                .await?;
            if json_out {
                return Ok(print_json(out, &job)?);
            }
            writeln!(
                out,
                "backup-job {} set: recovered VMs of plan {} replicate to {}/{}",
                cell(&job["name"]),
                a.plan,
                a.remote,
                a.pool
            )?;
        }
        DrCmd::FenceSource { plan } => {
            let r = client
                .post(
                    &format!("{}/actions/fence-source", plan_path(&plan)),
                    &json!({}),
                )
                .await?;
            if json_out {
                return Ok(print_json(out, &r)?);
            }
            for v in r["results"].as_array().into_iter().flatten() {
                let what = match (v["result"].as_str(), v["error"].as_str()) {
                    (Some("not_found"), _) => "not on the source cluster".to_string(),
                    (Some("different_vm"), _) => {
                        "a different VM has this name there; not stopped".to_string()
                    }
                    (Some("different_cluster"), e) => {
                        format!(
                            "not stopped: {}",
                            e.unwrap_or("the remote points at another cluster")
                        )
                    }
                    // 옛 서버(`result` 없음)
                    (None, Some(e)) if v["ok"] == json!(true) => e.to_string(),
                    _ if v["ok"] == json!(true) => "stopped".to_string(),
                    (_, e) => format!("not stopped: {}", e.unwrap_or("failed")),
                };
                writeln!(out, "{}: {what}", cell(&v["vm"]))?;
            }
            let pending = fence_names(&r["pending"]);
            if !pending.is_empty() {
                writeln!(out, "still to fence: {}", pending.join(", "))?;
                anyhow::bail!("some VMs are not stopped on the source cluster");
            }
        }
        DrCmd::ClearFence { plan } => {
            let before = client.get(&plan_path(&plan)).await?;
            let names = fence_names(&before["status"]["source_fence_pending"]);
            let r = client
                .post(
                    &format!("{}/actions/clear-fence", plan_path(&plan)),
                    &json!({}),
                )
                .await?;
            if json_out {
                return Ok(print_json(out, &r)?);
            }
            if names.is_empty() {
                writeln!(out, "nothing was pending for plan {plan}")?;
            } else {
                writeln!(
                    out,
                    "cleared the fence list of plan {plan}: {} (nothing was done on the source cluster)",
                    names.join(", ")
                )?;
            }
        }
    }
    Ok(())
}

async fn plan(
    client: &Client,
    cmd: PlanCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = |p: &str| format!("/dr-plans/{}", seg(p));
    match cmd {
        PlanCmd::Create(a) => {
            let (labels, spec) = match &a.file {
                Some(f) => {
                    let (labels, spec, _) = plan_parts(read_doc(f)?);
                    (labels, spec)
                }
                None => (None, flag_spec(&a)),
            };
            let mut body = json!({ "name": a.name, "spec": spec });
            if let Some(l) = labels {
                body["labels"] = l;
            }
            let p = client.post("/dr-plans", &body).await?;
            if json_out {
                return Ok(print_json(out, &p)?);
            }
            writeln!(
                out,
                "dr-plan {} created ({} VMs)",
                cell(&p["name"]),
                vm_count(&p["spec"])
            )?;
        }
        PlanCmd::Get { plan } => {
            let p = client.get(&path(&plan)).await?;
            if json_out {
                print_json(out, &p)?;
            } else {
                plan_view(out, &p)?;
            }
        }
        PlanCmd::List => {
            let list = client.get("/dr-plans").await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                return Ok(print_json(out, &Value::Array(items))?);
            }
            let rows: Vec<Vec<String>> = items.iter().map(plan_row).collect();
            table(out, PLAN_HEADERS, &rows)?;
        }
        PlanCmd::Set { plan, file } => {
            let (labels, spec, revision) = plan_parts(read_doc(&file)?);
            // 파일에 revision·labels가 없으면 지금 계획에서(읽은 뒤 바뀌었으면 서버가 412로 거절한다)
            let (labels, revision) = match (labels, revision) {
                (Some(l), Some(r)) => (l, r),
                (labels, revision) => {
                    let current = client.get(&path(&plan)).await?;
                    (
                        labels.unwrap_or_else(|| current["labels"].clone()),
                        revision.unwrap_or_else(|| current["revision"].clone()),
                    )
                }
            };
            let r = client
                .request(Method::PUT, &path(&plan))
                .header(reqwest::header::IF_MATCH, cell(&revision))
                .json(&json!({ "labels": labels, "spec": spec }));
            let p = client.send(r).await?;
            if json_out {
                return Ok(print_json(out, &p)?);
            }
            writeln!(
                out,
                "dr-plan {} updated ({} VMs)",
                cell(&p["name"]),
                vm_count(&p["spec"])
            )?;
        }
        PlanCmd::Delete { plan } => {
            client.delete(&path(&plan)).await?;
            writeln!(out, "dr-plan {plan} deleted")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use clap::Parser;

    use super::*;
    use crate::fake_api::fake_api;

    /// 시험용 명령줄(`dr`만)
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: Top,
    }

    #[derive(Subcommand)]
    enum Top {
        Dr {
            #[command(subcommand)]
            cmd: DrCmd,
        },
    }

    fn parse(args: &[&str]) -> Result<DrCmd, clap::Error> {
        let mut argv = vec!["t", "dr"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|c| match c.cmd {
            Top::Dr { cmd } => cmd,
        })
    }

    async fn exec(
        client: &Client,
        args: &[&str],
        json_out: bool,
    ) -> (anyhow::Result<()>, String, String) {
        let cmd = parse(args).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let r = run_cmd(
            client,
            cmd,
            Duration::from_millis(1),
            json_out,
            &mut out,
            &mut err,
        )
        .await;
        (
            r,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn not_found(path: &str) -> (u16, Value) {
        (404, json!({ "code": "not_found", "message": path }))
    }

    #[test]
    fn parses_flags_and_requires_what_is_needed() {
        assert_eq!(
            parse_group("db=db1, db2").unwrap(),
            ("db".into(), vec!["db1".into(), "db2".into()])
        );
        assert!(
            parse_group("db=").is_err() && parse_group("=a").is_err() && parse_group("db").is_err()
        );
        // 파일이 없으면 --source --pool --group, 파일이 있으면 그것들과 함께 쓸 수 없다.
        assert!(parse(&["plan", "create", "p", "--source", "prod", "--pool", "local"]).is_err());
        assert!(
            parse(&[
                "plan", "create", "p", "--source", "prod", "--pool", "local", "--group", "a=x"
            ])
            .is_ok()
        );
        assert!(parse(&["plan", "create", "p", "-f", "p.yaml"]).is_ok());
        assert!(parse(&["plan", "create", "p", "-f", "p.yaml", "--source", "prod"]).is_err());
        // 장애 조치는 방식과 확인이 있어야 한다.
        assert!(parse(&["failover", "p", "--mode", "disaster"]).is_err());
        assert!(parse(&["failover", "p", "--confirm", "p"]).is_err());
        assert!(parse(&["failover", "p", "--mode", "sideways", "--confirm", "p"]).is_err());
        assert!(
            parse(&[
                "failover",
                "p",
                "--mode",
                "planned",
                "--confirm",
                "p",
                "--wait"
            ])
            .is_ok()
        );
        // reprotect는 원격·풀·시각
        // 시각은 이미 있는 작업이면 생략해도 된다(서버가 그대로 둔다).
        assert!(parse(&["reprotect", "p", "--remote", "prod", "--pool", "b"]).is_ok());
        assert!(
            parse(&[
                "reprotect",
                "p",
                "--remote",
                "prod",
                "--pool",
                "b",
                "--at",
                "03:00"
            ])
            .is_ok()
        );
    }

    #[test]
    fn reads_yaml_and_json_plans() {
        let yaml = "source: prod\npool: local\ngroups:\n  - name: db\n    wait: guest\n    vms:\n      - vm: db1\n";
        let v = parse_doc(yaml).unwrap();
        assert_eq!(v["groups"][0]["vms"][0]["vm"], "db1");
        assert_eq!(parse_doc(r#"{"source":"prod"}"#).unwrap()["source"], "prod");
        assert!(parse_doc("- a\n- b\n").is_err());
        // `plan get -o json`의 출력: 라벨·spec·revision
        let (labels, spec, rev) =
            plan_parts(json!({ "name": "p", "labels": { "a": "b" }, "revision": 7,
            "spec": { "source": "prod" }, "status": {} }));
        assert_eq!(
            (labels, spec, rev),
            (
                Some(json!({ "a": "b" })),
                json!({ "source": "prod" }),
                Some(json!(7))
            )
        );
        let (labels, spec, rev) = plan_parts(json!({ "source": "prod" }));
        assert_eq!(
            (labels, spec, rev),
            (None, json!({ "source": "prod" }), None)
        );
    }

    #[test]
    fn durations_and_rows() {
        assert_eq!(dur(45), "45s");
        assert_eq!(dur(600), "10m");
        assert_eq!(dur(86_400), "24h");
        assert_eq!(dur(122_400), "34h");
        assert_eq!(dur(3 * 86_400), "3d");
        let p = json!({ "name": "web", "spec": { "source": "prod", "groups": [
                { "name": "db", "vms": [{ "vm": "db1" }] }, { "name": "app", "vms": [{ "vm": "a1" }, { "vm": "a2" }] }] },
            "status": { "protection": [{ "vm": "db1", "ok": true }, { "vm": "a1", "ok": false }, { "vm": "a2", "ok": true }],
                "last_test": { "run": "x", "at": 1, "rto_secs": 300, "result": "partial" } } });
        assert_eq!(
            plan_row(&p),
            ["web", "prod", "3", "2/3", "5m (partial)", "idle"]
        );
        let fresh = json!({ "name": "n", "spec": { "source": "prod", "groups": [] }, "status": { "run": "r" } });
        assert_eq!(plan_row(&fresh), ["n", "prod", "0", "-", "-", "running"]);
        let spec = json!({ "groups": [{ "name": "db", "wait": "ip", "timeout_secs": 900, "delay_secs": 30, "vms": [
            { "vm": "db1", "pool": "fast", "ip": [{ "nic": 0, "address": "10.0.0.5/24", "gateway": "10.0.0.1", "dns": ["1.1.1.1"] },
                { "nic": 1, "dhcp": true }] },
            { "vm": "db2" }] }] });
        assert_eq!(
            group_rows(&spec),
            [
                vec![
                    "db",
                    "ip",
                    "15m",
                    "30s",
                    "db1",
                    "fast",
                    "-",
                    "0=10.0.0.5/24 gw 10.0.0.1 dns 1.1.1.1; 1=dhcp"
                ],
                vec!["", "", "", "", "db2", "-", "-", "-"],
            ]
        );
        let step = json!({ "group": "db", "vm": "db1", "phase": "preparing", "prepare": { "stage": "replicate" }, "at": 1 });
        assert_eq!(
            step_row(&step),
            ["db", "db1", "-", "preparing (replicate)", "-", "-", "-", ""]
        );
        let r = json!({ "name": "web-test-1", "spec": { "kind": "test" }, "status": { "state": "succeeded",
            "started_at": 0, "rto_secs": 95, "steps": [{ "vm": "a", "phase": "ready" }, { "vm": "b", "phase": "timed_out" }] } });
        assert_eq!(
            run_row(&r),
            [
                "web-test-1",
                "test",
                "succeeded",
                "1970-01-01 00:00 UTC",
                "95s",
                "1/2",
                ""
            ]
        );
        let pr = json!({ "vm": "web3", "backup": "b", "point_at": 0, "age_secs": 122_400, "ok": false, "issue": "over-rpo" });
        assert_eq!(
            protection_row(&pr),
            ["web3", "b", "1970-01-01 00:00 UTC", "34h", "over RPO"]
        );
        let none = json!({ "vm": "db1", "ok": false, "issue": "no-recovery-point" });
        assert_eq!(protection_row(&none)[4], "no recovery point");
    }

    /// 이 빌드가 모르는 실행 상태는 `Unknown`이고, 끝난 시각이 있을 때만 끝으로 본다.
    #[test]
    fn unknown_run_states_are_tolerated() {
        let run = |state: &str, finished: Option<u64>| json!({ "name": "r", "status": { "state": state, "finished_at": finished } });
        assert_eq!(RunState::of(&run("rolling-back", None)), RunState::Unknown);
        assert_eq!(RunState::of(&json!({})), RunState::Unknown);
        assert!(!finished(&run("rolling-back", None)));
        assert!(finished(&run("rolling-back", Some(5))));
        assert!(!finished(&run("running", None)));
        assert!(finished(&run("partial", Some(5))));
        assert!(check_result(&run("succeeded", Some(5))).is_ok());
        let e = check_result(&run("partial", Some(5))).unwrap_err();
        assert_eq!(e.to_string(), "dr-run r ended partial");
    }

    /// 플래그로 만든 계획은 그룹 차례대로 VM만 적어 보내고(나머지는 서버 기본), 파일이면 spec과 라벨을 보낸다.
    #[tokio::test]
    async fn create_sends_the_plan() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/dr-plans") => (
                201,
                json!({ "name": "web", "spec": { "groups": [{ "name": "db", "vms": [{ "vm": "db1" }, { "vm": "db2" }] }] } }),
            ),
            _ => not_found(path),
        })
        .await;
        let args = [
            "plan",
            "create",
            "web",
            "--source",
            "prod",
            "--pool",
            "local",
            "--group",
            "db=db1,db2",
            "--group",
            "app=a1",
            "--folder",
            "/dr",
        ];
        let (r, out, _) = exec(&client, &args, false).await;
        r.unwrap();
        assert_eq!(out, "dr-plan web created (2 VMs)\n");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plan.yaml");
        std::fs::write(&file, "labels:\n  team: web\nspec:\n  source: prod\n  pool: local\n  rpo_target_secs: 3600\n  groups:\n    - name: db\n      vms: [{vm: db1}]\n").unwrap();
        let (r, _, _) = exec(
            &client,
            &["plan", "create", "web", "-f", file.to_str().unwrap()],
            false,
        )
        .await;
        r.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].body,
            json!({ "name": "web", "spec": { "source": "prod", "pool": "local", "folder": "/dr", "groups": [
                { "name": "db", "vms": [{ "vm": "db1" }, { "vm": "db2" }] }, { "name": "app", "vms": [{ "vm": "a1" }] }] } })
        );
        assert_eq!(
            seen[1].body,
            json!({ "name": "web", "labels": { "team": "web" }, "spec": { "source": "prod", "pool": "local",
                "rpo_target_secs": 3600, "groups": [{ "name": "db", "vms": [{ "vm": "db1" }] }] } })
        );
    }

    /// `plan set -f`는 통째로 PUT한다: 파일에 revision이 없으면 지금 계획을 읽어 그 revision(`If-Match`)과 라벨로,
    /// 있으면 읽지 않고 그것으로.
    #[tokio::test]
    async fn set_puts_the_whole_plan_with_if_match() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("GET", "/api/v1/dr-plans/web") => (
                200,
                json!({ "name": "web", "revision": 41, "labels": { "team": "web" } }),
            ),
            ("PUT", "/api/v1/dr-plans/web") => {
                (200, json!({ "name": "web", "spec": { "groups": [] } }))
            }
            _ => not_found(path),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.json");
        std::fs::write(&spec, r#"{"source":"prod","pool":"local","groups":[]}"#).unwrap();
        let (r, out, _) = exec(
            &client,
            &["plan", "set", "web", "-f", spec.to_str().unwrap()],
            false,
        )
        .await;
        r.unwrap();
        assert_eq!(out, "dr-plan web updated (0 VMs)\n");
        let full = dir.path().join("full.json");
        std::fs::write(&full, r#"{"name":"web","revision":50,"labels":{},"spec":{"source":"prod","pool":"x","groups":[]},"status":{}}"#).unwrap();
        let (r, _, _) = exec(
            &client,
            &["plan", "set", "web", "-f", full.to_str().unwrap()],
            false,
        )
        .await;
        r.unwrap();
        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|s| s.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/dr-plans/web HTTP/1.1",
                "PUT /api/v1/dr-plans/web HTTP/1.1",
                "PUT /api/v1/dr-plans/web HTTP/1.1"
            ]
        );
        assert_eq!(seen[1].if_match.as_deref(), Some("41"));
        assert_eq!(
            seen[1].body,
            json!({ "labels": { "team": "web" }, "spec": { "source": "prod", "pool": "local", "groups": [] } })
        );
        assert_eq!(seen[2].if_match.as_deref(), Some("50"));
        assert_eq!(
            seen[2].body,
            json!({ "labels": {}, "spec": { "source": "prod", "pool": "x", "groups": [] } })
        );
    }

    /// 실행 하나를 차례로 답하는 가짜 API: n번째 `GET /dr-runs/…`는 `runs[n]`(끝은 마지막 것)
    async fn run_api(start: Value, runs: Vec<Value>) -> (Client, crate::fake_api::SeenLog) {
        let n = Arc::new(AtomicUsize::new(0));
        fake_api(move |method, path| match (method, path) {
            ("POST", p) if p.starts_with("/api/v1/dr-plans/web/actions/") => (202, start.clone()),
            ("GET", p) if p.starts_with("/api/v1/dr-runs/") => {
                let i = n.fetch_add(1, Ordering::SeqCst).min(runs.len() - 1);
                (200, runs[i].clone())
            }
            _ => not_found(path),
        })
        .await
    }

    fn run_at(state: &str, phases: &[(&str, &str)], finished: Option<u64>) -> Value {
        let steps: Vec<Value> = phases
            .iter()
            .map(|(vm, phase)| json!({ "group": "g", "vm": vm, "phase": phase, "at": 0 }))
            .collect();
        json!({ "id": "019a0000-0000-7000-8000-000000000001", "name": "web-test-1",
            "spec": { "kind": "test", "plan_name": "web" },
            "status": { "state": state, "started_at": 0, "finished_at": finished, "rto_secs": finished, "steps": steps } })
    }

    /// `dr test --wait`: 시작을 알리고 단계가 바뀔 때마다 stderr에 한 줄, 끝에 실행 표. 끝 상태가 `succeeded`면 0.
    #[tokio::test]
    async fn test_wait_follows_the_run() {
        let start = run_at("running", &[("a", "pending")], None);
        let runs = vec![
            run_at("running", &[("a", "restoring")], None),
            run_at("running", &[("a", "restoring")], None),
            run_at("succeeded", &[("a", "ready")], Some(70)),
        ];
        let (client, seen) = run_api(start, runs).await;
        let (r, out, err) = exec(&client, &["test", "web", "--wait"], false).await;
        r.unwrap();
        assert_eq!(
            err,
            "dr-run web-test-1 started (test)\ndr-run web-test-1: a restoring\ndr-run web-test-1: a ready\n"
        );
        assert_eq!(
            out,
            "dr-run web-test-1 (test of plan web): succeeded, started 1970-01-01 00:00 UTC, finished 1970-01-01 00:01 UTC, RTO 70s\n\
             GROUP  VM  BACKUP  PHASE  NEW-VM  NODE  TASK  ERROR\n\
             g      a   -       ready  -       -     -\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].line,
            "POST /api/v1/dr-plans/web/actions/test HTTP/1.1"
        );
        assert_eq!(
            seen[1].line,
            "GET /api/v1/dr-runs/019a0000-0000-7000-8000-000000000001 HTTP/1.1"
        );
    }

    /// 장애 조치는 방식과 확인을 보내고, `--wait`의 끝이 `partial`이면 끝 코드 1이다. 기다리지 않으면 시작만 알린다.
    #[tokio::test]
    async fn failover_sends_mode_and_confirm() {
        let start = run_at("running", &[], None);
        let runs = vec![run_at(
            "partial",
            &[("a", "ready"), ("b", "failed")],
            Some(9),
        )];
        let (client, seen) = run_api(start, runs).await;
        let args = [
            "failover",
            "web",
            "--mode",
            "disaster",
            "--confirm",
            "web",
            "--wait",
        ];
        let (r, _, _) = exec(&client, &args, false).await;
        assert_eq!(
            r.unwrap_err().to_string(),
            "dr-run web-test-1 ended partial"
        );
        let (r, out, _) = exec(
            &client,
            &["failover", "web", "--mode", "planned", "--confirm", "web"],
            false,
        )
        .await;
        r.unwrap();
        assert_eq!(
            out,
            "dr-run web-test-1 started (test); follow it with `orbivisorctl dr run web-test-1 --watch`\n"
        );
        let (r, out, _) = exec(&client, &["cancel", "web"], false).await;
        r.unwrap();
        assert_eq!(out, "dr-run web-test-1: cancel requested\n");
        let args = [
            "failover",
            "web",
            "--mode",
            "disaster",
            "--confirm",
            "web",
            "--allow-missing",
            "--point-in-time",
            "2026-11-01T00:00:00Z",
        ];
        exec(&client, &args, false).await.0.unwrap();
        let args = [
            "test",
            "web",
            "--allow-routed-test-networks",
            "--skip-vm",
            "db1",
            "--skip-vm",
            "web2",
        ];
        exec(&client, &args, false).await.0.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[4].body,
            json!({ "mode": "disaster", "confirm": "web", "allow_missing": true, "point_in_time": 1_793_491_200u64 })
        );
        assert_eq!(
            seen[5].body,
            json!({ "allow_routed_test_networks": true, "skip_vms": ["db1", "web2"] })
        );
        assert_eq!(
            seen[0].body,
            json!({ "mode": "disaster", "confirm": "web" })
        );
        assert_eq!(seen[2].body, json!({ "mode": "planned", "confirm": "web" }));
        assert_eq!(
            seen[3].line,
            "POST /api/v1/dr-plans/web/actions/cancel HTTP/1.1"
        );
    }

    /// 두 벌 막기 목록은 새 모양(`{vm, source_id}`)과 옛 서버의 이름 목록을 함께 읽는다.
    #[test]
    fn fence_lists_read_both_shapes() {
        assert_eq!(
            fence_names(&json!([{ "vm": "db1", "source_id": "x" }, "web1"])),
            ["db1", "web1"]
        );
        assert!(fence_names(&Value::Null).is_empty());
    }

    /// `dr run --watch`는 바뀔 때만 다시 찍고(빈 줄로 나눔) 끝나면 멈춘다. 닿지 않는 오류는 알리고 다시 읽는다.
    #[tokio::test]
    async fn run_watch_redraws_on_change() {
        let runs = vec![
            run_at("running", &[("a", "starting")], None),
            run_at("running", &[("a", "starting")], None),
            run_at("succeeded", &[("a", "ready")], Some(5)),
        ];
        let (client, _) = run_api(Value::Null, runs).await;
        let (r, out, _) = exec(&client, &["run", "web-test-1", "--watch"], false).await;
        r.unwrap();
        let tables: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(tables.len(), 2, "{out}");
        assert!(tables[0].contains("starting") && tables[1].contains("ready"));
        let (r, out, _) = exec(&client, &["run", "web-test-1", "--watch"], true).await;
        r.unwrap();
        assert_eq!(out.lines().count(), 1, "JSON Lines: one per change");
        // 없는 실행은 기다리지 않고 끝낸다.
        let (client, _) = fake_api(|_, path| not_found(path)).await;
        let (r, _, _) = exec(&client, &["run", "nope", "--watch"], false).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn list_runs_and_report_are_tables() {
        let (client, _) = fake_api(|method, path| match (method, path) {
            ("GET", "/api/v1/dr-plans") => (
                200,
                json!({ "revision": 1, "items": [{ "name": "web", "spec": { "source": "prod", "groups": [] }, "status": {} }] }),
            ),
            ("GET", "/api/v1/dr-plans/web/runs") => (
                200,
                json!({ "items": [{ "name": "web-cleanup-2", "spec": { "kind": "cleanup" },
                    "status": { "state": "failed", "started_at": 0, "error": "boom" } }] }),
            ),
            ("GET", "/api/v1/dr-plans/web/report") => (
                200,
                json!({ "plan": "web", "source": "prod", "source_registered": true, "rpo_target_secs": 86400, "at": 0,
                    "total": 2, "protected": 1,
                    "protection": [{ "vm": "db1", "backup": "db1-b", "point_at": 0, "age_secs": 60, "ok": true },
                        { "vm": "web3", "ok": false, "issue": "no-recovery-point" }],
                    "tests": [{ "name": "web-test-1", "kind": "test", "state": "partial", "started_at": 0, "rto_secs": 300,
                        "failed_vms": ["web3"] }],
                    "last_failover": { "name": "web-failover-9", "kind": "failover-disaster", "state": "succeeded", "rto_secs": 4000 } }),
            ),
            _ => not_found(path),
        })
        .await;
        let (r, out, _) = exec(&client, &["plan", "list"], false).await;
        r.unwrap();
        assert_eq!(
            out,
            "NAME  SOURCE  VMS  RPO-OK  LAST-TEST-RTO  STATE\nweb   prod    0    -       -              idle\n"
        );
        let (r, out, _) = exec(&client, &["runs", "web"], false).await;
        r.unwrap();
        assert_eq!(
            out,
            "NAME           KIND     STATE   STARTED               RTO  READY  ERROR\n\
             web-cleanup-2  cleanup  failed  1970-01-01 00:00 UTC  -    0/0    boom\n"
        );
        let (r, out, _) = exec(&client, &["report", "web"], false).await;
        r.unwrap();
        assert_eq!(
            out,
            "dr-plan web (source prod): 1 of 2 VMs within RPO target 24h, at 1970-01-01 00:00 UTC\n\
             VM    BACKUP  POINT                 AGE  STATUS\n\
             db1   db1-b   1970-01-01 00:00 UTC  60s  ok\n\
             web3  -       -                     -    no recovery point\n\
             \n\
             tests:\n\
             RUN         STATE    STARTED               RTO   FAILED\n\
             web-test-1  partial  1970-01-01 00:00 UTC  >=5m  web3\n\
             last failover: web-failover-9 (failover-disaster) succeeded, RTO 66m\n"
        );
    }

    #[tokio::test]
    async fn reprotect_sends_the_backup_job() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/dr-plans/web/actions/reprotect") => {
                (201, json!({ "name": "dr-reprotect-web" }))
            }
            _ => not_found(path),
        })
        .await;
        let args = [
            "reprotect",
            "web",
            "--remote",
            "prod",
            "--pool",
            "backups",
            "--backup-pool",
            "local-b",
            "--at",
            "02:30",
            "--days",
            "mon,thu",
            "--keep-last",
            "7",
        ];
        let (r, out, _) = exec(&client, &args, false).await;
        r.unwrap();
        assert_eq!(
            out,
            "backup-job dr-reprotect-web set: recovered VMs of plan web replicate to prod/backups\n"
        );
        assert_eq!(
            seen.lock().unwrap()[0].body,
            json!({ "remote": "prod", "pool": "backups", "backup_pool": "local-b",
                "schedule": { "at": "02:30", "days": ["mon", "thu"] },
                "keep": { "last": 7, "daily": 0, "weekly": 0, "monthly": 0 } })
        );
    }
}
