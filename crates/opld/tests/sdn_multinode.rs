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

    /// 이 중앙의 `ovn-nbctl`·`ovn-sbctl --no-leader-only --bare …`(실패하면 빈 글, 따옴표는 뗀다)
    fn ovn_bare(&self, program: &str, db: &str, args: &[&str]) -> String {
        let socket = format!("--db=unix:/run/opl/ovn/{db}.sock");
        // 팔로어의 로컬 소켓에는 leader-only(기본) 클라이언트가 붙지 못한다(계획 67 스테이지 6에서 확인) — 읽기만 하므로
        // 어느 중앙에서든 읽는다.
        let mut all = vec![program, socket.as_str(), "--no-leader-only", "--bare"];
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
                    &[
                        "ovn-sbctl",
                        "--db=unix:/run/opl/ovn/sb.sock",
                        "--no-leader-only",
                        "show",
                    ],
                    &[
                        "ovn-nbctl",
                        "--db=unix:/run/opl/ovn/nb.sock",
                        "--no-leader-only",
                        "show",
                    ],
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
        Self::start_for(prefix, dev, filter, CAPTURE_SECS)
    }

    /// `secs`초 동안 잡는 캡처
    fn start_for(prefix: &[String], dev: &str, filter: &str, secs: &str) -> Self {
        let mut argv: Vec<String> = prefix.to_vec();
        argv.extend(
            ["timeout", secs, "tcpdump", "-n", "-l", "-i", dev, filter]
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

/// [`captured`]를 `ok`(보여야 하는 곳)가 맞을 때까지 [`CAPTURE_ATTEMPTS`]번까지 다시 하고 마지막 수를 돌려준다(호출자가
/// 그 수로 단언한다). `zero`의 자리(보이면 안 되는 곳)는 시도마다 본다([`capture_verdict`]).
fn captured_until(
    points: &[(Vec<String>, &str)],
    filter: &str,
    action: impl Fn(),
    zero: &[usize],
    ok: impl Fn(&[usize]) -> bool,
) -> Vec<usize> {
    let mut counts = Vec::new();
    for attempt in 1..=CAPTURE_ATTEMPTS {
        counts = captured(points, filter, &action);
        match capture_verdict(&counts, zero, &ok) {
            Ok(true) => break,
            Ok(false) => eprintln!(
                "capture attempt {attempt}/{CAPTURE_ATTEMPTS} [{filter}]: {counts:?}; again"
            ),
            Err(e) => panic!("capture attempt {attempt}/{CAPTURE_ATTEMPTS} [{filter}]: {e}"),
        }
    }
    counts
}

/// 캡처 한 번의 판정: `zero`의 자리(보이면 안 되는 곳)가 하나라도 0이 아니면 곧바로 오류 — 다시 보지 않는다(리뷰 2 m-3:
/// 세 번 가운데 한 번 새는 진짜 버그를 다시 보기가 가리지 않게). 아니면 `ok`(보여야 하는 곳이 보였는지 — 거짓이면
/// tcpdump가 놓친 것일 수 있어 다시 본다).
fn capture_verdict(
    counts: &[usize],
    zero: &[usize],
    ok: impl Fn(&[usize]) -> bool,
) -> Result<bool, String> {
    if let Some(i) = zero.iter().find(|i| counts[**i] != 0) {
        return Err(format!("point {i} must see nothing but saw {counts:?}"));
    }
    Ok(ok(counts))
}

#[test]
fn a_leak_in_any_capture_attempt_fails() {
    assert_eq!(capture_verdict(&[0, 3], &[0], |c| c[1] > 0), Ok(true));
    assert_eq!(capture_verdict(&[0, 0], &[0], |c| c[1] > 0), Ok(false));
    assert!(capture_verdict(&[1, 0], &[0], |c| c[1] > 0).is_err());
    assert!(capture_verdict(&[0, 5, 2], &[0, 2], |c| c[1] > 0).is_err());
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
    for m in ["openvswitch", "geneve", "vxlan"] {
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
    // 계획 67 스테이지 6
    let hook = Hook::start();
    Box::pin(security_group_section(&fleet)).await;
    Box::pin(alerts_setup(&fleet, &hook)).await;
    Box::pin(gateway_candidates_section(&fleet, &hook)).await;
    Box::pin(lb_section(&fleet, &hook)).await;
    Box::pin(vxlan_migration_section(&fleet)).await;
    Box::pin(lockdown_section(&fleet)).await;
    Box::pin(failover_section(&fleet)).await;
    Box::pin(dead_node_section(&fleet, &hook)).await;
    Box::pin(removed_node_section(&fleet)).await;
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

    // OVN 접근 잠금(계획 67 §10.4): 새 클러스터는 잠금을 켠 채 서고, 잠금 중 더한 중앙도 잠근 모양으로 합류한다(합류 전에
    // 다른 중앙의 허용 목록에 든다). 모든 중앙이 6645에 듣고 모든 노드에 허용 목록 표가 있다.
    for n in &fleet.nodes {
        wait_for(
            &format!("the lockdown on {}", n.name),
            AGENT_WAIT,
            || async { locked_shape(n) },
        )
        .await;
    }
    n_show(fleet.node("c1"), &["nft", "list", "ruleset"]);
}

/// 이 중앙이 잠근 모양인지: SB가 6645에 듣고 허용 목록 표가 있다.
fn locked_shape(n: &Node) -> bool {
    let (_, ss, _) = n.try_exec(&["ss", "-ltnH"]);
    let (_, tables, _) = n.try_exec(&["nft", "list", "tables"]);
    ss.contains(&format!("{}:6645", n.ip)) && tables.contains("inet opl_ovn_")
}

/// 잠금이 풀린 모양인지: 6645·허용 목록 표가 없다.
fn unlocked_shape(n: &Node) -> bool {
    let (_, ss, _) = n.try_exec(&["ss", "-ltnH"]);
    let (_, tables, _) = n.try_exec(&["nft", "list", "tables"]);
    !ss.contains(":6645") && !tables.contains("inet opl_ovn_")
}

/// 노드 `n` 안 이름공간 `ns`에서 0.2초마다 `to`로 ping을 띄운다(`stop_ping`이 멈추고 (보냄, 받음)을 센다).
fn start_ping(n: &Node, ns: &str, to: &str) -> Child {
    Command::new("docker")
        .args([
            "exec",
            &n.container,
            "ip",
            "netns",
            "exec",
            ns,
            "ping",
            "-i",
            "0.2",
            "-W",
            "1",
            to,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn stop_ping(n: &Node, to: &str, child: Child) -> (u64, u64) {
    n.try_exec(&["pkill", "-INT", "-f", &format!("ping -i 0.2 -W 1 {to}")]);
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text
        .lines()
        .find(|l| l.contains("packets transmitted"))
        .unwrap_or_else(|| panic!("no ping summary: {text}"));
    let nums: Vec<u64> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(2)
        .map(|s| s.parse().unwrap())
        .collect();
    eprintln!("ping {to}: {line}");
    (nums[0], nums[1])
}

/// 이름공간 `ns`의 장치 `dev`의 IPv4 주소
fn ns_ipv4(n: &Node, ns: &str, dev: &str) -> String {
    let out = n.exec(&["ip", "-n", ns, "-4", "-br", "addr", "show", dev]);
    out.split_whitespace()
        .nth(2)
        .and_then(|a| a.split('/').next())
        .unwrap_or_else(|| panic!("no address on {dev}: {out}"))
        .to_string()
}

/// 4b. 돌고 있는 중앙 셋의 잠금 끄기·켜기(계획 67 §10.4): 중앙을 하나씩 바꾸는 동안 c1 VM → c3 VM 트래픽(Geneve)이 하나도
/// 버려지지 않고, 그동안 OvnSync 쓰기(새 네트워크)가 NB에 닿는다. 끄면 6645·표가 없고 켜면 다시 생긴다. 걸린 시간은 PROBE.
async fn lockdown_section(fleet: &Fleet) {
    let (c1, c3) = (fleet.node("c1"), fleet.node("c3"));
    let to = ns_ipv4(c3, "vmb", "oplnsb");
    assert!(c1.ping("vma", &to));
    for (state, on) in [("off", false), ("on", true)] {
        let ping = start_ping(c1, "vma", &to);
        let started = Instant::now();
        let name = format!("lk-{state}");
        let said = c1.ctl_ok(&["sdn", "lockdown", state, "--no-wait"]).await;
        eprintln!("{said}");
        let id = said
            .split_whitespace()
            .find(|w| w.len() == 36 && w.contains('-'))
            .unwrap()
            .to_string();
        // 바꾸는 동안의 OvnSync 쓰기
        c1.ctl_ok(&[
            "network",
            "create",
            &name,
            "--sdn",
            "--subnet",
            "10.62.9.0/24",
        ])
        .await;
        let task = loop {
            let task = c1.ctl_json(&["task", "get", &id]).await;
            let state = task["status"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state == "succeeded" || state == "failed" {
                break task;
            }
            assert!(started.elapsed() < Duration::from_secs(600), "{task}");
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let took = started.elapsed();
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        wait_for(&format!("the switch of {name}"), AGENT_WAIT, || async {
            c1.ovn_bare(
                "ovn-nbctl",
                "nb",
                &["--columns=external_ids", "list", "Logical_Switch"],
            )
            .contains(&name)
        })
        .await;
        // 여러 차례의 ping이 더 지나가게 잠깐 둔다.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (sent, received) = stop_ping(c1, &to, ping);
        eprintln!(
            "PROBE lockdown {state} on three centrals took {took:?}; ping c1 -> c3 {received}/{sent}"
        );
        assert!(sent >= 10, "{sent}");
        assert_eq!(
            sent, received,
            "VM traffic was lost while switching the lockdown {state}"
        );
        for n in &fleet.nodes {
            let ok = if on {
                locked_shape(n)
            } else {
                unlocked_shape(n)
            };
            assert!(ok, "{} after lockdown {state}", n.name);
        }
        c1.ctl_ok(&["network", "delete", &name]).await;
    }
    for db in ["nb", "sb"] {
        for n in &fleet.nodes {
            assert!(n.cluster_status(db).is_member(), "{} {db}", n.name);
        }
    }
}

/// 노드 `from` 안에서 그 노드 인증서로 `to`의 `port`에 `ovsdb-client list-dbs`(성공 여부)
fn ovsdb_from(from: &Node, to: &str, port: u16) -> bool {
    ovsdb_within(from, to, port, 3)
}

/// [`ovsdb_from`]의 시간 한도를 `secs`초로
fn ovsdb_within(from: &Node, to: &str, port: u16, secs: u32) -> bool {
    let timeout = format!("--timeout={secs}");
    let (ok, out, err) = from.try_exec(&[
        "ovsdb-client",
        &timeout,
        "--private-key=/etc/opl/ovn/node.key",
        "--certificate=/etc/opl/ovn/node.crt",
        "--ca-cert=/etc/opl/ovn/ca.crt",
        "list-dbs",
        &format!("ssl:{to}:{port}"),
    ]);
    if !ok {
        eprintln!("{} -> {to}:{port}: {}", from.name, err.trim());
    }
    ok && out.contains("OVN_")
}

/// 7. 클러스터에서 지운 노드 끊기(계획 67 §10.3): 죽인 c2(이미 중앙에서 내쫓음)를 orbivisord 없이 다시 띄우면 노드
///    인증서로 c1·c3의 NB·SB에 붙고(아직 멤버) 6643~6645는 막힌다(중앙이 아님). `cluster remove-node c2` 뒤 몇 초 안에
///    c2의 인증서로는 c1·c3의 6641·6642에 붙지 못하고, 중앙 에이전트가 SB에서 c2의 chassis를 지운다. c1·c3는 그대로다.
async fn removed_node_section(fleet: &Fleet) {
    let (c1, c2, c3) = (fleet.node("c1"), fleet.node("c2"), fleet.node("c3"));
    run("docker", &["start", &c2.container]);
    wait_for("systemd in c2 again", AGENT_WAIT, || async {
        let (_, out, _) = c2.try_exec(&["systemctl", "is-system-running"]);
        matches!(out.trim(), "running" | "degraded")
    })
    .await;
    wait_for(
        "c2's certificate to reach the NB and SB",
        AGENT_WAIT,
        || async {
            [c1, c3]
                .iter()
                .all(|n| ovsdb_from(c2, n.ip, 6641) && ovsdb_from(c2, n.ip, 6642))
        },
    )
    .await;
    for port in [6643, 6644, 6645] {
        assert!(!ovsdb_from(c2, c1.ip, port), "c2 reached c1:{port}");
    }
    let started = Instant::now();
    c1.ctl_ok(&["cluster", "remove-node", "c2", "--force"])
        .await;
    // 끊긴 때는 한 곳(c1 NB)을 1초 한도로 거듭 붙어 보며 잰다(붙으면 곧바로 성공, 막히면 1초 시간 초과).
    let mut cut = None;
    while started.elapsed() < Duration::from_secs(30) {
        if !ovsdb_within(c2, c1.ip, 6641, 1) {
            cut = Some(started.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let cut = cut.expect("the removed node still reaches the OVN databases");
    eprintln!("PROBE removed node c2 cut off from NB/SB {cut:?} after the removal");
    assert!(cut <= Duration::from_secs(5), "{cut:?}");
    for n in [c1, c3] {
        for port in [6641, 6642] {
            assert!(
                !ovsdb_from(c2, n.ip, port),
                "c2 still reaches {}:{port}",
                n.name
            );
        }
    }
    // 스테이지 4가 본 "database connection failed ()"(c1의 로컬 SB 소켓): ovn-sbctl 기본(leader-only)은 팔로어의 소켓 하나로는
    // 붙지 못한다. 역할과 두 읽기를 남긴다.
    for n in [c1, c3] {
        let role = n.cluster_status("sb").role;
        let socket = "--db=unix:/run/opl/ovn/sb.sock";
        let leader_only = n.try_exec(&[
            "ovn-sbctl",
            socket,
            "--bare",
            "--columns=name",
            "list",
            "Chassis",
        ]);
        let any = n.try_exec(&[
            "ovn-sbctl",
            socket,
            "--no-leader-only",
            "--bare",
            "--columns=name",
            "list",
            "Chassis",
        ]);
        eprintln!(
            "PROBE {} SB role {role:?}: leader-only read ok={} ({}), --no-leader-only read ok={}",
            n.name,
            leader_only.0,
            leader_only.2.trim(),
            any.0
        );
        assert!(any.0, "{} SB local socket: {}", n.name, any.2);
    }
    wait_for("c2's chassis to leave the SB", AGENT_WAIT, || async {
        // 내쫓은 뒤 c1의 SB는 팔로어이거나 로컬 소켓이 답하지 않을 수 있다(탐침에서 본 "database connection failed",
        // §4의 leader-only) — 남은 중앙 어느 쪽에서든 leader-only 없이 읽는다.
        [c1, c3].iter().any(|n| {
            let names = n.ovn_bare(
                "ovn-sbctl",
                "sb",
                &["--no-leader-only", "--columns=name", "list", "Chassis"],
            );
            !names.is_empty() && !names.contains(&c2.id)
        })
    })
    .await;
    // 남은 둘은 그대로다.
    let to = ns_ipv4(c3, "vmb", "oplnsb");
    wait_for("c1 -> c3 after the removal", AGENT_WAIT, || async {
        c1.ping("vma", &to)
    })
    .await;
    assert!(ovsdb_from(c3, c1.ip, 6641) && ovsdb_from(c1, c3.ip, 6642));
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
        &[0],
        |c| c[1] > 0,
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
        &[0, 2],
        |c| c[1] > 0,
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
        &[],
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
async fn dead_node_section(fleet: &Fleet, hook: &Hook) {
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

    // 중앙 하나를 잃으면 `sdn_central_down`(계획 67 §11, 남은 둘의 리더가 평가)
    hook.wait_alert("sdn-central-down", "firing", "c2", Duration::from_secs(150))
        .await;

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

// ---- 계획 67 스테이지 6: 여러 chassis 보안 그룹·부하 분산·QoS·옮기기 다리·SDN 경보 ----

/// 붙어 한 줄을 받는 탐침(받은 줄, 또는 `refused`·`timeout`·`error …`)
const HIT: &str = r#"
import socket, sys
try:
    s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=2)
    s.settimeout(2); print(s.recv(200).decode().strip())
except ConnectionRefusedError: print("refused")
except socket.timeout: print("timeout")
except OSError as e: print("error", e)
"#;

/// 붙은 손님에게 `이름 손님주소`를 보내는 서버
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

/// 노드 `n`의 이름공간 `ns`에 이름 서버(`name`, 포트 `port`)를 띄운다(`docker exec -d` — [`stop_server`]가 끈다).
fn start_server(n: &Node, ns: &str, name: &str, port: u16) {
    n.exec(&[
        "sh",
        "-c",
        &format!(
            "nohup ip netns exec {ns} python3 -c '{NAME_SERVER}' {name} {port} >/dev/null 2>&1 &"
        ),
    ]);
}

fn stop_server(n: &Node, name: &str, port: u16) {
    let _ = n.try_exec(&["pkill", "-f", &format!("{name} {port}$")]);
}

/// 노드 `n`의 이름공간 `ns`에서 `to:port`에 붙어 받은 줄
fn hit_from(n: &Node, ns: &str, to: &str, port: u16) -> String {
    let p = port.to_string();
    let (_, out, _) = n.try_exec(&["ip", "netns", "exec", ns, "python3", "-c", HIT, to, &p]);
    out.trim().to_string()
}

/// 호스트 이름공간 `ext`(물리 망)에서 `to:port`에 붙어 받은 줄
fn ext_hit(to: &str, port: u16) -> String {
    let p = port.to_string();
    let (_, out, _) = try_run(
        "ip",
        &["netns", "exec", EXT_NS, "python3", "-c", HIT, to, &p],
    );
    out.trim().to_string()
}

/// 경보를 받는 webhook(호스트의 작은 HTTP 서버, 도커 망 게이트웨이 172.30.66.1에서 컨테이너가 닿는다). 받은 JSON 본문을
/// 모은다.
struct Hook {
    port: u16,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl Hook {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let s = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || hook_request(stream, &s));
            }
        });
        Self { port, seen }
    }

    fn url(&self) -> String {
        format!("http://172.30.66.1:{}/alerts", self.port)
    }

    /// `pred`에 맞는 알림이 올 때까지 기다린다(이미 온 것도 본다). 그 알림과 걸린 시간
    async fn wait(
        &self,
        what: &str,
        limit: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> (Value, Duration) {
        let started = Instant::now();
        loop {
            if let Some(v) = self.seen.lock().unwrap().iter().find(|v| pred(v)) {
                return (v.clone(), started.elapsed());
            }
            if started.elapsed() > limit {
                let all = self.seen.lock().unwrap().clone();
                panic!("no {what} notification in {limit:?}; got {all:#?}");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// 규칙 `rule`의 `state`(firing·resolved·event) 알림 가운데 대상 이름이 `subject`인 것
    async fn wait_alert(
        &self,
        rule: &str,
        state: &str,
        subject: &str,
        limit: Duration,
    ) -> (Value, Duration) {
        let what = format!("{rule} {state} on {subject}");
        let (v, took) = self
            .wait(&what, limit, |v| {
                v["rule"] == rule && v["state"] == state && v["subject"]["name"] == subject
            })
            .await;
        eprintln!("PROBE alert {what} after {took:?}: {}", v["detail"]);
        (v, took)
    }
}

/// webhook 요청 하나: 머리와 `Content-Length`만큼의 본문을 읽고 200으로 답한다.
fn hook_request(stream: std::net::TcpStream, seen: &Mutex<Vec<Value>>) {
    use std::io::{BufRead, Read, Write};
    let mut reader = std::io::BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    if let Ok(v) = serde_json::from_slice::<Value>(&body) {
        seen.lock().unwrap().push(v);
    }
    let mut stream = stream;
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

/// 6a. 여러 chassis에 걸친 보안 그룹(계획 66 §14 10번): c1의 vma → c3의 vmb(ns-b)에 그룹을 걸면 c3 쪽 `to-lport` ACL이
///     c1에서 온 Geneve 트래픽을 그룹대로 거른다(tcp 22만 허용 → ping 막힘, icmp 허용을 더하면 됨). 물리 망 `ext`에서
///     유동 IP(중앙 — c2 게이트웨이)로 들어오는 것도 그룹대로다.
async fn security_group_section(fleet: &Fleet) {
    let (c1, c3) = (fleet.node("c1"), fleet.node("c3"));
    let b_ip = ns_ipv4(c3, "vmb", "oplnsb");
    start_server(c3, "vmb", "sg-b", 22);
    assert!(c1.ping("vma", &b_ip));
    c1.ctl_ok(&[
        "security-group",
        "create",
        "sg-ssh",
        "--rule",
        "in accept tcp 22",
    ])
    .await;
    c1.ctl_ok(&[
        "security-group",
        "create",
        "sg-icmp",
        "--rule",
        "in accept icmp",
    ])
    .await;
    let started = Instant::now();
    c1.ctl_ok(&["vm", "nic-sg", "ns-b", "0", "sg-ssh"]).await;
    wait_for("ping c1 -> c3 blocked by sg-ssh", AGENT_WAIT, || async {
        !c1.ping("vma", &b_ip)
    })
    .await;
    eprintln!(
        "PROBE cross-chassis security group: ping blocked {:?} after nic-sg",
        started.elapsed()
    );
    let ssh = hit_from(c1, "vma", &b_ip, 22);
    assert!(ssh.starts_with("sg-b "), "tcp 22 across chassis: {ssh}");
    c1.ctl_ok(&["vm", "nic-sg", "ns-b", "0", "sg-ssh", "sg-icmp"])
        .await;
    wait_for("ping c1 -> c3 allowed by sg-icmp", AGENT_WAIT, || async {
        c1.ping("vma", &b_ip)
    })
    .await;
    // 바깥에서 유동 IP로: icmp 허용이면 닿고, 빼면 막히고 tcp 22는 된다.
    c1.ctl_ok(&[
        "fip",
        "create",
        "--network",
        "ext",
        "--ip",
        "203.0.113.122",
        "--name",
        "fb",
        "--attach",
        "ns-b",
    ])
    .await;
    wait_for("ext -> fb ping", AGENT_WAIT, || async {
        ext_ping("203.0.113.122")
    })
    .await;
    c1.ctl_ok(&["vm", "nic-sg", "ns-b", "0", "sg-ssh"]).await;
    wait_for("ext -> fb ping blocked", AGENT_WAIT, || async {
        !ext_ping("203.0.113.122")
    })
    .await;
    let ssh = ext_hit("203.0.113.122", 22);
    assert!(
        ssh.starts_with("sg-b 203.0.113.1"),
        "ext -> fb tcp 22: {ssh}"
    );
    // 치우기(뒤 절이 vmb에 ping한다)
    c1.ctl_ok(&["vm", "nic-sg", "ns-b", "0"]).await;
    c1.ctl_ok(&["fip", "delete", "fb"]).await;
    for g in ["sg-ssh", "sg-icmp"] {
        c1.ctl_ok(&["security-group", "delete", g]).await;
    }
    stop_server(c3, "sg-b", 22);
    wait_for("ping c1 -> c3 without groups", AGENT_WAIT, || async {
        c1.ping("vma", &b_ip)
    })
    .await;
}

/// 6b. SDN 경보(계획 67 §11): 호스트 webhook을 알림 대상으로, "SDN 경보 켜기"(`alert rule add-sdn-defaults`)로 권하는 여덟
///     규칙을 만들고 시험이 기다리지 않게 지속 시간을 5초로 줄인다.
async fn alerts_setup(fleet: &Fleet, hook: &Hook) {
    let c1 = fleet.node("c1");
    c1.ctl_ok(&[
        "notify",
        "target",
        "create",
        "hook",
        "webhook",
        "--url",
        &hook.url(),
    ])
    .await;
    let said = c1
        .ctl_ok(&["alert", "rule", "add-sdn-defaults", "--target", "hook"])
        .await;
    eprintln!("{said}");
    assert_eq!(said.matches(" created").count(), 8, "{said}");
    for rule in [
        "sdn-central-down",
        "sdn-chassis-down",
        "sdn-gateway-unavailable",
        "sdn-lb-member-down",
    ] {
        c1.ctl_ok(&["alert", "rule", "set", rule, "--for", "5"])
            .await;
    }
    println!("{}", c1.ctl_ok(&["alert", "rule", "list"]).await);
}

/// 6c. 게이트웨이 후보를 모두 빼면(`node sdn --no-gateway`) 라우터 r1에 `sdn_gateway_unavailable`이 뜨고, 되돌리면 c2가 다시
///     주인이 되고 해제가 온다.
async fn gateway_candidates_section(fleet: &Fleet, hook: &Hook) {
    let c1 = fleet.node("c1");
    for n in ["c2", "c3"] {
        c1.ctl_ok(&["node", "sdn", n, "--no-gateway"]).await;
    }
    let (v, _) = hook
        .wait_alert("sdn-gateway-unavailable", "firing", "r1", AGENT_WAIT)
        .await;
    assert!(
        v["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("no gateway node has physnet ext ready"),
        "{v}"
    );
    c1.ctl_ok(&["node", "sdn", "c2", "--gateway", "200"]).await;
    c1.ctl_ok(&["node", "sdn", "c3", "--gateway", "100"]).await;
    wait_for("r1 hosted on c2 again", AGENT_WAIT, || async {
        hosting(c1).await == "c2"
    })
    .await;
    // 해제는 거짓인 채 60초 뒤
    hook.wait_alert(
        "sdn-gateway-unavailable",
        "resolved",
        "r1",
        Duration::from_secs(180),
    )
    .await;
    wait_for("SNAT after the gateways are back", AGENT_WAIT, || async {
        c1.ping("vma", EXT_IP)
    })
    .await;
}

/// 6d. 여러 chassis 부하 분산(§14 2): 멤버 vma(c1)·vmb(c3), 손님 vmc(c2)와 물리 망 `ext`(바깥 VIP — 게이트웨이 c2). 멤버
///     듣기를 멈추면 `sdn_lb_member_down`. 게이트웨이를 넘기면(c2의 ovn-controller 멈춤) 바깥 VIP가 다시 닿는 시간(§7.4 GARP
///     탐침 — `ext`의 ARP 캐시를 비우지 않음)과 `sdn_gateway_failover`·`sdn_chassis_down`. 끝에 NIC QoS를 Geneve 너머에서
///     잰다(§14 3).
async fn lb_section(fleet: &Fleet, hook: &Hook) {
    let (c1, c2, c3) = (fleet.node("c1"), fleet.node("c2"), fleet.node("c3"));
    let client = api_vm(c1, "ns-c", "t2").await;
    namespace_vm(c2, "vmc", "oplnsc", &client, "10.61.2.1");
    start_server(c1, "vma", "lb-a", 8080);
    start_server(c3, "vmb", "lb-b", 8080);
    c1.ctl_ok(&[
        "lb",
        "create",
        "web",
        "--network",
        "t1",
        "--vip",
        "10.61.1.50",
        "--external",
        "ext,ip=203.0.113.130",
        "--listener",
        "tcp/80",
    ])
    .await;
    for m in ["ns-a@8080", "ns-b@8080"] {
        c1.ctl_ok(&["lb", "member", "add", "web", "tcp/80", m])
            .await;
    }
    c1.ctl_ok(&[
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
    .await;
    wait_for("both members online", AGENT_WAIT, || async {
        let lb = c1.ctl_json(&["lb", "get", "web"]).await;
        lb["members"]
            .as_array()
            .is_some_and(|m| m.len() == 2 && m.iter().all(|m| m["health"] == "online"))
    })
    .await;
    let spread = |n: &Node, ns: &str, to: &str, times: usize| {
        let mut m = std::collections::BTreeMap::<String, usize>::new();
        for _ in 0..times {
            let got = hit_from(n, ns, to, 80);
            *m.entry(got.split(' ').next().unwrap_or_default().to_string())
                .or_default() += 1;
        }
        m
    };
    wait_for("the VIP from c2", AGENT_WAIT, || async {
        hit_from(c2, "vmc", "10.61.1.50", 80).starts_with("lb-")
    })
    .await;
    let seen = spread(c2, "vmc", "10.61.1.50", 20);
    eprintln!("PROBE multi-chassis LB spread from c2: {seen:?}");
    assert!(
        seen.contains_key("lb-a") && seen.contains_key("lb-b"),
        "{seen:?}"
    );
    wait_for("the external VIP", AGENT_WAIT, || async {
        ext_hit("203.0.113.130", 80).starts_with("lb-")
    })
    .await;
    let outside = ext_hit("203.0.113.130", 80);
    assert!(outside.ends_with(" 203.0.113.1"), "{outside}");

    // 멤버 죽음: c3의 듣기를 멈춤 → 새 연결은 c1만, 경보
    stop_server(c3, "lb-b", 8080);
    let t0 = Instant::now();
    wait_for("only lb-a answers", AGENT_WAIT, || async {
        spread(c2, "vmc", "10.61.1.50", 6)
            .keys()
            .collect::<Vec<_>>()
            == ["lb-a"]
    })
    .await;
    eprintln!("PROBE LB member on c3 taken out after {:?}", t0.elapsed());
    let (v, _) = hook
        .wait_alert("sdn-lb-member-down", "firing", "web", AGENT_WAIT)
        .await;
    assert!(
        v["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("ns-b:8080"),
        "{v}"
    );
    start_server(c3, "vmb", "lb-b", 8080);
    wait_for("both members answer again", AGENT_WAIT, || async {
        spread(c2, "vmc", "10.61.1.50", 10).len() == 2
    })
    .await;

    // 게이트웨이 넘김 뒤 바깥 VIP(§7.4): `ext`의 이웃 표는 그대로 두고 다시 닿는 시간과 c3 veth의 VIP GARP를 잰다.
    assert_eq!(hosting(c1).await, "c2");
    assert!(ext_hit("203.0.113.130", 80).starts_with("lb-"));
    eprintln!(
        "ext neighbor of the VIP before: {}",
        ext_neigh("203.0.113.130")
    );
    let mut cap = Capture::start_for(&[], "oplv3", "arp and host 203.0.113.130", "45");
    cap.wait_listening();
    let t0 = Instant::now();
    c2.exec(&["systemctl", "stop", "ovn-controller"]);
    // 옛 게이트웨이의 흐름이 잠깐 남아 답할 수 있으므로 주인이 c3로 바뀐 뒤부터 잰다.
    wait_for("r1 hosted on c3", AGENT_WAIT, || async {
        hosting(c1).await == "c3"
    })
    .await;
    let moved = t0.elapsed();
    let mut reached = None;
    while t0.elapsed() < FAILOVER_LIMIT {
        if ext_hit("203.0.113.130", 80).starts_with("lb-") {
            reached = Some(t0.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let garp = cap.count();
    let reached = reached.expect("the external VIP did not come back after the gateway failover");
    eprintln!(
        "PROBE external VIP after the gateway failover: hosting c3 after {moved:?}, reachable again after \
         {reached:?} (ext ARP cache kept); ARP packets for the VIP on c3's veth: {garp}; ext neighbor {}",
        ext_neigh("203.0.113.130")
    );
    for _ in 0..5 {
        assert!(ext_hit("203.0.113.130", 80).starts_with("lb-"));
    }
    let (v, _) = hook
        .wait_alert("sdn-gateway-failover", "event", "r1", AGENT_WAIT)
        .await;
    assert!(
        v["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("from c2 to c3"),
        "{v}"
    );
    hook.wait_alert("sdn-chassis-down", "firing", "c2", AGENT_WAIT)
        .await;
    c2.exec(&["systemctl", "start", "ovn-controller"]);
    wait_for("r1 back on c2", AGENT_WAIT, || async {
        hosting(c1).await == "c2"
    })
    .await;
    wait_for(
        "the external VIP after the failback",
        AGENT_WAIT,
        || async { ext_hit("203.0.113.130", 80).starts_with("lb-") },
    )
    .await;

    // QoS(§14 3): c1의 vma가 보내는 속도를 50 Mbit/s로 — c3의 vmb가 받는 속도(Geneve 너머)
    let b_ip = ns_ipv4(c3, "vmb", "oplnsb");
    c3.exec(&["ip", "netns", "exec", "vmb", "iperf3", "-s", "-D"]);
    c1.ctl_ok(&["vm", "nic-qos", "ns-a", "0", "out=50"]).await;
    let mut measured = 0.0;
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        measured = iperf_from(c1, "vma", &b_ip);
        if (measured - 50.0).abs() <= 50.0 * 0.15 {
            break;
        }
    }
    eprintln!("PROBE QoS out_mbps=50 c1 -> c3 across Geneve: {measured:.1} Mbit/s");
    assert!((measured - 50.0).abs() <= 50.0 * 0.15, "{measured}");
    c1.ctl_ok(&["vm", "nic-qos", "ns-a", "0", "--clear"]).await;
    let _ = c3.try_exec(&["pkill", "-x", "iperf3"]);

    // 치우기
    c1.ctl_ok(&["lb", "delete", "web"]).await;
    stop_server(c1, "lb-a", 8080);
    stop_server(c3, "lb-b", 8080);
}

/// iperf3 클라이언트(노드 `n`의 이름공간 `ns` → `to`) — 받은 쪽 Mbit/s. 폴리싱 버스트를 빼려고 앞 3초를 뺀다.
fn iperf_from(n: &Node, ns: &str, to: &str) -> f64 {
    let (ok, out, err) = n.try_exec(&[
        "ip", "netns", "exec", ns, "iperf3", "-c", to, "-t", "6", "-O", "3", "-J",
    ]);
    if !ok {
        eprintln!("iperf3 {ns} -> {to}: {err}{out}");
        return 0.0;
    }
    serde_json::from_str::<Value>(&out).unwrap_or(Value::Null)["end"]["sum_received"]
        ["bits_per_second"]
        .as_f64()
        .unwrap_or(0.0)
        / 1e6
}

/// 노드 `n`의 이름공간 `ns`(없으면 만듦)를 Linux 브리지 `bridge`의 veth 한쪽(`dev`, MAC·주소)으로 둔다.
fn bridge_ns(n: &Node, ns: &str, dev: &str, bridge: &str, mac: &str, ip: &str) {
    let _ = n.try_exec(&["ip", "netns", "add", ns]);
    let _ = n.try_exec(&["ip", "netns", "exec", ns, "ip", "link", "del", dev]);
    let peer = format!("{dev}p");
    n.exec(&[
        "ip", "link", "add", dev, "type", "veth", "peer", "name", &peer,
    ]);
    n.exec(&["ip", "link", "set", &peer, "master", bridge, "up"]);
    n.exec(&["ip", "link", "set", dev, "netns", ns]);
    let in_ns = |args: &[&str]| {
        let mut all = vec!["ip", "netns", "exec", ns, "ip"];
        all.extend_from_slice(args);
        n.exec(&all);
    };
    in_ns(&["link", "set", dev, "address", mac]);
    in_ns(&["addr", "add", &format!("{ip}/24"), "dev", dev]);
    in_ns(&["link", "set", "lo", "up"]);
    in_ns(&["link", "set", dev, "up"]);
}

/// 노드 `n`의 이름공간 `ns`의 장치를 OVS 내부 포트(`dev`, 논리 포트 `port`)로 바꾼다 — "옮긴 뒤 다시 켠" VM.
fn ovs_ns(n: &Node, ns: &str, dev: &str, port: &str, mac: &str, ip: &str) {
    let _ = n.try_exec(&["ip", "netns", "exec", ns, "ip", "link", "del", dev]);
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
    in_ns(&["addr", "add", &format!("{ip}/24"), "dev", dev]);
    in_ns(&["link", "set", dev, "up"]);
}

/// 노드 `n`에서 OVS 포트를 지우고 이름공간 장치를 다시 Linux 브리지 veth로(되돌린 VM)
fn back_to_bridge(n: &Node, ns: &str, dev: &str, bridge: &str, mac: &str, ip: &str) {
    let _ = n.try_exec(&["ovs-vsctl", "--if-exists", "del-port", dev]);
    bridge_ns(n, ns, dev, bridge, mac, ip);
}

/// 노드 `n`의 Linux 브리지 `bridge`에 매달린 옮기기 다리 veth(`oplp…`) 이름(없으면 빈 글)
fn bridge_veth(n: &Node, bridge: &str) -> String {
    let (_, out, _) = n.try_exec(&["ip", "-j", "link", "show", "master", bridge]);
    serde_json::from_str::<Value>(&out)
        .unwrap_or(Value::Null)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["ifname"].as_str())
        .find(|name| name.starts_with("oplp"))
        .unwrap_or_default()
        .to_string()
}

/// tap이 붙은 곳(Linux 브리지 이름 또는 `br-int`)
fn tap_home(n: &Node, tap: &str) -> String {
    let (ok, out, _) = n.try_exec(&["ovs-vsctl", "port-to-br", tap]);
    if ok {
        return out.trim().to_string();
    }
    let (_, out, _) = n.try_exec(&["ip", "-j", "link", "show", "dev", tap]);
    serde_json::from_str::<Value>(&out).unwrap_or(Value::Null)[0]["master"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// 6e. VXLAN 원본의 옮기기(§14 4): VXLAN 단순 네트워크 `lanx`(c1·c2·c3의 `oplv4242`)의 c1 QEMU VM을 `live`로, c1 이름공간
///     VM을 `offline`으로 SDN `t-lanx`에 옮기면 다리 노드가 c1(옮길 VM이 가장 많음)이고, 옮긴 이름공간이 c3의 남은
///     이름공간에 닿는다(c1의 다리 veth를 지남). 모두 되돌린 뒤 `--bridge-node c2`로 다시 옮기면 다리가 c2에 놓이고 같은
///     ping이 c2의 veth를 지난다. 끝내기로 다리가 걷힌다.
async fn vxlan_migration_section(fleet: &Fleet) {
    let (c1, c2, c3) = (fleet.node("c1"), fleet.node("c2"), fleet.node("c3"));
    c1.ctl_ok(&["network", "create", "lanx", "--vxlan", "4242"])
        .await;
    c1.ctl_ok(&[
        "network",
        "create",
        "t-lanx",
        "--sdn",
        "--subnet",
        "10.66.5.0/24",
    ])
    .await;
    for n in &fleet.nodes {
        wait_for(&format!("oplv4242 on {}", n.name), AGENT_WAIT, || async {
            let me = c1.ctl_json(&["node", "get", n.name]).await;
            me["status"]["bridges"]
                .as_array()
                .is_some_and(|b| b.iter().any(|b| b["name"] == "oplv4242"))
        })
        .await;
    }
    let mut made = Vec::new();
    for name in ["mig-a", "mig-b"] {
        c1.ctl_ok(&["vm", "create", name, "--memory", "128", "--nic", "lanx"])
            .await;
        let vm = c1.ctl_json(&["vm", "get", name]).await;
        let id: uuid::Uuid = vm["id"].as_str().unwrap().parse().unwrap();
        let mac = vm["spec"]["nics"][0]["mac"].as_str().unwrap().to_string();
        made.push((id, mac));
    }
    let (a_id, a_mac) = made[0].clone();
    let (_, b_mac) = made[1].clone();
    let a_port = opl_types::sdn::port_name(a_id, a_mac.parse().unwrap());
    bridge_ns(c1, "mxa", "mxa0", "oplv4242", &a_mac, "10.66.5.11");
    bridge_ns(c3, "mxb", "mxb0", "oplv4242", &b_mac, "10.66.5.12");
    wait_for("the VXLAN source L2 c1 -> c3", AGENT_WAIT, || async {
        c1.ping("mxa", "10.66.5.12")
    })
    .await;
    c1.ctl_ok(&[
        "vm", "create", "mig-q", "--memory", "256", "--nic", "lanx", "--node", "c1", "--start",
    ])
    .await;
    let q = c1.ctl_json(&["vm", "get", "mig-q"]).await;
    let q_tap = opl_types::network::tap_name(q["id"].as_str().unwrap().parse().unwrap(), 0);
    let pid = q["status"]["pid"].as_u64();
    assert_eq!(tap_home(c1, &q_tap), "oplv4242");

    let migrate = |name: &'static str, bridge: Option<&'static str>| {
        let mut args = vec![
            "network",
            "migrate",
            "lanx",
            "--to",
            "t-lanx",
            "--name",
            name,
            "--vm",
            "mig-q",
            "--vm",
            "mig-a",
            "--allow-mtu-shrink",
            "--ip",
            "mig-q=10.66.5.10",
            "--ip",
            "mig-a=10.66.5.11",
        ];
        if let Some(b) = bridge {
            args.extend(["--bridge-node", b]);
        }
        args
    };
    // 미리 보기: 다리 노드 c1, QEMU VM live
    let mut dry = vec!["-o", "json"];
    dry.extend(migrate("mx", None));
    dry.push("--dry-run");
    let plan: Value = serde_json::from_str(&c1.ctl_ok(&dry).await).unwrap();
    eprintln!("VXLAN migration plan: {plan}");
    assert_eq!(plan["bridge"]["needed"], true, "{plan}");
    assert_eq!(plan["bridge"]["node"], c1.id.as_str(), "{plan}");

    for (round, (name, bridge_node, via)) in [("mx", None, c1), ("mx2", Some("c2"), c2)]
        .into_iter()
        .enumerate()
    {
        let started = Instant::now();
        let said = c1.ctl_ok(&migrate(name, bridge_node)).await;
        eprintln!("network migrate {name} ({:?}): {said}", started.elapsed());
        let mig = c1.ctl_json(&["network", "migration", "get", name]).await;
        assert_eq!(mig["status"]["phase"], "moved", "{mig}");
        assert_eq!(mig["status"]["bridge"]["node"], via.id.as_str(), "{mig}");
        // QEMU VM: tap만 br-int로, 같은 QEMU
        assert_eq!(tap_home(c1, &q_tap), "br-int");
        let q = c1.ctl_json(&["vm", "get", "mig-q"]).await;
        assert_eq!(q["status"]["pid"].as_u64(), pid, "{q}");
        // 다리 veth가 다리 노드의 oplv4242에
        wait_for(
            &format!("the bridge veth on {}", via.name),
            AGENT_WAIT,
            || async { !bridge_veth(via, "oplv4242").is_empty() },
        )
        .await;
        let veth = bridge_veth(via, "oplv4242");
        // "다시 켠" mig-a: OVS 포트로. 다리 너머 c3의 남은 이름공간에 닿는다.
        ovs_ns(c1, "mxa", "mxa0", &a_port, &a_mac, "10.66.5.11");
        let t0 = Instant::now();
        wait_for(
            "moved (c1) -> remaining (c3) across the bridge",
            AGENT_WAIT,
            || async { c1.ping("mxa", "10.66.5.12") },
        )
        .await;
        eprintln!(
            "PROBE VXLAN migration bridge on {} (round {}): moved -> remaining ping after {:?}",
            via.name,
            round + 1,
            t0.elapsed()
        );
        assert!(c3.ping("mxb", "10.66.5.11"), "remaining -> moved");
        let counts = captured_until(
            &[(via.prefix(), veth.as_str())],
            "icmp and host 10.66.5.12",
            || {
                c1.try_exec(&[
                    "ip",
                    "netns",
                    "exec",
                    "mxa",
                    "ping",
                    "-c",
                    "5",
                    "-i",
                    "0.3",
                    "10.66.5.12",
                ]);
            },
            &[],
            |c| c[0] > 0,
        );
        assert!(
            counts[0] > 0,
            "the ping did not cross {}'s bridge veth",
            via.name
        );
        if round == 0 {
            // 모두 되돌린다(tap이 oplv4242로, mig-a 스펙도). 다리가 걷힌다.
            let said = c1.ctl_ok(&["network", "migration", "rollback", name]).await;
            eprintln!("rollback {name}: {said}");
            let mig = c1.ctl_json(&["network", "migration", "get", name]).await;
            assert_eq!(mig["status"]["phase"], "rolled-back", "{mig}");
            assert_eq!(tap_home(c1, &q_tap), "oplv4242");
            back_to_bridge(c1, "mxa", "mxa0", "oplv4242", &a_mac, "10.66.5.11");
            wait_for("the c1 bridge veth to go", AGENT_WAIT, || async {
                bridge_veth(c1, "oplv4242").is_empty()
            })
            .await;
            wait_for("the VXLAN source L2 again", AGENT_WAIT, || async {
                c1.ping("mxa", "10.66.5.12")
            })
            .await;
        } else {
            c1.ctl_ok(&["network", "migration", "finish", name]).await;
            let mig = c1.ctl_json(&["network", "migration", "get", name]).await;
            assert_eq!(mig["status"]["phase"], "finished", "{mig}");
            wait_for("the c2 bridge veth to go", AGENT_WAIT, || async {
                bridge_veth(c2, "oplv4242").is_empty()
            })
            .await;
            wait_for("no path across the removed bridge", AGENT_WAIT, || async {
                !c1.ping("mxa", "10.66.5.12")
            })
            .await;
        }
    }
    // 치우기(뒤 절의 노드 죽이기와 상관없게)
    c1.ctl_ok(&["vm", "stop", "mig-q", "--force"]).await;
    for vm in ["mig-q", "mig-a", "mig-b"] {
        c1.ctl_ok(&["vm", "delete", vm]).await;
    }
    let _ = c1.try_exec(&["ovs-vsctl", "--if-exists", "del-port", "mxa0"]);
    let _ = c1.try_exec(&["ip", "netns", "del", "mxa"]);
    let _ = c3.try_exec(&["ip", "netns", "del", "mxb"]);
    let _ = c3.try_exec(&["ip", "link", "del", "mxb0p"]);
}
