//! Task 실패의 원인 코드 붙이기(계획 77-2). Task 실행 코드는 오류를 `String`으로 넘기고 위에서 문맥을 덧붙이므로
//! (`format!("…: {e}")`), 실패를 만드는 곳이 [`coded`]로 코드를 이 Task의 칸(task-local)에 적고 원문을 돌려준다. Task가
//! 실패로 끝나면 [`resolve`]가 마지막 오류 원문에 그 원문이 들어 있는 첫(가장 안쪽) 코드를 고른다 — 다시 시도해
//! 넘어간 오류의 코드는 원문이 남지 않으므로 붙지 않는다. 다른 프로그램(QEMU·커널)의 원문은 어느 경로에서 났는지에 따라
//! 원인과 해결이 다르므로 원문만으로 알아보지 않고, 그 원문이 나오는 경로가 대상을 알고 코드를 단다(리뷰 I2).

use std::sync::{Arc, Mutex};

use orbivisor_cluster::Cluster;
use orbivisor_types::node::NodeSpec;
use orbivisor_types::task_failure::{TaskFailure, code, is_no_space};
use uuid::Uuid;

/// 한 Task가 적어 두는 코드 수. 넘으면 가장 오래된 것을 버린다(다시 시도하는 고리가 앞에서 채워도 마지막 실패의 코드는
/// 남는다 — 리뷰 S5).
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
/// 같은 코드·원문은 한 번만 적는다.
pub(crate) fn coded(failure: TaskFailure, message: impl Into<String>) -> String {
    let message = message.into();
    let _ = SLOT.try_with(|slot| {
        let mut seen = slot.0.lock().unwrap();
        if seen.iter().any(|(f, m)| *f == failure && *m == message) {
            return;
        }
        if seen.len() == MAX {
            seen.remove(0);
        }
        seen.push((failure, message.clone()));
    });
    message
}

/// 지금까지 적힌 코드를 버리고 `failure`만 적는다 — 안쪽 코드보다 이 문맥이 원인을 더 잘 말할 때(선언형 적용의 단계 실패:
/// 안쪽 처리기의 코드는 아직 없는 대상을 가리킬 수 있다 — 리뷰 I6).
pub(crate) fn coded_over(failure: TaskFailure, message: impl Into<String>) -> String {
    let message = message.into();
    let _ = SLOT.try_with(|slot| {
        let mut seen = slot.0.lock().unwrap();
        seen.clear();
        seen.push((failure, message.clone()));
    });
    message
}

/// `message`가 디스크 공간 없음(ENOSPC)이면 `failure`(쓰던 대상 — 풀·백업 풀·저장소·업로드 디렉터리)를 적는다. 아니면
/// 그대로 돌려준다.
pub(crate) fn if_no_space(message: String, failure: impl FnOnce() -> TaskFailure) -> String {
    if is_no_space(&message) {
        coded(failure(), message)
    } else {
        message
    }
}

/// 원인 값에 넣을 노드 이름(읽지 못하면 id)
pub(crate) fn node_name(cluster: &Cluster, id: Uuid) -> String {
    cluster
        .get::<NodeSpec>(id)
        .ok()
        .flatten()
        .map_or_else(|| id.to_string(), |n| n.name)
}

/// 실패한 Task의 원인: 원문에 남은 가장 안쪽 코드, 없으면 실행기의 시간 초과·재시작 중단(`unfinished` — 실행기가 끊은
/// 것만: 다른 곳의 "timed out after …"는 이 Task의 시간 한도가 아니다)
pub(crate) fn resolve(error: &str, slot: Option<&Slot>, unfinished: bool) -> Option<TaskFailure> {
    if let Some(slot) = slot {
        let seen = slot.0.lock().unwrap();
        if let Some((f, _)) = seen
            .iter()
            .find(|(_, m)| !m.is_empty() && error.contains(m))
        {
            return Some(f.clone());
        }
    }
    if unfinished && error.starts_with("timed out after") {
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
        let f = resolve(&error, Some(&slot), false).unwrap();
        assert_eq!(f.code, code::POOL_FULL);
        assert_eq!(f.params["pool"], "local");
    }

    #[tokio::test]
    async fn outside_a_task_nothing_is_recorded() {
        assert_eq!(coded(TaskFailure::new(code::POOL_FULL), "x"), "x");
        assert!(resolve("x", None, false).is_none());
    }

    #[test]
    fn executor_errors() {
        assert_eq!(
            resolve("timed out after 600s", None, true).unwrap().code,
            code::TASK_TIMEOUT
        );
        // 펜싱 장치 등 다른 곳의 시간 초과는 이 Task의 한도가 아니다.
        assert!(resolve("timed out after 10s", None, false).is_none());
        let f = resolve(
            &format!("{INTERRUPTED} while the task was running"),
            None,
            true,
        )
        .unwrap();
        assert_eq!(f.code, code::TASK_INTERRUPTED);
        assert!(f.retryable);
        // 다른 프로그램의 원문만으로는 원인을 고르지 않는다(경로가 단 코드가 없으면 원문만).
        assert!(
            resolve(
                "qemu-img: write failed: No space left on device",
                None,
                false
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn no_space_is_coded_with_the_target_the_path_names() {
        let slot = Slot::default();
        let error = scope(slot.clone(), async {
            let other = if_no_space("connection reset".into(), || {
                TaskFailure::new(code::POOL_OUT_OF_SPACE)
            });
            assert_eq!(other, "connection reset");
            let full = if_no_space("write failed: No space left on device".into(), || {
                TaskFailure::new(code::BACKUP_POOL_FULL).param("pool", "bk")
            });
            format!("backup failed: {full}")
        })
        .await;
        let f = resolve(&error, Some(&slot), false).unwrap();
        assert_eq!(f.code, code::BACKUP_POOL_FULL);
        assert_eq!(f.params["pool"], "bk");
        assert_eq!(slot.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_retry_loop_does_not_push_out_the_last_cause() {
        let slot = Slot::default();
        let error = scope(slot.clone(), async {
            for i in 0..40 {
                // 같은 오류를 되풀이하면 한 번만, 다른 오류가 차면 가장 오래된 것부터 버린다.
                let _ = coded(TaskFailure::new(code::VMWARE_UNREACHABLE), "unreachable");
                let _ = coded(TaskFailure::new(code::DOWNLOAD_FAILED), format!("try {i}"));
            }
            coded(
                TaskFailure::new(code::VMWARE_AUTH),
                "the login was rejected",
            )
        })
        .await;
        assert!(slot.0.lock().unwrap().len() <= MAX);
        assert_eq!(
            resolve(&error, Some(&slot), false).unwrap().code,
            code::VMWARE_AUTH
        );
    }

    #[tokio::test]
    async fn coded_over_replaces_the_inner_codes() {
        let slot = Slot::default();
        let error = scope(slot.clone(), async {
            let inner = coded(
                TaskFailure::new(code::BACKUP_STORE_UNAVAILABLE).param("store", "s3"),
                "backup store \"s3\": unreachable",
            );
            coded_over(
                TaskFailure::new(code::APPLY_STEP_FAILED).param("step", "1/1"),
                format!("step 1/1 (create BackupStore \"s3\") failed: {inner}"),
            )
        })
        .await;
        let f = resolve(&error, Some(&slot), false).unwrap();
        assert_eq!(f.code, code::APPLY_STEP_FAILED);
    }
}
