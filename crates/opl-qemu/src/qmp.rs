use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    #[error("QMP job {id} did not finish within {timeout:?} ({stage})")]
    JobTimeout {
        id: String,
        timeout: Duration,
        /// 어디서 멈췄는지와 그때의 `query-jobs` 결과
        stage: String,
    },
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
    /// `-incoming`로 떠서 마이그레이션을 받기 전이거나 받는 중
    InMigrate,
    /// 마이그레이션을 보내 끝낸 원본. `cont`하면 다시 돈다.
    PostMigrate,
    Other(String),
}

impl RunState {
    fn parse(s: &str) -> Self {
        match s {
            "prelaunch" => RunState::Prelaunch,
            "running" => RunState::Running,
            "paused" => RunState::Paused,
            "shutdown" => RunState::Shutdown,
            "inmigrate" => RunState::InMigrate,
            "postmigrate" => RunState::PostMigrate,
            other => RunState::Other(other.to_string()),
        }
    }
}

/// `query-migrate`의 `status` 값
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationState {
    /// 마이그레이션을 한 적이 없다(`status`가 없다).
    None,
    Setup,
    Active,
    Completed,
    Failed,
    Cancelling,
    Cancelled,
    /// 능력 `pause-before-switchover`로 보낸 마이그레이션이 게스트를 멈추고 블록 장치를 놓기 전에
    /// 기다리는 상태. `migrate-continue`로 이어 간다.
    PreSwitchover,
    Other(String),
}

impl MigrationState {
    fn parse(s: Option<&str>) -> Self {
        match s {
            None | Some("none") => MigrationState::None,
            Some("setup") => MigrationState::Setup,
            Some("active") => MigrationState::Active,
            Some("completed") => MigrationState::Completed,
            Some("failed") => MigrationState::Failed,
            Some("cancelling") => MigrationState::Cancelling,
            Some("cancelled") => MigrationState::Cancelled,
            Some("pre-switchover") => MigrationState::PreSwitchover,
            Some(other) => MigrationState::Other(other.to_string()),
        }
    }

    /// 끝난 상태(완료·실패·취소)인지
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            MigrationState::Completed | MigrationState::Failed | MigrationState::Cancelled
        )
    }
}

/// `query-migrate`의 결과 중 opl이 보는 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationInfo {
    pub state: MigrationState,
    /// 실패했을 때 QEMU가 남긴 까닭(`error-desc`)
    pub error: Option<String>,
}

impl MigrationInfo {
    fn parse(v: &Value) -> Self {
        Self {
            state: MigrationState::parse(v.get("status").and_then(Value::as_str)),
            error: v
                .get("error-desc")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

/// TLS 자격 증명 객체(`tls-creds-x509`)를 쓰는 쪽
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsEndpoint {
    /// 받는 쪽. 상대(보내는 쪽)의 인증서도 확인한다(`verify-peer`).
    Server,
    /// 보내는 쪽
    Client,
}

/// `migrate-set-parameters`로 바꿀 값. `None`인 것은 보내지 않는다(QEMU의 지금 값 그대로).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationParameters {
    /// TLS 자격 증명 객체 id
    pub tls_creds: Option<String>,
    /// 보내는 쪽이 받는 쪽 인증서에서 확인할 이름(IP SAN이나 DNS SAN)
    pub tls_hostname: Option<String>,
    /// 바이트/초
    pub max_bandwidth: Option<u64>,
}

/// `query-named-block-nodes`의 블록 노드 하나 중 opl이 보는 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockNode {
    pub node_name: String,
    /// 블록 드라이버(`qcow2`, `raw`, `file` 등)
    pub driver: String,
    pub bitmaps: Vec<BitmapInfo>,
}

/// 블록 노드의 dirty bitmap
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitmapInfo {
    pub name: String,
    /// 쓰기를 따라가고 있는지
    pub recording: bool,
    /// 저장된 비트맵을 QEMU가 비정상으로 끝난 뒤 열어 믿을 수 없다(쓸 수 없다).
    pub inconsistent: bool,
    /// 이미지(qcow2)에 저장돼 QEMU를 다시 켜도 남는지
    pub persistent: bool,
    /// 비트맵이 바뀐 것으로 적은 바이트(QEMU의 `count`). 없으면 0
    pub count: u64,
}

impl BlockNode {
    fn parse(v: &Value) -> Result<Self, QmpError> {
        let text = |k: &str| {
            v[k].as_str()
                .map(str::to_string)
                .ok_or_else(|| QmpError::Protocol(format!("block node without {k}: {v}")))
        };
        let flag = |b: &Value, k: &str| b[k].as_bool().unwrap_or(false);
        // 이름 없는 비트맵은 블록 작업이 쓰는 것(미러가 바뀐 블록을 따라가는 비트맵)이라 건너뛴다.
        let bitmaps = v["dirty-bitmaps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| {
                Some(BitmapInfo {
                    name: b["name"].as_str()?.to_string(),
                    recording: flag(b, "recording"),
                    // QEMU는 inconsistent일 때만 이 항목을 넣는다.
                    inconsistent: flag(b, "inconsistent"),
                    persistent: flag(b, "persistent"),
                    count: b["count"].as_u64().unwrap_or(0),
                })
            })
            .collect();
        Ok(Self {
            node_name: text("node-name")?,
            driver: text("drv")?,
            bitmaps,
        })
    }
}

/// `query-blockstats`의 블록 백엔드 하나와 게스트가 본 I/O 누적 카운터. QEMU를 다시 켜면 0부터다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockStats {
    /// 백엔드 이름(`-drive`의 이름). `-blockdev`로 붙인 디스크는 빈 문자열이다.
    pub device: String,
    /// 붙은 장치의 qdev 경로(`/machine/peripheral/disk0-dev/virtio-backend`). 없으면 None
    pub qdev: Option<String>,
    /// 백엔드에 붙은 맨 위 블록 노드(`disk{i}`. 블록 작업 중에는 필터 노드일 수 있다)
    pub node_name: Option<String>,
    pub rd_bytes: u64,
    pub wr_bytes: u64,
    pub rd_operations: u64,
    pub wr_operations: u64,
}

impl BlockStats {
    fn parse(v: &Value) -> Result<Self, QmpError> {
        let text = |k: &str| v[k].as_str().map(str::to_string);
        let count = |k: &str| {
            v["stats"][k]
                .as_u64()
                .ok_or_else(|| QmpError::Protocol(format!("blockstats without {k}: {v}")))
        };
        Ok(Self {
            device: text("device").unwrap_or_default(),
            qdev: text("qdev"),
            node_name: text("node-name"),
            rd_bytes: count("rd_bytes")?,
            wr_bytes: count("wr_bytes")?,
            rd_operations: count("rd_operations")?,
            wr_operations: count("wr_operations")?,
        })
    }
}

/// `transaction` 안의 동작 하나. 모두 한 시점에 일어난다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionAction {
    /// 메모리 없는 내부 스냅샷(`blockdev-snapshot-internal-sync`)
    SnapshotInternal { device: String, name: String },
    /// 이 시점 뒤에 바뀐 블록을 따라갈 새 dirty bitmap(`block-dirty-bitmap-add`)
    BitmapAdd {
        node: String,
        name: String,
        persistent: bool,
    },
    /// `bitmap`이 가리키는 바뀐 블록만 `target` 노드에 압축해 쓰는 백업 작업(`blockdev-backup`,
    /// `sync: bitmap`). 비트맵은 그대로 두고(`bitmap-mode: never`. `sync: incremental`은 `on-success`만
    /// 받는다), 끝난 작업은 `job-dismiss`할 때까지 남는다.
    IncrementalBackup {
        job_id: String,
        device: String,
        target: String,
        bitmap: String,
    },
    /// 노드 `device`를 노드 `target`(복제본의 NBD 내보내기)에 쓰는 백업 작업(`blockdev-backup`). `sync`가
    /// 비트맵이면 그 비트맵이 가리키는 바뀐 블록만 쓰고 비트맵은 그대로 둔다(`bitmap-mode: never`). NBD
    /// 대상은 압축을 받지 못하므로 압축하지 않는다. 끝난 작업은 `job-dismiss`할 때까지 남는다. `speed`는
    /// 바이트/초(없으면 제한 없음).
    Backup {
        job_id: String,
        device: String,
        target: String,
        sync: BackupSync,
        speed: Option<u64>,
    },
}

/// [`TransactionAction::Backup`]이 쓰는 범위
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupSync {
    /// 디스크 전체
    Full,
    /// 이 비트맵이 가리키는 바뀐 블록만
    Bitmap(String),
}

impl TransactionAction {
    fn to_json(&self) -> Value {
        match self {
            TransactionAction::SnapshotInternal { device, name } => json!({
                "type": "blockdev-snapshot-internal-sync",
                "data": { "device": device, "name": name },
            }),
            TransactionAction::BitmapAdd {
                node,
                name,
                persistent,
            } => json!({
                "type": "block-dirty-bitmap-add",
                "data": { "node": node, "name": name, "persistent": persistent },
            }),
            TransactionAction::IncrementalBackup {
                job_id,
                device,
                target,
                bitmap,
            } => json!({
                "type": "blockdev-backup",
                "data": {
                    "job-id": job_id,
                    "device": device,
                    "target": target,
                    "sync": "bitmap",
                    "bitmap": bitmap,
                    "bitmap-mode": "never",
                    "compress": true,
                    "auto-dismiss": false,
                },
            }),
            TransactionAction::Backup {
                job_id,
                device,
                target,
                sync,
                speed,
            } => {
                let mut data = json!({
                    "job-id": job_id,
                    "device": device,
                    "target": target,
                    "auto-dismiss": false,
                });
                match sync {
                    BackupSync::Full => data["sync"] = json!("full"),
                    BackupSync::Bitmap(bitmap) => {
                        data["sync"] = json!("bitmap");
                        data["bitmap"] = json!(bitmap);
                        data["bitmap-mode"] = json!("never");
                    }
                }
                if let Some(speed) = speed {
                    data["speed"] = json!(speed);
                }
                json!({ "type": "blockdev-backup", "data": data })
            }
        }
    }
}

/// `query-jobs`의 작업 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    pub id: String,
    /// `created`, `running`, `ready`, … `concluded`
    pub status: String,
    /// 실패했으면(취소 포함) 그 까닭
    pub error: Option<String>,
    /// 한 일의 양. 미러는 복사한 바이트이고, 할당되지 않은 영역은 세지 않으며 복사 중 쓰기만큼
    /// `total_progress`와 함께 는다. 없으면 0.
    pub current_progress: u64,
    /// 할 일의 전체 양(늘 수 있다). 없으면 0.
    pub total_progress: u64,
}

impl JobInfo {
    pub fn is_concluded(&self) -> bool {
        self.status == "concluded"
    }

    /// 미러가 한 번 다 복사하고 원본의 쓰기를 따라가는 중인지
    pub fn is_ready(&self) -> bool {
        self.status == "ready"
    }
}

/// `query-hotpluggable-cpus`의 CPU 자리 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotpluggableCpu {
    /// `device_add`에 줄 CPU 장치 이름(`type`, 예: `max-x86_64-cpu`)
    pub kind: String,
    pub vcpus_count: u32,
    pub socket_id: Option<u32>,
    pub core_id: Option<u32>,
    pub thread_id: Option<u32>,
    /// 켜진 자리의 QOM 경로. 빈 자리는 없다.
    pub qom_path: Option<String>,
}

impl HotpluggableCpu {
    fn parse(v: &Value) -> Result<Self, QmpError> {
        let kind = v["type"]
            .as_str()
            .ok_or_else(|| QmpError::Protocol(format!("hotpluggable cpu without type: {v}")))?;
        let prop = |k: &str| v["props"][k].as_u64().and_then(|n| u32::try_from(n).ok());
        Ok(Self {
            kind: kind.to_string(),
            vcpus_count: v["vcpus-count"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(1),
            socket_id: prop("socket-id"),
            core_id: prop("core-id"),
            thread_id: prop("thread-id"),
            qom_path: v["qom-path"].as_str().map(str::to_string),
        })
    }
}

/// NBD 서버의 내보내기에 붙는 블록 노드(`blockdev-add`, `driver: nbd`). 늘 TLS로 붙는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NbdBlockdev {
    pub node_name: String,
    pub host: String,
    pub port: u16,
    /// 내보내기 이름
    pub export: String,
    /// TLS 자격 증명 객체 id(client)
    pub tls_creds: String,
    /// 서버 인증서에서 확인할 이름(IP SAN이나 DNS SAN)
    pub tls_hostname: String,
}

/// 작업이 제한 시간을 넘겼을 때 상태를 묻는 `query-jobs`를 기다리는 시간
const JOB_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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

    /// VM 실행을 멈춘다. 돌아오기 전에 진행 중인 블록 I/O를 비운다. `cont`로 이어서 실행한다.
    pub async fn stop(&self) -> Result<(), QmpError> {
        self.execute("stop", None).await.map(drop)
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

    /// 동작들을 `transaction` 하나로 보낸다. 하나라도 실패하면 아무것도 일어나지 않는다.
    pub async fn transaction(&self, actions: &[TransactionAction]) -> Result<(), QmpError> {
        let actions: Vec<Value> = actions.iter().map(TransactionAction::to_json).collect();
        self.execute("transaction", Some(json!({ "actions": actions })))
            .await
            .map(drop)
    }

    /// 모든 블록 노드와 그 dirty bitmap(`query-named-block-nodes`, backing 노드를 펼치지 않음)
    pub async fn query_block_nodes(&self) -> Result<Vec<BlockNode>, QmpError> {
        let ret = self
            .execute("query-named-block-nodes", Some(json!({ "flat": true })))
            .await?;
        ret.as_array()
            .ok_or_else(|| QmpError::Protocol(format!("query-named-block-nodes: {ret}")))?
            .iter()
            .map(BlockNode::parse)
            .collect()
    }

    /// 블록 백엔드마다의 I/O 카운터(`query-blockstats`, 아래 노드는 펼치지 않음)
    pub async fn query_blockstats(&self) -> Result<Vec<BlockStats>, QmpError> {
        let ret = self.execute("query-blockstats", None).await?;
        ret.as_array()
            .ok_or_else(|| QmpError::Protocol(format!("query-blockstats: {ret}")))?
            .iter()
            .map(BlockStats::parse)
            .collect()
    }

    /// 노드 `node`에 dirty bitmap `name`을 만든다. `persistent`면 이미지(qcow2)에 저장된다.
    pub async fn block_dirty_bitmap_add(
        &self,
        node: &str,
        name: &str,
        persistent: bool,
    ) -> Result<(), QmpError> {
        let args = json!({ "node": node, "name": name, "persistent": persistent });
        self.execute("block-dirty-bitmap-add", Some(args))
            .await
            .map(drop)
    }

    /// 노드 `node`의 dirty bitmap `name`을 지운다(저장된 것이면 이미지에서도).
    pub async fn block_dirty_bitmap_remove(&self, node: &str, name: &str) -> Result<(), QmpError> {
        let args = json!({ "node": node, "name": name });
        self.execute("block-dirty-bitmap-remove", Some(args))
            .await
            .map(drop)
    }

    /// qcow2 파일 `path`를 backing 없이 블록 노드 `node_name`으로 연다(백업 대상).
    pub async fn blockdev_add_qcow2(&self, node_name: &str, path: &Path) -> Result<(), QmpError> {
        let args = json!({
            "driver": "qcow2",
            "node-name": node_name,
            "file": { "driver": "file", "filename": path.display().to_string() },
            "backing": null,
        });
        self.execute("blockdev-add", Some(args)).await.map(drop)
    }

    /// NBD 서버의 내보내기에 TLS로 붙는 블록 노드를 연다(미러 대상).
    pub async fn blockdev_add_nbd(&self, nbd: &NbdBlockdev) -> Result<(), QmpError> {
        let args = json!({
            "driver": "nbd",
            "node-name": nbd.node_name,
            "server": { "type": "inet", "host": nbd.host, "port": nbd.port.to_string() },
            "export": nbd.export,
            "tls-creds": nbd.tls_creds,
            "tls-hostname": nbd.tls_hostname,
        });
        self.execute("blockdev-add", Some(args)).await.map(drop)
    }

    /// 노드 `device`를 노드 `target`으로 복사하는 미러 작업 `job_id`를 시작한다(`sync: full`,
    /// `copy-mode: background`). 다 복사하면 `ready`가 되어 원본의 쓰기를 따라가고,
    /// [`block_job_cancel`](Self::block_job_cancel)로 끝낸 뒤 `job-dismiss`할 때까지 남는다.
    /// `speed`는 바이트/초(없으면 제한 없음).
    pub async fn blockdev_mirror(
        &self,
        job_id: &str,
        device: &str,
        target: &str,
        speed: Option<u64>,
    ) -> Result<(), QmpError> {
        let mut args = json!({
            "job-id": job_id,
            "device": device,
            "target": target,
            "sync": "full",
            "copy-mode": "background",
            "auto-dismiss": false,
        });
        if let Some(speed) = speed {
            args["speed"] = json!(speed);
        }
        self.execute("blockdev-mirror", Some(args)).await.map(drop)
    }

    /// `host:port`에서 TLS(`tls_creds`, server)로만 받는 NBD 서버를 띄운다. QEMU 하나에 하나다.
    pub async fn nbd_server_start(
        &self,
        host: &str,
        port: u16,
        tls_creds: &str,
    ) -> Result<(), QmpError> {
        // addr는 옛 모양(SocketAddressLegacy)이다: 주소가 `data` 안에 들어간다.
        let args = json!({
            "addr": { "type": "inet", "data": { "host": host, "port": port.to_string() } },
            "tls-creds": tls_creds,
        });
        self.execute("nbd-server-start", Some(args)).await.map(drop)
    }

    /// 블록 노드 `node`를 NBD 서버에 이름 `name`으로 쓰기 가능하게 내보낸다(내보내기 id `id`).
    /// `-incoming defer`로 띄워 아직 비활성인 노드도 활성화해 쓰기를 받는다.
    pub async fn block_export_add_nbd(
        &self,
        id: &str,
        node: &str,
        name: &str,
    ) -> Result<(), QmpError> {
        let args = json!({
            "type": "nbd",
            "id": id,
            "node-name": node,
            "name": name,
            "writable": true,
        });
        self.execute("block-export-add", Some(args)).await.map(drop)
    }

    /// 내보내기 `id`를 닫는다. 붙은 NBD 클라이언트가 있으면 끊고 닫는다(`mode: hard`).
    pub async fn block_export_del(&self, id: &str) -> Result<(), QmpError> {
        self.execute(
            "block-export-del",
            Some(json!({ "id": id, "mode": "hard" })),
        )
        .await
        .map(drop)
    }

    /// NBD 서버와 그 내보내기를 모두 멈춘다. 서버가 없으면 오류다.
    pub async fn nbd_server_stop(&self) -> Result<(), QmpError> {
        self.execute("nbd-server-stop", None).await.map(drop)
    }

    /// `blockdev-add`로 연 노드를 닫는다.
    pub async fn blockdev_del(&self, node_name: &str) -> Result<(), QmpError> {
        self.execute("blockdev-del", Some(json!({ "node-name": node_name })))
            .await
            .map(drop)
    }

    pub async fn query_jobs(&self) -> Result<Vec<JobInfo>, QmpError> {
        let ret = self.execute("query-jobs", None).await?;
        let jobs = ret
            .as_array()
            .ok_or_else(|| QmpError::Protocol(format!("query-jobs: {ret}")))?;
        jobs.iter()
            .map(|j| match (j["id"].as_str(), j["status"].as_str()) {
                (Some(id), Some(status)) => Ok(JobInfo {
                    id: id.to_string(),
                    status: status.to_string(),
                    error: j["error"].as_str().map(str::to_string),
                    current_progress: j["current-progress"].as_u64().unwrap_or(0),
                    total_progress: j["total-progress"].as_u64().unwrap_or(0),
                }),
                _ => Err(QmpError::Protocol(format!("job without id or status: {j}"))),
            })
            .collect()
    }

    /// 작업을 취소한다. 곧바로 돌아오고, 작업은 오류와 함께 `concluded`가 된다.
    pub async fn job_cancel(&self, id: &str) -> Result<(), QmpError> {
        self.execute("job-cancel", Some(json!({ "id": id })))
            .await
            .map(drop)
    }

    /// 블록 작업 `id`를 끝낸다. `ready`인 미러에 `force: false`면 이 시점까지 맞춘 뒤 오류 없이
    /// `concluded`가 된다(대상이 원본과 같다). `force: true`면 상태와 상관없이 취소하고 작업은
    /// `Operation canceled` 오류로 끝난다. 곧바로 돌아오므로 끝은 `query-jobs`로 본다.
    pub async fn block_job_cancel(&self, id: &str, force: bool) -> Result<(), QmpError> {
        self.execute(
            "block-job-cancel",
            Some(json!({ "device": id, "force": force })),
        )
        .await
        .map(drop)
    }

    /// `concluded`인 작업을 지운다.
    pub async fn job_dismiss(&self, id: &str) -> Result<(), QmpError> {
        self.execute("job-dismiss", Some(json!({ "id": id })))
            .await
            .map(drop)
    }

    /// 블록 노드 `node`의 크기를 `size` 바이트로 바꾼다. 게스트가 바로 본다.
    pub async fn block_resize(&self, node: &str, size: u64) -> Result<(), QmpError> {
        let args = json!({ "node-name": node, "size": size });
        self.execute("block_resize", Some(args)).await.map(drop)
    }

    /// CPU 자리 목록(`query-hotpluggable-cpus`). 켜진 자리만 `qom_path`가 있다. CPU 핫플러그가 없는 머신
    /// (aarch64 `virt`)은 `GenericError`다.
    pub async fn query_hotpluggable_cpus(&self) -> Result<Vec<HotpluggableCpu>, QmpError> {
        let ret = self.execute("query-hotpluggable-cpus", None).await?;
        ret.as_array()
            .ok_or_else(|| QmpError::Protocol(format!("query-hotpluggable-cpus: {ret}")))?
            .iter()
            .map(HotpluggableCpu::parse)
            .collect()
    }

    /// 켜진 vCPU 수(`query-cpus-fast`)
    pub async fn query_cpus_fast(&self) -> Result<usize, QmpError> {
        let ret = self.execute("query-cpus-fast", None).await?;
        ret.as_array()
            .map(Vec::len)
            .ok_or_else(|| QmpError::Protocol(format!("query-cpus-fast: {ret}")))
    }

    /// 장치를 붙인다(`device_add`, 인자는 `driver`·`id`와 장치 속성).
    pub async fn device_add(&self, args: Value) -> Result<(), QmpError> {
        self.execute("device_add", Some(args)).await.map(drop)
    }

    /// 장치 `id`를 떼고 `DEVICE_DELETED`를 `timeout`까지 기다린다. 뗐으면(이미 없었으면) true, 게스트가 시간 안에
    /// 내려 주지 않았으면 false다(요청은 QEMU에 남아 게스트가 나중에 내려 줄 수 있다).
    pub async fn device_del_wait(&self, id: &str, timeout: Duration) -> Result<bool, QmpError> {
        // 이벤트가 `device_del`의 답보다 먼저 와도 놓치지 않게 보내기 전에 구독한다.
        let mut events = self.subscribe();
        let run = async {
            match self.execute("device_del", Some(json!({ "id": id }))).await {
                Ok(_) => {}
                Err(QmpError::Command { class, .. }) if class == "DeviceNotFound" => {
                    return Ok(true);
                }
                // 앞서 보낸 떼기를 게스트가 아직 처리하는 중이다(PCIe 핫플러그, QEMU 6.2+). 그 이벤트를 기다린다.
                Err(QmpError::Command { desc, .. })
                    if desc.contains("already in the process of unplug") => {}
                Err(e) => return Err(e),
            }
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        if ev.event == "DEVICE_DELETED" && ev.data["device"] == id {
                            return Ok(true);
                        }
                    }
                    // 이벤트를 놓쳤으면 장치가 아직 있는지 직접 본다.
                    Err(RecvError::Lagged(_)) => {
                        if !self.peripheral_exists(id).await? {
                            return Ok(true);
                        }
                    }
                    Err(RecvError::Closed) => return Err(QmpError::Disconnected),
                }
            }
        };
        match tokio::time::timeout(timeout, run).await {
            Ok(r) => r,
            Err(_) => Ok(false),
        }
    }

    /// id를 준 장치 `id`가 아직 있는지(`/machine/peripheral`의 자식)
    async fn peripheral_exists(&self, id: &str) -> Result<bool, QmpError> {
        let ret = self
            .execute("qom-list", Some(json!({ "path": "/machine/peripheral" })))
            .await?;
        let children = ret
            .as_array()
            .ok_or_else(|| QmpError::Protocol(format!("qom-list: {ret}")))?;
        Ok(children.iter().any(|c| c["name"] == id))
    }

    /// id를 준 장치의 id 목록(`/machine/peripheral`의 자식, `qom-list`에서 `child<…>` 속성)
    pub async fn peripheral_ids(&self) -> Result<Vec<String>, QmpError> {
        let ret = self
            .execute("qom-list", Some(json!({ "path": "/machine/peripheral" })))
            .await?;
        let children = ret
            .as_array()
            .ok_or_else(|| QmpError::Protocol(format!("qom-list: {ret}")))?;
        Ok(children
            .iter()
            .filter(|c| c["type"].as_str().is_some_and(|t| t.starts_with("child<")))
            .filter_map(|c| c["name"].as_str().map(str::to_string))
            .collect())
    }

    /// 네트워크 백엔드를 만든다(`netdev_add`, 인자는 `type`·`id`와 백엔드 속성).
    pub async fn netdev_add(&self, args: Value) -> Result<(), QmpError> {
        self.execute("netdev_add", Some(args)).await.map(drop)
    }

    /// NIC 장치 `name`(qdev id, 예: `nic0`)의 링크를 올리거나 내린다(`set_link` — 게스트가 링크 변화를 본다, 계획 67 §9.4).
    pub async fn set_link(&self, name: &str, up: bool) -> Result<(), QmpError> {
        self.execute("set_link", Some(json!({ "name": name, "up": up })))
            .await
            .map(drop)
    }

    /// 네트워크 백엔드 `id`를 지운다.
    pub async fn netdev_del(&self, id: &str) -> Result<(), QmpError> {
        self.execute("netdev_del", Some(json!({ "id": id })))
            .await
            .map(drop)
    }

    /// 블록 노드를 연다(`blockdev-add`, 인자는 `driver`·`node-name`과 드라이버 속성).
    pub async fn blockdev_add(&self, args: Value) -> Result<(), QmpError> {
        self.execute("blockdev-add", Some(args)).await.map(drop)
    }

    /// 이동식 장치 `id`(qdev id, 예: `cd0-dev`)의 트레이를 연다. `force`면 게스트가 잠가 두었어도 연다.
    pub async fn blockdev_open_tray(&self, id: &str, force: bool) -> Result<(), QmpError> {
        let args = json!({ "id": id, "force": force });
        self.execute("blockdev-open-tray", Some(args))
            .await
            .map(drop)
    }

    /// 이동식 장치 `id`의 트레이를 닫는다.
    pub async fn blockdev_close_tray(&self, id: &str) -> Result<(), QmpError> {
        self.execute("blockdev-close-tray", Some(json!({ "id": id })))
            .await
            .map(drop)
    }

    /// 트레이를 연 장치 `id`에서 매체를 뺀다(블록 노드는 그대로 남는다). 빈 드라이브도 성공한다.
    pub async fn blockdev_remove_medium(&self, id: &str) -> Result<(), QmpError> {
        self.execute("blockdev-remove-medium", Some(json!({ "id": id })))
            .await
            .map(drop)
    }

    /// 트레이를 연 빈 장치 `id`에 블록 노드 `node`를 매체로 넣는다.
    pub async fn blockdev_insert_medium(&self, id: &str, node: &str) -> Result<(), QmpError> {
        let args = json!({ "id": id, "node-name": node });
        self.execute("blockdev-insert-medium", Some(args))
            .await
            .map(drop)
    }

    /// 블록 장치마다의 qdev id와 넣은 매체의 파일(`query-block`). 빈 드라이브는 None이다. qdev가 경로
    /// (`/machine/peripheral/disk0-dev/virtio-backend`)로 오면 id(`disk0-dev`)만 남기고, 장치가 없는 백엔드는 뺀다.
    pub async fn query_block_media(&self) -> Result<Vec<(String, Option<PathBuf>)>, QmpError> {
        let ret = self.execute("query-block", None).await?;
        let blocks = ret
            .as_array()
            .ok_or_else(|| QmpError::Protocol(format!("query-block: {ret}")))?;
        Ok(blocks
            .iter()
            .filter_map(|b| {
                let qdev = b["qdev"].as_str()?;
                let id = match qdev.strip_prefix("/machine/peripheral/") {
                    Some(rest) => rest.split('/').next().unwrap_or(rest),
                    None => qdev,
                };
                let file = b["inserted"]["file"].as_str().map(PathBuf::from);
                Some((id.to_string(), file))
            })
            .collect())
    }

    /// 이동식 장치 `id`(qdev id)의 트레이가 열려 있는지(`query-block`의 `tray_open`). 없는 장치는 false다.
    pub async fn query_tray_open(&self, id: &str) -> Result<bool, QmpError> {
        let ret = self.execute("query-block", None).await?;
        let blocks = ret
            .as_array()
            .ok_or_else(|| QmpError::Protocol(format!("query-block: {ret}")))?;
        Ok(blocks.iter().any(|b| {
            let qdev = b["qdev"].as_str().unwrap_or_default();
            let dev = qdev
                .strip_prefix("/machine/peripheral/")
                .map_or(qdev, |rest| rest.split('/').next().unwrap_or(rest));
            dev == id && b["tray_open"].as_bool() == Some(true)
        }))
    }

    /// QOM 객체 `path`의 속성 `property`를 읽는다.
    pub async fn qom_get(&self, path: &str, property: &str) -> Result<Value, QmpError> {
        self.execute(
            "qom-get",
            Some(json!({ "path": path, "property": property })),
        )
        .await
    }

    /// QOM 객체 `path`의 속성 `property`를 `value`로 바꾼다. 예: virtio-mem의 `requested-size`(바이트)
    pub async fn qom_set(&self, path: &str, property: &str, value: Value) -> Result<(), QmpError> {
        self.execute(
            "qom-set",
            Some(json!({ "path": path, "property": property, "value": value })),
        )
        .await
        .map(drop)
    }

    /// QEMU 버전 `major.minor.micro`
    pub async fn query_version(&self) -> Result<String, QmpError> {
        let ret = self.execute("query-version", None).await?;
        let q = &ret["qemu"];
        match (
            q["major"].as_u64(),
            q["minor"].as_u64(),
            q["micro"].as_u64(),
        ) {
            (Some(major), Some(minor), Some(micro)) => Ok(format!("{major}.{minor}.{micro}")),
            _ => Err(QmpError::Protocol(format!(
                "query-version without version: {ret}"
            ))),
        }
    }

    /// TLS 자격 증명 객체 `id`를 만든다. `dir`에 `ca-cert.pem`과 받는 쪽은 `server-cert.pem`·
    /// `server-key.pem`, 보내는 쪽은 `client-cert.pem`·`client-key.pem`이 있어야 한다(만들 때 읽는다).
    /// 늘 상대의 인증서를 확인한다(`verify-peer`).
    pub async fn add_tls_creds(
        &self,
        id: &str,
        dir: &Path,
        endpoint: TlsEndpoint,
    ) -> Result<(), QmpError> {
        let endpoint = match endpoint {
            TlsEndpoint::Server => "server",
            TlsEndpoint::Client => "client",
        };
        let args = json!({
            "qom-type": "tls-creds-x509",
            "id": id,
            "dir": dir.display().to_string(),
            "endpoint": endpoint,
            "verify-peer": true,
        });
        self.execute("object-add", Some(args)).await.map(drop)
    }

    /// `object-add`로 만든 객체 `id`를 지운다.
    pub async fn object_del(&self, id: &str) -> Result<(), QmpError> {
        self.execute("object-del", Some(json!({ "id": id })))
            .await
            .map(drop)
    }

    /// 마이그레이션 능력을 켜거나 끈다. 예: `("events", true)`
    pub async fn migrate_set_capabilities(
        &self,
        capabilities: &[(&str, bool)],
    ) -> Result<(), QmpError> {
        let caps: Vec<Value> = capabilities
            .iter()
            .map(|(name, state)| json!({ "capability": name, "state": state }))
            .collect();
        self.execute(
            "migrate-set-capabilities",
            Some(json!({ "capabilities": caps })),
        )
        .await
        .map(drop)
    }

    pub async fn migrate_set_parameters(
        &self,
        params: &MigrationParameters,
    ) -> Result<(), QmpError> {
        let mut args = serde_json::Map::new();
        if let Some(creds) = &params.tls_creds {
            args.insert("tls-creds".into(), json!(creds));
        }
        if let Some(host) = &params.tls_hostname {
            args.insert("tls-hostname".into(), json!(host));
        }
        if let Some(bw) = params.max_bandwidth {
            args.insert("max-bandwidth".into(), json!(bw));
        }
        self.execute("migrate-set-parameters", Some(Value::Object(args)))
            .await
            .map(drop)
    }

    /// 마이그레이션을 `uri`(`tcp:host:port`)로 보내기 시작한다. 곧바로 돌아오므로 끝은
    /// [`wait_migration`](Self::wait_migration)으로 기다린다.
    pub async fn migrate(&self, uri: &str) -> Result<(), QmpError> {
        self.execute("migrate", Some(json!({ "uri": uri })))
            .await
            .map(drop)
    }

    /// `-incoming defer`로 띄운 QEMU가 `uri`에서 마이그레이션을 받기 시작한다. 돌아올 때는 이미
    /// 포트를 열었다. QEMU 하나에 한 번만 부를 수 있다(실패해도 다시 부를 수 없다).
    pub async fn migrate_incoming(&self, uri: &str) -> Result<(), QmpError> {
        self.execute("migrate-incoming", Some(json!({ "uri": uri })))
            .await
            .map(drop)
    }

    /// `pre-switchover`에서 기다리는 마이그레이션을 이어 간다(장치 상태를 보내고 끝낸다).
    pub async fn migrate_continue(&self) -> Result<(), QmpError> {
        self.execute(
            "migrate-continue",
            Some(json!({ "state": "pre-switchover" })),
        )
        .await
        .map(drop)
    }

    /// 진행 중인 마이그레이션을 취소한다. 원본은 계속 돈다.
    pub async fn migrate_cancel(&self) -> Result<(), QmpError> {
        self.execute("migrate_cancel", None).await.map(drop)
    }

    pub async fn query_migrate(&self) -> Result<MigrationInfo, QmpError> {
        let ret = self.execute("query-migrate", None).await?;
        Ok(MigrationInfo::parse(&ret))
    }

    /// 마이그레이션이 끝난 상태(완료·실패·취소)가 될 때까지 기다린다. `MIGRATION` 이벤트(능력
    /// `events`)를 받거나 `poll`마다 `query-migrate`로 확인한다. 제한 시간은 호출자가 건다.
    pub async fn wait_migration(&self, poll: Duration) -> Result<MigrationInfo, QmpError> {
        self.wait_migration_until(poll, MigrationState::is_finished)
            .await
    }

    /// [`wait_migration`](Self::wait_migration)과 같되 `until`이 참인 상태가 될 때까지 기다린다.
    /// 예: 끝났거나 `pre-switchover`
    pub async fn wait_migration_until(
        &self,
        poll: Duration,
        until: impl Fn(&MigrationState) -> bool,
    ) -> Result<MigrationInfo, QmpError> {
        // 처음 조회와 이벤트 사이에 끝나도 놓치지 않게 먼저 구독한다.
        let mut events = self.subscribe();
        loop {
            let info = self.query_migrate().await?;
            if until(&info.state) {
                return Ok(info);
            }
            tokio::select! {
                ev = events.recv() => match ev {
                    Err(RecvError::Closed) => return Err(QmpError::Disconnected),
                    // 이벤트 내용과 상관없이 다시 조회한다(놓쳤으면 Lagged).
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                },
                _ = tokio::time::sleep(poll) => {}
            }
        }
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
        // 제한 시간에 걸렸을 때 어디까지 갔는지
        let mut stage = format!("no reply to {command}");
        // 시작 명령부터 dismiss까지 모두 제한 시간 안에 끝나야 한다. QEMU는 snapshot-save에
        // 작업이 끝난 뒤에야 답할 수 있으므로, 시작 명령의 응답도 기다림에 넣어야 멈춘 작업을 잡는다.
        let run = async {
            self.execute(command, Some(arguments)).await?;
            stage = "job started, no JOB_STATUS_CHANGE seen".into();
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        if ev.event == "JOB_STATUS_CHANGE" && ev.data["id"] == id.as_str() {
                            stage = format!("last job status {}", ev.data["status"]);
                            if ev.data["status"] == "concluded" {
                                break;
                            }
                        }
                    }
                    // 이벤트를 놓쳤으면 query-jobs로 상태를 직접 본다.
                    Err(RecvError::Lagged(_)) => {
                        if self.query_job(&id).await?["status"] == "concluded" {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => return Err(QmpError::Disconnected),
                }
            }
            stage = "no reply to query-jobs after the job concluded".into();
            let error = self.query_job(&id).await?["error"]
                .as_str()
                .map(str::to_string);
            stage = "no reply to job-dismiss".into();
            self.execute("job-dismiss", Some(json!({ "id": id })))
                .await?;
            Ok(error)
        };
        let error = match tokio::time::timeout(timeout, run).await {
            Ok(r) => r?,
            Err(_) => {
                // QEMU의 모니터가 아직 답하는지, 작업이 어떤 상태인지 짧게 물어 오류에 남긴다.
                let jobs =
                    match tokio::time::timeout(JOB_PROBE_TIMEOUT, self.execute("query-jobs", None))
                        .await
                    {
                        Ok(Ok(jobs)) => format!("query-jobs: {jobs}"),
                        Ok(Err(e)) => format!("query-jobs failed: {e}"),
                        Err(_) => {
                            format!("QEMU did not answer query-jobs within {JOB_PROBE_TIMEOUT:?}")
                        }
                    };
                return Err(QmpError::JobTimeout {
                    id,
                    timeout,
                    stage: format!("{stage}; {jobs}"),
                });
            }
        };
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

/// CD-ROM 드라이브 `index`(장치 `cd{index}-dev`)의 매체를 `medium` 파일로 바꾸고, None이면 빼서 빈 드라이브로 둔다.
/// 매체는 읽기 전용 블록 노드 `{node}-file`(file)·`{node}`(raw)로 연다. 노드 이름은 켤 때의 `cd{index}`이거나 바꿀
/// 때마다 쓰지 않은 `cd{index}-{n}`이다. 새 매체의 노드를 **먼저** 열므로 파일을 열지 못하면 넣은 매체는 그대로다.
/// 그 뒤 트레이를 강제로 열고 매체를 빼고 새 노드를 넣은 다음 옛 노드(넣었던 매체와 앞서 실패하고 남은 것)를 닫는다.
/// 옛 노드를 닫지 못해도 트레이는 닫는다(남은 노드는 다음 바꾸기가 닫는다). 넣기가 실패하면 연 노드를 닫고 트레이를
/// 닫은 뒤 처음 오류를 돌려준다. `force`로 열므로 게스트가 트레이를 잠가 두었어도 바뀐다(게스트는 매체 바뀜을 본다).
pub async fn change_medium(
    qmp: &QmpClient,
    index: usize,
    medium: Option<&Path>,
) -> Result<(), QmpError> {
    let dev = format!("cd{index}-dev");
    let base = format!("cd{index}");
    // 이 드라이브의 매체 노드(raw `cd{i}`·`cd{i}-{n}`과 그 `-file`)
    let ours = |n: &str| {
        let raw = n.strip_suffix("-file").unwrap_or(n);
        raw == base
            || raw
                .strip_prefix(&base)
                .and_then(|r| r.strip_prefix('-'))
                .is_some_and(|k| !k.is_empty() && k.bytes().all(|c| c.is_ascii_digit()))
    };
    let old: Vec<String> = qmp
        .query_block_nodes()
        .await?
        .into_iter()
        .map(|n| n.node_name)
        .filter(|n| ours(n))
        .collect();
    let mut opened = None;
    if let Some(path) = medium {
        let node = std::iter::once(base.clone())
            .chain((1..).map(|k| format!("{base}-{k}")))
            .find(|n| !old.contains(n) && !old.contains(&format!("{n}-file")))
            .expect("a free node name");
        let file = format!("{node}-file");
        let open = async {
            qmp.blockdev_add(json!({
                "driver": "file", "filename": path.display().to_string(),
                "node-name": file, "read-only": true,
            }))
            .await?;
            qmp.blockdev_add(json!({
                "driver": "raw", "file": file, "node-name": node, "read-only": true,
            }))
            .await
        };
        if let Err(e) = open.await {
            // 정리 실패는 처음 오류를 가리지 않는다(없는 노드를 닫는 오류 등). 넣은 매체는 그대로다.
            let _ = qmp.blockdev_del(&node).await;
            let _ = qmp.blockdev_del(&file).await;
            return Err(e);
        }
        opened = Some((node, file));
    }
    let swap = async {
        qmp.blockdev_open_tray(&dev, true).await?;
        qmp.blockdev_remove_medium(&dev).await?;
        if let Some((node, _)) = &opened {
            qmp.blockdev_insert_medium(&dev, node).await?;
        }
        Ok(())
    };
    if let Err(e) = swap.await {
        if let Some((node, file)) = &opened {
            let _ = qmp.blockdev_del(node).await;
            let _ = qmp.blockdev_del(file).await;
        }
        let _ = qmp.blockdev_close_tray(&dev).await;
        return Err(e);
    }
    // raw 노드를 먼저 닫아야 그 아래 file 노드가 닫힌다.
    let (raws, files): (Vec<&String>, Vec<&String>) =
        old.iter().partition(|n| !n.ends_with("-file"));
    for n in raws.into_iter().chain(files) {
        let _ = qmp.blockdev_del(n).await;
    }
    qmp.blockdev_close_tray(&dev).await
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

    /// QEMU가 작업을 시작하는 명령 자체에 답하지 않아도(CI에서 본 snapshot-save 멈춤) 제한 시간에
    /// 끝나고, 어디서 멈췄는지와 그때 QEMU가 `query-jobs`에 답했는지를 오류에 담는다.
    #[tokio::test]
    async fn job_timeout_covers_unanswered_start_and_says_where_it_stuck() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        // 명령을 받기만 하고 답하지 않는다(연결은 열어 둔다).
        let fake_side =
            tokio::spawn(async move { while let Ok(Some(_)) = fake.lines.next_line().await {} });
        let res = tokio::time::timeout(
            Duration::from_secs(10),
            client.snapshot_save("s1", "disk0", &devices, Duration::from_millis(200)),
        )
        .await;
        fake_side.abort();
        let err = res
            .expect("run_job must not outlive its timeout")
            .unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, QmpError::JobTimeout { .. }), "{msg}");
        assert!(msg.contains("no reply to snapshot-save"), "{msg}");
        assert!(msg.contains("query-jobs"), "{msg}");
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
    async fn migration_states_are_parsed() {
        let (client, mut fake, _dir) = setup().await;
        let (status, ()) = tokio::join!(client.query_status(), async {
            fake.answer(
                "query-status",
                json!({ "return": { "running": false, "status": "postmigrate" } }),
            )
            .await;
        });
        assert_eq!(status.unwrap(), RunState::PostMigrate);
        let (status, ()) = tokio::join!(client.query_status(), async {
            fake.answer(
                "query-status",
                json!({ "return": { "running": false, "status": "inmigrate" } }),
            )
            .await;
        });
        assert_eq!(status.unwrap(), RunState::InMigrate);

        // 마이그레이션을 한 적이 없으면 status가 없다.
        for (ret, state, error) in [
            (json!({}), MigrationState::None, None),
            (
                json!({ "status": "setup", "socket-address": [{ "port": "60000" }] }),
                MigrationState::Setup,
                None,
            ),
            (json!({ "status": "active" }), MigrationState::Active, None),
            (
                json!({ "status": "completed" }),
                MigrationState::Completed,
                None,
            ),
            (
                json!({
                    "status": "failed",
                    "error-desc": "Failed to connect to '127.0.0.1:60099': Connection refused"
                }),
                MigrationState::Failed,
                Some("Failed to connect to '127.0.0.1:60099': Connection refused"),
            ),
            (
                json!({ "status": "cancelled" }),
                MigrationState::Cancelled,
                None,
            ),
            (
                json!({ "status": "pre-switchover" }),
                MigrationState::PreSwitchover,
                None,
            ),
            (
                json!({ "status": "device" }),
                MigrationState::Other("device".into()),
                None,
            ),
        ] {
            let (info, ()) = tokio::join!(client.query_migrate(), async {
                fake.answer("query-migrate", json!({ "return": ret })).await;
            });
            let info = info.unwrap();
            assert_eq!(info.state, state);
            assert_eq!(info.error.as_deref(), error);
        }
        assert!(MigrationState::Completed.is_finished());
        assert!(MigrationState::Failed.is_finished());
        assert!(MigrationState::Cancelled.is_finished());
        assert!(!MigrationState::Cancelling.is_finished());
        assert!(!MigrationState::None.is_finished());
        assert!(!MigrationState::PreSwitchover.is_finished());
    }

    #[tokio::test]
    async fn query_version_formats_major_minor_micro() {
        let (client, mut fake, _dir) = setup().await;
        let (v, ()) = tokio::join!(client.query_version(), async {
            fake.answer(
                "query-version",
                json!({ "return": { "qemu": { "micro": 2, "minor": 2, "major": 8 }, "package": "Debian 1:8.2.2" } }),
            )
            .await;
        });
        assert_eq!(v.unwrap(), "8.2.2");
        let (v, ()) = tokio::join!(client.query_version(), async {
            fake.answer("query-version", json!({ "return": {} })).await;
        });
        assert!(matches!(v, Err(QmpError::Protocol(_))));
    }

    #[tokio::test]
    async fn migration_setup_commands_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let dir = Path::new("/var/lib/opl/migration-tls");
        let (r, cmd) = tokio::join!(
            client.add_tls_creds("opl-mig-tls", dir, TlsEndpoint::Server),
            async {
                let cmd = fake.recv().await;
                fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
                cmd
            }
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "object-add");
        assert_eq!(
            cmd["arguments"],
            json!({
                "qom-type": "tls-creds-x509", "id": "opl-mig-tls",
                "dir": "/var/lib/opl/migration-tls", "endpoint": "server", "verify-peer": true
            })
        );
        let (r, cmd) = tokio::join!(client.add_tls_creds("c", dir, TlsEndpoint::Client), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["arguments"]["endpoint"], "client");

        let (r, cmd) = tokio::join!(client.object_del("opl-mig-tls"), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["execute"], "object-del");
        assert_eq!(cmd["arguments"], json!({ "id": "opl-mig-tls" }));

        let (r, cmd) = tokio::join!(
            client.migrate_set_capabilities(&[("events", true), ("auto-converge", true)]),
            async {
                let cmd = fake.recv().await;
                fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
                cmd
            }
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "migrate-set-capabilities");
        assert_eq!(
            cmd["arguments"],
            json!({ "capabilities": [
                { "capability": "events", "state": true },
                { "capability": "auto-converge", "state": true }
            ] })
        );

        // 정한 값만 보낸다.
        let params = MigrationParameters {
            tls_creds: Some("opl-mig-tls".into()),
            tls_hostname: Some("10.0.0.2".into()),
            max_bandwidth: Some(100 << 20),
        };
        let (r, cmd) = tokio::join!(client.migrate_set_parameters(&params), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["execute"], "migrate-set-parameters");
        assert_eq!(
            cmd["arguments"],
            json!({ "tls-creds": "opl-mig-tls", "tls-hostname": "10.0.0.2", "max-bandwidth": 100u64 << 20 })
        );
        let params = MigrationParameters {
            tls_creds: Some("opl-mig-tls".into()),
            ..Default::default()
        };
        let (r, cmd) = tokio::join!(client.migrate_set_parameters(&params), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["arguments"], json!({ "tls-creds": "opl-mig-tls" }));

        for (name, expect) in [("migrate", "migrate"), ("incoming", "migrate-incoming")] {
            let uri = "tcp:10.0.0.2:60000";
            let fut = async {
                match name {
                    "migrate" => client.migrate(uri).await,
                    _ => client.migrate_incoming(uri).await,
                }
            };
            let (r, cmd) = tokio::join!(fut, async {
                let cmd = fake.recv().await;
                fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
                cmd
            });
            r.unwrap();
            assert_eq!(cmd["execute"], expect);
            assert_eq!(cmd["arguments"], json!({ "uri": uri }));
        }
        let (r, ()) = tokio::join!(client.migrate_cancel(), async {
            fake.answer("migrate_cancel", json!({ "return": {} })).await;
        });
        r.unwrap();
    }

    /// 이벤트를 받으면 다시 조회하고, 끝난 상태에서 돌아온다.
    #[tokio::test]
    async fn wait_migration_requeries_on_events_until_finished() {
        let (client, mut fake, _dir) = setup().await;
        // 긴 poll: 이벤트로만 깨어나야 시간 안에 끝난다.
        let (info, ()) = tokio::join!(client.wait_migration(Duration::from_secs(3600)), async {
            fake.answer("query-migrate", json!({ "return": { "status": "active" } }))
                .await;
            fake.send(
                json!({ "event": "MIGRATION", "data": { "status": "failed" }, "timestamp": {} }),
            )
            .await;
            fake.answer(
                "query-migrate",
                json!({ "return": { "status": "failed", "error-desc": "TLS handshake failed" } }),
            )
            .await;
        });
        let info = info.unwrap();
        assert_eq!(info.state, MigrationState::Failed);
        assert_eq!(info.error.as_deref(), Some("TLS handshake failed"));
    }

    /// 이벤트가 없어도 poll마다 조회한다(능력 `events`가 꺼진 QEMU, 놓친 이벤트).
    #[tokio::test]
    async fn wait_migration_polls_without_events() {
        let (client, mut fake, _dir) = setup().await;
        let (info, ()) = tokio::join!(client.wait_migration(Duration::from_millis(20)), async {
            fake.answer("query-migrate", json!({ "return": { "status": "setup" } }))
                .await;
            fake.answer("query-migrate", json!({ "return": { "status": "active" } }))
                .await;
            fake.answer(
                "query-migrate",
                json!({ "return": { "status": "completed" } }),
            )
            .await;
        });
        assert_eq!(info.unwrap().state, MigrationState::Completed);
    }

    #[tokio::test]
    async fn wait_migration_fails_when_qemu_goes_away() {
        let (client, mut fake, _dir) = setup().await;
        let (info, ()) = tokio::join!(client.wait_migration(Duration::from_secs(3600)), async {
            fake.answer("query-migrate", json!({ "return": { "status": "active" } }))
                .await;
            drop(fake);
        });
        assert!(matches!(info, Err(QmpError::Disconnected)), "{info:?}");
    }

    /// 명령 하나를 받아 `ret`으로 답하고 그 명령을 돌려준다.
    async fn reply(fake: &mut FakeQemu, ret: Value) -> Value {
        let cmd = fake.recv().await;
        fake.send(json!({ "return": ret, "id": cmd["id"] })).await;
        cmd
    }

    /// 증분 백업의 transaction: 디스크마다 새 비트맵과 blockdev-backup이 한 시점에 들어간다.
    #[tokio::test]
    async fn transaction_serializes_bitmap_and_backup_actions() {
        let (client, mut fake, _dir) = setup().await;
        let actions = [
            TransactionAction::SnapshotInternal {
                device: "disk0".into(),
                name: "t".into(),
            },
            TransactionAction::BitmapAdd {
                node: "disk0".into(),
                name: "opl-new".into(),
                persistent: true,
            },
            TransactionAction::IncrementalBackup {
                job_id: "opl-backup-0".into(),
                device: "disk0".into(),
                target: "opl-backup-0".into(),
                bitmap: "opl-old".into(),
            },
        ];
        let (r, cmd) = tokio::join!(client.transaction(&actions), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(cmd["execute"], "transaction");
        assert_eq!(
            cmd["arguments"]["actions"],
            json!([
                { "type": "blockdev-snapshot-internal-sync", "data": { "device": "disk0", "name": "t" } },
                { "type": "block-dirty-bitmap-add",
                  "data": { "node": "disk0", "name": "opl-new", "persistent": true } },
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-backup-0", "device": "disk0", "target": "opl-backup-0",
                    "sync": "bitmap", "bitmap": "opl-old", "bitmap-mode": "never",
                    "compress": true, "auto-dismiss": false
                } }
            ])
        );
    }

    /// 복제의 blockdev-backup: 전체(`sync: full`)와 비트맵(`bitmap-mode: never`), 속도. NBD 대상이라 압축하지
    /// 않는다.
    #[tokio::test]
    async fn transaction_serializes_replication_backups() {
        let (client, mut fake, _dir) = setup().await;
        let actions = [
            TransactionAction::Backup {
                job_id: "opl-repl-0".into(),
                device: "disk0".into(),
                target: "opl-repl-nbd-0".into(),
                sync: BackupSync::Full,
                speed: None,
            },
            TransactionAction::Backup {
                job_id: "opl-repl-1".into(),
                device: "disk1".into(),
                target: "opl-repl-nbd-1".into(),
                sync: BackupSync::Bitmap("opl-repl-a-b".into()),
                speed: Some(1 << 20),
            },
        ];
        let (r, cmd) = tokio::join!(client.transaction(&actions), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(
            cmd["arguments"]["actions"],
            json!([
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-repl-0", "device": "disk0", "target": "opl-repl-nbd-0",
                    "sync": "full", "auto-dismiss": false
                } },
                { "type": "blockdev-backup", "data": {
                    "job-id": "opl-repl-1", "device": "disk1", "target": "opl-repl-nbd-1",
                    "sync": "bitmap", "bitmap": "opl-repl-a-b", "bitmap-mode": "never",
                    "speed": 1048576, "auto-dismiss": false
                } }
            ])
        );
    }

    /// 내보내기 닫기는 붙은 클라이언트를 끊는다(`mode: hard`).
    #[tokio::test]
    async fn block_export_del_sends_the_id() {
        let (client, mut fake, _dir) = setup().await;
        let (r, cmd) = tokio::join!(
            client.block_export_del("opl-repl-export-0"),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "block-export-del");
        assert_eq!(
            cmd["arguments"],
            json!({ "id": "opl-repl-export-0", "mode": "hard" })
        );
    }

    #[tokio::test]
    async fn bitmap_blockdev_and_job_commands_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let (r, cmd) = tokio::join!(
            client.block_dirty_bitmap_add("disk1", "opl-a", false),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "block-dirty-bitmap-add");
        assert_eq!(
            cmd["arguments"],
            json!({ "node": "disk1", "name": "opl-a", "persistent": false })
        );
        let (r, cmd) = tokio::join!(
            client.block_dirty_bitmap_remove("disk1", "opl-a"),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "block-dirty-bitmap-remove");
        assert_eq!(
            cmd["arguments"],
            json!({ "node": "disk1", "name": "opl-a" })
        );

        let path = Path::new("/pools/b/backups/x/disk0.qcow2.part");
        let (r, cmd) = tokio::join!(
            client.blockdev_add_qcow2("opl-backup-0", path),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "blockdev-add");
        assert_eq!(
            cmd["arguments"],
            json!({
                "driver": "qcow2", "node-name": "opl-backup-0",
                "file": { "driver": "file", "filename": "/pools/b/backups/x/disk0.qcow2.part" },
                "backing": null
            })
        );
        let (r, cmd) = tokio::join!(
            client.blockdev_del("opl-backup-0"),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "blockdev-del");
        assert_eq!(cmd["arguments"], json!({ "node-name": "opl-backup-0" }));

        for (name, expect) in [("cancel", "job-cancel"), ("dismiss", "job-dismiss")] {
            let fut = async {
                match name {
                    "cancel" => client.job_cancel("opl-backup-1").await,
                    _ => client.job_dismiss("opl-backup-1").await,
                }
            };
            let (r, cmd) = tokio::join!(fut, reply(&mut fake, json!({})));
            r.unwrap();
            assert_eq!(cmd["execute"], expect);
            assert_eq!(cmd["arguments"], json!({ "id": "opl-backup-1" }));
        }
    }

    #[tokio::test]
    async fn query_jobs_reads_status_and_error() {
        let (client, mut fake, _dir) = setup().await;
        // QEMU 11.1의 미러 작업 모양(ready 항목은 계획 42의 환경 탐색에서 본 그대로)
        let ret = json!([
            { "id": "opl-backup-0", "type": "backup", "status": "running",
              "current-progress": 1, "total-progress": 4 },
            { "id": "opl-backup-1", "type": "backup", "status": "concluded",
              "current-progress": 0, "total-progress": 4, "error": "No space left on device" },
            { "current-progress": 7405568, "status": "ready", "total-progress": 7405568,
              "type": "mirror", "id": "opl-mirror-0" },
            { "id": "old", "type": "mirror", "status": "created" }
        ]);
        let (jobs, _) = tokio::join!(client.query_jobs(), reply(&mut fake, ret));
        let jobs = jobs.unwrap();
        assert_eq!(
            jobs,
            [
                JobInfo {
                    id: "opl-backup-0".into(),
                    status: "running".into(),
                    error: None,
                    current_progress: 1,
                    total_progress: 4,
                },
                JobInfo {
                    id: "opl-backup-1".into(),
                    status: "concluded".into(),
                    error: Some("No space left on device".into()),
                    current_progress: 0,
                    total_progress: 4,
                },
                JobInfo {
                    id: "opl-mirror-0".into(),
                    status: "ready".into(),
                    error: None,
                    current_progress: 7405568,
                    total_progress: 7405568,
                },
                // 진행률이 없으면 0
                JobInfo {
                    id: "old".into(),
                    status: "created".into(),
                    error: None,
                    current_progress: 0,
                    total_progress: 0,
                },
            ]
        );
        assert!(!jobs[0].is_concluded());
        assert!(jobs[1].is_concluded());
        assert!(!jobs[0].is_ready());
        assert!(jobs[2].is_ready());
        assert!(!jobs[2].is_concluded());
    }

    /// 받는 쪽: TLS NBD 서버, 디스크 노드 내보내기, 서버 멈춤
    #[tokio::test]
    async fn nbd_export_commands_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let (r, cmd) = tokio::join!(
            client.nbd_server_start("10.0.0.2", 60001, "opl-nbd-tls"),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "nbd-server-start");
        // nbd-server-start의 addr는 옛 모양(SocketAddressLegacy, `data` 안에 주소)이고 포트는 문자열이다.
        assert_eq!(
            cmd["arguments"],
            json!({
                "addr": { "type": "inet", "data": { "host": "10.0.0.2", "port": "60001" } },
                "tls-creds": "opl-nbd-tls"
            })
        );
        let (r, cmd) = tokio::join!(
            client.block_export_add_nbd("opl-export-1", "disk1", "disk1"),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "block-export-add");
        assert_eq!(
            cmd["arguments"],
            json!({
                "type": "nbd", "id": "opl-export-1", "node-name": "disk1",
                "name": "disk1", "writable": true
            })
        );
        let (r, cmd) = tokio::join!(client.nbd_server_stop(), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(cmd["execute"], "nbd-server-stop");
        assert!(cmd.get("arguments").is_none(), "{cmd}");
        // 서버가 없으면 QEMU는 오류로 답한다(QEMU 11.1: "NBD server not running").
        let (r, ()) = tokio::join!(client.nbd_server_stop(), async {
            fake.answer(
                "nbd-server-stop",
                json!({ "error": { "class": "GenericError", "desc": "NBD server not running" } }),
            )
            .await;
        });
        assert!(matches!(r, Err(QmpError::Command { .. })), "{r:?}");
    }

    /// 보내는 쪽: TLS NBD 블록 노드, 미러 작업, 미러 끝내기·취소, 스위치오버 이어 가기
    #[tokio::test]
    async fn mirror_commands_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let nbd = NbdBlockdev {
            node_name: "opl-mirror-0".into(),
            host: "10.0.0.2".into(),
            port: 60001,
            export: "disk0".into(),
            tls_creds: "opl-nbd-tls".into(),
            tls_hostname: "10.0.0.2".into(),
        };
        let (r, cmd) = tokio::join!(client.blockdev_add_nbd(&nbd), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(cmd["execute"], "blockdev-add");
        assert_eq!(
            cmd["arguments"],
            json!({
                "driver": "nbd", "node-name": "opl-mirror-0",
                "server": { "type": "inet", "host": "10.0.0.2", "port": "60001" },
                "export": "disk0", "tls-creds": "opl-nbd-tls", "tls-hostname": "10.0.0.2"
            })
        );

        let (r, cmd) = tokio::join!(
            client.blockdev_mirror("opl-mirror-0", "disk0", "opl-mirror-0", Some(50 << 20)),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "blockdev-mirror");
        assert_eq!(
            cmd["arguments"],
            json!({
                "job-id": "opl-mirror-0", "device": "disk0", "target": "opl-mirror-0",
                "sync": "full", "copy-mode": "background", "auto-dismiss": false,
                "speed": 50u64 << 20
            })
        );
        // 속도 제한이 없으면 speed를 보내지 않는다(QEMU 기본 0 = 제한 없음).
        let (r, cmd) = tokio::join!(
            client.blockdev_mirror("opl-mirror-1", "disk1", "opl-mirror-1", None),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert!(cmd["arguments"].get("speed").is_none(), "{cmd}");

        for force in [false, true] {
            let (r, cmd) = tokio::join!(
                client.block_job_cancel("opl-mirror-0", force),
                reply(&mut fake, json!({}))
            );
            r.unwrap();
            assert_eq!(cmd["execute"], "block-job-cancel");
            assert_eq!(
                cmd["arguments"],
                json!({ "device": "opl-mirror-0", "force": force })
            );
        }

        let (r, cmd) = tokio::join!(client.migrate_continue(), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(cmd["execute"], "migrate-continue");
        assert_eq!(cmd["arguments"], json!({ "state": "pre-switchover" }));
    }

    /// `pause-before-switchover`로 보낸 마이그레이션은 `pre-switchover`에서 기다린다. 그 상태에서
    /// 돌아오는 기다림과, 끝난 상태까지 가는 기다림이 다르다.
    #[tokio::test]
    async fn wait_migration_until_stops_at_pre_switchover() {
        let (client, mut fake, _dir) = setup().await;
        let until = |s: &MigrationState| s.is_finished() || *s == MigrationState::PreSwitchover;
        let (info, ()) = tokio::join!(
            client.wait_migration_until(Duration::from_millis(20), until),
            async {
                fake.answer("query-migrate", json!({ "return": { "status": "setup" } }))
                    .await;
                fake.answer(
                    "query-migrate",
                    json!({ "return": { "status": "pre-switchover" } }),
                )
                .await;
            }
        );
        assert_eq!(info.unwrap().state, MigrationState::PreSwitchover);
        // 실패하면 pre-switchover에 닿지 않아도 돌아온다.
        let (info, ()) = tokio::join!(
            client.wait_migration_until(Duration::from_millis(20), until),
            async {
                fake.answer(
                    "query-migrate",
                    json!({ "return": { "status": "failed", "error-desc": "x" } }),
                )
                .await;
            }
        );
        assert_eq!(info.unwrap().state, MigrationState::Failed);
    }

    /// QEMU 8.2·10의 `query-named-block-nodes`(flat) 모양. `inconsistent`는 참일 때만 온다.
    #[tokio::test]
    async fn block_nodes_carry_their_dirty_bitmaps() {
        let (client, mut fake, _dir) = setup().await;
        let ret = json!([
            { "node-name": "disk0", "drv": "qcow2", "ro": false, "file": "/p/a.qcow2",
              "dirty-bitmaps": [
                { "name": "opl-a", "recording": true, "persistent": true, "busy": false,
                  "granularity": 65536, "count": 0 },
                { "name": "opl-b", "recording": true, "persistent": true, "busy": false,
                  "granularity": 65536, "count": 4096, "inconsistent": true }
              ] },
            { "node-name": "disk0-file", "drv": "file", "ro": false, "file": "/p/a.qcow2" },
            { "node-name": "disk1", "drv": "raw", "ro": false, "file": "/dev/vg/x",
              "dirty-bitmaps": [
                { "name": "opl-a", "recording": false, "persistent": false, "busy": false,
                  "granularity": 65536 },
                // 미러 작업이 디스크에 둔 이름 없는 비트맵(QEMU 11.1)
                { "busy": false, "count": 0, "granularity": 65536, "persistent": false,
                  "recording": false }
              ] }
        ]);
        let (nodes, cmd) = tokio::join!(client.query_block_nodes(), reply(&mut fake, ret));
        assert_eq!(cmd["execute"], "query-named-block-nodes");
        assert_eq!(cmd["arguments"], json!({ "flat": true }));
        let bitmap = |name: &str, recording, inconsistent, persistent, count| BitmapInfo {
            name: name.into(),
            recording,
            inconsistent,
            persistent,
            count,
        };
        assert_eq!(
            nodes.unwrap(),
            [
                BlockNode {
                    node_name: "disk0".into(),
                    driver: "qcow2".into(),
                    bitmaps: vec![
                        bitmap("opl-a", true, false, true, 0),
                        bitmap("opl-b", true, true, true, 4096)
                    ],
                },
                BlockNode {
                    node_name: "disk0-file".into(),
                    driver: "file".into(),
                    bitmaps: vec![],
                },
                // `count`가 없으면 0이다.
                BlockNode {
                    node_name: "disk1".into(),
                    driver: "raw".into(),
                    bitmaps: vec![bitmap("opl-a", false, false, false, 0)],
                },
            ]
        );

        let (nodes, _) = tokio::join!(
            client.query_block_nodes(),
            reply(&mut fake, json!([{ "node-name": "disk0" }]))
        );
        assert!(matches!(nodes, Err(QmpError::Protocol(_))), "{nodes:?}");
    }

    /// QEMU 8.2·11의 `query-blockstats` 모양: `-blockdev`로 붙인 디스크는 `device`가 비고 `qdev`가
    /// 장치 경로다. `-drive`로 붙인 pflash는 `device` 이름만 있고 `qdev`가 없다.
    #[tokio::test]
    async fn blockstats_read_counters_of_each_backend() {
        let (client, mut fake, _dir) = setup().await;
        let stats = |rd: u64, wr: u64, rd_ops: u64, wr_ops: u64| {
            json!({ "rd_bytes": rd, "wr_bytes": wr, "rd_operations": rd_ops,
                    "wr_operations": wr_ops, "flush_operations": 0, "idle_time_ns": 1,
                    "rd_merged": 0, "wr_merged": 0, "account_invalid": true })
        };
        let ret = json!([
            { "device": "pflash0", "node-name": "#block123", "stats": stats(1, 0, 1, 0) },
            { "device": "", "qdev": "/machine/peripheral/disk0-dev/virtio-backend",
              "node-name": "disk0", "stats": stats(4096, 512, 8, 1),
              "parent": { "node-name": "disk0-file", "stats": stats(0, 0, 0, 0) } },
            { "device": "", "qdev": "/machine/peripheral/disk1-dev", "stats": stats(2, 3, 4, 5) }
        ]);
        let (got, cmd) = tokio::join!(client.query_blockstats(), reply(&mut fake, ret));
        assert_eq!(cmd["execute"], "query-blockstats");
        let entry =
            |device: &str, qdev: Option<&str>, node: Option<&str>, c: [u64; 4]| BlockStats {
                device: device.into(),
                qdev: qdev.map(Into::into),
                node_name: node.map(Into::into),
                rd_bytes: c[0],
                wr_bytes: c[1],
                rd_operations: c[2],
                wr_operations: c[3],
            };
        assert_eq!(
            got.unwrap(),
            [
                entry("pflash0", None, Some("#block123"), [1, 0, 1, 0]),
                entry(
                    "",
                    Some("/machine/peripheral/disk0-dev/virtio-backend"),
                    Some("disk0"),
                    [4096, 512, 8, 1]
                ),
                entry(
                    "",
                    Some("/machine/peripheral/disk1-dev"),
                    None,
                    [2, 3, 4, 5]
                ),
            ]
        );

        // 카운터가 없으면 프로토콜 오류다.
        let (got, _) = tokio::join!(
            client.query_blockstats(),
            reply(&mut fake, json!([{ "device": "", "stats": {} }]))
        );
        assert!(matches!(got, Err(QmpError::Protocol(_))), "{got:?}");
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
    /// 핫플러그 명령은 받은 인자를 그대로 보낸다.
    #[tokio::test]
    async fn hotplug_commands_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let dev = json!({ "driver": "virtio-blk-pci", "drive": "disk1", "id": "disk1-dev", "bus": "hp0" });
        let (r, cmd) = tokio::join!(client.device_add(dev.clone()), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(
            (&cmd["execute"], &cmd["arguments"]),
            (&json!("device_add"), &dev)
        );

        let file = json!({ "driver": "file", "filename": "/p/b.qcow2", "node-name": "disk1-file" });
        let (r, cmd) = tokio::join!(
            client.blockdev_add(file.clone()),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(
            (&cmd["execute"], &cmd["arguments"]),
            (&json!("blockdev-add"), &file)
        );

        let tap = json!({ "type": "tap", "id": "net1", "ifname": "op0n1", "script": "no", "downscript": "no" });
        let (r, cmd) = tokio::join!(client.netdev_add(tap.clone()), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(
            (&cmd["execute"], &cmd["arguments"]),
            (&json!("netdev_add"), &tap)
        );

        let (r, cmd) = tokio::join!(client.netdev_del("net1"), reply(&mut fake, json!({})));
        r.unwrap();
        assert_eq!(cmd["execute"], "netdev_del");
        assert_eq!(cmd["arguments"], json!({ "id": "net1" }));

        let path = "/machine/peripheral/vmem0-dev";
        let (r, cmd) = tokio::join!(
            client.qom_set(path, "requested-size", json!(1u64 << 30)),
            reply(&mut fake, json!({}))
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "qom-set");
        assert_eq!(
            cmd["arguments"],
            json!({ "path": path, "property": "requested-size", "value": 1u64 << 30 })
        );

        let (r, cmd) = tokio::join!(
            client.qom_get(path, "size"),
            reply(&mut fake, json!(536870912u64))
        );
        assert_eq!(r.unwrap(), json!(536870912u64));
        assert_eq!(cmd["execute"], "qom-get");
        assert_eq!(
            cmd["arguments"],
            json!({ "path": path, "property": "size" })
        );

        let cpus = json!([
            { "cpu-index": 0, "qom-path": "/machine/unattached/device[0]", "thread-id": 1, "target": "x86_64" },
            { "cpu-index": 1, "qom-path": "/machine/peripheral/cpu1", "thread-id": 2, "target": "x86_64" }
        ]);
        let (r, cmd) = tokio::join!(client.query_cpus_fast(), reply(&mut fake, cpus));
        assert_eq!(r.unwrap(), 2);
        assert_eq!(cmd["execute"], "query-cpus-fast");
    }

    /// QEMU 11.1 q35의 `query-hotpluggable-cpus` 모양: 켜진 자리만 `qom-path`가 있다.
    #[tokio::test]
    async fn hotpluggable_cpus_are_parsed() {
        let (client, mut fake, _dir) = setup().await;
        let ret = json!([
            { "props": { "core-id": 1, "thread-id": 0, "socket-id": 0 }, "vcpus-count": 1,
              "type": "max-x86_64-cpu" },
            { "props": { "core-id": 0, "thread-id": 0, "socket-id": 0 }, "vcpus-count": 1,
              "qom-path": "/machine/unattached/device[0]", "type": "max-x86_64-cpu" }
        ]);
        let (cpus, cmd) = tokio::join!(client.query_hotpluggable_cpus(), reply(&mut fake, ret));
        assert_eq!(cmd["execute"], "query-hotpluggable-cpus");
        let slot = |core, qom_path: Option<&str>| HotpluggableCpu {
            kind: "max-x86_64-cpu".into(),
            vcpus_count: 1,
            socket_id: Some(0),
            core_id: Some(core),
            thread_id: Some(0),
            qom_path: qom_path.map(Into::into),
        };
        assert_eq!(
            cpus.unwrap(),
            [
                slot(1, None),
                slot(0, Some("/machine/unattached/device[0]"))
            ]
        );
        let (cpus, _) = tokio::join!(
            client.query_hotpluggable_cpus(),
            reply(&mut fake, json!([{ "props": {} }]))
        );
        assert!(matches!(cpus, Err(QmpError::Protocol(_))), "{cpus:?}");
    }

    /// `DEVICE_DELETED`가 `device_del`의 답보다 먼저 와도 잡고, 다른 장치의 이벤트는 무시한다.
    #[tokio::test]
    async fn device_del_wait_catches_the_event_even_before_the_reply() {
        let (client, mut fake, _dir) = setup().await;
        let (r, ()) = tokio::join!(client.device_del_wait("disk1-dev", T), async {
            let cmd = fake.recv().await;
            assert_eq!(cmd["execute"], "device_del");
            assert_eq!(cmd["arguments"], json!({ "id": "disk1-dev" }));
            let deleted = |device: &str| {
                json!({ "event": "DEVICE_DELETED", "timestamp": {},
                        "data": { "device": device, "path": format!("/machine/peripheral/{device}") } })
            };
            fake.send(deleted("nic1")).await;
            fake.send(deleted("disk1-dev")).await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
        });
        assert!(r.unwrap());
    }

    /// 다른 장치의 이벤트만 오고 시간이 넘으면 false다(게스트가 내려 주지 않음).
    #[tokio::test]
    async fn device_del_wait_is_false_when_the_guest_does_not_release() {
        let (client, mut fake, _dir) = setup().await;
        let (r, _fake) = tokio::join!(
            client.device_del_wait("cpu3", Duration::from_millis(200)),
            async {
                let cmd = fake.recv().await;
                fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
                fake.send(json!({ "event": "DEVICE_DELETED", "timestamp": {},
                                  "data": { "device": "cpu2", "path": "/machine/peripheral/cpu2" } }))
                    .await;
                fake
            }
        );
        assert!(!r.unwrap());
    }

    /// 이미 없는 장치(`DeviceNotFound`)는 뗀 것이다. 다른 오류는 그대로 돌려준다.
    #[tokio::test]
    async fn device_del_wait_treats_a_missing_device_as_removed() {
        let (client, mut fake, _dir) = setup().await;
        let (r, ()) = tokio::join!(client.device_del_wait("nic2", T), async {
            fake.answer(
                "device_del",
                json!({ "error": { "class": "DeviceNotFound", "desc": "Device 'nic2' not found" } }),
            )
            .await;
        });
        assert!(r.unwrap());
        let (r, ()) = tokio::join!(client.device_del_wait("nic2", T), async {
            fake.answer(
                "device_del",
                json!({ "error": { "class": "GenericError", "desc": "Bus 'pcie.0' does not support hotplugging" } }),
            )
            .await;
        });
        assert!(matches!(r, Err(QmpError::Command { .. })), "{r:?}");
    }

    /// 앞서 보낸 떼기가 아직 진행 중이라는 오류면 그 이벤트를 기다린다.
    #[tokio::test]
    async fn device_del_wait_keeps_waiting_for_an_unplug_in_progress() {
        let (client, mut fake, _dir) = setup().await;
        let (r, ()) = tokio::join!(client.device_del_wait("disk1-dev", T), async {
            let cmd = fake.recv().await;
            let busy = "Device disk1-dev is already in the process of unplug";
            fake.send(
                json!({ "error": { "class": "GenericError", "desc": busy }, "id": cmd["id"] }),
            )
            .await;
            fake.send(json!({ "event": "DEVICE_DELETED", "timestamp": {},
                              "data": { "device": "disk1-dev" } }))
                .await;
        });
        assert!(r.unwrap());
    }

    /// 이벤트를 놓치면(`Lagged`) `/machine/peripheral`에 장치가 남았는지 직접 본다.
    #[tokio::test]
    async fn device_del_wait_checks_the_device_after_lagging() {
        let (client, mut fake, _dir) = setup().await;
        let (r, ()) = tokio::join!(client.device_del_wait("disk1-dev", T), async {
            let cmd = fake.recv().await;
            for _ in 0..100 {
                job_status(&mut fake, &json!("other"), "running").await;
            }
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            let list = fake.recv().await;
            assert_eq!(list["execute"], "qom-list");
            assert_eq!(list["arguments"], json!({ "path": "/machine/peripheral" }));
            fake.send(json!({ "return": [{ "name": "type", "type": "string" },
                                         { "name": "disk0-dev", "type": "child<virtio-blk-pci>" }],
                              "id": list["id"] }))
                .await;
        });
        assert!(r.unwrap());
    }

    /// `peripheral_ids`는 `/machine/peripheral`의 자식 장치 이름만 준다(`type` 같은 속성은 뺀다).
    #[tokio::test]
    async fn peripheral_ids_list_the_child_devices() {
        let (client, mut fake, _dir) = setup().await;
        let (r, ()) = tokio::join!(client.peripheral_ids(), async {
            let list = fake.recv().await;
            assert_eq!(list["execute"], "qom-list");
            assert_eq!(list["arguments"], json!({ "path": "/machine/peripheral" }));
            fake.send(json!({ "return": [{ "name": "type", "type": "string" },
                                         { "name": "disk0-dev", "type": "child<virtio-blk-pci>" },
                                         { "name": "nic0", "type": "child<virtio-net-pci>" }],
                              "id": list["id"] }))
                .await;
        });
        assert_eq!(r.unwrap(), ["disk0-dev", "nic0"]);
    }

    /// 가짜 QEMU가 명령을 차례로 받아 이름·인자를 확인하고 답한다. 답이 `error`면 QMP 오류로 돌려준다.
    async fn expect_commands(fake: &mut FakeQemu, steps: Vec<(&str, Value, Value)>) {
        for (name, args, ret) in steps {
            let cmd = fake.recv().await;
            assert_eq!(cmd["execute"], name, "{cmd}");
            assert_eq!(cmd["arguments"], args, "{name}");
            let mut reply = match ret.get("error") {
                Some(_) => ret,
                None => json!({ "return": ret }),
            };
            reply["id"] = cmd["id"].clone();
            fake.send(reply).await;
        }
    }

    fn nodes(names: &[&str]) -> Value {
        names
            .iter()
            .map(|n| json!({ "node-name": n, "drv": "raw" }))
            .collect()
    }

    fn open_file(path: &str, node: &str) -> (&'static str, Value, Value) {
        (
            "blockdev-add",
            json!({ "driver": "file", "filename": path, "node-name": format!("{node}-file"), "read-only": true }),
            json!({}),
        )
    }

    fn open_raw(node: &str) -> (&'static str, Value, Value) {
        (
            "blockdev-add",
            json!({ "driver": "raw", "file": format!("{node}-file"), "node-name": node, "read-only": true }),
            json!({}),
        )
    }

    fn tray(cmd: &'static str) -> (&'static str, Value, Value) {
        let args = match cmd {
            "blockdev-open-tray" => json!({ "id": "cd1-dev", "force": true }),
            _ => json!({ "id": "cd1-dev" }),
        };
        (cmd, args, json!({}))
    }

    fn insert(node: &str) -> (&'static str, Value, Value) {
        (
            "blockdev-insert-medium",
            json!({ "id": "cd1-dev", "node-name": node }),
            json!({}),
        )
    }

    fn del(node: &str) -> (&'static str, Value, Value) {
        ("blockdev-del", json!({ "node-name": node }), json!({}))
    }

    /// 빈 드라이브에 넣기: 노드를 연 뒤 트레이를 강제로 열고, 빈 매체를 빼고, 넣고, 닫는다.
    #[tokio::test]
    async fn change_medium_inserts_into_an_empty_drive() {
        let (client, mut fake, _dir) = setup().await;
        let flat = json!({ "flat": true });
        let (r, ()) = tokio::join!(
            change_medium(&client, 1, Some(Path::new("/pools/iso/a.raw"))),
            expect_commands(
                &mut fake,
                vec![
                    (
                        "query-named-block-nodes",
                        flat,
                        nodes(&["disk0", "disk0-file", "cd10", "cd10-file"])
                    ),
                    open_file("/pools/iso/a.raw", "cd1"),
                    open_raw("cd1"),
                    tray("blockdev-open-tray"),
                    tray("blockdev-remove-medium"),
                    insert("cd1"),
                    tray("blockdev-close-tray"),
                ],
            )
        );
        r.unwrap();
    }

    /// 매체 바꾸기: 새 매체를 쓰지 않은 이름(`cd1-1`, 그다음 `cd1`)의 노드로 먼저 열고, 트레이를 열어 바꿔 넣은 뒤
    /// 옛 노드를 닫는다(트레이는 한 번만 연다).
    #[tokio::test]
    async fn change_medium_replaces_the_medium() {
        let (client, mut fake, _dir) = setup().await;
        let flat = json!({ "flat": true });
        let (r, ()) = tokio::join!(
            change_medium(&client, 1, Some(Path::new("/pools/iso/b.raw"))),
            expect_commands(
                &mut fake,
                vec![
                    (
                        "query-named-block-nodes",
                        flat.clone(),
                        nodes(&["disk0", "cd1", "cd1-file"])
                    ),
                    open_file("/pools/iso/b.raw", "cd1-1"),
                    open_raw("cd1-1"),
                    tray("blockdev-open-tray"),
                    tray("blockdev-remove-medium"),
                    insert("cd1-1"),
                    del("cd1"),
                    del("cd1-file"),
                    tray("blockdev-close-tray"),
                ],
            )
        );
        r.unwrap();
        let (r, ()) = tokio::join!(
            change_medium(&client, 1, Some(Path::new("/pools/iso/a.raw"))),
            expect_commands(
                &mut fake,
                vec![
                    (
                        "query-named-block-nodes",
                        flat,
                        nodes(&["disk0", "cd1-1", "cd1-1-file"])
                    ),
                    open_file("/pools/iso/a.raw", "cd1"),
                    open_raw("cd1"),
                    tray("blockdev-open-tray"),
                    tray("blockdev-remove-medium"),
                    insert("cd1"),
                    del("cd1-1"),
                    del("cd1-1-file"),
                    tray("blockdev-close-tray"),
                ],
            )
        );
        r.unwrap();
    }

    /// 빼기: 매체를 빼고 노드를 닫고 트레이를 닫아 빈 드라이브로 둔다. 앞서 실패하고 남은 file 노드만 있어도 닫고,
    /// 노드를 닫지 못해도 트레이는 닫는다.
    #[tokio::test]
    async fn change_medium_ejects_and_closes_leftover_nodes() {
        let (client, mut fake, _dir) = setup().await;
        let flat = json!({ "flat": true });
        let busy = json!({ "error": { "class": "GenericError", "desc": "Node cd1 is in use" } });
        let (r, ()) = tokio::join!(
            change_medium(&client, 1, None),
            expect_commands(
                &mut fake,
                vec![
                    (
                        "query-named-block-nodes",
                        flat.clone(),
                        nodes(&["cd1", "cd1-file"])
                    ),
                    tray("blockdev-open-tray"),
                    tray("blockdev-remove-medium"),
                    ("blockdev-del", json!({ "node-name": "cd1" }), busy),
                    del("cd1-file"),
                    tray("blockdev-close-tray"),
                ],
            )
        );
        r.unwrap();
        let (r, ()) = tokio::join!(
            change_medium(&client, 1, None),
            expect_commands(
                &mut fake,
                vec![
                    ("query-named-block-nodes", flat, nodes(&["cd1-2-file"])),
                    tray("blockdev-open-tray"),
                    tray("blockdev-remove-medium"),
                    del("cd1-2-file"),
                    tray("blockdev-close-tray"),
                ],
            )
        );
        r.unwrap();
    }

    /// 새 매체를 열지 못하면 연 노드만 닫고 트레이는 건드리지 않는다(넣은 매체 그대로). 넣기가 실패하면 연 노드를
    /// 닫고 트레이를 닫은 뒤 처음 오류를 돌려준다(정리 오류는 가리지 않는다).
    #[tokio::test]
    async fn change_medium_cleans_up_after_a_failure() {
        let (client, mut fake, _dir) = setup().await;
        let flat = json!({ "flat": true });
        let failed = json!({ "error": { "class": "GenericError", "desc": "Could not open" } });
        let gone = json!({ "error": { "class": "GenericError", "desc": "Failed to find node" } });
        let (r, ()) = tokio::join!(
            change_medium(&client, 1, Some(Path::new("/pools/iso/a.raw"))),
            expect_commands(
                &mut fake,
                vec![
                    (
                        "query-named-block-nodes",
                        flat.clone(),
                        nodes(&["cd1", "cd1-file"])
                    ),
                    open_file("/pools/iso/a.raw", "cd1-1"),
                    ("blockdev-add", open_raw("cd1-1").1, failed.clone()),
                    ("blockdev-del", json!({ "node-name": "cd1-1" }), gone),
                    del("cd1-1-file"),
                ],
            )
        );
        match r.unwrap_err() {
            QmpError::Command { desc, .. } => assert_eq!(desc, "Could not open"),
            other => panic!("unexpected error: {other:?}"),
        }
        let (r, ()) = tokio::join!(
            change_medium(&client, 1, Some(Path::new("/pools/iso/a.raw"))),
            expect_commands(
                &mut fake,
                vec![
                    ("query-named-block-nodes", flat, nodes(&[])),
                    open_file("/pools/iso/a.raw", "cd1"),
                    open_raw("cd1"),
                    tray("blockdev-open-tray"),
                    tray("blockdev-remove-medium"),
                    ("blockdev-insert-medium", insert("cd1").1, failed),
                    del("cd1"),
                    del("cd1-file"),
                    tray("blockdev-close-tray"),
                ],
            )
        );
        match r.unwrap_err() {
            QmpError::Command { desc, .. } => assert_eq!(desc, "Could not open"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 트레이가 열린 장치인지(`query-block`의 `tray_open`)
    #[tokio::test]
    async fn query_tray_open_reads_the_device() {
        let (client, mut fake, _dir) = setup().await;
        let ret = json!([
            { "device": "", "qdev": "/machine/peripheral/cd0-dev", "removable": true, "tray_open": true },
            { "device": "", "qdev": "cd1-dev", "removable": true, "tray_open": false }
        ]);
        for (dev, want) in [("cd0-dev", true), ("cd1-dev", false), ("cd2-dev", false)] {
            let (r, cmd) = tokio::join!(client.query_tray_open(dev), reply(&mut fake, ret.clone()));
            assert_eq!(cmd["execute"], "query-block");
            assert_eq!(r.unwrap(), want, "{dev}");
        }
    }

    /// `query-block`: qdev id(경로면 장치 id만)와 매체 파일. 빈 드라이브는 None, 장치 없는 백엔드는 뺀다.
    #[tokio::test]
    async fn query_block_media_reads_devices_and_files() {
        let (client, mut fake, _dir) = setup().await;
        let ret = json!([
            { "device": "", "qdev": "/machine/peripheral/disk0-dev/virtio-backend", "removable": false,
              "inserted": { "file": "/pools/local/a.qcow2", "node-name": "disk0", "ro": false } },
            { "device": "", "qdev": "cd0-dev", "removable": true, "tray_open": false,
              "inserted": { "file": "/pools/iso/u.raw", "node-name": "cd0", "ro": true } },
            { "device": "", "qdev": "/machine/peripheral/cd1-dev", "removable": true, "tray_open": false },
            { "device": "", "removable": false }
        ]);
        let (r, cmd) = tokio::join!(client.query_block_media(), reply(&mut fake, ret));
        assert_eq!(cmd["execute"], "query-block");
        assert_eq!(
            r.unwrap(),
            [
                (
                    "disk0-dev".to_string(),
                    Some(PathBuf::from("/pools/local/a.qcow2"))
                ),
                (
                    "cd0-dev".to_string(),
                    Some(PathBuf::from("/pools/iso/u.raw"))
                ),
                ("cd1-dev".to_string(), None),
            ]
        );
    }
}
