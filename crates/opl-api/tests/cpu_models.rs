//! 계획 63 API: CPU 모델·플래그의 기능 게이트(스테이지 1 — 모델 API를 열기 전에는 `host`·`max` 밖을 받지 않는다),
//! 리더의 클러스터 기준 기록과 마이그레이션의 CPU 확인(스테이지 4).

mod common;

use std::time::Duration;

use common::{Env, WAIT, vm_request};
use opl_api::{ClusterJanitor, ClusterJanitorConfig};
use opl_types::audit::AuditQuery;
use opl_types::cluster::ClusterSpec;
use opl_types::cpu::{
    Baseline, BaselineMode, BaselineSource, Bits, CpuCatalog, CpuSettings, CpuSource, NodeCpu,
    ResolvedCpu,
};
use opl_types::node::{CPU_MODELS, FEATURES, Node, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::settings::{SETTINGS_NAME, Settings, SettingsSpec};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::{Arch, CpuSpec, MachineType, PowerState, RunningHardware, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// 이 노드가 `cpu-models`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_cpu_models(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != CPU_MODELS);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn cpu_models_and_flags_are_gated() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // host·max는 지금처럼 된다.
    for (name, model) in [("h", "host"), ("m", "max")] {
        let mut req = vm_request(name);
        req["spec"]["cpu"]["model"] = json!(model);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["spec"]["cpu"]["model"], model);
        assert!(body["spec"]["cpu"].get("flags").is_none(), "{body}");
    }
    let tries = [
        json!({ "model": "x86-64-v3" }),
        json!({ "model": "cluster" }),
        json!({ "model": "Icelake-Server-v7" }),
        json!({ "model": "host", "flags": ["-avx512vbmi"] }),
    ];
    // 모든 노드가 알려도 스테이지 5 전에는 400이다.
    for cpu in &tries {
        let mut req = vm_request("new");
        req["spec"]["cpu"]["model"] = cpu["model"].clone();
        if let Some(f) = cpu.get("flags") {
            req["spec"]["cpu"]["flags"] = f.clone();
        }
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{cpu}: {body}");
    }
    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_cpu_models(&env).await;
    for cpu in &tries {
        let mut req = vm_request("new");
        req["spec"]["cpu"]["model"] = cpu["model"].clone();
        if let Some(f) = cpu.get("flags") {
            req["spec"]["cpu"]["flags"] = f.clone();
        }
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CONFLICT, "{cpu}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
    // 문법이 틀린 모델·플래그는 JSON을 풀지 못한다.
    for (model, flags) in [("base", json!([])), ("host", json!(["avx"]))] {
        let mut req = vm_request("bad");
        req["spec"]["cpu"]["model"] = json!(model);
        req["spec"]["cpu"]["flags"] = flags;
        let (status, body) = admin.post("/vms", req).await;
        assert!(status.is_client_error(), "{model}: {status} {body}");
    }
}

/// 저장된 VM의 모델·플래그를 그대로 두는 바꾸기(`vm set --cores`)는 스테이지 5 전에도 받는다(바꾸면 400).
#[tokio::test]
async fn an_update_that_keeps_the_stored_cpu_model_and_flags_is_accepted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 테스트 고리처럼 저장소에 바로 플래그를 쓴다.
    let mut vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    vm.spec.cpu.flags = vec!["-sve".parse().unwrap()];
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    let mut spec = serde_json::to_value(&vm.spec).unwrap();
    spec["cpu"]["cores"] = json!(2);
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["cpu"]["flags"], json!(["-sve"]), "{body}");
    spec["cpu"]["flags"] = json!(["-pauth"]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// Mock 카탈로그(`Icelake-Server-v7`은 `avx512vbmi`가 없어 못 켬 — 약한 노드)와 그것을 켤 수 있는 강한 카탈로그를 만들고
/// (강한 것, 약한 것)의 해시를 돌려준다.
async fn add_catalogs(env: &Env) -> (String, String) {
    let weak = opl_driver::mock_cpu_probe().catalog;
    let mut strong = weak.clone();
    for m in &mut strong.models {
        m.missing = Bits::default();
    }
    strong.supported = Bits::from_indices(0..strong.flags.len());
    let mut hashes = vec![];
    for c in [strong, weak] {
        let hash = c.hash();
        let r: CpuCatalog = Resource::new(&hash, c);
        env.cluster.create(&r).await.unwrap();
        hashes.push(hash);
    }
    (hashes[0].clone(), hashes[1].clone())
}

/// `cpu-models`를 아는 x86_64 KVM 노드(실행기 없음)를 카탈로그 `catalog`로 저장소에 만든다.
async fn add_x86_node(env: &Env, name: &str, catalog: &str) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.status = NodeStatus {
        cpu: Some(NodeCpu {
            catalog: catalog.into(),
            qemu: "9.2.0".into(),
            accel: "kvm".into(),
            vendor: Some("GenuineIntel".into()),
            model_id: None,
            probed_at: 1,
            error: None,
        }),
        online: true,
        cpus: 8,
        memory_mib: 16384,
        hypervisor: "qemu-kvm".into(),
        bridges: vec![],
        arch: Some(Arch::X86_64),
        pools: Default::default(),
        ha: None,
        underlay: None,
        firewall: false,
        version: None,
        updates: None,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        cluster_addresses: vec![],
        shutdown: None,
        fence_check: None,
        interfaces: vec![],
        checks: vec![],
        hotplug: None,
        firmware_caps: None,
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 설정 `cpu`를 저장소에 바로 쓴다(설정 API의 `cpu`는 스테이지 5).
async fn set_cpu_settings(env: &Env, cpu: CpuSettings) {
    match env
        .cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
    {
        Some(mut s) => {
            s.spec.cpu = Some(cpu);
            env.cluster
                .update_spec(s.id, None, s.labels.clone(), &s.spec)
                .await
                .unwrap();
        }
        None => {
            let spec = SettingsSpec {
                cpu: Some(cpu),
                ..Default::default()
            };
            let s: Settings = Resource::new(SETTINGS_NAME, spec);
            env.cluster.create(&s).await.unwrap();
        }
    }
}

/// 노드 `node`에 고정된 x86_64 VM 스펙(API를 거치지 않는다 — 모델 API는 스테이지 5)
fn x86_spec(model: &str, node: Uuid, power: PowerState) -> VmSpec {
    VmSpec {
        cpu: CpuSpec {
            flags: Vec::new(),
            sockets: 1,
            cores: 1,
            model: model.parse().unwrap(),
            max_vcpus: None,
        },
        memory_mib: 512,
        firmware: Default::default(),
        machine: MachineType::Q35,
        disks: vec![],
        nics: vec![],
        power,
        restart: Default::default(),
        node: Some(node),
        snapshot_schedule: None,
        ha: None,
        placement: None,
        max_memory_mib: None,
        replication: Vec::new(),
        cdroms: vec![],
        cloud_init: None,
        template: false,
        tpm: false,
        secure_boot: false,
        guest_agent: None,
        folder: None,
    }
}

/// 노드 `node`에 고정된 꺼진 VM
async fn stopped_vm(env: &Env, name: &str, node: Uuid, model: &str) {
    let vm: Vm = Resource::new(name, x86_spec(model, node, PowerState::Stopped));
    env.cluster.create(&vm).await.unwrap();
}

/// 노드 `node`에서 도는 VM. `resolved`는 켤 때 기록한 CPU다.
async fn running_vm(env: &Env, name: &str, node: Uuid, model: &str, resolved: ResolvedCpu) {
    let mut vm: Vm = Resource::new(name, x86_spec(model, node, PowerState::Running));
    env.cluster.create(&vm).await.unwrap();
    let mut hw = RunningHardware::legacy(&vm.spec);
    hw.cpu_resolved = Some(resolved);
    vm.status.phase = VmPhase::Running;
    vm.status.node = Some(node);
    vm.status.hardware = Some(hw);
    env.cluster
        .update_status::<VmSpec>(vm.id, &vm.status)
        .await
        .unwrap();
}

fn resolved(model: &str, source: CpuSource) -> ResolvedCpu {
    ResolvedCpu {
        model: model.into(),
        flags: Default::default(),
        enforce: model != "host",
        source,
    }
}

/// x86_64 기준이 `model`이 될 때까지 기다린다.
async fn wait_baseline(env: &Env, model: &str) -> Baseline {
    let start = std::time::Instant::now();
    loop {
        let info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
        if let Some(b) = info.status.cpu_baselines.get(&Arch::X86_64)
            && b.model == model
        {
            return b.clone();
        }
        assert!(
            start.elapsed() < WAIT,
            "baselines: {:#?}",
            info.status.cpu_baselines
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 기록 Task `cpu-baseline`의 작업(만든 순서)
fn baseline_records(env: &Env) -> Vec<TaskOp> {
    let mut tasks = env.cluster.list::<TaskSpec>().unwrap().1;
    tasks.sort_by_key(|t| t.id);
    tasks
        .into_iter()
        .map(|t| t.spec.op)
        .filter(|op| matches!(op, TaskOp::CpuBaseline { .. }))
        .collect()
}

/// 이 노드 감사 로그에서 `action`인 항목의 detail들
async fn audit_details(env: &Env, action: &str) -> Vec<String> {
    env.state
        .audit
        .read(AuditQuery {
            since: 0,
            until: u64::MAX,
            user: None,
            action: None,
            limit: 1000,
        })
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == action)
        .filter_map(|e| e.detail)
        .collect()
}

/// 노드 셋에 약한 노드가 들어온다: 리더가 auto 기준을 내려 기록하고(기록 Task·감사), 앞 기준으로 도는 `cluster` VM은
/// 약한 노드로 옮기지 못하며 나머지 노드로는 옮긴다. `host` VM은 옮기되 감사에 경고를 남긴다.
#[tokio::test]
async fn a_weaker_node_lowers_the_auto_baseline_and_running_vms_avoid_it() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (strong, weak) = add_catalogs(&env).await;
    let n1 = add_x86_node(&env, "n1", &strong).await;
    add_x86_node(&env, "n2", &strong).await;
    add_x86_node(&env, "n3", &strong).await;
    set_cpu_settings(
        &env,
        CpuSettings {
            baseline: BaselineMode::Auto,
            ..Default::default()
        },
    )
    .await;
    let janitor = ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            ..ClusterJanitorConfig::default()
        },
    );
    // 강한 노드 셋: 모두 켤 수 있는 가장 좋은 모델
    let first = wait_baseline(&env, "Icelake-Server-v7").await;
    assert_eq!(first.mode, BaselineSource::Auto);
    assert_eq!(first.nodes.len(), 3);
    assert!(first.since > 0);
    assert_eq!(first.previous, None);
    // aarch64 이 노드(env)는 auto 셈 밖이다.
    let info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
    assert_eq!(info.status.cpu_baselines.len(), 1);
    // 앞 기준으로 도는 cluster VM과 host VM
    running_vm(
        &env,
        "web",
        n1,
        "cluster",
        resolved("Icelake-Server-v7", CpuSource::Cluster),
    )
    .await;
    running_vm(&env, "db", n1, "host", resolved("host", CpuSource::Spec)).await;

    // 약한 노드가 들어온다.
    let n4 = add_x86_node(&env, "n4", &weak).await;
    let lowered = wait_baseline(&env, "Skylake-Server-v5").await;
    assert_eq!(lowered.limited_by, vec![n4]);
    assert_eq!(lowered.nodes.len(), 4);
    assert_eq!(
        lowered.previous.as_ref().unwrap().model,
        "Icelake-Server-v7"
    );
    assert!(lowered.since >= first.since);
    // 바뀜마다 기록 Task 하나
    let records = {
        let start = std::time::Instant::now();
        loop {
            let r = baseline_records(&env);
            if r.len() >= 2 || start.elapsed() > WAIT {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert_eq!(
        records,
        vec![
            TaskOp::CpuBaseline {
                arch: Arch::X86_64,
                mode: Some(BaselineSource::Auto),
                previous: None,
                current: Some("Icelake-Server-v7".into()),
                limited_by: vec![],
                running_vms: 0,
            },
            TaskOp::CpuBaseline {
                arch: Arch::X86_64,
                mode: Some(BaselineSource::Auto),
                previous: Some("Icelake-Server-v7".into()),
                current: Some("Skylake-Server-v5".into()),
                limited_by: vec!["n4".into()],
                running_vms: 1,
            },
        ]
    );
    let tasks = env.cluster.list::<TaskSpec>().unwrap().1;
    let record = tasks
        .iter()
        .find(|t| matches!(t.spec.op, TaskOp::CpuBaseline { .. }))
        .unwrap();
    assert!(record.status.state.is_finished());
    assert_eq!(record.spec.node, env.node);
    // 기준이 그대로면 더 쓰지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(baseline_records(&env).len(), 2);
    let audits = audit_details(&env, "cpu.baseline").await;
    assert!(
        audits.contains(
            &"cpu baseline x86_64 auto: Icelake-Server-v7 -> Skylake-Server-v5 (node n4)"
                .to_string()
        ),
        "{audits:?}"
    );

    // 앞 기준으로 도는 VM은 약한 노드로 옮기지 못한다.
    let (status, body) = admin
        .post("/vms/web/migrate", json!({ "node": "n4" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_capacity", "{body}");
    assert_eq!(
        body["message"],
        "node \"n4\" cannot run CPU model Icelake-Server-v7 (missing avx512vbmi)"
    );
    // 다른 강한 노드로는 옮긴다.
    let (status, body) = admin
        .post("/vms/web/migrate", json!({ "node": "n2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // host VM은 막지 않고 경고를 감사에 남긴다.
    let (status, body) = admin.post("/vms/db/migrate", json!({ "node": "n4" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let start = std::time::Instant::now();
    loop {
        let details = audit_details(&env, "POST /vms/{key}/migrate").await;
        if details.iter().any(|d| {
            d == "cpu-warning: target node \"n4\" lacks avx512vbmi that the guest may be using"
        }) {
            break;
        }
        assert!(start.elapsed() < WAIT, "{details:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 약한 노드에 고정된 꺼진 VM을 켜면 켤 수 있는 노드로 옮겨 켠다(작업자는 약한 노드에서 켜지 않는다).
    stopped_vm(&env, "app", n4, "Icelake-Server-v7").await;
    let (status, body) = admin.post("/vms/app/actions/start", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let app = env.cluster.get_by_name::<VmSpec>("app").unwrap().unwrap();
    assert_ne!(app.spec.node, Some(n4));
    assert!(app.spec.node.is_some());
    // 켤 수 있는 노드가 없으면 409와 까닭
    let mut weak_only = x86_spec("Icelake-Server-v7", n4, PowerState::Stopped);
    weak_only.cpu.flags = vec!["+avx512vbmi".parse().unwrap()];
    let mut solo: Vm = Resource::new("solo", weak_only);
    solo.spec.cpu.model = "Skylake-Server-v5".parse().unwrap();
    env.cluster.create(&solo).await.unwrap();
    for n in ["n1", "n2", "n3"] {
        let node = env.cluster.get_by_name::<NodeSpec>(n).unwrap().unwrap();
        let mut spec = node.spec.clone();
        spec.maintenance = true;
        env.cluster
            .update_spec(node.id, None, node.labels.clone(), &spec)
            .await
            .unwrap();
    }
    let (status, body) = admin.post("/vms/solo/actions/start", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_capacity", "{body}");
    assert_eq!(
        body["message"],
        "node \"n4\" cannot run CPU model Skylake-Server-v5 (missing avx512vbmi)"
    );
    janitor.shutdown().await;
}

/// 업그레이드 중(옛 노드 하나)에는 리더가 기준을 셈하지도 쓰지도 않는다.
#[tokio::test]
async fn the_leader_does_not_record_a_baseline_during_an_upgrade() {
    let env = Env::start().await;
    let (strong, _) = add_catalogs(&env).await;
    add_x86_node(&env, "n1", &strong).await;
    set_cpu_settings(
        &env,
        CpuSettings {
            baseline: BaselineMode::Auto,
            ..Default::default()
        },
    )
    .await;
    drop_cpu_models(&env).await;
    let janitor = ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            ..ClusterJanitorConfig::default()
        },
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    let info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
    assert!(info.status.cpu_baselines.is_empty());
    assert!(baseline_records(&env).is_empty());
    janitor.shutdown().await;
}
