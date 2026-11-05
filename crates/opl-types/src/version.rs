//! 빌드 버전과 Debian 규칙의 버전 비교.

use std::cmp::Ordering;

/// 이 빌드의 버전. 패키지 빌드는 환경 변수 `OPL_VERSION`으로 넣고, 없거나 비었으면 크레이트 버전이다.
pub const BUILD: &str = match option_env!("OPL_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

/// opLite APT 저장소의 Debian 배포판(suite)
pub const SUITE: &str = "trixie";

/// 이 빌드에 넣은 opLite APT 저장소 URL(빌드 때 환경 변수 `OPL_APT_URL`). 없으면 빈 문자열이다.
pub const APT_URL: &str = match option_env!("OPL_APT_URL") {
    Some(v) => v,
    None => "",
};

/// 두 버전을 Debian 규칙으로 비교한다: `[에포크:]업스트림[-리비전]`, `~`는 끝보다도 낮다.
/// 형식에 맞지 않는 문자열도 패닉하지 않고 같은 규칙으로 비교한다.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ae, au, ar) = split(a);
    let (be, bu, br) = split(b);
    compare_number(ae, be)
        .then_with(|| compare_part(au.as_bytes(), bu.as_bytes()))
        .then_with(|| compare_part(ar.as_bytes(), br.as_bytes()))
}

/// 에포크(숫자가 아니면 없는 것으로 본다), 업스트림, 리비전(마지막 `-` 뒤)
fn split(v: &str) -> (&str, &str, &str) {
    let (epoch, rest) = match v.split_once(':') {
        Some((e, rest)) if !e.is_empty() && e.bytes().all(|c| c.is_ascii_digit()) => (e, rest),
        _ => ("0", v),
    };
    let (upstream, revision) = rest.rsplit_once('-').unwrap_or((rest, ""));
    (epoch, upstream, revision)
}

/// 비숫자 부분 문자의 순서: `~` < 끝·숫자 < 문자 < 그 밖
fn order(c: Option<u8>) -> i32 {
    match c {
        Some(b'~') => -1,
        None => 0,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => i32::from(c),
        Some(c) => i32::from(c) + 256,
    }
}

/// 숫자 문자열을 수로 비교한다(앞 0 무시, 길이에 제한 없음).
fn compare_number(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// 업스트림·리비전을 비숫자 부분과 숫자 부분을 번갈아 비교한다(dpkg `verrevcmp`).
fn compare_part(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while a.get(i).is_some_and(|c| !c.is_ascii_digit())
            || b.get(j).is_some_and(|c| !c.is_ascii_digit())
        {
            let (x, y) = (order(a.get(i).copied()), order(b.get(j).copied()));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
        let (x, ni) = digits(a, i);
        let (y, nj) = digits(b, j);
        let ord = compare_number(x, y);
        if ord != Ordering::Equal {
            return ord;
        }
        (i, j) = (ni, nj);
    }
    Ordering::Equal
}

/// `from`에서 시작하는 숫자 부분과 그 끝 위치
fn digits(s: &[u8], from: usize) -> (&str, usize) {
    let end = from + s[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    (std::str::from_utf8(&s[from..end]).unwrap_or(""), end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_is_opl_version_or_crate_version() {
        let want = match option_env!("OPL_VERSION") {
            Some(v) if !v.is_empty() => v,
            _ => env!("CARGO_PKG_VERSION"),
        };
        assert!(!BUILD.is_empty());
        assert_eq!(BUILD, want);
    }

    #[test]
    fn apt_url_is_the_build_variable_or_empty() {
        assert_eq!(APT_URL, option_env!("OPL_APT_URL").unwrap_or(""));
        assert_eq!(SUITE, "trixie");
    }

    #[test]
    fn compares_like_dpkg() {
        use Ordering::{Equal, Greater, Less};
        for (a, b, want) in [
            // 같음, 앞 0, 없는 리비전은 0과 같다.
            ("1.2.3", "1.2.3", Equal),
            ("1.01", "1.1", Equal),
            ("1.0", "1.0-0", Equal),
            ("0:1.0", "1.0", Equal),
            // 숫자는 수로 비교한다.
            ("0.10.0", "0.9.0", Greater),
            ("1.0~ci12", "1.0~ci5", Greater),
            ("1.2", "1.2.1", Less),
            // `~`는 끝보다 낮고, 끝은 문자보다, 문자는 그 밖의 문자보다 낮다.
            ("1.0~rc1", "1.0", Less),
            ("1.0~~", "1.0~", Less),
            ("1.0", "1.0a", Less),
            ("1.0a", "1.0+", Less),
            ("1.0", "1.0+1", Less),
            ("1.0b", "1.0a", Greater),
            // 에포크가 먼저다.
            ("1:0.1", "2.0", Greater),
            ("2:1.0", "10:0.1", Less),
            // 리비전은 마지막 `-` 뒤다.
            ("1.0-1", "1.0-2", Less),
            ("1.0-10", "1.0-9", Greater),
            ("1.0-rc-2", "1.0-rc-10", Less),
            ("1.0-1", "1.0", Greater),
            // 아주 큰 수도 넘치지 않는다.
            (
                "99999999999999999999999",
                "99999999999999999999998",
                Greater,
            ),
        ] {
            assert_eq!(compare(a, b), want, "{a} vs {b}");
            assert_eq!(compare(b, a), want.reverse(), "{b} vs {a}");
        }
    }

    #[test]
    fn odd_strings_do_not_panic() {
        for a in ["", ":", "a:b", "-", "1:", "~", "1.0-", ":-:", "é1", "1:2:3"] {
            for b in ["", "1.0", "~", a] {
                let _ = compare(a, b);
            }
            assert_eq!(compare(a, a), Ordering::Equal, "{a}");
        }
        assert_eq!(compare("", "1"), Ordering::Less);
        assert_eq!(compare("~", ""), Ordering::Less);
    }
}
