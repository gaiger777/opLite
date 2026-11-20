//! 실제 OVN 설치·구성(계획 65 스테이지 2): OVN이 없는 깨끗한 호스트에서 orbivisord가 apt로 `ovn-central`·`ovn-host`·
//! `openvswitch-switch`를 받아 노드 하나에 NB·SB Raft 클러스터와 northd를 Orbivisor 단위로 세우고 chassis로 구성한다
//! (`sdn-init`). 배포판 단위가 꺼지고 가려졌는지, NB·SB가 노드 주소의 `pssl`에만 듣는지, 클러스터 CA 인증서로만 붙는지
//! (다른 CA·인증서 없음·평문 TCP는 실패), SB `Chassis`가 노드 UUID·Geneve·underlay인지, 노드 상태(`NodeStatus.sdn`)를 본다.
//! 같은 호스트에 `ovsdb-server` 한 쌍을 127.0.0.2로 띄워 두 번째 중앙의 합류·떠나기를 실제 ovsdb로 보고, 에이전트가 멈춘
//! 단위를 다시 켜고 장치 없는 OVS 포트를 지우는지, opld를 다시 띄워도 그대로인지 본다. 끝에 실제 출력(판, `cluster/status`,
//! `ovs-vsctl` JSON)을 찍고 opl-driver 해석기로 읽는다(시험 고정물과 견주기). 리더 OvnSync(스테이지 3)와 IPAM·DHCP
//! (스테이지 4 — 이름공간의 OVS 내부 포트가 busybox udhcpc로 IPAM 주소를 받고, 포트 보안이 위조 주소를 막는지)도 본다. Task와 SDN
//! 네트워크는 `/sdn`·`/networks` API와 orbivisorctl로 만든다(스테이지 6). 포트 바인딩(스테이지 5): SDN NIC의 QEMU VM tap이 br-int에 붙어 이 chassis에 바인딩되는지, 클라우드
//! 이미지(`OPL_TEST_CLOUD_IMAGE`, 있으면)의 게스트가 OVN DHCP로 IPAM 주소를 받는지 본다. 러너 하나는 OVS 하나라 두 chassis
//! 사이 라이브 마이그레이션은 여기서 보지 않는다(드라이버·OvnSync 시험).
//! `sudo -E env "PATH=$PATH" cargo test -p opld --features sdn-it --test sdn -- --nocapture`
//! Linux(Ubuntu 24.04 = OVN 24.03)에서 root와 apt가 닿는 네트워크가 필요하다. OVN이 설치되지 않은 호스트여야 하고 시험은
//! 호스트에 OVN 꾸러미와 데몬을 남긴다(CI 러너용).
#![cfg(all(feature = "sdn-it", target_os = "linux"))]

mod common;

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Daemon;
use opl_api::ovn_sync::nb::{NbActual, fence_key, fence_ops, parse_actual, read_ops};
use opl_driver::ovn::{
    Db, Endpoint, KnownCids, NB_DB, OvnHost, OvnOptions, OvnPaths, OvsdbClient, OvsdbTls,
    SystemRunner, TAG_KEY, Transport, ovsdb_server_args, parse_cluster_status, parse_external_ids,
    parse_interfaces, parse_version, quote,
};
use oplctl::Client;
use serde_json::{Value, json};

/// apt 설치를 기다리는 시간
const INSTALL_WAIT: Duration = Duration::from_secs(15 * 60);
/// 에이전트(`refresh_secs = 5`)가 맞추기를 기다리는 시간
const AGENT_WAIT: Duration = Duration::from_secs(60);
/// 클라우드 이미지 게스트가 켜져 DHCP로 주소를 받기까지(TCG면 몇 분)
const GUEST_WAIT: Duration = Duration::from_secs(8 * 60);

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
fn client(d: &Daemon) -> Client {
    let config: Value =
        serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
    Client::new(
        config["server"].as_str().unwrap(),
        config["token"].as_str().map(str::to_string),
        config["ca_pem"].as_str().map(str::to_string),
    )
    .unwrap()
}

/// 명령을 실행해 (성공, stdout, stderr)
fn try_run(program: &str, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 명령을 실행하고, 성공해야 하며 표준 출력을 돌려준다.
fn run(program: &str, args: &[&str]) -> String {
    let (ok, stdout, stderr) = try_run(program, args);
    assert!(ok, "{program} {args:?}: {stderr}");
    stdout
}

/// 명령의 출력을 진단으로 stderr에 찍는다(실패해도 계속).
fn show(program: &str, args: &[&str]) {
    match Command::new(program).args(args).output() {
        Ok(out) => eprintln!(
            "$ {program} {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => eprintln!("$ {program} {}: {e}", args.join(" ")),
    }
}

/// 시험이 실패하면(패닉으로 풀릴 때) OVN 상태, 단위 저널과 로그, opld 로그를 찍는다. 데몬 다음에 만든다.
struct Diagnostics {
    opld_log: PathBuf,
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        show("ovs-vsctl", &["show"]);
        show("ovs-vsctl", &["list", "Open_vSwitch"]);
        show(
            "ovs-appctl",
            &[
                "-t",
                "/run/opl/ovn/nb.ctl",
                "cluster/status",
                "OVN_Northbound",
            ],
        );
        show("ovn-sbctl", &["--db=unix:/run/opl/ovn/sb.sock", "show"]);
        show(
            "systemctl",
            &[
                "status",
                "--no-pager",
                "opl-ovn-nb",
                "opl-ovn-sb",
                "opl-ovn-northd",
                "ovn-controller",
            ],
        );
        show(
            "journalctl",
            &[
                "--no-pager",
                "-n",
                "80",
                "-u",
                "opl-ovn-nb",
                "-u",
                "opl-ovn-sb",
                "-u",
                "opl-ovn-northd",
                "-u",
                "ovn-controller",
            ],
        );
        for log in ["ovn-nb.log", "ovn-sb.log", "ovn-northd.log"] {
            show("tail", &["-n", "40", &format!("/var/log/opl/{log}")]);
        }
        show("tail", &["-n", "40", "/var/log/ovn/ovn-controller.log"]);
        let log = std::fs::read_to_string(&self.opld_log).unwrap_or_default();
        eprintln!("opld log:\n{log}");
    }
}

/// 조건이 맞을 때까지 `limit`만큼 다시 확인한다.
async fn wait_for<F, Fut>(what: &str, limit: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + limit;
    while !check().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Task가 끝날 때까지 기다린다.
async fn wait_task(d: &Daemon, task: &Value, limit: Duration) -> Value {
    let id = task["id"].as_str().unwrap().to_string();
    let c = client(d);
    let deadline = Instant::now() + limit;
    loop {
        let task = c.get(&format!("/tasks/{id}")).await.unwrap();
        let state = task["status"]["state"].as_str().unwrap_or("pending");
        if state == "succeeded" || state == "failed" {
            eprintln!("task {}: {task}", task["spec"]["op"]);
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// `GET /sdn`
async fn sdn_view(d: &Daemon) -> Value {
    client(d).get("/sdn").await.unwrap()
}

/// `orbivisorctl network create NAME --sdn ARGS…`로 SDN 네트워크를 만들고 id를 돌려준다.
async fn sdn_network(d: &Daemon, name: &str, args: &[&str]) -> String {
    let mut argv = vec!["network", "create", name, "--sdn"];
    argv.extend_from_slice(args);
    d.ctl(&argv).await.unwrap();
    client(d).get(&format!("/networks/{name}")).await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// OVN DB 파일의 클러스터 id
fn db_cid(db: &str) -> Option<uuid::Uuid> {
    let out = run("ovsdb-tool", &["db-cid", db]);
    out.trim().parse().ok()
}

/// `ovsdb-client`로 NB에 붙어 본다(성공, stderr).
fn ovsdb_client(tls: Option<(&str, &str, &str)>, remote: &str) -> (bool, String) {
    let mut args: Vec<String> = Vec::new();
    if let Some((key, cert, ca)) = tls {
        args.extend([
            format!("--private-key={key}"),
            format!("--certificate={cert}"),
            format!("--ca-cert={ca}"),
        ]);
    }
    args.extend(["--timeout=10".into(), "list-dbs".into(), remote.into()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (ok, stdout, stderr) = try_run("ovsdb-client", &args);
    (ok && stdout.contains("OVN_Northbound"), stderr)
}

/// 다른 CA가 서명한 인증서(키·인증서·CA 파일)
fn foreign_tls(dir: &std::path::Path) -> (String, String, String) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let cert = params.signed_by(&key, &issuer).unwrap();
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    };
    (
        write("other.key", &key.serialize_pem()),
        write("other.crt", &cert.pem()),
        write("other-ca.crt", &ca.pem()),
    )
}

/// 노드 TLS로 `ip`의 NB에 붙는다(서버 인증서는 노드 주소 127.0.0.1의 것이다).
fn nb_endpoint(ip: &str) -> Endpoint {
    Endpoint {
        addr: format!("{ip}:6641").parse().unwrap(),
        names: vec!["127.0.0.1".into()],
    }
}

fn client_tls(d: &Daemon) -> Transport {
    Transport::Tls(OvsdbTls::from_dir(&d.data_dir().join("pki")).unwrap())
}

/// NB 리더(127.0.0.1)의 우리 행
async fn nb_rows(d: &Daemon, tag: &str) -> NbActual {
    let c = OvsdbClient::connect(
        &client_tls(d),
        &nb_endpoint("127.0.0.1"),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    parse_actual(&c.transact(NB_DB, read_ops(tag)).await.unwrap())
}

fn has_switch(rows: &NbActual, name: &str) -> bool {
    rows.switches.iter().any(|s| s.name == name)
}

/// 시험이 띄운 `ovsdb-server`(풀릴 때 죽인다)
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn orbivisord_sets_up_ovn() {
    assert!(
        !std::path::Path::new("/etc/opl/ovn").exists(),
        "this host already has an Orbivisor OVN"
    );
    let mut d = Daemon::new("node1", "[sdn]\nrefresh_secs = 5\n");
    d.start().await;
    d.init("ovn").await;
    d.login().await;
    let _diag = Diagnostics {
        opld_log: d.path("opld.log"),
    };
    let me = d.ctl_json(&["node", "get", "node1"]).await;
    let node = me["id"].as_str().unwrap().to_string();
    assert_eq!(me["status"]["underlay"], "127.0.0.1", "{me}");

    // 1. 첫 세우기: 실제 apt 설치와 Orbivisor 단위
    // 노드 인증서 밖의 주소는 API가 받지 않는다.
    let err = client(&d)
        .post("/sdn", &json!({ "node": "node1", "address": "192.0.2.9" }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not in the certificate"), "{err}");
    d.ctl(&["sdn", "init", "node1"]).await.unwrap();
    let status = sdn_view(&d).await;
    assert_eq!(status["phase"], "ready", "{status}");
    assert_eq!(status["centrals"][0]["node"], node.as_str(), "{status}");
    assert_eq!(status["centrals"][0]["address"], "127.0.0.1", "{status}");
    let release = status["release"].as_str().unwrap();
    assert!(release.starts_with("24.03."), "{release}");
    assert_eq!(status["tasks"], json!([]), "{status}");
    let known = KnownCids {
        nb: db_cid("/var/lib/opl/ovn/nb.db"),
        sb: db_cid("/var/lib/opl/ovn/sb.db"),
    };
    assert!(known.nb.is_some() && known.sb.is_some(), "{status}");

    let dpkg = run(
        "dpkg-query",
        &[
            "-W",
            "-f",
            "${Package} ${Status}\n",
            "ovn-central",
            "ovn-host",
            "openvswitch-switch",
        ],
    );
    assert_eq!(dpkg.matches("install ok installed").count(), 3, "{dpkg}");
    for unit in [
        "ovn-central.service",
        "ovn-northd.service",
        "ovn-ovsdb-server-nb.service",
        "ovn-ovsdb-server-sb.service",
    ] {
        let (_, state, _) = try_run("systemctl", &["is-enabled", unit]);
        assert_eq!(state.trim(), "masked", "{unit}");
    }
    assert_eq!(
        std::fs::read_to_string("/etc/opl/ovn/distro-units").unwrap(),
        "ovn-central.service\novn-northd.service\novn-ovsdb-server-nb.service\novn-ovsdb-server-sb.service\n"
    );
    for unit in [
        "opl-ovn-nb.service",
        "opl-ovn-sb.service",
        "opl-ovn-northd.service",
    ] {
        assert_eq!(
            run("systemctl", &["is-active", unit]).trim(),
            "active",
            "{unit}"
        );
        assert_eq!(
            run("systemctl", &["is-enabled", unit]).trim(),
            "enabled",
            "{unit}"
        );
    }
    assert_eq!(
        run("systemctl", &["is-active", "ovn-controller.service"]).trim(),
        "active"
    );
    // NB·SB(와 Raft 포트)는 노드 주소에만 듣는다.
    let ss = run("ss", &["-ltnH"]);
    for port in [6641, 6642, 6643, 6644] {
        let listeners: Vec<&str> = ss
            .lines()
            .filter_map(|l| l.split_whitespace().nth(3))
            .filter(|a| a.ends_with(&format!(":{port}")))
            .collect();
        assert_eq!(listeners, [format!("127.0.0.1:{port}")], "{ss}");
    }
    let mode = |p: &str| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    };
    assert_eq!(mode("/etc/opl/ovn"), 0o700);
    assert_eq!(mode("/etc/opl/ovn/node.key"), 0o600);

    // 클러스터 CA mTLS: 노드 인증서만 붙는다.
    let node_tls = (
        "/etc/opl/ovn/node.key",
        "/etc/opl/ovn/node.crt",
        "/etc/opl/ovn/ca.crt",
    );
    let (ok, err) = ovsdb_client(Some(node_tls), "ssl:127.0.0.1:6641");
    assert!(ok, "{err}");
    let dir = tempfile::tempdir().unwrap();
    let (key, cert, ca) = foreign_tls(dir.path());
    let (ok, _) = ovsdb_client(
        Some((&key, &cert, "/etc/opl/ovn/ca.crt")),
        "ssl:127.0.0.1:6641",
    );
    assert!(!ok, "a certificate of another CA was accepted");
    let (ok, _) = ovsdb_client(Some((&key, &cert, &ca)), "ssl:127.0.0.1:6641");
    assert!(!ok, "a server certificate from another CA was trusted");
    let (ok, _) = ovsdb_client(None, "tcp:127.0.0.1:6641");
    assert!(!ok, "plain TCP was accepted");

    // SB Chassis = 노드 UUID, Geneve, underlay(ovn-controller가 SB에 다시 붙기까지 몇 초 걸린다)
    let chassis_rows = || {
        let out = run(
            "ovn-sbctl",
            &[
                "--db=unix:/run/opl/ovn/sb.sock",
                "--format=json",
                "--columns=name",
                "list",
                "Chassis",
            ],
        );
        serde_json::from_str::<Value>(&out).unwrap()
    };
    wait_for(
        "the chassis to register in the southbound DB",
        AGENT_WAIT,
        || async {
            chassis_rows()["data"]
                .as_array()
                .is_some_and(|r| !r.is_empty())
        },
    )
    .await;
    let chassis = chassis_rows();
    assert_eq!(chassis["data"], json!([[node]]), "{chassis}");
    let encap: Value = serde_json::from_str(&run(
        "ovn-sbctl",
        &[
            "--db=unix:/run/opl/ovn/sb.sock",
            "--format=json",
            "--columns=chassis_name,ip,type",
            "list",
            "Encap",
        ],
    ))
    .unwrap();
    assert_eq!(
        encap["data"],
        json!([[node, "127.0.0.1", "geneve"]]),
        "{encap}"
    );
    assert_eq!(
        std::fs::read_to_string("/etc/openvswitch/system-id.conf")
            .unwrap()
            .trim(),
        node
    );

    // 노드 상태(에이전트 → 검사 루프)
    wait_for("the node SDN status", AGENT_WAIT, || async {
        let me = d.ctl_json(&["node", "get", "node1"]).await;
        let sdn = &me["status"]["sdn"];
        sdn["chassis"] == "ready"
            && sdn["central"]["nb"] == "leader"
            && sdn["central"]["sb"] == "leader"
            && sdn["central"]["northd_active"] == true
    })
    .await;
    let me = d.ctl_json(&["node", "get", "node1"]).await;
    let sdn = &me["status"]["sdn"];
    assert_eq!(sdn["encap_ip"], "127.0.0.1", "{sdn}");
    assert_eq!(sdn["ovn_version"], release, "{sdn}");
    assert!(sdn.get("error").is_none(), "{sdn}");

    // 실제 출력과 해석기
    let ids_json = run(
        "ovs-vsctl",
        &[
            "--format=json",
            "--columns=external_ids",
            "list",
            "Open_vSwitch",
        ],
    );
    eprintln!("ovs-vsctl external_ids:\n{ids_json}");
    let ids = parse_external_ids(ids_json.as_bytes()).unwrap();
    let tag = ids[TAG_KEY].clone();
    assert_eq!(ids["system-id"], node);
    assert_eq!(ids["ovn-remote"], "ssl:127.0.0.1:6642");
    assert_eq!(ids["ovn-encap-type"], "geneve");
    for program in ["ovn-controller", "ovs-vsctl", "ovn-nbctl"] {
        let out = run(program, &["--version"]);
        eprintln!("{program} --version:\n{out}");
        assert!(parse_version(&out).is_some(), "{out}");
    }
    for (db, ctl) in [
        (Db::Nb, "/run/opl/ovn/nb.ctl"),
        (Db::Sb, "/run/opl/ovn/sb.ctl"),
    ] {
        let out = run("ovs-appctl", &["-t", ctl, "cluster/status", db.name()]);
        eprintln!("cluster/status {}:\n{out}", db.name());
        let s = parse_cluster_status(&out).unwrap();
        assert!(s.is_leader(), "{out}");
        let want = match db {
            Db::Nb => known.nb,
            Db::Sb => known.sb,
        };
        assert_eq!(s.cid, want);
    }
    // 쉼표가 든 값도 한 값이다(ovn-remote의 중앙 여럿).
    let value = "ssl:10.0.0.1:6642,ssl:[fd00::1]:6642";
    run(
        "ovs-vsctl",
        &[
            "set",
            "Open_vSwitch",
            ".",
            &format!("external_ids:opl-test={}", quote(value)),
        ],
    );
    assert_eq!(
        run(
            "ovs-vsctl",
            &["get", "Open_vSwitch", ".", "external_ids:opl-test"]
        )
        .trim(),
        quote(value)
    );
    run(
        "ovs-vsctl",
        &["remove", "Open_vSwitch", ".", "external_ids", "opl-test"],
    );

    // 2. 다시 하기: 이미 된 일은 건너뛴다(DB를 다시 만들지 않는다).
    let inode = || {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/var/lib/opl/ovn/nb.db").unwrap().ino()
    };
    let created = inode();
    let c = client(&d);
    let task = c.post("/sdn", &json!({ "node": "node1" })).await.unwrap();
    let task = wait_task(&d, &task, INSTALL_WAIT).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(inode(), created);
    d.ctl(&["sdn", "chassis", "install", "node1"])
        .await
        .unwrap();
    // 마지막 중앙은 API가 미리 거절한다(Task를 만들지 않는다).
    let err = d
        .ctl(&["sdn", "central", "remove", "node1"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("last OVN central node"), "{err}");
    let status = d.ctl(&["sdn", "status"]).await.unwrap();
    eprintln!("{status}");
    assert!(status.contains("node1"), "{status}");

    // 3. 두 번째 중앙(같은 호스트의 127.0.0.2): 실제 ovsdb로 합류·떠나기
    let root = tempfile::tempdir().unwrap();
    let paths = OvnPaths {
        var_lib: root.path().join("var"),
        run: root.path().join("run"),
        log: root.path().join("log"),
        systemd: root.path().join("systemd"),
        pki: d.data_dir().join("pki"),
        ..OvnPaths::default()
    };
    std::fs::create_dir_all(&paths.run).unwrap();
    std::fs::create_dir_all(&paths.log).unwrap();
    let options = OvnOptions {
        wait_limit: Duration::from_secs(60),
        ..OvnOptions::default()
    };
    let second = OvnHost::new(
        Arc::new(SystemRunner),
        paths.clone(),
        options.clone(),
        tag.clone(),
    );
    let main = OvnHost::new(
        Arc::new(SystemRunner),
        OvnPaths::default(),
        options,
        tag.clone(),
    );
    let addr: IpAddr = "127.0.0.2".parse().unwrap();
    let first: IpAddr = "127.0.0.1".parse().unwrap();
    let mut servers = Vec::new();
    for db in Db::ALL {
        assert!(second.join_cluster(db, addr, &[first]).await.unwrap());
        let args = ovsdb_server_args(db, addr, &paths);
        servers.push(Server(
            Command::new("/usr/sbin/ovsdb-server")
                .args(&args)
                .spawn()
                .unwrap(),
        ));
    }
    for (db, cid) in [(Db::Nb, known.nb), (Db::Sb, known.sb)] {
        let s = second.wait_member(db, cid).await.unwrap();
        assert!(s.is_member());
        let joined = run(
            "ovs-appctl",
            &[
                "-t",
                &paths.db_ctl(db).to_string_lossy(),
                "cluster/status",
                db.name(),
            ],
        );
        eprintln!("cluster/status {} (second, member):\n{joined}", db.name());
        let s = main.cluster_status(db).await.unwrap();
        assert_eq!(s.servers.len(), 2, "{s:?}");
        assert!(
            s.servers
                .iter()
                .any(|v| v.address == format!("ssl:127.0.0.2:{}", db.raft_port()))
        );
    }
    // 팔로어(127.0.0.2)는 리더만 받는 클라이언트가 쓰지 않는다(`ovn-nbctl`의 기본 `--leader-only`가 팔로어 소켓을
    // 거절하는 것과 같다). OvnSync는 리더로 옮기고, 읽기만 하는 쪽은 팔로어도 쓴다. 인증서 이름이 맞지 않으면 붙지 않는다.
    let tls = client_tls(&d);
    let both = [nb_endpoint("127.0.0.2"), nb_endpoint("127.0.0.1")];
    let t10 = Duration::from_secs(10);
    let c = OvsdbClient::connect_any(&tls, &both, NB_DB, true, t10)
        .await
        .unwrap();
    assert_eq!(c.endpoint().addr, both[1].addr);
    let f = OvsdbClient::connect_any(&tls, &both, NB_DB, false, t10)
        .await
        .unwrap();
    assert_eq!(f.endpoint().addr, both[0].addr);
    let s = f.server_db(NB_DB).await.unwrap().unwrap();
    assert!(!s.is_leader() && s.connected, "{s:?}");
    assert_eq!(s.cid, known.nb.map(|u| u.to_string()));
    f.select(NB_DB, "NB_Global", vec![], &["_uuid"])
        .await
        .unwrap();
    let e = OvsdbClient::connect_any(&tls, &both[..1], NB_DB, true, t10)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not the cluster leader"), "{e}");
    let wrong = Endpoint {
        addr: both[1].addr,
        names: vec!["10.255.0.1".into()],
    };
    assert!(OvsdbClient::connect(&tls, &wrong, t10).await.is_err());
    drop((c, f));
    second
        .check_foreign("node2", known, true)
        .await
        .expect("the joined databases belong to this SDN");
    for db in Db::ALL {
        second.leave_cluster(db).await.unwrap();
        let s = main.cluster_status(db).await.unwrap();
        assert_eq!(s.servers.len(), 1, "{s:?}");
        assert!(s.is_leader());
    }
    drop(servers);
    let moved = second.retire_dbs(1).unwrap();
    assert_eq!(moved.len(), 2);
    // 첫 서버의 NB는 계속 쓴다.
    run(
        "ovn-nbctl",
        &[
            "--db=unix:/run/opl/ovn/nb.sock",
            "ls-add",
            "opl-test-after-leave",
        ],
    );
    assert!(
        run("ovn-nbctl", &["--db=unix:/run/opl/ovn/nb.sock", "ls-list"])
            .contains("opl-test-after-leave")
    );
    run(
        "ovn-nbctl",
        &[
            "--db=unix:/run/opl/ovn/nb.sock",
            "ls-del",
            "opl-test-after-leave",
        ],
    );

    // 4. 에이전트: 멈춘 단위를 다시 켜고, 장치 없는 이 클러스터의 OVS 포트를 지운다(남의 포트는 둔다).
    run("systemctl", &["stop", "opl-ovn-northd.service"]);
    run("ip", &["tuntap", "add", "opltest0", "mode", "tap"]);
    run(
        "ovs-vsctl",
        &[
            "add-port",
            "br-int",
            "opltest0",
            "--",
            "set",
            "Interface",
            "opltest0",
            &format!("external_ids:{TAG_KEY}={tag}"),
        ],
    );
    run("ip", &["tuntap", "add", "othertest0", "mode", "tap"]);
    run("ovs-vsctl", &["add-port", "br-int", "othertest0"]);
    run("ip", &["link", "del", "opltest0"]);
    run("ip", &["link", "del", "othertest0"]);
    let found = run(
        "ovs-vsctl",
        &[
            "--format=json",
            "--columns=name,ofport,external_ids",
            "find",
            "Interface",
            &format!("external_ids:{TAG_KEY}={tag}"),
        ],
    );
    eprintln!("ovs-vsctl find Interface:\n{found}");
    let list = parse_interfaces(found.as_bytes()).unwrap();
    assert!(list.iter().any(|i| i.name == "opltest0"), "{found}");
    wait_for(
        "the agent to restart northd and remove the stale port",
        AGENT_WAIT,
        || async {
            let active = try_run("systemctl", &["is-active", "opl-ovn-northd.service"]).1;
            let ports = run("ovs-vsctl", &["list-ports", "br-int"]);
            active.trim() == "active" && !ports.lines().any(|p| p == "opltest0")
        },
    )
    .await;
    let ports = run("ovs-vsctl", &["list-ports", "br-int"]);
    assert!(ports.lines().any(|p| p == "othertest0"), "{ports}");
    run(
        "ovs-vsctl",
        &["--if-exists", "del-port", "br-int", "othertest0"],
    );

    // 5. 리더 OvnSync(스테이지 3): SDN 네트워크 → NB 논리 스위치, 펜싱 토큰, 되살리기, SB chassis 청소
    let sync_of = |sdn: &Value| sdn["sync"].clone();
    let net_a = sdn_network(&d, "tenant-a", &[]).await;
    let ls_a = format!("opl-{net_a}");
    wait_for("the logical switch of tenant-a", AGENT_WAIT, || async {
        has_switch(&nb_rows(&d, &tag).await, &ls_a)
    })
    .await;
    let rows = nb_rows(&d, &tag).await;
    let sw = rows.switches.iter().find(|s| s.name == ls_a).unwrap();
    assert_eq!(sw.external_ids["orbivisor:cluster"], tag);
    assert_eq!(sw.external_ids["orbivisor:id"], net_a);
    assert_eq!(sw.external_ids["orbivisor:name"], "tenant-a");
    wait_for("the sync record", AGENT_WAIT, || async {
        sync_of(&sdn_view(&d).await)["nb_cfg"].as_i64() >= Some(1)
    })
    .await;
    let sync = sync_of(&sdn_view(&d).await);
    eprintln!("sync: {sync}");
    assert!(sync.get("error").is_none(), "{sync}");
    let term = sync["term"].as_u64().unwrap();
    let global = rows.global.clone().unwrap();
    let token = global.external_ids[&fence_key(&tag)].clone();
    assert!(token.starts_with(&format!("{term}:")), "{token} {sync}");
    // 관리자가 만든 태그 없는 스위치는 남는다.
    let nbctl = |args: &[&str]| {
        let mut all = vec!["--db=unix:/run/opl/ovn/nb.sock"];
        all.extend_from_slice(args);
        run("ovn-nbctl", &all)
    };
    nbctl(&["ls-add", "admin-ls"]);

    // 펜싱: 읽은 뒤 다른 쓰는 이(더 큰 임기)가 토큰을 바꾸면 옛 쓰는 이의 트랜잭션은 실제 NB가 거절한다.
    let writer = OvsdbClient::connect(&client_tls(&d), &nb_endpoint("127.0.0.1"), t10)
        .await
        .unwrap();
    let stale = nb_rows(&d, &tag).await;
    let (newer, _) = fence_ops(stale.global.as_ref(), &tag, &format!("{}:0", term + 1));
    writer.transact(NB_DB, newer).await.unwrap();
    let (mut old, _) = fence_ops(stale.global.as_ref(), &tag, &format!("{term}:999999"));
    old.push(json!({"op": "insert", "table": "Logical_Switch", "row": {"name": "stale-writer"}}));
    let e = writer.transact(NB_DB, old).await.unwrap_err();
    assert!(e.is_wait_failed(), "{e}");
    assert!(!nbctl(&["ls-list"]).contains("stale-writer"));
    // 더 큰 임기의 토큰을 본 opld의 OvnSync도 쓰지 않는다.
    let net_b = sdn_network(&d, "tenant-b", &[]).await;
    let ls_b = format!("opl-{net_b}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!has_switch(&nb_rows(&d, &tag).await, &ls_b));
    assert_eq!(sync_of(&sdn_view(&d).await)["term"], term);
    // 토큰을 이 리더의 임기로 되돌리고, 누가 우리 스위치를 지우고, 지운 노드의 chassis와 관리자 chassis를 SB에 둔다.
    // 다음 다시 맞추기(60초)가 모두 고친다.
    let now = nb_rows(&d, &tag).await;
    let (back, _) = fence_ops(now.global.as_ref(), &tag, &format!("{term}:0"));
    writer.transact(NB_DB, back).await.unwrap();
    nbctl(&["ls-del", &ls_a]);
    let sbctl = |args: &[&str]| {
        let mut all = vec!["--db=unix:/run/opl/ovn/sb.sock"];
        all.extend_from_slice(args);
        run("ovn-sbctl", &all)
    };
    let gone = "8a0f6f8e-5d1c-4f4e-9a55-0d6b6f1d2c3b";
    sbctl(&["chassis-add", gone, "geneve", "127.0.0.9"]);
    sbctl(&["chassis-add", "admin-chassis", "geneve", "127.0.0.8"]);
    wait_for(
        "the resync to restore tenant-a, add tenant-b and drop the removed chassis",
        Duration::from_secs(120),
        || async {
            let rows = nb_rows(&d, &tag).await;
            let chassis = sbctl(&["--columns=name", "list", "Chassis"]);
            has_switch(&rows, &ls_a) && has_switch(&rows, &ls_b) && !chassis.contains(gone)
        },
    )
    .await;
    let chassis = sbctl(&["--columns=name", "list", "Chassis"]);
    assert!(
        chassis.contains("admin-chassis") && chassis.contains(&node),
        "{chassis}"
    );
    assert!(nbctl(&["ls-list"]).contains("admin-ls"));
    // 네트워크를 지우면 스위치가 곧 사라진다(변경이 차례를 부른다).
    d.ctl(&["network", "delete", "tenant-b"]).await.unwrap();
    wait_for("tenant-b's switch to go", AGENT_WAIT, || async {
        !has_switch(&nb_rows(&d, &tag).await, &ls_b)
    })
    .await;
    drop(writer);

    // 6. opld를 다시 띄워도 그대로다(OVN 단위는 opld 밖의 systemd 단위).
    // 멈춘 동안 누가 우리 스위치를 지워도, 다시 뜬 리더가 리더가 되자마자 되살리고 새 토큰을 쓴다. 투표 멤버가 하나뿐인
    // Raft는 다시 떠도 임기가 그대로일 수 있다(실제로 그렇다) — 토큰은 (임기, 인덱스)로 앞선다.
    let before = nb_rows(&d, &tag).await.global.unwrap().external_ids[&fence_key(&tag)].clone();
    let parse = |t: &str| -> (u64, u64) {
        let (a, b) = t.split_once(':').unwrap();
        (a.parse().unwrap(), b.parse().unwrap())
    };
    d.stop();
    assert_eq!(
        run("systemctl", &["is-active", "opl-ovn-nb.service"]).trim(),
        "active"
    );
    nbctl(&["ls-del", &ls_a]);
    d.start().await;
    assert_eq!(sdn_view(&d).await["phase"], "ready");
    wait_for(
        "the restarted leader to restore tenant-a",
        AGENT_WAIT,
        || async { has_switch(&nb_rows(&d, &tag).await, &ls_a) },
    )
    .await;
    let rows = nb_rows(&d, &tag).await;
    let token = rows.global.unwrap().external_ids[&fence_key(&tag)].clone();
    let (new_term, _) = parse(&token);
    assert!(parse(&token) > parse(&before), "{token} after {before}");
    wait_for("the sync record of the new term", AGENT_WAIT, || async {
        sync_of(&sdn_view(&d).await)["term"].as_u64() == Some(new_term)
    })
    .await;
    wait_for(
        "the node SDN status after a restart",
        AGENT_WAIT,
        || async {
            let me = d.ctl_json(&["node", "get", "node1"]).await;
            me["status"]["sdn"]["chassis"] == "ready"
        },
    )
    .await;

    // 7. IPAM·DHCP(스테이지 4): 서브넷이 있는 SDN 네트워크와 꺼진 VM 둘 → NB의 DHCP_Options·논리 포트(주소·포트 보안).
    // 이름공간 둘에 OVS 내부 포트를 그 두 포트의 iface-id·MAC으로 붙여(게스트 OS 없는 "VM") busybox udhcpc가 IPAM 주소·
    // MTU·DNS를 받고, 서로 ping하고, 주소를 바꾼 위조 패킷은 포트 보안이 막는다.
    Box::pin(dhcp_section(&d, &tag)).await;

    // 8. 포트 바인딩(스테이지 5): SDN NIC의 QEMU VM은 tap이 br-int의 OVS 포트(iface-id = 논리 포트)이고, ovn-controller가
    // 이 chassis에 바인딩한다(SB Port_Binding.chassis·up, NB up, ovn-installed). 클라우드 이미지가 있으면 게스트가 OVN DHCP로
    // IPAM 주소를 받았는지 같은 스위치의 이름공간 포트에서 ping으로 본다. 끄면 OVS 포트와 chassis 요청이 사라진다.
    Box::pin(binding_section(&d, &tag, &node)).await;
}

/// 이름공간 `ns`의 OVS 내부 포트 `dev`(논리 포트 `port`, MAC `mac`)
fn namespace_port(ns: &str, dev: &str, port: &str, mac: &str) {
    let _ = try_run("ip", &["netns", "del", ns]);
    run("ip", &["netns", "add", ns]);
    run(
        "ovs-vsctl",
        &[
            "--may-exist",
            "add-port",
            "br-int",
            dev,
            "--",
            "set",
            "Interface",
            dev,
            "type=internal",
            &format!("external_ids:iface-id={port}"),
        ],
    );
    run("ip", &["link", "set", dev, "netns", ns]);
    let in_ns = |args: &[&str]| {
        let mut all = vec!["netns", "exec", ns, "ip"];
        all.extend_from_slice(args);
        run("ip", &all)
    };
    in_ns(&["link", "set", dev, "address", mac]);
    in_ns(&["link", "set", "lo", "up"]);
    in_ns(&["link", "set", dev, "up"]);
}

/// 이름공간 `ns`에서 udhcpc로 주소를 받는다. 스크립트가 받은 `ip mask mtu dns`를 `out`에 적는다.
fn udhcpc(ns: &str, dev: &str, script: &std::path::Path, out: &std::path::Path) -> bool {
    let _ = std::fs::remove_file(out);
    let (ok, stdout, stderr) = try_run(
        "ip",
        &[
            "netns",
            "exec",
            ns,
            "busybox",
            "udhcpc",
            "-i",
            dev,
            "-f",
            "-q",
            "-n",
            "-t",
            "5",
            "-T",
            "1",
            "-O",
            "mtu",
            "-s",
            &script.to_string_lossy(),
        ],
    );
    eprintln!("udhcpc {ns}: ok={ok}\n{stdout}{stderr}");
    ok && out.exists()
}

async fn dhcp_section(d: &Daemon, tag: &str) {
    let net = sdn_network(d, "tenant-d", &["--subnet", "10.77.0.0/24,dns=10.77.0.53"]).await;
    let c = client(d);
    let mut vms = Vec::new();
    for name in ["dhcp-a", "dhcp-b"] {
        let vm = c
            .post(
                "/vms",
                &json!({ "name": name, "spec": {
                    "cpu": { "cores": 1 }, "memory_mib": 256,
                    "nics": [{ "network": "tenant-d", "model": "virtio" }]
                } }),
            )
            .await
            .unwrap();
        let id = vm["id"].as_str().unwrap().to_string();
        let mac = vm["spec"]["nics"][0]["mac"].as_str().unwrap().to_string();
        let ip = vm["spec"]["nics"][0]["ips"][0]
            .as_str()
            .unwrap()
            .to_string();
        let port = format!("opl-{id}-{}", mac.replace(':', ""));
        vms.push((name, id, mac, ip, port));
    }
    assert_eq!(vms[0].3, "10.77.0.2");
    assert_eq!(vms[1].3, "10.77.0.3");
    wait_for("the ports and DHCP row of tenant-d", AGENT_WAIT, || async {
        let rows = nb_rows(d, tag).await;
        rows.ports
            .iter()
            .filter(|p| vms.iter().any(|v| v.4 == p.name))
            .count()
            == 2
            && !rows.dhcp.is_empty()
    })
    .await;
    let rows = nb_rows(d, tag).await;
    let dhcp = rows
        .dhcp
        .iter()
        .find(|r| r.external_ids.get("orbivisor:id") == Some(&net))
        .unwrap();
    eprintln!("DHCP_Options: {dhcp:?}");
    assert_eq!(dhcp.cidr, "10.77.0.0/24");
    assert_eq!(dhcp.options["server_id"], "10.77.0.1");
    assert_eq!(dhcp.options["mtu"], "1442");
    assert!(!dhcp.options.contains_key("router"), "{dhcp:?}");
    for (_, _, mac, ip, port) in &vms {
        let p = rows.ports.iter().find(|p| &p.name == port).unwrap();
        assert_eq!(p.addresses, [format!("{mac} {ip}")]);
        assert_eq!(p.port_security, p.addresses);
        assert_eq!(p.dhcpv4.as_deref(), Some(dhcp.uuid.as_str()));
        // 꺼진 VM은 chassis를 정하지 않는다.
        assert!(!p.options.contains_key("requested-chassis"), "{p:?}");
    }

    let script = d.path("udhcpc.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1\" in\nbound|renew)\n  ip addr flush dev \"$interface\"\n  ip addr add \"$ip/$mask\" dev \"$interface\"\n  [ -n \"$mtu\" ] && ip link set \"$interface\" mtu \"$mtu\"\n  echo \"$ip $mask $mtu $dns\" > \"{}.$interface\"\n  ;;\nesac\n",
            d.path("lease").to_string_lossy()
        ),
    )
    .unwrap();
    run("chmod", &["755", &script.to_string_lossy()]);
    let mut leases = Vec::new();
    for (i, (_, _, mac, _, port)) in vms.iter().enumerate() {
        let (ns, dev) = (format!("oplns{i}"), format!("opldh{i}"));
        namespace_port(&ns, &dev, port, mac);
        let out = d.path(&format!("lease.{dev}"));
        wait_for(&format!("a DHCP lease in {ns}"), AGENT_WAIT, || async {
            udhcpc(&ns, &dev, &script, &out)
        })
        .await;
        let lease = std::fs::read_to_string(&out).unwrap();
        eprintln!("lease {ns}: {lease}");
        leases.push((ns, dev, lease));
    }
    for ((_, _, _, ip, _), (_, _, lease)) in vms.iter().zip(&leases) {
        assert_eq!(lease.trim(), format!("{ip} 24 1442 10.77.0.53"));
    }
    let ping = |ns: &str, to: &str| {
        try_run(
            "ip",
            &["netns", "exec", ns, "ping", "-c", "2", "-W", "2", to],
        )
        .0
    };
    let (ns_a, dev_a) = (leases[0].0.clone(), leases[0].1.clone());
    let (ip_a, ip_b) = (vms[0].3.clone(), vms[1].3.clone());
    assert!(ping(&ns_a, &ip_b), "ping {ip_a} -> {ip_b}");
    assert!(ping(&leases[1].0, &ip_a), "ping {ip_b} -> {ip_a}");
    // 위조: IPAM 주소가 아닌 주소로 보내면 포트 보안이 막는다. 제 주소로 돌리면 다시 된다.
    let in_a = |args: &[&str]| {
        let mut all = vec!["netns", "exec", ns_a.as_str(), "ip"];
        all.extend_from_slice(args);
        run("ip", &all);
    };
    in_a(&["addr", "flush", "dev", &dev_a]);
    in_a(&["addr", "add", "10.77.0.200/24", "dev", &dev_a]);
    assert!(!ping(&ns_a, &ip_b), "a spoofed source got through");
    in_a(&["addr", "flush", "dev", &dev_a]);
    in_a(&["addr", "add", &format!("{ip_a}/24"), "dev", &dev_a]);
    assert!(ping(&ns_a, &ip_b), "ping {ip_a} -> {ip_b} after restoring");
    // 주소 목록(스테이지 6): API 노드가 NB·SB에서 포트의 up과 바인딩 노드를 읽는다.
    wait_for("the IP list to show bound ports", AGENT_WAIT, || async {
        let ips = c.get("/networks/tenant-d/ips").await.unwrap();
        eprintln!("ips: {ips}");
        ips["allocations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["live"]["up"] == true && a["live"]["node"] == "node1")
    })
    .await;
    let table = d.ctl(&["network", "ips", "tenant-d"]).await.unwrap();
    assert!(
        table.contains("dhcp-a") && table.contains("10.77.0.2"),
        "{table}"
    );
    // 쓰는 서브넷은 뺄 수 없다.
    let err = d
        .ctl(&["network", "subnet", "remove", "tenant-d", "10.77.0.0/24"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("dhcp-a"), "{err}");

    // VM을 지우면 그 포트가 사라진다(주소도 풀린다).
    c.delete(&format!("/vms/{}", vms[1].1)).await.unwrap();
    wait_for("dhcp-b's port to go", AGENT_WAIT, || async {
        !nb_rows(d, tag)
            .await
            .ports
            .iter()
            .any(|p| p.name == vms[1].4)
    })
    .await;
    for (ns, dev, _) in &leases {
        let _ = try_run("ovs-vsctl", &["--if-exists", "del-port", "br-int", dev]);
        let _ = try_run("ip", &["netns", "del", ns]);
    }
}

/// OVN 소켓으로 부르는 `ovn-sbctl`·`ovn-nbctl`의 `--bare` 출력(앞뒤 공백 없이)
fn ovn_bare(program: &str, sock: &str, args: &[&str]) -> String {
    let db = format!("--db=unix:/run/opl/ovn/{sock}.sock");
    let mut all = vec![db.as_str(), "--bare"];
    all.extend_from_slice(args);
    run(program, &all).trim().to_string()
}

/// OVS 포트 `tap`의 `Interface.external_ids` 칸 하나(없으면 빈 글)
fn iface_id_field(tap: &str, key: &str) -> String {
    let (_, out, _) = try_run(
        "ovs-vsctl",
        &[
            "--if-exists",
            "get",
            "Interface",
            tap,
            &format!("external_ids:{key}"),
        ],
    );
    out.trim().trim_matches('"').to_string()
}

async fn binding_section(d: &Daemon, tag: &str, node: &str) {
    sdn_network(d, "tenant-q", &["--subnet", "10.78.0.0/24"]).await;
    // 클라우드 이미지(CI `ovn` 작업이 받아 둔다)가 있으면 게스트 OS로 켜 DHCP까지 본다. 없으면 디스크 없는 VM(펌웨어만 돈다)으로
    // 바인딩만 본다.
    let image = std::env::var_os("OPL_TEST_CLOUD_IMAGE").map(PathBuf::from);
    let mut create = vec!["vm", "create", "sdn-guest", "--nic", "tenant-q"];
    if let Some(image) = &image {
        let pool = d.path("pool");
        std::fs::create_dir_all(&pool).unwrap();
        d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
            .await
            .unwrap();
        d.wait_pool_ready("local").await;
        d.ctl(&[
            "volume",
            "import",
            "noble",
            "--pool",
            "local",
            "--file",
            image.to_str().unwrap(),
            "--source-format",
            "qcow2",
        ])
        .await
        .unwrap();
        create.extend([
            "--memory",
            "1024",
            "--machine",
            "q35",
            "--disk",
            "noble",
            "--ci-hostname",
            "sdn-guest",
        ]);
    } else {
        create.extend(["--memory", "256"]);
    }
    create.push("--start");
    let started = d.ctl(&create).await;
    if let Err(e) = started {
        panic!("vm sdn-guest did not start: {e}\n{}", d.log());
    }
    let vm = d.ctl_json(&["vm", "get", "sdn-guest"]).await;
    assert_eq!(vm["status"]["phase"], "running", "{vm}");
    let id: uuid::Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let mac: opl_types::vm::MacAddr = vm["spec"]["nics"][0]["mac"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let ip = vm["spec"]["nics"][0]["ips"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(ip, "10.78.0.2");
    let port = opl_types::sdn::port_name(id, mac);
    let tap = opl_types::network::tap_name(id, 0);

    // tap은 br-int의 OVS 포트이고 OVN 칸을 단다. Linux 브리지에는 없다.
    assert_eq!(run("ovs-vsctl", &["port-to-br", &tap]).trim(), "br-int");
    assert_eq!(iface_id_field(&tap, "iface-id"), port);
    assert_eq!(iface_id_field(&tap, "attached-mac"), mac.to_string());
    assert_eq!(iface_id_field(&tap, "vm-uuid"), id.to_string());
    assert_eq!(iface_id_field(&tap, "orbivisor-cluster"), tag);
    let link = run("ip", &["-j", "link", "show", "dev", &tap]);
    assert!(link.contains("\"master\":\"ovs-system\""), "{link}");
    assert!(link.contains("\"mtu\":1442"), "{link}");
    let args = std::fs::read_to_string(format!(
        "/proc/{}/cmdline",
        vm["status"]["pid"].as_u64().unwrap()
    ))
    .unwrap()
    .replace('\0', " ");
    assert!(
        args.contains(&format!("tap,id=net0,ifname={tap},script=no,downscript=no")),
        "{args}"
    );
    assert!(args.contains("host_mtu=1442"), "{args}");

    // NB는 이 노드를 요청하고, ovn-controller가 바인딩해 흐름을 다 깔면 OVS·SB·NB가 모두 안다.
    wait_for("the port to request this chassis", AGENT_WAIT, || async {
        nb_rows(d, tag).await.ports.iter().any(|p| {
            p.name == port && p.options.get("requested-chassis").map(String::as_str) == Some(node)
        })
    })
    .await;
    wait_for("ovn-installed on the tap", AGENT_WAIT, || async {
        iface_id_field(&tap, "ovn-installed") == "true"
    })
    .await;
    let chassis = ovn_bare(
        "ovn-sbctl",
        "sb",
        &[
            "--columns=_uuid",
            "find",
            "Chassis",
            &format!("name={node}"),
        ],
    );
    let binding = |column: &str| {
        ovn_bare(
            "ovn-sbctl",
            "sb",
            &[
                &format!("--columns={column}"),
                "find",
                "Port_Binding",
                &format!("logical_port={port}"),
            ],
        )
    };
    wait_for("the SB port binding", AGENT_WAIT, || async {
        binding("chassis") == chassis && binding("up") == "true"
    })
    .await;
    wait_for("the NB port to be up", AGENT_WAIT, || async {
        ovn_bare("ovn-nbctl", "nb", &["lsp-get-up", &port]) == "up"
    })
    .await;
    eprintln!(
        "Port_Binding {port}: chassis={} up={}",
        binding("chassis"),
        binding("up")
    );

    // 게스트 OS가 있으면: cloud-init network-config의 DHCP로 OVN이 IPAM 주소를 주었는지, 같은 스위치의 다른 포트(꺼진 VM의
    // 포트에 붙인 이름공간)에서 ping으로 본다.
    if image.is_some() {
        let c = client(d);
        let probe = c
            .post(
                "/vms",
                &json!({ "name": "sdn-probe", "spec": {
                    "cpu": { "cores": 1 }, "memory_mib": 256,
                    "nics": [{ "network": "tenant-q", "model": "virtio" }]
                } }),
            )
            .await
            .unwrap();
        let probe_id: uuid::Uuid = probe["id"].as_str().unwrap().parse().unwrap();
        let probe_mac = probe["spec"]["nics"][0]["mac"]
            .as_str()
            .unwrap()
            .to_string();
        let probe_port = opl_types::sdn::port_name(probe_id, probe_mac.parse().unwrap());
        wait_for("sdn-probe's port", AGENT_WAIT, || async {
            nb_rows(d, tag)
                .await
                .ports
                .iter()
                .any(|p| p.name == probe_port)
        })
        .await;
        let lease = d.path("lease.q");
        let script = d.path("udhcpc-q.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$1\" in\nbound|renew)\n  ip addr flush dev \"$interface\"\n  ip addr add \"$ip/$mask\" dev \"$interface\"\n  echo \"$ip\" > \"{}\"\n  ;;\nesac\n",
                lease.to_string_lossy()
            ),
        )
        .unwrap();
        run("chmod", &["755", &script.to_string_lossy()]);
        let (ns, dev) = ("oplnsq", "opldhq");
        namespace_port(ns, dev, &probe_port, &probe_mac);
        wait_for(
            "a DHCP lease for the probe namespace",
            AGENT_WAIT,
            || async { udhcpc(ns, dev, &script, &lease) },
        )
        .await;
        assert_eq!(std::fs::read_to_string(&lease).unwrap().trim(), "10.78.0.3");
        // 게스트 부팅(KVM이면 수십 초, TCG면 몇 분)
        wait_for(
            "the guest to answer on its IPAM address",
            GUEST_WAIT,
            || async {
                try_run(
                    "ip",
                    &["netns", "exec", ns, "ping", "-c", "1", "-W", "2", &ip],
                )
                .0
            },
        )
        .await;
        let neigh = run("ip", &["netns", "exec", ns, "ip", "neigh", "show", &ip]);
        assert!(neigh.contains(&mac.to_string()), "{neigh}");
        let _ = try_run("ovs-vsctl", &["--if-exists", "del-port", "br-int", dev]);
        let _ = try_run("ip", &["netns", "del", ns]);
        c.delete(&format!("/vms/{probe_id}")).await.unwrap();
    }

    // 끄면 tap과 OVS 포트가 사라지고(장치 없는 포트를 남기지 않는다), 꺼진 VM의 포트는 chassis를 요청하지 않는다.
    d.ctl(&["vm", "stop", "sdn-guest", "--force"])
        .await
        .unwrap();
    wait_for("the tap and its OVS port to go", AGENT_WAIT, || async {
        let ports = run("ovs-vsctl", &["list-ports", "br-int"]);
        !ports.lines().any(|p| p == tap) && !try_run("ip", &["link", "show", "dev", &tap]).0
    })
    .await;
    wait_for(
        "the stopped VM's port to release the chassis",
        AGENT_WAIT,
        || async {
            nb_rows(d, tag)
                .await
                .ports
                .iter()
                .any(|p| p.name == port && !p.options.contains_key("requested-chassis"))
        },
    )
    .await;
    wait_for("the SB port binding to go", AGENT_WAIT, || async {
        binding("chassis").is_empty()
    })
    .await;
    d.ctl(&["vm", "delete", "sdn-guest"]).await.unwrap();
    if image.is_some() {
        d.ctl(&["volume", "delete", "noble"]).await.unwrap();
    }
}
