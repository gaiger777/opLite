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

use std::path::PathBuf;
use std::pin::pin;
use std::sync::{Arc, Mutex};

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
    std::fs::create_dir_all(&config.data_dir)?;
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
        loop {
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
                    }
                }
            };
            let cluster_name = member
                .cluster()
                .list::<opl_types::cluster::ClusterSpec>()
                .ok()
                .and_then(|(_, l)| l.into_iter().next())
                .map(|c| c.name);
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
        } => match new_cluster_tls(config, node, &cluster_name) {
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
        },
        Request::Join { url, reply } => {
            let joined = async {
                let url: JoinUrl = url.parse().map_err(anyhow::Error::msg)?;
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
            let _ = member::remove_cluster_data(config);
            let _ = reply.send(Err(ControlError::failed(e)));
            None
        }
    }
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
