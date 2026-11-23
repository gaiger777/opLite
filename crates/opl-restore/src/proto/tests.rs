use super::*;

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}

fn resp(stream: u32, r: &Reply) -> Frame {
    Frame::json(stream, FrameKind::Resp, r)
}

fn data(stream: u32, b: &[u8]) -> Frame {
    Frame::new(stream, FrameKind::Data, b.to_vec())
}

fn file_attrs(size: u64) -> Attrs {
    Attrs {
        kind: EntryKind::File,
        size,
        mtime: 1,
        mtime_nsec: 0,
        mode: 0o644,
        uid: 0,
        gid: 0,
        nlink: 1,
    }
}

#[test]
fn frame_layout() {
    let f = Frame::new(7, FrameKind::Data, b"abc".to_vec());
    assert_eq!(f.encode(), [0, 0, 0, 12, 0, 0, 0, 7, 3, b'a', b'b', b'c']);
    let mut d = FrameDecoder::new();
    d.push(&f.encode()[..5]);
    assert_eq!(d.next_frame().unwrap(), None);
    assert!(d.finish().is_err());
    d.push(&f.encode()[5..]);
    assert_eq!(d.next_frame().unwrap(), Some(f));
    assert_eq!(d.next_frame().unwrap(), None);
    d.finish().unwrap();
}

#[test]
fn decoder_rejects_bad_headers_before_allocating() {
    let cases: &[(&[u8], FrameError)] = &[
        (&[0, 0, 0, 8, 0, 0, 0, 1, 2], FrameError::Short(8)),
        (
            &[0, 0x10, 0, 10, 0, 0, 0, 1, 2],
            FrameError::Long(0x0010_000a),
        ),
        (
            &[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 1, 2],
            FrameError::Long(u32::MAX),
        ),
        (&[0, 0, 0, 9, 0, 0, 0, 1, 9], FrameError::Kind(9)),
        (&[0, 0, 0, 9, 0, 0, 0, 1, 0], FrameError::Kind(0)),
        (&[0, 0, 0, 10, 0, 0, 0, 1, 4], FrameError::EndPayload(1)),
    ];
    for (bytes, want) in cases {
        let mut d = FrameDecoder::new();
        d.push(bytes);
        assert_eq!(d.next_frame().unwrap_err(), *want);
        // 한 번 틀리면 계속 틀린다.
        d.push(&Frame::end(1).encode());
        assert_eq!(d.next_frame().unwrap_err(), *want);
        let mut r: &[u8] = bytes;
        assert!(matches!(read_frame(&mut r), Err(ReadError::Frame(e)) if e == *want));
    }
    // 한도 꼭 맞는 틀은 받는다.
    let max = Frame::new(1, FrameKind::Data, vec![7; MAX_PAYLOAD]);
    let mut r: &[u8] = &max.encode();
    assert_eq!(read_frame(&mut r).unwrap(), Some(max));
}

#[test]
fn read_frame_boundaries() {
    let mut r: &[u8] = &[];
    assert!(read_frame(&mut r).unwrap().is_none());
    let bytes = Frame::end(3).encode();
    let mut r: &[u8] = &bytes[..4];
    assert!(matches!(
        read_frame(&mut r),
        Err(ReadError::Frame(FrameError::Truncated))
    ));
    let bytes = Frame::new(3, FrameKind::Data, vec![1; 10]).encode();
    let mut r: &[u8] = &bytes[..15];
    assert!(matches!(
        read_frame(&mut r),
        Err(ReadError::Frame(FrameError::Truncated))
    ));
}

#[test]
fn names_and_json_shapes() {
    assert_eq!(Name::new(""), Err(NameError::Empty));
    assert_eq!(Name::new("."), Err(NameError::Dot));
    assert_eq!(Name::new(".."), Err(NameError::Dot));
    assert_eq!(Name::new("a/b"), Err(NameError::Separator));
    assert_eq!(Name::new(b"a\0".to_vec()), Err(NameError::Separator));
    assert!(Name::new("x".repeat(MAX_NAME_BYTES)).is_ok());
    assert_eq!(
        Name::new("x".repeat(MAX_NAME_BYTES + 1)),
        Err(NameError::Long)
    );
    assert_eq!(serde_json::to_string(&n("a b")).unwrap(), "\"a b\"");
    let raw = Name::new(b"caf\xe9".to_vec()).unwrap();
    assert_eq!(
        serde_json::to_string(&raw).unwrap(),
        r#"{"b64":"Y2Fm6Q=="}"#
    );
    assert_eq!(
        serde_json::from_str::<Name>(r#"{"b64":"Y2Fm6Q=="}"#).unwrap(),
        raw
    );
    for bad in [
        r#""..""#,
        r#""a/b""#,
        r#""""#,
        r#"{"b64":"Li4="}"#,
        r#"{"b64":"!!"}"#,
        r#"{"b64":"YQ==","x":1}"#,
    ] {
        assert!(serde_json::from_str::<Name>(bad).is_err(), "{bad}");
    }
    let long = format!("\"{}\"", "y".repeat(MAX_NAME_BYTES + 1));
    assert!(serde_json::from_str::<Name>(&long).is_err());
    assert!(serde_json::from_str::<LinkTarget>(r#""""#).is_err());
    let lt = format!("\"{}\"", "/".repeat(MAX_LINK_TARGET + 1));
    assert!(serde_json::from_str::<LinkTarget>(&lt).is_err());
}

#[test]
fn request_json() {
    let r = Request::List {
        volume: "disk0p2".into(),
        path: vec![n("etc"), n("nginx")],
        cursor: None,
        limit: 100,
    };
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"op":"list","volume":"disk0p2","path":["etc","nginx"],"limit":100}"#
    );
    assert_eq!(
        serde_json::to_string(&Request::Probe).unwrap(),
        r#"{"op":"probe"}"#
    );
    let w: Request =
        serde_json::from_str(r#"{"op":"walk","volume":"disk0","paths":[["a"],[]]}"#).unwrap();
    assert_eq!(
        w,
        Request::Walk {
            volume: "disk0".into(),
            paths: vec![vec![n("a")], vec![]],
            hardlinks: false
        }
    );
    assert!(serde_json::from_str::<Request>(r#"{"op":"format"}"#).is_err());
}

#[test]
fn volume_ids_match_the_rules() {
    for ok in [
        "disk0",
        "disk12",
        "disk0p1",
        "disk3p15",
        "lvm/rl/root",
        "lvm/a+b/c.d_e-f",
    ] {
        assert!(valid_volume_id(ok), "{ok}");
    }
    for bad in [
        "",
        "disk",
        "disk01",
        "disk0p",
        "disk0p01",
        "lvm/rl",
        "lvm/-a/b",
        "lvm/a/b/c",
        "lvm/a b/c",
        "x",
    ] {
        assert!(!valid_volume_id(bad), "{bad}");
    }
    assert_eq!(
        lv_volume_id("rl", "root", "u1", "u2").as_deref(),
        Some("lvm/rl/root")
    );
    assert_eq!(
        lv_volume_id("r l", "root", "u1", "u2").as_deref(),
        Some("lvm/u1/u2")
    );
    assert_eq!(lv_volume_id("r l", "root", "u 1", "u2"), None);
}

#[test]
fn read_stream_counts_bytes() {
    let req = Request::Read {
        volume: "disk0".into(),
        path: vec![n("f")],
    };
    let mut c = StreamCheck::new(&req, 10);
    c.feed(resp(1, &Reply::FileHead { size: 5 })).unwrap();
    assert!(
        c.clone().feed(Frame::end(1)).is_err(),
        "end before the data"
    );
    c.feed(data(1, b"abc")).unwrap();
    assert!(
        c.clone().feed(data(1, b"xyz")).is_err(),
        "more than declared"
    );
    assert!(c.clone().feed(data(1, b"")).is_err(), "empty data frame");
    c.feed(data(1, b"de")).unwrap();
    assert!(c.clone().feed(data(1, b"x")).is_err());
    assert_eq!(c.feed(Frame::end(1)).unwrap(), Event::End);
    assert!(c.done());
    assert!(c.feed(Frame::end(1)).is_err(), "frame after the end");

    let mut c = StreamCheck::new(&req, 10);
    c.feed(resp(1, &Reply::FileHead { size: 0 })).unwrap();
    c.feed(Frame::end(1)).unwrap();

    // 오류는 어느 때나 흐름을 끝낸다.
    let mut c = StreamCheck::new(&req, 10);
    c.feed(resp(1, &Reply::FileHead { size: 5 })).unwrap();
    c.feed(data(1, b"ab")).unwrap();
    let err = AgentError::new(ErrorCode::Io, "read error");
    assert_eq!(
        c.feed(Frame::json(1, FrameKind::Err, &err)).unwrap(),
        Event::Failed(err)
    );
    assert!(c.done());
}

#[test]
fn replies_must_match_the_request() {
    let probe = StreamCheck::new(&Request::Probe, 10);
    for wrong in [
        resp(1, &Reply::FileHead { size: 1 }),
        resp(1, &Reply::Mounted { note: None }),
        data(1, b"x"),
        Frame::end(1),
        Frame::json(1, FrameKind::Req, &Request::Probe),
        Frame::new(1, FrameKind::Resp, b"{\"type\":\"surprise\"}".to_vec()),
        Frame::new(1, FrameKind::Resp, b"not json".to_vec()),
        Frame::new(1, FrameKind::Err, b"{}".to_vec()),
    ] {
        assert!(probe.clone().feed(wrong.clone()).is_err(), "{wrong:?}");
    }
    let mut c = StreamCheck::new(&Request::Cancel { stream: 3 }, 0);
    assert_eq!(c.feed(Frame::end(1)).unwrap(), Event::End);
    // 같은 답 두 번
    let mut c = StreamCheck::new(
        &Request::Mount {
            volume: "disk0".into(),
        },
        0,
    );
    c.feed(resp(1, &Reply::Mounted { note: None })).unwrap();
    assert!(c.feed(resp(1, &Reply::Mounted { note: None })).is_err());
}

fn volume(id: &str) -> Volume {
    Volume {
        id: id.into(),
        disk: 0,
        kind: VolumeKind::Disk,
        size: 1,
        partition: None,
        fs: Some("ext4".into()),
        label: None,
        uuid: None,
        mountable: true,
        note: None,
    }
}

#[test]
fn volume_limits() {
    let check = |vols: Vec<Volume>| {
        StreamCheck::new(&Request::Probe, 0).feed(resp(1, &Reply::Volumes { volumes: vols }))
    };
    check(
        (0..MAX_VOLUMES as u32)
            .map(|i| volume(&format!("disk{i}")))
            .collect(),
    )
    .unwrap();
    assert!(
        check(
            (0..=MAX_VOLUMES as u32)
                .map(|i| volume(&format!("disk{i}")))
                .collect()
        )
        .is_err()
    );
    assert!(check(vec![volume("disk0"), volume("disk0")]).is_err());
    assert!(check(vec![volume("../etc")]).is_err());
    let mut v = volume("disk0");
    v.label = Some("l".repeat(MAX_TEXT + 1));
    assert!(check(vec![v]).is_err());
}

fn entry(name: &str) -> Entry {
    Entry {
        name: n(name),
        attrs: file_attrs(1),
        link_target: None,
    }
}

#[test]
fn page_limits_and_order() {
    let list = |limit| Request::List {
        volume: "disk0".into(),
        path: vec![],
        cursor: None,
        limit,
    };
    let page = |entries: Vec<Entry>, next: Option<&str>| {
        resp(
            1,
            &Reply::Page {
                entries,
                next: next.map(n),
            },
        )
    };
    let ok = |limit, p| StreamCheck::new(&list(limit), 0).feed(p);
    ok(2, page(vec![entry("a"), entry("b")], Some("b"))).unwrap();
    ok(2, page(vec![entry("a"), entry("b")], None)).unwrap();
    assert!(
        ok(1, page(vec![entry("a"), entry("b")], None)).is_err(),
        "over the limit"
    );
    assert!(
        ok(5, page(vec![entry("b"), entry("a")], None)).is_err(),
        "not sorted"
    );
    assert!(
        ok(5, page(vec![entry("a"), entry("a")], None)).is_err(),
        "duplicate"
    );
    assert!(
        ok(5, page(vec![entry("a"), entry("b")], Some("a"))).is_err(),
        "cursor"
    );
    // 한도 0·과대 요청은 [1, 1000]으로
    let many: Vec<Entry> = (0..MAX_PAGE_ENTRIES + 1)
        .map(|i| entry(&format!("{i:05}")))
        .collect();
    assert!(ok(5000, page(many[..MAX_PAGE_ENTRIES].to_vec(), None)).is_ok());
    assert!(ok(5000, page(many, None)).is_err());
    let mut sym = entry("s");
    sym.attrs.kind = EntryKind::Symlink;
    assert!(
        ok(5, page(vec![sym.clone()], None)).is_err(),
        "symlink without target"
    );
    sym.link_target = LinkTarget::new("/x");
    ok(5, page(vec![sym], None)).unwrap();
    let mut bad = entry("m");
    bad.attrs.mode = 0o170000;
    assert!(ok(5, page(vec![bad], None)).is_err(), "mode bits");
}

fn item(path: &[&str], attrs: Attrs) -> Reply {
    Reply::Item(Item {
        path: path.iter().map(|p| n(p)).collect(),
        attrs,
        link_target: None,
        hardlink: None,
    })
}

#[test]
fn walk_stream() {
    let req = Request::Walk {
        volume: "disk0".into(),
        paths: vec![vec![n("d")]],
        hardlinks: true,
    };
    let dir = Attrs {
        kind: EntryKind::Dir,
        ..file_attrs(0)
    };
    let mut c = StreamCheck::new(&req, 4);
    c.feed(resp(1, &item(&["d"], dir))).unwrap();
    c.feed(resp(1, &item(&["d", "a"], file_attrs(3)))).unwrap();
    assert!(
        c.clone()
            .feed(resp(1, &item(&["d", "b"], file_attrs(0))))
            .is_err(),
        "item inside data"
    );
    c.feed(resp(
        1,
        &Reply::Progress {
            entries: 2,
            bytes: 0,
        },
    ))
    .unwrap();
    c.feed(data(1, b"ab")).unwrap();
    c.feed(resp(
        1,
        &Reply::FileError {
            message: "EIO".into(),
        },
    ))
    .unwrap();
    assert!(
        c.clone()
            .feed(resp(
                1,
                &Reply::FileError {
                    message: "x".into()
                }
            ))
            .is_err()
    );
    let mut h = Item {
        path: vec![n("d"), n("h")],
        attrs: file_attrs(3),
        link_target: None,
        hardlink: Some(vec![n("d"), n("a")]),
    };
    c.feed(resp(1, &Reply::Item(h.clone()))).unwrap();
    // 하드 링크 뒤에는 데이터가 없다.
    assert!(c.clone().feed(data(1, b"x")).is_err());
    c.feed(resp(
        1,
        &Reply::Skipped {
            path: vec![n("d"), n("x")],
            reason: "EACCES".into(),
        },
    ))
    .unwrap();
    c.feed(resp(1, &item(&["d", "e"], file_attrs(0)))).unwrap();
    assert_eq!(c.entries(), 4);
    assert!(
        c.clone()
            .feed(resp(1, &item(&["d", "f"], file_attrs(0))))
            .is_err(),
        "entry limit"
    );
    c.feed(Frame::end(1)).unwrap();

    let mut c = StreamCheck::new(&req, 100);
    assert!(
        c.clone()
            .feed(resp(
                1,
                &Reply::Item(Item {
                    path: vec![],
                    ..h.clone()
                })
            ))
            .is_err()
    );
    h.attrs.kind = EntryKind::Dir;
    assert!(
        c.clone().feed(resp(1, &Reply::Item(h))).is_err(),
        "hard link must be a file"
    );
    let deep: Vec<String> = (0..=MAX_PATH_DEPTH).map(|i| i.to_string()).collect();
    let deep: Vec<&str> = deep.iter().map(String::as_str).collect();
    assert!(c.clone().feed(resp(1, &item(&deep, dir))).is_err(), "depth");
    assert!(
        c.clone()
            .feed(resp(1, &item(&deep[..MAX_PATH_DEPTH], dir)))
            .is_ok()
    );
    c.feed(resp(1, &item(&["a"], file_attrs(2)))).unwrap();
    assert!(c.feed(Frame::end(1)).is_err(), "ended inside a file");
}

#[test]
fn hello_check() {
    let mut h = Hello {
        proto: PROTO_VERSION,
        image_version: "orbivisor 0.1.0".into(),
        kernel: "6.12.57".into(),
    };
    check_hello(&h).unwrap();
    h.proto = 2;
    assert!(check_hello(&h).is_err());
}
