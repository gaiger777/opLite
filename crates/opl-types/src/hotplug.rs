//! VM 무중단 유형 변경(계획 54)의 순수 계산: 스펙과 도는 하드웨어([`RunningHardware`])를 견주어 실행 중에 바로 할
//! 단계와 다음 재시작 때 적용할 변경을 고른다.

use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::cloud_init;
use crate::cpu::ResolvedCpu;
use crate::node::{HOTPLUG, Node, NodeHotplug};
use crate::runtime::{HOTPLUG_PORTS, HotplugLayout};
use crate::vm::{
    CpuSpec, DiskBus, NicSpec, PendingChange, PendingReason, RunningHardware, Vm, VmSpec,
};

/// 핫플러그 Task가 차례로 하는 일. 순서는 떼기(NIC → 디스크, 큰 순번부터) → vCPU → 메모리 → CD-ROM 매체 →
/// 붙이기(디스크 → NIC, 작은 순번부터)라 떼어 빈 루트 포트를 붙이기가 다시 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    RemoveNic(usize),
    RemoveDisk(usize),
    SetVcpus(u16),
    /// virtio-mem에 꽂을 크기(켤 때 메모리 위로 더한 것)
    SetMemory {
        plugged_mib: u32,
    },
    /// CD-ROM 드라이브 `index`의 매체를 스펙의 것으로 바꾼다(넣기·빼기). 루트 포트를 쓰지 않으므로 핫플러그 배치
    /// 없이 켠 VM도 된다.
    ChangeMedium {
        index: usize,
    },
    AddDisk {
        index: usize,
        port: u8,
    },
    AddNic {
        index: usize,
        port: u8,
    },
}

/// 스펙에 맞추는 계획: 바로 할 단계와 다음 재시작 때 적용할 변경
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub pending: Vec<PendingChange>,
}

/// 실행 중에 할 수 없는 디스크·NIC 변경. 디스크·NIC는 보류가 되지 않고 API가 409로 거절한다.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeviceError {
    /// 끝에 붙이기·실행 중 붙인 것을 끝에서 떼기가 아닌 변경(예: `remove disk "root"`)
    #[error("cannot {0} while the VM is running")]
    Changed(String),
    /// SATA 디스크 붙이기(루트 포트에는 virtio-blk만 붙인다)
    #[error("SATA disk {0:?} cannot be added while the VM is running")]
    SataDisk(String),
    /// 핫플러그 배치 없이 켠 VM
    #[error("the VM was started without hotplug support")]
    NoLayout,
    /// 빈 루트 포트가 없다.
    #[error("no free hotplug port")]
    NoFreePort,
}

/// 보류 이유의 설명
const NO_LAYOUT: &str = "the VM was started without hotplug support";
const NO_CPU_HOTPLUG: &str = "this node cannot hot-plug vCPUs";
const NO_VIRTIO_MEM: &str = "this node has no virtio-mem";
const NO_PREALLOC: &str =
    "this node's QEMU cannot preallocate hugepages for virtio-mem (needs QEMU 7.1)";

/// 이 노드(능력 `caps`)가 VM을 메모리 핫플러그(virtio-mem)로 켜는지: 상한이 있고 노드에 virtio-mem이 있으며,
/// hugepages VM이면 장치가 꽂을 때 미리 잡기(`prealloc`, QEMU 7.1+)를 한다(계획 64 리뷰 2 — 미리 잡지 않으면 모자란
/// hugepage에 게스트가 SIGBUS로 죽는다).
pub fn memory_hotplug(spec: &VmSpec, caps: NodeHotplug) -> bool {
    spec.max_memory_mib.is_some() && memory_caps(spec, caps)
}

/// 노드가 이 VM의 메모리를 실행 중에 꽂을 수 있는지(상한은 보지 않음)
fn memory_caps(spec: &VmSpec, caps: NodeHotplug) -> bool {
    caps.memory && (spec.hugepages.is_none() || caps.memory_prealloc)
}

/// 이 노드에서 켤 때의 스펙: 메모리 핫플러그 없이 켜면([`memory_hotplug`]가 거짓) 상한이 메모리다(늘릴 자리가 없다 —
/// 없애지 않는 것은 노드 목록의 검증이 상한 있는 VM의 늘린 메모리를 받기 때문). NUMA 맞춤(`any`의 페이지 크기)·장부가
/// 노드에서 실제로 켜질 모양을 보게 한다(리뷰 2 N9).
pub fn start_spec(spec: &VmSpec, caps: NodeHotplug) -> std::borrow::Cow<'_, VmSpec> {
    if spec.max_memory_mib.is_none() || memory_hotplug(spec, caps) {
        std::borrow::Cow::Borrowed(spec)
    } else {
        let mut s = spec.clone();
        s.max_memory_mib = Some(s.memory_mib);
        std::borrow::Cow::Owned(s)
    }
}

impl RunningHardware {
    /// 핫플러그 배치 없이 켠 VM의 기록(계획 54 전에 켰거나 클러스터가 업그레이드 중일 때).
    pub fn legacy(spec: &VmSpec) -> Self {
        RunningHardware {
            host_placement: None,
            numa: None,
            cpu: CpuSpec {
                max_vcpus: None,
                ..spec.cpu.clone()
            },
            memory_mib: spec.memory_mib,
            firmware: spec.firmware,
            machine: spec.machine,
            layout: None,
            disks: spec.disks.clone(),
            nics: spec.nics.clone(),
            cdroms: spec.cdroms.clone(),
            cloud_init: None,
            unplugging: Vec::new(),
            pending: Vec::new(),
            tpm: spec.tpm,
            secure_boot: spec.secure_boot,
            guest_agent: spec.agent_channel(),
            cpu_resolved: None,
        }
    }

    /// VM `id`(이름 `name`)를 켤 때의 기록과 QEMU에 줄 배치. `enabled`(모든 노드가 `hotplug`)가 아니면 배치가 없다.
    /// 노드가 CPU 핫플러그를 못 하면 `max_vcpus`를, virtio-mem이 없으면 메모리 상한을 두지 않는다. cloud-init이
    /// 있으면 넣을 씨앗의 instance-id를 적는다.
    pub fn at_start(
        id: Uuid,
        name: &str,
        spec: &VmSpec,
        caps: NodeHotplug,
        enabled: bool,
    ) -> (Self, Option<HotplugLayout>) {
        let mut hw = Self::legacy(spec);
        hw.cloud_init = seed_id(id, name, spec);
        if !enabled {
            return (hw, None);
        }
        if caps.cpu {
            hw.cpu.max_vcpus = spec.cpu.max_vcpus;
        }
        let layout = HotplugLayout {
            vcpus: u16::try_from(spec.cpu.vcpus()).unwrap_or(u16::MAX),
            max_memory_mib: match spec.max_memory_mib {
                Some(max) if memory_hotplug(spec, caps) => max,
                _ => spec.memory_mib,
            },
            plugged_mib: 0,
            ports: HOTPLUG_PORTS,
            boot_disks: u8::try_from(spec.disks.len()).unwrap_or(u8::MAX),
            boot_nics: u8::try_from(spec.nics.len()).unwrap_or(u8::MAX),
            disk_ports: Vec::new(),
            nic_ports: Vec::new(),
        };
        hw.layout = Some(layout.clone());
        (hw, Some(layout))
    }

    /// 켤 때 단 NIC(기록의 앞 `boot_nics`개, 배치가 없으면 모두). 켤 때 단 NIC는 실행 중에 떼거나 바꾸지 못하므로
    /// cloud-init 씨앗을 만들 때 쓴 NIC 입력(MAC)이다.
    pub fn boot_nics(&self) -> &[NicSpec] {
        let boot = self
            .layout
            .as_ref()
            .map_or(self.nics.len(), |l| usize::from(l.boot_nics));
        &self.nics[..boot.min(self.nics.len())]
    }

    /// 켤 때의 vCPU 수
    pub fn boot_vcpus(&self) -> u32 {
        self.cpu.vcpus()
    }

    /// 지금 켜진 vCPU 수
    pub fn vcpus(&self) -> u32 {
        self.layout
            .as_ref()
            .map_or(self.boot_vcpus(), |l| u32::from(l.vcpus))
    }

    /// 지금 메모리(켤 때 메모리 + virtio-mem에 꽂은 것, MiB)
    pub fn memory_now(&self) -> u32 {
        self.memory_mib + self.layout.as_ref().map_or(0, |l| l.plugged_mib)
    }

    /// 메모리 상한(MiB). virtio-mem이 없으면 켤 때 메모리다.
    pub fn max_memory_mib(&self) -> u32 {
        self.layout
            .as_ref()
            .map_or(self.memory_mib, |l| l.max_memory_mib)
    }

    /// 켤 때 모양 그대로인지: 실행 중에 바꾼 vCPU·메모리·장치가 없고 보류도 없다. 메모리 스냅샷은 이때만 담는다
    /// (되돌리기가 스냅샷의 스펙으로 새로 켜서 같은 장치여야 한다).
    pub fn matches_boot(&self) -> bool {
        self.pending.is_empty()
            && self.layout.as_ref().is_none_or(|l| {
                u32::from(l.vcpus) == self.boot_vcpus()
                    && l.plugged_mib == 0
                    && l.disk_ports.is_empty()
                    && l.nic_ports.is_empty()
            })
    }
}

/// VM `id`(이름 `name`)를 스펙 `spec`에 맞추는 계획. 이 노드의 능력은 `caps`다. 디스크·NIC를 실행 중에 바꿀 수
/// 없으면 오류다.
pub fn plan(
    id: Uuid,
    name: &str,
    spec: &VmSpec,
    hw: &RunningHardware,
    caps: NodeHotplug,
) -> Result<Plan, DeviceError> {
    let layout = hw.layout.as_ref();
    let disk_boot = layout.map_or(hw.disks.len(), |l| usize::from(l.boot_disks));
    let nic_boot = layout.map_or(hw.nics.len(), |l| usize::from(l.boot_nics));
    // 게스트가 아직 내려 주지 않은 장치(`unplugging`)는 원하는 장치와 같지 않은 것으로 본다: 그 자리의 떼기를 먼저
    // 마저 하고, 스펙에 있으면 다시 붙인다.
    let held = |id: String| hw.unplugging.contains(&id);
    let disks = device_changes(
        &hw.disks,
        &spec.disks,
        disk_boot,
        "disks",
        |a, b| a == b,
        |a, b| a.volume == b.volume,
        |_, d| format!("disk {:?}", d.volume),
        |i| held(format!("disk{i}-dev")),
    )?;
    let nics = device_changes(
        &hw.nics,
        &spec.nics,
        nic_boot,
        "NICs",
        same_nic,
        |a, b| a.mac == b.mac,
        |i, _| format!("NIC {i}"),
        |i| held(format!("nic{i}")),
    )?;
    if let Some(sata) = disks
        .added
        .iter()
        .map(|&i| &spec.disks[i])
        .find(|d| d.bus == DiskBus::Sata)
    {
        return Err(DeviceError::SataDisk(sata.volume.clone()));
    }
    // CD-ROM 드라이브(컨트롤러 포트)는 켤 때만 정해지고, 매체만 실행 중에 바꾼다.
    if spec.cdroms.len() != hw.cdroms.len() {
        return Err(DeviceError::Changed("add or remove CD-ROM drives".into()));
    }

    let mut steps: Vec<Step> = nics.removed.iter().map(|&i| Step::RemoveNic(i)).collect();
    steps.extend(disks.removed.iter().map(|&i| Step::RemoveDisk(i)));
    let mut pending = Vec::new();
    changes(id, name, spec, hw, caps, None, &mut steps, &mut pending);
    steps.extend(
        spec.cdroms
            .iter()
            .zip(&hw.cdroms)
            .enumerate()
            .filter(|(_, (want, now))| want.iso != now.iso)
            .map(|(index, _)| Step::ChangeMedium { index }),
    );

    if disks.is_empty() && nics.is_empty() {
        return Ok(Plan { steps, pending });
    }
    let Some(layout) = layout else {
        return Err(DeviceError::NoLayout);
    };
    // 떼는 장치의 포트는 비고, 붙이는 장치는 가장 작은 빈 포트부터 쓴다.
    let port_of = |ports: &[u8], boot: usize, i: usize| ports.get(i.wrapping_sub(boot)).copied();
    let mut used: BTreeSet<u8> = layout
        .disk_ports
        .iter()
        .chain(&layout.nic_ports)
        .copied()
        .collect();
    for &i in &disks.removed {
        if let Some(port) = port_of(&layout.disk_ports, disk_boot, i) {
            used.remove(&port);
        }
    }
    for &i in &nics.removed {
        if let Some(port) = port_of(&layout.nic_ports, nic_boot, i) {
            used.remove(&port);
        }
    }
    let mut free = (0..layout.ports).filter(|p| !used.contains(p));
    for &index in &disks.added {
        let port = free.next().ok_or(DeviceError::NoFreePort)?;
        steps.push(Step::AddDisk { index, port });
    }
    for &index in &nics.added {
        let port = free.next().ok_or(DeviceError::NoFreePort)?;
        steps.push(Step::AddNic { index, port });
    }
    Ok(Plan { steps, pending })
}

/// 디스크·NIC는 견주지 않고 vCPU·메모리·펌웨어·머신만 맞추는 계획(바로 할 단계와 보류). 디스크·NIC를 맞출 수 없을
/// 때도 남은 변경을 셀 수 있다.
pub fn plan_resources(
    id: Uuid,
    name: &str,
    spec: &VmSpec,
    hw: &RunningHardware,
    caps: NodeHotplug,
) -> Plan {
    let mut plan = Plan::default();
    changes(
        id,
        name,
        spec,
        hw,
        caps,
        None,
        &mut plan.steps,
        &mut plan.pending,
    );
    plan
}

/// 기록의 QEMU 장치 id(`disk{i}-dev`, `nic{i}`)를 사람이 읽을 이름으로: `disk "data"`, `NIC 1 on network "lan"`.
/// 기록에 없는 장치는 id 그대로다.
pub fn device_name(hw: &RunningHardware, device: &str) -> String {
    let disk = device
        .strip_prefix("disk")
        .and_then(|d| d.strip_suffix("-dev"))
        .and_then(|i| i.parse::<usize>().ok())
        .and_then(|i| hw.disks.get(i));
    let nic = device
        .strip_prefix("nic")
        .and_then(|i| i.parse::<usize>().ok())
        .and_then(|i| hw.nics.get(i).map(|n| (i, n)));
    match (disk, nic) {
        (Some(d), _) => format!("disk {:?}", d.volume),
        (_, Some((i, n))) => format!("NIC {i} on network {:?}", n.network),
        _ => device.to_string(),
    }
}

/// 핫플러그 Task 끝에 쓸 보류 목록: 지금 셈한 보류에 실패한 항목(`failed`)을 같은 `field` 자리에 덮어 더한다.
/// `wanted`는 VM이 도는 노드에서 지금 스펙을 해석한 CPU다(계획 63, 기준·숨김 목록). 있으면 그것이 켤 때 해석과 다를 때의
/// 보류 `cpu`도 남긴다(없으면 검사 루프가 쓴 보류를 지운다).
pub fn pending(
    id: Uuid,
    name: &str,
    spec: &VmSpec,
    hw: &RunningHardware,
    caps: NodeHotplug,
    failed: &[PendingChange],
    wanted: Option<&ResolvedCpu>,
) -> Vec<PendingChange> {
    let mut steps = Vec::new();
    let mut out = Vec::new();
    changes(id, name, spec, hw, caps, wanted, &mut steps, &mut out);
    for f in failed {
        match out.iter_mut().find(|p| p.field == f.field) {
            Some(p) => *p = f.clone(),
            None => out.push(f.clone()),
        }
    }
    out
}

/// 노드 `node`가 이 하드웨어로 도는 VM을 마이그레이션으로 받을 수 있는지. 받는 쪽은 원본과 같은 장치로 켜야 한다.
/// `hw`가 없으면(옛 노드가 켠 VM) 지금처럼 스펙으로 켜므로 된다.
pub fn can_receive(hw: Option<&RunningHardware>, node: &Node) -> Result<(), String> {
    let Some(hw) = hw else {
        return Ok(());
    };
    // 게스트가 아직 내려 주지 않은 장치는 받는 쪽이 켤 수 없다(기록에도 스펙에도 맞지 않는다).
    if !hw.unplugging.is_empty() {
        return Err(format!(
            "the guest has not released {} yet; run `vm hotplug` again",
            hw.unplugging.join(", ")
        ));
    }
    let Some(layout) = &hw.layout else {
        return Ok(());
    };
    if !node.status.features.iter().any(|f| f == HOTPLUG) {
        return Err(format!(
            "node {:?} does not support VMs started with hotplug support yet",
            node.name
        ));
    }
    let caps = node.status.hotplug.unwrap_or_default();
    let boot = hw.boot_vcpus();
    if !caps.cpu && (hw.cpu.max() > boot || u32::from(layout.vcpus) > boot) {
        return Err(format!("node {:?} cannot hot-plug vCPUs", node.name));
    }
    if !caps.memory && layout.max_memory_mib > hw.memory_mib {
        return Err(format!("node {:?} has no virtio-mem", node.name));
    }
    if !caps.memory_prealloc
        && layout.max_memory_mib > hw.memory_mib
        && hw.numa.as_ref().is_some_and(|n| n.page.is_some())
    {
        return Err(format!(
            "node {:?} cannot preallocate hugepages for virtio-mem (needs QEMU 7.1)",
            node.name
        ));
    }
    Ok(())
}

/// VM이 노드에서 차지한다고 셀 메모리(MiB): 스펙의 값과 지금 도는 메모리 가운데 큰 것(줄이기가 실패해 도는 메모리가
/// 스펙보다 클 수 있다).
pub fn allocated_memory_mib(vm: &Vm) -> u32 {
    vm.status
        .hardware
        .as_ref()
        .map_or(vm.spec.memory_mib, |hw| {
            vm.spec.memory_mib.max(hw.memory_now())
        })
}

/// 스펙의 디스크·NIC가 도는 QEMU의 장치(`hw`)와 같은지(NIC의 보안 그룹은 견주지 않는다). 다르면 핫플러그 Task가
/// 끝나지 않았거나 실패해 게스트가 아직 내려 주지 않은 장치가 있다 — 마이그레이션 대상은 스펙의 장치로 켜므로
/// 옮기면 안 된다.
/// CD-ROM 드라이브는 수와 매체를 견준다(부팅 순서는 다음 재시작 때 적용할 보류일 뿐이고, 받는 쪽은 기록의 값으로
/// 켠다). cloud-init 씨앗은 드라이브가 있는지만 견준다(내용은 받는 쪽이 스펙으로 다시 만든다). TPM·Secure Boot·
/// 게스트 에이전트 채널(계획 59)은 있는지를 견준다.
pub fn devices_match(spec: &VmSpec, hw: &RunningHardware) -> bool {
    spec.disks == hw.disks
        && spec.nics.len() == hw.nics.len()
        && spec.nics.iter().zip(&hw.nics).all(|(a, b)| same_nic(a, b))
        && spec.cdroms.len() == hw.cdroms.len()
        && spec
            .cdroms
            .iter()
            .zip(&hw.cdroms)
            .all(|(a, b)| a.iso == b.iso)
        && spec.cloud_init.is_some() == hw.cloud_init.is_some()
        && spec.tpm == hw.tpm
        && spec.secure_boot == hw.secure_boot
        && !agent_channel_differs(spec, hw)
}

/// 스펙의 게스트 에이전트 채널이 도는 QEMU와 다른지. 계획 59 전 노드의 기록에는 `guest_agent`가 없으므로, 스펙에
/// `guest_agent`가 없고 기록도 거짓이면 `ha.guest_watch`(실행 중에 바꿀 수 없다)가 정한 채널 그대로라 같다고 본다.
fn agent_channel_differs(spec: &VmSpec, hw: &RunningHardware) -> bool {
    spec.agent_channel() != hw.guest_agent && (spec.guest_agent.is_some() || hw.guest_agent)
}

/// 마이그레이션 대상이 켤 QEMU의 핫플러그·디스크·NIC·CD-ROM 장치 id(`/machine/peripheral`의 `disk{i}-dev`·
/// `nic{i}`·`cpu{i}`·`vmem0-dev`·`cd{i}-dev`·`cidata-dev`, 이름순): 디스크·NIC는 스펙에서, 붙인 vCPU와 virtio-mem은
/// 하드웨어 기록의 배치에서, CD-ROM 드라이브와 cloud-init 씨앗 드라이브는 기록(없으면 스펙)에서 나온다. 원본
/// 드라이버가 도는 QEMU와 견준다.
pub fn qemu_devices(spec: &VmSpec, hw: Option<&RunningHardware>) -> Vec<String> {
    let cdroms = hw.map_or(spec.cdroms.len(), |hw| hw.cdroms.len());
    let seed = hw.map_or(spec.cloud_init.is_some(), |hw| hw.cloud_init.is_some());
    let mut ids: Vec<String> = (0..spec.disks.len())
        .map(|i| format!("disk{i}-dev"))
        .chain((0..spec.nics.len()).map(|i| format!("nic{i}")))
        .chain((0..cdroms).map(|i| format!("cd{i}-dev")))
        .chain(seed.then(|| "cidata-dev".to_string()))
        .collect();
    if let Some(hw) = hw
        && let Some(layout) = &hw.layout
    {
        ids.extend((hw.boot_vcpus()..u32::from(layout.vcpus)).map(|i| format!("cpu{i}")));
        if layout.max_memory_mib > hw.memory_mib {
            // vNUMA VM(계획 64)은 게스트 노드마다 장치가 있다.
            let nodes = hw.numa.as_ref().map_or(1, |n| n.nodes.len().max(1));
            ids.extend((0..nodes).map(|i| format!("vmem{i}-dev")));
        }
    }
    ids.sort();
    ids
}

/// 스펙의 하드웨어가 바뀌었는지(CPU·메모리·상한·펌웨어·머신·디스크·보안 그룹을 뺀 NIC·CD-ROM 드라이브와 매체·
/// cloud-init·TPM·Secure Boot·게스트 에이전트 채널·vNUMA·hugepages). 실행 중이면 핫플러그 Task를 만든다. 에이전트의 `freeze_fs`·
/// `shutdown`은 장치가 아니라서 견주지 않는다.
pub fn hardware_changed(old: &VmSpec, new: &VmSpec) -> bool {
    old.cpu != new.cpu
        || old.memory_mib != new.memory_mib
        || old.max_memory_mib != new.max_memory_mib
        || old.firmware != new.firmware
        || old.machine != new.machine
        || old.disks != new.disks
        || old.nics.len() != new.nics.len()
        || old.nics.iter().zip(&new.nics).any(|(a, b)| !same_nic(a, b))
        || old.cdroms != new.cdroms
        || old.cloud_init != new.cloud_init
        || old.tpm != new.tpm
        || old.secure_boot != new.secure_boot
        || old.agent_channel() != new.agent_channel()
        || old.numa != new.numa
        || old.hugepages != new.hugepages
}

/// 같은 NIC 장치인지(보안 그룹은 방화벽 규칙이라 견주지 않는다)
fn same_nic(a: &NicSpec, b: &NicSpec) -> bool {
    a.network == b.network && a.model == b.model && a.mac == b.mac
}

/// 디스크나 NIC 목록의 변경: 뗄 순번(큰 것부터)과 붙일 순번(작은 것부터)
struct DeviceChanges {
    removed: Vec<usize>,
    added: Vec<usize>,
}

impl DeviceChanges {
    fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty()
    }
}

/// 도는 장치 `running`을 `wanted`로 바꾸는 떼기·붙이기. 같은 앞부분 뒤의 도는 장치는 모두 떼어야 하므로 실행 중
/// 붙인 것(순번 ≥ `boot`)이어야 하고 `wanted`에 다시 나타나면 안 된다(바뀜·순서 바꾸기·가운데 떼기). 떼는 중인 장치
/// (`held`)는 같은 앞부분이 아니고 늘 뗀다(`wanted`에 있으면 다시 붙인다).
#[allow(clippy::too_many_arguments)]
fn device_changes<T>(
    running: &[T],
    wanted: &[T],
    boot: usize,
    kinds: &str,
    same: impl Fn(&T, &T) -> bool,
    same_id: impl Fn(&T, &T) -> bool,
    name: impl Fn(usize, &T) -> String,
    held: impl Fn(usize) -> bool,
) -> Result<DeviceChanges, DeviceError> {
    let common = running
        .iter()
        .zip(wanted)
        .enumerate()
        .take_while(|(i, (r, w))| same(r, w) && !held(*i))
        .count();
    for (i, r) in running.iter().enumerate().skip(common) {
        if held(i) {
            continue;
        }
        let what = if let Some(pos) = wanted.iter().position(|w| same_id(r, w)) {
            if pos == i {
                format!("change {}", name(i, r))
            } else {
                format!("reorder the {kinds}")
            }
        } else if i < boot {
            if i < wanted.len() {
                format!("replace {}", name(i, r))
            } else {
                format!("remove {}", name(i, r))
            }
        } else if running[i + 1..]
            .iter()
            .any(|later| wanted.iter().any(|w| same_id(later, w)))
        {
            format!("remove {} from the middle", name(i, r))
        } else {
            continue;
        };
        return Err(DeviceError::Changed(what));
    }
    Ok(DeviceChanges {
        removed: (common..running.len()).rev().collect(),
        added: (common..wanted.len()).collect(),
    })
}

/// vCPU·메모리·펌웨어·머신·CD-ROM 부팅 순서·cloud-init: 바로 할 단계는 `steps`에, 못 하는 것은 `pending`에 더한다.
#[allow(clippy::too_many_arguments)]
fn changes(
    id: Uuid,
    name: &str,
    spec: &VmSpec,
    hw: &RunningHardware,
    caps: NodeHotplug,
    wanted: Option<&ResolvedCpu>,
    steps: &mut Vec<Step>,
    pending: &mut Vec<PendingChange>,
) {
    cpu_changes(spec, hw, caps, wanted, steps, pending);
    // CPU 정책·vNUMA·hugepages(계획 64)는 켤 때만 정해진다.
    pending.extend(crate::numa::pending_change(spec, hw));
    memory_changes(spec, hw, caps, steps, pending);
    if spec.firmware != hw.firmware {
        pending.push(change(
            "firmware",
            kebab(hw.firmware),
            kebab(spec.firmware),
            PendingReason::Restart,
            None,
        ));
    }
    if spec.machine != hw.machine {
        pending.push(change(
            "machine",
            kebab(hw.machine),
            kebab(spec.machine),
            PendingReason::Restart,
            None,
        ));
    }
    // CD-ROM 부팅 순서는 QEMU를 켤 때만 정해진다(`bootindex`).
    let order = |o: Option<u8>| o.map_or("none".to_string(), |o| o.to_string());
    for (i, (want, now)) in spec.cdroms.iter().zip(&hw.cdroms).enumerate() {
        if want.boot_order != now.boot_order {
            pending.push(change(
                &format!("cdroms[{i}].boot_order"),
                order(now.boot_order),
                order(want.boot_order),
                PendingReason::Restart,
                None,
            ));
        }
    }
    // cloud-init 씨앗은 QEMU를 켤 때 스펙으로 만든다(instance-id = 내용 해시).
    let seed = seed_id(id, name, spec);
    if seed != hw.cloud_init {
        let id = |s: Option<String>| s.unwrap_or_else(|| "none".into());
        pending.push(change(
            "cloud_init",
            id(hw.cloud_init.clone()),
            id(seed),
            PendingReason::Restart,
            None,
        ));
    }
    // TPM·Secure Boot·게스트 에이전트 채널(계획 59)은 QEMU를 켤 때만 단다.
    let on = |b: bool| if b { "on" } else { "off" }.to_string();
    if spec.tpm != hw.tpm {
        pending.push(change(
            "tpm",
            on(hw.tpm),
            on(spec.tpm),
            PendingReason::Restart,
            None,
        ));
    }
    if spec.secure_boot != hw.secure_boot {
        pending.push(change(
            "secure_boot",
            on(hw.secure_boot),
            on(spec.secure_boot),
            PendingReason::Restart,
            None,
        ));
    }
    if agent_channel_differs(spec, hw) {
        pending.push(change(
            "guest_agent",
            on(hw.guest_agent),
            on(spec.agent_channel()),
            PendingReason::Restart,
            None,
        ));
    }
}

/// 스펙 `spec`의 cloud-init으로 만들 씨앗이 도는 QEMU의 씨앗(기록의 instance-id)과 같은지. 마이그레이션 대상은
/// 씨앗을 스펙의 cloud-init과 켤 때 단 NIC([`RunningHardware::boot_nics`])로 다시 만들므로, 실행 중 cloud-init을
/// 바꿨거나 호스트 이름 없는 VM의 이름을 바꿨으면 게스트가 다른 씨앗을 보게 된다. 실행 중 붙인 NIC는 씨앗을 바꾸지
/// 않는다(다음 재시작 때 들어감).
pub fn seed_matches(id: Uuid, name: &str, spec: &VmSpec, hw: &RunningHardware) -> bool {
    spec.cloud_init
        .as_ref()
        .map(|ci| cloud_init::render(id, name, ci, hw.boot_nics()).instance_id)
        == hw.cloud_init
}

/// VM `id`(이름 `name`)를 스펙 `spec`으로 켜면 넣을 cloud-init 씨앗의 instance-id. cloud-init이 없으면 없다.
fn seed_id(id: Uuid, name: &str, spec: &VmSpec) -> Option<String> {
    spec.cloud_init
        .as_ref()
        .map(|ci| cloud_init::render(id, name, ci, &spec.nics).instance_id)
}

/// vCPU: 바로 = 배치 있음·노드가 CPU 핫플러그를 함·토폴로지·모델·상한이 켤 때와 같음·켤 때 수 ≤ 새 수 ≤ 상한.
/// 토폴로지·모델이 바뀌면 `cpu` 보류 하나로 끝낸다. 상한은 노드가 CPU 핫플러그를 할 때만 견준다(못 하면 켜도 상한이
/// 없다).
fn cpu_changes(
    spec: &VmSpec,
    hw: &RunningHardware,
    caps: NodeHotplug,
    wanted: Option<&ResolvedCpu>,
    steps: &mut Vec<Step>,
    pending: &mut Vec<PendingChange>,
) {
    // 토폴로지·모델·플래그(계획 63)는 켤 때만 정해진다 — vCPU를 따로 보지 않는다.
    if let Some(p) = crate::cpu::pending_change(&spec.cpu, hw, None) {
        pending.push(p);
        return;
    }
    // 해석(기준·숨김 목록)만 바뀌었으면 보류를 두되 vCPU는 그대로 본다(리뷰 1 M1 — 늘린 vCPU의 문제를 가리지 않게).
    if wanted.is_some()
        && let Some(p) = crate::cpu::pending_change(&spec.cpu, hw, wanted)
    {
        pending.push(p);
    }
    let boot = hw.boot_vcpus();
    let now = hw.vcpus();
    let want = spec.cpu.vcpus();
    let max = hw.cpu.max();
    let max_changed =
        caps.cpu && spec.cpu.max() != max && (spec.cpu.max_vcpus.is_some() || max > boot);
    if max_changed {
        pending.push(change(
            "cpu.max_vcpus",
            max.to_string(),
            spec.cpu.max().to_string(),
            PendingReason::Restart,
            None,
        ));
    }
    // 게스트가 아직 내려 주지 않은 vCPU가 있으면 같은 수라도 다시 맞춘다(드라이버가 빠진 자리를 다시 붙이거나 떼기를
    // 마저 한다).
    let held = hw.unplugging.iter().any(|d| d.starts_with("cpu"));
    if want == now && !held {
        return;
    }
    let why = if hw.layout.is_none() {
        Some((PendingReason::Unsupported, Some(NO_LAYOUT.to_string())))
    } else if !caps.cpu {
        Some((PendingReason::Unsupported, Some(NO_CPU_HOTPLUG.to_string())))
    } else if want < boot || want > max {
        let detail = if max == boot {
            "the VM was started without a vCPU maximum".to_string()
        } else {
            format!("vCPUs can change between {boot} and {max} while it runs")
        };
        Some((PendingReason::Limit, Some(detail)))
    } else if max_changed {
        Some((PendingReason::Restart, None))
    } else {
        None
    };
    match why {
        Some((reason, detail)) => pending.push(change(
            "vcpus",
            now.to_string(),
            want.to_string(),
            reason,
            detail,
        )),
        // 상한(≤ 256) 안이라 u16에 든다.
        None => steps.push(Step::SetVcpus(want as u16)),
    }
}

/// 메모리: 바로 = 배치에 virtio-mem이 있음·상한이 켤 때와 같음·켤 때 메모리 ≤ 새 값 ≤ 상한·2 MiB 단위. 상한은
/// 노드에 virtio-mem이 있을 때만 견준다.
fn memory_changes(
    spec: &VmSpec,
    hw: &RunningHardware,
    caps: NodeHotplug,
    steps: &mut Vec<Step>,
    pending: &mut Vec<PendingChange>,
) {
    let now = hw.memory_now();
    let want = spec.memory_mib;
    let max = hw.max_memory_mib();
    let virtio = max > hw.memory_mib;
    let wanted_max = spec.max_memory_mib.unwrap_or(want);
    let mem_caps = memory_caps(spec, caps);
    let max_changed = mem_caps && wanted_max != max && (spec.max_memory_mib.is_some() || virtio);
    if max_changed {
        pending.push(change(
            "max_memory_mib",
            max.to_string(),
            wanted_max.to_string(),
            PendingReason::Restart,
            None,
        ));
    }
    if want == now {
        return;
    }
    let why = if hw.layout.is_none() {
        Some((PendingReason::Unsupported, Some(NO_LAYOUT.to_string())))
    } else if !virtio {
        if !caps.memory && spec.max_memory_mib.is_some() {
            Some((PendingReason::Unsupported, Some(NO_VIRTIO_MEM.to_string())))
        } else if !mem_caps && spec.max_memory_mib.is_some() {
            Some((PendingReason::Unsupported, Some(NO_PREALLOC.to_string())))
        } else {
            Some((
                PendingReason::Limit,
                Some("the VM was started without a memory maximum".to_string()),
            ))
        }
    } else if want < hw.memory_mib || want > max || !(want - hw.memory_mib).is_multiple_of(2) {
        Some((
            PendingReason::Limit,
            Some(format!(
                "memory can change between {} and {max} MiB while it runs",
                hw.memory_mib
            )),
        ))
    } else if max_changed {
        Some((PendingReason::Restart, None))
    } else {
        None
    };
    match why {
        Some((reason, detail)) => pending.push(change(
            "memory_mib",
            now.to_string(),
            want.to_string(),
            reason,
            detail,
        )),
        None => steps.push(Step::SetMemory {
            plugged_mib: want - hw.memory_mib,
        }),
    }
}

fn change(
    field: &str,
    running: String,
    wanted: String,
    reason: PendingReason,
    detail: Option<String>,
) -> PendingChange {
    PendingChange {
        field: field.to_string(),
        running,
        wanted,
        reason,
        detail,
    }
}

/// 열거형의 JSON 이름(`uefi`, `q35` …)
fn kebab(v: impl Serialize) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_init::CloudInit;
    use crate::node::{NodeSpec, NodeStatus};
    use crate::resource::Resource;
    use crate::vm::{CdromSpec, CpuModel, DiskSpec, Firmware, MacAddr, MachineType, NicModel};

    /// 시험 VM의 id·이름("web")
    const VM: Uuid = Uuid::nil();

    const ALL: NodeHotplug = NodeHotplug {
        cpu: true,
        memory: true,
        memory_prealloc: true,
    };

    fn disk(volume: &str) -> DiskSpec {
        DiskSpec {
            volume: volume.into(),
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        }
    }

    fn nic(last: u8) -> NicSpec {
        NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac: Some(MacAddr([0x52, 0x54, 0, 0, 0, last])),
            security_groups: vec![],
        }
    }

    /// 1소켓 2코어(상한 8), 메모리 4096(상한 16384), 디스크 하나·NIC 하나
    fn spec() -> VmSpec {
        serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 2, "max_vcpus": 8 },
            "memory_mib": 4096,
            "max_memory_mib": 16384,
            "machine": "q35",
            "disks": [ { "volume": "root", "bus": "virtio-blk", "boot_order": 1 } ],
            "nics": [ { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01" } ],
        }))
        .unwrap()
    }

    fn started(spec: &VmSpec, caps: NodeHotplug) -> RunningHardware {
        RunningHardware::at_start(VM, "web", spec, caps, true).0
    }

    fn layout(hw: &mut RunningHardware) -> &mut HotplugLayout {
        hw.layout.as_mut().unwrap()
    }

    fn only_pending(plan: Plan) -> Vec<PendingChange> {
        assert!(plan.steps.is_empty(), "{plan:?}");
        plan.pending
    }

    fn reasons(pending: &[PendingChange]) -> Vec<(&str, PendingReason)> {
        pending
            .iter()
            .map(|p| (p.field.as_str(), p.reason))
            .collect()
    }

    #[test]
    fn at_start_records_the_layout_and_respects_node_caps() {
        let s = spec();
        let (hw, layout) = RunningHardware::at_start(VM, "web", &s, ALL, true);
        let layout = layout.unwrap();
        assert_eq!(hw.layout.as_ref(), Some(&layout));
        assert_eq!(layout.vcpus, 2);
        assert_eq!(layout.max_memory_mib, 16384);
        assert_eq!(layout.plugged_mib, 0);
        assert_eq!(layout.ports, HOTPLUG_PORTS);
        assert_eq!((layout.boot_disks, layout.boot_nics), (1, 1));
        assert_eq!(hw.cpu.max_vcpus, Some(8));
        assert_eq!(
            (hw.memory_mib, hw.firmware, hw.machine),
            (4096, Firmware::Uefi, MachineType::Q35)
        );
        assert_eq!(
            (hw.disks.clone(), hw.nics.clone()),
            (s.disks.clone(), s.nics.clone())
        );
        assert!(hw.matches_boot());

        // CPU 핫플러그·virtio-mem이 없는 노드는 상한을 두지 않는다.
        let (hw, layout) = RunningHardware::at_start(VM, "web", &s, NodeHotplug::default(), true);
        assert_eq!(hw.cpu.max_vcpus, None);
        assert_eq!(layout.unwrap().max_memory_mib, 4096);

        // 클러스터가 업그레이드 중이면 배치가 없다.
        let (hw, layout) = RunningHardware::at_start(VM, "web", &s, ALL, false);
        assert_eq!(layout, None);
        assert_eq!(hw, RunningHardware::legacy(&s));
        assert_eq!(hw.cpu.max_vcpus, None);
        assert_eq!(
            (hw.vcpus(), hw.memory_now(), hw.max_memory_mib()),
            (2, 4096, 4096)
        );
    }

    #[test]
    fn vcpus_grow_within_the_maximum() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.cpu.cores = 4;
        let first = plan(VM, "web", &s, &hw, ALL).unwrap();
        assert_eq!(first.steps, [Step::SetVcpus(4)]);
        assert!(first.pending.is_empty());

        // 실행 중 늘린 뒤 다시 줄이기(켤 때 수까지)
        let mut hw = hw;
        layout(&mut hw).vcpus = 6;
        assert_eq!(hw.vcpus(), 6);
        s.cpu.cores = 2;
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::SetVcpus(2)]
        );
    }

    #[test]
    fn vcpus_outside_the_running_range_wait_for_restart() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        // 상한도 함께 올렸다: 상한은 재시작, vCPU는 지금 상한 밖이다.
        s.cpu.cores = 9;
        s.cpu.max_vcpus = Some(16);
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(
            reasons(&pending),
            [
                ("cpu.max_vcpus", PendingReason::Restart),
                ("vcpus", PendingReason::Limit)
            ]
        );
        assert_eq!(
            (pending[0].running.as_str(), pending[0].wanted.as_str()),
            ("8", "16")
        );
        assert_eq!(
            (pending[1].running.as_str(), pending[1].wanted.as_str()),
            ("2", "9")
        );
        assert_eq!(
            pending[1].detail.as_deref(),
            Some("vCPUs can change between 2 and 8 while it runs")
        );

        // 켤 때 수 아래
        let mut s = spec();
        s.cpu.cores = 4;
        let hw = started(&s, ALL);
        s.cpu.cores = 3;
        assert_eq!(plan(VM, "web", &s, &hw, ALL).unwrap().steps, []);
        s.cpu.cores = 2;
        assert_eq!(
            reasons(&only_pending(plan(VM, "web", &s, &hw, ALL).unwrap())),
            [("vcpus", PendingReason::Limit)]
        );
    }

    #[test]
    fn a_changed_maximum_alone_waits_for_restart_and_blocks_the_live_change() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.cpu.cores = 4;
        s.cpu.max_vcpus = Some(12);
        assert_eq!(
            reasons(&only_pending(plan(VM, "web", &s, &hw, ALL).unwrap())),
            [
                ("cpu.max_vcpus", PendingReason::Restart),
                ("vcpus", PendingReason::Restart)
            ]
        );
        // 상한을 지금 수로 맞춰 없앤 것(`None`)은 상한이 같으면 바뀌지 않음이다.
        let mut s = spec();
        s.cpu.cores = 8;
        s.cpu.max_vcpus = None;
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::SetVcpus(8)]
        );
    }

    #[test]
    fn topology_and_model_changes_need_a_restart() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.cpu.sockets = 2;
        s.cpu.cores = 4;
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(reasons(&pending), [("cpu", PendingReason::Restart)]);
        assert_eq!(
            (pending[0].running.as_str(), pending[0].wanted.as_str()),
            ("1x2 host", "2x4 host")
        );

        let mut s = spec();
        s.cpu.model = CpuModel::Max;
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(reasons(&pending), [("cpu", PendingReason::Restart)]);
        assert_eq!(pending[0].wanted, "1x2 max");

        // 플래그(계획 63)도 켤 때만 정해진다. vCPU 수를 함께 바꿔도 `cpu` 보류 하나로 끝난다.
        let mut s = spec();
        s.cpu.flags = vec!["-avx512vbmi".parse().unwrap()];
        s.cpu.cores = 4;
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(reasons(&pending), [("cpu", PendingReason::Restart)]);
        assert_eq!(pending[0].wanted, "1x4 host -avx512vbmi");
        assert!(
            pending[0]
                .detail
                .as_deref()
                .is_some_and(|d| d.starts_with("-avx512vbmi; power cycle required")),
            "{:?}",
            pending[0].detail
        );
    }

    #[test]
    fn vcpus_are_unsupported_without_cpu_hotplug_or_layout() {
        let no_cpu = NodeHotplug {
            cpu: false,
            memory: true,
            memory_prealloc: true,
        };
        let mut s = spec();
        let hw = started(&s, no_cpu);
        s.cpu.cores = 4;
        let pending = only_pending(plan(VM, "web", &s, &hw, no_cpu).unwrap());
        // 상한은 이 노드에서 켜도 없으므로 보류가 아니다.
        assert_eq!(reasons(&pending), [("vcpus", PendingReason::Unsupported)]);
        assert_eq!(pending[0].detail.as_deref(), Some(NO_CPU_HOTPLUG));

        let hw = RunningHardware::legacy(&spec());
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        // 옛 배치: 상한(켤 때 없음)은 재시작, vCPU는 못 함
        assert_eq!(
            reasons(&pending),
            [
                ("cpu.max_vcpus", PendingReason::Restart),
                ("vcpus", PendingReason::Unsupported),
                ("max_memory_mib", PendingReason::Restart)
            ]
        );
        assert_eq!(pending[1].detail.as_deref(), Some(NO_LAYOUT));
    }

    #[test]
    fn memory_grows_and_shrinks_within_the_maximum() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.memory_mib = 8192;
        let plan1 = plan(VM, "web", &s, &hw, ALL).unwrap();
        assert_eq!(plan1.steps, [Step::SetMemory { plugged_mib: 4096 }]);
        assert!(plan1.pending.is_empty());

        let mut hw = hw;
        layout(&mut hw).plugged_mib = 4096;
        assert_eq!(hw.memory_now(), 8192);
        s.memory_mib = 6144;
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::SetMemory { plugged_mib: 2048 }]
        );
        s.memory_mib = 4096;
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::SetMemory { plugged_mib: 0 }]
        );
        // 켤 때 아래
        s.memory_mib = 2048;
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(reasons(&pending), [("memory_mib", PendingReason::Limit)]);
        assert_eq!(
            (pending[0].running.as_str(), pending[0].wanted.as_str()),
            ("8192", "2048")
        );
        assert_eq!(
            pending[0].detail.as_deref(),
            Some("memory can change between 4096 and 16384 MiB while it runs")
        );
    }

    #[test]
    fn memory_without_virtio_mem_or_with_a_new_maximum_waits() {
        let no_mem = NodeHotplug {
            cpu: true,
            memory: false,
            memory_prealloc: false,
        };
        let mut s = spec();
        let hw = started(&s, no_mem);
        s.memory_mib = 8192;
        let pending = only_pending(plan(VM, "web", &s, &hw, no_mem).unwrap());
        assert_eq!(
            reasons(&pending),
            [("memory_mib", PendingReason::Unsupported)]
        );
        assert_eq!(pending[0].detail.as_deref(), Some(NO_VIRTIO_MEM));

        // 상한 없이 켠 VM: 상한이 켤 때 메모리다.
        let mut s = spec();
        s.max_memory_mib = None;
        let hw = started(&s, ALL);
        s.memory_mib = 8192;
        assert_eq!(
            reasons(&only_pending(plan(VM, "web", &s, &hw, ALL).unwrap())),
            [("memory_mib", PendingReason::Limit)]
        );

        // 상한이 바뀌면 상한은 재시작이고 메모리도 재시작 때
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.max_memory_mib = Some(32768);
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(
            reasons(&pending),
            [("max_memory_mib", PendingReason::Restart)]
        );
        assert_eq!(
            (pending[0].running.as_str(), pending[0].wanted.as_str()),
            ("16384", "32768")
        );
        s.memory_mib = 8192;
        assert_eq!(
            reasons(&only_pending(plan(VM, "web", &s, &hw, ALL).unwrap())),
            [
                ("max_memory_mib", PendingReason::Restart),
                ("memory_mib", PendingReason::Restart)
            ]
        );
        // 상한을 메모리에 맞춰 없앤 것(`None`)은 바뀌지 않음이다.
        let mut s = spec();
        s.memory_mib = 16384;
        s.max_memory_mib = None;
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::SetMemory { plugged_mib: 12288 }]
        );
    }

    #[test]
    fn firmware_and_machine_need_a_restart() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.firmware = Firmware::Bios;
        s.machine = MachineType::Virt;
        let pending = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(
            reasons(&pending),
            [
                ("firmware", PendingReason::Restart),
                ("machine", PendingReason::Restart)
            ]
        );
        assert_eq!(
            (pending[0].running.as_str(), pending[0].wanted.as_str()),
            ("uefi", "bios")
        );
        assert_eq!(
            (pending[1].running.as_str(), pending[1].wanted.as_str()),
            ("q35", "virt")
        );
    }

    #[test]
    fn disks_are_added_at_the_end_on_free_ports() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.disks.push(disk("data"));
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::AddDisk { index: 1, port: 0 }]
        );
        // 부팅 순서는 받는다(다음 부팅부터).
        s.disks[1].boot_order = Some(2);
        s.disks.push(disk("logs"));
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [
                Step::AddDisk { index: 1, port: 0 },
                Step::AddDisk { index: 2, port: 1 }
            ]
        );
    }

    #[test]
    fn disks_added_while_running_are_removed_from_the_end() {
        let mut s = spec();
        let mut hw = started(&s, ALL);
        s.disks.push(disk("data"));
        s.disks.push(disk("logs"));
        hw.disks = s.disks.clone();
        layout(&mut hw).disk_ports = vec![3, 5];
        assert!(!hw.matches_boot());

        let mut less = s.clone();
        less.disks.pop();
        assert_eq!(
            plan(VM, "web", &less, &hw, ALL).unwrap().steps,
            [Step::RemoveDisk(2)]
        );
        less.disks.pop();
        assert_eq!(
            plan(VM, "web", &less, &hw, ALL).unwrap().steps,
            [Step::RemoveDisk(2), Step::RemoveDisk(1)]
        );
        // 떼고 붙이기: 떼기 먼저, 빈 포트(작은 것부터) 다시 씀
        let mut swap = s.clone();
        swap.disks[2] = disk("other");
        assert_eq!(
            plan(VM, "web", &swap, &hw, ALL).unwrap().steps,
            [Step::RemoveDisk(2), Step::AddDisk { index: 2, port: 0 }]
        );
        layout(&mut hw).nic_ports = vec![0];
        hw.nics.push(nic(2));
        swap.nics.push(nic(2));
        assert_eq!(
            plan(VM, "web", &swap, &hw, ALL).unwrap().steps,
            [Step::RemoveDisk(2), Step::AddDisk { index: 2, port: 1 }]
        );
    }

    #[test]
    fn other_disk_changes_are_refused_while_running() {
        let mut s = spec();
        s.disks.push(disk("data"));
        let mut hw = started(&s, ALL);
        s.disks.push(disk("logs"));
        s.disks.push(disk("tmp"));
        hw.disks = s.disks.clone();
        layout(&mut hw).disk_ports = vec![0, 1];
        let changed = |f: &dyn Fn(&mut VmSpec)| {
            let mut next = s.clone();
            f(&mut next);
            plan(VM, "web", &next, &hw, ALL).unwrap_err()
        };
        // 부팅 디스크 떼기
        assert_eq!(
            changed(&|n| {
                n.disks.truncate(1);
            }),
            DeviceError::Changed("remove disk \"data\"".into())
        );
        assert_eq!(
            changed(&|n| {
                n.disks[1] = disk("new");
            }),
            DeviceError::Changed("replace disk \"data\"".into())
        );
        // 가운데 떼기(실행 중 붙인 것이라도)
        assert_eq!(
            changed(&|n| {
                n.disks.remove(2);
            }),
            DeviceError::Changed("remove disk \"logs\" from the middle".into())
        );
        // 순서 바꾸기
        assert_eq!(
            changed(&|n| n.disks.swap(2, 3)),
            DeviceError::Changed("reorder the disks".into())
        );
        // 버스·부팅 순서 바꾸기
        assert_eq!(
            changed(&|n| n.disks[0].bus = DiskBus::Sata),
            DeviceError::Changed("change disk \"root\"".into())
        );
        assert_eq!(
            changed(&|n| n.disks[2].boot_order = Some(3)),
            DeviceError::Changed("change disk \"logs\"".into())
        );
        // SATA 붙이기
        assert_eq!(
            changed(&|n| n.disks.push(DiskSpec {
                bus: DiskBus::Sata,
                ..disk("win")
            })),
            DeviceError::SataDisk("win".into())
        );
    }

    #[test]
    fn devices_need_a_layout_and_a_free_port() {
        let mut s = spec();
        let hw = RunningHardware::legacy(&s);
        s.disks.push(disk("data"));
        assert_eq!(plan(VM, "web", &s, &hw, ALL), Err(DeviceError::NoLayout));
        // 장치가 그대로면 배치 없이도 vCPU·메모리 보류를 셈한다.
        let mut s = spec();
        s.memory_mib = 8192;
        assert_eq!(
            reasons(&only_pending(plan(VM, "web", &s, &hw, ALL).unwrap())),
            [
                ("cpu.max_vcpus", PendingReason::Restart),
                ("max_memory_mib", PendingReason::Restart),
                ("memory_mib", PendingReason::Unsupported)
            ]
        );

        // 포트 8개를 다 씀
        let mut s = spec();
        let mut hw = started(&s, ALL);
        for i in 0..4 {
            s.disks.push(disk(&format!("d{i}")));
            s.nics.push(nic(10 + i));
        }
        hw.disks = s.disks.clone();
        hw.nics = s.nics.clone();
        layout(&mut hw).disk_ports = vec![0, 2, 4, 6];
        layout(&mut hw).nic_ports = vec![1, 3, 5, 7];
        let mut more = s.clone();
        more.disks.push(disk("d9"));
        assert_eq!(
            plan(VM, "web", &more, &hw, ALL),
            Err(DeviceError::NoFreePort)
        );
        let mut more = s.clone();
        more.nics.push(nic(30));
        assert_eq!(
            plan(VM, "web", &more, &hw, ALL),
            Err(DeviceError::NoFreePort)
        );
        // 하나 떼면 그 포트를 다시 쓴다.
        let mut swap = s.clone();
        swap.nics[4] = nic(30);
        assert_eq!(
            plan(VM, "web", &swap, &hw, ALL).unwrap().steps,
            [Step::RemoveNic(4), Step::AddNic { index: 4, port: 7 }]
        );
    }

    #[test]
    fn nics_follow_the_same_rules_and_security_groups_change_freely() {
        let mut s = spec();
        let mut hw = started(&s, ALL);
        // 보안 그룹만 바꾸기는 단계가 없다.
        s.nics[0].security_groups = vec!["web".into()];
        assert_eq!(plan(VM, "web", &s, &hw, ALL).unwrap(), Plan::default());
        // 끝에 붙이기와 실행 중 붙인 것 떼기
        s.nics.push(nic(2));
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::AddNic { index: 1, port: 0 }]
        );
        hw.nics = s.nics.clone();
        layout(&mut hw).nic_ports = vec![0];
        let mut less = s.clone();
        less.nics.pop();
        assert_eq!(
            plan(VM, "web", &less, &hw, ALL).unwrap().steps,
            [Step::RemoveNic(1)]
        );
        // MAC·네트워크·모델 바꾸기와 부팅 NIC 떼기
        let refused = |f: &dyn Fn(&mut NicSpec)| {
            let mut next = s.clone();
            f(&mut next.nics[0]);
            plan(VM, "web", &next, &hw, ALL).unwrap_err()
        };
        assert_eq!(
            refused(&|n| n.mac = Some(MacAddr([0x52, 0x54, 0, 0, 0, 9]))),
            DeviceError::Changed("replace NIC 0".into())
        );
        assert_eq!(
            refused(&|n| n.network = "wan".into()),
            DeviceError::Changed("change NIC 0".into())
        );
        assert_eq!(
            refused(&|n| n.model = NicModel::E1000e),
            DeviceError::Changed("change NIC 0".into())
        );
        let mut none = s.clone();
        none.nics.clear();
        assert_eq!(
            plan(VM, "web", &none, &hw, ALL).unwrap_err(),
            DeviceError::Changed("remove NIC 0".into())
        );
        let mut swapped = s.clone();
        swapped.nics.swap(0, 1);
        assert_eq!(
            plan(VM, "web", &swapped, &hw, ALL).unwrap_err(),
            DeviceError::Changed("reorder the NICs".into())
        );
    }

    #[test]
    fn steps_run_removals_then_cpu_then_memory_then_additions() {
        let mut s = spec();
        let mut hw = started(&s, ALL);
        s.disks.push(disk("data"));
        s.nics.push(nic(2));
        hw.disks = s.disks.clone();
        hw.nics = s.nics.clone();
        layout(&mut hw).disk_ports = vec![0];
        layout(&mut hw).nic_ports = vec![1];
        s.disks[1] = disk("other");
        s.nics[1] = nic(3);
        s.disks.push(disk("more"));
        s.cpu.cores = 4;
        s.memory_mib = 8192;
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [
                Step::RemoveNic(1),
                Step::RemoveDisk(1),
                Step::SetVcpus(4),
                Step::SetMemory { plugged_mib: 4096 },
                Step::AddDisk { index: 1, port: 0 },
                Step::AddDisk { index: 2, port: 1 },
                Step::AddNic { index: 1, port: 2 },
            ]
        );
    }

    #[test]
    fn pending_keeps_failed_items() {
        let mut hw = started(&spec(), ALL);
        layout(&mut hw).vcpus = 4;
        let mut s = spec();
        s.cpu.cores = 2;
        s.firmware = Firmware::Bios;
        let failed = PendingChange {
            field: "vcpus".into(),
            running: "4".into(),
            wanted: "2".into(),
            reason: PendingReason::Failed,
            detail: Some("the guest did not release vCPU 3 within 60s".into()),
        };
        let out = pending(VM, "web", &s, &hw, ALL, std::slice::from_ref(&failed), None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].field, "firmware");
        assert_eq!(out[1], failed);
        // 셈한 항목과 같은 field면 그 자리를 덮는다.
        s.cpu.cores = 12;
        let out = pending(VM, "web", &s, &hw, ALL, std::slice::from_ref(&failed), None);
        assert_eq!(
            reasons(&out),
            [
                ("vcpus", PendingReason::Failed),
                ("firmware", PendingReason::Restart)
            ]
        );
        assert!(pending(VM, "web", &spec(), &started(&spec(), ALL), ALL, &[], None).is_empty());
    }

    /// 핫플러그 Task 끝의 보류는 노드의 지금 해석(`wanted`)을 받아, 기준·숨김 목록에서 온 보류 `cpu`를 지우지 않는다.
    #[test]
    fn pending_keeps_a_cpu_change_from_the_resolution() {
        let mut hw = started(&spec(), ALL);
        let now = crate::cpu::ResolvedCpu {
            model: "host".into(),
            flags: Default::default(),
            enforce: false,
            source: crate::cpu::CpuSource::Spec,
            dropped_mask: Vec::new(),
        };
        hw.cpu_resolved = Some(now.clone());
        let mut masked = now.clone();
        masked.flags.insert("avx512vbmi".into(), false);
        let out = pending(VM, "web", &spec(), &hw, ALL, &[], Some(&masked));
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].field.as_str(), out[0].wanted.as_str()),
            ("cpu", "host -avx512vbmi")
        );
        assert!(pending(VM, "web", &spec(), &hw, ALL, &[], Some(&now)).is_empty());
        // 문맥이 없으면 스펙만 견준다.
        assert!(pending(VM, "web", &spec(), &hw, ALL, &[], None).is_empty());
        // 해석만 바뀐 보류는 vCPU 문제를 가리지 않는다(리뷰 1 M1).
        let mut more = spec();
        more.cpu.cores = hw.cpu.max() as u16 + 1;
        let out = pending(VM, "web", &more, &hw, ALL, &[], Some(&masked));
        let fields: Vec<&str> = out.iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields, ["cpu", "vcpus"]);
        assert_eq!(out[1].reason, PendingReason::Limit);
    }

    /// 실행 중 CPU 정책·vNUMA·hugepages를 바꾸면(계획 64) 보류 `numa`이고 나머지 계획은 그대로다. vNUMA VM은 virtio-mem
    /// 장치가 게스트 노드마다 있다.
    #[test]
    fn numa_changes_are_pending_until_a_power_cycle() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.hugepages = Some(crate::numa::PageSize::Size2M);
        assert!(hardware_changed(&spec(), &s));
        let mut numa = spec();
        numa.numa = Some(crate::numa::NumaSpec {
            nodes: crate::numa::NumaNodes::Count(1),
            memory: None,
        });
        assert!(hardware_changed(&spec(), &numa));
        let out = only_pending(plan(VM, "web", &s, &hw, ALL).unwrap());
        assert_eq!(reasons(&out), [("numa", PendingReason::Restart)]);
        assert!(
            out[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("hugepages none → 2M")
        );
        // 기록과 같으면 보류가 없다.
        assert!(only_pending(plan(VM, "web", &spec(), &hw, ALL).unwrap()).is_empty());

        let mut hw = started(&spec(), ALL);
        layout(&mut hw).max_memory_mib = hw.memory_mib + 1024;
        hw.numa = Some(crate::numa::ResolvedNuma {
            nodes: vec![
                crate::numa::GuestNode {
                    sockets: vec![0],
                    memory_mib: hw.memory_mib / 2,
                },
                crate::numa::GuestNode {
                    sockets: vec![1],
                    memory_mib: hw.memory_mib / 2,
                },
            ],
            distances: Vec::new(),
            page: None,
            policy: Default::default(),
            thread_policy: None,
            emulator: None,
            memory: None,
        });
        let ids = qemu_devices(&spec(), Some(&hw));
        assert!(ids.contains(&"vmem0-dev".to_string()) && ids.contains(&"vmem1-dev".to_string()));
    }

    /// 게스트가 아직 내려 주지 않은 장치는 스펙의 같은 장치와 같지 않은 것으로 본다: 떼기를 먼저 마저 하고 스펙에
    /// 있으면 같은 순번에 다시 붙인다(옛 길·섞인 버전에서 스펙에 다시 들어간 장치).
    #[test]
    fn devices_still_being_released_are_removed_first() {
        let mut s = spec();
        s.disks.push(disk("data"));
        s.nics.push(nic(2));
        let mut hw = started(&spec(), ALL);
        hw.disks = s.disks.clone();
        hw.nics = s.nics.clone();
        layout(&mut hw).disk_ports = vec![0];
        layout(&mut hw).nic_ports = vec![1];
        hw.unplugging = vec!["disk1-dev".into()];
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::RemoveDisk(1), Step::AddDisk { index: 1, port: 0 }]
        );
        // 스펙에서도 뺐으면 떼기만 한다.
        hw.unplugging = vec!["nic1".into()];
        let mut less = s.clone();
        less.nics.pop();
        assert_eq!(
            plan(VM, "web", &less, &hw, ALL).unwrap().steps,
            [Step::RemoveNic(1)]
        );
        assert_eq!(device_name(&hw, "nic1"), "NIC 1 on network \"lan\"");
        assert_eq!(device_name(&hw, "disk1-dev"), "disk \"data\"");
        assert_eq!(device_name(&hw, "cpu3"), "cpu3");
    }

    /// 게스트가 아직 내려 주지 않은 vCPU가 있으면 같은 수라도 vCPU 단계를 다시 낸다.
    #[test]
    fn vcpus_still_being_released_are_set_again() {
        let mut hw = started(&spec(), ALL);
        layout(&mut hw).vcpus = 4;
        let mut s = spec();
        s.cpu.cores = 4;
        assert!(plan(VM, "web", &s, &hw, ALL).unwrap().steps.is_empty());
        hw.unplugging = vec!["cpu2".into(), "cpu3".into()];
        assert_eq!(
            plan(VM, "web", &s, &hw, ALL).unwrap().steps,
            [Step::SetVcpus(4)]
        );
    }

    /// 디스크·NIC를 실행 중에 맞출 수 없어도 vCPU·메모리 단계와 보류는 센다.
    #[test]
    fn resources_are_planned_without_the_devices() {
        let hw = started(&spec(), ALL);
        let mut s = spec();
        s.disks.clear();
        s.cpu.cores = 4;
        s.firmware = Firmware::Bios;
        assert!(plan(VM, "web", &s, &hw, ALL).is_err());
        let p = plan_resources(VM, "web", &s, &hw, ALL);
        assert_eq!(p.steps, [Step::SetVcpus(4)]);
        assert_eq!(reasons(&p.pending), [("firmware", PendingReason::Restart)]);
    }

    fn node(features: bool, caps: Option<NodeHotplug>) -> Node {
        let mut n = Resource::new(
            "n2",
            NodeSpec {
                host_resources: None,
                address: "10.0.0.2:8006".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status = NodeStatus {
            features: if features {
                vec![HOTPLUG.to_string()]
            } else {
                vec![]
            },
            hotplug: caps,
            ..Default::default()
        };
        n
    }

    #[test]
    fn can_receive_needs_the_same_layout_support() {
        let s = spec();
        let full = node(true, Some(ALL));
        assert_eq!(can_receive(None, &node(false, None)), Ok(()));
        // 옛 배치는 어느 노드든 된다.
        let legacy = RunningHardware::legacy(&s);
        assert_eq!(can_receive(Some(&legacy), &node(false, None)), Ok(()));

        let hw = started(&s, ALL);
        assert_eq!(can_receive(Some(&hw), &full), Ok(()));
        assert_eq!(
            can_receive(Some(&hw), &node(false, Some(ALL))),
            Err("node \"n2\" does not support VMs started with hotplug support yet".into())
        );
        let no_cpu = node(
            true,
            Some(NodeHotplug {
                cpu: false,
                memory: true,
                memory_prealloc: true,
            }),
        );
        assert_eq!(
            can_receive(Some(&hw), &no_cpu),
            Err("node \"n2\" cannot hot-plug vCPUs".into())
        );
        let no_mem = node(
            true,
            Some(NodeHotplug {
                cpu: true,
                memory: false,
                memory_prealloc: false,
            }),
        );
        assert_eq!(
            can_receive(Some(&hw), &no_mem),
            Err("node \"n2\" has no virtio-mem".into())
        );
        // 리뷰 2 N4: 꽂은 hugepages를 미리 잡지 못하는 노드는 hugepages + virtio-mem VM을 받지 않는다.
        let no_prealloc = node(
            true,
            Some(NodeHotplug {
                cpu: true,
                memory: true,
                memory_prealloc: false,
            }),
        );
        assert_eq!(can_receive(Some(&hw), &no_prealloc), Ok(()));
        let mut huge = hw.clone();
        huge.numa = Some(crate::numa::ResolvedNuma {
            nodes: Vec::new(),
            distances: Vec::new(),
            page: Some(crate::numa::PageKind::Size2M),
            policy: Default::default(),
            thread_policy: None,
            emulator: None,
            memory: None,
        });
        assert_eq!(
            can_receive(Some(&huge), &no_prealloc),
            Err("node \"n2\" cannot preallocate hugepages for virtio-mem (needs QEMU 7.1)".into())
        );
        // 상한 없이 켠 VM은 능력 없는 노드도 받는다.
        let mut plain = spec();
        plain.cpu.max_vcpus = None;
        plain.max_memory_mib = None;
        let hw = started(&plain, ALL);
        assert_eq!(can_receive(Some(&hw), &node(true, None)), Ok(()));
        // 게스트가 아직 내려 주지 않은 장치가 있으면 어느 노드도 받지 않는다.
        let mut hw = started(&s, ALL);
        hw.unplugging = vec!["disk1-dev".into()];
        assert_eq!(
            can_receive(Some(&hw), &full),
            Err("the guest has not released disk1-dev yet; run `vm hotplug` again".into())
        );
    }

    #[test]
    fn devices_match_compares_disks_and_nics_without_security_groups() {
        let mut s = spec();
        let hw = started(&s, ALL);
        assert!(devices_match(&s, &hw));
        s.nics[0].security_groups = vec!["web".into()];
        assert!(devices_match(&s, &hw));
        // 스펙에서 뗐지만 QEMU에 남은 장치, 스펙에만 있는 장치
        let mut removed = s.clone();
        removed.disks.clear();
        assert!(!devices_match(&removed, &hw));
        let mut added = s.clone();
        added.nics.push(nic(2));
        assert!(!devices_match(&added, &hw));
        let mut changed = s.clone();
        changed.nics[0].network = "dmz".into();
        assert!(!devices_match(&changed, &hw));
        // CPU·메모리는 견주지 않는다(다음 재시작 때 적용).
        s.cpu.cores = 4;
        s.memory_mib = 8192;
        assert!(devices_match(&s, &hw));
    }

    #[test]
    fn qemu_devices_lists_the_disks_nics_and_hot_plugged_cpus_and_memory() {
        let mut s = spec();
        s.disks.push(disk("data"));
        assert_eq!(
            qemu_devices(&s, None),
            ["disk0-dev", "disk1-dev", "nic0"].map(String::from)
        );
        let mut hw = started(&spec(), ALL);
        assert_eq!(
            qemu_devices(&s, Some(&hw)),
            ["disk0-dev", "disk1-dev", "nic0", "vmem0-dev"].map(String::from)
        );
        layout(&mut hw).vcpus = 4;
        assert_eq!(
            qemu_devices(&s, Some(&hw)),
            [
                "cpu2",
                "cpu3",
                "disk0-dev",
                "disk1-dev",
                "nic0",
                "vmem0-dev"
            ]
            .map(String::from)
        );
        // 배치 없이 켠 VM은 스펙의 디스크·NIC뿐이다.
        let legacy = RunningHardware::legacy(&spec());
        assert_eq!(
            qemu_devices(&spec(), Some(&legacy)),
            ["disk0-dev", "nic0"].map(String::from)
        );
    }

    #[test]
    fn allocated_memory_takes_the_larger_of_spec_and_running() {
        let mut vm: Vm = Resource::new("web", spec());
        assert_eq!(allocated_memory_mib(&vm), 4096);
        let mut hw = started(&vm.spec, ALL);
        layout(&mut hw).plugged_mib = 4096;
        vm.status.hardware = Some(hw);
        // 8192 → 4096 줄이기가 실패해 8192로 돈다.
        assert_eq!(allocated_memory_mib(&vm), 8192);
        vm.spec.memory_mib = 12288;
        assert_eq!(allocated_memory_mib(&vm), 12288);
    }

    #[test]
    fn matches_boot_only_without_live_changes_or_pending() {
        let mut hw = started(&spec(), ALL);
        assert!(hw.matches_boot());
        assert!(RunningHardware::legacy(&spec()).matches_boot());
        layout(&mut hw).vcpus = 3;
        assert!(!hw.matches_boot());
        let mut hw = started(&spec(), ALL);
        layout(&mut hw).plugged_mib = 128;
        assert!(!hw.matches_boot());
        let mut hw = started(&spec(), ALL);
        layout(&mut hw).nic_ports = vec![0];
        assert!(!hw.matches_boot());
        let mut hw = RunningHardware::legacy(&spec());
        hw.pending = pending(
            VM,
            "web",
            &VmSpec {
                firmware: Firmware::Bios,
                ..spec()
            },
            &hw,
            ALL,
            &[],
            None,
        );
        assert!(!hw.matches_boot());
    }

    #[test]
    fn hardware_changed_ignores_names_policies_and_security_groups() {
        let s = spec();
        let mut t = s.clone();
        t.ha = Some(Default::default());
        t.nics[0].security_groups = vec!["web".into()];
        t.restart = crate::vm::RestartPolicy::OnFailure;
        assert!(!hardware_changed(&s, &t));
        for f in [
            (|t: &mut VmSpec| t.cpu.cores = 3) as fn(&mut VmSpec),
            |t| t.cpu.max_vcpus = None,
            |t| t.memory_mib = 8192,
            |t| t.max_memory_mib = None,
            |t| t.firmware = Firmware::Bios,
            |t| t.machine = MachineType::Virt,
            |t| t.disks.push(disk("data")),
            |t| t.nics[0].network = "wan".into(),
            |t| t.nics.clear(),
        ] {
            let mut t = s.clone();
            f(&mut t);
            assert!(hardware_changed(&s, &t), "{t:?}");
        }
    }

    #[test]
    fn running_hardware_round_trips_and_omits_empty_parts() {
        let hw = RunningHardware::legacy(&spec());
        let json = serde_json::to_value(&hw).unwrap();
        assert!(json.get("layout").is_none(), "{json}");
        assert!(json.get("pending").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<RunningHardware>(json).unwrap(), hw);

        let mut hw = started(&spec(), ALL);
        layout(&mut hw).disk_ports = vec![0];
        hw.pending = vec![change(
            "firmware",
            "uefi".into(),
            "bios".into(),
            PendingReason::Restart,
            None,
        )];
        let json = serde_json::to_value(&hw).unwrap();
        assert_eq!(
            json["layout"],
            serde_json::json!({
                "vcpus": 2, "max_memory_mib": 16384, "plugged_mib": 0, "ports": 8,
                "boot_disks": 1, "boot_nics": 1, "disk_ports": [0]
            })
        );
        assert_eq!(
            json["pending"],
            serde_json::json!([{ "field": "firmware", "running": "uefi", "wanted": "bios", "reason": "restart" }])
        );
        assert_eq!(serde_json::from_value::<RunningHardware>(json).unwrap(), hw);
        assert_eq!(
            serde_json::to_value(PendingReason::Unsupported).unwrap(),
            "unsupported"
        );
    }

    fn cd(iso: Option<&str>, boot_order: Option<u8>) -> CdromSpec {
        CdromSpec {
            iso: iso.map(Into::into),
            boot_order,
        }
    }

    /// CD-ROM 드라이브 둘(하나는 빈 것)을 단 스펙
    fn with_cdroms() -> VmSpec {
        let mut s = spec();
        s.cdroms = vec![cd(Some("ubuntu"), None), cd(None, None)];
        s
    }

    #[test]
    fn at_start_records_the_cdroms() {
        let s = with_cdroms();
        assert_eq!(started(&s, ALL).cdroms, s.cdroms);
        assert_eq!(RunningHardware::legacy(&s).cdroms, s.cdroms);
    }

    #[test]
    fn changing_media_is_one_step_per_drive_even_without_a_layout() {
        let s = with_cdroms();
        for hw in [started(&s, ALL), RunningHardware::legacy(&s)] {
            let mut want = s.clone();
            want.cdroms[0].iso = None;
            want.cdroms[1].iso = Some("alpine".into());
            let plan = plan(VM, "web", &want, &hw, ALL).unwrap();
            assert_eq!(
                plan.steps,
                [
                    Step::ChangeMedium { index: 0 },
                    Step::ChangeMedium { index: 1 }
                ]
            );
            // 배치 없이 켠 기록은 상한이 없어 상한 보류만 있다.
            assert!(
                plan.pending
                    .iter()
                    .all(|p| p.field.ends_with("max_vcpus") || p.field == "max_memory_mib"),
                "{plan:?}"
            );
            // 같은 매체면 할 일이 없다.
            assert!(plan_fn(&s, &hw).steps.is_empty());
        }
    }

    fn plan_fn(spec: &VmSpec, hw: &RunningHardware) -> Plan {
        plan(VM, "web", spec, hw, ALL).unwrap()
    }

    #[test]
    fn media_changes_come_after_removals_and_resources_and_before_additions() {
        let s = with_cdroms();
        let hw = started(&s, ALL);
        let mut want = s.clone();
        want.cpu.cores = 4;
        want.cdroms[0].iso = Some("alpine".into());
        want.disks.push(DiskSpec {
            volume: "data".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        });
        let plan = plan_fn(&want, &hw);
        assert_eq!(
            plan.steps,
            [
                Step::SetVcpus(4),
                Step::ChangeMedium { index: 0 },
                Step::AddDisk { index: 1, port: 0 }
            ]
        );
    }

    #[test]
    fn adding_or_removing_cdrom_drives_needs_a_restart() {
        let s = with_cdroms();
        let hw = started(&s, ALL);
        let mut more = s.clone();
        more.cdroms.push(cd(None, None));
        let mut fewer = s.clone();
        fewer.cdroms.pop();
        for want in [more, fewer] {
            let err = plan(VM, "web", &want, &hw, ALL).unwrap_err();
            assert_eq!(
                err,
                DeviceError::Changed("add or remove CD-ROM drives".into())
            );
            assert_eq!(
                err.to_string(),
                "cannot add or remove CD-ROM drives while the VM is running"
            );
        }
        // CD-ROM이 없던 VM에 드라이브를 더하는 것도 같다.
        let hw = started(&spec(), ALL);
        assert!(plan(VM, "web", &s, &hw, ALL).is_err());
    }

    #[test]
    fn cdrom_boot_order_is_pending_until_restart() {
        let s = with_cdroms();
        let hw = started(&s, ALL);
        let mut want = s.clone();
        want.cdroms[1].boot_order = Some(5);
        let pending = only_pending(plan_fn(&want, &hw));
        assert_eq!(
            pending,
            [PendingChange {
                field: "cdroms[1].boot_order".into(),
                running: "none".into(),
                wanted: "5".into(),
                reason: PendingReason::Restart,
                detail: None,
            }]
        );
        // Task 끝의 보류에도 남는다.
        assert_eq!(
            super::pending(VM, "web", &want, &hw, ALL, &[], None),
            pending
        );
        // 부팅 순서만 다르면 마이그레이션 대상이 기록의 값으로 켠다.
        assert!(devices_match(&want, &hw));
    }

    #[test]
    fn devices_match_compares_cdrom_media() {
        let s = with_cdroms();
        let hw = started(&s, ALL);
        assert!(devices_match(&s, &hw));
        let mut want = s.clone();
        want.cdroms[1].iso = Some("alpine".into());
        assert!(!devices_match(&want, &hw));
        let mut want = s.clone();
        want.cdroms.pop();
        assert!(!devices_match(&want, &hw));
    }

    #[test]
    fn qemu_devices_lists_the_cdrom_drives() {
        let s = with_cdroms();
        let ids = ["cd0-dev", "cd1-dev", "disk0-dev", "nic0"].map(String::from);
        assert_eq!(qemu_devices(&s, None), ids);
        assert_eq!(qemu_devices(&s, Some(&RunningHardware::legacy(&s))), ids);
        // 기록이 있으면 기록의 드라이브 수다.
        let mut hw = RunningHardware::legacy(&s);
        hw.cdroms.pop();
        assert_eq!(
            qemu_devices(&s, Some(&hw)),
            ["cd0-dev", "disk0-dev", "nic0"].map(String::from)
        );
    }

    #[test]
    fn hardware_changed_sees_cdroms_and_cloud_init() {
        let s = with_cdroms();
        assert!(!hardware_changed(&s, &s.clone()));
        let mut media = s.clone();
        media.cdroms[0].iso = None;
        assert!(hardware_changed(&s, &media));
        let mut order = s.clone();
        order.cdroms[0].boot_order = Some(1);
        assert!(hardware_changed(&s, &order));
        let mut ci = s.clone();
        ci.cloud_init = Some(CloudInit::default());
        assert!(hardware_changed(&s, &ci));
        let mut host = ci.clone();
        host.cloud_init.as_mut().unwrap().hostname = Some("web".into());
        assert!(hardware_changed(&ci, &host));
    }

    /// cloud-init(호스트 이름 `host`)이 있는 스펙
    fn with_cloud_init(host: &str) -> VmSpec {
        let mut s = spec();
        s.cloud_init = Some(CloudInit {
            hostname: Some(host.into()),
            ..CloudInit::default()
        });
        s
    }

    #[test]
    fn at_start_records_the_seed_instance_id() {
        let s = with_cloud_init("web");
        let want = crate::cloud_init::render(VM, "web", s.cloud_init.as_ref().unwrap(), &s.nics)
            .instance_id;
        for enabled in [true, false] {
            let (hw, _) = RunningHardware::at_start(VM, "web", &s, ALL, enabled);
            assert_eq!(hw.cloud_init.as_deref(), Some(want.as_str()));
        }
        assert_eq!(started(&spec(), ALL).cloud_init, None);
        // 옛 노드가 켠 기록(씨앗 없음)
        assert_eq!(RunningHardware::legacy(&s).cloud_init, None);
    }

    #[test]
    fn seed_matches_compares_the_instance_id() {
        let s = with_cloud_init("web");
        let hw = started(&s, ALL);
        assert!(seed_matches(VM, "web", &s, &hw));
        // 실행 중 cloud-init을 바꿈·이름 바꿈(호스트 이름 없는 VM)은 다른 씨앗이다.
        assert!(!seed_matches(VM, "web", &with_cloud_init("web2"), &hw));
        let no_host = VmSpec {
            cloud_init: Some(CloudInit::default()),
            ..spec()
        };
        let hw = started(&no_host, ALL);
        assert!(seed_matches(VM, "web", &no_host, &hw));
        assert!(!seed_matches(VM, "web-new", &no_host, &hw));
        // cloud-init 없는 VM
        assert!(seed_matches(VM, "web", &spec(), &started(&spec(), ALL)));
    }

    /// 주소 설정이 있는 cloud-init VM에 실행 중 NIC를 붙여도 씨앗은 켤 때 단 NIC로 셈해 같다(받는 쪽도 그 NIC로 다시
    /// 만든다). 새 NIC는 다음 재시작 때 들어가므로 보류로 남는다.
    #[test]
    fn a_hot_plugged_nic_keeps_the_seed() {
        let mut s = spec();
        s.cloud_init = Some(CloudInit {
            networks: vec![crate::cloud_init::CloudInitNet {
                ipv4: Some(crate::cloud_init::IpConfig::Static {
                    address: "10.0.0.5/24".into(),
                    gateway: None,
                }),
                ipv6: None,
            }],
            ..CloudInit::default()
        });
        let mut hw = started(&s, ALL);
        let mut want = s.clone();
        want.nics.push(nic(2));
        // 핫플러그 Task가 붙인 뒤의 기록
        hw.nics.push(nic(2));
        layout(&mut hw).nic_ports.push(0);
        assert_eq!(hw.boot_nics(), &s.nics[..]);
        assert!(devices_match(&want, &hw));
        assert!(seed_matches(VM, "web", &want, &hw));
        assert_eq!(
            reasons(&plan_fn(&want, &hw).pending),
            [("cloud_init", PendingReason::Restart)]
        );
        // 배치 없이 켠 기록은 모든 NIC가 켤 때 단 것이다.
        let legacy = RunningHardware::legacy(&want);
        assert_eq!(legacy.boot_nics(), &want.nics[..]);
    }

    #[test]
    fn cloud_init_changes_are_pending_until_restart() {
        let s = with_cloud_init("web");
        let hw = started(&s, ALL);
        // 같은 내용이면 보류가 없다.
        assert_eq!(plan_fn(&s, &hw), Plan::default());
        let iid = hw.cloud_init.clone().unwrap();

        // 바꿈
        let changed = with_cloud_init("web2");
        let new_iid = started(&changed, ALL).cloud_init.unwrap();
        let want = [PendingChange {
            field: "cloud_init".into(),
            running: iid.clone(),
            wanted: new_iid.clone(),
            reason: PendingReason::Restart,
            detail: None,
        }];
        assert_eq!(only_pending(plan_fn(&changed, &hw)), want);
        assert_eq!(
            super::pending(VM, "web", &changed, &hw, ALL, &[], None),
            want
        );
        assert_eq!(plan_resources(VM, "web", &changed, &hw, ALL).pending, want);

        // 켬·끔
        let plain = started(&spec(), ALL);
        let on = only_pending(plan_fn(&s, &plain));
        assert_eq!(reasons(&on), [("cloud_init", PendingReason::Restart)]);
        assert_eq!(
            (on[0].running.as_str(), on[0].wanted.as_str()),
            ("none", iid.as_str())
        );
        let off = only_pending(plan_fn(&spec(), &hw));
        assert_eq!(
            (off[0].running.as_str(), off[0].wanted.as_str()),
            (iid.as_str(), "none")
        );

        // VM 이름이 호스트 이름이면 이름도 내용이다.
        let bare = {
            let mut s = spec();
            s.cloud_init = Some(CloudInit::default());
            s
        };
        let hw = started(&bare, ALL);
        assert!(plan(VM, "web", &bare, &hw, ALL).unwrap().pending.is_empty());
        assert_eq!(plan(VM, "db", &bare, &hw, ALL).unwrap().pending.len(), 1);
    }

    #[test]
    fn devices_match_compares_whether_there_is_a_seed() {
        let s = with_cloud_init("web");
        let hw = started(&s, ALL);
        assert!(devices_match(&s, &hw));
        // 내용만 다르면 받는 쪽이 스펙으로 다시 만든다.
        assert!(devices_match(&with_cloud_init("web2"), &hw));
        assert!(!devices_match(&spec(), &hw));
        assert!(!devices_match(&s, &started(&spec(), ALL)));
    }

    #[test]
    fn qemu_devices_lists_the_seed_drive() {
        let s = with_cloud_init("web");
        let ids = ["cidata-dev", "disk0-dev", "nic0"].map(String::from);
        assert_eq!(qemu_devices(&s, None), ids);
        let hw = started(&s, NodeHotplug::default());
        assert_eq!(qemu_devices(&s, Some(&hw)), ids);
        // 기록에 씨앗이 없으면(켤 때 없었음) 드라이브도 없다.
        assert_eq!(
            qemu_devices(&s, Some(&RunningHardware::legacy(&s))),
            ["disk0-dev", "nic0"].map(String::from)
        );
        // CD-ROM과 함께면 이름순이다.
        let mut both = with_cdroms();
        both.cloud_init = s.cloud_init.clone();
        assert_eq!(
            qemu_devices(&both, None),
            ["cd0-dev", "cd1-dev", "cidata-dev", "disk0-dev", "nic0"].map(String::from)
        );
    }

    fn uefi() -> VmSpec {
        let mut s = spec();
        s.firmware = crate::vm::Firmware::Uefi;
        s
    }

    #[test]
    fn records_and_compares_tpm_secure_boot_and_agent_channel() {
        let mut s = uefi();
        s.tpm = true;
        s.secure_boot = true;
        s.guest_agent = Some(Default::default());
        let hw = started(&s, ALL);
        assert!(hw.tpm && hw.secure_boot && hw.guest_agent);
        assert!(devices_match(&s, &hw));
        assert!(plan(VM, "web", &s, &hw, ALL).unwrap().pending.is_empty());

        // 실행 중 더하거나 빼면 다음 재시작 때 적용한다.
        let plain = uefi();
        assert!(hardware_changed(&s, &plain));
        assert!(!devices_match(&plain, &hw));
        let pending = only_pending(plan(VM, "web", &plain, &hw, ALL).unwrap());
        assert_eq!(
            pending
                .iter()
                .map(|p| (p.field.as_str(), p.running.as_str(), p.wanted.as_str()))
                .collect::<Vec<_>>(),
            [
                ("tpm", "on", "off"),
                ("secure_boot", "on", "off"),
                ("guest_agent", "on", "off")
            ]
        );
        assert!(pending.iter().all(|p| p.reason == PendingReason::Restart));

        // 에이전트의 freeze_fs·shutdown은 장치가 아니다.
        let mut acpi = s.clone();
        acpi.guest_agent = Some(crate::vm::GuestAgentSpec {
            freeze_fs: false,
            shutdown: crate::vm::AgentShutdown::Acpi,
        });
        assert!(!hardware_changed(&s, &acpi));
        assert!(devices_match(&acpi, &hw));
    }

    /// 계획 59 전 노드가 켠 게스트 감시 VM의 기록에는 `guest_agent`가 없다: 채널이 바뀐 것으로 보지 않는다.
    #[test]
    fn old_records_of_guest_watch_vms_still_match() {
        let mut s = spec();
        s.ha = Some(crate::vm::VmHa {
            guest_watch: Some(Default::default()),
            ..Default::default()
        });
        let mut hw = started(&s, ALL);
        assert!(hw.guest_agent);
        assert!(devices_match(&s, &hw));
        hw.guest_agent = false;
        assert!(devices_match(&s, &hw));
        assert!(plan(VM, "web", &s, &hw, ALL).unwrap().pending.is_empty());
        // 스펙에 guest_agent를 더하면 기록이 없는 채널도 다시 켜야 하는 것으로 본다.
        s.guest_agent = Some(Default::default());
        assert!(!devices_match(&s, &hw));
        // 게스트 감시도 에이전트도 없는데 채널이 있으면 다르다.
        let bare = spec();
        hw.guest_agent = true;
        assert!(!devices_match(&bare, &hw));
    }

    /// 리뷰 2 N4·N9: 장치가 미리 잡지 못하는 노드(QEMU < 7.1)에서는 hugepages VM을 메모리 핫플러그 없이 켜고(상한 =
    /// 메모리라 `any`도 1G를 쓸 수 있다), 메모리 바꾸기는 그 까닭으로 보류다. hugepages 없는 VM은 그대로 핫플러그한다.
    #[test]
    fn hugepage_vms_skip_memory_hotplug_without_virtio_mem_prealloc() {
        let old = NodeHotplug {
            cpu: true,
            memory: true,
            memory_prealloc: false,
        };
        let mut s = spec();
        assert!(memory_hotplug(&s, old));
        assert!(matches!(start_spec(&s, old), std::borrow::Cow::Borrowed(_)));
        s.hugepages = Some(crate::numa::PageSize::Any);
        assert!(!memory_hotplug(&s, old));
        assert!(memory_hotplug(&s, ALL));
        let eff = start_spec(&s, old);
        assert_eq!(eff.max_memory_mib, Some(s.memory_mib));
        assert_eq!(start_spec(&s, ALL).max_memory_mib, Some(16384));
        let hw = started(&s, old);
        assert_eq!(hw.max_memory_mib(), 4096);
        s.memory_mib = 8192;
        // (기록에 NUMA 모양이 없어 보류 `numa`도 있다 — 여기서는 메모리만 본다)
        let pending = only_pending(plan(VM, "web", &s, &hw, old).unwrap());
        let memory = pending.iter().find(|p| p.field == "memory_mib").unwrap();
        assert_eq!(memory.reason, PendingReason::Unsupported);
        assert_eq!(memory.detail.as_deref(), Some(NO_PREALLOC));
        assert!(!pending.iter().any(|p| p.field == "max_memory_mib"));
    }
}
