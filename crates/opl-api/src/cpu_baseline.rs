//! 리더의 클러스터 CPU 기준 기록(계획 63 §9). 정리 작업([`crate::janitor`])의 리더 tick마다 노드·카탈로그·설정으로
//! 기준을 다시 셈해([`opl_types::cpu::baseline`]) `ClusterStatus.cpu_baselines`와 다를 때만 쓴다. 모델·플래그가 바뀐
//! 아키텍처마다 기록 Task `cpu-baseline`과 감사 항목(`cpu.baseline`)을 남긴다. 켜기는 이 기록만 읽으므로 모든 노드가 같은
//! 기준을 쓴다. 모든 노드가 `cpu-models`를 알릴 때만 일한다(옛 노드는 새 Task를 풀지 못한다).

use std::collections::BTreeMap;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::audit::AuditTarget;
use opl_types::cluster::ClusterSpec;
use opl_types::cpu::{
    self, Baseline, BaselineMode, BaselineSource, CpuCatalogSpec, CpuModel, CpuSettings,
    PreviousBaseline,
};
use opl_types::node::{CPU_MODELS, Node, NodeSpec, all_have};
use opl_types::resource::Resource;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Arch, Vm, VmPhase, VmSpec};

use crate::audit::system_entry;
use crate::state::AppState;

/// 모델·플래그가 바뀐 아키텍처 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    pub arch: Arch,
    /// 앞 기준(없으면 처음 정해짐)
    pub previous: Option<Baseline>,
    /// 새 기준(없으면 기준이 없어짐 — off로 바꿈, 고정을 뺌, 그 아키텍처의 노드가 모두 빠짐)
    pub current: Option<Baseline>,
}

/// 새로 셈한 기준 `fresh`(`since`·`previous`가 빈 것)를 앞 기록 `old`와 견준다(순수). 모델·플래그가 같으면 앞의
/// `since`·`previous`를 이어 받고(노드·`limited_by`·경고만 바뀌었으면 기록은 고치되 바뀜으로 세지 않는다), 다르면
/// `since = now`이고 `previous`는 앞 모델·플래그다. 쓸 기록과 모델·플래그가 바뀐 아키텍처를 돌려준다.
pub(crate) fn merge(
    old: &BTreeMap<Arch, Baseline>,
    fresh: BTreeMap<Arch, Baseline>,
    now: u64,
) -> (BTreeMap<Arch, Baseline>, Vec<Change>) {
    let mut changes = Vec::new();
    let mut out = BTreeMap::new();
    for (arch, mut b) in fresh {
        match old.get(&arch) {
            Some(prev) if prev.same_cpu(&b) => {
                b.since = prev.since;
                b.previous = prev.previous.clone();
            }
            prev => {
                b.since = now;
                b.previous = prev.map(|p| PreviousBaseline {
                    model: p.model.clone(),
                    flags: p.flags.clone(),
                });
                changes.push(Change {
                    arch,
                    previous: prev.cloned(),
                    current: Some(b.clone()),
                });
            }
        }
        out.insert(arch, b);
    }
    for (arch, prev) in old {
        if !out.contains_key(arch) {
            changes.push(Change {
                arch: *arch,
                previous: Some(prev.clone()),
                current: None,
            });
        }
    }
    (out, changes)
}

/// 바뀌기 전 기준으로 도는 `cluster` 모델 VM 수(이 아키텍처, 실행 중)
fn running_cluster_vms(vms: &[Vm], arch: Arch) -> u32 {
    let n = vms
        .iter()
        .filter(|v| v.spec.cpu.model == CpuModel::Cluster && v.spec.machine.arch() == arch)
        .filter(|v| matches!(v.status.phase, VmPhase::Running | VmPhase::Migrating))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn arch_name(arch: Arch) -> String {
    format!("{arch:?}").to_lowercase()
}

/// 기록 Task의 작업
pub(crate) fn record_op(change: &Change, nodes: &[Node], vms: &[Vm]) -> TaskOp {
    let limited_by = change
        .current
        .iter()
        .flat_map(|b| &b.limited_by)
        .map(|id| {
            nodes
                .iter()
                .find(|n| n.id == *id)
                .map_or_else(|| id.to_string(), |n| n.name.clone())
        })
        .collect();
    TaskOp::CpuBaseline {
        arch: change.arch,
        mode: change.current.as_ref().map(|b| b.mode),
        previous: change.previous.as_ref().map(Baseline::describe),
        current: change.current.as_ref().map(Baseline::describe),
        limited_by,
        running_vms: running_cluster_vms(vms, change.arch),
    }
}

/// 감사 detail: `cpu baseline x86_64 auto: Cascadelake-Server-v5 -> Skylake-Server-v5 (node n4)`
pub(crate) fn audit_detail(op: &TaskOp) -> String {
    let TaskOp::CpuBaseline {
        arch,
        mode,
        previous,
        current,
        limited_by,
        ..
    } = op
    else {
        return String::new();
    };
    let mode = match mode {
        Some(cpu::BaselineSource::Auto) => " auto",
        Some(cpu::BaselineSource::Pinned) => " pinned",
        None => "",
    };
    let mut s = format!(
        "cpu baseline {}{mode}: {} -> {}",
        arch_name(*arch),
        previous.as_deref().unwrap_or("none"),
        current.as_deref().unwrap_or("none"),
    );
    if !limited_by.is_empty() {
        let word = if limited_by.len() == 1 {
            "node"
        } else {
            "nodes"
        };
        s.push_str(&format!(" ({word} {})", limited_by.join(", ")));
    }
    s
}

/// 리더가 부른다(정리 작업의 tick, 장벽 뒤). 기준을 다시 셈해 달라졌을 때만 쓰고 바뀜마다 기록을 남긴다. 그사이 클러스터
/// 상태가 바뀌었으면(revision) 다음 tick에 다시 한다.
pub(crate) async fn record(state: &AppState) {
    if let Err(e) = try_record(state).await {
        tracing::warn!(error = %e, "cannot record the cluster CPU baseline; will retry");
    }
}

/// 노드가 알린 해시의 카탈로그가 없을 때 앞 기록을 붙잡는 가장 긴 시간(그 탐침부터, 리뷰 2 #2)
pub(crate) const CATALOG_HOLD_SECS: u64 = 600;

/// 리더가 기록할 기준을 셈한다(순수). [`cpu::baseline`]에 더해, 노드가 알린 해시의 카탈로그 리소스가 없는 동안(정리
/// 작업이 같은 때 오래된 같은 해시를 지움, 올리기 실패) 그 아키텍처의 auto 기준은 앞 기록을 붙잡는다 — 그 노드가 빠져
/// 기준이 올랐다 내려오지 않게(리뷰 1 I1). 붙잡기에는 끝이 있다(리뷰 2 #2):
///
/// - 그 노드가 온라인이고 그 탐침이 [`CATALOG_HOLD_SECS`]보다 오래되지 않았을 때만이다. 그 뒤에는 그 노드를 빼고 셈하고
///   기준의 `warnings`에 적는다(꺼진 노드의 사라진 카탈로그가 기준을 영영 묶지 않게).
/// - 그 노드의 아키텍처만이다(aarch64 노드가 x86_64 기준을 묶지 않는다).
/// - 설정이 auto이고 앞 기록도 auto일 때만이다 — 고정·끔·auto로 바꾸기는 곧바로 적용된다. 붙잡는 동안에도 숨김 목록과
///   `auto_flags` 끄기는 적용한다(모델과 더한 플래그만 앞 것).
///
/// `old`는 지금 기록(`ClusterStatus.cpu_baselines`), `now`는 유닉스 초다.
pub(crate) fn compute(
    nodes: &[Node],
    catalogs: &BTreeMap<String, CpuCatalogSpec>,
    settings: &CpuSettings,
    old: &BTreeMap<Arch, Baseline>,
    now: u64,
) -> BTreeMap<Arch, Baseline> {
    let mut fresh = cpu::baseline(nodes, catalogs, settings);
    let mut holding: BTreeMap<Arch, Vec<&str>> = BTreeMap::new();
    for n in nodes.iter().filter(|n| !n.spec.witness) {
        let (Some(arch), Some(c)) = (n.status.arch, n.status.cpu.as_ref()) else {
            continue;
        };
        if c.catalog.is_empty() || catalogs.contains_key(&c.catalog) {
            continue;
        }
        if n.status.online && now.saturating_sub(c.probed_at) < CATALOG_HOLD_SECS {
            holding.entry(arch).or_default().push(&n.name);
        }
    }
    for (arch, names) in holding {
        let auto = settings.pinned_for(arch).is_none() && settings.baseline == BaselineMode::Auto;
        let Some(prev) = old
            .get(&arch)
            .filter(|o| auto && o.mode == BaselineSource::Auto)
        else {
            continue;
        };
        let mut kept = prev.clone();
        // 모델과 더한 플래그는 앞 것, 꺼진 플래그(숨김 목록)는 지금 설정의 것이다.
        let now_off = fresh
            .get(&arch)
            .map(|f| f.flags.clone())
            .unwrap_or_default();
        kept.flags = prev
            .flags
            .iter()
            .filter(|(_, on)| **on && settings.auto_flags)
            .chain(now_off.iter().filter(|(_, on)| !**on))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let left_out: Vec<String> = names
            .iter()
            .map(|name| {
                format!(
                    "node {name:?} announced a CPU catalog that is missing from the cluster and is left out"
                )
            })
            .collect();
        kept.warnings = fresh
            .get(&arch)
            .map(|f| f.warnings.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|w| !left_out.contains(w))
            .collect();
        for name in names {
            kept.warnings.push(format!(
                "node {name:?} announced a CPU catalog that is missing from the cluster; the previous baseline is kept for up to {} minutes",
                CATALOG_HOLD_SECS / 60
            ));
        }
        fresh.insert(arch, kept);
    }
    fresh
}

async fn try_record(state: &AppState) -> Result<(), ClusterError> {
    let cluster: &Cluster = &state.cluster;
    let (_, nodes) = cluster.list::<NodeSpec>()?;
    if nodes.is_empty() || !all_have(&nodes, CPU_MODELS) {
        return Ok(());
    }
    let Some(info) = cluster.list::<ClusterSpec>()?.1.into_iter().next() else {
        return Ok(());
    };
    // 노드가 알린 카탈로그만, 풀어 둔 것으로(리뷰 2 #9)
    let catalogs: BTreeMap<String, CpuCatalogSpec> =
        crate::scheduler::node_catalogs(cluster, &nodes)?
            .into_iter()
            .map(|(k, v)| (k, v.as_ref().clone()))
            .collect();
    let settings = crate::settings::current(cluster)?
        .spec
        .cpu
        .unwrap_or_default();
    let now = crate::auth::unix_now();
    let fresh = compute(
        &nodes,
        &catalogs,
        &settings,
        &info.status.cpu_baselines,
        now,
    );
    let (merged, changes) = merge(&info.status.cpu_baselines, fresh, now);
    if merged == info.status.cpu_baselines {
        return Ok(());
    }
    // 새로 생긴 경고(카탈로그 없는 노드, 붙잡기, 켤 수 없는 노드)는 로그에도 남긴다.
    for (arch, b) in &merged {
        let before = info.status.cpu_baselines.get(arch);
        for w in &b.warnings {
            if before.is_none_or(|o| !o.warnings.contains(w)) {
                tracing::warn!(arch = %arch_name(*arch), "cluster CPU baseline: {w}");
            }
        }
    }
    let mut status = info.status.clone();
    status.cpu_baselines = merged;
    match cluster
        .update_status_checked::<ClusterSpec>(info.id, info.revision, &status)
        .await
    {
        Ok(_) => {}
        // 관찰 노드 기록 등이 그사이 썼다. 다음 tick에 다시 셈한다.
        Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => return Ok(()),
        Err(e) => return Err(e),
    }
    if changes.is_empty() {
        return Ok(());
    }
    let (_, vms) = cluster.list::<VmSpec>()?;
    for change in &changes {
        let op = record_op(change, &nodes, &vms);
        let detail = audit_detail(&op);
        tracing::info!("{detail}");
        let mut task: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: info.id,
                node: cluster.id().0,
            },
        );
        task.name = task.id.to_string();
        // 실행기는 끝난 Task를 실행하지 않는다.
        task.status = TaskStatus {
            state: TaskState::Succeeded,
            error: None,
            progress: None,
        };
        if let Err(e) = cluster.create(&task).await {
            tracing::warn!(error = %e, "cannot record the CPU baseline task");
        }
        let target = AuditTarget {
            kind: "cpu".into(),
            key: arch_name(change.arch),
        };
        state
            .audit
            .record(system_entry("cpu.baseline", target, detail));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use opl_types::cpu::BaselineSource;
    use uuid::Uuid;

    use super::*;

    fn base(model: &str, flags: &[(&str, bool)]) -> Baseline {
        Baseline {
            model: model.into(),
            flags: flags.iter().map(|(f, on)| (f.to_string(), *on)).collect(),
            mode: BaselineSource::Auto,
            nodes: vec![],
            limited_by: vec![],
            since: 0,
            previous: None,
            warnings: vec![],
        }
    }

    fn cat_node(name: &str, arch: Arch, hash: Option<&str>, online: bool, probed_at: u64) -> Node {
        let mut n: Node = Resource::new(
            name,
            opl_types::node::NodeSpec {
                address: format!("{name}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status.arch = Some(arch);
        n.status.online = online;
        n.status.hypervisor = "qemu-kvm".into();
        n.status.cpu = hash.map(|h| cpu::NodeCpu {
            catalog: h.into(),
            accel: "kvm".into(),
            vendor: Some("GenuineIntel".into()),
            probed_at,
            ..Default::default()
        });
        n
    }

    /// 리뷰 1 I1·리뷰 2 #2: 알린 카탈로그가 없는 노드는 auto 기준을 잠깐만 붙잡는다 — 온라인이고 탐침이 10분 안일
    /// 때, 그 아키텍처만, 설정이 auto일 때만. 그 뒤에는 그 노드를 빼고 셈하고 경고를 남긴다.
    #[test]
    fn a_missing_catalog_holds_the_auto_baseline_only_briefly_and_only_for_its_arch() {
        let x = Arch::X86_64;
        let probe = opl_driver::mock_cpu_probe().catalog;
        let catalogs = BTreeMap::from([(probe.hash(), probe.clone())]);
        let h = probe.hash();
        let auto = CpuSettings {
            baseline: BaselineMode::Auto,
            auto_flags: false,
            mask: vec!["avx512vbmi".into()],
            ..CpuSettings::default()
        };
        let now = 10_000;
        let mut prev = base(
            "Cascadelake-Server-v5",
            &[("pcid", true), ("avx512vbmi", false)],
        );
        prev.since = 7;
        let old = BTreeMap::from([(x, prev.clone())]);
        let n1 = cat_node("n1", x, Some(&h), true, now);
        // 붙잡기: 온라인이고 방금 탐침했다. 모델과 더한 플래그는 앞 것, 꺼진 플래그는 지금 숨김 목록이다.
        let fresh_gap = cat_node("n5", x, Some("gone"), true, now - 30);
        let held = compute(&[n1.clone(), fresh_gap], &catalogs, &auto, &old, now);
        let b = &held[&x];
        assert_eq!(b.model, "Cascadelake-Server-v5");
        assert_eq!(b.flags, BTreeMap::from([("avx512vbmi".to_string(), false)]));
        assert!(
            b.warnings.iter().any(|w| w.contains("node \"n5\" announced a CPU catalog that is missing from the cluster; the previous baseline is kept")),
            "{:?}",
            b.warnings
        );
        let auto_flags = CpuSettings {
            auto_flags: true,
            ..auto.clone()
        };
        let held = compute(
            &[n1.clone(), cat_node("n5", x, Some("gone"), true, now - 30)],
            &catalogs,
            &auto_flags,
            &old,
            now,
        );
        assert_eq!(held[&x].flags.get("pcid"), Some(&true));
        // 오래된 탐침이나 꺼진 노드는 붙잡지 않는다: 그 노드를 빼고 셈하고 경고한다.
        for gap in [
            cat_node("n5", x, Some("gone"), true, now - CATALOG_HOLD_SECS),
            cat_node("n5", x, Some("gone"), false, now),
        ] {
            let got = compute(&[n1.clone(), gap], &catalogs, &auto, &old, now);
            let b = &got[&x];
            assert_ne!(b.model, "Cascadelake-Server-v5");
            assert!(b.warnings.contains(
                &"node \"n5\" announced a CPU catalog that is missing from the cluster and is left out".to_string()
            ));
        }
        // 고정·끔으로 바꾸기는 붙잡기와 상관없이 곧바로 적용된다.
        let gap = || cat_node("n5", x, Some("gone"), true, now);
        let off = CpuSettings::default();
        assert!(compute(&[n1.clone(), gap()], &catalogs, &off, &old, now).is_empty());
        let pinned = CpuSettings {
            pinned: vec![cpu::PinnedBaseline {
                arch: x,
                model: "Skylake-Server-v5".parse().unwrap(),
                flags: Vec::new(),
            }],
            ..CpuSettings::default()
        };
        let got = compute(&[n1.clone(), gap()], &catalogs, &pinned, &old, now);
        assert_eq!(got[&x].model, "Skylake-Server-v5");
        assert_eq!(got[&x].mode, BaselineSource::Pinned);
        // 다른 아키텍처의 빠진 카탈로그는 x86_64 기준을 묶지 않는다.
        let arm = cat_node("a1", Arch::Aarch64, Some("gone"), true, now);
        let got = compute(&[n1.clone(), arm], &catalogs, &auto, &old, now);
        assert_ne!(got[&x].model, "Cascadelake-Server-v5");
    }

    #[test]
    fn the_first_baseline_is_a_change_with_no_previous() {
        let fresh = BTreeMap::from([(Arch::X86_64, base("Skylake-Server-v5", &[]))]);
        let (merged, changes) = merge(&BTreeMap::new(), fresh, 100);
        assert_eq!(merged[&Arch::X86_64].since, 100);
        assert_eq!(merged[&Arch::X86_64].previous, None);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].previous, None);
    }

    #[test]
    fn the_same_cpu_keeps_since_and_previous_but_takes_new_nodes_and_warnings() {
        let mut old = base("Skylake-Server-v5", &[("pcid", true)]);
        old.since = 50;
        old.previous = Some(PreviousBaseline {
            model: "Cascadelake-Server-v5".into(),
            flags: BTreeMap::new(),
        });
        let old = BTreeMap::from([(Arch::X86_64, old)]);
        let mut fresh = base("Skylake-Server-v5", &[("pcid", true)]);
        let n = Uuid::now_v7();
        fresh.nodes = vec![n];
        fresh.warnings = vec!["w".into()];
        let (merged, changes) = merge(&old, BTreeMap::from([(Arch::X86_64, fresh)]), 100);
        assert!(changes.is_empty());
        let b = &merged[&Arch::X86_64];
        assert_eq!(b.since, 50);
        assert_eq!(b.previous.as_ref().unwrap().model, "Cascadelake-Server-v5");
        assert_eq!(b.nodes, vec![n]);
        assert_eq!(b.warnings, vec!["w".to_string()]);
        // 아무것도 바뀌지 않으면 앞 기록과 같다(쓰지 않는다).
        let (again, changes) = merge(
            &merged,
            BTreeMap::from([(Arch::X86_64, {
                let mut f = b.clone();
                f.since = 0;
                f.previous = None;
                f
            })]),
            200,
        );
        assert!(changes.is_empty());
        assert_eq!(again, merged);
    }

    #[test]
    fn a_lower_baseline_records_the_previous_cpu_and_a_removed_one_is_a_change() {
        let mut old = base("Cascadelake-Server-v5", &[("pcid", true)]);
        old.since = 50;
        let old = BTreeMap::from([
            (Arch::X86_64, old),
            (Arch::Aarch64, base("neoverse-n1", &[])),
        ]);
        let fresh = BTreeMap::from([(Arch::X86_64, base("Skylake-Server-v5", &[]))]);
        let (merged, changes) = merge(&old, fresh, 100);
        let b = &merged[&Arch::X86_64];
        assert_eq!(b.since, 100);
        assert_eq!(
            b.previous,
            Some(PreviousBaseline {
                model: "Cascadelake-Server-v5".into(),
                flags: BTreeMap::from([("pcid".to_string(), true)]),
            })
        );
        assert!(!merged.contains_key(&Arch::Aarch64));
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].arch, Arch::Aarch64);
        assert_eq!(changes[1].current, None);
    }

    #[test]
    fn the_record_names_the_limiting_nodes_and_counts_running_cluster_vms() {
        let mut node: Node = Resource::new(
            "n4",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.status.arch = Some(Arch::X86_64);
        let mut current = base("Skylake-Server-v5", &[("pcid", true)]);
        current.limited_by = vec![node.id];
        let change = Change {
            arch: Arch::X86_64,
            previous: Some(base("Cascadelake-Server-v5", &[])),
            current: Some(current),
        };
        let vm = |model: CpuModel, phase: VmPhase| {
            let spec = VmSpec {
                cpu: opl_types::vm::CpuSpec {
                    flags: Vec::new(),
                    sockets: 1,
                    cores: 1,
                    model,
                    max_vcpus: None,
                },
                memory_mib: 512,
                firmware: Default::default(),
                machine: opl_types::vm::MachineType::Q35,
                disks: vec![],
                nics: vec![],
                power: opl_types::vm::PowerState::Running,
                restart: Default::default(),
                node: None,
                snapshot_schedule: None,
                ha: None,
                placement: None,
                max_memory_mib: None,
                replication: Vec::new(),
                cdroms: vec![],
                cloud_init: None,
                template: false,
                tpm: false,
                secure_boot: false,
                guest_agent: None,
                folder: None,
            };
            let mut v: Vm = Resource::new("v", spec);
            v.status.phase = phase;
            v
        };
        let vms = vec![
            vm(CpuModel::Cluster, VmPhase::Running),
            vm(CpuModel::Cluster, VmPhase::Stopped),
            vm(CpuModel::Host, VmPhase::Running),
        ];
        let op = record_op(&change, &[node], &vms);
        assert_eq!(
            op,
            TaskOp::CpuBaseline {
                arch: Arch::X86_64,
                mode: Some(BaselineSource::Auto),
                previous: Some("Cascadelake-Server-v5".into()),
                current: Some("Skylake-Server-v5 +pcid".into()),
                limited_by: vec!["n4".into()],
                running_vms: 1,
            }
        );
        assert_eq!(
            audit_detail(&op),
            "cpu baseline x86_64 auto: Cascadelake-Server-v5 -> Skylake-Server-v5 +pcid (node n4)"
        );
    }
}
