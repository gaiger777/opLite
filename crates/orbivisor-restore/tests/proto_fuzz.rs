//! 통신 규약 해석기 성질 시험(설계 §14): 무작위 바이트·잘린 틀·거대 길이에 패닉 없이 한도 오류, 틀 묶음을 아무렇게나
//! 잘라 밀어 넣어도 같은 틀, 흐름 검사기는 무작위 틀에 패닉하지 않음. orbivisor-types와 같은 한도·볼륨 id 규칙.

use orbivisor_restore::proto::{
    self, AgentError, Attrs, EntryKind, ErrorCode, Frame, FrameDecoder, FrameError, FrameKind,
    Item, MAX_FRAME, Name, Reply, Request, StreamCheck, read_frame,
};
use proptest::prelude::*;

fn kind() -> impl Strategy<Value = FrameKind> {
    prop_oneof![
        Just(FrameKind::Req),
        Just(FrameKind::Resp),
        Just(FrameKind::Data),
        Just(FrameKind::End),
        Just(FrameKind::Err),
    ]
}

fn frame() -> impl Strategy<Value = Frame> {
    (
        any::<u32>(),
        kind(),
        prop::collection::vec(any::<u8>(), 0..2000),
    )
        .prop_map(|(s, k, p)| {
            let p = if k == FrameKind::End { Vec::new() } else { p };
            Frame::new(s, k, p)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn decoder_survives_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..4096), cut in 1usize..64) {
        let mut d = FrameDecoder::new();
        for c in bytes.chunks(cut) {
            d.push(c);
            while let Ok(Some(f)) = d.next_frame() {
                prop_assert!(f.payload.len() + 9 <= MAX_FRAME);
            }
        }
        let _ = d.finish();
        let mut r: &[u8] = &bytes;
        while let Ok(Some(_)) = read_frame(&mut r) {}
    }

    #[test]
    fn frames_round_trip_in_any_chunking(frames in prop::collection::vec(frame(), 0..20), cuts in prop::collection::vec(1usize..300, 1..50)) {
        let wire: Vec<u8> = frames.iter().flat_map(Frame::encode).collect();
        let mut d = FrameDecoder::new();
        let mut got = Vec::new();
        let mut pos = 0;
        let mut i = 0;
        while pos < wire.len() {
            let n = cuts[i % cuts.len()].min(wire.len() - pos);
            d.push(&wire[pos..pos + n]);
            pos += n;
            i += 1;
            while let Some(f) = d.next_frame().unwrap() {
                got.push(f);
            }
        }
        d.finish().unwrap();
        prop_assert_eq!(&got, &frames);
        let mut r: &[u8] = &wire;
        let mut got2 = Vec::new();
        while let Some(f) = read_frame(&mut r).unwrap() {
            got2.push(f);
        }
        prop_assert_eq!(got2, frames);
    }

    #[test]
    fn truncated_frames_wait_or_fail(f in frame(), cut in 0usize..2000) {
        let wire = f.encode();
        let cut = cut.min(wire.len().saturating_sub(1));
        let mut d = FrameDecoder::new();
        d.push(&wire[..cut]);
        prop_assert_eq!(d.next_frame().unwrap(), None);
        prop_assert_eq!(d.finish().is_err(), cut > 0);
        let mut r: &[u8] = &wire[..cut];
        match read_frame(&mut r) {
            Ok(None) => prop_assert_eq!(cut, 0),
            Err(proto::ReadError::Frame(FrameError::Truncated)) => prop_assert!(cut > 0),
            other => prop_assert!(false, "{other:?}"),
        }
    }

    #[test]
    fn huge_lengths_are_refused(len in (MAX_FRAME as u32 + 1)..=u32::MAX, stream in any::<u32>()) {
        let mut h = len.to_be_bytes().to_vec();
        h.extend_from_slice(&stream.to_be_bytes());
        h.push(3);
        let mut d = FrameDecoder::new();
        d.push(&h);
        prop_assert_eq!(d.next_frame(), Err(FrameError::Long(len)));
        let mut r: &[u8] = &h;
        prop_assert!(matches!(read_frame(&mut r), Err(proto::ReadError::Frame(FrameError::Long(_)))));
    }

    #[test]
    fn json_parsers_survive_garbage(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let f = Frame::new(1, FrameKind::Resp, bytes);
        let _ = f.parse::<Reply>();
        let _ = f.parse::<Request>();
        let _ = f.parse::<AgentError>();
    }

    #[test]
    fn stream_check_survives_random_frames(frames in prop::collection::vec(frame(), 0..30), op in 0usize..6) {
        let req = match op {
            0 => Request::Probe,
            1 => Request::Mount { volume: "disk0".into() },
            2 => Request::List { volume: "disk0".into(), path: vec![], cursor: None, limit: 10 },
            3 => Request::Stat { volume: "disk0".into(), path: vec![] },
            4 => Request::Read { volume: "disk0".into(), path: vec![] },
            _ => Request::Walk { volume: "disk0".into(), paths: vec![vec![]], hardlinks: true },
        };
        let mut c = StreamCheck::new(&req, 1000);
        for f in frames {
            if c.feed(f).is_err() {
                break;
            }
        }
    }

    #[test]
    fn walk_streams_accept_well_formed_sequences(sizes in prop::collection::vec(0u64..5000, 0..20), chunk in 1usize..2000) {
        let req = Request::Walk { volume: "disk0".into(), paths: vec![], hardlinks: false };
        let mut c = StreamCheck::new(&req, 1000);
        let mut total = 0;
        for (i, size) in sizes.iter().enumerate() {
            let item = Reply::Item(Item {
                path: vec![Name::new(format!("f{i}")).unwrap()],
                attrs: Attrs { kind: EntryKind::File, size: *size, mtime: 0, mtime_nsec: 0, mode: 0o644, uid: 0, gid: 0, nlink: 1 },
                link_target: None,
                hardlink: None,
            });
            c.feed(Frame::json(5, FrameKind::Resp, &item)).unwrap();
            let mut left = *size as usize;
            while left > 0 {
                let n = left.min(chunk);
                c.feed(Frame::new(5, FrameKind::Data, vec![0; n])).unwrap();
                left -= n;
                total += n;
            }
        }
        prop_assert_eq!(total as u64, sizes.iter().sum::<u64>());
        prop_assert_eq!(c.entries(), sizes.len() as u64);
        c.feed(Frame::end(5)).unwrap();
        prop_assert!(c.done());
    }

    /// orbivisor-types의 볼륨 id 규칙과 같다(opld가 에이전트 id를 API id로 그대로 쓴다).
    #[test]
    fn volume_id_rules_match_orbivisor_types(s in "(disk|lvm/|x)[0-9a-zA-Z+_./-]{0,12}") {
        prop_assert_eq!(proto::valid_volume_id(&s), orbivisor_types::file_restore::valid_volume_id(&s));
    }

    #[test]
    fn lv_ids_match_orbivisor_types(vg in "[ -~]{0,8}", lv in "[ -~]{0,8}", u1 in "[a-z0-9-]{0,6}", u2 in "[a-z0-9-]{0,6}") {
        prop_assert_eq!(
            proto::lv_volume_id(&vg, &lv, &u1, &u2),
            orbivisor_types::file_restore::lv_volume_id(&vg, &lv, &u1, &u2)
        );
    }
}

#[test]
fn limits_match_orbivisor_types() {
    use orbivisor_types::file_restore as t;
    assert_eq!(proto::MAX_NAME_BYTES, t::MAX_NAME_BYTES);
    assert_eq!(proto::MAX_PATH_DEPTH, t::MAX_PATH_DEPTH);
    assert_eq!(proto::MAX_LINK_TARGET, t::MAX_LINK_TARGET);
    assert_eq!(proto::MAX_VOLUMES, t::MAX_VOLUMES);
    assert_eq!(proto::MAX_PAGE_ENTRIES, t::MAX_PAGE_ENTRIES);
    // 이름의 JSON 꼴이 PathPart와 같다(opld가 그대로 넘긴다).
    for raw in [
        b"etc".to_vec(),
        b"caf\xe9".to_vec(),
        "한글".as_bytes().to_vec(),
    ] {
        let ours = serde_json::to_string(&Name::new(raw.clone()).unwrap()).unwrap();
        let theirs = serde_json::to_string(&t::PathPart::new(raw).unwrap()).unwrap();
        assert_eq!(ours, theirs);
    }
    // 오류 꼴
    let e = serde_json::to_string(&AgentError::new(ErrorCode::NotFound, "x")).unwrap();
    assert_eq!(e, r#"{"code":"not_found","message":"x"}"#);
}
