//! 방화벽 에이전트. 클러스터의 모든 VM에서 보안 그룹을 건 NIC를 모아 이 노드의 nftables 테이블
//! `bridge opl_<태그>` 하나로 넣는다(VM이 어느 노드에 있든 — 마이그레이션·복구로 언제 올지 모른다).
//! 워커가 VM을 켜기 직전에 부르는 확인([`FirewallGate`])도 같은 일을 한다.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_driver::{Firewall, FirewallNic, ruleset};
use opl_reconciler::FirewallGate;
use opl_types::network::tap_name;
use opl_types::resource::Kind;
use opl_types::security_group::{Action, Direction, Rule, SecurityGroup, SecurityGroupSpec};
use opl_types::vm::{Vm, VmSpec};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::host::Host;

/// 바뀐 것이 없어도 규칙을 다시 넣는 간격(누가 테이블을 지웠거나 바꿨을 때)
const FIREWALL_REFRESH: Duration = Duration::from_secs(60);
/// VM·SecurityGroup이 바뀌면 이만큼 모아 한 번에 넣는다.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// 클러스터의 방화벽 NIC(보안 그룹을 건 NIC). tap은 그 NIC의 tap 이름, 규칙은 NIC의 그룹 순서·그룹
/// 안 순서로 펼친 것이다. MAC이 없는 NIC는 건너뛰고(아직 켤 수 없다), 없는 그룹은 규칙이 없는 것으로
/// 보되 그 NIC의 규칙 끝에 나가기 차단을 더한다(들어오기·나가기 모두 앞 규칙에 맞는 것만 통한다).
pub fn firewall_nics(vms: &[Vm], groups: &[SecurityGroup]) -> Vec<FirewallNic> {
    let mut nics = Vec::new();
    for vm in vms {
        for (i, nic) in vm.spec.nics.iter().enumerate() {
            if nic.security_groups.is_empty() {
                continue;
            }
            let Some(mac) = nic.mac else {
                continue;
            };
            let found: Vec<_> = nic
                .security_groups
                .iter()
                .filter_map(|name| groups.iter().find(|g| &g.name == name))
                .collect();
            let mut rules: Vec<Rule> = found
                .iter()
                .flat_map(|g| g.spec.rules.iter().cloned())
                .collect();
            if found.len() < nic.security_groups.len() {
                rules.push(Rule {
                    direction: Direction::Out,
                    action: Action::Drop,
                    protocol: None,
                    ports: None,
                    remote: None,
                });
            }
            nics.push(FirewallNic {
                tap: tap_name(vm.id, i),
                mac,
                rules,
            });
        }
    }
    nics
}

/// 지금 클러스터 상태로 규칙 글을 만들어 넣는다. 에이전트 루프와 워커의 시작 직전 확인이 함께 쓰며
/// 한 번에 하나씩만 넣는다.
pub struct FirewallRules {
    cluster: Arc<Cluster>,
    firewall: Arc<dyn Firewall>,
    tag: String,
    /// 마지막으로 넣은 글. 이 잠금이 넣기를 직렬화한다.
    last: Mutex<Option<String>>,
}

impl FirewallRules {
    /// 위트니스가 아니고 이 노드에 방화벽(nftables)이 있을 때만 있다.
    pub fn new(
        cluster: Arc<Cluster>,
        config: &Config,
        host: &Host,
        tag: String,
    ) -> Option<Arc<Self>> {
        if config.witness {
            return None;
        }
        let firewall = host.firewall.clone()?;
        Some(Arc::new(Self {
            cluster,
            firewall,
            tag,
            last: Mutex::new(None),
        }))
    }

    /// 규칙 글을 만들어 넣는다. `force`가 아니면 마지막으로 넣은 글과 같을 때 건너뛴다. 넣었으면 true.
    async fn apply(&self, force: bool) -> anyhow::Result<bool> {
        let mut last = self.last.lock().await;
        const READ: &str = "cannot read cluster state for the firewall";
        let (_, vms) = self.cluster.list::<VmSpec>().context(READ)?;
        let (_, groups) = self.cluster.list::<SecurityGroupSpec>().context(READ)?;
        let text = ruleset(&self.tag, &firewall_nics(&vms, &groups));
        if !force && last.as_deref() == Some(text.as_str()) {
            return Ok(false);
        }
        // 실패하면 다음 차례에 같은 글이어도 다시 넣는다.
        *last = None;
        self.firewall
            .apply(&text)
            .await
            .context("cannot apply the firewall rules")?;
        *last = Some(text);
        Ok(true)
    }

    /// 이 클러스터의 테이블을 지운다(클러스터에서 빠질 때).
    async fn clear(&self) -> anyhow::Result<()> {
        let mut last = self.last.lock().await;
        *last = None;
        self.firewall.apply(&ruleset(&self.tag, &[])).await?;
        Ok(())
    }
}

#[async_trait]
impl FirewallGate for FirewallRules {
    /// 바뀐 것이 없어 보여도 다시 넣는다(누가 테이블을 지웠어도 켜기 전에 되살린다).
    async fn sync(&self) -> Result<(), String> {
        self.apply(true)
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    }
}

/// 방화벽 에이전트 작업
pub struct FirewallAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    rules: Arc<FirewallRules>,
}

impl FirewallAgent {
    pub fn spawn(rules: Arc<FirewallRules>) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_firewall(rules.clone(), FIREWALL_REFRESH, rx)),
            rules,
        }
    }

    /// 멈춘다. 규칙은 그대로 둔다(VM이 계속 돈다). 다만 클러스터에서 빠졌으면(`left`, 이 노드에 배정된
    /// VM이 없다) 테이블을 지운다. 지우지 못하면 경고만 남긴다.
    pub async fn shutdown(self, left: bool) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
        if !left {
            return;
        }
        if let Err(e) = self.rules.clear().await {
            tracing::warn!(error = %e, "cannot remove the firewall rules after leaving the cluster");
        }
    }
}

async fn run_firewall(
    rules: Arc<FirewallRules>,
    refresh: Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(refresh);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watch = None;
    loop {
        // 주기 차례(시작 포함)에는 바뀌지 않았어도 다시 넣는다.
        let force = tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => true,
            () = firewall_changed(&mut watch) => {
                // 잇따른 변경을 모은다. 구독이 끊긴 때도 같이 기다려 다시 구독하기를 몰아치지 않는다.
                tokio::select! {
                    _ = &mut shutdown => return,
                    () = tokio::time::sleep(DEBOUNCE) => {}
                }
                watch = None;
                false
            }
        };
        // 이번에 읽을 목록 뒤의 변경을 놓치지 않게 먼저 구독한다.
        if watch.is_none() {
            watch = rules
                .cluster
                .list::<VmSpec>()
                .ok()
                .and_then(|(revision, _)| rules.cluster.watch(revision).ok());
        }
        // 하던 적용은 끊지 않는다. 종료는 차례 사이에서만 본다.
        if let Err(e) = rules.apply(force).await {
            tracing::warn!(error = %e.root_cause(), "{e}");
        }
    }
}

/// VM이나 SecurityGroup 리소스가 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을
/// 버리고 돌아가 넣은 뒤 다시 구독하게 한다. 구독이 없으면 기다리기만 한다.
async fn firewall_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return std::future::pending().await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Vm | Kind::SecurityGroup,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Vm | Kind::SecurityGroup,
                    ..
                },
            ) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;
    use opl_cluster::types::NodeId;
    use opl_driver::DriverError;
    use opl_types::resource::Resource;
    use opl_types::vm::{CpuSpec, MacAddr, MachineType, NicModel, NicSpec, PowerState};
    use uuid::Uuid;

    use crate::bootstrap::tests::single_node;

    /// 받은 글을 기록한다. `fail`이면 실패한다.
    #[derive(Debug, Default)]
    struct RecordingFirewall {
        applied: StdMutex<Vec<String>>,
        fail: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl Firewall for RecordingFirewall {
        async fn apply(&self, ruleset: &str) -> Result<(), DriverError> {
            self.applied.lock().unwrap().push(ruleset.to_string());
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(DriverError::ToolFailed {
                    tool: "nft",
                    stderr: "boom".into(),
                });
            }
            Ok(())
        }
    }

    fn rule(json: serde_json::Value) -> Rule {
        serde_json::from_value(json).unwrap()
    }

    fn group(name: &str, rules: Vec<Rule>) -> SecurityGroup {
        Resource::new(
            name,
            SecurityGroupSpec {
                description: None,
                rules,
            },
        )
    }

    fn mac(last: u8) -> MacAddr {
        MacAddr([0x52, 0x54, 0, 0, 0, last])
    }

    fn nic(mac: Option<MacAddr>, groups: &[&str]) -> NicSpec {
        NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac,
            security_groups: groups.iter().map(|g| g.to_string()).collect(),
        }
    }

    fn vm(name: &str, node: Option<Uuid>, nics: Vec<NicSpec>) -> Vm {
        Resource::new(
            name,
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 1,
                    model: Default::default(),
                },
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics,
                power: PowerState::Stopped,
                restart: Default::default(),
                node,
                snapshot_schedule: None,
                ha: false,
            },
        )
    }

    #[test]
    fn firewall_nics_expand_the_groups_of_each_nic() {
        let ssh = rule(
            serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "22" }),
        );
        let web = rule(
            serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "80" }),
        );
        let smtp = rule(
            serde_json::json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }),
        );
        let out_drop = rule(serde_json::json!({ "direction": "out", "action": "drop" }));
        let groups = vec![
            group("ssh", vec![ssh.clone()]),
            group("web", vec![web.clone(), smtp.clone()]),
            group("unused", vec![ssh.clone()]),
        ];
        // 노드와 상관없이 모든 VM을 본다.
        let a = vm(
            "a",
            Some(Uuid::now_v7()),
            vec![
                // 그룹 없는 NIC는 방화벽이 없다.
                nic(Some(mac(1)), &[]),
                // NIC의 그룹 순서대로 펼친다.
                nic(Some(mac(2)), &["web", "ssh"]),
                // 없는 그룹은 규칙이 없는 것이고 나가기도 막는다.
                nic(Some(mac(3)), &["gone"]),
            ],
        );
        let b = vm(
            "b",
            None,
            vec![
                // MAC이 없는 NIC는 건너뛴다.
                nic(None, &["ssh"]),
                nic(Some(mac(5)), &["ssh", "gone", "web"]),
            ],
        );
        let c = vm("c", None, vec![nic(Some(mac(6)), &[])]);
        assert_eq!(
            firewall_nics(&[a.clone(), b.clone(), c], &groups),
            vec![
                FirewallNic {
                    tap: tap_name(a.id, 1),
                    mac: mac(2),
                    rules: vec![web.clone(), smtp.clone(), ssh.clone()],
                },
                FirewallNic {
                    tap: tap_name(a.id, 2),
                    mac: mac(3),
                    rules: vec![out_drop.clone()],
                },
                FirewallNic {
                    tap: tap_name(b.id, 1),
                    mac: mac(5),
                    rules: vec![ssh, web, smtp, out_drop],
                },
            ]
        );
        assert_eq!(firewall_nics(&[], &groups), vec![]);
    }

    fn test_config(witness: bool) -> Config {
        toml::from_str(&format!(
            "name = \"n\"\naddress = \"h\"\nwitness = {witness}\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false"
        ))
        .unwrap()
    }

    fn test_host(firewall: Option<Arc<RecordingFirewall>>) -> Host {
        let mut host = Host::from_config(&test_config(false), "t").unwrap();
        host.net = None;
        host.firewall = firewall.map(|f| f as Arc<dyn Firewall>);
        host
    }

    #[tokio::test]
    async fn runs_only_on_a_non_witness_node_with_a_firewall() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let host = test_host(None);
        assert!(
            FirewallRules::new(cluster.clone(), &test_config(false), &host, "t".into()).is_none()
        );
        let host = test_host(Some(Arc::default()));
        assert!(
            FirewallRules::new(cluster.clone(), &test_config(true), &host, "t".into()).is_none()
        );
        assert!(
            FirewallRules::new(cluster.clone(), &test_config(false), &host, "t".into()).is_some()
        );
        cluster.shutdown().await.unwrap();
    }

    fn rules_for(cluster: &Arc<Cluster>, firewall: &Arc<RecordingFirewall>) -> Arc<FirewallRules> {
        FirewallRules::new(
            cluster.clone(),
            &test_config(false),
            &test_host(Some(firewall.clone())),
            "abc".into(),
        )
        .unwrap()
    }

    /// 같은 글이면 다시 넣지 않고, 넣기가 실패했거나 강제면(주기 차례, 시작 직전 확인) 다시 넣는다.
    #[tokio::test]
    async fn skips_the_same_ruleset_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let firewall = Arc::new(RecordingFirewall::default());
        let rules = rules_for(&cluster, &firewall);
        let count = || firewall.applied.lock().unwrap().len();

        assert!(rules.apply(false).await.unwrap());
        assert_eq!(
            *firewall.applied.lock().unwrap(),
            ["table bridge opl_abc {}\ndelete table bridge opl_abc\n"]
        );
        assert!(!rules.apply(false).await.unwrap());
        assert_eq!(count(), 1);
        assert!(rules.apply(true).await.unwrap());
        assert_eq!(count(), 2);
        rules.sync().await.unwrap();
        assert_eq!(count(), 3);

        // 실패하면 같은 글이어도 다음 차례에 다시 넣는다.
        firewall
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let err = rules.sync().await.unwrap_err();
        assert!(
            err.contains("cannot apply the firewall rules") && err.contains("boom"),
            "{err}"
        );
        firewall
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(rules.apply(false).await.unwrap());
        assert_eq!(count(), 5);
        assert!(!rules.apply(false).await.unwrap());
        cluster.shutdown().await.unwrap();
    }

    /// 조건이 맞을 때까지 기다린다.
    async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(tokio::time::Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// VM·그룹이 바뀌면 주기를 기다리지 않고 다시 넣는다. 규칙 글이 같으면(방화벽과 상관없는 변경)
    /// 다시 넣지 않는다.
    #[tokio::test]
    async fn reapplies_on_vm_and_group_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(me)) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let firewall = Arc::new(RecordingFirewall::default());
        let rules = rules_for(&cluster, &firewall);
        let (stop, rx) = oneshot::channel();
        // 주기는 길게 둔다. 시작할 때 한 번, 그 뒤로는 바뀔 때마다 넣는다.
        let task = tokio::spawn(run_firewall(rules, Duration::from_secs(3600), rx));
        let applied = || firewall.applied.lock().unwrap().clone();
        let last = || applied().last().cloned().unwrap_or_default();

        wait_for("first apply", || applied().len() == 1).await;
        let web = group(
            "web",
            vec![rule(
                serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "80" }),
            )],
        );
        cluster.create(&web).await.unwrap();
        let guarded = vm("guarded", Some(me), vec![nic(Some(mac(1)), &["web"])]);
        cluster.create(&guarded).await.unwrap();
        let tap = tap_name(guarded.id, 0);
        wait_for("apply after the VM is created", || {
            last().contains(&format!("chain i_{tap} {{")) && last().contains("th dport 80 accept")
        })
        .await;

        // 그룹 규칙을 바꾸면 곧바로 다시 넣는다.
        let mut spec = web.spec.clone();
        spec.rules = vec![rule(
            serde_json::json!({ "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" }),
        )];
        cluster
            .update_spec(web.id, None, web.labels.clone(), &spec)
            .await
            .unwrap();
        wait_for("apply after the group changes", || {
            last().contains("th dport 443 accept") && !last().contains("th dport 80 accept")
        })
        .await;

        // 방화벽과 상관없는 VM은 글을 바꾸지 않으므로 다시 넣지 않는다.
        let before = applied().len();
        let plain = vm("plain", Some(me), vec![nic(Some(mac(2)), &[])]);
        cluster.create(&plain).await.unwrap();
        tokio::time::sleep(DEBOUNCE * 3).await;
        assert_eq!(applied().len(), before);

        // VM을 지우면 테이블을 지우기만 하는 글이 된다.
        cluster.delete::<VmSpec>(guarded.id, None).await.unwrap();
        wait_for("apply after the VM is deleted", || {
            last() == "table bridge opl_abc {}\ndelete table bridge opl_abc\n"
        })
        .await;
        stop.send(()).unwrap();
        task.await.unwrap();
        cluster.shutdown().await.unwrap();
    }

    /// 클러스터에서 빠질 때만 테이블을 지운다.
    #[tokio::test]
    async fn leaving_removes_the_table() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(me)) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        cluster.create(&group("web", vec![])).await.unwrap();
        cluster
            .create(&vm("guarded", Some(me), vec![nic(Some(mac(1)), &["web"])]))
            .await
            .unwrap();
        let firewall = Arc::new(RecordingFirewall::default());
        let applied = || firewall.applied.lock().unwrap().clone();
        let empty = "table bridge opl_abc {}\ndelete table bridge opl_abc\n";

        let agent = FirewallAgent::spawn(rules_for(&cluster, &firewall));
        wait_for("first apply", || applied().len() == 1).await;
        agent.shutdown(false).await;
        assert_eq!(applied().len(), 1);
        assert!(applied()[0].contains("chain i_"));

        let agent = FirewallAgent::spawn(rules_for(&cluster, &firewall));
        wait_for("first apply", || applied().len() == 2).await;
        agent.shutdown(true).await;
        assert_eq!(applied().len(), 3);
        assert_eq!(applied()[2], empty);
        cluster.shutdown().await.unwrap();
    }
}
