//! SDN(OVN, 계획 65 스테이지 2)의 설치·구성 Task. 노드의 명령은 가짜 실행기가 받고(실제 출력 고정물) 파일은 임시
//! 디렉터리에 쓴다. 리소스와 Task는 시험이 저장소에 직접 만든다(API의 검사는 `tests/sdn.rs`).

mod common;

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{Client, Env, MOUNT_ROOT};
use opl_cluster::watch::WatchEvent;
use opl_driver::DirStorage;
use opl_driver::DriverError;
use opl_driver::ovn::{
    CENTRAL_PACKAGES, Cmd, CmdOutput, CommandRunner, FakeRunner, OvnHost, OvnOptions, OvnPaths,
};
use opl_types::node::NodeSpec;
use opl_types::resource::{Kind, Resource};
use opl_types::sdn::{SDN_NAME, SdnCentral, SdnPending, SdnPhase, SdnSpec, SdnStatus};
use opl_types::task::{Task, TaskOp, TaskSpec};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const TAG: &str = "abcdef012345";
const CID_NB: &str = "5f8bebd0-7593-4d24-8db5-23678d1f6ce6";
const CID_SB: &str = "c3815d2b-00fb-45ec-afea-de92f8f9bf3e";
const OVN_VERSION: &str =
    include_str!("../../opl-driver/src/ovn/fixtures/ovn-controller-version.txt");
const OVS_VERSION: &str = include_str!("../../opl-driver/src/ovn/fixtures/ovs-vsctl-version.txt");
const FRESH_IDS: &str = include_str!("../../opl-driver/src/ovn/fixtures/external-ids-fresh.json");
const NB_LEADER: &str =
    include_str!("../../opl-driver/src/ovn/fixtures/cluster-status-nb-leader.txt");
const SB_LEADER: &str =
    include_str!("../../opl-driver/src/ovn/fixtures/cluster-status-sb-leader.txt");
const FOLLOWER: &str =
    include_str!("../../opl-driver/src/ovn/fixtures/cluster-status-follower.txt");

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

/// 가짜 실행기에 넘기되 `ovsdb-tool create-cluster`·`join-cluster`가 DB 파일을 만든 것처럼 하고, `cluster/leave`한
/// 서버의 `cluster/status`는 떠난 것(`unknown cluster`)으로 답한다.
struct Runner(FakeRunner, std::sync::Mutex<Vec<String>>);

#[async_trait]
impl CommandRunner for Runner {
    async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
        if cmd.program == "ovsdb-tool"
            && matches!(cmd.args[0].as_str(), "create-cluster" | "join-cluster")
        {
            std::fs::write(&cmd.args[1], "db").unwrap();
        }
        if cmd.program == "ovs-appctl" {
            let ctl = cmd.args.get(3).cloned().unwrap_or_default();
            if cmd.args.iter().any(|a| a == "cluster/status")
                && self.1.lock().unwrap().contains(&ctl)
            {
                self.0.run(cmd).await?;
                return Ok(CmdOutput {
                    status: 2,
                    stdout: b"unknown cluster\n".to_vec(),
                    stderr: Vec::new(),
                });
            }
            if cmd.args.iter().any(|a| a == "cluster/leave") {
                self.1.lock().unwrap().push(ctl);
            }
        }
        self.0.run(cmd).await
    }
}

/// 가짜 실행기와 임시 디렉터리를 쓰는 `OvnHost`
struct Host {
    _dir: TempDir,
    root: PathBuf,
    runner: Arc<Runner>,
    host: Arc<OvnHost>,
}

impl Host {
    fn new() -> Self {
        Self::with_wait_limit(Duration::from_millis(300))
    }

    /// Raft 기다리기 한도를 `wait_limit`으로 둔다. 시간 제한이 없음을 보는 시험은 느린 노드(svc01)에서도
    /// 기다리기가 먼저 끝나지 않게 넉넉히 둔다.
    fn with_wait_limit(wait_limit: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let paths = OvnPaths {
            etc: root.join("etc/opl/ovn"),
            var_lib: root.join("var/lib/opl/ovn"),
            run: root.join("run/opl/ovn"),
            log: root.join("var/log/opl"),
            systemd: root.join("etc/systemd/system"),
            pki: root.join("pki"),
            schemas: root.join("usr/share/ovn"),
            distro_nb_db: root.join("var/lib/ovn/ovnnb_db.db"),
            ovs_system_id: root.join("etc/openvswitch/system-id.conf"),
        };
        std::fs::create_dir_all(&paths.pki).unwrap();
        for f in ["ca.crt", "node.crt", "node.key"] {
            std::fs::write(paths.pki.join(f), f).unwrap();
        }
        let options = OvnOptions {
            wait_step: Duration::from_millis(5),
            wait_limit,
            any_os: true,
            ..OvnOptions::default()
        };
        let runner = Arc::new(Runner(FakeRunner::new(), Default::default()));
        let host = Arc::new(OvnHost::new(runner.clone(), paths, options, TAG.into()));
        Self {
            _dir: dir,
            root,
            runner,
            host,
        }
    }

    fn fake(&self) -> &FakeRunner {
        &self.runner.0
    }

    fn p(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }

    fn calls_of(&self, prefix: &str) -> Vec<String> {
        self.fake()
            .calls()
            .into_iter()
            .filter(|c| c.starts_with(prefix))
            .collect()
    }

    /// 꾸러미가 다 깔려 있고 OVN 24.03.8이다. OVS DB는 꾸러미가 막 만든 것(무작위 system-id)이다.
    fn installed(&self) {
        let out: String = CENTRAL_PACKAGES
            .iter()
            .map(|p| format!("{p} install ok installed\n"))
            .collect();
        self.fake().reply("dpkg-query", CmdOutput::ok(out));
        self.fake()
            .reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        self.fake()
            .reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
        self.fake().reply(
            "ovs-vsctl --format=json --columns=external_ids",
            CmdOutput::ok(FRESH_IDS),
        );
        self.fake()
            .reply("systemctl show -p LoadState", CmdOutput::ok("loaded\n"));
    }

    /// 새 클러스터 DB의 id와 리더
    fn new_cluster(&self) {
        for (db, cid) in [("nb", CID_NB), ("sb", CID_SB)] {
            for file in [format!(".{db}.db.opl-new"), format!("{db}.db")] {
                self.fake().reply(
                    &format!(
                        "ovsdb-tool db-cid {}",
                        self.p(&format!("var/lib/opl/ovn/{file}"))
                    ),
                    CmdOutput::ok(format!("{cid}\n")),
                );
            }
        }
        self.status(NB_LEADER, SB_LEADER);
    }

    fn status(&self, nb: &str, sb: &str) {
        self.fake().reply(
            &format!(
                "ovs-appctl --timeout 5 -t {} cluster/status",
                self.p("run/opl/ovn/nb.ctl")
            ),
            CmdOutput::ok(nb),
        );
        self.fake().reply(
            &format!(
                "ovs-appctl --timeout 5 -t {} cluster/status",
                self.p("run/opl/ovn/sb.ctl")
            ),
            CmdOutput::ok(sb),
        );
    }
}

/// 가짜 OVN 단계를 쓰는 Task 실행기
struct Sdn {
    env: Env,
    host: Host,
    admin: Client,
}

impl Sdn {
    async fn start() -> Self {
        Self::start_with(Duration::from_secs(600)).await
    }

    /// Task 시간 제한을 `timeout`으로 둔다(SDN Task는 받지 않는다).
    async fn start_with(timeout: Duration) -> Self {
        let mut env = Env::start().await;
        let host = if timeout < Duration::from_secs(1) {
            Host::with_wait_limit(Duration::from_secs(30))
        } else {
            Host::new()
        };
        env.stop_runner().await;
        let ovn = host.host.clone();
        Box::pin(
            env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
                c.sdn = Some(ovn);
                c.timeout = timeout;
            }),
        )
        .await;
        let admin = env.admin().await;
        let s = Self { env, host, admin };
        s.underlay(Some("10.1.0.5")).await;
        s
    }

    async fn underlay(&self, ip: Option<&str>) {
        let me = self
            .env
            .cluster
            .get::<NodeSpec>(self.env.node)
            .unwrap()
            .unwrap();
        let mut status = me.status;
        status.underlay = ip.map(|s| s.parse().unwrap());
        self.env
            .cluster
            .update_status::<NodeSpec>(me.id, &status)
            .await
            .unwrap();
    }

    /// `Sdn` 리소스를 만든다.
    async fn create(&self, status: SdnStatus) -> Uuid {
        let mut sdn: opl_types::sdn::Sdn = Resource::new(SDN_NAME, SdnSpec::default());
        sdn.status = status;
        self.env.cluster.create(&sdn).await.unwrap();
        sdn.id
    }

    fn sdn(&self) -> SdnStatus {
        self.env
            .cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .unwrap()
            .unwrap()
            .status
    }

    /// 자리(`pending`)를 잡은 Task를 미리 정한다(API가 하는 일).
    fn task(&self, op: TaskOp, target: Uuid) -> Task {
        let mut task: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target,
                node: self.env.node,
            },
        );
        task.name = task.id.to_string();
        task
    }

    /// Task를 만들고 끝날 때까지 기다린다.
    async fn run(&self, task: Task) -> Value {
        self.env.cluster.create(&task).await.unwrap();
        self.admin.wait_task(&json!({ "id": task.id })).await
    }

    async fn run_op(&self, op: TaskOp, target: Uuid) -> Value {
        self.run(self.task(op, target)).await
    }

    /// 이 Task가 자리를 잡은 상태로 `status`를 고친다.
    async fn hold(&self, task: &Task) {
        let sdn = self
            .env
            .cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .unwrap()
            .unwrap();
        let mut status = sdn.status;
        status.pending = Some(SdnPending {
            op: task.spec.op.clone(),
            node: task.spec.node,
            task: task.id,
        });
        self.env
            .cluster
            .update_status::<SdnSpec>(sdn.id, &status)
            .await
            .unwrap();
    }

    /// 이 노드 하나가 중앙인 준비된 SDN
    fn ready(&self) -> SdnStatus {
        SdnStatus {
            phase: SdnPhase::Ready,
            centrals: vec![SdnCentral {
                node: Uuid::nil(),
                address: ip("10.0.0.9"),
            }],
            release: Some("24.03.8".into()),
            init_node: Some(Uuid::nil()),
            nb_cid: Some(CID_NB.parse().unwrap()),
            sb_cid: Some(CID_SB.parse().unwrap()),
            ..SdnStatus::default()
        }
    }
}

fn state(task: &Value) -> &str {
    task["status"]["state"].as_str().unwrap()
}

fn error(task: &Value) -> &str {
    task["status"]["error"].as_str().unwrap_or_default()
}

/// 첫 세우기: 확인 → 꾸러미 → TLS·배포판 단위 끄기 → NB·SB 클러스터(id를 Raft에 적은 뒤 제자리에) → Orbivisor 단위 →
/// 리더 → chassis. 끝나면 `ready`, 중앙(노드 주소의 IP), 판·클러스터 id가 Raft에 있고 자리는 놓인다.
#[tokio::test]
async fn init_sets_up_the_first_central_and_chassis() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.new_cluster();
    let task = s.task(TaskOp::SdnInit { address: None }, Uuid::nil());
    let id = s
        .create(SdnStatus {
            init_node: Some(s.env.node),
            pending: Some(SdnPending {
                op: task.spec.op.clone(),
                node: s.env.node,
                task: task.id,
            }),
            ..SdnStatus::default()
        })
        .await;
    let task = Task {
        spec: TaskSpec {
            target: id,
            ..task.spec
        },
        ..task
    };
    let (revision, _) = s.env.cluster.list::<TaskSpec>().unwrap();
    let mut watch = s.env.cluster.watch(revision).unwrap();
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");

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
        if let Some(p) = t.status.progress
            && stages.last() != Some(&p.stage)
        {
            stages.push(p.stage);
        }
    }
    assert_eq!(
        stages,
        [
            "checking",
            "installing-packages",
            "writing-tls",
            "creating-cluster",
            "starting",
            "waiting-for-leader",
            "configuring-chassis",
        ]
    );

    let status = s.sdn();
    assert_eq!(status.phase, SdnPhase::Ready);
    assert_eq!(
        status.centrals,
        [SdnCentral {
            node: s.env.node,
            address: ip("127.0.0.1")
        }]
    );
    assert_eq!(status.release.as_deref(), Some("24.03.8"));
    assert_eq!(status.nb_cid, Some(CID_NB.parse().unwrap()));
    assert_eq!(status.sb_cid, Some(CID_SB.parse().unwrap()));
    assert_eq!((status.pending, status.last_error), (None, None));

    // DB는 제자리에, 단위는 노드 주소에만 듣는다.
    assert!(h.root.join("var/lib/opl/ovn/nb.db").exists());
    assert!(h.root.join("var/lib/opl/ovn/sb.db").exists());
    let unit =
        std::fs::read_to_string(h.root.join("etc/systemd/system/opl-ovn-nb.service")).unwrap();
    assert!(unit.contains("--remote=pssl:6641:127.0.0.1 "), "{unit}");
    let northd =
        std::fs::read_to_string(h.root.join("etc/systemd/system/opl-ovn-northd.service")).unwrap();
    assert!(
        northd.contains("--ovnsb-db=ssl:127.0.0.1:6642 "),
        "{northd}"
    );
    assert_eq!(
        std::fs::read_to_string(h.root.join("etc/opl/ovn/node.key")).unwrap(),
        "node.key"
    );
    assert_eq!(
        h.calls_of("systemctl disable"),
        [
            "systemctl disable --now ovn-central.service ovn-northd.service \
          ovn-ovsdb-server-nb.service ovn-ovsdb-server-sb.service"
        ]
    );
    assert_eq!(
        h.calls_of("ovsdb-tool create-cluster")
            .iter()
            .map(|c| c.rsplit(' ').next().unwrap().to_string())
            .collect::<Vec<_>>(),
        ["ssl:127.0.0.1:6643", "ssl:127.0.0.1:6644"]
    );
    assert!(h.calls_of("apt-get").is_empty());
    let set = h.calls_of("ovs-vsctl set Open_vSwitch");
    assert_eq!(set.len(), 1, "{set:?}");
    assert!(
        set[0].contains(&format!("external_ids:system-id=\"{}\"", s.env.node))
            && set[0].contains("external_ids:ovn-remote=\"ssl:127.0.0.1:6642\"")
            && set[0].contains("external_ids:ovn-encap-ip=\"10.1.0.5\"")
            && set[0].contains(&format!("external_ids:orbivisor-cluster=\"{TAG}\"")),
        "{set:?}"
    );
    assert_eq!(
        std::fs::read_to_string(h.root.join("etc/openvswitch/system-id.conf")).unwrap(),
        format!("{}\n", s.env.node)
    );

    // 다시 하면 이어서 한다(DB를 다시 만들지 않고 단위도 그대로).
    let again = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert_eq!(state(&again), "succeeded", "{again}");
    assert_eq!(h.calls_of("ovsdb-tool create-cluster").len(), 2);
    assert_eq!(h.calls_of("systemctl daemon-reload").len(), 1);
}

/// 리더가 서지 않으면 실패하고 이유를 리소스에 남긴다(phase는 그대로, 자리는 놓는다). 다시 하면 DB를 다시 만들지 않고
/// 이어서 `ready`가 된다.
#[tokio::test]
async fn a_failed_init_records_the_error_and_resumes() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.new_cluster();
    h.status(NB_LEADER, FOLLOWER);
    let task = s.task(TaskOp::SdnInit { address: None }, Uuid::nil());
    let id = s
        .create(SdnStatus {
            init_node: Some(s.env.node),
            pending: Some(SdnPending {
                op: task.spec.op.clone(),
                node: s.env.node,
                task: task.id,
            }),
            ..SdnStatus::default()
        })
        .await;
    let task = Task {
        spec: TaskSpec {
            target: id,
            ..task.spec
        },
        ..task
    };
    let done = s.run(task).await;
    assert_eq!(state(&done), "failed");
    assert!(
        error(&done).contains("southbound database to elect a leader"),
        "{done}"
    );
    let status = s.sdn();
    assert_eq!(status.phase, SdnPhase::Bootstrapping);
    assert_eq!(status.last_error.as_deref(), Some(error(&done)));
    assert_eq!(status.pending, None);
    assert_eq!(status.nb_cid, Some(CID_NB.parse().unwrap()));

    h.status(NB_LEADER, SB_LEADER);
    let done = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert_eq!(h.calls_of("ovsdb-tool create-cluster").len(), 2);
    let status = s.sdn();
    assert_eq!((status.phase, status.last_error), (SdnPhase::Ready, None));
}

/// 주어진 주소로 세우고, underlay가 없으면 꾸러미를 깔기 전에 멈춘다.
#[tokio::test]
async fn init_address_and_underlay() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.new_cluster();
    let id = s
        .create(SdnStatus {
            init_node: Some(s.env.node),
            ..SdnStatus::default()
        })
        .await;
    s.underlay(None).await;
    let done = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert!(error(&done).contains("has no underlay address"), "{done}");
    assert!(h.fake().calls().is_empty(), "{:?}", h.fake().calls());

    s.underlay(Some("10.1.0.5")).await;
    let done = s
        .run_op(
            TaskOp::SdnInit {
                address: Some(ip("10.2.0.7")),
            },
            id,
        )
        .await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert_eq!(s.sdn().centrals[0].address, ip("10.2.0.7"));
    let unit =
        std::fs::read_to_string(h.root.join("etc/systemd/system/opl-ovn-sb.service")).unwrap();
    assert!(unit.contains("--remote=pssl:6642:10.2.0.7 "), "{unit}");
}

/// 남의 OVN(ovn-remote가 이미 있음)이 있는 노드는 거절하고 아무것도 깔거나 바꾸지 않는다. 다른 노드의 첫 세우기는 멈춘다.
#[tokio::test]
async fn init_refuses_foreign_ovn_and_moved_setups() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.fake().reply(
        "ovs-vsctl --format=json --columns=external_ids",
        CmdOutput::ok(
            r#"{"data":[[["map",[["ovn-remote","tcp:192.0.2.1:6642"],["system-id","x"]]]]],"headings":["external_ids"]}"#,
        ),
    );
    let id = s
        .create(SdnStatus {
            init_node: Some(s.env.node),
            ..SdnStatus::default()
        })
        .await;
    let done = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert_eq!(state(&done), "failed");
    assert!(error(&done).contains("another OVN deployment"), "{done}");
    assert!(h.calls_of("apt-get").is_empty() && h.calls_of("systemctl").is_empty());
    assert!(
        s.sdn()
            .last_error
            .unwrap()
            .contains("another OVN deployment")
    );

    // 첫 세우기를 다른 노드로 옮겼으면 멈춘다(이유를 덮지 않는다).
    let other = Sdn::start().await;
    let id = other
        .create(SdnStatus {
            init_node: Some(Uuid::nil()),
            last_error: Some("old".into()),
            ..SdnStatus::default()
        })
        .await;
    let done = other.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert!(error(&done).contains("was moved off node"), "{done}");
    assert_eq!(other.sdn().last_error.as_deref(), Some("old"));
    assert!(other.host.fake().calls().is_empty());
}

/// 중앙 더하기: 자리를 가진 Task만, 기존 중앙의 Raft 주소로 합류하고 멤버가 되면 중앙 목록에 더한다. 둘이면 경고
#[tokio::test]
async fn central_add_joins_the_cluster() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.status(
        FOLLOWER,
        &SB_LEADER.replace("Role: leader", "Role: follower"),
    );
    let id = s.create(s.ready()).await;

    // 자리가 없으면 돌지 않는다.
    let done = s.run_op(TaskOp::SdnCentralAdd, id).await;
    assert!(
        error(&done).contains("does not hold the SDN central slot"),
        "{done}"
    );

    let task = s.task(TaskOp::SdnCentralAdd, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert!(
        done["status"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("now has 2 central nodes"),
        "{done}"
    );
    let joins = h.calls_of("ovsdb-tool join-cluster");
    assert_eq!(joins.len(), 2);
    assert!(
        joins[0].ends_with("OVN_Northbound ssl:127.0.0.1:6643 ssl:10.0.0.9:6643"),
        "{joins:?}"
    );
    let status = s.sdn();
    assert_eq!(
        status
            .centrals
            .iter()
            .map(|c| c.address)
            .collect::<Vec<_>>(),
        [ip("10.0.0.9"), ip("127.0.0.1")]
    );
    assert_eq!(status.pending, None);
    let northd =
        std::fs::read_to_string(h.root.join("etc/systemd/system/opl-ovn-northd.service")).unwrap();
    assert!(
        northd.contains("--ovnnb-db=ssl:10.0.0.9:6641,ssl:127.0.0.1:6641 "),
        "{northd}"
    );
    let set = h.calls_of("ovs-vsctl set Open_vSwitch");
    assert!(
        set[0].contains("ovn-remote=\"ssl:10.0.0.9:6642,ssl:127.0.0.1:6642\""),
        "{set:?}"
    );
}

/// 다른 클러스터에 붙은 DB(클러스터 id가 다름)는 멤버로 보지 않는다.
#[tokio::test]
async fn central_add_needs_the_same_cluster() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.status(FOLLOWER, FOLLOWER);
    let mut ready = s.ready();
    ready.nb_cid = Some(Uuid::nil());
    let id = s.create(ready).await;
    let task = s.task(TaskOp::SdnCentralAdd, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert!(
        error(&done).contains("northbound database to join the cluster"),
        "{done}"
    );
    assert_eq!(s.sdn().centrals.len(), 1);
    assert_eq!(s.sdn().pending, None);
}

/// 중앙 빼기: 마지막 중앙은 뺄 수 없다. 둘이면 떠나고, 단위를 멈추고, DB는 이름만 바꾼 뒤 목록에서 뺀다.
#[tokio::test]
async fn central_remove_leaves_and_keeps_the_files() {
    let s = Sdn::start().await;
    let h = &s.host;
    let mut ready = s.ready();
    ready.centrals = vec![SdnCentral {
        node: s.env.node,
        address: ip("127.0.0.1"),
    }];
    let id = s.create(ready).await;
    let task = s.task(TaskOp::SdnCentralRemove, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert!(error(&done).contains("last OVN central node"), "{done}");

    let id2 = {
        let sdn = s
            .env
            .cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .unwrap()
            .unwrap();
        let mut status = sdn.status;
        status.centrals.insert(
            0,
            SdnCentral {
                node: Uuid::nil(),
                address: ip("10.0.0.9"),
            },
        );
        s.env
            .cluster
            .update_status::<SdnSpec>(sdn.id, &status)
            .await
            .unwrap();
        sdn.id
    };
    assert_eq!(id, id2);
    std::fs::create_dir_all(h.root.join("var/lib/opl/ovn")).unwrap();
    std::fs::write(h.root.join("var/lib/opl/ovn/nb.db"), "nb").unwrap();
    std::fs::write(h.root.join("var/lib/opl/ovn/sb.db"), "sb").unwrap();
    let left = CmdOutput {
        status: 2,
        stdout: b"unknown cluster\n".to_vec(),
        stderr: Vec::new(),
    };
    for db in ["nb", "sb"] {
        h.fake().reply_seq(
            &format!(
                "ovs-appctl --timeout 5 -t {} cluster/status",
                h.p(&format!("run/opl/ovn/{db}.ctl"))
            ),
            vec![CmdOutput::ok(FOLLOWER), left.clone()],
        );
    }
    let task = s.task(TaskOp::SdnCentralRemove, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert_eq!(
        h.calls_of("ovs-appctl --timeout 5 -t")
            .iter()
            .filter(|c| c.contains("cluster/leave"))
            .count(),
        2
    );
    assert_eq!(
        h.calls_of("systemctl disable"),
        ["systemctl disable --now opl-ovn-nb.service opl-ovn-sb.service opl-ovn-northd.service"]
    );
    assert!(!h.root.join("var/lib/opl/ovn/nb.db").exists());
    let kept: Vec<String> = std::fs::read_dir(h.root.join("var/lib/opl/ovn"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".left-"))
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    let status = s.sdn();
    assert_eq!(status.centrals.len(), 1);
    assert_eq!(status.centrals[0].node, Uuid::nil());
    assert_eq!(status.pending, None);

    // 다시 하면(이미 빠졌다) 아무것도 하지 않는다.
    let task = s.task(TaskOp::SdnCentralRemove, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
}

/// chassis 설치: SDN이 준비돼야 하고, 대상은 노드다. chassis 꾸러미만 깔고 SB는 중앙 모두다.
#[tokio::test]
async fn chassis_install() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.fake().reply(
        "dpkg-query",
        CmdOutput::ok("openvswitch-switch install ok installed\n"),
    );
    let id = s
        .create(SdnStatus {
            init_node: Some(Uuid::nil()),
            ..SdnStatus::default()
        })
        .await;
    let done = s.run_op(TaskOp::SdnChassisInstall, s.env.node).await;
    assert!(error(&done).contains("not ready yet"), "{done}");

    let mut status = s.ready();
    status.centrals.push(SdnCentral {
        node: Uuid::now_v7(),
        address: ip("10.0.0.10"),
    });
    s.env
        .cluster
        .update_status::<SdnSpec>(id, &status)
        .await
        .unwrap();
    let done = s.run_op(TaskOp::SdnChassisInstall, s.env.node).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert_eq!(
        h.calls_of("apt-get install"),
        ["apt-get install -y -q -o DPkg::Lock::Timeout=600 ovn-host"]
    );
    // chassis만이면 배포판 중앙 단위를 건드리지 않는다.
    assert!(h.calls_of("systemctl disable").is_empty());
    assert!(h.calls_of("systemctl show").is_empty());
    let set = h.calls_of("ovs-vsctl set Open_vSwitch");
    assert!(
        set[0].contains("ovn-remote=\"ssl:10.0.0.9:6642,ssl:10.0.0.10:6642\""),
        "{set:?}"
    );
    assert_eq!(
        h.calls_of("systemctl"),
        [
            "systemctl enable --now ovn-host.service",
            "systemctl start ovn-controller.service"
        ]
    );
    assert_eq!(s.sdn().pending, None);
}

/// SDN Task는 Task 시간 제한을 받지 않는다(apt·Raft 기다리기). 실행기가 없는 노드에서는 실패한다.
#[tokio::test]
async fn sdn_tasks_have_no_timeout() {
    let s = Sdn::start_with(Duration::from_millis(100)).await;
    let h = &s.host;
    h.installed();
    h.new_cluster();
    let follower_sb = SB_LEADER.replace("Role: leader", "Role: follower");
    let mut seq = vec![CmdOutput::ok(follower_sb); 40];
    seq.push(CmdOutput::ok(SB_LEADER));
    h.fake().reply_seq(
        &format!(
            "ovs-appctl --timeout 5 -t {} cluster/status",
            h.p("run/opl/ovn/sb.ctl")
        ),
        seq,
    );
    let id = s
        .create(SdnStatus {
            init_node: Some(s.env.node),
            ..SdnStatus::default()
        })
        .await;
    let started = std::time::Instant::now();
    let done = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert!(started.elapsed() > Duration::from_millis(150));

    let mut env = Env::start().await;
    let admin = env.admin().await;
    env.stop_runner().await;
    Box::pin(env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |_| {}))
        .await;
    let mut sdn: opl_types::sdn::Sdn = Resource::new(SDN_NAME, SdnSpec::default());
    sdn.status.init_node = Some(env.node);
    env.cluster.create(&sdn).await.unwrap();
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::SdnInit { address: None },
            target: sdn.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    let done = admin.wait_task(&json!({ "id": task.id })).await;
    assert!(
        error(&done).contains("cannot run SDN (OVN) tasks"),
        "{done}"
    );
}

/// 준비된 SDN에서 첫 세우기를 다시 할 때 DB가 없으면 새 클러스터를 만들지 않고 실패한다(리뷰 1 I-7 — 다른 중앙과 갈라진
/// 새 NB·SB가 서지 않게).
#[tokio::test]
async fn init_on_a_ready_sdn_never_creates_a_second_cluster() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.new_cluster();
    let mut ready = s.ready();
    ready.centrals = vec![SdnCentral {
        node: s.env.node,
        address: ip("127.0.0.1"),
    }];
    ready.init_node = Some(s.env.node);
    let id = s.create(ready).await;
    let done = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert_eq!(state(&done), "failed", "{done}");
    assert!(error(&done).contains("database of node"), "{done}");
    assert!(error(&done).contains("is missing"), "{done}");
    assert!(h.calls_of("ovsdb-tool create-cluster").is_empty());
    assert_eq!(s.sdn().nb_cid, Some(CID_NB.parse().unwrap()));
}

/// 끝나지 않은 첫 세우기를 다른 주소로 다시 하면 아직 아무도 쓰지 않은 DB(옛 Raft 주소)를 이름만 바꿔 치우고 새로
/// 만든다(리뷰 1 I-7).
#[tokio::test]
async fn an_unfinished_init_with_a_new_address_recreates_the_databases() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.new_cluster();
    h.status(NB_LEADER, FOLLOWER);
    let id = s
        .create(SdnStatus {
            init_node: Some(s.env.node),
            ..SdnStatus::default()
        })
        .await;
    let done = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert_eq!(state(&done), "failed", "{done}");
    assert_eq!(h.calls_of("ovsdb-tool create-cluster").len(), 2);

    for db in ["nb", "sb"] {
        let port = if db == "nb" { 6643 } else { 6644 };
        h.fake().reply(
            &format!(
                "ovsdb-tool db-local-address {}",
                h.p(&format!("var/lib/opl/ovn/{db}.db"))
            ),
            CmdOutput::ok(format!("ssl:127.0.0.1:{port}\n")),
        );
    }
    // 같은 주소로 다시 하면 DB는 그대로다.
    let done = s.run_op(TaskOp::SdnInit { address: None }, id).await;
    assert_eq!(state(&done), "failed", "{done}");
    assert_eq!(h.calls_of("ovsdb-tool create-cluster").len(), 2);
    let stops = || h.calls_of("systemctl disable --now opl-ovn").len();
    assert_eq!(stops(), 0);

    h.status(NB_LEADER, SB_LEADER);
    let done = s
        .run_op(
            TaskOp::SdnInit {
                address: Some(ip("10.2.0.7")),
            },
            id,
        )
        .await;
    assert_eq!(state(&done), "succeeded", "{done}");
    let creates = h.calls_of("ovsdb-tool create-cluster");
    assert_eq!(creates.len(), 4, "{creates:?}");
    assert!(creates[2].ends_with("ssl:10.2.0.7:6643"), "{creates:?}");
    let kept: Vec<String> = std::fs::read_dir(h.root.join("var/lib/opl/ovn"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".left-"))
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert_eq!(s.sdn().centrals[0].address, ip("10.2.0.7"));
    // 치우기 전에 지난 시도의 서버를 멈춘다(리뷰 2 m-3 — 옛 서버의 압축이 새 DB를 덮지 않게). 한 번만.
    assert_eq!(stops(), 1);
    let calls = h.fake().calls();
    let stop = calls
        .iter()
        .position(|c| c.starts_with("systemctl disable --now opl-ovn"))
        .unwrap();
    let create = calls
        .iter()
        .rposition(|c| c.starts_with("ovsdb-tool create-cluster"))
        .unwrap();
    assert!(stop < create, "{calls:?}");
}

/// 중앙 더하기가 합류한 뒤 실패하면 클러스터를 다시 떠나고 단위를 멈추고 DB는 이름만 바꾼다(리뷰 1 I-7 — 목록에 없는
/// 멤버가 남지 않게).
#[tokio::test]
async fn a_failed_central_add_leaves_the_cluster_again() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.status(FOLLOWER, FOLLOWER);
    let mut ready = s.ready();
    ready.nb_cid = Some(Uuid::nil());
    let id = s.create(ready).await;
    let task = s.task(TaskOp::SdnCentralAdd, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "failed", "{done}");
    assert!(
        error(&done).contains("left the OVN cluster again"),
        "{done}"
    );
    assert_eq!(
        h.calls_of("ovs-appctl --timeout 5 -t")
            .iter()
            .filter(|c| c.contains("cluster/leave"))
            .count(),
        2
    );
    assert_eq!(
        h.calls_of("systemctl disable --now opl-ovn"),
        ["systemctl disable --now opl-ovn-nb.service opl-ovn-sb.service opl-ovn-northd.service"]
    );
    assert!(!h.root.join("var/lib/opl/ovn/nb.db").exists());
    assert_eq!(s.sdn().centrals.len(), 1);
}

/// 중앙 목록에 없는데 이 SDN의 DB가 남은 노드(반쯤 합류한 멤버)는 중앙 빼기로 떠난다. 중앙 목록은 그대로다.
#[tokio::test]
async fn a_half_joined_member_can_be_removed() {
    let s = Sdn::start().await;
    let h = &s.host;
    let id = s.create(s.ready()).await;
    std::fs::create_dir_all(h.root.join("var/lib/opl/ovn")).unwrap();
    std::fs::write(h.root.join("var/lib/opl/ovn/nb.db"), "nb").unwrap();
    std::fs::write(h.root.join("var/lib/opl/ovn/sb.db"), "sb").unwrap();
    h.status(FOLLOWER, FOLLOWER);
    let task = s.task(TaskOp::SdnCentralRemove, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert!(error(&done).contains("half joined"), "{done}");
    assert!(!h.root.join("var/lib/opl/ovn/nb.db").exists());
    assert_eq!(s.sdn().centrals.len(), 1);
    assert_eq!(s.sdn().pending, None);
}

/// 리뷰 2 N-3: 합류 중인 서버(또는 돌지 않고 클러스터 id를 아직 모르는 DB)는 Raft 구성에 없다 — 떠나기 없이 단위를 멈추고
/// DB를 치운다(OVS는 합류 중 떠나기를 거절해 빼기가 영영 실패했다).
#[tokio::test]
async fn a_joining_member_is_retired_without_leaving() {
    let s = Sdn::start().await;
    let h = &s.host;
    let id = s.create(s.ready()).await;
    std::fs::create_dir_all(h.root.join("var/lib/opl/ovn")).unwrap();
    std::fs::write(h.root.join("var/lib/opl/ovn/nb.db"), "nb").unwrap();
    std::fs::write(h.root.join("var/lib/opl/ovn/sb.db"), "sb").unwrap();
    let joining = include_str!("../../opl-driver/src/ovn/fixtures/cluster-status-joining.txt");
    h.fake().reply(
        &format!(
            "ovs-appctl --timeout 5 -t {} cluster/status",
            h.p("run/opl/ovn/nb.ctl")
        ),
        CmdOutput::ok(joining),
    );
    // SB 서버는 돌지 않고 DB의 클러스터 id를 아직 모른다.
    h.fake().reply(
        &format!(
            "ovs-appctl --timeout 5 -t {} cluster/status",
            h.p("run/opl/ovn/sb.ctl")
        ),
        CmdOutput::fail(1, "ovs-appctl: cannot connect"),
    );
    let sb = h.p("var/lib/opl/ovn/sb.db");
    h.fake().reply(
        &format!("ovsdb-tool db-cid {sb}"),
        CmdOutput::ok(format!("{sb}: cluster ID not yet known\n")),
    );
    let task = s.task(TaskOp::SdnCentralRemove, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert!(
        h.fake()
            .calls()
            .iter()
            .all(|c| !c.contains("cluster/leave")),
        "{:?}",
        h.fake().calls()
    );
    assert_eq!(h.calls_of("systemctl disable --now opl-ovn").len(), 1);
    assert!(!h.root.join("var/lib/opl/ovn/nb.db").exists());
    assert!(!h.root.join("var/lib/opl/ovn/sb.db").exists());
}

/// 리뷰 2 N-1·N-2: 살아 있는 중앙이 꺼진(지운) 노드의 Raft 주소를 NB·SB에서 내쫓고 중앙 목록과 지운 노드 기록에서 뺀다.
/// 마지막 중앙은 내쫓지 않고, 목록에 없는 주소면 내쫓을 것 없이 기록만 지운다.
#[tokio::test]
async fn a_live_central_kicks_a_dead_one_out() {
    let s = Sdn::start().await;
    let h = &s.host;
    let gone = Uuid::now_v7();
    let mut ready = s.ready();
    ready.centrals = vec![
        SdnCentral {
            node: s.env.node,
            address: ip("127.0.0.1"),
        },
        SdnCentral {
            node: gone,
            address: ip("127.0.0.2"),
        },
    ];
    ready.departed = vec![opl_types::sdn::SdnDeparted {
        node: gone,
        name: "dead".into(),
        address: ip("127.0.0.2"),
    }];
    let id = s.create(ready).await;
    let two = include_str!("../../opl-driver/src/ovn/fixtures/cluster-status-leader-two.txt");
    let one = |text: &str| {
        text.lines()
            .filter(|l| !l.contains("ssl:127.0.0.2"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for (db, port) in [("nb", "6643"), ("sb", "6644")] {
        let text = two.replace("6643", port).replace(
            "OVN_Northbound",
            if db == "nb" {
                "OVN_Northbound"
            } else {
                "OVN_Southbound"
            },
        );
        h.fake().reply_seq(
            &format!(
                "ovs-appctl --timeout 5 -t {} cluster/status",
                h.p(&format!("run/opl/ovn/{db}.ctl"))
            ),
            vec![CmdOutput::ok(text.clone()), CmdOutput::ok(one(&text))],
        );
    }
    let op = TaskOp::SdnCentralKick {
        node: gone,
        address: ip("127.0.0.2"),
    };
    let task = s.task(op.clone(), id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    let kicks = h.calls_of("ovs-appctl --timeout 5 -t");
    let kicks: Vec<&String> = kicks
        .iter()
        .filter(|c| c.contains("cluster/kick"))
        .collect();
    assert_eq!(kicks.len(), 2, "{kicks:?}");
    assert!(
        kicks[0].ends_with("cluster/kick OVN_Northbound ssl:127.0.0.2:6643"),
        "{kicks:?}"
    );
    assert!(
        kicks[1].ends_with("cluster/kick OVN_Southbound ssl:127.0.0.2:6644"),
        "{kicks:?}"
    );
    let status = s.sdn();
    assert_eq!(status.centrals.len(), 1);
    assert!(status.departed.is_empty());
    assert_eq!(status.pending, None);

    // 다시 하면 내쫓을 것이 없다(경고).
    let task = s.task(op, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert!(error(&done).contains("nothing to kick out"), "{done}");
    // 마지막 중앙(이 노드)은 내쫓지 않는다.
    let task = s.task(
        TaskOp::SdnCentralKick {
            node: s.env.node,
            address: ip("127.0.0.1"),
        },
        id,
    );
    s.hold(&task).await;
    let done = s.run(task).await;
    assert!(error(&done).contains("last OVN central"), "{done}");
}

/// 잠금을 켠 `Sdn` 리소스
async fn create_locked(s: &Sdn, status: SdnStatus) -> Uuid {
    let mut sdn: opl_types::sdn::Sdn = Resource::new(SDN_NAME, SdnSpec { lockdown: true });
    sdn.status = status;
    s.env.cluster.create(&sdn).await.unwrap();
    sdn.id
}

/// 계획 67 §10.4: 잠금을 켠 SDN의 첫 세우기는 SB를 처음부터 설정 파일로 띄우고(northd는 6645) 그 중앙을 `applied`에
/// 적는다. 중앙 더하기도 잠금을 따르고, 빼기는 `applied`에서도 뺀다.
#[tokio::test]
async fn locked_centrals_start_with_the_sb_config_file() {
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.new_cluster();
    let task = s.task(TaskOp::SdnInit { address: None }, Uuid::nil());
    let id = create_locked(
        &s,
        SdnStatus {
            init_node: Some(s.env.node),
            pending: Some(SdnPending {
                op: task.spec.op.clone(),
                node: s.env.node,
                task: task.id,
            }),
            ..SdnStatus::default()
        },
    )
    .await;
    let task = Task {
        spec: TaskSpec {
            target: id,
            ..task.spec
        },
        ..task
    };
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    let status = s.sdn();
    assert_eq!(status.lockdown.unwrap().applied, [s.env.node]);
    let sb = std::fs::read_to_string(h.root.join("etc/systemd/system/opl-ovn-sb.service")).unwrap();
    assert!(sb.contains("--config-file="), "{sb}");
    assert!(h.root.join("etc/opl/ovn/sb.json").exists());
    let northd =
        std::fs::read_to_string(h.root.join("etc/systemd/system/opl-ovn-northd.service")).unwrap();
    assert!(
        northd.contains("--ovnsb-db=ssl:127.0.0.1:6645 "),
        "{northd}"
    );

    // 중앙 더하기: 잠긴 다른 중앙(10.0.0.9)은 6645, 새 중앙도 잠근 채 뜬다.
    let s = Sdn::start().await;
    let h = &s.host;
    h.installed();
    h.status(
        FOLLOWER,
        &SB_LEADER.replace("Role: leader", "Role: follower"),
    );
    let mut ready = s.ready();
    ready.lockdown = Some(opl_types::sdn::LockdownState {
        applied: vec![Uuid::nil()],
        ..Default::default()
    });
    let id = create_locked(&s, ready).await;
    let task = s.task(TaskOp::SdnCentralAdd, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    let northd =
        std::fs::read_to_string(h.root.join("etc/systemd/system/opl-ovn-northd.service")).unwrap();
    assert!(
        northd.contains("--ovnsb-db=ssl:10.0.0.9:6645,ssl:127.0.0.1:6645 "),
        "{northd}"
    );
    let mut applied = s.sdn().lockdown.unwrap().applied;
    applied.sort();
    let mut want = vec![Uuid::nil(), s.env.node];
    want.sort();
    assert_eq!(applied, want);

    // 중앙 빼기는 잠긴 중앙에서도 뺀다.
    std::fs::write(h.root.join("var/lib/opl/ovn/nb.db"), "nb").unwrap();
    std::fs::write(h.root.join("var/lib/opl/ovn/sb.db"), "sb").unwrap();
    let left = CmdOutput {
        status: 2,
        stdout: b"unknown cluster\n".to_vec(),
        stderr: Vec::new(),
    };
    for db in ["nb", "sb"] {
        h.fake().reply_seq(
            &format!(
                "ovs-appctl --timeout 5 -t {} cluster/status",
                h.p(&format!("run/opl/ovn/{db}.ctl"))
            ),
            vec![CmdOutput::ok(FOLLOWER), left.clone()],
        );
    }
    let task = s.task(TaskOp::SdnCentralRemove, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    assert_eq!(s.sdn().lockdown.unwrap().applied, [Uuid::nil()]);
}

/// 이 노드가 알린 SDN 상태를 쓴다(에이전트 대신).
async fn report_node_sdn(s: &Sdn, sdn: Option<opl_types::sdn::NodeSdn>) {
    let me = s.env.cluster.get::<NodeSpec>(s.env.node).unwrap().unwrap();
    let mut status = me.status;
    status.sdn = sdn;
    s.env
        .cluster
        .update_status::<NodeSpec>(me.id, &status)
        .await
        .unwrap();
}

/// `sdn-lockdown`(계획 67 §10.4): OVN을 부르지 않고 `SdnSpec.lockdown`을 쓴 뒤, 에이전트가 중앙을 잠가 `applied`에 적고
/// chassis가 허용 목록을 맞출 때까지 단계(`writing-spec` → `switching-centrals` → `applying-firewall`)로 기다린다.
/// 끝나면 `pending`을 놓는다. 그새 다른 요청이 반대로 바꾸면 실패한다.
#[tokio::test]
async fn the_lockdown_task_waits_for_the_agents() {
    let s = Sdn::start().await;
    let mut ready = s.ready();
    ready.centrals = vec![SdnCentral {
        node: s.env.node,
        address: ip("127.0.0.1"),
    }];
    let id = s.create(ready).await;
    report_node_sdn(
        &s,
        Some(opl_types::sdn::NodeSdn {
            chassis: opl_types::sdn::ChassisState::Ready,
            ..Default::default()
        }),
    )
    .await;
    // 에이전트 흉내: 잠금을 원하면 중앙을 잠그고, 다음에 허용 목록을 넣는다.
    let cluster = s.env.cluster.clone();
    let node = s.env.node;
    let agent = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let Some(sdn) = cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap() else {
                continue;
            };
            let applied = sdn
                .status
                .lockdown
                .as_ref()
                .is_some_and(|l| l.applied.contains(&node));
            if sdn.spec.lockdown && !applied {
                let mut status = sdn.status.clone();
                status
                    .lockdown
                    .get_or_insert_with(Default::default)
                    .applied
                    .push(node);
                let _ = cluster
                    .update_status_checked::<SdnSpec>(sdn.id, sdn.revision, &status)
                    .await;
                continue;
            }
            if sdn.spec.lockdown && applied {
                let me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
                let mut st = me.status;
                let s = st.sdn.get_or_insert_with(Default::default);
                if s.lockdown.as_ref().is_some_and(|l| l.firewall) {
                    continue;
                }
                s.lockdown = Some(opl_types::sdn::LockdownNodeState {
                    firewall: true,
                    allowed: 1,
                    error: None,
                });
                let _ = cluster.update_status::<NodeSpec>(node, &st).await;
            }
        }
    });
    let task = s.task(TaskOp::SdnLockdown { enabled: true }, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    assert_eq!(state(&done), "succeeded", "{done}");
    let sdn = s
        .env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert!(sdn.spec.lockdown);
    let lockdown = sdn.status.lockdown.unwrap();
    assert_eq!(lockdown.applied, [s.env.node]);
    assert!(
        !lockdown.pending && lockdown.error.is_none(),
        "{lockdown:?}"
    );
    assert_eq!(sdn.status.pending, None);
    // OVN 명령을 하나도 부르지 않았다.
    assert!(
        s.host.fake().calls().is_empty(),
        "{:?}",
        s.host.fake().calls()
    );
    agent.abort();

    // 다른 요청이 그새 끄면 실패하고 그 까닭을 남긴다.
    let task = s.task(TaskOp::SdnLockdown { enabled: true }, id);
    s.hold(&task).await;
    let sdn = s
        .env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    let mut status = sdn.status.clone();
    status.lockdown.as_mut().unwrap().applied.clear();
    s.env
        .cluster
        .update_status::<SdnSpec>(sdn.id, &status)
        .await
        .unwrap();
    let cluster = s.env.cluster.clone();
    let flip = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let sdn = cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap().unwrap();
        cluster
            .update_spec(sdn.id, None, sdn.labels, &SdnSpec { lockdown: false })
            .await
            .unwrap();
    });
    let done = s.run(task).await;
    flip.await.unwrap();
    assert_eq!(state(&done), "failed", "{done}");
    assert!(error(&done).contains("another request changed"), "{done}");
    let lockdown = s.sdn().lockdown.unwrap();
    assert!(!lockdown.pending, "{lockdown:?}");
    assert!(
        lockdown
            .error
            .as_deref()
            .is_some_and(|e| e.contains("another request")),
        "{lockdown:?}"
    );
}

/// 리뷰 2 I2: 중앙이 허용 목록을 Raft를 끊어 걷었으면 `sdn-lockdown`(켜기)은 실패하고 `SdnSpec.lockdown`을 다시 끈다
/// (반쯤 걸린 채 두지 않음 — 에이전트가 SB를 되돌리고 표를 걷는다). 까닭은 `lockdown.error`에 남는다.
#[tokio::test]
async fn a_failed_lockdown_turns_itself_off_again() {
    let s = Sdn::start().await;
    let mut ready = s.ready();
    ready.centrals = vec![SdnCentral {
        node: s.env.node,
        address: ip("127.0.0.1"),
    }];
    let id = s.create(ready).await;
    report_node_sdn(
        &s,
        Some(opl_types::sdn::NodeSdn {
            chassis: opl_types::sdn::ChassisState::Ready,
            lockdown: Some(opl_types::sdn::LockdownNodeState {
                firewall: false,
                allowed: 1,
                error: Some(format!(
                    "{} (OVN_Northbound has not heard from ssl:10.0.0.2:6643); removed it again",
                    opl_types::sdn::LOCKDOWN_RAFT_CUT
                )),
            }),
            ..Default::default()
        }),
    )
    .await;
    // 에이전트 흉내: 잠금을 원하면 중앙을 잠근다(표는 끊김으로 걷었다고 이미 알렸다).
    let cluster = s.env.cluster.clone();
    let node = s.env.node;
    let agent = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let Some(sdn) = cluster.get_by_name::<SdnSpec>(SDN_NAME).unwrap() else {
                continue;
            };
            let applied = sdn
                .status
                .lockdown
                .as_ref()
                .is_some_and(|l| l.applied.contains(&node));
            if sdn.spec.lockdown && !applied {
                let mut status = sdn.status.clone();
                status
                    .lockdown
                    .get_or_insert_with(Default::default)
                    .applied
                    .push(node);
                let _ = cluster
                    .update_status_checked::<SdnSpec>(sdn.id, sdn.revision, &status)
                    .await;
            }
        }
    });
    let task = s.task(TaskOp::SdnLockdown { enabled: true }, id);
    s.hold(&task).await;
    let done = s.run(task).await;
    agent.abort();
    assert_eq!(state(&done), "failed", "{done}");
    assert!(
        error(&done).contains("could not keep its address allow-list"),
        "{done}"
    );
    assert!(
        error(&done).contains("turned the OVN lockdown off again"),
        "{done}"
    );
    let sdn = s
        .env
        .cluster
        .get_by_name::<SdnSpec>(SDN_NAME)
        .unwrap()
        .unwrap();
    assert!(!sdn.spec.lockdown, "the failed turn-on must not stay on");
    let lockdown = sdn.status.lockdown.unwrap();
    assert!(!lockdown.pending, "{lockdown:?}");
    assert!(
        lockdown
            .error
            .as_deref()
            .is_some_and(|e| e.contains("turned the OVN lockdown off again")),
        "{lockdown:?}"
    );
    assert_eq!(sdn.status.pending, None);
}
