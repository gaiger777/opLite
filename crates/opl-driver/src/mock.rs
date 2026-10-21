use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다. `stop(Acpi)` 요청에 게스트가 응한 경우도 이것으로 흉내 낸다.
    pub fn guest_shutdown(&self, id: Uuid) {
        self.remove(
            id,
            EventKind::Stopped {
                guest_initiated: true,
            },
        );
    }

    /// 프로세스가 비정상 종료한 것처럼 만든다.
    pub fn crash(&self, id: Uuid) {
        self.remove(id, EventKind::Crashed);
    }

    /// 실행 중인 VM을 살아 있지만 QMP로 다룰 수 없는 VM으로 만들거나(true) 되돌린다(false).
    /// 실제 드라이버처럼 조회에는 `unresponsive`로 드러나고, `start`와 `stop(Acpi)`는 다시 붙지
    /// 못해 실패하며, `stop(Force)`는 프로세스를 죽이므로 종료 이벤트 없이 사라진다.
    pub fn set_unresponsive(&self, id: Uuid, unresponsive: bool) {
        let mut st = self.state.lock().unwrap();
        if unresponsive {
            st.unresponsive.insert(id);
        } else {
            st.unresponsive.remove(&id);
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        st.unresponsive.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&id) && st.unresponsive.contains(&id) {
                // 실제 드라이버는 다시 붙지 못한 VM에 ACPI 요청을 보낼 수 없고, 강제 종료는
                // 프로세스를 죽이므로 종료 이벤트가 없다.
                return match mode {
                    StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                    StopMode::Force => {
                        st.unresponsive.remove(&id);
                        st.vms.remove(&id);
                        Ok(())
                    }
                };
            }
        }
        match mode {
            // 실제 QEMU처럼 요청만 보낸다. 게스트는 무시할 수 있으므로 실제 종료는
            // 테스트가 `guest_shutdown`으로 일으킨다.
            StopMode::Acpi => {}
            StopMode::Force => self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            ),
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(&st, id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(&st, id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        if self.state.lock().unwrap().vms.contains_key(&id) {
            Ok(PathBuf::from(format!("/mock/{id}/vnc.sock")))
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};

    use super::*;

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    #[tokio::test]
    async fn start_is_idempotent_and_emits_one_event() {
        let hv = MockHypervisor::default();
        let mut rx = hv.subscribe();
        let vm = spec();
        hv.start(&vm).await.unwrap();
        hv.start(&vm).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Started
            }
        );
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stop_removes_vm_and_is_idempotent() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: false
            }
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(matches!(
            hv.console_endpoint(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Acpi).await.unwrap();
        // 게스트가 요청을 무시할 수 있으므로 VM은 그대로 돌고 이벤트도 없다.
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);

        hv.guest_shutdown(vm.id);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: true
            }
        );
    }

    #[tokio::test]
    async fn unresponsive_vm_cannot_be_controlled_but_can_be_killed() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        let mut rx = hv.subscribe();
        hv.set_unresponsive(vm.id, true);

        let unresponsive = ActualState {
            id: vm.id,
            pid,
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(unresponsive.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![unresponsive]);
        assert!(matches!(hv.start(&vm).await, Err(DriverError::Timeout(_))));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a).await.unwrap();
        hv.start(&b).await.unwrap();
        let mut rx = hv.subscribe();
        hv.crash(a.id);
        hv.guest_shutdown(b.id);
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: a.id,
                kind: EventKind::Crashed
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: b.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.list().await.unwrap().is_empty());
    }
}
