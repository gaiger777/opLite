use std::collections::BTreeMap;
use std::fmt;
use std::io::Cursor;

use opl_types::resource::{Kind, Resource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// 클러스터 노드 식별자. `Node` 리소스의 id와 같은 UUID다.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct NodeId(pub Uuid);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// openraft 저장소 테스트 모음(`openraft::testing::Suite`)이 숫자로 노드를 만들 때 쓴다.
impl From<u64> for NodeId {
    fn from(n: u64) -> Self {
        NodeId(Uuid::from_u64_pair(0, n))
    }
}

/// 저장소가 다루는 종류 없는 리소스. spec과 status는 JSON 그대로 둔다.
pub type RawResource = Resource<Value, Value>;

/// Raft 로그에 기록되는 명령. 상태 머신이 커밋 순서대로 적용한다.
///
/// 저장 형식이다. 변형이나 필드를 추가할 때는 설계 문서 3.1절의 호환성 규칙을 따른다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Command {
    /// 새 리소스. `resource.revision`은 무시하고 커밋 인덱스로 채운다.
    Create { kind: Kind, resource: RawResource },
    /// spec과 labels를 바꾼다. `expected_revision`이 있으면 현재 revision과 같아야 한다.
    UpdateSpec {
        kind: Kind,
        id: Uuid,
        expected_revision: Option<u64>,
        labels: BTreeMap<String, String>,
        spec: Value,
    },
    /// status만 바꾼다. reconciler가 쓰며 revision을 확인하지 않는다.
    UpdateStatus { kind: Kind, id: Uuid, status: Value },
    /// 현재 revision이 `expected_revision`일 때만 status를 바꾼다. `UpdateStatus`에 필드를 더하면
    /// 옛 노드가 그 필드를 버리고 확인 없이 덮어쓰므로 새 명령으로 둔다. 옛 노드는 모르는 명령을
    /// 오류로 처리한다(설계 3.1절).
    UpdateStatusIf {
        kind: Kind,
        id: Uuid,
        expected_revision: u64,
        status: Value,
    },
    Delete {
        kind: Kind,
        id: Uuid,
        expected_revision: Option<u64>,
    },
}

/// 상태 머신이 명령을 거부한 이유. 모든 노드에서 똑같이 결정된다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Rejection {
    #[error("{} {id} already exists", kind.as_str())]
    AlreadyExists { kind: Kind, id: Uuid },
    #[error("{} name {name:?} is already taken", kind.as_str())]
    NameTaken { kind: Kind, name: String },
    #[error("{} {id} not found", kind.as_str())]
    NotFound { kind: Kind, id: Uuid },
    #[error("revision mismatch: expected {expected}, actual {actual}")]
    RevisionMismatch { expected: u64, actual: u64 },
    /// 클러스터 안에서 하나만 쓸 수 있는 값(VM에 붙인 볼륨, MAC 주소)이 이미 쓰이고 있다.
    #[error("{field} {value} is already used by {} {holder}", kind.as_str())]
    InUse {
        kind: Kind,
        field: String,
        value: String,
        holder: Uuid,
    },
    /// 리더가 이 노드를 펜싱하는 중이라 리스를 갱신해 주지 않는다(설계 §4.2).
    #[error("node {node} is being fenced")]
    Fenced { node: NodeId },
}

/// 리더가 처리하는 요청. 팔로워는 받은 쓰기와 멤버십 변경을 이것으로 리더에 전달한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum LeaderRequest {
    Write {
        command: Command,
    },
    /// 노드를 learner로 더한다. 따라잡기를 기다리지 않는다.
    AddLearner {
        id: NodeId,
        addr: String,
    },
    /// learner가 로그를 따라잡으면 투표 멤버로 올린다.
    AddVoter {
        id: NodeId,
    },
    /// 노드를 멤버십에서 뺀다(투표 멤버이든 learner이든).
    Remove {
        id: NodeId,
    },
    /// 노드의 HA 리스를 갱신한다. 리더는 과반수의 리더인지 확인한 뒤 받은 시각을 적고 자기가
    /// 적용한 마지막 로그 인덱스를 돌려준다. 로그에는 아무것도 쓰지 않는다.
    RenewLease {
        node: NodeId,
    },
    /// 노드의 VIP 리스를 갱신한다(설계 §4 내장 VIP). `vips`는 이 노드가 가질 수 있는 VIP 주소(IP)다.
    /// 헬스 체크가 200인 노드만 보낸다. 리더는 `RenewLease`처럼 과반수의 리더인지 확인한 뒤 받은 시각을
    /// 적고 자기가 적용한 마지막 로그 인덱스를 돌려준다. 로그에는 아무것도 쓰지 않는다.
    RenewVip {
        node: NodeId,
        vips: Vec<String>,
    },
}

/// 명령 적용 결과. 성공하면 새 revision(커밋된 로그 인덱스)을 담는다.
/// 명령이 아닌 엔트리(빈 엔트리, 멤버십 변경)도 `Ok(인덱스)`를 돌려준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response(pub Result<u64, Rejection>);

/// Raft 구성원 노드 값. 노드에 닿는 클러스터 링크들(`host:port`)이고 앞이 우선이다(설계 §7).
///
/// 저장 형식이다(로그·스냅샷·노드 간 gRPC). 늘 `addr`(첫 링크)를 쓰고 링크가 둘 이상일 때만
/// `addrs`를 더 쓴다. 링크 하나면 `openraft::BasicNode`와 바이트까지 같고, 옛 노드는 여러 링크
/// 항목을 첫 링크 하나로 읽는다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrbNode {
    pub addrs: Vec<String>,
}

impl OrbNode {
    pub fn new<S: Into<String>>(addrs: impl IntoIterator<Item = S>) -> Self {
        Self {
            addrs: addrs.into_iter().map(Into::into).collect(),
        }
    }

    /// 링크 하나인 노드.
    pub fn single(addr: impl Into<String>) -> Self {
        Self {
            addrs: vec![addr.into()],
        }
    }

    /// 첫 링크. 이 계획 전의 "그 노드의 주소" 자리다. 링크가 없으면 `""`.
    pub fn addr(&self) -> &str {
        self.addrs.first().map_or("", String::as_str)
    }
}

impl fmt::Display for OrbNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.addrs.join(","))
    }
}

impl Serialize for OrbNode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let many = self.addrs.len() > 1;
        let mut st = serializer.serialize_struct("OrbNode", if many { 2 } else { 1 })?;
        st.serialize_field("addr", self.addr())?;
        if many {
            st.serialize_field("addrs", &self.addrs)?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for OrbNode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            addr: String,
            #[serde(default)]
            addrs: Vec<String>,
        }
        let w = Wire::deserialize(deserializer)?;
        if !w.addrs.is_empty() {
            return Ok(Self { addrs: w.addrs });
        }
        // 빈 `addr`는 링크 없음(`Default`)으로 읽어 쓰고 읽기가 같은 값이 되게 한다.
        if w.addr.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self::single(w.addr))
    }
}

openraft::declare_raft_types!(
    /// Orbivisor의 openraft 타입 설정.
    pub TypeConfig:
        D = Command,
        R = Response,
        NodeId = NodeId,
        Node = OrbNode,
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn node_id_is_a_plain_uuid_string() {
        let id = NodeId::from(7);
        assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000007");
        assert_eq!(serde_json::to_value(id).unwrap(), json!(id.to_string()));
    }

    /// Raft 로그에 남는 형식이다. 바뀌면 이미 기록된 로그를 읽지 못한다.
    #[test]
    fn command_storage_format_is_stable() {
        let id = Uuid::from_u128(1);
        let cmd = Command::Delete {
            kind: Kind::StoragePool,
            id,
            expected_revision: Some(3),
        };
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            json!({
                "op": "delete",
                "kind": "storage-pool",
                "id": id.to_string(),
                "expected_revision": 3
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);
    }

    #[test]
    fn create_and_update_spec_storage_format_is_stable() {
        let id = Uuid::from_u128(1);
        let cmd = Command::Create {
            kind: Kind::Volume,
            resource: Resource {
                id,
                name: "disk-a".into(),
                revision: 0,
                labels: BTreeMap::from([("tier".into(), "gold".into())]),
                spec: json!({"size_bytes": 1}),
                status: json!(null),
            },
        };
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            json!({
                "op": "create",
                "kind": "volume",
                "resource": {
                    "id": id.to_string(),
                    "name": "disk-a",
                    "revision": 0,
                    "labels": {"tier": "gold"},
                    "spec": {"size_bytes": 1},
                    "status": null
                }
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);

        let cmd = Command::UpdateSpec {
            kind: Kind::Vm,
            id,
            expected_revision: None,
            labels: BTreeMap::new(),
            spec: json!({"cpus": 2}),
        };
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            json!({
                "op": "update-spec",
                "kind": "vm",
                "id": id.to_string(),
                "expected_revision": null,
                "labels": {},
                "spec": {"cpus": 2}
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);
    }

    /// 노드 값은 저장 형식이다. 링크 하나는 `BasicNode`와 같고 옛 형식도 읽는다(계획 49).
    #[test]
    fn orb_node_storage_format() {
        let old: OrbNode = serde_json::from_value(json!({"addr": "a:8007"})).unwrap();
        assert_eq!(old, OrbNode::single("a:8007"));
        assert_eq!(old.addr(), "a:8007");
        assert_eq!(
            serde_json::to_string(&OrbNode::single("a:8007")).unwrap(),
            r#"{"addr":"a:8007"}"#
        );

        let two = OrbNode::new(["a", "b"]);
        let text = serde_json::to_string(&two).unwrap();
        assert_eq!(text, r#"{"addr":"a","addrs":["a","b"]}"#);
        assert_eq!(serde_json::from_str::<OrbNode>(&text).unwrap(), two);
        assert_eq!(two.to_string(), "a,b");
        // 옛 노드(openraft `BasicNode`)는 첫 링크 하나로 읽는다.
        let basic: openraft::BasicNode = serde_json::from_str(&text).unwrap();
        assert_eq!(basic.addr, "a");

        let empty: OrbNode = serde_json::from_value(json!({"addr": "a", "addrs": []})).unwrap();
        assert_eq!(empty, OrbNode::single("a"));
        assert_eq!(OrbNode::default().addr(), "");
        let none = serde_json::to_string(&OrbNode::default()).unwrap();
        assert_eq!(
            serde_json::from_str::<OrbNode>(&none).unwrap(),
            OrbNode::default()
        );
    }

    /// 계획 48(ebb8f75)까지 `BasicNode`로 쓴 멤버십 로그 항목·`StoredMembership`·스냅샷 메타를
    /// 새 노드 타입으로 읽는다.
    #[test]
    fn basic_node_membership_formats_still_read() {
        let n1 = NodeId::from(1);
        let n2 = NodeId::from(2);
        let want = BTreeMap::from([
            (n1, OrbNode::single("10.0.0.1:8007")),
            (n2, OrbNode::single("10.0.0.2:8007")),
        ]);
        let nodes_of = |m: &openraft::Membership<NodeId, OrbNode>| {
            m.nodes()
                .map(|(id, n)| (*id, n.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let membership = r#"{"configs":[["00000000-0000-0000-0000-000000000001"]],"nodes":{"00000000-0000-0000-0000-000000000001":{"addr":"10.0.0.1:8007"},"00000000-0000-0000-0000-000000000002":{"addr":"10.0.0.2:8007"}}}"#;
        let log_id = r#"{"leader_id":{"term":3,"node_id":"00000000-0000-0000-0000-000000000001"},"index":7}"#;

        let entry_text =
            format!(r#"{{"log_id":{log_id},"payload":{{"Membership":{membership}}}}}"#);
        let entry: openraft::Entry<TypeConfig> = serde_json::from_str(&entry_text).unwrap();
        assert_eq!(entry.log_id.index, 7);
        let openraft::EntryPayload::Membership(m) = &entry.payload else {
            panic!("not a membership entry: {entry:?}");
        };
        assert_eq!(nodes_of(m), want);
        // 링크 하나는 쓴 바이트도 같다.
        assert_eq!(serde_json::to_string(&entry).unwrap(), entry_text);

        let stored = format!(r#"{{"log_id":{log_id},"membership":{membership}}}"#);
        let stored: openraft::StoredMembership<NodeId, OrbNode> =
            serde_json::from_str(&stored).unwrap();
        assert_eq!(nodes_of(stored.membership()), want);

        let meta = format!(
            r#"{{"last_log_id":{log_id},"last_membership":{{"log_id":{log_id},"membership":{membership}}},"snapshot_id":"7-abc"}}"#
        );
        let meta: openraft::SnapshotMeta<NodeId, OrbNode> = serde_json::from_str(&meta).unwrap();
        assert_eq!(meta.snapshot_id, "7-abc");
        assert_eq!(nodes_of(meta.last_membership.membership()), want);
    }

    /// 확인하지 않는 status 쓰기는 예전 형식 그대로다. 확인하는 쓰기는 새 명령이다.
    #[test]
    fn update_status_storage_format_is_stable() {
        let id = Uuid::from_u128(1);
        let old = json!({
            "op": "update-status",
            "kind": "vm",
            "id": id.to_string(),
            "status": {"phase": "running"}
        });
        let cmd = Command::UpdateStatus {
            kind: Kind::Vm,
            id,
            status: json!({"phase": "running"}),
        };
        assert_eq!(serde_json::to_value(&cmd).unwrap(), old);
        assert_eq!(serde_json::from_value::<Command>(old).unwrap(), cmd);

        let cmd = Command::UpdateStatusIf {
            kind: Kind::Vm,
            id,
            expected_revision: 7,
            status: json!(null),
        };
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            json!({
                "op": "update-status-if",
                "kind": "vm",
                "id": id.to_string(),
                "expected_revision": 7,
                "status": null
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);
    }

    /// 노드 사이로 오가는 형식이다. 리스 갱신과 그 거부.
    #[test]
    fn lease_wire_format_is_stable() {
        let node = NodeId::from(2);
        let req = LeaderRequest::RenewLease { node };
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value,
            json!({"op": "renew-lease", "node": node.to_string()})
        );
        assert_eq!(serde_json::from_value::<LeaderRequest>(value).unwrap(), req);

        let r = Rejection::Fenced { node };
        let value = serde_json::to_value(&r).unwrap();
        assert_eq!(value, json!({"reason": "fenced", "node": node.to_string()}));
        assert_eq!(serde_json::from_value::<Rejection>(value).unwrap(), r);
    }

    /// 노드 사이로 오가는 형식이다. VIP 리스 갱신.
    #[test]
    fn vip_lease_wire_format_is_stable() {
        let node = NodeId::from(2);
        let req = LeaderRequest::RenewVip {
            node,
            vips: vec!["192.168.0.200".into()],
        };
        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(
            value,
            json!({"op": "renew-vip", "node": node.to_string(), "vips": ["192.168.0.200"]})
        );
        assert_eq!(serde_json::from_value::<LeaderRequest>(value).unwrap(), req);
    }

    #[test]
    fn rejection_message_uses_wire_kind_name() {
        let r = Rejection::NotFound {
            kind: Kind::StoragePool,
            id: Uuid::from_u128(1),
        };
        assert_eq!(
            r.to_string(),
            "storage-pool 00000000-0000-0000-0000-000000000001 not found"
        );
    }

    #[test]
    fn unknown_command_is_an_error_not_ignored() {
        let value = json!({"op": "rename", "kind": "vm"});
        assert!(serde_json::from_value::<Command>(value).is_err());
    }
}
