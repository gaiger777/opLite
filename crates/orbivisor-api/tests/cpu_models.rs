//! 계획 63 API: CPU 모델·플래그의 기능 게이트와 검증·별칭 고정, CPU 보기(VM·노드·모델·플래그·기준), 설정 `cpu`, 노드
//! 다시 읽기(스테이지 5), 리더의 클러스터 기준 기록과 마이그레이션의 CPU 확인(스테이지 4).

mod common;

use std::time::Duration;

use common::{Env, WAIT, vm_request};
use orbivisor_api::{ClusterJanitor, ClusterJanitorConfig};
use orbivisor_types::audit::AuditQuery;
use orbivisor_types::cluster::ClusterSpec;
use orbivisor_types::cpu::{
    Baseline, BaselineMode, BaselineSource, Bits, CpuCatalog, CpuSettings, CpuSource, NodeCpu,
    ResolvedCpu,
};
use orbivisor_types::node::{CPU_MODELS, FEATURES, Node, NodeSpec, NodeStatus};
use orbivisor_types::resource::Resource;
use orbivisor_types::settings::{SETTINGS_NAME, Settings, SettingsSpec};
use orbivisor_types::task::{TaskOp, TaskSpec};
use orbivisor_types::vm::{
    Arch, CpuSpec, MachineType, PowerState, RunningHardware, Vm, VmPhase, VmSpec,
};
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

/// x86_64(q35) VM 만들기 요청
fn x86_request(name: &str, model: &str, flags: &[&str]) -> serde_json::Value {
    let mut req = vm_request(name);
    req["spec"]["machine"] = json!("q35");
    req["spec"]["cpu"]["model"] = json!(model);
    if !flags.is_empty() {
        req["spec"]["cpu"]["flags"] = json!(flags);
    }
    req
}

/// 옛 노드가 있으면 `host`·`max` 밖의 모델과 플래그는 검증보다 먼저 409 `cluster_upgrading`이다. 문법이 틀린 모델·플래그는
/// JSON을 풀지 못한다.
#[tokio::test]
async fn cpu_models_and_flags_are_gated() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for (name, model) in [("h", "host"), ("m", "max")] {
        let mut req = vm_request(name);
        req["spec"]["cpu"]["model"] = json!(model);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["spec"]["cpu"]["model"], model);
        assert!(body["spec"]["cpu"].get("flags").is_none(), "{body}");
    }
    drop_cpu_models(&env).await;
    let tries = [
        x86_request("new", "x86-64-v3", &[]),
        x86_request("new", "cluster", &[]),
        x86_request("new", "Icelake-Server-v7", &[]),
        x86_request("new", "host", &["-avx512vbmi"]),
    ];
    for req in tries {
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{req}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "cpu": { "mask": ["avx512vbmi"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    for (model, flags) in [("base", json!([])), ("host", json!(["avx"]))] {
        let mut req = vm_request("bad");
        req["spec"]["cpu"]["model"] = json!(model);
        req["spec"]["cpu"]["flags"] = flags;
        let (status, body) = admin.post("/vms", req).await;
        assert!(status.is_client_error(), "{model}: {status} {body}");
    }
}

/// 만들 때 별칭을 버전 모델로 고정하고(대소문자는 카탈로그 표기로) 감사에 적는다. 모르는 모델·플래그, 다른 아키텍처,
/// 숨김 목록의 `+flag`, 겹친 플래그는 §13의 400 코드다. 바꾸기에서 모델이 그대로면 다시 해석하지 않는다.
#[tokio::test]
async fn models_are_pinned_and_checked_against_the_node_catalogs() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (strong, _) = add_catalogs(&env).await;
    add_x86_node(&env, "n1", &strong).await;

    let (status, body) = admin
        .post(
            "/vms",
            x86_request("web", "Icelake-Server", &["-avx512vbmi"]),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["cpu"]["model"], "Icelake-Server-v7");
    assert_eq!(body["spec"]["cpu"]["flags"], json!(["-avx512vbmi"]));
    let want = "vm create web cpu=Icelake-Server-v7 (from Icelake-Server) cpu.flags=-avx512vbmi";
    let details = audit_details(&env, "POST /vms", |d| d.iter().any(|x| x == want)).await;
    assert!(details.contains(&want.to_string()), "{details:?}");
    let (status, body) = admin
        .post("/vms", x86_request("lower", "skylake-server-v5", &[]))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["cpu"]["model"], "Skylake-Server-v5");
    for model in ["x86-64-v3", "cluster"] {
        let (status, body) = admin
            .post(
                "/vms",
                x86_request(&format!("l{}", model.len()), model, &[]),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{model}: {body}");
    }

    let bad = [
        (
            x86_request("x", "Icelake-Servr-v7", &[]),
            "unknown_cpu_model",
        ),
        (
            x86_request("x", "host", &["-nonexistent"]),
            "unknown_cpu_flag",
        ),
        (
            x86_request("x", "host", &["-aes", "+aes"]),
            "invalid_cpu_flags",
        ),
        (
            {
                let mut r = vm_request("x");
                r["spec"]["cpu"]["model"] = json!("x86-64-v3");
                r
            },
            "cpu_model_arch",
        ),
        (
            {
                let mut r = vm_request("x");
                r["spec"]["cpu"]["model"] = json!("Skylake-Server-v5");
                r
            },
            "cpu_model_arch",
        ),
    ];
    for (req, code) in bad {
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{req}: {body}");
        assert_eq!(body["code"], code, "{req}: {body}");
    }
    let (_, body) = admin
        .post("/vms", x86_request("x", "Icelake-Servr-v7", &[]))
        .await;
    assert_eq!(body["details"]["similar"][0], "Icelake-Server-v7", "{body}");

    // 숨김 목록의 이름은 켤 수 없다.
    set_cpu_settings(
        &env,
        CpuSettings {
            mask: vec!["avx512vbmi".into()],
            ..Default::default()
        },
    )
    .await;
    let (status, body) = admin
        .post("/vms", x86_request("x", "host", &["+avx512vbmi"]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "cpu_flag_masked");

    // 별칭의 뜻이 노드마다 다르면 버전 이름을 요구한다.
    let mut other = orbivisor_driver::mock_cpu_probe().catalog;
    other.qemu = "8.2.2".into();
    other.models[0].alias_of = Some("Icelake-Server-v1".into());
    let hash = other.hash();
    let r: CpuCatalog = Resource::new(&hash, other);
    env.cluster.create(&r).await.unwrap();
    add_x86_node(&env, "n2", &hash).await;
    let (status, body) = admin
        .post("/vms", x86_request("x", "Icelake-Server", &[]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "cpu_model_ambiguous");
    assert_eq!(
        body["details"]["candidates"],
        json!(["Icelake-Server-v1", "Icelake-Server-v7"])
    );

    // 모델을 그대로 두는 바꾸기는 다시 해석하지 않는다(코어만 바꿈).
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    let mut spec = serde_json::to_value(&vm.spec).unwrap();
    spec["cpu"]["cores"] = json!(2);
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    spec["cpu"]["flags"] = json!(["-avx512vbmi", "-aes"]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let want = "vm set web cpu.flags=-avx512vbmi,-aes";
    let details = audit_details(&env, "PUT /vms/{key}", |d| d.iter().any(|x| x == want)).await;
    assert!(details.contains(&want.to_string()), "{details:?}");
}

/// `GET /vms/{key}/cpu`·`/nodes/{key}/cpu`·`/cpu/models`·`/cpu/flags`·`/cpu/baseline`
#[tokio::test]
async fn cpu_views_show_what_each_node_can_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (strong, weak) = add_catalogs(&env).await;
    add_x86_node(&env, "n1", &strong).await;
    add_x86_node(&env, "n4", &weak).await;
    let (status, body) = admin
        .post("/vms", x86_request("web", "Icelake-Server-v7", &[]))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = admin.get("/vms/web/cpu").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["model"], "Icelake-Server-v7");
    assert_eq!(
        body["nodes"],
        json!([
            { "node": "n1", "can_run": true },
            {
                "node": "n4",
                "can_run": false,
                "missing": ["avx512vbmi"],
                "reason": "node \"n4\" cannot run CPU model Icelake-Server-v7 (missing avx512vbmi)"
            }
        ])
    );

    let (status, body) = admin.get("/nodes/n4/cpu").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cpu"]["catalog"], weak);
    let models = body["models"].as_array().unwrap();
    let v7 = models
        .iter()
        .find(|m| m["name"] == "Icelake-Server-v7")
        .unwrap();
    assert_eq!(v7["usable"], false);
    assert_eq!(v7["missing"], json!(["avx512vbmi"]));
    let alias = models
        .iter()
        .find(|m| m["name"] == "Icelake-Server")
        .unwrap();
    assert_eq!(alias["alias_of"], "Icelake-Server-v7");
    let flag = body["flags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "avx512vbmi")
        .unwrap()
        .clone();
    assert_eq!(flag, json!({ "name": "avx512vbmi", "supported": false }));

    let (status, body) = admin.get("/cpu/models?arch=x86_64").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["nodes"], 2);
    let names: Vec<&str> = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "host",
            "max",
            "cluster",
            "x86-64-v2",
            "x86-64-v2-aes",
            "x86-64-v3",
            "x86-64-v4",
            "Icelake-Server",
            "Icelake-Server-v7",
            "Skylake-Server-v5"
        ]
    );
    let entry = |name: &str| {
        body["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(entry("Icelake-Server-v7")["usable"], 1);
    assert_eq!(entry("Skylake-Server-v5")["usable"], 2);
    // 기준이 없으면 `current`가 없다(`host`라 적으면 기준이 있는 것과 구별되지 않는다 — 리뷰 1 웹 #1).
    assert!(entry("cluster").get("current").is_none(), "{body}");
    let info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
    let mut status = info.status.clone();
    status.cpu_baselines.insert(
        Arch::X86_64,
        Baseline {
            model: "Skylake-Server-v5".into(),
            flags: Default::default(),
            mode: BaselineSource::Pinned,
            nodes: vec![],
            limited_by: vec![],
            since: 1,
            previous: None,
            warnings: vec![],
        },
    );
    env.cluster
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    let (_, now) = admin.get("/cpu/models?arch=x86_64").await;
    let cluster = now["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "cluster")
        .unwrap()
        .clone();
    assert_eq!(cluster["current"], "Skylake-Server-v5", "{now}");
    assert_eq!(entry("host")["usable"], 2);
    let (_, usable) = admin
        .get("/cpu/models?arch=x86_64&node=n4&usable=true")
        .await;
    assert!(
        usable["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "Icelake-Server-v7"),
        "{usable}"
    );
    // aarch64(이 노드)는 카탈로그가 없어 host·max·cluster만 켠다.
    let (_, arm) = admin.get("/cpu/models?arch=aarch64").await;
    assert_eq!(arm["models"].as_array().unwrap().len(), 3, "{arm}");

    let (status, body) = admin.get("/cpu/flags?arch=x86_64").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["nodes"], 2);
    assert!(
        body["flags"].as_array().unwrap().contains(
            &json!({ "name": "avx512vbmi", "supported": 1, "known": 2, "restricted": true })
        ),
        "{body}"
    );

    let (status, body) = admin.get("/cpu/baseline").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["mode"], "off");
    assert!(
        body["arches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["cluster_vms"] == 0)
    );
}

/// 설정 `cpu`: 숨김 목록은 맞춰 저장하고 영향 받는 실행 중 VM 수를 돌려준다. 모르는 이름은 400, 고정 기준을 켤 수 없는
/// 노드가 있으면 409 `cpu_baseline_unsupported`이고 `force`로 넘는다. 별칭은 버전 모델로 고정한다.
#[tokio::test]
async fn cpu_settings_are_checked_and_count_the_affected_vms() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (strong, weak) = add_catalogs(&env).await;
    let n1 = add_x86_node(&env, "n1", &strong).await;
    add_x86_node(&env, "n4", &weak).await;
    running_vm(&env, "web", n1, "host", resolved("host", CpuSource::Spec)).await;
    running_vm(
        &env,
        "db",
        n1,
        "Skylake-Server-v5",
        resolved("Skylake-Server-v5", CpuSource::Spec),
    )
    .await;

    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "cpu": { "mask": ["AVX512VBMI", "aes", "aes"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["cpu"]["mask"], json!(["aes", "avx512vbmi"]));
    assert_eq!(body["affected_running_vms"], 2, "{body}");
    let want = "settings cpu.mask=aes,avx512vbmi";
    let details = audit_details(&env, "PATCH /settings", |d| d.iter().any(|x| x == want)).await;
    assert!(details.contains(&want.to_string()), "{details:?}");
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "cpu": { "mask": ["nonexistent"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "unknown_cpu_flag");

    // 숨김 목록이 `avx512vbmi`를 끄면 약한 노드도 고정 기준을 켤 수 있으므로 지운 뒤에 고정한다.
    let pin = json!({ "cpu": { "mask": [], "pinned": [ { "arch": "x86_64", "model": "Icelake-Server" } ] } });
    let (status, body) = admin.patch("/settings", pin.clone(), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cpu_baseline_unsupported");
    assert_eq!(body["details"]["nodes"], json!(["n4"]));
    let mut forced = pin;
    forced["cpu"]["force"] = json!(true);
    let (status, body) = admin.patch("/settings", forced, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["cpu"]["pinned"],
        json!([{ "arch": "x86_64", "model": "Icelake-Server-v7" }])
    );
    // 리뷰 2 #7: 409를 넘었음을 감사에 남긴다.
    let forced = |d: &String| {
        d.contains("cpu.pinned=x86_64:Icelake-Server-v7") && d.contains("cpu.force=true")
    };
    let details = audit_details(&env, "PATCH /settings", |d| d.iter().any(forced)).await;
    assert!(details.iter().any(forced), "{details:?}");
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "cpu": { "pinned": [ { "arch": "x86_64", "model": "host" } ] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 모두 지우면 설정에서 `cpu`가 빠진다.
    let (status, body) = admin
        .patch("/settings", json!({ "cpu": { "pinned": [] } }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("cpu").is_none(), "{body}");
    assert_eq!(body["affected_running_vms"], 0, "{body}");
}

/// 노드 다시 읽기는 그 노드의 Task `node-cpu-probe`다(끝나지 않은 것이 있으면 그것). 위트니스는 400.
#[tokio::test]
async fn a_node_cpu_refresh_is_a_task_on_that_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let (status, body) = admin
        .post(&format!("/nodes/{}/cpu/refresh", me.name), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["spec"]["op"]["kind"], "node-cpu-probe");
    assert_eq!(body["spec"]["node"], env.node.to_string());
    // 이 시험 노드의 실행기는 다시 읽기가 없어 실패한다.
    let id = body["id"].as_str().unwrap().to_string();
    let start = std::time::Instant::now();
    loop {
        let (_, task) = admin.get(&format!("/tasks/{id}")).await;
        if task["status"]["state"] == "failed" {
            break;
        }
        assert!(start.elapsed() < WAIT, "{task}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop_cpu_models(&env).await;
    let (status, body) = admin
        .post(&format!("/nodes/{}/cpu/refresh", me.name), json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

/// Mock 카탈로그(`Icelake-Server-v7`은 `avx512vbmi`가 없어 못 켬 — 약한 노드)와 그것을 켤 수 있는 강한 카탈로그를 만들고
/// (강한 것, 약한 것)의 해시를 돌려준다.
async fn add_catalogs(env: &Env) -> (String, String) {
    let weak = orbivisor_driver::mock_cpu_probe().catalog;
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
            sdn: None,
            host_resources: None,
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.status = NodeStatus {
        sdn: None,
        qos: false,
        topology: None,
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
        // 이 노드와 같은 빌드(설정 쓰기가 업그레이드 중이 아니게)
        version: env
            .cluster
            .get::<NodeSpec>(env.node)
            .unwrap()
            .unwrap()
            .status
            .version,
        updates: None,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        cluster_addresses: vec![],
        shutdown: None,
        fence_check: None,
        interfaces: vec![],
        checks: vec![],
        hotplug: None,
        firmware_caps: None,
        backup_stores: Vec::new(),
        file_restore: None,
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 설정 `cpu`를 저장소에 바로 쓴다(리더 기록 시험이 API의 카탈로그 검사를 거치지 않게).
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

/// 노드 `node`에 고정된 x86_64 VM 스펙(API를 거치지 않는다)
fn x86_spec(model: &str, node: Uuid, power: PowerState) -> VmSpec {
    VmSpec {
        hugepages: None,
        numa: None,
        cpu: CpuSpec {
            emulator: None,
            policy: Default::default(),
            thread_policy: None,
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
        dropped_mask: Vec::new(),
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
/// `action`이 같은 감사 항목의 설명. 감사는 응답 뒤에 따로 적히므로 `ready`가 참이 될 때까지(아니면 `WAIT`까지)
/// 다시 읽는다 — 느린 러너에서 아직 적히지 않은 항목을 놓치지 않게.
async fn audit_details(env: &Env, action: &str, ready: impl Fn(&[String]) -> bool) -> Vec<String> {
    let start = std::time::Instant::now();
    loop {
        let details: Vec<String> = env
            .state
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
            .collect();
        if ready(&details) || start.elapsed() > WAIT {
            return details;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
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
    let want = "cpu baseline x86_64 auto: Icelake-Server-v7 -> Skylake-Server-v5 (node n4)";
    let audits = audit_details(&env, "cpu.baseline", |d| d.iter().any(|x| x == want)).await;
    assert!(audits.contains(&want.to_string()), "{audits:?}");

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
        let details = audit_details(&env, "POST /vms/{key}/migrate", |_| true).await;
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

/// 리뷰 1 I2: 껐다 켜기(reboot)는 같은 노드에서 새 CPU로 켠다. 그 노드가 켤 수 없으면 끄기 전에 409다.
#[tokio::test]
async fn a_power_cycle_is_refused_when_the_node_cannot_run_the_new_cpu() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (strong, weak) = add_catalogs(&env).await;
    let n1 = add_x86_node(&env, "n1", &strong).await;
    let n4 = add_x86_node(&env, "n4", &weak).await;
    // 둘 다 Skylake로 돌다가 스펙을 Icelake로 바꿨다(보류 cpu).
    for (name, node) in [("weak", n4), ("strong", n1)] {
        running_vm(
            &env,
            name,
            node,
            "Icelake-Server-v7",
            resolved("Skylake-Server-v5", CpuSource::Spec),
        )
        .await;
    }
    let (status, body) = admin.post("/vms/weak/actions/reboot", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_capacity", "{body}");
    assert_eq!(
        body["message"],
        "node \"n4\" cannot run CPU model Icelake-Server-v7 (missing avx512vbmi); migrate VM \"weak\" to a node that can run its CPU, then power cycle it"
    );
    let weak_vm = env.cluster.get_by_name::<VmSpec>("weak").unwrap().unwrap();
    assert_eq!(weak_vm.spec.power, PowerState::Running);
    let (status, body) = admin.post("/vms/strong/actions/reboot", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
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

/// 리뷰 1 웹 #1: 업그레이드 중에는 기록된 기준이 있어도 `cluster`의 `current`가 없다(웹이 새 VM을 `cluster`로 두지 않게 —
/// 그때 `host`·`max` 밖의 모델은 409 `cluster_upgrading`이다).
#[tokio::test]
async fn the_cluster_entry_has_no_current_baseline_during_an_upgrade() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (strong, _) = add_catalogs(&env).await;
    add_x86_node(&env, "n1", &strong).await;
    let info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
    let mut status = info.status.clone();
    status.cpu_baselines.insert(
        Arch::X86_64,
        Baseline {
            model: "Skylake-Server-v5".into(),
            flags: Default::default(),
            mode: BaselineSource::Pinned,
            nodes: vec![],
            limited_by: vec![],
            since: 1,
            previous: None,
            warnings: vec![],
        },
    );
    env.cluster
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    let current = |body: &serde_json::Value| {
        body["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "cluster")
            .unwrap()
            .get("current")
            .cloned()
    };
    let (_, body) = admin.get("/cpu/models?arch=x86_64").await;
    assert_eq!(current(&body), Some(json!("Skylake-Server-v5")), "{body}");
    drop_cpu_models(&env).await;
    let (status, body) = admin.get("/cpu/models?arch=x86_64").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(current(&body), None, "{body}");
}

/// 리뷰 2 #9: `Inventory`는 노드가 알린 카탈로그만 읽고 해시마다 한 번만 푼다(같은 `Arc`).
#[tokio::test]
async fn the_inventory_reads_only_announced_catalogs_and_parses_each_once() {
    let env = Env::start().await;
    let (strong, weak) = add_catalogs(&env).await;
    add_x86_node(&env, "n1", &strong).await;
    let a = orbivisor_api::scheduler::Inventory::load(&env.cluster).unwrap();
    assert!(a.cpu_catalogs.contains_key(&strong));
    assert!(!a.cpu_catalogs.contains_key(&weak));
    let b = orbivisor_api::scheduler::Inventory::load(&env.cluster).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &a.cpu_catalogs[&strong],
        &b.cpu_catalogs[&strong]
    ));
}
