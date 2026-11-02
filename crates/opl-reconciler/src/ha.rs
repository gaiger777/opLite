//! 노드 쪽 HA 펜싱(`HaGuard`). 리더에게 리스를 갱신하고, 리스가 살아 있는지(`fresh`)와 끝났는지
//! (`stale`)를 알려 주고, watchdog 장치를 쓰다듬는다. 규칙은 설계 문서
//! (2026-11-02-ha-fencing-witness-design.md) §3.2·§3.3을 따른다.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::{Cluster, ClusterError};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;

/// watchdog를 쓰다듬는 간격
const PING_INTERVAL: Duration = Duration::from_secs(1);

/// 리더에게 이 노드의 리스를 갱신한다. 시험은 가짜를 쓴다.
#[async_trait]
pub trait LeaseRenewer: Send + Sync {
    /// `timeout` 안에 리더가 갱신을 확인하고 그 인덱스까지 이 노드에 적용되면 성공이다.
    async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError>;
}

#[async_trait]
impl LeaseRenewer for Cluster {
    async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
        Cluster::renew_lease(self, timeout).await
    }
}

/// 연 watchdog 장치. 쓰다듬지 않고 제한 시간이 지나면 노드가 재시작된다.
pub trait Watchdog: Send {
    fn ping(&mut self) -> io::Result<()>;
    /// `V`를 쓰고 닫아 타이머를 멈춘다(magic close). 커널이 `nowayout`이면 멈추지 않는다.
    fn magic_close(self: Box<Self>) -> io::Result<()>;
    /// 장치가 제한 시간을 받아들이지 않았으면 그 이유. 그런 장치도 닫지 않고 쥐어 평소처럼 쓰다듬지만
    /// HA VM에는 쓰지 않는다(`arm` 실패).
    fn rejected(&self) -> Option<&str> {
        None
    }
}

/// watchdog 장치를 연다. 열면 장치의 타이머가 (다시) 돈다.
pub trait WatchdogOpener: Send + Sync {
    fn open(&self) -> io::Result<Box<dyn Watchdog>>;
}

/// Linux watchdog 장치(`/dev/watchdog`, 하드웨어 또는 `softdog`). 다른 OS에서는 열기가 실패한다.
#[derive(Debug, Clone)]
pub struct DeviceWatchdog {
    pub path: PathBuf,
    /// 장치에 정하는 제한 시간(초, `WDIOC_SETTIMEOUT`)
    pub timeout_secs: u64,
}

/// `WDIOC_SETTIMEOUT` = `_IOWR('W', 6, int)`
const WDIOC_SETTIMEOUT: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read_write::<std::ffi::c_int>(b'W', 6);
/// `WDIOC_GETTIMEOUT` = `_IOR('W', 7, int)`
const WDIOC_GETTIMEOUT: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read::<std::ffi::c_int>(b'W', 7);

impl WatchdogOpener for DeviceWatchdog {
    fn open(&self) -> io::Result<Box<dyn Watchdog>> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "watchdog devices need Linux",
            ));
        }
        let mut timeout = std::ffi::c_int::try_from(self.timeout_secs).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "watchdog timeout too large")
        })?;
        let fd = rustix::fs::open(
            &self.path,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        // SAFETY: WDIOC_SETTIMEOUT는 int 하나를 읽고, 장치가 실제로 정한 값을 그 자리에 쓴다.
        let set = unsafe {
            rustix::ioctl::ioctl(
                &fd,
                rustix::ioctl::Updater::<WDIOC_SETTIMEOUT, std::ffi::c_int>::new(&mut timeout),
            )
        }
        .map(|()| timeout)
        .map_err(io::Error::from);
        // SAFETY: WDIOC_GETTIMEOUT는 지금 제한 시간을 int 하나에 쓴다.
        let get = || unsafe {
            rustix::ioctl::ioctl(
                &fd,
                rustix::ioctl::Getter::<WDIOC_GETTIMEOUT, std::ffi::c_int>::new(),
            )
            .map_err(io::Error::from)
        };
        // 받아들이지 못해도 닫지 않고 쥐어 둔다. 열면 타이머가 돌므로 그냥 닫으면 나중에 노드가 재시작되고,
        // 열기는 리스가 살아 있을 때만 하므로 쥔 채 쓰다듬고 정상 종료 때 멈춰도 된다(`HaGuard`).
        let actual = match accept_timeout(self.timeout_secs, set, get) {
            Ok(actual) => actual,
            Err(e) => {
                let reason = format!(
                    "cannot set the watchdog timeout to {}s: {e}",
                    self.timeout_secs
                );
                return Ok(Box::new(Device {
                    fd,
                    rejected: Some(reason),
                }));
            }
        };
        if actual != self.timeout_secs {
            tracing::warn!(
                device = %self.path.display(),
                requested = self.timeout_secs,
                actual,
                "watchdog device chose a shorter timeout"
            );
        }
        Ok(Box::new(Device { fd, rejected: None }))
    }
}

/// 장치의 제한 시간을 받아들일지 정한다. `set`은 `WDIOC_SETTIMEOUT`의 결과(장치가 정한 값)이고,
/// 실패했으면 `get`(`WDIOC_GETTIMEOUT`)으로 지금 값을 읽는다. 요청보다 길면 리스가 끝난 뒤에도 노드가
/// 늦게 재시작되어 리더의 복구와 겹칠 수 있으므로 받아들이지 않는다.
fn accept_timeout(
    requested: u64,
    set: io::Result<std::ffi::c_int>,
    get: impl FnOnce() -> io::Result<std::ffi::c_int>,
) -> io::Result<u64> {
    let ok = |t: std::ffi::c_int| {
        u64::try_from(t)
            .ok()
            .filter(|t| (1..=requested).contains(t))
    };
    match set {
        Ok(t) => ok(t).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the device chose {t}s"),
            )
        }),
        Err(e) => get().ok().and_then(ok).ok_or(e),
    }
}

struct Device {
    fd: rustix::fd::OwnedFd,
    /// 제한 시간을 받아들이지 않은 이유
    rejected: Option<String>,
}

impl Watchdog for Device {
    fn ping(&mut self) -> io::Result<()> {
        rustix::io::write(&self.fd, b"\0")?;
        Ok(())
    }

    fn magic_close(self: Box<Self>) -> io::Result<()> {
        rustix::io::write(&self.fd, b"V")?;
        Ok(())
    }

    fn rejected(&self) -> Option<&str> {
        self.rejected.as_deref()
    }
}

/// `HaGuard` 설정. opld가 `[ha]`에서 만든다.
#[derive(Clone)]
pub struct HaConfig {
    /// 마지막으로 성공한 갱신(보내기 전 시각) 뒤 이만큼 지나면 리스가 끝난다.
    pub fence_after: Duration,
    /// 갱신 간격. 한 번의 갱신은 이 두 배까지 기다린다.
    pub renew_interval: Duration,
    /// watchdog 장치. None이면 `process` 펜싱(워커가 끄는 것만)이다.
    pub watchdog: Option<Arc<dyn WatchdogOpener>>,
}

/// 리스 상태. 바뀌면 [`HaGuard::subscribe`]로 알린다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseState {
    /// 이 프로세스에서 갱신에 성공했고 마지막 갱신 뒤 `fence_after`가 지나지 않았다.
    pub fresh: bool,
    /// 시작이나 마지막 갱신 중 늦은 것에서 `fence_after`가 지났다. HA VM을 꺼야 한다.
    pub stale: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ArmError {
    #[error("the HA lease is not fresh")]
    NotFresh,
    #[error("cannot open the watchdog device: {0}")]
    Open(io::Error),
    #[error("the watchdog device is unusable: {0}")]
    Rejected(String),
}

struct State {
    /// 마지막으로 성공한 갱신을 보내기 전 시각
    last_renewed: Option<Instant>,
    /// 이 노드에서 도는 HA VM
    running: HashSet<Uuid>,
    watchdog: Option<Box<dyn Watchdog>>,
}

struct Shared {
    /// 기준점. 다시 시작한 opld가 갱신하기 전에 곧바로 끄지 않게 한다.
    start: Instant,
    fence_after: Duration,
    state: Mutex<State>,
    tx: watch::Sender<LeaseState>,
    /// 갱신에 성공하면 알린다(상태가 바뀔 다음 시각이 늦춰진다).
    renewed: Notify,
}

impl Shared {
    fn lease(&self, state: &State, now: Instant) -> LeaseState {
        let fresh = state
            .last_renewed
            .is_some_and(|s| now.saturating_duration_since(s) < self.fence_after);
        let base = state.last_renewed.map_or(self.start, |s| s.max(self.start));
        LeaseState {
            fresh,
            stale: now.saturating_duration_since(base) >= self.fence_after,
        }
    }

    /// 지금 상태를 알리고, 갱신 없이 상태가 바뀔 다음 시각을 돌려준다. 이미 stale이면 None이다.
    fn publish(&self) -> Option<Instant> {
        let state = self.state.lock().unwrap();
        let now = Instant::now();
        let lease = self.lease(&state, now);
        self.tx.send_if_modified(|old| {
            let changed = *old != lease;
            *old = lease;
            changed
        });
        // 갱신했으면 마지막 갱신이 기준점보다 늦으므로 fresh와 stale이 같은 때 바뀐다.
        let next = state.last_renewed.map_or(self.start, |s| s.max(self.start)) + self.fence_after;
        (next > now).then_some(next)
    }
}

/// 노드의 HA 리스와 watchdog. 갱신 루프와 쓰다듬기 루프를 뒤에서 돌린다. 정상 종료 때 [`close`]한다.
///
/// [`close`]: HaGuard::close
pub struct HaGuard {
    shared: Arc<Shared>,
    opener: Option<Arc<dyn WatchdogOpener>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl HaGuard {
    /// 지금을 기준점으로 삼고 곧바로 첫 갱신을 보낸다.
    pub fn spawn(renewer: Arc<dyn LeaseRenewer>, config: HaConfig) -> Arc<Self> {
        let (tx, _) = watch::channel(LeaseState::default());
        let shared = Arc::new(Shared {
            start: Instant::now(),
            fence_after: config.fence_after,
            state: Mutex::new(State {
                last_renewed: None,
                running: HashSet::new(),
                watchdog: None,
            }),
            tx,
            renewed: Notify::new(),
        });
        let tasks = vec![
            tokio::spawn(renew_loop(shared.clone(), renewer, config.renew_interval)),
            tokio::spawn(watch_loop(shared.clone())),
        ];
        Arc::new(Self {
            shared,
            opener: config.watchdog,
            tasks: Mutex::new(tasks),
        })
    }

    pub fn state(&self) -> LeaseState {
        let state = self.shared.state.lock().unwrap();
        self.shared.lease(&state, Instant::now())
    }

    /// 리스가 살아 있다. HA VM을 켜도 된다.
    pub fn fresh(&self) -> bool {
        self.state().fresh
    }

    /// 리스가 끝났다. 도는 HA VM을 꺼야 한다.
    pub fn stale(&self) -> bool {
        self.state().stale
    }

    /// fresh·stale이 바뀔 때마다 알린다.
    pub fn subscribe(&self) -> watch::Receiver<LeaseState> {
        self.shared.tx.subscribe()
    }

    /// HA VM을 켜거나 도는 HA VM에 붙기 전에 watchdog를 연다(이미 열었으면 그대로). 리스가 지금 살아
    /// 있을 때(`fresh`)만 성공한다. 이전 프로세스가 켜 둔 타이머를 새 리스 없이 늘리면 안 된다.
    /// watchdog가 없으면(`process`) 리스만 본다. 제한 시간을 받아들이지 않은 장치는 쥔 채(다시 열지
    /// 않는다) 실패한다.
    pub fn arm(&self) -> Result<(), ArmError> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.shared.lease(&state, Instant::now()).fresh {
            return Err(ArmError::NotFresh);
        }
        if let (Some(opener), None) = (&self.opener, &state.watchdog) {
            let watchdog = opener.open().map_err(ArmError::Open)?;
            match watchdog.rejected() {
                Some(reason) => {
                    tracing::warn!(
                        reason,
                        "watchdog opened but unusable; HA VMs will not start"
                    )
                }
                None => tracing::info!("watchdog armed"),
            }
            state.watchdog = Some(watchdog);
        }
        match state.watchdog.as_ref().and_then(|w| w.rejected()) {
            Some(reason) => Err(ArmError::Rejected(reason.to_owned())),
            None => Ok(()),
        }
    }

    /// 이 노드에서 HA VM `vm`이 도는지 알린다. 리스가 끝났는데 도는 HA VM이 있으면 쓰다듬지 않는다.
    pub fn set_running(&self, vm: Uuid, running: bool) {
        let mut state = self.shared.state.lock().unwrap();
        if running {
            state.running.insert(vm);
        } else {
            state.running.remove(&vm);
        }
    }

    /// 도는 HA VM 가운데 `keep`에 없는 것을 뺀다. 작업자가 없어 아무도 빼지 않는 VM(옮겨 간 뒤 멈춘
    /// 원본 등)을 reconciler가 정리한다.
    pub fn retain_running(&self, keep: &HashSet<Uuid>) {
        self.shared
            .state
            .lock()
            .unwrap()
            .running
            .retain(|vm| keep.contains(vm));
    }

    /// 갱신과 쓰다듬기를 멈춘다. 도는 HA VM이 없으면 watchdog를 magic close로 멈추고 닫는다. 있으면
    /// 그대로 두어 opld가 제한 시간 안에 돌아오지 않으면 노드가 재시작된다.
    pub fn close(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        let mut state = self.shared.state.lock().unwrap();
        let Some(watchdog) = state.watchdog.take() else {
            return;
        };
        if state.running.is_empty() {
            if let Err(e) = watchdog.magic_close() {
                tracing::warn!(error = %e, "cannot stop the watchdog");
            }
        } else {
            tracing::warn!(
                vms = state.running.len(),
                "HA VMs are running; leaving the watchdog armed, so this node restarts unless opld comes back in time"
            );
            drop(watchdog);
        }
    }
}

impl Drop for HaGuard {
    fn drop(&mut self) {
        for task in self.tasks.get_mut().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// 갱신 간격마다 리스를 갱신한다. 한 번은 간격의 두 배까지 기다린다.
async fn renew_loop(shared: Arc<Shared>, renewer: Arc<dyn LeaseRenewer>, interval: Duration) {
    let limit = interval * 2;
    let mut failing = false;
    loop {
        // 보내기 전에 잰다. 리더가 받은 시각은 이보다 늦다(설계 §3.2).
        let sent = Instant::now();
        let result = tokio::time::timeout(limit, renewer.renew_lease(limit))
            .await
            .unwrap_or_else(|_| Err(ClusterError::Timeout));
        match result {
            Ok(()) => {
                {
                    let mut state = shared.state.lock().unwrap();
                    state.last_renewed = Some(state.last_renewed.map_or(sent, |s| s.max(sent)));
                }
                shared.publish();
                shared.renewed.notify_one();
                if failing {
                    tracing::info!("HA lease renewed again");
                    failing = false;
                }
            }
            Err(e) => {
                if !failing {
                    tracing::warn!(error = %e, "cannot renew the HA lease");
                    failing = true;
                }
            }
        }
        tokio::time::sleep_until(sent + interval).await;
    }
}

/// 리스 상태가 바뀌는 때에 알리고, 1초마다 watchdog를 쓰다듬는다. 리스가 살아 있거나 도는 HA VM이
/// 없을 때만 쓰다듬는다.
async fn watch_loop(shared: Arc<Shared>) {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut holding = false;
    loop {
        let next = shared.publish();
        tokio::select! {
            () = async {
                match next {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {}
            () = shared.renewed.notified() => {}
            _ = ping.tick() => {
                let mut guard = shared.state.lock().unwrap();
                let state = &mut *guard;
                let fresh = shared.lease(state, Instant::now()).fresh;
                let Some(watchdog) = state.watchdog.as_mut() else {
                    continue;
                };
                if fresh || state.running.is_empty() {
                    holding = false;
                    if let Err(e) = watchdog.ping() {
                        tracing::warn!(error = %e, "cannot ping the watchdog");
                    }
                } else if !holding {
                    holding = true;
                    tracing::warn!("HA lease lost with HA VMs running; no longer pinging the watchdog");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FENCE_AFTER: Duration = Duration::from_secs(10);
    const INTERVAL: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    /// 정해 둔 대로 갱신에 성공하거나 실패하고, 보낸 시각과 제한 시간을 남긴다.
    #[derive(Default)]
    struct FakeRenewer {
        ok: Mutex<bool>,
        /// 응답까지 걸리는 시간. None이면 끝나지 않는다.
        delay: Mutex<Option<Duration>>,
        /// 받은 갱신(보낸 시각, 제한 시간)
        calls: Mutex<Vec<(Instant, Duration)>>,
        /// 성공한 갱신을 보낸 시각
        oks: Mutex<Vec<Instant>>,
    }

    impl FakeRenewer {
        fn new(ok: bool) -> Arc<Self> {
            let r = Self::default();
            *r.ok.lock().unwrap() = ok;
            *r.delay.lock().unwrap() = Some(Duration::ZERO);
            Arc::new(r)
        }

        fn set_ok(&self, ok: bool) {
            *self.ok.lock().unwrap() = ok;
        }

        /// 마지막으로 성공한 갱신을 보낸 시각
        fn last_ok(&self) -> Instant {
            *self.oks.lock().unwrap().last().unwrap()
        }
    }

    #[async_trait]
    impl LeaseRenewer for FakeRenewer {
        async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
            let sent = Instant::now();
            self.calls.lock().unwrap().push((sent, timeout));
            let delay = *self.delay.lock().unwrap();
            match delay {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
            if *self.ok.lock().unwrap() {
                self.oks.lock().unwrap().push(sent);
                Ok(())
            } else {
                Err(ClusterError::Timeout)
            }
        }
    }

    /// 장치 대신 한 일을 적는다.
    #[derive(Default)]
    struct FakeWatchdog {
        log: Arc<Mutex<Vec<&'static str>>>,
        fail_open: Mutex<bool>,
        /// 열린 장치가 제한 시간을 받아들이지 않은 이유
        reject: Mutex<Option<&'static str>>,
    }

    impl FakeWatchdog {
        fn log(&self) -> Vec<&'static str> {
            self.log.lock().unwrap().clone()
        }

        fn count(&self, what: &str) -> usize {
            self.log().iter().filter(|e| **e == what).count()
        }
    }

    struct FakeDevice(Arc<Mutex<Vec<&'static str>>>, Option<&'static str>);

    impl Watchdog for FakeDevice {
        fn ping(&mut self) -> io::Result<()> {
            self.0.lock().unwrap().push("ping");
            Ok(())
        }

        fn magic_close(self: Box<Self>) -> io::Result<()> {
            self.0.lock().unwrap().push("magic_close");
            Ok(())
        }

        fn rejected(&self) -> Option<&str> {
            self.1
        }
    }

    impl Drop for FakeDevice {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("drop");
        }
    }

    impl WatchdogOpener for FakeWatchdog {
        fn open(&self) -> io::Result<Box<dyn Watchdog>> {
            if *self.fail_open.lock().unwrap() {
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, "busy"));
            }
            self.log.lock().unwrap().push("open");
            let reject = *self.reject.lock().unwrap();
            Ok(Box::new(FakeDevice(self.log.clone(), reject)))
        }
    }

    fn guard(renewer: &Arc<FakeRenewer>, watchdog: Option<&Arc<FakeWatchdog>>) -> Arc<HaGuard> {
        HaGuard::spawn(
            renewer.clone(),
            HaConfig {
                fence_after: FENCE_AFTER,
                renew_interval: INTERVAL,
                watchdog: watchdog.map(|w| w.clone() as Arc<dyn WatchdogOpener>),
            },
        )
    }

    async fn sleep_until(at: Instant) {
        tokio::time::sleep_until(at).await;
    }

    /// 한 번도 갱신하지 못하면 fresh가 아니고, 시작에서 fence_after가 지나야 stale이다.
    #[tokio::test(start_paused = true)]
    async fn without_renewal_stale_starts_counting_from_the_start() {
        let start = Instant::now();
        let renewer = FakeRenewer::new(false);
        let g = guard(&renewer, None);
        let mut rx = g.subscribe();
        assert_eq!(g.state(), LeaseState::default());
        sleep_until(start + FENCE_AFTER - MS).await;
        assert!(!g.fresh());
        assert!(!g.stale());
        rx.changed().await.unwrap();
        assert_eq!(Instant::now(), start + FENCE_AFTER);
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: false,
                stale: true
            }
        );
        assert!(g.stale());
        g.close();
    }

    /// 갱신하면 fresh이고, 마지막으로 성공한 갱신을 보낸 시각에서 fence_after가 지나면 stale이다.
    #[tokio::test(start_paused = true)]
    async fn fresh_until_fence_after_from_the_last_renewal_sent() {
        let renewer = FakeRenewer::new(true);
        // 응답에 시간이 걸려도 보낸 시각부터 센다.
        *renewer.delay.lock().unwrap() = Some(Duration::from_millis(300));
        let g = guard(&renewer, None);
        let mut rx = g.subscribe();
        rx.changed().await.unwrap();
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: true,
                stale: false
            }
        );
        // 몇 번 더 갱신한 뒤 끊는다.
        tokio::time::sleep(Duration::from_millis(3500)).await;
        renewer.set_ok(false);
        tokio::time::sleep(INTERVAL).await;
        let last = renewer.last_ok();
        assert!(last > Instant::now() - INTERVAL * 3);
        sleep_until(last + FENCE_AFTER - MS).await;
        assert!(g.fresh());
        assert!(!g.stale());
        rx.changed().await.unwrap();
        assert_eq!(Instant::now(), last + FENCE_AFTER);
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: false,
                stale: true
            }
        );
        // 다시 갱신되면 곧바로 fresh로 알린다.
        renewer.set_ok(true);
        rx.changed().await.unwrap();
        assert!(rx.borrow_and_update().fresh);
        assert!(!g.stale());
        g.close();
    }

    /// 한 번의 갱신은 간격의 두 배까지 기다리고, 끝나지 않으면 다음 갱신을 보낸다.
    #[tokio::test(start_paused = true)]
    async fn a_hung_renewal_times_out_after_twice_the_interval() {
        let renewer = FakeRenewer::new(true);
        *renewer.delay.lock().unwrap() = None;
        let start = Instant::now();
        let g = guard(&renewer, None);
        sleep_until(start + INTERVAL * 2 - MS).await;
        assert_eq!(renewer.calls.lock().unwrap().len(), 1);
        sleep_until(start + INTERVAL * 2 + MS).await;
        assert_eq!(
            *renewer.calls.lock().unwrap(),
            [(start, INTERVAL * 2), (start + INTERVAL * 2, INTERVAL * 2)]
        );
        assert!(!g.fresh());
        // 두 번째도 끝나지 않고, 세 번째는 성공한다.
        *renewer.delay.lock().unwrap() = Some(Duration::ZERO);
        sleep_until(start + INTERVAL * 4 + MS).await;
        assert!(g.fresh());
        assert_eq!(renewer.last_ok(), start + INTERVAL * 4);
        g.close();
    }

    /// 갱신하기 전에는 watchdog를 열지 않는다. 연 뒤에는 다시 열지 않고, 열지 못하면 다음에 다시 연다.
    #[tokio::test(start_paused = true)]
    async fn arm_needs_a_renewal_and_opens_the_watchdog_once() {
        let renewer = FakeRenewer::new(false);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(INTERVAL).await;
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        assert!(watchdog.log().is_empty());
        renewer.set_ok(true);
        tokio::time::sleep(INTERVAL).await;
        *watchdog.fail_open.lock().unwrap() = true;
        assert!(matches!(g.arm(), Err(ArmError::Open(_))));
        *watchdog.fail_open.lock().unwrap() = false;
        g.arm().unwrap();
        g.arm().unwrap();
        assert_eq!(watchdog.count("open"), 1);
        g.close();
    }

    /// 갱신한 적이 있어도 리스가 끝났으면 watchdog를 열지 않는다(리스 없이 타이머를 늘리지 않는다).
    #[tokio::test(start_paused = true)]
    async fn arm_needs_a_fresh_lease() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        let mut rx = g.subscribe();
        rx.wait_for(|s| s.fresh).await.unwrap();
        renewer.set_ok(false);
        rx.wait_for(|s| s.stale).await.unwrap();
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        assert!(watchdog.log().is_empty());
        renewer.set_ok(true);
        rx.wait_for(|s| s.fresh).await.unwrap();
        g.arm().unwrap();
        assert_eq!(watchdog.count("open"), 1);
        g.close();
    }

    /// watchdog가 없으면(process) 갱신했는지만 본다.
    #[tokio::test(start_paused = true)]
    async fn arm_without_a_watchdog_only_needs_a_renewal() {
        let renewer = FakeRenewer::new(false);
        let g = guard(&renewer, None);
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        renewer.set_ok(true);
        tokio::time::sleep(INTERVAL).await;
        g.arm().unwrap();
        g.close();
    }

    /// 리스가 살아 있거나 도는 HA VM이 없을 때만 1초마다 쓰다듬는다.
    #[tokio::test(start_paused = true)]
    async fn pings_only_while_fresh_or_without_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        let vm = Uuid::new_v4();
        g.set_running(vm, true);
        // 리스가 살아 있으면 HA VM이 돌아도 쓰다듬는다.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let pinged = watchdog.count("ping");
        assert!((4..=6).contains(&pinged), "{pinged}");
        // 리스가 끝나면 HA VM이 도는 동안 쓰다듬지 않는다.
        renewer.set_ok(false);
        let mut rx = g.subscribe();
        rx.wait_for(|s| !s.fresh).await.unwrap();
        let before = watchdog.count("ping");
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(watchdog.count("ping"), before);
        // 도는 HA VM이 없으면 리스가 없어도 쓰다듬는다.
        g.set_running(vm, false);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(watchdog.count("ping") >= before + 2);
        g.close();
    }

    /// 정상 종료 때 도는 HA VM이 없으면 magic close를 한다.
    #[tokio::test(start_paused = true)]
    async fn close_stops_the_watchdog_without_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        let vm = Uuid::new_v4();
        g.set_running(vm, true);
        g.set_running(vm, false);
        g.close();
        let log = watchdog.log();
        assert_eq!(&log[log.len() - 2..], ["magic_close", "drop"]);
        // 닫은 뒤에는 쓰다듬지 않는다.
        let pinged = watchdog.count("ping");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(watchdog.count("ping"), pinged);
    }

    /// 도는 HA VM이 있으면 magic close 없이 닫아 타이머를 그대로 둔다.
    #[tokio::test(start_paused = true)]
    async fn close_leaves_the_watchdog_running_with_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        g.set_running(Uuid::new_v4(), true);
        g.close();
        assert_eq!(watchdog.log().last(), Some(&"drop"));
        assert_eq!(watchdog.count("magic_close"), 0);
    }

    /// 제한 시간을 받아들이지 않은 장치도 닫지 않고 쥔다: 그냥 닫으면 커널 타이머가 남아 나중에 노드가
    /// 재시작된다. HA VM에는 쓰지 않고(`arm` 실패, 다시 열지 않는다) 평소 규칙대로 쓰다듬으며, 정상 종료
    /// 때 도는 HA VM이 없으면 magic close로 멈춘다.
    #[tokio::test(start_paused = true)]
    async fn a_rejected_watchdog_is_held_pinged_and_magic_closed() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        *watchdog.reject.lock().unwrap() = Some("the device chose 60s");
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        let Err(e) = g.arm() else {
            panic!("armed a rejected watchdog");
        };
        assert!(matches!(e, ArmError::Rejected(_)), "{e:?}");
        assert!(e.to_string().contains("the device chose 60s"), "{e}");
        assert!(matches!(g.arm(), Err(ArmError::Rejected(_))));
        assert_eq!(watchdog.log(), ["open"]);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(watchdog.count("ping") >= 2);
        g.close();
        let log = watchdog.log();
        assert_eq!(&log[log.len() - 2..], ["magic_close", "drop"]);
    }

    /// 실제 장치 구현은 watchdog가 아닌 파일을 받아들이지 않는다: Linux는 ioctl이 실패해 쓸 수 없는
    /// 장치로 쥐고(`V`를 쓰지 않는다), 다른 OS는 열기가 실패한다. 진짜 `/dev/watchdog`는 열지 않는다.
    #[test]
    fn device_watchdog_rejects_a_regular_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let device = DeviceWatchdog {
            path: file.path().into(),
            timeout_secs: 30,
        };
        match (cfg!(target_os = "linux"), device.open()) {
            (true, Ok(watchdog)) => {
                let reason = watchdog.rejected().expect("accepted a regular file");
                assert!(
                    reason.starts_with("cannot set the watchdog timeout to 30s"),
                    "{reason}"
                );
            }
            (false, Err(e)) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            (linux, opened) => panic!("linux: {linux}, opened: {}", opened.is_ok()),
        }
        assert_eq!(std::fs::read(file.path()).unwrap(), b"");
    }

    fn einval() -> io::Error {
        io::Error::from_raw_os_error(22)
    }

    /// 장치가 정한 제한 시간이 요청 이하일 때만 받아들인다. 정하지 못했으면 지금 값을 읽어 본다.
    #[test]
    fn device_timeout_must_not_exceed_the_request() {
        let unread = || -> io::Result<std::ffi::c_int> { panic!("read the timeout") };
        // 정한 대로이거나 더 짧으면 받아들인다.
        assert_eq!(accept_timeout(30, Ok(30), unread).unwrap(), 30);
        assert_eq!(accept_timeout(30, Ok(20), unread).unwrap(), 20);
        // 더 길게 골랐거나 0이면 안 된다.
        assert!(accept_timeout(30, Ok(60), unread).is_err());
        assert!(accept_timeout(30, Ok(0), unread).is_err());
        // 정하지 못했으면 지금 값이 요청 이하일 때만 받아들인다.
        assert_eq!(accept_timeout(30, Err(einval()), || Ok(10)).unwrap(), 10);
        assert_eq!(accept_timeout(30, Err(einval()), || Ok(30)).unwrap(), 30);
        let e = accept_timeout(30, Err(einval()), || Ok(60)).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(22));
        let e = accept_timeout(30, Err(einval()), || Err(io::Error::other("no"))).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(22));
    }
}
