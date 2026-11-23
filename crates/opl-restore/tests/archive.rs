//! zip·tar.gz 흘려 쓰기 작성기(설계 §11·§14): 우리 최소 읽기 검사기로 머리·크기·CRC·zip64·pax를 엄격하게 읽어 보고,
//! 경로 거르기(차례 어긋남·겹침·링크 아래 항목·하드 링크 대상)와 읽기 오류 보고서를 본다. 무작위 나무 성질 시험 포함.

mod common;

use std::collections::BTreeMap;

use opl_restore::agent::fs::path_of;
use opl_restore::archive::tar::TarGzWriter;
use opl_restore::archive::zip::ZipWriter;
use opl_restore::archive::{Archive, Feeder, REPORT_NAME};
use opl_restore::proto::{Attrs, EntryKind, Event, Item, LinkTarget, Name, Reply};
use proptest::prelude::*;

fn attrs(kind: EntryKind, size: u64, mode: u32) -> Attrs {
    Attrs {
        kind,
        size,
        mtime: 1_700_000_000,
        mtime_nsec: 0,
        mode,
        uid: 1000,
        gid: 1000,
        nlink: 1,
    }
}

fn file<A: Archive>(a: &mut A, path: &str, data: &[u8]) {
    let p = path_of(path);
    assert!(
        a.begin_file(&p, &attrs(EntryKind::File, data.len() as u64, 0o640))
            .unwrap()
    );
    for c in data.chunks(7) {
        a.write(c).unwrap();
    }
    a.end_file(None).unwrap();
}

fn names(z: &common::Zip) -> Vec<String> {
    z.entries
        .iter()
        .map(|e| String::from_utf8(e.name.clone()).unwrap())
        .collect()
}

fn zip_report(z: &common::Zip) -> String {
    let e = z
        .entries
        .iter()
        .find(|e| e.name == REPORT_NAME.as_bytes())
        .expect("report");
    String::from_utf8(e.data.clone()).unwrap()
}

#[test]
fn zip_files_dirs_and_report() {
    let mut z = ZipWriter::new(Vec::new());
    z.dir(&path_of("etc"), &attrs(EntryKind::Dir, 0, 0o755))
        .unwrap();
    let text = b"server { listen 80; }\n".repeat(100);
    file(&mut z, "etc/nginx.conf", &text);
    file(&mut z, "etc/logo.png", b"\x89PNG not really");
    file(&mut z, "etc/empty", b"");
    file(&mut z, "etc/한글.txt", "안녕".as_bytes());
    z.symlink(
        &path_of("etc/link"),
        &attrs(EntryKind::Symlink, 0, 0o777),
        &LinkTarget::new("/etc/passwd").unwrap(),
    )
    .unwrap();
    z.skip(
        &path_of("etc/null"),
        "special file (device, FIFO or socket)",
    );
    let zip = common::read_zip(&z.finish().unwrap());
    assert_eq!(
        names(&zip),
        [
            "etc/",
            "etc/nginx.conf",
            "etc/logo.png",
            "etc/empty",
            "etc/한글.txt",
            REPORT_NAME
        ]
    );
    let e = &zip.entries;
    assert_eq!(e[0].mode, 0o40755);
    assert_eq!(e[1].data, text);
    assert_eq!(e[1].method, 8, "text is deflated");
    assert!(e[1].data.len() > 1000);
    assert_eq!(e[1].mode, 0o100640);
    assert_eq!(e[1].mtime, Some(1_700_000_000));
    assert_eq!(e[2].method, 0, "png is stored");
    assert_eq!(e[3].data, b"");
    assert_eq!(e[4].flags & (1 << 11), 1 << 11, "UTF-8 flag");
    assert_eq!(e[1].flags & (1 << 11), 0, "ASCII name has no UTF-8 flag");
    assert!(e.iter().all(|e| !e.zip64_local && !e.zip64_central));
    assert!(!zip.zip64_eocd);
    let report = zip_report(&zip);
    assert!(
        report.contains("skipped: /etc/link — symbolic link"),
        "{report}"
    );
    assert!(
        report.contains("skipped: /etc/null — special file"),
        "{report}"
    );
}

#[test]
fn zip_without_report_when_clean() {
    let mut z = ZipWriter::new(Vec::new());
    file(&mut z, "a", b"x");
    let zip = common::read_zip(&z.finish().unwrap());
    assert_eq!(names(&zip), ["a"]);
}

#[test]
fn zip_escapes_non_utf8_backslash_and_control_names() {
    let mut z = ZipWriter::new(Vec::new());
    let raw = vec![Name::new(b"caf\xe9".to_vec()).unwrap()];
    assert!(
        z.begin_file(&raw, &attrs(EntryKind::File, 1, 0o644))
            .unwrap()
    );
    z.write(b"1").unwrap();
    z.end_file(None).unwrap();
    file(&mut z, "..\\..\\evil.txt", b"2");
    file(&mut z, "tab\there", b"3");
    let zip = common::read_zip(&z.finish().unwrap());
    assert_eq!(
        names(&zip),
        ["caf%E9", "..%5C..%5Cevil.txt", "tab%09here", REPORT_NAME]
    );
    let report = zip_report(&zip);
    assert!(
        report.contains("renamed: /caf\\xe9 — stored as /caf%E9"),
        "{report}"
    );
}

#[test]
fn zip_forced_zip64_headers() {
    let mut z = ZipWriter::new(Vec::new()).force_zip64();
    z.dir(&path_of("d"), &attrs(EntryKind::Dir, 0, 0o755))
        .unwrap();
    file(&mut z, "d/a.txt", &b"hello zip64 ".repeat(50));
    file(&mut z, "d/b", b"");
    let zip = common::read_zip(&z.finish().unwrap());
    assert!(zip.zip64_eocd);
    assert!(zip.entries.iter().all(|e| e.zip64_central));
    assert!(zip.entries[1].zip64_local && zip.entries[2].zip64_local);
    assert_eq!(zip.entries[1].data, b"hello zip64 ".repeat(50));
}

#[test]
fn zip_large_declared_size_uses_zip64_local_header() {
    // 4 GiB 가까운 크기를 선언한 파일은 지역 헤더부터 zip64다(데이터는 흘리지 않고 오류로 끝낸다).
    let mut z = ZipWriter::new(Vec::new());
    let big = attrs(EntryKind::File, 5 << 30, 0o644);
    assert!(z.begin_file(&path_of("disk.img"), &big).unwrap());
    z.write(b"partial").unwrap();
    z.end_file(Some("read error: Input/output error")).unwrap();
    let zip = common::read_zip(&z.finish().unwrap());
    assert!(zip.entries[0].zip64_local);
    assert!(!zip.entries[0].zip64_central, "actual sizes fit in 32 bits");
    assert_eq!(zip.entries[0].data, b"partial");
    let report = zip_report(&zip);
    assert!(
        report.contains("incomplete: /disk.img — read error"),
        "{report}"
    );
}

#[test]
fn zip_more_than_65535_entries() {
    let mut z = ZipWriter::new(Vec::new());
    z.dir(&path_of("many"), &attrs(EntryKind::Dir, 0, 0o755))
        .unwrap();
    for i in 0..70_000 {
        let p = path_of(&format!("many/f{i}"));
        assert!(z.begin_file(&p, &attrs(EntryKind::File, 0, 0o644)).unwrap());
        z.end_file(None).unwrap();
    }
    assert_eq!(z.entries(), 70_001);
    let zip = common::read_zip(&z.finish().unwrap());
    assert!(zip.zip64_eocd);
    assert_eq!(zip.entries.len(), 70_001);
    assert_eq!(zip.entries[70_000].name, b"many/f69999");
}

#[test]
fn zip_short_and_long_data() {
    let mut z = ZipWriter::new(Vec::new());
    assert!(
        z.begin_file(&path_of("short"), &attrs(EntryKind::File, 10, 0o644))
            .unwrap()
    );
    z.write(b"abcd").unwrap();
    assert!(z.write(b"0123456789").is_err(), "more than declared");
    z.end_file(None).unwrap();
    let zip = common::read_zip(&z.finish().unwrap());
    assert_eq!(zip.entries[0].data, b"abcd");
    assert!(zip_report(&zip).contains("incomplete: /short — the file ended before its size"));
}

/// 도우미가 보낸 경로를 다시 거른다: 링크 아래 항목, 차례 어긋남, 같은 경로, 건너뛴 디렉터리의 아래.
fn hostile<A: Archive>(a: &mut A) {
    let d = attrs(EntryKind::Dir, 0, 0o755);
    a.symlink(
        &path_of("a"),
        &attrs(EntryKind::Symlink, 0, 0o777),
        &LinkTarget::new("/etc").unwrap(),
    )
    .unwrap();
    // 링크 "a" 아래에 쓰려는 항목
    assert!(
        !a.begin_file(&path_of("a/passwd"), &attrs(EntryKind::File, 1, 0o644))
            .unwrap()
    );
    a.dir(&path_of("x"), &d).unwrap();
    file(a, "x/1", b"one");
    // 같은 디렉터리를 다시 — 둘째와 그 아래는 건너뜀(첫째에 섞이지 않음)
    a.dir(&path_of("x"), &d).unwrap();
    assert!(
        !a.begin_file(&path_of("x/2"), &attrs(EntryKind::File, 1, 0o644))
            .unwrap()
    );
    // 보낸 적 없는 부모
    assert!(
        !a.begin_file(&path_of("y/z"), &attrs(EntryKind::File, 1, 0o644))
            .unwrap()
    );
    // 파일 아래
    file(a, "f", b"f");
    assert!(
        !a.begin_file(&path_of("f/g"), &attrs(EntryKind::File, 1, 0o644))
            .unwrap()
    );
    // 하드 링크: 아카이브에 없는 대상
    a.hardlink(
        &path_of("h"),
        &attrs(EntryKind::File, 0, 0o644),
        &path_of("x/1"),
    )
    .unwrap();
}

#[test]
fn zip_rejects_hostile_paths() {
    let mut z = ZipWriter::new(Vec::new());
    hostile(&mut z);
    let zip = common::read_zip(&z.finish().unwrap());
    assert_eq!(names(&zip), ["x/", "x/1", "f", REPORT_NAME]);
    let r = zip_report(&zip);
    for line in [
        "skipped: /a — symbolic link",
        "skipped: /a/passwd — item is out of order",
        "skipped: /x — another item has the same path",
        "skipped: /x/2 — its parent directory was skipped",
        "skipped: /y/z — item is out of order",
        "skipped: /f/g — item is out of order",
        "skipped: /h — hard link without content",
    ] {
        assert!(r.contains(line), "{line}\n{r}");
    }
}

fn tar_report(t: &[common::TarEntry]) -> String {
    let e = t
        .iter()
        .find(|e| e.path == REPORT_NAME.as_bytes())
        .expect("report");
    String::from_utf8(e.data.clone()).unwrap()
}

#[test]
fn tar_rejects_hostile_paths() {
    let mut t = TarGzWriter::new(Vec::new());
    hostile(&mut t);
    let tar = common::read_tar_gz(&t.finish().unwrap());
    let paths: Vec<String> = tar
        .iter()
        .map(|e| String::from_utf8_lossy(&e.path).into_owned())
        .collect();
    assert_eq!(paths, ["a", "x/", "x/1", "f", REPORT_NAME]);
    assert_eq!(tar[0].typeflag, b'2');
    assert_eq!(tar[0].link, b"/etc");
    let r = tar_report(&tar);
    for line in [
        "skipped: /a/passwd — item is out of order",
        "skipped: /x — another item has the same path",
        "skipped: /x/2 — its parent directory was skipped",
        "skipped: /y/z — item is out of order",
        "skipped: /f/g — item is out of order",
        "skipped: /h — hard link to a file that is not in this archive",
    ] {
        assert!(r.contains(line), "{line}\n{r}");
    }
}

#[test]
fn tar_headers_pax_links_and_errors() {
    let mut t = TarGzWriter::new(Vec::new());
    let d = attrs(EntryKind::Dir, 0, 0o750);
    t.dir(&path_of("root"), &d).unwrap();
    // 하드 링크 원본(링크 수 2)
    let mut two = attrs(EntryKind::File, 3, 0o600);
    two.nlink = 2;
    assert!(t.begin_file(&path_of("root/orig"), &two).unwrap());
    t.write(b"abc").unwrap();
    t.end_file(None).unwrap();
    t.hardlink(&path_of("root/link"), &two, &path_of("root/orig"))
        .unwrap();
    t.symlink(
        &path_of("root/sym"),
        &attrs(EntryKind::Symlink, 0, 0o777),
        &LinkTarget::new("../outside/target").unwrap(),
    )
    .unwrap();
    let long = "x".repeat(150);
    file(&mut t, &format!("root/{long}"), b"long");
    let raw = vec![
        Name::new("root").unwrap(),
        Name::new(b"bin\xffary".to_vec()).unwrap(),
    ];
    let mut odd = attrs(EntryKind::File, 1, 0o644);
    odd.mtime_nsec = 123;
    odd.uid = 3_000_000;
    assert!(t.begin_file(&raw, &odd).unwrap());
    t.write(b"b").unwrap();
    t.end_file(None).unwrap();
    let mut old = attrs(EntryKind::File, 0, 0o644);
    old.mtime = -2;
    old.mtime_nsec = 500_000_000;
    assert!(t.begin_file(&path_of("root/old"), &old).unwrap());
    t.end_file(None).unwrap();
    assert!(
        t.begin_file(&path_of("root/broken"), &attrs(EntryKind::File, 10, 0o644))
            .unwrap()
    );
    t.write(b"1234").unwrap();
    t.end_file(Some("read error: Input/output error")).unwrap();
    t.skip(
        &path_of("root/fifo"),
        "special file (device, FIFO or socket)",
    );
    let tar = common::read_tar_gz(&t.finish().unwrap());
    let by: BTreeMap<Vec<u8>, &common::TarEntry> =
        tar.iter().map(|e| (e.path.clone(), e)).collect();
    let root = by[&b"root/".to_vec()];
    assert_eq!((root.typeflag, root.mode, root.uid), (b'5', 0o750, 1000));
    assert_eq!(by[&b"root/orig".to_vec()].data, b"abc");
    let link = by[&b"root/link".to_vec()];
    assert_eq!(
        (link.typeflag, link.link.as_slice(), link.data.len()),
        (b'1', &b"root/orig"[..], 0)
    );
    let sym = by[&b"root/sym".to_vec()];
    assert_eq!(
        (sym.typeflag, sym.link.as_slice()),
        (b'2', &b"../outside/target"[..])
    );
    let l = by[format!("root/{long}").as_bytes()];
    assert!(l.pax.contains_key("path"));
    assert_eq!(l.data, b"long");
    let b = by[&b"root/bin\xffary".to_vec()];
    assert_eq!(
        b.pax.get("hdrcharset").map(Vec::as_slice),
        Some(&b"BINARY"[..])
    );
    assert_eq!(b.uid, 3_000_000);
    assert_eq!(b.mtime, "1700000000.000000123");
    assert_eq!(by[&b"root/old".to_vec()].mtime, "-1.500000000");
    let broken = by[&b"root/broken".to_vec()];
    assert_eq!(
        broken.data, b"1234\0\0\0\0\0\0",
        "padded to the declared size"
    );
    let r = tar_report(&tar);
    assert!(r.contains("incomplete: /root/broken — read error"), "{r}");
    assert!(r.contains("skipped: /root/fifo — special file"), "{r}");
    // 평범한 항목에는 pax 머리가 없다.
    assert!(by[&b"root/orig".to_vec()].pax.is_empty());
}

#[test]
fn feeder_maps_walk_events() {
    let item = |p: &str, a: Attrs| {
        Event::Reply(Reply::Item(Item {
            path: path_of(p),
            attrs: a,
            link_target: None,
            hardlink: None,
        }))
    };
    let events = vec![
        item("d", attrs(EntryKind::Dir, 0, 0o755)),
        item("d/a", attrs(EntryKind::File, 5, 0o644)),
        Event::Data(b"he".to_vec()),
        Event::Data(b"llo".to_vec()),
        item("d/e", attrs(EntryKind::File, 0, 0o644)),
        item("d/bad", attrs(EntryKind::File, 4, 0o644)),
        Event::Data(b"ab".to_vec()),
        Event::Reply(Reply::FileError {
            message: "read error".into(),
        }),
        item("d/dev", attrs(EntryKind::Other, 0, 0o600)),
        Event::Reply(Reply::Skipped {
            path: path_of("d/locked"),
            reason: "permission denied".into(),
        }),
        Event::Reply(Reply::Progress {
            entries: 4,
            bytes: 7,
        }),
        Event::End,
    ];
    let mut z = ZipWriter::new(Vec::new());
    let mut f = Feeder::new();
    for e in &events {
        f.feed(&mut z, e).unwrap();
    }
    let zip = common::read_zip(&z.finish().unwrap());
    assert_eq!(names(&zip), ["d/", "d/a", "d/e", "d/bad", REPORT_NAME]);
    assert_eq!(zip.entries[1].data, b"hello");
    assert_eq!(zip.entries[3].data, b"ab");
    let r = zip_report(&zip);
    assert!(r.contains("incomplete: /d/bad — read error"), "{r}");
    assert!(r.contains("skipped: /d/dev — special file"), "{r}");
    assert!(r.contains("skipped: /d/locked — permission denied"), "{r}");
}

// ---------------------------------------------------------------------------------------------------------------
// 무작위 나무
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Tree {
    File(Vec<u8>),
    Dir(BTreeMap<String, Tree>),
}

fn tree() -> impl Strategy<Value = Tree> {
    let name = "[a-zA-Z0-9 ._가-힣-]{1,12}".prop_filter("not . or ..", |s| s != "." && s != "..");
    let leaf = prop::collection::vec(any::<u8>(), 0..3000).prop_map(Tree::File);
    leaf.prop_recursive(4, 40, 6, move |inner| {
        prop::collection::btree_map(name.clone(), inner, 0..6).prop_map(Tree::Dir)
    })
}

/// 나무를 깊이 우선으로 쓴다(데이터는 `chunk` 바이트씩). 기대: 경로 → 내용(디렉터리는 None)
fn emit<A: Archive>(
    a: &mut A,
    prefix: &[Name],
    t: &BTreeMap<String, Tree>,
    chunk: usize,
    want: &mut Vec<(Vec<u8>, Option<Vec<u8>>)>,
) {
    for (n, node) in t {
        let mut p = prefix.to_vec();
        p.push(Name::new(n.as_str()).unwrap());
        let joined = p
            .iter()
            .map(|x| x.as_bytes())
            .collect::<Vec<_>>()
            .join(&b'/');
        match node {
            Tree::File(data) => {
                assert!(
                    a.begin_file(&p, &attrs(EntryKind::File, data.len() as u64, 0o644))
                        .unwrap()
                );
                for c in data.chunks(chunk) {
                    a.write(c).unwrap();
                }
                a.end_file(None).unwrap();
                want.push((joined, Some(data.clone())));
            }
            Tree::Dir(children) => {
                a.dir(&p, &attrs(EntryKind::Dir, 0, 0o755)).unwrap();
                let mut d = joined;
                d.push(b'/');
                want.push((d, None));
                emit(a, &p, children, chunk, want);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn random_trees_round_trip(t in tree(), chunk in 1usize..5000, z64 in any::<bool>()) {
        let top = match t {
            Tree::Dir(m) => m,
            f @ Tree::File(_) => BTreeMap::from([("only".to_string(), f)]),
        };
        let mut want = Vec::new();
        let mut z = ZipWriter::new(Vec::new());
        if z64 {
            z = z.force_zip64();
        }
        emit(&mut z, &[], &top, chunk, &mut want);
        prop_assert!(z.report().is_empty());
        let zip = common::read_zip(&z.finish().unwrap());
        let got: Vec<(Vec<u8>, Option<Vec<u8>>)> = zip
            .entries
            .iter()
            .map(|e| (e.name.clone(), (!e.name.ends_with(b"/")).then(|| e.data.clone())))
            .collect();
        prop_assert_eq!(&got, &want);

        let mut want2 = Vec::new();
        let mut t = TarGzWriter::new(Vec::new());
        emit(&mut t, &[], &top, chunk, &mut want2);
        let tar = common::read_tar_gz(&t.finish().unwrap());
        let got: Vec<(Vec<u8>, Option<Vec<u8>>)> = tar
            .iter()
            .map(|e| (e.path.clone(), (e.typeflag == b'0').then(|| e.data.clone())))
            .collect();
        prop_assert_eq!(got, want2);
    }
}
