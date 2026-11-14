//! HA VM과 리스(설계 §3.3 워커 규칙): 켜기·`cont` 문턱, 리스를 잃으면 강제 종료, 펜싱이 꺼진 노드,
//! 도는 HA VM 알림(`set_running`).

mod common;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use async_trait::async_trait;
use common::{Env, vm};
use opl_cluster::ClusterError;
use opl_driver::{
    Capacity, DriverError, EventKind, GuestAnswer, Hypervisor, RuntimeStatus, SourceFormat,
    Storage, VolumeInfo, VolumeSource,
};
use opl_reconciler::ha::{LeaseRenewer, Watchdog, WatchdogOpener};
use opl_reconciler::{FirmwareError, FirmwareFetcher, HaConfig, HaGuard, ReconcilerConfig};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{
    DiskBus, DiskSpec, GuestWatch, MigrationStatus, PowerState, RestartPolicy, Vm, VmHa,
    VmHaStatus, VmPhase, VmSpec, VmStatus,
};
use uuid::Uuid;

const WAITING: &str = "waiting for the cluster majority before starting an HA VM";
const FENCED: &str = "fenced: lost the cluster majority";
const UNAVAILABLE: &str = "HA is not available on this node";

/// 정해 둔 대로 곧바로 갱신에 성공하거나 실패한다.
struct FakeRenewer(Mutex<bool>);

impl FakeRenewer {
    fn set_ok(&self, ok: bool) {
        *self.0.lock().unwrap() = ok;
    }
}

#[async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), ClusterError> {
        if *self.0.lock().unwrap() {
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
}

impl FakeWatchdog {
    fn count(&self, what: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == what)
            .count()
    }
}

struct FakeDevice(Arc<Mutex<Vec<&'static str>>>);

impl Watchdog for FakeDevice {
    fn ping(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().push("ping");
        Ok(())
    }

    fn magic_close(self: Box<Self>) -> io::Result<()> {
        self.0.lock().unwrap().push("magic_close");
        Ok(())
    }
}

impl WatchdogOpener for FakeWatchdog {
    fn open(&self) -> io::Result<Box<dyn Watchdog>> {
        if *self.fail_open.lock().unwrap() {
            return Err(io::Error::new(io::ErrorKind::ResourceBusy, "busy"));
        }
        self.log.lock().unwrap().push("open");
        Ok(Box::new(FakeDevice(self.log.clone())))
    }
}

/// 첫 받아 오기에서 멈춘다: 켜기 문턱을 지난 뒤 `hv.start` 전이다.
#[derive(Default)]
struct GateFetcher {
    calls: Mutex<usize>,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl FirmwareFetcher for GateFetcher {
    async fn fetch(&self, _vm: Uuid, _node: Uuid) -> Result<(), FirmwareError> {
        let first = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls == 1
        };
        if first {
            self.reached.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

/// `gate`가 켜져 있으면 다음 볼륨 켜기에서 멈춘다: 도는 VM에 다시 붙으며 디스크를 해석하는 중이다.
#[derive(Default)]
struct GateStorage {
    gate: Mutex<bool>,
    reached: Notify,
    release: Notify,
}

#[async_trait]
impl Storage for GateStorage {
    async fn activate_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        let gated = std::mem::take(&mut *self.gate.lock().unwrap());
        if gated {
            self.reached.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
    async fn create_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn import_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn delete_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_info(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        unreachable!()
    }
    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        unreachable!()
    }
    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        unreachable!()
    }
    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        unreachable!()
    }
    async fn resize_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: u64,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        unreachable!()
    }
    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn copy_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
}

struct Ha {
    renewer: Arc<FakeRenewer>,
    watchdog: Arc<FakeWatchdog>,
    guard: Arc<HaGuard>,
}

const FENCE_AFTER: Duration = Duration::from_millis(300);

/// 가짜 갱신과 가짜 watchdog로 guard를 만들어 reconciler에 넣는다. 리스는 300ms 뒤에 끝난다.
/// 부하로 런타임이 그만큼 멈추면 갱신에 성공하는 동안에도 리스가 잠깐 끝나(guard의 시계로는 맞다) 도는
/// HA VM을 끄고 갱신 뒤 다시 켠다. 리스가 끝나야 하는 시험만 쓰고, 그 시험은 그런 끄기·켜기에 기대지 않는다.
async fn start(ok: bool) -> (Env, Ha) {
    start_with(ok, FENCE_AFTER, |_| {}).await
}

async fn start_with(
    ok: bool,
    fence_after: Duration,
    tweak: impl FnOnce(&FakeWatchdog),
) -> (Env, Ha) {
    start_full(ok, fence_after, tweak, |_| {}).await
}

/// `config`로 reconciler 설정도 바꾼다.
async fn start_full(
    ok: bool,
    fence_after: Duration,
    tweak: impl FnOnce(&FakeWatchdog),
    config: impl FnOnce(&mut ReconcilerConfig),
) -> (Env, Ha) {
    let renewer = Arc::new(FakeRenewer(Mutex::new(ok)));
    let watchdog = Arc::new(FakeWatchdog::default());
    tweak(&watchdog);
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after,
            renew_interval: Duration::from_millis(20),
            watchdog: Some(watchdog.clone()),
        },
    );
    let g = guard.clone();
    let env = Env::start_with(move |c: &mut ReconcilerConfig| {
        c.ha = Some(g);
        config(c);
    })
    .await;
    (
        env,
        Ha {
            renewer,
            watchdog,
            guard,
        },
    )
}

fn ha_vm(name: &str, node: Uuid) -> Vm {
    let mut vm = vm(name, node);
    vm.spec.ha = Some(VmHa::default());
    vm
}

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

fn error_is(v: &Vm, msg: &str) -> bool {
    v.status.last_error.as_deref() == Some(msg)
}

/// 리스를 갱신하기 전에는 HA VM을 켜지 않고(실패로 세지 않는다) 기다린다. 갱신되면 watchdog를 연 뒤 켠다.
#[tokio::test]
async fn ha_vm_waits_for_the_lease_before_starting() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    // 기다리는 동안 다시 보아도 켜지 않고 원하는 상태도 그대로다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(ha.watchdog.count("open"), 0);
    assert_eq!(env.get(vm.id).spec.power, PowerState::Running);

    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.hv.launches().len(), 1);
    assert_eq!(ha.watchdog.count("open"), 1);
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 리스가 끊기면 켜지 않고 기다린다. 시작하는 동안에는 도는 HA VM으로
/// 알려 두어, 리스가 없으면 watchdog를 쓰다듬지 않는다.
#[tokio::test]
async fn ha_vm_is_not_started_if_the_lease_ends_while_starting() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, ha) = start_full(true, FENCE_AFTER, |_| {}, |c| c.firmware = Some(f)).await;
    // 다른 노드의 UEFI 변수를 받아 온 뒤 켠다.
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    assert_eq!(ha.watchdog.count("open"), 1);

    ha.renewer.set_ok(false);
    ha.guard.subscribe().wait_for(|s| s.stale).await.unwrap();
    let pinged = ha.watchdog.count("ping");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(ha.watchdog.count("ping"), pinged, "pinged while starting");

    fetcher.release.notify_one();
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(got.spec.power, PowerState::Running);
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    // 켜지 않았으므로 도는 HA VM이 아니다: 다시 쓰다듬는다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while ha.watchdog.count("ping") == pinged {
        assert!(std::time::Instant::now() < deadline, "not pinging again");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 리스가 돌아오면 켠다. 켠 뒤 런타임이 멈추면 다시 끄고 켤 수 있으므로 횟수는 보지 않는다.
    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
}

/// HA가 아닌 VM은 리스와 상관없이 켠다.
#[tokio::test]
async fn non_ha_vm_starts_without_the_lease() {
    let (env, ha) = start(false).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(ha.watchdog.count("open"), 0);
}

/// watchdog를 열지 못하면 HA VM을 켜지 않고 오류를 남긴다.
#[tokio::test]
async fn ha_vm_does_not_start_when_the_watchdog_cannot_be_opened() {
    let (env, ha) = start_with(true, FENCE_AFTER, |w| *w.fail_open.lock().unwrap() = true).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "watchdog error", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.starts_with("cannot open the watchdog device"))
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert!(env.hv.launches().is_empty());

    *ha.watchdog.fail_open.lock().unwrap() = false;
    env.wait(vm.id, "running", running).await;
}

/// 리스를 잃으면 도는 HA VM을 강제로 끈다. 우리가 요청한 종료이므로 실패로 세지 않고(`restart = never`여도
/// 원하는 상태가 그대로다) 리스가 돌아오면 다시 켠다. HA가 아닌 VM은 그대로 둔다.
#[tokio::test]
async fn lost_lease_force_stops_ha_vms_and_restarts_them_when_it_returns() {
    let (env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    vm.spec.restart = RestartPolicy::Never;
    let plain = common::vm("plain", env.node);
    env.create(&vm).await;
    env.create(&plain).await;
    env.wait(vm.id, "running", running).await;
    let plain_pid = env.wait(plain.id, "running", running).await.status.pid;

    ha.renewer.set_ok(false);
    let got = env.wait(vm.id, "fenced", |v| error_is(v, FENCED)).await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(got.status.pid, None);
    assert_eq!(env.pid(vm.id).await, None);
    // set_ok(false) 전에 런타임이 멈춰 리스가 잠깐 끝났으면 web을 이미 한 번 끄고 다시 켰을 수 있다(그때는
    // 갱신 뒤라 맞다). 그래서 펜싱을 본 뒤의 횟수에서 센다: 리스가 없는 동안에는 켜지 않는다.
    let fenced_web = launches_of(&env, vm.id);
    // 이벤트를 처리한 뒤에도 실패로 남지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_ne!(got.status.phase, VmPhase::Failed);
    assert_eq!(
        launches_of(&env, vm.id),
        fenced_web,
        "web launched while fenced"
    );
    assert_eq!(launches_of(&env, plain.id), 1, "plain launched again");
    assert_eq!(env.get(plain.id).status.pid, plain_pid);
    assert_eq!(env.pid(plain.id).await, plain_pid);

    ha.renewer.set_ok(true);
    let got = env.wait(vm.id, "running again", running).await;
    assert_eq!(got.status.last_error, None);
    assert!(launches_of(&env, vm.id) > fenced_web);
    assert_eq!(launches_of(&env, plain.id), 1, "plain launched again");
}

/// `id`를 켠 횟수
fn launches_of(env: &Env, id: Uuid) -> usize {
    env.hv.launches().iter().filter(|(v, _)| *v == id).count()
}

/// 펜싱이 꺼진 노드(guard 없음)에서는 HA VM을 켜지 않는다.
#[tokio::test]
async fn ha_vm_does_not_start_on_a_node_without_fencing() {
    let env = Env::start().await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "unavailable", |v| error_is(v, UNAVAILABLE))
        .await;
    assert_ne!(got.status.phase, VmPhase::Running);
    assert!(env.hv.launches().is_empty());
}

/// 펜싱이 꺼진 노드에서 이미 도는 VM을 HA로 바꿔도 끄지 않고 그 사실만 남긴다.
#[tokio::test]
async fn running_ha_vm_on_a_node_without_fencing_is_left_running() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let current = env.get(vm.id);
    vm.spec.ha = Some(VmHa::default());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "unavailable", |v| error_is(v, UNAVAILABLE))
        .await;
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert_eq!(env.pid(vm.id).await, pid);
}

/// reconciler가 멈춰도 도는 HA VM은 도는 것으로 남는다: 정상 종료 때 watchdog를 그대로 둔다.
#[tokio::test]
async fn shutdown_keeps_running_ha_vms_armed() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (mut env, ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 0);
}

/// 멈춘 HA VM은 도는 것에서 빠진다: 정상 종료 때 watchdog를 멈춘다.
#[tokio::test]
async fn stopped_ha_vm_is_no_longer_running() {
    let (mut env, ha) = start(true).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 다른 노드로 옮긴 HA VM은 작업자가 물러나며 도는 것에서 뺀다.
#[tokio::test]
async fn ha_vm_moved_away_is_no_longer_running() {
    let (mut env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 이미 도는 VM이 HA가 되면 리스가 살아 있을 때 watchdog를 연다. 그 전에는 오류 없이 기다렸다가
/// 리스가 살아나면 연다.
#[tokio::test]
async fn running_ha_vm_arms_the_watchdog_once_the_lease_is_fresh() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let mut vm = common::vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let current = env.get(vm.id);
    vm.spec.ha = Some(VmHa::default());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert!(got.spec.ha_active());
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.last_error, None);
    assert_eq!(ha.watchdog.count("open"), 0);

    ha.renewer.set_ok(true);
    let deadline = std::time::Instant::now() + common::WAIT;
    while ha.watchdog.count("open") == 0 {
        assert!(std::time::Instant::now() < deadline, "watchdog not armed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(env.pid(vm.id).await, pid);
    assert_eq!(env.get(vm.id).status.last_error, None);
}

/// 다른 노드로 옮겨졌어도 QEMU가 아직 돌면 도는 HA VM으로 남긴다: 정상 종료 때 watchdog를 그대로 둔다.
#[tokio::test]
async fn ha_vm_moved_away_while_running_stays_running() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (mut env, ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.pid(vm.id).await, pid);
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 0);
}

/// HA VM을 다른 노드로 옮긴 마이그레이션이 성공했다: 작업자는 보류 중에 물러나고(원본 QEMU가 아직
/// 있다) 원본은 그 뒤에 멈춘다. 작업자 없이 온 그 종료 이벤트로 도는 HA VM에서 빼므로 정상 종료 때
/// watchdog를 멈춘다.
#[tokio::test]
async fn ha_vm_migrated_away_is_dropped_when_its_source_stops() {
    let (mut env, ha) = start(true).await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 마이그레이션 Task가 원본을 끄고 보류를 푼다.
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    handle.end_migration(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 옮겨 간 HA VM의 QEMU가 이벤트 없이 사라졌어도 주기 재동기화가 도는 HA VM에서 뺀다.
#[tokio::test]
async fn resync_drops_ha_vms_no_longer_on_this_node() {
    let (mut env, ha) = start_full(
        true,
        FENCE_AFTER,
        |_| {},
        |c| c.resync_interval = Duration::from_millis(200),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // QMP로 다룰 수 없는 VM의 강제 종료는 이벤트 없이 사라진다.
    env.hv.set_unresponsive(vm.id, true);
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    assert_eq!(env.pid(vm.id).await, None);
    tokio::time::sleep(Duration::from_millis(600)).await;
    env.stop_reconciler().await;
    ha.guard.close();
    assert_eq!(ha.watchdog.count("magic_close"), 1);
}

/// 다른 노드로 배정을 바꾼다.
async fn reassign(env: &Env, id: Uuid) {
    let current = env.get(id);
    let mut spec = current.spec.clone();
    spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 다른 노드로 배정됐으면(리스는 살아 있다: 과반수 쪽의 복구 쓰기가
/// 이미 적용됐다) 켜지 않고 status도 덮어쓰지 않는다.
#[tokio::test]
async fn ha_vm_reassigned_while_starting_is_not_started() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, _ha) = start_full(
        true,
        Duration::from_secs(60),
        |_| {},
        |c| c.firmware = Some(f),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    reassign(&env, vm.id).await;
    fetcher.release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(env.get(vm.id).status.node, None);
}

/// 켜기 문턱을 지난 뒤 시작하기 전에 원하는 상태가 stopped로 바뀌었으면 켜지 않는다.
#[tokio::test]
async fn ha_vm_stopped_while_starting_is_not_started() {
    let fetcher = Arc::new(GateFetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let (env, _ha) = start_full(
        true,
        Duration::from_secs(60),
        |_| {},
        |c| c.firmware = Some(f),
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    vm.status.firmware_node = Some(Uuid::now_v7());
    env.create(&vm).await;
    fetcher.reached.notified().await;
    env.set_power(vm.id, PowerState::Stopped).await;
    fetcher.release.notify_one();
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}

/// 다룰 수 없는 HA VM에 다시 붙는 동안(디스크 해석) 다른 노드로 배정되고 QEMU가 사라졌으면 새로 띄우지
/// 않는다.
#[tokio::test]
async fn ha_vm_reassigned_while_reattaching_is_not_started() {
    let storage = Arc::new(GateStorage::default());
    let s: Arc<dyn Storage> = storage.clone();
    let (env, _ha) = start_full(true, Duration::from_secs(60), |_| {}, |c| c.storage = s).await;
    let pool: StoragePool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let mut vm = ha_vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches().len(), 1);

    // QMP로 다룰 수 없게 되면 작업자가 다시 붙는다(`start`).
    env.hv.set_unresponsive(vm.id, true);
    *storage.gate.lock().unwrap() = true;
    env.handle().end_migration(vm.id);
    storage.reached.notified().await;
    reassign(&env, vm.id).await;
    // 이벤트 없이 사라진다.
    env.hv
        .stop(vm.id, opl_driver::StopMode::Force)
        .await
        .unwrap();
    storage.release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.hv.launches().len(), 1, "{:?}", env.hv.launches());
    assert_eq!(env.pid(vm.id).await, None);
}

/// 켜는 동안 리스가 끝나고 다른 노드로 옮겨졌으면, 켜진 QEMU를 강제로 끄고 도는 HA VM에서 뺀다.
#[tokio::test]
async fn ha_vm_moved_away_after_the_lease_ended_is_force_stopped() {
    let (env, ha) = start(true).await;
    env.hv.delay_starts(Duration::from_secs(1));
    let mut vm = ha_vm("web", env.node);
    env.create(&vm).await;
    // 켜기 문턱을 지나 hv.start 안에 있다.
    tokio::time::sleep(Duration::from_millis(100)).await;
    ha.renewer.set_ok(false);
    ha.guard.subscribe().wait_for(|s| s.stale).await.unwrap();
    let current = env.get(vm.id);
    vm.spec.node = Some(Uuid::now_v7());
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.launches().is_empty() || env.pid(vm.id).await.is_some() {
        assert!(std::time::Instant::now() < deadline, "still running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 끈 뒤에는 도는 HA VM이 없으므로 리스가 없어도 다시 쓰다듬는다.
    let pinged = ha.watchdog.count("ping");
    while ha.watchdog.count("ping") == pinged {
        assert!(std::time::Instant::now() < deadline, "not pinging again");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 이 노드로 확정된 HA VM의 마이그레이션은 리스가 살아 있어야 이어서 실행한다(`cont`).
#[tokio::test]
async fn received_ha_vm_is_continued_only_with_the_lease() {
    // 시험하는 동안 stale이 되지 않게 리스를 길게 둔다.
    let (env, ha) = start_with(false, Duration::from_secs(60), |_| {}).await;
    let source = Uuid::now_v7();
    let mut vm = ha_vm("web", source);
    env.create(&vm).await;
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
        disks: vec![],
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    let spec = VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
        guest_agent: false,
    };
    env.hv
        .start_incoming(&spec, Uuid::now_v7(), "127.0.0.1", 60000..=60099, &[])
        .await
        .unwrap();
    env.hv.incoming_wait(vm.id).await.unwrap();

    // 원본이 확정한다. 리스를 갱신하지 못했으므로(아직 stale은 아니다) 멈춘 채 기다린다.
    let current = env.get(vm.id);
    vm.spec.node = Some(env.node);
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env.wait(vm.id, "waiting", |v| error_is(v, WAITING)).await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Paused)
    );
    assert!(env.hv.is_incoming(vm.id));

    ha.renewer.set_ok(true);
    let got = env
        .wait(vm.id, "taken over", |v| {
            running(v) && v.status.migration.is_none() && v.status.last_error.is_none()
        })
        .await;
    assert!(got.status.pid.is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.is_incoming(vm.id) {
        assert!(std::time::Instant::now() < deadline, "still incoming");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
    assert_eq!(ha.watchdog.count("open"), 1);
}

/// opld가 HA VM의 마이그레이션을 보내다 확정 전에 죽고 다시 떴다. 다시 뜬 opld의 guard는 아직 갱신하지
/// 못했다: 남은 표시를 지우고 `postmigrate`로 멈춘 원본을 리스를 기다리며(Starting) 그대로 둔다.
/// VM과 원본의 pid, 새 guard의 갱신을 돌려준다.
async fn ha_source_left_by_a_restart(env: &mut Env) -> (Vm, Option<u32>, Arc<FakeRenewer>) {
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.stop_reconciler().await;
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node: Uuid::now_v7(),
        task: Uuid::now_v7(),
        disks: vec![],
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    let target = opl_driver::MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = opl_driver::MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
        switchover_mirrors: false,
    };
    env.hv.migrate(vm.id, &target, &opts).await.unwrap();
    let renewer = Arc::new(FakeRenewer(Mutex::new(false)));
    env.config.ha = Some(HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after: Duration::from_secs(60),
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    ));
    env.start_reconciler();

    let got = env
        .wait(vm.id, "waiting", |v| {
            error_is(v, WAITING) && v.status.migration.is_none()
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let postmigrate = RuntimeStatus::Other("postmigrate".into());
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(postmigrate)
    );
    (vm, pid, renewer)
}

/// 남은 마이그레이션의 HA 원본은 리스를 갱신한 뒤에 이어서 실행한다.
#[tokio::test]
async fn ha_source_left_by_a_restart_is_resumed_only_with_the_lease() {
    let (mut env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let (vm, pid, renewer) = ha_source_left_by_a_restart(&mut env).await;

    renewer.set_ok(true);
    let got = env
        .wait(vm.id, "resumed", |v| {
            running(v) && v.status.last_error.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert_eq!(env.hv.launches().len(), 1);
}

/// 리스를 기다리는 원본의 VM이 HA가 아니게 되어도 낡은 원본으로 보고 끄지 않고 이어서 실행한다.
#[tokio::test]
async fn waiting_ha_source_is_resumed_when_ha_is_turned_off() {
    let (mut env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let (vm, pid, _renewer) = ha_source_left_by_a_restart(&mut env).await;

    let current = env.get(vm.id);
    let mut spec = current.spec.clone();
    spec.ha = None;
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "resumed", |v| {
            running(v) && v.status.last_error.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert_eq!(env.hv.launches().len(), 1, "{:?}", env.hv.launches());
}

/// 크래시 뒤 다시 켜질 때까지 기다린다(새 pid).
async fn crash_and_restart(env: &Env, id: Uuid) -> Vm {
    let before = env.get(id).status.pid;
    env.hv.crash(id);
    env.wait(id, "restarted", |v| running(v) && v.status.pid != before)
        .await
}

/// 옮겨 달라는 요청을 기다린다.
async fn wait_relocate(env: &Env, id: Uuid) -> Vm {
    let node = env.node;
    env.wait(id, "relocate request", |v| {
        v.status.ha.as_ref().and_then(|h| h.relocate_from) == Some(node)
    })
    .await
}

/// HA VM(`started`)은 `restart = never`여도 크래시 뒤 다시 켜고, `max_restart`(1)를 넘는 두 번째 크래시에는
/// 켜지 않고 리더에게 옮겨 달라고 한다(phase Failed, 원하는 상태는 그대로).
#[tokio::test]
async fn ha_vm_crashing_past_max_restart_asks_to_be_relocated() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let mut vm = ha_vm("web", env.node);
    vm.spec.restart = RestartPolicy::Never;
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    let got = crash_and_restart(&env, vm.id).await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.ha, None);
    assert_eq!(launches_of(&env, vm.id), 2);

    env.hv.crash(vm.id);
    let got = wait_relocate(&env, vm.id).await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Failed);
    assert_eq!(got.status.pid, None);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM crashed 2 times on this node; asking HA to move it")
    );
    // 요청한 VM은 다시 켜지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(launches_of(&env, vm.id), 2);
    assert_eq!(env.pid(vm.id).await, None);
    assert_eq!(env.get(vm.id).revision, got.revision);
}

/// `max_restart` 0이면 첫 크래시에 옮겨 달라고 한다.
#[tokio::test]
async fn max_restart_zero_asks_to_be_relocated_on_the_first_crash() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let mut vm = ha_vm("web", env.node);
    vm.spec.ha = Some(VmHa {
        max_restart: 0,
        ..Default::default()
    });
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.hv.crash(vm.id);
    let got = wait_relocate(&env, vm.id).await;
    assert_eq!(got.status.phase, VmPhase::Failed);
    assert_eq!(launches_of(&env, vm.id), 1);
}

/// `ha-placement`를 모르는 노드가 있으면 지금처럼 재시작 정책을 따른다(옛 리더는 요청을 모른다).
#[tokio::test]
async fn with_an_old_node_ha_vm_crashes_follow_the_restart_policy() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let old: Node = Resource::new(
        "old",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    env.cluster.create(&old).await.unwrap();
    let mut vm = ha_vm("web", env.node);
    vm.spec.ha = Some(VmHa {
        max_restart: 0,
        ..Default::default()
    });
    vm.spec.restart = RestartPolicy::Never;
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.hv.crash(vm.id);
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.last_error.as_deref(), Some("VM crashed"));
    assert_eq!(got.status.ha, None);
}

/// 게스트가 스스로 끈 HA VM은 지금처럼 끈다(크래시가 아니다).
#[tokio::test]
async fn guest_shutdown_of_an_ha_vm_powers_it_off() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let vm = ha_vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    env.hv.guest_shutdown(vm.id);
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.ha, None);
    assert_eq!(launches_of(&env, vm.id), 1);
}

/// 끈 VM의 옮기기 요청은 지우고(HA 오류는 남김), 다시 켜면 HA 오류도 지운다. 옮긴 기록은 남는다.
#[tokio::test]
async fn restarting_clears_the_relocation_request_and_the_ha_error() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let mut vm = ha_vm("web", env.node);
    let from = Uuid::now_v7();
    vm.spec.power = PowerState::Stopped;
    vm.status = VmStatus {
        phase: VmPhase::Failed,
        node: Some(env.node),
        ha: Some(VmHaStatus {
            relocate_from: Some(env.node),
            relocations: vec![100],
            relocated_from: Some(from),
            error: Some("gave up after 1 relocations".into()),
            evict: None,
        }),
        ..Default::default()
    };
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "request cleared", |v| {
            v.status
                .ha
                .as_ref()
                .is_some_and(|h| h.relocate_from.is_none())
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Failed);
    assert_eq!(
        got.status.ha.as_ref().and_then(|h| h.error.as_deref()),
        Some("gave up after 1 relocations")
    );

    env.set_power(vm.id, PowerState::Running).await;
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(
        got.status.ha,
        Some(VmHaStatus {
            relocations: vec![100],
            relocated_from: Some(from),
            ..Default::default()
        })
    );
}

/// 같은 노드 빠른 재시작: HA VM의 창 안 첫 크래시는 백오프(여기서는 3초)를 기다리지 않고 곧바로 다시 켠다.
/// 둘째부터는 지금 백오프다.
#[tokio::test]
async fn first_crash_of_an_ha_vm_restarts_at_once() {
    let (env, _ha) = start_full(
        true,
        Duration::from_secs(60),
        |_| {},
        |c| {
            c.restart.initial = Duration::from_secs(3);
            c.restart.max = Duration::from_secs(3);
        },
    )
    .await;
    let mut vm = ha_vm("web", env.node);
    vm.spec.ha = Some(VmHa {
        max_restart: 2,
        ..Default::default()
    });
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    let crashed = std::time::Instant::now();
    crash_and_restart(&env, vm.id).await;
    assert!(
        crashed.elapsed() < Duration::from_secs(1),
        "{:?}",
        crashed.elapsed()
    );
    assert_eq!(launches_of(&env, vm.id), 2);

    env.hv.crash(vm.id);
    env.wait(vm.id, "backoff", |v| {
        error_is(v, "VM crashed; retrying in 3s")
    })
    .await;
    assert_eq!(launches_of(&env, vm.id), 2);
}

/// 게스트 감시(`timeout_secs`)를 켠 HA VM
fn watched_vm(name: &str, node: Uuid, timeout_secs: u32) -> Vm {
    let mut vm = ha_vm(name, node);
    vm.spec.ha = Some(VmHa {
        guest_watch: Some(GuestWatch { timeout_secs }),
        ..Default::default()
    });
    vm
}

/// `id`의 게스트 에이전트를 몇 번 더 부를 때까지 기다린다.
async fn wait_pings(env: &Env, id: Uuid, more: usize) {
    let want = env.hv.guest_pings(id) + more;
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.guest_pings(id) < want {
        assert!(
            std::time::Instant::now() < deadline,
            "guest agent not pinged"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 게스트 감시가 없는 VM의 에이전트는 부르지 않는다. 감시하는 VM이라도 이 QEMU에서 답한 적이 없는 에이전트
/// (게스트 OS·에이전트 없음)는 시간이 넘어도 다시 켜지 않는다.
#[tokio::test]
async fn guest_agents_that_never_answered_are_not_watched() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let plain = ha_vm("plain", env.node);
    let watched = watched_vm("watched", env.node, 1);
    env.create(&plain).await;
    env.create(&watched).await;
    env.wait(plain.id, "running", running).await;
    env.wait(watched.id, "running", running).await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(env.hv.guest_pings(plain.id), 0);
    assert!(env.hv.guest_pings(watched.id) > 5);
    assert_eq!(launches_of(&env, watched.id), 1);
    assert!(running(&env.get(watched.id)));
}

/// 답하던 에이전트가 `timeout_secs` 넘게 답하지 않으면 강제로 끄고 다시 켜며 크래시 한 번으로 센다. 새 QEMU의
/// 에이전트는 다시 답할 때까지 감시하지 않고, 답한 뒤 또 멈추면 `max_restart`(1)를 넘어 옮겨 달라고 한다.
#[tokio::test]
async fn hung_guest_is_restarted_and_counted_as_a_crash() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let vm = watched_vm("web", env.node, 1);
    env.hv.set_guest_agent(vm.id, GuestAnswer::Ok);
    env.create(&vm).await;
    let first = env.wait(vm.id, "running", running).await.status.pid;
    wait_pings(&env, vm.id, 2).await;

    env.hv.set_guest_agent(vm.id, GuestAnswer::Hang);
    let hung = std::time::Instant::now();
    env.wait(vm.id, "restarted", |v| running(v) && v.status.pid != first)
        .await;
    assert!(
        hung.elapsed() >= Duration::from_secs(1),
        "{:?}",
        hung.elapsed()
    );
    assert_eq!(launches_of(&env, vm.id), 2);

    // 새 QEMU의 에이전트는 아직 답하지 않았다.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(launches_of(&env, vm.id), 2);

    env.hv.set_guest_agent(vm.id, GuestAnswer::Ok);
    wait_pings(&env, vm.id, 2).await;
    env.hv.set_guest_agent(vm.id, GuestAnswer::Hang);
    let got = wait_relocate(&env, vm.id).await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Failed);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM crashed 2 times on this node; asking HA to move it")
    );
    assert_eq!(launches_of(&env, vm.id), 2);
    assert_eq!(env.pid(vm.id).await, None);
}

/// 일시 정지한 동안은 감시 시간을 세지 않는다: 이어서 실행한 때부터 다시 센다.
#[tokio::test]
async fn paused_time_does_not_count_against_the_guest() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let vm = watched_vm("web", env.node, 1);
    env.hv.set_guest_agent(vm.id, GuestAnswer::Ok);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    wait_pings(&env, vm.id, 2).await;

    env.hv.pause(vm.id).await.unwrap();
    env.hv.set_guest_agent(vm.id, GuestAnswer::Hang);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(launches_of(&env, vm.id), 1);

    let resumed = std::time::Instant::now();
    env.hv.resume(vm.id).await.unwrap();
    env.wait(vm.id, "restarted", |_| launches_of(&env, vm.id) == 2)
        .await;
    assert!(
        resumed.elapsed() >= Duration::from_secs(1),
        "{:?}",
        resumed.elapsed()
    );
}

/// 게스트가 다시 부팅하면(QMP `RESET`, 종료 시작의 `SHUTDOWN`) 에이전트가 다시 답할 때까지 감시하지 않는다. 다시
/// 답한 뒤에는 멈추면 다시 켠다.
#[tokio::test]
async fn a_rebooting_guest_is_not_watched_until_its_agent_answers_again() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let vm = watched_vm("web", env.node, 1);
    env.hv.set_guest_agent(vm.id, GuestAnswer::Ok);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    wait_pings(&env, vm.id, 2).await;

    env.hv.set_guest_agent(vm.id, GuestAnswer::Hang);
    env.hv.guest_event(vm.id, EventKind::Reset);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(launches_of(&env, vm.id), 1);
    assert!(running(&env.get(vm.id)));

    env.hv.set_guest_agent(vm.id, GuestAnswer::Ok);
    wait_pings(&env, vm.id, 2).await;
    env.hv.set_guest_agent(vm.id, GuestAnswer::Hang);
    env.wait(vm.id, "restarted", |_| launches_of(&env, vm.id) == 2)
        .await;
}

/// 일시 정지·재개 이벤트(QMP `STOP`·`RESUME`)마다 답이 없는 시간을 다시 센다(확인 사이에 멈췄다 이어 간 것을
/// 놓쳐도).
#[tokio::test]
async fn pause_events_restart_the_guest_watch_clock() {
    let (env, _ha) = start_with(true, Duration::from_secs(60), |_| {}).await;
    let vm = watched_vm("web", env.node, 1);
    env.hv.set_guest_agent(vm.id, GuestAnswer::Ok);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    wait_pings(&env, vm.id, 2).await;

    env.hv.set_guest_agent(vm.id, GuestAnswer::Hang);
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        env.hv.guest_event(vm.id, EventKind::PauseChanged);
    }
    assert_eq!(launches_of(&env, vm.id), 1);
    let last = std::time::Instant::now();
    env.wait(vm.id, "restarted", |_| launches_of(&env, vm.id) == 2)
        .await;
    assert!(
        last.elapsed() >= Duration::from_secs(1),
        "{:?}",
        last.elapsed()
    );
}
