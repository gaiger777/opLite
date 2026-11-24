//! OVF 서술자 쓰기(계획 72 §12.2, 순수). Orbivisor VM을 OVF 1.0 봉투(`vmw` 확장)로 적는다 — ovftool·vSphere·
//! VirtualBox가 모두 받는 판이다. `rasd`·`vssd` 요소는 OVF 1.0 스키마(CIM)의 차례(알파벳)대로 쓴다(ovftool은 차례가
//! 틀리면 스키마 오류를 낸다). 같은 저장소의 OVF 읽개([`super::ovf::parse_ovf`])가 쓴 것을 다시 읽어 같은 원본 VM이
//! 나오는지 시험이 본다.

use std::fmt::Write as _;

use opl_types::vm::{DiskBus, Firmware, MachineType, NicModel, VmSpec};

/// streamOptimized VMDK의 OVF 형식 URI
pub const VMDK_STREAM_OPTIMIZED: &str =
    "http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized";
/// VMware guestId를 모를 때 쓰는 값
pub const OTHER_GUEST: &str = "otherGuest64";
/// pvscsi 컨트롤러 하나에 두는 디스크 수(단위 번호 0~15에서 7은 컨트롤러 자리)
const SCSI_PER_CONTROLLER: usize = 15;
/// AHCI 컨트롤러 하나의 포트 수
const SATA_PER_CONTROLLER: usize = 30;

/// 내보낸 디스크 하나(VM 디스크 차례)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvfDisk {
    /// 패키지 안 파일 이름(`web-disk0.vmdk`)
    pub file: String,
    /// 파일 크기(바이트)
    pub size: u64,
    /// 가상 크기(바이트)
    pub capacity: u64,
    /// 쓴 데이터의 크기(알면 — 받는 쪽의 공간 어림)
    pub populated: Option<u64>,
    pub bus: DiskBus,
}

/// 서술자를 쓰는 데 드는 것
#[derive(Debug, Clone)]
pub struct OvfVm<'a> {
    /// VM 이름(`VirtualSystem`의 id·이름)
    pub name: &'a str,
    pub spec: &'a VmSpec,
    /// VM 디스크 차례의 내보낸 디스크
    pub disks: &'a [OvfDisk],
    /// VMware guestId(`vmw:osType`)
    pub guest_os: &'a str,
    /// `vmx-N`
    pub hw_version: u8,
    /// NIC MAC을 적는다(`addressType=manual`)
    pub keep_macs: bool,
}

/// XML 글·속성 값 이스케이프
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// guestId의 CIM 운영체제 번호(`OperatingSystemSection ovf:id` — VirtualBox처럼 `vmw:osType`을 모르는 도구가 본다)
pub fn cim_os_id(guest_os: &str) -> u32 {
    const LINUX: &[&str] = &[
        "linux", "ubuntu", "debian", "rhel", "centos", "sles", "suse", "oracle", "rocky", "alma",
        "fedora", "photon", "coreos", "asianux",
    ];
    let g = guest_os.to_ascii_lowercase();
    let bits64 = g.contains("64");
    if g.starts_with("windows") || g.starts_with("win") {
        // 103: Microsoft Windows Server 2008 64-Bit, 105: Microsoft Windows 7(읽개가 Windows로 아는 번호)
        return if bits64 { 103 } else { 105 };
    }
    if LINUX.iter().any(|l| g.contains(l)) {
        // 101: Linux 64-Bit, 36: LINUX
        return if bits64 { 101 } else { 36 };
    }
    1
}

/// OVF에 넣지 못하는 설정의 경고(VM 스펙에서). `guest_os_known`이 거짓이면 guestId를 몰라 [`OTHER_GUEST`]를 쓴다는 경고도.
pub fn export_warnings(spec: &VmSpec, guest_os_known: bool, keep_macs: bool) -> Vec<String> {
    let mut w = Vec::new();
    if !guest_os_known {
        w.push(format!(
            "the guest OS type is unknown; the OVF says {OTHER_GUEST} (set guest_os to a VMware guestId)"
        ));
    }
    if spec.machine == MachineType::Virt {
        w.push("the VM is an aarch64 VM; x86 VMware hosts cannot run it".into());
    }
    if spec.tpm {
        w.push("the vTPM and its state are not exported; add a vTPM on the target".into());
    }
    if !spec.cdroms.is_empty() {
        w.push("CD-ROM drives are not exported".into());
    }
    if spec.cloud_init.is_some() {
        w.push("the cloud-init seed is not exported".into());
    }
    if spec.numa.is_some() || spec.hugepages.is_some() || !spec.cpu.policy.is_shared() {
        w.push("NUMA, dedicated CPU and hugepages settings are not exported".into());
    }
    if spec.cpu.max_vcpus.is_some() || spec.max_memory_mib.is_some() {
        w.push("CPU and memory hotplug limits are not exported".into());
    }
    if spec.ha.is_some() || spec.placement.is_some() || !spec.replication.is_empty() {
        w.push("HA, placement and replication settings are not exported".into());
    }
    if spec
        .nics
        .iter()
        .any(|n| !n.security_groups.is_empty() || !n.ips.is_empty() || n.qos.is_some())
    {
        w.push("security groups, SDN addresses and NIC rate limits are not exported".into());
    }
    if keep_macs && spec.nics.iter().any(|n| n.mac.is_none()) {
        w.push("NICs without a fixed MAC address get a new one on the target".into());
    }
    w
}

/// 컨트롤러 하나: (InstanceID, 종류, 같은 종류 안 번호)
struct Controller {
    instance: u32,
    sata: bool,
    address: usize,
}

/// OVF 서술자. 디스크는 VM 차례대로 virtio-blk → pvscsi(`VirtualSCSI`, Linux 커널에 드라이버가 있다), SATA → AHCI에 둔다.
/// NIC는 virtio → `VmxNet3`, e1000e → `E1000e`.
pub fn write_ovf(vm: &OvfVm<'_>) -> String {
    let spec = vm.spec;
    let name = esc(vm.name);
    let mut x = String::new();
    x.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    x.push('\n');
    x.push_str(concat!(
        r#"<Envelope ovf:version="1.0" xml:lang="en-US" vmw:buildId="orbivisor""#,
        r#" xmlns="http://schemas.dmtf.org/ovf/envelope/1""#,
        r#" xmlns:cim="http://schemas.dmtf.org/wbem/wscim/1/common""#,
        r#" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/1""#,
        r#" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData""#,
        r#" xmlns:vmw="http://www.vmware.com/schema/ovf""#,
        r#" xmlns:vssd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_VirtualSystemSettingData""#,
        r#" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#,
        "\n"
    ));

    // References
    x.push_str("  <References>\n");
    for (i, d) in vm.disks.iter().enumerate() {
        let _ = writeln!(
            x,
            r#"    <File ovf:href="{}" ovf:id="file{}" ovf:size="{}"/>"#,
            esc(&d.file),
            i + 1,
            d.size
        );
    }
    x.push_str("  </References>\n");

    // DiskSection
    x.push_str("  <DiskSection>\n    <Info>Virtual disk information</Info>\n");
    for (i, d) in vm.disks.iter().enumerate() {
        let populated = d
            .populated
            .map(|p| format!(r#" ovf:populatedSize="{p}""#))
            .unwrap_or_default();
        let _ = writeln!(
            x,
            r#"    <Disk ovf:capacity="{}" ovf:capacityAllocationUnits="byte" ovf:diskId="vmdisk{n}" ovf:fileRef="file{n}" ovf:format="{VMDK_STREAM_OPTIMIZED}"{populated}/>"#,
            d.capacity,
            n = i + 1,
        );
    }
    x.push_str("  </DiskSection>\n");

    // NetworkSection(나온 차례, 겹치지 않게)
    let mut networks: Vec<&str> = Vec::new();
    for n in &spec.nics {
        if !networks.contains(&n.network.as_str()) {
            networks.push(&n.network);
        }
    }
    x.push_str("  <NetworkSection>\n    <Info>The list of logical networks</Info>\n");
    for n in &networks {
        let n = esc(n);
        let _ = writeln!(
            x,
            "    <Network ovf:name=\"{n}\">\n      <Description>The {n} network</Description>\n    </Network>"
        );
    }
    x.push_str("  </NetworkSection>\n");

    // VirtualSystem
    let _ = writeln!(x, r#"  <VirtualSystem ovf:id="{name}">"#);
    x.push_str("    <Info>A virtual machine</Info>\n");
    let _ = writeln!(x, "    <Name>{name}</Name>");
    let _ = writeln!(
        x,
        "    <OperatingSystemSection ovf:id=\"{}\" vmw:osType=\"{}\">\n      <Info>The kind of installed guest operating system</Info>\n    </OperatingSystemSection>",
        cim_os_id(vm.guest_os),
        esc(vm.guest_os)
    );
    x.push_str("    <VirtualHardwareSection>\n      <Info>Virtual hardware requirements</Info>\n");
    let _ = writeln!(
        x,
        "      <System>\n        <vssd:ElementName>Virtual Hardware Family</vssd:ElementName>\n        <vssd:InstanceID>0</vssd:InstanceID>\n        <vssd:VirtualSystemIdentifier>{name}</vssd:VirtualSystemIdentifier>\n        <vssd:VirtualSystemType>vmx-{:02}</vssd:VirtualSystemType>\n      </System>",
        vm.hw_version
    );

    let vcpus = spec.cpu.vcpus();
    let _ = writeln!(
        x,
        "      <Item>\n        <rasd:AllocationUnits>hertz * 10^6</rasd:AllocationUnits>\n        <rasd:Description>Number of Virtual CPUs</rasd:Description>\n        <rasd:ElementName>{vcpus} virtual CPU(s)</rasd:ElementName>\n        <rasd:InstanceID>1</rasd:InstanceID>\n        <rasd:ResourceType>3</rasd:ResourceType>\n        <rasd:VirtualQuantity>{vcpus}</rasd:VirtualQuantity>\n        <vmw:CoresPerSocket ovf:required=\"false\">{}</vmw:CoresPerSocket>\n      </Item>",
        spec.cpu.cores
    );
    let _ = writeln!(
        x,
        "      <Item>\n        <rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits>\n        <rasd:Description>Memory Size</rasd:Description>\n        <rasd:ElementName>{m}MB of memory</rasd:ElementName>\n        <rasd:InstanceID>2</rasd:InstanceID>\n        <rasd:ResourceType>4</rasd:ResourceType>\n        <rasd:VirtualQuantity>{m}</rasd:VirtualQuantity>\n      </Item>",
        m = spec.memory_mib
    );

    // 컨트롤러: 디스크가 처음 쓰는 종류의 차례로, 한 종류 안에서는 다 차면 다음 것
    let mut next_instance = 3u32;
    let mut controllers: Vec<Controller> = Vec::new();
    // 디스크마다 (컨트롤러 InstanceID, 단위 번호)
    let mut placed: Vec<(u32, usize)> = Vec::new();
    let (mut scsi_n, mut sata_n) = (0usize, 0usize);
    for d in vm.disks {
        let sata = d.bus == DiskBus::Sata;
        let (n, per) = match sata {
            true => (&mut sata_n, SATA_PER_CONTROLLER),
            false => (&mut scsi_n, SCSI_PER_CONTROLLER),
        };
        let address = *n / per;
        let slot = *n % per;
        *n += 1;
        let instance = match controllers
            .iter()
            .find(|c| c.sata == sata && c.address == address)
        {
            Some(c) => c.instance,
            None => {
                let instance = next_instance;
                next_instance += 1;
                controllers.push(Controller {
                    instance,
                    sata,
                    address,
                });
                instance
            }
        };
        // SCSI 단위 번호 7은 컨트롤러 자신이다.
        let unit = if !sata && slot >= 7 { slot + 1 } else { slot };
        placed.push((instance, unit));
    }
    for c in &controllers {
        let (label, sub, kind) = match c.sata {
            true => ("SATA Controller", "vmware.sata.ahci", 20),
            false => ("SCSI Controller", "VirtualSCSI", 6),
        };
        let _ = writeln!(
            x,
            "      <Item>\n        <rasd:Address>{a}</rasd:Address>\n        <rasd:Description>{label}</rasd:Description>\n        <rasd:ElementName>{label} {a}</rasd:ElementName>\n        <rasd:InstanceID>{}</rasd:InstanceID>\n        <rasd:ResourceSubType>{sub}</rasd:ResourceSubType>\n        <rasd:ResourceType>{kind}</rasd:ResourceType>\n      </Item>",
            c.instance,
            a = c.address,
        );
    }
    for (i, (parent, unit)) in placed.iter().enumerate() {
        let instance = next_instance;
        next_instance += 1;
        let _ = writeln!(
            x,
            "      <Item>\n        <rasd:AddressOnParent>{unit}</rasd:AddressOnParent>\n        <rasd:ElementName>Hard disk {}</rasd:ElementName>\n        <rasd:HostResource>ovf:/disk/vmdisk{}</rasd:HostResource>\n        <rasd:InstanceID>{instance}</rasd:InstanceID>\n        <rasd:Parent>{parent}</rasd:Parent>\n        <rasd:ResourceType>17</rasd:ResourceType>\n      </Item>",
            i + 1,
            i + 1,
        );
    }
    for (i, nic) in spec.nics.iter().enumerate() {
        let instance = next_instance;
        next_instance += 1;
        let adapter = match nic.model {
            NicModel::Virtio => "VmxNet3",
            NicModel::E1000e => "E1000e",
        };
        let mac = nic.mac.filter(|_| vm.keep_macs);
        let address = mac
            .map(|m| format!("        <rasd:Address>{m}</rasd:Address>\n"))
            .unwrap_or_default();
        let network = esc(&nic.network);
        let manual = match mac {
            Some(_) => {
                "\n        <vmw:Config ovf:required=\"false\" vmw:key=\"addressType\" vmw:value=\"manual\"/>"
            }
            None => "",
        };
        let _ = writeln!(
            x,
            "      <Item>\n{address}        <rasd:AutomaticAllocation>true</rasd:AutomaticAllocation>\n        <rasd:Connection>{network}</rasd:Connection>\n        <rasd:Description>{adapter} ethernet adapter on &quot;{network}&quot;</rasd:Description>\n        <rasd:ElementName>Network adapter {}</rasd:ElementName>\n        <rasd:InstanceID>{instance}</rasd:InstanceID>\n        <rasd:ResourceSubType>{adapter}</rasd:ResourceSubType>\n        <rasd:ResourceType>10</rasd:ResourceType>{manual}\n      </Item>",
            i + 1,
        );
    }
    let firmware = match spec.firmware {
        Firmware::Uefi => "efi",
        Firmware::Bios => "bios",
    };
    let _ = writeln!(
        x,
        r#"      <vmw:Config ovf:required="false" vmw:key="firmware" vmw:value="{firmware}"/>"#
    );
    if spec.firmware == Firmware::Uefi && spec.secure_boot {
        x.push_str(
            "      <vmw:Config ovf:required=\"false\" vmw:key=\"bootOptions.efiSecureBootEnabled\" vmw:value=\"true\"/>\n",
        );
    }
    x.push_str("    </VirtualHardwareSection>\n  </VirtualSystem>\n</Envelope>\n");
    x
}

/// `.mf` 매니페스트: 파일마다 `SHA256(name)= hex` 한 줄(ovftool 꼴)
pub fn write_manifest<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::new();
    for (name, sha256) in files {
        let _ = writeln!(out, "SHA256({name})= {sha256}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm_imports::ova::{Digest, parse_manifest};
    use crate::vm_imports::ovf::parse_ovf;
    use opl_types::vm::{DiskSpec, MacAddr, NicSpec};
    use opl_types::vm_import::{SourceDisk, SourceNic, SourceVm};

    const GIB: u64 = 1 << 30;

    fn spec(disks: &[DiskBus], nics: &[(&str, NicModel, Option<&str>)]) -> VmSpec {
        let mut spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "sockets": 2, "cores": 2 },
            "memory_mib": 4096,
            "firmware": "uefi",
            "machine": "q35",
            "disks": [],
            "nics": [],
        }))
        .unwrap();
        spec.disks = disks
            .iter()
            .enumerate()
            .map(|(i, bus)| DiskSpec {
                volume: format!("web-disk{i}"),
                bus: *bus,
                boot_order: None,
            })
            .collect();
        spec.nics = nics
            .iter()
            .map(|(network, model, mac)| {
                let mut nic: NicSpec = serde_json::from_value(
                    serde_json::json!({ "network": network, "model": "virtio" }),
                )
                .unwrap();
                nic.model = *model;
                nic.mac = mac.map(|m| m.parse::<MacAddr>().unwrap());
                nic
            })
            .collect();
        spec
    }

    fn disks(buses: &[DiskBus]) -> Vec<OvfDisk> {
        buses
            .iter()
            .enumerate()
            .map(|(i, bus)| OvfDisk {
                file: format!("web-disk{i}.vmdk"),
                size: 1000 + i as u64,
                capacity: (i as u64 + 1) * GIB,
                populated: (i == 0).then_some(12345),
                bus: *bus,
            })
            .collect()
    }

    /// 쓴 서술자를 계획 29의 읽개로 다시 읽으면 같은 원본 VM이 나오고 경고가 없다(디스크 차례·크기·컨트롤러, NIC
    /// 네트워크·어댑터·MAC, CPU·메모리·펌웨어·guestId).
    #[test]
    fn round_trips_through_the_ovf_reader() {
        let s = spec(
            &[DiskBus::VirtioBlk, DiskBus::VirtioBlk],
            &[
                ("lan", NicModel::Virtio, Some("52:54:00:12:34:56")),
                ("dmz", NicModel::E1000e, Some("52:54:00:ab:cd:ef")),
                ("lan", NicModel::Virtio, None),
            ],
        );
        let d = disks(&[DiskBus::VirtioBlk, DiskBus::VirtioBlk]);
        let xml = write_ovf(&OvfVm {
            name: "web",
            spec: &s,
            disks: &d,
            guest_os: "ubuntu64Guest",
            hw_version: 14,
            keep_macs: true,
        });
        let parsed = parse_ovf(&xml).unwrap();
        assert_eq!(parsed.warnings, Vec::<String>::new());
        assert_eq!(parsed.files, ["web-disk0.vmdk", "web-disk1.vmdk"]);
        assert_eq!(
            parsed.vm,
            SourceVm {
                name: "web".into(),
                guest_os: "ubuntu64Guest".into(),
                firmware: Firmware::Uefi,
                vcpus: 4,
                cores_per_socket: 2,
                memory_mib: 4096,
                disks: vec![
                    SourceDisk {
                        name: "disk0".into(),
                        file: "web-disk0.vmdk".into(),
                        capacity_bytes: GIB,
                        controller: "scsi".into(),
                    },
                    SourceDisk {
                        name: "disk1".into(),
                        file: "web-disk1.vmdk".into(),
                        capacity_bytes: 2 * GIB,
                        controller: "scsi".into(),
                    },
                ],
                nics: vec![
                    SourceNic {
                        network: "lan".into(),
                        adapter: "vmxnet3".into(),
                        mac: Some("52:54:00:12:34:56".parse().unwrap()),
                    },
                    SourceNic {
                        network: "dmz".into(),
                        adapter: "e1000e".into(),
                        mac: Some("52:54:00:ab:cd:ef".parse().unwrap()),
                    },
                    SourceNic {
                        network: "lan".into(),
                        adapter: "vmxnet3".into(),
                        mac: None,
                    },
                ],
            }
        );
        // MAC을 쓰지 않으면 주소가 없다.
        let xml = write_ovf(&OvfVm {
            name: "web",
            spec: &s,
            disks: &d,
            guest_os: "ubuntu64Guest",
            hw_version: 14,
            keep_macs: false,
        });
        let parsed = parse_ovf(&xml).unwrap();
        assert!(parsed.vm.nics.iter().all(|n| n.mac.is_none()));
        assert!(!xml.contains("addressType"));
    }

    /// VMware·OVF 도구가 보는 모양: OVF 1.0 봉투와 네임스페이스, `vmx-NN`, streamOptimized 형식, pvscsi·AHCI, vmxnet3,
    /// efi와 Secure Boot, `rasd` 요소의 스키마 차례(알파벳), 쓰는 InstanceID가 겹치지 않음, 부모가 있는 컨트롤러.
    #[test]
    fn writes_what_vmware_expects() {
        let mut s = spec(
            &[DiskBus::Sata, DiskBus::VirtioBlk],
            &[("lan", NicModel::Virtio, Some("52:54:00:12:34:56"))],
        );
        s.secure_boot = true;
        let d = disks(&[DiskBus::Sata, DiskBus::VirtioBlk]);
        let xml = write_ovf(&OvfVm {
            name: "web",
            spec: &s,
            disks: &d,
            guest_os: "windows2019srv_64Guest",
            hw_version: 19,
            keep_macs: true,
        });
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let env = doc.root_element();
        assert_eq!(env.tag_name().name(), "Envelope");
        assert_eq!(
            env.tag_name().namespace(),
            Some("http://schemas.dmtf.org/ovf/envelope/1")
        );
        for (prefix, uri) in [
            ("ovf", "http://schemas.dmtf.org/ovf/envelope/1"),
            (
                "rasd",
                "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData",
            ),
            (
                "vssd",
                "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_VirtualSystemSettingData",
            ),
            ("vmw", "http://www.vmware.com/schema/ovf"),
        ] {
            assert_eq!(
                env.lookup_namespace_uri(Some(prefix)),
                Some(uri),
                "{prefix}"
            );
        }
        // 섹션 차례: References → DiskSection → NetworkSection → VirtualSystem
        let sections: Vec<&str> = env
            .children()
            .filter(|n| n.is_element())
            .map(|n| n.tag_name().name())
            .collect();
        assert_eq!(
            sections,
            [
                "References",
                "DiskSection",
                "NetworkSection",
                "VirtualSystem"
            ]
        );
        assert!(xml.contains("<vssd:VirtualSystemType>vmx-19</vssd:VirtualSystemType>"));
        assert!(xml.contains(&format!(r#"ovf:format="{VMDK_STREAM_OPTIMIZED}""#)));
        assert!(xml.contains(r#"ovf:populatedSize="12345""#));
        assert!(xml.contains("<rasd:ResourceSubType>vmware.sata.ahci</rasd:ResourceSubType>"));
        assert!(xml.contains("<rasd:ResourceSubType>VirtualSCSI</rasd:ResourceSubType>"));
        assert!(xml.contains("<rasd:ResourceSubType>VmxNet3</rasd:ResourceSubType>"));
        assert!(xml.contains(r#"vmw:key="firmware" vmw:value="efi""#));
        assert!(xml.contains(r#"vmw:key="bootOptions.efiSecureBootEnabled" vmw:value="true""#));
        assert!(xml.contains(r#"vmw:key="addressType" vmw:value="manual""#));
        assert!(xml.contains(
            r#"<OperatingSystemSection ovf:id="103" vmw:osType="windows2019srv_64Guest">"#
        ));

        let rasd =
            "http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData";
        let mut ids = std::collections::HashSet::new();
        for item in doc.descendants().filter(|n| n.has_tag_name("Item")) {
            let names: Vec<&str> = item
                .children()
                .filter(|n| n.is_element() && n.tag_name().namespace() == Some(rasd))
                .map(|n| n.tag_name().name())
                .collect();
            let mut sorted = names.clone();
            sorted.sort();
            assert_eq!(names, sorted, "rasd elements out of schema order");
            // 확장 요소(vmw)는 rasd 뒤에만
            let first_ext = item
                .children()
                .filter(|n| n.is_element())
                .position(|n| n.tag_name().namespace() != Some(rasd));
            if let Some(p) = first_ext {
                assert!(
                    item.children()
                        .filter(|n| n.is_element())
                        .skip(p)
                        .all(|n| n.tag_name().namespace() != Some(rasd))
                );
            }
            let id = item
                .children()
                .find(|n| n.has_tag_name((rasd, "InstanceID")))
                .and_then(|n| n.text())
                .unwrap();
            assert!(ids.insert(id.to_string()), "duplicate InstanceID {id}");
            if let Some(parent) = item
                .children()
                .find(|n| n.has_tag_name((rasd, "Parent")))
                .and_then(|n| n.text())
            {
                assert!(ids.contains(parent), "parent {parent} not before the disk");
            }
        }
        // 디스크 차례는 컨트롤러 종류의 첫 차례 — SATA 디스크가 첫째(부팅 디스크)로 남는다.
        let parsed = parse_ovf(&xml).unwrap();
        assert_eq!(
            parsed
                .vm
                .disks
                .iter()
                .map(|d| (d.file.as_str(), d.controller.as_str()))
                .collect::<Vec<_>>(),
            [("web-disk0.vmdk", "sata"), ("web-disk1.vmdk", "scsi")]
        );
        assert_eq!(parsed.vm.guest_os, "windows2019srv_64Guest");
        // 컨트롤러가 둘이라는 읽개의 알림뿐이다.
        assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
    }

    /// pvscsi 하나에 15개 — 16번째 디스크는 둘째 컨트롤러에, 단위 번호 7은 건너뛴다. BIOS VM은 `firmware=bios`.
    #[test]
    fn fills_controllers_and_skips_the_scsi_controller_unit() {
        let buses = vec![DiskBus::VirtioBlk; 16];
        let mut s = spec(&buses, &[]);
        s.firmware = Firmware::Bios;
        let d = disks(&buses);
        let xml = write_ovf(&OvfVm {
            name: "big",
            spec: &s,
            disks: &d,
            guest_os: OTHER_GUEST,
            hw_version: 14,
            keep_macs: false,
        });
        assert_eq!(
            xml.matches("<rasd:ResourceType>6</rasd:ResourceType>")
                .count(),
            2
        );
        assert!(!xml.contains("<rasd:AddressOnParent>7</rasd:AddressOnParent>"));
        assert_eq!(
            xml.matches("<rasd:AddressOnParent>15</rasd:AddressOnParent>")
                .count(),
            1
        );
        assert!(xml.contains(r#"vmw:value="bios""#));
        let parsed = parse_ovf(&xml).unwrap();
        assert_eq!(parsed.vm.firmware, Firmware::Bios);
        assert_eq!(
            parsed
                .vm
                .disks
                .iter()
                .map(|d| d.file.clone())
                .collect::<Vec<_>>(),
            (0..16)
                .map(|i| format!("web-disk{i}.vmdk"))
                .collect::<Vec<_>>()
        );
        assert!(
            parsed
                .warnings
                .iter()
                .any(|w| w.contains("more than one controller"))
        );
        assert_eq!(parsed.vm.nics, vec![]);
    }

    #[test]
    fn escapes_text_and_maps_os_ids() {
        assert_eq!(esc(r#"a<b>&"c'"#), "a&lt;b&gt;&amp;&quot;c&apos;");
        assert_eq!(cim_os_id("ubuntu64Guest"), 101);
        assert_eq!(cim_os_id("rhel9_64Guest"), 101);
        assert_eq!(cim_os_id("otherLinuxGuest"), 36);
        assert_eq!(cim_os_id("windows9_64Guest"), 103);
        assert_eq!(cim_os_id("winXPProGuest"), 105);
        assert_eq!(cim_os_id(OTHER_GUEST), 1);
    }

    #[test]
    fn warns_about_what_is_left_out() {
        let mut s = spec(&[DiskBus::VirtioBlk], &[("lan", NicModel::Virtio, None)]);
        assert_eq!(export_warnings(&s, true, false), Vec::<String>::new());
        s.tpm = true;
        s.cdroms = vec![Default::default()];
        s.machine = MachineType::Virt;
        s.nics[0].security_groups = vec!["web".into()];
        let w = export_warnings(&s, false, true);
        for needle in [
            "guest OS type is unknown",
            "aarch64",
            "vTPM",
            "CD-ROM",
            "security groups",
            "new one on the target",
        ] {
            assert!(w.iter().any(|x| x.contains(needle)), "{needle}: {w:?}");
        }
    }

    /// 매니페스트는 계획 29의 `.mf` 읽개가 SHA-256으로 읽는 꼴이다.
    #[test]
    fn manifest_reads_back() {
        let a = "ab".repeat(32);
        let b = "cd".repeat(32);
        let text = write_manifest([("web.ovf", a.as_str()), ("web-disk0.vmdk", b.as_str())]);
        assert_eq!(
            text,
            format!("SHA256(web.ovf)= {a}\nSHA256(web-disk0.vmdk)= {b}\n")
        );
        let mf = parse_manifest(&text).unwrap();
        assert_eq!(mf.digests["web.ovf"], Digest::Sha256(a));
        assert_eq!(mf.digests["web-disk0.vmdk"], Digest::Sha256(b));
        assert!(mf.unsupported.is_empty());
    }
}
