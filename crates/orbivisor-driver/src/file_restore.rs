//! 파일 단위 복원 도우미 VM(계획 69 설계 §7·§8.4·§9·§10). 백업 디스크를 읽기 전용 원본 위 버리는 덮개로 붙인 일회용
//! 마이크로 VM을 띄우고([`start_helper`]), 그 안 에이전트와 virtio-serial로 말한다([`HelperClient`] — 흐름마다 한도·시간,
//! 게스트가 보낸 것은 [`proto::StreamCheck`]로 검사). 끝은 [`stop_helper`](에이전트 `Shutdown` → SIGTERM → SIGKILL →
//! scope·디렉터리 치우기), opld가 다시 뜨면 [`sweep_leftovers`]가 남은 것을 치운다. 도우미 이미지는 노드가
//! [`check_image`]로 믿고 [`FileRestoreState`]가 `NodeStatus.file_restore`로 알린다. 가짜 하이퍼바이저의 도우미는
//! [`fake_helper`](에이전트 몸통 + 메모리 나무)다.
//!
//! 파일·디렉터리(세션 `id`마다):
//! - `{run_dir}/file-restore/<id>/`(0700): `ctl.sock`(에이전트 채널 — QEMU가 서버), `nbd.sock`(저장소 백업을 내보내는 opld
//!   NBD 서버 자리 — 세션이 띄운다), `qemu.pid`
//! - `{state_dir}/file-restore/<id>/`(0700): `ov{i}.qcow2`(덮개), `console.log`(직렬 콘솔, 1 MiB에서 자름), `qemu.log`(QEMU
//!   표준 오류)

use std::collections::{BTreeMap, HashMap};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use orbivisor_qemu::{
    HelperDisk, HelperDrop, HostProfile, LiveRestoreSource, RestoreHelperArgs,
    build_restore_helper_cmdline,
};
use orbivisor_restore::proto::{
    self, AgentError, Entry, EntryKind, Event, Frame, FrameDecoder, FrameKind, Hello, Name, Reply,
    Request, StreamCheck, Volume,
};
use orbivisor_types::file_restore::{
    self as fr, EntryView, FileRestoreNode, PartitionInfo, VolumeView,
};
use orbivisor_types::vm::Arch;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::DriverError;

/// `run_dir`·`state_dir` 아래 파일 복원 디렉터리 이름
pub const SUBDIR: &str = "file-restore";
/// 도우미 QEMU의 systemd scope 이름 앞(`opl-fr-<id>.scope`)
pub const UNIT_PREFIX: &str = "opl-fr-";
/// 콘솔 로그 상한(넘으면 지킴이가 비운다 — 게스트가 쓴 내용이라 디스크를 채우지 못하게)
pub const CONSOLE_MAX: u64 = 1 << 20;
/// 덮개·콘솔을 보는 간격(§9.4). 부팅 중(`Hello` 전)에도 본다 — 파티션 표·저널은 부팅 때 이미 해석된다.
pub const GUARD_EVERY: Duration = Duration::from_secs(2);
/// 끝낼 때 에이전트 `Shutdown`·SIGTERM 뒤 기다리는 시간
const STOP_WAIT: Duration = Duration::from_secs(5);
/// 유닉스 소켓 경로 상한(macOS `sun_path` 104바이트, NUL 포함)
const MAX_SOCKET_PATH: usize = 103;
/// 받는 쪽이 이만큼 받아 쓰면 몫을 돌려준다(창의 1/4 — 작은 틀마다 `Ack`를 보내지 않게). 받을 것이 없어 기다리기
/// 전에는 남은 것을 모두 돌려준다(에이전트가 몫을 기다리는 채 서로 기다리지 않게).
const ACK_EVERY: u64 = proto::STREAM_WINDOW / 4;
/// QEMU 탐침 한도
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// manifest.json 크기 상한
const MAX_MANIFEST: u64 = 64 << 10;

// ---------------------------------------------------------------------------------------------------------------
// 설정
// ---------------------------------------------------------------------------------------------------------------

/// 에이전트 채널 클라이언트의 한도(§9.2·§10.3)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientOptions {
    /// 요청의 첫 답과 데이터 사이를 기다리는 한도(넘으면 그 흐름을 취소)
    pub reply_timeout: Duration,
    /// 세션 하나의 동시 흐름(넘으면 [`HelperError::Busy`])
    pub max_streams: usize,
    /// 걷기 한 번의 항목 상한(`download_max_entries` — 건너뛴 항목도 셈)
    pub max_entries: u64,
    /// 걷기 한 번의 경로 바이트 합 상한(아카이브가 들고 있는 이름을 바이트로 묶음)
    pub max_path_bytes: u64,
    /// 시간 초과가 이만큼 이어지면 세션을 닫는다
    pub max_strikes: u32,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            reply_timeout: Duration::from_secs(60),
            max_streams: 4,
            max_entries: 1_000_000,
            max_path_bytes: proto::DEFAULT_MAX_PATH_BYTES,
            max_strikes: 3,
        }
    }
}

/// 도우미 VM 설정(opld `[file_restore]`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperConfig {
    /// 덮개를 만드는 `qemu-img`
    pub qemu_img: PathBuf,
    pub memory_mib: u32,
    pub vcpus: u32,
    /// QEMU 시작부터 에이전트 `Hello`까지
    pub boot_timeout: Duration,
    /// 덮개 합계 상한(바이트, 넘으면 세션을 닫음)
    pub overlay_max: u64,
    /// QEMU가 블록 장치·소켓을 연 뒤 내려갈 사용자(Linux에서 opld가 root일 때만, `None`이면 내리지 않음)
    pub run_as: Option<String>,
    pub client: ClientOptions,
}

impl Default for HelperConfig {
    fn default() -> Self {
        HelperConfig {
            qemu_img: PathBuf::from("qemu-img"),
            memory_mib: 512,
            vcpus: 2,
            boot_timeout: Duration::from_secs(300),
            overlay_max: 2048 << 20,
            run_as: Some("nobody".into()),
            client: ClientOptions::default(),
        }
    }
}

/// 세션 한도(opld `[file_restore]`, 세션·되돌려 쓰기 Task가 쓴다)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    /// 노드당 세션 수(0이면 이 노드는 파일 복원을 알리지 않음)
    pub max_sessions: u32,
    /// 나열·내려받기·되돌려 쓰기가 없으면 닫는 시간
    pub idle: Duration,
    /// 세션 최대 시간
    pub max: Duration,
    /// 게스트로 되돌려 쓰기 한 번의 바이트·파일 수 상한
    pub guest_max_bytes: u64,
    pub guest_max_files: u64,
}

impl Default for SessionLimits {
    fn default() -> Self {
        SessionLimits {
            max_sessions: 4,
            idle: Duration::from_secs(15 * 60),
            max: Duration::from_secs(8 * 3600),
            guest_max_bytes: 4 << 30,
            guest_max_files: 10_000,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 이미지 검사(§8.4)
// ---------------------------------------------------------------------------------------------------------------

/// 검사를 지난 도우미 이미지
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedImage {
    pub arch: Arch,
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    /// `orbivisor <판> lock-<해시>` — 에이전트 `Hello`와 같아야 한다
    pub image_version: String,
    pub kernel_release: String,
}

#[derive(Deserialize)]
struct Manifest {
    arch: Arch,
    files: BTreeMap<String, String>,
    image_version: String,
    initrd: String,
    kernel: String,
    #[serde(default)]
    kernel_release: String,
    proto: u32,
}

/// 아키텍처의 이미지 디렉터리 이름(`x86_64`·`aarch64`)
pub fn arch_dir(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    }
}

/// 이미지 `manifest.json`의 경로
pub fn manifest_path(image_dir: &Path, arch: Arch) -> PathBuf {
    image_dir.join(arch_dir(arch)).join("manifest.json")
}

/// manifest의 (크기, 수정 시각) — 바뀌면 다시 검사한다. 없으면 None
pub fn manifest_stamp(image_dir: &Path, arch: Arch) -> Option<(u64, Option<SystemTime>)> {
    let meta = std::fs::metadata(manifest_path(image_dir, arch)).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

fn plain_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains('/') && !s.contains('\0')
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// `image_dir/<arch>/manifest.json`을 읽어 아키텍처·통신 규약 판·이미지 판(`orbivisor <build> lock-…`)을 보고 커널·initrd
/// 해시를 맞춰 본다(수십 MB 읽기 — `spawn_blocking`에서). 어긋나면 까닭이다.
pub fn check_image(image_dir: &Path, arch: Arch, build: &str) -> Result<CheckedImage, String> {
    let path = manifest_path(image_dir, arch);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "no helper image in {}",
                path.parent().unwrap_or(&path).display()
            ));
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut text = Vec::new();
    file.take(MAX_MANIFEST + 1)
        .read_to_end(&mut text)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if text.len() as u64 > MAX_MANIFEST {
        return Err(format!("{} is too large", path.display()));
    }
    let m: Manifest =
        serde_json::from_slice(&text).map_err(|e| format!("bad {}: {e}", path.display()))?;
    if m.arch != arch {
        return Err(format!(
            "the helper image in {} is for {} (this node is {})",
            path.display(),
            arch_dir(m.arch),
            arch_dir(arch)
        ));
    }
    if m.proto != proto::PROTO_VERSION {
        return Err(format!(
            "the helper image speaks protocol {} (this orbivisord speaks {})",
            m.proto,
            proto::PROTO_VERSION
        ));
    }
    if m.image_version.len() > proto::MAX_TEXT
        || !m
            .image_version
            .starts_with(&format!("orbivisor {build} lock-"))
    {
        return Err(format!(
            "the helper image version {:?} does not match this orbivisord ({build}); install the image of the same version",
            m.image_version.chars().take(200).collect::<String>()
        ));
    }
    let dir = path.parent().unwrap_or(image_dir);
    let mut files = Vec::new();
    for name in [&m.kernel, &m.initrd] {
        if !plain_name(name) {
            return Err(format!("bad file name {name:?} in {}", path.display()));
        }
        let want = m
            .files
            .get(name)
            .ok_or_else(|| format!("{} has no hash for {name}", path.display()))?;
        let file = dir.join(name);
        let got = sha256_file(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        if !got.eq_ignore_ascii_case(want) {
            return Err(format!(
                "{} does not match its hash in the manifest",
                file.display()
            ));
        }
        files.push(file);
    }
    let initrd = files.pop().expect("two files");
    let kernel = files.pop().expect("two files");
    Ok(CheckedImage {
        arch,
        kernel,
        initrd,
        image_version: m.image_version,
        kernel_release: m.kernel_release.chars().take(proto::MAX_TEXT).collect(),
    })
}

// ---------------------------------------------------------------------------------------------------------------
// 노드 상태(§8.4·§10.3)
// ---------------------------------------------------------------------------------------------------------------

struct ImageCheck {
    stamp: Option<(u64, Option<SystemTime>)>,
    result: Result<CheckedImage, String>,
}

/// 노드의 파일 복원 상태: 설정, 검사한 이미지, 열린 세션 수. opld가 만들어 이미지를 검사하고 `NodeStatus.file_restore`로
/// 알리며([`FileRestoreState::node`]), 세션 Task가 자리를 쥔다([`FileRestoreState::try_open`]).
pub struct FileRestoreState {
    pub image_dir: PathBuf,
    pub arch: Arch,
    /// 도우미 가속기(`kvm`·`hvf`·`tcg`)
    pub accel: String,
    /// 이미지 판이 맞아야 하는 orbivisord 빌드 판
    pub build: String,
    pub helper: HelperConfig,
    pub limits: SessionLimits,
    image: Mutex<Option<ImageCheck>>,
    sessions: watch::Sender<u32>,
}

impl std::fmt::Debug for FileRestoreState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRestoreState")
            .field("image_dir", &self.image_dir)
            .field("arch", &self.arch)
            .field("accel", &self.accel)
            .field("sessions", &self.sessions())
            .finish()
    }
}

impl FileRestoreState {
    pub fn new(
        image_dir: PathBuf,
        host: &HostProfile,
        build: &str,
        helper: HelperConfig,
        limits: SessionLimits,
    ) -> Self {
        FileRestoreState {
            image_dir,
            arch: host.arch,
            accel: host.accel.as_str().into(),
            build: build.into(),
            helper,
            limits,
            image: Mutex::new(None),
            sessions: watch::channel(0).0,
        }
    }

    /// 처음이거나 manifest가 바뀌었으면 이미지를 다시 검사한다(무거우면 blocking 스레드에서). 결과가 바뀌었으면 참.
    pub async fn refresh_image(self: &Arc<Self>) -> bool {
        let stamp = manifest_stamp(&self.image_dir, self.arch);
        {
            let g = self.image.lock().unwrap();
            if g.as_ref().is_some_and(|c| c.stamp == stamp) {
                return false;
            }
        }
        let me = self.clone();
        let result =
            tokio::task::spawn_blocking(move || check_image(&me.image_dir, me.arch, &me.build))
                .await
                .unwrap_or_else(|e| Err(format!("image check failed: {e}")));
        match &result {
            Ok(img) => {
                tracing::info!(version = %img.image_version, "file restore helper image checked")
            }
            Err(e) => tracing::warn!("file restore disabled: {e}"),
        }
        let mut g = self.image.lock().unwrap();
        let changed = g.as_ref().map(|c| &c.result) != Some(&result);
        *g = Some(ImageCheck { stamp, result });
        changed
    }

    /// 검사 결과를 바로 둔다(시험과 가짜 하이퍼바이저 — 이미지 파일 없이 세션을 띄운다). manifest가 그대로면 다시 검사하지
    /// 않는다.
    pub fn set_checked_image(&self, image: CheckedImage) {
        let stamp = manifest_stamp(&self.image_dir, self.arch);
        *self.image.lock().unwrap() = Some(ImageCheck {
            stamp,
            result: Ok(image),
        });
    }

    /// 검사를 지난 이미지
    pub fn image(&self) -> Option<CheckedImage> {
        self.image
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|c| c.result.clone().ok())
    }

    /// 이미지를 쓸 수 없는 까닭(아직 검사 전이면 None)
    pub fn image_error(&self) -> Option<String> {
        self.image
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|c| c.result.clone().err())
    }

    /// `NodeStatus.file_restore`에 쓸 값: 이미지가 검사를 지났고 세션 상한이 0이 아닐 때만
    pub fn node(&self) -> Option<FileRestoreNode> {
        if self.limits.max_sessions == 0 {
            return None;
        }
        let image = self.image()?;
        Some(FileRestoreNode {
            image_version: image.image_version,
            arch: self.arch,
            accel: self.accel.clone(),
            sessions: *self.sessions.borrow(),
            max_sessions: self.limits.max_sessions,
        })
    }

    /// 열린 세션 수
    pub fn sessions(&self) -> u32 {
        *self.sessions.borrow()
    }

    /// 세션 수가 바뀌면 깨는 수신기(노드 상태를 다시 쓴다)
    pub fn subscribe(&self) -> watch::Receiver<u32> {
        self.sessions.subscribe()
    }

    /// 세션 자리 하나를 쥔다(상한이면 None). 자리를 버리면 수가 준다.
    pub fn try_open(self: &Arc<Self>) -> Option<SessionSlot> {
        let max = self.limits.max_sessions;
        let mut ok = false;
        self.sessions.send_if_modified(|n| {
            ok = *n < max;
            if ok {
                *n += 1;
            }
            ok
        });
        ok.then(|| SessionSlot {
            state: self.clone(),
        })
    }
}

/// 쥔 세션 자리([`FileRestoreState::try_open`])
pub struct SessionSlot {
    state: Arc<FileRestoreState>,
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        self.state
            .sessions
            .send_modify(|n| *n = n.saturating_sub(1));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 보기 변환
// ---------------------------------------------------------------------------------------------------------------

/// 에이전트의 볼륨 → API 보기(상태는 opld가 든다)
pub fn volume_view(v: &Volume, state: fr::VolumeState) -> VolumeView {
    VolumeView {
        id: v.id.clone(),
        disk: v.disk,
        kind: match v.kind {
            proto::VolumeKind::Disk => fr::VolumeKind::Disk,
            proto::VolumeKind::Partition => fr::VolumeKind::Partition,
            proto::VolumeKind::LogicalVolume => fr::VolumeKind::LogicalVolume,
        },
        size: v.size,
        partition: v.partition.as_ref().map(|p| PartitionInfo {
            table: match p.table {
                proto::PartitionTable::Mbr => fr::PartitionTable::Mbr,
                proto::PartitionTable::Gpt => fr::PartitionTable::Gpt,
            },
            number: p.number,
            type_name: p.type_name.clone(),
            uuid: p.uuid.clone(),
        }),
        fs: v.fs.clone(),
        label: v.label.clone(),
        uuid: v.uuid.clone(),
        mountable: v.mountable,
        state,
        note: v.note.clone(),
    }
}

/// 에이전트의 항목 → API 보기(이름은 UTF-8이 아니면 대체 글자 + 원래 바이트 base64)
pub fn entry_view(e: &Entry) -> EntryView {
    let raw = std::str::from_utf8(e.name.as_bytes())
        .is_err()
        .then(|| STANDARD.encode(e.name.as_bytes()));
    EntryView {
        name: e.name.lossy(),
        raw,
        kind: match e.attrs.kind {
            EntryKind::File => fr::EntryKind::File,
            EntryKind::Dir => fr::EntryKind::Dir,
            EntryKind::Symlink => fr::EntryKind::Symlink,
            EntryKind::Other => fr::EntryKind::Other,
        },
        size: e.attrs.size,
        mtime: e.attrs.mtime,
        mode: e.attrs.mode,
        uid: e.attrs.uid,
        gid: e.attrs.gid,
        link_target: e
            .link_target
            .as_ref()
            .map(|t| String::from_utf8_lossy(t.as_bytes()).into_owned()),
    }
}

/// 나열 커서(이름 바이트) → `ListPage.next`(URL에 그대로 쓰는 base64)
pub fn encode_cursor(name: &Name) -> String {
    URL_SAFE_NO_PAD.encode(name.as_bytes())
}

/// [`encode_cursor`]의 거꾸로(잘못된 값이면 None)
pub fn decode_cursor(s: &str) -> Option<Name> {
    Name::new(URL_SAFE_NO_PAD.decode(s).ok()?).ok()
}

/// API 경로 조각 → 규약 이름
pub fn path_names(path: &fr::EntryPath) -> Option<Vec<Name>> {
    path.parts()
        .iter()
        .map(|p| Name::new(p.as_bytes().to_vec()).ok())
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// 에이전트 채널 클라이언트(§9.2)
// ---------------------------------------------------------------------------------------------------------------

/// 도우미 요청 실패
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelperError {
    /// 도우미가 그 요청을 거절·실패했다(`not_found`, `not_mountable` …)
    #[error("{0}")]
    Agent(AgentError),
    /// 이 세션의 동시 요청이 상한이다
    #[error("too many concurrent requests in this file restore session")]
    Busy,
    /// 도우미가 시간 안에 답하지 않았다(그 흐름은 취소했다)
    #[error("the file restore helper did not answer in time")]
    Timeout,
    /// 세션 채널이 닫혔다(도우미가 끝났거나 규약을 어겼다 — 까닭)
    #[error("the file restore session is closed: {0}")]
    Closed(String),
    /// 걷기가 한도(항목 수·경로 바이트)를 넘어 그 흐름만 멈췄다(세션은 그대로)
    #[error("{0}")]
    Limit(String),
}

/// 흐름에서 받은 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamItem {
    Reply(Reply),
    Data(Vec<u8>),
}

/// 흐름의 사건과 그 틀이 쓴 몫(받는 쪽이 받아 쓰면 돌려준다)
type Sink = mpsc::UnboundedSender<(Result<Event, HelperError>, u64)>;

struct Slot {
    check: StreamCheck,
    tx: Option<Sink>,
    /// 받았으나 몫을 돌려주지 않은 바이트(창을 넘으면 에이전트가 규약을 어긴 것)
    queued: u64,
    /// 한도로 끊어 취소한 흐름: 끝 틀이 올 때까지 틀을 버린다
    draining: bool,
    _permit: Option<OwnedSemaphorePermit>,
}

#[derive(Default)]
struct Table {
    next: u32,
    streams: HashMap<u32, Slot>,
}

struct Inner {
    writer: tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    table: Mutex<Table>,
    permits: Arc<Semaphore>,
    closed: watch::Sender<Option<String>>,
    strikes: AtomicU32,
    opts: ClientOptions,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(r) = self.reader.lock().unwrap().take() {
            r.abort();
        }
    }
}

/// 도우미 에이전트 채널의 opld 쪽. 요청마다 흐름 하나(세션당 [`ClientOptions::max_streams`]), 답은 흐름마다 검사하고,
/// 규약 위반·채널 끊김은 세션을 닫는다(모든 흐름이 [`HelperError::Closed`]). 값싸게 복제된다.
#[derive(Clone)]
pub struct HelperClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for HelperClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperClient")
            .field("closed", &*self.inner.closed.borrow())
            .finish()
    }
}

impl HelperClient {
    /// 채널에서 에이전트의 `Hello`를 `hello_timeout`까지 기다려 검사하고(통신 규약 판·글자 길이) 클라이언트를 만든다.
    pub async fn connect<S>(
        stream: S,
        hello_timeout: Duration,
        opts: ClientOptions,
    ) -> Result<(HelperClient, Hello), HelperError>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (mut r, w) = tokio::io::split(stream);
        let mut dec = FrameDecoder::new();
        let first = tokio::time::timeout(hello_timeout, async {
            let mut buf = vec![0u8; 64 << 10];
            loop {
                match dec.next_frame() {
                    Ok(Some(f)) => return Ok(f),
                    Ok(None) => {}
                    Err(e) => {
                        return Err(HelperError::Closed(format!(
                            "agent protocol violation: {e}"
                        )));
                    }
                }
                match r.read(&mut buf).await {
                    Ok(0) => {
                        return Err(HelperError::Closed(
                            "the helper closed the channel before saying hello".into(),
                        ));
                    }
                    Ok(n) => dec.push(&buf[..n]),
                    Err(e) => {
                        return Err(HelperError::Closed(format!(
                            "cannot read the helper channel: {e}"
                        )));
                    }
                }
            }
        })
        .await
        .map_err(|_| HelperError::Timeout)??;
        let hello = match (first.stream, first.kind, first.parse::<Reply>()) {
            (0, FrameKind::Resp, Ok(Reply::Hello(h))) => h,
            _ => {
                return Err(HelperError::Closed(
                    "agent protocol violation: the first frame is not a hello".into(),
                ));
            }
        };
        proto::check_hello(&hello).map_err(|v| HelperError::Closed(v.to_string()))?;
        let inner = Arc::new(Inner {
            writer: tokio::sync::Mutex::new(Box::new(w)),
            table: Mutex::new(Table::default()),
            permits: Arc::new(Semaphore::new(opts.max_streams)),
            closed: watch::channel(None).0,
            strikes: AtomicU32::new(0),
            opts,
            reader: Mutex::new(None),
        });
        let reader = tokio::spawn(read_loop(Arc::downgrade(&inner), Box::new(r), dec));
        *inner.reader.lock().unwrap() = Some(reader);
        Ok((HelperClient { inner }, hello))
    }

    /// 닫혔으면 그 까닭
    pub fn closed_reason(&self) -> Option<String> {
        self.inner.closed.borrow().clone()
    }

    /// 닫힐 때까지 기다려 까닭을 돌려준다
    pub async fn closed(&self) -> String {
        let mut rx = self.inner.closed.subscribe();
        match rx.wait_for(Option::is_some).await {
            Ok(r) => r.clone().unwrap_or_default(),
            Err(_) => "the helper channel is gone".into(),
        }
    }

    /// 세션을 닫는다(이미 닫혔으면 앞 까닭을 둔다). 도는 흐름은 모두 [`HelperError::Closed`]로 끝난다.
    pub fn close(&self, reason: impl Into<String>) {
        self.inner.close(reason.into());
    }

    /// 지금 도는 요청 흐름 수
    pub fn active_streams(&self) -> usize {
        self.inner.opts.max_streams - self.inner.permits.available_permits()
    }

    async fn open(&self, req: Request, data: bool) -> Result<ReplyStream, HelperError> {
        if let Some(r) = self.closed_reason() {
            return Err(HelperError::Closed(r));
        }
        let permit = if data {
            Some(
                self.inner
                    .permits
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| HelperError::Busy)?,
            )
        } else {
            None
        };
        // 묶지 않은 통로지만 쌓이는 것은 흐름의 몫(STREAM_WINDOW)을 넘지 못한다(넘으면 위반).
        let (tx, rx) = mpsc::unbounded_channel();
        let id = {
            let mut t = self.inner.table.lock().unwrap();
            let mut id = t.next;
            loop {
                id = id.wrapping_add(1);
                if id != 0 && !t.streams.contains_key(&id) {
                    break;
                }
            }
            t.next = id;
            t.streams.insert(
                id,
                Slot {
                    check: StreamCheck::new(&req, self.inner.opts.max_entries)
                        .with_max_path_bytes(self.inner.opts.max_path_bytes),
                    tx: Some(tx),
                    queued: 0,
                    draining: false,
                    _permit: permit,
                },
            );
            id
        };
        if let Err(e) = self.inner.send(Frame::json(id, FrameKind::Req, &req)).await {
            self.inner.table.lock().unwrap().streams.remove(&id);
            return Err(e);
        }
        Ok(ReplyStream {
            id,
            rx,
            client: self.clone(),
            done: false,
            unacked: 0,
        })
    }

    /// 흐름 `stream`을 취소한다(답을 기다리지 않음)
    async fn cancel(&self, stream: u32) {
        if let Ok(mut s) = Box::pin(self.open(Request::Cancel { stream }, false)).await {
            // 취소 흐름은 곧 `End`로 끝난다. 기다리지 않고 표에서 떼어 둔다.
            s.detach();
            s.done = true;
        }
    }

    /// 한 답만 오는 요청
    async fn single(&self, req: Request) -> Result<Reply, HelperError> {
        let mut s = self.open(req, true).await?;
        let reply = match s.next().await? {
            Some(StreamItem::Reply(r)) => r,
            _ => return Err(self.inner.violation("expected a reply")),
        };
        match s.next().await? {
            None => Ok(reply),
            Some(_) => Err(self.inner.violation("expected the end of the stream")),
        }
    }

    /// 디스크·파티션·LVM 볼륨 찾기(검사한 목록)
    pub async fn probe(&self) -> Result<Vec<Volume>, HelperError> {
        match self.single(Request::Probe).await? {
            Reply::Volumes { volumes } => Ok(volumes),
            _ => Err(self.inner.violation("unexpected reply")),
        }
    }

    /// 볼륨을 붙인다(이미 붙었으면 그대로). 덧붙임(저널을 건너뜀 …)
    pub async fn mount(&self, volume: &str) -> Result<Option<String>, HelperError> {
        match self
            .single(Request::Mount {
                volume: volume.into(),
            })
            .await?
        {
            Reply::Mounted { note } => Ok(note),
            _ => Err(self.inner.violation("unexpected reply")),
        }
    }

    /// 디렉터리 한 쪽(이름 바이트 차례, `cursor` 다음부터)과 다음 커서
    pub async fn list(
        &self,
        volume: &str,
        path: Vec<Name>,
        cursor: Option<Name>,
        limit: u32,
    ) -> Result<(Vec<Entry>, Option<Name>), HelperError> {
        let req = Request::List {
            volume: volume.into(),
            path,
            cursor,
            limit: limit.clamp(1, proto::MAX_PAGE_ENTRIES as u32),
        };
        match self.single(req).await? {
            Reply::Page { entries, next } => Ok((entries, next)),
            _ => Err(self.inner.violation("unexpected reply")),
        }
    }

    /// 항목 하나(링크는 따라가지 않음)
    pub async fn stat(&self, volume: &str, path: Vec<Name>) -> Result<Entry, HelperError> {
        match self
            .single(Request::Stat {
                volume: volume.into(),
                path,
            })
            .await?
        {
            Reply::Entry(e) => Ok(e),
            _ => Err(self.inner.violation("unexpected reply")),
        }
    }

    /// 일반 파일 하나: 크기와 데이터 흐름(데이터 틀이 크기만큼 온 뒤 끝)
    pub async fn read(
        &self,
        volume: &str,
        path: Vec<Name>,
    ) -> Result<(u64, ReplyStream), HelperError> {
        let mut s = self
            .open(
                Request::Read {
                    volume: volume.into(),
                    path,
                },
                true,
            )
            .await?;
        match s.next().await? {
            Some(StreamItem::Reply(Reply::FileHead { size })) => Ok((size, s)),
            _ => Err(self.inner.violation("unexpected reply")),
        }
    }

    /// 고른 항목들을 깊이 우선으로(항목·데이터·파일 오류·건너뜀). `hardlinks`면 tar용 하드 링크 알림
    pub async fn walk(
        &self,
        volume: &str,
        paths: Vec<Vec<Name>>,
        hardlinks: bool,
    ) -> Result<ReplyStream, HelperError> {
        self.open(
            Request::Walk {
                volume: volume.into(),
                paths,
                hardlinks,
            },
            true,
        )
        .await
    }

    /// 에이전트에 끝내라고 한다(`End`를 받으면 돌아옴 — 그 뒤 도우미는 전원을 끈다)
    pub async fn shutdown(&self) -> Result<(), HelperError> {
        let mut s = self.open(Request::Shutdown, false).await?;
        match s.next().await? {
            None => Ok(()),
            Some(_) => Err(self.inner.violation("unexpected reply")),
        }
    }
}

impl Inner {
    async fn send(&self, frame: Frame) -> Result<(), HelperError> {
        let bytes = frame.encode();
        let mut w = self.writer.lock().await;
        let r = async {
            w.write_all(&bytes).await?;
            w.flush().await
        }
        .await;
        drop(w);
        r.map_err(|e| {
            let reason = format!("cannot write to the helper channel: {e}");
            self.close(reason.clone());
            HelperError::Closed(self.closed.borrow().clone().unwrap_or(reason))
        })
    }

    /// 규약 위반으로 세션을 닫는다
    fn violation(&self, what: &str) -> HelperError {
        let reason = format!("agent protocol violation: {what}");
        self.close(reason.clone());
        HelperError::Closed(reason)
    }

    fn close(&self, reason: String) {
        let first = self.closed.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason.clone());
                true
            } else {
                false
            }
        });
        if first {
            tracing::debug!(%reason, "file restore helper channel closed");
        }
        let reason = self.closed.borrow().clone().unwrap_or(reason);
        let streams = std::mem::take(&mut self.table.lock().unwrap().streams);
        for (_, slot) in streams {
            if let Some(tx) = slot.tx {
                let _ = tx.send((Err(HelperError::Closed(reason.clone())), 0));
            }
        }
    }

    fn detach(&self, id: u32) {
        if let Some(slot) = self.table.lock().unwrap().streams.get_mut(&id) {
            slot.tx = None;
        }
    }

    /// 받은 틀 하나를 그 흐름으로(기다리지 않음 — 받는 쪽이 느려도 다른 흐름을 막지 않는다. 쌓이는 것은 흐름의 몫까지).
    /// 규약 위반이면 까닭. 걷기 한도를 넘으면 그 흐름만 [`HelperError::Limit`]으로 끝내고 취소한다.
    fn dispatch(self: &Arc<Self>, frame: Frame) -> Result<(), String> {
        let id = frame.stream;
        if id == 0 {
            return Err("agent protocol violation: unexpected frame on stream 0".into());
        }
        let cost = frame.cost();
        let last = matches!(frame.kind, FrameKind::End | FrameKind::Err);
        let mut t = self.table.lock().unwrap();
        let Some(slot) = t.streams.get_mut(&id) else {
            return Err(format!(
                "agent protocol violation: frame for unknown stream {id}"
            ));
        };
        slot.queued += cost;
        if slot.queued > proto::STREAM_WINDOW {
            return Err(format!(
                "agent protocol violation: stream {id} sent more than its window"
            ));
        }
        if slot.draining {
            if last {
                t.streams.remove(&id);
            }
            return Ok(());
        }
        let event = match slot.check.feed(frame) {
            Ok(event) => event,
            Err(v) if slot.check.limit_reached() => {
                slot.draining = true;
                let tx = slot.tx.take();
                drop(t);
                if let Some(tx) = tx {
                    let _ = tx.send((Err(HelperError::Limit(v.0)), 0));
                }
                let client = HelperClient {
                    inner: self.clone(),
                };
                tokio::spawn(async move { client.cancel(id).await });
                return Ok(());
            }
            Err(v) => return Err(v.to_string()),
        };
        let finished = slot.check.done();
        let tx = slot.tx.clone();
        if finished {
            t.streams.remove(&id);
        }
        drop(t);
        if finished {
            self.strikes.store(0, Ordering::Relaxed);
        }
        // 받는 쪽이 흐름을 버렸으면(버릴 때 취소를 보냈다) 떼어 둔다.
        if let Some(tx) = tx
            && tx.send((Ok(event), cost)).is_err()
        {
            self.detach(id);
        }
        Ok(())
    }

    /// 받는 쪽이 `bytes`를 받아 썼다: 흐름의 몫을 돌려준다(끝난 흐름이면 보내지 않음).
    async fn ack(&self, id: u32, bytes: u64) {
        {
            let mut t = self.table.lock().unwrap();
            let Some(slot) = t.streams.get_mut(&id) else {
                return;
            };
            slot.queued = slot.queued.saturating_sub(bytes);
        }
        // 실패하면 세션이 닫힌다(send가 닫음) — 받는 쪽은 다음 next에서 그 까닭을 받는다.
        let _ = self
            .send(Frame::ack(id, bytes.min(u64::from(u32::MAX)) as u32))
            .await;
    }
}

async fn read_loop(
    weak: Weak<Inner>,
    mut r: Box<dyn AsyncRead + Send + Unpin>,
    mut dec: FrameDecoder,
) {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        loop {
            let frame = match dec.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => {
                    if let Some(inner) = weak.upgrade() {
                        inner.close(format!("agent protocol violation: {e}"));
                    }
                    return;
                }
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if inner.closed.borrow().is_some() {
                return;
            }
            if let Err(reason) = inner.dispatch(frame) {
                inner.close(reason);
                return;
            }
        }
        let read = r.read(&mut buf).await;
        let Some(inner) = weak.upgrade() else {
            return;
        };
        match read {
            Ok(0) => {
                inner.close("the helper closed the channel".into());
                return;
            }
            Ok(n) => dec.push(&buf[..n]),
            Err(e) => {
                inner.close(format!("cannot read the helper channel: {e}"));
                return;
            }
        }
    }
}

/// 요청 하나의 답 흐름. 다 받기 전에 버리면 에이전트에 취소를 보낸다.
pub struct ReplyStream {
    id: u32,
    rx: mpsc::UnboundedReceiver<(Result<Event, HelperError>, u64)>,
    client: HelperClient,
    done: bool,
    /// 받아 썼으나 아직 돌려주지 않은 몫
    unacked: u64,
}

impl std::fmt::Debug for ReplyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyStream")
            .field("id", &self.id)
            .field("done", &self.done)
            .finish()
    }
}

impl ReplyStream {
    /// 흐름 번호
    pub fn id(&self) -> u32 {
        self.id
    }

    /// 다음 답·데이터(`Progress`는 건너뜀). 성공으로 끝나면 None, 실패면 오류. 첫 답·데이터 사이가
    /// [`ClientOptions::reply_timeout`]을 넘으면 흐름을 취소하고 [`HelperError::Timeout`]이다.
    pub async fn next(&mut self) -> Result<Option<StreamItem>, HelperError> {
        if self.done {
            return Ok(None);
        }
        let opts = self.client.inner.opts;
        loop {
            let got = match self.rx.try_recv() {
                Ok(x) => Ok(Some(x)),
                Err(mpsc::error::TryRecvError::Disconnected) => Ok(None),
                Err(mpsc::error::TryRecvError::Empty) => {
                    // 기다리기 전에 몫을 모두 돌려준다 — 에이전트가 몫을 기다리고 있을 수 있다.
                    self.flush_ack().await;
                    tokio::time::timeout(opts.reply_timeout, self.rx.recv()).await
                }
            };
            let got = got.map(|x| {
                x.map(|(ev, cost)| {
                    self.unacked += cost;
                    ev
                })
            });
            if self.unacked >= ACK_EVERY {
                self.flush_ack().await;
            }
            let ev = match got {
                Err(_) => {
                    self.done = true;
                    self.detach();
                    self.client.cancel(self.id).await;
                    let n = self.client.inner.strikes.fetch_add(1, Ordering::Relaxed) + 1;
                    if n >= opts.max_strikes {
                        self.client.close(format!(
                            "the helper did not answer {n} requests in {}s",
                            opts.reply_timeout.as_secs()
                        ));
                    }
                    return Err(HelperError::Timeout);
                }
                Ok(None) => {
                    self.done = true;
                    let reason = self
                        .client
                        .closed_reason()
                        .unwrap_or_else(|| "the stream was dropped".into());
                    return Err(HelperError::Closed(reason));
                }
                Ok(Some(Err(e))) => {
                    self.done = true;
                    return Err(e);
                }
                Ok(Some(Ok(ev))) => ev,
            };
            return match ev {
                Event::Reply(Reply::Progress { .. }) => continue,
                Event::Reply(r) => Ok(Some(StreamItem::Reply(r))),
                Event::Data(d) => Ok(Some(StreamItem::Data(d))),
                Event::End => {
                    self.done = true;
                    Ok(None)
                }
                Event::Failed(e) => {
                    self.done = true;
                    Err(HelperError::Agent(e))
                }
            };
        }
    }

    fn detach(&self) {
        self.client.inner.detach(self.id);
    }

    async fn flush_ack(&mut self) {
        if self.unacked > 0 {
            let n = std::mem::take(&mut self.unacked);
            self.client.inner.ack(self.id, n).await;
        }
    }
}

impl Drop for ReplyStream {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.detach();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let (client, id) = (self.client.clone(), self.id);
            rt.spawn(async move { client.cancel(id).await });
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 도우미 VM
// ---------------------------------------------------------------------------------------------------------------

/// 도우미 디스크 하나의 원본(읽기 전용)과 가상 크기(덮개를 이 크기로 만든다)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperSource {
    pub source: LiveRestoreSource,
    pub size: u64,
}

/// 도우미 하나 띄우기
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperLaunch {
    /// 세션 id
    pub id: Uuid,
    pub image: CheckedImage,
    /// 백업 디스크 차례(`disk{i}`)
    pub disks: Vec<HelperSource>,
}

/// 세션 하나의 파일 자리
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperPaths {
    /// `{run_dir}/file-restore/<id>`
    pub run: PathBuf,
    /// `{state_dir}/file-restore/<id>`
    pub state: PathBuf,
}

impl HelperPaths {
    pub fn new(run_dir: &Path, state_dir: &Path, id: Uuid) -> Self {
        HelperPaths {
            run: run_dir.join(SUBDIR).join(id.to_string()),
            state: state_dir.join(SUBDIR).join(id.to_string()),
        }
    }

    /// 에이전트 채널 소켓
    pub fn ctl_socket(&self) -> PathBuf {
        self.run.join("ctl.sock")
    }

    /// 저장소 백업을 내보내는 opld NBD 서버의 소켓(세션이 띄운다 — 디렉터리와 함께 치워진다)
    pub fn nbd_socket(&self) -> PathBuf {
        self.run.join("nbd.sock")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.run.join("qemu.pid")
    }

    pub fn console_log(&self) -> PathBuf {
        self.state.join("console.log")
    }

    pub fn qemu_log(&self) -> PathBuf {
        self.state.join("qemu.log")
    }

    pub fn overlay(&self, i: usize) -> PathBuf {
        self.state.join(format!("ov{i}.qcow2"))
    }

    /// 두 디렉터리를 0700으로 만든다(있으면 그대로)
    pub fn prepare(&self) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        for dir in [&self.run, &self.state] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Ok(())
    }

    /// 두 디렉터리를 지운다
    pub fn remove(&self) {
        for dir in [&self.run, &self.state] {
            if let Err(e) = std::fs::remove_dir_all(dir)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(dir = %dir.display(), error = %e, "cannot remove file restore files");
            }
        }
    }
}

/// 띄운 도우미. [`stop_helper`]로 끝낸다(버려도 프로세스는 끝나고 파일은 치워진다).
pub struct RestoreHelper {
    pub id: Uuid,
    pub hello: Hello,
    pub client: HelperClient,
    /// 시작부터 `Hello`까지
    pub boot_time: Duration,
    /// QEMU pid(가짜 도우미는 없음)
    pub pid: Option<u32>,
    ended: watch::Receiver<Option<String>>,
    stop: Option<oneshot::Sender<()>>,
    monitor: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for RestoreHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreHelper")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .field("hello", &self.hello)
            .finish()
    }
}

impl RestoreHelper {
    /// 도우미가 끝났으면 그 까닭
    pub fn ended_reason(&self) -> Option<String> {
        self.ended.borrow().clone()
    }

    /// 도우미가 끝날 때까지 기다려 까닭을 돌려준다(QEMU가 끝남, 덮개 한도, 닫음)
    pub async fn ended(&self) -> String {
        let mut rx = self.ended.clone();
        match rx.wait_for(Option::is_some).await {
            Ok(r) => r.clone().unwrap_or_default(),
            Err(_) => "the helper is gone".into(),
        }
    }
}

/// 도우미를 끝낸다: 에이전트 `Shutdown`(5초) → 끝나기를 5초 → SIGTERM → 5초 → SIGKILL, scope·세션 디렉터리를 치운다.
/// 이미 끝났으면 치우기만 한다.
pub async fn stop_helper(mut helper: RestoreHelper) {
    if helper.ended_reason().is_none() {
        let _ = tokio::time::timeout(STOP_WAIT, helper.client.shutdown()).await;
        let _ = tokio::time::timeout(STOP_WAIT, helper.ended()).await;
    }
    drop(helper.stop.take());
    if let Some(m) = helper.monitor.take() {
        let _ = m.await;
    }
    helper.client.close("the session was closed");
}

/// 게스트가 쓴 글의 마지막 줄을 로그·오류에 넣을 꼴로(제어 문자 이스케이프, 200자)
fn last_line(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(std::io::SeekFrom::Start(len.saturating_sub(4096)))
        .ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    Some(
        line.chars()
            .flat_map(char::escape_default)
            .take(200)
            .collect(),
    )
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 끝난 까닭에 붙일 로그의 마지막 줄(QEMU 오류가 먼저, 없으면 콘솔)
fn log_tail(paths: &HelperPaths) -> String {
    last_line(&paths.qemu_log())
        .or_else(|| last_line(&paths.console_log()))
        .map(|l| format!(": {l}"))
        .unwrap_or_default()
}

/// QEMU가 받는 것(판마다 다름 — 바이너리마다 한 번 묻는다)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QemuCaps {
    run_with: bool,
    runas: bool,
    sandbox: bool,
}

static CAPS: Mutex<Option<HashMap<(PathBuf, String), QemuCaps>>> = Mutex::new(None);

/// `qemu <args> -version`이 성공하는지(옵션을 받는지 — `-version`은 옵션을 읽은 뒤 곧바로 끝낸다)
async fn qemu_accepts(binary: &Path, args: &[&str]) -> bool {
    let out = tokio::process::Command::new(binary)
        .args(args)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(tokio::time::timeout(PROBE_TIMEOUT, out).await, Ok(Ok(s)) if s.success())
}

async fn qemu_caps(binary: &Path, user: &str) -> QemuCaps {
    let key = (binary.to_path_buf(), user.to_string());
    if let Some(c) = CAPS.lock().unwrap().as_ref().and_then(|m| m.get(&key)) {
        return *c;
    }
    let caps = QemuCaps {
        run_with: qemu_accepts(binary, &["-run-with", &format!("user={user}")]).await,
        runas: qemu_accepts(binary, &["-runas", user]).await,
        sandbox: qemu_accepts(binary, &["-sandbox", orbivisor_qemu::HELPER_SANDBOX]).await,
    };
    CAPS.lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key, caps);
    caps
}

/// 이 노드에서 도우미의 권한 내림과 sandbox. Linux에서 opld가 root이고 `run_as`가 있을 때만 내린다(내릴 수 없는
/// QEMU면 오류 — 몰래 root로 돌리지 않는다). sandbox는 seccomp로 빌드한 QEMU면 늘.
async fn isolation(
    host: &HostProfile,
    run_as: Option<&str>,
) -> Result<(Option<HelperDrop>, bool), DriverError> {
    if !cfg!(target_os = "linux") {
        return Ok((None, false));
    }
    let user = run_as
        .filter(|u| !u.is_empty())
        .filter(|_| rustix::process::geteuid().is_root());
    let caps = qemu_caps(&host.qemu_binary, user.unwrap_or("root")).await;
    let drop = match user {
        None => None,
        Some(u) if caps.run_with => Some(HelperDrop::RunWith(u.into())),
        Some(u) if caps.runas => Some(HelperDrop::Runas(u.into())),
        Some(u) => {
            return Err(DriverError::FileRestore(format!(
                "this QEMU cannot run the helper as {u:?} (neither -run-with nor -runas works); set [file_restore] run_as = \"\" to run it as root"
            )));
        }
    };
    Ok((drop, caps.sandbox))
}

/// QEMU를 감싸는 `systemd-run`의 인자(`--` 앞): 메모리(설정 + QEMU 몫 256 MiB)·작업 수·CPU 몫을 묶는다(§7.2).
fn helper_scope_args(id: Uuid, cfg: &HelperConfig) -> Vec<String> {
    vec![
        "--scope".into(),
        "--quiet".into(),
        "--collect".into(),
        format!("--unit={UNIT_PREFIX}{id}"),
        format!("--property=MemoryMax={}M", u64::from(cfg.memory_mib) + 256),
        "--property=TasksMax=64".into(),
        format!("--property=CPUQuota={}%", cfg.vcpus * 100),
    ]
}

fn check_launch(host: &HostProfile, launch: &HelperLaunch) -> Result<(), DriverError> {
    if launch.image.arch != host.arch {
        return Err(DriverError::InvalidSpec(format!(
            "the helper image is for {} but this node is {}",
            arch_dir(launch.image.arch),
            arch_dir(host.arch)
        )));
    }
    if launch.disks.is_empty() {
        return Err(DriverError::InvalidSpec("the backup has no disks".into()));
    }
    for (i, d) in launch.disks.iter().enumerate() {
        if let LiveRestoreSource::Qcow2 { chain } = &d.source
            && chain.is_empty()
        {
            return Err(DriverError::InvalidSpec(format!(
                "disk {i} has an empty chain"
            )));
        }
        if d.size == 0 {
            return Err(DriverError::InvalidSpec(format!("disk {i} has no size")));
        }
    }
    Ok(())
}

/// 실제 도우미 VM을 띄운다(§7.4): 덮개를 만들고, 권한 내림·sandbox를 정하고, QEMU를 띄워(Linux systemd면
/// `opl-fr-<id>.scope`) 에이전트 채널에 붙어 `Hello`를 `boot_timeout`까지 기다린다. `Hello`의 이미지 판이 검사한 이미지와
/// 같아야 한다. 실패하면 띄운 것을 끝내고 치운다(까닭에 로그의 마지막 줄).
pub async fn start_helper(
    host: &HostProfile,
    systemd_scope: bool,
    run_dir: &Path,
    state_dir: &Path,
    cfg: &HelperConfig,
    launch: &HelperLaunch,
) -> Result<RestoreHelper, DriverError> {
    check_launch(host, launch)?;
    let paths = HelperPaths::new(run_dir, state_dir, launch.id);
    let ctl = paths.ctl_socket();
    if ctl.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(DriverError::InvalidSpec(format!(
            "the helper socket path {} is longer than {MAX_SOCKET_PATH} bytes; use a shorter run_dir",
            ctl.display()
        )));
    }
    paths.prepare()?;
    let started = Instant::now();
    let result = Box::pin(launch_qemu(host, systemd_scope, &paths, cfg, launch)).await;
    let (mut child, pid) = match result {
        Ok(c) => c,
        Err(e) => {
            cleanup(&paths, systemd_scope, launch.id).await;
            return Err(e);
        }
    };
    let overlays: Vec<PathBuf> = (0..launch.disks.len()).map(|i| paths.overlay(i)).collect();
    let connected = tokio::select! {
        r = Box::pin(connect_helper(&mut child, &paths, cfg, launch, started)) => r,
        e = boot_guard(&paths, &overlays, cfg.overlay_max) => Err(e),
    };
    let (client, hello) = match connected {
        Ok(c) => c,
        Err(e) => {
            terminate(&mut child).await;
            let e = match e {
                DriverError::FileRestore(m) => {
                    DriverError::FileRestore(format!("{m}{}", log_tail(&paths)))
                }
                e => e,
            };
            cleanup(&paths, systemd_scope, launch.id).await;
            return Err(e);
        }
    };
    let boot_time = started.elapsed();
    tracing::info!(session = %launch.id, pid, boot_ms = boot_time.as_millis() as u64, "file restore helper is up");
    let (ended_tx, ended) = watch::channel(None);
    let (stop, stop_rx) = oneshot::channel();
    let monitor = tokio::spawn(monitor_qemu(Monitor {
        child,
        paths,
        client: client.clone(),
        ended: ended_tx,
        stop: stop_rx,
        overlays,
        overlay_max: cfg.overlay_max,
        scope: systemd_scope,
        id: launch.id,
    }));
    Ok(RestoreHelper {
        id: launch.id,
        hello,
        client,
        boot_time,
        pid: Some(pid),
        ended,
        stop: Some(stop),
        monitor: Some(monitor),
    })
}

async fn launch_qemu(
    host: &HostProfile,
    systemd_scope: bool,
    paths: &HelperPaths,
    cfg: &HelperConfig,
    launch: &HelperLaunch,
) -> Result<(tokio::process::Child, u32), DriverError> {
    for i in 0..launch.disks.len() {
        remove_if_exists(&paths.overlay(i))?;
    }
    for (i, d) in launch.disks.iter().enumerate() {
        crate::storage::create_overlay(&cfg.qemu_img, &paths.overlay(i), d.size).await?;
    }
    for f in [paths.console_log(), paths.qemu_log(), paths.ctl_socket()] {
        remove_if_exists(&f)?;
    }
    std::fs::File::create(paths.console_log())?;
    let (drop, sandbox) = isolation(host, cfg.run_as.as_deref()).await?;
    if let Some(HelperDrop::RunWith(user) | HelperDrop::Runas(user)) = &drop {
        // QEMU는 내리기 전에 파일을 열지만, 내린 뒤 다시 열 일이 있어도 쓸 수 있게(§7.4).
        let mut files: Vec<PathBuf> = (0..launch.disks.len()).map(|i| paths.overlay(i)).collect();
        files.push(paths.console_log());
        let out = tokio::process::Command::new("chown")
            .arg(user)
            .args(&files)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await?;
        if !out.status.success() {
            return Err(DriverError::ToolFailed {
                tool: "chown",
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
    }
    let ctl = paths.ctl_socket();
    let console = paths.console_log();
    let overlays: Vec<PathBuf> = (0..launch.disks.len()).map(|i| paths.overlay(i)).collect();
    let disks: Vec<HelperDisk> = launch
        .disks
        .iter()
        .zip(&overlays)
        .map(|(d, o)| HelperDisk {
            source: &d.source,
            overlay: o,
        })
        .collect();
    let args = build_restore_helper_cmdline(&RestoreHelperArgs {
        id: launch.id,
        arch: host.arch,
        accel: host.accel,
        kernel: &launch.image.kernel,
        initrd: &launch.image.initrd,
        memory_mib: cfg.memory_mib,
        vcpus: cfg.vcpus,
        disks: &disks,
        ctl_socket: &ctl,
        console_log: &console,
        drop,
        sandbox,
    })
    .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
    let mut cmd = if systemd_scope {
        crate::qemu::clear_stale_scope(&format!("{UNIT_PREFIX}{}.scope", launch.id)).await;
        let mut cmd = tokio::process::Command::new("systemd-run");
        cmd.args(helper_scope_args(launch.id, cfg))
            .arg("--")
            .arg(&host.qemu_binary);
        cmd
    } else {
        tokio::process::Command::new(&host.qemu_binary)
    };
    let log = std::fs::File::create(paths.qemu_log())?;
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    if let Some(cpus) = crate::affinity::unpinned_child() {
        crate::qemu::pin_child(&mut cmd, &cpus);
    }
    let child = cmd.spawn().map_err(|e| {
        DriverError::FileRestore(format!("cannot start {}: {e}", host.qemu_binary.display()))
    })?;
    let pid = child.id().unwrap_or(0);
    std::fs::write(paths.pid_file(), pid.to_string())?;
    Ok((child, pid))
}

async fn connect_helper(
    child: &mut tokio::process::Child,
    paths: &HelperPaths,
    cfg: &HelperConfig,
    launch: &HelperLaunch,
    started: Instant,
) -> Result<(HelperClient, Hello), DriverError> {
    let deadline = started + cfg.boot_timeout;
    let ctl = paths.ctl_socket();
    let stream = loop {
        if let Some(status) = child.try_wait()? {
            return Err(DriverError::FileRestore(format!(
                "the helper VM exited ({status})"
            )));
        }
        if let Ok(s) = tokio::net::UnixStream::connect(&ctl).await {
            break s;
        }
        if Instant::now() >= deadline {
            return Err(DriverError::FileRestore(format!(
                "the helper VM did not open its channel in {}s",
                cfg.boot_timeout.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    let hello = HelperClient::connect(stream, remaining, cfg.client);
    // QEMU가 Hello 전에 끝나면(커널 패닉·에이전트 오류) 기다리지 않는다.
    let (client, hello) = tokio::select! {
        r = hello => r.map_err(|e| match e {
            HelperError::Timeout => DriverError::FileRestore(format!(
                "the helper VM did not answer in {}s",
                cfg.boot_timeout.as_secs()
            )),
            e => DriverError::FileRestore(e.to_string()),
        })?,
        status = child.wait() => {
            let status = status?;
            return Err(DriverError::FileRestore(format!("the helper VM exited ({status})")));
        }
    };
    if hello.image_version != launch.image.image_version {
        client.close("image version mismatch");
        return Err(DriverError::FileRestore(format!(
            "the helper says image {:?} but the manifest says {:?}",
            hello.image_version, launch.image.image_version
        )));
    }
    Ok((client, hello))
}

/// 자식 QEMU를 끝낸다: SIGTERM → 5초 → SIGKILL
async fn terminate(child: &mut tokio::process::Child) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    if let Some(pid) = child
        .id()
        .and_then(|p| i32::try_from(p).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    if tokio::time::timeout(STOP_WAIT, child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

async fn cleanup(paths: &HelperPaths, scope: bool, id: Uuid) {
    if scope {
        crate::qemu::clear_stale_scope(&format!("{UNIT_PREFIX}{id}.scope")).await;
    }
    paths.remove();
}

struct Monitor {
    child: tokio::process::Child,
    paths: HelperPaths,
    client: HelperClient,
    ended: watch::Sender<Option<String>>,
    stop: oneshot::Receiver<()>,
    overlays: Vec<PathBuf>,
    overlay_max: u64,
    scope: bool,
    id: Uuid,
}

fn set_once(tx: &watch::Sender<Option<String>>, reason: &str) {
    tx.send_if_modified(|r| {
        if r.is_none() {
            *r = Some(reason.to_string());
            true
        } else {
            false
        }
    });
}

/// 덮개 합계(바이트, qcow2 파일 크기)
fn overlay_bytes(overlays: &[PathBuf]) -> u64 {
    overlays
        .iter()
        .filter_map(|o| std::fs::metadata(o).ok())
        .map(|m| m.len())
        .sum()
}

/// 콘솔 로그가 상한을 넘으면 비운다(QEMU는 덧붙여 쓰므로 다음 줄부터 처음에 쓴다)
fn trim_console(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > CONSOLE_MAX)
        && let Ok(f) = std::fs::OpenOptions::new().write(true).open(path)
    {
        let _ = f.set_len(0);
    }
}

/// 덮개 한도를 넘은 까닭. 재는 것은 덮개 qcow2 파일 크기 합(빈 덮개의 머리 몇백 KiB 포함)이고, 덮개에 쓰는 것은 보통 저널
/// 재생이지만 도우미가 디스크에 쓴 무엇이든 될 수 있다.
fn overlay_reason(max: u64) -> String {
    format!(
        "the disk overlays grew past {} MiB (filesystem journal replay or other writes by the helper VM)",
        max.div_ceil(1 << 20)
    )
}

/// 부팅 중 지킴이(`Hello`까지): 콘솔 로그를 자르고 덮개 한도를 본다. 넘으면 그 까닭(부르는 쪽이 QEMU를 끝낸다).
async fn boot_guard(paths: &HelperPaths, overlays: &[PathBuf], max: u64) -> DriverError {
    let mut tick = tokio::time::interval(GUARD_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tick.tick().await;
        trim_console(&paths.console_log());
        if overlay_bytes(overlays) > max {
            return DriverError::FileRestore(overlay_reason(max));
        }
    }
}

/// 도우미 지킴이: QEMU가 끝나거나, 덮개가 한도를 넘거나(§9.4), 끝내라고 할 때까지. 끝난 까닭을 알리고 채널을 닫고
/// scope·디렉터리를 치운다.
async fn monitor_qemu(mut m: Monitor) {
    let mut tick = tokio::time::interval(GUARD_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    let reason = loop {
        tokio::select! {
            status = m.child.wait() => {
                let status = status.map_or_else(|e| e.to_string(), |s| s.to_string());
                break format!("the helper VM exited ({status}){}", log_tail(&m.paths));
            }
            _ = &mut m.stop => {
                m.client.close("the session was closed");
                terminate(&mut m.child).await;
                break "the session was closed".to_string();
            }
            _ = tick.tick() => {
                trim_console(&m.paths.console_log());
                let used = overlay_bytes(&m.overlays);
                if used > m.overlay_max {
                    let reason = overlay_reason(m.overlay_max);
                    // 채널이 끊긴 까닭보다 먼저 이 까닭을 남긴다.
                    set_once(&m.ended, &reason);
                    m.client.close(reason.clone());
                    terminate(&mut m.child).await;
                    break reason;
                }
            }
        }
    };
    set_once(&m.ended, &reason);
    let reason = m.ended.borrow().clone().unwrap_or(reason);
    m.client.close(reason);
    cleanup(&m.paths, m.scope, m.id).await;
}

/// opld가 시작할 때 앞 프로세스의 도우미를 치운다(§10.5 — 세션은 이어 가지 않는다): `{run_dir}/file-restore/*/qemu.pid`의
/// 살아 있는 QEMU(명령줄에 `opl-fr-<id>`)를 끝내고, 그 세션들의 `opl-fr-<id>.scope`를 멈추고, 두 `file-restore` 디렉터리를
/// 지운다. 이 opld의 디렉터리에 있는 세션만 건드린다 — 같은 호스트의 다른 opld(시험, 여러 인스턴스)의 도우미는 두지 않는다.
/// 치운 세션 수다.
pub async fn sweep_leftovers(run_dir: &Path, state_dir: &Path, systemd_scope: bool) -> usize {
    let mut ids = std::collections::BTreeSet::new();
    for base in [run_dir, state_dir] {
        let Ok(entries) = std::fs::read_dir(base.join(SUBDIR)) else {
            continue;
        };
        for e in entries.flatten() {
            if let Some(id) = e.file_name().to_str().and_then(|n| Uuid::try_parse(n).ok()) {
                ids.insert(id);
            }
        }
    }
    for &id in &ids {
        let paths = HelperPaths::new(run_dir, state_dir, id);
        let marker = format!("{UNIT_PREFIX}{id}");
        // 명령줄로 알아본다(pid가 다른 프로세스에 다시 쓰였을 수 있다). 끝난 좀비도 명령줄이 없어 끝난 것으로 본다.
        let ours = |pid: u32| crate::tpm::process_cmdline(pid).is_some_and(|c| c.contains(&marker));
        if let Some(pid) = std::fs::read_to_string(paths.pid_file())
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            && ours(pid)
            && let Some(target) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
        {
            tracing::info!(session = %id, pid, "stopping a file restore helper left by the previous process");
            let _ = rustix::process::kill_process(target, rustix::process::Signal::TERM);
            let gone = || !ours(pid);
            if !crate::qemu::wait_until(STOP_WAIT, gone).await {
                let _ = rustix::process::kill_process(target, rustix::process::Signal::KILL);
                crate::qemu::wait_until(STOP_WAIT, gone).await;
            }
        }
    }
    if systemd_scope {
        for unit in session_scopes(&ids) {
            crate::qemu::clear_stale_scope(&unit).await;
        }
    }
    for base in [run_dir, state_dir] {
        let dir = base.join(SUBDIR);
        if let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot remove file restore leftovers");
        }
    }
    ids.len()
}

/// 세션들의 scope 이름(`opl-fr-<id>.scope`) — 치우기는 제 디렉터리에서 찾은 세션의 것만 멈춘다(모든 `opl-fr-*`가 아니라).
fn session_scopes(ids: &std::collections::BTreeSet<Uuid>) -> Vec<String> {
    ids.iter()
        .map(|id| format!("{UNIT_PREFIX}{id}.scope"))
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// 가짜 도우미(시험·가짜 하이퍼바이저)
// ---------------------------------------------------------------------------------------------------------------

/// 가짜 도우미의 볼륨: 에이전트 몸통이 그대로 답한다(`fs`가 없으면 마운트할 수 없음)
pub type FakeVolumes = Vec<(Volume, Option<Arc<dyn orbivisor_restore::agent::Fs>>)>;

/// 가짜 도우미를 띄운다: 에이전트 몸통([`orbivisor_restore::agent::serve`])을 소켓 쌍 건너 스레드에서 돌리고 실제와 같은
/// [`HelperClient`]로 붙는다. `Hello`의 이미지 판은 `launch.image`의 것이다.
pub async fn fake_helper(
    launch: &HelperLaunch,
    volumes: FakeVolumes,
    opts: ClientOptions,
) -> Result<RestoreHelper, DriverError> {
    let started = Instant::now();
    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    let reader = theirs.try_clone()?;
    let killer = theirs.try_clone()?;
    let hello = Hello {
        proto: proto::PROTO_VERSION,
        image_version: launch.image.image_version.clone(),
        kernel: "fake".into(),
    };
    let (done_tx, done_rx) = oneshot::channel();
    let fixed = Arc::new(orbivisor_restore::agent::FixedVolumes::new(volumes));
    std::thread::Builder::new()
        .name("fake-restore-helper".into())
        .spawn(move || {
            let r = orbivisor_restore::agent::serve(
                fixed,
                hello,
                reader,
                theirs,
                orbivisor_restore::agent::ServeOptions::default(),
            );
            let _ = done_tx.send(r);
        })?;
    ours.set_nonblocking(true)?;
    let ours = tokio::net::UnixStream::from_std(ours)?;
    let (client, hello) = HelperClient::connect(ours, Duration::from_secs(10), opts)
        .await
        .map_err(|e| DriverError::FileRestore(e.to_string()))?;
    let (ended_tx, ended) = watch::channel(None);
    let (stop, stop_rx) = oneshot::channel::<()>();
    let monitor_client = client.clone();
    let monitor = tokio::spawn(async move {
        let reason = tokio::select! {
            r = done_rx => match r {
                Ok(Ok(())) => "the helper VM exited (exit status: 0)".to_string(),
                Ok(Err(e)) => format!("the helper VM exited: {e}"),
                Err(_) => "the helper VM exited".to_string(),
            },
            _ = stop_rx => {
                let _ = killer.shutdown(std::net::Shutdown::Both);
                "the session was closed".to_string()
            }
        };
        set_once(&ended_tx, &reason);
        monitor_client.close(reason);
    });
    Ok(RestoreHelper {
        id: launch.id,
        hello,
        client,
        boot_time: started.elapsed(),
        pid: None,
        ended,
        stop: Some(stop),
        monitor: Some(monitor),
    })
}

#[cfg(test)]
mod tests;
