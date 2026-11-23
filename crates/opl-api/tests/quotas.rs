//! 폴더 할당량의 API(계획 62 설계 §9): 만들기·바꾸기·옮기기·폴더 옮기기·볼륨 늘리기·복제·복원의 사전 확인(409
//! `quota_exceeded`, 폴더 경로가 든 메시지와 `details`), 늘리지 않는 쓰기는 받기, 사용량 API.

mod common;

use std::sync::Arc;

use common::{Client, Env, FakeStorage, vm_request};
use opl_types::backup::{BackupDisk, BackupSpec, BackupStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};

const GIB: u64 = 1 << 30;

async fn folder(admin: &Client, name: &str, parent: Option<&str>, quota: Value) -> String {
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": name, "kind": "vm", "parent": parent, "quota": quota }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

fn vm_in(name: &str, folder: Option<&str>) -> Value {
    let mut req = vm_request(name);
    req["spec"]["folder"] = json!(folder);
    req
}

fn assert_exceeded(status: StatusCode, body: &Value, message: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "quota_exceeded", "{body}");
    assert_eq!(body["message"], message, "{body}");
}

/// 풀 `local`의 Ready 볼륨을 저장소에 직접 만든다.
async fn volume(env: &Env, name: &str, size: u64) {
    let mut v = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: size,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    env.cluster.create(&v).await.unwrap();
    v.status.phase = VolumePhase::Ready;
    v.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(v.id, &v.status)
        .await
        .unwrap();
}

#[tokio::test]
async fn creating_past_the_vm_count_is_refused_with_the_folder_path() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let team = folder(&admin, "team", None, json!({"vms": 2})).await;
    let prod = folder(&admin, "prod", Some(&team), json!(null)).await;
    for name in ["a", "b"] {
        let (status, body) = admin.post("/vms", vm_in(name, Some(&prod))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = admin.post("/vms", vm_in("c", Some(&prod))).await;
    assert_exceeded(status, &body, "folder /team would use 3 of 2 VMs");
    assert_eq!(
        body["details"],
        json!({"folder": team, "dimension": "vms", "limit": 2, "used": 3})
    );
    // 루트에는 상한이 없다.
    let (status, body) = admin.post("/vms", vm_in("c", None)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 루트의 VM을 넣는 옮기기도 늘리기다(항목의 오류).
    let (status, body) = admin
        .post(
            &format!("/folders/{prod}/members"),
            json!({"items": [{"kind": "vm", "key": "c"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["ok"], false, "{body}");
    assert_eq!(
        body["items"][0]["error"]["code"], "quota_exceeded",
        "{body}"
    );

    // 사용량 보기: 하위 트리 합, 묶는 폴더
    let (status, body) = admin.get(&format!("/folders/{prod}/usage")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["path"], "/team/prod");
    assert_eq!(
        body["usage"],
        json!({"vcpus": 2, "memory_mib": 512, "disk_bytes": 0, "vms": 2})
    );
    assert_eq!(body["limits"][0]["folder"], team.as_str());
    assert_eq!(body["limits"][0]["path"], "/team");
    assert_eq!(body["limits"][0]["quota"], json!({"vms": 2}));
    assert_eq!(body["binding"], json!({"vms": team}));
}

/// 상한을 사용량 아래로 줄일 수 있고(초과), 그 뒤에도 늘리지 않는 바꾸기는 받는다.
#[tokio::test]
async fn only_growing_updates_are_refused() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let team = folder(&admin, "team", None, json!({"memory_mib": 1024})).await;
    for name in ["a", "b"] {
        let (status, body) = admin.post("/vms", vm_in(name, Some(&team))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = admin
        .patch(
            &format!("/folders/{team}"),
            json!({"quota": {"memory_mib": 256}}),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, vm) = admin.get("/vms/a").await;
    let mut spec: VmSpec = serde_json::from_value(vm["spec"].clone()).unwrap();
    spec.memory_mib = 128;
    let (status, body) = admin.put("/vms/a", json!({"spec": spec}), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    spec.memory_mib = 512;
    let (status, body) = admin.put("/vms/a", json!({"spec": spec}), None).await;
    assert_exceeded(
        status,
        &body,
        "folder /team would use 768 of 256 MiB of memory",
    );
    let (status, body) = admin.get(&format!("/folders/{team}/usage")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["usage"]["memory_mib"], 384);
}

#[tokio::test]
async fn moving_a_folder_under_a_quota_brings_its_usage() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let team = folder(&admin, "team", None, json!({"vcpus": 1})).await;
    let other = folder(&admin, "other", None, json!(null)).await;
    let (status, body) = admin.post("/vms", vm_in("a", Some(&other))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin.post("/vms", vm_in("b", Some(&team))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .patch(&format!("/folders/{other}"), json!({"parent": team}), None)
        .await;
    assert_exceeded(status, &body, "folder /team would use 2 of 1 vCPUs");
    // 이름 바꾸기는 막지 않는다.
    let (status, body) = admin
        .patch(
            &format!("/folders/{other}"),
            json!({"name": "renamed"}),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn growing_an_attached_disk_is_refused() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let team = folder(&admin, "team", None, json!({"disk_bytes": 3 * GIB})).await;
    volume(&env, "root", 2 * GIB).await;
    volume(&env, "loose", GIB).await;
    let mut req = vm_in("a", Some(&team));
    req["spec"]["disks"] = json!([{"volume": "root", "bus": "virtio-blk"}]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post("/volumes/root/resize", json!({"size_bytes": 4 * GIB}))
        .await;
    assert_exceeded(
        status,
        &body,
        &format!(
            "folder /team would use {} of {} bytes of disk",
            4 * GIB,
            3 * GIB
        ),
    );
    let (status, body) = admin
        .post("/volumes/root/resize", json!({"size_bytes": 3 * GIB}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // 붙지 않은 볼륨은 세지 않는다.
    let (status, body) = admin
        .post("/volumes/loose/resize", json!({"size_bytes": 8 * GIB}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

/// 복제·복원은 Task를 만들기 전에 막는다(Task가 디스크를 만든 뒤 실패하지 않게).
#[tokio::test]
async fn clone_and_restore_are_checked_before_the_task() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let team = folder(&admin, "team", None, json!({"vms": 1})).await;
    let (status, body) = admin.post("/vms", vm_in("a", Some(&team))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post("/vms/a/clone", json!({"name": "a2", "folder": team}))
        .await;
    assert_exceeded(status, &body, "folder /team would use 2 of 1 VMs");
    // 폴더를 주지 않으면 원본의 폴더다.
    let (status, body) = admin.post("/vms/a/clone", json!({"name": "a2"})).await;
    assert_exceeded(status, &body, "folder /team would use 2 of 1 VMs");

    let (_, vm) = admin.get("/vms/a").await;
    let mut backup = Resource::new(
        "a-backup",
        BackupSpec {
            vm: vm["id"].as_str().unwrap().parse().unwrap(),
            vm_name: "a".into(),
            pool: "backups".into(),
            vm_spec: serde_json::from_value(vm["spec"].clone()).unwrap(),
            job: None,
            origin: None,
            incremental: false,
            max_chain: None,
            source: None,
            store: None,
        },
    );
    env.cluster.create(&backup).await.unwrap();
    backup.status = BackupStatus {
        disks: vec![BackupDisk {
            name: "disk0".into(),
            file: "disk0.qcow2".into(),
            virtual_bytes: GIB,
            stored_bytes: 1,
            sha256: None,
        }],
        ..BackupStatus::default()
    };
    env.cluster
        .update_status::<BackupSpec>(backup.id, &backup.status)
        .await
        .unwrap();
    // 백업한 VM의 폴더로 되살리면 넘는다.
    let (status, body) = admin
        .post(
            "/backups/a-backup/restore",
            json!({"name": "a3", "pool": "local"}),
        )
        .await;
    assert_exceeded(status, &body, "folder /team would use 2 of 1 VMs");
}

/// 리뷰 1 I6: 늘리기 Task는 저장소를 건드리기 전에 요청한 크기를 Raft에 써서 몫을 잡고(그동안 다른 늘리기는 409),
/// 올림(LVM extent 등)으로 상한을 넘은 실제 크기도 기록한다(사용량을 적게 세지 않는다).
#[tokio::test]
async fn resizing_reserves_the_quota_before_the_disk_grows() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    let team = folder(&admin, "team", None, json!({"disk_bytes": 4 * GIB})).await;
    volume(&env, "root", 2 * GIB).await;
    volume(&env, "other", GIB).await;
    for (name, disk) in [("a", "root"), ("b", "other")] {
        let mut req = vm_in(name, Some(&team));
        req["spec"]["disks"] = json!([{"volume": disk, "bus": "virtio-blk"}]);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let cluster = env.cluster.clone();
    let id = move |name: &str| cluster.get_by_name::<VolumeSpec>(name).unwrap().unwrap();
    let storage = Arc::new(FakeStorage::default());
    storage.set_size(id("root").id, 2 * GIB);
    storage.set_size(id("other").id, GIB);
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;

    storage.pause();
    let (status, task) = admin
        .post("/volumes/root/resize", json!({"size_bytes": 3 * GIB}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    // 저장소가 늘리는 중인데 몫은 이미 잡혔다.
    assert_eq!(id("root").spec.size_bytes, 3 * GIB);
    let (status, body) = admin
        .post("/volumes/other/resize", json!({"size_bytes": 2 * GIB}))
        .await;
    assert_exceeded(
        status,
        &body,
        &format!(
            "folder /team would use {} of {} bytes of disk",
            5 * GIB,
            4 * GIB
        ),
    );
    // 저장소가 1.25 GiB 단위로 올린다: 3.75 GiB, 폴더는 4.75 GiB가 되어 상한을 넘지만 기록한다.
    let unit = 5 * GIB / 4;
    storage.round_resizes(unit);
    storage.resume();
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let root = id("root");
    assert_eq!(root.spec.size_bytes, 3 * unit);
    // 허가는 기록한 뒤 지운다.
    assert_eq!(root.status.disk_bytes, None);
    let (_, usage) = admin.get(&format!("/folders/{team}/usage")).await;
    assert_eq!(usage["usage"]["disk_bytes"], 3 * unit + GIB, "{usage}");
}

/// 리뷰 2 N8: 앞 늘리기가 지우지 못한 허가(`status.disk_bytes`)는 다음 늘리기가 몫을 잡기 전에 지운다. 남겨 두면 그
/// 크기까지의 기록이 Raft 할당량을 건너뛴다.
#[tokio::test]
async fn a_leftover_resize_permit_is_cleared_first() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    let team = folder(&admin, "team", None, json!({"disk_bytes": 4 * GIB})).await;
    volume(&env, "root", 2 * GIB).await;
    let mut req = vm_in("a", Some(&team));
    req["spec"]["disks"] = json!([{"volume": "root", "bus": "virtio-blk"}]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let cluster = env.cluster.clone();
    let get = move || cluster.get_by_name::<VolumeSpec>("root").unwrap().unwrap();
    // 기록하지 못한 허가가 남았다(8 GiB까지).
    let mut root = get();
    root.status.disk_bytes = Some(8 * GIB);
    env.cluster
        .update_status::<VolumeSpec>(root.id, &root.status)
        .await
        .unwrap();
    let storage = Arc::new(FakeStorage::default());
    storage.set_size(root.id, 2 * GIB);
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;

    storage.pause();
    let (status, task) = admin
        .post("/volumes/root/resize", json!({"size_bytes": 3 * GIB}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    let root = get();
    assert_eq!(root.status.disk_bytes, None);
    assert_eq!(root.spec.size_bytes, 3 * GIB);
    // 허가가 없으니 상한을 넘는 기록은 Raft가 거절한다.
    let mut spec = root.spec.clone();
    spec.size_bytes = 7 * GIB;
    assert!(
        env.cluster
            .update_spec(root.id, None, root.labels.clone(), &spec)
            .await
            .is_err()
    );
    storage.resume();
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(get().status.disk_bytes, None);
}
