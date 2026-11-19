//! 템플릿·연결 복제·이미지 라이브러리(계획 58): `orbivisorctl template …`, `image …`, `vm clone --linked|--full`,
//! `volume flatten`, `vm get`·`volume list`의 템플릿·기반 표시.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use opl_types::cloud_init::IpConfig;
use reqwest::Method;
use serde_json::{Value, json};

use crate::cli::{items, name_of, parse_size};
use crate::client::{Client, IMPORT_WAIT, seg};
use crate::media::{CloudInitArgs, CloudInitClear, cloud_init_update, watch_download};
use crate::output::{bytes, cell, json as print_json, table};

/// 배포한 VM이 켜질 때까지 기다리는 상한(`--start`)
const START_WAIT: Duration = Duration::from_secs(5 * 60);
/// 한 번에 배포할 수 있는 VM 수(`--count`)
const MAX_COUNT: u32 = 100;

#[derive(Subcommand)]
pub(crate) enum TemplateCmd {
    /// 템플릿(켤 수 없는 원본 VM)과 디스크·풀·연결 복제 수
    List,
    /// 꺼진 VM을 템플릿으로 바꾼다. 스냅샷·HA·복제·스냅샷 예약이 없어야 한다.
    Convert { vm: String },
    /// 템플릿을 보통 VM으로 되돌린다. 연결 복제가 남아 있으면 되돌리지 못한다(먼저 지우거나 평탄화).
    Unconvert {
        template: String,
        /// 되돌리기 Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
    /// 템플릿을 지운다: 보통 VM으로 되돌린 뒤 VM을 지운다. 디스크 볼륨은 남는다.
    Delete { template: String },
    /// 템플릿에서 새 VM을 배포한다(기본: 되는 디스크는 연결 복제, 안 되는 디스크는 전체 복사).
    Deploy(Box<TemplateDeploy>),
}

#[derive(Args)]
pub(crate) struct TemplateDeploy {
    template: String,
    /// 새 VM 이름. `--count`가 2 이상이면 `{n}`이 번호로 바뀐다(예: `web-{n}`).
    #[arg(long)]
    name: String,
    /// 배포할 VM 수(같은 설정으로 차례로 배포한다)
    #[arg(long, default_value_t = 1)]
    count: u32,
    /// `{n}`의 첫 번호
    #[arg(long, default_value_t = 1)]
    first: u32,
    /// 모든 디스크를 연결 복제로(안 되면 실패)
    #[arg(long, conflicts_with = "full")]
    linked: bool,
    /// 모든 디스크를 전체 복사로
    #[arg(long)]
    full: bool,
    /// 새 VM을 켤 노드(이름이나 id). 디스크가 닿아야 한다.
    #[arg(long)]
    node: Option<String>,
    /// 새 디스크를 둘 풀(전체 복사만 — 연결 복제는 템플릿 디스크와 같은 풀). 생략하면 템플릿 디스크의 풀이다.
    #[arg(long)]
    pool: Option<String>,
    /// vCPU 수(소켓 하나)
    #[arg(long)]
    cpus: Option<u16>,
    /// 메모리(MiB)
    #[arg(long)]
    memory: Option<u32>,
    #[command(flatten)]
    cpu: crate::cpu::CpuArgs,
    #[command(flatten)]
    numa: crate::numa::NumaArgs,
    /// NIC의 네트워크: `nicN=NETWORK`(`nicN=`이 없으면 준 순서대로 NIC 0, 1, …). NIC 수는 템플릿과 같다.
    #[arg(long = "network", value_name = "[nicN=]NETWORK", value_parser = parse_nic_network)]
    networks: Vec<(Option<usize>, String)>,
    /// 디스크의 새 크기(늘리기만): `diskN=SIZE`(`diskN=`이 없으면 준 순서대로). 단위 K, M, G, T.
    #[arg(long = "disk-size", value_name = "[diskN=]SIZE", value_parser = parse_disk_size)]
    disk_sizes: Vec<(Option<usize>, u64)>,
    #[command(flatten)]
    cloud_init: CloudInitArgs,
    /// 만든 뒤 켜고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// 배포 Task를 기다리지 않는다.
    #[arg(long)]
    no_wait: bool,
}

/// `nic1=lan`, `lan`
fn parse_nic_network(s: &str) -> Result<(Option<usize>, String), String> {
    let (index, network) = parse_indexed(s, "nic")?;
    if network.is_empty() {
        return Err(format!("invalid network {s:?}: use [nicN=]NETWORK"));
    }
    Ok((index, network.to_string()))
}

/// `disk0=20G`, `20G`
fn parse_disk_size(s: &str) -> Result<(Option<usize>, u64), String> {
    let (index, size) = parse_indexed(s, "disk")?;
    Ok((index, parse_size(size)?))
}

/// `{prefix}N=값`이면 (N, 값), 아니면 (None, 전체)
fn parse_indexed<'a>(s: &'a str, prefix: &str) -> Result<(Option<usize>, &'a str), String> {
    match s.split_once('=') {
        Some((key, value)) => {
            let n = key
                .strip_prefix(prefix)
                .and_then(|n| n.parse::<usize>().ok())
                .ok_or_else(|| format!("invalid {s:?}: use [{prefix}N=]VALUE"))?;
            Ok((Some(n), value))
        }
        None => Ok((None, s)),
    }
}

/// 순번별 값 목록: `nicN=`이 없는 값은 준 순서의 자리다. 빈 자리는 null(그대로)이다.
fn by_index<T: Clone + Into<Value>>(list: &[(Option<usize>, T)]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for (k, (index, value)) in list.iter().enumerate() {
        let i = index.unwrap_or(k);
        if out.len() <= i {
            out.resize(i + 1, Value::Null);
        }
        out[i] = value.clone().into();
    }
    out
}

/// 배포할 VM 이름들. `count`가 2 이상이면 `pattern`에 `{n}`이 있어야 하고, `{n}`은 `first`부터의 번호다.
pub(crate) fn deploy_names(pattern: &str, count: u32, first: u32) -> anyhow::Result<Vec<String>> {
    if count == 0 || count > MAX_COUNT {
        bail!("--count must be 1-{MAX_COUNT}");
    }
    if count > 1 && !pattern.contains("{n}") {
        bail!("--name must contain {{n}} when --count is more than 1 (e.g. 'web-{{n}}')");
    }
    let last = first
        .checked_add(count - 1)
        .context("--first is too large")?;
    Ok((first..=last)
        .map(|n| pattern.replace("{n}", &n.to_string()))
        .collect())
}

impl TemplateDeploy {
    /// 여러 VM에 같은 값을 줄 수 없는 cloud-init 값(호스트 이름·정적 IP)을 거절한다.
    fn check_identity(&self) -> anyhow::Result<()> {
        // CPU 배치 칸의 조합(`--numa none --numa-memory`)도 여기서 본다(본문 만들기는 실패하지 않는다).
        self.numa.request_fields(&mut json!({}), true)?;
        if self.count < 2 {
            return Ok(());
        }
        if self.cloud_init.ci_hostname.is_some() {
            bail!(
                "--ci-hostname cannot be used with --count {}: every VM would get the same host name (without it the \
                 VM name is the host name)",
                self.count
            );
        }
        let fixed = self
            .cloud_init
            .ci_ip
            .iter()
            .chain(&self.cloud_init.ci_ip6)
            .any(|(_, c)| matches!(c, IpConfig::Static { .. }));
        if fixed {
            bail!(
                "a static --ci-ip/--ci-ip6 cannot be used with --count {}: every VM would get the same address",
                self.count
            );
        }
        Ok(())
    }

    /// `POST /vms/{template}/clone`의 본문(이름 `name`). 주지 않은 값은 넣지 않아 템플릿 값 그대로다.
    fn body(&self, name: &str, cloud_init: &Value) -> Value {
        let mut body = json!({ "name": name });
        if self.linked {
            body["linked"] = json!(true);
        } else if self.full {
            body["linked"] = json!(false);
        }
        let mut set = |key: &str, value: Option<Value>| {
            if let Some(v) = value {
                body[key] = v;
            }
        };
        set("node", self.node.as_ref().map(|n| json!(n)));
        set("pool", self.pool.as_ref().map(|p| json!(p)));
        set("vcpus", self.cpus.map(|n| json!(n)));
        set("memory_mib", self.memory.map(|n| json!(n)));
        self.cpu.request_fields(&mut body);
        // 조합은 `check_identity`가 이미 봤다.
        let _ = self.numa.request_fields(&mut body, true);
        if !self.networks.is_empty() {
            body["nics"] = Value::Array(by_index(&self.networks));
        }
        if !self.disk_sizes.is_empty() {
            body["disks"] = Value::Array(by_index(&self.disk_sizes));
        }
        if !cloud_init.is_null() {
            body["cloud_init"] = cloud_init.clone();
        }
        if self.start {
            body["start"] = json!(true);
        }
        body
    }
}

/// `template list`의 열
const TEMPLATE_HEADERS: &[&str] = &["NAME", "DISKS", "POOL", "CLONES"];

/// 템플릿 디스크 볼륨들(`vm`의 디스크 이름으로 찾은 볼륨 목록 항목)
fn disk_volumes<'a>(vm: &Value, volumes: &'a [Value]) -> Vec<&'a Value> {
    vm["spec"]["disks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| volumes.iter().find(|v| v["name"] == d["volume"]))
        .collect()
}

/// 볼륨들의 연결 복제 수(`spec.base`가 그 id인 볼륨)
fn clone_count(bases: &[&Value], volumes: &[Value]) -> usize {
    volumes
        .iter()
        .filter(|v| bases.iter().any(|b| v["spec"]["base"] == b["id"]))
        .count()
}

/// `template list`의 행: 템플릿 VM마다 디스크 볼륨 이름들, 풀(겹치지 않게), 연결 복제 수
pub(crate) fn template_rows(vms: &[Value], volumes: &[Value]) -> Vec<Vec<String>> {
    vms.iter()
        .filter(|vm| vm["spec"]["template"] == true)
        .map(|vm| {
            let disks: Vec<String> = vm["spec"]["disks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| cell(&d["volume"]))
                .collect();
            let bases = disk_volumes(vm, volumes);
            let mut pools: Vec<String> = bases.iter().map(|v| cell(&v["spec"]["pool"])).collect();
            pools.dedup();
            vec![
                cell(&vm["name"]),
                if disks.is_empty() {
                    "-".into()
                } else {
                    disks.join(", ")
                },
                if pools.is_empty() {
                    "-".into()
                } else {
                    pools.join(", ")
                },
                clone_count(&bases, volumes).to_string(),
            ]
        })
        .collect()
}

/// `vm get`의 템플릿·연결 복제 줄: 템플릿이면 `template: 2 linked clone(s)`, 연결 복제 디스크마다
/// `  disk0: linked clone of tmpl-root`. 볼륨 목록을 읽지 못했으면(권한) 템플릿 줄만이다.
pub(crate) fn template_lines(vm: &Value, volumes: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    let disks = disk_volumes(vm, volumes);
    if vm["spec"]["template"] == true {
        lines.push(format!(
            "template: {} linked clone(s); deploy with `orbivisorctl template deploy {}`",
            clone_count(&disks, volumes),
            cell(&vm["name"])
        ));
    }
    for (i, d) in vm["spec"]["disks"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(volume) = volumes.iter().find(|v| v["name"] == d["volume"]) else {
            continue;
        };
        let base = &volume["spec"]["base"];
        if !base.is_null() {
            lines.push(format!(
                "disk{i}: linked clone of {}",
                name_of(volumes, base)
            ));
        }
    }
    lines
}

/// `volume list`의 열
pub(crate) const VOLUME_HEADERS: &[&str] = &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED", "BASE"];

/// `volume list`의 행. BASE는 연결 복제의 기반 볼륨 이름이다.
pub(crate) fn volume_row(v: &Value, volumes: &[Value]) -> Vec<String> {
    let base = &v["spec"]["base"];
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["pool"]),
        bytes(&v["spec"]["size_bytes"]),
        cell(&v["spec"]["format"]),
        bytes(&v["status"]["allocated_bytes"]),
        if base.is_null() {
            "-".into()
        } else {
            name_of(volumes, base)
        },
    ]
}

pub(crate) async fn template(
    client: &Client,
    cmd: TemplateCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        TemplateCmd::List => {
            let vms = items(&client.get("/vms").await?);
            if json_out {
                let list: Vec<Value> = vms
                    .into_iter()
                    .filter(|v| v["spec"]["template"] == true)
                    .collect();
                print_json(out, &Value::Array(list))?;
                return Ok(());
            }
            let volumes = items(&client.get("/volumes").await?);
            table(out, TEMPLATE_HEADERS, &template_rows(&vms, &volumes))?;
        }
        TemplateCmd::Convert { vm } => {
            client
                .send(client.request(Method::POST, &format!("/vms/{}/template", seg(&vm))))
                .await?;
            writeln!(out, "vm {vm} is now a template")?;
        }
        TemplateCmd::Unconvert { template, no_wait } => {
            let task = client
                .delete(&format!("/vms/{}/template", seg(&template)))
                .await?;
            if no_wait {
                writeln!(
                    out,
                    "template {template}: convert task {} started",
                    cell(&task["id"])
                )?;
            } else {
                client.wait_task(&task).await?;
                writeln!(out, "template {template} converted back to a VM")?;
            }
        }
        TemplateCmd::Delete { template } => {
            let vm = client.get(&format!("/vms/{}", seg(&template))).await?;
            if vm["spec"]["template"] != true {
                bail!("vm {template} is not a template; use `orbivisorctl vm delete`");
            }
            let task = client
                .delete(&format!("/vms/{}/template", seg(&template)))
                .await?;
            client.wait_task(&task).await?;
            client.delete(&format!("/vms/{}", seg(&template))).await?;
            writeln!(
                out,
                "template {template} deleted (its disk volumes are kept)"
            )?;
        }
        TemplateCmd::Deploy(args) => deploy(client, &args, out, err).await?,
    }
    Ok(())
}

/// 배포 Task 하나를 기다린 결과
enum Deployed {
    /// 성공(경고가 있으면 그 문구)
    Done(Option<String>),
    Failed(anyhow::Error),
}

/// 템플릿에서 VM을 배포한다. `--count`면 단건 API를 차례로 불러 Task를 모두 만든 뒤 함께 기다린다. 하나라도
/// 실패하면 나머지를 마저 기다린 뒤 실패한 VM들을 알린다.
async fn deploy(
    client: &Client,
    args: &TemplateDeploy,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let names = deploy_names(&args.name, args.count, args.first)?;
    args.check_identity()?;
    let cloud_init = if args.cloud_init.wanted() {
        let input = args.cloud_init.read()?;
        cloud_init_update(
            &Value::Null,
            &args.cloud_init,
            &input,
            CloudInitClear::default(),
        )
    } else {
        Value::Null
    };
    let path = format!("/vms/{}/clone", seg(&args.template));
    let mut tasks = Vec::new();
    let mut refused = None;
    for name in &names {
        match client.post(&path, &args.body(name, &cloud_init)).await {
            Ok(task) => tasks.push((name.clone(), task)),
            Err(e) => {
                // 같은 요청이라 다음 VM도 거의 늘 같은 이유로 거절된다 — 더 만들지 않는다.
                refused = Some((name.clone(), e));
                break;
            }
        }
    }
    if args.no_wait {
        for (name, task) in &tasks {
            writeln!(out, "vm {name}: deploy task {} started", cell(&task["id"]))?;
        }
        if let Some((name, e)) = refused {
            return Err(e.context(format!("cannot deploy vm {name}")));
        }
        return Ok(());
    }
    let how = if args.linked {
        " (linked)"
    } else if args.full {
        " (full copy)"
    } else {
        ""
    };
    let mut failed = Vec::new();
    for (name, task) in &tasks {
        let result = match client.wait_task_for(task, IMPORT_WAIT).await {
            Ok(done) => Deployed::Done(done["status"]["error"].as_str().map(str::to_string)),
            Err(e) => Deployed::Failed(e),
        };
        match result {
            Deployed::Done(warning) => {
                writeln!(out, "vm {name} deployed from {}{how}", args.template)?;
                if let Some(w) = warning {
                    writeln!(err, "warning: vm {name}: {w}")?;
                }
                if args.start {
                    match wait_running(client, name).await {
                        Ok(Some(node)) => writeln!(out, "vm {name}: running on {node}")?,
                        Ok(None) => writeln!(err, "warning: vm {name} was created stopped")?,
                        Err(e) => {
                            writeln!(err, "vm {name}: {e:#}")?;
                            failed.push(name.clone());
                        }
                    }
                }
            }
            Deployed::Failed(e) => {
                writeln!(err, "vm {name}: {e:#}")?;
                failed.push(name.clone());
            }
        }
    }
    if let Some((name, e)) = refused {
        let rest = names.len() - tasks.len() - 1;
        let skipped = if rest > 0 {
            format!(" ({rest} more not deployed)")
        } else {
            String::new()
        };
        return Err(e.context(format!("cannot deploy vm {name}{skipped}")));
    }
    if !failed.is_empty() {
        bail!(
            "{} of {} deployments failed: {}",
            failed.len(),
            names.len(),
            failed.join(", ")
        );
    }
    Ok(())
}

/// 배포한 VM이 켜질 때까지 기다려 노드 이름을 돌려준다. 원하는 전원이 `running`이 아니면(놓을 노드가 없어 꺼진 채
/// 만들었으면) None이다.
async fn wait_running(client: &Client, name: &str) -> anyhow::Result<Option<String>> {
    let deadline = Instant::now() + START_WAIT;
    loop {
        let vm = client.get(&format!("/vms/{}", seg(name))).await?;
        if vm["spec"]["power"] != "running" {
            return Ok(None);
        }
        if vm["status"]["phase"] == "running" {
            let nodes = items(&client.get("/nodes").await?);
            return Ok(Some(name_of(&nodes, &vm["status"]["node"])));
        }
        if Instant::now() > deadline {
            bail!(
                "not running after {} minutes (phase {}, last error: {})",
                START_WAIT.as_secs() / 60,
                cell(&vm["status"]["phase"]),
                cell(&vm["status"]["last_error"])
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// `vm clone`의 `--linked`·`--full`이 정한 `linked` 값(둘 다 없으면 None — 템플릿이면 되는 디스크만 연결)
pub(crate) fn linked_value(linked: bool, full: bool) -> Option<bool> {
    match (linked, full) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

/// `volume flatten`: 연결 복제를 기반과 떼어 독립 볼륨으로 만든다(쓰는 VM은 꺼져 있어야 함).
pub(crate) async fn volume_flatten(
    client: &Client,
    volume: &str,
    no_wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/volumes/{}/flatten", seg(volume)), &json!({}))
        .await?;
    if no_wait {
        writeln!(
            out,
            "volume {volume}: flatten task {} started",
            cell(&task["id"])
        )?;
        return Ok(());
    }
    // 평탄화는 기반 데이터를 디스크 크기만큼 복사한다(Task에 시간 제한이 없음).
    client.wait_task_for(&task, IMPORT_WAIT).await?;
    writeln!(out, "volume {volume} flattened")?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ArchArg {
    #[value(name = "x86_64")]
    X86_64,
    Aarch64,
}

impl ArchArg {
    fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ImageFormat {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ImageFirmware {
    Uefi,
    Bios,
}

#[derive(Subcommand)]
pub(crate) enum ImageCmd {
    /// 이미지 카탈로그: 내장 항목과 관리자 항목, 이미 받은 볼륨
    List {
        /// 이 아키텍처의 항목만
        #[arg(long, value_enum)]
        arch: Option<ArchArg>,
    },
    /// 카탈로그 이미지를 풀에 받아 체크섬을 확인한다. `--template`을 주면 받은 뒤 템플릿 VM을 만든다.
    #[command(visible_alias = "download")]
    Get(Box<ImageGet>),
    /// 관리자 카탈로그 항목 더하기·지우기(admin)
    Catalog {
        #[command(subcommand)]
        cmd: CatalogCmd,
    },
}

#[derive(Args)]
pub(crate) struct ImageGet {
    /// 카탈로그 항목 id(예: `ubuntu-24.04`)
    id: String,
    #[arg(long)]
    pool: String,
    /// 볼륨 이름. 없으면 id의 `.`을 `-`로(aarch64면 `-arm64`를 붙임)
    #[arg(long)]
    name: Option<String>,
    /// 받을 아키텍처. 없으면 풀에서 받는 노드의 아키텍처
    #[arg(long, value_enum)]
    arch: Option<ArchArg>,
    /// 받은 뒤 이 이름의 템플릿 VM을 만든다(디스크 하나, NIC 하나, cloud-init).
    #[arg(long, value_name = "NAME")]
    template: Option<String>,
    /// 템플릿 NIC의 네트워크(`--template`에 필요)
    #[arg(long, requires = "template")]
    network: Option<String>,
    /// 템플릿의 vCPU 수(기본 2)
    #[arg(long, requires = "template")]
    cpus: Option<u16>,
    /// 템플릿의 메모리(MiB, 기본 2048)
    #[arg(long, requires = "template")]
    memory: Option<u32>,
    #[command(flatten)]
    cpu: crate::cpu::CpuArgs,
    #[command(flatten)]
    numa: crate::numa::NumaArgs,
    /// 받은 디스크를 이 크기로 늘린다(이미지보다 클 때). 단위 K, M, G, T.
    #[arg(long, value_parser = parse_size, requires = "template")]
    size: Option<u64>,
    #[command(flatten)]
    cloud_init: CloudInitArgs,
    /// 받기 Task를 기다리지 않는다.
    #[arg(long)]
    no_wait: bool,
}

#[derive(Subcommand)]
pub(crate) enum CatalogCmd {
    /// 관리자 항목을 더한다. 체크섬(`--sha256`·`--sha512`·`--checksum-url`) 하나가 필요하다.
    Add(Box<CatalogAdd>),
    /// 관리자 항목을 지운다(받은 볼륨과 템플릿은 그대로). `--arch`가 없으면 그 id의 모든 아키텍처다.
    Remove {
        id: String,
        #[arg(long, value_enum)]
        arch: Option<ArchArg>,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("checksum").required(true).args(["sha256", "sha512", "checksum_url"])))]
pub(crate) struct CatalogAdd {
    /// 항목 id(소문자·숫자·`.`·`_`·`-`). 내장 항목의 id는 쓸 수 없다.
    id: String,
    /// 이미지 URL(http·https)
    #[arg(long)]
    url: String,
    /// 이미지의 SHA-256(16진 64자)
    #[arg(long, value_name = "HEX")]
    sha256: Option<String>,
    /// 이미지의 SHA-512(16진 128자)
    #[arg(long, value_name = "HEX")]
    sha512: Option<String>,
    /// 이미지와 같은 곳의 체크섬 파일(`SHA256SUMS` 등). 받을 때 이미지 파일 이름의 줄을 찾는다.
    #[arg(long, value_name = "URL")]
    checksum_url: Option<String>,
    /// 보이는 이름(기본: id)
    #[arg(long)]
    title: Option<String>,
    /// 배포판(예: ubuntu)
    #[arg(long)]
    os: String,
    /// 판(예: 24.04)
    #[arg(long)]
    version: String,
    #[arg(long, value_enum, default_value = "x86_64")]
    arch: ArchArg,
    #[arg(long, value_enum, default_value = "qcow2")]
    source_format: ImageFormat,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: ImageFirmware,
    /// cloud-init 기본 사용자 이름(예: ubuntu)
    #[arg(long)]
    default_user: Option<String>,
    /// 이미지의 디스크 크기(가상 크기). 단위 K, M, G, T.
    #[arg(long, value_parser = parse_size)]
    min_size: Option<u64>,
}

impl CatalogAdd {
    /// `POST /image-catalog`의 본문(`CatalogImage`)
    fn body(&self) -> Value {
        let checksum = match (&self.sha256, &self.sha512, &self.checksum_url) {
            (Some(v), _, _) => json!({ "kind": "sha256", "value": v }),
            (_, Some(v), _) => json!({ "kind": "sha512", "value": v }),
            (_, _, Some(url)) => json!({ "kind": "sums", "url": url }),
            _ => unreachable!("clap requires one checksum"),
        };
        let mut body = json!({
            "id": self.id,
            "title": self.title.clone().unwrap_or_else(|| self.id.clone()),
            "os": self.os,
            "version": self.version,
            "arch": self.arch.name(),
            "url": self.url,
            "checksum": checksum,
            "source_format": match self.source_format { ImageFormat::Qcow2 => "qcow2", ImageFormat::Raw => "raw" },
            "firmware": match self.firmware { ImageFirmware::Uefi => "uefi", ImageFirmware::Bios => "bios" },
            "min_size_bytes": self.min_size.unwrap_or(0),
        });
        if let Some(user) = &self.default_user {
            body["default_user"] = json!(user);
        }
        body
    }
}

impl ImageGet {
    /// `POST /image-catalog/{id}/download`의 본문. `cloud_init`은 만든 cloud-init 값(없으면 Null)이다.
    fn body(&self, cloud_init: &Value) -> anyhow::Result<Value> {
        let mut body = json!({ "pool": self.pool });
        if let Some(name) = &self.name {
            body["name"] = json!(name);
        }
        if let Some(arch) = self.arch {
            body["arch"] = json!(arch.name());
        }
        if let Some(name) = &self.template {
            let network = self
                .network
                .as_ref()
                .context("--template needs --network (the network of the template's NIC)")?;
            let mut t = json!({ "name": name, "network": network });
            if let Some(n) = self.cpus {
                t["vcpus"] = json!(n);
            }
            if let Some(n) = self.memory {
                t["memory_mib"] = json!(n);
            }
            if let Some(n) = self.size {
                t["size_bytes"] = json!(n);
            }
            self.cpu.request_fields(&mut t);
            self.numa.request_fields(&mut t, false)?;
            if !cloud_init.is_null() {
                t["cloud_init"] = cloud_init.clone();
            }
            body["template"] = t;
        } else if self.cloud_init.wanted() {
            bail!("--ci-* options need --template");
        } else if self.cpu.given() {
            bail!("--cpu-model and --cpu-flags need --template");
        } else if self.numa.given() {
            bail!("--cpu-policy, --numa and --hugepages need --template");
        }
        Ok(body)
    }
}

/// `image list`의 열
const IMAGE_HEADERS: &[&str] = &[
    "ID", "ARCH", "OS", "VERSION", "FIRMWARE", "SOURCE", "VOLUMES",
];

/// `image list`의 행
pub(crate) fn image_row(i: &Value) -> Vec<String> {
    let volumes: Vec<String> = i["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(cell)
        .collect();
    vec![
        cell(&i["id"]),
        cell(&i["arch"]),
        cell(&i["os"]),
        cell(&i["version"]),
        cell(&i["firmware"]),
        cell(&i["source"]),
        if volumes.is_empty() {
            "-".into()
        } else {
            volumes.join(", ")
        },
    ]
}

pub(crate) async fn image(
    client: &Client,
    cmd: ImageCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ImageCmd::List { arch } => {
            let path = match arch {
                Some(a) => format!("/image-catalog?arch={}", a.name()),
                None => "/image-catalog".to_string(),
            };
            let list = client.get(&path).await?;
            let rows = items(&list);
            if json_out {
                print_json(out, &Value::Array(rows))?;
            } else {
                let rows: Vec<_> = rows.iter().map(image_row).collect();
                table(out, IMAGE_HEADERS, &rows)?;
            }
        }
        ImageCmd::Get(args) => {
            let cloud_init = if args.cloud_init.wanted() {
                let input = args.cloud_init.read()?;
                cloud_init_update(
                    &Value::Null,
                    &args.cloud_init,
                    &input,
                    CloudInitClear::default(),
                )
            } else {
                Value::Null
            };
            let body = args.body(&cloud_init)?;
            let path = format!("/image-catalog/{}/download", seg(&args.id));
            let task = client.post(&path, &body).await?;
            let what = format!("image {}", args.id);
            let done = watch_download(client, &task, &what, args.no_wait, out, err).await?;
            if let Some(task) = done {
                let volume = client
                    .get(&format!("/volumes/{}", seg(&cell(&task["spec"]["target"]))))
                    .await?;
                writeln!(out, "volume {} ready", cell(&volume["name"]))?;
                if let Some(t) = &args.template {
                    writeln!(out, "template {t} created")?;
                }
            }
        }
        ImageCmd::Catalog { cmd } => match cmd {
            CatalogCmd::Add(args) => {
                client.post("/image-catalog", &args.body()).await?;
                writeln!(out, "image {} added to the catalog", args.id)?;
            }
            CatalogCmd::Remove { id, arch } => {
                let mut path = format!("/image-catalog/{}", seg(&id));
                if let Some(a) = arch {
                    path.push_str(&format!("?arch={}", a.name()));
                }
                client.delete(&path).await?;
                writeln!(out, "image {id} removed from the catalog")?;
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: TemplateCmd,
    }

    #[derive(Parser)]
    struct ImageCli {
        #[command(subcommand)]
        cmd: ImageCmd,
    }

    fn deploy_args(args: &[&str]) -> TemplateDeploy {
        let mut argv = vec!["t", "deploy"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).unwrap().cmd {
            TemplateCmd::Deploy(d) => *d,
            _ => unreachable!(),
        }
    }

    #[test]
    fn names_from_a_pattern() {
        assert_eq!(deploy_names("web", 1, 1).unwrap(), ["web"]);
        assert_eq!(
            deploy_names("web-{n}", 3, 1).unwrap(),
            ["web-1", "web-2", "web-3"]
        );
        assert_eq!(
            deploy_names("ci-{n}-x", 2, 9).unwrap(),
            ["ci-9-x", "ci-10-x"]
        );
        // 하나면 `{n}`은 첫 번호
        assert_eq!(deploy_names("web-{n}", 1, 5).unwrap(), ["web-5"]);
        assert!(
            deploy_names("web", 2, 1)
                .unwrap_err()
                .to_string()
                .contains("{n}")
        );
        assert!(deploy_names("web-{n}", 0, 1).is_err());
        assert!(deploy_names("web-{n}", MAX_COUNT + 1, 1).is_err());
        assert!(deploy_names("web-{n}", 2, u32::MAX).is_err());
    }

    #[test]
    fn parses_indexed_values() {
        assert_eq!(
            parse_nic_network("nic1=lan"),
            Ok((Some(1), "lan".to_string()))
        );
        assert_eq!(parse_nic_network("lan"), Ok((None, "lan".to_string())));
        assert!(parse_nic_network("nic=lan").is_err());
        assert!(parse_nic_network("eth0=lan").is_err());
        assert!(parse_nic_network("nic0=").is_err());
        assert_eq!(parse_disk_size("disk0=20G"), Ok((Some(0), 20 << 30)));
        assert_eq!(parse_disk_size("512M"), Ok((None, 512 << 20)));
        assert!(parse_disk_size("disk0=big").is_err());
        assert!(parse_disk_size("d0=1G").is_err());
    }

    #[test]
    fn deploy_body_has_only_given_values() {
        let args = deploy_args(&["tmpl", "--name", "web"]);
        assert_eq!(args.body("web", &Value::Null), json!({ "name": "web" }));

        let args = deploy_args(&[
            "tmpl",
            "--name",
            "web-{n}",
            "--count",
            "2",
            "--linked",
            "--node",
            "node2",
            "--cpus",
            "4",
            "--memory",
            "4096",
            "--network",
            "nic1=lan",
            "--disk-size",
            "disk0=20G",
            "--disk-size",
            "disk2=1G",
            "--start",
        ]);
        let ci = json!({ "user": "orbi" });
        assert_eq!(
            args.body("web-1", &ci),
            json!({
                "name": "web-1",
                "linked": true,
                "node": "node2",
                "vcpus": 4,
                "memory_mib": 4096,
                "nics": [null, "lan"],
                "disks": [20u64 << 30, null, 1u64 << 30],
                "cloud_init": { "user": "orbi" },
                "start": true,
            })
        );
        let args = deploy_args(&[
            "tmpl",
            "--name",
            "w",
            "--full",
            "--pool",
            "fast",
            "--network",
            "a",
            "--network",
            "b",
        ]);
        assert_eq!(
            args.body("w", &Value::Null),
            json!({ "name": "w", "linked": false, "pool": "fast", "nics": ["a", "b"] })
        );
        // --linked와 --full은 함께 쓰지 못한다.
        let argv = ["t", "deploy", "tmpl", "--name", "w", "--linked", "--full"];
        assert!(Cli::try_parse_from(argv).is_err());
    }

    #[test]
    fn bulk_deploy_refuses_fixed_identity() {
        let ok = deploy_args(&["tmpl", "--name", "w-{n}", "--count", "2", "--ci-ip", "dhcp"]);
        assert!(ok.check_identity().is_ok());
        let single = deploy_args(&[
            "tmpl",
            "--name",
            "w",
            "--ci-hostname",
            "h",
            "--ci-ip",
            "10.0.0.5/24",
        ]);
        assert!(single.check_identity().is_ok());
        let host = deploy_args(&[
            "tmpl",
            "--name",
            "w-{n}",
            "--count",
            "2",
            "--ci-hostname",
            "h",
        ]);
        assert!(
            host.check_identity()
                .unwrap_err()
                .to_string()
                .contains("--ci-hostname")
        );
        let ip = deploy_args(&[
            "tmpl",
            "--name",
            "w-{n}",
            "--count",
            "3",
            "--ci-ip6",
            "nic0=2001:db8::5/64",
        ]);
        assert!(
            ip.check_identity()
                .unwrap_err()
                .to_string()
                .contains("static")
        );
    }

    fn vm(name: &str, template: bool, disks: &[&str]) -> Value {
        let disks: Vec<Value> = disks.iter().map(|d| json!({ "volume": d })).collect();
        json!({ "name": name, "spec": { "template": template, "disks": disks } })
    }

    fn volume(id: &str, name: &str, pool: &str, base: Option<&str>) -> Value {
        json!({
            "id": id, "name": name,
            "spec": { "pool": pool, "size_bytes": 1u64 << 30, "format": "qcow2", "base": base },
            "status": { "allocated_bytes": 1u64 << 20 },
        })
    }

    #[test]
    fn template_list_rows() {
        let vms = [
            vm("tmpl", true, &["t0", "t1"]),
            vm("web-1", false, &["w0"]),
            vm("empty", true, &[]),
        ];
        let volumes = [
            volume("a", "t0", "local", None),
            volume("b", "t1", "local", None),
            volume("c", "w0", "local", Some("a")),
            volume("d", "w1", "local", Some("b")),
            volume("e", "orphan", "local", Some("a")),
        ];
        assert_eq!(
            template_rows(&vms, &volumes),
            [
                vec!["tmpl", "t0, t1", "local", "3"],
                vec!["empty", "-", "-", "0"],
            ]
        );
        assert_eq!(
            template_lines(&vms[0], &volumes),
            ["template: 3 linked clone(s); deploy with `orbivisorctl template deploy tmpl`"]
        );
        assert_eq!(
            template_lines(&vms[1], &volumes),
            ["disk0: linked clone of t0"]
        );
        // 볼륨을 읽지 못했으면 템플릿 줄만
        assert_eq!(template_lines(&vms[1], &[]), Vec::<String>::new());
        assert_eq!(
            volume_row(&volumes[2], &volumes),
            ["w0", "local", "1.0GiB", "qcow2", "1.0MiB", "t0"]
        );
        assert_eq!(volume_row(&volumes[0], &volumes)[5], "-");
    }

    #[test]
    fn linked_flags() {
        assert_eq!(linked_value(false, false), None);
        assert_eq!(linked_value(true, false), Some(true));
        assert_eq!(linked_value(false, true), Some(false));
    }

    #[test]
    fn image_bodies() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["i"];
            argv.extend_from_slice(args);
            ImageCli::try_parse_from(argv).map(|c| c.cmd)
        };
        let Ok(ImageCmd::Catalog {
            cmd: CatalogCmd::Add(add),
        }) = parse(&[
            "catalog",
            "add",
            "noble",
            "--url",
            "http://10.0.0.1/noble.img",
            "--checksum-url",
            "http://10.0.0.1/SHA256SUMS",
            "--os",
            "ubuntu",
            "--version",
            "24.04",
            "--default-user",
            "ubuntu",
            "--min-size",
            "3584M",
        ])
        else {
            panic!("catalog add did not parse");
        };
        assert_eq!(
            add.body(),
            json!({
                "id": "noble", "title": "noble", "os": "ubuntu", "version": "24.04", "arch": "x86_64",
                "url": "http://10.0.0.1/noble.img",
                "checksum": { "kind": "sums", "url": "http://10.0.0.1/SHA256SUMS" },
                "source_format": "qcow2", "firmware": "uefi", "default_user": "ubuntu",
                "min_size_bytes": 3584u64 << 20,
            })
        );
        // 체크섬은 하나만, 꼭 하나
        let add = |extra: &[&str]| {
            let mut argv = vec![
                "catalog",
                "add",
                "x",
                "--url",
                "http://h/x",
                "--os",
                "o",
                "--version",
                "1",
            ];
            argv.extend_from_slice(extra);
            parse(&argv).is_ok()
        };
        assert!(add(&["--sha256", "a"]));
        assert!(!add(&[]));
        assert!(!add(&["--sha256", "a", "--sha512", "b"]));
        let Ok(ImageCmd::Catalog {
            cmd: CatalogCmd::Add(add),
        }) = parse(&[
            "catalog",
            "add",
            "x",
            "--url",
            "http://h/x",
            "--sha512",
            "AB",
            "--arch",
            "aarch64",
            "--os",
            "alpine",
            "--version",
            "3.20",
            "--source-format",
            "raw",
            "--firmware",
            "bios",
        ])
        else {
            panic!("catalog add did not parse");
        };
        let body = add.body();
        assert_eq!(body["checksum"], json!({ "kind": "sha512", "value": "AB" }));
        assert_eq!(body["arch"], "aarch64");
        assert_eq!(body["source_format"], "raw");
        assert_eq!(body["firmware"], "bios");
        assert!(body.get("default_user").is_none());

        let get = |args: &[&str]| match parse(args).unwrap() {
            ImageCmd::Get(g) => *g,
            _ => unreachable!(),
        };
        let g = get(&["get", "ubuntu-24.04", "--pool", "local"]);
        assert_eq!(g.body(&Value::Null).unwrap(), json!({ "pool": "local" }));
        let g = get(&[
            "download",
            "ubuntu-24.04",
            "--pool",
            "local",
            "--name",
            "noble",
            "--arch",
            "x86_64",
            "--template",
            "ubuntu-tmpl",
            "--network",
            "lan",
            "--cpus",
            "4",
            "--memory",
            "4096",
            "--size",
            "20G",
            "--ci-user",
            "orbi",
        ]);
        assert_eq!(
            g.body(&json!({ "user": "orbi" })).unwrap(),
            json!({
                "pool": "local", "name": "noble", "arch": "x86_64",
                "template": {
                    "name": "ubuntu-tmpl", "network": "lan", "vcpus": 4, "memory_mib": 4096,
                    "size_bytes": 20u64 << 30, "cloud_init": { "user": "orbi" },
                },
            })
        );
        let g = get(&["get", "x", "--pool", "p", "--template", "t"]);
        assert!(
            g.body(&Value::Null)
                .unwrap_err()
                .to_string()
                .contains("--network")
        );
        let g = get(&["get", "x", "--pool", "p", "--ci-user", "orbi"]);
        assert!(
            g.body(&json!({ "user": "orbi" }))
                .unwrap_err()
                .to_string()
                .contains("--template")
        );
        // 템플릿 값은 --template과 함께만
        assert!(parse(&["get", "x", "--pool", "p", "--network", "lan"]).is_err());
    }

    #[test]
    fn image_list_row() {
        let i = json!({
            "id": "ubuntu-24.04", "arch": "x86_64", "os": "ubuntu", "version": "24.04",
            "firmware": "uefi", "source": "builtin", "volumes": ["ubuntu-24-04", "noble"],
        });
        assert_eq!(
            image_row(&i),
            [
                "ubuntu-24.04",
                "x86_64",
                "ubuntu",
                "24.04",
                "uefi",
                "builtin",
                "ubuntu-24-04, noble"
            ]
        );
        let mut i = i;
        i["volumes"] = json!([]);
        assert_eq!(image_row(&i)[6], "-");
    }
}
