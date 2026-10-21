//! opLite 노드 데몬. 클러스터 저장소, reconciler, Task 실행기, REST API를 한 프로세스로 묶는다.
//!
//! QEMU는 `-daemonize`로 떠 있으므로 opld가 멈추거나 죽어도 VM은 계속 돈다. 다시 시작하면
//! 하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙고 reconciler가 이어서 맞춘다.

mod bootstrap;
mod config;
mod host;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use opl_api::{AppState, TaskRunner, TaskRunnerConfig};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{DirStorage, Hypervisor, QemuConfig, QemuHypervisor, Storage};
use opl_qemu::HostProfile;
use opl_reconciler::{Reconciler, ReconcilerConfig};
use tracing_subscriber::EnvFilter;

use crate::config::Config;

/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

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
    let id = NodeId(node);

    // 노드 간 네트워크(gRPC + mTLS)는 계획 6에서 만든다. 그때까지는 1인 클러스터다.
    let router = MemRouter::default();
    let cluster = Arc::new(
        Cluster::open(
            ClusterConfig::new(id, config.data_dir.join("cluster")),
            router.network(id),
        )
        .await?,
    );
    router.register(id, cluster.raft().clone());
    bootstrap::start_cluster(&cluster, &config.listen).await?;
    bootstrap::ensure_admin(&cluster, &config.data_dir).await?;
    bootstrap::register_node(&cluster, &config).await?;

    let hv: Arc<dyn Hypervisor> = Arc::new(
        QemuHypervisor::new(QemuConfig {
            host: HostProfile::homebrew_hvf(&config.qemu_share_dir),
            run_dir: config.run_dir.clone(),
            state_dir: config.data_dir.clone(),
        })
        .await?,
    );
    let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
    let mut reconciler_config = ReconcilerConfig::new(node);
    reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
    let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
    // Task 실행기는 시작할 때 Task 없는 업로드와 볼륨을 지운다. 그것이 끝난 뒤에 API를 연다.
    let mut runner_config = TaskRunnerConfig::new(node);
    runner_config.upload_dir = Some(config.data_dir.join("uploads"));
    let runner =
        TaskRunner::spawn(cluster.clone(), storage, reconciler.handle(), runner_config).await;
    let state = Arc::new(AppState::new(
        cluster.clone(),
        node,
        hv,
        config.data_dir.join("uploads"),
    ));

    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!(node = %node, addr = %listener.local_addr()?, "opld is ready");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, opl_api::router(state))
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
    });

    tokio::select! {
        () = shutdown_signal() => {}
        result = &mut server => {
            // 서버가 스스로 멈췄다(포트 오류 등).
            result??;
        }
    }
    tracing::info!("shutting down; running VMs keep running");
    let _ = stop_tx.send(());
    if tokio::time::timeout(DRAIN_TIMEOUT, &mut server)
        .await
        .is_err()
    {
        server.abort();
    }
    runner.shutdown().await;
    reconciler.shutdown().await;
    bootstrap::mark_offline(&cluster).await;
    cluster.shutdown().await?;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
