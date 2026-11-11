//! OVF(XML) 해석. 원본 VM의 CPU·메모리·펌웨어·디스크·NIC를 읽는다. OVF 1.x·2.x와 만든 도구마다
//! 네임스페이스 접두어가 제각각이라 요소·속성은 로컬 이름으로 비교한다. DTD는 거절한다(roxmltree 기본값).

use std::collections::HashMap;

use opl_types::vm::{Firmware, MAX_NICS, MacAddr};
use opl_types::vm_import::{SourceDisk, SourceNic, SourceVm};
use roxmltree::{Document, Node};

use super::ova::OvaError;

const MIB: u64 = 1 << 20;
/// 가져올 수 있는 디스크의 최대 개수
const MAX_DISKS: usize = 32;

/// Windows인 CIM 운영체제 번호(`OperatingSystemSection`의 `ovf:id`). `vmw:osType`이 없을 때만 쓴다.
const WINDOWS_CIM_IDS: &[u32] = &[
    16, 17, 18, 19, 58, 63, 67, 69, 70, 71, 72, 73, 74, 75, 76, 77, 103, 105, 113, 114, 115, 116,
];

/// 해석한 OVF
#[derive(Debug, Clone)]
pub struct Ovf {
    pub vm: SourceVm,
    /// 가져오지 않는 장치
    pub warnings: Vec<String>,
    /// `References`의 모든 파일(OVA 안에 있어야 한다)
    pub files: Vec<String>,
}

fn invalid(message: impl Into<String>) -> OvaError {
    OvaError::invalid(message)
}

fn is(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn children<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children().filter(move |n| is(*n, name))
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &'static str) -> Option<Node<'a, 'i>> {
    children(node, name).next()
}

/// 자식 요소의 글(앞뒤 공백 뺌)
fn text<'a>(node: Node<'a, '_>, name: &'static str) -> Option<&'a str> {
    child(node, name).and_then(|n| n.text()).map(str::trim)
}

fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value())
}

/// `byte * 2^30`, `byte * 1024`, `MegaBytes` 같은 단위의 바이트 수
fn units(text: &str) -> Option<u64> {
    let t: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match t.as_str() {
        "byte" | "bytes" => Some(1),
        "kilobytes" | "kb" => Some(1 << 10),
        "megabytes" | "mb" => Some(1 << 20),
        "gigabytes" | "gb" => Some(1 << 30),
        _ => {
            let rest = t.strip_prefix("byte*")?;
            match rest.split_once('^') {
                Some((base, exp)) => base.parse::<u64>().ok()?.checked_pow(exp.parse().ok()?),
                None => rest.parse().ok(),
            }
        }
    }
}

/// `quantity`에 `unit`을 곱한 바이트 수
fn bytes(quantity: &str, unit: &str) -> Option<u64> {
    quantity
        .trim()
        .parse::<u64>()
        .ok()?
        .checked_mul(units(unit)?)
}

/// 디스크 컨트롤러
struct Controller<'a> {
    id: &'a str,
    /// `ide`, `sata`, `scsi`, `nvme`
    kind: &'static str,
    /// `rasd:Address`(같은 종류 컨트롤러의 번호)
    address: Option<u32>,
}

/// 디스크 항목(컨트롤러 순서를 정하기 전)
struct DiskItem<'a> {
    parent: &'a str,
    address: u32,
    disk_id: &'a str,
}

/// OVF를 해석한다. 지원하지 않는 모양(여러 VM, 압축·분할 파일, VMDK가 아닌 디스크)은 오류다.
pub fn parse_ovf(xml: &str) -> Result<Ovf, OvaError> {
    let doc = Document::parse(xml).map_err(|e| invalid(format!("invalid OVF XML: {e}")))?;
    let envelope = doc.root_element();
    if !is(envelope, "Envelope") {
        return Err(invalid("the .ovf file is not an OVF envelope"));
    }
    let single = || invalid("only single-VM OVA files are supported");
    if doc.descendants().any(|n| is(n, "VirtualSystemCollection")) {
        return Err(single());
    }
    let system = match children(envelope, "VirtualSystem").collect::<Vec<_>>()[..] {
        [system] => system,
        [] => return Err(invalid("the OVF has no VirtualSystem")),
        _ => return Err(single()),
    };

    // 파일 id → 이름
    let mut file_names = HashMap::new();
    let mut files = Vec::new();
    for file in child(envelope, "References")
        .into_iter()
        .flat_map(|r| children(r, "File"))
    {
        let href = attr(file, "href").ok_or_else(|| invalid("a File has no href"))?;
        if attr(file, "compression").is_some() {
            return Err(invalid(
                "compressed disk files (ovf:compression) are not supported",
            ));
        }
        if attr(file, "chunkSize").is_some() {
            return Err(invalid(
                "split disk files (ovf:chunkSize) are not supported",
            ));
        }
        if let Some(id) = attr(file, "id") {
            file_names.insert(id, href);
        }
        files.push(href.to_string());
    }

    // 디스크 id → (파일 이름, 크기)
    let mut disk_files = HashMap::new();
    for disk in child(envelope, "DiskSection")
        .into_iter()
        .flat_map(|s| children(s, "Disk"))
    {
        let id = attr(disk, "diskId").ok_or_else(|| invalid("a Disk has no diskId"))?;
        let file_ref =
            attr(disk, "fileRef").ok_or_else(|| invalid(format!("disk {id:?} has no file")))?;
        let file = file_names.get(file_ref).ok_or_else(|| {
            invalid(format!(
                "disk {id:?} refers to an unknown file {file_ref:?}"
            ))
        })?;
        if attr(disk, "format").is_some_and(|f| !f.to_ascii_lowercase().contains("vmdk")) {
            return Err(invalid(format!("disk {id:?} is not a VMDK")));
        }
        let capacity = attr(disk, "capacity")
            .and_then(|c| bytes(c, attr(disk, "capacityAllocationUnits").unwrap_or("byte")))
            .ok_or_else(|| invalid(format!("invalid capacity for disk {id:?}")))?;
        disk_files.insert(id, (*file, capacity));
    }

    let name = text(system, "Name")
        .filter(|n| !n.is_empty())
        .or_else(|| attr(system, "id"))
        .unwrap_or_default()
        .to_string();
    let guest_os = child(system, "OperatingSystemSection")
        .map(guest_os)
        .unwrap_or_else(|| "other".into());

    let hardware = child(system, "VirtualHardwareSection")
        .ok_or_else(|| invalid("the OVF has no VirtualHardwareSection"))?;
    let firmware = match children(hardware, "Config")
        .any(|c| attr(c, "key") == Some("firmware") && attr(c, "value") == Some("efi"))
    {
        true => Firmware::Uefi,
        false => Firmware::Bios,
    };

    let mut cpu = None;
    let mut memory_mib = None;
    let mut controllers = Vec::new();
    let mut disk_items = Vec::new();
    let mut nics = Vec::new();
    let mut warnings = Vec::new();
    let mut warn = |w: String| {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    };
    for item in hardware
        .children()
        .filter(|n| is(*n, "Item") || is(*n, "StorageItem") || is(*n, "EthernetPortItem"))
    {
        let resource_type: u32 = text(item, "ResourceType")
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| invalid("an Item has no valid ResourceType"))?;
        let sub_type = text(item, "ResourceSubType").unwrap_or_default();
        let element = text(item, "ElementName").unwrap_or_default();
        match resource_type {
            3 => {
                let vcpus = text(item, "VirtualQuantity")
                    .and_then(|q| q.parse().ok())
                    .ok_or_else(|| invalid("invalid CPU count"))?;
                let cores = text(item, "CoresPerSocket")
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(1);
                cpu = Some((vcpus, cores));
            }
            4 => {
                let size = text(item, "VirtualQuantity")
                    .and_then(|q| bytes(q, text(item, "AllocationUnits").unwrap_or("byte * 2^20")))
                    .ok_or_else(|| invalid("invalid memory size"))?;
                memory_mib = Some(size / MIB);
            }
            5 | 6 | 20 => {
                let kind = match resource_type {
                    5 => "ide",
                    6 => "scsi",
                    _ if sub_type.to_ascii_lowercase().contains("nvme") => "nvme",
                    _ => "sata",
                };
                controllers.push(Controller {
                    id: text(item, "InstanceID").unwrap_or_default(),
                    kind,
                    address: text(item, "Address").and_then(|a| a.parse().ok()),
                });
            }
            17 => {
                let resource = text(item, "HostResource")
                    .ok_or_else(|| invalid(format!("disk {element:?} has no HostResource")))?;
                let disk_id = resource
                    .rsplit_once("/disk/")
                    .map(|(_, id)| id)
                    .ok_or_else(|| invalid(format!("unsupported disk reference {resource:?}")))?;
                disk_items.push(DiskItem {
                    parent: text(item, "Parent").unwrap_or_default(),
                    address: text(item, "AddressOnParent")
                        .and_then(|a| a.parse().ok())
                        .unwrap_or(0),
                    disk_id,
                });
            }
            10 => {
                let network = text(item, "Connection").ok_or_else(|| {
                    invalid(format!("network adapter {element:?} has no network"))
                })?;
                nics.push(SourceNic {
                    network: network.to_string(),
                    adapter: sub_type.to_ascii_lowercase(),
                    mac: text(item, "Address").and_then(|a| a.parse::<MacAddr>().ok()),
                });
            }
            14 => warn("floppy drive is not imported".into()),
            15 | 16 => warn("CD-ROM drive is not imported".into()),
            23 => warn("USB controller is not imported".into()),
            // 화면은 Orbivisor가 따로 붙이고, VMCI는 VMware 전용이다
            24 => {}
            1 if sub_type == "vmware.vmci" => {}
            _ if !element.is_empty() => warn(format!("{element} is not imported")),
            _ => warn(format!(
                "a device of resource type {resource_type} is not imported"
            )),
        }
    }

    if disk_items.len() > MAX_DISKS {
        return Err(invalid(format!(
            "the OVF has {} disks; at most {MAX_DISKS} are supported",
            disk_items.len()
        )));
    }
    if nics.len() > MAX_NICS {
        return Err(invalid(format!(
            "the OVF has {} network adapters; at most {MAX_NICS} are supported",
            nics.len()
        )));
    }

    // 컨트롤러 종류가 처음 나온 순서, 같은 종류는 `rasd:Address` 순서(없으면 나온 순서), 그 안에서
    // 디스크 주소 순서
    if let Some(d) = disk_items
        .iter()
        .find(|d| !controllers.iter().any(|c| c.id == d.parent))
    {
        return Err(invalid(format!(
            "disk {:?} is on an unknown controller",
            d.disk_id
        )));
    }
    let kinds: Vec<&str> = controllers.iter().map(|c| c.kind).collect();
    controllers.sort_by_key(|c| {
        (
            kinds.iter().position(|k| *k == c.kind),
            c.address.unwrap_or(u32::MAX),
        )
    });
    if controllers
        .iter()
        .filter(|c| disk_items.iter().any(|d| d.parent == c.id))
        .count()
        > 1
    {
        warn("disks are on more than one controller; check that disk0 is the boot disk".into());
    }
    let mut disks = Vec::new();
    for c in &controllers {
        let mut on = disk_items
            .iter()
            .filter(|d| d.parent == c.id)
            .collect::<Vec<_>>();
        on.sort_by_key(|d| d.address);
        for d in on {
            let (file, capacity) = disk_files.get(d.disk_id).ok_or_else(|| {
                invalid(format!("disk {:?} is not in the DiskSection", d.disk_id))
            })?;
            disks.push(SourceDisk {
                name: format!("disk{}", disks.len()),
                file: file.to_string(),
                capacity_bytes: *capacity,
                controller: c.kind.to_string(),
            });
        }
    }

    let (vcpus, cores_per_socket) = cpu.ok_or_else(|| invalid("the OVF has no CPU count"))?;
    let memory_mib = memory_mib.ok_or_else(|| invalid("the OVF has no memory size"))?;
    Ok(Ovf {
        vm: SourceVm {
            name,
            guest_os,
            firmware,
            vcpus,
            cores_per_socket,
            memory_mib,
            disks,
            nics,
        },
        warnings,
        files,
    })
}

/// `vmw:osType`, 없으면 CIM 번호로 Windows인지만(`windows`), 그것도 아니면 설명
fn guest_os(os: Node<'_, '_>) -> String {
    if let Some(os_type) = attr(os, "osType") {
        return os_type.to_string();
    }
    let cim = attr(os, "id").and_then(|id| id.parse::<u32>().ok());
    if cim.is_some_and(|id| WINDOWS_CIM_IDS.contains(&id)) {
        return "windows".into();
    }
    text(os, "Description")
        .filter(|d| !d.is_empty())
        .unwrap_or("other")
        .to_string()
}
