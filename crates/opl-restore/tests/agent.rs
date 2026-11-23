//! 에이전트 몸통을 호스트에서(설계 §14): 실제 통신 규약(소켓 쌍)으로 탐지·마운트·나열·읽기·걷기·취소·동시 흐름을
//! 돌리고, 받은 틀은 opld 쪽 검사기([`StreamCheck`])를 지나게 한다. 파일시스템은 임시 디렉터리([`DirFs`] — 심볼릭
//! 링크로 루트 밖에 닿지 못함)와 메모리 나무([`MemFs`] — UTF-8이 아닌 이름·특수 파일·읽기 오류).

mod common;

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use opl_restore::agent::fs::path_of;
use opl_restore::agent::{DirFs, FixedVolumes, Fs, MemFs, ServeOptions, serve};
use opl_restore::archive::tar::TarGzWriter;
use opl_restore::archive::zip::ZipWriter;
use opl_restore::archive::{Archive, Feeder};
use opl_restore::proto::{
    AgentError, Attrs, EntryKind, ErrorCode, Event, Frame, FrameKind, Hello, Name, PROTO_VERSION,
    Reply, Request, StreamCheck, Volume, VolumeKind, read_frame, write_frame,
};

fn volume(id: &str, fs: Option<&str>) -> Volume {
    Volume {
        id: id.into(),
        disk: 0,
        kind: VolumeKind::Partition,
        size: 1 << 30,
        partition: None,
        fs: fs.map(Into::into),
        label: None,
        uuid: None,
        mountable: fs.is_some(),
        note: None,
    }
}

struct Client {
    sock: UnixStream,
    next: u32,
    /// 다른 흐름의 틀(나중에 읽음)
    parked: HashMap<u32, Vec<Frame>>,
    agent: Option<JoinHandle<std::io::Result<()>>>,
}

fn start(fs: Arc<dyn Fs>, opts: ServeOptions) -> (Client, Hello) {
    let volumes = FixedVolumes::new(vec![
        (volume("disk0p1", Some("ext4")), Some(fs)),
        (volume("disk0p2", None), None),
    ]);
    let (ours, theirs) = UnixStream::pair().unwrap();
    let reader = theirs.try_clone().unwrap();
    let hello = Hello {
        proto: PROTO_VERSION,
        image_version: "test".into(),
        kernel: "host".into(),
    };
    let h = hello.clone();
    let agent = std::thread::spawn(move || serve(Arc::new(volumes), h, reader, theirs, opts));
    let mut c = Client {
        sock: ours,
        next: 1,
        parked: HashMap::new(),
        agent: Some(agent),
    };
    let f = read_frame(&mut c.sock).unwrap().unwrap();
    assert_eq!((f.stream, f.kind), (0, FrameKind::Resp));
    let Reply::Hello(got) = f.parse::<Reply>().unwrap() else {
        panic!("hello first");
    };
    assert_eq!(got, hello);
    (c, got)
}

impl Client {
    fn send(&mut self, req: &Request) -> u32 {
        let id = self.next;
        self.next += 1;
        write_frame(&mut self.sock, &Frame::json(id, FrameKind::Req, req)).unwrap();
        id
    }

    fn frame_for(&mut self, id: u32) -> Frame {
        if let Some(q) = self.parked.get_mut(&id)
            && !q.is_empty()
        {
            return q.remove(0);
        }
        loop {
            let f = read_frame(&mut self.sock).unwrap().expect("agent closed");
            if f.stream == id {
                return f;
            }
            self.parked.entry(f.stream).or_default().push(f);
        }
    }

    /// 흐름 하나를 끝까지(검사기를 지나게)
    fn collect(&mut self, id: u32, req: &Request) -> Vec<Event> {
        let mut check = StreamCheck::new(req, 100_000);
        let mut out = Vec::new();
        while !check.done() {
            let f = self.frame_for(id);
            out.push(check.feed(f).expect("agent output passes the opld checks"));
        }
        out
    }

    fn call(&mut self, req: Request) -> Vec<Event> {
        let id = self.send(&req);
        self.collect(id, &req)
    }

    fn shutdown(mut self) {
        let events = self.call(Request::Shutdown);
        assert_eq!(events, [Event::End]);
        self.agent.take().unwrap().join().unwrap().unwrap();
    }
}

fn failed(events: &[Event]) -> &AgentError {
    match events.last() {
        Some(Event::Failed(e)) => e,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn data(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Data(d) => Some(d.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

fn list(c: &mut Client, path: &str, cursor: Option<Name>, limit: u32) -> Vec<Event> {
    c.call(Request::List {
        volume: "disk0p1".into(),
        path: path_of(path),
        cursor,
        limit,
    })
}

#[test]
fn probe_and_mount() {
    let (mut c, _) = start(Arc::new(MemFs::new()), ServeOptions::default());
    let ev = c.call(Request::Probe);
    let Event::Reply(Reply::Volumes { volumes }) = &ev[0] else {
        panic!("{ev:?}");
    };
    assert_eq!(volumes.len(), 2);
    assert_eq!(
        c.call(Request::Mount {
            volume: "disk0p1".into()
        })[0],
        Event::Reply(Reply::Mounted { note: None })
    );
    assert_eq!(
        failed(&c.call(Request::Mount {
            volume: "disk7".into()
        }))
        .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        failed(&c.call(Request::Mount {
            volume: "disk0p2".into()
        }))
        .code,
        ErrorCode::NotMountable
    );
    assert_eq!(
        list(&mut c, "", None, 10)[0],
        Event::Reply(Reply::Page {
            entries: vec![],
            next: None
        })
    );
    c.shutdown();
}

fn tmp_tree() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("vol");
    std::fs::create_dir_all(r.join("etc/nginx")).unwrap();
    for i in 0..25 {
        std::fs::write(r.join(format!("etc/f{i:02}")), format!("file {i}")).unwrap();
    }
    std::fs::write(
        r.join("etc/nginx/nginx.conf"),
        b"worker_processes 1;\n".repeat(1000),
    )
    .unwrap();
    std::fs::write(r.join("etc/nginx/mime.types"), b"text/html html;\n").unwrap();
    std::fs::hard_link(
        r.join("etc/nginx/mime.types"),
        r.join("etc/nginx/mime.copy"),
    )
    .unwrap();
    // 볼륨 밖
    std::fs::create_dir_all(t.path().join("outside")).unwrap();
    std::fs::write(t.path().join("outside/secret"), b"host secret").unwrap();
    std::os::unix::fs::symlink(t.path().join("outside"), r.join("etc/out")).unwrap();
    std::os::unix::fs::symlink("../../outside/secret", r.join("etc/nginx/rel")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", r.join("passwd")).unwrap();
    t
}

#[test]
fn dirfs_list_pages_in_name_order() {
    let t = tmp_tree();
    let fs = Arc::new(DirFs::open(&t.path().join("vol")).unwrap());
    let (mut c, _) = start(fs, ServeOptions::default());
    let mut names = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let ev = list(&mut c, "etc", cursor.clone(), 10);
        let Event::Reply(Reply::Page { entries, next }) = &ev[0] else {
            panic!("{ev:?}");
        };
        pages += 1;
        names.extend(entries.iter().map(|e| e.name.lossy()));
        for e in entries {
            if e.name.lossy() == "out" {
                assert_eq!(e.attrs.kind, EntryKind::Symlink);
                assert!(
                    e.link_target
                        .as_ref()
                        .unwrap()
                        .as_bytes()
                        .ends_with(b"outside")
                );
            }
            if e.name.lossy() == "f03" {
                assert_eq!((e.attrs.kind, e.attrs.size), (EntryKind::File, 6));
            }
        }
        match next {
            Some(n) => cursor = Some(n.clone()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    let mut want: Vec<String> = (0..25).map(|i| format!("f{i:02}")).collect();
    want.extend(["nginx".into(), "out".into()]);
    want.sort();
    assert_eq!(names, want);
    // 루트의 Stat, 디렉터리 아닌 것의 나열
    let ev = c.call(Request::Stat {
        volume: "disk0p1".into(),
        path: vec![],
    });
    assert!(matches!(&ev[0], Event::Reply(Reply::Entry(e)) if e.attrs.kind == EntryKind::Dir));
    assert_eq!(
        failed(&list(&mut c, "etc/f01", None, 10)).code,
        ErrorCode::NotDir
    );
    assert_eq!(
        failed(&list(&mut c, "nope", None, 10)).code,
        ErrorCode::NotFound
    );
    c.shutdown();
}

#[test]
fn dirfs_symlinks_never_lead_outside() {
    let t = tmp_tree();
    let fs = Arc::new(DirFs::open(&t.path().join("vol")).unwrap());
    let (mut c, _) = start(fs, ServeOptions::default());
    let read = |c: &mut Client, p: &str| {
        c.call(Request::Read {
            volume: "disk0p1".into(),
            path: path_of(p),
        })
    };
    for p in ["etc/out/secret", "etc/nginx/rel", "passwd", "etc/out"] {
        let ev = read(&mut c, p);
        let e = failed(&ev);
        assert!(
            matches!(
                e.code,
                ErrorCode::Symlink | ErrorCode::NotDir | ErrorCode::NotFile
            ),
            "{p}: {e:?}"
        );
        assert!(data(&ev).is_empty());
    }
    let e = failed(&list(&mut c, "etc/out", None, 10)).code;
    assert!(matches!(e, ErrorCode::NotDir | ErrorCode::Symlink), "{e:?}");
    // 걷기도 링크를 항목으로만 보인다.
    let ev = c.call(Request::Walk {
        volume: "disk0p1".into(),
        paths: vec![path_of("etc/out"), path_of("etc/nginx")],
        hardlinks: false,
    });
    let all = data(&ev);
    assert!(!all.windows(11).any(|w| w == b"host secret"));
    let ok = read(&mut c, "etc/nginx/nginx.conf");
    assert_eq!(data(&ok), b"worker_processes 1;\n".repeat(1000));
    c.shutdown();
}

/// 걷기 사건을 아카이브로
fn archive<A: Archive>(a: &mut A, events: &[Event]) {
    let mut f = Feeder::new();
    for e in events {
        f.feed(a, e).unwrap();
    }
}

#[test]
fn dirfs_walk_to_tar_and_zip() {
    let t = tmp_tree();
    let fs = Arc::new(DirFs::open(&t.path().join("vol")).unwrap());
    let (mut c, _) = start(fs, ServeOptions::default());
    let walk = |c: &mut Client, hardlinks| {
        c.call(Request::Walk {
            volume: "disk0p1".into(),
            paths: vec![path_of("etc/nginx")],
            hardlinks,
        })
    };
    let ev = walk(&mut c, true);
    let items: Vec<(String, EntryKind, bool)> = ev
        .iter()
        .filter_map(|e| match e {
            Event::Reply(Reply::Item(i)) => Some((
                opl_restore::proto::display_path(&i.path),
                i.attrs.kind,
                i.hardlink.is_some(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        items,
        [
            ("/nginx".into(), EntryKind::Dir, false),
            ("/nginx/mime.copy".into(), EntryKind::File, false),
            ("/nginx/mime.types".into(), EntryKind::File, true),
            ("/nginx/nginx.conf".into(), EntryKind::File, false),
            ("/nginx/rel".into(), EntryKind::Symlink, false),
        ]
    );
    let mut tar = TarGzWriter::new(Vec::new());
    archive(&mut tar, &ev);
    let tar = common::read_tar_gz(&tar.finish().unwrap());
    let summary: Vec<(String, u8, String)> = tar
        .iter()
        .map(|e| {
            (
                String::from_utf8_lossy(&e.path).into_owned(),
                e.typeflag,
                String::from_utf8_lossy(&e.link).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("nginx/".into(), b'5', String::new()),
            ("nginx/mime.copy".into(), b'0', String::new()),
            ("nginx/mime.types".into(), b'1', "nginx/mime.copy".into()),
            ("nginx/nginx.conf".into(), b'0', String::new()),
            ("nginx/rel".into(), b'2', "../../outside/secret".into()),
        ]
    );
    assert_eq!(tar[1].data, b"text/html html;\n");

    // zip은 하드 링크 대신 내용을 다시 받고, 링크는 보고서로
    let ev = walk(&mut c, false);
    let mut zip = ZipWriter::new(Vec::new());
    archive(&mut zip, &ev);
    let zip = common::read_zip(&zip.finish().unwrap());
    let names: Vec<String> = zip
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(&e.name).into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "nginx/",
            "nginx/mime.copy",
            "nginx/mime.types",
            "nginx/nginx.conf",
            "ORBIVISOR-RESTORE-REPORT.txt"
        ]
    );
    assert_eq!(zip.entries[2].data, b"text/html html;\n");
    c.shutdown();
}

fn mem_tree() -> Arc<MemFs> {
    let m = MemFs::new();
    m.file(&path_of("home/a.txt"), b"alpha".to_vec());
    m.file(
        &[
            Name::new("home").unwrap(),
            Name::new(b"r\xe9sum\xe9".to_vec()).unwrap(),
        ],
        b"bytes".to_vec(),
    );
    m.special(&path_of("home/dev"));
    m.file_with(
        &path_of("home/broken"),
        vec![7u8; 10_000],
        Some(3000),
        Attrs {
            kind: EntryKind::File,
            size: 0,
            mtime: 5,
            mtime_nsec: 0,
            mode: 0o600,
            uid: 1,
            gid: 1,
            nlink: 1,
        },
    );
    m.file(&path_of("home/z"), b"last".to_vec());
    m.symlink(&path_of("home/link"), b"/etc".to_vec());
    m.file(&path_of("big"), vec![1u8; 4 << 20]);
    Arc::new(m)
}

#[test]
fn memfs_names_errors_and_walk() {
    let (mut c, _) = start(mem_tree(), ServeOptions::default());
    let ev = list(&mut c, "home", None, 1000);
    let Event::Reply(Reply::Page { entries, .. }) = &ev[0] else {
        panic!();
    };
    let raw = entries
        .iter()
        .find(|e| e.name.as_bytes() == b"r\xe9sum\xe9")
        .unwrap();
    assert_eq!(raw.attrs.kind, EntryKind::File);
    assert_eq!(
        entries
            .iter()
            .find(|e| e.name.lossy() == "dev")
            .unwrap()
            .attrs
            .kind,
        EntryKind::Other
    );
    // 읽기 오류: 데이터 일부 뒤 실패
    let ev = c.call(Request::Read {
        volume: "disk0p1".into(),
        path: path_of("home/broken"),
    });
    assert_eq!(failed(&ev).code, ErrorCode::Io);
    assert_eq!(data(&ev).len(), 3000);
    assert_eq!(
        failed(&c.call(Request::Read {
            volume: "disk0p1".into(),
            path: path_of("home/dev")
        }))
        .code,
        ErrorCode::NotFile
    );
    assert_eq!(
        failed(&c.call(Request::Read {
            volume: "disk0p1".into(),
            path: path_of("home/link/passwd")
        }))
        .code,
        ErrorCode::Symlink
    );
    // 루트를 걸으면 그 아래 항목들
    let ev = c.call(Request::Walk {
        volume: "disk0p1".into(),
        paths: vec![vec![]],
        hardlinks: false,
    });
    let mut tar = TarGzWriter::new(Vec::new());
    archive(&mut tar, &ev);
    let tar = common::read_tar_gz(&tar.finish().unwrap());
    let paths: Vec<Vec<u8>> = tar.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        paths,
        [
            b"big".to_vec(),
            b"home/".to_vec(),
            b"home/a.txt".to_vec(),
            b"home/broken".to_vec(),
            b"home/link".to_vec(),
            b"home/r\xe9sum\xe9".to_vec(),
            b"home/z".to_vec(),
            b"ORBIVISOR-RESTORE-REPORT.txt".to_vec(),
        ]
    );
    assert_eq!(tar[0].data.len(), 4 << 20);
    assert_eq!(tar[3].data.len(), 10_000, "padded");
    assert_eq!(tar[6].data, b"last", "the walk goes on after a read error");
    let report = String::from_utf8(tar[7].data.clone()).unwrap();
    assert!(report.contains("incomplete: /home/broken"), "{report}");
    assert!(
        report.contains("skipped: /home/dev — special file"),
        "{report}"
    );
    c.shutdown();
}

#[test]
fn duplicate_selection_names_are_skipped() {
    let m = MemFs::new();
    m.file(&path_of("x/a"), b"1".to_vec());
    m.file(&path_of("y/a"), b"2".to_vec());
    let (mut c, _) = start(Arc::new(m), ServeOptions::default());
    let ev = c.call(Request::Walk {
        volume: "disk0p1".into(),
        paths: vec![path_of("x/a"), path_of("y/a")],
        hardlinks: false,
    });
    assert!(ev.iter().any(
        |e| matches!(e, Event::Reply(Reply::Skipped { reason, .. }) if reason.contains("same name"))
    ));
    assert_eq!(data(&ev), b"1");
    c.shutdown();
}

#[test]
fn cancel_stops_a_read() {
    let opts = ServeOptions {
        chunk: 4096,
        ..ServeOptions::default()
    };
    let m = MemFs::new();
    m.file(&path_of("huge"), vec![0u8; 64 << 20]);
    let (mut c, _) = start(Arc::new(m), opts);
    let req = Request::Read {
        volume: "disk0p1".into(),
        path: path_of("huge"),
    };
    let id = c.send(&req);
    let mut check = StreamCheck::new(&req, 0);
    check.feed(c.frame_for(id)).unwrap();
    check.feed(c.frame_for(id)).unwrap();
    let cancel = c.send(&Request::Cancel { stream: id });
    let mut last = None;
    let mut bytes = 0;
    while !check.done() {
        match check.feed(c.frame_for(id)).unwrap() {
            Event::Data(d) => bytes += d.len(),
            e => last = Some(e),
        }
    }
    assert!(bytes < 64 << 20, "stopped early");
    assert!(matches!(
        last,
        Some(Event::Failed(AgentError {
            code: ErrorCode::Cancelled,
            ..
        }))
    ));
    assert_eq!(c.frame_for(cancel), Frame::end(cancel));
    c.shutdown();
}

#[test]
fn concurrent_streams_and_busy() {
    let opts = ServeOptions {
        chunk: 4096,
        max_streams: 2,
        ..ServeOptions::default()
    };
    let m = MemFs::new();
    m.file(&path_of("a"), vec![b'a'; 3 << 20]);
    m.file(&path_of("b"), vec![b'b'; 3 << 20]);
    let (mut c, _) = start(Arc::new(m), opts);
    let ra = Request::Read {
        volume: "disk0p1".into(),
        path: path_of("a"),
    };
    let rb = Request::Read {
        volume: "disk0p1".into(),
        path: path_of("b"),
    };
    let a = c.send(&ra);
    let b = c.send(&rb);
    // 두 흐름이 소켓 버퍼를 채워 막혀 있는 동안 셋째는 거절된다.
    std::thread::sleep(Duration::from_millis(200));
    let third = c.call(Request::Probe);
    assert_eq!(failed(&third).code, ErrorCode::Busy);
    let eb = c.collect(b, &rb);
    let ea = c.collect(a, &ra);
    assert_eq!(data(&ea), vec![b'a'; 3 << 20]);
    assert_eq!(data(&eb), vec![b'b'; 3 << 20]);
    // 끝난 뒤에는 다시 받는다.
    assert!(matches!(
        c.call(Request::Probe)[0],
        Event::Reply(Reply::Volumes { .. })
    ));
    c.shutdown();
}

#[test]
fn progress_while_walking() {
    let opts = ServeOptions {
        progress_every: Duration::ZERO,
        ..ServeOptions::default()
    };
    let m = MemFs::new();
    for i in 0..5 {
        m.file(&path_of(&format!("d/{i}")), b"x".to_vec());
    }
    let (mut c, _) = start(Arc::new(m), opts);
    let ev = c.call(Request::Walk {
        volume: "disk0p1".into(),
        paths: vec![path_of("d")],
        hardlinks: false,
    });
    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::Reply(Reply::Progress { .. })))
    );
    c.shutdown();
}

#[test]
fn bad_requests_and_connection_end() {
    let (mut c, _) = start(Arc::new(MemFs::new()), ServeOptions::default());
    write_frame(
        &mut c.sock,
        &Frame::new(9, FrameKind::Req, b"{\"op\":\"format\"}".to_vec()),
    )
    .unwrap();
    let f = c.frame_for(9);
    assert_eq!(f.kind, FrameKind::Err);
    assert_eq!(f.parse::<AgentError>().unwrap().code, ErrorCode::BadRequest);
    // 읽는 쪽이 닫히면(EOF) 몸통이 돌아온다.
    c.sock.shutdown(std::net::Shutdown::Write).unwrap();
    c.agent.take().unwrap().join().unwrap().unwrap();

    // 요청이 아닌 틀은 연결을 끝낸다.
    let (mut c, _) = start(Arc::new(MemFs::new()), ServeOptions::default());
    write_frame(&mut c.sock, &Frame::end(3)).unwrap();
    assert!(c.agent.take().unwrap().join().unwrap().is_err());
}
