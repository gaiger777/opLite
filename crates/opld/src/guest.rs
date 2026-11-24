//! 게스트 정보 캐시(계획 59 §8). 이 노드에서 도는 VM의 게스트 에이전트가 답한 정보를 노드 메모리에 둔다(Raft에 쓰지
//! 않는다, 계획 44 지표와 같다). reconciler 작업자가 60초마다 모아 넣고([`GuestInfoSink`]), API와 다른 노드(노드 간 RPC
//! `GuestAgent`)가 읽는다([`GuestAgentSource`]). VM이 꺼지거나 노드를 떠나면 작업자가 지운다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_driver::{DriverError, Hypervisor};
use opl_peer::{GuestAgentError, GuestAgentSource};
use opl_reconciler::GuestInfoSink;
use opl_types::file_restore::{GuestFileAnswer, GuestFileOp};
use opl_types::guest::{GuestInfo, GuestInfoResponse, GuestInfoState};
use opl_types::vm::VmSpec;
use tokio::time::Instant;
use uuid::Uuid;

/// `refresh`로 지금 묻는 것은 VM마다 이만큼에 한 번이다. 그 사이에는 캐시를 돌려준다.
pub const REFRESH_EVERY: Duration = Duration::from_secs(5);

/// 이 노드에서 도는 VM의 게스트 정보
pub struct GuestInfoStore {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    infos: Mutex<HashMap<Uuid, GuestInfo>>,
    /// VM마다 마지막으로 `refresh`로 물은 때
    refreshed: Mutex<HashMap<Uuid, Instant>>,
}

impl GuestInfoStore {
    pub fn new(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>) -> Self {
        Self {
            cluster,
            hv,
            infos: Mutex::default(),
            refreshed: Mutex::default(),
        }
    }

    fn cached(&self, vm: Uuid) -> Option<GuestInfo> {
        self.infos.lock().unwrap().get(&vm).cloned()
    }

    /// 지금 물어도 되면(마지막으로 물은 지 [`REFRESH_EVERY`]가 지났으면) 그 시각을 적고 true다.
    fn may_refresh(&self, vm: Uuid) -> bool {
        let now = Instant::now();
        let mut refreshed = self.refreshed.lock().unwrap();
        if refreshed
            .get(&vm)
            .is_some_and(|at| now.duration_since(*at) < REFRESH_EVERY)
        {
            return false;
        }
        refreshed.insert(vm, now);
        true
    }

    /// VM이 이 노드에서 도는지
    async fn running(&self, vm: Uuid) -> bool {
        matches!(self.hv.inspect(vm).await, Ok(Some(_)))
    }
}

/// 드라이버 오류를 게스트 에이전트 오류로 바꾼다. 채널이 없으면 `NoAgent`, 에이전트가 답하지 않으면 `Unavailable`
/// (API는 409 `guest_agent_unavailable`)이다.
pub fn agent_error(e: DriverError) -> GuestAgentError {
    match e {
        DriverError::NotRunning(_) => GuestAgentError::NotRunning,
        DriverError::InvalidSpec(_) => GuestAgentError::NoAgent,
        DriverError::GuestAgent(m) => GuestAgentError::Unavailable(m),
        e => GuestAgentError::Unavailable(e.to_string()),
    }
}

impl GuestInfoSink for GuestInfoStore {
    fn put(&self, vm: Uuid, info: GuestInfo) {
        self.infos.lock().unwrap().insert(vm, info);
    }

    fn remove(&self, vm: Uuid) {
        self.infos.lock().unwrap().remove(&vm);
        self.refreshed.lock().unwrap().remove(&vm);
    }
}

#[async_trait]
impl GuestAgentSource for GuestInfoStore {
    /// 이 노드에서 돌지 않으면 `not_running`, 에이전트 채널이 없으면 `no_agent`다. `refresh`면(VM마다 5초에 한 번) 지금
    /// 묻고, 답하지 않으면 앞 정보를 돌려준다. 정보가 한 번도 없으면 `no_agent`다(에이전트가 아직 답한 적이 없음).
    async fn info(&self, vm: Uuid, refresh: bool) -> GuestInfoResponse {
        let response = |state, info| GuestInfoResponse { state, info };
        if !self.running(vm).await {
            return response(GuestInfoState::NotRunning, None);
        }
        let channel =
            matches!(self.cluster.get::<VmSpec>(vm), Ok(Some(v)) if v.spec.agent_channel());
        if !channel {
            return response(GuestInfoState::NoAgent, None);
        }
        if refresh && self.may_refresh(vm) {
            match self.hv.guest_info(vm).await {
                Ok(info) => self.put(vm, info),
                Err(e) => tracing::debug!(%vm, error = %e, "cannot refresh guest information"),
            }
        }
        match self.cached(vm) {
            Some(info) => response(GuestInfoState::Answered, Some(info)),
            None => response(GuestInfoState::NoAgent, None),
        }
    }

    async fn set_password(
        &self,
        vm: Uuid,
        user: &str,
        password: &str,
    ) -> Result<(), GuestAgentError> {
        if !self.running(vm).await {
            return Err(GuestAgentError::NotRunning);
        }
        self.hv
            .set_user_password(vm, user, password)
            .await
            .map_err(agent_error)
    }

    /// 파일 일 하나(계획 69 §12). 이 노드에서 돌지 않으면 `NotRunning`이다.
    async fn file(&self, vm: Uuid, op: GuestFileOp) -> Result<GuestFileAnswer, GuestAgentError> {
        if !self.running(vm).await {
            return Err(GuestAgentError::NotRunning);
        }
        self.hv.guest_file(vm, &op).await.map_err(agent_error)
    }
}

#[cfg(test)]
mod tests {
    use opl_driver::{GuestAnswer, MockHypervisor};
    use opl_types::resource::Resource;
    use opl_types::runtime::VmRuntimeSpec;

    use super::*;
    use crate::bootstrap::tests::single_node;

    async fn store() -> (tempfile::TempDir, Arc<MockHypervisor>, GuestInfoStore) {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(&dir.path().join("cluster")).await;
        let hv = Arc::new(MockHypervisor::default());
        let store = GuestInfoStore::new(Arc::new(cluster), hv.clone());
        (dir, hv, store)
    }

    /// 게스트 에이전트를 켠 VM을 클러스터에 만든다.
    async fn create_vm(store: &GuestInfoStore) -> Uuid {
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256, "firmware": "uefi",
            "machine": "virt", "disks": [], "nics": [], "power": "stopped", "guest_agent": {}
        }))
        .unwrap();
        let vm = Resource::new("win", spec);
        store.cluster.create(&vm).await.unwrap();
        vm.id
    }

    async fn start(hv: &MockHypervisor, vm: Uuid) {
        let spec: VmRuntimeSpec = serde_json::from_value(serde_json::json!({
            "id": vm, "name": "win", "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256,
            "firmware": "uefi", "machine": "virt", "disks": [], "nics": [], "guest_agent": true
        }))
        .unwrap();
        hv.start(&spec, None).await.unwrap();
    }

    #[tokio::test]
    async fn info_reports_not_running_no_agent_and_answers() {
        let (_dir, hv, store) = store().await;
        // 클러스터에 없는(채널을 모르는) VM은 no_agent다.
        let other = Uuid::now_v7();
        start(&hv, other).await;
        assert_eq!(
            store.info(other, false).await.state,
            GuestInfoState::NoAgent
        );

        let vm = create_vm(&store).await;
        assert_eq!(store.info(vm, true).await.state, GuestInfoState::NotRunning);
        start(&hv, vm).await;
        // 아직 답한 적이 없다.
        hv.set_guest_agent(vm, GuestAnswer::Hang);
        assert_eq!(store.info(vm, true).await.state, GuestInfoState::NoAgent);
        // 작업자가 모은 정보를 돌려준다.
        let info = GuestInfo {
            hostname: Some("win".into()),
            ..Default::default()
        };
        store.put(vm, info.clone());
        let got = store.info(vm, false).await;
        assert_eq!(got.state, GuestInfoState::Answered);
        assert_eq!(got.info, Some(info));
        // 새로 고치기는 5초에 한 번이다(방금 물었으므로 묻지 않는다).
        hv.set_guest_agent(vm, GuestAnswer::Ok);
        let fresh = GuestInfo {
            hostname: Some("win-2".into()),
            ..Default::default()
        };
        hv.set_guest_info(vm, fresh.clone());
        let calls = hv.guest_info_calls(vm);
        store.info(vm, true).await;
        assert_eq!(hv.guest_info_calls(vm), calls);
        store.refreshed.lock().unwrap().clear();
        assert_eq!(store.info(vm, true).await.info, Some(fresh));
        store.remove(vm);
        assert!(store.cached(vm).is_none());
    }

    #[tokio::test]
    async fn refresh_is_limited_per_vm() {
        let (_dir, _hv, store) = store().await;
        let vm = Uuid::now_v7();
        assert!(store.may_refresh(vm));
        assert!(!store.may_refresh(vm));
        assert!(store.may_refresh(Uuid::now_v7()));
        store.remove(vm);
        assert!(store.may_refresh(vm));
    }

    #[tokio::test]
    async fn passwords_map_agent_errors() {
        let (_dir, hv, store) = store().await;
        let vm = Uuid::now_v7();
        assert_eq!(
            store.set_password(vm, "root", "pw").await,
            Err(GuestAgentError::NotRunning)
        );
        start(&hv, vm).await;
        hv.set_guest_agent(vm, GuestAnswer::Hang);
        assert!(matches!(
            store.set_password(vm, "root", "pw").await,
            Err(GuestAgentError::Unavailable(_))
        ));
        hv.set_guest_agent(vm, GuestAnswer::Ok);
        store.set_password(vm, "root", "pw").await.unwrap();
        assert_eq!(hv.password_sets(vm), ["root"]);
    }

    #[tokio::test]
    async fn file_ops_reach_the_hypervisor() {
        let (_dir, hv, store) = store().await;
        let vm = Uuid::now_v7();
        assert_eq!(
            store.file(vm, GuestFileOp::Caps).await,
            Err(GuestAgentError::NotRunning)
        );
        start(&hv, vm).await;
        hv.set_guest_agent(vm, GuestAnswer::Ok);
        hv.set_guest_fs(vm, opl_driver::MockGuestFs::linux(&["/srv"]));
        let open = GuestFileOp::Open {
            path: "/srv/a".into(),
        };
        assert!(matches!(
            store.file(vm, open).await,
            Ok(GuestFileAnswer::Opened { .. })
        ));
        assert_eq!(
            hv.guest_fs(vm).unwrap().files.keys().collect::<Vec<_>>(),
            ["/srv/a"]
        );
    }
}
