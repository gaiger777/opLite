//! API 이름(클러스터 설정 `api_names`)을 이 노드의 API 인증서에 더하는 [`ApiCertAgent`]. 설정이 바뀌면
//! 클러스터 CA로 새 API 인증서를 서명해 리스너의 인증서를 바꿔 끼운다(연결은 끊지 않는다). 노드 간 mTLS는
//! 노드 인증서를 그대로 쓴다.

use std::sync::Arc;
use std::time::Duration;

use opl_cluster::{Cluster, ClusterError};
use opl_peer::pki::{ClusterCa, NodeTls, PkiError, api_cert};
use opl_peer::tls::ApiCert;
use opl_types::cluster::ClusterSpec;
use opl_types::settings::SettingsSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// 인증서를 만들지 못했을 때 다시 해 보기까지 기다리는 시간
const RETRY: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ApiCertError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Pki(#[from] PkiError),
    #[error("the cluster has not been initialized")]
    NoCa,
}

/// 이 노드의 API 인증서를 설정의 API 이름에 맞춘다.
struct Issuer {
    cluster: Arc<Cluster>,
    tls: NodeTls,
    cert: Arc<ApiCert>,
    /// 지금 인증서에 더한 이름(처음에는 노드 인증서 그대로라 비어 있다)
    served: Vec<String>,
}

impl Issuer {
    /// 설정의 API 이름이 지금 인증서와 다르면 새로 서명해 바꿔 끼운다. 비었으면 노드 인증서로 돌아간다.
    fn sync(&mut self) -> Result<(), ApiCertError> {
        let wanted = crate::settings::current(&self.cluster)?.spec.api_names;
        if wanted == self.served {
            return Ok(());
        }
        if wanted.is_empty() {
            self.cert.replace(&self.tls.cert_pem, &self.tls.key_pem)?;
        } else {
            let info = self
                .cluster
                .list::<ClusterSpec>()?
                .1
                .into_iter()
                .next()
                .ok_or(ApiCertError::NoCa)?;
            let ca = ClusterCa {
                cert_pem: info.spec.ca_cert_pem,
                key_pem: info.spec.ca_key_pem,
            };
            let (cert_pem, key_pem) = api_cert(&ca, &self.tls.cert_pem, &wanted)?;
            self.cert.replace(&cert_pem, &key_pem)?;
        }
        tracing::info!(names = ?wanted, "API certificate replaced");
        self.served = wanted;
        Ok(())
    }
}

/// API 인증서를 설정의 API 이름에 맞추는 반복 작업. API가 있는 노드마다 띄운다.
pub struct ApiCertAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ApiCertAgent {
    /// `cert`는 노드 인증서(`tls`)로 시작한 리스너 인증서다. 리스너를 열기 전에 지금 설정에 한 번
    /// 맞추고(다시 시작한 노드도 처음부터 API 이름을 쓴다), 그 뒤로는 설정 변경을 따른다.
    pub fn spawn(cluster: Arc<Cluster>, tls: NodeTls, cert: Arc<ApiCert>) -> Self {
        let mut issuer = Issuer {
            cluster,
            tls,
            cert,
            served: Vec::new(),
        };
        let pending = sync_logged(&mut issuer);
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(issuer, pending, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 맞추고, 실패했으면 경고하고 true
fn sync_logged(issuer: &mut Issuer) -> bool {
    match issuer.sync() {
        Ok(()) => false,
        Err(e) => {
            tracing::warn!(error = %e, "cannot issue the API certificate; will retry");
            true
        }
    }
}

async fn run(mut issuer: Issuer, mut pending: bool, mut shutdown: oneshot::Receiver<()>) {
    let mut watch = None;
    loop {
        // 이번에 읽을 설정 뒤의 변경을 놓치지 않게 먼저 구독하고 다시 맞춘다.
        if watch.is_none() {
            watch = issuer
                .cluster
                .list::<SettingsSpec>()
                .ok()
                .and_then(|(revision, _)| issuer.cluster.watch(revision).ok());
            pending = true;
        }
        // 실패했으면 남겨 두어 잠시 뒤 다시 해 본다.
        if pending {
            pending = sync_logged(&mut issuer);
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = crate::updates::settings_changed(&mut watch) => pending = true,
            () = tokio::time::sleep(RETRY), if pending => {}
        }
    }
}
