//! CPU 모델 E2E의 고리(계획 63). 시험 빌드(`cpu-it`)에만 있고 제어 소켓에 경로 셋을 더한다. API는 스테이지 5 전까지
//! `host`·`max` 밖의 모델·플래그와 설정 `cpu`를 받지 않으므로 시험이 클러스터에 바로 쓴다.
//!
//! - `POST /v1/test/vm-cpu` `{vm, model, flags}`: VM 스펙의 `cpu.model`·`cpu.flags`를 바꾼다(검증 없음).
//! - `POST /v1/test/cpu-mask` `{mask}`: 설정 `cpu.mask`를 바꾼다(설정이 없으면 만든다).
//! - `POST /v1/test/vcpu-props` `{vm, names}`: 이 노드에서 도는 VM의 vCPU마다 CPU 속성(QMP `qom-get`)
//! - `POST /v1/test/cpu-catalog`: 이 노드가 알린 CPU 카탈로그(없으면 404)

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::Cluster;
use opl_driver::Hypervisor;
use opl_types::cpu::{CpuCatalogSpec, CpuFlag, CpuModel, CpuSettings};
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::settings::{SETTINGS_NAME, Settings, SettingsSpec};
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
        .route("/v1/test/vm-cpu", post(vm_cpu))
        .route("/v1/test/cpu-mask", post(cpu_mask))
        .route("/v1/test/vcpu-props", post(vcpu_props))
        .route("/v1/test/cpu-catalog", post(cpu_catalog))
}

#[derive(Deserialize)]
struct VmCpu {
    vm: String,
    model: String,
    #[serde(default)]
    flags: Vec<String>,
}

async fn vm_cpu(Json(b): Json<VmCpu>) -> Result<StatusCode, (StatusCode, String)> {
    let (cluster, _) = member()?;
    let model: CpuModel = b.model.parse().map_err(bad)?;
    let flags = b
        .flags
        .iter()
        .map(|f| f.parse::<CpuFlag>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(bad)?;
    let vm = cluster
        .get_by_name::<VmSpec>(&b.vm)
        .map_err(bad)?
        .ok_or_else(|| bad(format!("no VM {:?}", b.vm)))?;
    let mut spec = vm.spec.clone();
    spec.cpu.model = model;
    spec.cpu.flags = flags;
    cluster
        .update_spec(vm.id, Some(vm.revision), vm.labels, &spec)
        .await
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Mask {
    mask: Vec<String>,
}

async fn cpu_mask(Json(b): Json<Mask>) -> Result<StatusCode, (StatusCode, String)> {
    let (cluster, _) = member()?;
    let cpu = CpuSettings {
        mask: b.mask,
        ..CpuSettings::default()
    };
    match cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .map_err(bad)?
    {
        Some(s) => {
            let mut spec = s.spec.clone();
            spec.cpu = Some(cpu);
            cluster
                .update_spec(s.id, Some(s.revision), s.labels, &spec)
                .await
                .map_err(bad)?;
        }
        None => {
            let s: Settings = Resource::new(
                SETTINGS_NAME,
                SettingsSpec {
                    cpu: Some(cpu),
                    ..SettingsSpec::default()
                },
            );
            cluster.create(&s).await.map_err(bad)?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
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
