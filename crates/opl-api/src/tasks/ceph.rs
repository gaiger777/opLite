//! Ceph 자동 구성(계획 56)의 Task: 부트스트랩(`ceph-init`), mon·mgr·OSD 더하기, 풀 만들기와 등록, 클라이언트
//! 꾸러미 설치. 노드에서 하는 일은 [`CephHost`]의 단계 함수가 하고(모두 멱등), 여기서는 Raft에서 필요한 것을
//! 읽어 단계를 차례로 부르고, 데몬이 실제로 섰음을 확인한 뒤에만 `Ceph` 리소스의 상태에 그 노드를 더한다.
//! 단계마다 진행률 stage를 단계 이름으로 쓴다(바이트 0/0). 실패한 Task는 같은 요청을 다시 하면 이어서 한다.

use std::net::IpAddr;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::MirrorProgress;
use opl_driver::ceph::{
    CephHost, CephVersion, MonInit, MonmapSource, PoolPlan, check_release, parse_osd_tree,
    render_client_conf, render_conf,
};
use opl_types::ceph::{
    CEPH_NAME, CLIENT_ID, CephCluster, CephMon, CephPhase, CephSpec, CephStatus, MAX_MONS, mon_addr,
};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, pool_listed_on};
use opl_types::task::{Task, TaskOp};
use uuid::Uuid;

use super::Ctx;
use super::migrate::write_progress;
use crate::resources::{check_name, check_pool_spec};

/// Ceph Task를 돌린다.
pub(super) async fn perform(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let host = ctx
        .config
        .ceph
        .as_deref()
        .ok_or("this node cannot run Ceph tasks")?;
    let ceph = ctx
        .cluster
        .get::<CephSpec>(task.spec.target)
        .map_err(|e| e.to_string())?
        .ok_or("no Ceph cluster is set up")?;
    let me = ctx
        .cluster
        .get::<NodeSpec>(ctx.config.node)
        .map_err(|e| e.to_string())?
        .ok_or("this node is not registered")?;
    let run = Run {
        ctx,
        task: task.id,
        host,
        ceph,
        me,
    };
    match &task.spec.op {
        TaskOp::CephInit => run.init().await,
        TaskOp::CephMonAdd => run.add_mon().await,
        TaskOp::CephMgrAdd => run.add_mgr().await,
        TaskOp::CephOsdAdd { device, wipe } => run.add_osd(device, *wipe).await,
        TaskOp::CephPoolCreate { name, pool, size } => run.create_pool(name, pool, *size).await,
        TaskOp::CephClientInstall => run.install_client().await,
        op => Err(format!("{op:?} is not a Ceph task")),
    }
}

/// 끝난 Ceph Task(`error`면 실패)의 뒤처리: 이 Task가 잡은 자리(`pending`)를 놓고, 실패한 부트스트랩이면 그 이유를
/// `Ceph` 리소스에 남긴다(phase는 그대로 — 다시 하면 이어서 한다). 다른 노드로 옮겨 간 부트스트랩의 버려진 Task는
/// 이유를 남기지 않는다(지금 부트스트랩의 오류를 덮지 않게).
pub(super) async fn finished(ctx: &Ctx, task: &Task, error: Option<&str>) {
    let result = update_ceph(&ctx.cluster, |s| {
        let mut changed = false;
        if s.pending.as_ref().is_some_and(|p| p.task == task.id) {
            s.pending = None;
            changed = true;
        }
        if task.spec.op == TaskOp::CephInit
            && s.init_node == Some(task.spec.node)
            && let Some(error) = error
            && s.last_error.as_deref() != Some(error)
        {
            s.last_error = Some(error.to_string());
            changed = true;
        }
        changed
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot record the end of a Ceph task");
    }
}

/// `Ceph` 리소스의 상태를 `change`로 바꾼다(읽은 revision 확인 — 다른 쓰기와 부딪치면 다시 읽어 바꾼다).
/// `change`가 거짓이면 쓰지 않는다. 리소스가 없으면 아무것도 하지 않는다.
pub(super) async fn update_ceph(
    cluster: &Cluster,
    mut change: impl FnMut(&mut CephStatus) -> bool,
) -> Result<(), String> {
    loop {
        let Some(current) = cluster
            .get_by_name::<CephSpec>(CEPH_NAME)
            .map_err(|e| e.to_string())?
        else {
            return Ok(());
        };
        let mut status = current.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<CephSpec>(current.id, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 목록에 없으면 더한다(더했으면 참).
fn add_node(list: &mut Vec<Uuid>, node: Uuid) -> bool {
    if list.contains(&node) {
        return false;
    }
    list.push(node);
    true
}

/// 도는 Ceph Task 하나
struct Run<'a> {
    ctx: &'a Ctx,
    task: Uuid,
    host: &'a CephHost,
    ceph: CephCluster,
    /// 이 노드
    me: Node,
}

impl Run<'_> {
    fn spec(&self) -> &CephSpec {
        &self.ceph.spec
    }

    fn status(&self) -> &CephStatus {
        &self.ceph.status
    }

    fn name(&self) -> &str {
        &self.me.name
    }

    /// 진행률 stage를 쓴다(바이트 0/0).
    async fn stage(&self, stage: &str) {
        write_progress(self.ctx, self.task, stage, MirrorProgress::default()).await;
    }

    /// 이 부트스트랩 Task가 아직 이 클러스터의 부트스트랩인지: 부트스트랩 노드가 이 노드이고, 자리(`pending`)가 다른
    /// Task의 것이 아니다. 부트스트랩을 다른 노드로 옮기면(`POST /ceph` 다시 하기) 거짓이 된다.
    fn owns_init(&self, s: &CephStatus) -> bool {
        s.init_node == Some(self.me.id) && s.pending.as_ref().is_none_or(|p| p.task == self.task)
    }

    /// 부트스트랩을 다른 노드로 옮겼으면 멈춘다(이 노드가 같은 fsid로 두 번째 클러스터를 세우지 않게).
    fn moved(&self) -> String {
        format!(
            "the Ceph bootstrap was moved off node {:?}; this task stops",
            self.name()
        )
    }

    /// 새로 읽은 `Ceph` 리소스로 [`owns_init`](Self::owns_init)를 확인한다.
    fn check_init(&self) -> Result<(), String> {
        let ceph = self
            .ctx
            .cluster
            .get_by_name::<CephSpec>(CEPH_NAME)
            .map_err(|e| e.to_string())?
            .ok_or("no Ceph cluster is set up")?;
        if ceph.id != self.ceph.id || !self.owns_init(&ceph.status) {
            return Err(self.moved());
        }
        Ok(())
    }

    /// 부트스트랩 Task의 상태 바꾸기: [`update_ceph`]와 같되 이 Task가 부트스트랩이 아니게 됐으면 쓰지 않고 실패한다.
    async fn update_init(
        &self,
        mut change: impl FnMut(&mut CephStatus) -> bool,
    ) -> Result<(), String> {
        let mut lost = false;
        update_ceph(&self.ctx.cluster, |s| {
            if !self.owns_init(s) {
                lost = true;
                return false;
            }
            change(s)
        })
        .await?;
        if lost {
            return Err(self.moved());
        }
        Ok(())
    }

    /// 부트스트랩 뒤의 Task는 클러스터가 준비된 뒤에만 돈다.
    fn require_ready(&self) -> Result<(), String> {
        if self.status().phase != CephPhase::Ready {
            return Err("the Ceph cluster is not ready yet".into());
        }
        Ok(())
    }

    /// `checking`: Linux인지, 이 노드에 남의 Ceph가 없는지
    async fn check(&self) -> Result<(), String> {
        self.stage("checking").await;
        self.host.check_platform().map_err(|e| e.to_string())?;
        self.host
            .check_foreign(self.name(), self.spec().fsid)
            .map_err(|e| e.to_string())
    }

    /// `installing-packages`: 데몬 꾸러미를 깔고 판이 클러스터와 같은지 본다.
    async fn install_daemon(&self) -> Result<CephVersion, String> {
        self.stage("installing-packages").await;
        let version = self
            .host
            .ensure_packages(&self.host.options().packages)
            .await
            .map_err(|e| e.to_string())?;
        check_release(self.name(), &version, self.status().release.as_deref())
            .map_err(|e| e.to_string())?;
        Ok(version)
    }

    /// 이 노드의 mon(이름 = 노드 이름, 주소 = public 망 안의 주소)
    fn mon(&self) -> Result<CephMon, String> {
        let net = self.spec().public_network;
        let addr: IpAddr = mon_addr(&self.me, &net).ok_or_else(|| {
            format!(
                "node {:?} has no address in the Ceph public network {net}",
                self.name()
            )
        })?;
        Ok(CephMon {
            node: self.me.id,
            name: self.me.name.clone(),
            addr,
        })
    }

    /// 데몬 노드의 `/etc/ceph/ceph.conf`(`extra`는 아직 정족수에 들지 않은 이 노드의 mon)와 admin 키링
    fn write_daemon_conf(&self, extra: Option<&CephMon>) -> Result<(), String> {
        let conf = render_conf(self.spec(), &self.status().mons, extra);
        self.host.write_conf(&conf).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 이 노드의 클라이언트 설정(`mons`로). 에이전트도 쓰지만 Task가 그보다 먼저 쓸 수 있다.
    fn write_client_conf(&self, mons: &[CephMon]) -> Result<(), String> {
        let keyring = self.host.client_keyring().to_string_lossy().into_owned();
        let client = render_client_conf(self.spec(), mons, &keyring);
        self.host
            .write_client(&client, &self.spec().keys.client)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn write_admin_keyring(&self) -> Result<(), String> {
        self.host
            .write_admin_keyring(&self.spec().keys.admin)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// `ceph-init`: 첫 mon·mgr을 세우고 클러스터를 `ready`로 한다.
    async fn init(&self) -> Result<(), String> {
        self.check().await?;
        let version = self.install_daemon().await?;
        let release = version.release();
        self.update_init(|s| {
            if s.release.as_deref() == Some(release.as_str()) {
                return false;
            }
            s.release = Some(release.clone());
            true
        })
        .await?;

        self.stage("writing-config").await;
        let mon = self.mon()?;
        self.write_daemon_conf(Some(&mon))?;
        self.write_admin_keyring()?;
        // 에이전트는 mon이 Raft에 오른 뒤에 쓰므로 그 전에 이 노드의 클라이언트 설정을 둔다.
        let mut mons = self.status().mons.clone();
        if !mons.iter().any(|m| m.name == mon.name) {
            mons.push(mon.clone());
        }
        self.write_client_conf(&mons)?;

        // mon 저장소를 만들기 바로 전에 다시 본다.
        self.check_init()?;
        self.start_mon(&mon, MonmapSource::New).await?;

        self.stage("creating-manager").await;
        self.start_mgr().await?;
        self.host.wait_manager().await.map_err(|e| e.to_string())?;

        self.update_init(|s| {
            if s.phase == CephPhase::Ready && s.last_error.is_none() {
                return false;
            }
            s.phase = CephPhase::Ready;
            s.last_error = None;
            true
        })
        .await
    }

    /// `creating-monitor`·`starting-monitor`·`waiting-for-quorum`: mon 저장소를 만들고 켜서 정족수에 들면
    /// Raft의 `mons`에 더한다. 기존 정족수에 더하면 정족수는 Raft의 mon들에만 묻는다. 부트스트랩(`New`)은 그사이
    /// 부트스트랩이 다른 노드로 옮겨 갔으면 `mons`에 더하지 않고 실패한다.
    async fn start_mon(&self, mon: &CephMon, monmap: MonmapSource) -> Result<(), String> {
        self.stage("creating-monitor").await;
        self.host
            .create_monitor(&MonInit {
                name: &mon.name,
                addr: mon.addr,
                fsid: self.spec().fsid,
                keys: &self.spec().keys,
                monmap,
                init_node: self.status().init_node == Some(self.me.id),
            })
            .await
            .map_err(|e| e.to_string())?;
        self.stage("starting-monitor").await;
        self.host
            .start_monitor(&mon.name)
            .await
            .map_err(|e| e.to_string())?;
        self.stage("waiting-for-quorum").await;
        let quorum: Vec<IpAddr> = match monmap {
            MonmapSource::New => Vec::new(),
            MonmapSource::Existing => self
                .status()
                .mons
                .iter()
                .filter(|m| m.node != mon.node)
                .map(|m| m.addr)
                .collect(),
        };
        self.host
            .wait_quorum(&mon.name, &quorum)
            .await
            .map_err(|e| e.to_string())?;
        let add = |s: &mut CephStatus| {
            if s.mons.iter().any(|m| m.node == mon.node) {
                return false;
            }
            s.mons.push(mon.clone());
            true
        };
        match monmap {
            MonmapSource::New => self.update_init(add).await,
            MonmapSource::Existing => update_ceph(&self.ctx.cluster, add).await,
        }
    }

    /// mgr을 만들고 켜서(유닛이 active) Raft의 `mgrs`에 더한다.
    async fn start_mgr(&self) -> Result<(), String> {
        self.host
            .create_manager(self.name(), self.spec().fsid)
            .await
            .map_err(|e| e.to_string())?;
        let node = self.me.id;
        update_ceph(&self.ctx.cluster, |s| add_node(&mut s.mgrs, node)).await
    }

    /// `ceph-mon-add`: 기존 정족수에 이 노드의 mon을 더한다.
    async fn add_mon(&self) -> Result<(), String> {
        self.require_ready()?;
        let mons = &self.status().mons;
        if !mons.iter().any(|m| m.node == self.me.id) && mons.len() >= MAX_MONS {
            return Err(format!(
                "a Ceph cluster here can have at most {MAX_MONS} monitors"
            ));
        }
        self.check().await?;
        self.install_daemon().await?;
        self.stage("writing-config").await;
        let mon = self.mon()?;
        self.write_daemon_conf(Some(&mon))?;
        self.write_admin_keyring()?;
        self.start_mon(&mon, MonmapSource::Existing).await
    }

    /// `ceph-mgr-add`: 이 노드에 (대기) mgr을 더한다.
    async fn add_mgr(&self) -> Result<(), String> {
        self.require_ready()?;
        self.check().await?;
        self.install_daemon().await?;
        self.stage("writing-config").await;
        self.write_daemon_conf(None)?;
        self.write_admin_keyring()?;
        self.stage("creating-manager").await;
        self.start_mgr().await
    }

    /// `ceph-osd-add`: 이 노드의 디스크 `device`에 OSD를 만든다(노드에서 디스크를 다시 가른다).
    async fn add_osd(&self, device: &str, wipe: bool) -> Result<(), String> {
        self.require_ready()?;
        self.check().await?;
        self.install_daemon().await?;
        self.stage("writing-config").await;
        self.write_daemon_conf(None)?;
        self.host
            .write_bootstrap_osd(&self.spec().keys.bootstrap_osd)
            .await
            .map_err(|e| e.to_string())?;
        // OSD가 up인지는 client.orbivisor로 본다(이 노드에 admin 키링이 없을 수 있다).
        self.write_client_conf(&self.status().mons)?;
        self.stage("preparing-osd").await;
        let pool_vgs = self.pool_vgs()?;
        let id = self
            .host
            .create_osd(device, wipe, self.spec().fsid, &pool_vgs)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(osd = id, device, "Ceph OSD is up");
        let node = self.me.id;
        update_ceph(&self.ctx.cluster, |s| add_node(&mut s.osd_nodes, node)).await
    }

    /// 이 노드가 쓰는 Orbivisor LVM 풀(LVM-thin·공유 LVM)의 (VG, 풀 이름). 그 PV는 OSD로 쓰지 않는다.
    fn pool_vgs(&self) -> Result<Vec<(String, String)>, String> {
        let (_, pools) = self
            .ctx
            .cluster
            .list::<StoragePoolSpec>()
            .map_err(|e| e.to_string())?;
        Ok(pools
            .into_iter()
            .filter(|p| pool_listed_on(p, self.me.id))
            .filter_map(|p| match &p.spec.backend {
                PoolBackend::LvmThin { vg, .. } | PoolBackend::LvmShared { vg, .. } => {
                    Some((vg.clone(), p.name.clone()))
                }
                _ => None,
            })
            .collect())
    }

    /// `ceph-pool-create`: Ceph 풀을 만들어 Orbivisor RBD 풀 `name`(사용자 `orbivisor`, 관리 conf)으로 등록한다.
    async fn create_pool(&self, name: &str, pool: &str, size: u8) -> Result<(), String> {
        self.require_ready()?;
        self.stage("checking").await;
        self.host.check_platform().map_err(|e| e.to_string())?;
        // Ceph 풀을 만들기 전에 등록할 수 있는지 본다(이름·RBD 값).
        let mut spec = StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: pool.to_string(),
                namespace: None,
                user: Some(CLIENT_ID.to_string()),
                conf: Some(self.host.client_conf().to_string_lossy().into_owned()),
            },
            nodes: Vec::new(),
            shared: true,
            backups: false,
        };
        check_name(name).map_err(|e| e.message)?;
        check_pool_spec(&self.ctx.cluster, &mut spec, true).map_err(|e| e.message)?;
        let exists = self
            .ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(name)
            .map_err(|e| e.to_string())?
            .is_some();
        if exists {
            return Err(format!("storage pool {name:?} already exists"));
        }

        self.stage("creating-pool").await;
        let tree = self
            .host
            .ceph(&["osd", "tree", "--format", "json"])
            .await
            .map_err(|e| e.to_string())?;
        let mut hosts: Vec<String> = parse_osd_tree(&tree)?
            .into_iter()
            .filter(|o| o.up)
            .map(|o| o.host)
            .collect();
        hosts.sort();
        hosts.dedup();
        self.host
            .create_pool(&PoolPlan {
                pool: pool.to_string(),
                name: name.to_string(),
                size,
                osd_hosts: hosts.len() as u32,
            })
            .await
            .map_err(|e| e.to_string())?;

        self.stage("registering-pool").await;
        let resource = Resource::new(name, spec);
        match self.ctx.cluster.create(&resource).await {
            Ok(_) => Ok(()),
            // 그사이 같은 이름의 풀을 만들었다. Ceph 풀은 남긴다.
            Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => Err(format!(
                "storage pool {name:?} already exists; the Ceph pool {pool} was created — register \
                 it with: pool create NAME --rbd {pool} --rbd-user {CLIENT_ID} --rbd-conf {}",
                self.host.client_conf().display()
            )),
            Err(e) => Err(e.to_string()),
        }
    }

    /// `ceph-client-install`: 클라이언트 꾸러미만 깐다(클라이언트 설정은 에이전트가 쓴다).
    async fn install_client(&self) -> Result<(), String> {
        self.stage("checking").await;
        self.host.check_platform().map_err(|e| e.to_string())?;
        self.stage("installing-packages").await;
        self.host
            .ensure_packages(&self.host.options().client_packages)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
