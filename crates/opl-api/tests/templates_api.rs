//! 템플릿·배포·평탄화·이미지 카탈로그 API(계획 58 스테이지 5): 템플릿 바꾸기·되돌리기와 불변 규칙, 409 네 가지
//! (`vm_is_template`·`volume_is_template`·`template_in_use`·`volume_has_clones`), `POST /vms/{key}/clone`의 배포 칸과
//! 미리 검사, 평탄화, 복제 켜기 거절, 기능 게이트, Task의 비밀번호 가림, 이미지 카탈로그.

mod common;

use std::sync::Arc;

use common::{Client, Env, FakeStorage, set_pool_state, vm_request};
use opl_types::node::{NodeSpec, PoolState, TEMPLATES};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::SnapshotSpec;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

async fn pool(env: &Env, name: &str, backend: PoolBackend, shared: bool) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend,
            nodes: vec![],
            shared,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

async fn dir_pool(env: &Env, name: &str) {
    let backend = PoolBackend::Dir {
        path: env.dir.path().join(name),
    };
    pool(env, name, backend, false).await;
}

async fn san_pool(env: &Env) {
    let backend = PoolBackend::LvmShared {
        vg: "san".into(),
        iscsi: None,
    };
    pool(env, "san", backend, true).await;
}

/// 바로 Ready인 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(
    env: &Env,
    name: &str,
    pool: &str,
    format: ImageFormat,
    base: Option<Uuid>,
) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: MIB,
            format,
            content: Default::default(),
            base,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    volume.id
}

/// 볼륨을 붙인 꺼진 VM을 만든다.
async fn vm_with_disks(admin: &Client, name: &str, volumes: &[&str]) -> Value {
    let mut req = vm_request(name);
    req["spec"]["disks"] = volumes
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm
}

/// 파일 없는 스토리지로 실행기를 다시 띄운다. `volumes`는 1 MiB로 알린다.
async fn fake_storage(env: &mut Env, volumes: &[Uuid]) -> Arc<FakeStorage> {
    let storage = Arc::new(FakeStorage::default());
    for id in volumes {
        storage.set_size(*id, MIB);
    }
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    storage
}

/// 이 노드가 `templates`를 모르는 옛 노드인 것처럼 기능 목록에서 뺀다.
async fn drop_templates_feature(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != TEMPLATES);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

fn get_volume(env: &Env, name: &str) -> Option<Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

fn stored_task(env: &Env, task: &Value) -> Task {
    let id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    env.cluster.get::<TaskSpec>(id).unwrap().unwrap()
}

/// 바꾸기: 꺼진 VM이고 스냅샷·HA·연결 복제 디스크가 없어야 한다. 이미 템플릿이면 그대로다. 되돌리기는 연결 복제가
/// 남았으면 409 `template_in_use`, 없으면 `vm-untemplate` Task다.
#[tokio::test]
async fn convert_and_unconvert_a_template() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let root = volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "tmpl", &["root"]).await;

    // 켜져 있으면 안 된다.
    succeeded(&admin.act("tmpl", "start").await);
    let got = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_running");
    succeeded(&admin.act("tmpl", "stop").await);

    // 스냅샷이 있으면 안 된다.
    let vm = env.cluster.get_by_name::<VmSpec>("tmpl").unwrap().unwrap();
    let mut snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: vm.id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: vm.spec.clone(),
            hardware: None,
        },
    );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    let got = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_has_snapshots");
    env.cluster
        .delete::<SnapshotSpec>(snapshot.id, None)
        .await
        .unwrap();

    // HA가 있으면 400.
    let mut spec = vm.spec.clone();
    spec.ha = Some(Default::default());
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &spec)
        .await
        .unwrap();
    let got = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "a template cannot have ha; remove it first"
    );
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();

    let (status, body) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["template"], true);
    // 다시 해도 된다.
    let (status, body) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["template"], true);

    // 연결 복제가 남았으면 되돌리지 못한다.
    let clone = volume(&env, "web-1-root", "local", ImageFormat::Qcow2, Some(root)).await;
    let got = admin.delete("/vms/tmpl/template").await;
    assert_error(&got, StatusCode::CONFLICT, "template_in_use");
    assert_eq!(
        got.1["message"],
        "template \"tmpl\" has 1 linked clone(s) (web-1-root); delete or flatten them first"
    );
    // 연결 복제 디스크로는 템플릿을 만들 수 없다.
    vm_with_disks(&admin, "web-1", &["web-1-root"]).await;
    let got = admin.post("/vms/web-1/template", Value::Null).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "volume \"web-1-root\" is a linked clone; flatten it before making a template"
    );
    let (status, _) = admin.delete("/vms/web-1").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    env.cluster.delete::<VolumeSpec>(clone, None).await.unwrap();

    let (status, task) = admin.delete("/vms/tmpl/template").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-untemplate");
    succeeded(&admin.wait_task(&task).await);
    let (_, vm) = admin.get("/vms/tmpl").await;
    assert!(vm["spec"].get("template").is_none(), "{vm}");
    assert_eq!(storage.calls().last(), Some(&("release_base", root)));
    // 템플릿이 아니면 400.
    let got = admin.delete("/vms/tmpl/template").await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 바꾸기는 복제·백업 같은 Task가 도는 VM을 409 `vm_busy`로 막는다(설계 §3). 템플릿의 스냅샷 되돌리기는(스냅샷이
/// 생길 수 없지만) 409 `vm_is_template`이다.
#[tokio::test]
async fn busy_vms_are_not_converted_and_templates_are_not_reverted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    vm_with_disks(&admin, "tmpl", &["root"]).await;
    let vm = env.cluster.get_by_name::<VmSpec>("tmpl").unwrap().unwrap();

    // 실행기가 없는 노드의 Task라 끝나지 않는다.
    let mut other = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    other.id = Uuid::now_v7();
    other.name = "node2".into();
    other.spec.address = "127.0.0.2:0".into();
    env.cluster.create(&other).await.unwrap();
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmClone {
                name: "copy".into(),
                snapshot: None,
                pool: None,
                linked: None,
                overrides: None,
            },
            target: vm.id,
            node: other.id,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    let got = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_busy");
    env.cluster.delete::<TaskSpec>(task.id, None).await.unwrap();

    let (status, body) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let vm = env.cluster.get_by_name::<VmSpec>("tmpl").unwrap().unwrap();
    let mut snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: vm.id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: vm.spec.clone(),
            hardware: None,
        },
    );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    let got = admin
        .post("/vms/tmpl/snapshots/s1/revert", Value::Null)
        .await;
    assert_error(&got, StatusCode::CONFLICT, "vm_is_template");
}

/// 템플릿인 동안: 켜기·재시작·마이그레이션·복제·스냅샷·CD 넣기·지우기는 409 `vm_is_template`, 디스크 목록 바꾸기와 켠
/// 스펙도 409, HA는 400이다. 나머지(메모리)는 바꿀 수 있고 PUT은 템플릿 표시를 바꾸지 않는다. 템플릿 디스크의 크기
/// 바꾸기·지우기는 409 `volume_is_template`이다.
#[tokio::test]
async fn templates_are_immutable() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    volume(&env, "other", "local", ImageFormat::Qcow2, None).await;
    vm_with_disks(&admin, "tmpl", &["root"]).await;
    let (status, tmpl) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{tmpl}");

    for action in ["start", "reboot"] {
        let got = admin
            .post(&format!("/vms/tmpl/actions/{action}"), Value::Null)
            .await;
        assert_error(&got, StatusCode::CONFLICT, "vm_is_template");
        assert_eq!(
            got.1["message"],
            "VM \"tmpl\" is a template; deploy a VM from it or convert it back"
        );
    }
    for (path, body) in [
        ("/vms/tmpl/migrate", json!({ "node": "node1" })),
        ("/vms/tmpl/replicate", json!({})),
        ("/vms/tmpl/snapshots", json!({ "name": "s1" })),
        ("/vms/tmpl/cdrom", json!({ "iso": "x" })),
    ] {
        let got = admin.post(path, body).await;
        assert_error(&got, StatusCode::CONFLICT, "vm_is_template");
    }
    let got = admin.delete("/vms/tmpl").await;
    assert_error(&got, StatusCode::CONFLICT, "vm_is_template");
    assert_eq!(
        got.1["message"],
        "convert template \"tmpl\" back to a VM before deleting it"
    );

    let spec = tmpl["spec"].clone();
    let mut running = spec.clone();
    running["power"] = json!("running");
    let got = admin
        .put("/vms/tmpl", json!({ "spec": running }), None)
        .await;
    assert_error(&got, StatusCode::CONFLICT, "vm_is_template");
    let mut disks = spec.clone();
    disks["disks"] = json!([{ "volume": "other", "bus": "virtio-blk" }]);
    let got = admin.put("/vms/tmpl", json!({ "spec": disks }), None).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_is_template");
    let mut ha = spec.clone();
    ha["ha"] = json!(true);
    let got = admin.put("/vms/tmpl", json!({ "spec": ha }), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    // 메모리는 바꿀 수 있고, 템플릿 표시를 뺀 PUT(옛 클라이언트)도 템플릿을 그대로 둔다.
    let mut memory = spec.clone();
    memory["memory_mib"] = json!(512);
    memory.as_object_mut().unwrap().remove("template");
    let (status, body) = admin
        .put("/vms/tmpl", json!({ "spec": memory }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["memory_mib"], 512);
    assert_eq!(body["spec"]["template"], true);
    // 보통 VM의 PUT으로 템플릿을 만들지 않는다.
    let (_, web) = admin.post("/vms", vm_request("web")).await;
    let mut spec = web["spec"].clone();
    spec["template"] = json!(true);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("template").is_none(), "{body}");

    let got = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 4 * MIB }))
        .await;
    assert_error(&got, StatusCode::CONFLICT, "volume_is_template");
    assert_eq!(
        got.1["message"],
        "volume \"root\" is a disk of template \"tmpl\""
    );
    let got = admin.delete("/volumes/root").await;
    assert_error(&got, StatusCode::CONFLICT, "volume_is_template");
}

/// 연결 복제의 기반은 지우지 못한다(409 `volume_has_clones`). 복제를 지우면 지울 수 있다.
#[tokio::test]
async fn a_base_with_linked_clones_cannot_be_deleted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let base = volume(&env, "base", "local", ImageFormat::Raw, None).await;
    volume(&env, "b-clone", "local", ImageFormat::Qcow2, Some(base)).await;
    volume(&env, "a-clone", "local", ImageFormat::Qcow2, Some(base)).await;
    let got = admin.delete("/volumes/base").await;
    assert_error(&got, StatusCode::CONFLICT, "volume_has_clones");
    assert_eq!(
        got.1["message"],
        "volume \"base\" is the base of 2 linked clone(s) (a-clone, b-clone); delete or flatten them first"
    );
}

/// `POST /volumes`는 `base`를 받지 않는다(400): 연결 복제는 배포 Task만 만든다. 빈 볼륨의 거짓 기록은 기반 지우기와
/// 되돌리기를 막고 옛 노드는 그 칸을 풀지 못한다.
#[tokio::test]
async fn volumes_cannot_be_created_with_a_base() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let base = volume(&env, "base", "local", ImageFormat::Raw, None).await;
    let got = admin
        .post(
            "/volumes",
            json!({
                "name": "fake-clone",
                "spec": { "pool": "local", "size_bytes": MIB, "format": "qcow2", "base": base }
            }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "base is set only by deploying from a template"
    );
    assert!(get_volume(&env, "fake-clone").is_none());
}

/// 배포: 새 칸으로 덮어쓰고 연결 복제를 만든다. 응답·조회의 Task는 cloud-init 비밀번호를 가리고 저장된 Task는 해시를
/// 담는다. 새 칸 없는 템플릿 복제는 계획 57과 같은 Task(새 칸 없음)이고 Task가 연결한다.
#[tokio::test]
async fn deploy_from_a_template_with_overrides() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let root = volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "tmpl", &["root"]).await;
    let (status, _) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK);

    let req = json!({
        "name": "web-1",
        "linked": true,
        "node": "node1",
        "vcpus": 4,
        "memory_mib": 512,
        "disks": [4 * MIB],
        "cloud_init": { "user": "orbi", "password": "secret", "vendor_data": "#cloud-config\n" },
    });
    let (status, task) = admin.post("/vms/tmpl/clone", req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let op = &task["spec"]["op"];
    assert_eq!(op["linked"], true, "{task}");
    assert_eq!(op["overrides"]["cloud_init"]["password"], "********");
    assert_eq!(op["overrides"]["node"], json!(env.node));
    let path = format!("/tasks/{}", task["id"].as_str().unwrap());
    let (_, got) = admin.get(&path).await;
    let ci = &got["spec"]["op"]["overrides"]["cloud_init"];
    assert_eq!(ci["password"], "********");
    assert_eq!(ci["vendor_data"], "#cloud-config\n");
    // VM 쓰기 권한이 없으면 vendor-data·user-data도 가린다(VM 스펙과 같음).
    let viewer = env.user("viewer", "viewer").await;
    let (_, got) = viewer.get(&path).await;
    assert_eq!(
        got["spec"]["op"]["overrides"]["cloud_init"]["vendor_data"], "********",
        "{got}"
    );
    let TaskOp::VmClone { overrides, .. } = stored_task(&env, &task).spec.op else {
        panic!("not a clone");
    };
    let password = overrides.unwrap().cloud_init.unwrap().password.unwrap();
    assert!(password.starts_with("$6$"), "{password}");
    succeeded(&admin.wait_task(&task).await);
    let (_, web) = admin.get("/vms/web-1").await;
    assert_eq!(web["spec"]["cpu"]["cores"], 4);
    assert_eq!(web["spec"]["memory_mib"], 512);
    assert_eq!(web["spec"]["node"], json!(env.node));
    assert_eq!(web["spec"]["cloud_init"]["password"], "********");
    let copy = get_volume(&env, "web-1-root").unwrap();
    assert_eq!(copy.spec.base, Some(root));
    assert_eq!(copy.spec.size_bytes, 4 * MIB);

    // 새 칸이 없으면 Task에도 없다. 템플릿이면 Task가 연결한다.
    let (status, task) = admin
        .post("/vms/tmpl/clone", json!({ "name": "web-2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let op = task["spec"]["op"].as_object().unwrap();
    assert!(
        !op.contains_key("linked") && !op.contains_key("overrides"),
        "{task}"
    );
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(
        get_volume(&env, "web-2-root").unwrap().spec.base,
        Some(root)
    );
    let links = storage.calls().iter().filter(|(c, _)| *c == "link").count();
    assert_eq!(links, 2);

    // 기반과 템플릿의 디스크는 그대로이고 복제가 있는 동안 되돌리지 못한다.
    let got = admin.delete("/vms/tmpl/template").await;
    assert_error(&got, StatusCode::CONFLICT, "template_in_use");
    assert_eq!(
        got.1["message"],
        "template \"tmpl\" has 2 linked clone(s) (web-1, web-2); delete or flatten them first"
    );
}

/// 배포 요청의 미리 검사: 연결은 템플릿에서만·되는 디스크만, 디스크는 늘리기만(512 배수), 없는 네트워크·노드는 400이다.
#[tokio::test]
async fn deploy_requests_are_checked_up_front() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    san_pool(&env).await;
    volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    volume(&env, "data", "san", ImageFormat::Raw, None).await;
    volume(&env, "plain", "local", ImageFormat::Qcow2, None).await;
    vm_with_disks(&admin, "tmpl", &["root", "data"]).await;
    vm_with_disks(&admin, "web", &["plain"]).await;
    let (status, _) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK);

    let bad = |body: Value, message: &'static str| (body, message);
    for (path, (body, message)) in [
        (
            "/vms/web/clone",
            bad(
                json!({ "name": "x", "linked": true }),
                "linked clones can be made only from templates",
            ),
        ),
        (
            "/vms/tmpl/clone",
            bad(
                json!({ "name": "x", "linked": true }),
                "disk1: pool \"san\" (lvm-shared) cannot hold linked clones",
            ),
        ),
        (
            "/vms/tmpl/clone",
            bad(
                json!({ "name": "x", "disks": [MIB / 2] }),
                "disk0: 524288 bytes is smaller than the current 1048576 bytes; disks can only grow",
            ),
        ),
        (
            "/vms/tmpl/clone",
            bad(
                json!({ "name": "x", "disks": [MIB + 1] }),
                "disks: 1048577 bytes is not a multiple of 512",
            ),
        ),
        (
            "/vms/tmpl/clone",
            bad(
                json!({ "name": "x", "nics": ["nowhere"] }),
                "network \"nowhere\" not found",
            ),
        ),
        (
            "/vms/tmpl/clone",
            bad(
                json!({ "name": "x", "node": "nowhere" }),
                "node \"nowhere\" not found",
            ),
        ),
        (
            "/vms/tmpl/clone",
            bad(
                json!({ "name": "x", "nics": [null, "user"] }),
                "nics: 2 given but the VM has 1 NIC(s)",
            ),
        ),
    ] {
        let got = admin.post(path, body).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(got.1["message"], message);
    }
    // 다른 노드의 로컬 풀에는 닿지 않는다.
    let mut other = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    other.id = Uuid::now_v7();
    other.name = "node2".into();
    other.spec.address = "127.0.0.2:0".into();
    other.status.pools.clear();
    env.cluster.create(&other).await.unwrap();
    set_pool_state(
        &env.cluster,
        other.id,
        "san",
        PoolState {
            ready: true,
            ..Default::default()
        },
    )
    .await;
    let got = admin
        .post("/vms/tmpl/clone", json!({ "name": "x", "node": "node2" }))
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "node \"node2\" cannot reach pool \"local\" of template \"tmpl\"; deploy on node \"node1\" and migrate the VM"
    );
}

/// 새 칸·새 Task는 모든 노드가 `templates`를 알아야 한다(409 `cluster_upgrading`, 검증보다 먼저). 새 칸 없는 복제는
/// 그대로 된다.
#[tokio::test]
async fn old_nodes_refuse_template_requests() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    vm_with_disks(&admin, "web", &["root"]).await;
    drop_templates_feature(&env).await;

    for (path, body) in [
        ("/vms/web/template", Value::Null),
        ("/vms/web/clone", json!({ "name": "", "vcpus": 2 })),
        ("/volumes/root/flatten", Value::Null),
        (
            "/image-catalog/nowhere/download",
            json!({ "pool": "local" }),
        ),
        (
            "/volumes/download",
            json!({ "name": "img", "pool": "local", "url": "https://a.example/x.img", "sha512": "x" }),
        ),
    ] {
        let got = admin.post(path, body).await;
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    }
    let got = admin.delete("/vms/web/template").await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    let (status, task) = admin
        .post("/vms/web/clone", json!({ "name": "web-2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 평탄화 API: 연결 복제가 아니면 400, 쓰는 VM이 켜져 있으면 409 `vm_running`, 파일 풀의 디스크를 가진 VM에 스냅샷이
/// 있으면 409 `vm_has_snapshots`, 풀 여유가 모자라면 409 `no_capacity`. 끝나면 기반을 지울 수 있다.
#[tokio::test]
async fn flatten_through_the_api() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let base = volume(&env, "base", "local", ImageFormat::Raw, None).await;
    let clone = volume(&env, "clone", "local", ImageFormat::Qcow2, Some(base)).await;
    let storage = fake_storage(&mut env, &[base, clone]).await;
    vm_with_disks(&admin, "web", &["clone"]).await;

    let got = admin.post("/volumes/base/flatten", Value::Null).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(got.1["message"], "volume \"base\" is not a linked clone");

    succeeded(&admin.act("web", "start").await);
    let got = admin.post("/volumes/clone/flatten", Value::Null).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_running");
    succeeded(&admin.act("web", "stop").await);

    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    let mut snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: vm.id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: vm.spec.clone(),
            hardware: None,
        },
    );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    let got = admin.post("/volumes/clone/flatten", Value::Null).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_has_snapshots");
    env.cluster
        .delete::<SnapshotSpec>(snapshot.id, None)
        .await
        .unwrap();

    // 기반이 쓰는 공간보다 풀 여유가 적다.
    let mut b = env.cluster.get::<VolumeSpec>(base).unwrap().unwrap();
    b.status.allocated_bytes = Some(10 * MIB);
    env.cluster
        .update_status::<VolumeSpec>(base, &b.status)
        .await
        .unwrap();
    let small = PoolState {
        ready: true,
        total_bytes: 100 * MIB,
        available_bytes: MIB,
        ..Default::default()
    };
    set_pool_state(&env.cluster, env.node, "local", small.clone()).await;
    let got = admin.post("/volumes/clone/flatten", Value::Null).await;
    assert_error(&got, StatusCode::CONFLICT, "no_capacity");
    let roomy = PoolState {
        available_bytes: 50 * MIB,
        ..small
    };
    set_pool_state(&env.cluster, env.node, "local", roomy).await;

    let (status, task) = admin.post("/volumes/clone/flatten", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "volume-flatten");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(get_volume(&env, "clone").unwrap().spec.base, None);
    assert!(storage.calls().contains(&("flatten", clone)));
    let (status, task) = admin.delete("/volumes/base").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 연결 복제 디스크가 있는 VM에는 복제 대상(계획 55)을 둘 수 없다(400).
#[tokio::test]
async fn replication_refuses_linked_clone_disks() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let base = volume(&env, "base", "local", ImageFormat::Raw, None).await;
    volume(&env, "clone", "local", ImageFormat::Qcow2, Some(base)).await;
    let mut other = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    other.id = Uuid::now_v7();
    other.name = "node2".into();
    other.spec.address = "127.0.0.2:0".into();
    env.cluster.create(&other).await.unwrap();

    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "clone", "bus": "virtio-blk" }]);
    req["spec"]["replication"] = json!([{ "node": other.id, "schedule": "*/15" }]);
    let got = admin.post("/vms", req).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "volume \"clone\" is a linked clone of \"base\"; flatten it before enabling replication"
    );
}

/// 받기의 새 확인 칸: 하나만, 길이 맞는 16진, http·https 체크섬 파일 URL.
#[tokio::test]
async fn volume_download_checks_one_checksum() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let req = |extra: Value| {
        let mut r = json!({ "name": "img", "pool": "local", "url": "http://127.0.0.1:1/x.img" });
        r.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        r
    };
    for (extra, message) in [
        (
            json!({ "sha256": "ab".repeat(32), "checksum_url": "https://a.example/SUMS" }),
            "give only one of sha256, sha512 and checksum_url",
        ),
        (
            json!({ "sha512": "ab".repeat(32) }),
            "sha512 must be 128 hex digits",
        ),
        (
            json!({ "checksum_url": "ftp://a.example/SUMS" }),
            "checksum_url must be an http or https URL",
        ),
    ] {
        let got = admin.post("/volumes/download", req(extra)).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(got.1["message"], message);
    }
    let (status, task) = admin
        .post(
            "/volumes/download",
            req(json!({ "checksum_url": "https://u:p@a.example/SHA256SUMS" })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"]["checksum_url"],
        "https://***@a.example/SHA256SUMS"
    );
    let TaskOp::VolumeDownload { checksum_url, .. } = stored_task(&env, &task).spec.op else {
        panic!("not a download");
    };
    assert_eq!(
        checksum_url.as_deref(),
        Some("https://u:p@a.example/SHA256SUMS")
    );
}

/// 관리자 카탈로그 항목(aarch64, 고정 SHA-256)
fn custom_image(id: &str, arch: &str) -> Value {
    json!({
        "id": id,
        "title": "Corp image",
        "os": "ubuntu",
        "version": "24.04",
        "arch": arch,
        "url": format!("http://127.0.0.1:1/{id}.qcow2"),
        "checksum": { "kind": "sha256", "value": "AB".repeat(32) },
        "source_format": "qcow2",
        "firmware": "uefi",
        "default_user": "ubuntu",
        "min_size_bytes": 2 * MIB,
    })
}

/// 카탈로그 목록(내장 + 관리자, 아키텍처 거름, 이미 받은 볼륨)과 관리자 항목 더하기·지우기
#[tokio::test]
async fn the_image_catalog_lists_adds_and_removes_images() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, list) = admin.get("/image-catalog").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let items = list["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["id"] == "ubuntu-24.04"), "{list}");
    assert!(items.iter().all(|i| i["source"] == "builtin"), "{list}");
    let (_, arm) = admin.get("/image-catalog?arch=aarch64").await;
    let arm = arm["items"].as_array().unwrap();
    assert!(!arm.is_empty() && arm.iter().all(|i| i["arch"] == "aarch64"));

    let (status, image) = admin
        .post("/image-catalog", custom_image("corp", "aarch64"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{image}");
    assert_eq!(image["checksum"]["value"], "ab".repeat(32));
    let got = admin
        .post("/image-catalog", custom_image("corp", "aarch64"))
        .await;
    assert_error(&got, StatusCode::CONFLICT, "already_exists");
    let got = admin
        .post("/image-catalog", custom_image("ubuntu-24.04", "aarch64"))
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    let mut bios = custom_image("arm-bios", "aarch64");
    bios["firmware"] = json!("bios");
    let got = admin.post("/image-catalog", bios).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    let (status, _) = admin
        .post("/image-catalog", custom_image("corp", "x86_64"))
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // 그 URL에서 받은 볼륨이 보인다.
    dir_pool(&env, "local").await;
    let id = volume(&env, "corp-arm64", "local", ImageFormat::Qcow2, None).await;
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeDownload {
                url: "http://127.0.0.1:1/corp.qcow2".into(),
                sha256: None,
                source_format: opl_types::storage::SourceFormat::Qcow2,
                sha512: None,
                checksum_url: None,
                template: None,
            },
            target: id,
            node: Uuid::now_v7(),
        },
    );
    task.name = task.id.to_string();
    task.status.state = TaskState::Succeeded;
    env.cluster.create(&task).await.unwrap();
    let (_, list) = admin.get("/image-catalog").await;
    let corp: Vec<&Value> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["id"] == "corp")
        .collect();
    assert_eq!(corp.len(), 2, "{list}");
    assert!(corp.iter().all(|i| i["source"] == "custom"));
    assert_eq!(corp[0]["volumes"], json!(["corp-arm64"]));

    let got = admin.delete("/image-catalog/ubuntu-24.04").await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    let got = admin.delete("/image-catalog/nowhere").await;
    assert_error(&got, StatusCode::NOT_FOUND, "not_found");
    let (status, _) = admin.delete("/image-catalog/corp?arch=x86_64").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, settings) = admin.get("/settings").await;
    let custom = settings["spec"]["image_catalog"].as_array().unwrap();
    assert_eq!(custom.len(), 1);
    assert_eq!(custom[0]["arch"], "aarch64");
    let (status, _) = admin.delete("/image-catalog/corp").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, settings) = admin.get("/settings").await;
    assert!(
        settings["spec"].get("image_catalog").is_none(),
        "{settings}"
    );

    // 설정 쓰기 권한이 있어야 한다.
    let viewer = env.user("viewer", "viewer").await;
    let got = viewer
        .post("/image-catalog", custom_image("corp", "aarch64"))
        .await;
    assert_eq!(got.0, StatusCode::FORBIDDEN, "{}", got.1);
    let (status, _) = viewer.get("/image-catalog").await;
    assert_eq!(status, StatusCode::OK);
}

/// 카탈로그 이미지 받기 → 템플릿: 항목에서 URL·체크섬·형식을, 머신·펌웨어를 아키텍처에서 채우고, 비밀번호는 해시해
/// 담고 응답에서 가린다. 미리 검사(이름, 네트워크, 크기, 아키텍처)는 400·409다.
#[tokio::test]
async fn download_a_catalog_image_as_a_template() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    for arch in ["aarch64", "x86_64"] {
        let (status, body) = admin
            .post("/image-catalog", custom_image("corp", arch))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let template = json!({
        "name": "corp-tmpl",
        "network": "user",
        "size_bytes": 4 * MIB,
        "cloud_init": { "user": "orbi", "password": "secret" },
    });

    let got = admin
        .post(
            "/image-catalog/nowhere/download",
            json!({ "pool": "local" }),
        )
        .await;
    assert_error(&got, StatusCode::NOT_FOUND, "not_found");
    let mut small = template.clone();
    small["size_bytes"] = json!(MIB);
    let got = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "template": small }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    // 템플릿에는 호스트 이름·고정 IP가 없다(배포할 때 준다).
    let mut named = template.clone();
    named["cloud_init"]["hostname"] = json!("web");
    let got = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "template": named }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "template.cloud_init.hostname: a template has no host name; give it when deploying"
    );
    let mut fixed = template.clone();
    fixed["cloud_init"]["networks"] =
        json!([{ "ipv4": { "mode": "static", "address": "10.0.0.5/24" } }]);
    let got = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "template": fixed }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("no static IP address"),
        "{}",
        got.1
    );
    let mut lan = template.clone();
    lan["network"] = json!("lan");
    let got = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "template": lan }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    // 이 노드는 aarch64다.
    let got = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "arch": "x86_64", "template": template }),
        )
        .await;
    assert_error(&got, StatusCode::BAD_REQUEST, "machine_arch_mismatch");
    admin.post("/vms", vm_request("taken")).await;
    let mut taken = template.clone();
    taken["name"] = json!("taken");
    let got = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "template": taken }),
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "name_taken");
    // 같은 이름의 볼륨이 있으면 409.
    volume(&env, "busy", "local", ImageFormat::Qcow2, None).await;
    let got = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "name": "busy" }),
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "name_taken");

    let (status, task) = admin
        .post(
            "/image-catalog/corp/download",
            json!({ "pool": "local", "template": template }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let op = &task["spec"]["op"];
    assert_eq!(op["url"], "http://127.0.0.1:1/corp.qcow2");
    assert_eq!(op["sha256"], "ab".repeat(32));
    assert_eq!(op["source_format"], "qcow2");
    assert_eq!(op["template"]["machine"], "virt");
    assert_eq!(op["template"]["firmware"], "uefi");
    assert_eq!(op["template"]["cloud_init"]["password"], "********");
    let TaskOp::VolumeDownload { template, .. } = stored_task(&env, &task).spec.op else {
        panic!("not a download");
    };
    let password = template.unwrap().cloud_init.unwrap().password.unwrap();
    assert!(password.starts_with("$6$"), "{password}");
    let (_, got) = admin
        .get(&format!("/tasks/{}", task["id"].as_str().unwrap()))
        .await;
    assert_eq!(
        got["spec"]["op"]["template"]["cloud_init"]["password"],
        "********"
    );
    let volume: Uuid = task["spec"]["target"].as_str().unwrap().parse().unwrap();
    assert_eq!(volume, stored_task(&env, &task).spec.target);
    let vol = env.cluster.get::<VolumeSpec>(volume);
    // 받기는 곧 실패해 볼륨을 지울 수 있다(127.0.0.1:1). 남아 있으면 이름과 형식을 본다.
    if let Some(vol) = vol.unwrap() {
        assert_eq!(vol.name, "corp-arm64");
        assert_eq!(vol.spec.format, ImageFormat::Qcow2);
    }
}

/// 관리자 카탈로그 항목의 URL: 사용자 정보는 받지 않고(400), 쿼리 문자열(서명·토큰)은 관리자에게만 보인다 — 목록·설정
/// 조회에서 관리자가 아니면 가린다.
#[tokio::test]
async fn catalog_urls_carry_no_user_info_and_are_redacted_for_non_admins() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut image = custom_image("corp", "x86_64");
    image["url"] = json!("https://u:p@mirror.lan/corp.qcow2");
    let got = admin.post("/image-catalog", image.clone()).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "image corp: urls must not contain a user name or password"
    );
    image["url"] = json!("https://mirror.lan/corp.qcow2?token=s3cret");
    image["checksum"] = json!({ "kind": "sums", "url": "https://token@mirror.lan/SHA256SUMS" });
    let got = admin.post("/image-catalog", image.clone()).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    image["checksum"] = json!({ "kind": "sums", "url": "https://mirror.lan/SHA256SUMS?sig=abc" });
    let (status, body) = admin.post("/image-catalog", image).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let corp = |list: &Value| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == "corp")
            .cloned()
            .unwrap()
    };
    let (_, list) = admin.get("/image-catalog").await;
    let entry = corp(&list);
    assert_eq!(entry["url"], "https://mirror.lan/corp.qcow2?token=s3cret");
    assert_eq!(
        entry["checksum"]["url"],
        "https://mirror.lan/SHA256SUMS?sig=abc"
    );
    let (_, settings) = admin.get("/settings").await;
    assert_eq!(
        settings["spec"]["image_catalog"][0]["url"],
        "https://mirror.lan/corp.qcow2?token=s3cret"
    );

    let viewer = env.user("viewer", "viewer").await;
    let (_, list) = viewer.get("/image-catalog").await;
    let entry = corp(&list);
    assert_eq!(entry["url"], "https://mirror.lan/corp.qcow2?***");
    assert_eq!(
        entry["checksum"]["url"],
        "https://mirror.lan/SHA256SUMS?***"
    );
    let (status, settings) = viewer.get("/settings").await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    let stored = &settings["spec"]["image_catalog"][0];
    assert_eq!(stored["url"], "https://mirror.lan/corp.qcow2?***");
    assert_eq!(
        stored["checksum"]["url"],
        "https://mirror.lan/SHA256SUMS?***"
    );
}

/// 지난 노드(`spec.node`·`status.node`)가 클러스터에서 빠졌거나 오프라인인 템플릿: 되돌리기·배포 Task는 그 노드가 아닌
/// 디스크가 닿는 노드에서 돈다(공유 풀은 쓸 수 있는 온라인 노드, 로컬 풀은 볼륨이 있는 노드).
#[tokio::test]
async fn templates_whose_last_node_is_gone_or_offline_still_run_tasks() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    san_pool(&env).await;
    let root = volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    let shared = volume(&env, "shared-root", "san", ImageFormat::Raw, None).await;
    fake_storage(&mut env, &[root, shared]).await;
    // 오프라인 노드(실행기가 없다)
    let mut offline = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    offline.id = Uuid::now_v7();
    offline.name = "node2".into();
    offline.spec.address = "127.0.0.2:0".into();
    offline.status.online = false;
    env.cluster.create(&offline).await.unwrap();

    for (name, disk, last) in [
        ("tmpl", "root", Uuid::now_v7()),
        ("shared", "shared-root", offline.id),
    ] {
        vm_with_disks(&admin, name, &[disk]).await;
        let (status, body) = admin
            .post(&format!("/vms/{name}/template"), Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let vm = env.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap();
        let mut spec = vm.spec.clone();
        spec.node = Some(last);
        env.cluster
            .update_spec(vm.id, None, vm.labels.clone(), &spec)
            .await
            .unwrap();
        let mut status = vm.status.clone();
        status.node = Some(last);
        env.cluster
            .update_status::<VmSpec>(vm.id, &status)
            .await
            .unwrap();

        let (status, task) = admin
            .post(
                &format!("/vms/{name}/clone"),
                json!({ "name": format!("{name}-1") }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        assert_eq!(stored_task(&env, &task).spec.node, env.node, "{name}");
        succeeded(&admin.wait_task(&task).await);
    }
    // 공유 LVM은 연결 복제가 없으므로(전체 복사) 되돌려 지울 수 있다.
    let (status, task) = admin.delete("/vms/shared/template").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(stored_task(&env, &task).spec.node, env.node);
    succeeded(&admin.wait_task(&task).await);
    let (status, body) = admin.delete("/vms/shared").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// 끝나지 않은 되돌리기 Task가 있는 VM: 둘째 되돌리기·켜기·고치기·지우기는 409 `vm_busy`다(되돌리기가 표시를 지운 뒤
/// 다시 보는 사이).
#[tokio::test]
async fn an_unfinished_untemplate_blocks_start_update_and_delete() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    vm_with_disks(&admin, "tmpl", &["root"]).await;
    let (status, _) = admin.post("/vms/tmpl/template", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let vm = env.cluster.get_by_name::<VmSpec>("tmpl").unwrap().unwrap();
    // 실행기가 없는 노드의 Task라 끝나지 않는다.
    let mut other = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    other.id = Uuid::now_v7();
    other.name = "node2".into();
    other.spec.address = "127.0.0.2:0".into();
    env.cluster.create(&other).await.unwrap();
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmUntemplate,
            target: vm.id,
            node: other.id,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();

    let got = admin.delete("/vms/tmpl/template").await;
    assert_error(&got, StatusCode::CONFLICT, "vm_busy");
    // 그 Task가 표시를 지운 뒤
    let mut spec = vm.spec.clone();
    spec.template = false;
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &spec)
        .await
        .unwrap();
    let got = admin.post("/vms/tmpl/actions/start", Value::Null).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_busy");
    let (_, current) = admin.get("/vms/tmpl").await;
    let mut put = json!({ "spec": current["spec"].clone() });
    put["spec"]["memory_mib"] = json!(1024);
    let got = admin.put("/vms/tmpl", put, None).await;
    assert_error(&got, StatusCode::CONFLICT, "vm_busy");
    let got = admin.delete("/vms/tmpl").await;
    assert_error(&got, StatusCode::CONFLICT, "vm_busy");

    env.cluster.delete::<TaskSpec>(task.id, None).await.unwrap();
    let (status, body) = admin.delete("/vms/tmpl").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}
