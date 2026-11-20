//! [`OvnHost`]의 단계 시험: 명령은 가짜 실행기가 받고(실제 출력 고정물), 파일은 임시 디렉터리에 쓴다.

use std::sync::Mutex;

use async_trait::async_trait;

use super::*;
use crate::ceph::runner::FakeRunner;

const TAG: &str = "abcdef012345";
const NODE: &str = "11111111-1111-1111-1111-111111111111";
const CID_NB: &str = "5f8bebd0-7593-4d24-8db5-23678d1f6ce6";
const CID_SB: &str = "c3815d2b-00fb-45ec-afea-de92f8f9bf3e";
const OVN_VERSION: &str = include_str!("../fixtures/ovn-controller-version.txt");
const OVS_VERSION: &str = include_str!("../fixtures/ovs-vsctl-version.txt");
const FRESH_IDS: &str = include_str!("../fixtures/external-ids-fresh.json");
const NB_LEADER: &str = include_str!("../fixtures/cluster-status-nb-leader.txt");
const SB_LEADER: &str = include_str!("../fixtures/cluster-status-sb-leader.txt");
const FOLLOWER: &str = include_str!("../fixtures/cluster-status-follower.txt");
const JOINING: &str = include_str!("../fixtures/cluster-status-joining.txt");

/// 가짜 실행기에 넘기되 `missing`의 프로그램은 없는 것으로 한다.
#[derive(Default)]
struct Runner {
    fake: FakeRunner,
    missing: Mutex<Vec<String>>,
}

#[async_trait]
impl CommandRunner for Runner {
    async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
        if self.missing.lock().unwrap().contains(&cmd.program) {
            return Err(DriverError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("cannot run {}", cmd.program),
            )));
        }
        self.fake.run(cmd).await
    }
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    runner: Arc<Runner>,
    host: OvnHost,
}

impl Env {
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
        let options = OvnOptions {
            wait_step: Duration::from_millis(1),
            wait_limit: Duration::from_millis(200),
            any_os: true,
            ..OvnOptions::default()
        };
        let runner = Arc::new(Runner::default());
        let host = OvnHost::new(runner.clone(), paths, options, TAG.into());
        Self {
            _dir: dir,
            root,
            runner,
            host,
        }
    }

    fn fake(&self) -> &FakeRunner {
        &self.runner.fake
    }

    fn missing(&self, program: &str) {
        self.runner.missing.lock().unwrap().push(program.into());
    }

    fn calls(&self) -> Vec<String> {
        self.fake().calls()
    }

    fn calls_of(&self, prefix: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with(prefix))
            .collect()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn p(&self, rel: &str) -> String {
        path_str(&self.path(rel))
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn installed(&self, packages: &[&str]) {
        let out: String = packages
            .iter()
            .map(|p| format!("{p} install ok installed\n"))
            .collect();
        self.fake().reply("dpkg-query", CmdOutput::ok(out));
        self.fake()
            .reply("ovn-controller --version", CmdOutput::ok(OVN_VERSION));
        self.fake()
            .reply("ovs-vsctl --version", CmdOutput::ok(OVS_VERSION));
    }

    /// 로컬 OVS DB의 `external_ids`
    fn ids(&self, pairs: &[(&str, &str)]) {
        let map: Vec<serde_json::Value> = pairs
            .iter()
            .map(|(k, v)| serde_json::json!([k, v]))
            .collect();
        let json = serde_json::json!({ "data": [[["map", map]]], "headings": ["external_ids"] });
        self.fake().reply(
            "ovs-vsctl --format=json --columns=external_ids",
            CmdOutput::ok(json.to_string()),
        );
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn node() -> Uuid {
    NODE.parse().unwrap()
}

/// 다 깔려 있으면 apt를 부르지 않고 판을 돌려준다. 빠진 것만 깔고, 24.03 미만은 거절한다.
#[tokio::test]
async fn packages() {
    let env = Env::new();
    env.installed(HOST_PACKAGES);
    let v = env
        .host
        .ensure_packages(&strings(HOST_PACKAGES))
        .await
        .unwrap();
    assert_eq!((v.ovn.as_str(), v.ovs.as_str()), ("24.03.8", "3.3.9"));
    assert!(env.calls_of("apt-get").is_empty(), "{:?}", env.calls());

    let env = Env::new();
    env.installed(&["openvswitch-switch"]);
    env.host
        .ensure_packages(&strings(CENTRAL_PACKAGES))
        .await
        .unwrap();
    assert_eq!(
        env.calls_of("apt-get"),
        [
            "apt-get update -q -o DPkg::Lock::Timeout=600",
            "apt-get install -y -q -o DPkg::Lock::Timeout=600 ovn-central ovn-host"
        ]
    );
    let apt = env
        .fake()
        .commands()
        .into_iter()
        .find(|c| c.program == "apt-get")
        .unwrap();
    assert_eq!(apt.timeout, Duration::from_secs(30 * 60));
    assert!(
        apt.env
            .contains(&("DEBIAN_FRONTEND".into(), "noninteractive".into()))
    );

    let env = Env::new();
    env.installed(HOST_PACKAGES);
    env.fake().reply(
        "ovn-controller --version",
        CmdOutput::ok("ovn-controller 23.09.1\n"),
    );
    let err = env
        .host
        .ensure_packages(&strings(HOST_PACKAGES))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, OvnError::Unsupported(m) if m.contains("23.09.1 is too old") && m.contains("24.03")),
        "{err}"
    );

    let env = Env::new();
    env.installed(&[]);
    env.fake().reply(
        "apt-get install",
        CmdOutput::fail(100, "E: Unable to locate package ovn-host"),
    );
    let err = env
        .host
        .ensure_packages(&strings(HOST_PACKAGES))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("Unable to locate package"),
        "{err}"
    );

    // OVS가 없으면 판도 없다.
    let env = Env::new();
    env.missing("ovn-controller");
    assert_eq!(env.host.versions().await.unwrap(), None);
}

/// 꾸러미가 막 깔린 노드(무작위 system-id만)는 받고, 남의 OVN·다른 Orbivisor 클러스터의 chassis는 거절한다.
#[tokio::test]
async fn foreign_chassis_configs_are_refused() {
    let env = Env::new();
    env.fake().reply(
        "ovs-vsctl --format=json --columns=external_ids",
        CmdOutput::ok(FRESH_IDS),
    );
    let warnings = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap();
    assert!(warnings.is_empty());

    // OVS가 없으면 볼 것이 없다.
    let env = Env::new();
    env.missing("ovs-vsctl");
    env.host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap();

    let env = Env::new();
    env.ids(&[("system-id", "x"), ("ovn-remote", "tcp:10.0.0.9:6642")]);
    let err = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, OvnError::Foreign(m) if m.contains("another OVN deployment") && m.contains("tcp:10.0.0.9:6642")),
        "{err}"
    );

    let env = Env::new();
    env.ids(&[
        (TAG_KEY, "999999999999"),
        ("ovn-remote", "ssl:10.0.0.9:6642"),
    ]);
    let err = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("another Orbivisor cluster"),
        "{err}"
    );

    // 이 클러스터의 chassis는 그대로 받는다(다시 하기).
    let env = Env::new();
    env.ids(&[(TAG_KEY, TAG), ("ovn-remote", "ssl:10.0.0.1:6642")]);
    env.host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap();
}

/// 이 노드의 중앙 DB는 Raft가 아는 클러스터 id일 때만 받는다. 합류 중(id를 아직 모름)인 DB는 합류할 때만 받는다.
#[tokio::test]
async fn central_databases_must_be_ours() {
    let known = KnownCids {
        nb: Some(CID_NB.parse().unwrap()),
        sb: Some(CID_SB.parse().unwrap()),
    };
    let env = Env::new();
    env.missing("ovs-vsctl");
    env.write("var/lib/opl/ovn/nb.db", "x");
    env.write("var/lib/opl/ovn/sb.db", "x");
    let nb = env.p("var/lib/opl/ovn/nb.db");
    let sb = env.p("var/lib/opl/ovn/sb.db");
    env.fake().reply(
        &format!("ovsdb-tool db-cid {nb}"),
        CmdOutput::ok(format!("{CID_NB}\n")),
    );
    env.fake().reply(
        &format!("ovsdb-tool db-cid {sb}"),
        CmdOutput::ok(format!("{CID_SB}\n")),
    );
    env.host.check_foreign("n1", known, false).await.unwrap();

    // Raft가 id를 모르면(다른 클러스터가 남긴 DB) 거절한다.
    let err = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, OvnError::Foreign(m) if m.contains("nb.db belongs to another OVN cluster")),
        "{err}"
    );
    // 다른 id
    let other = KnownCids {
        nb: Some(Uuid::nil()),
        ..known
    };
    let err = env
        .host
        .check_foreign("n1", other, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(CID_NB), "{err}");

    // 합류 중인 DB: 실제 `db-cid` 출력(종료 0)
    env.fake().reply(
        &format!("ovsdb-tool db-cid {nb}"),
        CmdOutput::ok(format!("{nb}: cluster ID not yet known\n")),
    );
    env.host.check_foreign("n1", known, true).await.unwrap();
    assert!(env.host.check_foreign("n1", known, false).await.is_err());
}

/// 배포판 NB DB에 논리 스위치가 있거나 클러스터 DB면 거절한다(꾸러미가 막 만든 빈 DB는 괜찮다).
#[tokio::test]
async fn distro_databases_with_switches_are_refused() {
    let env = Env::new();
    env.missing("ovs-vsctl");
    env.write("var/lib/ovn/ovnnb_db.db", "x");
    let file = env.p("var/lib/ovn/ovnnb_db.db");
    env.fake()
        .reply("ovsdb-tool db-is-clustered", CmdOutput::fail(1, ""));
    env.fake()
        .reply("ovsdb-tool query", CmdOutput::ok(r#"[{"rows":[]}]"#));
    env.host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap();
    assert_eq!(
        env.calls_of("ovsdb-tool query"),
        [format!(
            "ovsdb-tool query {file} [\"OVN_Northbound\",{{\"op\":\"select\",\"table\":\"Logical_Switch\",\"where\":[],\"columns\":[\"name\"]}}]"
        )]
    );

    env.fake().reply(
        "ovsdb-tool query",
        CmdOutput::ok(r#"[{"rows":[{"name":"public"},{"name":"private"}]}]"#),
    );
    let err = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("has 2 logical switch(es)"),
        "{err}"
    );

    env.fake()
        .reply("ovsdb-tool db-is-clustered", CmdOutput::ok(""));
    let err = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("clustered OVN database"), "{err}");
}

/// `br-int`에 표시 없는 포트가 있으면 경고만 한다(ovn-controller의 터널은 뺀다).
#[tokio::test]
async fn untagged_br_int_ports_warn() {
    let env = Env::new();
    env.fake().reply(
        "ovs-vsctl --format=json --columns=external_ids",
        CmdOutput::ok(FRESH_IDS),
    );
    env.fake().reply(
        "ovs-vsctl list-ports br-int",
        CmdOutput::ok("optest\noptap0\novn-1a2b3c-0\nvnet7\n"),
    );
    env.fake().reply(
        "ovs-vsctl --format=json --columns=name,ofport,external_ids find Interface",
        CmdOutput::ok(include_str!("../fixtures/find-interfaces.json")),
    );
    let warnings = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap();
    assert_eq!(
        warnings,
        ["br-int already has ports Orbivisor did not add: vnet7"]
    );
    // br-int가 없으면(종료 2) 경고도 없다.
    env.fake().reply(
        "ovs-vsctl list-ports br-int",
        CmdOutput::fail(2, "no bridge named br-int"),
    );
    let warnings = env
        .host
        .check_foreign("n1", KnownCids::default(), false)
        .await
        .unwrap();
    assert!(warnings.is_empty());
}

/// 노드 TLS를 0600으로 베끼고(디렉터리 0700) 같으면 쓰지 않는다. 인증서가 바뀌면 다시 쓴다.
#[tokio::test]
async fn tls_copy() {
    let env = Env::new();
    env.write("pki/ca.crt", "CA");
    env.write("pki/node.crt", "CERT");
    env.write("pki/node.key", "KEY");
    assert!(env.host.write_tls().unwrap());
    assert_eq!(mode(&env.path("etc/opl/ovn")), 0o700);
    for (file, text) in [("ca.crt", "CA"), ("node.crt", "CERT"), ("node.key", "KEY")] {
        let path = env.path(&format!("etc/opl/ovn/{file}"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(mode(&path), 0o600);
    }
    assert!(!env.host.write_tls().unwrap());
    env.write("pki/node.crt", "CERT2");
    assert!(env.host.write_tls().unwrap());
    assert_eq!(
        std::fs::read_to_string(env.path("etc/opl/ovn/node.crt")).unwrap(),
        "CERT2"
    );
}

/// 배포판 중앙 단위 가운데 있는 것만 끄고 가리며 적는다. 이미 가렸으면 다시 하지 않는다.
#[tokio::test]
async fn distro_units_are_disabled_and_masked() {
    let env = Env::new();
    env.fake().reply(
        "systemctl show -p LoadState --value ovn-central.service",
        CmdOutput::ok("loaded\n"),
    );
    env.fake().reply(
        "systemctl show -p LoadState --value ovn-northd.service",
        CmdOutput::ok("loaded\n"),
    );
    env.fake().reply(
        "systemctl show -p LoadState --value ovn-ovsdb-server-nb.service",
        CmdOutput::ok("masked\n"),
    );
    env.fake().reply(
        "systemctl show -p LoadState --value ovn-ovsdb-server-sb.service",
        CmdOutput::ok("not-found\n"),
    );
    let units = env.host.disable_distro_units().await.unwrap();
    assert_eq!(units, ["ovn-central.service", "ovn-northd.service"]);
    assert_eq!(
        env.calls_of("systemctl disable")
            .into_iter()
            .chain(env.calls_of("systemctl mask"))
            .collect::<Vec<_>>(),
        [
            "systemctl disable --now ovn-central.service ovn-northd.service",
            "systemctl mask ovn-central.service ovn-northd.service",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(env.path("etc/opl/ovn/distro-units")).unwrap(),
        "ovn-central.service\novn-northd.service\n"
    );
    // 모두 가렸으면 아무것도 하지 않는다.
    let env = Env::new();
    env.fake()
        .reply("systemctl show -p LoadState", CmdOutput::ok("masked\n"));
    assert!(env.host.disable_distro_units().await.unwrap().is_empty());
    assert!(env.calls_of("systemctl mask").is_empty());
    assert!(!env.path("etc/opl/ovn/distro-units").exists());
}

/// 새 클러스터 DB는 임시 파일에 만들어 id를 돌려주고, 제자리에 옮긴 뒤에는 다시 만들지 않는다.
#[tokio::test]
async fn cluster_databases() {
    let env = Env::new();
    let tmp = env.p("var/lib/opl/ovn/.nb.db.opl-new");
    env.fake().reply(
        &format!("ovsdb-tool db-cid {tmp}"),
        CmdOutput::ok(format!("{CID_NB}\n")),
    );
    let cid = env
        .host
        .create_cluster(Db::Nb, ip("10.0.0.1"))
        .await
        .unwrap();
    assert_eq!(cid, Some(CID_NB.parse().unwrap()));
    assert_eq!(
        env.calls_of("ovsdb-tool create-cluster"),
        [format!(
            "ovsdb-tool create-cluster {tmp} {} ssl:10.0.0.1:6643",
            env.p("usr/share/ovn/ovn-nb.ovsschema")
        )]
    );
    // 가짜 실행기는 파일을 만들지 않으므로 손수 둔다.
    std::fs::write(&tmp, "db").unwrap();
    assert!(!env.path("var/lib/opl/ovn/nb.db").exists());
    env.host.commit_db(Db::Nb).unwrap();
    assert!(env.path("var/lib/opl/ovn/nb.db").exists());
    assert!(!env.path("var/lib/opl/ovn/.nb.db.opl-new").exists());
    // 다시 하면 이미 있다.
    assert_eq!(
        env.host
            .create_cluster(Db::Nb, ip("10.0.0.1"))
            .await
            .unwrap(),
        None
    );
    env.host.commit_db(Db::Nb).unwrap();
    assert_eq!(env.calls_of("ovsdb-tool create-cluster").len(), 1);
    // id를 읽지 못하면 실패한다.
    env.fake().reply(
        "ovsdb-tool db-cid",
        CmdOutput::fail(1, "ovsdb-tool: bad file"),
    );
    let err = env
        .host
        .create_cluster(Db::Sb, ip("10.0.0.1"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("bad file"), "{err}");
}

/// 합류: 임시 파일을 지우고 다시 만든 뒤 제자리에 옮긴다(가짜 실행기 대신 파일을 만드는 실행기).
#[tokio::test]
async fn join_cluster_writes_into_place() {
    struct Join(FakeRunner);
    #[async_trait]
    impl CommandRunner for Join {
        async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
            if cmd.program == "ovsdb-tool" && cmd.args[0] == "join-cluster" {
                std::fs::write(&cmd.args[1], "joining").unwrap();
            }
            self.0.run(cmd).await
        }
    }
    let env = Env::new();
    let runner = Arc::new(Join(FakeRunner::new()));
    let host = OvnHost::new(
        runner.clone(),
        env.host.paths().clone(),
        env.host.options().clone(),
        TAG.into(),
    );
    assert!(
        host.join_cluster(Db::Sb, ip("10.0.0.2"), &[ip("10.0.0.1"), ip("fd00::3")])
            .await
            .unwrap()
    );
    let tmp = env.p("var/lib/opl/ovn/.sb.db.opl-new");
    assert_eq!(
        runner.0.calls(),
        [format!(
            "ovsdb-tool join-cluster {tmp} OVN_Southbound ssl:10.0.0.2:6644 ssl:10.0.0.1:6644 ssl:[fd00::3]:6644"
        )]
    );
    assert_eq!(
        std::fs::read_to_string(env.path("var/lib/opl/ovn/sb.db")).unwrap(),
        "joining"
    );
    assert!(
        !host
            .join_cluster(Db::Sb, ip("10.0.0.2"), &[ip("10.0.0.1")])
            .await
            .unwrap()
    );
    let err = host
        .join_cluster(Db::Nb, ip("10.0.0.2"), &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no existing OVN central"), "{err}");
}

/// 단위 파일을 쓰고 바뀌었을 때만 daemon-reload와 다시 시작한다. 표시 없는 같은 이름의 파일은 덮지 않는다.
#[tokio::test]
async fn units_are_written_and_started() {
    let env = Env::new();
    let changed = env
        .host
        .write_units(ip("10.0.0.1"), &[ip("10.0.0.1")])
        .await
        .unwrap();
    assert_eq!(
        changed,
        [
            "opl-ovn-nb.service",
            "opl-ovn-sb.service",
            "opl-ovn-northd.service"
        ]
    );
    let nb = std::fs::read_to_string(env.path("etc/systemd/system/opl-ovn-nb.service")).unwrap();
    assert_eq!(nb, db_unit(Db::Nb, ip("10.0.0.1"), env.host.paths()));
    assert_eq!(env.calls_of("systemctl daemon-reload").len(), 1);
    env.host.start_central(&changed).await.unwrap();
    assert_eq!(
        env.calls_of("systemctl")
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>(),
        [
            "systemctl enable opl-ovn-nb.service opl-ovn-sb.service opl-ovn-northd.service",
            "systemctl restart opl-ovn-nb.service",
            "systemctl restart opl-ovn-sb.service",
            "systemctl restart opl-ovn-northd.service",
        ]
    );

    // 같으면 아무것도 바꾸지 않는다. 중앙이 늘면 northd만 바뀐다.
    let changed = env
        .host
        .write_units(ip("10.0.0.1"), &[ip("10.0.0.1")])
        .await
        .unwrap();
    assert!(changed.is_empty());
    assert_eq!(env.calls_of("systemctl daemon-reload").len(), 1);
    let changed = env
        .host
        .write_units(ip("10.0.0.1"), &[ip("10.0.0.1"), ip("10.0.0.2")])
        .await
        .unwrap();
    assert_eq!(changed, ["opl-ovn-northd.service"]);

    env.write("etc/systemd/system/opl-ovn-sb.service", "[Unit]\n");
    let err = env
        .host
        .write_units(ip("10.0.0.1"), &[ip("10.0.0.1")])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, OvnError::Foreign(m) if m.contains("opl-ovn-sb.service was not written by Orbivisor")),
        "{err}"
    );

    // 에이전트: 멈춘 단위만 켠다.
    let env = Env::new();
    env.fake().reply(
        "systemctl is-active opl-ovn-northd.service",
        CmdOutput::fail(3, ""),
    );
    let started = env.host.ensure_central_running().await.unwrap();
    assert_eq!(started, ["opl-ovn-northd.service"]);
    assert_eq!(
        env.calls_of("systemctl start"),
        ["systemctl start opl-ovn-northd.service"]
    );
    env.host.stop_central().await.unwrap();
    assert_eq!(
        env.calls_of("systemctl disable"),
        ["systemctl disable --now opl-ovn-nb.service opl-ovn-sb.service opl-ovn-northd.service"]
    );
}

/// 리더·멤버 기다리기와 그 시간 초과
#[tokio::test]
async fn waits_for_the_raft_cluster() {
    let env = Env::new();
    let ctl = env.p("run/opl/ovn/nb.ctl");
    env.fake().reply_seq(
        &format!("ovs-appctl --timeout 5 -t {ctl} cluster/status OVN_Northbound"),
        vec![
            CmdOutput::fail(1, "cannot connect"),
            CmdOutput::ok(NB_LEADER),
        ],
    );
    let s = env.host.wait_leader(Db::Nb).await.unwrap();
    assert_eq!(s.cid, Some(CID_NB.parse().unwrap()));

    let env = Env::new();
    let ctl = env.p("run/opl/ovn/nb.ctl");
    env.fake().reply_seq(
        &format!("ovs-appctl --timeout 5 -t {ctl} cluster/status"),
        vec![CmdOutput::ok(JOINING), CmdOutput::ok(FOLLOWER)],
    );
    let s = env
        .host
        .wait_member(Db::Nb, Some(CID_NB.parse().unwrap()))
        .await
        .unwrap();
    assert!(s.is_member());
    // 다른 클러스터에 붙었으면 끝나지 않는다.
    let err = env
        .host
        .wait_member(Db::Nb, Some(Uuid::nil()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, OvnError::Timeout(w) if w.contains("northbound database to join")),
        "{err}"
    );
    let env = Env::new();
    env.fake().reply(
        "ovs-appctl",
        CmdOutput::ok(SB_LEADER.replace("Role: leader", "Role: follower")),
    );
    let err = env.host.wait_leader(Db::Sb).await.unwrap_err();
    assert!(
        matches!(err, OvnError::Timeout(w) if w.contains("southbound")),
        "{err}"
    );
}

/// 떠나기: 멤버면 `cluster/leave` 뒤 `unknown cluster`가 될 때까지 기다린다. 이미 떠났으면 그대로, 서버가 없으면 실패
#[tokio::test]
async fn leaving_the_cluster() {
    let env = Env::new();
    let ctl = env.p("run/opl/ovn/sb.ctl");
    let status = format!("ovs-appctl --timeout 5 -t {ctl} cluster/status OVN_Southbound");
    env.fake().reply_seq(
        &status,
        vec![
            CmdOutput::ok(SB_LEADER),
            CmdOutput::ok(SB_LEADER),
            CmdOutput {
                status: 2,
                stdout: b"unknown cluster\n".to_vec(),
                stderr: b"ovs-appctl: server returned an error\n".to_vec(),
            },
        ],
    );
    env.host.leave_cluster(Db::Sb).await.unwrap();
    assert_eq!(
        env.calls_of(&format!("ovs-appctl --timeout 5 -t {ctl} cluster/leave")),
        [format!(
            "ovs-appctl --timeout 5 -t {ctl} cluster/leave OVN_Southbound"
        )]
    );
    // 이미 떠났다.
    env.host.leave_cluster(Db::Sb).await.unwrap();
    assert_eq!(
        env.calls_of(&format!("ovs-appctl --timeout 5 -t {ctl} cluster/leave"))
            .len(),
        1
    );

    let env = Env::new();
    env.fake().reply(
        "ovs-appctl",
        CmdOutput::fail(
            1,
            "ovs-appctl: cannot connect to \"/run/opl/ovn/sb.ctl\" (No such file or directory)",
        ),
    );
    let err = env.host.leave_cluster(Db::Sb).await.unwrap_err();
    assert!(err.to_string().contains("server is not running"), "{err}");

    // 떠난 DB는 이름만 바꾼다.
    let env = Env::new();
    env.write("var/lib/opl/ovn/nb.db", "nb");
    env.write("var/lib/opl/ovn/sb.db", "sb");
    let moved = env.host.retire_dbs(1_700_000_000).unwrap();
    assert_eq!(
        moved,
        [
            env.path("var/lib/opl/ovn/nb.db.left-1700000000"),
            env.path("var/lib/opl/ovn/sb.db.left-1700000000")
        ]
    );
    assert!(!env.path("var/lib/opl/ovn/nb.db").exists());
    assert_eq!(std::fs::read_to_string(&moved[1]).unwrap(), "sb");
}

/// 리뷰 2 N-3: Raft 구성에 들지 않은 서버는 떠날 것이 없다 — 돌지 않고 DB의 클러스터 id를 아직 모르면(합류를 시작만
/// 함), 또는 아직 합류 중이면(OVS가 떠나기를 거절한다) `cluster/leave` 없이 끝난다. 클러스터 id를 아는 DB의 서버가 돌지
/// 않으면 그대로 실패다.
#[tokio::test]
async fn servers_outside_the_raft_configuration_have_nothing_to_leave() {
    let env = Env::new();
    let nb = env.p("var/lib/opl/ovn/nb.db");
    env.write("var/lib/opl/ovn/nb.db", "nb");
    env.fake().reply(
        "ovs-appctl",
        CmdOutput::fail(1, "ovs-appctl: cannot connect to \"/run/opl/ovn/nb.ctl\""),
    );
    env.fake().reply(
        &format!("ovsdb-tool db-cid {nb}"),
        CmdOutput::ok(format!("{nb}: cluster ID not yet known\n")),
    );
    env.host.leave_cluster(Db::Nb).await.unwrap();
    env.fake().reply(
        &format!("ovsdb-tool db-cid {nb}"),
        CmdOutput::ok(format!("{CID_NB}\n")),
    );
    let err = env.host.leave_cluster(Db::Nb).await.unwrap_err();
    assert!(err.to_string().contains("server is not running"), "{err}");

    let env = Env::new();
    env.fake().reply("ovs-appctl", CmdOutput::ok(JOINING));
    env.host.leave_cluster(Db::Nb).await.unwrap();
    assert!(
        env.calls_of("ovs-appctl")
            .iter()
            .all(|c| !c.contains("cluster/leave"))
    );
}

/// 리뷰 2 N-1·N-2: 살아 있는 중앙이 다른 서버를 Raft 주소로 내쫓고 목록에서 빠질 때까지 기다린다. 목록에 없으면 하지
/// 않고, 자기 주소는 거절한다.
#[tokio::test]
async fn kicking_a_dead_server_out() {
    let env = Env::new();
    let ctl = env.p("run/opl/ovn/nb.ctl");
    let status = format!("ovs-appctl --timeout 5 -t {ctl} cluster/status OVN_Northbound");
    let after = NB_LEADER
        .lines()
        .filter(|l| !l.contains("ssl:127.0.0.2:6643"))
        .collect::<Vec<_>>()
        .join("\n");
    let two = include_str!("../fixtures/cluster-status-leader-two.txt");
    env.fake().reply_seq(
        &status,
        vec![CmdOutput::ok(two), CmdOutput::ok(two), CmdOutput::ok(after)],
    );
    assert!(env.host.kick(Db::Nb, ip("127.0.0.2")).await.unwrap());
    assert_eq!(
        env.calls_of(&format!("ovs-appctl --timeout 5 -t {ctl} cluster/kick")),
        [format!(
            "ovs-appctl --timeout 5 -t {ctl} cluster/kick OVN_Northbound ssl:127.0.0.2:6643"
        )]
    );
    // 이미 빠졌다.
    assert!(!env.host.kick(Db::Nb, ip("127.0.0.2")).await.unwrap());
    // 자기 자신은 내쫓지 않는다.
    let err = env.host.kick(Db::Nb, ip("127.0.0.1")).await.unwrap_err();
    assert!(err.to_string().contains("this server"), "{err}");
    // 빠지지 않으면 시간 초과
    let env = Env::new();
    env.fake().reply("ovs-appctl", CmdOutput::ok(two));
    let err = env.host.kick(Db::Nb, ip("127.0.0.2")).await.unwrap_err();
    assert!(matches!(err, OvnError::Timeout(_)), "{err}");
}

/// chassis 구성: 다른 칸만 `ovs-vsctl set`(쉼표가 든 값은 따옴표), SSL, system-id.conf, 단위. 같으면 단위만 켠다.
#[tokio::test]
async fn chassis_configuration() {
    let env = Env::new();
    env.fake().reply(
        "ovs-vsctl --format=json --columns=external_ids",
        CmdOutput::ok(FRESH_IDS),
    );
    env.write(
        "etc/openvswitch/system-id.conf",
        "b72814a1-deac-4c3b-9c43-5719f7683fb6\n",
    );
    let centrals = [ip("10.0.0.1"), ip("10.0.0.2")];
    let chassis = Chassis {
        node: node(),
        centrals: &centrals,
        encap_ip: ip("10.1.0.5"),
    };
    assert!(env.host.configure_chassis(&chassis).await.unwrap());
    let key = env.p("etc/opl/ovn/node.key");
    let cert = env.p("etc/opl/ovn/node.crt");
    let ca = env.p("etc/opl/ovn/ca.crt");
    assert_eq!(
        env.calls()
            .into_iter()
            .filter(|c| !c.starts_with("ovs-vsctl --format=json"))
            .collect::<Vec<_>>(),
        [
            "ovs-vsctl get-ssl".to_string(),
            format!("ovs-vsctl set-ssl {key} {cert} {ca}"),
            format!(
                "ovs-vsctl set Open_vSwitch . external_ids:orbivisor-cluster=\"{TAG}\" \
                 external_ids:ovn-encap-ip=\"10.1.0.5\" external_ids:ovn-encap-type=\"geneve\" \
                 external_ids:ovn-remote=\"ssl:10.0.0.1:6642,ssl:10.0.0.2:6642\" \
                 external_ids:system-id=\"{NODE}\""
            ),
            "systemctl enable --now ovn-host.service".to_string(),
            "systemctl start ovn-controller.service".to_string(),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(env.path("etc/openvswitch/system-id.conf")).unwrap(),
        format!("{NODE}\n")
    );

    // 다 맞으면 쓰지 않는다.
    let env2 = Env::new();
    let want = chassis_ids(&chassis, TAG);
    let pairs: Vec<(&str, &str)> = want.iter().map(|(k, v)| (*k, v.as_str())).collect();
    env2.ids(&pairs);
    env2.fake().reply(
        "ovs-vsctl get-ssl",
        CmdOutput::ok(format!(
            "Private key: {}\nCertificate: {}\nCA Certificate: {}\nBootstrap: false\n",
            env2.p("etc/opl/ovn/node.key"),
            env2.p("etc/opl/ovn/node.crt"),
            env2.p("etc/opl/ovn/ca.crt")
        )),
    );
    env2.write("etc/openvswitch/system-id.conf", &format!("{NODE}\n"));
    assert!(!env2.host.configure_chassis(&chassis).await.unwrap());
    assert!(
        env2.calls_of("ovs-vsctl set").is_empty(),
        "{:?}",
        env2.calls()
    );
    // 에이전트의 맞추기: 중앙이 바뀌면 ovn-remote만 바꾸고 단위는 건드리지 않는다.
    let one = [ip("10.0.0.1")];
    let fewer = Chassis {
        centrals: &one,
        ..chassis.clone()
    };
    assert!(env2.host.reconcile_chassis(&fewer).await.unwrap());
    assert_eq!(
        env2.calls_of("ovs-vsctl set "),
        ["ovs-vsctl set Open_vSwitch . external_ids:ovn-remote=\"ssl:10.0.0.1:6642\""]
    );
    assert_eq!(env2.calls_of("systemctl").len(), 2);
    // 이 클러스터의 chassis가 아니면 맞추지 않는다.
    let env3 = Env::new();
    env3.fake().reply(
        "ovs-vsctl --format=json --columns=external_ids",
        CmdOutput::ok(FRESH_IDS),
    );
    assert!(!env3.host.reconcile_chassis(&chassis).await.unwrap());
    assert!(env3.calls_of("ovs-vsctl set").is_empty());
    // 중앙이 없으면 구성할 수 없다.
    let none = Chassis {
        centrals: &[],
        ..chassis
    };
    assert!(env3.host.configure_chassis(&none).await.is_err());
}

/// chassis 상태와 빠질 때 지우기
#[tokio::test]
async fn chassis_probe_and_unconfigure() {
    let env = Env::new();
    env.fake().reply(
        "ovs-vsctl --format=json --columns=external_ids",
        CmdOutput::ok(FRESH_IDS),
    );
    assert_eq!(
        env.host.chassis_probe(node(), &[ip("10.0.0.1")]).await,
        ChassisProbe::NotConfigured
    );
    assert!(!env.host.unconfigure_chassis().await.unwrap());

    env.ids(&[(TAG_KEY, TAG), ("ovn-encap-ip", "10.1.0.5")]);
    env.fake().reply(
        "ovn-appctl --timeout 5 -t ovn-controller connection-status",
        CmdOutput::ok("connected\n"),
    );
    env.fake().reply(
        "ovn-sbctl",
        CmdOutput::ok(format!(r#"{{"data":[["{NODE}"]],"headings":["name"]}}"#)),
    );
    assert_eq!(
        env.host.chassis_probe(node(), &[ip("10.0.0.1")]).await,
        ChassisProbe::Configured {
            connected: true,
            registered: Some(true),
            encap_ip: Some(ip("10.1.0.5")),
        }
    );
    let key = env.p("etc/opl/ovn/node.key");
    assert_eq!(
        env.calls_of("ovn-sbctl"),
        [format!(
            "ovn-sbctl --db=ssl:10.0.0.1:6642 --private-key={key} --certificate={} --ca-cert={} \
             --timeout=5 --no-leader-only --format=json --columns=name find Chassis name={NODE}",
            env.p("etc/opl/ovn/node.crt"),
            env.p("etc/opl/ovn/ca.crt")
        )]
    );
    env.fake().reply(
        "ovn-appctl --timeout 5 -t ovn-controller connection-status",
        CmdOutput::ok("not connected\n"),
    );
    env.fake().reply("ovn-sbctl", CmdOutput::fail(1, "timeout"));
    assert_eq!(
        env.host.chassis_probe(node(), &[ip("10.0.0.1")]).await,
        ChassisProbe::Configured {
            connected: false,
            registered: None,
            encap_ip: Some(ip("10.1.0.5")),
        }
    );
    assert!(env.host.unconfigure_chassis().await.unwrap());
    assert_eq!(
        env.calls_of("ovs-vsctl remove"),
        ["ovs-vsctl remove Open_vSwitch . external_ids orbivisor-cluster ovn-remote"]
    );
    assert_eq!(
        env.calls_of("systemctl"),
        ["systemctl stop ovn-controller.service"]
    );
}

/// 중앙 건강: Raft 역할과 northd. 읽지 못하면 `Down`·거짓
#[tokio::test]
async fn central_health() {
    let env = Env::new();
    let nb = env.p("run/opl/ovn/nb.ctl");
    env.fake().reply(
        &format!("ovs-appctl --timeout 5 -t {nb} cluster/status"),
        CmdOutput::ok(NB_LEADER),
    );
    env.fake().reply(
        &format!("ovs-appctl --timeout 5 -t {}", env.p("run/opl/ovn/sb.ctl")),
        CmdOutput::fail(1, "cannot connect"),
    );
    env.fake().reply(
        &format!(
            "ovn-appctl --timeout 5 -t {} status",
            env.p("run/opl/ovn/northd.ctl")
        ),
        CmdOutput::ok("Status: active\n"),
    );
    assert_eq!(
        env.host.central_health().await,
        CentralHealth {
            nb: RaftRole::Leader,
            sb: RaftRole::Down,
            northd_active: true,
        }
    );
    env.fake().reply(
        &format!(
            "ovn-appctl --timeout 5 -t {} status",
            env.p("run/opl/ovn/northd.ctl")
        ),
        CmdOutput::ok("Status: standby\n"),
    );
    assert!(!env.host.central_health().await.northd_active);
}

/// 스키마 변환은 리더에서, 필요할 때만 한다.
#[tokio::test]
async fn schema_conversion() {
    let env = Env::new();
    let sock = env.p("run/opl/ovn/nb.sock");
    let schema = env.p("usr/share/ovn/ovn-nb.ovsschema");
    env.fake().reply("ovs-appctl", CmdOutput::ok(NB_LEADER));
    env.fake()
        .reply("ovsdb-client needs-conversion", CmdOutput::ok("no\n"));
    assert!(!env.host.convert_if_needed(Db::Nb).await.unwrap());
    env.fake()
        .reply("ovsdb-client needs-conversion", CmdOutput::ok("yes\n"));
    assert!(env.host.convert_if_needed(Db::Nb).await.unwrap());
    assert_eq!(
        env.calls_of("ovsdb-client"),
        [
            format!("ovsdb-client needs-conversion unix:{sock} {schema}"),
            format!("ovsdb-client needs-conversion unix:{sock} {schema}"),
            format!("ovsdb-client convert unix:{sock} {schema}"),
        ]
    );
    let env = Env::new();
    env.fake().reply("ovs-appctl", CmdOutput::ok(FOLLOWER));
    assert!(!env.host.convert_if_needed(Db::Nb).await.unwrap());
    assert!(env.calls_of("ovsdb-client").is_empty());
}

/// 오래된 포트: 이 클러스터 표시가 있고 장치가 없으며 도는 VM의 tap이 아닌 것만 지운다.
#[tokio::test]
async fn stale_ports_are_removed() {
    let env = Env::new();
    env.fake().reply(
        "ovs-vsctl --format=json --columns=name,ofport,external_ids find Interface",
        CmdOutput::ok(include_str!("../fixtures/find-interfaces.json")),
    );
    let removed = env.host.remove_stale_ports(&BTreeSet::new()).await.unwrap();
    assert_eq!(removed, ["optap0"]);
    assert_eq!(
        env.calls_of("ovs-vsctl --if-exists"),
        ["ovs-vsctl --if-exists del-port br-int optap0"]
    );
    assert_eq!(
        env.calls_of("ovs-vsctl --format=json --columns=name"),
        [format!(
            "ovs-vsctl --format=json --columns=name,ofport,external_ids find Interface external_ids:orbivisor-cluster={TAG}"
        )]
    );
    let keep = BTreeSet::from(["optap0".to_string()]);
    let list = parse_interfaces(include_bytes!("../fixtures/find-interfaces.json")).unwrap();
    assert!(stale_ports(&list, &keep).is_empty());
}

#[test]
fn quoting() {
    assert_eq!(
        quote("ssl:10.0.0.1:6642,ssl:[fd00::1]:6642"),
        "\"ssl:10.0.0.1:6642,ssl:[fd00::1]:6642\""
    );
    assert_eq!(
        central_units(),
        [
            "opl-ovn-nb.service",
            "opl-ovn-sb.service",
            "opl-ovn-northd.service"
        ]
    );
}

#[test]
fn linux_only_unless_tests_allow_any_os() {
    let env = Env::new();
    assert!(env.host.check_platform().is_ok());
    let host = OvnHost::new(
        env.runner.clone(),
        env.host.paths().clone(),
        OvnOptions::default(),
        TAG.into(),
    );
    assert_eq!(host.check_platform().is_ok(), cfg!(target_os = "linux"));
}

/// 제자리 DB의 Raft 주소(`db-local-address`)와 이 SDN의 DB가 남았는지(리뷰 1 I-7)
#[tokio::test]
async fn database_address_and_membership() {
    let env = Env::new();
    // 파일이 없으면 모른다(명령도 부르지 않는다).
    assert_eq!(env.host.db_local_address(Db::Nb).await.unwrap(), None);
    assert!(env.calls().is_empty());
    let known = KnownCids {
        nb: Some(CID_NB.parse().unwrap()),
        sb: Some(CID_SB.parse().unwrap()),
    };
    assert!(!env.host.has_cluster_db(known).await);

    env.write("var/lib/opl/ovn/nb.db", "nb");
    let nb = env.p("var/lib/opl/ovn/nb.db");
    env.fake().reply(
        &format!("ovsdb-tool db-local-address {nb}"),
        CmdOutput::ok("ssl:10.0.0.1:6643\n"),
    );
    assert_eq!(
        env.host.db_local_address(Db::Nb).await.unwrap().as_deref(),
        Some("ssl:10.0.0.1:6643")
    );
    assert_eq!(
        env.host
            .db_address_differs(Db::Nb, ip("10.0.0.1"))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        env.host
            .db_address_differs(Db::Nb, ip("10.0.0.2"))
            .await
            .unwrap()
            .as_deref(),
        Some("ssl:10.0.0.1:6643")
    );
    // 주소 꼴이 아닌 답은 모르는 것이다.
    env.fake().reply(
        &format!("ovsdb-tool db-local-address {nb}"),
        CmdOutput::ok("something else\n"),
    );
    assert_eq!(
        env.host
            .db_address_differs(Db::Nb, ip("10.0.0.2"))
            .await
            .unwrap(),
        None
    );

    // 이 SDN의 id면(또는 합류 중이라 아직 모르면) 남은 멤버다. 다른 클러스터의 DB나 Raft가 id를 모르면 아니다.
    env.fake().reply(
        &format!("ovsdb-tool db-cid {nb}"),
        CmdOutput::ok(format!("{CID_NB}\n")),
    );
    assert!(env.host.has_cluster_db(known).await);
    assert!(!env.host.has_cluster_db(KnownCids::default()).await);
    env.fake().reply(
        &format!("ovsdb-tool db-cid {nb}"),
        CmdOutput::ok(format!("{CID_SB}\n")),
    );
    assert!(!env.host.has_cluster_db(known).await);
    env.fake().reply(
        &format!("ovsdb-tool db-cid {nb}"),
        CmdOutput::fail(1, "ovsdb-tool: database: cluster ID not yet known"),
    );
    assert!(env.host.has_cluster_db(known).await);

    // 하나만 이름을 바꾼다.
    let moved = env.host.retire_db(Db::Nb, 1_700_000_000).unwrap();
    assert_eq!(
        moved,
        Some(env.path("var/lib/opl/ovn/nb.db.left-1700000000"))
    );
    assert_eq!(env.host.retire_db(Db::Nb, 1).unwrap(), None);
}
