use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cloud_init::CloudInitSeed;
use crate::cpu::ResolvedCpu;
use crate::numa::{CpuSet, HostPlacement, ResolvedNuma};
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
    /// 게스트 에이전트(QGA) 채널을 단다(`guest_agent`나 `ha.guest_watch`가 있는 VM, [`VmSpec::agent_channel`]).
    /// 장치가 바뀌므로 받는 쪽도 같은 값이어야 마이그레이션된다.
    ///
    /// [`VmSpec::agent_channel`]: crate::vm::VmSpec::agent_channel
    #[serde(default)]
    pub guest_agent: bool,
    /// 핫플러그 배치(계획 54). 없으면 옛 명령줄 그대로(vCPU 자리·virtio-mem·루트 포트 없음). 받는 쪽도 같은 값이어야
    /// 마이그레이션된다.
    #[serde(default)]
    pub hotplug: Option<HotplugLayout>,
    /// CD-ROM 드라이브(계획 57, 순번 = QEMU의 `cd{i}-dev`). 없으면(옛 정의) 드라이브가 없다.
    #[serde(default)]
    pub cdroms: Vec<RuntimeCdrom>,
    /// cloud-init NoCloud 씨앗(계획 57). 있으면 드라이버가 실행 디렉터리에 `cidata.iso`로 써서 읽기 전용 드라이브로
    /// 넣는다. 없으면(옛 정의) 씨앗 드라이브가 없다.
    #[serde(default)]
    pub cloud_init: Option<CloudInitSeed>,
    /// TPM 2.0(계획 59): 드라이버가 swtpm을 먼저 띄우고 명령줄 끝에 TPM 장치를 단다. 없으면(옛 정의) TPM이 없고
    /// 명령줄은 계획 58과 같다.
    #[serde(default)]
    pub tpm: Option<RuntimeTpm>,
    /// Secure Boot 펌웨어(계획 59, UEFI만): secure-boot 펌웨어와 MS 키 vars 템플릿, x86_64는 SMM과 secure pflash.
    #[serde(default)]
    pub secure_boot: bool,
    /// 시점을 찍는 동작(디스크 스냅샷·백업·복제 회차)에서 게스트 파일 시스템을 얼린다(계획 59, `guest_agent.freeze_fs`).
    #[serde(default)]
    pub freeze_fs: bool,
    /// VM의 펌웨어 초기화 회차(`VmStatus.firmware_epoch`, 리뷰 1 I1). TPM이 없는 UEFI VM도 노드의 `firmware.epoch`와
    /// 다르면 그 노드의 UEFI 변수를 지우고 템플릿에서 새로 만든다. TPM VM은 `RuntimeTpm.epoch`와 같다.
    #[serde(default)]
    pub firmware_epoch: u32,
    /// 이 노드의 회차가 `firmware_epoch`와 달라 펌웨어 파일을 지울 때 TPM 상태는 남긴다(리뷰 2): 이 노드가 `tpm_node`이거나
    /// 이 노드의 파일 해시가 VM의 `tpm_sha256`과 같다(지금 것이다). TPM이 없는 UEFI VM도 같다(TPM을 끈 동안 유일한
    /// 지금 상태일 수 있다).
    #[serde(default)]
    pub keep_tpm: bool,
    /// 켤 때 해석한 CPU(계획 63, `RunningHardware.cpu_resolved`). 명령줄의 `-cpu`가 이것이다. 없으면(계획 63 전 기록,
    /// 클러스터 업그레이드 중) `cpu.model`을 이 노드의 가속기로 펴되 기준·숨김 목록 없이 켠다(`host`·`max`는 지금과 같다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_resolved: Option<ResolvedCpu>,
    /// 켤 때 정한 게스트 NUMA 모양(계획 64, `RunningHardware.numa`). 있으면 명령줄에 노드마다 메모리 백엔드와 `-numa`를
    /// 둔다. 없으면 지금 명령줄 그대로다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numa: Option<ResolvedNuma>,
    /// 이 노드의 배치(계획 64, `RunningHardware.host_placement`): 메모리 백엔드의 `host-nodes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_placement: Option<HostPlacement>,
    /// 스레드 고정(계획 64). 있으면 드라이버가 스코프 `AllowedCPUs`·자식 친화·vCPU 스레드 친화를 건다. 없으면 걸지
    /// 않는다(`host_resources`가 없는 노드의 묶지 않는 VM — 지금 그대로).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinning: Option<RuntimePinning>,
}

/// QEMU 스레드를 둘 호스트 CPU(계획 64 §10). 작업자·받는 쪽이 이 노드의 세 집합과 배치로 셈해 드라이버에 준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimePinning {
    /// vCPU가 아닌 스레드(에뮬레이터·일꾼·vhost)와 swtpm의 CPU: 전용 VM의 `isolate` 에뮬레이터 pCPU, 아니면 공유 집합
    pub emulator: CpuSet,
    /// vCPU 자리(소켓 순, 핫플러그 상한까지)마다 CPU. 비면 vCPU도 `emulator`에서 돈다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcpus: Vec<CpuSet>,
    /// 소켓마다 vCPU 자리 수(QMP의 `socket-id`·`core-id`를 자리로 바꾼다)
    pub slots_per_socket: u32,
    /// 스코프의 `AllowedCPUs`(cgroup v2 cpuset이 있을 때만). 없으면 걸지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<CpuSet>,
}

impl RuntimePinning {
    /// vCPU 자리 `slot`의 CPU(자리별 고정이 없으면 에뮬레이터 집합)
    pub fn vcpu(&self, slot: usize) -> &CpuSet {
        self.vcpus.get(slot).unwrap_or(&self.emulator)
    }

    /// 이 VM의 스레드가 쓰는 모든 CPU
    pub fn all(&self) -> CpuSet {
        self.vcpus
            .iter()
            .fold(self.emulator.clone(), |acc, s| acc.union(s))
    }
}

/// 켤 때의 TPM 상태 규칙(계획 59)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeTpm {
    /// 이 노드에 TPM 상태 파일이 없으면 새로 만든다(`swtpm_setup`, EK와 인증서). 거짓인데 파일이 없으면 몰래 새로
    /// 만들지 않고 실패한다. 있는 파일은 늘 그대로 쓴다.
    pub fresh: bool,
    /// VM의 펌웨어 초기화 회차(`VmStatus.firmware_epoch`). 노드의 `firmware.epoch`(없으면 0)와 다르면 그 노드의
    /// UEFI 변수·TPM은 초기화 전의 것이라 지우고 새로 만든다.
    pub epoch: u32,
}

/// 이름이 풀린 CD-ROM 드라이브
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeCdrom {
    /// 넣은 매체. 없으면 빈 드라이브다.
    pub medium: Option<RuntimeMedium>,
    /// QEMU `bootindex`(실제 부팅 순서 `effective_boot`로 채운 값)
    pub boot_order: Option<u8>,
}

/// CD-ROM 매체(ISO 볼륨). 파일 풀에만 있으므로 경로뿐이고 늘 읽기 전용으로 연다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMedium {
    pub path: PathBuf,
}

/// 핫플러그 배치가 켤 때 두는 빈 PCIe 루트 포트 수
pub const HOTPLUG_PORTS: u8 = 8;

/// 핫플러그 배치(계획 54): 켤 때 둔 vCPU 자리·virtio-mem·빈 루트 포트와 지금 그 안에서 바뀐 것. 마이그레이션 대상은
/// 같은 배치로 켠다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HotplugLayout {
    /// 지금 켜진 vCPU 수(켤 때 수 이상, 상한 이하)
    pub vcpus: u16,
    /// 메모리 상한(MiB). 켤 때 `memory_mib`와 같으면 virtio-mem 장치가 없다.
    pub max_memory_mib: u32,
    /// virtio-mem에 꽂은 크기(`requested-size`, MiB)
    pub plugged_mib: u32,
    /// 빈 루트 포트 수(`hp0`부터)
    pub ports: u8,
    /// 켤 때 단 디스크 수. 이 뒤의 디스크는 실행 중 붙인 것이다.
    pub boot_disks: u8,
    /// 켤 때 단 NIC 수. 이 뒤의 NIC는 실행 중 붙인 것이다.
    pub boot_nics: u8,
    /// 실행 중 붙인 디스크(순번 `boot_disks`부터)마다 그 루트 포트
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub disk_ports: Vec<u8>,
    /// 실행 중 붙인 NIC(순번 `boot_nics`부터)마다 그 루트 포트
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nic_ports: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDisk {
    /// 파일·블록 디스크의 경로. RBD 디스크는 비어 있고 `rbd`를 쓴다.
    pub path: PathBuf,
    /// 디스크 소스가 파일인지, 블록 장치인지, RBD 이미지인지
    #[serde(default)]
    pub kind: DiskKind,
    /// RBD 디스크(`kind`가 `rbd`)의 이미지
    #[serde(default)]
    pub rbd: Option<RbdTarget>,
    pub format: ImageFormat,
    pub bus: DiskBus,
    pub boot_order: Option<u8>,
}

/// 디스크 소스의 종류. QEMU에 `driver=file`, `driver=host_device` 또는 `driver=rbd`로 넘긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DiskKind {
    /// 이미지 파일(dir, nfs 풀)
    #[default]
    File,
    /// 블록 장치(LVM LV 등)
    Block,
    /// Ceph RBD 이미지(rbd 풀). QEMU가 네트워크로 직접 연다.
    Rbd,
}

/// QEMU가 여는 RBD 이미지. 값은 풀을 만들 때 검증된 Ceph 이름과 conf 경로다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RbdTarget {
    pub pool: String,
    pub namespace: Option<String>,
    /// 이미지 이름(`opl-{id}`)
    pub image: String,
    /// Ceph 클라이언트 id(`client.` 없이)
    pub user: String,
    /// ceph.conf 경로. 없으면 Ceph 기본.
    pub conf: Option<String>,
}

/// 라이브 복원(계획 68 §11.2) 중인 디스크 하나의 배선: 게스트는 오버레이(`disk{i}-lr`, 게스트가 쓴 블록만)를 보고, 오버레이의
/// backing(`disk{i}-lrsrc`)은 백업이다. 대상 볼륨(`disk{i}`)은 장치에 붙지 않고 mirror가 채운 뒤 갈아탄다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRestoreDisk {
    pub source: LiveRestoreSource,
    /// 오버레이 qcow2(헤더에 backing 없음 — backing은 명령줄에서만)
    pub overlay: PathBuf,
}

/// 라이브 복원이 읽는 백업
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveRestoreSource {
    /// 저장소 백업: `backup-serve` 헬퍼의 유닉스 소켓 NBD 내보내기(헬퍼가 죽으면 QEMU가 다시 붙는다)
    Nbd { socket: PathBuf, export: String },
    /// 옛 풀 백업(§11.5): qcow2 파일 체인(자식부터, 마지막이 전체 백업). backing은 헤더를 따르지 않고 명시한다.
    Qcow2 { chain: Vec<PathBuf> },
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
        /// VXLAN·SDN 네트워크의 MTU. virtio NIC에 `host_mtu`로 알린다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mtu: Option<u16>,
        /// SDN(OVN) 네트워크의 논리 포트 이름(계획 65, [`crate::sdn::port_name`]). 있으면 `bridge`는 OVS 통합 브리지
        /// `br-int`이고, 드라이버가 tap을 OVS 포트로 붙여 `external_ids:iface-id`를 이 이름으로 둔다. 없으면 Linux 브리지 tap
        /// 그대로다(옛 정의는 바이트까지 같다).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ovn_port: Option<String>,
        /// 단순 NIC의 속도 제한(계획 67 §8.2 — 드라이버가 tap의 tc로 건다). SDN NIC는 늘 None이다(OVN `QoS` 행이 한다).
        /// 없으면 옛 정의와 바이트까지 같다.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        qos: Option<crate::qos::NicQos>,
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

    #[test]
    fn rbd_disk_round_trips() {
        // rbd가 없는 옛 정의는 RBD 이미지가 없다.
        let disk: RuntimeDisk = serde_json::from_value(serde_json::json!({
            "path": "/dev/vg0/opl-a", "kind": "block", "format": "raw", "bus": "virtio-blk",
            "boot_order": null
        }))
        .unwrap();
        assert_eq!(disk.rbd, None);
        let rbd = RuntimeDisk {
            path: PathBuf::new(),
            kind: DiskKind::Rbd,
            rbd: Some(RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: "opl-a".into(),
                user: "admin".into(),
                conf: None,
            }),
            ..disk
        };
        let json = serde_json::to_value(&rbd).unwrap();
        assert_eq!(json["kind"], "rbd");
        assert_eq!(json["rbd"]["image"], "opl-a");
        assert_eq!(serde_json::from_value::<RuntimeDisk>(json).unwrap(), rbd);
    }

    #[test]
    fn hotplug_layout_defaults_to_none() {
        // hotplug이 없는 옛 정의(계획 54 전 opld가 남긴 것)는 배치가 없다.
        let old = serde_json::json!({
            "id": uuid::Uuid::nil(), "name": "a", "cpu": { "cores": 1 }, "memory_mib": 256,
            "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
        });
        let spec: VmRuntimeSpec = serde_json::from_value(old).unwrap();
        assert_eq!(spec.hotplug, None);
        assert!(spec.cdroms.is_empty());
        let layout = HotplugLayout {
            vcpus: 2,
            max_memory_mib: 4096,
            plugged_mib: 0,
            ports: HOTPLUG_PORTS,
            boot_disks: 1,
            boot_nics: 0,
            disk_ports: vec![],
            nic_ports: vec![],
        };
        let spec = VmRuntimeSpec {
            hotplug: Some(layout.clone()),
            ..spec
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["hotplug"]["ports"], 8);
        assert_eq!(
            serde_json::from_value::<VmRuntimeSpec>(json)
                .unwrap()
                .hotplug,
            Some(layout)
        );
    }

    #[test]
    fn tap_mtu_is_optional() {
        // mtu가 없는 옛 정의(브리지 네트워크)는 None이고, 다시 쓰면 그대로다.
        let old = serde_json::json!({
            "kind": "tap", "ifname": "op0123456789n0", "bridge": "vmbr0", "vlan": null
        });
        let tap: NicBackend = serde_json::from_value(old.clone()).unwrap();
        assert!(matches!(tap, NicBackend::Tap { mtu: None, .. }));
        assert_eq!(serde_json::to_value(&tap).unwrap(), old);
        let vxlan = NicBackend::Tap {
            ifname: "op0123456789n0".into(),
            bridge: "oplv1000".into(),
            vlan: None,
            mtu: Some(1450),
            ovn_port: None,
            qos: None,
        };
        let json = serde_json::to_value(&vxlan).unwrap();
        assert_eq!(json["mtu"], 1450);
        assert_eq!(serde_json::from_value::<NicBackend>(json).unwrap(), vxlan);
    }

    #[test]
    fn runtime_nic_ovn_port_is_optional() {
        // 계획 65 전의 정의(브리지·VXLAN tap, user)는 ovn_port 없이 읽히고 다시 쓰면 바이트까지 같다.
        for old in [
            r#"{"mac":"52:54:00:12:34:56","model":"virtio","backend":{"kind":"tap","ifname":"op0123456789n0","bridge":"vmbr0","vlan":10}}"#,
            r#"{"mac":"52:54:00:12:34:56","model":"virtio","backend":{"kind":"tap","ifname":"op0123456789n0","bridge":"oplv1000","vlan":null,"mtu":1450}}"#,
            r#"{"mac":"52:54:00:12:34:56","model":"e1000e","backend":{"kind":"user"}}"#,
        ] {
            let nic: RuntimeNic = serde_json::from_str(old).unwrap();
            assert!(
                !matches!(
                    nic.backend,
                    NicBackend::Tap {
                        ovn_port: Some(_),
                        ..
                    }
                ),
                "{old}"
            );
            assert_eq!(serde_json::to_string(&nic).unwrap(), old);
        }
        let sdn = RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: "op0123456789n0".into(),
                bridge: "br-int".into(),
                vlan: None,
                mtu: Some(1442),
                ovn_port: Some("opl-x-525400123456".into()),
                qos: None,
            },
        };
        let json = serde_json::to_value(&sdn).unwrap();
        assert_eq!(json["backend"]["ovn_port"], "opl-x-525400123456");
        assert_eq!(serde_json::from_value::<RuntimeNic>(json).unwrap(), sdn);
    }

    #[test]
    fn cdroms_and_seed_round_trip() {
        let old = serde_json::json!({
            "id": uuid::Uuid::nil(), "name": "a", "cpu": { "cores": 1 }, "memory_mib": 256,
            "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
        });
        let spec: VmRuntimeSpec = serde_json::from_value(old).unwrap();
        assert_eq!(spec.cloud_init, None);
        let spec = VmRuntimeSpec {
            cloud_init: Some(CloudInitSeed {
                instance_id: "iid-1".into(),
                meta_data: "{}".into(),
                user_data: "#cloud-config\n{}".into(),
                vendor_data: None,
                network_config: Some("{}".into()),
            }),
            cdroms: vec![
                RuntimeCdrom {
                    medium: Some(RuntimeMedium {
                        path: "/pools/iso/a.raw".into(),
                    }),
                    boot_order: Some(2),
                },
                RuntimeCdrom {
                    medium: None,
                    boot_order: None,
                },
            ],
            ..spec
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["cdroms"][0]["medium"]["path"], "/pools/iso/a.raw");
        assert_eq!(json["cdroms"][1]["medium"], serde_json::Value::Null);
        assert_eq!(json["cloud_init"]["instance_id"], "iid-1");
        assert_eq!(serde_json::from_value::<VmRuntimeSpec>(json).unwrap(), spec);
    }
}
