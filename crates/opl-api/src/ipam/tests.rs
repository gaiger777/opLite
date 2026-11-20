//! 주소 채우기 규칙(새 NIC·있던 NIC·템플릿·복원)

use opl_types::cloud_init::{CloudInit, CloudInitNet, IpConfig};
use opl_types::resource::Resource;
use opl_types::vm::{MacAddr, NicModel, NicSpec};
use serde_json::json;

use super::*;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn network(name: &str, subnets: serde_json::Value) -> Network {
    Resource::new(
        name,
        serde_json::from_value(json!({ "sdn": {}, "subnets": subnets })).unwrap(),
    )
}

fn networks() -> Vec<Network> {
    vec![
        network(
            "tenant",
            json!([{ "cidr": "10.0.0.0/29" }, { "cidr": "2001:db8::/64" }]),
        ),
        network("bare", json!([])),
        Resource::new(
            "lan",
            serde_json::from_value(json!({ "bridge": "br0" })).unwrap(),
        ),
    ]
}

fn nic(network: &str, mac: u8, ips: &[&str]) -> NicSpec {
    NicSpec {
        network: network.into(),
        model: NicModel::Virtio,
        mac: Some(MacAddr([0x52, 0x54, 0, 0, 0, mac])),
        security_groups: Vec::new(),
        ips: ips.iter().map(|s| ip(s)).collect(),
    }
}

fn spec(nics: Vec<NicSpec>) -> VmSpec {
    let mut s: VmSpec = serde_json::from_value(json!({
        "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "nics": []
    }))
    .unwrap();
    s.nics = nics;
    s
}

fn vm(name: &str, nics: Vec<NicSpec>) -> Vm {
    Resource::new(name, spec(nics))
}

#[test]
fn new_nics_get_the_lowest_free_address_of_each_family() {
    let other = vm(
        "other",
        vec![nic("tenant", 1, &["10.0.0.2", "2001:db8::2"])],
    );
    let mut s = spec(vec![
        nic("tenant", 2, &[]),
        nic("tenant", 3, &[]),
        nic("bare", 4, &[]),
        nic("lan", 5, &[]),
    ]);
    assign(
        &mut s,
        None,
        &[&other],
        &networks(),
        &HeldIps::new(),
        &|_| true,
    )
    .unwrap();
    assert_eq!(s.nics[0].ips, [ip("10.0.0.3"), ip("2001:db8::3")]);
    // 같은 스펙의 앞 NIC가 쓴 주소도 건너뛴다.
    assert_eq!(s.nics[1].ips, [ip("10.0.0.4"), ip("2001:db8::4")]);
    // 서브넷 없는 SDN 네트워크와 단순 네트워크는 주소가 없다.
    assert!(s.nics[2].ips.is_empty() && s.nics[3].ips.is_empty());
}

#[test]
fn given_addresses_are_checked() {
    let other = vm("other", vec![nic("tenant", 1, &["10.0.0.2"])]);
    let nets = networks();
    let try_ip = |a: &str| {
        let mut s = spec(vec![nic("tenant", 2, &[a])]);
        assign(&mut s, None, &[&other], &nets, &HeldIps::new(), &|_| true)
            .map(|()| s.nics[0].ips.clone())
    };
    // 준 체계는 그대로, 빈 체계(IPv6)는 채운다. 풀 밖이어도 서브넷 안이면 된다.
    assert_eq!(
        try_ip("10.0.0.6").unwrap(),
        [ip("10.0.0.6"), ip("2001:db8::2")]
    );
    let e = try_ip("10.0.0.2").unwrap_err();
    assert_eq!(e.code, "ip_in_use");
    assert!(e.message.contains("\"other\""), "{}", e.message);
    // 부른 사용자가 읽을 수 없는 VM은 이름을 대지 않는다(리뷰 1 I-2).
    let mut s = spec(vec![nic("tenant", 2, &["10.0.0.2"])]);
    let e = assign(&mut s, None, &[&other], &nets, &HeldIps::new(), &|_| false).unwrap_err();
    assert_eq!(e.code, "ip_in_use");
    assert!(!e.message.contains("other"), "{}", e.message);
    for bad in ["10.0.0.1", "10.0.0.7", "10.0.1.5"] {
        assert_eq!(try_ip(bad).unwrap_err().code, "invalid_request", "{bad}");
    }
    // 단순 네트워크의 주소, SDN NIC의 cloud-init 주소는 400이다.
    let mut s = spec(vec![nic("lan", 2, &["10.0.0.5"])]);
    assert_eq!(
        assign(&mut s, None, &[], &nets, &HeldIps::new(), &|_| true)
            .unwrap_err()
            .code,
        "ips_without_sdn"
    );
    // SDN NIC의 보안 그룹은 계획 66부터 받는다(OVN ACL — 기능 검사는 check_security_groups).
    let mut s = spec(vec![nic("tenant", 2, &[])]);
    s.nics[0].security_groups = vec!["web".into()];
    assign(&mut s, None, &[], &nets, &HeldIps::new(), &|_| true).unwrap();
    let mut s = spec(vec![nic("tenant", 2, &[]), nic("lan", 3, &[])]);
    let static_net = CloudInitNet {
        ipv4: Some(IpConfig::Static {
            address: "192.168.0.5/24".into(),
            gateway: None,
        }),
        ipv6: None,
    };
    s.cloud_init = Some(CloudInit {
        networks: vec![CloudInitNet::default(), static_net.clone()],
        ..CloudInit::default()
    });
    // 빈 자리 항목은 되고(뒤 단순 NIC의 설정을 주려고), 주소 항목은 400이다.
    assign(&mut s.clone(), None, &[], &nets, &HeldIps::new(), &|_| true).unwrap();
    s.cloud_init.as_mut().unwrap().networks[0] = static_net;
    assert_eq!(
        assign(&mut s, None, &[], &nets, &HeldIps::new(), &|_| true)
            .unwrap_err()
            .code,
        "cloud_init_sdn_nic"
    );
}

#[test]
fn exhausted_subnets_are_conflicts() {
    // /29의 빈 주소는 다섯(.2~.6)이다.
    let full: Vec<Vm> = (2..=6)
        .map(|i| {
            vm(
                &format!("v{i}"),
                vec![nic("tenant", i, &[&format!("10.0.0.{i}")])],
            )
        })
        .collect();
    let others: Vec<&Vm> = full.iter().collect();
    let mut s = spec(vec![nic("tenant", 9, &[])]);
    let e = assign(&mut s, None, &others, &networks(), &HeldIps::new(), &|_| {
        true
    })
    .unwrap_err();
    assert_eq!(e.code, "ip_exhausted");
    assert!(e.message.contains("10.0.0.0/29"), "{}", e.message);
}

#[test]
fn existing_nics_keep_their_addresses_and_are_not_filled() {
    let nets = networks();
    let current = spec(vec![
        nic("tenant", 1, &["10.0.0.5"]),
        nic("tenant", 2, &[]),
        nic("bare", 3, &[]),
    ]);
    // 주소 없이 온 있던 NIC는 지금 주소를 둔다. 주소가 없던 있던 NIC는 채우지 않는다(`--fill`만).
    let mut s = spec(vec![
        nic("tenant", 1, &[]),
        nic("tenant", 2, &[]),
        nic("tenant", 3, &[]),
    ]);
    assign(&mut s, Some(&current), &[], &nets, &HeldIps::new(), &|_| {
        true
    })
    .unwrap();
    assert_eq!(s.nics[0].ips, [ip("10.0.0.5")]);
    assert!(s.nics[1].ips.is_empty());
    // 네트워크를 바꾼 NIC는 새 NIC다.
    assert_eq!(s.nics[2].ips, [ip("10.0.0.2"), ip("2001:db8::2")]);
    // 있던 주소는 다시 보지 않는다(그사이 다른 VM이 같은 주소를 적은 기록이 있어도 — Raft가 막는다).
    let other = vm("other", vec![nic("tenant", 7, &["10.0.0.5"])]);
    let mut again = current.clone();
    assign(
        &mut again,
        Some(&current),
        &[&other],
        &nets,
        &HeldIps::new(),
        &|_| true,
    )
    .unwrap();
    assert_eq!(again.nics[0].ips, [ip("10.0.0.5")]);
    // 템플릿이던 스펙의 NIC(되돌리기)는 새 NIC다.
    let mut was_template = current.clone();
    was_template.template = true;
    let mut s = spec(vec![nic("tenant", 2, &[])]);
    assign(
        &mut s,
        Some(&was_template),
        &[],
        &nets,
        &HeldIps::new(),
        &|_| true,
    )
    .unwrap();
    assert_eq!(s.nics[0].ips, [ip("10.0.0.2"), ip("2001:db8::2")]);
}

#[test]
fn templates_have_no_addresses() {
    let mut s = spec(vec![nic("tenant", 1, &["10.0.0.5"])]);
    s.template = true;
    assign(&mut s, None, &[], &networks(), &HeldIps::new(), &|_| true).unwrap();
    assert!(s.nics[0].ips.is_empty());
    let mut s = spec(vec![nic("tenant", 1, &["10.0.0.5"])]);
    s.template = true;
    assert!(
        assign_new(&mut s, &[], &networks(), &HeldIps::new())
            .unwrap()
            .is_empty()
    );
    assert!(s.nics[0].ips.is_empty());
    // 템플릿의 주소는 쓴 것으로 세지 않는다.
    let t = Vm {
        spec: s,
        ..vm("t", Vec::new())
    };
    let mut n = spec(vec![nic("tenant", 2, &["10.0.0.5"])]);
    assign(&mut n, None, &[&t], &networks(), &HeldIps::new(), &|_| true).unwrap();
}

#[test]
fn restored_addresses_are_kept_when_free_and_replaced_with_a_warning() {
    let nets = networks();
    let other = vm("other", vec![nic("tenant", 1, &["10.0.0.2"])]);
    let mut s = spec(vec![
        nic("tenant", 2, &["10.0.0.5", "2001:db8::9"]),
        nic("tenant", 3, &["10.0.0.2"]),
        nic("lan", 4, &["192.168.0.4"]),
    ]);
    let warnings = assign_new(&mut s, &[&other], &nets, &HeldIps::new()).unwrap();
    assert_eq!(s.nics[0].ips, [ip("10.0.0.5"), ip("2001:db8::9")]);
    assert_eq!(s.nics[1].ips, [ip("10.0.0.3"), ip("2001:db8::2")]);
    assert!(s.nics[2].ips.is_empty());
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("10.0.0.2") && warnings[0].contains("10.0.0.3"));
    assert!(warnings[1].contains("not an SDN network"));
    // 빈 주소가 없으면 Task 오류다.
    let full: Vec<Vm> = (2..=6)
        .map(|i| {
            vm(
                &format!("v{i}"),
                vec![nic("tenant", i, &[&format!("10.0.0.{i}")])],
            )
        })
        .collect();
    let others: Vec<&Vm> = full.iter().collect();
    let mut s = spec(vec![nic("tenant", 9, &[])]);
    assert!(
        assign_new(&mut s, &others, &nets, &HeldIps::new())
            .unwrap_err()
            .contains("no free")
    );
}

#[test]
fn raft_ip_rejections_are_retried() {
    let taken = ClusterError::Rejected(Rejection::InUse {
        kind: opl_types::resource::Kind::Vm,
        field: "ip".into(),
        value: "tenant/10.0.0.5".into(),
        holder: uuid::Uuid::nil(),
    });
    assert!(is_ip_taken(&taken));
    let mac = ClusterError::Rejected(Rejection::InUse {
        kind: opl_types::resource::Kind::Vm,
        field: "mac".into(),
        value: "52:54:00:00:00:01".into(),
        holder: uuid::Uuid::nil(),
    });
    assert!(!is_ip_taken(&mac));
}

/// 복제·배포가 SDN으로 옮긴 NIC의 보안 그룹·cloud-init 주소 항목은 빠지고 경고가 남는다(리뷰 1 웹 #2).
#[test]
fn sdn_nics_of_new_vms_lose_groups_and_static_cloud_init() {
    let nets = networks();
    let mut tenant = nic("tenant", 2, &[]);
    tenant.security_groups = vec!["ssh".into()];
    let mut lan = nic("lan", 3, &[]);
    lan.security_groups = vec!["web".into()];
    let mut s = spec(vec![tenant, lan]);
    let static_net = CloudInitNet {
        ipv4: Some(IpConfig::Static {
            address: "192.168.0.5/24".into(),
            gateway: None,
        }),
        ipv6: None,
    };
    s.cloud_init = Some(CloudInit {
        networks: vec![static_net.clone(), static_net.clone()],
        ..CloudInit::default()
    });
    // 모든 노드가 sdn-routing을 알면 그룹은 그대로 두고 cloud-init 주소만 뺀다.
    let mut kept = s.clone();
    let warnings = strip_sdn_nic_config(&mut kept, &nets, true);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(kept.nics[0].security_groups, ["ssh"]);
    assert_eq!(
        kept.cloud_init.as_ref().unwrap().networks[0],
        CloudInitNet::default()
    );
    // 섞인 클러스터는 계획 65 그대로 그룹도 뺀다.
    let warnings = strip_sdn_nic_config(&mut s, &nets, false);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("ssh"), "{warnings:?}");
    assert!(s.nics[0].security_groups.is_empty());
    assert_eq!(s.nics[1].security_groups, ["web"]);
    let ci = s.cloud_init.as_ref().unwrap();
    assert_eq!(ci.networks[0], CloudInitNet::default());
    assert_eq!(ci.networks[1], static_net);
    // 이제 API 검사를 지난다.
    assign(&mut s, None, &[], &nets, &HeldIps::new(), &|_| true).unwrap();
    assert!(strip_sdn_nic_config(&mut s, &nets, false).is_empty());
}

/// 리뷰 2 N-4: 서브넷을 더하면 주소가 하나도 없는 NIC만 새 서브넷의 주소를 받고(템플릿 빼고), 모두 줄 수 없으면 모자란
/// 서브넷과 남은 수를 댄다.
#[test]
fn added_subnets_fill_only_nics_without_addresses() {
    let added: Vec<Subnet> = vec![
        serde_json::from_value(json!({ "cidr": "10.9.0.0/29" })).unwrap(),
        serde_json::from_value(json!({ "cidr": "fd09::/64", "dhcp": false })).unwrap(),
    ];
    let mut tmpl = vm("tmpl", vec![nic("t", 9, &[])]);
    tmpl.spec.template = true;
    let vms = vec![
        vm("a", vec![nic("t", 1, &[]), nic("other", 2, &[])]),
        vm("b", vec![nic("t", 3, &["10.9.0.2"])]),
        tmpl,
    ];
    assert_eq!(
        fill_shortages(&vms, &HeldIps::new(), "t", &added),
        (1, Vec::<String>::new())
    );
    let mut used = network_used(&vms, &HeldIps::new(), "t");
    let mut a = vms[0].spec.clone();
    assert_eq!(fill_empty(&mut a, "t", &added, &mut used).unwrap(), 1);
    assert_eq!(a.nics[0].ips, [ip("10.9.0.3"), ip("fd09::2")]);
    assert!(a.nics[1].ips.is_empty());
    let mut b = vms[1].spec.clone();
    assert_eq!(fill_empty(&mut b, "t", &added, &mut used).unwrap(), 0);
    let mut t = vms[2].spec.clone();
    assert_eq!(fill_empty(&mut t, "t", &added, &mut used).unwrap(), 0);

    let tight: Vec<Subnet> = vec![
        serde_json::from_value(json!({ "cidr": "10.9.0.0/29",
            "pools": [{ "start": "10.9.0.2", "end": "10.9.0.3" }] }))
        .unwrap(),
    ];
    let more = vec![
        vm("c", vec![nic("t", 4, &[])]),
        vm("d", vec![nic("t", 5, &[]), nic("t", 6, &[])]),
    ];
    assert_eq!(
        fill_shortages(&more, &HeldIps::new(), "t", &tight),
        (3, vec!["10.9.0.0/29 (2 free)".to_string()])
    );
    assert_eq!(
        fill_shortages(&more, &HeldIps::new(), "t", &[]),
        (3, Vec::<String>::new())
    );
}

/// 계획 66 §8: 라우터 외부 주소·유동 IP(`held`)도 쓴 주소이고, NIC는 유동 IP 풀을 건너뛴다.
#[test]
fn routers_and_floating_ips_hold_addresses() {
    let nets = vec![network(
        "ext",
        json!([{ "cidr": "203.0.113.0/29",
                 "floating_pools": [{ "start": "203.0.113.2", "end": "203.0.113.3" }] }]),
    )];
    let held = HeldIps::from([("ext".to_string(), [ip("203.0.113.4")].into())]);
    let mut s = spec(vec![nic("ext", 1, &[])]);
    assign(&mut s, None, &[], &nets, &held, &|_| true).unwrap();
    assert_eq!(s.nics[0].ips, [ip("203.0.113.5")]);
    let mut taken = spec(vec![nic("ext", 2, &["203.0.113.4"])]);
    let e = assign(&mut taken, None, &[], &nets, &held, &|_| true).unwrap_err();
    assert_eq!(e.code, "ip_in_use");
    assert!(e.message.contains("router or floating IP"), "{}", e.message);
    let mut s = spec(vec![nic("ext", 3, &[])]);
    let warnings = assign_new(&mut s, &[], &nets, &held).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(s.nics[0].ips, [ip("203.0.113.5")]);
    // 라우터·유동 IP의 주소 집합
    let r: opl_types::router::Router = Resource::new(
        "r1",
        serde_json::from_value(json!({ "external": { "network": "ext", "ips": ["203.0.113.2"] } }))
            .unwrap(),
    );
    let f: opl_types::floating_ip::FloatingIp = Resource::new(
        "f1",
        serde_json::from_value(json!({ "network": "ext", "address": "203.0.113.3" })).unwrap(),
    );
    let (routers, fips) = (vec![r], vec![f]);
    let all = held_ips(&routers, &fips, None);
    assert_eq!(all["ext"], [ip("203.0.113.2"), ip("203.0.113.3")].into());
    assert_eq!(
        held_ips(&routers, &fips, Some(routers[0].id))["ext"],
        [ip("203.0.113.3")].into()
    );
}

/// 계획 66 §10: SLAAC 서브넷의 NIC 주소는 MAC의 EUI-64이고 다른 주소는 400, MAC이 아직 없으면(복제 미리 보기) 비운다.
#[test]
fn slaac_subnets_use_the_mac() {
    let nets = vec![network(
        "t6",
        json!([{ "cidr": "10.6.0.0/24" }, { "cidr": "fd00:6::/64", "ipv6_mode": "slaac" }]),
    )];
    let mut s = spec(vec![nic("t6", 9, &[])]);
    assign(&mut s, None, &[], &nets, &HeldIps::new(), &|_| true).unwrap();
    assert_eq!(
        s.nics[0].ips,
        [ip("10.6.0.2"), ip("fd00:6::5054:ff:fe00:9")]
    );
    let mut bad = spec(vec![nic("t6", 9, &["fd00:6::9"])]);
    let e = assign(&mut bad, None, &[], &nets, &HeldIps::new(), &|_| true).unwrap_err();
    assert_eq!(
        e.status,
        axum::http::StatusCode::BAD_REQUEST,
        "{}",
        e.message
    );
    let mut no_mac = spec(vec![nic("t6", 9, &[])]);
    no_mac.nics[0].mac = None;
    assign(&mut no_mac, None, &[], &nets, &HeldIps::new(), &|_| true).unwrap();
    assert_eq!(no_mac.nics[0].ips, [ip("10.6.0.2")]);
    // 서브넷 더하기의 채우기도 EUI-64, 모자라지 않는다.
    let added: Vec<Subnet> = nets[0].spec.subnets[1..].to_vec();
    let vms = vec![vm("a", vec![nic("t6", 7, &[])])];
    assert_eq!(
        fill_shortages(&vms, &HeldIps::new(), "t6", &added),
        (1, Vec::<String>::new())
    );
    let mut a = vms[0].spec.clone();
    let mut used = network_used(&vms, &HeldIps::new(), "t6");
    assert_eq!(fill_empty(&mut a, "t6", &added, &mut used).unwrap(), 1);
    assert_eq!(a.nics[0].ips, [ip("fd00:6::5054:ff:fe00:7")]);
}

/// 계획 66 리뷰 2 I-1: 공급자 NIC에 준 주소는 풀 안·유동 IP 범위 밖이어야 한다. 이미 가진 주소와 오버레이 NIC는 보지 않는다.
#[test]
fn provider_nic_addresses_stay_in_the_pools() {
    let nets: Vec<Network> = vec![
        Resource::new(
            "ext",
            serde_json::from_value(serde_json::json!({
                "sdn": { "provider": { "physnet": "ext" } },
                "subnets": [{ "cidr": "203.0.113.0/24",
                    "pools": [{ "start": "203.0.113.10", "end": "203.0.113.99" }],
                    "floating_pools": [{ "start": "203.0.113.100", "end": "203.0.113.150" }] }]
            }))
            .unwrap(),
        ),
        Resource::new(
            "t1",
            serde_json::from_value(serde_json::json!({
                "sdn": {}, "subnets": [{ "cidr": "10.0.0.0/24",
                    "pools": [{ "start": "10.0.0.10", "end": "10.0.0.20" }] }]
            }))
            .unwrap(),
        ),
    ];
    let spec = |network: &str, ip: &str| -> VmSpec {
        serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt",
            "nics": [{ "network": network, "model": "virtio", "mac": "52:54:00:00:00:01", "ips": [ip] }]
        }))
        .unwrap()
    };
    assert!(check_provider_ips(&spec("ext", "203.0.113.20"), None, &nets).is_ok());
    for bad in ["203.0.113.5", "203.0.113.120", "203.0.113.250"] {
        let e = check_provider_ips(&spec("ext", bad), None, &nets).unwrap_err();
        assert!(
            e.message.contains("only a cluster administrator"),
            "{bad}: {}",
            e.message
        );
    }
    // 이미 가진 주소(같은 MAC·네트워크)는 다시 보지 않는다. 오버레이는 풀 밖도 된다(계획 65 그대로).
    let current = spec("ext", "203.0.113.5");
    assert!(check_provider_ips(&spec("ext", "203.0.113.5"), Some(&current), &nets).is_ok());
    assert!(check_provider_ips(&spec("t1", "10.0.0.50"), None, &nets).is_ok());
}
