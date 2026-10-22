//! opLite 노드 데몬. 클러스터 저장소, 노드 간 통신, reconciler, Task 실행기, REST API를 한 프로세스로
//! 묶는다.
//!
//! 노드 인증서(`{data_dir}/pki`)가 없으면 아직 클러스터에 속하지 않은 노드다. 이때는 제어 소켓만 열고
//! `oplctl cluster init`(새 클러스터)이나 `oplctl cluster join`(합류)을 기다린다. 멤버가 되면 모든
//! 것을 띄우고, `oplctl cluster leave`로 빠지면 다시 기다린다.
//!
//! QEMU는 `-daemonize`로 떠 있으므로 opld가 멈추거나 죽어도 VM은 계속 돈다. 다시 시작하면
//! 하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙고 reconciler가 이어서 맞춘다.

mod bootstrap;
mod config;
mod control;
mod host;
mod join;
mod member;

use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use opl_api::cluster::JoinUrl;
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use serde_json::json;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::config::Config;
use crate::control::{ControlError, Request, Status};
use crate::member::{Bootstrap, Member};

#[derive(Parser)]
#[command(version, about = "opLite node daemon")]
struct Args {
    #[arg(long, default_value = "/etc/opl/opld.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,openraft=warn")),
        )
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    run(config).await
}

async fn run(config: Config) -> anyhow::Result<()> {
    prepare_data_dir(&config.data_dir)?;
    // 끝날 때까지 쥐고 있는다.
    let _lock = lock_data_dir(&config.data_dir)?;
    let node = bootstrap::node_id(&config.data_dir)?;
    let status = Arc::new(Mutex::new(Status {
        node,
        name: config.name.clone(),
        state: "uninitialized",
        cluster: None,
    }));
    let (requests, mut control) = mpsc::channel(4);
    let control_task = control::serve(&config.control_socket(), requests, status.clone()).await?;
    let mut shutdown = pin!(shutdown_signal());

    let result = async {
        'node: loop {
            // 멤버가 아니면 init이나 join을 기다린다.
            let mut member = match NodeTls::load(&member::pki_dir(&config))? {
                Some(tls) => Member::start(&config, node, tls, Bootstrap::Restart).await?.0,
                None => {
                    tracing::info!(
                        socket = %config.control_socket().display(),
                        "this node is not in a cluster; waiting for `oplctl cluster init` or `oplctl cluster join`"
                    );
                    loop {
                        let req = tokio::select! {
                            () = &mut shutdown => return anyhow::Ok(()),
                            req = control.recv() => req,
                        };
                        let Some(req) = req else { return Ok(()) };
                        if let Some(member) = become_member(&config, node, req).await {
                            break member;
                        }
                        // 인증서를 저장한 뒤 실패했으면 이미 멤버(learner)다. 멤버로 다시 띄워 승격을 이어 간다.
                        if member::pki_dir(&config).exists() {
                            continue 'node;
                        }
                    }
                }
            };
            let mut cluster_name = read_cluster_name(&member);
            {
                let mut s = status.lock().unwrap();
                s.state = "member";
                s.cluster = cluster_name.clone();
            }
            // 멤버로 돈다.
            let left = loop {
                tokio::select! {
                    () = &mut shutdown => {
                        tracing::info!("shutting down; running VMs keep running");
                        member.close(false).await?;
                        return Ok(());
                    }
                    e = member.failed() => {
                        let _ = member.close(false).await;
                        return Err(e);
                    }
                    // learner로 다시 시작하면 저장소가 비어 있어 리더에게서 받을 때까지 이름을 모른다.
                    () = tokio::time::sleep(Duration::from_secs(1)), if cluster_name.is_none() => {
                        cluster_name = read_cluster_name(&member);
                        status.lock().unwrap().cluster = cluster_name.clone();
                    }
                    req = control.recv() => match req {
                        Some(Request::Leave { reply }) => match member.leave().await {
                            Ok(()) => break reply,
                            Err(e) => {
                                let _ = reply.send(Err(ControlError::failed(e)));
                            }
                        },
                        Some(Request::Init { reply, .. } | Request::Join { reply, .. }) => {
                            let message = format!(
                                "this node is already a member of cluster {:?}",
                                cluster_name.as_deref().unwrap_or("?")
                            );
                            let _ = reply.send(Err(ControlError::conflict(message)));
                        }
                        None => {
                            member.close(false).await?;
                            return Ok(());
                        }
                    },
                }
            };
            let closed = member.close(true).await;
            {
                let mut s = status.lock().unwrap();
                s.state = "uninitialized";
                s.cluster = None;
            }
            match closed {
                Ok(()) => {
                    tracing::info!("left the cluster");
                    let _ = left.send(Ok(json!({})));
                }
                Err(e) => {
                    let _ = left.send(Err(ControlError::failed(e)));
                }
            }
        }
    }
    .await;
    control_task.abort();
    let _ = std::fs::remove_file(config.control_socket());
    result
}

/// 클러스터 저장소에 기록된 클러스터 이름
fn read_cluster_name(member: &Member) -> Option<String> {
    member
        .cluster()
        .list::<opl_types::cluster::ClusterSpec>()
        .ok()
        .and_then(|(_, l)| l.into_iter().next())
        .map(|c| c.name)
}

/// 미초기화 노드가 받은 요청을 처리한다. 멤버가 되면 그 멤버를 돌려준다.
async fn become_member(config: &Config, node: Uuid, req: Request) -> Option<Member> {
    let (bootstrap, tls, reply, cluster) = match req {
        Request::Leave { reply } => {
            let _ = reply.send(Err(ControlError::conflict(
                "this node is not a member of a cluster",
            )));
            return None;
        }
        Request::Init {
            cluster_name,
            reply,
        } => {
            match check_ports(config).and_then(|()| new_cluster_tls(config, node, &cluster_name)) {
                Ok((ca, tls)) => (
                    Bootstrap::Init {
                        name: cluster_name.clone(),
                        ca,
                    },
                    tls,
                    reply,
                    cluster_name,
                ),
                Err(e) => {
                    let _ = reply.send(Err(ControlError::failed(e)));
                    return None;
                }
            }
        }
        Request::Join { url, reply } => {
            let joined = async {
                let url: JoinUrl = url.parse().map_err(anyhow::Error::msg)?;
                // 포트가 쓰이고 있으면 조인 토큰을 쓰기 전에 실패한다.
                check_ports(config)?;
                join::request(config, node, &url).await
            };
            match joined.await {
                Ok(joined) => (
                    Bootstrap::Join { via: joined.via },
                    joined.tls,
                    reply,
                    joined.cluster,
                ),
                Err(e) => {
                    let _ = reply.send(Err(ControlError::failed(e)));
                    return None;
                }
            }
        }
    };
    // 인증서가 없는데 클러스터 저장소가 있으면 끝나지 않은 init/join이 남긴 것이다.
    if member::cluster_dir(config).exists() {
        tracing::warn!("removing cluster data left by an unfinished init or join");
        if let Err(e) = member::remove_cluster_data(config) {
            let _ = reply.send(Err(ControlError::failed(e.into())));
            return None;
        }
    }
    match Member::start(config, node, tls, bootstrap).await {
        Ok((member, created)) => {
            let mut answer = json!({ "cluster": cluster, "node": config.name });
            if let Some(c) = created {
                answer["admin_password"] = json!(c.admin_password);
                answer["ca_fingerprint"] = json!(c.ca_fingerprint);
                answer["join_url"] = json!(c.join_url);
                answer["api_address"] = json!(config.api_address());
            }
            let _ = reply.send(Ok(answer));
            Some(member)
        }
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "cannot become a cluster member");
            // 인증서를 저장한 뒤의 실패면(합류해 learner가 된 뒤 승격 실패) 이미 멤버이므로 저장소를
            // 지우지 않는다. main 루프가 멤버로 다시 띄워 승격을 다시 요청한다.
            let e = if member::pki_dir(config).exists() {
                e.context("this node joined as a non-voting member and keeps asking to become a voting member")
            } else {
                let _ = member::remove_cluster_data(config);
                e
            };
            let _ = reply.send(Err(ControlError::failed(e)));
            None
        }
    }
}

/// 데이터 디렉터리를 만들고 주인만 들어갈 수 있게(0700) 한다. 클러스터 저장소에 CA 개인 키와
/// 비밀번호 해시가 있다. 다른 사용자가 가진 디렉터리는 그대로 둔다.
fn prepare_data_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    let meta = std::fs::metadata(dir)?;
    if meta.uid() == rustix::process::geteuid().as_raw() && meta.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// `{data_dir}/opld.lock`을 배타적으로 잠근다. 같은 데이터 디렉터리로 opld를 둘 띄우지 못하게 한다.
fn lock_data_dir(dir: &Path) -> anyhow::Result<File> {
    let path = dir.join("opld.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(rustix::io::Errno::WOULDBLOCK) => anyhow::bail!(
            "{} is locked; is another opld running with this data directory?",
            path.display()
        ),
        Err(e) => Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
}

/// API와 노드 간 포트를 잠깐 묶어 본다. 쓰이고 있으면 init/join을 시작하기 전에 실패한다.
fn check_ports(config: &Config) -> anyhow::Result<()> {
    for addr in [config.listen, config.cluster_listen] {
        std::net::TcpListener::bind(addr).with_context(|| format!("cannot listen on {addr}"))?;
    }
    Ok(())
}

/// 새 클러스터의 CA와 이 노드의 인증서
fn new_cluster_tls(
    config: &Config,
    node: Uuid,
    name: &str,
) -> anyhow::Result<(ClusterCa, NodeTls)> {
    opl_types::resource::validate_name(name)?;
    let ca = ClusterCa::generate(name)?;
    let (key_pem, csr) = node_key_and_csr()?;
    let cert_pem = ca.sign_node(&csr, node, &config.cert_names())?;
    let tls = NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem,
        key_pem,
    };
    Ok((ca, tls))
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
