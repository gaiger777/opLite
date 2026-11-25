//! SDN 경보의 관찰 읽기(계획 67 §11): 리더가 NB의 게이트웨이 주인(DGP `hosting-chassis`)과 SB `Service_Monitor`를 읽어
//! [`Observations`]에 둔다. OvnSync 루프와 따로 도는 Task다(리뷰 1 I4 — 죽은 중앙에 붙느라 NB 쓰기가 늦지 않게). 연결은
//! 이어 쓰고(끊겼거나 그 중앙이 클러스터에서 떨어졌을 때만 다시 붙되 지난번에 붙은 중앙을 먼저 — 리뷰 2 I3), 붙기·요청의
//! 시간 제한은 짧다([`OBSERVE_TIMEOUT`] — 연결의 요청 시간 제한은 붙을 때 준 것이다).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use orbivisor_cluster::Cluster;
use orbivisor_driver::ovn::{NB_DB, OvsdbClient, OvsdbError, SB_DB, Transport};
use orbivisor_types::alert::sdn::{MonitorObservation, SdnObservations};
use orbivisor_types::node::NodeSpec;
use orbivisor_types::sdn::{SDN_NAME, SdnPhase, SdnSpec};
use tokio::sync::Notify;

use super::{OvnSyncConfig, OvnSyncer, central_hosts, nb, now_ms, parse_monitor};

/// 관찰 읽기의 붙기·요청 시간 제한(OvnSync 쓰기의 `timeout`보다 짧다 — 꺼진 중앙을 오래 기다리지 않게)
pub(crate) const OBSERVE_TIMEOUT: Duration = Duration::from_secs(2);

/// 관찰을 읽는 쪽(연결을 이어 쓴다)
pub(super) struct Observer {
    cluster: Arc<Cluster>,
    config: OvnSyncConfig,
    nb: Option<OvsdbClient>,
    sb: Option<OvsdbClient>,
    /// 지난번에 붙은 중앙(NB, SB)
    last: (Option<SocketAddr>, Option<SocketAddr>),
}

impl Observer {
    pub(super) fn new(cluster: Arc<Cluster>, config: OvnSyncConfig) -> Self {
        Self {
            cluster,
            config,
            nb: None,
            sb: None,
            last: (None, None),
        }
    }

    fn timeout(&self) -> Duration {
        self.config.timeout.min(OBSERVE_TIMEOUT)
    }

    /// 열린 연결이 아직 클러스터에 붙어 있으면(`_Server.Database.connected` — 리뷰 2 I3, 갈라진 중앙의 얼어붙은 사본을
    /// 새 관찰로 적지 않게) 그것, 아니면 버리고 지난번 중앙을 먼저 해 보고 새로 붙는다(`connect_any`가 `connected`를 본다).
    async fn client(
        &self,
        current: Option<OvsdbClient>,
        centrals: &[(std::net::IpAddr, Option<String>)],
        db: &str,
        port: u16,
        prefer: Option<SocketAddr>,
    ) -> Result<OvsdbClient, OvsdbError> {
        if let Some(c) = current.filter(|c| !c.is_closed()) {
            match c.server_db(db).await {
                Ok(Some(s)) if s.connected => return Ok(c),
                Ok(_) => tracing::debug!(
                    central = %c.endpoint(),
                    "the OVN central left its cluster; reconnecting the SDN observer"
                ),
                Err(e) => tracing::debug!(error = %e, "reconnecting the SDN observer"),
            }
        }
        let transport = match &self.config.transport {
            super::OvnTransport::Tcp => Transport::Tcp,
            super::OvnTransport::Tls { pki } => {
                Transport::Tls(orbivisor_driver::ovn::OvsdbTls::from_dir(pki)?)
            }
        };
        let endpoints = OvnSyncer::endpoints(centrals, port, prefer);
        OvsdbClient::connect_any(&transport, &endpoints, db, false, self.timeout()).await
    }

    /// 리더이고 SDN이 준비됐으면 한 번 읽어 둔다. 읽지 못하면 앞 관찰을 그대로 두고 그 연결을 버린다(낡으면 경보 엔진이
    /// 모르는 것으로 본다). 읽기만 하므로 팔로어 중앙도 쓴다.
    pub(super) async fn observe(&mut self) {
        let Some(term) = self.cluster.leader_term() else {
            self.nb = None;
            self.sb = None;
            return;
        };
        let sdn = match self.cluster.get_by_name::<SdnSpec>(SDN_NAME) {
            Ok(Some(s)) if s.status.phase == SdnPhase::Ready && !s.status.centrals.is_empty() => s,
            _ => return,
        };
        let Ok((_, nodes)) = self.cluster.list::<NodeSpec>() else {
            return;
        };
        let centrals = central_hosts(&sdn, &nodes);
        let nb = self.nb.take();
        let hosting = async {
            let c = self
                .client(nb, &centrals, NB_DB, self.config.nb_port, self.last.0)
                .await?;
            let (conditions, columns) = nb::hosting_select(&self.config.tag, None);
            let rows = c
                .select(NB_DB, "Logical_Router_Port", conditions, &columns)
                .await?;
            Ok::<_, OvsdbError>((c, nb::parse_hosting(&rows)))
        };
        let hosting = Box::pin(hosting).await;
        let sb = self.sb.take();
        let monitors = async {
            let c = self
                .client(sb, &centrals, SB_DB, self.config.sb_port, self.last.1)
                .await?;
            let rows = c
                .select(
                    SB_DB,
                    "Service_Monitor",
                    vec![],
                    &["ip", "port", "protocol", "logical_port", "status"],
                )
                .await?;
            Ok::<_, OvsdbError>((c, rows.iter().filter_map(parse_monitor).collect::<Vec<_>>()))
        };
        let monitors = Box::pin(monitors).await;
        match (hosting, monitors) {
            (Ok((nb, hosting)), Ok((sb, monitors))) => {
                self.last = (Some(nb.endpoint().addr), Some(sb.endpoint().addr));
                self.nb = Some(nb);
                self.sb = Some(sb);
                if self.cluster.leader_term() != Some(term) {
                    return;
                }
                self.config.observations.set(SdnObservations {
                    term,
                    at: now_ms(),
                    hosting,
                    monitors: monitors
                        .into_iter()
                        .map(|m| MonitorObservation {
                            logical_port: m.logical_port,
                            port: m.port,
                            protocol: m.protocol,
                            status: m.status,
                        })
                        .collect(),
                });
            }
            (hosting, monitors) => {
                // 성공한 쪽의 연결은 이어 쓴다.
                if let Ok((nb, _)) = hosting {
                    self.last.0 = Some(nb.endpoint().addr);
                    self.nb = Some(nb);
                }
                if let Ok((sb, _)) = monitors {
                    self.last.1 = Some(sb.endpoint().addr);
                    self.sb = Some(sb);
                }
                tracing::debug!("cannot read the OVN state for SDN alerts");
            }
        }
    }
}

/// 관찰 Task: `every`마다, 또는 OvnSync가 한 차례를 마치면(`kick`) 읽는다.
pub(super) async fn run(cluster: Arc<Cluster>, config: OvnSyncConfig, kick: Arc<Notify>) {
    let every = config.observe_every;
    let mut observer = Observer::new(cluster, config);
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            () = kick.notified() => {}
        }
        Box::pin(observer.observe()).await;
    }
}
