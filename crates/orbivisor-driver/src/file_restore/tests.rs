use std::os::unix::fs::PermissionsExt;

use orbivisor_restore::agent::fs::path_of;
use orbivisor_restore::proto::{ErrorCode, PROTO_VERSION};
use orbivisor_types::file_restore::{EntryPath, VolumeState};
use tokio::net::UnixStream;

use super::*;

fn image() -> CheckedImage {
    CheckedImage {
        arch: Arch::native(),
        kernel: "/img/Image".into(),
        initrd: "/img/initrd.img".into(),
        image_version: "orbivisor 0.1.0 lock-000000000000".into(),
        kernel_release: "6.12".into(),
    }
}

fn launch() -> HelperLaunch {
    HelperLaunch {
        id: Uuid::now_v7(),
        image: image(),
        disks: vec![HelperSource {
            source: LiveRestoreSource::Qcow2 {
                chain: vec!["/bk/disk0.qcow2".into()],
            },
            size: 1 << 30,
        }],
    }
}

fn quick() -> ClientOptions {
    ClientOptions {
        reply_timeout: Duration::from_millis(300),
        ..ClientOptions::default()
    }
}

async fn read_all(s: &mut ReplyStream) -> Result<Vec<u8>, HelperError> {
    let mut out = Vec::new();
    while let Some(item) = s.next().await? {
        match item {
            StreamItem::Data(d) => out.extend(d),
            StreamItem::Reply(r) => panic!("unexpected {r:?}"),
        }
    }
    Ok(out)
}

/// 가짜 도우미(에이전트 몸통 + 메모리 나무)로 탐지·마운트·쪽 나눈 나열·항목·읽기·걷기·오류·끝내기가 실제 규약으로 된다.
#[tokio::test]
async fn the_fake_helper_answers_like_the_agent() {
    let l = launch();
    let helper = fake_helper(&l, crate::mock::mock_restore_volumes(), quick())
        .await
        .unwrap();
    assert_eq!(helper.hello.image_version, l.image.image_version);
    assert_eq!(helper.hello.proto, PROTO_VERSION);
    // 커널 명령줄의 판(orbivisor-qemu)도 같다 — 다르면 실제 에이전트가 곧바로 꺼진다.
    assert_eq!(orbivisor_qemu::HELPER_PROTO, PROTO_VERSION);
    let c = helper.client.clone();

    let volumes = c.probe().await.unwrap();
    let views: Vec<VolumeView> = volumes
        .iter()
        .map(|v| volume_view(v, VolumeState::Unmounted))
        .collect();
    let ids: Vec<&str> = views.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, ["disk0p1", "disk0p2", "lvm/vg0/data", "disk1"]);
    assert_eq!(views[0].kind, fr::VolumeKind::Partition);
    assert_eq!(
        views[0].partition.as_ref().unwrap().table,
        fr::PartitionTable::Gpt
    );
    assert_eq!(views[2].kind, fr::VolumeKind::LogicalVolume);
    assert!(!views[3].mountable);

    assert_eq!(c.mount("disk0p1").await.unwrap(), None);
    let err = c.mount("disk1").await.unwrap_err();
    assert!(
        matches!(&err, HelperError::Agent(e) if e.code == ErrorCode::NotMountable),
        "{err:?}"
    );

    // 한 쪽에 하나씩: 커서는 API에 실을 수 있는 문자열로 오간다.
    let mut names = Vec::new();
    let mut cursor = None;
    loop {
        let (entries, next) = c.list("disk0p1", path_of("etc"), cursor, 1).await.unwrap();
        names.extend(entries.iter().map(|e| entry_view(e).name));
        let Some(next) = next else { break };
        let encoded = encode_cursor(&next);
        cursor = Some(decode_cursor(&encoded).unwrap());
    }
    assert_eq!(names, ["hostname", "localtime", "nginx"]);

    let link = c.stat("disk0p1", path_of("etc/localtime")).await.unwrap();
    let view = entry_view(&link);
    assert_eq!(view.kind, fr::EntryKind::Symlink);
    assert_eq!(view.link_target.as_deref(), Some("/usr/share/zoneinfo/UTC"));

    let (size, mut data) = c.read("disk0p1", path_of("var/log/app.log")).await.unwrap();
    assert_eq!(size, 3 << 20);
    let bytes = read_all(&mut data).await.unwrap();
    assert_eq!(bytes.len() as u64, size);
    assert_eq!(&bytes[..16], b"0123456789abcdef");

    let err = c.read("disk0p1", path_of("etc/nope")).await.unwrap_err();
    assert!(
        matches!(&err, HelperError::Agent(e) if e.code == ErrorCode::NotFound),
        "{err:?}"
    );

    let path = EntryPath::parse("/etc/nginx").unwrap();
    let mut walk = c
        .walk("disk0p1", vec![path_names(&path).unwrap()], false)
        .await
        .unwrap();
    let mut items = Vec::new();
    let mut data_bytes = 0;
    while let Some(item) = walk.next().await.unwrap() {
        match item {
            StreamItem::Reply(Reply::Item(i)) => items.push(proto::display_path(&i.path)),
            StreamItem::Data(d) => data_bytes += d.len(),
            StreamItem::Reply(r) => panic!("unexpected {r:?}"),
        }
    }
    assert_eq!(items, ["/nginx", "/nginx/nginx.conf"]);
    assert_eq!(data_bytes, "worker_processes auto;\n".len());
    assert_eq!(c.active_streams(), 0);

    stop_helper(helper).await;
    let err = c.probe().await.unwrap_err();
    assert!(matches!(err, HelperError::Closed(_)), "{err:?}");
}

/// 세션당 동시 흐름은 4개 — 다섯째는 곧바로 바쁨. 다 받지 않고 버린 흐름은 에이전트에 취소를 보내 자리가 돌아온다.
#[tokio::test]
async fn concurrent_streams_are_limited_and_dropped_streams_are_cancelled() {
    // 창(4 MiB)보다 큰 파일이라야 받지 않는 흐름이 끝나지 않고 남는다.
    let helper = fake_helper(&launch(), big_volumes(), quick())
        .await
        .unwrap();
    let c = helper.client.clone();
    let mut open = Vec::new();
    for _ in 0..4 {
        open.push(c.read("disk0p1", path_of("big")).await.unwrap());
    }
    assert_eq!(c.active_streams(), 4);
    let err = c.probe().await.unwrap_err();
    assert_eq!(err, HelperError::Busy);
    drop(open);
    let freed = crate::qemu::wait_until(Duration::from_secs(5), || c.active_streams() == 0).await;
    assert!(freed, "{} streams still open", c.active_streams());
    assert_eq!(c.probe().await.unwrap().len(), 4);
    assert_eq!(helper.ended_reason(), None);
    stop_helper(helper).await;
}

/// 큰 파일 둘과 건너뛸 것이 많은 디렉터리가 있는 볼륨(`disk0p1`)
fn big_volumes() -> FakeVolumes {
    use orbivisor_restore::agent::MemFs;
    let m = MemFs::new();
    m.file(&path_of("big"), vec![b'b'; 24 << 20])
        .file(&path_of("small.txt"), "hi\n");
    for i in 0..50 {
        m.file(&path_of(&format!("many/f{i:02}")), "x");
    }
    let mut v = crate::mock::mock_restore_volumes();
    v[0].1 = Some(Arc::new(m) as Arc<dyn orbivisor_restore::agent::Fs>);
    v
}

/// 리뷰 1: 받는 쪽이 멈춘 흐름(멈춘 브라우저·느린 게스트 쓰기)은 그 흐름만 기다린다 — 다른 흐름은 바로 답하고, 시간
/// 초과(되풀이 세 번이면 세션 닫힘)를 세지 않으며, 멈춘 흐름은 `reply_timeout`보다 오래 쉬어도 이어서 끝까지 받는다.
#[tokio::test]
async fn a_stalled_stream_does_not_block_the_others() {
    let helper = fake_helper(&launch(), big_volumes(), quick())
        .await
        .unwrap();
    let c = helper.client.clone();
    let (size, mut big) = c.read("disk0p1", path_of("big")).await.unwrap();
    assert_eq!(size, 24 << 20);
    let mut got = match big.next().await.unwrap() {
        Some(StreamItem::Data(d)) => d.len(),
        other => panic!("{other:?}"),
    };
    // 멈춘 흐름 곁에서: reply_timeout(300 ms)의 몇 배 동안 다른 요청이 줄곧 답한다.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1500) {
        let t = Instant::now();
        assert_eq!(c.probe().await.unwrap().len(), 4);
        let (entries, _) = c.list("disk0p1", vec![], None, 100).await.unwrap();
        assert!(entries.iter().any(|e| e.name.as_bytes() == b"small.txt"));
        let (_, mut small) = c.read("disk0p1", path_of("small.txt")).await.unwrap();
        assert_eq!(read_all(&mut small).await.unwrap(), b"hi\n");
        assert!(
            t.elapsed() < Duration::from_millis(250),
            "{:?}",
            t.elapsed()
        );
    }
    // 멈춘 흐름도 이어서 끝까지
    got += read_all(&mut big).await.unwrap().len();
    assert_eq!(got, 24 << 20);
    assert_eq!(c.closed_reason(), None);
    stop_helper(helper).await;
}

/// 리뷰 1: 걷기 한도(항목 수 — 건너뜀도 셈)를 넘으면 그 흐름만 `Limit`으로 끝나고 취소된다. 세션은 그대로다.
#[tokio::test]
async fn a_walk_over_the_limit_ends_only_that_stream() {
    let opts = ClientOptions {
        max_entries: 10,
        ..quick()
    };
    let helper = fake_helper(&launch(), big_volumes(), opts).await.unwrap();
    let c = helper.client.clone();
    let mut w = c
        .walk("disk0p1", vec![path_of("many")], false)
        .await
        .unwrap();
    let err = loop {
        match w.next().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the walk should stop"),
            Err(e) => break e,
        }
    };
    assert!(
        matches!(&err, HelperError::Limit(m) if m.contains("more than 10 entries")),
        "{err:?}"
    );
    let freed = crate::qemu::wait_until(Duration::from_secs(5), || c.active_streams() == 0).await;
    assert!(freed, "{} streams still open", c.active_streams());
    assert_eq!(c.probe().await.unwrap().len(), 4);
    assert_eq!(c.closed_reason(), None);

    // 경로 바이트 한도도 같다.
    let opts = ClientOptions {
        max_path_bytes: 64,
        ..quick()
    };
    let helper2 = fake_helper(&launch(), big_volumes(), opts).await.unwrap();
    let mut w = helper2
        .client
        .walk("disk0p1", vec![path_of("many")], false)
        .await
        .unwrap();
    let err = loop {
        match w.next().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the walk should stop"),
            Err(e) => break e,
        }
    };
    assert!(
        matches!(&err, HelperError::Limit(m) if m.contains("MiB of path names")),
        "{err:?}"
    );
    assert_eq!(helper2.client.closed_reason(), None);
    stop_helper(helper2).await;
    stop_helper(helper).await;
}

/// 손으로 답하는 가짜 에이전트(규약 위반·침묵 시험)
struct RawAgent {
    sock: UnixStream,
    dec: FrameDecoder,
}

impl RawAgent {
    async fn send(&mut self, frame: Frame) {
        self.sock.write_all(&frame.encode()).await.unwrap();
    }

    async fn hello(&mut self, proto: u32) {
        let hello = Reply::Hello(Hello {
            proto,
            image_version: "v".into(),
            kernel: "k".into(),
        });
        self.send(Frame::json(0, FrameKind::Resp, &hello)).await;
    }

    /// 다음 요청 틀
    async fn request(&mut self) -> Option<(u32, Request)> {
        let mut buf = [0u8; 4096];
        loop {
            if let Some(f) = self.dec.next_frame().unwrap() {
                return Some((f.stream, f.parse().unwrap()));
            }
            let n = self.sock.read(&mut buf).await.ok()?;
            if n == 0 {
                return None;
            }
            self.dec.push(&buf[..n]);
        }
    }
}

fn raw_pair() -> (UnixStream, RawAgent) {
    let (a, b) = UnixStream::pair().unwrap();
    (
        a,
        RawAgent {
            sock: b,
            dec: FrameDecoder::new(),
        },
    )
}

#[tokio::test]
async fn hello_must_come_first_and_speak_our_protocol() {
    // 아무 말도 없으면 시간 초과
    let (ours, _agent) = raw_pair();
    let err = HelperClient::connect(ours, Duration::from_millis(100), quick())
        .await
        .unwrap_err();
    assert_eq!(err, HelperError::Timeout);

    // 다른 판
    let (ours, mut agent) = raw_pair();
    agent.hello(PROTO_VERSION + 1).await;
    let err = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, HelperError::Closed(m) if m.contains("protocol")),
        "{err:?}"
    );

    // 첫 틀이 인사가 아님
    let (ours, mut agent) = raw_pair();
    agent
        .send(Frame::json(
            0,
            FrameKind::Resp,
            &Reply::Mounted { note: None },
        ))
        .await;
    let err = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap_err();
    assert!(matches!(err, HelperError::Closed(_)), "{err:?}");

    // 인사 전에 닫음
    let (ours, agent) = raw_pair();
    drop(agent);
    let err = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, HelperError::Closed(m) if m.contains("before saying hello")),
        "{err:?}"
    );
}

/// 답하지 않는 요청은 시간 초과로 취소하고, 세 번 이어지면 세션을 닫는다.
#[tokio::test]
async fn silent_requests_time_out_and_three_strikes_close_the_session() {
    let (ours, mut agent) = raw_pair();
    agent.hello(PROTO_VERSION).await;
    let (c, _) = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap();
    let seen = tokio::spawn(async move {
        let mut reqs = Vec::new();
        while let Some((_, r)) = agent.request().await {
            reqs.push(r);
        }
        reqs
    });
    for i in 0..3 {
        let err = c.probe().await.unwrap_err();
        assert_eq!(err, HelperError::Timeout, "try {i}");
    }
    let reason = c.closed_reason().expect("closed after three strikes");
    assert!(reason.contains("did not answer 3 requests"), "{reason}");
    let err = c.probe().await.unwrap_err();
    assert!(matches!(err, HelperError::Closed(_)), "{err:?}");
    drop(c);
    let reqs = tokio::time::timeout(Duration::from_secs(5), seen)
        .await
        .unwrap()
        .unwrap();
    // 요청마다 취소가 뒤따랐다.
    let cancels = reqs
        .iter()
        .filter(|r| matches!(r, Request::Cancel { .. }))
        .count();
    assert_eq!(cancels, 3, "{reqs:?}");
}

/// 요청에 맞지 않는 답, 모르는 흐름의 틀, 채널 끊김은 세션을 닫는다(까닭이 남는다).
#[tokio::test]
async fn protocol_violations_close_the_session() {
    // 탐지에 쪽으로 답함
    let (ours, mut agent) = raw_pair();
    agent.hello(PROTO_VERSION).await;
    let (c, _) = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap();
    let answer = tokio::spawn(async move {
        let (id, _) = agent.request().await.unwrap();
        let page = Reply::Page {
            entries: vec![],
            next: None,
        };
        agent.send(Frame::json(id, FrameKind::Resp, &page)).await;
        agent
    });
    let err = c.probe().await.unwrap_err();
    assert!(
        matches!(&err, HelperError::Closed(m) if m.contains("agent protocol violation")),
        "{err:?}"
    );
    let _agent = answer.await.unwrap();
    assert!(c.closed_reason().unwrap().contains("violation"));

    // 모르는 흐름
    let (ours, mut agent) = raw_pair();
    agent.hello(PROTO_VERSION).await;
    let (c, _) = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap();
    agent.send(Frame::end(77)).await;
    let reason = tokio::time::timeout(Duration::from_secs(5), c.closed())
        .await
        .unwrap();
    assert!(reason.contains("unknown stream 77"), "{reason}");

    // 끊김
    let (ours, mut agent) = raw_pair();
    agent.hello(PROTO_VERSION).await;
    let (c, _) = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap();
    drop(agent);
    let reason = tokio::time::timeout(Duration::from_secs(5), c.closed())
        .await
        .unwrap();
    assert_eq!(reason, "the helper closed the channel");
}

fn write_image(dir: &Path, arch: Arch, version: &str, proto: u32, tamper: bool) {
    let d = dir.join(arch_dir(arch));
    std::fs::create_dir_all(&d).unwrap();
    let kernel = if arch == Arch::X86_64 {
        "vmlinuz"
    } else {
        "Image"
    };
    std::fs::write(d.join(kernel), b"kernel").unwrap();
    std::fs::write(d.join("initrd.img"), b"initrd").unwrap();
    let k = sha256_file(&d.join(kernel)).unwrap();
    let i = sha256_file(&d.join("initrd.img")).unwrap();
    let manifest = serde_json::json!({
        "arch": arch_dir(arch),
        "files": { kernel: k, "initrd.img": if tamper { "00".repeat(32) } else { i } },
        "image_version": version,
        "initrd": "initrd.img",
        "kernel": kernel,
        "kernel_release": "6.12.117+deb13-arm64",
        "proto": proto,
    });
    std::fs::write(
        d.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// §8.4: manifest의 아키텍처·통신 규약·이미지 판(같은 orbivisord 판)·파일 해시를 본다.
#[test]
fn image_check() {
    let dir = tempfile::tempdir().unwrap();
    let arch = Arch::Aarch64;
    let err = check_image(dir.path(), arch, "0.1.0").unwrap_err();
    assert!(err.starts_with("no helper image in"), "{err}");

    write_image(
        dir.path(),
        arch,
        "orbivisor 0.1.0 lock-233004539824",
        PROTO_VERSION,
        false,
    );
    let img = check_image(dir.path(), arch, "0.1.0").unwrap();
    assert_eq!(img.image_version, "orbivisor 0.1.0 lock-233004539824");
    assert_eq!(img.kernel, dir.path().join("aarch64/Image"));
    assert_eq!(img.initrd, dir.path().join("aarch64/initrd.img"));
    assert_eq!(img.kernel_release, "6.12.117+deb13-arm64");

    let err = check_image(dir.path(), arch, "0.2.0").unwrap_err();
    assert!(
        err.contains("does not match this orbivisord (0.2.0)"),
        "{err}"
    );
    // `0.1.0`이 `0.1.0-1`의 앞이어도 같은 판이 아니다.
    write_image(
        dir.path(),
        arch,
        "orbivisor 0.1.0-1 lock-233004539824",
        PROTO_VERSION,
        false,
    );
    assert!(check_image(dir.path(), arch, "0.1.0").is_err());

    write_image(
        dir.path(),
        arch,
        "orbivisor 0.1.0 lock-233004539824",
        1,
        false,
    );
    // 판 1(몫 없는 옛 이미지)은 다시 빌드해야 한다.
    let err = check_image(dir.path(), arch, "0.1.0").unwrap_err();
    assert!(err.contains("protocol 1"), "{err}");

    write_image(
        dir.path(),
        arch,
        "orbivisor 0.1.0 lock-233004539824",
        PROTO_VERSION,
        true,
    );
    let err = check_image(dir.path(), arch, "0.1.0").unwrap_err();
    assert!(err.contains("initrd.img does not match its hash"), "{err}");

    // 다른 아키텍처 디렉터리에 둔 이미지
    write_image(
        dir.path(),
        Arch::X86_64,
        "orbivisor 0.1.0 lock-233004539824",
        PROTO_VERSION,
        false,
    );
    std::fs::copy(
        dir.path().join("x86_64/manifest.json"),
        dir.path().join("aarch64/manifest.json"),
    )
    .unwrap();
    let err = check_image(dir.path(), arch, "0.1.0").unwrap_err();
    assert!(err.contains("is for x86_64"), "{err}");
}

fn state(dir: &Path, max: u32) -> Arc<FileRestoreState> {
    let host = HostProfile::debian(Arch::Aarch64, orbivisor_qemu::Accel::Tcg);
    Arc::new(FileRestoreState::new(
        dir.to_path_buf(),
        &host,
        "0.1.0",
        HelperConfig::default(),
        SessionLimits {
            max_sessions: max,
            ..SessionLimits::default()
        },
    ))
}

/// 노드 알림: 이미지가 검사를 지나야 알리고(manifest가 바뀔 때만 다시 검사), 세션 자리는 상한까지, 버리면 돌아온다.
#[tokio::test]
async fn node_state_follows_the_image_and_the_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path(), 2);
    assert!(st.refresh_image().await);
    assert_eq!(st.node(), None);
    assert!(st.image_error().unwrap().starts_with("no helper image"));
    assert!(!st.refresh_image().await, "same stamp, no new check");

    write_image(
        dir.path(),
        Arch::Aarch64,
        "orbivisor 0.1.0 lock-233004539824",
        PROTO_VERSION,
        false,
    );
    assert!(st.refresh_image().await);
    let node = st.node().unwrap();
    assert_eq!(node.image_version, "orbivisor 0.1.0 lock-233004539824");
    assert_eq!((node.arch, node.accel.as_str()), (Arch::Aarch64, "tcg"));
    assert_eq!((node.sessions, node.max_sessions), (0, 2));
    assert!(!node.accelerated());

    let mut changes = st.subscribe();
    let a = st.try_open().unwrap();
    let b = st.try_open().unwrap();
    assert!(st.try_open().is_none());
    assert_eq!(st.node().unwrap().sessions, 2);
    assert!(changes.has_changed().unwrap());
    changes.mark_unchanged();
    drop(a);
    assert!(changes.has_changed().unwrap());
    assert_eq!(st.sessions(), 1);
    drop(b);
    assert_eq!(st.sessions(), 0);

    // 상한 0은 이 노드에서 끔
    let off = state(dir.path(), 0);
    off.refresh_image().await;
    assert!(off.image().is_some());
    assert_eq!(off.node(), None);
    assert!(off.try_open().is_none());
}

#[test]
fn entry_views_keep_raw_names_and_cursors_round_trip() {
    let attrs = proto::Attrs {
        kind: EntryKind::File,
        size: 3,
        mtime: -5,
        mtime_nsec: 0,
        mode: 0o640,
        uid: 1000,
        gid: 1000,
        nlink: 1,
    };
    let e = Entry {
        name: Name::new(vec![b'a', 0xff]).unwrap(),
        attrs,
        link_target: None,
    };
    let v = entry_view(&e);
    assert_eq!(v.name, "a\u{fffd}");
    assert_eq!(v.raw.as_deref(), Some("Yf8="));
    assert_eq!(v.part().unwrap().as_bytes(), &[b'a', 0xff]);
    assert_eq!((v.mtime, v.mode, v.uid), (-5, 0o640, 1000));
    let utf8 = Entry {
        name: Name::new("한글.txt").unwrap(),
        attrs,
        link_target: None,
    };
    assert_eq!(entry_view(&utf8).raw, None);
    let c = encode_cursor(&e.name);
    assert!(!c.contains(['/', '+', '=']), "{c}");
    assert_eq!(decode_cursor(&c), Some(e.name.clone()));
    assert_eq!(decode_cursor("!!"), None);
    assert_eq!(decode_cursor(""), None);
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// 소켓 경로가 짧은 임시 디렉터리(macOS `sun_path` 104바이트)
fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oplfr")
        .tempdir_in("/tmp")
        .unwrap()
}

/// Hello 전에 끝나는 QEMU: QEMU의 오류 줄을 까닭에 싣고 세션 디렉터리를 치운다.
#[tokio::test]
async fn a_helper_that_dies_before_hello_reports_why_and_cleans_up() {
    let dir = short_tempdir();
    let qemu = script(
        dir.path(),
        "qemu",
        r#"for a; do [ "$a" = -version ] && exit 1; done
echo "qemu-system: -blockdev driver=nbd: Failed to connect" >&2
exit 1"#,
    );
    let qemu_img = script(dir.path(), "qemu-img", r#"touch "$5""#);
    let host = HostProfile {
        qemu_binary: qemu,
        ..HostProfile::debian(Arch::native(), orbivisor_qemu::Accel::Tcg)
    };
    // 권한 내림은 정하지 않는다: root로 돌리는 시험(GitHub linux-kvm)에서 기본 `nobody`면 가짜 QEMU가 `-run-with`를 몰라
    // 띄우기 전에 거절된다.
    let cfg = HelperConfig {
        qemu_img,
        boot_timeout: Duration::from_secs(10),
        run_as: None,
        ..HelperConfig::default()
    };
    let (run, state) = (dir.path().join("run"), dir.path().join("state"));
    let l = launch();
    let err = start_helper(&host, false, &run, &state, &cfg, &l)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("the helper VM exited"), "{msg}");
    assert!(msg.contains("Failed to connect"), "{msg}");
    let paths = HelperPaths::new(&run, &state, l.id);
    assert!(!paths.run.exists() && !paths.state.exists());

    // 다른 아키텍처의 이미지·빈 체인·긴 소켓 경로는 띄우기 전에 거절
    let mut other = l.clone();
    other.image.arch = match Arch::native() {
        Arch::X86_64 => Arch::Aarch64,
        Arch::Aarch64 => Arch::X86_64,
    };
    assert!(matches!(
        start_helper(&host, false, &run, &state, &cfg, &other).await,
        Err(DriverError::InvalidSpec(_))
    ));
    let mut empty = l.clone();
    empty.disks[0].source = LiveRestoreSource::Qcow2 { chain: vec![] };
    assert!(matches!(
        start_helper(&host, false, &run, &state, &cfg, &empty).await,
        Err(DriverError::InvalidSpec(_))
    ));
    let long = dir.path().join("x".repeat(80));
    let err = start_helper(&host, false, &long, &state, &cfg, &l)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("longer than"), "{err}");
}

/// 리뷰 1: 덮개 지킴이는 `Hello` 전(부팅 중)에도 돈다 — 부팅 때 저널 재생이 덮개를 한도 넘게 채우면 세션이 곧 실패한다.
#[tokio::test]
async fn the_overlay_guard_runs_while_booting() {
    let dir = short_tempdir();
    let (run, state) = (dir.path().join("run"), dir.path().join("state"));
    let l = launch();
    let overlay = HelperPaths::new(&run, &state, l.id).overlay(0);
    let qemu = script(
        dir.path(),
        "qemu",
        &format!(
            r#"case " $* " in *" -kernel "*) ;; *) exit 1 ;; esac
dd if=/dev/zero of="{}" bs=1048576 count=4 2>/dev/null
sleep 30"#,
            overlay.display()
        ),
    );
    let qemu_img = script(dir.path(), "qemu-img", r#"touch "$5""#);
    let host = HostProfile {
        qemu_binary: qemu,
        ..HostProfile::debian(Arch::native(), orbivisor_qemu::Accel::Tcg)
    };
    let cfg = HelperConfig {
        qemu_img,
        boot_timeout: Duration::from_secs(25),
        overlay_max: 1 << 20,
        run_as: None,
        ..HelperConfig::default()
    };
    let started = Instant::now();
    let err = start_helper(&host, false, &run, &state, &cfg, &l)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("the disk overlays grew past 1 MiB"),
        "{err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    let paths = HelperPaths::new(&run, &state, l.id);
    assert!(!paths.run.exists() && !paths.state.exists());
}

/// 다시 뜬 opld: 앞 프로세스가 남긴 도우미(명령줄에 `opl-fr-<id>`)를 끝내고 두 디렉터리를 지운다. 남의 프로세스는 두지 않는다.
#[tokio::test]
async fn sweep_stops_leftover_helpers_and_removes_their_files() {
    if std::process::Command::new("perl")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("skipping: no perl");
        return;
    }
    let dir = short_tempdir();
    let (run, state) = (dir.path().join("run"), dir.path().join("state"));
    let id = Uuid::now_v7();
    let paths = HelperPaths::new(&run, &state, id);
    paths.prepare().unwrap();
    std::fs::write(paths.overlay(0), b"x").unwrap();
    let mut helper = std::process::Command::new("perl")
        .args(["-e", "sleep 60", &format!("{UNIT_PREFIX}{id}")])
        .spawn()
        .unwrap();
    std::fs::write(paths.pid_file(), helper.id().to_string()).unwrap();
    // pid 파일이 남의 프로세스를 가리킴(명령줄이 다름)
    let other_id = Uuid::now_v7();
    let other_paths = HelperPaths::new(&run, &state, other_id);
    other_paths.prepare().unwrap();
    let mut other = std::process::Command::new("perl")
        .args(["-e", "sleep 60"])
        .spawn()
        .unwrap();
    std::fs::write(other_paths.pid_file(), other.id().to_string()).unwrap();

    // 같은 호스트의 다른 opld(다른 run_dir·state_dir)의 도우미
    let (run_b, state_b) = (dir.path().join("run-b"), dir.path().join("state-b"));
    let id_b = Uuid::now_v7();
    let paths_b = HelperPaths::new(&run_b, &state_b, id_b);
    paths_b.prepare().unwrap();
    let mut helper_b = std::process::Command::new("perl")
        .args(["-e", "sleep 60", &format!("{UNIT_PREFIX}{id_b}")])
        .spawn()
        .unwrap();
    std::fs::write(paths_b.pid_file(), helper_b.id().to_string()).unwrap();

    assert_eq!(sweep_leftovers(&run, &state, false).await, 2);
    // 리뷰 1(GitHub file-restore 실패): 다른 opld의 도우미·디렉터리·scope는 건드리지 않는다.
    assert!(
        helper_b.try_wait().unwrap().is_none(),
        "another opld's helper"
    );
    assert!(paths_b.run.exists() && paths_b.state.exists());
    let ids = [id, other_id].into_iter().collect();
    let scopes = session_scopes(&ids);
    assert_eq!(scopes.len(), 2);
    assert!(scopes.contains(&format!("opl-fr-{id}.scope")));
    assert!(!scopes.iter().any(|u| u.contains(&id_b.to_string())));
    assert_eq!(sweep_leftovers(&run_b, &state_b, false).await, 1);
    assert!(!helper_b.wait().unwrap().success());
    let status = helper.wait().unwrap();
    assert!(!status.success());
    assert!(other.try_wait().unwrap().is_none(), "not ours to kill");
    assert!(!run.join(SUBDIR).exists() && !state.join(SUBDIR).exists());
    other.kill().unwrap();
    other.wait().unwrap();
    assert_eq!(sweep_leftovers(&run, &state, false).await, 0);
}

/// 리뷰 1: 몫(창)을 넘겨 보내는 에이전트는 규약 위반이다(opld가 흐름마다 쌓는 것이 창을 넘지 않는다).
#[tokio::test]
async fn an_agent_that_ignores_the_window_is_closed() {
    let (ours, mut agent) = raw_pair();
    agent.hello(PROTO_VERSION).await;
    let (c, _) = HelperClient::connect(ours, Duration::from_secs(5), quick())
        .await
        .unwrap();
    let flood = tokio::spawn(async move {
        let (id, _) = agent.request().await.unwrap();
        let size = 64u64 << 20;
        agent
            .send(Frame::json(id, FrameKind::Resp, &Reply::FileHead { size }))
            .await;
        for _ in 0..8 {
            agent
                .send(Frame::new(id, FrameKind::Data, vec![0; 1 << 20]))
                .await;
        }
        agent
    });
    // 받는 쪽이 읽지 않아도 채널을 읽는 쪽이 창을 넘는 것을 알아챈다.
    let (_, _stream) = c.read("disk0p1", path_of("x")).await.unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(5), c.closed())
        .await
        .unwrap();
    assert!(reason.contains("more than its window"), "{reason}");
    // 아무도 읽지 않으니 가짜 에이전트의 남은 쓰기는 막혀 있다.
    flood.abort();
}
