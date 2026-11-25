//! 계획 73 선언형 적용(§8·§14): 의존 차례로 만들고 다시 적용하면 모두 `unchanged`, 칸 하나 바꾸면 update 하나, 세 갈래
//! (앞 선언에서 빠진 칸은 기본값으로·선언하지 않은 칸은 그대로), 미리 확인은 아무것도 쓰지 않음, 가지치기(데이터 종류는
//! `prune_data`·주석은 건너뜀), 내보내기 → 적용 왕복, 숨은 폴더·네트워크가 새지 않음, 저장한 계획 stale·다른 사용자,
//! 실패 단계에서 멈추고 다시 적용하면 남은 것만, 옛 노드 → 409, 이력 출처 `apply`.

mod common;

use std::sync::Arc;

use common::{Client, Env, FakeStorage};
use opl_types::apply_set::ApplySetSpec;
use opl_types::history::ChangeSource;
use opl_types::manifest::APPLY_SET_LABEL;
use opl_types::node::{DECLARATIVE, NodeSpec};
use opl_types::resource::{Kind, Resource};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::storage::VolumeSpec;
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};

const MIB: u64 = 1 << 20;

/// 파일 없는 스토리지로 실행기를 띄우고(CI `check`에는 `qemu-img`가 없다) 풀 `local`을 만든다.
async fn start() -> (Env, Client) {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_with(Arc::new(FakeStorage::default()))
        .await;
    let admin = env.admin().await;
    let dir = env.dir.path().join("local");
    std::fs::create_dir_all(&dir).unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": dir } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    (env, admin)
}

fn doc(kind: &str, name: &str, spec: Value) -> Value {
    json!({ "apiVersion": "orbivisor.io/v1", "kind": kind, "metadata": { "name": name }, "spec": spec })
}

fn vm(name: &str, folder: &str, memory: u32) -> Value {
    let mut d = doc(
        "Vm",
        name,
        json!({
            "cpu": { "cores": 1 },
            "memory_mib": memory,
            "machine": "virt",
            "disks": [{ "volume": format!("{name}-root"), "bus": "virtio-blk" }],
            "nics": [{ "network": "user", "model": "virtio" }],
        }),
    );
    d["metadata"]["folder"] = json!(folder);
    d["state"] = json!("stopped");
    d
}

/// 폴더 /prod, 보안 그룹, 볼륨, VM — 일부러 거꾸로 적는다(차례는 서버가 정한다).
fn web(memory: u32) -> Vec<Value> {
    vec![
        vm("web01", "/prod", memory),
        doc(
            "Volume",
            "web01-root",
            json!({ "pool": "local", "size_bytes": MIB }),
        ),
        doc("SecurityGroup", "web-sg", json!({ "rules": [] })),
        doc("Folder", "prod", json!({ "kind": "vm" })),
    ]
}

async fn dry_run(client: &Client, body: Value) -> Value {
    let (status, plan) = client.post("/apply?dry_run=true", body).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    plan
}

/// 적용하고 Task가 끝나기를 기다린다(끝난 Task를 돌려준다).
async fn apply(client: &Client, body: Value) -> Value {
    let (status, started) = client.post("/apply", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    client.wait_task(&started["task"]).await
}

fn summary(plan: &Value) -> [u64; 5] {
    let s = &plan["summary"];
    ["create", "update", "delete", "unchanged", "blocked"].map(|k| s[k].as_u64().unwrap())
}

fn actions(plan: &Value) -> Vec<(String, String, String)> {
    plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_string(),
                s["name"].as_str().unwrap().to_string(),
                s["action"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn apply_creates_in_order_and_reapplying_changes_nothing() {
    let (env, admin) = start().await;
    let body = json!({ "documents": web(256), "apply_set": "web" });

    // 미리 확인: 의존 차례, 아무것도 쓰지 않는다.
    let before = env.cluster.revision();
    let plan = dry_run(&admin, body.clone()).await;
    assert_eq!(summary(&plan), [4, 0, 0, 0, 0], "{plan}");
    let order: Vec<_> = actions(&plan).into_iter().map(|(k, _, _)| k).collect();
    assert_eq!(order, ["Folder", "SecurityGroup", "Volume", "Vm"]);
    let vm_step = &plan["steps"][3];
    assert_eq!(vm_step["after"], json!([3, 1]), "{vm_step}");
    assert!(
        vm_step["diff"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "memory_mib" && c["op"] == "add")
    );
    assert_eq!(env.cluster.revision(), before, "a dry run writes nothing");
    assert!(
        env.cluster
            .get_by_name::<VmSpec>("web01")
            .unwrap()
            .is_none()
    );

    let task = apply(&admin, body.clone()).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = env.cluster.get_by_name::<VmSpec>("web01").unwrap().unwrap();
    assert_eq!(
        vm.labels.get(APPLY_SET_LABEL).map(String::as_str),
        Some("web")
    );
    assert!(vm.spec.folder.is_some());
    assert_eq!(vm.spec.disks[0].volume, "web01-root");
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>("web01-root")
        .unwrap()
        .unwrap();
    assert_eq!(volume.spec.format, opl_types::runtime::ImageFormat::Qcow2);
    // 설정 이력: 출처 apply, 묶음 id, Task를 만든 사용자
    let set = env
        .cluster
        .get_by_name::<ApplySetSpec>("web")
        .unwrap()
        .unwrap();
    let rows = env.cluster.history_of(Kind::Vm, vm.id).unwrap();
    assert_eq!(rows[0].source, ChangeSource::Apply, "{rows:?}");
    assert_eq!(rows[0].apply_set, Some(set.id));
    assert_eq!(rows[0].actor.user.as_deref(), Some("admin"));
    assert_eq!(set.status.objects.len(), 4, "{:?}", set.status);
    assert!(set.status.last_error.is_none());

    // 다시 적용하면 모두 그대로다(서버가 채운 MAC·머신·형식이 있어도).
    let plan = dry_run(&admin, body.clone()).await;
    assert_eq!(summary(&plan), [0, 0, 0, 4, 0], "{plan}");

    // 칸 하나를 바꾸면 그 단계만 바뀐다.
    let changed = json!({ "documents": web(512), "apply_set": "web" });
    let plan = dry_run(&admin, changed.clone()).await;
    assert_eq!(summary(&plan), [0, 1, 0, 3, 0], "{plan}");
    let step = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["action"] == "update")
        .unwrap();
    assert_eq!(step["name"], "web01");
    assert_eq!(
        step["diff"],
        json!([{ "path": "memory_mib", "op": "replace", "old": 256, "new": 512 }])
    );
    let task = apply(&admin, changed).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = env.cluster.get_by_name::<VmSpec>("web01").unwrap().unwrap();
    assert_eq!(vm.spec.memory_mib, 512);

    // 내보낸 것을 그대로 적용하면 모두 그대로다(왕복).
    let (status, exported) = admin.get("/manifests?apply_set=web").await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let docs = exported["documents"].as_array().unwrap().clone();
    assert_eq!(docs.len(), 4, "{exported}");
    let vm_doc = docs.iter().find(|d| d["kind"] == "Vm").unwrap();
    assert_eq!(vm_doc["metadata"]["folder"], "/prod");
    assert_eq!(vm_doc["state"], "stopped");
    assert!(vm_doc["spec"].get("folder").is_none(), "{vm_doc}");
    assert!(vm_doc["spec"]["nics"][0]["mac"].is_string(), "{vm_doc}");
    let plan = dry_run(&admin, json!({ "documents": docs, "apply_set": "web" })).await;
    assert_eq!(summary(&plan), [0, 0, 0, 4, 0], "{plan}");
    // 하나만 읽기
    let (status, one) = admin.get("/manifests/vm/web01").await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["kind"], "Vm");
    let (status, _) = admin.get("/manifests/Vm/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 적용 묶음 보기
    let (status, sets) = admin.get("/apply-sets").await;
    assert_eq!(status, StatusCode::OK, "{sets}");
    assert_eq!(sets["items"][0]["name"], "web");
}

#[tokio::test]
async fn undeclared_fields_stay_and_dropped_fields_go_back_to_defaults() {
    let (env, admin) = start().await;
    let sg = |spec: Value| json!({ "documents": [doc("SecurityGroup", "web-sg", spec)], "apply_set": "net" });
    let task = apply(
        &admin,
        sg(json!({ "description": "web tier", "rules": [] })),
    )
    .await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // 선언하지 않은 칸을 밖에서 바꿔도(여기서는 레이블) 다시 적용이 덮지 않는다.
    let group = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    let mut labels = group.labels.clone();
    labels.insert("team".into(), "a".into());
    env.cluster
        .update_spec(group.id, None, labels, &group.spec)
        .await
        .unwrap();
    let plan = dry_run(
        &admin,
        sg(json!({ "description": "web tier", "rules": [] })),
    )
    .await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");
    // 앞 선언에 있던 설명을 빼면 기본값(없음)으로 돌아간다.
    let plan = dry_run(&admin, sg(json!({ "rules": [] }))).await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    assert_eq!(plan["steps"][0]["diff"][0]["path"], "description");
    assert_eq!(plan["steps"][0]["diff"][0]["op"], "remove");
    let task = apply(&admin, sg(json!({ "rules": [] }))).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let group = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    assert_eq!(group.spec.description, None);
    assert_eq!(group.labels.get("team").map(String::as_str), Some("a"));
}

#[tokio::test]
async fn pruning_keeps_data_and_annotated_objects_unless_asked() {
    let (env, admin) = start().await;
    let mut docs = web(256);
    // 보안 그룹은 가지치기에서 건너뛴다.
    docs[2]["metadata"]["annotations"] = json!({ "orbivisor.io/prune": "false" });
    let task = apply(&admin, json!({ "documents": docs, "apply_set": "web" })).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // 폴더만 남기고 가지치기: VM·볼륨은 데이터라 막힌다(아무것도 지우지 않는다).
    let keep = json!({ "documents": [doc("Folder", "prod", json!({ "kind": "vm" }))], "apply_set": "web", "prune": true });
    let (status, body) = admin.post("/apply", keep.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "plan_blocked");
    let steps = actions(&body["details"]);
    assert!(
        steps.contains(&("Vm".into(), "web01".into(), "blocked".into())),
        "{steps:?}"
    );
    assert!(steps.contains(&("Volume".into(), "web01-root".into(), "blocked".into())));
    assert!(
        !steps.iter().any(|(k, _, _)| k == "SecurityGroup"),
        "{steps:?}"
    );
    assert!(
        env.cluster
            .get_by_name::<VmSpec>("web01")
            .unwrap()
            .is_some()
    );

    // `prune_data`면 지운다(VM이 먼저, 볼륨이 뒤).
    let mut all = keep.clone();
    all["prune_data"] = json!(true);
    let plan = dry_run(&admin, all.clone()).await;
    let steps = actions(&plan);
    assert_eq!(
        steps,
        [
            ("Folder".into(), "prod".into(), "unchanged".into()),
            ("Vm".into(), "web01".into(), "delete".into()),
            ("Volume".into(), "web01-root".into(), "delete".into()),
        ]
    );
    let task = apply(&admin, all).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(
        env.cluster
            .get_by_name::<VmSpec>("web01")
            .unwrap()
            .is_none()
    );
    assert!(
        env.cluster
            .get_by_name::<VolumeSpec>("web01-root")
            .unwrap()
            .is_none()
    );
    assert!(
        env.cluster
            .get_by_name::<SecurityGroupSpec>("web-sg")
            .unwrap()
            .is_some()
    );
    // 묶음을 지우면 리소스는 남고 레이블만 떨어진다.
    let (status, body) = admin.delete("/apply-sets/web").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let group = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    assert!(
        !group.labels.contains_key(APPLY_SET_LABEL),
        "{:?}",
        group.labels
    );
    assert!(
        env.cluster
            .get_by_name::<ApplySetSpec>("web")
            .unwrap()
            .is_none()
    );
}

/// 전역 역할 `none`인 사용자에게 폴더 하나의 operator를 주고 로그인한다.
async fn folder_user(env: &Env, admin: &Client, name: &str, folder: &str) -> Client {
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": name, "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": body["id"] },
                "role": { "type": "builtin", "role": "operator" },
                "target": { "type": "folder", "id": folder },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.login(name, "password-1").await
}

async fn folder(admin: &Client, name: &str, kind: &str) -> String {
    let (status, body) = admin
        .post("/folders", json!({ "name": name, "kind": kind }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn hidden_folders_and_networks_read_as_missing() {
    let (env, admin) = start().await;
    let mine = folder(&admin, "mine", "vm").await;
    folder(&admin, "theirs", "vm").await;
    let nets = folder(&admin, "nets", "network").await;
    let net: opl_types::network::Network = Resource::new(
        "hidden",
        serde_json::from_value(json!({ "vxlan": { "vni": 4242 }, "folder": nets })).unwrap(),
    );
    env.cluster.create(&net).await.unwrap();
    let alice = folder_user(&env, &admin, "alice", &mine).await;

    let with_nic = |name: &str, folder: &str, network: &str| {
        let mut d = vm(name, folder, 256);
        d["spec"]["disks"] = json!([]);
        d["spec"]["nics"] = json!([{ "network": network, "model": "virtio" }]);
        d
    };
    let plan = dry_run(
        &alice,
        json!({ "documents": [
            with_nic("a", "/mine", "hidden"),
            with_nic("b", "/mine", "nope"),
            with_nic("c", "/theirs", "user"),
            with_nic("d", "/nowhere", "user"),
        ] }),
    )
    .await;
    let problems: Vec<(String, String)> = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let p = &s["problems"][0];
            (
                p["code"].as_str().unwrap().into(),
                p["message"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        problems,
        [
            (
                "missing_reference".into(),
                "Network \"hidden\" not found".into()
            ),
            (
                "missing_reference".into(),
                "Network \"nope\" not found".into()
            ),
            (
                "missing_reference".into(),
                "folder \"/theirs\" not found".into()
            ),
            (
                "missing_reference".into(),
                "folder \"/nowhere\" not found".into()
            ),
        ],
        "{plan}"
    );
    // 자기 폴더의 VM은 계획대로다.
    let plan = dry_run(
        &alice,
        json!({ "documents": [with_nic("e", "/mine", "user")] }),
    )
    .await;
    assert_eq!(summary(&plan), [1, 0, 0, 0, 0], "{plan}");
    // 내보내기도 볼 수 있는 것만
    let (status, exported) = alice.get("/manifests?kinds=network,folder").await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let names: Vec<_> = exported["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["metadata"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["mine"], "{exported}");
}

#[tokio::test]
async fn saved_plans_go_stale_and_belong_to_their_user() {
    let (env, admin) = start().await;
    let body = json!({ "documents": [doc("SecurityGroup", "web-sg", json!({ "rules": [] }))] });
    let task = apply(&admin, body.clone()).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let changed = json!({ "documents": [doc("SecurityGroup", "web-sg", json!({ "description": "x", "rules": [] }))] });
    let plan = dry_run(&admin, changed.clone()).await;
    let id = plan["plan"].clone();

    // 다른 사용자는 그 계획을 모른다.
    let (status, _) = admin
        .post(
            "/users",
            json!({ "name": "bob", "password": "password-1", "role": "admin" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let bob = env.login("bob", "password-1").await;
    let (status, body) = bob.post("/apply", json!({ "plan": id })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "plan_not_found");
    // 문서가 다르면 412
    let (status, body) = admin
        .post("/apply", json!({ "plan": id, "documents": web(256) }))
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    // 그사이 바뀌면 412
    let group = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    env.cluster
        .update_spec(
            group.id,
            None,
            [("x".to_string(), "y".to_string())].into(),
            &group.spec,
        )
        .await
        .unwrap();
    let (status, body) = admin.post("/apply", json!({ "plan": id })).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_eq!(body["code"], "plan_stale");
    // 새 계획은 적용된다(같은 문서를 함께 보내도 된다).
    let plan = dry_run(&admin, changed.clone()).await;
    let mut with_plan = changed;
    with_plan["plan"] = plan["plan"].clone();
    let task = apply(&admin, with_plan).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let group = env
        .cluster
        .get_by_name::<SecurityGroupSpec>("web-sg")
        .unwrap()
        .unwrap();
    assert_eq!(group.spec.description.as_deref(), Some("x"));
}

#[tokio::test]
async fn a_failed_step_stops_and_reapplying_does_the_rest() {
    let (env, admin) = start().await;
    // 라우터는 SDN(OVN)이 없으면 만들 수 없다(409 `sdn_not_ready`) — 계획은 지나고 적용에서 실패한다.
    let docs = json!({
        "documents": [
            doc("Router", "r1", json!({})),
            doc("Folder", "a", json!({ "kind": "network" })),
        ],
        "apply_set": "net",
    });
    let task = apply(&admin, docs).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.contains("step 2/2") && error.contains("Router"),
        "{error}"
    );
    let set = env
        .cluster
        .get_by_name::<ApplySetSpec>("net")
        .unwrap()
        .unwrap();
    assert_eq!(set.status.objects.len(), 1, "{:?}", set.status);
    assert!(set.status.last_error.is_some());
    // 라우터를 빼고 다시 적용하면 폴더는 그대로다.
    let plan = dry_run(
        &admin,
        json!({ "documents": [doc("Folder", "a", json!({ "kind": "network" }))], "apply_set": "net" }),
    )
    .await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");
}

#[tokio::test]
async fn bad_documents_are_refused_with_their_index() {
    let (_env, admin) = start().await;
    let cases = [
        (
            json!([{ "apiVersion": "orbivisor.io/v1", "kind": "Vm", "metadata": { "name": "a" }, "spek": {} }]),
            "manifest_invalid",
        ),
        (
            json!([doc(
                "Vm",
                "a",
                json!({ "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "colour": 1 })
            )]),
            "manifest_invalid",
        ),
        (
            json!([
                doc("SecurityGroup", "a", json!({})),
                doc("SecurityGroup", "a", json!({}))
            ]),
            "manifest_duplicate",
        ),
        // 모든 종류에 어댑터가 있다(스테이지 3): 빠진 칸은 그 종류의 꼴 오류다.
        (
            json!([doc("BackupJob", "a", json!({}))]),
            "manifest_invalid",
        ),
    ];
    for (documents, code) in cases {
        let (status, body) = admin
            .post("/apply?dry_run=true", json!({ "documents": documents }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], code, "{body}");
    }
}

#[tokio::test]
async fn old_nodes_make_apply_wait_for_the_upgrade() {
    let (env, admin) = start().await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != DECLARATIVE);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    for (code, body) in [
        admin
            .post("/apply?dry_run=true", json!({ "documents": [] }))
            .await,
        admin.get("/manifests").await,
        admin.get("/apply-sets").await,
    ] {
        assert_eq!(code, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "cluster_upgrading");
    }
}

#[tokio::test]
async fn the_engine_is_in_the_api_document() {
    let (env, _admin) = start().await;
    let (status, doc) = env.anonymous().get("/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    for path in [
        "/apply",
        "/manifests",
        "/manifests/{kind}/{key}",
        "/apply-sets",
        "/apply-sets/{key}",
    ] {
        assert!(doc["paths"].get(path).is_some(), "{path}");
    }
    for schema in ["ManifestVm", "ManifestVolume", "ManifestFolder", "PlanView"] {
        assert!(
            doc["components"]["schemas"].get(schema).is_some(),
            "{schema}"
        );
    }
    assert_eq!(
        doc["components"]["schemas"]["ManifestVm"]["properties"]["spec"]["$ref"],
        "#/components/schemas/VmSpec"
    );
}

#[tokio::test]
async fn secrets_are_compared_without_being_shown_or_exported() {
    let (env, admin) = start().await;
    let with_password = |password: Value| {
        let mut d = vm("db01", "/", 256);
        d["metadata"].as_object_mut().unwrap().remove("folder");
        d["spec"]["disks"] = json!([]);
        d["spec"]["cloud_init"] = json!({ "user": "rocky", "password": password });
        json!({ "documents": [d] })
    };
    let plan = dry_run(&admin, with_password(json!("hunter2-hunter2"))).await;
    let text = plan.to_string();
    assert!(!text.contains("hunter2"), "{plan}");
    assert!(text.contains("(secret)"), "{plan}");
    let task = apply(&admin, with_password(json!("hunter2-hunter2"))).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = env.cluster.get_by_name::<VmSpec>("db01").unwrap().unwrap();
    let hash = vm
        .spec
        .cloud_init
        .as_ref()
        .unwrap()
        .password
        .clone()
        .unwrap();
    assert!(hash.starts_with("$6$"), "{hash}");
    // 같은 평문은 그대로(저장한 해시와 견준다), 다른 평문은 `(changed)`만
    let plan = dry_run(&admin, with_password(json!("hunter2-hunter2"))).await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");
    let plan = dry_run(&admin, with_password(json!("another-secret"))).await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    assert_eq!(
        plan["steps"][0]["diff"],
        json!([{ "path": "cloud_init.password", "op": "replace", "old": "(changed)", "new": "(changed)" }])
    );
    // 내보내기는 참조이고, 참조를 다시 적용하면 비밀은 그대로다.
    let (status, one) = admin.get("/manifests/vm/db01").await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert!(!one.to_string().contains("$6$"), "{one}");
    assert_eq!(
        one["spec"]["cloud_init"]["password"],
        json!({ "secretRef": "Vm/db01/cloud_init.password" })
    );
    let plan = dry_run(&admin, json!({ "documents": [one] })).await;
    assert_eq!(summary(&plan), [0, 0, 0, 1, 0], "{plan}");
    // 아직 없는 VM의 참조는 풀 수 없다.
    let mut other = with_password(json!({ "secretRef": "Vm/x/cloud_init.password" }));
    other["documents"][0]["metadata"]["name"] = json!("db02");
    let plan = dry_run(&admin, other).await;
    assert_eq!(
        plan["steps"][0]["problems"][0]["code"], "secret_unresolved",
        "{plan}"
    );
}

#[tokio::test]
async fn pools_terraform_labels_and_single_deletes() {
    let (env, admin) = start().await;
    let dir = env.dir.path().join("fast");
    std::fs::create_dir_all(&dir).unwrap();
    let pool = |backups: bool| {
        json!({ "documents": [doc("StoragePool", "fast", json!({
            "backend": { "kind": "dir", "path": dir }, "nodes": ["node1"], "backups": backups,
        }))], "manager": "terraform" })
    };
    let task = apply(&admin, pool(false)).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let stored = env
        .cluster
        .get_by_name::<opl_types::storage::StoragePoolSpec>("fast")
        .unwrap()
        .unwrap();
    assert_eq!(stored.spec.nodes, [env.node]);
    assert_eq!(
        stored
            .labels
            .get(opl_types::manifest::MANAGED_BY_LABEL)
            .map(String::as_str),
        Some("terraform")
    );
    let rows = env
        .cluster
        .history_of(Kind::StoragePool, stored.id)
        .unwrap();
    assert_eq!(rows[0].source, ChangeSource::Terraform);
    // 바꿀 수 있는 칸(`backups`)은 update, 바꿀 수 없는 칸(백엔드 경로)은 막힘
    let plan = dry_run(&admin, pool(true)).await;
    assert_eq!(summary(&plan), [0, 1, 0, 0, 0], "{plan}");
    let mut moved = pool(false);
    moved["documents"][0]["spec"]["backend"]["path"] = json!("/elsewhere");
    let plan = dry_run(&admin, moved).await;
    assert_eq!(summary(&plan), [0, 0, 0, 0, 1], "{plan}");
    assert_eq!(plan["steps"][0]["problems"][0]["code"], "immutable");
    // 내보낸 풀은 노드 이름을 쓴다.
    let (status, one) = admin.get("/manifests/storage-pool/fast").await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(one["spec"]["nodes"], json!(["node1"]));
    assert!(one["metadata"].get("labels").is_none(), "{one}");
    // 하나 지우기(Terraform Delete)
    let (status, body) = admin.delete("/manifests/storage-pool/fast").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        env.cluster
            .get_by_name::<opl_types::storage::StoragePoolSpec>("fast")
            .unwrap()
            .is_none()
    );
}
