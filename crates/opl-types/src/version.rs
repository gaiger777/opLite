//! 빌드 버전.

/// 이 빌드의 버전. 패키지 빌드는 환경 변수 `OPL_VERSION`으로 넣고, 없거나 비었으면 크레이트 버전이다.
pub const BUILD: &str = match option_env!("OPL_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

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
}
