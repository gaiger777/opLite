//! 노드의 경보 장부(설계 §8·§9): 이 노드가 평가하는 노드 경보의 상태(`{data_dir}/alerts/state.json`)와 이 노드가 보낸
//! 알림 기록(`alerts/history.log`, JSON Lines, 4 MiB에서 한 번 돌림)이다. 다른 노드는 `ReadAlerts`로 읽는다.
//! 상태 파일은 임시 파일 → fsync → rename → 디렉터리 fsync로 바꾸므로 opld가 죽어도 반쯤 쓴 파일이 남지 않는다.
//! 파일 IO는 막히는 호출이다(async 쪽은 `spawn_blocking`에서 부른다).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orbivisor_peer::AlertsSource;
use orbivisor_types::alert::{
    ActiveAlert, AlertState, AlertsQuery, AlertsReply, HistoryWindow, NotificationRecord,
};
use serde::{Deserialize, Serialize};

/// `{data_dir}` 아래 장부 디렉터리
pub const DIR: &str = "alerts";
const STATE_FILE: &str = "state.json";
const HISTORY_FILE: &str = "history.log";
const HISTORY_OLD: &str = "history.log.1";
/// 기록 파일이 이보다 커지면 `history.log.1`로 밀고(옛 것은 지움) 새로 쓴다.
pub(crate) const HISTORY_ROTATE_BYTES: u64 = 4 << 20;
/// 노드마다 따라가는 활성 경보의 상한
pub const MAX_ACTIVE: usize = 500;
/// 보낸 이벤트(같은 Task로 두 번 보내지 않으려는 키)를 이만큼 기억한다(끝난 Task를 지우는 기본 기간과 같다).
pub(crate) const EVENT_KEEP_MS: i64 = 7 * 24 * 3600 * 1000;

/// 노드 경보의 상태(`state.json`)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct NodeAlertState {
    /// 처리한 watch 리비전. 다시 시작하면 그 뒤부터 이어 본다.
    #[serde(default)]
    pub revision: u64,
    /// 이 노드가 평가하는 활성 경보
    #[serde(default)]
    pub active: Vec<ActiveAlert>,
    /// 보낸 이벤트 키(`{rule}/task/{id}`)와 보낸 때
    #[serde(default)]
    pub events: BTreeMap<String, i64>,
    /// 활성 경보가 상한을 넘어 더 생긴 경보를 따라가지 못했다.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overflow: bool,
}

impl NodeAlertState {
    /// 파일에 다시 쓸 만큼 바뀌었는지. 지표 값(`value`)과 리비전만 바뀐 것은 쓰지 않는다(5초마다 쓰지 않게).
    pub fn durable_eq(&self, other: &Self) -> bool {
        let strip = |a: &ActiveAlert| ActiveAlert {
            value: None,
            ..a.clone()
        };
        self.events == other.events
            && self.overflow == other.overflow
            && self.active.len() == other.active.len()
            && self
                .active
                .iter()
                .zip(&other.active)
                .all(|(a, b)| strip(a) == strip(b))
    }
}

/// 이 노드의 경보 장부. 복제하면 같은 장부를 가리킨다.
#[derive(Clone)]
pub struct AlertBook(Arc<Inner>);

struct Inner {
    /// 기록의 `node`에 넣을 이 노드 이름
    node: String,
    /// 없으면 메모리에만 둔다(시험).
    dir: Option<PathBuf>,
    state: Mutex<NodeAlertState>,
    /// 기록 파일 쓰기·돌리기를 하나씩. 메모리 장부는 여기에 기록을 둔다.
    history: Mutex<Vec<NotificationRecord>>,
}

impl std::fmt::Debug for AlertBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlertBook")
            .field("node", &self.0.node)
            .field("dir", &self.0.dir)
            .finish_non_exhaustive()
    }
}

impl AlertBook {
    /// 메모리에만 두는 장부(시험)
    pub fn memory(node: impl Into<String>) -> Self {
        Self(Arc::new(Inner {
            node: node.into(),
            dir: None,
            state: Mutex::default(),
            history: Mutex::default(),
        }))
    }

    /// `dir`(없으면 0700으로 만든다)의 상태 파일을 읽는다. 상태 파일이 깨졌으면 `state.json.corrupt`로 옮기고(경고)
    /// 빈 상태로 시작한다. 막히는 호출이다.
    pub fn open(dir: impl Into<PathBuf>, node: impl Into<String>) -> io::Result<Self> {
        let dir = dir.into();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let path = dir.join(STATE_FILE);
        let state = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<NodeAlertState>(&bytes) {
                Ok(s) => s,
                Err(e) => {
                    let aside = dir.join(format!("{STATE_FILE}.corrupt"));
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "the alert state file is broken; moving it aside and starting empty"
                    );
                    std::fs::rename(&path, &aside)?;
                    NodeAlertState::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => NodeAlertState::default(),
            Err(e) => return Err(e),
        };
        Ok(Self(Arc::new(Inner {
            node: node.into(),
            dir: Some(dir),
            state: Mutex::new(state),
            history: Mutex::default(),
        })))
    }

    /// 이 노드 이름
    pub fn node(&self) -> &str {
        &self.0.node
    }

    pub(crate) fn snapshot(&self) -> NodeAlertState {
        self.0.state.lock().unwrap().clone()
    }

    /// 새 상태를 둔다. `persist`면 먼저 파일에 쓴다(쓰지 못하면 메모리도 바꾸지 않는다). 막히는 호출이다.
    pub(crate) fn store(&self, new: NodeAlertState, persist: bool) -> io::Result<()> {
        if persist && let Some(dir) = &self.0.dir {
            write_atomic(&dir.join(STATE_FILE), &serde_json::to_vec(&new)?)?;
        }
        *self.0.state.lock().unwrap() = new;
        Ok(())
    }

    /// 활성 노드 경보
    pub fn active(&self) -> Vec<ActiveAlert> {
        self.0.state.lock().unwrap().active.clone()
    }

    /// `firing`인 노드 경보
    pub(crate) fn firing(&self) -> Vec<ActiveAlert> {
        self.0
            .state
            .lock()
            .unwrap()
            .active
            .iter()
            .filter(|a| a.state == AlertState::Firing)
            .cloned()
            .collect()
    }

    /// 보낸 알림 하나를 기록에 덧붙인다(`node`는 이 노드 이름으로 채운다). 실패는 경고만 한다. 막히는 호출이다.
    pub fn record(&self, mut rec: NotificationRecord) {
        rec.node = self.0.node.clone();
        let mut memory = self.0.history.lock().unwrap();
        let Some(dir) = &self.0.dir else {
            memory.push(rec);
            return;
        };
        if let Err(e) = append(dir, &rec) {
            tracing::warn!(error = %e, "cannot write the notification history");
        }
    }

    /// 창에 든 기록을 새 것부터 `limit + 1`개까지 읽는다. 막히는 호출이다.
    pub fn read_history(&self, w: HistoryWindow) -> io::Result<Vec<NotificationRecord>> {
        let mut out: Vec<NotificationRecord> = {
            let memory = self.0.history.lock().unwrap();
            match &self.0.dir {
                None => memory.clone(),
                Some(dir) => {
                    let mut all = read_lines(&dir.join(HISTORY_OLD))?;
                    all.extend(read_lines(&dir.join(HISTORY_FILE))?);
                    all
                }
            }
        };
        out.retain(|r| r.at >= w.since && r.at <= w.until);
        // 같은 시각은 파일 순서(뒤에 쓴 것이 먼저)
        out.reverse();
        out.sort_by_key(|r| std::cmp::Reverse(r.at));
        out.truncate(w.limit as usize + 1);
        Ok(out)
    }
}

#[async_trait]
impl AlertsSource for AlertBook {
    async fn read(&self, query: AlertsQuery) -> Result<AlertsReply, String> {
        let (active, overflow) = {
            let s = self.0.state.lock().unwrap();
            (s.active.clone(), s.overflow)
        };
        let history = match query.history {
            None => Vec::new(),
            // 파일을 읽으므로 막히는 일을 하는 스레드에서 한다.
            Some(w) => {
                let book = self.clone();
                tokio::task::spawn_blocking(move || book.read_history(w))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?
            }
        };
        Ok(AlertsReply {
            active,
            history,
            overflow,
        })
    }
}

/// 기록 한 줄을 덧붙인다. 파일이 [`HISTORY_ROTATE_BYTES`]를 넘었으면 먼저 `history.log.1`로 민다.
fn append(dir: &Path, rec: &NotificationRecord) -> io::Result<()> {
    let path = dir.join(HISTORY_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= HISTORY_ROTATE_BYTES) {
        std::fs::rename(&path, dir.join(HISTORY_OLD))?;
    }
    let mut line = serde_json::to_vec(rec)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&line)?;
    file.sync_data()
}

/// 기록 파일의 줄들. 없으면 비고, 풀지 못한 줄(끊긴 마지막 줄)은 건너뛴다.
fn read_lines(path: &Path) -> io::Result<Vec<NotificationRecord>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        if let Ok(rec) = serde_json::from_str(&line?) {
            out.push(rec);
        }
    }
    Ok(out)
}

/// 임시 파일에 쓰고 fsync → rename → 디렉터리 fsync
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbivisor_types::alert::{AlertSubject, NotificationState, SubjectKind};
    use uuid::Uuid;

    fn alert(value: f64) -> ActiveAlert {
        ActiveAlert {
            rule: Uuid::nil(),
            subject: AlertSubject {
                kind: SubjectKind::Node,
                id: Uuid::nil(),
                name: "node1".into(),
                node: Some("node1".into()),
                port: None,
            },
            state: AlertState::Firing,
            since: 1,
            value: Some(value),
            last_sent: Some(1),
            clear_since: None,
        }
    }

    fn record(at: i64) -> NotificationRecord {
        NotificationRecord {
            at,
            node: String::new(),
            rule: Some("cpu-hot".into()),
            subject: Some("node1".into()),
            state: NotificationState::Firing,
            target: "ops".into(),
            ok: true,
            error: None,
        }
    }

    #[test]
    fn state_survives_reopening_and_only_durable_changes_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIR);
        let book = AlertBook::open(&path, "node1").unwrap();
        assert_eq!(book.snapshot(), NodeAlertState::default());
        let mut s = NodeAlertState {
            revision: 42,
            active: vec![alert(91.0)],
            ..Default::default()
        };
        s.events.insert("r/task/t".into(), 5);
        book.store(s.clone(), true).unwrap();
        let mode = std::fs::metadata(path.join(STATE_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let reopened = AlertBook::open(&path, "node1").unwrap();
        assert_eq!(reopened.snapshot(), s);
        // 값과 리비전만 바뀐 것은 다시 쓸 일이 아니다.
        let mut t = s.clone();
        t.active[0].value = Some(99.0);
        t.revision = 50;
        assert!(s.durable_eq(&t));
        t.active[0].last_sent = Some(2);
        assert!(!s.durable_eq(&t));
    }

    #[test]
    fn a_broken_state_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), b"{not json").unwrap();
        let book = AlertBook::open(dir.path(), "node1").unwrap();
        assert_eq!(book.snapshot(), NodeAlertState::default());
        assert!(dir.path().join("state.json.corrupt").exists());
    }

    #[test]
    fn history_is_read_newest_first_within_the_window_and_rotates_once() {
        let dir = tempfile::tempdir().unwrap();
        let book = AlertBook::open(dir.path(), "node7").unwrap();
        for at in 1..=5 {
            book.record(record(at));
        }
        let w = |since, until, limit| HistoryWindow {
            since,
            until,
            limit,
        };
        let got = book.read_history(w(2, 4, 10)).unwrap();
        assert_eq!(got.iter().map(|r| r.at).collect::<Vec<_>>(), [4, 3, 2]);
        assert!(got.iter().all(|r| r.node == "node7"));
        // limit + 1개까지(잘렸는지 알게)
        assert_eq!(book.read_history(w(0, 10, 1)).unwrap().len(), 2);
        // 크기를 넘으면 한 번 밀고 옛 것과 함께 읽는다.
        let log = dir.path().join(HISTORY_FILE);
        let f = OpenOptions::new().append(true).open(&log).unwrap();
        f.set_len(HISTORY_ROTATE_BYTES).unwrap();
        book.record(record(6));
        assert!(dir.path().join(HISTORY_OLD).exists());
        let got = book.read_history(w(0, 10, 10)).unwrap();
        assert_eq!(
            got.iter().map(|r| r.at).collect::<Vec<_>>(),
            [6, 5, 4, 3, 2, 1]
        );
    }

    #[tokio::test]
    async fn peers_read_active_alerts_and_history() {
        let book = AlertBook::memory("node1");
        book.store(
            NodeAlertState {
                active: vec![alert(1.0)],
                overflow: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        book.record(record(10));
        let reply = AlertsSource::read(&book, AlertsQuery::default())
            .await
            .unwrap();
        assert_eq!(reply.active.len(), 1);
        assert!(reply.overflow);
        assert!(reply.history.is_empty());
        let reply = AlertsSource::read(
            &book,
            AlertsQuery {
                history: Some(HistoryWindow {
                    since: 0,
                    until: 100,
                    limit: 10,
                }),
            },
        )
        .await
        .unwrap();
        assert_eq!(reply.history.len(), 1);
    }
}
