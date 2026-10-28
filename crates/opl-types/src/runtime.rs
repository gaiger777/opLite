use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::vm::{CpuSpec, DiskBus, Firmware, MacAddr, MachineType, NicModel};

/// 하이퍼바이저 드라이버가 받는, 이름 참조가 모두 풀린 VM 정의.
/// Volume 이름은 파일 경로로, Network 이름은 백엔드로, MAC은 실제 값으로 바뀌어 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmRuntimeSpec {
    pub id: Uuid,
    pub name: String,
    pub cpu: CpuSpec,
    pub memory_mib: u32,
    pub firmware: Firmware,
    pub machine: MachineType,
    pub disks: Vec<RuntimeDisk>,
    pub nics: Vec<RuntimeNic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDisk {
    pub path: PathBuf,
    /// `path`가 파일인지 블록 장치인지
    #[serde(default)]
    pub kind: DiskKind,
    pub format: ImageFormat,
    pub bus: DiskBus,
    pub boot_order: Option<u8>,
}

/// 디스크 소스의 종류. QEMU에 `driver=file` 또는 `driver=host_device`로 넘긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DiskKind {
    /// 이미지 파일(dir, nfs 풀)
    #[default]
    File,
    /// 블록 장치(LVM LV 등)
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ImageFormat {
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeNic {
    pub mac: MacAddr,
    pub model: NicModel,
    pub backend: NicBackend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum NicBackend {
    /// QEMU user-mode NAT
    User,
    /// 드라이버가 만들어 브리지에 붙이는 tap 장치
    Tap {
        ifname: String,
        bridge: String,
        vlan: Option<u16>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_kind_defaults_to_file() {
        // kind가 없는 옛 정의(이전 opld가 남긴 것)는 파일 디스크다.
        let disk: RuntimeDisk = serde_json::from_value(serde_json::json!({
            "path": "/pools/a.qcow2", "format": "qcow2", "bus": "virtio-blk", "boot_order": null
        }))
        .unwrap();
        assert_eq!(disk.kind, DiskKind::File);
        let block = RuntimeDisk {
            path: "/dev/vg0/opl-a".into(),
            kind: DiskKind::Block,
            ..disk
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["kind"], "block");
        assert_eq!(serde_json::from_value::<RuntimeDisk>(json).unwrap(), block);
    }
}
