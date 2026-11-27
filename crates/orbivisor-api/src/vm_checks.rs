//! VM 만들기 미리 확인 `POST /vm-checks`(계획 71 §9): 만들기(`POST /vms`)와 같은 검사를 만들 볼륨·배치 규칙을 얹어
//! 돌리고, 아무것도 쓰지 않고 문제를 자리(빌더의 칸 — 웹 폼의 오류 열쇠와 같은 꼴)마다 돌려준다. 잠그지 않고 감사
//! 로그에 적지 않는다(`audit::layer`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use orbivisor_types::authz::Privilege;
use orbivisor_types::node::{FOLDERS, NodeSpec, VM_CHECK};
use orbivisor_types::placement::{PlacementRule, PlacementRuleSpec};
use orbivisor_types::resource::{Kind, Resource, ResourceSpec};
use orbivisor_types::storage::{Volume, VolumePhase, VolumeSpec};
use orbivisor_types::vm::{PowerState, Vm, VmSpec};
use orbivisor_types::vm_check::{VmCheck, VmCheckLevel, VmCheckProblem};
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
    /// VM보다 먼저 만들 볼륨(`POST /volumes`의 본문, 16개까지). 디스크가 이름으로 가리키면 준비된 볼륨으로 본다.
    #[serde(default)]
    pub volumes: Vec<CreateRequest<VolumeSpec>>,
    /// VM보다 먼저 만들 배치 규칙(`POST /placement-rules`의 본문 — 예: 장애 영역 규칙, 16개까지)
    #[serde(default)]
    pub rules: Vec<CreateRequest<PlacementRuleSpec>>,
}

/// 한 확인에 얹을 수 있는 만들 볼륨 수(넘으면 400 — 리뷰 1 4). VM 하나의 NIC 상한과 같다.
pub const MAX_PLANNED_VOLUMES: usize = 16;
/// 한 확인에 얹을 수 있는 만들 배치 규칙 수(넘으면 400)
pub const MAX_PLANNED_RULES: usize = 16;

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

/// `prepare` 오류 `e`의 자리: [`area_of`]에 더해, 문구가 주어 자리(`network "n"`·`volume "v"`·`ISO "i"`)로 가리키는 NIC
/// 네트워크·디스크 볼륨·CD ISO가 있으면 그 칸(`nics.N`·`disks.N`·`cdroms.N`)이다 — 예: 400 `volume "x.iso" not found`는
/// `cdroms.0`. 문구에 함께 든 다른 따옴표 이름(`… attached to VM "a"`)은 보지 않는다. 디스크 코드(`volume_in_use` 등)는
/// 맞는 디스크가 없으면 CD를 본다(`ISO "x" is being deleted`).
fn slot_of(e: &ApiError, spec: &VmSpec) -> String {
    let named = |kinds: &[&str], name: &str| {
        kinds
            .iter()
            .any(|kind| e.message.contains(&format!("{kind} {name:?}")))
    };
    let find = |kind: &str| -> Option<String> {
        let i = match kind {
            "nics" => spec
                .nics
                .iter()
                .position(|n| named(&["network"], &n.network)),
            "disks" => spec
                .disks
                .iter()
                .position(|d| named(&["volume"], &d.volume)),
            _ => spec.cdroms.iter().position(|c| {
                c.iso
                    .as_deref()
                    .is_some_and(|iso| named(&["volume", "ISO"], iso))
            }),
        }?;
        Some(format!("{kind}.{i}"))
    };
    let kinds: &[&str] = match area_of(e.code) {
        "nics" => &["nics"],
        "disks" => &["disks", "cdroms"],
        "cdroms" => &["cdroms"],
        "spec" => &["disks", "cdroms", "nics"],
        area => return area.into(),
    };
    kinds
        .iter()
        .find_map(|k| find(k))
        .unwrap_or_else(|| area_of(e.code).into())
}

/// 모은 문제(같은 자리·코드는 하나로)
#[derive(Default)]
struct Problems {
    found: Vec<VmCheckProblem>,
    /// 권한 거절(403·404)이 있었다: 만들기처럼 거기서 멈추고 더 깊은 검사(할당량·`prepare` — 숨은 VM·주소·볼륨 상태가
    /// 드러난다)를 돌리지 않는다(리뷰 1 1).
    refused: bool,
}

impl Problems {
    fn push(&mut self, at: &str, level: VmCheckLevel, code: &str, message: String) {
        if self.found.iter().any(|p| p.at == at && p.code == code) {
            return;
        }
        self.found.push(VmCheckProblem {
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
        self.refused |= is_refusal(&e);
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
            ok: !self.found.iter().any(|p| p.level == VmCheckLevel::Error),
            problems: self.found,
            node: None,
            start_node: None,
            machine: None,
            cpu_model: None,
            ips: Vec::new(),
        }
    }
}

/// 권한 거절(403, 또는 읽을 수 없어 404)인지
fn is_refusal(e: &ApiError) -> bool {
    matches!(
        e.status,
        axum::http::StatusCode::FORBIDDEN | axum::http::StatusCode::NOT_FOUND
    )
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
/// 않음), 기능 `vm-check`를 모르는 노드가 있으면 409 `cluster_upgrading`, 본문을 풀 수 없거나 만들 볼륨·규칙이 16개를
/// 넘으면 400. 첨부·만들 것의 권한 거절(403·404)이 있으면 그 거절만 돌려준다(할당량·배치·주소 검사 없이, `node`·`ips` 없이).
#[utoipa::path(post, path = "/vm-checks", tag = "vms",
    request_body = VmCheckRequest,
    responses((status = 200, body = VmCheck), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<VmCheckRequest>,
) -> Result<Json<VmCheck>, ApiError> {
    require_feature(&state.cluster, VM_CHECK)?;
    if req.volumes.len() > MAX_PLANNED_VOLUMES || req.rules.len() > MAX_PLANNED_RULES {
        return Err(ApiError::bad_request(format!(
            "a check can plan at most {MAX_PLANNED_VOLUMES} volumes and {MAX_PLANNED_RULES} placement rules"
        )));
    }
    Ok(Json(
        Box::pin(check_planned(&state, &principal, req))
            .await?
            .check,
    ))
}

/// 미리 확인 하나의 결과: 답과, 통과했을 때 스케줄러가 고른 노드 id(이름을 볼 수 없어도 — 안쪽에서만 쓴다)
pub(crate) struct Checked {
    pub check: VmCheck,
    pub node: Option<Uuid>,
}

impl Checked {
    fn answer(found: Problems) -> Self {
        Self {
            check: found.answer(),
            node: None,
        }
    }
}

/// [`check_vm`]의 검사(기능·개수 한도 없이 — VMware 이전의 VM마다 미리 확인도 부른다, 계획 72 §8.5). 대상 폴더에 `vm.create`가
/// 없으면 403(404)이고, 그 밖의 문제는 답에 모은다.
pub(crate) async fn check_planned(
    state: &AppState,
    principal: &Principal,
    req: VmCheckRequest,
) -> Result<Checked, ApiError> {
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
    let machine = match fill_machine(state, &mut spec) {
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
        .require_new(principal, Privilege::VmCreate, Kind::Vm, folder)?;
    let (Some(spec), None) = (parsed, machine.as_ref()) else {
        found.error("compute", machine.expect("no spec without a machine error"))?;
        return Ok(Checked::answer(found));
    };

    for (at, e) in attachment_problems(state, principal, &spec, None)? {
        found.error(&at, e)?;
    }
    found.check(
        "compute",
        crate::cpu::check_flag_privilege(state, principal, &spec.cpu.flags, &[]),
    )?;
    found.check(
        "compute",
        crate::numa::check_privilege(state, principal, &spec, None),
    )?;

    let mut planned = Planned {
        preview: true,
        ..Planned::default()
    };
    let mut sizes = BTreeMap::new();
    for v in &req.volumes {
        let at = spec
            .disks
            .iter()
            .position(|d| d.volume == v.name)
            .map_or_else(|| "disks".to_string(), |i| format!("disks.{i}"));
        // 풀 권한을 다른 볼륨 검사보다 먼저 본다(리뷰 2 — `check_new_volume`은 400 검사(이름·내용·크기·`base`)가
        // 앞서므로, 400인 볼륨을 얹으면 할당할 수 없는(볼 수도 없는) 풀의 성질이 `prepare`로 드러났다). 없는 풀도 거절처럼
        // 멈춘다. `POST /volumes`의 차례는 그대로다.
        let pool = match find_pool(state, &v.spec.pool).and_then(|pool| {
            state
                .authz
                .require(principal, Privilege::StorageAllocate, &pool)
                .map(|()| pool)
        }) {
            Ok(pool) => pool,
            Err(e) => {
                found.error(&at, e)?;
                found.refused = true;
                continue;
            }
        };
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
        match check_new_volume(state, principal, v) {
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
                // 검사를 지나지 못한 볼륨도(풀 권한은 위에서 지났다) 그 풀의 노드에 둔다(뒤의 만들기 검사가 "풀에 닿는 노드
                // 없음"을 덧붙이지 않게).
                volume.status.node = pool_node(state, &pool).ok();
            }
        }
        planned.volumes.push(volume);
    }
    for r in &req.rules {
        // 만들 권한이 없는 규칙은 보지도 얹지도 않는다(리뷰 1 2 — 얹으면 숨은 VM이 도는 노드가 드러난다).
        if let Err(e) = state.authorize(principal, Action::Write, PlacementRuleSpec::KIND) {
            found.error("placement", e)?;
            continue;
        }
        let mut rule: PlacementRule = Resource::new(r.name.clone(), r.spec.clone());
        rule.labels = r.labels.clone();
        let checked = check_name(&r.name).and_then(|()| {
            match state.cluster.get_by_name::<PlacementRuleSpec>(&r.name)? {
                Some(_) => Err(ApiError::conflict(
                    "name_taken",
                    format!("a placement rule named {:?} already exists", r.name),
                )),
                None => crate::placement_rules::check(state, &rule),
            }
        });
        found.check("placement", checked)?;
        planned.rules.push(rule);
    }
    // 권한 거절이 있으면 만들기도 거기서 멈춘다: 거절만 돌려준다(노드·주소 없이).
    if found.refused {
        return Ok(Checked::answer(found));
    }
    found.check(
        "quota",
        crate::quota::precheck_new_vm(&state.cluster, &spec, &sizes),
    )?;

    let mut vm: Vm = Resource::new(req.name, spec);
    vm.labels = req.labels;
    // 꺼진 채 노드 없이 만들어도 "켜면 놓일 노드"를 미리 보인다(계획 77-5) — 노드 이름을 볼 수 있는 사용자에게만(못 보면
    // 어차피 버리므로 스케줄도 부하 읽기도 하지 않는다). 부하 요약은 스케줄할 때만 읽는다.
    let sees = crate::cpu::sees_node_names(state, principal);
    let off = vm.spec.power != PowerState::Running && vm.spec.node.is_none();
    let picks = vm.spec.node.is_none() && (!off || sees);
    let scoring = score::scorer(state, picks).await;
    match prepare_with(state, &mut vm, &scoring.scorer(), &planned) {
        Err(e) => {
            let e = hide_node_reasons(state, principal, e);
            let at = slot_of(&e, &vm.spec);
            found.error(&at, e)?;
            Ok(Checked::answer(found))
        }
        Ok(()) => {
            let name = |id: Option<Uuid>| -> Result<Option<String>, ApiError> {
                match id {
                    Some(id) if sees => Ok(state.cluster.get::<NodeSpec>(id)?.map(|n| n.name)),
                    _ => Ok(None),
                }
            };
            let node = name(vm.spec.node)?;
            let start_node = if off && sees {
                name(start_node(state, &vm, &scoring.scorer(), &planned)?)?
            } else {
                None
            };
            Ok(Checked {
                check: VmCheck {
                    node,
                    start_node,
                    machine: Some(vm.spec.machine),
                    cpu_model: Some(vm.spec.cpu.model.clone()),
                    ips: vm.spec.nics.iter().map(|n| n.ips.clone()).collect(),
                    ..found.answer()
                },
                node: vm.spec.node,
            })
        }
    }
}

/// 꺼진 채 노드 없이 만들 VM을 지금 켜면 스케줄러가 고를 노드(예상). 놓을 노드가 없으면 없다 — 만들기는 받으므로 문제로
/// 적지 않는다(켤 때 그 까닭이 나온다).
fn start_node(
    state: &AppState,
    vm: &Vm,
    scorer: &score::Scorer,
    planned: &Planned,
) -> Result<Option<Uuid>, ApiError> {
    let mut inv = crate::scheduler::Inventory::load(&state.cluster)?;
    inv.volumes.extend(planned.volumes.iter().cloned());
    inv.rules.extend(planned.rules.iter().cloned());
    let mut on = vm.clone();
    on.spec.power = PowerState::Running;
    Ok(crate::scheduler::schedule_with(&on, &inv, scorer).ok())
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
        // 주어 자리의 이름만 본다(뒤의 VM 이름 "a"는 디스크 a가 아니다 — 리뷰 1 7).
        assert_eq!(
            at(
                "volume_in_use",
                "volume \"b\" is already attached to VM \"a\""
            ),
            "disks.1"
        );
        // 지우는 중인 ISO는 디스크 코드지만 CD 칸이다.
        assert_eq!(
            at("volume_in_use", "ISO \"x.iso\" is being deleted (task t)"),
            "cdroms.0"
        );
        assert_eq!(
            at(
                "invalid_request",
                "HA and replicated VMs can use ISOs only from shared pools; ISO \"x.iso\" is in node-local pool \"a\""
            ),
            "cdroms.0"
        );
        // 다른 종류의 같은 이름(네트워크 문구의 "a")은 디스크가 아니다.
        assert_eq!(at("invalid_request", "network \"a\" not found"), "spec");
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
        assert_eq!(p.found.len(), 2);
        p.error("x", ApiError::internal("boom")).unwrap_err();
        let answer = p.answer();
        assert!(!answer.ok);
        let mut warn = Problems::default();
        warn.push("disks.0", VmCheckLevel::Warn, "pool_space", "w".into());
        assert!(warn.answer().ok);
    }
}
