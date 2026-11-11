//! VM 가져오기. 다른 하이퍼바이저(VMware)에서 내보낸 VM(OVA)을 올린 노드에 두고, 원본 VM에서 읽은
//! 값([`SourceVm`])으로 Orbivisor VM 설정을 제안([`propose`])한 뒤, 사용자가 고친 값([`ImportRun`])으로
//! 디스크를 볼륨으로 옮기고 VM을 만든다.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::{DiskBus, Firmware, MIN_MEMORY_MIB, MacAddr, NicModel};

pub type VmImport = Resource<VmImportSpec, VmImportStatus>;

/// q35 AHCI 포트 수. SATA 디스크는 이만큼만 붙일 수 있다.
pub const SATA_PORTS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmImportSpec {
    /// 원본 파일이 있는 노드(올린 노드)
    pub node: Uuid,
    pub source: ImportSource,
}

impl ResourceSpec for VmImportSpec {
    const KIND: Kind = Kind::VmImport;
    type Status = VmImportStatus;
}

/// 원본. JSON은 `{"ova": {...}}` 또는 `{"esxi": {...}}`다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ImportSource {
    /// 올린 OVA 파일
    Ova(OvaSource),
    /// ESXi·vCenter 데이터스토어의 VM. 비밀번호는 여기에 없다(받은 노드의 메모리에만 둔다).
    Esxi(EsxiSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct OvaSource {
    /// 올린 파일 이름(표시용)
    pub file_name: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct EsxiSource {
    pub host: String,
    pub port: u16,
    /// 데이터센터(ESXi는 `ha-datacenter`)
    pub datacenter: String,
    pub datastore: String,
    /// 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`)
    pub vmx: String,
    pub user: String,
    /// 믿기로 한 서버 인증서의 SHA-256 지문(대문자 콜론 구분 `AB:CD:…`)
    pub fingerprint: String,
}

/// ESXi·vCenter 연결(API 요청 본문). 비밀번호가 있으므로 저장하지 않고 로그에도 찍지 않는다.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct EsxiConnection {
    pub host: String,
    #[serde(default = "https_port")]
    #[cfg_attr(feature = "openapi", schema(default = 443))]
    pub port: u16,
    pub user: String,
    pub password: String,
    /// vCenter면 데이터센터 이름
    #[serde(default = "ha_datacenter")]
    #[cfg_attr(feature = "openapi", schema(default = "ha-datacenter"))]
    pub datacenter: String,
    /// 믿는 서버 인증서의 SHA-256 지문. 없으면 서버가 지문을 알려 주며 거절한다.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

fn https_port() -> u16 {
    443
}

fn ha_datacenter() -> String {
    "ha-datacenter".into()
}

/// 로그에 비밀번호가 찍히지 않게 한다.
impl fmt::Debug for EsxiConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EsxiConnection")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("datacenter", &self.datacenter)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// 인증서 SHA-256 지문(16진 64자, 콜론·공백 허용, 대소문자 무시)을 ESXi 화면과 같은 대문자 콜론
/// 구분(`AB:CD:…`)으로 바꾼다. 그 모양이 아니면 `None`.
pub fn normalize_fingerprint(text: &str) -> Option<String> {
    let hex: Vec<char> = text
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect();
    if hex.len() != 64 || !hex.iter().all(char::is_ascii_hexdigit) {
        return None;
    }
    let pairs: Vec<String> = hex
        .chunks(2)
        .map(|p| p.iter().collect::<String>().to_ascii_uppercase())
        .collect();
    Some(pairs.join(":"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmImportStatus {
    pub phase: ImportPhase,
    /// 원본 VM에서 읽은 것(표시용)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub source_vm: Option<SourceVm>,
    /// [`propose`]가 만든 기본값
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub proposal: Option<ImportProposal>,
    /// 해석과 제안의 경고
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// 만든 VM id(`done`일 때)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vm: Option<Uuid>,
    /// 마지막 실행이 실패한 이유(phase는 `ready`로 돌아간다), 또는 `failed`인 이유
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ImportPhase {
    /// 실행할 수 있다(실패한 실행 뒤에도)
    #[default]
    Ready,
    /// 실행 중
    Importing,
    /// VM을 만들었다
    Done,
    /// 원본을 쓸 수 없다
    Failed,
}

/// 원본 VM에서 읽은 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceVm {
    pub name: String,
    /// 게스트 OS(OVF `vmw:osType` 등)
    pub guest_os: String,
    pub firmware: Firmware,
    pub vcpus: u32,
    pub cores_per_socket: u32,
    pub memory_mib: u64,
    /// 디스크(컨트롤러 순서, 그 안에서 주소 순서)
    pub disks: Vec<SourceDisk>,
    pub nics: Vec<SourceNic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceDisk {
    /// `disk{i}`
    pub name: String,
    /// 원본 안의 디스크 파일 이름
    pub file: String,
    pub capacity_bytes: u64,
    /// 원본 컨트롤러(`ide`, `sata`, `scsi`, `nvme`)
    pub controller: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceNic {
    /// 원본 네트워크 이름
    pub network: String,
    /// 원본 어댑터 종류(`vmxnet3`, `e1000` 등)
    pub adapter: String,
    #[serde(default)]
    pub mac: Option<MacAddr>,
}

/// Orbivisor VM 설정 제안. 실행할 때 [`ImportRun`]에 없는 값의 기본값이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportProposal {
    pub sockets: u16,
    pub cores: u16,
    pub memory_mib: u32,
    pub firmware: Firmware,
    pub disk_bus: DiskBus,
    pub nic_model: NicModel,
    /// 원본 네트워크 → 이름이 같은 Orbivisor 네트워크(없으면 null, 실행 때 정해야 한다)
    pub networks: BTreeMap<String, Option<String>>,
}

/// 실행 요청(`POST /vm-imports/{key}/run`). 비운 값은 제안 값을 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ImportRun {
    /// 만들 VM 이름
    pub name: String,
    /// 디스크 볼륨을 만들 풀
    pub pool: String,
    /// 원본 네트워크 → Orbivisor 네트워크. 원본 NIC의 네트워크마다 있어야 한다.
    #[serde(default)]
    pub networks: BTreeMap<String, String>,
    #[serde(default)]
    pub disk_bus: Option<DiskBus>,
    #[serde(default)]
    pub nic_model: Option<NicModel>,
    #[serde(default)]
    pub firmware: Option<Firmware>,
    #[serde(default)]
    pub sockets: Option<u16>,
    #[serde(default)]
    pub cores: Option<u16>,
    #[serde(default)]
    pub memory_mib: Option<u32>,
    /// 원본 MAC 주소를 그대로 쓴다. 끄면 새 MAC을 받는다.
    #[serde(default = "yes")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub keep_macs: bool,
}

fn yes() -> bool {
    true
}

/// 게스트 OS가 Windows인지(`vmw:osType`이 `win`으로 시작)
pub fn is_windows(guest_os: &str) -> bool {
    guest_os.to_ascii_lowercase().starts_with("win")
}

/// 원본 VM으로 Orbivisor VM 설정을 제안한다. `networks`는 지금 있는 Orbivisor 네트워크 이름이다.
/// 그대로 옮길 수 없는 것은 경고로 돌려준다.
pub fn propose(src: &SourceVm, networks: &[String]) -> (ImportProposal, Vec<String>) {
    let mut warnings = Vec::new();
    let small = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
    // 소켓당 코어로 나누어떨어지지 않으면 소켓 하나에 모두 둔다.
    let (sockets, cores) = match src.cores_per_socket {
        c if c > 0 && src.vcpus.is_multiple_of(c) => (small(src.vcpus / c), small(c)),
        _ => (1, small(src.vcpus.max(1))),
    };
    let memory_mib = match u32::try_from(src.memory_mib) {
        Ok(m) if m >= MIN_MEMORY_MIB => m,
        Ok(m) => {
            warnings.push(format!(
                "memory {m} MiB is below the minimum; using {MIN_MEMORY_MIB} MiB"
            ));
            MIN_MEMORY_MIB
        }
        Err(_) => {
            warnings.push(format!(
                "memory is too large ({} MiB); set memory_mib when running the import",
                src.memory_mib
            ));
            MIN_MEMORY_MIB
        }
    };
    if src.firmware == Firmware::Uefi {
        warnings.push(
            "UEFI boot entries (NVRAM) are not imported; the guest boots from its default boot path"
                .into(),
        );
    }
    let (mut disk_bus, nic_model) = if is_windows(&src.guest_os) {
        (DiskBus::Sata, NicModel::E1000e)
    } else {
        (DiskBus::VirtioBlk, NicModel::Virtio)
    };
    if disk_bus == DiskBus::Sata && src.disks.len() > SATA_PORTS {
        disk_bus = DiskBus::VirtioBlk;
        warnings.push(format!(
            "{} disks do not fit on SATA ({SATA_PORTS} ports); using virtio-blk, which needs guest drivers",
            src.disks.len()
        ));
    }
    if src.nics.iter().any(|n| n.mac.is_some()) {
        warnings.push(
            "power off the VMware VM before starting the imported one; it keeps the same MAC addresses"
                .into(),
        );
    }
    let networks = src
        .nics
        .iter()
        .map(|n| {
            let same = networks.contains(&n.network).then(|| n.network.clone());
            (n.network.clone(), same)
        })
        .collect();
    let proposal = ImportProposal {
        sockets,
        cores,
        memory_mib,
        firmware: src.firmware,
        disk_bus,
        nic_model,
        networks,
    };
    (proposal, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source() -> SourceVm {
        SourceVm {
            name: "web01".into(),
            guest_os: "ubuntu64Guest".into(),
            firmware: Firmware::Bios,
            vcpus: 4,
            cores_per_socket: 2,
            memory_mib: 2048,
            disks: vec![disk(0)],
            nics: vec![SourceNic {
                network: "VM Network".into(),
                adapter: "vmxnet3".into(),
                mac: None,
            }],
        }
    }

    fn disk(i: usize) -> SourceDisk {
        SourceDisk {
            name: format!("disk{i}"),
            file: format!("web01-disk{}.vmdk", i + 1),
            capacity_bytes: 1 << 30,
            controller: "scsi".into(),
        }
    }

    fn with(f: impl FnOnce(&mut SourceVm)) -> SourceVm {
        let mut s = source();
        f(&mut s);
        s
    }

    fn propose_none(src: &SourceVm) -> (ImportProposal, Vec<String>) {
        propose(src, &[])
    }

    #[test]
    fn spec_source_is_externally_tagged() {
        let spec = VmImportSpec {
            node: Uuid::nil(),
            source: ImportSource::Ova(OvaSource {
                file_name: "web01.ova".into(),
                size_bytes: 123,
            }),
        };
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v,
            json!({
                "node": Uuid::nil(),
                "source": { "ova": { "file_name": "web01.ova", "size_bytes": 123 } }
            })
        );
        assert_eq!(serde_json::from_value::<VmImportSpec>(v).unwrap(), spec);
        assert!(
            serde_json::from_value::<VmImportSpec>(json!({
                "node": Uuid::nil(),
                "source": { "ova": { "file_name": "a.ova", "size_bytes": 1, "x": 1 } }
            }))
            .is_err()
        );
    }

    #[test]
    fn esxi_source_has_no_password() {
        let spec = VmImportSpec {
            node: Uuid::nil(),
            source: ImportSource::Esxi(EsxiSource {
                host: "esx1.example.com".into(),
                port: 443,
                datacenter: "ha-datacenter".into(),
                datastore: "datastore1".into(),
                vmx: "web01/web01.vmx".into(),
                user: "root".into(),
                fingerprint: "AB".into(),
            }),
        };
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v["source"],
            json!({ "esxi": {
                "host": "esx1.example.com", "port": 443, "datacenter": "ha-datacenter",
                "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": "root",
                "fingerprint": "AB"
            } })
        );
        assert_eq!(
            serde_json::from_value::<VmImportSpec>(v.clone()).unwrap(),
            spec
        );
        // 비밀번호 칸은 받지 않는다
        let mut with_password = v;
        with_password["source"]["esxi"]["password"] = json!("secret");
        assert!(serde_json::from_value::<VmImportSpec>(with_password).is_err());
    }

    #[test]
    fn esxi_connection_defaults_and_hides_password() {
        let c: EsxiConnection = serde_json::from_value(json!({
            "host": "esx1", "user": "root", "password": "s3cret!"
        }))
        .unwrap();
        assert_eq!(c.port, 443);
        assert_eq!(c.datacenter, "ha-datacenter");
        assert_eq!(c.fingerprint, None);
        assert_eq!(c.password, "s3cret!");
        let debug = format!("{c:?}");
        assert!(!debug.contains("s3cret!"), "{debug}");
        assert!(debug.contains("esx1") && debug.contains("root"), "{debug}");
        let c: EsxiConnection = serde_json::from_value(json!({
            "host": "vc1", "port": 8443, "user": "u", "password": "p",
            "datacenter": "DC1", "fingerprint": "ab:cd"
        }))
        .unwrap();
        assert_eq!((c.port, c.datacenter.as_str()), (8443, "DC1"));
        assert_eq!(c.fingerprint.as_deref(), Some("ab:cd"));
        assert!(
            serde_json::from_value::<EsxiConnection>(json!({
                "host": "h", "user": "u", "password": "p", "bogus": 1
            }))
            .is_err()
        );
    }

    #[test]
    fn normalizes_fingerprints() {
        let hex = "0123456789abcdef".repeat(4);
        let want = ["01:23:45:67:89:AB:CD:EF"; 4].join(":");
        assert_eq!(want.len(), 95);
        let colons = want.to_ascii_lowercase();
        let spaced = colons.replace(':', " ");
        let upper = hex.to_ascii_uppercase();
        let padded = format!("  {hex}\n");
        for ok in [&hex, &upper, &want, &colons, &spaced, &padded] {
            assert_eq!(
                normalize_fingerprint(ok).as_deref(),
                Some(want.as_str()),
                "{ok}"
            );
        }
        for bad in [
            String::new(),
            hex[..62].to_string(),
            format!("{hex}00"),
            hex.replacen('0', "g", 1),
            // SHA-1 지문(20바이트)
            "ab:".repeat(20),
            format!("sha256:{hex}"),
        ] {
            assert_eq!(normalize_fingerprint(&bad), None, "{bad}");
        }
    }

    #[test]
    fn empty_status_is_just_ready() {
        let v = serde_json::to_value(VmImportStatus::default()).unwrap();
        assert_eq!(v, json!({ "phase": "ready" }));
        assert_eq!(
            serde_json::from_value::<VmImportStatus>(json!({})).unwrap(),
            VmImportStatus::default()
        );
        for (phase, s) in [
            (ImportPhase::Ready, "ready"),
            (ImportPhase::Importing, "importing"),
            (ImportPhase::Done, "done"),
            (ImportPhase::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(phase).unwrap(), s);
        }
    }

    #[test]
    fn full_status_round_trips() {
        let (proposal, warnings) = propose_none(&source());
        let status = VmImportStatus {
            phase: ImportPhase::Done,
            source_vm: Some(with(|s| {
                s.nics[0].mac = Some("00:50:56:01:02:03".parse().unwrap())
            })),
            proposal: Some(proposal),
            warnings,
            vm: Some(Uuid::nil()),
            error: Some("x".into()),
        };
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["source_vm"]["nics"][0]["mac"], "00:50:56:01:02:03");
        assert_eq!(v["proposal"]["networks"], json!({ "VM Network": null }));
        assert_eq!(serde_json::from_value::<VmImportStatus>(v).unwrap(), status);
    }

    #[test]
    fn run_defaults_and_rejects_unknown_fields() {
        let run: ImportRun =
            serde_json::from_value(json!({ "name": "web01", "pool": "local" })).unwrap();
        assert!(run.keep_macs);
        assert!(run.networks.is_empty());
        assert_eq!(run.disk_bus, None);
        assert_eq!(run.memory_mib, None);
        let run: ImportRun = serde_json::from_value(json!({
            "name": "web01", "pool": "local",
            "networks": { "VM Network": "lan" },
            "disk_bus": "sata", "nic_model": "e1000e", "firmware": "uefi",
            "sockets": 2, "cores": 2, "memory_mib": 8192, "keep_macs": false
        }))
        .unwrap();
        assert!(!run.keep_macs);
        assert_eq!(run.networks["VM Network"], "lan");
        assert_eq!(run.disk_bus, Some(DiskBus::Sata));
        assert_eq!(run.nic_model, Some(NicModel::E1000e));
        assert_eq!(run.sockets, Some(2));
        assert!(
            serde_json::from_value::<ImportRun>(json!({ "name": "a", "pool": "p", "bogus": 1 }))
                .is_err()
        );
    }

    #[test]
    fn detects_windows() {
        for g in ["windows9_64Guest", "win2000ServGuest", "Windows"] {
            assert!(is_windows(g), "{g}");
        }
        for g in ["ubuntu64Guest", "otherGuest", "", "darwinGuest"] {
            assert!(!is_windows(g), "{g}");
        }
    }

    #[test]
    fn linux_gets_virtio() {
        let (p, warnings) = propose_none(&source());
        assert_eq!(p.disk_bus, DiskBus::VirtioBlk);
        assert_eq!(p.nic_model, NicModel::Virtio);
        assert_eq!(p.firmware, Firmware::Bios);
        assert_eq!(p.memory_mib, 2048);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn windows_gets_sata_and_e1000e() {
        let (p, _) = propose_none(&with(|s| s.guest_os = "windows2019srv_64Guest".into()));
        assert_eq!(p.disk_bus, DiskBus::Sata);
        assert_eq!(p.nic_model, NicModel::E1000e);
    }

    #[test]
    fn splits_cores_into_sockets() {
        for (vcpus, per, sockets, cores) in [
            (4, 2, 2, 2),
            (4, 4, 1, 4),
            (8, 1, 8, 1),
            // 나누어떨어지지 않으면 소켓 하나
            (6, 4, 1, 6),
            // 소켓당 코어를 모르면 소켓 하나
            (3, 0, 1, 3),
        ] {
            let (p, _) = propose_none(&with(|s| {
                s.vcpus = vcpus;
                s.cores_per_socket = per;
            }));
            assert_eq!((p.sockets, p.cores), (sockets, cores), "{vcpus}/{per}");
        }
    }

    #[test]
    fn small_memory_becomes_minimum_with_warning() {
        let (p, warnings) = propose_none(&with(|s| s.memory_mib = 32));
        assert_eq!(p.memory_mib, MIN_MEMORY_MIB);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("minimum"), "{warnings:?}");
    }

    #[test]
    fn huge_memory_becomes_minimum_with_warning() {
        let (p, warnings) = propose_none(&with(|s| s.memory_mib = u64::from(u32::MAX) + 1));
        assert_eq!(p.memory_mib, MIN_MEMORY_MIB);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("memory is too large"), "{warnings:?}");
        // u32에 들어가면 그대로
        let (p, warnings) = propose_none(&with(|s| s.memory_mib = u64::from(u32::MAX)));
        assert_eq!(p.memory_mib, u32::MAX);
        assert!(warnings.is_empty());
    }

    #[test]
    fn uefi_warns_about_boot_entries() {
        let (p, warnings) = propose_none(&with(|s| s.firmware = Firmware::Uefi));
        assert_eq!(p.firmware, Firmware::Uefi);
        assert_eq!(
            warnings,
            [
                "UEFI boot entries (NVRAM) are not imported; the guest boots from its default boot path"
            ]
        );
    }

    #[test]
    fn too_many_sata_disks_fall_back_to_virtio_blk() {
        let windows = |n: usize| {
            with(|s| {
                s.guest_os = "windows9_64Guest".into();
                s.disks = (0..n).map(disk).collect();
            })
        };
        let (p, warnings) = propose_none(&windows(6));
        assert_eq!(p.disk_bus, DiskBus::Sata);
        assert!(warnings.is_empty());
        let (p, warnings) = propose_none(&windows(7));
        assert_eq!(p.disk_bus, DiskBus::VirtioBlk);
        assert_eq!(p.nic_model, NicModel::E1000e);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("virtio-blk"), "{warnings:?}");
    }

    #[test]
    fn maps_networks_with_the_same_name() {
        let src = with(|s| {
            s.nics = ["VM Network", "lan", "VM Network"]
                .into_iter()
                .map(|n| SourceNic {
                    network: n.into(),
                    adapter: "e1000".into(),
                    mac: None,
                })
                .collect();
        });
        let (p, _) = propose(&src, &["lan".into(), "dmz".into()]);
        assert_eq!(
            p.networks,
            BTreeMap::from([
                ("VM Network".to_string(), None),
                ("lan".to_string(), Some("lan".to_string())),
            ])
        );
    }

    #[test]
    fn source_macs_warn_to_power_off_the_original() {
        let (_, warnings) = propose_none(&with(|s| {
            s.nics.push(SourceNic {
                network: "lan".into(),
                adapter: "vmxnet3".into(),
                mac: Some("00:50:56:aa:bb:cc".parse().unwrap()),
            })
        }));
        assert_eq!(
            warnings,
            [
                "power off the VMware VM before starting the imported one; it keeps the same MAC addresses"
            ]
        );
    }
}
