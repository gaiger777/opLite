use std::sync::Arc;
use std::time::Duration;

use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, EventKind, Hypervisor, RuntimeStatus, StopMode};
use opl_types::vm::{PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backoff::{FailureTracker, Next};
use crate::config::ReconcilerConfig;
use crate::resolve::{ResolveError, runtime_spec};

/// 실행 중이던 VM이 사라졌을 때 그 이유를 알리는 종료 이벤트를 기다리는 시간.
/// 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 된다.
/// QEMU 드라이버는 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 최대 5초(`EXIT_GRACE`) 기다린 다음에
/// 크래시를 알리므로 그보다 길게 잡는다. 짧으면 느리게 끝난 크래시를 두 번 세게 된다.
const EVENT_GRACE: Duration = Duration::from_secs(6);

/// 작업자를 깨우는 이유.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Trigger {
    /// spec이 바뀌었거나 전체 재동기화. 원하는 상태와 실제 상태를 다시 맞춘다.
    Sync,
    /// 하이퍼바이저가 알린 수명 이벤트
    Event(EventKind),
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다.
    ForceStop,
}

#[derive(Debug, thiserror::Error)]
enum StepError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Driver(#[from] DriverError),
}

/// VM 하나를 맡는 작업자. 이 VM에 대한 드라이버 호출은 모두 이 태스크에서만 일어난다.
///
/// 매번 로컬 저장소에서 VM을 다시 읽고 실제 상태를 조회해 맞추므로(level-triggered),
/// 같은 트리거를 여러 번 받아도 안전하다. 이벤트는 "게스트가 스스로 꺼졌다"처럼 실제 상태만
/// 봐서는 알 수 없는 사실을 알릴 때만 쓴다.
pub(crate) struct Worker {
    id: Uuid,
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    /// 진행 중인 ACPI 종료 요청을 보낸 시각. 강제 종료로 넘어갈 시점을 정한다.
    /// 원하는 상태가 다시 running으로 바뀌면 취소하므로, 새 종료는 늘 ACPI부터 다시 시작한다.
    stop_requested: Option<Instant>,
    /// 이 작업자가 마지막으로 종료를 요청한(ACPI 또는 강제) 시각. 뒤이은 종료 이벤트가 우리
    /// 요청의 결과인지 가린다. 요청을 취소했거나 원하는 상태가 다시 running으로 바뀌어도
    /// 게스트(또는 강제 종료)는 요청대로 꺼질 수 있으므로 `acpi_timeout` 동안 유효하다.
    stopped_by_us: Option<Instant>,
    /// 다음 단계에서 ACPI 응답을 기다리지 않고 강제 종료한다. 원하는 상태가 stopped일 때만 켜고,
    /// 그 뒤 running으로 바뀌면 reconcile이 끈다.
    force_requested: bool,
    failures: FailureTracker,
    /// 백오프 중이면 이 시각까지 시작하지 않는다. status에 보일 이유를 함께 둔다.
    backoff: Option<(Instant, String)>,
    /// spec.power를 stopped로 바꿔야 한다(게스트 종료, 재시작 포기). 안쪽 값은 status에 남길 오류다.
    /// 쓰기에 실패하면 남겨 두었다가 다음 단계에서 다시 쓴다.
    power_off: Option<Option<String>>,
    /// 마지막으로 조회했을 때 VM이 실행 중이었는지
    was_running: bool,
    /// 사라진 VM의 종료 이벤트를 이 시각까지 기다린다.
    awaiting_event: Option<Instant>,
    wake_at: Option<Instant>,
}

impl Worker {
    pub(crate) fn new(
        id: Uuid,
        cluster: Arc<Cluster>,
        hv: Arc<dyn Hypervisor>,
        config: Arc<ReconcilerConfig>,
    ) -> Self {
        Self {
            id,
            cluster,
            hv,
            config,
            stop_requested: None,
            stopped_by_us: None,
            force_requested: false,
            failures: FailureTracker::default(),
            backoff: None,
            power_off: None,
            was_running: false,
            awaiting_event: None,
            wake_at: None,
        }
    }

    /// 보내는 쪽이 모두 사라지면 남은 트리거를 처리하고 끝난다. `previous`는 같은 VM을 맡았던
    /// 작업자다. 같은 VM에 대한 드라이버 호출이 겹치지 않도록 그것이 끝난 뒤에 시작한다.
    pub(crate) async fn run(
        mut self,
        previous: Option<JoinHandle<()>>,
        mut rx: mpsc::UnboundedReceiver<Trigger>,
    ) {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        loop {
            let trigger = match self.wake_at {
                Some(at) => tokio::select! {
                    t = rx.recv() => t,
                    () = tokio::time::sleep_until(at) => Some(Trigger::Sync),
                },
                None => rx.recv().await,
            };
            let Some(trigger) = trigger else { return };
            self.wake_at = None;
            if let Err(e) = self.step(trigger).await {
                tracing::warn!(vm = %self.id, error = %e, "reconcile failed; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
        }
    }

    fn wake(&mut self, at: Instant) {
        self.wake_at = Some(self.wake_at.map_or(at, |w| w.min(at)));
    }

    async fn step(&mut self, trigger: Trigger) -> Result<(), StepError> {
        let node = self.config.node;
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        let Some(vm) = vm.filter(|vm| vm.spec.node == Some(node)) else {
            // 삭제되었거나 다른 노드로 배정되었다. 돌고 있더라도 멋대로 끄지 않는다.
            if self.hv.inspect(self.id).await?.is_some() {
                tracing::warn!(vm = %self.id, "VM is running but not assigned to this node; leaving it alone");
            }
            return Ok(());
        };
        match trigger {
            Trigger::Event(kind) => self.on_event(&vm, kind),
            // running을 원할 때 온 명령은 버린다. 남겨 두면 나중의 종료가 ACPI 없이 강제 종료된다.
            Trigger::ForceStop if vm.spec.power == PowerState::Stopped => {
                self.force_requested = true
            }
            Trigger::ForceStop => {}
            Trigger::Sync => {}
        }
        if self.power_off.is_none() {
            let status = self.reconcile(&vm).await?;
            if self.power_off.is_none() {
                if status != vm.status {
                    self.cluster
                        .update_status::<VmSpec>(self.id, &status)
                        .await?;
                }
                return Ok(());
            }
        }
        self.write_power_off(vm).await
    }

    fn on_event(&mut self, vm: &Vm, kind: EventKind) {
        let now = Instant::now();
        if kind != EventKind::Started {
            self.was_running = false;
            self.awaiting_event = None;
            self.stop_requested = None;
        }
        let requested = match kind {
            EventKind::Started => return,
            EventKind::Stopped { .. } | EventKind::Crashed => self
                .stopped_by_us
                .take()
                .is_some_and(|at| now < at + self.config.acpi_timeout),
        };
        // 원하던 대로 꺼졌다. 강제 종료의 결과로 Crashed가 와도 실패로 세지 않는다.
        if vm.spec.power == PowerState::Stopped {
            return;
        }
        match kind {
            // 우리가 요청한 종료가, 원하는 상태가 다시 running으로 바뀐 뒤에 끝났다. 다시 시작한다.
            // 강제 종료의 결과로 Crashed가 와도 크래시로 보지 않는다.
            _ if requested => {}
            // 게스트가 스스로 꺼졌다(설계 5.3). `guest_initiated`는 opld가 요청한 ACPI 종료에서도
            // true이므로 우리가 요청했는지로 먼저 가린다.
            EventKind::Stopped {
                guest_initiated: true,
            } => self.power_off = Some(None),
            // 우리가 요청하지 않았고 게스트의 결정도 아니다(호스트의 SIGTERM 등). 원하는 상태는
            // 그대로 두므로 실패로 세지 않고 다시 시작한다.
            EventKind::Stopped {
                guest_initiated: false,
            } => {}
            EventKind::Crashed => match vm.spec.restart {
                RestartPolicy::Never => self.power_off = Some(Some("VM crashed".into())),
                RestartPolicy::OnFailure => self.record_failure("VM crashed".into()),
            },
            EventKind::Started => {}
        }
    }

    fn record_failure(&mut self, reason: String) {
        let now = Instant::now();
        match self.failures.record(now, &self.config.restart) {
            Next::RetryAfter(delay) => {
                self.backoff = Some((now + delay, format!("{reason}; retrying in {delay:?}")));
            }
            Next::GiveUp => {
                let n = self.config.restart.max_failures;
                self.power_off = Some(Some(format!("{reason}; gave up after {n} failures")));
            }
        }
    }

    /// 원하는 상태를 향해 한 걸음 움직이고, 기록해야 할 status를 돌려준다.
    async fn reconcile(&mut self, vm: &Vm) -> Result<VmStatus, StepError> {
        let actual = self.hv.inspect(self.id).await?;
        let mut status = vm.status.clone();
        status.node = Some(self.config.node);
        let now = Instant::now();
        if std::mem::replace(&mut self.was_running, actual.is_some()) && actual.is_none() {
            self.awaiting_event = Some(now + EVENT_GRACE);
        }
        match (vm.spec.power, actual) {
            (PowerState::Running, Some(state)) => {
                // 진행 중인 종료 요청을 취소한다. 게스트가 이미 받은 요청대로 꺼지면
                // `stopped_by_us`로 가려 다시 시작한다.
                self.stop_requested = None;
                self.force_requested = false;
                status.phase = VmPhase::Running;
                status.pid = Some(state.pid);
                status.last_error = None;
                if state.status != RuntimeStatus::Running {
                    // opld가 기동 도중(cont 전)에 죽었거나 QMP로 다룰 수 없다. start()가 cont를
                    // 보내거나 다시 붙는다. 실패해도 돌고 있는 VM이므로 끄거나 실패로 세지 않는다.
                    if let Err(e) = self.reattach(vm).await {
                        tracing::warn!(vm = %self.id, error = %e, "VM is unresponsive; will retry");
                        status.last_error = Some(format!("VM is unresponsive: {e}"));
                        self.wake(now + self.config.retry_interval);
                    }
                }
            }
            // 방금까지 돌던 VM이 사라졌다. 왜 꺼졌는지(게스트 종료, 크래시) 이벤트로 안 뒤에 움직인다.
            // 기다려도 오지 않으면(이벤트 유실) 원하는 상태대로 다시 시작한다.
            (PowerState::Running, None)
                if self.awaiting_event.is_some_and(|deadline| now < deadline) =>
            {
                self.wake(self.awaiting_event.unwrap());
            }
            (PowerState::Running, None) => {
                self.awaiting_event = None;
                self.force_requested = false;
                if self.backoff.as_ref().is_some_and(|(at, _)| *at <= now) {
                    self.backoff = None;
                }
                if self.backoff.is_none() && self.start(vm).await? {
                    // 이제 돌고 있다. 곧바로 꺼져도 이벤트보다 먼저 온 재동기화가 다시 시작하지
                    // 않고 이벤트를 기다리게 한다.
                    self.was_running = true;
                    match self.hv.inspect(self.id).await? {
                        Some(state) => {
                            status.phase = VmPhase::Running;
                            status.pid = Some(state.pid);
                            status.last_error = None;
                        }
                        // 시작하자마자 꺼졌다. 뒤따르는 이벤트에서 다시 맞춘다.
                        None => {
                            self.was_running = false;
                            let deadline = Instant::now() + EVENT_GRACE;
                            self.awaiting_event = Some(deadline);
                            self.wake(deadline);
                            status.phase = VmPhase::Starting;
                            status.pid = None;
                        }
                    }
                }
                if let Some((at, reason)) = self.backoff.clone() {
                    self.wake(at);
                    status.phase = VmPhase::Starting;
                    status.pid = None;
                    status.last_error = Some(reason);
                }
            }
            (PowerState::Stopped, Some(state)) => {
                self.backoff = None;
                match self.stop_requested {
                    _ if self.force_requested => {
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.force_requested = false;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    None => {
                        // 요청이 실패해도(QMP로 다룰 수 없는 VM) acpi_timeout 뒤 강제 종료한다.
                        self.stop_requested = Some(now);
                        self.stopped_by_us = Some(now);
                        self.wake(now + self.config.acpi_timeout);
                        if let Err(e) = self.hv.stop(self.id, StopMode::Acpi).await {
                            tracing::warn!(vm = %self.id, error = %e, "ACPI shutdown request failed; will force after timeout");
                        }
                    }
                    Some(at) if now >= at + self.config.acpi_timeout => {
                        tracing::warn!(vm = %self.id, "guest ignored ACPI shutdown; forcing");
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    Some(at) => self.wake(at + self.config.acpi_timeout),
                }
                status.phase = VmPhase::Stopping;
                status.pid = Some(state.pid);
            }
            (PowerState::Stopped, None) => {
                self.stop_requested = None;
                self.force_requested = false;
                self.awaiting_event = None;
                self.backoff = None;
                self.failures.clear();
                // 포기해서 멈춘 VM은 사용자가 다시 시작할 때까지 Failed로 남긴다.
                if status.phase != VmPhase::Failed {
                    status.phase = VmPhase::Stopped;
                }
                status.pid = None;
            }
        }
        Ok(status)
    }

    /// VM을 시작한다. 시작하지 못하고 실패를 기록했으면(백오프 또는 포기) false.
    async fn start(&mut self, vm: &Vm) -> Result<bool, StepError> {
        let spec = match runtime_spec(&self.cluster, vm, self.config.node) {
            Ok(spec) => spec,
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 참조가 틀렸다. spec을 고쳐야 하므로 다시 시도하지 않는다.
            Err(e) => {
                self.power_off = Some(Some(e.to_string()));
                return Ok(false);
            }
        };
        match self.hv.start(&spec, None).await {
            Ok(()) => Ok(true),
            Err(e) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(false)
            }
            Err(e) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
                Ok(false)
            }
        }
    }

    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    async fn reattach(&self, vm: &Vm) -> Result<(), String> {
        let spec = runtime_spec(&self.cluster, vm, self.config.node).map_err(|e| e.to_string())?;
        self.hv.start(&spec, None).await.map_err(|e| e.to_string())
    }

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
        if vm.spec.power == PowerState::Running {
            let mut spec = vm.spec.clone();
            spec.power = PowerState::Stopped;
            // 이 단계에서 읽은 뒤 spec이 바뀌었으면 RevisionMismatch로 거부된다. 다음 단계에서
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            self.cluster
                .update_spec(self.id, Some(vm.revision), vm.labels.clone(), &spec)
                .await?;
        }
        let status = VmStatus {
            phase: if error.is_some() {
                VmPhase::Failed
            } else {
                VmPhase::Stopped
            },
            node: Some(self.config.node),
            pid: None,
            last_error: error,
            snapshot: vm.status.snapshot,
        };
        self.cluster
            .update_status::<VmSpec>(self.id, &status)
            .await?;
        self.power_off = None;
        self.backoff = None;
        self.failures.clear();
        Ok(())
    }
}
