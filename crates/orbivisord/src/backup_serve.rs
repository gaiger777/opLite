//! `orbivisord backup-serve`(계획 68 설계 §11.1): 표준 입력의 JSON 설정으로 저장소 백업의 디스크를 유닉스 소켓
//! NBD로 내보낸다. SIGTERM·SIGINT에 소켓과 pid 파일을 지우고 끝난다. 띄우기·지켜보기는 orbivisor-driver의
//! `start_backup_serve`가 한다.

use std::io::Read;

use anyhow::Context;
use orbivisor_store::nbd::serve::{ServeConfig, run as serve};

pub fn run() -> anyhow::Result<()> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .context("cannot read the configuration from standard input")?;
    let cfg: ServeConfig =
        serde_json::from_slice(&input).context("invalid backup-serve configuration")?;
    drop(input);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .context("cannot start the runtime")?;
    rt.block_on(async {
        let shutdown = async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
            let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        };
        serve(&cfg, shutdown).await
    })
    .with_context(|| format!("backup-serve of backup {}", cfg.backup))
}
