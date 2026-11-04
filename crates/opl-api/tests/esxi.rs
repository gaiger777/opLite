//! ESXi HTTPS 클라이언트: 지문 고정 TLS, probe, 기본 인증, `/folder` 목록 해석, 작은 파일 읽기,
//! 내려받기. 가짜 ESXi(`common/esxi.rs`)를 상대로 한다.

mod common;

use std::collections::BTreeMap;

use common::esxi::MockEsxi;
use opl_api::vm_imports::esxi::{
    Entry, EsxiError, download, get_small, list, parse_listing, probe,
};
use tempfile::TempDir;

const USER: &str = "root";
const PASSWORD: &str = "VMware1!";
const VMX: &str = "displayName = \"web01\"\nmemSize = \"1024\"\n";

fn flat() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

async fn esxi() -> MockEsxi {
    let files = BTreeMap::from([
        (
            "datastore1/web01/web01.vmx".to_string(),
            VMX.as_bytes().to_vec(),
        ),
        (
            "datastore1/web01/web01.vmdk".to_string(),
            b"# Disk DescriptorFile\n".to_vec(),
        ),
        ("datastore1/web01/web01-flat.vmdk".to_string(), flat()),
        ("datastore1/web01/web01.vmx.lck".to_string(), Vec::new()),
        (
            "datastore1/web01/logs/vmware.log".to_string(),
            b"log".to_vec(),
        ),
        ("datastore1/iso/ubuntu.iso".to_string(), b"iso".to_vec()),
        (
            "datastore 2/my vm & co/my vm & co.vmx".to_string(),
            VMX.as_bytes().to_vec(),
        ),
    ]);
    MockEsxi::start(files, USER, PASSWORD).await
}

fn entry(name: &str, is_dir: bool) -> Entry {
    Entry {
        name: name.into(),
        is_dir,
    }
}

fn sorted(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

#[tokio::test]
async fn probe_returns_the_server_fingerprint_without_sending_a_request() {
    let esxi = esxi().await;
    let fingerprint = probe("127.0.0.1", esxi.addr.port()).await.unwrap();
    assert_eq!(fingerprint, esxi.fingerprint);
    assert!(esxi.requests().is_empty());
}

#[tokio::test]
async fn lists_datastores_and_directories_with_the_pinned_fingerprint() {
    let esxi = esxi().await;
    // 지문은 소문자·콜론 없이 적어도 된다.
    let loose = esxi.fingerprint.replace(':', "").to_lowercase();
    let conn = esxi.connection(PASSWORD, Some(&loose));

    let datastores = list(&conn, None, "").await.unwrap();
    assert_eq!(
        sorted(datastores),
        vec![entry("datastore 2", true), entry("datastore1", true)]
    );
    let top = list(&conn, Some("datastore1"), "").await.unwrap();
    assert_eq!(sorted(top), vec![entry("iso", true), entry("web01", true)]);
    let vm = list(&conn, Some("datastore1"), "web01").await.unwrap();
    assert_eq!(
        sorted(vm),
        vec![
            entry("logs", true),
            entry("web01-flat.vmdk", false),
            entry("web01.vmdk", false),
            entry("web01.vmx", false),
            entry("web01.vmx.lck", false),
        ]
    );
    let odd = list(&conn, Some("datastore 2"), "my vm & co")
        .await
        .unwrap();
    assert_eq!(odd, vec![entry("my vm & co.vmx", false)]);
    // 파일을 디렉터리로 잘못 보고 읽으면(HTML이 아님) 빈 목록이다.
    let file = list(&conn, Some("datastore1"), "iso/ubuntu.iso")
        .await
        .unwrap();
    assert!(file.is_empty());

    let requests = esxi.requests();
    assert!(
        requests.iter().all(|r| r.method == "GET"
            && r.authorization.as_deref() == Some("Basic cm9vdDpWTXdhcmUxIQ==")),
        "{requests:?}"
    );
    let odd = requests.iter().find(|r| r.path.contains("my vm")).unwrap();
    assert_eq!(odd.path, "/folder/my vm & co");
    assert_eq!(odd.query, "dcPath=ha-datacenter&dsName=datastore%202");
}

#[tokio::test]
async fn reads_small_files_and_downloads_large_ones() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let text = get_small(&conn, "datastore1", "web01/web01.vmx", 64 << 10)
        .await
        .unwrap();
    assert_eq!(text, VMX);
    let text = get_small(&conn, "datastore 2", "my vm & co/my vm & co.vmx", 64 << 10)
        .await
        .unwrap();
    assert_eq!(text, VMX);

    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("disk.raw");
    let data = flat();
    download(
        &conn,
        "datastore1",
        "web01/web01-flat.vmdk",
        &dest,
        data.len() as u64,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[tokio::test]
async fn small_reads_are_limited() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let err = get_small(&conn, "datastore1", "web01/web01-flat.vmdk", 64 << 10)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, EsxiError::TooLarge { path, limit: 65536 } if path == "[datastore1] web01/web01-flat.vmdk"),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "[datastore1] web01/web01-flat.vmdk is larger than 65536 bytes"
    );
}

#[tokio::test]
async fn a_download_of_the_wrong_length_leaves_no_file() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("disk.raw");
    let len = flat().len() as u64;

    // 서버가 덜 보냄
    let err = download(
        &conn,
        "datastore1",
        "web01/web01-flat.vmdk",
        &dest,
        len + 512,
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "downloaded {len} bytes of [datastore1] web01/web01-flat.vmdk, expected {}",
            len + 512
        )
    );
    assert!(!dest.exists());

    // 서버가 더 보냄: 기대한 길이를 넘자마자 멈춘다.
    let err = download(&conn, "datastore1", "web01/web01-flat.vmdk", &dest, 1000)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .ends_with("of [datastore1] web01/web01-flat.vmdk, expected 1000"),
        "{err}"
    );
    assert!(!dest.exists());

    // 없는 파일도 남기지 않는다.
    let err = download(&conn, "datastore1", "web01/none-flat.vmdk", &dest, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::NotFound(_)), "{err}");
    assert!(!dest.exists());
}

#[tokio::test]
async fn a_wrong_fingerprint_fails_before_any_credentials_are_sent() {
    let esxi = esxi().await;
    let wrong = ["AB"; 32].join(":");
    let conn = esxi.connection(PASSWORD, Some(&wrong));
    match list(&conn, None, "").await {
        Err(EsxiError::Fingerprint { expected, actual }) => {
            assert_eq!(expected, wrong);
            assert_eq!(actual, esxi.fingerprint);
        }
        other => panic!("{other:?}"),
    }
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("disk.raw");
    let err = download(&conn, "datastore1", "web01/web01-flat.vmdk", &dest, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::Fingerprint { .. }), "{err}");
    assert!(!dest.exists());
    // 모양이 틀린 지문도 맞지 않는다.
    let conn = esxi.connection(PASSWORD, Some("not-a-fingerprint"));
    let err = get_small(&conn, "datastore1", "web01/web01.vmx", 1024)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::Fingerprint { .. }), "{err}");
    // 서버는 요청을 하나도 받지 않았다(Authorization 헤더도).
    assert!(esxi.requests().is_empty(), "{:?}", esxi.requests());
}

#[tokio::test]
async fn without_a_fingerprint_the_server_fingerprint_is_returned_untrusted() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, None);
    match get_small(&conn, "datastore1", "web01/web01.vmx", 1024).await {
        Err(EsxiError::Untrusted(actual)) => assert_eq!(actual, esxi.fingerprint),
        other => panic!("{other:?}"),
    }
    assert!(esxi.requests().is_empty());
}

#[tokio::test]
async fn a_wrong_password_is_an_auth_error_and_missing_files_are_not_found() {
    let esxi = esxi().await;
    let conn = esxi.connection("wrong", Some(&esxi.fingerprint));
    let err = list(&conn, None, "").await.unwrap_err();
    assert!(matches!(err, EsxiError::Auth), "{err}");
    assert_eq!(err.to_string(), "ESXi rejected the user name or password");

    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let err = get_small(&conn, "datastore1", "web01/missing.vmx", 1024)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::NotFound(_)), "{err}");
    assert_eq!(
        err.to_string(),
        "[datastore1] web01/missing.vmx was not found on the ESXi host"
    );
    let err = list(&conn, Some("datastore9"), "").await.unwrap_err();
    assert!(matches!(err, EsxiError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn an_unreachable_host_is_a_connect_error() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let err = probe("127.0.0.1", port).await.unwrap_err();
    assert!(matches!(err, EsxiError::Connect(_)), "{err}");
}

/// ESXi 7의 데이터스토어 브라우저 HTML(디렉터리 `web01`)
const ESXI_DIR: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en" lang="en">
<head><meta http-equiv="Content-Type" content="text/html; charset=utf-8" />
<title>Index of [datastore1] web01</title></head>
<body>
<h1>Index of [datastore1] web01</h1>
<table>
<tr><th>Name</th><th>Last modified</th><th>Size</th></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter&amp;dsName=datastore1">Parent Directory</a></td></tr>
<tr><td><a href="/folder/web01/web01%2evmx?dcPath=ha%2ddatacenter&amp;dsName=datastore1">web01.vmx</a></td><td align="right">04-Nov-2026 10:00</td><td align="right">  3 KB</td></tr>
<tr><td><A HREF='/folder/web01/web01-flat.vmdk?dcPath=ha%2ddatacenter&amp;dsName=datastore1'>web01-flat.vmdk</A></td></tr>
<tr><td><a class="file" href="/folder/web01/my%20disk%20%26%20more.vmdk?dcPath=ha%2ddatacenter&amp;dsName=datastore1">my disk &amp; more.vmdk</a></td></tr>
<tr><td><a href="/folder/web01/sub.dir/?dcPath=ha%2ddatacenter&amp;dsName=datastore1">sub.dir</a></td></tr>
<tr><td><a href="/folder/web01/web01.vmx?dcPath=ha%2ddatacenter&amp;dsName=datastore2">other datastore</a></td></tr>
<tr><td><a href="/folder/web01/deeper/x.vmx?dcPath=ha%2ddatacenter&amp;dsName=datastore1">too deep</a></td></tr>
<tr><td><a href="/mob/?moid=ha%2ddatacenter">Managed Object Browser</a></td></tr>
<tr><td><abbr title="x">abbr</abbr><a name="anchor">no href</a></td></tr>
</table>
</body>
</html>
"#;

/// ESXi 데이터센터 화면(데이터스토어 목록)
const ESXI_DATACENTER: &str = r#"<html><body>
<table>
<tr><th>Datastore Name</th></tr>
<tr><td><a href="/">Parent</a></td></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter&amp;dsName=datastore1">datastore1</a></td></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter&amp;dsName=NVMe%20SSD%20%282%29">NVMe SSD (2)</a></td></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter">Parent Datacenter</a></td></tr>
</table>
</body></html>
"#;

#[test]
fn parses_esxi_directory_listings() {
    let entries = parse_listing(ESXI_DIR, Some("datastore1"), "web01");
    assert_eq!(
        entries,
        vec![
            entry("web01.vmx", false),
            entry("web01-flat.vmdk", false),
            entry("my disk & more.vmdk", false),
            // 이름의 `.`만으로는 파일이 아니다(아는 확장자만).
            entry("sub.dir", true),
        ]
    );
    // 끝의 `/`는 같은 디렉터리다.
    assert_eq!(
        parse_listing(ESXI_DIR, Some("datastore1"), "/web01/"),
        entries
    );
    // 다른 디렉터리의 목록으로 읽으면 아무것도 바로 아래가 아니다.
    assert!(parse_listing(ESXI_DIR, Some("datastore1"), "").is_empty());
}

#[test]
fn parses_esxi_datastore_listings() {
    assert_eq!(
        parse_listing(ESXI_DATACENTER, None, ""),
        vec![entry("datastore1", true), entry("NVMe SSD (2)", true)]
    );
}

/// 쿼리 값의 `+`는 공백이다(`%2B`가 `+`).
#[test]
fn query_plus_is_a_space() {
    let html = r#"<a href="/folder?dcPath=ha-datacenter&dsName=SSD+1">SSD 1</a>
<a href="/folder?dcPath=ha-datacenter&dsName=a%2Bb">a+b</a>"#;
    assert_eq!(
        parse_listing(html, None, ""),
        vec![entry("SSD 1", true), entry("a+b", true)]
    );
    let html = r#"<a href="/folder/vm/x.vmx?dcPath=ha-datacenter&dsName=SSD+1">x.vmx</a>"#;
    assert_eq!(
        parse_listing(html, Some("SSD 1"), "vm"),
        vec![entry("x.vmx", false)]
    );
}

/// 목록 크기 제한(1 MiB)만큼의 나쁜 입력도 빨리 끝난다: `;` 없는 `&`가 많은 href, 서로 다른 항목이 많은
/// 목록.
#[test]
fn large_listings_are_parsed_quickly() {
    let started = std::time::Instant::now();
    let html = format!(
        "<a href=\"/folder/vm/{}x.vmx?dsName=ds\">x</a>",
        "&".repeat(1 << 20)
    );
    let entries = parse_listing(&html, Some("ds"), "vm");
    assert_eq!(entries.len(), 1);
    let html: String = (0..30_000)
        .map(|i| format!("<a href=\"/folder/vm/f{i}.vmdk?dsName=ds\">f</a>\n"))
        .collect();
    assert_eq!(parse_listing(&html, Some("ds"), "vm").len(), 30_000);
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
}
