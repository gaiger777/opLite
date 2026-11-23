//! 만든 zip·tar.gz를 실제 도구로(설계 §14): `unzip -t`·풀기, `bsdtar`, `tar -tvzf`·풀기, 있으면 Python `zipfile`.
//! 풀어 낸 내용·모드·링크를 견주고, 도우미가 링크 아래로 쓰려 한 항목이 풀 때 밖에 아무것도 만들지 않는지 본다.
//! `cargo test -p opl-restore --features storage-it --test archive_tools`(unzip·tar 필요, bsdtar·python3는 있으면 —
//! Debian/Ubuntu: apt install unzip libarchive-tools)
#![cfg(feature = "storage-it")]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

use opl_restore::agent::fs::path_of;
use opl_restore::archive::tar::TarGzWriter;
use opl_restore::archive::zip::ZipWriter;
use opl_restore::archive::{Archive, REPORT_NAME};
use opl_restore::proto::{Attrs, EntryKind, LinkTarget, Name};

fn run(dir: &Path, cmd: &str, args: &[&str]) -> String {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{cmd}: {e}"));
    assert!(
        out.status.success(),
        "{cmd} {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn attrs(kind: EntryKind, size: u64, mode: u32) -> Attrs {
    Attrs {
        kind,
        size,
        mtime: 1_700_000_000,
        mtime_nsec: 0,
        mode,
        uid: 0,
        gid: 0,
        nlink: 1,
    }
}

fn file<A: Archive>(a: &mut A, path: &str, data: &[u8], mode: u32) {
    assert!(
        a.begin_file(
            &path_of(path),
            &attrs(EntryKind::File, data.len() as u64, mode)
        )
        .unwrap()
    );
    for c in data.chunks(65_536) {
        a.write(c).unwrap();
    }
    a.end_file(None).unwrap();
}

/// 이것저것 든 나무(디렉터리, 압축되는 것·저장되는 것·빈 것, UTF-8 이름, 긴 이름, 깊은 경로)
fn sample<A: Archive>(a: &mut A) -> Vec<(String, Vec<u8>)> {
    let d = attrs(EntryKind::Dir, 0, 0o755);
    let mut want = Vec::new();
    a.dir(&path_of("etc"), &d).unwrap();
    let conf = b"server { listen 80; }\n".repeat(5000);
    file(a, "etc/nginx.conf", &conf, 0o644);
    want.push(("etc/nginx.conf".to_string(), conf));
    let rnd: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    file(a, "etc/blob.gz", &rnd, 0o600);
    want.push(("etc/blob.gz".to_string(), rnd));
    file(a, "etc/empty", b"", 0o644);
    want.push(("etc/empty".to_string(), Vec::new()));
    a.dir(&path_of("etc/한글"), &d).unwrap();
    file(a, "etc/한글/메모.txt", "안녕하세요".as_bytes(), 0o644);
    want.push((
        "etc/한글/메모.txt".to_string(),
        "안녕하세요".as_bytes().to_vec(),
    ));
    let long = format!("etc/{}", "n".repeat(180));
    file(a, &long, b"long name", 0o644);
    want.push((long, b"long name".to_vec()));
    let mut deep = "etc".to_string();
    for i in 0..30 {
        deep.push_str(&format!("/level{i}"));
        a.dir(&path_of(&deep), &d).unwrap();
    }
    let deep_file = format!("{deep}/bottom");
    file(a, &deep_file, b"deep", 0o644);
    want.push((deep_file, b"deep".to_vec()));
    want
}

/// 있으면 쓰는 도구(bsdtar·python3 — CI는 둘 다 깐다)
fn has(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn python_zip_check(dir: &Path, zip: &str) {
    if !has("python3") {
        eprintln!("python3 not found — skipping zipfile check");
        return;
    }
    let script = "import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); \
                  bad = z.testzip(); print(bad); sys.exit(1 if bad else 0)";
    run(dir, "python3", &["-I", "-c", script, zip]);
}

#[test]
fn zip_with_unzip_bsdtar_python() {
    for zip64 in [false, true] {
        let t = tempfile::tempdir().unwrap();
        let mut z = ZipWriter::new(Vec::new());
        if zip64 {
            z = z.force_zip64();
        }
        let want = sample(&mut z);
        z.symlink(
            &path_of("etc/link"),
            &attrs(EntryKind::Symlink, 0, 0o777),
            &LinkTarget::new("/etc/passwd").unwrap(),
        )
        .unwrap();
        std::fs::write(t.path().join("a.zip"), z.finish().unwrap()).unwrap();
        let tested = run(t.path(), "unzip", &["-t", "a.zip"]);
        assert!(tested.contains("No errors detected"), "{tested}");
        run(t.path(), "unzip", &["-q", "a.zip", "-d", "u"]);
        let mut roots = vec!["u"];
        if has("bsdtar") {
            std::fs::create_dir(t.path().join("b")).unwrap();
            run(t.path(), "bsdtar", &["-xf", "a.zip", "-C", "b"]);
            roots.push("b");
        }
        for root in roots {
            for (p, data) in &want {
                let got = std::fs::read(t.path().join(root).join(p)).unwrap();
                assert_eq!(&got, data, "{root}/{p} (zip64 {zip64})");
            }
            assert!(
                !t.path().join(root).join("etc/link").exists(),
                "no links in zip"
            );
            let report = std::fs::read_to_string(t.path().join(root).join(REPORT_NAME)).unwrap();
            assert!(report.contains("skipped: /etc/link"));
        }
        let mode = std::fs::metadata(t.path().join("u/etc/blob.gz"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "unzip restores unix modes");
        python_zip_check(t.path(), "a.zip");
    }
}

#[test]
fn zip64_many_entries_with_unzip() {
    let t = tempfile::tempdir().unwrap();
    let mut z = ZipWriter::new(Vec::new());
    z.dir(&path_of("m"), &attrs(EntryKind::Dir, 0, 0o755))
        .unwrap();
    for i in 0..70_000 {
        file(&mut z, &format!("m/{i}"), b"", 0o644);
    }
    std::fs::write(t.path().join("m.zip"), z.finish().unwrap()).unwrap();
    let out = run(t.path(), "unzip", &["-tq", "m.zip"]);
    assert!(out.contains("No errors detected"), "{out}");
    if has("bsdtar") {
        let listed = run(t.path(), "bsdtar", &["-tf", "m.zip"]);
        assert_eq!(listed.lines().count(), 70_001);
    }
    python_zip_check(t.path(), "m.zip");
}

#[test]
fn tar_gz_with_tar_and_bsdtar() {
    let t = tempfile::tempdir().unwrap();
    let mut w = TarGzWriter::new(Vec::new());
    let want = sample(&mut w);
    let mut two = attrs(EntryKind::File, 5, 0o640);
    two.nlink = 2;
    assert!(w.begin_file(&path_of("etc/orig"), &two).unwrap());
    w.write(b"hello").unwrap();
    w.end_file(None).unwrap();
    w.hardlink(&path_of("etc/hard"), &two, &path_of("etc/orig"))
        .unwrap();
    w.symlink(
        &path_of("etc/sym"),
        &attrs(EntryKind::Symlink, 0, 0o777),
        &LinkTarget::new("nginx.conf").unwrap(),
    )
    .unwrap();
    let mut ns = attrs(EntryKind::File, 1, 0o644);
    ns.mtime_nsec = 250_000_000;
    assert!(w.begin_file(&path_of("etc/nanos"), &ns).unwrap());
    w.write(b"n").unwrap();
    w.end_file(None).unwrap();
    std::fs::write(t.path().join("a.tar.gz"), w.finish().unwrap()).unwrap();
    let listing = run(t.path(), "tar", &["-tvzf", "a.tar.gz"]);
    assert!(listing.contains("etc/sym -> nginx.conf"), "{listing}");
    for (tool, dir) in [("tar", "x"), ("bsdtar", "y")] {
        if !has(tool) {
            eprintln!("{tool} not found — skipping");
            continue;
        }
        std::fs::create_dir(t.path().join(dir)).unwrap();
        run(t.path(), tool, &["-xzf", "a.tar.gz", "-C", dir]);
        let root = t.path().join(dir);
        for (p, data) in &want {
            assert_eq!(&std::fs::read(root.join(p)).unwrap(), data, "{tool} {p}");
        }
        assert_eq!(
            std::fs::read_link(root.join("etc/sym")).unwrap(),
            Path::new("nginx.conf")
        );
        let (a, b) = (
            std::fs::metadata(root.join("etc/orig")).unwrap(),
            std::fs::metadata(root.join("etc/hard")).unwrap(),
        );
        assert_eq!(a.ino(), b.ino(), "{tool}: hard link");
        assert_eq!(a.permissions().mode() & 0o777, 0o640);
        assert_eq!(a.mtime(), 1_700_000_000);
        assert_eq!(
            std::fs::metadata(root.join("etc/nanos")).unwrap().mtime(),
            1_700_000_000
        );
        assert!(!root.join(REPORT_NAME).exists(), "nothing to report");
    }
}

#[test]
fn tar_gz_non_utf8_names_are_listed_as_bytes() {
    let t = tempfile::tempdir().unwrap();
    let mut w = TarGzWriter::new(Vec::new());
    let raw = vec![Name::new(b"caf\xe9.txt".to_vec()).unwrap()];
    assert!(
        w.begin_file(&raw, &attrs(EntryKind::File, 1, 0o644))
            .unwrap()
    );
    w.write(b"x").unwrap();
    w.end_file(None).unwrap();
    std::fs::write(t.path().join("n.tar.gz"), w.finish().unwrap()).unwrap();
    let out = Command::new("tar")
        .args(["-tzf", "n.tar.gz"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // GNU tar는 바이트 그대로, bsdtar는 바꿔 보일 수 있다 — 이름이 하나 있고 "caf"로 시작하는지만
    let listed = String::from_utf8_lossy(&out.stdout);
    assert_eq!(listed.lines().count(), 1);
    assert!(listed.starts_with("caf"), "{listed}");
}

/// 도우미가 링크 "a" 아래에 파일을 쓰려 하면(차례를 어긴 항목) 작성기가 빼므로, 풀어도 링크 밖에 아무것도 생기지
/// 않는다.
#[test]
fn hostile_walk_cannot_write_through_links() {
    let t = tempfile::tempdir().unwrap();
    let outside = t.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let mut w = TarGzWriter::new(Vec::new());
    w.symlink(
        &path_of("a"),
        &attrs(EntryKind::Symlink, 0, 0o777),
        &LinkTarget::new(outside.to_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        !w.begin_file(&path_of("a/planted"), &attrs(EntryKind::File, 4, 0o644))
            .unwrap()
    );
    w.dir(&path_of("d"), &attrs(EntryKind::Dir, 0, 0o755))
        .unwrap();
    w.dir(&path_of("d"), &attrs(EntryKind::Dir, 0, 0o755))
        .unwrap();
    assert!(
        !w.begin_file(&path_of("d/x"), &attrs(EntryKind::File, 1, 0o644))
            .unwrap()
    );
    std::fs::write(t.path().join("h.tar.gz"), w.finish().unwrap()).unwrap();
    std::fs::create_dir(t.path().join("x")).unwrap();
    run(t.path(), "tar", &["-xzf", "h.tar.gz", "-C", "x"]);
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing written outside"
    );
    assert!(!t.path().join("x/d/x").exists());
    let report = std::fs::read_to_string(t.path().join("x").join(REPORT_NAME)).unwrap();
    assert!(report.contains("skipped: /a/planted"), "{report}");
}
