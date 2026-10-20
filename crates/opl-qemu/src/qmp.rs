use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

#[derive(Debug, Error)]
pub enum QmpError {
    #[error("QMP i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid QMP message: {0}")]
    Protocol(String),
    #[error("QMP command failed: {class}: {desc}")]
    Command { class: String, desc: String },
    #[error("QMP connection closed")]
    Disconnected,
}

/// QEMU가 보낸 비동기 이벤트 (SHUTDOWN, STOP, RESUME 등)
#[derive(Debug, Clone, PartialEq)]
pub struct QmpEvent {
    pub event: String,
    pub data: Value,
}

/// `query-status`의 `status` 값
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    /// `-S`로 시작해 아직 `cont`를 받지 않은 상태
    Prelaunch,
    Running,
    Paused,
    Shutdown,
    Other(String),
}

impl RunState {
    fn parse(s: &str) -> Self {
        match s {
            "prelaunch" => RunState::Prelaunch,
            "running" => RunState::Running,
            "paused" => RunState::Paused,
            "shutdown" => RunState::Shutdown,
            other => RunState::Other(other.to_string()),
        }
    }
}

type Reply = Result<Value, QmpError>;
/// None이면 연결이 끊긴 상태다. 이후의 명령은 바로 Disconnected로 실패한다.
type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Reply>>>>>;

/// QMP 유닉스 소켓 클라이언트. 명령은 동시에 여러 개 보낼 수 있고, 응답은 `id`로 짝을 맞춘다.
pub struct QmpClient {
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    pending: Pending,
    next_id: AtomicU64,
    events: broadcast::Sender<QmpEvent>,
    reader: JoinHandle<()>,
}

impl QmpClient {
    /// 소켓에 연결하고 greeting을 확인한 뒤 `qmp_capabilities`로 명령 모드에 진입한다.
    pub async fn connect(path: &Path) -> Result<Self, QmpError> {
        let stream = UnixStream::connect(path).await?;
        let (read, write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        let greeting = lines.next_line().await?.ok_or(QmpError::Disconnected)?;
        let greeting: Value = serde_json::from_str(&greeting)
            .map_err(|e| QmpError::Protocol(format!("greeting is not JSON: {e}")))?;
        if greeting.get("QMP").is_none() {
            return Err(QmpError::Protocol(format!(
                "unexpected greeting: {greeting}"
            )));
        }

        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (events, _) = broadcast::channel(64);
        let reader = tokio::spawn(read_loop(lines, pending.clone(), events.clone()));
        let client = Self {
            writer: tokio::sync::Mutex::new(write),
            pending,
            next_id: AtomicU64::new(1),
            events,
            reader,
        };
        client.execute("qmp_capabilities", None).await?;
        Ok(client)
    }

    pub async fn execute(
        &self,
        command: &str,
        arguments: Option<Value>,
    ) -> Result<Value, QmpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        match self.pending.lock().unwrap().as_mut() {
            Some(map) => {
                map.insert(id, tx);
            }
            None => return Err(QmpError::Disconnected),
        }

        let mut msg = json!({ "execute": command, "id": id });
        if let Some(args) = arguments {
            msg["arguments"] = args;
        }
        let mut line = serde_json::to_vec(&msg).expect("json! value always serializes");
        line.push(b'\n');
        if let Err(e) = self.writer.lock().await.write_all(&line).await {
            if let Some(map) = self.pending.lock().unwrap().as_mut() {
                map.remove(&id);
            }
            return Err(e.into());
        }
        rx.await.unwrap_or(Err(QmpError::Disconnected))
    }

    /// 이 시점 이후에 도착하는 이벤트를 받는다.
    pub fn subscribe(&self) -> broadcast::Receiver<QmpEvent> {
        self.events.subscribe()
    }

    pub async fn query_status(&self) -> Result<RunState, QmpError> {
        let ret = self.execute("query-status", None).await?;
        let status = ret
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| QmpError::Protocol(format!("query-status without status: {ret}")))?;
        Ok(RunState::parse(status))
    }

    pub async fn cont(&self) -> Result<(), QmpError> {
        self.execute("cont", None).await.map(drop)
    }

    /// 게스트에 ACPI 전원 버튼 이벤트를 보낸다. 게스트가 응답하지 않을 수 있다.
    pub async fn system_powerdown(&self) -> Result<(), QmpError> {
        self.execute("system_powerdown", None).await.map(drop)
    }

    /// QEMU를 즉시 종료한다. 응답 전에 연결이 끊겨도 성공으로 본다.
    pub async fn quit(&self) -> Result<(), QmpError> {
        match self.execute("quit", None).await {
            Ok(_) | Err(QmpError::Disconnected) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for QmpClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_loop(
    mut lines: Lines<BufReader<OwnedReadHalf>>,
    pending: Pending,
    events: broadcast::Sender<QmpEvent>,
) {
    while let Ok(Some(line)) = lines.next_line().await {
        dispatch(&line, &pending, &events);
    }
    if let Some(map) = pending.lock().unwrap().take() {
        for (_, tx) in map {
            let _ = tx.send(Err(QmpError::Disconnected));
        }
    }
}

fn dispatch(line: &str, pending: &Pending, events: &broadcast::Sender<QmpEvent>) {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if let Some(event) = msg.get("event").and_then(Value::as_str) {
        let data = msg.get("data").cloned().unwrap_or(Value::Null);
        let _ = events.send(QmpEvent {
            event: event.to_string(),
            data,
        });
        return;
    }
    let Some(id) = msg.get("id").and_then(Value::as_u64) else {
        return;
    };
    let Some(tx) = pending.lock().unwrap().as_mut().and_then(|m| m.remove(&id)) else {
        return;
    };
    let reply = if let Some(ret) = msg.get("return") {
        Ok(ret.clone())
    } else if let Some(err) = msg.get("error") {
        let field = |k: &str| {
            err.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Err(QmpError::Command {
            class: field("class"),
            desc: field("desc"),
        })
    } else {
        Err(QmpError::Protocol(format!(
            "reply without return or error: {msg}"
        )))
    };
    let _ = tx.send(reply);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::net::UnixListener;

    use super::*;

    const GREETING: &str = r#"{"QMP":{"version":{"qemu":{"major":10,"minor":1,"micro":0},"package":""},"capabilities":[]}}"#;

    /// 테스트용 가짜 QEMU 쪽 연결
    struct FakeQemu {
        lines: Lines<BufReader<OwnedReadHalf>>,
        write: OwnedWriteHalf,
    }

    impl FakeQemu {
        async fn send(&mut self, v: Value) {
            let mut line = v.to_string().into_bytes();
            line.push(b'\n');
            self.write.write_all(&line).await.unwrap();
        }

        async fn recv(&mut self) -> Value {
            let line = self.lines.next_line().await.unwrap().unwrap();
            serde_json::from_str(&line).unwrap()
        }

        /// 명령 하나를 받아 이름을 확인하고 `reply`를 `id`와 함께 돌려준다.
        async fn answer(&mut self, expect: &str, reply: Value) {
            let cmd = self.recv().await;
            assert_eq!(cmd["execute"], expect);
            let mut reply = reply;
            reply["id"] = cmd["id"].clone();
            self.send(reply).await;
        }
    }

    /// 리스너를 열고, 클라이언트 연결을 받아 greeting과 capabilities 협상까지 처리한다.
    async fn setup() -> (QmpClient, FakeQemu, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("qmp.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, write) = stream.into_split();
            let mut fake = FakeQemu {
                lines: BufReader::new(read).lines(),
                write,
            };
            fake.write
                .write_all(format!("{GREETING}\n").as_bytes())
                .await
                .unwrap();
            fake.answer("qmp_capabilities", json!({ "return": {} }))
                .await;
            fake
        });
        let client = QmpClient::connect(&path).await.unwrap();
        (client, server.await.unwrap(), dir)
    }

    #[tokio::test]
    async fn negotiates_and_queries_status() {
        let (client, mut fake, _dir) = setup().await;
        let (status, ()) = tokio::join!(client.query_status(), async {
            fake.answer(
                "query-status",
                json!({ "return": { "running": false, "status": "prelaunch" } }),
            )
            .await;
        });
        assert_eq!(status.unwrap(), RunState::Prelaunch);
    }

    #[tokio::test]
    async fn command_errors_carry_class_and_desc() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.cont(), async {
            fake.answer(
                "cont",
                json!({ "error": { "class": "GenericError", "desc": "boom" } }),
            )
            .await;
        });
        match res.unwrap_err() {
            QmpError::Command { class, desc } => {
                assert_eq!(class, "GenericError");
                assert_eq!(desc, "boom");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_are_broadcast_to_subscribers() {
        let (client, mut fake, _dir) = setup().await;
        let mut rx = client.subscribe();
        fake.send(json!({ "event": "SHUTDOWN", "data": { "guest": true }, "timestamp": {} }))
            .await;
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.event, "SHUTDOWN");
        assert_eq!(ev.data["guest"], true);
    }

    #[tokio::test]
    async fn pending_and_later_commands_fail_after_disconnect() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.query_status(), async {
            let _ = fake.recv().await;
            drop(fake);
        });
        assert!(matches!(res, Err(QmpError::Disconnected)));
        assert!(matches!(client.cont().await, Err(QmpError::Disconnected)));
    }

    #[tokio::test]
    async fn quit_succeeds_when_qemu_closes_without_reply() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.quit(), async {
            let _ = fake.recv().await;
            drop(fake);
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn rejects_non_qmp_greeting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qmp.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"{\"hello\":1}\n").await.unwrap();
            // 클라이언트가 결과를 읽을 때까지 연결을 유지한다.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });
        assert!(matches!(
            QmpClient::connect(&path).await,
            Err(QmpError::Protocol(_))
        ));
    }
}
