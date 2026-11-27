//! 복원 맵(계획 76 §3.3) Task: 백업의 네트워크·보안 그룹을 맵대로 바꾼 새 VM, cloud-init 주소·DNS를 새로 붙이기·덮어쓰기,
//! SDN 네트워크로 간 NIC의 고정 주소를 IPAM에 잡기와 겹치면 실패. 디스크 없는 VM의 백업(리소스·매니페스트)을 직접 만들고
//! 파일 없는 스토리지(FakeStorage)로 복원 Task를 돌린다. 요청 검사(400·409)는 `tests/backups.rs`.

mod common;

use std::sync::Arc;

use common::{Client, Env, FakeStorage, vm_request};
use orbivisor_types::backup::{
    Backup, BackupManifest, BackupPhase, BackupSpec, BackupStatus, MANIFEST_FILE, MANIFEST_VERSION,
    backup_dir,
};
use orbivisor_types::network::Network;
use orbivisor_types::resource::Resource;
use orbivisor_types::security_group::SecurityGroup;
use orbivisor_types::storage::{PoolBackend, StoragePoolSpec};
use orbivisor_types::vm::{Vm, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 파일 없는 스토리지로 실행기를 다시 띄운 환경
async fn env() -> Env {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_with(Arc::new(FakeStorage::default()))
        .await;
    env
}

/// 이 노드에서 준비된 dir 풀
async fn pool(env: &Env, name: &str, backups: bool) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 저장소에 바로 만드는 네트워크(`spec`은 NetworkSpec JSON)
async fn network(env: &Env, name: &str, spec: Value) {
    let net: Network = Resource::new(name, serde_json::from_value(spec).unwrap());
    env.cluster.create(&net).await.unwrap();
}

async fn group(env: &Env, name: &str) {
    let g: SecurityGroup = Resource::new(name, serde_json::from_value(json!({})).unwrap());
    env.cluster.create(&g).await.unwrap();
}

/// 풀 `bk`에 디스크 없는 VM(스펙 `spec`의 칸을 더함)의 `ready` 백업 `b1`을 리소스와 매니페스트로 만든다.
async fn backup(env: &Env, spec: Value) -> Backup {
    pool(env, "bk", true).await;
    pool(env, "local", false).await;
    let mut vm = vm_request("web")["spec"].clone();
    vm.as_object_mut()
        .unwrap()
        .extend(spec.as_object().unwrap().clone());
    let vm_spec: VmSpec = serde_json::from_value(vm).unwrap();
    let source = Uuid::now_v7();
    let spec: BackupSpec = serde_json::from_value(
        json!({ "vm": source, "vm_name": "web", "pool": "bk", "vm_spec": vm_spec }),
    )
    .unwrap();
    let mut b: Backup = Resource::new("b1", spec);
    b.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(1),
        node: Some(env.node),
        ..Default::default()
    };
    env.cluster.create(&b).await.unwrap();
    env.cluster
        .update_status::<BackupSpec>(b.id, &b.status)
        .await
        .unwrap();
    let dir = backup_dir(&env.dir.path().join("bk"), b.id);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = BackupManifest {
        version: MANIFEST_VERSION,
        backup: b.id,
        parent: None,
        vm: source,
        vm_name: "web".into(),
        vm_spec,
        disks: vec![],
        uefi_vars: false,
        created_at: 1,
        tpm_state: false,
    };
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    b
}

/// 복원을 요청하고 Task가 끝나기를 기다린다.
async fn restore(admin: &Client, body: Value) -> Value {
    let (status, task) = admin.post("/backups/b1/restore", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn vm(env: &Env, name: &str) -> Vm {
    env.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap()
}

/// 네트워크·보안 그룹 맵: 맵에 있는 이름은 바뀌고 없는 이름은 그대로다. cloud-init이 없던 백업에 주소·DNS를 주면
/// cloud-init을 새로 붙인다.
#[tokio::test]
async fn restore_maps_networks_groups_and_adds_cloud_init() {
    let env = env().await;
    let admin = env.admin().await;
    for n in ["lan", "dr-lan", "db"] {
        network(&env, n, json!({ "bridge": "vmbr0" })).await;
    }
    for g in ["web", "dr-web", "ssh"] {
        group(&env, g).await;
    }
    backup(
        &env,
        json!({ "nics": [
            { "network": "lan", "model": "virtio", "security_groups": ["web", "ssh"] },
            { "network": "db", "model": "virtio" }
        ] }),
    )
    .await;

    let task = restore(
        &admin,
        json!({
            "name": "web-dr", "pool": "local",
            "networks": { "lan": "dr-lan" },
            "security_groups": { "web": "dr-web" },
            "cloud_init_networks": [ { "ipv4": { "mode": "static", "address": "10.20.0.5/24", "gateway": "10.20.0.1" } } ],
            "cloud_init_dns": { "servers": ["10.20.0.1"] }
        }),
    )
    .await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(
        task["spec"]["op"]["remap"]["networks"],
        json!({ "lan": "dr-lan" })
    );
    let new = vm(&env, "web-dr");
    let nets: Vec<&str> = new.spec.nics.iter().map(|n| n.network.as_str()).collect();
    assert_eq!(nets, ["dr-lan", "db"]);
    assert_eq!(new.spec.nics[0].security_groups, ["dr-web", "ssh"]);
    let ci = serde_json::to_value(new.spec.cloud_init.as_ref().unwrap()).unwrap();
    assert_eq!(
        ci,
        json!({
            "networks": [ { "ipv4": { "mode": "static", "address": "10.20.0.5/24", "gateway": "10.20.0.1" } } ],
            "dns": { "servers": ["10.20.0.1"] }
        })
    );
}

/// 백업에 cloud-init이 있으면 주소 설정만 바꾸고 나머지(사용자 등)는 그대로다. DNS를 주지 않으면 백업의 DNS 그대로다.
#[tokio::test]
async fn restore_overwrites_cloud_init_addresses() {
    let env = env().await;
    let admin = env.admin().await;
    network(&env, "lan", json!({ "bridge": "vmbr0" })).await;
    backup(
        &env,
        json!({
            "nics": [ { "network": "lan", "model": "virtio" } ],
            "cloud_init": {
                "user": "ops",
                "networks": [ { "ipv4": { "mode": "static", "address": "192.168.1.9/24" } } ],
                "dns": { "servers": ["192.168.1.1"] }
            }
        }),
    )
    .await;
    let task = restore(
        &admin,
        json!({
            "name": "web-dr", "pool": "local",
            "cloud_init_networks": [ { "ipv4": { "mode": "dhcp" } } ]
        }),
    )
    .await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let ci = vm(&env, "web-dr").spec.cloud_init.unwrap();
    assert_eq!(ci.user.as_deref(), Some("ops"));
    assert_eq!(
        serde_json::to_value(&ci.networks).unwrap(),
        json!([ { "ipv4": { "mode": "dhcp" } } ])
    );
    assert_eq!(
        serde_json::to_value(&ci.dns).unwrap(),
        json!({ "servers": ["192.168.1.1"] })
    );
}

/// SDN 네트워크로 간 NIC에 준 고정 주소는 IPAM 주소가 되고(cloud-init 항목은 비고 경고 없음), 다른 VM이 그 주소를 쓰면
/// Task가 분명한 까닭으로 실패하고 VM을 만들지 않는다.
#[tokio::test]
async fn sdn_nics_take_the_given_address_or_fail() {
    let env = env().await;
    let admin = env.admin().await;
    network(&env, "lan", json!({ "bridge": "vmbr0" })).await;
    network(
        &env,
        "tenant",
        json!({ "sdn": {}, "subnets": [ { "cidr": "10.10.0.0/24" } ] }),
    )
    .await;
    backup(
        &env,
        json!({ "nics": [
            { "network": "lan", "model": "virtio" },
            { "network": "lan", "model": "virtio" }
        ] }),
    )
    .await;
    let body = |name: &str| {
        json!({
            "name": name, "pool": "local",
            "networks": { "lan": "tenant" },
            "cloud_init_networks": [ { "ipv4": { "mode": "static", "address": "10.10.0.50/24", "gateway": "10.10.0.1" } } ]
        })
    };
    let task = restore(&admin, body("web-dr")).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(task["status"]["error"].is_null(), "{task}");
    let new = vm(&env, "web-dr");
    assert_eq!(
        new.spec.nics[0].ips,
        ["10.10.0.50".parse::<std::net::IpAddr>().unwrap()]
    );
    // 주소를 주지 않은 NIC는 지금처럼 IPAM이 고른다.
    assert_eq!(new.spec.nics[1].ips.len(), 1);
    assert_ne!(new.spec.nics[1].ips, new.spec.nics[0].ips);
    let ci = new.spec.cloud_init.unwrap();
    assert_eq!(serde_json::to_value(&ci.networks).unwrap(), json!([{}]));

    // 같은 주소를 또 달라면(web-dr가 씀) 실패한다.
    let task = restore(&admin, body("web-dr2")).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.contains("cannot give NIC 0 the requested address 10.10.0.50 on network \"tenant\""),
        "{error}"
    );
    assert!(
        env.cluster
            .get_by_name::<VmSpec>("web-dr2")
            .unwrap()
            .is_none()
    );
}
