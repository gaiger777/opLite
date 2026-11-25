use super::*;

fn p(parts: &[&str]) -> Vec<Vec<u8>> {
    parts.iter().map(|s| s.as_bytes().to_vec()).collect()
}

#[test]
fn guard_follows_depth_first_order() {
    let mut g = PathGuard::new();
    g.admit(&p(&["a"]), true).unwrap();
    g.admit(&p(&["a", "b"]), true).unwrap();
    g.admit(&p(&["a", "b", "c"]), false).unwrap();
    g.admit(&p(&["a", "d"]), false).unwrap();
    // 끝난 하위 디렉터리로 돌아가지 못한다.
    assert!(g.admit(&p(&["a", "b", "e"]), false).is_err());
    assert!(g.admit(&p(&["a", "d"]), false).is_err(), "duplicate");
    g.admit(&p(&["z"]), false).unwrap();
    assert!(g.admit(&p(&["z", "y"]), false).is_err(), "under a file");
    assert!(g.admit(&p(&["a"]), true).is_err(), "duplicate top");
    assert!(
        g.admit(&p(&["a", "q"]), false).is_err(),
        "under a skipped dir"
    );
    assert!(g.admit(&[], false).is_err());
    assert!(g.admit(&p(&[".."]), false).is_err());
    assert!(g.admit(&p(&["x/y"]), false).is_err());
    assert!(g.admit(&[b"n\0".to_vec()], false).is_err());
    let deep: Vec<Vec<u8>> = (0..=MAX_PATH_DEPTH)
        .map(|i| i.to_string().into_bytes())
        .collect();
    let mut g = PathGuard::new();
    for i in 1..=MAX_PATH_DEPTH {
        g.admit(&deep[..i], true).unwrap();
    }
    assert!(g.admit(&deep, false).is_err());
    assert!(g.has_top(b"0"));
}

#[test]
fn report_names_avoid_collisions() {
    let mut g = PathGuard::new();
    assert_eq!(report_name(&g), REPORT_NAME.as_bytes());
    g.admit(&p(&[REPORT_NAME]), false).unwrap();
    g.admit(&p(&["ORBIVISOR-RESTORE-REPORT-2.txt"]), false)
        .unwrap();
    assert_eq!(report_name(&g), b"ORBIVISOR-RESTORE-REPORT-3.txt");
}

#[test]
fn escaped_paths() {
    assert_eq!(
        escape_path(&[b"a".to_vec(), b"b\xffc\n".to_vec()]),
        "/a/b\\xffc\\x0a"
    );
}

#[test]
fn dos_times() {
    assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
    assert_eq!(civil(1_700_000_000), (2023, 11, 14, 22, 13, 20));
    assert_eq!(civil(-1), (1969, 12, 31, 23, 59, 59));
    assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
    // 2023-11-14 22:13:20 → 시각 22<<11|13<<5|10, 날짜 43<<9|11<<5|14
    assert_eq!(
        dos_datetime(1_700_000_000),
        ((22 << 11) | (13 << 5) | 10, (43 << 9) | (11 << 5) | 14)
    );
    assert_eq!(dos_datetime(0), (0, (1 << 5) | 1), "before 1980");
    assert_eq!(dos_datetime(i64::MIN / 2), (0, (1 << 5) | 1));
    let (_, d) = dos_datetime(i64::MAX / 4);
    assert_eq!(d >> 9, 127, "clamped to 2107");
}

#[test]
fn zip_names() {
    assert_eq!(zip::zip_component(b"plain.txt"), b"plain.txt");
    assert_eq!(zip::zip_component("한글".as_bytes()), "한글".as_bytes());
    assert_eq!(zip::zip_component(b"a\\b"), b"a%5Cb");
    assert_eq!(zip::zip_component(b"x\xff\xfe"), b"x%FF%FE");
    assert_eq!(zip::zip_component(b"\x1b[31m"), b"%1B[31m");
    assert!(zip::stored_by_name(b"photo.JPG"));
    assert!(zip::stored_by_name(b"a.tar.gz"));
    assert!(!zip::stored_by_name(b"notes.txt"));
    assert!(!zip::stored_by_name(b"README"));
}

#[test]
fn pax_values() {
    let mut v = Vec::new();
    tar::pax_record(&mut v, "path", b"abc");
    assert_eq!(v, b"12 path=abc\n");
    // 자리수가 바뀌는 경계: 본문 8바이트 → "10 " 붙어 11? 길이는 자기 자리수 포함
    for n in 0..200 {
        let mut v = Vec::new();
        let val = vec![b'x'; n];
        tar::pax_record(&mut v, "k", &val);
        let sp = v.iter().position(|c| *c == b' ').unwrap();
        let len: usize = std::str::from_utf8(&v[..sp]).unwrap().parse().unwrap();
        assert_eq!(len, v.len(), "n={n}");
    }
    assert_eq!(tar::pax_time(5, 0), "5");
    assert_eq!(tar::pax_time(5, 7), "5.000000007");
    assert_eq!(tar::pax_time(-2, 500_000_000), "-1.500000000");
    assert_eq!(tar::pax_time(-1, 250_000_000), "-0.750000000");
    assert_eq!(tar::pax_time(-3, 0), "-3");
    assert!(tar::fits_ustar(b"etc/nginx.conf"));
    assert!(!tar::fits_ustar("é".as_bytes()));
    assert!(!tar::fits_ustar(&[b'a'; 101]));
}
