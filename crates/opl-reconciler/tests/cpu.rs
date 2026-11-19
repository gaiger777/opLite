//! 켤 때의 CPU 해석과 기록(계획 63): 클러스터 기준·숨김 목록·노드 카탈로그로 `-cpu`를 정해 `cpu_resolved`에 남기고,
//! 받는 쪽은 기록 그대로 쓰며, 검사 루프가 보류 `cpu`를 다시 셈한다.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{Env, vm};
use opl_reconciler::cpu::{CpuContext, NodeCatalog, refresh_pending};
use opl_reconciler::resolve::incoming_spec;
use opl_types::cluster::{ClusterInfo, ClusterSpec};
use opl_types::cpu::{
    Baseline, BaselineSource, CpuCatalog, CpuSettings, CpuSource, NodeCpu, ResolvedCpu,
};
use opl_types::node::{CPU_MODELS, Node, NodeSpec, PLACEMENT};
use opl_types::resource::Resource;
use opl_types::settings::{SETTINGS_NAME, Settings, SettingsSpec};
use opl_types::vm::{Arch, MachineType, PendingReason, PowerState, RunningHardware, Vm, VmPhase};
use uuid::Uuid;

/// 이 노드 기록: x86_64 KVM, `features`, 카탈로그 해시 `catalog`(None이면 `NodeStatus.cpu` 없음)
async fn add_node(env: &Env, features: &[&str], catalog: Option<&str>) {
    let mut node: Node = Resource::new(
        "n1",
        NodeSpec {
            host_resources: None,
            address: "a".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.id = env.node;
    node.status.features = features.iter().map(|f| f.to_string()).collect();
    node.status.arch = Some(Arch::X86_64);
    node.status.hypervisor = "qemu-kvm".into();
    node.status.cpu = catalog.map(|hash| NodeCpu {
        catalog: hash.into(),
        accel: "kvm".into(),
        ..NodeCpu::default()
    });
    env.cluster.create(&node).await.unwrap();
}

/// Mock 카탈로그를 해시 이름으로 올리고 해시를 돌려준다.
async fn publish_catalog(env: &Env) -> String {
    let catalog = opl_driver::mock_cpu_probe().catalog;
    let hash = catalog.hash();
    let res: CpuCatalog = Resource::new(&hash, catalog);
    env.cluster.create(&res).await.unwrap();
    hash
}

fn mock_hash() -> String {
    opl_driver::mock_cpu_probe().catalog.hash()
}

async fn set_mask(env: &Env, mask: &[&str]) {
    let cpu = CpuSettings {
        mask: mask.iter().map(|m| m.to_string()).collect(),
        ..CpuSettings::default()
    };
    match env
        .cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
    {
        Some(s) => {
            let mut spec = s.spec.clone();
            spec.cpu = Some(cpu);
            env.cluster
                .update_spec(s.id, Some(s.revision), s.labels, &spec)
                .await
                .unwrap();
        }
        None => {
            let s: Settings = Resource::new(
                SETTINGS_NAME,
                SettingsSpec {
                    cpu: Some(cpu),
                    ..SettingsSpec::default()
                },
            );
            env.cluster.create(&s).await.unwrap();
        }
    }
}

fn baseline(model: &str) -> Baseline {
    Baseline {
        model: model.into(),
        flags: BTreeMap::new(),
        mode: BaselineSource::Pinned,
        nodes: Vec::new(),
        limited_by: Vec::new(),
        since: 0,
        previous: None,
        warnings: Vec::new(),
    }
}

async fn set_baseline(env: &Env, model: &str) {
    match env
        .cluster
        .list::<ClusterSpec>()
        .unwrap()
        .1
        .into_iter()
        .next()
    {
        Some(c) => {
            let mut status = c.status.clone();
            status.cpu_baselines.insert(Arch::X86_64, baseline(model));
            env.cluster
                .update_status_checked::<ClusterSpec>(c.id, c.revision, &status)
                .await
                .unwrap();
        }
        None => {
            let mut c: ClusterInfo = Resource::new(
                "cluster",
                ClusterSpec {
                    ca_cert_pem: "cert".into(),
                    ca_key_pem: "key".into(),
                },
            );
            c.status.cpu_baselines.insert(Arch::X86_64, baseline(model));
            env.cluster.create(&c).await.unwrap();
        }
    }
}

fn x86_vm(model: &str, flags: &[&str]) -> Vm {
    let mut vm = vm("web", Uuid::nil());
    vm.spec.machine = MachineType::Q35;
    vm.spec.cpu.model = model.parse().unwrap();
    vm.spec.cpu.flags = flags.iter().map(|f| f.parse().unwrap()).collect();
    vm
}

fn hardware(vm: &Vm) -> RunningHardware {
    vm.status.hardware.clone().expect("no running hardware")
}

fn launched_cpu(env: &Env, id: Uuid) -> Option<ResolvedCpu> {
    env.hv
        .launch_specs()
        .into_iter()
        .rfind(|s| s.id == id)
        .expect("VM was not launched")
        .cpu_resolved
}

/// 숨김 목록과 VM 플래그를 해석해 그대로 켜고 기록한다. 카탈로그가 모르는 숨김 이름(`sve`)은 건너뛴다.
#[tokio::test]
async fn the_cpu_is_resolved_at_start_and_recorded() {
    let env = Env::start().await;
    let hash = publish_catalog(&env).await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&hash)).await;
    set_mask(&env, &["avx512vbmi", "sve"]).await;
    let mut vm = x86_vm("Skylake-Server-v5", &["+aes"]);
    vm.spec.node = Some(env.node);
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "running", |v| {
            v.status.phase == VmPhase::Running && v.status.hardware.is_some()
        })
        .await;
    let want = ResolvedCpu {
        model: "Skylake-Server-v5".into(),
        flags: [("aes".to_string(), true), ("avx512vbmi".to_string(), false)].into(),
        enforce: true,
        source: CpuSource::Spec,
        dropped_mask: Vec::new(),
    };
    assert_eq!(hardware(&got).cpu_resolved, Some(want.clone()));
    assert_eq!(launched_cpu(&env, vm.id), Some(want));
    assert!(hardware(&got).pending.is_empty());
}

/// 한 노드라도 `cpu-models`를 알리지 않으면(업그레이드 중) 해석을 기록하지 않는다(기록이 지금과 바이트까지 같다).
#[tokio::test]
async fn nothing_is_recorded_while_the_cluster_is_upgrading() {
    let env = Env::start().await;
    add_node(&env, &[PLACEMENT], None).await;
    let mut vm = x86_vm("host", &[]);
    vm.spec.node = Some(env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "running", |v| {
            v.status.phase == VmPhase::Running && v.status.hardware.is_some()
        })
        .await;
    assert_eq!(hardware(&got).cpu_resolved, None);
    assert_eq!(launched_cpu(&env, vm.id), None);
}

/// 카탈로그가 없는 노드는 `host`·`max`만 켠다. 명명 모델은 켜지 않고 까닭을 남긴다.
#[tokio::test]
async fn a_node_without_a_catalog_runs_only_host_and_max() {
    let env = Env::start().await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some("")).await;
    let mut vm = x86_vm("Skylake-Server-v5", &[]);
    vm.spec.node = Some(env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.last_error.is_some())
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some(
            "failed to start: the VM's node has no CPU catalog; only the host and max CPU models can run there"
        )
    );
    assert!(env.hv.launches().is_empty());
}

/// 카탈로그로 켤 수 없는 모델은 켜지 않는다. 못 쓰는 까닭인 플래그를 끄면(`-avx512vbmi`) 켠다.
#[tokio::test]
async fn a_model_the_node_cannot_run_is_refused() {
    let env = Env::start().await;
    let hash = publish_catalog(&env).await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&hash)).await;
    let mut vm = x86_vm("Icelake-Server-v7", &[]);
    vm.spec.node = Some(env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.last_error.is_some())
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some(
            "failed to start: the VM's node cannot run CPU model Icelake-Server-v7 (missing avx512vbmi)"
        )
    );
    assert!(env.hv.launches().is_empty());

    let mut ok = x86_vm("Icelake-Server-v7", &["-avx512vbmi"]);
    ok.name = "ok".into();
    ok.spec.node = Some(env.node);
    env.create(&ok).await;
    env.wait(ok.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(
        launched_cpu(&env, ok.id).unwrap().describe(),
        "Icelake-Server-v7 -avx512vbmi"
    );
}

/// 노드가 알린 카탈로그가 아직 없으면 켜지 않고 기다린다(실패로 세지 않음). 카탈로그가 오면 켠다.
#[tokio::test]
async fn start_waits_for_the_catalog_resource() {
    let env = Env::start().await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&mock_hash())).await;
    let mut vm = x86_vm("Skylake-Server-v5", &[]);
    vm.spec.node = Some(env.node);
    env.create(&vm).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(env.hv.launches().is_empty());
    assert_eq!(env.get(vm.id).spec.power, PowerState::Running);
    publish_catalog(&env).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(
        launched_cpu(&env, vm.id).unwrap().model,
        "Skylake-Server-v5"
    );
}

/// `cluster` 모델은 기준으로 켜고, 기준·숨김 목록이 바뀌면 검사 루프가 보류 `cpu`를 쓰고(실행 중 CPU는 그대로) 되돌리면
/// 지운다.
#[tokio::test]
async fn the_check_loop_refreshes_the_pending_cpu_change() {
    let env = Env::start().await;
    let hash = publish_catalog(&env).await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&hash)).await;
    set_baseline(&env, "Skylake-Server-v5").await;
    let mut vm = x86_vm("cluster", &[]);
    vm.spec.node = Some(env.node);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "running", |v| {
            v.status.phase == VmPhase::Running && v.status.hardware.is_some()
        })
        .await;
    let running = hardware(&got).cpu_resolved.unwrap();
    assert_eq!(
        (running.model.as_str(), running.source),
        ("Skylake-Server-v5", CpuSource::Cluster)
    );
    assert_eq!(refresh_pending(&env.cluster, env.node).await.unwrap(), 0);

    // 기준이 바뀜
    set_baseline(&env, "Icelake-Server-v7").await;
    set_mask(&env, &["avx512vbmi"]).await;
    assert_eq!(refresh_pending(&env.cluster, env.node).await.unwrap(), 1);
    let hw = hardware(&env.get(vm.id));
    assert_eq!(hw.cpu_resolved, Some(running.clone()));
    assert_eq!(hw.pending.len(), 1);
    let p = &hw.pending[0];
    assert_eq!(
        (
            p.field.as_str(),
            p.reason,
            p.running.as_str(),
            p.wanted.as_str()
        ),
        (
            "cpu",
            PendingReason::Restart,
            "Skylake-Server-v5",
            "Icelake-Server-v7 -avx512vbmi"
        )
    );
    assert!(
        p.detail
            .as_deref()
            .unwrap()
            .starts_with("Skylake-Server-v5 → Icelake-Server-v7 (cluster baseline changed)")
    );
    // 같으면 다시 쓰지 않는다.
    assert_eq!(refresh_pending(&env.cluster, env.node).await.unwrap(), 0);

    // 되돌리면 지운다.
    set_baseline(&env, "Skylake-Server-v5").await;
    set_mask(&env, &[]).await;
    assert_eq!(refresh_pending(&env.cluster, env.node).await.unwrap(), 1);
    assert!(hardware(&env.get(vm.id)).pending.is_empty());
}

/// 받는 쪽은 이 노드의 기준·숨김 목록이 달라도 기록된 CPU 그대로 켠다.
#[tokio::test]
async fn the_receiving_side_uses_the_recorded_cpu() {
    let env = Env::start().await;
    let hash = publish_catalog(&env).await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&hash)).await;
    set_mask(&env, &["avx2"]).await;
    let mut vm = x86_vm("host", &[]);
    vm.id = Uuid::now_v7();
    let (mut hw, _) =
        RunningHardware::at_start(vm.id, &vm.name, &vm.spec, Default::default(), false);
    let recorded = ResolvedCpu {
        model: "host".into(),
        flags: [("avx512vbmi".to_string(), false)].into(),
        enforce: false,
        source: CpuSource::Spec,
        dropped_mask: Vec::new(),
    };
    hw.cpu_resolved = Some(recorded.clone());
    vm.status.hardware = Some(hw);
    let storage = opl_driver::DirStorage::new("qemu-img", "/nonexistent/opl-pools");
    let spec = incoming_spec(
        &env.cluster,
        &vm,
        env.node,
        Path::new("/nonexistent"),
        &storage,
    )
    .await
    .unwrap();
    assert_eq!(spec.cpu_resolved, Some(recorded));

    // 이 노드에서 새로 켜면 지금 숨김 목록이다.
    let ctx = CpuContext::load(&env.cluster, env.node).unwrap().unwrap();
    assert!(matches!(ctx.catalog, NodeCatalog::Found(_)));
    assert_eq!(ctx.start(&vm.spec.cpu).unwrap().describe(), "host -avx2");
}

/// 검사 루프는 계획 63 전 기록(해석 없음)의 VM에도 숨김 목록의 보류를 쓴다.
#[tokio::test]
async fn a_mask_marks_vms_started_before_the_upgrade() {
    let env = Env::start().await;
    add_node(&env, &[PLACEMENT], None).await;
    let mut vm = x86_vm("host", &[]);
    vm.spec.node = Some(env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| {
        v.status.phase == VmPhase::Running && v.status.hardware.is_some()
    })
    .await;
    // 업그레이드가 끝났다.
    let hash = publish_catalog(&env).await;
    let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    me.status.features.push(CPU_MODELS.into());
    me.status.cpu = Some(NodeCpu {
        catalog: hash,
        accel: "kvm".into(),
        ..NodeCpu::default()
    });
    env.cluster
        .update_status_checked::<NodeSpec>(me.id, me.revision, &me.status)
        .await
        .unwrap();
    assert_eq!(refresh_pending(&env.cluster, env.node).await.unwrap(), 0);
    set_mask(&env, &["avx512vbmi"]).await;
    assert_eq!(refresh_pending(&env.cluster, env.node).await.unwrap(), 1);
    let hw = hardware(&env.get(vm.id));
    assert_eq!(hw.cpu_resolved, None);
    assert_eq!(
        (
            hw.pending[0].running.as_str(),
            hw.pending[0].wanted.as_str()
        ),
        ("host", "host -avx512vbmi")
    );
}

/// 메모리 스냅샷 `name`을 스펙 `vm_spec`과 하드웨어 기록 `hardware`로 남긴다.
async fn memory_snapshot(
    env: &Env,
    vm: &Vm,
    name: &str,
    vm_spec: opl_types::vm::VmSpec,
    hardware: Option<RunningHardware>,
) -> Uuid {
    let snapshot: Resource<opl_types::snapshot::SnapshotSpec, _> = Resource::new(
        name,
        opl_types::snapshot::SnapshotSpec {
            vm: vm.id,
            name: name.into(),
            parent: None,
            memory: true,
            description: String::new(),
            scheduled: false,
            vm_spec,
            hardware,
        },
    );
    env.cluster.create(&snapshot).await.unwrap();
    snapshot.id
}

/// 리뷰 1 M4: 메모리 스냅샷을 불러오며 켤 때 기록된 CPU를 이 노드가 켤 수 없으면 QEMU 전에 까닭을 남긴다. 기록이 없는
/// 옛 스냅샷은 지금 스펙이 아니라 담을 때 스펙의 CPU 모델로 켠다.
#[tokio::test]
async fn a_memory_snapshot_restore_checks_its_recorded_cpu() {
    let env = Env::start().await;
    let hash = publish_catalog(&env).await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&hash)).await;
    let mut vm = x86_vm("Skylake-Server-v5", &[]);
    vm.spec.node = Some(env.node);
    vm.spec.power = PowerState::Stopped;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;

    // 옛 스냅샷(기록 없음): 담을 때 host였다.
    let host_spec = x86_vm("host", &[]).spec;
    let mut taken = host_spec.clone();
    taken.node = Some(env.node);
    let old = memory_snapshot(&env, &vm, "old", taken, None).await;
    env.restore_on_start(vm.id, &old.to_string());
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "restored", |v| {
        v.status.phase == VmPhase::Running && v.status.hardware.is_some()
    })
    .await;
    let launched = env
        .hv
        .launch_specs()
        .into_iter()
        .rfind(|s| s.id == vm.id)
        .unwrap();
    assert_eq!(launched.cpu.model.as_str(), "host");
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;

    // 이 노드가 켤 수 없는 CPU로 담은 스냅샷
    let mut hw = RunningHardware::legacy(&vm.spec);
    hw.cpu_resolved = Some(ResolvedCpu {
        model: "Icelake-Server-v7".into(),
        flags: BTreeMap::new(),
        enforce: true,
        source: CpuSource::Spec,
        dropped_mask: Vec::new(),
    });
    let launches = env.hv.launches().len();
    let ice = memory_snapshot(&env, &vm, "ice", vm.spec.clone(), Some(hw)).await;
    env.restore_on_start(vm.id, &ice.to_string());
    env.set_power(vm.id, PowerState::Running).await;
    let got = env
        .wait(vm.id, "refused", |v| v.status.last_error.is_some())
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some(
            "failed to start: the VM's node cannot run CPU model Icelake-Server-v7 (missing avx512vbmi); the memory snapshot was taken with that CPU"
        )
    );
    assert_eq!(env.hv.launches().len(), launches);
}

/// 리뷰 2 #1: 담을 때 그 노드의 QEMU가 몰라 뺀 숨김 이름이 있는 메모리 스냅샷은 그 이름을 아는 노드에서 되돌리지
/// 않는다(되돌린 게스트에 숨긴 기능이 드러난다).
#[tokio::test]
async fn a_memory_snapshot_with_dropped_mask_names_is_not_restored_where_qemu_knows_them() {
    let env = Env::start().await;
    let hash = publish_catalog(&env).await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&hash)).await;
    set_mask(&env, &["avx512vbmi"]).await;
    let mut vm = x86_vm("host", &[]);
    vm.spec.node = Some(env.node);
    vm.spec.power = PowerState::Stopped;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    let mut hw = RunningHardware::legacy(&vm.spec);
    hw.cpu_resolved = Some(ResolvedCpu {
        model: "host".into(),
        flags: BTreeMap::new(),
        enforce: false,
        source: CpuSource::Spec,
        dropped_mask: vec!["avx512vbmi".into()],
    });
    let launches = env.hv.launches().len();
    let snap = memory_snapshot(&env, &vm, "old-qemu", vm.spec.clone(), Some(hw)).await;
    env.restore_on_start(vm.id, &snap.to_string());
    env.set_power(vm.id, PowerState::Running).await;
    let got = env
        .wait(vm.id, "refused", |v| v.status.last_error.is_some())
        .await;
    let e = got.status.last_error.unwrap();
    assert!(
        e.starts_with(
            "failed to start: the VM's node would expose the hidden CPU flags avx512vbmi when the memory snapshot is restored"
        ),
        "{e}"
    );
    assert_eq!(env.hv.launches().len(), launches);
}

/// 리뷰 2 #4: 작업자가 스스로 다시 켜는 VM(크래시)은 이 노드가 보류된 새 CPU를 켤 수 없으면 쓰던 CPU로 다시 켜고
/// 바뀜은 보류로 남긴다. 사용자가 끈 뒤 켜면(전원 껐다 켜기) 새 CPU로 켜려다 거절한다.
#[tokio::test]
async fn a_crash_restarts_with_the_running_cpu_when_the_new_one_cannot_run_here() {
    let env = Env::start().await;
    let hash = publish_catalog(&env).await;
    add_node(&env, &[PLACEMENT, CPU_MODELS], Some(&hash)).await;
    let mut vm = x86_vm("Skylake-Server-v5", &[]);
    vm.spec.node = Some(env.node);
    vm.spec.restart = opl_types::vm::RestartPolicy::OnFailure;
    env.create(&vm).await;
    let before = env
        .wait(vm.id, "running", |v| {
            v.status.phase == VmPhase::Running && v.status.hardware.is_some()
        })
        .await;
    let running = hardware(&before).cpu_resolved.unwrap();
    assert_eq!(running.model, "Skylake-Server-v5");

    // 이 노드가 켤 수 없는 모델로 바꾼다(실행 중이라 보류).
    let current = env.get(vm.id);
    let mut spec = current.spec.clone();
    spec.cpu.model = "Icelake-Server-v7".parse().unwrap();
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    let launches = env.hv.launches().len();
    env.hv.crash(vm.id);
    let got = env
        .wait(vm.id, "restarted", |v| {
            v.status.phase == VmPhase::Running
                && v.status.pid.is_some()
                && v.status.pid != before.status.pid
                && v.status.hardware.is_some()
        })
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(env.hv.launches().len(), launches + 1);
    assert_eq!(launched_cpu(&env, vm.id), Some(running.clone()));
    assert_eq!(hardware(&got).cpu_resolved, Some(running));

    // 사용자의 끄기·켜기는 새 CPU다: 이 노드는 켤 수 없으니 까닭을 남긴다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    let got = env
        .wait(vm.id, "refused", |v| v.status.last_error.is_some())
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some(
            "failed to start: the VM's node cannot run CPU model Icelake-Server-v7 (missing avx512vbmi)"
        )
    );
}
