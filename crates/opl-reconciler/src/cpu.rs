//! 켤 때의 CPU 해석 문맥(계획 63): 이 노드의 아키텍처·가속기·카탈로그와 클러스터 기준·숨김 목록. 작업자가 VM을 켤 때
//! [`CpuContext::start`]로 `-cpu`를 정해 `RunningHardware.cpu_resolved`에 기록하고, 검사 루프([`refresh_pending`])와
//! 핫플러그 Task가 [`CpuContext::wanted`]로 실행 중 VM의 보류 `cpu`를 다시 셈한다.

use std::collections::BTreeMap;
use std::sync::Arc;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::cluster::ClusterSpec;
use opl_types::cpu::{self, Accel, Baseline, CpuCatalogSpec, CpuError, ResolvedCpu};
use opl_types::node::{CPU_MODELS, Node, NodeSpec, all_have};
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use opl_types::vm::{Arch, CpuSpec, PendingChange, VmPhase, VmSpec};
use uuid::Uuid;

/// 이 노드의 CPU 카탈로그
#[derive(Debug, Clone, PartialEq)]
pub enum NodeCatalog {
    /// `NodeStatus.cpu`가 없거나 첫 탐침이 실패했다(빈 해시): `host`·`max`만 켠다.
    None,
    /// 노드가 알린 해시의 카탈로그 리소스가 아직 없다(검사 회차가 곧 만든다).
    Missing(String),
    /// 여러 노드가 같은 카탈로그를 나눠 쓴다(스케줄러는 노드마다 문맥을 만든다 — 40 KB를 베끼지 않게).
    Found(Arc<CpuCatalogSpec>),
}

/// 켜지 않는 까닭
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuRefusal {
    /// 기다리면 풀린다(카탈로그가 아직 없음, 클러스터를 읽지 못함).
    Waiting(String),
    /// 이 노드에서 켤 수 없다(스펙이나 배치를 바꿔야 한다).
    Cannot(String),
}

/// VM을 켤 노드의 해석 문맥
#[derive(Debug, Clone, PartialEq)]
pub struct CpuContext {
    pub node_name: String,
    pub arch: Arch,
    pub accel: Accel,
    /// 이 아키텍처의 클러스터 기준(`ClusterStatus.cpu_baselines`)
    pub baseline: Option<Baseline>,
    /// 클러스터 숨김 목록(설정 `cpu.mask`)
    pub mask: Vec<String>,
    pub catalog: NodeCatalog,
}

impl CpuContext {
    /// 노드 `node`의 문맥. 모든 노드가 `cpu-models`를 알리지 않았거나(업그레이드 중 — 기록을 바이트 그대로 둔다) 노드의
    /// 아키텍처·가속기를 모르면 None이다(그때 명령줄은 모델만 편다).
    pub fn load(cluster: &Cluster, node: Uuid) -> Result<Option<Self>, ClusterError> {
        let (_, nodes) = cluster.list::<NodeSpec>()?;
        let Some(me) = nodes.iter().find(|n| n.id == node) else {
            return Ok(None);
        };
        if !all_have(&nodes, CPU_MODELS) {
            return Ok(None);
        }
        let catalog = match me.status.cpu.as_ref().map(|c| c.catalog.as_str()) {
            None | Some("") => None,
            Some(hash) => cluster
                .get_by_name::<CpuCatalogSpec>(hash)?
                .map(|c| Arc::new(c.spec)),
        };
        let mask = cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)?
            .and_then(|s| s.spec.cpu)
            .map(|c| c.mask)
            .unwrap_or_default();
        let baselines = cluster
            .list::<ClusterSpec>()?
            .1
            .into_iter()
            .next()
            .map(|c| c.status.cpu_baselines)
            .unwrap_or_default();
        Ok(Self::from_parts(&nodes, me, |_| catalog, &baselines, &mask))
    }

    /// 이미 읽은 것으로 노드 `node`의 문맥을 만든다(순수, [`CpuContext::load`]와 같은 규칙). `catalog`는 노드가 알린
    /// 해시로 카탈로그 리소스를 찾는다. 스케줄러(opl-api `Inventory`)가 작업자와 같은 규칙으로 노드를 거르려고 쓴다 —
    /// 그래야 배치한 노드가 켜기를 거절하지 않는다.
    pub fn from_parts(
        nodes: &[Node],
        node: &Node,
        catalog: impl FnOnce(&str) -> Option<Arc<CpuCatalogSpec>>,
        baselines: &BTreeMap<Arch, Baseline>,
        mask: &[String],
    ) -> Option<Self> {
        if !all_have(nodes, CPU_MODELS) {
            return None;
        }
        let arch = node.status.arch?;
        let accel = Accel::parse(&node.status.hypervisor)?;
        let catalog = match node.status.cpu.as_ref().map(|c| c.catalog.as_str()) {
            None | Some("") => NodeCatalog::None,
            Some(hash) => match catalog(hash) {
                Some(c) => NodeCatalog::Found(c),
                None => NodeCatalog::Missing(hash.to_string()),
            },
        };
        Some(CpuContext {
            node_name: node.name.clone(),
            arch,
            accel,
            baseline: baselines.get(&arch).cloned(),
            mask: mask.to_vec(),
            catalog,
        })
    }

    fn found(&self) -> Option<&CpuCatalogSpec> {
        match &self.catalog {
            NodeCatalog::Found(c) => Some(c),
            _ => None,
        }
    }

    /// 스펙 `cpu`를 이 노드에서 지금 기준·숨김 목록으로 해석한다.
    pub fn resolve(&self, cpu: &CpuSpec) -> Result<ResolvedCpu, CpuError> {
        cpu::resolve(
            cpu,
            self.arch,
            self.accel,
            self.baseline.as_ref(),
            &self.mask,
            self.found(),
        )
    }

    /// 지금 켜면 쓸 CPU(보류 `cpu` 셈). 해석할 수 없으면 None(스펙만 견준다).
    pub fn wanted(&self, cpu: &CpuSpec) -> Option<ResolvedCpu> {
        self.resolve(cpu).ok()
    }

    /// 해석한 CPU를 이 노드가 켤 수 있는지. 플래그 없는 `host`·`max`는 늘 된다. 그 밖에는 카탈로그가 있고 `can_run`
    /// 이어야 한다 — 플래그(숨김 목록 포함)가 붙은 `host`·`max`도 그 이름을 QEMU가 아는지 카탈로그로 확인한다. 카탈로그가
    /// 없는 노드는 확인할 수 없는 숨김을 명령줄에 두지 않고, 숨김을 빼고 켜서 감춘 기능을 드러내지도 않는다(거절).
    pub fn check(&self, resolved: &ResolvedCpu) -> Result<(), CpuRefusal> {
        let host = matches!(resolved.model.as_str(), "host" | "max");
        if host && resolved.flags.is_empty() {
            return Ok(());
        }
        match &self.catalog {
            NodeCatalog::None if host => Err(CpuRefusal::Cannot(format!(
                "node {:?} has no CPU catalog, so the CPU flags {} cannot be checked there; only the host and max CPU models without flags or a cluster mask can run there",
                self.node_name,
                resolved
                    .flags
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            NodeCatalog::None => Err(CpuRefusal::Cannot(format!(
                "node {:?} has no CPU catalog; only the host and max CPU models can run there",
                self.node_name
            ))),
            NodeCatalog::Missing(hash) => Err(CpuRefusal::Waiting(format!(
                "waiting for the CPU catalog {hash} of node {:?}",
                self.node_name
            ))),
            NodeCatalog::Found(c) => cpu::can_run(resolved, c)
                .map_err(|e| CpuRefusal::Cannot(format!("node {:?} {e}", self.node_name))),
        }
    }

    /// 다른 노드에서 켠 기록 `resolved`를 이 노드의 QEMU로 그대로 켜면(받는 쪽 마이그레이션, 메모리 스냅샷 되돌리기)
    /// 드러날 숨김 이름(리뷰 2 #1). 켤 때 그 노드의 QEMU가 몰라 뺀 이름(`dropped_mask`)을 이 노드의 QEMU가 알면
    /// `-cpu host`가 그 기능을 준다. 이 노드의 카탈로그를 모르면(없음·아직 없음) 확인할 수 없으니 뺀 이름 모두를
    /// 드러날 것으로 본다. 빈 목록이면 안전하다.
    pub fn exposed(&self, resolved: &ResolvedCpu) -> Vec<String> {
        match self.found() {
            Some(c) => resolved
                .dropped_mask
                .iter()
                .filter(|m| c.flag_index(m).is_some())
                .cloned()
                .collect(),
            None => resolved.dropped_mask.clone(),
        }
    }

    /// [`CpuContext::exposed`]가 비지 않으면 그 까닭(`what`은 무엇을 하려다 막혔나)
    pub fn check_exposed(&self, resolved: &ResolvedCpu, what: &str) -> Result<(), String> {
        let exposed = self.exposed(resolved);
        if exposed.is_empty() {
            return Ok(());
        }
        let known = match self.catalog {
            NodeCatalog::Found(_) => "",
            _ => " (its CPU catalog is unknown, so the hidden flags cannot be checked)",
        };
        Err(format!(
            "node {:?}{known} would expose the hidden CPU flags {} {what}: the QEMU that started the VM did not know them; power cycle the VM instead",
            self.node_name,
            exposed.join(", ")
        ))
    }

    /// 켤 때의 CPU: 해석하고 이 노드가 켤 수 있는지 본다.
    pub fn start(&self, cpu: &CpuSpec) -> Result<ResolvedCpu, CpuRefusal> {
        let resolved = self
            .resolve(cpu)
            .map_err(|e| CpuRefusal::Cannot(e.to_string()))?;
        self.check(&resolved)?;
        Ok(resolved)
    }
}

/// 까닭의 앞 `node "n3" `를 `the VM's node `로 바꾼다(리뷰 2 #6). VM 상태(`last_error`)는 노드 이름을 볼 수 없는 폴더
/// 사용자도 읽으므로 작업자가 남기는 CPU 까닭에는 노드 이름을 쓰지 않는다. 노드로 시작하지 않으면 그대로다.
pub fn without_node_name(reason: &str) -> String {
    if let Some(rest) = reason.strip_prefix("node \"")
        && let Some(end) = rest.find("\" ")
    {
        return format!("the VM's node {}", &rest[end + 2..]);
    }
    reason.to_string()
}

/// 보류 목록 `pending`의 `cpu` 항목을 `cpu`로 바꾼다(없으면 뺀다, 새로 생기면 앞에 둔다 — 핫플러그 계획과 같은 자리).
pub fn with_cpu_pending(
    pending: &[PendingChange],
    cpu: Option<PendingChange>,
) -> Vec<PendingChange> {
    let mut out: Vec<PendingChange> = Vec::with_capacity(pending.len() + 1);
    let mut cpu = cpu;
    let mut seen = false;
    for p in pending {
        if p.field == "cpu" {
            seen = true;
            out.extend(cpu.take());
        } else {
            out.push(p.clone());
        }
    }
    if !seen && let Some(c) = cpu {
        out.insert(0, c);
    }
    out
}

/// 검사 루프: 이 노드에서 도는 VM마다 보류 `cpu`를 지금 기준·숨김 목록·카탈로그로 다시 셈해 달라졌을 때만 쓴다
/// (revision 확인, 그 사이 바뀌었으면 다음 회차). 문맥이 없으면(업그레이드 중) 아무것도 하지 않는다. 쓴 VM 수를
/// 돌려준다.
pub async fn refresh_pending(cluster: &Cluster, node: Uuid) -> Result<usize, ClusterError> {
    let (_, vms) = cluster.list::<VmSpec>()?;
    let running: Vec<_> = vms
        .into_iter()
        .filter(|vm| {
            vm.status.node == Some(node)
                && vm.status.phase == VmPhase::Running
                && vm.status.hardware.is_some()
        })
        .collect();
    if running.is_empty() {
        return Ok(0);
    }
    let Some(ctx) = CpuContext::load(cluster, node)? else {
        return Ok(0);
    };
    let mut written = 0;
    for vm in running {
        let Some(hw) = &vm.status.hardware else {
            continue;
        };
        let wanted = ctx.wanted(&vm.spec.cpu);
        let change = cpu::pending_change(&vm.spec.cpu, hw, wanted.as_ref());
        let pending = with_cpu_pending(&hw.pending, change);
        if pending == hw.pending {
            continue;
        }
        let mut status = vm.status.clone();
        if let Some(hw) = &mut status.hardware {
            hw.pending = pending;
        }
        match cluster
            .update_status_checked::<VmSpec>(vm.id, vm.revision, &status)
            .await
        {
            Ok(_) => written += 1,
            Err(ClusterError::Rejected(
                Rejection::RevisionMismatch { .. } | Rejection::NotFound { .. },
            )) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::vm::PendingReason;

    fn change(field: &str, wanted: &str) -> PendingChange {
        PendingChange {
            field: field.into(),
            running: "a".into(),
            wanted: wanted.into(),
            reason: PendingReason::Restart,
            detail: None,
        }
    }

    /// 리뷰 1 C1: 카탈로그 없는 노드는 확인할 수 없는 숨김을 명령줄에 두지 않는다.
    #[test]
    fn host_with_flags_needs_a_catalog_to_check_the_names() {
        let ctx = |catalog| CpuContext {
            node_name: "n1".into(),
            arch: Arch::X86_64,
            accel: Accel::Kvm,
            baseline: None,
            mask: vec!["avx512vbmi".into()],
            catalog,
        };
        let host = CpuSpec {
            sockets: 1,
            cores: 1,
            model: cpu::CpuModel::Host,
            max_vcpus: None,
            flags: Vec::new(),
        };
        let plain = ResolvedCpu {
            model: "host".into(),
            flags: BTreeMap::new(),
            enforce: false,
            source: cpu::CpuSource::Spec,
            dropped_mask: Vec::new(),
        };
        let none = ctx(NodeCatalog::None);
        assert_eq!(none.check(&plain), Ok(()));
        assert!(matches!(
            none.start(&host),
            Err(CpuRefusal::Cannot(m)) if m.contains("CPU flags avx512vbmi cannot be checked")
        ));
        assert!(matches!(
            ctx(NodeCatalog::Missing("h".into())).start(&host),
            Err(CpuRefusal::Waiting(_))
        ));
        let known = known_catalog();
        let found = ctx(NodeCatalog::Found(Arc::new(known)));
        assert_eq!(
            found.start(&host).map(|r| r.flags),
            Ok([("avx512vbmi".to_string(), false)].into())
        );
    }

    /// 리뷰 2 #1: 켤 때 뺀 숨김 이름은 그 이름을 아는 노드나 카탈로그를 모르는 노드에서 드러난다.
    #[test]
    fn dropped_mask_names_are_exposed_where_qemu_knows_them_or_cannot_tell() {
        let ctx = |catalog| CpuContext {
            node_name: "n2".into(),
            arch: Arch::X86_64,
            accel: Accel::Kvm,
            baseline: None,
            mask: vec!["avx512vbmi".into()],
            catalog,
        };
        let mut rec = ResolvedCpu {
            model: "host".into(),
            flags: BTreeMap::new(),
            enforce: false,
            source: cpu::CpuSource::Spec,
            dropped_mask: vec!["avx512vbmi".into()],
        };
        let knows = ctx(NodeCatalog::Found(Arc::new(known_catalog())));
        assert_eq!(knows.exposed(&rec), ["avx512vbmi"]);
        let e = knows.check_exposed(&rec, "on migration").unwrap_err();
        assert!(
            e.starts_with("node \"n2\" would expose the hidden CPU flags avx512vbmi on migration"),
            "{e}"
        );
        let mut old = known_catalog();
        old.flags = vec!["aes".into()];
        assert!(
            ctx(NodeCatalog::Found(Arc::new(old)))
                .exposed(&rec)
                .is_empty()
        );
        for unknown in [NodeCatalog::None, NodeCatalog::Missing("h".into())] {
            let e = ctx(unknown)
                .check_exposed(&rec, "on migration")
                .unwrap_err();
            assert!(e.contains("cannot be checked"), "{e}");
        }
        rec.dropped_mask.clear();
        assert_eq!(ctx(NodeCatalog::None).check_exposed(&rec, "x"), Ok(()));
    }

    /// `avx512vbmi`만 아는 x86 카탈로그
    fn known_catalog() -> CpuCatalogSpec {
        CpuCatalogSpec {
            arch: Arch::X86_64,
            accel: "kvm".into(),
            qemu: "11.1.2".into(),
            machine: Arch::X86_64.machine(),
            flags: vec!["avx512vbmi".into()],
            supported: cpu::Bits::from_indices([0]),
            vendor: None,
            models: Vec::new(),
        }
    }

    #[test]
    fn stored_reasons_do_not_name_the_node() {
        assert_eq!(
            without_node_name("node \"n3\" cannot run CPU model X (missing a)"),
            "the VM's node cannot run CPU model X (missing a)"
        );
        assert_eq!(without_node_name("unknown CPU model"), "unknown CPU model");
    }

    #[test]
    fn the_cpu_entry_is_replaced_in_place_added_first_or_removed() {
        let list = vec![change("firmware", "bios"), change("cpu", "old")];
        assert_eq!(
            with_cpu_pending(&list, Some(change("cpu", "new"))),
            vec![change("firmware", "bios"), change("cpu", "new")]
        );
        assert_eq!(
            with_cpu_pending(&list, None),
            vec![change("firmware", "bios")]
        );
        let list = vec![change("firmware", "bios")];
        assert_eq!(
            with_cpu_pending(&list, Some(change("cpu", "new"))),
            vec![change("cpu", "new"), change("firmware", "bios")]
        );
        assert!(with_cpu_pending(&[], None).is_empty());
    }
}
