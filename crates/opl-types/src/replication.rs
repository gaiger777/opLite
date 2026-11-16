//! 로컬 디스크 복제(계획 55): 노드 로컬 풀의 VM 디스크를 다른 노드의 같은 id 볼륨으로 주기적으로 복제한다.
//! 처음은 전체, 다음부터는 대상마다 따로 둔 dirty bitmap으로 바뀐 블록만 보낸다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::vm::Vm;

/// VM 하나의 복제 대상 수의 최댓값
pub const MAX_REPLICATION_TARGETS: usize = 3;

/// 복제 주기의 기본값(15분)
pub const DEFAULT_INTERVAL: &str = "*/15";

/// 복제 주기 `*/N`의 N 최댓값(분, 하루)
pub const MAX_INTERVAL_MINUTES: u32 = 1440;

/// 주기 문법이 틀렸을 때의 문구
const INTERVAL_ERROR: &str =
    "replication schedule must look like */N with N minutes from 1 to 1440";

/// 복제 비트맵 이름의 접두사(백업 비트맵과 같은 `opl-` 무리)
pub const REPLICA_BITMAP_PREFIX: &str = "opl-repl-";

/// 마이그레이션 따라잡기 임시 비트맵 이름의 접두사(같은 `opl-` 무리)
pub const MIGRATION_BITMAP_PREFIX: &str = "opl-mig-";

/// VM의 복제 대상 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplicationTarget {
    /// 복제본을 둘 노드의 id
    pub node: Uuid,
    /// 주기 `*/N`(분). 앞 시도를 시작한 때에서 N분 뒤에 다시 한다(시계에 맞추지 않는다).
    #[serde(default = "default_interval")]
    #[cfg_attr(feature = "openapi", schema(default = "*/15"))]
    pub schedule: String,
    /// 그 노드에서 이 VM의 로컬 디스크를 모두 둘 풀. 없으면 디스크마다 같은 이름의 풀이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub pool: Option<String>,
}

fn default_interval() -> String {
    DEFAULT_INTERVAL.into()
}

/// 주기 `*/N`의 N(분, 1~1440)
pub fn parse_interval(s: &str) -> Result<u32, String> {
    s.strip_prefix("*/")
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|n| (1..=MAX_INTERVAL_MINUTES).contains(n))
        .ok_or_else(|| INTERVAL_ERROR.to_string())
}

/// 복제 대상 하나의 상태
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ReplicationState {
    /// 아직 끝난 회차가 없다.
    #[default]
    Pending,
    /// `generation`의 시점으로 `volumes`가 모두 대상에 온전히 있다.
    Ready,
    /// 전체 복사가 복제본 위에 쓰는 중이다(쓸 수 없다).
    Syncing,
    /// HA 복구 뒤 남은 옛 원본이다(전체 복사가 필요하다).
    Stale,
}

/// 복제 회차가 담은 로컬 디스크 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReplicaVolume {
    /// 볼륨 id(복제본도 같은 id다)
    pub id: Uuid,
    /// 그 회차의 볼륨 크기(볼륨의 `spec.size_bytes` — 백엔드가 맞춘 이미지의 가상 크기가 아니다)
    pub size_bytes: u64,
}

/// VM `status.replication`의 대상 하나
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReplicaStatus {
    /// 대상 노드의 id
    pub node: Uuid,
    pub state: ReplicationState,
    /// 마지막으로 끝난 회차의 id(그 회차 Task의 id)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub generation: Option<Uuid>,
    /// 그 회차의 시점(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub synced_at: Option<u64>,
    /// 그 회차가 담은 로컬 디스크
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub volumes: Vec<ReplicaVolume>,
    /// 마지막 회차가 보낸 바이트
    #[serde(default)]
    pub last_bytes: u64,
    /// 마지막 회차가 걸린 시간(초)
    #[serde(default)]
    pub last_secs: u64,
    /// 마지막 회차가 전체 복사였는지
    #[serde(default)]
    pub last_full: bool,
    /// 마지막으로 회차를 시작한 때(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_try: Option<u64>,
    /// 마지막 회차가 실패한 이유. 성공하면 지운다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// HA 복구가 이 복제본으로 VM을 켜라고 한 표시. 대상 노드가 복제본을 승격한 뒤 지운다.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub promote: bool,
}

impl ReplicaStatus {
    /// 아직 끝난 회차가 없는 대상 `node`의 상태
    pub fn pending(node: Uuid) -> Self {
        ReplicaStatus {
            node,
            ..Default::default()
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// VM `vm`의 노드 `node` 복제본을 쓸 수 있으면 그 상태: `ready`이고 끝난 회차가 있으며, VM의 지금 로컬 디스크
/// `local`(볼륨 id와 지금 `spec.size_bytes`)이 모두 그 회차에 같은 크기로 담겨 있다. HA 복구·마이그레이션·균형
/// 조정기가 본다.
pub fn replica_ready<'a>(
    vm: &'a Vm,
    node: Uuid,
    local: &[ReplicaVolume],
) -> Option<&'a ReplicaStatus> {
    vm.status
        .replication
        .iter()
        .find(|r| r.node == node)
        .filter(|r| r.state == ReplicationState::Ready && r.generation.is_some())
        .filter(|r| local.iter().all(|v| r.volumes.contains(v)))
}

/// 대상 노드 `node`로의 복제 회차 `run`의 시점 뒤에 바뀐 블록을 따라가는 비트맵 이름
/// `opl-repl-{노드 id simple}-{회차 id simple}`
pub fn replica_bitmap(node: Uuid, run: Uuid) -> String {
    format!("{REPLICA_BITMAP_PREFIX}{}-{}", node.simple(), run.simple())
}

/// [`replica_bitmap`] 이름이면 (대상 노드, 회차)
pub fn parse_replica_bitmap(name: &str) -> Option<(Uuid, Uuid)> {
    let (node, run) = name.strip_prefix(REPLICA_BITMAP_PREFIX)?.split_once('-')?;
    Some((parse_simple(node)?, parse_simple(run)?))
}

/// 마이그레이션 Task `task`의 `k`번째 따라잡기 패스가 두는 임시 비트맵 이름 `opl-mig-{task simple}-{k}`
pub fn migration_bitmap(task: Uuid, k: u8) -> String {
    format!("{MIGRATION_BITMAP_PREFIX}{}-{k}", task.simple())
}

/// 소문자 16진 32자(`Uuid::simple` 모양)의 id
pub(crate) fn parse_simple(s: &str) -> Option<Uuid> {
    if s.len() != 32 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    Uuid::parse_str(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Resource;
    use crate::vm::VmSpec;

    #[test]
    fn target_defaults_the_schedule_and_omits_the_pool() {
        let node = Uuid::now_v7();
        let t: ReplicationTarget =
            serde_json::from_value(serde_json::json!({ "node": node })).unwrap();
        assert_eq!(t.schedule, "*/15");
        assert_eq!(t.pool, None);
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            serde_json::json!({ "node": node, "schedule": "*/15" })
        );
        let t = ReplicationTarget {
            node,
            schedule: "*/5".into(),
            pool: Some("b".into()),
        };
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "node": node, "schedule": "*/5", "pool": "b" })
        );
        assert_eq!(
            serde_json::from_value::<ReplicationTarget>(json).unwrap(),
            t
        );
        assert!(
            serde_json::from_value::<ReplicationTarget>(
                serde_json::json!({ "node": node, "every": "*/5" })
            )
            .is_err()
        );
    }

    #[test]
    fn parse_interval_takes_minutes_from_1_to_1440() {
        assert_eq!(parse_interval("*/1"), Ok(1));
        assert_eq!(parse_interval("*/15"), Ok(15));
        assert_eq!(parse_interval("*/1440"), Ok(1440));
        assert_eq!(parse_interval(DEFAULT_INTERVAL), Ok(15));
        for bad in [
            "*/0",
            "*/1441",
            "15",
            "*/x",
            "0/15",
            "*/",
            "*/+5",
            "*/ 5",
            "*/99999999999",
        ] {
            assert_eq!(
                parse_interval(bad),
                Err(
                    "replication schedule must look like */N with N minutes from 1 to 1440"
                        .to_string()
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn status_omits_unset_fields_and_round_trips() {
        let node = Uuid::now_v7();
        let pending = ReplicaStatus::pending(node);
        assert_eq!(
            serde_json::to_value(&pending).unwrap(),
            serde_json::json!({
                "node": node, "state": "pending", "last_bytes": 0, "last_secs": 0, "last_full": false
            })
        );
        let read: ReplicaStatus =
            serde_json::from_value(serde_json::json!({ "node": node, "state": "pending" }))
                .unwrap();
        assert_eq!(read, pending);

        let ready = ReplicaStatus {
            node,
            state: ReplicationState::Ready,
            generation: Some(Uuid::now_v7()),
            synced_at: Some(1_700_000_000),
            volumes: vec![ReplicaVolume {
                id: Uuid::now_v7(),
                size_bytes: 1 << 30,
            }],
            last_bytes: 12 << 20,
            last_secs: 3,
            last_full: true,
            last_try: Some(1_700_000_000),
            error: Some("node \"b\" is offline".into()),
            promote: true,
        };
        let json = serde_json::to_value(&ready).unwrap();
        assert_eq!(json["promote"], true);
        assert_eq!(json["volumes"][0]["size_bytes"], 1u64 << 30);
        assert_eq!(
            serde_json::from_value::<ReplicaStatus>(json).unwrap(),
            ready
        );
        for (state, name) in [
            (ReplicationState::Ready, "ready"),
            (ReplicationState::Syncing, "syncing"),
            (ReplicationState::Stale, "stale"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
    }

    fn vm_with(replication: Vec<ReplicaStatus>) -> Vm {
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt"
        }))
        .unwrap();
        let mut vm: Vm = Resource::new("web", spec);
        vm.status.replication = replication;
        vm
    }

    #[test]
    fn replica_ready_needs_a_complete_replica_of_every_local_disk() {
        let (b, c) = (Uuid::now_v7(), Uuid::now_v7());
        let root = ReplicaVolume {
            id: Uuid::now_v7(),
            size_bytes: 1 << 30,
        };
        let data = ReplicaVolume {
            id: Uuid::now_v7(),
            size_bytes: 2 << 30,
        };
        let ready = ReplicaStatus {
            node: b,
            state: ReplicationState::Ready,
            generation: Some(Uuid::now_v7()),
            synced_at: Some(100),
            volumes: vec![root, data],
            ..ReplicaStatus::pending(b)
        };
        let vm = vm_with(vec![ready.clone()]);
        assert_eq!(replica_ready(&vm, b, &[root, data]), Some(&ready));
        assert_eq!(replica_ready(&vm, b, &[data]), Some(&ready));
        // 다른 노드에는 복제본이 없다.
        assert_eq!(replica_ready(&vm, c, &[root, data]), None);
        // 회차 뒤에 더한 디스크
        let added = ReplicaVolume {
            id: Uuid::now_v7(),
            size_bytes: 1 << 20,
        };
        assert_eq!(replica_ready(&vm, b, &[root, data, added]), None);
        // 회차 뒤에 늘린 디스크
        let grown = ReplicaVolume {
            size_bytes: 3 << 30,
            ..data
        };
        assert_eq!(replica_ready(&vm, b, &[root, grown]), None);
        // 끝난 회차가 없거나 쓸 수 없는 상태
        for state in [
            ReplicationState::Pending,
            ReplicationState::Syncing,
            ReplicationState::Stale,
        ] {
            let vm = vm_with(vec![ReplicaStatus {
                state,
                ..ready.clone()
            }]);
            assert_eq!(replica_ready(&vm, b, &[root, data]), None, "{state:?}");
        }
        let vm = vm_with(vec![ReplicaStatus {
            generation: None,
            ..ready.clone()
        }]);
        assert_eq!(replica_ready(&vm, b, &[root, data]), None);
    }

    #[test]
    fn replica_bitmaps_name_the_target_and_the_run() {
        let (node, run) = (Uuid::now_v7(), Uuid::now_v7());
        let name = replica_bitmap(node, run);
        assert_eq!(name, format!("opl-repl-{}-{}", node.simple(), run.simple()));
        assert_eq!(parse_replica_bitmap(&name), Some((node, run)));
        assert_eq!(parse_replica_bitmap(&crate::backup::bitmap_name(run)), None);
        assert_eq!(parse_replica_bitmap(&migration_bitmap(run, 1)), None);
        assert_eq!(parse_replica_bitmap("opl-repl-x-y"), None);
        assert_eq!(
            parse_replica_bitmap(&format!("opl-repl-{}", node.simple())),
            None
        );
        assert_eq!(
            parse_replica_bitmap(&name.to_uppercase().replace("OPL-REPL-", "opl-repl-")),
            None
        );
        assert_eq!(
            migration_bitmap(run, 2),
            format!("opl-mig-{}-2", run.simple())
        );
    }
}
