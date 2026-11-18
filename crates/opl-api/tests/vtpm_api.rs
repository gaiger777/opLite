//! 계획 59 API: 기능 게이트(`vtpm`·`guest-agent`), 펌웨어 초기화 경로와 노드 고르기, Secure Boot 바꾸기 규칙, 실행 중
//! TPM 변경의 보류, 꺼진 TPM VM 켜기의 미리 보기, 게스트 정보·비밀번호(가림과 감사), 복원의 409.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use common::{Client, Env, WAIT, vm_request};
use opl_peer::{GuestAgentError, GuestAgentSource};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::backup::{Backup, BackupPhase, BackupSpec};
use opl_types::guest::{GuestInfo, GuestInfoResponse, GuestInfoState};
use opl_types::node::{GUEST_AGENT, Node, NodeSpec, NodeStatus, VTPM};
use opl_types::resource::Resource;
use opl_types::vm::{Vm, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn get_vm(env: &Env, id: Uuid) -> Vm {
    env.cluster.get::<VmSpec>(id).unwrap().unwrap()
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// UEFI VM을 만든다. `tweak`으로 스펙을 더 바꾼다.
async fn uefi_vm(admin: &Client, name: &str, tweak: impl FnOnce(&mut Value)) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["firmware"] = json!("uefi");
    tweak(&mut req["spec"]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// VM status를 바꾼다(작업자가 펌웨어 칸을 기록한 것처럼).
async fn set_status(env: &Env, vm: Uuid, f: impl FnOnce(&mut VmStatus)) {
    let mut status = get_vm(env, vm).status;
    f(&mut status);
    env.cluster
        .update_status::<VmSpec>(vm, &status)
        .await
        .unwrap();
}

/// 이 노드가 기능 `feature`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_feature(env: &Env, feature: &str) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != feature);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 꺼진 다른 노드 `node2`를 더한다.
async fn offline_node(env: &Env) -> Uuid {
    let mut node: Node = Resource::new(
        "node2",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.status = NodeStatus {
        online: false,
        ..env
            .cluster
            .get::<NodeSpec>(env.node)
            .unwrap()
            .unwrap()
            .status
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// VM `name`의 스펙을 `f`로 고쳐 PUT한다.
async fn put_spec(admin: &Client, name: &str, f: impl FnOnce(&mut Value)) -> (StatusCode, Value) {
    let (status, vm) = admin.get(&format!("/vms/{name}")).await;
    assert_eq!(status, StatusCode::OK, "{vm}");
    let mut spec = vm["spec"].clone();
    f(&mut spec);
    admin
        .put(&format!("/vms/{name}"), json!({ "spec": spec }), None)
        .await
}

async fn audit_entries(env: &Env, found: impl Fn(&[AuditEntry]) -> bool) -> Vec<AuditEntry> {
    let start = Instant::now();
    loop {
        let entries = env
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
            .unwrap();
        if found(&entries) {
            return entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn old_nodes_refuse_tpm_secure_boot_and_the_guest_agent() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uefi_vm(&admin, "plain", |_| {}).await;
    drop_feature(&env, VTPM).await;
    drop_feature(&env, GUEST_AGENT).await;
    for (field, value) in [
        ("tpm", json!(true)),
        ("secure_boot", json!(true)),
        ("guest_agent", json!({})),
    ] {
        let mut req = vm_request("new");
        req["spec"]["firmware"] = json!("uefi");
        req["spec"][field] = value.clone();
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
        let (status, body) = put_spec(&admin, "plain", |s| s[field] = value.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
    // 새 필드가 없는 VM은 그대로 고친다.
    let (status, body) = put_spec(&admin, "plain", |s| s["memory_mib"] = json!(512)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin
        .post("/vms/plain/firmware/reset", json!({ "uefi_vars": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let (status, body) = admin
        .post(
            "/vms/plain/guest/password",
            json!({ "user": "root", "password": "pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
}

#[tokio::test]
async fn firmware_reset_makes_a_task_for_a_stopped_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let id = uefi_vm(&admin, "win", |s| s["tpm"] = json!(true)).await;

    let (status, body) = admin.post("/vms/win/firmware/reset", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = admin
        .post(
            "/vms/win/firmware/reset",
            json!({ "tpm": true, "secure_boot": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = admin
        .post(
            "/vms/win/firmware/reset",
            json!({ "uefi_vars": true, "x": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    set_status(&env, id, |s| {
        s.firmware_node = Some(env.node);
        s.tpm_sha256 = Some("ab".repeat(32));
    })
    .await;
    let (status, task) = admin
        .post(
            "/vms/win/firmware/reset",
            json!({ "uefi_vars": true, "tpm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-firmware-reset");
    assert_eq!(task["spec"]["op"]["uefi_vars"], true);
    assert_eq!(task["spec"]["op"]["tpm"], true);
    assert_eq!(task["spec"]["node"], env.node.to_string());
    succeeded(&admin.wait_task(&task).await);
    let vm = get_vm(&env, id);
    assert_eq!(vm.status.firmware_epoch, 1);
    assert_eq!(vm.status.tpm_sha256, None);
    let entries = audit_entries(&env, |es| {
        es.iter()
            .any(|e| e.action == "POST /vms/{key}/firmware/reset" && e.status == 202)
    })
    .await;
    let entry = entries
        .iter()
        .find(|e| e.action == "POST /vms/{key}/firmware/reset" && e.status == 202)
        .unwrap();
    assert_eq!(
        entry.detail.as_deref(),
        Some("vm firmware reset win (uefi vars, tpm)")
    );

    // 켜진 VM은 초기화하지 않는다.
    succeeded(&admin.act("win", "start").await);
    let (status, body) = admin
        .post("/vms/win/firmware/reset", json!({ "tpm": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");

    // 다른 권한: operator는 되고 viewer는 안 된다.
    let viewer = env.user("v", "viewer").await;
    let (status, _) = viewer
        .post("/vms/win/firmware/reset", json!({ "tpm": true }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// TPM 상태가 있는 VM의 변수만 초기화는 그 상태를 가진 노드가 오프라인이면 409이고, TPM까지 초기화하면 이 노드에서 돈다.
#[tokio::test]
async fn a_vars_only_reset_of_a_tpm_vm_needs_its_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let id = uefi_vm(&admin, "win", |s| s["tpm"] = json!(true)).await;
    let node2 = offline_node(&env).await;
    set_status(&env, id, |s| {
        s.firmware_node = Some(node2);
        s.tpm_sha256 = Some("ab".repeat(32));
    })
    .await;
    let (status, body) = admin
        .post("/vms/win/firmware/reset", json!({ "uefi_vars": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert!(
        body["message"].as_str().unwrap().contains("\"node2\""),
        "{body}"
    );
    let (status, task) = admin
        .post(
            "/vms/win/firmware/reset",
            json!({ "uefi_vars": true, "tpm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    succeeded(&admin.wait_task(&task).await);
    let vm = get_vm(&env, id);
    assert_eq!(vm.status.firmware_node, None);
    assert_eq!(vm.status.tpm_sha256, None);
}

/// 켤 노드가 TPM 상태를 얻을 수 없으면 켜기가 409 `tpm_state_unavailable`이다. 그 노드에 사본이 있으면 켠다.
#[tokio::test]
async fn start_refuses_a_tpm_vm_whose_state_no_node_can_give() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let id = uefi_vm(&admin, "win", |s| s["tpm"] = json!(true)).await;
    let node2 = offline_node(&env).await;
    set_status(&env, id, |s| {
        s.firmware_node = Some(node2);
        s.tpm_sha256 = Some("ab".repeat(32));
    })
    .await;
    let (status, body) = admin.post("/vms/win/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "tpm_state_unavailable");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("\"node2\" (offline)") && message.contains("vm firmware reset win --tpm"),
        "{message}"
    );
    assert_eq!(
        get_vm(&env, id).spec.power,
        opl_types::vm::PowerState::Stopped
    );

    set_status(&env, id, |s| s.firmware_copies = vec![env.node]).await;
    let (status, body) = admin.post("/vms/win/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

/// UEFI 변수가 있는 VM의 Secure Boot는 PUT으로 바꾸지 못하고(409 `firmware_reset_required`), 초기화와 함께 바꾼다.
/// 초기화 Task가 끝날 때까지 켜지 않는다. 실행 중에는 409 `vm_running`이다.
#[tokio::test]
async fn secure_boot_changes_with_a_reset_of_the_uefi_variables() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let id = uefi_vm(&admin, "win", |s| s["tpm"] = json!(true)).await;
    set_status(&env, id, |s| s.firmware_node = Some(env.node)).await;
    let (status, body) = put_spec(&admin, "win", |s| s["secure_boot"] = json!(true)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "firmware_reset_required");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("--secure-boot on --reset-uefi-vars"),
        "{body}"
    );

    env.stop_runner().await;
    let (status, task) = admin
        .post(
            "/vms/win/firmware/reset",
            json!({ "uefi_vars": true, "secure_boot": true }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    // 스펙은 Task가 초기화와 함께 쓴다(리뷰 1 M1).
    assert_eq!(task["spec"]["op"]["secure_boot"], true, "{task}");
    assert!(!get_vm(&env, id).spec.secure_boot);
    let (status, body) = admin.post("/vms/win/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    let (status, body) = put_spec(&admin, "win", |s| s["power"] = json!("running")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    env.start_runner().await;
    succeeded(&admin.wait_task(&task).await);
    assert!(get_vm(&env, id).spec.secure_boot);
    // TPM VM의 변수만 초기화는 TPM 상태의 자리를 지킨다.
    assert_eq!(get_vm(&env, id).status.firmware_node, Some(env.node));
    let entries = audit_entries(&env, |es| {
        es.iter()
            .any(|e| e.action == "POST /vms/{key}/firmware/reset" && e.status == 202)
    })
    .await;
    assert!(
        entries
            .iter()
            .any(|e| e.detail.as_deref()
                == Some("vm firmware reset win (uefi vars, secure boot on)"))
    );

    // 변수가 없는 VM은 PUT으로 바꾼다. 켜진 VM은 409 `vm_running`이다.
    uefi_vm(&admin, "fresh", |_| {}).await;
    let (status, body) = put_spec(&admin, "fresh", |s| s["secure_boot"] = json!(true)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    succeeded(&admin.act("fresh", "start").await);
    let (status, body) = put_spec(&admin, "fresh", |s| s["secure_boot"] = json!(false)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");
    // 끌 때도 변수 초기화와 함께다. UEFI가 아닌 VM의 Secure Boot는 400이다.
    let (status, body) = admin
        .post(
            "/vms/win/firmware/reset",
            json!({ "uefi_vars": true, "secure_boot": false }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    succeeded(&admin.wait_task(&body).await);
    assert!(!get_vm(&env, id).spec.secure_boot);
    let mut bios = vm_request("bios");
    bios["spec"]["machine"] = json!("q35");
    bios["spec"]["firmware"] = json!("bios");
    let (status, body) = admin.post("/vms", bios).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/vms/bios/firmware/reset",
            json!({ "uefi_vars": true, "secure_boot": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!body.to_string().contains("task"), "{body}");
}

/// 실행 중에 TPM과 게스트 에이전트 채널을 켜면 다음 재시작 때 적용하는 보류가 된다.
#[tokio::test]
async fn tpm_and_agent_channel_changes_wait_for_a_restart() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let id = uefi_vm(&admin, "web", |_| {}).await;
    succeeded(&admin.act("web", "start").await);
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["tpm"] = json!(true);
    spec["guest_agent"] = json!({});
    let (status, task, body) = admin.put_task("/vms/web", json!({ "spec": spec })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task = task.expect("a hotplug task");
    succeeded(&admin.wait_task(&json!({ "id": task })).await);
    let hw = get_vm(&env, id).status.hardware.unwrap();
    let pending: Vec<_> = hw.pending.iter().map(|p| p.field.as_str()).collect();
    assert_eq!(pending, ["tpm", "guest_agent"], "{hw:?}");
    // 장치가 아닌 설정은 그대로 받는다.
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["guest_agent"] = json!({ "freeze_fs": false, "shutdown": "acpi" });
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn restore_refuses_tpm_backups_on_old_clusters() {
    let env = Env::start().await;
    let admin = env.admin().await;
    drop_feature(&env, VTPM).await;
    let vm_spec: VmSpec = serde_json::from_value(json!({
        "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "firmware": "uefi",
    }))
    .unwrap();
    let mut backup: Backup = Resource::new(
        "b1",
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: "win".into(),
            pool: "backups".into(),
            vm_spec,
            job: None,
            origin: None,
            incremental: false,
            max_chain: None,
        },
    );
    backup.status.phase = BackupPhase::Ready;
    backup.status.tpm_state = true;
    env.cluster.create(&backup).await.unwrap();
    env.cluster
        .update_status::<BackupSpec>(backup.id, &backup.status)
        .await
        .unwrap();
    let (status, body) = admin
        .post(
            "/backups/b1/restore",
            json!({ "name": "win2", "pool": "local" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
}

/// 시험용 게스트 에이전트 소스: 정보는 늘 답하고, 비밀번호 설정을 적는다.
#[derive(Default)]
struct FakeGuest {
    refreshes: Mutex<Vec<bool>>,
    passwords: Mutex<Vec<(String, String)>>,
    fail: Mutex<Option<GuestAgentError>>,
}

#[async_trait]
impl GuestAgentSource for FakeGuest {
    async fn info(&self, _vm: Uuid, refresh: bool) -> GuestInfoResponse {
        self.refreshes.lock().unwrap().push(refresh);
        GuestInfoResponse {
            state: GuestInfoState::Answered,
            info: Some(GuestInfo {
                collected_at: 7,
                hostname: Some("guest-1".into()),
                ..Default::default()
            }),
        }
    }

    async fn set_password(
        &self,
        _vm: Uuid,
        user: &str,
        password: &str,
    ) -> Result<(), GuestAgentError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        self.passwords
            .lock()
            .unwrap()
            .push((user.into(), password.into()));
        Ok(())
    }
}

#[tokio::test]
async fn guest_information_comes_from_the_node_running_the_vm() {
    let guest = Arc::new(FakeGuest::default());
    let source = guest.clone();
    let env = Env::start_with_state(|s| s.guest = Some(source)).await;
    let admin = env.admin().await;
    uefi_vm(&admin, "agent", |s| s["guest_agent"] = json!({})).await;
    uefi_vm(&admin, "plain", |_| {}).await;

    let (status, body) = admin.get("/vms/agent/guest").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "state": "not_running" }));
    succeeded(&admin.act("agent", "start").await);
    succeeded(&admin.act("plain", "start").await);
    let (status, body) = admin.get("/vms/plain/guest").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "no_agent");

    let viewer = env.user("v", "viewer").await;
    let (status, body) = viewer.get("/vms/agent/guest").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "answered");
    assert_eq!(body["info"]["hostname"], "guest-1");
    let (status, body) = viewer.get("/vms/agent/guest?refresh=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(*guest.refreshes.lock().unwrap(), [false, true]);
    let (status, _) = viewer.get("/vms/agent/guest?refresh=maybe").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 게스트 정보 소스가 없는 노드(옛 노드처럼)의 VM은 `unsupported`다.
#[tokio::test]
async fn guest_information_is_unsupported_without_a_source() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uefi_vm(&admin, "agent", |s| s["guest_agent"] = json!({})).await;
    succeeded(&admin.act("agent", "start").await);
    let (status, body) = admin.get("/vms/agent/guest").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "state": "unsupported" }));
}

#[tokio::test]
async fn the_guest_password_is_set_but_never_recorded() {
    let guest = Arc::new(FakeGuest::default());
    let source = guest.clone();
    let env = Env::start_with_state(|s| s.guest = Some(source)).await;
    let admin = env.admin().await;
    uefi_vm(&admin, "agent", |s| s["guest_agent"] = json!({})).await;
    uefi_vm(&admin, "plain", |_| {}).await;
    let set = |vm: &'static str, user: &'static str| {
        let admin = &admin;
        async move {
            admin
                .post(
                    &format!("/vms/{vm}/guest/password"),
                    json!({ "user": user, "password": "s3cret-Pw" }),
                )
                .await
        }
    };

    let (status, body) = set("agent", "Administrator").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_not_running");
    succeeded(&admin.act("agent", "start").await);
    succeeded(&admin.act("plain", "start").await);
    let (status, body) = set("agent", "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = set("plain", "root").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "guest_agent_unavailable");

    let (status, body) = set("agent", "Administrator").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "user": "Administrator" }));
    assert_eq!(
        *guest.passwords.lock().unwrap(),
        [("Administrator".to_string(), "s3cret-Pw".to_string())]
    );

    *guest.fail.lock().unwrap() = Some(GuestAgentError::Unavailable("timed out".into()));
    let (status, body) = set("agent", "Administrator").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "guest_agent_unavailable");
    assert!(!body.to_string().contains("s3cret"), "{body}");

    // 읽기 권한으로는 바꾸지 못한다.
    let viewer = env.user("v", "viewer").await;
    let (status, _) = viewer
        .post(
            "/vms/agent/guest/password",
            json!({ "user": "root", "password": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let action = "POST /vms/{key}/guest/password";
    let entries = audit_entries(&env, |es| {
        es.iter().any(|e| e.action == action && e.status == 200)
    })
    .await;
    let ok = entries
        .iter()
        .find(|e| e.action == action && e.status == 200)
        .unwrap();
    assert_eq!(
        ok.detail.as_deref(),
        Some("vm guest password agent user Administrator")
    );
    let all = serde_json::to_string(&entries).unwrap();
    assert!(!all.contains("s3cret"), "{all}");
}
