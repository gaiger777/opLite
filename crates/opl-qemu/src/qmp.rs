use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

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
    #[error("QMP job {id} failed: {error}")]
    JobFailed { id: String, error: String },
    #[error("QMP job {id} did not finish within {timeout:?}")]
    JobTimeout { id: String, timeout: Duration },
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
    writer: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next_id: AtomicU64,
    /// 강한 Sender는 리더 태스크만 가진다. 연결이 끊기면 구독자가 `Closed`를 받게 하기 위해서다.
    events: broadcast::WeakSender<QmpEvent>,
    reader: JoinHandle<()>,
    writer_task: JoinHandle<()>,
}

/// QEMU의 QMP 소켓은 대기열(backlog)이 1이다. Linux에서는 대기열이 차 있으면(앞선 클라이언트가
/// 아직 받아들여지지 않았으면, 예: 멈췄다 깨어난 QEMU) 연결이 곧바로 `EAGAIN`으로 실패하므로
/// 잠시 뒤 다시 시도한다. macOS는 이때 기다려 준다.
async fn connect_socket(path: &Path) -> std::io::Result<UnixStream> {
    const RETRY: Duration = Duration::from_millis(50);
    const ATTEMPTS: u32 = 100;
    let mut attempt = 1;
    loop {
        match UnixStream::connect(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && attempt < ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(RETRY).await;
            }
            other => return other,
        }
    }
}

impl QmpClient {
    /// 소켓에 연결하고 greeting을 확인한 뒤 `qmp_capabilities`로 명령 모드에 진입한다.
    pub async fn connect(path: &Path) -> Result<Self, QmpError> {
        let stream = connect_socket(path).await?;
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
        let weak_events = events.downgrade();
        let reader = tokio::spawn(read_loop(lines, pending.clone(), events));
        let (writer, rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(write_loop(write, rx, pending.clone()));
        let client = Self {
            writer,
            pending,
            next_id: AtomicU64::new(1),
            events: weak_events,
            reader,
            writer_task,
        };
        client.execute("qmp_capabilities", None).await?;
        Ok(client)
    }

    /// 명령을 보내고 응답을 기다린다. 타임아웃 등으로 취소해도 이미 보낸 명령은 나중에
    /// 실행될 수 있으므로, 결과는 알 수 없는 것으로 보고 `query_status`로 다시 확인한다.
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
        // 취소(drop)되거나 실패해도 pending 항목이 남지 않게 한다.
        let _guard = PendingGuard {
            pending: &self.pending,
            id,
        };

        let mut msg = json!({ "execute": command, "id": id });
        if let Some(args) = arguments {
            msg["arguments"] = args;
        }
        let mut line = serde_json::to_vec(&msg).expect("json! value always serializes");
        line.push(b'\n');
        // 줄 전체를 writer 태스크가 끝까지 쓰므로, 호출자가 취소해도 반쪽 JSON이 남지 않는다.
        self.writer.send(line).map_err(|_| QmpError::Disconnected)?;
        rx.await.unwrap_or(Err(QmpError::Disconnected))
    }

    /// 이 시점 이후에 도착하는 이벤트를 받는다.
    ///
    /// 구독 전에 도착한 이벤트는 받을 수 없고, 느린 구독자는 `Lagged`를 받는다.
    /// 재접속 직후나 `Lagged`를 받았을 때는 `query_status`로 상태를 다시 맞춘다.
    /// 연결이 끊기면 `Closed`를 받는다.
    pub fn subscribe(&self) -> broadcast::Receiver<QmpEvent> {
        match self.events.upgrade() {
            Some(s) => s.subscribe(),
            None => broadcast::channel(1).1,
        }
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

    /// 내부 스냅샷 `tag`를 `devices`(블록 node-name)에 만들고 메모리 상태를 `vmstate`에 담는다.
    /// 저장하는 동안 VM이 멈췄다가 원래 상태로 돌아온다.
    pub async fn snapshot_save(
        &self,
        tag: &str,
        vmstate: &str,
        devices: &[String],
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let args = json!({ "tag": tag, "vmstate": vmstate, "devices": devices });
        self.run_job("snapshot-save", args, timeout).await
    }

    /// 내부 스냅샷 `tag`로 디스크와 메모리 상태를 되돌린다. `-S`로 띄운 뒤 `cont` 전에 부를 수 있다.
    pub async fn snapshot_load(
        &self,
        tag: &str,
        vmstate: &str,
        devices: &[String],
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let args = json!({ "tag": tag, "vmstate": vmstate, "devices": devices });
        self.run_job("snapshot-load", args, timeout).await
    }

    /// `devices`에서 내부 스냅샷 `tag`를 지운다.
    pub async fn snapshot_delete(
        &self,
        tag: &str,
        devices: &[String],
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let args = json!({ "tag": tag, "devices": devices });
        self.run_job("snapshot-delete", args, timeout).await
    }

    /// 메모리 없이 `devices` 모두에 내부 스냅샷 `tag`를 한 번에 만든다. VM은 멈추지 않는다.
    pub async fn snapshot_disks(&self, tag: &str, devices: &[String]) -> Result<(), QmpError> {
        let actions: Vec<Value> = devices
            .iter()
            .map(|d| {
                json!({
                    "type": "blockdev-snapshot-internal-sync",
                    "data": { "device": d, "name": tag },
                })
            })
            .collect();
        self.execute("transaction", Some(json!({ "actions": actions })))
            .await
            .map(drop)
    }

    /// 블록 노드 `node`의 크기를 `size` 바이트로 바꾼다. 게스트가 바로 본다.
    pub async fn block_resize(&self, node: &str, size: u64) -> Result<(), QmpError> {
        let args = json!({ "node-name": node, "size": size });
        self.execute("block_resize", Some(args)).await.map(drop)
    }

    /// 작업(job) 명령을 새 `job-id`로 시작하고 `concluded`까지 기다린 뒤, 오류를 읽고 `job-dismiss`한다.
    ///
    /// 제한 시간이 지나면 작업은 QEMU에 남겨 두고 `JobTimeout`을 돌려준다.
    async fn run_job(
        &self,
        command: &str,
        mut arguments: Value,
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let id = format!("opl-{}", Uuid::new_v4());
        arguments["job-id"] = json!(id);
        // 작업 이벤트를 놓치지 않게 시작하기 전에 구독한다.
        let mut events = self.subscribe();
        self.execute(command, Some(arguments)).await?;

        let wait = async {
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        if ev.event == "JOB_STATUS_CHANGE"
                            && ev.data["id"] == id.as_str()
                            && ev.data["status"] == "concluded"
                        {
                            return Ok(());
                        }
                    }
                    // 이벤트를 놓쳤으면 query-jobs로 상태를 직접 본다.
                    Err(RecvError::Lagged(_)) => {
                        if self.query_job(&id).await?["status"] == "concluded" {
                            return Ok(());
                        }
                    }
                    Err(RecvError::Closed) => return Err(QmpError::Disconnected),
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| QmpError::JobTimeout {
                id: id.clone(),
                timeout,
            })??;

        let error = self.query_job(&id).await?["error"]
            .as_str()
            .map(str::to_string);
        self.execute("job-dismiss", Some(json!({ "id": id })))
            .await?;
        match error {
            Some(error) => Err(QmpError::JobFailed { id, error }),
            None => Ok(()),
        }
    }

    /// `query-jobs`에서 작업 `id`를 찾는다.
    async fn query_job(&self, id: &str) -> Result<Value, QmpError> {
        let jobs = self.execute("query-jobs", None).await?;
        jobs.as_array()
            .and_then(|jobs| jobs.iter().find(|j| j["id"] == id))
            .cloned()
            .ok_or_else(|| QmpError::Protocol(format!("job {id} not in query-jobs: {jobs}")))
    }
}

impl Drop for QmpClient {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer_task.abort();
    }
}

/// drop되면 `pending`에서 id를 제거한다. 응답이 이미 처리됐다면 아무 일도 하지 않는다.
struct PendingGuard<'a> {
    pending: &'a Pending,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if let Some(map) = self.pending.lock().unwrap().as_mut() {
            map.remove(&self.id);
        }
    }
}

/// 연결이 끊겼음을 알리고 대기 중인 모든 호출을 `Disconnected`로 끝낸다.
fn fail_all(pending: &Pending) {
    if let Some(map) = pending.lock().unwrap().take() {
        for (_, tx) in map {
            let _ = tx.send(Err(QmpError::Disconnected));
        }
    }
}

async fn write_loop(
    mut write: OwnedWriteHalf,
    mut lines: mpsc::UnboundedReceiver<Vec<u8>>,
    pending: Pending,
) {
    while let Some(line) = lines.recv().await {
        if write.write_all(&line).await.is_err() {
            fail_all(&pending);
            return;
        }
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
    fail_all(&pending);
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

    use std::time::Duration;

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

    /// 작업을 시작하는 명령 하나를 받아 빈 응답을 주고, 인자(`job-id` 포함)를 돌려준다.
    async fn accept_job(fake: &mut FakeQemu, expect: &str) -> Value {
        let cmd = fake.recv().await;
        assert_eq!(cmd["execute"], expect);
        fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
        cmd["arguments"].clone()
    }

    async fn job_status(fake: &mut FakeQemu, id: &Value, status: &str) {
        fake.send(json!({
            "event": "JOB_STATUS_CHANGE",
            "data": { "id": id, "status": status },
            "timestamp": {}
        }))
        .await;
    }

    /// 실제 QEMU처럼 created부터 concluded까지 이벤트를 보내고, `query-jobs`(실패면 `error`)와
    /// `job-dismiss`에 답한다.
    async fn run_job(fake: &mut FakeQemu, id: &Value, error: Option<&str>) {
        for status in ["created", "running", "waiting", "pending", "concluded"] {
            job_status(fake, id, status).await;
        }
        answer_jobs(fake, id, error).await;
    }

    async fn answer_jobs(fake: &mut FakeQemu, id: &Value, error: Option<&str>) {
        let mut job = json!({
            "id": id, "type": "snapshot-save", "status": "concluded",
            "current-progress": 1, "total-progress": 1
        });
        if let Some(e) = error {
            job["error"] = json!(e);
        }
        fake.answer("query-jobs", json!({ "return": [job] })).await;
        let cmd = fake.recv().await;
        assert_eq!(cmd["execute"], "job-dismiss");
        assert_eq!(&cmd["arguments"]["id"], id);
        fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
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

    #[tokio::test]
    async fn cancelled_command_does_not_corrupt_the_stream() {
        let (client, mut fake, _dir) = setup().await;
        // fake가 읽지 않으므로 소켓 버퍼가 차서 쓰기가 막힌 채 타임아웃으로 취소된다.
        let big = json!({ "pad": "x".repeat(8 << 20) });
        let res =
            tokio::time::timeout(Duration::from_millis(50), client.execute("x", Some(big))).await;
        assert!(res.is_err(), "execute should have been cancelled");

        let test = async {
            let (status, ()) = tokio::join!(client.query_status(), async {
                let first = fake.recv().await;
                assert_eq!(first["execute"], "x");
                fake.answer(
                    "query-status",
                    json!({ "return": { "running": true, "status": "running" } }),
                )
                .await;
            });
            assert_eq!(status.unwrap(), RunState::Running);
        };
        tokio::time::timeout(Duration::from_secs(10), test)
            .await
            .expect("stream was corrupted by the cancelled command");
        assert_eq!(client.pending.lock().unwrap().as_ref().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn subscribers_see_closed_after_disconnect() {
        let (client, fake, _dir) = setup().await;
        let mut rx = client.subscribe();
        drop(fake);
        let res = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("subscriber was never told the connection closed");
        assert!(matches!(res, Err(RecvError::Closed)));
        let mut late = client.subscribe();
        assert!(matches!(late.recv().await, Err(RecvError::Closed)));
    }

    #[tokio::test]
    async fn concurrent_commands_are_matched_by_id() {
        let (client, mut fake, _dir) = setup().await;
        let mut rx = client.subscribe();
        let (status, name, ()) = tokio::join!(
            client.query_status(),
            client.execute("query-name", None),
            async {
                let a = fake.recv().await;
                let b = fake.recv().await;
                let reply = |cmd: &Value| match cmd["execute"].as_str().unwrap() {
                    "query-status" => {
                        json!({ "return": { "running": true, "status": "running" }, "id": cmd["id"] })
                    }
                    "query-name" => json!({ "return": { "name": "vm1" }, "id": cmd["id"] }),
                    other => panic!("unexpected command {other}"),
                };
                fake.send(reply(&b)).await;
                fake.send(json!({ "event": "RESUME", "data": {}, "timestamp": {} }))
                    .await;
                fake.send(reply(&a)).await;
            }
        );
        assert_eq!(status.unwrap(), RunState::Running);
        assert_eq!(name.unwrap()["name"], "vm1");
        assert_eq!(rx.recv().await.unwrap().event, "RESUME");
    }

    const T: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn snapshot_save_waits_for_job_and_dismisses_it() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string(), "disk1".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_save("s1", "disk0", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-save").await;
            assert_eq!(args["tag"], "s1");
            assert_eq!(args["vmstate"], "disk0");
            assert_eq!(args["devices"], json!(["disk0", "disk1"]));
            let id = args["job-id"].clone();
            assert!(id.as_str().is_some_and(|s| !s.is_empty()));
            // 다른 작업의 이벤트는 무시해야 한다.
            job_status(&mut fake, &json!("other"), "concluded").await;
            run_job(&mut fake, &id, None).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn snapshot_load_and_delete_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_load("s1", "disk0", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-load").await;
            assert_eq!(args["tag"], "s1");
            assert_eq!(args["vmstate"], "disk0");
            assert_eq!(args["devices"], json!(["disk0"]));
            run_job(&mut fake, &args["job-id"], None).await;
        });
        res.unwrap();

        let (res, ()) = tokio::join!(client.snapshot_delete("s1", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-delete").await;
            assert_eq!(args["tag"], "s1");
            assert_eq!(args["devices"], json!(["disk0"]));
            assert!(args.get("vmstate").is_none());
            run_job(&mut fake, &args["job-id"], None).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn failed_job_reports_error_and_is_still_dismissed() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_load("nope", "disk0", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-load").await;
            run_job(
                &mut fake,
                &args["job-id"],
                Some("Snapshot 'nope' does not exist"),
            )
            .await;
        });
        match res.unwrap_err() {
            QmpError::JobFailed { error, .. } => {
                assert_eq!(error, "Snapshot 'nope' does not exist");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn job_start_error_is_returned_without_waiting() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.snapshot_delete("s1", &[], T), async {
            fake.answer(
                "snapshot-delete",
                json!({ "error": { "class": "GenericError", "desc": "bad" } }),
            )
            .await;
        });
        assert!(matches!(res, Err(QmpError::Command { .. })));
    }

    #[tokio::test]
    async fn job_times_out_when_it_never_concludes() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, _fake) = tokio::join!(
            client.snapshot_save("s1", "disk0", &devices, Duration::from_millis(200)),
            async {
                let args = accept_job(&mut fake, "snapshot-save").await;
                job_status(&mut fake, &args["job-id"], "running").await;
                fake
            }
        );
        assert!(matches!(res, Err(QmpError::JobTimeout { .. })));
    }

    #[tokio::test]
    async fn job_wait_fails_when_connection_closes() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_save("s1", "disk0", &devices, T), async {
            accept_job(&mut fake, "snapshot-save").await;
            drop(fake);
        });
        assert!(matches!(res, Err(QmpError::Disconnected)));
    }

    #[tokio::test]
    async fn lagged_job_wait_falls_back_to_query_jobs() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_save("s1", "disk0", &devices, T), async {
            let cmd = fake.recv().await;
            let id = cmd["arguments"]["job-id"].clone();
            // 응답보다 먼저 이벤트를 넘치게 보내 구독자가 `Lagged`를 받게 한다.
            // concluded 이벤트도 이 사이에 묻힌다.
            for _ in 0..100 {
                job_status(&mut fake, &json!("other"), "running").await;
            }
            job_status(&mut fake, &id, "concluded").await;
            for _ in 0..100 {
                job_status(&mut fake, &json!("other"), "running").await;
            }
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            // 놓친 것을 알고 상태를 묻는다. 그다음은 보통처럼 오류 확인과 dismiss.
            fake.answer(
                "query-jobs",
                json!({ "return": [{ "id": id, "type": "snapshot-save", "status": "concluded" }] }),
            )
            .await;
            answer_jobs(&mut fake, &id, None).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn snapshot_disks_uses_one_transaction() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string(), "disk1".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_disks("s1", &devices), async {
            let cmd = fake.recv().await;
            assert_eq!(cmd["execute"], "transaction");
            assert_eq!(
                cmd["arguments"]["actions"],
                json!([
                    { "type": "blockdev-snapshot-internal-sync", "data": { "device": "disk0", "name": "s1" } },
                    { "type": "blockdev-snapshot-internal-sync", "data": { "device": "disk1", "name": "s1" } }
                ])
            );
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn block_resize_sends_node_and_size() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.block_resize("disk1", 2 << 30), async {
            let cmd = fake.recv().await;
            assert_eq!(cmd["execute"], "block_resize");
            assert_eq!(
                cmd["arguments"],
                json!({ "node-name": "disk1", "size": 2u64 << 30 })
            );
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
        });
        res.unwrap();
    }
}
