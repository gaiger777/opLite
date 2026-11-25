//! 설정 이력 읽기 RPC(`ReadHistory`, 계획 73 §12.1): 이력 표는 Raft 구성원의 상태 머신에만 있다(관찰 노드는 리소스만
//! 따라온다). 관찰 노드의 API가 이력을 물으면 구성원에게 이 RPC로 읽는다. 관찰 노드가 받으면 `failed_precondition`이다.

use opl_types::history::HistoryRecord;
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 한 번에 읽는 줄 수 한도(타임라인 쪽 하나)
pub const MAX_HISTORY_READ: usize = 5_000;

/// 답 하나의 JSON 바이트 예산(gRPC 한도 64 MiB의 반 — 리뷰 1: 이력은 압축해 재지만 답은 풀어 보낸다)
pub const MAX_REPLY_BYTES: usize = 32 << 20;

/// 무엇을 읽나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "read", rename_all = "snake_case")]
pub enum HistoryRequest {
    /// 리소스 하나의 줄(오래된 것부터)
    Resource { kind: Kind, id: Uuid },
    /// 클러스터 전체를 새것부터 `limit`줄(`before` revision 앞에서)
    Timeline {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<u64>,
        limit: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryReadReply {
    pub records: Vec<HistoryRecord>,
}

/// 답을 바이트 예산 안으로 줄인다(줄 하나는 늘 보낸다). 타임라인(새것부터)은 앞쪽을 남기고 — 받는 쪽은 빈 쪽이 올 때까지
/// `before`로 이어 읽는다 —, 리소스 하나(오래된 것부터)는 새 줄을 남긴다(관찰 노드에서 아주 큰 리소스의 오래된 줄은
/// 보이지 않는다).
pub fn fit_reply(req: &HistoryRequest, records: Vec<HistoryRecord>) -> Vec<HistoryRecord> {
    fit(req, records, MAX_REPLY_BYTES)
}

fn fit(req: &HistoryRequest, mut records: Vec<HistoryRecord>, budget: usize) -> Vec<HistoryRecord> {
    let size = |r: &HistoryRecord| serde_json::to_vec(r).map_or(0, |v| v.len());
    let newest_first = matches!(req, HistoryRequest::Timeline { .. });
    if !newest_first {
        records.reverse();
    }
    let mut total = 0;
    let fits = records
        .iter()
        .take_while(|r| {
            total += size(r);
            total <= budget
        })
        .count();
    records.truncate(fits.max(1));
    if !newest_first {
        records.reverse();
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::history::{ChangeMeta, HistoryOp};
    use serde_json::json;

    fn rec(revision: u64) -> HistoryRecord {
        HistoryRecord::new(
            &ChangeMeta::system(),
            revision,
            Kind::Vm,
            Uuid::nil(),
            "vm".into(),
            HistoryOp::Update,
            Default::default(),
            json!({ "description": "x".repeat(1000) }),
        )
    }

    #[test]
    fn replies_fit_the_byte_budget() {
        let one = serde_json::to_vec(&rec(1)).unwrap().len();
        let timeline = HistoryRequest::Timeline {
            before: None,
            limit: 10,
        };
        let newest_first: Vec<_> = (1..=10).rev().map(rec).collect();
        let kept = fit(&timeline, newest_first, one * 3 + 1);
        assert_eq!(
            kept.iter().map(|r| r.revision).collect::<Vec<_>>(),
            [10, 9, 8]
        );
        let resource = HistoryRequest::Resource {
            kind: Kind::Vm,
            id: Uuid::nil(),
        };
        let oldest_first: Vec<_> = (1..=10).map(rec).collect();
        let kept = fit(&resource, oldest_first, one * 3 + 1);
        assert_eq!(
            kept.iter().map(|r| r.revision).collect::<Vec<_>>(),
            [8, 9, 10]
        );
        // 줄 하나가 예산보다 커도 그 줄은 보낸다.
        assert_eq!(fit(&timeline, vec![rec(1)], 1).len(), 1);
        assert!(fit(&timeline, Vec::new(), 1).is_empty());
    }
}
