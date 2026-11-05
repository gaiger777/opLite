//! 텔레메트리 설정·id·미리 보기: 끝점 검사, 켤 때 id, reset-id, 권한, 옛 노드, 보낼 내용(이름·주소가 없음).

mod common;

use common::Env;
use opl_types::backup_job::BackupJobSpec;
use opl_types::network::NetworkSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use opl_types::storage::StoragePoolSpec;
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use opl_types::version::{BUILD, TELEMETRY_URL};
use opl_types::vm::{Arch, VmPhase, VmSpec};
use opl_types::vm_import::VmImportSpec;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

/// 저장소 개발 키의 비밀 키 견본
const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");
const ENDPOINT: &str = "https://telemetry.example.com/v1/report";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

fn stored(env: &Env) -> Option<opl_types::settings::Settings> {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
}

fn telemetry_id(settings: &Value) -> Uuid {
    settings["status"]["telemetry"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id: {settings}"))
        .parse()
        .unwrap()
}

async fn add_node(
    env: &Env,
    name: &str,
    witness: bool,
    version: Option<&str>,
    tweak: impl FnOnce(&mut NodeStatus),
) -> Uuid {
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "10.9.8.7:8006".into(),
            maintenance: false,
            witness,
        },
    );
    node.status.version = version.map(Into::into);
    tweak(&mut node.status);
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// JSON spec으로 리소스를 바로 만든다.
async fn create<S>(env: &Env, name: &str, spec: Value)
where
    S: ResourceSpec + DeserializeOwned,
    S::Status: Default,
{
    let resource: Resource<S, S::Status> =
        Resource::new(name, serde_json::from_value::<S>(spec).unwrap());
    env.cluster.create(&resource).await.unwrap();
}

/// 끝점은 `https://` 주소여야 하고, 빌드 기본 끝점이 없으면 끝점 없이 켜지 않는다.
#[tokio::test]
async fn telemetry_needs_an_https_endpoint() {
    let env = Env::start().await;
    let admin = env.admin().await;
    if TELEMETRY_URL.is_empty() {
        let got = admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true } }),
                None,
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "telemetry_endpoint_required");
        assert_eq!(
            got.1["message"],
            "set telemetry.endpoint; this build has no default endpoint"
        );
    }
    for endpoint in [
        "http://telemetry.example.com/r",
        "https://",
        "https://bad host/r",
        "telemetry.example.com",
    ] {
        let got = admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true, "endpoint": endpoint } }),
                None,
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    // 끝점에 사용자 정보(비밀번호)를 두지 않는다 — 설정은 누구나 읽는다.
    for endpoint in [
        "https://user:secret@telemetry.example.com/r",
        "https://user@telemetry.example.com/r",
    ] {
        let got = admin
            .patch(
                "/settings",
                json!({ "telemetry": { "endpoint": endpoint } }),
                None,
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "telemetry.endpoint must not contain credentials"
        );
    }
    assert!(stored(&env).is_none());

    // 끝점만 두는 것도 된다(켜지 않음).
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "endpoint": ENDPOINT } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["spec"]["telemetry"],
        json!({ "enabled": false, "endpoint": ENDPOINT })
    );
    assert_eq!(got["status"], json!({}));
}

/// 처음 켜면 id가 생기고, 꺼도 남고, reset-id가 바꾼다. 끝점 `""`는 지운다.
#[tokio::test]
async fn enabling_makes_an_id_that_stays_until_reset() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["spec"],
        json!({
            "updates": { "channel": "stable" },
            "telemetry": { "enabled": true, "endpoint": ENDPOINT }
        })
    );
    let id = telemetry_id(&got);
    assert_eq!(id.get_version_num(), 4);
    assert_eq!(got["status"], json!({ "telemetry": { "id": id } }));
    assert_eq!(admin.get("/settings").await.1, got);
    let stored = stored(&env).unwrap();
    assert!(stored.spec.telemetry.enabled);
    assert_eq!(stored.status.telemetry.unwrap().id, Some(id));

    // 다시 켜도 id는 그대로다.
    let (_, again) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": true } }),
            None,
        )
        .await;
    assert_eq!(telemetry_id(&again), id);
    assert_eq!(again["revision"], got["revision"]);

    let (status, off) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": false } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{off}");
    assert_eq!(off["spec"]["telemetry"]["enabled"], false);
    assert_eq!(off["spec"]["telemetry"]["endpoint"], ENDPOINT);
    assert_eq!(telemetry_id(&off), id);

    // 끝점을 지우면 기본값이 되어 spec에서 빠진다.
    let (status, cleared) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "endpoint": "" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(
        cleared["spec"],
        json!({ "updates": { "channel": "stable" } })
    );

    let (status, reset) = admin.post("/telemetry/reset-id", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    let new_id = telemetry_id(&reset);
    assert_ne!(new_id, id);
    assert_eq!(new_id.get_version_num(), 4);
    assert_eq!(reset["name"], SETTINGS_NAME);
    assert_eq!(admin.get("/settings").await.1, reset);
}

/// 설정이 아직 없어도 reset-id가 만들고, 그 응답은 구독 키를 담지 않는다.
#[tokio::test]
async fn reset_id_creates_the_settings_and_hides_the_key() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Standard,
            nodes: 3,
            issued_at: now() - 60,
            expires_at: now() + 86_400,
        },
    )
    .unwrap();
    let (status, got) = admin.post("/telemetry/reset-id", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"], json!({ "updates": { "channel": "stable" } }));
    let first = telemetry_id(&got);
    assert_eq!(
        stored(&env).unwrap().status.telemetry.unwrap().id,
        Some(first)
    );

    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key }), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, got) = admin.post("/telemetry/reset-id", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_ne!(telemetry_id(&got), first);
    assert!(!got.to_string().contains(&key), "{got}");
    assert_eq!(stored(&env).unwrap().spec.subscription, Some(key));
}

#[tokio::test]
async fn only_admins_change_telemetry_or_preview_it() {
    let env = Env::start().await;
    let body = json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } });
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        assert_error(
            &client.patch("/settings", body.clone(), None).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
        assert_error(
            &client.post("/telemetry/reset-id", json!({})).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
        assert_error(
            &client.get("/telemetry/preview").await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    assert!(stored(&env).is_none());

    // 켠 뒤에는 누구나 상태를 읽는다.
    let admin = env.admin().await;
    let (_, got) = admin.patch("/settings", body, None).await;
    let viewer = env.user("viewer2", "viewer").await;
    let (status, seen) = viewer.get("/settings").await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert_eq!(seen["status"], got["status"]);
}

/// 이 빌드보다 낮은 노드가 있으면 텔레메트리를 바꾸지 않는다(옛 노드는 새 필드를 풀지 못한다).
#[tokio::test]
async fn telemetry_waits_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    add_node(&env, "old", true, Some("0.0.1~old"), |_| {}).await;
    for got in [
        admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } }),
                None,
            )
            .await,
        admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": false } }),
                None,
            )
            .await,
        admin.post("/telemetry/reset-id", json!({})).await,
    ] {
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
        assert_eq!(
            got.1["message"],
            "some nodes run an older opLite (old); finish the upgrade on every node first"
        );
    }
    assert!(stored(&env).is_none());
    // 채널은 그대로 바꾼다. 미리 보기도 된다.
    assert_eq!(
        admin
            .patch(
                "/settings",
                json!({ "updates": { "channel": "testing" } }),
                None
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(admin.get("/telemetry/preview").await.0, StatusCode::OK);
}

/// 미리 보기는 지금 보낼 그대로다: 수만 있고 이름·주소·MAC·고객·키는 없다.
#[tokio::test]
async fn preview_counts_resources_without_names_or_addresses() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "secret-sub".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes: 10,
            issued_at: now() - 60,
            expires_at: now() + 86_400,
        },
    )
    .unwrap();
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key }), None)
            .await
            .0,
        StatusCode::OK
    );

    // node1(Env): 이 빌드, aarch64, mock, CPU 4, 8192 MiB
    let node2 = add_node(&env, "secret-node2", false, Some("0.0.1~old"), |s| {
        s.arch = Some(Arch::X86_64);
        s.cpus = 8;
        s.memory_mib = 16_896;
        s.hypervisor = "qemu-kvm".into();
    })
    .await;
    add_node(&env, "secret-node3", false, None, |s| {
        s.cpus = 2;
        s.memory_mib = 1024;
        s.hypervisor = "qemu-kvm".into();
    })
    .await;
    // 위트니스는 버전과 노드 수에만 센다.
    add_node(&env, "secret-witness", true, Some(BUILD), |s| {
        s.arch = Some(Arch::X86_64);
        s.cpus = 64;
        s.memory_mib = 1 << 20;
        s.hypervisor = "none".into();
    })
    .await;

    // VM은 다른 노드에 둔다(이 노드의 reconciler가 건드리지 않게).
    for (name, mac, ha, running) in [
        ("secret-vm-a", "52:54:00:ab:cd:01", true, true),
        ("secret-vm-b", "52:54:00:ab:cd:02", false, true),
        ("secret-vm-c", "52:54:00:ab:cd:03", false, false),
    ] {
        let mut vm: opl_types::vm::Vm = Resource::new(
            name,
            serde_json::from_value::<VmSpec>(json!({
                "cpu": { "cores": 1 },
                "memory_mib": 256,
                "machine": "q35",
                "nics": [ { "network": "secret-net", "model": "virtio", "mac": mac } ],
                "node": node2,
                "ha": ha
            }))
            .unwrap(),
        );
        if running {
            vm.status.phase = VmPhase::Running;
        }
        env.cluster.create(&vm).await.unwrap();
    }
    for (name, backend) in [
        (
            "secret-pool-a",
            json!({ "kind": "dir", "path": "/srv/secret" }),
        ),
        (
            "secret-pool-b",
            json!({ "kind": "dir", "path": "/srv/secret2" }),
        ),
        (
            "secret-pool-c",
            json!({ "kind": "nfs", "server": "10.9.8.7", "export": "/secret" }),
        ),
    ] {
        create::<StoragePoolSpec>(&env, name, json!({ "backend": backend, "nodes": [node2] }))
            .await;
    }
    for (name, spec) in [
        ("secret-net", json!({ "bridge": "vmbr-secret" })),
        (
            "secret-vlan",
            json!({ "bridge": "vmbr-secret", "vlan": 42 }),
        ),
        ("secret-vx", json!({ "vxlan": { "vni": 4242 } })),
    ] {
        create::<NetworkSpec>(&env, name, spec).await;
    }
    for name in ["secret-sg-a", "secret-sg-b"] {
        create::<SecurityGroupSpec>(&env, name, json!({ "description": "secret-desc" })).await;
    }
    create::<BackupJobSpec>(
        &env,
        "secret-job",
        json!({ "vms": { "all": true }, "pool": "secret-pool-a", "schedule": { "at": "03:00" } }),
    )
    .await;
    for (name, source) in [
        (
            "secret-imp-a",
            json!({ "ova": { "file_name": "secret.ova", "size_bytes": 1 } }),
        ),
        (
            "secret-imp-b",
            json!({ "ova": { "file_name": "secret2.ova", "size_bytes": 1 } }),
        ),
        (
            "secret-imp-c",
            json!({ "esxi": {
                "host": "esxi.secret.example", "port": 443, "datacenter": "ha-datacenter",
                "datastore": "secret-ds", "vmx": "secret/secret.vmx", "user": "root",
                "fingerprint": "AB:CD"
            } }),
        ),
    ] {
        create::<VmImportSpec>(&env, name, json!({ "node": node2, "source": source })).await;
    }

    let before = now();
    let (status, report) = admin.get("/telemetry/preview").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let sent_at = report["sent_at"].as_u64().unwrap();
    assert!(sent_at >= before && sent_at <= now(), "{report}");
    let mut versions = serde_json::Map::new();
    versions.insert(BUILD.into(), json!(2));
    versions.insert("0.0.1~old".into(), json!(1));
    versions.insert("unknown".into(), json!(1));
    assert_eq!(
        report,
        json!({
            "schema": 1,
            "id": Uuid::nil(),
            "sent_at": sent_at,
            "versions": versions,
            "nodes": { "total": 4, "witness": 1 },
            "arch": { "aarch64": 1, "x86_64": 1, "unknown": 1 },
            "hypervisor": { "mock": 1, "qemu-kvm": 2 },
            "cpus": 14,
            // (8192 + 16896 + 1024) MiB = 25.5 GiB
            "memory_gib": 25,
            "vms": { "total": 3, "running": 2, "ha": 1 },
            "pools": { "dir": 2, "nfs": 1 },
            "networks": { "bridge": 1, "vlan": 1, "vxlan": 1 },
            "security_groups": 2,
            "backup_jobs": 1,
            "imports": { "ova": 2, "esxi": 1 },
            "subscription": "premium"
        })
    );
    let text = report.to_string();
    for secret in [
        "secret",
        "node1",
        "10.9.8.7",
        "127.0.0.1",
        "52:54:00",
        "Example Corp",
        "root",
        "admin",
        key.as_str(),
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

/// 켠 뒤 미리 보기의 id는 상태의 id다. 구독이 없으면 `none`.
#[tokio::test]
async fn preview_uses_the_telemetry_id() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, preview) = admin.get("/telemetry/preview").await;
    assert_eq!(preview["id"], json!(Uuid::nil()));
    assert_eq!(preview["subscription"], "none");
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (_, preview) = admin.get("/telemetry/preview").await;
    assert_eq!(preview["id"], json!(telemetry_id(&got)));
}
