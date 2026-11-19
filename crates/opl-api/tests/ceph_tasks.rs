//! Ceph 자동 구성(계획 56)의 Task와 클라이언트 설정 에이전트. 노드의 명령은 가짜 실행기가 받고 파일은 임시
//! 디렉터리에 쓴다. 리소스와 Task는 시험이 저장소에 직접 만든다(API는 계획 56 스테이지 5).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Client, Env, MOUNT_ROOT, WAIT};
use opl_api::CephAgent;
use opl_cluster::watch::WatchEvent;
use opl_driver::DirStorage;
use opl_driver::ceph::{
    CONF_MARK, CephHost, CephOptions, CephPaths, CmdOutput, DAEMON_PACKAGES, FakeRunner,
};
use opl_types::ceph::{CEPH_NAME, CephKeys, CephMon, CephPending, CephPhase, CephSpec, CephStatus};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Kind, Resource};
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const FSID: &str = "18796e2f-112c-44cf-9d48-09f825f0a14b";
const VERSION: &str =
    "ceph version 19.2.3 (c92aebb279828e9c3c1f5d24613efca272649e62) squid (stable)\n";
const RELEASE: &str = "19.2.3 squid";
const LSBLK: &[u8] = include_bytes!("../../opl-driver/src/ceph/fixtures/lsblk-root.json");
const PVS: &[u8] = include_bytes!("../../opl-driver/src/ceph/fixtures/pvs.json");
const OSD_TREE: &[u8] = include_bytes!("../../opl-driver/src/ceph/fixtures/osd-tree.json");

fn keys() -> CephKeys {
    CephKeys {
        mon: "AQmon".into(),
        admin: "AQadmin".into(),
        bootstrap_osd: "AQosd".into(),
        client: "AQclient".into(),
    }
}

fn spec() -> CephSpec {
    CephSpec {
        fsid: FSID.parse().unwrap(),
        // 시험 노드의 주소는 127.0.0.1이다.
        public_network: "127.0.0.0/8".parse().unwrap(),
        cluster_network: None,
        keys: keys(),
    }
}

fn mon(node: Uuid, name: &str, addr: &str) -> CephMon {
    CephMon {
        node,
        name: name.into(),
        addr: addr.parse().unwrap(),
    }
}

/// 가짜 실행기와 임시 디렉터리를 쓰는 `CephHost`
struct Host {
    _dir: TempDir,
    root: PathBuf,
    fake: Arc<FakeRunner>,
    host: Arc<CephHost>,
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let fake = Arc::new(FakeRunner::new());
        let paths = CephPaths {
            etc_ceph: root.join("etc/ceph"),
            var_lib_ceph: root.join("var/lib/ceph"),
            client_dir: root.join("etc/opl/ceph"),
            by_id: root.join("dev/disk/by-id"),
        };
        let options = CephOptions {
            wait_step: Duration::from_millis(5),
            wait_limit: Duration::from_millis(300),
            any_os: true,
            ..CephOptions::default()
        };
        let host = Arc::new(CephHost::new(fake.clone(), paths, options));
        Self {
            _dir: dir,
            root,
            fake,
            host,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap()
    }

    /// 꾸러미가 모두 깔려 있고 Ceph 19.2다.
    fn installed(&self) {
        let out: String = DAEMON_PACKAGES
            .iter()
            .map(|p| format!("{p} install ok installed\n"))
            .collect();
        self.fake.reply("dpkg-query", CmdOutput::ok(out));
        self.fake.reply("ceph --version", CmdOutput::ok(VERSION));
    }

    /// `prefix`로 시작하는 명령 줄의 수
    fn count(&self, prefix: &str) -> usize {
        self.fake
            .calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
}

/// 가짜 Ceph 단계를 쓰는 Task 실행기와 API
struct Ceph {
    env: Env,
    host: Host,
    admin: Client,
}

impl Ceph {
    async fn start() -> Self {
        let mut env = Env::start().await;
        let host = Host::new();
        env.stop_runner().await;
        let ceph = host.host.clone();
        env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
            c.ceph = Some(ceph)
        })
        .await;
        let admin = env.admin().await;
        Self { env, host, admin }
    }

    /// `Ceph` 리소스를 만든다.
    async fn create(&self, status: CephStatus) -> Uuid {
        let mut ceph = Resource::new(CEPH_NAME, spec());
        ceph.status = status;
        self.env.cluster.create(&ceph).await.unwrap();
        ceph.id
    }

    fn ceph(&self) -> CephStatus {
        self.env
            .cluster
            .get_by_name::<CephSpec>(CEPH_NAME)
            .unwrap()
            .unwrap()
            .status
    }

    /// 이 노드의 Task를 만들고 끝날 때까지 기다린다.
    async fn run(&self, op: TaskOp, ceph: Uuid) -> Value {
        let mut task: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: ceph,
                node: self.env.node,
            },
        );
        task.name = task.id.to_string();
        self.env.cluster.create(&task).await.unwrap();
        self.admin.wait_task(&json!({ "id": task.id })).await
    }

    fn ready(&self) -> CephStatus {
        CephStatus {
            phase: CephPhase::Ready,
            mons: vec![mon(Uuid::nil(), "node0", "127.0.0.9")],
            mgrs: vec![Uuid::nil()],
            release: Some(RELEASE.into()),
            ..CephStatus::default()
        }
    }
}

fn state(task: &Value) -> &str {
    task["status"]["state"].as_str().unwrap()
}

fn error(task: &Value) -> &str {
    task["status"]["error"].as_str().unwrap_or_default()
}

/// 부트스트랩: 설치 확인 → mon(새 monmap) → 정족수 → mgr → available. 끝나면 `ready`와 mon·mgr·판이 Raft에,
/// 데몬 conf(이 mon)·admin 키링·클라이언트 설정이 노드에 있다.
#[tokio::test]
async fn init_bootstraps_a_monitor_and_a_manager() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.installed();
    h.fake.reply_seq(
        "ceph --connect-timeout 10 quorum_status",
        vec![
            CmdOutput::ok(r#"{"quorum_names":[]}"#),
            CmdOutput::ok(r#"{"quorum_names":["node1"]}"#),
        ],
    );
    h.fake.reply(
        "ceph --connect-timeout 10 mgr stat",
        CmdOutput::ok(r#"{"available":true,"active_name":"node1"}"#),
    );
    let id = c
        .create(CephStatus {
            init_node: Some(c.env.node),
            ..CephStatus::default()
        })
        .await;
    let (revision, _) = c.env.cluster.list::<TaskSpec>().unwrap();
    let mut watch = c.env.cluster.watch(revision).unwrap();
    let task = c.run(TaskOp::CephInit, id).await;
    assert_eq!(state(&task), "succeeded", "{task}");
    // 진행률 stage는 단계 이름이다(바이트 0/0).
    let mut stages: Vec<String> = Vec::new();
    while let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(200), watch.next()).await {
        let WatchEvent::Put {
            kind: Kind::Task,
            resource,
        } = ev
        else {
            continue;
        };
        let t: Task = serde_json::from_value(serde_json::to_value(resource).unwrap()).unwrap();
        if let Some(p) = t.status.progress {
            assert_eq!((p.done_bytes, p.total_bytes), (0, 0));
            if stages.last() != Some(&p.stage) {
                stages.push(p.stage);
            }
        }
    }
    assert_eq!(
        stages,
        [
            "checking",
            "installing-packages",
            "writing-config",
            "creating-monitor",
            "starting-monitor",
            "waiting-for-quorum",
            "creating-manager",
        ]
    );

    let status = c.ceph();
    assert_eq!(status.phase, CephPhase::Ready);
    assert_eq!(status.mons, [mon(c.env.node, "node1", "127.0.0.1")]);
    assert_eq!(status.mgrs, [c.env.node]);
    assert_eq!(status.release.as_deref(), Some(RELEASE));
    assert_eq!(status.last_error, None);

    // 명령의 차례(앞 세 낱말)
    let order: Vec<String> = h
        .fake
        .calls()
        .iter()
        .map(|c| c.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        order,
        [
            "dpkg-query -W -f",
            "ceph --version",
            "monmaptool --create --addv",
            "chown -R ceph:ceph",
            "ceph-mon --mkfs -i",
            "systemctl enable ceph.target",
            "systemctl enable --now",
            "ceph --connect-timeout 10",
            "ceph --connect-timeout 10",
            "ceph --connect-timeout 10",
            "chown -R ceph:ceph",
            "systemctl enable ceph-mgr.target",
            "systemctl enable --now",
            "systemctl is-active ceph-mgr@node1",
            "ceph --connect-timeout 10",
        ]
    );
    assert!(h.fake.calls()[9].contains("auth get-or-create mgr.node1"));
    assert!(h.fake.calls()[14].contains("mgr stat"));

    let addrv = "[v2:127.0.0.1:3300,v1:127.0.0.1:6789]";
    assert_eq!(
        h.read("etc/ceph/ceph.conf"),
        format!(
            "{CONF_MARK}\n[global]\nfsid = {FSID}\nmon_host = {addrv}\npublic_network = 127.0.0.0/8\n\
             auth_cluster_required = cephx\nauth_service_required = cephx\nauth_client_required = cephx\n\
             [mon]\nauth_allow_insecure_global_id_reclaim = false\n"
        )
    );
    assert!(
        h.read("etc/ceph/ceph.client.admin.keyring")
            .starts_with("[client.admin]\n\tkey = AQadmin\n")
    );
    let keyring = h.path("etc/opl/ceph/ceph.client.orbivisor.keyring");
    assert_eq!(
        std::fs::read_to_string(&keyring).unwrap(),
        "[client.orbivisor]\n\tkey = AQclient\n"
    );
    assert_eq!(
        h.read("etc/opl/ceph/ceph.conf"),
        format!(
            "{CONF_MARK}\n[global]\nfsid = {FSID}\nmon_host = {addrv}\n\
             auth_cluster_required = cephx\nauth_service_required = cephx\nauth_client_required = cephx\n\
             [client.orbivisor]\nkeyring = {}\n",
            keyring.display()
        )
    );
    assert!(h.path("var/lib/ceph/mon/ceph-node1/done").exists());
}

/// 정족수에 들지 못하면 실패하고 이유를 리소스에 남긴다(phase는 그대로). 같은 요청을 다시 하면 mon 저장소를
/// 다시 만들지 않고 이어서 `ready`가 된다.
#[tokio::test]
async fn a_failed_init_records_the_error_and_resumes() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.installed();
    h.fake.reply(
        "ceph --connect-timeout 10 quorum_status",
        CmdOutput::ok(r#"{"quorum_names":[]}"#),
    );
    let id = c
        .create(CephStatus {
            init_node: Some(c.env.node),
            ..CephStatus::default()
        })
        .await;
    let task = c.run(TaskOp::CephInit, id).await;
    assert_eq!(state(&task), "failed");
    assert_eq!(
        error(&task),
        "timed out waiting for the monitor to join the quorum"
    );
    let status = c.ceph();
    assert_eq!(status.phase, CephPhase::Bootstrapping);
    assert_eq!(
        status.last_error.as_deref(),
        Some("timed out waiting for the monitor to join the quorum")
    );
    assert!(status.mons.is_empty());
    assert_eq!(h.count("ceph-mon --mkfs"), 1);

    h.fake.reply(
        "ceph --connect-timeout 10 quorum_status",
        CmdOutput::ok(r#"{"quorum_names":["node1"]}"#),
    );
    h.fake.reply(
        "ceph --connect-timeout 10 mgr stat",
        CmdOutput::ok(r#"{"available":true,"active_name":"node1"}"#),
    );
    let task = c.run(TaskOp::CephInit, id).await;
    assert_eq!(state(&task), "succeeded", "{task}");
    assert_eq!(h.count("ceph-mon --mkfs"), 1);
    let status = c.ceph();
    assert_eq!(status.phase, CephPhase::Ready);
    assert_eq!(status.last_error, None);
    assert_eq!(status.mons.len(), 1);
}

/// 부트스트랩을 다른 노드로 옮긴 뒤(또는 자리가 다른 Task의 것) 옛 부트스트랩 Task가 돌면 mon 저장소를 만들기
/// 전에 멈춘다 — 같은 fsid로 두 번째 클러스터를 세우지 않는다. 버려진 Task의 실패는 `last_error`에 남기지 않는다.
#[tokio::test]
async fn an_init_moved_to_another_node_stops_before_mkfs() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.installed();
    let elsewhere = Uuid::now_v7();
    let id = c
        .create(CephStatus {
            init_node: Some(elsewhere),
            ..CephStatus::default()
        })
        .await;
    let task = c.run(TaskOp::CephInit, id).await;
    assert_eq!(state(&task), "failed", "{task}");
    assert_eq!(
        error(&task),
        "the Ceph bootstrap was moved off node \"node1\"; this task stops"
    );
    assert_eq!(h.count("monmaptool"), 0);
    assert_eq!(h.count("ceph-mon --mkfs"), 0);
    let status = c.ceph();
    assert_eq!(status.init_node, Some(elsewhere));
    assert_eq!(status.last_error, None);
    assert_eq!(status.release, None);
    assert!(status.mons.is_empty());

    // 부트스트랩 노드는 이 노드지만 자리는 다른(새) Task의 것이다.
    let other = Uuid::now_v7();
    c.env
        .cluster
        .update_status::<CephSpec>(
            id,
            &CephStatus {
                init_node: Some(c.env.node),
                pending: Some(CephPending {
                    op: TaskOp::CephInit,
                    node: c.env.node,
                    task: other,
                }),
                ..CephStatus::default()
            },
        )
        .await
        .unwrap();
    let task = c.run(TaskOp::CephInit, id).await;
    assert_eq!(state(&task), "failed", "{task}");
    assert_eq!(h.count("ceph-mon --mkfs"), 0);
    assert_eq!(c.ceph().pending.map(|p| p.task), Some(other));
}

/// 표시 없는 `/etc/ceph/ceph.conf`가 있으면 아무 명령 없이 실패하고 mon·mgr은 그대로다.
#[tokio::test]
async fn a_foreign_ceph_config_stops_the_task() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.installed();
    std::fs::create_dir_all(h.path("etc/ceph")).unwrap();
    std::fs::write(h.path("etc/ceph/ceph.conf"), "[global]\nfsid = x\n").unwrap();
    let id = c
        .create(CephStatus {
            init_node: Some(c.env.node),
            ..CephStatus::default()
        })
        .await;
    let task = c.run(TaskOp::CephInit, id).await;
    assert_eq!(state(&task), "failed");
    let expected = format!(
        "node \"node1\" already has {} not written by Orbivisor (another Ceph cluster?); \
         Orbivisor does not adopt an existing Ceph — use it as an rbd storage pool instead, or \
         move the file away",
        h.path("etc/ceph/ceph.conf").display()
    );
    assert_eq!(error(&task), expected);
    assert!(h.fake.calls().is_empty());
    let status = c.ceph();
    assert_eq!(status.phase, CephPhase::Bootstrapping);
    assert!(status.mons.is_empty() && status.mgrs.is_empty() && status.release.is_none());
    assert_eq!(status.last_error.as_deref(), Some(expected.as_str()));
    assert_eq!(h.read("etc/ceph/ceph.conf"), "[global]\nfsid = x\n");
}

/// 기다리던 파일이 `want`를 담을 때까지 기다린다.
async fn wait_file(path: &Path, want: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(want) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "{} has no {want:?}: {text}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// mon 더하기: 정족수에서 monmap을 받아 이 mon을 더하고 정족수에 들면 `mons`에 더한다. 에이전트가 클라이언트
/// 설정의 `mon_host`를 둘로 고친다.
#[tokio::test]
async fn adding_a_monitor_updates_every_client_config() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.installed();
    h.fake.reply(
        "ceph --connect-timeout 10 quorum_status",
        CmdOutput::ok(r#"{"quorum_names":["node0","node1"]}"#),
    );
    let id = c.create(c.ready()).await;
    let agent = CephAgent::spawn(c.env.cluster.clone(), c.env.node, h.host.clone());
    let client = h.path("etc/opl/ceph/ceph.conf");
    wait_file(
        &client,
        "mon_host = [v2:127.0.0.9:3300,v1:127.0.0.9:6789]\n",
    )
    .await;

    let task = c.run(TaskOp::CephMonAdd, id).await;
    assert_eq!(state(&task), "succeeded", "{task}");
    assert_eq!(
        c.ceph().mons,
        [
            mon(Uuid::nil(), "node0", "127.0.0.9"),
            mon(c.env.node, "node1", "127.0.0.1")
        ]
    );
    let tmp = h.path("var/lib/ceph/tmp/opl-mon-node1");
    let calls = h.fake.calls();
    // 정족수는 Raft의 mon(node0)에만 묻는다.
    assert!(
        calls.iter().any(|l| l
            == "ceph --connect-timeout 10 quorum_status -m [v2:127.0.0.9:3300,v1:127.0.0.9:6789] \
                --format json"),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|l| l == "ceph --connect-timeout 10 quorum_status --format json"),
        "{calls:?}"
    );
    let getmap = calls
        .iter()
        .position(|l| l.contains("mon getmap"))
        .expect("mon getmap");
    assert_eq!(
        calls[getmap..getmap + 3],
        [
            format!(
                "ceph --connect-timeout 10 mon getmap -o {}/monmap",
                tmp.display()
            ),
            format!("monmaptool --print {}/monmap", tmp.display()),
            format!(
                "monmaptool --addv node1 [v2:127.0.0.1:3300,v1:127.0.0.1:6789] {}/monmap",
                tmp.display()
            ),
        ]
    );
    wait_file(
        &client,
        "mon_host = [v2:127.0.0.9:3300,v1:127.0.0.9:6789] [v2:127.0.0.1:3300,v1:127.0.0.1:6789]\n",
    )
    .await;
    // 데몬 conf도 Orbivisor가 쓴 것이라 에이전트가 맞춘다.
    wait_file(
        &h.path("etc/ceph/ceph.conf"),
        "mon_host = [v2:127.0.0.9:3300,v1:127.0.0.9:6789] [v2:127.0.0.1:3300,v1:127.0.0.1:6789]\n",
    )
    .await;
    agent.shutdown().await;
}

/// 클러스터가 준비되기 전에는 mon·mgr·OSD를 더하지 않는다.
#[tokio::test]
async fn tasks_after_init_need_a_ready_cluster() {
    let c = Ceph::start().await;
    let id = c.create(CephStatus::default()).await;
    for op in [
        TaskOp::CephMonAdd,
        TaskOp::CephMgrAdd,
        TaskOp::CephOsdAdd {
            device: "/dev/sdb".into(),
            wipe: false,
        },
    ] {
        let task = c.run(op, id).await;
        assert_eq!(error(&task), "the Ceph cluster is not ready yet");
    }
    assert!(c.host.fake.calls().is_empty());
}

/// mgr 더하기: 키링을 만들고 유닛이 active가 되면 `mgrs`에 더한다.
#[tokio::test]
async fn adding_a_manager() {
    let c = Ceph::start().await;
    c.host.installed();
    let id = c.create(c.ready()).await;
    let task = c.run(TaskOp::CephMgrAdd, id).await;
    assert_eq!(state(&task), "succeeded", "{task}");
    assert_eq!(c.ceph().mgrs, [Uuid::nil(), c.env.node]);
    assert_eq!(
        c.host
            .count("ceph --connect-timeout 10 auth get-or-create mgr.node1"),
        1
    );
    assert_eq!(c.host.count("systemctl enable --now ceph-mgr@node1"), 1);
    assert!(c.host.path("etc/ceph/ceph.client.admin.keyring").exists());
}

/// OSD 더하기: 빈 디스크에 OSD를 만들고 `osd_nodes`에 더한다. 쓰는 중인 디스크는 명령 없이 실패한다. 판이 다른
/// 노드는 거절한다. mon·mgr이 없는 이 노드에는 admin 키링이 없어(admin으로 부른 `ceph`는 실패) OSD가 up인지는
/// client.orbivisor(Task가 쓴 클라이언트 설정)로 본다.
#[tokio::test]
async fn adding_osds() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.installed();
    h.fake.reply("lsblk", CmdOutput::ok(LSBLK));
    h.fake.reply("pvs", CmdOutput::ok(PVS));
    h.fake
        .reply("lvs", CmdOutput::ok(r#"{"report":[{"lv":[]}]}"#));
    h.fake.reply(
        "ceph --connect-timeout",
        CmdOutput::fail(
            1,
            "auth: unable to find a keyring on /etc/ceph/ceph.client.admin.keyring\n",
        ),
    );
    let osd_tree = format!(
        "ceph --conf {} --id orbivisor --connect-timeout 10 osd tree",
        h.path("etc/opl/ceph/ceph.conf").display()
    );
    h.fake
        .reply("ceph-volume lvm list --format json", CmdOutput::ok("{}"));
    h.fake.reply(
        "ceph-volume lvm list /dev/sdb",
        CmdOutput::ok(format!(
            r#"{{"3":[{{"devices":["/dev/sdb"],"type":"block","tags":{{"ceph.cluster_fsid":"{FSID}","ceph.osd_fsid":"0851efa7-c274-4d7d-a734-b3a881914253","ceph.osd_id":"3"}}}}]}}"#
        )),
    );
    let id = c.create(c.ready()).await;

    let task = c
        .run(
            TaskOp::CephOsdAdd {
                device: "/dev/sda".into(),
                wipe: true,
            },
            id,
        )
        .await;
    assert_eq!(
        error(&task),
        "/dev/sda cannot hold an OSD: system disk (/ is on it)"
    );
    assert!(c.ceph().osd_nodes.is_empty());
    assert_eq!(h.count("ceph-volume lvm zap"), 0);

    // 유닛은 active지만 OSD가 Ceph에서 down이면 Task가 실패하고 노드를 더하지 않는다.
    h.fake.reply(
        &osd_tree,
        CmdOutput::ok(
            r#"{"nodes":[{"id":-3,"name":"node1","type":"host","children":[3]},{"id":3,"name":"osd.3","type":"osd","status":"down","reweight":1}],"stray":[]}"#,
        ),
    );
    let task = c
        .run(
            TaskOp::CephOsdAdd {
                device: "/dev/sdb".into(),
                wipe: false,
            },
            id,
        )
        .await;
    assert_eq!(
        error(&task),
        "OSD 3 did not come up: its unit is active but ceph osd tree does not show it up"
    );
    assert!(c.ceph().osd_nodes.is_empty());

    // 다시 하면 이미 만든 OSD를 켜기만 하고 up(osd-tree.json의 osd.3)을 기다린다.
    h.fake.reply(
        "ceph-volume lvm list --format json",
        CmdOutput::ok(format!(
            r#"{{"3":[{{"devices":["/dev/sdb"],"type":"block","tags":{{"ceph.cluster_fsid":"{FSID}","ceph.osd_fsid":"0851efa7-c274-4d7d-a734-b3a881914253","ceph.osd_id":"3"}}}}]}}"#
        )),
    );
    h.fake.reply(&osd_tree, CmdOutput::ok(OSD_TREE));
    let task = c
        .run(
            TaskOp::CephOsdAdd {
                device: "/dev/sdb".into(),
                wipe: false,
            },
            id,
        )
        .await;
    assert_eq!(state(&task), "succeeded", "{task}");
    assert_eq!(c.ceph().osd_nodes, [c.env.node]);
    assert_eq!(
        h.count("ceph-volume lvm create --bluestore --data /dev/sdb"),
        1
    );
    assert_eq!(
        h.count("ceph-volume lvm activate 3 0851efa7-c274-4d7d-a734-b3a881914253"),
        1
    );
    assert_eq!(h.count("ceph-volume lvm activate --all"), 0);
    assert_eq!(h.count("ceph --connect-timeout"), 0);
    assert!(!h.path("etc/ceph/ceph.client.admin.keyring").exists());
    assert!(
        h.read("etc/opl/ceph/ceph.conf")
            .contains("mon_host = [v2:127.0.0.9:3300,v1:127.0.0.9:6789]")
    );
    assert!(
        h.read("var/lib/ceph/bootstrap-osd/ceph.keyring")
            .starts_with("[client.bootstrap-osd]\n\tkey = AQosd\n")
    );

    // 클러스터가 다른 판을 돌린다.
    let mut status = c.ready();
    status.release = Some("18.2.7 reef".into());
    let ceph = c
        .env
        .cluster
        .get_by_name::<CephSpec>(CEPH_NAME)
        .unwrap()
        .unwrap();
    c.env
        .cluster
        .update_status::<CephSpec>(ceph.id, &status)
        .await
        .unwrap();
    let task = c
        .run(
            TaskOp::CephOsdAdd {
                device: "/dev/sdb".into(),
                wipe: false,
            },
            id,
        )
        .await;
    assert_eq!(
        error(&task),
        "node \"node1\" has ceph 19.2.3 squid but the cluster runs 18.2.7 reef; upgrade the \
         packages first"
    );
}

/// 풀 만들기: Ceph 풀을 만들고(호스트 수는 `osd tree`) Orbivisor RBD 풀(사용자 `orbivisor`, 관리 conf)로 등록한다.
/// 같은 이름의 Orbivisor 풀이 있으면 Ceph를 건드리기 전에 실패한다.
#[tokio::test]
async fn creating_a_pool_registers_an_rbd_storage_pool() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.fake.reply(
        "ceph --connect-timeout 10 osd pool ls --format json",
        CmdOutput::ok(r#"[".mgr"]"#),
    );
    h.fake.reply(
        "ceph --connect-timeout 10 osd tree",
        CmdOutput::ok(OSD_TREE),
    );
    h.fake.reply(
        "ceph --connect-timeout 10 osd pool ls detail",
        CmdOutput::ok(r#"[{"pool_name":".mgr","size":3},{"pool_name":"vmpool","size":2}]"#),
    );
    h.fake.reply(
        "ceph --connect-timeout 10 pg ls-by-pool vmpool",
        CmdOutput::ok(r#"{"pg_ready":true,"pg_stats":[{"pgid":"2.0","state":"active+clean"}]}"#),
    );
    let id = c.create(c.ready()).await;
    let op = |name: &str| TaskOp::CephPoolCreate {
        name: name.into(),
        pool: "vmpool".into(),
        size: 2,
    };
    let task = c.run(op("vmpool"), id).await;
    assert_eq!(state(&task), "succeeded", "{task}");
    let pool = c
        .env
        .cluster
        .get_by_name::<StoragePoolSpec>("vmpool")
        .unwrap()
        .unwrap();
    assert_eq!(
        pool.spec.backend,
        PoolBackend::Rbd {
            pool: "vmpool".into(),
            namespace: None,
            user: Some("orbivisor".into()),
            conf: Some(h.path("etc/opl/ceph/ceph.conf").display().to_string()),
        }
    );
    assert!(pool.spec.shared && pool.spec.nodes.is_empty());
    assert_eq!(
        h.count("ceph --connect-timeout 10 osd pool create vmpool"),
        1
    );
    // 호스트 둘(osd-tree.json) → .mgr을 2로 낮춘다.
    assert_eq!(
        h.count("ceph --connect-timeout 10 osd pool set .mgr size 2"),
        1
    );
    assert_eq!(h.count("rbd pool init vmpool"), 1);
    // Ceph 풀에 등록할 Orbivisor 풀 이름을 표시로 단다.
    assert_eq!(
        h.count(
            "ceph --connect-timeout 10 osd pool application set vmpool rbd orbivisor-pool vmpool"
        ),
        1
    );

    // 같은 이름의 Orbivisor 풀이 이미 있다.
    let before = h.fake.calls().len();
    let task = c.run(op("vmpool"), id).await;
    assert_eq!(error(&task), "storage pool \"vmpool\" already exists");
    assert_eq!(h.fake.calls().len(), before);
}

/// 클라이언트 꾸러미: 빠진 것만 깐다.
#[tokio::test]
async fn installing_client_packages() {
    let c = Ceph::start().await;
    let h = &c.host;
    h.fake.reply(
        "dpkg-query",
        CmdOutput::ok("ceph-common install ok installed\n"),
    );
    h.fake.reply("ceph --version", CmdOutput::ok(VERSION));
    let id = c.create(c.ready()).await;
    let task = c.run(TaskOp::CephClientInstall, id).await;
    assert_eq!(state(&task), "succeeded", "{task}");
    assert_eq!(
        h.count("apt-get install -y -q -o DPkg::Lock::Timeout=600 qemu-block-extra"),
        1
    );
}

/// Ceph 단계가 없는 노드(위트니스 등)는 Ceph Task를 하지 못한다.
#[tokio::test]
async fn a_node_without_ceph_cannot_run_ceph_tasks() {
    let c = Ceph::start().await;
    let id = c.create(CephStatus::default()).await;
    let mut env = c.env;
    env.stop_runner().await;
    env.start_runner().await;
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::CephInit,
            target: id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    let task = c.admin.wait_task(&json!({ "id": task.id })).await;
    assert_eq!(error(&task), "this node cannot run Ceph tasks");
}

/// 에이전트: 리소스가 생기면(mon이 있을 때) 비위트니스 노드에 클라이언트 설정을 쓰고, 위트니스에는 쓰지 않는다.
/// 내용이 같으면 다시 쓰지 않고, Orbivisor가 쓰지 않은 `/etc/ceph/ceph.conf`는 그대로 둔다.
#[tokio::test]
async fn the_agent_writes_client_configs() {
    let c = Ceph::start().await;
    let h = &c.host;
    // 위트니스 노드와 그 노드의 에이전트
    let mut witness = Resource::new(
        "witness1",
        NodeSpec {
            host_resources: None,
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: true,
            roles: None,
        },
    );
    witness.status = NodeStatus {
        online: true,
        ..NodeStatus::default()
    };
    c.env.cluster.create(&witness).await.unwrap();
    let w = Host::new();
    let witness_agent = CephAgent::spawn(c.env.cluster.clone(), witness.id, w.host.clone());

    // 이 노드는 mgr 노드이고 /etc/ceph/ceph.conf는 남의 것이다.
    std::fs::create_dir_all(h.path("etc/ceph")).unwrap();
    std::fs::write(h.path("etc/ceph/ceph.conf"), "[global]\nfsid = x\n").unwrap();
    let agent = CephAgent::spawn(c.env.cluster.clone(), c.env.node, h.host.clone());
    // mon이 없으면 쓰지 않는다.
    let id = c.create(CephStatus::default()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!h.path("etc/opl/ceph").exists());

    let mut status = c.ready();
    status.mgrs.push(c.env.node);
    c.env
        .cluster
        .update_status::<CephSpec>(id, &status)
        .await
        .unwrap();
    let client = h.path("etc/opl/ceph/ceph.conf");
    wait_file(
        &client,
        "mon_host = [v2:127.0.0.9:3300,v1:127.0.0.9:6789]\n",
    )
    .await;
    assert_eq!(
        h.read("etc/opl/ceph/ceph.client.orbivisor.keyring"),
        "[client.orbivisor]\n\tkey = AQclient\n"
    );
    let written = std::fs::metadata(&client).unwrap().modified().unwrap();

    // 클라이언트 설정이 바뀌지 않는 변경(판)
    status.release = Some("19.2.4 squid".into());
    c.env
        .cluster
        .update_status::<CephSpec>(id, &status)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        std::fs::metadata(&client).unwrap().modified().unwrap(),
        written
    );
    assert_eq!(h.read("etc/ceph/ceph.conf"), "[global]\nfsid = x\n");
    assert!(!w.path("etc/opl/ceph").exists());
    agent.shutdown().await;
    witness_agent.shutdown().await;
}

/// 끝난 Ceph Task는 자기가 잡은 자리(`pending`)를 놓는다(성공·실패 모두). 다른 Task의 자리는 그대로 둔다.
#[tokio::test]
async fn a_finished_task_releases_its_pending_slot() {
    async fn hold(c: &Ceph, id: Uuid, task: Uuid) {
        let mut status = c.ready();
        status.pending = Some(CephPending {
            op: TaskOp::CephMgrAdd,
            node: c.env.node,
            task,
        });
        c.env
            .cluster
            .update_status::<CephSpec>(id, &status)
            .await
            .unwrap();
    }
    let c = Ceph::start().await;
    c.host.installed();
    let id = c.create(c.ready()).await;
    for ok in [true, false] {
        if !ok {
            c.host.fake.reply(
                "systemctl enable --now ceph-mgr",
                CmdOutput::fail(1, "Failed to start\n"),
            );
        }
        let mut task: Task = Resource::new(
            "",
            TaskSpec {
                op: TaskOp::CephMgrAdd,
                target: id,
                node: c.env.node,
            },
        );
        task.name = task.id.to_string();
        hold(&c, id, task.id).await;
        c.env.cluster.create(&task).await.unwrap();
        let done = c.admin.wait_task(&json!({ "id": task.id })).await;
        assert_eq!(
            state(&done),
            if ok { "succeeded" } else { "failed" },
            "{done}"
        );
        assert_eq!(c.ceph().pending, None);
    }
    // 남의 자리
    let other = Uuid::now_v7();
    hold(&c, id, other).await;
    let task = c.run(TaskOp::CephMgrAdd, id).await;
    assert_eq!(state(&task), "failed", "{task}");
    assert_eq!(c.ceph().pending.map(|p| p.task), Some(other));
}
