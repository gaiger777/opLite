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
}

/// 명령 적용 결과. 성공하면 새 revision(커밋된 로그 인덱스)을 담는다.
/// 명령이 아닌 엔트리(빈 엔트리, 멤버십 변경)도 `Ok(인덱스)`를 돌려준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response(pub Result<u64, Rejection>);

openraft::declare_raft_types!(
    /// opLite의 openraft 타입 설정.
    pub TypeConfig:
        D = Command,
        R = Response,
        NodeId = NodeId,
        Node = openraft::BasicNode,
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
