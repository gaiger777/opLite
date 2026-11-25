//! 이 노드 API 인증서의 차례를 맞추는 [`ApiCertAgent`](계획 73 §11.5): (1) `[api] tls_cert` 파일 — 있으면 이 작업을 띄우지
//! 않는다, (2) `install: api`이고 이 노드를 덮는 살아 있는 ACME 인증서(노드마다 먼저 — 모두 SNI 이름으로도 고른다),
//! (3) 클러스터 CA로 서명한 API 인증서(설정 `api_names` — 비면 노드 인증서). 설정·인증서가 바뀌면 리스너의 인증서를
//! 바꿔 끼운다(연결은 끊지 않는다). 노드 간 mTLS는 노드 인증서를 그대로 쓴다.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_peer::pki::{ClusterCa, NodeTls, PkiError, api_cert};
use opl_peer::tls::ApiCert;
use opl_types::acme::CertificateSpec;
use opl_types::cluster::ClusterSpec;
use opl_types::resource::Kind;
use opl_types::settings::SettingsSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// 인증서를 만들지 못했을 때 다시 해 보기까지 기다리는 시간
const RETRY: Duration = Duration::from_secs(10);
/// 바뀐 것이 없어도 다시 맞추는 간격(끼운 ACME 인증서가 만료되면 다음 차례로)
const RECHECK: Duration = Duration::from_secs(3600);
/// ACME 인증서를 끼운 동안 `served`에 두는 표시(내부 CA로 돌아갈 때 다시 서명하게)
const ACME_SERVED: &str = "\0acme";

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
    /// 노드마다 ACME 인증서의 키 디렉터리(없으면 ACME 인증서를 끼우지 않는다)
    acme_keys: Option<PathBuf>,
    /// 지금 끼운 ACME 인증서들
    served_acme: Vec<crate::acme::install::Installable>,
}

impl Issuer {
    /// 끼울 ACME 인증서가 있으면 그것(짝이 맞지 않는 것은 경고하고 뺀다), 없으면 설정의 API 이름이 지금 인증서와 다를 때
    /// 새로 서명해 바꿔 끼운다. 비었으면 노드 인증서로 돌아간다.
    fn sync(&mut self) -> Result<(), ApiCertError> {
        if let Some(dir) = &self.acme_keys {
            let (_, certs) = self.cluster.list::<CertificateSpec>()?;
            let found = crate::acme::install::installable(
                &certs,
                self.cluster.id().0,
                Some(dir),
                crate::backups::now() as i64,
            );
            let usable: Vec<_> = found
                .into_iter()
                .filter(|i| match opl_peer::tls::check_pair(&i.chain_pem, &i.key_pem) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(certificate = %i.certificate, error = %e, "the ACME certificate does not match its key; not installing it");
                        false
                    }
                })
                .collect();
            if !usable.is_empty() {
                if usable != self.served_acme {
                    let first = &usable[0];
                    self.cert.replace(&first.chain_pem, &first.key_pem)?;
                    let named: Vec<_> = usable
                        .iter()
                        .map(|i| (i.names.clone(), i.chain_pem.clone(), i.key_pem.clone()))
                        .collect();
                    self.cert.set_named(&named)?;
                    tracing::info!(names = ?first.names, "API certificate replaced with an ACME certificate");
                    self.served_acme = usable;
                    self.served = vec![ACME_SERVED.into()];
                }
                return Ok(());
            }
            if !self.served_acme.is_empty() {
                self.cert.set_named(&[])?;
                self.served_acme.clear();
            }
        }
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
        Self::spawn_with_acme(cluster, tls, cert, None)
    }

    /// [`spawn`](Self::spawn)과 같되 ACME 인증서(노드마다 키는 `acme_keys` 아래)도 끼운다(계획 73).
    pub fn spawn_with_acme(
        cluster: Arc<Cluster>,
        tls: NodeTls,
        cert: Arc<ApiCert>,
        acme_keys: Option<PathBuf>,
    ) -> Self {
        let mut issuer = Issuer {
            cluster,
            tls,
            cert,
            served: Vec::new(),
            acme_keys,
            served_acme: Vec::new(),
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
            () = changed(&mut watch) => pending = true,
            () = tokio::time::sleep(RETRY), if pending => {}
            () = tokio::time::sleep(RECHECK) => pending = true,
        }
    }
}

/// 설정이나 ACME 인증서가 바뀔 때까지 기다린다. 구독이 끊기면(`Reset`) 비워 다시 구독하게 한다.
async fn changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return tokio::time::sleep(RETRY).await;
    };
    loop {
        match w.next().await {
            Ok(WatchEvent::Put { kind, .. } | WatchEvent::Deleted { kind, .. })
                if matches!(kind, Kind::Settings | Kind::Certificate) =>
            {
                return;
            }
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}
