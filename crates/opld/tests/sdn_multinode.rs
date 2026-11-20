//! 여러 노드 SDN(OVN) 시험(계획 66 스테이지 6): 한 러너에 privileged systemd 컨테이너 셋(`ci/sdn-node/Dockerfile`)을 띄워
//! 노드마다 orbivisord를 돌리고, 모든 것을 orbivisorctl로 만든다. 컨테이너마다 자기 netns·systemd·OVS·ovn-controller가 돌아
//! 진짜 chassis 셋이 되고(커널 모듈만 호스트와 함께 쓴다), OVN은 orbivisord가 apt로 깐다.
//!
//! 보는 것: Orbivisor 클러스터 셋 → `sdn init`·`sdn central add` 둘 → **실제 OVN Raft 셋**(NB·SB `cluster/status` 서버 셋),
//! `sdn chassis install --all` → SB `Chassis` 셋과 컨테이너 사이 Geneve 터널. 노드마다 physnet `ext=vmbr1`(컨테이너 안 Linux
//! 브리지 — 호스트 브리지 `oplext0`에 veth로 이어짐, 호스트 이름공간 `ext` 203.0.113.1이 물리 라우터), 게이트웨이 c2(200)·
//! c3(100), 공급자 네트워크 `ext`, 오버레이 `t1`·`t2`, 라우터 r1. 이름공간 "VM"(꺼진 API VM의 포트) 둘로 **chassis 사이 동서
//! 라우팅**(c2의 Geneve에 지나지 않음), **SNAT**(c2의 veth로 나감). 게스트 없는 QEMU VM의 **분산 유동 IP**(c1의 veth로 듦,
//! `ext`의 이웃 MAC = NIC MAC), c1에서 physnet을 빼면 중앙(c2로 지남), 다시 넣으면 분산. 그 VM의 **두 chassis 라이브
//! 마이그레이션**(c1 → c3: NB `requested-chassis = "c1,c3"`·`activation-strategy=rarp`, 끝에 SB 바인딩 c3, 유동 IP가 c3의
//! veth로). **게이트웨이 넘김** 세 길(`systemctl stop ovn-controller`, `kill -9` ovs-vswitchd·ovn-controller, `docker kill` 노드
//! 통째)의 넘김 시간과 되돌아감, 노드를 잃은 뒤 Orbivisor Raft·OVN 정족수로 새 유동 IP가 닿는지, **죽은 중앙 내쫓기**(`sdn
//! central remove` → 살아 있는 중앙의 실제 `cluster/kick`, 서버 둘).
//!
//! `sudo -E env "PATH=$PATH" OPL_MULTINODE_IMAGE=opl-sdn-node cargo test -p opld --features sdn-multinode-it --test sdn_multinode -- --nocapture`
//! Linux(Ubuntu 24.04)에서 root, docker, 컨테이너 이미지(`docker build -t opl-sdn-node ci/sdn-node`), 커널 모듈 `openvswitch`·
//! `geneve`(호스트가 `modprobe`), tcpdump, 컨테이너가 apt에 닿는 네트워크가 필요하다. 호스트에 도커 망 `oplmgmt`
//! (172.30.66.0/24)·브리지 `oplext0`·이름공간 `ext`를 만들고 끝에 지운다(CI `sdn-multinode` 작업).
#![cfg(all(feature = "sdn-multinode-it", target_os = "linux"))]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opl_driver::ovn::{ClusterStatus, parse_cluster_status};
use serde_json::Value;

/// 도커 망(노드 주소·Raft·OVN·Geneve)
const MGMT_NET: &str = "oplmgmt";
const MGMT_SUBNET: &str = "172.30.66.0/24";
/// 노드 셋의 이름과 주소
const NODES: [(&str, &str); 3] = [
    ("c1", "172.30.66.11"),
    ("c2", "172.30.66.12"),
    ("c3", "172.30.66.13"),
];
/// 호스트 쪽 물리 망: Linux 브리지와 "물리 라우터" 이름공간
const EXT_BRIDGE: &str = "oplext0";
const EXT_NS: &str = "ext";
const EXT_IP: &str = "203.0.113.1";
const ROUTER_IP: &str = "203.0.113.50";
/// 에이전트·OvnSync가 맞추기를 기다리는 시간
const AGENT_WAIT: Duration = Duration::from_secs(90);
/// 게이트웨이 넘김의 상한(설계 §14 — BFD 기본값의 넘김). 탐침에서 잰 값은 3초 안팎이지만 바쁜 러너에서 노드를 통째로
/// 죽이면(Raft 투표 멤버·OVN 중앙도 함께) 더 걸릴 수 있어 넉넉히 둔다(리뷰 1 #5). 잰 값은 `PROBE` 줄로 남는다.
const FAILOVER_LIMIT: Duration = Duration::from_secs(60);
/// tcpdump 캡처 하나의 길이(초). tcpdump가 붙기를 기다린 뒤 `action`을 하므로 붙는 시간은 빼지 않는다.
const CAPTURE_SECS: &str = "12";
/// 캡처 확인을 다시 하는 횟수(바쁜 러너에서 tcpdump가 늦게 붙거나 첫 패킷을 놓쳐도 거짓 실패가 없게 — 리뷰 1 #5)
const CAPTURE_ATTEMPTS: usize = 3;

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
    assert!(ok, "{program} {args:?}: {stderr}{stdout}");
    stdout
}

/// 명령의 출력을 진단으로 stderr에 찍는다(실패해도 계속).
fn show(program: &str, args: &[&str]) {
    let (_, out, err) = try_run(program, args);
    eprintln!("$ {program} {}\n{out}{err}", args.join(" "));
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

/// 컨테이너 하나 = Orbivisor 노드 하나. 설정·데이터·제어 소켓·로그는 호스트 디렉터리 `dir`(컨테이너에도 같은 경로로
/// 바인드 마운트)에 있어 호스트의 oplctl이 제어 소켓에 바로 붙는다.
struct Node {
    name: &'static str,
    container: String,
    ip: &'static str,
    dir: PathBuf,
    admin_password: Option<String>,
    ca_fingerprint: Option<String>,
    join_url: Option<String>,
    /// 노드 UUID(= OVN chassis 이름)
    id: String,
}

impl Node {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn server(&self) -> String {
        format!("https://{}:8006", self.ip)
    }

    /// 컨테이너 안에서 명령을 실행해 (성공, stdout, stderr)
    fn try_exec(&self, args: &[&str]) -> (bool, String, String) {
        let mut all = vec!["exec", self.container.as_str()];
        all.extend_from_slice(args);
        try_run("docker", &all)
    }

    /// 컨테이너 안에서 명령을 실행하고 성공해야 하며 표준 출력을 돌려준다.
    fn exec(&self, args: &[&str]) -> String {
        let (ok, out, err) = self.try_exec(args);
        assert!(ok, "{} {args:?}: {err}{out}", self.container);
        out
    }

    /// `docker exec` 접두사(캡처·백그라운드 명령용)
    fn prefix(&self) -> Vec<String> {
        vec!["docker".into(), "exec".into(), self.container.clone()]
    }

    /// 컨테이너를 띄우고 systemd가 부팅을 마칠 때까지 기다린다.
    async fn boot(&self, image: &str, binary: &str) {
        let dir = self.dir.display().to_string();
        let ip = self.ip;
        run(
            "docker",
            &[
                "run",
                "-d",
                "--name",
                &self.container,
                "--hostname",
                self.name,
                "--privileged",
                "--cgroupns=private",
                "--network",
                MGMT_NET,
                "--ip",
                ip,
                "--tmpfs",
                "/run",
                "--tmpfs",
                "/run/lock",
                "-v",
                "/lib/modules:/lib/modules:ro",
                "-v",
                &format!("{dir}:{dir}"),
                "-v",
                &format!("{binary}:/usr/local/bin/orbivisord:ro"),
                image,
            ],
        );
        wait_for(
            &format!("systemd in {}", self.container),
            AGENT_WAIT,
            || async {
                let (_, out, _) = self.try_exec(&["systemctl", "is-system-running"]);
                matches!(out.trim(), "running" | "degraded")
            },
        )
        .await;
        let (_, failed, _) = self.try_exec(&["systemctl", "--failed", "--no-legend"]);
        if !failed.trim().is_empty() {
            eprintln!("{}: failed units:\n{failed}", self.container);
        }
        let ldd = self.exec(&["ldd", "/usr/local/bin/orbivisord"]);
        assert!(!ldd.contains("not found"), "{ldd}");
    }

    /// 설정을 쓰고 orbivisord를 systemd 단위로 띄운다(제어 소켓이 응답할 때까지). 에이전트의 다시 맞추기는 30초마다라
    /// 게이트웨이 넘김 시험 동안 멈춘 단위를 곧바로 다시 켜지 않는다(넘김은 BFD로 몇 초).
    async fn start(&self) {
        std::fs::create_dir_all(&self.dir).unwrap();
        let config = format!(
            "name = \"{name}\"\naddress = \"{ip}\"\nlisten = \"{ip}:8006\"\ncluster_listen = \"{ip}:8007\"\n\
             control_socket = \"{sock}\"\ndata_dir = \"{data}\"\nrun_dir = \"{run}\"\n\
             qemu_share_dir = \"/usr/share/qemu\"\n[sdn]\nrefresh_secs = 30\n\
             [migration]\nbandwidth_mib = 1\n[hypervisor]\nsystemd_scope = false\n[updates]\nenabled = false\n",
            name = self.name,
            ip = self.ip,
            sock = self.path("opld.sock").display(),
            data = self.path("data").display(),
            run = self.path("run").display(),
        );
        std::fs::write(self.path("opld.toml"), config).unwrap();
        let log = self.path("opld.log").display().to_string();
        self.exec(&[
            "systemd-run",
            "--unit=orbivisord",
            &format!("--property=StandardOutput=append:{log}"),
            &format!("--property=StandardError=append:{log}"),
            "/usr/local/bin/orbivisord",
            "--config",
            &self.path("opld.toml").display().to_string(),
        ]);
        let http = reqwest::Client::builder()
            .unix_socket(self.path("opld.sock"))
            .build()
            .unwrap();
        wait_for(
            &format!("orbivisord on {}", self.name),
            AGENT_WAIT,
            || async {
                http.get("http://opld/v1/status")
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
            },
        )
        .await;
    }

    /// API가 HTTPS로 응답할 때까지 기다린다.
    async fn wait_api(&self) {
        let http = reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        let url = format!("{}/api/v1/auth/whoami", self.server());
        wait_for(&format!("the API of {}", self.name), AGENT_WAIT, || async {
            http.get(&url)
                .send()
                .await
                .is_ok_and(|r| r.status() == reqwest::StatusCode::UNAUTHORIZED)
        })
        .await;
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.path("opld.log")).unwrap_or_default()
    }

    /// oplctl 명령(로그인 정보는 이 노드의 디렉터리)
    async fn ctl(&self, args: &[&str]) -> anyhow::Result<String> {
        let config = self.path("oplctl.json");
        let mut argv = vec!["oplctl", "--config", config.to_str().unwrap()];
        argv.extend_from_slice(args);
        let mut out = Vec::new();
        oplctl::run(argv, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    /// 성공해야 하는 oplctl 명령
    async fn ctl_ok(&self, args: &[&str]) -> String {
        match self.ctl(args).await {
            Ok(out) => out,
            Err(e) => panic!("orbivisorctl {args:?} on {}: {e:#}", self.name),
        }
    }

    /// `oplctl -o json`의 결과
    async fn ctl_json(&self, args: &[&str]) -> Value {
        let mut argv = vec!["-o", "json"];
        argv.extend_from_slice(args);
        serde_json::from_str(&self.ctl_ok(&argv).await).unwrap()
    }

    /// `cluster init`(제어 소켓)과 출력(비밀번호, CA 지문, 조인 URL)
    async fn init(&mut self, cluster: &str) {
        let socket = self.path("opld.sock").display().to_string();
        let out = self
            .ctl_ok(&["cluster", "init", "--name", cluster, "--socket", &socket])
            .await;
        let field = |prefix: &str| {
            out.lines()
                .find_map(|l| l.trim().strip_prefix(prefix).map(str::to_string))
                .unwrap_or_else(|| panic!("no {prefix:?} in {out}"))
        };
        self.admin_password = Some(field("admin password (shown only once): "));
        self.ca_fingerprint = Some(field("CA fingerprint: "));
        self.join_url = Some(format!("opl-join://{}", field("opl-join://")));
        self.wait_api().await;
    }

    async fn join(&self, url: &str) {
        let socket = self.path("opld.sock").display().to_string();
        self.ctl_ok(&["cluster", "join", url, "--socket", &socket])
            .await;
        self.wait_api().await;
    }

    async fn login_with(&self, password: &str, ca_fingerprint: &str) {
        let file = self.path("admin-password");
        std::fs::write(&file, password).unwrap();
        self.ctl_ok(&[
            "login",
            "--server",
            &self.server(),
            "--user",
            "admin",
            "--password-file",
            file.to_str().unwrap(),
            "--ca-fingerprint",
            ca_fingerprint,
        ])
        .await;
    }

    /// 이 중앙의 NB·SB `cluster/status`
    fn cluster_status(&self, db: &str) -> ClusterStatus {
        let (ctl, name) = match db {
            "nb" => ("/run/opl/ovn/nb.ctl", "OVN_Northbound"),
            _ => ("/run/opl/ovn/sb.ctl", "OVN_Southbound"),
        };
        let out = self.exec(&["ovs-appctl", "-t", ctl, "cluster/status", name]);
        eprintln!("{} cluster/status {name}:\n{out}", self.name);
        parse_cluster_status(&out).unwrap()
    }

    /// 이 중앙의 `ovn-nbctl`·`ovn-sbctl --bare …`(실패하면 빈 글, 따옴표는 뗀다)
    fn ovn_bare(&self, program: &str, db: &str, args: &[&str]) -> String {
        let socket = format!("--db=unix:/run/opl/ovn/{db}.sock");
        let mut all = vec![program, socket.as_str(), "--bare"];
        all.extend_from_slice(args);
        let (ok, out, _) = self.try_exec(&all);
        if ok {
            out.trim().trim_matches('"').to_string()
        } else {
            String::new()
        }
    }

    /// 컨테이너 안 이름공간 `ns`에서 ping
    fn ping(&self, ns: &str, to: &str) -> bool {
        self.try_exec(&["ip", "netns", "exec", ns, "ping", "-c", "1", "-W", "1", to])
            .0
    }
}

/// 노드 셋과 호스트 쪽 물리 망. 시험이 끝나면(실패해도) 컨테이너·망·브리지·이름공간을 지운다. 실패하면 먼저 진단을 찍는다.
struct Fleet {
    nodes: Vec<Node>,
    root: PathBuf,
    /// 바꾼 sysctl의 옛 값(끝나면 되돌린다 — 개발 호스트에서 돌려도 남지 않게, 리뷰 1 M-10)
    sysctls: Vec<(String, String)>,
}

impl Fleet {
    /// 지난 시험이 남긴 것을 지우고 도커 망·호스트 브리지·`ext` 이름공간을 만들고 컨테이너 셋을 띄운다.
    async fn up(image: &str) -> Self {
        let root = PathBuf::from("/var/tmp/oplmn");
        let mut fleet = Fleet {
            sysctls: Vec::new(),
            nodes: NODES
                .iter()
                .map(|(name, ip)| Node {
                    name,
                    container: format!("opl-{name}"),
                    ip,
                    dir: root.join(name),
                    admin_password: None,
                    ca_fingerprint: None,
                    join_url: None,
                    id: String::new(),
                })
                .collect(),
            root,
        };
        fleet.clean();
        std::fs::create_dir_all(&fleet.root).unwrap();
        run(
            "docker",
            &["network", "create", "--subnet", MGMT_SUBNET, MGMT_NET],
        );
        // 물리 망: 호스트 Linux 브리지 + "물리 라우터" 이름공간. 브리지 프레임이 도커의 iptables FORWARD DROP을 지나지
        // 않게 한다(br_netfilter가 있으면).
        for key in ["bridge-nf-call-iptables", "bridge-nf-call-ip6tables"] {
            let path = format!("/proc/sys/net/bridge/{key}");
            if let Ok(old) = std::fs::read_to_string(&path) {
                fleet.sysctls.push((path.clone(), old.trim().to_string()));
                std::fs::write(&path, "0").unwrap();
            }
        }
        run("ip", &["link", "add", EXT_BRIDGE, "type", "bridge"]);
        run("ip", &["link", "set", EXT_BRIDGE, "up"]);
        run("ip", &["netns", "add", EXT_NS]);
        run(
            "ip",
            &[
                "link", "add", "oplextr", "type", "veth", "peer", "name", "oplexti",
            ],
        );
        run(
            "ip",
            &["link", "set", "oplextr", "master", EXT_BRIDGE, "up"],
        );
        run("ip", &["link", "set", "oplexti", "netns", EXT_NS]);
        ext(&["ip", "link", "set", "lo", "up"]);
        ext(&[
            "ip",
            "addr",
            "add",
            &format!("{EXT_IP}/24"),
            "dev",
            "oplexti",
        ]);
        ext(&["ip", "link", "set", "oplexti", "up"]);

        let binary = env!("CARGO_BIN_EXE_orbivisord");
        for n in &fleet.nodes {
            std::fs::create_dir_all(&n.dir).unwrap();
            n.boot(image, binary).await;
        }
        // 노드마다 관리자 브리지 vmbr1과 그것을 호스트 브리지에 잇는 veth(호스트 쪽 `oplv<i>`)
        for (i, n) in fleet.nodes.iter().enumerate() {
            let (host, inner) = (format!("oplv{}", i + 1), format!("oplu{}", i + 1));
            let pid = run("docker", &["inspect", "-f", "{{.State.Pid}}", &n.container]);
            run(
                "ip",
                &["link", "add", &host, "type", "veth", "peer", "name", &inner],
            );
            run("ip", &["link", "set", &host, "master", EXT_BRIDGE, "up"]);
            run("ip", &["link", "set", &inner, "netns", pid.trim()]);
            n.exec(&["ip", "link", "add", "vmbr1", "type", "bridge"]);
            n.exec(&["ip", "link", "set", &inner, "master", "vmbr1", "up"]);
            n.exec(&["ip", "link", "set", "vmbr1", "up"]);
        }
        for n in &fleet.nodes {
            n.start().await;
        }
        fleet
    }

    fn node(&self, name: &str) -> &Node {
        self.nodes.iter().find(|n| n.name == name).unwrap()
    }

    /// 컨테이너·도커 망·호스트 브리지·이름공간·디렉터리를 지운다(없어도 된다).
    fn clean(&self) {
        for n in &self.nodes {
            let _ = try_run("docker", &["rm", "-f", &n.container]);
        }
        let _ = try_run("docker", &["network", "rm", MGMT_NET]);
        let _ = try_run("ip", &["netns", "del", EXT_NS]);
        // 이름공간이 아직 붙들려 있어도 호스트 쪽 veth는 지운다(다음 `ip link add`가 실패하지 않게 — 리뷰 1 M-10).
        let _ = try_run("ip", &["link", "del", "oplextr"]);
        let _ = try_run("ip", &["link", "del", EXT_BRIDGE]);
        for i in 1..=NODES.len() {
            let _ = try_run("ip", &["link", "del", &format!("oplv{i}")]);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }

    /// 실패 진단: 노드마다 OVN·OVS 상태, 단위 저널, opld 로그
    fn diagnose(&self) {
        show("docker", &["ps", "-a"]);
        show("ip", &["-br", "link"]);
        show("bridge", &["fdb", "show", "br", EXT_BRIDGE]);
        show("ip", &["netns", "exec", EXT_NS, "ip", "neigh"]);
        for n in &self.nodes {
            eprintln!("===== {} =====", n.name);
            let running = try_run(
                "docker",
                &["inspect", "-f", "{{.State.Running}}", &n.container],
            )
            .1;
            if running.trim() == "true" {
                for args in [
                    &["ovs-vsctl", "show"][..],
                    &["ovn-sbctl", "--db=unix:/run/opl/ovn/sb.sock", "show"],
                    &["ovn-nbctl", "--db=unix:/run/opl/ovn/nb.sock", "show"],
                    &[
                        "systemctl",
                        "status",
                        "--no-pager",
                        "orbivisord",
                        "ovn-controller",
                        "ovs-vswitchd",
                        "opl-ovn-nb",
                        "opl-ovn-sb",
                        "opl-ovn-northd",
                    ],
                    &[
                        "journalctl",
                        "--no-pager",
                        "-n",
                        "60",
                        "-u",
                        "ovn-controller",
                        "-u",
                        "ovs-vswitchd",
                        "-u",
                        "opl-ovn-northd",
                    ],
                    &["tail", "-n", "40", "/var/log/ovn/ovn-controller.log"],
                ] {
                    let mut all = vec!["exec", n.container.as_str()];
                    all.extend_from_slice(args);
                    show("docker", &all);
                }
            }
            let log = n.log();
            let tail: Vec<&str> = log.lines().rev().take(200).collect();
            eprintln!(
                "opld log of {} (last 200 lines):\n{}",
                n.name,
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.diagnose();
        }
        self.clean();
        for (path, old) in &self.sysctls {
            let _ = std::fs::write(path, old);
        }
    }
}

/// 호스트 이름공간 `ext`에서 명령(성공해야 함)
fn ext(args: &[&str]) -> String {
    let mut all = vec!["netns", "exec", EXT_NS];
    all.extend_from_slice(args);
    run("ip", &all)
}

/// `ext`에서 `to`로 ping 셋(답이 없어도 된다 — 패킷을 보내는 것이 목적일 때)
fn ext_ping(to: &str) -> bool {
    try_run(
        "ip",
        &[
            "netns", "exec", EXT_NS, "ping", "-c", "3", "-i", "0.3", "-W", "1", to,
        ],
    )
    .0
}

/// `ext`의 이웃 표에서 `ip`의 MAC(없으면 빈 글)
fn ext_neigh(ip: &str) -> String {
    let out = ext(&["ip", "-j", "neigh", "show", ip]);
    let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    v[0]["lladdr"].as_str().unwrap_or_default().to_string()
}

/// 패킷 캡처 하나: `prefix`(빈 것이면 호스트, 아니면 `docker exec <컨테이너>`)의 장치 `dev`에서 `filter`에 맞는 패킷을
/// [`CAPTURE_SECS`] 동안 센다.
struct Capture {
    what: String,
    child: Child,
}

impl Capture {
    fn start(prefix: &[String], dev: &str, filter: &str) -> Self {
        let mut argv: Vec<String> = prefix.to_vec();
        argv.extend(
            [
                "timeout",
                CAPTURE_SECS,
                "tcpdump",
                "-n",
                "-l",
                "-i",
                dev,
                filter,
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            what: format!("{} {dev} [{filter}]", prefix.last().map_or("host", |s| s)),
            child,
        }
    }

    /// tcpdump가 장치에 붙었다고 알릴 때까지(stderr의 `listening on`, 15초까지) 기다린다. stderr의 나머지는 버린다.
    fn wait_listening(&mut self) {
        use std::io::BufRead;
        let stderr = self.child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                if line.contains("listening on") {
                    let _ = tx.send(());
                }
            }
        });
        if rx.recv_timeout(Duration::from_secs(15)).is_err() {
            eprintln!("capture {}: tcpdump did not report listening", self.what);
        }
    }

    /// 끝나기를 기다려 잡은 패킷 수
    fn count(self) -> usize {
        let out = self.child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let n = text.lines().filter(|l| !l.trim().is_empty()).count();
        eprintln!("capture {}: {n} packets\n{text}", self.what);
        n
    }
}

/// 캡처들을 띄우고(tcpdump가 모두 붙을 때까지 기다림), `action`을 하고, 장치마다 잡은 수를 돌려준다.
fn captured(points: &[(Vec<String>, &str)], filter: &str, action: impl FnOnce()) -> Vec<usize> {
    let mut caps: Vec<Capture> = points
        .iter()
        .map(|(prefix, dev)| Capture::start(prefix, dev, filter))
        .collect();
    for c in &mut caps {
        c.wait_listening();
    }
    action();
    caps.into_iter().map(Capture::count).collect()
}

/// [`captured`]를 `ok`가 맞을 때까지 [`CAPTURE_ATTEMPTS`]번까지 다시 하고 마지막 수를 돌려준다(호출자가 그 수로
/// 단언한다 — 정말 틀리면 모든 시도가 틀린다).
fn captured_until(
    points: &[(Vec<String>, &str)],
    filter: &str,
    action: impl Fn(),
    ok: impl Fn(&[usize]) -> bool,
) -> Vec<usize> {
    let mut counts = Vec::new();
    for attempt in 1..=CAPTURE_ATTEMPTS {
        counts = captured(points, filter, &action);
        if ok(&counts) {
            break;
        }
        eprintln!("capture attempt {attempt}/{CAPTURE_ATTEMPTS} [{filter}]: {counts:?}; again");
    }
    counts
}

/// 호스트 쪽 veth(노드 `i` = 1..3)
fn host_veth(i: usize) -> (Vec<String>, &'static str) {
    (Vec::new(), ["oplv1", "oplv2", "oplv3"][i - 1])
}

/// 꺼진 API VM(NIC 하나, `network`)을 만들고 (id, MAC, IPAM 주소, 논리 포트)
async fn api_vm(c1: &Node, name: &str, network: &str) -> (String, String, String, String) {
    c1.ctl_ok(&["vm", "create", name, "--memory", "128", "--nic", network])
        .await;
    let vm = c1.ctl_json(&["vm", "get", name]).await;
    let id = vm["id"].as_str().unwrap().to_string();
    let mac = vm["spec"]["nics"][0]["mac"].as_str().unwrap().to_string();
    let ip = vm["spec"]["nics"][0]["ips"][0]
        .as_str()
        .unwrap()
        .to_string();
    let port = opl_types::sdn::port_name(id.parse().unwrap(), mac.parse().unwrap());
    (id, mac, ip, port)
}

/// 노드 `n`의 이름공간 `ns`에 OVS 내부 포트 `dev`(논리 포트 `port`, MAC·주소·기본 경로)를 둔다 — 게스트 없는 "VM".
fn namespace_vm(n: &Node, ns: &str, dev: &str, vm: &(String, String, String, String), gw: &str) {
    let (_, mac, ip, port) = vm;
    n.exec(&["ip", "netns", "add", ns]);
    n.exec(&[
        "ovs-vsctl",
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
    ]);
    n.exec(&["ip", "link", "set", dev, "netns", ns]);
    let in_ns = |args: &[&str]| {
        let mut all = vec!["ip", "netns", "exec", ns, "ip"];
        all.extend_from_slice(args);
        n.exec(&all);
    };
    in_ns(&["link", "set", dev, "address", mac]);
    in_ns(&["link", "set", dev, "mtu", "1442"]);
    in_ns(&["link", "set", "lo", "up"]);
    in_ns(&["link", "set", dev, "up"]);
    in_ns(&["addr", "add", &format!("{ip}/24"), "dev", dev]);
    in_ns(&["route", "add", "default", "via", gw]);
}

/// 라우터 r1 보기의 게이트웨이 주인(노드 이름, 없으면 빈 글)
async fn hosting(c1: &Node) -> String {
    match c1.ctl(&["-o", "json", "router", "get", "r1"]).await {
        Ok(out) => serde_json::from_str::<Value>(&out).unwrap()["hosting"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        Err(e) => {
            eprintln!("router get r1: {e:#}");
            String::new()
        }
    }
}

/// 넘김 하나를 잰다: `action` 뒤 라우터 주인이 `to`가 되고 이름공간 VM(c1의 `vma`)의 SNAT ping이 다시 닿을 때까지.
/// (주인이 바뀐 시간, ping이 닿은 시간)
async fn failover(
    fleet: &Fleet,
    what: &str,
    to: &str,
    action: impl FnOnce(),
) -> (Duration, Duration) {
    let c1 = fleet.node("c1");
    // 앞 넘김의 되돌아감 바로 뒤에는 잠깐 흔들릴 수 있다(ovs-vswitchd를 다시 켠 뒤 흐름을 다시 까는 동안).
    wait_for(&format!("SNAT before {what}"), AGENT_WAIT, || async {
        c1.ping("vma", EXT_IP)
    })
    .await;
    let t0 = Instant::now();
    action();
    let (mut moved, mut reached) = (None, None);
    let mut last = String::new();
    while moved.is_none() || reached.is_none() {
        assert!(
            t0.elapsed() < AGENT_WAIT,
            "{what}: the gateway did not move to {to} (hosting {last:?}, ping {reached:?})"
        );
        if moved.is_none() {
            last = hosting(c1).await;
            if last == to {
                moved = Some(t0.elapsed());
            }
        }
        // 주인이 바뀌기 전의 ping은 옛 게이트웨이가 아직 일할 때도 닿을 수 있다(점잖은 멈춤) — 바뀐 뒤에 닿은 것을 잰다.
        if moved.is_some() && reached.is_none() && c1.ping("vma", EXT_IP) {
            reached = Some(t0.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (moved, reached) = (moved.unwrap(), reached.unwrap());
    eprintln!(
        "PROBE failover ({what}): hosting -> {to} after {moved:?}, SNAT ping again after {reached:?}"
    );
    (moved, reached)
}

/// 노드 `i`(1..3)의 호스트 쪽 veth로 `router_mac`이 나가는지(호스트 브리지 FDB)
fn fdb_port(mac: &str) -> String {
    let out = run("bridge", &["-j", "fdb", "show", "br", EXT_BRIDGE]);
    let v: Value = serde_json::from_str(&out).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .find(|e| e["mac"] == mac)
        .and_then(|e| e["ifname"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn three_container_nodes_run_ovn() {
    let image = std::env::var("OPL_MULTINODE_IMAGE").unwrap_or_else(|_| "opl-sdn-node".into());
    for m in ["openvswitch", "geneve"] {
        assert!(
            std::path::Path::new(&format!("/sys/module/{m}")).exists(),
            "kernel module {m} is not loaded (modprobe {m})"
        );
    }
    let mut fleet = Fleet::up(&image).await;
    Box::pin(cluster_section(&mut fleet)).await;
    Box::pin(ovn_section(&fleet)).await;
    Box::pin(routing_section(&fleet)).await;
    Box::pin(migration_section(&fleet)).await;
    Box::pin(failover_section(&fleet)).await;
    Box::pin(dead_node_section(&fleet)).await;
}

/// 1. Orbivisor 클러스터 셋(투표 멤버 셋)
async fn cluster_section(fleet: &mut Fleet) {
    let c1 = &mut fleet.nodes[0];
    c1.init("mn").await;
    let (password, fp) = (
        c1.admin_password.clone().unwrap(),
        c1.ca_fingerprint.clone().unwrap(),
    );
    let url = c1.join_url.clone().unwrap();
    c1.login_with(&password, &fp).await;
    fleet.nodes[1].join(&url).await;
    let url = fleet.nodes[0].ctl_ok(&["cluster", "join-token"]).await;
    fleet.nodes[2].join(url.trim()).await;
    for n in &fleet.nodes[1..] {
        n.login_with(&password, &fp).await;
    }
    let c1 = &fleet.nodes[0];
    wait_for("three online voters", AGENT_WAIT, || async {
        let view = c1.ctl_json(&["cluster", "status"]).await;
        let members = view["members"].as_array().cloned().unwrap_or_default();
        members.len() == 3
            && members
                .iter()
                .all(|m| m["voter"] == true && m["online"] == true)
    })
    .await;
    let list = c1.ctl_json(&["node", "list"]).await;
    for n in &mut fleet.nodes {
        n.id = list
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == n.name)
            .and_then(|v| v["id"].as_str())
            .unwrap_or_else(|| panic!("node {} not in {list}", n.name))
            .to_string();
    }
}

/// 2. OVN 중앙 셋(실제 NB·SB Raft 셋)과 chassis 셋, 컨테이너 사이 Geneve
async fn ovn_section(fleet: &Fleet) {
    let c1 = fleet.node("c1");
    let started = Instant::now();
    c1.ctl_ok(&["sdn", "init", "c1"]).await;
    eprintln!("sdn init c1: {:?}", started.elapsed());
    for name in ["c2", "c3"] {
        let started = Instant::now();
        c1.ctl_ok(&["sdn", "central", "add", name]).await;
        eprintln!("sdn central add {name}: {:?}", started.elapsed());
    }
    let started = Instant::now();
    let said = c1.ctl_ok(&["sdn", "chassis", "install", "--all"]).await;
    eprintln!("sdn chassis install --all: {:?}\n{said}", started.elapsed());

    // 실제 OVN Raft 셋: 중앙마다 NB·SB 서버 셋, 클러스터 id 하나, 리더 하나
    for db in ["nb", "sb"] {
        let mut cids = std::collections::BTreeSet::new();
        let mut leaders = 0;
        for n in &fleet.nodes {
            let mut status = n.cluster_status(db);
            // 합류한 서버가 리더의 목록을 따라잡기까지 잠깐
            let deadline = Instant::now() + AGENT_WAIT;
            while status.servers.len() != 3 || !status.is_member() {
                assert!(Instant::now() < deadline, "{} {db}: {status:?}", n.name);
                tokio::time::sleep(Duration::from_secs(1)).await;
                status = n.cluster_status(db);
            }
            assert_eq!(
                status.address,
                format!("ssl:{}:{}", n.ip, if db == "nb" { 6643 } else { 6644 }),
                "{status:?}"
            );
            cids.insert(status.cid);
            leaders += usize::from(status.is_leader());
        }
        assert_eq!(cids.len(), 1, "{db}: {cids:?}");
        assert!(leaders <= 1, "{db}: {leaders} leaders");
    }
    let view = c1.ctl_json(&["sdn", "status"]).await;
    eprintln!("sdn status: {view}");
    assert_eq!(view["centrals"].as_array().unwrap().len(), 3, "{view}");

    // SB Chassis 셋(노드 UUID)과 노드 상태
    let mut want: Vec<String> = fleet.nodes.iter().map(|n| n.id.clone()).collect();
    want.sort();
    wait_for("three chassis in the SB", AGENT_WAIT, || async {
        let names = c1.ovn_bare("ovn-sbctl", "sb", &["--columns=name", "list", "Chassis"]);
        let mut got: Vec<String> = names.split_whitespace().map(str::to_string).collect();
        got.sort();
        got == want
    })
    .await;
    for n in &fleet.nodes {
        wait_for(&format!("{} chassis ready", n.name), AGENT_WAIT, || async {
            let me = c1.ctl_json(&["node", "get", n.name]).await;
            me["status"]["sdn"]["chassis"] == "ready" && me["status"]["sdn"]["encap_ip"] == n.ip
        })
        .await;
    }
    // 컨테이너 사이 Geneve: 노드마다 다른 둘로 가는 터널
    for n in &fleet.nodes {
        wait_for(
            &format!("Geneve tunnels on {}", n.name),
            AGENT_WAIT,
            || async {
                let (_, out, _) = n.try_exec(&[
                    "ovs-vsctl",
                    "--bare",
                    "--columns=options",
                    "find",
                    "Interface",
                    "type=geneve",
                ]);
                fleet.nodes.iter().filter(|o| o.name != n.name).all(|o| {
                    out.contains(&format!("remote_ip=\"{}\"", o.ip))
                        || out.contains(&format!("remote_ip={}", o.ip))
                })
            },
        )
        .await;
    }
    n_show(fleet.node("c1"), &["ovs-vsctl", "show"]);
}

/// 진단용 출력(컨테이너 안)
fn n_show(n: &Node, args: &[&str]) {
    let (_, out, err) = n.try_exec(args);
    eprintln!("{}$ {}\n{out}{err}", n.name, args.join(" "));
}

/// 3. physnet·게이트웨이·공급자 네트워크·오버레이 둘·라우터 r1, 이름공간 VM 둘로 chassis 사이 동서 라우팅과 SNAT,
///    게스트 없는 QEMU VM의 분산 유동 IP와 중앙으로 바뀌기
async fn routing_section(fleet: &Fleet) {
    let (c1, c2, c3) = (fleet.node("c1"), fleet.node("c2"), fleet.node("c3"));
    c1.ctl_ok(&["node", "sdn", "c1", "--physnet", "ext=vmbr1"])
        .await;
    c1.ctl_ok(&[
        "node",
        "sdn",
        "c2",
        "--physnet",
        "ext=vmbr1",
        "--gateway",
        "200",
    ])
    .await;
    c1.ctl_ok(&[
        "node",
        "sdn",
        "c3",
        "--physnet",
        "ext=vmbr1",
        "--gateway",
        "100",
    ])
    .await;
    for n in &fleet.nodes {
        wait_for(
            &format!("physnet ext on {}", n.name),
            AGENT_WAIT,
            || async {
                let me = c1.ctl_json(&["node", "get", n.name]).await;
                me["status"]["sdn"]["physnets"][0]["ready"] == true
            },
        )
        .await;
    }
    c1.ctl_ok(&[
        "network",
        "create",
        "ext",
        "--sdn",
        "--provider",
        "ext",
        "--subnet",
        "203.0.113.0/24,gateway=203.0.113.1,floating=203.0.113.100-203.0.113.199",
    ])
    .await;
    c1.ctl_ok(&[
        "network",
        "create",
        "t1",
        "--sdn",
        "--subnet",
        "10.61.1.0/24",
    ])
    .await;
    c1.ctl_ok(&[
        "network",
        "create",
        "t2",
        "--sdn",
        "--subnet",
        "10.61.2.0/24",
    ])
    .await;
    c1.ctl_ok(&[
        "router",
        "create",
        "r1",
        "--external",
        &format!("ext,ip={ROUTER_IP}"),
        "--interface",
        "t1",
        "--interface",
        "t2",
    ])
    .await;
    // 게이트웨이 순위: c2(200)가 주인, c3(100) 다음
    wait_for("r1 hosted on c2", AGENT_WAIT, || async {
        hosting(c1).await == "c2"
    })
    .await;
    let view = c1.ctl_json(&["router", "get", "r1"]).await;
    eprintln!("router r1: {view}");
    assert_eq!(view["gateways"][0]["node"], "c2", "{view}");
    assert_eq!(view["gateways"][1]["node"], "c3", "{view}");

    // 이름공간 VM: c1의 vma(t1), c3의 vmb(t2)
    let a = api_vm(c1, "ns-a", "t1").await;
    let b = api_vm(c1, "ns-b", "t2").await;
    namespace_vm(c1, "vma", "oplnsa", &a, "10.61.1.1");
    namespace_vm(c3, "vmb", "oplnsb", &b, "10.61.2.1");
    wait_for(
        "an east-west ping c1 -> c3 across r1",
        AGENT_WAIT,
        || async { c1.ping("vma", &b.2) },
    )
    .await;
    // 분산 라우팅: c1 → c3 패킷은 Geneve로 c3에 곧바로 가고 게이트웨이 c2를 지나지 않는다.
    let counts = captured_until(
        &[(c2.prefix(), "eth0"), (c3.prefix(), "eth0")],
        "geneve and icmp",
        || {
            c1.try_exec(&[
                "ip", "netns", "exec", "vma", "ping", "-c", "5", "-i", "0.3", &b.2,
            ]);
        },
        |c| c[0] == 0 && c[1] > 0,
    );
    assert_eq!(
        counts[0], 0,
        "east-west traffic went through the gateway c2"
    );
    assert!(counts[1] > 0, "no Geneve traffic seen on c3");

    // SNAT: 주인 게이트웨이 c2의 veth로 나가고 `ext`는 라우터 주소를 본다(`ext`에는 10.x 경로가 없다).
    wait_for(
        "an SNAT ping to the physical router",
        AGENT_WAIT,
        || async { c1.ping("vma", EXT_IP) },
    )
    .await;
    let src = format!("icmp and src host {ROUTER_IP}");
    let counts = captured_until(
        &[host_veth(1), host_veth(2), host_veth(3)],
        &src,
        || {
            c1.try_exec(&[
                "ip", "netns", "exec", "vma", "ping", "-c", "5", "-i", "0.3", EXT_IP,
            ]);
        },
        |c| c[0] == 0 && c[1] > 0 && c[2] == 0,
    );
    assert_eq!(
        counts,
        [0, counts[1], 0],
        "SNAT did not leave only through c2"
    );
    assert!(counts[1] > 0, "SNAT did not leave through c2");

    // 분산 유동 IP: c1에서 도는 게스트 없는 QEMU VM(펌웨어만). 밖 → 유동 IP 패킷이 c1의 veth로 들어 VM tap에 닿고,
    // c1이 ARP에 NIC MAC으로 답한다(게이트웨이를 지나지 않음).
    c1.ctl_ok(&[
        "vm", "create", "vm-q", "--memory", "256", "--nic", "t1", "--node", "c1", "--start",
    ])
    .await;
    let q = c1.ctl_json(&["vm", "get", "vm-q"]).await;
    assert_eq!(q["status"]["phase"], "running", "{q}");
    let q_mac = q["spec"]["nics"][0]["mac"].as_str().unwrap().to_string();
    let q_ip = q["spec"]["nics"][0]["ips"][0].as_str().unwrap().to_string();
    let q_tap = opl_types::network::tap_name(q["id"].as_str().unwrap().parse().unwrap(), 0);
    c1.ctl_ok(&[
        "fip",
        "create",
        "--network",
        "ext",
        "--ip",
        "203.0.113.120",
        "--name",
        "fq",
        "--attach",
        "vm-q",
    ])
    .await;
    let fip_state = |want: &'static str, node: &'static str| async move {
        let v = c1.ctl_json(&["fip", "get", "fq"]).await;
        v["state"]["kind"] == want && (node.is_empty() || v["state"]["node"] == node)
    };
    wait_for("fq distributed on c1", AGENT_WAIT, || {
        fip_state("distributed", "c1")
    })
    .await;
    let dst = "icmp and dst host 203.0.113.120";
    let inbound = |label: &str| -> Vec<usize> {
        ext(&["ip", "neigh", "flush", "all"]);
        let counts = captured(&[host_veth(1), host_veth(2), host_veth(3)], dst, || {
            ext_ping("203.0.113.120");
        });
        eprintln!(
            "{label}: fq inbound per veth {counts:?}, neighbor {}",
            ext_neigh("203.0.113.120")
        );
        counts
    };
    wait_for("fq distributed on the wire", AGENT_WAIT, || async {
        let counts = inbound("distributed");
        counts[0] > 0 && counts[1] == 0 && ext_neigh("203.0.113.120") == q_mac
    })
    .await;
    let tap = captured_until(
        &[(c1.prefix(), q_tap.as_str())],
        &format!("icmp and dst host {q_ip}"),
        || {
            ext_ping("203.0.113.120");
        },
        |c| c[0] > 0,
    );
    assert!(tap[0] > 0, "the DNAT-ed packets did not reach the VM tap");

    // c1에서 physnet을 빼면 중앙: 게이트웨이 c2가 받아 Geneve로 c1에 보낸다.
    c1.ctl_ok(&["node", "sdn", "c1", "--no-physnet", "ext"])
        .await;
    wait_for("fq centralized", AGENT_WAIT, || {
        fip_state("centralized", "")
    })
    .await;
    wait_for("fq centralized on the wire", AGENT_WAIT, || async {
        let counts = inbound("centralized");
        counts[0] == 0 && counts[1] > 0 && ext_neigh("203.0.113.120") != q_mac
    })
    .await;
    // 다시 넣으면 분산
    c1.ctl_ok(&["node", "sdn", "c1", "--physnet", "ext=vmbr1"])
        .await;
    wait_for("fq distributed again", AGENT_WAIT, || {
        fip_state("distributed", "c1")
    })
    .await;
    wait_for("fq distributed on the wire again", AGENT_WAIT, || async {
        let counts = inbound("distributed again");
        counts[0] > 0 && counts[1] == 0
    })
    .await;
}

/// 4. 두 chassis 라이브 마이그레이션: vm-q를 c1 → c3로 옮기는 동안 NB `requested-chassis`가 둘이고(`rarp`), 끝에 SB 바인딩이
///    c3이며 분산 유동 IP가 c3의 veth로 옮겨 간다.
async fn migration_section(fleet: &Fleet) {
    let (c1, c3) = (fleet.node("c1"), fleet.node("c3"));
    let q = c1.ctl_json(&["vm", "get", "vm-q"]).await;
    let port = opl_types::sdn::port_name(
        q["id"].as_str().unwrap().parse().unwrap(),
        q["spec"]["nics"][0]["mac"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    );
    // 옮기는 동안 NB 포트의 칸을 0.2초마다 읽는다(다른 스레드 — 옮기기 명령은 끝날 때까지 기다린다).
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let poller = {
        let (seen, stop) = (seen.clone(), stop.clone());
        let container = c1.container.clone();
        let port = port.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let (_, out, _) = try_run(
                    "docker",
                    &[
                        "exec",
                        &container,
                        "ovn-nbctl",
                        "--db=unix:/run/opl/ovn/nb.sock",
                        "--bare",
                        "get",
                        "Logical_Switch_Port",
                        &port,
                        "options",
                    ],
                );
                let mut seen = seen.lock().unwrap();
                let value = out.trim().to_string();
                if seen.last() != Some(&value) {
                    seen.push(value);
                }
                drop(seen);
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let started = Instant::now();
    let said = c1.ctl(&["vm", "migrate", "vm-q", "c3"]).await;
    stop.store(true, Ordering::Relaxed);
    poller.join().unwrap();
    let said = said.unwrap_or_else(|e| panic!("vm migrate vm-q c3: {e:#}"));
    eprintln!("vm migrate vm-q c3 ({:?}): {said}", started.elapsed());
    let seen = seen.lock().unwrap().clone();
    eprintln!("PROBE NB options of {port} during the migration: {seen:#?}");
    let both = format!("{},{}", c1.id, c3.id);
    assert!(
        seen.iter()
            .any(|o| o.contains(&both) && o.contains("activation-strategy=rarp")),
        "requested-chassis was never {both} with rarp: {seen:?}"
    );
    let q = c1.ctl_json(&["vm", "get", "vm-q"]).await;
    assert_eq!(q["status"]["node"], c3.id.as_str(), "{q}");
    // SB 바인딩은 c3
    let chassis_uuid = c1.ovn_bare(
        "ovn-sbctl",
        "sb",
        &[
            "--columns=_uuid",
            "find",
            "Chassis",
            &format!("name={}", c3.id),
        ],
    );
    wait_for("the SB binding on c3", AGENT_WAIT, || async {
        c1.ovn_bare(
            "ovn-sbctl",
            "sb",
            &[
                "--columns=chassis",
                "find",
                "Port_Binding",
                &format!("logical_port={port}"),
            ],
        ) == chassis_uuid
    })
    .await;
    wait_for("the NB port to request only c3", AGENT_WAIT, || async {
        c1.ovn_bare(
            "ovn-nbctl",
            "nb",
            &[
                "get",
                "Logical_Switch_Port",
                &port,
                "options:requested-chassis",
            ],
        ) == c3.id
    })
    .await;
    // 분산 유동 IP가 c3의 veth로
    wait_for("fq distributed on c3", AGENT_WAIT, || async {
        let v = c1.ctl_json(&["fip", "get", "fq"]).await;
        v["state"]["kind"] == "distributed" && v["state"]["node"] == "c3"
    })
    .await;
    let started = Instant::now();
    wait_for("fq inbound through c3", AGENT_WAIT, || async {
        ext(&["ip", "neigh", "flush", "all"]);
        let counts = captured(
            &[host_veth(1), host_veth(2), host_veth(3)],
            "icmp and dst host 203.0.113.120",
            || {
                ext_ping("203.0.113.120");
            },
        );
        counts[0] == 0 && counts[1] == 0 && counts[2] > 0
    })
    .await;
    eprintln!(
        "PROBE fq moved to c3's veth {:?} after the migration ended",
        started.elapsed()
    );
}

/// 5. 게이트웨이 넘김: 점잖은 멈춤(`systemctl stop ovn-controller`)과 프로세스 죽이기(`kill -9`), 각각 되돌아감
async fn failover_section(fleet: &Fleet) {
    let (c1, c2) = (fleet.node("c1"), fleet.node("c2"));
    let r1 = c1.ctl_json(&["router", "get", "r1"]).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let gw_mac = c1.ovn_bare(
        "ovn-nbctl",
        "nb",
        &["get", "Logical_Router_Port", &format!("opl-gw-{r1}"), "mac"],
    );
    assert_eq!(hosting(c1).await, "c2");

    // 점잖은 멈춤
    let (moved, reached) = failover(fleet, "systemctl stop ovn-controller on c2", "c3", || {
        c2.exec(&["systemctl", "stop", "ovn-controller"]);
    })
    .await;
    assert!(
        moved <= FAILOVER_LIMIT && reached <= FAILOVER_LIMIT,
        "{moved:?} {reached:?}"
    );
    // 물리 망(호스트 브리지)이 라우터 MAC을 c3 쪽에서 배움(GARP·응답)
    wait_for("the router MAC behind c3", AGENT_WAIT, || async {
        c1.ping("vma", EXT_IP);
        fdb_port(&gw_mac) == "oplv3"
    })
    .await;
    // 되돌아감: c2가 돌아오면 OVN이 곧바로 그쪽으로
    let (moved, reached) = failover(
        fleet,
        "systemctl start ovn-controller on c2 (failback)",
        "c2",
        || {
            c2.exec(&["systemctl", "start", "ovn-controller"]);
        },
    )
    .await;
    assert!(
        moved <= AGENT_WAIT && reached <= AGENT_WAIT,
        "{moved:?} {reached:?}"
    );

    // 프로세스 죽이기: systemd가 곧바로 다시 켜지 않게 잠시 Restart=no를 얹는다.
    let units = ["ovs-vswitchd.service", "ovn-controller.service"];
    for unit in units {
        n_show(c2, &["systemctl", "show", "-p", "Restart", unit]);
        let dir = format!("/run/systemd/system/{unit}.d");
        c2.exec(&["mkdir", "-p", &dir]);
        c2.exec(&[
            "sh",
            "-c",
            &format!("printf '[Service]\\nRestart=no\\n' > {dir}/opl-test.conf"),
        ]);
    }
    c2.exec(&["systemctl", "daemon-reload"]);
    let (moved, reached) = failover(
        fleet,
        "kill -9 ovs-vswitchd and ovn-controller on c2",
        "c3",
        || {
            c2.exec(&["pkill", "-9", "-x", "ovs-vswitchd"]);
            c2.exec(&["pkill", "-9", "-x", "ovn-controller"]);
        },
    )
    .await;
    assert!(
        moved <= FAILOVER_LIMIT && reached <= FAILOVER_LIMIT,
        "{moved:?} {reached:?}"
    );
    for unit in units {
        c2.exec(&["rm", "-rf", &format!("/run/systemd/system/{unit}.d")]);
    }
    c2.exec(&["systemctl", "daemon-reload"]);
    let _ = c2.try_exec(&["systemctl", "reset-failed"]);
    let (moved, reached) = failover(
        fleet,
        "restart ovs-vswitchd and ovn-controller on c2 (failback)",
        "c2",
        || {
            c2.exec(&["systemctl", "start", "ovs-vswitchd", "ovn-controller"]);
        },
    )
    .await;
    assert!(
        moved <= AGENT_WAIT && reached <= AGENT_WAIT,
        "{moved:?} {reached:?}"
    );
}

/// 6. 노드 통째 죽이기(`docker kill c2` — Orbivisor 투표 멤버·OVN 중앙·게이트웨이 주인이 함께): c3로 넘기고, 남은 둘로
///    Orbivisor Raft·OVN NB/SB가 쓰기를 이어 새 유동 IP가 닿는다. 그 뒤 `sdn central remove c2`가 살아 있는 중앙의 실제
///    `cluster/kick`으로 c2를 NB·SB에서 내쫓는다.
async fn dead_node_section(fleet: &Fleet) {
    let (c1, c2, c3) = (fleet.node("c1"), fleet.node("c2"), fleet.node("c3"));
    assert_eq!(hosting(c1).await, "c2");
    let (moved, reached) = failover(fleet, "docker kill c2", "c3", || {
        run("docker", &["kill", &c2.container]);
    })
    .await;
    assert!(
        moved <= FAILOVER_LIMIT && reached <= FAILOVER_LIMIT,
        "{moved:?} {reached:?}"
    );

    // 남은 둘의 정족수: Orbivisor 쓰기 → OvnSync → NB → 새 유동 IP(꺼진 VM ns-a라 중앙 — c3)가 닿는다.
    let started = Instant::now();
    c1.ctl_ok(&[
        "fip",
        "create",
        "--network",
        "ext",
        "--ip",
        "203.0.113.121",
        "--name",
        "fa",
        "--attach",
        "ns-a",
    ])
    .await;
    wait_for("a ping to the new floating IP", AGENT_WAIT, || async {
        ext_ping("203.0.113.121")
    })
    .await;
    eprintln!(
        "PROBE new floating IP reachable {:?} after the write with c2 dead",
        started.elapsed()
    );
    for db in ["nb", "sb"] {
        let leaders = [c1, c3]
            .iter()
            .filter(|n| n.cluster_status(db).is_leader())
            .count();
        assert_eq!(leaders, 1, "{db}: no leader among c1 and c3");
    }

    // 죽은 중앙 내쫓기
    wait_for("c2 offline", AGENT_WAIT, || async {
        let view = c1.ctl_json(&["cluster", "status"]).await;
        view["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "c2" && m["online"] == false)
    })
    .await;
    let said = c1.ctl_ok(&["sdn", "central", "remove", "c2"]).await;
    eprintln!("sdn central remove c2: {said}");
    let tasks = c1.ctl_json(&["task", "list"]).await;
    let kick = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["spec"]["op"]["kind"] == "sdn-central-kick")
        .cloned()
        .unwrap_or_else(|| panic!("no sdn-central-kick task: {tasks}"));
    eprintln!("kick task: {kick}");
    assert_eq!(kick["status"]["state"], "succeeded", "{kick}");
    for n in [c1, c3] {
        for db in ["nb", "sb"] {
            let status = n.cluster_status(db);
            assert_eq!(status.servers.len(), 2, "{} {db}: {status:?}", n.name);
            assert!(
                !status.servers.iter().any(|s| s.address.contains(c2.ip)),
                "{status:?}"
            );
        }
    }
    let view = c1.ctl_json(&["sdn", "status"]).await;
    assert_eq!(view["centrals"].as_array().unwrap().len(), 2, "{view}");
    // 쓰기는 여전히 된다(유동 IP 떼기 → SNAT로).
    c1.ctl_ok(&["fip", "detach", "fa"]).await;
    wait_for("fa unreachable after the detach", AGENT_WAIT, || async {
        !ext_ping("203.0.113.121")
    })
    .await;
}
