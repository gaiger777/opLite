use super::*;
use crate::ovn::fake_ovsdb::FakeOvsdb;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 실제 ovsdb-server(OVN 24.03.8, 두 서버 NB 클러스터)에 보낸 요청과 받은 답(`fixtures/ovsdb-rpc.jsonl`)
const RECORDED: &str = include_str!("../fixtures/ovsdb-rpc.jsonl");

fn recorded(name: &str) -> (Value, Value) {
    RECORDED
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["name"] == name)
        .map(|v| (v["request"].clone(), v["response"].clone()))
        .unwrap_or_else(|| panic!("no recorded exchange {name}"))
}

/// 받아 둔 답 하나를 내는 서버: 요청을 하나 읽고 `id`만 바꾼 답을 보낸다. 받은 요청을 돌려준다.
async fn replay(answers: Vec<Value>) -> (SocketAddr, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut got = Vec::new();
        let mut buf = Vec::new();
        for mut answer in answers {
            let req = loop {
                let mut chunk = [0u8; 4096];
                let n = tcp.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let mut msgs = take_messages(&mut buf).unwrap();
                if !msgs.is_empty() {
                    break msgs.remove(0);
                }
            };
            answer["id"] = req["id"].clone();
            got.push(req);
            // 둘로 나눠 보내 덜 받은 메시지를 이어 붙이는지도 본다.
            let bytes = answer.to_string().into_bytes();
            let (a, b) = bytes.split_at(bytes.len() / 2);
            tcp.write_all(a).await.unwrap();
            tcp.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
            tcp.write_all(b).await.unwrap();
        }
        got
    });
    (addr, task)
}

async fn client(addr: SocketAddr) -> OvsdbClient {
    OvsdbClient::connect(&Transport::Tcp, &Endpoint::ip(addr), DEFAULT_TIMEOUT)
        .await
        .unwrap()
}

#[tokio::test]
async fn reads_recorded_server_database_rows() {
    let (_, leader) = recorded("server-leader");
    let (_, follower) = recorded("server-follower");
    let (addr, task) = replay(vec![leader, follower]).await;
    let c = client(addr).await;
    let s = c.server_db(NB_DB).await.unwrap().unwrap();
    assert_eq!(
        s,
        ServerDb {
            model: "clustered".into(),
            connected: true,
            leader: true,
            cid: Some("8f7aaf94-cbd2-4263-ae9b-b08fa2d1f8b4".into()),
        }
    );
    assert!(s.is_leader());
    let s = c.server_db(NB_DB).await.unwrap().unwrap();
    assert!(!s.leader && s.connected && !s.is_leader());
    // 보낸 요청은 받아 둔 요청과 같은 모양이다.
    let sent = task.await.unwrap();
    let (want, _) = recorded("server-leader");
    assert_eq!(sent[0]["method"], want["method"]);
    assert_eq!(sent[0]["params"][0], "_Server");
    assert_eq!(sent[0]["params"][1]["where"], want["params"][1]["where"]);
}

#[tokio::test]
async fn parses_recorded_rows_and_single_element_sets() {
    let (_, switches) = recorded("select-switches");
    let (_, ports) = recorded("select-ports");
    let (_, global) = recorded("nb-global-select");
    let (addr, _task) = replay(vec![switches, ports, global]).await;
    let c = client(addr).await;
    let rows = c
        .select(NB_DB, "Logical_Switch", vec![], &["_uuid", "ports"])
        .await
        .unwrap();
    // 포트가 하나인 `ports`는 집합이 아니라 참조 하나로 온다.
    assert_eq!(
        parse_uuids(&rows[0]["ports"]),
        vec!["5cc0c1ec-0adb-4a5a-b2d7-7ee0c4000dd2"]
    );
    assert_eq!(
        parse_map(&rows[0]["external_ids"]),
        BTreeMap::from([
            ("orbivisor:cluster".to_string(), "t1".to_string()),
            ("orbivisor:id".to_string(), "n1".to_string())
        ])
    );
    let rows = c
        .select(NB_DB, "Logical_Switch_Port", vec![], &[])
        .await
        .unwrap();
    assert_eq!(
        parse_strings(&rows[0]["addresses"]),
        vec!["02:00:00:00:00:01 10.0.0.5"]
    );
    assert!(parse_strings(&rows[0]["port_security"]).is_empty());
    assert_eq!(parse_bool(&rows[0]["enabled"]), None);
    assert!(parse_uuids(&rows[0]["dhcpv4_options"]).is_empty());
    let rows = c.select(NB_DB, "NB_Global", vec![], &[]).await.unwrap();
    assert_eq!(parse_int(&rows[0]["nb_cfg"]), Some(1));
    assert_eq!(
        parse_map(&rows[0]["external_ids"])["orbivisor:t1:fence"],
        "3:17"
    );
}

#[tokio::test]
async fn reports_recorded_operation_and_commit_errors() {
    let names = [
        "wait-fails",
        "delete-referenced",
        "unknown-table",
        "echo",
        "list-dbs",
    ];
    let answers = names.iter().map(|n| recorded(n).1).collect();
    let (addr, _task) = replay(answers).await;
    let c = client(addr).await;
    // 펜싱 `wait`이 맞지 않으면 첫 연산의 "timed out"이다.
    let e = c
        .transact(NB_DB, vec![json!({}), json!({})])
        .await
        .unwrap_err();
    assert!(e.is_wait_failed(), "{e}");
    assert!(matches!(e, OvsdbError::Op { index: 0, .. }));
    // 커밋 실패는 연산 수 뒤의 한 칸이다.
    let e = c.transact(NB_DB, vec![json!({})]).await.unwrap_err();
    match e {
        OvsdbError::Op {
            index,
            error,
            details,
        } => {
            assert_eq!(index, 1);
            assert_eq!(error, "referential integrity violation");
            assert!(details.unwrap().contains("remaining reference"));
        }
        e => panic!("{e}"),
    }
    let e = c.transact(NB_DB, vec![json!({})]).await.unwrap_err();
    assert!(e.to_string().contains("No table named Nope"), "{e}");
    c.echo().await.unwrap();
    assert_eq!(c.list_dbs().await.unwrap(), vec![NB_DB, "_Server"]);
}

#[tokio::test]
async fn answers_the_server_inactivity_probe() {
    let fake = FakeOvsdb::start(NB_DB).await;
    fake.with(|s| s.probe = Some(Duration::from_millis(50)));
    let c = client(fake.addr()).await;
    c.echo().await.unwrap();
    // 탐침이 여러 번 지나도 연결이 살아 있다.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(fake.with(|s| s.echo_replies) >= 2);
    assert!(!c.is_closed());
    c.echo().await.unwrap();
}

#[tokio::test]
async fn a_closed_connection_fails_waiting_and_new_calls() {
    let fake = FakeOvsdb::start(NB_DB).await;
    let c = client(fake.addr()).await;
    c.echo().await.unwrap();
    fake.disconnect_all();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(c.is_closed());
    assert!(matches!(c.echo().await, Err(OvsdbError::Closed(_))));
}

#[tokio::test]
async fn a_silent_server_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _keep = tokio::spawn(async move {
        let (_tcp, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await
    });
    let c = OvsdbClient::connect(
        &Transport::Tcp,
        &Endpoint::ip(addr),
        Duration::from_millis(100),
    )
    .await
    .unwrap();
    assert!(matches!(c.echo().await, Err(OvsdbError::Timeout { .. })));
}

#[tokio::test]
async fn connects_to_the_leader_and_skips_followers() {
    let follower = FakeOvsdb::start(NB_DB).await;
    follower.with(|s| s.leader = false);
    let detached = FakeOvsdb::start(NB_DB).await;
    detached.with(|s| {
        s.leader = false;
        s.connected = false;
    });
    let leader = FakeOvsdb::start(NB_DB).await;
    let dead = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let all = [
        Endpoint::ip(dead),
        Endpoint::ip(follower.addr()),
        Endpoint::ip(detached.addr()),
        Endpoint::ip(leader.addr()),
    ];
    let c = OvsdbClient::connect_any(&Transport::Tcp, &all, NB_DB, true, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(c.endpoint().addr, leader.addr());
    // 읽기만 하면 클러스터에 붙은 팔로어도 된다(붙지 않은 서버는 아니다).
    let c = OvsdbClient::connect_any(
        &Transport::Tcp,
        &all[..3],
        NB_DB,
        false,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(c.endpoint().addr, follower.addr());
    let e = OvsdbClient::connect_any(
        &Transport::Tcp,
        &all[..3],
        NB_DB,
        true,
        Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    let text = e.to_string();
    assert!(text.contains("as the leader"), "{text}");
    assert!(text.contains("not the cluster leader"), "{text}");
    assert!(text.contains("not connected to the cluster"), "{text}");
    // SB만 내는 서버는 NB를 내지 않는다.
    let sb = FakeOvsdb::start(SB_DB).await;
    let e = OvsdbClient::connect_any(
        &Transport::Tcp,
        &[Endpoint::ip(sb.addr())],
        NB_DB,
        true,
        Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("does not serve"), "{e}");
}

#[tokio::test]
async fn transacts_against_the_fake_server_like_the_real_one() {
    let fake = FakeOvsdb::start(NB_DB).await;
    let c = client(fake.addr()).await;
    let ext = |k: &str, v: &str| map_value(&BTreeMap::from([(k.to_string(), v.to_string())]));
    // 받아 둔 "insert-switch"와 같은 요청
    let (req, _) = recorded("insert-switch");
    let ops: Vec<Value> = req["params"].as_array().unwrap()[1..].to_vec();
    c.transact(NB_DB, ops).await.unwrap();
    let rows = c
        .select(
            NB_DB,
            "Logical_Switch",
            vec![cond(
                "external_ids",
                "includes",
                ext("orbivisor:cluster", "t1"),
            )],
            &["_uuid", "name", "ports"],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let port = parse_uuids(&rows[0]["ports"]);
    assert_eq!(port.len(), 1);
    // 참조가 남은 포트를 지우면 커밋 실패, 참조 없는 포트는 사라진다.
    let e = c
        .transact(
            NB_DB,
            vec![json!({"op": "delete", "table": "Logical_Switch_Port", "where": [["name", "==", "opl-a-0"]]})],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, OvsdbError::Op { index: 1, ref error, .. } if error == "referential integrity violation"),
        "{e}"
    );
    c.transact(
        NB_DB,
        vec![json!({"op": "insert", "table": "Logical_Switch_Port", "row": {"name": "orphan"}})],
    )
    .await
    .unwrap();
    assert_eq!(fake.rows("Logical_Switch_Port").len(), 1);
    // 같은 이름의 포트는 색인 위반이다.
    let e = c
        .transact(
            NB_DB,
            vec![
                json!({"op": "insert", "table": "Logical_Switch_Port", "uuid-name": "p", "row": {"name": "opl-a-0"}}),
                json!({"op": "mutate", "table": "Logical_Switch", "where": [], "mutations": [["ports", "insert", ["set", [["named-uuid", "p"]]]]]}),
            ],
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("constraint violation"), "{e}");
    // 스위치를 지우면 포트도 사라지고, 포트의 DHCP 약한 참조는 DHCP 행을 지우면 빈다.
    c.transact(
        NB_DB,
        vec![json!({"op": "delete", "table": "Logical_Switch", "where": [["name", "==", "opl-net"]]})],
    )
    .await
    .unwrap();
    assert!(fake.rows("Logical_Switch_Port").is_empty());
    // 펜싱: 읽은 맵 그대로면 쓰고, 아니면 아무것도 바뀌지 않는다.
    let wait = |m: Value| json!({"op": "wait", "table": "NB_Global", "where": [], "columns": ["external_ids"], "until": "==", "rows": m, "timeout": 0});
    c.transact(
        NB_DB,
        vec![
            wait(json!([])),
            json!({"op": "insert", "table": "NB_Global", "row": {"external_ids": ext("orbivisor:t:fence", "1:5")}}),
        ],
    )
    .await
    .unwrap();
    let e = c
        .transact(
            NB_DB,
            vec![
                wait(json!([{"external_ids": ext("orbivisor:t:fence", "1:4")}])),
                json!({"op": "mutate", "table": "NB_Global", "where": [], "mutations": [["nb_cfg", "+=", 1]]}),
            ],
        )
        .await
        .unwrap_err();
    assert!(e.is_wait_failed());
    c.transact(
        NB_DB,
        vec![
            wait(json!([{"external_ids": ext("orbivisor:t:fence", "1:5")}])),
            json!({"op": "mutate", "table": "NB_Global", "where": [], "mutations": [["nb_cfg", "+=", 1]]}),
        ],
    )
    .await
    .unwrap();
    assert_eq!(parse_int(&fake.rows("NB_Global")[0]["nb_cfg"]), Some(1));
}

// ---- mTLS ----

struct Pki {
    ca_pem: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl Pki {
    fn new() -> Self {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "test CA");
        let cert = params.self_signed(&key).unwrap();
        Self {
            ca_pem: cert.pem(),
            issuer: rcgen::Issuer::new(params, key),
        }
    }

    /// (인증서, 키) PEM
    fn node(&self, sans: &[&str]) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params =
            rcgen::CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap();
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let cert = params.signed_by(&key, &self.issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    /// 이 CA의 클라이언트 인증서를 요구하는 서버 설정
    fn server(&self, sans: &[&str]) -> rustls::ServerConfig {
        let (cert, key) = self.node(sans);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(self.ca_pem.as_bytes()).unwrap())
            .unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .unwrap();
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(cert.as_bytes()).unwrap()],
                PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap(),
            )
            .unwrap()
    }

    fn client(&self) -> Transport {
        let (cert, key) = self.node(&["10.9.9.9"]);
        Transport::Tls(OvsdbTls::from_pem(&self.ca_pem, &cert, &key).unwrap())
    }
}

#[tokio::test]
async fn mutual_tls_checks_the_ca_and_the_server_name() {
    let pki = Pki::new();
    let fake = FakeOvsdb::start_tls(NB_DB, pki.server(&["127.0.0.1", "node1.lan"])).await;
    let at = |names: &[&str]| Endpoint {
        addr: fake.addr(),
        names: names.iter().map(|s| s.to_string()).collect(),
    };
    let t = Duration::from_secs(5);
    let c = OvsdbClient::connect(&pki.client(), &at(&["127.0.0.1"]), t)
        .await
        .unwrap();
    assert!(c.server_db(NB_DB).await.unwrap().unwrap().is_leader());
    // 노드 주소가 이름이면 그 이름으로도 된다(이름 중 하나만 맞으면).
    let c = OvsdbClient::connect(&pki.client(), &at(&["10.1.1.1", "node1.lan"]), t)
        .await
        .unwrap();
    c.echo().await.unwrap();
    // 인증서에 없는 이름
    let e = OvsdbClient::connect(&pki.client(), &at(&["10.1.1.1"]), t)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("TLS"), "{e}");
    // 다른 CA의 서버를 믿지 않고, 다른 CA의 클라이언트는 서버가 받지 않는다.
    let other = Pki::new();
    assert!(
        OvsdbClient::connect(&other.client(), &at(&["127.0.0.1"]), t)
            .await
            .is_err()
            || {
                // TLS 1.3은 클라이언트 인증서 거절을 첫 읽기에서 알린다.
                let c = OvsdbClient::connect(&other.client(), &at(&["127.0.0.1"]), t)
                    .await
                    .unwrap();
                c.echo().await.is_err()
            }
    );
    // 평문은 TLS 서버와 말하지 못한다.
    let c = OvsdbClient::connect(
        &Transport::Tcp,
        &at(&["127.0.0.1"]),
        Duration::from_millis(500),
    )
    .await
    .unwrap();
    assert!(c.echo().await.is_err());
}

#[test]
fn tls_material_is_read_from_the_node_pki_directory() {
    let pki = Pki::new();
    let (cert, key) = pki.node(&["10.0.0.1"]);
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        OvsdbTls::from_dir(dir.path()),
        Err(OvsdbError::Tls(_))
    ));
    std::fs::write(dir.path().join("ca.crt"), &pki.ca_pem).unwrap();
    std::fs::write(dir.path().join("node.crt"), cert).unwrap();
    std::fs::write(dir.path().join("node.key"), key).unwrap();
    OvsdbTls::from_dir(dir.path()).unwrap();
    assert!(OvsdbTls::from_pem("", "", "").is_err());
}

#[test]
fn takes_concatenated_and_partial_messages() {
    let mut buf = br#"{"id":1,"result":[]} {"id":2,"result":{"a":"}"}}{"id":3,"res"#.to_vec();
    let m = take_messages(&mut buf).unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[1]["result"]["a"], "}");
    assert_eq!(buf, br#"{"id":3,"res"#);
    buf.extend_from_slice(br#"ult":null}"#);
    assert_eq!(take_messages(&mut buf).unwrap().len(), 1);
    assert!(buf.is_empty());
    let mut bad = b"{\"id\":1}]".to_vec();
    assert!(take_messages(&mut bad).is_err());
}

#[test]
fn values_round_trip() {
    let m = BTreeMap::from([("a".to_string(), "1".to_string())]);
    assert_eq!(parse_map(&map_value(&m)), m);
    assert!(parse_map(&json!(["set", []])).is_empty());
    assert_eq!(parse_strings(&json!("x")), vec!["x"]);
    assert_eq!(
        parse_strings(&string_set(&["a".into(), "b".into()])),
        vec!["a", "b"]
    );
    assert_eq!(parse_uuids(&uuid_value("u")), vec!["u"]);
    assert_eq!(
        parse_uuids(&set_value(vec![uuid_value("u"), uuid_value("v")])),
        vec!["u", "v"]
    );
    assert_eq!(parse_int(&json!(["set", []])), None);
    assert_eq!(parse_bool(&json!(true)), Some(true));
    assert_eq!(named_uuid("n"), json!(["named-uuid", "n"]));
}
