//! Task 실패의 원인 코드 붙이기(계획 77-2). Task 실행 코드는 오류를 `String`으로 넘기고 위에서 문맥을 덧붙이므로
//! (`format!("…: {e}")`), 실패를 만드는 곳이 [`coded`]로 코드를 이 Task의 칸(task-local)에 적고 원문을 돌려준다. Task가
//! 실패로 끝나면 [`resolve`]가 마지막 오류 원문에 그 원문이 들어 있는 첫(가장 안쪽) 코드를 고른다 — 다시 시도해
//! 넘어간 오류의 코드는 원문이 남지 않으므로 붙지 않는다.

use std::sync::{Arc, Mutex};

use orbivisor_cluster::Cluster;
use orbivisor_types::node::NodeSpec;
use orbivisor_types::task_failure::{TaskFailure, classify, code};
use uuid::Uuid;

/// 한 Task가 적은 코드가 이보다 많으면 더 적지 않는다(안쪽 것이 먼저다).
const MAX: usize = 16;

/// 한 Task 실행의 코드 칸
#[derive(Clone, Default)]
pub(crate) struct Slot(Arc<Mutex<Vec<(TaskFailure, String)>>>);

tokio::task_local! {
    static SLOT: Slot;
}

/// `future`를 `slot`을 칸으로 돌린다.
pub(crate) async fn scope<F: Future>(slot: Slot, future: F) -> F::Output {
    SLOT.scope(slot, future).await
}

/// `failure`를 지금 Task의 칸에 적고 `message`를 돌려준다. Task 밖(API 요청)이면 적지 않는다.
pub(crate) fn coded(failure: TaskFailure, message: impl Into<String>) -> String {
    let message = message.into();
    let _ = SLOT.try_with(|slot| {
        let mut seen = slot.0.lock().unwrap();
        if seen.len() < MAX {
            seen.push((failure, message.clone()));
        }
    });
    message
}

/// 원인 값에 넣을 노드 이름(읽지 못하면 id)
pub(crate) fn node_name(cluster: &Cluster, id: Uuid) -> String {
    cluster
        .get::<NodeSpec>(id)
        .ok()
        .flatten()
        .map_or_else(|| id.to_string(), |n| n.name)
}

/// 실패한 Task의 원인: 다른 프로그램의 원문(QEMU TLS, 공간 없음)을 먼저, 다음은 원문에 남은 가장 안쪽 코드, 끝으로
/// 실행기의 시간 초과·재시작 중단
pub(crate) fn resolve(error: &str, slot: Option<&Slot>) -> Option<TaskFailure> {
    if let Some(f) = classify(error) {
        return Some(f);
    }
    if let Some(slot) = slot {
        let seen = slot.0.lock().unwrap();
        if let Some((f, _)) = seen
            .iter()
            .find(|(_, m)| !m.is_empty() && error.contains(m))
        {
            return Some(f.clone());
        }
    }
    if error.starts_with("timed out after") {
        return Some(TaskFailure::new(code::TASK_TIMEOUT));
    }
    if error.starts_with(INTERRUPTED) {
        return Some(TaskFailure::new(code::TASK_INTERRUPTED));
    }
    None
}

/// 다시 뜬 orbivisord가 이어 하지 않는 Task의 오류 머리
pub(crate) const INTERRUPTED: &str = "interrupted: orbivisord restarted";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn picks_the_innermost_code_still_in_the_error() {
        let slot = Slot::default();
        let error = scope(slot.clone(), async {
            // 다시 시도해 넘어간 오류(원문이 남지 않음)
            let _ = coded(TaskFailure::new(code::TARGET_BUSY), "busy for a moment");
            let inner = coded(
                TaskFailure::new(code::POOL_FULL).param("pool", "local"),
                "pool \"local\" has 3 GiB free",
            );
            let outer = coded(
                TaskFailure::new(code::MIGRATION_TARGET_REFUSED),
                format!("the target node cannot receive the VM: {inner}"),
            );
            format!("{outer}; the source VM keeps running")
        })
        .await;
        let f = resolve(&error, Some(&slot)).unwrap();
        assert_eq!(f.code, code::POOL_FULL);
        assert_eq!(f.params["pool"], "local");
    }

    #[tokio::test]
    async fn outside_a_task_nothing_is_recorded() {
        assert_eq!(coded(TaskFailure::new(code::POOL_FULL), "x"), "x");
        assert!(resolve("x", None).is_none());
    }

    #[test]
    fn executor_errors_and_foreign_messages() {
        assert_eq!(
            resolve("timed out after 600s", None).unwrap().code,
            code::TASK_TIMEOUT
        );
        let f = resolve(&format!("{INTERRUPTED} while the task was running"), None).unwrap();
        assert_eq!(f.code, code::TASK_INTERRUPTED);
        assert!(f.retryable);
        // QEMU 원문이 우리 코드보다 먼저
        let slot = Slot::default();
        slot.0.lock().unwrap().push((
            TaskFailure::new(code::MIGRATION_TARGET_REFUSED),
            "the target node cannot receive the VM".into(),
        ));
        let error = "the target node cannot receive the VM: QMP command failed: GenericError: TLS credentials support requires GNUTLS";
        assert_eq!(
            resolve(error, Some(&slot)).unwrap().code,
            code::MIGRATION_TLS_MISSING
        );
    }
}
