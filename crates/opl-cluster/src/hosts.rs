//! 노드 자원 장부(계획 64 설계 §9의 첫 층). 전용 pCPU와 hugepages는 노드 자원이라, 상태 머신이 커밋 순서대로 노드마다
//! 켜기로 된 VM의 요구 합([`numa::demand`])을 상한([`HostLimits`])과 견준다. 두 API 노드(또는 API와 HA 리더)가 같은
//! 노드의 마지막 전용 CPU에 동시에 VM을 놓아도 하나만 이긴다. 정확한 pCPU 번호는 그 노드의 할당기가 정한다.
//!
//! 전용 CPU·hugepages를 쓰지 않는 VM(`demand` 0)은 보지 않으므로 그런 VM만 있는 클러스터는 지금과 같고, 그런 VM은 모든
//! 노드가 `numa`를 알릴 때만 생기므로 복제본마다 판정이 같다(계획 62 할당량과 같은 논리).

use opl_types::numa::{self, Demand, HostLimits, HostResources, NodeTopology, PageKind};
use opl_types::resource::Kind;
use opl_types::vm::VmSpec;
use redb::{ReadableTable, Table};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::store::StoreError;
use crate::types::{RawResource, Rejection};

type Objects<'t> = Table<'t, (&'static str, u128), &'static [u8]>;

/// 켜기로 된 노드: `power`가 running이고 템플릿이 아니며 `node`가 있다. JSON만 보므로 VM을 다 풀지 않는다.
fn placed_on(spec: &Value) -> Option<Uuid> {
    if spec.get("power")?.as_str()? != "running"
        || spec.get("template").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    spec.get("node")?.as_str()?.parse().ok()
}

/// VM 스펙의 요구. 풀 수 없는 스펙(API가 쓰지 않는다)은 0이다. `any`는 상태의 기록(`hardware.numa.page`)이 있으면 켤
/// 때 고른 크기로 센다([`Demand::resolve_any`]).
fn demand_of(spec: &Value, status: &Value, topology: Option<&NodeTopology>) -> Demand {
    let mut d = VmSpec::deserialize(spec)
        .map(|vm| numa::demand(&vm, topology))
        .unwrap_or_default();
    d.resolve_any(recorded_page(status));
    d
}

/// 상태의 `hardware.numa.page`(켤 때 고른 hugepage 크기)
fn recorded_page(status: &Value) -> Option<PageKind> {
    PageKind::deserialize(status.get("hardware")?.get("numa")?.get("page")?).ok()
}

/// 장부 검사 하나. [`host_check`]가 만들고, VM을 한 번 훑으며 [`HostCheck::add`]로 그 노드의 다른 VM을 모은 뒤
/// [`HostCheck::finish`]가 판정한다. `check_exclusive`의 VM 훑기에 붙여 VM을 한 번만 푼다.
pub(crate) struct HostCheck {
    vm: Uuid,
    node: Uuid,
    limits: HostLimits,
    topology: Option<NodeTopology>,
    /// 이 VM의 쓰기 전·뒤 몫(그 노드에 켜기로 된 때만)
    old: Demand,
    new: Demand,
    /// 그 노드에 켜기로 된 다른 VM의 합
    others: Demand,
    /// VM을 이미 다 훑었다.
    pub(crate) scanned: bool,
}

/// 이 쓰기가 노드의 장부를 늘릴 수 있으면 검사를 준비한다. VM 쓰기만 보고, 쓰기 뒤 켜기로 된 노드가 없거나 요구가
/// 0이거나 그 노드에서 쓰기 전보다 늘지 않으면 None이라 VM을 훑지 않는다. 마이그레이션 확정(`spec.node`가
/// `status.migration.node`로 바뀜)도 None이다 — 받는 쪽 할당기가 이미 잡았다. `old`는 바꾸기 전 리소스(만들기는 None).
pub(crate) fn host_check(
    objects: &Objects,
    kind: Kind,
    id: Uuid,
    spec: &Value,
    old: Option<&RawResource>,
) -> Result<Option<HostCheck>, StoreError> {
    if kind != Kind::Vm {
        return Ok(None);
    }
    let Some(node) = placed_on(spec) else {
        return Ok(None);
    };
    let Ok(vm) = VmSpec::deserialize(spec) else {
        return Ok(None);
    };
    if numa::demand(&vm, None).is_zero() {
        return Ok(None);
    }
    let was_on = old.and_then(|o| placed_on(&o.spec));
    if let Some(o) = old
        && was_on != Some(node)
        && o.status["migration"]["node"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            == Some(node)
    {
        return Ok(None);
    }
    let (resources, topology) = match objects.get((Kind::Node.as_str(), node.as_u128()))? {
        Some(v) => {
            // 두 칸만 푼다(노드 스펙·상태의 다른 칸과 상관없이 같은 판정).
            let raw: RawResource = serde_json::from_slice(v.value())?;
            let field = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
            (
                Option::<HostResources>::deserialize(field(&raw.spec, "host_resources"))
                    .ok()
                    .flatten(),
                Option::<NodeTopology>::deserialize(field(&raw.status, "topology"))
                    .ok()
                    .flatten(),
            )
        }
        None => (None, None),
    };
    let mut new = numa::demand(&vm, topology.as_ref());
    // 스펙 쓰기는 상태를 바꾸지 않는다: 도는 VM의 `any`는 그 기록의 크기 그대로다.
    new.resolve_any(old.and_then(|o| recorded_page(&o.status)));
    let old = match old {
        Some(o) if was_on == Some(node) => demand_of(&o.spec, &o.status, topology.as_ref()),
        _ => Demand::default(),
    };
    if new.within(&old) {
        return Ok(None);
    }
    Ok(Some(HostCheck {
        vm: id,
        node,
        limits: HostLimits::new(resources.as_ref(), topology.as_ref()),
        topology,
        old,
        new,
        others: Demand::default(),
        scanned: false,
    }))
}

impl HostCheck {
    /// 훑기에서 만난 VM 하나(`id`, 저장된 리소스)를 센다.
    pub(crate) fn add(&mut self, id: Uuid, raw: &RawResource) {
        if id != self.vm && placed_on(&raw.spec) == Some(self.node) {
            self.others
                .add(&demand_of(&raw.spec, &raw.status, self.topology.as_ref()));
        }
    }

    /// 아직 훑지 않았으면 VM을 훑고, 그 노드의 상한을 넘으면 `HostResourcesExceeded`.
    pub(crate) fn finish(mut self, objects: &Objects) -> Result<Result<(), Rejection>, StoreError> {
        if !self.scanned {
            let kind = Kind::Vm.as_str();
            for item in objects.range((kind, 0u128)..=(kind, u128::MAX))? {
                let (key, v) = item?;
                let raw: RawResource = serde_json::from_slice(v.value())?;
                self.add(Uuid::from_u128(key.value().1), &raw);
            }
        }
        let mut before = self.others.clone();
        before.add(&self.old);
        let mut after = self.others;
        after.add(&self.new);
        Ok(match numa::exceeded(&self.limits, &before, &after) {
            Some(e) => Err(Rejection::HostResourcesExceeded {
                node: self.node,
                resource: e.resource,
                limit: e.limit,
                used: e.used,
            }),
            None => Ok(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use opl_types::numa::HostResource;
    use opl_types::resource::Resource;
    use redb::Database;
    use redb::backends::InMemoryBackend;
    use serde_json::json;

    use super::*;
    use crate::state_machine::apply_command;
    use crate::store::{NAMES, OBJECTS};
    use crate::types::Command;

    const N1: Uuid = Uuid::from_u128(0xa1);
    const N2: Uuid = Uuid::from_u128(0xa2);

    /// 상태 머신의 명령 적용만 하는 메모리 저장소
    struct Sm {
        db: Database,
        index: u64,
    }

    impl Sm {
        fn new() -> Self {
            let db = Database::builder()
                .create_with_backend(InMemoryBackend::new())
                .unwrap();
            Sm { db, index: 0 }
        }

        fn apply(&mut self, cmd: Command) -> Result<(), Rejection> {
            self.index += 1;
            let txn = self.db.begin_write().unwrap();
            let r = {
                let mut objects = txn.open_table(OBJECTS).unwrap();
                let mut names = txn.open_table(NAMES).unwrap();
                apply_command(&mut objects, &mut names, cmd, self.index)
                    .unwrap()
                    .map(|_| ())
            };
            txn.commit().unwrap();
            r
        }

        fn create(
            &mut self,
            kind: Kind,
            id: Uuid,
            spec: Value,
            status: Value,
        ) -> Result<(), Rejection> {
            let mut resource: RawResource = Resource::new(id.to_string(), spec);
            resource.id = id;
            resource.status = status;
            self.apply(Command::Create { kind, resource })
        }

        fn vm(&mut self, id: u128, spec: Value) -> Result<(), Rejection> {
            self.create(Kind::Vm, Uuid::from_u128(id), spec, Value::Null)
        }

        fn update(&mut self, id: u128, spec: Value) -> Result<(), Rejection> {
            self.apply(Command::UpdateSpec {
                kind: Kind::Vm,
                id: Uuid::from_u128(id),
                expected_revision: None,
                labels: BTreeMap::new(),
                spec,
            })
        }

        fn status(&mut self, id: u128, status: Value) {
            self.apply(Command::UpdateStatus {
                kind: Kind::Vm,
                id: Uuid::from_u128(id),
                status,
            })
            .unwrap();
        }

        /// CPU 0-7(노드 하나, 2M 풀 512개 = 1024 MiB), 전용 `dedicated`
        fn node(&mut self, id: Uuid, dedicated: &str) {
            let spec = json!({
                "address": "127.0.0.1:1",
                "host_resources": {"reserved_cpus": "", "dedicated_cpus": dedicated},
            });
            let status = json!({"topology": {
                "sockets": 1, "threads_per_core": 1, "cpus": "0-7", "pinning": true, "cpuset": true,
                "probed_at": 1,
                "nodes": [{"id": 0, "cpus": "0-7", "memory_mib": 16384,
                           "hugepages": [{"size_kib": 2048, "total": 512}]}],
            }});
            self.create(Kind::Node, id, spec, status).unwrap();
        }
    }

    fn vm(node: Uuid, power: &str, extra: Value) -> Value {
        let mut v = json!({
            "cpu": {"sockets": 1, "cores": 2}, "memory_mib": 512, "machine": "q35",
            "power": power, "node": node,
        });
        for (k, x) in extra.as_object().unwrap() {
            if k == "cpu" {
                for (ck, cv) in x.as_object().unwrap() {
                    v["cpu"][ck] = cv.clone();
                }
            } else {
                v[k] = x.clone();
            }
        }
        v
    }

    fn dedicated(node: Uuid, power: &str) -> Value {
        vm(node, power, json!({"cpu": {"policy": "dedicated"}}))
    }

    fn exceeded(resource: HostResource, limit: u64, used: u64) -> Rejection {
        Rejection::HostResourcesExceeded {
            node: N1,
            resource,
            limit,
            used,
        }
    }

    /// 마지막 전용 CPU는 하나만 잡는다: 만들기·켜기 모두 커밋 순서대로 본다. 꺼진 VM·다른 노드·템플릿·공유 VM은 세지 않는다.
    #[test]
    fn dedicated_cpus_are_counted_per_node() {
        let mut sm = Sm::new();
        sm.node(N1, "2-5");
        sm.node(N2, "2-3");
        sm.vm(1, dedicated(N1, "running")).unwrap();
        sm.vm(2, dedicated(N1, "stopped")).unwrap();
        sm.vm(3, dedicated(N2, "running")).unwrap();
        sm.vm(4, vm(N1, "running", json!({"cpu": {"cores": 8}})))
            .unwrap();
        sm.vm(
            5,
            vm(
                N1,
                "running",
                json!({"cpu": {"policy": "dedicated"}, "template": true}),
            ),
        )
        .unwrap();
        sm.update(2, dedicated(N1, "running")).unwrap();
        assert_eq!(
            sm.vm(6, dedicated(N1, "running")),
            Err(exceeded(HostResource::DedicatedCpus, 4, 6))
        );
        assert_eq!(
            sm.update(
                1,
                vm(
                    N1,
                    "running",
                    json!({"cpu": {"policy": "dedicated", "cores": 3}})
                )
            ),
            Err(exceeded(HostResource::DedicatedCpus, 4, 5))
        );
        // 꺼진 채 만들기는 되고, 켜기는 거절된다. 하나를 끄면 켜진다.
        sm.vm(6, dedicated(N1, "stopped")).unwrap();
        assert_eq!(
            sm.update(6, dedicated(N1, "running")),
            Err(exceeded(HostResource::DedicatedCpus, 4, 6))
        );
        sm.update(1, dedicated(N1, "stopped")).unwrap();
        sm.update(6, dedicated(N1, "running")).unwrap();
        // 에뮬레이터 isolate는 하나를 더 잡는다.
        sm.update(
            1,
            vm(
                N2,
                "running",
                json!({"cpu": {"policy": "dedicated", "cores": 1, "emulator": "isolate"}}),
            ),
        )
        .unwrap_err();
    }

    /// 상한이 줄어 이미 넘친 노드에서도 늘리지 않는 쓰기(이름·디스크 바꾸기, 줄이기, 끄기)는 된다.
    #[test]
    fn writes_that_do_not_grow_pass_on_an_overfull_node() {
        let mut sm = Sm::new();
        sm.node(N1, "2-5");
        sm.vm(1, dedicated(N1, "running")).unwrap();
        sm.vm(2, dedicated(N1, "running")).unwrap();
        // 노드 설정을 바꿔 전용을 둘로(스테이지 5 API는 force로만)
        sm.apply(Command::UpdateSpec {
            kind: Kind::Node,
            id: N1,
            expected_revision: None,
            labels: BTreeMap::new(),
            spec: json!({"address": "127.0.0.1:1",
                         "host_resources": {"reserved_cpus": "", "dedicated_cpus": "2-3"}}),
        })
        .unwrap();
        sm.update(
            1,
            vm(
                N1,
                "running",
                json!({"cpu": {"policy": "dedicated"}, "memory_mib": 1024}),
            ),
        )
        .unwrap();
        sm.update(
            1,
            vm(
                N1,
                "running",
                json!({"cpu": {"policy": "dedicated", "cores": 1}}),
            ),
        )
        .unwrap();
        assert_eq!(
            sm.update(1, dedicated(N1, "running")),
            Err(exceeded(HostResource::DedicatedCpus, 2, 4))
        );
        sm.update(2, dedicated(N1, "stopped")).unwrap();
    }

    /// 마이그레이션 확정(`spec.node`가 `status.migration.node`로)은 보지 않는다. HA 복구(다른 노드로 옮겨 켜기)는 본다.
    #[test]
    fn migration_commit_is_not_checked_but_other_moves_are() {
        let mut sm = Sm::new();
        sm.node(N1, "2-3");
        sm.node(N2, "2-3");
        sm.vm(1, dedicated(N1, "running")).unwrap();
        sm.vm(2, dedicated(N2, "running")).unwrap();
        assert_eq!(
            sm.update(2, dedicated(N1, "running")),
            Err(exceeded(HostResource::DedicatedCpus, 2, 4))
        );
        sm.status(2, json!({"migration": {"node": N1, "task": N2}}));
        sm.update(2, dedicated(N1, "running")).unwrap();
    }

    /// hugepages는 크기마다 풀 전체(MiB)와, `any`는 두 크기 합과 견준다. 토폴로지가 없는 노드는 상한이 0이다.
    #[test]
    fn hugepages_are_counted_per_size_and_any() {
        let mut sm = Sm::new();
        sm.node(N1, "2-3");
        sm.vm(
            1,
            vm(N1, "running", json!({"hugepages": "2M", "memory_mib": 512})),
        )
        .unwrap();
        sm.vm(
            2,
            vm(
                N1,
                "running",
                json!({"hugepages": "any", "memory_mib": 512}),
            ),
        )
        .unwrap();
        assert_eq!(
            sm.vm(
                3,
                vm(N1, "running", json!({"hugepages": "any", "memory_mib": 2}))
            )
            .unwrap_err(),
            exceeded(HostResource::Hugepages, 1024, 1026)
        );
        assert_eq!(
            sm.vm(
                3,
                vm(
                    N1,
                    "running",
                    json!({"hugepages": "1G", "memory_mib": 1024})
                )
            )
            .unwrap_err(),
            exceeded(HostResource::Hugepages1G, 0, 1024)
        );
        // 토폴로지를 모르는 노드
        let none = Uuid::from_u128(0xff);
        assert_eq!(
            sm.vm(4, dedicated(none, "running")),
            Err(Rejection::HostResourcesExceeded {
                node: none,
                resource: HostResource::DedicatedCpus,
                limit: 0,
                used: 2,
            })
        );
        // 쓰지 않는 VM은 늘 된다.
        sm.vm(5, vm(none, "running", json!({}))).unwrap();
    }

    /// 리뷰 1 C1: 호스트 노드에 묶는 보통 메모리(전용·`numa.memory`, hugepages 없이)는 노드의 보통 메모리 합(16384 −
    /// 2M 풀 1024 = 15360 MiB)과 견준다. 묶지 않는 VM은 세지 않는다.
    #[test]
    fn bound_memory_is_counted_against_the_small_memory() {
        let mut sm = Sm::new();
        sm.node(N1, "2-3");
        let bound = |mib: u32| {
            vm(
                N1,
                "running",
                json!({"cpu": {"cores": 1}, "numa": {"nodes": 1, "memory": "strict"}, "memory_mib": mib}),
            )
        };
        sm.vm(1, bound(8192)).unwrap();
        sm.vm(2, vm(N1, "running", json!({"memory_mib": 8192})))
            .unwrap();
        assert_eq!(
            sm.vm(3, bound(8192)),
            Err(exceeded(HostResource::BoundMemory, 15360, 16384))
        );
        sm.vm(3, bound(7168)).unwrap();
    }

    /// 리뷰 1 M6: 켤 때 1G로 풀린 `any` VM은 1G 몫에도 센다(기록 `hardware.numa.page`).
    #[test]
    fn running_any_vms_count_in_the_size_they_got() {
        let mut sm = Sm::new();
        let spec = json!({
            "address": "127.0.0.1:1",
            "host_resources": {"reserved_cpus": "", "dedicated_cpus": "2-3"},
        });
        let status = json!({"topology": {
            "sockets": 1, "threads_per_core": 1, "cpus": "0-7", "pinning": true, "cpuset": true,
            "probed_at": 1,
            "nodes": [{"id": 0, "cpus": "0-7", "memory_mib": 16384,
                       "hugepages": [{"size_kib": 2048, "total": 512}, {"size_kib": 1048576, "total": 2}]}],
        }});
        sm.create(Kind::Node, N1, spec, status).unwrap();
        let any = vm(
            N1,
            "running",
            json!({"hugepages": "any", "memory_mib": 2048}),
        );
        sm.vm(1, any.clone()).unwrap();
        let one_g = vm(
            N1,
            "running",
            json!({"hugepages": "1G", "memory_mib": 1024}),
        );
        // 아직 켜지 않음: `any`는 두 크기 합에만 센다.
        sm.vm(
            2,
            vm(
                N1,
                "stopped",
                json!({"hugepages": "1G", "memory_mib": 1024}),
            ),
        )
        .unwrap();
        sm.status(
            1,
            json!({"hardware": {"numa": {"nodes": [], "page": "1G"}}}),
        );
        assert_eq!(
            sm.update(2, one_g),
            Err(exceeded(HostResource::Hugepages1G, 2048, 3072))
        );
        // 그 VM 자신의 스펙 쓰기(이름 등)는 같은 크기로 세어 늘지 않는다.
        sm.update(1, any).unwrap();
    }
}
