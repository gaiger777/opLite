//! VM 내보내기(계획 72 §12): 꺼진 Orbivisor VM을 vSphere·Workstation·VirtualBox가 받는 OVF 패키지(서술자 +
//! streamOptimized VMDK + SHA-256 매니페스트)로 파일 풀의 `orbivisor-exports/{내보내기 id}/`에 쓴다. 내려받기는 그 파일들을
//! 하나의 OVA(ustar)로 흘려 보낸다. Task `vm-export`가 쓰고 `vm-export-delete`가 디렉터리를 지운다.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec, validate_name};

pub type VmExport = Resource<VmExportSpec, VmExportStatus>;

/// 풀 안에서 내보내기 디렉터리들을 두는 디렉터리
pub const EXPORT_DIR: &str = "orbivisor-exports";
/// 기본 VMware 하드웨어 버전(`vmx-14`, ESXi 6.7 이상)
pub const DEFAULT_HW_VERSION: u8 = 14;
/// 받는 하드웨어 버전 범위(`vmx-7`은 CBT 최소, `vmx-21`은 vSphere 8.0U2)
pub const HW_VERSIONS: std::ops::RangeInclusive<u8> = 7..=21;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VmExportSpec {
    /// 내보낸 VM
    pub vm: Uuid,
    /// 파일을 둘 풀(dir·NFS)
    pub pool: String,
    /// OVF `vmw:osType`(VMware guestId). 없으면 가져온 VM의 원본 값, 그것도 없으면 `otherGuest64`다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub guest_os: Option<String>,
    /// VMware 하드웨어 버전(`vmx-N`, 7~21)
    #[serde(default = "default_hw_version")]
    #[cfg_attr(feature = "openapi", schema(default = 14))]
    pub hw_version: u8,
    /// NIC의 MAC 주소를 그대로 적는다(`addressType=manual`). 끄면 받는 쪽이 새로 정한다.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub keep_macs: bool,
    /// 파일을 쓴 노드
    pub node: Uuid,
}

impl ResourceSpec for VmExportSpec {
    const KIND: Kind = Kind::VmExport;
    type Status = VmExportStatus;
}

fn default_hw_version() -> u8 {
    DEFAULT_HW_VERSION
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VmExportError {
    #[error("pool {0:?} is not a valid storage pool name")]
    Pool(String),
    #[error(
        "guest_os must be 1-64 letters and digits (a VMware guestId such as \"ubuntu64Guest\")"
    )]
    GuestOs,
    #[error("hw_version must be between 7 and 21")]
    HwVersion,
}

impl VmExportSpec {
    pub fn validate(&self) -> Result<(), VmExportError> {
        validate_name(&self.pool).map_err(|_| VmExportError::Pool(self.pool.clone()))?;
        if self.guest_os.as_deref().is_some_and(|g| {
            !(1..=64).contains(&g.len())
                || !g.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        }) {
            return Err(VmExportError::GuestOs);
        }
        if !HW_VERSIONS.contains(&self.hw_version) {
            return Err(VmExportError::HwVersion);
        }
        Ok(())
    }

    /// 풀 안의 이 내보내기 디렉터리(`orbivisor-exports/{id}`)
    pub fn dir(id: Uuid) -> String {
        format!("{EXPORT_DIR}/{id}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmExportStatus {
    pub phase: VmExportPhase,
    /// 쓴 파일(OVA 차례: `.ovf` → `.mf` → 디스크)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<VmExportFile>,
    /// 내려받을 OVA(ustar)의 정확한 크기
    #[serde(skip_serializing_if = "is_zero")]
    pub ova_bytes: u64,
    /// OVF에 넣지 못한 것(vTPM·CD·cloud-init 등)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmExportPhase {
    /// Task를 기다린다.
    #[default]
    Pending,
    /// 디스크를 VMDK로 바꾸고 해시를 잰다.
    Writing,
    /// 내려받을 수 있다.
    Ready,
    Failed,
}

/// 내보낸 파일 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmExportFile {
    /// 디렉터리 안 파일 이름(`web.ovf`, `web.mf`, `web-disk0.vmdk`)
    pub name: String,
    pub size: u64,
    /// SHA-256(소문자 16진 64자)
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spec_defaults_and_round_trip() {
        let vm = Uuid::now_v7();
        let node = Uuid::now_v7();
        let spec: VmExportSpec =
            serde_json::from_value(json!({ "vm": vm, "pool": "nfs1", "node": node })).unwrap();
        assert_eq!(spec.hw_version, 14);
        assert!(!spec.keep_macs);
        assert_eq!(spec.validate(), Ok(()));
        let v = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            v,
            json!({ "vm": vm, "pool": "nfs1", "hw_version": 14, "node": node })
        );
        assert_eq!(serde_json::from_value::<VmExportSpec>(v).unwrap(), spec);
        let full = VmExportSpec {
            guest_os: Some("ubuntu64Guest".into()),
            hw_version: 19,
            keep_macs: true,
            ..spec.clone()
        };
        let v = serde_json::to_value(&full).unwrap();
        assert_eq!(v["guest_os"], "ubuntu64Guest");
        assert_eq!(v["keep_macs"], true);
        assert_eq!(serde_json::from_value::<VmExportSpec>(v).unwrap(), full);
        assert!(
            serde_json::from_value::<VmExportSpec>(
                json!({ "vm": vm, "pool": "nfs1", "node": node, "x": 1 })
            )
            .is_err()
        );
        assert_eq!(VmExportSpec::dir(vm), format!("orbivisor-exports/{vm}"));
    }

    #[test]
    fn spec_validates() {
        let base: VmExportSpec = serde_json::from_value(
            json!({ "vm": Uuid::nil(), "pool": "nfs1", "node": Uuid::nil() }),
        )
        .unwrap();
        let cases = [
            (
                VmExportSpec {
                    pool: "NFS".into(),
                    ..base.clone()
                },
                VmExportError::Pool("NFS".into()),
            ),
            (
                VmExportSpec {
                    guest_os: Some("a b".into()),
                    ..base.clone()
                },
                VmExportError::GuestOs,
            ),
            (
                VmExportSpec {
                    hw_version: 6,
                    ..base.clone()
                },
                VmExportError::HwVersion,
            ),
            (
                VmExportSpec {
                    hw_version: 22,
                    ..base.clone()
                },
                VmExportError::HwVersion,
            ),
        ];
        for (spec, want) in cases {
            assert_eq!(spec.validate(), Err(want));
        }
    }

    #[test]
    fn status_writes_only_what_it_has() {
        assert_eq!(
            serde_json::to_value(VmExportStatus::default()).unwrap(),
            json!({ "phase": "pending" })
        );
        let st = VmExportStatus {
            phase: VmExportPhase::Ready,
            files: vec![VmExportFile {
                name: "web.ovf".into(),
                size: 4096,
                sha256: "ab".repeat(32),
            }],
            ova_bytes: 6144,
            warnings: vec!["vTPM state is not exported".into()],
            error: None,
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["phase"], "ready");
        assert_eq!(v["ova_bytes"], 6144);
        assert_eq!(serde_json::from_value::<VmExportStatus>(v).unwrap(), st);
        assert_eq!(Kind::VmExport.as_str(), "vm-export");
    }
}
