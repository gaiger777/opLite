//! VM 하나의 매핑과 VMware 쪽 미리 확인(계획 72 §8.4·§8.5, 순수 함수 — 단위 시험): 원본 VM(인벤토리) + 규칙 + VM 덮어쓰기
//! → 계획 29 `ImportRun` → `import_spec`의 VmSpec과 디스크마다 만들 볼륨(디스크마다 다른 풀). 차례는 덮어쓰기 > 규칙 >
//! 제안(`propose`). VMware 쪽 검사는 인벤토리 값만 본다(원본을 부르지 않는다).

use std::collections::BTreeMap;
use std::net::IpAddr;

use orbivisor_types::network::Network;
use orbivisor_types::resource::validate_name;
use orbivisor_types::runtime::ImageFormat;
use orbivisor_types::storage::VolumeSpec;
use orbivisor_types::vm::{CdromSpec, DiskBus, MacAddr, PowerState, VmSpec};
use orbivisor_types::vm_check::{VmCheckLevel, VmCheckProblem};
use orbivisor_types::vm_import::{ImportRun, is_windows, propose};
use orbivisor_types::vmware::{
    VMWARE_VM_LABEL, VmwareCutoverPolicy, VmwareFolderRule, VmwareMigrationMode, VmwarePower,
    VmwareRules, VmwareShutdown, VmwareSourceStatus, VmwareTransport, VmwareVmSpec,
};
use uuid::Uuid;

use crate::resources::CreateRequest;
use crate::vm_imports::import_spec;
use crate::vmware::inventory::{InvVm, Inventory};

/// 원본 VM 하나의 디스크 상한(계획 29 `.vmx`·OVF와 같다)
pub const MAX_DISKS: usize = 32;

/// 문제 하나
pub(crate) fn problem(
    at: &str,
    level: VmCheckLevel,
    code: &str,
    message: String,
) -> VmCheckProblem {
    VmCheckProblem {
        at: at.into(),
        code: code.into(),
        message,
        level,
    }
}

fn blocked(at: &str, code: &str, message: impl Into<String>) -> VmCheckProblem {
    problem(at, VmCheckLevel::Error, code, message.into())
}

fn warn(at: &str, code: &str, message: impl Into<String>) -> VmCheckProblem {
    problem(at, VmCheckLevel::Warn, code, message.into())
}

/// 매핑 결과: 만들 VM의 스펙, 디스크마다 만들 볼륨, 제안의 경고
#[derive(Debug)]
pub(crate) struct Mapped {
    pub spec: VmSpec,
    pub volumes: Vec<CreateRequest<VolumeSpec>>,
    pub warnings: Vec<VmCheckProblem>,
}

/// 매핑에 드는 것
pub(crate) struct MapInput<'a> {
    pub rules: &'a VmwareRules,
    pub item_id: Uuid,
    pub item: &'a VmwareVmSpec,
    pub vm: &'a InvVm,
    /// 묻는 주체가 볼 수 있는 우리 네트워크 이름(제안이 같은 이름을 고른다 — 숨은 네트워크는 고르지 않는다)
    pub networks: &'a [String],
    /// 풀의 기본 볼륨 형식(없는 풀은 qcow2 — 만들기 검사가 거절한다)
    pub format: &'a dyn Fn(&str) -> ImageFormat,
}

/// 디스크 `i`의 볼륨 이름 `{대상}-disk{i}`(63자까지)
pub(crate) fn volume_name(target: &str, i: usize) -> String {
    crate::tasks::volume_name(target, &format!("disk{i}"), |_| false)
}

/// VMware 폴더 경로(`DC1/vm/Prod/Web`)에서 데이터센터의 `vm` 폴더 아래 부분(`Prod/Web`)
pub(crate) fn mirror_path(vm: &InvVm) -> Option<String> {
    let folder = vm.folder.as_deref()?;
    let dc = vm.datacenter.as_deref()?;
    let rest = folder.strip_prefix(dc)?.strip_prefix("/vm")?;
    let rest = rest.trim_start_matches('/');
    (!rest.is_empty()).then(|| rest.to_string())
}

/// 만든 VM이 갈 우리 폴더(권한을 볼 곳): 덮어쓰기 > 규칙(`flat.into`, `mirror.under` — Mirror는 그 아래에 원본 경로를
/// 만들 계획이다)
pub(crate) fn target_folder(rules: &VmwareRules, item: &VmwareVmSpec) -> Option<Uuid> {
    item.overrides
        .as_ref()
        .and_then(|o| o.folder)
        .or(match rules.folders {
            VmwareFolderRule::Flat { into } => into,
            VmwareFolderRule::Mirror { under } => under,
        })
}

/// 디스크마다 만들 대상 볼륨(`{대상}-disk{i}`, 크기 = 원본 용량, 레이블 [`VMWARE_VM_LABEL`] = 항목 id). 풀은 덮어쓰기(디스크
/// 순번) > 데이터스토어 규칙 > `default_pool`. 풀이 없는 디스크는 문제다(미리 확인과 첫 동기화가 함께 쓴다).
pub(crate) fn disk_volumes(
    rules: &VmwareRules,
    item_id: Uuid,
    item: &VmwareVmSpec,
    vm: &InvVm,
    format: &dyn Fn(&str) -> ImageFormat,
) -> Result<Vec<CreateRequest<VolumeSpec>>, Vec<VmCheckProblem>> {
    let empty = Default::default();
    let o = item.overrides.as_ref().unwrap_or(&empty);
    let mut problems = Vec::new();
    let mut volumes = Vec::new();
    for (i, d) in vm.disks.iter().enumerate() {
        let ds = d.datastore.as_deref().unwrap_or("");
        let pool = o
            .pools
            .get(i)
            .map(String::as_str)
            .or_else(|| rules.pool_for(ds));
        let Some(pool) = pool else {
            problems.push(blocked(
                &format!("disks.{i}"),
                "pool_unmapped",
                format!(
                    "datastore {ds:?} has no storage pool; add a datastore rule or a default pool"
                ),
            ));
            continue;
        };
        volumes.push(CreateRequest {
            name: volume_name(&item.target_name, i),
            labels: BTreeMap::from([(VMWARE_VM_LABEL.to_string(), item_id.to_string())]),
            spec: VolumeSpec {
                pool: pool.to_string(),
                size_bytes: d.capacity,
                format: format(pool),
                content: Default::default(),
                base: None,
            },
        });
    }
    if problems.is_empty() {
        Ok(volumes)
    } else {
        Err(problems)
    }
}

/// 원본 VM을 우리 VM 스펙과 만들 볼륨으로 옮긴다. 옮길 수 없으면(네트워크·풀 매핑 없음, SATA 개수) 그 문제들이다.
pub(crate) fn map_vm(input: &MapInput<'_>) -> Result<Mapped, Vec<VmCheckProblem>> {
    let MapInput {
        rules,
        item_id,
        item,
        vm,
        networks,
        format,
    } = input;
    let empty = Default::default();
    let o = item.overrides.as_ref().unwrap_or(&empty);
    let source = vm.to_source().vm;
    let (proposal, notes) = propose(&source, networks);
    let mut problems = Vec::new();

    // 네트워크: 덮어쓰기(원본 이름) > 규칙(이름 또는 `moref:{키}`) > 같은 이름의 볼 수 있는 네트워크
    let mut nets = BTreeMap::new();
    for (i, nic) in vm.nics.iter().enumerate() {
        let by_ref = nic
            .network_ref
            .as_deref()
            .and_then(|r| rules.network_for(&format!("moref:{r}")));
        let to = o
            .networks
            .get(&nic.network)
            .map(String::as_str)
            .or(by_ref)
            .or_else(|| rules.network_for(&nic.network))
            .or_else(|| {
                proposal
                    .networks
                    .get(&nic.network)
                    .and_then(|n| n.as_deref())
            });
        match to {
            Some(to) => {
                nets.insert(nic.network.clone(), to.to_string());
            }
            None => problems.push(blocked(
                &format!("nics.{i}"),
                "network_unmapped",
                format!(
                    "source network {:?} has no mapping; add a network rule",
                    nic.network
                ),
            )),
        }
    }

    let volumes = match disk_volumes(rules, *item_id, item, vm, format) {
        Ok(v) => v,
        Err(p) => {
            problems.extend(p);
            Vec::new()
        }
    };
    if !problems.is_empty() {
        return Err(problems);
    }

    let devices = rules.devices;
    let run = ImportRun {
        name: item.target_name.clone(),
        pool: String::new(),
        networks: nets,
        disk_bus: o.disk_bus.or(devices.map(|d| d.disk_bus)),
        nic_model: o.nic_model.or(devices.map(|d| d.nic_model)),
        firmware: o.firmware,
        sockets: o.sockets,
        cores: o.cores,
        memory_mib: o.memory_mib,
        ips: o.ips.clone(),
        keep_macs: o.keep_macs.unwrap_or(rules.keep_macs),
        folder: target_folder(rules, item),
        cpu_model: o.cpu_model.clone().or_else(|| rules.cpu_model.clone()),
        cpu_flags: Vec::new(),
        cpu_policy: None,
        cpu_thread_policy: None,
        cpu_emulator: None,
        numa: None,
        hugepages: None,
    };
    let names: Vec<String> = volumes.iter().map(|v| v.name.clone()).collect();
    let mut spec = import_spec(&source, &proposal, &run, &names)
        .map_err(|e| vec![blocked("disks", "invalid_mapping", e)])?;
    if rules.start_after {
        spec.power = PowerState::Running;
    }
    spec.node = item.node.or(rules.node);
    if is_windows(&vm.guest_id)
        && let Some(iso) = &rules.driver_iso
    {
        spec.cdroms.push(CdromSpec {
            iso: Some(iso.clone()),
            boot_order: None,
        });
    }
    // 전환이 원본을 끈 뒤 VM을 만들므로 MAC 경고는 이전에 맞지 않는다.
    let warnings = notes
        .into_iter()
        .filter(|n| !n.starts_with("power off the VMware VM"))
        .map(|n| warn("compute", "proposal", n))
        .collect();
    Ok(Mapped {
        spec,
        volumes,
        warnings,
    })
}

/// Tools가 알린 IP를 SDN 주소로 예약한다(설계 §11 `reserve_ips`): 대상 네트워크가 SDN이고 주소를 받지 않은 NIC마다, 그 NIC
/// MAC의 Tools IP 중 네트워크 서브넷 안의 것(주소 체계마다 하나)을 `ips`에 둔다. 서브넷 밖의 주소는 경고로 알린다.
pub(crate) fn reserve_ips(
    spec: &mut VmSpec,
    guest_ips: &BTreeMap<String, Vec<IpAddr>>,
    networks: &[Network],
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (i, nic) in spec.nics.iter_mut().enumerate() {
        let Some(mac) = nic.mac else { continue };
        let Some(net) = networks
            .iter()
            .find(|n| n.name == nic.network && n.spec.is_sdn())
        else {
            continue;
        };
        if !nic.ips.is_empty() {
            continue;
        }
        let ips = guest_ips
            .iter()
            .filter(|(m, _)| m.parse::<MacAddr>().is_ok_and(|m| m == mac))
            .flat_map(|(_, ips)| ips);
        for ip in ips {
            let inside = net.spec.subnets.iter().any(|s| s.cidr.contains(*ip));
            if !inside {
                warnings.push(format!(
                    "NIC {i}: address {ip} reported by VMware Tools is outside the subnets of network {:?}; not reserved",
                    net.name
                ));
            } else if !nic.ips.iter().any(|a| a.is_ipv4() == ip.is_ipv4()) {
                nic.ips.push(*ip);
            }
        }
    }
    warnings
}

/// RHEL 계열 guestId(dracut `hostonly` — initramfs에 virtio 모듈이 없을 수 있다)
fn rhel_family(guest_id: &str) -> bool {
    let g = guest_id.to_ascii_lowercase();
    ["rhel", "centos", "oraclelinux", "rocky", "alma"]
        .iter()
        .any(|p| g.starts_with(p))
}

/// VMware 쪽 검사가 보는 계획·원본의 값
pub(crate) struct SourceContext<'a> {
    pub mode: VmwareMigrationMode,
    pub cutover: &'a VmwareCutoverPolicy,
    pub status: &'a VmwareSourceStatus,
    pub transport: &'a VmwareTransport,
    pub inventory: &'a Inventory,
}

/// VMware 쪽 검사(설계 §8.5 2): 인벤토리 값만 본다. `ours`는 원본에 있는 우리 스냅샷의 moref(사용자 스냅샷이 아니다).
pub(crate) fn source_problems(
    ctx: &SourceContext<'_>,
    item: &VmwareVmSpec,
    vm: &InvVm,
    ours: &[&str],
) -> Vec<VmCheckProblem> {
    let SourceContext {
        mode,
        cutover,
        status: source,
        transport,
        inventory,
    } = *ctx;
    let warm = mode == VmwareMigrationMode::Warm;
    let mut out = Vec::new();
    if vm.instance_uuid.as_deref() != Some(item.instance_uuid.as_str()) {
        out.push(blocked(
            "source",
            "vmware_vm_replaced",
            format!(
                "source VM {:?} is no longer the VM in this plan (instance UUID changed)",
                vm.name
            ),
        ));
    }
    if vm
        .snapshots
        .iter()
        .any(|s| !ours.contains(&s.moref.as_str()))
    {
        out.push(blocked(
            "source",
            "vmware_snapshots",
            format!(
                "source VM {:?} has snapshots; consolidate or delete snapshots first",
                vm.name
            ),
        ));
    }
    if warm && vm.ft {
        out.push(blocked(
            "source",
            "vmware_ft",
            "fault tolerance is on; snapshots are not possible (turn FT off or use a cold migration)",
        ));
    }
    if warm && vm.template {
        out.push(blocked(
            "source",
            "vmware_template",
            "templates cannot be snapshotted; use a cold migration",
        ));
    }
    if warm && vm.hw_version.is_some_and(|v| v < 7) {
        out.push(blocked(
            "source",
            "vmware_hw_version",
            format!(
                "hardware version vmx-{} has no changed block tracking (needs 7 or later); upgrade it or use a cold migration",
                vm.hw_version.unwrap_or_default()
            ),
        ));
    }
    if warm && source.restricted {
        out.push(blocked(
            "source",
            "vmware_restricted",
            "the source has a read-only API license (free ESXi); use a cold migration",
        ));
    }
    if !warm && vm.power != Some(VmwarePower::PoweredOff) {
        out.push(blocked(
            "source",
            "vmware_powered_on",
            format!(
                "source VM {:?} is not powered off; power it off or use a warm migration",
                vm.name
            ),
        ));
    }
    if warm && cutover.shutdown == VmwareShutdown::Guest && !vm.tools_running() {
        out.push(warn(
            "source",
            "vmware_no_tools",
            "VMware Tools is not running; a guest shutdown at cutover will be refused (start Tools or use shutdown power-off)",
        ));
    }
    if vm.disks.len() > MAX_DISKS {
        out.push(blocked(
            "disks",
            "vmware_too_many_disks",
            format!(
                "{} disks are more than the {MAX_DISKS} a VM can have",
                vm.disks.len()
            ),
        ));
    }
    if vm.disks.is_empty() {
        out.push(blocked(
            "disks",
            "vmware_no_disks",
            "the source VM has no disks",
        ));
    }
    for (i, d) in vm.disks.iter().enumerate() {
        let at = format!("disks.{i}");
        if d.backing == "rdm" {
            out.push(blocked(
                &at,
                "vmware_rdm",
                format!("disk {:?} is a raw device mapping (RDM)", d.file),
            ));
        } else if d.backing != "flat" {
            out.push(blocked(
                &at,
                "vmware_disk_backing",
                format!(
                    "disk {:?} has a {} backing that cannot be read",
                    d.file, d.backing
                ),
            ));
        }
        if d.multi_writer {
            out.push(blocked(
                &at,
                "vmware_multi_writer",
                format!("disk {:?} is shared (multi-writer)", d.file),
            ));
        }
        if warm && d.independent() {
            out.push(blocked(
                &at,
                "vmware_independent_disk",
                format!(
                    "disk {:?} is independent ({}); snapshots skip it (use a cold migration)",
                    d.file, d.mode
                ),
            ));
        }
        let needs_vddk = d
            .datastore
            .as_deref()
            .and_then(|ds| inventory.datastore_named(ds))
            .is_some_and(|ds| ds.needs_vddk());
        if needs_vddk && transport.is_folder() {
            out.push(blocked(
                &at,
                "vmware_needs_vddk",
                format!(
                    "disk {:?} is on a {} datastore that cannot be read without the VDDK transport",
                    d.file,
                    d.datastore.as_deref().unwrap_or("?")
                ),
            ));
        }
    }
    out
}

/// 게스트 힌트(설계 §8.5 3·§11 — 모두 경고)
pub(crate) fn guest_hints(rules: &VmwareRules, vm: &InvVm, spec: &VmSpec) -> Vec<VmCheckProblem> {
    let mut out = Vec::new();
    let virtio = spec.disks.iter().any(|d| d.bus == DiskBus::VirtioBlk);
    if rhel_family(&vm.guest_id) && virtio {
        out.push(warn(
            "guest",
            "guest_initramfs",
            "RHEL-family guests may lack virtio drivers in the initramfs: run `dracut -f --add-drivers 'virtio_blk virtio_net virtio_pci'` (or `--no-hostonly`) in the guest before cutover",
        ));
    }
    if is_windows(&vm.guest_id) && rules.driver_iso.is_none() {
        out.push(warn(
            "guest",
            "guest_drivers",
            "Windows boots on SATA and e1000e; set rules.driver_iso (virtio-win) to install virtio drivers later",
        ));
    }
    let renamed = vm
        .nics
        .iter()
        .zip(&spec.nics)
        .any(|(src, dst)| src.adapter != format!("{:?}", dst.model).to_ascii_lowercase());
    if !vm.guest_ips.is_empty() && renamed {
        out.push(warn(
            "guest",
            "guest_nic_renamed",
            "the NIC model changes; static IP settings bound to the old adapter name may not apply in the guest",
        ));
    }
    out
}

/// 대상 VM 이름: 준 것, 없으면 원본 이름을 다듬은 것
pub(crate) fn target_name(given: Option<&str>, source: &str) -> Result<String, String> {
    let name = given.map_or_else(
        || orbivisor_types::vmware::name_part(source, 63),
        str::to_string,
    );
    validate_name(&name)
        .map(|()| name.clone())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vmware::inventory::{InvDatastore, InvDisk, InvNic, InvSnapshot};
    use orbivisor_types::vm::{Firmware, NicModel};
    use orbivisor_types::vmware::{VmwareDevices, VmwareMapping, VmwareOverrides};

    fn disk(key: i32, ds: &str) -> InvDisk {
        InvDisk {
            key,
            file: format!("[{ds}] web01/web01_{key}.vmdk"),
            current_file: format!("[{ds}] web01/web01_{key}.vmdk"),
            capacity: 4 << 20,
            mode: "persistent".into(),
            thin: true,
            backing: "flat".into(),
            multi_writer: false,
            datastore: Some(ds.into()),
            controller: "scsi".into(),
            bus: 0,
            unit: key as u32,
            change_id: None,
        }
    }

    fn vm() -> InvVm {
        InvVm {
            moref: "vm-1".into(),
            name: "web01".into(),
            path: "DC1/vm/Prod/Web/web01".into(),
            folder: Some("DC1/vm/Prod/Web".into()),
            datacenter: Some("DC1".into()),
            uuid: None,
            instance_uuid: Some("5003-aa".into()),
            host: Some("host-1".into()),
            host_name: Some("esx1".into()),
            cluster: None,
            power: Some(VmwarePower::PoweredOn),
            guest_id: "rhel9_64Guest".into(),
            guest_full_name: None,
            firmware: Firmware::Bios,
            hw_version: Some(19),
            cpus: 4,
            cores_per_socket: 2,
            memory_mib: 4096,
            disks: vec![disk(2000, "ds1"), disk(2001, "ds2")],
            nics: vec![
                InvNic {
                    key: 4000,
                    network: "VM Network".into(),
                    network_ref: Some("network-1".into()),
                    adapter: "vmxnet3".into(),
                    mac: Some("00:50:56:aa:bb:01".into()),
                    start_connected: Some(true),
                },
                InvNic {
                    key: 4001,
                    network: "dvpg-web".into(),
                    network_ref: Some("dvportgroup-12".into()),
                    adapter: "vmxnet3".into(),
                    mac: Some("00:50:56:aa:bb:02".into()),
                    start_connected: Some(true),
                },
            ],
            snapshots: vec![],
            current_snapshot: None,
            cbt: false,
            tools: Some("guestToolsRunning".into()),
            ft: false,
            template: false,
            annotation: None,
            guest_ips: BTreeMap::new(),
            committed_bytes: 0,
            resource_pool: None,
        }
    }

    fn item() -> VmwareVmSpec {
        VmwareVmSpec {
            migration: Uuid::nil(),
            source_ref: "vm-1".into(),
            instance_uuid: "5003-aa".into(),
            target_name: "web01".into(),
            overrides: None,
            node: None,
        }
    }

    fn rules() -> VmwareRules {
        VmwareRules {
            networks: vec![VmwareMapping {
                from: "moref:dvportgroup-12".into(),
                to: "web".into(),
            }],
            datastores: vec![VmwareMapping {
                from: "ds1".into(),
                to: "fast".into(),
            }],
            default_pool: Some("bulk".into()),
            ..VmwareRules::default()
        }
    }

    fn map(
        rules: &VmwareRules,
        item: &VmwareVmSpec,
        vm: &InvVm,
    ) -> Result<Mapped, Vec<VmCheckProblem>> {
        let nets = vec!["VM Network".to_string()];
        let raw = |p: &str| {
            if p == "bulk" {
                ImageFormat::Raw
            } else {
                ImageFormat::Qcow2
            }
        };
        map_vm(&MapInput {
            rules,
            item_id: Uuid::nil(),
            item,
            vm,
            networks: &nets,
            format: &raw,
        })
    }

    /// Tools IP 예약: SDN 네트워크의 NIC만, 그 MAC의 IP 중 서브넷 안의 것(체계마다 하나), 밖이면 경고. 주소가 있는 NIC·SDN
    /// 아닌 네트워크는 그대로.
    #[test]
    fn tools_ips_are_reserved_on_sdn_networks_inside_the_subnets() {
        let mut v = vm();
        v.guest_ips = BTreeMap::from([
            (
                "00:50:56:AA:BB:01".to_string(),
                vec![
                    "10.0.0.5".parse().unwrap(),
                    "10.0.0.6".parse().unwrap(),
                    "192.168.9.9".parse().unwrap(),
                ],
            ),
            (
                "00:50:56:aa:bb:02".to_string(),
                vec!["10.1.0.7".parse().unwrap()],
            ),
        ]);
        let mut m = map(&rules(), &item(), &v).unwrap();
        let net = |name: &str, spec: serde_json::Value| -> Network {
            orbivisor_types::resource::Resource::new(name, serde_json::from_value(spec).unwrap())
        };
        let networks = vec![
            net(
                "VM Network",
                serde_json::json!({ "sdn": {}, "subnets": [{ "cidr": "10.0.0.0/24" }] }),
            ),
            net("web", serde_json::json!({ "bridge": "br0" })),
        ];
        let warnings = reserve_ips(&mut m.spec.clone(), &v.guest_ips, &networks);
        assert_eq!(
            warnings,
            [
                "NIC 0: address 192.168.9.9 reported by VMware Tools is outside the subnets of network \"VM Network\"; not reserved"
            ]
        );
        reserve_ips(&mut m.spec, &v.guest_ips, &networks);
        let ips: Vec<Vec<IpAddr>> = m.spec.nics.iter().map(|n| n.ips.clone()).collect();
        assert_eq!(
            ips,
            vec![vec!["10.0.0.5".parse::<IpAddr>().unwrap()], vec![]]
        );
        // 이미 주소가 있으면 건드리지 않는다.
        assert!(reserve_ips(&mut m.spec, &v.guest_ips, &networks).is_empty());
        assert_eq!(m.spec.nics[0].ips.len(), 1);
    }

    #[test]
    fn maps_disks_to_pools_per_datastore_and_networks_by_rule_then_name() {
        let m = map(&rules(), &item(), &vm()).unwrap();
        let pools: Vec<_> = m
            .volumes
            .iter()
            .map(|v| (v.name.as_str(), v.spec.pool.as_str(), v.spec.format))
            .collect();
        assert_eq!(
            pools,
            [
                ("web01-disk0", "fast", ImageFormat::Qcow2),
                ("web01-disk1", "bulk", ImageFormat::Raw)
            ]
        );
        assert_eq!(
            m.volumes[0].labels[VMWARE_VM_LABEL],
            Uuid::nil().to_string()
        );
        assert_eq!(m.volumes[0].spec.size_bytes, 4 << 20);
        let nets: Vec<_> = m.spec.nics.iter().map(|n| n.network.as_str()).collect();
        // dvpg-web은 moref 규칙, VM Network는 같은 이름의 볼 수 있는 네트워크
        assert_eq!(nets, ["VM Network", "web"]);
        assert_eq!(m.spec.disks[0].volume, "web01-disk0");
        assert_eq!(m.spec.disks[0].bus, DiskBus::VirtioBlk);
        // 원본 MAC을 지키고(규칙 기본), 켜진 채 만들 계획이다(start_after).
        assert_eq!(m.spec.nics[0].mac.unwrap().to_string(), "00:50:56:aa:bb:01");
        assert_eq!(m.spec.power, PowerState::Running);
        assert_eq!((m.spec.cpu.sockets, m.spec.cpu.cores), (2, 2));
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn overrides_win_over_rules_and_rules_over_the_proposal() {
        let mut r = rules();
        r.devices = Some(VmwareDevices {
            disk_bus: DiskBus::Sata,
            nic_model: NicModel::E1000e,
        });
        r.keep_macs = false;
        let mut it = item();
        it.overrides = Some(VmwareOverrides {
            pools: vec!["other".into()],
            networks: BTreeMap::from([("VM Network".into(), "lan".into())]),
            nic_model: Some(NicModel::Virtio),
            memory_mib: Some(2048),
            ..Default::default()
        });
        let m = map(&r, &it, &vm()).unwrap();
        assert_eq!(m.volumes[0].spec.pool, "other");
        assert_eq!(m.volumes[1].spec.pool, "bulk");
        assert_eq!(m.spec.nics[0].network, "lan");
        assert_eq!(m.spec.nics[0].model, NicModel::Virtio);
        assert_eq!(m.spec.disks[0].bus, DiskBus::Sata);
        assert!(m.spec.nics[0].mac.is_none());
        assert_eq!(m.spec.memory_mib, 2048);
    }

    #[test]
    fn unmapped_networks_and_datastores_are_blocked_per_slot() {
        let mut r = rules();
        r.default_pool = None;
        r.networks.clear();
        let err = map(&r, &item(), &vm()).unwrap_err();
        let at: Vec<_> = err
            .iter()
            .map(|p| (p.at.as_str(), p.code.as_str()))
            .collect();
        assert_eq!(
            at,
            [("nics.1", "network_unmapped"), ("disks.1", "pool_unmapped")]
        );
        // 숨은(볼 수 없는) 같은 이름 네트워크는 제안이 고르지 않는다.
        let nets: Vec<String> = vec![];
        let fmt = |_: &str| ImageFormat::Qcow2;
        let err = map_vm(&MapInput {
            rules: &rules(),
            item_id: Uuid::nil(),
            item: &item(),
            vm: &vm(),
            networks: &nets,
            format: &fmt,
        })
        .unwrap_err();
        assert_eq!(err[0].at, "nics.0");
    }

    #[test]
    fn windows_gets_the_driver_iso_and_sata() {
        let mut v = vm();
        v.guest_id = "windows2019srv_64Guest".into();
        let mut r = rules();
        r.driver_iso = Some("virtio-win.iso".into());
        let m = map(&r, &item(), &v).unwrap();
        assert_eq!(m.spec.disks[0].bus, DiskBus::Sata);
        assert_eq!(m.spec.cdroms[0].iso.as_deref(), Some("virtio-win.iso"));
        let hints = guest_hints(&r, &v, &m.spec);
        assert!(hints.iter().all(|h| h.code != "guest_drivers"));
        let m = map(&rules(), &item(), &v).unwrap();
        assert!(m.spec.cdroms.is_empty());
        let hints = guest_hints(&rules(), &v, &m.spec);
        assert!(hints.iter().any(|h| h.code == "guest_drivers"));
    }

    #[test]
    fn folders_follow_overrides_then_rules_and_mirror_strips_the_datacenter() {
        let f = Uuid::from_u128(7);
        let g = Uuid::from_u128(8);
        let mut r = rules();
        r.folders = VmwareFolderRule::Mirror { under: Some(f) };
        assert_eq!(target_folder(&r, &item()), Some(f));
        let mut it = item();
        it.overrides = Some(VmwareOverrides {
            folder: Some(g),
            ..Default::default()
        });
        assert_eq!(target_folder(&r, &it), Some(g));
        assert_eq!(mirror_path(&vm()).as_deref(), Some("Prod/Web"));
        let mut top = vm();
        top.folder = Some("DC1/vm".into());
        assert_eq!(mirror_path(&top), None);
        assert_eq!(target_folder(&VmwareRules::default(), &item()), None);
    }

    fn inv() -> Inventory {
        Inventory {
            datastores: vec![InvDatastore {
                moref: "datastore-3".into(),
                name: "vsanDatastore".into(),
                kind: "vsan".into(),
                capacity: 0,
                free: 0,
                browser: None,
                datacenter: None,
            }],
            ..Default::default()
        }
    }

    fn codes(mode: VmwareMigrationMode, vm: &InvVm, ours: &[&str]) -> Vec<String> {
        let inv = inv();
        let ctx = SourceContext {
            mode,
            cutover: &VmwareCutoverPolicy::default(),
            status: &VmwareSourceStatus::default(),
            transport: &VmwareTransport::Folder,
            inventory: &inv,
        };
        source_problems(&ctx, &item(), vm, ours)
            .into_iter()
            .map(|p| format!("{}:{}", p.at, p.code))
            .collect()
    }

    #[test]
    fn source_checks_follow_the_mode() {
        let warm = VmwareMigrationMode::Warm;
        let cold = VmwareMigrationMode::Cold;
        assert!(codes(warm, &vm(), &[]).is_empty());
        assert_eq!(codes(cold, &vm(), &[]), ["source:vmware_powered_on"]);

        let mut v = vm();
        v.snapshots = vec![InvSnapshot {
            moref: "snapshot-1".into(),
            name: "before upgrade".into(),
            description: String::new(),
            id: None,
            created: None,
            parent: None,
        }];
        assert_eq!(codes(warm, &v, &[]), ["source:vmware_snapshots"]);
        // 우리 스냅샷은 사용자 스냅샷이 아니다.
        assert!(codes(warm, &v, &["snapshot-1"]).is_empty());

        let mut v = vm();
        v.ft = true;
        v.template = true;
        v.hw_version = Some(4);
        v.tools = None;
        v.disks[0].mode = "independent_persistent".into();
        v.disks[1].backing = "rdm".into();
        v.disks[1].multi_writer = true;
        assert_eq!(
            codes(warm, &v, &[]),
            [
                "source:vmware_ft",
                "source:vmware_template",
                "source:vmware_hw_version",
                "source:vmware_no_tools",
                "disks.0:vmware_independent_disk",
                "disks.1:vmware_rdm",
                "disks.1:vmware_multi_writer",
            ]
        );
        // 차가운 이전(꺼짐)은 스냅샷이 없어 독립 디스크·FT·템플릿·옛 하드웨어를 받는다.
        v.power = Some(VmwarePower::PoweredOff);
        assert_eq!(
            codes(cold, &v, &[]),
            ["disks.1:vmware_rdm", "disks.1:vmware_multi_writer"]
        );

        let mut v = vm();
        v.disks[0].datastore = Some("vsanDatastore".into());
        v.instance_uuid = Some("other".into());
        assert_eq!(
            codes(warm, &v, &[]),
            ["source:vmware_vm_replaced", "disks.0:vmware_needs_vddk"]
        );
        let vddk = VmwareTransport::Vddk(orbivisor_types::vmware::VddkTransport {
            libdir: None,
            thumbprint_sha1: String::new(),
        });
        let restricted = VmwareSourceStatus {
            restricted: true,
            ..Default::default()
        };
        let inv = inv();
        let ctx = SourceContext {
            mode: warm,
            cutover: &VmwareCutoverPolicy::default(),
            status: &restricted,
            transport: &vddk,
            inventory: &inv,
        };
        let got = source_problems(&ctx, &item(), &v, &[]);
        assert!(got.iter().any(|p| p.code == "vmware_restricted"));
        assert!(got.iter().all(|p| p.code != "vmware_needs_vddk"));
        assert!(got.iter().all(|p| p.level == VmCheckLevel::Error));
    }

    #[test]
    fn guest_hints_warn_about_initramfs_and_renamed_nics() {
        let mut v = vm();
        v.guest_ips = BTreeMap::from([(
            "00:50:56:aa:bb:01".into(),
            vec!["10.0.0.5".parse().unwrap()],
        )]);
        let m = map(&rules(), &item(), &v).unwrap();
        let hints: Vec<_> = guest_hints(&rules(), &v, &m.spec)
            .into_iter()
            .map(|h| h.code)
            .collect();
        assert_eq!(hints, ["guest_initramfs", "guest_nic_renamed"]);
        v.guest_id = "ubuntu64Guest".into();
        v.guest_ips.clear();
        assert!(guest_hints(&rules(), &v, &m.spec).is_empty());
    }

    #[test]
    fn target_names_default_to_the_trimmed_source_name() {
        assert_eq!(target_name(None, "Web 01 (prod)").unwrap(), "web-01-prod");
        assert_eq!(target_name(Some("db"), "x").unwrap(), "db");
        assert!(target_name(Some("Bad Name"), "x").is_err());
        assert_eq!(volume_name(&"v".repeat(63), 3).len(), 63);
    }
}
