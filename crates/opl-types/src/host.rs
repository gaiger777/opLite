//! 노드 주소(클러스터 링크·API 주소)의 호스트 규칙. opld 설정 검사와 API 합류 검사가 함께 쓴다.

use std::net::IpAddr;

/// 다른 노드가 닿고 인증서 SAN에 넣을 수 있는 호스트인지: 특수 주소(0.0.0.0·::·멀티캐스트·브로드캐스트)가 아닌
/// IP나 RFC 1123 호스트 이름(와일드카드, 끝의 점 없이)
pub fn valid_host(host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        let broadcast = matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
        return !ip.is_unspecified() && !ip.is_multicast() && !broadcast;
    }
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// 같은 호스트인지: IP는 값으로(`fd00:0::5` = `fd00::5`), 이름은 대소문자 없이
pub fn same_host(a: &str, b: &str) -> bool {
    match (a.parse::<IpAddr>(), b.parse::<IpAddr>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// 같은 링크(`host:port`, IPv6은 `[addr]:port`)인지: 포트가 같고 [`same_host`]. 모양이 아니면 문자열로 비교한다.
pub fn same_link(a: &str, b: &str) -> bool {
    match (split_link(a), split_link(b)) {
        (Some((ha, pa)), Some((hb, pb))) => pa == pb && same_host(ha, hb),
        _ => a == b,
    }
}

fn split_link(link: &str) -> Option<(&str, u16)> {
    let (host, port) = link.rsplit_once(':')?;
    let port = port.parse().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_host_accepts_ips_and_host_names_only() {
        for good in ["node1.example", "localhost", "10.0.0.5", "::1", "a-b.c1"] {
            assert!(valid_host(good), "{good}");
        }
        let long_label = format!("{}.example", "a".repeat(64));
        let long_name = vec!["a".repeat(63); 4].join(".");
        for bad in [
            "",
            "*.corp.example",
            "evil host/x",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
            &long_label,
            &long_name,
            "-a.example",
            "a-.example",
            "node1.example.",
            "a..b",
            "노드.example",
            "[::1]",
            "10.0.0.5:8007",
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
    }

    #[test]
    fn hosts_and_links_compare_by_value() {
        assert!(same_host("fd00::5", "fd00:0::5"));
        assert!(same_host("Node1.LAN", "node1.lan"));
        assert!(!same_host("10.0.0.5", "10.0.0.6"));
        assert!(same_link("[fd00::5]:8007", "[fd00:0:0::5]:8007"));
        assert!(same_link("Node1.lan:8007", "node1.LAN:8007"));
        assert!(!same_link("[fd00::5]:8007", "[fd00::5]:9007"));
        assert!(!same_link("10.0.0.5:8007", "10.0.0.6:8007"));
        assert!(same_link("odd", "odd"));
    }
}
