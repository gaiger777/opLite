//! 단순 → SDN 옮기기 엔진(계획 67 §9, 스테이지 4): 계획을 적은 리소스, `network-migrate` Task의 VM마다 방식(끈 채·라이브
//! `RelinkNic`·다시 켜기), 주소 옮기기(준 주소·cloud-init 고정 주소), 다리 놓기·걷기, VM 하나 되돌리기, 끝내기(라우터
//! 붙이기), 라이브 실패의 되돌림, 다시 계획이 달라진 VM, opld가 다시 떠도 이어서 하기, 일반 `PUT`의 `nic_network_live`와
//! 다리 동안 라우터 붙이기의 `network_migrating`. Mock 하이퍼바이저와 가짜 OVN 실행기(태그만 쓴다)로 돈다.

mod common;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, vm_request};
use orbivisor_driver::HotplugCall;
use orbivisor_driver::ovn::{FakeRunner, OvnHost, OvnOptions, OvnPaths};
use orbivisor_types::network::NetworkSpec;
use orbivisor_types::network_migration::{
    AddressSource, MigrationPhase, MigrationStep, MoveState, NetworkMigration,
    NetworkMigrationSpec, PlannedMode,
};
use orbivisor_types::node::{HOTPLUG, NodeHotplug, NodeSpec};
use orbivisor_types::resource::Resource;
use orbivisor_types::sdn::{ChassisState, NodeSdn, PhysnetState};
use orbivisor_types::task::{TaskSpec, TaskState};
use orbivisor_types::vm::{Vm, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const TAG: &str = "abcdef012345";
const LIMIT: Duration = Duration::from_secs(30);

/// 가짜 실행기의 OVN 단계(Task는 다리 physnet 이름의 태그만 읽는다)
fn ovn_host(dir: &std::path::Path) -> Arc<OvnHost> {
    let paths = OvnPaths {
        etc: dir.join("etc"),
        var_lib: dir.join("var"),
        run: dir.join("run"),
        log: dir.join("log"),
        systemd: dir.join("systemd"),
        pki: dir.join("pki"),
        schemas: dir.join("schemas"),
        distro_nb_db: dir.join("nb.db"),
        ovs_system_id: dir.join("system-id.conf"),
    };
    Arc::new(OvnHost::new(
        Arc::new(FakeRunner::new()),
        paths,
        OvnOptions::default(),
        TAG.into(),
    ))
}

struct T {
    env: Env,
    admin: Client,
}

impl T {
    /// 이 노드가 chassis(핫플러그 기록 있음)이고 단순 네트워크 `lan`(브리지 br0)과 같은 CIDR의 SDN 오버레이 `t-lan`이 있다.
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// [`T::start`]에 Task 실행기 설정 `tweak`을 더한다.
    async fn start_with(tweak: impl FnOnce(&mut orbivisor_api::TaskRunnerConfig)) -> Self {
        Self::start_env(Env::start().await, tweak).await
    }

    /// [`T::start`]에 게스트 에이전트 정보 소스 `guest`를 단다.
    async fn start_guest(guest: Arc<dyn orbivisor_peer::GuestAgentSource>) -> Self {
        let env = Box::pin(Env::start_with_state(|s| s.guest = Some(guest))).await;
        Self::start_env(env, |_| {}).await
    }

    async fn start_env(
        mut env: Env,
        tweak: impl FnOnce(&mut orbivisor_api::TaskRunnerConfig),
    ) -> Self {
        let tmp = env.dir.path().join("ovn");
        env.stop_runner().await;
        let host = ovn_host(&tmp);
        Box::pin(
            env.start_runner_config(Arc::new(FakeStorage::default()), move |c| {
                c.sdn = Some(host);
                tweak(c);
            }),
        )
        .await;
        let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
        me.status.hotplug = Some(NodeHotplug::default());
        me.status.bridges = vec![orbivisor_types::node::BridgeInfo {
            name: "br0".into(),
            vlan_filtering: false,
        }];
        me.status.sdn = Some(NodeSdn {
            chassis: ChassisState::Ready,
            ..NodeSdn::default()
        });
        assert!(me.status.features.iter().any(|f| f == HOTPLUG));
        env.cluster
            .update_status::<NodeSpec>(env.node, &me.status)
            .await
            .unwrap();
        for (name, spec) in [
            ("lan", json!({ "bridge": "br0" })),
            (
                "t-lan",
                json!({ "sdn": {}, "subnets": [{ "cidr": "10.66.0.0/24", "gateway": "10.66.0.1" }] }),
            ),
        ] {
            let n = Resource::new(name, serde_json::from_value::<NetworkSpec>(spec).unwrap());
            env.cluster.create(&n).await.unwrap();
        }
        // 라우터 API가 받도록 준비된 SDN(중앙 하나 — 이 시험은 OVN을 부르지 않는다)
        let mut sdn: orbivisor_types::sdn::Sdn = Resource::new(
            orbivisor_types::sdn::SDN_NAME,
            orbivisor_types::sdn::SdnSpec::default(),
        );
        sdn.status.phase = orbivisor_types::sdn::SdnPhase::Ready;
        sdn.status.centrals = vec![orbivisor_types::sdn::SdnCentral {
            node: env.node,
            address: "10.1.0.5".parse().unwrap(),
        }];
        env.cluster.create(&sdn).await.unwrap();
        let admin = env.admin().await;
        Self { env, admin }
    }

    /// 단순 `lan` NIC 하나인 VM. `ci`면 cloud-init 고정 주소 `ci`/24, `start`면 켠다.
    async fn vm(&self, name: &str, ci: Option<&str>, start: bool) -> Uuid {
        let mut req = vm_request(name);
        req["spec"]["nics"] = json!([{ "network": "lan", "model": "virtio" }]);
        if let Some(ip) = ci {
            req["spec"]["cloud_init"] = json!({ "networks": [{ "ipv4": { "mode": "static", "address": format!("{ip}/24"), "gateway": "10.66.0.1" } }] });
        }
        let (status, vm) = self.admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{vm}");
        if start {
            let task = self.admin.act(name, "start").await;
            assert_eq!(task["status"]["state"], "succeeded", "{task}");
        }
        vm["id"].as_str().unwrap().parse().unwrap()
    }

    fn vm_of(&self, id: Uuid) -> Vm {
        self.env.cluster.get::<VmSpec>(id).unwrap().unwrap()
    }

    fn migration(&self, id: Uuid) -> NetworkMigration {
        self.env
            .cluster
            .get::<NetworkMigrationSpec>(id)
            .unwrap()
            .unwrap()
    }

    /// `POST /network-migrations`(스테이지 5 API)
    async fn create(&self, spec: Value) -> NetworkMigration {
        let (status, m) = self
            .admin
            .post("/network-migrations", json!({ "name": "m", "spec": spec }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{m}");
        assert!(m["plan"]["nics"].is_array(), "{m}");
        self.migration(m["id"].as_str().unwrap().parse().unwrap())
    }

    /// `POST /network-migrations/{id}/{step}`
    async fn post_step(&self, id: Uuid, step: &str, body: Value) -> (StatusCode, Value) {
        self.admin
            .post(&format!("/network-migrations/{id}/{step}"), body)
            .await
    }

    /// 걸음 하나를 Task로 하고 끝난 Task(상태, 오류·경고)
    async fn step(
        &self,
        id: Uuid,
        step: MigrationStep,
        vms: Vec<Uuid>,
        router: Option<&str>,
    ) -> (TaskState, Option<String>) {
        let (path, body) = match step {
            MigrationStep::Start => ("start", json!({})),
            MigrationStep::Rollback => ("rollback", json!({ "vms": vms })),
            MigrationStep::Finish => ("finish", json!({ "router": router })),
        };
        let (status, task) = self.post_step(id, path, body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        self.wait_task(task["id"].as_str().unwrap().parse().unwrap())
            .await
    }

    async fn wait_task(&self, id: Uuid) -> (TaskState, Option<String>) {
        let deadline = tokio::time::Instant::now() + LIMIT;
        loop {
            let t = self.env.cluster.get::<TaskSpec>(id).unwrap().unwrap();
            if t.status.state.is_finished() {
                return (t.status.state, t.status.error);
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "task {id} did not finish"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 다리 노드의 SDN 에이전트처럼: 옮기기의 다리 physnet을 알리거나(있으면) 거둔다.
    fn bridge_agent(&self, id: Uuid) -> tokio::task::JoinHandle<()> {
        let cluster = self.env.cluster.clone();
        let node = self.env.node;
        tokio::spawn(async move {
            loop {
                let want = cluster
                    .get::<NetworkMigrationSpec>(id)
                    .ok()
                    .flatten()
                    .and_then(|m| m.status.bridge)
                    .filter(|b| b.node == node);
                let mut me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
                let sdn = me.status.sdn.get_or_insert_with(NodeSdn::default);
                let physnets: Vec<PhysnetState> = want
                    .iter()
                    .map(|b| PhysnetState {
                        name: b.physnet.clone(),
                        bridge: "br0".into(),
                        ready: true,
                        error: None,
                    })
                    .collect();
                if sdn.physnets != physnets {
                    sdn.physnets = physnets;
                    let _ = cluster.update_status::<NodeSpec>(node, &me.status).await;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    }
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn relinks(env: &Env) -> Vec<(usize, bool)> {
    env.hv
        .hotplug_calls()
        .into_iter()
        .filter_map(|c| match c {
            HotplugCall::RelinkNic { index, to_ovn, .. } => Some((index, to_ovn)),
            _ => None,
        })
        .collect()
}

/// 꺼진 VM(cloud-init 주소)은 스펙만, 켜진 VM(준 주소, MTU가 줄어도 됨)은 tap만 옮긴다. 다리가 놓였다가 끝내기에서 걷히고,
/// 한 VM은 되돌리면 원본 모습 그대로다. 끝낸 옮기기는 원본·VM을 더 잡지 않는다.
#[tokio::test]
async fn offline_and_live_moves_rollback_and_finish() {
    let t = T::start().await;
    let a = t.vm("a", Some("10.66.0.11"), false).await;
    let b = t.vm("b", None, true).await;
    let b_before = t.vm_of(b).spec;
    let b_mac = b_before.nics[0].mac.unwrap();
    let m = t
        .create(json!({
            "source": "lan", "target": "t-lan", "allow_mtu_shrink": true,
            "addresses": [{ "vm": b, "mac": b_mac.to_string(), "ip": ["10.66.0.12"] }]
        }))
        .await;
    assert_eq!(m.status.phase, MigrationPhase::Planned);
    let by =
        |m: &NetworkMigration, vm: Uuid| m.status.vms.iter().find(|v| v.vm == vm).unwrap().clone();
    assert_eq!(by(&m, a).mode, PlannedMode::Offline);
    assert_eq!(by(&m, a).address_source, AddressSource::CloudInit);
    assert_eq!(by(&m, b).mode, PlannedMode::Live);
    assert_eq!(by(&m, b).address_source, AddressSource::Given);
    // 같은 원본에 두 번째 옮기기는 Raft가 막는다.
    let (status, err) = t
        .admin
        .post(
            "/network-migrations",
            json!({ "name": "m2", "spec": { "source": "lan", "target": "t-lan" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");

    let agent = t.bridge_agent(m.id);
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let m = t.migration(m.id);
    assert_eq!(m.status.phase, MigrationPhase::Moved, "{:?}", m.status);
    let bridge = m.status.bridge.clone().expect("bridge laid");
    assert_eq!(bridge.node, t.env.node);
    assert_eq!(
        bridge.physnet,
        orbivisor_types::sdn::migration_physnet(TAG, m.id)
    );
    assert!(m.status.vms.iter().all(|v| v.state == MoveState::Moved));
    // a: 스펙만, 주소는 cloud-init의 것, cloud-init 항목은 빈 `{}`
    let va = t.vm_of(a);
    assert_eq!(va.spec.nics[0].network, "t-lan");
    assert_eq!(va.spec.nics[0].ips, [ip("10.66.0.11")]);
    assert_eq!(
        va.spec.cloud_init.as_ref().unwrap().networks[0],
        Default::default()
    );
    // b: tap만 옮김(도는 NIC 기록도), 장치 떼고 붙이기 없음
    let vb = t.vm_of(b);
    assert_eq!(vb.spec.nics[0].network, "t-lan");
    assert_eq!(vb.spec.nics[0].ips, [ip("10.66.0.12")]);
    assert_eq!(
        vb.status.hardware.as_ref().unwrap().nics[0].network,
        "t-lan"
    );
    assert_eq!(relinks(&t.env), [(0, true)]);
    assert!(!t.env.hv.hotplug_calls().iter().any(|c| matches!(
        c,
        HotplugCall::AddNic { .. } | HotplugCall::RemoveNic { .. }
    )));
    // 다리 동안 대상 네트워크를 라우터에 붙이지 못한다.
    let (status, body) = t
        .admin
        .post(
            "/routers",
            json!({ "name": "r1", "spec": { "interfaces": [{ "network": "t-lan" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "network_migrating", "{body}");

    // b 하나를 되돌린다: tap이 원본 브리지로, 스펙은 옮기기 전 그대로(주소를 놓음).
    let (state, error) = t.step(m.id, MigrationStep::Rollback, vec![b], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let m = t.migration(m.id);
    assert_eq!(by(&m, b).state, MoveState::RolledBack);
    assert_eq!(by(&m, a).state, MoveState::Moved);
    assert_eq!(m.status.phase, MigrationPhase::Moved);
    assert!(m.status.bridge.is_some());
    let vb = t.vm_of(b);
    assert_eq!(vb.spec.nics, b_before.nics);
    assert_eq!(vb.spec.cloud_init, b_before.cloud_init);
    assert_eq!(vb.status.hardware.as_ref().unwrap().nics[0].network, "lan");
    assert_eq!(relinks(&t.env), [(0, true), (0, false)]);

    // 끝내기: 다리를 걷고 라우터에 붙인다. 원본에 남은 b는 경고다.
    let (status, body) = t
        .admin
        .post("/routers", json!({ "name": "r1", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (state, warning) = t
        .step(m.id, MigrationStep::Finish, vec![], Some("r1"))
        .await;
    assert_eq!(state, TaskState::Succeeded, "{warning:?}");
    // 리뷰 2 m1: Task 글에는 VM id 없이 수만
    let warning = warning.unwrap();
    assert_eq!(warning, "1 NIC(s) are still on network \"lan\"");
    assert!(!warning.contains(&b.to_string()));
    let m = t.migration(m.id);
    assert_eq!(m.status.phase, MigrationPhase::Finished);
    assert_eq!(m.status.bridge, None);
    let (_, r1) = t.admin.get("/routers/r1").await;
    assert_eq!(
        r1["spec"]["interfaces"],
        json!([{ "network": "t-lan" }]),
        "{r1}"
    );
    // 끝낸 옮기기는 원본을 더 잡지 않는다(새 옮기기가 된다).
    let (status, m2) = t
        .admin
        .post(
            "/network-migrations?dry_run=true",
            json!({ "name": "m2", "spec": { "source": "lan", "target": "t-lan" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{m2}");
    assert!(m2["nics"].is_array(), "the dry run returns the plan: {m2}");
    let (status, m2) = t
        .admin
        .post(
            "/network-migrations",
            json!({ "name": "m2", "spec": { "source": "lan", "target": "t-lan" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{m2}");
    // 끝난 옮기기에 걸음은 409
    let (status, err) = t.post_step(m.id, "start", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["code"], "migration_finished", "{err}");
    agent.abort();
}

/// 라이브가 실패하면(포트가 켜지지 않음 따위) 그 VM만 실패이고 스펙은 옮기기 전 모습(cloud-init 항목 포함)이다. 되돌리기는
/// 스펙만 고르고 끝난다.
#[tokio::test]
async fn a_failed_live_move_keeps_the_vm_on_its_network() {
    let t = T::start().await;
    let b = t.vm("b", Some("10.66.0.12"), true).await;
    let before = t.vm_of(b).spec;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "allow_mtu_shrink": true, "bridge": false }))
        .await;
    assert_eq!(m.status.vms[0].mode, PlannedMode::Live);
    t.env.hv.fail_next_hotplug("ovn-installed did not come");
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    assert_eq!(state, TaskState::Failed);
    assert_eq!(
        error.as_deref(),
        Some("1 NIC(s) failed to move"),
        "task error"
    );
    let m = t.migration(m.id);
    assert_eq!(m.status.phase, MigrationPhase::Failed);
    assert!(
        m.status.vms[0]
            .error
            .as_deref()
            .unwrap()
            .contains("ovn-installed"),
        "{:?}",
        m.status.vms[0]
    );
    assert_eq!(m.status.vms[0].state, MoveState::Failed);
    assert_eq!(m.status.bridge, None);
    let vb = t.vm_of(b);
    assert_eq!(vb.spec.nics, before.nics);
    assert_eq!(vb.spec.cloud_init, before.cloud_init);
    assert_eq!(vb.status.hardware.as_ref().unwrap().nics[0].network, "lan");

    let (state, error) = t.step(m.id, MigrationStep::Rollback, vec![], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let m = t.migration(m.id);
    assert_eq!(m.status.phase, MigrationPhase::RolledBack);
    assert_eq!(m.status.vms[0].state, MoveState::RolledBack);
    assert_eq!(t.vm_of(b).spec, before);
}

/// MTU가 줄고 허용하지 않으면 켜진 VM은 다시 켜며 옮긴다(끄기 → 스펙 → 켜기). 되돌리기는 MTU가 늘므로 tap만 옮긴다.
#[tokio::test]
async fn restart_moves_stop_and_start_the_vm() {
    let t = T::start().await;
    let c = t.vm("c", Some("10.66.0.13"), true).await;
    let pid = t.env.pid(c).await.unwrap();
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "bridge": false }))
        .await;
    assert_eq!(m.status.vms[0].mode, PlannedMode::Restart);
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let vc = t.vm_of(c);
    assert_eq!(vc.spec.nics[0].network, "t-lan");
    assert_eq!(vc.spec.nics[0].ips, [ip("10.66.0.13")]);
    assert_eq!(vc.status.phase, orbivisor_types::vm::VmPhase::Running);
    assert_eq!(
        vc.status.hardware.as_ref().unwrap().nics[0].network,
        "t-lan"
    );
    assert_ne!(t.env.pid(c).await, Some(pid), "the VM was started again");
    assert!(relinks(&t.env).is_empty());
    assert_eq!(t.migration(m.id).status.phase, MigrationPhase::Moved);

    let (state, error) = t.step(m.id, MigrationStep::Rollback, vec![], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    assert_eq!(relinks(&t.env), [(0, false)]);
    let vc = t.vm_of(c);
    assert_eq!(vc.spec.nics[0].network, "lan");
    assert!(vc.spec.nics[0].ips.is_empty());
    assert_eq!(t.migration(m.id).status.phase, MigrationPhase::RolledBack);
}

/// 계획 뒤에 켠 VM은 다시 계획이 달라 그 VM만 실패한다(몰래 다시 켜지 않는다). 켜진 VM의 네트워크를 일반 `PUT`으로
/// 단순 ↔ SDN으로 바꾸면 409 `nic_network_live`.
#[tokio::test]
async fn a_changed_plan_fails_only_that_vm_and_put_cannot_relink() {
    let t = T::start().await;
    let a = t.vm("a", Some("10.66.0.11"), false).await;
    let d = t.vm("d", Some("10.66.0.14"), false).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "bridge": false }))
        .await;
    assert!(m.status.vms.iter().all(|v| v.mode == PlannedMode::Offline));
    let task = t.admin.act("d", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    assert_eq!(state, TaskState::Failed);
    assert!(!error.unwrap().contains(&d.to_string()));
    let m = t.migration(m.id);
    let rec = |vm: Uuid| m.status.vms.iter().find(|v| v.vm == vm).unwrap();
    assert!(rec(d).error.as_deref().unwrap().contains("plan changed"));
    assert_eq!(rec(a).state, MoveState::Moved);
    assert_eq!(rec(d).state, MoveState::Failed);
    assert_eq!(t.vm_of(d).spec.nics[0].network, "lan");

    let vd = t.vm_of(d);
    let mut spec = serde_json::to_value(&vd.spec).unwrap();
    spec["nics"][0]["network"] = json!("t-lan");
    spec["cloud_init"]["networks"] = json!([{}]);
    let (status, body) = t.admin.put("/vms/d", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "nic_network_live", "{body}");
}

/// opld가 옮기기 Task 도중 다시 떠도(실행 중으로 남은 Task) 새 실행기가 이어서 한다: 이미 옮긴(`moving`인 채 스펙을 쓴) VM은
/// 다시 쓰지 않고 끝낸다.
#[tokio::test]
async fn an_interrupted_migration_task_resumes() {
    let mut t = T::start().await;
    let a = t.vm("a", Some("10.66.0.11"), false).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "bridge": false }))
        .await;
    // 앞선 Task가 스펙을 쓰고 기록을 적기 전에 멈춘 것처럼
    let va = t.vm_of(a);
    let mut spec = va.spec.clone();
    orbivisor_types::network_migration::move_nic(&mut spec, 0, "t-lan", &[ip("10.66.0.11")]);
    t.env
        .cluster
        .update_spec(a, Some(va.revision), va.labels.clone(), &spec)
        .await
        .unwrap();
    let mut status = m.status.clone();
    status.vms[0].state = MoveState::Moving;
    status.phase = MigrationPhase::Running;
    t.env
        .cluster
        .update_status::<NetworkMigrationSpec>(m.id, &status)
        .await
        .unwrap();
    t.env.stop_runner().await;
    let (status, task) = t.post_step(m.id, "start", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = t
        .env
        .cluster
        .get::<TaskSpec>(task["id"].as_str().unwrap().parse().unwrap())
        .unwrap()
        .unwrap();
    // 앞선 opld가 시작한 Task(실행 중으로 남음)
    let mut ts = task.status.clone();
    ts.state = TaskState::Running;
    t.env
        .cluster
        .update_status::<TaskSpec>(task.id, &ts)
        .await
        .unwrap();
    let revision = t.vm_of(a).revision;
    let host = ovn_host(&t.env.dir.path().join("ovn2"));
    Box::pin(
        t.env
            .start_runner_config(Arc::new(FakeStorage::default()), move |c| {
                c.sdn = Some(host);
            }),
    )
    .await;
    let (state, error) = t.wait_task(task.id).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let m = t.migration(m.id);
    assert_eq!(m.status.vms[0].state, MoveState::Moved);
    assert_eq!(m.status.vms[0].ips, [ip("10.66.0.11")]);
    assert_eq!(m.status.phase, MigrationPhase::Moved);
    assert_eq!(
        t.vm_of(a).revision,
        revision,
        "the VM was not written again"
    );
}

/// 도는 Task가 있으면 다른 걸음은 409, 도는 옮기기가 아니면 멈춤도 409.
#[tokio::test]
async fn steps_do_not_overlap() {
    let mut t = T::start().await;
    t.vm("a", Some("10.66.0.11"), false).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "bridge": false }))
        .await;
    let (status, err) = t.post_step(m.id, "pause", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["code"], "migration_not_running", "{err}");
    t.env.stop_runner().await;
    let (status, task) = t.post_step(m.id, "start", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    for (step, body) in [
        ("finish", json!({})),
        ("rollback", json!({})),
        ("resume", json!({})),
    ] {
        let (status, err) = t.post_step(m.id, step, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{step}");
        assert_eq!(err["code"], "migration_in_progress", "{step}: {err}");
    }
    // 도는 옮기기는 지우지 못한다(먼저 멈춤).
    let (status, err) = t.admin.delete("/network-migrations/m").await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    // 옮기기에 없는 VM의 되돌리기는 400
    let (status, err) = t
        .post_step(m.id, "rollback", json!({ "vms": ["nope"] }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
}

/// 클러스터에서 지운 노드에 넣은 노드(오프라인) — 다리 노드 지우기 시험용
async fn add_dead_node(env: &Env, name: &str) -> Uuid {
    let mut r: Resource<NodeSpec, orbivisor_types::node::NodeStatus> = Resource::new(
        name,
        NodeSpec {
            sdn: None,
            host_resources: None,
            address: "127.0.0.9:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    r.status.version = Some(orbivisor_types::version::BUILD.into());
    r.status.features = orbivisor_types::node::FEATURES
        .iter()
        .map(|f| f.to_string())
        .collect();
    env.cluster.create(&r).await.unwrap();
    r.id
}

async fn set_online(env: &Env, node: Uuid, online: bool) {
    let mut n = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    n.status.online = online;
    env.cluster
        .update_status::<NodeSpec>(node, &n.status)
        .await
        .unwrap();
}

/// 계획 67 §13: 다리 노드를 클러스터에서 지우면 옮기기는 `paused`(까닭)가 되고 다리 기록이 지워진다. `resume`에
/// `bridge_node`를 주면 그 노드로 다리를 다시 놓고 이어 간다. 살아 있는 다른 노드에 다리가 놓였으면 바꾸지 못한다.
#[tokio::test]
async fn a_removed_bridge_node_pauses_the_migration() {
    let t = T::start().await;
    t.vm("a", Some("10.66.0.11"), false).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan" }))
        .await;
    let agent = t.bridge_agent(m.id);
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let laid = t.migration(m.id).status.bridge.unwrap();
    assert_eq!(laid.node, t.env.node);

    // 살아 있는 다른 노드에 다리가 있으면 다리 노드를 바꾸지 못한다.
    let other = add_dead_node(&t.env, "other").await;
    set_online(&t.env, other, true).await;
    let mut status = t.migration(m.id).status;
    status.bridge.as_mut().unwrap().node = other;
    t.env
        .cluster
        .update_status::<NetworkMigrationSpec>(m.id, &status)
        .await
        .unwrap();
    let node = t.env.node.to_string();
    let (status, err) = t
        .post_step(m.id, "resume", json!({ "bridge_node": node }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "migration_bridge_laid", "{err}");
    set_online(&t.env, other, false).await;

    // 그 노드를 지우면 옮기기가 멈추고 다리 기록이 지워진다.
    let (status, body) = t.admin.delete("/nodes/other").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["warnings"][0]
            .as_str()
            .unwrap()
            .contains("network migration \"m\" paused"),
        "{body}"
    );
    let paused = t.migration(m.id);
    assert_eq!(paused.status.phase, MigrationPhase::Paused);
    assert!(paused.status.bridge.is_none());
    assert!(paused.status.error.unwrap().contains("bridge node"));

    // 다른 다리 노드로 이어 간다.
    let (status, task) = t
        .post_step(m.id, "resume", json!({ "bridge_node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (state, error) = t
        .wait_task(task["id"].as_str().unwrap().parse().unwrap())
        .await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let resumed = t.migration(m.id);
    assert_eq!(resumed.spec.bridge_node, Some(t.env.node));
    assert_eq!(resumed.status.bridge.unwrap().node, t.env.node);
    assert_eq!(resumed.status.phase, MigrationPhase::Moved);
    let (_, audit) = t.admin.get("/audit?limit=5").await;
    assert!(
        audit
            .to_string()
            .contains("network migrate lan resume --bridge-node node1"),
        "{audit}"
    );
    agent.abort();
}

/// 리뷰 1 M5: 다리 노드가 오프라인이면(지우지 않았어도) `resume --bridge-node`로 다른 노드에 다리를 다시 놓는다.
#[tokio::test]
async fn an_offline_bridge_node_can_be_replaced() {
    let t = T::start().await;
    t.vm("a", Some("10.66.0.11"), false).await;
    t.vm("b", Some("10.66.0.12"), false).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "vms": [] }))
        .await;
    let agent = t.bridge_agent(m.id);
    // 다리가 오프라인 노드에 놓였고 옮기기가 멈춘 모습
    let other = add_dead_node(&t.env, "other").await;
    let mut status = t.migration(m.id).status;
    status.bridge = Some(orbivisor_types::network_migration::MigrationBridge {
        node: other,
        physnet: "mig00000000".into(),
    });
    status.phase = MigrationPhase::Paused;
    t.env
        .cluster
        .update_status::<NetworkMigrationSpec>(m.id, &status)
        .await
        .unwrap();
    let node = t.env.node.to_string();
    let (status, task) = t
        .post_step(m.id, "resume", json!({ "bridge_node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (state, error) = t
        .wait_task(task["id"].as_str().unwrap().parse().unwrap())
        .await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let resumed = t.migration(m.id);
    assert_eq!(resumed.status.bridge.unwrap().node, t.env.node);
    assert_eq!(resumed.status.phase, MigrationPhase::Moved);
    agent.abort();
}

/// 리뷰 1 I1·웹 I2: 한 대상 네트워크에 다리를 놓을 수 있는 옮기기는 하나(다리 없이는 여럿), 이름을 주지 않으면
/// `<원본>-to-<대상>`이고 쓰였으면 `-2`다.
#[tokio::test]
async fn one_bridge_per_target_and_free_names() {
    let t = T::start().await;
    let n = Resource::new(
        "lan2",
        serde_json::from_value::<NetworkSpec>(json!({ "bridge": "br0", "vlan": 20 })).unwrap(),
    );
    t.env.cluster.create(&n).await.unwrap();
    t.vm("a", Some("10.66.0.11"), false).await;
    let mut req = vm_request("b");
    req["spec"]["nics"] = json!([{ "network": "lan2", "model": "virtio" }]);
    let (status, body) = t.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 이름 `lan-to-t-lan`을 다른 원본(다리 없음)이 먼저 쥔다.
    let (status, first) = t
        .admin
        .post(
            "/network-migrations",
            json!({ "name": "lan-to-t-lan", "spec": { "source": "lan2", "target": "t-lan", "bridge": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, second) = t
        .admin
        .post(
            "/network-migrations",
            json!({ "spec": { "source": "lan", "target": "t-lan" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    assert_eq!(second["name"], "lan-to-t-lan-2", "{second}");
    // 다리를 놓는 옮기기가 같은 대상에 또 있으면 409(첫째를 지우고 다리를 놓는 것으로 다시)
    let (status, _) = t.admin.delete("/network-migrations/lan-to-t-lan").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, err) = t
        .admin
        .post(
            "/network-migrations",
            json!({ "spec": { "source": "lan2", "target": "t-lan" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "migration_overlap", "{err}");
}

/// 리뷰 1 I2: 끝나지 않은 옮기기가 계획한 주소(원본의 VM이 아직 쓴다)와 원본 VM의 cloud-init 고정 주소는 대상에서 쓴
/// 주소다 — 새 VM이 고를 수 없다. 옮기는 VM 자신은 그 주소로 옮겨 간다.
#[tokio::test]
async fn planned_addresses_are_held_on_the_target() {
    let t = T::start().await;
    t.vm("a", Some("10.66.0.2"), false).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "bridge": false }))
        .await;
    // 준 주소로 새 VM을 만들 수 없고, 주소 없이 만들면 다른 주소를 받는다.
    let mut req = vm_request("x");
    req["spec"]["nics"] = json!([{ "network": "t-lan", "model": "virtio", "ips": ["10.66.0.2"] }]);
    let (status, err) = t.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "ip_in_use", "{err}");
    let mut req = vm_request("y");
    req["spec"]["nics"] = json!([{ "network": "t-lan", "model": "virtio" }]);
    let (status, y) = t.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{y}");
    assert_ne!(y["spec"]["nics"][0]["ips"][0], "10.66.0.2", "{y}");
    // 옮기는 VM은 그 주소로 간다.
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let moved = t.migration(m.id);
    assert_eq!(moved.status.vms[0].ips, [ip("10.66.0.2")]);
}

/// 리뷰 1 M1: 원본에 NIC 둘인 VM은 둘 다 옮긴다(첫 NIC를 옮긴 뒤 다시 계획이 "다른 NIC가 대상에 있음"으로 막지 않는다).
#[tokio::test]
async fn a_vm_with_two_source_nics_moves_both() {
    let t = T::start().await;
    let mut req = vm_request("two");
    req["spec"]["nics"] = json!([
        { "network": "lan", "model": "virtio" },
        { "network": "lan", "model": "virtio" }
    ]);
    req["spec"]["cloud_init"] = json!({ "networks": [
        { "ipv4": { "mode": "static", "address": "10.66.0.21/24" } },
        { "ipv4": { "mode": "static", "address": "10.66.0.22/24" } }
    ] });
    let (status, vm) = t.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "bridge": false }))
        .await;
    assert_eq!(m.status.vms.len(), 2);
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let m = t.migration(m.id);
    assert!(
        m.status.vms.iter().all(|v| v.state == MoveState::Moved),
        "{:?}",
        m.status.vms
    );
    let v = t.vm_of(id);
    assert!(v.spec.nics.iter().all(|n| n.network == "t-lan"));
}

/// 리뷰 1 I3: 읽을 수 없는 VM은 계획에 없고(수만), 만든 옮기기의 상태에도 적히지 않으며, 다른 사람이 만든 옮기기를 읽어도
/// 그 VM의 기록은 빠진다. 스펙에 그 VM을 주면 없는 VM과 같다.
#[tokio::test]
async fn unreadable_vms_are_left_out_of_plans_and_records() {
    let t = T::start().await;
    let (status, f) = t
        .admin
        .post("/folders", json!({ "name": "nf", "kind": "network" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{f}");
    let folder: Uuid = f["id"].as_str().unwrap().parse().unwrap();
    for (name, spec) in [
        ("lan3", json!({ "bridge": "br0", "folder": folder })),
        (
            "t3",
            json!({ "sdn": {}, "folder": folder, "subnets": [{ "cidr": "10.67.0.0/24" }] }),
        ),
    ] {
        let n = Resource::new(name, serde_json::from_value::<NetworkSpec>(spec).unwrap());
        t.env.cluster.create(&n).await.unwrap();
    }
    let mut ids = Vec::new();
    for name in ["mine", "theirs"] {
        let mut req = vm_request(name);
        req["spec"]["nics"] = json!([{ "network": "lan3", "model": "virtio" }]);
        let (status, vm) = t.admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{vm}");
        ids.push(vm["id"].as_str().unwrap().to_string());
    }
    let (_, user) = t
        .admin
        .post(
            "/users",
            json!({ "name": "dave", "password": "password-1", "role": "none" }),
        )
        .await;
    for (role, privileges, target) in [
        (
            "netop",
            json!(["network.config", "network.assign", "system.read"]),
            json!({ "type": "folder", "id": folder }),
        ),
        (
            "vmop",
            json!(["vm.config", "system.read"]),
            json!({ "type": "object", "kind": "vm", "id": ids[0] }),
        ),
    ] {
        let (status, r) = t
            .admin
            .post(
                "/access-roles",
                json!({ "name": role, "privileges": privileges }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{r}");
        let (status, body) = t
            .admin
            .post(
                "/grants",
                json!({ "principal": { "type": "user", "id": user["id"] },
                    "role": { "type": "custom", "id": r["id"] }, "target": target }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let dave = t.env.login("dave", "password-1").await;
    let spec = json!({ "source": "lan3", "target": "t3", "bridge": false });
    let (status, plan) = dave
        .post("/network-migrations?dry_run=true", json!({ "spec": spec }))
        .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["nics"].as_array().unwrap().len(), 1, "{plan}");
    assert_eq!(plan["nics"][0]["name"], "mine");
    assert_eq!(plan["hidden"], 1, "{plan}");
    assert!(!plan.to_string().contains(&ids[1]), "{plan}");
    // 읽을 수 없는 VM을 주면 없는 VM과 같다.
    let (status, err) = dave
        .post(
            "/network-migrations?dry_run=true",
            json!({ "spec": { "source": "lan3", "target": "t3", "vms": [ids[1]] } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    let (status, created) = dave
        .post("/network-migrations", json!({ "spec": spec }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"]["vms"].as_array().unwrap().len(), 1);
    let (status, _) = t.admin.delete("/network-migrations/lan3-to-t3").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // 관리자가 만든 옮기기를 읽으면 그 VM의 기록이 빠진다(watch 이벤트도).
    let since = dave.get("/network-migrations").await.1["revision"]
        .as_u64()
        .unwrap();
    let mut ws = watch(
        &dave,
        &format!("/watch?kinds=network-migration&since={since}"),
    )
    .await;
    let (status, created) = t
        .admin
        .post("/network-migrations", json!({ "spec": spec }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"]["vms"].as_array().unwrap().len(), 2);
    let (status, seen) = dave.get("/network-migrations/lan3-to-t3").await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert_eq!(seen["status"]["vms"].as_array().unwrap().len(), 1, "{seen}");
    assert!(!seen.to_string().contains(&ids[1]), "{seen}");
    let (_, list) = dave.get("/network-migrations").await;
    assert!(!list.to_string().contains(&ids[1]), "{list}");
    let ev = loop {
        let ev = next_event(&mut ws).await;
        if ev["kind"] == "network-migration" && ev["resource"]["name"] == "lan3-to-t3" {
            break ev;
        }
    };
    assert_eq!(
        ev["resource"]["status"]["vms"].as_array().unwrap().len(),
        1,
        "{ev}"
    );
    assert!(!ev.to_string().contains(&ids[1]), "{ev}");
}

/// 리뷰 2 m2·m3: 이름을 고르기 전에 네트워크를 본다(ASCII가 아닌 긴 원본 이름이 처리기를 멈추게 하지 않음 — 400).
/// 공급자 대상에는 다리가 없으니 `bridge`를 거짓으로 적어, 같은 공급자 네트워크로 옮기는 둘째도 만들어진다.
#[tokio::test]
async fn provider_targets_take_no_bridge_key_and_odd_names_are_400() {
    let t = T::start().await;
    let (status, err) = t
        .admin
        .post(
            "/network-migrations",
            json!({ "spec": { "source": "é".repeat(40), "target": "t-lan" } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert_eq!(err["code"], "invalid_migration", "{err}");
    for (name, spec) in [
        ("lan2", json!({ "bridge": "br0", "vlan": 20 })),
        (
            "prov",
            json!({ "sdn": { "provider": { "physnet": "ext" } }, "subnets": [{ "cidr": "10.66.0.0/24" }] }),
        ),
    ] {
        let n = Resource::new(name, serde_json::from_value::<NetworkSpec>(spec).unwrap());
        t.env.cluster.create(&n).await.unwrap();
    }
    for source in ["lan", "lan2"] {
        let (status, created) = t
            .admin
            .post(
                "/network-migrations",
                json!({ "spec": { "source": source, "target": "prov" } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["spec"]["bridge"], false, "{created}");
    }
}

/// VM마다 정한 게스트 에이전트 정보
#[derive(Default)]
struct Guests(
    std::sync::Mutex<std::collections::BTreeMap<Uuid, orbivisor_types::guest::GuestInfo>>,
);

#[async_trait::async_trait]
impl orbivisor_peer::GuestAgentSource for Guests {
    async fn info(&self, vm: Uuid, _refresh: bool) -> orbivisor_types::guest::GuestInfoResponse {
        let info = self.0.lock().unwrap().get(&vm).cloned();
        orbivisor_types::guest::GuestInfoResponse {
            state: if info.is_some() {
                orbivisor_types::guest::GuestInfoState::Answered
            } else {
                orbivisor_types::guest::GuestInfoState::NoAgent
            },
            info,
        }
    }

    async fn set_password(
        &self,
        _vm: Uuid,
        _user: &str,
        _password: &str,
    ) -> Result<(), orbivisor_peer::GuestAgentError> {
        Ok(())
    }
}

/// 리뷰 2 I4: 계획에 없이 원본에 남는 켜진 VM(읽을 수 없거나 뺀 VM)이 게스트 에이전트로 알린 주소도 대상에 잡아 둔다 —
/// 서버가 묻고 VM id 없이 `status.held_ips`에 적으며, API 보기(만들기 응답·GET)에는 내지 않는다.
#[tokio::test]
async fn addresses_of_vms_left_on_the_source_are_held() {
    let guests = Arc::new(Guests::default());
    let t = T::start_guest(guests.clone()).await;
    let mine = t.vm("mine", None, false).await;
    let mut req = vm_request("theirs");
    req["spec"]["nics"] = json!([{ "network": "lan", "model": "virtio" }]);
    req["spec"]["guest_agent"] = json!({});
    let (status, vm) = t.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let theirs: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let task = t.admin.act("theirs", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let mac = t.vm_of(theirs).spec.nics[0].mac.unwrap().to_string();
    guests.0.lock().unwrap().insert(
        theirs,
        serde_json::from_value(json!({
            "collected_at": 1,
            "interfaces": [{ "name": "eth0", "mac": mac,
                "addresses": [{ "ip": "10.66.0.9", "prefix": 24 }] }]
        }))
        .unwrap(),
    );
    let (status, created) = t
        .admin
        .post(
            "/network-migrations",
            json!({ "spec": { "source": "lan", "target": "t-lan", "bridge": false, "vms": [mine] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(t.migration(id).status.held_ips, [ip("10.66.0.9")]);
    assert!(!created.to_string().contains("10.66.0.9"), "{created}");
    let (_, seen) = t.admin.get(&format!("/network-migrations/{id}")).await;
    assert!(!seen.to_string().contains("10.66.0.9"), "{seen}");
    // 대상에서 그 주소를 고를 수 없다.
    let mut req = vm_request("x");
    req["spec"]["nics"] = json!([{ "network": "t-lan", "model": "virtio", "ips": ["10.66.0.9"] }]);
    let (status, err) = t.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["code"], "ip_in_use", "{err}");
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn watch(client: &common::Client, path: &str) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let url = format!("{}/api/v1{path}", client.base.replace("http://", "ws://"));
    let mut req = url.into_client_request().unwrap();
    let token = client.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_event(ws: &mut Socket) -> Value {
    use futures_util::StreamExt;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

/// 리뷰 1 M2: 우리 스펙 쓰기 전에 만든 핫플러그 Task가 tap을 옮기지 않고 끝나면 새 Task로 한 번 더 한다(스펙만 대상에 남은 채
/// 원본에서 돌지 않게).
#[tokio::test]
async fn a_stale_hotplug_task_does_not_leave_the_spec_moved() {
    let t = T::start().await;
    let b = t.vm("b", Some("10.66.0.12"), true).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "allow_mtu_shrink": true, "bridge": false }))
        .await;
    assert_eq!(m.status.vms[0].mode, PlannedMode::Live);
    // 다른 노드(없는 노드)에 걸린, 옮기기 전에 만든 핫플러그 Task
    let mut stale: Resource<TaskSpec, orbivisor_types::task::TaskStatus> = Resource::new(
        "",
        TaskSpec {
            op: orbivisor_types::task::TaskOp::VmHotplug,
            target: b,
            node: Uuid::now_v7(),
        },
    );
    stale.name = stale.id.to_string();
    t.env.cluster.create(&stale).await.unwrap();
    let cluster = t.env.cluster.clone();
    let stale_id = stale.id;
    let finisher = tokio::spawn(async move {
        // 옮기기가 스펙을 쓰면 옛 Task가 (tap을 옮기지 않고) 끝난다.
        loop {
            let vm = cluster.get::<VmSpec>(b).unwrap().unwrap();
            if vm.spec.nics[0].network == "t-lan" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut task = cluster.get::<TaskSpec>(stale_id).unwrap().unwrap();
        task.status.state = TaskState::Succeeded;
        cluster
            .update_status::<TaskSpec>(stale_id, &task.status)
            .await
            .unwrap();
    });
    let (state, error) = t.step(m.id, MigrationStep::Start, vec![], None).await;
    finisher.await.unwrap();
    assert_eq!(state, TaskState::Succeeded, "{error:?}");
    let vb = t.vm_of(b);
    assert_eq!(vb.spec.nics[0].network, "t-lan");
    assert_eq!(
        vb.status.hardware.as_ref().unwrap().nics[0].network,
        "t-lan"
    );
    assert_eq!(relinks(&t.env), [(0, true)]);
}

/// 리뷰 1 M3: 다시 켜기에서 새 네트워크로 켜지지 못한 채 시간이 다 되면 끄고 원래 네트워크로 되돌려 켠다.
#[tokio::test]
async fn a_restart_that_times_out_goes_back() {
    let t = T::start_with(|c| c.migration_vm_wait = Duration::from_secs(3)).await;
    let c = t.vm("c", Some("10.66.0.13"), true).await;
    let m = t
        .create(json!({ "source": "lan", "target": "t-lan", "bridge": false }))
        .await;
    assert_eq!(m.status.vms[0].mode, PlannedMode::Restart);
    t.env.hv.hold_next_start();
    let (status, task) = t.post_step(m.id, "start", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    // 새 네트워크로 켜기가 멈춰 있다. 옮기기가 시간이 다 되어 끄라고 하면 놓아준다.
    t.env.hv.start_held().await;
    let deadline = tokio::time::Instant::now() + LIMIT;
    loop {
        let vm = t.vm_of(c);
        if vm.spec.power == orbivisor_types::vm::PowerState::Stopped {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "not stopped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    t.env.hv.release_start();
    let (state, error) = t
        .wait_task(task["id"].as_str().unwrap().parse().unwrap())
        .await;
    assert_eq!(state, TaskState::Failed, "{error:?}");
    let error = t.migration(m.id).status.vms[0].error.clone().unwrap();
    assert!(error.contains("in time and was started on"), "{error}");
    let vc = t.vm_of(c);
    assert_eq!(vc.spec.nics[0].network, "lan");
    assert_eq!(vc.spec.power, orbivisor_types::vm::PowerState::Running);
}
