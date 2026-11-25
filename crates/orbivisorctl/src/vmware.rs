//! `orbivisorctl vmware source …`·`vmware migration …`·`vmware vm …`(계획 72 §13): 저장한 vCenter·ESXi 연결(지문 확인 흐름,
//! 비밀번호는 파일·`ORBIVISOR_VMWARE_PASSWORD`·터미널에서), 인벤토리 찾아보기, 이전 계획 만들기·미리 확인·시작·멈춤·이어 가기·
//! 취소·지우기, 계획 안 VM 하나의 다시 하기·취소·전환·되돌리기·마무리.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};

use crate::client::{ApiError, BACKUP_WAIT, Client, seg};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum VmwareCmd {
    /// 저장한 vCenter·ESXi 연결(`cluster.admin`): 더하기·목록·보기·바꾸기·다시 확인·지우기·찾아보기
    Source {
        #[command(subcommand)]
        cmd: SourceCmd,
    },
    /// 이전 계획: 원본의 VM 여러 대를 같은 매핑 규칙으로 옮긴다(루트 `vm.create`).
    Migration {
        #[command(subcommand)]
        cmd: MigrationCmd,
    },
    /// 계획 안 VM 하나(항목 이름·원본 이름·moref·id)
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum VmCmd {
    /// 실패한 VM을 다시 한다(복사를 시작하지 않았으면 미리 확인부터, 아니면 체크포인트부터). 오래 전환하지 않아 멈춘
    /// 동기화도 다시 시작한다.
    Retry { vm: String },
    /// VM 하나를 취소한다: 원본의 Orbivisor 스냅샷·우리가 켠 CBT·대상 볼륨을 치운다(원본 VM·디스크는 그대로).
    Cancel {
        vm: String,
        /// 정리 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
    /// VM 하나를 전환한다: (따뜻한 이전은 켜진 채 델타 한 번) 원본을 끄고 마지막 델타를 옮긴 뒤 Orbivisor VM을 만들어 켠다.
    /// 어느 걸음에서 실패해도 원본을 다시 켜서 되돌린다.
    Cutover {
        vm: String,
        /// 전환이 끝날 때까지 기다리고 내린 시간을 알린다.
        #[arg(long)]
        wait: bool,
    },
    /// 전환한 VM을 되돌린다: Orbivisor VM을 끄고(지우지 않음) 원본을 다시 켠다. Orbivisor VM에서 쓴 데이터는 원본으로 가지
    /// 않는다.
    Revert {
        vm: String,
        /// 확인: Orbivisor VM 이름
        #[arg(long, value_name = "VM_NAME")]
        confirm: String,
        #[arg(long)]
        wait: bool,
    },
    /// 전환을 마무리한다: 원본의 Orbivisor 스냅샷·우리가 켠 CBT를 치운다(원본 보호 설정은 남김, 되돌릴 수 없다).
    Finalize {
        vm: String,
        #[arg(long)]
        wait: bool,
    },
}

/// 비밀번호를 받는 곳(인자로는 받지 않는다)
#[derive(Args, Clone, Default)]
pub(crate) struct PasswordArgs {
    /// 비밀번호가 든 파일. 없으면 `ORBIVISOR_VMWARE_PASSWORD`, 그것도 없으면 터미널에서 묻는다.
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum SourceCmd {
    /// 연결을 더한다. 지문을 주지 않으면 서버 인증서의 지문을 알려 준다(확인하고 `--fingerprint`로 다시).
    Add {
        name: String,
        /// vCenter·ESXi 주소
        #[arg(long)]
        host: String,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        user: String,
        #[command(flatten)]
        password: PasswordArgs,
        /// 믿을 서버 인증서의 SHA-256 지문
        #[arg(long)]
        fingerprint: Option<String>,
        /// 디스크를 VDDK(운영자가 설치한 nbdkit `vddk` 플러그인)로 읽는다 — vSAN·vVols에 필요
        #[arg(long)]
        vddk: bool,
        /// VDDK 라이브러리 디렉터리(없으면 노드 설정 `[vmware] vddk_libdir`)
        #[arg(long, value_name = "DIR", requires = "vddk")]
        vddk_libdir: Option<String>,
        /// 데이터센터 이름 힌트(ESXi는 `ha-datacenter`)
        #[arg(long)]
        datacenter: Option<String>,
    },
    List,
    Get {
        source: String,
    },
    /// 바꾼다(주어진 것만). 바꾸면 서버가 다시 로그인해 본 뒤 저장한다.
    Set {
        source: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        user: Option<String>,
        /// 새 비밀번호를 받는다(`--password-file`·`ORBIVISOR_VMWARE_PASSWORD`·터미널).
        #[arg(long)]
        password: bool,
        #[command(flatten)]
        password_from: PasswordArgs,
        #[arg(long)]
        fingerprint: Option<String>,
        /// VDDK로 읽는다.
        #[arg(long, conflicts_with = "no_vddk")]
        vddk: bool,
        /// `/folder` 범위 읽기로 돌아간다.
        #[arg(long)]
        no_vddk: bool,
        #[arg(long, value_name = "DIR", requires = "vddk")]
        vddk_libdir: Option<String>,
        #[arg(long)]
        datacenter: Option<String>,
    },
    /// 다시 로그인해 종류·판을 확인한다.
    Check {
        source: String,
    },
    Delete {
        source: String,
    },
    /// 인벤토리의 VM 표(`PATH NAME POWER OS CPU MEMORY DISKS SNAPSHOTS CBT`). `-o json`이면 인벤토리 전체.
    Browse {
        source: String,
        /// 이 경로(`DC1/vm/Prod`)로 시작하는 VM만
        #[arg(long)]
        path: Option<String>,
        /// 노드 캐시(60초)를 쓰지 않고 다시 읽는다.
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum FolderRule {
    /// 모두 한 폴더(`--folder`, 없으면 루트)에
    Flat,
    /// VMware VM 폴더 경로를 `--folder`(없으면 루트) 아래에 만든다.
    Mirror,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Shutdown {
    /// 게스트 종료(VMware Tools 필요)
    Guest,
    PowerOff,
}

/// `vmware migration create`의 인자
#[derive(Args)]
pub(crate) struct CreateArgs {
    name: String,
    /// 원본 연결
    #[arg(long)]
    source: String,
    /// 켜진 채 복사하고 CBT 델타를 옮긴 뒤 전환 때만 끈다.
    #[arg(long, conflicts_with = "cold", required_unless_present = "cold")]
    warm: bool,
    /// 꺼진 VM을 통째로 옮긴다.
    #[arg(long)]
    cold: bool,
    /// 옮길 VM(인벤토리 경로 `DC1/vm/Prod/web01`, moref `vm-42` 또는 instanceUuid). 여러 번 준다.
    #[arg(long = "vm", value_name = "VM", required = true)]
    vms: Vec<String>,
    /// 원본 네트워크(이름 또는 `moref:dvportgroup-12`) → 우리 네트워크
    #[arg(long = "net", value_name = "FROM=TO", value_parser = parse_pair)]
    nets: Vec<(String, String)>,
    /// 원본 데이터스토어 → 우리 풀
    #[arg(long = "datastore", value_name = "FROM=POOL", value_parser = parse_pair)]
    datastores: Vec<(String, String)>,
    /// 규칙에 없는 데이터스토어의 디스크를 둘 풀
    #[arg(long)]
    pool: Option<String>,
    #[arg(long, value_enum)]
    folders: Option<FolderRule>,
    /// 만든 VM을 둘 폴더(경로 `/team` 또는 id) — `--folders mirror`면 그 아래
    #[arg(long)]
    folder: Option<String>,
    /// 대상 노드(없으면 시작할 때 고른다)
    #[arg(long)]
    node: Option<String>,
    /// 동시 동기화·전환 수
    #[arg(long, value_name = "N")]
    parallel: Option<u16>,
    /// ESXi 호스트마다 동시 수
    #[arg(long, value_name = "N")]
    per_host: Option<u16>,
    /// 대상 노드마다 동시 수
    #[arg(long, value_name = "N")]
    per_node: Option<u16>,
    /// 따뜻한 이전의 델타 주기(`45m`, `2h`, 분 수)
    #[arg(long, value_name = "INTERVAL", value_parser = parse_minutes)]
    sync_every: Option<u32>,
    /// 예약 전환 시각(RFC 3339, 예: `2026-12-01T02:00:00Z`)
    #[arg(long, value_name = "TIME", value_parser = parse_time)]
    cutover_at: Option<u64>,
    #[arg(long, value_enum)]
    shutdown: Option<Shutdown>,
    /// Windows VM에 CD로 붙일 드라이버 ISO 볼륨(virtio-win)
    #[arg(long, value_name = "ISO")]
    driver_iso: Option<String>,
    /// 원본 MAC을 쓰지 않고 새 MAC을 받는다.
    #[arg(long)]
    new_macs: bool,
    /// 전환 끝에 대상 VM을 켜지 않는다.
    #[arg(long)]
    no_start: bool,
}

#[derive(Subcommand)]
pub(crate) enum MigrationCmd {
    /// 계획을 만든다(`draft` — 미리 확인하고 `start`).
    Create(Box<CreateArgs>),
    /// VM마다 미리 확인(표 `VM LEVEL PROBLEMS`). 막힌 VM이 있으면 끝 코드 1.
    Preflight {
        migration: String,
        /// 이 VM만(항목 이름·원본 이름·moref)
        #[arg(long = "vm", value_name = "VM")]
        vms: Vec<String>,
        /// 인벤토리를 다시 읽는다.
        #[arg(long)]
        refresh: bool,
    },
    List,
    /// 계획과 VM 표(`VM STATE ROUND COPIED LAST-DELTA ESTIMATE NEXT-SYNC`)
    Get {
        migration: String,
    },
    /// 미리 확인하고 시작한다(막힌 VM은 `failed`, 나머지는 차례를 기다린다).
    Start {
        migration: String,
    },
    Pause {
        migration: String,
    },
    Resume {
        migration: String,
    },
    Cancel {
        migration: String,
    },
    /// 끝났거나 시작하지 않은 계획을 지운다(만든 VM·원본은 그대로).
    Delete {
        migration: String,
    },
}

fn parse_pair(s: &str) -> Result<(String, String), String> {
    // 원본 이름에 `=`가 있을 수 있다. 우리 이름에는 없다.
    match s.rsplit_once('=') {
        Some((from, to)) if !from.is_empty() && !to.is_empty() => Ok((from.into(), to.into())),
        _ => Err(format!("invalid mapping {s:?}: use FROM=TO")),
    }
}

/// `45m`·`2h`·`90`(분) → 분
fn parse_minutes(s: &str) -> Result<u32, String> {
    let bad = || format!("invalid interval {s:?}: use 45m, 2h or minutes");
    let (n, mult) = if let Some(h) = s.strip_suffix('h') {
        (h, 60)
    } else {
        (s.strip_suffix('m').unwrap_or(s), 1)
    };
    n.parse::<u32>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .ok_or_else(bad)
}

/// RFC 3339 → 유닉스 초
fn parse_time(s: &str) -> Result<u64, String> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .map_err(|e| {
            format!("invalid time {s:?}: {e} (use RFC 3339, e.g. 2026-12-01T02:00:00Z)")
        })?;
    u64::try_from(t.unix_timestamp()).map_err(|_| format!("time {s:?} is before 1970"))
}

/// 비밀번호: `--password-file`, `ORBIVISOR_VMWARE_PASSWORD`, 터미널 순서
fn password(args: &PasswordArgs, prompt: &str) -> anyhow::Result<String> {
    if let Some(file) = &args.password_file {
        return Ok(std::fs::read_to_string(file)
            .with_context(|| format!("cannot read {}", file.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string());
    }
    match orbivisor_types::legacy::env("VMWARE_PASSWORD") {
        Some(p) => Ok(p),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

/// 지문이 없으면(409 `untrusted_certificate`) 서버 지문을 확인하고 다시 하라고 알린다.
fn trust_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.code == "untrusted_certificate" || api.code == "fingerprint_mismatch" => {
            let fingerprint = cell(&api.details["fingerprint"]);
            anyhow::anyhow!(
                "the vSphere certificate fingerprint is {fingerprint}; check it and rerun with --fingerprint {fingerprint}"
            )
        }
        _ => e,
    }
}

const SOURCE_HEADERS: &[&str] = &[
    "NAME",
    "HOST",
    "USER",
    "KIND",
    "VERSION",
    "TRANSPORT",
    "ERROR",
];

fn source_row(s: &Value) -> Vec<String> {
    let spec = &s["spec"];
    let st = &s["status"];
    let host = match spec["port"].as_u64() {
        Some(443) | None => cell(&spec["host"]),
        Some(p) => format!("{}:{p}", cell(&spec["host"])),
    };
    vec![
        cell(&s["name"]),
        host,
        cell(&spec["user"]),
        cell(&st["kind"]),
        cell(&st["version"]),
        if spec["transport"].get("vddk").is_some() {
            "vddk".into()
        } else {
            "folder".into()
        },
        st["error"].as_str().unwrap_or("").to_string(),
    ]
}

const BROWSE_HEADERS: &[&str] = &[
    "PATH",
    "NAME",
    "POWER",
    "OS",
    "CPU",
    "MEMORY",
    "DISKS",
    "SNAPSHOTS",
    "CBT",
];

fn vm_row(v: &Value) -> Vec<String> {
    let disks = v["disks"].as_array().cloned().unwrap_or_default();
    let size: u64 = disks.iter().filter_map(|d| d["capacity"].as_u64()).sum();
    vec![
        cell(&v["path"]),
        cell(&v["name"]),
        cell(&v["power"]),
        cell(&v["guest_id"]),
        cell(&v["cpus"]),
        bytes(&json!(v["memory_mib"].as_u64().unwrap_or(0) << 20)),
        format!("{} ({})", disks.len(), bytes(&json!(size))),
        v["snapshots"].as_array().map_or(0, Vec::len).to_string(),
        if v["cbt"] == json!(true) { "on" } else { "off" }.into(),
    ]
}

pub(crate) async fn vmware(
    client: &Client,
    cmd: VmwareCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmwareCmd::Source { cmd } => Box::pin(source(client, cmd, json_out, out)).await,
        VmwareCmd::Migration { cmd } => Box::pin(migration(client, cmd, json_out, out)).await,
        VmwareCmd::Vm { cmd } => Box::pin(vm(client, cmd, json_out, out)).await,
    }
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let (vm, action, wait, body) = match &cmd {
        VmCmd::Retry { vm } => (vm, "retry", false, json!({})),
        VmCmd::Cancel { vm, wait } => (vm, "cancel", *wait, json!({})),
        VmCmd::Cutover { vm, wait } => (vm, "cutover", *wait, json!({})),
        VmCmd::Revert { vm, confirm, wait } => (vm, "revert", *wait, json!({ "confirm": confirm })),
        VmCmd::Finalize { vm, wait } => (vm, "finalize", *wait, json!({})),
    };
    let got = client
        .post(&format!("/vmware-vms/{}/{action}", seg(vm)), &body)
        .await?;
    if json_out && !wait {
        return Ok(print_json(out, &got)?);
    }
    let name = cell(&got["item"]["name"]);
    let task = got.get("task").filter(|t| t.is_string());
    if let (VmCmd::Cutover { .. } | VmCmd::Revert { .. } | VmCmd::Finalize { .. }, Some(task)) =
        (&cmd, task)
    {
        if !wait {
            let doing = match action {
                "cutover" => "being cut over",
                "revert" => "being reverted",
                _ => "being finalized",
            };
            writeln!(out, "vmware-vm {name} is {doing} (task {})", cell(task))?;
            return Ok(());
        }
        client
            .wait_task_for(&json!({ "id": task }), BACKUP_WAIT)
            .await?;
        let item = client.get(&format!("/vmware-vms/{}", seg(vm))).await?;
        if json_out {
            return Ok(print_json(out, &item)?);
        }
        let state = cell(&item["status"]["state"]);
        match item["status"]["downtime_s"].as_u64() {
            Some(s) if action == "cutover" => {
                writeln!(out, "vmware-vm {name} is {state} (downtime {s}s)")?
            }
            _ => writeln!(out, "vmware-vm {name} is {state}")?,
        }
        return Ok(());
    }
    match (&cmd, task) {
        (VmCmd::Retry { .. }, _) => writeln!(
            out,
            "vmware-vm {name} is {} again",
            cell(&got["item"]["status"]["state"])
        )?,
        (VmCmd::Cancel { .. }, None) => writeln!(out, "vmware-vm {name} cancelled")?,
        (VmCmd::Cancel { .. }, Some(task)) if wait => {
            client
                .wait_task_for(&json!({ "id": task }), BACKUP_WAIT)
                .await?;
            writeln!(out, "vmware-vm {name} cancelled")?;
        }
        (VmCmd::Cancel { .. }, Some(task)) => writeln!(
            out,
            "vmware-vm {name} is being cancelled (task {})",
            cell(task)
        )?,
        // 전환·되돌리기·마무리는 늘 Task가 있다(위에서 다룸).
        (_, _) => writeln!(out, "vmware-vm {name}")?,
    }
    Ok(())
}

async fn source(
    client: &Client,
    cmd: SourceCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SourceCmd::Add {
            name,
            host,
            port,
            user,
            password: from,
            fingerprint,
            vddk,
            vddk_libdir,
            datacenter,
        } => {
            let mut body = json!({ "name": name, "host": host, "user": user });
            if let Some(p) = port {
                body["port"] = json!(p);
            }
            if let Some(f) = &fingerprint {
                body["fingerprint"] = json!(f);
            }
            if vddk {
                body["vddk"] = json!({ "libdir": vddk_libdir });
            }
            if let Some(d) = datacenter {
                body["datacenter_hint"] = json!(d);
            }
            if fingerprint.is_none() {
                // 지문을 먼저 받는다(비밀번호는 보내지 않는다 — 서버는 연결만 한다).
                body["password"] = json!("-");
                return match client.post("/vmware-sources", &body).await {
                    Ok(_) => Err(anyhow::anyhow!(
                        "the server added the source without a fingerprint"
                    )),
                    Err(e) => Err(trust_error(e)),
                };
            }
            body["password"] = json!(password(&from, &format!("password for {user}@{host}: "))?);
            let got = client
                .post("/vmware-sources", &body)
                .await
                .map_err(trust_error)?;
            writeln!(
                out,
                "vmware-source {} added ({} {})",
                cell(&got["name"]),
                cell(&got["status"]["kind"]),
                cell(&got["status"]["version"])
            )?;
        }
        SourceCmd::List => {
            let list = client.get("/vmware-sources").await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                return Ok(print_json(out, &Value::Array(items))?);
            }
            let rows: Vec<Vec<String>> = items.iter().map(source_row).collect();
            table(out, SOURCE_HEADERS, &rows)?;
        }
        SourceCmd::Get { source } => {
            print_json(
                out,
                &client
                    .get(&format!("/vmware-sources/{}", seg(&source)))
                    .await?,
            )?;
        }
        SourceCmd::Set {
            source,
            host,
            port,
            user,
            password: change,
            password_from,
            fingerprint,
            vddk,
            no_vddk,
            vddk_libdir,
            datacenter,
        } => {
            let mut body = Map::new();
            for (k, v) in [
                ("host", host),
                ("user", user),
                ("fingerprint", fingerprint),
                ("datacenter_hint", datacenter),
            ] {
                if let Some(v) = v {
                    body.insert(k.into(), json!(v));
                }
            }
            if let Some(p) = port {
                body.insert("port".into(), json!(p));
            }
            if change || password_from.password_file.is_some() {
                body.insert(
                    "password".into(),
                    json!(password(&password_from, "new password: ")?),
                );
            }
            if vddk {
                body.insert("vddk".into(), json!({ "libdir": vddk_libdir }));
            } else if no_vddk {
                body.insert("vddk".into(), Value::Null);
            }
            anyhow::ensure!(!body.is_empty(), "nothing to change");
            let got = client
                .patch(
                    &format!("/vmware-sources/{}", seg(&source)),
                    &Value::Object(body),
                )
                .await
                .map_err(trust_error)?;
            writeln!(out, "vmware-source {} updated", cell(&got["name"]))?;
        }
        SourceCmd::Check { source } => {
            let got = client
                .post(
                    &format!("/vmware-sources/{}/actions/check", seg(&source)),
                    &json!({}),
                )
                .await
                .map_err(trust_error)?;
            writeln!(
                out,
                "vmware-source {} ok ({} {} {})",
                cell(&got["name"]),
                cell(&got["status"]["kind"]),
                cell(&got["status"]["version"]),
                cell(&got["status"]["build"])
            )?;
        }
        SourceCmd::Delete { source } => {
            client
                .delete(&format!("/vmware-sources/{}", seg(&source)))
                .await?;
            writeln!(out, "vmware-source {source} deleted")?;
        }
        SourceCmd::Browse {
            source,
            path,
            refresh,
        } => {
            let mut url = format!("/vmware-sources/{}/inventory", seg(&source));
            if refresh {
                url.push_str("?refresh=true");
            }
            let got = client.get(&url).await?;
            if json_out {
                return Ok(print_json(out, &got)?);
            }
            let rows: Vec<Vec<String>> = got["inventory"]["vms"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|v| {
                    path.as_deref()
                        .is_none_or(|p| v["path"].as_str().is_some_and(|vp| vp.starts_with(p)))
                })
                .map(vm_row)
                .collect();
            table(out, BROWSE_HEADERS, &rows)?;
        }
    }
    Ok(())
}

/// `migration create`의 요청 본문(청하지 않은 것은 보내지 않는다 — 서버 기본값)
async fn create_body(client: &Client, a: &CreateArgs) -> anyhow::Result<Value> {
    let mut rules = Map::new();
    let pairs = |list: &[(String, String)]| -> Value {
        list.iter()
            .map(|(from, to)| json!({ "from": from, "to": to }))
            .collect()
    };
    if !a.nets.is_empty() {
        rules.insert("networks".into(), pairs(&a.nets));
    }
    if !a.datastores.is_empty() {
        rules.insert("datastores".into(), pairs(&a.datastores));
    }
    if let Some(p) = &a.pool {
        rules.insert("default_pool".into(), json!(p));
    }
    let folder = match &a.folder {
        Some(f) => crate::access::resolve_folder(client, "vm", f).await?,
        None => None,
    };
    match (a.folders, folder) {
        (Some(FolderRule::Mirror), f) => {
            rules.insert("folders".into(), json!({ "kind": "mirror", "under": f }));
        }
        (_, Some(f)) => {
            rules.insert("folders".into(), json!({ "kind": "flat", "into": f }));
        }
        _ => {}
    }
    if let Some(n) = &a.node {
        let node = client.get(&format!("/nodes/{}", seg(n))).await?;
        rules.insert("node".into(), node["id"].clone());
    }
    if let Some(iso) = &a.driver_iso {
        rules.insert("driver_iso".into(), json!(iso));
    }
    if a.new_macs {
        rules.insert("keep_macs".into(), json!(false));
    }
    if a.no_start {
        rules.insert("start_after".into(), json!(false));
    }
    let mut body = json!({
        "name": a.name,
        "source": a.source,
        "mode": if a.cold { "cold" } else { "warm" },
        "vms": a.vms.iter().map(|v| json!({ "ref": v })).collect::<Vec<_>>(),
    });
    if !rules.is_empty() {
        body["rules"] = Value::Object(rules);
    }
    let mut limits = Map::new();
    for (k, v) in [
        ("max_parallel", a.parallel),
        ("max_per_host", a.per_host),
        ("max_per_node", a.per_node),
    ] {
        if let Some(v) = v {
            limits.insert(k.into(), json!(v));
        }
    }
    if !limits.is_empty() {
        body["limits"] = Value::Object(limits);
    }
    if let Some(m) = a.sync_every {
        body["sync"] = json!({ "interval_min": m });
    }
    let mut cutover = Map::new();
    if let Some(at) = a.cutover_at {
        cutover.insert("at".into(), json!(at));
    }
    if let Some(s) = a.shutdown {
        cutover.insert(
            "shutdown".into(),
            json!(match s {
                Shutdown::Guest => "guest",
                Shutdown::PowerOff => "power-off",
            }),
        );
    }
    if !cutover.is_empty() {
        body["cutover"] = Value::Object(cutover);
    }
    Ok(body)
}

const PREFLIGHT_HEADERS: &[&str] = &["VM", "LEVEL", "PROBLEMS"];

fn preflight_row(v: &Value) -> Vec<String> {
    let problems: Vec<String> = v["problems"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            format!(
                "{} {}: {}",
                cell(&p["at"]),
                cell(&p["code"]),
                cell(&p["message"])
            )
        })
        .collect();
    vec![
        cell(&v["name"]),
        cell(&v["level"]),
        if problems.is_empty() {
            "-".into()
        } else {
            problems.join("; ")
        },
    ]
}

const MIGRATION_HEADERS: &[&str] = &["NAME", "SOURCE", "MODE", "PHASE", "VMS"];

fn migration_row(m: &Value, sources: &[Value]) -> Vec<String> {
    let source = cell(&m["spec"]["source"]);
    let source = sources
        .iter()
        .find(|s| cell(&s["id"]) == source)
        .map_or(source, |s| cell(&s["name"]));
    let counts = m["status"]["counts"]
        .as_object()
        .map(|c| {
            c.iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    vec![
        cell(&m["name"]),
        source,
        cell(&m["spec"]["mode"]),
        cell(&m["status"]["phase"]),
        if counts.is_empty() {
            "-".into()
        } else {
            counts
        },
    ]
}

const VM_HEADERS: &[&str] = &[
    "VM",
    "STATE",
    "ROUND",
    "COPIED",
    "LAST-DELTA",
    "ESTIMATE",
    "NEXT-SYNC",
];

/// 유닉스 초 → `2026-11-25T02:00:00Z`
fn when(v: &Value) -> String {
    v.as_i64()
        .and_then(|s| time::OffsetDateTime::from_unix_timestamp(s).ok())
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| "-".into())
}

fn item_row(i: &Value) -> Vec<String> {
    let st = &i["status"];
    let disks = st["disks"].as_array().cloned().unwrap_or_default();
    let sum = |k: &str| -> Value {
        if disks.is_empty() {
            Value::Null
        } else {
            json!(disks.iter().filter_map(|d| d[k].as_u64()).sum::<u64>())
        }
    };
    let mut state = cell(&st["state"]);
    if let Some(step) = st["cutover_step"].as_str() {
        state = format!("{state} ({step})");
    }
    vec![
        cell(&i["spec"]["target_name"]),
        state,
        st["rounds"].as_u64().unwrap_or(0).to_string(),
        bytes(&sum("copied_bytes")),
        bytes(&sum("last_delta_bytes")),
        st["estimate_s"]
            .as_u64()
            .map_or_else(|| "-".into(), |s| format!("{s}s")),
        when(&st["next_sync_at"]),
    ]
}

async fn migration(
    client: &Client,
    cmd: MigrationCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = |m: &str| format!("/vmware-migrations/{}", seg(m));
    match cmd {
        MigrationCmd::Create(args) => {
            let body = create_body(client, &args).await?;
            let got = client.post("/vmware-migrations", &body).await?;
            writeln!(
                out,
                "vmware-migration {} created ({} VMs); run `vmware migration preflight {}`",
                cell(&got["migration"]["name"]),
                got["vms"].as_array().map_or(0, Vec::len),
                cell(&got["migration"]["name"]),
            )?;
        }
        MigrationCmd::Preflight {
            migration,
            vms,
            refresh,
        } => {
            let got = client
                .post(
                    &format!("{}/preflight", path(&migration)),
                    &json!({ "vms": vms, "refresh": refresh }),
                )
                .await?;
            let list = got["vms"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &got)?;
            } else {
                let rows: Vec<Vec<String>> = list.iter().map(preflight_row).collect();
                table(out, PREFLIGHT_HEADERS, &rows)?;
            }
            let blocked = list.iter().filter(|v| v["level"] == "blocked").count();
            anyhow::ensure!(blocked == 0, "{blocked} VM(s) are blocked");
        }
        MigrationCmd::List => {
            let list = client.get("/vmware-migrations").await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                return Ok(print_json(out, &Value::Array(items))?);
            }
            let sources = client.get("/vmware-sources").await?;
            let sources = sources["items"].as_array().cloned().unwrap_or_default();
            let rows: Vec<Vec<String>> = items.iter().map(|m| migration_row(m, &sources)).collect();
            table(out, MIGRATION_HEADERS, &rows)?;
        }
        MigrationCmd::Get { migration } => {
            let m = client.get(&path(&migration)).await?;
            let items = client
                .get(&format!("/vmware-vms?migration={}", seg(&migration)))
                .await?;
            let items = items["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                return Ok(print_json(out, &json!({ "migration": m, "vms": items }))?);
            }
            writeln!(
                out,
                "{} ({}, {})",
                cell(&m["name"]),
                cell(&m["spec"]["mode"]),
                cell(&m["status"]["phase"])
            )?;
            let rows: Vec<Vec<String>> = items.iter().map(item_row).collect();
            table(out, VM_HEADERS, &rows)?;
            for i in &items {
                if let Some(e) = i["status"]["error"].as_str() {
                    writeln!(out, "{}: {e}", cell(&i["spec"]["target_name"]))?;
                }
            }
        }
        MigrationCmd::Start { migration } => {
            let got = client
                .post(&format!("{}/actions/start", path(&migration)), &json!({}))
                .await?;
            let failed: Vec<&Value> = got["vms"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i["status"]["state"] == "failed")
                .collect();
            writeln!(out, "vmware-migration {migration} started")?;
            for i in failed {
                writeln!(
                    out,
                    "{} skipped: {}",
                    cell(&i["spec"]["target_name"]),
                    cell(&i["status"]["error"])
                )?;
            }
        }
        MigrationCmd::Pause { migration } => {
            client
                .post(&format!("{}/actions/pause", path(&migration)), &json!({}))
                .await?;
            writeln!(out, "vmware-migration {migration} paused")?;
        }
        MigrationCmd::Resume { migration } => {
            client
                .post(&format!("{}/actions/resume", path(&migration)), &json!({}))
                .await?;
            writeln!(out, "vmware-migration {migration} resumed")?;
        }
        MigrationCmd::Cancel { migration } => {
            client
                .post(&format!("{}/actions/cancel", path(&migration)), &json!({}))
                .await?;
            writeln!(out, "vmware-migration {migration} cancelled")?;
        }
        MigrationCmd::Delete { migration } => {
            client.delete(&path(&migration)).await?;
            writeln!(out, "vmware-migration {migration} deleted")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;

    #[test]
    fn parses_pairs_intervals_and_times() {
        assert_eq!(
            parse_pair("VM Network=lan").unwrap(),
            ("VM Network".into(), "lan".into())
        );
        assert_eq!(parse_pair("a=b=c").unwrap(), ("a=b".into(), "c".into()));
        assert!(parse_pair("=x").is_err() && parse_pair("x").is_err());
        assert_eq!(parse_minutes("45m"), Ok(45));
        assert_eq!(parse_minutes("2h"), Ok(120));
        assert_eq!(parse_minutes("90"), Ok(90));
        assert!(parse_minutes("soon").is_err());
        assert_eq!(parse_time("2026-12-01T02:00:00Z"), Ok(1_796_090_400));
        assert!(parse_time("tomorrow").is_err());
    }

    #[test]
    fn rows_show_sources_vms_and_items() {
        let s = json!({ "name": "vc1", "spec": { "host": "vc", "port": 8443, "user": "u", "transport": { "vddk": {} } },
            "status": { "kind": "vcenter", "version": "8.0.2" } });
        assert_eq!(
            source_row(&s),
            ["vc1", "vc:8443", "u", "vcenter", "8.0.2", "vddk", ""]
        );
        let v = json!({ "path": "DC1/vm/web01", "name": "web01", "power": "powered-on", "guest_id": "rhel9_64Guest",
            "cpus": 2, "memory_mib": 2048, "disks": [{ "capacity": 4u64 << 30 }], "snapshots": [], "cbt": true });
        assert_eq!(
            vm_row(&v),
            [
                "DC1/vm/web01",
                "web01",
                "powered-on",
                "rhel9_64Guest",
                "2",
                "2.0GiB",
                "1 (4.0GiB)",
                "0",
                "on"
            ]
        );
        let i = json!({ "spec": { "target_name": "web01" }, "status": { "state": "cutting-over", "cutover_step": "final-sync",
            "rounds": 3, "disks": [{ "copied_bytes": 1024, "last_delta_bytes": 0 }], "estimate_s": 40, "next_sync_at": 0 } });
        assert_eq!(
            item_row(&i),
            [
                "web01",
                "cutting-over (final-sync)",
                "3",
                "1.0KiB",
                "0B",
                "40s",
                "1970-01-01T00:00:00Z"
            ]
        );
        let p = json!({ "name": "web01", "level": "blocked", "problems": [{ "at": "source", "code": "vmware_snapshots", "message": "m" }] });
        assert_eq!(
            preflight_row(&p),
            ["web01", "blocked", "source vmware_snapshots: m"]
        );
    }

    /// `migration create`는 준 것만 보내고 `--cutover-at`을 유닉스 초로 바꾼다.
    #[tokio::test]
    async fn create_sends_only_what_was_given() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/vmware-migrations") => (
                201,
                json!({ "migration": { "name": "wave1" }, "vms": [{}, {}] }),
            ),
            _ => (404, json!({ "code": "not_found", "message": path })),
        })
        .await;
        let args = CreateArgs {
            name: "wave1".into(),
            source: "vc1".into(),
            warm: true,
            cold: false,
            vms: vec!["DC1/vm/web01".into(), "vm-7".into()],
            nets: vec![("VM Network".into(), "lan".into())],
            datastores: vec![],
            pool: Some("local".into()),
            folders: None,
            folder: None,
            node: None,
            parallel: Some(2),
            per_host: None,
            per_node: None,
            sync_every: Some(30),
            cutover_at: Some(1_796_090_400),
            shutdown: Some(Shutdown::PowerOff),
            driver_iso: None,
            new_macs: false,
            no_start: false,
        };
        let mut out = Vec::new();
        migration(
            &client,
            MigrationCmd::Create(Box::new(args)),
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .starts_with("vmware-migration wave1 created (2 VMs)")
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].body,
            json!({
                "name": "wave1", "source": "vc1", "mode": "warm",
                "vms": [{ "ref": "DC1/vm/web01" }, { "ref": "vm-7" }],
                "rules": { "networks": [{ "from": "VM Network", "to": "lan" }], "default_pool": "local" },
                "limits": { "max_parallel": 2 },
                "sync": { "interval_min": 30 },
                "cutover": { "at": 1_796_090_400u64, "shutdown": "power-off" },
            })
        );
    }

    /// `vm retry`·`vm cancel`은 항목 경로로 보내고, 정리 Task가 있으면 알린다.
    #[tokio::test]
    async fn vm_retry_and_cancel_post_to_the_item() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/vmware-vms/w-web01/retry") => (
                200,
                json!({ "item": { "name": "w-web01", "status": { "state": "syncing" } } }),
            ),
            ("POST", "/api/v1/vmware-vms/w-web01/cancel") => (
                200,
                json!({ "item": { "name": "w-web01", "status": { "state": "syncing" } }, "task": "t-1" }),
            ),
            _ => (404, json!({ "code": "not_found", "message": path })),
        })
        .await;
        let mut out = Vec::new();
        vm(
            &client,
            VmCmd::Retry {
                vm: "w-web01".into(),
            },
            false,
            &mut out,
        )
        .await
        .unwrap();
        let cmd = VmCmd::Cancel {
            vm: "w-web01".into(),
            wait: false,
        };
        vm(&client, cmd, false, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("vmware-vm w-web01 is syncing again"),
            "{text}"
        );
        assert!(text.contains("being cancelled (task t-1)"), "{text}");
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// `vm cutover --wait`는 Task를 기다린 뒤 내린 시간을 알리고, `vm revert`는 확인 이름을 본문에 보낸다.
    #[tokio::test]
    async fn vm_cutover_waits_and_revert_sends_the_confirmation() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/vmware-vms/w-web01/cutover") => (
                200,
                json!({ "item": { "name": "w-web01", "status": { "state": "syncing" } }, "task": "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3f" }),
            ),
            ("GET", "/api/v1/tasks/0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3f") => (
                200,
                json!({ "id": "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3f", "spec": { "node": "n1" }, "status": { "state": "succeeded" } }),
            ),
            ("GET", "/api/v1/vmware-vms/w-web01") => (
                200,
                json!({ "name": "w-web01", "status": { "state": "done", "downtime_s": 42 } }),
            ),
            ("POST", "/api/v1/vmware-vms/w-web01/revert") => (
                200,
                json!({ "item": { "name": "w-web01", "status": { "state": "done" } }, "task": "t-2" }),
            ),
            _ => (404, json!({ "code": "not_found", "message": path })),
        })
        .await;
        let mut out = Vec::new();
        let cmd = VmCmd::Cutover {
            vm: "w-web01".into(),
            wait: true,
        };
        vm(&client, cmd, false, &mut out).await.unwrap();
        let cmd = VmCmd::Revert {
            vm: "w-web01".into(),
            confirm: "web01".into(),
            wait: false,
        };
        vm(&client, cmd, false, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("vmware-vm w-web01 is done (downtime 42s)"),
            "{text}"
        );
        assert!(text.contains("is being reverted (task t-2)"), "{text}");
        let seen = seen.lock().unwrap();
        let revert = seen.iter().find(|r| r.line.contains("/revert ")).unwrap();
        assert_eq!(revert.body, json!({ "confirm": "web01" }));
    }

    /// 지문 없이 더하면 서버 지문을 알려 주고 비밀번호를 묻지 않는다. 막힌 VM이 있는 미리 확인은 실패로 끝난다.
    #[tokio::test]
    async fn add_reports_the_fingerprint_and_preflight_fails_when_blocked() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/vmware-sources") => (
                409,
                json!({ "code": "untrusted_certificate", "message": "verify",
                    "details": { "fingerprint": "AB:CD", "thumbprint_sha1": "EF" } }),
            ),
            ("POST", "/api/v1/vmware-migrations/w/preflight") => (
                200,
                json!({ "fetched_at": 1, "vms": [
                    { "name": "web01", "level": "warn", "problems": [] },
                    { "name": "db01", "level": "blocked", "problems": [] },
                ] }),
            ),
            _ => (404, json!({ "code": "not_found", "message": path })),
        })
        .await;
        let mut out = Vec::new();
        let cmd = SourceCmd::Add {
            name: "vc1".into(),
            host: "vc".into(),
            port: None,
            user: "u".into(),
            password: PasswordArgs::default(),
            fingerprint: None,
            vddk: false,
            vddk_libdir: None,
            datacenter: None,
        };
        let err = source(&client, cmd, false, &mut out).await.unwrap_err();
        assert!(err.to_string().contains("--fingerprint AB:CD"), "{err}");
        let cmd = MigrationCmd::Preflight {
            migration: "w".into(),
            vms: vec![],
            refresh: false,
        };
        let err = migration(&client, cmd, false, &mut out).await.unwrap_err();
        assert_eq!(err.to_string(), "1 VM(s) are blocked");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("db01   blocked"), "{text}");
        assert_eq!(seen.lock().unwrap()[0].body["password"], "-");
    }
}
