//! 실제 OVN 설치·구성(계획 65 스테이지 2): OVN이 없는 깨끗한 호스트에서 orbivisord가 apt로 `ovn-central`·`ovn-host`·
//! `openvswitch-switch`를 받아 노드 하나에 NB·SB Raft 클러스터와 northd를 Orbivisor 단위로 세우고 chassis로 구성한다
//! (`sdn-init`). 배포판 단위가 꺼지고 가려졌는지, NB·SB가 노드 주소의 `pssl`에만 듣는지, 클러스터 CA 인증서로만 붙는지
//! (다른 CA·인증서 없음·평문 TCP는 실패), SB `Chassis`가 노드 UUID·Geneve·underlay인지, 노드 상태(`NodeStatus.sdn`)를 본다.
//! 같은 호스트에 `ovsdb-server` 한 쌍을 127.0.0.2로 띄워 두 번째 중앙의 합류·떠나기를 실제 ovsdb로 보고, 에이전트가 멈춘
//! 단위를 다시 켜고 장치 없는 OVS 포트를 지우는지, opld를 다시 띄워도 그대로인지 본다. 끝에 실제 출력(판, `cluster/status`,
//! `ovs-vsctl` JSON)을 찍고 orbivisor-driver 해석기로 읽는다(시험 고정물과 견주기). 리더 OvnSync(스테이지 3)와 IPAM·DHCP
//! (스테이지 4 — 이름공간의 OVS 내부 포트가 busybox udhcpc로 IPAM 주소를 받고, 포트 보안이 위조 주소를 막는지)도 본다. Task와 SDN
//! 네트워크는 `/sdn`·`/networks` API와 orbivisorctl로 만든다(스테이지 6). 포트 바인딩(스테이지 5): SDN NIC의 QEMU VM tap이 br-int에 붙어 이 chassis에 바인딩되는지, 클라우드
//! 이미지(`OPL_TEST_CLOUD_IMAGE`, 있으면)의 게스트가 OVN DHCP로 IPAM 주소를 받는지 본다. 러너 하나는 OVS 하나라 두 chassis
//! 사이 라이브 마이그레이션은 여기서 보지 않는다(드라이버·OvnSync 시험). 계획 66 스테이지 2: 노드 physnet(관리자 Linux 브리지
//! `oplext0`에 veth·OVS 브리지·`ovn-bridge-mappings`, 게이트웨이 표시)과 그 localnet 데이터 길, 빼기 청소를 본다. 계획 66
//! 스테이지 3: OvnSync가 쓴 공급자 네트워크의 localnet 포트, 라우터(동서 라우팅·DHCP 기본 경로), 게이트웨이 chassis의 SNAT·
//! 유동 IP, 외부에서 들어오는 큰 패킷(MTU 탐침), RA(SLAAC)·상태 있는 DHCPv6를 물리 망 이름공간 `ext`와 견준다. 계획 66
//! 스테이지 5: 노드 physnet·공급자 네트워크·라우터·유동 IP는 API와 orbivisorctl(`node sdn`·`network create --provider`·
//! `router`·`fip`)로 만들고, IPv6 이름공간 "VM"은 API VM의 포트(EUI-64·DHCPv6 IPAM)이며, 클라우드 이미지가 있으면 게스트가
//! cloud-init `ipv6_slaac`로 EUI-64 주소를 쓰는지 본다.
//! `sudo -E env "PATH=$PATH" cargo test -p orbivisord --features sdn-it --test sdn -- --nocapture`
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
use orbivisor_api::ovn_sync::nb::{NbActual, fence_key, fence_ops, parse_actual, read_ops};
use orbivisor_driver::ovn::{
    Db, Endpoint, KnownCids, NB_DB, OvnHost, OvnOptions, OvnPaths, OvsdbClient, OvsdbTls,
    SystemRunner, TAG_KEY, Transport, ovsdb_server_args, parse_cluster_status, parse_external_ids,
    parse_interfaces, parse_version, quote,
};
use orbivisorctl::Client;
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
        show("ovn-nbctl", &["--db=unix:/run/opl/ovn/nb.sock", "show"]);
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

    // 1b. OVN 접근 잠금(계획 67 §10): 새 클러스터의 `sdn init`은 잠금을 켠 채 세운다(SB 설정 파일·6645·허용 목록).
    // 뒤의 두 번째 중앙(127.0.0.2 — Orbivisor 중앙이 아님)과 이전 계획의 절은 잠금을 끄고 본다(끄면 계획 66 글 그대로).
    // 잠금 자체(RBAC·허용 목록·다시 켜기)는 끝의 `lockdown_section`이 본다.
    wait_for("the initial lockdown", AGENT_WAIT, || async {
        lockdown_shape(&tag, true).is_ok()
    })
    .await;
    lockdown_shape(&tag, true).unwrap();
    let said = d.ctl(&["sdn", "lockdown", "off"]).await.unwrap();
    eprintln!("{said}");
    lockdown_shape(&tag, false).unwrap();

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

    // 9. physnet 연결(계획 66 스테이지 2): 관리자 Linux 브리지 `oplext0`에 veth로 Orbivisor OVS 브리지를 잇고
    // `ovn-bridge-mappings`·`ovn-cms-options`를 쓴다(남의 항목은 둔다). Orbivisor 공급자 네트워크(스테이지 3 — OvnSync의
    // localnet 포트)로 이름공간 "VM"이 물리 망 이름공간 `ext`에 ping한다. veth가 사라지면(재부팅) 다시 만든다. 라우터·SNAT·
    // 유동 IP·IPv6(스테이지 3)를 본 뒤, 빼면 내 것만 다 치운다.
    Box::pin(physnet_section(&d, &tag, &node)).await;

    // 11. SDN NIC의 보안 그룹(계획 66 스테이지 4): 그룹 목록 Port_Group·ACL로 허용·막기, 처음 맞는 규칙(그룹 순서),
    // 그룹을 바꾸는 동안 열린 순간이 없음, 허용을 빼면 맺은 연결이 끊김 — 같은 그룹을 단순 브리지의 nftables(방화벽 에이전트의
    // `ruleset`)에도 걸어 같은 탐침 표가 같은지 본다(다른 것은 맺은 연결뿐 — 설계 §11.4).
    Box::pin(security_section(&d, &tag)).await;

    // 12. 단순 → SDN 옮기기(계획 67 §9, 스테이지 4): Linux 브리지 네트워크 `lan`의 QEMU VM은 tap만 `br-int`로(라이브),
    // 꺼진 이름공간 "VM"은 스펙만(cloud-init 주소를 그대로), 그동안 `l2gateway` 다리로 옮긴 쪽과 남은 쪽이 같은 L2다. 하나를
    // 되돌리고 끝내면 다리(NB 행·매핑·veth)가 사라진다.
    Box::pin(migration_section(&d, &tag, &node)).await;

    // 13. OVN 접근 잠금(계획 67 §10): 다시 켜고 SB RBAC(역할 포트에서 남의 행·다른 표 쓰기 거절, 자기 행은 됨, northd는
    // 6645로 일함)·허용 목록 밖 출발 주소의 시간 초과를 본 뒤 끄면 계획 66 글 그대로다.
    Box::pin(lockdown_section(&d, &tag, &node)).await;
}

/// 잠금이 켜졌으면(`on`) SB 설정 파일(0600)·SB와 northd 단위가 잠근 모양이고 6645에 듣고 허용 목록 표가 있다. 꺼졌으면
/// 설정 파일·6645·표가 없고 단위 글은 계획 66과 바이트까지 같다. 다르면 까닭
fn lockdown_shape(tag: &str, on: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let paths = OvnPaths::default();
    let me: IpAddr = "127.0.0.1".parse().unwrap();
    let mine = [me];
    let locked: &[IpAddr] = if on { &mine } else { &[] };
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let config = std::path::Path::new("/etc/opl/ovn/sb.json");
    if on {
        if read("/etc/opl/ovn/sb.json") != orbivisor_driver::ovn::sb_config(me, &paths) {
            return Err("sb.json".into());
        }
        let mode = std::fs::metadata(config).unwrap().permissions().mode() & 0o777;
        if mode != 0o600 {
            return Err(format!("sb.json mode {mode:o}"));
        }
    } else if config.exists() {
        return Err("sb.json is still there".into());
    }
    if read("/etc/systemd/system/opl-ovn-sb.service")
        != orbivisor_driver::ovn::db_unit(Db::Sb, me, on, &paths)
    {
        return Err("the SB unit".into());
    }
    if read("/etc/systemd/system/opl-ovn-northd.service")
        != orbivisor_driver::ovn::northd_unit(&[me], locked, &paths)
    {
        return Err("the northd unit".into());
    }
    let ss = run("ss", &["-ltnH"]);
    if ss.contains("127.0.0.1:6645") != on {
        return Err(format!("listeners:\n{ss}"));
    }
    let table = format!("opl_ovn_{tag}");
    let (present, _, _) = try_run("nft", &["list", "table", "inet", &table]);
    if present != on {
        return Err(format!("nft table present = {present}"));
    }
    Ok(())
}

/// 출발 주소 `from`에서 `127.0.0.1:port`로 TCP를 맺어 본다(5초 — 허용 목록 밖이면 SYN이 버려져 시간 초과).
fn tcp_connects(from: &str, port: u16) -> bool {
    let script = "import socket,sys\ns=socket.socket()\ns.settimeout(5)\ns.bind((sys.argv[1],0))\n\
                  try:\n  s.connect(('127.0.0.1',int(sys.argv[2])))\n  print('connected')\n\
                  except Exception as e:\n  print('failed',e)\n";
    let out = run("python3", &["-c", script, from, &port.to_string()]);
    eprintln!("tcp {from} -> 127.0.0.1:{port}: {}", out.trim());
    out.starts_with("connected")
}

/// 노드 인증서로 `port`의 SB에 `ovn-sbctl`(성공, stderr)
fn sb_as_node(port: u16, args: &[&str]) -> (bool, String) {
    let db = format!("--db=ssl:127.0.0.1:{port}");
    let mut all = vec![
        db.as_str(),
        "--private-key=/etc/opl/ovn/node.key",
        "--certificate=/etc/opl/ovn/node.crt",
        "--ca-cert=/etc/opl/ovn/ca.crt",
        "--timeout=20",
    ];
    all.extend_from_slice(args);
    let (ok, out, err) = try_run("ovn-sbctl", &all);
    (ok, format!("{out}{err}"))
}

/// OVN 접근 잠금(12번, 노드 하나): `sdn lockdown on` → SB가 설정 파일로 6642(`ovn-controller` 역할)·6645(northd)에 듣고,
/// 노드 인증서(CN = 이 chassis)로 역할 포트에서 남의 `Chassis` 행 바꾸기·남의 이름으로 chassis 넣기·`SB_Global` 쓰기가
/// RBAC로 거절되고 자기 행은 바뀐다. 특권 포트는 같은 인증서로 된다. ovn-controller가 역할 포트로 `nb_cfg`를 따라잡고
/// (`--wait=hv`), northd가 6645로 일한다. 허용 목록 밖 출발 주소(127.0.0.3)는 6641·6642·6645에서 시간 초과다. 끄면
/// 계획 66 글로 돌아가고 다시 붙는다.
async fn lockdown_section(d: &Daemon, tag: &str, node: &str) {
    let started = Instant::now();
    let said = d.ctl(&["sdn", "lockdown", "on"]).await.unwrap();
    eprintln!(
        "{said}\nPROBE lockdown on (one central) took {:?}",
        started.elapsed()
    );
    lockdown_shape(tag, true).unwrap();
    let remotes = run(
        "ovs-appctl",
        &["-t", "/run/opl/ovn/sb.ctl", "ovsdb-server/list-remotes"],
    );
    eprintln!("PROBE SB list-remotes under lockdown:\n{remotes}");
    let nft = run("nft", &["list", "table", "inet", &format!("opl_ovn_{tag}")]);
    eprintln!("nft table:\n{nft}");

    // RBAC: 남의 chassis(이름이 UUID가 아니라 에이전트 청소가 두는 것)를 로컬 소켓으로 만든다.
    let sbctl = |args: &[&str]| {
        let mut all = vec!["--db=unix:/run/opl/ovn/sb.sock"];
        all.extend_from_slice(args);
        run("ovn-sbctl", &all)
    };
    sbctl(&["chassis-add", "opl-fake-chassis", "geneve", "127.0.0.9"]);
    let (ok, said) = sb_as_node(
        6642,
        &[
            "set",
            "Chassis",
            "opl-fake-chassis",
            "external_ids:opl-test=x",
        ],
    );
    eprintln!("PROBE RBAC another chassis via 6642: ok={ok} {said}");
    assert!(!ok, "a chassis certificate changed another chassis: {said}");
    let (ok, said) = sb_as_node(
        6642,
        &[
            "chassis-add",
            "8d6b5c4a-0000-4000-8000-000000000001",
            "geneve",
            "127.0.0.7",
        ],
    );
    assert!(!ok, "a chassis certificate added another chassis: {said}");
    let (ok, said) = sb_as_node(6642, &["set", "SB_Global", ".", "external_ids:opl-test=x"]);
    eprintln!("PROBE RBAC SB_Global via 6642: ok={ok} {said}");
    assert!(!ok, "a chassis certificate wrote SB_Global: {said}");
    let (ok, said) = sb_as_node(6642, &["set", "Chassis", node, "external_ids:opl-test=1"]);
    assert!(ok, "its own chassis row: {said}");
    let (ok, said) = sb_as_node(
        6642,
        &["remove", "Chassis", node, "external_ids", "opl-test"],
    );
    assert!(ok, "{said}");
    // 특권 포트(northd 몫)는 역할이 없다.
    let (ok, said) = sb_as_node(
        6645,
        &[
            "set",
            "Chassis",
            "opl-fake-chassis",
            "external_ids:opl-test=x",
        ],
    );
    assert!(ok, "the privileged port refused a write: {said}");
    sbctl(&["chassis-del", "opl-fake-chassis"]);
    // ovn-controller가 역할 포트로 자기 `Chassis_Private.nb_cfg`를 쓰고 northd가 6645로 일한다.
    run(
        "ovn-nbctl",
        &[
            "--db=unix:/run/opl/ovn/nb.sock",
            "--timeout=60",
            "--wait=hv",
            "sync",
        ],
    );
    assert_eq!(
        run("ovn-appctl", &["-t", "/run/opl/ovn/northd.ctl", "status"]).trim(),
        "Status: active"
    );

    // 허용 목록: 루프백 127.0.0.1(노드 주소)은 붙고 그 밖의 출발 주소는 시간 초과
    for port in [6641, 6642, 6645] {
        assert!(tcp_connects("127.0.0.1", port), "{port}");
        assert!(!tcp_connects("127.0.0.3", port), "127.0.0.3 reached {port}");
    }

    // 끄기: 계획 66 글, 다시 붙는다.
    let started = Instant::now();
    d.ctl(&["sdn", "lockdown", "off"]).await.unwrap();
    eprintln!(
        "PROBE lockdown off (one central) took {:?}",
        started.elapsed()
    );
    lockdown_shape(tag, false).unwrap();
    assert!(tcp_connects("127.0.0.3", 6641));
    run(
        "ovn-nbctl",
        &[
            "--db=unix:/run/opl/ovn/nb.sock",
            "--timeout=60",
            "--wait=hv",
            "sync",
        ],
    );
}

/// 로컬 OVS DB `Open_vSwitch`의 `external_ids` 칸 하나(없으면 빈 글)
fn ovs_id(key: &str) -> String {
    let ids = parse_external_ids(
        run(
            "ovs-vsctl",
            &[
                "--format=json",
                "--columns=external_ids",
                "list",
                "Open_vSwitch",
            ],
        )
        .as_bytes(),
    )
    .unwrap();
    ids.get(key).cloned().unwrap_or_default()
}

fn set_ovs_id(key: &str, value: &str) {
    run(
        "ovs-vsctl",
        &[
            "set",
            "Open_vSwitch",
            ".",
            &format!("external_ids:{key}={}", quote(value)),
        ],
    );
}

/// `ovn-nbctl --bare get …`(없거나 실패하면 빈 글). 식별자가 아닌 글(숫자로 시작하는 UUID 따위)의 따옴표는 뗀다.
fn nb_get(args: &[&str]) -> String {
    let mut all = vec!["--db=unix:/run/opl/ovn/nb.sock", "--bare", "get"];
    all.extend_from_slice(args);
    let (ok, out, _) = try_run("ovn-nbctl", &all);
    if ok {
        out.trim().trim_matches('"').to_string()
    } else {
        String::new()
    }
}

/// 이름공간 `ns`에서 ping(`extra`는 앞에 붙는 인자)
fn ping_from(ns: &str, extra: &[&str], to: &str) -> bool {
    let mut all = vec!["netns", "exec", ns, "ping"];
    all.extend_from_slice(extra);
    all.extend_from_slice(&["-c", "2", "-W", "2", to]);
    try_run("ip", &all).0
}

/// 이름공간 `ns`의 장치 `dev`에서 `filter`에 맞는 패킷 하나를 기다리며 `action`을 한다. tcpdump가 없으면 None.
fn captured(ns: &str, dev: &str, filter: &str, action: impl FnOnce()) -> Option<bool> {
    if !try_run("which", &["tcpdump"]).0 {
        eprintln!("tcpdump is not installed; not capturing {filter}");
        return None;
    }
    let mut args = vec![
        "netns", "exec", ns, "timeout", "15", "tcpdump", "-n", "-l", "-c", "1", "-i", dev,
    ];
    args.extend(filter.split(' '));
    let child = Command::new("ip")
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    action();
    let out = child.wait_with_output().unwrap();
    eprintln!(
        "tcpdump {filter}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Some(out.status.success() && !out.stdout.is_empty())
}

/// API로 꺼진 VM(NIC 하나, `network`)을 만들고 (id, MAC, IPAM 주소, 논리 포트)
async fn api_vm(d: &Daemon, name: &str, network: &str) -> (String, String, String, String) {
    let vm = client(d)
        .post(
            "/vms",
            &json!({ "name": name, "spec": {
                "cpu": { "cores": 1 }, "memory_mib": 256,
                "nics": [{ "network": network, "model": "virtio" }]
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
    (id, mac, ip, port)
}

async fn physnet_section(d: &Daemon, tag: &str, node: &str) {
    let (o, b, x) = orbivisor_types::sdn::physnet_ifnames(tag, "ext");
    // 관리자 망: Linux 브리지와 "물리 라우터" 이름공간
    let _ = try_run("ip", &["netns", "del", "ext"]);
    let _ = try_run("ip", &["link", "del", "oplext0"]);
    run("ip", &["link", "add", "oplext0", "type", "bridge"]);
    run("ip", &["link", "set", "oplext0", "up"]);
    run("ip", &["netns", "add", "ext"]);
    run(
        "ip",
        &[
            "link", "add", "oplextr", "type", "veth", "peer", "name", "oplexti",
        ],
    );
    run("ip", &["link", "set", "oplextr", "master", "oplext0", "up"]);
    run("ip", &["link", "set", "oplexti", "netns", "ext"]);
    let in_ext = |args: &[&str]| {
        let mut all = vec!["netns", "exec", "ext", "ip"];
        all.extend_from_slice(args);
        run("ip", &all);
    };
    in_ext(&["link", "set", "lo", "up"]);
    in_ext(&["addr", "add", "203.0.113.1/24", "dev", "oplexti"]);
    in_ext(&["link", "set", "oplexti", "up"]);
    // 남의 매핑·옵션
    set_ovs_id("ovn-bridge-mappings", "lab:br-lab");
    set_ovs_id("ovn-cms-options", "foo=bar");

    // 노드 SDN 설정은 API(`PUT /nodes`의 `spec.sdn`)로 — orbivisorctl `node sdn`
    let said = d
        .ctl(&[
            "node",
            "sdn",
            "node1",
            "--physnet",
            "ext=oplext0",
            "--gateway",
            "100",
        ])
        .await
        .unwrap();
    assert!(said.contains("ext=oplext0"), "{said}");
    wait_for("physnet ext to be ready", AGENT_WAIT, || async {
        let me = d.ctl_json(&["node", "get", "node1"]).await;
        let sdn = &me["status"]["sdn"];
        sdn["physnets"][0]["ready"] == true && sdn["gateway"] == true
    })
    .await;
    let me = d.ctl_json(&["node", "get", "node1"]).await;
    assert_eq!(
        me["status"]["sdn"]["physnets"],
        json!([{ "name": "ext", "bridge": "oplext0", "ready": true }]),
        "{me}"
    );
    show("ovs-vsctl", &["show"]);
    assert_eq!(ovs_id("ovn-bridge-mappings"), format!("lab:br-lab,ext:{x}"));
    assert_eq!(ovs_id("ovn-cms-options"), "foo=bar,enable-chassis-as-gw");
    assert_eq!(
        run(
            "ovs-vsctl",
            &["get", "Bridge", &x, &format!("external_ids:{TAG_KEY}")]
        )
        .trim()
        // ovs-vsctl는 식별자 같은 글을 따옴표 없이 찍는다(모두 숫자인 태그는 따옴표).
        .trim_matches('"'),
        tag
    );
    assert_eq!(
        run(
            "ovs-vsctl",
            &["get", "Bridge", &x, "external_ids:orbivisor-physnet"]
        )
        .trim(),
        "ext"
    );
    let link = |name: &str| -> Value {
        serde_json::from_str::<Value>(&run("ip", &["-j", "link", "show", "dev", name])).unwrap()[0]
            .clone()
    };
    let veth = link(&b);
    assert_eq!(veth["master"], "oplext0", "{veth}");
    assert!(
        veth["flags"].as_array().unwrap().contains(&json!("UP")),
        "{veth}"
    );
    // 관리자 브리지에는 veth 한쪽만 더해진다(다른 포트는 그대로).
    let ports: Vec<String> =
        serde_json::from_str::<Value>(&run("ip", &["-j", "link", "show", "master", "oplext0"]))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["ifname"].as_str().unwrap().to_string())
            .collect();
    assert_eq!(ports, ["oplextr".to_string(), b.clone()], "{ports:?}");
    // OVS 브리지에는 veth와 ovn-controller의 patch 포트(localnet이 생기면)
    let ovs_ports = run("ovs-vsctl", &["list-ports", &x]);
    assert!(ovs_ports.lines().any(|p| p == o), "{ovs_ports}");

    // 데이터 길: Orbivisor 공급자 네트워크 `ext`(OvnSync가 쓴 localnet 포트)로 물리 망에 닿는다. 이름공간 "VM"은 그
    // 스위치에 손으로 넣은 포트(주인 표시 없음 — OvnSync가 두는 남의 포트)다.
    let ext = sdn_network(
        d,
        "ext",
        &[
            "--provider",
            "ext",
            "--subnet",
            "203.0.113.0/24,gateway=203.0.113.1,floating=203.0.113.100-203.0.113.199",
        ],
    )
    .await;
    let list = d.ctl(&["network", "list"]).await.unwrap();
    assert!(
        list.lines()
            .any(|l| l.starts_with("ext ") && l.contains("provider") && l.contains("1500")),
        "{list}"
    );
    let ln = format!("opl-ln-{ext}");
    wait_for("the localnet port of ext", AGENT_WAIT, || async {
        nb_rows(d, tag).await.ports.iter().any(|p| p.name == ln)
    })
    .await;
    let rows = nb_rows(d, tag).await;
    let port = rows.ports.iter().find(|p| p.name == ln).unwrap();
    assert_eq!(port.kind, "localnet", "{port:?}");
    assert_eq!(port.addresses, ["unknown"]);
    assert_eq!(port.options["network_name"], "ext");
    assert_eq!(port.tag, None);
    // 공급자 서브넷의 DHCP는 기본 꺼짐
    assert!(
        !rows
            .dhcp
            .iter()
            .any(|r| r.external_ids.get("orbivisor:id") == Some(&ext)),
        "{:?}",
        rows.dhcp
    );
    let nb = |args: &[&str]| ovn_bare("ovn-nbctl", "nb", args);
    let ext_switch = format!("opl-{ext}");
    nb(&[
        "lsp-add",
        &ext_switch,
        "probe-vm",
        "--",
        "lsp-set-addresses",
        "probe-vm",
        "02:00:00:00:66:02 203.0.113.10",
    ]);
    namespace_port("oplphys", "oplphys0", "probe-vm", "02:00:00:00:66:02");
    run(
        "ip",
        &[
            "netns",
            "exec",
            "oplphys",
            "ip",
            "addr",
            "add",
            "203.0.113.10/24",
            "dev",
            "oplphys0",
        ],
    );
    wait_for("the localnet patch port", AGENT_WAIT, || async {
        run("ovs-vsctl", &["list-ports", &x])
            .lines()
            .any(|p| p.starts_with("patch-"))
    })
    .await;
    let ping = |ns: &str, to: &str| {
        try_run(
            "ip",
            &["netns", "exec", ns, "ping", "-c", "2", "-W", "2", to],
        )
        .0
    };
    wait_for("a ping over the physnet", AGENT_WAIT, || async {
        ping("oplphys", "203.0.113.1")
    })
    .await;
    assert!(ping("ext", "203.0.113.10"), "ping from the physical side");

    // 재부팅처럼 veth가 사라지면 에이전트가 다시 만든다(OVS 브리지는 OVS DB에 남는다).
    run("ip", &["link", "del", "dev", &b]);
    wait_for("the veth to come back", AGENT_WAIT, || async {
        try_run("ip", &["link", "show", "dev", &b]).0 && ping("oplphys", "203.0.113.1")
    })
    .await;
    assert_eq!(ovs_id("ovn-bridge-mappings"), format!("lab:br-lab,ext:{x}"));
    // OvnSync는 남의 포트를 두었다.
    assert!(
        nb(&["lsp-list", &ext_switch]).contains("probe-vm"),
        "the foreign port is gone"
    );
    let _ = try_run(
        "ovs-vsctl",
        &["--if-exists", "del-port", "br-int", "oplphys0"],
    );
    let _ = try_run("ip", &["netns", "del", "oplphys"]);
    nb(&["lsp-del", "probe-vm"]);

    // 10. 라우터·SNAT·유동 IP·IPv6(스테이지 3)
    Box::pin(routing_section(d, tag, node)).await;

    // 12. 계획 67 스테이지 2: 부하 분산(VIP·건강 검사 넘김·바깥 VIP·출발지 고정)과 QoS(OVN 행·단순 NIC tc)
    Box::pin(services_section(d, tag)).await;

    // 빼기: 내 매핑 항목·veth·OVS 브리지·게이트웨이 낱말만 사라지고 남의 것은 남는다.
    d.ctl(&["network", "delete", "ext"]).await.unwrap();
    wait_for("ext's switch to go", AGENT_WAIT, || async {
        !has_switch(&nb_rows(d, tag).await, &ext_switch)
    })
    .await;
    d.ctl(&[
        "node",
        "sdn",
        "node1",
        "--no-physnet",
        "ext",
        "--no-gateway",
    ])
    .await
    .unwrap();
    wait_for("physnet ext to go", AGENT_WAIT, || async {
        let me = d.ctl_json(&["node", "get", "node1"]).await;
        let sdn = &me["status"]["sdn"];
        sdn.get("physnets").is_none() && sdn.get("gateway").is_none()
    })
    .await;
    assert_eq!(ovs_id("ovn-bridge-mappings"), "lab:br-lab");
    assert_eq!(ovs_id("ovn-cms-options"), "foo=bar");
    assert!(
        !try_run("ovs-vsctl", &["br-exists", &x]).0,
        "{x} is still there"
    );
    assert!(
        !try_run("ip", &["link", "show", "dev", &b]).0,
        "{b} is still there"
    );
    assert!(
        !try_run("ip", &["link", "show", "dev", &o]).0,
        "{o} is still there"
    );
    assert!(try_run("ip", &["link", "show", "dev", "oplext0"]).0);
    assert!(try_run("ip", &["link", "show", "dev", "oplextr"]).0);

    run(
        "ovs-vsctl",
        &[
            "remove",
            "Open_vSwitch",
            ".",
            "external_ids",
            "ovn-bridge-mappings",
            "ovn-cms-options",
        ],
    );
    let _ = try_run("ip", &["netns", "del", "ext"]);
    let _ = try_run("ip", &["link", "del", "oplext0"]);
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
    let mac: orbivisor_types::vm::MacAddr = vm["spec"]["nics"][0]["mac"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let ip = vm["spec"]["nics"][0]["ips"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(ip, "10.78.0.2");
    let port = orbivisor_types::sdn::port_name(id, mac);
    let tap = orbivisor_types::network::tap_name(id, 0);

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
        let probe_port = orbivisor_types::sdn::port_name(probe_id, probe_mac.parse().unwrap());
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

/// 계획 66 스테이지 3: 오버레이 둘을 잇는 라우터(외부 = 공급자 네트워크 `ext`, 게이트웨이 = 이 노드)와 이름공간 "VM"들.
/// 동서 ping, DHCP 기본 경로, SNAT(`ext`는 10.x로 가는 경로가 없으므로 답이 오는 것이 곧 주소 바꾸기다), 중앙 유동 IP(꺼진
/// VM이라 chassis가 없다 — 분산은 OvnSync 시험과 여러 노드 작업), 큰 패킷 탐침, RA·DHCPv6.
async fn routing_section(d: &Daemon, tag: &str, node: &str) {
    let t1 = sdn_network(d, "rt-t1", &["--subnet", "10.10.0.0/24"]).await;
    let t2 = sdn_network(d, "rt-t2", &["--subnet", "10.20.0.0/24"]).await;
    let a = api_vm(d, "rt-a", "rt-t1").await;
    let b = api_vm(d, "rt-b", "rt-t2").await;
    assert_eq!(a.2, "10.10.0.2");
    assert_eq!(b.2, "10.20.0.2");
    let said = d
        .ctl(&[
            "router",
            "create",
            "r1",
            "--external",
            "ext,ip=203.0.113.50",
            "--interface",
            "rt-t1",
            "--interface",
            "rt-t2",
        ])
        .await
        .unwrap();
    assert!(said.contains("203.0.113.50"), "{said}");
    let c = client(d);
    let r1 = c.get("/routers/r1?live=false").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let gw = format!("opl-gw-{r1}");
    wait_for("the gateway port to be hosted here", AGENT_WAIT, || async {
        let hosting = nb_get(&["Logical_Router_Port", &gw, "status:hosting-chassis"]);
        eprintln!("hosting-chassis of {gw}: {hosting:?}");
        hosting == node
    })
    .await;
    // 보기(계획 66 스테이지 5): NB의 `hosting-chassis`가 노드 이름으로, 게이트웨이 순위 하나
    let view = d.ctl_json(&["router", "get", "r1"]).await;
    assert_eq!(view["hosting"], "node1", "{view}");
    assert_eq!(view["gateways"][0]["node"], "node1", "{view}");
    assert_eq!(view["gateways"][0]["active"], true, "{view}");
    assert_eq!(view["gateways"][0]["priority"], 32767, "{view}");
    let rows = nb_rows(d, tag).await;
    eprintln!("routers: {:?}\nnat: {:?}", rows.routers, rows.nat);
    assert_eq!(rows.ha_chassis.len(), 1);
    assert_eq!(rows.ha_chassis[0].chassis_name, node);
    assert_eq!(rows.ha_chassis[0].priority, 32767);
    let mut snat: Vec<(&str, &str, &str)> = rows
        .nat
        .iter()
        .map(|n| {
            (
                n.kind.as_str(),
                n.external_ip.as_str(),
                n.logical_ip.as_str(),
            )
        })
        .collect();
    snat.sort();
    assert_eq!(
        snat,
        [
            ("snat", "203.0.113.50", "10.10.0.0/24"),
            ("snat", "203.0.113.50", "10.20.0.0/24")
        ]
    );
    for (net, gw_ip) in [(&t1, "10.10.0.1"), (&t2, "10.20.0.1")] {
        let dhcp = rows
            .dhcp
            .iter()
            .find(|r| r.external_ids.get("orbivisor:id") == Some(net))
            .unwrap();
        assert_eq!(dhcp.options["router"], gw_ip, "{dhcp:?}");
    }

    // 이름공간 "VM" 둘: DHCP로 주소·기본 경로
    let script = d.path("udhcpc-route.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncase \"$1\" in\nbound|renew)\n  ip addr flush dev \"$interface\"\n  ip addr add \"$ip/$mask\" dev \"$interface\"\n  [ -n \"$mtu\" ] && ip link set \"$interface\" mtu \"$mtu\"\n  [ -n \"$router\" ] && ip route replace default via \"$router\"\n  echo \"$ip $mask $mtu $router\" > \"{}.$interface\"\n  ;;\nesac\n",
            d.path("rlease").to_string_lossy()
        ),
    )
    .unwrap();
    run("chmod", &["755", &script.to_string_lossy()]);
    for (i, (vm, gw_ip)) in [(&a, "10.10.0.1"), (&b, "10.20.0.1")].iter().enumerate() {
        let (ns, dev) = (format!("oplrt{i}"), format!("oplrt{i}d"));
        namespace_port(&ns, &dev, &vm.3, &vm.1);
        let out = d.path(&format!("rlease.{dev}"));
        wait_for(&format!("a DHCP lease in {ns}"), AGENT_WAIT, || async {
            udhcpc(&ns, &dev, &script, &out)
        })
        .await;
        let lease = std::fs::read_to_string(&out).unwrap();
        assert_eq!(lease.trim(), format!("{} 24 1442 {gw_ip}", vm.2));
    }
    // 동서(분산 라우팅): 서로 다른 오버레이 사이
    wait_for("a ping across the router", AGENT_WAIT, || async {
        ping_from("oplrt0", &[], &b.2)
    })
    .await;
    assert!(ping_from("oplrt1", &[], &a.2), "ping {} -> {}", b.2, a.2);
    // SNAT: `ext`에는 10.x로 가는 경로가 없다.
    assert!(
        !ping_from("ext", &[], &a.2),
        "ext reached a private address"
    );
    wait_for(
        "a ping to the physical router through SNAT",
        AGENT_WAIT,
        || async { ping_from("oplrt0", &[], "203.0.113.1") },
    )
    .await;
    let seen = captured("ext", "oplexti", "icmp and src host 203.0.113.50", || {
        assert!(ping_from("oplrt1", &[], "203.0.113.1"));
    });
    assert_ne!(seen, Some(false), "ext did not see the router's address");

    // 유동 IP(중앙): 밖에서 들어오고, 나가는 패킷의 보낸 주소가 유동 IP다.
    d.ctl(&[
        "fip",
        "create",
        "--network",
        "ext",
        "--ip",
        "203.0.113.120",
        "--name",
        "fip1",
        "--attach",
        &format!("rt-a:{}", a.1),
    ])
    .await
    .unwrap();
    let view = d.ctl_json(&["fip", "get", "fip1"]).await;
    assert_eq!(view["state"]["kind"], "centralized", "{view}");
    // 유동 IP 풀에서 고른 주소(이름 없이 만들면 주소에서 이름)
    let said = d.ctl(&["fip", "create", "--network", "ext"]).await.unwrap();
    assert!(said.contains("203-0-113-100"), "{said}");
    d.ctl(&["fip", "delete", "203-0-113-100"]).await.unwrap();
    wait_for("the floating IP NAT row", AGENT_WAIT, || async {
        nb_rows(d, tag)
            .await
            .nat
            .iter()
            .any(|n| n.kind == "dnat_and_snat" && n.external_ip == "203.0.113.120")
    })
    .await;
    let rows = nb_rows(d, tag).await;
    let fip = rows.nat.iter().find(|n| n.kind == "dnat_and_snat").unwrap();
    assert_eq!(fip.logical_ip, a.2);
    // 꺼진 VM이라 chassis가 없어 중앙이다.
    assert_eq!(fip.external_mac, None, "{fip:?}");
    wait_for("a ping to the floating IP", AGENT_WAIT, || async {
        ping_from("ext", &[], "203.0.113.120")
    })
    .await;
    let seen = captured("ext", "oplexti", "icmp and src host 203.0.113.120", || {
        assert!(ping_from("oplrt0", &[], "203.0.113.1"));
    });
    assert_ne!(seen, Some(false), "ext did not see the floating IP");
    // 큰 패킷 탐침(설계 §9.1): 밖(1500)에서 DF로 안(1442)으로
    let (ok, out, err) = try_run(
        "ip",
        &[
            "netns",
            "exec",
            "ext",
            "ping",
            "-M",
            "do",
            "-s",
            "1472",
            "-c",
            "3",
            "-W",
            "2",
            "203.0.113.120",
        ],
    );
    eprintln!("PROBE large DF ping ext -> floating IP: ok={ok}\n{out}{err}");
    show(
        "ip",
        &[
            "netns",
            "exec",
            "ext",
            "ip",
            "route",
            "get",
            "203.0.113.120",
        ],
    );
    // 떼면 SNAT로 돌아간다.
    d.ctl(&["fip", "detach", "fip1"]).await.unwrap();
    wait_for("the floating IP NAT row to go", AGENT_WAIT, || async {
        !nb_rows(d, tag)
            .await
            .nat
            .iter()
            .any(|n| n.kind == "dnat_and_snat")
    })
    .await;
    assert!(ping_from("oplrt0", &[], "203.0.113.1"));
    let seen = captured("ext", "oplexti", "icmp and src host 203.0.113.50", || {
        assert!(ping_from("oplrt0", &[], "203.0.113.1"));
    });
    assert_ne!(
        seen,
        Some(false),
        "ext did not see the router's address again"
    );

    // IPv6: SLAAC(RA)와 상태 있는 DHCPv6. 이름공간 "VM"은 API VM의 포트다 — SLAAC 서브넷의 IPAM 주소는 MAC의 EUI-64,
    // 상태 있는 DHCPv6 서브넷은 풀에서(계획 66 스테이지 5).
    let t6 = sdn_network(
        d,
        "rt-t6",
        &[
            "--subnet",
            "10.60.0.0/24",
            "--subnet",
            "fd00:60::/64,ipv6-mode=slaac,dns=fd00:60::53",
        ],
    )
    .await;
    let t7 = sdn_network(
        d,
        "rt-t7",
        &[
            "--subnet",
            "10.70.0.0/24",
            "--subnet",
            "fd00:70::/64,ipv6-mode=dhcpv6-stateful",
        ],
    )
    .await;
    for net in ["rt-t6", "rt-t7"] {
        d.ctl(&["router", "interface", "add", "r1", net])
            .await
            .unwrap();
    }
    let lrp6 = format!("opl-lrp-{t6}");
    wait_for("the RA config of rt-t6", AGENT_WAIT, || async {
        nb_get(&["Logical_Router_Port", &lrp6, "ipv6_ra_configs:address_mode"]) == "slaac"
    })
    .await;
    let v6vm = c
        .post(
            "/vms",
            &json!({ "name": "rt-v6", "spec": {
                "cpu": { "cores": 1 }, "memory_mib": 256,
                "nics": [{ "network": "rt-t6", "model": "virtio" }]
            } }),
        )
        .await
        .unwrap();
    let v6id = v6vm["id"].as_str().unwrap().to_string();
    let mac6: orbivisor_types::vm::MacAddr = v6vm["spec"]["nics"][0]["mac"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let eui = orbivisor_types::ipam::eui64("fd00:60::".parse().unwrap(), mac6);
    assert_eq!(v6vm["spec"]["nics"][0]["ips"][1], eui.to_string(), "{v6vm}");
    let port6 = orbivisor_types::sdn::port_name(v6id.parse().unwrap(), mac6);
    let sysctl = |ns: &str, key: &str| {
        run("ip", &["netns", "exec", ns, "sysctl", "-qw", key]);
    };
    let ns6 = "oplrt6";
    let _ = try_run("ip", &["netns", "del", ns6]);
    run("ip", &["netns", "add", ns6]);
    for key in [
        "net.ipv6.conf.default.accept_ra=2",
        "net.ipv6.conf.default.autoconf=1",
        "net.ipv6.conf.default.use_tempaddr=0",
        "net.ipv6.conf.default.addr_gen_mode=0",
    ] {
        sysctl(ns6, key);
    }
    run(
        "ovs-vsctl",
        &[
            "--may-exist",
            "add-port",
            "br-int",
            "oplrt6d",
            "--",
            "set",
            "Interface",
            "oplrt6d",
            "type=internal",
            &format!("external_ids:iface-id={port6}"),
        ],
    );
    run("ip", &["link", "set", "oplrt6d", "netns", ns6]);
    let in6 = |args: &[&str]| {
        let mut all = vec!["netns", "exec", ns6, "ip"];
        all.extend_from_slice(args);
        run("ip", &all)
    };
    in6(&["link", "set", "oplrt6d", "address", &mac6.to_string()]);
    in6(&["link", "set", "lo", "up"]);
    in6(&["link", "set", "oplrt6d", "up"]);
    wait_for(
        "a SLAAC address and default route from RA",
        AGENT_WAIT,
        || async {
            let addr = in6(&["-6", "addr", "show", "dev", "oplrt6d"]);
            let route = in6(&["-6", "route", "show", "default"]);
            addr.contains(&eui.to_string()) && route.contains("via fe80::")
        },
    )
    .await;
    eprintln!(
        "RA: {}{}",
        in6(&["-6", "addr", "show", "dev", "oplrt6d"]),
        in6(&["-6", "route"])
    );
    wait_for("a ping6 to the router port", AGENT_WAIT, || async {
        ping_from(ns6, &["-6"], "fd00:60::1")
    })
    .await;
    // 클라우드 이미지 게스트의 SLAAC(설계 §10 탐침): cloud-init v1 `ipv6_slaac`로 켠 Ubuntu 24.04 게스트가 RA에서 MAC의
    // EUI-64 주소를 만드는지. 포트 보안이 IPAM 주소(EUI-64)만 보내므로 그 주소로 ping6이 닿으면 게스트가 EUI-64를 쓴다.
    if let Some(image) = std::env::var_os("OPL_TEST_CLOUD_IMAGE").map(PathBuf::from) {
        Box::pin(guest_slaac(d, &image, ns6)).await;
    }

    // 상태 있는 DHCPv6: API VM의 포트에 OvnSync가 우리 DHCPv6 행을 단다.
    let d6 = api_vm(d, "rt-d6", "rt-t7").await;
    let d6vm = c.get("/vms/rt-d6").await.unwrap();
    let d6addr = d6vm["spec"]["nics"][0]["ips"][1]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(d6addr, "fd00:70::2", "{d6vm}");
    wait_for("the DHCPv6 options on rt-d6's port", AGENT_WAIT, || async {
        let rows = nb_rows(d, tag).await;
        let Some(row) = rows
            .dhcp
            .iter()
            .find(|r| r.external_ids.get("orbivisor:id") == Some(&t7) && r.cidr == "fd00:70::/64")
        else {
            return false;
        };
        rows.ports
            .iter()
            .any(|p| p.name == d6.3 && p.dhcpv6.as_deref() == Some(row.uuid.as_str()))
    })
    .await;
    namespace_port("oplrt7", "oplrt7d", &d6.3, &d6.1);
    let (has_udhcpc6, _, _) = try_run("busybox", &["udhcpc6", "--help"]);
    if has_udhcpc6 {
        let s6 = d.path("udhcpc6.sh");
        std::fs::write(
            &s6,
            format!(
                "#!/bin/sh\ncase \"$1\" in\nbound|renew)\n  echo \"$ipv6\" > \"{}\"\n  ;;\nesac\n",
                d.path("lease6").to_string_lossy()
            ),
        )
        .unwrap();
        run("chmod", &["755", &s6.to_string_lossy()]);
        let out6 = d.path("lease6");
        wait_for("a DHCPv6 lease", AGENT_WAIT, || async {
            let (ok, o, e) = try_run(
                "ip",
                &[
                    "netns",
                    "exec",
                    "oplrt7",
                    "busybox",
                    "udhcpc6",
                    "-i",
                    "oplrt7d",
                    "-f",
                    "-q",
                    "-n",
                    "-t",
                    "5",
                    "-T",
                    "1",
                    "-s",
                    &s6.to_string_lossy(),
                ],
            );
            eprintln!("udhcpc6: ok={ok}\n{o}{e}");
            ok && out6.exists()
        })
        .await;
        let lease = std::fs::read_to_string(&out6).unwrap();
        // udhcpc6는 주소를 줄이지 않고 찍는다.
        let got: std::net::Ipv6Addr = lease.trim().parse().unwrap();
        assert_eq!(got, d6addr.parse::<std::net::Ipv6Addr>().unwrap());
    } else {
        eprintln!("PROBE busybox has no udhcpc6; the stateful DHCPv6 lease was not checked");
    }

    // 치우기: 이름공간, 손으로 넣은 포트, 유동 IP·라우터·네트워크·VM. OvnSync가 우리 라우터 행을 모두 지운다.
    for (ns, dev) in [
        ("oplrt0", "oplrt0d"),
        ("oplrt1", "oplrt1d"),
        (ns6, "oplrt6d"),
        ("oplrt7", "oplrt7d"),
    ] {
        let _ = try_run("ovs-vsctl", &["--if-exists", "del-port", "br-int", dev]);
        let _ = try_run("ip", &["netns", "del", ns]);
    }
    d.ctl(&["fip", "delete", "fip1"]).await.unwrap();
    // 인터페이스가 남은 라우터는 지울 수 없다(409 `router_in_use`).
    let refused = d.ctl(&["router", "delete", "r1"]).await.unwrap_err();
    assert!(refused.to_string().contains("interfaces"), "{refused}");
    for net in ["rt-t1", "rt-t2", "rt-t6", "rt-t7"] {
        d.ctl(&["router", "interface", "remove", "r1", net])
            .await
            .unwrap();
    }
    d.ctl(&["router", "delete", "r1"]).await.unwrap();
    wait_for("the router rows to go", AGENT_WAIT, || async {
        let rows = nb_rows(d, tag).await;
        rows.routers.is_empty()
            && rows.router_ports.is_empty()
            && rows.ha_groups.is_empty()
            && rows.ha_chassis.is_empty()
            && rows.nat.is_empty()
            && rows.routes.is_empty()
            && !rows.ports.iter().any(|p| p.kind == "router")
    })
    .await;
    for vm in [&a.0, &b.0, &v6id, &d6.0] {
        c.delete(&format!("/vms/{vm}")).await.unwrap();
    }
    for name in ["rt-t1", "rt-t2", "rt-t6", "rt-t7"] {
        d.ctl(&["network", "delete", name]).await.unwrap();
    }
}

/// 클라우드 이미지 게스트(`rt-guest6`, SLAAC 서브넷 `rt-t6`)가 EUI-64 주소로 답하는지 이름공간 `ns6`에서 ping6으로 본다.
/// 풀 `local`은 바인딩 절이 만들어 두었다.
async fn guest_slaac(d: &Daemon, image: &std::path::Path, ns6: &str) {
    d.ctl(&[
        "volume",
        "import",
        "noble6",
        "--pool",
        "local",
        "--file",
        image.to_str().unwrap(),
        "--source-format",
        "qcow2",
    ])
    .await
    .unwrap();
    let started = d
        .ctl(&[
            "vm",
            "create",
            "rt-guest6",
            "--nic",
            "rt-t6",
            "--memory",
            "1024",
            "--machine",
            "q35",
            "--disk",
            "noble6",
            "--ci-hostname",
            "rt-guest6",
            "--start",
        ])
        .await;
    if let Err(e) = started {
        panic!("vm rt-guest6 did not start: {e}\n{}", d.log());
    }
    let vm = d.ctl_json(&["vm", "get", "rt-guest6"]).await;
    let mac: orbivisor_types::vm::MacAddr = vm["spec"]["nics"][0]["mac"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let eui = orbivisor_types::ipam::eui64("fd00:60::".parse().unwrap(), mac);
    assert_eq!(vm["spec"]["nics"][0]["ips"][1], eui.to_string(), "{vm}");
    let answered = {
        let start = std::time::Instant::now();
        loop {
            if ping_from(ns6, &["-6"], &eui.to_string()) {
                break true;
            }
            if start.elapsed() > GUEST_WAIT {
                break false;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    };
    eprintln!("PROBE cloud image guest answers on its EUI-64 SLAAC address {eui}: {answered}");
    if !answered {
        show("ip", &["netns", "exec", ns6, "ip", "-6", "neigh", "show"]);
    }
    d.ctl(&["vm", "stop", "rt-guest6", "--force"])
        .await
        .unwrap();
    d.ctl(&["vm", "delete", "rt-guest6"]).await.unwrap();
    d.ctl(&["volume", "delete", "noble6"]).await.unwrap();
    assert!(
        answered,
        "the cloud image guest did not answer on its EUI-64 address {eui} (cloud-init ipv6_slaac)"
    );
}

/// 이름공간의 파이썬 메아리 서버(tcp 22·8080, 받은 바이트를 돌려줌)
const ECHO_SERVER: &str = r#"
import socket, threading
def serve(port):
    s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("0.0.0.0", port)); s.listen(64)
    while True:
        c, _ = s.accept()
        def h(c):
            try:
                while True:
                    d = c.recv(64)
                    if not d: break
                    c.sendall(d)
            except OSError: pass
        threading.Thread(target=h, args=(c,), daemon=True).start()
for p in (22, 8080): threading.Thread(target=serve, args=(p,), daemon=True).start()
threading.Event().wait()
"#;

/// TCP로 붙어 한 바이트가 돌아오는지(2초)
const TCP_PROBE: &str = r#"
import socket, sys
try:
    s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=2)
    s.sendall(b"x"); sys.exit(0 if s.recv(1) == b"x" else 1)
except OSError: sys.exit(1)
"#;

/// 맺은 연결 하나로 0.2초마다 메아리를 받는다. 1초 안에 돌아오지 않으면 3으로 끝난다.
const HELD_CONNECTION: &str = r#"
import socket, sys, time
s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=2)
s.settimeout(1)
while True:
    s.sendall(b"x")
    try:
        if s.recv(1) != b"x": sys.exit(2)
    except OSError: sys.exit(3)
    print("ok", flush=True); time.sleep(0.2)
"#;

/// `seconds` 동안 20ms마다 tcp 22·8080에 붙어 본다. 붙은 수를 찍는다.
const CONTINUOUS_PROBE: &str = r#"
import socket, sys, time
end = time.time() + float(sys.argv[2]); hits = 0; tries = 0
while time.time() < end:
    for port in (22, 8080):
        tries += 1
        try:
            s = socket.create_connection((sys.argv[1], port), timeout=0.05); s.close(); hits += 1
        except OSError: pass
    time.sleep(0.02)
print(hits, tries)
"#;

fn py_in(ns: &str, script: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new("ip");
    cmd.args(["netns", "exec", ns, "python3", "-c", script]);
    cmd.args(args);
    cmd
}

fn tcp_from(ns: &str, to: &str, port: u16) -> bool {
    py_in(ns, TCP_PROBE, &[to, &port.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

/// 보안 그룹 탐침 표의 "세계" 하나: 지키는 VM 이름공간과 손님 둘(c1 = 허용된 원격, c2 = 다른 주소)
struct SgWorld {
    vm: String,
    c1: String,
    c2: String,
}

const VM_IP: &str = "10.88.0.2";
const C1_IP: &str = "10.88.0.3";

impl SgWorld {
    /// (탐침, 닿았는지): 손님 둘의 ICMP·tcp 22, c1의 tcp 8080, VM에서 나가는 ping
    fn probes(&self) -> Vec<(&'static str, bool)> {
        vec![
            ("icmp from c1", ping_from(&self.c1, &[], VM_IP)),
            ("icmp from c2", ping_from(&self.c2, &[], VM_IP)),
            ("tcp 22 from c1", tcp_from(&self.c1, VM_IP, 22)),
            ("tcp 22 from c2", tcp_from(&self.c2, VM_IP, 22)),
            ("tcp 8080 from c1", tcp_from(&self.c1, VM_IP, 8080)),
            ("ping out to c1", ping_from(&self.vm, &[], C1_IP)),
        ]
    }

    fn tcp22_c1(&self) -> bool {
        tcp_from(&self.c1, VM_IP, 22)
    }

    /// c1에서 tcp 22로 맺어 둔 연결(메아리가 끊기면 끝남)
    fn hold(&self) -> Server {
        Server(
            py_in(&self.c1, HELD_CONNECTION, &[VM_IP, "22"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

/// 맺은 연결 `held`가 `within` 안에 끊기는지(끝난 상태 코드). 끊기지 않으면 None.
fn cut_within(held: &mut Server, within: Duration) -> Option<std::process::ExitStatus> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(s) = held.0.try_wait().unwrap() {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

const ALLOWED: [(&str, bool); 6] = [
    ("icmp from c1", true),
    ("icmp from c2", true),
    ("tcp 22 from c1", true),
    ("tcp 22 from c2", false),
    ("tcp 8080 from c1", false),
    ("ping out to c1", true),
];

async fn security_section(d: &Daemon, tag: &str) {
    let c = client(d);
    sdn_network(d, "sg-t", &["--subnet", "10.88.0.0/24"]).await;
    for (name, rules) in [
        (
            "sg-icmp",
            json!([{"direction": "in", "action": "accept", "protocol": "icmp"}]),
        ),
        (
            "sg-ssh",
            json!([{"direction": "in", "action": "accept", "protocol": "tcp", "ports": "22",
                    "remote": format!("{C1_IP}/32")}]),
        ),
        (
            "sg-no22",
            json!([{"direction": "in", "action": "drop", "protocol": "tcp", "ports": "22"}]),
        ),
    ] {
        c.post(
            "/security-groups",
            &json!({ "name": name, "spec": { "rules": rules } }),
        )
        .await
        .unwrap();
    }
    // 지키는 VM(그룹 icmp, ssh)과 손님 둘(그룹 없음). IPAM이 .2, .3, .4를 준다.
    let mut ports = Vec::new();
    for (name, groups) in [
        ("sg-vm", json!(["sg-icmp", "sg-ssh"])),
        ("sg-c1", json!([])),
        ("sg-c2", json!([])),
    ] {
        let vm = c
            .post(
                "/vms",
                &json!({ "name": name, "spec": {
                    "cpu": { "cores": 1 }, "memory_mib": 256,
                    "nics": [{ "network": "sg-t", "model": "virtio", "security_groups": groups }]
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
        ports.push((format!("opl-{id}-{}", mac.replace(':', "")), mac, ip));
    }
    assert_eq!(
        ports.iter().map(|p| p.2.as_str()).collect::<Vec<_>>(),
        [VM_IP, C1_IP, "10.88.0.4"]
    );
    let vm_port = ports[0].0.clone();
    // Port_Group 하나에 지키는 VM의 포트만, ACL 다섯(DHCP, 규칙 둘, 기본 둘)
    wait_for("the port group of sg-vm", AGENT_WAIT, || async {
        let rows = nb_rows(d, tag).await;
        let Some(port) = rows.ports.iter().find(|p| p.name == vm_port) else {
            return false;
        };
        rows.port_groups.len() == 1 && rows.port_groups[0].ports == std::slice::from_ref(&port.uuid)
    })
    .await;
    let rows = nb_rows(d, tag).await;
    let pg = &rows.port_groups[0];
    eprintln!("port group {pg:?}\nacls {:?}", rows.acls);
    assert_eq!(pg.external_ids["orbivisor:sg-list"], "sg-icmp,sg-ssh");
    assert_eq!(pg.acls.len(), 5);

    // OVN 세계: 이름공간 셋(OVS 내부 포트, 고정 주소)
    let ovn = SgWorld {
        vm: "oplsgv".into(),
        c1: "oplsg1".into(),
        c2: "oplsg2".into(),
    };
    for ((port, mac, ip), (ns, dev)) in ports.iter().zip([
        (&ovn.vm, "oplsgv0"),
        (&ovn.c1, "oplsg10"),
        (&ovn.c2, "oplsg20"),
    ]) {
        namespace_port(ns, dev, port, mac);
        run(
            "ip",
            &[
                "netns",
                "exec",
                ns,
                "ip",
                "addr",
                "add",
                &format!("{ip}/24"),
                "dev",
                dev,
            ],
        );
    }

    // nftables 세계: 단순 Linux 브리지와 veth 셋(브리지 쪽 이름이 tap 이름), 같은 주소·MAC
    let nft = SgWorld {
        vm: "oplsbv".into(),
        c1: "oplsb1".into(),
        c2: "oplsb2".into(),
    };
    let _ = try_run("ip", &["link", "del", "oplsgbr"]);
    run("ip", &["link", "add", "oplsgbr", "type", "bridge"]);
    run("ip", &["link", "set", "oplsgbr", "up"]);
    // 러너에 br_netfilter가 있으면 브리지 프레임이 iptables FORWARD(도커의 DROP)를 지난다.
    for f in [
        "/proc/sys/net/bridge/bridge-nf-call-iptables",
        "/proc/sys/net/bridge/bridge-nf-call-ip6tables",
    ] {
        if std::path::Path::new(f).exists() {
            std::fs::write(f, "0").unwrap();
        }
    }
    let taps = ["opsgv", "opsgc1", "opsgc2"];
    for (((_, mac, ip), ns), tap) in ports.iter().zip([&nft.vm, &nft.c1, &nft.c2]).zip(taps) {
        let _ = try_run("ip", &["netns", "del", ns]);
        run("ip", &["netns", "add", ns]);
        run(
            "ip",
            &[
                "link", "add", tap, "type", "veth", "peer", "name", "eth0", "netns", ns,
            ],
        );
        run("ip", &["link", "set", tap, "master", "oplsgbr", "up"]);
        for args in [
            vec!["link", "set", "eth0", "address", mac],
            vec!["link", "set", "lo", "up"],
            vec!["link", "set", "eth0", "up"],
            vec!["addr", "add", &format!("{ip}/24"), "dev", "eth0"],
        ] {
            let mut all = vec!["netns", "exec", ns.as_str(), "ip"];
            all.extend(args);
            run("ip", &all);
        }
    }
    let groups: Vec<orbivisor_types::security_group::SecurityGroup> =
        serde_json::from_value(c.get("/security-groups").await.unwrap()["items"].clone()).unwrap();
    let vm_mac: orbivisor_types::vm::MacAddr = ports[0].1.parse().unwrap();
    let apply_nft = |names: &[&str]| {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let nic = orbivisor_driver::firewall::FirewallNic {
            tap: taps[0].into(),
            mac: vm_mac,
            rules: orbivisor_types::security_group::nic_policy(&names, &groups).rules,
        };
        let text = orbivisor_driver::firewall::ruleset("sgcmp", &[nic]);
        let mut child = Command::new("nft")
            .args(["-f", "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "nft -f:\n{text}");
    };
    // 그룹 전: 둘 다 열려 있다(OVN은 그룹 없는 포트를 거르지 않는다 — 여기서는 c1→c2로 본다).
    assert!(ping_from(&nft.c1, &[], "10.88.0.4"));
    assert!(ping_from(&ovn.c1, &[], "10.88.0.4"));
    let _servers: Vec<Server> = [&ovn.vm, &nft.vm]
        .iter()
        .map(|ns| Server(py_in(ns, ECHO_SERVER, &[]).spawn().unwrap()))
        .collect();
    std::thread::sleep(Duration::from_secs(1));
    apply_nft(&["sg-icmp", "sg-ssh"]);

    // 1. 같은 그룹 [icmp, ssh]: ICMP는 누구나, tcp 22는 c1만, 8080은 막힘, 나가기는 됨
    let got_ovn = ovn.probes();
    let got_nft = nft.probes();
    eprintln!("probes ovn {got_ovn:?}\nprobes nft {got_nft:?}");
    assert_eq!(got_ovn, ALLOWED);
    assert_eq!(got_nft, ALLOWED);

    // 2. 처음 맞는 것이 이긴다: [no22, icmp, ssh]면 c1의 22도 막히고, [icmp, ssh, no22]면 다시 열린다.
    d.ctl(&["vm", "nic-sg", "sg-vm", "0", "sg-no22", "sg-icmp", "sg-ssh"])
        .await
        .unwrap();
    apply_nft(&["sg-no22", "sg-icmp", "sg-ssh"]);
    wait_for(
        "the drop group first to close tcp 22",
        AGENT_WAIT,
        || async { !ovn.tcp22_c1() },
    )
    .await;
    assert!(!nft.tcp22_c1());
    let mut first_drop = ALLOWED.to_vec();
    first_drop[2].1 = false;
    assert_eq!(ovn.probes(), first_drop);
    assert_eq!(nft.probes(), first_drop);
    d.ctl(&["vm", "nic-sg", "sg-vm", "0", "sg-icmp", "sg-ssh", "sg-no22"])
        .await
        .unwrap();
    apply_nft(&["sg-icmp", "sg-ssh", "sg-no22"]);
    wait_for(
        "the drop group last to reopen tcp 22",
        AGENT_WAIT,
        || async { ovn.tcp22_c1() },
    )
    .await;
    assert_eq!(ovn.probes(), ALLOWED);
    assert_eq!(nft.probes(), ALLOWED);

    // 3. 그룹을 바꾸는 동안 열린 순간이 없다: c2가 20ms마다 22·8080에 붙어 보는 동안 목록을 네 번 바꾼다(포트가 새
    // Port_Group으로 옮겨 가고 옛 것은 지워진다).
    let probe = py_in(&ovn.c2, CONTINUOUS_PROBE, &[VM_IP, "14"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for (i, list) in [
        &["sg-ssh", "sg-icmp"][..],
        &["sg-icmp", "sg-ssh"],
        &["sg-ssh", "sg-icmp", "sg-no22"],
        &["sg-icmp", "sg-ssh"],
    ]
    .iter()
    .enumerate()
    {
        let mut args = vec!["vm", "nic-sg", "sg-vm", "0"];
        args.extend_from_slice(list);
        d.ctl(&args).await.unwrap();
        let want = list.join(",");
        wait_for(
            &format!("group change {i} in the NB"),
            AGENT_WAIT,
            || async {
                let rows = nb_rows(d, tag).await;
                rows.port_groups.len() == 1
                    && rows.port_groups[0].external_ids["orbivisor:sg-list"] == want
                    && !rows.port_groups[0].ports.is_empty()
            },
        )
        .await;
    }
    let out = probe.wait_with_output().unwrap();
    let counts = String::from_utf8_lossy(&out.stdout).to_string();
    eprintln!("continuous probe from c2 (hits tries): {counts}");
    let mut it = counts.split_whitespace();
    let hits: u32 = it.next().unwrap().parse().unwrap();
    let tries: u32 = it.next().unwrap().parse().unwrap();
    assert!(tries > 100, "{counts}");
    assert_eq!(hits, 0, "c2 got through while the groups changed");
    assert_eq!(ovn.probes(), ALLOWED);

    // 4. 허용을 빼면(ssh 그룹 떼기) OVN은 맺은 연결도 끊는다(ct_mark.blocked). nftables는 conntrack 항목이 끝날 때까지
    // 둔다(설계 §11.4의 차이 그대로).
    let mut held_ovn = ovn.hold();
    let mut held_nft = nft.hold();
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        held_ovn.0.try_wait().unwrap().is_none(),
        "the OVN connection is up"
    );
    assert!(
        held_nft.0.try_wait().unwrap().is_none(),
        "the nft connection is up"
    );
    d.ctl(&["vm", "nic-sg", "sg-vm", "0", "sg-icmp"])
        .await
        .unwrap();
    apply_nft(&["sg-icmp"]);
    let cut = cut_within(&mut held_ovn, Duration::from_secs(20));
    eprintln!("OVN held connection after removing sg-ssh: {cut:?}");
    assert_eq!(
        cut.and_then(|s| s.code()),
        Some(3),
        "the OVN connection was not cut"
    );
    assert!(
        cut_within(&mut held_nft, Duration::from_secs(4)).is_none(),
        "nftables keeps established connections"
    );
    assert!(!ovn.tcp22_c1());
    assert!(!nft.tcp22_c1());
    drop(held_nft);

    // 5. 그룹을 모두 떼면 Port_Group이 사라지고 포트는 거르지 않는다(단순 NIC처럼).
    d.ctl(&["vm", "nic-sg", "sg-vm", "0"]).await.unwrap();
    wait_for("the port group to go", AGENT_WAIT, || async {
        let rows = nb_rows(d, tag).await;
        rows.port_groups.is_empty() && rows.acls.is_empty()
    })
    .await;
    assert!(tcp_from(&ovn.c2, VM_IP, 8080));

    // 치우기
    let _ = try_run("nft", &["delete", "table", "bridge", "opl_sgcmp"]);
    for tap in taps {
        let _ = try_run("ip", &["link", "del", tap]);
    }
    let _ = try_run("ip", &["link", "del", "oplsgbr"]);
    for dev in ["oplsgv0", "oplsg10", "oplsg20"] {
        let _ = try_run("ovs-vsctl", &["--if-exists", "del-port", "br-int", dev]);
    }
    for ns in [&ovn.vm, &ovn.c1, &ovn.c2, &nft.vm, &nft.c1, &nft.c2] {
        let _ = try_run("ip", &["netns", "del", ns]);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 계획 67 스테이지 2: 부하 분산·건강 검사 넘김·바깥 VIP·QoS(OVN `QoS` 행과 단순 NIC tc)
// ---------------------------------------------------------------------------------------------------------------

/// 붙으면 제 이름과 상대 주소 한 줄을 보내고 끊는 TCP 서버(`NAME PORT`)
const NAME_SERVER: &str = r#"
import socket, sys
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", int(sys.argv[2]))); s.listen(64)
while True:
    c, peer = s.accept()
    try: c.sendall(("%s %s\n" % (sys.argv[1], peer[0])).encode())
    except OSError: pass
    c.close()
"#;

/// 붙어 한 줄을 받는다: 받은 줄, 또는 `refused`·`timeout`·`error`
const HIT: &str = r#"
import socket, sys
try:
    s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=2)
    s.settimeout(2); print(s.recv(200).decode().strip())
except ConnectionRefusedError: print("refused")
except socket.timeout: print("timeout")
except OSError as e: print("error", e)
"#;

fn hit(ns: &str, to: &str, port: u16) -> String {
    let out = py_in(ns, HIT, &[to, &port.to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// 이름공간 `ns`의 이름 서버(포트 `port`)
fn name_server(ns: &str, name: &str, port: u16) -> Server {
    Server(
        py_in(ns, NAME_SERVER, &[name, &port.to_string()])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    )
}

/// 이름공간 "VM"에 고정 주소와 기본 경로, 오버레이 MTU
fn static_ip(ns: &str, dev: &str, ip: &str, gw: &str) {
    let in_ns = |args: &[&str]| {
        let mut all = vec!["netns", "exec", ns, "ip"];
        all.extend_from_slice(args);
        run("ip", &all)
    };
    in_ns(&["link", "set", dev, "mtu", "1442"]);
    in_ns(&["addr", "add", &format!("{ip}/24"), "dev", dev]);
    in_ns(&["route", "add", "default", "via", gw]);
}

/// SB `Service_Monitor`의 (주소 → 상태)(상태가 비면 빈 글)
fn service_monitors() -> std::collections::BTreeMap<String, String> {
    let out = run(
        "ovn-sbctl",
        &[
            "--db=unix:/run/opl/ovn/sb.sock",
            "--format=json",
            "--columns=ip,port,status",
            "list",
            "Service_Monitor",
        ],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let status = r[2].as_str().map(String::from).unwrap_or_default();
            (r[0].as_str().unwrap_or_default().to_string(), status)
        })
        .collect()
}

/// iperf3 클라이언트(이름공간 `ns` → `to`, `reverse`면 받기) — 받은 쪽 Mbit/s. 폴리싱의 버스트(기본 속도의 0.8초분)가
/// 처음 몇 초를 부풀리므로 앞 3초를 빼고 6초를 잰다(`-O 3`).
fn iperf(ns: &str, to: &str, reverse: bool) -> f64 {
    let mut args = vec![
        "netns", "exec", ns, "iperf3", "-c", to, "-t", "6", "-O", "3", "-J",
    ];
    if reverse {
        args.push("-R");
    }
    let (ok, out, err) = try_run("ip", &args);
    assert!(ok, "iperf3 {ns} -> {to}: {out}{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    v["end"]["sum_received"]["bits_per_second"]
        .as_f64()
        .unwrap()
        / 1e6
}

fn near(measured: f64, want: f64) -> bool {
    (measured - want).abs() <= want * 0.15
}

/// 계획 67 스테이지 2·5: 오버레이 `lb-t1`(멤버 b1·b2, 같은 서브넷 클라이언트 c0)·`lb-t2`(클라이언트 c)를 잇는 라우터 `lr1`
/// (바깥 `ext`)에 LB `web`(VIP 10.40.0.50, 바깥 VIP 203.0.113.130, tcp/80 → 8080, 건강 검사)을 orbivisorctl `lb`로 만들고
/// 실제 트래픽과 API 보기의 멤버 건강(SB `Service_Monitor`)을 본다. 이어서 SDN NIC QoS(`vm nic-qos` → OVN `QoS` 행 —
/// iperf3·DSCP)와 단순 NIC tc(드라이버 `set_tap_qos`)를 잰다.
async fn services_section(d: &Daemon, tag: &str) {
    sdn_network(d, "lb-t1", &["--subnet", "10.40.0.0/24"]).await;
    sdn_network(d, "lb-t2", &["--subnet", "10.41.0.0/24"]).await;
    let b1 = api_vm(d, "lb-b1", "lb-t1").await;
    let b2 = api_vm(d, "lb-b2", "lb-t1").await;
    let c0 = api_vm(d, "lb-c0", "lb-t1").await;
    let c = api_vm(d, "lb-c", "lb-t2").await;
    d.ctl(&[
        "router",
        "create",
        "lr1",
        "--external",
        "ext,ip=203.0.113.60",
        "--interface",
        "lb-t1",
        "--interface",
        "lb-t2",
    ])
    .await
    .unwrap();
    let worlds = [
        ("oplb1", "oplb1d", &b1, "10.40.0.1"),
        ("oplb2", "oplb2d", &b2, "10.40.0.1"),
        ("oplc0", "oplc0d", &c0, "10.40.0.1"),
        ("oplc", "oplcd", &c, "10.41.0.1"),
    ];
    for (ns, dev, vm, gw) in worlds {
        namespace_port(ns, dev, &vm.3, &vm.1);
        static_ip(ns, dev, &vm.2, gw);
    }
    wait_for("a ping across lr1", AGENT_WAIT, || async {
        ping_from("oplc", &[], &b1.2)
    })
    .await;
    let mut s1 = Some(name_server("oplb1", "lb-b1", 8080));
    let mut s2 = Some(name_server("oplb2", "lb-b2", 8080));

    // LB(orbivisorctl — 스테이지 5 API): VIP·바깥 VIP를 주고, 멤버와 건강 검사를 더한다(검사 주소는 API가 고른다).
    d.ctl(&[
        "lb",
        "create",
        "web",
        "--network",
        "lb-t1",
        "--vip",
        "10.40.0.50",
        "--external",
        "ext,ip=203.0.113.130",
        "--listener",
        "tcp/80",
    ])
    .await
    .unwrap();
    for m in ["lb-b1@8080", "lb-b2@8080"] {
        d.ctl(&["lb", "member", "add", "web", "tcp/80", m])
            .await
            .unwrap();
    }
    d.ctl(&[
        "lb",
        "health",
        "set",
        "web",
        "tcp/80",
        "--interval",
        "2",
        "--timeout",
        "1",
        "--rise",
        "1",
        "--fall",
        "2",
    ])
    .await
    .unwrap();
    let lb = d.ctl_json(&["lb", "get", "web"]).await;
    eprintln!("lb web: {lb}");
    assert_eq!(lb["spec"]["monitors"][0]["network"], "lb-t1", "{lb}");
    wait_for("the load balancer rows", AGENT_WAIT, || async {
        let rows = nb_rows(d, tag).await;
        !rows.load_balancers.is_empty() && rows.routers.iter().any(|r| !r.load_balancers.is_empty())
    })
    .await;
    let rows = nb_rows(d, tag).await;
    eprintln!(
        "load balancers: {:?}\nhealth checks: {:?}",
        rows.load_balancers, rows.health_checks
    );
    show("ovn-nbctl", &["--db=unix:/run/opl/ovn/nb.sock", "lb-list"]);

    // 다른 서브넷의 클라이언트(분산 DNAT): 두 멤버가 모두 답한다.
    wait_for("the VIP to answer from lb-t2", AGENT_WAIT, || async {
        hit("oplc", "10.40.0.50", 80).starts_with("lb-b")
    })
    .await;
    let seen = |ns: &str, to: &str, n: usize| -> std::collections::BTreeMap<String, usize> {
        let mut m = std::collections::BTreeMap::new();
        for _ in 0..n {
            let got = hit(ns, to, 80);
            *m.entry(got.split(' ').next().unwrap_or_default().to_string())
                .or_insert(0) += 1;
        }
        m
    };
    let spread = seen("oplc", "10.40.0.50", 30);
    eprintln!("PROBE LB spread from another subnet: {spread:?}");
    assert!(
        spread.contains_key("lb-b1") && spread.contains_key("lb-b2"),
        "{spread:?}"
    );
    // 같은 서브넷 클라이언트(§7.2 탐침): 라우터가 VIP의 ARP에 답하는지, 라우터를 지나 되돌아오는지
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut same = String::new();
    while Instant::now() < deadline {
        same = hit("oplc0", "10.40.0.50", 80);
        if same.starts_with("lb-b") {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    eprintln!("PROBE same-subnet client -> VIP: {same:?}");
    show("ip", &["netns", "exec", "oplc0", "ip", "neigh"]);

    // 건강 검사: 둘 다 online → b1 듣기 멈춤 → offline까지의 시간과 그동안의 성공률 → b2만 → 되살림 → 둘
    wait_for("both members online", AGENT_WAIT, || async {
        let m = service_monitors();
        eprintln!("service monitors: {m:?}");
        m.get(&b1.2).map(String::as_str) == Some("online")
            && m.get(&b2.2).map(String::as_str) == Some("online")
    })
    .await;
    // API 보기의 멤버 건강(SB `Service_Monitor`를 역할 포트로 읽음)
    wait_for(
        "the view to show both members online",
        AGENT_WAIT,
        || async {
            let lb = d.ctl_json(&["lb", "get", "web"]).await;
            lb["members"]
                .as_array()
                .is_some_and(|m| m.len() == 2 && m.iter().all(|m| m["health"] == "online"))
        },
    )
    .await;
    println!("{}", d.ctl(&["lb", "get", "web"]).await.unwrap());
    s1.take();
    let t0 = Instant::now();
    let (mut tries, mut ok) = (0, 0);
    while service_monitors().get(&b1.2).map(String::as_str) != Some("offline") {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "b1 did not go offline"
        );
        tries += 1;
        if hit("oplc", "10.40.0.50", 80).starts_with("lb-b") {
            ok += 1;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    eprintln!(
        "PROBE health check failover: b1 offline after {:.1}s; {ok}/{tries} connections succeeded meanwhile",
        t0.elapsed().as_secs_f64()
    );
    let only = seen("oplc", "10.40.0.50", 10);
    assert_eq!(only.keys().collect::<Vec<_>>(), ["lb-b2"], "{only:?}");
    let lb = d.ctl_json(&["lb", "get", "web"]).await;
    let health = |vm: &str| {
        lb["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["vm"] == vm)
            .map(|m| m["health"].clone())
    };
    assert_eq!(health(&b1.0), Some(json!("offline")), "{lb}");
    s1 = Some(name_server("oplb1", "lb-b1", 8080));
    let t0 = Instant::now();
    wait_for("b1 back online", AGENT_WAIT, || async {
        service_monitors().get(&b1.2).map(String::as_str) == Some("online")
    })
    .await;
    eprintln!(
        "PROBE health check recovery: b1 online after {:.1}s",
        t0.elapsed().as_secs_f64()
    );
    wait_for("both members answer again", AGENT_WAIT, || async {
        seen("oplc", "10.40.0.50", 20).len() == 2
    })
    .await;
    // 둘 다 죽으면 reject: 조용히 버리지 않고 곧바로 거절(RST)
    s1.take();
    s2.take();
    wait_for("both members offline", AGENT_WAIT, || async {
        let m = service_monitors();
        m.get(&b1.2).map(String::as_str) == Some("offline")
            && m.get(&b2.2).map(String::as_str) == Some("offline")
    })
    .await;
    let dead = hit("oplc", "10.40.0.50", 80);
    eprintln!("PROBE no live member: {dead:?}");
    assert_eq!(dead, "refused");
    s1 = Some(name_server("oplb1", "lb-b1", 8080));
    s2 = Some(name_server("oplb2", "lb-b2", 8080));
    wait_for("both members online again", AGENT_WAIT, || async {
        let m = service_monitors();
        m.get(&b1.2).map(String::as_str) == Some("online")
            && m.get(&b2.2).map(String::as_str) == Some("online")
    })
    .await;

    // 바깥 VIP: 물리 망 `ext`에서 들어오고 멤버가 본 출발 주소가 `ext` 주소다(SNAT 없음).
    wait_for("the external VIP to answer", AGENT_WAIT, || async {
        hit("ext", "203.0.113.130", 80).starts_with("lb-b")
    })
    .await;
    let outside = hit("ext", "203.0.113.130", 80);
    eprintln!("PROBE external VIP: {outside:?}");
    assert!(outside.ends_with(" 203.0.113.1"), "{outside}");

    // 출발지 고정: 같은 클라이언트는 같은 멤버(API `PATCH`)
    client(d)
        .patch("/load-balancers/web", &json!({ "affinity_secs": 60 }))
        .await
        .unwrap();
    wait_for("affinity in the NB", AGENT_WAIT, || async {
        nb_rows(d, tag)
            .await
            .load_balancers
            .iter()
            .any(|l| l.options.get("affinity_timeout").map(String::as_str) == Some("60"))
    })
    .await;
    let sticky = seen("oplc", "10.40.0.50", 10);
    eprintln!("PROBE affinity: {sticky:?}");
    assert_eq!(sticky.len(), 1, "{sticky:?}");

    // 바깥 VIP가 쓰는 라우터 바깥은 떼지 못한다(409 `lb_in_use`) — 바깥 VIP를 먼저 뗀다.
    let err = d
        .ctl(&["router", "external", "clear", "lr1"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("load balancer"), "{err}");
    d.ctl(&["lb", "external", "clear", "web"]).await.unwrap();
    // DGP 없는 라우터(§7.2 탐침): 바깥을 떼어도 안쪽 VIP가 닿는지
    d.ctl(&["router", "external", "clear", "lr1"])
        .await
        .unwrap();
    wait_for("lr1 without its gateway port", AGENT_WAIT, || async {
        !nb_rows(d, tag)
            .await
            .router_ports
            .iter()
            .any(|p| p.name.starts_with("opl-gw-"))
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!(
        "PROBE VIP without a gateway port: other subnet {:?}, same subnet {:?}",
        hit("oplc", "10.40.0.50", 80),
        hit("oplc0", "10.40.0.50", 80)
    );

    // VIP 네트워크는 라우터에서 떼지 못한다(409 `lb_in_use`).
    let err = d
        .ctl(&["router", "interface", "remove", "lr1", "lb-t1"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("load balancer"), "{err}");
    // LB를 지우면 행이 사라진다.
    d.ctl(&["lb", "delete", "web"]).await.unwrap();
    wait_for("the load balancer rows to go", AGENT_WAIT, || async {
        let rows = nb_rows(d, tag).await;
        rows.load_balancers.is_empty() && rows.health_checks.is_empty()
    })
    .await;
    drop((s1, s2));

    // SDN NIC QoS(OVN `QoS` 행 — 미터 폴리싱): b1이 보내는 속도·받는 속도, 실행 중 바꾸기, DSCP
    let iperf_server = Server(
        Command::new("ip")
            .args(["netns", "exec", "oplc", "iperf3", "-s"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let free = iperf("oplb1", &c.2, false);
    eprintln!("PROBE iperf3 without QoS: {free:.1} Mbit/s");
    // `vm nic-qos`(지금 값에 합친다 — `--clear`로 비운 뒤 다시 줄 수 있다)
    let set_qos = |args: &'static [&'static str]| async move {
        let mut argv = vec!["vm", "nic-qos", "lb-b1", "0"];
        argv.extend_from_slice(args);
        d.ctl(&argv).await.unwrap();
    };
    let qos_rows = || async { nb_rows(d, tag).await.qos };
    set_qos(&["out=50"]).await;
    wait_for("the QoS row", AGENT_WAIT, || async {
        !qos_rows().await.is_empty()
    })
    .await;
    eprintln!("qos rows: {:?}", qos_rows().await);
    show(
        "ovn-nbctl",
        &["--db=unix:/run/opl/ovn/nb.sock", "list", "QoS"],
    );
    show("ovs-ofctl", &["-O", "OpenFlow15", "dump-meters", "br-int"]);
    let out50 = iperf("oplb1", &c.2, false);
    eprintln!("PROBE SDN QoS out_mbps=50: {out50:.1} Mbit/s");
    assert!(near(out50, 50.0), "out_mbps=50 measured {out50:.1}");
    set_qos(&["in=30"]).await;
    wait_for("two QoS rows", AGENT_WAIT, || async {
        qos_rows().await.len() == 2
    })
    .await;
    let in30 = iperf("oplb1", &c.2, true);
    eprintln!("PROBE SDN QoS in_mbps=30: {in30:.1} Mbit/s");
    assert!(near(in30, 30.0), "in_mbps=30 measured {in30:.1}");
    set_qos(&["out=20"]).await;
    wait_for("the changed QoS row", AGENT_WAIT, || async {
        qos_rows()
            .await
            .iter()
            .any(|q| q.row.bandwidth.get("rate").map(String::as_str) == Some("20000"))
    })
    .await;
    let out20 = iperf("oplb1", &c.2, false);
    eprintln!("PROBE SDN QoS changed to out_mbps=20: {out20:.1} Mbit/s");
    assert!(near(out20, 20.0), "out_mbps=20 measured {out20:.1}");
    set_qos(&["--clear"]).await;
    set_qos(&["dscp=46"]).await;
    wait_for("the DSCP row", AGENT_WAIT, || async {
        qos_rows()
            .await
            .iter()
            .any(|q| q.row.action.get("dscp").map(String::as_str) == Some("46"))
    })
    .await;
    let marked = captured(
        "oplc",
        "oplcd",
        &format!("src host {} and ip[1] & 0xfc == 184", b1.2),
        || {
            ping_from("oplb1", &[], &c.2);
        },
    );
    eprintln!("PROBE DSCP 46 seen by the receiver: {marked:?}");
    assert_ne!(marked, Some(false), "the receiver did not see DSCP 46");
    set_qos(&["--clear"]).await;
    wait_for("the QoS rows to go", AGENT_WAIT, || async {
        qos_rows().await.is_empty()
    })
    .await;
    drop(iperf_server);

    Box::pin(plain_tc_section()).await;

    // 치우기
    for (ns, dev, _, _) in worlds {
        let _ = try_run("ovs-vsctl", &["--if-exists", "del-port", "br-int", dev]);
        let _ = try_run("ip", &["netns", "del", ns]);
    }
    let api = client(d);
    for net in ["lb-t1", "lb-t2"] {
        d.ctl(&["router", "interface", "remove", "lr1", net])
            .await
            .unwrap();
    }
    d.ctl(&["router", "delete", "lr1"]).await.unwrap();
    for vm in [&b1.0, &b2.0, &c0.0, &c.0] {
        api.delete(&format!("/vms/{vm}")).await.unwrap();
    }
    for name in ["lb-t1", "lb-t2"] {
        d.ctl(&["network", "delete", name]).await.unwrap();
    }
}

/// `tc filter show dev X HOOK`의 우리 police 행동 번호(`police 0x…`) — 다시 넣으면 바뀐다.
fn police_index(dev: &str, hook: &str) -> Option<String> {
    let out = run("tc", &["filter", "show", "dev", dev, hook]);
    let words: Vec<&str> = out.split_whitespace().collect();
    words
        .iter()
        .position(|w| *w == "police")
        .and_then(|i| words.get(i + 1))
        .map(|s| s.to_string())
}

/// 단순 NIC tc(계획 67 §8.2): Linux 브리지에 veth 둘(이름공간 둘 — 한쪽이 tap 자리)을 잇고 드라이버의 `set_tap_qos`로
/// 보내기·받기 속도를 걸어 iperf3로 잰다. 같은 값이면 tc를 건드리지 않고(police 번호 그대로), 바꾸면 지우고 다시 넣고,
/// 빼면 `clsact`까지 사라지며 남의 필터가 있으면 `clsact`와 그 필터를 둔다.
async fn plain_tc_section() {
    use orbivisor_driver::HostNet;
    use orbivisor_types::qos::NicQos;
    let net = orbivisor_driver::IpRoute::default();
    assert!(net.probe_qos().await, "tc clsact is not available");
    let _ = try_run("ip", &["link", "del", "oplqbr"]);
    run("ip", &["link", "add", "oplqbr", "type", "bridge"]);
    run("ip", &["link", "set", "oplqbr", "up"]);
    for (i, ns) in ["oplq1", "oplq2"].iter().enumerate() {
        let _ = try_run("ip", &["netns", "del", ns]);
        run("ip", &["netns", "add", ns]);
        let (host, inner) = (format!("{ns}h"), format!("{ns}n"));
        run(
            "ip",
            &["link", "add", &host, "type", "veth", "peer", "name", &inner],
        );
        run("ip", &["link", "set", &host, "master", "oplqbr", "up"]);
        run("ip", &["link", "set", &inner, "netns", ns]);
        let in_ns = |args: &[&str]| {
            let mut all = vec!["netns", "exec", ns, "ip"];
            all.extend_from_slice(args);
            run("ip", &all)
        };
        in_ns(&[
            "addr",
            "add",
            &format!("192.168.77.{}/24", i + 1),
            "dev",
            &inner,
        ]);
        in_ns(&["link", "set", &inner, "up"]);
        in_ns(&["link", "set", "lo", "up"]);
    }
    let server = Server(
        Command::new("ip")
            .args(["netns", "exec", "oplq2", "iperf3", "-s"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let tap = "oplq1h";
    let qos = |out: Option<u32>, inn: Option<u32>| NicQos {
        out_mbps: out,
        in_mbps: inn,
        ..Default::default()
    };
    net.set_tap_qos(tap, Some(&qos(Some(40), Some(25))))
        .await
        .unwrap();
    show("tc", &["qdisc", "show", "dev", tap]);
    show("tc", &["filter", "show", "dev", tap, "ingress"]);
    show("tc", &["filter", "show", "dev", tap, "egress"]);
    let out40 = iperf("oplq1", "192.168.77.2", false);
    let in25 = iperf("oplq1", "192.168.77.2", true);
    eprintln!("PROBE tc out_mbps=40: {out40:.1} Mbit/s, in_mbps=25: {in25:.1} Mbit/s");
    assert!(near(out40, 40.0), "tc out 40 measured {out40:.1}");
    assert!(near(in25, 25.0), "tc in 25 measured {in25:.1}");
    // 같은 값이면 아무것도 하지 않는다(되읽은 속도·버스트가 같음).
    let (i0, e0) = (police_index(tap, "ingress"), police_index(tap, "egress"));
    net.set_tap_qos(tap, Some(&qos(Some(40), Some(25))))
        .await
        .unwrap();
    assert_eq!(
        (police_index(tap, "ingress"), police_index(tap, "egress")),
        (i0.clone(), e0.clone())
    );
    // 보내기만 바꾸면 그 필터만 지우고 다시 넣는다(matchall은 제자리 바꾸기를 받지 않는다).
    net.set_tap_qos(tap, Some(&qos(Some(15), Some(25))))
        .await
        .unwrap();
    assert_ne!(police_index(tap, "ingress"), i0);
    assert_eq!(police_index(tap, "egress"), e0);
    let out15 = iperf("oplq1", "192.168.77.2", false);
    eprintln!("PROBE tc changed to out_mbps=15: {out15:.1} Mbit/s");
    assert!(near(out15, 15.0), "tc out 15 measured {out15:.1}");
    // 받기만 빼면 egress 필터만 사라진다.
    net.set_tap_qos(tap, Some(&qos(Some(15), None)))
        .await
        .unwrap();
    assert_eq!(police_index(tap, "egress"), None);
    assert!(police_index(tap, "ingress").is_some());
    // 남의 필터가 있으면 다 빼도 clsact와 그 필터는 남는다.
    run(
        "tc",
        &[
            "filter", "add", "dev", tap, "egress", "pref", "10", "matchall", "action", "pass",
        ],
    );
    net.set_tap_qos(tap, None).await.unwrap();
    assert!(run("tc", &["qdisc", "show", "dev", tap]).contains("clsact"));
    assert!(run("tc", &["filter", "show", "dev", tap, "egress"]).contains("pref 10"));
    assert_eq!(police_index(tap, "ingress"), None);
    run("tc", &["filter", "del", "dev", tap, "egress", "pref", "10"]);
    net.set_tap_qos(tap, None).await.unwrap();
    assert!(!run("tc", &["qdisc", "show", "dev", tap]).contains("clsact"));
    drop(server);
    for ns in ["oplq1", "oplq2"] {
        let _ = try_run("ip", &["netns", "del", ns]);
    }
    let _ = try_run("ip", &["link", "del", "oplqbr"]);
}

/// orbivisorctl 옮기기 명령 하나(Task를 기다린다)와 걸린 시간
async fn migration_ctl(d: &Daemon, args: &[&str]) -> String {
    let started = Instant::now();
    let out = d.ctl(args).await.unwrap();
    eprintln!(
        "{}: {:.1}s\n{out}",
        args.join(" "),
        started.elapsed().as_secs_f64()
    );
    out
}

/// 이름공간 `ns`에 Linux 브리지 `bridge`의 veth 한쪽(MAC `mac`, 주소 `ip`/24)
fn bridge_namespace(ns: &str, bridge: &str, mac: &str, ip: &str) {
    let _ = try_run("ip", &["netns", "del", ns]);
    run("ip", &["netns", "add", ns]);
    let (host, inner) = (format!("{ns}h"), format!("{ns}n"));
    let _ = try_run("ip", &["link", "del", &host]);
    run(
        "ip",
        &["link", "add", &host, "type", "veth", "peer", "name", &inner],
    );
    run("ip", &["link", "set", &host, "master", bridge, "up"]);
    run("ip", &["link", "set", &inner, "netns", ns]);
    let in_ns = |args: &[&str]| {
        let mut all = vec!["netns", "exec", ns, "ip"];
        all.extend_from_slice(args);
        run("ip", &all)
    };
    in_ns(&["link", "set", &inner, "address", mac]);
    in_ns(&["addr", "add", &format!("{ip}/24"), "dev", &inner]);
    in_ns(&["link", "set", "lo", "up"]);
    in_ns(&["link", "set", &inner, "up"]);
}

/// 계획 67 스테이지 4: Linux 브리지 `oplmig0`의 단순 네트워크 `lan`(이름공간 둘과 디스크 없는 QEMU VM 하나)을 같은 CIDR의
/// SDN 오버레이 `t-lan`으로 옮긴다. 미리 보기 계획 → 시작(다리: NB `l2gateway` 행, 노드 매핑 `mig…`, veth) → QEMU VM은
/// `live`(tap이 `br-int`로, `ovn-installed`, QEMU pid 그대로), 이름공간 VM `mig-a`는 `offline`(스펙만 — cloud-init 주소를
/// 그대로 가져가고, 시험이 그 이름공간을 OVS 포트로 다시 붙여 "다시 켬"을 흉내) → 다리 너머 ping(옮긴 쪽 ↔ 남은 `mig-b`) →
/// QEMU VM 되돌리기(tap이 `oplmig0`로) → 끝내기(다리가 모두 걷힘).
async fn migration_section(d: &Daemon, tag: &str, node: &str) {
    let c = client(d);
    let _ = try_run("ip", &["link", "del", "oplmig0"]);
    run("ip", &["link", "add", "oplmig0", "type", "bridge"]);
    run("ip", &["link", "set", "oplmig0", "up"]);
    c.post(
        "/networks",
        &json!({ "name": "lan", "spec": { "bridge": "oplmig0" } }),
    )
    .await
    .unwrap();
    sdn_network(d, "t-lan", &["--subnet", "10.66.0.0/24"]).await;
    wait_for(
        "the node to report bridge oplmig0",
        Duration::from_secs(120),
        || async {
            let me = d.ctl_json(&["node", "get", "node1"]).await;
            me["status"]["bridges"]
                .as_array()
                .is_some_and(|b| b.iter().any(|b| b["name"] == "oplmig0"))
        },
    )
    .await;
    // 꺼진 "VM" 둘(이름공간이 대신 돈다): a는 cloud-init 고정 주소, b는 옮기지 않고 남는다.
    let mut made = Vec::new();
    for (name, ip) in [("mig-a", "10.66.0.11"), ("mig-b", "10.66.0.12")] {
        let vm = c
            .post(
                "/vms",
                &json!({ "name": name, "spec": {
                    "cpu": { "cores": 1 }, "memory_mib": 256,
                    "nics": [{ "network": "lan", "model": "virtio" }],
                    "cloud_init": { "networks": [{ "ipv4": { "mode": "static", "address": format!("{ip}/24"), "gateway": "10.66.0.1" } }] }
                } }),
            )
            .await
            .unwrap();
        let mac = vm["spec"]["nics"][0]["mac"].as_str().unwrap().to_string();
        let id = vm["id"].as_str().unwrap().to_string();
        made.push((id, mac, ip));
    }
    let (a_id, a_mac, _) = made[0].clone();
    let (_, b_mac, _) = made[1].clone();
    bridge_namespace("oplmiga", "oplmig0", &a_mac, "10.66.0.11");
    bridge_namespace("oplmigb", "oplmig0", &b_mac, "10.66.0.12");
    assert!(
        ping_from("oplmiga", &[], "10.66.0.12"),
        "the source L2 works"
    );
    // 켠 QEMU VM(게스트 없음)
    d.ctl(&[
        "vm", "create", "mig-q", "--nic", "lan", "--memory", "256", "--start",
    ])
    .await
    .unwrap();
    let q = d.ctl_json(&["vm", "get", "mig-q"]).await;
    let q_id: uuid::Uuid = q["id"].as_str().unwrap().parse().unwrap();
    let q_mac: orbivisor_types::vm::MacAddr = q["spec"]["nics"][0]["mac"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let pid = q["status"]["pid"].as_u64().unwrap();
    let tap = orbivisor_types::network::tap_name(q_id, 0);
    let master = |dev: &str| -> String {
        serde_json::from_str::<Value>(&run("ip", &["-j", "link", "show", "dev", dev])).unwrap()[0]
            ["master"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(master(&tap), "oplmig0");

    // 미리 보기(`network migrate … --dry-run`)
    let migrate = [
        "network",
        "migrate",
        "lan",
        "--to",
        "t-lan",
        "--name",
        "mig",
        "--vm",
        "mig-q",
        "--vm",
        "mig-a",
        "--allow-mtu-shrink",
        "--ip",
        "mig-q=10.66.0.10",
    ];
    let mut dry = vec!["-o", "json"];
    dry.extend_from_slice(&migrate);
    dry.push("--dry-run");
    let plan: Value = serde_json::from_str(&d.ctl(&dry).await.unwrap()).unwrap();
    let mut table = migrate.to_vec();
    table.push("--dry-run");
    println!("{}", d.ctl(&table).await.unwrap());
    eprintln!("migration plan: {plan}");
    let planned = |vm: &str| {
        plan["nics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["vm"] == vm)
            .unwrap()
            .clone()
    };
    assert_eq!(planned(&q_id.to_string())["mode"], "live", "{plan}");
    assert_eq!(
        planned(&q_id.to_string())["address_source"],
        "given",
        "{plan}"
    );
    assert_eq!(planned(&a_id)["mode"], "offline", "{plan}");
    assert_eq!(planned(&a_id)["address_source"], "cloud-init", "{plan}");
    assert_eq!(planned(&a_id)["ips"], json!(["10.66.0.11"]), "{plan}");
    assert_eq!(plan["bridge"]["needed"], true, "{plan}");
    assert_eq!(plan["bridge"]["node"], node, "{plan}");
    // 만들고 시작(Task를 기다린다)
    migration_ctl(d, &migrate).await;
    let mig = d.ctl_json(&["network", "migration", "get", "mig"]).await;
    let mig_id: uuid::Uuid = mig["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(mig["status"]["phase"], "moved", "{mig}");

    // 다리: NB l2gateway 행, 노드 매핑과 veth
    let physnet = orbivisor_types::sdn::migration_physnet(tag, mig_id);
    let (_, veth, ovs_bridge) = orbivisor_types::sdn::physnet_ifnames(tag, &physnet);
    let l2gw = format!("opl-l2gw-{mig_id}");
    wait_for("the l2gateway port", AGENT_WAIT, || async {
        nb_rows(d, tag).await.ports.iter().any(|p| p.name == l2gw)
    })
    .await;
    let rows = nb_rows(d, tag).await;
    let port = rows.ports.iter().find(|p| p.name == l2gw).unwrap();
    assert_eq!(port.kind, "l2gateway");
    assert_eq!(port.options["network_name"], physnet);
    assert_eq!(port.options["l2gateway-chassis"], node);
    assert!(
        ovs_id("ovn-bridge-mappings").contains(&format!("{physnet}:{ovs_bridge}")),
        "{}",
        ovs_id("ovn-bridge-mappings")
    );
    assert_eq!(master(&veth), "oplmig0");
    // 주소 옮기기: 준 주소·cloud-init 주소가 논리 포트 주소다.
    let q_port = orbivisor_types::sdn::port_name(q_id, q_mac);
    let a_port = format!("opl-{a_id}-{}", a_mac.replace(':', ""));
    for (p, ip) in [(&q_port, "10.66.0.10"), (&a_port, "10.66.0.11")] {
        let row = rows.ports.iter().find(|x| &x.name == p).unwrap();
        assert!(row.addresses.iter().any(|a| a.ends_with(ip)), "{row:?}");
    }
    let va = c.get(&format!("/vms/{a_id}")).await.unwrap();
    assert_eq!(va["spec"]["nics"][0]["network"], "t-lan", "{va}");
    assert_eq!(va["spec"]["nics"][0]["ips"], json!(["10.66.0.11"]), "{va}");
    assert_eq!(va["spec"]["cloud_init"]["networks"], json!([{}]), "{va}");
    // QEMU VM: tap만 br-int로(같은 QEMU), 포트가 켜짐
    assert_eq!(run("ovs-vsctl", &["port-to-br", &tap]).trim(), "br-int");
    assert_eq!(iface_id_field(&tap, "iface-id"), q_port);
    assert_eq!(iface_id_field(&tap, "ovn-installed"), "true");
    let q = d.ctl_json(&["vm", "get", "mig-q"]).await;
    assert_eq!(q["status"]["pid"].as_u64(), Some(pid), "{q}");
    assert_eq!(
        q["status"]["hardware"]["nics"][0]["network"], "t-lan",
        "{q}"
    );

    // "다시 켠" a: 이름공간을 OVS 포트로 다시 붙인다. 다리 너머 남은 b에 닿는다(같은 L2).
    namespace_port("oplmiga", "oplmiga0", &a_port, &a_mac);
    run(
        "ip",
        &[
            "netns",
            "exec",
            "oplmiga",
            "ip",
            "addr",
            "add",
            "10.66.0.11/24",
            "dev",
            "oplmiga0",
        ],
    );
    wait_for(
        "the moved namespace port to be installed",
        AGENT_WAIT,
        || async { iface_id_field("oplmiga0", "ovn-installed") == "true" },
    )
    .await;
    let started = Instant::now();
    wait_for("a ping across the migration bridge", AGENT_WAIT, || async {
        ping_from("oplmiga", &[], "10.66.0.12")
    })
    .await;
    eprintln!(
        "migration bridge: moved -> remaining ping after {:.1}s",
        started.elapsed().as_secs_f64()
    );
    assert!(
        ping_from("oplmigb", &[], "10.66.0.11"),
        "remaining -> moved"
    );

    // QEMU VM 하나를 되돌린다: tap이 원래 브리지로(같은 QEMU), 주소를 놓는다.
    migration_ctl(d, &["network", "migration", "rollback", "mig", "mig-q"]).await;
    assert_eq!(master(&tap), "oplmig0");
    assert!(
        !run("ovs-vsctl", &["list-ports", "br-int"])
            .lines()
            .any(|p| p == tap)
    );
    let q = d.ctl_json(&["vm", "get", "mig-q"]).await;
    assert_eq!(q["status"]["pid"].as_u64(), Some(pid), "{q}");
    assert_eq!(q["spec"]["nics"][0]["network"], "lan", "{q}");
    assert!(q["spec"]["nics"][0]["ips"].is_null(), "{q}");
    // 다리는 아직 있다(a가 옮겨져 있음).
    assert!(ping_from("oplmiga", &[], "10.66.0.12"));

    // 끝내기: 다리(NB 행·매핑·veth·OVS 브리지)가 걷힌다.
    migration_ctl(d, &["network", "migration", "finish", "mig"]).await;
    let mig = d.ctl_json(&["network", "migration", "get", "mig"]).await;
    assert_eq!(mig["status"]["phase"], "finished", "{mig}");
    wait_for("the migration bridge to go", AGENT_WAIT, || async {
        !nb_rows(d, tag).await.ports.iter().any(|p| p.name == l2gw)
            && !ovs_id("ovn-bridge-mappings").contains(&physnet)
            && !try_run("ip", &["link", "show", "dev", &veth]).0
            && !run("ovs-vsctl", &["list-br"])
                .lines()
                .any(|b| b == ovs_bridge)
    })
    .await;
    assert!(
        !ping_from("oplmiga", &[], "10.66.0.12"),
        "the bridge is gone"
    );

    // 치우기
    d.ctl(&["vm", "stop", "mig-q", "--force"]).await.unwrap();
    for vm in ["mig-q", "mig-a", "mig-b"] {
        d.ctl(&["vm", "delete", vm]).await.unwrap();
    }
    for ns in ["oplmiga", "oplmigb"] {
        let _ = try_run("ip", &["netns", "del", ns]);
    }
    let _ = try_run("ovs-vsctl", &["--if-exists", "del-port", "oplmiga0"]);
    let _ = try_run("ip", &["link", "del", "oplmigbh"]);
    let _ = try_run("ip", &["link", "del", "oplmig0"]);
}
