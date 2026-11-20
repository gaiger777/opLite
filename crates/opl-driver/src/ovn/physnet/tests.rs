//! physnet 연결의 순수 함수와 [`OvnHost::reconcile_physnets`](가짜 실행기)

use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::ceph::runner::{CmdOutput, FakeRunner};
use crate::ovn::{OvnOptions, OvnPaths};

const TAG: &str = "abcdef012345";

fn lines(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter().map(Cmd::line).collect()
}

fn bridge(mtu: u32, vlan_filtering: bool) -> LinkFacts {
    LinkFacts {
        mtu: Some(mtu),
        up: true,
        bridge: true,
        vlan_filtering,
        ..LinkFacts::default()
    }
}

fn port(master: Option<&str>, mtu: u32) -> LinkFacts {
    LinkFacts {
        mtu: Some(mtu),
        master: master.map(str::to_string),
        up: true,
        ..LinkFacts::default()
    }
}

/// 처음 잇기: veth 짝, Linux 브리지에 매달기(MTU = 브리지), VLAN 인식이면 트렁크, OVS 브리지·포트와 표시
#[test]
fn commands_for_a_new_physnet() {
    let (o, b, x) = physnet_ifnames(TAG, "ext");
    let cmds = physnet_commands(TAG, "ext", "vmbr1", &bridge(9000, true), None, None, false);
    assert_eq!(
        lines(&cmds),
        [
            format!("ip link add {b} type veth peer name {o}"),
            format!("ip link set dev {b} master vmbr1 mtu 9000 up"),
            format!("ip link set dev {o} mtu 9000 up"),
            format!("bridge vlan add dev {b} vid 2-4094"),
            format!(
                "ovs-vsctl --may-exist add-br {x} -- set Bridge {x} external_ids:orbivisor-cluster={TAG} \
                 external_ids:orbivisor-physnet=ext -- --may-exist add-port {x} {o} -- set Interface {o} \
                 external_ids:orbivisor-cluster={TAG}"
            ),
        ]
    );
    // VLAN 인식이 아닌 브리지는 트렁크를 열지 않는다.
    let cmds = physnet_commands(TAG, "ext", "vmbr1", &bridge(1500, false), None, None, true);
    assert_eq!(
        lines(&cmds),
        [
            format!("ip link add {b} type veth peer name {o}"),
            format!("ip link set dev {b} master vmbr1 mtu 1500 up"),
            format!("ip link set dev {o} mtu 1500 up"),
        ]
    );
}

/// 다 된 연결은 명령이 없다. 브리지 MTU가 바뀌면 MTU만, 다른 브리지에 매달려 있으면 다시 매단다.
#[test]
fn commands_follow_the_current_state() {
    let (o, b, _) = physnet_ifnames(TAG, "ext");
    let br = bridge(1500, true);
    let done = physnet_commands(
        TAG,
        "ext",
        "vmbr1",
        &br,
        Some(&port(Some("vmbr1"), 1500)),
        Some(&port(None, 1500)),
        true,
    );
    assert!(done.is_empty(), "{:?}", lines(&done));

    let mtu = physnet_commands(
        TAG,
        "ext",
        "vmbr1",
        &bridge(9000, true),
        Some(&port(Some("vmbr1"), 1500)),
        Some(&port(None, 1500)),
        true,
    );
    assert_eq!(
        lines(&mtu),
        [
            format!("ip link set dev {b} master vmbr1 mtu 9000 up"),
            format!("ip link set dev {o} mtu 9000 up"),
        ]
    );

    let moved = physnet_commands(
        TAG,
        "ext",
        "vmbr1",
        &br,
        Some(&port(Some("vmbr0"), 1500)),
        Some(&port(None, 1500)),
        true,
    );
    assert_eq!(
        lines(&moved),
        [
            format!("ip link set dev {b} master vmbr1 mtu 1500 up"),
            format!("bridge vlan add dev {b} vid 2-4094"),
        ]
    );
}

/// 매핑 합치기: 남의 항목은 차례 그대로 두고, 내 항목은 지금 원하는 것만, 남이 가진 physnet은 덮지 않는다.
#[test]
fn mappings_keep_foreign_entries() {
    let (_, _, x) = physnet_ifnames(TAG, "ext");
    let (_, _, old) = physnet_ifnames(TAG, "gone");
    let ours = BTreeSet::from([x.clone(), old.clone()]);
    let want = BTreeMap::from([("ext".to_string(), x.clone())]);

    let m = merge_mappings("", &ours, &want);
    assert_eq!(m.text, format!("ext:{x}"));
    assert!(m.conflicts.is_empty());

    let m = merge_mappings(
        &format!("physnet1:br-ex,gone:{old},ext:{x},lan:br-lan"),
        &ours,
        &want,
    );
    assert_eq!(m.text, format!("physnet1:br-ex,lan:br-lan,ext:{x}"));
    // 같은 입력이면 같은 답(다시 쓰지 않음)
    assert_eq!(merge_mappings(&m.text, &ours, &want).text, m.text);

    // 다른 클러스터 모양의 이름도 내 브리지가 아니면 남의 것이다.
    let (_, _, foreign) = physnet_ifnames("ba9876543210", "dmz");
    let m = merge_mappings(&format!("dmz:{foreign}"), &ours, &BTreeMap::new());
    assert_eq!(m.text, format!("dmz:{foreign}"));

    let m = merge_mappings("ext:br-ex", &ours, &want);
    assert_eq!(m.text, "ext:br-ex");
    assert_eq!(
        m.conflicts,
        BTreeMap::from([("ext".to_string(), "br-ex".to_string())])
    );

    // 다 빼면 빈 값(칸을 지운다)
    assert_eq!(
        merge_mappings(&format!("ext:{x}"), &ours, &BTreeMap::new()).text,
        ""
    );
}

#[test]
fn cms_options_touch_only_the_gateway_word() {
    assert_eq!(merge_cms_options("", true), "enable-chassis-as-gw");
    assert_eq!(
        merge_cms_options("availability-zones=az0:az1", true),
        "availability-zones=az0:az1,enable-chassis-as-gw"
    );
    assert_eq!(
        merge_cms_options("enable-chassis-as-gw,availability-zones=az0", false),
        "availability-zones=az0"
    );
    assert_eq!(merge_cms_options("enable-chassis-as-gw", false), "");
    assert!(has_gateway_option("a,enable-chassis-as-gw"));
    assert!(!has_gateway_option("a"));
}

#[test]
fn links_are_read_without_details() {
    let links = json!([
        {"ifindex":2,"ifname":"eth0","flags":["UP"],"mtu":1500,"master":"vmbr1"},
        {"ifindex":3,"ifname":"vmbr1","flags":["BROADCAST","UP"],"mtu":9000},
        {"ifindex":4,"ifname":"oplpdeadbeefb","flags":["BROADCAST"],"mtu":1500,"master":"vmbr1"}
    ]);
    let bridges = json!([
        {"ifindex":3,"ifname":"vmbr1","mtu":9000,"linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":1}}}
    ]);
    let parsed = parse_links(links.to_string().as_bytes(), bridges.to_string().as_bytes()).unwrap();
    assert_eq!(parsed["vmbr1"], bridge(9000, true));
    assert_eq!(
        parsed["oplpdeadbeefb"],
        LinkFacts {
            mtu: Some(1500),
            master: Some("vmbr1".into()),
            ..LinkFacts::default()
        }
    );
    assert!(!parsed["eth0"].bridge);
    assert!(parse_links(b"", b"\n").unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// OvnHost::reconcile_physnets
// ---------------------------------------------------------------------------------------------------------------

struct Env {
    _dir: tempfile::TempDir,
    fake: Arc<FakeRunner>,
    host: OvnHost,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = OvnPaths {
            etc: dir.path().join("etc"),
            ..OvnPaths::default()
        };
        let options = OvnOptions {
            any_os: true,
            ..OvnOptions::default()
        };
        let fake = Arc::new(FakeRunner::new());
        let host = OvnHost::new(fake.clone(), paths, options, TAG.into());
        Self {
            _dir: dir,
            fake,
            host,
        }
    }

    fn ids(&self, pairs: &[(&str, &str)]) {
        let map: Vec<serde_json::Value> = pairs.iter().map(|(k, v)| json!([k, v])).collect();
        self.fake.reply(
            "ovs-vsctl --format=json --columns=external_ids",
            CmdOutput::ok(
                json!({ "data": [[["map", map]]], "headings": ["external_ids"] }).to_string(),
            ),
        );
    }

    /// 내 태그가 있는 OVS 브리지들(이름, physnet)과 포트들
    fn ovs(&self, bridges: &[(&str, &str)], ports: &[&str]) {
        let rows: Vec<serde_json::Value> = bridges
            .iter()
            .map(|(name, physnet)| {
                json!([
                    name,
                    [
                        "map",
                        [["orbivisor-cluster", TAG], ["orbivisor-physnet", physnet]]
                    ]
                ])
            })
            .collect();
        self.fake.reply(
            "ovs-vsctl --format=json --columns=name,external_ids find Bridge",
            CmdOutput::ok(
                json!({ "data": rows, "headings": ["name", "external_ids"] }).to_string(),
            ),
        );
        let rows: Vec<serde_json::Value> = ports
            .iter()
            .map(|name| json!([name, 1, ["map", [["orbivisor-cluster", TAG]]]]))
            .collect();
        self.fake.reply(
            "ovs-vsctl --format=json --columns=name,ofport,external_ids find Interface",
            CmdOutput::ok(
                json!({ "data": rows, "headings": ["name", "ofport", "external_ids"] }).to_string(),
            ),
        );
    }

    fn links(&self, links: serde_json::Value, bridges: serde_json::Value) {
        self.fake
            .reply("ip -j link show", CmdOutput::ok(links.to_string()));
        self.fake.reply(
            "ip -j -d link show type bridge",
            CmdOutput::ok(bridges.to_string()),
        );
    }

    /// 읽기 명령을 뺀 명령(바꾸는 것)
    fn writes(&self) -> Vec<String> {
        self.fake
            .calls()
            .into_iter()
            .filter(|c| {
                !c.starts_with("ovs-vsctl --format=json")
                    && !c.starts_with("ip -j")
                    && !c.starts_with("ovs-vsctl list-ports")
            })
            .collect()
    }
}

fn config(physnets: &[(&str, &str)], gateway: Option<u16>) -> NodeSdnConfig {
    NodeSdnConfig {
        physnets: physnets
            .iter()
            .map(|(p, b)| (p.to_string(), b.to_string()))
            .collect(),
        gateway,
    }
}

fn vmbr1() -> serde_json::Value {
    json!([{"ifname":"vmbr1","mtu":1500,"linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":0}}}])
}

/// 처음 잇기(veth·OVS 브리지·매핑·게이트웨이 표시), 다 된 뒤 다시 부르면 아무것도 바꾸지 않는다.
#[tokio::test]
async fn connects_a_physnet_and_marks_the_gateway() {
    let env = Env::new();
    let (o, b, x) = physnet_ifnames(TAG, "ext");
    env.ids(&[
        (TAG_KEY, TAG),
        ("ovn-bridge-mappings", "physnet1:br-ex"),
        ("ovn-cms-options", "availability-zones=az0"),
    ]);
    env.ovs(&[], &[]);
    env.links(
        json!([{"ifname":"eth1","mtu":1500,"master":"vmbr1","flags":["UP"]},{"ifname":"vmbr1","mtu":1500,"flags":["UP"]}]),
        vmbr1(),
    );
    let report = env
        .host
        .reconcile_physnets(Some(&config(&[("ext", "vmbr1")], Some(100))))
        .await
        .unwrap();
    assert_eq!(
        report,
        PhysnetReport {
            physnets: vec![PhysnetState {
                name: "ext".into(),
                bridge: "vmbr1".into(),
                ready: true,
                error: None,
            }],
            gateway: true,
            errors: Vec::new(),
        }
    );
    let writes = env.writes();
    assert_eq!(
        writes,
        [
            format!("ip link add {b} type veth peer name {o}"),
            format!("ip link set dev {b} master vmbr1 mtu 1500 up"),
            format!("ip link set dev {o} mtu 1500 up"),
            format!(
                "ovs-vsctl --may-exist add-br {x} -- set Bridge {x} external_ids:orbivisor-cluster={TAG} \
                 external_ids:orbivisor-physnet=ext -- --may-exist add-port {x} {o} -- set Interface {o} \
                 external_ids:orbivisor-cluster={TAG}"
            ),
            format!(
                "ovs-vsctl set Open_vSwitch . external_ids:ovn-bridge-mappings=\"physnet1:br-ex,ext:{x}\""
            ),
            "ovs-vsctl set Open_vSwitch . external_ids:ovn-cms-options=\"availability-zones=az0,enable-chassis-as-gw\"".to_string(),
        ]
    );
    // 관리자의 NIC(eth1)는 건드리지 않는다.
    assert!(writes.iter().all(|w| !w.contains("eth1")), "{writes:?}");

    // 다 된 상태
    let env = Env::new();
    env.ids(&[
        (TAG_KEY, TAG),
        ("ovn-bridge-mappings", &format!("physnet1:br-ex,ext:{x}")),
        ("ovn-cms-options", "enable-chassis-as-gw"),
    ]);
    env.ovs(&[(&x, "ext")], &[&o]);
    env.links(
        json!([
            {"ifname":"vmbr1","mtu":1500,"flags":["UP"]},
            {"ifname":b,"mtu":1500,"master":"vmbr1","flags":["UP"]},
            {"ifname":o,"mtu":1500,"master":"ovs-system","flags":["UP"]}
        ]),
        vmbr1(),
    );
    let report = env
        .host
        .reconcile_physnets(Some(&config(&[("ext", "vmbr1")], Some(100))))
        .await
        .unwrap();
    assert!(report.physnets[0].ready && report.gateway, "{report:?}");
    assert!(env.writes().is_empty(), "{:?}", env.writes());
}

/// 남이 가진 physnet은 덮지 않고, 없는 브리지·브리지가 아닌 장치는 준비되지 않음으로 알린다(아무것도 만들지 않음).
#[tokio::test]
async fn refuses_foreign_mappings_and_missing_bridges() {
    let env = Env::new();
    env.ids(&[(TAG_KEY, TAG), ("ovn-bridge-mappings", "ext:br-ex")]);
    env.ovs(&[], &[]);
    env.links(json!([{"ifname":"eth9","mtu":1500}]), vmbr1());
    let report = env
        .host
        .reconcile_physnets(Some(&config(
            &[("ext", "vmbr1"), ("dmz", "vmbr9"), ("lan", "eth9")],
            None,
        )))
        .await
        .unwrap();
    let errors: Vec<(String, bool, Option<String>)> = report
        .physnets
        .iter()
        .map(|p| (p.name.clone(), p.ready, p.error.clone()))
        .collect();
    assert_eq!(
        errors,
        [
            (
                "dmz".to_string(),
                false,
                Some("Linux bridge vmbr9 does not exist".to_string())
            ),
            (
                "ext".to_string(),
                false,
                Some("physnet ext is mapped to br-ex by someone else".to_string())
            ),
            (
                "lan".to_string(),
                false,
                Some("eth9 is not a Linux bridge".to_string())
            ),
        ]
    );
    assert!(!report.gateway);
    assert!(env.writes().is_empty(), "{:?}", env.writes());
}

/// 빼기: 매핑 항목 → veth → OVS 브리지, 게이트웨이 낱말. 남의 항목·옵션과 남의 브리지는 그대로다.
#[tokio::test]
async fn removes_only_its_own_physnets() {
    let env = Env::new();
    let (o, b, x) = physnet_ifnames(TAG, "ext");
    env.ids(&[
        (TAG_KEY, TAG),
        ("ovn-bridge-mappings", &format!("ext:{x},physnet1:br-ex")),
        ("ovn-cms-options", "enable-chassis-as-gw,foo"),
    ]);
    // 태그가 같아도 physnet 모양이 아닌 브리지(br-ex)는 내 것이 아니다.
    env.ovs(&[(&x, "ext"), ("br-ex", "physnet1")], &[&o]);
    env.links(
        json!([{"ifname":b,"mtu":1500,"master":"vmbr1","flags":["UP"]},{"ifname":"vmbr1","mtu":1500}]),
        vmbr1(),
    );
    let report = env.host.reconcile_physnets(None).await.unwrap();
    assert_eq!(report, PhysnetReport::default());
    assert_eq!(
        env.writes(),
        [
            "ovs-vsctl set Open_vSwitch . external_ids:ovn-bridge-mappings=\"physnet1:br-ex\""
                .to_string(),
            format!("ip link del dev {b}"),
            format!("ovs-vsctl --if-exists del-br {x}"),
            "ovs-vsctl set Open_vSwitch . external_ids:ovn-cms-options=\"foo\"".to_string(),
        ]
    );

    // 마지막 항목이면 칸을 지운다. veth가 이미 없으면 브리지만.
    let env = Env::new();
    env.ids(&[
        (TAG_KEY, TAG),
        ("ovn-bridge-mappings", &format!("ext:{x}")),
        ("ovn-cms-options", "enable-chassis-as-gw"),
    ]);
    env.ovs(&[(&x, "ext")], &[]);
    env.links(json!([]), json!([]));
    env.host.reconcile_physnets(None).await.unwrap();
    assert_eq!(
        env.writes(),
        [
            "ovs-vsctl remove Open_vSwitch . external_ids ovn-bridge-mappings".to_string(),
            format!("ovs-vsctl --if-exists del-br {x}"),
            "ovs-vsctl remove Open_vSwitch . external_ids ovn-cms-options".to_string(),
        ]
    );
}

/// Linux 브리지가 잠시 없어도 이미 있는 내 연결과 매핑은 그대로 둔다(빼지 않음). 명령이 실패하면 그 physnet만 준비되지
/// 않음이다.
#[tokio::test]
async fn keeps_an_existing_connection_and_reports_failures() {
    let env = Env::new();
    let (o, _, x) = physnet_ifnames(TAG, "ext");
    env.ids(&[(TAG_KEY, TAG), ("ovn-bridge-mappings", &format!("ext:{x}"))]);
    env.ovs(&[(&x, "ext")], &[&o]);
    env.links(json!([]), json!([]));
    let report = env
        .host
        .reconcile_physnets(Some(&config(&[("ext", "vmbr1")], None)))
        .await
        .unwrap();
    assert_eq!(
        report.physnets[0].error.as_deref(),
        Some("Linux bridge vmbr1 does not exist")
    );
    assert!(env.writes().is_empty(), "{:?}", env.writes());

    let env = Env::new();
    env.ids(&[(TAG_KEY, TAG)]);
    env.ovs(&[], &[]);
    env.links(json!([{"ifname":"vmbr1","mtu":1500}]), vmbr1());
    env.fake.reply(
        "ip link add",
        CmdOutput::fail(2, "RTNETLINK answers: Operation not permitted"),
    );
    let report = env
        .host
        .reconcile_physnets(Some(&config(&[("ext", "vmbr1")], None)))
        .await
        .unwrap();
    assert!(!report.physnets[0].ready);
    assert!(
        report.physnets[0]
            .error
            .as_deref()
            .unwrap()
            .contains("Operation not permitted"),
        "{report:?}"
    );
    // 실패한 physnet은 매핑하지 않는다.
    assert!(
        env.writes()
            .iter()
            .all(|w| !w.contains("ovn-bridge-mappings")),
        "{:?}",
        env.writes()
    );
}

/// 클러스터에서 빠지면 physnet 연결과 게이트웨이 표시도 뺀다. 장치 없는 physnet veth는 오래된 br-int 포트로 지우지
/// 않는다(그 OVS 브리지의 포트다).
#[tokio::test]
async fn leaving_removes_physnets_and_stale_cleanup_skips_them() {
    let env = Env::new();
    let (o, b, x) = physnet_ifnames(TAG, "ext");
    env.ids(&[
        (TAG_KEY, TAG),
        ("ovn-bridge-mappings", &format!("ext:{x}")),
        ("ovn-cms-options", "enable-chassis-as-gw"),
    ]);
    env.ovs(&[(&x, "ext")], &[&o]);
    env.links(json!([{"ifname":b,"mtu":1500,"master":"vmbr1"}]), json!([]));
    assert!(env.host.unconfigure_chassis().await.unwrap());
    assert_eq!(
        env.writes(),
        [
            "ovs-vsctl remove Open_vSwitch . external_ids ovn-bridge-mappings".to_string(),
            format!("ip link del dev {b}"),
            format!("ovs-vsctl --if-exists del-br {x}"),
            "ovs-vsctl remove Open_vSwitch . external_ids ovn-cms-options".to_string(),
            format!("ovs-vsctl remove Open_vSwitch . external_ids {TAG_KEY} ovn-remote"),
            "systemctl stop ovn-controller.service".to_string(),
        ]
    );

    let gone = |name: &str| crate::ovn::OvsInterface {
        name: name.into(),
        ofport: Some(-1),
        external_ids: BTreeMap::new(),
    };
    assert_eq!(
        crate::ovn::stale_ports(&[gone(&o), gone("optap1")], &BTreeSet::new()),
        ["optap1"]
    );
}
