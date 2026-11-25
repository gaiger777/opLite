//! vSphere 인벤토리(설계 §7.3): 루트 폴더 위 컨테이너 보기 하나 + `RetrievePropertiesEx`(쪽 나눔)로 데이터센터·
//! 폴더·컴퓨트 리소스(클러스터)·호스트·데이터스토어·네트워크·VM의 필요한 속성만 읽고 `parent`로 나무를 잇는다.
//! VM 요약 [`InvVm`]은 계획 29 [`SourceVm`]으로 바꿀 수 있다(디스크 순서는 `.vmx` 규칙과 같다).

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use orbivisor_types::vm::{Firmware, MacAddr};
use orbivisor_types::vm_import::{SourceDisk, SourceNic, SourceVm};
use orbivisor_types::vmware::{VmwarePower, VmwareSourceVm};
use serde::Serialize;

use super::VsphereError;
use super::client::{ObjectContent, PropSpec, Session, disk_capacity, parse_power};
use super::http::pem_fingerprint;
use super::soap::{MoRef, Node};

/// 보기에 넣는 형식
pub const VIEW_KINDS: [&str; 7] = [
    "Folder",
    "Datacenter",
    "ComputeResource",
    "HostSystem",
    "Datastore",
    "Network",
    "VirtualMachine",
];

pub const FOLDER_PATHS: [&str; 3] = ["name", "parent", "childType"];
pub const DATACENTER_PATHS: [&str; 2] = ["name", "parent"];
pub const COMPUTE_PATHS: [&str; 2] = ["name", "parent"];
pub const HOST_PATHS: [&str; 4] = [
    "name",
    "parent",
    "runtime.connectionState",
    "config.certificate",
];
pub const DATASTORE_PATHS: [&str; 6] = [
    "name",
    "parent",
    "summary.type",
    "summary.capacity",
    "summary.freeSpace",
    "browser",
];
pub const NETWORK_PATHS: [&str; 2] = ["name", "parent"];
/// 분산 포트 그룹(`Network`의 하위 형식 — 위 경로에 더해)
pub const DVPG_PATHS: [&str; 2] = ["key", "config.uplink"];
/// NSX 불투명 네트워크(`Network`의 하위 형식)
pub const OPAQUE_PATHS: [&str; 1] = ["summary.opaqueNetworkId"];
pub const VM_PATHS: [&str; 24] = [
    "name",
    "parent",
    "config.uuid",
    "config.instanceUuid",
    "config.guestId",
    "config.guestFullName",
    "config.firmware",
    "config.version",
    "config.template",
    "config.changeTrackingEnabled",
    "config.hardware.numCPU",
    "config.hardware.numCoresPerSocket",
    "config.hardware.memoryMB",
    "config.hardware.device",
    "config.annotation",
    "config.extraConfig",
    "runtime.powerState",
    "runtime.host",
    "runtime.faultToleranceState",
    "snapshot",
    "guest.toolsRunningStatus",
    "guest.net",
    "summary.storage.committed",
    "resourcePool",
];

/// 인벤토리를 읽는 속성 묶음
pub fn prop_specs() -> Vec<PropSpec<'static>> {
    vec![
        PropSpec {
            kind: "Folder",
            paths: &FOLDER_PATHS,
        },
        PropSpec {
            kind: "Datacenter",
            paths: &DATACENTER_PATHS,
        },
        PropSpec {
            kind: "ComputeResource",
            paths: &COMPUTE_PATHS,
        },
        PropSpec {
            kind: "HostSystem",
            paths: &HOST_PATHS,
        },
        PropSpec {
            kind: "Datastore",
            paths: &DATASTORE_PATHS,
        },
        PropSpec {
            kind: "Network",
            paths: &NETWORK_PATHS,
        },
        PropSpec {
            kind: "DistributedVirtualPortgroup",
            paths: &DVPG_PATHS,
        },
        PropSpec {
            kind: "OpaqueNetwork",
            paths: &OPAQUE_PATHS,
        },
        PropSpec {
            kind: "VirtualMachine",
            paths: &VM_PATHS,
        },
    ]
}

/// 디스크 컨트롤러 종류의 차례(계획 30 `.vmx`와 같다)
const CONTROLLERS: [&str; 4] = ["ide", "sata", "scsi", "nvme"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvDatacenter {
    pub moref: String,
    pub name: String,
    /// 루트 폴더 아래 경로(`DC1`, `Europe/DC2`) — `/folder`의 `dcPath`
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvFolder {
    pub moref: String,
    pub name: String,
    pub parent: Option<String>,
    /// `DC1/vm/Prod`
    pub path: String,
    /// 담는 형식(`VirtualMachine`, `Datacenter` …)
    pub child_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvCompute {
    pub moref: String,
    pub name: String,
    /// 클러스터(`ClusterComputeResource`)인가 — 아니면 단독 호스트의 컴퓨트 리소스
    pub cluster: bool,
    pub path: String,
    /// 데이터센터 경로(`DC1`, `Europe/DC2` — `/folder`의 `dcPath`)
    pub datacenter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvHost {
    pub moref: String,
    /// 관리 주소(vCenter에 더한 이름 — 보통 FQDN이나 IP)
    pub name: String,
    pub compute: Option<String>,
    /// 클러스터 이름(단독 호스트면 없음)
    pub cluster: Option<String>,
    pub connection_state: Option<String>,
    /// vCenter가 아는 호스트 인증서의 SHA-256 지문(`config.certificate`)
    pub certificate_sha256: Option<String>,
    /// 데이터센터 경로(`DC1`, `Europe/DC2` — `/folder`의 `dcPath`)
    pub datacenter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvDatastore {
    pub moref: String,
    pub name: String,
    /// `summary.type`(`VMFS`, `NFS`, `NFS41`, `vsan`, `VVOL` …)
    pub kind: String,
    pub capacity: u64,
    pub free: u64,
    pub browser: Option<String>,
    /// 데이터센터 경로(`DC1`, `Europe/DC2` — `/folder`의 `dcPath`)
    pub datacenter: Option<String>,
}

impl InvDatastore {
    /// 파일 시스템이 아니어서 `/folder`로 디스크를 읽을 수 없다(vSAN·vVols — VDDK 길이 필요).
    pub fn needs_vddk(&self) -> bool {
        let k = self.kind.to_ascii_lowercase();
        k.starts_with("vsan") || k == "vvol"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkKind {
    Standard,
    DvPortgroup,
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvNetwork {
    pub moref: String,
    pub name: String,
    pub kind: NetworkKind,
    /// 분산 포트 그룹 키(`dvportgroup-12`)
    pub key: Option<String>,
    /// NSX 불투명 네트워크 id
    pub opaque_id: Option<String>,
    /// 데이터센터 경로(`DC1`, `Europe/DC2` — `/folder`의 `dcPath`)
    pub datacenter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvDisk {
    /// 장치 키
    pub key: i32,
    /// 기반 디스크 디스크립터(`[ds] web/web.vmdk` — 스냅샷이 있어도 체인의 맨 아래)
    pub file: String,
    /// 지금 쓰는 디스크(스냅샷이 있으면 델타 `web-000001.vmdk`)
    pub current_file: String,
    pub capacity: u64,
    /// `persistent`, `independent_persistent`, `independent_nonpersistent` …
    pub mode: String,
    pub thin: bool,
    /// `flat` | `rdm` | `sparse` | `other`
    pub backing: String,
    /// `sharingMultiWriter`면 여러 VM이 쓴다
    pub multi_writer: bool,
    pub datastore: Option<String>,
    /// `ide`·`sata`·`scsi`·`nvme`
    pub controller: String,
    pub bus: u32,
    pub unit: u32,
    pub change_id: Option<String>,
}

impl InvDisk {
    pub fn independent(&self) -> bool {
        self.mode.starts_with("independent")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvNic {
    pub key: i32,
    /// 네트워크 이름(분산 포트 그룹은 포트 그룹 이름, 모르면 `dvs:{키}`·`nsx:{id}`)
    pub network: String,
    pub network_ref: Option<String>,
    /// `vmxnet3`, `e1000`, `e1000e` …(`.vmx` `virtualDev`와 같은 이름)
    pub adapter: String,
    pub mac: Option<String>,
    pub start_connected: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvSnapshot {
    pub moref: String,
    pub name: String,
    pub description: String,
    pub id: Option<i64>,
    pub created: Option<String>,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct InvVm {
    pub moref: String,
    pub name: String,
    /// `DC1/vm/Prod/web01`
    pub path: String,
    /// VM이 든 폴더 경로(`DC1/vm/Prod`)
    pub folder: Option<String>,
    /// 데이터센터 경로(`DC1`, `Europe/DC2` — `/folder`의 `dcPath`)
    pub datacenter: Option<String>,
    pub uuid: Option<String>,
    pub instance_uuid: Option<String>,
    pub host: Option<String>,
    pub host_name: Option<String>,
    pub cluster: Option<String>,
    pub power: Option<VmwarePower>,
    pub guest_id: String,
    pub guest_full_name: Option<String>,
    pub firmware: Firmware,
    /// `vmx-NN`의 NN
    pub hw_version: Option<u32>,
    pub cpus: u32,
    pub cores_per_socket: u32,
    pub memory_mib: u64,
    pub disks: Vec<InvDisk>,
    pub nics: Vec<InvNic>,
    pub snapshots: Vec<InvSnapshot>,
    pub current_snapshot: Option<String>,
    pub cbt: bool,
    pub tools: Option<String>,
    pub ft: bool,
    pub template: bool,
    pub annotation: Option<String>,
    /// Tools가 알린 MAC(소문자) → IP
    #[schema(value_type = BTreeMap<String, Vec<String>>)]
    pub guest_ips: BTreeMap<String, Vec<IpAddr>>,
    pub committed_bytes: u64,
    pub resource_pool: Option<String>,
}

impl InvVm {
    /// Tools가 돌고 있나(`ShutdownGuest`에 필요)
    pub fn tools_running(&self) -> bool {
        self.tools.as_deref() == Some("guestToolsRunning")
    }

    /// 계획 29 원본 VM 꼴로(`propose`·`import_spec`가 받는 값)
    pub fn to_source(&self) -> VmwareSourceVm {
        let disks = self
            .disks
            .iter()
            .enumerate()
            .map(|(i, d)| SourceDisk {
                name: format!("disk{i}"),
                file: d.file.clone(),
                capacity_bytes: d.capacity,
                controller: d.controller.clone(),
            })
            .collect();
        let nics = self
            .nics
            .iter()
            .map(|n| SourceNic {
                network: n.network.clone(),
                adapter: n.adapter.clone(),
                mac: n.mac.as_deref().and_then(|m| m.parse::<MacAddr>().ok()),
            })
            .collect();
        VmwareSourceVm {
            vm: SourceVm {
                name: self.name.clone(),
                guest_os: self.guest_id.clone(),
                firmware: self.firmware,
                vcpus: self.cpus,
                cores_per_socket: self.cores_per_socket,
                memory_mib: self.memory_mib,
                disks,
                nics,
            },
            host: self.host_name.clone(),
            tools: self.tools.clone(),
            power: self.power,
            guest_ips: self.guest_ips.clone(),
        }
    }
}

/// 읽은 인벤토리
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Inventory {
    pub datacenters: Vec<InvDatacenter>,
    pub folders: Vec<InvFolder>,
    pub computes: Vec<InvCompute>,
    pub hosts: Vec<InvHost>,
    pub datastores: Vec<InvDatastore>,
    pub networks: Vec<InvNetwork>,
    pub vms: Vec<InvVm>,
}

impl Inventory {
    pub fn vm(&self, moref: &str) -> Option<&InvVm> {
        self.vms.iter().find(|v| v.moref == moref)
    }

    pub fn vm_by_path(&self, path: &str) -> Option<&InvVm> {
        self.vms.iter().find(|v| v.path == path)
    }

    pub fn host(&self, moref: &str) -> Option<&InvHost> {
        self.hosts.iter().find(|h| h.moref == moref)
    }

    pub fn datastore_named(&self, name: &str) -> Option<&InvDatastore> {
        self.datastores.iter().find(|d| d.name == name)
    }
}

/// 원본의 인벤토리를 읽는다(보기를 만들고 다 읽은 뒤 지운다).
pub async fn inventory(session: &Session) -> Result<Inventory, VsphereError> {
    let root = session.content().root_folder.clone();
    let view = session.create_container_view(&root, &VIEW_KINDS).await?;
    let objects = session.retrieve_view(&view, &prop_specs()).await;
    let _ = session.destroy_view(&view).await;
    Ok(build(&root, objects?))
}

/// 읽은 객체들로 나무를 잇는다(순수 함수 — 견본 XML로 시험).
pub fn build(root: &MoRef, objects: Vec<ObjectContent>) -> Inventory {
    let by_ref: HashMap<MoRef, &ObjectContent> =
        objects.iter().map(|o| (o.obj.clone(), o)).collect();
    let name_of = |r: &MoRef| by_ref.get(r).and_then(|o| o.string("name"));
    // 루트 폴더 아래 경로(루트 이름 `Datacenters`는 넣지 않는다)
    let path_of = |r: &MoRef| -> String {
        let mut parts = Vec::new();
        let mut at = Some(r.clone());
        let mut guard = 0;
        while let Some(cur) = at {
            if cur == *root || guard > 64 {
                break;
            }
            guard += 1;
            let o = by_ref.get(&cur);
            parts.push(
                o.and_then(|o| o.string("name"))
                    .unwrap_or_else(|| cur.value.clone()),
            );
            at = o.and_then(|o| o.moref("parent"));
        }
        parts.reverse();
        parts.join("/")
    };
    let ancestor = |r: &MoRef, kind: &str| -> Option<MoRef> {
        let mut at = by_ref.get(r).and_then(|o| o.moref("parent"));
        let mut guard = 0;
        while let Some(cur) = at {
            if cur.kind == kind {
                return Some(cur);
            }
            guard += 1;
            if guard > 64 {
                return None;
            }
            at = by_ref.get(&cur).and_then(|o| o.moref("parent"));
        }
        None
    };
    // 데이터센터 경로(`/folder`의 `dcPath` — 폴더 안 데이터센터는 `Europe/DC2`)
    let datacenter_of = |r: &MoRef| ancestor(r, "Datacenter").map(|d| path_of(&d));

    let mut inv = Inventory::default();
    for o in &objects {
        let name = o.string("name").unwrap_or_else(|| o.obj.value.clone());
        match o.obj.kind.as_str() {
            "Datacenter" => inv.datacenters.push(InvDatacenter {
                moref: o.obj.value.clone(),
                name,
                path: path_of(&o.obj),
            }),
            "Folder" if o.obj != *root => inv.folders.push(InvFolder {
                moref: o.obj.value.clone(),
                name,
                parent: o.moref("parent").map(|p| p.value),
                path: path_of(&o.obj),
                child_types: o
                    .get("childType")
                    .map(|n| {
                        n.children
                            .iter()
                            .map(|c| c.text.trim().to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            "ComputeResource" | "ClusterComputeResource" => inv.computes.push(InvCompute {
                moref: o.obj.value.clone(),
                name,
                cluster: o.obj.kind == "ClusterComputeResource",
                path: path_of(&o.obj),
                datacenter: datacenter_of(&o.obj),
            }),
            "HostSystem" => {
                let compute = o.moref("parent");
                let cluster = compute
                    .as_ref()
                    .filter(|c| c.kind == "ClusterComputeResource")
                    .and_then(&name_of);
                inv.hosts.push(InvHost {
                    moref: o.obj.value.clone(),
                    name,
                    compute: compute.map(|c| c.value),
                    cluster,
                    connection_state: o.string("runtime.connectionState"),
                    certificate_sha256: o
                        .get("config.certificate")
                        .and_then(|n| pem_fingerprint(&byte_array(n))),
                    datacenter: datacenter_of(&o.obj),
                });
            }
            "Datastore" => inv.datastores.push(InvDatastore {
                moref: o.obj.value.clone(),
                name,
                kind: o.string("summary.type").unwrap_or_default(),
                capacity: o.parse_num("summary.capacity").unwrap_or(0),
                free: o.parse_num("summary.freeSpace").unwrap_or(0),
                browser: o.moref("browser").map(|b| b.value),
                datacenter: datacenter_of(&o.obj),
            }),
            "Network" | "DistributedVirtualPortgroup" | "OpaqueNetwork" => {
                // 분산 스위치의 업링크 포트 그룹은 VM이 붙는 네트워크가 아니다.
                if o.bool("config.uplink") == Some(true) {
                    continue;
                }
                let kind = match o.obj.kind.as_str() {
                    "DistributedVirtualPortgroup" => NetworkKind::DvPortgroup,
                    "OpaqueNetwork" => NetworkKind::Opaque,
                    _ => NetworkKind::Standard,
                };
                inv.networks.push(InvNetwork {
                    moref: o.obj.value.clone(),
                    name,
                    kind,
                    key: (kind == NetworkKind::DvPortgroup)
                        .then(|| o.string("key").unwrap_or_else(|| o.obj.value.clone())),
                    opaque_id: o.string("summary.opaqueNetworkId"),
                    datacenter: datacenter_of(&o.obj),
                });
            }
            _ => {}
        }
    }
    for o in &objects {
        if o.obj.kind != "VirtualMachine" {
            continue;
        }
        let parent = o.moref("parent");
        let host = o.moref("runtime.host");
        let host_inv = host.as_ref().and_then(|h| inv.host(&h.value)).cloned();
        let vm = vm_from(
            o,
            &inv,
            VmPlace {
                path: path_of(&o.obj),
                folder: parent.as_ref().map(path_of),
                datacenter: datacenter_of(&o.obj),
                host: host.map(|h| h.value),
                host_name: host_inv.as_ref().map(|h| h.name.clone()),
                cluster: host_inv.and_then(|h| h.cluster),
            },
        );
        inv.vms.push(vm);
    }
    inv.vms.sort_by(|a, b| a.path.cmp(&b.path));
    inv.folders.sort_by(|a, b| a.path.cmp(&b.path));
    inv
}

/// VM 하나를 다시 읽는다(동기화 회차마다 — 스냅샷·디스크·전원·CBT·호스트가 그새 바뀌었을 수 있다). 경로·폴더·데이터센터는
/// `inv`의 것을 쓰고 호스트는 지금 `runtime.host`로 `inv`에서 찾는다. `inv`에 없거나 원본에서 없어진 VM은 `NotFound`.
pub async fn refresh_vm(
    session: &Session,
    inv: &Inventory,
    moref: &str,
) -> Result<InvVm, VsphereError> {
    let cached = inv.vm(moref).ok_or_else(|| VsphereError::NotFound {
        kind: "ManagedObjectNotFound".into(),
        message: format!("VM {moref} is not in the inventory"),
    })?;
    let o = session
        .retrieve_one(&MoRef::new("VirtualMachine", moref), &VM_PATHS)
        .await?;
    let host = o.moref("runtime.host");
    let host_inv = host.as_ref().and_then(|h| inv.host(&h.value));
    let place = VmPlace {
        path: cached.path.clone(),
        folder: cached.folder.clone(),
        datacenter: cached.datacenter.clone(),
        host_name: host_inv.map(|h| h.name.clone()),
        cluster: host_inv.and_then(|h| h.cluster.clone()),
        host: host.map(|h| h.value),
    };
    Ok(vm_from(&o, inv, place))
}

struct VmPlace {
    path: String,
    folder: Option<String>,
    datacenter: Option<String>,
    host: Option<String>,
    host_name: Option<String>,
    cluster: Option<String>,
}

/// `ArrayOfByte`(`<byte>45</byte>…`, xsd:byte는 부호 있음)나 base64 글자를 바이트로
fn byte_array(n: &Node) -> Vec<u8> {
    if n.children.is_empty() {
        use base64::Engine as _;
        return base64::engine::general_purpose::STANDARD
            .decode(n.text.trim())
            .unwrap_or_default();
    }
    n.children
        .iter()
        .filter_map(|b| b.text.trim().parse::<i16>().ok())
        .map(|v| v as u8)
        .collect()
}

fn controller_kind(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "VirtualIDEController" => "ide",
        "VirtualAHCIController" | "VirtualSATAController" => "sata",
        "ParaVirtualSCSIController"
        | "VirtualLsiLogicController"
        | "VirtualLsiLogicSASController"
        | "VirtualBusLogicController"
        | "VirtualSCSIController" => "scsi",
        "VirtualNVMEController" => "nvme",
        _ => return None,
    })
}

fn nic_adapter(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "VirtualVmxnet3" => "vmxnet3",
        "VirtualVmxnet3Vrdma" => "vrdma",
        "VirtualVmxnet2" => "vmxnet2",
        "VirtualVmxnet" => "vmxnet",
        "VirtualE1000" => "e1000",
        "VirtualE1000e" => "e1000e",
        "VirtualPCNet32" => "vlance",
        "VirtualSriovEthernetCard" => "sriov",
        _ => return None,
    })
}

fn disk_backing_kind(ty: Option<&str>) -> &'static str {
    match ty.unwrap_or_default() {
        "VirtualDiskFlatVer2BackingInfo" | "VirtualDiskFlatVer1BackingInfo" => "flat",
        "VirtualDiskRawDiskMappingVer1BackingInfo" | "VirtualDiskRawDiskVer2BackingInfo" => "rdm",
        "VirtualDiskSeSparseBackingInfo"
        | "VirtualDiskSparseVer2BackingInfo"
        | "VirtualDiskSparseVer1BackingInfo" => "sparse",
        _ => "other",
    }
}

fn vm_from(o: &ObjectContent, inv: &Inventory, place: VmPlace) -> InvVm {
    let devices: Vec<&Node> = o
        .get("config.hardware.device")
        .map(|n| n.children.iter().collect())
        .unwrap_or_default();
    // 컨트롤러 키 → (종류, 버스)
    let controllers: HashMap<i32, (&'static str, u32)> = devices
        .iter()
        .filter_map(|d| {
            let kind = controller_kind(d.ty()?)?;
            Some((
                d.parse_at(&["key"])?,
                (kind, d.parse_at(&["busNumber"]).unwrap_or(0)),
            ))
        })
        .collect();
    let ds_name = |r: Option<MoRef>| {
        r.and_then(|r| inv.datastores.iter().find(|d| d.moref == r.value))
            .map(|d| d.name.clone())
    };
    let mut disks = Vec::new();
    let mut nics = Vec::new();
    for d in &devices {
        let Some(ty) = d.ty() else { continue };
        let Some(key) = d.parse_at::<i32>(&["key"]) else {
            continue;
        };
        if ty == "VirtualDisk" {
            let backing = d.child("backing");
            let current = backing
                .and_then(|b| b.string_at(&["fileName"]))
                .unwrap_or_default();
            // 스냅샷 델타의 `parent`를 따라 맨 아래 기반 디스크로
            let mut base = backing;
            while let Some(p) = base.and_then(|b| b.child("parent")) {
                base = Some(p);
            }
            let (controller, bus) = d
                .parse_at::<i32>(&["controllerKey"])
                .and_then(|k| controllers.get(&k).copied())
                .unwrap_or(("scsi", 0));
            disks.push(InvDisk {
                key,
                file: base
                    .and_then(|b| b.string_at(&["fileName"]))
                    .unwrap_or_else(|| current.clone()),
                current_file: current,
                capacity: disk_capacity(d),
                mode: backing
                    .and_then(|b| b.string_at(&["diskMode"]))
                    .unwrap_or_else(|| "persistent".into()),
                thin: backing
                    .and_then(|b| b.bool_at(&["thinProvisioned"]))
                    .unwrap_or(false),
                backing: disk_backing_kind(backing.and_then(Node::ty)).to_string(),
                multi_writer: backing.and_then(|b| b.text_at(&["sharing"]))
                    == Some("sharingMultiWriter"),
                datastore: ds_name(backing.and_then(|b| b.moref_at(&["datastore"]))),
                controller: controller.to_string(),
                bus,
                unit: d.parse_at(&["unitNumber"]).unwrap_or(0),
                change_id: backing.and_then(|b| b.string_at(&["changeId"])),
            });
        } else if let Some(adapter) = nic_adapter(ty) {
            let backing = d.child("backing");
            let (network, network_ref) = nic_network(backing, inv);
            nics.push(InvNic {
                key,
                network,
                network_ref,
                adapter: adapter.to_string(),
                mac: d.string_at(&["macAddress"]).map(|m| m.to_ascii_lowercase()),
                start_connected: d.bool_at(&["connectable", "startConnected"]),
            });
        }
    }
    // 차례: `bios.hddOrder`에 적힌 디스크가 먼저, 그다음 컨트롤러 종류 → 버스 → 유닛
    let extra: HashMap<String, String> = o
        .get("config.extraConfig")
        .map(|n| {
            n.children
                .iter()
                .filter_map(|v| Some((v.string_at(&["key"])?, v.string_at(&["value"])?)))
                .collect()
        })
        .unwrap_or_default();
    let hdd_order: Vec<String> = extra
        .get("bios.hddOrder")
        .map(|o| {
            o.split(',')
                .map(|d| d.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    disks.sort_by_key(|d| {
        let label = format!("{}{}:{}", d.controller, d.bus, d.unit);
        let rank = CONTROLLERS
            .iter()
            .position(|c| *c == d.controller)
            .unwrap_or(0);
        (
            hdd_order
                .iter()
                .position(|h| *h == label)
                .unwrap_or(usize::MAX),
            rank,
            d.bus,
            d.unit,
        )
    });
    nics.sort_by_key(|n| n.key);

    let mut snapshots = Vec::new();
    let tree = o.get("snapshot");
    if let Some(tree) = tree {
        for s in tree.children_named("rootSnapshotList") {
            flatten_snapshots(s, None, &mut snapshots);
        }
    }
    let guest_ips = o
        .get("guest.net")
        .map(|n| {
            n.children
                .iter()
                .filter_map(|g| {
                    let mac = g.string_at(&["macAddress"])?.to_ascii_lowercase();
                    let ips: Vec<IpAddr> = g
                        .children_named("ipAddress")
                        .filter_map(|i| i.text.trim().parse().ok())
                        .collect();
                    (!ips.is_empty()).then_some((mac, ips))
                })
                .collect()
        })
        .unwrap_or_default();
    let ft = o
        .text("runtime.faultToleranceState")
        .is_some_and(|s| s != "notConfigured");
    InvVm {
        moref: o.obj.value.clone(),
        name: o.string("name").unwrap_or_else(|| o.obj.value.clone()),
        path: place.path,
        folder: place.folder,
        datacenter: place.datacenter,
        uuid: o.string("config.uuid"),
        instance_uuid: o.string("config.instanceUuid"),
        host: place.host,
        host_name: place.host_name,
        cluster: place.cluster,
        power: o.text("runtime.powerState").and_then(parse_power),
        guest_id: o
            .string("config.guestId")
            .unwrap_or_else(|| "otherGuest".into()),
        guest_full_name: o.string("config.guestFullName"),
        firmware: if o.text("config.firmware") == Some("efi") {
            Firmware::Uefi
        } else {
            Firmware::Bios
        },
        hw_version: o
            .text("config.version")
            .and_then(|v| v.strip_prefix("vmx-"))
            .and_then(|v| v.parse().ok()),
        cpus: o.parse_num("config.hardware.numCPU").unwrap_or(1),
        cores_per_socket: o
            .parse_num("config.hardware.numCoresPerSocket")
            .filter(|c| *c > 0)
            .unwrap_or(1),
        memory_mib: o.parse_num("config.hardware.memoryMB").unwrap_or(0),
        disks,
        nics,
        snapshots,
        current_snapshot: tree
            .and_then(|t| t.moref_at(&["currentSnapshot"]))
            .map(|m| m.value),
        cbt: o.bool("config.changeTrackingEnabled").unwrap_or(false),
        tools: o.string("guest.toolsRunningStatus"),
        ft,
        template: o.bool("config.template").unwrap_or(false),
        annotation: o.string("config.annotation").filter(|a| !a.is_empty()),
        guest_ips,
        committed_bytes: o.parse_num("summary.storage.committed").unwrap_or(0),
        resource_pool: o.moref("resourcePool").map(|r| r.value),
    }
}

fn nic_network(backing: Option<&Node>, inv: &Inventory) -> (String, Option<String>) {
    let Some(b) = backing else {
        return (String::new(), None);
    };
    match b.ty().unwrap_or_default() {
        "VirtualEthernetCardDistributedVirtualPortBackingInfo" => {
            let key = b.string_at(&["port", "portgroupKey"]).unwrap_or_default();
            match inv
                .networks
                .iter()
                .find(|n| n.key.as_deref() == Some(key.as_str()) || n.moref == key)
            {
                Some(n) => (n.name.clone(), Some(n.moref.clone())),
                None => (format!("dvs:{key}"), None),
            }
        }
        "VirtualEthernetCardOpaqueNetworkBackingInfo" => {
            let id = b.string_at(&["opaqueNetworkId"]).unwrap_or_default();
            match inv
                .networks
                .iter()
                .find(|n| n.opaque_id.as_deref() == Some(id.as_str()))
            {
                Some(n) => (n.name.clone(), Some(n.moref.clone())),
                None => (format!("nsx:{id}"), None),
            }
        }
        _ => {
            let by_ref = b
                .moref_at(&["network"])
                .and_then(|r| inv.networks.iter().find(|n| n.moref == r.value));
            match (by_ref, b.string_at(&["deviceName"])) {
                (Some(n), _) => (n.name.clone(), Some(n.moref.clone())),
                (None, Some(name)) => (name, None),
                (None, None) => (String::new(), None),
            }
        }
    }
}

fn flatten_snapshots(node: &Node, parent: Option<&str>, out: &mut Vec<InvSnapshot>) {
    let Some(moref) = node.moref_at(&["snapshot"]) else {
        return;
    };
    out.push(InvSnapshot {
        moref: moref.value.clone(),
        name: node.string_at(&["name"]).unwrap_or_default(),
        description: node.string_at(&["description"]).unwrap_or_default(),
        id: node.parse_at(&["id"]),
        created: node.string_at(&["createTime"]),
        parent: parent.map(str::to_string),
    });
    for child in node.children_named("childSnapshotList") {
        flatten_snapshots(child, Some(&moref.value), out);
    }
}

/// `RetrievePropertiesEx`·`ContinueRetrievePropertiesEx` 응답 요소에서 객체들(견본 시험용)
pub fn objects_of(response: &Node) -> (Vec<ObjectContent>, Option<String>) {
    let Some(v) = response.child("returnval") else {
        return (Vec::new(), None);
    };
    (
        v.children_named("objects")
            .filter_map(ObjectContent::parse)
            .collect(),
        v.string_at(&["token"]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vmware::soap::{Reply, parse_reply};

    fn objects(xml: &str) -> Vec<ObjectContent> {
        let Reply::Ok(node) = parse_reply(xml).unwrap() else {
            panic!("fault")
        };
        objects_of(&node).0
    }

    #[test]
    fn builds_the_vcsim_inventory() {
        let mut all = objects(include_str!(
            "../../tests/fixtures/vsphere/inventory-page1.xml"
        ));
        let Reply::Ok(page1) = parse_reply(include_str!(
            "../../tests/fixtures/vsphere/inventory-page1.xml"
        ))
        .unwrap() else {
            panic!()
        };
        assert!(objects_of(&page1).1.is_some(), "page 1 has a token");
        all.extend(objects(include_str!(
            "../../tests/fixtures/vsphere/inventory-page2.xml"
        )));
        let inv = build(&MoRef::new("Folder", "group-d1"), all);
        assert_eq!(inv.datacenters.len(), 1);
        assert_eq!(inv.datacenters[0].path, "DC0");
        assert_eq!(inv.vms.len(), 4);
        let vm = inv.vm_by_path("DC0/vm/DC0_H0_VM0").unwrap();
        assert_eq!(vm.folder.as_deref(), Some("DC0/vm"));
        assert_eq!(vm.datacenter.as_deref(), Some("DC0"));
        assert_eq!(vm.disks.len(), 1);
        assert_eq!(vm.disks[0].file, "[LocalDS_0] DC0_H0_VM0/disk1.vmdk");
        assert_eq!(vm.disks[0].controller, "scsi");
        assert_eq!(vm.disks[0].datastore.as_deref(), Some("LocalDS_0"));
        assert_eq!(vm.disks[0].capacity, 10 << 30);
        assert!(vm.disks[0].thin);
        // 분산 포트 그룹 키가 이름으로 풀린다(계획 30 `.vmx`는 `dvs:…`로만 보였다).
        assert_eq!(vm.nics[0].network, "DC0_DVPG0");
        assert_eq!(vm.nics[0].adapter, "e1000");
        assert_eq!(vm.hw_version, Some(13));
        // 견본을 잡기 전에 이 VM에 ShutdownGuest를 보냈다
        assert_eq!(vm.power, Some(VmwarePower::PoweredOff));
        assert!(vm.host_name.is_some());
        // vcsim은 `config.uplink`를 주지 않아 업링크 포트 그룹도 목록에 있다(실제 vCenter는 뺀다 — 아래 시험).
        assert!(inv.networks.iter().any(|n| n.name.contains("DVUplinks")));
        let src = vm.to_source();
        assert_eq!(src.vm.disks[0].name, "disk0");
        assert_eq!(
            src.vm.nics[0].mac.map(|m| m.to_string()).as_deref(),
            Some("00:0c:29:63:65:61")
        );
        // 호스트 인증서가 지문으로
        assert!(
            inv.hosts.iter().all(|h| h.certificate_sha256.is_some()),
            "{:?}",
            inv.hosts
        );
        // 클러스터 안 호스트의 VM은 클러스터 이름을 안다
        let c = inv.vms.iter().find(|v| v.name == "DC0_C0_RP0_VM0").unwrap();
        assert_eq!(c.cluster.as_deref(), Some("DC0_C0"));
    }

    #[test]
    fn real_vcenter_vm_with_snapshots_and_two_controllers() {
        let inv = build(
            &MoRef::new("Folder", "group-d1"),
            objects(include_str!("../../tests/fixtures/vsphere/vcenter-vm.xml")),
        );
        let vm = &inv.vms[0];
        assert_eq!(vm.path, "Lab/vm/Prod/web01");
        // hddOrder가 scsi0:1을 앞에 둔다; sata는 scsi보다 앞 컨트롤러 차례지만 hddOrder가 먼저
        let order: Vec<(&str, u32, u32)> = vm
            .disks
            .iter()
            .map(|d| (d.controller.as_str(), d.bus, d.unit))
            .collect();
        assert_eq!(order, vec![("scsi", 0, 1), ("sata", 0, 0), ("scsi", 0, 0)]);
        // 스냅샷 델타의 parent 체인 맨 아래가 기반
        let d = vm
            .disks
            .iter()
            .find(|d| d.unit == 0 && d.controller == "scsi")
            .unwrap();
        assert_eq!(d.file, "[ds1] web01/web01.vmdk");
        assert_eq!(d.current_file, "[ds1] web01/web01-000002.vmdk");
        assert_eq!(
            d.change_id.as_deref(),
            Some("52 1d 3f 4a 52 7b 0e 2e-6a 4f 00 44 4d 28 58 0e/12")
        );
        assert_eq!(vm.snapshots.len(), 2);
        assert_eq!(vm.snapshots[1].parent.as_deref(), Some("snapshot-101"));
        assert_eq!(vm.current_snapshot.as_deref(), Some("snapshot-102"));
        assert!(vm.cbt);
        assert!(vm.tools_running());
        assert_eq!(vm.firmware, Firmware::Uefi);
        assert_eq!(vm.guest_ips.get("00:50:56:aa:bb:01").map(Vec::len), Some(2));
        let multi = vm.disks.iter().find(|d| d.controller == "sata").unwrap();
        assert!(multi.independent());
        assert_eq!(multi.backing, "flat");
        let nic = &vm.nics[0];
        assert_eq!(nic.network, "VM Network");
        assert_eq!(nic.adapter, "vmxnet3");
        assert_eq!(nic.start_connected, Some(true));
        assert_eq!(vm.nics[1].network, "nsx:seg-1");
        assert!(inv.datastores.iter().any(|d| d.needs_vddk()));
        assert!(inv.networks.iter().all(|n| !n.name.contains("DVUplinks")));
        assert_eq!(vm.host_name.as_deref(), Some("esx1.lab.local"));
        assert_eq!(vm.cluster.as_deref(), Some("Cluster1"));
    }

    #[test]
    fn datacenters_in_folders_have_a_path() {
        let obj = |kind: &str, value: &str, name: &str, parent: Option<(&str, &str)>| {
            let mut props = BTreeMap::new();
            props.insert(
                "name".to_string(),
                Node {
                    name: "val".into(),
                    text: name.into(),
                    ..Node::default()
                },
            );
            if let Some((k, v)) = parent {
                props.insert(
                    "parent".to_string(),
                    Node {
                        name: "val".into(),
                        mo_type: Some(k.into()),
                        text: v.into(),
                        ..Node::default()
                    },
                );
            }
            ObjectContent {
                obj: MoRef::new(kind, value),
                props,
            }
        };
        let inv = build(
            &MoRef::new("Folder", "group-d1"),
            vec![
                obj("Folder", "group-d1", "Datacenters", None),
                obj("Folder", "group-x", "Europe", Some(("Folder", "group-d1"))),
                obj("Datacenter", "dc-2", "DC2", Some(("Folder", "group-x"))),
                obj("Folder", "group-v9", "vm", Some(("Datacenter", "dc-2"))),
                obj(
                    "VirtualMachine",
                    "vm-1",
                    "app",
                    Some(("Folder", "group-v9")),
                ),
            ],
        );
        assert_eq!(inv.datacenters[0].path, "Europe/DC2");
        let vm = &inv.vms[0];
        assert_eq!(vm.path, "Europe/DC2/vm/app");
        assert_eq!(vm.datacenter.as_deref(), Some("Europe/DC2"));
        // 루트 폴더는 목록에 없다
        assert_eq!(inv.folders.len(), 2);
    }
}
