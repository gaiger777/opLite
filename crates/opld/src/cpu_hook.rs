//! CPU 모델 E2E의 고리(계획 63). 시험 빌드(`cpu-it`)에만 있고 제어 소켓에 API에 없는 읽기 둘을 더한다. 모델·플래그·숨김
//! 목록은 시험이 CLI로 바꾼다(스테이지 5).
//!
//! - `POST /v1/test/vcpu-props` `{vm, names}`: 이 노드에서 도는 VM의 vCPU마다 CPU 속성(QMP `qom-get`)
//! - `POST /v1/test/cpu-catalog`: 이 노드가 알린 CPU 카탈로그(없으면 404)

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::Cluster;
use opl_driver::Hypervisor;
use opl_types::cpu::CpuCatalogSpec;
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use serde::Deserialize;
use serde_json::{Value, json};

type Member = (Weak<Cluster>, Weak<dyn Hypervisor>);

static MEMBER: OnceLock<Mutex<Option<Member>>> = OnceLock::new();

/// 멤버가 되면(다시 시작 포함) 그 클러스터 저장소와 하이퍼바이저를 알린다.
pub fn set_member(cluster: Weak<Cluster>, hv: &Arc<dyn Hypervisor>) {
    *MEMBER.get_or_init(Default::default).lock().unwrap() = Some((cluster, Arc::downgrade(hv)));
}

type Live = (Arc<Cluster>, Arc<dyn Hypervisor>);

fn member() -> Result<Live, (StatusCode, String)> {
    let gone = || (StatusCode::NOT_FOUND, "not a member".to_string());
    let m = MEMBER.get().ok_or_else(gone)?.lock().unwrap().clone();
    let (c, hv) = m.ok_or_else(gone)?;
    Ok((
        c.upgrade().ok_or_else(gone)?,
        hv.upgrade().ok_or_else(gone)?,
    ))
}

fn bad(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/v1/test/vcpu-props", post(vcpu_props))
        .route("/v1/test/cpu-catalog", post(cpu_catalog))
}

#[derive(Deserialize)]
struct Props {
    vm: String,
    names: Vec<String>,
}

async fn vcpu_props(Json(b): Json<Props>) -> Result<Json<Value>, (StatusCode, String)> {
    let (cluster, hv) = member()?;
    let vm = cluster
        .get_by_name::<VmSpec>(&b.vm)
        .map_err(bad)?
        .ok_or_else(|| bad(format!("no VM {:?}", b.vm)))?;
    let props = hv.vcpu_props(vm.id, &b.names).await.map_err(bad)?;
    Ok(Json(json!(props)))
}

async fn cpu_catalog() -> Result<Json<Value>, (StatusCode, String)> {
    let (cluster, _) = member()?;
    let missing = || (StatusCode::NOT_FOUND, "no CPU catalog".to_string());
    let me = cluster
        .get::<NodeSpec>(cluster.id().0)
        .map_err(bad)?
        .ok_or_else(missing)?;
    let hash = me.status.cpu.map(|c| c.catalog).ok_or_else(missing)?;
    let catalog = cluster
        .get_by_name::<CpuCatalogSpec>(&hash)
        .map_err(bad)?
        .ok_or_else(missing)?;
    Ok(Json(json!(catalog.spec)))
}
