//! 로컬 디스크 복제(계획 55)의 노드 사이 RPC: 원본 노드의 `VmReplicate` Task가 대상 노드에 받을 준비
//! (`ReplicaPrepare`), 회차 끝내기(`ReplicaFinish`), 버리기(`ReplicaAbort`)를 부른다. 디스크 데이터는 gRPC가
//! 아니라 원본 QEMU가 대상의 받는 QEMU에 NBD+TLS로 바로 보낸다. 받는 쪽 처리는 opld가
//! [`ReplicaReceiver`]로 넣는다.

use async_trait::async_trait;
use orbivisor_types::runtime::ImageFormat;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 받을 준비 요청: 회차 하나에서 복제할 로컬 디스크들
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaPrepareRequest {
    pub vm: Uuid,
    /// 원본 노드에서 도는 `VmReplicate` Task
    pub task: Uuid,
    /// 회차 id(지금은 Task id와 같다)
    pub run: Uuid,
    /// 복제할 디스크(VM 디스크 순서)
    pub volumes: Vec<PrepareVolume>,
    /// 증분 디스크들의 바뀐 바이트 합(원본 비트맵의 `count`). 겹층 공간을 확인한다.
    #[serde(default)]
    pub dirty_bytes: u64,
}

/// 복제할 디스크 하나: 대상 노드에 둘 풀·형식과 원본의 크기
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareVolume {
    /// 볼륨 id(복제본도 같은 id다)
    pub id: Uuid,
    /// 대상 노드에서 복제본을 둘 풀
    pub pool: String,
    /// 복제본의 형식
    pub format: ImageFormat,
    /// 원본의 가상 크기
    pub size_bytes: u64,
    /// 원본이 실제로 쓰는 바이트. 새 복제본의 공간을 확인한다.
    pub allocated_bytes: u64,
    pub mode: PrepareMode,
}

/// 디스크 하나를 어떻게 보내는지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PrepareMode {
    /// 디스크 전체를 복제본에 바로 쓴다.
    Full,
    /// 시점 `base_synced_at`(유닉스 초)의 회차 뒤 바뀐 블록만 겹층에 쓴다. 받는 쪽 복제본이 그 시점보다 옛것이면
    /// 거절한다(`refused`).
    Incremental { base_synced_at: u64 },
}

/// 받을 준비가 끝났다(또는 거절한 디스크가 있다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaPrepareResponse {
    /// 받는 QEMU의 NBD 포트(거절한 디스크가 있으면 0)
    pub port: u16,
    /// 요청 디스크 순서의 NBD 내보내기 이름(볼륨 id simple)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<String>,
    /// 증분을 받을 수 없는 디스크(받는 쪽에 기준 회차가 없다). 있으면 아무것도 준비하지 않았으므로 원본은 그
    /// 디스크를 전체로 바꿔 다시 부른다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<Uuid>,
}

/// 회차 끝내기 요청
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaFinishRequest {
    pub vm: Uuid,
    pub task: Uuid,
    pub run: Uuid,
    /// 원본이 모든 디스크를 보냈는지. 아니면 받는 쪽은 이 회차를 버린다.
    pub ok: bool,
    /// 회차의 시점(유닉스 초)
    pub synced_at: u64,
    /// 이 회차가 보낸 디스크(볼륨 id). 받는 쪽은 이것만 온전해진 것으로 적는다. 비면(옛 원본) 기록의 모든 디스크다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<Uuid>,
}

/// 회차 버리기 요청
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaAbortRequest {
    pub vm: Uuid,
    pub task: Uuid,
    pub run: Uuid,
}

/// 받는 쪽이 거절했거나 실패한 이유. 응답 본문에 담아 보낸다(gRPC 상태는 전송 문제만).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ReplicaError {
    /// 요청이 지금 상태와 맞지 않는다(이 노드가 대상이 아니다, 기능이 모자란 노드가 있다 등).
    #[error("{0}")]
    Rejected(String),
    /// 받는 노드에서 디스크나 받는 QEMU가 실패했다.
    #[error("{0}")]
    Failed(String),
    /// 받는 노드에 닿지 못했거나 그 노드가 복제본을 받지 않는다.
    #[error("cannot reach the target node: {0}")]
    Unavailable(String),
}

/// 대상 노드에서 복제 회차를 받는 처리
#[async_trait]
pub trait ReplicaReceiver: Send + Sync {
    /// 복제본(전체)이나 겹층(증분)을 마련하고 받는 QEMU를 띄워 내보낸다.
    async fn prepare(
        &self,
        req: ReplicaPrepareRequest,
    ) -> Result<ReplicaPrepareResponse, ReplicaError>;

    /// 받는 QEMU를 끝내고, `ok`면 회차를 기록해 겹층을 복제본에 합친다. 아니면 회차를 버린다.
    async fn finish(&self, req: ReplicaFinishRequest) -> Result<(), ReplicaError>;

    /// 회차를 버린다: 받는 QEMU를 끄고 받지 못한 겹층을 지운다.
    async fn abort(&self, req: ReplicaAbortRequest) -> Result<(), ReplicaError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_keep_their_shape() {
        let (vm, run, id) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let req = ReplicaPrepareRequest {
            vm,
            task: run,
            run,
            volumes: vec![
                PrepareVolume {
                    id,
                    pool: "b".into(),
                    format: ImageFormat::Qcow2,
                    size_bytes: 1 << 30,
                    allocated_bytes: 1 << 20,
                    mode: PrepareMode::Full,
                },
                PrepareVolume {
                    id,
                    pool: "b".into(),
                    format: ImageFormat::Raw,
                    size_bytes: 1 << 30,
                    allocated_bytes: 0,
                    mode: PrepareMode::Incremental {
                        base_synced_at: 1700,
                    },
                },
            ],
            dirty_bytes: 65536,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json["volumes"][0]["mode"],
            serde_json::json!({ "kind": "full" })
        );
        assert_eq!(
            json["volumes"][1]["mode"],
            serde_json::json!({ "kind": "incremental", "base_synced_at": 1700 })
        );
        assert_eq!(json["volumes"][1]["format"], "raw");
        assert_eq!(
            serde_json::from_value::<ReplicaPrepareRequest>(json).unwrap(),
            req
        );

        // 거절이 없으면 빼고 보낸다.
        let reply = ReplicaPrepareResponse {
            port: 60000,
            exports: vec![id.simple().to_string()],
            refused: vec![],
        };
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "port": 60000, "exports": [id.simple().to_string()] })
        );
        assert_eq!(
            serde_json::from_value::<ReplicaPrepareResponse>(json).unwrap(),
            reply
        );
        let refused = serde_json::json!({ "port": 0, "refused": [id] });
        assert_eq!(
            serde_json::from_value::<ReplicaPrepareResponse>(refused).unwrap(),
            ReplicaPrepareResponse {
                port: 0,
                exports: vec![],
                refused: vec![id],
            }
        );

        // 끝내기의 디스크 목록은 비면 빼고 보내고, 옛 원본의 요청(목록 없음)도 읽는다.
        let old =
            serde_json::json!({ "vm": vm, "task": run, "run": run, "ok": true, "synced_at": 1700 });
        let finish: ReplicaFinishRequest = serde_json::from_value(old.clone()).unwrap();
        assert!(finish.volumes.is_empty());
        assert_eq!(serde_json::to_value(&finish).unwrap(), old);
        let finish = ReplicaFinishRequest {
            volumes: vec![id],
            ..finish
        };
        assert_eq!(
            serde_json::to_value(&finish).unwrap()["volumes"],
            serde_json::json!([id])
        );
    }
}
