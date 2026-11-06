# opLite 계획 34: 감사 로그 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 누가 언제 무엇을 바꿨는지 남긴다. 노드마다 그 노드가 처리한 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 로컬 파일(JSON Lines, 회전)과 journald에 적고, 어느 노드에서나 클러스터 전체 기록을 모아 읽는다(`GET /api/v1/audit`, `oplctl audit`, 웹 `/audit`). 하위 프로젝트 10(엔터프라이즈 기능)의 첫 계획이다.

**Architecture:**
- **저장:** Raft가 아니라 노드 로컬 파일 `{data_dir}/audit/audit.log`(0600, 크기 회전). 로그인 실패마다 Raft에 쓰면 공격이 곧 쓰기 부하이고, 스냅샷이 커지며, 정족수를 잃은 동안의 요청이 남지 않기 때문이다. 쓰기는 전용 스레드가 채널에서 받아 덧붙인다(요청이 디스크를 기다리지 않음).
- **기록 지점:** 라우터의 미들웨어 하나(`route_layer`)가 모든 바꾸는 요청을 적는다. 누가 했는지는 인증 추출기가 요청마다 둔 칸에 적는다. 새 API는 저절로 기록된다.
- **읽기:** 받은 노드가 노드 사이 gRPC `ReadAudit`로 다른 노드(위트니스 제외)에 묻고 시간순으로 합친다. 닿지 않은 노드와 감사가 꺼진 노드는 따로 알린다.
- **남용 방지:** 필드는 512바이트로 자르고, 인증 없는 요청은 IP마다 1분에 60개까지만 적고 나머지는 수로 남긴다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum 미들웨어·utoipa, tonic(노드 사이 RPC, JSON 코덱), tokio, clap, `libc`(`O_NOFOLLOW`, 이미 `Cargo.lock`에 있는 판). 웹은 React·TypeScript·react-query·vitest·msw. 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-06-enterprise-design.md` (§2와 §2.6 "구현(계획 34)")
**선행 계획:** 계획 1~33 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 노드 로컬 감사 파일·회전·journald 대상 `opl_audit` | 변조 방지(해시 체인·서명), 원격 syslog 직접 전송(journald에서 관리자가 전달) |
| 바꾸는 요청·로그인·로그아웃·콘솔 열기 기록 | 요청 본문, 다른 GET, 콘솔 닫힘 |
| 노드 사이 `ReadAudit`와 `GET /api/v1/audit` | 날짜로 지우기(크기·개수로만) |
| oplctl `audit`, 웹 `/audit`, opld `[audit]` | 외부 인증(계획 35·36), 원격 클러스터(계획 37) |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust·웹 변경과 시험(opld 프로세스 E2E 포함). 노드 사이 RPC 시험은 127.0.0.1의 실제 gRPC다.
- **Linux에서만 보는 것:** journald·rsyslog 전달 예(README)는 실제 systemd 호스트에서만 확인된다.

## 설계 결정

- **action은 표가 아니라 라우트 템플릿:** `POST /vms/{key}/actions/{action}`처럼 메서드와 axum의 `MatchedPath`(`/api/v1` 뗌). VM 동작은 템플릿이 같으므로 `action` 거르기는 실제 경로도 본다.
- **auth:** `Authorization: Bearer`가 있으면 성공·실패와 관계없이 `token`, 세션으로 인증되면 `session`, 그 밖은 `none`.
- **로그인:** `detail`은 `username=<이름>`(이름 형식이 아니면 `<invalid>`). 비밀번호는 어디에도 넣지 않는다.
- **꺼진 노드:** `[audit] enabled = false`인 노드는 `ReadAudit`에 `failed_precondition`으로 답하고 응답의 `disabled`에 들어간다(`unreachable`과 구별).
- **opld 로그 색:** 표준 출력이 터미널일 때만 ANSI 색을 써서 journald의 `opl_audit` 줄을 rsyslog가 그대로 거를 수 있게 한다.

## 파일 구조

```
crates/opl-types/src/{audit.rs, lib.rs}
crates/opl-api/src/{audit.rs, auth.rs, users.rs, subscription.rs, limit.rs, state.rs, lib.rs}, crates/opl-api/tests/{audit.rs, two_nodes.rs, common/mod.rs}
crates/opl-peer/{build.rs, src/audit.rs, src/client.rs, src/server.rs, src/lib.rs, tests/grpc.rs}
crates/opld/src/{config.rs, main.rs, member.rs}, crates/opld/tests/audit.rs, crates/oplctl/src/cli.rs
web/src/audit/{AuditPage.tsx, AuditPage.test.tsx}, web/src/{App.tsx, api/types.ts, layout/AppLayout.tsx, i18n/ko.json, i18n/en.json}
README.md, docs/superpowers/specs/…
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

`web/src/api/schema.d.ts`는 싣지 않는다. API가 바뀌는 Task의 "생성 파일" 단계에서 `pnpm -C web gen:api`로 만든다. `Cargo.lock`도 싣지 않는다. Task 1의 "생성 파일" 단계에서 `cargo check`가 의존 목록 줄을 고친다.

---

### Task 1: 감사 항목 타입과 파일 기록·읽기

`opl_types::audit`: 감사 항목 `AuditEntry`(시각, 노드, 사용자·역할·인증 방식, IP, 메서드·경로·동작·대상, 상태, 걸린 시간, detail)와 거르기 `AuditQuery`. opl-api `AuditLog`: 노드의 `{dir}/audit.log`에 한 줄 JSON으로 덧붙이는 쓰기 스레드(채널 1만, 넘치면 `audit.dropped`), 크기 회전과 개수 한도, 1초 묶음 `sync_data`, 필드 512바이트 자르기, 인증 없는 항목의 IP별(IPv6는 /64) 1분 60개 한도와 `audit.suppressed`, tracing 대상 `opl_audit`, 새 파일부터 거꾸로 읽는 `read`, 마감 있는 `shutdown`. `AppState.audit`(기본 꺼짐). 새 직접 의존 `libc`(이미 `Cargo.lock`에 있는 판).

**Files:**
- Create: `crates/opl-api/src/audit.rs`
- Create: `crates/opl-types/src/audit.rs`
- Modify: `Cargo.toml`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/limit.rs`
- Modify: `crates/opl-api/src/state.rs`
- Modify: `crates/opl-types/src/lib.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`Cargo.toml`에서

```toml
rust-embed = { version = "8.13.0", features = ["mime-guess", "interpolate-folder-path"] }
```

을 다음으로 바꾼다:

```toml
rust-embed = { version = "8.13.0", features = ["mime-guess", "interpolate-folder-path"] }
libc = "0.2.191"
```

`crates/opl-api/Cargo.toml`에서

```toml
hyper-util.workspace = true
opl-cluster.workspace = true
```

을 다음으로 바꾼다:

```toml
hyper-util.workspace = true
libc.workspace = true
opl-cluster.workspace = true
```

`crates/opl-api/src/audit.rs`를 만든다:

```rust
//! 감사 로그. 항목을 채널로 받아 쓰기 스레드가 `{dir}/audit.log`에 한 줄 JSON으로 덧붙이고
//! (요청이 디스크를 기다리지 않음), 같은 줄을 tracing 대상 `opl_audit`로도 내보낸다.
//! 파일이 커지면 `audit.log.1`, `.2`… 로 밀고, 읽을 때는 새 파일부터 거꾸로 훑는다.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use uuid::Uuid;

/// 쓰기 채널의 용량. 가득 차면 항목을 버리고 버린 수를 센다.
const CHANNEL_CAPACITY: usize = 10_000;
/// 쓴 것이 있으면 이만큼마다 `sync_data`
const SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// `shutdown`이 남은 것을 쓰기를 기다리는 최대 시간
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// 항목의 문자열 필드(`path`·`target.key`·`detail`·`user`)의 최대 바이트. 넘으면 잘라 `…`을 붙인다.
const MAX_FIELD_BYTES: usize = 512;
/// 인증 없는 항목(`user` 없음)은 IP마다 이 창에 `UNAUTHENTICATED_PER_WINDOW`개까지만 적는다.
const UNAUTHENTICATED_WINDOW: Duration = Duration::from_secs(60);
const UNAUTHENTICATED_PER_WINDOW: u32 = 60;
/// 인증 없는 항목을 세는 IP의 최대 수. 넘치면 가장 오래된 창부터 버린다.
const UNAUTHENTICATED_IPS: usize = 10_000;
const FILE_NAME: &str = "audit.log";

/// 감사 로그 설정
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// 파일을 두는 디렉터리(없으면 0700으로 만든다)
    pub dir: PathBuf,
    /// 항목의 `node`에 넣을 이 노드의 이름
    pub node: String,
    /// 한 파일의 최대 크기(바이트). 넘으면 회전한다.
    pub max_file_size: u64,
    /// 지금 파일을 포함한 최대 파일 수
    pub max_files: usize,
}

/// 노드의 감사 로그. 꺼져 있으면 아무것도 적지 않고 읽으면 빈 목록이다.
pub struct AuditLog {
    inner: Option<Inner>,
}

struct Inner {
    node: String,
    dir: PathBuf,
    tx: SyncSender<Msg>,
    /// 채널이 가득 차 버린 항목 수. 쓰기 스레드가 다음 항목 앞에 `audit.dropped`로 적는다.
    dropped: Arc<AtomicU64>,
    /// 인증 없는 항목의 IP별 창(쓰기 스레드가 끝난 창을 훑는다)
    unauthenticated: Arc<std::sync::Mutex<Unauthenticated>>,
    /// 쓰기 스레드가 끝난 뒤의 기록을 이미 경고했다.
    stopped: std::sync::atomic::AtomicBool,
}

enum Msg {
    Entry(Box<AuditEntry>),
    /// 남은 것을 쓰고 fsync한 뒤 스레드를 끝낸다.
    Shutdown(tokio::sync::oneshot::Sender<()>),
}

impl AuditLog {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// 디렉터리를 만들고 쓰기 스레드를 띄운다.
    pub fn open(config: AuditConfig) -> io::Result<Self> {
        Self::start(config, CHANNEL_CAPACITY, None)
    }

    /// `gate`가 있으면 쓰기 스레드는 그것을 받을 때까지 기다린다(시험에서 채널을 채우려고).
    fn start(config: AuditConfig, capacity: usize, gate: Option<Receiver<()>>) -> io::Result<Self> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.dir)?;
        // 이미 있던 디렉터리도 0700으로 맞춘다.
        std::fs::set_permissions(&config.dir, std::fs::Permissions::from_mode(0o700))?;
        let writer = Writer::open(&config)?;
        let (tx, rx) = sync_channel(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let counter = dropped.clone();
        let node = config.node.clone();
        let unauthenticated = Arc::new(std::sync::Mutex::new(Unauthenticated::default()));
        let table = unauthenticated.clone();
        std::thread::Builder::new()
            .name("opl-audit".into())
            .spawn(move || {
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
                run(writer, rx, &counter, &node, &table);
            })?;
        Ok(Self {
            inner: Some(Inner {
                node: config.node,
                dir: config.dir,
                tx,
                dropped,
                unauthenticated,
                stopped: Default::default(),
            }),
        })
    }

    /// 항목을 적는다(기다리지 않음). `node`는 이 노드의 이름으로 바꾸고 긴 필드는 자른다.
    /// 인증 없는 항목은 IP(IPv6는 /64)마다 1분에 60개까지만 적고, 넘친 수는 창이 끝나면 쓰기
    /// 스레드가 1초 안에(그 전에 그 IP가 다시 오면 그 항목 앞에, 또는 `shutdown` 때)
    /// `audit.suppressed`로 적는다.
    pub fn record(&self, entry: AuditEntry) {
        self.record_at(entry, Instant::now());
    }

    /// `now`는 인증 없는 항목의 창을 재는 시각이다(시험이 넣는다).
    fn record_at(&self, mut entry: AuditEntry, now: Instant) {
        let Some(inner) = &self.inner else { return };
        truncate_fields(&mut entry);
        if entry.user.is_none() {
            let key = unauthenticated_key(&entry.ip);
            let (write, suppressed) = inner.unauthenticated.lock().unwrap().admit(&key, now);
            for s in suppressed {
                inner.send(suppressed_entry(s));
            }
            if !write {
                return;
            }
        }
        inner.send(entry);
    }

    /// 조건에 맞는 항목을 새 것부터 `limit + 1`개까지 돌려준다(시간 내림차순).
    pub async fn read(&self, query: AuditQuery) -> io::Result<Vec<AuditEntry>> {
        let Some(inner) = &self.inner else {
            return Ok(Vec::new());
        };
        let dir = inner.dir.clone();
        tokio::task::spawn_blocking(move || read_dir(&dir, &query))
            .await
            .map_err(io::Error::other)?
    }

    /// 남은 `audit.suppressed`와 채널에 남은 것을 쓰고 fsync한다. 그 뒤의 항목은 적지 않는다.
    /// 5초 안에 끝나지 않으면 경고하고 돌아간다.
    pub async fn shutdown(&self) {
        self.shutdown_within(SHUTDOWN_TIMEOUT).await;
    }

    async fn shutdown_within(&self, timeout: Duration) {
        let Some(inner) = &self.inner else { return };
        let (ack, done) = tokio::sync::oneshot::channel();
        let tx = inner.tx.clone();
        let finish = async move {
            // 채널이 가득 차 있을 수 있다. 막히는 send 대신 자리가 날 때까지 다시 시도해,
            // 제한 시간이 지나면 남는 것 없이 그만둔다.
            let mut msg = Msg::Shutdown(ack);
            loop {
                match tx.try_send(msg) {
                    Ok(()) => break,
                    Err(TrySendError::Full(back)) => {
                        msg = back;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            let _ = done.await;
        };
        if tokio::time::timeout(timeout, finish).await.is_err() {
            tracing::warn!("the audit log did not finish writing within {timeout:?}; giving up");
        }
    }
}

/// 쓰기 스레드
fn run(
    mut writer: Writer,
    rx: Receiver<Msg>,
    dropped: &AtomicU64,
    node: &str,
    unauthenticated: &std::sync::Mutex<Unauthenticated>,
) {
    let write_suppressed = |writer: &mut Writer, suppressed: Vec<Suppressed>| {
        for s in suppressed {
            writer.write(&AuditEntry {
                node: node.into(),
                ..suppressed_entry(s)
            });
        }
    };
    // 1초마다 끝난 인증 없는 창의 넘친 수를 적고 `sync_data`한다.
    let mut last_sync = Instant::now();
    loop {
        let wait = SYNC_INTERVAL.saturating_sub(last_sync.elapsed());
        match rx.recv_timeout(wait) {
            Ok(Msg::Entry(entry)) => {
                write_dropped(&mut writer, dropped, node);
                writer.write(&entry);
            }
            Ok(Msg::Shutdown(ack)) => {
                write_dropped(&mut writer, dropped, node);
                let suppressed = unauthenticated.lock().unwrap().drain();
                write_suppressed(&mut writer, suppressed);
                writer.sync();
                let _ = ack.send(());
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                writer.sync();
                return;
            }
        }
        if last_sync.elapsed() >= SYNC_INTERVAL {
            let suppressed = unauthenticated.lock().unwrap().sweep(Instant::now());
            write_suppressed(&mut writer, suppressed);
            writer.sync();
            last_sync = Instant::now();
        }
    }
}

/// 버린 항목이 있으면 그 수를 `audit.dropped` 항목으로 적는다.
fn write_dropped(writer: &mut Writer, dropped: &AtomicU64, node: &str) {
    let n = dropped.swap(0, Ordering::Relaxed);
    if n == 0 {
        return;
    }
    writer.write(&AuditEntry {
        id: Uuid::now_v7(),
        time: now_ms(),
        node: node.into(),
        user: None,
        user_id: None,
        role: None,
        auth: AuditAuth::None,
        ip: String::new(),
        method: String::new(),
        path: String::new(),
        action: "audit.dropped".into(),
        target: None,
        status: 0,
        duration_ms: 0,
        detail: Some(format!("{n} entries dropped")),
    });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Inner {
    /// 이 노드의 이름을 넣어 쓰기 스레드에 넘긴다. 채널이 가득 찼거나 스레드가 끝났으면 버리고 센다.
    fn send(&self, mut entry: AuditEntry) {
        entry.node.clone_from(&self.node);
        match self.tx.try_send(Msg::Entry(Box::new(entry))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                if !self.stopped.swap(true, Ordering::Relaxed) {
                    tracing::warn!("the audit log writer has stopped; entries are not recorded");
                }
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// 긴 문자열 필드를 `MAX_FIELD_BYTES`로 자른다(UTF-8 글자 경계, 잘랐으면 끝에 `…`).
fn truncate_fields(entry: &mut AuditEntry) {
    let fields = [
        Some(&mut entry.path),
        entry.target.as_mut().map(|t| &mut t.key),
        entry.detail.as_mut(),
        entry.user.as_mut(),
    ];
    for field in fields.into_iter().flatten() {
        if field.len() > MAX_FIELD_BYTES {
            let mut end = MAX_FIELD_BYTES;
            while !field.is_char_boundary(end) {
                end -= 1;
            }
            field.truncate(end);
            field.push('…');
        }
    }
}

/// 한 IP(IPv6는 /64)의 끝난(또는 버린) 창에서 적지 않은 수
#[derive(Debug, PartialEq)]
struct Suppressed {
    key: String,
    count: u64,
    /// 그 창의 시작
    since: Instant,
}

/// 인증 없는 항목 `count`개를 적지 않았다는 항목. `ip`는 세는 단위(IPv6는 `…::/64`)다.
fn suppressed_entry(s: Suppressed) -> AuditEntry {
    // 창 시작(Instant)을 벽시계 시각으로 옮긴다.
    let ago = Instant::now()
        .saturating_duration_since(s.since)
        .as_millis() as u64;
    let since =
        time::OffsetDateTime::from_unix_timestamp((now_ms().saturating_sub(ago) / 1000) as i64)
            .ok()
            .and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_default();
    AuditEntry {
        id: Uuid::now_v7(),
        time: now_ms(),
        // record·쓰기 스레드가 이 노드의 이름으로 채운다.
        node: String::new(),
        user: None,
        user_id: None,
        role: None,
        auth: AuditAuth::None,
        ip: s.key,
        method: String::new(),
        path: String::new(),
        action: "audit.suppressed".into(),
        target: None,
        status: 0,
        duration_ms: 0,
        detail: Some(format!(
            "{} unauthenticated requests not recorded since {since}",
            s.count
        )),
    }
}

/// 인증 없는 항목을 세는 단위. IPv4는 주소, IPv6는 /64 네트워크(로그인 시도 제한과 같은 규칙)
fn unauthenticated_key(ip: &str) -> String {
    match ip.parse::<std::net::IpAddr>().map(crate::limit::counted_ip) {
        Ok(v6 @ std::net::IpAddr::V6(_)) => format!("{v6}/64"),
        Ok(v4) => v4.to_string(),
        Err(_) => ip.to_string(),
    }
}

/// 인증 없는 항목의 IP별 1분 창
#[derive(Default)]
struct Unauthenticated {
    windows: std::collections::HashMap<String, Window>,
    /// (창 시작, 키) 순서. 가장 오래된 창과 끝난 창을 훑지 않고 찾는다.
    order: std::collections::BTreeSet<(Instant, String)>,
}

struct Window {
    start: Instant,
    written: u32,
    /// 이 창에서 적지 않은 수
    suppressed: u64,
}

impl Window {
    fn new(start: Instant) -> Self {
        Self {
            start,
            written: 1,
            suppressed: 0,
        }
    }

    fn suppressed(&self, key: String) -> Option<Suppressed> {
        (self.suppressed > 0).then_some(Suppressed {
            key,
            count: self.suppressed,
            since: self.start,
        })
    }
}

impl Unauthenticated {
    /// `key`의 인증 없는 항목을 적을지와, 그 앞에 먼저 적을 넘친 수를 돌려준다.
    fn admit(&mut self, key: &str, now: Instant) -> (bool, Vec<Suppressed>) {
        let mut first = Vec::new();
        if let Some(w) = self.windows.get_mut(key) {
            if now.duration_since(w.start) < UNAUTHENTICATED_WINDOW {
                if w.written < UNAUTHENTICATED_PER_WINDOW {
                    w.written += 1;
                    return (true, first);
                }
                w.suppressed += 1;
                return (false, first);
            }
            // 쓰기 스레드가 훑기 전에 다시 왔다. 앞 창에서 넘친 수를 먼저 적고 새 창을 연다.
            let old = std::mem::replace(w, Window::new(now));
            self.order.remove(&(old.start, key.to_string()));
            self.order.insert((now, key.to_string()));
            first.extend(old.suppressed(key.to_string()));
            return (true, first);
        }
        if self.windows.len() >= UNAUTHENTICATED_IPS
            && let Some((_, oldest)) = self.order.pop_first()
            && let Some(w) = self.windows.remove(&oldest)
        {
            first.extend(w.suppressed(oldest));
        }
        self.windows.insert(key.to_string(), Window::new(now));
        self.order.insert((now, key.to_string()));
        (true, first)
    }

    /// 끝난 창을 모두 지우고, 그 가운데 넘친 수가 있는 것을 돌려준다.
    fn sweep(&mut self, now: Instant) -> Vec<Suppressed> {
        let mut out = Vec::new();
        while let Some((start, _)) = self.order.first()
            && now.duration_since(*start) >= UNAUTHENTICATED_WINDOW
        {
            let (_, key) = self.order.pop_first().expect("first above");
            if let Some(w) = self.windows.remove(&key) {
                out.extend(w.suppressed(key));
            }
        }
        out
    }

    /// 넘친 수가 남은 것을 모두 꺼낸다(종료 때).
    fn drain(&mut self) -> Vec<Suppressed> {
        self.order.clear();
        self.windows
            .drain()
            .filter_map(|(key, w)| w.suppressed(key))
            .collect()
    }
}

/// `n`번째 파일(0은 지금 파일)
fn file_path(dir: &Path, n: usize) -> PathBuf {
    if n == 0 {
        dir.join(FILE_NAME)
    } else {
        dir.join(format!("{FILE_NAME}.{n}"))
    }
}

/// 지금 파일과 회전
struct Writer {
    dir: PathBuf,
    max_file_size: u64,
    max_files: usize,
    /// 열지 못했으면 없다(다음 쓰기에서 다시 연다).
    file: Option<File>,
    size: u64,
    dirty: bool,
}

impl Writer {
    fn open(config: &AuditConfig) -> io::Result<Self> {
        let mut writer = Self {
            dir: config.dir.clone(),
            max_file_size: config.max_file_size,
            max_files: config.max_files.max(1),
            file: None,
            size: 0,
            dirty: false,
        };
        // `max_files`를 줄였으면 남은 오래된 파일(`audit.log.N`, N ≥ max_files)을 지운다.
        let prefix = format!("{FILE_NAME}.");
        for item in std::fs::read_dir(&config.dir)? {
            let name = item?.file_name();
            let n = name
                .to_str()
                .and_then(|n| n.strip_prefix(&prefix))
                .and_then(|n| n.parse::<usize>().ok());
            if let Some(n) = n.filter(|n| *n >= writer.max_files) {
                remove(&file_path(&config.dir, n))?;
            }
        }
        writer.reopen()?;
        Ok(writer)
    }

    fn reopen(&mut self) -> io::Result<()> {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            // 심볼릭 링크를 따라가 다른 파일에 쓰지 않는다.
            .custom_flags(libc::O_NOFOLLOW)
            .open(file_path(&self.dir, 0))?;
        // 이미 있던 파일도 0600으로 맞춘다.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        self.size = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }

    /// 한 줄을 덧붙인다. 실패하면 경고만 남긴다(요청은 이미 끝났다).
    fn write(&mut self, entry: &AuditEntry) {
        let Ok(json) = serde_json::to_string(entry) else {
            return;
        };
        tracing::info!(target: "opl_audit", "{json}");
        if let Err(e) = self.append(&json) {
            tracing::warn!("cannot write the audit log: {e}");
        }
    }

    fn append(&mut self, json: &str) -> io::Result<()> {
        let line = format!("{json}\n");
        let len = line.len() as u64;
        if self.file.is_some() && self.size > 0 && self.size + len > self.max_file_size {
            self.rotate()?;
        }
        if self.file.is_none() {
            self.reopen()?;
        }
        let file = self.file.as_mut().expect("opened above");
        file.write_all(line.as_bytes())?;
        self.size += len;
        self.dirty = true;
        Ok(())
    }

    /// 가장 오래된 것을 지우고 `.N` → `.N+1`, 지금 파일 → `.1`로 민 뒤 새 파일을 연다.
    fn rotate(&mut self) -> io::Result<()> {
        self.sync();
        self.file = None;
        let keep = self.max_files - 1;
        if keep == 0 {
            remove(&file_path(&self.dir, 0))?;
        } else {
            remove(&file_path(&self.dir, keep))?;
            for n in (1..keep).rev() {
                rename(&file_path(&self.dir, n), &file_path(&self.dir, n + 1))?;
            }
            rename(&file_path(&self.dir, 0), &file_path(&self.dir, 1))?;
        }
        self.reopen()
    }

    fn sync(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(file) = &self.file
            && let Err(e) = file.sync_data()
        {
            tracing::warn!("cannot sync the audit log: {e}");
        }
        self.dirty = false;
    }
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 새 파일부터 줄을 거꾸로 훑어 조건에 맞는 것을 `limit + 1`개까지 모은다. 망가진 줄은 건너뛴다.
fn read_dir(dir: &Path, query: &AuditQuery) -> io::Result<Vec<AuditEntry>> {
    let want = query.limit as usize + 1;
    let mut out = Vec::new();
    // 읽는 중에 회전하면 같은 항목이 두 파일에서 보일 수 있다.
    let mut seen = std::collections::HashSet::new();
    for n in 0.. {
        let text = match std::fs::read_to_string(file_path(dir, n)) {
            Ok(text) => text,
            // 지금 파일은 회전 중에 잠깐 없을 수 있다.
            Err(e) if e.kind() == io::ErrorKind::NotFound && n == 0 => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        };
        let lines: Vec<&str> = text.lines().collect();
        let (Some(first), Some(last)) = (
            lines.iter().find_map(|l| parse(l)),
            lines.iter().rev().find_map(|l| parse(l)),
        ) else {
            continue;
        };
        // 파일 안은 오래된 것부터다. 이 파일의 가장 새 것이 범위 앞이면 더 오래된 파일도 그렇다.
        if last.time < query.since {
            break;
        }
        if first.time > query.until {
            continue;
        }
        for entry in lines.iter().rev().filter_map(|l| parse(l)) {
            if query.matches(&entry) && seen.insert(entry.id) {
                out.push(entry);
                if out.len() >= want {
                    break;
                }
            }
        }
        if out.len() >= want {
            break;
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse((e.time, e.id)));
    Ok(out)
}

fn parse(line: &str) -> Option<AuditEntry> {
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn config(dir: &Path, max_file_size: u64, max_files: usize) -> AuditConfig {
        AuditConfig {
            dir: dir.join("audit"),
            node: "n1".into(),
            max_file_size,
            max_files,
        }
    }

    fn entry(time: u64, user: &str, action: &str) -> AuditEntry {
        AuditEntry {
            id: Uuid::now_v7(),
            time,
            node: "n1".into(),
            user: Some(user.into()),
            user_id: None,
            role: None,
            auth: AuditAuth::Session,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms".into(),
            action: action.into(),
            target: None,
            status: 201,
            duration_ms: 3,
            detail: None,
        }
    }

    fn all(limit: u32) -> AuditQuery {
        AuditQuery {
            since: 0,
            until: u64::MAX,
            user: None,
            action: None,
            limit,
        }
    }

    fn times(entries: &[AuditEntry]) -> Vec<u64> {
        entries.iter().map(|e| e.time).collect()
    }

    fn lines(path: &Path) -> Vec<AuditEntry> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// 한 줄의 크기(시각이 같은 자릿수면 같다)
    fn line_len(time: u64) -> u64 {
        serde_json::to_string(&entry(time, "alice", "POST /vms"))
            .unwrap()
            .len() as u64
            + 1
    }

    #[tokio::test]
    async fn writes_and_reads_back_with_private_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let mut first = entry(1_000, "alice", "POST /vms");
        // node는 이 노드의 이름으로 바뀐다.
        first.node = "other".into();
        log.record(first.clone());
        log.record(entry(2_000, "bob", "DELETE /vms/{key}"));
        log.shutdown().await;
        // shutdown 뒤의 항목은 적지 않는다.
        log.record(entry(3_000, "carol", "POST /vms"));

        let dir_mode = std::fs::metadata(&config.dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        let file = config.dir.join("audit.log");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let read = log.read(all(10)).await.unwrap();
        assert_eq!(times(&read), [2_000, 1_000]);
        assert_eq!(read[1].node, "n1");
        assert_eq!(read[1].id, first.id);
        assert_eq!(lines(&file).len(), 2);
    }

    #[tokio::test]
    async fn disabled_writes_nothing_and_reads_empty() {
        let log = AuditLog::disabled();
        log.record(entry(1, "alice", "POST /vms"));
        log.shutdown().await;
        assert!(log.read(all(10)).await.unwrap().is_empty());
    }

    #[test]
    fn rotates_by_size_and_removes_the_oldest() {
        let tmp = tempfile::tempdir().unwrap();
        let line = line_len(1_000);
        // 한 파일에 세 줄, 파일은 모두 세 개
        let config = config(tmp.path(), line * 3, 3);
        std::fs::create_dir_all(&config.dir).unwrap();
        let mut writer = Writer::open(&config).unwrap();
        for t in 1_000..1_010 {
            writer.write(&entry(t, "alice", "POST /vms"));
        }
        writer.sync();
        let dir = &config.dir;
        assert_eq!(times(&lines(&dir.join("audit.log"))), [1_009]);
        assert_eq!(
            times(&lines(&dir.join("audit.log.1"))),
            [1_006, 1_007, 1_008]
        );
        assert_eq!(
            times(&lines(&dir.join("audit.log.2"))),
            [1_003, 1_004, 1_005]
        );
        assert!(!dir.join("audit.log.3").exists());
        let mode = std::fs::metadata(dir.join("audit.log"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        // 회전한 파일까지 새 것부터 읽는다.
        let read = read_dir(dir, &all(100)).unwrap();
        assert_eq!(times(&read), (1_003..1_010).rev().collect::<Vec<_>>());
    }

    #[test]
    fn a_single_file_is_replaced_when_full() {
        let tmp = tempfile::tempdir().unwrap();
        let line = line_len(1_000);
        let config = config(tmp.path(), line * 2, 1);
        std::fs::create_dir_all(&config.dir).unwrap();
        let mut writer = Writer::open(&config).unwrap();
        for t in 1_000..1_005 {
            writer.write(&entry(t, "alice", "POST /vms"));
        }
        let dir = &config.dir;
        assert_eq!(times(&lines(&dir.join("audit.log"))), [1_004]);
        assert!(!dir.join("audit.log.1").exists());
    }

    /// 파일들을 직접 만든다(각 목록은 오래된 것부터).
    fn write_files(dir: &Path, files: &[&[AuditEntry]]) {
        std::fs::create_dir_all(dir).unwrap();
        for (n, entries) in files.iter().enumerate() {
            let text: String = entries
                .iter()
                .map(|e| serde_json::to_string(e).unwrap() + "\n")
                .collect();
            std::fs::write(file_path(dir, n), text).unwrap();
        }
    }

    #[test]
    fn filters_newest_first_up_to_limit_plus_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_files(
            dir,
            &[
                &[
                    entry(500, "alice", "POST /vms"),
                    entry(600, "bob", "POST /vms/{key}/actions/start"),
                    entry(700, "alice", "POST /vms/{key}/actions/stop"),
                ],
                &[
                    entry(200, "alice", "POST /vms/{key}/actions/start"),
                    entry(300, "alice", "DELETE /vms/{key}"),
                    entry(400, "alice", "POST /vms/{key}/actions/start"),
                ],
            ],
        );
        let q = AuditQuery {
            since: 250,
            until: 650,
            user: Some("alice".into()),
            action: Some("/actions/".into()),
            limit: 10,
        };
        assert_eq!(times(&read_dir(dir, &q).unwrap()), [400]);

        let q = AuditQuery {
            user: Some("alice".into()),
            ..all(2)
        };
        // limit + 1개(합칠 때 넘침을 알 수 있게)
        assert_eq!(times(&read_dir(dir, &q).unwrap()), [700, 500, 400]);
        assert_eq!(times(&read_dir(dir, &all(0)).unwrap()), [700]);
    }

    #[test]
    fn skips_broken_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let good = |t| serde_json::to_string(&entry(t, "alice", "POST /vms")).unwrap();
        let text = format!(
            "{}\nnot json\n{{\"id\":1}}\n{}\n{{\"trunc",
            good(1),
            good(2)
        );
        std::fs::write(dir.join("audit.log"), text).unwrap();
        assert_eq!(times(&read_dir(dir, &all(10)).unwrap()), [2, 1]);
    }

    #[test]
    fn stops_at_files_older_than_since_and_skips_newer_than_until() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_files(
            dir,
            &[
                &[entry(900, "alice", "x")],
                &[entry(500, "alice", "x"), entry(600, "alice", "x")],
                &[entry(100, "alice", "x")],
            ],
        );
        // 가장 새 것이 since 앞인 파일(.2)에서 멈추므로 더 오래된 망가진 파일(.3)은 읽지 않는다.
        std::fs::write(file_path(dir, 3), [0xff, 0xfe]).unwrap();
        let q = AuditQuery {
            since: 550,
            until: 800,
            ..all(10)
        };
        assert_eq!(times(&read_dir(dir, &q).unwrap()), [600]);
        // 멈추지 않으면 망가진(UTF-8이 아닌) 파일을 읽다가 오류가 난다.
        assert!(read_dir(dir, &all(10)).is_err());
    }

    #[tokio::test]
    async fn an_overflow_is_recorded_as_audit_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let (open, gate) = std::sync::mpsc::channel();
        let log = AuditLog::start(config(tmp.path(), 1 << 20, 8), 2, Some(gate)).unwrap();
        for t in 1..=5 {
            log.record(entry(t, "alice", "POST /vms"));
        }
        open.send(()).unwrap();
        log.shutdown().await;
        let file = tmp.path().join("audit/audit.log");
        let written = lines(&file);
        assert_eq!(written.len(), 3);
        let dropped = &written[0];
        assert_eq!(dropped.action, "audit.dropped");
        assert_eq!(dropped.detail.as_deref(), Some("3 entries dropped"));
        assert_eq!((dropped.status, dropped.method.as_str()), (0, ""));
        assert_eq!(dropped.node, "n1");
        assert_eq!(times(&written[1..]), [1, 2]);
    }

    #[tokio::test]
    async fn long_fields_are_cut_at_512_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let mut long = entry(1_000, &"u".repeat(600), "POST /vms");
        long.path = format!("/vms/{}", "a".repeat(60 * 1024));
        long.target = Some(opl_types::audit::AuditTarget {
            kind: "vms".into(),
            key: "k".repeat(1_000),
        });
        // UTF-8 글자 경계에서 자른다(한 글자 3바이트).
        long.detail = Some("가".repeat(300));
        log.record(long);
        log.shutdown().await;

        let file = config.dir.join("audit.log");
        let written = &lines(&file)[0];
        assert_eq!(written.path, format!("/vms/{}…", "a".repeat(507)));
        let key = &written.target.as_ref().unwrap().key;
        assert_eq!(*key, format!("{}…", "k".repeat(512)));
        let detail = written.detail.as_deref().unwrap();
        assert_eq!(detail, format!("{}…", "가".repeat(170)));
        assert_eq!(
            *written.user.as_ref().unwrap(),
            format!("{}…", "u".repeat(512))
        );
        assert!(std::fs::metadata(&file).unwrap().len() < 4096);
    }

    /// `ip`에서 온 인증 없는 항목(로그인 실패 같은)
    fn anonymous(time: u64, ip: &str) -> AuditEntry {
        AuditEntry {
            user: None,
            auth: AuditAuth::None,
            ip: ip.into(),
            ..entry(time, "-", "POST /auth/login")
        }
    }

    #[tokio::test]
    async fn unauthenticated_entries_are_limited_per_ip_and_window() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let start = Instant::now();
        // 한 창에 61개면 61번째는 적지 않는다. 다른 IP와 인증된 항목은 따로 센다.
        for t in 1..=61 {
            log.record_at(anonymous(t, "10.0.0.1"), start);
        }
        log.record_at(anonymous(100, "10.0.0.2"), start);
        for t in 200..300 {
            let signed_in = AuditEntry {
                ip: "10.0.0.1".into(),
                ..entry(t, "alice", "POST /vms")
            };
            log.record_at(signed_in, start);
        }
        log.record_at(anonymous(62, "10.0.0.1"), start + Duration::from_secs(59));
        // 다음 창에서 처음 적을 때 그 앞에 넘친 수를 적는다.
        log.record_at(
            anonymous(1_000, "10.0.0.1"),
            start + Duration::from_secs(60),
        );
        // 끝날 때 남은 넘친 수도 적는다.
        for t in 2_000..2_062 {
            log.record_at(anonymous(t, "10.0.0.3"), start);
        }
        log.shutdown().await;

        let written = lines(&config.dir.join("audit.log"));
        let from = |ip: &str| -> Vec<u64> {
            written
                .iter()
                .filter(|e| e.ip == ip && e.user.is_none() && e.action != "audit.suppressed")
                .map(|e| e.time)
                .collect()
        };
        assert_eq!(
            from("10.0.0.1"),
            (1..=60).chain([1_000]).collect::<Vec<_>>()
        );
        assert_eq!(from("10.0.0.2"), [100]);
        assert_eq!(from("10.0.0.3"), (2_000..2_060).collect::<Vec<_>>());
        assert_eq!(written.iter().filter(|e| e.user.is_some()).count(), 100);
        let at = written
            .iter()
            .position(|e| e.action == "audit.suppressed")
            .unwrap();
        let first = &written[at];
        assert_eq!(first.ip, "10.0.0.1");
        assert_suppressed(first, 2);
        assert_eq!(
            (first.method.as_str(), first.path.as_str(), first.status),
            ("", "", 0)
        );
        assert_eq!((first.node.as_str(), &first.user), ("n1", &None));
        assert_eq!(written[at + 1].time, 1_000);
        let last = written.last().unwrap();
        assert_eq!(last.action, "audit.suppressed");
        assert_eq!(last.ip, "10.0.0.3");
        assert_suppressed(last, 2);
    }

    /// `n`개를 적지 않았다는 항목이고, `detail`의 창 시작이 RFC 3339다.
    fn assert_suppressed(entry: &AuditEntry, n: u64) -> u64 {
        assert_eq!(entry.action, "audit.suppressed");
        let detail = entry.detail.as_deref().unwrap();
        let prefix = format!("{n} unauthenticated requests not recorded since ");
        let since = detail.strip_prefix(&prefix).expect(detail);
        let since =
            time::OffsetDateTime::parse(since, &time::format_description::well_known::Rfc3339)
                .expect(since);
        (since.unix_timestamp() * 1000) as u64
    }

    #[tokio::test]
    async fn an_ended_window_is_written_without_new_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        // 61초 전에 시작한(이미 끝난) 창에서 넘쳤고, 그 IP는 다시 오지 않는다.
        let past = Instant::now().checked_sub(Duration::from_secs(61)).unwrap();
        for t in 1..=62 {
            log.record_at(anonymous(t, "10.0.0.1"), past);
        }
        // 넘치지 않은 끝난 창은 적지 않고 지운다.
        log.record_at(anonymous(100, "10.0.0.2"), past);
        let file = config.dir.join("audit.log");
        let deadline = Instant::now() + Duration::from_secs(5);
        let suppressed = loop {
            let written = lines(&file);
            if let Some(e) = written.iter().find(|e| e.action == "audit.suppressed") {
                break e.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no audit.suppressed: {written:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(
            (suppressed.ip.as_str(), suppressed.node.as_str()),
            ("10.0.0.1", "n1")
        );
        let since = assert_suppressed(&suppressed, 2);
        let ago = now_ms() - since;
        assert!((59_000..=64_000).contains(&ago), "{ago} ms ago");
        {
            let table = log.inner.as_ref().unwrap().unauthenticated.lock().unwrap();
            assert!(table.windows.is_empty() && table.order.is_empty());
        }
        log.shutdown().await;
        let written = lines(&file);
        let count = |action: &str| written.iter().filter(|e| e.action == action).count();
        assert_eq!(count("audit.suppressed"), 1);
        assert_eq!(count("POST /auth/login"), 61);
    }

    #[test]
    fn a_sweep_takes_ended_windows_only() {
        let mut table = Unauthenticated::default();
        let start = Instant::now();
        for _ in 0..=UNAUTHENTICATED_PER_WINDOW {
            table.admit("10.0.0.1", start);
        }
        table.admit("10.0.0.2", start);
        table.admit("10.0.0.3", start + Duration::from_secs(30));
        assert_eq!(table.sweep(start + Duration::from_secs(59)), []);
        assert_eq!(
            table.sweep(start + Duration::from_secs(60)),
            [Suppressed {
                key: "10.0.0.1".into(),
                count: 1,
                since: start
            }]
        );
        assert_eq!(table.windows.keys().collect::<Vec<_>>(), ["10.0.0.3"]);
        assert_eq!(table.order.len(), 1);
    }

    #[tokio::test]
    async fn ipv6_is_limited_per_slash_64() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let log = AuditLog::open(config.clone()).unwrap();
        let now = Instant::now();
        // 같은 /64 안의 다른 주소들은 한 한도를 나눠 쓴다.
        for t in 1..=61 {
            log.record_at(anonymous(t, &format!("2001:db8:1:2::{t:x}")), now);
        }
        log.record_at(anonymous(100, "2001:db8:1:3::1"), now);
        log.shutdown().await;
        let written = lines(&config.dir.join("audit.log"));
        let logins: Vec<&str> = written
            .iter()
            .filter(|e| e.action != "audit.suppressed")
            .map(|e| e.ip.as_str())
            .collect();
        assert_eq!(logins.len(), 61);
        // 항목의 ip는 실제 주소 그대로다.
        assert_eq!(logins[0], "2001:db8:1:2::1");
        assert_eq!(logins[59], "2001:db8:1:2::3c");
        assert_eq!(logins[60], "2001:db8:1:3::1");
        let suppressed = written.last().unwrap();
        assert_eq!(suppressed.ip, "2001:db8:1:2::/64");
        assert_suppressed(suppressed, 1);
        assert_eq!(unauthenticated_key("10.0.0.1"), "10.0.0.1");
        assert_eq!(unauthenticated_key("::ffff:10.0.0.1"), "10.0.0.1");
        assert_eq!(unauthenticated_key("not an ip"), "not an ip");
    }

    #[test]
    fn the_oldest_window_goes_when_too_many_ips_are_counted() {
        let mut table = Unauthenticated::default();
        let start = Instant::now();
        for _ in 0..=UNAUTHENTICATED_PER_WINDOW {
            table.admit("10.0.0.1", start);
        }
        for n in 1..UNAUTHENTICATED_IPS {
            let later = start + Duration::from_secs(1);
            assert_eq!(table.admit(&format!("ip-{n}"), later), (true, vec![]));
        }
        // 가장 오래된 창(10.0.0.1)을 버리면서 그 넘친 수를 바로 적게 돌려준다.
        assert_eq!(
            table.admit("10.9.9.9", start + Duration::from_secs(2)),
            (
                true,
                vec![Suppressed {
                    key: "10.0.0.1".into(),
                    count: 1,
                    since: start
                }]
            )
        );
        assert_eq!(table.windows.len(), UNAUTHENTICATED_IPS);
        assert_eq!(table.order.len(), UNAUTHENTICATED_IPS);
        assert!(!table.windows.contains_key("10.0.0.1"));
    }

    #[test]
    fn an_entry_in_two_files_is_read_once() {
        // 읽는 중에 회전하면 같은 항목이 두 파일에서 보일 수 있다.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let moved = entry(500, "alice", "POST /vms");
        write_files(
            dir,
            &[
                std::slice::from_ref(&moved),
                &[entry(400, "alice", "POST /vms"), moved.clone()],
            ],
        );
        assert_eq!(times(&read_dir(dir, &all(10)).unwrap()), [500, 400]);
    }

    #[test]
    fn opening_removes_files_beyond_max_files() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 2);
        let old = |t| vec![entry(t, "alice", "POST /vms")];
        write_files(&config.dir, &[&old(4), &old(3), &old(2), &old(1)]);
        Writer::open(&config).unwrap();
        assert!(config.dir.join("audit.log.1").exists());
        assert!(!config.dir.join("audit.log.2").exists());
        assert!(!config.dir.join("audit.log.3").exists());
        assert_eq!(times(&read_dir(&config.dir, &all(10)).unwrap()), [4, 3]);
    }

    #[test]
    fn opening_tightens_permissions_and_refuses_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config(tmp.path(), 1 << 20, 8);
        let mode = |path: &Path| {
            std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        let chmod = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        std::fs::create_dir_all(&config.dir).unwrap();
        chmod(&config.dir, 0o755);
        let file = config.dir.join("audit.log");
        std::fs::write(&file, "").unwrap();
        chmod(&file, 0o644);
        drop(AuditLog::open(config.clone()).unwrap());
        assert_eq!(mode(&config.dir), 0o700);
        assert_eq!(mode(&file), 0o600);

        // 지금 파일이 심볼릭 링크면 열지 않는다.
        std::fs::remove_file(&file).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::write(&elsewhere, "").unwrap();
        chmod(&elsewhere, 0o644);
        std::os::unix::fs::symlink(&elsewhere, &file).unwrap();
        assert!(AuditLog::open(config).is_err());
        assert_eq!(mode(&elsewhere), 0o644);
    }

    #[test]
    fn shutdown_gives_up_when_the_writer_is_stuck() {
        let tmp = tempfile::tempdir().unwrap();
        let (open, gate) = std::sync::mpsc::channel();
        let log = AuditLog::start(config(tmp.path(), 1 << 20, 8), 1, Some(gate)).unwrap();
        // 쓰기 스레드가 멈춘 채 채널이 찼다.
        log.record(entry(1, "alice", "POST /vms"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let started = Instant::now();
        runtime.block_on(log.shutdown_within(Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(5));
        // 막힌 blocking 태스크가 남았으면 런타임을 떨어뜨릴 때 그것을 기다린다.
        let (dropped, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(runtime);
            let _ = dropped.send(());
        });
        let finished = done.recv_timeout(Duration::from_secs(5)).is_ok();
        open.send(()).unwrap();
        assert!(finished, "dropping the runtime waited for a blocked task");
    }
}
```

`crates/opl-api/src/lib.rs`에서

```rust

pub mod auth;
```

을 다음으로 바꾼다:

```rust

pub mod audit;
pub mod auth;
```

`crates/opl-api/src/lib.rs`에서

```rust

pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
```

을 다음으로 바꾼다:

```rust

pub use audit::{AuditConfig, AuditLog};
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
```

`crates/opl-api/src/limit.rs`에서

```rust
/// 쉽게 바꿔 쓸 수 있는 주소 묶음이다).
fn counted_ip(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
```

을 다음으로 바꾼다:

```rust
/// 쉽게 바꿔 쓸 수 있는 주소 묶음이다).
pub(crate) fn counted_ip(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
```

`crates/opl-api/src/state.rs`에서

```rust

use crate::auth::{Authorizer, RoleAuthorizer, Sessions};
```

을 다음으로 바꾼다:

```rust

use crate::audit::AuditLog;
use crate::auth::{Authorizer, RoleAuthorizer, Sessions};
```

`crates/opl-api/src/state.rs`에서

```rust
    pub import_credentials: Arc<ImportCredentials>,
}
```

을 다음으로 바꾼다:

```rust
    pub import_credentials: Arc<ImportCredentials>,
    /// 이 노드의 감사 로그. 기본은 꺼져 있다(opld가 연다).
    pub audit: Arc<AuditLog>,
}
```

`crates/opl-api/src/state.rs`에서

```rust
            import_credentials: Arc::default(),
        }
```

을 다음으로 바꾼다:

```rust
            import_credentials: Arc::default(),
            audit: Arc::new(AuditLog::disabled()),
        }
```

`crates/opl-types/src/audit.rs`를 만든다:

```rust
//! 감사 로그 항목. 각 노드가 바꾸는 API 요청·로그인·콘솔 열기를 한 줄 JSON으로 적는다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Role;

/// 감사 로그 한 줄
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditEntry {
    /// UUID v7
    pub id: Uuid,
    /// 유닉스 밀리초
    pub time: u64,
    /// 적은 노드의 이름
    pub node: String,
    /// 사용자 이름. 인증 전이나 실패면 없다.
    pub user: Option<String>,
    pub user_id: Option<Uuid>,
    pub role: Option<Role>,
    pub auth: AuditAuth,
    /// 클라이언트 IP
    pub ip: String,
    pub method: String,
    /// 쿼리를 뺀 경로(`/api/v1` 아래)
    pub path: String,
    /// `"<METHOD> <라우트 템플릿>"`(예: `POST /vms/{key}/actions/start`)
    pub action: String,
    pub target: Option<AuditTarget>,
    /// 응답 상태
    pub status: u16,
    pub duration_ms: u64,
    /// 핸들러가 남긴 짧은 문자열(비밀은 넣지 않는다)
    pub detail: Option<String>,
}

/// 요청을 인증한 방식
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AuditAuth {
    Session,
    Token,
    None,
}

/// 요청의 대상(라우트 템플릿의 첫 경로 조각과 첫 매개변수 값)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditTarget {
    /// 예: `vms`
    pub kind: String,
    /// 예: `web-1`
    pub key: String,
}

/// 감사 로그 거르기
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditQuery {
    /// 이 시각(유닉스 밀리초)부터(포함)
    pub since: u64,
    /// 이 시각까지(포함)
    pub until: u64,
    /// 사용자 이름이 정확히 같은 것
    pub user: Option<String>,
    /// `action`에 이 문자열이 들어 있는 것
    pub action: Option<String>,
    /// 돌려줄 최대 개수
    pub limit: u32,
}

impl AuditQuery {
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        (self.since..=self.until).contains(&entry.time)
            && self
                .user
                .as_deref()
                .is_none_or(|u| entry.user.as_deref() == Some(u))
            && self
                .action
                .as_deref()
                .is_none_or(|a| entry.action.contains(a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(time: u64, user: Option<&str>, action: &str) -> AuditEntry {
        AuditEntry {
            id: Uuid::now_v7(),
            time,
            node: "n1".into(),
            user: user.map(Into::into),
            user_id: None,
            role: None,
            auth: AuditAuth::None,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms/web-1/actions/start".into(),
            action: action.into(),
            target: None,
            status: 200,
            duration_ms: 1,
            detail: None,
        }
    }

    fn query() -> AuditQuery {
        AuditQuery {
            since: 100,
            until: 200,
            user: None,
            action: None,
            limit: 10,
        }
    }

    #[test]
    fn time_range_is_inclusive() {
        let q = query();
        assert!(q.matches(&entry(100, None, "x")));
        assert!(q.matches(&entry(200, None, "x")));
        assert!(!q.matches(&entry(99, None, "x")));
        assert!(!q.matches(&entry(201, None, "x")));
    }

    #[test]
    fn user_must_match_exactly() {
        let q = AuditQuery {
            user: Some("alice".into()),
            ..query()
        };
        assert!(q.matches(&entry(150, Some("alice"), "x")));
        assert!(!q.matches(&entry(150, Some("alice2"), "x")));
        assert!(!q.matches(&entry(150, None, "x")));
    }

    #[test]
    fn action_matches_a_substring() {
        let q = AuditQuery {
            action: Some("/vms/".into()),
            ..query()
        };
        assert!(q.matches(&entry(150, None, "POST /vms/{key}/actions/start")));
        assert!(!q.matches(&entry(150, None, "POST /auth/login")));
    }

    #[test]
    fn entry_serializes_auth_in_snake_case() {
        let json = serde_json::to_value(entry(1, None, "x")).unwrap();
        assert_eq!(json["auth"], "none");
        assert_eq!(json["user"], serde_json::Value::Null);
        let back: AuditEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back.auth, AuditAuth::None);
    }
}
```

`crates/opl-types/src/lib.rs`에서

```rust

pub mod auth;
```

을 다음으로 바꾼다:

```rust

pub mod audit;
pub mod auth;
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo check --workspace`
Expected: `Cargo.lock`의 opl-api 의존 목록 줄만 바뀐다(새 패키지 없음, 이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1271개, 실패 0

- [ ] **Step 4: 커밋**

```bash
git add Cargo.toml Cargo.lock crates
git commit -m "feat(audit): 감사 항목 타입과 파일 기록·읽기 — opl-types audit(AuditEntry·AuditAuth·AuditTarget·AuditQuery::matches), opl-api audit의 AuditConfig·AuditLog(disabled·open·record·read·shutdown): 용량 1만 채널과 쓰기 스레드, {dir}/audit.log 한 줄 JSON(디렉터리 0700·파일 0600), 크기 회전과 개수 한도, 1초 묶음 sync_data, 넘침은 다음 항목 앞에 audit.dropped, tracing 대상 opl_audit, 새 파일부터 거꾸로 읽어 limit+1개(망가진 줄 건너뜀, since 앞 파일에서 멈춤), AppState.audit(기본 꺼짐)"
```

---

### Task 2: 감사 미들웨어와 기록 지점, opld [audit]

`opl_api::router`의 `route_layer` 미들웨어가 라우트가 맞은 바꾸는 요청(GET·HEAD·OPTIONS 밖)과 콘솔 열기를 적는다. 동작은 `<METHOD> <라우트 템플릿>`, 대상은 첫 경로 조각과 첫 매개변수 값, 경로는 쿼리 없이. `Principal` 추출기가 주체와 인증 방식을 요청의 칸에 적고, 핸들러는 `AuditNote`로 detail을 남긴다(로그인 `username=…`, 로그아웃의 사용자, 구독 PUT `id=…`; 비밀번호·키는 넣지 않음). opld `[audit] enabled·max_file_mib(1~256)·max_files`, `{data_dir}/audit`, 로그 색은 터미널일 때만(journald 줄에 이스케이프 코드 없음).

**Files:**
- Create: `crates/opl-api/tests/audit.rs`
- Modify: `crates/opl-api/src/audit.rs`
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/subscription.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/audit.rs`에서

```rust
//! 파일이 커지면 `audit.log.1`, `.2`… 로 밀고, 읽을 때는 새 파일부터 거꾸로 훑는다.

use std::fs::{File, OpenOptions};
```

을 다음으로 바꾼다:

```rust
//! 파일이 커지면 `audit.log.1`, `.2`… 로 밀고, 읽을 때는 새 파일부터 거꾸로 훑는다.
//! 항목은 미들웨어 [`layer`]가 바꾸는 요청마다 만든다. 인증 추출기와 핸들러는 요청의
//! [`AuditSlot`]에 주체와 짧은 설명을 남긴다.

use std::convert::Infallible;
use std::fs::{File, OpenOptions};
```

`crates/opl-api/src/audit.rs`에서

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequestParts, MatchedPath, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::Response;
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery, AuditTarget};
use uuid::Uuid;

use crate::auth::Principal;
use crate::limit::ClientIp;
use crate::state::AppState;
```

`crates/opl-api/src/audit.rs`에서

```rust
            tracing::warn!("the audit log did not finish writing within {timeout:?}; giving up");
        }
    }
}

/// 쓰기 스레드
```

을 다음으로 바꾼다:

```rust
            tracing::warn!("the audit log did not finish writing within {timeout:?}; giving up");
        }
    }
}

/// 요청 하나의 감사 정보. 미들웨어가 요청 확장에 넣고, 인증 추출기와 핸들러가 채운다.
#[derive(Clone, Default)]
pub(crate) struct AuditSlot(Arc<Mutex<SlotData>>);

#[derive(Default)]
struct SlotData {
    principal: Option<Principal>,
    auth: Option<AuditAuth>,
    detail: Option<String>,
}

impl AuditSlot {
    fn set_principal(&self, principal: Principal, auth: AuditAuth) {
        let mut data = self.0.lock().unwrap();
        data.principal = Some(principal);
        data.auth = Some(auth);
    }
}

/// 인증 결과를 요청의 slot에 적는다. 실패해도 Bearer 토큰으로 시도했으면 그것은 남긴다.
pub(crate) fn note_auth(parts: &Parts, principal: Option<&Principal>) {
    let Some(slot) = parts.extensions.get::<AuditSlot>() else {
        return;
    };
    let auth = if bearer(&parts.headers) {
        AuditAuth::Token
    } else {
        AuditAuth::Session
    };
    match principal {
        Some(p) => slot.set_principal(p.clone(), auth),
        None if auth == AuditAuth::Token => slot.0.lock().unwrap().auth = Some(auth),
        None => {}
    }
}

fn bearer(headers: &HeaderMap) -> bool {
    headers
        .get(AUTHORIZATION)
        .is_some_and(|v| v.as_bytes().starts_with(b"Bearer "))
}

/// 핸들러가 감사 항목에 남기는 것. 미들웨어 없이 불린 라우터에서는 아무것도 하지 않는다.
pub struct AuditNote(Option<AuditSlot>);

impl AuditNote {
    /// 짧은 설명(비밀은 넣지 않는다)
    pub(crate) fn detail(&self, detail: impl Into<String>) {
        if let Some(slot) = &self.0 {
            slot.0.lock().unwrap().detail = Some(detail.into());
        }
    }

    /// 요청의 주체(로그인·로그아웃처럼 인증 추출기를 쓰지 않는 핸들러)
    pub(crate) fn principal(&self, principal: Principal, auth: AuditAuth) {
        if let Some(slot) = &self.0 {
            slot.set_principal(principal, auth);
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuditNote {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.extensions.get::<AuditSlot>().cloned()))
    }
}

/// 라우트가 맞은 요청 가운데 바꾸는 것(GET·HEAD·OPTIONS가 아닌 것)과 콘솔 열기를 적는다.
pub(crate) async fn layer(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(template) = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| api_path(m.as_str()).to_string())
    else {
        return next.run(req).await;
    };
    let method = req.method().clone();
    let read = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    if read && template != "/vms/{key}/console" {
        return next.run(req).await;
    }
    let path = api_path(req.uri().path()).to_string();
    let slot = AuditSlot::default();
    req.extensions_mut().insert(slot.clone());
    let start = Instant::now();
    let response = next.run(req).await;
    let data = std::mem::take(&mut *slot.0.lock().unwrap());
    let principal = data.principal;
    state.audit.record(AuditEntry {
        id: Uuid::now_v7(),
        time: now_ms(),
        // record가 이 노드의 이름으로 채운다.
        node: String::new(),
        user: principal.as_ref().map(|p| p.name.clone()),
        user_id: principal.as_ref().map(|p| p.user),
        role: principal.as_ref().map(|p| p.role),
        auth: data.auth.unwrap_or(AuditAuth::None),
        ip: ip.to_canonical().to_string(),
        action: format!("{method} {template}"),
        target: target(&template, &path),
        method: method.to_string(),
        path,
        status: response.status().as_u16(),
        duration_ms: start.elapsed().as_millis() as u64,
        detail: data.detail,
    });
    response
}

/// `/api/v1` 아래의 경로
fn api_path(path: &str) -> &str {
    path.strip_prefix("/api/v1").unwrap_or(path)
}

/// 템플릿의 첫 조각과, 템플릿의 첫 `{…}` 자리에 있는 실제 경로 조각. 매개변수가 없으면 None.
fn target(template: &str, path: &str) -> Option<AuditTarget> {
    let mut segments = template.split('/').filter(|s| !s.is_empty());
    let kind = segments.next()?;
    let index = template.split('/').position(|s| s.starts_with('{'))?;
    let key = path.split('/').nth(index)?;
    Some(AuditTarget {
        kind: kind.into(),
        key: key.into(),
    })
}

/// 쓰기 스레드
```

`crates/opl-api/src/audit.rs`에서

```rust
            + 1
    }
```

을 다음으로 바꾼다:

```rust
            + 1
    }

    #[test]
    fn target_is_the_first_segment_and_the_first_parameter() {
        let t = |template, path| target(template, path).map(|t| (t.kind, t.key));
        assert_eq!(
            t("/vms/{key}/actions/{action}", "/vms/web-1/actions/start"),
            Some(("vms".into(), "web-1".into()))
        );
        assert_eq!(
            t("/vms/{key}/snapshots/{name}", "/vms/web-1/snapshots/s1"),
            Some(("vms".into(), "web-1".into()))
        );
        assert_eq!(t("/users", "/users"), None);
        assert_eq!(t("/auth/login", "/auth/login"), None);
        assert_eq!(api_path("/api/v1/vms/{key}"), "/vms/{key}");
        assert_eq!(api_path("/vms"), "/vms");
    }
```

`crates/opl-api/src/auth.rs`에서

```rust
impl AppState {
    fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        Ok(self.cluster.get::<UserSpec>(user)?.map(principal_of))
```

을 다음으로 바꾼다:

```rust
impl AppState {
    pub(crate) fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        Ok(self.cluster.get::<UserSpec>(user)?.map(principal_of))
```

`crates/opl-api/src/auth.rs`에서

```rust
    ) -> Result<Self, Self::Rejection> {
        state.authenticate(parts).await
    }
```

을 다음으로 바꾼다:

```rust
    ) -> Result<Self, Self::Rejection> {
        let principal = state.authenticate(parts).await;
        crate::audit::note_auth(parts, principal.as_ref().ok());
        principal
    }
```

`crates/opl-api/src/lib.rs`에서

```rust
    let doc = Arc::new(doc);
    let api = api.route(
        "/openapi.json",
        get(move || {
            let doc = doc.clone();
            async move { axum::Json(doc.as_ref().clone()) }
        }),
    );
    Router::new().nest("/api/v1", api).with_state(state)
```

을 다음으로 바꾼다:

```rust
    let doc = Arc::new(doc);
    // 라우트가 맞은 요청만 감사 로그에 적는다(라우트 템플릿이 동작 이름이다).
    let api = api
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            audit::layer,
        ))
        .route(
            "/openapi.json",
            get(move || {
                let doc = doc.clone();
                async move { axum::Json(doc.as_ref().clone()) }
            }),
        );
    Router::new().nest("/api/v1", api).with_state(state)
```

`crates/opl-api/src/subscription.rs`에서

```rust

use crate::auth::{Action, Principal};
```

을 다음으로 바꾼다:

```rust

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
```

`crates/opl-api/src/subscription.rs`에서

```rust
    principal: Principal,
    Json(req): Json<SubscriptionKey>,
```

을 다음으로 바꾼다:

```rust
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<SubscriptionKey>,
```

`crates/opl-api/src/subscription.rs`에서

```rust
    tracing::info!(id = %license.id, "subscription key set");
    Ok(Json(info(&state.cluster, &settings)?))
```

을 다음으로 바꾼다:

```rust
    tracing::info!(id = %license.id, "subscription key set");
    audit.detail(format!("id={}", license.id));
    Ok(Json(info(&state.cluster, &settings)?))
```

`crates/opl-api/src/users.rs`에서

```rust
use axum::response::{IntoResponse, Response};
use opl_types::auth::{ApiToken, ApiTokenSpec, Role, User, UserSpec};
```

을 다음으로 바꾼다:

```rust
use axum::response::{IntoResponse, Response};
use opl_types::audit::AuditAuth;
use opl_types::auth::{ApiToken, ApiTokenSpec, Role, User, UserSpec};
```

`crates/opl-api/src/users.rs`에서

```rust

use crate::auth::{
```

을 다음으로 바꾼다:

```rust

use crate::audit::AuditNote;
use crate::auth::{
```

`crates/opl-api/src/users.rs`에서

```rust
    ClientIp(ip): ClientIp,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    // 이름 형식이 아니면 그런 사용자는 있을 수 없다. 해시를 계산하지 않고 IP만 실패로 센다(긴 이름을
    // 기록에 넣지 않는다). 있을 수 없는 이름이라 응답 시간으로 드러나는 것도 없다.
    if validate_name(&req.username).is_err() {
        state.attempts.begin_ip(ip)?;
```

을 다음으로 바꾼다:

```rust
    ClientIp(ip): ClientIp,
    audit: AuditNote,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    // 이름 형식이 아닌 것은 감사 로그에 넣지 않는다(비밀번호를 잘못 넣었을 수 있다).
    let name_ok = validate_name(&req.username).is_ok();
    audit.detail(if name_ok {
        format!("username={}", req.username)
    } else {
        "username=<invalid>".into()
    });
    // 이름 형식이 아니면 그런 사용자는 있을 수 없다. 해시를 계산하지 않고 IP만 실패로 센다(긴 이름을
    // 기록에 넣지 않는다). 있을 수 없는 이름이라 응답 시간으로 드러나는 것도 없다.
    if !name_ok {
        state.attempts.begin_ip(ip)?;
```

`crates/opl-api/src/users.rs`에서

```rust
    state.attempts.success(&req.username, ip, attempt);
    let session = state.sessions.create(user.id, user.spec.session_epoch);
```

을 다음으로 바꾼다:

```rust
    state.attempts.success(&req.username, ip, attempt);
    audit.principal(
        Principal {
            user: user.id,
            name: user.name.clone(),
            role: user.spec.role,
        },
        AuditAuth::Session,
    );
    let session = state.sessions.create(user.id, user.spec.session_epoch);
```

`crates/opl-api/src/users.rs`에서

```rust
        headers(("set-cookie" = String)))))]
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(id) = session_cookie(&headers) {
        state.sessions.remove(id);
```

을 다음으로 바꾼다:

```rust
        headers(("set-cookie" = String)))))]
pub async fn logout(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    headers: HeaderMap,
) -> Response {
    if let Some(id) = session_cookie(&headers) {
        // 지우기 전에 세션의 사용자를 감사 로그에 남긴다.
        if let Some((user, _)) = state.sessions.get(id)
            && let Ok(Some(principal)) = state.principal_for(user)
        {
            audit.principal(principal, AuditAuth::Session);
        }
        state.sessions.remove(id);
```

`crates/opl-api/tests/audit.rs`를 만든다:

```rust
//! 감사 로그 기록: 바꾸는 요청·로그인·로그아웃·콘솔 열기가 누가·어떻게·무엇을과 함께 적히고,
//! 읽는 요청과 비밀번호는 적히지 않는다.

mod common;

use std::time::Instant;

use common::{ADMIN_PASSWORD, Env, WAIT};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery, AuditTarget};
use opl_types::auth::Role;
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use reqwest::StatusCode;
use serde_json::json;

const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");

/// `found`가 참이 될 때까지 이 노드의 감사 로그를 다시 읽는다(쓰기 스레드가 곧 적는다).
async fn entries_until(env: &Env, found: impl Fn(&[AuditEntry]) -> bool) -> Vec<AuditEntry> {
    let start = Instant::now();
    loop {
        let entries = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        if found(&entries) {
            return entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `action`과 `status`가 같은 가장 새 항목
async fn entry(env: &Env, action: &str, status: u16) -> AuditEntry {
    let has = |e: &AuditEntry| e.action == action && e.status == status;
    entries_until(env, |es| es.iter().any(has))
        .await
        .into_iter()
        .find(has)
        .unwrap()
}

fn target(kind: &str, key: &str) -> Option<AuditTarget> {
    Some(AuditTarget {
        kind: kind.into(),
        key: key.into(),
    })
}

#[tokio::test]
async fn a_changing_request_is_recorded_with_who_and_what() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "carol", "password": "password-1", "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = entry(&env, "POST /users", 201).await;
    assert_eq!(created.user.as_deref(), Some("admin"));
    assert!(created.user_id.is_some());
    assert_eq!(created.role, Some(Role::Admin));
    assert_eq!(created.auth, AuditAuth::Token);
    assert_eq!(created.method, "POST");
    assert_eq!(created.path, "/users");
    assert_eq!(created.target, None);
    assert_eq!(created.ip, "127.0.0.1");
    assert_eq!(created.node, "node1");

    // 쿼리 문자열은 경로에 넣지 않는다. 대상은 템플릿의 첫 조각과 첫 매개변수 값이다.
    let (status, body) = admin.delete("/users/carol?reason=secret").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let deleted = entry(&env, "DELETE /users/{key}", 204).await;
    assert_eq!(deleted.path, "/users/carol");
    assert_eq!(deleted.target, target("users", "carol"));
    assert_eq!(deleted.user.as_deref(), Some("admin"));
}

#[tokio::test]
async fn denied_and_unauthenticated_requests_are_recorded() {
    let env = Env::start().await;
    let viewer = env.user("vic", "viewer").await;
    let (status, _) = viewer
        .post(
            "/users",
            json!({ "name": "x1", "password": "password-1", "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let denied = entry(&env, "POST /users", 403).await;
    assert_eq!(denied.user.as_deref(), Some("vic"));
    assert_eq!(denied.role, Some(Role::Viewer));
    assert_eq!(denied.auth, AuditAuth::Token);

    // 틀린 토큰: 사용자는 모르지만 토큰으로 시도한 것은 남는다.
    let mut forged = env.anonymous();
    forged.token = Some(format!("opl_{}_deadbeef", uuid::Uuid::now_v7().simple()));
    let (status, _) = forged.delete("/users/vic").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let bad = entry(&env, "DELETE /users/{key}", 401).await;
    assert_eq!(bad.user, None);
    assert_eq!(bad.user_id, None);
    assert_eq!(bad.auth, AuditAuth::Token);
    assert_eq!(bad.target, target("users", "vic"));

    // 자격 증명이 없는 요청
    let (status, _) = env
        .anonymous()
        .post("/tokens", json!({ "name": "t" }))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let anon = entry(&env, "POST /tokens", 401).await;
    assert_eq!((anon.user, anon.auth), (None, AuditAuth::None));
}

#[tokio::test]
async fn logins_are_recorded_without_the_password() {
    let env = Env::start().await;
    let http = reqwest::Client::new();
    let url = format!("{}/api/v1/auth/login", env.base);
    let wrong = "wrong-pass-7f3a";
    for (user, password, status) in [
        ("admin", wrong, 401),
        ("Not A Name", wrong, 401),
        ("admin", ADMIN_PASSWORD, 200),
    ] {
        let resp = http
            .post(&url)
            .json(&json!({ "username": user, "password": password }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), status, "{user}");
    }
    let entries = entries_until(&env, |es| {
        es.iter().filter(|e| e.action == "POST /auth/login").count() == 3
    })
    .await;
    let logins: Vec<&AuditEntry> = entries
        .iter()
        .filter(|e| e.action == "POST /auth/login")
        .collect();
    // 새 것부터
    let (ok, invalid, failed) = (logins[0], logins[1], logins[2]);
    assert_eq!(failed.status, 401);
    assert_eq!(failed.detail.as_deref(), Some("username=admin"));
    assert_eq!((&failed.user, failed.auth), (&None, AuditAuth::None));
    assert_eq!(invalid.status, 401);
    assert_eq!(invalid.detail.as_deref(), Some("username=<invalid>"));
    assert_eq!(ok.status, 200);
    assert_eq!(ok.detail.as_deref(), Some("username=admin"));
    assert_eq!(ok.user.as_deref(), Some("admin"));
    assert_eq!(ok.role, Some(Role::Admin));
    assert_eq!(ok.auth, AuditAuth::Session);

    env.state.audit.shutdown().await;
    let text = std::fs::read_to_string(env.dir.path().join("audit/audit.log")).unwrap();
    assert!(text.contains("username=admin"), "{text}");
    for secret in [wrong, ADMIN_PASSWORD] {
        assert!(!text.contains(secret), "{text}");
    }
}

#[tokio::test]
async fn session_requests_and_logout_name_the_user() {
    let env = Env::start().await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("{}/api/v1{p}", env.base);
    let resp = http
        .post(url("/auth/login"))
        .json(&json!({ "username": "admin", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    let cookie = resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let resp = http
        .post(url("/tokens"))
        .header("cookie", &cookie)
        .json(&json!({ "name": "t1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let token = entry(&env, "POST /tokens", 201).await;
    assert_eq!(token.user.as_deref(), Some("admin"));
    assert_eq!(token.auth, AuditAuth::Session);

    let resp = http
        .post(url("/auth/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let out = entry(&env, "POST /auth/logout", 204).await;
    assert_eq!(out.user.as_deref(), Some("admin"));
    assert_eq!(out.auth, AuditAuth::Session);

    // 세션이 없으면 사용자는 모른다.
    let resp = http.post(url("/auth/logout")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let entries = entries_until(&env, |es| {
        es.iter()
            .filter(|e| e.action == "POST /auth/logout")
            .count()
            == 2
    })
    .await;
    let anon = entries
        .iter()
        .find(|e| e.action == "POST /auth/logout")
        .unwrap();
    assert_eq!((&anon.user, anon.auth), (&None, AuditAuth::None));
}

#[tokio::test]
async fn reads_are_not_recorded_but_opening_a_console_is() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.get("/users").await.0, StatusCode::OK);
    assert_eq!(admin.get("/vms/web-1").await.0, StatusCode::NOT_FOUND);
    let (status, _) = admin.get("/vms/web-1/console").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let console = entry(&env, "GET /vms/{key}/console", 404).await;
    assert_eq!(console.target, target("vms", "web-1"));
    assert_eq!(console.user.as_deref(), Some("admin"));
    // 콘솔 앞의 GET은 적지 않았다(콘솔 항목은 그 뒤에 적혔다).
    let entries = entries_until(&env, |_| true).await;
    assert!(
        entries
            .iter()
            .all(|e| e.method != "GET" || e.action == "GET /vms/{key}/console"),
        "{entries:#?}"
    );
}

#[tokio::test]
async fn a_subscription_key_is_recorded_by_its_id() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes: 3,
            issued_at: now - 86_400,
            expires_at: now + 30 * 86_400,
        },
    )
    .unwrap();
    let (status, body) = admin
        .put("/subscription", json!({ "key": key }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let put = entry(&env, "PUT /subscription", 200).await;
    assert_eq!(put.detail.as_deref(), Some("id=sub-1"));
    // 나쁜 키는 detail이 없다.
    let (status, _) = admin
        .put("/subscription", json!({ "key": "nope" }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(entry(&env, "PUT /subscription", 400).await.detail, None);
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use opl_api::vm_imports::ImportCredentials;
use opl_api::{AppState, TaskRunner, TaskRunnerConfig, hash_password};
use opl_cluster::memnet::MemRouter;
```

을 다음으로 바꾼다:

```rust
use opl_api::vm_imports::ImportCredentials;
use opl_api::{AppState, AuditConfig, AuditLog, TaskRunner, TaskRunnerConfig, hash_password};
use opl_cluster::memnet::MemRouter;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        state.import_credentials = credentials;
        let state = Arc::new(state);
```

을 다음으로 바꾼다:

```rust
        state.import_credentials = credentials;
        // 감사 로그는 `{dir}/audit`에 적는다.
        state.audit = Arc::new(
            AuditLog::open(AuditConfig {
                dir: dir.path().join("audit"),
                node: "node1".into(),
                max_file_size: 1 << 20,
                max_files: 2,
            })
            .unwrap(),
        );
        let state = Arc::new(state);
```

`crates/opld/src/config.rs`에서

```rust
use opl_api::{
    BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig, TelemetryConfig,
    UpdatesConfig,
};
```

을 다음으로 바꾼다:

```rust
use opl_api::{
    AuditConfig, BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig,
    TelemetryConfig, UpdatesConfig,
};
```

`crates/opld/src/config.rs`에서

```rust
    pub telemetry: TelemetrySection,
}
```

을 다음으로 바꾼다:

```rust
    pub telemetry: TelemetrySection,
    #[serde(default)]
    pub audit: AuditSection,
}
```

`crates/opld/src/config.rs`에서

```rust

fn default_telemetry_tick() -> Duration {
```

을 다음으로 바꾼다:

```rust

/// 감사 로그(`{data_dir}/audit/audit.log`). API를 여는 노드만 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSection {
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,
    /// 한 파일의 최대 크기(MiB). 넘으면 회전한다.
    #[serde(default = "default_audit_max_file_mib")]
    pub max_file_mib: u64,
    /// 지금 파일을 포함해 남기는 파일 수
    #[serde(default = "default_audit_max_files")]
    pub max_files: usize,
}

impl Default for AuditSection {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            max_file_mib: default_audit_max_file_mib(),
            max_files: default_audit_max_files(),
        }
    }
}

impl AuditSection {
    /// API의 감사 로그 설정. 꺼졌으면 None이다.
    pub fn audit_config(&self, data_dir: &Path, node: &str) -> Option<AuditConfig> {
        self.enabled.then(|| AuditConfig {
            dir: data_dir.join("audit"),
            node: node.into(),
            max_file_size: self.max_file_mib << 20,
            max_files: self.max_files,
        })
    }
}

fn default_audit_enabled() -> bool {
    true
}
fn default_audit_max_file_mib() -> u64 {
    16
}
fn default_audit_max_files() -> usize {
    8
}

fn default_telemetry_tick() -> Duration {
```

`crates/opld/src/config.rs`에서

```rust
            anyhow::bail!("pools.refresh_secs must be at least 1");
        }
```

을 다음으로 바꾼다:

```rust
            anyhow::bail!("pools.refresh_secs must be at least 1");
        }
        // 감사 로그를 읽을 때 파일을 통째로 메모리에 올리므로 256 MiB까지만 받는다.
        if self.audit.max_file_mib == 0
            || self.audit.max_files == 0
            || self.audit.max_file_mib > 256
        {
            anyhow::bail!(
                "audit.max_file_mib must be between 1 and 256 and audit.max_files at least 1"
            );
        }
```

`crates/opld/src/config.rs`에서

```rust
    #[test]
    fn telemetry_section_defaults_and_overrides() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn audit_section_defaults_and_rejects_zero() {
        let c = parse("name = \"n1\"\naddress = \"h\"\ndata_dir = \"/d\"");
        assert_eq!(c.audit, AuditSection::default());
        let a = c.audit.audit_config(&c.data_dir, &c.name).unwrap();
        assert_eq!(a.dir, Path::new("/d/audit"));
        assert_eq!(a.node, "n1");
        assert_eq!(a.max_file_size, 16 << 20);
        assert_eq!(a.max_files, 8);

        let c = parse("name = \"n\"\naddress = \"h\"\n[audit]\nmax_file_mib = 1\nmax_files = 2");
        c.validate().unwrap();
        // 읽을 때 파일을 통째로 메모리에 올리므로 256 MiB까지
        parse("name = \"n\"\naddress = \"h\"\n[audit]\nmax_file_mib = 256")
            .validate()
            .unwrap();
        let a = c.audit.audit_config(&c.data_dir, &c.name).unwrap();
        assert_eq!((a.max_file_size, a.max_files), (1 << 20, 2));
        let off = parse("name = \"n\"\naddress = \"h\"\n[audit]\nenabled = false");
        assert!(off.audit.audit_config(&off.data_dir, &off.name).is_none());

        for bad in ["max_file_mib = 0", "max_files = 0", "max_file_mib = 257"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[audit]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[audit]\ndir = \"/x\"")
                .is_err()
        );
    }

    #[test]
    fn telemetry_section_defaults_and_overrides() {
```

`crates/opld/src/main.rs`에서

```rust
use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
```

을 다음으로 바꾼다:

```rust
use std::fs::File;
use std::io::IsTerminal;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
```

`crates/opld/src/main.rs`에서

```rust
        )
        .init();
```

을 다음으로 바꾼다:

```rust
        )
        // journald나 파일로 갈 때는 색 코드를 넣지 않는다(감사 줄을 그대로 전달할 수 있게).
        .with_ansi(std::io::stdout().is_terminal())
        .init();
```

`crates/opld/src/member.rs`에서

```rust
use opl_api::{
    AppState, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig,
    IncomingMigrations, SnapshotScheduler, TaskRunner, TaskRunnerConfig, TelemetryReporter,
    UefiVarsFetcher, UpdateAgent, Updates,
};
```

을 다음으로 바꾼다:

```rust
use opl_api::{
    AppState, AuditLog, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig,
    IncomingConfig, IncomingMigrations, SnapshotScheduler, TaskRunner, TaskRunnerConfig,
    TelemetryReporter, UefiVarsFetcher, UpdateAgent, Updates,
};
```

`crates/opld/src/member.rs`에서

```rust
    telemetry: TelemetryReporter,
}
```

을 다음으로 바꾼다:

```rust
    telemetry: TelemetryReporter,
    /// API가 멈춘 뒤 남은 감사 항목을 쓰고 닫는다.
    audit: Arc<AuditLog>,
}
```

`crates/opld/src/member.rs`에서

```rust
        }
        self.monitor.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        }
        self.audit.shutdown().await;
        self.monitor.shutdown().await;
```

`crates/opld/src/member.rs`에서

```rust
            state.secure_cookies = true;
            let created = match password {
```

을 다음으로 바꾼다:

```rust
            state.secure_cookies = true;
            if let Some(audit) = config.audit.audit_config(&config.data_dir, &config.name) {
                let dir = audit.dir.clone();
                state.audit =
                    Arc::new(AuditLog::open(audit).with_context(|| {
                        format!("cannot open the audit log in {}", dir.display())
                    })?);
            }
            let created = match password {
```

`crates/opld/src/member.rs`에서

```rust
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
        let app = opl_api::router(state).merge(crate::web::router());
```

을 다음으로 바꾼다:

```rust
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
        let audit = state.audit.clone();
        let app = opl_api::router(state).merge(crate::web::router());
```

`crates/opld/src/member.rs`에서

```rust
                telemetry,
                recovery,
```

을 다음으로 바꾼다:

```rust
                telemetry,
                audit,
                recovery,
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1279개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(audit): 감사 미들웨어와 기록 지점, opld [audit] — router의 route_layer로 라우트가 맞은 바꾸는 요청(GET·HEAD·OPTIONS 제외)과 콘솔 GET을 적음(action \"<METHOD> <템플릿>\", target은 첫 조각과 첫 매개변수 값, 쿼리 없는 path, 상태·걸린 시간·클라이언트 IP). 요청의 AuditSlot에 Principal 추출기가 주체와 auth(Bearer면 token, 아니면 session; 실패해도 Bearer면 token)를 적고, 핸들러는 AuditNote로 detail·주체를 남김: 로그인 username=<이름>|<invalid>와 성공한 사용자(비밀번호는 넣지 않음), 로그아웃은 세션의 사용자, 구독 PUT은 id=<키 id>. opld [audit] enabled·max_file_mib(16)·max_files(8), 0이면 시작 오류, {data_dir}/audit에 열어 AppState.audit에 넣고 API를 멈춘 뒤 shutdown. 시험 Env는 {dir}/audit에 감사 로그를 씀"
```

---

### Task 3: 모아 읽기 API와 노드 사이 RPC

`GET /api/v1/audit`(admin): 위트니스가 아닌 모든 멤버의 감사 로그를 노드 사이 RPC `ReadAudit`(opl-peer `AuditSource`·`PeerService::with_audit`·`Peers::read_audit`, 5초 제한)로 읽어 시간 내림차순으로 합친다. 응답 `{entries, unreachable, disabled, truncated}`. since·until은 유닉스 밀리초나 RFC 3339(기본 지난 24시간), user·node·action 거르기(action은 경로도 봄), limit 1~5000(기본 200).

**Files:**
- Create: `crates/opl-peer/src/audit.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/audit.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/tests/audit.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-peer/build.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/lib.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-types/src/audit.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
thiserror.workspace = true
time.workspace = true
tokio.workspace = true
```

을 다음으로 바꾼다:

```toml
thiserror.workspace = true
# 감사 로그 읽기의 RFC 3339 시각
time = { workspace = true, features = ["parsing"] }
tokio.workspace = true
```

`crates/opl-api/src/audit.rs`에서

```rust
//! 항목은 미들웨어 [`layer`]가 바꾸는 요청마다 만든다. 인증 추출기와 핸들러는 요청의
//! [`AuditSlot`]에 주체와 짧은 설명을 남긴다.
```

을 다음으로 바꾼다:

```rust
//! 항목은 미들웨어 [`layer`]가 바꾸는 요청마다 만든다. 인증 추출기와 핸들러는 요청의
//! [`AuditSlot`]에 주체와 짧은 설명을 남긴다. `GET /api/v1/audit`는 모든 노드의 감사 로그를
//! 노드 사이 RPC(`ReadAudit`)로 읽어 합친다.
```

`crates/opl-api/src/audit.rs`에서

```rust

use axum::extract::{FromRequestParts, MatchedPath, Request, State};
```

을 다음으로 바꾼다:

```rust

use async_trait::async_trait;
use axum::extract::{FromRequestParts, MatchedPath, Request, State};
```

`crates/opl-api/src/audit.rs`에서

```rust
use axum::response::Response;
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery, AuditTarget};
use uuid::Uuid;

use crate::auth::Principal;
use crate::limit::ClientIp;
```

을 다음으로 바꾼다:

```rust
use axum::response::Response;
use opl_cluster::types::NodeId;
use opl_peer::AuditSource;
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery, AuditResponse, AuditTarget};
use opl_types::auth::Role;
use opl_types::node::NodeSpec;
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::Principal;
use crate::error::{ApiError, Json, Query};
use crate::limit::ClientIp;
```

`crates/opl-api/src/audit.rs`에서

```rust
const FILE_NAME: &str = "audit.log";
```

을 다음으로 바꾼다:

```rust
const FILE_NAME: &str = "audit.log";
/// 다른 노드의 감사 로그를 읽는 데 기다리는 최대 시간
const PEER_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// 읽기 API의 기본 범위(`until`부터 거슬러)
const DEFAULT_RANGE_MS: u64 = 24 * 60 * 60 * 1000;
const DEFAULT_LIMIT: u32 = 200;
```

`crates/opl-api/src/audit.rs`에서

```rust
        Self { inner: None }
    }
```

을 다음으로 바꾼다:

```rust
        Self { inner: None }
    }

    /// 감사 로그를 적는지(`disabled`가 아닌지)
    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }
```

`crates/opl-api/src/audit.rs`에서

```rust
            tracing::warn!("the audit log did not finish writing within {timeout:?}; giving up");
        }
    }
}

/// 요청 하나의 감사 정보. 미들웨어가 요청 확장에 넣고, 인증 추출기와 핸들러가 채운다.
```

을 다음으로 바꾼다:

```rust
            tracing::warn!("the audit log did not finish writing within {timeout:?}; giving up");
        }
    }
}

/// 다른 노드가 노드 사이 RPC로 이 노드의 감사 로그를 읽는다.
#[async_trait]
impl AuditSource for AuditLog {
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String> {
        AuditLog::read(self, query).await.map_err(|e| e.to_string())
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }
}

/// `GET /audit`의 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AuditParams {
    /// 이 시각부터(포함): 유닉스 밀리초나 RFC 3339. 기본은 `until`의 24시간 전
    pub since: Option<String>,
    /// 이 시각까지(포함): 유닉스 밀리초나 RFC 3339. 기본은 지금
    pub until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    pub user: Option<String>,
    /// 노드 이름(정확히 같은 것)
    pub node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 들어 있는 문자열
    pub action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    pub limit: Option<u32>,
}

/// 모든 노드의 감사 로그를 새 것부터 읽는다. admin만 읽는다. 읽지 못한 노드는 `unreachable`에,
/// 감사 로그를 끈 노드는 `disabled`에, `limit`보다 많으면 잘라 `truncated`다.
#[utoipa::path(get, path = "/audit", tag = "audit",
    params(AuditParams),
    responses((status = 200, body = AuditResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_audit(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(params): Query<AuditParams>,
) -> Result<Json<AuditResponse>, ApiError> {
    if principal.role != Role::Admin {
        return Err(ApiError::forbidden());
    }
    let until = match &params.until {
        Some(s) => parse_time("until", s)?,
        None => now_ms(),
    };
    let since = match &params.since {
        Some(s) => parse_time("since", s)?,
        None => until.saturating_sub(DEFAULT_RANGE_MS),
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=AuditQuery::MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {}",
            AuditQuery::MAX_LIMIT
        )));
    }
    let query = AuditQuery {
        since,
        until,
        user: params.user,
        action: params.action,
        limit,
    };
    // 위트니스는 API가 없어 감사 로그도 없다. Node 리소스가 없는 멤버는 id를 이름으로 쓴다.
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let members = state.cluster.members();
    let reads = members
        .voters
        .iter()
        .chain(&members.learners)
        .filter_map(|(id, addr)| {
            let node = nodes.iter().find(|n| n.id == id.0);
            if node.is_some_and(|n| n.spec.witness) {
                return None;
            }
            let name = node.map_or_else(|| id.0.to_string(), |n| n.name.clone());
            if params.node.as_ref().is_some_and(|want| *want != name) {
                return None;
            }
            Some(read_node(&state, *id, addr, name, query.clone()))
        });
    let mut entries = Vec::new();
    let mut unreachable = Vec::new();
    let mut disabled = Vec::new();
    for (name, read) in futures_util::future::join_all(reads).await {
        match read {
            Ok(Some(read)) => entries.extend(read),
            Ok(None) => disabled.push(name),
            Err(e) => {
                tracing::warn!(node = %name, error = %e, "cannot read the audit log of a node");
                unreachable.push(name);
            }
        }
    }
    unreachable.sort();
    disabled.sort();
    entries.sort_by_key(|e| std::cmp::Reverse((e.time, e.id)));
    let truncated = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    Ok(Json(AuditResponse {
        entries,
        unreachable,
        disabled,
        truncated,
    }))
}

/// 노드 하나의 감사 로그. 이 노드면 파일을 직접, 다른 노드면 노드 사이 RPC로 읽는다.
/// 감사 로그를 끈 노드면 None이다.
async fn read_node(
    state: &AppState,
    id: NodeId,
    addr: &str,
    name: String,
    query: AuditQuery,
) -> (String, Result<Option<Vec<AuditEntry>>, String>) {
    if id.0 == state.node {
        if !state.audit.is_enabled() {
            return (name, Ok(None));
        }
        let read = state.audit.read(query).await.map(Some);
        return (name, read.map_err(|e| e.to_string()));
    }
    let Some(peers) = &state.peers else {
        return (name, Err("no node-to-node connection".into()));
    };
    let read = tokio::time::timeout(
        PEER_READ_TIMEOUT,
        peers.read_audit(addr, query, PEER_READ_TIMEOUT),
    )
    .await;
    let read = match read {
        Ok(Ok(entries)) => Ok(entries),
        Ok(Err(status)) => Err(status.message().to_string()),
        Err(_) => Err("timed out".into()),
    };
    (name, read)
}

/// 유닉스 밀리초나 RFC 3339 시각
fn parse_time(field: &str, value: &str) -> Result<u64, ApiError> {
    if let Ok(ms) = value.parse::<u64>() {
        return Ok(ms);
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|t| u64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok())
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "{field} must be unix milliseconds or an RFC 3339 time, not {value:?}"
            ))
        })
}

/// 요청 하나의 감사 정보. 미들웨어가 요청 확장에 넣고, 인증 추출기와 핸들러가 채운다.
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(watch::watch))
}
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(watch::watch))
        .routes(routes!(audit::list_audit))
}
```

`crates/opl-api/tests/audit.rs`에서

```rust
//! 감사 로그 기록: 바꾸는 요청·로그인·로그아웃·콘솔 열기가 누가·어떻게·무엇을과 함께 적히고,
//! 읽는 요청과 비밀번호는 적히지 않는다.
```

을 다음으로 바꾼다:

```rust
//! 감사 로그 기록: 바꾸는 요청·로그인·로그아웃·콘솔 열기가 누가·어떻게·무엇을과 함께 적히고,
//! 읽는 요청과 비밀번호는 적히지 않는다. 읽기 API `GET /audit`: admin만, 시각·사용자·동작·노드
//! 거르기, limit과 truncated.
```

`crates/opl-api/tests/audit.rs`에서

```rust
    assert_eq!(entry(&env, "PUT /subscription", 400).await.detail, None);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(entry(&env, "PUT /subscription", 400).await.detail, None);
}

/// 시각 `time`에 `user`가 `path`로 한 바꾸는 요청
fn seeded(time: u64, user: &str, path: &str) -> AuditEntry {
    AuditEntry {
        id: uuid::Uuid::now_v7(),
        time,
        node: String::new(),
        user: Some(user.into()),
        user_id: None,
        role: Some(Role::Operator),
        auth: AuditAuth::Token,
        ip: "10.0.0.9".into(),
        method: "POST".into(),
        path: path.into(),
        action: "POST /vms/{key}/actions/{action}".into(),
        target: target("vms", "web-1"),
        status: 202,
        duration_ms: 4,
        detail: None,
    }
}

/// 옛 시각(1~4초)의 항목 넷을 적고 읽힐 때까지 기다린다. 로그인 같은 지금 항목과 섞이지 않는다.
async fn seed(env: &Env) {
    for (time, user, path) in [
        (1_000, "alice", "/vms/web-1/actions/start"),
        (2_000, "bob", "/vms/web-1/actions/stop"),
        (3_000, "alice", "/vms/web-1/actions/stop"),
        (4_000, "alice", "/vms/web-1/actions/reboot"),
    ] {
        env.state.audit.record(seeded(time, user, path));
    }
    entries_until(env, |es| es.iter().filter(|e| e.time <= 4_000).count() == 4).await;
}

/// 응답 항목의 시각
fn times(body: &serde_json::Value) -> Vec<u64> {
    body["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{body}"))
        .iter()
        .map(|e| e["time"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn only_admins_read_the_audit_log() {
    let env = Env::start().await;
    seed(&env).await;
    let admin = env.admin().await;
    let (status, body) = admin.get("/audit?since=0&until=5000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(times(&body), [4_000, 3_000, 2_000, 1_000]);
    assert_eq!(body["entries"][0]["node"], "node1");
    assert_eq!(body["unreachable"], json!([]));
    assert_eq!(body["disabled"], json!([]));
    assert_eq!(body["truncated"], false);
    for role in ["operator", "viewer"] {
        let user = env.user(&format!("{role}1"), role).await;
        let (status, body) = user.get("/audit?since=0&until=5000").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {body}");
        assert_eq!(body["code"], "forbidden");
    }
    let (status, _) = env.anonymous().get("/audit").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_range_takes_milliseconds_or_rfc3339() {
    let env = Env::start().await;
    seed(&env).await;
    let admin = env.admin().await;
    let (status, body) = admin.get("/audit?since=2000&until=3000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(times(&body), [3_000, 2_000]);
    let (status, body) = admin
        .get("/audit?since=1970-01-01T00:00:01.500Z&until=1970-01-01T01:00:03%2B01:00")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(times(&body), [3_000, 2_000]);
    // 기본은 지난 24시간이라 옛 항목은 없고 방금 로그인한 것은 있다.
    let (status, body) = admin.get("/audit").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let recent = times(&body);
    assert!(
        !recent.is_empty() && recent.iter().all(|t| *t > 4_000),
        "{body}"
    );

    for bad in [
        "since=yesterday",
        "until=-5",
        "since=2026-13-01T00:00:00Z",
        "limit=0",
        "limit=5001",
        "limit=many",
        "verbose=true",
    ] {
        let (status, body) = admin.get(&format!("/audit?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(body["code"], "invalid_request", "{bad}");
    }
    let (status, _) = admin.get("/audit?limit=5000").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn filters_and_limit_narrow_the_entries() {
    let env = Env::start().await;
    seed(&env).await;
    let admin = env.admin().await;
    let get = |q: &'static str| {
        let admin = &admin;
        async move {
            let (status, body) = admin.get(&format!("/audit?since=0&until=5000&{q}")).await;
            assert_eq!(status, StatusCode::OK, "{q}: {body}");
            body
        }
    };
    assert_eq!(times(&get("user=alice").await), [4_000, 3_000, 1_000]);
    // 동작은 action이나 경로의 부분 문자열이다(VM 동작은 경로로 가른다).
    assert_eq!(times(&get("action=actions/stop").await), [3_000, 2_000]);
    assert_eq!(times(&get("action=%7Baction%7D&user=bob").await), [2_000]);
    assert_eq!(times(&get("node=node1&user=bob").await), [2_000]);
    let body = get("node=elsewhere").await;
    assert_eq!(times(&body), Vec::<u64>::new());
    assert_eq!(body["unreachable"], json!([]));

    let body = get("limit=2").await;
    assert_eq!(times(&body), [4_000, 3_000]);
    assert_eq!(body["truncated"], true);
    let body = get("limit=4").await;
    assert_eq!(times(&body).len(), 4);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn this_node_with_the_audit_log_off_is_listed_as_disabled() {
    let env = Env::start().await;
    // 같은 노드의 API를 감사 로그 없이(opld의 `[audit] enabled = false`처럼) 하나 더 띄운다.
    let hv: std::sync::Arc<dyn opl_driver::Hypervisor> = env.hv.clone();
    let state = opl_api::AppState::new(
        env.cluster.clone(),
        env.node,
        hv,
        env.dir.path().join("off"),
    );
    assert!(!state.audit.is_enabled());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(std::sync::Arc::new(state));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert!(env.state.audit.is_enabled());

    let admin = common::login_at(&base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin.get("/audit").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["disabled"], json!(["node1"]));
    assert_eq!(body["unreachable"], json!([]));
    assert_eq!(body["entries"], json!([]));
}
```

`crates/opl-api/tests/two_nodes.rs` 전체:

```rust
//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기(HA VM 포함), 공유 디스크의 잠금 바꾸기.
//! 감사 로그: 한 노드의 `GET /audit`가 두 노드의 항목을 합치고, 닿지 않는 노드를 알린다.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{
    AppState, AuditConfig, AuditLog, IncomingConfig, IncomingMigrations, TaskRunner,
    UefiVarsFetcher, hash_password,
};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{
    DriverError, Hypervisor, MigrateTarget, MockHypervisor, RuntimeStatus, Storage, uefi_vars_path,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{FirmwareFetcher, HaConfig, HaGuard, Reconciler};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{Arch, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use common::{
    ADMIN_PASSWORD, FakeStorage, MOUNT_ROOT, WAIT, login_at, reconciler_config, runner_config,
    vm_request,
};

/// 받는 노드가 마이그레이션을 받는 포트 범위. Mock 하이퍼바이저는 첫 포트를 연다.
const MIGRATION_PORTS: std::ops::RangeInclusive<u16> = 61000..=61009;

struct Node {
    id: Uuid,
    /// Node 리소스와 감사 로그의 노드 이름
    name: String,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 볼륨 호출을 적는 저장소(reconciler, Task 실행기, 마이그레이션 받기가 함께 쓴다)
    storage: Arc<FakeStorage>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    /// 감사 로그를 끈 노드(opld의 `[audit] enabled = false`)
    audit_off: bool,
    /// 노드 간 서버를 멈춘다.
    peer_stop: Option<tokio::sync::oneshot::Sender<()>>,
    peer_server: Option<tokio::task::JoinHandle<()>>,
    base: String,
    /// reconciler의 HA guard. None이면 펜싱이 꺼진 노드다.
    ha: Option<Arc<HaGuard>>,
    _workers: Option<(Reconciler, TaskRunner)>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir, name: &str) -> Node {
        let id = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(NodeId(id), root.path().join(id.to_string()));
        // 테스트가 동시에 많이 돌면 첫 선거가 몇 초씩 걸린다.
        config.write_timeout = Duration::from_secs(30);
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        Node {
            id,
            name: name.into(),
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            storage: Arc::new(FakeStorage::default()),
            tls,
            listener: Some(listener),
            audit_off: false,
            peer_stop: None,
            peer_server: None,
            base: String::new(),
            ha: None,
            _workers: None,
        }
    }

    /// 노드 간 서버(마이그레이션 받기 포함), reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = self.storage.clone();
        let incoming = IncomingMigrations::new(
            self.cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node: self.id,
                mount_root: MOUNT_ROOT.into(),
                storage: storage.clone(),
                listen_host: "127.0.0.1".into(),
                ports: MIGRATION_PORTS,
            },
        );
        let work = root.path().join(format!("{}-work", self.id));
        // opld처럼 API와 노드 간 서버가 같은 감사 로그를 쓴다.
        let audit = Arc::new(if self.audit_off {
            AuditLog::disabled()
        } else {
            AuditLog::open(AuditConfig {
                dir: work.join("audit"),
                node: self.name.clone(),
                max_file_size: 1 << 20,
                max_files: 2,
            })
            .unwrap()
        });
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(work.join("state"))
            .with_audit(audit.clone());
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        self.peer_stop = Some(stop);
        self.peer_server = Some(tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        }));
        // UEFI 변수 파일을 가진 노드에서 받아 온다(opld처럼 reconciler와 Task 실행기가 함께 쓴다).
        // 시험의 변수 파일은 모두 템플릿과 같은 6바이트다.
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(template(root, self), b"vars 0").unwrap();
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            self.cluster.clone(),
            self.peers.clone(),
            work.join("state"),
            template(root, self),
        ));
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        reconciler_config.firmware = Some(firmware.clone());
        reconciler_config.ha = self.ha.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.firmware = Some(firmware);
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            hv.clone(),
            reconciler.handle(),
            config,
        )
        .await;
        let mut state = AppState::new(self.cluster.clone(), self.id, hv, work.join("uploads"));
        state.peers = Some(self.peers.clone());
        state.audit = audit;
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(Arc::new(state));
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
        self._workers = Some((reconciler, runner));
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    two_nodes_with(None, true).await
}

/// `ha`가 있으면 A의 reconciler에 넣고 두 노드를 HA 가능 노드(`process`)로 등록한다.
/// `b_audit`가 거짓이면 B는 감사 로그를 끈 노드다.
async fn two_nodes_with(ha: Option<Arc<HaGuard>>, b_audit: bool) -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root, "node1").await;
    let mut b = Node::open(&ca, &root, "node2").await;
    let node_ha = ha.as_ref().map(|_| NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 60,
    });
    a.ha = ha;
    b.audit_off = !b_audit;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for node in [&a, &b] {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            node.name.clone(),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id;
        r.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
            ha: node_ha,
            underlay: None,
            firewall: false,
            version: None,
            updates: None,
        };
        // 리더가 뽑힐 때까지 쓰기가 기다린다.
        a.cluster.create(&r).await.unwrap();
    }
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: Role::Admin,
            session_epoch: 0,
        },
    );
    a.cluster.create(&admin).await.unwrap();
    (root, a, b)
}

#[tokio::test]
async fn console_of_a_vm_on_another_node_is_relayed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    b.hv.set_console_dir(tmp.path());

    // 노드 A의 API로 노드 B에 VM을 만들고 켠다. Task는 B의 실행기가 끝낸다.
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(b.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = b
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert!(a.hv.inspect(vm.id).await.unwrap().is_none());

    let listener =
        tokio::net::UnixListener::bind(tmp.path().join(format!("{}.vnc", vm.id))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });
    let url = format!("{}/api/v1/vms/web/console", a.base).replacen("http", "ws", 1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", admin.token.as_ref().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let greeting = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    let echo = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::binary(b"ping".to_vec()));

    // A의 API로 만든 VM은 B에서도 곧 보인다(같은 저장소).
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let deadline = Instant::now() + WAIT;
    while admin_b.get("/vms/web").await.0 != StatusCode::OK {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 노드의 드라이버 `state_dir`(Task 실행기 설정과 노드 간 서버가 같은 곳을 쓴다)
fn state_dir(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path().join(format!("{}-work", node.id)).join("state")
}

/// 노드의 UEFI 변수 템플릿(받은 변수 파일의 크기를 이와 견준다)
fn template(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path()
        .join(format!("{}-work", node.id))
        .join("vars-template.fd")
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
    let (root, a, b) = two_nodes().await;
    let vm = Uuid::now_v7();
    let src = uefi_vars_path(&state_dir(&root, &b), vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars b").unwrap();
    let fetcher = UefiVarsFetcher::new(
        a.cluster.clone(),
        a.peers.clone(),
        state_dir(&root, &a),
        template(&root, &a),
    );
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 크기가 이 노드의 템플릿과 다르면(다른 펌웨어 빌드) 받은 것을 버린다.
    std::fs::write(&src, b"other firmware").unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert!(err.0.contains("14 bytes"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 파일이 없거나 멤버가 아닌 노드면 실패하고 이 노드의 파일은 그대로다.
    let err = fetcher.fetch(Uuid::now_v7(), b.id).await.unwrap_err();
    assert!(err.0.contains("no UEFI variables"), "{err}");
    let err = fetcher.fetch(vm, Uuid::now_v7()).await.unwrap_err();
    assert!(err.0.contains("not a cluster member"), "{err}");

    // 오프라인으로 기록된 노드에서는 받지 않는다.
    std::fs::write(&src, b"newer").unwrap();
    let mut node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    node.status.online = false;
    a.cluster
        .update_status::<NodeSpec>(b.id, &node.status)
        .await
        .unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert_eq!(err.0, "node \"node2\" is offline");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");
}

/// 노드의 VM `vm` UEFI 변수 파일에 쓴다.
fn write_vars(root: &TempDir, node: &Node, vm: Uuid, data: &[u8]) {
    let path = uefi_vars_path(&state_dir(root, node), vm);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

fn read_vars(root: &TempDir, node: &Node, vm: Uuid) -> Vec<u8> {
    std::fs::read(uefi_vars_path(&state_dir(root, node), vm)).unwrap()
}

/// 멈춘 UEFI VM을 다른 노드로 옮기면, 그 노드의 스냅샷·복제·시작이 앞 노드의 변수를 받아 쓴다.
/// 되돌린 변수는 그 노드의 것이 된다(다시 받지 않는다).
#[tokio::test]
async fn variables_follow_a_stopped_vm_to_another_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    write_vars(&root, &a, vm.id, b"vars 1");

    // 멈춘 채 B로 옮긴다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = serde_json::json!(b.id);
    let (status, body) = admin
        .put("/vms/web", serde_json::json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 스냅샷은 B에서 찍고 A의 변수를 받아 담는다.
    let (status, task) = admin
        .post("/vms/web/snapshots", serde_json::json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    let s1 = b
        .cluster
        .list::<opl_types::snapshot::SnapshotSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|s| s.spec.vm == vm.id)
        .unwrap();
    let copy = state_dir(&root, &b).join(format!("vm/{}/snapshots/{}.vars", vm.id, s1.id));
    assert_eq!(std::fs::read(copy).unwrap(), b"vars 1");

    // 복제도 B에서 A의 변수를 받아 복사하고, 새 VM의 변수는 B에 있다.
    write_vars(&root, &a, vm.id, b"vars 2");
    let (status, task) = admin
        .post("/vms/web/clone", serde_json::json!({ "name": "web-2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = b.cluster.get_by_name::<VmSpec>("web-2").unwrap().unwrap();
    assert_eq!(read_vars(&root, &b, copy.id), b"vars 2");
    assert_eq!(copy.status.firmware_node, Some(b.id));

    // 켜면 B의 reconciler가 A의 변수를 받아 켜고 B를 변수 노드로 남긴다.
    write_vars(&root, &a, vm.id, b"vars 3");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 3");
    wait_vm(&b, vm.id, "variables on B", |v| {
        v.status.firmware_node == Some(b.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // 되돌리기는 스냅샷 때의 변수를 B에 쓰고 B를 변수 노드로 삼는다. 다음 시작은 A에서 받지 않는다.
    let mut moved = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    moved.status.firmware_node = Some(a.id);
    b.cluster
        .update_status::<VmSpec>(vm.id, &moved.status)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reverted = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(reverted.status.firmware_node, Some(b.id));
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 1");
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
async fn running_vm(a: &Node, admin: &common::Client) -> opl_types::vm::Vm {
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap()
}

async fn migrate(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

async fn runtime_status(node: &Node, id: Uuid) -> Option<RuntimeStatus> {
    node.hv.inspect(id).await.unwrap().map(|s| s.status)
}

/// 두 노드에서 VM이 원하는 대로 보일 때까지 기다린다.
async fn wait_vm(node: &Node, id: Uuid, what: &str, pred: impl Fn(&opl_types::vm::Vm) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let vm = node.cluster.get::<VmSpec>(id).unwrap().unwrap();
        if pred(&vm) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {:?}", vm.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 확정 전에 실패한 마이그레이션 뒤: VM은 A에 남아 돌고, B에는 받는 QEMU가 없다.
async fn assert_still_on_a(a: &Node, b: &Node, id: Uuid) {
    let vm = a.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(a.id));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.node, Some(a.id));
    assert_eq!(vm.status.migration, None);
    assert_eq!(runtime_status(a, id).await, Some(RuntimeStatus::Running));
    assert_eq!(runtime_status(b, id).await, None);
    assert!(!b.hv.is_incoming(id));
}

#[tokio::test]
async fn running_vm_migrates_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    // B의 reconciler가 넘겨받으며 `firmware_node`를 쓰기 전에, 확정 뒤 기록이 B를 쓰는지 본다.
    let b_reconciler = b._workers.as_ref().unwrap().0.handle();
    b_reconciler.begin_migration(vm.id).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = migrate(&admin_b, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], a.id.to_string());

    // A의 QEMU는 B의 클러스터 주소의 호스트, B가 연 포트로 보냈다.
    assert_eq!(
        a.hv.migrations(),
        vec![(
            vm.id,
            MigrateTarget {
                host: "127.0.0.1".into(),
                port: *MIGRATION_PORTS.start(),
                tls_hostname: "127.0.0.1".into(),
            }
        )]
    );
    let vm_now = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(vm_now.spec.node, Some(b.id));
    assert_eq!(vm_now.status.node, Some(b.id));
    assert_eq!(vm_now.status.migration, None);
    // QEMU가 변수를 B로 옮겼다.
    assert_eq!(vm_now.status.firmware_node, Some(b.id));
    assert_eq!(runtime_status(&a, vm.id).await, None);
    b_reconciler.end_migration(vm.id);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id) && v.status.pid.is_some()
    })
    .await;
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(b.hv.launches().is_empty(), "{:?}", b.hv.launches());

    // 옮긴 VM은 B에서 다룬다: 끄면 B의 QEMU가 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

#[tokio::test]
async fn failed_send_keeps_the_vm_running_on_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration(
        "Failed to connect to '127.0.0.1:61000': Connection refused".into(),
    ));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("Connection refused"), "{error}");
    assert_still_on_a(&a, &b, vm.id).await;
    // 다른 동작이 다시 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 원본은 다 보냈지만(`postmigrate`) 대상이 받기에 실패했다: 확정하지 않고 원본을 이어서 실행한다.
#[tokio::test]
async fn failed_receive_resumes_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("TLS handshake failed"), "{error}");
    assert_eq!(a.hv.migrations().len(), 1);
    assert_still_on_a(&a, &b, vm.id).await;
}

/// 정해 둔 대로 곧바로 갱신에 성공하거나 실패한다.
struct FakeRenewer(std::sync::Mutex<bool>);

#[async_trait::async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), opl_cluster::ClusterError> {
        if *self.0.lock().unwrap() {
            Ok(())
        } else {
            Err(opl_cluster::ClusterError::Timeout)
        }
    }
}

/// A에 가짜 리스의 HA guard(`process`, 리스는 `fence_after` 뒤에 끝난다)를 두고 HA VM `web`을 켠다.
async fn ha_vm_on_a(
    fence_after: Duration,
) -> (TempDir, Node, Node, Arc<FakeRenewer>, opl_types::vm::Vm) {
    let renewer = Arc::new(FakeRenewer(std::sync::Mutex::new(true)));
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after,
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    );
    let (root, a, b) = two_nodes_with(Some(guard), true).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["ha"] = serde_json::json!(true);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    wait_vm(&a, vm.id, "running", |v| v.status.phase == VmPhase::Running).await;
    (root, a, b, renewer, vm)
}

/// 되돌린 HA VM의 원본은 reconciler가 리스를 확인한 뒤 이어서 실행한다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_resumes_the_source_with_the_lease() {
    let (_root, a, b, _renewer, vm) = ha_vm_on_a(Duration::from_secs(60)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let deadline = Instant::now() + WAIT;
    while runtime_status(&a, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_still_on_a(&a, &b, vm.id).await;
    assert_eq!(a.hv.launches().len(), 1);
}

/// 마이그레이션하는 동안 리스가 끝났으면 되돌려도 원본을 이어서 실행하지 않고 끈다. 리스가 돌아오면
/// 다시 켠다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_without_the_lease_stops_the_source() {
    let (_root, a, b, renewer, vm) = ha_vm_on_a(Duration::from_millis(500)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    a.hv.delay_migrations(Duration::from_millis(1500));
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    *renewer.0.lock().unwrap() = false;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    wait_vm(&a, vm.id, "fenced", |v| {
        v.status.last_error.as_deref() == Some("fenced: lost the cluster majority")
    })
    .await;
    assert_eq!(runtime_status(&a, vm.id).await, None);
    assert!(
        !a.hv.pause_calls(vm.id).contains(&"resume"),
        "{:?}",
        a.hv.pause_calls(vm.id)
    );

    *renewer.0.lock().unwrap() = true;
    wait_vm(&a, vm.id, "running again", |v| {
        v.status.phase == VmPhase::Running && v.status.last_error.is_none()
    })
    .await;
    assert_eq!(a.hv.launches().len(), 2);
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

/// 원본 QEMU가 보내는 중인데 기다리기가 실패했다: 되돌리며 그 마이그레이션을 취소한다. 나중에 끝나
/// 원본을 멈추지 않는다.
#[tokio::test]
async fn unfinished_send_is_cancelled_by_the_rollback() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!a.hv.is_outgoing(vm.id));
    a.hv.complete_outgoing(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_still_on_a(&a, &b, vm.id).await;
}

/// API 검사를 거치지 않고 Task를 만든다(검사와 마이그레이션 기록 사이에 들어온 요청).
async fn put_task(node: &Node, op: TaskOp, target: Uuid, on: Uuid) -> Uuid {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: on,
        },
    );
    task.name = task.id.to_string();
    node.cluster.create(&task).await.unwrap();
    task.id
}

/// Task가 끝날 때까지 기다린다.
async fn finished_task(node: &Node, id: Uuid) -> Task {
    let deadline = Instant::now() + WAIT;
    loop {
        let task = node.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        if task.status.state.is_finished() {
            return task;
        }
        assert!(Instant::now() < deadline, "task {id}: {:?}", task.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// API가 마이그레이션을 받은 뒤 같은 VM의 스냅샷·디스크 크기 Task가 들어왔다: 원본 Task가 기록한 뒤
/// 다시 보고 되돌린다.
#[tokio::test]
async fn task_started_alongside_stops_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    // 실행하는 노드가 없어 끝나지 않는다.
    let elsewhere = Uuid::now_v7();
    let others = [
        (
            TaskOp::SnapshotCreate {
                snapshot: Uuid::now_v7(),
            },
            vm.id,
            "snapshotted",
        ),
        (
            TaskOp::VolumeResize {
                size_bytes: 2 << 20,
            },
            volume,
            "resized",
        ),
    ];
    for (op, target, what) in others {
        let other = put_task(&a, op, target, elsewhere).await;
        let migrate = put_task(&a, TaskOp::VmMigrate { node: b.id }, vm.id, a.id).await;
        let task = finished_task(&a, migrate).await;
        assert_eq!(task.status.state, TaskState::Failed, "{:?}", task.status);
        let error = task.status.error.unwrap();
        assert!(error.contains(what), "{error}");
        assert!(a.hv.migrations().is_empty());
        assert_still_on_a(&a, &b, vm.id).await;
        a.cluster.delete::<TaskSpec>(other, None).await.unwrap();
    }
}

/// 보내는 동안 VM을 껐다(검사와 기록 사이에 들어온 정지): 확정하지 않는다.
#[tokio::test]
async fn vm_stopped_during_the_migration_is_not_committed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    wait_vm(&a, vm.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let current = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.power = PowerState::Stopped;
    a.cluster
        .update_spec(vm.id, None, current.labels, &spec)
        .await
        .unwrap();

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("stopped"), "{error}");
    let got = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(got.spec.node, Some(a.id));
    assert_eq!(runtime_status(&b, vm.id).await, None);
    // A의 reconciler가 원하는 대로 끈다.
    wait_vm(&a, vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped).await;
}

#[tokio::test]
async fn different_qemu_versions_do_not_migrate() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.set_qemu_version("9.0.0");
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("QEMU 9.0.0"), "{error}");
    assert!(a.hv.migrations().is_empty());
    assert_still_on_a(&a, &b, vm.id).await;
}

/// A에서 도는 VM `web`. 디스크는 공유 dir 풀 `nas`의 볼륨 `root`다. 볼륨 id를 함께 돌려준다.
async fn running_vm_with_disk(a: &Node, admin: &common::Client) -> (opl_types::vm::Vm, Uuid) {
    let pool: StoragePool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/nonexistent/opl-nas".into(),
            },
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
    a.cluster.create(&pool).await.unwrap();
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["disks"] = serde_json::json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    (vm, volume.id)
}

/// 노드의 볼륨 `id`에 대한 공유·배타·끄기 호출(차례대로)
fn lock_calls(node: &Node, id: Uuid) -> Vec<&'static str> {
    node.storage
        .calls()
        .into_iter()
        .filter(|(op, v)| *v == id && ["share", "exclusive", "deactivate"].contains(op))
        .map(|(op, _)| op)
        .collect()
}

/// 공유 디스크(공유 LVM이면 LV 잠금): 원본은 보내기 전에 공유로 바꾸고 확정 뒤 끈다. 대상은 공유로
/// 켜서 받고, 넘겨받은 뒤 원본이 끌 때까지(일시 오류) 다시 시도해 배타로 바꾼다.
#[tokio::test]
async fn shared_disks_are_shared_during_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    assert!(lock_calls(&a, volume).is_empty());

    b.storage.hold_exclusives(1);
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(lock_calls(&a, volume), ["share", "deactivate"]);
    let deadline = Instant::now() + WAIT;
    while lock_calls(&b, volume).len() < 3 {
        assert!(
            Instant::now() < deadline,
            "B did not make the disk exclusive: {:?}",
            lock_calls(&b, volume)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(lock_calls(&b, volume), ["share", "exclusive", "exclusive"]);
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    // 한 번 배타로 바꾼 뒤에는 다시 하지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&b, volume).len(), 3);
}

/// 확정 전 실패: 대상은 받으려고 켠 디스크를 끄고, 원본은(보류가 풀린 reconciler가) 다시 배타로
/// 바꾼다.
#[tokio::test]
async fn failed_migration_makes_shared_disks_exclusive_again() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration("Connection refused".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_still_on_a(&a, &b, vm.id).await;
    let deadline = Instant::now() + WAIT;
    while lock_calls(&a, volume).len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", lock_calls(&a, volume));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&a, volume), ["share", "exclusive"]);
    assert_eq!(lock_calls(&b, volume), ["share", "deactivate"]);
}

/// `found`가 참이 될 때까지 `GET /audit?{query}`를 다시 부른다(쓰기 스레드와 복제를 기다린다).
async fn audit_until(
    admin: &common::Client,
    query: &str,
    found: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let (status, body) = admin.get(&format!("/audit?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if found(&body) {
            return body;
        }
        assert!(start.elapsed() < WAIT, "audit: {body:#}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 응답 항목의 노드 이름
fn audit_nodes(body: &serde_json::Value) -> Vec<&str> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["node"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn audit_entries_of_both_nodes_are_merged() {
    let (_root, a, mut b) = two_nodes().await;
    // 로그인(세션과 토큰 만들기)이 각 노드의 감사 로그에 적힌다.
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let both = |body: &serde_json::Value| {
        let nodes = audit_nodes(body);
        nodes.contains(&"node1") && nodes.contains(&"node2")
    };
    let body = audit_until(&admin, "action=/tokens", both).await;
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["disabled"], serde_json::json!([]));
    assert_eq!(body["truncated"], false);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{body:#}");
    // 두 노드의 항목을 합쳐 새 것부터
    let times: Vec<u64> = entries
        .iter()
        .map(|e| e["time"].as_u64().unwrap())
        .collect();
    assert!(times[0] >= times[1], "{body:#}");
    // 노드로 거르면 그 노드만 묻는다. 한 노드가 limit을 넘기면 잘렸다고 알린다.
    let body = audit_until(&admin, "node=node2", |_| true).await;
    assert!(
        !audit_nodes(&body).is_empty() && audit_nodes(&body).iter().all(|n| *n == "node2"),
        "{body:#}"
    );
    let body = audit_until(&admin, "node=node2&limit=1", |_| true).await;
    assert_eq!(audit_nodes(&body), ["node2"]);
    assert_eq!(body["truncated"], true);

    // 위트니스는 API가 없어 묻지 않는다(닿지 않는 노드에도 넣지 않는다).
    let node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    let witness = NodeSpec {
        witness: true,
        ..node.spec.clone()
    };
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &witness)
        .await
        .unwrap();
    let body = audit_until(&admin, "", |body| !audit_nodes(body).contains(&"node2")).await;
    assert_eq!(body["unreachable"], serde_json::json!([]));
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    audit_until(&admin, "", |body| audit_nodes(body).contains(&"node2")).await;

    // B의 노드 간 서버를 멈추면 B는 닿지 않는 노드다. A의 항목은 그대로 온다.
    let _ = b.peer_stop.take().unwrap().send(());
    b.peer_server.take().unwrap().await.unwrap();
    let body = audit_until(&admin, "", |body| {
        body["unreachable"] != serde_json::json!([])
    })
    .await;
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    let nodes = audit_nodes(&body);
    assert!(
        !nodes.is_empty() && nodes.iter().all(|n| *n == "node1"),
        "{body:#}"
    );
}

#[tokio::test]
async fn a_node_with_the_audit_log_off_is_disabled_not_unreachable() {
    let (_root, a, _b) = two_nodes_with(None, false).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let body = audit_until(&admin, "", |body| {
        audit_nodes(body).contains(&"node1") && body["disabled"] != serde_json::json!([])
    })
    .await;
    assert_eq!(body["disabled"], serde_json::json!(["node2"]));
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert!(audit_nodes(&body).iter().all(|n| *n == "node1"), "{body:#}");
}
```

`crates/opl-peer/build.rs`에서

```rust
        ))
        .method(
```

을 다음으로 바꾼다:

```rust
        ))
        .method(json(
            "read_audit",
            "ReadAudit",
            "opl_types::audit::AuditQuery",
            "crate::audit::AuditReply",
        ))
        .method(
```

`crates/opl-peer/src/audit.rs`를 만든다:

```rust
//! 감사 로그 읽기의 노드 사이 RPC: 한 노드의 API가 다른 노드의 감사 로그를 읽는다(`ReadAudit`).
//! 읽는 쪽 처리는 opld가 [`AuditSource`]로 넣는다.

use async_trait::async_trait;
use opl_types::audit::{AuditEntry, AuditQuery};
use serde::{Deserialize, Serialize};

/// 읽은 항목(새 것부터, 최대 `limit + 1`개)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReply {
    pub entries: Vec<AuditEntry>,
}

/// 이 노드의 감사 로그를 읽는 처리
#[async_trait]
pub trait AuditSource: Send + Sync {
    /// 조건에 맞는 항목을 새 것부터 `limit + 1`개까지 돌려준다.
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String>;

    /// 감사 로그가 꺼져 있으면 거짓이다. 읽기 요청은 `failed_precondition`으로 거부한다.
    fn enabled(&self) -> bool {
        true
    }
}
```

`crates/opl-peer/src/client.rs`에서

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기,
//! UEFI 변수 받기.
```

을 다음으로 바꾼다:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기,
//! UEFI 변수 받기, 감사 로그 읽기.
```

`crates/opl-peer/src/client.rs`에서

```rust
use opl_cluster::{ClusterError, Forwarder};
use tokio_stream::wrappers::ReceiverStream;
```

을 다음으로 바꾼다:

```rust
use opl_cluster::{ClusterError, Forwarder};
use opl_types::audit::{AuditEntry, AuditQuery};
use tokio_stream::wrappers::ReceiverStream;
```

`crates/opl-peer/src/client.rs`에서

```rust

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
```

을 다음으로 바꾼다:

```rust

impl Peers {
    /// `addr`의 노드의 감사 로그에서 `query`에 맞는 항목을 읽는다(새 것부터, 최대 `limit + 1`개).
    /// 그 노드가 감사 로그를 껐으면 None이다.
    pub async fn read_audit(
        &self,
        addr: &str,
        query: AuditQuery,
        timeout: Duration,
    ) -> Result<Option<Vec<AuditEntry>>, Status> {
        let mut req = Request::new(query);
        req.set_timeout(timeout);
        match self.client(addr)?.read_audit(req).await {
            Ok(reply) => Ok(Some(reply.into_inner().entries)),
            Err(s) if s.code() == Code::FailedPrecondition => Ok(None),
            Err(s) => Err(s),
        }
    }
}

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
```

`crates/opl-peer/src/lib.rs`에서

```rust
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기, UEFI 변수
//! 파일 보내기다.

mod client;
```

을 다음으로 바꾼다:

```rust
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기, UEFI 변수
//! 파일 보내기, 감사 로그 읽기다.

pub mod audit;
mod client;
```

`crates/opl-peer/src/lib.rs`에서

```rust

pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
```

을 다음으로 바꾼다:

```rust

pub use audit::{AuditReply, AuditSource};
pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
```

`crates/opl-peer/src/server.rs`에서

```rust
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
```

을 다음으로 바꾼다:

```rust
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
use opl_types::audit::AuditQuery;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
```

`crates/opl-peer/src/server.rs`에서

```rust

use crate::firmware::{UefiVarsChunk, UefiVarsRequest};
```

을 다음으로 바꾼다:

```rust

use crate::audit::{AuditReply, AuditSource};
use crate::firmware::{UefiVarsChunk, UefiVarsRequest};
```

`crates/opl-peer/src/server.rs`에서

```rust
    migrations: Option<Arc<dyn MigrationReceiver>>,
    /// 드라이버의 `state_dir`. UEFI 변수 파일을 여기서 찾아 보낸다.
```

을 다음으로 바꾼다:

```rust
    migrations: Option<Arc<dyn MigrationReceiver>>,
    audit: Option<Arc<dyn AuditSource>>,
    /// 드라이버의 `state_dir`. UEFI 변수 파일을 여기서 찾아 보낸다.
```

`crates/opl-peer/src/server.rs`에서

```rust
            migrations: None,
            state_dir: None,
```

을 다음으로 바꾼다:

```rust
            migrations: None,
            audit: None,
            state_dir: None,
```

`crates/opl-peer/src/server.rs`에서

```rust
        self.migrations = Some(receiver);
        self
```

을 다음으로 바꾼다:

```rust
        self.migrations = Some(receiver);
        self
    }

    /// 이 노드의 감사 로그를 `source`로 읽어 준다. 없으면 읽기 요청을 거부한다.
    pub fn with_audit(mut self, source: Arc<dyn AuditSource>) -> Self {
        self.audit = Some(source);
        self
```

`crates/opl-peer/src/server.rs`에서

```rust
            self.migrations()?.abort(req.into_inner()).await,
        ))
    }
```

을 다음으로 바꾼다:

```rust
            self.migrations()?.abort(req.into_inner()).await,
        ))
    }

    async fn read_audit(&self, req: Request<AuditQuery>) -> Result<Response<AuditReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .audit
            .as_ref()
            .ok_or_else(|| Status::unimplemented("this node does not keep an audit log"))?;
        if !source.enabled() {
            return Err(Status::failed_precondition("audit log disabled"));
        }
        let mut query = req.into_inner();
        query.limit = query.limit.min(AuditQuery::MAX_LIMIT);
        let entries = source.read(query).await.map_err(Status::internal)?;
        Ok(Response::new(AuditReply { entries }))
    }
```

`crates/opl-peer/tests/grpc.rs` 전체:

```rust
//! 실제 gRPC + mTLS로 잇는 노드들(한 프로세스 안, 127.0.0.1의 빈 포트).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{
    AuditSource, GrpcNetwork, IncomingError, IncomingMigration, MigrateIncomingRequest,
    MigrationReceiver, NodeMonitor, NodeMonitorConfig, PeerService, Peers, UefiVarsError,
};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

/// 테스트가 동시에 많이 돌아 CPU가 바쁘면 선거가 몇 초씩 걸린다(openraft는 리더 임기 동안 다른
/// 노드에 투표하지 않으므로 리더를 잃으면 보통 선거 제한 시간의 두 배쯤 걸린다).
const WAIT: Duration = Duration::from_secs(30);

fn tls_for(ca: &ClusterCa, id: NodeId) -> NodeTls {
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca.sign_node(&csr, id.0, &["127.0.0.1".into()]).unwrap(),
        key_pem,
    }
}

fn volume(name: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

struct Node {
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 이 노드의 드라이버 `state_dir`(UEFI 변수 파일을 보내는 곳)
    state_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Node {
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        Self::start_with(ca, root, None, None).await
    }

    /// `migrations`가 있으면 마이그레이션을 그것으로 받고, `audit`이 있으면 감사 로그를 그것으로 읽는다.
    async fn start_with(
        ca: &ClusterCa,
        root: &TempDir,
        migrations: Option<Arc<dyn MigrationReceiver>>,
        audit: Option<Arc<dyn AuditSource>>,
    ) -> Node {
        let id = NodeId(Uuid::now_v7());
        let tls = tls_for(ca, id);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
        config.write_timeout = WAIT;
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        let hv = Arc::new(MockHypervisor::default());
        let state_dir = root.path().join(format!("{id}-state"));
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>))
                .with_uefi_vars(&state_dir);
        if let Some(migrations) = migrations {
            service = service.with_migrations(migrations);
        }
        if let Some(audit) = audit {
            service = service.with_audit(audit);
        }
        let (stop, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        Node {
            addr,
            cluster,
            peers,
            hv,
            state_dir,
            stop: Some(stop),
            server: Some(server),
        }
    }

    fn id(&self) -> NodeId {
        self.cluster.id()
    }

    async fn stop(&mut self) {
        // opld처럼 노드 간 서버가 끝난 뒤 Raft를 멈춘다. 먼저 멈추면 서버가 아직 받는 전달 요청에
        // 다시 시도하지 않는 "raft stopped"로 답한다.
        let _ = self.stop.take().unwrap().send(());
        self.server.take().unwrap().await.unwrap();
        self.cluster.shutdown().await.unwrap();
    }
}

/// 받은 요청을 적고 정해 둔 결과를 돌려주는 받는 쪽
#[derive(Default)]
struct FakeReceiver {
    calls: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl MigrationReceiver for FakeReceiver {
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError> {
        self.calls.lock().unwrap().push(format!(
            "incoming {} {} {}",
            req.vm, req.task, req.qemu_version
        ));
        if req.qemu_version != "8.2.2" {
            return Err(IncomingError::Incompatible(format!(
                "QEMU {} differs from 8.2.2",
                req.qemu_version
            )));
        }
        Ok(60007)
    }

    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("wait {} {}", req.vm, req.task));
        Err(IncomingError::Failed("the receiving QEMU exited".into()))
    }

    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("abort {} {}", req.vm, req.task));
        Ok(())
    }
}

/// 받은 조건을 적고 항목 하나를 돌려주는 감사 로그
#[derive(Default)]
struct FakeAudit {
    queries: std::sync::Mutex<Vec<AuditQuery>>,
}

#[async_trait::async_trait]
impl AuditSource for FakeAudit {
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String> {
        if query.user.as_deref() == Some("broken") {
            return Err("cannot read the audit log".into());
        }
        self.queries.lock().unwrap().push(query);
        Ok(vec![AuditEntry {
            id: Uuid::now_v7(),
            time: 1_000,
            node: "n2".into(),
            user: Some("alice".into()),
            user_id: None,
            role: None,
            auth: AuditAuth::Session,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms".into(),
            action: "POST /vms".into(),
            target: None,
            status: 201,
            duration_ms: 2,
            detail: None,
        }])
    }
}

/// 노드 `n`개를 띄우고 첫 노드에서 클러스터를 초기화한 뒤 모두가 같은 리더를 알 때까지 기다린다.
async fn start(n: usize) -> (ClusterCa, TempDir, Vec<Node>) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut nodes = Vec::new();
    for _ in 0..n {
        nodes.push(Node::start(&ca, &root).await);
    }
    let members: BTreeMap<_, _> = nodes
        .iter()
        .map(|n| (n.id(), BasicNode::new(&n.addr)))
        .collect();
    nodes[0].cluster.initialize(members).await.unwrap();
    wait_leader(&nodes).await;
    (ca, root, nodes)
}

async fn wait_leader(nodes: &[Node]) -> NodeId {
    let deadline = Instant::now() + WAIT;
    loop {
        let leaders: Vec<_> = nodes
            .iter()
            .map(|n| n.cluster.raft().metrics().borrow().current_leader)
            .collect();
        if let Some(Some(leader)) = leaders.first()
            && leaders.iter().all(|l| *l == Some(*leader))
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().metrics().borrow().state == ServerState::Leader
        {
            return *leader;
        }
        assert!(Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn nodes_replicate_and_forward_over_mtls() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();

    let v = volume("disk-a");
    nodes[follower].cluster.create(&v).await.unwrap();
    assert!(
        nodes[follower]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );

    // 리더가 죽어도 남은 두 노드로 쓰기가 이어진다.
    let leader = nodes.iter().position(|n| n.id() == leader).unwrap();
    nodes[leader].stop().await;
    let rest: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
    let v = volume("disk-b");
    nodes[rest[0]].cluster.create(&v).await.unwrap();
    assert!(
        nodes[rest[0]]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn strangers_and_non_members_are_refused() {
    let (ca, root, nodes) = start(1).await;
    // 다른 CA의 인증서로는 TLS 연결이 맺어지지 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let stranger = Peers::new(&tls_for(&other, NodeId(Uuid::now_v7())));
    assert!(stranger.ping(&nodes[0].addr, WAIT).await.is_err());
    // 같은 CA라도 멤버가 아니면 거부한다(제거된 노드).
    let outsider = Node::start(&ca, &root).await;
    let err = outsider.peers.ping(&nodes[0].addr, WAIT).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err = outsider
        .peers
        .leader_request(&nodes[0].addr, LeaderRequest::Remove { id: nodes[0].id() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");
    // 멤버는 된다.
    assert_eq!(
        nodes[0].peers.ping(&nodes[0].addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
}

#[tokio::test]
async fn a_new_node_joins_as_learner_then_voter() {
    let (ca, root, mut nodes) = start(1).await;
    let rev = nodes[0].cluster.create(&volume("before")).await.unwrap();

    let new = Node::start(&ca, &root).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: new.id(),
            addr: new.addr.clone(),
        })
        .await
        .unwrap();
    new.cluster
        .raft()
        .wait(Some(WAIT))
        .applied_index_at_least(Some(rev), "learner caught up")
        .await
        .unwrap();
    // 새 노드가 스스로 승격을 요청한다(리더로 전달된다).
    new.cluster
        .request(LeaderRequest::AddVoter { id: new.id() })
        .await
        .unwrap();
    let members = new.cluster.members();
    assert_eq!(members.voters.len(), 2);
    assert!(members.voters.contains_key(&new.id()));
    assert!(
        new.cluster
            .get_by_name::<VolumeSpec>("before")
            .unwrap()
            .is_some()
    );
    nodes.push(new);
    // 새 노드에서 한 쓰기도 커밋된다(이제 과반수는 2).
    nodes[1].cluster.create(&volume("after")).await.unwrap();
}

#[tokio::test]
async fn console_is_relayed_to_the_node_running_the_vm() {
    let (_ca, _root, nodes) = start(2).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let target = &nodes[1];
    target.hv.set_console_dir(tmp.path());
    let vm = Uuid::now_v7();
    let spec: VmRuntimeSpec = serde_json::from_value(serde_json::json!({
        "id": vm, "name": "web", "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256,
        "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
    }))
    .unwrap();
    target.hv.start(&spec, None).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    let mut console = nodes[0].peers.console(&target.addr, vm).await.unwrap();
    assert_eq!(&console.recv().await.unwrap()[..], b"RFB 003.008\n");
    assert!(console.send(bytes::Bytes::from_static(b"ping")).await);
    assert_eq!(&console.recv().await.unwrap()[..], b"ping");

    // 그 노드에서 돌지 않는 VM은 거부한다.
    let err = nodes[0]
        .peers
        .console(&nodes[0].addr, vm)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, opl_peer::ConsoleError::NotRunning), "{err}");
}

#[tokio::test]
async fn leader_records_when_nodes_go_offline_and_come_back() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
        r.status.online = node.id() == leader;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let online = |id: NodeId| {
        leader_cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };
    let wait_for = |want: bool, id: NodeId| {
        let online = &online;
        async move {
            let deadline = Instant::now() + WAIT;
            while online(id) != want {
                assert!(
                    Instant::now() < deadline,
                    "node {id} never became online={want}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    wait_for(true, follower_id).await;
    nodes[follower].stop().await;
    wait_for(false, follower_id).await;
    assert!(online(leader));
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn new_leader_does_not_revive_nodes_recorded_offline() {
    let (_ca, _root, mut nodes) = start(5).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        r.status.online = true;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let online = |cluster: &Cluster, id: NodeId| {
        cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };

    // 팔로워 하나를 멈추고 리더가 오프라인으로 기록할 때까지 기다린다.
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let mut dead = nodes.remove(follower);
    let dead_id = dead.id();
    dead.stop().await;
    let old = nodes.iter().position(|n| n.id() == leader).unwrap();
    let deadline = Instant::now() + WAIT;
    while online(&nodes[old].cluster, dead_id) {
        assert!(
            Instant::now() < deadline,
            "dead follower never went offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 리더도 멈추면 새 리더가 그 기록을 다시 온라인으로 바꾸지 않아야 한다.
    let mut old = nodes.remove(old);
    old.stop().await;
    let new_leader = wait_leader(&nodes).await;
    let new_leader = nodes.iter().find(|n| n.id() == new_leader).unwrap();
    // 새 리더가 이전 커밋을 모두 적용한 뒤부터 본다. openraft는 리더 확인 RPC를 하트비트 간격
    // (50ms)만 기다리고, 노드 둘이 죽어 남는 여유가 없어 CPU가 바쁘면 한 번에 실패할 수 있다.
    // 운영 코드(노드 감시, 예약 작업)처럼 다시 한다.
    let deadline = Instant::now() + WAIT;
    while let Err(e) = new_leader.cluster.read_barrier().await {
        assert!(
            Instant::now() < deadline,
            "read barrier never succeeded: {e}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 감시 간격보다 길고 `offline_after`보다 짧게 지켜본다.
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            !online(&new_leader.cluster, dead_id),
            "new leader revived a node recorded offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn migration_requests_reach_the_receiver_of_a_member() {
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(FakeReceiver::default());
    let target = Node::start_with(&ca, &root, Some(receiver.clone()), None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    // 대상 노드가 멤버십을 받아 원본 노드를 멤버로 알 때까지 기다린다.
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (vm, task) = (Uuid::now_v7(), Uuid::now_v7());
    let peers = &nodes[0].peers;
    let req = |version: &str| MigrateIncomingRequest {
        vm,
        task,
        qemu_version: version.into(),
    };
    assert_eq!(
        peers
            .migrate_incoming(&target.addr, req("8.2.2"))
            .await
            .unwrap(),
        60007
    );
    // 받는 쪽의 거절과 실패는 그 종류 그대로 온다.
    let err = peers
        .migrate_incoming(&target.addr, req("9.0.0"))
        .await
        .unwrap_err();
    assert!(matches!(err, IncomingError::Incompatible(_)), "{err:?}");
    let migration = IncomingMigration { vm, task };
    let err = peers
        .migrate_incoming_wait(&target.addr, migration)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        IncomingError::Failed("the receiving QEMU exited".into())
    );
    peers
        .migrate_incoming_abort(&target.addr, migration)
        .await
        .unwrap();
    assert_eq!(
        *receiver.calls.lock().unwrap(),
        [
            format!("incoming {vm} {task} 8.2.2"),
            format!("incoming {vm} {task} 9.0.0"),
            format!("wait {vm} {task}"),
            format!("abort {vm} {task}"),
        ]
    );

    // 받는 처리가 없는 노드는 거부한다.
    let err = target
        .peers
        .migrate_incoming_abort(&nodes[0].addr, migration)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("does not receive migrations")),
        "{err:?}"
    );
    // 멤버가 아닌 노드의 요청은 받는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .migrate_incoming(&target.addr, req("8.2.2"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert_eq!(receiver.calls.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_a_member() {
    let (ca, root, nodes) = start(2).await;
    let (from, to) = (&nodes[1], &nodes[0]);
    let vm = Uuid::now_v7();
    // 데이터 조각, 0만 있는 조각 둘, 끝이 짧은 조각(마지막 바이트만 0이 아님)
    let chunk = opl_peer::firmware::CHUNK_BYTES;
    let mut vars: Vec<u8> = (0..chunk).map(|i| (i % 251) as u8 + 1).collect();
    vars.resize(3 * chunk + 100, 0);
    *vars.last_mut().unwrap() = 0xaa;
    let src = uefi_vars_path(&from.state_dir, vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, &vars).unwrap();

    // 받는 노드의 옛 파일을 바꾼다.
    let dest = uefi_vars_path(&to.state_dir, vm);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"stale").unwrap();
    let len = to
        .peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(len, vars.len() as u64);
    assert!(std::fs::read(&dest).unwrap() == vars, "content differs");

    // 0만 있는 파일, 빈 파일도 크기 그대로 온다.
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(
        to.peers
            .uefi_vars(&from.addr, vm, &dest, None)
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::read(&dest).unwrap().is_empty());

    // 그 노드에 파일이 없으면 NotFound이고 받는 쪽 파일은 그대로다.
    std::fs::write(&dest, b"kept").unwrap();
    let err = to
        .peers
        .uefi_vars(&from.addr, Uuid::now_v7(), &dest, None)
        .await
        .unwrap_err();
    assert_eq!(err, UefiVarsError::NotFound);
    assert_eq!(std::fs::read(&dest).unwrap(), b"kept");

    // 멤버가 아닌 노드에는 보내지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .uefi_vars(&from.addr, vm, &root.path().join("outsider.fd"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, UefiVarsError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert!(!root.path().join("outsider.fd").exists());
}

#[tokio::test]
async fn audit_logs_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let audit = Arc::new(FakeAudit::default());
    let target = Node::start_with(&ca, &root, None, Some(audit.clone())).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |limit, user: Option<&str>| AuditQuery {
        since: 10,
        until: 20_000,
        user: user.map(Into::into),
        action: Some("/vms".into()),
        limit,
    };
    let peers = &nodes[0].peers;
    let entries = peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "POST /vms");
    // 너무 큰 limit은 받는 쪽이 줄인다.
    peers
        .read_audit(&target.addr, query(1_000_000, None), WAIT)
        .await
        .unwrap();
    assert_eq!(
        *audit.queries.lock().unwrap(),
        [query(7, None), query(AuditQuery::MAX_LIMIT, None)]
    );
    // 읽기 실패는 오류로 온다.
    let err = peers
        .read_audit(&target.addr, query(7, Some("broken")), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Internal, "{err:?}");

    // 감사 로그가 없는 노드는 거부한다.
    let err = target
        .peers
        .read_audit(&nodes[0].addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented, "{err:?}");
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(audit.queries.lock().unwrap().len(), 2);
}
```

`crates/opl-types/src/audit.rs`에서

```rust
    pub user: Option<String>,
    /// `action`에 이 문자열이 들어 있는 것
    pub action: Option<String>,
```

을 다음으로 바꾼다:

```rust
    pub user: Option<String>,
    /// `action`이나 `path`에 이 문자열이 들어 있는 것(VM 동작은 템플릿이 같아 경로로 가른다)
    pub action: Option<String>,
```

`crates/opl-types/src/audit.rs`에서

```rust
impl AuditQuery {
    pub fn matches(&self, entry: &AuditEntry) -> bool {
```

을 다음으로 바꾼다:

```rust
impl AuditQuery {
    /// `limit`의 최댓값
    pub const MAX_LIMIT: u32 = 5000;

    pub fn matches(&self, entry: &AuditEntry) -> bool {
```

`crates/opl-types/src/audit.rs`에서

```rust
                .as_deref()
                .is_none_or(|a| entry.action.contains(a))
    }
}
```

을 다음으로 바꾼다:

```rust
                .as_deref()
                .is_none_or(|a| entry.action.contains(a) || entry.path.contains(a))
    }
}

/// `GET /audit`의 응답: 모든 노드의 항목을 합친 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditResponse {
    /// 시간 내림차순
    pub entries: Vec<AuditEntry>,
    /// 읽지 못한 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 감사 로그를 끈 노드의 이름(정렬)
    pub disabled: Vec<String>,
    /// 조건에 맞는 항목이 `limit`보다 많아 잘랐다.
    pub truncated: bool,
}
```

`crates/opl-types/src/audit.rs`에서

```rust
        assert!(q.matches(&entry(150, None, "POST /vms/{key}/actions/start")));
        assert!(!q.matches(&entry(150, None, "POST /auth/login")));
    }
```

을 다음으로 바꾼다:

```rust
        assert!(q.matches(&entry(150, None, "POST /vms/{key}/actions/start")));
        let login = AuditEntry {
            path: "/auth/login".into(),
            ..entry(150, None, "POST /auth/login")
        };
        assert!(!q.matches(&login));
    }

    #[test]
    fn action_also_matches_a_substring_of_the_path() {
        // VM 동작은 action 템플릿이 같아 경로(`/vms/web-1/actions/start`)로 가른다.
        let q = AuditQuery {
            action: Some("actions/start".into()),
            ..query()
        };
        assert!(q.matches(&entry(150, None, "POST /vms/{key}/actions/{action}")));
        let q = AuditQuery {
            action: Some("actions/stop".into()),
            ..query()
        };
        assert!(!q.matches(&entry(150, None, "POST /vms/{key}/actions/{action}")));
    }
```

`crates/opld/src/member.rs`에서

```rust
        );
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone());
        let peer = serve_peer(peer_listener, &tls, service, exited_tx.clone());
```

을 다음으로 바꾼다:

```rust
        );
        // API가 적고, 다른 노드가 노드 간 서버로 읽는다.
        let audit = match config.audit.audit_config(&config.data_dir, &config.name) {
            Some(audit) => {
                let dir = audit.dir.clone();
                match AuditLog::open(audit) {
                    Ok(audit) => Arc::new(audit),
                    Err(e) => {
                        let _ = cluster.shutdown().await;
                        return Err(anyhow::Error::new(e)
                            .context(format!("cannot open the audit log in {}", dir.display())));
                    }
                }
            }
            None => Arc::new(AuditLog::disabled()),
        };
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone())
            .with_audit(audit.clone());
        let peer = serve_peer(peer_listener, &tls, service, exited_tx.clone());
```

`crates/opld/src/member.rs`에서

```rust
            state.secure_cookies = true;
            if let Some(audit) = config.audit.audit_config(&config.data_dir, &config.name) {
                let dir = audit.dir.clone();
                state.audit =
                    Arc::new(AuditLog::open(audit).with_context(|| {
                        format!("cannot open the audit log in {}", dir.display())
                    })?);
            }
            let created = match password {
```

을 다음으로 바꾼다:

```rust
            state.secure_cookies = true;
            state.audit = audit;
            let created = match password {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1287개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(audit): 모아 읽기 API와 노드 사이 RPC — GET /api/v1/audit(admin만, since·until은 유닉스 밀리초나 RFC 3339, 기본 지난 24시간, user·node·action 거르기, limit 1~5000 기본 200)가 위트니스가 아닌 모든 멤버의 감사 로그를 읽어 시간 내림차순으로 합치고, 읽지 못한 노드는 unreachable(정렬), limit을 넘으면 truncated. 이 노드는 파일을 직접, 다른 노드는 opl-peer ReadAudit(AuditSource·PeerService::with_audit·Peers::read_audit, 받는 쪽이 limit을 5000으로 자름, 없으면 unimplemented)을 5초 제한으로 동시에. AuditLog가 AuditSource를 구현하고 opld는 감사 로그를 노드 간 서버보다 먼저 열어 함께 넘김. AuditQuery의 action 거르기는 경로의 부분 문자열도 봄(VM 동작은 템플릿이 같음). AuditResponse와 schema.d.ts"
```

---

### Task 4: oplctl audit와 opld E2E

`oplctl audit [--since 30m|1h|7d|시각] [--until] [--user] [--node] [--action] [--limit]`: 표(TIME·NODE·USER·IP·ACTION·TARGET·STATUS), 읽지 못한 노드·꺼진 노드·잘림 안내는 stderr. opld E2E(노드 셋, 한 노드는 감사 꺼짐): 한 노드의 `oplctl audit`가 다른 노드의 기록까지 보이고, 파일은 0600.

**Files:**
- Create: `crates/opld/tests/audit.rs`
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
        cmd: TelemetryCmd,
    },
}
```

을 다음으로 바꾼다:

```rust
        cmd: TelemetryCmd,
    },
    /// 감사 로그(admin만): 모든 노드의 바꾸는 요청·로그인·콘솔 열기를 새 것부터
    Audit(AuditArgs),
}
```

`crates/oplctl/src/cli.rs`에서

```rust
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

을 다음으로 바꾼다:

```rust
}

#[derive(Args)]
struct AuditArgs {
    /// 이 시각부터: `30m`·`1h`·`7d`(지금에서 뺌)나 RFC 3339. 기본은 `--until`의 24시간 전
    #[arg(long)]
    since: Option<String>,
    /// 이 시각까지(RFC 3339). 기본은 지금
    #[arg(long)]
    until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    #[arg(long)]
    user: Option<String>,
    /// 노드 이름
    #[arg(long)]
    node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 든 문자열
    #[arg(long)]
    action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    #[arg(long)]
    limit: Option<u32>,
}

/// `--since`: 기간(`30m`·`1h`·`7d`, 초 `s`도)이면 지금(`now_ms`)에서 뺀 유닉스 밀리초, 아니면
/// 그대로(RFC 3339나 밀리초) 보내 서버가 푼다.
fn audit_since(s: &str, now_ms: u64) -> String {
    match audit_duration_ms(s) {
        Some(ms) => now_ms.saturating_sub(ms).to_string(),
        None => s.to_string(),
    }
}

/// `30m`·`1h`·`7d`(초 `s`도) 같은 기간의 밀리초. 기간이 아니면 None
fn audit_duration_ms(s: &str) -> Option<u64> {
    let unit = match s.chars().last()? {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let n = s[..s.len() - 1].parse::<u64>().ok()?;
    Some(n.saturating_mul(unit))
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn unix_ms_time(ms: u64) -> String {
    match time::OffsetDateTime::from_unix_timestamp((ms / 1000) as i64) {
        Ok(t) => format!(
            "{}T{:02}:{:02}:{:02}Z",
            t.date(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => ms.to_string(),
    }
}

/// `audit`의 표 한 줄
fn audit_row(e: &Value) -> Vec<String> {
    // 대상이 없으면 detail(예: `audit.dropped`의 수, 로그인의 `username=…`)
    let target = match (e["target"]["kind"].as_str(), e["target"]["key"].as_str()) {
        (Some(kind), Some(key)) => format!("{kind}/{key}"),
        _ => e["detail"].as_str().unwrap_or("-").to_string(),
    };
    vec![
        e["time"].as_u64().map_or_else(|| "-".into(), unix_ms_time),
        cell(&e["node"]),
        cell(&e["user"]),
        cell(&e["ip"]),
        cell(&e["action"]),
        target,
        cell(&e["status"]),
    ]
}

/// `GET /audit`를 부르고 표로 보인다. 읽지 못한 노드, 감사 로그를 끈 노드와 잘림은 표 뒤
/// `err`(stderr)에 알린다.
async fn audit(
    client: &Client,
    args: AuditArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    if args.until.is_some()
        && args
            .since
            .as_deref()
            .is_some_and(|s| audit_duration_ms(s).is_some())
    {
        bail!("a relative --since cannot be combined with --until; give a time");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let query: Vec<(&str, String)> = [
        ("since", args.since.map(|s| audit_since(&s, now_ms))),
        ("until", args.until),
        ("user", args.user),
        ("node", args.node),
        ("action", args.action),
        ("limit", args.limit.map(|n| n.to_string())),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();
    let mut r = client.request(Method::GET, "/audit");
    if !query.is_empty() {
        r = r.query(&query);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = resp["entries"]
        .as_array()
        .map(|a| a.iter().map(audit_row).collect())
        .unwrap_or_default();
    table(
        out,
        &["TIME", "NODE", "USER", "IP", "ACTION", "TARGET", "STATUS"],
        &rows,
    )?;
    let unreachable: Vec<&str> = resp["unreachable"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !unreachable.is_empty() {
        writeln!(
            err,
            "warning: no audit records from: {}",
            unreachable.join(", ")
        )?;
    }
    let disabled: Vec<&str> = resp["disabled"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !disabled.is_empty() {
        writeln!(
            err,
            "note: the audit log is disabled on: {}",
            disabled.join(", ")
        )?;
    }
    if resp["truncated"] == true {
        writeln!(
            err,
            "note: more entries match; narrow the range or raise --limit"
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

`crates/oplctl/src/cli.rs`에서

```rust
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
    }
```

을 다음으로 바꾼다:

```rust
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
    }

    #[test]
    fn cli_definition_is_valid() {
```

을 다음으로 바꾼다:

```rust
    }

    /// `--since`의 기간(`30m`·`1h`·`7d`)은 지금에서 뺀 유닉스 밀리초로, 나머지는 그대로 보낸다.
    #[test]
    fn audit_since_turns_durations_into_milliseconds() {
        let now = 1_762_400_000_000;
        assert_eq!(audit_since("30m", now), (now - 30 * 60_000).to_string());
        assert_eq!(audit_since("1h", now), (now - 3_600_000).to_string());
        assert_eq!(audit_since("7d", now), (now - 7 * 86_400_000).to_string());
        assert_eq!(audit_since("90s", now), (now - 90_000).to_string());
        assert_eq!(
            audit_since("2026-11-06T00:00:00Z", now),
            "2026-11-06T00:00:00Z"
        );
        assert_eq!(audit_since("1762400000000", now), "1762400000000");
        assert_eq!(audit_since("xd", now), "xd");
    }

    /// `audit`는 거르기를 쿼리로 보내고 표를 보인다. 읽지 못한 노드와 잘림은 stderr에 알린다.
    /// `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
    #[tokio::test]
    async fn audit_shows_a_table_and_warnings() {
        let (client, seen) = fake_api(|_| {
            json!({
                "entries": [
                    { "id": "019a0000-0000-7000-8000-000000000002", "time": 1_762_387_205_123u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "token", "ip": "10.0.0.5", "method": "POST",
                      "path": "/vms/web-1/actions/start", "action": "POST /vms/{key}/actions/{action}",
                      "target": { "kind": "vms", "key": "web-1" }, "status": 202,
                      "duration_ms": 12, "detail": null },
                    { "id": "019a0000-0000-7000-8000-000000000001", "time": 1_762_387_200_000u64,
                      "node": "node-b", "user": null, "user_id": null, "role": null,
                      "auth": "none", "ip": "10.0.0.9", "method": "POST", "path": "/auth/login",
                      "action": "POST /auth/login", "target": null, "status": 401,
                      "duration_ms": 3, "detail": "username=bob" },
                    { "id": "019a0000-0000-7000-8000-000000000000", "time": 1_762_387_100_000u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "session", "ip": "10.0.0.5", "method": "POST", "path": "/auth/logout",
                      "action": "POST /auth/logout", "target": null, "status": 204,
                      "duration_ms": 1, "detail": null }
                ],
                "unreachable": ["node-c", "node-d"],
                "disabled": ["node-e"],
                "truncated": true
            })
        })
        .await;
        let args = [
            "oplctl",
            "audit",
            "--since",
            "2026-11-06T00:00:00Z",
            "--until",
            "1762400000000",
            "--user",
            "alice",
            "--node",
            "node-a",
            "--action",
            "start",
            "--limit",
            "5",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, false, &mut out, &mut err).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/audit?since=2026-11-06T00%3A00%3A00Z&until=1762400000000\
              &user=alice&node=node-a&action=start&limit=5 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "TIME                  NODE    USER   IP        ACTION                            TARGET        STATUS\n\
             2025-11-06T00:00:05Z  node-a  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202\n\
             2025-11-06T00:00:00Z  node-b  -      10.0.0.9  POST /auth/login                  username=bob  401\n\
             2025-11-05T23:58:20Z  node-a  alice  10.0.0.5  POST /auth/logout                 -             204\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no audit records from: node-c, node-d\n\
             note: the audit log is disabled on: node-e\n\
             note: more entries match; narrow the range or raise --limit\n"
        );

        // 거르기가 없으면 쿼리도 없다(서버 기본값: 지난 24시간, 200개).
        let Cmd::Audit(a) = Cli::try_parse_from(["oplctl", "audit"]).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, true, &mut out, &mut err).await.unwrap();
        assert_eq!(seen.lock().unwrap()[1], "GET /api/v1/audit HTTP/1.1");
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["unreachable"], json!(["node-c", "node-d"]));
        assert_eq!(shown["entries"][1]["detail"], "username=bob");
        assert!(err.is_empty());

        // 기간 --since는 --until과 함께 줄 수 없다(서버에 보내지 않는다).
        let args = [
            "oplctl",
            "audit",
            "--since",
            "1h",
            "--until",
            "1762400000000",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let e = audit(&client, a, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "a relative --since cannot be combined with --until; give a time"
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn cli_definition_is_valid() {
```

`crates/opld/tests/audit.rs`를 만든다:

```rust
//! 계획 34: 노드 셋으로 만든 클러스터에서 각 노드에 바꾸는 요청을 보내면, 한 노드의
//! `oplctl -o json audit`가 두 노드의 기록을 함께 보인다. 감사 파일은 `{data_dir}/audit/audit.log`(0600)이고,
//! `[audit] enabled = false`인 노드는 파일을 만들지 않는다. QEMU는 쓰지 않는다.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::{Daemon, eventually};
use serde_json::{Value, json};

/// `entries`에 노드 `node`의 성공한 `action` 기록(admin, 토큰)이 있는지. `target`은 대상 JSON
fn has(entries: &Value, node: &str, action: &str, target: Value) -> bool {
    entries.as_array().unwrap().iter().any(|e| {
        e["node"] == node
            && e["action"] == action
            && e["target"] == target
            && e["user"] == "admin"
            && e["auth"] == "token"
            && e["status"].as_u64().is_some_and(|s| s < 300)
    })
}

#[tokio::test]
async fn audit_reads_the_records_of_every_node() {
    let mut a = Daemon::new("node-a", "");
    let mut b = Daemon::new("node-b", "");
    let mut c = Daemon::new("node-c", "[audit]\nenabled = false");
    for d in [&mut a, &mut b, &mut c] {
        d.start().await;
    }
    a.init("prod").await;
    let password = a.admin_password.clone().unwrap();
    let fp = a.ca_fingerprint.clone().unwrap();
    a.login().await;
    for d in [&mut b, &mut c] {
        let url = a.ctl(&["cluster", "join-token"]).await.unwrap();
        d.join(url.trim()).await.unwrap();
        d.login_with(&password, &fp).await;
    }

    // 노드마다 다른 바꾸는 요청(oplctl은 로그인한 노드의 API로 보낸다)
    let pw = a.path("alice-password");
    std::fs::write(&pw, "alice-password-1").unwrap();
    let pw = pw.to_str().unwrap();
    a.ctl(&[
        "user",
        "create",
        "alice",
        "--role",
        "viewer",
        "--password-file",
        pw,
    ])
    .await
    .unwrap();
    // node-b가 사용자 생성을 적용한 뒤 지운다.
    eventually("node-b deletes alice", || async {
        b.ctl(&["user", "delete", "alice"]).await.is_ok()
    })
    .await;
    c.ctl(&["token", "create", "ci"]).await.unwrap();

    // 쓰기 스레드가 파일에 적은 뒤 읽힌다.
    eventually("records of node-a and node-b", || async {
        let shown = b.ctl_json(&["audit"]).await;
        has(&shown["entries"], "node-a", "POST /users", Value::Null)
            && has(
                &shown["entries"],
                "node-b",
                "DELETE /users/{key}",
                json!({ "kind": "users", "key": "alice" }),
            )
    })
    .await;
    let shown = b.ctl_json(&["audit", "--since", "1h"]).await;
    // 감사가 꺼진 노드는 읽지 못한 노드가 아니라 꺼진 노드다.
    assert_eq!(shown["unreachable"], json!([]), "{shown}");
    assert_eq!(shown["disabled"], json!(["node-c"]), "{shown}");
    assert_eq!(shown["truncated"], false, "{shown}");
    let entries = shown["entries"].as_array().unwrap();
    assert!(entries.iter().all(|e| e["node"] != "node-c"), "{shown}");
    // 새 것부터
    let times: Vec<u64> = entries
        .iter()
        .map(|e| e["time"].as_u64().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "{times:?}");
    // 노드로 거르기
    let only_a = b.ctl_json(&["audit", "--node", "node-a"]).await;
    let only_a = only_a["entries"].as_array().unwrap();
    assert!(!only_a.is_empty() && only_a.iter().all(|e| e["node"] == "node-a"));
    // 표도 두 노드를 보인다.
    let table = a.ctl(&["audit", "--user", "admin"]).await.unwrap();
    assert!(table.starts_with("TIME"), "{table}");
    assert!(
        table.contains("node-a") && table.contains("node-b"),
        "{table}"
    );

    for d in [&a, &b] {
        let file = d.data_dir().join("audit").join("audit.log");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", file.display());
    }
    assert!(!c.data_dir().join("audit").exists());
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1290개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): oplctl audit와 opld E2E — oplctl audit [--since 30m|1h|7d|RFC 3339] [--until] [--user] [--node] [--action] [--limit]가 GET /api/v1/audit를 부름(기간은 클라이언트가 지금에서 뺀 유닉스 밀리초로, 그 밖은 그대로 보내 서버가 풂, 준 거르기만 쿼리에), 표 TIME(UTC YYYY-MM-DDTHH:MM:SSZ)·NODE·USER·IP·ACTION·TARGET(kind/key)·STATUS, 표 뒤 stderr에 unreachable이면 \"warning: no audit records from: …\", truncated면 \"note: more entries match; narrow the range or raise --limit\", -o json은 응답 그대로(stderr 없음). fake_api 시험, opld E2E(tests/audit.rs: 노드 셋 중 node-c는 [audit] enabled = false — 노드마다 바꾸는 요청 뒤 node-b의 oplctl -o json audit가 node-a·node-b 기록을 함께 보이고 unreachable 없음, 새 것부터, --node 거르기, 표, audit.log 0600, 꺼진 노드는 audit 디렉터리 없음)"
```

---

### Task 5: 웹: 감사 로그 화면

`/audit`(admin에게만 메뉴): 시간 범위(1시간·24시간·7일), 사용자·동작 거르기, 표(4xx·5xx 경고 색), 읽지 못한 노드·꺼진 노드·잘림 안내, 다시 읽기 실패 문구, 행을 누르면 항목 JSON. ko·en.

**Files:**
- Create: `web/src/audit/AuditPage.test.tsx`
- Create: `web/src/audit/AuditPage.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx
import { UsersPage } from "@/accounts/UsersPage";
import { LiveProvider } from "@/api/LiveProvider";
```

을 다음으로 바꾼다:

```tsx
import { UsersPage } from "@/accounts/UsersPage";
import { AuditPage } from "@/audit/AuditPage";
import { LiveProvider } from "@/api/LiveProvider";
```

`web/src/App.tsx`에서

```tsx
            { path: "tasks", element: <TasksPage /> },
            { path: "users", element: <UsersPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "tasks", element: <TasksPage /> },
            { path: "audit", element: <AuditPage /> },
            { path: "users", element: <UsersPage /> },
```

`web/src/api/types.ts`에서

```ts
export type TelemetryReport = Schemas["TelemetryReport"];
```

을 다음으로 바꾼다:

```ts
export type TelemetryReport = Schemas["TelemetryReport"];
export type AuditEntry = Schemas["AuditEntry"];
```

`web/src/audit/AuditPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { AuditEntry } from "@/api/types";
import { AuditPage } from "@/audit/AuditPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const START: AuditEntry = {
  id: "019a0f3c-8a10-7000-8000-000000000002",
  time: 1761192217104,
  node: "node-a",
  user: "alice",
  user_id: "00000000-0000-0000-0000-000000000002",
  role: "operator",
  auth: "token",
  ip: "10.0.0.5",
  method: "POST",
  path: "/vms/web-1/actions/start",
  action: "POST /vms/{key}/actions/{action}",
  target: { kind: "vms", key: "web-1" },
  status: 202,
  duration_ms: 12,
  detail: null,
};

const LOGIN: AuditEntry = {
  id: "019a0f3c-8a10-7000-8000-000000000001",
  time: 1761192200000,
  node: "node-b",
  user: null,
  user_id: null,
  role: null,
  auth: "none",
  ip: "10.0.0.9",
  method: "POST",
  path: "/auth/login",
  action: "POST /auth/login",
  target: null,
  status: 401,
  duration_ms: 3,
  detail: "username=mallory",
};

/** 감사 API의 응답을 정하고, 받은 쿼리를 모은다. */
function mockAudit(
  body: {
    entries: AuditEntry[];
    unreachable?: string[];
    disabled?: string[];
    truncated?: boolean;
  } = {
    entries: [START, LOGIN],
  },
) {
  const queries: URLSearchParams[] = [];
  server.use(
    http.get(apiUrl("/audit"), ({ request }) => {
      queries.push(new URL(request.url).searchParams);
      return HttpResponse.json({ unreachable: [], disabled: [], truncated: false, ...body });
    }),
  );
  return queries;
}

it("lists entries with their node, user, action, target and status", async () => {
  mockAudit();
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  const rows = within(screen.getByRole("table", { name: "Audit log" })).getAllByRole("row");
  expect(rows).toHaveLength(3);
  for (const text of [
    "node-a",
    "alice",
    "10.0.0.5",
    "POST /vms/{key}/actions/{action}",
    "vms/web-1",
    "202",
  ]) {
    expect(rows[1]).toHaveTextContent(text);
  }
  expect(rows[1]!.querySelector("time")).toHaveAttribute("datetime", "2025-10-23T04:03:37.104Z");
  // 인증 전 실패: 사용자가 없고, 대상이 없으니 detail을 보이고, 상태는 경고 색이다.
  const cells = within(rows[2]!).getAllByRole("cell");
  expect(cells[2]).toHaveTextContent("—");
  expect(cells[5]).toHaveTextContent("username=mallory");
  expect(cells[6]).toHaveTextContent("401");
  expect(cells[6]).toHaveClass("text-destructive");
  expect(within(rows[1]!).getAllByRole("cell")[6]).not.toHaveClass("text-destructive");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("puts the range and the applied filters in the query", async () => {
  const queries = mockAudit();
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  // 기본은 지난 24시간이고, 거르기는 넣지 않는다.
  expect(queries).toHaveLength(1);
  const day = Date.now() - Number(queries[0]!.get("since"));
  expect(day).toBeGreaterThanOrEqual(86_400_000);
  expect(day).toBeLessThan(86_400_000 + 60_000);
  expect([...queries[0]!.keys()]).toEqual(["since"]);

  await userEvent.selectOptions(screen.getByLabelText("Time range"), "Last hour");
  await vi.waitFor(() => expect(queries).toHaveLength(2));
  const hour = Date.now() - Number(queries[1]!.get("since"));
  expect(hour).toBeGreaterThanOrEqual(3_600_000);
  expect(hour).toBeLessThan(3_600_000 + 60_000);

  // 입력만 해서는 읽지 않는다. 적용해야 쿼리에 들어간다.
  await userEvent.type(screen.getByLabelText("User"), " alice ");
  await userEvent.type(screen.getByLabelText("Action"), "actions/start");
  expect(queries).toHaveLength(2);
  await userEvent.click(screen.getByRole("button", { name: "Apply" }));
  await vi.waitFor(() => expect(queries).toHaveLength(3));
  expect(queries[2]!.get("user")).toBe("alice");
  expect(queries[2]!.get("action")).toBe("actions/start");
  expect(Date.now() - Number(queries[2]!.get("since"))).toBeLessThan(3_600_000 + 60_000);
  expect(queries[2]!.has("limit")).toBe(false);
});

it("reads again on refresh", async () => {
  const queries = mockAudit();
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
  await vi.waitFor(() => expect(queries).toHaveLength(2));
});

it("warns about unreachable nodes and says when the list was cut", async () => {
  mockAudit({ entries: [START], unreachable: ["node-b", "node-c"], truncated: true });
  renderPage(<AuditPage />);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "No audit entries from these nodes (unreachable): node-b, node-c",
  );
  expect(screen.getByRole("status")).toHaveTextContent(
    "Showing the newest 1 entries. More match; narrow the range or the filters.",
  );
});

it("says which nodes have the audit log turned off", async () => {
  mockAudit({ entries: [{ ...START, target: null }], disabled: ["node-c", "node-d"] });
  renderPage(<AuditPage />);
  expect(await screen.findByRole("status")).toHaveTextContent(
    "The audit log is turned off on these nodes: node-c, node-d",
  );
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  // 대상도 detail도 없으면 줄표
  const row = within(screen.getByRole("table", { name: "Audit log" })).getAllByRole("row")[1]!;
  expect(within(row).getAllByRole("cell")[5]).toHaveTextContent("—");
});

it("keeps the entries and says when reading again failed", async () => {
  let calls = 0;
  server.use(
    http.get(apiUrl("/audit"), () => {
      calls += 1;
      return calls === 1
        ? HttpResponse.json({ entries: [START], unreachable: [], disabled: [], truncated: false })
        : HttpResponse.json({ code: "invalid_request", message: "bad since" }, { status: 400 });
    }),
  );
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Could not read the audit log again. bad since",
  );
  expect(screen.getByText("alice")).toBeInTheDocument();
});

it("shows an entry as JSON when its row is clicked", async () => {
  mockAudit();
  renderPage(<AuditPage />);
  const login = await screen.findByRole("button", { name: "POST /auth/login" });
  await userEvent.click(login.closest("tr")!.querySelector("td")!);
  const dialog = await screen.findByRole("dialog", { name: "Audit entry" });
  expect(dialog).toHaveTextContent('"detail": "username=mallory"');
  expect(dialog).toHaveTextContent('"node": "node-b"');
  await userEvent.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  await vi.waitFor(() => expect(login).toHaveFocus());
});

it("says why the log could not be read", async () => {
  server.use(
    http.get(apiUrl("/audit"), () =>
      HttpResponse.json({ code: "invalid_request", message: "bad since" }, { status: 400 }),
    ),
  );
  renderPage(<AuditPage />);
  expect(await screen.findByText("bad since")).toBeInTheDocument();
});

it("says empty ranges are empty", async () => {
  mockAudit({ entries: [] });
  renderPage(<AuditPage />);
  expect(await screen.findByText("No audit entries in this range.")).toBeInTheDocument();
});

it("does not read the log for other roles", async () => {
  const audit: string[] = [];
  const seen = ({ request }: { request: Request }) => {
    if (new URL(request.url).pathname === "/api/v1/audit") audit.push(request.url);
  };
  server.events.on("request:start", seen);
  try {
    renderPage(<AuditPage />, { as: { ...ADMIN, role: "operator" } });
    expect(
      await screen.findByText("Only administrators can read the audit log."),
    ).toBeInTheDocument();
    expect(audit).toEqual([]);
  } finally {
    server.events.removeListener("request:start", seen);
  }
});
```

`web/src/audit/AuditPage.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { AuditEntry } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { focusBack } from "@/components/focusBack";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";

/** 고를 수 있는 시간 범위(지금부터 거꾸로, 밀리초) */
const RANGES = { "1h": 3_600_000, "24h": 86_400_000, "7d": 604_800_000 } as const;
type Range = keyof typeof RANGES;

/**
 * 모든 노드의 감사 로그. admin만 읽는다. 자동으로 다시 읽지 않고, 범위·거르기를 바꾸거나 새로고침할 때
 * 읽는다(그때의 지금부터 범위를 잰다).
 */
export function AuditPage() {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const admin = me.role === "admin";
  const [range, setRange] = useState<Range>("24h");
  // 입력한 값과 적용한 값. 적용 버튼을 눌러야 쿼리에 들어간다.
  const [user, setUser] = useState("");
  const [action, setAction] = useState("");
  const [filter, setFilter] = useState({ user: "", action: "" });
  const [opened, setOpened] = useState<AuditEntry | null>(null);
  const returnFocus = useRef<HTMLElement | null>(null);
  const audit = useQuery({
    queryKey: ["audit", range, filter],
    queryFn: () =>
      call(
        api.GET("/audit", {
          params: {
            query: {
              since: String(Date.now() - RANGES[range]),
              ...(filter.user && { user: filter.user }),
              ...(filter.action && { action: filter.action }),
            },
          },
        }),
      ),
    enabled: admin,
  });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. admin이 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.audit")}</h1>
        <p className="text-sm text-muted-foreground">{t("audit.adminOnly")}</p>
      </div>
    );
  }

  const entries = audit.data?.entries ?? [];
  const unreachable = audit.data?.unreachable ?? [];
  const disabled = audit.data?.disabled ?? [];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="audit-title" className="text-2xl font-semibold">
          {t("nav.audit")}
        </h1>
        <Button variant="outline" disabled={audit.isFetching} onClick={() => void audit.refetch()}>
          <RefreshCw />
          {t("audit.refresh")}
        </Button>
      </div>
      <div className="flex flex-wrap items-end gap-4">
        <div className="flex flex-col gap-2">
          <Label htmlFor="audit-range">{t("audit.range")}</Label>
          <NativeSelect
            id="audit-range"
            value={range}
            onChange={(e) => setRange(e.target.value as Range)}
          >
            {(Object.keys(RANGES) as Range[]).map((r) => (
              <NativeSelectOption key={r} value={r}>
                {t(`audit.ranges.${r}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </div>
        <form
          noValidate
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setFilter({ user: user.trim(), action: action.trim() });
          }}
        >
          <div className="flex flex-col gap-2">
            <Label htmlFor="audit-user">{t("audit.user")}</Label>
            <Input id="audit-user" value={user} onChange={(e) => setUser(e.target.value)} />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="audit-action">{t("audit.action")}</Label>
            <Input
              id="audit-action"
              placeholder="POST /vms"
              value={action}
              onChange={(e) => setAction(e.target.value)}
            />
          </div>
          <Button type="submit" variant="outline">
            {t("audit.apply")}
          </Button>
        </form>
      </div>
      {/* 다시 읽지 못하면 마지막으로 읽은 내용을 그대로 보이고 오류를 알린다. */}
      {audit.isError && entries.length > 0 && (
        <p role="alert" className="text-sm text-destructive">
          {t("audit.refreshFailed", { error: errorMessage(t, audit.error) })}
        </p>
      )}
      {unreachable.length > 0 && (
        <p
          role="alert"
          className="rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-sm"
        >
          {t("audit.unreachable", { nodes: unreachable.join(", ") })}
        </p>
      )}
      {disabled.length > 0 && (
        <p role="status" className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm">
          {t("audit.disabled", { nodes: disabled.join(", ") })}
        </p>
      )}
      {audit.data?.truncated && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("audit.truncated", { count: entries.length })}
        </p>
      )}
      <Table aria-labelledby="audit-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("audit.time")}</TableHead>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("audit.user")}</TableHead>
            <TableHead>{t("audit.ip")}</TableHead>
            <TableHead>{t("audit.action")}</TableHead>
            <TableHead>{t("audit.target")}</TableHead>
            <TableHead>{t("audit.status")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.length === 0 && (
            <TableMessage colSpan={7} query={audit} empty={t("audit.empty")} />
          )}
          {entries.map((entry) => {
            const time = new Date(entry.time);
            return (
              <TableRow
                key={entry.id}
                className="cursor-pointer"
                onClick={(e) => {
                  returnFocus.current = e.currentTarget.querySelector("button");
                  setOpened(entry);
                }}
              >
                <TableCell className="whitespace-nowrap">
                  <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>
                </TableCell>
                <TableCell>{entry.node}</TableCell>
                <TableCell>{entry.user ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">{entry.ip}</TableCell>
                <TableCell>
                  {/* 키보드로도 열 수 있게 단추로 둔다. 누르면 행의 onClick이 연다. */}
                  <button
                    type="button"
                    className="text-left font-mono text-xs underline-offset-4 hover:underline"
                  >
                    {entry.action}
                  </button>
                </TableCell>
                <TableCell>
                  {/* 대상이 없으면 detail(예: audit.dropped의 수, 로그인의 username=…) */}
                  {entry.target
                    ? `${entry.target.kind}/${entry.target.key}`
                    : (entry.detail ?? "—")}
                </TableCell>
                <TableCell
                  className={entry.status >= 400 ? "font-medium text-destructive" : undefined}
                >
                  {entry.status}
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {opened && (
        <Dialog open onOpenChange={(open) => !open && setOpened(null)}>
          <DialogContent
            aria-describedby={undefined}
            className="sm:max-w-2xl"
            onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
          >
            <DialogHeader>
              <DialogTitle>{t("audit.entryTitle")}</DialogTitle>
            </DialogHeader>
            <pre className="max-h-96 overflow-auto rounded-md bg-muted p-2 font-mono text-xs">
              {JSON.stringify(opened, null, 2)}
            </pre>
            <DialogFooter>
              <Button variant="outline" onClick={() => setOpened(null)}>
                {t("common.done")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}
```

`web/src/i18n/en.json`에서

```json
    "tasks": "Tasks",
    "users": "Users",
```

을 다음으로 바꾼다:

```json
    "tasks": "Tasks",
    "audit": "Audit log",
    "users": "Users",
```

`web/src/i18n/en.json`에서

```json
    }
  }
```

을 다음으로 바꾼다:

```json
    }
  },
  "audit": {
    "adminOnly": "Only administrators can read the audit log.",
    "refresh": "Refresh",
    "range": "Time range",
    "ranges": {
      "1h": "Last hour",
      "24h": "Last 24 hours",
      "7d": "Last 7 days"
    },
    "user": "User",
    "action": "Action",
    "apply": "Apply",
    "time": "Time",
    "ip": "IP",
    "target": "Target",
    "status": "Status",
    "refreshFailed": "Could not read the audit log again. {{error}}",
    "empty": "No audit entries in this range.",
    "unreachable": "No audit entries from these nodes (unreachable): {{nodes}}",
    "disabled": "The audit log is turned off on these nodes: {{nodes}}",
    "truncated": "Showing the newest {{count}} entries. More match; narrow the range or the filters.",
    "entryTitle": "Audit entry"
  }
```

`web/src/i18n/ko.json`에서

```json
    "tasks": "작업",
    "users": "사용자",
```

을 다음으로 바꾼다:

```json
    "tasks": "작업",
    "audit": "감사 로그",
    "users": "사용자",
```

`web/src/i18n/ko.json`에서

```json
    }
  }
```

을 다음으로 바꾼다:

```json
    }
  },
  "audit": {
    "adminOnly": "관리자만 감사 로그를 읽을 수 있습니다.",
    "refresh": "새로고침",
    "range": "시간 범위",
    "ranges": {
      "1h": "지난 1시간",
      "24h": "지난 24시간",
      "7d": "지난 7일"
    },
    "user": "사용자",
    "action": "동작",
    "apply": "적용",
    "time": "시각",
    "ip": "IP",
    "target": "대상",
    "status": "상태",
    "refreshFailed": "감사 로그를 다시 읽지 못했습니다. {{error}}",
    "empty": "이 범위에 감사 기록이 없습니다.",
    "unreachable": "다음 노드의 감사 기록을 읽지 못했습니다: {{nodes}}",
    "disabled": "다음 노드는 감사 로그가 꺼져 있습니다: {{nodes}}",
    "truncated": "최근 {{count}}개만 보입니다. 더 있으니 범위나 거르기를 좁히세요.",
    "entryTitle": "감사 항목"
  }
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.getByRole("link", { name: "Users" })).toHaveAttribute("href", "/users");
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("link", { name: "Users" })).toHaveAttribute("href", "/users");
  expect(screen.getByRole("link", { name: "Audit log" })).toHaveAttribute("href", "/audit");
});
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업, 가져오기는 누구나 읽는다.
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 감사 로그도 admin만 읽는다.
  expect(screen.queryByRole("link", { name: "Audit log" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업, 가져오기는 누구나 읽는다.
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  Network,
  Server,
```

을 다음으로 바꾼다:

```tsx
  Network,
  ScrollText,
  Server,
```

`web/src/layout/AppLayout.tsx`에서

```tsx

/** 왼쪽 메뉴. `kind`가 있으면 그 종류를 읽을 수 있는 역할에만 보인다. */
const NAV: { to: string; key: string; icon: LucideIcon; end: boolean; kind?: Kind }[] = [
  { to: "/", key: "nav.dashboard", icon: LayoutDashboard, end: true },
```

을 다음으로 바꾼다:

```tsx

/** 왼쪽 메뉴. `kind`가 있으면 그 종류를 읽을 수 있는 역할에만, `admin`이면 admin에게만 보인다. */
const NAV: {
  to: string;
  key: string;
  icon: LucideIcon;
  end: boolean;
  kind?: Kind;
  admin?: boolean;
}[] = [
  { to: "/", key: "nav.dashboard", icon: LayoutDashboard, end: true },
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
  { to: "/users", key: "nav.users", icon: Users, end: false, kind: "user" },
```

을 다음으로 바꾼다:

```tsx
  { to: "/tasks", key: "nav.tasks", icon: ListChecks, end: false },
  { to: "/audit", key: "nav.audit", icon: ScrollText, end: false, admin: true },
  { to: "/users", key: "nav.users", icon: Users, end: false, kind: "user" },
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  });
  const nav = NAV.filter((item) => !item.kind || can(me.role, "read", item.kind));
  return (
```

을 다음으로 바꾼다:

```tsx
  });
  const nav = NAV.filter(
    (item) =>
      (!item.kind || can(me.role, "read", item.kind)) && (!item.admin || me.role === "admin"),
  );
  return (
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 감사 로그 화면 — /audit(admin만, 메뉴 nav.audit \"감사 로그\"/\"Audit log\"는 admin에게만 보이고 다른 역할은 주소로 와도 읽지 않고 안내). 시간 범위(지난 1시간·24시간·7일, 기본 24시간 — 읽을 때의 지금에서 뺀 유닉스 밀리초를 since로), 사용자·동작 입력은 적용을 눌러야 쿼리에 들어감, 표(시각(로컬), 노드, 사용자, IP, 동작, 대상 kind/key, 상태 — 4xx·5xx는 경고 색), unreachable 경고, truncated 안내, 행을 누르면 항목 JSON 대화상자, 새로고침 버튼(자동 갱신 없음), 읽기 오류·빈 범위 문구, ko·en"
```

---

### Task 6: 문서

README 새 절 "감사 로그"(무엇을 적나, 파일 위치·권한·회전, journald `opl_audit`와 rsyslog 전달 예, `[audit]`, `oplctl audit`, API, 웹, 한계), 엔터프라이즈 설계 §2.6 "구현(계획 34)"과 §9, 처음 분해 문서의 하위 프로젝트 10 줄, 계획 32·33 설계의 감사 로그 메모.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`
- Modify: `docs/superpowers/specs/2026-11-06-enterprise-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
  (`Settings.spec.subscription`)에 저장하지만 API(`GET`·`PATCH /api/v1/settings`, watch)로는 내보내지 않는다. 감사
  로그는 없고 opld 로그에 키 id만 남는다.
- **웹.** "설정" 화면의 구독 카드가 상태·고객·수준·노드(사용/한도)·만료일을 보이고, admin은 키를 붙여 넣거나 지운다.
```

을 다음으로 바꾼다:

```markdown
  (`Settings.spec.subscription`)에 저장하지만 API(`GET`·`PATCH /api/v1/settings`, watch)로는 내보내지 않는다. 감사
  로그(`detail` `id=<키 id>`, "감사 로그")와 opld 로그에는 키 id만 남는다.
- **웹.** "설정" 화면의 구독 카드가 상태·고객·수준·노드(사용/한도)·만료일을 보이고, admin은 키를 붙여 넣거나 지운다.
```

`README.md`에서

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`README.md`에서

```markdown
  때 한 번 인증하므로 비밀번호를 바꿔도 끊기지 않는다.
```

을 다음으로 바꾼다:

````markdown
  때 한 번 인증하므로 비밀번호를 바꿔도 끊기지 않는다.

## 감사 로그

노드마다 그 노드가 받은 바꾸는 API 요청(`POST`·`PUT`·`PATCH`·`DELETE`, 성공·실패와 401·403 포함), 로그인 성공·실패,
로그아웃, VM 콘솔 열기(`GET /api/v1/vms/{key}/console`)를 한 줄 JSON으로 적는다. 다른 `GET`(감사 로그 읽기도),
웹 정적 파일, 노드 사이 RPC, 제어 소켓(`cluster init`·`join`·`leave`), 맞는 라우트가 없는 요청은 적지 않는다.
요청 본문은 적지 않는다.

```bash
oplctl audit                                     # admin. 모든 노드의 지난 24시간, 새 것부터 200개
oplctl audit --since 1h --user alice             # --since: 90s·30m·1h·7d(지금부터 거슬러) 또는 RFC 3339
oplctl audit --since 2026-11-06T00:00:00Z --until 2026-11-07T00:00:00Z --node node1 --limit 1000
oplctl audit --action web-1                      # 동작이나 경로에 든 문자열(VM 동작은 경로로 가른다)
# TIME                  NODE   USER   IP        ACTION                            TARGET     STATUS
# 2026-11-06T09:30:05Z  node1  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202
# 2026-11-06T09:29:58Z  node2  -      10.0.0.9  POST /auth/login                  username=bob  401
oplctl -o json audit --since 7d                  # 응답 그대로(항목의 모든 필드)
```

- **항목.** `{"id","time","node","user","user_id","role","auth","ip","method","path","action","target","status",
  "duration_ms","detail"}`. `id`는 UUID v7, `time`은 유닉스 밀리초, `node`는 적은 노드의 이름이다. `user`·`user_id`·
  `role`은 인증 전이거나 실패하면 `null`, `auth`는 `session`·`token`·`none`(`Authorization: Bearer`가 있으면 인증에
  실패해도 `token`)이다. `ip`는 시도 제한과 같은 클라이언트 IP(TLS 연결의 주소, "로그인과 세션"), `path`는
  `/api/v1` 아래의 실제 경로이고 쿼리는 넣지 않는다(쿼리에 비밀이 올 수 있다). `action`은 `"<메서드> <라우트
  템플릿>"`(예: `POST /vms/{key}/actions/{action}`, `DELETE /users/{key}`, `POST /auth/login`)이라 새 API도 저절로
  이름을 가진다. `target`은 템플릿의 첫 조각과 첫 매개변수 값(`{"kind":"vms","key":"web-1"}`)이고, 매개변수가 없는
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다), 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
  `detail`·`user`는 512바이트까지만 적는다(넘으면 글자 경계에서 잘라 끝에 `…`).
- **파일.** `{data_dir}/audit/audit.log`(패키지는 `/var/lib/opl/audit/audit.log`). 디렉터리는 0700, 파일은 0600이다.
  요청은 디스크를 기다리지 않는다: 쓰기 스레드가 덧붙이고 1초마다 묶어 `fsync`한다. 줄을 더하면 `max_file_mib`를
  넘을 때 `audit.log` → `audit.log.1` → `.2` …로 밀고, 지금 파일을 포함해 `max_files`개를 넘는 가장 오래된 것을 지운다.
  쓰기가 밀려 대기열(1만 개)이 차면 항목을 버리고, 다음 항목 앞에 `"action":"audit.dropped"`, `"detail":"<N> entries
  dropped"` 항목을 적는다. 인증 없는 항목(인증 실패·로그인 실패·익명, `user`가 `null`)은 IP마다 1분에 60개까지만
  적고(IPv6는 /64 네트워크 하나로 센다), 넘친 것은 그 1분 창이 끝나고 1초 안에(또는 opld를 멈출 때)
  `"action":"audit.suppressed"`, `"ip"`(IPv6면 `<네트워크>/64`), `"detail":"<N> unauthenticated requests
  not recorded since <창 시작 RFC 3339>"` 항목으로 수만 남긴다(IP는 1만 개까지 세고, 넘치면 가장 오래된 것의
  수를 바로 적고 버린다). 인증된 요청은 한도가 없다. opld가 열 때 디렉터리는 0700, 파일은 0600으로 다시 맞추고,
  `audit.log`가 심볼릭 링크면 열지 않으며(opld가 시작하지 않음), `max_files`를 줄였으면 넘는 옛 파일을 지운다.
- **journald.** 같은 JSON을 tracing 대상 `opl_audit`(INFO)로도 내보낸다. 패키지의 opld는 systemd 서비스라 journald에
  `… INFO opl_audit: {"id":…}` 줄로 남는다(`journalctl -u opld -g opl_audit`). `RUST_LOG`를 바꿀 때는 `opl_audit=info`가
  남게 한다(파일은 `RUST_LOG`와 상관없다). opld는 로그가 터미널로 갈 때만 색 코드를 넣으므로 journald 줄에는 색 코드가
  없다(`NO_COLOR`를 따로 줄 필요가 없다). 밖으로 보내려면 rsyslog로 전달한다. Debian 13은 rsyslog가 기본으로 없다
  (`apt install rsyslog`):

  ```
  # /etc/rsyslog.d/60-opl-audit.conf
  # rsyslog가 이미 journald에서 받으면(/var/log/syslog에 opld 줄이 보이면) 다음 module 줄은 뺀다.
  module(load="imjournal" StateFile="imjournal.state")
  if $programname == 'opld' and $msg contains 'opl_audit:' then {
      action(type="omfwd" target="siem.example.com" port="514" protocol="tcp")
  }
  ```

  `systemctl restart rsyslog` 뒤 바꾸는 요청 하나를 보내 수집 서버에 줄이 오는지 본다.
- **읽기.** `GET /api/v1/audit?since&until&user&node&action&limit`(admin만, 다른 역할은 **403**)는 받은 노드가
  위트니스가 아닌 모든 멤버의 감사 로그를 읽어(자기 파일은 직접, 다른 노드는 노드 사이 RPC로 노드마다 5초 제한)
  시간 내림차순으로 합친다. `since`·`until`은 유닉스 밀리초나 RFC 3339(기본: 지금까지 24시간, 양 끝 포함), `user`·
  `node`는 정확히 같은 이름, `action`은 `action`이나 `path`에 든 문자열이다. `limit`은 1~5000(기본 200), 아니거나 시각이
  틀리면 **400** `invalid_request`다. 응답은 `{"entries":[…],"unreachable":["node3"],"disabled":["node4"],"truncated":false}`:
  읽지 못한 노드(꺼짐, 제한 시간, 이 기능이 없는 옛 opld)는 `unreachable`에, 감사 로그를 끈(`[audit] enabled = false`)
  노드는 `disabled`에 이름 순으로, 조건에 맞는 항목이 `limit`보다 많으면 `truncated: true`다. 같은 항목은 한 번만 온다
  (읽는 중에 회전해도). oplctl은 표 뒤 stderr에 `warning: no audit records from: node3`, `note: the audit log is disabled
  on: node4`, `note: more entries match; narrow the range or raise --limit`을 쓴다(`-o json`이면 쓰지 않는다). 표의
  `TARGET`은 대상이 없으면 `detail`(로그인의 `username=…`, `audit.dropped`의 수)이다. 기간 `--since`(`1h` 등)와 `--until`은
  함께 줄 수 없다(`a relative --since cannot be combined with --until; give a time` — `--since`에 시각을 준다).
- **웹.** "감사 로그" 메뉴(admin에게만 보인다): 시간 범위(지난 1시간·24시간·7일, 기본 24시간), 사용자·동작 입력(적용을
  눌러야 거른다), 표(시각은 브라우저의 지역 시간, 대상이 없으면 `detail`, 4xx·5xx 상태는 경고 색), 읽지 못한 노드 경고,
  감사 로그를 끈 노드 안내와 잘림 안내, 행을 누르면 항목 JSON. 자동으로 다시 읽지 않는다(새로고침 버튼). 다시 읽기가
  실패하면 마지막 표를 두고 위에 오류를 보인다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[audit]
enabled = true       # 끄면 이 노드는 파일도 journald 줄도 남기지 않는다
max_file_mib = 16    # 한 파일의 최대 크기(MiB, 1~256 — 읽을 때 파일을 통째로 메모리에 올린다)
max_files = 8        # 지금 파일을 포함해 남기는 파일 수(1 이상). 0이면 opld가 시작하지 않는다
```

감사 디렉터리를 만들거나 파일을 열지 못하면 opld가 시작하지 않는다. opld를 멈출 때는 API를 닫은 뒤 남은 항목을 쓰고
`fsync`한다.

- **한계.** 꺼진 노드의 기록은 그 노드가 돌아와야 보이고, 디스크를 잃으면 사라진다(journald 전달로 보완한다). 변조
  방지(해시 체인·서명)는 없어 노드의 root는 파일을 고칠 수 있다. 롤링 업그레이드 중 옛 opld 노드는 `unreachable`이다.
  opld가 갑자기 죽으면 대기열에 남은 항목을, 전원이 나가면 마지막 1초쯤을 잃을 수 있다. 버린 항목은 수만 남는다.
  파일 쓰기가 실패하면(디스크가 참 등) opld 로그에 경고만 남기고 요청은 그대로 처리한다. 리버스 프록시 뒤에서는
  `ip`가 프록시 주소다. 콘솔은 연 것만 적고 닫은 때와 입력은 적지 않는다. 알림과 날짜로 지우기는 없다(크기와 개수로만
  지운다). 위트니스 노드는 API가 없어 감사 로그도 없다. 인증 없는 요청은 IP(IPv6는 /64)마다 1분에 60개까지만 적고 나머지는
  창이 끝나고 1초 안에 적히는 `audit.suppressed`의 수만 남는다(같은 IP에서 많은 로그인 실패가 오면 그 시각·이름은
  다 남지 않는다). 각 문자열 필드는 512바이트까지만 적는다. 로그인의 사용자 이름 칸에 비밀번호를 잘못 넣으면 이름 형식(1~63자의 영문 소문자·숫자·`-`)이면
  `username=<그 값>`으로 그대로 적힌다. 감사 로그를 끈 노드는 `disabled`로 보일 뿐 그 노드의 요청은 어디에도 남지 않는다.
  `max_file_mib`는 256까지이고, 읽을 때 파일 하나를 통째로 메모리에 올린다(한 번 읽기에 최대 `max_file_mib`쯤).
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료), 계획 32(버전과 업데이트: Debian 규칙의 버전 비교, `GET /api/v1/version`, 노드 버전과 낮은 버전의 합류 거절, 클러스터 설정 `Settings`의 업데이트 채널 `stable`·`testing`, 노드마다 opld가 관리하는 APT 소스 파일과 주기적인 업데이트 확인(`NodeStatus.updates`), 다시 확인 Task와 한 번에 한 노드씩 `systemd-run`으로 도는 업그레이드 Task(opld가 다시 시작해도 이어 봄), 서명한 APT 저장소 스크립트 `build-repo.sh`와 패키지 키링, oplctl·웹, 완료)과 계획 33(구독과 텔레메트리: Ed25519로 서명한 오프라인 구독 키와 발급 도구 `opl-license`, 빌드 값 `OPL_LICENSE_KEYS`(없으면 개발 키, 정식 패키지는 반드시), 읽을 때 계산하는 구독 상태(none·active·expired·invalid·over_limit), 구독이 있어야 하는 `enterprise` 채널과 노드의 APT 인증 파일, 이 빌드보다 낮은 노드가 있으면 새 설정 값을 쓰지 않는 게이트 `require_current`, 옵트인 텔레메트리(미리 보기, 리더의 `TelemetryReporter`가 하루 한 번, 수만 보냄), 웹 설정 화면과 대시보드 구독 안내, oplctl, 완료)으로 나눴다(`2026-11-05-install-update-subscription-design.md`). 하위 프로젝트 9는 끝났다. 오프라인 설치, ZFS 루트·RAID 설치, arm64 ISO, 기능 잠금, 온라인 구독 확인, 구독 키의 클러스터 묶기(하드웨어 지문), 텔레메트리 수집 서버, 자동·롤링 업그레이드, 노드 비우기(유지보수 모드), 커널·펌웨어 관리, 업데이트 되돌리기는 넣지 않았다.
```

을 다음으로 바꾼다:

```markdown
구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료), 계획 32(버전과 업데이트: Debian 규칙의 버전 비교, `GET /api/v1/version`, 노드 버전과 낮은 버전의 합류 거절, 클러스터 설정 `Settings`의 업데이트 채널 `stable`·`testing`, 노드마다 opld가 관리하는 APT 소스 파일과 주기적인 업데이트 확인(`NodeStatus.updates`), 다시 확인 Task와 한 번에 한 노드씩 `systemd-run`으로 도는 업그레이드 Task(opld가 다시 시작해도 이어 봄), 서명한 APT 저장소 스크립트 `build-repo.sh`와 패키지 키링, oplctl·웹, 완료)과 계획 33(구독과 텔레메트리: Ed25519로 서명한 오프라인 구독 키와 발급 도구 `opl-license`, 빌드 값 `OPL_LICENSE_KEYS`(없으면 개발 키, 정식 패키지는 반드시), 읽을 때 계산하는 구독 상태(none·active·expired·invalid·over_limit), 구독이 있어야 하는 `enterprise` 채널과 노드의 APT 인증 파일, 이 빌드보다 낮은 노드가 있으면 새 설정 값을 쓰지 않는 게이트 `require_current`, 옵트인 텔레메트리(미리 보기, 리더의 `TelemetryReporter`가 하루 한 번, 수만 보냄), 웹 설정 화면과 대시보드 구독 안내, oplctl, 완료)으로 나눴다(`2026-11-05-install-update-subscription-design.md`). 하위 프로젝트 9는 끝났다. 오프라인 설치, ZFS 루트·RAID 설치, arm64 ISO, 기능 잠금, 온라인 구독 확인, 구독 키의 클러스터 묶기(하드웨어 지문), 텔레메트리 수집 서버, 자동·롤링 업그레이드, 노드 비우기(유지보수 모드), 커널·펌웨어 관리, 업데이트 되돌리기는 넣지 않았다.

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP), 계획 36(OIDC SSO), 계획 37(원격 클러스터), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 35~38은 남았다.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
  - `PATCH`는 JSON merge patch가 아니라 기존 Patch 관례다: `SettingsPatch { updates: Option<{ channel: Option<Channel> }> }`(`deny_unknown_fields`). `{}`는 400, `{"updates":{}}`는 쓰지 않고 200. 없으면 `create`, 있으면 `update_spec`. `If-Match`가 있으면 그 revision(없는 설정은 0)이 아니면 412(`If-Match: 0`으로 함께 만들다 진 요청도 409 `name_taken`이 아니라 412), 없으면 `RevisionMismatch`·`NameTaken`에 10번 다시 한 뒤 409 `busy`. 같은 값이면 쓰지 않는다.
  - 감사 로그가 코드베이스에 없으므로 남기지 않는다.
- `[updates]`(opld `UpdatesSection`): 설계의 키에 `first_check`(기본 "1m"), `dpkg`(`/usr/bin/dpkg`), `poll`(기본 "2s")을 더했다. `enabled`가 없으면 `apt_get`이 있을 때 켜진다. 꺼지면 `opl_api::UpdatesConfig`·`Updates`가 아예 없다(`TaskRunnerConfig.updates: None`). 같은 `Arc<Updates>`를 Task 실행기와 `UpdateAgent`가 쓴다. opld 전체 프로세스 시험의 데몬은 `enabled = false`다(Ubuntu CI 러너에 진짜 `apt-get`이 있다).
```

을 다음으로 바꾼다:

```markdown
  - `PATCH`는 JSON merge patch가 아니라 기존 Patch 관례다: `SettingsPatch { updates: Option<{ channel: Option<Channel> }> }`(`deny_unknown_fields`). `{}`는 400, `{"updates":{}}`는 쓰지 않고 200. 없으면 `create`, 있으면 `update_spec`. `If-Match`가 있으면 그 revision(없는 설정은 0)이 아니면 412(`If-Match: 0`으로 함께 만들다 진 요청도 409 `name_taken`이 아니라 412), 없으면 `RevisionMismatch`·`NameTaken`에 10번 다시 한 뒤 409 `busy`. 같은 값이면 쓰지 않는다.
  - 감사 로그가 코드베이스에 없으므로 남기지 않는다. (계획 34의 감사 로그가 `PATCH /settings`를 적는다)
- `[updates]`(opld `UpdatesSection`): 설계의 키에 `first_check`(기본 "1m"), `dpkg`(`/usr/bin/dpkg`), `poll`(기본 "2s")을 더했다. `enabled`가 없으면 `apt_get`이 있을 때 켜진다. 꺼지면 `opl_api::UpdatesConfig`·`Updates`가 아예 없다(`TaskRunnerConfig.updates: None`). 같은 `Arc<Updates>`를 Task 실행기와 `UpdateAgent`가 쓴다. opld 전체 프로세스 시험의 데몬은 `enabled = false`다(Ubuntu CI 러너에 진짜 `apt-get`이 있다).
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
- **옛 노드 게이트 `require_current`**(설계에 없음): 계획 32 노드의 `SettingsSpec`·`SettingsStatus`는 `deny_unknown_fields`이고 `Channel`에 `enterprise`가 없어, 새 필드가 든 설정 Raft 항목을 풀지 못한다. 그래서 (1) 새 필드는 기본값이면 직렬화하지 않고(`subscription` None, `telemetry` 기본값, `status.telemetry` 빈 값) 채널만 바꾸는 쓰기는 옛 노드도 계속 풀며, (2) 새 값을 쓰는 모든 곳(구독 `PUT`·`DELETE`, `enterprise`로 바꾸는 `PATCH`, `telemetry`가 든 `PATCH`(끄기도), `reset-id`)은 쓰기 전에 위트니스를 포함한 모든 노드의 `status.version`이 있고 이 빌드(`BUILD`) 이상인지 보고 아니면 409 `cluster_upgrading`(`require_upgraded`와 같은 문구, 이름 순)이다. 리포터도 이 조건이 아니면 그 틱을 건너뛴다. `require_upgraded`(버전 없는 노드만 봄)는 업그레이드 경로가 낮은 노드가 있어도 돌아야 하므로 그대로 둔다. 두 struct의 `deny_unknown_fields`도 그대로다.
- 구독 API: `PUT` 거절 순서 403 → 400 `invalid_subscription_key` → 409 `cluster_upgrading`, `DELETE`는 403 → 409(없으면 쓰지 않고 200 `none`). 설정 쓰기의 다시 하기 루프를 `settings::write`로 빼 `PATCH /settings`와 함께 쓴다. 키 문자열은 `GET`·`PATCH /settings`, `reset-id`의 응답과 watch의 설정 이벤트에서 지우고(`settings::public`), OpenAPI 스키마에서도 숨긴다. 설계와 다른 점: 감사 로그가 코드베이스에 없으므로 남기지 않고 `tracing::info!`로 키 id만 남긴다(텔레메트리 바꾸기도 감사 로그 없음).
- APT 인증 파일의 두 번째 방어: 키 검사가 id를 막지만, `auth_contents`는 호스트나 id에 공백·제어 문자가 있으면 쓰지 않는다(관리 파일은 지우고 `warn!`).
```

을 다음으로 바꾼다:

```markdown
- **옛 노드 게이트 `require_current`**(설계에 없음): 계획 32 노드의 `SettingsSpec`·`SettingsStatus`는 `deny_unknown_fields`이고 `Channel`에 `enterprise`가 없어, 새 필드가 든 설정 Raft 항목을 풀지 못한다. 그래서 (1) 새 필드는 기본값이면 직렬화하지 않고(`subscription` None, `telemetry` 기본값, `status.telemetry` 빈 값) 채널만 바꾸는 쓰기는 옛 노드도 계속 풀며, (2) 새 값을 쓰는 모든 곳(구독 `PUT`·`DELETE`, `enterprise`로 바꾸는 `PATCH`, `telemetry`가 든 `PATCH`(끄기도), `reset-id`)은 쓰기 전에 위트니스를 포함한 모든 노드의 `status.version`이 있고 이 빌드(`BUILD`) 이상인지 보고 아니면 409 `cluster_upgrading`(`require_upgraded`와 같은 문구, 이름 순)이다. 리포터도 이 조건이 아니면 그 틱을 건너뛴다. `require_upgraded`(버전 없는 노드만 봄)는 업그레이드 경로가 낮은 노드가 있어도 돌아야 하므로 그대로 둔다. 두 struct의 `deny_unknown_fields`도 그대로다.
- 구독 API: `PUT` 거절 순서 403 → 400 `invalid_subscription_key` → 409 `cluster_upgrading`, `DELETE`는 403 → 409(없으면 쓰지 않고 200 `none`). 설정 쓰기의 다시 하기 루프를 `settings::write`로 빼 `PATCH /settings`와 함께 쓴다. 키 문자열은 `GET`·`PATCH /settings`, `reset-id`의 응답과 watch의 설정 이벤트에서 지우고(`settings::public`), OpenAPI 스키마에서도 숨긴다. 설계와 다른 점: 감사 로그가 코드베이스에 없으므로 남기지 않고 `tracing::info!`로 키 id만 남긴다(텔레메트리 바꾸기도 감사 로그 없음). (계획 34에서 감사 로그를 더함 — 구독 `PUT`(`detail`에 키 id)·`DELETE`, 텔레메트리를 바꾸는 `PATCH /settings`와 `reset-id`도 모든 바꾸는 요청처럼 감사 로그에 적힌다)
- APT 인증 파일의 두 번째 방어: 키 검사가 id를 막지만, `auth_contents`는 호스트나 id에 공백·제어 문자가 있으면 쓰지 않는다(관리 파일은 지우고 `warn!`).
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
- 웹 Vitest: 표, 거르기, 경고, 권한에 따른 메뉴.
```

을 다음으로 바꾼다:

```markdown
- 웹 Vitest: 표, 거르기, 경고, 권한에 따른 메뉴.

### 2.6 구현(계획 34)

- 타입(`opl_types::audit`): `AuditEntry`(§2.1의 필드), `AuditAuth{Session, Token, None}`(snake_case), `AuditTarget{kind, key}`, `AuditQuery{since, until, user, action, limit}`(`MAX_LIMIT` 5000, `matches`: 시각은 양 끝 포함, `user`는 정확히 같음), `AuditResponse{entries, unreachable, disabled, truncated}`. 설계와 다른 점:
  - `action`은 라우트 표의 이름(`vm.start`)이 아니라 `"<METHOD> <라우트 템플릿>"`(axum `MatchedPath`에서 `/api/v1`을 뗀 것, 예: `POST /vms/{key}/actions/{action}`, `POST /auth/login`)이다. 표를 따로 두지 않아 새 API가 저절로 이름을 가진다. VM 동작은 템플릿이 `/vms/{key}/actions/{action}` 하나라 `action`만으로는 가를 수 없으므로 `AuditQuery`의 `action` 거르기는 `action` **또는 `path`**의 부분 문자열이다.
  - `target`은 템플릿의 첫 조각(복수형 그대로 `vms`)과 템플릿의 첫 `{…}` 자리의 실제 경로 조각이다. 매개변수가 없는 템플릿은 None.
  - 넘친 기록은 `action: "audit.dropped"`, `detail: "<N> entries dropped"`, method·path·ip 빈 문자열, status 0.
  - (설계에 없음) 인증 없는 항목(`user` None — 인증 실패·로그인 실패·익명)은 IP마다 1분 창에 60개까지만 적는다. IP는 IPv4면 주소, IPv6면 /64 네트워크로 센다(계획 13 `limit::counted_ip`와 같은 규칙, IPv4-mapped는 IPv4). 넘친 것은 그 IP의 카운터로 세고, 창이 끝나면 쓰기 스레드의 1초 틱이 끝난 창을 훑어 `action: "audit.suppressed"`, `ip`(세는 단위 — IPv6는 `<네트워크>/64`), `detail: "<N> unauthenticated requests not recorded since <창 시작 RFC 3339>"`(`time`은 적는 시각), method·path 빈 문자열, status 0 항목을 적고 그 기록을 지운다(넘친 것이 없는 끝난 창도 지움). 그래서 넘친 수는 창이 끝나고 1초 안에 적힌다. 훑기 전에 그 IP가 다시 오면 그 항목 앞에, `shutdown` 때는 남은 것을 모두 적는다. 기록표는 IP 1만 개까지이고(끝난 창이 곧 지워지므로 1분 안에 서로 다른 IP 1만 개가 와야 닿음) 넘치면 가장 오래된 창을 버린다(그 카운터가 0이 아니면 그 `audit.suppressed`를 바로 적음). 가장 오래된 창과 끝난 창은 `(창 시작, 키)` `BTreeSet` 색인으로 찾아 표 전체를 훑지 않는다. 인증된 항목은 한도가 없다. 인증 없는 요청이 쏟아져도 파일이 회전으로 밀려 나가지 않게 하려는 것이다.
  - (설계에 없음) `path`·`target.key`·`detail`·`user`는 각각 512바이트까지만 적는다(UTF-8 글자 경계에서 자르고 끝에 `…`).
- 쓰기(`opl_api::audit`, `AuditConfig{dir, node, max_file_size, max_files}`·`AuditLog`): `disabled()`(아무것도 적지 않고 읽으면 빈 목록), `open(cfg)`(디렉터리 0700), `record`(동기 `try_send`, `node`는 설정의 노드 이름으로 덮음, 필드 자르기와 인증 없는 항목의 IP별 한도 — 시계는 `Instant`, 시험은 비공개 `record_at`), `read`(`spawn_blocking`), `shutdown`(채널에 남은 것과 남은 `audit.suppressed`를 쓰고 fsync, 5초 안에 끝나지 않으면 `warn!`하고 돌아감 — 종료 메시지는 막히는 `send` 대신 `try_send`와 10 ms async sleep을 마감까지 되풀이해 넘기므로 시간이 지나도 막힌 blocking 태스크가 남지 않는다), `is_enabled`. 쓰기 스레드가 끝난 뒤의 `record`는 처음 한 번 `warn!`하고 그 뒤는 버린 수로 센다. 설계와 다른 점: 쓰기는 tokio 태스크가 아니라 전용 스레드 `opl-audit`(std `sync_channel` 1만)다 — 파일 쓰기·fsync가 동기라서. `open`은 이미 있던 디렉터리도 0700으로 맞추고, `audit.log`를 `O_NOFOLLOW`로 열어(심볼릭 링크면 실패 — opld 시작 오류) 0600으로 맞추며(`libc`를 opl-api의 직접 의존으로), `max_files` 이상의 옛 `audit.log.N`(`max_files`를 줄인 경우)을 지운다. 파일 `audit.log`는 0600, 쓰기 전 크기 + 줄 길이가 한도를 넘으면(빈 파일이 아닐 때) 회전(`.{max_files-1}` 지우고 `.N` → `.N+1`, `audit.log` → `.1`), 쓴 것이 있으면 1초마다와 회전·종료 때 `sync_data`. 쓰기 실패는 `warn!`만 하고 다음 쓰기에서 파일을 다시 연다. tracing `opl_audit`(info, 메시지는 JSON) 줄은 파일에 쓸 때 낸다(버린 항목은 journald에도 없다). 읽기는 새 파일부터 통째로 읽어 줄을 거꾸로 훑고(망가진 줄 건너뜀), 파일의 가장 새 줄이 `since` 앞이면 멈추고 가장 오래된 줄이 `until` 뒤면 그 파일을 건너뛰며, `limit + 1`개까지 모은다(합칠 때 `truncated` 판단). 읽는 중에 회전하면 같은 항목이 두 파일에서 보일 수 있어 `id`로 중복을 없앤다.
- 기록 지점: `router()`가 `api()`에 `route_layer(from_fn_with_state(state, audit::layer))`를 붙인다(라우트가 맞은 요청만 — 맞는 라우트가 없는 404와 `/openapi.json`, opld의 제어 소켓·웹 정적 파일은 적지 않음). 메서드가 GET·HEAD·OPTIONS가 아니거나 템플릿이 `/vms/{key}/console`이면 요청 확장에 `AuditSlot`을 넣고 응답 뒤 항목을 만든다(`ip`는 `ClientIp`의 `to_canonical`, `path`는 쿼리 없는 `uri.path()`에서 `/api/v1`을 뗀 것). `Principal` 추출기가 `audit::note_auth`로 주체와 auth를 적는다 — `Authorization: Bearer `가 있으면 인증에 실패해도 늘 `token`. 핸들러는 추출기 `AuditNote`(실패 없음, 미들웨어 없으면 아무 일 안 함)의 `.detail(s)`·`.principal(p, auth)`로 남긴다: 로그인은 이름을 확인하기 전에 `username=<이름>`(이름 형식이 아니면 `username=<invalid>`)을 적어 401·429에도 있고 성공하면 그 사용자와 `session`, 로그아웃은 세션을 지우기 전에 그 사용자, 구독 `PUT`은 성공하면 `id=<키 id>`. 설계와 다른 점: 로그인 `detail`에 영역은 없다(영역은 계획 35).
- 읽기 API: `GET /api/v1/audit`(`audit::list_audit`, `AuditParams{since, until, user, node, action, limit}` `deny_unknown_fields`). 거절 순서 401 → 403(admin 아님, `ApiError::forbidden`) → 400 `invalid_request`(시각이 밀리초도 RFC 3339도 아님, `limit`이 1~5000 밖). `since > until`은 빈 목록. 설계와 다른 점: 위트니스는 API가 없어 감사 로그도 없으므로 `NodeSpec.witness`인 멤버는 묻지 않는다(`unreachable`에도 넣지 않음). 이름은 Node 리소스의 이름, 없으면 노드 id 문자열. 자기 노드는 `state.audit.read`(실패하면 `unreachable`), 다른 노드는 `Peers::read_audit`를 노드마다 5초 제한으로 동시에. 오류·제한 시간·`peers` 없음·`ReadAudit`이 없는 옛 opld(unimplemented)는 `unreachable`(정렬), 감사 로그를 끈 노드는 응답의 `disabled`(정렬 — 설계에 없음)다: 이 노드는 `AuditLog::is_enabled`로, 다른 노드는 `ReadAudit`의 `failed_precondition`으로 안다. 합친 뒤 `(time, id)` 내림차순, `limit`을 넘으면 자르고 `truncated`.
- 노드 사이 RPC: `build.rs`의 JSON 메서드 `ReadAudit`(`AuditQuery` → `opl_peer::AuditReply{entries}`), 트레이트 `opl_peer::AuditSource`(async_trait, `MigrationReceiver` 모양, `Result<Vec<AuditEntry>, String>`), `PeerService::with_audit(Arc<dyn AuditSource>)`(없으면 `Status::unimplemented`, `AuditSource::enabled`(기본 참)가 거짓이면 `Status::failed_precondition("audit log disabled")`, 받는 쪽이 `limit`을 5000으로 자름), `Peers::read_audit(addr, query, timeout)`(꺼진 노드면 `Ok(None)`). opld는 끈 노드에도 `AuditLog::disabled`를 넘긴다. `AuditLog`가 `AuditSource`를 구현한다.
- opld `[audit]`(`AuditSection`): 설계의 `max_file_size` 대신 `max_file_mib`(기본 16)이고 `enabled`(true)·`max_files`(8). `max_file_mib`가 0이거나 256을 넘거나(읽기가 파일을 통째로 메모리에 올림) `max_files`가 0이면 시작 오류, 모르는 키도 오류(`deny_unknown_fields`). `audit_config(data_dir, node)`가 `{data_dir}/audit`의 설정(꺼졌으면 None). `member.rs`는 감사 로그를 `PeerService`보다 먼저 열어(열지 못하면 시작 오류) `AppState.audit`과 `PeerService::with_audit`에 같은 `Arc<AuditLog>`를 넘기고, 종료 때 API를 멈춘 뒤 `shutdown`한다. opld의 tracing 출력은 stdout이 터미널일 때만 색 코드를 넣는다(journald로 가는 `opl_audit` 줄을 그대로 전달할 수 있게).
- oplctl `audit [--since] [--until] [--user] [--node] [--action] [--limit]`: 준 거르기만 쿼리에 넣고(없으면 서버 기본 24시간·200), `--since`의 `90s`·`30m`·`1h`·`7d`는 클라이언트가 지금에서 뺀 밀리초로, 그 밖(RFC 3339·밀리초)은 그대로 보내 서버가 푼다. 기간 `--since`와 `--until`을 함께 주면 보내지 않고 오류(`a relative --since cannot be combined with --until; give a time`). 표 `TIME`(UTC `YYYY-MM-DDTHH:MM:SSZ`)·`NODE`·`USER`·`IP`·`ACTION`·`TARGET`(`kind/key`, 없으면 `detail`, 둘 다 없으면 `-`)·`STATUS`, 표 뒤 stderr에 `warning: no audit records from: …`, `note: the audit log is disabled on: …`, `note: more entries match; narrow the range or raise --limit`, `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
- 웹: `web/src/audit/AuditPage.tsx`, 라우트 `/audit`, 메뉴 항목의 `admin?: boolean`(admin에게만 보임 — 다른 역할이 주소로 오면 읽지 않고 "관리자만" 안내), i18n `nav.audit`·`audit.*`. 범위는 `since`만 보내(읽을 때의 지금에서 뺀 밀리초) 새로고침이 창을 앞으로 민다. 사용자·동작은 적용을 눌러야, 다듬어 비어 있지 않을 때만 보낸다. 노드 거르기는 없다. 대상 열은 target이 없으면 detail, `disabled`는 정보 색 안내(`audit.disabled`), 데이터가 있는데 다시 읽기가 실패하면 표 위에 오류(`audit.refreshFailed`).
- 시험: opl-types 단위(`matches`의 범위 경계·사용자·action·path 부분 문자열), `opl_api::audit` 단위(쓰고 읽기, 0600·0700, 크기·개수 회전과 가장 오래된 것 지움, 새 것부터·거르기·`limit + 1`, 망가진 줄, 파일 순서로 멈춤, 비공개 `AuditLog::start(cfg, capacity, gate)`로 채운 채널 뒤 `audit.dropped`, disabled, `target`, 필드 512바이트 자르기, 인증 없는 항목의 IP별 한도와 `audit.suppressed`, 새 요청 없이 끝난 창의 수가 1초 틱에 적힘(61초 전 시계), 훑기는 끝난 창만, IPv6 /64, 1만 IP에서 가장 오래된 창 축출, 두 파일의 같은 `id` 한 번, `max_files` 넘는 옛 파일 지움, 0700·0600으로 맞추기와 링크 거절, 쓰기 스레드가 막힌 채 `shutdown`이 마감 뒤 돌아오고 런타임을 떨어뜨려도 끝남), `opl-api/tests/audit.rs`(시험 `Env`는 늘 `{dir}/audit`에 노드 "node1"·1 MiB·2개의 감사 로그: 주체·auth·템플릿·target·status, 403·401·틀린 토큰, 로그인 성공·실패·`<invalid>`와 비밀번호가 파일에 없음, 로그아웃, GET은 안 적고 콘솔 GET은 적음, 쿼리가 path에 없음, 구독 `id=`, `GET /audit`의 권한·시각·limit 경계·거르기·truncated, 감사 로그를 끈 이 노드는 `disabled`), `opl-peer/tests/grpc.rs`(`ReadAudit`·unimplemented·limit 자르기), `two_nodes.rs`(두 노드 합치기, 한 노드를 멈춘 뒤 `unreachable`, 감사 로그를 끈 다른 노드는 `unreachable`이 아니라 `disabled`), opld 설정 단위(`max_file_mib` 256·257), oplctl `fake_api`(`disabled` 안내, TARGET의 detail, 기간 `--since`와 `--until`을 함께 주면 오류), opld E2E `crates/opld/tests/audit.rs`(노드 셋 중 `[audit] enabled = false` 하나 — 노드마다 바꾸는 요청 뒤 한 노드의 `oplctl -o json audit`가 두 노드 기록을 함께 보이고 `unreachable` 없음, `--node`, 표, `audit.log` 0600, 꺼진 노드는 디렉터리 없고 `disabled`에 있음), 웹 Vitest(`AuditPage.test.tsx` — 다른 역할은 요청 없음, 데이터가 있을 때 다시 읽기 오류, `disabled` 안내, 대상 열 detail — 와 메뉴).
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
- 꺼진 노드의 감사 기록은 그 노드가 돌아와야 보이고, 디스크를 잃으면 사라진다(journald 전달로 보완). 루트 권한을 가진 사람은 파일을 고칠 수 있다(변조 방지 없음).
- 외부 사용자를 디렉터리에서 지워도 이미 있는 세션(최대 12시간)과 API 토큰은 남는다.
```

을 다음으로 바꾼다:

```markdown
- 꺼진 노드의 감사 기록은 그 노드가 돌아와야 보이고, 디스크를 잃으면 사라진다(journald 전달로 보완). 루트 권한을 가진 사람은 파일을 고칠 수 있다(변조 방지 없음).
- (계획 34) 롤링 업그레이드 중 `ReadAudit`이 없는 옛 노드는 `GET /audit`의 `unreachable`이다. opld가 갑자기 죽으면 대기열에 남은 항목을, 전원이 나가면 fsync 전 마지막 1초쯤을 잃을 수 있고, 대기열이 차서 버린 항목은 `audit.dropped`의 수만 남는다(journald에도 없음). 파일 쓰기가 실패하면 `warn!`만 남기고 요청은 그대로 처리한다. 감사 로그를 끈 노드의 요청은 어디에도 남지 않는다(그 노드는 `disabled`로 보인다). 인증 없는 요청은 IP마다 1분에 60개까지만 적고 나머지는 `audit.suppressed`의 수만 남는다. 필드는 512바이트까지만 적는다. 로그인의 사용자 이름 칸에 비밀번호를 잘못 넣으면 이름 형식이면 `username=`에 그대로 적힌다. 읽기는 파일 하나를 통째로 메모리에 올리므로 `max_file_mib`는 256까지다. 콘솔은 연 것만 적는다(닫은 때·입력 없음). 리버스 프록시 뒤에서는 `ip`가 프록시 주소다(계획 13과 같음). 보존은 크기·개수로만 하고 날짜로 지우지 않는다.
- 외부 사용자를 디렉터리에서 지워도 이미 있는 세션(최대 12시간)과 API 토큰은 남는다.
```

- [ ] **Step 2: 확인**

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 감사 로그 — README \"감사 로그\" 절(무엇을 적나, {data_dir}/audit/audit.log 위치·0600·회전, journald 대상 opl_audit와 NO_COLOR·rsyslog 전달 예, [audit] 설정, oplctl audit, GET /api/v1/audit, 웹, 한계)과 구독 절·웹 UI 목록 갱신, 엔터프라이즈 설계 §2.6 구현(계획 34)(action은 메서드와 라우트 템플릿, action 거르기는 경로도, 쓰기 전용 스레드, 위트니스는 묻지 않음, max_file_mib 등 설계와 다른 점)과 §9, 노드 코어 설계 §1.1에 하위 프로젝트 10(계획 34 완료, 35~38 남음), 계획 33 설계 §4.6에 감사 로그 메모"
```

---

## 완료 기준 (계획 34)

- 노드 둘 이상의 클러스터에서 각 노드에 바꾸는 요청을 하면, 어느 노드에서든 `oplctl audit`가 모든 노드의 기록을 시간 내림차순으로 보인다(사용자·IP·동작·대상·상태). 감사가 꺼진 노드는 `disabled`, 닿지 않은 노드는 `unreachable`로 알린다.
- 로그인 실패·403·401도 적히고, 비밀번호·토큰·쿼리 문자열은 감사 파일 어디에도 없다.
- `{data_dir}/audit/audit.log`는 0600이고 `max_file_mib`·`max_files`로 회전한다. 같은 줄이 tracing 대상 `opl_audit`로 journald에 간다.
- 웹 `/audit`는 admin에게만 보이고, 다른 역할은 API가 403이다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 `typecheck`·`lint`·`test`·`build`가 통과한다.

## 알고 받아들인 제한

- 꺼진 노드의 기록은 그 노드가 돌아와야 보이고, 디스크를 잃으면 사라진다. 루트 권한이 있으면 파일을 고칠 수 있다(변조 방지 없음).
- opld가 비정상 종료하면 채널에 있던 항목과 마지막 1초의 쓰기를 잃을 수 있다. 넘친 항목은 수로만 남는다.
- 인증 없는 요청은 IP(IPv6는 /64)마다 1분에 60개까지만 적고 나머지는 `audit.suppressed`의 수로 남긴다.
- 롤링 업그레이드 중의 옛 노드는 `unreachable`로 보인다. 프록시 뒤에서는 프록시의 IP가 적힌다.
- 그 밖의 제한은 설계 문서 §2.6·§9와 README "감사 로그" 절에 있다.

## 실행 후 기록 (2026-11-06)

- **Task 1~6:** `feat/plan34-audit`에서 계획대로 적용했다. 계획 문서의 편집 블록은 모두 그대로 맞았고, Task마다 검증한 스테이지 커밋과 트리가 같았다(생성 파일 제외).
- **스테이지에서 정한 것(설계와 다른 점, 설계 §2.6에 적음):**
  - `action`은 따로 표를 두지 않고 `<METHOD> <라우트 템플릿>`이다. VM 동작은 템플릿이 같으므로 `action` 거르기는 실제 경로도 본다.
  - 위트니스는 API가 없어 감사 로그도 없으므로 모아 읽을 때 묻지 않는다.
  - 쓰기는 tokio 태스크가 아니라 전용 스레드 `opl-audit`가 한다.
- **1차 리뷰에서 고친 것:**
  - 필드(경로·대상 키·detail·사용자) 512바이트 자르기. 인증 없이 아주 긴 경로로 기록을 밀어내는 것을 막는다.
  - 인증 없는 항목은 IP마다 1분에 60개까지만 적고 넘친 수는 `audit.suppressed`로 남긴다.
  - 읽는 중 회전으로 생기는 중복을 id로 없앤다. `max_files`를 줄였을 때 남는 파일을 지운다. 이미 있던 디렉터리·파일의 권한을 맞추고 `audit.log`를 `O_NOFOLLOW`로 연다.
  - `shutdown` 마감, 쓰기 스레드가 끝난 뒤의 기록에 경고.
  - opld 로그는 터미널일 때만 색을 쓴다. journald 줄에 이스케이프 코드가 있으면 README의 rsyslog 거르기가 맞지 않았다.
  - `max_file_mib` 상한 256(읽을 때 파일을 메모리에 올림).
  - 감사가 꺼진 노드를 `unreachable`이 아니라 응답의 `disabled`로 알린다(oplctl·웹 안내 포함).
  - oplctl: 상대 `--since`와 `--until`을 함께 주면 오류, TARGET 열이 비면 detail. 웹: 다시 읽기 실패 문구, 다른 역할은 요청하지 않음을 시험이 확인.
  - 계획 32·33 설계의 "감사 로그 없음" 문장에 메모.
- **2차 리뷰에서 고친 것:**
  - 넘친 수를 그 IP가 다시 오지 않아도 창이 끝나면 1초 안에 적는다(전에는 메모리에만 남을 수 있었다). IPv6는 /64로 묶는다. 표가 가득 찰 때 전체를 훑지 않게 순서 색인을 둔다.
  - `shutdown`의 마감이 막힌 디스크에서도 프로세스 종료를 막지 않게, 막히는 `send` 대신 `try_send`와 짧은 대기로 바꿨다.
- **불안정한 시험(부하 때, 다시 돌리면 통과):** opl-api `updates.rs` resuming_an_upgrade_that_never_started_starts_it(스테이지 검증에서 한 번).

**테스트:**
- macOS `cargo test --workspace` 1290개(Task 4 이후)
- 웹 시험 504개와 `typecheck`·`lint`·`build`
- `fmt`·`clippy -D warnings`(모든 기능)

Linux CI(프로브)에서 같은 시험과 패키지·linux-kvm 작업이 통과했다.

## 다음 계획으로 넘길 사항

- 계획 35(인증 영역과 LDAP): 새 리소스 `Realm`과 외부 사용자. 로그인 `detail`에 영역을 더한다(지금은 `username=…`만). 새 API는 미들웨어로 저절로 감사 로그에 남는다.
- 계획 36(OIDC), 37(원격 클러스터 — 중계한 바꾸는 요청은 `remote.proxy`로 남기기로 설계했지만 지금 action 규칙으로는 라우트 템플릿이 된다; 그때 detail로 원격 메서드·경로를 남긴다), 38(원격 복제)이 남았다.
- 감사 로그의 변조 방지, 날짜로 지우기, 콘솔 닫힘 기록은 넣지 않았다.
