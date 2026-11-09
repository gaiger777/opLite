//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{
    ApiError, BACKUP_WAIT, Client, DRAIN_WAIT, IMPORT_WAIT, MIGRATE_WAIT, UPGRADE_WAIT, fetch_ca,
    fingerprint, local, seg,
};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version = opl_types::version::BUILD, about = "opLite command-line client")]
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
    /// 저장한 API 토큰을 지운다(login이 만든 토큰 `oplctl-…`이면 서버에서도).
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
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지나 opld가 만드는 VXLAN 네트워크)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// VM NIC에 거는 방화벽 규칙 묶음. nftables가 있는 노드가 적용한다.
    SecurityGroup {
        #[command(subcommand)]
        cmd: SecurityGroupCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    /// VM 백업(백업 풀에 담은 디스크 복사본과 VM 설정)과 복원
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// 백업 작업: 정한 시각(UTC)에 고른 VM들을 백업하고 보존 규칙대로 옛 백업을 지운다.
    BackupJob {
        #[command(subcommand)]
        cmd: BackupJobCmd,
    },
    /// 다른 하이퍼바이저(VMware)의 VM 가져오기(OVA, ESXi·vCenter)
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
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
    /// 인증 영역(admin만): 바깥 LDAP 디렉터리나 OIDC IdP로 로그인하게 한다.
    Realm {
        #[command(subcommand)]
        cmd: RealmCmd,
    },
    /// 클러스터 설정(업데이트 채널)
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// 구독 키와 상태. 구독은 기능을 막지 않는다.
    Subscription {
        #[command(subcommand)]
        cmd: SubscriptionCmd,
    },
    /// 옵트인 텔레메트리(기본 꺼짐). 켜면 리더가 하루 한 번 클러스터 규모를 보낸다.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },
    /// 감사 로그(admin만): 모든 노드의 바꾸는 요청·로그인·콘솔 열기를 새 것부터
    Audit(AuditArgs),
    /// 원격 클러스터: 다른 opLite 클러스터를 등록해 상태를 모아 보고, 그 클러스터의 API를 이 클러스터를
    /// 거쳐 부른다(저장한 그 클러스터의 API 토큰으로).
    Remote {
        #[command(subcommand)]
        cmd: RemoteCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin", conflicts_with = "token_file")]
    user: String,
    /// 인증 영역. `local`이 아니면 그 영역의 사용자 이름(`@영역` 없이)으로 로그인한다.
    #[arg(long, default_value = "local", conflicts_with = "token_file")]
    realm: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long, conflicts_with = "token_file")]
    password_file: Option<PathBuf>,
    /// 웹 UI에서 만든 API 토큰이 든 파일(`-`면 표준 입력). 비밀번호 대신 이 토큰을 확인해 저장한다
    /// (SSO(OIDC) 사용자).
    #[arg(long)]
    token_file: Option<PathBuf>,
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
    Get {
        node: String,
    },
    /// 노드의 업데이트 확인 결과(올릴 수 있는 패키지)를 보인다.
    Updates {
        node: String,
        /// 먼저 노드에서 다시 확인하고(Task가 끝날 때까지 기다린다) 결과를 보인다.
        #[arg(long)]
        refresh: bool,
    },
    /// 노드의 패키지를 올린다(`apt-get dist-upgrade`). VM은 계속 돌고 opld가 잠깐 다시 시작한다. 한 번에
    /// 한 노드만 한다.
    Upgrade {
        node: String,
        /// 업그레이드 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
    /// 노드를 유지보수로 바꾸거나(`on` — 새 VM을 놓지 않는다) 끝낸다(`off`). 도는 VM은 그대로다.
    Maintenance {
        node: String,
        state: Switch,
    },
    /// 노드를 유지보수로 바꾸고 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션한다. 로컬 풀
    /// 디스크 VM은 옮기지 못한다.
    Drain {
        node: String,
        /// 비우기 Task가 끝날 때까지 기다린다. 옮기지 못한 VM이 있으면 실패한다.
        #[arg(long)]
        wait: bool,
    },
}

/// `node maintenance`의 켜기·끄기
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Switch {
    On,
    Off,
}

/// `node maintenance`: 노드를 읽어 라벨과 spec은 그대로 두고 `maintenance`만 바꿔 그 revision으로
/// (`If-Match`) 다시 쓴다. 읽은 뒤 status만 바뀌어도 revision이 달라지므로 `revision_mismatch`면 다시 한다.
/// 끄면 그 노드의 끝나지 않은 비우기가 멈춘다고 `err`(stderr)에 알린다(Task 목록을 읽지 못하면 그 경고).
async fn node_maintenance(
    client: &Client,
    node: &str,
    on: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let path = format!("/nodes/{}", seg(node));
    let mut retries = SpecRetries::default();
    let id = loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&spec_update(&current, "maintenance", json!(on)));
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => break cell(&current["id"]),
        }
    };
    let state = if on { "on" } else { "off" };
    writeln!(out, "node {node}: maintenance {state}")?;
    if !on {
        // 비우기 Task는 유지보수가 끝나면 멈춘다. 유지보수는 이미 꺼졌으므로 찾아보지 못해도 경고만 한다.
        let tasks = match client.get("/tasks").await {
            Ok(tasks) => tasks,
            Err(e) => {
                writeln!(err, "warning: cannot check for a running drain: {e}")?;
                return Ok(());
            }
        };
        let draining = tasks["items"].as_array().into_iter().flatten().find(|t| {
            t["spec"]["op"]["kind"] == "node-drain"
                && cell(&t["spec"]["node"]) == id
                && !matches!(t["status"]["state"].as_str(), Some("succeeded" | "failed"))
        });
        if let Some(task) = draining {
            writeln!(
                err,
                "warning: the running drain of {node} stops (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

/// `node drain`: 비우기 Task를 만든다(노드는 곧바로 유지보수가 된다). `wait`이면 끝날 때까지 기다리고,
/// 아니면 Task를 따라가는 방법을 알린다.
async fn node_drain(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{}/drain", seg(node)), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, DRAIN_WAIT).await?;
        writeln!(out, "node {node} drained")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "drain of {node} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node upgrade`: 업그레이드 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn node_upgrade(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{}/upgrade", seg(node)), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, UPGRADE_WAIT).await?;
        writeln!(out, "node {node} upgraded")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "upgrade of {node} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node updates`의 패키지 표 열
const UPDATE_HEADERS: &[&str] = &["PACKAGE", "CURRENT", "CANDIDATE"];

/// `node updates`: `--refresh`면 다시 확인 Task를 기다린 뒤, 노드의 `status.updates`를 보인다. 확인 시각,
/// 오류, 패키지 표(새로 설치할 패키지는 CURRENT가 `-`), 잘렸는지. `-o json`이면 `status.updates` 그대로.
async fn node_updates(
    client: &Client,
    node: &str,
    refresh: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if refresh {
        let task = client
            .post(&format!("/nodes/{}/updates/refresh", seg(node)), &json!({}))
            .await?;
        client.wait_task(&task).await?;
    }
    let found = client.get(&format!("/nodes/{}", seg(node))).await?;
    let updates = &found["status"]["updates"];
    if json_out {
        return Ok(print_json(out, updates)?);
    }
    if !updates.is_object() {
        bail!("node {node} does not check for updates");
    }
    let checked = updates["checked_at"]
        .as_i64()
        .map_or_else(|| "never".to_string(), unix_time);
    writeln!(out, "checked: {checked}")?;
    if let Some(error) = updates["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let packages = updates["packages"].as_array().cloned().unwrap_or_default();
    if packages.is_empty() {
        if updates["checked_at"].is_u64() && updates["error"].is_null() {
            writeln!(out, "no updates available")?;
        }
        return Ok(());
    }
    let rows: Vec<_> = packages
        .iter()
        .map(|p| vec![cell(&p["name"]), cell(&p["current"]), cell(&p["candidate"])])
        .collect();
    table(out, UPDATE_HEADERS, &rows)?;
    if updates["truncated"] == true {
        writeln!(out, "(only the first {} packages are shown)", rows.len())?;
    }
    Ok(())
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다. 백업을 담는 풀로 표시하거나 끈다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args(["shared", "no_shared", "backups"])))]
struct PoolSet {
    pool: String,
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long, conflicts_with = "no_shared")]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
    /// VM 백업을 담는 풀로 표시한다(true, 디렉터리·NFS 풀만)거나 끈다(false, 백업이 남아 있으면 거절한다).
    #[arg(long, value_name = "true|false")]
    backups: Option<bool>,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`). 주지 않은 값은 보내지 않는다.
fn pool_set_body(set: &PoolSet) -> Value {
    let mut body = json!({});
    if set.shared || set.no_shared {
        body["shared"] = json!(set.shared);
    }
    if let Some(backups) = set.backups {
        body["backups"] = json!(backups);
    }
    body
}

/// `pool set`이 바꾼 것: `shared, backups on`
fn pool_set_summary(set: &PoolSet) -> String {
    let mut parts = Vec::new();
    if set.shared {
        parts.push("shared");
    } else if set.no_shared {
        parts.push("node-local");
    }
    match set.backups {
        Some(true) => parts.push("backups on"),
        Some(false) => parts.push("backups off"),
        None => {}
    }
    parts.join(", ")
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
    /// VM 백업을 담는 풀로 표시한다. 디렉터리·NFS 풀만 된다.
    #[arg(long, conflicts_with_all = ["lvm_thin", "zfs", "rbd", "lvm_shared"])]
    backups: bool,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
        backups,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    if backups {
        spec["backups"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크나 VXLAN 네트워크를 만든다.
    Create(NetworkCreate),
    Delete {
        network: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["bridge", "vxlan"])))]
struct NetworkCreate {
    name: String,
    /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
    #[arg(long)]
    bridge: Option<String>,
    /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
    #[arg(long, conflicts_with = "vxlan")]
    vlan: Option<u16>,
    /// VXLAN 네트워크 식별자(VNI, 1-16777215). opld가 모든 노드에 브리지 oplv<VNI>를 만든다.
    #[arg(long, value_name = "VNI")]
    vxlan: Option<u32>,
    /// VXLAN 네트워크의 MTU(1280-9000, 기본 1450)
    #[arg(long, conflicts_with = "bridge")]
    mtu: Option<u16>,
}

/// `network create`의 요청 본문(`POST /networks`). 주지 않은 값은 보내지 않는다.
fn network_create_body(c: &NetworkCreate) -> Value {
    let mut spec = serde_json::Map::new();
    if let Some(bridge) = &c.bridge {
        spec.insert("bridge".into(), json!(bridge));
    }
    if let Some(vlan) = c.vlan {
        spec.insert("vlan".into(), json!(vlan));
    }
    if let Some(vni) = c.vxlan {
        spec.insert("vxlan".into(), json!({ "vni": vni }));
    }
    if let Some(mtu) = c.mtu {
        spec.insert("mtu".into(), json!(mtu));
    }
    json!({ "name": c.name, "spec": spec })
}

#[derive(Subcommand)]
enum SecurityGroupCmd {
    List,
    Get {
        group: String,
    },
    /// 보안 그룹을 만든다. 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. 맞는 규칙이 없으면
    /// 들어오는 것은 막고 나가는 것은 보낸다.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 규칙 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`
        /// (예: "in accept tcp 22 10.0.0.0/8"). 여러 번 줄 수 있다.
        #[arg(long = "rule", value_name = "RULE", value_parser = parse_rule)]
        rules: Vec<Value>,
    },
    /// 보안 그룹을 지운다. 그룹을 쓰는 VM NIC가 없어야 한다.
    Delete {
        group: String,
    },
    /// 규칙을 더하거나 뺀다. 그룹을 쓰는 노드가 곧바로 적용한다.
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// 규칙 하나를 더한다(RULE은 `security-group create --rule`과 같다).
    Add {
        group: String,
        #[arg(value_parser = parse_rule)]
        rule: Value,
        /// 이 번호(1부터) 자리에 넣는다. 생략하면 맨 끝이다.
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// N번 규칙(1부터, `security-group get`의 순서)을 뺀다.
    Rm { group: String, n: usize },
}

/// `in accept tcp 22 10.0.0.0/8`처럼 쓴다: 방향과 동작, 그 뒤로 프로토콜·포트·원격 CIDR을 이 차례로
/// (모두 생략할 수 있다). 값의 나머지 검사(포트 범위, 프로토콜과 포트·주소의 짝)는 API가 한다.
fn parse_rule(s: &str) -> Result<Value, String> {
    let bad = || {
        format!("invalid rule {s:?}: use in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]")
    };
    let mut words = s.split_whitespace().peekable();
    let direction = words
        .next()
        .filter(|w| matches!(*w, "in" | "out"))
        .ok_or_else(bad)?;
    let action = words
        .next()
        .filter(|w| matches!(*w, "accept" | "drop"))
        .ok_or_else(bad)?;
    let mut rule = json!({ "direction": direction, "action": action });
    if let Some(protocol) = words.next_if(|w| matches!(*w, "tcp" | "udp" | "icmp" | "icmpv6")) {
        rule["protocol"] = json!(protocol);
    }
    if let Some(ports) = words.next_if(|w| is_ports(w)) {
        rule["ports"] = json!(ports);
    }
    if let Some(remote) = words.next_if(|w| w.contains(['.', ':'])) {
        rule["remote"] = json!(remote);
    }
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(rule)
}

/// `22`나 `8000-8080` 모양인지
fn is_ports(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match word.split_once('-') {
        Some((start, end)) => digits(start) && digits(end),
        None => digits(word),
    }
}

/// `security-group create`의 요청 본문(`POST /security-groups`)
fn security_group_create_body(name: &str, description: Option<&str>, rules: &[Value]) -> Value {
    let mut spec = json!({ "rules": rules });
    if let Some(description) = description {
        spec["description"] = json!(description);
    }
    json!({ "name": name, "spec": spec })
}

/// 그룹의 규칙에 `rule`을 `at`번(1부터, 생략하면 맨 끝) 자리에 넣은 목록
fn rules_with(group: &Value, rule: Value, at: Option<usize>) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = at.unwrap_or(rules.len() + 1);
    if at == 0 || at > rules.len() + 1 {
        bail!(
            "cannot add a rule at {at}: security group {} has {} rules (use 1 to {})",
            cell(&group["name"]),
            rules.len(),
            rules.len() + 1
        );
    }
    rules.insert(at - 1, rule);
    Ok(rules)
}

/// 그룹의 규칙에서 `n`번(1부터)을 뺀 목록
fn rules_without(group: &Value, n: usize) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if n == 0 || n > rules.len() {
        bail!(
            "security group {} has no rule {n} ({} rules, numbered from 1)",
            cell(&group["name"]),
            rules.len()
        );
    }
    rules.remove(n - 1);
    Ok(rules)
}

/// 그룹을 읽어 `edit`으로 만든 규칙 목록으로 바꾼다. 읽은 revision을 `If-Match`로 보내므로 그 사이
/// 다른 사람이 바꿨으면 실패한다(규칙 번호가 달라졌을 수 있다).
async fn patch_rules(
    client: &Client,
    group: &str,
    edit: impl FnOnce(&Value) -> anyhow::Result<Vec<Value>>,
) -> anyhow::Result<()> {
    let path = format!("/security-groups/{}", seg(group));
    let current = client.get(&path).await?;
    let rules = edit(&current)?;
    let r = client
        .request(Method::PATCH, &path)
        .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
        .json(&json!({ "rules": rules }));
    client.send(r).await?;
    Ok(())
}

/// `security-group list`의 열
const SECURITY_GROUP_HEADERS: &[&str] = &["NAME", "RULES", "DESCRIPTION"];

/// `security-group list`의 한 행
fn security_group_row(g: &Value) -> Vec<String> {
    vec![
        cell(&g["name"]),
        g["spec"]["rules"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        cell(&g["spec"]["description"]),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
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
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
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
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
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
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    /// NIC의 보안 그룹을 바꾼다. 실행 중이어도 곧바로 적용된다. 그룹을 주지 않으면 모두 뺀다.
    NicSg {
        vm: String,
        /// NIC 번호(0부터, `vm get`의 nics 순서)
        index: usize,
        groups: Vec<String>,
    },
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
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
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
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다. `,sg=A+B`를 붙이면 그 NIC에
    /// 보안 그룹을 건다(예: `lan,sg=web+db`).
    #[arg(long = "nic", value_name = "NETWORK[,sg=GROUP+...]", value_parser = parse_nic)]
    nics: Vec<NicArg>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

/// `--nic`으로 받은 네트워크와 보안 그룹
#[derive(Debug, Clone, PartialEq, Eq)]
struct NicArg {
    network: String,
    security_groups: Vec<String>,
}

/// `lan`, `lan,sg=web+db`처럼 쓴다. 그룹이 있는지는 API가 본다.
fn parse_nic(s: &str) -> Result<NicArg, String> {
    let bad = || format!("invalid nic {s:?}: use NETWORK or NETWORK,sg=GROUP+GROUP");
    let (network, groups) = match s.split_once(',') {
        Some((network, option)) => {
            let groups = option.strip_prefix("sg=").ok_or_else(bad)?;
            let groups: Vec<String> = groups.split('+').map(str::to_string).collect();
            if groups.iter().any(|g| g.is_empty() || g.contains(',')) {
                return Err(bad());
            }
            (network, groups)
        }
        None => (s, Vec::new()),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NicArg {
        network: network.into(),
        security_groups: groups,
    })
}

/// `vm create`의 NIC 하나. 그룹이 없으면 `security_groups`를 보내지 않는다.
fn nic_body(nic: &NicArg) -> Value {
    let mut body = json!({ "network": nic.network, "model": "virtio" });
    if !nic.security_groups.is_empty() {
        body["security_groups"] = json!(nic.security_groups);
    }
    body
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum BackupCmd {
    /// VM을 백업한다. 실행 중이면 디스크를 잠깐 스냅샷한 때의 상태(crash-consistent)를 담는다.
    Create {
        vm: String,
        /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
        #[arg(long)]
        pool: String,
        /// 생략하면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
        #[arg(long)]
        name: Option<String>,
        /// 할 수 있으면 증분으로(같은 풀의 바로 앞 백업을 부모로 바뀐 블록만) 백업한다. 할 수 없으면
        /// 전체로 하고 그 이유를 보인다.
        #[arg(long)]
        incremental: bool,
    },
    /// 백업 목록. VM을 지운 백업도 보인다.
    List {
        /// 이 VM(이름이나 id)의 백업만. 지운 VM은 백업에 남은 이름으로 찾는다.
        #[arg(long)]
        vm: Option<String>,
    },
    /// 백업(JSON). 증분 백업은 `chain`에 전체 백업부터 이 백업까지의 이름과 체인 파일 크기의 합을 더한다.
    Get { backup: String },
    /// 백업 파일과 기록을 지운다. 실패한 백업도 지운다.
    Delete {
        backup: String,
        /// 파일을 지울 노드가 없으면 기록만 지운다(파일은 백업 풀에 남는다).
        #[arg(long)]
        force: bool,
    },
    /// 백업에서 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Restore {
        backup: String,
        /// 새 VM 이름
        #[arg(long)]
        name: String,
        /// 새 디스크를 둘 풀
        #[arg(long)]
        pool: String,
        /// 복원을 실행할 노드(이름이나 id). 생략하면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
        #[arg(long)]
        node: Option<String>,
    },
    /// 백업을 원격 클러스터(`remote add`로 등록)의 백업 풀에 사본으로 보낸다. 원격 쪽 토큰 주인은
    /// operator 이상이어야 한다.
    Replicate {
        backup: String,
        /// 원격 이름
        #[arg(long)]
        remote: String,
        /// 원격의 백업 풀
        #[arg(long)]
        pool: String,
        /// 복제 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Subcommand)]
enum BackupJobCmd {
    /// 백업 작업을 만든다. 정한 요일의 시각(UTC)에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛
    /// 백업을 지운다.
    Create(BackupJobCreate),
    List,
    Get {
        job: String,
    },
    /// 준 값만 바꾼다. 일정이나 보존 규칙의 일부만 주면 나머지는 지금 값을 그대로 둔다.
    Set(BackupJobSet),
    /// 작업만 지운다. 작업이 만든 백업은 남는다.
    Delete {
        job: String,
    },
    /// 지금 실행한다. 리더의 스케줄러가 다음 차례에 시작하며 끝나기를 기다리지 않는다.
    Run {
        job: String,
    },
}

/// 백업할 VM 고르기. 셋 중 하나만 준다.
#[derive(Args)]
struct VmsArgs {
    /// 모든 VM
    #[arg(long)]
    all: bool,
    /// 이 이름의 VM(여러 번 줄 수 있다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
    /// 이 라벨을 모두 가진 VM(여러 번 줄 수 있다)
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    labels: Vec<(String, String)>,
}

/// 보존 규칙. 모두 0이면 지우지 않는다.
#[derive(Args)]
struct KeepArgs {
    /// 최신 N개를 남긴다.
    #[arg(long, value_name = "N")]
    keep_last: Option<u32>,
    /// 그다음 하루(UTC)마다 가장 새 것 하나씩 N일
    #[arg(long, value_name = "N")]
    keep_daily: Option<u32>,
    /// 그다음 ISO 주마다 가장 새 것 하나씩 N주
    #[arg(long, value_name = "N")]
    keep_weekly: Option<u32>,
    /// 그다음 달마다 가장 새 것 하나씩 N달
    #[arg(long, value_name = "N")]
    keep_monthly: Option<u32>,
}

impl KeepArgs {
    fn given(&self) -> bool {
        self.keep_last.is_some()
            || self.keep_daily.is_some()
            || self.keep_weekly.is_some()
            || self.keep_monthly.is_some()
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").required(true).args(["all", "vms", "labels"])))]
struct BackupJobCreate {
    name: String,
    /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
    #[arg(long)]
    pool: String,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
    /// 작업이 만든 백업을 원격의 백업 풀에 사본으로 보낸다.
    #[arg(long, value_name = "REMOTE/POOL", value_parser = parse_replicate)]
    replicate: Option<Value>,
    /// 1 이상이면 할 수 있을 때 증분으로 백업하고, 체인의 증분이 N개가 되면 다음은 전체다. 0이면 늘 전체.
    #[arg(long, value_name = "N")]
    full_every: Option<u32>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").args(["all", "vms", "labels"])))]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "all", "vms", "labels", "pool", "at", "days",
    "keep_last", "keep_daily", "keep_weekly", "keep_monthly", "enabled",
    "replicate", "no_replicate", "full_every",
])))]
struct BackupJobSet {
    job: String,
    #[arg(long)]
    pool: Option<String>,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 모든 요일을 주면 매일이다.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 켜거나(다음 실행 시각을 지금부터 다시 정한다) 끈다. 진행 중인 실행은 끝까지 간다.
    #[arg(long, value_name = "true|false")]
    enabled: Option<bool>,
    /// 작업이 만든 백업을 원격의 백업 풀에 사본으로 보낸다.
    #[arg(long, value_name = "REMOTE/POOL", value_parser = parse_replicate, conflicts_with = "no_replicate")]
    replicate: Option<Value>,
    /// 원격 복제를 끈다. 이미 보낸 사본은 남는다.
    #[arg(long)]
    no_replicate: bool,
    /// 1 이상이면 할 수 있을 때 증분으로 백업하고, 체인의 증분이 N개가 되면 다음은 전체다. 0이면 늘 전체.
    #[arg(long, value_name = "N")]
    full_every: Option<u32>,
}

/// `--replicate`의 `REMOTE/POOL`을 `replicate` 값(`{ remote, pool }`)으로
fn parse_replicate(s: &str) -> Result<Value, String> {
    match s.split_once('/') {
        Some((remote, pool)) if !remote.is_empty() && !pool.is_empty() && !pool.contains('/') => {
            Ok(json!({ "remote": remote, "pool": pool }))
        }
        _ => Err(format!("invalid target {s:?}: use REMOTE/POOL")),
    }
}

/// `--label`의 `KEY=VALUE`. 값에는 `=`가 있어도 된다.
fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err(format!("invalid label {s:?}: use KEY=VALUE")),
    }
}

/// VM 고르기(`vms` 값). 주지 않았으면 `None`이다.
fn vms_value(args: &VmsArgs) -> Option<Value> {
    if args.all {
        Some(json!({ "all": true }))
    } else if !args.vms.is_empty() {
        Some(json!({ "names": args.vms }))
    } else if !args.labels.is_empty() {
        let labels: serde_json::Map<String, Value> = args
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Some(json!({ "labels": labels }))
    } else {
        None
    }
}

fn days_value(days: &[Day]) -> Value {
    json!(days.iter().map(|d| value_name(*d)).collect::<Vec<_>>())
}

/// 보존 규칙(`keep` 값). 주지 않은 단위는 `current`의 값, 거기도 없으면 0이다.
fn keep_value(args: &KeepArgs, current: &Value) -> Value {
    let pick = |arg: Option<u32>, field: &str| {
        arg.map_or_else(|| current[field].as_u64().unwrap_or(0), u64::from)
    };
    json!({
        "last": pick(args.keep_last, "last"),
        "daily": pick(args.keep_daily, "daily"),
        "weekly": pick(args.keep_weekly, "weekly"),
        "monthly": pick(args.keep_monthly, "monthly"),
    })
}

/// `backup-job create`의 요청 본문
fn backup_job_create_body(args: &BackupJobCreate) -> Value {
    let mut body = json!({
        "name": args.name,
        "spec": {
            "vms": vms_value(&args.vms),
            "pool": args.pool,
            "schedule": { "at": args.at, "days": days_value(&args.days) },
            "keep": keep_value(&args.keep, &Value::Null),
            "enabled": !args.disabled,
        }
    });
    // 복제하지 않으면 보내지 않는다(옛 서버는 모르는 필드를 거절한다).
    if let Some(replicate) = &args.replicate {
        body["spec"]["replicate"] = replicate.clone();
    }
    // 주지 않으면 보내지 않는다(옛 서버는 모르는 필드를 거절한다).
    if let Some(full_every) = args.full_every {
        body["spec"]["full_every"] = json!(full_every);
    }
    body
}

/// `backup-job set`의 요청 본문(`PATCH /backup-jobs/{key}`). 준 값만 보낸다. 서버는 일정과 보존
/// 규칙을 통째로 바꾸므로 그 일부만 주었으면 나머지는 읽은 작업(`current`)의 값으로 채운다.
fn backup_job_set_body(args: &BackupJobSet, current: &Value) -> Value {
    let mut body = json!({});
    if let Some(vms) = vms_value(&args.vms) {
        body["vms"] = vms;
    }
    if let Some(pool) = &args.pool {
        body["pool"] = json!(pool);
    }
    let schedule = &current["spec"]["schedule"];
    if args.at.is_some() || !args.days.is_empty() {
        body["schedule"] = json!({
            "at": args.at.as_ref().map_or_else(|| schedule["at"].clone(), |at| json!(at)),
            "days": if args.days.is_empty() {
                schedule["days"].as_array().map_or_else(|| json!([]), |d| json!(d))
            } else {
                days_value(&args.days)
            },
        });
    }
    if args.keep.given() {
        body["keep"] = keep_value(&args.keep, &current["spec"]["keep"]);
    }
    if let Some(enabled) = args.enabled {
        body["enabled"] = json!(enabled);
    }
    // `null`은 복제를 끈다(생략하면 그대로).
    if let Some(replicate) = &args.replicate {
        body["replicate"] = replicate.clone();
    } else if args.no_replicate {
        body["replicate"] = Value::Null;
    }
    if let Some(full_every) = args.full_every {
        body["full_every"] = json!(full_every);
    }
    body
}

#[derive(Subcommand)]
enum ImportCmd {
    /// VMware 등에서 내보낸 OVA를 이 서버의 노드에 올리고, 원본 VM을 읽어 VM 설정을 제안받는다.
    /// 원본 파일은 그 노드에 남고 실행도 그 노드에서 한다.
    Ova {
        file: PathBuf,
        /// 가져오기 이름
        #[arg(long)]
        name: String,
    },
    /// ESXi·vCenter의 데이터스토어에 있는 VM을 보인다. 비밀번호는 `--password-file`이나
    /// `OPL_ESXI_PASSWORD`에서, 없으면 터미널에서 묻는다. 지문을 주지 않으면 서버 인증서의 지문을 알려 준다.
    EsxiBrowse {
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// ESXi·vCenter의 꺼진 VM을 읽어 VM 설정을 제안받는다. 비밀번호는 `esxi-browse`처럼 받고 연결을
    /// 받은 노드의 메모리에만 남으므로, 그 opld를 다시 시작하면 가져오기를 다시 만들어야 한다.
    Esxi {
        /// 가져오기 이름
        name: String,
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문(`import esxi-browse`가 알려 준다)
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        datastore: String,
        /// 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`)
        #[arg(long)]
        vmx: String,
    },
    List,
    /// 원본 VM, 제안, 경고, 마지막 실행의 오류
    Get {
        import: String,
    },
    /// 원본 디스크를 볼륨으로 옮겨 새 VM(꺼짐)을 만든다. 주지 않은 값은 제안 값이다.
    Run(Box<ImportRunArgs>),
    /// 가져오기와 원본 파일을 지운다(만든 VM은 남는다).
    Delete {
        import: String,
        /// 원본 노드가 없거나 오프라인이면 기록만 지운다(원본 파일은 그 노드에 남는다).
        #[arg(long)]
        force: bool,
    },
}

/// ESXi·vCenter 연결. 비밀번호는 인자로 받지 않는다.
#[derive(Args)]
struct EsxiArgs {
    /// ESXi 호스트나 vCenter 주소
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u16,
    #[arg(long)]
    user: String,
    /// vCenter면 데이터센터 이름
    #[arg(long, default_value = "ha-datacenter")]
    datacenter: String,
    /// 비밀번호가 든 파일. 없으면 `OPL_ESXI_PASSWORD`, 그것도 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
}

/// ESXi 비밀번호. `--password-file`, `OPL_ESXI_PASSWORD` 순서로 보고, 둘 다 없으면 터미널에서 묻는다.
fn esxi_password(conn: &EsxiArgs) -> anyhow::Result<String> {
    if let Some(file) = &conn.password_file {
        return read_password(Some(file), "");
    }
    match std::env::var("OPL_ESXI_PASSWORD") {
        Ok(password) => Ok(password),
        Err(_) => Ok(rpassword::prompt_password(format!(
            "password for {}@{}: ",
            conn.user, conn.host
        ))?),
    }
}

/// API의 `EsxiConnection`
fn esxi_connection(conn: &EsxiArgs, fingerprint: Option<&str>, password: &str) -> Value {
    json!({
        "host": conn.host,
        "port": conn.port,
        "user": conn.user,
        "password": password,
        "datacenter": conn.datacenter,
        "fingerprint": fingerprint,
    })
}

/// `import esxi`의 요청 본문
fn esxi_import_body(name: &str, connection: &Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": connection, "datastore": datastore, "vmx": vmx })
}

#[derive(Args)]
struct ImportRunArgs {
    import: String,
    /// 새 VM 이름
    #[arg(long)]
    name: String,
    /// 디스크 볼륨을 만들 풀
    #[arg(long)]
    pool: String,
    /// 원본 네트워크를 opLite 네트워크에 잇는다(예: `"VM Network=lan"`). 원본 NIC의 네트워크마다
    /// 준다. 원본 네트워크가 하나면 `NETWORK`만 줘도 된다.
    #[arg(long = "net", value_name = "SOURCE=NETWORK", value_parser = parse_net)]
    nets: Vec<NetArg>,
    #[arg(long, value_enum)]
    disk_bus: Option<DiskBus>,
    #[arg(long, value_enum)]
    nic_model: Option<NicModel>,
    #[arg(long, value_enum)]
    firmware: Option<Firmware>,
    #[arg(long)]
    sockets: Option<u16>,
    #[arg(long)]
    cores: Option<u16>,
    /// 메모리(MiB)
    #[arg(long)]
    memory: Option<u32>,
    /// 원본 MAC 주소 대신 새 MAC을 받는다.
    #[arg(long)]
    new_macs: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DiskBus {
    VirtioBlk,
    Sata,
}

#[derive(Clone, Copy, ValueEnum)]
enum NicModel {
    Virtio,
    E1000e,
}

/// `--net`으로 받은 매핑. `source`가 없으면 하나뿐인 원본 네트워크다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NetArg {
    source: Option<String>,
    network: String,
}

fn parse_net(s: &str) -> Result<NetArg, String> {
    let bad = || format!("invalid network mapping {s:?}: use SOURCE=NETWORK or NETWORK");
    // 원본 네트워크 이름에 `=`가 있을 수 있다. opLite 네트워크 이름에는 없다.
    let (source, network) = match s.rsplit_once('=') {
        Some(("", _)) => return Err(bad()),
        Some((source, network)) => (Some(source.to_string()), network),
        None => (None, s),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NetArg {
        source,
        network: network.into(),
    })
}

/// 가져오기의 원본 네트워크(제안의 네트워크 매핑 키)
fn source_networks(import: &Value) -> Vec<String> {
    import["status"]["proposal"]["networks"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `import run`의 요청 본문. `sources`는 `=` 없는 `--net`을 이을 원본 네트워크다.
fn import_run_body(args: &ImportRunArgs, sources: &[String]) -> anyhow::Result<Value> {
    let mut networks = serde_json::Map::new();
    for net in &args.nets {
        let source = match (&net.source, sources) {
            (Some(source), _) => source.clone(),
            (None, [only]) => only.clone(),
            (None, _) => bail!(
                "--net {} needs SOURCE=NETWORK: vm-import {} has {} source networks ({})",
                net.network,
                args.import,
                sources.len(),
                sources
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        networks.insert(source, json!(net.network));
    }
    Ok(json!({
        "name": args.name,
        "pool": args.pool,
        "networks": networks,
        "disk_bus": args.disk_bus.map(value_name),
        "nic_model": args.nic_model.map(value_name),
        "firmware": args.firmware.map(value_name),
        "sockets": args.sockets,
        "cores": args.cores,
        "memory_mib": args.memory,
        "keep_macs": !args.new_macs,
    }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// 설정을 `키: 값` 줄로 보인다.
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing|enterprise)
    Set {
        /// 예: `updates.channel=testing`
        #[arg(required = true, value_name = "KEY=VALUE")]
        values: Vec<String>,
    },
}

/// `settings set`이 아는 키
const SETTING_KEYS: &[&str] = &["updates.channel"];

/// `settings set`의 요청 본문(`PATCH /settings`). 준 키만 보낸다.
fn settings_set_body(values: &[String]) -> anyhow::Result<Value> {
    let mut body = json!({});
    for value in values {
        let Some((key, v)) = value.split_once('=') else {
            bail!("expected KEY=VALUE, got {value:?}");
        };
        match key {
            "updates.channel" => body["updates"]["channel"] = json!(v),
            _ => bail!("unknown setting {key}; known: {}", SETTING_KEYS.join(", ")),
        }
    }
    Ok(body)
}

/// 객체의 값들을 `a.b: 값` 줄로 펼친다.
fn setting_lines(prefix: &str, value: &Value, lines: &mut Vec<String>) {
    match value.as_object() {
        Some(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                setting_lines(&key, v, lines);
            }
        }
        None => lines.push(format!("{prefix}: {}", cell(value))),
    }
}

async fn settings(
    client: &Client,
    cmd: SettingsCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SettingsCmd::Get => {
            let settings = client.get("/settings").await?;
            if json_out {
                print_json(out, &settings)?;
            } else {
                let mut lines = Vec::new();
                setting_lines("", &settings["spec"], &mut lines);
                for line in lines {
                    writeln!(out, "{line}")?;
                }
            }
        }
        SettingsCmd::Set { values } => {
            client
                .patch("/settings", &settings_set_body(&values)?)
                .await?;
            writeln!(out, "settings updated")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum SubscriptionCmd {
    /// 구독 상태를 보인다.
    Show,
    /// 구독 키를 넣는다(있던 키는 바꾼다).
    #[command(group(ArgGroup::new("source").required(true).args(["key", "file"])))]
    Set {
        /// `opl1.<payload>.<signature>`
        key: Option<String>,
        /// 키가 든 파일(`-`면 표준 입력)
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 구독 키를 지운다.
    Remove,
}

/// 구독 상태(`SubscriptionInfo`)를 `키: 값` 줄로. 없는 값은 줄을 쓰지 않는다.
fn subscription_lines(info: &Value) -> Vec<String> {
    let mut state = cell(&info["state"]);
    if info["development"] == true {
        state.push_str(" (development key)");
    }
    let mut lines = vec![format!("state: {state}")];
    for key in ["customer", "level"] {
        if !info[key].is_null() {
            lines.push(format!("{key}: {}", cell(&info[key])));
        }
    }
    let used = cell(&info["used_nodes"]);
    lines.push(match info["nodes"].as_u64() {
        Some(limit) => format!("nodes: {used}/{limit}"),
        None => format!("nodes: {used}"),
    });
    if let Some(at) = info["expires_at"].as_u64() {
        lines.push(format!("expires: {}", opl_types::subscription::date(at)));
    }
    lines
}

fn print_subscription(
    out: &mut (dyn Write + Send),
    json_out: bool,
    info: &Value,
) -> anyhow::Result<()> {
    if json_out {
        print_json(out, info)?;
    } else {
        for line in subscription_lines(info) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

async fn subscription(
    client: &Client,
    cmd: SubscriptionCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SubscriptionCmd::Show => {
            let info = client.get("/subscription").await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Set { key, file } => {
            let key = match key {
                Some(key) => key,
                None => match file {
                    Some(path) if path.as_os_str() == "-" => {
                        read_stdin_key(&mut std::io::stdin().lock())?
                    }
                    file => read_password(file.as_ref(), "")?,
                },
            };
            let info = client
                .put("/subscription", &json!({ "key": key.trim() }))
                .await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Remove => {
            client.delete("/subscription").await?;
            writeln!(out, "subscription removed")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum TelemetryCmd {
    /// 텔레메트리 설정과 마지막 전송을 보인다.
    Show,
    /// 텔레메트리를 켠다. 무엇을 보내는지는 `preview`로 먼저 본다.
    Enable {
        /// 보낼 `https://` 주소. 없으면 지금 끝점(설정 또는 빌드 기본값)
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// 텔레메트리를 끈다(id는 남는다).
    Disable,
    /// 지금 보낼 보고를 JSON 그대로 보인다(admin).
    Preview,
    /// 보고에 붙는 id를 새로 만든다.
    ResetId,
}

/// `telemetry enable|disable`의 요청 본문(`PATCH /settings`)
fn telemetry_body(enabled: bool, endpoint: Option<&str>) -> Value {
    let mut body = json!({ "telemetry": { "enabled": enabled } });
    if let Some(endpoint) = endpoint {
        body["telemetry"]["endpoint"] = json!(endpoint);
    }
    body
}

/// 설정 리소스의 텔레메트리 부분을 `키: 값` 줄로. 끝점이 없으면 이 oplctl 빌드의 기본 끝점을 보인다.
fn telemetry_lines(settings: &Value) -> Vec<String> {
    let spec = &settings["spec"]["telemetry"];
    let status = &settings["status"]["telemetry"];
    let endpoint = match spec["endpoint"].as_str() {
        Some(endpoint) => endpoint.to_string(),
        None if !opl_types::version::TELEMETRY_URL.is_empty() => {
            format!("{} (build default)", opl_types::version::TELEMETRY_URL)
        }
        None => "none".into(),
    };
    let last_sent = match status["last_sent_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    vec![
        format!("enabled: {}", spec["enabled"] == true),
        format!("endpoint: {endpoint}"),
        format!("id: {}", status["id"].as_str().unwrap_or("none")),
        format!("last sent: {last_sent}"),
        format!(
            "last error: {}",
            status["last_error"].as_str().unwrap_or("none")
        ),
    ]
}

async fn telemetry(
    client: &Client,
    cmd: TelemetryCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let settings = match cmd {
        TelemetryCmd::Show => client.get("/settings").await?,
        TelemetryCmd::Enable { endpoint } => {
            client
                .patch("/settings", &telemetry_body(true, endpoint.as_deref()))
                .await?
        }
        TelemetryCmd::Disable => {
            client
                .patch("/settings", &telemetry_body(false, None))
                .await?
        }
        TelemetryCmd::Preview => {
            // 보낼 JSON 그대로 보인다.
            let report = client.get("/telemetry/preview").await?;
            print_json(out, &report)?;
            return Ok(());
        }
        TelemetryCmd::ResetId => client.post("/telemetry/reset-id", &json!({})).await?,
    };
    if json_out {
        print_json(out, &settings)?;
    } else {
        for line in telemetry_lines(&settings) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// 원격 클러스터들과 마지막으로 모은 상태(이름, 클러스터, 버전, 노드 online/total, VM running/total)
    List,
    /// 원격 하나의 설정과 상태를 보인다(토큰은 id만).
    Show { name: String },
    /// 원격을 등록한다(admin). 서버가 그 클러스터에 연결해 CA 지문과 토큰을 확인한다.
    Add(RemoteAdd),
    /// 원격을 바꾼다(admin). 준 것만 바꾼다. 주소나 지문을 바꾸면 토큰도 다시 준다.
    Set(RemoteSet),
    /// 원격을 지운다(admin). 원격 클러스터의 토큰은 그대로 남는다.
    Remove { name: String },
    /// 원격 상태를 곧 다시 모아 보인다(admin).
    Refresh { name: String },
    /// 원격 클러스터의 VM 목록(이 클러스터를 거쳐 읽는다)
    Vms { name: String },
    /// 원격 클러스터의 VM을 시작하거나(start) 정지한다(stop — 로컬 `vm stop`처럼 ACPI, `--force`면
    /// 곧바로 끄기). 원격 쪽 권한은 토큰 주인의 역할이 정한다. Task를 기다리지 않는다.
    Vm {
        name: String,
        #[arg(value_enum)]
        action: RemoteVmAction,
        vm: String,
        /// stop에서 ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct RemoteAdd {
    name: String,
    /// 원격 클러스터 노드의 API 주소(`https://<호스트>[:포트]`)
    #[arg(long)]
    url: String,
    /// 원격 클러스터 CA의 지문(`sha256:...`, 원격 노드의 `oplctl cluster status`). 없으면 CA를 받아
    /// 지문을 보이고 확인을 묻는다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 원격 클러스터에서 만든 API 토큰이 든 파일(`-`면 표준 입력 — 그때는 확인을 물을 수 없으므로
    /// `--ca-fingerprint`나 `--yes`가 필요하다)
    #[arg(long)]
    token_file: PathBuf,
    /// 받은 CA 지문을 묻지 않고 믿는다.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct RemoteSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 새 API 토큰이 든 파일(`-`면 표준 입력). 주지 않으면 저장된 토큰을 그대로 둔다.
    #[arg(long)]
    token_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum RemoteVmAction {
    Start,
    Stop,
}

/// 원격 VM 동작이 부르는 원격 API의 동작(`/vms/<VM>/actions/<동작>`)과 보일 이름
fn remote_vm_action(
    action: RemoteVmAction,
    force: bool,
) -> anyhow::Result<(&'static str, &'static str)> {
    Ok(match (action, force) {
        (RemoteVmAction::Start, false) => ("start", "start"),
        (RemoteVmAction::Start, true) => bail!("--force is only for stop"),
        (RemoteVmAction::Stop, false) => ("stop", "stop"),
        (RemoteVmAction::Stop, true) => ("stop?mode=force", "force stop"),
    })
}

/// 토큰 파일(`-`면 표준 입력)의 토큰. 앞뒤 공백을 뗀다.
fn read_remote_token(file: &PathBuf) -> anyhow::Result<String> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    Ok(token)
}

/// 받은 CA 지문을 보이고 확인을 받는다. `--yes`면 묻지 않고, 터미널이 아니면 묻지 못하므로
/// `--ca-fingerprint`로 다시 실행하게 한다.
fn confirm_fingerprint(
    actual: &str,
    yes: bool,
    terminal: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    writeln!(out, "the remote cluster CA fingerprint is {actual}")?;
    if yes {
        return Ok(());
    }
    if !terminal {
        bail!(
            "check the fingerprint (`oplctl cluster status` on a node of the remote cluster) and \
             run again with --ca-fingerprint {actual}"
        );
    }
    write!(out, "Is this the CA of the remote cluster? [y/N] ")?;
    out.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        bail!("cancelled");
    }
    Ok(())
}

/// `<a>/<b>` 모양의 수(`online/total` 등). 모르면 `-`.
fn remote_counts(v: &Value, part: &str) -> String {
    if v.is_null() {
        return "-".into();
    }
    format!("{}/{}", cell(&v[part]), cell(&v["total"]))
}

/// 원격의 상태 칸: 닿으면 `reachable`, 아니면 `unreachable: <이유>`
fn remote_state(status: &Value) -> String {
    if status["reachable"] == true {
        "reachable".into()
    } else {
        format!("unreachable: {}", cell(&status["error"]))
    }
}

/// `remote list`의 열
const REMOTE_HEADERS: &[&str] = &["NAME", "CLUSTER", "VERSION", "NODES", "VMS", "STATUS"];

/// `remote list`의 한 행
fn remote_row(r: &Value) -> Vec<String> {
    let s = &r["status"];
    vec![
        cell(&r["name"]),
        cell(&s["cluster_name"]),
        cell(&s["version"]),
        remote_counts(&s["nodes"], "online"),
        remote_counts(&s["vms"], "running"),
        remote_state(s),
    ]
}

/// `remote show`의 `키: 값` 줄
fn remote_lines(r: &Value) -> Vec<String> {
    let s = &r["status"];
    let checked = match s["checked_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    let memory = match s["memory_gib"].as_u64() {
        Some(gib) => format!("{gib} GiB"),
        None => "-".into(),
    };
    vec![
        format!("name: {}", cell(&r["name"])),
        format!("url: {}", cell(&r["url"])),
        format!("ca fingerprint: {}", cell(&r["ca_fingerprint"])),
        format!("token id: {}", cell(&r["token_id"])),
        format!("status: {}", remote_state(s)),
        format!("changed at: {checked}"),
        format!("cluster: {}", cell(&s["cluster_name"])),
        format!("version: {}", cell(&s["version"])),
        format!("nodes online: {}", remote_counts(&s["nodes"], "online")),
        format!("vms running: {}", remote_counts(&s["vms"], "running")),
        format!("cpus: {}", cell(&s["cpus"])),
        format!("memory: {memory}"),
    ]
}

/// `remote set`의 본문: 지금 원격에 준 것만 바꾼다. 토큰을 주지 않으면 빼서 저장된 토큰을 둔다.
fn remote_set_body(current: &Value, args: &RemoteSet, token: Option<String>) -> Value {
    let mut body = json!({
        "url": args.url.as_deref().map_or_else(|| current["url"].clone(), |u| json!(u)),
        "ca_fingerprint": args
            .ca_fingerprint
            .as_deref()
            .map_or_else(|| current["ca_fingerprint"].clone(), |f| json!(f)),
    });
    if let Some(token) = token {
        body["token"] = json!(token);
    }
    body
}

async fn remote(
    client: &Client,
    cmd: RemoteCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let show = |out: &mut (dyn Write + Send), r: &Value| -> anyhow::Result<()> {
        if json_out {
            print_json(out, r)?;
        } else {
            for line in remote_lines(r) {
                writeln!(out, "{line}")?;
            }
        }
        Ok(())
    };
    match cmd {
        RemoteCmd::List => {
            let list = client.get("/remotes").await?;
            print_list(out, json_out, &list, REMOTE_HEADERS, remote_row)?;
        }
        RemoteCmd::Show { name } => {
            show(out, &client.get(&format!("/remotes/{}", seg(&name))).await?)?
        }
        RemoteCmd::Add(args) => {
            // 표준 입력은 토큰을 읽느라 다 썼으므로 지문 확인을 물을 수 없다.
            if args.token_file.as_os_str() == "-" && args.ca_fingerprint.is_none() && !args.yes {
                bail!("reading the token from stdin needs --ca-fingerprint or --yes");
            }
            let token = read_remote_token(&args.token_file)?;
            let fingerprint = match args.ca_fingerprint {
                Some(f) => f,
                None => {
                    let actual = fingerprint(&fetch_ca(&args.url).await?)?;
                    use std::io::IsTerminal;
                    let stdin = std::io::stdin();
                    let terminal = stdin.is_terminal();
                    confirm_fingerprint(&actual, args.yes, terminal, &mut stdin.lock(), out)?;
                    actual
                }
            };
            let body = json!({
                "name": args.name,
                "url": args.url,
                "ca_fingerprint": fingerprint,
                "token": token,
            });
            let created = client.post("/remotes", &body).await?;
            writeln!(
                out,
                "remote {} added ({})",
                args.name,
                remote_state(&created["status"])
            )?;
        }
        RemoteCmd::Set(args) => {
            let token = args
                .token_file
                .as_ref()
                .map(read_remote_token)
                .transpose()?;
            let path = format!("/remotes/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&remote_set_body(&current, &args, token));
            client.send(r).await?;
            writeln!(out, "remote {} updated", args.name)?;
        }
        RemoteCmd::Remove { name } => {
            client.delete(&format!("/remotes/{}", seg(&name))).await?;
            writeln!(out, "remote {name} removed")?;
        }
        RemoteCmd::Refresh { name } => {
            let r = client
                .post(
                    &format!("/remotes/{}/actions/refresh", seg(&name)),
                    &json!({}),
                )
                .await?;
            show(out, &r)?;
        }
        RemoteCmd::Vms { name } => {
            let list = client
                .get(&format!("/remotes/{}/api/vms", seg(&name)))
                .await?;
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
        RemoteCmd::Vm {
            name,
            action,
            vm,
            force,
        } => {
            let (action, shown) = remote_vm_action(action, force)?;
            let task = client
                .send(client.request(
                    Method::POST,
                    // 동작은 이 파일이 정한 값이고 쿼리를 담으므로 인코딩하지 않는다.
                    &format!(
                        "/remotes/{}/api/vms/{}/actions/{}",
                        seg(&name),
                        seg(&vm),
                        action
                    ),
                ))
                .await?;
            writeln!(
                out,
                "vm {vm} on remote {name}: {shown} requested (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

#[derive(Args)]
struct AuditArgs {
    /// 이 시각부터: `30m`·`1h`·`7d`(지금에서 뺌)나 RFC 3339. 기본은 `--until`의 24시간 전
    #[arg(long)]
    since: Option<String>,
    /// 이 시각까지(RFC 3339). 기본은 지금
    #[arg(long)]
    until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    #[arg(long)]
    user: Option<String>,
    /// 노드 이름
    #[arg(long)]
    node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 든 문자열
    #[arg(long)]
    action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    #[arg(long)]
    limit: Option<u32>,
}

/// `--since`: 기간(`30m`·`1h`·`7d`, 초 `s`도)이면 지금(`now_ms`)에서 뺀 유닉스 밀리초, 아니면
/// 그대로(RFC 3339나 밀리초) 보내 서버가 푼다.
fn audit_since(s: &str, now_ms: u64) -> String {
    match audit_duration_ms(s) {
        Some(ms) => now_ms.saturating_sub(ms).to_string(),
        None => s.to_string(),
    }
}

/// `30m`·`1h`·`7d`(초 `s`도) 같은 기간의 밀리초. 기간이 아니면 None
fn audit_duration_ms(s: &str) -> Option<u64> {
    let unit = match s.chars().last()? {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let n = s[..s.len() - 1].parse::<u64>().ok()?;
    Some(n.saturating_mul(unit))
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn unix_ms_time(ms: u64) -> String {
    match time::OffsetDateTime::from_unix_timestamp((ms / 1000) as i64) {
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

/// `audit`의 표 한 줄
fn audit_row(e: &Value) -> Vec<String> {
    // 대상이 없으면 detail(예: `audit.dropped`의 수, 로그인의 `username=…`)
    let target = match (e["target"]["kind"].as_str(), e["target"]["key"].as_str()) {
        (Some(kind), Some(key)) => format!("{kind}/{key}"),
        _ => e["detail"].as_str().unwrap_or("-").to_string(),
    };
    vec![
        e["time"].as_u64().map_or_else(|| "-".into(), unix_ms_time),
        cell(&e["node"]),
        cell(&e["user"]),
        cell(&e["ip"]),
        cell(&e["action"]),
        target,
        cell(&e["status"]),
    ]
}

/// `GET /audit`를 부르고 표로 보인다. 읽지 못한 노드, 감사 로그를 끈 노드와 잘림은 표 뒤
/// `err`(stderr)에 알린다.
async fn audit(
    client: &Client,
    args: AuditArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    if args.until.is_some()
        && args
            .since
            .as_deref()
            .is_some_and(|s| audit_duration_ms(s).is_some())
    {
        bail!("a relative --since cannot be combined with --until; give a time");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let query: Vec<(&str, String)> = [
        ("since", args.since.map(|s| audit_since(&s, now_ms))),
        ("until", args.until),
        ("user", args.user),
        ("node", args.node),
        ("action", args.action),
        ("limit", args.limit.map(|n| n.to_string())),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();
    let mut r = client.request(Method::GET, "/audit");
    if !query.is_empty() {
        r = r.query(&query);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = resp["entries"]
        .as_array()
        .map(|a| a.iter().map(audit_row).collect())
        .unwrap_or_default();
    table(
        out,
        &["TIME", "NODE", "USER", "IP", "ACTION", "TARGET", "STATUS"],
        &rows,
    )?;
    let unreachable: Vec<&str> = resp["unreachable"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !unreachable.is_empty() {
        writeln!(
            err,
            "warning: no audit records from: {}",
            unreachable.join(", ")
        )?;
    }
    let disabled: Vec<&str> = resp["disabled"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !disabled.is_empty() {
        writeln!(
            err,
            "note: the audit log is disabled on: {}",
            disabled.join(", ")
        )?;
    }
    if resp["truncated"] == true {
        writeln!(
            err,
            "note: more entries match; narrow the range or raise --limit"
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
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

#[derive(Subcommand)]
enum RealmCmd {
    List,
    /// 영역 설정을 보인다(서비스 계정 비밀번호·클라이언트 비밀은 있는지만).
    Show {
        name: String,
    },
    /// LDAP 영역을 만든다.
    AddLdap(RealmAdd),
    /// LDAP 영역을 바꾼다. 준 것만 바꾼다.
    SetLdap(RealmSet),
    /// OIDC 영역을 만든다. IdP에는 `<노드 주소>/api/v1/auth/oidc/callback`을 리디렉트 URI로 등록한다.
    AddOidc(RealmAddOidc),
    /// OIDC 영역을 바꾼다. 준 것만 바꾼다.
    SetOidc(RealmSetOidc),
    /// 영역을 지운다. 그 영역 사용자들의 세션과 토큰은 멈춘다.
    Remove {
        name: String,
    },
    /// 영역을 시험한다(사용자를 만들지 않는다). LDAP은 사용자 이름과 비밀번호로 디렉터리를,
    /// OIDC는 입력 없이 IdP의 discovery 문서와 키를 확인한다.
    Test {
        name: String,
        /// 영역 안의 사용자 이름(`@영역` 없이). LDAP 영역에는 꼭 필요하고, OIDC 영역이면 무시한다(경고).
        #[arg(long)]
        user: Option<String>,
        /// 비밀번호가 든 파일(`-`면 표준 입력). 없으면 터미널에서 묻는다.
        #[arg(long, requires = "user")]
        password_file: Option<PathBuf>,
    },
}

#[derive(Args)]
struct RealmAdd {
    name: String,
    /// `ldap://host[:port]`(StartTLS) 또는 `ldaps://host[:port]`
    #[arg(long)]
    url: String,
    /// 사용자를 찾을 기준 DN
    #[arg(long)]
    base_dn: String,
    #[command(flatten)]
    ldap: LdapArgs,
}

#[derive(Args)]
struct RealmSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    base_dn: Option<String>,
    #[command(flatten)]
    ldap: LdapArgs,
    /// 저장된 서비스 계정 비밀번호를 지운다. 서비스 계정 DN이 남으면 안 된다(`--clear-bind-dn`).
    #[arg(long, conflicts_with = "bind_password_file")]
    clear_bind_password: bool,
    /// 서비스 계정 DN과 비밀번호를 지운다(익명 검색).
    #[arg(long, conflicts_with_all = ["bind_dn", "bind_password_file"])]
    clear_bind_dn: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

#[derive(Args)]
struct RealmAddOidc {
    name: String,
    /// IdP의 issuer(`https://...`). discovery 문서의 `issuer`와 글자 그대로 같아야 한다.
    #[arg(long)]
    issuer: String,
    /// IdP에 등록한 클라이언트 ID
    #[arg(long)]
    client_id: String,
    #[command(flatten)]
    oidc: OidcArgs,
}

#[derive(Args)]
struct RealmSetOidc {
    name: String,
    #[arg(long)]
    issuer: Option<String>,
    #[arg(long)]
    client_id: Option<String>,
    #[command(flatten)]
    oidc: OidcArgs,
    /// 저장된 클라이언트 비밀을 지운다(공개 클라이언트).
    #[arg(long, conflicts_with = "client_secret_file")]
    clear_client_secret: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

/// `realm add-oidc`와 `set-oidc`의 공통 선택
#[derive(Args)]
struct OidcArgs {
    /// 클라이언트 비밀이 든 파일(`-`면 표준 입력). 없으면 공개 클라이언트(PKCE만)
    #[arg(long)]
    client_secret_file: Option<PathBuf>,
    /// 요청할 scope(여러 번, `openid`가 있어야 함). set-oidc에서 주면 목록 전체를 바꾼다.
    #[arg(long = "scope", value_name = "SCOPE")]
    scopes: Vec<String>,
    /// 사용자 이름을 담은 클레임(기본 preferred_username)
    #[arg(long)]
    username_claim: Option<String>,
    /// 그룹(문자열 배열)을 담은 클레임(기본 groups)
    #[arg(long)]
    groups_claim: Option<String>,
    /// IdP 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// IdP 요청마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹 클레임 값을 가진 사용자의 역할(`<그룹>=<role>`, 여러 번). set-oidc에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "GROUP=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `realm add-ldap`과 `set-ldap`의 공통 선택
#[derive(Args)]
struct LdapArgs {
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[arg(long)]
    bind_dn: Option<String>,
    /// 서비스 계정 비밀번호가 든 파일(`-`면 표준 입력)
    #[arg(long)]
    bind_password_file: Option<PathBuf>,
    /// 사용자 찾기 필터. `{username}`이 로그인 이름으로 바뀐다.
    #[arg(long)]
    user_filter: Option<String>,
    /// 그룹 DN을 담은 사용자 속성(기본 memberOf)
    #[arg(long)]
    group_attr: Option<String>,
    /// 서버 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// 연결과 작업마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹에 든 사용자의 역할(`<그룹 DN>=<role>`, 여러 번). set-ldap에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "DN=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `--group`의 `<그룹 DN>=<role>`. DN에 `=`가 있으므로 마지막 `=`에서 나눈다.
fn parse_group_role(s: &str) -> Result<(String, Role), String> {
    let (group, role) = s
        .rsplit_once('=')
        .ok_or_else(|| format!("invalid group {s:?}: use <group DN>=<role>"))?;
    let role = Role::from_str(role, true)
        .map_err(|_| format!("invalid role {role:?} in {s:?}: use admin, operator or viewer"))?;
    if group.trim().is_empty() {
        return Err(format!("invalid group {s:?}: use <group DN>=<role>"));
    }
    Ok((group.to_string(), role))
}

/// 파일에서 읽은 영역 값들(서비스 계정 비밀번호, CA)
#[derive(Default)]
struct LdapFiles {
    bind_password: Option<String>,
    ca_pem: Option<String>,
}

impl LdapFiles {
    fn read(args: &LdapArgs) -> anyhow::Result<Self> {
        Ok(Self {
            bind_password: args
                .bind_password_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// 비밀번호 파일(`-`면 표준 입력). 끝의 줄바꿈만 뗀다.
fn read_secret_file(path: &PathBuf) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .context("cannot read standard input")?;
        return Ok(s.trim_end_matches(['\r', '\n']).to_string());
    }
    read_password(Some(path), "")
}

fn group_roles(groups: &[(String, Role)]) -> Value {
    groups
        .iter()
        .map(|(group, role)| json!({ "group": group, "role": value_name(*role) }))
        .collect()
}

/// `realm add-ldap`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_body(args: &RealmAdd, files: LdapFiles) -> Value {
    let l = &args.ldap;
    let mut backend = json!({ "kind": "ldap", "url": args.url, "base_dn": args.base_dn });
    let optional = [
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("bind_password", files.bind_password.map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&l.groups, l.default_role)
    });
    if let Some(display) = &l.display {
        body["display"] = json!(display);
    }
    body
}

/// 만들 영역의 그룹 → 역할 표와 기본 역할
fn new_roles(groups: &[(String, Role)], default: Option<Role>) -> Value {
    let mut roles = json!({ "groups": group_roles(groups) });
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    roles
}

/// 바꿀 영역의 역할: 지금 표(`current`)에 준 것만 바꾼다.
fn changed_roles(
    current: &Value,
    groups: &[(String, Role)],
    default: Option<Role>,
    clear_groups: bool,
    clear_default_role: bool,
) -> Value {
    let mut roles = current.clone();
    if !groups.is_empty() {
        roles["groups"] = group_roles(groups);
    }
    if clear_groups {
        roles["groups"] = json!([]);
    }
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    if clear_default_role && let Some(r) = roles.as_object_mut() {
        r.remove("default");
    }
    roles
}

/// `set-ldap`·`set-oidc`가 바꿀 영역의 종류가 맞는지
fn ensure_kind(current: &Value, name: &str, kind: &str) -> anyhow::Result<()> {
    if current["backend"]["kind"] != kind {
        anyhow::bail!(
            "realm {name} is not an {} realm (its kind is {})",
            kind.to_uppercase(),
            cell(&current["backend"]["kind"])
        );
    }
    Ok(())
}

/// 파일에서 읽은 OIDC 영역 값들(클라이언트 비밀, CA)
#[derive(Default)]
struct OidcFiles {
    client_secret: Option<String>,
    ca_pem: Option<String>,
}

impl OidcFiles {
    fn read(args: &OidcArgs) -> anyhow::Result<Self> {
        Ok(Self {
            client_secret: args
                .client_secret_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// `realm add-oidc`·`set-oidc`가 준 선택 값들(이름과 값)
fn oidc_options(o: &OidcArgs, files: OidcFiles) -> [(&'static str, Option<Value>); 6] {
    [
        ("client_secret", files.client_secret.map(Value::from)),
        (
            "scopes",
            (!o.scopes.is_empty()).then(|| Value::from(o.scopes.clone())),
        ),
        ("username_claim", o.username_claim.clone().map(Value::from)),
        ("groups_claim", o.groups_claim.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", o.timeout.map(Value::from)),
    ]
}

/// `realm add-oidc`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_oidc_body(args: &RealmAddOidc, files: OidcFiles) -> Value {
    let o = &args.oidc;
    let mut backend = json!({ "kind": "oidc", "issuer": args.issuer, "client_id": args.client_id });
    for (key, value) in oidc_options(o, files) {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&o.groups, o.default_role)
    });
    if let Some(display) = &o.display {
        body["display"] = json!(display);
    }
    body
}

/// `realm set-oidc`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 클라이언트 비밀은 주지 않으면 빼서(서버가 저장된 값을 둔다 — issuer·client_id·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_oidc_body(
    current: &Value,
    args: &RealmSetOidc,
    files: OidcFiles,
) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "oidc")?;
    let o = &args.oidc;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_client_secret");
    }
    let mut changes = vec![
        ("issuer", args.issuer.clone().map(Value::from)),
        ("client_id", args.client_id.clone().map(Value::from)),
    ];
    changes.extend(oidc_options(o, files));
    if args.clear_client_secret {
        changes.push(("client_secret", Some(json!(""))));
    }
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if args.clear_ca
        && let Some(b) = backend.as_object_mut()
    {
        b.remove("ca_pem");
    }
    let roles = changed_roles(
        &current["roles"],
        &o.groups,
        o.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = o
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// `realm set-ldap`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 서비스 계정 비밀번호는 주지 않으면 빼서(서버가 저장된 값을 둔다 — url·bind_dn·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_body(current: &Value, args: &RealmSet, files: LdapFiles) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "ldap")?;
    let l = &args.ldap;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_bind_password");
    }
    let changes = [
        ("url", args.url.clone().map(Value::from)),
        ("base_dn", args.base_dn.clone().map(Value::from)),
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
        (
            "bind_password",
            if args.clear_bind_password || args.clear_bind_dn {
                Some(json!(""))
            } else {
                files.bind_password.map(Value::from)
            },
        ),
    ];
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if let Some(b) = backend.as_object_mut() {
        if args.clear_bind_dn {
            b.remove("bind_dn");
        }
        if args.clear_ca {
            b.remove("ca_pem");
        }
        // 서비스 계정 DN에는 비밀번호가 있어야 한다.
        if args.clear_bind_password && b.get("bind_dn").is_some_and(Value::is_string) {
            anyhow::bail!(
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
    }
    let roles = changed_roles(
        &current["roles"],
        &l.groups,
        l.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = l
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// OIDC 영역의 콜백 경로. IdP에는 노드 주소 뒤에 붙여 리디렉트 URI로 등록한다.
const OIDC_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";

/// `realm show`의 `키: 값` 줄
fn realm_lines(realm: &Value) -> Vec<String> {
    let b = &realm["backend"];
    let set = |v: bool| if v { "set" } else { "none" };
    let ca = if b["ca_pem"].is_string() {
        "custom (PEM)"
    } else {
        "system roots"
    };
    let mut lines = vec![
        format!("name: {}", cell(&realm["name"])),
        format!("display: {}", cell(&realm["display"])),
        format!("kind: {}", cell(&b["kind"])),
    ];
    if b["kind"] == "oidc" {
        let scopes: Vec<String> = b["scopes"]
            .as_array()
            .map(|a| a.iter().map(cell).collect())
            .unwrap_or_default();
        lines.extend([
            format!("issuer: {}", cell(&b["issuer"])),
            format!("client id: {}", cell(&b["client_id"])),
            format!("client secret: {}", set(b["has_client_secret"] == true)),
            format!("scopes: {}", scopes.join(" ")),
            format!("username claim: {}", cell(&b["username_claim"])),
            format!("groups claim: {}", cell(&b["groups_claim"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
            // IdP에는 노드 주소 뒤에 이 경로를 붙여 리디렉트 URI로 등록한다.
            format!("callback path: {OIDC_CALLBACK_PATH}"),
        ]);
    } else {
        lines.extend([
            format!("url: {}", cell(&b["url"])),
            format!("base dn: {}", cell(&b["base_dn"])),
            format!("bind dn: {}", cell(&b["bind_dn"])),
            format!("bind password: {}", set(b["has_bind_password"] == true)),
            format!("user filter: {}", cell(&b["user_filter"])),
            format!("group attribute: {}", cell(&b["group_attr"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
        ]);
    }
    let groups = realm["roles"]["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if groups.is_empty() {
        lines.push("groups: none".into());
    } else {
        lines.push("groups:".into());
        for g in &groups {
            lines.push(format!("  {} = {}", cell(&g["group"]), cell(&g["role"])));
        }
    }
    lines.push(format!(
        "default role: {}",
        realm["roles"]["default"].as_str().unwrap_or("none")
    ));
    lines
}

async fn realm(
    client: &Client,
    cmd: RealmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        RealmCmd::List => {
            let list = client.get("/realms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "DISPLAY", "KIND", "URL"],
                |r| {
                    vec![
                        cell(&r["name"]),
                        cell(&r["display"]),
                        cell(&r["backend"]["kind"]),
                        // OIDC 영역은 issuer
                        cell(if r["backend"]["kind"] == "oidc" {
                            &r["backend"]["issuer"]
                        } else {
                            &r["backend"]["url"]
                        }),
                    ]
                },
            )?;
        }
        RealmCmd::Show { name } => {
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            if json_out {
                print_json(out, &realm)?;
            } else {
                for line in realm_lines(&realm) {
                    writeln!(out, "{line}")?;
                }
            }
        }
        RealmCmd::AddLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            client
                .post("/realms", &realm_add_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::AddOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            client
                .post("/realms", &realm_add_oidc_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_oidc_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::Remove { name } => {
            client.delete(&format!("/realms/{}", seg(&name))).await?;
            writeln!(out, "realm {name} removed")?;
        }
        RealmCmd::Test {
            name,
            user,
            password_file,
        } => {
            // OIDC 영역은 입력 없이 시험한다(LDAP 영역이면 서버가 사용자를 달라고 한다).
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            let kind = realm["backend"]["kind"].as_str().unwrap_or_default();
            if kind == "oidc" && user.is_some() {
                eprintln!("warning: --user is ignored for an OIDC realm");
            }
            let body = match &user {
                Some(user) if kind != "oidc" => {
                    let password = match &password_file {
                        Some(f) => read_secret_file(f)?,
                        None => read_password(None, "Password: ")?,
                    };
                    json!({ "username": user, "password": password })
                }
                _ => json!({}),
            };
            let result = client
                .post(&format!("/realms/{}/actions/test", seg(&name)), &body)
                .await?;
            let steps = result["steps"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &result)?;
            } else {
                let rows: Vec<Vec<String>> = steps
                    .iter()
                    .map(|s| {
                        let ok = if s["ok"] == true { "ok" } else { "failed" };
                        vec![cell(&s["name"]), ok.into(), cell(&s["message"])]
                    })
                    .collect();
                table(out, &["STEP", "RESULT", "MESSAGE"], &rows)?;
            }
            // 사용자 결과(DN·그룹·역할)는 LDAP 시험에만 있다.
            if !json_out && kind == "ldap" {
                writeln!(out, "dn: {}", cell(&result["dn"]))?;
                let groups: Vec<String> = result["groups"]
                    .as_array()
                    .map(|a| a.iter().map(cell).collect())
                    .unwrap_or_default();
                writeln!(
                    out,
                    "groups: {}",
                    if groups.is_empty() {
                        "-".into()
                    } else {
                        groups.join(", ")
                    }
                )?;
                writeln!(out, "role: {}", result["role"].as_str().unwrap_or("none"))?;
            }
            if let Some(failed) = steps.iter().find(|s| s["ok"] != true) {
                bail!("realm {name}: the {} step failed", cell(&failed["name"]));
            }
        }
    }
    Ok(())
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

/// 표준 입력의 구독 키(앞뒤 공백을 다듬음). 키를 명령줄에 두지 않으려고 쓴다.
fn read_stdin_key(input: &mut dyn std::io::Read) -> anyhow::Result<String> {
    let mut key = String::new();
    input
        .read_to_string(&mut key)
        .context("cannot read the key from standard input")?;
    Ok(key.trim().to_string())
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{}", seg(pool))).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
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
        let (token, name) = match &args.token_file {
            Some(file) => token_login(&server, file, ca_pem.clone()).await?,
            None => login(&server, args, ca_pem.clone()).await?,
        };
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        // 새 토큰과 같으면 두고, 이 CLI의 login이 만든 토큰(`oplctl-…`)만 지운다(`--token-file`로 넣은
        // 토큰은 사용자가 웹 UI에서 만든 것이다).
        if let Some((old, old_server)) = previous
            && old != token
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(old), ca_pem)?;
            remove_login_token(&client, id).await;
        }
        writeln!(out, "logged in to {server} as {name}")?;
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
            // login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 이미 지워진 토큰이어도 로컬에서는 지운다.
            let kept = match config.token.as_deref().and_then(token_id) {
                Some(id) => remove_login_token(&client, id).await == Some(false),
                None => false,
            };
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
            if kept {
                writeln!(
                    out,
                    "the token stays valid on the server; delete it in the web UI"
                )?;
            }
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{}", seg(&node))).await?)?
            }
            NodeCmd::Updates { node, refresh } => {
                node_updates(&client, &node, refresh, json_out, out).await?
            }
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
            NodeCmd::Maintenance { node, state } => {
                node_maintenance(
                    &client,
                    &node,
                    state == Switch::On,
                    out,
                    &mut std::io::stderr(),
                )
                .await?
            }
            NodeCmd::Drain { node, wait } => node_drain(&client, &node, wait, out).await?,
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client
                    .get(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(set) => {
                client
                    .patch(
                        &format!("/storage-pools/{}", seg(&set.pool)),
                        &pool_set_body(&set),
                    )
                    .await?;
                writeln!(out, "storage pool {}: {}", set.pool, pool_set_summary(&set))?;
            }
            PoolCmd::Delete { pool } => {
                client
                    .delete(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => print_json(
                out,
                &client.get(&format!("/networks/{}", seg(&network))).await?,
            )?,
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client
                    .delete(&format!("/networks/{}", seg(&network)))
                    .await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::SecurityGroup { cmd } => security_group(&client, cmd, json_out, out).await?,
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
            VolumeCmd::Get { volume } => print_json(
                out,
                &client.get(&format!("/volumes/{}", seg(&volume))).await?,
            )?,
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
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
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{}/resize", seg(&volume)), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{}", seg(&volume))).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Import { cmd } => import(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{}", seg(&task))).await?)?
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
                client.put(&format!("/users/{}", seg(&name)), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{}", seg(&name))).await?;
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
                client.delete(&format!("/tokens/{}", seg(&token))).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
        Cmd::Realm { cmd } => realm(&client, cmd, json_out, out).await?,
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
        Cmd::Remote { cmd } => remote(&client, cmd, json_out, out).await?,
    }
    Ok(())
}

async fn security_group(
    client: &Client,
    cmd: SecurityGroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SecurityGroupCmd::List => {
            let list = client.get("/security-groups").await?;
            print_list(
                out,
                json_out,
                &list,
                SECURITY_GROUP_HEADERS,
                security_group_row,
            )?;
        }
        SecurityGroupCmd::Get { group } => print_json(
            out,
            &client
                .get(&format!("/security-groups/{}", seg(&group)))
                .await?,
        )?,
        SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } => {
            let body = security_group_create_body(&name, description.as_deref(), &rules);
            client.post("/security-groups", &body).await?;
            writeln!(out, "security group {name} created")?;
        }
        SecurityGroupCmd::Delete { group } => {
            client
                .delete(&format!("/security-groups/{}", seg(&group)))
                .await?;
            writeln!(out, "security group {group} deleted")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Add { group, rule, at },
        } => {
            patch_rules(client, &group, |g| rules_with(g, rule, at)).await?;
            writeln!(out, "security group {group}: rule added")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Rm { group, n },
        } => {
            patch_rules(client, &group, |g| rules_without(g, n)).await?;
            writeln!(out, "security group {group}: rule {n} removed")?;
        }
    }
    Ok(())
}

async fn backup(
    client: &Client,
    cmd: BackupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupCmd::Create {
            vm,
            pool,
            name,
            incremental,
        } => backup_create(client, &vm, &pool, name, incremental, out).await?,
        BackupCmd::List { vm } => {
            let mut r = client.request(Method::GET, "/backups");
            if let Some(vm) = &vm {
                r = r.query(&[("vm", vm)]);
            }
            let list = client.send(r).await?;
            print_list(out, json_out, &list, BACKUP_HEADERS, backup_row)?;
        }
        BackupCmd::Get { backup } => {
            let mut b = client.get(&format!("/backups/{}", seg(&backup))).await?;
            if b["status"]["parent"].is_string() {
                b["chain"] = backup_chain(&b, &items(&client.get("/backups").await?));
            }
            print_json(out, &b)?
        }
        BackupCmd::Delete { backup, force } => backup_delete(client, &backup, force, out).await?,
        BackupCmd::Restore {
            backup,
            name,
            pool,
            node,
        } => {
            let body = json!({ "name": name, "pool": pool, "node": node });
            let task = client
                .post(&format!("/backups/{}/restore", seg(&backup)), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            writeln!(out, "vm {name} restored from backup {backup}")?;
        }
        BackupCmd::Replicate {
            backup,
            remote,
            pool,
            wait,
        } => backup_replicate(client, &backup, &remote, &pool, wait, out).await?,
    }
    Ok(())
}

/// `backup create`: 백업 Task를 기다린다. 증분을 청했으면 증분이 됐는지, 아니면 전체로 한 이유를 보인다.
async fn backup_create(
    client: &Client,
    vm: &str,
    pool: &str,
    name: Option<String>,
    incremental: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut body = json!({ "pool": pool, "name": name });
    // 청하지 않으면 보내지 않는다(옛 서버는 모르는 필드를 거절한다).
    if incremental {
        body["incremental"] = json!(true);
    }
    let task = client
        .post(&format!("/vms/{}/backups", seg(vm)), &body)
        .await?;
    client.wait_task_for(&task, BACKUP_WAIT).await?;
    // 이름을 주지 않았거나 증분을 청했으면 만든 백업을 읽는다.
    let backup = if name.is_none() || incremental {
        let id = cell(&task["spec"]["op"]["backup"]);
        client.get(&format!("/backups/{}", seg(&id))).await?
    } else {
        Value::Null
    };
    let name = name.unwrap_or_else(|| cell(&backup["name"]));
    let kind = if !incremental {
        String::new()
    } else if backup["status"]["parent"].is_string() {
        " (incremental)".into()
    } else {
        format!(" (full: {})", cell(&backup["status"]["full_reason"]))
    };
    writeln!(out, "backup {name} of vm {vm} created{kind}")?;
    Ok(())
}

/// 증분 백업의 체인: 전체 백업부터 이 백업까지의 이름과 그 파일 크기의 합. `backups`는 백업 목록이고,
/// 거기 없는 조상은 id로 적고 거기서 멈춘다(이미 본 백업이 다시 나와도 멈춘다). 체인을 끝까지 따라가지
/// 못했거나 다 만들지 않은(`ready`가 아닌) 백업이 있으면 크기는 모르므로(부분 합은 보이지 않는다) 이름만
/// 적는다.
fn backup_chain(backup: &Value, backups: &[Value]) -> Value {
    let stored = |b: &Value| -> u64 {
        b["status"]["disks"].as_array().map_or(0, |disks| {
            disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum()
        })
    };
    let ready = |b: &Value| b["status"]["phase"] == "ready";
    let mut names = vec![cell(&backup["name"])];
    let mut total = stored(backup);
    let mut sized = ready(backup);
    let mut seen = std::collections::HashSet::from([backup["id"].clone()]);
    let mut parent = backup["status"]["parent"].clone();
    while let Some(id) = parent.as_str() {
        if !seen.insert(parent.clone()) {
            sized = false;
            break;
        }
        let Some(b) = backups.iter().find(|b| b["id"] == id) else {
            names.push(id.to_string());
            sized = false;
            break;
        };
        names.push(cell(&b["name"]));
        total += stored(b);
        sized &= ready(b);
        parent = b["status"]["parent"].clone();
    }
    names.reverse();
    if sized {
        json!({ "backups": names, "stored_bytes": total })
    } else {
        json!({ "backups": names })
    }
}

/// `backup replicate`: 복제 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn backup_replicate(
    client: &Client,
    backup: &str,
    remote: &str,
    pool: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(
            &format!("/backups/{}/actions/replicate", seg(backup)),
            &json!({ "remote": remote, "pool": pool }),
        )
        .await?;
    if wait {
        client.wait_task_for(&task, BACKUP_WAIT).await?;
        writeln!(
            out,
            "backup {backup} replicated to pool {pool} of remote {remote}"
        )?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "replication of backup {backup} to remote {remote} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
    }
    Ok(())
}

/// 백업을 지운다. `force`로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
async fn backup_delete(
    client: &Client,
    backup: &str,
    force: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut r = client.request(Method::DELETE, &format!("/backups/{}", seg(backup)));
    if force {
        r = r.query(&[("force", "true")]);
    }
    let got = client.send(r).await?;
    if let Some(message) = got["message"].as_str() {
        writeln!(out, "{message}")?;
    } else {
        let task = client.wait_task(&got).await?;
        writeln!(out, "backup {backup} deleted")?;
        // 원격 사본을 지우지 못했으면 성공한 Task에 경고가 남는다.
        if let Some(warning) = task["status"]["error"].as_str() {
            writeln!(out, "warning: {warning}")?;
        }
    }
    Ok(())
}

/// `backup list`의 열
const BACKUP_HEADERS: &[&str] = &[
    "NAME", "VM", "POOL", "PHASE", "CREATED", "SIZE", "TYPE", "ORIGIN", "REPLICAS",
];

/// `backup list`의 한 행. VM은 만든 때의 이름이고(VM을 지웠어도 남는다), SIZE는 백업 파일 크기의 합이다
/// (증분 백업은 증분 파일만). TYPE은 부모가 있으면 `incr`, 아니면 `full`이다. ORIGIN은 원격에서 받은
/// 사본의 `<보낸 클러스터>/<원본 id 앞 8자>`, REPLICAS는 원격 사본마다 `<원격>:<상태>`다.
fn backup_row(b: &Value) -> Vec<String> {
    let status = &b["status"];
    let created = status["created_at"]
        .as_i64()
        .map_or_else(|| "-".to_string(), unix_time);
    let size = match status["disks"].as_array() {
        Some(disks) if status["phase"] == "ready" => bytes(
            &disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum::<u64>()
                .into(),
        ),
        _ => "-".into(),
    };
    let origin = &b["spec"]["origin"];
    let origin = match origin["backup"].as_str() {
        Some(id) => format!("{}/{}", cell(&origin["cluster"]), &id[..id.len().min(8)]),
        None => "-".into(),
    };
    let replicas = match status["replicas"].as_array() {
        Some(r) if !r.is_empty() => r
            .iter()
            .map(|r| format!("{}:{}", cell(&r["remote"]), cell(&r["state"])))
            .collect::<Vec<_>>()
            .join(","),
        _ => "-".into(),
    };
    vec![
        cell(&b["name"]),
        cell(&b["spec"]["vm_name"]),
        cell(&b["spec"]["pool"]),
        cell(&status["phase"]),
        created,
        size,
        if status["parent"].is_string() {
            "incr"
        } else {
            "full"
        }
        .into(),
        origin,
        replicas,
    ]
}

async fn backup_job(
    client: &Client,
    cmd: BackupJobCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupJobCmd::Create(args) => {
            let job = client
                .post("/backup-jobs", &backup_job_create_body(&args))
                .await?;
            if args.disabled {
                writeln!(out, "backup job {} created (disabled)", args.name)?;
            } else {
                let next = job["status"]["next_run"]
                    .as_i64()
                    .map_or_else(|| "-".to_string(), unix_time);
                writeln!(out, "backup job {} created; next run {next}", args.name)?;
            }
        }
        BackupJobCmd::List => {
            let list = client.get("/backup-jobs").await?;
            print_list(out, json_out, &list, BACKUP_JOB_HEADERS, backup_job_row)?;
        }
        BackupJobCmd::Get { job } => print_json(
            out,
            &client.get(&format!("/backup-jobs/{}", seg(&job))).await?,
        )?,
        BackupJobCmd::Set(args) => {
            // If-Match 없이 보낸다: 스케줄러가 status를 자주 써서 revision이 곧잘 바뀐다.
            let path = format!("/backup-jobs/{}", seg(&args.job));
            let current = client.get(&path).await?;
            client
                .patch(&path, &backup_job_set_body(&args, &current))
                .await?;
            writeln!(out, "backup job {} updated", args.job)?;
        }
        BackupJobCmd::Delete { job } => {
            client
                .delete(&format!("/backup-jobs/{}", seg(&job)))
                .await?;
            writeln!(out, "backup job {job} deleted; its backups are kept")?;
        }
        BackupJobCmd::Run { job } => {
            client
                .send(client.request(Method::POST, &format!("/backup-jobs/{}/run", seg(&job))))
                .await?;
            writeln!(
                out,
                "backup job {job} will run at the scheduler's next check"
            )?;
        }
    }
    Ok(())
}

/// `backup-job list`의 열
const BACKUP_JOB_HEADERS: &[&str] = &[
    "NAME", "POOL", "VMS", "SCHEDULE", "KEEP", "ENABLED", "NEXT", "LAST",
];

/// `backup-job list`의 한 행. 시각은 모두 UTC다. 끈 작업은 다음 실행이 없다.
fn backup_job_row(j: &Value) -> Vec<String> {
    let spec = &j["spec"];
    let status = &j["status"];
    let vms = &spec["vms"];
    let vms = if vms["all"] == true {
        "all".to_string()
    } else if let Some(names) = vms["names"].as_array() {
        names.iter().map(cell).collect::<Vec<_>>().join(",")
    } else if let Some(labels) = vms["labels"].as_object() {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "-".to_string()
    };
    let days = match spec["schedule"]["days"].as_array() {
        Some(days) if !days.is_empty() => days.iter().map(cell).collect::<Vec<_>>().join(","),
        _ => "daily".to_string(),
    };
    let mut schedule = format!("{days} {} UTC", cell(&spec["schedule"]["at"]));
    if let Some(n) = spec["full_every"].as_u64().filter(|n| *n > 0) {
        schedule.push_str(&format!(", full every {n}"));
    }
    let keep: Vec<String> = ["last", "daily", "weekly", "monthly"]
        .iter()
        .filter_map(|unit| match spec["keep"][unit].as_u64() {
            Some(n) if n > 0 => Some(format!("{unit}={n}")),
            _ => None,
        })
        .collect();
    let keep = if keep.is_empty() {
        "all".to_string()
    } else {
        keep.join(",")
    };
    let enabled = spec["enabled"] != false;
    let next = match status["next_run"].as_i64() {
        Some(t) if enabled => unix_time(t),
        _ => "-".to_string(),
    };
    let last = if !status["run"].is_null() {
        "running".to_string()
    } else {
        let r = &status["last_run"];
        match r["finished_at"].as_i64() {
            Some(t) => format!(
                "{} ({} ok, {} failed)",
                unix_time(t),
                cell(&r["ok"]),
                cell(&r["failed"])
            ),
            None => "-".to_string(),
        }
    };
    vec![
        cell(&j["name"]),
        cell(&spec["pool"]),
        vms,
        schedule,
        keep,
        enabled.to_string(),
        next,
        last,
    ]
}

async fn import(
    client: &Client,
    cmd: ImportCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ImportCmd::Ova { file, name } => {
            // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
            let size = tokio::fs::metadata(&file)
                .await
                .with_context(|| format!("cannot read {}", file.display()))?
                .len()
                .to_string();
            client
                .send(
                    client
                        .request(Method::POST, "/vm-imports/ova/check")
                        .query(&[("name", name.as_str()), ("size", size.as_str())]),
                )
                .await?;
            let file_name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{name}.ova"));
            let query = [("name", name.as_str()), ("file_name", file_name.as_str())];
            client.upload("/vm-imports/ova", &query, &file).await?;
            writeln!(
                out,
                "vm-import {name} uploaded; review it with `oplctl import get {name}`"
            )?;
        }
        ImportCmd::EsxiBrowse { conn, fingerprint } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, fingerprint.as_deref(), &password);
            esxi_browse(client, &connection, json_out, out).await?;
        }
        ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, Some(&fingerprint), &password);
            let body = esxi_import_body(&name, &connection, &datastore, &vmx);
            esxi_create(client, &body, out).await?;
        }
        ImportCmd::List => {
            let list = client.get("/vm-imports").await?;
            let (nodes, vms) = if json_out {
                (Vec::new(), Vec::new())
            } else {
                (
                    items(&client.get("/nodes").await?),
                    items(&client.get("/vms").await?),
                )
            };
            print_list(out, json_out, &list, IMPORT_HEADERS, |i| {
                import_row(i, &nodes, &vms)
            })?;
        }
        ImportCmd::Get { import } => {
            let import = client.get(&format!("/vm-imports/{}", seg(&import))).await?;
            if json_out {
                print_json(out, &import)?;
            } else {
                let nodes = items(&client.get("/nodes").await?);
                let vms = if import["status"]["vm"].is_null() {
                    Vec::new()
                } else {
                    items(&client.get("/vms").await?)
                };
                print_import(out, &import, &nodes, &vms)?;
            }
        }
        ImportCmd::Run(args) => {
            // `=` 없는 --net이 있을 때만 원본 네트워크를 읽는다.
            let sources = if args.nets.iter().any(|n| n.source.is_none()) {
                source_networks(
                    &client
                        .get(&format!("/vm-imports/{}", seg(&args.import)))
                        .await?,
                )
            } else {
                Vec::new()
            };
            let body = import_run_body(&args, &sources)?;
            let task = client
                .post(&format!("/vm-imports/{}/run", seg(&args.import)), &body)
                .await?;
            client.wait_task_for(&task, IMPORT_WAIT).await?;
            writeln!(out, "vm {} imported", args.name)?;
        }
        ImportCmd::Delete { import, force } => {
            let mut r = client.request(Method::DELETE, &format!("/vm-imports/{}", seg(&import)));
            if force {
                r = r.query(&[("force", "true")]);
            }
            let got = client.send(r).await?;
            // force로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
            if let Some(message) = got["message"].as_str() {
                writeln!(out, "{message}")?;
            } else {
                client.wait_task(&got).await?;
                writeln!(out, "vm-import {import} deleted")?;
            }
        }
    }
    Ok(())
}

/// ESXi 연결 오류. 믿을 지문이 없으면 서버 인증서의 지문을 확인하고 다시 하라고 알린다.
fn esxi_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.code == "untrusted_certificate" => {
            let fingerprint = cell(&api.details["fingerprint"]);
            anyhow::anyhow!(
                "the ESXi certificate fingerprint is {fingerprint}; check it and rerun with --fingerprint {fingerprint}"
            )
        }
        _ => e,
    }
}

/// `import esxi-browse`: 데이터스토어마다 VM 표
async fn esxi_browse(
    client: &Client,
    connection: &Value,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let got = client
        .post("/vm-imports/esxi/browse", connection)
        .await
        .map_err(esxi_error)?;
    if json_out {
        return Ok(print_json(out, &got)?);
    }
    let mut rows = Vec::new();
    for ds in got["datastores"].as_array().into_iter().flatten() {
        for vm in ds["vms"].as_array().into_iter().flatten() {
            rows.push(esxi_vm_row(&ds["name"], vm));
        }
    }
    table(out, ESXI_VM_HEADERS, &rows)?;
    Ok(())
}

/// `import esxi-browse`의 열
const ESXI_VM_HEADERS: &[&str] = &[
    "DATASTORE",
    "VMX",
    "NAME",
    "GUEST",
    "CPU",
    "MEMORY",
    "DISKS",
    "LOCKED",
];

/// `import esxi-browse`의 한 행. `.vmx`를 읽지 못한 VM은 NAME 자리에 오류를 보인다.
fn esxi_vm_row(datastore: &Value, vm: &Value) -> Vec<String> {
    let (name, guest, cpu, memory, disks) = match vm["error"].as_str() {
        Some(error) => (
            format!("error: {error}"),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ),
        None => (
            cell(&vm["name"]),
            cell(&vm["guest_os"]),
            cell(&vm["vcpus"]),
            bytes(&vm["memory_mib"].as_u64().map(|m| m << 20).into()),
            cell(&vm["disks"]),
        ),
    };
    vec![
        cell(datastore),
        cell(&vm["vmx"]),
        name,
        guest,
        cpu,
        memory,
        disks,
        cell(&vm["locked"]),
    ]
}

/// `import esxi`: 가져오기를 만든다.
async fn esxi_create(
    client: &Client,
    body: &Value,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    client
        .post("/vm-imports/esxi", body)
        .await
        .map_err(esxi_error)?;
    writeln!(
        out,
        "vm-import {name} created; review it with `oplctl import get {name}`",
        name = cell(&body["name"])
    )?;
    Ok(())
}

/// ESXi 원본의 표시(`esxi://호스트/데이터스토어/.vmx`). OVA 원본이면 `None`이다.
fn esxi_source(i: &Value) -> Option<String> {
    let esxi = &i["spec"]["source"]["esxi"];
    esxi.is_object().then(|| {
        format!(
            "esxi://{}/{}/{}",
            cell(&esxi["host"]),
            cell(&esxi["datastore"]),
            cell(&esxi["vmx"])
        )
    })
}

/// 목록에서 id가 `id`인 항목의 이름. 없으면 id다.
fn name_of(list: &[Value], id: &Value) -> String {
    list.iter()
        .find(|v| v["id"] == *id)
        .map_or_else(|| cell(id), |v| cell(&v["name"]))
}

/// `import list`의 열
const IMPORT_HEADERS: &[&str] = &["NAME", "NODE", "SOURCE", "PHASE", "VM"];

/// `import list`의 한 행. SOURCE는 원본 VM 이름이고, 원본을 읽지 못했으면 올린 파일 이름이다.
/// ESXi 원본이면 `esxi://호스트/데이터스토어/.vmx`다.
fn import_row(i: &Value, nodes: &[Value], vms: &[Value]) -> Vec<String> {
    let status = &i["status"];
    let source = match (esxi_source(i), status["source_vm"]["name"].as_str()) {
        (Some(esxi), _) => esxi,
        (None, Some(name)) => name.to_string(),
        (None, None) => cell(&i["spec"]["source"]["ova"]["file_name"]),
    };
    vec![
        cell(&i["name"]),
        name_of(nodes, &i["spec"]["node"]),
        source,
        cell(&status["phase"]),
        name_of(vms, &status["vm"]),
    ]
}

/// `import get`: 머리 줄들, 원본 디스크와 NIC 표, 경고
fn print_import(
    out: &mut (dyn Write + Send),
    i: &Value,
    nodes: &[Value],
    vms: &[Value],
) -> anyhow::Result<()> {
    let ova = &i["spec"]["source"]["ova"];
    let status = &i["status"];
    writeln!(out, "vm-import: {}", cell(&i["name"]))?;
    writeln!(out, "node: {}", name_of(nodes, &i["spec"]["node"]))?;
    match esxi_source(i) {
        Some(esxi) => writeln!(out, "source: {esxi}")?,
        None => writeln!(
            out,
            "source: {} ({})",
            cell(&ova["file_name"]),
            bytes(&ova["size_bytes"])
        )?,
    }
    writeln!(out, "phase: {}", cell(&status["phase"]))?;
    if !status["vm"].is_null() {
        writeln!(out, "vm: {}", name_of(vms, &status["vm"]))?;
    }
    if let Some(error) = status["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let src = &status["source_vm"];
    if src.is_object() {
        writeln!(
            out,
            "source vm: {} ({}, {}, {} vCPUs, {} cores per socket, {} MiB)",
            cell(&src["name"]),
            cell(&src["guest_os"]),
            cell(&src["firmware"]),
            cell(&src["vcpus"]),
            cell(&src["cores_per_socket"]),
            cell(&src["memory_mib"])
        )?;
    }
    let p = &status["proposal"];
    if p.is_object() {
        writeln!(
            out,
            "proposal: {} sockets x {} cores, {} MiB, {}, {} disks, {} NICs",
            cell(&p["sockets"]),
            cell(&p["cores"]),
            cell(&p["memory_mib"]),
            cell(&p["firmware"]),
            cell(&p["disk_bus"]),
            cell(&p["nic_model"])
        )?;
    }
    if src.is_object() {
        let disks: Vec<_> = src["disks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                vec![
                    cell(&d["name"]),
                    cell(&d["file"]),
                    bytes(&d["capacity_bytes"]),
                    cell(&d["controller"]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["DISK", "FILE", "SIZE", "CONTROLLER"], &disks)?;
        // PROPOSED는 이름이 같아 제안한 opLite 네트워크다.
        let nics: Vec<_> = src["nics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                vec![
                    cell(&n["network"]),
                    cell(&n["adapter"]),
                    cell(&n["mac"]),
                    cell(&p["networks"][n["network"].as_str().unwrap_or_default()]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["NETWORK", "ADAPTER", "MAC", "PROPOSED"], &nics)?;
    }
    if let Some(warnings) = status["warnings"].as_array().filter(|w| !w.is_empty()) {
        writeln!(out, "\nwarnings:")?;
        for w in warnings {
            writeln!(out, "  {}", cell(w))?;
        }
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
            print_cluster_status(out, &view)?;
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
            client.delete(&format!("/nodes/{}", seg(&node))).await?;
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
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{}", seg(&vm))).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{}", seg(key))).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args.nics.iter().map(nic_body).collect();
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
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::NicSg { vm, index, groups } => {
            put_spec(client, &vm, |current| {
                nic_groups_update(current, index, &groups)
            })
            .await?;
            if groups.is_empty() {
                writeln!(out, "vm {vm}: NIC {index} has no security groups")?;
            } else {
                writeln!(
                    out,
                    "vm {vm}: NIC {index} security groups {}",
                    groups.join(", ")
                )?;
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
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(
                    &format!("/vms/{}/migrate", seg(&vm)),
                    &json!({ "node": node }),
                )
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                    let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{}/snapshots/{}", seg(&vm), seg(&cell(&s["id"])));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{}", seg(&vm))).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client
                .post(&format!("/vms/{}/clone", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{}", seg(&vm))).await?;
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

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 열
const VM_HEADERS: &[&str] = &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"];

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client
                .post(&format!("/vms/{}/snapshots", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current =
                client.get(&format!("/vms/{}", seg(&vm))).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{}/snapshots/{}/revert", seg(&vm), seg(&name));
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{}/snapshots/{}", seg(&vm), seg(&name)))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", seg(&args.vm));
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    put_spec(client, vm, |current| {
        Ok(spec_update(current, field, value.clone()))
    })
    .await
}

/// VM `vm`을 읽어 `body`(읽은 VM → 수정 본문)로 만든 본문을 그 revision으로(`If-Match`) 다시 쓴다.
async fn put_spec(
    client: &Client,
    vm: &str,
    body: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", seg(vm));
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body(&current)?);
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// VM 수정 본문: NIC `index`(0부터)의 보안 그룹만 `groups`로 바꾼다.
fn nic_groups_update(vm: &Value, index: usize, groups: &[String]) -> anyhow::Result<Value> {
    let mut nics = vm["spec"]["nics"].as_array().cloned().unwrap_or_default();
    let count = nics.len();
    let Some(nic) = nics.get_mut(index) else {
        bail!(
            "VM {} has no NIC {index} ({count} NICs, numbered from 0)",
            cell(&vm["name"])
        );
    };
    nic["security_groups"] = json!(groups);
    Ok(spec_update(vm, "nics", Value::Array(nics)))
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(
            Method::POST,
            // 동작은 이 파일이 정한 값이고 `stop?mode=force`처럼 쿼리를 담으므로 인코딩하지 않는다.
            &format!("/vms/{}/actions/{action}", seg(vm)),
        ))
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

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "VERSION",
    "UNDERLAY",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        // 옛 버전 노드는 기록하지 않는다.
        cell(&n["status"]["version"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
        cell(&n["status"]["underlay"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `network list`의 열
const NETWORK_HEADERS: &[&str] = &["NAME", "TYPE", "BRIDGE", "VLAN/VNI", "MTU"];

/// VXLAN 네트워크의 기본 MTU(서버의 `DEFAULT_VXLAN_MTU`)
const DEFAULT_VXLAN_MTU: u64 = 1450;

/// `network list`의 한 행. VXLAN 네트워크의 브리지는 opld가 만드는 `oplv<vni>`다.
fn network_row(n: &Value) -> Vec<String> {
    let spec = &n["spec"];
    match spec["vxlan"]["vni"].as_u64() {
        Some(vni) => vec![
            cell(&n["name"]),
            "vxlan".into(),
            format!("oplv{vni}"),
            vni.to_string(),
            spec["mtu"]
                .as_u64()
                .unwrap_or(DEFAULT_VXLAN_MTU)
                .to_string(),
        ],
        None => vec![
            cell(&n["name"]),
            "bridge".into(),
            cell(&spec["bridge"]),
            cell(&spec["vlan"]),
            "-".into(),
        ],
    }
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
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
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
    // 버전을 모르는 멤버(옛 버전 노드, 합류 중)는 세지 않는다.
    let mut versions: Vec<&str> = members
        .iter()
        .filter_map(|m| m["version"].as_str())
        .collect();
    versions.sort_by(|a, b| opl_types::version::compare(a, b));
    versions.dedup_by(|a, b| opl_types::version::compare(a, b).is_eq());
    if versions.len() > 1 {
        writeln!(
            out,
            "warning: nodes run different opLite versions ({}); finish the upgrade on every node",
            versions.join(", ")
        )?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    // 백업 풀만 적는다(옛 서버와 끈 풀은 보내지 않는다).
    if pool["spec"]["backups"] == true {
        text.push_str("backups: true\n");
    }
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
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

/// 이 CLI의 login이 만든 토큰(이름 `oplctl-…`)이면 지운다. 토큰 자신으로 읽고 지운다(실패는 무시).
/// 지웠으면 `Some(true)`, 다른 토큰(웹 UI에서 만든 것)이라 두었으면 `Some(false)`, 목록을 읽지 못했거나
/// 목록에 없으면(이미 지워짐) `None`.
async fn remove_login_token(client: &Client, id: uuid::Uuid) -> Option<bool> {
    let tokens = client.get("/tokens").await.ok()?;
    let token = tokens["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["id"].as_str() == Some(id.to_string().as_str()))?;
    let made_by_login = token["name"]
        .as_str()
        .is_some_and(|n| n.starts_with(LOGIN_TOKEN_PREFIX));
    if made_by_login {
        let _ = client.delete(&format!("/tokens/{id}")).await;
    }
    Some(made_by_login)
}

/// 웹 UI에서 만든 API 토큰으로 로그인한다: 그 토큰으로 `GET /auth/whoami`를 불러 확인하고 사용자 이름을
/// 얻는다. 토큰과 사용자 이름을 돌려준다.
async fn token_login(
    server: &str,
    file: &PathBuf,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    let client = Client::new(server, Some(token.clone()), ca_pem)?;
    let me = client
        .get("/auth/whoami")
        .await
        .context("the API token was not accepted")?;
    let name = me["name"]
        .as_str()
        .context("server did not return the user")?
        .to_string();
    Ok((token, name))
}

/// OIDC 영역은 브라우저로만 로그인하므로 비밀번호를 묻기 전에 거절한다. 로그인 화면의
/// 영역 목록(`GET /auth/realms`)을 읽지 못하면 그냥 지나간다(서버가 로그인에서 판단한다).
async fn oidc_login_refused(anon: &Client, server: &str, realm: &str) -> anyhow::Result<()> {
    let resp = anon
        .http
        .get(anon.url("/auth/realms"))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    let list: Value = if resp.status().is_success() {
        resp.json().await.unwrap_or_default()
    } else {
        Value::Null
    };
    let oidc = list
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| r["name"] == realm && r["kind"] == "oidc");
    if oidc {
        bail!(
            "sign in to {realm} in the web UI, create an API token there and run `oplctl login --token-file`"
        );
    }
    Ok(())
}

/// `login`이 만드는 API 토큰 이름의 앞부분
const LOGIN_TOKEN_PREFIX: &str = "oplctl-";

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다. 토큰과 서버가 정한
/// 사용자 이름(영역 사용자는 `<사용자>@<영역>`)을 돌려준다.
async fn login(
    server: &str,
    args: &LoginArgs,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let anon = Client::new(server, None, ca_pem)?;
    if args.realm != opl_types::realm::LOCAL_REALM {
        oidc_login_refused(&anon, server, &args.realm).await?;
    }
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let mut body = json!({ "username": args.user, "password": password });
    if args.realm != opl_types::realm::LOCAL_REALM {
        body["realm"] = json!(args.realm);
    }
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&body)
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
    let user: Value = resp.json().await.unwrap_or_default();
    let user = user["name"].as_str().unwrap_or(&args.user).to_string();
    let name = format!(
        "{LOGIN_TOKEN_PREFIX}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    let token = created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")?;
    Ok((token, user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool.clone(), pool_set_body(&s), pool_set_summary(&s)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }), "shared".into())
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            (
                "nas".into(),
                json!({ "shared": false }),
                "node-local".into()
            )
        );
        // 백업 표시는 따로 바꾸거나 공유 여부와 함께 바꾼다. 주지 않은 값은 보내지 않는다.
        assert_eq!(
            set(&["--backups", "true"]).unwrap(),
            (
                "nas".into(),
                json!({ "backups": true }),
                "backups on".into()
            )
        );
        assert_eq!(
            set(&["--no-shared", "--backups", "false"]).unwrap().1,
            json!({ "shared": false, "backups": false })
        );
        assert_eq!(
            set(&["--shared", "--backups", "false"]).unwrap().2,
            "shared, backups off"
        );
        // 하나는 꼭, 공유와 노드 로컬은 함께 못 준다, 백업은 true나 false
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
        assert!(set(&["--backups"]).is_err());
        assert!(set(&["--backups", "yes"]).is_err());
    }

    #[test]
    fn pool_create_marks_backup_pools() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "bk"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            body(&["--path", "/srv/bk", "--backups"]).unwrap()["spec"],
            json!({ "backend": { "kind": "dir", "path": "/srv/bk" }, "nodes": [], "backups": true })
        );
        assert_eq!(
            body(&["--nfs", "nas:/bk", "--backups"]).unwrap()["spec"]["backups"],
            json!(true)
        );
        // 끈 풀은 보내지 않는다.
        assert!(body(&["--path", "/srv/bk"]).unwrap()["spec"]["backups"].is_null());
        // 블록 풀은 백업을 담지 못한다.
        for args in [
            &["--lvm-thin", "vg0/tp", "--backups"][..],
            &["--zfs", "tank/opl", "--backups"],
            &["--rbd", "rbd", "--backups"],
            &["--lvm-shared", "san", "--backups"],
        ] {
            assert!(body(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn describes_a_backup_pool() {
        let pool = json!({
            "name": "bk",
            "spec": { "backend": { "kind": "dir", "path": "/srv/bk" }, "backups": true }
        });
        assert_eq!(
            describe_pool(&pool),
            "pool: bk\nkind: dir\nlocation: /srv/bk\nshared: -\nbackups: true\n"
        );
    }

    #[test]
    fn parses_backup_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Backup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk"]).unwrap(),
            BackupCmd::Create { vm, pool, name: None, incremental: false } if vm == "web" && pool == "bk"
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--incremental"]).unwrap(),
            BackupCmd::Create {
                incremental: true,
                ..
            }
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--name", "before-upgrade"]).unwrap(),
            BackupCmd::Create { name: Some(n), .. } if n == "before-upgrade"
        ));
        assert!(matches!(
            parse(&["list", "--vm", "web"]).unwrap(),
            BackupCmd::List { vm: Some(v) } if v == "web"
        ));
        assert!(matches!(
            parse(&["restore", "web-1", "--name", "web2", "--pool", "local", "--node", "n1"])
                .unwrap(),
            BackupCmd::Restore { backup, name, pool, node: Some(node) }
                if backup == "web-1" && name == "web2" && pool == "local" && node == "n1"
        ));
        assert!(matches!(
            parse(&["replicate", "web-1", "--remote", "dr", "--pool", "bk"]).unwrap(),
            BackupCmd::Replicate { backup, remote, pool, wait: false }
                if backup == "web-1" && remote == "dr" && pool == "bk"
        ));
        assert!(matches!(
            parse(&[
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk",
                "--wait"
            ])
            .unwrap(),
            BackupCmd::Replicate { wait: true, .. }
        ));
        // 백업 풀과 복원의 새 이름·풀, 복제할 원격·풀은 꼭 준다.
        for args in [
            &["create", "web"][..],
            &["restore", "web-1", "--pool", "local"],
            &["restore", "web-1", "--name", "web2"],
            &["replicate", "web-1", "--pool", "bk"],
            &["replicate", "web-1", "--remote", "dr"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_list_shows_the_vm_name_and_stored_size() {
        let ready = json!({
            "name": "web-20261104-030000",
            "spec": { "vm": "x", "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "disks": [
                    { "name": "disk0", "stored_bytes": 3u64 << 30 },
                    { "name": "disk1", "stored_bytes": 1u64 << 29 }
                ]
            }
        });
        assert_eq!(
            backup_row(&ready),
            [
                "web-20261104-030000",
                "web",
                "bk",
                "ready",
                "2026-11-04 03:00 UTC",
                "3.5GiB",
                "full",
                "-",
                "-"
            ]
        );
        // 만드는 중이거나 실패한 백업은 크기가 없다.
        let pending = json!({
            "name": "b", "spec": { "vm_name": "web", "pool": "bk" },
            "status": { "phase": "pending", "created_at": null, "disks": [] }
        });
        assert_eq!(backup_row(&pending)[3..6], ["pending", "-", "-"]);
        // 부모가 있으면 증분이고, 크기는 증분 파일만이다.
        let incr = json!({
            "name": "web-2", "spec": { "vm_name": "web", "pool": "bk", "incremental": true },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "parent": "019a4f2c-1111-7000-8000-000000000000",
                "disks": [{ "name": "disk0", "stored_bytes": 1u64 << 20 }]
            }
        });
        assert_eq!(backup_row(&incr)[5..7], ["1.0MiB", "incr"]);
        // 증분을 청했어도 전체로 했으면 전체다.
        let full = json!({
            "name": "web-1", "spec": { "vm_name": "web", "pool": "bk", "incremental": true },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "full_reason": "VM is stopped", "disks": []
            }
        });
        assert_eq!(backup_row(&full)[6], "full");
    }

    #[test]
    fn backup_chain_names_the_backups_from_the_full_one() {
        let backup = |id: &str, name: &str, parent: Option<&str>, bytes: u64| {
            json!({
                "id": id, "name": name,
                "status": { "phase": "ready", "parent": parent, "disks": [
                    { "name": "disk0", "stored_bytes": bytes },
                    { "name": "disk1", "stored_bytes": 1 }
                ] }
            })
        };
        let all = [
            backup("a", "full", None, 1000),
            backup("b", "incr-1", Some("a"), 100),
            backup("c", "incr-2", Some("b"), 10),
            backup("x", "other", None, 5),
        ];
        assert_eq!(
            backup_chain(&all[2], &all),
            json!({ "backups": ["full", "incr-1", "incr-2"], "stored_bytes": 1113 })
        );
        // 목록에 없는 조상은 id로 적고 멈춘다. 체인 크기는 모른다.
        assert_eq!(
            backup_chain(&all[2], &all[1..]),
            json!({ "backups": ["a", "incr-1", "incr-2"] })
        );
        // 다 만들지 않은 백업이 있으면 크기는 적지 않는다.
        let mut pending = all.clone();
        pending[0]["status"]["phase"] = json!("failed");
        assert_eq!(
            backup_chain(&pending[2], &pending),
            json!({ "backups": ["full", "incr-1", "incr-2"] })
        );
        // 잘못된 목록에서 고리를 이루어도 멈춘다. 전체 백업에 닿지 못했으므로 크기는 모른다.
        let mut ring = all.clone();
        ring[0]["status"]["parent"] = json!("c");
        assert_eq!(
            backup_chain(&ring[2], &ring),
            json!({ "backups": ["full", "incr-1", "incr-2"] })
        );
    }

    /// `backup get`은 증분 백업에만 백업 목록으로 만든 `chain`을 붙인다.
    #[tokio::test]
    async fn backup_get_adds_the_chain_of_incremental_backups() {
        fn reply(path: &str) -> Value {
            let full = json!({ "id": "a", "name": "full", "status": { "phase": "ready",
                "disks": [{ "name": "disk0", "stored_bytes": 1000 }] } });
            let incr = json!({ "id": "b", "name": "incr", "status": { "phase": "ready",
                "parent": "a", "disks": [{ "name": "disk0", "stored_bytes": 10 }] } });
            match path {
                "/api/v1/backups/incr" => incr,
                "/api/v1/backups/full" => full,
                "/api/v1/backups" => json!({ "items": [full, incr] }),
                _ => json!({}),
            }
        }
        let get = |name: &'static str| async move {
            let (client, seen) = fake_api(reply).await;
            let mut out = Vec::new();
            backup(
                &client,
                BackupCmd::Get {
                    backup: name.into(),
                },
                false,
                &mut out,
            )
            .await
            .unwrap();
            let paths = seen.lock().unwrap().clone();
            (serde_json::from_slice::<Value>(&out).unwrap(), paths)
        };
        let (incr, paths) = get("incr").await;
        assert_eq!(
            incr["chain"],
            json!({ "backups": ["full", "incr"], "stored_bytes": 1010 })
        );
        assert_eq!(
            paths,
            [
                "GET /api/v1/backups/incr HTTP/1.1",
                "GET /api/v1/backups HTTP/1.1"
            ]
        );
        // 전체 백업에는 체인이 없고 목록도 읽지 않는다.
        let (full, paths) = get("full").await;
        assert!(full.get("chain").is_none(), "{full}");
        assert_eq!(paths, ["GET /api/v1/backups/full HTTP/1.1"]);
    }

    /// `backup create --incremental`은 `incremental`을 보내고 증분이 됐는지, 아니면 전체로 한 이유를
    /// 보인다. 청하지 않으면 보내지 않는다.
    #[tokio::test]
    async fn backup_create_reports_incremental_or_why_full() {
        let run = |reply: fn(&str) -> Value, name: Option<&'static str>, incremental: bool| async move {
            let (base, seen) = fake_api_bodies(reply).await;
            let client = Client::new(&base, None, None).unwrap();
            let mut out = Vec::new();
            backup_create(
                &client,
                "web",
                "bk",
                name.map(str::to_string),
                incremental,
                &mut out,
            )
            .await
            .unwrap();
            let seen = seen.lock().unwrap();
            let lines: Vec<String> = seen.iter().map(|r| r.line.clone()).collect();
            (String::from_utf8(out).unwrap(), lines, seen[0].body.clone())
        };
        fn task(path: &str) -> Option<Value> {
            match path {
                "/api/v1/vms/web/backups" => {
                    Some(json!({ "id": "t1", "spec": { "op": { "backup": "b1" } } }))
                }
                "/api/v1/tasks/t1" => {
                    Some(json!({ "id": "t1", "status": { "state": "succeeded" } }))
                }
                _ => None,
            }
        }

        let (out, lines, body) = run(
            |path| {
                task(path).unwrap_or_else(
                    || json!({ "id": "b1", "name": "web-1", "status": { "parent": "b0" } }),
                )
            },
            None,
            true,
        )
        .await;
        assert_eq!(out, "backup web-1 of vm web created (incremental)\n");
        assert_eq!(
            body,
            json!({ "pool": "bk", "name": null, "incremental": true })
        );
        assert_eq!(
            lines,
            [
                "POST /api/v1/vms/web/backups HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "GET /api/v1/backups/b1 HTTP/1.1"
            ]
        );

        let (out, _, _) = run(
            |path| {
                task(path).unwrap_or_else(|| {
                    json!({ "id": "b1", "name": "nightly", "status": { "full_reason": "VM is stopped" } })
                })
            },
            Some("nightly"),
            true,
        )
        .await;
        assert_eq!(
            out,
            "backup nightly of vm web created (full: VM is stopped)\n"
        );

        // 청하지 않고 이름을 주면 백업을 다시 읽지 않는다.
        let (out, lines, body) = run(|path| task(path).unwrap(), Some("nightly"), false).await;
        assert_eq!(out, "backup nightly of vm web created\n");
        assert_eq!(body, json!({ "pool": "bk", "name": "nightly" }));
        assert_eq!(lines.len(), 2, "{lines:?}");
    }

    #[test]
    fn backup_list_shows_the_origin_and_replicas() {
        // 원격에서 받은 사본은 보낸 클러스터와 원본 id 앞 8자
        let copy = json!({
            "name": "web-20261104-030000",
            "spec": {
                "vm_name": "web", "pool": "bk",
                "origin": {
                    "cluster": "prod", "ca_fingerprint": "sha256:aa",
                    "backup": "019a4f2c-1111-7000-8000-000000000000"
                }
            },
            "status": { "phase": "receiving", "created_at": 1_793_761_200, "disks": [] }
        });
        assert_eq!(backup_row(&copy)[7..], ["prod/019a4f2c", "-"]);
        // 원격 사본마다 원격:상태
        let original = json!({
            "name": "web-20261104-030000",
            "spec": { "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200, "disks": [],
                "replicas": [
                    { "remote": "dr", "pool": "bk", "backup": "019a4f2c-1111-7000-8000-000000000000", "state": "ready", "at": 1 },
                    { "remote": "far", "pool": "bk", "state": "failed", "error": "boom", "at": 2 }
                ]
            }
        });
        assert_eq!(backup_row(&original)[7..], ["-", "dr:ready,far:failed"]);
    }

    /// `backup-job create`의 인자를 요청 본문으로
    fn backup_job_create(args: &[&str]) -> Result<Value, clap::Error> {
        let mut argv = vec!["oplctl", "backup-job", "create", "nightly"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::BackupJob {
                cmd: BackupJobCmd::Create(c),
            } => backup_job_create_body(&c),
            _ => unreachable!(),
        })
    }

    #[test]
    fn backup_job_create_builds_the_spec() {
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--label",
                "tier=prod",
                "--label",
                "env=a=b",
                "--at",
                "03:00",
                "--days",
                "mon,wed",
                "--keep-last",
                "2",
                "--keep-weekly",
                "4",
            ])
            .unwrap(),
            json!({
                "name": "nightly",
                "spec": {
                    "vms": { "labels": { "tier": "prod", "env": "a=b" } },
                    "pool": "bk",
                    "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                    "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                    "enabled": true
                }
            })
        );
        // 요일을 주지 않으면 매일, 보존 규칙을 주지 않으면 모두 0(지우지 않음)
        let all =
            backup_job_create(&["--pool", "bk", "--all", "--at", "00:00", "--disabled"]).unwrap();
        assert_eq!(all["spec"]["vms"], json!({ "all": true }));
        assert_eq!(
            all["spec"]["schedule"],
            json!({ "at": "00:00", "days": [] })
        );
        assert_eq!(
            all["spec"]["keep"],
            json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 })
        );
        assert_eq!(all["spec"]["enabled"], json!(false));
        // 복제하지 않으면 `replicate`를 보내지 않는다.
        assert!(all["spec"].get("replicate").is_none(), "{all}");
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/bk"
            ])
            .unwrap()["spec"]["replicate"],
            json!({ "remote": "dr", "pool": "bk" })
        );
        assert_eq!(
            backup_job_create(&["--pool", "bk", "--vm", "web", "--vm", "db", "--at", "03:00"])
                .unwrap()["spec"]["vms"],
            json!({ "names": ["web", "db"] })
        );
        // 전체 백업 주기는 주었을 때만 보낸다.
        assert!(all["spec"].get("full_every").is_none(), "{all}");
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--full-every",
                "6"
            ])
            .unwrap()["spec"]["full_every"],
            json!(6)
        );
        // VM 고르기는 셋 중 꼭 하나, 풀과 시각은 꼭, 라벨은 KEY=VALUE, 요일은 mon..sun
        for args in [
            &["--pool", "bk", "--at", "03:00"][..],
            &["--pool", "bk", "--all", "--vm", "web", "--at", "03:00"],
            &[
                "--pool", "bk", "--vm", "web", "--label", "a=b", "--at", "03:00",
            ],
            &["--all", "--at", "03:00"],
            &["--pool", "bk", "--all"],
            &["--pool", "bk", "--label", "tier", "--at", "03:00"],
            &["--pool", "bk", "--label", "=prod", "--at", "03:00"],
            &["--pool", "bk", "--all", "--at", "03:00", "--days", "monday"],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--keep-last",
                "-1",
            ],
            // 복제 대상은 REMOTE/POOL
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "/bk",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/bk/x",
            ],
        ] {
            assert!(backup_job_create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_set_changes_only_what_is_given() {
        let current = json!({
            "name": "nightly",
            "revision": 9,
            "spec": {
                "vms": { "all": true }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["sat"] },
                "keep": { "last": 2, "daily": 7, "weekly": 0, "monthly": 0 },
                "enabled": true
            }
        });
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup-job", "set", "nightly"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::BackupJob {
                    cmd: BackupJobCmd::Set(s),
                } => backup_job_set_body(&s, &current),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--enabled", "false"]).unwrap(),
            json!({ "enabled": false })
        );
        assert_eq!(
            set(&["--vm", "web", "--pool", "bk2"]).unwrap(),
            json!({ "vms": { "names": ["web"] }, "pool": "bk2" })
        );
        // 일정과 보존 규칙은 통째로 바뀌므로 주지 않은 부분은 지금 값을 보낸다.
        assert_eq!(
            set(&["--at", "04:30"]).unwrap(),
            json!({ "schedule": { "at": "04:30", "days": ["sat"] } })
        );
        assert_eq!(
            set(&["--days", "sun,mon"]).unwrap(),
            json!({ "schedule": { "at": "03:00", "days": ["sun", "mon"] } })
        );
        assert_eq!(
            set(&["--keep-monthly", "6", "--keep-last", "1"]).unwrap(),
            json!({ "keep": { "last": 1, "daily": 7, "weekly": 0, "monthly": 6 } })
        );
        // 복제는 대상을 주면 바꾸고 --no-replicate면 null(끔)
        assert_eq!(
            set(&["--replicate", "dr/bk"]).unwrap(),
            json!({ "replicate": { "remote": "dr", "pool": "bk" } })
        );
        assert_eq!(
            set(&["--no-replicate"]).unwrap(),
            json!({ "replicate": null })
        );
        // 0이면 늘 전체로 돌아간다.
        assert_eq!(
            set(&["--full-every", "6"]).unwrap(),
            json!({ "full_every": 6 })
        );
        assert_eq!(
            set(&["--full-every", "0"]).unwrap(),
            json!({ "full_every": 0 })
        );
        // 바꿀 것은 꼭 하나, VM 고르기는 하나만, 사용 여부는 true나 false, 복제는 켜거나 끄거나
        for args in [
            &[][..],
            &["--all", "--vm", "web"],
            &["--enabled"],
            &["--enabled", "yes"],
            &["--replicate", "dr/bk", "--no-replicate"],
            &["--replicate", "dr"],
            &["--full-every", "-1"],
        ] {
            assert!(set(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_list_shows_the_schedule_and_runs() {
        let job = json!({
            "name": "nightly",
            "spec": {
                "vms": { "labels": { "env": "a", "tier": "prod" } }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                "enabled": true
            },
            "status": {
                "next_run": 1_793_761_200, "run": null,
                "last_run": { "started_at": 1_793_674_800, "finished_at": 1_793_675_100, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "db", "error": "boom" }]
            }
        });
        assert_eq!(
            backup_job_row(&job),
            [
                "nightly",
                "bk",
                "env=a,tier=prod",
                "mon,wed 03:00 UTC",
                "last=2,weekly=4",
                "true",
                "2026-11-04 03:00 UTC",
                "2026-11-03 03:05 UTC (3 ok, 1 failed)"
            ]
        );
        // 매일, 보존 규칙 없음, 진행 중
        let mut running = job.clone();
        running["spec"]["vms"] = json!({ "names": ["web", "db"] });
        running["spec"]["schedule"]["days"] = json!([]);
        running["spec"]["keep"] = json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 });
        running["status"]["run"] = json!({ "started_at": 1, "queue": [], "backups": [] });
        assert_eq!(
            backup_job_row(&running)[2..],
            [
                "web,db",
                "daily 03:00 UTC",
                "all",
                "true",
                "2026-11-04 03:00 UTC",
                "running"
            ]
        );
        // 끈 작업은 다음 실행이 없고, 한 번도 돌지 않았으면 마지막 실행도 없다.
        let mut off = job.clone();
        off["spec"]["vms"] = json!({ "all": true });
        off["spec"]["enabled"] = json!(false);
        off["status"]["last_run"] = Value::Null;
        assert_eq!(backup_job_row(&off)[2], "all");
        assert_eq!(backup_job_row(&off)[5..], ["false", "-", "-"]);
        // 증분 백업 작업은 전체 백업 주기를 함께 보인다(0은 늘 전체).
        let mut incremental = job.clone();
        incremental["spec"]["full_every"] = json!(6);
        assert_eq!(
            backup_job_row(&incremental)[3],
            "mon,wed 03:00 UTC, full every 6"
        );
        incremental["spec"]["full_every"] = json!(0);
        assert_eq!(backup_job_row(&incremental)[3], "mon,wed 03:00 UTC");
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 브리지와 VXLAN 중 정확히 하나. VLAN은 브리지와, MTU는 VXLAN과만. 주지 않은 값은 보내지 않는다.
    #[test]
    fn parses_network_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "network", "create", "lan"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Network {
                    cmd: NetworkCmd::Create(c),
                } => network_create_body(&c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--bridge", "vmbr0"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } })
        );
        assert_eq!(
            create(&["--bridge", "vmbr0", "--vlan", "10"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0", "vlan": 10 } })
        );
        assert_eq!(
            create(&["--vxlan", "100"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 } } })
        );
        assert_eq!(
            create(&["--vxlan", "100", "--mtu", "1400"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 }, "mtu": 1400 } })
        );
        for args in [
            &[][..],
            &["--bridge", "vmbr0", "--vxlan", "100"],
            &["--vxlan", "100", "--vlan", "10"],
            &["--bridge", "vmbr0", "--mtu", "1400"],
            &["--vlan", "10"],
            &["--mtu", "1400"],
            &["--vxlan", "-1"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 종류, 붙는 브리지(VXLAN은 관리 이름), VLAN이나 VNI, VXLAN MTU(없으면 기본값)
    #[test]
    fn network_list_shows_kind_bridge_and_mtu() {
        let rows: Vec<_> = [
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
            json!({ "name": "dmz", "spec": { "bridge": "vmbr0", "vlan": 20 } }),
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 100 } } }),
            json!({ "name": "jumbo", "spec": { "vxlan": { "vni": 7 }, "mtu": 8950 } }),
        ]
        .iter()
        .map(network_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NETWORK_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   TYPE    BRIDGE   VLAN/VNI  MTU\n\
             lan    bridge  vmbr0    -         -\n\
             dmz    bridge  vmbr0    20        -\n\
             ov     vxlan   oplv100  100       1450\n\
             jumbo  vxlan   oplv7    7         8950\n"
        );
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 받은 요청 줄을 `seen`에 남기고 경로에 맞는 JSON을 200으로 돌려주는 작은 HTTP 서버
    async fn fake_api(
        reply: fn(&str) -> Value,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        fake_api_status(move |path| (200, reply(path))).await
    }

    /// `fake_api`와 같고 응답 상태 코드도 정한다.
    async fn fake_api_status(
        reply: impl Fn(&str) -> (u16, Value) + Send + 'static,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(line);
                let (status, body) = reply(&path);
                let body = body.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        (client, seen)
    }

    /// `backup delete --force`는 `?force=true`로 보내고, 기록만 지웠으면 서버의 알림을 보인다.
    /// 아니면 Task를 기다린다.
    #[tokio::test]
    async fn backup_delete_force_shows_the_server_message() {
        let cli = Cli::try_parse_from(["oplctl", "backup", "delete", "b1", "--force"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                cmd: BackupCmd::Delete { force: true, .. }
            }
        ));

        let (client, seen) =
            fake_api(|_| json!({ "message": "backup \"b1\" deleted; its files are left" })).await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", true, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/backups/b1?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup \"b1\" deleted; its files are left\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/backups/b1 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");

        // 원격 사본을 지우지 못했으면 Task의 경고를 보인다.
        let (client, _) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": {
                    "state": "succeeded",
                    "error": "backup deleted, but remote copies may be left: dr: unreachable"
                } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup b1 deleted\nwarning: backup deleted, but remote copies may be left: dr: unreachable\n"
        );
    }

    /// `backup replicate`는 원격·풀을 본문으로 복제 Task를 만들고 따라가는 방법을 알린다. `--wait`이면
    /// Task를 기다린다.
    #[tokio::test]
    async fn backup_replicate_starts_the_task() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/backups/web-1/actions/replicate" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Backup {
                    cmd:
                        BackupCmd::Replicate {
                            backup,
                            remote,
                            pool,
                            wait,
                        },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                backup_replicate(&client, &backup, &remote, &pool, wait, &mut out)
                    .await
                    .unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&[
                "oplctl",
                "backup",
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk"
            ])
            .await,
            "replication of backup web-1 to remote dr started (task t1); follow it with `oplctl task get t1`\n"
        );
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "{seen:?}");
            assert_eq!(
                seen[0].line,
                "POST /api/v1/backups/web-1/actions/replicate HTTP/1.1"
            );
            assert_eq!(seen[0].body, json!({ "remote": "dr", "pool": "bk" }));
        }
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&[
                "oplctl",
                "backup",
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk",
                "--wait"
            ])
            .await,
            "backup web-1 replicated to pool bk of remote dr\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "POST /api/v1/backups/web-1/actions/replicate HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `backup-job set`은 작업을 읽어 그 값에 맞춰 PATCH하고, `run`은 본문 없이 POST한다.
    #[tokio::test]
    async fn backup_job_set_reads_then_patches_and_run_posts() {
        let (client, seen) = fake_api(|_| {
            json!({
                "name": "nightly", "revision": 3,
                "spec": { "schedule": { "at": "03:00", "days": [] }, "keep": {} },
                "status": {}
            })
        })
        .await;
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "set", "nightly", "--at", "04:00"])
            .unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        let mut out = Vec::new();
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "run", "nightly"]).unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/backup-jobs/nightly HTTP/1.1",
                "PATCH /api/v1/backup-jobs/nightly HTTP/1.1",
                "POST /api/v1/backup-jobs/nightly/run HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup job nightly updated\nbackup job nightly will run at the scheduler's next check\n"
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        a["status"]["underlay"] = json!("10.0.0.1");
        a["status"]["version"] = json!("0.2.0");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        // underlay가 없는 노드(위트니스, 옛 서버)는 `-`다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        VERSION  UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  0.2.0    10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -        -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -        -         true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 버전을 아는 멤버의 버전이 둘 이상이면 낮은 것부터 한 번씩 적어 경고한다. 모르는 멤버는 세지 않는다.
    #[test]
    fn cluster_status_warns_about_mixed_versions() {
        let member = |name: &str, version: Option<&str>| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": true,
                "online": true,
                "witness": false,
                "version": version,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.9.0")),
            member("c", Some("0.10.0")),
            member("d", Some("0.10.0~rc1")),
            member("e", None),
        ]);
        assert!(
            out.ends_with(
                "warning: nodes run different opLite versions (0.9.0, 0.10.0~rc1, 0.10.0); \
                 finish the upgrade on every node\n"
            ),
            "{out}"
        );
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.10.0")),
            member("c", None),
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 방향과 동작은 꼭, 프로토콜·포트·CIDR은 이 차례로 생략할 수 있다.
    #[test]
    fn parses_rules() {
        assert_eq!(
            parse_rule("in accept tcp 22 10.0.0.0/8"),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "tcp",
                "ports": "22", "remote": "10.0.0.0/8"
            }))
        );
        assert_eq!(
            parse_rule("out drop tcp 25"),
            Ok(json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }))
        );
        assert_eq!(
            parse_rule("in accept icmp"),
            Ok(json!({ "direction": "in", "action": "accept", "protocol": "icmp" }))
        );
        assert_eq!(
            parse_rule("  in   accept udp 8000-8080 "),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "udp", "ports": "8000-8080"
            }))
        );
        assert_eq!(
            parse_rule("out accept 2001:db8::/32"),
            Ok(json!({ "direction": "out", "action": "accept", "remote": "2001:db8::/32" }))
        );
        assert_eq!(
            parse_rule("in drop"),
            Ok(json!({ "direction": "in", "action": "drop" }))
        );
        // 포트 범위와 프로토콜·포트의 짝은 서버가 본다.
        assert_eq!(
            parse_rule("in accept 99999"),
            Ok(json!({ "direction": "in", "action": "accept", "ports": "99999" }))
        );
        for bad in [
            "",
            "in",
            "accept in",
            "up accept",
            "in allow",
            "in accept sctp",
            "in accept tcp 22 10.0.0.0/8 extra",
            "in accept 10.0.0.0/8 tcp",
            "in accept tcp 10.0.0.0/8 22",
            "in accept tcp ssh",
            "in accept tcp 22-",
            "in accept tcp -22",
            "IN accept",
        ] {
            let err = parse_rule(bad).unwrap_err();
            assert!(err.contains("in|out accept|drop"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_security_group_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "security-group"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::SecurityGroup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&[
            "create",
            "web",
            "--description",
            "web servers",
            "--rule",
            "in accept tcp 443",
            "--rule",
            "in accept icmp",
        ])
        .unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [
                    { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" },
                    { "direction": "in", "action": "accept", "protocol": "icmp" },
                ]
            }})
        );
        // 설명이 없으면 보내지 않는다. 규칙 없는 그룹도 된다.
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&["create", "empty"]).unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "empty", "spec": { "rules": [] } })
        );
        assert!(parse(&["create", "web", "--rule", "in maybe"]).is_err());
        assert!(matches!(
            parse(&["rule", "add", "web", "out drop tcp 25", "--at", "2"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Add { group, rule, at: Some(2) } }
                if group == "web" && rule["ports"] == "25"
        ));
        assert!(matches!(
            parse(&["rule", "rm", "web", "3"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Rm { group, n: 3 } } if group == "web"
        ));
        assert!(parse(&["rule", "add", "web", "sideways accept"]).is_err());
        assert!(parse(&["rule", "rm", "web", "first"]).is_err());
        assert!(matches!(
            parse(&["get", "web"]).unwrap(),
            SecurityGroupCmd::Get { group } if group == "web"
        ));
        assert!(matches!(
            parse(&["delete", "web"]).unwrap(),
            SecurityGroupCmd::Delete { group } if group == "web"
        ));
    }

    /// 규칙 번호는 1부터다. 범위 밖이면 아무것도 바꾸지 않고 실패한다.
    #[test]
    fn adds_and_removes_rules_by_number() {
        let rule =
            |n: u16| json!({ "direction": "in", "action": "accept", "ports": n.to_string() });
        let group = json!({ "name": "web", "spec": { "rules": [rule(1), rule(2)] } });
        assert_eq!(
            rules_with(&group, rule(9), None).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(1)).unwrap(),
            vec![rule(9), rule(1), rule(2)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(3)).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        for at in [0, 4] {
            let err = rules_with(&group, rule(9), Some(at))
                .unwrap_err()
                .to_string();
            assert!(err.contains("use 1 to 3"), "{err}");
        }
        assert_eq!(rules_without(&group, 1).unwrap(), vec![rule(2)]);
        assert_eq!(rules_without(&group, 2).unwrap(), vec![rule(1)]);
        for n in [0, 3] {
            let err = rules_without(&group, n).unwrap_err().to_string();
            assert!(err.contains(&format!("has no rule {n}")), "{err}");
        }
        // 규칙이 없는 그룹은 응답에 rules가 비어 있다.
        let empty = json!({ "name": "e", "spec": { "rules": [] } });
        assert_eq!(rules_with(&empty, rule(1), None).unwrap(), vec![rule(1)]);
        assert!(rules_without(&empty, 1).is_err());
    }

    #[test]
    fn security_group_list_shows_rule_counts() {
        let rows: Vec<_> = [
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [{ "direction": "in", "action": "accept" }, { "direction": "out", "action": "drop" }]
            }}),
            json!({ "name": "empty", "spec": { "rules": [] } }),
        ]
        .iter()
        .map(security_group_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, SECURITY_GROUP_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   RULES  DESCRIPTION\n\
             web    2      web servers\n\
             empty  0      -\n"
        );
    }

    /// `--nic NETWORK[,sg=A+B]`. 그룹이 없으면 `security_groups`를 보내지 않는다.
    #[test]
    fn parses_nics_with_security_groups() {
        let nics = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "create", "web"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Vm {
                    cmd: VmCmd::Create(c),
                } => c.nics.iter().map(nic_body).collect::<Vec<_>>(),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            nics(&[
                "--nic",
                "user",
                "--nic",
                "lan,sg=web+db",
                "--nic",
                "dmz,sg=web"
            ])
            .unwrap(),
            vec![
                json!({ "network": "user", "model": "virtio" }),
                json!({ "network": "lan", "model": "virtio", "security_groups": ["web", "db"] }),
                json!({ "network": "dmz", "model": "virtio", "security_groups": ["web"] }),
            ]
        );
        for bad in [
            "",
            ",sg=web",
            "lan,",
            "lan,sg=",
            "lan,sg=web+",
            "lan,sg=+web",
            "lan,group=web",
            "lan,sg=web,sg=db",
        ] {
            assert!(nics(&["--nic", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_vm_nic_sg() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "1", "a", "b"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { vm, index: 1, groups } }
                if vm == "web" && groups == ["a", "b"]
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "0"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { index: 0, groups, .. } } if groups.is_empty()
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "-1"]).is_err());
    }

    /// 그 NIC의 그룹만 바꾸고 나머지(다른 NIC, 다른 필드, 라벨)는 그대로 둔다.
    #[test]
    fn nic_groups_update_changes_one_nic() {
        let vm = json!({
            "name": "web",
            "revision": 4,
            "labels": { "team": "a" },
            "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["old"] },
                ],
            },
        });
        let groups = ["web".to_string(), "db".to_string()];
        assert_eq!(
            nic_groups_update(&vm, 1, &groups).unwrap(),
            json!({ "labels": { "team": "a" }, "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["web", "db"] },
                ],
            }})
        );
        assert_eq!(
            nic_groups_update(&vm, 1, &[]).unwrap()["spec"]["nics"][1]["security_groups"],
            json!([])
        );
        let err = nic_groups_update(&vm, 2, &groups).unwrap_err().to_string();
        assert_eq!(err, "VM web has no NIC 2 (2 NICs, numbered from 0)");
    }

    /// `import` 명령의 인자
    fn parse_import(args: &[&str]) -> Result<ImportCmd, clap::Error> {
        let mut argv = vec!["oplctl", "import"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::Import { cmd } => cmd,
            _ => unreachable!(),
        })
    }

    /// `import run`의 인자를 요청 본문으로. `sources`는 가져오기의 원본 네트워크다.
    fn import_run(args: &[&str], sources: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["run", "web01"];
        argv.extend_from_slice(args);
        let ImportCmd::Run(run) = parse_import(&argv)? else {
            unreachable!()
        };
        let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
        import_run_body(&run, &sources)
    }

    #[test]
    fn parses_import_commands() {
        assert!(matches!(
            parse_import(&["ova", "/tmp/web01.ova", "--name", "web01"]).unwrap(),
            ImportCmd::Ova { file, name } if file == std::path::Path::new("/tmp/web01.ova") && name == "web01"
        ));
        assert!(matches!(parse_import(&["list"]).unwrap(), ImportCmd::List));
        assert!(matches!(
            parse_import(&["get", "web01"]).unwrap(),
            ImportCmd::Get { import } if import == "web01"
        ));
        assert!(matches!(
            parse_import(&["delete", "web01", "--force"]).unwrap(),
            ImportCmd::Delete { import, force: true } if import == "web01"
        ));
        // 가져오기 이름, 새 VM 이름과 풀은 꼭 준다.
        for args in [
            &["ova", "/tmp/web01.ova"][..],
            &["run", "web01", "--pool", "local"],
            &["run", "web01", "--name", "web"],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--disk-bus",
                "ide",
            ],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--net",
                "VM Network=",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "=lan",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "",
            ],
        ] {
            assert!(parse_import(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn import_run_builds_the_body() {
        // 주지 않은 값은 비워 두어 서버가 제안 값을 쓴다.
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "VM Network=lan",
                    "--net",
                    "DMZ=dmz",
                    "--net",
                    "a=b=lan"
                ],
                &["DMZ", "VM Network", "a=b"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan", "DMZ": "dmz", "a=b": "lan" },
                "disk_bus": null, "nic_model": null, "firmware": null,
                "sockets": null, "cores": null, "memory_mib": null,
                "keep_macs": true
            })
        );
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "lan",
                    "--disk-bus",
                    "sata",
                    "--nic-model",
                    "e1000e",
                    "--firmware",
                    "bios",
                    "--sockets",
                    "2",
                    "--cores",
                    "4",
                    "--memory",
                    "8192",
                    "--new-macs"
                ],
                &["VM Network"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan" },
                "disk_bus": "sata", "nic_model": "e1000e", "firmware": "bios",
                "sockets": 2, "cores": 4, "memory_mib": 8192,
                "keep_macs": false
            })
        );
        assert_eq!(
            import_run(
                &["--name", "web", "--pool", "p", "--disk-bus", "virtio-blk"],
                &[]
            )
            .unwrap()["disk_bus"],
            "virtio-blk"
        );
        // `=` 없는 --net은 원본 네트워크가 하나일 때만 쓴다.
        let err = import_run(
            &["--name", "web", "--pool", "p", "--net", "lan"],
            &["A", "B"],
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 2 source networks (\"A\", \"B\")"
        );
        let err = import_run(&["--name", "web", "--pool", "p", "--net", "lan"], &[]);
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 0 source networks ()"
        );
    }

    fn sample_import() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "web01",
            "spec": {
                "node": "n-1",
                "source": { "ova": { "file_name": "web01.ova", "size_bytes": 3u64 << 29 } }
            },
            "status": {
                "phase": "ready",
                "source_vm": {
                    "name": "web01-src", "guest_os": "otherLinux64Guest", "firmware": "uefi",
                    "vcpus": 2, "cores_per_socket": 2, "memory_mib": 1024,
                    "disks": [
                        { "name": "disk0", "file": "web01-disk1.vmdk", "capacity_bytes": 10u64 << 30, "controller": "scsi" }
                    ],
                    "nics": [
                        { "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" },
                        { "network": "DMZ", "adapter": "e1000" }
                    ]
                },
                "proposal": {
                    "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
                    "disk_bus": "virtio-blk", "nic_model": "virtio",
                    "networks": { "DMZ": null, "VM Network": "VM Network" }
                },
                "warnings": ["first warning", "second warning"],
                "error": "checksum mismatch for web01-disk1.vmdk"
            }
        })
    }

    #[test]
    fn source_networks_are_the_proposal_keys() {
        assert_eq!(source_networks(&sample_import()), ["DMZ", "VM Network"]);
        assert!(source_networks(&json!({ "status": {} })).is_empty());
    }

    #[test]
    fn import_list_shows_node_source_and_vm_names() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let vms = [json!({ "id": "v-1", "name": "web" })];
        assert_eq!(
            import_row(&sample_import(), &nodes, &vms),
            ["web01", "node1", "web01-src", "ready", "-"]
        );
        // 만든 VM은 이름으로, 지운 VM·빠진 노드는 id로 보인다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        assert_eq!(import_row(&done, &nodes, &vms)[3..], ["done", "web"]);
        assert_eq!(
            import_row(&done, &[], &[])[1..],
            ["n-1", "web01-src", "done", "v-1"]
        );
        // 원본을 읽지 못한 가져오기는 파일 이름을 보인다.
        let mut failed = sample_import();
        failed["status"] = json!({ "phase": "failed" });
        assert_eq!(import_row(&failed, &nodes, &vms)[2], "web01.ova");
    }

    #[test]
    fn import_get_shows_the_source_proposal_and_warnings() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let mut out = Vec::new();
        print_import(&mut out, &sample_import(), &nodes, &[]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
vm-import: web01
node: node1
source: web01.ova (1.5GiB)
phase: ready
error: checksum mismatch for web01-disk1.vmdk
source vm: web01-src (otherLinux64Guest, uefi, 2 vCPUs, 2 cores per socket, 1024 MiB)
proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs

DISK   FILE              SIZE     CONTROLLER
disk0  web01-disk1.vmdk  10.0GiB  scsi

NETWORK     ADAPTER  MAC                PROPOSED
VM Network  vmxnet3  00:50:56:aa:bb:cc  VM Network
DMZ         e1000    -                  -

warnings:
  first warning
  second warning
"
        );
        // 끝난 가져오기는 만든 VM을, 경고가 없으면 경고 줄을 뺀다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        done["status"]["warnings"] = json!([]);
        done["status"]["error"] = Value::Null;
        let mut out = Vec::new();
        print_import(
            &mut out,
            &done,
            &nodes,
            &[json!({ "id": "v-1", "name": "web" })],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("phase: done\nvm: web\nsource vm: "), "{text}");
        assert!(
            !text.contains("warnings") && !text.contains("error"),
            "{text}"
        );
    }

    /// `import ova`는 크기로 먼저 확인하고 실제 파일 이름과 함께 올린다.
    #[tokio::test]
    async fn import_ova_checks_then_uploads_with_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        // 본문이 없어야 작은 시험 서버가 요청 줄만 읽고 답할 수 있다.
        let file = dir.path().join("web01-export.ova");
        std::fs::write(&file, b"").unwrap();
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let cmd = ImportCmd::Ova {
            file,
            name: "web01".into(),
        };
        let mut out = Vec::new();
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/vm-imports/ova/check?name=web01&size=0 HTTP/1.1",
                "POST /api/v1/vm-imports/ova?name=web01&file_name=web01-export.ova HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 uploaded; review it with `oplctl import get web01`\n"
        );
    }

    /// `=` 없는 `--net`이면 가져오기를 읽어 원본 네트워크를 찾고, 실행 Task를 기다린다.
    #[tokio::test]
    async fn import_run_reads_the_import_for_a_bare_net_and_waits() {
        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else if path.ends_with("/run") {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            } else {
                json!({ "status": { "proposal": { "networks": { "VM Network": null } } } })
            }
        })
        .await;
        let run = |args: &[&str]| {
            let mut argv = vec![
                "oplctl", "import", "run", "web01", "--name", "web", "--pool", "p",
            ];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Import { cmd } => cmd,
                _ => unreachable!(),
            }
        };
        let mut out = Vec::new();
        import(&client, run(&["--net", "lan"]), false, &mut out)
            .await
            .unwrap();
        // SOURCE=NETWORK만 주면 읽지 않는다.
        import(&client, run(&["--net", "VM Network=lan"]), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/vm-imports/web01 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm web imported\nvm web imported\n"
        );
    }

    /// `import delete --force`는 `?force=true`로 보내고 서버의 알림을 보인다. 아니면 Task를 기다린다.
    #[tokio::test]
    async fn import_delete_force_shows_the_server_message() {
        let (client, seen) = fake_api(|_| {
            json!({ "message": "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)" })
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: true,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/vm-imports/web01?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: false,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/vm-imports/web01 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "vm-import web01 deleted\n");
    }

    #[test]
    fn parses_import_esxi_commands() {
        let ImportCmd::EsxiBrowse { conn, fingerprint } =
            parse_import(&["esxi-browse", "--host", "esx1", "--user", "root"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fingerprint, None);
        // 포트와 데이터센터는 ESXi 기본값이다.
        assert_eq!(
            esxi_connection(&conn, fingerprint.as_deref(), "secret"),
            json!({
                "host": "esx1", "port": 443, "user": "root", "password": "secret",
                "datacenter": "ha-datacenter", "fingerprint": null
            })
        );
        let ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } = parse_import(&[
            "esxi",
            "web01",
            "--host",
            "vc1",
            "--port",
            "8443",
            "--user",
            "admin@vsphere.local",
            "--datacenter",
            "DC1",
            "--datastore",
            "datastore1",
            "--vmx",
            "web01/web01.vmx",
            "--fingerprint",
            "AB:CD",
        ])
        .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            esxi_import_body(
                &name,
                &esxi_connection(&conn, Some(&fingerprint), "pw"),
                &datastore,
                &vmx
            ),
            json!({
                "name": "web01",
                "connection": {
                    "host": "vc1", "port": 8443, "user": "admin@vsphere.local", "password": "pw",
                    "datacenter": "DC1", "fingerprint": "AB:CD"
                },
                "datastore": "datastore1",
                "vmx": "web01/web01.vmx"
            })
        );
        // 만들기에는 지문·데이터스토어·.vmx가 꼭 있어야 하고, 비밀번호는 인자로 받지 않는다.
        let full = [
            "esxi",
            "web01",
            "--host",
            "esx1",
            "--user",
            "root",
            "--datastore",
            "ds",
            "--vmx",
            "a/a.vmx",
            "--fingerprint",
            "AB",
        ];
        assert!(parse_import(&full).is_ok());
        for skip in ["--datastore", "--vmx", "--fingerprint", "--host", "--user"] {
            let i = full.iter().position(|a| *a == skip).unwrap();
            let args: Vec<&str> = full[..i].iter().chain(&full[i + 2..]).copied().collect();
            assert!(parse_import(&args).is_err(), "{args:?}");
        }
        let mut with_password = full.to_vec();
        with_password.extend(["--password", "x"]);
        assert!(parse_import(&with_password).is_err());
        assert!(parse_import(&["esxi-browse", "--host", "esx1"]).is_err());

        // 비밀번호 파일(끝의 줄바꿈은 뺀다)
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("esxi-password");
        std::fs::write(&file, "VMware1!\n").unwrap();
        let mut with_file = full.to_vec();
        with_file.extend(["--password-file", file.to_str().unwrap()]);
        let ImportCmd::Esxi { conn, .. } = parse_import(&with_file).unwrap() else {
            unreachable!()
        };
        assert_eq!(esxi_password(&conn).unwrap(), "VMware1!");
    }

    fn sample_browse() -> Value {
        json!({
            "datastores": [
                {
                    "name": "datastore1",
                    "vms": [
                        {
                            "vmx": "web01/web01.vmx", "name": "web01", "guest_os": "ubuntu-64",
                            "vcpus": 2, "memory_mib": 1024, "disks": 1, "locked": false, "error": null
                        },
                        {
                            "vmx": "db/db.vmx", "name": "db", "guest_os": "", "vcpus": 0,
                            "memory_mib": 0, "disks": 0, "locked": true,
                            "error": "memsize is missing"
                        }
                    ]
                },
                { "name": "empty", "vms": [] }
            ]
        })
    }

    /// `import esxi-browse`는 연결을 보내고 데이터스토어의 VM을 표로 보인다. 읽지 못한 VM은 NAME
    /// 자리에 오류를 보인다.
    #[tokio::test]
    async fn import_esxi_browse_shows_the_vms() {
        let (client, seen) = fake_api(|_| sample_browse()).await;
        let mut out = Vec::new();
        let conn = json!({ "host": "esx1" });
        esxi_browse(&client, &conn, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi/browse HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
DATASTORE   VMX              NAME                       GUEST      CPU  MEMORY  DISKS  LOCKED
datastore1  web01/web01.vmx  web01                      ubuntu-64  2    1.0GiB  1      false
datastore1  db/db.vmx        error: memsize is missing  -          -    -       -      true
"
        );
        // --json이면 응답 그대로
        let mut out = Vec::new();
        esxi_browse(&client, &conn, true, &mut out).await.unwrap();
        let printed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed, sample_browse());
    }

    /// 지문을 주지 않았거나(409 `untrusted_certificate`) 틀리면 서버 인증서의 지문으로 다시 하라고 알린다.
    #[tokio::test]
    async fn import_esxi_explains_an_untrusted_certificate() {
        let (client, _) = fake_api_status(|_| {
            (
                409,
                json!({
                    "code": "untrusted_certificate",
                    "message": "verify the ESXi certificate fingerprint",
                    "details": { "fingerprint": "AB:CD:EF" }
                }),
            )
        })
        .await;
        let conn = json!({ "host": "esx1" });
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        let body = esxi_import_body("web01", &conn, "ds", "a/a.vmx");
        let err = esxi_create(&client, &body, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        // 다른 오류는 서버의 메시지와 코드 그대로
        let (client, _) = fake_api_status(|_| {
            (
                400,
                json!({ "code": "esxi_auth", "message": "ESXi rejected the user name or password" }),
            )
        })
        .await;
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ESXi rejected the user name or password (esxi_auth)"
        );
    }

    /// `import esxi`는 가져오기를 만들고 검토 명령을 알린다.
    #[tokio::test]
    async fn import_esxi_creates_the_import() {
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let mut out = Vec::new();
        let body = esxi_import_body("web01", &json!({}), "ds", "a/a.vmx");
        esxi_create(&client, &body, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 created; review it with `oplctl import get web01`\n"
        );
    }

    fn sample_esxi_import() -> Value {
        let mut i = sample_import();
        i["spec"]["source"] = json!({ "esxi": {
            "host": "esx1.example.com", "port": 443, "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": "root",
            "fingerprint": "AB:CD"
        } });
        i
    }

    /// ESXi에서 가져오면 SOURCE는 `esxi://호스트/데이터스토어/.vmx`다.
    #[test]
    fn import_list_and_get_show_an_esxi_source() {
        let url = "esxi://esx1.example.com/datastore1/web01/web01.vmx";
        assert_eq!(import_row(&sample_esxi_import(), &[], &[])[2], url);
        let mut out = Vec::new();
        print_import(&mut out, &sample_esxi_import(), &[], &[]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!(
                "vm-import: web01\nnode: n-1\nsource: {url}\nphase: ready\n"
            )),
            "{text}"
        );
    }

    /// `node updates`는 확인 시각, 오류, 패키지 표, 잘렸는지를 보인다. `--refresh`면 다시 확인 Task를
    /// 기다린 뒤 읽는다.
    #[tokio::test]
    async fn node_updates_shows_the_check() {
        fn reply(path: &str) -> Value {
            match path {
                "/api/v1/nodes/n1/updates/refresh" => json!({ "id": "t1" }),
                "/api/v1/tasks/t1" => json!({ "id": "t1", "status": { "state": "succeeded" } }),
                "/api/v1/nodes/n1" => json!({
                    "name": "n1",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000,
                        "packages": [
                            { "name": "oplite", "current": "0.2.0", "candidate": "0.2.1" },
                            { "name": "linux-image-6.12.48+deb13-amd64", "current": null, "candidate": "6.12.48-1" }
                        ],
                        "truncated": true,
                        "error": null
                    } }
                }),
                "/api/v1/nodes/n2" => json!({
                    "name": "n2",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000, "packages": [],
                        "error": "apt-get update -q failed (exit status: 100): E: no network"
                    } }
                }),
                "/api/v1/nodes/n3" => json!({
                    "name": "n3",
                    "status": { "updates": { "checked_at": 1_793_000_000, "packages": [], "error": null } }
                }),
                "/api/v1/nodes/n4" => json!({
                    "name": "n4",
                    "status": { "updates": { "checked_at": null, "packages": [], "error": null } }
                }),
                _ => json!({ "name": "w", "status": { "online": true } }),
            }
        }
        let (client, seen) = fake_api(reply).await;
        let run = |args: &'static [&'static str], json_out: bool| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Updates { node, refresh },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_updates(&client, &node, refresh, json_out, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        let text = run(&["oplctl", "node", "updates", "n1", "--refresh"], false)
            .await
            .unwrap();
        assert_eq!(
            text,
            "checked: 2026-10-26 07:33 UTC\n\
             PACKAGE                          CURRENT  CANDIDATE\n\
             oplite                           0.2.0    0.2.1\n\
             linux-image-6.12.48+deb13-amd64  -        6.12.48-1\n\
             (only the first 2 packages are shown)\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/updates/refresh HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1"
            ]
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n2"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\n\
             error: apt-get update -q failed (exit status: 100): E: no network\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n3"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\nno updates available\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n4"], false)
                .await
                .unwrap(),
            "checked: never\n"
        );
        let err = run(&["oplctl", "node", "updates", "w"], false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "node w does not check for updates");
        let shown: Value = serde_json::from_str(
            &run(&["oplctl", "node", "updates", "n3"], true)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(shown["packages"], json!([]));
    }

    /// `node upgrade`는 업그레이드 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다린다.
    #[tokio::test]
    async fn node_upgrade_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/upgrade" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Upgrade { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_upgrade(&client, &node, wait, &mut out).await.unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1"]).await,
            "upgrade of n1 started (task t1); follow it with `oplctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/upgrade HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1", "--wait"]).await,
            "node n1 upgraded\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/upgrade HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `node maintenance`는 on·off만 받는다. 노드를 읽어 라벨과 spec은 그대로 두고 `maintenance`만 바꿔
    /// 읽은 revision으로(`If-Match`) 다시 쓴다.
    #[tokio::test]
    async fn node_maintenance_puts_the_spec() {
        let run = |args: &'static [&'static str], base: String| async move {
            let Cmd::Node {
                cmd: NodeCmd::Maintenance { node, state },
            } = Cli::try_parse_from(args).unwrap().cmd
            else {
                unreachable!()
            };
            let client = Client::new(&base, None, None).unwrap();
            let mut out = Vec::new();
            let mut err = Vec::new();
            node_maintenance(&client, &node, state == Switch::On, &mut out, &mut err)
                .await
                .unwrap();
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
            String::from_utf8(out).unwrap()
        };
        for args in [
            &["oplctl", "node", "maintenance", "n1"][..],
            &["oplctl", "node", "maintenance", "n1", "yes"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }

        let (base, seen) = fake_api_bodies(|_| {
            json!({
                "name": "n1",
                "revision": 7,
                "labels": { "rack": "a" },
                "spec": { "address": "10.0.0.1:7443", "maintenance": false, "witness": false },
                "status": { "online": true }
            })
        })
        .await;
        assert_eq!(
            run(&["oplctl", "node", "maintenance", "n1", "on"], base.clone()).await,
            "node n1: maintenance on\n"
        );
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2, "{seen:?}");
            assert_eq!(seen[0].line, "GET /api/v1/nodes/n1 HTTP/1.1");
            assert_eq!(seen[1].line, "PUT /api/v1/nodes/n1 HTTP/1.1");
            assert_eq!(seen[1].if_match.as_deref(), Some("7"));
            assert_eq!(
                seen[1].body,
                json!({
                    "labels": { "rack": "a" },
                    "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false }
                })
            );
        }
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "maintenance", "n1", "off"], base).await,
            "node n1: maintenance off\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].body["spec"]["maintenance"], false);
        // 끄면 그 노드의 비우기 Task를 찾아본다(없으면 경고하지 않는다).
        assert_eq!(seen[2].line, "GET /api/v1/tasks HTTP/1.1");
    }

    /// 유지보수를 끄면 그 노드의 끝나지 않은 비우기가 멈춘다고 stderr에 알린다.
    #[tokio::test]
    async fn node_maintenance_off_warns_about_a_running_drain() {
        let (base, _) = fake_api_bodies(|path| {
            if path == "/api/v1/tasks" {
                let drain = |id: &str, node: &str, state: &str| {
                    json!({
                        "id": id,
                        "spec": { "op": { "kind": "node-drain" }, "target": node, "node": node },
                        "status": { "state": state }
                    })
                };
                json!({ "items": [
                    drain("t1", "id-1", "failed"),
                    drain("t2", "id-2", "running"),
                    drain("t3", "id-1", "running"),
                ] })
            } else {
                json!({
                    "id": "id-1",
                    "name": "n1",
                    "revision": 7,
                    "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false },
                    "status": { "online": true }
                })
            }
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        node_maintenance(&client, "n1", false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "node n1: maintenance off\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: the running drain of n1 stops (task t3)\n"
        );
    }

    /// 유지보수는 이미 꺼졌으므로, 비우기 Task를 찾아보지 못해도 경고만 하고 성공한다.
    #[tokio::test]
    async fn node_maintenance_off_succeeds_when_tasks_cannot_be_read() {
        let (client, _) = fake_api_status(|path| {
            if path == "/api/v1/tasks" {
                (
                    500,
                    json!({ "code": "internal", "message": "tasks unavailable" }),
                )
            } else {
                (
                    200,
                    json!({
                        "id": "id-1",
                        "name": "n1",
                        "revision": 7,
                        "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false },
                        "status": { "online": true }
                    }),
                )
            }
        })
        .await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        node_maintenance(&client, "n1", false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "node n1: maintenance off\n"
        );
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.starts_with("warning: cannot check for a running drain: ")
                && err.contains("tasks unavailable"),
            "{err}"
        );
    }

    /// 읽은 뒤 노드가 바뀌어 PUT이 `revision_mismatch`면 다시 읽어 쓴다.
    #[tokio::test]
    async fn node_maintenance_retries_a_revision_mismatch() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            // GET, PUT(409), GET, PUT 차례다.
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                1 => (
                    409,
                    json!({ "code": "revision_mismatch", "message": "node n1 changed" }),
                ),
                _ => (
                    200,
                    json!({
                        "id": "id-1",
                        "name": "n1",
                        "revision": 7,
                        "spec": { "address": "10.0.0.1:7443", "maintenance": false, "witness": false },
                        "status": { "online": true }
                    }),
                ),
            }
        })
        .await;
        let mut out = Vec::new();
        node_maintenance(&client, "n1", true, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "node n1: maintenance on\n");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/nodes/n1 HTTP/1.1",
                "PUT /api/v1/nodes/n1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1",
                "PUT /api/v1/nodes/n1 HTTP/1.1",
            ]
        );
    }

    /// `node drain`은 비우기 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다리고, Task가
    /// 실패하면(옮기지 못한 VM) 그 오류로 끝난다.
    #[tokio::test]
    async fn node_drain_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/drain" => json!({ "id": "t1" }),
            "/api/v1/nodes/n2/drain" => json!({ "id": "t2" }),
            "/api/v1/tasks/t2" => json!({ "id": "t2", "status": {
                "state": "failed",
                "error": "1 VM(s) were not moved: db (disks on a local pool)"
            } }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Drain { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_drain(&client, &node, wait, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "drain", "n1"]).await.unwrap(),
            "drain of n1 started (task t1); follow it with `oplctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/drain HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "drain", "n1", "--wait"])
                .await
                .unwrap(),
            "node n1 drained\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/drain HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        let err = run(&["oplctl", "node", "drain", "n2", "--wait"])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "task t2 failed: 1 VM(s) were not moved: db (disks on a local pool)"
        );
    }

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
    #[test]
    fn settings_set_body_maps_known_keys() {
        assert_eq!(
            settings_set_body(&["updates.channel=testing".into()]).unwrap(),
            json!({ "updates": { "channel": "testing" } })
        );
        let err = settings_set_body(&["updates.mirror=x".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown setting updates.mirror; known: updates.channel"
        );
        let err = settings_set_body(&["updates.channel".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected KEY=VALUE, got \"updates.channel\""
        );
    }

    /// `settings get`은 spec을 `키: 값` 줄로, `-o json`이면 리소스 그대로 보인다. `set`은 PATCH한다.
    #[tokio::test]
    async fn settings_get_and_set() {
        let (client, seen) = fake_api(|_| {
            json!({
                "id": "00000000-0000-0000-0000-000000000000", "name": "cluster", "revision": 0,
                "spec": { "updates": { "channel": "stable" } }, "status": {}
            })
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "settings", "get"][..],
            &["oplctl", "settings", "set", "updates.channel=testing"][..],
        ] {
            let Cmd::Settings { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            settings(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "updates.channel: stable\nsettings updated\n"
        );

        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "get"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let mut out = Vec::new();
        settings(&client, cmd, true, &mut out).await.unwrap();
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["spec"]["updates"]["channel"], "stable");

        // 모르는 키는 요청을 보내지 않는다.
        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "set", "x=1"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        assert!(
            settings(&client, cmd, false, &mut Vec::new())
                .await
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    /// `subscription show`는 상태를 줄로 보인다. 한도가 없으면 쓰는 노드 수만.
    #[test]
    fn subscription_lines_show_the_state() {
        let active = json!({
            "state": "active", "id": "sub-1", "customer": "Example Corp", "level": "premium",
            "nodes": 3, "used_nodes": 2, "expires_at": 1_700_000_000u64, "development": true
        });
        assert_eq!(
            subscription_lines(&active),
            [
                "state: active (development key)",
                "customer: Example Corp",
                "level: premium",
                "nodes: 2/3",
                "expires: 2023-11-14",
            ]
        );
        let expired = json!({
            "state": "expired", "id": "sub-1", "customer": "C", "level": "standard",
            "nodes": 1, "used_nodes": 1, "expires_at": 0, "development": false
        });
        assert_eq!(subscription_lines(&expired)[0], "state: expired");
        assert_eq!(
            subscription_lines(&json!({ "state": "none", "used_nodes": 1 })),
            ["state: none", "nodes: 1"]
        );
    }

    /// `subscription set --file -`은 표준 입력에서 키를 읽는다(앞뒤 공백·줄바꿈을 다듬음).
    #[test]
    fn subscription_key_can_come_from_stdin() {
        let args = ["oplctl", "subscription", "set", "--file", "-"];
        let Cmd::Subscription {
            cmd: SubscriptionCmd::Set { key: None, file },
        } = Cli::try_parse_from(args).unwrap().cmd
        else {
            panic!("parse --file -")
        };
        assert_eq!(file.unwrap().to_str(), Some("-"));
        let mut input = std::io::Cursor::new(" opl1.a.b\r\n");
        assert_eq!(read_stdin_key(&mut input).unwrap(), "opl1.a.b");
    }

    /// `subscription set`은 키(인자나 파일)를 PUT하고 상태를 보인다. `remove`는 DELETE한다.
    #[tokio::test]
    async fn subscription_set_show_and_remove() {
        let (client, seen) = fake_api(|_| {
            json!({ "state": "active", "customer": "C", "level": "standard", "nodes": 2,
                    "used_nodes": 1, "expires_at": 1_700_000_000u64, "development": false })
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "opl1.a.b\r\n").unwrap();
        let file = file.to_str().unwrap();
        let mut out = Vec::new();
        for args in [
            &["oplctl", "subscription", "show"][..],
            &["oplctl", "subscription", "set", "opl1.a.b"][..],
            &["oplctl", "subscription", "set", "--file", file][..],
            &["oplctl", "subscription", "remove"][..],
        ] {
            let Cmd::Subscription { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            subscription(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "DELETE /api/v1/subscription HTTP/1.1",
            ]
        );
        let shown =
            "state: active\ncustomer: C\nlevel: standard\nnodes: 1/2\nexpires: 2023-11-14\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}subscription removed\n")
        );

        // 키나 파일 중 하나만 준다.
        for args in [
            &["oplctl", "subscription", "set"][..],
            &["oplctl", "subscription", "set", "k", "--file", "f"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    /// `telemetry show`는 켜짐·끝점·id·마지막 전송·오류를 줄로 보인다.
    #[test]
    fn telemetry_lines_show_the_state() {
        let settings = json!({
            "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
            "status": { "telemetry": {
                "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last_sent_at": 1_761_471_000, "last_error": "HTTP 500"
            } }
        });
        assert_eq!(
            telemetry_lines(&settings),
            [
                "enabled: true",
                "endpoint: https://t.example/r",
                "id: 6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last sent: 2025-10-26 09:30 UTC",
                "last error: HTTP 500",
            ]
        );
        let default_endpoint = if opl_types::version::TELEMETRY_URL.is_empty() {
            "endpoint: none".to_string()
        } else {
            format!(
                "endpoint: {} (build default)",
                opl_types::version::TELEMETRY_URL
            )
        };
        assert_eq!(
            telemetry_lines(&json!({ "spec": { "updates": {} }, "status": {} })),
            [
                "enabled: false".to_string(),
                default_endpoint,
                "id: none".into(),
                "last sent: never".into(),
                "last error: none".into(),
            ]
        );
    }

    #[test]
    fn telemetry_bodies_send_only_what_was_given() {
        assert_eq!(
            telemetry_body(true, None),
            json!({ "telemetry": { "enabled": true } })
        );
        assert_eq!(
            telemetry_body(true, Some("https://t.example/r")),
            json!({ "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } })
        );
        assert_eq!(
            telemetry_body(false, None),
            json!({ "telemetry": { "enabled": false } })
        );
    }

    /// `telemetry`의 하위 명령이 부르는 API. preview는 받은 JSON을 그대로 예쁘게 보인다.
    #[tokio::test]
    async fn telemetry_commands_call_the_api() {
        let (client, seen) = fake_api(|path| {
            if path == "/api/v1/telemetry/preview" {
                json!({ "schema": 1, "vms": { "total": 2 } })
            } else {
                json!({
                    "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
                    "status": { "telemetry": { "id": "abc" } }
                })
            }
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "telemetry", "show"][..],
            &[
                "oplctl",
                "telemetry",
                "enable",
                "--endpoint",
                "https://t.example/r",
            ][..],
            &["oplctl", "telemetry", "disable"][..],
            &["oplctl", "telemetry", "reset-id"][..],
            &["oplctl", "telemetry", "preview"][..],
        ] {
            let Cmd::Telemetry { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            telemetry(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "POST /api/v1/telemetry/reset-id HTTP/1.1",
                "GET /api/v1/telemetry/preview HTTP/1.1",
            ]
        );
        let shown = "enabled: true\nendpoint: https://t.example/r\nid: abc\nlast sent: never\nlast error: none\n";
        let preview = "{\n  \"schema\": 1,\n  \"vms\": {\n    \"total\": 2\n  }\n}\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}{shown}{preview}")
        );
    }

    /// `--since`의 기간(`30m`·`1h`·`7d`)은 지금에서 뺀 유닉스 밀리초로, 나머지는 그대로 보낸다.
    #[test]
    fn audit_since_turns_durations_into_milliseconds() {
        let now = 1_762_400_000_000;
        assert_eq!(audit_since("30m", now), (now - 30 * 60_000).to_string());
        assert_eq!(audit_since("1h", now), (now - 3_600_000).to_string());
        assert_eq!(audit_since("7d", now), (now - 7 * 86_400_000).to_string());
        assert_eq!(audit_since("90s", now), (now - 90_000).to_string());
        assert_eq!(
            audit_since("2026-11-06T00:00:00Z", now),
            "2026-11-06T00:00:00Z"
        );
        assert_eq!(audit_since("1762400000000", now), "1762400000000");
        assert_eq!(audit_since("xd", now), "xd");
    }

    /// `audit`는 거르기를 쿼리로 보내고 표를 보인다. 읽지 못한 노드와 잘림은 stderr에 알린다.
    /// `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
    #[tokio::test]
    async fn audit_shows_a_table_and_warnings() {
        let (client, seen) = fake_api(|_| {
            json!({
                "entries": [
                    { "id": "019a0000-0000-7000-8000-000000000002", "time": 1_762_387_205_123u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "token", "ip": "10.0.0.5", "method": "POST",
                      "path": "/vms/web-1/actions/start", "action": "POST /vms/{key}/actions/{action}",
                      "target": { "kind": "vms", "key": "web-1" }, "status": 202,
                      "duration_ms": 12, "detail": null },
                    { "id": "019a0000-0000-7000-8000-000000000001", "time": 1_762_387_200_000u64,
                      "node": "node-b", "user": null, "user_id": null, "role": null,
                      "auth": "none", "ip": "10.0.0.9", "method": "POST", "path": "/auth/login",
                      "action": "POST /auth/login", "target": null, "status": 401,
                      "duration_ms": 3, "detail": "username=bob" },
                    { "id": "019a0000-0000-7000-8000-000000000000", "time": 1_762_387_100_000u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "session", "ip": "10.0.0.5", "method": "POST", "path": "/auth/logout",
                      "action": "POST /auth/logout", "target": null, "status": 204,
                      "duration_ms": 1, "detail": null }
                ],
                "unreachable": ["node-c", "node-d"],
                "disabled": ["node-e"],
                "truncated": true
            })
        })
        .await;
        let args = [
            "oplctl",
            "audit",
            "--since",
            "2026-11-06T00:00:00Z",
            "--until",
            "1762400000000",
            "--user",
            "alice",
            "--node",
            "node-a",
            "--action",
            "start",
            "--limit",
            "5",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, false, &mut out, &mut err).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/audit?since=2026-11-06T00%3A00%3A00Z&until=1762400000000\
              &user=alice&node=node-a&action=start&limit=5 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "TIME                  NODE    USER   IP        ACTION                            TARGET        STATUS\n\
             2025-11-06T00:00:05Z  node-a  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202\n\
             2025-11-06T00:00:00Z  node-b  -      10.0.0.9  POST /auth/login                  username=bob  401\n\
             2025-11-05T23:58:20Z  node-a  alice  10.0.0.5  POST /auth/logout                 -             204\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no audit records from: node-c, node-d\n\
             note: the audit log is disabled on: node-e\n\
             note: more entries match; narrow the range or raise --limit\n"
        );

        // 거르기가 없으면 쿼리도 없다(서버 기본값: 지난 24시간, 200개).
        let Cmd::Audit(a) = Cli::try_parse_from(["oplctl", "audit"]).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, true, &mut out, &mut err).await.unwrap();
        assert_eq!(seen.lock().unwrap()[1], "GET /api/v1/audit HTTP/1.1");
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["unreachable"], json!(["node-c", "node-d"]));
        assert_eq!(shown["entries"][1]["detail"], "username=bob");
        assert!(err.is_empty());

        // 기간 --since는 --until과 함께 줄 수 없다(서버에 보내지 않는다).
        let args = [
            "oplctl",
            "audit",
            "--since",
            "1h",
            "--until",
            "1762400000000",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let e = audit(&client, a, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "a relative --since cannot be combined with --until; give a time"
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// `fake_api_bodies`가 남긴 요청
    #[derive(Debug)]
    struct SeenRequest {
        line: String,
        if_match: Option<String>,
        body: Value,
    }

    /// `fake_api`와 같고 요청의 `if-match`와 JSON 본문도 남긴다. 응답에는 세션 쿠키를 붙인다.
    async fn fake_api_bodies(
        reply: fn(&str) -> Value,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<SeenRequest>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = vec![0; 4096];
                // 머리와 content-length만큼의 본문을 다 읽는다.
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "connection closed mid-request");
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let length = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break (head.to_string(), body.to_string());
                    }
                };
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let if_match = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("if-match")
                        .then(|| v.trim().to_string())
                });
                log.lock().unwrap().push(SeenRequest {
                    line,
                    if_match,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let body = reply(&path).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nset-cookie: opl_session=s1; Path=/\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        (base, seen)
    }

    fn realm_cmd(args: &[&str]) -> Result<RealmCmd, clap::Error> {
        let mut all = vec!["oplctl", "realm"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Realm { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `realm add-ldap`은 url·base DN이 필요하고, `--group`은 마지막 `=`에서 나눈다.
    #[test]
    fn parses_realm_commands() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--group",
            "cn=admins,dc=example,dc=com=admin",
            "--group",
            "cn=staff,dc=example,dc=com=Viewer",
        ])
        .unwrap() else {
            panic!("add-ldap")
        };
        assert_eq!(add.ldap.groups.len(), 2);
        assert_eq!(add.ldap.groups[0].0, "cn=admins,dc=example,dc=com");
        assert!(matches!(add.ldap.groups[0].1, Role::Admin));
        assert!(matches!(add.ldap.groups[1].1, Role::Viewer));

        assert_eq!(
            parse_group_role("cn=a,dc=x=root").unwrap_err(),
            "invalid role \"root\" in \"cn=a,dc=x=root\": use admin, operator or viewer"
        );
        assert!(parse_group_role("admins").is_err());
        assert!(parse_group_role("=admin").is_err());

        for args in [
            // url·base DN이 없다.
            &["add-ldap", "corp", "--url", "ldaps://h"][..],
            &["add-ldap", "corp", "--base-dn", "dc=x"][..],
            // 비밀번호를 주면서 지울 수는 없다.
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-password-file",
                "f",
            ][..],
            // 지우기와 설정은 함께 줄 수 없다.
            &["set-ldap", "corp", "--clear-bind-dn", "--bind-dn", "cn=x"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-dn",
                "--bind-password-file",
                "f",
            ][..],
            &["set-ldap", "corp", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &[
                "set-ldap",
                "corp",
                "--clear-groups",
                "--group",
                "cn=x=admin",
            ][..],
            // 비밀번호 파일은 사용자와 함께
            &["test", "corp", "--password-file", "f"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "corp", "--user", "alice"]).unwrap()
        else {
            panic!("test")
        };
        assert_eq!(user.as_deref(), Some("alice"));
    }

    /// `add-ldap`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_body_sends_only_what_is_given() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldap://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_body(&add, LdapFiles::default()),
            json!({
                "name": "corp",
                "backend": { "kind": "ldap", "url": "ldap://ldap.example.com", "base_dn": "dc=example,dc=com" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--bind-dn",
            "cn=svc,dc=example,dc=com",
            "--user-filter",
            "(sAMAccountName={username})",
            "--group-attr",
            "isMemberOf",
            "--timeout",
            "5",
            "--display",
            "Corp",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("svc-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_body(&add, files),
            json!({
                "name": "corp",
                "display": "Corp",
                "backend": {
                    "kind": "ldap", "url": "ldaps://ldap.example.com", "base_dn": "dc=example,dc=com",
                    "bind_dn": "cn=svc,dc=example,dc=com", "bind_password": "svc-pw",
                    "user_filter": "(sAMAccountName={username})", "group_attr": "isMemberOf",
                    "ca_pem": "PEM", "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000001", "name": "corp", "revision": 4,
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 10, "has_bind_password": true
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-ldap`은 지금 영역에 준 것만 바꾼다. 비밀번호는 주지 않으면 빼고(서버가 둔다),
    /// `--clear-bind-password`면 빈 글이다. `--group`을 주면 표 전체를 바꾼다.
    #[test]
    fn realm_set_body_changes_only_what_is_given() {
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        let kept = json!({
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 3
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        });
        assert_eq!(
            realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap(),
            kept
        );

        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--url",
            "ldap://other.example.com",
            "--clear-bind-dn",
            "--clear-ca",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "admin",
            "--display",
            "Corporate",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        let body = realm_set_body(&with_ca, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["display"], "Corporate");
        assert_eq!(body["backend"]["url"], "ldap://other.example.com");
        // --clear-bind-dn은 저장된 비밀번호도 지운다.
        assert_eq!(body["backend"]["bind_password"], "");
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
        assert!(body["backend"].get("ca_pem").is_none(), "{body}");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                "default": "admin"
            })
        );

        // 새 비밀번호와 CA
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("new-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_body(&sample_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["bind_password"], "new-pw");
        assert_eq!(body["backend"]["ca_pem"], "PEM");

        // 그룹 표와 기본 역할 지우기
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-groups", "--clear-default-role"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert_eq!(body["roles"], json!({ "groups": [] }));

        // 서비스 계정 DN이 남는데 비밀번호만 지울 수는 없다(새 DN을 줘도).
        for args in [
            &["set-ldap", "corp", "--clear-bind-password"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-dn",
                "cn=other",
            ][..],
        ] {
            let RealmCmd::SetLdap(set) = realm_cmd(args).unwrap() else {
                unreachable!()
            };
            let e = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap_err();
            assert_eq!(
                e.to_string(),
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
        // DN이 없는 영역이나 DN도 지우면 된다.
        let mut anon = sample_realm();
        anon["backend"].as_object_mut().unwrap().remove("bind_dn");
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-bind-password"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&anon, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["backend"]["bind_password"], "");
        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--clear-bind-password",
            "--clear-bind-dn",
        ])
        .unwrap() else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
    }

    /// url·bind_dn·CA를 바꾸면서 비밀번호를 주지 않으면 서버의 400을 그대로 보인다.
    #[tokio::test]
    async fn realm_set_shows_the_server_refusal() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                (200, sample_realm())
            } else {
                (
                    400,
                    json!({
                        "code": "invalid_request",
                        "message": "bind_password must be given again when url, bind_dn or ca_pem changes"
                    }),
                )
            }
        })
        .await;
        let e = realm(
            &client,
            realm_cmd(&["set-ldap", "corp", "--url", "ldaps://other.example.com"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("bind_password must be given again when url, bind_dn or ca_pem changes"),
            "{e}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1"
            ]
        );
    }

    #[test]
    fn realm_show_lines() {
        assert_eq!(
            realm_lines(&sample_realm()).join("\n"),
            "name: corp\n\
             display: Corp\n\
             kind: ldap\n\
             url: ldaps://ldap.example.com\n\
             base dn: dc=example,dc=com\n\
             bind dn: cn=svc,dc=example,dc=com\n\
             bind password: set\n\
             user filter: (uid={username})\n\
             group attribute: memberOf\n\
             ca: system roots\n\
             timeout: 10s\n\
             groups:\n  cn=admins,dc=example,dc=com = admin\n\
             default role: viewer"
        );
        let mut bare = sample_realm();
        bare["backend"]["has_bind_password"] = json!(false);
        bare["backend"]["ca_pem"] = json!("PEM");
        bare["roles"] = json!({ "groups": [] });
        let lines = realm_lines(&bare);
        for line in [
            "bind password: none",
            "ca: custom (PEM)",
            "groups: none",
            "default role: none",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// 영역 명령이 부르는 API: 파일의 비밀번호·CA는 본문에, `set-ldap`은 읽은 revision을
    /// If-Match로, `test`는 단계 표를 보이고 실패한 단계가 있으면 오류다.
    #[tokio::test]
    async fn realm_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_realm()] }),
            "/api/v1/realms/corp/actions/test" => json!({
                "steps": [
                    { "name": "connect", "ok": true, "message": "connected to ldaps://ldap.example.com" },
                    { "name": "service_bind", "ok": true, "message": "bound as cn=svc,dc=example,dc=com" },
                    { "name": "search", "ok": true, "message": "found uid=alice,dc=example,dc=com" },
                    { "name": "user_bind", "ok": true, "message": "the password is correct" },
                    { "name": "groups", "ok": true, "message": "2 groups" }
                ],
                "dn": "uid=alice,dc=example,dc=com",
                "groups": ["cn=admins,dc=example,dc=com", "cn=staff,dc=example,dc=com"],
                "role": "admin"
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "connect", "ok": false, "message": "connection refused" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("pw");
        std::fs::write(&secret, "svc-pw\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-ldap",
                "corp",
                "--url",
                "ldaps://ldap.example.com",
                "--base-dn",
                "dc=example,dc=com",
                "--bind-dn",
                "cn=svc,dc=example,dc=com",
                "--bind-password-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-ldap", "corp", "--bind-password-file", secret][..],
            &["remove", "corp"][..],
            &["test", "corp", "--user", "alice", "--password-file", secret][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken", "--user", "a", "--password-file", secret]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the connect step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1",
                "DELETE /api/v1/realms/corp HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "POST /api/v1/realms/corp/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["bind_password"], "svc-pw");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("4"));
        assert_eq!(seen[3].body["backend"]["bind_password"], "svc-pw");
        assert!(seen[3].body["backend"].get("has_bind_password").is_none());
        assert_eq!(
            seen[6].body,
            json!({ "username": "alice", "password": "svc-pw" })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             corp  Corp     ldap  ldaps://ldap.example.com\n\
             realm corp created\n\
             realm corp updated\n\
             realm corp removed\n\
             STEP          RESULT  MESSAGE\n\
             connect       ok      connected to ldaps://ldap.example.com\n\
             service_bind  ok      bound as cn=svc,dc=example,dc=com\n\
             search        ok      found uid=alice,dc=example,dc=com\n\
             user_bind     ok      the password is correct\n\
             groups        ok      2 groups\n\
             dn: uid=alice,dc=example,dc=com\n\
             groups: cn=admins,dc=example,dc=com, cn=staff,dc=example,dc=com\n\
             role: admin\n"
        );
    }

    /// `login --realm`은 로그인 본문에 영역을 넣고(local이면 넣지 않음) 서버가 정한 이름을 돌려준다.
    #[tokio::test]
    async fn login_sends_the_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/login" => json!({ "name": "alice@corp", "role": "admin" }),
            "/api/v1/tokens" => json!({ "secret": "opl_t_s" }),
            _ => json!({}),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "alice-pw\n").unwrap();
        let pw = pw.to_str().unwrap();
        for (realm, expected) in [(Some("corp"), Some("corp")), (None, None)] {
            let mut args = vec!["oplctl", "login", "--user", "Alice", "--password-file", pw];
            if let Some(realm) = realm {
                args.extend(["--realm", realm]);
            }
            let Cmd::Login(args) = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            assert_eq!(
                login(&base, &args, None).await.unwrap(),
                ("opl_t_s".to_string(), "alice@corp".to_string())
            );
            let seen = seen.lock().unwrap();
            let sent = &seen[seen.len() - 3];
            assert_eq!(sent.line, "POST /api/v1/auth/login HTTP/1.1");
            assert_eq!(sent.body["username"], "Alice");
            assert_eq!(sent.body["password"], "alice-pw");
            assert_eq!(sent.body.get("realm").and_then(Value::as_str), expected);
        }
    }

    /// `realm add-oidc`는 issuer·client ID가 필요하고, 지우기와 설정은 함께 줄 수 없다.
    /// `test`는 사용자 없이도 된다(OIDC).
    #[test]
    fn parses_oidc_realm_commands() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--group",
            "opl-admins=admin",
        ])
        .unwrap() else {
            panic!("add-oidc")
        };
        assert_eq!(add.oidc.scopes, ["openid", "groups"]);
        assert_eq!(add.oidc.groups[0].0, "opl-admins");
        assert!(matches!(add.oidc.groups[0].1, Role::Admin));

        for args in [
            &["add-oidc", "okta", "--issuer", "https://idp"][..],
            &["add-oidc", "okta", "--client-id", "oplite"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-client-secret",
                "--client-secret-file",
                "f",
            ][..],
            &["set-oidc", "okta", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &["set-oidc", "okta", "--clear-groups", "--group", "g=admin"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "okta"]).unwrap() else {
            panic!("test")
        };
        assert_eq!(user, None);
    }

    /// `add-oidc`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_oidc_body_sends_only_what_is_given() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_oidc_body(&add, OidcFiles::default()),
            json!({
                "name": "okta",
                "backend": { "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com/oauth2/default",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--timeout",
            "5",
            "--display",
            "Okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("s3cret".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_oidc_body(&add, files),
            json!({
                "name": "okta",
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com/oauth2/default",
                    "client_id": "oplite", "client_secret": "s3cret", "scopes": ["openid", "groups"],
                    "username_claim": "email", "groups_claim": "roles", "ca_pem": "PEM",
                    "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "opl-ops", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_oidc_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000002", "name": "okta", "revision": 9,
            "display": "Okta",
            "backend": {
                "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                "groups_claim": "groups", "timeout_secs": 10, "has_client_secret": true
            },
            "roles": {
                "groups": [{ "group": "opl-admins", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-oidc`는 지금 영역에 준 것만 바꾼다. 비밀은 주지 않으면 빼고(서버가 둔다),
    /// `--clear-client-secret`면 빈 글이다. 다른 종류의 영역은 바꾸지 않는다.
    #[test]
    fn realm_set_oidc_body_changes_only_what_is_given() {
        let RealmCmd::SetOidc(set) = realm_cmd(&["set-oidc", "okta", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            realm_set_oidc_body(&sample_oidc_realm(), &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                    "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                    "groups_claim": "groups", "timeout_secs": 3
                },
                "roles": {
                    "groups": [{ "group": "opl-admins", "role": "admin" }],
                    "default": "viewer"
                }
            })
        );

        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--issuer",
            "https://other.example.com",
            "--client-id",
            "opl2",
            "--scope",
            "openid",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--clear-client-secret",
            "--clear-ca",
            "--clear-groups",
            "--clear-default-role",
            "--display",
            "Other",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_oidc_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        assert_eq!(
            realm_set_oidc_body(&with_ca, &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Other",
                "backend": {
                    "kind": "oidc", "issuer": "https://other.example.com", "client_id": "opl2",
                    "client_secret": "", "scopes": ["openid"], "username_claim": "email",
                    "groups_claim": "roles", "timeout_secs": 10
                },
                "roles": { "groups": [] }
            })
        );

        // 새 비밀·CA와 그룹 표
        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "admin",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("new-secret".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_oidc_body(&sample_oidc_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["client_secret"], "new-secret");
        assert_eq!(body["backend"]["ca_pem"], "PEM");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "opl-ops", "role": "operator" }],
                "default": "admin"
            })
        );

        // 종류가 다른 영역
        let e = realm_set_oidc_body(&sample_realm(), &set, OidcFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm okta is not an OIDC realm (its kind is ldap)"
        );
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let e = realm_set_body(&sample_oidc_realm(), &set, LdapFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm corp is not an LDAP realm (its kind is oidc)"
        );
    }

    #[test]
    fn realm_show_lines_for_oidc() {
        assert_eq!(
            realm_lines(&sample_oidc_realm()).join("\n"),
            "name: okta\n\
             display: Okta\n\
             kind: oidc\n\
             issuer: https://idp.example.com\n\
             client id: oplite\n\
             client secret: set\n\
             scopes: openid profile email\n\
             username claim: preferred_username\n\
             groups claim: groups\n\
             ca: system roots\n\
             timeout: 10s\n\
             callback path: /api/v1/auth/oidc/callback\n\
             groups:\n  opl-admins = admin\n\
             default role: viewer"
        );
        let mut public = sample_oidc_realm();
        public["backend"]["has_client_secret"] = json!(false);
        public["backend"]["ca_pem"] = json!("PEM");
        let lines = realm_lines(&public);
        for line in ["client secret: none", "ca: custom (PEM)"] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// OIDC 영역 명령이 부르는 API: 목록의 URL 칸은 issuer, 파일의 비밀·CA는 본문에,
    /// `set-oidc`는 읽은 revision을 If-Match로, `test`는 영역 종류를 읽고 입력 없이 단계 표만 보인다
    /// (`--user`를 줘도 비밀번호를 묻지 않고 DN·그룹·역할 줄이 없다).
    #[tokio::test]
    async fn realm_oidc_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_oidc_realm()] }),
            "/api/v1/realms/okta/actions/test" => json!({
                "steps": [
                    { "name": "discovery", "ok": true, "message": "read https://idp.example.com/.well-known/openid-configuration" },
                    { "name": "jwks", "ok": true, "message": "2 signing keys (RS256 1, ES256 1)" }
                ],
                "dn": null, "groups": [], "role": null
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "discovery", "ok": false, "message": "issuer differs" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_oidc_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-oidc",
                "okta",
                "--issuer",
                "https://idp.example.com",
                "--client-id",
                "oplite",
                "--client-secret-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-oidc", "okta", "--client-secret-file", secret][..],
            &["test", "okta", "--user", "alice"][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the discovery step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "PUT /api/v1/realms/okta HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "POST /api/v1/realms/okta/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["kind"], "oidc");
        assert_eq!(seen[1].body["backend"]["client_secret"], "s3cret");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("9"));
        assert_eq!(seen[3].body["backend"]["client_secret"], "s3cret");
        assert!(seen[3].body["backend"].get("has_client_secret").is_none());
        assert_eq!(seen[5].body, json!({}));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             okta  Okta     oidc  https://idp.example.com\n\
             realm okta created\n\
             realm okta updated\n\
             STEP       RESULT  MESSAGE\n\
             discovery  ok      read https://idp.example.com/.well-known/openid-configuration\n\
             jwks       ok      2 signing keys (RS256 1, ES256 1)\n"
        );
    }

    /// `login --realm <OIDC 영역>`은 비밀번호를 묻기 전에 웹 UI로 보낸다.
    #[tokio::test]
    async fn login_refuses_an_oidc_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/realms" => json!([
                { "name": "local", "display": null, "kind": "local" },
                { "name": "corp", "display": null, "kind": "ldap" },
                { "name": "okta", "display": "Okta", "kind": "oidc" }
            ]),
            _ => json!({}),
        })
        .await;
        // 비밀번호 파일이 없으므로 묻는다면 터미널이 없어 다른 오류가 난다.
        let Cmd::Login(args) = Cli::try_parse_from(["oplctl", "login", "--realm", "okta"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let e = login(&base, &args, None).await.unwrap_err();
        assert_eq!(
            e.to_string(),
            "sign in to okta in the web UI, create an API token there and run `oplctl login --token-file`"
        );
        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["GET /api/v1/auth/realms HTTP/1.1"]);
    }

    /// `login --token-file`은 비밀번호 로그인 인자와 함께 줄 수 없다.
    #[test]
    fn token_file_login_conflicts_with_password_login() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["oplctl", "login", "--token-file", "t"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args).map(|_| ())
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--ca-fingerprint", "sha256:ab"]).is_ok());
        for extra in [
            &["--user", "bob"][..],
            &["--realm", "okta"][..],
            &["--password-file", "pw"][..],
        ] {
            let e = parse(extra).unwrap_err();
            assert_eq!(
                e.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{extra:?}"
            );
        }
    }

    /// `login --token-file`: 토큰으로 whoami를 불러 확인하고 저장한다. 같은 토큰을 다시 넣으면 지우지 않고,
    /// 이전 토큰은 login이 만든 것(`oplctl-…`)만 지운다. 받지 않는 토큰은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_checks_and_saves_the_token() {
        let ids = [
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
        ];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[2] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[2] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/auth/whoami" => (
                200,
                json!({ "name": "alice@corp.com@okta", "role": "admin" }),
            ),
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        let login_with = async |token: &str| {
            std::fs::write(&file, format!("{token}\n")).unwrap();
            let mut out = Vec::new();
            let args = [
                "oplctl",
                "--config",
                config.to_str().unwrap(),
                "--server",
                client.base.as_str(),
                "login",
                "--token-file",
                file.to_str().unwrap(),
            ];
            run(args, &mut out)
                .await
                .map(|()| String::from_utf8(out).unwrap())
        };
        let saved = || CliConfig::load(&config).unwrap().token;
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)
        assert_eq!(
            login_with(&tokens[0]).await.unwrap(),
            format!("logged in to {} as alice@corp.com@okta\n", client.base)
        );
        assert_eq!(saved().as_deref(), Some(tokens[0].as_str()));
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 같은 토큰을 다시 넣어도 지우지 않는다.
        login_with(&tokens[0]).await.unwrap();
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 다른 토큰: 이전 토큰(laptop)은 login이 만든 것이 아니므로 두다.
        login_with(&tokens[1]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1",
                "GET /api/v1/tokens HTTP/1.1"
            ]
        );
        // 이전 토큰(oplctl-…)은 지운다.
        login_with(&tokens[2]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1".to_string(),
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
        assert_eq!(saved().as_deref(), Some(tokens[2].as_str()));
    }

    /// 서버가 받지 않는 토큰(401)은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_does_not_save_a_refused_token() {
        let (client, seen) = fake_api_status(|_| {
            (
                401,
                json!({ "code": "unauthorized", "message": "invalid token" }),
            )
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        std::fs::write(&file, "opl_bad_token\n").unwrap();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "login",
            "--token-file",
            file.to_str().unwrap(),
        ];
        let e = run(args, &mut Vec::new()).await.unwrap_err();
        assert_eq!(e.to_string(), "the API token was not accepted");
        assert!(!config.exists());
        assert_eq!(*seen.lock().unwrap(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
    }

    /// 시험 서버 쪽에서 경로 조각의 퍼센트 인코딩을 푼다.
    fn percent_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    /// `vm stop --force`는 동작 뒤의 쿼리를 그대로 보낸다(VM 이름만 인코딩).
    #[tokio::test]
    async fn vm_stop_force_keeps_the_query() {
        let (client, seen) =
            fake_api(|_| json!({ "id": "t1", "status": { "state": "succeeded" } })).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "vm",
            "stop",
            "web 1",
            "--force",
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "vm web 1: stop done\n");
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines[0],
            "POST /api/v1/vms/web%201/actions/stop?mode=force HTTP/1.1"
        );
    }

    /// 이름을 경로에 넣을 때 퍼센트 인코딩해, `+`·`@`가 든 OIDC 사용자 이름도 서버에서 같은 이름으로 풀린다.
    #[tokio::test]
    async fn user_delete_encodes_the_name_in_the_path() {
        let (client, seen) = fake_api(|_| json!({})).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let name = "alice+ops@corp.com@okta";
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "user",
            "delete",
            name,
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("user {name} deleted\n")
        );
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines,
            ["DELETE /api/v1/users/alice%2Bops%40corp.com%40okta HTTP/1.1"]
        );
        let path = lines[0].split(' ').nth(1).unwrap();
        assert_eq!(
            percent_decode(path.strip_prefix("/api/v1/users/").unwrap()),
            name
        );
    }

    /// `logout`은 login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 웹 UI에서 만든 토큰은 로컬 설정에서만
    /// 지우고 서버에서는 그대로라고 알린다.
    #[tokio::test]
    async fn logout_deletes_only_tokens_made_by_login() {
        let ids = [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[1] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[1] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let logout_with = async |token: &str| {
            CliConfig {
                server: Some(client.base.clone()),
                token: Some(token.into()),
                ca_pem: None,
            }
            .save(&config)
            .unwrap();
            let mut out = Vec::new();
            let args = ["oplctl", "--config", config.to_str().unwrap(), "logout"];
            run(args, &mut out).await.unwrap();
            assert_eq!(CliConfig::load(&config).unwrap().token, None);
            String::from_utf8(out).unwrap()
        };
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)은 서버에서 지우지 않는다.
        assert_eq!(
            logout_with(&tokens[0]).await,
            "logged out\nthe token stays valid on the server; delete it in the web UI\n"
        );
        assert_eq!(lines(), ["GET /api/v1/tokens HTTP/1.1"]);
        // login이 만든 토큰은 지운다.
        assert_eq!(logout_with(&tokens[1]).await, "logged out\n");
        assert_eq!(
            lines(),
            [
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    fn remote_cmd(args: &[&str]) -> Result<RemoteCmd, clap::Error> {
        let mut all = vec!["oplctl", "remote"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Remote { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `remote add`는 주소와 토큰 파일이 필요하다. `remote vm`의 stop은 로컬 `vm stop`처럼 ACPI이고
    /// `--force`면 곧바로 끈다.
    #[test]
    fn parses_remote_commands() {
        let RemoteCmd::Add(add) = remote_cmd(&[
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
            "--yes",
        ])
        .unwrap() else {
            panic!("not add");
        };
        assert_eq!(add.url, "https://10.0.0.5:8443");
        assert_eq!(add.token_file, PathBuf::from("-"));
        assert!(add.yes && add.ca_fingerprint.is_none());
        assert!(remote_cmd(&["add", "dr", "--url", "https://h"]).is_err());
        assert!(remote_cmd(&["add", "dr", "--token-file", "t"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "reboot", "web"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "shutdown", "web"]).is_err());
        let actions: Vec<_> = [
            &["start"][..],
            &["stop"][..],
            &["stop", "--force"][..],
            &["start", "--force"][..],
        ]
        .iter()
        .map(|a| {
            let mut args = vec!["vm", "dr"];
            args.extend_from_slice(&a[..1]);
            args.push("web");
            args.extend_from_slice(&a[1..]);
            match remote_cmd(&args).unwrap() {
                RemoteCmd::Vm { action, force, .. } => {
                    remote_vm_action(action, force).map_err(|e| e.to_string())
                }
                _ => unreachable!(),
            }
        })
        .collect();
        assert_eq!(
            actions,
            [
                Ok(("start", "start")),
                Ok(("stop", "stop")),
                Ok(("stop?mode=force", "force stop")),
                Err("--force is only for stop".to_string()),
            ]
        );
    }

    /// 토큰을 표준 입력으로 받으면 지문 확인을 물을 수 없으므로 `--ca-fingerprint`나 `--yes`가 없으면 곧바로
    /// 실패한다(표준 입력을 읽거나 원격에 연결하지 않고).
    #[tokio::test]
    async fn remote_add_from_stdin_needs_a_fingerprint_or_yes() {
        let client = Client::new("http://127.0.0.1:1", None, None).unwrap();
        let args = [
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
        ];
        let err = remote(&client, remote_cmd(&args).unwrap(), false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "reading the token from stdin needs --ca-fingerprint or --yes"
        );
    }

    /// 원격이 저장한 토큰을 거절하면 중계가 502 `remote_unauthorized`를 주므로 그 메시지를 보이고, 이
    /// 클러스터에 다시 로그인하라고 하지 않는다.
    #[tokio::test]
    async fn a_refused_remote_token_is_not_a_login_problem() {
        let message = "the remote cluster refused the stored token; update it with `oplctl remote set --token-file`";
        let (client, _) = fake_api_status(move |_| {
            (
                502,
                json!({ "code": "remote_unauthorized", "message": message }),
            )
        })
        .await;
        let err = remote(
            &client,
            remote_cmd(&["vms", "dr"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(err, format!("{message} (remote_unauthorized)"));
        assert!(!err.contains("oplctl login"), "{err}");
    }

    /// 받은 CA 지문은 보이고, `--yes`가 아니면 터미널에서 묻는다. 터미널이 아니면 지문을 달라고 한다.
    #[test]
    fn remote_fingerprint_needs_a_confirmation() {
        let fp = "sha256:ab";
        let confirm = |yes: bool, terminal: bool, answer: &str| {
            let mut out = Vec::new();
            let r = confirm_fingerprint(fp, yes, terminal, &mut answer.as_bytes(), &mut out);
            (
                r.map_err(|e| e.to_string()),
                String::from_utf8(out).unwrap(),
            )
        };
        let shown = "the remote cluster CA fingerprint is sha256:ab\n";
        assert_eq!(confirm(true, false, ""), (Ok(()), shown.to_string()));
        let (r, out) = confirm(false, false, "y\n");
        assert_eq!(out, shown);
        assert!(
            r.unwrap_err()
                .contains("run again with --ca-fingerprint sha256:ab"),
            "not a terminal"
        );
        let prompt = format!("{shown}Is this the CA of the remote cluster? [y/N] ");
        assert_eq!(confirm(false, true, "Yes\n"), (Ok(()), prompt.clone()));
        assert_eq!(
            confirm(false, true, "\n"),
            (Err("cancelled".to_string()), prompt)
        );
    }

    fn sample_remote() -> Value {
        json!({
            "id": "0190b9d4-0000-7000-8000-000000000001",
            "name": "dr",
            "revision": 9,
            "url": "https://10.0.0.5:8443",
            "ca_fingerprint": "sha256:aa",
            "token_id": "0190b9d4-0000-7000-8000-000000000002",
            "status": {
                "reachable": true,
                "checked_at": 1_793_000_000,
                "error": null,
                "cluster_name": "dr-site",
                "version": "0.37.0",
                "nodes": { "total": 3, "online": 2 },
                "vms": { "total": 5, "running": 4 },
                "cpus": 48,
                "memory_gib": 192
            }
        })
    }

    /// 목록의 노드·VM 칸은 `online/total`·`running/total`이고, 닿지 않는 원격은 이유를 보인다.
    #[test]
    fn remote_rows_and_lines() {
        assert_eq!(
            remote_row(&sample_remote()),
            ["dr", "dr-site", "0.37.0", "2/3", "4/5", "reachable"]
        );
        let down = json!({
            "name": "old",
            "status": { "reachable": false, "error": "connection refused" }
        });
        assert_eq!(
            remote_row(&down),
            ["old", "-", "-", "-", "-", "unreachable: connection refused"]
        );
        let lines = remote_lines(&sample_remote());
        for line in [
            "url: https://10.0.0.5:8443",
            "ca fingerprint: sha256:aa",
            "token id: 0190b9d4-0000-7000-8000-000000000002",
            "status: reachable",
            "nodes online: 2/3",
            "vms running: 4/5",
            "cpus: 48",
            "memory: 192 GiB",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
        assert!(
            remote_lines(&down).contains(&"changed at: never".to_string()),
            "{:?}",
            remote_lines(&down)
        );
    }

    /// 원격 명령이 부르는 API: `add`는 토큰 파일과 준 지문을, `set`은 읽은 revision을 If-Match로 하고
    /// 토큰을 주지 않으면 빼서 보낸다. `vms`·`vm`은 중계 경로로 원격 API를 부른다.
    #[tokio::test]
    async fn remote_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes/dr/api/vms" => json!({
                "revision": 3,
                "items": [{
                    "name": "web",
                    "spec": { "power": "running", "ha": false },
                    "status": { "phase": "running", "node": "n1", "pid": 42 }
                }]
            }),
            "/api/v1/remotes/dr/api/vms/web/actions/stop?mode=force" => {
                json!({ "id": "t1", "status": { "state": "pending" } })
            }
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        let secret = format!("opl_{}_s3cret", uuid::Uuid::now_v7().simple());
        std::fs::write(&token, format!("{secret}\n")).unwrap();
        let token = token.to_str().unwrap();

        let mut out = Vec::new();
        for args in [
            &[
                "add",
                "dr",
                "--url",
                "https://10.0.0.5:8443",
                "--ca-fingerprint",
                "sha256:aa",
                "--token-file",
                token,
            ][..],
            &["set", "dr", "--url", "https://10.0.0.6:8443"][..],
            &["set", "dr", "--token-file", token][..],
            &["remove", "dr"][..],
            &["vms", "dr"][..],
            &["vm", "dr", "stop", "web", "--force"][..],
        ] {
            remote(&client, remote_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "POST /api/v1/remotes HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "DELETE /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr/api/vms HTTP/1.1",
                "POST /api/v1/remotes/dr/api/vms/web/actions/stop?mode=force HTTP/1.1",
            ]
        );
        assert_eq!(
            seen[0].body,
            json!({
                "name": "dr",
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(seen[2].if_match.as_deref(), Some("9"));
        assert_eq!(
            seen[2].body,
            json!({ "url": "https://10.0.0.6:8443", "ca_fingerprint": "sha256:aa" })
        );
        assert_eq!(
            seen[4].body,
            json!({
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "remote dr added (reachable)\n\
             remote dr updated\n\
             remote dr updated\n\
             remote dr removed\n\
             NAME  POWER    PHASE    NODE  HA     PID  ERROR\n\
             web   running  running  n1    false  42   -\n\
             vm web on remote dr: force stop requested (task t1)\n"
        );
    }

    /// `remote list`는 원격마다 한 줄, `show`·`refresh`는 `키: 값` 줄이다.
    #[tokio::test]
    async fn remote_list_and_refresh() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes" => json!({ "revision": 9, "items": [sample_remote()] }),
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        remote(&client, remote_cmd(&["list"]).unwrap(), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  CLUSTER  VERSION  NODES  VMS  STATUS\n\
             dr    dr-site  0.37.0   2/3    4/5  reachable\n"
        );
        let mut out = Vec::new();
        remote(
            &client,
            remote_cmd(&["refresh", "dr"]).unwrap(),
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            remote_lines(&sample_remote()).join("\n") + "\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/remotes HTTP/1.1",
                "POST /api/v1/remotes/dr/actions/refresh HTTP/1.1",
            ]
        );
    }
}
