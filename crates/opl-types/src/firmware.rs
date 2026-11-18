//! TPM 상태가 노드를 따라다니는 규칙(계획 59)의 순수 계산: 켤 노드가 TPM 상태를 어디서 얻는지([`tpm_source`])와
//! 펌웨어 사본(UEFI 변수 + TPM)을 밀어 둘 노드([`firmware_copy_nodes`]).

use uuid::Uuid;

use crate::node::{Node, ha_capable};
use crate::placement::HaGroupSpec;
use crate::vm::{Vm, VmStatus};

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
/// 1. 이 노드의 파일 해시가 `tpm_sha256`과 같거나, `tpm_node`가 이 노드이고 파일이 있으면(그 노드의 파일은 해시
///    기록 뒤에 바뀌었을 수 있다) [`TpmSource::Local`]. 해시가 다른 파일은 `tpm_node`에서만 받아들인다(리뷰 2 #2):
///    UEFI 변수만 가진 `firmware_node`의 낡은 파일을 지금 것으로 보지 않는다.
/// 2. 아니면 온라인인 `tpm_node`, 그다음 온라인인 `firmware_copies`에서 받는다([`TpmSource::Fetch`]).
/// 3. `tpm_sha256`이 없으면(이것을 가장 먼저 본다) [`TpmSource::Fresh`]: 이 노드에 파일이 있으면 드라이버가 그대로
///    쓴다. 설계는 `firmware_node`도 없을 때만 새로 만들었으나, UEFI 변수만 있던 VM(`firmware_node` 있음)에 TPM을
///    더하면 어느 노드에도 TPM 상태가 없으므로 해시가 없는 것으로 정한다.
/// 4. 그 밖이면 [`TpmSource::Unavailable`]
pub fn tpm_source(vm: &Vm, node: Uuid, local_hash: Option<&str>, nodes: &[Node]) -> TpmSource {
    let Some(hash) = vm.status.tpm_sha256.as_deref() else {
        return TpmSource::Fresh;
    };
    if local_hash == Some(hash) || (vm.status.tpm_node == Some(node) && local_hash.is_some()) {
        return TpmSource::Local;
    }
    let online = |id: Uuid| nodes.iter().any(|n| n.id == id && n.status.online);
    let mut from: Vec<Uuid> = Vec::new();
    for id in vm.status.tpm_node.iter().chain(&vm.status.firmware_copies) {
        if *id != node && online(*id) && !from.contains(id) {
            from.push(*id);
        }
    }
    if !from.is_empty() {
        return TpmSource::Fetch(from);
    }
    // 이 노드가 `tpm_node`인데 파일이 없으면 오프라인 보유 노드를 알 수 없다.
    let holder = vm.status.tpm_node.filter(|id| *id != node).map(|id| {
        nodes
            .iter()
            .find(|n| n.id == id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    });
    TpmSource::Unavailable(tpm_unavailable(&vm.name, holder.as_deref()))
}

/// 노드 `node`에 있는 VM의 TPM 상태 파일(해시 `local_hash`, 없으면 None)이 지금 것인지(리뷰 2): `node`가 `tpm_node`
/// 이거나(해시 기록 뒤에 바뀌었을 수 있다) 해시가 기록된 `tpm_sha256`과 같다. 펌웨어 회차가 바뀌어 파일을 지울 때
/// 지금 것인 TPM 상태는 남긴다(유일한 지금 사본일 수 있다).
pub fn tpm_is_current(status: &VmStatus, node: Uuid, local_hash: Option<&str>) -> bool {
    status.tpm_node == Some(node)
        || status
            .tpm_sha256
            .as_deref()
            .is_some_and(|h| local_hash == Some(h))
}

/// TPM 상태를 노드 `tpm_from`에서 받은 뒤 UEFI 변수를 받을 노드(리뷰 2 #3): 이 노드(`node`)가 `firmware_node`면 없다(이
/// 노드의 것이 지금 것). 온라인인 `firmware_node`가 있으면 그 노드(TPM을 끈 채 다른 노드에서 돌았으면 변수는 거기에
/// 있다), 아니면 `tpm_from`(사본은 둘을 함께 둔다 — 그 노드의 회차가 다르면 그 노드가 주지 않는다).
pub fn vars_source(vm: &Vm, node: Uuid, tpm_from: Uuid, nodes: &[Node]) -> Option<Uuid> {
    match vm.status.firmware_node {
        Some(f) if f == node => None,
        Some(f) if nodes.iter().any(|n| n.id == f && n.status.online) => Some(f),
        _ => Some(tpm_from),
    }
}

/// UEFI VM의 `firmware_node`를 `to`(그 VM의 변수 파일로 도는 노드)로 옮긴다. `tpm_here`면(TPM VM으로 돈다 — TPM 상태도
/// `to`에 있다) `tpm_node`도 `to`로 옮기고, 옮겼으면 사본(앞 노드의 상태에서 갈라짐)을 비운다. 기록된 TPM 상태가 없으면
/// 지킬 사본도 없어 옮길 때 비운다. 아니면(TPM을 끈 채 돈다)
/// TPM 상태는 `tpm_node`에 그대로 있으므로 `tpm_node`와 사본을 두어 TPM을 다시 켤 때 거기서 받게 한다(리뷰 1 I2·리뷰 2).
pub fn move_firmware_node(status: &mut VmStatus, to: Uuid, tpm_here: bool) {
    let moved = status.firmware_node.replace(to) != Some(to);
    if (tpm_here && status.tpm_node.replace(to) != Some(to))
        || (moved && status.tpm_sha256.is_none())
    {
        status.firmware_copies.clear();
    }
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
/// 아니면 그 뒤에 그룹 밖 노드, 같으면 이름순). 어느 쪽이든 `firmware_node`·`tpm_node`가 아니고, 온라인이고, 유지 보수 중이
/// 아니고, TPM 능력(`firmware_caps.tpm`)이 있는 노드만이다. HA도 복제도 아니면 비어 있다.
pub fn firmware_copy_nodes(vm: &Vm, nodes: &[Node], group: Option<&HaGroupSpec>) -> Vec<Uuid> {
    let usable = |n: &&Node| {
        Some(n.id) != vm.status.firmware_node
            && Some(n.id) != vm.status.tpm_node
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
        v.status.tpm_node = Some(a.id);
        assert_eq!(tpm_source(&v, a.id, None, &nodes), TpmSource::Fresh);
        assert_eq!(tpm_source(&v, b.id, None, &nodes), TpmSource::Fresh);

        // 1. tpm_node가 이 노드이고 파일이 있거나(해시는 달라도 된다) 이 노드의 해시가 같다.
        v.status.tpm_sha256 = Some(hash.clone());
        assert_eq!(tpm_source(&v, a.id, Some("00"), &nodes), TpmSource::Local);
        assert_eq!(tpm_source(&v, b.id, Some(&hash), &nodes), TpmSource::Local);
        // tpm_node인데 파일이 없으면 사본에서 받고, 없으면 켜지 않는다.
        v.status.firmware_copies = vec![b.id];
        assert_eq!(
            tpm_source(&v, a.id, None, &nodes),
            TpmSource::Fetch(vec![b.id])
        );
        v.status.firmware_copies.clear();
        let TpmSource::Unavailable(msg) = tpm_source(&v, a.id, None, &nodes) else {
            panic!("expected unavailable");
        };
        assert!(msg.starts_with("no online node holds"), "{msg}");
        // 2·3. tpm_node가 먼저, 그다음 사본(이 노드와 겹치는 것은 뺀다).
        v.status.firmware_copies = vec![b.id, c.id, a.id];
        assert_eq!(
            tpm_source(&v, b.id, Some("00"), &nodes),
            TpmSource::Fetch(vec![a.id, c.id])
        );
        // tpm_node가 오프라인이면 온라인 사본에서만 받는다.
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
        // tpm_node가 없는데 해시만 있으면(상태를 가진 노드를 모름) 사본만 본다.
        v.status.tpm_node = None;
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

    /// 리뷰 2 #2: UEFI 변수만 가진 `firmware_node`(TPM을 끈 채 켬)의 낡은 TPM 파일(해시 다름)은 지금 것이 아니다. TPM
    /// 상태는 `tpm_node`에서 받는다.
    #[test]
    fn a_stale_tpm_on_the_vars_node_is_not_local() {
        let a = node("a", true, true, false);
        let b = node("b", true, true, false);
        let nodes = [a.clone(), b.clone()];
        let mut v = vm();
        v.status.tpm_sha256 = Some("cd".repeat(32));
        v.status.tpm_node = Some(a.id);
        v.status.firmware_node = Some(a.id);
        // TPM을 끈 채 b에서 켰다: b가 변수의 노드가 되고 TPM 상태는 a에 그대로 있다.
        move_firmware_node(&mut v.status, b.id, false);
        assert_eq!(v.status.firmware_node, Some(b.id));
        assert_eq!(v.status.tpm_node, Some(a.id));
        // b의 파일은 오프라인 동안 남은 낡은 사본(H1)이다: a에서 받는다.
        assert_eq!(
            tpm_source(&v, b.id, Some(&"ab".repeat(32)), &nodes),
            TpmSource::Fetch(vec![a.id])
        );
        assert!(!tpm_is_current(&v.status, b.id, Some(&"ab".repeat(32))));
        assert!(tpm_is_current(&v.status, a.id, Some("00")));
        assert!(tpm_is_current(&v.status, b.id, Some(&"cd".repeat(32))));
        assert!(!tpm_is_current(&VmStatus::default(), b.id, None));
    }

    /// 변수는 `firmware_node`에서, 오프라인이면 TPM 상태를 준 노드에서 받는다(리뷰 2 #3).
    #[test]
    fn vars_come_from_the_vars_node_first() {
        let a = node("a", true, true, false);
        let b = node("b", true, true, false);
        let c = node("c", true, true, false);
        let nodes = [a.clone(), b.clone(), c.clone()];
        let mut v = vm();
        v.status.firmware_node = Some(b.id);
        assert_eq!(vars_source(&v, c.id, a.id, &nodes), Some(b.id));
        assert_eq!(vars_source(&v, b.id, a.id, &nodes), None);
        let offline_b = [a.clone(), node_offline(&b), c.clone()];
        assert_eq!(vars_source(&v, c.id, a.id, &offline_b), Some(a.id));
        v.status.firmware_node = None;
        assert_eq!(vars_source(&v, c.id, a.id, &nodes), Some(a.id));
    }

    /// TPM을 끈 채 다른 노드로 옮기면 TPM 상태의 노드(`tpm_node`)와 사본은 그대로다(리뷰 1 I2). TPM VM으로 옮기면(상태도
    /// 옮겨 옴) `tpm_node`를 옮기고 사본을 비운다.
    #[test]
    fn moving_the_firmware_node_keeps_the_tpm_holder() {
        let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let mut status = VmStatus {
            firmware_node: Some(a),
            tpm_node: Some(a),
            firmware_copies: vec![c, b],
            tpm_sha256: Some("ab".repeat(32)),
            ..Default::default()
        };
        move_firmware_node(&mut status, a, true);
        assert_eq!(status.firmware_copies, [c, b]);
        move_firmware_node(&mut status, b, false);
        assert_eq!(status.firmware_node, Some(b));
        assert_eq!(status.tpm_node, Some(a));
        assert_eq!(status.firmware_copies, [c, b]);
        move_firmware_node(&mut status, c, true);
        assert_eq!(status.firmware_node, Some(c));
        assert_eq!(status.tpm_node, Some(c));
        assert!(status.firmware_copies.is_empty());
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
