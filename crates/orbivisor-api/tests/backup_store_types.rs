//! 계획 68: 백업 저장소의 새 칸(백업 작업 `store`·`copy_to`, 경보 조건 `backup_verify_failed`·`backup_store_unavailable`,
//! 할당량 `backup_bytes`)은 옛 노드가 남았으면 409 `cluster_upgrading`(검증보다 먼저)이고, 모든 노드가 `backup-store`를
//! 알면 받는다(작업의 저장소는 있어야 한다). 이 칸을 쓰지 않는 요청과 저장 모양은 지금 그대로다. `backup_bytes` 할당량은
//! 사용량 보기와 VM 옮기기 사전 확인이 센다.

mod common;

use common::{Env, vm_request};
use orbivisor_types::backup::{BackupDisk, BackupSpec, BackupStatus, StoreBackupStats};
use orbivisor_types::folder::{FolderQuota, FolderSpec};
use orbivisor_types::node::{BACKUP_STORE, NodeSpec};
use orbivisor_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 이 노드가 `backup-store`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_feature(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != BACKUP_STORE);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

fn upgrading(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

#[tokio::test]
async fn backup_store_fields_need_the_feature() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let store_job = json!({
        "name": "nightly",
        "spec": { "vms": { "all": true }, "store": "local-st", "schedule": { "at": "03:00" } }
    });
    let copy_job = json!({
        "name": "nightly",
        "spec": { "vms": { "all": true }, "pool": "backups", "schedule": { "at": "03:00" },
                  "copy_to": { "store": "s3" } }
    });
    let rule = json!({
        "name": "verify",
        "spec": { "condition": { "kind": "backup_verify_failed" }, "severity": "warning", "targets": ["ops"] }
    });
    let quota = json!({ "name": "team", "kind": "vm", "quota": { "backup_bytes": 1024 } });

    // 기능이 있으면 받는다: 작업의 저장소는 있어야 하고(400), 복사본은 저장소 작업에만(400).
    let (status, body) = admin.post("/backup-jobs", store_job.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("backup store \"local-st\" not found"),
        "{body}"
    );
    let (status, body) = admin.post("/backup-jobs", copy_job.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 경보 조건은 형식대로 본다(알림 대상이 없음).
    let (status, body) = admin.post("/alert-rules", rule.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("notification target"),
        "{body}"
    );
    let (status, body) = admin.post("/folders", quota.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["spec"]["quota"],
        json!({ "backup_bytes": 1024 }),
        "{body}"
    );
    let folder = body["id"].as_str().unwrap().to_string();
    let (status, body) = admin
        .patch(
            &format!("/folders/{folder}"),
            json!({ "quota": { "vms": 2, "backup_bytes": 10 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 대상이 둘이거나 없는 작업은 400이다(형식).
    let (status, body) = admin
        .post(
            "/backup-jobs",
            json!({ "name": "none", "spec": { "vms": { "all": true }, "schedule": { "at": "03:00" } } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("exactly one of pool and store"),
        "{body}"
    );

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_feature(&env).await;
    let quota2 = json!({ "name": "team2", "kind": "vm", "quota": { "backup_bytes": 1024 } });
    for (path, req) in [
        ("/backup-jobs", store_job),
        ("/backup-jobs", copy_job),
        ("/alert-rules", rule),
        ("/folders", quota2),
        (
            "/backup-stores",
            json!({ "name": "st", "spec": { "backend": { "pool": "bk" } } }),
        ),
    ] {
        let (status, body) = admin.post(path, req).await;
        upgrading(status, &body);
    }
}

/// 저장소에 직접 쓴 `backup_bytes` 할당량: 사용량 보기가 백업 바이트를 세고(VM
/// 몫, 지운 VM의 백업은 백업한 때의 폴더), VM을 할당량 폴더로 옮기기는 넘으면 `quota_exceeded`다.
#[tokio::test]
async fn backup_bytes_are_counted_by_the_usage_view_and_the_precheck() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post("/folders", json!({ "name": "team", "kind": "vm" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let team: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let mut folder = env.cluster.get::<FolderSpec>(team).unwrap().unwrap();
    folder.spec.quota = Some(FolderQuota {
        backup_bytes: Some(100),
        ..FolderQuota::default()
    });
    env.cluster
        .update_spec(folder.id, None, folder.labels.clone(), &folder.spec)
        .await
        .unwrap();

    let mut req = vm_request("a");
    req["spec"]["folder"] = json!(team);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, b) = admin.post("/vms", vm_request("b")).await;
    assert_eq!(status, StatusCode::CREATED, "{b}");

    let backup = |name: &str, vm: &Value, folder: Option<Uuid>, status: BackupStatus| {
        let mut spec: orbivisor_types::vm::VmSpec =
            serde_json::from_value(vm["spec"].clone()).unwrap();
        spec.folder = folder;
        let mut r = Resource::new(
            name,
            BackupSpec {
                vm: vm["id"].as_str().unwrap().parse().unwrap(),
                vm_name: name.into(),
                pool: "backups".into(),
                store: None,
                source: None,
                vm_spec: spec,
                job: None,
                origin: None,
                incremental: false,
                max_chain: None,
            },
        );
        r.status = status;
        r
    };
    let disk = |bytes: u64| BackupStatus {
        disks: vec![BackupDisk {
            name: "disk0".into(),
            file: "disk0.qcow2".into(),
            virtual_bytes: 1 << 30,
            stored_bytes: bytes,
            sha256: None,
        }],
        ..BackupStatus::default()
    };
    let (_, a) = admin.get("/vms/a").await;
    let gone = json!({ "id": Uuid::now_v7(), "spec": a["spec"].clone() });
    for r in [
        backup("a-1", &a, Some(team), disk(30)),
        // 지운 VM의 백업은 백업한 때의 폴더에 든다.
        backup("gone-1", &gone, Some(team), disk(20)),
        // 저장소 백업은 처음 쓴 바이트다.
        backup(
            "b-1",
            &b,
            None,
            BackupStatus {
                store_stats: Some(StoreBackupStats {
                    new_bytes: 60,
                    ..StoreBackupStats::default()
                }),
                ..disk(999)
            },
        ),
    ] {
        let status = r.status.clone();
        env.cluster.create(&r).await.unwrap();
        env.cluster
            .update_status::<BackupSpec>(r.id, &status)
            .await
            .unwrap();
    }

    let (status, body) = admin.get(&format!("/folders/{team}/usage")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["usage"]["backup_bytes"], 50, "{body}");
    assert_eq!(body["limits"][0]["quota"], json!({ "backup_bytes": 100 }));

    // VM b(백업 60)를 옮기면 110 > 100이다.
    let (status, body) = admin
        .post(
            &format!("/folders/{team}/members"),
            json!({ "items": [{ "kind": "vm", "key": "b" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = &body["items"][0];
    assert_eq!(item["ok"], false, "{body}");
    assert_eq!(item["error"]["code"], "quota_exceeded", "{body}");
    assert_eq!(
        item["error"]["message"], "folder /team would use 110 of 100 bytes of backups",
        "{body}"
    );
    // 백업 없는 VM은 넣을 수 있다(넘었어도 늘리지 않는다).
    let mut req = vm_request("c");
    req["spec"]["folder"] = json!(team);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}
