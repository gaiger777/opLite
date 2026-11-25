//! 부하 분산기(`LoadBalancer` — 네트워크 폴더): NIC 멤버의 VM은 이름(저장은 id), 네트워크는 이름 그대로. 서버가 고르는
//! VIP·바깥 VIP 주소·건강 검사 출발 주소(`monitors` — 매니페스트에 없다, 처리기가 주면 거절)는 선언하지 않으면 지금 값을
//! 둔다. 설명·바깥 VIP·리스너·출발지 고정을 바꾼다(`PATCH`), VIP 네트워크·VIP는 바꿀 수 없다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use orbivisor_cluster::Cluster;
use orbivisor_types::authz::Privilege;
use orbivisor_types::folder::FolderKind;
use orbivisor_types::load_balancer::LoadBalancerSpec;
use orbivisor_types::manifest::{ManifestKind, ManifestState};
use orbivisor_types::resource::ResourceSpec;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    Adapter, Done, Fut, Live, Update, World, bad, lives_of, refs_of, roundtrip, typed,
    write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::error::{ApiError, Json};
use crate::load_balancers::{
    CreateLoadBalancer, LoadBalancerPatch, create_load_balancer, delete_load_balancer,
    patch_load_balancer,
};
use crate::state::AppState;

pub(crate) struct LbAdapter;

/// 리스너 멤버의 `vm` 값마다 `f`
fn map_member_vms(spec: &mut Value, f: impl Fn(&str) -> String) {
    let Some(listeners) = spec.get_mut("listeners").and_then(Value::as_array_mut) else {
        return;
    };
    for l in listeners {
        let Some(members) = l.get_mut("members").and_then(Value::as_array_mut) else {
            continue;
        };
        for m in members {
            if let Some(vm) = m.get_mut("vm")
                && let Some(s) = vm.as_str().map(str::to_string)
            {
                *vm = Value::String(f(&s));
            }
        }
    }
}

fn member_vms(spec: &Value) -> Vec<String> {
    spec.get("listeners")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| l.get("members").and_then(Value::as_array))
        .flatten()
        .filter_map(|m| m.get("vm").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// `PATCH` 본문: 바뀐 칸만, 없어진 바깥 VIP·출발지 고정은 `null`(떼기), 없어진 설명은 `""`
fn patch_body(desired: &Value, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        let gone = if f == "description" {
            json!("")
        } else {
            Value::Null
        };
        out.insert(f.clone(), desired.get(f).cloned().unwrap_or(gone));
    }
    Value::Object(out)
}

impl Adapter for LbAdapter {
    fn tree(&self) -> Option<FolderKind> {
        Some(FolderKind::Network)
    }

    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<LoadBalancerSpec>(cluster, ManifestKind::LoadBalancer)
    }

    fn to_manifest(&self, spec: &Value, world: &World) -> Value {
        let mut out = super::without(spec, &["folder", "monitors"]);
        map_member_vms(&mut out, |s| {
            s.parse::<Uuid>()
                .map_or_else(|_| s.to_string(), |id| world.vm_name(id))
        });
        out
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        folder: Option<Uuid>,
        _state: Option<ManifestState>,
        world: &World,
    ) -> Result<Value, String> {
        let mut out = super::with(spec, "folder", folder.map(|f| Value::String(f.to_string())));
        // 아직 없는 VM(같은 계획에서 먼저 만든다)은 적용 때 다시 푼다.
        map_member_vms(&mut out, |s| {
            world
                .vm_id(s)
                .map_or_else(|| Uuid::nil().to_string(), |id| id.to_string())
        });
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<LoadBalancerSpec>(store)
    }

    /// VIP·건강 검사 주소는 지금 값. 바깥 VIP 주소는 같은 네트워크일 때만 지금 값(다른 네트워크면 서버가 다시 고른다).
    fn keep_server_fields(
        &self,
        _state: &AppState,
        declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        let Some(obj) = desired.as_object_mut() else {
            return;
        };
        if declared.get("vip").is_none_or(Value::is_null)
            && let Some(v) = live.get("vip")
        {
            obj.insert("vip".into(), v.clone());
        }
        if let Some(m) = live.get("monitors") {
            obj.insert("monitors".into(), m.clone());
        }
        if let Some(ext) = obj.get_mut("external").and_then(Value::as_object_mut) {
            let said_ip = declared
                .pointer("/external/ip")
                .is_some_and(|v| !v.is_null());
            if !said_ip {
                match live.get("external") {
                    Some(old) if old.get("network") == ext.get("network") => {
                        if let Some(ip) = old.get("ip") {
                            ext.insert("ip".into(), ip.clone());
                        }
                    }
                    _ => {
                        ext.remove("ip");
                    }
                }
            }
        }
    }

    fn updatable(&self) -> Update {
        Update::Fields(&[
            "description",
            "external",
            "listeners",
            "affinity_secs",
            "monitors",
        ])
    }

    fn keeps_id_on_restore(&self) -> bool {
        true
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let mut nets: Vec<String> = spec
            .get("network")
            .and_then(Value::as_str)
            .map(str::to_string)
            .into_iter()
            .collect();
        nets.extend(
            spec.pointer("/external/network")
                .and_then(Value::as_str)
                .map(str::to_string),
        );
        let mut out = refs_of(ManifestKind::Network, nets);
        out.extend(refs_of(ManifestKind::Vm, member_vms(spec)));
        out
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        state.authz.require_new(
            principal,
            Privilege::NetworkConfig,
            LoadBalancerSpec::KIND,
            folder,
        )
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/load-balancers/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/load-balancers/{key}")
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
            let spec: LoadBalancerSpec = typed(&spec).map_err(bad)?;
            let (_, _, Json(lb)) = Box::pin(create_load_balancer(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateLoadBalancer { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: lb.id,
                task: None,
            })
        })
    }

    fn update<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
        _labels: BTreeMap<String, String>,
        desired: Value,
        fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            // 건강 검사 주소는 서버가 고른다(리스너를 바꾸면 처리기가 다시 정한다).
            let fields: Vec<String> = fields.into_iter().filter(|f| f != "monitors").collect();
            let patch: LoadBalancerPatch = typed(&patch_body(&desired, &fields)).map_err(bad)?;
            Box::pin(patch_load_balancer(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                Json(patch),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: None,
            })
        })
    }

    fn delete<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        _principal: &'a Principal,
        live: &'a Live,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            Box::pin(delete_load_balancer(
                State(state.clone()),
                AuditNote::detached(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: None,
            })
        })
    }

    fn write_labels<'a>(
        &'a self,
        cluster: &'a Cluster,
        id: Uuid,
        revision: u64,
        labels: BTreeMap<String, String>,
    ) -> Fut<'a, ()> {
        write_labels_as::<LoadBalancerSpec>(cluster, id, revision, labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_vms_and_patch_bodies() {
        let mut spec = json!({ "network": "t1", "listeners": [
            { "protocol": "tcp", "port": 80, "members": [{ "vm": "web1", "port": 8080 }, { "ip": "10.0.0.9", "port": 80 }] }
        ] });
        assert_eq!(member_vms(&spec), ["web1"]);
        map_member_vms(&mut spec, |s| format!("{s}-id"));
        assert_eq!(spec["listeners"][0]["members"][0]["vm"], "web1-id");
        let body = patch_body(
            &json!({ "listeners": [] }),
            &["listeners".into(), "external".into(), "description".into()],
        );
        assert_eq!(
            body,
            json!({ "listeners": [], "external": null, "description": "" })
        );
    }
}
