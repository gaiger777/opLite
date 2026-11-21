//! 단순 → SDN 옮기기 엔진(계획 67 §9, 스테이지 4): 계획을 적은 리소스, `network-migrate` Task의 VM마다 방식(끈 채·라이브
//! `RelinkNic`·다시 켜기), 주소 옮기기(준 주소·cloud-init 고정 주소), 다리 놓기·걷기, VM 하나 되돌리기, 끝내기(라우터
//! 붙이기), 라이브 실패의 되돌림, 다시 계획이 달라진 VM, opld가 다시 떠도 이어서 하기, 일반 `PUT`의 `nic_network_live`와
//! 다리 동안 라우터 붙이기의 `network_migrating`. Mock 하이퍼바이저와 가짜 OVN 실행기(태그만 쓴다)로 돈다.

mod common;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, vm_request};
use opl_driver::HotplugCall;
use opl_driver::ovn::{FakeRunner, OvnHost, OvnOptions, OvnPaths};
use opl_types::network::NetworkSpec;
use opl_types::network_migration::{
    AddressSource, MigrationPhase, MigrationStep, MoveState, NetworkMigration,
    NetworkMigrationSpec, PlannedMode,
};
use opl_types::node::{HOTPLUG, NodeHotplug, NodeSpec};
use opl_types::resource::Resource;
use opl_types::sdn::{ChassisState, NodeSdn, PhysnetState};
use opl_types::task::{TaskSpec, TaskState};
use opl_types::vm::{Vm, VmSpec};
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
        let mut env = Env::start().await;
        let tmp = env.dir.path().join("ovn");
        env.stop_runner().await;
        let host = ovn_host(&tmp);
        Box::pin(
            env.start_runner_config(Arc::new(FakeStorage::default()), move |c| {
                c.sdn = Some(host);
            }),
        )
        .await;
        let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
        me.status.hotplug = Some(NodeHotplug::default());
        me.status.bridges = vec![opl_types::node::BridgeInfo {
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
        let mut sdn: opl_types::sdn::Sdn =
            Resource::new(opl_types::sdn::SDN_NAME, opl_types::sdn::SdnSpec::default());
        sdn.status.phase = opl_types::sdn::SdnPhase::Ready;
        sdn.status.centrals = vec![opl_types::sdn::SdnCentral {
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
    assert_eq!(bridge.physnet, opl_types::sdn::migration_physnet(TAG, m.id));
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
    assert!(warning.unwrap().contains(&b.to_string()));
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
    assert!(error.unwrap().contains("ovn-installed"), "task error");
    let m = t.migration(m.id);
    assert_eq!(m.status.phase, MigrationPhase::Failed);
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
    assert_eq!(vc.status.phase, opl_types::vm::VmPhase::Running);
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
    assert!(error.unwrap().contains("plan changed"));
    let m = t.migration(m.id);
    let rec = |vm: Uuid| m.status.vms.iter().find(|v| v.vm == vm).unwrap();
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
    opl_types::network_migration::move_nic(&mut spec, 0, "t-lan", &[ip("10.66.0.11")]);
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
    let mut r: Resource<NodeSpec, opl_types::node::NodeStatus> = Resource::new(
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
    r.status.version = Some(opl_types::version::BUILD.into());
    r.status.features = opl_types::node::FEATURES
        .iter()
        .map(|f| f.to_string())
        .collect();
    env.cluster.create(&r).await.unwrap();
    r.id
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
