use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{ActualState, DriverError, EventKind, Hypervisor, RuntimeStatus, StopMode};
use opl_types::node::{HA_PLACEMENT, NodeSpec};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{
    Firmware, HaState, PowerState, RestartPolicy, Vm, VmHaStatus, VmPhase, VmSpec, VmStatus,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backoff::{FailureTracker, Next};
use crate::config::ReconcilerConfig;
use crate::ha::ArmError;
use crate::reconciler::{Held, Restores};
use crate::resolve::{
    ResolveError, apply_disk_moves, running_spec, runtime_spec, set_disks_shared,
};

/// 실행 중이던 VM이 사라졌을 때 그 이유를 알리는 종료 이벤트를 기다리는 시간.
/// 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 된다.
/// QEMU 드라이버는 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 최대 5초(`EXIT_GRACE`) 기다린 다음에
/// 크래시를 알리므로 그보다 길게 잡는다. 짧으면 느리게 끝난 크래시를 두 번 세게 된다.
const EVENT_GRACE: Duration = Duration::from_secs(6);

/// HA VM을 켜거나 이어서 실행하기 전에 리스를 기다리는 동안의 status
const HA_WAITING: &str = "waiting for the cluster majority before starting an HA VM";
/// 리스를 잃어 HA VM을 끈 뒤 리스를 기다리는 동안의 status
const HA_FENCED: &str = "fenced: lost the cluster majority";
/// 펜싱이 꺼진 노드(`[ha] fencing = "off"`)의 HA VM status
const HA_UNAVAILABLE: &str = "HA is not available on this node";
/// 다른 프로세스가 디스크 이미지를 잠가 두어 다시 시도하는 동안의 status
const IMAGE_LOCKED: &str = "disk image is locked by another process; retrying";
/// 방화벽이 없는 노드(nftables 없음, macOS)에서 보안 그룹을 건 NIC가 있는 VM을 켜려 할 때의 오류
const NO_FIREWALL: &str = "failed to start: firewall: this node has no firewall (nftables)";
/// 남은 마이그레이션을 취소하지 못해(남은 디스크 미러) 표시를 남긴 채 다시 하는 연속 실패 수. 이만큼
/// 실패하면 경고 뒤 표시를 지운다([`Worker::clear_migration`]).
const CANCEL_ATTEMPTS: u32 = 3;

/// 작업자를 깨우는 이유.
#[derive(Debug)]
pub(crate) enum Trigger {
    /// spec이 바뀌었거나 전체 재동기화. 원하는 상태와 실제 상태를 다시 맞춘다.
    Sync,
    /// 하이퍼바이저가 알린 수명 이벤트
    Event(EventKind),
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다.
    ForceStop,
    /// 앞의 단계를 마쳤음을 알린다(`ReconcilerHandle::begin_migration`).
    Barrier(tokio::sync::oneshot::Sender<()>),
}

/// HA VM을 켜거나 멈춘 QEMU를 이어서 실행하기 직전(느린 디스크 해석 뒤)에 다시 본 결과
/// ([`Worker::ha_recheck`])
enum Recheck {
    Go,
    /// 리스가 끝났다. 실패로 세지 않고 다시 본다(status에 남길 이유).
    NotFresh(&'static str),
    /// 그 사이 다른 노드로 배정됐거나 원하는 상태가 stopped가 됐거나 지워졌다(다시 읽은 VM). 켜지 않고
    /// status도 쓰지 않은 채 다시 맞춘다.
    Changed(Option<Box<Vm>>),
}

/// [`Worker::start`]의 결과
enum Start {
    Started,
    /// 켜지 않았다(백오프, 포기, 리스 기다림).
    NotStarted,
    /// HA VM이 그 사이 바뀌었다([`Recheck::Changed`]).
    Changed(Option<Box<Vm>>),
}

/// 멈춘 QEMU를 이어서 실행하지 못한 이유
enum ContinueError {
    /// QMP로 다룰 수 없거나 참조를 풀지 못했다.
    Unresponsive(String),
    /// HA VM의 리스가 끝났다([`Recheck::NotFresh`]).
    NotFresh(&'static str),
    /// HA VM이 그 사이 바뀌었다([`Recheck::Changed`]).
    Changed(Option<Box<Vm>>),
}

/// 게스트 감시(`ha.guest_watch`)의 상태. 이 QEMU(`pid`)에서 에이전트가 한 번이라도 답한 뒤에야 센다.
struct GuestAgent {
    /// 감시하는 QEMU. 바뀌면(새로 띄움) 처음부터 다시 본다.
    pid: u32,
    /// 이 QEMU에서 에이전트가 한 번이라도 답했다. 게스트가 다시 부팅하면(`Reset`) 지운다.
    answered_once: bool,
    /// 답이 없는 시간을 세는 기준(마지막 답). None이면 다음 확인부터 센다(일시 정지·재개 이벤트·마이그레이션 보류
    /// 뒤).
    seen_at: Option<Instant>,
    /// 다음으로 에이전트를 부를 때
    next_ping: Instant,
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
    /// 다음 시작에 불러올 스냅샷 태그(`ReconcilerHandle::restore_on_start`)
    restores: Restores,
    /// 마이그레이션 Task가 다루는 중인 VM. 들어 있으면 이 작업자는 아무것도 하지 않는다.
    held: Held,
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
    /// HA VM이 이 노드에서 `max_restart`보다 많이 크래시했다. 다시 켜지 않고 리더에게 다른 노드로 옮겨 달라고
    /// 한다(`status.ha.relocate_from`). 안쪽 값은 status에 남길 오류다. 요청이 저장소에 보이면 지운다.
    relocate: Option<String>,
    /// 마지막으로 조회했을 때 VM이 실행 중이었는지
    was_running: bool,
    /// 사라진 VM의 종료 이벤트를 이 시각까지 기다린다.
    awaiting_event: Option<Instant>,
    /// 디스크 볼륨이 켜져 있을 수 있다(시작하려고 켰거나 VM이 도는 것을 봤다). QEMU가 끝난 것을
    /// 확인하면 끈다([`Worker::deactivate_disks`]).
    disks_active: bool,
    /// 라이브 마이그레이션이 디스크 볼륨을 원본·대상이 함께 쓰게 해 두었을 수 있다(받은 VM을
    /// 넘겨받았거나, 남은 마이그레이션을 되돌렸거나, 마이그레이션 Task가 보류했다 풀었다). VM이 도는
    /// 동안 다시 이 노드만 쓰게 한다([`Worker::exclusive_disks`], 성공할 때까지). 저장소에 남지 않는
    /// 상태이므로 이 작업자가 직접 켜서 시작한 것이 아니면 참으로 시작한다(opld 재시작이나 넘겨받기
    /// 뒤 이미 도는 VM에 다시 붙으면 한 번 배타로 바꾼다, 멱등).
    disks_shared: bool,
    /// 보류 중에 받은 마지막 수명 이벤트(원본 QEMU의 크래시 등). 보류가 풀리면 처리한다.
    held_event: Option<EventKind>,
    /// 리스를 잃어 이 HA VM을 껐다. 다시 켜거나 원하는 상태가 stopped가 되면 지운다.
    fenced: bool,
    /// guard에 도는 HA VM으로 알려 두었다([`Worker::report_running`]).
    ha_running: bool,
    /// 남은 마이그레이션의 취소가 잇달아 실패한 수([`Worker::clear_migration`])
    cancel_failures: u32,
    /// 게스트 감시 상태([`Worker::guest_hung`]). 감시하지 않거나 QEMU가 없으면 None.
    agent: Option<GuestAgent>,
    wake_at: Option<Instant>,
}

impl Worker {
    pub(crate) fn new(
        id: Uuid,
        cluster: Arc<Cluster>,
        hv: Arc<dyn Hypervisor>,
        config: Arc<ReconcilerConfig>,
        restores: Restores,
        held: Held,
    ) -> Self {
        Self {
            id,
            cluster,
            hv,
            config,
            restores,
            held,
            stop_requested: None,
            stopped_by_us: None,
            force_requested: false,
            failures: FailureTracker::default(),
            backoff: None,
            power_off: None,
            relocate: None,
            was_running: false,
            awaiting_event: None,
            disks_active: false,
            disks_shared: true,
            held_event: None,
            fenced: false,
            ha_running: false,
            cancel_failures: 0,
            agent: None,
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
            let Some(trigger) = trigger else {
                self.exit().await;
                return;
            };
            // 트리거는 차례로 처리하므로 앞의 단계는 이미 끝났다. 마이그레이션 Task가 디스크를
            // 공유로 바꿀 수 있으므로 보류가 풀린 뒤 다시 배타로 바꾼다.
            if let Trigger::Barrier(done) = trigger {
                self.disks_shared = true;
                let _ = done.send(());
                continue;
            }
            self.wake_at = None;
            if let Err(e) = self.step(trigger).await {
                tracing::warn!(vm = %self.id, error = %e, "reconcile failed; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
        }
    }

    /// 작업자가 끝난다. VM이 이 노드에서 빠졌고(삭제, 다른 노드로 옮김) QEMU도 없으면 도는 HA VM에서
    /// 뺀다. QEMU가 아직 돌면 남겨 리스가 끝났을 때 watchdog가 시간을 지키게 한다. reconciler가 멈춰
    /// 끝날 때는 그대로 둔다. VM은 계속 돌고, guard는 정상 종료 때 이 목록으로 watchdog를 멈출지 정한다.
    async fn exit(&self) {
        let Some(guard) = &self.config.ha else { return };
        match self.cluster.get::<VmSpec>(self.id) {
            Ok(Some(vm)) if vm.spec.node == Some(self.config.node) => {}
            Ok(_) => match self.hv.inspect(self.id).await {
                Ok(None) => guard.set_running(self.id, false),
                Ok(Some(_)) => {}
                Err(e) => tracing::warn!(vm = %self.id, error = %e, "cannot inspect VM"),
            },
            Err(e) => tracing::warn!(vm = %self.id, error = %e, "cannot read VM"),
        }
    }

    fn wake(&mut self, at: Instant) {
        self.wake_at = Some(self.wake_at.map_or(at, |w| w.min(at)));
    }

    async fn step(&mut self, trigger: Trigger) -> Result<(), StepError> {
        // 마이그레이션 Task가 status와 QEMU를 다룬다. 끝나면(`end_migration`) 다시 깨운다. 그동안
        // QEMU가 끝났으면(크래시 등) 보류가 풀린 뒤 그 이벤트대로 다룬다(재시작 정책).
        if self.held.lock().unwrap().contains(&self.id) {
            if let Trigger::Event(kind) = trigger
                && !self.guest_event(kind)
            {
                self.held_event = Some(kind);
            }
            // 마이그레이션 Task가 다루는 동안은 게스트 감시 시간을 세지 않는다.
            if let Some(agent) = &mut self.agent {
                agent.seen_at = None;
            }
            return Ok(());
        }
        let node = self.config.node;
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        let Some(mut vm) = vm.filter(|vm| vm.spec.node == Some(node)) else {
            // 다른 노드로 옮겼다(보낸 원본이 꺼진 이벤트).
            self.held_event = None;
            self.unassigned().await?;
            return Ok(());
        };
        // 남은 마이그레이션을 정리하는 쓰기는 과반수가 있어야 하므로 그 전에 끈다.
        self.fence(&vm).await?;
        if vm.status.migration.is_some() || vm.status.phase == VmPhase::Migrating {
            match self.clear_migration(&vm).await? {
                Some(current) => {
                    self.disks_shared = true;
                    vm = current;
                }
                // 그 사이 바뀌었다. 다시 읽어 맞춘다.
                None => {
                    self.wake(Instant::now());
                    return Ok(());
                }
            }
        }
        if let Some(kind) = self.held_event.take() {
            self.on_event(&vm, kind);
        }
        match trigger {
            Trigger::Event(kind) => self.on_event(&vm, kind),
            // running을 원할 때 온 명령은 버린다. 남겨 두면 나중의 종료가 ACPI 없이 강제 종료된다.
            Trigger::ForceStop if vm.spec.power == PowerState::Stopped => {
                self.force_requested = true
            }
            Trigger::ForceStop => {}
            Trigger::Sync | Trigger::Barrier(_) => {}
        }
        if self.power_off.is_none() {
            let status = self.reconcile(&mut vm).await?;
            if self.power_off.is_none() {
                if status != vm.status {
                    self.write_status(status, vm).await?;
                }
                return Ok(());
            }
        }
        self.write_power_off(vm).await
    }

    /// 게스트 이벤트를 게스트 감시에 반영한다(게스트 이벤트였으면 true). 재부팅·종료 시작(`Reset`)이면 에이전트가
    /// 다시 답할 때까지 감시하지 않고, 일시 정지·재개(`PauseChanged`)면 답이 없는 시간을 다음 확인부터 다시 센다.
    fn guest_event(&mut self, kind: EventKind) -> bool {
        if !kind.is_guest() {
            return false;
        }
        if let Some(agent) = &mut self.agent {
            if kind == EventKind::Reset {
                agent.answered_once = false;
            }
            agent.seen_at = None;
        }
        true
    }

    fn on_event(&mut self, vm: &Vm, kind: EventKind) {
        if self.guest_event(kind) {
            return;
        }
        let now = Instant::now();
        if kind != EventKind::Started {
            self.was_running = false;
            self.awaiting_event = None;
            self.stop_requested = None;
        }
        let requested = match kind {
            EventKind::Started | EventKind::Reset | EventKind::PauseChanged => return,
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
            // HA VM(`started`)은 재시작 정책과 관계없이 다시 켜고, 횟수를 넘으면 옮겨 달라고 한다.
            EventKind::Crashed => match (self.ha_restarts(vm), vm.spec.restart) {
                (Some(max), _) => self.record_crash(max, "VM crashed"),
                (None, RestartPolicy::Never) => self.power_off = Some(Some("VM crashed".into())),
                (None, RestartPolicy::OnFailure) => self.record_failure("VM crashed".into()),
            },
            EventKind::Started | EventKind::Reset | EventKind::PauseChanged => {}
        }
    }

    /// 리스가 끝났으면(stale) 이 노드에서 도는 HA VM을 강제로 끈다(설계 §3.3). 그 사이 리더가 다른
    /// 노드에서 켤 수 있다. 우리가 요청한 종료로 표시하므로 실패로 세지 않고 원하는 상태도 바꾸지
    /// 않는다. 원하는 상태가 stopped여도 끈다(끄는 중에 과반수 쪽에서 다시 running으로 바뀌었을 수 있다).
    async fn fence(&mut self, vm: &Vm) -> Result<(), StepError> {
        if !vm.spec.ha_active() || !self.config.ha.as_ref().is_some_and(|g| g.stale()) {
            return Ok(());
        }
        if self.hv.inspect(self.id).await?.is_none() {
            return Ok(());
        }
        tracing::warn!(vm = %self.id, "HA lease lost; force stopping the VM");
        self.stopped_by_us = Some(Instant::now());
        self.stop_requested = None;
        self.force_requested = false;
        self.hv.stop(self.id, StopMode::Force).await?;
        // 우리가 껐으므로 종료 이벤트를 기다리지 않는다.
        self.was_running = false;
        self.fenced = true;
        Ok(())
    }

    /// 삭제되었거나 다른 노드로 배정되었다. 돌고 있더라도 멋대로 끄지 않는다. 단 도는 HA VM으로 알린
    /// VM은 리스가 끝났으면 강제로 끈다(그 사이 리더가 다른 노드에서 켤 수 있다). QEMU가 없을 때만 도는
    /// HA VM에서 뺀다.
    async fn unassigned(&mut self) -> Result<(), StepError> {
        let mut actual = self.hv.inspect(self.id).await?;
        let fenced = actual.is_some()
            && self.ha_running
            && self.config.ha.as_ref().is_some_and(|g| g.stale());
        if fenced {
            tracing::warn!(vm = %self.id, "HA lease lost; force stopping the VM assigned elsewhere");
            self.stopped_by_us = Some(Instant::now());
            self.hv.stop(self.id, StopMode::Force).await?;
            self.was_running = false;
            actual = self.hv.inspect(self.id).await?;
        }
        match actual {
            // 방금 강제로 껐다. 그대로 둔다는 로그를 남기지 않는다.
            Some(_) if fenced => {}
            Some(_) => {
                tracing::warn!(vm = %self.id, "VM is running but not assigned to this node; leaving it alone");
            }
            None => {
                if let Some(guard) = &self.config.ha {
                    guard.set_running(self.id, false);
                }
                self.ha_running = false;
            }
        }
        Ok(())
    }

    /// HA VM을 켜거나 멈춘 QEMU를 이어서 실행해도 되는지 본다: 리스가 살아 있고 watchdog가 켜져
    /// 있어야 한다. 안 되면 status에 남길 이유를 돌려주고, 펜싱이 있는 노드면 잠시 뒤 다시 본다
    /// (실패로 세지 않는다). 리스가 바뀌면 reconciler가 따로 깨운다.
    fn ha_blocked(&mut self, vm: &Vm) -> Option<String> {
        if !vm.spec.ha_active() {
            return None;
        }
        let Some(guard) = &self.config.ha else {
            return Some(HA_UNAVAILABLE.into());
        };
        let ready = if guard.fresh() {
            guard.arm().map_err(|e| e.to_string())
        } else if self.fenced {
            Err(HA_FENCED.into())
        } else {
            Err(HA_WAITING.into())
        };
        let reason = ready.err()?;
        self.wake(Instant::now() + self.config.retry_interval);
        Some(reason)
    }

    /// 이 노드에서 HA VM이 도는지 guard에 알린다.
    fn report_running(&mut self, vm: &Vm, running: bool) {
        if let Some(guard) = &self.config.ha {
            self.ha_running = vm.spec.ha_active() && running;
            guard.set_running(self.id, self.ha_running);
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

    /// 크래시한 이 VM을 HA 정책으로 다시 켤 때 그 `max_restart`. `started` HA VM이고 모든 노드가
    /// `ha-placement`를 알릴 때만이다(옛 리더는 `relocate_from`을 모른다). 아니면 재시작 정책을 따른다.
    /// 노드 목록은 크래시 때만 읽는다.
    fn ha_restarts(&self, vm: &Vm) -> Option<u8> {
        let ha = vm
            .spec
            .ha
            .as_ref()
            .filter(|ha| ha.state == HaState::Started)?;
        ha_placement_everywhere(&self.cluster).then_some(ha.max_restart)
    }

    /// HA VM의 크래시를 센다. `window` 안의 첫 크래시는 기다리지 않고 곧바로 다시 켠다(같은 노드 빠른 재시작),
    /// 둘째부터는 백오프다. `max`번 넘게 크래시하면 다시 켜지 않고 옮겨 달라고 한다.
    fn record_crash(&mut self, max: u8, reason: &str) {
        let now = Instant::now();
        let limit = usize::from(max) + 1;
        match self
            .failures
            .record_first_free(now, &self.config.restart, limit)
        {
            Next::RetryAfter(delay) if delay.is_zero() => self.backoff = None,
            Next::RetryAfter(delay) => {
                self.backoff = Some((now + delay, format!("{reason}; retrying in {delay:?}")));
            }
            Next::GiveUp => {
                tracing::warn!(vm = %self.id, crashes = limit, "HA VM crashed too often on this node; asking HA to move it");
                self.backoff = None;
                self.relocate = Some(format!(
                    "VM crashed {limit} times on this node; asking HA to move it"
                ));
            }
        }
    }

    /// 게스트 감시: `started` HA VM의 정책에 `guest_watch`가 있으면 도는 VM의 게스트 에이전트를
    /// `guest_ping_interval`마다 부른다. 이 QEMU에서 한 번 답한 에이전트가 정책의 시간 넘게 답하지 않으면 true다
    /// (다시 켤 때). VM이 실행 중(`running`)이 아닌 동안(일시 정지·스냅샷·마이그레이션)은 세지 않는다. 채널이 없는
    /// VM(실행 중에 감시를 켬)은 부르기가 실패하므로 감시하지 않는다.
    async fn guest_hung(&mut self, vm: &Vm, state: &ActualState) -> bool {
        let Some(limit) = guest_watch(vm) else {
            self.agent = None;
            return false;
        };
        let now = Instant::now();
        let mut agent = self
            .agent
            .take()
            .filter(|a| a.pid == state.pid)
            .unwrap_or(GuestAgent {
                pid: state.pid,
                answered_once: false,
                seen_at: None,
                next_ping: now,
            });
        let mut hung = false;
        if state.status != RuntimeStatus::Running {
            agent.seen_at = None;
        }
        if now >= agent.next_ping {
            agent.next_ping = now + self.config.guest_ping_interval;
            if state.status == RuntimeStatus::Running {
                let answered = match self.hv.guest_ping(self.id).await {
                    Ok(answered) => answered,
                    Err(e) => {
                        tracing::debug!(vm = %self.id, error = %e, "cannot ping the guest agent");
                        false
                    }
                };
                let now = Instant::now();
                if answered {
                    agent.answered_once = true;
                    agent.seen_at = Some(now);
                } else if agent.answered_once {
                    let since = *agent.seen_at.get_or_insert(now);
                    hung = now.duration_since(since) >= limit;
                }
            }
        }
        self.wake(agent.next_ping);
        self.agent = Some(agent);
        hung
    }

    /// 게스트가 멈췄다: 강제로 끄고 크래시 한 번으로 센다(창 안 첫 번째면 곧바로 다시 켜고, `max_restart`를
    /// 넘으면 다른 노드로 옮겨 달라고 한다). 우리가 요청한 종료로 표시해 뒤따르는 종료 이벤트는 세지 않는다.
    async fn restart_hung(&mut self, vm: &Vm, mut status: VmStatus) -> Result<VmStatus, StepError> {
        let secs = guest_watch(vm).unwrap_or_default().as_secs();
        tracing::warn!(vm = %self.id, secs, "guest agent stopped answering; restarting the VM");
        let reason = format!("guest agent did not answer for {secs}s");
        self.stopped_by_us = Some(Instant::now());
        self.stop_requested = None;
        self.force_requested = false;
        self.hv.stop(self.id, StopMode::Force).await?;
        self.was_running = false;
        self.agent = None;
        match self.ha_restarts(vm) {
            Some(max) => self.record_crash(max, &reason),
            None => self.record_failure(reason.clone()),
        }
        status.phase = VmPhase::Starting;
        status.pid = None;
        status.last_error = Some(format!("{reason}; restarting"));
        self.wake(Instant::now());
        Ok(status)
    }

    /// 원하는 상태를 향해 한 걸음 움직이고, 기록해야 할 status를 돌려준다. 그 전에 status를 먼저
    /// 기록했으면 `vm`을 다시 읽은 것으로 바꾼다.
    async fn reconcile(&mut self, vm: &mut Vm) -> Result<VmStatus, StepError> {
        let mut actual = self.hv.inspect(self.id).await?;
        if let Some(state) = &actual
            && state.status == postmigrate()
            && !(vm.status.node == Some(self.config.node)
                && matches!(
                    vm.status.phase,
                    VmPhase::Running | VmPhase::Migrating | VmPhase::Starting
                ))
        {
            // 확정은 늘 status.node를 대상으로 바꾼다. 이 노드에서 돌던 VM이 아니면 다른 노드로 확정된
            // 마이그레이션의 원본이 정리되지 않고 남은 것이다(그 뒤 VM이 이 노드로 다시 옮겨졌다). 멈춘
            // 메모리는 그 사이 바뀐 디스크와 맞지 않으므로 이어서 실행하지 않고 끈 뒤 새로 시작한다.
            // 리스를 기다리는 HA VM의 원본은 이 작업자가 Starting으로 적어 둔 것이다(아래). 기다리는
            // 동안 HA가 꺼져도 같은 원본이므로 HA 여부는 보지 않는다.
            tracing::warn!(vm = %self.id, "stopping the stale source of a migration committed elsewhere");
            self.stopped_by_us = Some(Instant::now());
            if !self.hv.stop_migrated(self.id).await? {
                self.hv.stop(self.id, StopMode::Force).await?;
            }
            actual = self.hv.inspect(self.id).await?;
        }
        self.report_running(vm, actual.is_some());
        if actual.is_none() {
            self.agent = None;
        }
        let mut status = vm.status.clone();
        status.node = Some(self.config.node);
        let now = Instant::now();
        if std::mem::replace(&mut self.was_running, actual.is_some()) && actual.is_none() {
            self.awaiting_event = Some(now + EVENT_GRACE);
        }
        if actual.is_some() {
            self.disks_active = true;
            // 이 노드의 QEMU가 이 노드의 변수 파일을 쓰고 있다(다시 붙음, 넘겨받은 마이그레이션 포함).
            self.own_firmware(vm, &mut status);
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
                if state.status != RuntimeStatus::Running
                    && let Some(reason) = self.ha_blocked(vm)
                {
                    // 멈춘 HA VM(넘겨받은 마이그레이션 등)은 리스가 있어야 이어서 실행한다.
                    status.phase = VmPhase::Starting;
                    status.last_error = Some(reason);
                } else if state.status != RuntimeStatus::Running {
                    // opld가 기동 도중(cont 전)이나 스냅샷으로 멈춘 채 죽었거나 QMP로 다룰 수 없다.
                    // 붙어 있으면 디스크를 다시 해석하지 않고(LVM이 응답하지 않을 수 있다) 이어서
                    // 실행하고, 아니면 start()가 다시 붙는다. 실패해도 돌고 있는 VM이므로 끄거나
                    // 실패로 세지 않는다.
                    let unresponsive = |e: DriverError| ContinueError::Unresponsive(e.to_string());
                    let mut continued = if state.status == postmigrate() {
                        // 마이그레이션을 보내고 멈춘 원본인데 이 노드에서 돌던 VM이고(위에서 가렸다)
                        // 표시가 없고(위에서 지웠다) 마이그레이션 Task도 다루지 않는다(보류가 아니다). 확정 쓰기는 표시가
                        // 있는 revision에서만 적용되므로 이제 확정될 수 없다: 이어서 실행한다(되돌리기의
                        // 재개 실패, 되돌린 뒤 끝난 마이그레이션).
                        tracing::warn!(vm = %self.id, "resuming the source of a migration that was not committed");
                        self.hv.resume(self.id).await.map_err(unresponsive)
                    } else {
                        // 멈춘 VM을 이어서 실행하거나 다시 붙기 전에 방화벽 규칙을 넣는다.
                        match self.sync_firewall(vm).await {
                            Err(e) => Err(e),
                            Ok(()) => match self.hv.resume_left_stopped(self.id).await {
                                Ok(true) => Ok(()),
                                Ok(false) => self.reattach(vm).await,
                                Err(e) => Err(unresponsive(e)),
                            },
                        }
                    };
                    // 이 노드로 마이그레이션을 받아 멈춰 있는 QEMU다(확정되어 이 노드에 배정됐다).
                    // 받는 중 표시가 있을 때만 이어서 실행하고 표시를 지운다.
                    if continued.is_ok() && state.status == RuntimeStatus::Paused {
                        continued = self.hv.finish_incoming(self.id).await.map_err(unresponsive);
                        self.disks_shared |= continued.is_ok();
                    }
                    match continued {
                        Ok(()) => {}
                        Err(ContinueError::Unresponsive(e)) => {
                            tracing::warn!(vm = %self.id, error = %e, "VM is unresponsive; will retry");
                            status.last_error = Some(format!("VM is unresponsive: {e}"));
                            self.wake(now + self.config.retry_interval);
                        }
                        Err(ContinueError::NotFresh(reason)) => {
                            status.phase = VmPhase::Starting;
                            status.last_error = Some(reason.into());
                            self.wake(now + self.config.retry_interval);
                        }
                        // 켜지 않고 status도 쓰지 않는다. 다시 읽어 맞춘다.
                        Err(ContinueError::Changed(current)) => {
                            if let Some(current) = current {
                                *vm = *current;
                            }
                            self.wake(Instant::now());
                            return Ok(vm.status.clone());
                        }
                    }
                }
                if self.disks_shared && status.last_error.is_none() {
                    self.exclusive_disks(vm).await;
                }
                if vm.spec.ha_active() && status.last_error.is_none() {
                    self.attach_ha(&mut status);
                }
                if self.guest_hung(vm, &state).await {
                    return self.restart_hung(vm, status).await;
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
                self.deactivate_disks(vm).await;
                // 옮겨 달라고 했다. 리더가 옮기거나(spec의 노드), 포기하거나(power stopped), 사용자가 끌
                // 때까지 켜지 않는다. 저장소에 요청이 있으면 리더가 쓴 status를 덮지 않는다.
                let node = self.config.node;
                if vm.status.ha.as_ref().and_then(|ha| ha.relocate_from) == Some(node) {
                    self.relocate = None;
                    self.backoff = None;
                    return Ok(vm.status.clone());
                }
                if let Some(error) = self.relocate.clone() {
                    self.backoff = None;
                    status.phase = VmPhase::Failed;
                    status.pid = None;
                    status.last_error = Some(error);
                    status.ha = Some(VmHaStatus {
                        relocate_from: Some(node),
                        ..status.ha.unwrap_or_default()
                    });
                    return Ok(status);
                }
                // 다시 켠다: 지난 옮기기 요청과 HA 오류를 지운다(옮긴 기록은 남긴다).
                clear_ha(&mut status, |ha| {
                    ha.relocate_from = None;
                    ha.error = None;
                });
                if self.backoff.as_ref().is_some_and(|(at, _)| *at <= now) {
                    self.backoff = None;
                }
                if let Some(reason) = self.ha_blocked(vm) {
                    status.phase = if self.config.ha.is_some() {
                        VmPhase::Starting
                    } else {
                        VmPhase::Stopped
                    };
                    status.pid = None;
                    status.last_error = Some(reason);
                    return Ok(status);
                }
                if self.backoff.is_none() {
                    // 켜는 동안(Starting 기록, 디스크 해석, 변수 받아 오기) 리스가 끝나면 watchdog가
                    // 시간을 지키도록 도는 HA VM으로 먼저 알린다. 켜지 못하면 뺀다.
                    self.report_running(vm, true);
                    let started = self.start_stopped(vm, &mut status).await;
                    if !self.was_running {
                        self.report_running(vm, false);
                    }
                    if let Some(current) = started? {
                        return Ok(current);
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
                self.relocate = None;
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
                self.fenced = false;
                self.stop_requested = None;
                self.force_requested = false;
                self.awaiting_event = None;
                self.deactivate_disks(vm).await;
                self.backoff = None;
                self.relocate = None;
                self.failures.clear();
                // 꺼진 VM은 옮길 것이 없다. HA 오류는 사용자가 다시 켤 때까지 보인다.
                clear_ha(&mut status, |ha| ha.relocate_from = None);
                // 포기해서 멈춘 VM은 사용자가 다시 시작할 때까지 Failed로 남긴다.
                if status.phase != VmPhase::Failed {
                    status.phase = VmPhase::Stopped;
                }
                status.pid = None;
            }
        }
        Ok(status)
    }

    /// 멈춘 VM을 켠다(Starting 기록, [`Worker::start`]). 켰으면 `was_running`이다. 켜지 않고 그대로
    /// 돌려줄 status가 있으면 Some이다.
    async fn start_stopped(
        &mut self,
        vm: &mut Vm,
        status: &mut VmStatus,
    ) -> Result<Option<VmStatus>, StepError> {
        if vm.status.phase == VmPhase::Running {
            // 저장된 phase가 Running인 채 다시 켜면(QEMU가 스스로 끝났다) 그사이 API·Task
            // 실행기가 실행 중 VM으로 보고 디스크를 늘린다(공유 LVM은 늘린 부분을 0으로 채우기
            // 전에 QEMU가 연다). 디스크를 해석하기 전에 Starting을 먼저 기록해, 먼저 들어온
            // 크기 늘리기 Task는 resolve가 보고 나중 것은 API·실행기가 거절하게 한다. 기록하지
            // 못해도(예: 과반수 없음 — 그동안은 Task도 만들 수 없다) VM은 다시 켠다(설계 10절
            // 불변식 4). 기록이 시간 초과로 실패한 사이 과반수가 돌아오면 좁은 틈이 남는다.
            // HA VM은 켜지 않고 다시 시도한다: 과반수가 없으면 리더가 다른 노드에서 켤 수 있다.
            let mut starting = status.clone();
            starting.phase = VmPhase::Starting;
            starting.pid = None;
            match self.write_status(starting, vm.clone()).await {
                Ok(()) => match self.cluster.get::<VmSpec>(self.id)? {
                    // 그 사이 다른 노드로 옮겨졌다. 켜지 않고(status도 쓰지 않고) 다시 읽어 맞춘다.
                    Some(current) if current.spec.node != Some(self.config.node) => {
                        *vm = current;
                        self.wake(Instant::now());
                        return Ok(Some(vm.status.clone()));
                    }
                    Some(current) => *vm = current,
                    None => return Ok(Some(status.clone())),
                },
                Err(e) if vm.spec.ha_active() => return Err(e),
                Err(e) => {
                    tracing::warn!(vm = %self.id, error = %e, "cannot record starting phase; restarting anyway");
                }
            }
        }
        match self.start(vm).await? {
            Start::Started => {}
            Start::NotStarted => return Ok(None),
            // 켜지 않고 status도 쓰지 않는다. 다시 읽어 맞춘다.
            Start::Changed(current) => {
                if let Some(current) = current {
                    *vm = *current;
                }
                self.wake(Instant::now());
                return Ok(Some(vm.status.clone()));
            }
        }
        // 이제 돌고 있다. 곧바로 꺼져도 이벤트보다 먼저 온 재동기화가 다시 시작하지
        // 않고 이벤트를 기다리게 한다.
        self.was_running = true;
        self.fenced = false;
        self.report_running(vm, true);
        // 이 노드의 변수 파일(받아 왔거나 로컬, 없으면 템플릿)로 켰다.
        self.own_firmware(vm, status);
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
        Ok(None)
    }

    /// VM을 시작한다. 시작하지 못하고 실패를 기록했으면(백오프 또는 포기) `NotStarted`. HA VM의 리스가
    /// 그 사이 끝났으면 실패로 세지 않고 잠시 뒤 다시 본다(백오프, `NotStarted`). HA VM이 그 사이
    /// 바뀌었으면 `Changed`.
    async fn start(&mut self, vm: &Vm) -> Result<Start, StepError> {
        let spec = match self.resolve(vm).await {
            Ok(spec) => spec,
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 풀이 이 노드에서 아직 준비되지 않았거나(NFS 서버가 멈춤, 마운트 전 등) 디스크를 늘리는
            // 중이다. 끝나면 시작할 수 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다.
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::VolumeResizing(_))) => {
                let delay = self.config.retry_interval;
                self.backoff = Some((
                    Instant::now() + delay,
                    format!("{e}; retrying in {delay:?}"),
                ));
                return Ok(Start::NotStarted);
            }
            // 참조가 틀렸거나, 이 노드가 지원하지 않는 풀(NFS on macOS)이거나, 저장소가 볼륨을
            // 내주지 못했다. spec을 고치거나 VM을 옮겨야 하므로 다시 시도하지 않는다.
            Err(e) => {
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(e.to_string()));
                return Ok(Start::NotStarted);
            }
        };
        self.disks_active = true;
        // 방금 이 노드에서 켰다(배타).
        self.disks_shared = false;
        self.fetch_firmware(vm).await;
        // 켜기 문턱을 지난 뒤 여기까지 오는 사이 리스가 끝났거나 VM이 바뀌었을 수 있다.
        match self.ha_recheck(vm)? {
            Recheck::Go => {}
            // 실패로 세지 않고 다시 본다.
            Recheck::NotFresh(reason) => {
                self.deactivate_disks(vm).await;
                self.backoff = Some((Instant::now() + self.config.retry_interval, reason.into()));
                return Ok(Start::NotStarted);
            }
            Recheck::Changed(current) => {
                self.deactivate_disks(vm).await;
                return Ok(Start::Changed(current));
            }
        }
        // 보안 그룹을 건 NIC가 있으면 켜기 직전에 지금 클러스터 상태로 이 노드의 방화벽 규칙을 넣는다.
        // 넣지 못하면 켜지 않는다(방화벽 없이 도는 순간이 없게).
        if vm.spec.has_firewall() {
            let Some(gate) = &self.config.firewall else {
                self.deactivate_disks(vm).await;
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(NO_FIREWALL.into()));
                return Ok(Start::NotStarted);
            };
            if let Err(e) = gate.sync().await {
                tracing::warn!(vm = %self.id, error = %e, "cannot apply the firewall rules; not starting");
                self.deactivate_disks(vm).await;
                self.record_failure(format!("failed to start: firewall: {e}"));
                return Ok(Start::NotStarted);
            }
        }
        // 실패해도 다시 쓰지 않는다.
        let restore = self.restores.lock().unwrap().remove(&self.id);
        let started = self.hv.start(&spec, restore.as_deref()).await;
        if started.is_err() {
            self.deactivate_disks(vm).await;
        }
        match (started, restore) {
            (Ok(()), _) => Ok(Start::Started),
            // 태그 없이 다시 시도하면 스냅샷이 아닌 처음부터 부팅하게 되므로 일시 오류라도 멈춘다.
            (Err(e), Some(tag)) => {
                self.power_off = Some(Some(format!("failed to restore snapshot {tag}: {e}")));
                Ok(Start::NotStarted)
            }
            // 다른 프로세스가 이미지를 잠가 두었다(죽은 노드의 NFS 임대 등, 설계 §4.3). 풀리면 켤 수
            // 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다. 사용자는 정지로 멈출 수 있다.
            (Err(e @ DriverError::ImageLocked(_)), None) => {
                tracing::warn!(vm = %self.id, error = %e, "disk image is locked; will retry");
                self.backoff = Some((
                    Instant::now() + self.config.retry_interval,
                    IMAGE_LOCKED.into(),
                ));
                Ok(Start::NotStarted)
            }
            (Err(e), None) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(Start::NotStarted)
            }
            (Err(e), None) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
                Ok(Start::NotStarted)
            }
        }
    }

    /// HA VM을 켜거나 이어서 실행하기 직전에 다시 본다: 리스가 살아 있어야 하고, 로컬 저장소의 VM이
    /// 여전히 이 노드에 배정되어 있고 원하는 상태가 running이어야 한다. 리스가 살아 있으면 그 갱신
    /// 인덱스까지 이 노드에 적용되어 있으므로 과반수 쪽의 쓰기(다른 노드로의 복구 등)를 본다. HA가 아닌
    /// VM은 늘 `Go`다.
    fn ha_recheck(&self, vm: &Vm) -> Result<Recheck, ClusterError> {
        if !vm.spec.ha_active() {
            return Ok(Recheck::Go);
        }
        if !self.config.ha.as_ref().is_some_and(|g| g.fresh()) {
            let reason = if self.fenced { HA_FENCED } else { HA_WAITING };
            return Ok(Recheck::NotFresh(reason));
        }
        Ok(match self.cluster.get::<VmSpec>(self.id)? {
            Some(current)
                if current.spec.node == Some(self.config.node)
                    && current.spec.power == PowerState::Running =>
            {
                Recheck::Go
            }
            current => Recheck::Changed(current.map(Box::new)),
        })
    }

    /// 이미 도는 HA VM에 붙는다: watchdog를 켠다(opld를 다시 시작한 뒤 등). 리스가 살아 있지 않으면
    /// 리스가 바뀔 때 다시 본다. 펜싱이 꺼진 노드면 끄지 않고 그 사실만 남긴다.
    fn attach_ha(&mut self, status: &mut VmStatus) {
        let Some(guard) = &self.config.ha else {
            status.last_error = Some(HA_UNAVAILABLE.into());
            return;
        };
        match guard.arm() {
            Ok(()) | Err(ArmError::NotFresh) => {}
            Err(e) => {
                status.last_error = Some(e.to_string());
                self.wake(Instant::now() + self.config.retry_interval);
            }
        }
    }

    /// 멈춘 UEFI VM을 새로 켜기 전에, 변수 파일을 가진 노드(`firmware_node`)가 다른 노드면 거기서
    /// 받아 와 이 노드의 파일을 바꾼다(설계 §5). 받지 못하면(그 노드가 오프라인이다, 파일이 없다 등)
    /// 경고만 남기고 이 노드의 파일(없으면 템플릿)로 켠다. 도는 VM에 다시 붙거나 마이그레이션을
    /// 넘겨받을 때는 부르지 않는다(QEMU가 이미 이 노드의 파일을 쓴다).
    async fn fetch_firmware(&self, vm: &Vm) {
        let (Some(fetcher), Some(node)) = (&self.config.firmware, vm.status.firmware_node) else {
            return;
        };
        if vm.spec.firmware != Firmware::Uefi || node == self.config.node {
            return;
        }
        match fetcher.fetch(self.id, node).await {
            Ok(()) => tracing::info!(vm = %self.id, from = %node, "fetched UEFI variables"),
            Err(e) => {
                tracing::warn!(vm = %self.id, from = %node, error = %e, "cannot fetch UEFI variables; starting with this node's copy");
            }
        }
    }

    /// UEFI VM이 이 노드의 변수 파일로 돈다: 이 노드를 `firmware_node`로 남긴다. BIOS VM은 그대로 둔다.
    fn own_firmware(&self, vm: &Vm, status: &mut VmStatus) {
        if vm.spec.firmware == Firmware::Uefi {
            status.firmware_node = Some(self.config.node);
        }
    }

    /// 켜져 있을 수 있는 디스크 볼륨을 끈다(QEMU가 끝난 뒤). 끄지 못하면 경고만 남긴다(켜진 채 남은
    /// 볼륨은 Task 실행기의 정리가 다시 끈다).
    async fn deactivate_disks(&mut self, vm: &Vm) {
        // 끄면 공유도 풀린다.
        self.disks_shared = false;
        if !std::mem::take(&mut self.disks_active) {
            return;
        }
        for disk in &vm.spec.disks {
            let found = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .and_then(|volume| {
                    let Some(volume) = volume else {
                        return Ok(None);
                    };
                    let pool = self
                        .cluster
                        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                    Ok(pool.map(|pool| (volume, pool)))
                });
            let (volume, pool) = match found {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                    continue;
                }
            };
            if let Err(e) = self
                .config
                .storage
                .deactivate_volume(&pool, volume.id, volume.spec.format)
                .await
            {
                tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
            }
        }
    }

    /// 도는 VM의 디스크 볼륨을 다시 이 노드만 쓰게 한다(공유 LVM은 배타 잠금). 마이그레이션의 다른
    /// 쪽 노드가 아직 볼륨을 끄지 않았으면(원본이 정리 전이다) 잠시 뒤 다시 한다. 그동안에도 VM은
    /// 돈다.
    async fn exclusive_disks(&mut self, vm: &Vm) {
        match set_disks_shared(&self.cluster, vm, &*self.config.storage, false).await {
            Ok(()) => self.disks_shared = false,
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::Cluster(_))) => {
                tracing::info!(vm = %self.id, error = %e, "disks are still shared with another node; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
            Err(e) => {
                tracing::warn!(vm = %self.id, error = %e, "cannot make disks exclusive to this node again");
                self.disks_shared = false;
            }
        }
    }

    /// 끝나지 않은 채 남은 마이그레이션 표시(`Migrating`, `status.migration`)를 지운다. 이 노드의
    /// 마이그레이션 Task가 다루지 않는 VM이므로, 원본 opld가 확정 전에 죽었거나(원본 노드) 확정 뒤
    /// 넘겨받는 중이다(대상 노드). 읽은 revision으로만 쓰므로 그 사이 확정되었으면(남은 확정 쓰기가
    /// 먼저 적용됨) 쓰지 않고 `None`이다. 쓰기 전에 이 노드의 QEMU가
    /// 아직 보내고 있으면 취소하고(나중에 끝나 원본이 멈추지 않게, 남은 디스크 미러도 치운다 — 실패하면
    /// 표시를 남긴 채 오류로 돌아가 다음에 다시 한다. 다만 QEMU에 붙지 못했거나(`NotAttached`) 이 VM에서
    /// [`CANCEL_ATTEMPTS`]번 잇달아 실패하면 경고 뒤 표시를 지운다: 남은 미러는 대상의 정리가 받는 QEMU를
    /// 끄면 실패하고 다음 미러·백업 정리가 치운다. 확정 쓰기는 마이그레이션이 끝난 뒤에만 오므로 끝나지
    /// 않은 마이그레이션을 취소해도 확정과 겹치지 않는다), 쓰고 나서 마이그레이션을 보내고 멈춰 있으면
    /// (`postmigrate`, 취소하는 사이에 끝난 경우 포함) 이어서 실행한다(HA VM은 reconcile이 리스를 확인한
    /// 뒤에). 대상의 QEMU는 확정 뒤에만
    /// 돌므로 두 QEMU가 함께 돌지 않는다. 다시 읽은 VM을 돌려준다.
    /// 이 노드로 확정된 마이그레이션이 디스크를 복사했으면 표시를 지우기 전에 볼륨 기록을 옮긴
    /// 위치로 고친다(`apply_disk_moves`, 멱등). 못 고치면 오류로 돌아가 다시 하고, 그동안 디스크를
    /// 해석하지 않는다.
    async fn clear_migration(&mut self, vm: &Vm) -> Result<Option<Vm>, StepError> {
        apply_disk_moves(&self.cluster, vm).await?;
        if let Err(e) = self.hv.cancel_migration(self.id).await {
            self.cancel_failures += 1;
            if !matches!(e, DriverError::NotAttached(_)) && self.cancel_failures < CANCEL_ATTEMPTS {
                return Err(e.into());
            }
            tracing::warn!(vm = %self.id, error = %e, "cannot cancel a migration left behind; clearing its mark anyway (the target's cleanup ends leftover disk mirrors)");
        }
        self.cancel_failures = 0;
        let mut status = vm.status.clone();
        status.migration = None;
        if status.phase == VmPhase::Migrating {
            status.phase = VmPhase::Running;
        }
        match self
            .cluster
            .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => return Ok(None),
            r => r?,
        };
        // HA VM은 reconcile이 리스와 watchdog를 확인한 뒤 이어서 실행한다(이 노드에서 도는 VM의 원본).
        if !vm.spec.ha_active()
            && let Some(state) = self.hv.inspect(self.id).await?
            && state.status == postmigrate()
        {
            tracing::warn!(vm = %self.id, "resuming the source of a migration that did not complete");
            self.hv.resume(self.id).await?;
        }
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        Ok(vm.filter(|vm| vm.spec.node == Some(self.config.node)))
    }

    /// VM의 런타임 정의. 디스크 볼륨을 활성화한다.
    async fn resolve(&self, vm: &Vm) -> Result<VmRuntimeSpec, ResolveError> {
        let c = &self.config;
        runtime_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage).await
    }

    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    /// 방화벽 규칙은 부르는 쪽이 먼저 넣는다([`Worker::sync_firewall`]).
    /// QEMU가 이미 디스크를 열었으므로 크기 늘리기 Task를 기다리지 않는다. HA VM은 디스크를 해석한 뒤
    /// 다시 본다([`Worker::ha_recheck`]).
    async fn reattach(&self, vm: &Vm) -> Result<(), ContinueError> {
        let c = &self.config;
        let unresponsive = |e: &dyn std::fmt::Display| ContinueError::Unresponsive(e.to_string());
        let spec = running_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage)
            .await
            .map_err(|e| unresponsive(&e))?;
        match self.ha_recheck(vm) {
            Ok(Recheck::Go) => {}
            Ok(Recheck::NotFresh(reason)) => return Err(ContinueError::NotFresh(reason)),
            Ok(Recheck::Changed(current)) => return Err(ContinueError::Changed(current)),
            Err(e) => return Err(unresponsive(&e)),
        }
        self.hv
            .start(&spec, None)
            .await
            .map_err(|e| unresponsive(&e))
    }

    /// 보안 그룹을 건 NIC가 있으면 멈춘 채 남은 VM을 이어서 실행하거나 다시 붙기 전에 방화벽 규칙을
    /// 넣는다. 넣지 못하면 이어서 실행하지 않고 다음 차례에 다시 본다.
    async fn sync_firewall(&self, vm: &Vm) -> Result<(), ContinueError> {
        if !vm.spec.has_firewall() {
            return Ok(());
        }
        let unresponsive = |e: &dyn std::fmt::Display| ContinueError::Unresponsive(e.to_string());
        let Some(gate) = &self.config.firewall else {
            return Err(unresponsive(
                &"firewall: this node has no firewall (nftables)",
            ));
        };
        gate.sync()
            .await
            .map_err(|e| unresponsive(&format!("firewall: {e}")))
    }

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)과 `schedule_error`는
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. `firmware_node`도 이 작업자가 바꾸지 않았으면
    /// 저장소의 것을 지킨다(스냅샷 되돌리기가 쓴다). `rule_violations`는 리더가 쓰므로 늘, `ha`(HA 기록)는
    /// 이 작업자가 바꾼 필드만 저장소의 값 위에 다시 적용한다([`merge_ha`]). 나머지 필드는 전처럼 이 작업자가
    /// 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        let keep_firmware = status.firmware_node == vm.status.firmware_node;
        let read_ha = vm.status.ha.clone().unwrap_or_default();
        let mine = status.ha.clone().unwrap_or_default();
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            if keep_firmware {
                status.firmware_node = vm.status.firmware_node;
            }
            status.rule_violations = vm.status.rule_violations.clone();
            status.ha = merge_ha(&read_ha, &mine, vm.status.ha.clone());
            match self
                .cluster
                .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
                .await
            {
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                    match self.cluster.get::<VmSpec>(self.id)? {
                        Some(current) if current.spec.node == Some(self.config.node) => {
                            vm = current
                        }
                        // 지워졌거나 그 사이 다른 노드로 옮겨졌다(HA 복구 등). 그 노드의 status를
                        // 덮지 않는다.
                        _ => return Ok(()),
                    }
                }
                r => return r.map(|_| ()).map_err(Into::into),
            }
        }
    }

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, mut vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
        if vm.spec.power == PowerState::Running {
            let mut spec = vm.spec.clone();
            spec.power = PowerState::Stopped;
            // 이 단계에서 읽은 뒤 spec이 바뀌었으면 RevisionMismatch로 거부된다. 다음 단계에서
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            // status는 방금 쓴 revision으로 곧바로 이어 쓴다.
            vm.revision = self
                .cluster
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
            schedule_error: vm.status.schedule_error.clone(),
            migration: None,
            firmware_node: vm.status.firmware_node,
            rule_violations: vm.status.rule_violations.clone(),
            ha: vm.status.ha.clone(),
        };
        self.write_status(status, vm).await?;
        self.power_off = None;
        self.backoff = None;
        self.failures.clear();
        Ok(())
    }
}

/// status의 HA 기록을 고친다. 기본값이 되면 기록을 뺀다.
fn clear_ha(status: &mut VmStatus, change: impl FnOnce(&mut VmHaStatus)) {
    if let Some(ha) = status.ha.as_mut() {
        change(ha);
        if *ha == VmHaStatus::default() {
            status.ha = None;
        }
    }
}

/// 처음 읽은 HA 기록 `read`에서 이 작업자가 바꾼 필드(`mine`과 다른 필드)만 저장소의 값 `stored` 위에 다시
/// 적용한다. 그 사이 리더가 쓴 값(포기한 오류 문구 등)은 이 작업자가 그 필드를 바꾸지 않았으면 남는다.
fn merge_ha(
    read: &VmHaStatus,
    mine: &VmHaStatus,
    stored: Option<VmHaStatus>,
) -> Option<VmHaStatus> {
    let mut ha = stored.unwrap_or_default();
    if mine.relocate_from != read.relocate_from {
        ha.relocate_from = mine.relocate_from;
    }
    if mine.relocations != read.relocations {
        ha.relocations = mine.relocations.clone();
    }
    if mine.error != read.error {
        ha.error = mine.error.clone();
    }
    (ha != VmHaStatus::default()).then_some(ha)
}

/// `started` HA VM의 게스트 감시 시간(`ha.guest_watch`). 없으면 감시하지 않는다.
fn guest_watch(vm: &Vm) -> Option<Duration> {
    let ha = vm
        .spec
        .ha
        .as_ref()
        .filter(|ha| ha.state == HaState::Started)?;
    ha.guest_watch
        .map(|w| Duration::from_secs(u64::from(w.timeout_secs)))
}

/// 모든 노드(위트니스·관찰 노드 포함)가 `ha-placement`를 알린다. 읽지 못하면 아니라고 본다.
fn ha_placement_everywhere(cluster: &Cluster) -> bool {
    cluster.list::<NodeSpec>().is_ok_and(|(_, nodes)| {
        nodes
            .iter()
            .all(|n| n.status.features.iter().any(|f| f == HA_PLACEMENT))
    })
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 리더가 포기하며 오류를 쓴 사이 이 작업자가 꺼진 VM의 요청만 지웠으면 리더의 오류와 기록을 지킨다.
    #[test]
    fn merging_ha_reapplies_only_the_fields_this_worker_changed() {
        let node = Uuid::now_v7();
        let read = VmHaStatus {
            relocate_from: Some(node),
            relocations: vec![10],
            relocated_from: None,
            error: None,
        };
        let mine = VmHaStatus {
            relocate_from: None,
            ..read.clone()
        };
        let leader = VmHaStatus {
            relocate_from: None,
            relocations: vec![10],
            relocated_from: Some(Uuid::now_v7()),
            error: Some("gave up after 1 relocations".into()),
        };
        assert_eq!(
            merge_ha(&read, &mine, Some(leader.clone())),
            Some(leader.clone())
        );
        // 처음 쓸 때(저장소가 읽은 그대로)는 이 작업자의 값이다.
        assert_eq!(
            merge_ha(&read, &mine, Some(read.clone())),
            Some(mine.clone())
        );
        // 이 작업자가 바꾸지 않았으면 저장소의 값 그대로다(없으면 없음).
        assert_eq!(merge_ha(&read, &read, Some(leader.clone())), Some(leader));
        assert_eq!(merge_ha(&read, &read, None), None);
        // 모두 기본으로 돌아가면 쓰지 않는다.
        let empty = VmHaStatus {
            relocate_from: Some(node),
            ..Default::default()
        };
        assert_eq!(
            merge_ha(&empty, &VmHaStatus::default(), Some(empty.clone())),
            None
        );
    }
}
