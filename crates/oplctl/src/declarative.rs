//! `orbivisorctl apply·diff·plan·export·set`(계획 73 §9): YAML·JSON 매니페스트 파일을 읽어(여러 문서 `---`, 앵커는 펼친
//! 크기는 파서 예산과 8 MiB 한도) `secretFrom`(환경 변수·파일)을 평문으로 풀고 크기 문자열(`40Gi` — `_bytes`·`_mib` 칸)을 숫자로 바꿔
//! 서버(`POST /apply`)에 보낸다. 계획은 서버가 세운다 — CLI는 보이고 묻고 저장한다. 서버는 YAML을 읽지 않는다.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::client::{Client, seg};
use crate::output::{cell, json as print_json, table};

/// 한 번에 보내는 매니페스트의 최대 크기(서버 한도와 같다)
const MAX_BYTES: usize = 8 << 20;
/// 적용 Task를 기다리는 한도
const APPLY_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
/// 쓰기를 한 도구(설정 이력의 출처)
const CLIENT: &str = concat!("cli/", env!("CARGO_PKG_VERSION"));

/// 끝 코드를 정하는 오류(`diff` — 0 같음, 1 다름, 2 오류). `main`이 메시지를 쓰고 그 코드로 끝낸다.
#[derive(Debug)]
pub struct Exit {
    pub code: u8,
    pub message: Option<String>,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.message {
            Some(m) => f.write_str(m),
            None => write!(f, "exit {}", self.code),
        }
    }
}

impl std::error::Error for Exit {}

/// 매니페스트 파일 인자
#[derive(Args, Clone)]
pub(crate) struct Files {
    /// 매니페스트 파일·디렉터리(`*.yaml`·`*.yml`·`*.json`, 이름 차례). `-`는 표준 입력
    #[arg(short = 'f', long = "filename", value_name = "PATH")]
    files: Vec<PathBuf>,
    /// 디렉터리 아래로 내려간다.
    #[arg(short = 'R', long)]
    recursive: bool,
    /// 적용 묶음: 마지막 선언을 기억해(세 갈래 비교) 든 리소스에 레이블 `orbivisor.io/apply-set`을 단다.
    #[arg(long = "set", value_name = "NAME")]
    set: Option<String>,
    /// 묶음이 기억하는데 파일에 없는 리소스를 지운다(`--set`이 있어야 한다).
    #[arg(long, requires = "set")]
    prune: bool,
    /// 가지치기가 VM·볼륨·백업 저장소도 지운다.
    #[arg(long, requires = "prune")]
    prune_data: bool,
}

#[derive(Args)]
pub(crate) struct ApplyArgs {
    #[command(flatten)]
    files: Files,
    /// `orbivisorctl plan --out`이 저장한 계획을 그대로 적용한다(그사이 바뀌었으면 거절).
    #[arg(long, value_name = "FILE", conflicts_with = "files")]
    plan: Option<PathBuf>,
    /// 바꾸는 까닭(설정 이력에 남는다)
    #[arg(long)]
    reason: Option<String>,
    /// 묻지 않고 적용한다(단말이 아니면 있어야 한다).
    #[arg(short, long)]
    yes: bool,
    /// 적용 Task를 기다리지 않는다.
    #[arg(long)]
    no_wait: bool,
}

#[derive(Args)]
pub(crate) struct PlanArgs {
    #[command(flatten)]
    files: Files,
    /// 계획(id·서버·문서)을 이 파일에 저장한다 — `apply --plan FILE`
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

/// `export`(하위 명령 없이)의 인자: 지금 클러스터를 매니페스트로
#[derive(Args, Clone, Default)]
pub(crate) struct ExportArgs {
    /// 이 종류만(`vm`·`Volume`… 여러 번)
    #[arg(long = "kind", value_name = "KIND")]
    kinds: Vec<String>,
    /// 이 폴더 경로와 그 아래만(`/prod/web`)
    #[arg(long, value_name = "PATH")]
    folder: Option<String>,
    /// 이 적용 묶음이 기억하는 것만
    #[arg(long = "set", value_name = "NAME")]
    set: Option<String>,
    /// 기본값과 같은 칸과 서버가 만든 MAC·주소를 뺀다(사람이 읽을 파일).
    #[arg(long)]
    minimal: bool,
    /// 종류마다 파일(`10-folders.yaml`… 적용 차례 번호)을 이 디렉터리에 쓴다. 없으면 표준 출력
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum SetCmd {
    /// 적용 묶음 목록
    List,
    Get {
        name: String,
    },
    /// 묶음을 지운다(리소스는 그대로, 레이블만 뗀다).
    Delete {
        name: String,
    },
}

/// 파일 하나를 문서들로
fn parse_text(text: &str, path: &Path) -> anyhow::Result<Vec<Value>> {
    let json = path.extension().is_some_and(|e| e == "json");
    if json {
        let v: Value = serde_json::from_str(text).with_context(|| format!("{}", path.display()))?;
        return Ok(match v {
            Value::Array(docs) => docs,
            doc => vec![doc],
        });
    }
    // 별칭 펼침·깊이·크기 한도(빌리언 래프)는 파서의 기본 예산이 막는다. 빈 문서(`---`만)는 건너뛴다.
    serde_saphyr::from_multiple::<Value>(text).with_context(|| format!("{}", path.display()))
}

/// 디렉터리의 매니페스트 파일(이름 차례)
fn files_in(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if recursive {
                files_in(&p, recursive, out)?;
            }
        } else if p
            .extension()
            .is_some_and(|e| e == "yaml" || e == "yml" || e == "json")
        {
            out.push(p);
        }
    }
    Ok(())
}

/// 파일 인자의 모든 문서(크기 문자열·`secretFrom`을 풀고)
pub(crate) fn read_documents(files: &Files) -> anyhow::Result<Vec<Value>> {
    if files.files.is_empty() {
        bail!("give manifest files with -f PATH");
    }
    let mut paths = Vec::new();
    for f in &files.files {
        if f.as_os_str() == "-" || !f.is_dir() {
            paths.push(f.clone());
        } else {
            files_in(f, files.recursive, &mut paths)?;
        }
    }
    let mut docs = Vec::new();
    let mut total = 0;
    for p in paths {
        let text = if p.as_os_str() == "-" {
            let mut s = String::new();
            std::io::stdin()
                .take(MAX_BYTES as u64 + 1)
                .read_to_string(&mut s)?;
            s
        } else {
            std::fs::read_to_string(&p).with_context(|| format!("cannot read {}", p.display()))?
        };
        if text.len() > MAX_BYTES {
            bail!("{} is larger than {MAX_BYTES} bytes", p.display());
        }
        for mut doc in parse_text(&text, &p)? {
            resolve(&mut doc, &p)?;
            total += serde_json::to_vec(&doc)?.len();
            if total > MAX_BYTES {
                bail!(
                    "the manifests are larger than {MAX_BYTES} bytes (after expanding anchors); split them"
                );
            }
            docs.push(doc);
        }
    }
    Ok(docs)
}

/// 크기 문자열(`512`·`40Gi`·`1.5G`는 안 됨 — 정수와 `K`·`Ki`·`M`·`Mi`·`G`·`Gi`·`T`·`Ti`)을 바이트로
pub(crate) fn parse_size(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().with_context(|| format!("not a size: {s:?}"))?;
    let mul: u64 = match unit.trim() {
        "" => 1,
        "K" => 1_000,
        "Ki" => 1 << 10,
        "M" => 1_000_000,
        "Mi" => 1 << 20,
        "G" => 1_000_000_000,
        "Gi" => 1 << 30,
        "T" => 1_000_000_000_000,
        "Ti" => 1 << 40,
        other => bail!("unknown size unit {other:?} in {s:?} (use Ki, Mi, Gi, Ti)"),
    };
    n.checked_mul(mul)
        .with_context(|| format!("size {s:?} is too large"))
}

/// 문서를 보내기 전에 푼다: `{"secretFrom": {"env": X}|{"file": P}}` → 평문, `_bytes`·`_mib` 칸의 크기 문자열 → 숫자.
fn resolve(v: &mut Value, source: &Path) -> anyhow::Result<()> {
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                if let Some(from) = secret_from(child) {
                    *child = Value::String(read_secret(from, source)?);
                    continue;
                }
                if let Value::String(s) = child {
                    if k.ends_with("_bytes") {
                        *child = json!(parse_size(s).with_context(|| k.clone())?);
                    } else if k.ends_with("_mib") {
                        // 단위가 없으면 이미 MiB다.
                        let mib = if s.trim().chars().all(|c| c.is_ascii_digit()) {
                            parse_size(s).with_context(|| k.clone())?
                        } else {
                            let bytes = parse_size(s).with_context(|| k.clone())?;
                            if bytes % (1 << 20) != 0 {
                                bail!("{k}: {s:?} is not a whole number of MiB");
                            }
                            bytes >> 20
                        };
                        *child = json!(mib);
                    }
                    continue;
                }
                resolve(child, source)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                if let Some(from) = secret_from(item) {
                    *item = Value::String(read_secret(from, source)?);
                } else {
                    resolve(item, source)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn secret_from(v: &Value) -> Option<&Map<String, Value>> {
    let m = v.as_object()?;
    if m.len() != 1 {
        return None;
    }
    m.get("secretFrom")?.as_object()
}

fn read_secret(from: &Map<String, Value>, source: &Path) -> anyhow::Result<String> {
    if let Some(name) = from.get("env").and_then(Value::as_str) {
        return std::env::var(name)
            .with_context(|| format!("secretFrom: environment variable {name} is not set"));
    }
    if let Some(file) = from.get("file").and_then(Value::as_str) {
        // 상대 경로는 매니페스트 파일 옆
        let path = match source.parent() {
            Some(dir) if !Path::new(file).is_absolute() && source.as_os_str() != "-" => {
                dir.join(file)
            }
            _ => PathBuf::from(file),
        };
        let s = std::fs::read_to_string(&path)
            .with_context(|| format!("secretFrom: cannot read {}", path.display()))?;
        return Ok(s.trim_end_matches(['\n', '\r']).to_string());
    }
    bail!("secretFrom needs env or file")
}

/// `POST /apply` 본문
fn body(files: &Files, documents: Vec<Value>) -> Value {
    let mut b = json!({ "documents": documents });
    if let Some(set) = &files.set {
        b["apply_set"] = json!(set);
    }
    if files.prune {
        b["prune"] = json!(true);
    }
    if files.prune_data {
        b["prune_data"] = json!(true);
    }
    b
}

async fn post(
    client: &Client,
    path: &str,
    body: &Value,
    reason: Option<&str>,
) -> anyhow::Result<Value> {
    let mut r = client
        .request(Method::POST, path)
        .header("orbivisor-client", CLIENT)
        .json(body);
    if let Some(reason) = reason {
        r = r.header("orbivisor-change-reason", reason);
    }
    client.send(r).await
}

fn summary(plan: &Value) -> [u64; 5] {
    let s = &plan["summary"];
    ["create", "update", "delete", "unchanged", "blocked"].map(|k| s[k].as_u64().unwrap_or(0))
}

fn short(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(59).collect::<String>())
    } else {
        s
    }
}

/// 계획을 사람이 읽는 꼴로(`+` 만들기, `~` 바꾸기, `-` 지우기, `!` 막힘)
pub(crate) fn print_plan(out: &mut dyn Write, plan: &Value) -> std::io::Result<()> {
    for step in plan["steps"].as_array().into_iter().flatten() {
        let mark = match step["action"].as_str() {
            Some("create") => "+",
            Some("update") => "~",
            Some("delete") => "-",
            Some("blocked") => "!",
            _ => continue,
        };
        let folder = step["folder"]
            .as_str()
            .map_or_else(String::new, |f| format!(" ({f})"));
        writeln!(
            out,
            "{mark} {} {}{folder}",
            cell(&step["kind"]),
            cell(&step["name"])
        )?;
        if mark == "~" {
            for c in step["diff"].as_array().into_iter().flatten() {
                let line = match c["op"].as_str() {
                    Some("add") => format!("    + {}: {}", cell(&c["path"]), short(&c["new"])),
                    Some("remove") => format!("    - {}: {}", cell(&c["path"]), short(&c["old"])),
                    _ => format!(
                        "    ~ {}: {} -> {}",
                        cell(&c["path"]),
                        short(&c["old"]),
                        short(&c["new"])
                    ),
                };
                writeln!(out, "{line}")?;
            }
        }
        if step["restart"] == true {
            writeln!(
                out,
                "    note: the VM is running; this hardware change applies by hotplug or at its next restart"
            )?;
        }
        for p in step["problems"].as_array().into_iter().flatten() {
            let level = if p["level"] == "warning" {
                "warning"
            } else {
                "error"
            };
            writeln!(
                out,
                "    {level}: {} ({})",
                cell(&p["message"]),
                cell(&p["code"])
            )?;
        }
    }
    let [c, u, d, k, b] = summary(plan);
    writeln!(
        out,
        "Plan: {c} to create, {u} to update, {d} to delete, {k} unchanged{}.",
        if b > 0 {
            format!(", {b} blocked")
        } else {
            String::new()
        }
    )
}

/// `apply`
pub(crate) async fn apply(
    client: &Client,
    args: ApplyArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let reason = args.reason.as_deref();
    let started = if let Some(file) = &args.plan {
        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(file)
                .with_context(|| format!("cannot read {}", file.display()))?,
        )
        .with_context(|| format!("{} is not a saved plan", file.display()))?;
        if saved["server"].as_str().map(|s| s.trim_end_matches('/')) != Some(client.base.as_str()) {
            bail!(
                "the plan was made on {}; plans live on the node that made them (use the same --server)",
                cell(&saved["server"])
            );
        }
        let mut b = saved["request"].clone();
        b["plan"] = saved["plan"].clone();
        post(client, "/apply", &b, reason).await?
    } else {
        let documents = read_documents(&args.files)?;
        let b = body(&args.files, documents);
        let plan = post(client, "/apply?dry_run=true", &b, None).await?;
        if json_out {
            print_json(out, &plan)?;
        } else {
            print_plan(out, &plan)?;
        }
        let [c, u, d, _, blocked] = summary(&plan);
        if blocked > 0 {
            bail!("{blocked} step(s) are blocked; nothing was applied");
        }
        if c + u + d == 0 {
            if !json_out {
                writeln!(out, "No changes.")?;
            }
            return Ok(());
        }
        if !args.yes {
            if !std::io::stdin().is_terminal() {
                bail!("not a terminal: give --yes to apply");
            }
            eprint!("Apply these changes? [y/N] ");
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim(), "y" | "Y" | "yes") {
                bail!("not applied");
            }
        }
        // 보인 계획 그대로(그사이 바뀌었으면 서버가 412로 거절한다)
        let mut b = b;
        b["plan"] = plan["plan"].clone();
        post(client, "/apply", &b, reason).await?
    };
    let task = &started["task"];
    if args.no_wait {
        if json_out {
            print_json(out, &started)?;
        } else {
            writeln!(out, "apply started (task {})", cell(&task["id"]))?;
        }
        return Ok(());
    }
    let done = client.wait_task_for(task, APPLY_WAIT).await?;
    if json_out {
        print_json(out, &done)?;
    } else {
        let s = &started["plan"]["summary"];
        writeln!(
            out,
            "Applied: {} created, {} updated, {} deleted.",
            cell(&s["create"]),
            cell(&s["update"]),
            cell(&s["delete"])
        )?;
    }
    Ok(())
}

/// `plan`: 계획을 보이고(`--out`이면 저장한다)
pub(crate) async fn plan(
    client: &Client,
    args: PlanArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let documents = read_documents(&args.files)?;
    let b = body(&args.files, documents);
    let plan = post(client, "/apply?dry_run=true", &b, None).await?;
    if json_out {
        print_json(out, &plan)?;
    } else {
        print_plan(out, &plan)?;
    }
    if let Some(path) = &args.out {
        let saved = json!({ "server": client.base, "plan": plan["plan"], "request": b });
        std::fs::write(path, serde_json::to_vec_pretty(&saved)?)
            .with_context(|| format!("cannot write {}", path.display()))?;
        if !json_out {
            writeln!(
                out,
                "Saved the plan to {} (valid 30 minutes on this server).",
                path.display()
            )?;
        }
    }
    Ok(())
}

/// `diff`: 끝 코드 0 같음, 1 다름, 2 오류·막힘(`kubectl diff`와 같은 약속)
pub(crate) async fn diff(
    client: &Client,
    files: Files,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let fail = |e: anyhow::Error| Exit {
        code: 2,
        message: Some(format!("{e:#}")),
    };
    let documents = read_documents(&files).map_err(fail)?;
    let plan = post(
        client,
        "/apply?dry_run=true",
        &body(&files, documents),
        None,
    )
    .await
    .map_err(fail)?;
    if json_out {
        print_json(out, &plan)?;
    } else {
        print_plan(out, &plan)?;
    }
    let [c, u, d, _, blocked] = summary(&plan);
    if blocked > 0 {
        return Err(Exit {
            code: 2,
            message: Some(format!("{blocked} step(s) are blocked")),
        }
        .into());
    }
    if c + u + d > 0 {
        return Err(Exit {
            code: 1,
            message: None,
        }
        .into());
    }
    Ok(())
}

/// 종류의 내보내기 파일 이름(적용 차례 번호)
fn export_file(kind: &str) -> String {
    let order = [
        ("Folder", "folders"),
        ("StoragePool", "storage-pools"),
        ("Network", "networks"),
        ("SecurityGroup", "security-groups"),
        ("Router", "routers"),
        ("Volume", "volumes"),
        ("Vm", "vms"),
        ("FloatingIp", "floating-ips"),
        ("PlacementRule", "placement-rules"),
    ];
    match order.iter().position(|(k, _)| *k == kind) {
        Some(i) => format!("{}-{}.yaml", (i + 1) * 10, order[i].1),
        None => format!("99-{}.yaml", kind.to_lowercase()),
    }
}

/// 문서들을 YAML(`---`로 잇기)로
pub(crate) fn to_yaml(docs: &[Value]) -> anyhow::Result<String> {
    let mut out = String::new();
    for d in docs {
        out.push_str("---\n");
        out.push_str(&serde_saphyr::to_string(d)?);
    }
    Ok(out)
}

/// `export`(하위 명령 없이)
pub(crate) async fn export(
    client: &Client,
    args: ExportArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut query = Vec::new();
    if !args.kinds.is_empty() {
        query.push(format!("kinds={}", seg(&args.kinds.join(","))));
    }
    if let Some(f) = &args.folder {
        query.push(format!("folder={}", seg(f)));
    }
    if let Some(s) = &args.set {
        query.push(format!("apply_set={}", seg(s)));
    }
    if args.minimal {
        query.push("minimal=true".into());
    }
    let path = if query.is_empty() {
        "/manifests".to_string()
    } else {
        format!("/manifests?{}", query.join("&"))
    };
    let list = client.get(&path).await?;
    let docs = list["documents"].as_array().cloned().unwrap_or_default();
    if let Some(dir) = &args.dir {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot make {}", dir.display()))?;
        let mut files: Vec<(String, Vec<Value>)> = Vec::new();
        for d in docs {
            let name = export_file(d["kind"].as_str().unwrap_or_default());
            match files.iter_mut().find(|(n, _)| *n == name) {
                Some((_, list)) => list.push(d),
                None => files.push((name, vec![d])),
            }
        }
        for (name, list) in &files {
            let path = dir.join(name);
            std::fs::write(&path, to_yaml(list)?)
                .with_context(|| format!("cannot write {}", path.display()))?;
            writeln!(out, "{} ({} documents)", path.display(), list.len())?;
        }
        return Ok(());
    }
    if json_out {
        return Ok(print_json(out, &Value::Array(docs))?);
    }
    write!(out, "{}", to_yaml(&docs)?)?;
    Ok(())
}

/// `set …`
pub(crate) async fn set(
    client: &Client,
    cmd: SetCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SetCmd::List => {
            let list = client.get("/apply-sets").await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                return Ok(print_json(out, &Value::Array(items))?);
            }
            let rows: Vec<Vec<String>> = items
                .iter()
                .map(|s| {
                    let st = &s["status"];
                    vec![
                        cell(&s["name"]),
                        st["objects"].as_array().map_or(0, |o| o.len()).to_string(),
                        cell(&st["applied_by"]),
                        st["last_error"].as_str().unwrap_or("").to_string(),
                    ]
                })
                .collect();
            table(out, &["NAME", "OBJECTS", "APPLIED BY", "ERROR"], &rows)?;
        }
        SetCmd::Get { name } => {
            let set = client.get(&format!("/apply-sets/{}", seg(&name))).await?;
            print_json(out, &set)?;
        }
        SetCmd::Delete { name } => {
            client
                .delete(&format!("/apply-sets/{}", seg(&name)))
                .await?;
            writeln!(
                out,
                "apply-set {name} deleted (its resources keep running; the label was removed)"
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;

    fn files(paths: &[&Path]) -> Files {
        Files {
            files: paths.iter().map(|p| p.to_path_buf()).collect(),
            recursive: false,
            set: None,
            prune: false,
            prune_data: false,
        }
    }

    #[test]
    fn sizes_and_secrets_are_resolved_before_sending() {
        assert_eq!(parse_size("40Gi").unwrap(), 40 << 30);
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("2G").unwrap(), 2_000_000_000);
        assert!(parse_size("1.5Gi").is_err());
        assert!(parse_size("3Qi").is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pw.txt"), "s3cret\n").unwrap();
        let yaml = dir.path().join("web.yaml");
        std::fs::write(
            &yaml,
            "apiVersion: orbivisor.io/v1\nkind: Volume\nmetadata: { name: d }\nspec: { pool: local, size_bytes: 40Gi }\n---\n\
             apiVersion: orbivisor.io/v1\nkind: Vm\nmetadata: { name: v }\nspec:\n  memory_mib: 2Gi\n  cpu: { cores: 1 }\n\
             \x20 cloud_init: { password: { secretFrom: { file: pw.txt } }, user_data: { secretFrom: { env: PATH } } }\n---\n",
        )
        .unwrap();
        let docs = read_documents(&files(&[&yaml])).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0]["spec"]["size_bytes"], json!(40u64 << 30));
        assert_eq!(docs[1]["spec"]["memory_mib"], json!(2048));
        assert_eq!(docs[1]["spec"]["cloud_init"]["password"], "s3cret");
        assert_eq!(
            docs[1]["spec"]["cloud_init"]["user_data"],
            std::env::var("PATH").unwrap()
        );
        // 디렉터리는 이름 차례로, 다른 확장자는 건너뛴다.
        std::fs::write(dir.path().join("a.json"), r#"[{"kind":"Folder"}]"#).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let docs = read_documents(&files(&[dir.path()])).unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0]["kind"], "Folder");
        // 없는 환경 변수는 오류
        std::fs::write(
            &yaml,
            "spec: { token: { secretFrom: { env: OPLCTL_TEST_UNSET_VAR } } }\n",
        )
        .unwrap();
        let e = read_documents(&files(&[&yaml])).unwrap_err();
        assert!(format!("{e:#}").contains("OPLCTL_TEST_UNSET_VAR"), "{e:#}");
    }

    #[test]
    fn yaml_written_for_export_reads_back_the_same() {
        let docs = vec![
            json!({ "apiVersion": "orbivisor.io/v1", "kind": "Folder", "metadata": { "name": "prod" }, "spec": { "kind": "vm" } }),
            json!({ "apiVersion": "orbivisor.io/v1", "kind": "Vm", "metadata": { "name": "web", "folder": "/prod" },
                "spec": { "nics": [{ "mac": "52:54:00:00:00:01", "network": "on" }], "memory_mib": 512 } }),
        ];
        let text = to_yaml(&docs).unwrap();
        assert_eq!(parse_text(&text, Path::new("x.yaml")).unwrap(), docs);
        assert_eq!(export_file("Vm"), "70-vms.yaml");
        assert_eq!(export_file("Folder"), "10-folders.yaml");
    }

    /// 별칭을 겹겹이 펼치는 문서(빌리언 래프)는 파서 예산에서 멈춘다. 작은 앵커는 받는다.
    #[test]
    fn alias_bombs_are_refused_and_small_anchors_work() {
        let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..10 {
            let prev = format!("*a{}", i - 1);
            bomb.push_str(&format!("a{i}: &a{i} [{}]\n", vec![prev; 10].join(", ")));
        }
        assert!(parse_text(&bomb, Path::new("bomb.yaml")).is_err());
        let ok = "base: &b { cores: 2 }\nspec: { cpu: *b }\n";
        let docs = parse_text(ok, Path::new("ok.yaml")).unwrap();
        assert_eq!(docs[0]["spec"]["cpu"]["cores"], 2);
    }

    #[test]
    fn plans_are_printed_with_marks() {
        let plan = json!({
            "steps": [
                { "kind": "Folder", "name": "prod", "action": "unchanged" },
                { "kind": "Vm", "name": "web", "folder": "/prod", "action": "update", "restart": true,
                  "diff": [{ "path": "memory_mib", "op": "replace", "old": 512, "new": 1024 }] },
                { "kind": "Volume", "name": "d", "action": "create" },
                { "kind": "Vm", "name": "old", "action": "blocked",
                  "problems": [{ "code": "protected", "message": "holds data", "level": "error" }] },
            ],
            "summary": { "create": 1, "update": 1, "delete": 0, "unchanged": 1, "blocked": 1 },
        });
        let mut out = Vec::new();
        print_plan(&mut out, &plan).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "~ Vm web (/prod)\n    ~ memory_mib: 512 -> 1024\n    note: the VM is running; this hardware change applies by hotplug or at its next restart\n\
             + Volume d\n! Vm old\n    error: holds data (protected)\n\
             Plan: 1 to create, 1 to update, 0 to delete, 1 unchanged, 1 blocked.\n"
        );
    }

    #[tokio::test]
    async fn apply_plans_then_applies_the_shown_plan_and_diff_sets_the_exit_code() {
        let plan = json!({ "plan": "p1", "steps": [{ "kind": "Folder", "name": "a", "action": "create" }],
            "summary": { "create": 1, "update": 0, "delete": 0, "unchanged": 0, "blocked": 0 } });
        let reply_plan = plan.clone();
        let (client, seen) = fake_api(move |method, path| match (method, path) {
            ("POST", "/api/v1/apply?dry_run=true") => (200, reply_plan.clone()),
            ("POST", "/api/v1/apply") => (
                202,
                json!({ "plan": reply_plan.clone(), "task": { "id": "t1" } }),
            ),
            ("GET", "/api/v1/tasks/t1") => (
                200,
                json!({ "id": "t1", "status": { "state": "succeeded" } }),
            ),
            _ => (404, json!({ "code": "not_found", "message": "no" })),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.yaml");
        std::fs::write(&file, "apiVersion: orbivisor.io/v1\nkind: Folder\nmetadata: { name: a }\nspec: { kind: vm }\n").unwrap();
        let mut f = files(&[&file]);
        f.set = Some("web".into());
        let args = ApplyArgs {
            files: f.clone(),
            plan: None,
            reason: Some("INC-1".into()),
            yes: true,
            no_wait: false,
        };
        let mut out = Vec::new();
        apply(&client, args, false, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("+ Folder a") && text.contains("Applied: 1 created"),
            "{text}"
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0].line, "POST /api/v1/apply?dry_run=true HTTP/1.1");
        assert_eq!(seen[0].body["apply_set"], "web");
        assert_eq!(seen[0].body["documents"][0]["kind"], "Folder");
        // 보인 계획 id로 적용한다.
        assert_eq!(seen[1].line, "POST /api/v1/apply HTTP/1.1");
        assert_eq!(seen[1].body["plan"], "p1");

        // diff: 바뀜이 있으면 끝 코드 1
        let e = diff(&client, f.clone(), false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(e.downcast_ref::<Exit>().unwrap().code, 1);
        // 읽을 수 없는 파일은 2
        let e = diff(
            &client,
            files(&[Path::new("/no/such/file.yaml")]),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.downcast_ref::<Exit>().unwrap().code, 2);

        // plan --out → apply --plan
        let saved = dir.path().join("plan.json");
        plan_cmd(&client, f, &saved).await;
        let saved_json: Value =
            serde_json::from_str(&std::fs::read_to_string(&saved).unwrap()).unwrap();
        assert_eq!(saved_json["plan"], "p1");
        let args = ApplyArgs {
            files: files(&[]),
            plan: Some(saved),
            reason: None,
            yes: false,
            no_wait: true,
        };
        let mut out = Vec::new();
        apply(&client, args, false, &mut out).await.unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("apply started (task t1)")
        );
    }

    async fn plan_cmd(client: &Client, files: Files, out_file: &Path) {
        let args = PlanArgs {
            files,
            out: Some(out_file.to_path_buf()),
        };
        plan(client, args, false, &mut Vec::new()).await.unwrap();
    }
}
