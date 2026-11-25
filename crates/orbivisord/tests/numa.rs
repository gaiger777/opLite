//! 실제 QEMU로 vCPU·에뮬레이터 스레드 고정과 hugepages를 본다(계획 64 스테이지 4, Linux). GitHub `linux-kvm`(root, KVM,
//! 4 vCPU, NUMA 노드 하나)에서 돈다: `cargo test -p orbivisord --features numa-it --test numa`
//!
//! 노드 설정은 `node resources`, VM의 CPU 배치는 `vm create|set --cpu-policy/--hugepages`(계획 64 스테이지 5)로 준다.
//! 스레드 친화는 `/proc/<pid>/task/<tid>/status`의 `Cpus_allowed_list`로 본다(vCPU 스레드는 `-name debug-threads=on`의
//! 이름 `CPU n/KVM`). 커널 일꾼(`kvm-nx-lpage-recovery`·`vhost-*` — `stat` 플래그 `PF_USER_WORKER` 등)은 커널이 친화를
//! 정하므로 보지 않는다(상한은 스코프 cpuset). hugepages는 root와 `OPL_TEST_NUMA_SYS=1`일 때만 실행 중 예약하고
//! (`host_resources.hugepages`) 끝에 원래 수로 되돌린다.
#![cfg(all(feature = "numa-it", target_os = "linux"))]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::{Daemon, eventually};
use orbivisorctl::Client;
use serde_json::{Value, json};

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

/// `/sys`에 써도 되는 시험(root와 `OPL_TEST_NUMA_SYS=1`)
fn sys_writable() -> bool {
    std::env::var_os("OPL_TEST_NUMA_SYS").is_some_and(|v| v == "1")
        && unsafe { libc::geteuid() } == 0
}

const NR_2M: &str = "/sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages";

/// 시험 앞의 2M hugepages 수로 되돌린다(Orbivisor는 설정에서 뺀 예약을 되돌리지 않는다).
struct RestoreHugepages(String);

impl Drop for RestoreHugepages {
    fn drop(&mut self) {
        if let Err(e) = std::fs::write(NR_2M, &self.0) {
            eprintln!("cannot restore {NR_2M}: {e}");
        }
    }
}

fn meminfo(key: &str) -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse().ok())
        .unwrap()
}

/// 스레드 이름 → 친화(`Cpus_allowed_list`). 같은 이름이 여럿이면 tid를 붙인다.
fn threads(pid: u64) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let dir: PathBuf = entry.unwrap().path();
        let Ok(comm) = std::fs::read_to_string(dir.join("comm")) else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(dir.join("status")) else {
            continue;
        };
        // 커널 일꾼(PF_USER_WORKER·PF_KTHREAD·PF_NO_SETAFFINITY)은 커널이 친화를 정한다.
        let kernel = std::fs::read_to_string(dir.join("stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(6)?.parse::<u64>().ok())
                .is_some_and(|flags| flags & (0x4000 | 0x20_0000 | 0x400_0000) != 0)
        });
        if kernel {
            continue;
        }
        let cpus = status
            .lines()
            .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
            .unwrap()
            .trim()
            .to_string();
        let tid = dir.file_name().unwrap().to_string_lossy().to_string();
        let name = comm.trim().to_string();
        let key = if out.contains_key(&name) {
            format!("{name}#{tid}")
        } else {
            name
        };
        out.insert(key, cpus);
    }
    out
}

fn is_vcpu(name: &str) -> bool {
    name.starts_with("CPU ") && name.contains('/')
}

/// vCPU 스레드의 친화(이름 순)
fn vcpu_cpus(pid: u64) -> Vec<String> {
    threads(pid)
        .into_iter()
        .filter(|(n, _)| is_vcpu(n))
        .map(|(_, c)| c)
        .collect()
}

/// vCPU 밖 스레드의 친화가 모두 `want`가 될 때까지 기다린다. QEMU는 일꾼 스레드(`worker`)를 필요할 때 만들고,
/// vCPU 스레드가 만든 일꾼은 그 vCPU의 친화를 물려받는다 — 고정 정리(30초마다)가 바로잡으므로 그만큼 기다린다.
async fn others_on(pid: u64, want: &str) {
    let start = std::time::Instant::now();
    loop {
        let wrong: Vec<(String, String)> = threads(pid)
            .into_iter()
            .filter(|(name, cpus)| !is_vcpu(name) && cpus != want)
            .collect();
        if wrong.is_empty() {
            return;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(75),
            "threads of QEMU {pid} not on {want}: {wrong:?}"
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn vm(c: &Client, name: &str) -> Value {
    c.get(&format!("/vms/{name}")).await.unwrap()
}

async fn pid(c: &Client, name: &str) -> u64 {
    vm(c, name).await["status"]["pid"].as_u64().unwrap()
}

/// 노드가 토폴로지를 알릴 때까지 기다리고 돌려준다.
async fn topology(c: &Client, node: &str, ok: impl Fn(&Value) -> bool) -> Value {
    eventually("node topology", || async {
        let n = c.get(&format!("/nodes/{node}")).await.unwrap();
        n["status"]["topology"].is_object() && ok(&n["status"]["topology"])
    })
    .await;
    c.get(&format!("/nodes/{node}")).await.unwrap()["status"]["topology"].clone()
}

fn has_cpus_0_to_3(t: &Value) -> bool {
    let cpus = t["cpus"].as_str().unwrap_or_default();
    cpus.starts_with("0-")
        && cpus[2..]
            .split([',', '-'])
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            >= Some(3)
}

/// 노드 하나(예약 0, 전용 2-3, 공유 1): 전용 VM의 vCPU 스레드는 2·3 하나씩, 나머지 스레드는 1, 스코프 cpuset은 1-3이고
/// 2M hugepages 백엔드가 HugePages_Free를 줄인다. 공유 VM의 모든 스레드는 1이다. 전용 CPU가 모자란 둘째 전용 VM은 켜지
/// 않는다. x86_64는 실행 중 붙인 vCPU도 잡아 둔 pCPU에 고정된다.
#[tokio::test]
async fn dedicated_vms_are_pinned_and_use_hugepages() {
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("numa").await;
    d.login().await;
    let c = client(&d);
    let t = topology(&c, "node1", |_| true).await;
    println!("topology: {t}");
    if !has_cpus_0_to_3(&t) {
        eprintln!("skipped: this host has fewer than 4 CPUs ({})", t["cpus"]);
        d.stop();
        return;
    }
    let hugepages = sys_writable();
    let _restore = hugepages.then(|| RestoreHugepages(std::fs::read_to_string(NR_2M).unwrap()));
    let mut resources = vec![
        "node".to_string(),
        "resources".into(),
        "node1".into(),
        "--reserved-cpus".into(),
        "0".into(),
        "--dedicated-cpus".into(),
        "2-3".into(),
    ];
    if hugepages {
        let now: u64 = std::fs::read_to_string(NR_2M)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        resources.extend(["--hugepages".into(), format!("2M:{}", now + 128)]);
    }
    let args: Vec<&str> = resources.iter().map(String::as_str).collect();
    let out = d.ctl(&args).await.unwrap();
    assert!(out.contains("dedicated=2-3 reserved=0"), "{out}");
    if hugepages {
        topology(&c, "node1", |t| {
            t["nodes"][0]["hugepages"].as_array().is_some_and(|p| {
                p.iter()
                    .any(|p| p["size_kib"] == 2048 && p["total"].as_u64() >= Some(128))
            })
        })
        .await;
    }

    // 전용 VM(vCPU 2, hugepages 2M 256 MiB)
    let mut create = vec![
        "vm",
        "create",
        "ded",
        "--memory",
        "256",
        "--cores",
        "2",
        "--cpu-policy",
        "dedicated",
    ];
    if hugepages {
        create.extend(["--hugepages", "2M"]);
    }
    d.ctl(&create).await.unwrap();
    let free_before = meminfo("HugePages_Free:");
    let out = d.ctl(&["vm", "start", "ded"]).await.unwrap();
    assert!(out.contains("vm ded: start done"), "{out}");
    let v = vm(&c, "ded").await;
    let hw = &v["status"]["hardware"];
    let node_id = c.get("/nodes/node1").await.unwrap()["id"].clone();
    assert_eq!(hw["host_placement"]["node"], node_id, "{v}");
    assert_eq!(hw["numa"]["policy"], "dedicated", "{v}");
    let mut placed: Vec<u64> = hw["host_placement"]["vcpus"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_u64().unwrap())
        .collect();
    placed.sort_unstable();
    assert_eq!(placed, [2, 3], "{v}");
    let p = pid(&c, "ded").await;
    let mut vcpus = vcpu_cpus(p);
    vcpus.sort();
    assert_eq!(vcpus, ["2", "3"], "{:?}", threads(p));
    others_on(p, "1").await;
    if std::env::var_os("OPL_TEST_SYSTEMD").is_some() && t["cpuset"] == true {
        let id = v["id"].as_str().unwrap();
        let out = std::process::Command::new("systemctl")
            .args(["show", "--property=ControlGroup", "--value"])
            .arg(format!("opl-vm-{id}.scope"))
            .output()
            .unwrap();
        let cg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let cpus = std::fs::read_to_string(format!("/sys/fs/cgroup{cg}/cpuset.cpus")).unwrap();
        assert_eq!(cpus.trim(), "1-3", "scope {cg}");
    }
    if hugepages {
        assert_eq!(hw["numa"]["page"], "2M", "{v}");
        let used = free_before - meminfo("HugePages_Free:");
        assert!(used >= 128, "the VM took {used} free 2M hugepages");
    }

    // 공유 VM: 모든 스레드가 공유 집합(1)
    d.ctl(&["vm", "create", "plain", "--memory", "128", "--start"])
        .await
        .unwrap();
    let p = pid(&c, "plain").await;
    for (name, cpus) in threads(p) {
        assert_eq!(cpus, "1", "thread {name} of the shared VM");
    }
    assert_eq!(
        vm(&c, "plain").await["status"]["hardware"]["numa"],
        Value::Null
    );

    // 전용 CPU가 모자라면 켜지 않는다(409 no_capacity).
    d.ctl(&[
        "vm",
        "create",
        "ded2",
        "--memory",
        "128",
        "--cpu-policy",
        "dedicated",
    ])
    .await
    .unwrap();
    let err = d.ctl(&["vm", "start", "ded2"]).await.unwrap_err();
    assert!(
        err.to_string().contains("dedicated CPUs"),
        "second dedicated VM: {err}"
    );

    d.ctl(&["vm", "stop", "ded", "--force"]).await.unwrap();
    if hugepages {
        eventually("hugepages returned", || async {
            meminfo("HugePages_Free:") >= free_before
        })
        .await;
    }

    // 실행 중 붙인 vCPU(x86_64 KVM): 켤 때 상한까지 잡은 자리의 pCPU에 고정된다.
    if cfg!(target_arch = "x86_64") {
        d.ctl(&[
            "vm",
            "create",
            "hot",
            "--memory",
            "128",
            "--cores",
            "1",
            "--max-vcpus",
            "2",
            "--cpu-policy",
            "dedicated",
        ])
        .await
        .unwrap();
        d.ctl(&["vm", "start", "hot"]).await.unwrap();
        let v = vm(&c, "hot").await;
        let slots = v["status"]["hardware"]["host_placement"]["vcpus"].clone();
        assert_eq!(slots.as_array().map(Vec::len), Some(2), "{v}");
        let p = pid(&c, "hot").await;
        assert_eq!(vcpu_cpus(p), [slots[0].to_string()], "{:?}", threads(p));
        // 전용 VM의 `vm set`도 상한 안의 vCPU를 바로 붙인다(핫플러그 Task).
        d.ctl(&["vm", "set", "hot", "--cores", "2"]).await.unwrap();
        eventually("two vCPU threads", || async { vcpu_cpus(p).len() == 2 }).await;
        let mut got = vcpu_cpus(p);
        got.sort();
        let mut want = vec![slots[0].to_string(), slots[1].to_string()];
        want.sort();
        assert_eq!(got, want, "{:?}", threads(p));
        d.ctl(&["vm", "stop", "hot", "--force"]).await.unwrap();
    }

    d.ctl(&["vm", "stop", "plain", "--force"]).await.unwrap();
    let out = d
        .ctl(&["node", "resources", "node1", "--clear"])
        .await
        .unwrap();
    assert!(out.contains("node node1: none"), "{out}");
    d.stop();
}

/// 한 호스트의 opld 둘(node1 전용 2, node2 전용 3)로 전용 VM을 라이브 마이그레이션하면 받는 쪽이 자기 pCPU에 다시
/// 고정하고, 기록의 배치가 node2의 것이 된다. 한 호스트라 스코프는 끈다.
#[tokio::test]
async fn migration_re_pins_on_the_target() {
    let config = |ports: &str| {
        format!("[reconciler]\nacpi_timeout_secs = 5\n[migration]\nports = \"{ports}\"\n")
    };
    let mut d = [
        Daemon::with_hypervisor("node1", &config("60420-60429"), "systemd_scope = false"),
        Daemon::with_hypervisor("node2", &config("60430-60439"), "systemd_scope = false"),
    ];
    for d in &mut d {
        d.start().await;
    }
    d[0].init("numa-mig").await;
    d[0].login().await;
    let url = d[0].join_url.clone().unwrap();
    d[1].join(&url).await.unwrap();
    let c = client(&d[0]);
    eventually("both nodes online", || async {
        let nodes = c.get("/nodes").await.unwrap();
        let nodes = nodes["items"].as_array().unwrap();
        nodes.len() == 2 && nodes.iter().all(|n| n["status"]["online"] == true)
    })
    .await;
    let t = topology(&c, "node1", |_| true).await;
    topology(&c, "node2", |_| true).await;
    if !has_cpus_0_to_3(&t) {
        eprintln!("skipped: this host has fewer than 4 CPUs ({})", t["cpus"]);
        for d in &mut d {
            d.stop();
        }
        return;
    }
    for (node, cpus) in [("node1", "2"), ("node2", "3")] {
        d[0].ctl(&["node", "resources", node, "--dedicated-cpus", cpus])
            .await
            .unwrap();
    }

    d[0].ctl(&[
        "vm",
        "create",
        "web",
        "--memory",
        "128",
        "--node",
        "node1",
        "--cpu-policy",
        "dedicated",
    ])
    .await
    .unwrap();
    d[0].ctl(&["vm", "start", "web"]).await.unwrap();
    let p = pid(&c, "web").await;
    assert_eq!(vcpu_cpus(p), ["2"], "{:?}", threads(p));
    others_on(p, "0-1,3").await;

    let task = c
        .post("/vms/web/migrate", &json!({ "node": "node2" }))
        .await
        .unwrap();
    c.wait_task(&task).await.unwrap();
    let node2 = c.get("/nodes/node2").await.unwrap()["id"].clone();
    eventually("running on node2", || async {
        let v = vm(&c, "web").await;
        v["status"]["phase"] == "running"
            && v["status"]["pid"].is_u64()
            && v["status"]["node"] == node2
    })
    .await;
    let v = vm(&c, "web").await;
    assert_eq!(
        v["status"]["hardware"]["host_placement"]["node"], node2,
        "{v}"
    );
    assert_eq!(
        v["status"]["hardware"]["host_placement"]["vcpus"],
        json!([3]),
        "{v}"
    );
    let p = pid(&c, "web").await;
    assert_eq!(vcpu_cpus(p), ["3"], "{:?}", threads(p));
    others_on(p, "0-2").await;

    d[0].ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d[0].ctl(&["vm", "delete", "web"]).await.unwrap();
    for d in &mut d {
        d.stop();
    }
}
