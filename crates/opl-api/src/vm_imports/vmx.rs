//! ESXi 데이터스토어의 VM 설정(`.vmx`)과 VMDK 디스크립터 해석. 순수 함수다(파일은 ESXi 클라이언트가
//! 읽어 온다). 경로는 모두 데이터스토어 안의 경로(`web01/web01.vmx`)다.

use std::collections::BTreeMap;

use opl_types::vm::{Firmware, MAX_NICS, MacAddr};
use opl_types::vm_import::{SourceNic, SourceVm};

/// `.vmx`·디스크립터의 최대 크기
pub const MAX_TEXT_BYTES: usize = 64 << 10;
/// 가져올 수 있는 디스크의 최대 개수(OVF와 같다)
const MAX_DISKS: usize = 32;
/// 디스크 컨트롤러 종류. `bios.hddOrder`가 없으면 이 순서로 부트 디스크(`disk0`)를 고른다.
const CONTROLLERS: [&str; 4] = ["ide", "sata", "scsi", "nvme"];
/// 디스크로 보는 `deviceType`(없어도 디스크다)
const DISK_TYPES: [&str; 3] = ["disk", "scsi-harddisk", "ata-harddisk"];
const SECTOR: u64 = 512;

/// `.vmx`·디스크립터를 쓸 수 없는 이유. 메시지는 사용자에게 그대로 보인다.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct VmxError(pub String);

fn invalid(message: impl Into<String>) -> VmxError {
    VmxError(message.into())
}

/// 해석한 `.vmx`. 키는 소문자다.
#[derive(Debug, Clone, Default)]
pub struct Vmx {
    values: BTreeMap<String, String>,
}

impl Vmx {
    /// 값(키는 대소문자를 가리지 않는다)
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn is_true(&self, key: &str) -> bool {
        self.get(key)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }
}

/// `.vmx` 안의 디스크(디스크립터를 읽기 전)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmxDisk {
    /// `disk{i}`
    pub name: String,
    /// `.vmx`에 적힌 경로 그대로(풀기는 [`disk_path`])
    pub path: String,
    /// `ide`, `sata`, `scsi`, `nvme`
    pub controller: String,
}

/// `key = "value"` 줄을 읽는다. 따옴표 없는 값도 받고, `#`로 시작하는 줄과 `=`가 없는 줄은 건너뛴다.
/// 키는 소문자로 맞추고, 값의 VMware 이스케이프(`|22` = `"`)를 푼다. 같은 키는 뒤의 것이 이긴다.
pub fn parse_vmx(text: &str) -> Result<Vmx, VmxError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(invalid("the .vmx file is larger than 64 KiB"));
    }
    if text.contains('\0') {
        return Err(invalid("not a .vmx file"));
    }
    let mut values = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = match value.strip_prefix('"') {
            // 값 안의 따옴표는 `|22`로 적히므로 다음 따옴표가 끝이다
            Some(rest) => rest.split_once('"').map_or(rest, |(v, _)| v),
            None => value,
        };
        values.insert(key.trim().to_ascii_lowercase(), unescape(value));
    }
    Ok(Vmx { values })
}

/// `|XX`(16진 바이트)를 푼다. 16진이 아니면 그대로 둔다.
fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|'
            && let Some(h) = bytes.get(i + 1..i + 3)
            && h.iter().all(u8::is_ascii_hexdigit)
        {
            let hex = std::str::from_utf8(h).unwrap_or_default();
            out.push(u8::from_str_radix(hex, 16).unwrap_or_default());
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `scsi0:1` 같은 디스크 장치 이름을 (컨트롤러 종류, 버스, 유닛)으로
fn disk_device(dev: &str) -> Option<(&'static str, u32, u32)> {
    let kind = CONTROLLERS.into_iter().find(|k| dev.starts_with(k))?;
    let (bus, unit) = dev[kind.len()..].split_once(':')?;
    Some((kind, bus.parse().ok()?, unit.parse().ok()?))
}

fn number(vmx: &Vmx, key: &str, default: Option<u64>) -> Result<u64, VmxError> {
    match vmx.get(key) {
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| invalid(format!("invalid {key} {v:?}"))),
        None => default.ok_or_else(|| invalid(format!("the .vmx has no {key}"))),
    }
}

fn small(vmx: &Vmx, key: &str) -> Result<u32, VmxError> {
    let n = number(vmx, key, Some(1))?;
    u32::try_from(n).map_err(|_| invalid(format!("invalid {key} \"{n}\"")))
}

/// `.vmx`에서 원본 VM을 읽는다. `vmx_path`는 데이터스토어 안의 `.vmx` 경로이고, 이름이 없을 때 파일
/// 이름을 쓴다. 돌려준 [`SourceVm`]의 `disks`는 비어 있다 — 디스크 크기와 실제 파일은 디스크립터를
/// 읽어야 알 수 있으므로, 호출하는 쪽이 [`VmxDisk`]마다 디스크립터를 읽어 채운다.
pub fn vmx_to_source(
    vmx: &Vmx,
    vmx_path: &str,
) -> Result<(SourceVm, Vec<String>, Vec<VmxDisk>), VmxError> {
    let mut warnings = Vec::new();
    let mut warn = |w: String| {
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    };

    // 켜진 디스크 장치를 (종류 순서, 버스, 유닛) 순서로
    let mut devices: Vec<(usize, u32, u32, &str)> = vmx
        .values
        .iter()
        .filter_map(|(key, value)| {
            let dev = key.strip_suffix(".present")?;
            let (kind, bus, unit) = disk_device(dev)?;
            value.eq_ignore_ascii_case("true").then(|| {
                let rank = CONTROLLERS.iter().position(|k| *k == kind).unwrap_or(0);
                (rank, bus, unit, dev)
            })
        })
        .collect();
    devices.sort();
    let mut disks = Vec::new();
    for (rank, bus, _, dev) in devices {
        let device_type = vmx
            .get(&format!("{dev}.deviceType"))
            .map(str::to_ascii_lowercase);
        match device_type.as_deref() {
            None => {}
            Some(t) if DISK_TYPES.contains(&t) => {}
            Some(t) if t.contains("cdrom") => {
                warn("CD-ROM drive is not imported".into());
                continue;
            }
            Some(t) => {
                warn(format!("{dev} ({t}) is not imported"));
                continue;
            }
        }
        let file = vmx
            .get(&format!("{dev}.fileName"))
            .ok_or_else(|| invalid(format!("{dev} has no fileName")))?;
        if !file.to_ascii_lowercase().ends_with(".vmdk") {
            return Err(invalid(format!("disk {dev} is not a VMDK: {file:?}")));
        }
        disks.push((dev.to_string(), (rank, bus), file.to_string()));
    }
    if disks.len() > MAX_DISKS {
        return Err(invalid(format!(
            "the .vmx has {} disks; at most {MAX_DISKS} are supported",
            disks.len()
        )));
    }
    // `bios.hddOrder`(예: `scsi0:1,scsi0:0`)에 적힌 디스크가 앞에 온다(그 순서대로).
    let hdd_order: Vec<String> = vmx
        .get("bios.hddOrder")
        .map(|o| {
            o.split(',')
                .map(|d| d.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    disks.sort_by_key(|(dev, _, _)| {
        hdd_order
            .iter()
            .position(|d| d == dev)
            .unwrap_or(usize::MAX)
    });
    let mut controllers: Vec<_> = disks.iter().map(|(_, c, _)| *c).collect();
    controllers.sort();
    controllers.dedup();

    for (key, what) in [
        ("floppy0.present", "floppy drive"),
        ("usb.present", "USB controller"),
        ("sound.present", "sound card"),
    ] {
        if vmx.is_true(key) {
            warn(format!("{what} is not imported"));
        }
    }
    if hdd_order.is_empty() && controllers.len() > 1 {
        warn("disks are on more than one controller; check that disk0 is the boot disk".into());
    }

    // 켜진 NIC를 번호 순서로
    let mut ethernets: Vec<u32> = vmx
        .values
        .iter()
        .filter_map(|(key, value)| {
            let n = key.strip_prefix("ethernet")?.strip_suffix(".present")?;
            value
                .eq_ignore_ascii_case("true")
                .then(|| n.parse().ok())
                .flatten()
        })
        .collect();
    ethernets.sort();
    if ethernets.len() > MAX_NICS {
        return Err(invalid(format!(
            "the .vmx has {} network adapters; at most {MAX_NICS} are supported",
            ethernets.len()
        )));
    }
    let mut nics = Vec::new();
    for n in ethernets {
        let get = |key: &str| vmx.get(&format!("ethernet{n}.{key}"));
        // vCenter 분산 스위치의 NIC는 `networkName` 대신 포트 그룹을 적는다.
        let network = match (get("networkName"), get("dvs.portgroupId")) {
            (Some(network), _) => network.to_string(),
            (None, Some(id)) => {
                warn(format!(
                    "ethernet{n} is on a distributed switch; map its network when running the import"
                ));
                get("dvs.portgroupName")
                    .filter(|name| !name.is_empty())
                    .map_or_else(|| format!("dvs:{id}"), str::to_string)
            }
            // NSX 네트워크의 NIC는 불투명 네트워크 id만 적는다.
            (None, None) => match get("opaqueNetwork.id") {
                Some(id) => {
                    warn(format!(
                        "ethernet{n} is on an NSX network; map its network when running the import"
                    ));
                    format!("nsx:{id}")
                }
                None => return Err(invalid(format!("ethernet{n} has no networkName"))),
            },
        };
        let mac = match get("addressType") {
            Some(t) if t.eq_ignore_ascii_case("static") => get("address"),
            _ => get("generatedAddress"),
        };
        nics.push(SourceNic {
            network,
            adapter: get("virtualDev").unwrap_or("e1000").to_ascii_lowercase(),
            mac: mac.and_then(|m| m.parse::<MacAddr>().ok()),
        });
    }

    let file_name = vmx_path.rsplit('/').next().unwrap_or(vmx_path);
    let name = vmx
        .get("displayName")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| file_name.strip_suffix(".vmx").unwrap_or(file_name));
    let firmware = match vmx.get("firmware") {
        Some(f) if f.eq_ignore_ascii_case("efi") => Firmware::Uefi,
        _ => Firmware::Bios,
    };
    let vm = SourceVm {
        name: name.to_string(),
        guest_os: vmx.get("guestOS").unwrap_or("other").to_string(),
        firmware,
        vcpus: small(vmx, "numvcpus")?,
        cores_per_socket: small(vmx, "cpuid.coresPerSocket")?,
        memory_mib: number(vmx, "memSize", None)?,
        disks: Vec::new(),
        nics,
    };
    let disks = disks
        .into_iter()
        .enumerate()
        .map(|(i, (_, (rank, _), path))| VmxDisk {
            name: format!("disk{i}"),
            path,
            controller: CONTROLLERS[rank].to_string(),
        })
        .collect();
    Ok((vm, warnings, disks))
}

/// VMDK 디스크립터(텍스트)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub create_type: String,
    pub extents: Vec<Extent>,
    /// 부모 디스크가 있다(`parentFileNameHint`, 스냅샷)
    pub parent: bool,
}

/// extent 줄(`RW 8192 VMFS "web01-flat.vmdk" [offset]`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extent {
    /// `RW`, `RDONLY`, `NOACCESS`
    pub access: String,
    pub sectors: u64,
    /// `VMFS`, `FLAT`, `VMFSSPARSE` 등
    pub kind: String,
    pub file: String,
    pub offset: u64,
}

const ACCESS: [&str; 3] = ["RW", "RDONLY", "NOACCESS"];

/// 디스크립터를 읽는다. 스파스 VMDK처럼 바이너리이거나 `createType`이 없으면 디스크립터가 아니다.
pub fn parse_descriptor(text: &str) -> Result<Descriptor, VmxError> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(invalid("the VMDK descriptor is larger than 64 KiB"));
    }
    let not = || invalid("not a VMDK descriptor");
    if text.contains('\0') {
        return Err(not());
    }
    let mut create_type = None;
    let mut parent = false;
    let mut extents = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if ACCESS
            .iter()
            .any(|a| line.strip_prefix(a).is_some_and(|r| r.starts_with(' ')))
        {
            extents.push(
                extent(line).ok_or_else(|| invalid(format!("invalid VMDK extent {line:?}")))?,
            );
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "createType" => create_type = Some(value.to_string()),
            "parentFileNameHint" if !value.is_empty() => parent = true,
            _ => {}
        }
    }
    Ok(Descriptor {
        create_type: create_type.ok_or_else(not)?,
        extents,
        parent,
    })
}

/// `RW 8192 VMFS "file" [offset]`
fn extent(line: &str) -> Option<Extent> {
    let (head, rest) = line.split_once('"')?;
    let (file, tail) = rest.split_once('"')?;
    let mut words = head.split_whitespace();
    let (access, sectors, kind) = (words.next()?, words.next()?, words.next()?);
    if words.next().is_some() {
        return None;
    }
    let offset = match tail.trim() {
        "" => 0,
        o => o.parse().ok()?,
    };
    Some(Extent {
        access: access.to_string(),
        sectors: sectors.parse().ok()?,
        kind: kind.to_string(),
        file: file.to_string(),
        offset,
    })
}

/// 통째로 내려받을 수 있는 디스크(`vmfs`·`monolithicFlat`, extent 하나)인지 보고 extent 파일 이름
/// (디스크립터와 같은 디렉터리)과 바이트 수를 돌려준다.
pub fn check_flat(d: &Descriptor) -> Result<(String, u64), VmxError> {
    let create_type = d.create_type.to_ascii_lowercase();
    if d.parent
        || [
            "vmfssparse",
            "sesparse",
            "vsansparse",
            "twogbmaxextentsparse",
        ]
        .contains(&create_type.as_str())
    {
        return Err(invalid(
            "the VM has snapshots; consolidate or delete them in VMware first",
        ));
    }
    if ["vmfsrdm", "vmfsrawdevicemap", "vmfspassthroughrawdevicemap"]
        .contains(&create_type.as_str())
    {
        return Err(invalid("raw device mappings are not supported"));
    }
    if !["vmfs", "monolithicflat"].contains(&create_type.as_str()) {
        return Err(invalid(format!("unsupported VMDK type {}", d.create_type)));
    }
    let [e] = &d.extents[..] else {
        return Err(invalid(format!(
            "the VMDK descriptor has {} extents; only one is supported",
            d.extents.len()
        )));
    };
    if e.access != "RW" || !["VMFS", "FLAT"].contains(&e.kind.as_str()) || e.offset != 0 {
        return Err(invalid(format!(
            "unsupported VMDK extent {} {} {} {:?} {}",
            e.access, e.sectors, e.kind, e.file, e.offset
        )));
    }
    // 같은 디렉터리의 파일 이름만(`/`·`\`가 없으면 `.`·`..` 자체만 디렉터리를 가리킨다)
    if e.file.is_empty() || e.file.contains(['/', '\\']) || e.file == "." || e.file == ".." {
        return Err(invalid(format!("invalid extent file name {:?}", e.file)));
    }
    let bytes = e
        .sectors
        .checked_mul(SECTOR)
        .ok_or_else(|| invalid(format!("the disk {:?} is too large", e.file)))?;
    Ok((e.file.clone(), bytes))
}

/// VMFS 볼륨 UUID(`62f3a1b2-0c1d2e3f-4a5b-001122334455`) 모양인지
fn is_volume_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 8, 4, 12])
        && parts
            .iter()
            .all(|p| p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `.vmx`(또는 디스크립터) 옆에 적힌 파일 경로를 데이터스토어 안의 경로로 푼다. 상대 경로는 `from`의
/// 디렉터리 기준이고, `/vmfs/volumes/{datastore}/…`는 같은 데이터스토어여야 한다. 그 밖의 절대 경로,
/// `..`, `\`는 거절한다.
pub fn disk_path(from: &str, datastore: &str, file: &str) -> Result<String, VmxError> {
    let unsupported = || invalid(format!("disk path {file:?} is not supported"));
    if file.is_empty() || file.contains('\\') || file.split('/').any(|p| p == "..") {
        return Err(unsupported());
    }
    if let Some(rest) = file.strip_prefix("/vmfs/volumes/") {
        let (volume, path) = rest.split_once('/').ok_or_else(unsupported)?;
        let path = without_dots(path);
        if path.is_empty() {
            return Err(unsupported());
        }
        if volume == datastore {
            return Ok(path);
        }
        if is_volume_uuid(volume) {
            return Err(invalid(format!(
                "disk {file:?} uses a datastore UUID path; it is not supported"
            )));
        }
        return Err(invalid(format!("disk {file:?} is on another datastore")));
    }
    if file.starts_with('/') {
        return Err(unsupported());
    }
    let file = without_dots(file);
    Ok(match from.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{file}"),
        None => file,
    })
}

/// 경로의 `.` 조각을 뺀다(`./x.vmdk` → `x.vmdk`).
fn without_dots(path: &str) -> String {
    path.split('/')
        .filter(|p| *p != ".")
        .collect::<Vec<_>>()
        .join("/")
}
