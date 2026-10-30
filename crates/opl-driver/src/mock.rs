use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::QmpError;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
use crate::migration::{MigrateOptions, MigrateTarget};

/// 따로 정하지 않았을 때 `qemu_version`이 돌려주는 버전
pub const MOCK_QEMU_VERSION: &str = "8.2.2";

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
    /// 멈춘 `snapshot_save`가 들어왔다
    save_reached: Notify,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// `start`가 이만큼 기다린 뒤에 동작한다.
    start_delay: Option<Duration>,
    /// 다음 `snapshot_save`·`snapshot_delete` 한 번을 실패시킬 오류
    fail_next_snapshot: Option<DriverError>,
    /// 다음 `snapshot_save` 한 번을 VM이 꺼질 때까지 멈춘다.
    hang_next_save: bool,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
    console_dir: Option<PathBuf>,
    /// 새로 띄우려 한 VM과 불러오려 한 스냅샷 태그 (실패한 시도 포함)
    launches: Vec<(Uuid, Option<String>)>,
    /// VM별 내부 스냅샷 (태그, 메모리 포함)
    snapshots: HashMap<Uuid, Vec<(String, bool)>>,
    /// `block_resize`로 바꾼 (VM, 디스크 번호)별 크기
    disk_sizes: HashMap<(Uuid, usize), u64>,
    /// `pause`로 멈추고 아직 `resume`하지 않은 VM
    paused: HashSet<Uuid>,
    /// 멈춘 채 남은 표시(`paused-by-opl`)가 있는 VM(이전 opld가 멈춘 채 죽었다)
    marked: HashSet<Uuid>,
    /// `pause`·`resume` 호출 (VM, 이름)
    pauses: Vec<(Uuid, &'static str)>,
    /// 조회에 드러낼 상태(QEMU가 스스로 멈춘 상태 등)
    statuses: HashMap<Uuid, RuntimeStatus>,
    /// `set_qemu_version`으로 정한 QEMU 버전
    qemu_version: Option<String>,
    /// 받는 중 표시가 있는 VM(`start_incoming`으로 띄우고 아직 `finish_incoming`하지 않음)
    incoming: HashSet<Uuid>,
    /// 마이그레이션이 만든 상태: 받는 쪽 `inmigrate`·받은 뒤 멈춤, 보낸 원본 `postmigrate`.
    /// VM이 꺼지면 사라진다.
    migration_status: HashMap<Uuid, RuntimeStatus>,
    /// 다음 `migrate` 한 번을 실패시킬 오류
    fail_next_migrate: Option<DriverError>,
    /// 다음 `incoming_wait` 한 번을 실패시킬 오류. 실제 QEMU처럼 받는 QEMU는 끝난다.
    fail_next_incoming: Option<DriverError>,
    /// `migrate` 호출 (VM, 보낸 곳). 실패한 호출 포함
    migrations: Vec<(Uuid, MigrateTarget)>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
            save_reached: Notify::new(),
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 이후의 `start`가 `delay`만큼 기다린 뒤에 동작하게 한다. 시작 도중에 일어나는 일을 흉내 낸다.
    pub fn delay_starts(&self, delay: Duration) {
        self.state.lock().unwrap().start_delay = Some(delay);
    }

    /// 다음 `snapshot_save`나 `snapshot_delete` 한 번을 이 오류로 실패시킨다. `snapshot_save`는
    /// 태그를 남긴 채 실패한다(일부 디스크에 만들었거나, 시간 초과로 작업이 아직 돌고 있는 것처럼).
    pub fn fail_next_snapshot(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_snapshot = Some(err);
    }

    /// 다음 `snapshot_save` 한 번이 VM이 꺼질 때까지 끝나지 않게 한다. 실제 QEMU의 메모리 저장처럼
    /// 그동안 VM을 강제로 끄면 QMP가 끊긴 오류로 실패하고 태그를 남기지 않는다.
    pub fn hang_next_snapshot_save(&self) {
        self.state.lock().unwrap().hang_next_save = true;
    }

    /// 멈춘 `snapshot_save`가 들어올 때까지 기다린다.
    pub async fn snapshot_save_reached(&self) {
        self.save_reached.notified().await;
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

    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }

    /// 새로 띄우려 한 VM과 `start`에 준 `restore` 태그를 순서대로 돌려준다. 이미 실행 중이어서
    /// 띄우지 않은 `start`는 빠지고, 주입한 오류로 실패한 시도는 들어간다.
    pub fn launches(&self) -> Vec<(Uuid, Option<String>)> {
        self.state.lock().unwrap().launches.clone()
    }

    /// 실행 중에 `snapshot_save`로 만들고 지우지 않은 스냅샷 (태그, 메모리 포함). VM이 꺼져도 남는다.
    pub fn snapshots(&self, id: Uuid) -> Vec<(String, bool)> {
        let st = self.state.lock().unwrap();
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// `block_resize`로 마지막에 바꾼 크기
    pub fn disk_size(&self, id: Uuid, disk: usize) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .disk_sizes
            .get(&(id, disk))
            .copied()
    }

    /// `pause`로 멈추고 아직 `resume`하지 않았는지
    pub fn is_paused(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().paused.contains(&id)
    }

    /// 이전 opld가 `pause`한 채 죽은 것처럼 VM을 표시와 함께 멈춘다. `resume_left_stopped`가
    /// 이어서 실행한다(지금 `pause`해 둔 VM은 표시가 없는 것으로 본다).
    pub fn leave_paused(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        st.paused.insert(id);
        st.marked.insert(id);
    }

    /// VM에 부른 `pause`·`resume`(실패한 호출 포함)을 순서대로 돌려준다.
    pub fn pause_calls(&self, id: Uuid) -> Vec<&'static str> {
        let st = self.state.lock().unwrap();
        st.pauses
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, call)| *call)
            .collect()
    }

    /// 조회(`inspect`·`list`)가 VM의 상태를 `status`로 알리게 한다(`None`이면 되돌린다). QEMU가 스스로
    /// 멈춘 상태(`io-error` 등)를 흉내 낸다. QMP로 다룰 수 없는 VM은 그대로 `unresponsive`다.
    pub fn set_status(&self, id: Uuid, status: Option<RuntimeStatus>) {
        let mut st = self.state.lock().unwrap();
        match status {
            Some(status) => st.statuses.insert(id, status),
            None => st.statuses.remove(&id),
        };
    }

    /// `qemu_version`·`running_qemu_version`이 돌려줄 버전(기본 [`MOCK_QEMU_VERSION`])
    pub fn set_qemu_version(&self, version: &str) {
        self.state.lock().unwrap().qemu_version = Some(version.to_string());
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. 원본은 그대로 돈다.
    pub fn fail_next_migrate(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate = Some(err);
    }

    /// 다음 `incoming_wait` 한 번을 이 오류로 실패시킨다. 받는 QEMU는 크래시로 끝난다.
    pub fn fail_next_incoming_wait(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_incoming = Some(err);
    }

    /// `migrate` 호출(실패 포함)을 순서대로 돌려준다.
    pub fn migrations(&self) -> Vec<(Uuid, MigrateTarget)> {
        self.state.lock().unwrap().migrations.clone()
    }

    /// 받는 중 표시가 있는지
    pub fn is_incoming(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().incoming.contains(&id)
    }

    fn version(st: &MockState) -> String {
        st.qemu_version
            .clone()
            .unwrap_or_else(|| MOCK_QEMU_VERSION.to_string())
    }

    /// 실제 드라이버처럼 실행 중인 VM만 다룬다. 살아 있지만 QMP로 다룰 수 없으면 다시 시도할 오류다.
    fn controllable(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        if !st.vms.contains_key(&id) {
            Err(DriverError::NotRunning(id))
        } else if st.unresponsive.contains(&id) {
            Err(DriverError::NotAttached(id))
        } else {
            Ok(())
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        st.unresponsive.remove(&id);
        st.paused.remove(&id);
        st.marked.remove(&id);
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else if let Some(status) = st.statuses.get(&id) {
            status.clone()
        } else if let Some(status) = st.migration_status.get(&id) {
            status.clone()
        } else if st.paused.contains(&id) {
            RuntimeStatus::Paused
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        let delay = self.state.lock().unwrap().start_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            st.launches.push((vm.id, restore.map(str::to_string)));
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
                        st.paused.remove(&id);
                        st.incoming.remove(&id);
                        st.migration_status.remove(&id);
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
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let hang = {
            let mut st = self.state.lock().unwrap();
            Self::controllable(&st, id)?;
            std::mem::take(&mut st.hang_next_save)
        };
        if hang {
            self.save_reached.notify_one();
            while self.state.lock().unwrap().vms.contains_key(&id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            return Err(DriverError::Qmp(QmpError::Disconnected));
        }
        let mut st = self.state.lock().unwrap();
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), memory));
        match st.fail_next_snapshot.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_snapshot.take() {
            return Err(err);
        }
        if let Some(snapshots) = st.snapshots.get_mut(&id) {
            snapshots.retain(|(t, _)| t != tag);
        }
        Ok(())
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.disk_sizes.insert((id, disk), size_bytes);
        Ok(())
    }

    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "pause"));
        Self::controllable(&st, id)?;
        st.paused.insert(id);
        Ok(())
    }

    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "resume"));
        Self::controllable(&st, id)?;
        st.paused.remove(&id);
        st.marked.remove(&id);
        // 마이그레이션을 보낸 원본도 `cont`로 다시 돈다.
        if st.migration_status.get(&id) == Some(&postmigrate()) {
            st.migration_status.remove(&id);
        }
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        if Self::controllable(&st, id).is_err() {
            return Ok(false);
        }
        if st.marked.remove(&id) {
            st.paused.remove(&id);
        }
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        Ok(Self::version(&self.state.lock().unwrap()))
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Ok(Self::version(&st))
    }

    /// 범위의 첫 포트를 돌려준다. `fail_next_start`로 주입한 오류로 실패할 수 있다.
    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        _listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&vm.id) {
                return Err(DriverError::Migration(format!(
                    "VM {} is already running on this node",
                    vm.id
                )));
            }
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
            st.incoming.insert(vm.id);
            st.migration_status.insert(vm.id, inmigrate());
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(*ports.start())
    }

    /// 주입한 실패가 없으면 받기를 마친 것으로 본다(멈춘 채).
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains(&id) {
            return Err(DriverError::Migration(format!(
                "VM {id} is not receiving a migration"
            )));
        }
        if let Some(err) = st.fail_next_incoming.take() {
            drop(st);
            self.remove(id, EventKind::Crashed);
            return Err(err);
        }
        st.migration_status.insert(id, RuntimeStatus::Paused);
        Ok(())
    }

    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains(&id) {
            return Ok(());
        }
        if st.migration_status.get(&id) == Some(&inmigrate()) {
            return Err(DriverError::Migration(format!(
                "incoming migration of VM {id} has not completed"
            )));
        }
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<Uuid>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .incoming
            .iter()
            .filter(|id| st.vms.contains_key(id))
            .copied()
            .collect())
    }

    /// 주입한 실패가 없으면 원본을 `postmigrate`로 멈춘다.
    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        _opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.migrations.push((id, target.clone()));
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_migrate.take() {
            return Err(err);
        }
        st.migration_status.insert(id, postmigrate());
        Ok(())
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let migrated = {
            let st = self.state.lock().unwrap();
            st.vms.contains_key(&id) && st.migration_status.get(&id) == Some(&postmigrate())
        };
        if migrated {
            self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            );
        }
        Ok(migrated)
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

fn inmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("inmigrate".into())
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
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
        hv.start(&vm, None).await.unwrap();
        hv.start(&vm, None).await.unwrap();
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
        hv.start(&vm, None).await.unwrap();
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
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm, None).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm, None).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
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
        hv.start(&vm, None).await.unwrap();
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
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::Timeout(_))
        ));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm, None).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn records_restore_tag_of_each_launch() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, Some("s1")).await.unwrap();
        // 이미 실행 중이면 새로 띄우지 않으므로 기록하지 않는다.
        hv.start(&vm, None).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.launches(),
            vec![(vm.id, Some("s1".to_string())), (vm.id, None)]
        );
    }

    #[tokio::test]
    async fn snapshot_ops_need_a_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        for res in [
            hv.snapshot_save(vm.id, "s", true).await,
            hv.snapshot_delete(vm.id, "s").await,
            hv.block_resize(vm.id, 0, 1 << 30).await,
            hv.pause(vm.id).await,
            hv.resume(vm.id).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }

        hv.start(&vm, None).await.unwrap();
        hv.snapshot_save(vm.id, "mem", true).await.unwrap();
        hv.snapshot_save(vm.id, "disk", false).await.unwrap();
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        // 없는 태그를 지워도 성공한다.
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        assert_eq!(hv.snapshots(vm.id), vec![("disk".to_string(), false)]);
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        assert_eq!(hv.disk_size(vm.id, 1), Some(2 << 30));
        assert_eq!(hv.disk_size(vm.id, 0), None);
        hv.pause(vm.id).await.unwrap();
        assert!(hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Paused);
        hv.resume(vm.id).await.unwrap();
        assert!(!hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // QMP로 다룰 수 없는 VM은 다시 시도할 수 있는 오류로 끝난다.
        hv.set_unresponsive(vm.id, true);
        let err = hv.snapshot_save(vm.id, "x", false).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn injected_snapshot_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        // 저장은 태그를 남긴 채 실패한다.
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_save(vm.id, "s", true).await.is_err());
        assert_eq!(hv.snapshots(vm.id), vec![("s".to_string(), true)]);
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_delete(vm.id, "s").await.is_err());
        assert_eq!(hv.snapshots(vm.id).len(), 1);
        hv.snapshot_delete(vm.id, "s").await.unwrap();
        assert!(hv.snapshots(vm.id).is_empty());
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60000,
            tls_hostname: "10.0.0.2".into(),
        }
    }

    fn opts() -> MigrateOptions {
        MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(60),
        }
    }

    /// 원본 노드와 대상 노드의 하이퍼바이저로 한 번의 마이그레이션을 흉내 낸다.
    #[tokio::test]
    async fn migration_between_two_mocks() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        assert_eq!(src.qemu_version().await.unwrap(), MOCK_QEMU_VERSION);
        assert!(matches!(
            src.running_qemu_version(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
        src.start(&vm, None).await.unwrap();
        assert_eq!(
            src.running_qemu_version(vm.id).await.unwrap(),
            dst.qemu_version().await.unwrap()
        );

        let mut events = dst.subscribe();
        let port = dst
            .start_incoming(&vm, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        assert_eq!(port, 60000);
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Started);
        assert!(dst.is_incoming(vm.id));
        assert_eq!(dst.list_incoming().await.unwrap(), vec![vm.id]);
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            st.vms
                .get(&vm.id)
                .map(|&pid| MockHypervisor::state_of(&st, vm.id, pid).status)
        };
        assert_eq!(status(&dst), Some(inmigrate()));
        // 아직 받는 중이면 넘겨받을 수 없다.
        assert!(matches!(
            dst.finish_incoming(vm.id).await,
            Err(DriverError::Migration(_))
        ));
        // 이미 돌고 있는 VM은 받을 수 없다.
        assert!(matches!(
            src.start_incoming(&vm, "10.0.0.1", 60000..=60099).await,
            Err(DriverError::Migration(_))
        ));

        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(src.migrations(), vec![(vm.id, target())]);
        assert_eq!(status(&src), Some(postmigrate()));
        dst.incoming_wait(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Paused));
        dst.finish_incoming(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Running));
        assert!(!dst.is_incoming(vm.id));
        assert!(dst.list_incoming().await.unwrap().is_empty());
        // 두 번째 넘겨받기는 아무것도 하지 않는다.
        dst.finish_incoming(vm.id).await.unwrap();
        // 받는 중 표시가 없으면 기다릴 것이 없다.
        assert!(matches!(
            dst.incoming_wait(vm.id).await,
            Err(DriverError::Migration(_))
        ));

        // 돌고 있는 VM은 끄지 않는다.
        assert!(!dst.stop_migrated(vm.id).await.unwrap());
        assert!(src.stop_migrated(vm.id).await.unwrap());
        assert_eq!(src.inspect(vm.id).await.unwrap(), None);
        assert!(!src.stop_migrated(vm.id).await.unwrap());
    }

    #[tokio::test]
    async fn failed_migration_keeps_source_and_receiver_exits() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        src.start(&vm, None).await.unwrap();
        src.fail_next_migrate(DriverError::Migration("Connection refused".into()));
        assert!(matches!(
            src.migrate(vm.id, &target(), &opts()).await,
            Err(DriverError::Migration(_))
        ));
        let state = src.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // 받기에 실패한 QEMU는 크래시로 끝난다.
        dst.start_incoming(&vm, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        let mut events = dst.subscribe();
        dst.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
        assert!(dst.incoming_wait(vm.id).await.is_err());
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Crashed);
        assert!(dst.list_incoming().await.unwrap().is_empty());

        // 시작 실패를 주입하면 받기도 실패한다.
        dst.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            dst.start_incoming(&vm, "10.0.0.2", 60000..=60099).await,
            Err(DriverError::StartFailed(_))
        ));
        assert_eq!(dst.inspect(vm.id).await.unwrap(), None);

        // 보낸 뒤 확정 전에 실패하면 원본을 이어서 실행한다.
        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            postmigrate()
        );
        src.resume(vm.id).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            RuntimeStatus::Running
        );
        assert!(!src.stop_migrated(vm.id).await.unwrap());

        // 받는 쪽에서 버전을 다르게 보이게 할 수 있다.
        dst.set_qemu_version("9.0.0");
        assert_eq!(dst.qemu_version().await.unwrap(), "9.0.0");
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a, None).await.unwrap();
        hv.start(&b, None).await.unwrap();
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
