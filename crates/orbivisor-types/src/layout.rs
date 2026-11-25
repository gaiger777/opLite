//! 노드 디렉터리 배치의 옛 이름과 새 이름(계획 74 설계 §3.2·§3.4). 데몬은 시작할 때 옛 디렉터리를 새 이름으로 옮기고
//! 옛 이름을 링크로 남긴다(`orbivisord::layout`). 그러므로 새 판 노드에서는 두 이름이 같은 곳이고, 옛 판 노드에는 옛
//! 이름만 있다.
//!
//! 다른 노드가 여는 경로를 클러스터 저장소에 적을 때는 모든 노드가 기능
//! [`ORBIVISOR_LAYOUT`](crate::node::ORBIVISOR_LAYOUT)을 알릴 때까지 옛 이름([`to_legacy`])으로 적는다. 저장된 경로를
//! 견줄 때는 [`same_path`]로 견준다(이름 바꾸기 앞에 저장된 값이 남아 있다).

use std::borrow::Cow;

/// (옛, 새) 디렉터리 짝
pub const LAYOUT: [(&str, &str); 4] = [
    ("/etc/opl", "/etc/orbivisor"),
    ("/var/lib/opl", "/var/lib/orbivisor"),
    ("/run/opl", "/run/orbivisor"),
    ("/var/log/opl", "/var/log/orbivisor"),
];

/// `path`가 디렉터리 `dir`이거나 그 아래면 나머지(`""` 또는 `/…`)
fn under<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    path.strip_prefix(dir)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// 새 디렉터리 아래의 경로를 옛 디렉터리 아래로 옮겨 쓴다(그 밖의 경로는 그대로).
pub fn to_legacy(path: &str) -> Cow<'_, str> {
    for (old, new) in LAYOUT {
        if let Some(rest) = under(path, new) {
            return Cow::Owned(format!("{old}{rest}"));
        }
    }
    Cow::Borrowed(path)
}

/// 옛 디렉터리 아래의 경로를 새 디렉터리 아래로 옮겨 쓴다(그 밖의 경로는 그대로).
pub fn to_current(path: &str) -> Cow<'_, str> {
    for (old, new) in LAYOUT {
        if let Some(rest) = under(path, old) {
            return Cow::Owned(format!("{new}{rest}"));
        }
    }
    Cow::Borrowed(path)
}

/// 다른 노드가 여는 경로 `path`를 클러스터 저장소에 적을 값: 모든 노드(`nodes`)가 기능
/// [`ORBIVISOR_LAYOUT`](crate::node::ORBIVISOR_LAYOUT)을 알리면 그대로, 아니면 옛 이름([`to_legacy`]).
pub fn stored<'a>(path: &'a str, nodes: &[crate::node::Node]) -> Cow<'a, str> {
    if crate::node::all_have(nodes, crate::node::ORBIVISOR_LAYOUT) {
        Cow::Borrowed(path)
    } else {
        to_legacy(path)
    }
}

/// 두 경로가 옛 이름·새 이름을 빼면 같은지
pub fn same_path(a: &str, b: &str) -> bool {
    to_current(a) == to_current(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{FEATURES, Node, NodeSpec, ORBIVISOR_LAYOUT};
    use crate::resource::Resource;

    fn node(features: &[&str]) -> Node {
        let mut n: Node = Resource::new(
            "n",
            NodeSpec {
                sdn: None,
                host_resources: None,
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status.features = features.iter().map(|f| f.to_string()).collect();
        n
    }

    #[test]
    fn stores_old_names_until_every_node_has_the_new_layout() {
        let conf = "/etc/orbivisor/ceph/ceph.conf";
        let old = node(&FEATURES[..FEATURES.len() - 1]);
        assert!(!old.status.features.iter().any(|f| f == ORBIVISOR_LAYOUT));
        assert_eq!(
            stored(conf, &[node(FEATURES), old]),
            "/etc/opl/ceph/ceph.conf"
        );
        assert_eq!(stored(conf, &[]), "/etc/opl/ceph/ceph.conf");
        assert_eq!(stored(conf, &[node(FEATURES), node(FEATURES)]), conf);
        assert_eq!(stored("/srv/ceph.conf", &[node(&[])]), "/srv/ceph.conf");
    }

    #[test]
    fn maps_between_old_and_new_names() {
        assert_eq!(
            to_legacy("/etc/orbivisor/ceph/ceph.conf"),
            "/etc/opl/ceph/ceph.conf"
        );
        assert_eq!(to_legacy("/var/lib/orbivisor"), "/var/lib/opl");
        assert_eq!(to_legacy("/srv/ceph/ceph.conf"), "/srv/ceph/ceph.conf");
        assert_eq!(to_legacy("/etc/orbivisorx/a"), "/etc/orbivisorx/a");
        assert_eq!(
            to_current("/etc/opl/ceph/ceph.conf"),
            "/etc/orbivisor/ceph/ceph.conf"
        );
        assert_eq!(to_current("/etc/oplite/a"), "/etc/oplite/a");
        assert_eq!(to_current("/run/opl/vm/x"), "/run/orbivisor/vm/x");
        assert!(same_path(
            "/etc/opl/ceph/ceph.conf",
            "/etc/orbivisor/ceph/ceph.conf"
        ));
        assert!(same_path("/srv/a", "/srv/a"));
        assert!(!same_path("/etc/opl/ceph/a", "/etc/orbivisor/ceph/b"));
    }
}
