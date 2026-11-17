//! 템플릿 Task(계획 58): 템플릿에서 배포(연결 복제·대체 전체 복사·덮어쓰기·켜기), 평탄화, 템플릿 되돌리기, 기반 지우기
//! 재확인. API(스테이지 5) 없이 Task를 저장소에 바로 만들어 파일 없는 스토리지로 실행한다.

mod common;

use std::sync::Arc;

use common::{Client, Env, FakeStorage, vm_request};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::template::CloneOverrides;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
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
async fn vm_with_disks(admin: &Client, name: &str, volumes: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = volumes
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// VM을 템플릿으로 바꾼다(저장소에 직접 — 바꾸기 API는 스테이지 5).
async fn make_template(env: &Env, id: Uuid, template: bool) {
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let spec = VmSpec {
        template,
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(id, None, vm.labels.clone(), &spec)
        .await
        .unwrap();
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

/// Task를 저장소에 바로 만들고 끝나기를 기다린다.
async fn run(env: &Env, admin: &Client, op: TaskOp, target: Uuid) -> Value {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    admin.wait_task(&json!({ "id": task.id })).await
}

fn deploy(name: &str, linked: Option<bool>, overrides: Option<CloneOverrides>) -> TaskOp {
    TaskOp::VmClone {
        name: name.into(),
        snapshot: None,
        pool: None,
        linked,
        overrides: overrides.map(Box::new),
    }
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

fn failed_with(task: &Value, error: &str) {
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let got = task["status"]["error"].as_str().unwrap();
    assert!(got.contains(error), "{task}");
}

fn get_vm(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn get_volume(env: &Env, name: &str) -> Option<Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

/// 템플릿에서 연결 배포: 복제 볼륨은 기반을 기록하고(같은 풀, 파일 풀이면 qcow2) 저장소에는 연결 복제를 만든 뒤 늘린다.
/// 덮어쓰기(vCPU·메모리·디스크 크기·cloud-init)가 새 VM에 적용되고 새 VM은 템플릿이 아니다.
#[tokio::test]
async fn a_linked_deploy_records_the_base_and_applies_the_overrides() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let root = volume(&env, "root", "local", ImageFormat::Raw, None).await;
    let storage = fake_storage(&mut env, &[root]).await;
    let tmpl = vm_with_disks(&admin, "tmpl", &["root"]).await;
    make_template(&env, tmpl, true).await;

    let ov = CloneOverrides {
        vcpus: Some(4),
        memory_mib: Some(1024),
        disks: vec![Some(4 * MIB)],
        cloud_init: Some(opl_types::cloud_init::CloudInit {
            user: Some("orbi".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let task = run(&env, &admin, deploy("web-1", None, Some(ov)), tmpl).await;
    succeeded(&task);
    assert!(task["status"]["error"].is_null(), "{task}");

    let web = get_vm(&env, "web-1").unwrap();
    assert!(!web.spec.template);
    assert_eq!(web.spec.power, PowerState::Stopped);
    assert_eq!(web.spec.node, None);
    assert_eq!((web.spec.cpu.sockets, web.spec.cpu.cores), (1, 4));
    assert_eq!(web.spec.memory_mib, 1024);
    assert_eq!(
        web.spec.cloud_init.as_ref().unwrap().user.as_deref(),
        Some("orbi")
    );
    let copy = get_volume(&env, "web-1-root").unwrap();
    assert_eq!(web.spec.disks[0].volume, copy.name);
    assert_eq!(copy.spec.base, Some(root));
    assert_eq!(copy.spec.pool, "local");
    assert_eq!(copy.spec.format, ImageFormat::Qcow2);
    assert_eq!(copy.spec.size_bytes, 4 * MIB);
    assert_eq!(copy.status.phase, VolumePhase::Ready);
    assert_eq!(storage.calls(), [("link", copy.id), ("resize", copy.id)]);
    // 템플릿은 그대로다.
    assert!(get_vm(&env, "tmpl").unwrap().spec.template);

    // 디스크는 줄이지 못한다.
    let shrink = CloneOverrides {
        disks: vec![Some(MIB / 2)],
        ..Default::default()
    };
    let task = run(&env, &admin, deploy("web-2", None, Some(shrink)), tmpl).await;
    failed_with(&task, "disks can only grow");
    assert!(get_volume(&env, "web-2-root").is_none());
}

/// `linked` 없음: 연결 복제를 못 하는 풀(공유 LVM)의 디스크는 전체 복사하고 경고를 남긴다. `linked: true`면 실패하고 만든
/// 볼륨을 지운다. `linked: false`면 모두 전체 복사다(계획 11 그대로).
#[tokio::test]
async fn disks_that_cannot_link_fall_back_to_a_full_copy() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    san_pool(&env).await;
    let root = volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    let data = volume(&env, "data", "san", ImageFormat::Raw, None).await;
    let storage = fake_storage(&mut env, &[root, data]).await;
    let tmpl = vm_with_disks(&admin, "tmpl", &["root", "data"]).await;
    make_template(&env, tmpl, true).await;

    let task = run(&env, &admin, deploy("web-1", None, None), tmpl).await;
    succeeded(&task);
    assert_eq!(
        task["status"]["error"],
        "disk1: pool \"san\" (lvm-shared) cannot hold linked clones; made a full copy"
    );
    let (r, d) = (
        get_volume(&env, "web-1-root").unwrap(),
        get_volume(&env, "web-1-data").unwrap(),
    );
    assert_eq!(r.spec.base, Some(root));
    assert_eq!(d.spec.base, None);
    assert_eq!(storage.calls(), [("link", r.id), ("copy", d.id)]);

    let task = run(&env, &admin, deploy("web-2", Some(true), None), tmpl).await;
    failed_with(
        &task,
        "disk1: pool \"san\" (lvm-shared) cannot hold linked clones",
    );
    assert!(get_vm(&env, "web-2").is_none());
    assert!(get_volume(&env, "web-2-root").is_none());

    let task = run(&env, &admin, deploy("web-3", Some(false), None), tmpl).await;
    succeeded(&task);
    assert!(task["status"]["error"].is_null(), "{task}");
    for name in ["web-3-root", "web-3-data"] {
        assert_eq!(get_volume(&env, name).unwrap().spec.base, None, "{name}");
    }
    let calls: Vec<&str> = storage.calls().into_iter().map(|(c, _)| c).collect();
    assert_eq!(calls, ["link", "copy", "copy", "copy"]);
}

/// 연결 복제는 템플릿에서만 된다(되돌린 VM이면 Task가 다시 보고 실패한다). 템플릿이 아닌 VM에도 덮어쓰기는 된다.
#[tokio::test]
async fn linked_clones_need_a_template() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let root = volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    let storage = fake_storage(&mut env, &[root]).await;
    let web = vm_with_disks(&admin, "web", &["root"]).await;

    let task = run(&env, &admin, deploy("web-1", Some(true), None), web).await;
    failed_with(&task, "linked clones can be made only from templates");
    assert!(storage.calls().is_empty());

    let ov = CloneOverrides {
        memory_mib: Some(512),
        ..Default::default()
    };
    succeeded(&run(&env, &admin, deploy("web-2", None, Some(ov)), web).await);
    let copy = get_volume(&env, "web-2-root").unwrap();
    assert_eq!(copy.spec.base, None);
    assert_eq!(storage.calls(), [("copy", copy.id)]);
    assert_eq!(get_vm(&env, "web-2").unwrap().spec.memory_mib, 512);
}

/// `start`면 새 VM을 놓을 노드를 골라 켠 채로 만든다(이 노드의 reconciler가 켠다). 놓을 노드가 없으면 꺼진 채 만들고
/// 경고를 남긴다.
#[tokio::test]
async fn a_deploy_with_start_places_and_starts_the_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let root = volume(&env, "root", "local", ImageFormat::Qcow2, None).await;
    fake_storage(&mut env, &[root]).await;
    let tmpl = vm_with_disks(&admin, "tmpl", &["root"]).await;
    make_template(&env, tmpl, true).await;

    let start = CloneOverrides {
        start: true,
        ..Default::default()
    };
    succeeded(
        &run(
            &env,
            &admin,
            deploy("web-1", None, Some(start.clone())),
            tmpl,
        )
        .await,
    );
    let web = get_vm(&env, "web-1").unwrap();
    assert_eq!(web.spec.power, PowerState::Running);
    assert_eq!(web.spec.node, Some(env.node));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while get_vm(&env, "web-1").unwrap().status.phase != VmPhase::Running {
        assert!(std::time::Instant::now() < deadline, "web-1 did not start");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // 유지 보수 중인 노드뿐이면 켜지 않는다.
    let mut node = env
        .cluster
        .get::<opl_types::node::NodeSpec>(env.node)
        .unwrap()
        .unwrap();
    node.spec.maintenance = true;
    env.cluster
        .update_spec(env.node, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    let pinned = CloneOverrides {
        node: Some(env.node),
        ..start
    };
    let task = run(&env, &admin, deploy("web-2", None, Some(pinned)), tmpl).await;
    succeeded(&task);
    assert_eq!(
        task["status"]["error"],
        format!(
            "node \"{}\" is in maintenance; start \"web-2\" later",
            node.name
        )
    );
    let web = get_vm(&env, "web-2").unwrap();
    assert_eq!(web.spec.power, PowerState::Stopped);
    assert_eq!(web.spec.node, Some(env.node));
}

/// 평탄화: 저장소에서 떼고 기록의 `base`를 지운다. 이미 독립이면 할 일이 없다. 쓰는 VM이 켜져 있으면 실패한다.
#[tokio::test]
async fn flatten_detaches_the_clone_and_drops_its_base() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let base = volume(&env, "base", "local", ImageFormat::Raw, None).await;
    let clone = volume(&env, "clone", "local", ImageFormat::Qcow2, Some(base)).await;
    let storage = fake_storage(&mut env, &[base, clone]).await;
    vm_with_disks(&admin, "web", &["clone"]).await;

    succeeded(&run(&env, &admin, TaskOp::VolumeFlatten, clone).await);
    let v = get_volume(&env, "clone").unwrap();
    assert_eq!(v.spec.base, None);
    assert_eq!(v.status.phase, VolumePhase::Ready);
    assert_eq!(storage.calls(), [("flatten", clone)]);
    // 다시 해도 된다(저장소는 부르지 않는다).
    succeeded(&run(&env, &admin, TaskOp::VolumeFlatten, clone).await);
    assert_eq!(storage.calls().len(), 1);

    let other = volume(&env, "other", "local", ImageFormat::Qcow2, Some(base)).await;
    storage.set_size(other, MIB);
    vm_with_disks(&admin, "db", &["other"]).await;
    let started = admin.act("db", "start").await;
    succeeded(&started);
    let task = run(&env, &admin, TaskOp::VolumeFlatten, other).await;
    failed_with(
        &task,
        "VM \"db\" is running; stop it before flattening its disk",
    );
    assert_eq!(get_volume(&env, "other").unwrap().spec.base, Some(base));
}

/// 되돌리기: 연결 복제가 남았으면 실패하고(쓰는 VM 이름), 없으면 디스크마다 기반을 푼 뒤 템플릿 표시를 지운다.
#[tokio::test]
async fn untemplate_releases_the_bases_once_no_clone_is_left() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let root = volume(&env, "root", "local", ImageFormat::Raw, None).await;
    let clone = volume(&env, "web-1-root", "local", ImageFormat::Qcow2, Some(root)).await;
    let loose = volume(&env, "loose", "local", ImageFormat::Qcow2, Some(root)).await;
    let storage = fake_storage(&mut env, &[root, clone, loose]).await;
    let tmpl = vm_with_disks(&admin, "tmpl", &["root"]).await;
    make_template(&env, tmpl, true).await;
    vm_with_disks(&admin, "web-1", &["web-1-root"]).await;

    let task = run(&env, &admin, TaskOp::VmUntemplate, tmpl).await;
    failed_with(
        &task,
        "template \"tmpl\" has 2 linked clone(s) (loose, web-1); delete or flatten them first",
    );
    assert!(get_vm(&env, "tmpl").unwrap().spec.template);

    succeeded(&run(&env, &admin, TaskOp::VolumeFlatten, clone).await);
    succeeded(&run(&env, &admin, TaskOp::VolumeFlatten, loose).await);
    succeeded(&run(&env, &admin, TaskOp::VmUntemplate, tmpl).await);
    assert!(!get_vm(&env, "tmpl").unwrap().spec.template);
    assert_eq!(storage.calls().last(), Some(&("release_base", root)));
    // 이미 보통 VM이면 할 일이 없다.
    succeeded(&run(&env, &admin, TaskOp::VmUntemplate, tmpl).await);
    assert_eq!(storage.calls().len(), 3);
}

/// 지우기 Task는 지우기 직전에 연결 복제가 남았는지 다시 본다(API가 확인한 뒤 다른 노드가 복제를 만들었을 수 있다).
#[tokio::test]
async fn deleting_a_base_fails_while_linked_clones_exist() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "local").await;
    let base = volume(&env, "base", "local", ImageFormat::Raw, None).await;
    let clone = volume(&env, "clone", "local", ImageFormat::Qcow2, Some(base)).await;
    let storage = fake_storage(&mut env, &[base, clone]).await;

    let task = run(&env, &admin, TaskOp::VolumeDelete, base).await;
    failed_with(
        &task,
        "volume \"base\" is the base of 1 linked clone(s) (clone); delete or flatten them first",
    );
    assert!(get_volume(&env, "base").is_some());
    assert!(storage.deleted().is_empty());

    succeeded(&run(&env, &admin, TaskOp::VolumeDelete, clone).await);
    succeeded(&run(&env, &admin, TaskOp::VolumeDelete, base).await);
    assert!(get_volume(&env, "base").is_none());
}
