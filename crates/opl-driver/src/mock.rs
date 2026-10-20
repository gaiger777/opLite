use std::collections::HashMap;
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
    /// 다음 `start` 한 번을 이 오류로 실패시킨다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다.
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

    fn remove(&self, id: Uuid, kind: EventKind) {
        if self.state.lock().unwrap().vms.remove(&id).is_some() {
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(id: Uuid, pid: u32) -> ActualState {
        ActualState {
            id,
            pid,
            status: RuntimeStatus::Running,
        }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            if st.vms.contains_key(&vm.id) {
                return Ok(());
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
        // 실제 QEMU에서도 ACPI 종료는 게스트가 스스로 꺼진 것(guest: true)으로 보고된다.
        let guest_initiated = mode == StopMode::Acpi;
        self.remove(id, EventKind::Stopped { guest_initiated });
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(id, pid))
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
