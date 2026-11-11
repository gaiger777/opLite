//! systemd 알림(설계 §3.3). 유닛이 `Type=notify`·`WatchdogSec=30s`라 시작이 끝나면 `READY=1`을,
//! `core`·`api` 실행기가 빈 작업을 제때 돌리는 동안 `WATCHDOG=1`을 보낸다. 둘 중 하나가 20초 넘게
//! 막히면 쓰다듬기가 멈춰 systemd가 데몬을 끝내고(`WatchdogSignal=SIGKILL`) 다시 띄운다
//! (`KillMode=process`라 VM은 그대로 돈다). `work`가 늦으면 경고만 남긴다: 데몬을 다시 띄우는 동안
//! HA watchdog 장치의 제한 시간(30초)을 넘기면 노드가 재부팅되므로, 리스와 API가 도는 한 끄지 않는다.
//!
//! `$NOTIFY_SOCKET`(유닉스 데이터그램, `@`로 시작하면 Linux 추상 소켓)에 직접 쓴다. 변수가 없으면
//! (시험·macOS·systemd 밖) 아무것도 보내지 않는다.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::runtime::Handles;

/// 실행기 확인 간격(`WATCHDOG_USEC`이 없을 때)
const CHECK_INTERVAL: Duration = Duration::from_secs(10);
/// 빈 작업이 이 안에 돌아와야 그 회차에 쓰다듬는다(`WATCHDOG_USEC`이 없을 때).
const CHECK_DEADLINE: Duration = Duration::from_secs(2);

/// systemd가 준 `WATCHDOG_USEC`(마이크로초)에 맞춘 (확인 간격, 마감). 간격은 그 1/3(1초~10초),
/// 마감은 2초와 간격의 절반 중 짧은 것이다. 없거나 읽지 못하면 10초·2초다.
fn timing(watchdog_usec: Option<&str>) -> (Duration, Duration) {
    match watchdog_usec.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(usec) => {
            let interval = (Duration::from_micros(usec) / 3)
                .clamp(Duration::from_secs(1), Duration::from_secs(10));
            (interval, CHECK_DEADLINE.min(interval / 2))
        }
        None => (CHECK_INTERVAL, CHECK_DEADLINE),
    }
}

/// `$NOTIFY_SOCKET`으로 상태를 보낸다.
#[derive(Clone)]
pub struct Notifier {
    addr: Option<SocketAddr>,
}

impl Notifier {
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("NOTIFY_SOCKET").as_deref())
    }

    fn new(socket: Option<&OsStr>) -> Self {
        let addr = socket
            .filter(|s| !s.is_empty())
            .and_then(|s| match socket_addr(s) {
                Ok(addr) => Some(addr),
                Err(e) => {
                    tracing::warn!(error = %e, socket = ?s, "cannot use NOTIFY_SOCKET");
                    None
                }
            });
        Self { addr }
    }

    /// `state`(예: `READY=1`)를 보낸다. 실패하면 경고만 남긴다.
    pub fn notify(&self, state: &str) {
        let Some(addr) = &self.addr else { return };
        let sent = UnixDatagram::unbound().and_then(|s| s.send_to_addr(state.as_bytes(), addr));
        if let Err(e) = sent {
            tracing::warn!(error = %e, state, "cannot notify systemd");
        }
    }
}

fn socket_addr(socket: &OsStr) -> io::Result<SocketAddr> {
    match socket.as_bytes() {
        [b'@', name @ ..] => abstract_addr(name),
        _ => SocketAddr::from_pathname(socket),
    }
}

#[cfg(target_os = "linux")]
fn abstract_addr(name: &[u8]) -> io::Result<SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    SocketAddr::from_abstract_name(name)
}

#[cfg(not(target_os = "linux"))]
fn abstract_addr(_name: &[u8]) -> io::Result<SocketAddr> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "abstract sockets are Linux-only",
    ))
}

/// 실행기 확인 스레드(`orbivisor-notify`). 놓으면 멈춘다.
pub struct Liveness {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    /// 마지막 회차에 늦은 실행기
    slow: Arc<Mutex<Vec<&'static str>>>,
}

impl Liveness {
    /// 바로 한 번, 그 뒤 10초(`WATCHDOG_USEC`의 1/3)마다 세 실행기를 확인하고 `core`·`api`가 2초 안에
    /// 답한 회차에 `WATCHDOG=1`을 보낸다.
    pub fn spawn(rt: Handles, notifier: Notifier) -> Self {
        let (interval, deadline) = timing(std::env::var("WATCHDOG_USEC").ok().as_deref());
        Self::start(rt, notifier, interval, deadline)
    }

    fn start(rt: Handles, notifier: Notifier, interval: Duration, deadline: Duration) -> Self {
        let (stop, rx) = mpsc::channel();
        let last = Arc::new(Mutex::new(Vec::new()));
        let slow_out = last.clone();
        let thread = std::thread::Builder::new()
            .name("orbivisor-notify".into())
            .spawn(move || {
                loop {
                    let slow = check(&rt, deadline);
                    *last.lock().unwrap() = slow.clone();
                    if slow.iter().all(|name| *name == "work") {
                        notifier.notify("WATCHDOG=1");
                    }
                    if slow == ["work"] {
                        tracing::warn!(
                            runtimes = ?slow,
                            "runtimes did not run a task within {deadline:?}; still petting the systemd watchdog"
                        );
                    } else if !slow.is_empty() {
                        tracing::warn!(
                            runtimes = ?slow,
                            "runtimes did not run a task within {deadline:?}; not petting the systemd watchdog"
                        );
                    }
                    if !matches!(rx.recv_timeout(interval), Err(RecvTimeoutError::Timeout)) {
                        return;
                    }
                }
            })
            .expect("cannot start the notify thread");
        Self {
            stop: Some(stop),
            thread: Some(thread),
            slow: slow_out,
        }
    }

    /// 마지막 회차에 마감 안에 답하지 않은 실행기(`core`·`api`·`work`). 계획 46의 헬스가 읽는다.
    pub fn slow(&self) -> Vec<&'static str> {
        self.slow.lock().unwrap().clone()
    }
}

/// 마지막 회차에 늦은 실행기를 보인다.
impl std::fmt::Debug for Liveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Liveness")
            .field("slow", &self.slow())
            .finish_non_exhaustive()
    }
}

impl Drop for Liveness {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 세 실행기에 빈 작업을 보내 `deadline` 안에 돌리지 못한 실행기의 이름
fn check(rt: &Handles, deadline: Duration) -> Vec<&'static str> {
    let (tx, rx) = mpsc::channel();
    let runtimes = [("core", &rt.core), ("api", &rt.api), ("work", &rt.work)];
    for (name, handle) in runtimes {
        let tx = tx.clone();
        // 끝난 실행기면 작업이 버려지고 `tx`가 닫힌다.
        handle.spawn(async move {
            let _ = tx.send(name);
        });
    }
    drop(tx);
    let mut waiting: Vec<_> = runtimes.iter().map(|(name, _)| *name).collect();
    let until = Instant::now() + deadline;
    while !waiting.is_empty() {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(name) => waiting.retain(|n| *n != name),
            Err(_) => break,
        }
    }
    waiting
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::runtime::Runtimes;

    /// 받은 데이터그램 하나. 시간 안에 없으면 `None`.
    fn recv(socket: &UnixDatagram, timeout: Duration) -> Option<String> {
        socket.set_read_timeout(Some(timeout)).unwrap();
        let mut buf = [0u8; 256];
        let n = socket.recv(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// `want`이 올 때까지 받는다.
    fn wait_for(socket: &UnixDatagram, want: &str) {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if recv(socket, Duration::from_millis(500)).as_deref() == Some(want) {
                return;
            }
        }
        panic!("{want} did not arrive");
    }

    #[test]
    fn notifies_the_socket_path_and_does_nothing_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let socket = UnixDatagram::bind(&path).unwrap();
        Notifier::new(Some(path.as_os_str())).notify("READY=1");
        assert_eq!(
            recv(&socket, Duration::from_secs(1)).as_deref(),
            Some("READY=1")
        );
        // 변수가 없거나 비었거나 받는 쪽이 없으면 보내지 않는다(실패해도 돌아온다).
        for none in [None, Some(OsStr::new(""))] {
            assert!(Notifier::new(none).addr.is_none());
        }
        Notifier::new(Some(dir.path().join("missing").as_os_str())).notify("READY=1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn notifies_an_abstract_socket() {
        use std::os::linux::net::SocketAddrExt;
        let name = format!("orbivisor-notify-test-{}", uuid::Uuid::now_v7());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let socket = UnixDatagram::bind_addr(&addr).unwrap();
        Notifier::new(Some(OsStr::new(&format!("@{name}")))).notify("READY=1");
        assert_eq!(
            recv(&socket, Duration::from_secs(1)).as_deref(),
            Some("READY=1")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn abstract_sockets_are_ignored_off_linux() {
        assert!(Notifier::new(Some(OsStr::new("@x"))).addr.is_none());
    }

    /// `handle` 실행기의 작업자 `workers`개를 모두 `how_long` 동안 동기 sleep으로 막는다.
    fn block(handle: &tokio::runtime::Handle, workers: usize, how_long: Duration) {
        let barrier = Arc::new(Barrier::new(workers + 1));
        for _ in 0..workers {
            let barrier = barrier.clone();
            handle.spawn(async move {
                barrier.wait();
                std::thread::sleep(how_long);
            });
        }
        barrier.wait();
    }

    /// 실행기가 답하면 쓰다듬고, `api`나 `core` 실행기의 모든 스레드가 막히면 멈췄다가 풀리면 다시
    /// 쓰다듬는다.
    #[test]
    fn pets_only_while_core_and_api_run_tasks() {
        let rt = Runtimes::new(2, 2).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let socket = UnixDatagram::bind(&path).unwrap();
        let notifier = Notifier::new(Some(path.as_os_str()));
        let liveness = Liveness::start(
            rt.handles(),
            notifier,
            Duration::from_millis(100),
            Duration::from_millis(300),
        );
        wait_for(&socket, "WATCHDOG=1");

        for (name, handle) in [("api", rt.api.handle()), ("core", rt.core.handle())] {
            block(handle, 2, Duration::from_secs(3));
            // 막기 전에 시작한 회차가 끝나기를 기다리고 그때까지 온 것은 버린다.
            std::thread::sleep(Duration::from_millis(400));
            while recv(&socket, Duration::from_millis(1)).is_some() {}
            assert_eq!(recv(&socket, Duration::from_secs(1)), None, "{name}");
            assert_eq!(check(&rt.handles(), Duration::from_millis(500)), [name]);
            assert!(liveness.slow().contains(&name), "{name}");

            wait_for(&socket, "WATCHDOG=1");
        }
        drop(liveness);
    }

    /// `work`만 막히면 계속 쓰다듬고 마지막 회차에 늦은 실행기로 남긴다.
    #[test]
    fn keeps_petting_while_only_work_is_blocked() {
        let rt = Runtimes::new(2, 2).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let socket = UnixDatagram::bind(&path).unwrap();
        let notifier = Notifier::new(Some(path.as_os_str()));
        let liveness = Liveness::start(
            rt.handles(),
            notifier,
            Duration::from_millis(100),
            Duration::from_millis(300),
        );
        wait_for(&socket, "WATCHDOG=1");
        assert_eq!(liveness.slow(), Vec::<&str>::new());

        block(rt.work.handle(), 2, Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(400));
        while recv(&socket, Duration::from_millis(1)).is_some() {}
        wait_for(&socket, "WATCHDOG=1");
        assert_eq!(liveness.slow(), ["work"]);
        drop(liveness);
    }

    /// `WATCHDOG_USEC`의 1/3마다(1초~10초) 확인하고, 마감은 2초와 그 절반 중 짧은 것이다.
    #[test]
    fn timing_follows_watchdog_usec() {
        let secs = Duration::from_secs;
        let ms = Duration::from_millis;
        assert_eq!(timing(None), (secs(10), secs(2)));
        assert_eq!(timing(Some("x")), (secs(10), secs(2)));
        assert_eq!(timing(Some("30000000")), (secs(10), secs(2)));
        assert_eq!(timing(Some("120000000")), (secs(10), secs(2)));
        assert_eq!(timing(Some("6000000")), (secs(2), secs(1)));
        assert_eq!(timing(Some("4500000")), (ms(1500), ms(750)));
        assert_eq!(timing(Some("1000000")), (secs(1), ms(500)));
    }
}
