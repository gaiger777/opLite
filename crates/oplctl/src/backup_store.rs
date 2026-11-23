//! `orbivisorctl backup-store …`(계획 68 §14): 백업 저장소(백업 풀 안·S3, 암호화, GC·검증 일정) 만들기·목록·바꾸기·지우기,
//! GC·검증·연결 확인·scan·넘겨받기, 키 내보내기·escrow. 그리고 백업의 저장소 동작(`backup verify|convert|copy`)과
//! `vm live-restore status|cancel`. 비밀(S3 비밀 키·복구 키·암호 문구)은 명령줄이 아니라 파일로 받는다.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::client::{BACKUP_WAIT, Client, seg};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum BackupStoreCmd {
    /// 저장소를 만든다(초기화 Task를 기다린다). 새 키(`--encrypt`)면 복구 키를 한 번 보인다 — 잃으면 백업을 되살릴 수 없다.
    Create(Box<StoreCreate>),
    /// 저장소 목록(백엔드·암호화·스냅샷 수·저장 크기·dedup 배율·마지막 GC·검증)
    List,
    /// 저장소(JSON, 비밀 없음)
    Get { store: String },
    /// 일정·동시 요청 수·S3 자격 증명·CA를 바꾼다(백엔드·암호화는 바꾸지 못한다).
    Set(StoreSet),
    /// 저장소 리소스를 지운다. 저장소 안 데이터(버킷·디렉터리)는 남는다. 백업이 남아 있으면 거절한다.
    Delete { store: String },
    /// 가비지 수집을 지금 돌린다.
    Gc {
        store: String,
        /// 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
    /// 검증을 지금 돌린다(백업을 주지 않으면 일정의 규칙대로 고른다).
    Verify {
        store: String,
        /// 검증할 백업(여러 번)
        #[arg(long = "backup", value_name = "BACKUP")]
        backups: Vec<String>,
        #[arg(long)]
        wait: bool,
    },
    /// 이 노드가 저장소에 닿는지와 노드마다 보고한 닿음
    Check { store: String },
    /// 읽기 전용으로 붙인 저장소의 백업을 가져온다(다시 하면 새것만).
    Scan {
        store: String,
        #[arg(long)]
        wait: bool,
    },
    /// 읽기 전용으로 붙인 저장소의 주인이 된다(옛 주인의 하트비트가 24시간보다 오래됐을 때, 아니면 `--force`).
    TakeOver {
        store: String,
        /// 옛 주인이 아직 살아 있어도(두 클러스터가 서로의 백업을 지울 수 있다)
        #[arg(long)]
        force: bool,
    },
    /// 암호화 키: 복구 키 다시 보기(admin, 감사에 남음), 저장소 안 escrow 쓰기
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum KeyCmd {
    /// 복구 키를 보인다.
    Export { store: String },
    /// 주 키를 암호 문구로 감싸 저장소 안에 둔다(클러스터를 잃어도 저장소와 암호 문구로 연다).
    Escrow {
        store: String,
        /// 암호 문구가 든 파일(첫 줄)
        #[arg(long, value_name = "FILE")]
        passphrase_file: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum VerifyModeArg {
    Full,
    Metadata,
}

#[derive(Args)]
#[command(group(ArgGroup::new("backend").required(true).args(["pool", "s3_endpoint"])))]
#[command(group(ArgGroup::new("key").args(["encrypt", "recovery_key_file", "escrow_unlock_file"])))]
pub(crate) struct StoreCreate {
    name: String,
    /// 백업 풀(`--backups`를 켠 dir·NFS 풀) 안 저장소
    #[arg(long)]
    pool: Option<String>,
    /// S3 끝점 `https://host[:port]`
    #[arg(long, value_name = "URL", requires_all = ["bucket", "access_key_id", "secret_key_file"])]
    s3_endpoint: Option<String>,
    #[arg(long)]
    bucket: Option<String>,
    /// 버킷 안 접두사
    #[arg(long)]
    prefix: Option<String>,
    /// 기본 us-east-1
    #[arg(long)]
    region: Option<String>,
    /// 경로 방식 주소(MinIO·Ceph RGW)
    #[arg(long)]
    path_style: bool,
    /// 끝점 인증서를 확인할 CA(PEM 파일). 없으면 시스템 루트.
    #[arg(long, value_name = "FILE")]
    ca_file: Option<PathBuf>,
    #[arg(long, value_name = "ID")]
    access_key_id: Option<String>,
    /// S3 비밀 키가 든 파일(첫 줄)
    #[arg(long, value_name = "FILE")]
    secret_key_file: Option<PathBuf>,
    /// `http` 끝점을 허락한다(시험용 — 자격 증명과 데이터가 평문으로 오간다)
    #[arg(long)]
    insecure_http: bool,
    /// 새 키로 암호화한다(복구 키를 보인다).
    #[arg(long)]
    encrypt: bool,
    /// 만들 때 주 키를 이 파일의 암호 문구로 감싸 저장소 안에 둔다.
    #[arg(long, value_name = "FILE")]
    escrow_passphrase_file: Option<PathBuf>,
    /// 이 복구 키(파일)로 연다(다른 클러스터의 저장소·사본).
    #[arg(long, value_name = "FILE")]
    recovery_key_file: Option<PathBuf>,
    /// 저장소 안 escrow를 이 암호 문구(파일)로 풀어 연다.
    #[arg(long, value_name = "FILE")]
    escrow_unlock_file: Option<PathBuf>,
    /// 다른 클러스터가 주인인 저장소를 복원용으로 붙인다.
    #[arg(long)]
    read_only: bool,
    /// 읽기 전용으로 붙이는 백업 풀 안 저장소의 id(`{풀}/store/<id>/`)
    #[arg(long, value_name = "UUID")]
    store_id: Option<String>,
    /// GC 시각(UTC, 기본 05:00)
    #[arg(long, value_name = "HH:MM")]
    gc_at: Option<String>,
    /// GC 요일(쉼표, 기본 매일)
    #[arg(long, value_name = "mon,tue,...")]
    gc_days: Option<String>,
    /// 예약 검증 시각(UTC). 주면 예약 검증을 켠다.
    #[arg(long, value_name = "HH:MM")]
    verify_at: Option<String>,
    /// 예약 검증 요일(쉼표, 기본 매일)
    #[arg(long, value_name = "mon,tue,...")]
    verify_days: Option<String>,
    /// 이 날 수보다 오래전에 검증한 백업을 다시 검증한다(기본 30).
    #[arg(long, value_name = "N")]
    reverify_days: Option<u32>,
    #[arg(long, value_enum)]
    verify_mode: Option<VerifyModeArg>,
    /// 새 백업이 ready가 되면 곧바로 검증한다.
    #[arg(long)]
    verify_new: bool,
    /// S3 동시 요청 수(1~64, 기본 8)
    #[arg(long, value_name = "N")]
    max_streams: Option<u8>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "gc_at", "gc_days", "verify_at", "verify_days", "reverify_days", "verify_mode", "verify_new", "no_verify",
    "max_streams", "access_key_id", "ca_file", "no_ca",
])))]
pub(crate) struct StoreSet {
    store: String,
    #[arg(long, value_name = "HH:MM")]
    gc_at: Option<String>,
    #[arg(long, value_name = "mon,tue,...")]
    gc_days: Option<String>,
    #[arg(long, value_name = "HH:MM")]
    verify_at: Option<String>,
    #[arg(long, value_name = "mon,tue,...")]
    verify_days: Option<String>,
    #[arg(long, value_name = "N")]
    reverify_days: Option<u32>,
    #[arg(long, value_enum)]
    verify_mode: Option<VerifyModeArg>,
    #[arg(long, value_name = "true|false")]
    verify_new: Option<bool>,
    /// 예약 검증을 끈다.
    #[arg(long, conflicts_with_all = ["verify_at", "verify_days", "reverify_days", "verify_mode", "verify_new"])]
    no_verify: bool,
    #[arg(long, value_name = "N")]
    max_streams: Option<u8>,
    /// 새 S3 자격 증명(비밀 키는 `--secret-key-file`)
    #[arg(long, value_name = "ID", requires = "secret_key_file")]
    access_key_id: Option<String>,
    #[arg(long, value_name = "FILE")]
    secret_key_file: Option<PathBuf>,
    #[arg(long, value_name = "FILE", conflicts_with = "no_ca")]
    ca_file: Option<PathBuf>,
    /// 사용자 CA를 지우고 시스템 루트로
    #[arg(long)]
    no_ca: bool,
}

#[derive(Subcommand)]
pub(crate) enum LiveRestoreCmd {
    /// 라이브 복원 단계·진행·까닭
    Status { vm: String },
    /// 라이브 복원을 취소한다: VM을 끄고 VM·대상 볼륨을 지운다(게스트가 쓴 것을 버린다 — `--discard`가 있어야 한다).
    Cancel {
        vm: String,
        #[arg(long)]
        discard: bool,
    },
}

/// 비밀 파일의 첫 줄(끝 공백 뺌)
fn secret(path: &PathBuf) -> anyhow::Result<String> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    if line.is_empty() {
        bail!("{} is empty", path.display());
    }
    Ok(line)
}

/// `mon,tue`를 요일 목록으로
fn days(s: &str) -> anyhow::Result<Value> {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let mut out = Vec::new();
    for d in s.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        let d = d.to_ascii_lowercase();
        if !DAYS.contains(&d.as_str()) {
            bail!("invalid day {d:?}: use mon,tue,wed,thu,fri,sat,sun");
        }
        out.push(d);
    }
    Ok(json!(out))
}

fn mode(m: VerifyModeArg) -> &'static str {
    match m {
        VerifyModeArg::Full => "full",
        VerifyModeArg::Metadata => "metadata",
    }
}

/// `backup-store create`의 요청 본문
fn create_body(a: &StoreCreate) -> anyhow::Result<Value> {
    let backend = match (&a.pool, &a.s3_endpoint) {
        (Some(pool), _) => json!({ "pool": pool }),
        (None, Some(endpoint)) => {
            let mut s3 = json!({
                "endpoint": endpoint,
                "bucket": a.bucket,
                "access_key_id": a.access_key_id,
                "secret_access_key": secret(a.secret_key_file.as_ref().context("--secret-key-file is required")?)?,
            });
            if let Some(p) = &a.prefix {
                s3["prefix"] = json!(p);
            }
            if let Some(r) = &a.region {
                s3["region"] = json!(r);
            }
            if a.path_style {
                s3["path_style"] = json!(true);
            }
            if a.insecure_http {
                s3["insecure_http"] = json!(true);
            }
            if let Some(f) = &a.ca_file {
                s3["ca_pem"] = json!(
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))?
                );
            }
            json!({ "s3": s3 })
        }
        (None, None) => bail!("give --pool or --s3-endpoint"),
    };
    let mut spec = json!({ "backend": backend });
    if a.read_only {
        spec["read_only"] = json!(true);
    }
    if let Some(id) = &a.store_id {
        spec["store_id"] = json!(id);
    }
    if a.gc_at.is_some() || a.gc_days.is_some() {
        spec["gc"] = json!({
            "at": a.gc_at.clone().unwrap_or_else(|| "05:00".into()),
            "days": a.gc_days.as_deref().map(days).transpose()?.unwrap_or_else(|| json!([])),
        });
    }
    if let Some(at) = &a.verify_at {
        let mut v = json!({
            "schedule": { "at": at, "days": a.verify_days.as_deref().map(days).transpose()?.unwrap_or_else(|| json!([])) },
        });
        if let Some(n) = a.reverify_days {
            v["reverify_days"] = json!(n);
        }
        if let Some(m) = a.verify_mode {
            v["mode"] = json!(mode(m));
        }
        if a.verify_new {
            v["verify_new"] = json!(true);
        }
        spec["verify"] = v;
    } else if a.verify_days.is_some()
        || a.reverify_days.is_some()
        || a.verify_mode.is_some()
        || a.verify_new
    {
        bail!("scheduled verification needs --verify-at");
    }
    if let Some(n) = a.max_streams {
        spec["max_streams"] = json!(n);
    }
    let mut body = json!({ "name": a.name, "spec": spec });
    if a.encrypt {
        body["encryption"] = json!({ "generate": true });
    } else if let Some(f) = &a.recovery_key_file {
        body["encryption"] = json!({ "recovery_key": secret(f)? });
    } else if let Some(f) = &a.escrow_unlock_file {
        body["encryption"] = json!({ "escrow_passphrase": secret(f)? });
    }
    if let Some(f) = &a.escrow_passphrase_file {
        body["escrow_passphrase"] = json!(secret(f)?);
    }
    Ok(body)
}

/// `backup-store set`의 요청 본문. 일정은 통째로 바꾸므로 주지 않은 칸은 지금 값(`current`)으로 채운다.
fn set_body(a: &StoreSet, current: &Value) -> anyhow::Result<Value> {
    let mut body = json!({});
    let spec = &current["spec"];
    if a.gc_at.is_some() || a.gc_days.is_some() {
        body["gc"] = json!({
            "at": a.gc_at.clone().map_or_else(|| spec["gc"]["at"].clone(), |v| json!(v)),
            "days": match &a.gc_days { Some(d) => days(d)?, None => spec["gc"]["days"].clone() },
        });
    }
    if a.no_verify {
        body["verify"] = Value::Null;
    } else if a.verify_at.is_some()
        || a.verify_days.is_some()
        || a.reverify_days.is_some()
        || a.verify_mode.is_some()
        || a.verify_new.is_some()
    {
        let cur = &spec["verify"];
        let at = a
            .verify_at
            .clone()
            .map(|v| json!(v))
            .or_else(|| cur["schedule"]["at"].as_str().map(|s| json!(s)))
            .context("scheduled verification is off; give --verify-at")?;
        body["verify"] = json!({
            "schedule": {
                "at": at,
                "days": match &a.verify_days { Some(d) => days(d)?, None => cur["schedule"]["days"].as_array().map_or_else(|| json!([]), |d| json!(d)) },
            },
            "reverify_days": a.reverify_days.map_or_else(|| cur["reverify_days"].as_u64().map_or(json!(30), |n| json!(n)), |n| json!(n)),
            "mode": a.verify_mode.map_or_else(|| cur["mode"].as_str().map_or(json!("full"), |m| json!(m)), |m| json!(mode(m))),
            "verify_new": a.verify_new.unwrap_or_else(|| cur["verify_new"].as_bool().unwrap_or(false)),
        });
    }
    if let Some(n) = a.max_streams {
        body["max_streams"] = json!(n);
    }
    if let Some(id) = &a.access_key_id {
        let file = a
            .secret_key_file
            .as_ref()
            .context("--secret-key-file is required")?;
        body["s3_credentials"] = json!({ "access_key_id": id, "secret_access_key": secret(file)? });
    }
    if let Some(f) = &a.ca_file {
        body["ca_pem"] = json!(
            std::fs::read_to_string(f).with_context(|| format!("cannot read {}", f.display()))?
        );
    } else if a.no_ca {
        body["ca_pem"] = Value::Null;
    }
    Ok(body)
}

const STORE_HEADERS: &[&str] = &[
    "NAME",
    "BACKEND",
    "ENCRYPTED",
    "SNAPSHOTS",
    "STORED",
    "DEDUP",
    "LAST-GC",
    "LAST-VERIFY",
    "PHASE",
];

/// `backup-store list`의 한 행. DEDUP은 논리 바이트 ÷ 저장 바이트(마지막 GC가 잰 것).
fn store_row(s: &Value) -> Vec<String> {
    let backend = &s["spec"]["backend"];
    let backend = if let Some(p) = backend["pool"].as_str() {
        format!("pool:{p}")
    } else {
        let s3 = &backend["s3"];
        let mut t = format!("s3:{}", cell(&s3["bucket"]));
        if let Some(p) = s3["prefix"].as_str() {
            t.push('/');
            t.push_str(p.trim_end_matches('/'));
        }
        t
    };
    let mut encrypted = s["spec"]["encryption"]["fingerprint"]
        .as_str()
        .map_or_else(|| "no".into(), |f| f.chars().take(11).collect::<String>());
    if s["spec"]["read_only"] == true {
        encrypted.push_str(" (ro)");
    }
    let usage = &s["status"]["usage"];
    let dedup = match (
        usage["logical_bytes"].as_u64(),
        usage["stored_bytes"].as_u64(),
    ) {
        (Some(l), Some(st)) if st > 0 => format!("{:.1}x", l as f64 / st as f64),
        _ => "-".into(),
    };
    let when = |v: &Value| {
        v["finished_at"]
            .as_i64()
            .map_or_else(|| "-".to_string(), crate::cli::unix_time)
    };
    let gc = &s["status"]["last_gc"];
    let gc = if gc.is_null() {
        "-".into()
    } else if gc["error"].is_string() {
        format!("{} (failed)", when(gc))
    } else {
        when(gc)
    };
    let v = &s["status"]["last_verify"];
    let verify = if v.is_null() {
        "-".into()
    } else if v["bad"].as_u64().unwrap_or(0) > 0 || v["error"].is_string() {
        format!("{} ({} bad)", when(v), v["bad"].as_u64().unwrap_or(0))
    } else {
        when(v)
    };
    vec![
        cell(&s["name"]),
        backend,
        encrypted,
        usage["snapshots"]
            .as_u64()
            .map_or_else(|| "-".into(), |n| n.to_string()),
        if usage["stored_bytes"].is_u64() {
            bytes(&usage["stored_bytes"])
        } else {
            "-".into()
        },
        dedup,
        gc,
        verify,
        cell(&s["status"]["phase"]),
    ]
}

/// Task를 만든 응답을 알리고 `wait`이면 끝날 때까지 기다린다.
async fn started(
    client: &Client,
    task: &Value,
    wait: bool,
    what: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let id = cell(&task["id"]);
    if wait {
        let done = client.wait_task_for(task, BACKUP_WAIT).await?;
        match done["status"]["warning"].as_str() {
            Some(w) => writeln!(out, "{what} done: {w}")?,
            None => writeln!(out, "{what} done")?,
        }
    } else {
        writeln!(
            out,
            "{what} started (task {id}); follow it with `orbivisorctl task get {id}`"
        )?;
    }
    Ok(())
}

pub(crate) async fn backup_store(
    client: &Client,
    cmd: BackupStoreCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupStoreCmd::Create(a) => {
            let body = create_body(&a)?;
            let created = client.post("/backup-stores", &body).await?;
            if json_out {
                print_json(out, &created)?;
            }
            // 복구 키는 초기화를 기다리기 전에 보인다(초기화가 실패해도 키는 이미 리소스에 있다).
            if let Some(key) = created["recovery_key"].as_str()
                && !json_out
            {
                writeln!(out, "recovery key: {key}")?;
                writeln!(
                    out,
                    "store it now; backups in this store cannot be recovered without it"
                )?;
            }
            client
                .wait_task_for(&created["task"], BACKUP_WAIT)
                .await
                .with_context(|| format!("backup store {} was not initialized", a.name))?;
            if !json_out {
                writeln!(out, "backup store {} created", a.name)?;
            }
        }
        BackupStoreCmd::List => {
            let list = client.get("/backup-stores").await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &Value::Array(items))?;
            } else {
                let rows: Vec<_> = items.iter().map(store_row).collect();
                table(out, STORE_HEADERS, &rows)?;
            }
        }
        BackupStoreCmd::Get { store } => {
            print_json(
                out,
                &client
                    .get(&format!("/backup-stores/{}", seg(&store)))
                    .await?,
            )?;
        }
        BackupStoreCmd::Set(a) => {
            let path = format!("/backup-stores/{}", seg(&a.store));
            let current = client.get(&path).await?;
            let body = set_body(&a, &current)?;
            let s = client.patch(&path, &body).await?;
            if json_out {
                print_json(out, &s)?;
            } else {
                writeln!(out, "backup store {} updated", a.store)?;
            }
        }
        BackupStoreCmd::Delete { store } => {
            client
                .delete(&format!("/backup-stores/{}", seg(&store)))
                .await?;
            writeln!(
                out,
                "backup store {store} deleted; its data is left at the store location"
            )?;
        }
        BackupStoreCmd::Gc { store, wait } => {
            let task = client
                .post(
                    &format!("/backup-stores/{}/actions/gc", seg(&store)),
                    &json!({}),
                )
                .await?;
            started(
                client,
                &task,
                wait,
                &format!("GC of backup store {store}"),
                out,
            )
            .await?;
        }
        BackupStoreCmd::Verify {
            store,
            backups,
            wait,
        } => {
            let task = client
                .post(
                    &format!("/backup-stores/{}/actions/verify", seg(&store)),
                    &json!({ "backups": backups }),
                )
                .await?;
            started(
                client,
                &task,
                wait,
                &format!("verification of backup store {store}"),
                out,
            )
            .await?;
        }
        BackupStoreCmd::Check { store } => {
            let c = client
                .post(
                    &format!("/backup-stores/{}/actions/check", seg(&store)),
                    &json!({}),
                )
                .await?;
            if json_out {
                print_json(out, &c)?;
            } else {
                if c["checked_here"] == true {
                    match c["error"].as_str() {
                        None => writeln!(out, "this node: ok")?,
                        Some(e) => writeln!(out, "this node: {e}")?,
                    }
                } else {
                    writeln!(out, "this node: does not use the store")?;
                }
                let rows: Vec<Vec<String>> = c["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|n| {
                        vec![
                            cell(&n["name"]),
                            if n["ok"] == true { "yes" } else { "no" }.into(),
                            n["checked_at"]
                                .as_i64()
                                .map_or_else(|| "-".into(), crate::cli::unix_time),
                            n["error"].as_str().unwrap_or("-").to_string(),
                        ]
                    })
                    .collect();
                table(out, &["NODE", "REACHES", "CHECKED", "ERROR"], &rows)?;
            }
        }
        BackupStoreCmd::Scan { store, wait } => {
            let task = client
                .post(
                    &format!("/backup-stores/{}/actions/scan", seg(&store)),
                    &json!({}),
                )
                .await?;
            started(
                client,
                &task,
                wait,
                &format!("scan of backup store {store}"),
                out,
            )
            .await?;
        }
        BackupStoreCmd::TakeOver { store, force } => {
            client
                .post(
                    &format!("/backup-stores/{}/actions/take-over", seg(&store)),
                    &json!({ "force": force }),
                )
                .await?;
            writeln!(out, "backup store {store} is now owned by this cluster")?;
        }
        BackupStoreCmd::Key {
            cmd: KeyCmd::Export { store },
        } => {
            let k = client
                .post(
                    &format!("/backup-stores/{}/key/export", seg(&store)),
                    &json!({}),
                )
                .await?;
            writeln!(out, "{}", cell(&k["recovery_key"]))?;
        }
        BackupStoreCmd::Key {
            cmd:
                KeyCmd::Escrow {
                    store,
                    passphrase_file,
                },
        } => {
            client
                .post(
                    &format!("/backup-stores/{}/key/escrow", seg(&store)),
                    &json!({ "passphrase": secret(&passphrase_file)? }),
                )
                .await?;
            writeln!(
                out,
                "the key of backup store {store} is escrowed in the store"
            )?;
        }
    }
    Ok(())
}

/// `backup verify B`: 저장소의 검증 Task. 기다리면 결과(`status.verify`)를 보인다.
pub(crate) async fn backup_verify(
    client: &Client,
    backup: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(
            &format!("/backups/{}/actions/verify", seg(backup)),
            &json!({}),
        )
        .await?;
    if !wait {
        return started(
            client,
            &task,
            false,
            &format!("verification of backup {backup}"),
            out,
        )
        .await;
    }
    client.wait_task_for(&task, BACKUP_WAIT).await?;
    let b = client.get(&format!("/backups/{}", seg(backup))).await?;
    let v = &b["status"]["verify"];
    match v["state"].as_str() {
        Some("ok") => writeln!(out, "backup {backup} verified: ok")?,
        Some(state) => writeln!(
            out,
            "backup {backup} verified: {state} ({} bad chunks{})",
            v["bad_chunks"].as_u64().unwrap_or(0),
            v["error"]
                .as_str()
                .map(|e| format!(", {e}"))
                .unwrap_or_default()
        )?,
        None => writeln!(out, "backup {backup} was not verified")?,
    }
    Ok(())
}

/// `backup convert B --store S` 또는 `--all-in-pool P --store S`: 옛 풀 백업을 저장소로 옮긴다(원본은 남는다). 풀 전체는
/// `ready` 백업을 만든 차례로 하나씩(이미 옮긴 것은 건너뛴다).
pub(crate) async fn backup_convert(
    client: &Client,
    backup: Option<&str>,
    all_in_pool: Option<&str>,
    store: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let names: Vec<String> = match (backup, all_in_pool) {
        (Some(b), None) => vec![b.to_string()],
        (None, Some(pool)) => {
            let list = client.get("/backups").await?;
            let mut mine: Vec<Value> = crate::cli::items(&list)
                .into_iter()
                .filter(|b| b["spec"]["pool"] == pool && b["status"]["phase"] == "ready")
                .collect();
            mine.sort_by_key(|b| (b["status"]["created_at"].as_u64(), cell(&b["id"])));
            mine.iter().map(|b| cell(&b["name"])).collect()
        }
        _ => bail!("give a backup or --all-in-pool"),
    };
    let mut failed = 0;
    for name in &names {
        let r = client
            .post(
                &format!("/backups/{}/actions/convert", seg(name)),
                &json!({ "store": store }),
            )
            .await;
        let task = match r {
            Ok(task) => task,
            Err(e)
                if all_in_pool.is_some() && e.to_string().contains("already in backup store") =>
            {
                writeln!(out, "backup {name}: already in store {store}")?;
                continue;
            }
            Err(e) if all_in_pool.is_some() => {
                writeln!(out, "backup {name}: {e}")?;
                failed += 1;
                continue;
            }
            Err(e) => return Err(e),
        };
        match client.wait_task_for(&task, BACKUP_WAIT).await {
            Ok(_) => writeln!(
                out,
                "backup {name} moved to store {store} (the original is kept; delete it when done)"
            )?,
            Err(e) if all_in_pool.is_some() => {
                writeln!(out, "backup {name}: {e}")?;
                failed += 1;
            }
            Err(e) => return Err(e),
        }
    }
    if failed > 0 {
        bail!("{failed} of {} backup(s) could not be moved", names.len());
    }
    Ok(())
}

/// `backup copy B --store S`: 다른 저장소로 복사한다(없는 청크만).
pub(crate) async fn backup_copy(
    client: &Client,
    backup: &str,
    store: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(
            &format!("/backups/{}/actions/copy", seg(backup)),
            &json!({ "store": store }),
        )
        .await?;
    started(
        client,
        &task,
        wait,
        &format!("copy of backup {backup} to store {store}"),
        out,
    )
    .await
}

/// `vm live-restore status|cancel`
pub(crate) async fn live_restore(
    client: &Client,
    cmd: LiveRestoreCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        LiveRestoreCmd::Status { vm } => {
            let v = client.get(&format!("/vms/{}", seg(&vm))).await?;
            let lr = &v["status"]["live_restore"];
            if json_out {
                return Ok(print_json(out, lr)?);
            }
            if lr.is_null() {
                writeln!(out, "vm {vm} is not being live-restored")?;
                return Ok(());
            }
            let (done, total) = (
                lr["done_bytes"].as_u64().unwrap_or(0),
                lr["total_bytes"].as_u64().unwrap_or(0),
            );
            let pct = if total > 0 {
                format!(" {:.0}%", done as f64 * 100.0 / total as f64)
            } else {
                String::new()
            };
            writeln!(
                out,
                "vm {vm}: {}{pct} ({} of {}){}",
                cell(&lr["phase"]),
                bytes(&json!(done)),
                bytes(&json!(total)),
                lr["error"]
                    .as_str()
                    .map(|e| format!(" — {e}"))
                    .unwrap_or_default()
            )?;
        }
        LiveRestoreCmd::Cancel { vm, discard } => {
            if !discard {
                bail!(
                    "cancelling deletes the VM, its new disks and what the guest wrote; confirm with --discard"
                );
            }
            let task = client
                .post(
                    &format!("/vms/{}/live-restore/cancel", seg(&vm)),
                    &json!({ "discard": true }),
                )
                .await?;
            writeln!(
                out,
                "live restore of vm {vm} is being cancelled (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(subcommand)]
        cmd: BackupStoreCmd,
    }

    #[test]
    fn create_reads_secrets_from_files_and_builds_the_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "s3cr3t\n").unwrap();
        let pass = dir.path().join("pass");
        std::fs::write(&pass, "correct horse battery\n").unwrap();
        let t = T::try_parse_from([
            "t",
            "create",
            "s3",
            "--s3-endpoint",
            "https://s3.example.com",
            "--bucket",
            "bk1",
            "--prefix",
            "orbi",
            "--path-style",
            "--access-key-id",
            "AKID",
            "--secret-key-file",
            key.to_str().unwrap(),
            "--encrypt",
            "--escrow-passphrase-file",
            pass.to_str().unwrap(),
            "--verify-at",
            "06:00",
            "--verify-days",
            "sun",
            "--verify-new",
        ])
        .unwrap();
        let BackupStoreCmd::Create(a) = t.cmd else {
            panic!()
        };
        let body = create_body(&a).unwrap();
        assert_eq!(body["spec"]["backend"]["s3"]["secret_access_key"], "s3cr3t");
        assert_eq!(body["spec"]["backend"]["s3"]["path_style"], true);
        assert_eq!(body["encryption"], json!({ "generate": true }));
        assert_eq!(body["escrow_passphrase"], "correct horse battery");
        assert_eq!(
            body["spec"]["verify"],
            json!({ "schedule": { "at": "06:00", "days": ["sun"] }, "verify_new": true })
        );
        // 백엔드는 하나, 키 고르기도 하나
        assert!(T::try_parse_from(["t", "create", "x"]).is_err());
        assert!(
            T::try_parse_from([
                "t",
                "create",
                "x",
                "--pool",
                "bk",
                "--encrypt",
                "--recovery-key-file",
                "k"
            ])
            .is_err()
        );
    }

    #[test]
    fn list_rows_show_backend_key_and_dedup() {
        let s = json!({
            "name": "st", "spec": { "backend": { "s3": { "bucket": "bk1", "prefix": "orbi/" } },
                                    "encryption": { "fingerprint": "ab:cd:ef:01:23:45:67:89:aa:bb:cc:dd:ee:ff:00:11" } },
            "status": { "phase": "ready", "usage": { "snapshots": 3, "stored_bytes": 1073741824u64,
                        "logical_bytes": 4294967296u64, "chunks": 10, "measured_at": 1 } }
        });
        let row = store_row(&s);
        assert_eq!(row[1], "s3:bk1/orbi");
        assert_eq!(row[2], "ab:cd:ef:01");
        assert_eq!(row[3], "3");
        assert_eq!(row[5], "4.0x");
        assert_eq!(row[8], "ready");
    }

    #[test]
    fn set_keeps_the_schedule_parts_not_given() {
        let t = T::try_parse_from(["t", "set", "st", "--verify-days", "sat,sun"]).unwrap();
        let BackupStoreCmd::Set(a) = t.cmd else {
            panic!()
        };
        let current = json!({ "spec": { "verify": { "schedule": { "at": "06:00", "days": [] },
                                                     "reverify_days": 10, "mode": "metadata" } } });
        let body = set_body(&a, &current).unwrap();
        assert_eq!(
            body["verify"],
            json!({ "schedule": { "at": "06:00", "days": ["sat", "sun"] }, "reverify_days": 10,
                    "mode": "metadata", "verify_new": false })
        );
        let t = T::try_parse_from(["t", "set", "st", "--no-verify"]).unwrap();
        let BackupStoreCmd::Set(a) = t.cmd else {
            panic!()
        };
        assert_eq!(set_body(&a, &current).unwrap(), json!({ "verify": null }));
    }
}
