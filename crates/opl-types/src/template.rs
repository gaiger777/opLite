//! 템플릿에서 배포할 때 바꾸는 값과 이미지 받기 → 템플릿 초안(계획 58).
//!
//! API가 `POST /vms/{key}/clone`에서 [`apply_overrides`]를 미리 적용해 VM 만들기와 같은 검사를 하고, 복제 Task가
//! 실행 때 다시 적용한다.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::cloud_init::CloudInit;
use crate::vm::{
    Arch, CpuModel, CpuSpec, DiskBus, DiskSpec, Firmware, MachineType, NicModel, NicSpec,
    PowerState, RestartPolicy, ValidationError, VmSpec,
};

/// 템플릿 초안의 기본 vCPU 수와 메모리(MiB)
pub const DEFAULT_TEMPLATE_VCPUS: u16 = 2;
pub const DEFAULT_TEMPLATE_MEMORY_MIB: u32 = 2048;

/// 배포(복제) 때 원본 스펙에서 바꾸는 값. 비어 있는 칸은 원본 그대로다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CloneOverrides {
    /// 새 VM을 켤 노드(`spec.node`). 디스크가 닿아야 한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<Uuid>,
    /// vCPU 수(소켓 하나에 코어로 둔다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vcpus: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub memory_mib: Option<u32>,
    /// NIC 순번별 새 네트워크 이름. null은 그대로다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nics: Vec<Option<String>>,
    /// 디스크 순번별 새 크기(바이트, 늘리기만). null은 그대로다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub disks: Vec<Option<u64>>,
    /// 주어진·비지 않은 칸만 원본의 cloud-init 설정을 바꾼다. 비밀번호는 crypt 해시다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cloud_init: Option<CloudInit>,
    /// 만든 뒤 켠다(`power: running`).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub start: bool,
    /// 새 VM을 둘 폴더(계획 62). 없으면 원본의 폴더다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub folder: Option<Uuid>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl CloneOverrides {
    /// 바꾸는 것이 없는지
    pub fn is_empty(&self) -> bool {
        *self == CloneOverrides::default()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TemplateError {
    #[error("nics: {given} given but the VM has {has} NIC(s)")]
    TooManyNics { given: usize, has: usize },
    #[error("disks: {given} given but the VM has {has} disk(s)")]
    TooManyDisks { given: usize, has: usize },
    #[error(
        "disk{disk}: {size} bytes is smaller than the current {current} bytes; disks can only grow"
    )]
    DiskShrink {
        disk: usize,
        size: u64,
        current: u64,
    },
    #[error(transparent)]
    Invalid(#[from] ValidationError),
}

/// `spec`에 `ov`를 적용한 새 스펙. vCPU·메모리·노드·NIC 네트워크·cloud-init·`start`를 바꾸고 VM 검증을 한다.
/// 디스크 크기는 볼륨에 있으므로 개수만 보고 [`grown_disks`]가 따로 정한다. `template` 같은 나머지는 그대로 두므로
/// 새 VM의 `template: false`는 부르는 쪽이 정한다.
pub fn apply_overrides(spec: &VmSpec, ov: &CloneOverrides) -> Result<VmSpec, TemplateError> {
    if ov.nics.len() > spec.nics.len() {
        return Err(TemplateError::TooManyNics {
            given: ov.nics.len(),
            has: spec.nics.len(),
        });
    }
    if ov.disks.len() > spec.disks.len() {
        return Err(TemplateError::TooManyDisks {
            given: ov.disks.len(),
            has: spec.disks.len(),
        });
    }
    let mut new = spec.clone();
    if let Some(node) = ov.node {
        new.node = Some(node);
    }
    if let Some(folder) = ov.folder {
        new.folder = Some(folder);
    }
    if let Some(vcpus) = ov.vcpus {
        new.cpu.sockets = 1;
        new.cpu.cores = vcpus;
    }
    if let Some(memory) = ov.memory_mib {
        new.memory_mib = memory;
    }
    for (nic, network) in new.nics.iter_mut().zip(&ov.nics) {
        if let Some(network) = network {
            nic.network = network.clone();
        }
    }
    if let Some(ci) = &ov.cloud_init {
        new.cloud_init = Some(merge_cloud_init(new.cloud_init.take(), ci));
    }
    if ov.start {
        new.power = PowerState::Running;
    }
    new.validate()?;
    Ok(new)
}

/// 원본 cloud-init 설정(없으면 빈 설정)의 칸을 `ov`의 주어진·비지 않은 칸으로 바꾼다.
fn merge_cloud_init(base: Option<CloudInit>, ov: &CloudInit) -> CloudInit {
    let mut ci = base.unwrap_or_default();
    fn set<T: Clone>(to: &mut Option<T>, from: &Option<T>) {
        if from.is_some() {
            to.clone_from(from);
        }
    }
    set(&mut ci.hostname, &ov.hostname);
    set(&mut ci.user, &ov.user);
    set(&mut ci.password, &ov.password);
    set(&mut ci.dns, &ov.dns);
    set(&mut ci.user_data, &ov.user_data);
    set(&mut ci.vendor_data, &ov.vendor_data);
    if !ov.ssh_keys.is_empty() {
        ci.ssh_keys.clone_from(&ov.ssh_keys);
    }
    if !ov.networks.is_empty() {
        ci.networks.clone_from(&ov.networks);
    }
    ci
}

/// 디스크마다 늘릴 새 크기(`current`는 원본 디스크 볼륨의 크기, 순번 = 디스크 순번). 같은 크기는 늘리지 않고(None),
/// 줄이면 거절한다.
pub fn grown_disks(
    ov: &CloneOverrides,
    current: &[u64],
) -> Result<Vec<Option<u64>>, TemplateError> {
    if ov.disks.len() > current.len() {
        return Err(TemplateError::TooManyDisks {
            given: ov.disks.len(),
            has: current.len(),
        });
    }
    current
        .iter()
        .enumerate()
        .map(
            |(disk, &current)| match ov.disks.get(disk).copied().flatten() {
                Some(size) if size < current => Err(TemplateError::DiskShrink {
                    disk,
                    size,
                    current,
                }),
                Some(size) if size > current => Ok(Some(size)),
                _ => Ok(None),
            },
        )
        .collect()
}

/// 카탈로그 이미지를 받은 뒤 만들 템플릿 VM의 초안(`POST /image-catalog/{id}/download`의 `template`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TemplateDraft {
    /// 템플릿 VM 이름
    pub name: String,
    #[serde(default = "default_vcpus")]
    #[cfg_attr(feature = "openapi", schema(default = 2))]
    pub vcpus: u16,
    #[serde(default = "default_memory")]
    #[cfg_attr(feature = "openapi", schema(default = 2048))]
    pub memory_mib: u32,
    /// 받은 디스크를 이 크기로 늘린다(바이트). 없으면 이미지 크기 그대로다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub size_bytes: Option<u64>,
    /// NIC 하나의 네트워크 이름
    pub network: String,
    /// 없으면 빈 설정(cloud-init 기본)이다. 비밀번호는 crypt 해시다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cloud_init: Option<CloudInit>,
    /// 머신 타입. API가 카탈로그 항목의 아키텍처로 채운다(없으면 이 노드의 것).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub machine: Option<MachineType>,
    /// 펌웨어. API가 카탈로그 항목의 것으로 채운다(없으면 UEFI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub firmware: Option<Firmware>,
}

fn default_vcpus() -> u16 {
    DEFAULT_TEMPLATE_VCPUS
}

fn default_memory() -> u32 {
    DEFAULT_TEMPLATE_MEMORY_MIB
}

impl TemplateDraft {
    /// 볼륨 `volume`을 첫 부팅 디스크(`virtio-blk`, `boot_order 1`)로, `network`의 NIC 하나와 cloud-init을 가진
    /// 꺼진 템플릿 VM 스펙
    pub fn vm_spec(&self, volume: &str) -> Result<VmSpec, TemplateError> {
        let spec = VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: self.vcpus,
                model: CpuModel::default(),
                max_vcpus: None,
            },
            memory_mib: self.memory_mib,
            max_memory_mib: None,
            firmware: self.firmware.unwrap_or_default(),
            machine: self.machine.unwrap_or_else(|| Arch::native().machine()),
            disks: vec![DiskSpec {
                volume: volume.into(),
                bus: DiskBus::VirtioBlk,
                boot_order: Some(1),
            }],
            nics: vec![NicSpec {
                network: self.network.clone(),
                model: NicModel::Virtio,
                mac: None,
                security_groups: Vec::new(),
            }],
            power: PowerState::Stopped,
            restart: RestartPolicy::default(),
            node: None,
            snapshot_schedule: None,
            ha: None,
            placement: None,
            replication: Vec::new(),
            cdroms: Vec::new(),
            cloud_init: Some(self.cloud_init.clone().unwrap_or_default()),
            template: true,
            tpm: false,
            secure_boot: false,
            guest_agent: None,
            folder: None,
        };
        spec.validate()?;
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_init::{CloudInitNet, IpConfig};

    fn template() -> VmSpec {
        serde_json::from_value(serde_json::json!({
            "cpu": { "sockets": 2, "cores": 1 },
            "memory_mib": 1024,
            "machine": "q35",
            "disks": [
                { "volume": "tmpl-disk0", "bus": "virtio-blk", "boot_order": 1 },
                { "volume": "tmpl-disk1", "bus": "virtio-blk" }
            ],
            "nics": [
                { "network": "lan", "model": "virtio" },
                { "network": "lan", "model": "virtio" }
            ],
            "node": "0193a6f2-7c4e-7d1a-9b3e-2f4c5d6e7f80",
            "cloud_init": { "user": "orbi", "ssh_keys": ["ssh-ed25519 AAAA"] },
            "template": true
        }))
        .unwrap()
    }

    #[test]
    fn empty_overrides_change_nothing_and_are_not_written() {
        let ov = CloneOverrides::default();
        assert!(ov.is_empty());
        assert_eq!(serde_json::to_string(&ov).unwrap(), "{}");
        assert_eq!(apply_overrides(&template(), &ov).unwrap(), template());
        assert_eq!(grown_disks(&ov, &[10, 20]).unwrap(), [None, None]);
    }

    #[test]
    fn overrides_round_trip_and_reject_unknown_fields() {
        let ov = CloneOverrides {
            node: Some(Uuid::now_v7()),
            vcpus: Some(4),
            memory_mib: Some(4096),
            nics: vec![None, Some("dmz".into())],
            disks: vec![Some(20 << 30)],
            cloud_init: Some(CloudInit {
                hostname: Some("web-1".into()),
                ..CloudInit::default()
            }),
            start: true,
            folder: None,
        };
        assert!(!ov.is_empty());
        let json = serde_json::to_value(&ov).unwrap();
        assert_eq!(json["nics"], serde_json::json!([null, "dmz"]));
        assert_eq!(serde_json::from_value::<CloneOverrides>(json).unwrap(), ov);
        assert!(
            serde_json::from_value::<CloneOverrides>(serde_json::json!({ "cpus": 2 })).is_err()
        );
    }

    #[test]
    fn overrides_change_the_given_values() {
        let node = Uuid::now_v7();
        let ov = CloneOverrides {
            node: Some(node),
            vcpus: Some(4),
            memory_mib: Some(4096),
            nics: vec![None, Some("dmz".into())],
            start: true,
            ..CloneOverrides::default()
        };
        let new = apply_overrides(&template(), &ov).unwrap();
        assert_eq!(new.node, Some(node));
        assert_eq!((new.cpu.sockets, new.cpu.cores, new.cpu.vcpus()), (1, 4, 4));
        assert_eq!(new.memory_mib, 4096);
        assert_eq!(new.nics[0].network, "lan");
        assert_eq!(new.nics[1].network, "dmz");
        assert_eq!(new.power, PowerState::Running);
        // 템플릿 플래그·디스크·cloud-init은 그대로다.
        assert!(new.template);
        assert_eq!(new.disks, template().disks);
        assert_eq!(new.cloud_init, template().cloud_init);
    }

    #[test]
    fn cloud_init_overrides_replace_only_given_fields() {
        let ov = CloneOverrides {
            cloud_init: Some(CloudInit {
                hostname: Some("web-1".into()),
                password: Some("$6$salt$hash".into()),
                networks: vec![CloudInitNet {
                    ipv4: Some(IpConfig::Static {
                        address: "10.0.0.5/24".into(),
                        gateway: None,
                    }),
                    ipv6: None,
                }],
                ..CloudInit::default()
            }),
            ..CloneOverrides::default()
        };
        let ci = apply_overrides(&template(), &ov)
            .unwrap()
            .cloud_init
            .unwrap();
        assert_eq!(ci.hostname.as_deref(), Some("web-1"));
        assert_eq!(ci.password.as_deref(), Some("$6$salt$hash"));
        assert_eq!(ci.user.as_deref(), Some("orbi"));
        assert_eq!(ci.ssh_keys, ["ssh-ed25519 AAAA"]);
        assert_eq!(ci.networks.len(), 1);

        // 원본에 cloud-init이 없으면 주어진 것이 그대로 설정이 된다.
        let mut plain = template();
        plain.cloud_init = None;
        let ci = apply_overrides(&plain, &ov).unwrap().cloud_init.unwrap();
        assert_eq!(ci.user, None);
        assert_eq!(ci.hostname.as_deref(), Some("web-1"));
    }

    #[test]
    fn overrides_are_validated() {
        let too_many = CloneOverrides {
            nics: vec![None, None, Some("x".into())],
            ..CloneOverrides::default()
        };
        assert_eq!(
            apply_overrides(&template(), &too_many),
            Err(TemplateError::TooManyNics { given: 3, has: 2 })
        );
        let too_many = CloneOverrides {
            disks: vec![None, None, None],
            ..CloneOverrides::default()
        };
        assert_eq!(
            apply_overrides(&template(), &too_many),
            Err(TemplateError::TooManyDisks { given: 3, has: 2 })
        );
        let tiny = CloneOverrides {
            memory_mib: Some(8),
            ..CloneOverrides::default()
        };
        assert_eq!(
            apply_overrides(&template(), &tiny),
            Err(TemplateError::Invalid(ValidationError::TooLittleMemory))
        );
        let zero = CloneOverrides {
            vcpus: Some(0),
            ..CloneOverrides::default()
        };
        assert_eq!(
            apply_overrides(&template(), &zero),
            Err(TemplateError::Invalid(ValidationError::ZeroCores))
        );
        // cloud-init 네트워크는 NIC 수를 넘을 수 없다.
        let nets = CloneOverrides {
            cloud_init: Some(CloudInit {
                networks: vec![CloudInitNet::default(); 3],
                ..CloudInit::default()
            }),
            ..CloneOverrides::default()
        };
        assert!(matches!(
            apply_overrides(&template(), &nets),
            Err(TemplateError::Invalid(ValidationError::CloudInit(_)))
        ));
    }

    #[test]
    fn disks_only_grow() {
        let ov = CloneOverrides {
            disks: vec![None, Some(20)],
            ..CloneOverrides::default()
        };
        assert_eq!(grown_disks(&ov, &[10, 10]).unwrap(), [None, Some(20)]);
        assert_eq!(grown_disks(&ov, &[10, 20]).unwrap(), [None, None]);
        assert_eq!(
            grown_disks(&ov, &[10, 30]),
            Err(TemplateError::DiskShrink {
                disk: 1,
                size: 20,
                current: 30
            })
        );
        assert_eq!(
            grown_disks(&ov, &[10]),
            Err(TemplateError::TooManyDisks { given: 2, has: 1 })
        );
        assert_eq!(
            TemplateError::DiskShrink {
                disk: 1,
                size: 20,
                current: 30
            }
            .to_string(),
            "disk1: 20 bytes is smaller than the current 30 bytes; disks can only grow"
        );
    }

    #[test]
    fn draft_takes_defaults_and_makes_a_stopped_template() {
        let draft: TemplateDraft = serde_json::from_value(serde_json::json!({
            "name": "ubuntu-tmpl",
            "network": "lan"
        }))
        .unwrap();
        assert_eq!(draft.vcpus, DEFAULT_TEMPLATE_VCPUS);
        assert_eq!(draft.memory_mib, DEFAULT_TEMPLATE_MEMORY_MIB);
        assert_eq!(draft.size_bytes, None);
        let back: TemplateDraft =
            serde_json::from_value(serde_json::to_value(&draft).unwrap()).unwrap();
        assert_eq!(back, draft);
        assert!(
            serde_json::from_value::<TemplateDraft>(serde_json::json!({
                "name": "t", "network": "lan", "cpus": 2
            }))
            .is_err()
        );

        let spec = TemplateDraft {
            machine: Some(MachineType::Virt),
            firmware: Some(Firmware::Uefi),
            ..draft.clone()
        }
        .vm_spec("ubuntu-24-04-arm64")
        .unwrap();
        assert!(spec.template);
        assert_eq!(spec.power, PowerState::Stopped);
        assert_eq!(spec.machine, MachineType::Virt);
        assert_eq!(spec.firmware, Firmware::Uefi);
        assert_eq!(spec.cpu.vcpus(), 2);
        assert_eq!(spec.memory_mib, 2048);
        assert_eq!(
            spec.disks,
            [DiskSpec {
                volume: "ubuntu-24-04-arm64".into(),
                bus: DiskBus::VirtioBlk,
                boot_order: Some(1),
            }]
        );
        assert_eq!(spec.nics.len(), 1);
        assert_eq!(spec.nics[0].network, "lan");
        assert_eq!(spec.cloud_init, Some(CloudInit::default()));

        let bios = TemplateDraft {
            firmware: Some(Firmware::Bios),
            machine: Some(MachineType::Q35),
            ..draft.clone()
        }
        .vm_spec("alpine-3-20")
        .unwrap();
        assert_eq!(
            (bios.machine, bios.firmware),
            (MachineType::Q35, Firmware::Bios)
        );

        let tiny = TemplateDraft {
            memory_mib: 1,
            ..draft
        };
        assert_eq!(
            tiny.vm_spec("v"),
            Err(TemplateError::Invalid(ValidationError::TooLittleMemory))
        );
    }
}
