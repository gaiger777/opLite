use serde_json::json;

use super::*;
use crate::backup::{BackupOrigin, BackupSpec, BackupStatus};
use crate::vm::VmSpec;

const CA: &str = "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const OTHER_CA: &str = "sha256:ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";

fn vm_spec(networks: &[&str]) -> VmSpec {
    let nics: Vec<_> = networks
        .iter()
        .map(|n| json!({ "network": n, "model": "virtio" }))
        .collect();
    serde_json::from_value(json!({
        "cpu": { "cores": 1 },
        "memory_mib": 512,
        "machine": "virt",
        "disks": [ { "volume": "root", "bus": "virtio-blk" } ],
        "nics": nics
    }))
    .unwrap()
}

/// DR 쪽 사본 하나
fn copy(name: &str, vm: &str, ca: &str, created_at: u64) -> Backup {
    let mut b = Resource::new(
        name,
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: vm.into(),
            pool: "dr-backups".into(),
            store: None,
            source: None,
            vm_spec: vm_spec(&["lan"]),
            job: None,
            origin: Some(BackupOrigin {
                cluster: "prod".into(),
                ca_fingerprint: ca.into(),
                backup: Uuid::now_v7(),
                job: None,
            }),
            incremental: false,
            max_chain: None,
        },
    );
    b.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(created_at),
        ..Default::default()
    };
    b
}

fn group(name: &str, vms: &[&str]) -> DrGroup {
    DrGroup {
        name: name.into(),
        wait: DrWait::Running,
        timeout_secs: DEFAULT_TIMEOUT_SECS,
        delay_secs: 0,
        vms: vms
            .iter()
            .map(|v| DrVm {
                vm: (*v).into(),
                pool: None,
                node: None,
                live: false,
                ip: Vec::new(),
                source_backup_pool: None,
            })
            .collect(),
    }
}

fn plan(groups: Vec<DrGroup>) -> RecoveryPlanSpec {
    RecoveryPlanSpec {
        source: "prod".into(),
        rpo_target_secs: DEFAULT_RPO_SECS,
        pool: "local".into(),
        folder: None,
        networks: BTreeMap::new(),
        security_groups: BTreeMap::new(),
        test_networks: BTreeMap::new(),
        source_backup_pool: None,
        on_timeout: OnTimeout::Continue,
        groups,
    }
}

fn run_spec(kind: DrRunKind, plan_spec: RecoveryPlanSpec) -> DrRunSpec {
    DrRunSpec {
        plan: Uuid::now_v7(),
        plan_name: "web-tier".into(),
        kind,
        plan_spec,
        max_parallel: DEFAULT_MAX_PARALLEL,
        cleanup: None,
        allow_missing: false,
        point_in_time: None,
    }
}

// ---- 계획 검사 ----

#[test]
fn plan_round_trips_with_defaults() {
    let p: RecoveryPlanSpec = serde_json::from_value(json!({
        "source": "prod",
        "pool": "local",
        "groups": [ { "name": "db", "vms": [ { "vm": "db1" } ] } ]
    }))
    .unwrap();
    assert_eq!(p.rpo_target_secs, DEFAULT_RPO_SECS);
    assert_eq!(p.on_timeout, OnTimeout::Continue);
    assert_eq!(p.groups[0].wait, DrWait::Running);
    assert_eq!(p.groups[0].timeout_secs, 600);
    assert_eq!(p.groups[0].delay_secs, 0);
    assert_eq!(p.validate(), Ok(()));
    let out = serde_json::to_value(&p).unwrap();
    assert_eq!(
        out,
        json!({
            "source": "prod",
            "rpo_target_secs": 86400,
            "pool": "local",
            "groups": [ { "name": "db", "wait": "running", "timeout_secs": 600, "delay_secs": 0,
                          "vms": [ { "vm": "db1" } ] } ]
        })
    );
    assert_eq!(serde_json::from_value::<RecoveryPlanSpec>(out).unwrap(), p);

    let full = json!({
        "source": "prod", "rpo_target_secs": 3600, "pool": "local", "folder": "/dr/prod",
        "networks": { "lan": "dr-lan" }, "security_groups": { "web": "dr-web" },
        "test_networks": { "lan": "lab" }, "on_timeout": "stop",
        "groups": [ { "name": "web", "wait": "ip", "timeout_secs": 120, "delay_secs": 30, "vms": [
            { "vm": "web1", "pool": "fast", "node": "n2", "live": true,
              "ip": [ { "nic": 0, "address": "10.20.0.5/24", "gateway": "10.20.0.1", "dns": ["10.20.0.2"] },
                      { "nic": 1, "dhcp": true } ] } ] } ]
    });
    let p: RecoveryPlanSpec = serde_json::from_value(full.clone()).unwrap();
    assert_eq!(p.validate(), Ok(()));
    assert_eq!(serde_json::to_value(&p).unwrap(), full);
    // 모르는 칸은 받지 않는다.
    let mut bad = full.clone();
    bad["extra"] = json!(1);
    assert!(serde_json::from_value::<RecoveryPlanSpec>(bad).is_err());
    let mut bad = full;
    bad["groups"][0]["vms"][0]["ip"][0]["mask"] = json!(24);
    assert!(serde_json::from_value::<RecoveryPlanSpec>(bad).is_err());
}

#[test]
fn plan_validation_errors() {
    let ok = plan(vec![group("db", &["db1"]), group("web", &["web1", "web2"])]);
    assert_eq!(ok.validate(), Ok(()));
    let check = |f: &dyn Fn(&mut RecoveryPlanSpec)| {
        let mut p = ok.clone();
        f(&mut p);
        p.validate()
    };
    assert!(matches!(
        check(&|p| p.source = "Prod".into()),
        Err(DrPlanError::Name { field, .. }) if field == "source"
    ));
    assert!(matches!(
        check(&|p| p.pool = String::new()),
        Err(DrPlanError::Name { field, .. }) if field == "pool"
    ));
    assert!(matches!(
        check(&|p| { p.networks.insert("lan".into(), "Bad Net".into()); }),
        Err(DrPlanError::Name { field, .. }) if field == "networks"
    ));
    assert!(matches!(
        check(&|p| { p.security_groups.insert("-x".into(), "y".into()); }),
        Err(DrPlanError::Name { field, .. }) if field == "security_groups"
    ));
    assert!(matches!(
        check(&|p| { p.test_networks.insert("lan".into(), "".into()); }),
        Err(DrPlanError::Name { field, .. }) if field == "test_networks"
    ));
    assert_eq!(
        check(&|p| p.folder = Some("a//b".into())),
        Err(DrPlanError::Folder("a//b".into()))
    );
    assert_eq!(
        check(&|p| p.folder = Some(Uuid::now_v7().to_string())),
        Ok(())
    );
    assert_eq!(check(&|p| p.folder = Some("/dr/prod".into())), Ok(()));
    assert!(matches!(
        check(&|p| p.rpo_target_secs = MAX_RPO_SECS + 1),
        Err(DrPlanError::Range { field, .. }) if field == "rpo_target_secs"
    ));
    assert_eq!(check(&|p| p.rpo_target_secs = 0), Ok(()));
    assert_eq!(check(&|p| p.groups.clear()), Err(DrPlanError::Groups(0)));
    assert_eq!(
        check(&|p| {
            p.groups = (0..33)
                .map(|i| group(&format!("g{i}"), &[&format!("v{i}")]))
                .collect()
        }),
        Err(DrPlanError::Groups(33))
    );
    assert_eq!(
        check(&|p| {
            let names: Vec<String> = (0..501).map(|i| format!("v{i}")).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            p.groups = vec![group("all", &refs)];
        }),
        Err(DrPlanError::Vms(501))
    );
    assert_eq!(
        check(&|p| p.groups[1].vms.clear()),
        Err(DrPlanError::EmptyGroup("web".into()))
    );
    assert_eq!(
        check(&|p| p.groups[1].name = "db".into()),
        Err(DrPlanError::DuplicateGroup("db".into()))
    );
    assert_eq!(
        check(&|p| p.groups[1].vms[0].vm = "db1".into()),
        Err(DrPlanError::DuplicateVm("db1".into()))
    );
    assert!(matches!(
        check(&|p| p.groups[0].name = "DB".into()),
        Err(DrPlanError::Name { field, .. }) if field == "groups[0].name"
    ));
    assert!(matches!(
        check(&|p| p.groups[1].vms[1].vm = "web_2".into()),
        Err(DrPlanError::Name { field, .. }) if field == "groups[1].vms[1].vm"
    ));
    assert!(matches!(
        check(&|p| p.groups[0].vms[0].pool = Some("x y".into())),
        Err(DrPlanError::Name { field, .. }) if field == "groups[0].vms[0].pool"
    ));
    assert!(matches!(
        check(&|p| p.groups[0].vms[0].node = Some("N1".into())),
        Err(DrPlanError::Name { field, .. }) if field == "groups[0].vms[0].node"
    ));
    for (t, ok) in [(9, false), (10, true), (86_400, true), (86_401, false)] {
        let r = check(&|p| p.groups[0].timeout_secs = t);
        assert_eq!(r.is_ok(), ok, "{t}");
    }
    assert!(matches!(
        check(&|p| p.groups[0].delay_secs = 3601),
        Err(DrPlanError::Range { field, .. }) if field == "groups[0].delay_secs"
    ));
}

fn rule(nic: u32, address: Option<&str>, dhcp: bool) -> DrIpRule {
    DrIpRule {
        nic,
        address: address.map(str::to_string),
        gateway: None,
        dns: Vec::new(),
        dhcp,
    }
}

#[test]
fn ip_rule_validation() {
    let with = |rules: Vec<DrIpRule>| {
        let mut p = plan(vec![group("db", &["db1"])]);
        p.groups[0].vms[0].ip = rules;
        p.validate()
    };
    let reason = |r: Result<(), DrPlanError>| match r {
        Err(DrPlanError::IpRule { reason, .. }) => reason,
        other => panic!("{other:?}"),
    };
    assert_eq!(with(vec![rule(0, Some("10.0.0.5/24"), false)]), Ok(()));
    assert_eq!(
        with(vec![
            rule(0, Some("10.0.0.5/24"), false),
            rule(0, Some("fd00::5/64"), false)
        ]),
        Ok(())
    );
    assert!(reason(with(vec![rule(0, None, false)])).contains("exactly one"));
    assert!(reason(with(vec![rule(0, Some("10.0.0.5/24"), true)])).contains("exactly one"));
    let mut dhcp_gw = rule(0, None, true);
    dhcp_gw.gateway = Some("10.0.0.1".into());
    assert!(reason(with(vec![dhcp_gw])).contains("no gateway"));
    assert!(
        reason(with(vec![
            rule(0, Some("10.0.0.5/24"), false),
            rule(0, None, true)
        ]))
        .contains("two ipv4")
    );
    assert!(reason(with(vec![rule(0, Some("10.0.0.5"), false)])).contains("prefix"));
    let mut bad_gw = rule(0, Some("10.0.0.5/24"), false);
    bad_gw.gateway = Some("fd00::1".into());
    assert!(reason(with(vec![bad_gw])).contains("gateway"));
    assert!(reason(with(vec![rule(16, None, true)])).contains("out of range"));
    let mut dns = rule(0, Some("10.0.0.5/24"), false);
    dns.dns = ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"]
        .iter()
        .map(|a| a.parse().unwrap())
        .collect();
    assert!(reason(with(vec![dns.clone()])).contains("dns"));
    // 겹치는 DNS는 한 번만 센다.
    dns.dns.truncate(3);
    let mut again = rule(1, None, true);
    again.dns = dns.dns.clone();
    assert_eq!(with(vec![dns, again]), Ok(()));
}

#[test]
fn ip_rules_become_cloud_init() {
    let base = vec![CloudInitNet {
        ipv4: Some(IpConfig::Static {
            address: "192.168.1.5/24".into(),
            gateway: None,
        }),
        ipv6: Some(IpConfig::Slaac),
    }];
    let mut r0 = rule(0, Some("10.20.0.5/24"), false);
    r0.gateway = Some("10.20.0.1".into());
    r0.dns = vec!["10.20.0.2".parse().unwrap()];
    let r2 = rule(2, None, true);
    let (nets, dns) = cloud_init_networks(&base, &[r0, r2]);
    assert_eq!(nets.len(), 3);
    assert_eq!(
        nets[0].ipv4,
        Some(IpConfig::Static {
            address: "10.20.0.5/24".into(),
            gateway: Some("10.20.0.1".into())
        })
    );
    // 규칙이 없는 칸은 백업의 것 그대로, 사이 NIC는 빈 항목(IPv4 DHCP)
    assert_eq!(nets[0].ipv6, Some(IpConfig::Slaac));
    assert_eq!(nets[1], CloudInitNet::default());
    assert_eq!(nets[2].ipv4, Some(IpConfig::Dhcp));
    assert_eq!(
        dns.unwrap().servers,
        vec!["10.20.0.2".parse::<IpAddr>().unwrap()]
    );
    let (nets, dns) = cloud_init_networks(&[], &[rule(0, Some("fd00::5/64"), false)]);
    assert_eq!(nets[0].ipv4, None);
    assert!(matches!(nets[0].ipv6, Some(IpConfig::Static { .. })));
    assert_eq!(dns, None);
}

// ---- 보호 상태 ----

#[test]
fn protection_picks_newest_ready_copy_from_the_source() {
    let p = plan(vec![
        group("db", &["db1"]),
        group("web", &["web1", "web2", "web3"]),
    ]);
    let now = 1_000_000;
    let old = copy("db1-old", "db1", CA, now - 7200);
    let new = copy("db1-new", "db1", CA, now - 3600);
    let mut failed = copy("db1-failed", "db1", CA, now - 10);
    failed.status.phase = BackupPhase::Failed;
    let foreign = copy("db1-foreign", "db1", OTHER_CA, now - 5);
    let mut local = copy("web1-local", "web1", CA, now - 5);
    local.spec.origin = None;
    let stale = copy("web2-stale", "web2", CA, now - 2 * 86_400);
    let backups = vec![old, new.clone(), failed, foreign, local, stale.clone()];
    let prot = protection(&p, &backups, Some(CA), now);
    assert_eq!(prot.len(), 4);
    assert_eq!(prot[0].vm, "db1");
    assert_eq!(prot[0].backup.as_deref(), Some("db1-new"));
    assert_eq!(prot[0].backup_id, Some(new.id));
    assert_eq!(prot[0].point_at, Some(now - 3600));
    assert_eq!(prot[0].age_secs, Some(3600));
    assert!(prot[0].ok);
    assert_eq!(prot[0].issue, None);
    // 이 클러스터에서 만든 백업은 복구 지점이 아니다.
    assert_eq!(prot[1].issue, Some(ProtectionIssue::NoRecoveryPoint));
    assert!(!prot[1].ok);
    assert_eq!(prot[2].backup.as_deref(), Some("web2-stale"));
    assert_eq!(prot[2].issue, Some(ProtectionIssue::OverRpo));
    assert!(!prot[2].ok);
    assert_eq!(prot[3].issue, Some(ProtectionIssue::NoRecoveryPoint));
    // 다른 클러스터(CA 지문이 다름)의 사본만 있으면 없음
    let prot = protection(&p, &backups, Some(OTHER_CA), now);
    assert_eq!(prot[0].backup.as_deref(), Some("db1-foreign"));
    assert_eq!(prot[2].issue, Some(ProtectionIssue::NoRecoveryPoint));
    // 원격이 없으면 모두 없음
    let prot = protection(&p, &backups, None, now);
    assert!(
        prot.iter()
            .all(|v| v.issue == Some(ProtectionIssue::NoRecoveryPoint))
    );
    // RPO 0은 나이를 보지 않는다.
    let mut off = p.clone();
    off.rpo_target_secs = 0;
    assert!(protection(&off, &backups, Some(CA), now)[2].ok);
    let found = recovery_point("web2", &backups, CA).unwrap();
    assert_eq!(found.id, stale.id);
    // 보호 상태 직렬화 왕복
    let status = RecoveryPlanStatus {
        protection: protection(&p, &backups, Some(CA), now),
        checked_at: Some(now),
        last_test: Some(LastTest {
            run: Uuid::now_v7(),
            at: now,
            rto_secs: Some(42),
            result: DrRunState::Succeeded,
            readiness_issues: 0,
        }),
        run: None,
        source_fence_pending: Vec::new(),
    };
    let v = serde_json::to_value(&status).unwrap();
    assert_eq!(
        v["protection"][1],
        json!({ "vm": "web1", "ok": false, "issue": "no-recovery-point" })
    );
    assert_eq!(
        serde_json::from_value::<RecoveryPlanStatus>(v).unwrap(),
        status
    );
    assert_eq!(
        serde_json::to_value(RecoveryPlanStatus::default()).unwrap(),
        json!({})
    );
}

#[test]
fn incremental_copies_need_a_ready_chain() {
    let p = plan(vec![group("db", &["db1"])]);
    let now = 100_000;
    let full = copy("full", "db1", CA, now - 300);
    let mut inc1 = copy("inc1", "db1", CA, now - 200);
    inc1.status.parent = Some(full.id);
    let mut inc2 = copy("inc2", "db1", CA, now - 100);
    inc2.status.parent = Some(inc1.id);
    let all = vec![full.clone(), inc1.clone(), inc2.clone()];
    assert_eq!(
        protection(&p, &all, Some(CA), now)[0].backup.as_deref(),
        Some("inc2")
    );
    // 사슬 가운데가 실패면 그 뒤 증분은 쓰지 못하고 전체 사본으로 돌아간다.
    let mut broken = all.clone();
    broken[1].status.phase = BackupPhase::Receiving;
    assert_eq!(
        protection(&p, &broken, Some(CA), now)[0].backup.as_deref(),
        Some("full")
    );
    // 부모가 없어졌으면
    let missing = vec![inc1.clone(), inc2.clone()];
    assert_eq!(
        protection(&p, &missing, Some(CA), now)[0].issue,
        Some(ProtectionIssue::NoRecoveryPoint)
    );
    // 고리는 쓰지 않는다.
    let mut a = copy("a", "db1", CA, now - 10);
    let mut b = copy("b", "db1", CA, now - 20);
    a.status.parent = Some(b.id);
    b.status.parent = Some(a.id);
    assert_eq!(protection(&p, &[a, b], Some(CA), now)[0].backup, None);
}

#[test]
fn rpo_summary_names_the_bad_vms() {
    let mk = |vm: &str, age: Option<u64>, issue: Option<ProtectionIssue>| VmProtection {
        vm: vm.into(),
        backup: None,
        backup_id: None,
        point_at: None,
        age_secs: age,
        ok: issue.is_none(),
        issue,
    };
    let mut prot = vec![
        mk("web3", Some(34 * 3600), Some(ProtectionIssue::OverRpo)),
        mk("db1", None, Some(ProtectionIssue::NoRecoveryPoint)),
    ];
    prot.extend((0..10).map(|i| mk(&format!("ok{i}"), Some(60), None)));
    assert_eq!(
        rpo_summary("web-tier", 86_400, &prot).unwrap(),
        "plan web-tier: 2 of 12 VMs over RPO 24h (web3 34h, db1 no recovery point)"
    );
    assert_eq!(rpo_summary("web-tier", 86_400, &prot[2..]), None);
    let many: Vec<_> = (0..7)
        .map(|i| {
            mk(
                &format!("v{i}"),
                None,
                Some(ProtectionIssue::NoRecoveryPoint),
            )
        })
        .collect();
    let s = rpo_summary("p", 0, &many).unwrap();
    assert!(
        s.starts_with("plan p: 7 of 7 VMs unprotected (v0 no recovery point"),
        "{s}"
    );
    assert!(s.ends_with(", +2 more)"), "{s}");
}

// ---- 이름 ----

#[test]
fn test_names_fit_the_name_rules() {
    assert_eq!(test_vm_name("web1"), "web1-drt");
    assert_eq!(bubble_network_name("web-tier", 1), "drt-web-tier-1");
    assert_eq!(test_folder("web-tier"), "dr-test/web-tier");
    assert!(parse_folder_path(&test_folder(&"p".repeat(63))).is_ok());
    let long = "a".repeat(63);
    let long2 = format!("{}b", "a".repeat(62));
    let dashy = format!("{}-{}", "a".repeat(50), "b".repeat(12));
    for name in [
        long.as_str(),
        long2.as_str(),
        dashy.as_str(),
        &"x".repeat(59),
        &"x".repeat(60),
    ] {
        let t = test_vm_name(name);
        assert!(t.len() <= 63, "{t}");
        assert!(validate_name(&t).is_ok(), "{t}");
        assert!(t.ends_with("-drt"), "{t}");
        for n in [1, 9, 32] {
            let b = bubble_network_name(name, n);
            assert!(b.len() <= 63 && validate_name(&b).is_ok(), "{b}");
            assert!(
                b.starts_with("drt-") && b.ends_with(&format!("-{n}")),
                "{b}"
            );
        }
    }
    assert_eq!(
        test_vm_name(&"x".repeat(59)),
        format!("{}-drt", "x".repeat(59))
    );
    // 줄인 이름끼리는 해시로 갈린다.
    assert_ne!(test_vm_name(&long), test_vm_name(&long2));
    assert_ne!(
        bubble_network_name(&long, 1),
        bubble_network_name(&long2, 1)
    );
    // 같은 이름은 늘 같은 이름(리더가 바뀌어도)
    assert_eq!(test_vm_name(&long), test_vm_name(&long));
}

/// 3단계: 실행 이름과 되돌리기 백업 작업 이름도 이름 규칙에 맞게 줄인다.
#[test]
fn run_and_reprotect_names_fit_the_name_rules() {
    assert_eq!(
        run_name("web-tier", "test", 1764200000),
        "web-tier-test-1764200000"
    );
    assert_eq!(reprotect_job_name("web-tier"), "dr-reprotect-web-tier");
    let long = "a".repeat(63);
    let long2 = format!("{}b", "a".repeat(62));
    for name in [long.as_str(), long2.as_str(), &"x".repeat(50)] {
        let r = run_name(name, "failover", 1764200000);
        assert!(validate_name(&r).is_ok(), "{r}");
        assert!(r.ends_with("-failover-1764200000"), "{r}");
        let j = reprotect_job_name(name);
        assert!(validate_name(&j).is_ok(), "{j}");
        assert!(j.starts_with("dr-reprotect-"), "{j}");
    }
    assert_eq!(
        reprotect_job_name(&"x".repeat(50)),
        format!("dr-reprotect-{}", "x".repeat(50))
    );
    assert_ne!(reprotect_job_name(&long), reprotect_job_name(&long2));
    assert_ne!(run_name(&long, "test", 1), run_name(&long2, "test", 1));
}

// ---- 상태 기계 ----

/// 가짜 클러스터: 할 일을 받아 Task를 만들고, 시험이 Task를 끝낸다.
struct World {
    obs: DrObservation,
    /// Task → (할 일, VM 이름)
    tasks: BTreeMap<Uuid, (&'static str, String)>,
    /// 만든 Task 수(할 일 종류마다)
    issued: BTreeMap<&'static str, usize>,
    /// 복원 Task → 만들 VM id
    vm_ids: BTreeMap<Uuid, Uuid>,
}

impl World {
    fn new(now: u64) -> Self {
        World {
            obs: DrObservation {
                now,
                source_ca: Some(CA.into()),
                ..Default::default()
            },
            tasks: BTreeMap::new(),
            issued: BTreeMap::new(),
            vm_ids: BTreeMap::new(),
        }
    }

    fn task(&mut self, what: &'static str, vm: &str) -> DrResult {
        let id = Uuid::now_v7();
        self.tasks.insert(id, (what, vm.to_string()));
        self.obs.tasks.insert(id, TaskOutcome::Running);
        *self.issued.entry(what).or_default() += 1;
        DrResult::Task(id)
    }

    fn perform(&mut self, a: &DrAction) -> DrResult {
        match a {
            DrAction::Restore { name, .. } => {
                let DrResult::Task(task) = self.task("restore", name) else {
                    unreachable!()
                };
                let vm = Uuid::now_v7();
                self.vm_ids.insert(task, vm);
                DrResult::Restored { task, vm }
            }
            DrAction::Start { vm, .. } => self.task("start", vm),
            DrAction::DeleteVm { vm, .. } => self.task("delete", vm),
            DrAction::CreateBubble { name, .. } => {
                self.obs.networks.insert(name.clone());
                self.obs.test_networks.insert(name.clone());
                DrResult::Done
            }
            DrAction::DeleteNetwork { name, .. } => {
                self.obs.networks.remove(name);
                DrResult::Done
            }
            _ => DrResult::Done,
        }
    }

    /// 걸음 하나: 상태 기계 → 할 일 → 결과 적기
    fn tick(&mut self, spec: &DrRunSpec, s: &DrRunStatus) -> (DrRunStatus, Vec<DrAction>) {
        let (mut next, actions) = advance(spec, s, &self.obs);
        for a in &actions {
            let r = self.perform(a);
            next.record(a, r, self.obs.now);
        }
        (next, actions)
    }

    /// 도는 Task를 모두 끝낸다(VM에 그 결과를 남김). `fail`의 VM은 실패.
    fn finish_tasks(&mut self, fail: &[&str]) {
        let running: Vec<Uuid> = self
            .obs
            .tasks
            .iter()
            .filter(|(_, o)| **o == TaskOutcome::Running)
            .map(|(id, _)| *id)
            .collect();
        for id in running {
            let (what, vm) = self.tasks[&id].clone();
            if fail.contains(&vm.as_str()) {
                self.obs
                    .tasks
                    .insert(id, TaskOutcome::Failed(format!("{what} failed")));
                continue;
            }
            self.obs.tasks.insert(id, TaskOutcome::Succeeded);
            match what {
                "restore" => {
                    let seen = VmObservation {
                        id: self.vm_ids.get(&id).copied(),
                        labelled: true,
                        ..Default::default()
                    };
                    self.obs.vms.insert(vm, seen);
                }
                "start" => {
                    let v = self.obs.vms.entry(vm).or_default();
                    v.running = true;
                }
                "delete" => {
                    self.obs.vms.remove(&vm);
                }
                _ => {}
            }
        }
    }

    fn running_tasks(&self, what: &str) -> usize {
        self.obs
            .tasks
            .iter()
            .filter(|(id, o)| **o == TaskOutcome::Running && self.tasks[*id].0 == what)
            .count()
    }
}

fn phases(s: &DrRunStatus) -> Vec<(String, DrPhase)> {
    s.steps.iter().map(|st| (st.vm.clone(), st.phase)).collect()
}

fn copies(vms: &[&str], now: u64) -> Vec<Backup> {
    vms.iter()
        .map(|v| copy(&format!("{v}-c"), v, CA, now - 600))
        .collect()
}

#[test]
fn groups_run_in_order_and_measure_rto() {
    let now = 10_000;
    let mut p = plan(vec![group("db", &["db1"]), group("web", &["web1", "web2"])]);
    p.groups[0].delay_secs = 30;
    let spec = run_spec(DrRunKind::FailoverDisaster, p);
    let backups = copies(&["db1", "web1", "web2"], now);
    let s0 = DrRunStatus::start(&spec, &backups, Some(CA), now);
    assert_eq!(s0.started_at, now);
    assert!(s0.bubbles.is_empty(), "장애 조치에는 거품이 없다");
    let mut w = World::new(now);
    let (s, a) = w.tick(&spec, &s0);
    // 첫 그룹만 복원
    assert_eq!(a.len(), 1);
    let DrAction::Restore {
        name,
        pool,
        label,
        folder,
        backup,
        ..
    } = &a[0]
    else {
        panic!("{a:?}")
    };
    assert_eq!(name, "db1");
    assert_eq!(pool, "local");
    assert_eq!(folder, &None);
    assert_eq!(*backup, backups[0].id);
    assert_eq!(label, &(DR_LABEL.to_string(), "web-tier".to_string()));
    assert_eq!(s.steps[0].phase, DrPhase::Restoring);
    assert!(s.steps[0].task.is_some());
    // Task가 도는 동안은 다시 내지 않는다.
    let (s, a) = w.tick(&spec, &s);
    assert!(a.is_empty(), "{a:?}");
    w.finish_tasks(&[]);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert!(
        matches!(&a[..], [DrAction::Start { vm, .. }] if vm == "db1"),
        "{a:?}"
    );
    assert_eq!(s.steps[0].phase, DrPhase::Starting);
    w.finish_tasks(&[]);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    // 준비됨 → 그룹이 끝났지만 30초 쉰다.
    assert_eq!(s.steps[0].phase, DrPhase::Ready);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.delay_until, Some(w.obs.now + 30));
    w.obs.now += 29;
    let (s, a) = w.tick(&spec, &s);
    assert!(a.is_empty());
    w.obs.now += 1;
    let (s, a) = w.tick(&spec, &s);
    assert_eq!(s.group, 1);
    assert_eq!(a.len(), 2, "둘째 그룹은 함께");
    for _ in 0..2 {
        w.finish_tasks(&[]);
        w.obs.now += 5;
    }
    let (s, _) = w.tick(&spec, &s);
    w.finish_tasks(&[]);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert!(a.is_empty());
    assert_eq!(s.state, DrRunState::Succeeded, "{:?}", phases(&s));
    assert_eq!(s.finished_at, Some(w.obs.now));
    assert_eq!(s.rto_secs, Some(w.obs.now - now));
    // 끝난 실행은 그대로
    let (again, a) = advance(&spec, &s, &w.obs);
    assert_eq!(again, s);
    assert!(a.is_empty());
}

#[test]
fn concurrency_is_capped_per_run_and_per_node() {
    let now = 10_000;
    let names: Vec<String> = (1..=6).map(|i| format!("v{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut p = plan(vec![group("all", &refs)]);
    // v1~v3는 n1에 묶음
    for v in &mut p.groups[0].vms[..3] {
        v.node = Some("n1".into());
    }
    let mut spec = run_spec(DrRunKind::FailoverDisaster, p);
    spec.max_parallel = 3;
    let s0 = DrRunStatus::start(&spec, &copies(&refs, now), Some(CA), now);
    let mut w = World::new(now);
    let (s, a) = w.tick(&spec, &s0);
    let started: Vec<&str> = a
        .iter()
        .map(|a| match a {
            DrAction::Restore { name, .. } => name.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    // 실행 전체 3개, n1에는 2개: v1·v2(n1)와 v4(노드 모름)
    assert_eq!(started, ["v1", "v2", "v4"]);
    // 다시 걸어도 늘지 않는다.
    let (mut s, a) = w.tick(&spec, &s);
    assert!(a.is_empty());
    let mut peak = 0;
    for _ in 0..30 {
        w.finish_tasks(&[]);
        w.obs.now += 5;
        let (next, _) = w.tick(&spec, &s);
        s = next;
        let in_flight = w.running_tasks("restore") + w.running_tasks("start");
        peak = peak.max(in_flight);
        assert!(in_flight <= 3, "{in_flight}");
        let n1 = s
            .steps
            .iter()
            .filter(|st| st.phase.in_flight() && st.node.as_deref() == Some("n1"))
            .count();
        assert!(n1 <= PER_NODE_PARALLEL);
        if s.state.is_finished() {
            break;
        }
    }
    assert_eq!(peak, 3);
    assert_eq!(s.state, DrRunState::Succeeded, "{:?}", phases(&s));
}

/// 기다림이 시간을 넘김: `continue`면 그 VM만 timed_out, 다음 그룹은 간다(partial).
#[test]
fn timeout_continue_goes_on() {
    let now = 10_000;
    let mut p = plan(vec![group("db", &["db1", "db2"]), group("web", &["web1"])]);
    p.groups[0].wait = DrWait::Guest;
    p.groups[0].timeout_secs = 60;
    let spec = run_spec(DrRunKind::FailoverDisaster, p);
    let s0 = DrRunStatus::start(&spec, &copies(&["db1", "db2", "web1"], now), Some(CA), now);
    let mut w = World::new(now);
    let (mut s, _) = w.tick(&spec, &s0);
    for _ in 0..3 {
        w.finish_tasks(&[]);
        w.obs.now += 5;
        s = w.tick(&spec, &s).0;
    }
    assert_eq!(s.steps[0].phase, DrPhase::Waiting);
    // db1의 게스트 에이전트만 답한다.
    w.obs.vms.get_mut("db1").unwrap().guest = true;
    w.obs.now += 5;
    s = w.tick(&spec, &s).0;
    assert_eq!(s.steps[0].phase, DrPhase::Ready);
    assert_eq!(s.steps[1].phase, DrPhase::Waiting);
    w.obs.now = s.steps[1].at + 59;
    s = w.tick(&spec, &s).0;
    assert_eq!(s.steps[1].phase, DrPhase::Waiting);
    w.obs.now += 1;
    let (next, a) = w.tick(&spec, &s);
    s = next;
    assert_eq!(s.steps[1].phase, DrPhase::TimedOut);
    assert!(s.steps[1].error.as_deref().unwrap().contains("guest agent"));
    assert!(
        matches!(&a[..], [DrAction::Restore { name, .. }] if name == "web1"),
        "{a:?}"
    );
    for _ in 0..3 {
        w.finish_tasks(&[]);
        w.obs.now += 5;
        s = w.tick(&spec, &s).0;
    }
    assert_eq!(s.state, DrRunState::Partial, "{:?}", phases(&s));
    assert_eq!(s.steps[2].phase, DrPhase::Ready);
}

/// `stop`이면 실패가 생긴 뒤 남은 VM은 skipped이고, 진행 중인 VM(같은 그룹)은 끝까지 간다.
#[test]
fn timeout_stop_skips_the_rest() {
    let now = 10_000;
    let mut p = plan(vec![
        group("db", &["db1", "db2"]),
        group("web", &["web1", "web2"]),
    ]);
    p.on_timeout = OnTimeout::Stop;
    let spec = run_spec(DrRunKind::FailoverDisaster, p);
    let s0 = DrRunStatus::start(
        &spec,
        &copies(&["db1", "db2", "web1", "web2"], now),
        Some(CA),
        now,
    );
    let mut w = World::new(now);
    let (mut s, _) = w.tick(&spec, &s0);
    // db1 복원 실패, db2는 성공
    w.finish_tasks(&["db1"]);
    w.obs.now += 5;
    let (next, a) = w.tick(&spec, &s);
    s = next;
    assert_eq!(s.steps[0].phase, DrPhase::Failed);
    assert!(s.stopping);
    // db2는 같은 그룹이라 시작한다. 둘째 그룹은 건너뜀.
    assert!(
        matches!(&a[..], [DrAction::Start { vm, .. }] if vm == "db2"),
        "{a:?}"
    );
    assert_eq!(s.steps[2].phase, DrPhase::Skipped);
    assert_eq!(s.steps[3].phase, DrPhase::Skipped);
    assert_eq!(s.steps[2].error.as_deref(), Some("skipped after a failure"));
    assert_eq!(s.state, DrRunState::Running);
    w.finish_tasks(&[]);
    w.obs.now += 5;
    s = w.tick(&spec, &s).0;
    assert_eq!(s.steps[1].phase, DrPhase::Ready);
    assert_eq!(s.state, DrRunState::Partial);
    assert_eq!(w.issued.get("restore"), Some(&2));
}

/// 복구 지점이 없는 VM은 처음부터 failed이고, 모두 그렇다면 실행은 failed다.
#[test]
fn missing_recovery_points_fail_the_vm() {
    let now = 10_000;
    let spec = run_spec(
        DrRunKind::FailoverDisaster,
        plan(vec![group("db", &["db1"]), group("web", &["web1"])]),
    );
    let s0 = DrRunStatus::start(&spec, &copies(&["web1"], now), Some(CA), now);
    assert_eq!(s0.steps[0].phase, DrPhase::Failed);
    assert_eq!(s0.steps[0].error.as_deref(), Some("no recovery point"));
    let mut w = World::new(now);
    let (s, a) = w.tick(&spec, &s0);
    assert!(matches!(&a[..], [DrAction::Restore { name, .. }] if name == "web1"));
    assert_eq!(s.group, 1);
    let s0 = DrRunStatus::start(&spec, &[], Some(CA), now);
    let (s, a) = advance(&spec, &s0, &w.obs);
    assert!(a.is_empty());
    assert_eq!(s.state, DrRunState::Failed);
    assert_eq!(s.rto_secs, None);
}

/// 취소: 도는 Task는 끝까지, 그 뒤 단계는 없다. 시험이면 끝날 때 뒷정리를 시작한다.
#[test]
fn cancel_stops_after_running_tasks() {
    let now = 10_000;
    let spec = run_spec(
        DrRunKind::Test,
        plan(vec![group("db", &["db1", "db2"]), group("web", &["web1"])]),
    );
    let backups = copies(&["db1", "db2", "web1"], now);
    let s0 = DrRunStatus::start(&spec, &backups, Some(CA), now);
    let mut w = World::new(now);
    let (mut s, a) = w.tick(&spec, &s0);
    // 거품을 먼저 만들고 그다음 걸음에 복원
    assert!(matches!(&a[..], [DrAction::CreateBubble { .. }]), "{a:?}");
    s = w.tick(&spec, &s).0;
    assert_eq!(s.steps[0].phase, DrPhase::Restoring);
    s.cancel_requested = true;
    let (next, a) = w.tick(&spec, &s);
    s = next;
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.steps[2].phase, DrPhase::Skipped);
    assert_eq!(s.steps[0].phase, DrPhase::Restoring, "도는 Task는 그대로");
    assert_eq!(s.state, DrRunState::Running);
    w.finish_tasks(&[]);
    w.obs.now += 5;
    let (next, a) = w.tick(&spec, &s);
    s = next;
    // 복원은 끝났지만 시작하지 않는다.
    assert_eq!(s.steps[0].phase, DrPhase::Skipped);
    assert_eq!(s.steps[0].error.as_deref(), Some("cancelled"));
    assert_eq!(s.state, DrRunState::Cancelled);
    assert_eq!(a, vec![DrAction::StartCleanup]);
    assert_eq!(w.issued.get("start"), None);
    // 뒷정리 대상: 이름을 정한 시험 VM과 거품
    let target = s.cleanup_target(Uuid::nil());
    assert_eq!(target.vms, ["db1-drt", "db2-drt"]);
    assert_eq!(target.networks, ["drt-web-tier-1"]);
}

/// 새 리더: 저장된 기록과 같은 관찰이면 같은 출력이다. Task id가 있는 단계는 다시 내지 않고, 없는 단계(낸 뒤 적기 전에
/// 리더가 바뀜)는 같은 할 일을 다시 낸다.
#[test]
fn new_leader_continues_from_the_record() {
    let now = 10_000;
    let spec = run_spec(
        DrRunKind::FailoverDisaster,
        plan(vec![group("db", &["db1", "db2"])]),
    );
    let s0 = DrRunStatus::start(&spec, &copies(&["db1", "db2"], now), Some(CA), now);
    let w = World::new(now);
    let first = advance(&spec, &s0, &w.obs);
    let again = advance(&spec, &s0, &w.obs);
    assert_eq!(first, again);
    // 할 일을 냈지만 db2의 결과를 적기 전에 리더가 바뀜
    let (mut s, a) = first;
    s.record(&a[0], DrResult::Task(Uuid::now_v7()), now);
    let mut obs = w.obs.clone();
    obs.tasks
        .insert(s.steps[0].task.unwrap(), TaskOutcome::Running);
    let (s2, a2) = advance(&spec, &s, &obs);
    assert_eq!(a2.len(), 1);
    assert_eq!(a2[0], a[1], "적지 못한 할 일만 다시");
    assert_eq!(s2.steps[0].task, s.steps[0].task);
    // 결과를 적은 뒤에는 아무것도 다시 내지 않는다.
    s.record(&a[1], DrResult::Task(Uuid::now_v7()), now);
    for st in &s.steps {
        obs.tasks.insert(st.task.unwrap(), TaskOutcome::Running);
    }
    let (_, a3) = advance(&spec, &s, &obs);
    assert!(a3.is_empty());
    // 저장한 기록을 JSON으로 왕복해도 같다(새 리더는 Raft에서 읽는다).
    let stored: DrRunStatus = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(stored, s);
    assert_eq!(advance(&spec, &stored, &obs), advance(&spec, &s, &obs));
    // Task가 없어졌으면 VM으로 판단한다.
    let mut gone = obs.clone();
    gone.tasks.clear();
    gone.vms.insert("db1".into(), VmObservation::default());
    let (s4, _) = advance(&spec, &s, &gone);
    assert_eq!(s4.steps[0].phase, DrPhase::Starting);
    assert_eq!(s4.steps[1].phase, DrPhase::Failed);
    assert!(s4.steps[1].error.as_deref().unwrap().contains("is gone"));
}

/// 시험: 거품 네트워크를 만들고, 시험 이름·폴더·라벨·네트워크 맵으로 복원하고, 기다림 `ip`는 기대 주소를 본다.
#[test]
fn test_run_uses_bubbles_and_test_names() {
    let now = 10_000;
    let mut p = plan(vec![group("web", &["web1"])]);
    p.groups[0].wait = DrWait::Ip;
    p.groups[0].vms[0].ip = vec![rule(0, Some("10.20.0.5/24"), false)];
    p.groups[0].vms[0].live = true;
    p.test_networks.insert("mgmt".into(), "lab".into());
    p.networks.insert("lan".into(), "dr-lan".into());
    p.security_groups.insert("web".into(), "dr-web".into());
    let spec = run_spec(DrRunKind::Test, p.clone());
    let mut b = copy("web1-c", "web1", CA, now - 60);
    b.spec.vm_spec = vm_spec(&["lan", "mgmt", "dmz"]);
    let backups = vec![b];
    assert_eq!(bubble_sources(&p, &backups, Some(CA)), ["dmz", "lan"]);
    let s0 = DrRunStatus::start(&spec, &backups, Some(CA), now);
    assert_eq!(
        s0.bubbles
            .iter()
            .map(|b| (b.source.as_str(), b.name.as_str(), b.like.as_str()))
            .collect::<Vec<_>>(),
        [
            ("dmz", "drt-web-tier-1", "dmz"),
            ("lan", "drt-web-tier-2", "dr-lan")
        ]
    );
    let mut w = World::new(now);
    // 한 거품은 이미 있음(앞 리더가 만들고 적지 못함 — 시험 라벨이 이 계획)
    w.obs.networks.insert("drt-web-tier-1".into());
    w.obs.test_networks.insert("drt-web-tier-1".into());
    let (s, a) = w.tick(&spec, &s0);
    assert_eq!(
        a,
        vec![DrAction::CreateBubble {
            bubble: 1,
            name: "drt-web-tier-2".into(),
            like: "dr-lan".into()
        }]
    );
    assert!(s.bubbles.iter().all(|b| b.state == BubbleState::Ready));
    let (s, a) = w.tick(&spec, &s);
    let DrAction::Restore {
        name,
        folder,
        networks,
        security_groups,
        label,
        live,
        ip,
        ..
    } = &a[0]
    else {
        panic!("{a:?}")
    };
    assert_eq!(name, "web1-drt");
    assert_eq!(folder.as_deref(), Some("dr-test/web-tier"));
    assert_eq!(label, &(DR_TEST_LABEL.to_string(), "web-tier".to_string()));
    assert!(*live);
    assert_eq!(ip.len(), 1);
    let want: BTreeMap<String, String> = [
        ("dmz", "drt-web-tier-1"),
        ("lan", "drt-web-tier-2"),
        ("mgmt", "lab"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(networks, &want);
    assert_eq!(
        security_groups.get("web").map(String::as_str),
        Some("dr-web")
    );
    // 라이브 복원은 시작 Task 없이 기다림으로
    w.finish_tasks(&[]);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.steps[0].phase, DrPhase::Waiting);
    let vm = w.obs.vms.get_mut("web1-drt").unwrap();
    vm.running = true;
    vm.guest = true;
    vm.ips = vec!["fe80::1".parse().unwrap(), "10.0.0.9".parse().unwrap()];
    w.obs.now += 5;
    let (s, _) = w.tick(&spec, &s);
    assert_eq!(
        s.steps[0].phase,
        DrPhase::Waiting,
        "기대 주소가 아니면 기다린다"
    );
    w.obs
        .vms
        .get_mut("web1-drt")
        .unwrap()
        .ips
        .push("10.20.0.5".parse().unwrap());
    w.obs.now += 5;
    let (s, _) = w.tick(&spec, &s);
    assert_eq!(s.state, DrRunState::Succeeded);
    assert_eq!(s.rto_secs, Some(w.obs.now - now));
    assert_eq!(w.issued.get("start"), None);
}

#[test]
fn ip_wait_without_rules_takes_any_global_address() {
    let v = |ips: &[&str]| VmObservation {
        running: true,
        guest: true,
        ips: ips.iter().map(|a| a.parse().unwrap()).collect(),
        node: None,
        id: None,
        labelled: false,
    };
    assert!(!wait_met(DrWait::Ip, &v(&[]), &[]));
    assert!(!wait_met(
        DrWait::Ip,
        &v(&["127.0.0.1", "169.254.1.1", "fe80::2", "::1"]),
        &[]
    ));
    assert!(wait_met(DrWait::Ip, &v(&["fe80::2", "2001:db8::5"]), &[]));
    assert!(wait_met(DrWait::Ip, &v(&["10.1.1.1"]), &[]));
    let mut stopped = v(&["10.1.1.1"]);
    stopped.running = false;
    assert!(!wait_met(DrWait::Ip, &stopped, &[]));
    assert!(!wait_met(DrWait::Guest, &stopped, &[]));
}

/// 거품 네트워크를 만들지 못하면 VM을 하나도 되살리지 않고 실패한다.
#[test]
fn bubble_failure_fails_the_test() {
    let now = 10_000;
    let spec = run_spec(DrRunKind::Test, plan(vec![group("web", &["web1"])]));
    let s0 = DrRunStatus::start(&spec, &copies(&["web1"], now), Some(CA), now);
    let w = World::new(now);
    let (mut s, a) = advance(&spec, &s0, &w.obs);
    s.record(&a[0], DrResult::Failed("no SDN".into()), now);
    let (s, a) = advance(&spec, &s, &w.obs);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.state, DrRunState::Failed);
    assert_eq!(s.error.as_deref(), Some("network drt-web-tier-1: no SDN"));
    assert_eq!(s.steps[0].phase, DrPhase::Skipped);
}

/// 계획된 이전: 운영 쪽을 그룹 역순으로 끄고(5분 뒤 stop), 새 백업을 떠 복제하고, 그 사본으로 그룹 차례대로 복원한다.
#[test]
fn planned_failover_prepares_the_source_first() {
    let now = 10_000;
    let spec = run_spec(
        DrRunKind::FailoverPlanned,
        plan(vec![group("db", &["db1"]), group("web", &["web1"])]),
    );
    let s0 = DrRunStatus::start(&spec, &[], Some(CA), now);
    assert!(s0.steps.iter().all(|st| st.phase == DrPhase::Preparing));
    let mut w = World::new(now);
    // 원격에 닿지 않으면 기다리고 그 까닭을 적는다(나아감은 아니다).
    w.obs.source_error = Some("timed out".into());
    let (s, a) = advance(&spec, &s0, &w.obs);
    assert!(a.is_empty());
    assert_eq!(s.progress_at, s0.progress_at);
    for st in &s.steps {
        assert_eq!(
            st.last_error.as_deref(),
            Some("cannot read the source cluster: timed out")
        );
        assert_eq!(st.retrying_since, Some(now));
    }
    let mut src = SourceObservation::default();
    src.vms.insert("db1".into(), false);
    src.vms.insert("web1".into(), false);
    w.obs.source = Some(src);
    // 뒤 그룹(web)부터 끈다.
    let (s, a) = w.tick(&spec, &s0);
    assert_eq!(
        a,
        vec![DrAction::ShutdownSource {
            step: 1,
            vm: "web1".into()
        }]
    );
    w.obs.now += SHUTDOWN_GRACE_SECS;
    let (s, a) = w.tick(&spec, &s);
    assert_eq!(
        a,
        vec![DrAction::StopSource {
            step: 1,
            vm: "web1".into()
        }]
    );
    w.obs
        .source
        .as_mut()
        .unwrap()
        .vms
        .insert("web1".into(), true);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    // web1은 꺼져 백업으로, 그래서 같은 걸음에 앞 그룹(db1)의 shutdown
    assert_eq!(
        a,
        vec![
            DrAction::BackupSource {
                step: 1,
                vm: "web1".into()
            },
            DrAction::ShutdownSource {
                step: 0,
                vm: "db1".into()
            },
        ]
    );
    let web_backup = Uuid::now_v7();
    let mut s = s;
    s.record(
        &a[0],
        DrResult::SourceBackup {
            name: "web1-last".into(),
            id: web_backup,
        },
        w.obs.now,
    );
    w.obs
        .source
        .as_mut()
        .unwrap()
        .vms
        .insert("db1".into(), true);
    w.obs.source.as_mut().unwrap().backups.insert(
        "web1-last".into(),
        SourceBackup {
            id: web_backup,
            ready: true,
            error: None,
            replica_error: None,
        },
    );
    w.obs.now += 5;
    let (mut s, a) = w.tick(&spec, &s);
    assert_eq!(
        a,
        vec![
            DrAction::ReplicateSource {
                step: 1,
                backup: "web1-last".into()
            },
            DrAction::BackupSource {
                step: 0,
                vm: "db1".into()
            },
        ]
    );
    assert_eq!(
        s.steps[1].prepare.as_ref().unwrap().stage,
        PrepareStage::Replicate
    );
    // 복제를 다시 보내지 않는다.
    let (again, a) = advance(&spec, &s, &w.obs);
    assert!(
        !a.iter()
            .any(|a| matches!(a, DrAction::ReplicateSource { .. })),
        "{a:?}"
    );
    assert_eq!(again.steps[1], s.steps[1]);
    // db1 백업은 실패
    let db_backup = Uuid::now_v7();
    s.record(
        &DrAction::BackupSource {
            step: 0,
            vm: "db1".into(),
        },
        DrResult::SourceBackup {
            name: "db1-last".into(),
            id: db_backup,
        },
        w.obs.now,
    );
    w.obs.source.as_mut().unwrap().backups.insert(
        "db1-last".into(),
        SourceBackup {
            id: db_backup,
            ready: false,
            error: Some("disk full".into()),
            replica_error: None,
        },
    );
    // web1 사본이 옴
    let mut c = copy("web1-copy", "web1", CA, now);
    c.spec.origin.as_mut().unwrap().backup = web_backup;
    w.obs.backups.push(c.clone());
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert_eq!(s.steps[0].phase, DrPhase::Failed);
    assert!(s.steps[0].error.as_deref().unwrap().contains("disk full"));
    assert_eq!(s.prepare_secs, Some(w.obs.now - now));
    assert_eq!(s.steps[1].backup_id, Some(c.id));
    // 준비가 끝나 db 그룹(실패)을 지나 web 그룹을 복원한다. 이 실행이 끄고 되살리지 못하는 db1은 운영 쪽에서 다시 켠다.
    assert!(
        matches!(&a[..], [DrAction::StartSource { step: 0, vm }, DrAction::Restore { name, backup, .. }]
            if vm == "db1" && name == "web1" && *backup == c.id),
        "{a:?}"
    );
    assert!(s.steps[0].prepare.as_ref().unwrap().restarted);
    assert!(
        s.steps[0].notes[0].starts_with("restarted the source VM db1"),
        "{:?}",
        s.steps[0].notes
    );
    let mut s = s;
    for _ in 0..3 {
        w.finish_tasks(&[]);
        w.obs.now += 5;
        s = w.tick(&spec, &s).0;
    }
    assert_eq!(s.state, DrRunState::Partial, "{:?}", phases(&s));
    assert!(s.rto_secs.unwrap() >= s.prepare_secs.unwrap());
}

/// 뒷정리: VM을 지운 뒤 거품 네트워크를 지운다(이미 없는 것은 건너뜀).
#[test]
fn cleanup_deletes_vms_then_bubbles() {
    let now = 10_000;
    let mut spec = run_spec(DrRunKind::Cleanup, plan(vec![group("web", &["web1"])]));
    spec.max_parallel = 1;
    spec.cleanup = Some(DrCleanupTarget {
        run: Uuid::now_v7(),
        vms: vec!["web1-drt".into(), "web2-drt".into(), "gone-drt".into()],
        vm_ids: Vec::new(),
        networks: vec!["drt-web-tier-1".into(), "drt-web-tier-2".into()],
    });
    let s0 = DrRunStatus::start(&spec, &[], None, now);
    let mut w = World::new(now);
    w.obs.vms.insert(
        "web1-drt".into(),
        VmObservation {
            running: true,
            ..Default::default()
        },
    );
    w.obs
        .vms
        .insert("web2-drt".into(), VmObservation::default());
    w.obs.networks.insert("drt-web-tier-1".into());
    let (s, a) = w.tick(&spec, &s0);
    assert_eq!(
        a,
        vec![DrAction::DeleteVm {
            step: 0,
            vm: "web1-drt".into(),
            id: None
        }]
    );
    assert_eq!(s.steps[2].phase, DrPhase::Deleted, "없는 VM");
    w.finish_tasks(&[]);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert_eq!(
        a,
        vec![DrAction::DeleteVm {
            step: 1,
            vm: "web2-drt".into(),
            id: None
        }]
    );
    w.finish_tasks(&[]);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert_eq!(
        a,
        vec![DrAction::DeleteNetwork {
            bubble: 0,
            name: "drt-web-tier-1".into()
        }]
    );
    assert_eq!(s.bubbles[1].state, BubbleState::Removed);
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert!(a.is_empty());
    assert_eq!(s.state, DrRunState::Succeeded);
    assert_eq!(s.rto_secs, None);
    // 지우기 실패는 partial
    let s0 = DrRunStatus::start(&spec, &[], None, now);
    let mut w = World::new(now);
    w.obs
        .vms
        .insert("web1-drt".into(), VmObservation::default());
    let (mut s, _) = w.tick(&spec, &s0);
    w.finish_tasks(&["web1-drt"]);
    for _ in 0..3 {
        w.obs.now += 5;
        s = w.tick(&spec, &s).0;
    }
    assert_eq!(s.state, DrRunState::Partial, "{:?}", phases(&s));
    assert_eq!(s.steps[0].phase, DrPhase::Failed);
}

/// 뒷정리(4단계 엔진): 켜진 VM은 끄는 Task를 내고, 끝나 꺼지면 같은 할 일을 다시 받아 지운다. 곧바로 지우면(`Done`)
/// `deleted`이고, Task 없이 `deleting`인데 VM이 없어졌어도 `deleted`다.
#[test]
fn cleanup_stops_running_vms_before_deleting_them() {
    let now = 10_000;
    let mut spec = run_spec(DrRunKind::Cleanup, plan(vec![group("web", &["web1"])]));
    spec.cleanup = Some(DrCleanupTarget {
        run: Uuid::now_v7(),
        vms: vec!["web1-drt".into(), "web2-drt".into()],
        vm_ids: Vec::new(),
        networks: vec![],
    });
    let s0 = DrRunStatus::start(&spec, &[], None, now);
    let mut obs = DrObservation {
        now,
        ..Default::default()
    };
    let on = VmObservation {
        running: true,
        ..Default::default()
    };
    obs.vms.insert("web1-drt".into(), on);
    obs.vms.insert("web2-drt".into(), VmObservation::default());
    let (mut s, a) = advance(&spec, &s0, &obs);
    assert_eq!(a.len(), 2);
    // web1: 끄는 Task, web2: 곧바로 지움
    let stop = Uuid::now_v7();
    s.record(&a[0], DrResult::Task(stop), now);
    s.record(&a[1], DrResult::Done, now);
    assert_eq!(s.steps[1].phase, DrPhase::Deleted);
    obs.vms.remove("web2-drt");
    obs.tasks.insert(stop, TaskOutcome::Succeeded);
    obs.vms.get_mut("web1-drt").unwrap().running = false;
    obs.now += 5;
    let (mut s, a) = advance(&spec, &s, &obs);
    assert_eq!(
        a,
        vec![DrAction::DeleteVm {
            step: 0,
            vm: "web1-drt".into(),
            id: None
        }]
    );
    assert_eq!(s.steps[0].phase, DrPhase::Deleting);
    // 지우는 데 Task가 없었고(디스크 없음) 그사이 리더가 바뀌어 결과를 적지 못했어도 VM이 없으면 `deleted`
    obs.vms.remove("web1-drt");
    s.steps[0].task = None;
    obs.now += 5;
    let (s, a) = advance(&spec, &s, &obs);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.steps[0].phase, DrPhase::Deleted);
    assert_eq!(s.state, DrRunState::Succeeded);
}

#[test]
fn restore_failure_is_recorded() {
    let now = 10_000;
    let spec = run_spec(
        DrRunKind::FailoverDisaster,
        plan(vec![group("db", &["db1"])]),
    );
    let s0 = DrRunStatus::start(&spec, &copies(&["db1"], now), Some(CA), now);
    let w = World::new(now);
    let (mut s, a) = advance(&spec, &s0, &w.obs);
    // 이름이 이미 쓰임(설계 §3.2 `dr_name_taken`)
    s.record(
        &a[0],
        DrResult::Failed(format!("{ERR_NAME_TAKEN}: VM \"db1\" already exists")),
        now,
    );
    assert_eq!(s.steps[0].phase, DrPhase::Failed);
    let (s, _) = advance(&spec, &s, &w.obs);
    assert_eq!(s.state, DrRunState::Failed);
}

#[test]
fn run_round_trips() {
    let now = 10_000;
    let mut spec = run_spec(
        DrRunKind::FailoverPlanned,
        plan(vec![group("db", &["db1"])]),
    );
    let json = serde_json::to_value(&spec).unwrap();
    assert_eq!(json["kind"], "failover-planned");
    assert_eq!(json["max_parallel"], 4);
    assert!(json.get("cleanup").is_none());
    assert_eq!(serde_json::from_value::<DrRunSpec>(json).unwrap(), spec);
    spec.cleanup = Some(DrCleanupTarget {
        run: Uuid::nil(),
        vms: vec![],
        vm_ids: vec![],
        networks: vec![],
    });
    assert_eq!(
        serde_json::from_value::<DrRunSpec>(serde_json::to_value(&spec).unwrap()).unwrap(),
        spec
    );
    let s = DrRunStatus::start(&spec, &[], Some(CA), now);
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(
        v,
        json!({ "state": "running", "started_at": now, "progress_at": now, "steps": [
            { "group": "db", "vm": "db1", "phase": "preparing", "at": now,
              "prepare": { "stage": "shutdown" } } ] })
    );
    assert_eq!(serde_json::from_value::<DrRunStatus>(v).unwrap(), s);
    assert_eq!(
        serde_json::to_value(DrPhase::TimedOut).unwrap(),
        "timed_out"
    );
    assert_eq!(
        serde_json::to_value(DrRunState::Partial).unwrap(),
        "partial"
    );
    assert_eq!(RecoveryPlanSpec::KIND, Kind::RecoveryPlan);
    assert_eq!(DrRunSpec::KIND, Kind::DrRun);
    assert!(DrRunKind::FailoverDisaster.is_failover() && !DrRunKind::Test.is_failover());
}

// ---- 리뷰 1 ----

/// 복원 Task가 실패하면(깨진 사본일 수 있다) 다음 오래된 복구 지점으로 다시 한다 — 써 본 것을 적고, 모두 셋까지. 검증에
/// 실패한 사본은 처음부터 고르지 않는다.
#[test]
fn a_failed_restore_falls_back_to_older_recovery_points() {
    let now = 10_000;
    let spec = run_spec(
        DrRunKind::FailoverDisaster,
        plan(vec![group("db", &["db1"])]),
    );
    let mut bad = copy("db1-new", "db1", CA, now - 60);
    bad.status.verify =
        Some(serde_json::from_value(json!({ "at": now, "state": "failed" })).unwrap());
    let backups = vec![
        copy("db1-a", "db1", CA, now - 400),
        copy("db1-b", "db1", CA, now - 300),
        copy("db1-c", "db1", CA, now - 200),
        copy("db1-d", "db1", CA, now - 100),
        bad,
    ];
    let s0 = DrRunStatus::start(&spec, &backups, Some(CA), now);
    assert_eq!(
        s0.steps[0].backup.as_deref(),
        Some("db1-d"),
        "검증 실패 사본은 건너뜀"
    );
    let mut w = World::new(now);
    w.obs.backups = backups.clone();
    let (mut s, _) = w.tick(&spec, &s0);
    for want in ["db1-c", "db1-b"] {
        w.finish_tasks(&["db1"]);
        w.obs.now += 5;
        let (next, a) = w.tick(&spec, &s);
        s = next;
        assert!(
            matches!(&a[..], [DrAction::Restore { backup, .. }] if Some(*backup) == s.steps[0].backup_id),
            "{a:?}"
        );
        assert_eq!(s.steps[0].backup.as_deref(), Some(want));
        assert_eq!(s.steps[0].phase, DrPhase::Restoring);
        assert!(s.steps[0].notes.last().unwrap().contains(want));
    }
    assert_eq!(s.steps[0].tried, ["db1-d", "db1-c"]);
    // 셋째도 실패하면 그 VM은 실패(db1-a는 쓰지 않는다)
    w.finish_tasks(&["db1"]);
    w.obs.now += 5;
    let s = w.tick(&spec, &s).0;
    assert_eq!(s.steps[0].phase, DrPhase::Failed);
    assert_eq!(s.steps[0].new_vm, None);
    assert_eq!(w.issued.get("restore"), Some(&3));
    // 시각을 주면 그 시각 이전의 가장 새 것
    let mut at = spec.clone();
    at.point_in_time = Some(now - 250);
    let s = DrRunStatus::start(&at, &backups, Some(CA), now);
    assert_eq!(s.steps[0].backup.as_deref(), Some("db1-b"));
}

/// `allow_missing`이면 복구 지점이 없는 VM은 실패가 아니라 `skipped`이고 `on_timeout: stop`에 들지 않는다.
#[test]
fn allowed_missing_points_are_skipped_not_failed() {
    let now = 10_000;
    let mut p = plan(vec![group("db", &["db1"]), group("web", &["web1"])]);
    p.on_timeout = OnTimeout::Stop;
    let mut spec = run_spec(DrRunKind::FailoverDisaster, p);
    spec.allow_missing = true;
    let s0 = DrRunStatus::start(&spec, &copies(&["db1"], now), Some(CA), now);
    assert_eq!(s0.steps[1].phase, DrPhase::Skipped);
    let mut w = World::new(now);
    let (mut s, a) = w.tick(&spec, &s0);
    assert!(!s.stopping);
    assert!(matches!(&a[..], [DrAction::Restore { name, .. }] if name == "db1"));
    for _ in 0..3 {
        w.finish_tasks(&[]);
        w.obs.now += 5;
        s = w.tick(&spec, &s).0;
    }
    assert_eq!(s.steps[0].phase, DrPhase::Ready);
    assert_eq!(s.state, DrRunState::Partial);
}

/// 라이브 복원: Task가 VM을 켜면(Task는 아직 디스크를 옮김) 곧 기다림으로 넘어가 RTO는 그때다. 복사가 실패하면 경고만 적고,
/// 실행은 복사가 끝나야 끝난다.
#[test]
fn live_restores_count_when_the_vm_runs() {
    let now = 10_000;
    let mut p = plan(vec![group("web", &["web1"])]);
    p.groups[0].vms[0].live = true;
    let spec = run_spec(DrRunKind::FailoverDisaster, p);
    let s0 = DrRunStatus::start(&spec, &copies(&["web1"], now), Some(CA), now);
    let mut w = World::new(now);
    let (s, _) = w.tick(&spec, &s0);
    let task = s.steps[0].task.unwrap();
    // VM이 켜졌다(라이브 복원 Task는 아직 돈다).
    w.obs.now += 5;
    w.obs.vms.insert(
        "web1".into(),
        VmObservation {
            running: true,
            id: s.steps[0].vm_id,
            labelled: true,
            ..Default::default()
        },
    );
    let (s, a) = w.tick(&spec, &s);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.steps[0].phase, DrPhase::Ready);
    assert_eq!(s.steps[0].copy_task, Some(task));
    assert_eq!(s.state, DrRunState::Running, "복사가 끝나야 끝난다");
    let ready_at = w.obs.now;
    w.obs.now += 600;
    w.obs
        .tasks
        .insert(task, TaskOutcome::Failed("mirror broke".into()));
    let (s, _) = w.tick(&spec, &s);
    assert_eq!(s.steps[0].copy_task, None);
    assert!(s.steps[0].notes[0].contains("mirror broke"));
    assert_eq!(s.state, DrRunState::Succeeded);
    assert_eq!(s.rto_secs, Some(ready_at - now));
}

/// 되살린 VM에 라벨이 붙지 않았으면 실행은 끝나지 않는다(스케줄러가 틱마다 다시 단다). 복원을 거절당한(이름 겹침) VM은 이
/// 실행이 만든 것이 아니라 새 VM·뒷정리 대상이 아니다.
#[test]
fn runs_finish_only_after_labels_and_skip_refused_restores() {
    let now = 10_000;
    let spec = run_spec(DrRunKind::Test, plan(vec![group("web", &["web1", "web2"])]));
    let s0 = DrRunStatus::start(&spec, &copies(&["web1", "web2"], now), Some(CA), now);
    let mut w = World::new(now);
    // 첫 걸음은 거품 네트워크
    let (s, _) = w.tick(&spec, &s0);
    let (mut s, a) = advance(&spec, &s, &w.obs);
    assert_eq!(a.len(), 2, "{a:?}");
    for act in &a {
        let r = match act {
            DrAction::Restore { step: 1, .. } => {
                DrResult::Failed("dr_name_taken: VM exists".into())
            }
            other => w.perform(other),
        };
        s.record(act, r, w.obs.now);
    }
    assert_eq!(s.steps[1].phase, DrPhase::Failed);
    assert_eq!(s.steps[1].new_vm, None);
    for _ in 0..2 {
        w.finish_tasks(&[]);
        w.obs.vms.get_mut("web1-drt").unwrap().labelled = false;
        w.obs.now += 5;
        s = w.tick(&spec, &s).0;
    }
    assert_eq!(s.steps[0].phase, DrPhase::Ready);
    assert_eq!(s.state, DrRunState::Running, "라벨이 아직 없다");
    w.obs.vms.get_mut("web1-drt").unwrap().labelled = true;
    s = w.tick(&spec, &s).0;
    assert_eq!(s.state, DrRunState::Partial);
    let t = s.cleanup_target(Uuid::nil());
    assert_eq!(t.vms, ["web1-drt"]);
    assert_eq!(t.vm_ids, [s.steps[0].vm_id.unwrap()]);
}

/// 같은 이름의 네트워크가 이 계획의 시험 라벨 없이 있으면 그 거품은 쓰지 않는다(격리를 알 수 없음 — 실행 멈춤).
#[test]
fn a_foreign_network_with_the_bubble_name_fails_the_bubble() {
    let now = 10_000;
    let spec = run_spec(DrRunKind::Test, plan(vec![group("web", &["web1"])]));
    let s0 = DrRunStatus::start(&spec, &copies(&["web1"], now), Some(CA), now);
    let mut w = World::new(now);
    w.obs.networks.insert("drt-web-tier-1".into());
    let (s, a) = w.tick(&spec, &s0);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.bubbles[0].state, BubbleState::Failed);
    assert!(s.stopping);
    assert_eq!(s.steps[0].phase, DrPhase::Skipped);
    // 되살릴 VM이 없으면 거품을 만들지 않는다.
    let s0 = DrRunStatus::start(&spec, &[], Some(CA), now);
    let (s, a) = advance(&spec, &s0, &World::new(now).obs);
    assert!(a.is_empty());
    assert!(s.bubbles.is_empty());
}

/// 계획된 이전 + `on_timeout: stop`: 뒤 그룹의 준비가 실패하면 앞 그룹 VM에 shutdown을 보내지 않고, 이미 끈 뒤 그룹 VM은
/// 운영 쪽에서 다시 켠다. 꺼지지 않는 운영 VM은 상한 뒤 실패다.
#[test]
fn planned_stop_spares_the_rest_and_restarts_what_it_stopped() {
    let now = 10_000;
    let mut p = plan(vec![group("db", &["db1"]), group("web", &["web1", "web2"])]);
    p.on_timeout = OnTimeout::Stop;
    let spec = run_spec(DrRunKind::FailoverPlanned, p);
    let s0 = DrRunStatus::start(&spec, &[], Some(CA), now);
    let mut w = World::new(now);
    let mut src = SourceObservation::default();
    for vm in ["db1", "web1", "web2"] {
        src.vms.insert(vm.into(), false);
    }
    w.obs.source = Some(src);
    let (s, a) = w.tick(&spec, &s0);
    assert_eq!(a.len(), 2, "뒤 그룹 둘에 shutdown: {a:?}");
    // web1은 꺼졌고 web2는 그 VM이 운영 쪽에서 사라졌다(실패).
    let src = w.obs.source.as_mut().unwrap();
    src.vms.insert("web1".into(), true);
    src.vms.remove("web2");
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert_eq!(s.steps[2].phase, DrPhase::Failed);
    assert!(s.stopping);
    assert!(
        !a.iter().any(|a| matches!(
            a,
            DrAction::ShutdownSource { .. } | DrAction::BackupSource { .. }
        )),
        "{a:?}"
    );
    assert_eq!(
        a,
        vec![DrAction::StartSource {
            step: 1,
            vm: "web1".into()
        }]
    );
    assert_eq!(s.steps[0].phase, DrPhase::Skipped);
    assert_eq!(s.steps[1].phase, DrPhase::Skipped);
    assert!(s.steps[1].prepare.as_ref().unwrap().restarted);
    assert!(
        !s.steps[0].prepare.as_ref().unwrap().stopped,
        "db1은 끄지 않았다"
    );
    assert_eq!(s.state, DrRunState::Running, "다시 켜기를 보낸 걸음");
    w.obs.now += 5;
    let (s, a) = w.tick(&spec, &s);
    assert!(a.is_empty(), "{a:?}");
    assert_eq!(s.state, DrRunState::Failed);

    // 꺼지지 않는 운영 VM: 상한이 지나면 그 VM은 실패
    let spec = run_spec(
        DrRunKind::FailoverPlanned,
        plan(vec![group("db", &["db1"])]),
    );
    let s0 = DrRunStatus::start(&spec, &[], Some(CA), now);
    let mut w = World::new(now);
    let mut src = SourceObservation::default();
    src.vms.insert("db1".into(), false);
    w.obs.source = Some(src);
    let (mut s, _) = w.tick(&spec, &s0);
    w.obs.now += PREPARE_STOP_LIMIT_SECS;
    let (next, a) = w.tick(&spec, &s);
    s = next;
    assert_eq!(s.steps[0].phase, DrPhase::Failed);
    assert!(
        s.steps[0]
            .error
            .as_deref()
            .unwrap()
            .contains("did not stop")
    );
    assert!(matches!(&a[..], [DrAction::StartSource { .. }]), "{a:?}");
}

/// 장애 조치 준비: 복구 지점이 쓰는 네트워크·보안 그룹의 대상(맵, 없으면 같은 이름), 풀, 노드가 이 클러스터에 있는지
#[test]
fn failover_readiness_checks_maps_pools_and_nodes() {
    let now = 10_000;
    let mut p = plan(vec![group("web", &["web1", "web2"])]);
    p.networks.insert("lan".into(), "dr-lan".into());
    p.groups[0].vms[1].node = Some("n9".into());
    let mut b = copy("web1-c", "web1", CA, now - 60);
    b.spec.vm_spec = serde_json::from_value(json!({
        "cpu": { "cores": 1 }, "memory_mib": 512, "machine": "virt",
        "disks": [ { "volume": "root", "bus": "virtio-blk" } ],
        "nics": [ { "network": "lan", "model": "virtio", "security_groups": ["web"] },
                  { "network": "user", "model": "virtio" } ]
    }))
    .unwrap();
    let here = DrInventory {
        networks: ["dr-lan".to_string()].into(),
        security_groups: BTreeSet::new(),
        pools: ["local".to_string()].into(),
        nodes: BTreeSet::new(),
    };
    let r = failover_readiness(&p, &[b], Some(CA), &here);
    assert_eq!(r[0].vm, "web1");
    assert_eq!(
        r[0].issues,
        [ReadinessIssue {
            kind: ReadinessKind::SecurityGroup,
            name: "web".into()
        }]
    );
    assert!(!r[1].ok);
    let kinds: Vec<ReadinessKind> = r[1].issues.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, [ReadinessKind::Node, ReadinessKind::RecoveryPoint]);
}

/// 가장 새 사본이 검증에 실패하면 보호 상태는 `verify-failed`(복구 지점은 그 앞의 것)
#[test]
fn protection_flags_a_newest_copy_that_failed_verification() {
    let now = 10_000;
    let p = plan(vec![group("web", &["web1"])]);
    let mut bad = copy("web1-new", "web1", CA, now - 60);
    bad.status.verify =
        Some(serde_json::from_value(json!({ "at": now, "state": "failed" })).unwrap());
    let old = copy("web1-old", "web1", CA, now - 600);
    let got = protection(&p, &[bad.clone(), old.clone()], Some(CA), now);
    assert!(!got[0].ok);
    assert_eq!(got[0].issue, Some(ProtectionIssue::VerifyFailed));
    assert_eq!(got[0].backup.as_deref(), Some("web1-old"));
    let got = protection(&p, &[bad], Some(CA), now);
    assert_eq!(got[0].issue, Some(ProtectionIssue::VerifyFailed));
    assert_eq!(got[0].backup, None);
    assert!(
        rpo_summary("web-tier", 0, &got)
            .unwrap()
            .contains("web1 newest copy failed verification")
    );
}
