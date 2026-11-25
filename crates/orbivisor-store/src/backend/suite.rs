//! 백엔드 공통 시험 묶음: 메모리·로컬·S3(가짜 S3)가 같은 시험을 지난다. S3만의 다시 하기·실패 주입 시험도 여기.

use std::time::Duration;

use super::local::LocalBackend;
use super::mem::MemBackend;
use super::s3::S3Backend;
use super::{Backend, StoreError, list_all};
use crate::fake_s3::{Conditional, FakeS3};

/// 쪽 크기를 3으로 둔 백엔드에서 돈다.
async fn exercise(b: &dyn Backend) {
    // 쓰기·읽기·있음
    assert_eq!(b.head("a/x").await.unwrap(), None);
    assert!(matches!(b.get("a/x").await, Err(StoreError::NotFound(_))));
    b.put("a/x", b"one".to_vec()).await.unwrap();
    assert_eq!(b.get("a/x").await.unwrap(), b"one");
    let m = b.head("a/x").await.unwrap().unwrap();
    assert_eq!(m.size, 3);
    assert!(m.modified > 1_600_000_000, "{m:?}");
    b.put("a/x", b"two!".to_vec()).await.unwrap();
    assert_eq!(b.get("a/x").await.unwrap(), b"two!");
    // 빈 객체
    b.put("a/empty", Vec::new()).await.unwrap();
    assert_eq!(b.get("a/empty").await.unwrap(), b"");
    assert_eq!(b.head("a/empty").await.unwrap().unwrap().size, 0);

    // 없을 때만 쓰기
    assert!(b.put_if_absent("owner.json", b"me".to_vec()).await.unwrap());
    assert!(
        !b.put_if_absent("owner.json", b"you".to_vec())
            .await
            .unwrap()
    );
    assert_eq!(b.get("owner.json").await.unwrap(), b"me");

    // 지우기(없어도 성공)
    b.delete("a/empty").await.unwrap();
    b.delete("a/empty").await.unwrap();
    assert_eq!(b.head("a/empty").await.unwrap(), None);

    // 나열: 이름 차례, 쪽 넘김, 접두사 밖은 빠짐
    let mut want = Vec::new();
    for i in 0..10u32 {
        let k = format!("chunks/{:04x}/{:08x}", i % 3, i);
        b.put(&k, vec![i as u8; i as usize]).await.unwrap();
        want.push(k);
    }
    b.put("chunksx", b"no".to_vec()).await.unwrap();
    b.put("snapshots/v/b/index.json", b"{}".to_vec())
        .await
        .unwrap();
    b.put("snapshots/v/b/disk0.fidx", b"ix".to_vec())
        .await
        .unwrap();
    want.sort();
    let page = b.list("chunks/", None).await.unwrap();
    assert_eq!(page.objects.len(), 3);
    assert!(page.next.is_some());
    let all = list_all(b, "chunks/").await.unwrap();
    let keys: Vec<_> = all.iter().map(|o| o.key.clone()).collect();
    assert_eq!(keys, want);
    assert_eq!(
        all.iter()
            .find(|o| o.key.ends_with("00000009"))
            .unwrap()
            .size,
        9
    );
    let snaps = list_all(b, "snapshots/").await.unwrap();
    assert_eq!(
        snaps.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
        vec!["snapshots/v/b/disk0.fidx", "snapshots/v/b/index.json"]
    );
    let one = list_all(b, "chunks/0001/").await.unwrap();
    assert_eq!(one.len(), 3);
    assert!(list_all(b, "nothing/").await.unwrap().is_empty());

    // 옮기기
    b.rename("chunks/0000/00000000", "bad/00000000.5")
        .await
        .unwrap();
    assert_eq!(b.head("chunks/0000/00000000").await.unwrap(), None);
    assert_eq!(b.get("bad/00000000.5").await.unwrap(), b"");
    assert!(matches!(
        b.rename("chunks/0000/00000000", "bad/x").await,
        Err(StoreError::NotFound(_))
    ));

    // 만지기
    b.touch("a/x").await.unwrap();
    assert_eq!(b.get("a/x").await.unwrap(), b"two!");
    assert!(matches!(
        b.touch("a/none").await,
        Err(StoreError::NotFound(_))
    ));

    // 이름 검사
    assert!(matches!(
        b.put("../x", vec![]).await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(b.get("/x").await, Err(StoreError::Invalid(_))));

    // 스냅샷 지우기
    b.delete("snapshots/v/b/index.json").await.unwrap();
    b.delete("snapshots/v/b/disk0.fidx").await.unwrap();
    assert!(list_all(b, "snapshots/").await.unwrap().is_empty());
}

#[tokio::test]
async fn mem_backend() {
    let b = MemBackend::new();
    b.set_page_size(3);
    exercise(&b).await;
    // 시계
    b.set_now(1000);
    b.put("t", vec![]).await.unwrap();
    assert_eq!(b.head("t").await.unwrap().unwrap().modified, 1000);
    b.advance(5);
    b.touch("t").await.unwrap();
    assert_eq!(b.head("t").await.unwrap().unwrap().modified, 1005);
}

#[tokio::test]
async fn local_backend() {
    let dir = tempfile::tempdir().unwrap();
    let b = LocalBackend::new(dir.path().join("store")).with_page_size(3);
    exercise(&b).await;
    // 임시 파일이 남지 않는다
    let leftovers: Vec<_> = list_all(&b, "")
        .await
        .unwrap()
        .into_iter()
        .filter(|o| o.key.contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // 빈 스냅샷 디렉터리는 치워진다(snapshots/ 자체는 남음)
    assert!(dir.path().join("store/snapshots").exists());
    assert!(!dir.path().join("store/snapshots/v").exists());
    // 만지기는 mtime을 지금으로
    let p = dir.path().join("store/a/x");
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert_eq!(
        b.head("a/x").await.unwrap().unwrap().modified,
        1_000_000_000
    );
    b.touch("a/x").await.unwrap();
    assert!(b.head("a/x").await.unwrap().unwrap().modified > 1_600_000_000);
}

#[tokio::test]
async fn local_prepare_makes_chunk_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let b = LocalBackend::new(dir.path().join("store"));
    b.prepare().await.unwrap();
    assert!(dir.path().join("store/chunks/0000").is_dir());
    assert!(dir.path().join("store/chunks/ffff").is_dir());
    assert_eq!(
        std::fs::read_dir(dir.path().join("store/chunks"))
            .unwrap()
            .count(),
        65536
    );
    // 다시 해도 된다
    b.prepare().await.unwrap();
    assert!(list_all(&b, "chunks/").await.unwrap().is_empty());
}

async fn fake() -> (FakeS3, S3Backend) {
    let f = FakeS3::start("bk", "AKIDTEST", "secret/key+1").await;
    let b = S3Backend::new(&f.target(Some("orbi/")), 4)
        .unwrap()
        .with_retry_budget(Duration::from_secs(5));
    (f, b)
}

#[tokio::test]
async fn s3_backend() {
    let (f, b) = fake().await;
    f.set_page_size(3);
    exercise(&b).await;
    // 접두사 아래에 쓴다
    assert!(
        f.objects().keys().iter().all(|k| k.starts_with("orbi/")),
        "{:?}",
        f.objects().keys()
    );
    // 만지기는 같은 키 복사로 시각을 새로
    f.objects().set_now(2_000_000_000);
    b.touch("a/x").await.unwrap();
    assert_eq!(
        b.head("a/x").await.unwrap().unwrap().modified,
        2_000_000_000
    );
    // 접두사 없이도
    let b2 = S3Backend::new(&f.target(None), 2).unwrap();
    b2.put("top", b"t".to_vec()).await.unwrap();
    assert!(f.objects().keys().contains(&"top".to_string()));
    assert_eq!(list_all(&b2, "top").await.unwrap().len(), 1);
}

#[tokio::test]
async fn s3_retries_then_gives_up() {
    let (f, b) = fake().await;
    // 503 두 번 뒤 성공
    f.fail_next(2, 503);
    b.put("k", b"v".to_vec()).await.unwrap();
    assert_eq!(f.count("put"), 1);
    // 500도
    f.fail_next(1, 500);
    assert_eq!(b.get("k").await.unwrap(), b"v");
    // 응답 도중 끊김
    f.cut_next(1);
    assert_eq!(b.get("k").await.unwrap(), b"v");
    // 계속 실패하면 다시 하기 시간 뒤 Unavailable
    let b = b.with_retry_budget(Duration::from_millis(700));
    f.fail_next(1000, 503);
    let start = std::time::Instant::now();
    let e = b.get("k").await.unwrap_err();
    assert!(
        matches!(e, StoreError::Unavailable(ref m) if m.contains("SlowDown")),
        "{e}"
    );
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn s3_client_errors_fail_at_once() {
    let f = FakeS3::start("bk", "AKIDTEST", "right").await;
    let mut t = f.target(None);
    t.secret_access_key = "wrong".into();
    let b = S3Backend::new(&t, 2).unwrap();
    let e = b.put("k", vec![1]).await.unwrap_err();
    assert!(
        matches!(e, StoreError::Denied(ref m) if m.contains("SignatureDoesNotMatch")),
        "{e}"
    );
    let mut t = f.target(None);
    t.bucket = "other".into();
    let b = S3Backend::new(&t, 2).unwrap();
    let e = b.put("k", vec![1]).await.unwrap_err();
    assert!(
        matches!(e, StoreError::Rejected(ref m) if m.contains("NoSuchBucket")),
        "{e}"
    );
    // 오지 않는 끝점: 다시 하기 뒤 Unavailable
    let mut t = f.target(None);
    t.endpoint = "http://127.0.0.1:1".into();
    let b = S3Backend::new(&t, 2)
        .unwrap()
        .with_retry_budget(Duration::from_millis(300));
    assert!(matches!(b.head("k").await, Err(StoreError::Unavailable(_))));
}

#[tokio::test]
async fn s3_conditional_write_modes() {
    let (f, b) = fake().await;
    for mode in [
        Conditional::Supported,
        Conditional::Ignored,
        Conditional::NotImplemented,
    ] {
        f.set_conditional(mode);
        let k = format!("owner-{mode:?}");
        assert!(
            b.put_if_absent(&k, b"a".to_vec()).await.unwrap(),
            "{mode:?}"
        );
        assert!(
            !b.put_if_absent(&k, b"b".to_vec()).await.unwrap(),
            "{mode:?}"
        );
        assert_eq!(b.get(&k).await.unwrap(), b"a");
    }
}

#[tokio::test]
async fn s3_https_with_user_ca() {
    // 사용자 CA로 서명한 서버 인증서를 믿는다(이름 확인 포함). 서버는 rustls + 같은 가짜 S3를 앞에서 TLS로 감싼다.
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let srv_key = KeyPair::generate().unwrap();
    let issuer = Issuer::from_ca_cert_pem(
        &ca.pem(),
        KeyPair::from_pem(&ca_key.serialize_pem()).unwrap(),
    )
    .unwrap();
    let srv = CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&srv_key, &issuer)
        .unwrap();

    let f = FakeS3::start("bk", "AK", "SK").await;
    let backend_addr = f.endpoint().trim_start_matches("http://").to_string();
    let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![srv.der().clone()],
        rustls_pki_types::PrivateKeyDer::try_from(srv_key.serialize_der()).unwrap(),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let proxy = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let backend_addr = backend_addr.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let Ok(mut up) = tokio::net::TcpStream::connect(backend_addr).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut up).await;
            });
        }
    });

    let mut t = f.target(None);
    t.endpoint = format!("https://localhost:{port}");
    t.insecure_http = false;
    t.ca_pem = Some(ca.pem());
    let b = S3Backend::new(&t, 2).unwrap();
    b.put("tls", b"ok".to_vec()).await.unwrap();
    assert_eq!(b.get("tls").await.unwrap(), b"ok");

    // 다른 CA는 믿지 않는다
    let other_key = KeyPair::generate().unwrap();
    let mut op = CertificateParams::new(Vec::<String>::new()).unwrap();
    op.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    t.ca_pem = Some(op.self_signed(&other_key).unwrap().pem());
    let b = S3Backend::new(&t, 2)
        .unwrap()
        .with_retry_budget(Duration::from_millis(300));
    let e = b.get("tls").await.unwrap_err();
    assert!(
        matches!(e, StoreError::Unavailable(ref m) if m.contains("TLS")),
        "{e}"
    );
    proxy.abort();
}
