//! redb 쓰기 전용 스레드(`orbivisor-store`, 설계 §3.2). 쓰기 트랜잭션의 커밋(fsync)이 느려도
//! 비동기 실행기의 작업자 스레드를 막지 않는다. 쓰기는 큐 하나에서 넣은 차례대로 돈다.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::mpsc;

use redb::Database;
use tokio::sync::oneshot;

use crate::store::StoreError;

type Job = Box<dyn FnOnce(&Database) + Send>;

/// 저장소 스레드에 쓰기를 넣는 손잡이. 모든 손잡이가 사라지면 남은 쓰기를 마치고 스레드가 끝난다.
/// 스레드는 데이터베이스를 약하게만 쥔다. 저장소를 모두 놓으면 곧바로 닫혀 같은 파일을 다시 열 수
/// 있고, 그 뒤에 남은 쓰기는 버린다.
///
/// 쓰기가 panic하면 오류 로그를 남기고 스레드를 멈춘다. 커밋하지 않은 트랜잭션은 버려지지만 메모리의
/// Raft 상태(적용한 위치, 기다리는 콜백)는 디스크와 어긋났을 수 있으므로 계속 쓰지 않는다. 그 뒤의
/// 쓰기는 `WriterStopped`다. openraft는 저장소 오류를 받으면 이 노드의 Raft를 멈추고
/// ([`Cluster::stopped`](crate::Cluster::stopped)가 끝남), opld는 그것을 보고 0이 아닌 코드로 끝나
/// systemd가 다시 띄운다.
#[derive(Clone)]
pub(crate) struct StoreWriter {
    tx: mpsc::Sender<Job>,
}

impl StoreWriter {
    pub(crate) fn spawn(db: &Arc<Database>) -> Result<Self, StoreError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let db = Arc::downgrade(db);
        std::thread::Builder::new()
            .name("orbivisor-store".into())
            .spawn(move || {
                for job in rx {
                    let Some(db) = db.upgrade() else {
                        continue;
                    };
                    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| job(&db))) {
                        let message = panic
                            .downcast_ref::<&str>()
                            .map(|m| m.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_default();
                        tracing::error!(panic = %message, "a store write panicked; stopping the store thread");
                        break;
                    }
                }
            })
            .map_err(StoreError::Thread)?;
        Ok(Self { tx })
    }

    /// 쓰기를 큐에 넣고 기다리지 않는다.
    pub(crate) fn submit(
        &self,
        job: impl FnOnce(&Database) + Send + 'static,
    ) -> Result<(), StoreError> {
        self.tx
            .send(Box::new(job))
            .map_err(|_| StoreError::WriterStopped)
    }

    /// 쓰기를 큐에 넣고 저장소 스레드가 마칠 때까지 기다린다. 쓰기가 panic하면 `WriterPanicked`다.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Database) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let (tx, rx) = oneshot::channel();
        self.submit(move |db| match catch_unwind(AssertUnwindSafe(|| f(db))) {
            Ok(result) => {
                let _ = tx.send(result);
            }
            Err(panic) => {
                let _ = tx.send(Err(StoreError::WriterPanicked));
                // 스레드가 로그를 남기고 멈춘다.
                resume_unwind(panic);
            }
        })?;
        rx.await.map_err(|_| StoreError::WriterStopped)?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;

    fn writer() -> (tempfile::TempDir, Arc<Database>, StoreWriter) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::create(dir.path().join("db")).unwrap());
        let writer = StoreWriter::spawn(&db).unwrap();
        (dir, db, writer)
    }

    /// 쓰기는 이름 붙은 스레드 하나에서 넣은 차례대로 돈다.
    #[tokio::test]
    async fn writes_run_in_order_on_the_store_thread() {
        let (_dir, _db, writer) = writer();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..20 {
            let seen = seen.clone();
            writer
                .submit(move |_| {
                    let name = std::thread::current().name().map(str::to_owned);
                    seen.lock().unwrap().push((i, name));
                })
                .unwrap();
        }
        let last = writer.run(|_| Ok(20)).await.unwrap();
        assert_eq!(last, 20);
        let seen = seen.lock().unwrap();
        assert_eq!(
            *seen,
            (0..20)
                .map(|i| (i, Some("orbivisor-store".to_owned())))
                .collect::<Vec<_>>()
        );
    }

    /// 쓰기가 panic하면 그 쓰기는 `WriterPanicked`이고, 스레드가 멈춰 뒤의 쓰기는 `WriterStopped`다.
    #[tokio::test]
    async fn a_panicking_write_stops_the_store_thread() {
        let (_dir, _db, writer) = writer();
        let err = writer
            .run(|_| -> Result<(), StoreError> { panic!("write bug") })
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::WriterPanicked), "{err}");
        let err = writer.run(|_| Ok(())).await.unwrap_err();
        assert!(matches!(err, StoreError::WriterStopped), "{err}");
    }

    /// 느린 쓰기를 기다리는 동안에도 (작업자 스레드 하나인) 실행기의 다른 작업은 돈다.
    #[tokio::test]
    async fn a_slow_write_does_not_block_the_runtime() {
        let (_dir, _db, writer) = writer();
        let slow = tokio::spawn({
            let writer = writer.clone();
            async move {
                writer
                    .run(|_| {
                        std::thread::sleep(Duration::from_millis(500));
                        Ok(())
                    })
                    .await
            }
        });
        let start = Instant::now();
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(start.elapsed() < Duration::from_millis(400));
        assert!(!slow.is_finished());
        slow.await.unwrap().unwrap();
    }
}
