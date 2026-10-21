//! 볼륨 생성·가져오기·삭제. qemu-img가 필요하다.
//! `cargo test -p opl-api --features storage-it --test volumes`
#![cfg(feature = "storage-it")]

mod common;

use std::path::Path;
use std::process::Command;

use common::{Client, Env};
use reqwest::StatusCode;
use serde_json::json;

const MIB: u64 = 1 << 20;

async fn create_pool(admin: &Client, dir: &Path) {
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
    create_pool(&admin, &pool).await;

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
    create_pool(&admin, &env.dir.path().join("missing")).await;
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
    create_pool(&admin, &pool).await;
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
    create_pool(&admin, &pool).await;
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
    create_pool(&admin, &pool).await;
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
