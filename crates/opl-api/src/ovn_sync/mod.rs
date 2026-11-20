//! 리더 `OvnSync`(계획 65 §8): Raft의 SDN 네트워크·서브넷·VM NIC를 OVN northbound DB(NB)의 논리 스위치·DHCP_Options·
//! 논리 포트로 옮겨 쓴다. NB는 Raft에서 다시 만들 수
//! 있는 파생물이다 — 리더가 바뀌거나 누가 NB를 바꾸거나 비워도 다음 차례가 되살린다.
//!
//! 위트니스가 아닌 모든 노드가 띄우고 리더에서만 일한다. 차례는 리더가 된 직후, Network·Vm·Sdn·Router·FloatingIp·SecurityGroup 변경과
//! 노드 지우기, 라우터에 쓰이는 노드 칸([`nb::NodeInputs`])의 변경(0.5초 모아 한 번), 60초마다다. `Sdn`이 없거나 `Ready`가 아니면 쉰다. 한 차례는
//!
//! 1. 내 Raft 임기를 잡고 `read_barrier()` 뒤 Raft에서 읽어 바라는 행([`nb::desired`])을 만든다. Raft 읽기가 실패하면
//!    차례를 건너뛴다(빈 바라는 상태로 지우지 않는다).
//! 2. NB 리더에 붙어(팔로어는 낡은 행을 줄 수 있다) 주인 표시가 있는 우리 행과 `NB_Global`의 펜싱 토큰을 읽는다.
//!    토큰의 임기가 내 임기보다 크면 쓰지 않는다(나는 물러난 리더다).
//! 3. 다른 것만([`nb::plan`]) 펜싱 연산(`wait` + 토큰 `"<임기>:<Raft 인덱스>"` + `nb_cfg` 1 올림)을 앞에 붙여 쓴다.
//!    네트워크마다(지울 스위치·DHCP 행도 따로) 한 트랜잭션이다 — 한 네트워크의 실패가 다른 네트워크를 막지 않는다(리뷰 1
//!    I-6). 실패한 묶음만 다음 차례에 다시 한다.
//! 4. SB에서 Raft에 없는 노드(노드 UUID 이름)의 `Chassis`·`Chassis_Private` 행을 지운다(`ovn-sbctl chassis-del`과
//!    같다 — 65에서 SB에 쓰는 유일한 일). 쓰기 바로 앞에서 아직 같은 임기의 리더인지 다시 본다(리뷰 1 m-1).
//! 5. `SdnStatus.sync`를 바뀔 때만(성공이 이어지면 60초에 한 번) 쓴다.

pub mod acl;
pub mod nb;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::Cluster;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_driver::ovn::ovsdb::{cond, parse_uuids};
use opl_driver::ovn::{Endpoint, NB_DB, OvsdbClient, OvsdbError, OvsdbTls, SB_DB, Transport};
use opl_types::floating_ip::FloatingIpSpec;
use opl_types::network::NetworkSpec;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Kind;
use opl_types::router::RouterSpec;
use opl_types::sdn::{NB_PORT, SB_PORT, SDN_NAME, SdnPhase, SdnSpec, SdnSync};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::vm::VmSpec;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::backups::now;
use crate::cluster::host_of;
use crate::tasks::sdn::update_sdn;

/// 성공이 이어질 때 `SdnStatus.sync.at`을 새로 쓰는 간격
const SYNC_AT_REFRESH: u64 = 60;
/// 리더가 되었는지 보는 간격
const LEADER_POLL: Duration = Duration::from_secs(1);
/// 구독이 끊겼을 때 다시 구독하기 전 기다림
const RESUBSCRIBE: Duration = Duration::from_secs(1);

/// OVN에 붙는 방법
#[derive(Debug, Clone)]
pub enum OvnTransport {
    /// 노드 TLS 디렉터리(`{data_dir}/pki`)의 클러스터 CA mTLS. 붙을 때마다 읽는다(다시 서명한 인증서를 곧바로 씀).
    Tls { pki: PathBuf },
    /// 평문 TCP(시험의 가짜 서버만)
    Tcp,
}

#[derive(Debug, Clone)]
pub struct OvnSyncConfig {
    /// 클러스터 태그(NB 행의 주인 표시)
    pub tag: String,
    pub transport: OvnTransport,
    /// 중앙의 NB·SB 포트(시험만 바꾼다)
    pub nb_port: u16,
    pub sb_port: u16,
    /// 변경이 없어도 다시 맞추는 간격
    pub resync: Duration,
    /// 변경을 모으는 시간
    pub debounce: Duration,
    /// OVSDB 연결·요청의 시간 제한
    pub timeout: Duration,
}

impl OvnSyncConfig {
    pub fn new(tag: impl Into<String>, transport: OvnTransport) -> Self {
        Self {
            tag: tag.into(),
            transport,
            nb_port: NB_PORT,
            sb_port: SB_PORT,
            resync: Duration::from_secs(60),
            debounce: Duration::from_millis(500),
            timeout: opl_driver::ovn::ovsdb::DEFAULT_TIMEOUT,
        }
    }
}

/// 한 차례의 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// 리더가 아니다.
    NotLeader,
    /// SDN이 설정 전이거나 준비되지 않았다.
    Idle,
    /// Raft를 읽지 못해 건너뛰었다(아무것도 지우지 않았다).
    Skipped(String),
    /// NB의 토큰이 더 큰 임기라 쓰지 않았다.
    Fenced { term: u64 },
    /// NB를 맞추었다(같으면 트랜잭션 0). `failed`는 실패한 묶음 수(오류는 `SdnStatus.sync.error`).
    Synced {
        transactions: usize,
        operations: usize,
        failed: usize,
        chassis_deleted: usize,
    },
    /// NB에 붙지 못했다.
    Failed(String),
}

/// OvnSync의 한 차례를 하는 것. 연결을 차례 사이에 들고 있는다.
pub struct OvnSyncer {
    cluster: Arc<Cluster>,
    config: OvnSyncConfig,
    nb: Option<OvsdbClient>,
    sb: Option<OvsdbClient>,
    /// 지난 차례가 읽은 VM마다 논리 포트에 쓰이는 칸(리뷰 2 m-2 — 이것이 그대로인 VM 쓰기로는 깨지 않는다)
    seen: HashMap<Uuid, nb::VmInputs>,
    /// 지난 차례가 읽은 노드마다 라우터 행에 쓰이는 칸(계획 66)
    seen_nodes: HashMap<Uuid, nb::NodeInputs>,
}

impl OvnSyncer {
    pub fn new(cluster: Arc<Cluster>, config: OvnSyncConfig) -> Self {
        Self {
            cluster,
            config,
            nb: None,
            sb: None,
            seen: HashMap::new(),
            seen_nodes: HashMap::new(),
        }
    }

    /// VM 쓰기 `spec`·`status`(VM `id`)가 지난 차례가 본 것과 논리 포트에 쓰이는 칸이 같은지(같으면 다시 맞출 일이 없다)
    pub fn unchanged_vm(&self, id: Uuid, inputs: &nb::VmInputs) -> bool {
        self.seen.get(&id) == Some(inputs)
    }

    /// 노드 쓰기가 지난 차례가 본 것과 라우터 행에 쓰이는 칸이 같은지
    pub fn unchanged_node(&self, id: Uuid, inputs: &nb::NodeInputs) -> bool {
        self.seen_nodes.get(&id) == Some(inputs)
    }

    /// 연결을 놓는다(리더가 아니게 되었거나 SDN이 쉴 때).
    pub fn disconnect(&mut self) {
        self.nb = None;
        self.sb = None;
    }

    /// 한 차례
    pub async fn sync(&mut self) -> SyncOutcome {
        let cluster = self.cluster.clone();
        let Some(term) = cluster.leader_term() else {
            self.disconnect();
            return SyncOutcome::NotLeader;
        };
        if let Err(e) = cluster.read_barrier().await {
            return SyncOutcome::Skipped(e.to_string());
        }
        // 장벽 사이에 임기가 바뀌었으면 이 읽기는 그 임기의 것이 아니다.
        if cluster.leader_term() != Some(term) {
            self.disconnect();
            return SyncOutcome::NotLeader;
        }
        let sdn = match cluster.get_by_name::<SdnSpec>(SDN_NAME) {
            Ok(Some(s)) if s.status.phase == SdnPhase::Ready && !s.status.centrals.is_empty() => s,
            Ok(_) => {
                self.disconnect();
                return SyncOutcome::Idle;
            }
            Err(e) => return SyncOutcome::Skipped(e.to_string()),
        };
        let index = cluster.revision();
        let (networks, vms, nodes) = match (
            cluster.list::<NetworkSpec>(),
            cluster.list::<VmSpec>(),
            cluster.list::<NodeSpec>(),
        ) {
            (Ok((_, n)), Ok((_, v)), Ok((_, m))) => (n, v, m),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                return SyncOutcome::Skipped(e.to_string());
            }
        };
        let (routers, fips, groups) = match (
            cluster.list::<RouterSpec>(),
            cluster.list::<FloatingIpSpec>(),
            cluster.list::<SecurityGroupSpec>(),
        ) {
            (Ok((_, r)), Ok((_, f)), Ok((_, g))) => (r, f, g),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                return SyncOutcome::Skipped(e.to_string());
            }
        };
        self.seen = vms
            .iter()
            .map(|v| (v.id, nb::VmInputs::of(&v.spec, &v.status)))
            .collect();
        self.seen_nodes = nodes
            .iter()
            .map(|n| (n.id, nb::NodeInputs::of(&n.spec, &n.status)))
            .collect();
        let want = nb::desired_all(
            &networks,
            &vms,
            &nodes,
            &routers,
            &fips,
            &groups,
            &self.config.tag,
        );
        let centrals = central_hosts(&sdn, &nodes);
        let old = sdn.status.sync.clone();

        let outcome = Box::pin(self.write_nb(&want, &centrals, term, index)).await;
        let mut sync = old.clone().unwrap_or_default();
        sync.at = now();
        let result = match outcome {
            Ok(NbResult::Fenced(seen)) => {
                // 물러난 리더는 기록도 쓰지 않는다(새 리더의 몫이다). 아직 리더인데 NB의 토큰이 더 크면(Raft를 새로
                // 세우고 OVN DB는 둔 경우) 차례마다 오류로 남긴다 — 저절로 풀리지 않는다(리뷰 1 m-2).
                if cluster.leader_term() == Some(term) {
                    tracing::error!(
                        term,
                        newer = seen,
                        key = %nb::fence_key(&self.config.tag),
                        "the OVN northbound DB carries a fence token from a newer Raft term than this leader's; \
                         OvnSync stops writing. If the Orbivisor cluster was rebuilt, remove the key with \
                         `ovn-nbctl remove NB_Global . external_ids <key>` on the NB leader"
                    );
                }
                return SyncOutcome::Fenced { term: seen };
            }
            Ok(NbResult::Done(done)) => {
                sync.term = done.token.0;
                sync.index = done.token.1;
                sync.nb_cfg = done.nb_cfg;
                sync.hv_cfg = done.hv_cfg;
                sync.error = done.error.clone();
                Ok(done)
            }
            Err(e) => {
                sync.error = Some(e.to_string());
                Err(e)
            }
        };
        // SB 청소는 NB와 따로다(NB가 안 되어도 한다). 그새 물러났으면 하지 않는다(낡은 노드 목록으로 막 합류한 노드의
        // chassis를 지우지 않게 — 리뷰 1 m-1).
        let still_leader = cluster.leader_term() == Some(term);
        let chassis_deleted =
            match Box::pin(self.clean_chassis(&centrals, &nodes, still_leader)).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot remove the OVN chassis of removed nodes");
                    0
                }
            };
        if let Err(e) = report(&cluster, old.as_ref(), sync).await {
            tracing::warn!(error = %e, "cannot record the OVN sync");
        }
        match result {
            Ok(done) => SyncOutcome::Synced {
                transactions: done.transactions,
                operations: done.operations,
                failed: done.failed,
                chassis_deleted,
            },
            Err(e) => {
                tracing::warn!(error = %e, "cannot sync the OVN northbound DB");
                SyncOutcome::Failed(e.to_string())
            }
        }
    }

    fn transport(&self) -> Result<Transport, OvsdbError> {
        Ok(match &self.config.transport {
            OvnTransport::Tcp => Transport::Tcp,
            OvnTransport::Tls { pki } => Transport::Tls(OvsdbTls::from_dir(pki)?),
        })
    }

    /// 중앙들의 `port` 주소. 서버 인증서는 중앙 주소 IP나 노드 주소의 호스트 이름 가운데 하나에 맞으면 된다.
    pub(crate) fn endpoints(
        centrals: &[(std::net::IpAddr, Option<String>)],
        port: u16,
        prefer: Option<SocketAddr>,
    ) -> Vec<Endpoint> {
        let mut out: Vec<Endpoint> = centrals
            .iter()
            .map(|(ip, host)| {
                let mut names = vec![ip.to_string()];
                if let Some(h) = host.as_ref().filter(|h| **h != names[0]) {
                    names.push(h.clone());
                }
                Endpoint {
                    addr: SocketAddr::new(*ip, port),
                    names,
                }
            })
            .collect();
        // 지난번 리더를 먼저 본다.
        if let Some(p) = prefer {
            out.sort_by_key(|e| e.addr != p);
        }
        out
    }

    /// `db`의 리더에 붙은 연결(있던 연결이 아직 리더면 그것)
    async fn leader(
        &self,
        current: Option<OvsdbClient>,
        centrals: &[(std::net::IpAddr, Option<String>)],
        db: &str,
        port: u16,
    ) -> Result<OvsdbClient, OvsdbError> {
        let prefer = current.as_ref().map(|c| c.endpoint().addr);
        if let Some(c) = current
            && !c.is_closed()
            && c.server_db(db)
                .await
                .is_ok_and(|s| s.is_some_and(|s| s.is_leader()))
        {
            return Ok(c);
        }
        let transport = self.transport()?;
        let endpoints = Self::endpoints(centrals, port, prefer);
        OvsdbClient::connect_any(&transport, &endpoints, db, true, self.config.timeout).await
    }

    async fn write_nb(
        &mut self,
        want: &nb::NbState,
        centrals: &[(std::net::IpAddr, Option<String>)],
        term: u64,
        index: u64,
    ) -> Result<NbResult, OvsdbError> {
        let current = self.nb.take();
        let client = self
            .leader(current, centrals, NB_DB, self.config.nb_port)
            .await?;
        let tag = self.config.tag.clone();
        let results = client.transact(NB_DB, nb::read_ops(&tag)).await;
        let results = match results {
            Ok(r) => r,
            Err(e) => return Err(e),
        };
        let have = nb::parse_actual(&results);
        let fence = nb::fence_key(&tag);
        let seen = have
            .global
            .as_ref()
            .and_then(|g| g.external_ids.get(&fence))
            .and_then(|t| nb::parse_token(t));
        if let Some((seen_term, _)) = seen
            && seen_term > term
        {
            tracing::warn!(
                term,
                newer = seen_term,
                "a newer leader wrote the OVN northbound DB; not writing"
            );
            self.nb = Some(client);
            return Ok(NbResult::Fenced(seen_term));
        }
        let plan = nb::plan(want, &have);
        let mut done = Done {
            token: seen.unwrap_or((0, 0)),
            nb_cfg: have.global.as_ref().map_or(0, |g| g.nb_cfg),
            hv_cfg: have.global.as_ref().map_or(0, |g| g.hv_cfg),
            ..Done::default()
        };
        let token = format!("{term}:{index}");
        let mut global = have.global.clone();
        let mut errors = Vec::new();
        // `nb_cfg`는 한 차례에 한 번(마지막 묶음)만 올린다(리뷰 2 m-2). 마지막 묶음이 실패했으면 끝에 따로 올린다.
        let mut bumped = false;
        for (i, batch) in plan.iter().enumerate() {
            let bump = i + 1 == plan.len();
            let (fence_ops, ids) = nb::fence(global.as_ref(), &tag, &token, bump);
            let n_fence = fence_ops.len();
            let mut ops = fence_ops;
            ops.extend(batch.ops.iter().cloned());
            let count = ops.len();
            match client.transact(NB_DB, ops).await {
                Ok(results) => {
                    done.transactions += 1;
                    done.operations += count;
                    done.token = (term, index);
                    bumped |= bump;
                    global = Some(bumped_global(global, ids, &results, bump));
                }
                // 펜싱 연산(앞 `n_fence`개)의 실패만 "NB가 바뀜"이다. 묶음 안의 `wait`(목록 Port_Group이 있는지 — 계획 66
                // §11.3)이 실패한 것은 그 묶음만의 실패다.
                Err(e) if matches!(&e, OvsdbError::Op { index, .. } if *index < n_fence) => {
                    // 읽은 뒤 누가 NB_Global을 바꿨다(새 리더나 northd). 남은 묶음은 다음 차례에 다시 읽고 한다.
                    done.failed += plan.len() - i;
                    errors.push(format!(
                        "{}: the northbound DB changed while syncing ({e})",
                        batch.key
                    ));
                    break;
                }
                Err(e @ (OvsdbError::Op { .. } | OvsdbError::Rpc { .. })) => {
                    done.failed += 1;
                    errors.push(format!("{}: {e}", batch.key));
                }
                Err(e) => {
                    // 연결이 끊겼다.
                    done.failed += plan.len() - i;
                    errors.push(format!("{}: {e}", batch.key));
                    break;
                }
            }
        }
        if done.transactions > 0 && !bumped && !client.is_closed() {
            let (ops, ids) = nb::fence(global.as_ref(), &tag, &token, true);
            match client.transact(NB_DB, ops).await {
                Ok(results) => {
                    done.transactions += 1;
                    bumped = true;
                    global = Some(bumped_global(global, ids, &results, true));
                }
                Err(e) => errors.push(format!("NB_Global: {e}")),
            }
        }
        if bumped {
            done.nb_cfg = global.as_ref().map_or(done.nb_cfg, |g| g.nb_cfg);
        }
        if !errors.is_empty() {
            let more = errors.len().saturating_sub(3);
            let mut text = errors.into_iter().take(3).collect::<Vec<_>>().join("; ");
            if more > 0 {
                text.push_str(&format!("; and {more} more"));
            }
            done.error = Some(text);
        }
        if !client.is_closed() {
            self.nb = Some(client);
        }
        Ok(NbResult::Done(done))
    }

    /// SB에서 Raft에 없는 노드의 chassis를 지운다. chassis 이름은 노드 UUID다(§5) — UUID가 아닌 이름(관리자가 붙인
    /// chassis)은 건드리지 않는다.
    async fn clean_chassis(
        &mut self,
        centrals: &[(std::net::IpAddr, Option<String>)],
        nodes: &[Node],
        leader: bool,
    ) -> Result<usize, OvsdbError> {
        if !leader {
            return Ok(0);
        }
        let current = self.sb.take();
        let client = self
            .leader(current, centrals, SB_DB, self.config.sb_port)
            .await?;
        let read = vec![
            json!({"op": "select", "table": "Chassis", "where": [], "columns": ["_uuid", "name"]}),
            json!({"op": "select", "table": "Chassis_Private", "where": [], "columns": ["_uuid", "name"]}),
        ];
        let results = client.transact(SB_DB, read).await?;
        let gone = |r: &Value| {
            r["name"]
                .as_str()
                .and_then(|n| Uuid::parse_str(n).ok())
                .is_some_and(|id| !nodes.iter().any(|n| n.id == id))
        };
        let mut ops = Vec::new();
        let mut names = Vec::new();
        for (table, result) in ["Chassis", "Chassis_Private"].iter().zip(&results) {
            for r in result["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| gone(r))
            {
                let uuid = parse_uuids(&r["_uuid"])
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                ops.push(json!({"op": "delete", "table": table,
                    "where": [cond("_uuid", "==", json!(["uuid", uuid]))]}));
                if *table == "Chassis" {
                    names.push(r["name"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        let n = names.len();
        if !ops.is_empty() {
            client.transact(SB_DB, ops).await?;
            tracing::info!(chassis = ?names, "removed the OVN chassis of removed nodes");
        }
        if !client.is_closed() {
            self.sb = Some(client);
        }
        Ok(n)
    }
}

/// 펜싱 연산을 붙인 트랜잭션 뒤의 `NB_Global`(`bump`면 `nb_cfg` 1 더함). 없던 행이면 넣은 행(두 번째 결과)의 uuid다.
fn bumped_global(
    global: Option<nb::Global>,
    ids: std::collections::BTreeMap<String, String>,
    results: &[Value],
    bump: bool,
) -> nb::Global {
    let step = i64::from(bump);
    match global {
        Some(mut g) => {
            g.external_ids = ids;
            g.nb_cfg += step;
            g
        }
        None => nb::Global {
            uuid: results
                .get(1)
                .map(|r| parse_uuids(&r["uuid"]))
                .unwrap_or_default()
                .into_iter()
                .next()
                .unwrap_or_default(),
            external_ids: ids,
            nb_cfg: step,
            hv_cfg: 0,
        },
    }
}

enum NbResult {
    Fenced(u64),
    Done(Done),
}

#[derive(Default)]
struct Done {
    /// NB에 있는 토큰(이번에 썼으면 내 것)
    token: (u64, u64),
    nb_cfg: i64,
    hv_cfg: i64,
    transactions: usize,
    operations: usize,
    failed: usize,
    error: Option<String>,
}

/// `SdnStatus.sync`를 바뀔 때만 쓴다. `at`만 바뀌면 60초 넘게 낡았을 때만 쓴다(Raft 쓰기가 차례마다 생기지 않게).
async fn report(cluster: &Cluster, old: Option<&SdnSync>, new: SdnSync) -> Result<(), String> {
    if let Some(old) = old {
        let same = SdnSync {
            at: old.at,
            ..new.clone()
        } == *old;
        if same && new.at.saturating_sub(old.at) < SYNC_AT_REFRESH {
            return Ok(());
        }
    }
    update_sdn(cluster, |s| {
        if s.phase != SdnPhase::Ready {
            return false;
        }
        s.sync = Some(new.clone());
        true
    })
    .await
}

/// OvnSync 반복 작업. 모든 비위트니스 노드에서 띄우고 리더에서만 일한다.
pub struct OvnSync {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl OvnSync {
    pub fn spawn(cluster: Arc<Cluster>, config: OvnSyncConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(cluster: Arc<Cluster>, config: OvnSyncConfig, mut shutdown: oneshot::Receiver<()>) {
    let (resync, debounce) = (config.resync, config.debounce);
    let mut syncer = OvnSyncer::new(cluster.clone(), config);
    let mut watch: Option<Watch> = None;
    loop {
        // 이번 차례가 읽을 것 뒤의 변경을 놓치지 않게 먼저 구독한다.
        if watch.is_none() {
            watch = cluster
                .list::<SdnSpec>()
                .ok()
                .and_then(|(revision, _)| cluster.watch(revision).ok());
        }
        let mut last_term = cluster.leader_term();
        if last_term.is_some() {
            // 선거 중에는 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
            tokio::select! {
                _ = &mut shutdown => return,
                _ = Box::pin(syncer.sync()) => {}
            }
        } else {
            syncer.disconnect();
        }
        let deadline = tokio::time::sleep(resync);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut shutdown => return,
                () = &mut deadline => break,
                () = changed(&mut watch, &syncer) => {
                    // 모아서 한 번
                    tokio::select! {
                        _ = &mut shutdown => return,
                        () = tokio::time::sleep(debounce) => {}
                    }
                    break;
                }
                () = tokio::time::sleep(LEADER_POLL) => {
                    // 리더가 되었거나 임기가 바뀌었으면 곧바로 맞춘다.
                    let term = cluster.leader_term();
                    if term.is_some() && term != last_term {
                        break;
                    }
                    last_term = term;
                }
            }
        }
    }
}

/// 맞출 일이 생길 때까지 기다린다: Network·Sdn·Router·FloatingIp·SecurityGroup 변경, 논리 포트에 쓰이는 칸이 바뀐 VM 쓰기(지난 차례가
/// 본 것과 견줌 — 리뷰 2 m-2), 라우터 행에 쓰이는 칸이 바뀐 노드 쓰기(계획 66), VM·노드 지우기(SB chassis 청소). 구독이 끊기면 버리고 돌아가 다시 맞추게 한다.
async fn changed(watch: &mut Option<Watch>, syncer: &OvnSyncer) {
    let Some(w) = watch else {
        return tokio::time::sleep(RESUBSCRIBE).await;
    };
    loop {
        match w.next().await {
            Ok(WatchEvent::Put {
                kind: Kind::Vm,
                resource,
            }) => {
                let same = match (
                    serde_json::from_value::<VmSpec>(resource.spec),
                    serde_json::from_value::<opl_types::vm::VmStatus>(resource.status),
                ) {
                    (Ok(spec), Ok(status)) => {
                        syncer.unchanged_vm(resource.id, &nb::VmInputs::of(&spec, &status))
                    }
                    _ => false,
                };
                if !same {
                    return;
                }
            }
            Ok(WatchEvent::Put {
                kind: Kind::Node,
                resource,
            }) => {
                let same = match (
                    serde_json::from_value::<NodeSpec>(resource.spec),
                    serde_json::from_value::<opl_types::node::NodeStatus>(resource.status),
                ) {
                    (Ok(spec), Ok(status)) => {
                        syncer.unchanged_node(resource.id, &nb::NodeInputs::of(&spec, &status))
                    }
                    _ => false,
                };
                if !same {
                    return;
                }
            }
            Ok(WatchEvent::Put {
                kind:
                    Kind::Network | Kind::Sdn | Kind::Router | Kind::FloatingIp | Kind::SecurityGroup,
                ..
            })
            | Ok(WatchEvent::Deleted {
                kind:
                    Kind::Network
                    | Kind::Vm
                    | Kind::Sdn
                    | Kind::Node
                    | Kind::Router
                    | Kind::FloatingIp
                    | Kind::SecurityGroup,
                ..
            }) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod acl_tests;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod tests;

/// 중앙마다 (주소, 노드 주소의 호스트 이름) — 서버 인증서가 맞아야 할 이름들([`OvnSyncer::endpoints`])
pub(crate) fn central_hosts(
    sdn: &opl_types::sdn::Sdn,
    nodes: &[Node],
) -> Vec<(std::net::IpAddr, Option<String>)> {
    sdn.status
        .centrals
        .iter()
        .map(|c| {
            let host = nodes
                .iter()
                .find(|n| n.id == c.node)
                .and_then(|n| host_of(&n.spec.address))
                .map(String::from);
            (c.address, host)
        })
        .collect()
}

/// 논리 포트의 지금 상태(`GET /networks/{key}/ips`)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PortState {
    /// NB `Logical_Switch_Port.up`(northd가 씀 — chassis가 포트를 바인딩했다)
    pub up: bool,
    /// SB `Port_Binding.chassis`의 이름(노드 UUID). SB를 읽지 못했으면 None
    pub chassis: Option<String>,
}

/// API 노드가 우리 논리 포트(이름 → 상태)를 읽는다(설계 §9 보기). 읽기만 하므로 팔로어도 쓴다(`leader_only = false` —
/// 조금 낡았을 수 있다). NB를 읽지 못하면 오류, SB(바인딩 chassis)를 읽지 못하면 chassis 없이 돌려준다.
pub(crate) async fn read_ports(
    config: &OvnSyncConfig,
    centrals: &[(std::net::IpAddr, Option<String>)],
) -> Result<std::collections::BTreeMap<String, PortState>, OvsdbError> {
    let transport = match &config.transport {
        OvnTransport::Tcp => Transport::Tcp,
        OvnTransport::Tls { pki } => Transport::Tls(OvsdbTls::from_dir(pki)?),
    };
    let nb = OvsdbClient::connect_any(
        &transport,
        &OvnSyncer::endpoints(centrals, config.nb_port, None),
        NB_DB,
        false,
        config.timeout,
    )
    .await?;
    let ours = vec![cond(
        "external_ids",
        "includes",
        opl_driver::ovn::ovsdb::map_value(&std::collections::BTreeMap::from([(
            nb::CLUSTER_KEY.to_string(),
            config.tag.clone(),
        )])),
    )];
    let mut out = std::collections::BTreeMap::new();
    for row in nb
        .select(NB_DB, "Logical_Switch_Port", ours, &["name", "up"])
        .await?
    {
        let Some(name) = row.get("name").and_then(Value::as_str) else {
            continue;
        };
        let up = row
            .get("up")
            .and_then(opl_driver::ovn::ovsdb::parse_bool)
            .unwrap_or(false);
        out.insert(name.to_string(), PortState { up, chassis: None });
    }
    match Box::pin(read_bindings(config, &transport, centrals)).await {
        Ok(bindings) => {
            for (port, state) in &mut out {
                state.chassis = bindings.get(port).cloned();
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot read the OVN port bindings"),
    }
    Ok(out)
}

/// SB의 논리 포트 → 바인딩한 chassis 이름
async fn read_bindings(
    config: &OvnSyncConfig,
    transport: &Transport,
    centrals: &[(std::net::IpAddr, Option<String>)],
) -> Result<std::collections::BTreeMap<String, String>, OvsdbError> {
    let sb = OvsdbClient::connect_any(
        transport,
        &OvnSyncer::endpoints(centrals, config.sb_port, None),
        SB_DB,
        false,
        config.timeout,
    )
    .await?;
    let chassis: std::collections::BTreeMap<String, String> = sb
        .select(SB_DB, "Chassis", vec![], &["_uuid", "name"])
        .await?
        .into_iter()
        .filter_map(|r| {
            let uuid = parse_uuids(r.get("_uuid")?).into_iter().next()?;
            Some((uuid, r.get("name")?.as_str()?.to_string()))
        })
        .collect();
    Ok(sb
        .select(SB_DB, "Port_Binding", vec![], &["logical_port", "chassis"])
        .await?
        .into_iter()
        .filter_map(|r| {
            let port = r.get("logical_port")?.as_str()?.to_string();
            let uuid = parse_uuids(r.get("chassis")?).into_iter().next()?;
            Some((port, chassis.get(&uuid)?.clone()))
        })
        .collect())
}
