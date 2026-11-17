//! TPM 상태가 노드를 따라다니는 규칙(계획 59)의 순수 계산: 켤 노드가 TPM 상태를 어디서 얻는지([`tpm_source`])와
//! 펌웨어 사본(UEFI 변수 + TPM)을 밀어 둘 노드([`firmware_copy_nodes`]).

use uuid::Uuid;

use crate::node::{Node, ha_capable};
use crate::placement::HaGroupSpec;
use crate::vm::Vm;

/// 펌웨어 사본을 두는 노드 수의 상한
pub const MAX_FIRMWARE_COPIES: usize = 3;

/// 켤 노드가 TPM 상태를 얻는 곳
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TpmSource {
    /// 이 노드의 파일로 켠다(`firmware_node`가 이 노드이거나 이 노드의 파일 해시가 `tpm_sha256`과 같다). 파일이
    /// 없으면 드라이버가 실패한다(몰래 새로 만들지 않는다).
    Local,
    /// 이 노드들에서 차례로 받는다(`firmware_node`, 그다음 `firmware_copies`, 모두 온라인). 받은 파일의 해시가
    /// `tpm_sha256`과 다르면 버리고 다음 노드를 본다. 모두 실패하면 켜지 않는다.
    Fetch(Vec<Uuid>),
    /// 기록된 TPM 상태가 없다(처음 켜기·복제·초기화 뒤, UEFI 변수만 있던 VM에 TPM을 더함): 이 노드에 파일이 없으면
    /// 새로 만들고 있으면 그대로 쓴다(`RuntimeTpm.fresh`).
    Fresh,
    /// 지금 TPM 상태를 가진 온라인 노드가 없다. 켜지 않고 이 문구를 남긴다.
    Unavailable(String),
}

/// TPM VM `vm`을 노드 `node`에서 켤 때 TPM 상태를 얻는 곳(설계 §5의 켜기 규칙). `local_hash`는 이 노드에 있는 TPM
/// 상태 파일의 SHA-256이다(없으면 None). `nodes`로 온라인 여부와 이름을 본다.
///
/// 1. `firmware_node`가 이 노드이거나 이 노드의 파일 해시가 `tpm_sha256`과 같으면 [`TpmSource::Local`]
/// 2. 아니면 온라인인 `firmware_node`, 그다음 온라인인 `firmware_copies`에서 받는다([`TpmSource::Fetch`]).
/// 3. `tpm_sha256`이 없으면(이것을 가장 먼저 본다) [`TpmSource::Fresh`]: 이 노드에 파일이 있으면 드라이버가 그대로
///    쓴다. 설계는 `firmware_node`도 없을 때만 새로 만들었으나, UEFI 변수만 있던 VM(`firmware_node` 있음)에 TPM을
///    더하면 어느 노드에도 TPM 상태가 없으므로 해시가 없는 것으로 정한다.
/// 4. 그 밖이면 [`TpmSource::Unavailable`]
pub fn tpm_source(vm: &Vm, node: Uuid, local_hash: Option<&str>, nodes: &[Node]) -> TpmSource {
    let Some(hash) = vm.status.tpm_sha256.as_deref() else {
        return TpmSource::Fresh;
    };
    if vm.status.firmware_node == Some(node) || local_hash == Some(hash) {
        return TpmSource::Local;
    }
    let online = |id: Uuid| nodes.iter().any(|n| n.id == id && n.status.online);
    let mut from: Vec<Uuid> = Vec::new();
    for id in vm
        .status
        .firmware_node
        .iter()
        .chain(&vm.status.firmware_copies)
    {
        if *id != node && online(*id) && !from.contains(id) {
            from.push(*id);
        }
    }
    if !from.is_empty() {
        return TpmSource::Fetch(from);
    }
    let holder = vm.status.firmware_node.map(|id| {
        nodes
            .iter()
            .find(|n| n.id == id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    });
    TpmSource::Unavailable(tpm_unavailable(&vm.name, holder.as_deref()))
}

/// TPM 상태를 얻지 못해 VM을 켜지 않을 때의 문구. `holder`는 `firmware_node`의 이름이다(없으면 None).
pub fn tpm_unavailable(vm: &str, holder: Option<&str>) -> String {
    let fix = format!("reset the TPM with `vm firmware reset {vm} --tpm`");
    match holder {
        Some(node) => format!(
            "the TPM state of VM {vm:?} is on node {node:?} (offline) and no other node holds a current copy; \
             start it when {node:?} is back, or {fix}"
        ),
        None => format!(
            "no online node holds the current TPM state of VM {vm:?}; bring back a node that holds it, or {fix}"
        ),
    }
}

/// 펌웨어 사본(UEFI 변수 + TPM)을 밀어 둘 노드([`MAX_FIRMWARE_COPIES`]개까지): 로컬 디스크 복제 대상(스펙 순서)이
/// 먼저이고, 그다음 HA VM이면 HA 가능 노드다(HA 그룹 `group`이 있으면 우선순위가 큰 그룹 노드부터, `restricted`가
/// 아니면 그 뒤에 그룹 밖 노드, 같으면 이름순). 어느 쪽이든 `firmware_node`가 아니고, 온라인이고, 유지 보수 중이
/// 아니고, TPM 능력(`firmware_caps.tpm`)이 있는 노드만이다. HA도 복제도 아니면 비어 있다.
pub fn firmware_copy_nodes(vm: &Vm, nodes: &[Node], group: Option<&HaGroupSpec>) -> Vec<Uuid> {
    let usable = |n: &&Node| {
        Some(n.id) != vm.status.firmware_node
            && n.status.online
            && !n.spec.maintenance
            && !n.spec.witness
            && n.status.firmware_caps.is_some_and(|c| c.tpm)
    };
    let mut out: Vec<Uuid> = Vec::new();
    for target in &vm.spec.replication {
        if let Some(n) = nodes.iter().filter(usable).find(|n| n.id == target.node)
            && !out.contains(&n.id)
        {
            out.push(n.id);
        }
    }
    if vm.spec.ha_active() {
        let mut ha: Vec<(&Node, Option<u32>)> = nodes
            .iter()
            .filter(usable)
            .filter(|n| ha_capable(n))
            .map(|n| (n, group.and_then(|g| g.priority(&n.name))))
            .filter(|(_, p)| p.is_some() || !group.is_some_and(|g| g.restricted))
            .collect();
        // 그룹 노드(우선순위 큰 것부터) → 그룹 밖 노드, 같으면 이름순
        ha.sort_by(|(a, pa), (b, pb)| pb.cmp(pa).then_with(|| a.name.cmp(&b.name)));
        for (n, _) in ha {
            if !out.contains(&n.id) {
                out.push(n.id);
            }
        }
    }
    out.truncate(MAX_FIRMWARE_COPIES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Fencing, FirmwareCaps, NodeHa, NodeSpec};
    use crate::placement::HaGroupNode;
    use crate::replication::ReplicationTarget;
    use crate::resource::Resource;
    use crate::vm::VmHa;

    fn node(name: &str, online: bool, tpm: bool, ha: bool) -> Node {
        let mut n: Node = Resource::new(
            name,
            NodeSpec {
                address: format!("{name}:8007"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status.online = online;
        n.status.firmware_caps = Some(FirmwareCaps {
            tpm,
            secure_boot: false,
        });
        if ha {
            n.status.ha = Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 60,
                fence_after_secs: None,
            });
        }
        n
    }

    fn vm() -> Vm {
        Resource::new(
            "win",
            serde_json::from_value(serde_json::json!({
                "cpu": { "cores": 2 },
                "memory_mib": 4096,
                "firmware": "uefi",
                "machine": "q35",
                "tpm": true
            }))
            .unwrap(),
        )
    }

    #[test]
    fn tpm_source_follows_the_start_rules() {
        let a = node("a", true, true, false);
        let b = node("b", true, true, false);
        let c = node("c", true, true, false);
        let nodes = [a.clone(), b.clone(), c.clone()];
        let hash = "ab".repeat(32);

        // 기록된 상태가 없다(처음 켜기, UEFI 변수만 있던 VM에 TPM을 더함): 어느 노드에서든 새로(있으면 그대로).
        let mut v = vm();
        assert_eq!(tpm_source(&v, a.id, None, &nodes), TpmSource::Fresh);
        v.status.firmware_node = Some(a.id);
        assert_eq!(tpm_source(&v, a.id, None, &nodes), TpmSource::Fresh);
        assert_eq!(tpm_source(&v, b.id, None, &nodes), TpmSource::Fresh);

        // 1. firmware_node가 이 노드이거나 이 노드의 해시가 같다.
        v.status.tpm_sha256 = Some(hash.clone());
        assert_eq!(tpm_source(&v, a.id, None, &nodes), TpmSource::Local);
        assert_eq!(tpm_source(&v, b.id, Some(&hash), &nodes), TpmSource::Local);
        // 2·3. firmware_node가 먼저, 그다음 사본(이 노드와 겹치는 것은 뺀다).
        v.status.firmware_copies = vec![b.id, c.id, a.id];
        assert_eq!(
            tpm_source(&v, b.id, Some("00"), &nodes),
            TpmSource::Fetch(vec![a.id, c.id])
        );
        // firmware_node가 오프라인이면 온라인 사본에서만 받는다.
        let offline_a = [node_offline(&a), b.clone(), c.clone()];
        assert_eq!(
            tpm_source(&v, b.id, None, &offline_a),
            TpmSource::Fetch(vec![c.id])
        );
        // 5. 아무 데도 없으면 켜지 않는다.
        v.status.firmware_copies.clear();
        assert_eq!(
            tpm_source(&v, b.id, None, &offline_a),
            TpmSource::Unavailable(
                "the TPM state of VM \"win\" is on node \"a\" (offline) and no other node holds a current \
                 copy; start it when \"a\" is back, or reset the TPM with `vm firmware reset win --tpm`"
                    .into()
            )
        );
        // firmware_node가 없는데 해시만 있으면(상태를 가진 노드를 모름) 사본만 본다.
        v.status.firmware_node = None;
        let TpmSource::Unavailable(msg) = tpm_source(&v, b.id, None, &nodes) else {
            panic!("expected unavailable");
        };
        assert!(msg.starts_with("no online node holds"), "{msg}");
        v.status.firmware_copies = vec![c.id];
        assert_eq!(
            tpm_source(&v, b.id, None, &nodes),
            TpmSource::Fetch(vec![c.id])
        );
    }

    fn node_offline(n: &Node) -> Node {
        let mut n = n.clone();
        n.status.online = false;
        n
    }

    #[test]
    fn copy_nodes_put_replication_targets_first_then_ha_nodes_by_group() {
        let a = node("a", true, true, true);
        let b = node("b", true, true, true);
        let c = node("c", true, true, true);
        let d = node("d", true, true, true);
        let no_tpm = node("e", true, false, true);
        let off = node("f", false, true, true);
        let not_ha = node("g", true, true, false);
        let nodes = [
            a.clone(),
            b.clone(),
            c.clone(),
            d.clone(),
            no_tpm.clone(),
            off,
            not_ha.clone(),
        ];

        // HA도 복제도 아니면 사본이 없다.
        let mut v = vm();
        v.status.firmware_node = Some(a.id);
        assert!(firmware_copy_nodes(&v, &nodes, None).is_empty());

        // 복제 대상: 스펙 순서, 능력 없는 노드는 뺀다.
        v.spec.replication = [not_ha.id, no_tpm.id]
            .map(|node| ReplicationTarget {
                node,
                schedule: "*/15".into(),
                pool: None,
            })
            .to_vec();
        assert_eq!(firmware_copy_nodes(&v, &nodes, None), vec![not_ha.id]);

        // HA: 그룹이 없으면 이름순, 복제 대상 뒤, 셋까지.
        v.spec.ha = Some(VmHa::default());
        assert_eq!(
            firmware_copy_nodes(&v, &nodes, None),
            vec![not_ha.id, b.id, c.id]
        );
        v.spec.replication.clear();
        assert_eq!(
            firmware_copy_nodes(&v, &nodes, None),
            vec![b.id, c.id, d.id]
        );
        // 그룹 우선순위가 큰 노드부터, 그다음 그룹 밖 노드
        let group = HaGroupSpec {
            nodes: vec![
                HaGroupNode {
                    node: "c".into(),
                    priority: 1,
                },
                HaGroupNode {
                    node: "d".into(),
                    priority: 10,
                },
            ],
            restricted: false,
            nofailback: false,
        };
        assert_eq!(
            firmware_copy_nodes(&v, &nodes, Some(&group)),
            vec![d.id, c.id, b.id]
        );
        let restricted = HaGroupSpec {
            restricted: true,
            ..group
        };
        assert_eq!(
            firmware_copy_nodes(&v, &nodes, Some(&restricted)),
            vec![d.id, c.id]
        );
    }
}
