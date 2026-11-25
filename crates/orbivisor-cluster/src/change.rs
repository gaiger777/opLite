//! 쓰기의 출처(계획 73 §7.1). API 미들웨어·Task 실행기가 작업 지역 값으로 두면 [`Cluster::write`](crate::Cluster)가
//! 읽어 `Command::Attributed`로 싼다. 값이 없는 쓰기(리더 루프·reconciler·시험)는 `System`이다.

use std::future::Future;
use std::sync::{Arc, Mutex};

use orbivisor_types::history::ChangeMeta;
use orbivisor_types::resource::Kind;
use uuid::Uuid;

tokio::task_local! {
    static CHANGE: ChangeContext;
    static REUSE: Arc<Mutex<Option<(Kind, Uuid)>>>;
}

/// 지운 리소스 살리기(계획 73 §12.3): `fut` 안에서 처음 하는 종류 `kind`의 [`Cluster::create`](crate::Cluster)가
/// 리소스 id를 `id`로 바꿔 만든다(만들기 처리기를 그대로 거치며 옛 id를 되살린다 — 다른 리소스가 id로 가리키던 참조가
/// 다시 풀린다). 답의 둘째 값은 그 바꿈이 일어났는지다.
pub async fn reuse_id<F: Future>(kind: Kind, id: Uuid, fut: F) -> (F::Output, bool) {
    let slot = Arc::new(Mutex::new(Some((kind, id))));
    let out = REUSE.scope(slot.clone(), fut).await;
    let used = slot.lock().unwrap().is_none();
    (out, used)
}

/// 만들기가 쓸 옛 id(종류가 맞으면 한 번만 꺼낸다)
pub(crate) fn take_reused_id(kind: Kind) -> Option<Uuid> {
    REUSE
        .try_with(|slot| {
            let mut slot = slot.lock().unwrap();
            match *slot {
                Some((k, id)) if k == kind => {
                    *slot = None;
                    Some(id)
                }
                _ => None,
            }
        })
        .ok()
        .flatten()
}

/// 지금 작업의 출처. 요청 처리 중에 채워지는 칸(인증 뒤의 사용자)이 있어 나눠 쥔 값이다 — 미들웨어가 범위를 연 뒤
/// 인증 추출기가 [`ChangeContext::update`]로 주체를 적는다.
#[derive(Clone, Default)]
pub struct ChangeContext(Arc<Mutex<ChangeMeta>>);

impl ChangeContext {
    pub fn new(meta: ChangeMeta) -> Self {
        Self(Arc::new(Mutex::new(meta)))
    }

    /// 출처를 고친다(같은 범위의 뒤 쓰기부터 보인다).
    pub fn update(&self, f: impl FnOnce(&mut ChangeMeta)) {
        f(&mut self.0.lock().unwrap());
    }

    /// 지금 값
    pub fn meta(&self) -> ChangeMeta {
        self.0.lock().unwrap().clone()
    }

    /// `fut` 안의 쓰기가 이 출처를 쓴다. 안에서 띄운 작업(`tokio::spawn`)에는 넘어가지 않는다.
    pub async fn scope<F: Future>(self, fut: F) -> F::Output {
        CHANGE.scope(self, fut).await
    }

    /// 지금 작업의 출처(범위 밖이면 `None`)
    pub fn current() -> Option<ChangeMeta> {
        CHANGE.try_with(ChangeContext::meta).ok()
    }

    /// 지금 작업의 출처 값 자체(범위 밖이면 `None`) — 범위 안에서 칸을 고칠 때
    pub fn current_context() -> Option<ChangeContext> {
        CHANGE.try_with(Clone::clone).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbivisor_types::history::ChangeSource;

    #[tokio::test]
    async fn scope_carries_and_updates_meta() {
        assert_eq!(ChangeContext::current(), None);
        let ctx = ChangeContext::new(ChangeMeta {
            source: ChangeSource::Cli,
            ..ChangeMeta::default()
        });
        let seen = ctx
            .clone()
            .scope(async {
                let before = ChangeContext::current().unwrap();
                ChangeContext::current_context()
                    .unwrap()
                    .update(|m| m.reason = Some("r".into()));
                (before, ChangeContext::current().unwrap())
            })
            .await;
        assert_eq!(seen.0.source, ChangeSource::Cli);
        assert_eq!(seen.0.reason, None);
        assert_eq!(seen.1.reason.as_deref(), Some("r"));
        assert_eq!(ctx.meta().reason.as_deref(), Some("r"));
        assert_eq!(ChangeContext::current(), None);
    }

    #[tokio::test]
    async fn a_reused_id_is_taken_once_for_its_kind() {
        let id = Uuid::now_v7();
        assert_eq!(take_reused_id(Kind::Vm), None);
        let ((other, first, second), used) = reuse_id(Kind::Vm, id, async {
            (
                take_reused_id(Kind::Volume),
                take_reused_id(Kind::Vm),
                take_reused_id(Kind::Vm),
            )
        })
        .await;
        assert_eq!((other, first, second), (None, Some(id), None));
        assert!(used);
        let (_, used) = reuse_id(Kind::Vm, id, async {}).await;
        assert!(!used);
    }
}
