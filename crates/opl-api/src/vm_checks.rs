//! VM 만들기 미리 확인 `POST /vm-checks`(계획 71 §9): 만들기(`POST /vms`)와 같은 검사를 만들 볼륨·배치 규칙을 얹어
//! 돌리고, 아무것도 쓰지 않고 문제를 자리(빌더의 칸 — 웹 폼의 오류 열쇠와 같은 꼴)마다 돌려준다. 잠그지 않고 감사
//! 로그에 적지 않는다(`audit::layer`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use opl_types::authz::Privilege;
use opl_types::node::{FOLDERS, NodeSpec, VM_CHECK};
use opl_types::placement::{PlacementRule, PlacementRuleSpec};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{PowerState, Vm, VmSpec};
use opl_types::vm_check::{VmCheck, VmCheckLevel, VmCheckProblem};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, check_name};
use crate::score;
use crate::state::AppState;
use crate::vms::{
    Planned, attachment_problems, fill_machine, hide_node_reasons, parse_spec, prepare_with,
};
use crate::volumes::{check_new_volume, find_pool, pool_node};

/// 미리 확인할 VM 만들기와 그 앞에 만들 것
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VmCheckRequest {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// `POST /vms`의 spec과 같다(`machine`을 빼면 채운다).
    #[schema(value_type = VmSpec)]
    pub spec: serde_json::Value,
    /// VM보다 먼저 만들 볼륨(`POST /volumes`의 본문). 디스크가 이름으로 가리키면 준비된 볼륨으로 본다.
    #[serde(default)]
    pub volumes: Vec<CreateRequest<VolumeSpec>>,
    /// VM보다 먼저 만들 배치 규칙(`POST /placement-rules`의 본문 — 예: 장애 영역 규칙)
    #[serde(default)]
    pub rules: Vec<CreateRequest<PlacementRuleSpec>>,
}

/// 오류 `code`의 자리. `prepare`는 첫 오류에서 멈추므로 그 오류의 자리를 이 표로 정한다(코드마다 시험).
pub(crate) fn area_of(code: &str) -> &'static str {
    match code {
        "name_taken" => "name",
        "mac_in_use"
        | "ip_in_use"
        | "ip_exhausted"
        | "ips_without_sdn"
        | "invalid_qos"
        | "qos_dscp_needs_sdn"
        | "qos_unsupported"
        | "firewall_unsupported" => "nics",
        "volume_not_ready" | "volume_in_use" | "pool_unavailable" | "pool_space" => "disks",
        "cdrom_local" => "cdroms",
        "cloud_init_sdn_nic" => "cloudInit",
        "placement_rule"
        | "rule_conflict"
        | "no_node"
        | "node_unavailable"
        | "no_capacity"
        | "machine_arch_mismatch" => "placement",
        "invalid_numa"
        | "invalid_host_resources"
        | "host_cpus_in_use"
        | "hugepages_in_use"
        | "unknown_cpu_model"
        | "unknown_cpu_flag"
        | "invalid_cpu_flags" => "compute",
        "replication_unsupported" => "replication",
        "quota_exceeded" => "quota",
        c if c.starts_with("network_") => "nics",
        c if c.starts_with("volume_") => "disks",
        c if c.starts_with("cpu_") => "compute",
        c if c.starts_with("ha_") => "protection",
        _ => "spec",
    }
}

/// `prepare` 오류 `e`의 자리: [`area_of`]에 더해, 문구가 따옴표 이름(`{:?}`)으로 가리키는 NIC 네트워크·디스크 볼륨·CD ISO가
/// 있으면 그 칸(`nics.N`·`disks.N`·`cdroms.N`)이다 — 예: 400 `volume "x.iso" not found`는 `cdroms.0`.
fn slot_of(e: &ApiError, spec: &VmSpec) -> String {
    let named = |name: &str| e.message.contains(&format!("{name:?}"));
    let find = |kind: &str| -> Option<String> {
        let i = match kind {
            "nics" => spec.nics.iter().position(|n| named(&n.network)),
            "disks" => spec.disks.iter().position(|d| named(&d.volume)),
            _ => spec
                .cdroms
                .iter()
                .position(|c| c.iso.as_deref().is_some_and(named)),
        }?;
        Some(format!("{kind}.{i}"))
    };
    match area_of(e.code) {
        area @ ("nics" | "disks" | "cdroms") => find(area).unwrap_or_else(|| area.into()),
        "spec" => ["disks", "cdroms", "nics"]
            .into_iter()
            .find_map(find)
            .unwrap_or_else(|| "spec".into()),
        area => area.into(),
    }
}

/// 모은 문제(같은 자리·코드는 하나로)
#[derive(Default)]
struct Problems(Vec<VmCheckProblem>);

impl Problems {
    fn push(&mut self, at: &str, level: VmCheckLevel, code: &str, message: String) {
        if self.0.iter().any(|p| p.at == at && p.code == code) {
            return;
        }
        self.0.push(VmCheckProblem {
            at: at.into(),
            code: code.into(),
            message,
            level,
        });
    }

    /// 거절 `e`를 자리 `at`의 오류로. 클라이언트 오류가 아니면(저장소 읽기 실패 등) 확인할 수 없어 `Err`다.
    fn error(&mut self, at: &str, e: ApiError) -> Result<(), ApiError> {
        if !e.status.is_client_error() {
            return Err(e);
        }
        self.push(at, VmCheckLevel::Error, e.code, e.message);
        Ok(())
    }

    fn check(&mut self, at: &str, r: Result<(), ApiError>) -> Result<(), ApiError> {
        match r {
            Ok(()) => Ok(()),
            Err(e) => self.error(at, e),
        }
    }

    fn answer(self) -> VmCheck {
        VmCheck {
            ok: !self.0.iter().any(|p| p.level == VmCheckLevel::Error),
            problems: self.0,
            node: None,
            machine: None,
            cpu_model: None,
            ips: Vec::new(),
        }
    }
}

/// 머신을 채우지 못한 spec에서 폴더만 읽는다(권한 확인용).
fn folder_of(spec: &serde_json::Value) -> Result<Option<Uuid>, ApiError> {
    match spec.get("folder") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| ApiError::bad_request(format!("spec.folder: {e}"))),
    }
}

/// 만들기를 미리 확인한다: 만들기와 같은 검사(이름, 첨부 권한, CPU 플래그·NUMA 권한, 만들 볼륨(`POST /volumes`의 검사와 풀
/// 남은 용량 경고), 만들 배치 규칙(`POST /placement-rules`의 검사), 폴더 할당량, 그리고 만들 것을 얹은 `prepare`)를 쓰지
/// 않고 돌려 문제를 자리마다 모은다. 문제가 있어도 200이다. 대상 폴더에 `vm.create`가 없으면 403(그 밖을 알려 주지
/// 않음), 기능 `vm-check`를 모르는 노드가 있으면 409 `cluster_upgrading`, 본문을 풀 수 없으면 400.
#[utoipa::path(post, path = "/vm-checks", tag = "vms",
    request_body = VmCheckRequest,
    responses((status = 200, body = VmCheck), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<VmCheckRequest>,
) -> Result<Json<VmCheck>, ApiError> {
    require_feature(&state.cluster, VM_CHECK)?;
    let mut found = Problems::default();
    match check_name(&req.name) {
        Err(e) => found.error("name", e)?,
        Ok(()) if state.cluster.get_by_name::<VmSpec>(&req.name)?.is_some() => found.push(
            "name",
            VmCheckLevel::Error,
            "name_taken",
            format!("a VM named {:?} already exists", req.name),
        ),
        Ok(()) => {}
    }
    let mut spec = req.spec;
    let machine = match fill_machine(&state, &mut spec) {
        Ok(()) => None,
        Err(e) if e.status.is_client_error() => Some(e),
        Err(e) => return Err(e),
    };
    let (folder, parsed) = match machine {
        Some(_) => (folder_of(&spec)?, None),
        None => {
            let spec = parse_spec(spec)?;
            (spec.folder, Some(spec))
        }
    };
    if folder.is_some() {
        require_feature(&state.cluster, FOLDERS)?;
    }
    state
        .authz
        .require_new(&principal, Privilege::VmCreate, Kind::Vm, folder)?;
    let (Some(spec), None) = (parsed, machine.as_ref()) else {
        found.error("compute", machine.expect("no spec without a machine error"))?;
        return Ok(Json(found.answer()));
    };

    for (at, e) in attachment_problems(&state, &principal, &spec, None)? {
        found.error(&at, e)?;
    }
    found.check(
        "compute",
        crate::cpu::check_flag_privilege(&state, &principal, &spec.cpu.flags, &[]),
    )?;
    found.check(
        "compute",
        crate::numa::check_privilege(&state, &principal, &spec, None),
    )?;

    let mut planned = Planned::default();
    let mut sizes = BTreeMap::new();
    for v in &req.volumes {
        let at = spec
            .disks
            .iter()
            .position(|d| d.volume == v.name)
            .map_or_else(|| "disks".to_string(), |i| format!("disks.{i}"));
        let mut volume: Volume = Resource::new(v.name.clone(), v.spec.clone());
        volume.labels = v.labels.clone();
        volume.status.phase = VolumePhase::Ready;
        if sizes.insert(v.name.clone(), v.spec.size_bytes).is_some()
            || state.cluster.get_by_name::<VolumeSpec>(&v.name)?.is_some()
        {
            found.push(
                &at,
                VmCheckLevel::Error,
                "name_taken",
                format!("a volume named {:?} already exists", v.name),
            );
        }
        match check_new_volume(&state, &principal, v) {
            Ok((pool, node)) => {
                volume.status.node = Some(node);
                let free = state
                    .cluster
                    .get::<NodeSpec>(node)?
                    .and_then(|n| n.status.pools.get(&pool.name).cloned())
                    .filter(|p| p.pool_id == pool.id && p.total_bytes > 0)
                    .map(|p| p.available_bytes);
                if let Some(free) = free
                    && free < v.spec.size_bytes
                {
                    found.push(
                        &at,
                        VmCheckLevel::Warn,
                        "pool_space",
                        format!(
                            "storage pool {:?} has {free} bytes free, less than the {} bytes of volume {:?}",
                            pool.name, v.spec.size_bytes, v.name
                        ),
                    );
                }
            }
            Err(e) => {
                found.error(&at, e)?;
                // 검사를 지나지 못한 볼륨도 풀이 있으면 그 노드에 둔다(뒤의 만들기 검사가 "풀에 닿는 노드 없음"을 덧붙이지 않게).
                volume.status.node = find_pool(&state, &v.spec.pool)
                    .and_then(|pool| pool_node(&state, &pool))
                    .ok();
            }
        }
        planned.volumes.push(volume);
    }
    for r in &req.rules {
        let mut rule: PlacementRule = Resource::new(r.name.clone(), r.spec.clone());
        rule.labels = r.labels.clone();
        let checked = state
            .authorize(&principal, Action::Write, PlacementRuleSpec::KIND)
            .and_then(|()| check_name(&r.name))
            .and_then(
                |()| match state.cluster.get_by_name::<PlacementRuleSpec>(&r.name)? {
                    Some(_) => Err(ApiError::conflict(
                        "name_taken",
                        format!("a placement rule named {:?} already exists", r.name),
                    )),
                    None => crate::placement_rules::check(&state, &rule),
                },
            );
        found.check("placement", checked)?;
        planned.rules.push(rule);
    }
    found.check(
        "quota",
        crate::quota::precheck_new_vm(&state.cluster, &spec, &sizes),
    )?;

    let mut vm: Vm = Resource::new(req.name, spec);
    vm.labels = req.labels;
    let picks = vm.spec.power == PowerState::Running && vm.spec.node.is_none();
    let scoring = score::scorer(&state, picks).await;
    match prepare_with(&state, &mut vm, &scoring.scorer(), &planned) {
        Err(e) => {
            let e = hide_node_reasons(&state, &principal, e);
            let at = slot_of(&e, &vm.spec);
            found.error(&at, e)?;
            Ok(Json(found.answer()))
        }
        Ok(()) => {
            let node = match vm.spec.node {
                Some(id) if crate::cpu::sees_node_names(&state, &principal) => {
                    state.cluster.get::<NodeSpec>(id)?.map(|n| n.name)
                }
                _ => None,
            };
            Ok(Json(VmCheck {
                node,
                machine: Some(vm.spec.machine),
                cpu_model: Some(vm.spec.cpu.model.clone()),
                ips: vm.spec.nics.iter().map(|n| n.ips.clone()).collect(),
                ..found.answer()
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `prepare`가 낼 수 있는 코드마다 자리(설계 §9.2 표)
    #[test]
    fn each_code_has_its_slot() {
        for (code, at) in [
            ("name_taken", "name"),
            ("mac_in_use", "nics"),
            ("ip_in_use", "nics"),
            ("ip_exhausted", "nics"),
            ("ips_without_sdn", "nics"),
            ("invalid_qos", "nics"),
            ("qos_dscp_needs_sdn", "nics"),
            ("qos_unsupported", "nics"),
            ("firewall_unsupported", "nics"),
            ("network_whatever", "nics"),
            ("volume_not_ready", "disks"),
            ("volume_in_use", "disks"),
            ("pool_unavailable", "disks"),
            ("pool_space", "disks"),
            ("cdrom_local", "cdroms"),
            ("cloud_init_sdn_nic", "cloudInit"),
            ("placement_rule", "placement"),
            ("rule_conflict", "placement"),
            ("no_node", "placement"),
            ("node_unavailable", "placement"),
            ("no_capacity", "placement"),
            ("machine_arch_mismatch", "placement"),
            ("cpu_model_arch", "compute"),
            ("cpu_flag_masked", "compute"),
            ("cpu_flag_restricted", "compute"),
            ("cpu_model_ambiguous", "compute"),
            ("cpu_baseline_unsupported", "compute"),
            ("unknown_cpu_model", "compute"),
            ("unknown_cpu_flag", "compute"),
            ("invalid_cpu_flags", "compute"),
            ("invalid_numa", "compute"),
            ("invalid_host_resources", "compute"),
            ("host_cpus_in_use", "compute"),
            ("hugepages_in_use", "compute"),
            ("ha_unsupported", "protection"),
            ("ha_capacity", "protection"),
            ("replication_unsupported", "replication"),
            ("quota_exceeded", "quota"),
            ("cluster_upgrading", "spec"),
            ("invalid_request", "spec"),
        ] {
            assert_eq!(area_of(code), at, "{code}");
        }
    }

    #[test]
    fn named_items_narrow_the_slot() {
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt",
            "disks": [{ "volume": "a", "bus": "virtio-blk" }, { "volume": "b", "bus": "virtio-blk" }],
            "cdroms": [{ "iso": "x.iso" }],
            "nics": [{ "network": "user", "model": "virtio" }, { "network": "lan", "model": "virtio" }],
        }))
        .unwrap();
        let at = |code: &'static str, message: &str| {
            slot_of(&ApiError::bad_request(message).with_code(code), &spec)
        };
        assert_eq!(
            at("invalid_request", "volume \"x.iso\" not found"),
            "cdroms.0"
        );
        assert_eq!(at("invalid_request", "volume \"b\" not found"), "disks.1");
        assert_eq!(at("invalid_request", "network \"lan\" not found"), "nics.1");
        assert_eq!(at("invalid_request", "HA group \"g\" not found"), "spec");
        assert_eq!(
            at("volume_in_use", "volume \"b\" is already attached"),
            "disks.1"
        );
        assert_eq!(at("volume_in_use", "something else"), "disks");
        assert_eq!(at("ip_in_use", "address taken"), "nics");
        // 자리가 정해진 코드는 이름이 있어도 그대로다.
        assert_eq!(at("no_node", "volume \"a\""), "placement");
    }

    #[test]
    fn the_same_slot_and_code_is_kept_once() {
        let mut p = Problems::default();
        p.push("disks.0", VmCheckLevel::Error, "volume_in_use", "a".into());
        p.push("disks.0", VmCheckLevel::Error, "volume_in_use", "b".into());
        p.push("disks", VmCheckLevel::Error, "volume_in_use", "c".into());
        assert_eq!(p.0.len(), 2);
        p.error("x", ApiError::internal("boom")).unwrap_err();
        let answer = p.answer();
        assert!(!answer.ok);
        let mut warn = Problems::default();
        warn.push("disks.0", VmCheckLevel::Warn, "pool_space", "w".into());
        assert!(warn.answer().ok);
    }
}
