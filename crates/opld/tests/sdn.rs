//! 실제 OVN 설치·구성(계획 65 스테이지 2): OVN이 없는 깨끗한 호스트에서 orbivisord가 apt로 `ovn-central`·`ovn-host`·
//! `openvswitch-switch`를 받아 노드 하나에 NB·SB Raft 클러스터와 northd를 Orbivisor 단위로 세우고 chassis로 구성한다
//! (`sdn-init`). 배포판 단위가 꺼지고 가려졌는지, NB·SB가 노드 주소의 `pssl`에만 듣는지, 클러스터 CA 인증서로만 붙는지
//! (다른 CA·인증서 없음·평문 TCP는 실패), SB `Chassis`가 노드 UUID·Geneve·underlay인지, 노드 상태(`NodeStatus.sdn`)를 본다.
//! 같은 호스트에 `ovsdb-server` 한 쌍을 127.0.0.2로 띄워 두 번째 중앙의 합류·떠나기를 실제 ovsdb로 보고, 에이전트가 멈춘
//! 단위를 다시 켜고 장치 없는 OVS 포트를 지우는지, opld를 다시 띄워도 그대로인지 본다. 끝에 실제 출력(판, `cluster/status`,
//! `ovs-vsctl` JSON)을 찍고 opl-driver 해석기로 읽는다(시험 고정물과 견주기). Task는 제어 소켓의 시험 고리로 만든다(`/sdn`
//! API는 스테이지 6).
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
use opl_driver::ovn::{
    Db, KnownCids, OvnHost, OvnOptions, OvnPaths, SystemRunner, TAG_KEY, ovsdb_server_args,
    parse_cluster_status, parse_external_ids, parse_interfaces, parse_version, quote,
};
use oplctl::Client;
use serde_json::{Value, json};

/// apt 설치를 기다리는 시간
const INSTALL_WAIT: Duration = Duration::from_secs(15 * 60);
/// 에이전트(`refresh_secs = 5`)가 맞추기를 기다리는 시간
const AGENT_WAIT: Duration = Duration::from_secs(60);

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

/// 제어 소켓의 시험 고리(`POST /v1/test/…`)
async fn hook(d: &Daemon, path: &str, body: Value) -> Value {
    let http = reqwest::Client::builder()
        .unix_socket(d.path("opld.sock"))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = http
        .post(format!("http://opld/v1/test/{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert!(status.is_success(), "{path}: {status} {text}");
    serde_json::from_str(&text).unwrap()
}

/// SDN Task를 만들고 끝날 때까지 기다린다.
async fn sdn_task(d: &Daemon, op: Value, limit: Duration) -> Value {
    let id = hook(d, "sdn-task", json!({ "op": op })).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let c = client(d);
    let deadline = Instant::now() + limit;
    loop {
        let task = c.get(&format!("/tasks/{id}")).await.unwrap();
        let state = task["status"]["state"].as_str().unwrap_or("pending");
        if state == "succeeded" || state == "failed" {
            eprintln!("task {op}: {task}");
            return task;
        }
        assert!(Instant::now() < deadline, "task did not finish: {task}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
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
    let task = sdn_task(&d, json!({ "kind": "sdn-init" }), INSTALL_WAIT).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let sdn = hook(&d, "sdn", json!({})).await;
    let status = &sdn["status"];
    assert_eq!(status["phase"], "ready", "{sdn}");
    assert_eq!(
        status["centrals"],
        json!([{ "node": node, "address": "127.0.0.1" }])
    );
    let release = status["release"].as_str().unwrap();
    assert!(release.starts_with("24.03."), "{release}");
    assert!(status.get("pending").is_none(), "{sdn}");
    let known = KnownCids {
        nb: status["nb_cid"].as_str().map(|s| s.parse().unwrap()),
        sb: status["sb_cid"].as_str().map(|s| s.parse().unwrap()),
    };
    assert!(known.nb.is_some() && known.sb.is_some(), "{sdn}");

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
    let task = sdn_task(&d, json!({ "kind": "sdn-init" }), INSTALL_WAIT).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(inode(), created);
    let task = sdn_task(&d, json!({ "kind": "sdn-chassis-install" }), INSTALL_WAIT).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let task = sdn_task(&d, json!({ "kind": "sdn-central-remove" }), AGENT_WAIT).await;
    assert_eq!(task["status"]["state"], "failed");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("last OVN central node"),
        "{task}"
    );

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

    // 5. opld를 다시 띄워도 그대로다(OVN 단위는 opld 밖의 systemd 단위).
    d.stop();
    assert_eq!(
        run("systemctl", &["is-active", "opl-ovn-nb.service"]).trim(),
        "active"
    );
    d.start().await;
    let sdn = hook(&d, "sdn", json!({})).await;
    assert_eq!(sdn["status"]["phase"], "ready");
    wait_for(
        "the node SDN status after a restart",
        AGENT_WAIT,
        || async {
            let me = d.ctl_json(&["node", "get", "node1"]).await;
            me["status"]["sdn"]["chassis"] == "ready"
        },
    )
    .await;
}
