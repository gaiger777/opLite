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
    assign(&mut s, None, &[&other], &networks(), &|_| true).unwrap();
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
        assign(&mut s, None, &[&other], &nets, &|_| true).map(|()| s.nics[0].ips.clone())
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
    let e = assign(&mut s, None, &[&other], &nets, &|_| false).unwrap_err();
    assert_eq!(e.code, "ip_in_use");
    assert!(!e.message.contains("other"), "{}", e.message);
    for bad in ["10.0.0.1", "10.0.0.7", "10.0.1.5"] {
        assert_eq!(try_ip(bad).unwrap_err().code, "invalid_request", "{bad}");
    }
    // 단순 네트워크의 주소, SDN NIC의 보안 그룹·cloud-init 주소는 400이다.
    let mut s = spec(vec![nic("lan", 2, &["10.0.0.5"])]);
    assert_eq!(
        assign(&mut s, None, &[], &nets, &|_| true)
            .unwrap_err()
            .code,
        "ips_without_sdn"
    );
    let mut s = spec(vec![nic("tenant", 2, &[])]);
    s.nics[0].security_groups = vec!["web".into()];
    assert_eq!(
        assign(&mut s, None, &[], &nets, &|_| true)
            .unwrap_err()
            .code,
        "sdn_security_groups"
    );
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
    assign(&mut s.clone(), None, &[], &nets, &|_| true).unwrap();
    s.cloud_init.as_mut().unwrap().networks[0] = static_net;
    assert_eq!(
        assign(&mut s, None, &[], &nets, &|_| true)
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
    let e = assign(&mut s, None, &others, &networks(), &|_| true).unwrap_err();
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
    assign(&mut s, Some(&current), &[], &nets, &|_| true).unwrap();
    assert_eq!(s.nics[0].ips, [ip("10.0.0.5")]);
    assert!(s.nics[1].ips.is_empty());
    // 네트워크를 바꾼 NIC는 새 NIC다.
    assert_eq!(s.nics[2].ips, [ip("10.0.0.2"), ip("2001:db8::2")]);
    // 있던 주소는 다시 보지 않는다(그사이 다른 VM이 같은 주소를 적은 기록이 있어도 — Raft가 막는다).
    let other = vm("other", vec![nic("tenant", 7, &["10.0.0.5"])]);
    let mut again = current.clone();
    assign(&mut again, Some(&current), &[&other], &nets, &|_| true).unwrap();
    assert_eq!(again.nics[0].ips, [ip("10.0.0.5")]);
    // 템플릿이던 스펙의 NIC(되돌리기)는 새 NIC다.
    let mut was_template = current.clone();
    was_template.template = true;
    let mut s = spec(vec![nic("tenant", 2, &[])]);
    assign(&mut s, Some(&was_template), &[], &nets, &|_| true).unwrap();
    assert_eq!(s.nics[0].ips, [ip("10.0.0.2"), ip("2001:db8::2")]);
}

#[test]
fn templates_have_no_addresses() {
    let mut s = spec(vec![nic("tenant", 1, &["10.0.0.5"])]);
    s.template = true;
    assign(&mut s, None, &[], &networks(), &|_| true).unwrap();
    assert!(s.nics[0].ips.is_empty());
    let mut s = spec(vec![nic("tenant", 1, &["10.0.0.5"])]);
    s.template = true;
    assert!(assign_new(&mut s, &[], &networks()).unwrap().is_empty());
    assert!(s.nics[0].ips.is_empty());
    // 템플릿의 주소는 쓴 것으로 세지 않는다.
    let t = Vm {
        spec: s,
        ..vm("t", Vec::new())
    };
    let mut n = spec(vec![nic("tenant", 2, &["10.0.0.5"])]);
    assign(&mut n, None, &[&t], &networks(), &|_| true).unwrap();
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
    let warnings = assign_new(&mut s, &[&other], &nets).unwrap();
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
        assign_new(&mut s, &others, &nets)
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
    let warnings = strip_sdn_nic_config(&mut s, &nets);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("ssh"), "{warnings:?}");
    assert!(s.nics[0].security_groups.is_empty());
    assert_eq!(s.nics[1].security_groups, ["web"]);
    let ci = s.cloud_init.as_ref().unwrap();
    assert_eq!(ci.networks[0], CloudInitNet::default());
    assert_eq!(ci.networks[1], static_net);
    // 이제 API 검사를 지난다.
    assign(&mut s, None, &[], &nets, &|_| true).unwrap();
    assert!(strip_sdn_nic_config(&mut s, &nets).is_empty());
}
