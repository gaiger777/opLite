# opLite 계획 29: OVA 가져오기 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** VMware(vSphere·Workstation)가 내보낸 OVA를 올리면 opLite가 OVF를 읽어 VM 설정을 제안하고, 사용자가 확인하고 고친 값으로 디스크를 풀로 옮겨 멈춘 VM을 만든다. 하위 프로젝트 8(VMware 마이그레이션 마법사)의 첫 계획이다.

**Architecture:** `VmImport` 리소스가 올린 OVA(올린 노드의 `{data_dir}/uploads/imports/{id}/source.ova`), 해석한 원본 VM, 제안, 경고를 들고 있다. 올리기 요청 안에서 tar를 손으로 읽고 OVF를 `roxmltree`로 해석하며, 순수 함수 `propose`가 게스트 OS로 장치(Windows는 SATA·e1000e, 그 밖은 virtio)를 고른다. 실행(`POST /vm-imports/{key}/run`)은 원본 노드에서 도는 Task `VmImport`를 만들고, Task는 디스크마다 `.mf` 해시를 보며 추출해 기존 이미지 가져오기 길로 볼륨을 만든 뒤 복제·복원과 같은 방식으로 VM을 만든다. 실패하면 만든 것을 지우고 가져오기를 다시 실행할 수 있게 되돌린다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9, axum 0.8, utoipa, clap, 새 의존성 `roxmltree`(OVF XML)·`sha1`(`.mf`); 웹은 계획 8~28과 같다.

**설계 문서:** `docs/superpowers/specs/2026-11-04-vmware-import-design.md` (§2와 §4·§5의 계획 29 부분, "구현(계획 29)" 메모 포함)
**선행 계획:** 계획 1~28 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `VmImport` 리소스, OVA 올리기·해석(tar, OVF, `.mf`), 제안과 경고 | ESXi·vCenter에서 바로 가져오기 (계획 30) |
| 실행 API와 `VmImport` Task(추출·해시·볼륨·VM), 지우기와 강제 지우기 | 켜진 VMware VM 옮기기, 스냅샷 체인, OVF 압축·분할, 여러 VM OVA |
| 노드 시작 때 정리, 끊긴 실행 되돌리기 | VMware Tools 제거·virtio 드라이버 주입(게스트 안의 일) |
| oplctl `import`, 웹 가져오기 화면, 실제 시험, README | CD-ROM·플로피·USB·직렬·사운드 장치 |

## 이전 계획에서 이어받는 사항

- 이미지 올리기(`volumes.rs`의 받기·여유 검사·`check_import_source`·`import_volume`·`mark_ready`), 업로드 디렉터리와 그 정리.
- 새 VM을 만드는 Task(`tasks/clone.rs`의 `Made`·`volume_name`·`create_vm`·`discard`, 계획 27의 복원), 미리 정한 VM id를 대상으로 하는 Task, 백업 지우기의 `?force=true`.

## 실행 환경

계획 28과 같다. Task 시험(`opl-api` `storage-it` `tests/vm_import_tasks.rs`)과 실제 시험(`opld` `qemu-it` `tests/vm_import.rs`)은 이 Mac(qemu-img·QEMU)에서 돈다. 이 계획이 CI `check` 작업의 storage-it 단계에 `vm_import_tasks`를 더한다. 가져온 VM은 켜지 않는다(x86 게스트, 이 Mac은 aarch64).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `Cargo.lock`(`cargo build`), `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

## 설계 결정

- **올리기 → 제안 확인 → 실행:** 큰 OVA를 다시 올리지 않고 값만 바꿔 다시 실행할 수 있다. 실행이 실패하면 `ready` + error로 돌아간다.
- **지금 장치 안에서 매핑:** vmxnet3·pvscsi를 흉내 내지 않는다. Windows는 기본 드라이버가 있는 SATA·e1000e, Linux는 virtio.
- **믿지 않는 입력:** tar 항목 이름은 파일 경로로 쓰지 않고(추출 파일은 고정 이름), 크기·개수·이름 길이를 제한하며, OVF는 DTD를 받지 않는다. 추출한 VMDK는 다른 파일을 참조하지 않는지(`check_import_source`) 본 뒤 변환한다.
- **MAC은 유지:** 게스트 설정이 MAC에 묶인 경우가 많다. 원본을 끄라고 경고하고, `keep_macs: false`로 새 MAC을 받을 수 있다.
- **원본은 올린 노드에:** 프록시 없이 받은 노드가 원본을 들고 Task를 돌린다. 그 노드가 사라지면 `--force`로 기록만 지운다.

## 파일 구조

```
crates/opl-types/src/{vm_import.rs, task.rs, resource.rs, lib.rs}          # 타입, propose, TaskOp
crates/opl-api/src/{vm_imports.rs, vm_imports/{ova.rs, ovf.rs}, tasks/import.rs, tasks.rs, volumes.rs, ...}
crates/opl-api/tests/{ova.rs, vm_imports.rs, vm_import_tasks.rs, common/ustar.rs, fixtures/*.ovf}
crates/oplctl/src/{cli.rs, client.rs}, crates/opld/tests/vm_import.rs
web/src/imports/*, web/src/storage/upload.ts, web/src/{api, auth, layout, i18n, tasks}/...
.github/workflows/ci.yml, README.md, docs/superpowers/specs/{2026-11-04-vmware-import-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 줄 단위로 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 가져오기 타입과 제안

opl-types `vm_import.rs`: `VmImport { spec: { node, source: {"ova": { file_name, size_bytes }} }, status: { phase: ready|importing|done|failed, source_vm, proposal, warnings, vm, error } }`, 원본 VM(`SourceVm`: 이름·게스트 OS·펌웨어·vCPU·소켓당 코어·메모리·디스크·NIC), 제안(`ImportProposal`: 소켓·코어·메모리·펌웨어·디스크 버스·NIC 모델·원본 네트워크 → 같은 이름 opLite 네트워크), 실행 요청(`ImportRun`). `Kind::VmImport`(`vm-import`). 순수 함수 `propose`: Windows(`osType`이 `win`으로 시작)면 SATA·e1000e, 아니면 virtio-blk·virtio; 소켓당 코어로 나누어떨어지면 소켓×코어; 메모리 최소값; UEFI는 NVRAM을 옮기지 않는다는 경고; SATA 6포트를 넘으면 virtio-blk; MAC을 유지하니 원본을 끄라는 경고.

**Files:**
- Create: `crates/opl-types/src/vm_import.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-types/src/lib.rs`에서

```rust
pub mod vm;
```

을 다음으로 바꾼다:

```rust
pub mod vm;
pub mod vm_import;
```

`crates/opl-types/src/resource.rs`에서

```rust
    BackupJob,
}
```

을 다음으로 바꾼다:

```rust
    BackupJob,
    VmImport,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::BackupJob => "backup-job",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::BackupJob => "backup-job",
            Kind::VmImport => "vm-import",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::BackupJob,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::BackupJob,
            Kind::VmImport,
        ] {
```

`crates/opl-types/src/vm_import.rs`를 만든다:

```rust
//! VM 가져오기. 다른 하이퍼바이저(VMware)에서 내보낸 VM(OVA)을 올린 노드에 두고, 원본 VM에서 읽은
//! 값([`SourceVm`])으로 opLite VM 설정을 제안([`propose`])한 뒤, 사용자가 고친 값([`ImportRun`])으로
//! 디스크를 볼륨으로 옮기고 VM을 만든다.

use std::collections::BTreeMap;

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

/// 원본. JSON은 `{"ova": {...}}`다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ImportSource {
    /// 올린 OVA 파일
    Ova(OvaSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct OvaSource {
    /// 올린 파일 이름(표시용)
    pub file_name: String,
    pub size_bytes: u64,
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

/// opLite VM 설정 제안. 실행할 때 [`ImportRun`]에 없는 값의 기본값이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportProposal {
    pub sockets: u16,
    pub cores: u16,
    pub memory_mib: u32,
    pub firmware: Firmware,
    pub disk_bus: DiskBus,
    pub nic_model: NicModel,
    /// 원본 네트워크 → 이름이 같은 opLite 네트워크(없으면 null, 실행 때 정해야 한다)
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
    /// 원본 네트워크 → opLite 네트워크. 원본 NIC의 네트워크마다 있어야 한다.
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

/// 원본 VM으로 opLite VM 설정을 제안한다. `networks`는 지금 있는 opLite 네트워크 이름이다.
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
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1025개, 실패 0

Run: `cargo test -p opl-types vm_import`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opl-types): VM 가져오기 타입과 제안 — VmImport(ImportSource::Ova·상태·원본 VM·제안·실행 요청), Kind::VmImport, is_windows, propose(매핑 규칙과 경고)"
```

---

### Task 2: OVA·OVF 해석

opl-api `vm_imports/ova.rs`: tar(ustar)를 손으로 읽어 항목 표를 만든다(prefix, pax `path`·`size`, GNU 긴 이름, base-256 크기, 이름 4 KiB·항목 10 000개 제한, 이름 겹침·잘린 파일 거절). `.ovf` 하나(4 MiB 이하), `.mf`(SHA-1/256/384/512; 모르는 알고리즘·목록에 없는 디스크는 경고), `.cert`는 경고. 디스크 추출은 해시를 보며 하고 다르면 지운다. `vm_imports/ovf.rs`: `roxmltree`(DTD 거절)로 VirtualSystem 하나, CPU·메모리(AllocationUnits)·`vmw:Config firmware`·컨트롤러(종류, 같은 종류는 Address 순)와 디스크(AddressOnParent 순, capacity 단위)·NIC를 읽고, 가져오지 않는 장치는 경고. compression·chunkSize·collection 거절, 디스크 32개·NIC 16개 제한. 의존성 `roxmltree`·`sha1`. 시험용 ustar 작성기와 OVF 견본.

**Files:**
- Create: `crates/opl-api/src/vm_imports.rs`
- Create: `crates/opl-api/src/vm_imports/ova.rs`
- Create: `crates/opl-api/src/vm_imports/ovf.rs`
- Create: `crates/opl-api/tests/common/ustar.rs`
- Create: `crates/opl-api/tests/fixtures/vsphere.ovf`
- Create: `crates/opl-api/tests/fixtures/workstation.ovf`
- Create: `crates/opl-api/tests/ova.rs`
- Modify: `Cargo.toml`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`Cargo.toml`에서

```toml
sha2 = "0.11.1"
getrandom = "0.4.3"
```

을 다음으로 바꾼다:

```toml
sha2 = "0.11.1"
sha1 = "0.11.0"
roxmltree = "0.21.2"
getrandom = "0.4.3"
```

`crates/opl-api/Cargo.toml`에서

```toml
opl-types = { workspace = true, features = ["openapi"] }
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
```

을 다음으로 바꾼다:

```toml
opl-types = { workspace = true, features = ["openapi"] }
roxmltree.workspace = true
serde.workspace = true
serde_json.workspace = true
sha1.workspace = true
sha2.workspace = true
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod users;
mod vms;
```

을 다음으로 바꾼다:

```rust
pub mod users;
pub mod vm_imports;
mod vms;
```

`crates/opl-api/src/vm_imports.rs`를 만든다:

```rust
//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아낸다.

pub mod ova;
pub mod ovf;
```

`crates/opl-api/src/vm_imports/ova.rs`를 만든다:

```rust
//! OVA 읽기. OVA는 tar(ustar)다. 헤더만 훑어 항목 표([`OvaIndex`])를 만들고, OVF와 `.mf`만 읽는다.
//! 디스크는 실행 때 항목을 그대로 복사하면서 `.mf`의 해시를 확인한다([`extract`]).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use opl_types::vm_import::SourceVm;
use sha2::Digest as _;

use super::ovf::parse_ovf;

const BLOCK: u64 = 512;
/// `.ovf` 파일의 최대 크기
const MAX_OVF_BYTES: u64 = 4 << 20;
/// `.mf` 파일과 pax·GNU 긴 이름 헤더 본문의 최대 크기
const MAX_SMALL_BYTES: u64 = 1 << 20;
/// tar 항목 이름의 최대 길이(pax `path`·GNU 긴 이름 포함)
const MAX_NAME_BYTES: usize = 4 << 10;
/// tar 헤더(항목)의 최대 개수
const MAX_ENTRIES: usize = 10_000;
/// 해석 경고의 최대 개수(넘는 것은 개수만 알린다)
const MAX_WARNINGS: usize = 50;

/// OVA·OVF를 쓸 수 없는 이유. 메시지는 사용자에게 그대로 보인다.
#[derive(Debug, thiserror::Error)]
pub enum OvaError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl OvaError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

fn truncated() -> OvaError {
    OvaError::invalid("the OVA is truncated")
}

fn not_tar() -> OvaError {
    OvaError::invalid("the file is not an OVA (tar) archive")
}

/// tar 안의 파일 하나. `offset`은 본문이 시작하는 위치다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvaEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct OvaIndex {
    /// tar 안의 순서
    pub entries: Vec<OvaEntry>,
    /// 이름 → `entries`의 위치
    by_name: HashMap<String, usize>,
}

impl OvaIndex {
    pub fn get(&self, name: &str) -> Option<&OvaEntry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }
}

/// tar 헤더를 훑어 파일 항목 표를 만든다. 디렉터리와 pax 전역 헤더는 건너뛰고, pax 확장 헤더(`path`,
/// `size`)와 GNU 긴 이름은 다음 파일·디렉터리 항목에 쓴다(사이의 다른 pax·GNU 긴 이름 헤더에는 쓰지
/// 않는다). 끝은 0 블록 또는 파일 끝이다. 이름이 4 KiB를 넘거나 항목이 10 000개를 넘으면 오류다.
pub fn read_index(path: &Path) -> Result<OvaIndex, OvaError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut index = OvaIndex::default();
    let mut pax_name: Option<String> = None;
    let mut pax_size: Option<u64> = None;
    let mut long_name: Option<String> = None;
    let mut header = [0u8; BLOCK as usize];
    let mut pos = 0;
    let mut count = 0;
    while pos < len {
        if len - pos < BLOCK {
            return Err(truncated());
        }
        file.seek(SeekFrom::Start(pos))?;
        file.read_exact(&mut header)?;
        if header.iter().all(|&b| b == 0) {
            break;
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(OvaError::invalid(format!(
                "the OVA has more than {MAX_ENTRIES} entries"
            )));
        }
        check_checksum(&header)?;
        let typeflag = header[156];
        // pax·GNU 긴 이름 값은 그것이 가리키는 항목(파일·디렉터리)에만 쓴다.
        let entry = matches!(typeflag, b'0' | 0 | b'5');
        let header_size = parse_size(&header[124..136])?;
        let (name, size) = match entry {
            true => (
                pax_name
                    .take()
                    .or(long_name.take())
                    .unwrap_or_else(|| header_name(&header)),
                pax_size.take().unwrap_or(header_size),
            ),
            false => (header_name(&header), header_size),
        };
        if name.len() > MAX_NAME_BYTES {
            return Err(OvaError::invalid(
                "a file name in the OVA is longer than 4 KiB",
            ));
        }
        let data = pos + BLOCK;
        if size > len - data {
            return Err(truncated());
        }
        pos = data + size.next_multiple_of(BLOCK);
        match typeflag {
            b'0' | 0 => {
                let name = name.strip_prefix("./").unwrap_or(&name).to_string();
                if index.by_name.contains_key(&name) {
                    return Err(OvaError::invalid(format!(
                        "duplicate file {name:?} in the OVA"
                    )));
                }
                index.by_name.insert(name.clone(), index.entries.len());
                index.entries.push(OvaEntry {
                    name,
                    offset: data,
                    size,
                });
            }
            b'5' | b'g' => {}
            b'x' => {
                let records = read_header_data(&mut file, data, size)?;
                let (path, size) = parse_pax(&records)?;
                pax_name = path.or(pax_name);
                pax_size = size.or(pax_size);
            }
            b'L' => long_name = Some(c_str(&read_header_data(&mut file, data, size)?)),
            t => {
                return Err(OvaError::invalid(format!(
                    "unsupported tar entry type '{}' for {name:?}",
                    t as char
                )));
            }
        }
    }
    Ok(index)
}

/// 헤더 체크섬(체크섬 칸을 공백으로 두고 바이트를 더한 값)이 맞는지 본다.
fn check_checksum(header: &[u8]) -> Result<(), OvaError> {
    let stored = parse_octal(&header[148..156]).ok_or_else(not_tar)?;
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                u64::from(b)
            }
        })
        .sum();
    if stored != sum {
        return Err(not_tar());
    }
    Ok(())
}

/// 이름. ustar면 `prefix/name`이다(GNU tar는 prefix 자리를 다른 데 쓴다).
fn header_name(header: &[u8]) -> String {
    let name = c_str(&header[..100]);
    let prefix = c_str(&header[345..500]);
    if &header[257..263] == b"ustar\0" && !prefix.is_empty() {
        format!("{prefix}/{name}")
    } else {
        name
    }
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field).ok()?;
    let text = text.trim_matches(|c| c == '\0' || c == ' ');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

/// 크기 칸. 8진수, 또는 첫 바이트의 맨 위 비트가 켜져 있으면 base-256(큰 파일).
fn parse_size(field: &[u8]) -> Result<u64, OvaError> {
    let invalid = || OvaError::invalid("invalid tar header");
    if field[0] & 0x80 == 0 {
        return parse_octal(field).ok_or_else(invalid);
    }
    if field[0] != 0x80 {
        return Err(invalid());
    }
    field[1..].iter().try_fold(0u64, |acc, &b| {
        acc.checked_mul(256)
            .map(|v| v | u64::from(b))
            .ok_or_else(invalid)
    })
}

fn read_at(file: &mut File, offset: u64, size: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0; size as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// pax·GNU 긴 이름 헤더의 본문
fn read_header_data(file: &mut File, offset: u64, size: u64) -> Result<Vec<u8>, OvaError> {
    if size > MAX_SMALL_BYTES {
        return Err(OvaError::invalid("a tar header is too large"));
    }
    Ok(read_at(file, offset, size)?)
}

/// pax 레코드(`길이 키=값\n`)에서 `path`와 `size`를 읽는다.
fn parse_pax(mut data: &[u8]) -> Result<(Option<String>, Option<u64>), OvaError> {
    let invalid = || OvaError::invalid("invalid pax header");
    let (mut path, mut size) = (None, None);
    while !data.is_empty() {
        let space = data.iter().position(|&b| b == b' ').ok_or_else(invalid)?;
        let len: usize = std::str::from_utf8(&data[..space])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(invalid)?;
        if len <= space + 1 || len > data.len() || data[len - 1] != b'\n' {
            return Err(invalid());
        }
        let record = std::str::from_utf8(&data[space + 1..len - 1]).map_err(|_| invalid())?;
        let (key, value) = record.split_once('=').ok_or_else(invalid)?;
        match key {
            "path" => path = Some(value.to_string()),
            "size" => size = Some(value.parse().map_err(|_| invalid())?),
            _ => {}
        }
        data = &data[len..];
    }
    Ok((path, size))
}

/// `.mf`의 해시(16진수 소문자)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Digest {
    Sha1(String),
    Sha256(String),
    Sha384(String),
    Sha512(String),
}

impl Digest {
    fn hex(&self) -> &str {
        match self {
            Digest::Sha1(hex) | Digest::Sha256(hex) | Digest::Sha384(hex) | Digest::Sha512(hex) => {
                hex
            }
        }
    }
}

/// 해석한 `.mf`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// 파일 → 해시
    pub digests: BTreeMap<String, Digest>,
    /// 모르는 알고리즘으로 적힌 파일(확인하지 않는다)
    pub unsupported: BTreeSet<String>,
}

/// `.mf` 해석. 줄마다 `SHA256(file)= hex`처럼 `SHA1`·`SHA256`·`SHA384`·`SHA512`(공백 차이는 허용).
/// 모르는 알고리즘의 줄은 오류가 아니라 그 파일을 `unsupported`에 넣는다.
pub fn parse_manifest(text: &str) -> Result<Manifest, OvaError> {
    let mut manifest = Manifest::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let bad = || OvaError::invalid(format!("invalid manifest line {line:?}"));
        let (algorithm, rest) = line.split_once('(').ok_or_else(bad)?;
        let (file, hex) = rest.rsplit_once(')').ok_or_else(bad)?;
        let hex = hex
            .trim_start()
            .strip_prefix('=')
            .ok_or_else(bad)?
            .trim()
            .to_ascii_lowercase();
        let (digest, len): (fn(String) -> Digest, usize) =
            match algorithm.trim().to_ascii_uppercase().as_str() {
                "SHA1" => (Digest::Sha1, 40),
                "SHA256" => (Digest::Sha256, 64),
                "SHA384" => (Digest::Sha384, 96),
                "SHA512" => (Digest::Sha512, 128),
                _ => {
                    manifest.unsupported.insert(file.to_string());
                    continue;
                }
            };
        if hex.len() != len || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        manifest.digests.insert(file.to_string(), digest(hex));
    }
    Ok(manifest)
}

/// 해석한 OVA
#[derive(Debug, Clone)]
pub struct Ova {
    pub vm: SourceVm,
    /// 해석 경고(가져오지 않는 장치, 서명, 확인하지 못하는 디스크). 겹치지 않게 50개까지
    pub warnings: Vec<String>,
    pub index: OvaIndex,
    /// `.mf`의 파일 → 해시(`.mf`가 없으면 비어 있다)
    pub manifest: BTreeMap<String, Digest>,
}

/// OVA를 해석한다. `.ovf`는 정확히 하나(4 MiB 이하), `.mf`는 없거나 하나여야 하고, OVF가 가리키는
/// 파일이 모두 tar 안에 있어야 한다. `.mf`는 형식만 보고 해시는 [`extract`]가 확인한다.
pub fn parse_ova(path: &Path) -> Result<Ova, OvaError> {
    let index = read_index(path)?;
    let with_ext = |ext: &str| -> Vec<&OvaEntry> {
        index
            .entries
            .iter()
            .filter(|e| e.name.to_ascii_lowercase().ends_with(ext))
            .collect()
    };
    let ovf = match with_ext(".ovf")[..] {
        [ovf] => ovf,
        [] => return Err(OvaError::invalid("the OVA has no .ovf file")),
        _ => return Err(OvaError::invalid("the OVA has more than one .ovf file")),
    };
    if ovf.size > MAX_OVF_BYTES {
        return Err(OvaError::invalid("the .ovf file is larger than 4 MiB"));
    }
    let manifest = match with_ext(".mf")[..] {
        [] => None,
        [mf] if mf.size > MAX_SMALL_BYTES => {
            return Err(OvaError::invalid("the .mf file is larger than 1 MiB"));
        }
        [mf] => Some(parse_manifest(&read_text(path, mf)?)?),
        _ => return Err(OvaError::invalid("the OVA has more than one .mf file")),
    };
    let parsed = parse_ovf(&read_text(path, ovf)?)?;
    if let Some(file) = parsed.files.iter().find(|f| index.get(f).is_none()) {
        return Err(OvaError::invalid(format!(
            "file {file:?} is not in the OVA"
        )));
    }
    let mut warnings = parsed.warnings;
    if !with_ext(".cert").is_empty() {
        warnings.push("the OVA signature (.cert) is not verified".into());
    }
    // `.mf`가 있으면 확인하지 못하는 디스크를 알린다.
    for file in manifest.iter().flat_map(|mf| {
        parsed
            .vm
            .disks
            .iter()
            .map(|d| &d.file)
            .filter(|f| !mf.digests.contains_key(*f))
            .map(|f| (f, mf.unsupported.contains(f)))
    }) {
        warnings.push(match file {
            (f, true) => {
                format!("the manifest uses an unsupported digest for {f}; it is not verified")
            }
            (f, false) => format!("{f} is not listed in the manifest; it is not verified"),
        });
    }
    Ok(Ova {
        vm: parsed.vm,
        warnings: cap_warnings(warnings),
        index,
        manifest: manifest.map(|mf| mf.digests).unwrap_or_default(),
    })
}

/// 겹치는 경고를 빼고 [`MAX_WARNINGS`]개까지 남긴다. 넘는 것은 마지막에 개수만 알린다.
fn cap_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut unique: Vec<String> = warnings
        .into_iter()
        .filter(|w| seen.insert(w.clone()))
        .collect();
    if unique.len() > MAX_WARNINGS {
        let more = unique.len() - MAX_WARNINGS;
        unique.truncate(MAX_WARNINGS);
        unique.push(format!("… and {more} more"));
    }
    unique
}

fn read_text(path: &Path, entry: &OvaEntry) -> Result<String, OvaError> {
    let bytes = read_at(&mut File::open(path)?, entry.offset, entry.size)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| OvaError::invalid(format!("{} is not UTF-8 text", entry.name)))?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

/// 항목을 `dest`로 복사(fsync)하면서 해시를 확인한다. 실패하면 `dest`를 지운다.
pub fn extract(
    path: &Path,
    entry: &OvaEntry,
    dest: &Path,
    digest: Option<&Digest>,
) -> Result<(), OvaError> {
    let result = copy_entry(path, entry, dest, digest);
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

fn copy_entry(
    path: &Path,
    entry: &OvaEntry,
    dest: &Path,
    digest: Option<&Digest>,
) -> Result<(), OvaError> {
    let mut src = File::open(path)?;
    src.seek(SeekFrom::Start(entry.offset))?;
    let mut src = src.take(entry.size);
    let mut out = File::create(dest)?;
    let mut hasher = match digest {
        None => Hasher::None,
        Some(Digest::Sha1(_)) => Hasher::Sha1(sha1::Sha1::new()),
        Some(Digest::Sha256(_)) => Hasher::Sha256(sha2::Sha256::new()),
        Some(Digest::Sha384(_)) => Hasher::Sha384(sha2::Sha384::new()),
        Some(Digest::Sha512(_)) => Hasher::Sha512(sha2::Sha512::new()),
    };
    let mut buf = vec![0; 1 << 20];
    let mut copied = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        copied += n as u64;
    }
    if copied != entry.size {
        return Err(truncated());
    }
    out.sync_all()?;
    if let Some(expected) = digest
        && hasher.hex() != expected.hex()
    {
        return Err(OvaError::invalid(format!(
            "checksum mismatch for {}",
            entry.name
        )));
    }
    Ok(())
}

enum Hasher {
    None,
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
}

impl Hasher {
    fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::None => {}
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha384(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    fn hex(self) -> String {
        match self {
            Hasher::None => String::new(),
            Hasher::Sha1(h) => hex::encode(h.finalize()),
            Hasher::Sha256(h) => hex::encode(h.finalize()),
            Hasher::Sha384(h) => hex::encode(h.finalize()),
            Hasher::Sha512(h) => hex::encode(h.finalize()),
        }
    }
}
```

`crates/opl-api/src/vm_imports/ovf.rs`를 만든다:

```rust
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
            // 화면은 opLite가 따로 붙이고, VMCI는 VMware 전용이다
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
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
#![allow(dead_code)]
```

을 다음으로 바꾼다:

```rust
#![allow(dead_code)]

pub mod ustar;
```

`crates/opl-api/tests/common/ustar.rs`를 만든다:

````rust
//! 시험용 작은 ustar 작성기. OVA(tar)를 만든다. 표준 라이브러리만 쓰므로 다른 크레이트의 시험도
//! `#[path = ".../opl-api/tests/common/ustar.rs"] mod ustar;`로 가져다 쓸 수 있다.
//!
//! ```ignore
//! let ova = Tar::new().file("vm.ovf", ovf.as_bytes()).file("vm-disk1.vmdk", &disk).finish();
//! ```

/// 헤더 하나를 마음대로 정할 때 쓴다(잘못된 tar를 만드는 시험).
pub struct Header<'a> {
    pub name: &'a str,
    pub prefix: &'a str,
    pub typeflag: u8,
    /// 헤더의 크기 칸에 쓸 값(본문 길이와 달라도 된다)
    pub size: u64,
    /// 크기를 8진수 대신 base-256으로 쓴다
    pub base256: bool,
}

#[derive(Default)]
pub struct Tar(Vec<u8>);

impl Tar {
    pub fn new() -> Self {
        Self::default()
    }

    /// 보통 파일. 이름이 100바이트를 넘으면 `/`에서 나누어 앞부분을 prefix에 쓴다.
    pub fn file(self, name: &str, data: &[u8]) -> Self {
        let (prefix, name) = match name.len() > 100 {
            true => name.rsplit_once('/').expect("long name needs a '/'"),
            false => ("", name),
        };
        self.raw(
            Header {
                name,
                prefix,
                typeflag: b'0',
                size: data.len() as u64,
                base256: false,
            },
            data,
        )
    }

    pub fn dir(self, name: &str) -> Self {
        self.raw(
            Header {
                name,
                prefix: "",
                typeflag: b'5',
                size: 0,
                base256: false,
            },
            &[],
        )
    }

    /// pax 확장 헤더(`x`). 다음 항목의 값을 덮는다.
    pub fn pax(self, records: &[(&str, &str)]) -> Self {
        let mut data = Vec::new();
        for (key, value) in records {
            // 길이는 자기 자신의 자릿수를 포함한다
            let body = format!(" {key}={value}\n");
            let mut len = body.len() + 1;
            while (len.to_string().len() + body.len()) != len {
                len += 1;
            }
            data.extend_from_slice(format!("{len}{body}").as_bytes());
        }
        self.special(b'x', &data)
    }

    /// GNU 긴 이름(`L`). 다음 항목의 이름이다.
    pub fn gnu_long_name(self, name: &str) -> Self {
        let mut data = name.as_bytes().to_vec();
        data.push(0);
        self.special(b'L', &data)
    }

    fn special(self, typeflag: u8, data: &[u8]) -> Self {
        self.raw(
            Header {
                name: "././@LongLink",
                prefix: "",
                typeflag,
                size: data.len() as u64,
                base256: false,
            },
            data,
        )
    }

    /// 헤더와 본문(512바이트로 채움)을 쓴다.
    pub fn raw(mut self, h: Header<'_>, data: &[u8]) -> Self {
        let mut block = [0u8; 512];
        block[..h.name.len()].copy_from_slice(h.name.as_bytes());
        block[100..108].copy_from_slice(b"0000644\0");
        block[108..116].copy_from_slice(b"0000000\0");
        block[116..124].copy_from_slice(b"0000000\0");
        if h.base256 {
            block[124] = 0x80;
            block[128..136].copy_from_slice(&h.size.to_be_bytes());
        } else {
            block[124..136].copy_from_slice(format!("{:011o}\0", h.size).as_bytes());
        }
        block[136..148].copy_from_slice(b"00000000000\0");
        block[156] = h.typeflag;
        block[257..263].copy_from_slice(b"ustar\0");
        block[263..265].copy_from_slice(b"00");
        block[345..345 + h.prefix.len()].copy_from_slice(h.prefix.as_bytes());
        // 체크섬은 체크섬 칸을 공백으로 두고 더한 값이다
        block[148..156].copy_from_slice(b"        ");
        let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
        block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        self.0.extend_from_slice(&block);
        self.0.extend_from_slice(data);
        self.0.resize(self.0.len().next_multiple_of(512), 0);
        self
    }

    /// 끝 표시(0 블록 둘)를 붙인다.
    pub fn finish(mut self) -> Vec<u8> {
        self.0.extend_from_slice(&[0u8; 1024]);
        self.0
    }

    /// 끝 표시 없이(파일 끝에서 끝나는 tar)
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
````

`crates/opl-api/tests/fixtures/vsphere.ovf`를 만든다:

```
<?xml version="1.0" encoding="UTF-8"?>
<!--Generated by VMware ovftool 4.6.0 (build-21452615), UTC time: 2026-10-30T02:11:45.117Z-->
<Envelope vmw:buildId="build-21452615" xmlns="http://schemas.dmtf.org/ovf/envelope/2" xmlns:cim="http://schemas.dmtf.org/wbem/wscim/1/common" xmlns:epasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_EthernetPortAllocationSettingData" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/2" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData" xmlns:sasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_StorageAllocationSettingData" xmlns:vmw="http://www.vmware.com/schema/ovf" xmlns:vssd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_VirtualSystemSettingData" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <References>
    <File ovf:href="web01-disk1.vmdk" ovf:id="file1" ovf:size="1572864000"/>
    <File ovf:href="web01-disk2.vmdk" ovf:id="file2" ovf:size="68608"/>
    <File ovf:href="web01-disk3.vmdk" ovf:id="file3" ovf:size="68608"/>
    <File ovf:href="web01-file1.nvram" ovf:id="file4" ovf:size="270840"/>
  </References>
  <DiskSection>
    <Info>Virtual disk information</Info>
    <Disk ovf:capacity="40" ovf:capacityAllocationUnits="byte * 2^30" ovf:diskId="vmdisk1" ovf:fileRef="file1" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized" ovf:populatedSize="3221225472"/>
    <Disk ovf:capacity="10737418240" ovf:diskId="vmdisk2" ovf:fileRef="file2" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized" ovf:populatedSize="0"/>
    <Disk ovf:capacity="2048" ovf:capacityAllocationUnits="byte * 2^20" ovf:diskId="vmdisk3" ovf:fileRef="file3" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized" ovf:populatedSize="0"/>
  </DiskSection>
  <NetworkSection>
    <Info>The list of logical networks</Info>
    <Network ovf:name="VM Network">
      <Description>The VM Network network</Description>
    </Network>
    <Network ovf:name="Backup Net">
      <Description>The Backup Net network</Description>
    </Network>
  </NetworkSection>
  <VirtualSystem ovf:id="web01">
    <Info>A virtual machine</Info>
    <Name>web01</Name>
    <OperatingSystemSection ovf:id="94" vmw:osType="ubuntu64Guest">
      <Info>The kind of installed guest operating system</Info>
      <Description>Ubuntu Linux (64-bit)</Description>
    </OperatingSystemSection>
    <VirtualHardwareSection>
      <Info>Virtual hardware requirements</Info>
      <System>
        <vssd:ElementName>Virtual Hardware Family</vssd:ElementName>
        <vssd:InstanceID>0</vssd:InstanceID>
        <vssd:VirtualSystemIdentifier>web01</vssd:VirtualSystemIdentifier>
        <vssd:VirtualSystemType>vmx-19</vssd:VirtualSystemType>
      </System>
      <Item>
        <rasd:AllocationUnits>hertz * 10^6</rasd:AllocationUnits>
        <rasd:Description>Number of Virtual CPUs</rasd:Description>
        <rasd:ElementName>4 virtual CPU(s)</rasd:ElementName>
        <rasd:InstanceID>1</rasd:InstanceID>
        <rasd:ResourceType>3</rasd:ResourceType>
        <rasd:VirtualQuantity>4</rasd:VirtualQuantity>
        <vmw:CoresPerSocket ovf:required="false">2</vmw:CoresPerSocket>
      </Item>
      <Item>
        <rasd:AllocationUnits>byte * 2^30</rasd:AllocationUnits>
        <rasd:Description>Memory Size</rasd:Description>
        <rasd:ElementName>8GB of memory</rasd:ElementName>
        <rasd:InstanceID>2</rasd:InstanceID>
        <rasd:ResourceType>4</rasd:ResourceType>
        <rasd:VirtualQuantity>8</rasd:VirtualQuantity>
      </Item>
      <Item>
        <rasd:Address>0</rasd:Address>
        <rasd:Description>SCSI Controller</rasd:Description>
        <rasd:ElementName>SCSI Controller 0</rasd:ElementName>
        <rasd:InstanceID>3</rasd:InstanceID>
        <rasd:ResourceSubType>VirtualSCSI</rasd:ResourceSubType>
        <rasd:ResourceType>6</rasd:ResourceType>
      </Item>
      <Item>
        <rasd:Address>0</rasd:Address>
        <rasd:Description>SATA Controller</rasd:Description>
        <rasd:ElementName>SATA Controller 0</rasd:ElementName>
        <rasd:InstanceID>4</rasd:InstanceID>
        <rasd:ResourceSubType>vmware.sata.ahci</rasd:ResourceSubType>
        <rasd:ResourceType>20</rasd:ResourceType>
      </Item>
      <Item ovf:required="false">
        <rasd:AddressOnParent>1</rasd:AddressOnParent>
        <rasd:AutomaticAllocation>false</rasd:AutomaticAllocation>
        <rasd:ElementName>CD/DVD drive 1</rasd:ElementName>
        <rasd:InstanceID>5</rasd:InstanceID>
        <rasd:Parent>4</rasd:Parent>
        <rasd:ResourceSubType>vmware.cdrom.remotepassthrough</rasd:ResourceSubType>
        <rasd:ResourceType>15</rasd:ResourceType>
      </Item>
      <StorageItem>
        <sasd:AddressOnParent>0</sasd:AddressOnParent>
        <sasd:ElementName>Hard disk 3</sasd:ElementName>
        <sasd:HostResource>ovf:/disk/vmdisk3</sasd:HostResource>
        <sasd:InstanceID>6</sasd:InstanceID>
        <sasd:Parent>4</sasd:Parent>
        <sasd:ResourceType>17</sasd:ResourceType>
      </StorageItem>
      <StorageItem>
        <sasd:AddressOnParent>1</sasd:AddressOnParent>
        <sasd:ElementName>Hard disk 2</sasd:ElementName>
        <sasd:HostResource>ovf:/disk/vmdisk2</sasd:HostResource>
        <sasd:InstanceID>7</sasd:InstanceID>
        <sasd:Parent>3</sasd:Parent>
        <sasd:ResourceType>17</sasd:ResourceType>
      </StorageItem>
      <StorageItem>
        <sasd:AddressOnParent>0</sasd:AddressOnParent>
        <sasd:ElementName>Hard disk 1</sasd:ElementName>
        <sasd:HostResource>ovf:/disk/vmdisk1</sasd:HostResource>
        <sasd:InstanceID>8</sasd:InstanceID>
        <sasd:Parent>3</sasd:Parent>
        <sasd:ResourceType>17</sasd:ResourceType>
      </StorageItem>
      <EthernetPortItem>
        <epasd:Address>00:50:56:8a:12:34</epasd:Address>
        <epasd:AutomaticAllocation>true</epasd:AutomaticAllocation>
        <epasd:Connection>VM Network</epasd:Connection>
        <epasd:Description>VmxNet3 ethernet adapter on &quot;VM Network&quot;</epasd:Description>
        <epasd:ElementName>Network adapter 1</epasd:ElementName>
        <epasd:InstanceID>9</epasd:InstanceID>
        <epasd:ResourceSubType>VmxNet3</epasd:ResourceSubType>
        <epasd:ResourceType>10</epasd:ResourceType>
        <vmw:Config ovf:required="false" vmw:key="slotInfo.pciSlotNumber" vmw:value="192"/>
        <vmw:Config ovf:required="false" vmw:key="connectable.allowGuestControl" vmw:value="true"/>
      </EthernetPortItem>
      <EthernetPortItem>
        <epasd:Address>00:50:56:8A:56:78</epasd:Address>
        <epasd:AutomaticAllocation>true</epasd:AutomaticAllocation>
        <epasd:Connection>Backup Net</epasd:Connection>
        <epasd:Description>E1000e ethernet adapter on &quot;Backup Net&quot;</epasd:Description>
        <epasd:ElementName>Network adapter 2</epasd:ElementName>
        <epasd:InstanceID>10</epasd:InstanceID>
        <epasd:ResourceSubType>E1000e</epasd:ResourceSubType>
        <epasd:ResourceType>10</epasd:ResourceType>
      </EthernetPortItem>
      <Item ovf:required="false">
        <rasd:AutomaticAllocation>false</rasd:AutomaticAllocation>
        <rasd:ElementName>Video card</rasd:ElementName>
        <rasd:InstanceID>11</rasd:InstanceID>
        <rasd:ResourceType>24</rasd:ResourceType>
        <vmw:Config ovf:required="false" vmw:key="videoRamSizeInKB" vmw:value="4096"/>
      </Item>
      <Item ovf:required="false">
        <rasd:AutomaticAllocation>false</rasd:AutomaticAllocation>
        <rasd:ElementName>VMCI device</rasd:ElementName>
        <rasd:InstanceID>12</rasd:InstanceID>
        <rasd:ResourceSubType>vmware.vmci</rasd:ResourceSubType>
        <rasd:ResourceType>1</rasd:ResourceType>
      </Item>
      <vmw:Config ovf:required="false" vmw:key="cpuHotAddEnabled" vmw:value="false"/>
      <vmw:Config ovf:required="false" vmw:key="firmware" vmw:value="efi"/>
      <vmw:Config ovf:required="false" vmw:key="bootOptions.efiSecureBootEnabled" vmw:value="true"/>
      <vmw:ExtraConfig ovf:required="false" vmw:key="nvram" vmw:value="ovf:/file/file4"/>
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>
```

`crates/opl-api/tests/fixtures/workstation.ovf`를 만든다:

```
<?xml version="1.0" encoding="UTF-8"?>
<!--Generated by VMware VMware Workstation 17.5.2 build-23775571, UTC time: 2026-10-29T08:42:10.388Z-->
<Envelope vmw:buildId="build-23775571" xmlns="http://schemas.dmtf.org/ovf/envelope/1" xmlns:cim="http://schemas.dmtf.org/wbem/wscim/1/common" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/1" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData" xmlns:vmw="http://www.vmware.com/schema/ovf" xmlns:vssd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_VirtualSystemSettingData" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <References>
    <File ovf:href="Windows 10 x64-disk1.vmdk" ovf:id="file1" ovf:size="9873420288"/>
  </References>
  <DiskSection>
    <Info>Virtual disk information</Info>
    <Disk ovf:capacity="64" ovf:capacityAllocationUnits="byte * 2^30" ovf:diskId="vmdisk1" ovf:fileRef="file1" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized" ovf:populatedSize="21474836480"/>
  </DiskSection>
  <NetworkSection>
    <Info>The list of logical networks</Info>
    <Network ovf:name="nat">
      <Description>The nat network</Description>
    </Network>
  </NetworkSection>
  <VirtualSystem ovf:id="Windows 10 x64">
    <Info>A virtual machine</Info>
    <OperatingSystemSection ovf:id="1" vmw:osType="windows9-64">
      <Info>The kind of installed guest operating system</Info>
    </OperatingSystemSection>
    <VirtualHardwareSection>
      <Info>Virtual hardware requirements</Info>
      <System>
        <vssd:ElementName>Virtual Hardware Family</vssd:ElementName>
        <vssd:InstanceID>0</vssd:InstanceID>
        <vssd:VirtualSystemIdentifier>Windows 10 x64</vssd:VirtualSystemIdentifier>
        <vssd:VirtualSystemType>vmx-21</vssd:VirtualSystemType>
      </System>
      <Item>
        <rasd:AllocationUnits>hertz * 10^6</rasd:AllocationUnits>
        <rasd:Description>Number of Virtual CPUs</rasd:Description>
        <rasd:ElementName>2 virtual CPU(s)</rasd:ElementName>
        <rasd:InstanceID>1</rasd:InstanceID>
        <rasd:ResourceType>3</rasd:ResourceType>
        <rasd:VirtualQuantity>2</rasd:VirtualQuantity>
      </Item>
      <Item>
        <rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits>
        <rasd:Description>Memory Size</rasd:Description>
        <rasd:ElementName>4096MB of memory</rasd:ElementName>
        <rasd:InstanceID>2</rasd:InstanceID>
        <rasd:ResourceType>4</rasd:ResourceType>
        <rasd:VirtualQuantity>4096</rasd:VirtualQuantity>
      </Item>
      <Item>
        <rasd:Address>0</rasd:Address>
        <rasd:Description>SATA Controller</rasd:Description>
        <rasd:ElementName>sataController0</rasd:ElementName>
        <rasd:InstanceID>3</rasd:InstanceID>
        <rasd:ResourceSubType>vmware.sata.ahci</rasd:ResourceSubType>
        <rasd:ResourceType>20</rasd:ResourceType>
      </Item>
      <Item>
        <rasd:Address>0</rasd:Address>
        <rasd:Description>USB Controller (XHCI)</rasd:Description>
        <rasd:ElementName>usb3</rasd:ElementName>
        <rasd:InstanceID>4</rasd:InstanceID>
        <rasd:ResourceSubType>vmware.usb.xhci</rasd:ResourceSubType>
        <rasd:ResourceType>23</rasd:ResourceType>
      </Item>
      <Item>
        <rasd:AddressOnParent>0</rasd:AddressOnParent>
        <rasd:ElementName>disk0</rasd:ElementName>
        <rasd:HostResource>ovf:/disk/vmdisk1</rasd:HostResource>
        <rasd:InstanceID>5</rasd:InstanceID>
        <rasd:Parent>3</rasd:Parent>
        <rasd:ResourceType>17</rasd:ResourceType>
      </Item>
      <Item ovf:required="false">
        <rasd:AddressOnParent>1</rasd:AddressOnParent>
        <rasd:AutomaticAllocation>false</rasd:AutomaticAllocation>
        <rasd:ElementName>cdrom0</rasd:ElementName>
        <rasd:InstanceID>6</rasd:InstanceID>
        <rasd:Parent>3</rasd:Parent>
        <rasd:ResourceSubType>vmware.cdrom.remoteatapi</rasd:ResourceSubType>
        <rasd:ResourceType>15</rasd:ResourceType>
      </Item>
      <Item ovf:required="false">
        <rasd:AutomaticAllocation>false</rasd:AutomaticAllocation>
        <rasd:ElementName>floppy0</rasd:ElementName>
        <rasd:InstanceID>7</rasd:InstanceID>
        <rasd:ResourceSubType>vmware.floppy.remotedevice</rasd:ResourceSubType>
        <rasd:ResourceType>14</rasd:ResourceType>
      </Item>
      <Item>
        <rasd:AddressOnParent>2</rasd:AddressOnParent>
        <rasd:AutomaticAllocation>true</rasd:AutomaticAllocation>
        <rasd:Connection>nat</rasd:Connection>
        <rasd:Description>E1000e ethernet adapter on &quot;nat&quot;</rasd:Description>
        <rasd:ElementName>ethernet0</rasd:ElementName>
        <rasd:InstanceID>8</rasd:InstanceID>
        <rasd:ResourceSubType>E1000e</rasd:ResourceSubType>
        <rasd:ResourceType>10</rasd:ResourceType>
      </Item>
      <Item ovf:required="false">
        <rasd:AutomaticAllocation>false</rasd:AutomaticAllocation>
        <rasd:ElementName>sound</rasd:ElementName>
        <rasd:InstanceID>9</rasd:InstanceID>
        <rasd:ResourceSubType>vmware.soundcard.hdaudio</rasd:ResourceSubType>
        <rasd:ResourceType>1</rasd:ResourceType>
      </Item>
      <Item ovf:required="false">
        <rasd:AutomaticAllocation>false</rasd:AutomaticAllocation>
        <rasd:ElementName>video</rasd:ElementName>
        <rasd:InstanceID>10</rasd:InstanceID>
        <rasd:ResourceType>24</rasd:ResourceType>
      </Item>
      <vmw:Config ovf:required="false" vmw:key="tools.syncTimeWithHost" vmw:value="false"/>
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>
```

`crates/opl-api/tests/ova.rs`를 만든다:

```rust
//! OVA(tar) 항목 표, OVF 해석, `.mf` 해석, 디스크 추출과 해시 확인.

mod common;

use std::path::{Path, PathBuf};

use common::ustar::{Header, Tar};
use opl_api::vm_imports::ova::{
    Digest, OvaEntry, OvaError, extract, parse_manifest, parse_ova, read_index,
};
use opl_api::vm_imports::ovf::parse_ovf;
use opl_types::vm::{Firmware, MacAddr};
use opl_types::vm_import::{SourceDisk, SourceNic};
use sha2::Digest as _;
use tempfile::TempDir;

const VSPHERE: &str = include_str!("fixtures/vsphere.ovf");
const WORKSTATION: &str = include_str!("fixtures/workstation.ovf");
const GIB: u64 = 1 << 30;

fn write(dir: &TempDir, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join("source.ova");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn index(bytes: Vec<u8>) -> Result<Vec<(String, u64, u64)>, OvaError> {
    let dir = TempDir::new().unwrap();
    let index = read_index(&write(&dir, &bytes))?;
    Ok(index
        .entries
        .into_iter()
        .map(|e| (e.name, e.offset, e.size))
        .collect())
}

fn err(r: Result<impl std::fmt::Debug, OvaError>) -> String {
    r.unwrap_err().to_string()
}

fn header(name: &str, typeflag: u8, size: u64) -> Header<'_> {
    Header {
        name,
        prefix: "",
        typeflag,
        size,
        base256: false,
    }
}

// --- tar 항목 표 ---

#[test]
fn indexes_plain_files() {
    let tar = Tar::new()
        .file("a.ovf", b"hello")
        .file("b.vmdk", &[7u8; 600])
        .finish();
    assert_eq!(
        index(tar).unwrap(),
        vec![
            ("a.ovf".into(), 512, 5),
            // 앞 항목: 헤더 512 + 본문 512(채움)
            ("b.vmdk".into(), 1536, 600),
        ]
    );
}

#[test]
fn joins_ustar_prefix_and_strips_dot_slash() {
    let long = format!("{}/disk.vmdk", "d".repeat(120));
    let tar = Tar::new().file(&long, b"x").file("./vm.ovf", b"y").finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec![long, "vm.ovf".to_string()]);
}

#[test]
fn pax_header_overrides_path_and_size() {
    let long = format!("{}.vmdk", "p".repeat(150));
    let tar = Tar::new()
        .pax(&[("path", &long), ("size", "700"), ("mtime", "1761811905.5")])
        // 헤더의 이름·크기 칸은 pax 값으로 덮인다
        .raw(header("short.vmdk", b'0', 0), &[1u8; 700])
        .file("after.ovf", b"z")
        .finish();
    let entries = index(tar).unwrap();
    assert_eq!(entries[0], (long, 1536, 700));
    // 다음 항목은 pax 값이 남지 않는다
    assert_eq!(entries[1], ("after.ovf".into(), 1536 + 1024 + 512, 1));
}

#[test]
fn skips_global_pax_and_directories() {
    let tar = Tar::new()
        .raw(
            header("pax_global_header", b'g', 20),
            b"20 comment=opLite!!\n",
        )
        .dir("vm/")
        .file("vm/vm.ovf", b"o")
        .finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["vm/vm.ovf".to_string()]);
}

#[test]
fn gnu_long_name_names_the_next_entry() {
    let long = format!("{}.vmdk", "g".repeat(200));
    let tar = Tar::new()
        .gnu_long_name(&long)
        .file("cut.vmdk", b"abc")
        .file("next.ovf", b"n")
        .finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec![long, "next.ovf".to_string()]);
}

#[test]
fn pax_values_skip_gnu_long_name_headers() {
    // pax 값은 사이의 GNU 긴 이름 헤더가 아니라 다음 파일에 쓴다. pax `path`가 GNU 이름보다 앞선다.
    let long = format!("{}.vmdk", "q".repeat(150));
    let tar = Tar::new()
        .pax(&[("path", &long), ("size", "700")])
        .gnu_long_name("gnu-name.vmdk")
        .raw(header("short.vmdk", b'0', 0), &[1u8; 700])
        .file("after.ovf", b"z")
        .finish();
    let entries = index(tar).unwrap();
    assert_eq!(entries[0], (long, 2560, 700));
    assert_eq!(entries[1].0, "after.ovf");
    // GNU 긴 이름 뒤의 pax 헤더도 이름을 가져가지 않는다.
    let tar = Tar::new()
        .gnu_long_name("gnu-name.vmdk")
        .pax(&[("mtime", "1")])
        .file("cut.vmdk", b"abc")
        .finish();
    let names: Vec<String> = index(tar).unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["gnu-name.vmdk".to_string()]);
}

#[test]
fn limits_name_length_and_entry_count() {
    let long = "n".repeat(4097);
    let tar = Tar::new().pax(&[("path", &long)]).file("x", b"x").finish();
    assert_eq!(
        err(index(tar)),
        "a file name in the OVA is longer than 4 KiB"
    );
    let tar = Tar::new().gnu_long_name(&long).file("x", b"x").finish();
    assert_eq!(
        err(index(tar)),
        "a file name in the OVA is longer than 4 KiB"
    );
    // 4 KiB까지는 된다.
    let tar = Tar::new()
        .gnu_long_name(&long[1..])
        .file("x", b"x")
        .finish();
    assert_eq!(index(tar).unwrap()[0].0, long[1..]);

    let many = (0..10_000).fold(Tar::new(), |t, i| t.file(&format!("f{i}"), b""));
    let dir = TempDir::new().unwrap();
    let path = write(&dir, &many.into_bytes());
    // 이름으로 찾는다.
    assert_eq!(read_index(&path).unwrap().get("f9999").unwrap().size, 0);
    let mut too_many = std::fs::read(&path).unwrap();
    too_many.extend(Tar::new().file("f10000", b"").finish());
    assert_eq!(err(index(too_many)), "the OVA has more than 10000 entries");
}

#[test]
fn reads_base256_sizes() {
    let tar = Tar::new()
        .raw(
            Header {
                base256: true,
                ..header("big.vmdk", b'0', 3)
            },
            b"abc",
        )
        .finish();
    assert_eq!(index(tar).unwrap(), vec![("big.vmdk".into(), 512, 3)]);
}

#[test]
fn ends_at_zero_block_or_end_of_file() {
    // 0 블록 뒤의 것은 읽지 않는다
    let mut tar = Tar::new().file("a.ovf", b"a").finish();
    tar.extend(Tar::new().file("b.ovf", b"b").finish());
    assert_eq!(index(tar).unwrap().len(), 1);
    // 끝 표시 없이 파일이 끝나도 된다
    let tar = Tar::new().file("a.ovf", b"a").into_bytes();
    assert_eq!(index(tar).unwrap().len(), 1);
}

#[test]
fn rejects_broken_archives() {
    // 크기가 파일을 넘는다
    let mut tar = Tar::new()
        .raw(header("disk.vmdk", b'0', 4096), &[0u8; 512])
        .into_bytes();
    assert_eq!(err(index(tar.clone())), "the OVA is truncated");
    // 헤더가 잘렸다
    tar = Tar::new().file("a.ovf", b"a").into_bytes();
    tar.extend_from_slice(&[1u8; 100]);
    assert_eq!(err(index(tar)), "the OVA is truncated");
    // tar가 아니다(체크섬이 맞지 않는다)
    assert_eq!(
        err(index(vec![b'x'; 2048])),
        "the file is not an OVA (tar) archive"
    );
    // 지원하지 않는 항목(심볼릭 링크)
    let tar = Tar::new().raw(header("link", b'2', 0), &[]).finish();
    assert_eq!(
        err(index(tar)),
        "unsupported tar entry type '2' for \"link\""
    );
    // 이름이 겹친다
    let tar = Tar::new()
        .file("a.vmdk", b"1")
        .file("a.vmdk", b"2")
        .finish();
    assert_eq!(err(index(tar)), "duplicate file \"a.vmdk\" in the OVA");
}

// --- OVF ---

#[test]
fn parses_vsphere_ovf() {
    let ovf = parse_ovf(VSPHERE).unwrap();
    let vm = ovf.vm;
    assert_eq!(vm.name, "web01");
    assert_eq!(vm.guest_os, "ubuntu64Guest");
    assert_eq!(vm.firmware, Firmware::Uefi);
    assert_eq!((vm.vcpus, vm.cores_per_socket), (4, 2));
    assert_eq!(vm.memory_mib, 8192);
    // 컨트롤러가 나온 순서(SCSI → SATA), 그 안에서 주소 순서
    assert_eq!(
        vm.disks,
        vec![
            SourceDisk {
                name: "disk0".into(),
                file: "web01-disk1.vmdk".into(),
                capacity_bytes: 40 * GIB,
                controller: "scsi".into(),
            },
            SourceDisk {
                name: "disk1".into(),
                file: "web01-disk2.vmdk".into(),
                capacity_bytes: 10 * GIB,
                controller: "scsi".into(),
            },
            SourceDisk {
                name: "disk2".into(),
                file: "web01-disk3.vmdk".into(),
                capacity_bytes: 2 * GIB,
                controller: "sata".into(),
            },
        ]
    );
    assert_eq!(
        vm.nics,
        vec![
            SourceNic {
                network: "VM Network".into(),
                adapter: "vmxnet3".into(),
                mac: Some("00:50:56:8a:12:34".parse::<MacAddr>().unwrap()),
            },
            SourceNic {
                network: "Backup Net".into(),
                adapter: "e1000e".into(),
                mac: Some("00:50:56:8a:56:78".parse::<MacAddr>().unwrap()),
            },
        ]
    );
    // 비디오 카드·VMCI는 경고 없이 뺀다. 디스크가 두 컨트롤러에 걸쳐 있다.
    assert_eq!(
        ovf.warnings,
        vec![
            "CD-ROM drive is not imported",
            "disks are on more than one controller; check that disk0 is the boot disk"
        ]
    );
    assert_eq!(
        ovf.files,
        vec![
            "web01-disk1.vmdk",
            "web01-disk2.vmdk",
            "web01-disk3.vmdk",
            "web01-file1.nvram"
        ]
    );
}

#[test]
fn parses_workstation_ovf() {
    let ovf = parse_ovf(WORKSTATION).unwrap();
    let vm = ovf.vm;
    // Name이 없으면 ovf:id
    assert_eq!(vm.name, "Windows 10 x64");
    assert_eq!(vm.guest_os, "windows9-64");
    assert_eq!(vm.firmware, Firmware::Bios);
    // CoresPerSocket이 없으면 1
    assert_eq!((vm.vcpus, vm.cores_per_socket), (2, 1));
    assert_eq!(vm.memory_mib, 4096);
    assert_eq!(
        vm.disks,
        vec![SourceDisk {
            name: "disk0".into(),
            file: "Windows 10 x64-disk1.vmdk".into(),
            capacity_bytes: 64 * GIB,
            controller: "sata".into(),
        }]
    );
    assert_eq!(
        vm.nics,
        vec![SourceNic {
            network: "nat".into(),
            adapter: "e1000e".into(),
            mac: None,
        }]
    );
    assert_eq!(
        ovf.warnings,
        vec![
            "USB controller is not imported",
            "CD-ROM drive is not imported",
            "floppy drive is not imported",
            "sound is not imported",
        ]
    );
}

#[test]
fn reads_other_memory_units_and_cim_os_ids() {
    // osType이 없으면 CIM 번호로 Windows인지만 안다
    let ovf = WORKSTATION
        .replace(" vmw:osType=\"windows9-64\"", "")
        .replace("ovf:id=\"1\"", "ovf:id=\"105\"")
        .replace(
            "<rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits>",
            "<rasd:AllocationUnits>MegaBytes</rasd:AllocationUnits>",
        );
    let vm = parse_ovf(&ovf).unwrap().vm;
    assert_eq!(vm.guest_os, "windows");
    assert_eq!(vm.memory_mib, 4096);

    let ovf = VSPHERE.replace(" vmw:osType=\"ubuntu64Guest\"", "");
    assert_eq!(
        parse_ovf(&ovf).unwrap().vm.guest_os,
        "Ubuntu Linux (64-bit)"
    );
}

#[test]
fn orders_controllers_of_a_kind_by_address() {
    // SATA 컨트롤러를 SCSI 1번(Address 0)으로, 원래 SCSI 컨트롤러를 Address 1로 바꾼다.
    let ovf = VSPHERE
        .replace(
            "<rasd:Address>0</rasd:Address>\n        <rasd:Description>SCSI Controller",
            "<rasd:Address>1</rasd:Address>\n        <rasd:Description>SCSI Controller",
        )
        .replace(
            "<rasd:ResourceSubType>vmware.sata.ahci</rasd:ResourceSubType>\n        <rasd:ResourceType>20</rasd:ResourceType>",
            "<rasd:ResourceSubType>VirtualSCSI</rasd:ResourceSubType>\n        <rasd:ResourceType>6</rasd:ResourceType>",
        );
    let ovf = parse_ovf(&ovf).unwrap();
    let files: Vec<&str> = ovf.vm.disks.iter().map(|d| d.file.as_str()).collect();
    assert_eq!(
        files,
        ["web01-disk3.vmdk", "web01-disk1.vmdk", "web01-disk2.vmdk"]
    );
    assert!(ovf.vm.disks.iter().all(|d| d.controller == "scsi"));
    assert!(
        ovf.warnings
            .iter()
            .any(|w| w.starts_with("disks are on more than one controller"))
    );

    // 한 컨트롤러에만 디스크가 있으면 경고하지 않는다.
    let ovf = parse_ovf(WORKSTATION).unwrap();
    assert!(!ovf.warnings.iter().any(|w| w.starts_with("disks are on")));
}

/// `count`개의 디스크(SCSI 컨트롤러 하나)와 NIC를 가진 OVF
fn ovf_with(disks: usize, nics: usize) -> String {
    let mut files = String::new();
    let mut section = String::new();
    let mut items = String::new();
    for i in 0..disks {
        files += &format!(r#"<File ovf:href="d{i}.vmdk" ovf:id="f{i}"/>"#);
        section += &format!(
            r#"<Disk ovf:capacity="1" ovf:capacityAllocationUnits="byte * 2^30" ovf:diskId="d{i}" ovf:fileRef="f{i}"/>"#
        );
        items += &format!(
            "<Item><rasd:AddressOnParent>{i}</rasd:AddressOnParent><rasd:HostResource>ovf:/disk/d{i}</rasd:HostResource><rasd:InstanceID>{}</rasd:InstanceID><rasd:Parent>3</rasd:Parent><rasd:ResourceType>17</rasd:ResourceType></Item>",
            100 + i
        );
    }
    for i in 0..nics {
        items += &format!(
            "<Item><rasd:Connection>net{i}</rasd:Connection><rasd:InstanceID>{}</rasd:InstanceID><rasd:ResourceSubType>VmxNet3</rasd:ResourceSubType><rasd:ResourceType>10</rasd:ResourceType></Item>",
            200 + i
        );
    }
    format!(
        r#"<Envelope xmlns="http://schemas.dmtf.org/ovf/envelope/1" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/1" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData">
  <References>{files}</References>
  <DiskSection>{section}</DiskSection>
  <VirtualSystem ovf:id="vm">
    <VirtualHardwareSection>
      <Item><rasd:InstanceID>1</rasd:InstanceID><rasd:ResourceType>3</rasd:ResourceType><rasd:VirtualQuantity>1</rasd:VirtualQuantity></Item>
      <Item><rasd:InstanceID>2</rasd:InstanceID><rasd:ResourceType>4</rasd:ResourceType><rasd:VirtualQuantity>512</rasd:VirtualQuantity></Item>
      <Item><rasd:InstanceID>3</rasd:InstanceID><rasd:ResourceType>6</rasd:ResourceType></Item>
      {items}
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>"#
    )
}

#[test]
fn limits_disks_and_nics() {
    let ovf = parse_ovf(&ovf_with(32, 16)).unwrap();
    assert_eq!((ovf.vm.disks.len(), ovf.vm.nics.len()), (32, 16));
    assert_eq!(
        err(parse_ovf(&ovf_with(33, 1))),
        "the OVF has 33 disks; at most 32 are supported"
    );
    assert_eq!(
        err(parse_ovf(&ovf_with(1, 17))),
        "the OVF has 17 network adapters; at most 16 are supported"
    );
}

#[test]
fn rejects_unsupported_ovf() {
    let cases = [
        (
            VSPHERE
                .replace("<VirtualSystem ovf:id=\"web01\">", "<VirtualSystemCollection ovf:id=\"vapp\"><VirtualSystem ovf:id=\"web01\">")
                .replace("</VirtualSystem>", "</VirtualSystem></VirtualSystemCollection>"),
            "only single-VM OVA files are supported",
        ),
        (
            VSPHERE.replace(
                "ovf:id=\"file1\"",
                "ovf:id=\"file1\" ovf:compression=\"gzip\"",
            ),
            "compressed disk files (ovf:compression) are not supported",
        ),
        (
            VSPHERE.replace(
                "ovf:id=\"file1\"",
                "ovf:id=\"file1\" ovf:chunkSize=\"2147483648\"",
            ),
            "split disk files (ovf:chunkSize) are not supported",
        ),
        (
            VSPHERE.replace(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
                "<?xml version=\"1.0\"?><!DOCTYPE Envelope [<!ENTITY x SYSTEM \"file:///etc/passwd\">]>",
            ),
            "invalid OVF XML",
        ),
        (
            VSPHERE.replace("ovf:/disk/vmdisk2", "ovf:/disk/nope"),
            "disk \"nope\" is not in the DiskSection",
        ),
        (
            VSPHERE.replace("ovf:fileRef=\"file2\"", "ovf:fileRef=\"file9\""),
            "disk \"vmdisk2\" refers to an unknown file \"file9\"",
        ),
        (
            VSPHERE.replace("vmdk.html#streamOptimized\" ovf:populatedSize=\"0\"/>\n    <Disk ovf:capacity=\"2048\"", "raw\" ovf:populatedSize=\"0\"/>\n    <Disk ovf:capacity=\"2048\""),
            "disk \"vmdisk2\" is not a VMDK",
        ),
    ];
    for (xml, message) in cases {
        let e = err(parse_ovf(&xml));
        assert!(
            e.starts_with(message),
            "{e:?} does not start with {message:?}"
        );
    }
}

// --- .mf ---

#[test]
fn parses_manifest_lines() {
    let sha256 = "a".repeat(64);
    let sha1 = "B".repeat(40);
    let text =
        format!("SHA256(web01.ovf)= {sha256}\r\nSHA1 (Windows 10 x64-disk1.vmdk) = {sha1}\n\n");
    let text = format!(
        "{text}SHA384(a.vmdk)= {}\nSHA512(b.vmdk)= {}\nMD5(c.vmdk)= 00\n",
        "c".repeat(96),
        "d".repeat(128)
    );
    let mf = parse_manifest(&text).unwrap();
    assert_eq!(mf.digests.len(), 4);
    assert_eq!(mf.digests["web01.ovf"], Digest::Sha256(sha256));
    // 16진수는 소문자로
    assert_eq!(
        mf.digests["Windows 10 x64-disk1.vmdk"],
        Digest::Sha1("b".repeat(40))
    );
    assert_eq!(mf.digests["a.vmdk"], Digest::Sha384("c".repeat(96)));
    assert_eq!(mf.digests["b.vmdk"], Digest::Sha512("d".repeat(128)));
    // 모르는 알고리즘은 오류가 아니라 확인하지 않는 파일이다.
    assert_eq!(mf.unsupported, ["c.vmdk".to_string()].into());

    for bad in [
        "SHA256(a.vmdk) abc".to_string(),
        format!("SHA256(a.vmdk)= {}", "a".repeat(40)),
        format!("SHA1(a.vmdk)= {}", "z".repeat(40)),
        format!("SHA512(a.vmdk)= {}", "a".repeat(64)),
    ] {
        let e = err(parse_manifest(&bad));
        assert!(e.starts_with("invalid manifest line"), "{bad}: {e}");
    }
}

// --- OVA 전체 ---

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

/// vSphere 견본 OVF와 그 파일들
fn vsphere_ova(tweak: impl FnOnce(Tar) -> Tar) -> Vec<u8> {
    let disk = b"KDMV fake disk";
    let mf = format!(
        "SHA256(web01.ovf)= {}\nSHA256(web01-disk1.vmdk)= {}\n",
        sha256_hex(VSPHERE.as_bytes()),
        sha256_hex(disk)
    );
    let tar = Tar::new()
        .file("web01.ovf", VSPHERE.as_bytes())
        .file("web01.mf", mf.as_bytes())
        .file("web01-disk1.vmdk", disk)
        .file("web01-disk2.vmdk", disk)
        .file("web01-disk3.vmdk", disk)
        .file("web01-file1.nvram", b"nvram");
    tweak(tar).finish()
}

#[test]
fn parses_whole_ova() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, &vsphere_ova(|t| t.file("web01.cert", b"cert")));
    let ova = parse_ova(&path).unwrap();
    assert_eq!(ova.vm.name, "web01");
    assert_eq!(ova.vm.disks.len(), 3);
    // `.mf`에 없는 디스크는 확인하지 않는다고 알린다.
    assert_eq!(
        ova.warnings,
        vec![
            "CD-ROM drive is not imported",
            "disks are on more than one controller; check that disk0 is the boot disk",
            "the OVA signature (.cert) is not verified",
            "web01-disk2.vmdk is not listed in the manifest; it is not verified",
            "web01-disk3.vmdk is not listed in the manifest; it is not verified",
        ]
    );
    assert_eq!(ova.index.entries.len(), 7);
    assert_eq!(ova.index.get("web01-disk1.vmdk").unwrap().size, 14);
    assert_eq!(
        ova.manifest["web01-disk1.vmdk"],
        Digest::Sha256(sha256_hex(b"KDMV fake disk"))
    );
}

#[test]
fn ova_without_manifest_is_fine() {
    let dir = TempDir::new().unwrap();
    let tar = Tar::new()
        .file("Windows 10 x64.ovf", WORKSTATION.as_bytes())
        .file("Windows 10 x64-disk1.vmdk", b"disk")
        .finish();
    let ova = parse_ova(&write(&dir, &tar)).unwrap();
    assert!(ova.manifest.is_empty());
    assert_eq!(ova.vm.disks[0].file, "Windows 10 x64-disk1.vmdk");
    // `.mf`가 없으면 확인하지 않는다는 경고도 없다.
    assert!(!ova.warnings.iter().any(|w| w.contains("manifest")));
}

#[test]
fn unsupported_manifest_digest_is_a_warning() {
    let dir = TempDir::new().unwrap();
    let tar = Tar::new()
        .file("Windows 10 x64.ovf", WORKSTATION.as_bytes())
        .file("w.mf", b"MD5(Windows 10 x64-disk1.vmdk)= 00\n")
        .file("Windows 10 x64-disk1.vmdk", b"disk")
        .finish();
    let ova = parse_ova(&write(&dir, &tar)).unwrap();
    assert!(ova.manifest.is_empty());
    assert_eq!(
        ova.warnings.last().unwrap(),
        "the manifest uses an unsupported digest for Windows 10 x64-disk1.vmdk; it is not verified"
    );
}

#[test]
fn warnings_are_unique_and_capped() {
    // 이름이 다른 가져오지 않는 장치 60개와 같은 이름 둘
    let devices: String = (0..60)
        .map(|i| format!("<Item><rasd:ElementName>dev{i}</rasd:ElementName><rasd:InstanceID>{}</rasd:InstanceID><rasd:ResourceType>1</rasd:ResourceType></Item>", 1000 + i))
        .chain(["<Item><rasd:ElementName>dev0</rasd:ElementName><rasd:InstanceID>2000</rasd:InstanceID><rasd:ResourceType>1</rasd:ResourceType></Item>".to_string()])
        .collect();
    let ovf = WORKSTATION.replace(
        "</VirtualHardwareSection>",
        &format!("{devices}</VirtualHardwareSection>"),
    );
    let dir = TempDir::new().unwrap();
    let tar = Tar::new()
        .file("w.ovf", ovf.as_bytes())
        .file("Windows 10 x64-disk1.vmdk", b"disk")
        .finish();
    let warnings = parse_ova(&write(&dir, &tar)).unwrap().warnings;
    // 견본의 경고 넷 + 장치 60개 = 64개 중 50개와 나머지 개수
    assert_eq!(warnings.len(), 51);
    assert_eq!(warnings[4], "dev0 is not imported");
    assert_eq!(warnings[50], "… and 14 more");
}

/// OVA에 넣을 (이름, 내용)
type Files<'a> = Vec<(&'a str, &'a [u8])>;

#[test]
fn rejects_bad_ova_layouts() {
    let ovf = WORKSTATION.as_bytes();
    let disk = ("Windows 10 x64-disk1.vmdk", &b"disk"[..]);
    let big = vec![b' '; (4 << 20) + 1];
    let cases: Vec<(Files, &str)> = vec![
        (vec![disk], "the OVA has no .ovf file"),
        (
            vec![("a.ovf", ovf), ("b.ovf", ovf), disk],
            "the OVA has more than one .ovf file",
        ),
        (
            vec![("a.ovf", &big[..]), disk],
            "the .ovf file is larger than 4 MiB",
        ),
        (
            vec![("a.ovf", ovf), ("a.mf", &b""[..]), ("b.mf", &b""[..]), disk],
            "the OVA has more than one .mf file",
        ),
        (
            vec![("a.ovf", ovf)],
            "file \"Windows 10 x64-disk1.vmdk\" is not in the OVA",
        ),
        (
            vec![("a.ovf", ovf), ("a.mf", &b"garbage"[..]), disk],
            "invalid manifest line",
        ),
    ];
    for (files, message) in cases {
        let dir = TempDir::new().unwrap();
        let tar = files
            .iter()
            .fold(Tar::new(), |t, (name, data)| t.file(name, data))
            .finish();
        let e = err(parse_ova(&write(&dir, &tar)));
        assert!(
            e.starts_with(message),
            "{e:?} does not start with {message:?}"
        );
    }
}

// --- 추출 ---

fn entry_of(path: &Path, name: &str) -> OvaEntry {
    read_index(path).unwrap().get(name).unwrap().clone()
}

#[test]
fn extracts_and_checks_digest() {
    let dir = TempDir::new().unwrap();
    let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let tar = Tar::new()
        .file("x.ovf", b"o")
        .file("disk.vmdk", &data)
        .finish();
    let path = write(&dir, &tar);
    let entry = entry_of(&path, "disk.vmdk");
    let dest = dir.path().join("disk-out.vmdk");

    extract(
        &path,
        &entry,
        &dest,
        Some(&Digest::Sha256(sha256_hex(&data))),
    )
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    let sha1 = hex::encode(sha1::Sha1::digest(&data));
    extract(&path, &entry, &dest, Some(&Digest::Sha1(sha1))).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    let sha384 = hex::encode(sha2::Sha384::digest(&data));
    extract(&path, &entry, &dest, Some(&Digest::Sha384(sha384))).unwrap();
    let sha512 = hex::encode(sha2::Sha512::digest(&data));
    extract(&path, &entry, &dest, Some(&Digest::Sha512(sha512))).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);

    // 해시가 없으면 복사만
    std::fs::remove_file(&dest).unwrap();
    extract(&path, &entry, &dest, None).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[test]
fn digest_mismatch_removes_the_copy() {
    let dir = TempDir::new().unwrap();
    let tar = Tar::new().file("disk.vmdk", b"payload").finish();
    let path = write(&dir, &tar);
    let entry = entry_of(&path, "disk.vmdk");
    let dest = dir.path().join("disk-out.vmdk");

    let wrong = Digest::Sha256(sha256_hex(b"other"));
    assert_eq!(
        err(extract(&path, &entry, &dest, Some(&wrong))),
        "checksum mismatch for disk.vmdk"
    );
    assert!(!dest.exists());
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1049개, 실패 0

Run: `cargo test -p opl-api --test ova`
Expected: PASS

Run: `cargo deny check`
Expected: ok

- [ ] **Step 3: 커밋**

```bash
git add Cargo.toml Cargo.lock crates
git commit -m "feat(opl-api): OVA·OVF 해석 — tar 항목 표(ustar·pax·GNU 긴 이름·base-256), OVF에서 원본 VM 읽기(CPU·메모리 단위·efi·컨트롤러와 디스크 순서·NIC·가져오지 않는 장치 경고), .mf 해석, 해시를 확인하는 디스크 추출, 시험용 ustar 작성기와 vSphere·Workstation OVF 견본"
```

---

### Task 3: 올리기·목록 API

`POST /vm-imports/ova/check?name=&size=`(204)와 `POST /vm-imports/ova?name=&file_name=`(본문 OVA → `{upload_dir}/imports/{id}/source.ova`에 받아 해석·제안 → 201 `ready` 가져오기; 실패·끊김이면 디렉터리를 지움, 잘못된 OVA는 400 `invalid_ova`), `GET /vm-imports`, `GET /vm-imports/{key}`. 이름 `ova`는 예약, 위트니스 400, 여유 507. 볼륨 올리기의 받기·여유 검사를 함께 쓴다. 노드 시작 때 리소스가 없는 `imports/{id}`를 지우고, 원본이 없는 `ready`는 `failed`. operator 쓰기 권한, watch 종류 `vm-import`.

**Files:**
- Create: `crates/opl-api/tests/vm_imports.rs`
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/vm_imports.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/src/watch.rs`
- Modify: `crates/opl-api/tests/openapi.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/auth.rs`에서

```rust
                        kind,
                        Kind::Vm | Kind::Volume | Kind::Backup | Kind::BackupJob
                    )
```

을 다음으로 바꾼다:

```rust
                        kind,
                        Kind::Vm | Kind::Volume | Kind::Backup | Kind::BackupJob | Kind::VmImport
                    )
```

`crates/opl-api/src/auth.rs`에서

```rust
        assert!(a.check(&op, Action::Write, Kind::BackupJob));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
```

을 다음으로 바꾼다:

```rust
        assert!(a.check(&op, Action::Write, Kind::BackupJob));
        assert!(a.check(&op, Action::Write, Kind::VmImport));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(backup_jobs::run_backup_job))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(backup_jobs::run_backup_job))
        .routes(routes!(r::list_vm_imports))
        .routes(routes!(r::get_vm_import))
        .routes(routes!(vm_imports::upload_ova))
        .routes(routes!(vm_imports::check_ova))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

`crates/opl-api/src/resources.rs`에서

```rust
use opl_types::vm::{VmSpec, VmStatus};
use serde::{Deserialize, Serialize};
```

을 다음으로 바꾼다:

```rust
use opl_types::vm::{VmSpec, VmStatus};
use opl_types::vm_import::{VmImportSpec, VmImportStatus};
use serde::{Deserialize, Serialize};
```

`crates/opl-api/src/resources.rs`에서

```rust
    "backups"
);
```

을 다음으로 바꾼다:

```rust
    "backups"
);
list_and_get!(
    list_vm_imports,
    get_vm_import,
    VmImportSpec,
    VmImportStatus,
    "/vm-imports",
    "/vm-imports/{key}",
    "vm-imports"
);
```

`crates/opl-api/src/tasks.rs`에서

```rust
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
```

을 다음으로 바꾼다:

```rust
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일, VM 가져오기의 남은 파일([`crate::vm_imports::sweep`]).
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
```

`crates/opl-api/src/tasks.rs`에서

```rust
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
```

을 다음으로 바꾼다:

```rust
    };
    crate::vm_imports::sweep(&ctx.cluster, ctx.config.node, dir).await;
    let Ok(entries) = std::fs::read_dir(dir) else {
```

`crates/opl-api/src/tasks.rs`에서

```rust
        let path = entry.path();
        let wanted = unfinished
```

을 다음으로 바꾼다:

```rust
        let path = entry.path();
        // 하위 디렉터리(`imports`)는 가져오기의 정리가 맡는다.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let wanted = unfinished
```

`crates/opl-api/src/vm_imports.rs`에서

```rust
//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아낸다.

pub mod ova;
pub mod ovf;
```

을 다음으로 바꾼다:

```rust
//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아내고 opLite VM 설정을 제안한다.
//! 원본은 올린 노드의 `{upload_dir}/imports/{가져오기 id}/source.ova`에 둔다.

pub mod ova;
pub mod ovf;

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::Cluster;
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::vm_import::{
    ImportPhase, ImportSource, OvaSource, VmImport, VmImportSpec, VmImportStatus, propose,
};
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::check_name;
use crate::state::AppState;
use crate::volumes::{ImageFile, check_upload_space, content_length, receive};
use ova::{OvaError, parse_ova};

/// `/vm-imports/ova` 경로와 겹치므로 가져오기 이름으로 쓸 수 없다.
const RESERVED_NAME: &str = "ova";
/// 가져오기 디렉터리 안의 원본 OVA 파일 이름
pub(crate) const SOURCE_OVA: &str = "source.ova";

/// 가져오기 `id`의 원본 디렉터리
pub(crate) fn import_dir(upload_dir: &FsPath, id: Uuid) -> PathBuf {
    upload_dir.join("imports").join(id.to_string())
}

/// OVA를 쓸 수 없으면 400 `invalid_ova`(이유를 메시지에), 읽기 오류는 500
pub(crate) fn ova_error(e: OvaError) -> ApiError {
    match e {
        OvaError::Invalid(message) => {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_ova", message)
        }
        OvaError::Io(e) => ApiError::internal(e),
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct OvaUploadQuery {
    /// 가져오기 이름
    pub name: String,
    /// 올린 파일 이름(표시용). 없으면 `{name}.ova`
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct OvaCheckQuery {
    /// 가져오기 이름
    pub name: String,
    /// 올릴 파일 크기(바이트)
    pub size: u64,
}

/// OVA를 받기 전의 확인. 사전 확인과 올리기가 함께 쓴다. 확인 뒤에 다른 요청이 이름을 가져가면
/// 저장소에 만들 때 `name_taken`이 난다.
async fn check_upload(
    state: &AppState,
    principal: &Principal,
    name: &str,
    size: u64,
) -> Result<(), ApiError> {
    state.authorize(principal, Action::Write, VmImportSpec::KIND)?;
    check_name(name)?;
    if name == RESERVED_NAME {
        return Err(ApiError::bad_request(format!(
            "the name {RESERVED_NAME} is reserved"
        )));
    }
    // 원본은 올린 노드에 남고 가져오기도 그 노드에서 하므로 VM을 돌리는 노드여야 한다.
    if let Some(node) = state.cluster.get::<NodeSpec>(state.node)?
        && node.spec.witness
    {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness node and cannot run VMs; upload to another node",
            node.name
        )));
    }
    if state.cluster.get_by_name::<VmImportSpec>(name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("vm-import {name:?} already exists"),
        ));
    }
    check_upload_space(state, size).await
}

/// OVA를 받기 전에 받을 수 있는지 확인한다(204). 거절 이유는 올리기와 같다. 본문은 받지 않는다.
#[utoipa::path(post, path = "/vm-imports/ova/check", tag = "vm-imports",
    params(OvaCheckQuery),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_ova(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<OvaCheckQuery>,
) -> Result<StatusCode, ApiError> {
    check_upload(&state, &principal, &q.name, q.size).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 요청 본문(OVA)을 이 노드에 받아 해석하고, 원본 VM과 제안을 담은 `ready` 가져오기를 돌려준다(201).
/// OVA를 쓸 수 없으면 400 `invalid_ova`. 실패하거나 연결이 끊기면 받은 파일을 지운다.
#[utoipa::path(post, path = "/vm-imports/ova", tag = "vm-imports",
    params(OvaUploadQuery),
    request_body(content = ImageFile, content_type = "application/octet-stream"),
    responses((status = 201, body = Resource<VmImportSpec, VmImportStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn upload_ova(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<OvaUploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<VmImport>), ApiError> {
    let length = content_length(&headers)?;
    check_upload(&state, &principal, &q.name, length).await?;
    let file_name = q.file_name.unwrap_or_else(|| format!("{}.ova", q.name));
    let spec = VmImportSpec {
        node: state.node,
        source: ImportSource::Ova(OvaSource {
            file_name,
            size_bytes: length,
        }),
    };
    let mut import: VmImport = Resource::new(q.name, spec);
    let dir = import_dir(&state.upload_dir, import.id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(ApiError::internal)?;
    // 여기서부터 실패하거나 클라이언트가 끊어 이 future가 버려지면 디렉터리를 지운다.
    let mut guard = DirGuard {
        dir: dir.clone(),
        armed: true,
    };
    let source = dir.join(SOURCE_OVA);
    receive(&source, body, length).await?;
    let ova = tokio::task::spawn_blocking(move || parse_ova(&source))
        .await
        .map_err(ApiError::internal)?
        .map_err(ova_error)?;
    let (_, networks) = state.cluster.list::<NetworkSpec>()?;
    let networks: Vec<String> = networks.into_iter().map(|n| n.name).collect();
    let (proposal, proposal_warnings) = propose(&ova.vm, &networks);
    let mut warnings = ova.warnings;
    warnings.extend(proposal_warnings);
    import.status = VmImportStatus {
        phase: ImportPhase::Ready,
        source_vm: Some(ova.vm),
        proposal: Some(proposal),
        warnings,
        vm: None,
        error: None,
    };
    import.revision = state.cluster.create(&import).await?;
    guard.armed = false;
    Ok((StatusCode::CREATED, Json(import)))
}

/// 올리기를 끝내지 못한 가져오기 디렉터리를 지운다. 정상 경로에서는 `armed`를 내린다.
struct DirGuard {
    dir: PathBuf,
    armed: bool,
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let dir = self.dir.clone();
        tokio::spawn(async move {
            if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %e, "cannot remove a failed upload");
            }
        });
    }
}

/// 이전 opld가 남긴 가져오기 파일을 정리한다(노드가 시작할 때, API를 띄우기 전). 리소스가 없는
/// `imports/{id}` 디렉터리를 지우고, 이 노드의 `ready` 가져오기인데 원본이 없으면 `failed`로 바꾼다.
pub(crate) async fn sweep(cluster: &Cluster, node: Uuid, upload_dir: &FsPath) {
    let imports = match cluster.list::<VmImportSpec>() {
        Ok((_, imports)) => imports,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list vm imports");
            return;
        }
    };
    if let Ok(entries) = std::fs::read_dir(upload_dir.join("imports")) {
        for entry in entries.flatten() {
            let known = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::try_parse(name).ok())
                .is_some_and(|id| imports.iter().any(|i| i.id == id));
            if !known {
                let path = entry.path();
                tracing::warn!(path = %path.display(), "removing import files without an import");
                let removed = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if let Err(e) = removed {
                    tracing::warn!(path = %path.display(), error = %e, "cannot remove import files");
                }
            }
        }
    }
    for import in imports {
        if import.spec.node != node
            || import.status.phase != ImportPhase::Ready
            || import_dir(upload_dir, import.id).join(SOURCE_OVA).exists()
        {
            continue;
        }
        tracing::warn!(import = %import.name, "the source files of the import are gone");
        let status = VmImportStatus {
            phase: ImportPhase::Failed,
            error: Some("source files are gone".into()),
            ..import.status.clone()
        };
        if let Err(e) = cluster
            .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
            .await
        {
            tracing::warn!(import = %import.name, error = %e, "cannot mark the import failed");
        }
    }
}
```

`crates/opl-api/src/volumes.rs`에서

```rust
    }
    tokio::fs::create_dir_all(&state.upload_dir)
```

을 다음으로 바꾼다:

```rust
    }
    check_upload_space(state, size).await?;
    Ok((pool, format))
}

/// 업로드 디렉터리에 `size`바이트를 받고도 [`UPLOAD_RESERVE_BYTES`]가 남는지 본다. 아니면 507.
pub(crate) async fn check_upload_space(state: &AppState, size: u64) -> Result<(), ApiError> {
    tokio::fs::create_dir_all(&state.upload_dir)
```

`crates/opl-api/src/volumes.rs`에서

```rust
    }
    Ok((pool, format))
}
```

을 다음으로 바꾼다:

```rust
    }
    Ok(())
}
```

`crates/opl-api/src/volumes.rs`에서

```rust

fn content_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
```

을 다음으로 바꾼다:

```rust

pub(crate) fn content_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
```

`crates/opl-api/src/volumes.rs`에서

```rust
/// 본문을 `length`바이트까지 받는다. 그보다 길거나 짧으면 실패다.
async fn receive(path: &FsPath, body: Body, length: u64) -> Result<(), ApiError> {
    let mut file = tokio::fs::File::create_new(path)
```

을 다음으로 바꾼다:

```rust
/// 본문을 `length`바이트까지 받는다. 그보다 길거나 짧으면 실패다.
pub(crate) async fn receive(path: &FsPath, body: Body, length: u64) -> Result<(), ApiError> {
    let mut file = tokio::fs::File::create_new(path)
```

`crates/opl-api/src/watch.rs`에서

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 10] = [
    Kind::Vm,
```

을 다음으로 바꾼다:

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 11] = [
    Kind::Vm,
```

`crates/opl-api/src/watch.rs`에서

```rust
    Kind::BackupJob,
];
```

을 다음으로 바꾼다:

```rust
    Kind::BackupJob,
    Kind::VmImport,
];
```

`crates/opl-api/src/watch.rs`에서

```rust
    #[test]
    fn security_groups_are_watchable() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn vm_imports_are_watchable() {
        let kinds = parse_kinds("vm-import").unwrap();
        assert!(kinds.contains(&Kind::VmImport));
    }

    #[test]
    fn security_groups_are_watchable() {
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/backup-jobs/{key}/run",
        "/watch",
```

을 다음으로 바꾼다:

```rust
        "/backup-jobs/{key}/run",
        "/vm-imports",
        "/vm-imports/{key}",
        "/vm-imports/ova",
        "/vm-imports/ova/check",
        "/watch",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    assert!(doc["components"]["schemas"]["Resource_BackupJobSpec_BackupJobStatus"].is_object());
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
```

을 다음으로 바꾼다:

```rust
    assert!(doc["components"]["schemas"]["Resource_BackupJobSpec_BackupJobStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_VmImportSpec_VmImportStatus"].is_object());
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
```

`crates/opl-api/tests/vm_imports.rs`를 만든다:

```rust
//! VM 가져오기 API: OVA 올리기 사전 확인, 올리기(해석과 제안), 목록·읽기, 노드 시작 때의 정리.

mod common;

use std::path::PathBuf;

use common::ustar::Tar;
use common::{Client, Env};
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::vm_import::{
    ImportPhase, ImportSource, OvaSource, VmImport, VmImportSpec, VmImportStatus,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const VSPHERE: &str = include_str!("fixtures/vsphere.ovf");

/// vSphere 견본 OVF(두 번째 네트워크 이름을 `lan`으로 바꿈)와 그 파일들
fn vsphere_ova() -> Vec<u8> {
    let ovf = VSPHERE.replace("Backup Net", "lan");
    let disk = b"KDMV fake disk";
    Tar::new()
        .file("web01.ovf", ovf.as_bytes())
        .file("web01-disk1.vmdk", disk)
        .file("web01-disk2.vmdk", disk)
        .file("web01-disk3.vmdk", disk)
        .file("web01-file1.nvram", b"nvram")
        .finish()
}

async fn network(env: &Env, name: &str) {
    let network = Resource::new(
        name,
        NetworkSpec {
            bridge: Some("br0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    env.cluster.create(&network).await.unwrap();
}

fn imports_dir(env: &Env) -> PathBuf {
    env.dir.path().join("uploads").join("imports")
}

/// `imports` 아래의 디렉터리 이름
fn import_dirs(env: &Env) -> Vec<String> {
    match std::fs::read_dir(imports_dir(env)) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect(),
        Err(_) => vec![],
    }
}

async fn check(client: &Client, name: &str, size: u64) -> (StatusCode, Value) {
    client
        .post(
            &format!("/vm-imports/ova/check?name={name}&size={size}"),
            Value::Null,
        )
        .await
}

/// 저장소에 가져오기 리소스를 직접 만든다.
async fn import_on(env: &Env, name: &str, node: Uuid, phase: ImportPhase) -> VmImport {
    let mut import: VmImport = Resource::new(
        name,
        VmImportSpec {
            node,
            source: ImportSource::Ova(OvaSource {
                file_name: format!("{name}.ova"),
                size_bytes: 1,
            }),
        },
    );
    import.status = VmImportStatus {
        phase,
        ..Default::default()
    };
    env.cluster.create(&import).await.unwrap();
    import
}

#[tokio::test]
async fn uploads_an_ova_and_proposes_a_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    network(&env, "lan").await;
    let ova = vsphere_ova();

    let (status, body) = check(&admin, "web01", ova.len() as u64).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = admin
        .upload(
            "/vm-imports/ova?name=web01&file_name=web01.ova",
            ova.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "web01");
    assert_eq!(body["spec"]["node"], env.node.to_string());
    assert_eq!(
        body["spec"]["source"],
        json!({ "ova": { "file_name": "web01.ova", "size_bytes": ova.len() } })
    );
    let st = &body["status"];
    assert_eq!(st["phase"], "ready");
    assert_eq!(st["source_vm"]["name"], "web01");
    assert_eq!(st["source_vm"]["disks"].as_array().unwrap().len(), 3);
    assert_eq!(
        st["proposal"],
        json!({
            "sockets": 2, "cores": 2, "memory_mib": 8192, "firmware": "uefi",
            "disk_bus": "virtio-blk", "nic_model": "virtio",
            // 이름이 같은 네트워크만 매핑한다.
            "networks": { "VM Network": null, "lan": "lan" },
        })
    );
    // 해석 경고 뒤에 제안 경고
    let warnings: Vec<&str> = st["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(warnings[0], "CD-ROM drive is not imported");
    assert!(warnings.iter().any(|w| w.starts_with("UEFI boot entries")));
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("power off the VMware VM"))
    );
    assert!(st.get("error").is_none() && st.get("vm").is_none(), "{st}");

    // 원본은 `imports/{id}/source.ova`에 남는다.
    let id = body["id"].as_str().unwrap();
    let source = imports_dir(&env).join(id).join("source.ova");
    assert_eq!(std::fs::read(&source).unwrap(), ova);

    let (status, list) = admin.get("/vm-imports").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["id"], id);
    let (status, got) = admin.get("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"], body["status"]);
    let (status, got) = admin.get(&format!("/vm-imports/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{got}");

    // 파일 이름이 없으면 `{이름}.ova`
    let (status, body) = admin.upload("/vm-imports/ova?name=web02", ova).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["source"]["ova"]["file_name"], "web02.ova");
}

#[tokio::test]
async fn invalid_ova_is_rejected_and_removed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // OVF가 가리키는 디스크가 tar에 없다.
    let ova = Tar::new()
        .file("web01.ovf", VSPHERE.as_bytes())
        .file("web01-disk1.vmdk", b"disk")
        .finish();
    let (status, body) = admin.upload("/vm-imports/ova?name=web01", ova).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_ova");
    assert_eq!(
        body["message"],
        "file \"web01-disk2.vmdk\" is not in the OVA"
    );

    let (status, body) = admin
        .upload("/vm-imports/ova?name=web01", b"not a tar".repeat(100))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_ova");

    let (_, list) = admin.get("/vm-imports").await;
    assert_eq!(list["items"], json!([]));
    // 지우기는 응답 뒤에 끝날 수 있다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while !import_dirs(&env).is_empty() {
        assert!(std::time::Instant::now() < deadline, "upload left behind");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn check_and_upload_reject_with_the_same_codes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    import_on(&env, "taken", env.node, ImportPhase::Ready).await;

    for (name, size, want, code) in [
        (
            "Bad_Name",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        // `/vm-imports/ova` 경로와 겹친다.
        ("ova", 1 << 20, StatusCode::BAD_REQUEST, "invalid_request"),
        ("taken", 1 << 20, StatusCode::CONFLICT, "name_taken"),
        (
            "web01",
            u64::MAX,
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
        ),
    ] {
        let (status, body) = check(&admin, name, size).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (want, Some(code)),
            "{name} {body}"
        );
    }

    // 올리기도 같은 확인을 거친다. 받기 전에 거절하므로 남는 파일이 없다.
    let (status, body) = admin
        .upload("/vm-imports/ova?name=taken", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    assert_eq!(body["message"], "vm-import \"taken\" already exists");
    assert!(import_dirs(&env).is_empty());

    // 크기가 없으면 확인할 수 없다.
    let (status, body) = admin
        .post("/vm-imports/ova/check?name=web01", Value::Null)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn operators_upload_and_viewers_only_read() {
    let env = Env::start().await;
    let operator = env.user("op", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = check(&viewer, "web01", 1 << 20).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = viewer
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = check(&operator, "web01", 1 << 20).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = operator
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, list) = viewer.get("/vm-imports").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (status, body) = viewer.get("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = viewer.get("/vm-imports/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn witness_nodes_do_not_take_uploads() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let spec = NodeSpec {
        witness: true,
        ..me.spec.clone()
    };
    env.cluster
        .update_spec(env.node, None, me.labels.clone(), &spec)
        .await
        .unwrap();

    let (status, body) = check(&admin, "web01", 1 << 20).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("witness"),
        "{body}"
    );
    let (status, body) = admin
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(import_dirs(&env).is_empty());
}

/// 노드가 시작할 때: 리소스가 없는 디렉터리는 지우고, 원본이 없는 이 노드의 `ready` 가져오기는
/// `failed`로 바꾼다. 다른 노드의 가져오기와 원본이 있는 가져오기는 그대로 둔다.
#[tokio::test]
async fn startup_sweeps_import_directories() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let kept = import_on(&env, "kept", env.node, ImportPhase::Ready).await;
    let gone = import_on(&env, "gone", env.node, ImportPhase::Ready).await;
    let done = import_on(&env, "done", env.node, ImportPhase::Done).await;
    let other = import_on(&env, "other", Uuid::now_v7(), ImportPhase::Ready).await;
    let kept_dir = imports_dir(&env).join(kept.id.to_string());
    std::fs::create_dir_all(&kept_dir).unwrap();
    std::fs::write(kept_dir.join("source.ova"), b"ova").unwrap();
    let orphan = imports_dir(&env).join(Uuid::now_v7().to_string());
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("source.ova"), b"ova").unwrap();
    // 원본 없는 디렉터리(올리기 도중 죽었다)도 리소스가 있으면 남는다.
    let empty = imports_dir(&env).join(gone.id.to_string());
    std::fs::create_dir_all(&empty).unwrap();
    // 볼륨 업로드 정리는 그대로 한다.
    let stray = env.dir.path().join("uploads").join("stray.upload");
    std::fs::write(&stray, b"x").unwrap();

    env.start_runner().await;

    assert!(!orphan.exists());
    assert!(kept_dir.join("source.ova").exists());
    assert!(empty.exists());
    assert!(!stray.exists());
    let phase = |id: Uuid| {
        let import = env.cluster.get::<VmImportSpec>(id).unwrap().unwrap();
        (import.status.phase, import.status.error)
    };
    assert_eq!(phase(kept.id), (ImportPhase::Ready, None));
    assert_eq!(
        phase(gone.id),
        (ImportPhase::Failed, Some("source files are gone".into()))
    );
    assert_eq!(phase(done.id), (ImportPhase::Done, None));
    assert_eq!(phase(other.id), (ImportPhase::Ready, None));
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1056개, 실패 0

Run: `cargo test -p opl-api --test vm_imports --test openapi`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 382개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(opl-api): VM 가져오기 올리기·목록 API — /vm-imports/ova 사전 확인과 올리기(받아서 OVA 해석·제안 → ready 가져오기, 실패·끊김이면 디렉터리 지움), 목록·읽기, 원본은 업로드 디렉터리 imports/{id}/source.ova, 노드 시작 때 리소스 없는 디렉터리 지움·원본 없는 ready는 failed, watch 종류 vm-import, operator 쓰기 권한"
```

---

### Task 4: 실행·지우기와 Task

`POST /vm-imports/{key}/run`(본문 `ImportRun`; 원본 네트워크마다 매핑 — 없으면 제안의 같은 이름 네트워크, VM·볼륨 이름, 풀이 원본 노드에서 쓸 수 있는지, MAC 겹침, SATA 6개, 여유를 보고 `importing` + Task `VmImport`, 대상은 새 VM id) → 202. `DELETE /vm-imports/{key}`(Task `VmImportDelete`, `?force=true`면 원본 노드가 없거나 오프라인일 때 리소스만). Task(가져오기 줄, 시간 제한 없음): MAC 다시 확인 → 디스크마다 여유 확인·해시 확인 추출·`check_import_source`·볼륨으로 가져오기 → 멈춘 VM → `done`·원본 지움. 실패하면 볼륨·VM을 지우고 `ready` + error(대상 VM이 이미 있으면 `done`). 끝나지 않은 Task가 없는 `importing`은 쉬는 것으로 본다. 노드 시작 때 Task 없는 `importing`을 되돌린다. CI storage-it에 `vm_import_tasks`.

**Files:**
- Create: `crates/opl-api/src/tasks/import.rs`
- Create: `crates/opl-api/tests/vm_import_tasks.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opl-api/src/backups.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/backup.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/src/vm_imports.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/vm_imports.rs`
- Modify: `crates/opl-types/src/task.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
          cargo test -p opl-api --features storage-it --test volumes
          cargo test -p opl-api --features storage-it --test backup_tasks --test backup_job_runs
```

을 다음으로 바꾼다:

```yaml
          cargo test -p opl-api --features storage-it --test volumes
          cargo test -p opl-api --features storage-it --test backup_tasks --test backup_job_runs --test vm_import_tasks
```

`crates/opl-api/src/backups.rs`에서

```rust
/// 노드 `id`가 온라인인지 확인한다. 아니면 409 `node_offline`.
fn check_online(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    match state.cluster.get::<NodeSpec>(id)? {
```

을 다음으로 바꾼다:

```rust
/// 노드 `id`가 온라인인지 확인한다. 아니면 409 `node_offline`.
pub(crate) fn check_online(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    match state.cluster.get::<NodeSpec>(id)? {
```

`crates/opl-api/src/backups.rs`에서

```rust
/// 노드 `id`의 이름(없으면 id)
fn node_name(state: &AppState, id: Uuid) -> Result<String, ApiError> {
    Ok(state
```

을 다음으로 바꾼다:

```rust
/// 노드 `id`의 이름(없으면 id)
pub(crate) fn node_name(state: &AppState, id: Uuid) -> Result<String, ApiError> {
    Ok(state
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(r::list_vm_imports))
        .routes(routes!(r::get_vm_import))
        .routes(routes!(vm_imports::upload_ova))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(r::list_vm_imports))
        .routes(routes!(r::get_vm_import, vm_imports::delete_import))
        .routes(routes!(vm_imports::run_import))
        .routes(routes!(vm_imports::upload_ova))
```

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

mod backup;
mod clone;
mod import;
mod migrate;
mod snapshot;

pub(crate) use clone::volume_name;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_peer::Peers;
use opl_reconciler::{FirmwareFetcher, ReconcilerHandle};
use opl_types::backup::{BackupPhase, BackupSpec};
use opl_types::resource::{Kind, Resource};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
    is_shared, pool_listed_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::vms::is_stopped;
use crate::volumes::{block_pool_kind, usable_node};

/// 새 Task를 저장소에 만든다. 이름은 id 문자열이다.
pub(crate) async fn create_task(
    cluster: &Cluster,
    op: TaskOp,
    target: Uuid,
    node: Uuid,
) -> Result<Task, ApiError> {
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    task.revision = cluster.create(&task).await?;
    Ok(task)
}

#[derive(Clone)]
pub struct TaskRunnerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 Task만 실행한다.
    pub node: Uuid,
    /// Task 하나가 이보다 오래 걸리면 실패로 끝낸다.
    pub timeout: Duration,
    /// 저장소 쓰기가 실패했을 때 다시 시도하는 간격, VM 상태를 다시 확인하는 간격
    pub poll_interval: Duration,
    /// API가 올린 이미지를 받아 두는 디렉터리. 시작할 때 Task가 없는 파일을 지운다.
    pub upload_dir: Option<PathBuf>,
    /// VM별 상태 디렉터리의 뿌리(`QemuConfig::state_dir`). 스냅샷의 UEFI 변수 사본을 둔다.
    /// 없으면 변수를 복사하거나 되돌리지 않는다.
    pub state_dir: Option<PathBuf>,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 백업 Task가 백업 풀의 파일 경로를 정한다.
    pub mount_root: PathBuf,
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨, 시간을 넘긴 블록 스냅샷이 남긴 태그를
    /// 다시 지워 보는 간격
    pub orphan_retry: Duration,
    /// 블록 스냅샷이 남긴 태그, 이 노드에서 켜진 채 남은 볼륨을 다시 찾아 정리하는 간격(시작할 때
    /// 한 번 찾고 그 뒤로). 확정되지 않았거나 확정 뒤 남은 마이그레이션 QEMU도 이 간격으로 정리한다.
    pub tag_sweep: Duration,
    /// 라이브 마이그레이션을 보낼 때 대상 노드를 부르는 노드 간 클라이언트. 없으면 마이그레이션 Task는
    /// 실패한다.
    pub peers: Option<Arc<Peers>>,
    /// 마이그레이션을 보내는 대역폭 상한(바이트/초). `None`이면 QEMU 기본값
    pub migration_bandwidth: Option<u64>,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다. 마이그레이션 Task는 `timeout` 대신 이것을 쓴다.
    pub migration_timeout: Duration,
    /// 스냅샷·복제가 UEFI 변수를 복사하기 전에, 변수 파일이 다른 노드에 있으면 받아 온다. 없으면
    /// 이 노드의 파일을 그대로 쓴다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
            mount_root: PathBuf::new(),
            orphan_retry: Duration::from_secs(60),
            tag_sweep: Duration::from_secs(30 * 60),
            peers: None,
            migration_bandwidth: None,
            migration_timeout: Duration::from_secs(30 * 60),
            firmware: None,
        }
    }
}

pub struct TaskRunner {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Ctx {
    cluster: Arc<Cluster>,
    storage: Arc<dyn Storage>,
    hv: Arc<dyn Hypervisor>,
    reconciler: ReconcilerHandle,
    config: TaskRunnerConfig,
    /// 실행 중인 복제 Task가 만든 것(Task id별). 실패하면 정리가 지운다.
    cloned: std::sync::Mutex<HashMap<Uuid, clone::Made>>,
    /// 실행기가 멈췄는지. 뒤에서 도는 일(남은 태그 지우기)이 보고 함께 멈춘다.
    stopped: watch::Sender<bool>,
}

impl TaskRunner {
    /// 고아(Task 없는 Pending 볼륨·스냅샷, 올린 파일)를 지운 뒤 실행 루프를 띄우고 돌아온다.
    /// API가 막 만든 볼륨이나 올린 파일을 고아로 지우지 않도록 API 서버는 이것을 기다린 뒤 띄운다.
    /// `hv`는 실행 중인 VM의 스냅샷을 다룬다(이 노드의 reconciler와 같은 하이퍼바이저).
    pub async fn spawn(
        cluster: Arc<Cluster>,
        storage: Arc<dyn Storage>,
        hv: Arc<dyn Hypervisor>,
        reconciler: ReconcilerHandle,
        config: TaskRunnerConfig,
    ) -> Self {
        let ctx = Arc::new(Ctx {
            cluster,
            storage,
            hv,
            reconciler,
            config,
            cloned: Default::default(),
            stopped: watch::channel(false).0,
        });
        let (revision, tasks) = loop {
            match ctx.cluster.list::<TaskSpec>() {
                Ok(list) => break list,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot list tasks; will retry");
                    tokio::time::sleep(ctx.config.poll_interval).await;
                }
            }
        };
        let unmounted = sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx, unmounted)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// `unmounted`는 풀이 마운트되지 않아 지우지 못한 고아 볼륨이다. 따로 다시 지워 본다.
async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>, unmounted: Vec<Uuid>) {
    let retry = tokio::spawn(retry_orphans(ctx.clone(), unmounted));
    let tags = tokio::spawn(sweep_block_tags_every(ctx.clone()));
    let idle = tokio::spawn(deactivate_idle_volumes_every(ctx.clone()));
    let migrations = tokio::spawn(migrate::sweep_every(ctx.clone()));
    // 실행을 시작한 Task. 목록을 다시 읽을 때 같은 Task를 두 번 시작하지 않게 한다.
    let mut started: HashSet<Uuid> = HashSet::new();
    // 실행 중인 Task. 실행기가 멈출 때 함께 멈춘다.
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 줄(볼륨, VM)별 마지막 Task. 같은 줄의 Task는 앞의 것이 끝난 뒤에 시작한다(`queue`).
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
    let mut first = true;
    'sync: loop {
        let (revision, tasks) = match ctx.cluster.list::<TaskSpec>() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list tasks; will retry");
                tokio::select! {
                    _ = &mut shutdown => break,
                    () = tokio::time::sleep(ctx.config.poll_interval) => continue,
                }
            }
        };
        running.retain(|_, h| !h.is_finished());
        by_target.retain(|_, h| !h.is_finished());
        started.retain(|id| {
            tasks
                .iter()
                .any(|t| t.id == *id && !t.status.state.is_finished())
        });
        for task in tasks {
            if task.spec.node != ctx.config.node || started.contains(&task.id) {
                continue;
            }
            match task.status.state {
                TaskState::Pending => start(
                    &ctx,
                    &mut started,
                    &mut running,
                    &mut by_target,
                    task,
                    false,
                ),
                // 이전 opld가 실행하다 내려갔다.
                TaskState::Running if first => {
                    start(&ctx, &mut started, &mut running, &mut by_target, task, true)
                }
                _ => {}
            }
        }
        first = false;
        let mut watch = match ctx.cluster.watch(revision) {
            Ok(w) => w,
            Err(_) => continue,
        };
        loop {
            tokio::select! {
                _ = &mut shutdown => break 'sync,
                ev = watch.next() => match ev {
                    Ok(WatchEvent::Put { kind: Kind::Task, resource }) => {
                        let Ok(task) = serde_json::to_value(resource).and_then(serde_json::from_value::<Task>)
                        else {
                            continue;
                        };
                        if task.spec.node == ctx.config.node
                            && task.status.state == TaskState::Pending
                            && !started.contains(&task.id)
                        {
                            start(&ctx, &mut started, &mut running, &mut by_target, task, false);
                        }
                    }
                    Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                    Ok(_) => {}
                },
            }
        }
    }
    for handle in running.values() {
        handle.abort();
    }
    retry.abort();
    tags.abort();
    idle.abort();
    migrations.abort();
    ctx.stopped.send_replace(true);
}

/// 공유 풀에서 남은 태그로 보고 지우려면 스냅샷이 만들어진 뒤 지나야 하는 시간. 다른 노드의 스냅샷
/// Task가 막 찍은 태그를 그 리소스보다 먼저 보고 지우지 않게 한다.
const SHARED_TAG_MIN_AGE: Duration = Duration::from_secs(30 * 60);

/// 남은 블록 스냅샷 태그를 시작할 때와 그 뒤 `tag_sweep`마다 지운다([`sweep_block_tags`]). 실행기가
/// 멈추면 함께 멈춘다.
async fn sweep_block_tags_every(ctx: Arc<Ctx>) {
    loop {
        sweep_block_tags(ctx.clone()).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 시간을 넘긴 블록 스냅샷이 남긴 태그를 지운다: 이 노드가 맡는 블록 풀(LVM-thin, ZFS, RBD) 볼륨의
/// 태그 중 UUID 모양인데 그 id의 스냅샷 리소스가 없는 것. 스냅샷 Task는 리소스를 만든 뒤 태그를
/// 찍고 태그를 지운 뒤 리소스를 지우므로 그런 태그는 남은 것이다. 백업의 임시 스냅샷(백업 id의 simple
/// 문자열)도 같은 모양이므로, 백업 Task가 끝나지 않은 `pending` 백업의 태그는 둔다. opLite 밖에서 만든 태그는 두고,
/// 스토리지 명령이 느려도 실행기 시작을 막지 않게 뒤에서 돈다.
///
/// 노드 로컬 풀의 볼륨은 그 볼륨을 만든 노드가 맡는다. 공유 풀(RBD)의 볼륨은 어느 노드에서나
/// 보이므로 노드마다 같은 답이 나오게 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드가
/// 맡는다(그런 노드가 없으면 만든 노드). 공유 풀에서는 다른 노드의 Task가 막 찍은 태그일 수 있으므로
/// 만든 지 [`SHARED_TAG_MIN_AGE`]가 지난 태그만 지운다.
async fn sweep_block_tags(ctx: Arc<Ctx>) {
    let volumes = match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => volumes,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list volumes");
            return;
        }
    };
    for volume in volumes {
        if volume.status.phase != VolumePhase::Ready {
            continue;
        }
        let Ok(Some(pool)) = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        else {
            continue;
        };
        if !uses_block_snapshots(&pool) {
            continue;
        }
        let shared = is_shared(&pool);
        let owner = if shared {
            usable_node(&ctx.cluster, &pool, None)
                .ok()
                .flatten()
                .or(volume.status.node)
        } else {
            volume.status.node
        };
        if owner.is_some_and(|n| n != ctx.config.node) {
            continue;
        }
        let format = volume.spec.format;
        let tags = if shared {
            ctx.storage
                .snapshot_tags_older_than(&pool, volume.id, format, SHARED_TAG_MIN_AGE)
                .await
        } else {
            ctx.storage.snapshot_tags(&pool, volume.id, format).await
        };
        let tags = match tags {
            Ok(tags) => tags,
            Err(e) => {
                tracing::debug!(volume = %volume.name, error = %e, "cannot read block snapshots");
                continue;
            }
        };
        for tag in tags {
            let Ok(id) = tag.parse::<Uuid>() else {
                continue;
            };
            if !matches!(ctx.cluster.get::<SnapshotSpec>(id), Ok(None)) {
                continue;
            }
            match ctx.cluster.get::<BackupSpec>(id) {
                Ok(Some(b)) if b.status.phase != BackupPhase::Pending => {}
                // 채울 Task가 없는 pending 백업(Task가 끝났거나 지워졌다)의 태그도 남은 것이다.
                Ok(Some(_)) if !backing_up(&ctx, id) => {}
                Ok(None) => {}
                _ => continue,
            }
            tracing::warn!(volume = %volume.name, tag, "removing a block snapshot left without its snapshot");
            if let Err(e) = ctx
                .storage
                .snapshot_delete(&pool, volume.id, format, &tag)
                .await
            {
                tracing::warn!(volume = %volume.name, tag, error = %e, "cannot remove block snapshot");
            }
        }
    }
}

/// 백업 `id`를 채우는 백업 Task가 끝나지 않았는지. 목록을 읽지 못하면 그렇다고 본다.
fn backing_up(ctx: &Ctx, id: Uuid) -> bool {
    let Ok((_, tasks)) = ctx.cluster.list::<TaskSpec>() else {
        return true;
    };
    tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VmBackup { backup } if backup == id)
    })
}

/// 이 노드에서 켜진 채 남은 볼륨을 시작할 때와 그 뒤 `tag_sweep`마다 끈다
/// ([`deactivate_idle_volumes`]). 실행기가 멈추면 함께 멈춘다.
async fn deactivate_idle_volumes_every(ctx: Arc<Ctx>) {
    loop {
        deactivate_idle_volumes(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 이 노드에서 켜진 채 남은 볼륨(공유 LVM의 배타 활성화)을 끈다. VM이 멈춘 뒤 reconciler가 끄지
/// 못했거나 opld가 그 전에 내려갔을 때 남는다. 이 노드에 놓여 실행 중이거나 켜려는 VM(하이퍼바이저에서
/// 도는 VM 포함)의 디스크, 이 노드에서 끝나지 않은 Task의 볼륨(VM Task면 그 VM의 디스크), 아직
/// 만드는 중인(pending) 볼륨은 그대로 둔다. 리소스가 없는 볼륨은 끈다.
///
/// 켜진 볼륨을 먼저 읽고 그 뒤에 VM·Task를 읽는다: 볼륨을 켜는 VM·Task는 켜기 전에 이미 저장소에
/// 있으므로 쓰는 중인 볼륨을 끄지 않는다(그 사이 켠 볼륨은 이번에 보지 않는다). 이미 켜져 있던 볼륨을
/// 그 사이 시작한 VM이 그대로 쓸 수도 있으므로 VM·Task는 볼륨마다 끄기 직전에 다시 읽는다.
async fn deactivate_idle_volumes(ctx: &Ctx) {
    let me = ctx.config.node;
    let pools = match ctx.cluster.list::<StoragePoolSpec>() {
        Ok((_, pools)) => pools,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list storage pools");
            return;
        }
    };
    let mut active = Vec::new();
    for pool in pools.into_iter().filter(|p| pool_listed_on(p, me)) {
        match ctx.storage.active_volumes(&pool).await {
            Ok(ids) if !ids.is_empty() => active.push((pool, ids)),
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(pool = %pool.name, error = %e, "cannot list active volumes")
            }
        }
    }
    for (pool, ids) in active {
        for id in ids {
            let (in_use, formats) = match volumes_in_use(ctx).await {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read which volumes are in use");
                    return;
                }
            };
            if in_use.contains(&id) {
                continue;
            }
            let format = formats.get(&id).copied().unwrap_or(ImageFormat::Raw);
            tracing::info!(pool = %pool.name, volume = %id, "deactivating a volume left active");
            if let Err(e) = ctx.storage.deactivate_volume(&pool, id, format).await {
                tracing::warn!(pool = %pool.name, volume = %id, error = %e, "cannot deactivate volume");
            }
        }
    }
}

/// 이 노드가 쓰는 볼륨([`deactivate_idle_volumes`]이 그대로 두는 볼륨)과 볼륨별 형식. 이 노드로
/// 마이그레이션하는 중인 VM의 디스크도 쓰는 중이다(받는 QEMU를 띄우기 전에 켠다). 이 노드에서 보내는
/// 중인 VM은 원하는 상태가 running이고 마이그레이션 Task가 있다.
async fn volumes_in_use(ctx: &Ctx) -> Result<(HashSet<Uuid>, HashMap<Uuid, ImageFormat>), String> {
    let me = ctx.config.node;
    let running = ctx
        .hv
        .list()
        .await
        .map_err(|e| format!("cannot list running VMs: {e}"))?;
    let (volumes, tasks, vms) = ctx
        .cluster
        .list::<VolumeSpec>()
        .and_then(|(_, volumes)| {
            Ok((
                volumes,
                ctx.cluster.list::<TaskSpec>()?.1,
                ctx.cluster.list::<VmSpec>()?.1,
            ))
        })
        .map_err(|e| format!("cannot list volumes, tasks or VMs: {e}"))?;
    let mut in_use: HashSet<Uuid> = volumes
        .iter()
        .filter(|v| v.status.phase != VolumePhase::Ready)
        .map(|v| v.id)
        .collect();
    let mut task_vms = HashSet::new();
    for task in tasks
        .iter()
        .filter(|t| t.spec.node == me && !t.status.state.is_finished())
    {
        in_use.insert(task.spec.target);
        task_vms.insert(task.spec.target);
    }
    let by_name: HashMap<&str, Uuid> = volumes.iter().map(|v| (v.name.as_str(), v.id)).collect();
    for vm in &vms {
        let used = task_vms.contains(&vm.id)
            || running.iter().any(|r| r.id == vm.id)
            || (vm.spec.node == Some(me) && vm.spec.power == PowerState::Running)
            || vm.status.migration.is_some_and(|m| m.node == me);
        if used {
            in_use.extend(
                vm.spec
                    .disks
                    .iter()
                    .filter_map(|d| by_name.get(d.volume.as_str())),
            );
        }
    }
    let formats = volumes.iter().map(|v| (v.id, v.spec.format)).collect();
    Ok((in_use, formats))
}

/// 풀이 마운트되지 않았거나 응답하지 않아 지우지 못한 고아 볼륨을 `orphan_retry`마다 다시 지워 본다. 지웠거나 볼륨이
/// 없어졌으면 목록에서 뺀다. 마운트 말고 다른 이유로 실패하면 시작할 때처럼 기록만 하고 뺀다.
async fn retry_orphans(ctx: Arc<Ctx>, mut volumes: Vec<Uuid>) {
    while !volumes.is_empty() {
        tokio::time::sleep(ctx.config.orphan_retry).await;
        let mut left = Vec::new();
        for id in volumes {
            match remove_volume(&ctx, id).await {
                Ok(()) => tracing::info!(volume = %id, "removed a volume left unfinished"),
                Err(RemoveError::Storage(
                    DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                )) => left.push(id),
                Err(e) => tracing::warn!(volume = %id, error = %e, "cannot remove volume"),
            }
        }
        volumes = left;
    }
}

fn is_volume_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. } | TaskOp::VolumeDelete
    )
}

fn is_snapshot_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::SnapshotDelete { .. }
    )
}

/// Task가 설 줄. 볼륨 Task는 그 볼륨의 줄에 선다(`Storage`의 요구). 스냅샷 Task는 VM의 줄에
/// 선다(부모·현재 위치가 차례로 바뀐다). 복제 Task도 원본 VM의 줄에 선다. 복사하는 동안 그 스냅샷을
/// 지우거나 디스크를 되돌리면 안 된다. 볼륨 크기 늘리기는 그 볼륨을 쓰는 VM의 줄에 선다. 스냅샷
/// 만들기·되돌리기가 같은 디스크를 다루고 되돌리기는 크기도 되돌리므로 섞이면 안 된다. 지금 쓰는
/// VM이 없으면 그 볼륨을 담은 스냅샷의 VM(되돌리면 다시 붙는다), 그것도 없으면 볼륨의 줄이다.
/// VM 전원 Task는 기다리기만 하므로 줄을 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
/// 마이그레이션 Task는 VM의 줄에 선다(앞의 스냅샷·복제가 끝난 뒤 옮긴다). 백업 Task도 VM의 줄에
/// 선다(임시 스냅샷을 찍고 디스크를 읽는다). 백업 지우기와 복원은 그 백업의 줄에 서서 겹치지 않는다
/// (복원 Task의 대상은 새 VM이다). VM 가져오기와 가져오기 지우기도 그 가져오기의 줄에 선다(가져오기
/// Task의 대상은 새 VM이다).
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    match task.spec.op {
        TaskOp::BackupRestore { backup, .. } => return Some(backup),
        TaskOp::VmImport { import, .. } => return Some(import),
        _ => {}
    }
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_queue = matches!(
            task.spec.op,
            TaskOp::VmClone { .. } | TaskOp::VmMigrate { .. } | TaskOp::VmBackup { .. }
        );
        let backup_queue = matches!(task.spec.op, TaskOp::BackupDelete | TaskOp::VmImportDelete);
        return (is_volume_op(&task.spec.op)
            || is_snapshot_op(&task.spec.op)
            || vm_queue
            || backup_queue)
            .then_some(target);
    }
    let Ok(Some(volume)) = ctx.cluster.get::<VolumeSpec>(target) else {
        return Some(target);
    };
    let uses = |spec: &VmSpec| spec.disks.iter().any(|d| d.volume == volume.name);
    if let Ok((_, vms)) = ctx.cluster.list::<VmSpec>()
        && let Some(vm) = vms.iter().find(|vm| uses(&vm.spec))
    {
        return Some(vm.id);
    }
    if let Ok((_, snapshots)) = ctx.cluster.list::<SnapshotSpec>()
        && let Some(s) = snapshots.iter().find(|s| uses(&s.spec.vm_spec))
    {
        return Some(s.spec.vm);
    }
    Some(target)
}

/// Task를 실행한다. 줄(`queue`)이 있으면 그 줄의 앞 Task가 끝난 뒤에 시작한다.
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다.
fn start(
    ctx: &Arc<Ctx>,
    started: &mut HashSet<Uuid>,
    running: &mut HashMap<Uuid, AbortHandle>,
    by_target: &mut HashMap<Uuid, JoinHandle<()>>,
    task: Task,
    interrupted: bool,
) {
    started.insert(task.id);
    let id = task.id;
    let queue = queue(ctx, &task);
    let previous = queue.and_then(|q| by_target.remove(&q));
    let ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        if interrupted {
            let error = "interrupted: opld restarted while the task was running".to_string();
            fail(&ctx, &task, error, true).await;
        } else {
            execute(&ctx, task).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
}

async fn execute(ctx: &Ctx, task: Task) {
    if !begin(ctx, task.id).await {
        return;
    }
    // 시간을 넘겨 버린 작업은 끝나지 않았다(명령이 아직 돌 수 있다). 마이그레이션은 단계마다
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    // 백업과 복원, VM 가져오기는 디스크 크기만큼 오래 걸릴 수 있으므로 시간 제한을 두지 않는다(설계 §2.3).
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. }
            | TaskOp::VmBackup { .. }
            | TaskOp::BackupRestore { .. }
            | TaskOp::VmImport { .. }
    );
    let (result, unfinished) = if unlimited {
        (perform(ctx, &task).await, false)
    } else {
        match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
            Ok(r) => (r, false),
            Err(_) => (
                Err(format!("timed out after {:?}", ctx.config.timeout)),
                true,
            ),
        }
    };
    match result {
        Ok(()) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, None).await
        }
        Err(e) => fail(ctx, &task, e, unfinished).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다. `unfinished`면 작업이 끝나지 않았다(시간 초과, 재시작으로 중단).
async fn fail(ctx: &Ctx, task: &Task, error: String, unfinished: bool) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task, &error, unfinished).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제·복원·VM 가져오기의 흔적을 지워 이름을 다시 쓸 수 있게
/// 한다. 실패한 백업은 디렉터리를 지우고 `failed`와 `error`를 남긴다. 실패한 VM 가져오기는 `ready`로
/// 돌아가 `error`를 남긴다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
/// 블록 디스크의 태그는 뒤에서 다시 지운다([`snapshot::remove_unfinished_tags`]).
async fn cleanup(ctx: &Ctx, task: &Task, error: &str, unfinished: bool) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op {
        if unfinished {
            snapshot::remove_unfinished_tags(ctx, snapshot);
        }
        if let Err(e) = snapshot::discard_pending(ctx, snapshot).await {
            tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
        }
    }
    if let TaskOp::VolumeImport { source, .. } = &task.spec.op {
        let _ = tokio::fs::remove_file(source).await;
    }
    if let TaskOp::VmClone { .. } | TaskOp::BackupRestore { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
    }
    if let TaskOp::VmBackup { backup } = task.spec.op {
        backup::discard(ctx, task, backup, error, unfinished).await;
    }
    if let TaskOp::VmImport { import, .. } = task.spec.op {
        import::discard(ctx, task, import, error).await;
    }
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. }
    ) && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// `pending`을 `running`으로 읽은 revision에서만 바꾼다. 그사이 다른 쪽(리더의 정리 작업)이
/// 실패로 끝냈거나 Task가 지워졌으면 `false`로, 실행하지 않는다. `running`으로 읽히면 응답을 잃은
/// 앞의 쓰기가 적용된 것이다(`running`은 이 노드만 쓴다). 기록될 때까지 다시 시도한다.
async fn begin(ctx: &Ctx, id: Uuid) -> bool {
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
    };
    loop {
        let current = match ctx.cluster.get::<TaskSpec>(id) {
            Ok(Some(task)) => task,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot read task; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        match current.status.state {
            TaskState::Pending => {}
            TaskState::Running => return true,
            TaskState::Succeeded | TaskState::Failed => return false,
        }
        match ctx
            .cluster
            .update_status_checked::<TaskSpec>(id, current.revision, &running)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return false,
            // 바뀐 Task가 이 노드에 적용되면 다시 읽어 판단한다.
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task start; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus { state, error };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
            // 정리 작업이 지운 Task는 끝난 것이다(`begin`과 같다).
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task status; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

async fn perform(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
        TaskOp::VmStop => wait_vm(ctx, id, stopped).await,
        TaskOp::VmForceStop => {
            ctx.reconciler.force_stop(id);
            wait_vm(ctx, id, stopped).await
        }
        TaskOp::VmReboot => reboot(ctx, task).await,
        TaskOp::VolumeCreate => create_volume(ctx, id).await,
        TaskOp::VolumeImport {
            source,
            source_format,
        } => import_volume(ctx, id, source, *source_format).await,
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
        TaskOp::SnapshotCreate { snapshot } => snapshot::create(ctx, id, *snapshot).await,
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot, .. } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        TaskOp::VmClone {
            name,
            snapshot,
            pool,
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
        TaskOp::VmMigrate { node } => migrate::migrate(ctx, task, *node).await,
        // 기록 Task는 끝난 상태로 만들어지므로 실행되지 않는다.
        TaskOp::VmHaRecover { .. } => Err("an HA recovery task is a record and is not run".into()),
        TaskOp::VmBackup { backup } => backup::backup(ctx, task, *backup).await,
        TaskOp::BackupDelete => backup::delete(ctx, id).await,
        TaskOp::BackupRestore { backup, name, pool } => {
            backup::restore(ctx, task, *backup, name, pool).await
        }
        TaskOp::VmImport { import, run } => import::import(ctx, task, *import, run).await,
        TaskOp::VmImportDelete => import::delete(ctx, id).await,
    }
}

/// VM을 켰다: 원하는 상태와 실제 상태가 모두 running. reconciler가 포기해 원하는 상태를
/// stopped로 바꿨으면 실패다. 이전 실행의 Failed status는 원하는 상태가 running인 동안 무시한다.
fn started(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, VmPhase::Running) => Some(Ok(())),
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Err(vm
            .status
            .last_error
            .clone()
            .unwrap_or_else(|| "VM was stopped".into()))),
        _ => None,
    }
}

fn stopped(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        (PowerState::Running, _) => Some(Err("VM was started again".into())),
        _ => None,
    }
}

/// VM이 조건을 만족할 때까지 로컬 저장소를 다시 읽는다.
async fn wait_vm(
    ctx: &Ctx,
    id: Uuid,
    done: impl Fn(&Vm) -> Option<Result<(), String>>,
) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if let Some(result) = done(&vm) {
            return result;
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

/// 원하는 상태를 바꾼다. reconciler의 status 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_power(ctx: &Ctx, id: Uuid, power: PowerState) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if vm.spec.power == power {
            return Ok(());
        }
        let mut spec = vm.spec.clone();
        spec.power = power;
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// ACPI로 끈 뒤 다시 켠다. 게스트가 ACPI를 무시하면 reconciler가 `acpi_timeout` 뒤 강제로 끈다.
/// 그 사이 같은 VM에 대한 요청(시작, 정지, 강제 종료)이 새로 들어오면 그 요청을 따르고 다시 켜지 않는다.
async fn reboot(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    // 뒤의 요청이 이미 끝났을 수 있다(켜진 VM의 start는 곧바로 끝난다). 그 뒤에 끄면 안 된다.
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Stopped).await?;
    wait_vm(ctx, id, |vm| match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, _) | (_, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        _ => None,
    })
    .await?;
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Running).await?;
    wait_vm(ctx, id, started).await
}

/// 이 Task 뒤에 같은 VM의 전원을 바꾸는 Task(동작, 스냅샷 되돌리기)가 만들어졌는지.
/// 스냅샷 만들기·삭제는 전원을 바꾸지 않으므로 세지 않는다. Task id는 UUIDv7이라 만든 순서대로 커진다.
fn superseded(ctx: &Ctx, task: &Task) -> Result<bool, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == task.spec.target
            && t.id > task.id
            && matches!(
                t.spec.op,
                TaskOp::VmStart
                    | TaskOp::VmStop
                    | TaskOp::VmForceStop
                    | TaskOp::VmReboot
                    | TaskOp::SnapshotRevert { .. }
            )
    }))
}

fn volume_and_pool(ctx: &Ctx, id: Uuid) -> Result<(Volume, StoragePool), String> {
    let volume = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or("volume was deleted")?;
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok((volume, pool))
}

/// 볼륨 스펙의 크기를 바꾼다. 다른 쓰기(라벨 등)를 덮지 않게 다시 읽은 revision으로 쓰고,
/// 그 사이 바뀌었으면 다시 읽어 쓴다.
async fn set_volume_size(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    loop {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("volume was deleted")?;
        if volume.spec.size_bytes == size_bytes {
            return Ok(());
        }
        let spec = VolumeSpec {
            size_bytes,
            ..volume.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(volume.revision), volume.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 파일이 준비된 볼륨을 Ready로 기록한다. 실제 크기(qemu-img가 512바이트 단위로 올림하고, 가져온
/// 이미지는 원본 크기다)와 할당 크기도 남기는데, 이것은 실패해도 넘어간다. Ready는 기록될 때까지
/// 다시 시도한다. 실패로 끝내면 정리하면서 다 만든 파일을 지우게 된다.
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            None
        }
    };
    if let Some(info) = info
        && volume.spec.size_bytes != info.virtual_bytes
    {
        let mut spec = volume.spec.clone();
        spec.size_bytes = info.virtual_bytes;
        if let Err(e) = ctx
            .cluster
            .update_spec(volume.id, None, volume.labels.clone(), &spec)
            .await
        {
            tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
        }
    }
    let status = VolumeStatus {
        phase: VolumePhase::Ready,
        allocated_bytes: info.map(|i| i.allocated_bytes),
        ..volume.status.clone()
    };
    while let Err(e) = ctx
        .cluster
        .update_status::<VolumeSpec>(volume.id, &status)
        .await
    {
        tracing::warn!(volume = %volume.id, error = %e, "cannot mark volume ready; will retry");
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

async fn create_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    match ctx
        .storage
        .create_volume(&pool, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn import_volume(
    ctx: &Ctx,
    id: Uuid,
    source: &Path,
    source_format: SourceFormat,
) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    ctx.storage
        .check_import_source(source, source_format)
        .await
        .map_err(|e| e.to_string())?;
    ctx.storage
        .import_volume(&pool, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    remove_volume(ctx, id).await.map_err(|e| e.to_string())
}

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD, 공유 LVM)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
async fn resize_volume(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err("volume is not ready".into());
    }
    if size_bytes <= volume.spec.size_bytes {
        return Err(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        ));
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let user = vms.iter().find_map(|vm| {
        let disk = vm.spec.disks.iter().position(|d| d.volume == volume.name)?;
        Some((vm, disk))
    });
    match user {
        // 꺼진 VM의 경로(저장소로 늘린다)는 원하는 상태도 꺼짐일 때만 탄다. 켜는 중에 늘리면 그동안
        // 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
        Some((vm, _)) if vm.status.phase != VmPhase::Running && !is_stopped(vm) => {
            return Err(format!(
                "VM {:?} is starting or stopping; try again when it has settled",
                vm.name
            ));
        }
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다. 블록 볼륨은
        // QEMU가 늘릴 수 없으므로 저장소로 먼저 늘린 뒤 QEMU에 볼륨의 실제 크기(LVM은 extent,
        // ZFS는 1 MiB 단위로 올린다)를 알린다. 그래야 게스트가 보는 크기가 아래에서 기록하는
        // 크기와 같다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => {
            let mut size_bytes = size_bytes;
            if block_pool_kind(&pool).is_some() {
                ctx.storage
                    .resize_volume(&pool, id, volume.spec.format, size_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                size_bytes = ctx
                    .storage
                    .volume_info(&pool, id, volume.spec.format)
                    .await
                    .map_err(|e| format!("cannot read the new size: {e}"))?
                    .virtual_bytes;
            }
            ctx.hv
                .block_resize(vm.id, disk, size_bytes)
                .await
                .map_err(|e| e.to_string())?
        }
        _ => ctx
            .storage
            .resize_volume(&pool, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
    }
    let info = ctx
        .storage
        .volume_info(&pool, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    set_volume_size(ctx, id, info.virtual_bytes).await
}

/// 볼륨을 지우지 못한 이유
#[derive(Debug, thiserror::Error)]
enum RemoveError {
    #[error(transparent)]
    Storage(DriverError),
    #[error("{0}")]
    Other(String),
}

impl From<String> for RemoveError {
    fn from(e: String) -> Self {
        RemoveError::Other(e)
    }
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), RemoveError> {
    let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(format!("volume is attached to VM {:?}", vm.name).into());
    }
    if let Some(pool) = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
    {
        match ctx
            .storage
            .delete_volume(&pool, id, volume.spec.format)
            .await
        {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(RemoveError::Storage(e)),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string().into()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일, VM 가져오기의 남은 파일([`crate::vm_imports::sweep`]).
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
/// 풀이 아직 마운트되지 않아(opld가 막 시작했다) 지우지 못한 볼륨을 돌려준다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) -> Vec<Uuid> {
    let mut unmounted = Vec::new();
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .collect();
    match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => {
            for volume in volumes {
                // Task 목록보다 나중에 만든 볼륨은 그 목록으로 판단할 수 없다(다른 노드의 API가
                // 방금 만들었을 수 있다).
                if volume.revision > revision
                    || volume.status.node.is_some_and(|n| n != ctx.config.node)
                {
                    continue;
                }
                let has_task = unfinished.iter().any(|t| t.spec.target == volume.id);
                if volume.status.phase == VolumePhase::Pending && !has_task {
                    tracing::warn!(volume = %volume.name, "removing a volume left unfinished");
                    match remove_volume(ctx, volume.id).await {
                        Ok(()) => {}
                        Err(
                            e @ RemoveError::Storage(
                                DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                            ),
                        ) => {
                            tracing::info!(volume = %volume.name, error = %e, "will remove the volume when its pool is mounted");
                            unmounted.push(volume.id);
                        }
                        Err(e) => {
                            tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume")
                        }
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list volumes"),
    }
    match ctx.cluster.list::<SnapshotSpec>() {
        Ok((_, snapshots)) => {
            for s in snapshots {
                if s.revision > revision || s.status.phase != SnapshotPhase::Pending {
                    continue;
                }
                let creates = |t: &&Task| matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id);
                if unfinished.iter().any(creates) {
                    continue;
                }
                // 만들던 노드: 끝난 Task가 있으면 그 노드, 없으면(Task를 만들기 전에 API가 죽었다)
                // API가 Task를 배정했을 노드(`vm_placement`, 공유 풀이면 노드마다 같은 답이 나오게
                // 이 노드를 앞세우지 않는다). 그것도 없으면 디스크가 없는 VM이라 태그가 없으니 어느
                // 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, None, &vm)));
                if owner.is_some_and(|n| n != ctx.config.node) {
                    continue;
                }
                tracing::warn!(snapshot = %s.id, vm = %s.spec.vm, "removing a snapshot left unfinished");
                if let Err(e) = snapshot::discard(ctx, &s).await {
                    tracing::warn!(snapshot = %s.id, error = %e, "cannot remove snapshot");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list snapshots"),
    }
    let Some(dir) = &ctx.config.upload_dir else {
        return unmounted;
    };
    crate::vm_imports::sweep(&ctx.cluster, ctx.config.node, dir, revision, tasks).await;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return unmounted;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 하위 디렉터리(`imports`)는 가져오기의 정리가 맡는다.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let wanted = unfinished
            .iter()
            .any(|t| matches!(&t.spec.op, TaskOp::VolumeImport { source, .. } if *source == path));
        if !wanted {
            tracing::warn!(path = %path.display(), "removing an upload without a task");
            let _ = std::fs::remove_file(&path);
        }
    }
    unmounted
}
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
/// 않는다) 오류다.
fn ready_pool(ctx: &Ctx, name: &str) -> Result<StoragePool, String> {
    let pool = ctx
```

을 다음으로 바꾼다:

```rust
/// 않는다) 오류다.
pub(super) fn ready_pool(ctx: &Ctx, name: &str) -> Result<StoragePool, String> {
    let pool = ctx
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
    new.spec.ha = false;
    new.spec
```

을 다음으로 바꾼다:

```rust
    new.spec.ha = false;
    // 되살린 VM은 MAC을 새로 받는다.
    for nic in &mut new.spec.nics {
        nic.mac = None;
    }
    new.spec
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
/// 새 볼륨 이름 `{vm}-{volume}`. 63자로 자르고, `taken`이면 `-2`, `-3`…을 붙인다(붙인 뒤에도 63자).
pub(super) fn volume_name(vm: &str, volume: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{vm}-{volume}");
```

을 다음으로 바꾼다:

```rust
/// 새 볼륨 이름 `{vm}-{volume}`. 63자로 자르고, `taken`이면 `-2`, `-3`…을 붙인다(붙인 뒤에도 63자).
pub(crate) fn volume_name(vm: &str, volume: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{vm}-{volume}");
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
    }
    create_vm(ctx, task, new).await
}

/// 복제·복원이 만든 새 VM `new`를 저장소에 만든다: MAC을 새로 받고, 노드·예약 없이 꺼진 VM이다.
/// 성공하면 Task가 만든 것을 더는 정리하지 않는다.
pub(super) async fn create_vm(ctx: &Ctx, task: &Task, mut new: Vm) -> Result<(), String> {
```

을 다음으로 바꾼다:

```rust
    }
    // 복제본은 MAC을 새로 받는다.
    for nic in &mut new.spec.nics {
        nic.mac = None;
    }
    create_vm(ctx, task, new).await
}

/// 복제·복원·가져오기가 만든 새 VM `new`를 저장소에 만든다: 빠진 MAC을 새로 받고, 노드·예약 없이 꺼진
/// VM이다. 성공하면 Task가 만든 것을 더는 정리하지 않는다.
pub(super) async fn create_vm(ctx: &Ctx, task: &Task, mut new: Vm) -> Result<(), String> {
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
        .iter()
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    for nic in &mut new.spec.nics {
        let mac = std::iter::repeat_with(MacAddr::generate)
```

을 다음으로 바꾼다:

```rust
        .iter()
        .chain([&new])
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    for nic in new.spec.nics.iter_mut().filter(|n| n.mac.is_none()) {
        let mac = std::iter::repeat_with(MacAddr::generate)
```

`crates/opl-api/src/tasks/import.rs`를 만든다:

```rust
//! VM 가져오기 Task(설계 §2.4)와 가져오기 지우기 Task. 둘 다 가져오기의 줄에 서서 겹치지 않고, 원본이
//! 있는 노드에서 돈다. API가 확인한 뒤 줄에서 기다리는 사이 이름이나 풀이 바뀌었을 수 있으므로 다시
//! 확인한다.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::resource::Resource;
use opl_types::storage::{SourceFormat, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{MacAddr, Vm, VmSpec};
use opl_types::vm_import::{ImportPhase, ImportRun, VmImport, VmImportSpec, VmImportStatus};
use uuid::Uuid;

use super::backup::ready_pool;
use super::clone::{create_vm, made, volume_name};
use super::{Ctx, mark_ready};
use crate::vm_imports::ova::{extract, parse_ova};
use crate::vm_imports::{EXTRACTED_DISK, SOURCE_OVA, import_dir, import_spec};
use crate::volumes::{UPLOAD_RESERVE_BYTES, default_format};

fn get_import(ctx: &Ctx, id: Uuid) -> Result<Option<VmImport>, String> {
    ctx.cluster
        .get::<VmImportSpec>(id)
        .map_err(|e| e.to_string())
}

/// 이 노드에 있는 가져오기 `id`의 원본 디렉터리
fn source_dir(ctx: &Ctx, id: Uuid) -> Result<PathBuf, String> {
    let dir = ctx
        .config
        .upload_dir
        .as_deref()
        .ok_or("this node has no upload directory")?;
    Ok(import_dir(dir, id))
}

/// 가져오기의 status를 `change`로 바꿔 기록한다. `change`가 `false`면 쓰지 않는다. 다른 쓰기와 부딪치면
/// 다시 읽어 바꾸고, 저장소에 닿지 않으면 기록될 때까지 다시 시도한다. 가져오기가 없어졌으면 그만둔다.
async fn update_import(ctx: &Ctx, id: Uuid, change: impl Fn(&mut VmImportStatus) -> bool) {
    loop {
        let import = match get_import(ctx, id) {
            Ok(Some(import)) => import,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(import = %id, error = %e, "cannot read vm-import; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        let mut status = import.status.clone();
        if !change(&mut status) {
            return;
        }
        match ctx
            .cluster
            .update_status_checked::<VmImportSpec>(id, import.revision, &status)
            .await
        {
            Ok(_) => return,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(import = %id, error = %e, "cannot record vm-import state; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// `importing` 가져오기 `id`를 `run`의 값으로 대상 id의 새 VM으로 만든다(설계 §2.4): 원본 디스크마다
/// OVA에서 추출하고(`.mf`가 있으면 해시 확인) 풀 `run.pool`에 새 볼륨 `{name}-disk{i}`(풀의 기본 형식)를
/// 만들어 옮긴 뒤, 꺼진 VM을 만든다. 끝나면 가져오기를 `done`으로 바꾸고 원본 디렉터리를 지운다.
/// 원본 MAC을 쓰는데 다른 VM이 그 MAC을 쓰면 디스크를 옮기기 전에 실패한다. 디스크마다 업로드
/// 디렉터리에 추출할 여유를 본다.
/// 실패하면 실행기의 정리([`discard`])가 만든 볼륨과 추출한 파일을 지우고 가져오기를 `ready`로 되돌린다.
pub(super) async fn import(
    ctx: &Ctx,
    task: &Task,
    id: Uuid,
    run: &ImportRun,
) -> Result<(), String> {
    let import = get_import(ctx, id)?.ok_or("vm-import was deleted")?;
    if import.status.phase != ImportPhase::Importing {
        return Err("the import is not being run".into());
    }
    let (Some(src), Some(proposal)) = (&import.status.source_vm, &import.status.proposal) else {
        return Err("the import has no source VM".into());
    };
    if ctx
        .cluster
        .get_by_name::<VmSpec>(&run.name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("VM {:?} already exists", run.name));
    }
    if run.keep_macs {
        let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        let used: BTreeSet<MacAddr> = vms
            .iter()
            .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
            .collect();
        if let Some(mac) = src
            .nics
            .iter()
            .filter_map(|n| n.mac)
            .find(|m| used.contains(m))
        {
            return Err(format!("MAC address {mac} is already in use"));
        }
    }
    let pool = ready_pool(ctx, &run.pool)?;
    let format = default_format(&pool);
    let dir = source_dir(ctx, id)?;
    let source = dir.join(SOURCE_OVA);
    let path = source.clone();
    let ova = tokio::task::spawn_blocking(move || parse_ova(&path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("cannot read the OVA: {e}"))?;
    let extracted = dir.join(EXTRACTED_DISK);

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let mut copies: Vec<Volume> = Vec::with_capacity(src.disks.len());
    for disk in &src.disks {
        let failed = |e: &dyn std::fmt::Display| format!("cannot import disk {:?}: {e}", disk.name);
        let entry = ova
            .index
            .get(&disk.file)
            .cloned()
            .ok_or_else(|| failed(&format!("{:?} is not in the OVA", disk.file)))?;
        let available = fs4::available_space(&dir).map_err(|e| failed(&e))?;
        if entry.size.saturating_add(UPLOAD_RESERVE_BYTES) > available {
            return Err(failed(&format!(
                "not enough space in the upload directory to extract {}",
                disk.file
            )));
        }
        let digest = ova.manifest.get(&disk.file).cloned();
        let (from, to) = (source.clone(), extracted.clone());
        tokio::task::spawn_blocking(move || extract(&from, &entry, &to, digest.as_ref()))
            .await
            .map_err(|e| failed(&e))?
            .map_err(|e| failed(&e))?;
        ctx.storage
            .check_import_source(&extracted, SourceFormat::Vmdk)
            .await
            .map_err(|e| failed(&e))?;
        let name = volume_name(&run.name, &disk.name, |n| taken.contains(n));
        taken.insert(name.clone());
        let mut volume: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: pool.name.clone(),
                size_bytes: disk.capacity_bytes,
                format,
            },
        );
        volume.status.node = Some(ctx.config.node);
        // 만들기 전에 적는다. 만들기가 끝났는지 모르는 채 끊겨도 정리가 찾는다.
        made(ctx, task.id, |m| m.volumes.push(volume.id));
        ctx.cluster
            .create(&volume)
            .await
            .map_err(|e| format!("cannot create volume {:?}: {e}", volume.name))?;
        ctx.storage
            .import_volume(&pool, volume.id, &extracted, SourceFormat::Vmdk, format)
            .await
            .map_err(|e| failed(&e))?;
        remove_file(&extracted).await;
        copies.push(volume);
    }

    // 볼륨은 VM을 만들기 바로 전에 `ready`로 바꾼다(복제·복원과 같다).
    for volume in &copies {
        mark_ready(ctx, volume, &pool).await;
    }
    let names: Vec<String> = copies.iter().map(|v| v.name.clone()).collect();
    let spec = import_spec(src, proposal, run, &names)?;
    spec.validate()
        .map_err(|e| format!("invalid VM spec: {e}"))?;
    let mut new: Vm = Resource::new(run.name.as_str(), spec);
    new.id = task.spec.target;
    create_vm(ctx, task, new).await?;

    let vm = task.spec.target;
    update_import(ctx, id, |s| {
        s.phase = ImportPhase::Done;
        s.vm = Some(vm);
        s.error = None;
        true
    })
    .await;
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        tracing::warn!(path = %dir.display(), error = %e, "cannot remove the import files");
    }
    Ok(())
}

async fn remove_file(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "cannot remove extracted disk");
    }
}

/// 실패한 가져오기가 만든 볼륨(파일과 리소스)과 추출한 파일을 지우고, 가져오기를 `ready`로 되돌려 이유를
/// 남긴다. 원본은 남으므로 값을 바꿔 다시 실행할 수 있다. 실행 중이 아니던 가져오기는 그대로 둔다.
/// 대상 VM이 이미 있으면(VM을 만든 뒤 opld가 죽어 Task가 실패로 끝났다) 볼륨과 VM을 두고 가져오기를
/// `done`으로 끝내며 원본 디렉터리를 지운다. 대상 VM이 있는지 읽지 못하면 읽을 때까지 다시 시도한다(있는 VM의
/// 볼륨을 지우지 않게).
pub(super) async fn discard(ctx: &Ctx, task: &Task, id: Uuid, error: &str) {
    let vm = task.spec.target;
    let exists = loop {
        match ctx.cluster.get::<VmSpec>(vm) {
            Ok(found) => break found.is_some(),
            Err(e) => {
                tracing::warn!(vm = %vm, error = %e, "cannot read the imported VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    };
    if exists {
        ctx.cloned.lock().unwrap().remove(&task.id);
        update_import(ctx, id, |s| {
            if s.phase != ImportPhase::Importing {
                return false;
            }
            s.phase = ImportPhase::Done;
            s.vm = Some(vm);
            s.error = None;
            true
        })
        .await;
        if let Ok(dir) = source_dir(ctx, id)
            && let Err(e) = tokio::fs::remove_dir_all(&dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %dir.display(), error = %e, "cannot remove the import files");
        }
        return;
    }
    super::clone::discard(ctx, task.id).await;
    if let Ok(dir) = source_dir(ctx, id) {
        remove_file(&dir.join(EXTRACTED_DISK)).await;
    }
    update_import(ctx, id, |s| {
        if s.phase != ImportPhase::Importing {
            return false;
        }
        s.phase = ImportPhase::Ready;
        s.error = Some(error.to_string());
        true
    })
    .await;
}

/// 가져오기의 원본 디렉터리와 리소스를 지운다. 실행 Task가 끝나지 않았으면 지우지 않는다(`importing`이어도
/// 그 Task가 끝났으면 지운다).
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    if get_import(ctx, id)?.is_none() {
        return Ok(());
    }
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VmImport { import, .. } if import == id)
    }) {
        return Err("the import is being run".into());
    }
    let dir = source_dir(ctx, id)?;
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove the import files: {e}")),
    }
    match ctx.cluster.delete::<VmImportSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
```

`crates/opl-api/src/vm_imports.rs` 전체:

```rust
//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아내고 opLite VM 설정을 제안한다.
//! 원본은 올린 노드의 `{upload_dir}/imports/{가져오기 id}/source.ova`에 둔다. 실행과 지우기는 그 노드의
//! Task 실행기가 한다.

pub mod ova;
pub mod ovf;

use std::collections::BTreeSet;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{
    CpuModel, CpuSpec, DiskBus, DiskSpec, MacAddr, MachineType, NicSpec, PowerState, RestartPolicy,
    VmSpec,
};
use opl_types::vm_import::{
    ImportPhase, ImportProposal, ImportRun, ImportSource, OvaSource, SATA_PORTS, SourceVm,
    VmImport, VmImportSpec, VmImportStatus, propose,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_online, node_name};
use crate::error::{ApiError, Json, Query};
use crate::resources::{check_name, lookup};
use crate::state::AppState;
use crate::tasks::{create_task, volume_name};
use crate::volumes::{ImageFile, check_pool_on, check_upload_space, content_length, receive};
use ova::{OvaError, parse_ova, read_index};

/// `/vm-imports/ova` 경로와 겹치므로 가져오기 이름으로 쓸 수 없다.
const RESERVED_NAME: &str = "ova";
/// 가져오기 디렉터리 안의 원본 OVA 파일 이름
pub(crate) const SOURCE_OVA: &str = "source.ova";
/// 가져오기 Task가 디스크를 하나씩 추출해 두는 파일 이름(가져오기 디렉터리 안)
pub(crate) const EXTRACTED_DISK: &str = "disk.vmdk";

/// 가져오기 `id`의 원본 디렉터리
pub(crate) fn import_dir(upload_dir: &FsPath, id: Uuid) -> PathBuf {
    upload_dir.join("imports").join(id.to_string())
}

/// OVA를 쓸 수 없으면 400 `invalid_ova`(이유를 메시지에), 읽기 오류는 500
pub(crate) fn ova_error(e: OvaError) -> ApiError {
    match e {
        OvaError::Invalid(message) => {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_ova", message)
        }
        OvaError::Io(e) => ApiError::internal(e),
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct OvaUploadQuery {
    /// 가져오기 이름
    pub name: String,
    /// 올린 파일 이름(표시용). 없으면 `{name}.ova`
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct OvaCheckQuery {
    /// 가져오기 이름
    pub name: String,
    /// 올릴 파일 크기(바이트)
    pub size: u64,
}

/// OVA를 받기 전의 확인. 사전 확인과 올리기가 함께 쓴다. 확인 뒤에 다른 요청이 이름을 가져가면
/// 저장소에 만들 때 `name_taken`이 난다.
async fn check_upload(
    state: &AppState,
    principal: &Principal,
    name: &str,
    size: u64,
) -> Result<(), ApiError> {
    state.authorize(principal, Action::Write, VmImportSpec::KIND)?;
    check_name(name)?;
    if name == RESERVED_NAME {
        return Err(ApiError::bad_request(format!(
            "the name {RESERVED_NAME} is reserved"
        )));
    }
    // 원본은 올린 노드에 남고 가져오기도 그 노드에서 하므로 VM을 돌리는 노드여야 한다.
    if let Some(node) = state.cluster.get::<NodeSpec>(state.node)?
        && node.spec.witness
    {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness node and cannot run VMs; upload to another node",
            node.name
        )));
    }
    if state.cluster.get_by_name::<VmImportSpec>(name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("vm-import {name:?} already exists"),
        ));
    }
    check_upload_space(state, size).await
}

/// OVA를 받기 전에 받을 수 있는지 확인한다(204). 거절 이유는 올리기와 같다. 본문은 받지 않는다.
#[utoipa::path(post, path = "/vm-imports/ova/check", tag = "vm-imports",
    params(OvaCheckQuery),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_ova(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<OvaCheckQuery>,
) -> Result<StatusCode, ApiError> {
    check_upload(&state, &principal, &q.name, q.size).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 요청 본문(OVA)을 이 노드에 받아 해석하고, 원본 VM과 제안을 담은 `ready` 가져오기를 돌려준다(201).
/// OVA를 쓸 수 없으면 400 `invalid_ova`. 실패하거나 연결이 끊기면 받은 파일을 지운다.
#[utoipa::path(post, path = "/vm-imports/ova", tag = "vm-imports",
    params(OvaUploadQuery),
    request_body(content = ImageFile, content_type = "application/octet-stream"),
    responses((status = 201, body = Resource<VmImportSpec, VmImportStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn upload_ova(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<OvaUploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<VmImport>), ApiError> {
    let length = content_length(&headers)?;
    check_upload(&state, &principal, &q.name, length).await?;
    let file_name = q.file_name.unwrap_or_else(|| format!("{}.ova", q.name));
    let spec = VmImportSpec {
        node: state.node,
        source: ImportSource::Ova(OvaSource {
            file_name,
            size_bytes: length,
        }),
    };
    let mut import: VmImport = Resource::new(q.name, spec);
    let dir = import_dir(&state.upload_dir, import.id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(ApiError::internal)?;
    // 여기서부터 실패하거나 클라이언트가 끊어 이 future가 버려지면 디렉터리를 지운다.
    let mut guard = DirGuard {
        dir: dir.clone(),
        armed: true,
    };
    let source = dir.join(SOURCE_OVA);
    receive(&source, body, length).await?;
    let ova = tokio::task::spawn_blocking(move || parse_ova(&source))
        .await
        .map_err(ApiError::internal)?
        .map_err(ova_error)?;
    let (_, networks) = state.cluster.list::<NetworkSpec>()?;
    let networks: Vec<String> = networks.into_iter().map(|n| n.name).collect();
    let (proposal, proposal_warnings) = propose(&ova.vm, &networks);
    let mut warnings = ova.warnings;
    warnings.extend(proposal_warnings);
    import.status = VmImportStatus {
        phase: ImportPhase::Ready,
        source_vm: Some(ova.vm),
        proposal: Some(proposal),
        warnings,
        vm: None,
        error: None,
    };
    import.revision = state.cluster.create(&import).await?;
    guard.armed = false;
    Ok((StatusCode::CREATED, Json(import)))
}

/// 올리기를 끝내지 못한 가져오기 디렉터리를 지운다. 정상 경로에서는 `armed`를 내린다.
struct DirGuard {
    dir: PathBuf,
    armed: bool,
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let dir = self.dir.clone();
        tokio::spawn(async move {
            if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %e, "cannot remove a failed upload");
            }
        });
    }
}

/// 이전 opld가 남긴 가져오기 파일을 정리한다(노드가 시작할 때, API를 띄우기 전). 리소스가 없는
/// `imports/{id}` 디렉터리를 지우고, 이 노드의 `ready` 가져오기인데 원본이 없으면 `failed`로 바꾼다.
/// 실행할 Task가 없는 이 노드의 `importing` 가져오기(Task를 만들기 전에 API가 죽었다)는 `ready`로
/// 되돌리고 이유를 남긴다. 다만 그 가져오기의 Task가 VM을 만든 뒤 죽었으면(대상 VM이 있다) `done`으로
/// 끝내고 원본 디렉터리를 지운다. 끝나지 않은 Task가 있으면 실행기가 그 Task를 실패로 끝내며 정리한다.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 그보다 나중에 바뀐 가져오기는 그 목록으로 판단할 수 없다.
pub(crate) async fn sweep(
    cluster: &Cluster,
    node: Uuid,
    upload_dir: &FsPath,
    revision: u64,
    tasks: &[Task],
) {
    let imports = match cluster.list::<VmImportSpec>() {
        Ok((_, imports)) => imports,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list vm imports");
            return;
        }
    };
    if let Ok(entries) = std::fs::read_dir(upload_dir.join("imports")) {
        for entry in entries.flatten() {
            let known = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::try_parse(name).ok())
                .is_some_and(|id| imports.iter().any(|i| i.id == id));
            if !known {
                let path = entry.path();
                tracing::warn!(path = %path.display(), "removing import files without an import");
                let removed = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if let Err(e) = removed {
                    tracing::warn!(path = %path.display(), error = %e, "cannot remove import files");
                }
            }
        }
    }
    for import in imports {
        if import.spec.node != node {
            continue;
        }
        let mut status = import.status.clone();
        let running = tasks.iter().any(|t| {
            !t.status.state.is_finished()
                && matches!(t.spec.op, TaskOp::VmImport { import: i, .. } if i == import.id)
        });
        if status.phase == ImportPhase::Importing && import.revision <= revision && !running {
            match made_vm(cluster, import.id, tasks) {
                Some(vm) => {
                    tracing::warn!(import = %import.name, "the import made its VM before it was interrupted");
                    status.phase = ImportPhase::Done;
                    status.vm = Some(vm);
                    status.error = None;
                    let dir = import_dir(upload_dir, import.id);
                    if let Err(e) = std::fs::remove_dir_all(&dir)
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %dir.display(), error = %e, "cannot remove import files");
                    }
                }
                None => {
                    tracing::warn!(import = %import.name, "the import was interrupted");
                    status.phase = ImportPhase::Ready;
                    status.error = Some("the import was interrupted".into());
                }
            }
        }
        if status.phase == ImportPhase::Ready
            && !import_dir(upload_dir, import.id).join(SOURCE_OVA).exists()
        {
            tracing::warn!(import = %import.name, "the source files of the import are gone");
            status.phase = ImportPhase::Failed;
            status.error = Some("source files are gone".into());
        }
        if status == import.status {
            continue;
        }
        if let Err(e) = cluster
            .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
            .await
        {
            tracing::warn!(import = %import.name, error = %e, "cannot record the import state");
        }
    }
}

/// 가져오기 `import`의 Task 가운데 대상 VM(미리 정한 새 VM id)이 이미 있는 것의 VM id. VM을 만든 뒤
/// 가져오기를 `done`으로 바꾸기 전에 opld가 죽었을 때 쓴다.
fn made_vm(cluster: &Cluster, import: Uuid, tasks: &[Task]) -> Option<Uuid> {
    tasks
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::VmImport { import: i, .. } if i == import))
        .map(|t| t.spec.target)
        .find(|vm| matches!(cluster.get::<VmSpec>(*vm), Ok(Some(_))))
}

/// 원본 VM `src`를 실행 요청 `run`의 값(없으면 제안 `proposal`의 값)으로 옮긴 VM 스펙. `volumes`는 원본
/// 디스크 순서의 볼륨 이름이다. 첫 디스크로 켜고, 노드 없이 꺼진 VM이다. `keep_macs`면 원본 MAC을 쓰고,
/// 아니면 비워 두어 만들 때 새로 받는다. 원본 NIC의 네트워크가 `run.networks`에 없으면 제안의 같은 이름
/// 매핑을 쓰고, 그것도 없으면 오류다. SATA 디스크는 [`SATA_PORTS`]개까지다. 실행 API의 검사와 가져오기
/// Task가 같은 스펙을 만든다.
pub(crate) fn import_spec(
    src: &SourceVm,
    proposal: &ImportProposal,
    run: &ImportRun,
    volumes: &[String],
) -> Result<VmSpec, String> {
    let bus = run.disk_bus.unwrap_or(proposal.disk_bus);
    let model = run.nic_model.unwrap_or(proposal.nic_model);
    if bus == DiskBus::Sata && volumes.len() > SATA_PORTS {
        return Err(format!(
            "{} disks do not fit on SATA ({SATA_PORTS} ports); use virtio-blk",
            volumes.len()
        ));
    }
    let disks = volumes
        .iter()
        .enumerate()
        .map(|(i, volume)| DiskSpec {
            volume: volume.clone(),
            bus,
            boot_order: (i == 0).then_some(1),
        })
        .collect();
    let nics = src
        .nics
        .iter()
        .map(|nic| {
            let network = run
                .networks
                .get(&nic.network)
                .or_else(|| proposal.networks.get(&nic.network).and_then(Option::as_ref))
                .ok_or_else(|| format!("no network mapping for {:?}", nic.network))?;
            Ok(NicSpec {
                network: network.clone(),
                model,
                mac: nic.mac.filter(|_| run.keep_macs),
                security_groups: vec![],
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(VmSpec {
        cpu: CpuSpec {
            sockets: run.sockets.unwrap_or(proposal.sockets),
            cores: run.cores.unwrap_or(proposal.cores),
            model: CpuModel::Host,
        },
        memory_mib: run.memory_mib.unwrap_or(proposal.memory_mib),
        firmware: run.firmware.unwrap_or(proposal.firmware),
        machine: MachineType::Q35,
        disks,
        nics,
        power: PowerState::Stopped,
        restart: RestartPolicy::Never,
        node: None,
        snapshot_schedule: None,
        ha: false,
    })
}

/// 가져오기 `id`를 다루는 끝나지 않은 Task가 하는 일(실행, 지우기)
fn import_tasks(state: &AppState, id: Uuid) -> Result<Vec<&'static str>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .filter_map(|t| match t.spec.op {
            TaskOp::VmImport { import, .. } if import == id => Some("being imported"),
            TaskOp::VmImportDelete if t.spec.target == id => Some("being deleted"),
            _ => None,
        })
        .collect())
}

fn import_busy(import: &VmImport, doing: &str) -> ApiError {
    ApiError::conflict(
        "import_busy",
        format!(
            "vm-import {:?} is {doing}; try again when the task has finished",
            import.name
        ),
    )
}

/// 가져오기를 다루는 Task가 끝나지 않았으면 409 `import_busy`. `importing`이어도 끝나지 않은 Task가
/// 없으면(원본 노드가 없어져 Task가 실패로 끝났다) 쉬는 것으로 본다.
fn check_idle(state: &AppState, import: &VmImport) -> Result<(), ApiError> {
    match import_tasks(state, import.id)?.first() {
        Some(doing) => Err(import_busy(import, doing)),
        None => Ok(()),
    }
}

/// 끝난 실행 Task가 대상 VM을 이미 만들었으면(VM을 만든 뒤 가져오기를 `done`으로 바꾸지 못했다) 가져오기를
/// `done`으로 끝내고 409 `import_done`. 원본이 이 노드에 있으면 원본 디렉터리도 지운다(다른 노드의 것은
/// 가져오기를 지울 때 그 노드가 지운다).
async fn finish_made(state: &AppState, import: &VmImport) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let Some(vm) = made_vm(&state.cluster, import.id, &tasks) else {
        return Ok(());
    };
    tracing::warn!(import = %import.name, "the import made its VM before it was interrupted");
    let status = VmImportStatus {
        phase: ImportPhase::Done,
        vm: Some(vm),
        error: None,
        ..import.status.clone()
    };
    match state
        .cluster
        .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
            return Err(ApiError::conflict(
                "import_busy",
                format!(
                    "vm-import {:?} was changed by another request; try again",
                    import.name
                ),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    if import.spec.node == state.node {
        let dir = import_dir(&state.upload_dir, import.id);
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %dir.display(), error = %e, "cannot remove import files");
        }
    }
    Err(ApiError::conflict(
        "import_done",
        format!("vm-import {:?} has already been imported", import.name),
    ))
}

/// 원본 디렉터리에 가장 큰 디스크를 추출하고도 여유가 남는지 본다(아니면 507). 원본이 없으면 409
/// `import_failed`.
async fn check_extract_space(
    state: &AppState,
    import: &VmImport,
    src: &SourceVm,
) -> Result<(), ApiError> {
    let source = import_dir(&state.upload_dir, import.id).join(SOURCE_OVA);
    if !tokio::fs::try_exists(&source).await.unwrap_or(false) {
        return Err(ApiError::conflict(
            "import_failed",
            format!("the source files of vm-import {:?} are gone", import.name),
        ));
    }
    let index = tokio::task::spawn_blocking(move || read_index(&source))
        .await
        .map_err(ApiError::internal)?
        .map_err(ova_error)?;
    let largest = src
        .disks
        .iter()
        .filter_map(|d| index.get(&d.file))
        .map(|e| e.size)
        .max()
        .unwrap_or(0);
    check_upload_space(state, largest).await
}

/// `ready` 가져오기를 `run`의 값으로 실행할 Task를 돌려준다(202). Task는 원본이 있는 노드에서 돌고, 대상은
/// 미리 정한 새 VM id다. 실행 Task가 끝나지 않았으면 409 `import_busy`, 이미 가져왔으면 `import_done`(끝난 실행
/// Task가 VM을 만들었으면 가져오기를 `done`으로 끝내고), 원본을 쓸 수
/// 없으면 `import_failed`, 원본 노드가 오프라인이면 `node_offline`. 원본 NIC의 네트워크마다 있는 opLite
/// 네트워크로 매핑해야 하고(400), VM 이름과 볼륨 이름 `{name}-disk{i}`가 비어 있어야 한다(409
/// `name_taken`). 풀은 원본 노드에서 쓸 수 있어야 하고(409 `pool_not_on_node`·`pool_unavailable`), 결과
/// VM 스펙은 VM 만들기의 검증을 통과해야 한다(400). 원본 MAC을 쓰면 다른 VM과 겹치지 않아야 한다(409
/// `mac_in_use`). 원본이 이 노드에 있으면 가장 큰 디스크를 추출할 여유도 본다(507).
#[utoipa::path(post, path = "/vm-imports/{key}/run", tag = "vm-imports",
    params(("key" = String, Path, description = "id or name")),
    request_body = ImportRun,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn run_import(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(run): Json<ImportRun>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmImportSpec::KIND)?;
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    let import = lookup::<VmImportSpec>(&state.cluster, &key)?;
    match import.status.phase {
        ImportPhase::Ready | ImportPhase::Importing => {
            check_idle(&state, &import)?;
            finish_made(&state, &import).await?;
        }
        ImportPhase::Done => {
            return Err(ApiError::conflict(
                "import_done",
                format!("vm-import {:?} has already been imported", import.name),
            ));
        }
        ImportPhase::Failed => {
            return Err(ApiError::conflict(
                "import_failed",
                format!(
                    "vm-import {:?} cannot be run: {}",
                    import.name,
                    import.status.error.as_deref().unwrap_or("it failed")
                ),
            ));
        }
    }
    check_online(&state, import.spec.node)?;
    let (Some(src), Some(proposal)) = (&import.status.source_vm, &import.status.proposal) else {
        return Err(ApiError::conflict(
            "import_failed",
            format!("vm-import {:?} has no source VM", import.name),
        ));
    };
    check_name(&run.name)?;
    // 이름이 그새 쓰이면 Task가 VM이나 볼륨을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&run.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", run.name),
        ));
    }
    let volumes: Vec<String> = src
        .disks
        .iter()
        .map(|d| volume_name(&run.name, &d.name, |_| false))
        .collect();
    for volume in &volumes {
        if state.cluster.get_by_name::<VolumeSpec>(volume)?.is_some() {
            return Err(ApiError::conflict(
                "name_taken",
                format!("volume {volume:?} already exists"),
            ));
        }
    }
    let spec = import_spec(src, proposal, &run, &volumes).map_err(ApiError::bad_request)?;
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if state
            .cluster
            .get_by_name::<NetworkSpec>(&nic.network)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
    let pool = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&run.pool)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {:?} not found", run.pool)))?;
    check_pool_on(&state, &pool, import.spec.node, || {
        format!(
            "storage pool {:?} is not available on node {:?} holding vm-import {:?}",
            pool.name,
            node_name(&state, import.spec.node).unwrap_or_else(|_| import.spec.node.to_string()),
            import.name
        )
    })?;
    if import.spec.node == state.node {
        check_extract_space(&state, &import, src).await?;
    }
    // 원본 MAC이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let used: BTreeSet<MacAddr> = vms
        .iter()
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    if let Some(mac) = spec
        .nics
        .iter()
        .filter_map(|n| n.mac)
        .find(|m| used.contains(m))
    {
        return Err(ApiError::conflict(
            "mac_in_use",
            format!("MAC address {mac} is already in use"),
        ));
    }

    let status = VmImportStatus {
        phase: ImportPhase::Importing,
        error: None,
        ..import.status.clone()
    };
    match state
        .cluster
        .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
            return Err(ApiError::conflict(
                "import_busy",
                format!(
                    "vm-import {:?} was changed by another request; try again",
                    import.name
                ),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    let op = TaskOp::VmImport {
        import: import.id,
        run,
    };
    match create_task(&state.cluster, op, Uuid::now_v7(), import.spec.node).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(task))),
        Err(e) => {
            // 실행할 Task가 없으면 다시 실행할 수 있게 되돌린다.
            if let Err(d) = state
                .cluster
                .update_status::<VmImportSpec>(import.id, &import.status)
                .await
            {
                tracing::warn!(import = %import.name, error = %d, "cannot return the import to ready");
            }
            Err(e)
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct DeleteImportQuery {
    /// 원본 노드가 오프라인이거나 클러스터에 없어도 리소스만 지운다. 원본 파일은 그 노드에 남는다.
    #[serde(default)]
    pub force: bool,
}

/// 리소스만 지운 가져오기 지우기(`force`)의 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct VmImportForgotten {
    pub message: String,
}

/// 원본 디렉터리와 리소스를 지울 Task를 돌려준다(202). Task는 원본 노드에서 돈다. 그것을 다루는 Task가
/// 끝나지 않았으면(`force`여도) 409 `import_busy`. 원본 노드가 오프라인이거나 클러스터에 없으면 409
/// `node_offline`이고, `force`면 리소스만 바로 지우고 200이다.
#[utoipa::path(delete, path = "/vm-imports/{key}", tag = "vm-imports",
    params(("key" = String, Path, description = "id or name"), DeleteImportQuery),
    responses(
        (status = 202, body = Resource<TaskSpec, TaskStatus>),
        (status = 200, body = VmImportForgotten, description = "force: only the resource was deleted"),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn delete_import(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(q): Query<DeleteImportQuery>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Write, VmImportSpec::KIND)?;
    let import = lookup::<VmImportSpec>(&state.cluster, &key)?;
    check_idle(&state, &import)?;
    if let Err(e) = check_online(&state, import.spec.node) {
        if !q.force {
            return Err(e);
        }
        tracing::warn!(import = %import.name, node = %import.spec.node, "deleting vm-import without its files");
        state
            .cluster
            .delete::<VmImportSpec>(import.id, None)
            .await?;
        let message = format!(
            "vm-import {:?} deleted; its files are left on node {:?} ({})",
            import.name,
            node_name(&state, import.spec.node)?,
            e.message
        );
        return Ok(Json(VmImportForgotten { message }).into_response());
    }
    let task = create_task(
        &state.cluster,
        TaskOp::VmImportDelete,
        import.id,
        import.spec.node,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[cfg(test)]
mod tests {
    use opl_types::vm::{Firmware, NicModel};
    use opl_types::vm_import::{SourceDisk, SourceNic};

    use super::*;

    fn source() -> SourceVm {
        let disk = |i: usize| SourceDisk {
            name: format!("disk{i}"),
            file: format!("vm-disk{i}.vmdk"),
            capacity_bytes: 1 << 30,
            controller: "scsi".into(),
        };
        let nic = |network: &str, mac: Option<&str>| SourceNic {
            network: network.into(),
            adapter: "vmxnet3".into(),
            mac: mac.map(|m| m.parse().unwrap()),
        };
        SourceVm {
            name: "vm".into(),
            guest_os: "ubuntu64Guest".into(),
            firmware: Firmware::Uefi,
            vcpus: 4,
            cores_per_socket: 2,
            memory_mib: 2048,
            disks: vec![disk(0), disk(1)],
            nics: vec![
                nic("VM Network", Some("00:50:56:8a:12:34")),
                nic("Backup", None),
            ],
        }
    }

    fn run(networks: &[(&str, &str)]) -> ImportRun {
        ImportRun {
            name: "web".into(),
            pool: "local".into(),
            networks: networks
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            disk_bus: None,
            nic_model: None,
            firmware: None,
            sockets: None,
            cores: None,
            memory_mib: None,
            keep_macs: true,
        }
    }

    #[test]
    fn import_spec_takes_the_proposal_unless_the_run_overrides_it() {
        let src = source();
        let (proposal, _) = propose(&src, &[]);
        let volumes = vec!["web-disk0".to_string(), "web-disk1".to_string()];
        let mut r = run(&[("VM Network", "lan"), ("Backup", "user")]);
        let spec = import_spec(&src, &proposal, &r, &volumes).unwrap();
        assert_eq!((spec.cpu.sockets, spec.cpu.cores), (2, 2));
        assert_eq!(spec.cpu.model, CpuModel::Host);
        assert_eq!(spec.memory_mib, 2048);
        assert_eq!(spec.firmware, Firmware::Uefi);
        assert_eq!(spec.machine, MachineType::Q35);
        assert_eq!(spec.power, PowerState::Stopped);
        assert_eq!(spec.node, None);
        let disks: Vec<(&str, DiskBus, Option<u8>)> = spec
            .disks
            .iter()
            .map(|d| (d.volume.as_str(), d.bus, d.boot_order))
            .collect();
        assert_eq!(
            disks,
            [
                ("web-disk0", DiskBus::VirtioBlk, Some(1)),
                ("web-disk1", DiskBus::VirtioBlk, None)
            ]
        );
        let nics: Vec<(&str, NicModel, Option<String>)> = spec
            .nics
            .iter()
            .map(|n| (n.network.as_str(), n.model, n.mac.map(|m| m.to_string())))
            .collect();
        assert_eq!(
            nics,
            [
                (
                    "lan",
                    NicModel::Virtio,
                    Some("00:50:56:8a:12:34".to_string())
                ),
                ("user", NicModel::Virtio, None)
            ]
        );

        r.disk_bus = Some(DiskBus::Sata);
        r.nic_model = Some(NicModel::E1000e);
        r.firmware = Some(Firmware::Bios);
        r.sockets = Some(1);
        r.cores = Some(4);
        r.memory_mib = Some(4096);
        r.keep_macs = false;
        let spec = import_spec(&src, &proposal, &r, &volumes).unwrap();
        assert_eq!((spec.cpu.sockets, spec.cpu.cores), (1, 4));
        assert_eq!(spec.memory_mib, 4096);
        assert_eq!(spec.firmware, Firmware::Bios);
        assert!(spec.disks.iter().all(|d| d.bus == DiskBus::Sata));
        assert!(spec.nics.iter().all(|n| n.model == NicModel::E1000e));
        assert!(spec.nics.iter().all(|n| n.mac.is_none()));
    }

    #[test]
    fn import_spec_needs_a_mapping_for_every_source_network() {
        let src = source();
        let (proposal, _) = propose(&src, &[]);
        let err = import_spec(&src, &proposal, &run(&[("VM Network", "lan")]), &[]).unwrap_err();
        assert_eq!(err, "no network mapping for \"Backup\"");
        // 원본 NIC이 쓰지 않는 매핑은 무시한다.
        let r = run(&[("VM Network", "lan"), ("Backup", "lan"), ("other", "x")]);
        let spec = import_spec(&src, &proposal, &r, &[]).unwrap();
        assert_eq!(spec.nics.len(), 2);
    }

    #[test]
    fn import_spec_falls_back_to_the_proposed_network() {
        let src = source();
        // 이름이 같은 opLite 네트워크 `Backup`이 있다.
        let (proposal, _) = propose(&src, &["Backup".to_string()]);
        let spec = import_spec(&src, &proposal, &run(&[("VM Network", "lan")]), &[]).unwrap();
        let networks: Vec<&str> = spec.nics.iter().map(|n| n.network.as_str()).collect();
        assert_eq!(networks, ["lan", "Backup"]);
        // 요청의 매핑이 제안보다 앞선다.
        let r = run(&[("VM Network", "lan"), ("Backup", "user")]);
        let spec = import_spec(&src, &proposal, &r, &[]).unwrap();
        assert_eq!(spec.nics[1].network, "user");
        // 제안에도 없으면 오류다.
        let err = import_spec(&src, &proposal, &run(&[]), &[]).unwrap_err();
        assert_eq!(err, "no network mapping for \"VM Network\"");
    }

    #[test]
    fn import_spec_puts_at_most_six_disks_on_sata() {
        let src = source();
        let (proposal, _) = propose(&src, &[]);
        let mut r = run(&[("VM Network", "lan"), ("Backup", "lan")]);
        r.disk_bus = Some(DiskBus::Sata);
        let volumes: Vec<String> = (0..7).map(|i| format!("web-disk{i}")).collect();
        let err = import_spec(&src, &proposal, &r, &volumes).unwrap_err();
        assert_eq!(err, "7 disks do not fit on SATA (6 ports); use virtio-blk");
        assert!(import_spec(&src, &proposal, &r, &volumes[..6]).is_ok());
        r.disk_bus = Some(DiskBus::VirtioBlk);
        assert!(import_spec(&src, &proposal, &r, &volumes).is_ok());
    }
}
```

`crates/opl-api/src/volumes.rs`에서

```rust
/// qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    if block_pool_kind(pool).is_some() {
```

을 다음으로 바꾼다:

```rust
/// qcow2
pub(crate) fn default_format(pool: &StoragePool) -> ImageFormat {
    if block_pool_kind(pool).is_some() {
```

`crates/opl-api/src/volumes.rs`에서

```rust
/// 올린 파일이 다 들어와도 데이터 디렉터리(Raft 저장소가 있다)에 남겨 둘 여유 공간
const UPLOAD_RESERVE_BYTES: u64 = 1 << 30;
```

을 다음으로 바꾼다:

```rust
/// 올린 파일이 다 들어와도 데이터 디렉터리(Raft 저장소가 있다)에 남겨 둘 여유 공간
pub(crate) const UPLOAD_RESERVE_BYTES: u64 = 1 << 30;
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/vm-imports/{key}",
        "/vm-imports/ova",
```

을 다음으로 바꾼다:

```rust
        "/vm-imports/{key}",
        "/vm-imports/{key}/run",
        "/vm-imports/ova",
```

`crates/opl-api/tests/vm_import_tasks.rs`를 만든다:

```rust
//! VM 가져오기 Task와 가져오기 지우기 Task: Mock 하이퍼바이저와 dir 풀로, 시험에서 `qemu-img`로 만든
//! streamOptimized VMDK를 담은 OVA를 가져와 꺼진 VM과 볼륨을 만든다. `.mf` 해시가 다르거나 VM을 만들지
//! 못하면 만든 볼륨을 지우고 가져오기를 `ready`로 되돌리며, 고친 값으로 다시 실행할 수 있다.
//! qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test vm_import_tasks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::ustar::{Header, Tar};
use common::{Client, Env, MOUNT_ROOT};
use opl_driver::DirStorage;
use opl_types::network::NetworkSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{CpuModel, DiskBus, Firmware, MachineType, NicModel, PowerState, Vm, VmSpec};
use opl_types::vm_import::{ImportPhase, VmImport, VmImportSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MIB: u64 = 1 << 20;
const MAC: &str = "00:50:56:8a:12:34";

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 4 MiB 디스크의 처음 64 KiB를 `mark`로, 1 MiB부터 64 KiB를 `0x5a`로 채운 streamOptimized VMDK
fn vmdk(dir: &Path, name: &str, mark: u8) -> Vec<u8> {
    let raw = dir.join(format!("{name}.raw"));
    let mut data = vec![0u8; 4 * MIB as usize];
    data[..64 << 10].fill(mark);
    data[MIB as usize..(MIB as usize + (64 << 10))].fill(0x5a);
    std::fs::write(&raw, data).unwrap();
    let out = dir.join(format!("{name}.vmdk"));
    qemu_img(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        "subformat=streamOptimized",
        raw.to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    std::fs::read(out).unwrap()
}

/// vSphere 모양의 OVF: 2 vCPU(소켓당 코어 2), 1024 MiB, efi, Linux, SCSI 디스크 `disks`(파일 이름, 4 MiB),
/// "VM Network"의 vmxnet3 NIC 하나([`MAC`])
fn ovf(disks: &[&str]) -> String {
    let mut files = String::new();
    let mut disk_section = String::new();
    let mut items = String::new();
    for (i, file) in disks.iter().enumerate() {
        files += &format!(r#"<File ovf:href="{file}" ovf:id="file{i}"/>"#);
        disk_section += &format!(
            r#"<Disk ovf:capacity="4" ovf:capacityAllocationUnits="byte * 2^20" ovf:diskId="vmdisk{i}" ovf:fileRef="file{i}" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized"/>"#
        );
        items += &format!(
            r#"<StorageItem>
        <sasd:AddressOnParent>{i}</sasd:AddressOnParent>
        <sasd:ElementName>Hard disk {i}</sasd:ElementName>
        <sasd:HostResource>ovf:/disk/vmdisk{i}</sasd:HostResource>
        <sasd:InstanceID>{}</sasd:InstanceID>
        <sasd:Parent>3</sasd:Parent>
        <sasd:ResourceType>17</sasd:ResourceType>
      </StorageItem>"#,
            10 + i
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Envelope xmlns="http://schemas.dmtf.org/ovf/envelope/2" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/2" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData" xmlns:sasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_StorageAllocationSettingData" xmlns:epasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_EthernetPortAllocationSettingData" xmlns:vmw="http://www.vmware.com/schema/ovf">
  <References>{files}</References>
  <DiskSection><Info>Virtual disk information</Info>{disk_section}</DiskSection>
  <NetworkSection><Info>Networks</Info><Network ovf:name="VM Network"/></NetworkSection>
  <VirtualSystem ovf:id="web">
    <Info>A virtual machine</Info>
    <Name>web</Name>
    <OperatingSystemSection ovf:id="101" vmw:osType="ubuntu64Guest"><Info>Guest OS</Info></OperatingSystemSection>
    <VirtualHardwareSection>
      <Info>Virtual hardware requirements</Info>
      <Item>
        <rasd:ElementName>2 virtual CPU(s)</rasd:ElementName>
        <rasd:InstanceID>1</rasd:InstanceID>
        <rasd:ResourceType>3</rasd:ResourceType>
        <rasd:VirtualQuantity>2</rasd:VirtualQuantity>
        <vmw:CoresPerSocket ovf:required="false">2</vmw:CoresPerSocket>
      </Item>
      <Item>
        <rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits>
        <rasd:ElementName>1024MB of memory</rasd:ElementName>
        <rasd:InstanceID>2</rasd:InstanceID>
        <rasd:ResourceType>4</rasd:ResourceType>
        <rasd:VirtualQuantity>1024</rasd:VirtualQuantity>
      </Item>
      <Item>
        <rasd:Address>0</rasd:Address>
        <rasd:ElementName>SCSI Controller 0</rasd:ElementName>
        <rasd:InstanceID>3</rasd:InstanceID>
        <rasd:ResourceSubType>VirtualSCSI</rasd:ResourceSubType>
        <rasd:ResourceType>6</rasd:ResourceType>
      </Item>
      {items}
      <EthernetPortItem>
        <epasd:Address>{MAC}</epasd:Address>
        <epasd:Connection>VM Network</epasd:Connection>
        <epasd:ElementName>Network adapter 1</epasd:ElementName>
        <epasd:InstanceID>4</epasd:InstanceID>
        <epasd:ResourceSubType>VmxNet3</epasd:ResourceSubType>
        <epasd:ResourceType>10</epasd:ResourceType>
      </EthernetPortItem>
      <vmw:Config ovf:required="false" vmw:key="firmware" vmw:value="efi"/>
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>
"#
    )
}

fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// 디스크 `disks`(파일 이름, 내용)와 `.mf`를 담은 OVA. `wrong`인 파일은 `.mf`의 해시를 틀리게 적는다.
fn ova(disks: &[(&str, &[u8])], wrong: Option<&str>) -> Vec<u8> {
    let names: Vec<&str> = disks.iter().map(|(n, _)| *n).collect();
    let ovf = ovf(&names);
    let mut mf = format!("SHA256(web.ovf)= {}\n", sha256(ovf.as_bytes()));
    for (name, data) in disks {
        let digest = match wrong == Some(*name) {
            true => sha256(b"something else"),
            false => sha256(data),
        };
        mf += &format!("SHA256({name})= {digest}\n");
    }
    let mut tar = Tar::new()
        .file("web.ovf", ovf.as_bytes())
        .file("web.mf", mf.as_bytes());
    for (name, data) in disks {
        tar = tar.file(name, data);
    }
    tar.finish()
}

fn pool_dir(env: &Env) -> PathBuf {
    env.dir.path().join("local")
}

/// 네트워크 `lan`과 이 노드에서 준비된 dir 풀 `local`
async fn setup(env: &Env) {
    let network = Resource::new(
        "lan",
        NetworkSpec {
            bridge: Some("br0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    env.cluster.create(&network).await.unwrap();
    std::fs::create_dir_all(pool_dir(env)).unwrap();
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: pool_dir(env),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
}

/// OVA를 가져오기 `web`으로 올린다.
async fn upload(admin: &Client, ova: Vec<u8>) -> Value {
    let (status, body) = admin.upload("/vm-imports/ova?name=web", ova).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

/// 가져오기 `web`을 `body`로 실행하고 Task가 끝날 때까지 기다린다.
async fn run(admin: &Client, body: Value) -> Value {
    let (status, task) = admin.post("/vm-imports/web/run", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn run_request() -> Value {
    json!({ "name": "web", "pool": "local", "networks": { "VM Network": "lan" } })
}

fn import(env: &Env) -> Option<VmImport> {
    env.cluster.get_by_name::<VmImportSpec>("web").unwrap()
}

fn get_vm(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn get_volume(env: &Env, name: &str) -> Option<Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

/// 원본 디렉터리 안의 파일 이름
fn source_files(env: &Env, import: &VmImport) -> Vec<String> {
    let dir = env
        .dir
        .path()
        .join("uploads/imports")
        .join(import.id.to_string());
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect(),
        Err(_) => vec![],
    }
}

/// 가져오기는 디스크를 qcow2 볼륨 `{name}-disk{i}`로 옮기고, 제안대로(Linux: virtio, efi: uefi, 2 vCPU를
/// 소켓 1·코어 2로) 원본 MAC을 쓰는 꺼진 VM을 만든다. 새 VM id는 Task의 대상이다. 끝나면 가져오기는
/// `done`이고 원본 디렉터리는 없다. 디스크 크기만큼 걸리므로 Task 시간 제한을 받지 않는다. 다 가져온
/// 가져오기를 지워도 VM은 남는다.
#[tokio::test]
async fn imports_an_ova_into_a_stopped_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.timeout = Duration::ZERO
    })
    .await;
    let disk = vmdk(env.dir.path(), "src", 0xaa);
    let uploaded = upload(&admin, ova(&[("web-disk1.vmdk", &disk)], None)).await;
    assert_eq!(uploaded["status"]["proposal"]["disk_bus"], "virtio-blk");

    let task = run(&admin, run_request()).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = get_vm(&env, "web").unwrap();
    assert_eq!(task["spec"]["target"], vm.id.to_string());
    let spec = &vm.spec;
    assert_eq!((spec.cpu.sockets, spec.cpu.cores), (1, 2));
    assert_eq!(spec.cpu.model, CpuModel::Host);
    assert_eq!(spec.memory_mib, 1024);
    assert_eq!(spec.firmware, Firmware::Uefi);
    assert_eq!(spec.machine, MachineType::Q35);
    assert_eq!(spec.power, PowerState::Stopped);
    assert_eq!(spec.node, None);
    assert_eq!(spec.disks.len(), 1);
    assert_eq!(spec.disks[0].volume, "web-disk0");
    assert_eq!(spec.disks[0].bus, DiskBus::VirtioBlk);
    assert_eq!(spec.disks[0].boot_order, Some(1));
    assert_eq!(spec.nics.len(), 1);
    assert_eq!(spec.nics[0].network, "lan");
    assert_eq!(spec.nics[0].model, NicModel::Virtio);
    assert_eq!(
        spec.nics[0].mac.map(|m| m.to_string()).as_deref(),
        Some(MAC)
    );

    let volume = get_volume(&env, "web-disk0").unwrap();
    assert_eq!(volume.spec.pool, "local");
    assert_eq!(volume.spec.format, ImageFormat::Qcow2);
    assert_eq!(volume.spec.size_bytes, 4 * MIB);
    assert_eq!(volume.status.phase, VolumePhase::Ready);
    assert_eq!(volume.status.node, Some(env.node));
    let path = pool_dir(&env).join(format!("{}.qcow2", volume.id));
    assert!(qemu_io(&path, "qcow2", "read -P 0xaa 0 64k"));
    assert!(qemu_io(&path, "qcow2", "read -P 0x5a 1M 64k"));

    let done = import(&env).unwrap();
    assert_eq!(done.status.phase, ImportPhase::Done);
    assert_eq!(done.status.vm, Some(vm.id));
    assert_eq!(done.status.error, None);
    assert!(source_files(&env, &done).is_empty());

    // 다 가져온 가져오기는 다시 실행하지 않는다. 지우면 리소스만 없어지고 VM과 볼륨은 남는다.
    let (status, body) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "import_done");
    // 지우기 Task는 시간 제한을 받으므로 보통 설정으로 되돌린다.
    env.stop_runner().await;
    env.start_runner().await;
    let (status, task) = admin.delete("/vm-imports/web").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(import(&env).is_none());
    assert!(get_vm(&env, "web").is_some());
    assert!(get_volume(&env, "web-disk0").is_some());
}

/// `.mf`의 해시가 다른 디스크가 있으면 실행은 실패한다: 그때까지 만든 볼륨(파일과 리소스)과 추출한
/// 파일을 지우고 VM을 만들지 않으며, 가져오기는 `ready`로 돌아가 이유를 남긴다. 원본은 남는다.
#[tokio::test]
async fn checksum_mismatch_fails_and_leaves_no_volumes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let first = vmdk(env.dir.path(), "a", 0xaa);
    let second = vmdk(env.dir.path(), "b", 0xbb);
    let disks: [(&str, &[u8]); 2] = [("web-disk1.vmdk", &first), ("web-disk2.vmdk", &second)];
    upload(&admin, ova(&disks, Some("web-disk2.vmdk"))).await;

    let task = run(&admin, run_request()).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert_eq!(
        error,
        "cannot import disk \"disk1\": checksum mismatch for web-disk2.vmdk"
    );
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Ready);
    assert_eq!(failed.status.error.as_deref(), Some(error));
    assert!(get_vm(&env, "web").is_none());
    assert!(get_volume(&env, "web-disk0").is_none());
    assert!(get_volume(&env, "web-disk1").is_none());
    let left: Vec<_> = std::fs::read_dir(pool_dir(&env)).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(source_files(&env, &failed), ["source.ova"]);
}

/// 원본 MAC을 쓰는 실행은 그새 다른 VM이 그 MAC을 가져갔으면 디스크를 옮기기 전에 실패한다. 값을
/// 고쳐(새 MAC) 다시 실행하면 가져오고 이전 오류를 지운다.
#[tokio::test]
async fn failed_run_can_be_run_again_with_other_values() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let disk = vmdk(env.dir.path(), "src", 0xaa);
    upload(&admin, ova(&[("web-disk1.vmdk", &disk)], None)).await;

    // API가 확인한 뒤 Task가 돌기 전에 같은 MAC의 VM이 생긴다.
    env.stop_runner().await;
    let (status, task) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let mut other = common::vm_request("other");
    other["spec"]["nics"][0]["mac"] = json!(MAC);
    let (status, body) = admin.post("/vms", other).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.start_runner().await;

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert_eq!(error, format!("MAC address {MAC} is already in use"));
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Ready);
    assert_eq!(failed.status.error.as_deref(), Some(error));
    assert!(get_vm(&env, "web").is_none());
    assert!(get_volume(&env, "web-disk0").is_none());
    let left: Vec<_> = std::fs::read_dir(pool_dir(&env)).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");

    let mut body = run_request();
    body["keep_macs"] = json!(false);
    let task = run(&admin, body).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = get_vm(&env, "web").unwrap();
    let mac = vm.spec.nics[0].mac.unwrap().to_string();
    assert_ne!(mac, MAC);
    let done = import(&env).unwrap();
    assert_eq!(done.status.phase, ImportPhase::Done);
    assert_eq!(done.status.error, None);
    assert!(qemu_io(
        &pool_dir(&env).join(format!(
            "{}.qcow2",
            get_volume(&env, "web-disk0").unwrap().id
        )),
        "qcow2",
        "read -P 0xaa 0 64k"
    ));
}

/// 디스크마다 추출하기 전에 업로드 디렉터리의 여유(디스크 크기 + 1 GiB)를 본다. 모자라면 볼륨을 남기지
/// 않고 실패해 가져오기는 `ready`로 돌아간다. 여기서는 API가 확인한 뒤 원본을 디스크가 아주 큰(빈 곳이 많은)
/// OVA로 바꾼다.
#[tokio::test]
async fn extract_needs_space_in_the_upload_directory() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let disk = vmdk(env.dir.path(), "src", 0xaa);
    let uploaded = upload(&admin, ova(&[("web-disk1.vmdk", &disk)], None)).await;
    env.stop_runner().await;
    let (status, task) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    let source = env
        .dir
        .path()
        .join("uploads/imports")
        .join(uploaded["id"].as_str().unwrap())
        .join("source.ova");
    let available = fs4::available_space(source.parent().unwrap()).unwrap();
    let size = available.next_multiple_of(512);
    let ovf = ovf(&["web-disk1.vmdk"]);
    let tar = Tar::new()
        .file("web.ovf", ovf.as_bytes())
        .pax(&[("size", &size.to_string())])
        .raw(
            Header {
                name: "web-disk1.vmdk",
                prefix: "",
                typeflag: b'0',
                size: 0,
                base256: false,
            },
            &[],
        )
        .into_bytes();
    std::fs::write(&source, &tar).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(tar.len() as u64 + size)
        .unwrap();
    env.start_runner().await;

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert_eq!(
        error,
        "cannot import disk \"disk0\": not enough space in the upload directory to extract web-disk1.vmdk"
    );
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Ready);
    assert!(get_volume(&env, "web-disk0").is_none());
    assert_eq!(source_files(&env, &failed), ["source.ova"]);
}
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust
//! VM 가져오기 API: OVA 올리기 사전 확인, 올리기(해석과 제안), 목록·읽기, 노드 시작 때의 정리.
```

을 다음으로 바꾼다:

```rust
//! VM 가져오기 API: OVA 올리기 사전 확인, 올리기(해석과 제안), 목록·읽기, 실행 요청의 검사, 지우기와
//! 강제 지우기, 노드 시작 때의 정리.
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust
use common::ustar::Tar;
use common::{Client, Env};
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::vm_import::{
```

을 다음으로 바꾼다:

```rust
use common::ustar::Tar;
use common::{Client, Env, vm_request};
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm_import::{
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust
    assert_eq!(phase(other.id), (ImportPhase::Ready, None));
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(phase(other.id), (ImportPhase::Ready, None));
}

/// 이 노드에서 준비된 dir 풀. `nodes`가 비어 있지 않으면 그 노드에서만 쓸 수 있다.
async fn pool(env: &Env, name: &str, nodes: Vec<Uuid>) {
    let dir = env.dir.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path: dir },
            nodes,
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 네트워크 `lan`, 풀 `local`, 올린 가져오기 `web01`(vSphere 견본: 디스크 셋, NIC 둘)
async fn uploaded(env: &Env, admin: &Client) -> Value {
    network(env, "lan").await;
    pool(env, "local", vec![]).await;
    let (status, body) = admin
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

/// 원본 네트워크를 모두 `lan`에 매핑한 실행 요청
fn run_request(name: &str) -> Value {
    json!({
        "name": name,
        "pool": "local",
        "networks": { "VM Network": "lan", "lan": "lan" },
    })
}

async fn run(client: &Client, import: &str, body: Value) -> (StatusCode, Value) {
    client
        .post(&format!("/vm-imports/{import}/run"), body)
        .await
}

fn import_named(env: &Env, name: &str) -> Option<VmImport> {
    env.cluster.get_by_name::<VmImportSpec>(name).unwrap()
}

async fn set_phase(env: &Env, name: &str, phase: ImportPhase) {
    let import = import_named(env, name).unwrap();
    let status = VmImportStatus {
        phase,
        ..import.status
    };
    env.cluster
        .update_status::<VmImportSpec>(import.id, &status)
        .await
        .unwrap();
}

/// Task를 저장소에 직접 만든다(API의 검사 없이).
async fn put_task(env: &Env, op: TaskOp, target: Uuid, state: TaskState) -> Value {
    let mut task: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    if state != TaskState::Pending {
        let status = TaskStatus { state, error: None };
        env.cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
    }
    json!({ "id": task.id })
}

/// 실행 요청은 Task를 만들기 전에 매핑·이름·풀·MAC과 결과 VM 스펙을 확인한다.
#[tokio::test]
async fn run_checks_the_request() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let (status, vm) = admin.post("/vms", vm_request("taken")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let volume = Resource::new(
        "vol-disk1",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: opl_types::runtime::ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    pool(&env, "far", vec![Uuid::now_v7()]).await;
    // 원본의 첫 NIC과 같은 MAC을 쓰는 VM
    let mut mac = vm_request("mac");
    mac["spec"]["nics"][0]["mac"] = json!("00:50:56:8a:12:34");
    let (status, vm) = admin.post("/vms", mac).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");

    let with = |patch: Value| {
        let mut body = run_request("web01");
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    for (body, want, code, message) in [
        (
            with(json!({ "networks": { "lan": "lan" } })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("no network mapping for \"VM Network\""),
        ),
        (
            with(json!({ "networks": { "VM Network": "nope", "lan": "lan" } })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("network \"nope\" not found"),
        ),
        (
            with(json!({ "name": "Bad_Name" })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            None,
        ),
        (
            with(json!({ "name": "taken" })),
            StatusCode::CONFLICT,
            "name_taken",
            Some("VM \"taken\" already exists"),
        ),
        (
            with(json!({ "name": "vol" })),
            StatusCode::CONFLICT,
            "name_taken",
            Some("volume \"vol-disk1\" already exists"),
        ),
        (
            with(json!({ "pool": "nope" })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("storage pool \"nope\" not found"),
        ),
        (
            with(json!({ "pool": "far" })),
            StatusCode::CONFLICT,
            "pool_not_on_node",
            None,
        ),
        (
            with(json!({ "sockets": 0 })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("cpu.sockets must be at least 1"),
        ),
        (
            run_request("web01"),
            StatusCode::CONFLICT,
            "mac_in_use",
            Some("MAC address 00:50:56:8a:12:34 is already in use"),
        ),
        (
            with(json!({ "surprise": 1 })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            None,
        ),
    ] {
        let (status, got) = run(&admin, "web01", body.clone()).await;
        assert_eq!(
            (status, got["code"].as_str()),
            (want, Some(code)),
            "{body} {got}"
        );
        if let Some(message) = message {
            assert_eq!(got["message"], message, "{body}");
        }
    }
    // 거절한 요청은 가져오기를 바꾸지 않는다.
    let import = import_named(&env, "web01").unwrap();
    assert_eq!(import.status.phase, ImportPhase::Ready);
    assert_eq!(import.status.error, None);

    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = run(&viewer, "web01", run_request("web01")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = run(&admin, "nope", run_request("web01")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// 받은 요청은 가져오기를 `importing`으로 바꾸고 새 VM id를 대상으로 한 Task를 돌려준다. 그동안 다시
/// 실행하거나 지울 수 없다. Task가 실패하면 `ready`로 돌아가 이유를 남긴다(여기서는 가짜 디스크라 실패한다).
#[tokio::test]
async fn run_starts_a_task_and_failure_returns_to_ready() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let mut body = run_request("web01");
    // 새 MAC을 받으면 원본 MAC이 겹쳐도 된다.
    body["keep_macs"] = json!(false);
    // 요청에 없는 원본 네트워크 `lan`은 제안의 매핑(이름이 같은 네트워크)을 쓴다.
    body["networks"] = json!({ "VM Network": "lan" });
    let mut mac = vm_request("mac");
    mac["spec"]["nics"][0]["mac"] = json!("00:50:56:8a:12:34");
    let (status, vm) = admin.post("/vms", mac).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");

    let (status, task) = run(&admin, "web01", body.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let import = import_named(&env, "web01").unwrap();
    assert_eq!(task["spec"]["op"]["kind"], "vm-import");
    assert_eq!(task["spec"]["op"]["import"], import.id.to_string());
    assert_eq!(task["spec"]["op"]["run"]["keep_macs"], false);
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let target = task["spec"]["target"].as_str().unwrap();
    assert_ne!(target, import.id.to_string());

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let import = import_named(&env, "web01").unwrap();
    assert_eq!(import.status.phase, ImportPhase::Ready);
    let error = import.status.error.unwrap();
    assert!(error.starts_with("cannot import disk \"disk0\""), "{error}");
    assert_eq!(task["status"]["error"], error.as_str());
    // 추출한 파일은 지우고 원본은 남긴다.
    let dir = imports_dir(&env).join(import.id.to_string());
    assert!(!dir.join("disk.vmdk").exists());
    assert!(dir.join("source.ova").exists());
    let (_, volumes) = env.cluster.list::<VolumeSpec>().unwrap();
    assert!(volumes.is_empty(), "{volumes:?}");
}

/// 실행 Task가 끝나지 않은 가져오기와 끝난·쓸 수 없는 가져오기는 실행하지 않는다. 실행·지우기 Task가
/// 끝나지 않았으면 지우지도 않는다(`force`여도).
#[tokio::test]
async fn run_and_delete_need_an_idle_import() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let id = import_named(&env, "web01").unwrap().id;
    env.stop_runner().await;
    let op = TaskOp::VmImport {
        import: id,
        run: serde_json::from_value(run_request("web01")).unwrap(),
    };
    put_task(&env, op, Uuid::now_v7(), TaskState::Running).await;
    for (phase, code) in [
        (ImportPhase::Importing, "import_busy"),
        (ImportPhase::Done, "import_done"),
        (ImportPhase::Failed, "import_failed"),
    ] {
        set_phase(&env, "web01", phase).await;
        let (status, body) = run(&admin, "web01", run_request("web01")).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::CONFLICT, Some(code)),
            "{body}"
        );
    }
    set_phase(&env, "web01", ImportPhase::Importing).await;
    for path in ["/vm-imports/web01", "/vm-imports/web01?force=true"] {
        let (status, body) = admin.delete(path).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::CONFLICT, Some("import_busy")),
            "{body}"
        );
    }

    // 끝나지 않은 지우기 Task가 있어도 바쁘다.
    set_phase(&env, "web01", ImportPhase::Ready).await;
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    for task in tasks {
        let status = TaskStatus {
            state: TaskState::Failed,
            error: None,
        };
        env.cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
    }
    put_task(&env, TaskOp::VmImportDelete, id, TaskState::Pending).await;
    let (status, body) = run(&admin, "web01", run_request("web01")).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("import_busy")),
        "{body}"
    );
    let (status, body) = admin.delete("/vm-imports/web01").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("import_busy")),
        "{body}"
    );
}

/// 원본 노드가 없어져 실행 Task가 실패로 끝나면(정리가 돌지 않아) 가져오기는 `importing`에 남는다. 끝나지
/// 않은 Task가 없으면 쉬는 것으로 보아 다시 실행하거나 지울 수 있고, 노드가 없으면 `force`로 지운다.
#[tokio::test]
async fn importing_without_a_running_task_is_idle() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let web01 = import_named(&env, "web01").unwrap();
    let failed = |import: Uuid| TaskOp::VmImport {
        import,
        run: serde_json::from_value(run_request("web01")).unwrap(),
    };
    put_task(&env, failed(web01.id), Uuid::now_v7(), TaskState::Failed).await;
    set_phase(&env, "web01", ImportPhase::Importing).await;

    // 다시 실행할 수 있다(가짜 디스크라 실패해 `ready`로 돌아간다).
    let (status, task) = run(&admin, "web01", run_request("web01")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        import_named(&env, "web01").unwrap().status.phase,
        ImportPhase::Ready
    );

    // 지우기 Task도 지운다.
    set_phase(&env, "web01", ImportPhase::Importing).await;
    let (status, task) = admin.delete("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(import_named(&env, "web01").is_none());
    assert!(!imports_dir(&env).join(web01.id.to_string()).exists());

    // 클러스터에서 없어진 노드의 가져오기는 `force`로 지운다.
    let far = import_on(&env, "far", Uuid::now_v7(), ImportPhase::Importing).await;
    put_task(&env, failed(far.id), Uuid::now_v7(), TaskState::Failed).await;
    let (status, body) = admin.delete("/vm-imports/far").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("node_offline")),
        "{body}"
    );
    let (status, body) = admin.delete("/vm-imports/far?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(import_named(&env, "far").is_none());
}

/// 끝난 실행 Task가 대상 VM을 이미 만들었는데 가져오기가 `done`이 되지 못했으면(VM을 만든 뒤 opld가
/// 죽었다), 실행 요청이 가져오기를 `done`으로 끝내고 409 `import_done`이다. 이 노드의 원본 디렉터리는 지우고
/// VM은 둔다. `importing`이든 `ready`든 같다.
#[tokio::test]
async fn run_finishes_an_import_whose_vm_was_made() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let web01 = import_named(&env, "web01").unwrap();
    let other = import_on(&env, "other", env.node, ImportPhase::Ready).await;
    for (import, phase) in [
        (&web01, ImportPhase::Importing),
        (&other, ImportPhase::Ready),
    ] {
        set_phase(&env, &import.name, phase).await;
        let mut vm: opl_types::vm::Vm = Resource::new(
            format!("{}-vm", import.name),
            serde_json::from_value(vm_request("x")["spec"].clone()).unwrap(),
        );
        vm.id = Uuid::now_v7();
        env.cluster.create(&vm).await.unwrap();
        let op = TaskOp::VmImport {
            import: import.id,
            run: serde_json::from_value(run_request(&import.name)).unwrap(),
        };
        put_task(&env, op, vm.id, TaskState::Failed).await;

        let (status, body) = run(&admin, &import.name, run_request("again")).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::CONFLICT, Some("import_done")),
            "{body}"
        );
        let done = import_named(&env, &import.name).unwrap();
        assert_eq!(done.status.phase, ImportPhase::Done);
        assert_eq!(done.status.vm, Some(vm.id));
        assert_eq!(done.status.error, None);
        assert!(!imports_dir(&env).join(import.id.to_string()).exists());
        assert!(
            env.cluster
                .get::<opl_types::vm::VmSpec>(vm.id)
                .unwrap()
                .is_some()
        );
    }
}

/// 지우기 Task는 원본 디렉터리와 리소스를 지운다. 원본 노드가 오프라인이거나 없으면 409 `node_offline`이고,
/// `force`면 리소스만 지운다(200).
#[tokio::test]
async fn delete_removes_files_and_force_forgets_an_unreachable_import() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let body = uploaded(&env, &admin).await;
    let dir = imports_dir(&env).join(body["id"].as_str().unwrap());
    assert!(dir.exists());

    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = viewer.delete("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, task) = admin.delete("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-import-delete");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(import_named(&env, "web01").is_none());
    assert!(!dir.exists());

    // 클러스터에 없는 노드의 가져오기
    import_on(&env, "far", Uuid::now_v7(), ImportPhase::Ready).await;
    let (status, body) = admin.delete("/vm-imports/far").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("node_offline")),
        "{body}"
    );
    let (status, body) = run(&admin, "far", run_request("far")).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("node_offline")),
        "{body}"
    );
    let (status, body) = admin.delete("/vm-imports/far?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("vm-import \"far\" deleted; its files are left on node"),
        "{body}"
    );
    assert!(import_named(&env, "far").is_none());
}

/// 노드가 시작할 때 실행할 Task가 없는 `importing` 가져오기는 `ready`로 되돌리고 이유를 남긴다. 이전
/// opld가 돌리던 가져오기 Task는 실패로 끝나고, 그 정리가 가져오기를 `ready`로 되돌린다.
#[tokio::test]
async fn startup_returns_interrupted_imports_to_ready() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let lost = import_on(&env, "lost", env.node, ImportPhase::Importing).await;
    let cut = import_on(&env, "cut", env.node, ImportPhase::Importing).await;
    for import in [&lost, &cut] {
        let dir = imports_dir(&env).join(import.id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source.ova"), b"ova").unwrap();
    }
    let run = serde_json::from_value(run_request("cut")).unwrap();
    let op = TaskOp::VmImport {
        import: cut.id,
        run,
    };
    let task = put_task(&env, op, Uuid::now_v7(), TaskState::Running).await;

    env.start_runner().await;

    let lost = env.cluster.get::<VmImportSpec>(lost.id).unwrap().unwrap();
    assert_eq!(lost.status.phase, ImportPhase::Ready);
    assert_eq!(
        lost.status.error.as_deref(),
        Some("the import was interrupted")
    );
    let admin = env.admin().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let cut = env.cluster.get::<VmImportSpec>(cut.id).unwrap().unwrap();
    assert_eq!(cut.status.phase, ImportPhase::Ready);
    assert!(
        cut.status
            .error
            .as_deref()
            .unwrap()
            .starts_with("interrupted"),
        "{cut:?}"
    );
}

/// VM을 만든 뒤 가져오기를 `done`으로 바꾸기 전에 opld가 죽었으면, 노드가 시작할 때 대상 VM이 있는
/// 가져오기를 `done`으로 끝내고 원본 디렉터리를 지운다. 끝난 Task(시작 정리)와 끝나지 않은 Task(실행기가
/// 실패로 끝내는 정리) 둘 다 그렇다. VM과 볼륨은 지우지 않는다.
#[tokio::test]
async fn startup_finishes_imports_whose_vm_was_made() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let ended = import_on(&env, "ended", env.node, ImportPhase::Importing).await;
    let cut = import_on(&env, "cut", env.node, ImportPhase::Importing).await;
    let mut tasks = vec![];
    for (import, state) in [(&ended, TaskState::Failed), (&cut, TaskState::Running)] {
        let dir = imports_dir(&env).join(import.id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source.ova"), b"ova").unwrap();
        let mut vm: opl_types::vm::Vm = Resource::new(
            import.name.as_str(),
            serde_json::from_value(vm_request("x")["spec"].clone()).unwrap(),
        );
        vm.id = Uuid::now_v7();
        env.cluster.create(&vm).await.unwrap();
        let op = TaskOp::VmImport {
            import: import.id,
            run: serde_json::from_value(run_request(&import.name)).unwrap(),
        };
        tasks.push((put_task(&env, op, vm.id, state).await, vm.id));
    }

    env.start_runner().await;

    let ended_done = env.cluster.get::<VmImportSpec>(ended.id).unwrap().unwrap();
    assert_eq!(ended_done.status.phase, ImportPhase::Done);
    assert_eq!(ended_done.status.vm, Some(tasks[0].1));
    assert_eq!(ended_done.status.error, None);
    assert!(!imports_dir(&env).join(ended.id.to_string()).exists());
    let admin = env.admin().await;
    let task = admin.wait_task(&tasks[1].0).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let cut_done = env.cluster.get::<VmImportSpec>(cut.id).unwrap().unwrap();
    assert_eq!(cut_done.status.phase, ImportPhase::Done);
    assert_eq!(cut_done.status.vm, Some(tasks[1].1));
    assert!(!imports_dir(&env).join(cut.id.to_string()).exists());
    for (_, vm) in tasks {
        assert!(
            env.cluster
                .get::<opl_types::vm::VmSpec>(vm)
                .unwrap()
                .is_some()
        );
    }
}
```

`crates/opl-types/src/task.rs`에서

```rust
use crate::storage::SourceFormat;
```

을 다음으로 바꾼다:

```rust
use crate::storage::SourceFormat;
use crate::vm_import::ImportRun;
```

`crates/opl-types/src/task.rs`에서

```rust
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제·백업 Task는 (원본) VM, 백업 지우기는 백업,
    /// 복원은 새 VM이다.
    pub target: Uuid,
```

을 다음으로 바꾼다:

```rust
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제·백업 Task는 (원본) VM, 백업 지우기는 백업,
    /// 복원과 VM 가져오기는 새 VM, 가져오기 지우기는 가져오기다.
    pub target: Uuid,
```

`crates/opl-types/src/task.rs`에서

```rust
    },
}
```

을 다음으로 바꾼다:

```rust
    },
    /// 가져오기 `import`를 `run`의 값으로 대상 id(미리 정한 새 VM id)의 새 VM으로 만든다. 가져오기의
    /// 노드(원본이 있는 노드)에서 돈다.
    VmImport {
        import: Uuid,
        run: ImportRun,
    },
    /// 대상 가져오기의 원본 디렉터리와 리소스를 지운다.
    VmImportDelete,
}
```

`crates/opl-types/src/task.rs`에서

```rust
    #[test]
    fn new_task_is_pending() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn vm_import_ops_are_tagged_by_kind() {
        let import = Uuid::now_v7();
        let run: ImportRun = serde_json::from_value(serde_json::json!({
            "name": "web01",
            "pool": "local",
            "networks": { "VM Network": "lan" }
        }))
        .unwrap();
        for (op, json) in [
            (
                TaskOp::VmImport { import, run },
                serde_json::json!({
                    "kind": "vm-import",
                    "import": import,
                    "run": {
                        "name": "web01",
                        "pool": "local",
                        "networks": { "VM Network": "lan" },
                        "disk_bus": null,
                        "nic_model": null,
                        "firmware": null,
                        "sockets": null,
                        "cores": null,
                        "memory_mib": null,
                        "keep_macs": true
                    }
                }),
            ),
            (
                TaskOp::VmImportDelete,
                serde_json::json!({ "kind": "vm-import-delete" }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&op).unwrap(), json);
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        }
    }

    #[test]
    fn new_task_is_pending() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1069개, 실패 0

Run: `cargo test -p opl-api --features storage-it --test vm_import_tasks`
Expected: PASS (qemu-img가 필요)

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 382개

- [ ] **Step 4: 커밋**

```bash
git add .github crates web
git commit -m "feat(opl-api): VM 가져오기 실행·지우기와 Task — /vm-imports/{key}/run(매핑·이름·볼륨 이름·풀·MAC·결과 스펙 검사 → importing, 새 VM id 대상 Task), DELETE /vm-imports/{key}(+force), Task VmImport(디스크마다 .mf 해시를 보며 추출 → 볼륨으로 옮김 → 꺼진 VM, 끝나면 done·원본 지움, 실패하면 볼륨 지우고 ready+error)·VmImportDelete, 가져오기 줄·시간 제한 없음, 시작 때 Task 없는 importing 되돌리기, CI storage-it에 vm_import_tasks"
```

---

### Task 5: oplctl 가져오기 명령

`oplctl import ova FILE --name N`(검사 → 실제 파일 이름과 함께 올림), `import list`(`NAME NODE SOURCE PHASE VM`), `import get N`(원본 VM·제안·디스크와 NIC 표·경고·오류), `import run N --name V --pool P [--net SOURCE=NETWORK | --net NETWORK] [--disk-bus] [--nic-model] [--firmware] [--sockets] [--cores] [--memory] [--new-macs]`(Task를 기다림, `vm V imported`), `import delete N [--force]`.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust

use crate::client::{BACKUP_WAIT, Client, MIGRATE_WAIT, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
```

을 다음으로 바꾼다:

```rust

use crate::client::{BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
```

`crates/oplctl/src/cli.rs`에서

```rust
        cmd: BackupJobCmd,
    },
```

을 다음으로 바꾼다:

```rust
        cmd: BackupJobCmd,
    },
    /// 다른 하이퍼바이저(VMware)에서 내보낸 VM(OVA) 가져오기
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
    },
```

`crates/oplctl/src/cli.rs`에서

```rust
#[derive(Subcommand)]
enum TaskCmd {
```

을 다음으로 바꾼다:

```rust
#[derive(Subcommand)]
enum ImportCmd {
    /// VMware 등에서 내보낸 OVA를 이 서버의 노드에 올리고, 원본 VM을 읽어 VM 설정을 제안받는다.
    /// 원본 파일은 그 노드에 남고 실행도 그 노드에서 한다.
    Ova {
        file: PathBuf,
        /// 가져오기 이름
        #[arg(long)]
        name: String,
    },
    List,
    /// 원본 VM, 제안, 경고, 마지막 실행의 오류
    Get {
        import: String,
    },
    /// 원본 디스크를 볼륨으로 옮겨 새 VM(꺼짐)을 만든다. 주지 않은 값은 제안 값이다.
    Run(Box<ImportRunArgs>),
    /// 가져오기와 원본 파일을 지운다(만든 VM은 남는다).
    Delete {
        import: String,
        /// 원본 노드가 없거나 오프라인이면 기록만 지운다(원본 파일은 그 노드에 남는다).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct ImportRunArgs {
    import: String,
    /// 새 VM 이름
    #[arg(long)]
    name: String,
    /// 디스크 볼륨을 만들 풀
    #[arg(long)]
    pool: String,
    /// 원본 네트워크를 opLite 네트워크에 잇는다(예: `"VM Network=lan"`). 원본 NIC의 네트워크마다
    /// 준다. 원본 네트워크가 하나면 `NETWORK`만 줘도 된다.
    #[arg(long = "net", value_name = "SOURCE=NETWORK", value_parser = parse_net)]
    nets: Vec<NetArg>,
    #[arg(long, value_enum)]
    disk_bus: Option<DiskBus>,
    #[arg(long, value_enum)]
    nic_model: Option<NicModel>,
    #[arg(long, value_enum)]
    firmware: Option<Firmware>,
    #[arg(long)]
    sockets: Option<u16>,
    #[arg(long)]
    cores: Option<u16>,
    /// 메모리(MiB)
    #[arg(long)]
    memory: Option<u32>,
    /// 원본 MAC 주소 대신 새 MAC을 받는다.
    #[arg(long)]
    new_macs: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DiskBus {
    VirtioBlk,
    Sata,
}

#[derive(Clone, Copy, ValueEnum)]
enum NicModel {
    Virtio,
    E1000e,
}

/// `--net`으로 받은 매핑. `source`가 없으면 하나뿐인 원본 네트워크다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NetArg {
    source: Option<String>,
    network: String,
}

fn parse_net(s: &str) -> Result<NetArg, String> {
    let bad = || format!("invalid network mapping {s:?}: use SOURCE=NETWORK or NETWORK");
    // 원본 네트워크 이름에 `=`가 있을 수 있다. opLite 네트워크 이름에는 없다.
    let (source, network) = match s.rsplit_once('=') {
        Some(("", _)) => return Err(bad()),
        Some((source, network)) => (Some(source.to_string()), network),
        None => (None, s),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NetArg {
        source,
        network: network.into(),
    })
}

/// 가져오기의 원본 네트워크(제안의 네트워크 매핑 키)
fn source_networks(import: &Value) -> Vec<String> {
    import["status"]["proposal"]["networks"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `import run`의 요청 본문. `sources`는 `=` 없는 `--net`을 이을 원본 네트워크다.
fn import_run_body(args: &ImportRunArgs, sources: &[String]) -> anyhow::Result<Value> {
    let mut networks = serde_json::Map::new();
    for net in &args.nets {
        let source = match (&net.source, sources) {
            (Some(source), _) => source.clone(),
            (None, [only]) => only.clone(),
            (None, _) => bail!(
                "--net {} needs SOURCE=NETWORK: vm-import {} has {} source networks ({})",
                net.network,
                args.import,
                sources.len(),
                sources
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        networks.insert(source, json!(net.network));
    }
    Ok(json!({
        "name": args.name,
        "pool": args.pool,
        "networks": networks,
        "disk_bus": args.disk_bus.map(value_name),
        "nic_model": args.nic_model.map(value_name),
        "firmware": args.firmware.map(value_name),
        "sockets": args.sockets,
        "cores": args.cores,
        "memory_mib": args.memory,
        "keep_macs": !args.new_macs,
    }))
}

#[derive(Subcommand)]
enum TaskCmd {
```

`crates/oplctl/src/cli.rs`에서

```rust
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
```

을 다음으로 바꾼다:

```rust
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Import { cmd } => import(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
```

`crates/oplctl/src/cli.rs`에서

```rust
        last,
    ]
}
```

을 다음으로 바꾼다:

```rust
        last,
    ]
}

async fn import(
    client: &Client,
    cmd: ImportCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ImportCmd::Ova { file, name } => {
            // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
            let size = tokio::fs::metadata(&file)
                .await
                .with_context(|| format!("cannot read {}", file.display()))?
                .len()
                .to_string();
            client
                .send(
                    client
                        .request(Method::POST, "/vm-imports/ova/check")
                        .query(&[("name", name.as_str()), ("size", size.as_str())]),
                )
                .await?;
            let file_name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{name}.ova"));
            let query = [("name", name.as_str()), ("file_name", file_name.as_str())];
            client.upload("/vm-imports/ova", &query, &file).await?;
            writeln!(
                out,
                "vm-import {name} uploaded; review it with `oplctl import get {name}`"
            )?;
        }
        ImportCmd::List => {
            let list = client.get("/vm-imports").await?;
            let (nodes, vms) = if json_out {
                (Vec::new(), Vec::new())
            } else {
                (
                    items(&client.get("/nodes").await?),
                    items(&client.get("/vms").await?),
                )
            };
            print_list(out, json_out, &list, IMPORT_HEADERS, |i| {
                import_row(i, &nodes, &vms)
            })?;
        }
        ImportCmd::Get { import } => {
            let import = client.get(&format!("/vm-imports/{import}")).await?;
            if json_out {
                print_json(out, &import)?;
            } else {
                let nodes = items(&client.get("/nodes").await?);
                let vms = if import["status"]["vm"].is_null() {
                    Vec::new()
                } else {
                    items(&client.get("/vms").await?)
                };
                print_import(out, &import, &nodes, &vms)?;
            }
        }
        ImportCmd::Run(args) => {
            // `=` 없는 --net이 있을 때만 원본 네트워크를 읽는다.
            let sources = if args.nets.iter().any(|n| n.source.is_none()) {
                source_networks(&client.get(&format!("/vm-imports/{}", args.import)).await?)
            } else {
                Vec::new()
            };
            let body = import_run_body(&args, &sources)?;
            let task = client
                .post(&format!("/vm-imports/{}/run", args.import), &body)
                .await?;
            client.wait_task_for(&task, IMPORT_WAIT).await?;
            writeln!(out, "vm {} imported", args.name)?;
        }
        ImportCmd::Delete { import, force } => {
            let mut r = client.request(Method::DELETE, &format!("/vm-imports/{import}"));
            if force {
                r = r.query(&[("force", "true")]);
            }
            let got = client.send(r).await?;
            // force로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
            if let Some(message) = got["message"].as_str() {
                writeln!(out, "{message}")?;
            } else {
                client.wait_task(&got).await?;
                writeln!(out, "vm-import {import} deleted")?;
            }
        }
    }
    Ok(())
}

/// 목록에서 id가 `id`인 항목의 이름. 없으면 id다.
fn name_of(list: &[Value], id: &Value) -> String {
    list.iter()
        .find(|v| v["id"] == *id)
        .map_or_else(|| cell(id), |v| cell(&v["name"]))
}

/// `import list`의 열
const IMPORT_HEADERS: &[&str] = &["NAME", "NODE", "SOURCE", "PHASE", "VM"];

/// `import list`의 한 행. SOURCE는 원본 VM 이름이고, 원본을 읽지 못했으면 올린 파일 이름이다.
fn import_row(i: &Value, nodes: &[Value], vms: &[Value]) -> Vec<String> {
    let status = &i["status"];
    let source = match status["source_vm"]["name"].as_str() {
        Some(name) => name.to_string(),
        None => cell(&i["spec"]["source"]["ova"]["file_name"]),
    };
    vec![
        cell(&i["name"]),
        name_of(nodes, &i["spec"]["node"]),
        source,
        cell(&status["phase"]),
        name_of(vms, &status["vm"]),
    ]
}

/// `import get`: 머리 줄들, 원본 디스크와 NIC 표, 경고
fn print_import(
    out: &mut (dyn Write + Send),
    i: &Value,
    nodes: &[Value],
    vms: &[Value],
) -> anyhow::Result<()> {
    let ova = &i["spec"]["source"]["ova"];
    let status = &i["status"];
    writeln!(out, "vm-import: {}", cell(&i["name"]))?;
    writeln!(out, "node: {}", name_of(nodes, &i["spec"]["node"]))?;
    writeln!(
        out,
        "source: {} ({})",
        cell(&ova["file_name"]),
        bytes(&ova["size_bytes"])
    )?;
    writeln!(out, "phase: {}", cell(&status["phase"]))?;
    if !status["vm"].is_null() {
        writeln!(out, "vm: {}", name_of(vms, &status["vm"]))?;
    }
    if let Some(error) = status["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let src = &status["source_vm"];
    if src.is_object() {
        writeln!(
            out,
            "source vm: {} ({}, {}, {} vCPUs, {} cores per socket, {} MiB)",
            cell(&src["name"]),
            cell(&src["guest_os"]),
            cell(&src["firmware"]),
            cell(&src["vcpus"]),
            cell(&src["cores_per_socket"]),
            cell(&src["memory_mib"])
        )?;
    }
    let p = &status["proposal"];
    if p.is_object() {
        writeln!(
            out,
            "proposal: {} sockets x {} cores, {} MiB, {}, {} disks, {} NICs",
            cell(&p["sockets"]),
            cell(&p["cores"]),
            cell(&p["memory_mib"]),
            cell(&p["firmware"]),
            cell(&p["disk_bus"]),
            cell(&p["nic_model"])
        )?;
    }
    if src.is_object() {
        let disks: Vec<_> = src["disks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                vec![
                    cell(&d["name"]),
                    cell(&d["file"]),
                    bytes(&d["capacity_bytes"]),
                    cell(&d["controller"]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["DISK", "FILE", "SIZE", "CONTROLLER"], &disks)?;
        // PROPOSED는 이름이 같아 제안한 opLite 네트워크다.
        let nics: Vec<_> = src["nics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                vec![
                    cell(&n["network"]),
                    cell(&n["adapter"]),
                    cell(&n["mac"]),
                    cell(&p["networks"][n["network"].as_str().unwrap_or_default()]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["NETWORK", "ADAPTER", "MAC", "PROPOSED"], &nics)?;
    }
    if let Some(warnings) = status["warnings"].as_array().filter(|w| !w.is_empty()) {
        writeln!(out, "\nwarnings:")?;
        for w in warnings {
            writeln!(out, "  {}", cell(w))?;
        }
    }
    Ok(())
}
```

`crates/oplctl/src/cli.rs`에서

```rust
    }

    #[test]
    fn cli_definition_is_valid() {
```

을 다음으로 바꾼다:

```rust
    }

    /// `import` 명령의 인자
    fn parse_import(args: &[&str]) -> Result<ImportCmd, clap::Error> {
        let mut argv = vec!["oplctl", "import"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::Import { cmd } => cmd,
            _ => unreachable!(),
        })
    }

    /// `import run`의 인자를 요청 본문으로. `sources`는 가져오기의 원본 네트워크다.
    fn import_run(args: &[&str], sources: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["run", "web01"];
        argv.extend_from_slice(args);
        let ImportCmd::Run(run) = parse_import(&argv)? else {
            unreachable!()
        };
        let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
        import_run_body(&run, &sources)
    }

    #[test]
    fn parses_import_commands() {
        assert!(matches!(
            parse_import(&["ova", "/tmp/web01.ova", "--name", "web01"]).unwrap(),
            ImportCmd::Ova { file, name } if file == std::path::Path::new("/tmp/web01.ova") && name == "web01"
        ));
        assert!(matches!(parse_import(&["list"]).unwrap(), ImportCmd::List));
        assert!(matches!(
            parse_import(&["get", "web01"]).unwrap(),
            ImportCmd::Get { import } if import == "web01"
        ));
        assert!(matches!(
            parse_import(&["delete", "web01", "--force"]).unwrap(),
            ImportCmd::Delete { import, force: true } if import == "web01"
        ));
        // 가져오기 이름, 새 VM 이름과 풀은 꼭 준다.
        for args in [
            &["ova", "/tmp/web01.ova"][..],
            &["run", "web01", "--pool", "local"],
            &["run", "web01", "--name", "web"],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--disk-bus",
                "ide",
            ],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--net",
                "VM Network=",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "=lan",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "",
            ],
        ] {
            assert!(parse_import(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn import_run_builds_the_body() {
        // 주지 않은 값은 비워 두어 서버가 제안 값을 쓴다.
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "VM Network=lan",
                    "--net",
                    "DMZ=dmz",
                    "--net",
                    "a=b=lan"
                ],
                &["DMZ", "VM Network", "a=b"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan", "DMZ": "dmz", "a=b": "lan" },
                "disk_bus": null, "nic_model": null, "firmware": null,
                "sockets": null, "cores": null, "memory_mib": null,
                "keep_macs": true
            })
        );
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "lan",
                    "--disk-bus",
                    "sata",
                    "--nic-model",
                    "e1000e",
                    "--firmware",
                    "bios",
                    "--sockets",
                    "2",
                    "--cores",
                    "4",
                    "--memory",
                    "8192",
                    "--new-macs"
                ],
                &["VM Network"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan" },
                "disk_bus": "sata", "nic_model": "e1000e", "firmware": "bios",
                "sockets": 2, "cores": 4, "memory_mib": 8192,
                "keep_macs": false
            })
        );
        assert_eq!(
            import_run(
                &["--name", "web", "--pool", "p", "--disk-bus", "virtio-blk"],
                &[]
            )
            .unwrap()["disk_bus"],
            "virtio-blk"
        );
        // `=` 없는 --net은 원본 네트워크가 하나일 때만 쓴다.
        let err = import_run(
            &["--name", "web", "--pool", "p", "--net", "lan"],
            &["A", "B"],
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 2 source networks (\"A\", \"B\")"
        );
        let err = import_run(&["--name", "web", "--pool", "p", "--net", "lan"], &[]);
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 0 source networks ()"
        );
    }

    fn sample_import() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "web01",
            "spec": {
                "node": "n-1",
                "source": { "ova": { "file_name": "web01.ova", "size_bytes": 3u64 << 29 } }
            },
            "status": {
                "phase": "ready",
                "source_vm": {
                    "name": "web01-src", "guest_os": "otherLinux64Guest", "firmware": "uefi",
                    "vcpus": 2, "cores_per_socket": 2, "memory_mib": 1024,
                    "disks": [
                        { "name": "disk0", "file": "web01-disk1.vmdk", "capacity_bytes": 10u64 << 30, "controller": "scsi" }
                    ],
                    "nics": [
                        { "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" },
                        { "network": "DMZ", "adapter": "e1000" }
                    ]
                },
                "proposal": {
                    "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
                    "disk_bus": "virtio-blk", "nic_model": "virtio",
                    "networks": { "DMZ": null, "VM Network": "VM Network" }
                },
                "warnings": ["first warning", "second warning"],
                "error": "checksum mismatch for web01-disk1.vmdk"
            }
        })
    }

    #[test]
    fn source_networks_are_the_proposal_keys() {
        assert_eq!(source_networks(&sample_import()), ["DMZ", "VM Network"]);
        assert!(source_networks(&json!({ "status": {} })).is_empty());
    }

    #[test]
    fn import_list_shows_node_source_and_vm_names() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let vms = [json!({ "id": "v-1", "name": "web" })];
        assert_eq!(
            import_row(&sample_import(), &nodes, &vms),
            ["web01", "node1", "web01-src", "ready", "-"]
        );
        // 만든 VM은 이름으로, 지운 VM·빠진 노드는 id로 보인다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        assert_eq!(import_row(&done, &nodes, &vms)[3..], ["done", "web"]);
        assert_eq!(
            import_row(&done, &[], &[])[1..],
            ["n-1", "web01-src", "done", "v-1"]
        );
        // 원본을 읽지 못한 가져오기는 파일 이름을 보인다.
        let mut failed = sample_import();
        failed["status"] = json!({ "phase": "failed" });
        assert_eq!(import_row(&failed, &nodes, &vms)[2], "web01.ova");
    }

    #[test]
    fn import_get_shows_the_source_proposal_and_warnings() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let mut out = Vec::new();
        print_import(&mut out, &sample_import(), &nodes, &[]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
vm-import: web01
node: node1
source: web01.ova (1.5GiB)
phase: ready
error: checksum mismatch for web01-disk1.vmdk
source vm: web01-src (otherLinux64Guest, uefi, 2 vCPUs, 2 cores per socket, 1024 MiB)
proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs

DISK   FILE              SIZE     CONTROLLER
disk0  web01-disk1.vmdk  10.0GiB  scsi

NETWORK     ADAPTER  MAC                PROPOSED
VM Network  vmxnet3  00:50:56:aa:bb:cc  VM Network
DMZ         e1000    -                  -

warnings:
  first warning
  second warning
"
        );
        // 끝난 가져오기는 만든 VM을, 경고가 없으면 경고 줄을 뺀다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        done["status"]["warnings"] = json!([]);
        done["status"]["error"] = Value::Null;
        let mut out = Vec::new();
        print_import(
            &mut out,
            &done,
            &nodes,
            &[json!({ "id": "v-1", "name": "web" })],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("phase: done\nvm: web\nsource vm: "), "{text}");
        assert!(
            !text.contains("warnings") && !text.contains("error"),
            "{text}"
        );
    }

    /// `import ova`는 크기로 먼저 확인하고 실제 파일 이름과 함께 올린다.
    #[tokio::test]
    async fn import_ova_checks_then_uploads_with_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        // 본문이 없어야 작은 시험 서버가 요청 줄만 읽고 답할 수 있다.
        let file = dir.path().join("web01-export.ova");
        std::fs::write(&file, b"").unwrap();
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let cmd = ImportCmd::Ova {
            file,
            name: "web01".into(),
        };
        let mut out = Vec::new();
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/vm-imports/ova/check?name=web01&size=0 HTTP/1.1",
                "POST /api/v1/vm-imports/ova?name=web01&file_name=web01-export.ova HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 uploaded; review it with `oplctl import get web01`\n"
        );
    }

    /// `=` 없는 `--net`이면 가져오기를 읽어 원본 네트워크를 찾고, 실행 Task를 기다린다.
    #[tokio::test]
    async fn import_run_reads_the_import_for_a_bare_net_and_waits() {
        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else if path.ends_with("/run") {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            } else {
                json!({ "status": { "proposal": { "networks": { "VM Network": null } } } })
            }
        })
        .await;
        let run = |args: &[&str]| {
            let mut argv = vec![
                "oplctl", "import", "run", "web01", "--name", "web", "--pool", "p",
            ];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Import { cmd } => cmd,
                _ => unreachable!(),
            }
        };
        let mut out = Vec::new();
        import(&client, run(&["--net", "lan"]), false, &mut out)
            .await
            .unwrap();
        // SOURCE=NETWORK만 주면 읽지 않는다.
        import(&client, run(&["--net", "VM Network=lan"]), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/vm-imports/web01 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm web imported\nvm web imported\n"
        );
    }

    /// `import delete --force`는 `?force=true`로 보내고 서버의 알림을 보인다. 아니면 Task를 기다린다.
    #[tokio::test]
    async fn import_delete_force_shows_the_server_message() {
        let (client, seen) = fake_api(|_| {
            json!({ "message": "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)" })
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: true,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/vm-imports/web01?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: false,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/vm-imports/web01 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "vm-import web01 deleted\n");
    }

    #[test]
    fn cli_definition_is_valid() {
```

`crates/oplctl/src/client.rs`에서

```rust
pub const BACKUP_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

을 다음으로 바꾼다:

```rust
pub const BACKUP_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// VM 가져오기 Task를 기다리는 시간. 디스크를 모두 옮기므로 Task 실행기는 시간 제한을 두지 않는다.
pub const IMPORT_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1077개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): VM 가져오기 명령 — import ova(크기로 먼저 확인하고 실제 파일 이름과 함께 올림)·list(NAME NODE SOURCE PHASE VM)·get(원본 VM·제안·디스크와 NIC 표·경고·오류)·run(--net SOURCE=NETWORK, 원본 네트워크가 하나면 NETWORK만, 제안을 고치는 선택 인자와 --new-macs, Task를 기다림)·delete(--force면 서버 알림)"
```

---

### Task 6: 웹 가져오기 화면

사이드바 "가져오기"(VM 아래), `/imports`(목록, OVA 올리기 — 파일 이름에서 이름 제안, 사전 확인 후 진행률 알림), `/imports/:name`(원본 요약·디스크와 NIC 표·경고·오류, 제안으로 채운 실행 폼과 원본 네트워크마다 매핑, 실패 뒤에도 입력 유지, Task 추적, 지우기). 이미지 올리기의 XHR·진행률 알림을 함께 쓰게 나눈다. watch 종류·권한·Task 이름, en·ko.

**Files:**
- Create: `web/src/imports/ImportPage.test.tsx`
- Create: `web/src/imports/ImportPage.tsx`
- Create: `web/src/imports/ImportsPage.test.tsx`
- Create: `web/src/imports/ImportsPage.tsx`
- Create: `web/src/imports/imports.test.ts`
- Create: `web/src/imports/imports.ts`
- Create: `web/src/imports/ova.test.tsx`
- Create: `web/src/imports/ova.ts`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/api/LiveProvider.test.tsx`
- Modify: `web/src/api/live.ts`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/auth/permissions.ts`
- Modify: `web/src/components/Field.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/storage/upload.ts`
- Modify: `web/src/tasks/TasksPage.test.tsx`
- Modify: `web/src/tasks/TasksPage.tsx`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/test/render.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.test.tsx`에서

```tsx
      "/backup-jobs",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
```

을 다음으로 바꾼다:

```tsx
      "/backup-jobs",
      "/vm-imports",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
```

`web/src/App.tsx`에서

```tsx
import { Toaster } from "@/components/ui/sonner";
import { AppLayout } from "@/layout/AppLayout";
```

을 다음으로 바꾼다:

```tsx
import { Toaster } from "@/components/ui/sonner";
import { ImportPage } from "@/imports/ImportPage";
import { ImportsPage } from "@/imports/ImportsPage";
import { AppLayout } from "@/layout/AppLayout";
```

`web/src/App.tsx`에서

```tsx
            { path: "vms/:name/console", element: <ConsolePage /> },
            { path: "volumes", element: <VolumesPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "vms/:name/console", element: <ConsolePage /> },
            { path: "imports", element: <ImportsPage /> },
            { path: "imports/:name", element: <ImportPage /> },
            { path: "volumes", element: <VolumesPage /> },
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
    http.get(apiUrl("/backup-jobs"), empty),
  ];
```

을 다음으로 바꾼다:

```tsx
    http.get(apiUrl("/backup-jobs"), empty),
    http.get(apiUrl("/vm-imports"), empty),
  ];
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group,backup,backup-job&since=7",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group,backup,backup-job,vm-import&since=7",
  );
```

`web/src/api/live.ts`에서

```ts
  "backup-job",
];
```

을 다음으로 바꾼다:

```ts
  "backup-job",
  "vm-import",
];
```

`web/src/api/live.ts`에서

```ts
        return call(api.GET("/backup-jobs"));
    }
```

을 다음으로 바꾼다:

```ts
        return call(api.GET("/backup-jobs"));
      case "vm-import":
        return call(api.GET("/vm-imports"));
    }
```

`web/src/api/types.ts`에서

```ts
export type BackupJob = Schemas["Resource_BackupJobSpec_BackupJobStatus"];
export type Weekday = Schemas["Weekday"];
```

을 다음으로 바꾼다:

```ts
export type BackupJob = Schemas["Resource_BackupJobSpec_BackupJobStatus"];
export type VmImport = Schemas["Resource_VmImportSpec_VmImportStatus"];
export type ImportRun = Schemas["ImportRun"];
export type Weekday = Schemas["Weekday"];
```

`web/src/api/types.ts`에서

```ts
  | "backup"
  | "backup-job";
```

을 다음으로 바꾼다:

```ts
  | "backup"
  | "backup-job"
  | "vm-import";
```

`web/src/api/types.ts`에서

```ts
  "backup-job": BackupJob;
}
```

을 다음으로 바꾼다:

```ts
  "backup-job": BackupJob;
  "vm-import": VmImport;
}
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("operator", "write", "backup-job")).toBe(true);
});
```

을 다음으로 바꾼다:

```ts
  expect(can("operator", "write", "backup-job")).toBe(true);
  // VM 가져오기도 같다.
  expect(can("viewer", "read", "vm-import")).toBe(true);
  expect(can("viewer", "write", "vm-import")).toBe(false);
  expect(can("operator", "write", "vm-import")).toBe(true);
});
```

`web/src/auth/permissions.ts`에서

```ts
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨, 백업과 백업 작업만 바꾸고, viewer는 읽기만 한다. 둘 다 계정은 읽지 못한다.
 */
```

을 다음으로 바꾼다:

```ts
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨, 백업과 백업 작업, VM 가져오기만 바꾸고, viewer는 읽기만 한다. 둘 다 계정은 읽지 못한다.
 */
```

`web/src/auth/permissions.ts`에서

```ts
        ? !account
        : kind === "vm" || kind === "volume" || kind === "backup" || kind === "backup-job";
    case "viewer":
```

을 다음으로 바꾼다:

```ts
        ? !account
        : kind === "vm" ||
            kind === "volume" ||
            kind === "backup" ||
            kind === "backup-job" ||
            kind === "vm-import";
    case "viewer":
```

`web/src/components/Field.tsx`에서

```tsx
/**
 * 이름표와 입력, 그 아래의 오류(번역 키)와 도움말. 오류나 도움말이 있는 입력은 `children`에 함수를
 * 주어 `FieldA11y`를 받는다.
```

을 다음으로 바꾼다:

```tsx
/**
 * 이름표와 입력, 그 아래의 오류(번역 키, 끼울 값은 `errorValues`)와 도움말. 오류나 도움말이 있는 입력은 `children`에 함수를
 * 주어 `FieldA11y`를 받는다.
```

`web/src/components/Field.tsx`에서

```tsx
  error,
  hint,
```

을 다음으로 바꾼다:

```tsx
  error,
  errorValues,
  hint,
```

`web/src/components/Field.tsx`에서

```tsx
  error?: string;
  hint?: string;
```

을 다음으로 바꾼다:

```tsx
  error?: string;
  errorValues?: Record<string, string>;
  hint?: string;
```

`web/src/components/Field.tsx`에서

```tsx
        <p id={errorId} className="text-xs text-destructive">
          {t(error)}
        </p>
```

을 다음으로 바꾼다:

```tsx
        <p id={errorId} className="text-xs text-destructive">
          {t(error, errorValues)}
        </p>
```

`web/src/i18n/en.json`에서

```json
    "job_running": "The backup job is already running. Wait for the run to finish.",
    "job_disabled": "The backup job is off. Turn on \"Run on schedule\" first."
  },
```

을 다음으로 바꾼다:

```json
    "job_running": "The backup job is already running. Wait for the run to finish.",
    "job_disabled": "The backup job is off. Turn on \"Run on schedule\" first.",
    "invalid_ova": "The file is not a usable OVA: {{message}}",
    "import_busy": "The import is running, or another task on it has not finished.",
    "import_done": "The VM has already been imported.",
    "import_failed": "The import cannot run: {{message}}"
  },
```

`web/src/i18n/en.json`에서

```json
    "vms": "Virtual machines",
    "volumes": "Volumes",
```

을 다음으로 바꾼다:

```json
    "vms": "Virtual machines",
    "imports": "Imports",
    "volumes": "Volumes",
```

`web/src/i18n/en.json`에서

```json
      "backup-delete": "Delete backup {{target}}",
      "backup-restore": "Restore a backup as {{target}}"
    }
```

을 다음으로 바꾼다:

```json
      "backup-delete": "Delete backup {{target}}",
      "backup-restore": "Restore a backup as {{target}}",
      "vm-import": "Import VM {{target}}",
      "vm-import-delete": "Delete import {{target}}"
    }
```

`web/src/i18n/en.json`에서

```json
      "keep": "Enter whole numbers of 0 or more."
    }
```

을 다음으로 바꾼다:

```json
      "keep": "Enter whole numbers of 0 or more."
    }
  },
  "imports": {
    "pageHint": "Bring VMware VMs into opLite from OVA files. Upload an OVA, review the settings proposed from the source VM, then run the import to create a stopped VM.",
    "empty": "No imports.",
    "upload": "Upload OVA",
    "uploadTitle": "Upload an OVA",
    "uploadDescription": "The file goes to the node you are connected to ({{node}}) and stays there until the import is done or deleted. The import runs on that node.",
    "file": "OVA file",
    "uploaded": "Uploaded {{name}}. Review the proposed settings and run the import.",
    "review": "Review",
    "noResponse": "{{name}}: no response from the server. Check the import list for the result.",
    "sourceVm": "Source VM",
    "vm": "VM",
    "vmGone": "Deleted VM",
    "phase": {
      "ready": "Ready",
      "importing": "Importing",
      "done": "Done",
      "failed": "Failed"
    },
    "notFound": "Import {{name}} not found",
    "source": "Source",
    "fileName": "File",
    "guestOs": "Guest OS",
    "cpuValue": "{{vcpus}} vCPU, {{cores}} per socket",
    "diskFile": "File in the OVA",
    "capacity": "Size",
    "controller": "Controller",
    "adapter": "Adapter",
    "warnings": "Warnings",
    "lastError": "The last run failed: {{error}}",
    "failedError": "The source cannot be used: {{error}}",
    "importing": "Importing. The progress is on the Tasks page.",
    "imported": "Imported as VM",
    "settings": "VM settings",
    "settingsHint": "Proposed from the source VM. Change what you need.",
    "vmName": "New VM name",
    "pool": "Storage pool for the disks",
    "poolHint": "Only pools usable on {{node}}, where the OVA is, are listed.",
    "sockets": "Sockets",
    "cores": "Cores per socket",
    "diskBus": "Disk bus",
    "nicModel": "NIC model",
    "networks": "Network mapping",
    "networksHint": "Choose an opLite network for each network the source VM uses.",
    "chooseNetwork": "Choose a network",
    "keepMacs": "Keep the source MAC addresses",
    "keepMacsHint": "Power off the VMware VM before starting the imported one. Turn this off to give the interfaces new MAC addresses.",
    "run": "Import",
    "deleteNamed": "Delete import {{name}}",
    "deleteTitle": "Delete import {{name}}?",
    "deleteDescription": "Deletes the uploaded OVA from {{node}}. A VM already imported from it is kept.",
    "forceHint": "{{error}} To forget the import and leave its files on that node, run oplctl import delete {{name}} --force.",
    "codes": {
      "node_offline": "The node holding the OVA is offline or has left the cluster.",
      "mac_in_use": "Another VM uses a source MAC address. Turn off \"Keep the source MAC addresses\" to give the interfaces new MAC addresses."
    },
    "errors": {
      "reserved": "The name ova is reserved.",
      "exists": "An import with this name already exists.",
      "vmExists": "A VM with this name already exists.",
      "volumeExists": "A volume this import would create (NAME-disk0, …) already exists.",
      "pool": "Choose a storage pool.",
      "network": "Choose a network.",
      "poolNotOnNode": "This storage pool is not available on the node holding the OVA.",
      "poolUnavailable": "This storage pool is not ready on the node holding the OVA.",
      "sockets": "Enter 1 to 65535 sockets.",
      "nameTaken": "Name in use: {{message}}"
    }
```

`web/src/i18n/i18n.test.ts`에서

```ts
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

을 다음으로 바꾼다:

```ts
  );
  expect(errorMessage(t, new ApiError(400, "invalid_ova", "the OVA has no .ovf file"))).toBe(
    "The file is not a usable OVA: the OVA has no .ovf file",
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

`web/src/i18n/ko.json`에서

```json
    "job_running": "백업 작업이 이미 실행 중입니다. 실행이 끝난 뒤 다시 시도하세요.",
    "job_disabled": "꺼 둔 백업 작업입니다. 먼저 \"일정대로 실행\"을 켜세요."
  },
```

을 다음으로 바꾼다:

```json
    "job_running": "백업 작업이 이미 실행 중입니다. 실행이 끝난 뒤 다시 시도하세요.",
    "job_disabled": "꺼 둔 백업 작업입니다. 먼저 \"일정대로 실행\"을 켜세요.",
    "invalid_ova": "쓸 수 있는 OVA가 아닙니다: {{message}}",
    "import_busy": "가져오는 중이거나 이 가져오기의 다른 작업이 끝나지 않았습니다.",
    "import_done": "이미 가져온 VM입니다.",
    "import_failed": "가져오기를 실행할 수 없습니다: {{message}}"
  },
```

`web/src/i18n/ko.json`에서

```json
    "vms": "가상 머신",
    "volumes": "볼륨",
```

을 다음으로 바꾼다:

```json
    "vms": "가상 머신",
    "imports": "가져오기",
    "volumes": "볼륨",
```

`web/src/i18n/ko.json`에서

```json
      "backup-delete": "백업 {{target}} 삭제",
      "backup-restore": "백업을 {{target}}(으)로 복원"
    }
```

을 다음으로 바꾼다:

```json
      "backup-delete": "백업 {{target}} 삭제",
      "backup-restore": "백업을 {{target}}(으)로 복원",
      "vm-import": "VM {{target}} 가져오기",
      "vm-import-delete": "가져오기 {{target}} 삭제"
    }
```

`web/src/i18n/ko.json`에서

```json
      "keep": "0 이상의 정수를 적으세요."
    }
```

을 다음으로 바꾼다:

```json
      "keep": "0 이상의 정수를 적으세요."
    }
  },
  "imports": {
    "pageHint": "OVA 파일로 VMware VM을 opLite로 가져옵니다. OVA를 올리고 원본 VM에서 제안한 설정을 확인한 뒤 실행하면 꺼진 VM이 만들어집니다.",
    "empty": "가져오기가 없습니다.",
    "upload": "OVA 올리기",
    "uploadTitle": "OVA 올리기",
    "uploadDescription": "파일은 지금 접속한 노드({{node}})로 올라가고 가져오기를 마치거나 지울 때까지 그 노드에 남습니다. 가져오기도 그 노드에서 돕니다.",
    "file": "OVA 파일",
    "uploaded": "{{name}}을(를) 올렸습니다. 제안한 설정을 확인하고 가져오기를 실행하세요.",
    "review": "확인",
    "noResponse": "{{name}}: 서버 응답이 없습니다. 가져오기 목록에서 결과를 확인하세요.",
    "sourceVm": "원본 VM",
    "vm": "VM",
    "vmGone": "삭제한 VM",
    "phase": {
      "ready": "준비됨",
      "importing": "가져오는 중",
      "done": "완료",
      "failed": "실패"
    },
    "notFound": "가져오기 {{name}}을(를) 찾을 수 없습니다",
    "source": "원본",
    "fileName": "파일",
    "guestOs": "게스트 OS",
    "cpuValue": "vCPU {{vcpus}}개, 소켓당 코어 {{cores}}개",
    "diskFile": "OVA 안의 파일",
    "capacity": "크기",
    "controller": "컨트롤러",
    "adapter": "어댑터",
    "warnings": "경고",
    "lastError": "마지막 실행이 실패했습니다: {{error}}",
    "failedError": "원본을 쓸 수 없습니다: {{error}}",
    "importing": "가져오는 중입니다. 진행 상황은 작업 화면에 있습니다.",
    "imported": "가져온 VM",
    "settings": "VM 설정",
    "settingsHint": "원본 VM에서 제안한 값입니다. 필요한 것을 고치세요.",
    "vmName": "새 VM 이름",
    "pool": "디스크를 둘 스토리지 풀",
    "poolHint": "OVA가 있는 {{node}}에서 쓸 수 있는 풀만 보입니다.",
    "sockets": "소켓",
    "cores": "소켓당 코어",
    "diskBus": "디스크 버스",
    "nicModel": "NIC 모델",
    "networks": "네트워크 매핑",
    "networksHint": "원본 VM이 쓰는 네트워크마다 opLite 네트워크를 고르세요.",
    "chooseNetwork": "네트워크를 고르세요",
    "keepMacs": "원본 MAC 주소 유지",
    "keepMacsHint": "가져온 VM을 켜기 전에 VMware의 원본 VM을 끄세요. 끄면 인터페이스가 새 MAC 주소를 받습니다.",
    "run": "가져오기",
    "deleteNamed": "가져오기 {{name}} 삭제",
    "deleteTitle": "가져오기 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "{{node}}에 올린 OVA를 지웁니다. 이미 가져온 VM은 남습니다.",
    "forceHint": "{{error}} 파일을 그 노드에 남기고 가져오기만 지우려면 oplctl import delete {{name}} --force를 실행하세요.",
    "codes": {
      "node_offline": "OVA가 있는 노드가 오프라인이거나 클러스터에서 빠졌습니다.",
      "mac_in_use": "다른 VM이 원본 MAC 주소를 쓰고 있습니다. \"원본 MAC 주소 유지\"를 끄면 새 MAC 주소를 받습니다."
    },
    "errors": {
      "reserved": "ova는 예약된 이름입니다.",
      "exists": "같은 이름의 가져오기가 이미 있습니다.",
      "vmExists": "같은 이름의 VM이 이미 있습니다.",
      "volumeExists": "가져오기가 만들 볼륨(이름-disk0, …)이 이미 있습니다.",
      "pool": "스토리지 풀을 고르세요.",
      "network": "네트워크를 고르세요.",
      "poolNotOnNode": "OVA가 있는 노드에서 쓸 수 없는 스토리지 풀입니다.",
      "poolUnavailable": "OVA가 있는 노드에서 준비되지 않은 스토리지 풀입니다.",
      "sockets": "소켓 수는 1에서 65535 사이여야 합니다.",
      "nameTaken": "이미 쓰는 이름입니다: {{message}}"
    }
```

`web/src/imports/ImportPage.test.tsx`를 만든다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Network, VmImport } from "@/api/types";
import { ImportPage } from "@/imports/ImportPage";
import { NODE, pool, task, vm, vmImport, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const lan = { id: "019a0f3c-0000-7000-8004-000000000001", name: "lan" } as Network;

/** 원본 NIC가 둘이고 그중 "Backup"은 이름이 같은 opLite 네트워크가 없는 가져오기 */
const web01 = vmImport("web01", 1, {
  source_vm: {
    ...vmImport("web01", 1).status.source_vm!,
    nics: [
      { network: "VM Network", adapter: "vmxnet3", mac: "00:50:56:01:02:03" },
      { network: "Backup", adapter: "e1000", mac: null },
    ],
  },
  proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  error: "checksum mismatch for web01-disk1.vmdk",
});

function open(imp: VmImport, as = ADMIN) {
  return renderPage(<ImportPage />, { path: "/imports/:name", url: `/imports/${imp.name}`, as });
}

it("shows the source VM, its warnings and the last error", async () => {
  mockLists({ "vm-import": [web01], node: [NODE] });
  open(web01);
  expect(await screen.findByRole("heading", { name: "web01" })).toBeInTheDocument();
  expect(screen.getByText("Ready")).toBeInTheDocument();
  // 페이지에 남는 오류라 알림(alert)이 아니다.
  expect(
    screen.getByText("The last run failed: checksum mismatch for web01-disk1.vmdk"),
  ).toHaveAttribute("role", "status");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(await screen.findByText("web01.ova (3 GiB), node-a")).toBeInTheDocument();
  expect(screen.getByText("Web 01")).toBeInTheDocument();
  expect(screen.getByText("2 vCPU, 2 per socket")).toBeInTheDocument();
  const disks = screen.getByRole("table", { name: "Disks" });
  expect(within(disks).getAllByRole("row")[1]).toHaveTextContent("disk0web01-disk1.vmdk16 GiBscsi");
  const nics = screen.getByRole("table", { name: "Network interfaces" });
  expect(within(nics).getAllByRole("row")[1]).toHaveTextContent(
    "VM Networkvmxnet300:50:56:01:02:03",
  );
  expect(within(nics).getAllByRole("row")[2]).toHaveTextContent("Backupe1000—");
  expect(screen.getByRole("region", { name: "Warnings" })).toHaveTextContent(
    "UEFI boot entries (NVRAM) are not imported",
  );
});

it("runs the import with the reviewed settings after every network is mapped", async () => {
  mockLists({
    "vm-import": [web01],
    node: [NODE],
    network: [lan],
    "storage-pool": [pool("local"), pool("far", { nodes: ["n2"] })],
    vm: [vm("web01", "stopped")],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), async ({ request, params }) => {
      expect(params.key).toBe(web01.id);
      body = await request.json();
      return HttpResponse.json(task("t1", "vm-import", "new-vm"), { status: 202 });
    }),
  );
  open(web01);
  // 제안으로 채운다. VM 이름은 가져오기 이름이다.
  expect(await screen.findByLabelText("New VM name")).toHaveValue("web01");
  expect(screen.getByLabelText("Sockets")).toHaveValue("1");
  expect(screen.getByLabelText("Cores per socket")).toHaveValue("2");
  expect(screen.getByLabelText("Memory (MiB)")).toHaveValue("1024");
  expect(screen.getByLabelText("Firmware")).toHaveValue("uefi");
  expect(screen.getByLabelText("Disk bus")).toHaveValue("virtio-blk");
  expect(screen.getByLabelText("NIC model")).toHaveValue("virtio");
  // 네트워크 목록을 읽어야 고른 네트워크가 보인다.
  await screen.findAllByRole("option", { name: "lan" });
  expect(screen.getByLabelText("VM Network")).toHaveValue("lan");
  expect(screen.getByLabelText("Backup")).toHaveValue("");
  expect(screen.getByLabelText("Keep the source MAC addresses")).toBeChecked();
  // OVA가 있는 노드(n1)에서 쓸 수 있는 풀만 고른다.
  await screen.findByRole("option", { name: "local" });
  expect(
    within(screen.getByLabelText("Storage pool for the disks"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["—", "local"]);

  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(screen.getByText("A VM with this name already exists.")).toBeVisible();
  expect(screen.getByText("Choose a storage pool.")).toBeVisible();
  expect(screen.getByText("Choose a network.")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(screen.getByLabelText("New VM name"));
  await userEvent.type(screen.getByLabelText("New VM name"), "web");
  await userEvent.selectOptions(screen.getByLabelText("Storage pool for the disks"), "local");
  await userEvent.selectOptions(screen.getByLabelText("Backup"), "user");
  await userEvent.clear(screen.getByLabelText("Memory (MiB)"));
  await userEvent.type(screen.getByLabelText("Memory (MiB)"), "2048");
  await userEvent.selectOptions(screen.getByLabelText("Disk bus"), "sata");
  await userEvent.click(screen.getByLabelText("Keep the source MAC addresses"));
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByText("Import VM web")).toBeInTheDocument();
  expect(body).toEqual({
    name: "web",
    pool: "local",
    networks: { "VM Network": "lan", Backup: "user" },
    disk_bus: "sata",
    nic_model: "virtio",
    firmware: "uefi",
    sockets: 1,
    cores: 2,
    memory_mib: 2048,
    keep_macs: false,
  });
});

it("checks the volume names the import will create", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({
    "vm-import": [imp],
    node: [NODE],
    network: [lan],
    "storage-pool": [pool("local")],
    volume: [volume("web01-disk0", "local")],
  });
  open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(
    screen.getByText("A volume this import would create (NAME-disk0, …) already exists."),
  ).toBeVisible();
});

it("explains server errors in terms of the node holding the OVA", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({ "vm-import": [imp], node: [NODE], network: [lan], "storage-pool": [pool("local")] });
  let code = "pool_not_on_node";
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), () =>
      HttpResponse.json({ code, message: "nope" }, { status: 409 }),
    ),
  );
  open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(
    await screen.findByText("This storage pool is not available on the node holding the OVA."),
  ).toBeVisible();
  code = "node_offline";
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The node holding the OVA is offline or has left the cluster.",
  );
});

it("keeps the form after a failed run and does not send it twice", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({ "vm-import": [imp], node: [NODE], network: [lan], "storage-pool": [pool("local")] });
  let sent = 0;
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), () => {
      sent += 1;
      return HttpResponse.json(task("t1", "vm-import", "new-vm"), { status: 202 });
    }),
  );
  const { queryClient } = open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.clear(screen.getByLabelText("New VM name"));
  await userEvent.type(screen.getByLabelText("New VM name"), "web");
  await userEvent.clear(screen.getByLabelText("Memory (MiB)"));
  await userEvent.type(screen.getByLabelText("Memory (MiB)"), "2048");
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByText("Import VM web")).toBeInTheDocument();
  // 가져오기가 바뀔 때까지 다시 보내거나 지우지 못한다.
  expect(screen.getByRole("button", { name: "Import" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Delete import web01" })).toBeDisabled();
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(sent).toBe(1);

  const update = (revision: number, status: Partial<VmImport["status"]>) =>
    queryClient.setQueryData(["list", "vm-import"], {
      revision,
      items: [{ ...imp, revision, status: { ...imp.status, ...status } }],
    });
  update(2, { phase: "importing" });
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument(),
  );
  // 실패해 `ready`로 돌아오면 고친 값이 그대로 있고 다시 보낼 수 있다.
  update(3, { phase: "ready", error: "cannot create VM" });
  expect(await screen.findByLabelText("New VM name")).toHaveValue("web");
  expect(screen.getByLabelText("Memory (MiB)")).toHaveValue("2048");
  expect(screen.getByLabelText("Storage pool for the disks")).toHaveValue("local");
  expect(screen.getByRole("button", { name: "Import" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Delete import web01" })).toBeEnabled();
});

it("shows taken names on the name field and explains MAC conflicts", async () => {
  const imp = vmImport("web01", 1, {
    proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
  });
  mockLists({ "vm-import": [imp], node: [NODE], network: [lan], "storage-pool": [pool("local")] });
  let reply = { code: "name_taken", message: 'volume "web01-disk0" already exists' };
  server.use(
    http.post(apiUrl("/vm-imports/:key/run"), () => HttpResponse.json(reply, { status: 409 })),
  );
  open(imp);
  await userEvent.selectOptions(
    await screen.findByLabelText("Storage pool for the disks"),
    await screen.findByRole("option", { name: "local" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  // 서버 메시지는 번역 키가 아니라 정해진 문장에 끼운다.
  expect(await screen.findByText('Name in use: volume "web01-disk0" already exists')).toBeVisible();
  expect(screen.getByLabelText("New VM name")).toHaveAccessibleDescription(
    'Name in use: volume "web01-disk0" already exists',
  );
  // 실패하면 다시 보낼 수 있다.
  expect(screen.getByRole("button", { name: "Import" })).toBeEnabled();
  reply = { code: "mac_in_use", message: "MAC address 00:50:56:01:02:03 is already in use" };
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    'Another VM uses a source MAC address. Turn off "Keep the source MAC addresses" to give the interfaces new MAC addresses.',
  );
});

it("links the imported VM and offers no form once done or while importing", async () => {
  const web = vm("web", "stopped");
  const done = vmImport("web01", 1, { phase: "done", vm: web.id, warnings: [] });
  const busy = vmImport("db01", 2, { phase: "importing" });
  mockLists({ "vm-import": [done, busy], vm: [web] });
  open(done);
  expect(await screen.findByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(screen.getByText("Done")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument();
  expect(screen.queryByRole("region", { name: "Warnings" })).not.toBeInTheDocument();
});

it("cannot run while importing and lets the server decide on deleting", async () => {
  const busy = vmImport("db01", 2, { phase: "importing" });
  mockLists({ "vm-import": [busy, web01], node: [NODE] });
  server.use(
    http.delete(apiUrl("/vm-imports/:key"), () =>
      HttpResponse.json({ code: "import_busy", message: "busy" }, { status: 409 }),
    ),
  );
  open(busy);
  expect(await screen.findByText("Importing. The progress is on the Tasks page.")).toBeVisible();
  expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument();
  // 실행 Task가 이미 끝났으면(원본 노드가 없어졌다) `importing`이어도 지울 수 있다.
  await userEvent.click(screen.getByRole("button", { name: "Delete import db01" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete import db01?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(
    await within(dialog).findByText(
      "The import is running, or another task on it has not finished.",
    ),
  ).toBeVisible();
});

it("is read-only for viewers", async () => {
  mockLists({ "vm-import": [web01] });
  open(web01, { ...ADMIN, role: "viewer" });
  expect(await screen.findByRole("heading", { name: "web01" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Import" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete import web01" })).not.toBeInTheDocument();
});

it("deletes the import and goes back to the list", async () => {
  mockLists({ "vm-import": [web01], node: [NODE] });
  let offline = true;
  server.use(
    http.delete(apiUrl("/vm-imports/:key"), () =>
      offline
        ? HttpResponse.json({ code: "node_offline", message: "offline" }, { status: 409 })
        : HttpResponse.json(task("t2", "vm-import-delete", web01.id), { status: 202 }),
    ),
  );
  open(web01);
  await userEvent.click(await screen.findByRole("button", { name: "Delete import web01" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete import web01?" });
  expect(dialog).toHaveTextContent("Deletes the uploaded OVA from node-a.");
  // 원본 노드가 없으면 웹에서는 지울 수 없다. 리소스만 지우는 명령을 알린다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(
    await within(dialog).findByText(
      "The node holding the OVA is offline or has left the cluster. To forget the import and leave its files on that node, run oplctl import delete web01 --force.",
    ),
  ).toBeVisible();
  offline = false;
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("moved")).toBeInTheDocument();
  expect(await screen.findByText("Delete import web01")).toBeInTheDocument();
});

it("says when the import does not exist", async () => {
  mockLists({ "vm-import": [] });
  open(vmImport("nope", 9));
  expect(await screen.findByText("Import nope not found")).toBeInTheDocument();
});
```

`web/src/imports/ImportPage.tsx`를 만든다:

```tsx
import type { TFunction } from "i18next";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Task, VmImport } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { runForm, sourceNetworks, toRun, validateRun, type RunForm } from "@/imports/imports";
import { ImportedVm, ImportPhaseBadge } from "@/imports/ImportsPage";
import { formatBytes, formatMib } from "@/lib/format";
import { poolsUsableOn } from "@/storage/pools";
import { trackTask } from "@/tasks/tasks";

/** 사용자 네트워크(NAT). 네트워크 리소스가 아니어도 매핑할 수 있다. */
const USER_NETWORK = "user";

/** 서버 오류를 가져오기에 맞는 문장으로(`node_offline`은 VM이 아니라 OVA가 있는 노드다). */
function importError(t: TFunction, err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "imports.codes")
    : errorMessage(t, err);
}

/** 가져오기 하나: 원본 요약·경고·오류, `ready`면 실행 폼, 끝났으면 만든 VM. */
export function ImportPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const imports = useList("vm-import");
  if (!imports.data) {
    return <ListPending error={imports.error} />;
  }
  const imp = imports.data.find((i) => i.name === name);
  if (!imp) {
    return <h1 className="text-2xl font-semibold">{t("imports.notFound", { name })}</h1>;
  }
  return <ImportView key={imp.id} imp={imp} />;
}

function ImportView({ imp }: { imp: VmImport }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const nodes = useList("node");
  const write = can(me.role, "write", "vm-import");
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  // 폼은 가져오기마다 하나다. 실행이 실패해 `ready`로 돌아와도 고친 값이 남는다.
  const [form, setForm] = useState<RunForm>(() => runForm(imp));
  // 실행을 보낸 때의 revision. 가져오기가 바뀔 때까지(성공하면 `importing`이 된다) 다시 보내거나 지우지 않는다.
  const [sentAt, setSentAt] = useState<number | null>(null);
  const busy = sentAt === imp.revision;
  const node = nodes.data?.find((n) => n.id === imp.spec.node)?.name ?? "—";
  const { phase, error, warnings, source_vm: src } = imp.status;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-2">
        <div className="flex flex-col gap-1">
          <Link
            to="/imports"
            className="text-sm text-muted-foreground underline-offset-4 hover:underline"
          >
            {t("nav.imports")}
          </Link>
          <h1 className="text-2xl font-semibold">{imp.name}</h1>
        </div>
        {write && (
          // 가져오는 중인지는 서버가 정한다(실행 Task가 끝나지 않았으면 import_busy). 실행을 보내는 동안만 막는다.
          <Button
            variant="outline"
            aria-label={t("imports.deleteNamed", { name: imp.name })}
            disabled={busy}
            onClick={(e) => {
              opener.current = e.currentTarget;
              setDeleteOpen(true);
            }}
          >
            {t("common.delete")}
          </Button>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <ImportPhaseBadge phase={phase} />
        {phase === "importing" && (
          <span className="text-muted-foreground">{t("imports.importing")}</span>
        )}
        {phase === "done" && (
          <span>
            {t("imports.imported")} <ImportedVm imp={imp} />
          </span>
        )}
      </div>
      {error && (
        <p role="status" className="text-sm text-destructive">
          {t(phase === "failed" ? "imports.failedError" : "imports.lastError", { error })}
        </p>
      )}
      <Card>
        <CardHeader>
          <CardTitle>{t("imports.source")}</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">{t("imports.fileName")}</dt>
            <dd>
              {imp.spec.source.ova.file_name} ({formatBytes(imp.spec.source.ova.size_bytes)}),{" "}
              {node}
            </dd>
            {src && (
              <>
                <dt className="text-muted-foreground">{t("imports.sourceVm")}</dt>
                <dd>{src.name}</dd>
                <dt className="text-muted-foreground">{t("imports.guestOs")}</dt>
                <dd>{src.guest_os}</dd>
                <dt className="text-muted-foreground">{t("vms.cpu")}</dt>
                <dd>{t("imports.cpuValue", { vcpus: src.vcpus, cores: src.cores_per_socket })}</dd>
                <dt className="text-muted-foreground">{t("vms.memory")}</dt>
                <dd>{formatMib(src.memory_mib)}</dd>
                <dt className="text-muted-foreground">{t("vms.firmware")}</dt>
                <dd>{src.firmware}</dd>
              </>
            )}
          </dl>
          {src && (
            <>
              <Table aria-label={t("vms.disks")}>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("common.name")}</TableHead>
                    <TableHead>{t("imports.diskFile")}</TableHead>
                    <TableHead>{t("imports.capacity")}</TableHead>
                    <TableHead>{t("imports.controller")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {src.disks.map((d) => (
                    <TableRow key={d.name}>
                      <TableCell>{d.name}</TableCell>
                      <TableCell>{d.file}</TableCell>
                      <TableCell>{formatBytes(d.capacity_bytes)}</TableCell>
                      <TableCell>{d.controller}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
              <Table aria-label={t("vms.nics")}>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("vms.network")}</TableHead>
                    <TableHead>{t("imports.adapter")}</TableHead>
                    <TableHead>{t("vms.mac")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {src.nics.map((n, i) => (
                    <TableRow key={i}>
                      <TableCell>{n.network}</TableCell>
                      <TableCell>{n.adapter}</TableCell>
                      <TableCell>{n.mac ?? "—"}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </>
          )}
          {warnings.length > 0 && (
            <section aria-labelledby="import-warnings" className="flex flex-col gap-1">
              <h2 id="import-warnings" className="text-sm font-medium">
                {t("imports.warnings")}
              </h2>
              <ul className="list-disc pl-5 text-sm text-muted-foreground">
                {warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            </section>
          )}
        </CardContent>
      </Card>
      {phase === "ready" && write && src && (
        <RunSection
          imp={imp}
          node={node}
          form={form}
          setForm={setForm}
          busy={busy}
          setSent={(sent) => setSentAt(sent ? imp.revision : null)}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("imports.deleteTitle", { name: imp.name })}
        description={t("imports.deleteDescription", { node })}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          try {
            // `force` 없이 지우면 응답은 늘 Task다(200 `{ message }`는 force만).
            const task = (await call(
              api.DELETE("/vm-imports/{key}", { params: { path: { key: imp.id } } }),
            )) as Task;
            trackTask(task, t("tasks.op.vm-import-delete", { target: imp.name }));
          } catch (err) {
            // 원본 노드가 없으면 웹에서는 지울 수 없다. 리소스만 지우는 방법을 알린다.
            if (err instanceof ApiError && err.code === "node_offline") {
              throw new Error(
                t("imports.forceHint", { error: importError(t, err), name: imp.name }),
                { cause: err },
              );
            }
            throw err;
          }
          void navigate("/imports");
        }}
      />
    </div>
  );
}

/**
 * 제안으로 채운 실행 폼. 보내면 가져오기가 `importing`이 되어 폼이 사라진다. 폼 값과 보내는 중인지는
 * `ImportView`가 들고 있다.
 */
function RunSection({
  imp,
  node,
  form,
  setForm,
  busy,
  setSent,
}: {
  imp: VmImport;
  node: string;
  form: RunForm;
  setForm: React.Dispatch<React.SetStateAction<RunForm>>;
  busy: boolean;
  setSent: (sent: boolean) => void;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const networks = useList("network");
  const nodes = useList("node");
  const [errors, setErrors] = useState<Record<string, string>>({});
  // `name_taken`의 서버 메시지(VM과 만들 볼륨 가운데 어느 이름인지)
  const [taken, setTaken] = useState("");
  const [failure, setFailure] = useState<string | null>(null);
  // Task는 OVA가 있는 노드에서 돈다. 그 노드에서 준비된 풀만 고른다.
  const usable = poolsUsableOn(pools.data ?? [], nodes.data ?? [], imp.spec.node);
  const networkNames = [USER_NETWORK, ...(networks.data ?? []).map((n) => n.name)];
  const set = (patch: Partial<RunForm>) => setForm((f) => ({ ...f, ...patch }));

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const found = validateRun(
      form,
      imp,
      {
        vms: (vms.data ?? []).map((v) => v.name),
        volumes: (volumes.data ?? []).map((v) => v.name),
      },
      networkNames,
    );
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    // 성공하면 가져오기가 `importing`으로 바뀔 때까지 막아 두어 두 번 보내지 않는다. 실패할 때만 푼다.
    setSent(true);
    setFailure(null);
    try {
      const task = await call(
        api.POST("/vm-imports/{key}/run", {
          params: { path: { key: imp.id } },
          body: toRun(form),
        }),
      );
      trackTask(task, t("tasks.op.vm-import", { target: form.name }));
    } catch (err) {
      setSent(false);
      // 일반 번역은 업로드의 "접속한 노드"를 말한다. 가져오기는 OVA가 있는 노드에서 돈다.
      if (err instanceof ApiError && err.code === "pool_not_on_node") {
        setErrors({ pool: "imports.errors.poolNotOnNode" });
      } else if (err instanceof ApiError && err.code === "pool_unavailable") {
        setErrors({ pool: "imports.errors.poolUnavailable" });
      } else if (err instanceof ApiError && err.code === "name_taken") {
        // VM이나 만들 볼륨의 이름이다. 서버가 어느 것인지 말한다.
        setTaken(err.message);
        setErrors({ name: "imports.errors.nameTaken" });
      } else {
        setFailure(importError(t, err));
      }
    }
  }

  const select = (
    id: string,
    label: string,
    value: string,
    options: readonly string[],
    onChange: (value: string) => void,
  ) => (
    <Field id={id} label={label}>
      <NativeSelect
        id={id}
        className="w-full"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      >
        {options.map((o) => (
          <NativeSelectOption key={o} value={o}>
            {o}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </Field>
  );
  const number = (id: string, label: string, field: "sockets" | "cores" | "memory") => (
    <Field id={id} label={label} error={errors[field]}>
      {(a11y) => (
        <Input
          id={id}
          inputMode="numeric"
          value={form[field]}
          onChange={(e) => set({ [field]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <form onSubmit={(e) => void submit(e)} className="flex flex-col gap-4" noValidate>
      <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
        <Card>
          <CardHeader>
            <CardTitle>{t("imports.settings")}</CardTitle>
            <CardDescription>{t("imports.settingsHint")}</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2">
            <Field
              id="run-name"
              label={t("imports.vmName")}
              error={errors.name}
              errorValues={{ message: taken }}
            >
              {(a11y) => (
                <Input
                  id="run-name"
                  value={form.name}
                  onChange={(e) => set({ name: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id="run-pool"
              label={t("imports.pool")}
              error={errors.pool}
              hint={t("imports.poolHint", { node })}
            >
              {(a11y) => (
                <NativeSelect
                  id="run-pool"
                  className="w-full"
                  value={form.pool}
                  onChange={(e) => set({ pool: e.target.value })}
                  {...a11y}
                >
                  <NativeSelectOption value="">—</NativeSelectOption>
                  {usable.map((p) => (
                    <NativeSelectOption key={p.id} value={p.name}>
                      {p.name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              )}
            </Field>
            {!pools.isSuccess && <ListPending error={pools.error} />}
            {number("run-sockets", t("imports.sockets"), "sockets")}
            {number("run-cores", t("imports.cores"), "cores")}
            {number("run-memory", t("vmForm.memory"), "memory")}
            {select("run-firmware", t("vms.firmware"), form.firmware, ["uefi", "bios"], (v) =>
              set({ firmware: v as RunForm["firmware"] }),
            )}
            {select(
              "run-disk-bus",
              t("imports.diskBus"),
              form.diskBus,
              ["virtio-blk", "sata"],
              (v) => set({ diskBus: v as RunForm["diskBus"] }),
            )}
            {select(
              "run-nic-model",
              t("imports.nicModel"),
              form.nicModel,
              ["virtio", "e1000e"],
              (v) => set({ nicModel: v as RunForm["nicModel"] }),
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t("imports.networks")}</CardTitle>
            <CardDescription>{t("imports.networksHint")}</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4">
            {!networks.isSuccess && <ListPending error={networks.error} />}
            <div className="grid gap-4 sm:grid-cols-2">
              {sourceNetworks(imp).map((source, i) => (
                <Field
                  key={source}
                  id={`run-network-${i}`}
                  label={source}
                  error={errors[`networks.${source}`]}
                >
                  {(a11y) => (
                    <NativeSelect
                      id={`run-network-${i}`}
                      className="w-full"
                      value={form.networks[source] ?? ""}
                      onChange={(e) =>
                        set({ networks: { ...form.networks, [source]: e.target.value } })
                      }
                      {...a11y}
                    >
                      <NativeSelectOption value="">{t("imports.chooseNetwork")}</NativeSelectOption>
                      <NativeSelectOption value={USER_NETWORK}>
                        {t("vmForm.userNetwork")}
                      </NativeSelectOption>
                      {(networks.data ?? []).map((n) => (
                        <NativeSelectOption key={n.id} value={n.name}>
                          {n.name}
                        </NativeSelectOption>
                      ))}
                    </NativeSelect>
                  )}
                </Field>
              ))}
            </div>
            <div className="flex flex-col gap-1">
              <div className="flex items-center gap-2">
                <Switch
                  id="run-keep-macs"
                  aria-describedby="run-keep-macs-hint"
                  checked={form.keepMacs}
                  onCheckedChange={(keepMacs) => set({ keepMacs })}
                />
                <Label htmlFor="run-keep-macs">{t("imports.keepMacs")}</Label>
              </div>
              <p id="run-keep-macs-hint" className="text-xs text-muted-foreground">
                {t("imports.keepMacsHint")}
              </p>
            </div>
          </CardContent>
        </Card>
        <div className="flex items-center gap-3">
          <Button type="submit">{t("imports.run")}</Button>
          {failure && (
            <p role="alert" className="text-sm text-destructive">
              {failure}
            </p>
          )}
        </div>
      </fieldset>
    </form>
  );
}
```

`web/src/imports/ImportsPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ImportsPage } from "@/imports/ImportsPage";
import { resetUploads } from "@/storage/upload";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, vm, vmImport } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
});

const web = vm("web", "stopped");
const done = vmImport("web01", 1, { phase: "done", vm: web.id });
const retry = vmImport("db01", 2, { error: "checksum mismatch for db01-disk1.vmdk" });
const gone = vmImport("old", 3, { phase: "done", vm: "019a0f3c-0000-7000-8000-00000000dead" });

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

it("lists imports with their source, state and VM", async () => {
  mockLists({ "vm-import": [done, retry, gone], vm: [web], node: [NODE] });
  renderPage(<ImportsPage />);
  const table = await screen.findByRole("table", { name: "Imports" });
  await within(table).findByText("web01");
  expect(within(table).getByRole("link", { name: "web01" })).toHaveAttribute(
    "href",
    "/imports/web01",
  );
  const row = cells(table, "web01");
  expect(row("Node")).toHaveTextContent("node-a");
  expect(row("Source VM")).toHaveTextContent("Web 01web01.ova");
  expect(row("State")).toHaveTextContent("Done");
  expect(await within(row("VM")!).findByRole("link", { name: "web" })).toHaveAttribute(
    "href",
    "/vms/web",
  );
  // 실패한 실행의 오류는 상태 아래에 보인다.
  const db = cells(table, "db01");
  expect(db("State")).toHaveTextContent("Readychecksum mismatch for db01-disk1.vmdk");
  expect(db("VM")).toHaveTextContent("—");
  expect(cells(table, "old")("VM")).toHaveTextContent("Deleted VM");
});

it("hides the upload from viewers", async () => {
  mockLists({ "vm-import": [done] });
  renderPage(<ImportsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("web01")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Upload OVA" })).not.toBeInTheDocument();
});

it("uploads an OVA under a name taken from the file", async () => {
  mockLists({ "vm-import": [done], node: [NODE] });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
    http.post(apiUrl("/vm-imports/ova/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderPage(<ImportsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload OVA" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload an OVA" });
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("Choose a file.")).toBeVisible();

  // 이름은 확장자를 빼고 이름 규칙에 맞춘다. 이미 있는 이름과 예약한 이름은 받지 않는다.
  await userEvent.upload(within(dialog).getByLabelText("OVA file"), new File(["ova"], "Web01.ova"));
  expect(within(dialog).getByLabelText("Name")).toHaveValue("web01");
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("An import with this name already exists.")).toBeVisible();
  await userEvent.upload(within(dialog).getByLabelText("OVA file"), new File(["ova"], "OVA.ova"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("The name ova is reserved.")).toBeVisible();

  await userEvent.upload(
    within(dialog).getByLabelText("OVA file"),
    new File(["ova"], "Web Server 2019.ova"),
  );
  expect(within(dialog).getByLabelText("Name")).toHaveValue("web-server-2019");
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  // 확인을 통과한 뒤에 보낸다.
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  expect(FakeXhr.last!.url).toContain(
    "/vm-imports/ova?name=web-server-2019&file_name=Web+Server+2019.ova",
  );
  expect(await screen.findByText(/^Uploading web-server-2019: 0%/)).toBeInTheDocument();
});
```

`web/src/imports/ImportsPage.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { Upload } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { VmImport } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { importNameError } from "@/imports/imports";
import { startOvaUpload } from "@/imports/ova";
import { formatBytes } from "@/lib/format";
import { nameFromFile } from "@/storage/upload";

type ImportPhase = components["schemas"]["ImportPhase"];

const PHASE_VARIANT = {
  ready: "default",
  importing: "secondary",
  done: "outline",
  failed: "destructive",
} as const;

/** 가져오기 상태 */
export function ImportPhaseBadge({ phase }: { phase: ImportPhase }) {
  const { t } = useTranslation();
  return <Badge variant={PHASE_VARIANT[phase]}>{t(`imports.phase.${phase}`)}</Badge>;
}

/** 만든 VM으로 가는 링크. 만들지 않았으면 "—", VM을 지웠으면 그렇다고 보인다. */
export function ImportedVm({ imp }: { imp: VmImport }) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const id = imp.status.vm;
  if (!id) {
    return <>—</>;
  }
  const vm = vms.data?.find((v) => v.id === id);
  if (!vm) {
    return <>{vms.isSuccess ? t("imports.vmGone") : "—"}</>;
  }
  return (
    <Link to={`/vms/${encodeURIComponent(vm.name)}`} className="underline-offset-4 hover:underline">
      {vm.name}
    </Link>
  );
}

/** VM 가져오기 목록과 OVA 올리기. 항목을 열면 원본을 보고 가져오기를 실행한다. */
export function ImportsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const imports = useList("vm-import");
  const nodes = useList("node");
  const [uploading, setUploading] = useState(false);
  const write = can(me.role, "write", "vm-import");
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name ?? "—";

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-col gap-1">
          <h1 id="imports-title" className="text-2xl font-semibold">
            {t("nav.imports")}
          </h1>
          <p className="text-sm text-muted-foreground">{t("imports.pageHint")}</p>
        </div>
        {write && (
          <Button onClick={() => setUploading(true)}>
            <Upload />
            {t("imports.upload")}
          </Button>
        )}
      </div>
      <Table aria-labelledby="imports-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("imports.sourceVm")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("imports.vm")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(imports.data ?? []).length === 0 && (
            <TableMessage colSpan={5} query={imports} empty={t("imports.empty")} />
          )}
          {(imports.data ?? []).map((imp) => (
            <TableRow key={imp.id}>
              <TableCell className="font-medium">
                <Link
                  to={`/imports/${encodeURIComponent(imp.name)}`}
                  className="underline-offset-4 hover:underline"
                >
                  {imp.name}
                </Link>
              </TableCell>
              <TableCell>{nodeName(imp.spec.node)}</TableCell>
              <TableCell>
                {imp.status.source_vm?.name ?? "—"}
                <div className="text-xs text-muted-foreground">{imp.spec.source.ova.file_name}</div>
              </TableCell>
              <TableCell>
                <ImportPhaseBadge phase={imp.status.phase} />
                {imp.status.error && (
                  <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                    {imp.status.error}
                  </div>
                )}
              </TableCell>
              <TableCell>
                <ImportedVm imp={imp} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {uploading && <OvaUploadDialog onClose={() => setUploading(false)} />}
    </div>
  );
}

/** OVA를 고르고 가져오기 이름을 정한다. 이름은 파일 이름에서 제안한다. */
function OvaUploadDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const imports = useList("vm-import");
  const nodes = useList("node");
  // 파일은 접속한 노드로 간다(이미지 올리기와 같은 질의).
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
  const [file, setFile] = useState<File | null>(null);
  const [name, setName] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  function choose(chosen: File | null) {
    setFile(chosen);
    if (chosen) {
      setName(nameFromFile(chosen.name));
    }
  }

  async function submit() {
    const found: Record<string, string> = {};
    if (!file) {
      found.file = "uploads.errors.file";
    }
    const nameError = importNameError(name, imports.data?.map((i) => i.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // 올리는 동안 대화상자를 닫아 둔다. 진행률은 알림에 보인다.
    startOvaUpload(file!, name, t, (imp) => void navigate(`/imports/${encodeURIComponent(imp)}`));
  }

  return (
    <FormDialog
      title={t("imports.uploadTitle")}
      description={t("imports.uploadDescription", { node: here })}
      submitLabel={t("uploads.submit")}
      onClose={onClose}
      onSubmit={submit}
    >
      {cluster.isError && (
        <p role="alert" className="text-sm text-destructive">
          {t("uploads.noNode", { error: errorMessage(t, cluster.error) })}
        </p>
      )}
      <Field
        id="ova-file"
        label={t("imports.file")}
        error={errors.file}
        hint={file ? formatBytes(file.size) : undefined}
      >
        {(a11y) => (
          <Input
            id="ova-file"
            type="file"
            accept=".ova"
            onChange={(e) => choose(e.target.files?.[0] ?? null)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="ova-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input id="ova-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/imports/imports.test.ts`를 만든다:

```ts
import {
  importNameError,
  runForm,
  sourceNetworks,
  toRun,
  validateRun,
  volumeNames,
} from "@/imports/imports";
import { vmImport } from "@/test/fixtures";

const NONE = { vms: [], volumes: [] };

/** 원본 NIC가 둘(네트워크 둘, 한 네트워크에 둘)인 가져오기 */
const two = vmImport("web01", 1, {
  source_vm: {
    ...vmImport("web01", 1).status.source_vm!,
    nics: [
      { network: "VM Network", adapter: "vmxnet3", mac: "00:50:56:01:02:03" },
      { network: "Backup", adapter: "e1000", mac: null },
      { network: "VM Network", adapter: "vmxnet3", mac: null },
    ],
  },
  proposal: { ...vmImport("web01", 1).status.proposal!, networks: { "VM Network": "lan" } },
});

it("checks new import names", () => {
  expect(importNameError("web01", ["db"])).toBeUndefined();
  expect(importNameError("Web 01", [])).toBe("forms.name");
  // `/vm-imports/ova` 경로와 겹친다.
  expect(importNameError("ova", [])).toBe("imports.errors.reserved");
  expect(importNameError("db", ["db"])).toBe("imports.errors.exists");
});

it("lists each source network once", () => {
  expect(sourceNetworks(two)).toEqual(["VM Network", "Backup"]);
  expect(sourceNetworks(vmImport("x", 2, { source_vm: undefined }))).toEqual([]);
});

it("names the volumes like the server", () => {
  expect(volumeNames(vmImport("web01", 1), "web")).toEqual(["web-disk0"]);
  // 63자로 자르고 끝의 `-`를 뗀다.
  expect(volumeNames(vmImport("web01", 1), `${"a".repeat(62)}-b`)).toEqual(["a".repeat(62)]);
});

it("fills the form from the proposal", () => {
  expect(runForm(two)).toEqual({
    name: "web01",
    pool: "",
    sockets: "1",
    cores: "2",
    memory: "1024",
    firmware: "uefi",
    diskBus: "virtio-blk",
    nicModel: "virtio",
    // 이름이 같은 네트워크가 없는 원본 네트워크는 비워 둔다.
    networks: { "VM Network": "lan", Backup: "" },
    keepMacs: true,
  });
});

it("requires a mapping to an existing network for every source network", () => {
  const form = { ...runForm(two), pool: "local" };
  expect(validateRun(form, two, NONE, ["lan", "user"])).toEqual({
    "networks.Backup": "imports.errors.network",
  });
  // 제안한 네트워크가 지워졌다.
  expect(
    validateRun({ ...form, networks: { "VM Network": "lan", Backup: "user" } }, two, NONE, [
      "user",
    ]),
  ).toEqual({ "networks.VM Network": "imports.errors.network" });
  expect(
    validateRun({ ...form, networks: { "VM Network": "lan", Backup: "user" } }, two, NONE, [
      "lan",
      "user",
    ]),
  ).toEqual({});
});

it("checks the name, pool and numbers", () => {
  const imp = vmImport("web01", 1);
  const form = { ...runForm(imp), networks: { "VM Network": "lan" } };
  expect(
    validateRun({ ...form, name: "-x", sockets: "0", cores: "1.5", memory: "32" }, imp, NONE, [
      "lan",
    ]),
  ).toEqual({
    name: "forms.name",
    pool: "imports.errors.pool",
    sockets: "imports.errors.sockets",
    cores: "vmForm.errors.cores",
    memory: "vmForm.errors.memory",
  });
  const ok = { ...form, pool: "local" };
  expect(validateRun({ ...ok, memory: "" }, imp, NONE, ["lan"])).toEqual({
    memory: "vmForm.errors.memory",
  });
  expect(validateRun(ok, imp, { vms: ["web01"], volumes: [] }, ["lan"])).toEqual({
    name: "imports.errors.vmExists",
  });
  expect(validateRun(ok, imp, { vms: [], volumes: ["web01-disk0"] }, ["lan"])).toEqual({
    name: "imports.errors.volumeExists",
  });
});

it("sends every value of the form", () => {
  const form = {
    ...runForm(vmImport("web01", 1)),
    name: "web",
    pool: "local",
    sockets: "2",
    cores: "4",
    memory: "8192",
    firmware: "bios" as const,
    diskBus: "sata" as const,
    nicModel: "e1000e" as const,
    networks: { "VM Network": "lan" },
    keepMacs: false,
  };
  expect(toRun(form)).toEqual({
    name: "web",
    pool: "local",
    networks: { "VM Network": "lan" },
    disk_bus: "sata",
    nic_model: "e1000e",
    firmware: "bios",
    sockets: 2,
    cores: 4,
    memory_mib: 8192,
    keep_macs: false,
  });
});
```

`web/src/imports/imports.ts`를 만든다:

```ts
import type { components } from "@/api/schema";
import type { ImportRun, VmImport } from "@/api/types";
import { MIN_MEMORY_MIB, validName } from "@/vms/form";

type Schemas = components["schemas"];

/** `/vm-imports/ova` 경로와 겹쳐 서버가 가져오기 이름으로 받지 않는다. */
export const RESERVED_IMPORT = "ova";
/** 서버의 `cores`·`sockets`(u16)와 `memory_mib`(u32)의 최댓값 */
const MAX_COUNT = 65535;
const MAX_MEMORY_MIB = 2 ** 32 - 1;
/** 서버의 리소스 이름 길이 */
const MAX_NAME = 63;

/** 새 가져오기 이름의 입력 오류(번역 키). `taken`은 이미 있는 가져오기의 이름이다. */
export function importNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (name === RESERVED_IMPORT) {
    return "imports.errors.reserved";
  }
  if (taken.includes(name)) {
    return "imports.errors.exists";
  }
  return undefined;
}

/** 원본 NIC들의 네트워크. 처음 나온 차례로, 겹치지 않게. */
export function sourceNetworks(imp: VmImport): string[] {
  return [...new Set((imp.status.source_vm?.nics ?? []).map((n) => n.network))];
}

/**
 * 서버의 `volume_name`처럼 디스크마다 만들 볼륨 이름: `{VM 이름}-{디스크 이름}`, 63자로 자르고 끝의 `-`를
 * 뗀다.
 */
export function volumeNames(imp: VmImport, vmName: string): string[] {
  return (imp.status.source_vm?.disks ?? []).map((d) =>
    `${vmName}-${d.name}`.slice(0, MAX_NAME).replace(/-+$/, ""),
  );
}

/** 실행 폼. 숫자 칸은 입력 그대로의 문자열이다. */
export interface RunForm {
  name: string;
  pool: string;
  sockets: string;
  cores: string;
  memory: string;
  firmware: Schemas["Firmware"];
  diskBus: Schemas["DiskBus"];
  nicModel: Schemas["NicModel"];
  /** 원본 네트워크 → opLite 네트워크(고르지 않았으면 빈 문자열) */
  networks: Record<string, string>;
  keepMacs: boolean;
}

/** 제안으로 채운 실행 폼. VM 이름은 가져오기 이름, 풀은 고르게 비워 둔다. */
export function runForm(imp: VmImport): RunForm {
  const p = imp.status.proposal;
  return {
    name: imp.name,
    pool: "",
    sockets: String(p?.sockets ?? 1),
    cores: String(p?.cores ?? 1),
    memory: String(p?.memory_mib ?? MIN_MEMORY_MIB),
    firmware: p?.firmware ?? "bios",
    diskBus: p?.disk_bus ?? "virtio-blk",
    nicModel: p?.nic_model ?? "virtio",
    networks: Object.fromEntries(sourceNetworks(imp).map((n) => [n, p?.networks[n] ?? ""])),
    keepMacs: true,
  };
}

/** `min`부터 `max`까지의 정수인지 */
function intIn(value: string, min: number, max: number): boolean {
  const n = Number(value);
  return value.trim() !== "" && Number.isInteger(n) && n >= min && n <= max;
}

/**
 * 입력 오류: 필드 → 번역 키. 서버도 검사하지만 흔한 실수는 보내기 전에 알린다. `taken`은 이미 있는 VM과
 * 볼륨 이름, `networks`는 고를 수 있는 네트워크 이름이다. 네트워크 오류의 필드는 `networks.<원본 네트워크>`다.
 */
export function validateRun(
  form: RunForm,
  imp: VmImport,
  taken: { vms: readonly string[]; volumes: readonly string[] },
  networks: readonly string[],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!validName(form.name)) {
    errors.name = "forms.name";
  } else if (taken.vms.includes(form.name)) {
    errors.name = "imports.errors.vmExists";
  } else if (volumeNames(imp, form.name).some((v) => taken.volumes.includes(v))) {
    errors.name = "imports.errors.volumeExists";
  }
  if (form.pool === "") {
    errors.pool = "imports.errors.pool";
  }
  if (!intIn(form.sockets, 1, MAX_COUNT)) {
    errors.sockets = "imports.errors.sockets";
  }
  if (!intIn(form.cores, 1, MAX_COUNT)) {
    errors.cores = "vmForm.errors.cores";
  }
  if (!intIn(form.memory, MIN_MEMORY_MIB, MAX_MEMORY_MIB)) {
    errors.memory = "vmForm.errors.memory";
  }
  for (const source of sourceNetworks(imp)) {
    // 제안한 네트워크가 그새 지워졌을 수도 있다.
    if (!networks.includes(form.networks[source] ?? "")) {
      errors[`networks.${source}`] = "imports.errors.network";
    }
  }
  return errors;
}

/** 실행 요청 본문. 폼의 값을 모두 보낸다(제안과 같아도). */
export function toRun(form: RunForm): ImportRun {
  return {
    name: form.name,
    pool: form.pool,
    networks: form.networks,
    disk_bus: form.diskBus,
    nic_model: form.nicModel,
    firmware: form.firmware,
    sockets: Number(form.sockets),
    cores: Number(form.cores),
    memory_mib: Number(form.memory),
    keep_macs: form.keepMacs,
  };
}
```

`web/src/imports/ova.test.tsx`를 만든다:

```tsx
import { act, fireEvent, render, screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { ApiError } from "@/api/client";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { startOvaUpload, uploadOva } from "@/imports/ova";
import { ThemeProvider } from "@/lib/theme";
import { resetUploads } from "@/storage/upload";
import { FakeXhr } from "@/test/fakeXhr";
import { vmImport } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";

/** OVA 확인 요청의 쿼리. 기본으로 통과(204)시킨다. */
let checks: URLSearchParams[] = [];

beforeEach(() => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  checks = [];
  server.use(
    http.post(apiUrl("/vm-imports/ova/check"), ({ request }) => {
      checks.push(new URL(request.url).searchParams);
      return new HttpResponse(null, { status: 204 });
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
});

it("posts the OVA with its file name and returns the import", async () => {
  const file = new File(["ova"], "Web 01.ova");
  const done = uploadOva(file, "web01", {});
  const xhr = FakeXhr.last!;
  expect(xhr.method).toBe("POST");
  expect(xhr.url).toBe(
    `${window.location.origin}/api/v1/vm-imports/ova?name=web01&file_name=Web+01.ova`,
  );
  expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
  expect(xhr.body).toBe(file);
  xhr.respond(201, vmImport("web01", 1));
  await expect(done).resolves.toMatchObject({ name: "web01" });

  // 해석하지 못한 OVA
  const bad = uploadOva(file, "web01", {});
  FakeXhr.last!.respond(400, { code: "invalid_ova", message: "no .ovf file" });
  await expect(bad).rejects.toEqual(new ApiError(400, "invalid_ova", "no .ovf file"));
});

it("checks first, shows progress and offers to open the import", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  const open = vi.fn();
  act(() => startOvaUpload(new File(["x".repeat(2048)], "web01.ova"), "web01", i18n.t, open));
  expect(await screen.findByText("Uploading web01: 0% (0 B / 2 KiB)")).toBeInTheDocument();
  const xhr = await vi.waitFor(() => {
    expect(FakeXhr.last).toBeDefined();
    return FakeXhr.last!;
  });
  expect(Object.fromEntries(checks[0]!)).toEqual({ name: "web01", size: "2048" });
  act(() => xhr.uploaded());
  expect(await screen.findByText("Processing web01…")).toBeInTheDocument();
  act(() => xhr.respond(201, vmImport("web01", 1)));
  expect(
    await screen.findByText("Uploaded web01. Review the proposed settings and run the import."),
  ).toBeInTheDocument();
  // sonner는 알림을 누르면 jsdom에 없는 포인터 캡처를 쓰므로 버튼만 누른다.
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  expect(open).toHaveBeenCalledWith("web01");
});

it("says why the server rejected the OVA", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  act(() => startOvaUpload(new File(["x"], "a.ova"), "a", i18n.t, () => {}));
  const xhr = await vi.waitFor(() => {
    expect(FakeXhr.last).toBeDefined();
    return FakeXhr.last!;
  });
  act(() => xhr.respond(400, { code: "invalid_ova", message: "no .ovf file" }));
  expect(
    await screen.findByText("Upload of a failed. The file is not a usable OVA: no .ovf file"),
  ).toBeInTheDocument();
});
```

`web/src/imports/ova.ts`를 만든다:

```ts
import type { TFunction } from "i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { VmImport } from "@/api/types";
import { runUpload, sendFile, type SendOptions } from "@/storage/upload";

/** OVA를 보내기 전에 서버가 받을 수 있는지 묻는다(`/vm-imports/ova/check`, 204). 거절하면 `ApiError`. */
export async function checkOva(name: string, size: number, signal?: AbortSignal) {
  await call(api.POST("/vm-imports/ova/check", { params: { query: { name, size } }, signal }));
}

/**
 * OVA를 `/vm-imports/ova`로 올린다. 서버가 받아 해석한 가져오기(201)로 끝난다. 파일 이름은 표시용으로
 * 함께 보낸다. 중간에 끊기면 서버가 받은 파일을 지운다.
 */
export function uploadOva(file: File, name: string, options: SendOptions): Promise<VmImport> {
  const query = new URLSearchParams({ name, file_name: file.name });
  return sendFile("/vm-imports/ova", query, file, 201, isImport, options);
}

function isImport(body: unknown): body is VmImport {
  return typeof (body as Partial<VmImport> | null)?.id === "string";
}

/**
 * OVA 업로드를 시작한다(이미지 올리기와 같은 진행률 알림). 다 올리면 가져오기 화면을 여는 버튼을
 * 알림에 보인다. `open`은 가져오기 이름을 받는다.
 */
export function startOvaUpload(
  file: File,
  name: string,
  t: TFunction,
  open: (name: string) => void,
) {
  runUpload(
    file,
    {
      name,
      check: (signal) => checkOva(name, file.size, signal),
      send: (options) => uploadOva(file, name, options),
      done: (imp, id) => {
        toast.success(t("imports.uploaded", { name: imp.name }), {
          id,
          duration: 10_000,
          closeButton: true,
          action: { label: t("imports.review"), onClick: () => open(imp.name) },
        });
      },
      noResponse: t("imports.noResponse", { name }),
    },
    t,
  );
}
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업은 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업, 가져오기는 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.getByRole("link", { name: "Backup jobs" })).toHaveAttribute("href", "/backup-jobs");
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("link", { name: "Backup jobs" })).toHaveAttribute("href", "/backup-jobs");
  expect(screen.getByRole("link", { name: "Imports" })).toHaveAttribute("href", "/imports");
});
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  HardDrive,
  KeyRound,
```

을 다음으로 바꾼다:

```tsx
  HardDrive,
  Import,
  KeyRound,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/vms", key: "nav.vms", icon: Server, end: false },
  { to: "/volumes", key: "nav.volumes", icon: HardDrive, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/vms", key: "nav.vms", icon: Server, end: false },
  { to: "/imports", key: "nav.imports", icon: Import, end: false },
  { to: "/volumes", key: "nav.volumes", icon: HardDrive, end: false },
```

`web/src/storage/upload.ts` 전체:

```ts
import type { TFunction } from "i18next";
import { toast } from "sonner";

import {
  api,
  call,
  currentAuthEpoch,
  NetworkError,
  reportUnauthorized,
  toApiError,
} from "@/api/client";
import type { components } from "@/api/schema";
import type { Task } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

type Schemas = components["schemas"];

export interface UploadParams {
  name: string;
  pool: string;
  format: Schemas["ImageFormat"];
  sourceFormat: Schemas["SourceFormat"];
}

/** 파일 이름으로 볼륨 이름을 제안한다: `Ubuntu 24.04.vmdk` → `ubuntu-24-04` */
export function nameFromFile(file: string): string {
  return file
    .replace(/\.[^.]*$/, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, 63)
    .replace(/-+$/, "");
}

/** 확장자로 원본 형식을 짐작한다. 모르면 raw다(`.img`, `.raw`, `.iso`). */
export function sourceFormatOf(file: string): Schemas["SourceFormat"] {
  const ext = file.toLowerCase().split(".").pop();
  return ext === "vmdk" ? "vmdk" : ext === "qcow2" ? "qcow2" : "raw";
}

/** 다 보낸 뒤 이만큼 응답이 없으면 알림을 바꾼다. 요청은 끊지 않는다(끊으면 서버가 받은 볼륨을 지운다). */
export const NO_RESPONSE_MS = 2 * 60 * 1000;

/**
 * 본문을 보내기 전에 서버가 받을 수 있는지 묻는다(`/volumes/import/check`, 204). 권한·이름·풀·공간 따위로
 * 거절하면 `ApiError`를 던진다.
 */
export async function checkUpload(size: number, params: UploadParams, signal?: AbortSignal) {
  await call(
    api.POST("/volumes/import/check", {
      params: {
        query: {
          name: params.name,
          pool: params.pool,
          format: params.format,
          source_format: params.sourceFormat,
          size,
        },
      },
      signal,
    }),
  );
}

/** `sendFile`의 진행률·다 보냄·취소 */
export interface SendOptions {
  onProgress?(loaded: number, total: number): void;
  onSent?(): void;
  signal?: AbortSignal;
}

/**
 * 이미지를 `/volumes/import`로 올린다. 받은 Task(202)로 끝난다. 그 밖은 `sendFile`과 같다. 중간에 끊기면
 * 서버가 만들던 볼륨과 받은 파일을 지운다.
 */
export function uploadImage(file: Blob, params: UploadParams, options: SendOptions): Promise<Task> {
  const query = new URLSearchParams({
    name: params.name,
    pool: params.pool,
    format: params.format,
    source_format: params.sourceFormat,
  });
  // Task가 아닌 본문(프록시 따위)은 실패로 본다.
  return sendFile("/volumes/import", query, file, 202, isTask, options);
}

function isTask(body: unknown): body is Task {
  return typeof (body as Partial<Task> | null)?.id === "string";
}

/**
 * 파일을 API `path`로 올린다. fetch는 올리는 진행률을 알려 주지 않아 `XMLHttpRequest`를 쓴다.
 * 본문을 다 보내면 `onSent`를 부른다(빈 파일이어도). `status` 응답의 본문이 `accept`를 통과하면 그것으로
 * 끝나고, 그 밖의 응답은 `ApiError`, 연결 실패는 `NetworkError`, 취소는 `signal`의 이유로 거부한다.
 */
export function sendFile<T>(
  path: string,
  query: URLSearchParams,
  file: Blob,
  status: number,
  accept: (body: unknown) => body is T,
  { onProgress, onSent, signal }: SendOptions,
): Promise<T> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(signal.reason);
      return;
    }
    const xhr = new XMLHttpRequest();
    const sentAt = currentAuthEpoch();
    const onAbort = () => xhr.abort();
    signal?.addEventListener("abort", onAbort);
    const done = () => signal?.removeEventListener("abort", onAbort);
    xhr.open("POST", `${window.location.origin}/api/v1${path}?${query}`);
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.upload.onprogress = (e) => onProgress?.(e.loaded, e.total || file.size);
    xhr.upload.onload = () => onSent?.();
    xhr.onload = () => {
      done();
      let body: unknown = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        // 프록시의 HTML 오류 페이지 따위
      }
      if (xhr.status === status && accept(body)) {
        resolve(body);
        return;
      }
      if (xhr.status === 401) {
        reportUnauthorized(sentAt);
      }
      reject(toApiError(xhr.status, body));
    };
    xhr.onerror = () => {
      done();
      reject(new NetworkError(new Error("upload failed")));
    };
    xhr.onabort = () => {
      done();
      reject(signal?.reason ?? new DOMException("upload aborted", "AbortError"));
    };
    xhr.send(file);
  });
}

/** 진행 중인 업로드: 알림 id → 취소. `silent`면 취소를 알리지 않는다. */
const active = new Map<string, { controller: AbortController; silent: boolean }>();

/** 올리는 중에 페이지를 떠나려 하면 브라우저가 확인한다. */
function guard(e: BeforeUnloadEvent) {
  e.preventDefault();
  // 옛 브라우저
  e.returnValue = "";
}

/** 업로드를 모두 취소한다. 로그아웃하거나 세션이 끝나면 부른다(다음 사용자에게 이어지지 않게). */
export function resetUploads() {
  for (const upload of active.values()) {
    upload.silent = true;
    upload.controller.abort();
  }
}

/**
 * 이미지 업로드를 시작한다(`runUpload`). 다 올리면 볼륨을 채우는 Task를 `trackTask`로 넘긴다.
 */
export function startUpload(file: File, params: UploadParams, t: TFunction) {
  runUpload(
    file,
    {
      name: params.name,
      check: (signal) => checkUpload(file.size, params, signal),
      send: (options) => uploadImage(file, params, options),
      done: (task, id) => {
        toast.dismiss(id);
        trackTask(task, t("tasks.op.volume-import", { target: params.name }));
      },
      noResponse: t("uploads.noResponse", { name: params.name }),
    },
    t,
  );
}

/** `runUpload`이 할 일 */
export interface UploadJob<T> {
  /** 알림에 보일 이름 */
  name: string;
  /** 본문을 보내기 전에 서버가 받을 수 있는지 묻는다. 거절하면 던진다. */
  check(signal: AbortSignal): Promise<void>;
  /** 파일을 보낸다(`sendFile`). */
  send(options: SendOptions): Promise<T>;
  /** 받은 결과를 알림 `id`와 함께 넘긴다. 던지면 업로드 실패로 알린다. */
  done(result: T, id: string): void;
  /** 다 보낸 뒤 오래 답이 없을 때의 알림 */
  noResponse: string;
}

/**
 * 업로드를 시작하고 진행률과 취소 버튼을 알림으로 보여 준다. 화면을 옮겨도 계속되고, 진행 중에는
 * 페이지를 떠나려 하면 확인한다. 끝나면 결과를 `job.done`으로 넘긴다.
 */
export function runUpload<T>(file: File, job: UploadJob<T>, t: TFunction) {
  const id = `upload-${crypto.randomUUID()}`;
  const controller = new AbortController();
  const cancel = { label: t("common.cancel"), onClick: () => controller.abort() };
  if (active.size === 0) {
    window.addEventListener("beforeunload", guard);
  }
  const upload = { controller, silent: false };
  active.set(id, upload);
  let shown = -1;
  let processing = false;
  let waiting: ReturnType<typeof setTimeout> | undefined;
  // 다 보냈다. 서버가 파일을 쓰고 Task를 만드는 동안은 취소하지 않는다.
  const sentAll = () => {
    processing = true;
    toast.loading(t("uploads.processing", { name: job.name }), {
      id,
      action: undefined,
      duration: Infinity,
    });
    // 오래 답이 없으면 닫을 수 있는 알림으로 바꾼다. 늦게 오는 응답은 평소처럼 알린다.
    waiting = setTimeout(() => {
      toast.warning(job.noResponse, {
        id,
        duration: Infinity,
        closeButton: true,
      });
    }, NO_RESPONSE_MS);
  };
  const show = (loaded: number, total: number) => {
    if (processing) {
      return;
    }
    // 화면 낭독기가 덜 읽도록 5% 단위로만 바꾼다.
    const percent = total > 0 ? Math.floor((loaded / total) * 20) * 5 : 0;
    if (percent === shown) {
      return;
    }
    shown = percent;
    toast.loading(
      t("uploads.progress", {
        name: job.name,
        percent,
        loaded: formatBytes(loaded),
        total: formatBytes(total),
      }),
      { id, action: cancel, duration: Infinity },
    );
  };
  show(0, file.size);
  // 확인에서 거절되면 본문을 보내지 않는다.
  let sent = false;
  void job
    .check(controller.signal)
    .then(() => {
      sent = true;
      return job.send({ onProgress: show, onSent: sentAll, signal: controller.signal });
    })
    .then((result) => job.done(result, id))
    // 넘기다 던져도 알림이 "올리는 중"에 남지 않게 여기서 함께 받는다.
    // `trackTask`에서 던졌다면 서버의 가져오기 Task는 이미 돌고 있을 수 있다.
    .catch((err: unknown) => {
      // 같은 id로 바꾸는 알림은 이전 옵션을 물려받으므로 취소 버튼을 지운다.
      if (upload.silent) {
        toast.dismiss(id);
      } else if (controller.signal.aborted) {
        toast.info(t("uploads.cancelled", { name: job.name }), {
          id,
          action: undefined,
          duration: 4000,
        });
      } else {
        // 서버가 본문을 다 받기 전에 거절해도(401·409·507 따위) 브라우저는 연결 끊김으로만 알린다.
        const error =
          sent && err instanceof NetworkError ? t("uploads.dropped") : errorMessage(t, err);
        toast.error(t("uploads.failed", { name: job.name, error }), {
          id,
          action: undefined,
          duration: Infinity,
          closeButton: true,
        });
      }
    })
    .finally(() => {
      clearTimeout(waiting);
      active.delete(id);
      if (active.size === 0) {
        window.removeEventListener("beforeunload", guard);
      }
    });
}
```

`web/src/tasks/TasksPage.test.tsx`에서

```tsx

it("shows HA recoveries with the failed node's name", async () => {
```

을 다음으로 바꾼다:

```tsx

it("names import tasks by the new VM and the import", async () => {
  const imp = "019a0f3c-0000-7000-8008-000000000001";
  mockLists({
    "vm-import": [{ id: imp, name: "web01" }],
    task: [
      {
        // 가져오기의 대상도 아직 없는 새 VM이다.
        id: "019a0f3c-8a10-7000-8000-000000000001",
        spec: {
          op: {
            kind: "vm-import",
            import: imp,
            run: { name: "web", pool: "local", keep_macs: true },
          },
          target: "019a0f3c-0000-7000-8000-00000000cafe",
          node: "n1",
        },
        status: { state: "running", error: null },
      },
      {
        id: "019a0f3c-8a10-7000-8000-000000000002",
        spec: { op: { kind: "vm-import-delete" }, target: imp, node: "n1" },
        status: { state: "running", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  expect(await screen.findByText("Import VM web")).toBeInTheDocument();
  expect(await screen.findByRole("link", { name: "Delete import web01" })).toHaveAttribute(
    "href",
    "/imports/web01",
  );
});

it("shows HA recoveries with the failed node's name", async () => {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx

/** Task 대상의 이름. VM이면 VM 화면으로 이어진다. */
export function useTaskTarget() {
```

을 다음으로 바꾼다:

```tsx

/** Task 대상의 이름. VM이면 VM 화면으로, 가져오기면 가져오기 화면으로 이어진다. */
export function useTaskTarget() {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
  const backups = useList("backup");
  return useMemo(() => {
```

을 다음으로 바꾼다:

```tsx
  const backups = useList("backup");
  const imports = useList("vm-import");
  return useMemo(() => {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
    const backupById = new Map(backups.data?.map((b) => [b.id, b]));
    return (task: Task) => {
```

을 다음으로 바꾼다:

```tsx
    const backupById = new Map(backups.data?.map((b) => [b.id, b]));
    const importById = new Map(imports.data?.map((i) => [i.id, i]));
    return (task: Task) => {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
      }
      const name = volumeById.get(task.spec.target)?.name ?? backupById.get(task.spec.target)?.name;
```

을 다음으로 바꾼다:

```tsx
      }
      // 가져오기도 같다.
      if (op.kind === "vm-import") {
        return { name: op.run.name, href: undefined };
      }
      const imp = importById.get(task.spec.target);
      if (imp) {
        return { name: imp.name, href: `/imports/${encodeURIComponent(imp.name)}` };
      }
      const name = volumeById.get(task.spec.target)?.name ?? backupById.get(task.spec.target)?.name;
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
    };
  }, [vms.data, volumes.data, backups.data]);
}
```

을 다음으로 바꾼다:

```tsx
    };
  }, [vms.data, volumes.data, backups.data, imports.data]);
}
```

`web/src/test/fixtures.ts`에서

```ts
  Vm,
  Volume,
```

을 다음으로 바꾼다:

```ts
  Vm,
  VmImport,
  Volume,
```

`web/src/test/fixtures.ts`에서

```ts
    status: { next_run: 1_793_761_200, run: null, last_run: null, errors: [], ...extra.status },
  };
}
```

을 다음으로 바꾼다:

```ts
    status: { next_run: 1_793_761_200, run: null, last_run: null, errors: [], ...extra.status },
  };
}

/**
 * 테스트용 VM 가져오기: 노드 n1에 올린 `web01.ova`. 원본은 vSphere의 Linux VM(2 vCPU·소켓당 코어 2,
 * 1 GiB, UEFI, SCSI 디스크 하나, "VM Network"의 vmxnet3 NIC 하나)이고, 제안은 `propose` 결과다.
 */
export function vmImport(
  name: string,
  n: number,
  status: Partial<VmImport["status"]> = {},
): VmImport {
  return {
    id: `019a0f3c-0000-7000-8008-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { node: "n1", source: { ova: { file_name: `${name}.ova`, size_bytes: 3 * 1024 ** 3 } } },
    status: {
      phase: "ready",
      source_vm: {
        name: "Web 01",
        guest_os: "ubuntu64Guest",
        firmware: "uefi",
        vcpus: 2,
        cores_per_socket: 2,
        memory_mib: 1024,
        disks: [
          {
            name: "disk0",
            file: "web01-disk1.vmdk",
            capacity_bytes: 16 * 1024 ** 3,
            controller: "scsi",
          },
        ],
        nics: [{ network: "VM Network", adapter: "vmxnet3", mac: "00:50:56:01:02:03" }],
      },
      proposal: {
        sockets: 1,
        cores: 2,
        memory_mib: 1024,
        firmware: "uefi",
        disk_bus: "virtio-blk",
        nic_model: "virtio",
        networks: { "VM Network": null },
      },
      warnings: [
        "UEFI boot entries (NVRAM) are not imported; the guest boots from its default boot path",
      ],
      ...status,
    },
  };
}
```

`web/src/test/render.tsx`에서

```tsx
  "backup-job": "/backup-jobs",
};
```

을 다음으로 바꾼다:

```tsx
  "backup-job": "/backup-jobs",
  "vm-import": "/vm-imports",
};
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 407개

Run: `pnpm -C web build`
Expected: 성공

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): VM 가져오기 화면 — 사이드바 \"가져오기\", 목록(이름·노드·원본 VM·상태와 오류·만든 VM 링크), OVA 올리기(파일 이름에서 이름 제안, 사전 확인 후 진행률 알림, 끝나면 항목 열기), 항목 화면(원본 요약·디스크와 NIC 표·경고·오류, 제안으로 채운 실행 폼과 원본 네트워크마다 매핑 필수, Task 추적, 지우기), 이미지 올리기의 XHR·진행률 알림을 함께 쓰게 나눔, watch 종류·권한·Task 이름 vm-import, ko·en"
```

---

### Task 7: 실제 시험과 문서

`crates/opld/tests/vm_import.rs`(기능 `qemu-it`): 표식을 쓴 디스크로 streamOptimized VMDK·vSphere 모양 OVF·SHA256 `.mf`·tar를 만들어 `import ova` → `import get`(제안) → `import run --net lan` → 멈춘 VM(UEFI, 1×2, virtio-blk, 원본 MAC)과 볼륨의 표식, `done`, 원본 디렉터리 없음; `.mf`가 틀리면 실행이 실패하고 `ready` + error, 볼륨이 남지 않음. README "VMware에서 가져오기", 설계 "구현(계획 29)" 메모와 알려진 틈, 처음 분해 문서(하위 프로젝트 8 진행).

**Files:**
- Create: `crates/opld/tests/vm_import.rs`
- Modify: `README.md`
- Modify: `crates/opld/Cargo.toml`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-04-vmware-import-design.md`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown

## 웹 UI
```

을 다음으로 바꾼다:

````markdown

## VMware에서 가져오기

VMware VM을 OVA 파일로 내보내 opLite로 가져온다. 가져오기(`vm-import`)는 세 단계다: OVA를 올리면 opld가 OVF를 읽어
원본 VM(CPU, 메모리, 펌웨어, 디스크, NIC)과 opLite VM 제안을 보이고, 사용자가 이름·풀·네트워크 매핑과 고칠 값을 정해
실행하면 디스크를 볼륨으로 옮겨 **꺼진** 새 VM을 만든다. 실행이 실패하면 원본이 남으므로 값을 바꿔 다시 실행한다.

**OVA 만들기.** VMware에서 VM을 먼저 끈다(스냅샷이 있으면 통합하거나 지운다).

- vSphere Client의 "Export OVF Template"은 `.ovf`·`.mf`·`.vmdk` 파일들을 내려받는다(OVA 하나가 아니다). 그 폴더를
  `ovftool`로 OVA 하나로 묶는다: `ovftool web01/web01.ovf web01.ova`. `ovftool`로 바로 내보내도 된다:
  `ovftool vi://user@vcenter/DC/vm/web01 web01.ova`.
- VMware Workstation·Fusion은 "File → Export to OVF…"에서 파일 이름을 `.ova`로 정한다.
- 디스크는 VMDK(streamOptimized·monolithicSparse)여야 한다. 압축(`ovf:compression`)·분할(`ovf:chunkSize`) 디스크,
  VM 여럿이 든 OVA, 디스크가 32개 또는 NIC가 16개를 넘는 VM은 받지 않는다(400 `invalid_ova`).

```bash
oplctl import ova web01.ova --name web01        # 먼저 받을 수 있는지 묻고 올린다(이 노드에 원본이 남는다)
oplctl import list
# NAME   NODE   SOURCE  PHASE  VM
# web01  node1  web01   ready  -
oplctl import get web01                         # 원본 VM, 제안, 디스크와 NIC, 경고
# proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs
oplctl import run web01 --name web --pool local --net "VM Network=lan"   # 디스크를 볼륨으로 옮겨 꺼진 VM web을 만든다
oplctl import run web01 --name web --pool local --net lan --disk-bus sata --nic-model e1000e --memory 4096 --new-macs
oplctl vm start web
oplctl import delete web01                      # 가져오기와 남은 원본 파일을 지운다(만든 VM은 남는다)
```

- **제안.** 게스트 OS(`vmw:osType`)가 Windows(`win…`)면 디스크 `sata`·NIC `e1000e`(Windows가 드라이버를 갖고 있다),
  그 밖은 `virtio-blk`·`virtio`다. vCPU는 원본의 소켓당 코어로 나누고(나누어떨어지지 않으면 소켓 하나), 메모리는
  그대로(최소값보다 작으면 최소값), 펌웨어는 원본의 `efi`면 `uefi`, 아니면 `bios`, 머신은 `q35`, CPU 모델은 `host`다.
  첫 디스크로 켠다(디스크가 여러 컨트롤러에 걸쳐 있으면 첫 디스크가 부트 디스크인지 확인하라는 경고가 나온다). SATA
  디스크가 6개를 넘으면 `virtio-blk`를 제안한다(AHCI 포트 수, 실행 때 `--disk-bus sata`로 6개를 넘기면 400).
  CD-ROM·플로피·USB 같은 장치는 가져오지 않고 경고로 보인다. `import run`의 `--disk-bus`·`--nic-model`·`--firmware`·`--sockets`·`--cores`·`--memory`가
  제안을 바꾼다.
- **네트워크 매핑.** 원본 NIC의 네트워크마다 opLite 네트워크를 정해야 한다(`--net "원본=opLite"`, 원본 네트워크가
  하나면 `--net lan`만). 이름이 같은 opLite 네트워크가 있으면 제안에 들어 있고, 실행 때 그 원본 네트워크를 주지
  않으면 제안대로 매핑한다. 매핑도 제안도 없거나 없는 네트워크면 400이다.
- **MAC과 원본 끄기.** 게스트의 고정 IP·라이선스가 MAC에 묶인 경우가 많아 원본 MAC을 그대로 쓴다. **VMware의 원본
  VM을 끈 뒤에** 가져온 VM을 켠다(같은 네트워크에 같은 MAC이 둘이면 안 된다). 새 MAC을 받으려면 `--new-macs`다.
  다른 opLite VM이 같은 MAC을 쓰면 409 `mac_in_use`다(실행 작업도 디스크를 옮기기 전에 다시 본다).
- **Windows.** 처음에는 제안대로 `sata`·`e1000e`로 켠다. virtio로 바꾸려면 게스트에
  [virtio-win](https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/) 드라이버를 먼저 넣고(VM에
  작은 `virtio-blk` 볼륨을 하나 더 붙여 켜면 Windows가 드라이버를 붙인다), VM을 끄고 웹의 VM 편집(또는 API
  `PUT /api/v1/vms/{name}`)으로 디스크 버스·NIC 모델을 바꾼다. 드라이버 없이 부트 디스크를 `virtio-blk`로 바꾸면 Windows가 켜지지 않는다
  (`INACCESSIBLE_BOOT_DEVICE`).
- **VMware Tools.** 게스트 안에서 지운다(Windows: 앱 및 기능, Linux: `open-vm-tools` 패키지). Linux 게스트는 virtio
  드라이버를 커널에 갖고 있지만, 네트워크 장치 이름이 바뀌어(`ens192` → `enp1s0` 등) 네트워크 설정을 고쳐야 할 수 있다.
- **UEFI 부트 항목.** UEFI 변수(NVRAM)는 옮기지 않는다. 대부분 OS 기본 경로(`\EFI\BOOT\BOOTX64.EFI` 등)로 켜지지만,
  켜지지 않으면 콘솔에서 펌웨어 메뉴(켤 때 Esc)의 Boot Manager로 OS 부트 로더를 고른다.
- **원본은 올린 노드에 남는다.** OVA는 접속한 노드의 `{data_dir}/uploads/imports/{가져오기 id}/source.ova`에 두고(올리기에
  파일 크기 + 1 GiB, 실행에 가장 큰 디스크 파일 + 1 GiB의 여유가 필요하다, 아니면 507; 실행 작업도 디스크를 꺼내기
  전마다 다시 보고 모자라면 실패한다), 실행도 그 노드에서 한다. 그래서
  풀은 그 노드에서 쓸 수 있어야 하고(409 `pool_not_on_node`), 그 노드가 꺼져 있으면 실행할 수 없다(409
  `node_offline`). 위트니스 노드에는 올릴 수 없다(400). 실행이 끝나면 원본을 지운다. 원본 노드가 클러스터에서 빠졌거나
  꺼진 채 돌아오지 않으면 `oplctl import delete web01 --force`로 기록만 지운다(원본 파일은 그 노드에 남는다).
- **볼륨.** 디스크마다 OVA에서 꺼내(`.mf`가 있으면 SHA-1·SHA-256·SHA-384·SHA-512를 확인한다; 모르는 알고리즘이거나
  `.mf`에 없는 디스크는 확인하지 않고 경고한다) 볼륨 `{VM 이름}-disk{i}`(풀의 기본 형식:
  dir·NFS는 qcow2, 블록 풀은 raw)로 옮긴다. 해시가 다르면 실행이 실패하고("checksum mismatch for …") 그때까지 만든
  볼륨을 지운다. 실행은 디스크 크기만큼 걸리므로 작업(Task) 시간 제한이 없고, `oplctl import run`은 24시간까지 기다린다.
- **상태.** `ready`(실행할 수 있다; 실패한 실행의 이유는 `error`), `importing`(실행 중, 지울 수 없다 — 409
  `import_busy`), `done`(만든 VM이 `vm`에), `failed`(원본을 쓸 수 없다, 예: opld가 다시 시작할 때 원본 파일이 없음 —
  지우고 다시 올린다). 실행 중에 opld가 멈추면 다시 시작할 때 `ready`로 돌아간다(VM을 이미 만들었으면 `done`;
  그렇게 끝나지 못한 가져오기는 다시 실행하면 `done`이 되고 409 `import_done`이다).
  원본 노드가 클러스터에서 빠져 실행 작업이 실패로 끝나면 `importing`에 남지만, 끝나지 않은 작업이 없으면 다시
  실행하거나 지울 수 있다(노드가 없으면 `--force`). 웹의 지우기 단추는 `importing`에서도 누를 수 있고, 아직 실행 중이면
  서버가 `import_busy`로 거절한다.
- **아키텍처.** 가져온 VM은 x86 `q35`다. aarch64(macOS) 노드에도 가져올 수는 있지만 켜려면 x86_64 노드가 필요하다.
- 웹은 사이드바 "가져오기" 화면에서 한다(목록, "OVA 올리기" — 진행률, 이름은 파일 이름에서; 항목 화면 — 원본 요약·경고,
  이름·풀·CPU·메모리·펌웨어·디스크 버스·NIC 모델·네트워크 매핑·MAC 유지, 실행과 진행, 만든 VM 링크, 지우기). 강제
  지우기는 `oplctl`로 한다. API는 `POST /api/v1/vm-imports/ova/check?name=&size=`, `POST /api/v1/vm-imports/ova?name=`
  (본문 OVA, 201), `GET /api/v1/vm-imports`, `GET`·`DELETE /api/v1/vm-imports/{name}`(`?force=true`),
  `POST /api/v1/vm-imports/{name}/run`(`{"name": …, "pool": …, "networks": {…}, …}`, 202 작업)이고, 읽기는 모든 사용자,
  올리기·실행·지우기는 operator 이상이다.
- **한계.** 실행 중인 VMware VM 옮기기, 증분 동기화, VMware 스냅샷 체인, VMware Tools 제거·virtio 드라이버 넣기(게스트에서
  한다), OVA 서명(`.cert`, 확인하지 않고 경고), 압축·분할 디스크, 여러 VM이 든 OVA, VM 일괄 가져오기는 없다. ESXi·vCenter에서
  바로 가져오기는 계획 30에서 온다.
- **남을 수 있는 것.** 실행이 볼륨을 다 만든 뒤 VM을 만들기 전에 opld가 죽으면 `{VM 이름}-disk{i}` 볼륨이 남는다(복제·복원과
  같다) — `oplctl volume delete`로 지운다.

## 웹 UI
````

`README.md`에서

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`README.md`에서

```markdown
cargo test -p opld --features qemu-it --test backup                 # E2E: 실행 중 백업 → VM 지우기 → 복원(qemu-img, qemu-io 필요)
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
```

을 다음으로 바꾼다:

```markdown
cargo test -p opld --features qemu-it --test backup                 # E2E: 실행 중 백업 → VM 지우기 → 복원(qemu-img, qemu-io 필요)
cargo test -p opld --features qemu-it --test vm_import              # E2E: OVA 올리기 → 제안 → 가져오기 → 볼륨 내용(qemu-img 필요)
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
```

`README.md`에서

```markdown
- [백업 설계 (하위 프로젝트 7: 계획 27·28)](docs/superpowers/specs/2026-11-04-backup-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [백업 설계 (하위 프로젝트 7: 계획 27·28)](docs/superpowers/specs/2026-11-04-backup-design.md)
- [VMware 가져오기 설계 (하위 프로젝트 8: 계획 29·30)](docs/superpowers/specs/2026-11-04-vmware-import-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`crates/opld/Cargo.toml`에서

```toml
serde_json.workspace = true
tempfile.workspace = true
```

을 다음으로 바꾼다:

```toml
serde_json.workspace = true
sha2.workspace = true
tempfile.workspace = true
```

`crates/opld/tests/vm_import.rs`를 만든다:

```rust
//! 실제 opld로 VMware OVA를 가져온다: 시험이 만든 OVA(표식을 쓴 디스크의 streamOptimized VMDK, vSphere
//! 모양 OVF, SHA256 `.mf`)를 `oplctl import ova`로 올리고 제안을 본 뒤 `import run`으로 꺼진 VM과 볼륨을
//! 만든다. 볼륨을 `qemu-img convert`로 raw로 빼서 표식이 있는지 본다. `.mf` 해시가 틀린 OVA는 실행이
//! 실패하고 볼륨을 남기지 않는다. 가져온 VM은 켜지 않는다(게스트 OS가 없고, aarch64 macOS 노드는 x86
//! 게스트를 돌리지 못한다).
//! `cargo test -p opld --features qemu-it --test vm_import`
//! QEMU(`qemu-it`와 같다)와 qemu-img가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;
#[allow(dead_code)]
#[path = "../../opl-api/tests/common/ustar.rs"]
mod ustar;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ustar::Tar;

/// 디스크 앞에 써 두는 표식의 길이
const MARK: usize = 64 * 1024;
/// 원본 디스크 크기(4 MiB)
const DISK_BYTES: usize = 4 << 20;
const MAC: &str = "00:50:56:8a:12:34";

/// 클러스터를 만들고 로그인해 dir 풀 `local`과 네트워크 `lan`을 만든다. VM은 켜지 않으므로 브리지는 없어도
/// 된다.
async fn setup(d: &mut Daemon) {
    d.start().await;
    d.init("import").await;
    d.login().await;
    let dir = d.path("local");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&["network", "create", "lan", "--bridge", "br0"])
        .await
        .unwrap();
}

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// 앞 `MARK` 바이트가 `mark`인 4 MiB 디스크의 streamOptimized VMDK(vSphere가 OVA에 넣는 형식)
fn vmdk(d: &Daemon, mark: u8) -> Vec<u8> {
    let raw = d.path("src.raw");
    let mut data = vec![0u8; DISK_BYTES];
    data[..MARK].fill(mark);
    std::fs::write(&raw, data).unwrap();
    let out = d.path("src.vmdk");
    qemu_img(&[
        "convert",
        "-f",
        "raw",
        "-O",
        "vmdk",
        "-o",
        "subformat=streamOptimized",
        raw.to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    let vmdk = std::fs::read(&out).unwrap();
    std::fs::remove_file(raw).unwrap();
    std::fs::remove_file(out).unwrap();
    vmdk
}

/// vSphere "Export OVF Template" 모양의 OVF: Linux 게스트, 2 vCPU(소켓당 코어 2), 1024 MiB, efi, SCSI
/// 디스크 `web01-disk1.vmdk`(4 MiB), "VM Network"의 vmxnet3 NIC([`MAC`])
fn ovf() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Envelope xmlns="http://schemas.dmtf.org/ovf/envelope/2" xmlns:ovf="http://schemas.dmtf.org/ovf/envelope/2" xmlns:rasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_ResourceAllocationSettingData" xmlns:sasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_StorageAllocationSettingData" xmlns:epasd="http://schemas.dmtf.org/wbem/wscim/1/cim-schema/2/CIM_EthernetPortAllocationSettingData" xmlns:vmw="http://www.vmware.com/schema/ovf">
  <References><File ovf:href="web01-disk1.vmdk" ovf:id="file1"/></References>
  <DiskSection>
    <Info>Virtual disk information</Info>
    <Disk ovf:capacity="4" ovf:capacityAllocationUnits="byte * 2^20" ovf:diskId="vmdisk1" ovf:fileRef="file1" ovf:format="http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized"/>
  </DiskSection>
  <NetworkSection><Info>Networks</Info><Network ovf:name="VM Network"/></NetworkSection>
  <VirtualSystem ovf:id="web01">
    <Info>A virtual machine</Info>
    <Name>web01</Name>
    <OperatingSystemSection ovf:id="101" vmw:osType="ubuntu64Guest"><Info>Guest OS</Info></OperatingSystemSection>
    <VirtualHardwareSection>
      <Info>Virtual hardware requirements</Info>
      <Item>
        <rasd:ElementName>2 virtual CPU(s)</rasd:ElementName>
        <rasd:InstanceID>1</rasd:InstanceID>
        <rasd:ResourceType>3</rasd:ResourceType>
        <rasd:VirtualQuantity>2</rasd:VirtualQuantity>
        <vmw:CoresPerSocket ovf:required="false">2</vmw:CoresPerSocket>
      </Item>
      <Item>
        <rasd:AllocationUnits>byte * 2^20</rasd:AllocationUnits>
        <rasd:ElementName>1024MB of memory</rasd:ElementName>
        <rasd:InstanceID>2</rasd:InstanceID>
        <rasd:ResourceType>4</rasd:ResourceType>
        <rasd:VirtualQuantity>1024</rasd:VirtualQuantity>
      </Item>
      <Item>
        <rasd:Address>0</rasd:Address>
        <rasd:ElementName>SCSI Controller 0</rasd:ElementName>
        <rasd:InstanceID>3</rasd:InstanceID>
        <rasd:ResourceSubType>VirtualSCSI</rasd:ResourceSubType>
        <rasd:ResourceType>6</rasd:ResourceType>
      </Item>
      <StorageItem>
        <sasd:AddressOnParent>0</sasd:AddressOnParent>
        <sasd:ElementName>Hard disk 1</sasd:ElementName>
        <sasd:HostResource>ovf:/disk/vmdisk1</sasd:HostResource>
        <sasd:InstanceID>4</sasd:InstanceID>
        <sasd:Parent>3</sasd:Parent>
        <sasd:ResourceType>17</sasd:ResourceType>
      </StorageItem>
      <EthernetPortItem>
        <epasd:Address>{MAC}</epasd:Address>
        <epasd:Connection>VM Network</epasd:Connection>
        <epasd:ElementName>Network adapter 1</epasd:ElementName>
        <epasd:InstanceID>5</epasd:InstanceID>
        <epasd:ResourceSubType>VmxNet3</epasd:ResourceSubType>
        <epasd:ResourceType>10</epasd:ResourceType>
      </EthernetPortItem>
      <vmw:Config ovf:required="false" vmw:key="firmware" vmw:value="efi"/>
    </VirtualHardwareSection>
  </VirtualSystem>
</Envelope>
"#
    )
}

fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// OVF, `.mf`(SHA256), 디스크 순서의 OVA 파일 `web01.ova`를 만든다. `wrong_digest`면 `.mf`의 디스크
/// 해시를 틀리게 적는다.
fn write_ova(d: &Daemon, disk: &[u8], wrong_digest: bool) -> PathBuf {
    let ovf = ovf();
    let digest = match wrong_digest {
        true => sha256(b"something else"),
        false => sha256(disk),
    };
    let mf = format!(
        "SHA256(web01.ovf)= {}\nSHA256(web01-disk1.vmdk)= {digest}\n",
        sha256(ovf.as_bytes())
    );
    let ova = Tar::new()
        .file("web01.ovf", ovf.as_bytes())
        .file("web01.mf", mf.as_bytes())
        .file("web01-disk1.vmdk", disk)
        .finish();
    let path = d.path("web01.ova");
    std::fs::write(&path, ova).unwrap();
    path
}

/// 볼륨 `name`의 파일(풀 `local`)
async fn volume_file(d: &Daemon, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let format = volume["spec"]["format"].as_str().unwrap();
    d.path("local")
        .join(format!("{}.{format}", volume["id"].as_str().unwrap()))
}

/// 이미지의 앞 `MARK` 바이트. raw로 빼서 읽는다.
fn head(d: &Daemon, file: &Path) -> Vec<u8> {
    let raw = d.path("head.raw");
    qemu_img(&[
        "convert",
        "-O",
        "raw",
        file.to_str().unwrap(),
        raw.to_str().unwrap(),
    ]);
    let bytes = std::fs::read(&raw).unwrap();
    std::fs::remove_file(&raw).unwrap();
    bytes[..MARK].to_vec()
}

/// 가져오기의 원본 디렉터리(올린 노드의 `{data_dir}/uploads/imports/{id}`)
fn import_dir(d: &Daemon, import: &Value) -> PathBuf {
    d.data_dir()
        .join("uploads/imports")
        .join(import["id"].as_str().unwrap())
}

/// OVA를 올리면 원본 VM을 읽어 제안을 만들고(Linux라 virtio, efi라 uefi, 2 vCPU를 소켓 1·코어 2로, 같은
/// 이름 네트워크가 없어 매핑 없음), 실행하면 디스크를 볼륨 `web-disk0`으로 옮기고 원본 MAC을 쓰는 꺼진
/// VM `web`을 만든다. 끝나면 가져오기는 `done`이고 원본 디렉터리는 없다. 가져오기를 지워도 VM은 남는다.
#[tokio::test]
async fn ova_import_creates_a_stopped_vm_with_the_disk_contents() {
    let mut d = Daemon::new("node1", "");
    setup(&mut d).await;
    let disk = vmdk(&d, 0xab);
    let ova = write_ova(&d, &disk, false);

    let out = d
        .ctl(&["import", "ova", ova.to_str().unwrap(), "--name", "web01"])
        .await
        .unwrap();
    assert_eq!(
        out,
        "vm-import web01 uploaded; review it with `oplctl import get web01`\n"
    );
    let import = d.ctl_json(&["import", "get", "web01"]).await;
    let status = &import["status"];
    assert_eq!(status["phase"], "ready", "{import}");
    assert_eq!(
        import["spec"]["source"]["ova"],
        json!({ "file_name": "web01.ova", "size_bytes": std::fs::metadata(&ova).unwrap().len() }),
        "{import}"
    );
    assert_eq!(status["source_vm"]["guest_os"], "ubuntu64Guest", "{import}");
    assert_eq!(
        status["source_vm"]["disks"][0]["capacity_bytes"], DISK_BYTES,
        "{import}"
    );
    assert_eq!(
        status["proposal"]["networks"],
        json!({ "VM Network": null })
    );
    let warnings = status["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("NVRAM")),
        "{import}"
    );
    assert!(
        warnings.iter().any(|w| w.as_str().unwrap().contains("MAC")),
        "{import}"
    );
    let dir = import_dir(&d, &import);
    assert!(dir.join("source.ova").exists());
    let text = d.ctl(&["import", "get", "web01"]).await.unwrap();
    assert!(
        text.contains(
            "proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs\n"
        ),
        "{text}"
    );

    // 원본 네트워크가 하나뿐이라 `--net lan`이 그것을 잇는다.
    let out = d
        .ctl(&[
            "import", "run", "web01", "--name", "web", "--pool", "local", "--net", "lan",
        ])
        .await
        .unwrap();
    assert_eq!(out, "vm web imported\n");

    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    let spec = &vm["spec"];
    assert_eq!(spec["power"], "stopped", "{vm}");
    assert_eq!(spec["firmware"], "uefi", "{vm}");
    assert_eq!(spec["machine"], "q35", "{vm}");
    assert_eq!(spec["memory_mib"], 1024, "{vm}");
    assert_eq!(
        (&spec["cpu"]["sockets"], &spec["cpu"]["cores"]),
        (&json!(1), &json!(2))
    );
    let disks = spec["disks"].as_array().unwrap();
    assert_eq!(disks.len(), 1, "{vm}");
    assert_eq!(disks[0]["volume"], "web-disk0", "{vm}");
    assert_eq!(disks[0]["bus"], "virtio-blk", "{vm}");
    assert_eq!(disks[0]["boot_order"], 1, "{vm}");
    let nics = spec["nics"].as_array().unwrap();
    assert_eq!(nics.len(), 1, "{vm}");
    assert_eq!(nics[0]["network"], "lan", "{vm}");
    assert_eq!(nics[0]["model"], "virtio", "{vm}");
    assert_eq!(nics[0]["mac"], MAC, "{vm}");

    // 볼륨에 원본 디스크의 내용이 있다.
    let volume = d.ctl_json(&["volume", "get", "web-disk0"]).await;
    assert_eq!(volume["spec"]["size_bytes"], DISK_BYTES, "{volume}");
    let file = volume_file(&d, "web-disk0").await;
    assert_eq!(head(&d, &file), vec![0xab; MARK]);

    let done = d.ctl_json(&["import", "get", "web01"]).await;
    assert_eq!(done["status"]["phase"], "done", "{done}");
    assert_eq!(done["status"]["vm"], vm["id"], "{done}");
    assert!(done["status"]["error"].is_null(), "{done}");
    assert!(!dir.exists(), "source directory left");
    let list = d.ctl(&["import", "list"]).await.unwrap();
    assert!(
        list.lines()
            .nth(1)
            .is_some_and(|l| l.split_whitespace().collect::<Vec<_>>()
                == ["web01", "node1", "web01", "done", "web"]),
        "{list}"
    );

    // 가져오기를 지워도 만든 VM과 볼륨은 남는다.
    let out = d.ctl(&["import", "delete", "web01"]).await.unwrap();
    assert_eq!(out, "vm-import web01 deleted\n");
    assert_eq!(d.ctl_json(&["import", "list"]).await, json!([]));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "web-disk0"]).await.unwrap();
    d.stop();
}

/// `.mf`의 디스크 해시가 틀린 OVA는 올릴 때는 받지만(해시는 실행 때 본다) 실행이 실패한다: VM과 볼륨을
/// 남기지 않고, 가져오기는 `ready`로 돌아가 이유를 남기며 원본은 그대로다.
#[tokio::test]
async fn checksum_mismatch_fails_the_run_and_leaves_no_volume() {
    let mut d = Daemon::new("node1", "");
    setup(&mut d).await;
    let disk = vmdk(&d, 0xab);
    let ova = write_ova(&d, &disk, true);
    d.ctl(&["import", "ova", ova.to_str().unwrap(), "--name", "web01"])
        .await
        .unwrap();

    let err = d
        .ctl(&[
            "import",
            "run",
            "web01",
            "--name",
            "web",
            "--pool",
            "local",
            "--net",
            "VM Network=lan",
        ])
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("checksum mismatch for web01-disk1.vmdk"),
        "{err:#}"
    );

    let import = d.ctl_json(&["import", "get", "web01"]).await;
    assert_eq!(import["status"]["phase"], "ready", "{import}");
    assert!(
        import["status"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("checksum mismatch for web01-disk1.vmdk")),
        "{import}"
    );
    assert!(import["status"]["vm"].is_null(), "{import}");
    assert_eq!(d.ctl_json(&["vm", "list"]).await, json!([]));
    assert_eq!(d.ctl_json(&["volume", "list"]).await, json!([]));
    let left: Vec<_> = std::fs::read_dir(d.path("local")).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    let files: Vec<_> = std::fs::read_dir(import_dir(&d, &import))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files, ["source.ova"]);

    d.ctl(&["import", "delete", "web01"]).await.unwrap();
    assert!(!import_dir(&d, &import).exists(), "source directory left");
    d.stop();
}
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
구현: 하위 프로젝트 7은 계획 27(백업과 복원: dir·NFS 풀의 `backups` 표시, `Backup` 리소스, 실행 중·꺼진 VM의 crash-consistent 전체 백업을 디스크마다 압축 qcow2와 `manifest.json`으로, 새 VM으로 복원, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 28(백업 작업과 보존: `BackupJob`(대상 VM 고르기·UTC 일정·보존 규칙), 리더의 `BackupJobScheduler`, 노드당 동시 백업 제한, Proxmox식 보존 규칙으로 지우기, API·oplctl·웹, 실제 시험, 완료)으로 나눴다(`2026-11-04-backup-design.md`). 하위 프로젝트 7은 끝났다. 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존 VM 덮어쓰기는 넣지 않았다.
```

을 다음으로 바꾼다:

```markdown
구현: 하위 프로젝트 7은 계획 27(백업과 복원: dir·NFS 풀의 `backups` 표시, `Backup` 리소스, 실행 중·꺼진 VM의 crash-consistent 전체 백업을 디스크마다 압축 qcow2와 `manifest.json`으로, 새 VM으로 복원, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 28(백업 작업과 보존: `BackupJob`(대상 VM 고르기·UTC 일정·보존 규칙), 리더의 `BackupJobScheduler`, 노드당 동시 백업 제한, Proxmox식 보존 규칙으로 지우기, API·oplctl·웹, 실제 시험, 완료)으로 나눴다(`2026-11-04-backup-design.md`). 하위 프로젝트 7은 끝났다. 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존 VM 덮어쓰기는 넣지 않았다.

구현: 하위 프로젝트 8은 계획 29(OVA 가져오기: `VmImport` 리소스, OVA 올리기와 손으로 읽는 tar·OVF 해석, 원본 VM에서 opLite VM 제안(게스트 OS로 디스크 버스·NIC 모델, 코어 나누기, 경고), 사용자가 고친 값으로 디스크를 `.mf` 해시를 보며 추출해 볼륨으로 옮기고 꺼진 VM을 만드는 `VmImport` Task, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 30(ESXi·vCenter에서 바로 가져오기: 지문을 확인하는 HTTPS 연결, 데이터스토어의 `.vmx`·디스크립터 해석, 같은 `VmImport`와 Task, 예정)으로 나눴다(`2026-11-04-vmware-import-design.md`). 하위 프로젝트 8은 진행 중이다.
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown

### 2.2 올리기와 OVF 해석
```

을 다음으로 바꾼다:

```markdown

구현(계획 29): `opl_types::vm_import`의 `VmImport = Resource<VmImportSpec { node, source: ImportSource }, VmImportStatus { phase: ImportPhase(Ready(기본)|Importing|Done|Failed), source_vm: Option<SourceVm>, proposal: Option<ImportProposal>, warnings, vm: Option<Uuid>, error: Option<String> }>`(빈 값은 빼고 쓴다), `Kind::VmImport`(`vm-import`, watch 종류에도 있다). `ImportSource`는 외부 태그 enum이고 지금은 `Ova(OvaSource { file_name, size_bytes })`(JSON `{"ova": {…}}`)뿐이다 — 계획 30이 `esxi` 변형을 더한다. `SourceVm { name, guest_os, firmware, vcpus, cores_per_socket, memory_mib, disks: Vec<SourceDisk { name: "disk{i}", file(tar 항목 이름), capacity_bytes, controller }>, nics: Vec<SourceNic { network, adapter(소문자), mac: Option<MacAddr> }> }`. 제안은 VmSpec 초안 대신 `ImportProposal { sockets, cores, memory_mib, firmware, disk_bus, nic_model, networks: BTreeMap<원본 네트워크, Option<opLite 네트워크>> }`이고, 실행 때 VmSpec을 만든다(§2.4). 이름 `ova`는 예약(400, 경로 `/vm-imports/ova`와 겹친다). 원본은 `{upload_dir}/imports/{id}/source.ova`(`upload_dir` = `{data_dir}/uploads`), 실행 중 추출 파일은 같은 디렉터리의 `disk.vmdk`다. 노드 시작 때 정리(`vm_imports::sweep`, Task 실행기의 업로드 sweep 안에서 API를 띄우기 전)는 리소스가 없는 `imports/*`를 지우고, 이 노드의 가져오기 가운데 끝나지 않은 `VmImport` Task가 없는 `importing`을 `ready` + "the import was interrupted"로(그 가져오기의 Task 대상 VM이 이미 있으면 — VM을 만든 뒤 `done`을 쓰기 전에 죽었다 — `done` + `vm`으로 끝내고 원본 디렉터리를 지운다; 끝나지 않은 Task가 있으면 실행기가 그 Task를 실패로 끝내며 정리한다), `source.ova`가 없는 `ready`를 `failed` + "source files are gone"으로 바꾼다. 볼륨 업로드 sweep은 `imports` 디렉터리를 건드리지 않는다.

### 2.2 올리기와 OVF 해석
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown
- `.mf`가 있으면 해석 때는 형식만 보고, 실행 때 디스크를 추출하면서 SHA-256(또는 SHA-1)을 비교한다. 다르면 실행 실패.
```

을 다음으로 바꾼다:

```markdown
- `.mf`가 있으면 해석 때는 형식만 보고, 실행 때 디스크를 추출하면서 SHA-256(또는 SHA-1)을 비교한다. 다르면 실행 실패.

구현(계획 29): opl-api `vm_imports/ova.rs`·`ovf.rs`(순수·동기, 새 의존성 `roxmltree`·`sha1`). tar는 손으로 읽는다: 512바이트 헤더(체크섬 확인), 이름은 ustar `prefix/name`, 크기는 8진수 또는 base-256, `0`·`\0`은 파일, `5`(디렉터리)·`g`(pax 전역)는 건너뛰고, `x`(pax)의 `path`·`size`와 `L`(GNU 긴 이름)은 다음 파일·디렉터리 항목에 쓴다(사이의 다른 pax·`L` 헤더는 가져가지 않는다; 둘 다 있으면 pax `path`). 그 밖의 종류, 겹친 이름, 파일을 넘는 크기, 4 KiB를 넘는 이름, 10 000개를 넘는 헤더는 400. 끝은 0 블록 또는 파일 끝. 항목 표는 tar 순서의 `Vec`과 이름 색인(`HashMap`)이다. `parse_ova`는 `.ovf` 정확히 하나(4 MiB 이하, UTF-8), `.mf` 0~1개(1 MiB 이하; 줄 `SHA256(file)= hex`처럼 `SHA1`·`SHA256`·`SHA384`·`SHA512`, 공백 차이 허용; 모르는 알고리즘의 줄은 오류가 아니라 그 디스크를 확인하지 않고 경고 "the manifest uses an unsupported digest for X; it is not verified", `.mf`에 없는 디스크는 경고 "X is not listed in the manifest; it is not verified"), `.cert`가 있으면 경고 "the OVA signature (.cert) is not verified", OVF `References`의 파일이 모두 tar에 있어야 한다. OVF는 요소·속성을 로컬 이름으로 비교한다(OVF 1.x·2.x와 도구마다 접두어가 다르다). `vmw:osType`이 없으면 CIM 번호가 Windows일 때 `windows`, 아니면 `Description` 또는 `other`. 메모리 단위는 `byte * 2^N`·`byte * N`·`MegaBytes` 등, 디스크 `ovf:format`에 `vmdk`가 없으면 400(VMDK 하위 형식은 실행 때 `check_import_source`가 본다). 컨트롤러 종류는 ResourceType 5 ide, 6 scsi, 20 sata(하위 종류에 `nvme`가 있으면 nvme). 디스크 순서는 컨트롤러 종류가 처음 나온 순서, 같은 종류는 `rasd:Address` 순서(없으면 나온 순서), 그 안에서 `AddressOnParent` 순서이고, 디스크가 둘 이상의 컨트롤러에 걸쳐 있으면 경고 "disks are on more than one controller; check that disk0 is the boot disk". 디스크가 32개, NIC가 `MAX_NICS`(16)개를 넘으면 400. 화면(24)과 VMCI는 조용히 빼고, 플로피·CD-ROM·USB와 그 밖의 장치는 경고("… is not imported"). 해석 경고는 겹치지 않게 50개까지이고 넘으면 마지막에 "… and N more". API는 opl-api `vm_imports.rs`: `POST /vm-imports/ova/check?name&size`(204), `POST /vm-imports/ova?name[&file_name]`(201, `file_name` 기본 `{name}.ova` — oplctl·웹은 실제 파일 이름을 보낸다). 둘 다 `Kind::VmImport` 쓰기 권한, 이름 검증·409 `name_taken`, 위트니스 400, 여유 `size + UPLOAD_RESERVE_BYTES`(507)를 본다. 받기는 볼륨 업로드의 `receive`를 쓰고, `spawn_blocking`으로 해석한 뒤 그때의 네트워크 이름으로 `propose`해 해석 경고와 제안 경고를 `warnings`에 넣는다. 해석 실패(`OvaError::Invalid`)는 400 `invalid_ova`, 읽기 실패(`Io`)는 500이고, 실패하거나 연결이 끊기면(future가 버려지면) 디렉터리를 지운다. `GET /vm-imports`, `GET /vm-imports/{key}`는 모든 사용자.
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown
SATA 디스크가 6개를 넘으면 제안은 `virtio-blk`로 하고 경고한다(q35 AHCI 포트 수).
```

을 다음으로 바꾼다:

```markdown
SATA 디스크가 6개를 넘으면 제안은 `virtio-blk`로 하고 경고한다(q35 AHCI 포트 수).

구현(계획 29): `opl_types::vm_import::{is_windows, propose(&SourceVm, &[있는 네트워크 이름]) -> (ImportProposal, 경고)}`. 소켓당 코어 `c`가 0이거나 vCPU를 나누지 못하면 `1 × n`. 메모리가 최소값(`MIN_MEMORY_MIB`)보다 작으면 최소값 + 경고, u32에 들어가지 않으면 최소값 + 경고 "memory is too large (…); set memory_mib when running the import". UEFI면 경고 "UEFI boot entries (NVRAM) are not imported; the guest boots from its default boot path". SATA 포트는 6개로 보고 디스크가 7개 이상이면 `virtio-blk` + 경고. 원본 NIC에 MAC이 하나라도 있으면 경고 "power off the VMware VM before starting the imported one; it keeps the same MAC addresses". 네트워크는 이름이 같은 opLite 네트워크가 있으면 그것, 없으면 `null`(실행 때 필수).
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown

### 2.5 그 밖의 API·oplctl·웹
```

을 다음으로 바꾼다:

```markdown

구현(계획 29): `ImportRun { name, pool, networks(생략하면 빈 표), disk_bus?, nic_model?, firmware?, sockets?, cores?, memory_mib?, keep_macs(기본 true) }`(`deny_unknown_fields`). 실행 API는 `Kind::VmImport`·VM·볼륨 쓰기 권한을 보고, 이 가져오기의 끝나지 않은 Task(실행·지우기)가 있으면 409 `import_busy`(`importing`이어도 끝나지 않은 Task가 없으면 — 원본 노드가 없어져 janitor가 Task를 실패로 끝내 정리가 돌지 않았다 — 쉬는 것으로 본다), `done`이면 `import_done`, `failed`(또는 원본 VM이 없음, 원본 파일이 없음)면 `import_failed`, 원본 노드가 없거나 오프라인이면 `node_offline`(백업의 `check_online`). `ready`이거나 쉬는 `importing`인데 이 가져오기의 끝난 실행 Task의 대상 VM이 있으면(VM을 만든 뒤 `done`으로 바꾸지 못했다 — 시작 정리의 `made_vm`과 같은 판단) 가져오기를 `done` + `vm`으로 끝내고(원본이 이 노드에 있으면 원본 디렉터리도 지운다, 다른 노드의 것은 지우기 Task가 지운다) 409 `import_done`. 그다음 VM 이름(409 `name_taken`)과 볼륨 이름 `{name}-disk{i}`(409 `name_taken`)를 보고, VM 스펙을 `vm_imports::import_spec(&SourceVm, &ImportProposal, &ImportRun, 볼륨 이름들)`로 만든다 — 실행 값이 없으면 제안 값, cpu 모델 `host`, 머신 `q35`, 첫 디스크 `boot_order: 1`, NIC는 매핑한 네트워크(요청에 없으면 제안의 같은 이름 매핑)와 `keep_macs`면 원본 MAC(없으면 만들 때 새 MAC), 보안 그룹 없음, 꺼짐, 노드·예약·HA 없음. 매핑도 제안도 없으면 400 `no network mapping for "X"`, `sata`인데 디스크가 6개를 넘으면 400, 스펙 검증 실패와 없는 네트워크·풀은 400, 풀은 원본 노드에서 써야 하고(`check_pool_on`: 409 `pool_not_on_node`·`pool_unavailable`), 원본이 이 노드에 있으면 OVA 안 가장 큰 디스크 파일 크기 + `UPLOAD_RESERVE_BYTES`의 여유(507), 원본 MAC이 다른 VM과 겹치면 409 `mac_in_use`. 그다음 revision을 검사해 `importing`(error 비움)으로 쓰고(겹치면 409 `import_busy`) Task `TaskOp::VmImport { import, run }`(`vm-import`, 대상 = 미리 정한 새 VM id, 노드 = `spec.node`)를 만든다. Task를 만들지 못하면 `ready`로 되돌린다. Task는 opl-api `tasks/import.rs`: VM 큐가 아니라 가져오기 id 큐(같은 가져오기의 실행·지우기가 겹치지 않는다), 시간 제한 없음. VM 이름·풀과 `keep_macs`면 원본 MAC 겹침(디스크를 옮기기 전에 실패)을 다시 보고 `parse_ova`로 index와 `.mf`를 다시 읽어 디스크마다 업로드 디렉터리 여유(항목 크기 + `UPLOAD_RESERVE_BYTES`, 아니면 "not enough space in the upload directory to extract {file}")를 보고 `extract`(복사·fsync하며 해시, 다르면 추출 파일을 지우고 "checksum mismatch for {file}") → `check_import_source(Vmdk)` → 볼륨(크기는 OVF의 capacity로 만들고 `ready`로 바꿀 때 `mark_ready`가 실제 가상 크기로 고친다, 형식은 풀의 기본 형식 `default_format`, `status.node`는 이 노드, 이름은 복제의 `volume_name`)을 `Made`에 먼저 적고 만들고 `import_volume` → 추출 파일 지움. 볼륨은 VM을 만들기 바로 전에 `ready`로 바꾸고, VM은 복제의 `create_vm`으로 만든다. 끝나면 가져오기 `done` + `vm` + error 비움, 원본 디렉터리를 지운다. 실패 정리(`import::discard`)는 대상 VM이 있는지 읽지 못하면 읽을 때까지 다시 시도하고(있는 VM의 볼륨을 지우지 않게), 대상 VM이 이미 있으면(VM을 만든 뒤 opld가 죽었다) 볼륨·VM을 두고 가져오기를 `done` + `vm`으로 끝내며 원본 디렉터리를 지우고, 아니면 복제의 정리로 만든 볼륨·VM을 지우고 추출 파일을 지우고, 가져오기가 `importing`이면 `ready` + error(Task 오류 그대로, 예: `cannot import disk "disk1": checksum mismatch for web-disk2.vmdk`)로 되돌린다.

### 2.5 그 밖의 API·oplctl·웹
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown
- 웹: 사이드바 "가져오기" 페이지. 목록과 "OVA 올리기"(진행률). 항목을 열면 마법사: ① 원본 요약과 경고 ② 이름·풀·CPU·메모리·펌웨어·디스크 버스·NIC 모델 ③ 네트워크 매핑(원본 네트워크마다 선택) ④ 확인 → 실행, Task 진행 → 만든 VM 링크. 올리기는 지금의 볼륨 올리기처럼 UI를 내준 노드로 간다(그 노드에 원본이 남는다).
```

을 다음으로 바꾼다:

```markdown
- 웹: 사이드바 "가져오기" 페이지. 목록과 "OVA 올리기"(진행률). 항목을 열면 마법사: ① 원본 요약과 경고 ② 이름·풀·CPU·메모리·펌웨어·디스크 버스·NIC 모델 ③ 네트워크 매핑(원본 네트워크마다 선택) ④ 확인 → 실행, Task 진행 → 만든 VM 링크. 올리기는 지금의 볼륨 올리기처럼 UI를 내준 노드로 간다(그 노드에 원본이 남는다).

구현(계획 29): `DELETE /vm-imports/{key}` → 202 Task `vm-import-delete`(`TaskOp::VmImportDelete`, 대상 = 가져오기 id, 원본 노드, 가져오기 큐; 디렉터리를 지우고 리소스를 지운다, 디렉터리가 없어도 된다). 이 가져오기의 끝나지 않은 Task가 있으면(`force`여도, phase는 보지 않는다) 409 `import_busy`, 원본 노드가 없거나 오프라인이면 409 `node_offline`이고 `?force=true`면 리소스만 지우고 200 `VmImportForgotten { message }`("… its files are left on node …", 백업 지우기의 force와 같다). 만든 VM과 볼륨은 남는다. 권한: 올리기·실행·지우기는 `RoleAuthorizer`의 `Kind::VmImport` 쓰기(operator 이상; 실행은 VM·볼륨 쓰기도), 읽기는 모두. OpenAPI 태그 `vm-imports`, 웹 `schema.d.ts`. oplctl `import ova FILE --name N`(크기로 `check` → 실제 파일 이름과 함께 올림, 진행률 표시는 없다 — 볼륨 올리기와 같다; 출력 ``vm-import N uploaded; review it with `oplctl import get N` ``), `import list`(열 `NAME NODE SOURCE PHASE VM`, SOURCE는 원본 VM 이름 또는 파일 이름), `import get N`(머리 줄, `source vm: …`, `proposal: S sockets x C cores, M MiB, FW, BUS disks, MODEL NICs`, 디스크 표 `DISK FILE SIZE CONTROLLER`, NIC 표 `NETWORK ADAPTER MAC PROPOSED`, 경고), `import run N --name V --pool P [--net SOURCE=NETWORK | --net NETWORK(원본 네트워크가 하나일 때)]… [--disk-bus] [--nic-model] [--firmware] [--sockets] [--cores] [--memory] [--new-macs]`(Task를 24시간 `IMPORT_WAIT`까지 기다림, 출력 "vm V imported"), `import delete N [--force]`(출력 "vm-import N deleted" 또는 서버 알림). 웹은 사이드바 "가져오기"(VM 아래): `/imports`(목록: 이름·노드·원본 VM·상태와 오류·만든 VM 링크, "OVA 올리기" 대화상자 — 파일 이름에서 확장자를 빼고 이름 규칙에 맞춘 이름 제안, 사전 확인 뒤 볼륨 업로드와 같은 진행률 알림(`storage/upload.ts`를 함께 쓰게 나눔), 끝나면 항목을 연다), `/imports/:name`(원본 요약·디스크와 NIC 표·경고·오류, 제안으로 채운 한 페이지 폼 — 이름·풀·소켓·코어·메모리·펌웨어·디스크 버스·NIC 모델·원본 네트워크마다 필수 매핑·MAC 유지, 실행 → Task 진행 → VM 링크, 지우기 — `importing`이어도 누를 수 있고 서버가 `import_busy`로 정한다, 실행을 보내는 동안만 막는다; `name_taken`은 서버 메시지를 `imports.errors.nameTaken`에 끼워 이름 칸에 보인다). 강제 지우기는 웹에 없다(안내만). watch 종류·권한·Task 종류 문구 `vm-import`·`vm-import-delete`, ko·en.

구현(계획 29) 시험: opl-types 단위(직렬화 모양, 제안 표), opl-api 단위(tar 항목 표, vSphere 2.x·Workstation 1.x 모양 OVF 견본 `tests/fixtures/*.ovf`, 거절, `.mf`, `extract` 해시)와 시험용 ustar 작성기 `crates/opl-api/tests/common/ustar.rs`(opld 시험도 `#[path]`로 쓴다), `tests/vm_imports.rs`(API: 올리기·검사·sweep·실행 검사·Task 없는 `importing`의 실행·지우기·지우기, 대상 VM이 있는 가져오기를 `done`으로 끝내는 시작 정리와 실행 요청), `tests/vm_import_tasks.rs`(`storage-it`, MockHypervisor·dir 풀·`qemu-img`로 만든 streamOptimized VMDK: 가져오기·`.mf` 불일치 정리·MAC 겹침으로 일찍 실패한 뒤 고친 값으로 재실행·추출 공간 부족·지우기; CI `check (ubuntu)` storage-it 단계), oplctl 단위(인자 → 본문, 표 출력, 작은 시험 서버), 웹 시험. 실제 시험 `crates/opld/tests/vm_import.rs`(`qemu-it`, macOS HVF와 CI linux-kvm — linux-kvm 작업은 `-p opld`의 모든 시험을 돌리므로 목록을 바꿀 필요가 없다): opld 하나, dir 풀 `local`, 네트워크 `lan`(브리지 `br0` — VM을 켜지 않으므로 없어도 된다). 시험이 4 MiB raw 디스크 앞 64 KiB에 표식을 쓰고 `qemu-img convert -O vmdk -o subformat=streamOptimized`, vSphere 모양 OVF(Linux `ubuntu64Guest`, 2 vCPU·소켓당 코어 2, 1024 MiB, efi, SCSI 디스크, "VM Network"의 vmxnet3 NIC와 MAC), SHA256 `.mf`를 ustar로 묶는다. `oplctl import ova` → `import get`(phase `ready`, 파일 이름·크기, 제안 `1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs`, 매핑 없음, NVRAM·MAC 경고, `source.ova` 있음) → `import run --net lan` → VM `web`(꺼짐, uefi, q35, 1×2, 디스크 하나 `web-disk0` virtio-blk boot 1, NIC `lan` virtio 원본 MAC), 볼륨 크기 4 MiB이고 `qemu-img convert -O raw`로 뺀 앞 64 KiB가 표식, 가져오기 `done`·`vm` = VM id, 원본 디렉터리 없음, 목록 행, 지워도 VM과 볼륨이 남음. 두 번째 시험: `.mf`의 디스크 해시를 틀리게 적은 OVA는 올리기는 되고 `import run --net "VM Network=lan"`이 "checksum mismatch for web01-disk1.vmdk"로 실패, 가져오기 `ready` + 같은 error, VM·볼륨 없음, 풀 디렉터리 비어 있음, 원본 디렉터리에 `source.ova`만, 지우면 디렉터리가 사라진다. VM은 켜지 않는다(게스트 OS가 없고 aarch64 macOS 노드는 x86 게스트를 돌리지 못한다).

알려진 틈(계획 29): 볼륨을 `ready`로 만든 뒤 VM을 만들기 전에 opld가 죽으면 볼륨이 남는다(복제·복원과 같다 — `oplctl volume delete`). 원본은 올린 노드에만 있으므로 그 노드가 꺼져 있으면 실행할 수 없고, 노드가 빠지면 force 지우기로 기록만 지우며 파일은 그 노드에 남는다. 실행에는 업로드 디렉터리에 원본 OVA와 추출한 디스크 하나가 함께 있어야 한다(실행 API는 원본 노드에서 받았을 때 가장 큰 디스크 기준으로, Task는 디스크마다 다시 본다). 풀 용량은 미리 보지 않는다(복제·복원과 같다 — 모자라면 옮기다 실패하고 정리한다). 노드가 시작할 때의 정리와 그사이 들어온 실행 요청이 짧게 겹치면 실행이 `ready`로 되돌려질 수 있다(다시 실행하면 된다). `failed` 가져오기는 다시 쓸 수 없다(지우고 다시 올린다). OVA 서명(`.cert`)은 확인하지 않는다. oplctl 올리기는 진행률을 보이지 않는다. MAC 겹침·이름은 Task가 확인한 뒤 VM을 만들기 전에 바뀔 수 있다(VM을 만들 때 실패하고 정리한다). 올리기에서 가져오기 리소스를 저장소에 만드는 동안 클라이언트가 끊으면(만들기는 됐는데 응답 전에 요청이 버려지면) 올리기 정리가 원본 디렉터리를 지워 `source.ova` 없는 `ready` 가져오기가 남을 수 있다(실행은 원본 노드에서 받으면 409 `import_failed`, 노드가 다시 시작할 때 `failed`가 된다 — 지우고 다시 올린다).
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1077개, 실패 0

Run: `cargo test -p opld --features qemu-it --test vm_import`
Expected: PASS 2개(이 Mac, 실제 QEMU)

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock README.md crates docs
git commit -m "test(opld): VM 가져오기 E2E와 문서 — 실제 opld로 OVA 올리기·제안·가져오기·볼륨 표식 확인, .mf 불일치 실패와 정리, README \"VMware에서 가져오기\", 설계 \"구현(계획 29)\" 메모와 알려진 틈, 하위 프로젝트 8 진행"
```

---

## 완료 기준 (계획 29)

- vSphere·Workstation 모양 OVA를 올리면 원본 VM·제안·경고가 있는 `ready` 가져오기가 생기고, 잘못된 OVA는 400과 함께 남는 파일이 없다.
- 실행하면 원본 노드에서 디스크가 해시 확인 뒤 풀의 볼륨이 되고 멈춘 VM이 만들어지며(디스크 내용이 같다), 가져오기는 `done`이 되고 원본이 지워진다.
- 실행이 실패하면 볼륨·VM이 남지 않고 `ready` + error로 돌아가 다른 값으로 다시 실행할 수 있다. 원본 노드가 사라져도 `--force`로 기록을 지울 수 있다.
- 이 Mac과 Linux CI에서 `opl-api` `vm_import_tasks`(storage-it)와 `opld` `vm_import`(qemu-it) 시험이 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **원본 VM을 끄는 일은 사용자 몫**이다. OVA는 꺼진 VM에서 나오지만, 같은 MAC을 쓰므로 VMware 쪽 VM을 켜 두면 안 된다.
- **UEFI 부트 항목(NVRAM)은 옮기지 않는다.** 대부분 기본 부트 경로로 켜진다.
- **풀 용량은 미리 보지 않는다**(복제·복원과 같다). 모자라면 Task가 실패하고 정리한다.
- 그 밖의 제한은 설계 문서 §2와 "알려진 틈(계획 29)", README "VMware에서 가져오기" 절에 있다.
