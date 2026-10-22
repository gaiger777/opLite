//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get { node: String },
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀을 만든다.
    Create {
        name: String,
        /// 풀 디렉터리(절대 경로)
        #[arg(long)]
        path: PathBuf,
        /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
        #[arg(long = "node")]
        nodes: Vec<String>,
    },
    Delete {
        pool: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
    },
    Delete {
        volume: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    Delete {
        vm: String,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 호스트에 맞춘다(Apple Silicon: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(지금은 user만). 여러 번 줄 수 있다.
    #[arg(long = "nic")]
    nics: Vec<String>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let token = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &[
                        "NAME",
                        "ID",
                        "ADDRESS",
                        "ONLINE",
                        "CPUS",
                        "MEMORY",
                        "MAINTENANCE",
                    ],
                    |n| {
                        vec![
                            cell(&n["name"]),
                            cell(&n["id"]),
                            cell(&n["spec"]["address"]),
                            cell(&n["status"]["online"]),
                            cell(&n["status"]["cpus"]),
                            bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
                            cell(&n["spec"]["maintenance"]),
                        ]
                    },
                )?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                print_list(out, json_out, &list, &["NAME", "PATH", "NODES"], |p| {
                    let nodes = p["spec"]["nodes"].as_array().map_or(0, Vec::len);
                    vec![
                        cell(&p["name"]),
                        cell(&p["spec"]["backend"]["path"]),
                        if nodes == 0 {
                            "all".into()
                        } else {
                            nodes.to_string()
                        },
                    ]
                })?;
            }
            PoolCmd::Get { pool } => {
                print_json(out, &client.get(&format!("/storage-pools/{pool}")).await?)?
            }
            PoolCmd::Create { name, path, nodes } => {
                let body = json!({
                    "name": name,
                    "spec": { "backend": { "kind": "dir", "path": path }, "nodes": nodes }
                });
                client.post("/storage-pools", &body).await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    |t| {
                        vec![
                            cell(&t["id"]),
                            cell(&t["spec"]["op"]["kind"]),
                            cell(&t["spec"]["target"]),
                            cell(&t["status"]["state"]),
                            cell(&t["status"]["error"]),
                        ]
                    },
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            writeln!(out, "cluster: {}", cell(&view["name"]))?;
            writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
            let leader = view["leader"].clone();
            let rows: Vec<Vec<String>> = view["members"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|m| {
                    vec![
                        cell(&m["name"]),
                        cell(&m["id"]),
                        cell(&m["address"]),
                        if m["voter"] == true {
                            "voter"
                        } else {
                            "learner"
                        }
                        .into(),
                        cell(&m["online"]),
                        (m["id"] == leader).to_string(),
                    ]
                })
                .collect();
            table(
                out,
                &["NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER"],
                &rows,
            )?;
            let voters = view["members"]
                .as_array()
                .map_or(0, |m| m.iter().filter(|m| m["voter"] == true).count());
            if voters == 2 {
                writeln!(
                    out,
                    "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
                )?;
            }
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "PID", "ERROR"],
                |v| {
                    vec![
                        cell(&v["name"]),
                        cell(&v["spec"]["power"]),
                        cell(&v["status"]["phase"]),
                        cell(&v["status"]["node"]),
                        cell(&v["status"]["pid"]),
                        cell(&v["status"]["last_error"]),
                    ]
                },
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let machine = args.machine.unwrap_or(if cfg!(target_arch = "aarch64") {
                Machine::Virt
            } else {
                Machine::Q35
            });
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?["id"].clone()),
                None => None,
            };
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args
                .nics
                .iter()
                .map(|network| json!({ "network": network, "model": "virtio" }))
                .collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Delete { vm } => {
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !server.starts_with("https://") {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs, ca_pem: Option<String>) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&json!({ "username": args.user, "password": password }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
