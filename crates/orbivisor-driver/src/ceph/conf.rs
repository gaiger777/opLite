//! Orbivisor가 쓰는 Ceph 설정 파일: 데몬 노드의 `/etc/ceph/ceph.conf`, 모든 노드의 클라이언트 설정
//! (`{client_dir}/ceph.conf`), 키링.

use std::net::IpAddr;

use orbivisor_types::ceph::{CLIENT_ID, CephMon, CephSpec};

/// Orbivisor가 쓴 설정 파일의 첫 줄. 이 줄이 없는 `/etc/ceph/ceph.conf`는 남의 것이라 건드리지 않는다.
pub const CONF_MARK: &str = "# managed by orbivisord; edits are overwritten";

/// cephx를 켜는 세 줄
const AUTH: &str = "auth_cluster_required = cephx\n\
                    auth_service_required = cephx\n\
                    auth_client_required = cephx\n";

/// mon 하나의 주소 벡터(메신저 v2+v1): `[v2:10.0.0.1:3300,v1:10.0.0.1:6789]`, IPv6는 `[v2:[fd00::1]:3300,…]`
pub fn mon_addrv(ip: IpAddr) -> String {
    let host = match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    format!("[v2:{host}:3300,v1:{host}:6789]")
}

/// `mon_host` 값: mon들(과 아직 정족수에 들지 않은 `extra`)을 이름 순으로, 빈칸으로 잇는다.
fn mon_host(mons: &[CephMon], extra: Option<&CephMon>) -> String {
    let mut all: Vec<&CephMon> = mons.iter().collect();
    if let Some(extra) = extra.filter(|e| !mons.iter().any(|m| m.name == e.name)) {
        all.push(extra);
    }
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all.iter()
        .map(|m| mon_addrv(m.addr))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 표시·`[global]`·fsid·mon_host
fn global(spec: &CephSpec, mons: &[CephMon], extra: Option<&CephMon>) -> String {
    format!(
        "{CONF_MARK}\n[global]\nfsid = {}\nmon_host = {}\n",
        spec.fsid,
        mon_host(mons, extra)
    )
}

/// 데몬 노드의 `/etc/ceph/ceph.conf`. `extra`는 아직 정족수에 들지 않은 새 mon(그 노드의 conf에만)이다.
pub fn render_conf(spec: &CephSpec, mons: &[CephMon], extra: Option<&CephMon>) -> String {
    let mut text = global(spec, mons, extra);
    text.push_str(&format!("public_network = {}\n", spec.public_network));
    if let Some(net) = spec.cluster_network {
        text.push_str(&format!("cluster_network = {net}\n"));
    }
    text.push_str(AUTH);
    text.push_str("[mon]\nauth_allow_insecure_global_id_reclaim = false\n");
    text
}

/// 노드가 RBD 풀에 붙는 클라이언트 설정(`{client_dir}/ceph.conf`). `keyring`은 `client.orbivisor` 키링의
/// 경로다.
pub fn render_client_conf(spec: &CephSpec, mons: &[CephMon], keyring: &str) -> String {
    let mut text = global(spec, mons, None);
    text.push_str(AUTH);
    text.push_str(&format!("[client.{CLIENT_ID}]\nkeyring = {keyring}\n"));
    text
}

/// 키링 항목 하나: 이름(`mon.`, `client.admin`), 키, caps(`("mon", "allow *")` 꼴, 이 차례로)
pub type KeyringEntry<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

/// `ceph-authtool`과 같은 모양의 키링(탭 들여쓰기, `caps mon = "profile rbd"`)
pub fn render_keyring(entries: &[KeyringEntry<'_>]) -> String {
    let mut text = String::new();
    for (entity, key, caps) in entries {
        text.push_str(&format!("[{entity}]\n\tkey = {key}\n"));
        for (service, cap) in *caps {
            text.push_str(&format!("\tcaps {service} = \"{cap}\"\n"));
        }
    }
    text
}

/// Orbivisor가 쓴 설정 파일인지(첫 줄이 [`CONF_MARK`])
pub fn is_managed(text: &str) -> bool {
    text.lines().next().map(str::trim_end) == Some(CONF_MARK)
}

#[cfg(test)]
mod tests {
    use orbivisor_types::ceph::CephKeys;
    use uuid::Uuid;

    use super::*;

    const FSID: &str = "18796e2f-112c-44cf-9d48-09f825f0a14b";

    fn spec(public: &str, cluster: Option<&str>) -> CephSpec {
        CephSpec {
            fsid: FSID.parse().unwrap(),
            public_network: public.parse().unwrap(),
            cluster_network: cluster.map(|c| c.parse().unwrap()),
            keys: CephKeys {
                mon: "m".into(),
                admin: "a".into(),
                bootstrap_osd: "b".into(),
                client: "c".into(),
            },
        }
    }

    fn mon(name: &str, addr: &str) -> CephMon {
        CephMon {
            node: Uuid::nil(),
            name: name.into(),
            addr: addr.parse().unwrap(),
        }
    }

    #[test]
    fn mon_address_vectors() {
        assert_eq!(
            mon_addrv("10.0.0.1".parse().unwrap()),
            "[v2:10.0.0.1:3300,v1:10.0.0.1:6789]"
        );
        assert_eq!(
            mon_addrv("fd00::1".parse().unwrap()),
            "[v2:[fd00::1]:3300,v1:[fd00::1]:6789]"
        );
    }

    #[test]
    fn renders_the_daemon_conf_with_one_monitor() {
        let conf = render_conf(
            &spec("10.0.0.0/24", None),
            &[mon("node1", "10.0.0.1")],
            None,
        );
        assert_eq!(
            conf,
            "# managed by orbivisord; edits are overwritten\n\
             [global]\n\
             fsid = 18796e2f-112c-44cf-9d48-09f825f0a14b\n\
             mon_host = [v2:10.0.0.1:3300,v1:10.0.0.1:6789]\n\
             public_network = 10.0.0.0/24\n\
             auth_cluster_required = cephx\n\
             auth_service_required = cephx\n\
             auth_client_required = cephx\n\
             [mon]\n\
             auth_allow_insecure_global_id_reclaim = false\n"
        );
        assert!(is_managed(&conf));
    }

    /// mon은 이름 순이고 `cluster_network`는 있을 때만이다.
    #[test]
    fn renders_three_monitors_in_name_order_and_the_cluster_network() {
        let mons = [
            mon("node3", "10.0.0.3"),
            mon("node1", "10.0.0.1"),
            mon("node2", "10.0.0.2"),
        ];
        let conf = render_conf(&spec("10.0.0.0/24", Some("10.1.0.0/24")), &mons, None);
        assert_eq!(
            conf,
            "# managed by orbivisord; edits are overwritten\n\
             [global]\n\
             fsid = 18796e2f-112c-44cf-9d48-09f825f0a14b\n\
             mon_host = [v2:10.0.0.1:3300,v1:10.0.0.1:6789] [v2:10.0.0.2:3300,v1:10.0.0.2:6789] \
             [v2:10.0.0.3:3300,v1:10.0.0.3:6789]\n\
             public_network = 10.0.0.0/24\n\
             cluster_network = 10.1.0.0/24\n\
             auth_cluster_required = cephx\n\
             auth_service_required = cephx\n\
             auth_client_required = cephx\n\
             [mon]\n\
             auth_allow_insecure_global_id_reclaim = false\n"
        );
    }

    /// 새 mon(`extra`)은 그 노드의 conf에 더한다. 이미 mon이면 두 번 넣지 않는다.
    #[test]
    fn renders_an_extra_monitor_and_ipv6() {
        let s = spec("fd00::/64", None);
        let mons = [mon("node2", "fd00::2")];
        let conf = render_conf(&s, &mons, Some(&mon("node1", "fd00::1")));
        assert!(
            conf.contains(
                "\nmon_host = [v2:[fd00::1]:3300,v1:[fd00::1]:6789] \
                 [v2:[fd00::2]:3300,v1:[fd00::2]:6789]\npublic_network = fd00::/64\n"
            ),
            "{conf}"
        );
        assert_eq!(
            render_conf(&s, &mons, Some(&mons[0])),
            render_conf(&s, &mons, None)
        );
    }

    #[test]
    fn renders_the_client_conf_with_its_keyring() {
        let conf = render_client_conf(
            &spec("10.0.0.0/24", Some("10.1.0.0/24")),
            &[mon("node2", "10.0.0.2"), mon("node1", "10.0.0.1")],
            "/etc/opl/ceph/ceph.client.orbivisor.keyring",
        );
        assert_eq!(
            conf,
            "# managed by orbivisord; edits are overwritten\n\
             [global]\n\
             fsid = 18796e2f-112c-44cf-9d48-09f825f0a14b\n\
             mon_host = [v2:10.0.0.1:3300,v1:10.0.0.1:6789] [v2:10.0.0.2:3300,v1:10.0.0.2:6789]\n\
             auth_cluster_required = cephx\n\
             auth_service_required = cephx\n\
             auth_client_required = cephx\n\
             [client.orbivisor]\n\
             keyring = /etc/opl/ceph/ceph.client.orbivisor.keyring\n"
        );
    }

    /// 키링은 항목·caps를 받은 차례로, 탭 들여쓰기로 쓴다.
    #[test]
    fn renders_keyrings_like_ceph_authtool() {
        let text = render_keyring(&[
            ("mon.", "AQmon==", &[("mon", "allow *")]),
            (
                "client.orbivisor",
                "AQclient==",
                &[
                    ("mon", "profile rbd"),
                    ("osd", "profile rbd"),
                    ("mgr", "profile rbd"),
                ],
            ),
            ("client.x", "AQx==", &[]),
        ]);
        assert_eq!(
            text,
            "[mon.]\n\
             \tkey = AQmon==\n\
             \tcaps mon = \"allow *\"\n\
             [client.orbivisor]\n\
             \tkey = AQclient==\n\
             \tcaps mon = \"profile rbd\"\n\
             \tcaps osd = \"profile rbd\"\n\
             \tcaps mgr = \"profile rbd\"\n\
             [client.x]\n\
             \tkey = AQx==\n"
        );
        assert_eq!(render_keyring(&[]), "");
    }

    #[test]
    fn managed_conf_has_the_mark_on_its_first_line() {
        assert!(is_managed(&format!("{CONF_MARK}\n[global]\n")));
        assert!(is_managed(&format!("{CONF_MARK}\r\n")));
        assert!(is_managed(CONF_MARK));
        // 표시가 첫 줄이 아니면 사람이 고친(또는 남의) 파일이다.
        assert!(!is_managed(&format!("[global]\n{CONF_MARK}\n")));
        assert!(!is_managed(&format!("\n{CONF_MARK}\n")));
        assert!(!is_managed("[global]\nfsid = x\n"));
        assert!(!is_managed(""));
    }
}
