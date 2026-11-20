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

/// 가짜 실행기에 넘기되 `ovsdb-tool create-cluster`·`join-cluster`가 DB 파일을 만든 것처럼 한다.
struct Runner(FakeRunner);

#[async_trait]
impl CommandRunner for Runner {
    async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
        if cmd.program == "ovsdb-tool"
            && matches!(cmd.args[0].as_str(), "create-cluster" | "join-cluster")
        {
            std::fs::write(&cmd.args[1], "db").unwrap();
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
            wait_limit: Duration::from_millis(300),
            any_os: true,
            ..OvnOptions::default()
        };
        let runner = Arc::new(Runner(FakeRunner::new()));
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
        let host = Host::new();
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
        let mut sdn: opl_types::sdn::Sdn = Resource::new(SDN_NAME, SdnSpec {});
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
    let mut sdn: opl_types::sdn::Sdn = Resource::new(SDN_NAME, SdnSpec {});
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
