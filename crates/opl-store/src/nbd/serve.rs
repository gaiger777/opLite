//! `orbivisord backup-serve` 헬퍼의 몸통(설계 §11.1·§8.6): 표준 입력의 JSON 설정(저장소 백엔드·자격 증명·키·백업)
//! → 저장소를 열고 매니페스트·인덱스를 읽어 → 디스크마다 읽기 전용 내보내기 `disk{i}`를 유닉스 소켓(0600) 하나에
//! 연다. 비밀은 argv·환경에 두지 않는다.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use opl_types::backup_store::S3Target;
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use uuid::Uuid;

use super::export::{ChunkCache, StoreExport};
use super::server::NbdServer;
use crate::backend::local::LocalBackend;
use crate::backend::s3::S3Backend;
use crate::backend::{Backend, StoreError};
use crate::crypto::MasterKey;
use crate::store::Store;

/// 캐시 기본 크기(`[backup] serve_cache_mib`)
pub const DEFAULT_CACHE_MIB: u64 = 256;

/// 헬퍼 설정(표준 입력의 JSON)
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServeConfig {
    /// 내보낼 유닉스 소켓(있으면 지우고 다시 만든다)
    pub socket: PathBuf,
    pub backend: ServeBackend,
    /// 암호화 저장소의 주 키(base64)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub vm: Uuid,
    pub backup: Uuid,
    /// 풀린 청크 캐시(MiB)
    #[serde(default = "default_cache_mib")]
    pub cache_mib: u64,
    /// 소켓을 연 뒤 pid를 적을 파일(끝날 때 지운다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_file: Option<PathBuf>,
}

fn default_cache_mib() -> u64 {
    DEFAULT_CACHE_MIB
}

impl std::fmt::Debug for ServeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServeConfig")
            .field("socket", &self.socket)
            .field("backend", &self.backend)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("vm", &self.vm)
            .field("backup", &self.backup)
            .field("cache_mib", &self.cache_mib)
            .finish()
    }
}

/// 저장소가 있는 곳
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ServeBackend {
    /// 백업 풀 안 디렉터리(`{풀 루트}/store/{저장소 id}`)
    Local { root: PathBuf },
    /// S3 호환(`S3Target`의 `Debug`는 비밀을 뺀다)
    S3 { target: S3Target, max_streams: u8 },
}

impl ServeBackend {
    pub fn open(&self) -> Result<Arc<dyn Backend>, StoreError> {
        Ok(match self {
            ServeBackend::Local { root } => Arc::new(LocalBackend::new(root.clone())),
            ServeBackend::S3 {
                target,
                max_streams,
            } => Arc::new(S3Backend::new(target, *max_streams)?),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("backup store: {0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Io(String),
}

fn io(what: &str, path: &Path, e: std::io::Error) -> ServeError {
    ServeError::Io(format!("{what} {}: {e}", path.display()))
}

/// 저장소를 열고 백업의 디스크마다 내보내기를 만든다(소켓은 아직 열지 않음).
pub async fn exports(cfg: &ServeConfig) -> Result<NbdServer, ServeError> {
    let backend = cfg.backend.open()?;
    let key = cfg
        .key
        .as_deref()
        .map(MasterKey::from_base64)
        .transpose()
        .map_err(|e| StoreError::KeyMismatch(e.to_string()))?;
    let store = Arc::new(Store::open(backend, key.as_ref()).await?);
    let manifest = store.read_manifest(cfg.vm, cfg.backup).await?;
    let cache = Arc::new(ChunkCache::new(
        cfg.cache_mib.max(8) << 20,
        store.max_streams(),
    ));
    let mut server = NbdServer::new();
    for disk in &manifest.disks {
        let index = store.read_index(&manifest, disk).await?;
        server.add(
            disk.name.clone(),
            Arc::new(StoreExport::new(store.clone(), index, cache.clone())),
        );
    }
    Ok(server)
}

/// 소켓 `path`를 0600으로 연다(남은 소켓 파일은 지운다).
pub fn bind(path: &Path) -> Result<UnixListener, ServeError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(io("cannot remove the old socket", path, e));
        }
        _ => {}
    }
    if let Some(dir) = path.parent() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| io("cannot create", dir, e))?;
    }
    let listener = UnixListener::bind(path).map_err(|e| io("cannot listen on", path, e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| io("cannot restrict", path, e))?;
    Ok(listener)
}

/// 헬퍼를 돌린다: 내보내기를 만들고 소켓을 열어 `shutdown`까지 받는다. 끝나면 소켓과 pid 파일을 지운다.
pub async fn run(
    cfg: &ServeConfig,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<(), ServeError> {
    let server = Arc::new(exports(cfg).await?);
    let listener = bind(&cfg.socket)?;
    if let Some(pid) = &cfg.pid_file {
        std::fs::write(pid, format!("{}\n", std::process::id()))
            .map_err(|e| io("cannot write", pid, e))?;
    }
    tracing::info!(
        socket = %cfg.socket.display(),
        backup = %cfg.backup,
        exports = ?server.names().collect::<Vec<_>>(),
        "serving the backup over NBD"
    );
    server.serve(listener, shutdown).await;
    let _ = std::fs::remove_file(&cfg.socket);
    if let Some(pid) = &cfg.pid_file {
        let _ = std::fs::remove_file(pid);
    }
    Ok(())
}
