//! 굶김 시험의 고리(설계 §3.4). 시험 빌드(`starve-it`)에만 있고 제어 소켓에 경로 둘을 더한다.
//!
//! - `POST /v1/test/block-api`: `api` 실행기의 작업자를 모두 `[test] block_api_ms` 동안 동기 sleep으로
//!   막는다. 모두 막힌 뒤에 답한다.
//! - `GET /v1/test/raft`: 이 노드가 아는 Raft 리더·임기와, 리더면 노드별 HA 리스 나이(밀리초)

use std::collections::BTreeMap;
use std::sync::{Arc, Barrier, Mutex, OnceLock, Weak};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post};
use opl_cluster::Cluster;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::time::Instant;
use uuid::Uuid;

struct Hooks {
    api: Handle,
    block: Duration,
    /// 지금 멤버의 클러스터 저장소. 닫힌 저장소를 붙잡지 않게 약한 참조로 둔다.
    cluster: Mutex<Weak<Cluster>>,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// 실행기를 만든 뒤 한 번 부른다.
pub fn install(api: Handle, block: Duration) {
    let hooks = Hooks {
        api,
        block,
        cluster: Mutex::new(Weak::new()),
    };
    assert!(
        HOOKS.set(hooks).is_ok(),
        "the test hooks are already installed"
    );
}

/// 멤버가 되면(다시 시작 포함) 그 클러스터 저장소를 알린다.
pub fn set_cluster(cluster: Weak<Cluster>) {
    if let Some(hooks) = HOOKS.get() {
        *hooks.cluster.lock().unwrap() = cluster;
    }
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/v1/test/block-api", post(block_api))
        .route("/v1/test/raft", get(raft))
}

async fn block_api() -> StatusCode {
    let Some(hooks) = HOOKS.get() else {
        return StatusCode::NOT_FOUND;
    };
    let workers = hooks.api.metrics().num_workers();
    let started = Arc::new(Barrier::new(workers + 1));
    for _ in 0..workers {
        let started = started.clone();
        let block = hooks.block;
        hooks.api.spawn(async move {
            started.wait();
            std::thread::sleep(block);
        });
    }
    // 작업자가 모두 하나씩 받아 막힐 때까지 기다린다(막힌 작업자는 다음 작업을 받지 못한다).
    let _ = tokio::task::spawn_blocking(move || started.wait()).await;
    StatusCode::NO_CONTENT
}

async fn raft() -> Result<axum::Json<Value>, StatusCode> {
    let cluster = HOOKS
        .get()
        .and_then(|h| h.cluster.lock().unwrap().upgrade())
        .ok_or(StatusCode::NOT_FOUND)?;
    let leases = cluster.leases();
    let now = Instant::now();
    let ages: BTreeMap<Uuid, u128> = leases
        .renewed
        .iter()
        .map(|(id, at)| (id.0, now.saturating_duration_since(*at).as_millis()))
        .collect();
    Ok(axum::Json(json!({
        "node": cluster.id().0,
        "leader": cluster.members().leader.map(|id| id.0),
        "term": leases.term,
        "lease_age_ms": ages,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtimes;

    /// 답하면 `api`의 작업자가 모두 막혀 새 작업을 돌리지 못하고, 다른 실행기는 그대로 돈다.
    #[test]
    fn block_api_occupies_every_api_worker() {
        let rt = Runtimes::new(3, 2).unwrap();
        let h = rt.handles();
        install(h.api.clone(), Duration::from_secs(2));
        rt.core.block_on(async {
            assert_eq!(block_api().await, StatusCode::NO_CONTENT);
            let api = h.api.spawn(async {});
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(!api.is_finished());
            h.work.spawn(async {}).await.unwrap();
            api.await.unwrap();
        });
    }
}
