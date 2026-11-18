//! TPM 상태가 VM을 따라다닌다(계획 59 §5): 켜기 규칙(이 노드·받아 오기·새로·켜지 않기), 해시 기록과 사본 밀어 두기.
//! 그리고 게스트 에이전트(§8·§9): 게스트 정보 수집과 에이전트 종료.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm};
use opl_driver::GuestAnswer;
use opl_driver::StopMode;
use opl_reconciler::{FirmwareCopy, FirmwareError, FirmwareFetcher, GuestInfoSink};
use opl_types::firmware::tpm_unavailable;
use opl_types::guest::GuestInfo;
use opl_types::node::{FirmwareCaps, Node, NodeSpec};
use opl_types::replication::ReplicationTarget;
use opl_types::resource::Resource;
use opl_types::runtime::RuntimeTpm;
use opl_types::vm::{GuestAgentSpec, PowerState, Vm, VmPhase};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 받아 오기·밀어 두기 호출을 적는다. `fail_tpm`의 노드에서는 TPM 상태를 받지 못한다.
#[derive(Default)]
struct Fetcher {
    vars: Mutex<Vec<Uuid>>,
    tpm: Mutex<Vec<(Uuid, String, u32)>>,
    fail_tpm: Mutex<Vec<Uuid>>,
    pushes: Mutex<Vec<FirmwareCopy>>,
}

#[async_trait]
impl FirmwareFetcher for Fetcher {
    async fn fetch(&self, _vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
        self.vars.lock().unwrap().push(node);
        Ok(())
    }

    async fn fetch_tpm(
        &self,
        _vm: Uuid,
        node: Uuid,
        sha256: &str,
        epoch: u32,
    ) -> Result<(), FirmwareError> {
        self.tpm.lock().unwrap().push((node, sha256.into(), epoch));
        if self.fail_tpm.lock().unwrap().contains(&node) {
            return Err(FirmwareError("connection reset".into()));
        }
        Ok(())
    }

    async fn push(&self, copy: &FirmwareCopy) -> Result<(), FirmwareError> {
        self.pushes.lock().unwrap().push(copy.clone());
        Ok(())
    }
}

/// 받은 게스트 정보를 적는다.
#[derive(Default)]
struct Sink {
    infos: Mutex<Vec<(Uuid, Option<GuestInfo>)>>,
}

impl GuestInfoSink for Sink {
    fn put(&self, vm: Uuid, info: GuestInfo) {
        self.infos.lock().unwrap().push((vm, Some(info)));
    }

    fn remove(&self, vm: Uuid) {
        self.infos.lock().unwrap().push((vm, None));
    }
}

impl Sink {
    fn last(&self, vm: Uuid) -> Option<Option<GuestInfo>> {
        self.infos
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(v, _)| *v == vm)
            .map(|(_, i)| i.clone())
    }
}

/// 클러스터에 노드 기록을 둔다(TPM 능력 있음).
async fn add_node(env: &Env, name: &str, online: bool) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: format!("{name}:8007"),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    env.cluster.create(&node).await.unwrap();
    node.status.online = online;
    node.status.firmware_caps = Some(FirmwareCaps {
        tpm: true,
        secure_boot: false,
    });
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();
    node.id
}

async fn set_online(env: &Env, id: Uuid, online: bool) {
    let mut node = env.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    node.status.online = online;
    env.cluster
        .update_status::<NodeSpec>(id, &node.status)
        .await
        .unwrap();
}

fn tpm_vm(env: &Env) -> Vm {
    let mut v = vm("web", env.node);
    v.spec.tpm = true;
    v
}

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

fn last_tpm(env: &Env) -> Option<RuntimeTpm> {
    env.hv.launch_specs().last().unwrap().tpm
}

/// 지금 TPM 상태를 가진 노드가 받기에 실패하면 사본 노드에서 받고, 변수도 그 노드에서 받는다. 이 노드의 상태로 켠다.
#[tokio::test]
async fn the_tpm_state_is_fetched_from_the_holder_then_a_copy() {
    let fetcher = Arc::new(Fetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let env = Env::start_with(|c| c.firmware = Some(f)).await;
    let (a, b) = (
        add_node(&env, "a", true).await,
        add_node(&env, "b", true).await,
    );
    fetcher.fail_tpm.lock().unwrap().push(a);
    let mut v = tpm_vm(&env);
    v.status.firmware_node = Some(a);
    v.status.tpm_sha256 = Some("ab".repeat(32));
    v.status.firmware_copies = vec![b];
    v.status.firmware_epoch = 2;
    env.create(&v).await;
    let got = env.wait(v.id, "running", running).await;

    assert_eq!(
        *fetcher.tpm.lock().unwrap(),
        [(a, "ab".repeat(32), 2), (b, "ab".repeat(32), 2)]
    );
    assert_eq!(*fetcher.vars.lock().unwrap(), [b]);
    assert_eq!(
        last_tpm(&env),
        Some(RuntimeTpm {
            fresh: false,
            epoch: 2
        })
    );
    assert_eq!(got.status.firmware_node, Some(env.node));
    // 회차는 작업자가 바꾸지 않는다.
    assert_eq!(got.status.firmware_epoch, 2);
}

/// 지금 TPM 상태를 가진 노드가 오프라인이고 사본도 없으면 켜지 않고 그 이유를 남긴 채 다시 본다. 노드가 돌아오면 켠다.
#[tokio::test]
async fn a_vm_whose_tpm_state_is_unreachable_waits() {
    let fetcher = Arc::new(Fetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let env = Env::start_with(|c| c.firmware = Some(f)).await;
    let a = add_node(&env, "a", false).await;
    let mut v = tpm_vm(&env);
    v.status.firmware_node = Some(a);
    v.status.tpm_sha256 = Some("ab".repeat(32));
    env.create(&v).await;
    let want = tpm_unavailable("web", Some("a"));
    let got = env
        .wait(v.id, "waiting", |v| {
            v.status.last_error.as_deref() == Some(want.as_str())
        })
        .await;
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(got.spec.power, PowerState::Running);
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());

    set_online(&env, a, true).await;
    env.wait(v.id, "running", running).await;
    assert_eq!(fetcher.tpm.lock().unwrap().last().unwrap().0, a);
}

/// 기록된 TPM 상태가 없으면(처음 켜기) 새로 만들고, 이 노드가 지금 것을 가졌으면 받지 않는다.
#[tokio::test]
async fn a_new_tpm_is_created_only_without_a_recorded_state() {
    let state = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let dir = state.path().to_path_buf();
    let env = Env::start_with(|c| {
        c.firmware = Some(f);
        c.state_dir = Some(dir);
    })
    .await;
    let v = tpm_vm(&env);
    env.create(&v).await;
    env.wait(v.id, "running", running).await;
    assert_eq!(
        last_tpm(&env),
        Some(RuntimeTpm {
            fresh: true,
            epoch: 0
        })
    );

    // firmware_node가 이 노드이고 파일이 있으면(해시는 기록 뒤 바뀌었을 수 있다) 이 노드의 상태로 켠다.
    let mut mine = vm("mine", env.node);
    mine.spec.tpm = true;
    mine.status.firmware_node = Some(env.node);
    mine.status.tpm_sha256 = Some("cd".repeat(32));
    let permall = opl_driver::tpm_state_path(state.path(), mine.id);
    std::fs::create_dir_all(permall.parent().unwrap()).unwrap();
    std::fs::write(&permall, b"changed since the hash").unwrap();
    env.create(&mine).await;
    env.wait(mine.id, "running", running).await;
    assert_eq!(
        last_tpm(&env),
        Some(RuntimeTpm {
            fresh: false,
            epoch: 0
        })
    );
    assert!(fetcher.tpm.lock().unwrap().is_empty());
}

/// TPM 상태를 가진 노드의 작업자는 상태 파일의 해시를 기록하고, 사본 대상(복제 대상)에 밀어 둔다. 파일이 바뀌면 해시를
/// 다시 쓰고 사본을 비운 뒤 다시 밀어 둔다.
#[tokio::test]
async fn the_tpm_hash_is_recorded_and_copies_are_pushed() {
    let state = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fetcher::default());
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    let dir = state.path().to_path_buf();
    let env = Env::start_with(|c| {
        c.firmware = Some(f);
        c.state_dir = Some(dir);
    })
    .await;
    let b = add_node(&env, "b", true).await;
    let mut v = tpm_vm(&env);
    v.spec.replication = vec![ReplicationTarget {
        node: b,
        schedule: "*/15".into(),
        pool: None,
    }];
    let path = opl_driver::tpm_state_path(state.path(), v.id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"first").unwrap();
    env.create(&v).await;
    let first = hex(b"first");
    let got = env
        .wait(v.id, "hash and copy", |v| {
            v.status.tpm_sha256.as_deref() == Some(first.as_str())
                && v.status.firmware_copies == [b]
        })
        .await;
    assert_eq!(got.status.firmware_node, Some(env.node));
    let pushes = fetcher.pushes.lock().unwrap().clone();
    assert_eq!(
        pushes,
        [FirmwareCopy {
            vm: v.id,
            to: b,
            sha256: first.clone(),
            epoch: 0,
            secure_boot: false,
        }]
    );

    // 도는 동안 바뀌면(swtpm이 씀) 다시 기록하고 다시 밀어 둔다.
    std::fs::write(&path, b"second, longer").unwrap();
    let second = hex(b"second, longer");
    env.wait(v.id, "new hash and copy", |v| {
        v.status.tpm_sha256.as_deref() == Some(second.as_str()) && v.status.firmware_copies == [b]
    })
    .await;
    assert_eq!(
        fetcher.pushes.lock().unwrap().last().unwrap().sha256,
        second
    );

    // 멈추면 곧바로 다시 본다(같으면 쓰지 않는다).
    env.set_power(v.id, PowerState::Stopped).await;
    let stopped = env
        .wait(v.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(stopped.status.tpm_sha256.as_deref(), Some(second.as_str()));
    assert_eq!(fetcher.pushes.lock().unwrap().len(), 2);
}

fn hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 게스트 에이전트가 답하면 정보를 캐시에 두고, 끌 때 에이전트로 끈다. VM이 꺼지면 정보를 지운다.
#[tokio::test]
async fn an_answering_agent_is_polled_and_shuts_the_vm_down() {
    let sink = Arc::new(Sink::default());
    let s: Arc<dyn GuestInfoSink> = sink.clone();
    let env = Env::start_with(|c| c.guest_info = Some(s)).await;
    let mut v = vm("web", env.node);
    v.spec.guest_agent = Some(GuestAgentSpec::default());
    env.hv.set_guest_agent(v.id, GuestAnswer::Ok);
    let info = GuestInfo {
        hostname: Some("web".into()),
        ..Default::default()
    };
    env.hv.set_guest_info(v.id, info.clone());
    env.create(&v).await;
    env.wait(v.id, "running", running).await;
    wait_for(|| sink.last(v.id) == Some(Some(info.clone()))).await;

    env.set_power(v.id, PowerState::Stopped).await;
    env.wait(v.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(env.hv.stop_requests(v.id), [StopMode::Agent]);
    wait_for(|| sink.last(v.id) == Some(None)).await;
}

/// 에이전트가 이 QEMU에서 답한 적이 없거나 종료 방법이 ACPI면 ACPI로 끈다. 채널이 없는 VM은 묻지 않는다.
#[tokio::test]
async fn vms_without_an_answering_agent_shut_down_with_acpi() {
    let env = Env::start().await;
    let mut silent = vm("silent", env.node);
    silent.spec.guest_agent = Some(GuestAgentSpec::default());
    env.hv.set_guest_agent(silent.id, GuestAnswer::Hang);
    let mut acpi = vm("acpi", env.node);
    acpi.spec.guest_agent = Some(GuestAgentSpec {
        shutdown: opl_types::vm::AgentShutdown::Acpi,
        ..Default::default()
    });
    env.hv.set_guest_agent(acpi.id, GuestAnswer::Ok);
    let plain = vm("plain", env.node);
    for v in [&silent, &acpi, &plain] {
        env.create(v).await;
        env.wait(v.id, "running", running).await;
    }
    wait_for(|| env.hv.guest_info_calls(silent.id) > 0 && env.hv.guest_info_calls(acpi.id) > 0)
        .await;
    for v in [&silent, &acpi, &plain] {
        env.set_power(v.id, PowerState::Stopped).await;
        env.wait(v.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
            .await;
        assert_eq!(env.hv.stop_requests(v.id), [StopMode::Acpi], "{}", v.name);
    }
    assert_eq!(env.hv.guest_info_calls(plain.id), 0);
}

async fn wait_for(cond: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + common::WAIT;
    while !cond() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
