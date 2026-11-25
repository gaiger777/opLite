//! VM(`Vm`): 디스크는 볼륨 이름, NIC는 네트워크·보안 그룹 이름, `state`는 전원(`power`), 노드는 이름. 바꾸기는
//! `PUT /vms/{key}` 통째(실행 중 하드웨어는 핫플러그 Task 또는 다음 재시작 — 계획에 `restart`). 서버가 채운 MAC·SDN
//! 주소·노드·고정한 CPU 모델은 선언하지 않으면 그대로 둔다. cloud-init 비밀번호는 비밀 칸이다(`secretRef`면 그대로 둠,
//! 평문이면 저장한 해시와 견준다 — 해시를 내보내지 않는다).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::authz::Privilege;
use opl_types::cloud_init::{PASSWORD_MASK, is_crypt_hash};
use opl_types::crypt::sha512_crypt;
use opl_types::folder::FolderKind;
use opl_types::manifest::{ManifestKind, ManifestState, SecretValue};
use opl_types::resource::Kind;
use opl_types::vm::{CpuSpec, PowerState, VmSpec};
use opl_types::vm_check::VmCheckLevel;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Planned, Update, World, bad, lives_of, names_at, problem,
    refs_of, roundtrip, typed, write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, UpdateRequest};
use crate::state::AppState;
use crate::vm_checks::{VmCheckRequest, check_planned};
use crate::vms::{attachment_problems, create_vm, delete_vm, fill_machine, is_stopped, update_vm};

pub(crate) struct VmAdapter;

/// 비밀 칸 경로
const PASSWORD: &str = "/cloud_init/password";
/// 끄기를 기다리는 한도(§8.3 — 게스트 종료)
const STOP_WAIT: Duration = Duration::from_secs(600);

/// 저장 spec의 전원 → 매니페스트 `state`
pub(crate) fn state_of(spec: &Value) -> Option<ManifestState> {
    match spec.get("power").and_then(Value::as_str) {
        Some("running") => Some(ManifestState::Running),
        _ => Some(ManifestState::Stopped),
    }
}

/// 평문이 SHA-512 crypt 해시 `hash`와 맞는지(해시를 남기지 않고 견준다)
fn password_matches(plain: &str, hash: &str) -> bool {
    hash.strip_prefix("$6$")
        .is_some_and(|rest| sha512_crypt(plain.as_bytes(), rest) == hash)
}

/// 실행 중인 VM의 하드웨어를 바꾸는지(핫플러그 Task 또는 다음 재시작 — 계획의 `restart`)
pub(crate) fn needs_restart(cluster: &Cluster, id: Uuid, desired: &Value) -> bool {
    let (Ok(Some(vm)), Ok(spec)) = (cluster.get::<VmSpec>(id), typed::<VmSpec>(desired)) else {
        return false;
    };
    !is_stopped(&vm) && opl_types::hotplug::hardware_changed(&vm.spec, &spec)
}

impl Adapter for VmAdapter {
    fn tree(&self) -> Option<FolderKind> {
        Some(FolderKind::Vm)
    }

    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<VmSpec>(cluster, ManifestKind::Vm)
    }

    /// 폴더·전원(→ `state`)·노드(스케줄러가 고른다)를 빼고 비밀번호는 참조로
    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        let mut out = super::without(spec, &["folder", "power", "node"]);
        if let Some(p) = out.pointer_mut(PASSWORD)
            && !p.is_null()
        {
            *p = serde_json::to_value(SecretValue::Ref {
                secret_ref: String::new(),
            })
            .expect("secret ref");
        }
        out
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        folder: Option<Uuid>,
        state: Option<ManifestState>,
        world: &World,
    ) -> Result<Value, String> {
        let mut out = super::with(spec, "folder", folder.map(|f| Value::String(f.to_string())));
        if let Some(state) = state {
            let power = match state {
                ManifestState::Running => "running",
                ManifestState::Stopped => "stopped",
            };
            out = super::with(&out, "power", Some(json!(power)));
        }
        if let Some(node) = out.get_mut("node")
            && let Some(name) = node.as_str().map(str::to_string)
        {
            *node = Value::String(
                world
                    .node_id(&name)
                    .ok_or_else(|| format!("node {name:?} not found"))?
                    .to_string(),
            );
        }
        // 참조(`secretRef`)는 "저장한 비밀번호 그대로"(`PUT /vms`의 가림 표시)
        if let Some(p) = out.pointer_mut(PASSWORD)
            && p.is_object()
        {
            *p = json!(PASSWORD_MASK);
        }
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<VmSpec>(store)
    }

    fn keep_server_fields(
        &self,
        state: &AppState,
        declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        let Some(obj) = desired.as_object_mut() else {
            return;
        };
        // 전원·노드·템플릿 표시는 선언하지 않으면 지금 값
        for field in ["power", "node", "template"] {
            if !obj.contains_key(field)
                && let Some(v) = live.get(field)
            {
                obj.insert(field.into(), v.clone());
            }
        }
        // NIC의 MAC·SDN 주소: 같은 자리·같은 네트워크면 지금 값
        let declared_nics = declared.get("nics").and_then(Value::as_array);
        if let (Some(nics), Some(old)) = (
            obj.get_mut("nics").and_then(Value::as_array_mut),
            live.get("nics").and_then(Value::as_array),
        ) {
            for (i, nic) in nics.iter_mut().enumerate() {
                let Some(prev) = old.get(i) else { continue };
                if nic.get("network") != prev.get("network") {
                    continue;
                }
                let said = declared_nics.and_then(|d| d.get(i));
                for field in ["mac", "ips"] {
                    let given = said
                        .and_then(|d| d.get(field))
                        .is_some_and(|v| !v.is_null());
                    if !given && let (Some(n), Some(v)) = (nic.as_object_mut(), prev.get(field)) {
                        n.insert(field.into(), v.clone());
                    }
                }
            }
        }
        // 비밀번호: 참조면 저장한 해시, 평문이 저장한 해시와 맞으면 그 해시(바뀜 없음)
        let stored = live
            .pointer(PASSWORD)
            .and_then(Value::as_str)
            .filter(|h| is_crypt_hash(h))
            .map(str::to_string);
        if let Some(p) = desired.pointer_mut(PASSWORD)
            && let (Some(given), Some(stored)) = (p.as_str().map(str::to_string), stored)
            && (given == PASSWORD_MASK || password_matches(&given, &stored))
        {
            *p = Value::String(stored);
        }
        // 별칭 CPU 모델(`x86-64-v3` 등)은 만들 때 버전 모델로 고정된다: 같은 것으로 고정되면 지금 값
        if let (Some(cpu), Some(old)) = (desired.get("cpu"), live.get("cpu"))
            && cpu.get("model") != old.get("model")
            && let (Ok(cpu), Ok(old_cpu), Ok(spec)) = (
                typed::<CpuSpec>(cpu),
                typed::<CpuSpec>(old),
                typed::<VmSpec>(live),
            )
            && let Ok(inv) = crate::scheduler::Inventory::load(&state.cluster)
            && let Ok(pinned) = crate::cpu::check_cpu(&inv, spec.machine.arch(), &cpu, None)
            && pinned.model == old_cpu.model
            && let Some(model) = desired.pointer_mut("/cpu/model")
        {
            *model = serde_json::to_value(&old_cpu.model).expect("cpu model");
        }
    }

    /// HA 넘기기·옮기기·자동 균형이 쓰는 노드, 켜고 끄기가 쓰는 전원
    fn server_fields(&self) -> &'static [&'static str] {
        &["node", "power"]
    }

    fn create_defaults(
        &self,
        state: &AppState,
        _world: &World,
        _planned: &Planned,
        desired: &mut Value,
    ) -> Result<(), ApiError> {
        fill_machine(state, desired)
    }

    fn updatable(&self) -> Update {
        Update::Whole
    }

    fn update_carries_labels(&self) -> bool {
        true
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let mut out = refs_of(ManifestKind::Volume, names_at(spec, "disks", "volume"));
        out.extend(refs_of(
            ManifestKind::Volume,
            names_at(spec, "cdroms", "iso"),
        ));
        out.extend(refs_of(
            ManifestKind::Network,
            names_at(spec, "nics", "network")
                .into_iter()
                .filter(|n| n != opl_types::network::USER_NETWORK),
        ));
        let groups = spec
            .get("nics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|n| n.get("security_groups").and_then(Value::as_array))
            .flatten()
            .filter_map(|g| g.as_str().map(str::to_string));
        out.extend(refs_of(ManifestKind::SecurityGroup, groups));
        // HA 묶음(`ha.group`)은 VM보다 먼저
        if let Some(g) = spec.pointer("/ha/group").and_then(Value::as_str) {
            out.push((ManifestKind::HaGroup, g.to_string()));
        }
        out
    }

    fn secret_paths(&self) -> &'static [&'static str] {
        &["cloud_init.password"]
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        state
            .authz
            .require_new(principal, Privilege::VmCreate, Kind::Vm, folder)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/vms/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/vms/{key}")
    }

    /// 만들기는 계획 71의 미리 확인(이 계획이 먼저 만들 볼륨·배치 규칙을 얹어), 바꾸기는 첨부 권한·할당량
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = Vec::new();
            match check.live {
                None => {
                    if check.desired.pointer(PASSWORD) == Some(&json!(PASSWORD_MASK)) {
                        out.push(Problem::error(
                            "secret_unresolved",
                            "cloud_init.password is a secretRef but the VM does not exist yet; give the password (secretFrom)",
                            Some("cloud_init.password".into()),
                        ));
                        return Ok(out);
                    }
                    // 같은 계획이 먼저 만들 HA 묶음은 아직 없다(그 묶음의 검사는 그 단계가 본다).
                    let mut spec = check.desired.clone();
                    if let Some(g) = spec.pointer("/ha/group").and_then(Value::as_str)
                        && check
                            .planned
                            .docs
                            .contains_key(&(ManifestKind::HaGroup, g.to_string()))
                        && let Some(ha) = spec.get_mut("ha").and_then(Value::as_object_mut)
                    {
                        ha.remove("group");
                    }
                    let req = VmCheckRequest {
                        name: check.name.to_string(),
                        labels: check.labels.clone(),
                        spec,
                        volumes: check
                            .planned
                            .volumes
                            .iter()
                            .map(|v| CreateRequest {
                                name: v.name.clone(),
                                labels: v.labels.clone(),
                                spec: v.spec.clone(),
                            })
                            .collect(),
                        rules: check
                            .planned
                            .rules
                            .iter()
                            .map(|r| CreateRequest {
                                name: r.name.clone(),
                                labels: r.labels.clone(),
                                spec: r.spec.clone(),
                            })
                            .collect(),
                    };
                    match Box::pin(check_planned(check.state, check.principal, req)).await {
                        Ok(checked) => {
                            for p in checked.check.problems {
                                let path = Some(p.at.clone());
                                out.push(match p.level {
                                    VmCheckLevel::Error => Problem::error(p.code, p.message, path),
                                    VmCheckLevel::Warn => Problem::warning(p.code, p.message, path),
                                });
                            }
                        }
                        Err(e) if e.status.is_client_error() => out.push(problem(None, e)),
                        Err(e) => return Err(e),
                    }
                }
                Some(live) => {
                    let (Ok(spec), Ok(current)) =
                        (typed::<VmSpec>(check.desired), typed::<VmSpec>(&live.spec))
                    else {
                        return Ok(out);
                    };
                    for (at, e) in
                        attachment_problems(check.state, check.principal, &spec, Some(&current))?
                    {
                        out.push(problem(Some(&at), e));
                    }
                    if let Err(e) = crate::quota::precheck_vm(
                        &check.state.cluster,
                        Some((live.id, &current)),
                        &spec,
                    ) {
                        out.push(problem(Some("quota"), e));
                    }
                }
            }
            Ok(out)
        })
    }

    fn create<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        name: String,
        labels: BTreeMap<String, String>,
        spec: Value,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let (_, _, Json(vm)) = Box::pin(create_vm(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: vm.id,
                task: None,
            })
        })
    }

    fn update<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
        labels: BTreeMap<String, String>,
        desired: Value,
        _fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let spec: VmSpec = typed(&desired).map_err(bad)?;
            let (headers, _) = Box::pin(update_vm(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                Json(UpdateRequest { labels, spec }),
            ))
            .await?;
            let task = headers
                .get(crate::revision::TASK)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            Ok(Done { id: live.id, task })
        })
    }

    /// 켜진 VM은 먼저 끈다(게스트 종료 — `PUT`의 `power: stopped`, 600초까지 기다림). 그 뒤 `DELETE /vms/{key}`.
    fn delete<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let mut revision = live.revision;
            let current = crate::resources::lookup::<VmSpec>(&state.cluster, &live.id.to_string())?;
            if !is_stopped(&current) {
                let mut spec = current.spec.clone();
                spec.power = PowerState::Stopped;
                Box::pin(update_vm(
                    State(state.clone()),
                    principal.clone(),
                    AuditNote::detached(),
                    Path(live.id.to_string()),
                    super::if_match(revision),
                    Json(UpdateRequest {
                        labels: current.labels.clone(),
                        spec,
                    }),
                ))
                .await?;
                let start = Instant::now();
                loop {
                    let vm =
                        crate::resources::lookup::<VmSpec>(&state.cluster, &live.id.to_string())?;
                    if is_stopped(&vm) {
                        revision = vm.revision;
                        break;
                    }
                    if start.elapsed() > STOP_WAIT {
                        return Err(ApiError::conflict(
                            "vm_running",
                            format!("VM {:?} did not stop within {STOP_WAIT:?}", live.name),
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            Box::pin(delete_vm(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(revision),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: None,
            })
        })
    }

    /// 만들기 처리기가 `Cluster::create` 뒤에 id를 다시 쓰지 않는다(지운 것을 같은 id로 살린다).
    fn keeps_id_on_restore(&self) -> bool {
        true
    }

    fn write_labels<'a>(
        &'a self,
        cluster: &'a Cluster,
        id: Uuid,
        revision: u64,
        labels: BTreeMap<String, String>,
    ) -> Fut<'a, ()> {
        write_labels_as::<VmSpec>(cluster, id, revision, labels)
    }
}
