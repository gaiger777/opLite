//! 실행기 셋(설계 §3.1). API·작업이 바빠도 Raft 하트비트와 HA 리스 갱신이 늦지 않게 서로 다른
//! 작업자 스레드에서 돌린다.
//!
//! - `core`(2개, `orbivisor-core`): Raft, 노드 간 서버, `HaGuard`, `HaManager`, 노드 감시
//! - `api`(`orbivisor-api`): HTTPS API
//! - `work`(`orbivisor-work`): reconciler, Task 실행기, 스케줄러, 정리, 지표, 풀, 텔레메트리 등
//!
//! 실행기 사이 호출은 `Arc`로 나눈 객체의 메서드이고, tokio의 채널·`Notify`·`watch`는 실행기를
//! 넘어 동작한다.

use std::future::Future;

use tokio::runtime::{Builder, Handle, Runtime};

/// `core` 실행기의 작업자 스레드 수(고정)
pub const CORE_THREADS: usize = 2;

pub struct Runtimes {
    pub core: Runtime,
    pub api: Runtime,
    pub work: Runtime,
}

/// 실행기 셋의 핸들. 서비스를 띄울 때 쓴다.
#[derive(Clone)]
pub struct Handles {
    pub core: Handle,
    pub api: Handle,
    pub work: Handle,
}

impl Runtimes {
    pub fn new(api_threads: usize, work_threads: usize) -> std::io::Result<Self> {
        let build = |name: &str, threads: usize| {
            Builder::new_multi_thread()
                .worker_threads(threads)
                .thread_name(name)
                .enable_all()
                .build()
        };
        Ok(Self {
            core: build("orbivisor-core", CORE_THREADS)?,
            api: build("orbivisor-api", api_threads)?,
            work: build("orbivisor-work", work_threads)?,
        })
    }

    pub fn handles(&self) -> Handles {
        Handles {
            core: self.core.handle().clone(),
            api: self.api.handle().clone(),
            work: self.work.handle().clone(),
        }
    }
}

/// `handle` 실행기의 문맥에서 `f`를 부른다. 안에서 `tokio::spawn`한 작업은 그 실행기에서 돈다.
pub fn on<T>(handle: &Handle, f: impl FnOnce() -> T) -> T {
    let _guard = handle.enter();
    f()
}

/// `fut`을 `handle` 실행기에서 돌리고 끝나기를 기다린다. 안에서 띄운 작업도 그 실행기에서 돈다.
/// `fut`이 panic하면 그대로 다시 던진다. 대상 실행기가 끝나 작업이 취소되어도 panic하므로, 대상
/// 실행기가 부른 쪽보다 먼저 끝날 수 있는 곳에서는 쓰지 않는다.
pub async fn run_on<F>(handle: &Handle, fut: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match handle.spawn(fut).await {
        Ok(output) => output,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_name() -> String {
        std::thread::current()
            .name()
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn each_runtime_has_named_threads_of_its_size() {
        let rt = Runtimes::new(3, 4).unwrap();
        let h = rt.handles();
        for (handle, name, threads) in [
            (&h.core, "orbivisor-core", 2),
            (&h.api, "orbivisor-api", 3),
            (&h.work, "orbivisor-work", 4),
        ] {
            assert_eq!(handle.metrics().num_workers(), threads, "{name}");
            let ran = rt.core.block_on(run_on(handle, async { thread_name() }));
            assert_eq!(ran, name);
        }
    }

    /// `core`에서 도는 코드가 `on`·`run_on`으로 띄운 작업은 고른 실행기에서 돈다.
    #[test]
    fn on_and_run_on_spawn_into_the_chosen_runtime() {
        let rt = Runtimes::new(2, 2).unwrap();
        let h = rt.handles();
        rt.core.block_on(async {
            let here = tokio::spawn(async { thread_name() }).await.unwrap();
            assert_eq!(here, "orbivisor-core");
            let api = on(&h.api, || tokio::spawn(async { thread_name() }));
            assert_eq!(api.await.unwrap(), "orbivisor-api");
            // run_on 안에서 다시 띄운 작업도 그 실행기다.
            let work = run_on(&h.work, async {
                tokio::spawn(async { thread_name() }).await.unwrap()
            });
            assert_eq!(work.await, "orbivisor-work");
        });
    }
}
