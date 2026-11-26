//! 운영자 대시보드의 두 읽기(계획 75 설계 §3.2·§3.3): 물리 ↔ 가상 네트워크 관계(`GET /topology/physical`)와 노드별
//! 용량(`GET /capacity`). 계산은 `orbivisor_types::{physical, capacity}`의 순수 함수이고, 여기서는 상태 머신에서 입력을
//! 읽고 결과를 끝점마다 하나씩 메모리에 둔다: 입력(상태 머신 리비전, 용량은 노드 부하도)이 같으면 다시 계산하지 않는다.
//! 권한은 경로표의 루트 `cluster.read`, 온라인 노드가 모두 기능 `operator-dashboard`를 알아야 한다(아니면 409
//! `cluster_upgrading`). 꺼진 옛 노드는 막지 않는다(리뷰 1 — 지우지 않은 죽은 노드 하나가 대시보드를 영영 막지 않게):
//! 그 노드는 `legacy`로 표시되고 마지막으로 본 상태를 보인다.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use orbivisor_types::capacity::{self, Capacity};
use orbivisor_types::network::NetworkSpec;
use orbivisor_types::node::{NodeSpec, OPERATOR_DASHBOARD};
use orbivisor_types::physical::{self, PhysicalTopology};
use orbivisor_types::storage::{StoragePoolSpec, VolumeSpec};
use orbivisor_types::vm::VmSpec;
use uuid::Uuid;

use crate::error::{ApiError, Json};
use crate::state::AppState;

/// 마지막 결과 하나: 그때의 입력 열쇠와 값. 같은 열쇠면 값을 그대로 돌려준다.
pub(crate) struct Memo<K, V>(Mutex<Option<(K, V)>>);

impl<K, V> Default for Memo<K, V> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<K: PartialEq, V: Clone> Memo<K, V> {
    /// 열쇠가 같으면 둔 값, 아니면 `compute`로 새로 계산해 둔다. 계산은 잠금 밖에서 한다(느린 계산이 다른 읽기를
    /// 막지 않게 — 동시에 둘이 계산하면 나중 것이 남는다).
    fn get_or<E>(&self, key: K, compute: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        if let Some((k, v)) = self.0.lock().expect("memo lock").as_ref()
            && *k == key
        {
            return Ok(v.clone());
        }
        let v = compute()?;
        *self.0.lock().expect("memo lock") = Some((key, v.clone()));
        Ok(v)
    }
}

/// 용량 계산의 입력 열쇠: 상태 머신 리비전과 노드마다 부하(용량이 쓰는 칸 — 1분 CPU 사용률·사용 메모리)
type CapacityKey = (u64, Vec<(Uuid, Option<u16>, Option<u64>)>);

/// 온라인 노드가 모두 `operator-dashboard`를 아는지(설계 §5: 업그레이드 중이면 웹은 지난 홈). 오프라인 옛 노드는 보지 않는다.
fn require_dashboard(cluster: &orbivisor_cluster::Cluster) -> Result<(), ApiError> {
    let mut old: Vec<String> = cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| n.status.online && !n.status.features.iter().any(|f| f == OPERATOR_DASHBOARD))
        .map(|n| n.name)
        .collect();
    if old.is_empty() {
        return Ok(());
    }
    old.sort();
    Err(ApiError::conflict(
        "cluster_upgrading",
        format!(
            "some nodes do not support operator dashboard yet ({}); upgrade them first",
            old.join(", ")
        ),
    ))
}

/// 두 끝점의 캐시(`AppState`에 하나)
#[derive(Default)]
pub(crate) struct DashboardCache {
    physical: Memo<u64, PhysicalTopology>,
    capacity: Memo<CapacityKey, Capacity>,
}

/// 물리 ↔ 가상 네트워크 관계: 노드마다 포트(본딩·VLAN·LLDP 이웃·역할)·브리지(업링크·네트워크·VM NIC 수)·underlay·
/// physnet과 포트·본딩 장애의 영향(위 브리지·네트워크·VM), 네트워크마다 노드별 레일 상태(up/degraded/down/absent).
/// 옛 노드(`legacy` — 꺼진 옛 노드만 닿는다)는 VLAN 아래 장치를 이름(`eno1.100`)으로 추정하고 MTU·LLDP가 빈다. 루트
/// `cluster.read`, 온라인 노드가 모두 `operator-dashboard`를 알아야 한다.
#[utoipa::path(get, path = "/topology/physical", tag = "nodes",
    responses((status = 200, body = PhysicalTopology), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn physical_topology(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PhysicalTopology>, ApiError> {
    require_dashboard(&state.cluster)?;
    let cluster = &state.cluster;
    // 리비전을 먼저 읽는다: 목록이 그 뒤의 것이어도 다음 읽기의 리비전이 달라 다시 계산할 뿐이다.
    let revision = cluster.revision();
    let topology = state.dashboard.physical.get_or(revision, || {
        let nodes = cluster.list::<NodeSpec>()?.1;
        let networks = cluster.list::<NetworkSpec>()?.1;
        let vms = cluster.list::<VmSpec>()?.1;
        Ok::<_, ApiError>(physical::build(&nodes, &networks, &vms))
    })?;
    Ok(Json(topology))
}

/// 노드별 용량: CPU(스레드·예약·전용·vCPU 할당·실행·오버커밋·사용률), 메모리(총·예약·할당·사용·hugepages·오버커밋),
/// NUMA 노드별, 로컬 풀(총·사용·프로비전 = 볼륨 크기 합·볼륨 수), 공유 풀, 온라인 노드의 합. 사용률·사용 메모리는
/// `GET /placement`의 노드 부하이고, 리더에 묻지 못하면 빈다(나머지는 그대로). 루트 `cluster.read`, 온라인 노드가 모두
/// `operator-dashboard`를 알아야 한다.
#[utoipa::path(get, path = "/capacity", tag = "nodes",
    responses((status = 200, body = Capacity), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_capacity(State(state): State<Arc<AppState>>) -> Result<Json<Capacity>, ApiError> {
    require_dashboard(&state.cluster)?;
    let loads = match crate::score::status(&state).await {
        Ok(s) => s.nodes,
        Err(e) => {
            tracing::debug!(error = ?e, "no node loads for the capacity summary");
            Vec::new()
        }
    };
    let cluster = &state.cluster;
    let key: CapacityKey = (
        cluster.revision(),
        loads
            .iter()
            .map(|l| (l.node, l.cpu1_pct, l.mem_used_mib))
            .collect(),
    );
    let out = state.dashboard.capacity.get_or(key, || {
        let nodes = cluster.list::<NodeSpec>()?.1;
        let vms = cluster.list::<VmSpec>()?.1;
        let volumes = cluster.list::<VolumeSpec>()?.1;
        let pools = cluster.list::<StoragePoolSpec>()?.1;
        Ok::<_, ApiError>(capacity::build(&nodes, &vms, &volumes, &pools, &loads))
    })?;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memo_keeps_the_value_while_the_key_stays() {
        let memo: Memo<u64, u32> = Memo::default();
        let mut runs = 0;
        let mut get = |key: u64, value: u32| {
            memo.get_or(key, || {
                runs += 1;
                Ok::<_, ()>(value)
            })
            .unwrap()
        };
        assert_eq!(get(1, 10), 10);
        assert_eq!(get(1, 20), 10, "the same key returns the stored value");
        assert_eq!(get(2, 30), 30);
        assert_eq!(runs, 2);
        // 계산이 실패하면 둔 값을 바꾸지 않는다.
        assert_eq!(memo.get_or(3, || Err::<u32, _>("no")), Err("no"));
        assert_eq!(memo.get_or(2, || Ok::<_, ()>(40)), Ok(30));
    }
}
