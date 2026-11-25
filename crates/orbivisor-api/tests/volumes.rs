//! 볼륨 생성·가져오기·삭제. qemu-img가 필요하다.
//! `cargo test -p orbivisor-api --features storage-it --test volumes`
#![cfg(feature = "storage-it")]

mod common;

use std::path::Path;
use std::process::Command;

use common::{Client, Env};
use reqwest::StatusCode;
use serde_json::json;

const MIB: u64 = 1 << 20;

async fn create_pool(env: &Env, admin: &Client, dir: &Path) {
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": "local",
                "spec": { "backend": { "kind": "dir", "path": dir } }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
}

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[tokio::test]
async fn creates_and_deletes_volume_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;

    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "root",
                "spec": { "pool": "local", "size_bytes": 10 * MIB, "format": "qcow2" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/root").await;
    assert_eq!(volume["status"]["phase"], "ready");
    assert!(volume["status"]["allocated_bytes"].as_u64().unwrap() > 0);
    let file = pool.join(format!("{}.qcow2", volume["id"].as_str().unwrap()));
    assert!(file.exists());

    let (status, body) = admin.delete("/storage-pools/local").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "pool_in_use");

    // VM이 쓰는 볼륨은 지울 수 없다.
    let mut vm = common::vm_request("web");
    vm["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    assert_eq!(admin.post("/vms", vm).await.0, StatusCode::CREATED);
    let (status, body) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "volume_in_use");
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);

    let (status, task) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(!file.exists());
    assert_eq!(admin.get("/volumes/root").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        admin.delete("/storage-pools/local").await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn failed_create_removes_the_volume() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 풀 디렉터리가 없으면 qemu-img가 실패한다.
    create_pool(&env, &admin, &env.dir.path().join("missing")).await;
    let (_, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "root",
                "spec": { "pool": "local", "size_bytes": MIB, "format": "raw" }
            }),
        )
        .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(admin.get("/volumes/root").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn imports_uploaded_vmdk() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    let raw = env.dir.path().join("disk.raw");
    std::fs::write(&raw, vec![7u8; 3 * MIB as usize]).unwrap();
    let vmdk = env.dir.path().join("disk.vmdk");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw.to_str().unwrap(),
        vmdk.to_str().unwrap(),
    ]);

    let (status, task) = admin
        .upload(
            "/volumes/import?name=imported&pool=local&source_format=vmdk",
            std::fs::read(&vmdk).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/imported").await;
    assert_eq!(volume["spec"]["size_bytes"], 3 * MIB);
    assert_eq!(volume["spec"]["format"], "qcow2");
    // 올린 파일은 가져온 뒤 지운다.
    let uploads: Vec<_> = std::fs::read_dir(env.dir.path().join("uploads"))
        .unwrap()
        .collect();
    assert!(uploads.is_empty());
}

#[tokio::test]
async fn upload_needs_content_length() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    // 스트림 본문은 길이를 모르므로 chunked로 간다.
    let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(vec![0u8; 512])]);
    let r = admin
        .http
        .post(format!(
            "{}/api/v1/volumes/import?name=x&pool=local&source_format=raw",
            admin.base
        ))
        .bearer_auth(admin.token.as_ref().unwrap())
        .body(reqwest::Body::wrap_stream(chunks));
    let (status, body) = admin.send(r).await;
    assert_eq!(status, StatusCode::LENGTH_REQUIRED, "{body}");
    assert_eq!(admin.get("/volumes/x").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_images_that_refer_to_other_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    let descriptor = "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
                      createType=\"monolithicFlat\"\n\nRW 2048 FLAT \"/etc/hosts\" 0\n";

    let (status, task) = admin
        .upload(
            "/volumes/import?name=evil&pool=local&source_format=vmdk",
            descriptor.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("descriptor"), "{error}");
    assert_eq!(admin.get("/volumes/evil").await.0, StatusCode::NOT_FOUND);
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
}

/// 시험 ISO(ISO 9660 + Joliet, 파일 하나)
fn iso_image() -> Vec<u8> {
    orbivisor_qemu::iso9660::build("TEST", &[("readme.txt", b"install me")]).unwrap()
}

/// ISO를 `local` 풀에 올리고 Task가 성공할 때까지 기다린다.
async fn upload_iso(admin: &Client, name: &str) -> serde_json::Value {
    let (status, task) = admin
        .upload(
            &format!("/volumes/import?name={name}&pool=local&source_format=raw&content=iso"),
            iso_image(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    admin.get(&format!("/volumes/{name}")).await.1
}

#[tokio::test]
async fn uploads_iso_images_as_raw_iso_volumes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;

    let volume = upload_iso(&admin, "installer").await;
    assert_eq!(volume["spec"]["content"], "iso", "{volume}");
    assert_eq!(volume["spec"]["format"], "raw");
    assert_eq!(volume["status"]["phase"], "ready");
    assert_eq!(volume["spec"]["size_bytes"], iso_image().len() as u64);
    // 파일 풀 안 이름은 디스크처럼 `{id}.raw`이고 내용은 올린 그대로다.
    let file = pool.join(format!("{}.raw", volume["id"].as_str().unwrap()));
    assert_eq!(std::fs::read(&file).unwrap(), iso_image());

    // 디스크 볼륨은 `content`를 적지 않는다(옛 노드가 읽는 모양).
    let (_, task) = admin
        .post(
            "/volumes",
            json!({ "name": "root", "spec": { "pool": "local", "size_bytes": MIB, "format": "qcow2" } }),
        )
        .await;
    admin.wait_task(&task).await;
    let (_, disk) = admin.get("/volumes/root").await;
    assert!(disk["spec"].get("content").is_none(), "{disk}");

    // 늘리지 못한다.
    let (status, body) = admin
        .post(
            "/volumes/installer/resize",
            json!({ "size_bytes": 64 * MIB }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "ISO volumes cannot be resized");
}

#[tokio::test]
async fn rejects_iso_uploads_that_are_not_install_media() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;

    let (status, task) = admin
        .upload(
            "/volumes/import?name=fake&pool=local&source_format=raw&content=iso",
            vec![0u8; 64 * 1024],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(task["status"]["error"], "not an ISO 9660 or UDF image");
    assert_eq!(admin.get("/volumes/fake").await.0, StatusCode::NOT_FOUND);
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
    let uploads = std::fs::read_dir(env.dir.path().join("uploads")).unwrap();
    assert_eq!(uploads.count(), 0);
}

#[tokio::test]
async fn iso_volumes_follow_the_iso_rules() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    let thin = orbivisor_types::resource::Resource::new(
        "thin",
        orbivisor_types::storage::StoragePoolSpec {
            backend: orbivisor_types::storage::PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&thin).await.unwrap();
    env.pool_ready("thin").await;

    let bad = |query: &str| format!("/volumes/import?name=x&{query}&content=iso");
    for (query, message) in [
        (
            "pool=thin&source_format=raw",
            "ISO images can be kept only in dir or nfs pools",
        ),
        (
            "pool=local&source_format=raw&format=qcow2",
            "ISO volumes are raw",
        ),
        ("pool=local&source_format=qcow2", "upload ISO images as raw"),
    ] {
        let (status, body) = admin.upload(&bad(query), iso_image()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert_eq!(body["message"], message, "{query}");
        // 사전 확인도 같다.
        let check = format!(
            "{}&size=100",
            bad(query).replace("/import?", "/import/check?")
        );
        let (status, body) = admin.post(&check, serde_json::Value::Null).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert_eq!(body["message"], message, "{query}");
    }
    assert_eq!(admin.get("/volumes/x").await.0, StatusCode::NOT_FOUND);

    // 비어 있는 ISO 볼륨은 만들지 않는다.
    let (status, body) = admin
        .post(
            "/volumes",
            json!({
                "name": "x",
                "spec": { "pool": "local", "size_bytes": MIB, "format": "raw", "content": "iso" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "create ISO volumes by uploading or downloading an image"
    );
    // `download`는 `/volumes/download` 경로와 겹친다.
    let (status, body) = admin
        .post(
            "/volumes",
            json!({ "name": "download", "spec": { "pool": "local", "size_bytes": MIB, "format": "qcow2" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// 꺼진 VM `name`을 `cdroms`(ISO 이름 또는 빈 드라이브)로 만든다.
async fn vm_with_cdroms(admin: &Client, name: &str, cdroms: &[Option<&str>]) {
    let mut vm = common::vm_request(name);
    vm["spec"]["cdroms"] = cdroms.iter().map(|iso| json!({ "iso": iso })).collect();
    let (status, body) = admin.post("/vms", vm).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn isos_in_cdrom_drives_or_snapshots_cannot_be_deleted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    upload_iso(&admin, "installer").await;

    // 한 ISO를 두 VM이 함께 쓴다.
    vm_with_cdroms(&admin, "a", &[None, Some("installer")]).await;
    vm_with_cdroms(&admin, "b", &[Some("installer")]).await;
    let (status, body) = admin.delete("/volumes/installer").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_use");
    assert_eq!(
        body["message"],
        "volume \"installer\" is in the CD-ROM drive of VM \"a\" and 1 more"
    );
    assert_eq!(admin.delete("/vms/b").await.0, StatusCode::NO_CONTENT);
    let (_, body) = admin.delete("/volumes/installer").await;
    assert_eq!(
        body["message"],
        "volume \"installer\" is in the CD-ROM drive of VM \"a\""
    );

    // 스펙에서 뺐어도 도는 QEMU의 기록에 남은 매체(빼기를 마치지 못함)는 쓰는 중이다.
    let mut vm = env
        .cluster
        .get_by_name::<orbivisor_types::vm::VmSpec>("a")
        .unwrap()
        .unwrap();
    let mut hardware = orbivisor_types::vm::RunningHardware::legacy(&vm.spec);
    vm.spec.cdroms[1].iso = None;
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    hardware.cdroms[1].iso = Some("installer".into());
    vm.status.hardware = Some(hardware);
    env.cluster
        .update_status::<orbivisor_types::vm::VmSpec>(vm.id, &vm.status)
        .await
        .unwrap();
    let (status, body) = admin.delete("/volumes/installer").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_use");
    vm.status.hardware = None;
    env.cluster
        .update_status::<orbivisor_types::vm::VmSpec>(vm.id, &vm.status)
        .await
        .unwrap();

    // 스냅샷이 그 매체로 되돌린다.
    let mut snapshot_spec = vm.spec.clone();
    snapshot_spec.cdroms[1].iso = Some("installer".into());
    let mut snapshot: orbivisor_types::snapshot::Snapshot =
        orbivisor_types::resource::Resource::new(
            "",
            orbivisor_types::snapshot::SnapshotSpec {
                vm: vm.id,
                name: "before".into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled: false,
                vm_spec: snapshot_spec,
                hardware: None,
            },
        );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    let (status, body) = admin.delete("/volumes/installer").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_snapshot");
    env.cluster
        .delete::<orbivisor_types::snapshot::SnapshotSpec>(snapshot.id, None)
        .await
        .unwrap();

    // API가 확인한 뒤 Task가 돌기 전에 VM이 그 ISO를 넣었으면 지우기 Task가 다시 보고 실패한다(ISO는 그대로).
    vm.spec.cdroms[1].iso = Some("installer".into());
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    let volume = env
        .cluster
        .get_by_name::<orbivisor_types::storage::VolumeSpec>("installer")
        .unwrap()
        .unwrap();
    let mut late: orbivisor_types::task::Task = orbivisor_types::resource::Resource::new(
        "",
        orbivisor_types::task::TaskSpec {
            op: orbivisor_types::task::TaskOp::VolumeDelete,
            target: volume.id,
            node: env.node,
        },
    );
    late.name = late.id.to_string();
    env.cluster.create(&late).await.unwrap();
    let task = admin.wait_task(&json!({ "id": late.id })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "volume \"installer\" is in the CD-ROM drive of VM \"a\""
    );
    assert_eq!(admin.get("/volumes/installer").await.0, StatusCode::OK);
    vm.spec.cdroms[1].iso = None;
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    // 확인한 뒤 그 매체가 든 스냅샷을 찍었어도 지우기 Task가 다시 보고 실패한다.
    let mut snapshot = snapshot.clone();
    snapshot.id = uuid::Uuid::now_v7();
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    let mut late = late.clone();
    late.id = uuid::Uuid::now_v7();
    late.name = late.id.to_string();
    env.cluster.create(&late).await.unwrap();
    let task = admin.wait_task(&json!({ "id": late.id })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        format!(
            "volume \"installer\" is used by snapshot \"before\" of VM {}",
            vm.id
        )
    );
    assert_eq!(admin.get("/volumes/installer").await.0, StatusCode::OK);
    env.cluster
        .delete::<orbivisor_types::snapshot::SnapshotSpec>(snapshot.id, None)
        .await
        .unwrap();

    let (status, task) = admin.delete("/volumes/installer").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}
