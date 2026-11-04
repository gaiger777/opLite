# opLite 계획 30: ESXi에서 바로 가져오기 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** OVA로 내보내지 않고 ESXi·vCenter에 연결해 데이터스토어의 VM을 고르면, opLite가 `.vmx`와 디스크를 읽어 계획 29와 같은 제안을 만들고 디스크를 HTTPS로 받아 멈춘 VM을 만든다. 하위 프로젝트 8(VMware 마이그레이션 마법사)을 마친다.

**Architecture:** ESXi·vCenter의 데이터스토어 HTTP 접근(`/folder`, 기본 인증)을 쓴다. 클라이언트는 rustls + hyper로 만들고, 인증서는 시스템 CA 대신 사용자가 확인한 SHA-256 지문을 TLS 핸드셰이크 안에서 비교한다(다르면 자격 증명을 보내기 전에 끊는다). 비밀번호는 Raft 상태·Task·로그에 남기지 않고 요청을 받은 노드의 메모리에만 둔다. 가져오기 리소스·제안·실행 API·Task 뒷부분은 계획 29의 것을 그대로 쓰고, Task의 디스크 얻기 단계만 "tar에서 추출" 대신 "HTTPS로 내려받기"가 된다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9, axum 0.8, utoipa, clap, rustls 0.23(ring)·tokio-rustls·hyper 1(이미 잠금 파일에 있던 것을 직접 의존성으로); 웹은 계획 8~29와 같다. 새 외부 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-04-vmware-import-design.md` (§3과 §4·§5의 계획 30 부분, "구현(계획 30)" 메모 포함)
**선행 계획:** 계획 1~29 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| ESXi 원본 타입, `.vmx`·VMDK 디스크립터 해석, 지문 고정 HTTPS 클라이언트 | VDDK·NFC·SOAP API, 변경 블록 추적(warm migration) |
| VM 찾기·가져오기 만들기 API, 메모리 속 자격 증명, 켜짐·일시 중단 짐작 | 스냅샷 체인 가져오기(통합 필요), RDM, sparse 디스크 |
| Task의 내려받기 단계, 노드 다시 시작 때 처리 | 내려받기 취소·이어받기, 실제 vCenter 시험 |
| oplctl `import esxi-browse`·`import esxi`, 웹 대화상자, 가짜 ESXi 실제 시험, README | 자격 증명 저장·갱신, 연결 대상 허용 목록 |

## 이전 계획에서 이어받는 사항

- 계획 29: `VmImport` 리소스와 상태, `propose`, `import_spec`, 실행·지우기 API, `VmImport` Task(볼륨 만들기·VM 만들기·실패 정리·`done`), 노드 시작 sweep, oplctl `import`, 웹 `/imports`.
- opl-peer의 rustls·rcgen 사용(가짜 ESXi의 자체 서명 인증서와 TLS 수신).

## 실행 환경

계획 29와 같다. Task 시험(`opl-api` `storage-it` `tests/vm_import_tasks.rs`)과 실제 시험(`opld` `qemu-it` `tests/vm_import.rs`)은 이 Mac에서 시험 안의 가짜 ESXi로 돈다. 실제 ESXi·vCenter는 쓰지 않는다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `Cargo.lock`(`cargo build`), `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

## 설계 결정

- **지문 고정:** ESXi는 거의 자체 서명이라 시스템 CA로 검증하지 않는다. 처음 연결하면 서버의 지문을 알려 주고(409 `untrusted_certificate`), 사용자가 확인한 지문으로만 자격 증명을 보낸다.
- **비밀번호는 메모리에만:** Raft·Task·로그·오류 문구에 남지 않는다. 그 대가로 opld가 다시 시작하면 그 노드의 ESXi 가져오기는 다시 만들어야 한다.
- **믿지 않는 응답:** 목록·작은 파일·내려받기에 크기 제한과 대기 시간 제한을 두고, `.vmx`의 디스크 경로와 extent 이름은 같은 데이터스토어 안의 정해진 모양만 받는다.
- **켜짐은 짐작:** `/folder`로는 전원 상태를 알 수 없어 `.lck`·`.lck-`·`.vswp`·`.vmss`로 경고만 한다. 가져오기 전에 VMware에서 끄는 것은 사용자 몫이다.

## 파일 구조

```
crates/opl-types/src/vm_import.rs                                         # EsxiSource, EsxiConnection, normalize_fingerprint
crates/opl-api/src/{vm_imports.rs, vm_imports/{vmx.rs, esxi.rs}, tasks/import.rs, tasks.rs, state.rs, lib.rs}
crates/opl-api/tests/{vmx.rs, esxi.rs, vm_imports.rs, vm_import_tasks.rs, common/esxi.rs, fixtures/esxi{7,8}.vmx}
crates/oplctl/src/{cli.rs, client.rs}, crates/opld/{src/member.rs, tests/vm_import.rs}
web/src/imports/{EsxiImportDialog.tsx, ImportsPage.tsx, ImportPage.tsx, imports.ts, ...}, web/src/i18n/*
README.md, docs/superpowers/specs/{2026-11-04-vmware-import-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 줄 단위로 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: ESXi 원본 타입과 .vmx·디스크립터 해석

opl-types: `ImportSource::Esxi(EsxiSource { host, port, datacenter, datastore, vmx, user, fingerprint })`(비밀번호 없음), API 요청용 `EsxiConnection { host, port: 443, user, password, datacenter: "ha-datacenter", fingerprint? }`(Debug에서 비밀번호를 가림, Serialize 없음), `normalize_fingerprint`(SHA-256 → `AB:CD:…`). opl-api `vm_imports/vmx.rs`(순수): `.vmx`(`key = "value"`, 대소문자 무시, `|XX` 이스케이프, 64 KiB)에서 원본 VM(CPU·소켓당 코어·메모리·efi·`present` 디스크와 부트 순서 — `bios.hddOrder` 우선, 아니면 ide·sata·scsi·nvme·버스·유닛 — NIC와 MAC 규칙, 가져오지 않는 장치 경고)을 읽고, VMDK 디스크립터를 해석해 `vmfs`·`monolithicFlat` 단일 extent만 받는다(스냅샷·델타·RDM 거절). 디스크 경로는 VM 디렉터리 기준 상대 경로나 `/vmfs/volumes/{같은 데이터스토어}/…`만(`.`는 지우고 `..`·다른 데이터스토어·UUID 경로 거절). 웹은 새 원본 종류에 맞춰 OVA 파일 이름을 OVA일 때만 보인다.

**Files:**
- Create: `crates/opl-api/src/vm_imports/vmx.rs`
- Create: `crates/opl-api/tests/fixtures/esxi7.vmx`
- Create: `crates/opl-api/tests/fixtures/esxi8.vmx`
- Create: `crates/opl-api/tests/vmx.rs`
- Modify: `crates/opl-api/src/vm_imports.rs`
- Modify: `crates/opl-types/src/vm_import.rs`
- Modify: `web/src/imports/ImportPage.tsx`
- Modify: `web/src/imports/ImportsPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/vm_imports.rs`에서

```rust
pub mod ovf;
```

을 다음으로 바꾼다:

```rust
pub mod ovf;
pub mod vmx;
```

`crates/opl-api/src/vm_imports/vmx.rs`를 만든다:

```rust
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
        let network =
            get("networkName").ok_or_else(|| invalid(format!("ethernet{n} has no networkName")))?;
        let mac = match get("addressType") {
            Some(t) if t.eq_ignore_ascii_case("static") => get("address"),
            _ => get("generatedAddress"),
        };
        nics.push(SourceNic {
            network: network.to_string(),
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
```

`crates/opl-api/tests/fixtures/esxi7.vmx`를 만든다:

```
.encoding = "UTF-8"
config.version = "8"
virtualHW.version = "19"
vmci0.present = "TRUE"
floppy0.present = "FALSE"
numvcpus = "4"
cpuid.coresPerSocket = "2"
memSize = "8192"
bios.bootRetry.delay = "10"
firmware = "efi"
powerType.powerOff = "default"
powerType.suspend = "soft"
powerType.reset = "default"
tools.upgrade.policy = "manual"
sched.cpu.units = "mhz"
sched.cpu.affinity = "all"
vm.createDate = "1695186735512398"
scsi0.virtualDev = "pvscsi"
scsi0.present = "TRUE"
sata0.present = "TRUE"
sata0:0.deviceType = "cdrom-image"
sata0:0.fileName = "/vmfs/volumes/62f3a1b2-0c1d2e3f-4a5b-001122334455/iso/WinSrv2022.iso"
sata0:0.present = "TRUE"
scsi0:0.deviceType = "scsi-hardDisk"
scsi0:0.fileName = "win01.vmdk"
sched.scsi0:0.shares = "normal"
scsi0:0.present = "TRUE"
scsi0:1.deviceType = "scsi-hardDisk"
scsi0:1.fileName = "win01_1.vmdk"
scsi0:1.present = "TRUE"
ethernet0.virtualDev = "e1000e"
ethernet0.networkName = "VM Network"
ethernet0.addressType = "vpx"
ethernet0.generatedAddress = "00:0c:29:12:34:56"
ethernet0.uptCompatibility = "TRUE"
ethernet0.present = "TRUE"
displayName = "win01"
guestOS = "windows2019srvNext-64"
toolScripts.afterPowerOn = "TRUE"
uuid.bios = "42 1a 2b 3c 4d 5e 6f 70-81 92 a3 b4 c5 d6 e7 f8"
vc.uuid = "50 1a 2b 3c 4d 5e 6f 70-81 92 a3 b4 c5 d6 e7 f8"
nvram = "win01.nvram"
svga.present = "TRUE"
sched.mem.min = "0"
pciBridge0.present = "TRUE"
pciBridge4.present = "TRUE"
pciBridge4.virtualDev = "pcieRootPort"
```

`crates/opl-api/tests/fixtures/esxi8.vmx`를 만든다:

```
.encoding = "UTF-8"
config.version = "8"
virtualHW.version = "20"
nvram = "web01.nvram"
svga.present = "TRUE"
pciBridge0.present = "TRUE"
vmci0.present = "TRUE"
hpet0.present = "TRUE"
floppy0.present = "FALSE"
usb.present = "TRUE"
ehci.present = "TRUE"
sound.present = "FALSE"
numvcpus = "2"
cpuid.coresPerSocket = "2"
memSize = "2048"
firmware = "efi"
# 사용자가 고친 이름: 따옴표와 '#'는 VMware가 |22·|23으로 적는다
displayName = "web01 |22prod|22 |23 1"
guestOS = "ubuntu-64"
nvme0.present = "TRUE"
nvme0:0.fileName = "web01.vmdk"
nvme0:0.present = "TRUE"
nvme0:1.fileName = "/vmfs/volumes/datastore2/web01/web01_1.vmdk"
nvme0:1.present = "TRUE"
scsi0.virtualDev = "pvscsi"
scsi0.present = "TRUE"
scsi0:0.fileName = "web01_2.vmdk"
scsi0:0.present = "FALSE"
sata0.present = "TRUE"
sata0:0.deviceType = "atapi-cdrom"
sata0:0.fileName = "CD/DVD drive 0"
sata0:0.present = "TRUE"
sata0:1.deviceType = "cdrom-raw"
sata0:1.present = "TRUE"
ETHERNET0.VIRTUALDEV = "vmxnet3"
ethernet0.networkName = "VM Network"
ethernet0.addressType = "static"
ethernet0.address = "00:50:56:aa:bb:cc"
ethernet0.present = "TRUE"
ethernet1.virtualDev = "vmxnet3"
ethernet1.networkName = "Storage"
ethernet1.addressType = "generated"
ethernet1.generatedAddress = "00:0C:29:AB:CD:EF"
ethernet1.present = "TRUE"
ethernet2.networkName = "Old"
ethernet2.present = "FALSE"
ethernet3.networkName = "Lab"
ethernet3.present = "TRUE"
tools.syncTime = "FALSE"
```

`crates/opl-api/tests/vmx.rs`를 만든다:

```rust
//! ESXi `.vmx`와 VMDK 디스크립터 해석, 데이터스토어 안 디스크 경로.

use opl_api::vm_imports::vmx::{
    Descriptor, Extent, VmxDisk, check_flat, disk_path, parse_descriptor, parse_vmx, vmx_to_source,
};
use opl_types::vm::{Firmware, MAX_NICS, MacAddr};
use opl_types::vm_import::{SourceNic, SourceVm};

const ESXI7: &str = include_str!("fixtures/esxi7.vmx");
const ESXI8: &str = include_str!("fixtures/esxi8.vmx");

fn source(text: &str) -> Result<(SourceVm, Vec<String>, Vec<VmxDisk>), String> {
    let vmx = parse_vmx(text).map_err(|e| e.to_string())?;
    vmx_to_source(&vmx, "web01/web01.vmx").map_err(|e| e.to_string())
}

fn disks(text: &str) -> Vec<(String, String, String)> {
    source(text)
        .unwrap()
        .2
        .into_iter()
        .map(|d| (d.name, d.path, d.controller))
        .collect()
}

fn triple(name: &str, path: &str, controller: &str) -> (String, String, String) {
    (name.into(), path.into(), controller.into())
}

fn mac(s: &str) -> Option<MacAddr> {
    Some(s.parse().unwrap())
}

// --- .vmx ---

#[test]
fn parses_esxi8_vmx() {
    let (vm, warnings, disks) = source(ESXI8).unwrap();
    // `|22`·`|23` 이스케이프를 푼다
    assert_eq!(vm.name, "web01 \"prod\" # 1");
    assert_eq!(vm.guest_os, "ubuntu-64");
    assert_eq!(vm.firmware, Firmware::Uefi);
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (2, 2, 2048));
    // 디스크는 디스크립터를 읽은 뒤 채운다
    assert!(vm.disks.is_empty());
    assert_eq!(
        disks
            .into_iter()
            .map(|d| (d.name, d.path, d.controller))
            .collect::<Vec<_>>(),
        [
            triple("disk0", "web01.vmdk", "nvme"),
            triple(
                "disk1",
                "/vmfs/volumes/datastore2/web01/web01_1.vmdk",
                "nvme"
            ),
        ]
    );
    assert_eq!(
        vm.nics,
        [
            // static이면 address, 대문자 키도 읽는다
            SourceNic {
                network: "VM Network".into(),
                adapter: "vmxnet3".into(),
                mac: mac("00:50:56:aa:bb:cc"),
            },
            // generated면 generatedAddress
            SourceNic {
                network: "Storage".into(),
                adapter: "vmxnet3".into(),
                mac: mac("00:0c:29:ab:cd:ef"),
            },
            // 꺼진 ethernet2는 빼고, 어댑터 종류가 없으면 e1000, MAC이 없으면 None
            SourceNic {
                network: "Lab".into(),
                adapter: "e1000".into(),
                mac: None,
            },
        ]
    );
    // CD-ROM 경고는 한 번만, 꺼진 사운드·플로피는 경고하지 않는다
    assert_eq!(
        warnings,
        [
            "CD-ROM drive is not imported",
            "USB controller is not imported"
        ]
    );
}

#[test]
fn parses_esxi7_vmx() {
    let (vm, warnings, disks) = source(ESXI7).unwrap();
    assert_eq!(vm.name, "win01");
    assert_eq!(vm.guest_os, "windows2019srvNext-64");
    assert_eq!(vm.firmware, Firmware::Uefi);
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (4, 2, 8192));
    assert_eq!(
        disks
            .into_iter()
            .map(|d| (d.name, d.path, d.controller))
            .collect::<Vec<_>>(),
        [
            triple("disk0", "win01.vmdk", "scsi"),
            triple("disk1", "win01_1.vmdk", "scsi"),
        ]
    );
    // `vpx`(vCenter가 정한 주소)도 generatedAddress
    assert_eq!(
        vm.nics,
        [SourceNic {
            network: "VM Network".into(),
            adapter: "e1000e".into(),
            mac: mac("00:0c:29:12:34:56"),
        }]
    );
    assert_eq!(warnings, ["CD-ROM drive is not imported"]);
}

#[test]
fn defaults_for_a_minimal_vmx() {
    let (vm, warnings, disks) = source("memsize = 512\n").unwrap();
    // 이름이 없으면 .vmx 파일 이름
    assert_eq!(vm.name, "web01");
    assert_eq!(vm.guest_os, "other");
    assert_eq!(vm.firmware, Firmware::Bios);
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (1, 1, 512));
    assert!(vm.nics.is_empty() && disks.is_empty() && warnings.is_empty());
    let (vm, _, _) = source("memsize = 512\nfirmware = \"bios\"\n").unwrap();
    assert_eq!(vm.firmware, Firmware::Bios);
}

#[test]
fn reads_comments_case_and_unquoted_values() {
    let text = "# 주석\n\n  MemSize=\"1024\"  \nNUMVCPUS = 8 \ncpuid.CoresPerSocket = \"4\"\n\
                displayname = \"a|7cb|0a\"\nnot a key value line\nfirmware = \"EFI\"\n";
    let (vm, _, _) = source(text).unwrap();
    assert_eq!((vm.vcpus, vm.cores_per_socket, vm.memory_mib), (8, 4, 1024));
    assert_eq!(vm.name, "a|b\n");
    assert_eq!(vm.firmware, Firmware::Uefi);
    // 같은 키가 또 나오면 뒤의 것
    let (vm, _, _) = source("memsize = 1\nmemSize = \"2\"\n").unwrap();
    assert_eq!(vm.memory_mib, 2);
}

#[test]
fn rejects_bad_vmx() {
    for (text, want) in [
        ("numvcpus = 2\n", "the .vmx has no memSize"),
        ("memsize = lots\n", "invalid memSize \"lots\""),
        ("memsize = 1\nnumvcpus = -1\n", "invalid numvcpus \"-1\""),
        ("memsize = 1\0\n", "not a .vmx file"),
        (
            "memsize = 1\nethernet1.present = \"TRUE\"\n",
            "ethernet1 has no networkName",
        ),
        (
            "memsize = 1\nscsi0:0.present = \"TRUE\"\n",
            "scsi0:0 has no fileName",
        ),
        (
            "memsize = 1\nide0:0.present = \"TRUE\"\nide0:0.fileName = \"x.iso\"\n",
            "disk ide0:0 is not a VMDK: \"x.iso\"",
        ),
    ] {
        assert_eq!(source(text).unwrap_err(), want, "{text:?}");
    }
    let big = format!("memsize = 1\n{}", "#".repeat(64 << 10));
    assert_eq!(
        source(&big).unwrap_err(),
        "the .vmx file is larger than 64 KiB"
    );
}

#[test]
fn orders_disks_by_controller_bus_and_unit() {
    let mut text = String::from("memsize = 1\n");
    for dev in [
        "nvme0:0", "scsi1:0", "scsi0:2", "scsi0:10", "sata0:1", "ide1:0", "ide0:1",
    ] {
        text += &format!("{dev}.present = \"TRUE\"\n{dev}.fileName = \"{dev}.vmdk\"\n");
    }
    // 꺼진 디스크와 디스크가 아닌 장치는 뺀다
    text += "scsi0:0.present = \"FALSE\"\nscsi0:0.fileName = \"off.vmdk\"\n";
    text += "scsi0:1.present = \"TRUE\"\nscsi0:1.deviceType = \"scsi-passthru\"\n";
    let (_, warnings, d) = source(&text).unwrap();
    let order: Vec<_> = d.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        order,
        [
            "ide0:1.vmdk",
            "ide1:0.vmdk",
            "sata0:1.vmdk",
            "scsi0:2.vmdk",
            "scsi0:10.vmdk",
            "scsi1:0.vmdk",
            "nvme0:0.vmdk",
        ]
    );
    let names: Vec<_> = d.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names[0], "disk0");
    assert_eq!(names[6], "disk6");
    assert_eq!(
        warnings,
        [
            "scsi0:1 (scsi-passthru) is not imported",
            "disks are on more than one controller; check that disk0 is the boot disk",
        ]
    );
}

#[test]
fn hdd_order_puts_the_boot_disk_first() {
    let text = "memsize = 1\n\
                scsi0:0.present = \"TRUE\"\nscsi0:0.fileName = \"data.vmdk\"\n\
                scsi0:1.present = \"TRUE\"\nscsi0:1.fileName = \"boot.vmdk\"\n\
                sata0:0.present = \"TRUE\"\nsata0:0.fileName = \"other.vmdk\"\n\
                bios.hddOrder = \"scsi0:1\"\n";
    assert_eq!(
        disks(text),
        [
            triple("disk0", "boot.vmdk", "scsi"),
            triple("disk1", "other.vmdk", "sata"),
            triple("disk2", "data.vmdk", "scsi"),
        ]
    );
    let (_, warnings, _) = source(text).unwrap();
    // 부트 디스크를 정했으므로 여러 컨트롤러 경고는 없다
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn accepts_known_disk_device_types() {
    for t in ["disk", "scsi-hardDisk", "ata-hardDisk", "SCSI-HARDDISK"] {
        let text = format!(
            "memsize = 1\nsata0:0.present = \"TRUE\"\nsata0:0.deviceType = \"{t}\"\n\
             sata0:0.fileName = \"a.VMDK\"\n"
        );
        assert_eq!(disks(&text), [triple("disk0", "a.VMDK", "sata")], "{t}");
    }
}

#[test]
fn warns_about_devices_that_are_not_imported() {
    let text = "memsize = 1\nfloppy0.present = \"TRUE\"\nusb.present = \"true\"\n\
                sound.present = \"TRUE\"\nide1:0.present = \"TRUE\"\n\
                ide1:0.deviceType = \"cdrom-image\"\nide1:0.fileName = \"a.iso\"\n";
    let (_, warnings, _) = source(text).unwrap();
    assert_eq!(
        warnings,
        [
            "CD-ROM drive is not imported",
            "floppy drive is not imported",
            "USB controller is not imported",
            "sound card is not imported",
        ]
    );
}

#[test]
fn limits_disks_and_nics() {
    let with = |disks: usize, nics: usize| {
        let mut text = String::from("memsize = 1\n");
        for i in 0..disks {
            let dev = format!("scsi{}:{}", i / 15, i % 15);
            text += &format!("{dev}.present = \"TRUE\"\n{dev}.fileName = \"d{i}.vmdk\"\n");
        }
        for i in 0..nics {
            text += &format!("ethernet{i}.present = \"TRUE\"\nethernet{i}.networkName = \"n\"\n");
        }
        source(&text)
    };
    assert_eq!(with(32, MAX_NICS).unwrap().2.len(), 32);
    assert_eq!(
        with(33, 0).unwrap_err(),
        "the .vmx has 33 disks; at most 32 are supported"
    );
    assert_eq!(
        with(0, MAX_NICS + 1).unwrap_err(),
        format!(
            "the .vmx has {} network adapters; at most {MAX_NICS} are supported",
            MAX_NICS + 1
        )
    );
}

// --- 디스크립터 ---

const VMFS: &str = r#"# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=fffffffe
parentCID=ffffffff
createType="vmfs"

# Extent description
RW 8192 VMFS "web01-flat.vmdk"

# The Disk Data Base
#DDB

ddb.adapterType = "lsilogic"
ddb.geometry.cylinders = "4"
ddb.thinProvisioned = "1"
ddb.virtualHWVersion = "20"
"#;

fn descriptor(create_type: &str, extents: &str, extra: &str) -> String {
    format!(
        "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n{extra}\
         createType=\"{create_type}\"\n\n# Extent description\n{extents}\n\nddb.adapterType = \"lsilogic\"\n"
    )
}

fn flat(text: &str) -> Result<(String, u64), String> {
    let d = parse_descriptor(text).map_err(|e| e.to_string())?;
    check_flat(&d).map_err(|e| e.to_string())
}

#[test]
fn parses_vmfs_descriptor() {
    assert_eq!(
        parse_descriptor(VMFS).unwrap(),
        Descriptor {
            create_type: "vmfs".into(),
            extents: vec![Extent {
                access: "RW".into(),
                sectors: 8192,
                kind: "VMFS".into(),
                file: "web01-flat.vmdk".into(),
                offset: 0,
            }],
            parent: false,
        }
    );
    assert_eq!(flat(VMFS).unwrap(), ("web01-flat.vmdk".into(), 8192 * 512));
}

#[test]
fn accepts_monolithic_flat_with_zero_offset() {
    let text = descriptor("monolithicFlat", "RW 2097152 FLAT \"disk-flat.vmdk\" 0", "");
    assert_eq!(flat(&text).unwrap(), ("disk-flat.vmdk".into(), 1 << 30));
}

#[test]
fn rejects_snapshots_rdm_and_other_layouts() {
    let snap = "the VM has snapshots; consolidate or delete them in VMware first";
    let rdm = "raw device mappings are not supported";
    let one = "RW 8192 VMFS \"a-flat.vmdk\"";
    for (text, want) in [
        (
            descriptor(
                "vmfsSparse",
                "RW 8192 VMFSSPARSE \"a-000001-delta.vmdk\"",
                "",
            ),
            snap.to_string(),
        ),
        (
            descriptor(
                "seSparse",
                "RW 8192 SESPARSE \"a-000001-sesparse.vmdk\"",
                "",
            ),
            snap.into(),
        ),
        (
            descriptor(
                "twoGbMaxExtentSparse",
                "RW 4192256 SPARSE \"a-s001.vmdk\"",
                "",
            ),
            snap.into(),
        ),
        // 부모가 있으면 createType이 vmfs여도 스냅샷이다
        (
            descriptor("vmfs", one, "parentFileNameHint=\"a.vmdk\"\n"),
            snap.into(),
        ),
        (
            descriptor("vmfsRawDeviceMap", "RW 8192 VMFSRDM \"a-rdm.vmdk\"", ""),
            rdm.into(),
        ),
        (
            descriptor(
                "vmfsPassthroughRawDeviceMap",
                "RW 8192 VMFSRDM \"a-rdmp.vmdk\"",
                "",
            ),
            rdm.into(),
        ),
        (descriptor("vmfsRDM", one, ""), rdm.into()),
        (
            descriptor("streamOptimized", "RW 8192 SPARSE \"a.vmdk\"", ""),
            "unsupported VMDK type streamOptimized".into(),
        ),
        (
            descriptor(
                "monolithicFlat",
                "RW 8192 FLAT \"a-f001.vmdk\" 0\nRW 8192 FLAT \"a-f002.vmdk\" 0",
                "",
            ),
            "the VMDK descriptor has 2 extents; only one is supported".into(),
        ),
        (
            descriptor("vmfs", "", ""),
            "the VMDK descriptor has 0 extents; only one is supported".into(),
        ),
        (
            descriptor("monolithicFlat", "RW 8192 FLAT \"a-flat.vmdk\" 128", ""),
            "unsupported VMDK extent RW 8192 FLAT \"a-flat.vmdk\" 128".into(),
        ),
        (
            descriptor("vmfs", "RDONLY 8192 VMFS \"a-flat.vmdk\"", ""),
            "unsupported VMDK extent RDONLY 8192 VMFS \"a-flat.vmdk\" 0".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"../b/a-flat.vmdk\"", ""),
            "invalid extent file name \"../b/a-flat.vmdk\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"sub/a-flat.vmdk\"", ""),
            "invalid extent file name \"sub/a-flat.vmdk\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"a\\b.vmdk\"", ""),
            "invalid extent file name \"a\\\\b.vmdk\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \"..\"", ""),
            "invalid extent file name \"..\"".into(),
        ),
        (
            descriptor("vmfs", "RW 8192 VMFS \".\"", ""),
            "invalid extent file name \".\"".into(),
        ),
    ] {
        assert_eq!(flat(&text).unwrap_err(), want, "{text}");
    }
}

/// 이름 안의 `..`는 경로가 아니다(`/`·`\`가 없으므로 디렉터리를 벗어나지 않는다).
#[test]
fn accepts_dots_inside_extent_names() {
    let text = descriptor("vmfs", "RW 8192 VMFS \"web..01-flat.vmdk\"", "");
    assert_eq!(
        flat(&text).unwrap(),
        ("web..01-flat.vmdk".into(), 8192 * 512)
    );
}

#[test]
fn rejects_non_descriptors() {
    let not = "not a VMDK descriptor";
    // 스파스 VMDK 헤더(KDMV)처럼 바이너리
    assert_eq!(flat("KDMV\u{1}\0\0\0\u{3}").unwrap_err(), not);
    assert_eq!(flat("hello world\n").unwrap_err(), not);
    assert_eq!(
        flat(&descriptor("vmfs", "RW lots VMFS \"a-flat.vmdk\"", "")).unwrap_err(),
        "invalid VMDK extent \"RW lots VMFS \\\"a-flat.vmdk\\\"\""
    );
    assert_eq!(
        flat(&descriptor("vmfs", "RW 8192 VMFS a-flat.vmdk", "")).unwrap_err(),
        "invalid VMDK extent \"RW 8192 VMFS a-flat.vmdk\""
    );
    let big = format!("{VMFS}{}", "#".repeat(64 << 10));
    assert_eq!(
        flat(&big).unwrap_err(),
        "the VMDK descriptor is larger than 64 KiB"
    );
}

// --- 디스크 경로 ---

#[test]
fn resolves_disk_paths_in_the_datastore() {
    let path = |file: &str| disk_path("web01/web01.vmx", "datastore1", file);
    assert_eq!(path("web01.vmdk").unwrap(), "web01/web01.vmdk");
    assert_eq!(path("sub/web01.vmdk").unwrap(), "web01/sub/web01.vmdk");
    assert_eq!(
        path("/vmfs/volumes/datastore1/other/x.vmdk").unwrap(),
        "other/x.vmdk"
    );
    // `.` 조각은 뺀다.
    assert_eq!(path("./web01.vmdk").unwrap(), "web01/web01.vmdk");
    assert_eq!(path("sub/./x.vmdk").unwrap(), "web01/sub/x.vmdk");
    assert_eq!(
        path("/vmfs/volumes/datastore1/./other/x.vmdk").unwrap(),
        "other/x.vmdk"
    );
    // 맨 위의 .vmx
    assert_eq!(
        disk_path("top.vmx", "datastore1", "top.vmdk").unwrap(),
        "top.vmdk"
    );
    // 디스크립터 옆의 extent도 같은 함수로
    assert_eq!(
        disk_path("web01/web01.vmdk", "datastore1", "web01-flat.vmdk").unwrap(),
        "web01/web01-flat.vmdk"
    );
    for (file, want) in [
        (
            "/vmfs/volumes/datastore2/web01/x.vmdk",
            "disk \"/vmfs/volumes/datastore2/web01/x.vmdk\" is on another datastore",
        ),
        (
            "/vmfs/volumes/62f3a1b2-0c1d2e3f-4a5b-001122334455/web01/x.vmdk",
            "disk \"/vmfs/volumes/62f3a1b2-0c1d2e3f-4a5b-001122334455/web01/x.vmdk\" uses a datastore UUID path; it is not supported",
        ),
        (
            "/data/x.vmdk",
            "disk path \"/data/x.vmdk\" is not supported",
        ),
        (
            "/vmfs/volumes/datastore1/",
            "disk path \"/vmfs/volumes/datastore1/\" is not supported",
        ),
        ("../x.vmdk", "disk path \"../x.vmdk\" is not supported"),
        (
            "/vmfs/volumes/datastore1/a/../x.vmdk",
            "disk path \"/vmfs/volumes/datastore1/a/../x.vmdk\" is not supported",
        ),
        ("a\\x.vmdk", "disk path \"a\\\\x.vmdk\" is not supported"),
        ("", "disk path \"\" is not supported"),
    ] {
        assert_eq!(path(file).unwrap_err().to_string(), want, "{file}");
    }
}
```

`crates/opl-types/src/vm_import.rs`에서

```rust
use std::collections::BTreeMap;
```

을 다음으로 바꾼다:

```rust
use std::collections::BTreeMap;
use std::fmt;
```

`crates/opl-types/src/vm_import.rs`에서

```rust

/// 원본. JSON은 `{"ova": {...}}`다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
```

을 다음으로 바꾼다:

```rust

/// 원본. JSON은 `{"ova": {...}}` 또는 `{"esxi": {...}}`다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
```

`crates/opl-types/src/vm_import.rs`에서

```rust
    Ova(OvaSource),
}
```

을 다음으로 바꾼다:

```rust
    Ova(OvaSource),
    /// ESXi·vCenter 데이터스토어의 VM. 비밀번호는 여기에 없다(받은 노드의 메모리에만 둔다).
    Esxi(EsxiSource),
}
```

`crates/opl-types/src/vm_import.rs`에서

```rust
    pub size_bytes: u64,
}
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-types/src/vm_import.rs`에서

```rust
    #[test]
    fn empty_status_is_just_ready() {
```

을 다음으로 바꾼다:

```rust
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
```

`web/src/imports/ImportPage.tsx`에서

```tsx
  const node = nodes.data?.find((n) => n.id === imp.spec.node)?.name ?? "—";
  const { phase, error, warnings, source_vm: src } = imp.status;
```

을 다음으로 바꾼다:

```tsx
  const node = nodes.data?.find((n) => n.id === imp.spec.node)?.name ?? "—";
  const ova = "ova" in imp.spec.source ? imp.spec.source.ova : undefined;
  const { phase, error, warnings, source_vm: src } = imp.status;
```

`web/src/imports/ImportPage.tsx`에서

```tsx
            <dd>
              {imp.spec.source.ova.file_name} ({formatBytes(imp.spec.source.ova.size_bytes)}),{" "}
              {node}
```

을 다음으로 바꾼다:

```tsx
            <dd>
              {ova && `${ova.file_name} (${formatBytes(ova.size_bytes)}), `}
              {node}
```

`web/src/imports/ImportsPage.tsx`에서

```tsx
                {imp.status.source_vm?.name ?? "—"}
                <div className="text-xs text-muted-foreground">{imp.spec.source.ova.file_name}</div>
              </TableCell>
```

을 다음으로 바꾼다:

```tsx
                {imp.status.source_vm?.name ?? "—"}
                {"ova" in imp.spec.source && (
                  <div className="text-xs text-muted-foreground">
                    {imp.spec.source.ova.file_name}
                  </div>
                )}
              </TableCell>
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1096개, 실패 0

Run: `cargo test -p opl-api --test vmx`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 407개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(vm-import): ESXi 원본 타입과 .vmx·VMDK 디스크립터 해석 — ImportSource::Esxi(EsxiSource, 비밀번호 없음), EsxiConnection(요청 본문, 기본 포트 443·ha-datacenter, Debug에서 비밀번호 가림), normalize_fingerprint(SHA-256 → 대문자 콜론 구분), vm_imports/vmx.rs(parse_vmx·vmx_to_source — 이스케이프·대소문자·꺼진 장치·CD-ROM 경고·부트 순서 ide/sata/scsi/nvme와 bios.hddOrder·NIC MAC, parse_descriptor·check_flat — vmfs·monolithicFlat extent 하나만, 스냅샷·RDM 거절, disk_path — 상대·같은 데이터스토어 경로만), ESXi 7·8 모양 .vmx 견본, schema.d.ts와 웹의 OVA 원본 표시를 ova일 때만"
```

---

### Task 2: ESXi HTTPS 클라이언트와 가짜 ESXi

opl-api `vm_imports/esxi.rs`: rustls(ring) + hyper HTTP/1.1. 사용자 정의 인증서 검증기가 핸드셰이크 안에서 서버 끝 인증서의 SHA-256을 기대 지문과 비교하므로 지문이 다르면 자격 증명을 보내기 전에 끊긴다(서명 검증은 그대로, 연결마다 새 설정이라 세션 재개 없음). 지문이 없으면 `Untrusted(실제 지문)`, `probe`는 인증서만 읽는다. 기본 인증, `/folder` HTML 목록 해석(위로 가는 링크 제외, 엔티티·퍼센트 인코딩, `+`는 공백, 1 MiB), `get_small`(64 KiB, 넘으면 `TooLarge`), `download`(스트리밍·fsync, 길이가 다르면 파일을 지움), 연결 15초·응답 대기 60초. 대괄호 IPv6 호스트. 시험 도우미 `tests/common/esxi.rs`: ESXi 7/8 모양 HTML을 내주는 가짜 ESXi(rcgen 자체 서명, 기본 인증, 요청 기록).

**Files:**
- Create: `crates/opl-api/src/vm_imports/esxi.rs`
- Create: `crates/opl-api/tests/common/esxi.rs`
- Create: `crates/opl-api/tests/esxi.rs`
- Modify: `Cargo.toml`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/vm_imports.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`Cargo.toml`에서

```toml
rustls-pki-types = "1.15.1"
native-tls = "0.2.18"
```

을 다음으로 바꾼다:

```toml
rustls-pki-types = "1.15.1"
hyper = { version = "1.12.0", default-features = false, features = ["client", "http1"] }
hyper-util = { version = "0.1.21", default-features = false, features = ["tokio"] }
http-body-util = "0.1.5"
native-tls = "0.2.18"
```

`crates/opl-api/Cargo.toml`에서

```toml
hex.workspace = true
opl-cluster.workspace = true
```

을 다음으로 바꾼다:

```toml
hex.workspace = true
http-body-util.workspace = true
hyper.workspace = true
hyper-util.workspace = true
opl-cluster.workspace = true
```

`crates/opl-api/Cargo.toml`에서

```toml
roxmltree.workspace = true
serde.workspace = true
```

을 다음으로 바꾼다:

```toml
roxmltree.workspace = true
rustls.workspace = true
rustls-pki-types.workspace = true
serde.workspace = true
```

`crates/opl-api/Cargo.toml`에서

```toml
tokio.workspace = true
tracing.workspace = true
```

을 다음으로 바꾼다:

```toml
tokio.workspace = true
tokio-rustls.workspace = true
tracing.workspace = true
```

`crates/opl-api/Cargo.toml`에서

```toml
opl-qemu.workspace = true
reqwest.workspace = true
```

을 다음으로 바꾼다:

```toml
opl-qemu.workspace = true
rcgen.workspace = true
reqwest.workspace = true
```

`crates/opl-api/src/vm_imports.rs`에서

```rust

pub mod ova;
```

을 다음으로 바꾼다:

```rust

pub mod esxi;
pub mod ova;
```

`crates/opl-api/src/vm_imports/esxi.rs`를 만든다:

```rust
//! ESXi·vCenter 데이터스토어 HTTPS 클라이언트(`/folder`). 서버 인증서는 CA로 검증하지 않고 TLS
//! 핸드셰이크 안에서 SHA-256 지문을 고정해 비교한다. 지문이 다르면 자격 증명을 보내기 전에 끊는다.
//! 요청마다 새 연결을 연다(HTTP/1.1, 기본 인증).

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use opl_types::vm_import::{EsxiConnection, normalize_fingerprint};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// TCP 연결과 TLS 핸드셰이크의 시간 제한
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 응답 헤더나 본문 조각을 기다리는 최대 시간. 멈춘 서버가 요청을 붙잡고 있지 못하게 한다.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 디렉터리 목록(HTML) 응답의 최대 크기
const MAX_LISTING_BYTES: usize = 1 << 20;
/// HTML 문자 참조 `&…;`의 `&`와 `;` 사이 최대 길이(`#x10FFFF`도 들어간다)
const MAX_ENTITY: usize = 10;
/// 파일로 보는 확장자. 그 밖의 이름은 디렉터리로 보고, 목록을 읽을 때 HTML이 아니면 건너뛴다
/// (`/folder` 목록에는 파일·디렉터리 구분이 없다).
const FILE_EXTENSIONS: [&str; 20] = [
    "vmx",
    "vmdk",
    "nvram",
    "log",
    "lck",
    "vmsd",
    "vmxf",
    "vswp",
    "vmss",
    "vmsn",
    "vmem",
    "vmtx",
    "hlog",
    "iso",
    "flp",
    "ovf",
    "ova",
    "mf",
    "scoreboard",
    "vmx~",
];

/// ESXi에 연결하거나 읽을 수 없는 이유. 메시지는 사용자에게 그대로 보인다.
#[derive(Debug, thiserror::Error)]
pub enum EsxiError {
    #[error("ESXi rejected the user name or password")]
    Auth,
    #[error("{0} was not found on the ESXi host")]
    NotFound(String),
    #[error("the ESXi host answered HTTP {0}")]
    Http(u16),
    #[error("cannot connect to the ESXi host: {0}")]
    Connect(String),
    /// 믿기로 한 지문이 없다. 서버 인증서의 지문을 알려 준다.
    #[error("the ESXi certificate fingerprint {0} is not trusted yet")]
    Untrusted(String),
    #[error("the ESXi certificate fingerprint is {actual}, expected {expected}")]
    Fingerprint { expected: String, actual: String },
    /// 응답이 크기 제한을 넘었다(`path`는 사용자에게 보일 경로)
    #[error("{path} is larger than {limit} bytes")]
    TooLarge { path: String, limit: usize },
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn connect_error(e: impl std::fmt::Display) -> EsxiError {
    EsxiError::Connect(e.to_string())
}

/// `/folder` 목록의 항목
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// 인증서 DER의 SHA-256 지문(대문자 콜론 구분, ESXi 화면과 같은 모양)
pub fn fingerprint_of(der: &[u8]) -> String {
    let hex = hex::encode_upper(Sha256::digest(der));
    hex.as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(":")
}

/// 서버 인증서의 지문만 읽고 끊는다(자격 증명은 보내지 않는다).
pub async fn probe(host: &str, port: u16) -> Result<String, EsxiError> {
    match connect(host, port, None).await {
        Err(EsxiError::Untrusted(actual)) => Ok(actual),
        Err(e) => Err(e),
        Ok(_) => Err(connect_error("the TLS handshake did not stop")),
    }
}

/// 디렉터리 목록. `datastore`가 없으면 데이터센터의 데이터스토어 목록(`dir`은 무시), 있으면 그
/// 데이터스토어 안 `dir`(`""`은 맨 위)의 바로 아래 항목. 응답이 HTML이 아니면(파일) 빈 목록이다.
pub async fn list(
    conn: &EsxiConnection,
    datastore: Option<&str>,
    dir: &str,
) -> Result<Vec<Entry>, EsxiError> {
    let dir = dir.trim_matches('/');
    let path = if datastore.is_some() { dir } else { "" };
    let response = get(conn, datastore, path).await?;
    let html = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("html"));
    if !html {
        return Ok(Vec::new());
    }
    let what = display_path(datastore, path);
    let body = read_limited(response.into_body(), MAX_LISTING_BYTES, &what).await?;
    Ok(parse_listing(
        &String::from_utf8_lossy(&body),
        datastore,
        dir,
    ))
}

/// 작은 텍스트 파일(`.vmx`·디스크립터)을 읽는다. `limit` 바이트를 넘으면 오류다.
pub async fn get_small(
    conn: &EsxiConnection,
    datastore: &str,
    path: &str,
    limit: usize,
) -> Result<String, EsxiError> {
    let what = display_path(Some(datastore), path);
    let response = get(conn, Some(datastore), path).await?;
    let body = read_limited(response.into_body(), limit, &what).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// 파일을 `dest`로 받는다(스트리밍, fsync). 받은 길이가 `expected_len`과 다르거나 실패하면 `dest`를
/// 지운다.
pub async fn download(
    conn: &EsxiConnection,
    datastore: &str,
    path: &str,
    dest: &Path,
    expected_len: u64,
) -> Result<(), EsxiError> {
    let result = download_to(conn, datastore, path, dest, expected_len).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(dest).await;
    }
    result
}

async fn download_to(
    conn: &EsxiConnection,
    datastore: &str,
    path: &str,
    dest: &Path,
    expected_len: u64,
) -> Result<(), EsxiError> {
    let what = display_path(Some(datastore), path);
    let mut body = get(conn, Some(datastore), path).await?.into_body();
    let mut out = tokio::fs::File::create(dest).await?;
    let mut received = 0u64;
    let short = |n: u64| {
        EsxiError::Invalid(format!(
            "downloaded {n} bytes of {what}, expected {expected_len}"
        ))
    };
    while let Some(data) = next_data(&mut body).await? {
        received += data.len() as u64;
        // 더 오면 디스크를 채우기 전에 멈춘다.
        if received > expected_len {
            return Err(short(received));
        }
        out.write_all(&data).await?;
    }
    if received != expected_len {
        return Err(short(received));
    }
    out.flush().await?;
    out.sync_all().await?;
    Ok(())
}

/// 사용자에게 보일 데이터스토어 경로(`[datastore1] web01/web01.vmx`, vSphere 표기)
fn display_path(datastore: Option<&str>, path: &str) -> String {
    match datastore {
        Some(ds) => format!("[{ds}] {path}"),
        None => "the datastore list".into(),
    }
}

/// `/folder` URL(경로와 쿼리)
fn folder_url(datacenter: &str, datastore: Option<&str>, path: &str) -> String {
    let mut url = String::from("/folder");
    if !path.is_empty() {
        url.push('/');
        url.push_str(&percent_encode(path, true));
    }
    url.push_str("?dcPath=");
    url.push_str(&percent_encode(datacenter, false));
    if let Some(ds) = datastore {
        url.push_str("&dsName=");
        url.push_str(&percent_encode(ds, false));
    }
    url
}

/// RFC 3986 unreserved 밖의 바이트를 `%XX`로. `keep_slash`면 `/`는 그대로 둔다.
fn percent_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `Authorization: Basic …` 값
pub fn basic_auth(user: &str, password: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{user}:{password}");
    let mut out = String::from("Basic ");
    for chunk in input.as_bytes().chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// GET 하나. 200이 아니면 오류다.
async fn get(
    conn: &EsxiConnection,
    datastore: Option<&str>,
    path: &str,
) -> Result<Response<Incoming>, EsxiError> {
    // 모양이 틀린 지문은 어떤 인증서와도 맞지 않는다(그대로 두어 오류에 보인다).
    let expected = conn
        .fingerprint
        .as_ref()
        .map(|f| normalize_fingerprint(f).unwrap_or_else(|| f.clone()));
    let tls = connect(&conn.host, conn.port, expected).await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .map_err(connect_error)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::get(folder_url(&conn.datacenter, datastore, path))
        .header(HOST, host_header(&conn.host, conn.port))
        .header(AUTHORIZATION, basic_auth(&conn.user, &conn.password))
        .body(Empty::<Bytes>::new())
        .map_err(|e| EsxiError::Invalid(format!("invalid ESXi request: {e}")))?;
    let response = tokio::time::timeout(IDLE_TIMEOUT, sender.send_request(request))
        .await
        .map_err(|_| connect_error("timed out waiting for a response"))?
        .map_err(connect_error)?;
    match response.status() {
        StatusCode::OK => Ok(response),
        StatusCode::UNAUTHORIZED => Err(EsxiError::Auth),
        StatusCode::NOT_FOUND => Err(EsxiError::NotFound(display_path(datastore, path))),
        status => Err(EsxiError::Http(status.as_u16())),
    }
}

/// 본문의 다음 데이터 조각(끝이면 None)
async fn next_data(body: &mut Incoming) -> Result<Option<Bytes>, EsxiError> {
    loop {
        let frame = tokio::time::timeout(IDLE_TIMEOUT, body.frame())
            .await
            .map_err(|_| connect_error("timed out reading the response"))?;
        match frame {
            None => return Ok(None),
            Some(frame) => {
                if let Ok(data) = frame.map_err(connect_error)?.into_data() {
                    return Ok(Some(data));
                }
            }
        }
    }
}

async fn read_limited(mut body: Incoming, limit: usize, what: &str) -> Result<Vec<u8>, EsxiError> {
    let mut out = Vec::new();
    while let Some(data) = next_data(&mut body).await? {
        if out.len() + data.len() > limit {
            return Err(EsxiError::TooLarge {
                path: what.to_string(),
                limit,
            });
        }
        out.extend_from_slice(&data);
    }
    Ok(out)
}

/// 연결할 호스트: IPv6 주소를 `[::1]`처럼 괄호로 적었으면 괄호를 뗀다.
fn bare_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

/// Host 헤더 값(IPv6 주소는 괄호로 감싼다)
fn host_header(host: &str, port: u16) -> String {
    let host = bare_host(host);
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// TCP와 TLS를 연다. `expected`가 없거나 서버 지문과 다르면 핸드셰이크 안에서 끊는다.
async fn connect(
    host: &str,
    port: u16,
    expected: Option<String>,
) -> Result<TlsStream<TcpStream>, EsxiError> {
    let host = bare_host(host);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedCert {
        expected: expected.clone(),
        seen: Mutex::new(None),
        algorithms: provider.signature_verification_algorithms,
    });
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(connect_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    // IP 주소면 SNI를 보내지 않는다(rustls가 알아서 뺀다).
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| connect_error(format!("invalid host name {host}")))?;
    let connector = TlsConnector::from(Arc::new(config));
    let result = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = TcpStream::connect((host, port)).await?;
        connector.connect(name, tcp).await
    })
    .await;
    match result {
        Err(_) => Err(connect_error("timed out")),
        Ok(Ok(tls)) => Ok(tls),
        Ok(Err(e)) => {
            let seen = verifier
                .seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            Err(match (expected, seen) {
                (None, Some(actual)) => EsxiError::Untrusted(actual),
                (Some(expected), Some(actual)) if expected != actual => {
                    EsxiError::Fingerprint { expected, actual }
                }
                _ => connect_error(e),
            })
        }
    }
}

/// 서버 끝 인증서의 지문을 고정해 비교한다. 본 지문은 `seen`에 남겨 오류에 쓴다.
#[derive(Debug)]
struct PinnedCert {
    expected: Option<String>,
    seen: Mutex<Option<String>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = fingerprint_of(end_entity);
        let trusted = self.expected.as_deref() == Some(actual.as_str());
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(actual);
        if trusted {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the certificate fingerprint is not trusted".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// `/folder` HTML 목록을 읽는다. `<a href="…">`의 href만 본다(링크 글자는 잘리거나 바뀔 수 있다).
/// 데이터스토어 목록은 `dsName`이 있는 `/folder?…` 링크, 디렉터리 목록은 같은 데이터스토어의
/// `/folder/{dir}/{이름}` 링크다. 위로 가는 링크("Parent Directory"·"Parent Datacenter")는 `dir`의
/// 바로 아래가 아니므로 빠진다.
pub fn parse_listing(html: &str, datastore: Option<&str>, dir: &str) -> Vec<Entry> {
    let dir = dir.trim_matches('/');
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = HashSet::new();
    for href in hrefs(html) {
        let href = decode_entities(&href);
        let (path, query) = href.split_once('?').unwrap_or((&href, ""));
        let ds = query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            // 쿼리 값의 `+`는 공백이다.
            key.eq_ignore_ascii_case("dsName")
                .then(|| percent_decode(&value.replace('+', " ")))
        });
        let Some(path) = path.strip_prefix("/folder") else {
            continue;
        };
        let path = percent_decode(path);
        let path = path.trim_matches('/');
        let entry = match datastore {
            None => match ds {
                Some(name) if path.is_empty() && !name.is_empty() => Entry { name, is_dir: true },
                _ => continue,
            },
            Some(want) => {
                if ds.as_deref() != Some(want) {
                    continue;
                }
                let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
                if parent != dir || name.is_empty() {
                    continue;
                }
                Entry {
                    name: name.to_string(),
                    is_dir: !is_file_name(name),
                }
            }
        };
        if seen.insert(entry.name.clone()) {
            entries.push(entry);
        }
    }
    entries
}

fn is_file_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        FILE_EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// `<a … href="…">`의 href 값들(대소문자 무시, 큰·작은따옴표)
fn hrefs(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<a").map(|i| at + i) {
        let end = lower[start..].find('>').map_or(lower.len(), |i| start + i);
        at = end;
        let tag = &lower[start..end];
        // `<abbr>` 같은 다른 태그는 건너뛴다.
        if !tag[2..].starts_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }
        let Some(h) = tag.find("href") else {
            continue;
        };
        let rest = tag[h + 4..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value_start = end - rest.len();
        let quote = rest.chars().next();
        let value = match quote {
            Some(q @ ('"' | '\'')) => {
                let body = &html[value_start + 1..end];
                body.find(q).map(|i| &body[..i])
            }
            _ => {
                let body = &html[value_start..end];
                Some(body.split_ascii_whitespace().next().unwrap_or(""))
            }
        };
        if let Some(value) = value {
            out.push(value.to_string());
        }
    }
    out
}

/// HTML 문자 참조(`&amp;`, `&#39;`, `&#x2F;` …)를 푼다. 모르는 것은 그대로 둔다. `;`는 `&` 뒤
/// [`MAX_ENTITY`] 바이트 안에서만 찾는다(`;` 없는 `&`가 많아도 한 번만 훑는다).
fn decode_entities(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest
            .bytes()
            .skip(1)
            .take(MAX_ENTITY)
            .position(|b| b == b';')
            .map(|i| i + 1);
        let decoded = end.and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let code = name.strip_prefix('#')?;
                    let n = match code.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => code.parse().ok()?,
                    };
                    char::from_u32(n)
                }
            }?;
            Some((c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `%XX`를 푼다(대소문자 무시). 잘못된 것은 그대로 두고, UTF-8이 아니면 손실 변환한다.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_is_base64_of_user_and_password() {
        // RFC 7617의 예
        assert_eq!(
            basic_auth("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(basic_auth("root", "p"), "Basic cm9vdDpw");
        assert_eq!(basic_auth("ro", "x"), "Basic cm86eA==");
    }

    #[test]
    fn folder_urls_encode_paths_and_names() {
        assert_eq!(
            folder_url("ha-datacenter", None, ""),
            "/folder?dcPath=ha-datacenter"
        );
        assert_eq!(
            folder_url("ha-datacenter", Some("datastore1"), ""),
            "/folder?dcPath=ha-datacenter&dsName=datastore1"
        );
        assert_eq!(
            folder_url("DC 1", Some("ssd & co"), "my vm/my vm-flat.vmdk"),
            "/folder/my%20vm/my%20vm-flat.vmdk?dcPath=DC%201&dsName=ssd%20%26%20co"
        );
    }

    #[test]
    fn decodes_entities_and_percent_escapes() {
        assert_eq!(
            decode_entities("a&amp;b&#39;c&#x2F;d&bogus;&"),
            "a&b'c/d&bogus;&"
        );
        // `;`가 `&` 뒤 10바이트를 넘으면 참조가 아니다.
        assert_eq!(decode_entities("&#x00000041;&#x0000041;"), "&#x00000041;A");
        assert_eq!(
            percent_decode("ha%2ddatacenter%20x%zz%E2%82%AC"),
            "ha-datacenter x%zz€"
        );
    }

    #[test]
    fn bracketed_ipv6_hosts_connect_without_brackets() {
        assert_eq!(bare_host("[::1]"), "::1");
        assert_eq!(bare_host("::1"), "::1");
        assert_eq!(bare_host("esxi1.lab"), "esxi1.lab");
        assert_eq!(bare_host("10.0.0.5"), "10.0.0.5");
        // Host 헤더는 IPv6면 괄호로 감싼다(한 번만).
        assert_eq!(host_header("[::1]", 443), "[::1]:443");
        assert_eq!(host_header("fe80::1", 8443), "[fe80::1]:8443");
        assert_eq!(host_header("esxi1.lab", 443), "esxi1.lab:443");
    }

    #[test]
    fn fingerprints_are_upper_case_colon_separated() {
        let f = fingerprint_of(b"");
        assert_eq!(f.len(), 32 * 3 - 1);
        assert!(f.starts_with("E3:B0:C4:42:"), "{f}");
        assert_eq!(normalize_fingerprint(&f).as_deref(), Some(f.as_str()));
    }
}
```

`crates/opl-api/tests/common/esxi.rs`를 만든다:

````rust
//! 시험용 가짜 ESXi. 자체 서명 인증서의 HTTPS로 데이터스토어 브라우저(`/folder`)를 흉내 낸다:
//! 데이터센터의 데이터스토어 목록, 디렉터리 목록(ESXi 7/8 모양 HTML), 파일 내려받기, 기본 인증(틀리면
//! 401). 받은 요청을 모두 기록한다. 다른 크레이트의 시험도
//! `#[path = ".../opl-api/tests/common/esxi.rs"] mod esxi;`로 가져다 쓸 수 있다(axum·tokio·opl-api·
//! opl-peer·opl-types·rcgen·rustls·sha2가 필요하다).
//!
//! ```ignore
//! let files = BTreeMap::from([("datastore1/web01/web01.vmx".to_string(), vmx.into_bytes())]);
//! let esxi = MockEsxi::start(files, "root", "secret").await;
//! let conn = esxi.connection("secret", Some(&esxi.fingerprint));
//! ```
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::Response;
use opl_api::vm_imports::esxi::basic_auth;
use opl_types::vm_import::EsxiConnection;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 가짜 ESXi가 받는 데이터센터(ESXi 단독 호스트의 이름)
pub const DATACENTER: &str = "ha-datacenter";

/// 받은 요청 하나
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    /// 퍼센트 인코딩을 푼 경로(`/folder/web01/web01.vmx`)
    pub path: String,
    /// 쿼리 그대로
    pub query: String,
    pub authorization: Option<String>,
}

pub struct MockEsxi {
    pub addr: SocketAddr,
    /// 서버 인증서의 SHA-256 지문(대문자 콜론 구분)
    pub fingerprint: String,
    user: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    server: JoinHandle<()>,
}

struct Site {
    /// `"{데이터스토어}/{경로}"` → 내용
    files: BTreeMap<String, Vec<u8>>,
    authorization: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockEsxi {
    /// `files`의 키는 `"{데이터스토어}/{경로}"`(예: `"datastore1/web01/web01.vmx"`)다. 데이터스토어와
    /// 디렉터리는 파일 경로에서 나온다.
    pub async fn start(files: BTreeMap<String, Vec<u8>>, user: &str, password: &str) -> MockEsxi {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let der = key.cert.der().clone();
        let fingerprint = Sha256::digest(&der)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let private =
            rustls_pki_types::PrivateKeyDer::Pkcs8(key.signing_key.serialize_der().into());
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der], private)
        .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let site = Arc::new(Site {
            files,
            authorization: basic_auth(user, password),
            requests: requests.clone(),
        });
        let app = axum::Router::new().fallback(move |request: Request| {
            let site = site.clone();
            async move { site.handle(request) }
        });
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        MockEsxi {
            addr,
            fingerprint,
            user: user.to_string(),
            requests,
            server,
        }
    }

    /// 지금까지 받은 요청
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// 이 서버로 가는 연결(사용자는 `start`의 것)
    pub fn connection(&self, password: &str, fingerprint: Option<&str>) -> EsxiConnection {
        EsxiConnection {
            host: self.addr.ip().to_string(),
            port: self.addr.port(),
            user: self.user.clone(),
            password: password.into(),
            datacenter: DATACENTER.into(),
            fingerprint: fingerprint.map(str::to_string),
        }
    }
}

impl Drop for MockEsxi {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Site {
    fn handle(&self, request: Request) -> Response {
        let path = percent_decode(request.uri().path());
        let query = request.uri().query().unwrap_or("").to_string();
        let authorization = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        self.requests.lock().unwrap().push(Recorded {
            method: request.method().to_string(),
            path: path.clone(),
            query: query.clone(),
            authorization: authorization.clone(),
        });
        if authorization.as_deref() != Some(self.authorization.as_str()) {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"VMware HTTP server\"",
                )
                .body(Body::empty())
                .unwrap();
        }
        let params: BTreeMap<String, String> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), percent_decode(v)))
            .collect();
        if params.get("dcPath").map(String::as_str) != Some(DATACENTER) {
            return not_found();
        }
        let Some(rest) = path.strip_prefix("/folder") else {
            return not_found();
        };
        let rest = rest.trim_matches('/');
        let Some(ds) = params.get("dsName") else {
            return if rest.is_empty() {
                html(self.datacenter_page())
            } else {
                not_found()
            };
        };
        let key = if rest.is_empty() {
            ds.clone()
        } else {
            format!("{ds}/{rest}")
        };
        if let Some(bytes) = self.files.get(&key) {
            return Response::builder()
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(bytes.clone()))
                .unwrap();
        }
        let prefix = format!("{key}/");
        let children: BTreeSet<&str> = self
            .files
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(|r| r.split('/').next().unwrap_or(r))
            .collect();
        if children.is_empty() {
            return not_found();
        }
        html(directory_page(ds, rest, &children))
    }

    fn datastores(&self) -> BTreeSet<&str> {
        self.files
            .keys()
            .filter_map(|k| k.split('/').next())
            .collect()
    }

    fn datacenter_page(&self) -> String {
        let rows: String = self
            .datastores()
            .iter()
            .map(|ds| {
                format!(
                    "<tr><td><a href=\"/folder?dcPath={}&amp;dsName={}\">{}</a></td></tr>\n",
                    encode(DATACENTER),
                    encode(ds),
                    escape(ds)
                )
            })
            .collect();
        page(
            "Datastores",
            &format!(
                "<table>\n<tr><th>Datastore Name</th></tr>\n<tr><td><a href=\"/\">Parent Directory</a></td></tr>\n{rows}</table>\n"
            ),
        )
    }
}

/// ESXi 7/8 데이터스토어 브라우저 모양의 디렉터리 목록. 맨 위면 "Parent Datacenter", 그 아래면
/// "Parent Directory" 링크가 먼저 온다.
fn directory_page(ds: &str, dir: &str, children: &BTreeSet<&str>) -> String {
    let query = format!("dcPath={}&amp;dsName={}", encode(DATACENTER), encode(ds));
    let parent = if dir.is_empty() {
        format!(
            "<a href=\"/folder?dcPath={}\">Parent Datacenter</a>",
            encode(DATACENTER)
        )
    } else {
        let up = dir.rsplit_once('/').map_or("", |(p, _)| p);
        let up = if up.is_empty() {
            String::new()
        } else {
            format!("/{}", encode(up))
        };
        format!("<a href=\"/folder{up}?{query}\">Parent Directory</a>")
    };
    let rows: String = children
        .iter()
        .map(|name| {
            let path = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            format!(
                "<tr><td><a href=\"/folder/{}?{query}\">{}</a></td><td align=\"right\">04-Nov-2026 10:00</td><td align=\"right\">  3 KB</td></tr>\n",
                encode(&path),
                escape(name)
            )
        })
        .collect();
    page(
        &format!("Index of {}", escape(&format!("[{ds}] {dir}"))),
        &format!(
            "<table>\n<tr><th>Name</th><th>Last modified</th><th>Size</th></tr>\n<tr><td colspan=\"3\">{parent}</td></tr>\n{rows}</table>\n"
        ),
    )
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\">\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\" lang=\"en\">\n\
         <head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\" /><title>{title}</title></head>\n\
         <body>\n<h1>{title}</h1>\n{body}</body>\n</html>\n"
    )
}

fn html(body: String) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(body))
        .unwrap()
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .unwrap()
}

/// ESXi처럼 영숫자와 `/`·`.`·`_` 밖은 소문자 `%xx`로 쓴다(`ha%2ddatacenter`).
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/._".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02x}")
            }
        })
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}
````

`crates/opl-api/tests/common/mod.rs`에서

```rust

pub mod ustar;
```

을 다음으로 바꾼다:

```rust

pub mod esxi;
pub mod ustar;
```

`crates/opl-api/tests/esxi.rs`를 만든다:

```rust
//! ESXi HTTPS 클라이언트: 지문 고정 TLS, probe, 기본 인증, `/folder` 목록 해석, 작은 파일 읽기,
//! 내려받기. 가짜 ESXi(`common/esxi.rs`)를 상대로 한다.

mod common;

use std::collections::BTreeMap;

use common::esxi::MockEsxi;
use opl_api::vm_imports::esxi::{
    Entry, EsxiError, download, get_small, list, parse_listing, probe,
};
use tempfile::TempDir;

const USER: &str = "root";
const PASSWORD: &str = "VMware1!";
const VMX: &str = "displayName = \"web01\"\nmemSize = \"1024\"\n";

fn flat() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

async fn esxi() -> MockEsxi {
    let files = BTreeMap::from([
        (
            "datastore1/web01/web01.vmx".to_string(),
            VMX.as_bytes().to_vec(),
        ),
        (
            "datastore1/web01/web01.vmdk".to_string(),
            b"# Disk DescriptorFile\n".to_vec(),
        ),
        ("datastore1/web01/web01-flat.vmdk".to_string(), flat()),
        ("datastore1/web01/web01.vmx.lck".to_string(), Vec::new()),
        (
            "datastore1/web01/logs/vmware.log".to_string(),
            b"log".to_vec(),
        ),
        ("datastore1/iso/ubuntu.iso".to_string(), b"iso".to_vec()),
        (
            "datastore 2/my vm & co/my vm & co.vmx".to_string(),
            VMX.as_bytes().to_vec(),
        ),
    ]);
    MockEsxi::start(files, USER, PASSWORD).await
}

fn entry(name: &str, is_dir: bool) -> Entry {
    Entry {
        name: name.into(),
        is_dir,
    }
}

fn sorted(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

#[tokio::test]
async fn probe_returns_the_server_fingerprint_without_sending_a_request() {
    let esxi = esxi().await;
    let fingerprint = probe("127.0.0.1", esxi.addr.port()).await.unwrap();
    assert_eq!(fingerprint, esxi.fingerprint);
    assert!(esxi.requests().is_empty());
}

#[tokio::test]
async fn lists_datastores_and_directories_with_the_pinned_fingerprint() {
    let esxi = esxi().await;
    // 지문은 소문자·콜론 없이 적어도 된다.
    let loose = esxi.fingerprint.replace(':', "").to_lowercase();
    let conn = esxi.connection(PASSWORD, Some(&loose));

    let datastores = list(&conn, None, "").await.unwrap();
    assert_eq!(
        sorted(datastores),
        vec![entry("datastore 2", true), entry("datastore1", true)]
    );
    let top = list(&conn, Some("datastore1"), "").await.unwrap();
    assert_eq!(sorted(top), vec![entry("iso", true), entry("web01", true)]);
    let vm = list(&conn, Some("datastore1"), "web01").await.unwrap();
    assert_eq!(
        sorted(vm),
        vec![
            entry("logs", true),
            entry("web01-flat.vmdk", false),
            entry("web01.vmdk", false),
            entry("web01.vmx", false),
            entry("web01.vmx.lck", false),
        ]
    );
    let odd = list(&conn, Some("datastore 2"), "my vm & co")
        .await
        .unwrap();
    assert_eq!(odd, vec![entry("my vm & co.vmx", false)]);
    // 파일을 디렉터리로 잘못 보고 읽으면(HTML이 아님) 빈 목록이다.
    let file = list(&conn, Some("datastore1"), "iso/ubuntu.iso")
        .await
        .unwrap();
    assert!(file.is_empty());

    let requests = esxi.requests();
    assert!(
        requests.iter().all(|r| r.method == "GET"
            && r.authorization.as_deref() == Some("Basic cm9vdDpWTXdhcmUxIQ==")),
        "{requests:?}"
    );
    let odd = requests.iter().find(|r| r.path.contains("my vm")).unwrap();
    assert_eq!(odd.path, "/folder/my vm & co");
    assert_eq!(odd.query, "dcPath=ha-datacenter&dsName=datastore%202");
}

#[tokio::test]
async fn reads_small_files_and_downloads_large_ones() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let text = get_small(&conn, "datastore1", "web01/web01.vmx", 64 << 10)
        .await
        .unwrap();
    assert_eq!(text, VMX);
    let text = get_small(&conn, "datastore 2", "my vm & co/my vm & co.vmx", 64 << 10)
        .await
        .unwrap();
    assert_eq!(text, VMX);

    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("disk.raw");
    let data = flat();
    download(
        &conn,
        "datastore1",
        "web01/web01-flat.vmdk",
        &dest,
        data.len() as u64,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[tokio::test]
async fn small_reads_are_limited() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let err = get_small(&conn, "datastore1", "web01/web01-flat.vmdk", 64 << 10)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, EsxiError::TooLarge { path, limit: 65536 } if path == "[datastore1] web01/web01-flat.vmdk"),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "[datastore1] web01/web01-flat.vmdk is larger than 65536 bytes"
    );
}

#[tokio::test]
async fn a_download_of_the_wrong_length_leaves_no_file() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("disk.raw");
    let len = flat().len() as u64;

    // 서버가 덜 보냄
    let err = download(
        &conn,
        "datastore1",
        "web01/web01-flat.vmdk",
        &dest,
        len + 512,
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "downloaded {len} bytes of [datastore1] web01/web01-flat.vmdk, expected {}",
            len + 512
        )
    );
    assert!(!dest.exists());

    // 서버가 더 보냄: 기대한 길이를 넘자마자 멈춘다.
    let err = download(&conn, "datastore1", "web01/web01-flat.vmdk", &dest, 1000)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .ends_with("of [datastore1] web01/web01-flat.vmdk, expected 1000"),
        "{err}"
    );
    assert!(!dest.exists());

    // 없는 파일도 남기지 않는다.
    let err = download(&conn, "datastore1", "web01/none-flat.vmdk", &dest, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::NotFound(_)), "{err}");
    assert!(!dest.exists());
}

#[tokio::test]
async fn a_wrong_fingerprint_fails_before_any_credentials_are_sent() {
    let esxi = esxi().await;
    let wrong = ["AB"; 32].join(":");
    let conn = esxi.connection(PASSWORD, Some(&wrong));
    match list(&conn, None, "").await {
        Err(EsxiError::Fingerprint { expected, actual }) => {
            assert_eq!(expected, wrong);
            assert_eq!(actual, esxi.fingerprint);
        }
        other => panic!("{other:?}"),
    }
    let dir = TempDir::new().unwrap();
    let dest = dir.path().join("disk.raw");
    let err = download(&conn, "datastore1", "web01/web01-flat.vmdk", &dest, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::Fingerprint { .. }), "{err}");
    assert!(!dest.exists());
    // 모양이 틀린 지문도 맞지 않는다.
    let conn = esxi.connection(PASSWORD, Some("not-a-fingerprint"));
    let err = get_small(&conn, "datastore1", "web01/web01.vmx", 1024)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::Fingerprint { .. }), "{err}");
    // 서버는 요청을 하나도 받지 않았다(Authorization 헤더도).
    assert!(esxi.requests().is_empty(), "{:?}", esxi.requests());
}

#[tokio::test]
async fn without_a_fingerprint_the_server_fingerprint_is_returned_untrusted() {
    let esxi = esxi().await;
    let conn = esxi.connection(PASSWORD, None);
    match get_small(&conn, "datastore1", "web01/web01.vmx", 1024).await {
        Err(EsxiError::Untrusted(actual)) => assert_eq!(actual, esxi.fingerprint),
        other => panic!("{other:?}"),
    }
    assert!(esxi.requests().is_empty());
}

#[tokio::test]
async fn a_wrong_password_is_an_auth_error_and_missing_files_are_not_found() {
    let esxi = esxi().await;
    let conn = esxi.connection("wrong", Some(&esxi.fingerprint));
    let err = list(&conn, None, "").await.unwrap_err();
    assert!(matches!(err, EsxiError::Auth), "{err}");
    assert_eq!(err.to_string(), "ESXi rejected the user name or password");

    let conn = esxi.connection(PASSWORD, Some(&esxi.fingerprint));
    let err = get_small(&conn, "datastore1", "web01/missing.vmx", 1024)
        .await
        .unwrap_err();
    assert!(matches!(err, EsxiError::NotFound(_)), "{err}");
    assert_eq!(
        err.to_string(),
        "[datastore1] web01/missing.vmx was not found on the ESXi host"
    );
    let err = list(&conn, Some("datastore9"), "").await.unwrap_err();
    assert!(matches!(err, EsxiError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn an_unreachable_host_is_a_connect_error() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let err = probe("127.0.0.1", port).await.unwrap_err();
    assert!(matches!(err, EsxiError::Connect(_)), "{err}");
}

/// ESXi 7의 데이터스토어 브라우저 HTML(디렉터리 `web01`)
const ESXI_DIR: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Strict//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en" lang="en">
<head><meta http-equiv="Content-Type" content="text/html; charset=utf-8" />
<title>Index of [datastore1] web01</title></head>
<body>
<h1>Index of [datastore1] web01</h1>
<table>
<tr><th>Name</th><th>Last modified</th><th>Size</th></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter&amp;dsName=datastore1">Parent Directory</a></td></tr>
<tr><td><a href="/folder/web01/web01%2evmx?dcPath=ha%2ddatacenter&amp;dsName=datastore1">web01.vmx</a></td><td align="right">04-Nov-2026 10:00</td><td align="right">  3 KB</td></tr>
<tr><td><A HREF='/folder/web01/web01-flat.vmdk?dcPath=ha%2ddatacenter&amp;dsName=datastore1'>web01-flat.vmdk</A></td></tr>
<tr><td><a class="file" href="/folder/web01/my%20disk%20%26%20more.vmdk?dcPath=ha%2ddatacenter&amp;dsName=datastore1">my disk &amp; more.vmdk</a></td></tr>
<tr><td><a href="/folder/web01/sub.dir/?dcPath=ha%2ddatacenter&amp;dsName=datastore1">sub.dir</a></td></tr>
<tr><td><a href="/folder/web01/web01.vmx?dcPath=ha%2ddatacenter&amp;dsName=datastore2">other datastore</a></td></tr>
<tr><td><a href="/folder/web01/deeper/x.vmx?dcPath=ha%2ddatacenter&amp;dsName=datastore1">too deep</a></td></tr>
<tr><td><a href="/mob/?moid=ha%2ddatacenter">Managed Object Browser</a></td></tr>
<tr><td><abbr title="x">abbr</abbr><a name="anchor">no href</a></td></tr>
</table>
</body>
</html>
"#;

/// ESXi 데이터센터 화면(데이터스토어 목록)
const ESXI_DATACENTER: &str = r#"<html><body>
<table>
<tr><th>Datastore Name</th></tr>
<tr><td><a href="/">Parent</a></td></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter&amp;dsName=datastore1">datastore1</a></td></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter&amp;dsName=NVMe%20SSD%20%282%29">NVMe SSD (2)</a></td></tr>
<tr><td><a href="/folder?dcPath=ha%2ddatacenter">Parent Datacenter</a></td></tr>
</table>
</body></html>
"#;

#[test]
fn parses_esxi_directory_listings() {
    let entries = parse_listing(ESXI_DIR, Some("datastore1"), "web01");
    assert_eq!(
        entries,
        vec![
            entry("web01.vmx", false),
            entry("web01-flat.vmdk", false),
            entry("my disk & more.vmdk", false),
            // 이름의 `.`만으로는 파일이 아니다(아는 확장자만).
            entry("sub.dir", true),
        ]
    );
    // 끝의 `/`는 같은 디렉터리다.
    assert_eq!(
        parse_listing(ESXI_DIR, Some("datastore1"), "/web01/"),
        entries
    );
    // 다른 디렉터리의 목록으로 읽으면 아무것도 바로 아래가 아니다.
    assert!(parse_listing(ESXI_DIR, Some("datastore1"), "").is_empty());
}

#[test]
fn parses_esxi_datastore_listings() {
    assert_eq!(
        parse_listing(ESXI_DATACENTER, None, ""),
        vec![entry("datastore1", true), entry("NVMe SSD (2)", true)]
    );
}

/// 쿼리 값의 `+`는 공백이다(`%2B`가 `+`).
#[test]
fn query_plus_is_a_space() {
    let html = r#"<a href="/folder?dcPath=ha-datacenter&dsName=SSD+1">SSD 1</a>
<a href="/folder?dcPath=ha-datacenter&dsName=a%2Bb">a+b</a>"#;
    assert_eq!(
        parse_listing(html, None, ""),
        vec![entry("SSD 1", true), entry("a+b", true)]
    );
    let html = r#"<a href="/folder/vm/x.vmx?dcPath=ha-datacenter&dsName=SSD+1">x.vmx</a>"#;
    assert_eq!(
        parse_listing(html, Some("SSD 1"), "vm"),
        vec![entry("x.vmx", false)]
    );
}

/// 목록 크기 제한(1 MiB)만큼의 나쁜 입력도 빨리 끝난다: `;` 없는 `&`가 많은 href, 서로 다른 항목이 많은
/// 목록.
#[test]
fn large_listings_are_parsed_quickly() {
    let started = std::time::Instant::now();
    let html = format!(
        "<a href=\"/folder/vm/{}x.vmx?dsName=ds\">x</a>",
        "&".repeat(1 << 20)
    );
    let entries = parse_listing(&html, Some("ds"), "vm");
    assert_eq!(entries.len(), 1);
    let html: String = (0..30_000)
        .map(|i| format!("<a href=\"/folder/vm/f{i}.vmdk?dsName=ds\">f</a>\n"))
        .collect();
    assert_eq!(parse_listing(&html, Some("ds"), "vm").len(), 30_000);
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1114개, 실패 0

Run: `cargo test -p opl-api --test esxi`
Expected: PASS

Run: `cargo deny check`
Expected: ok

- [ ] **Step 3: 커밋**

```bash
git add Cargo.toml Cargo.lock crates
git commit -m "feat(vm-import): ESXi HTTPS 클라이언트와 가짜 ESXi — vm_imports/esxi.rs(TLS 핸드셰이크 안에서 서버 인증서 SHA-256 지문을 고정해 비교하고 다르면 자격 증명을 보내기 전에 끊음, 지문이 없으면 Untrusted(실제 지문), probe, 요청마다 새 HTTP/1.1 연결과 기본 인증, /folder 데이터스토어·디렉터리 목록 해석 — href만 보고 위로 가는 링크 제외·HTML 엔티티·퍼센트 인코딩·공백 이름, 아는 확장자만 파일, get_small 크기 제한, download 스트리밍·fsync·길이가 다르면 파일 지움), 시험 도우미 tests/common/esxi.rs(자체 서명 인증서, ESXi 7/8 모양 목록 HTML, 401, 요청 기록), hyper·hyper-util·http-body-util을 워크스페이스 의존성으로"
```

---

### Task 3: ESXi 찾기·만들기 API와 메모리 속 자격 증명

`POST /vm-imports/esxi/browse`(본문 연결 → 데이터스토어마다 맨 위 디렉터리의 `.vmx` 요약, 동시 8개, 켜짐 짐작 `locked` — `.lck`·`.lck-…`·`.vswp`·`.vmss`), `POST /vm-imports/esxi`(본문 `{ name, connection, datastore, vmx }` → `.vmx`·디스크립터를 읽어 원본 VM·제안·경고와 `ready` 가져오기, 201). 오류: 지문 없음 409 `untrusted_certificate`(details.fingerprint), 다름 409 `fingerprint_mismatch`, 틀린 형식 400, 인증 400 `esxi_auth`, 연결 502 `esxi_unreachable`, 스냅샷 등 400. 분산 스위치 NIC는 `dvs:{portgroupId}`, NSX는 `nsx:{id}`로 두고 경고. 비밀번호는 받은 노드의 메모리 `ImportCredentials`에만(리소스를 만들기 전에 넣고 실패·끊김이면 지움). 노드 시작 때 이 노드의 `ready` ESXi 가져오기는 `failed`. 이름 `esxi`도 예약.

**Files:**
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/state.rs`
- Modify: `crates/opl-api/src/vm_imports.rs`
- Modify: `crates/opl-api/src/vm_imports/vmx.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/vm_imports.rs`
- Modify: `crates/opl-api/tests/vmx.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(vm_imports::check_ova))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(vm_imports::check_ova))
        .routes(routes!(vm_imports::browse_esxi))
        .routes(routes!(vm_imports::create_esxi))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

`crates/opl-api/src/state.rs`에서

```rust
use crate::limit::AttemptLimiter;
```

을 다음으로 바꾼다:

```rust
use crate::limit::AttemptLimiter;
use crate::vm_imports::ImportCredentials;
```

`crates/opl-api/src/state.rs`에서

```rust
    pub(crate) vm_lock: tokio::sync::Mutex<()>,
}
```

을 다음으로 바꾼다:

```rust
    pub(crate) vm_lock: tokio::sync::Mutex<()>,
    /// 이 노드가 받은 ESXi 가져오기의 자격 증명(메모리에만). 같은 노드의 Task 실행기와 나눠 쓴다.
    pub import_credentials: Arc<ImportCredentials>,
}
```

`crates/opl-api/src/state.rs`에서

```rust
            vm_lock: tokio::sync::Mutex::new(()),
        }
```

을 다음으로 바꾼다:

```rust
            vm_lock: tokio::sync::Mutex::new(()),
            import_credentials: Arc::default(),
        }
```

`crates/opl-api/src/vm_imports.rs` 전체:

```rust
//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아내고 opLite VM 설정을 제안한다.
//! 원본은 올린 노드의 `{upload_dir}/imports/{가져오기 id}/source.ova`에 둔다. 실행과 지우기는 그 노드의
//! Task 실행기가 한다. ESXi·vCenter 데이터스토어의 VM은 `.vmx`·디스크립터를 HTTPS로 읽어([`esxi`]·
//! [`vmx`]) 같은 리소스를 만들고, 비밀번호는 요청을 받은 노드의 메모리([`ImportCredentials`])에만 둔다.

pub mod esxi;
pub mod ova;
pub mod ovf;
pub mod vmx;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt, TryStreamExt};
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
    EsxiConnection, EsxiSource, ImportPhase, ImportProposal, ImportRun, ImportSource, OvaSource,
    SATA_PORTS, SourceDisk, SourceVm, VmImport, VmImportSpec, VmImportStatus,
    normalize_fingerprint, propose,
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
use esxi::EsxiError;
use ova::{OvaError, parse_ova, read_index};
use vmx::{MAX_TEXT_BYTES, VmxError};

/// `/vm-imports/ova`·`/vm-imports/esxi` 경로와 겹치므로 가져오기 이름으로 쓸 수 없다.
const RESERVED_NAMES: [&str; 2] = ["ova", "esxi"];
/// 이 노드 메모리에 자격 증명이 없는(opld가 다시 시작했거나 요청이 끊긴) ESXi 가져오기의 오류
pub(crate) const CREDENTIALS_LOST: &str = "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again";
/// browse가 `.vmx`를 동시에 읽는 개수
const BROWSE_CONCURRENCY: usize = 8;
/// browse가 보는 VM 디렉터리의 최대 개수(넘으면 자른다)
const BROWSE_MAX_DIRS: usize = 500;
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

/// 이 노드에 새 가져오기를 만들 수 있는지 본다(권한, 이름, 위트니스 노드). OVA 올리기와 ESXi 가져오기가
/// 함께 쓴다. 확인 뒤에 다른 요청이 이름을 가져가면 저장소에 만들 때 `name_taken`이 난다.
fn check_new(state: &AppState, principal: &Principal, name: &str) -> Result<(), ApiError> {
    state.authorize(principal, Action::Write, VmImportSpec::KIND)?;
    check_name(name)?;
    if RESERVED_NAMES.contains(&name) {
        return Err(ApiError::bad_request(format!(
            "the name {name} is reserved"
        )));
    }
    // 원본은 받은 노드에 남고 가져오기도 그 노드에서 하므로 VM을 돌리는 노드여야 한다.
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
    Ok(())
}

/// OVA를 받기 전의 확인. 사전 확인과 올리기가 함께 쓴다.
async fn check_upload(
    state: &AppState,
    principal: &Principal,
    name: &str,
    size: u64,
) -> Result<(), ApiError> {
    check_new(state, principal, name)?;
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

/// ESXi 가져오기의 자격 증명(가져오기 id → 연결). 비밀번호를 Raft·Task·로그에 남기지 않으려고 요청을
/// 받은 노드의 메모리에만 둔다. opld가 다시 시작하면 사라진다(그 가져오기는 시작 정리가 `failed`로 바꾼다).
#[derive(Default)]
pub struct ImportCredentials(Mutex<HashMap<Uuid, EsxiConnection>>);

impl ImportCredentials {
    pub fn insert(&self, import: Uuid, conn: EsxiConnection) {
        self.0.lock().unwrap().insert(import, conn);
    }

    pub fn get(&self, import: Uuid) -> Option<EsxiConnection> {
        self.0.lock().unwrap().get(&import).cloned()
    }

    pub fn remove(&self, import: Uuid) {
        self.0.lock().unwrap().remove(&import);
    }

    pub fn is_empty(&self) -> bool {
        self.0.lock().unwrap().is_empty()
    }
}

/// 만들기를 끝내지 못한 가져오기의 자격 증명을 지운다. 정상 경로에서는 `armed`를 내린다.
struct CredentialGuard {
    credentials: Arc<ImportCredentials>,
    import: Uuid,
    armed: bool,
}

impl CredentialGuard {
    fn insert(credentials: Arc<ImportCredentials>, import: Uuid, conn: EsxiConnection) -> Self {
        credentials.insert(import, conn);
        CredentialGuard {
            credentials,
            import,
            armed: true,
        }
    }
}

impl Drop for CredentialGuard {
    fn drop(&mut self) {
        if self.armed {
            self.credentials.remove(self.import);
        }
    }
}

/// ESXi 오류를 API 오류로. 지문이 없으면 409 `untrusted_certificate`, 다르면 409 `fingerprint_mismatch`
/// (둘 다 `details.fingerprint`에 서버 인증서의 지문), 인증 실패 400 `esxi_auth`, 연결할 수 없으면 502
/// `esxi_unreachable`. 없는 파일·그 밖의 HTTP 응답·잘못된 응답은 400(메시지에 이유).
pub(crate) fn esxi_error(e: EsxiError) -> ApiError {
    let with_fingerprint = |mut error: ApiError, fingerprint: &str| {
        error.details = Some(serde_json::json!({ "fingerprint": fingerprint }));
        error
    };
    match e {
        EsxiError::Untrusted(actual) => with_fingerprint(
            ApiError::conflict(
                "untrusted_certificate",
                "verify the ESXi certificate fingerprint",
            ),
            &actual,
        ),
        EsxiError::Fingerprint { ref actual, .. } => with_fingerprint(
            ApiError::conflict("fingerprint_mismatch", e.to_string()),
            actual,
        ),
        EsxiError::Auth => ApiError::new(StatusCode::BAD_REQUEST, "esxi_auth", e.to_string()),
        EsxiError::Connect(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "esxi_unreachable", e.to_string())
        }
        EsxiError::NotFound(_)
        | EsxiError::Http(_)
        | EsxiError::TooLarge { .. }
        | EsxiError::Invalid(_) => ApiError::bad_request(e.to_string()),
        EsxiError::Io(e) => ApiError::internal(e),
    }
}

/// 요청의 지문을 정규화한다. 모양이 틀리면 연결하기 전에 400(어떤 인증서와도 맞지 않으므로).
fn check_fingerprint(conn: &mut EsxiConnection) -> Result<(), ApiError> {
    if let Some(f) = &conn.fingerprint {
        let normalized = normalize_fingerprint(f).ok_or_else(|| {
            ApiError::bad_request(format!(
                "invalid fingerprint {f:?}; expected the 64 hex digits of a SHA-256 fingerprint"
            ))
        })?;
        conn.fingerprint = Some(normalized);
    }
    Ok(())
}

/// 켜진 VM이 디렉터리에 남기는 항목인지: 잠금(`.lck`로 끝남, 디렉터리일 수도), NFS 잠금(`.lck-`로
/// 시작), 스왑 파일(`.vswp`, 켜진 동안만 있다). 짐작일 뿐이다.
fn lock_file(name: &str) -> bool {
    name.ends_with(".lck") || name.starts_with(".lck-") || name.ends_with(".vswp")
}

/// 일시 중단된 VM이 디렉터리에 남기는 메모리 상태 파일(`.vmss`)인지
fn suspend_file(name: &str) -> bool {
    name.ends_with(".vmss")
}

fn vmx_error(e: VmxError) -> ApiError {
    ApiError::bad_request(e.0)
}

/// 연결 자체의 문제(인증·지문·연결)인지. browse는 이것만 요청 전체의 실패로 보고, 디렉터리 하나를 읽지
/// 못한 것은 그 디렉터리만 건너뛴다.
fn connection_failed(e: &EsxiError) -> bool {
    matches!(
        e,
        EsxiError::Auth
            | EsxiError::Connect(_)
            | EsxiError::Untrusted(_)
            | EsxiError::Fingerprint { .. }
            | EsxiError::Io(_)
    )
}

/// browse 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiBrowse {
    pub datastores: Vec<EsxiDatastore>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiDatastore {
    pub name: String,
    /// 맨 위 디렉터리마다 첫 `.vmx`(경로 순서)
    pub vms: Vec<EsxiVm>,
}

/// 데이터스토어의 VM 요약. `.vmx`를 해석할 수 없으면 `error`가 있고 나머지는 빈 값이다.
#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiVm {
    /// 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`)
    pub vmx: String,
    pub name: String,
    pub guest_os: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disks: u32,
    /// 디렉터리에 잠금·스왑 파일(`.lck`, `.lck-…`, `.vswp`)이 있거나(켜져 있을 수 있다) 일시 중단 파일
    /// (`.vmss`)이 있다
    pub locked: bool,
    pub error: Option<String>,
}

/// ESXi·vCenter에 연결해 데이터스토어마다 맨 위 디렉터리의 VM(`.vmx`)을 요약한다. 지문이 없으면 409
/// `untrusted_certificate`(`details.fingerprint`), 다르면 409 `fingerprint_mismatch`, 인증 실패 400
/// `esxi_auth`, 연결할 수 없으면 502 `esxi_unreachable`. VM 디렉터리는 500개까지 본다.
#[utoipa::path(post, path = "/vm-imports/esxi/browse", tag = "vm-imports",
    request_body = EsxiConnection,
    responses((status = 200, body = EsxiBrowse), (status = "4XX", body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
pub async fn browse_esxi(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut conn): Json<EsxiConnection>,
) -> Result<Json<EsxiBrowse>, ApiError> {
    state.authorize(&principal, Action::Write, VmImportSpec::KIND)?;
    check_fingerprint(&mut conn)?;
    let datastores = esxi::list(&conn, None, "").await.map_err(esxi_error)?;
    let mut dirs = Vec::new();
    for ds in &datastores {
        match esxi::list(&conn, Some(&ds.name), "").await {
            Ok(entries) => dirs.extend(
                entries
                    .into_iter()
                    .filter(|e| e.is_dir)
                    .map(|e| (ds.name.clone(), e.name)),
            ),
            Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
            Err(e) => {
                tracing::info!(datastore = %ds.name, error = %e, "cannot list an ESXi datastore")
            }
        }
    }
    dirs.truncate(BROWSE_MAX_DIRS);
    let conn = &conn;
    let vms: Vec<Option<(String, EsxiVm)>> = futures_util::stream::iter(dirs)
        .map(|(ds, dir)| async move {
            Ok::<_, ApiError>(browse_dir(conn, &ds, &dir).await?.map(|vm| (ds, vm)))
        })
        .buffered(BROWSE_CONCURRENCY)
        .try_collect()
        .await?;
    let mut vms: Vec<(String, EsxiVm)> = vms.into_iter().flatten().collect();
    let datastores = datastores
        .iter()
        .map(|ds| EsxiDatastore {
            name: ds.name.clone(),
            vms: vms
                .extract_if(.., |(name, _)| *name == ds.name)
                .map(|(_, vm)| vm)
                .collect(),
        })
        .collect();
    Ok(Json(EsxiBrowse { datastores }))
}

/// 디렉터리 `dir`의 첫 `.vmx`를 요약한다. `.vmx`가 없거나 디렉터리를 읽을 수 없으면 None.
async fn browse_dir(
    conn: &EsxiConnection,
    datastore: &str,
    dir: &str,
) -> Result<Option<EsxiVm>, ApiError> {
    let entries = match esxi::list(conn, Some(datastore), dir).await {
        Ok(entries) => entries,
        Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
        Err(e) => {
            tracing::info!(datastore, dir, error = %e, "cannot list an ESXi directory");
            return Ok(None);
        }
    };
    let Some(file) = entries
        .iter()
        .find(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".vmx"))
    else {
        return Ok(None);
    };
    let path = format!("{dir}/{}", file.name);
    let locked = entries
        .iter()
        .any(|e| lock_file(&e.name) || suspend_file(&e.name));
    let summary = match esxi::get_small(conn, datastore, &path, MAX_TEXT_BYTES).await {
        Ok(text) => vmx::parse_vmx(&text)
            .and_then(|v| vmx::vmx_to_source(&v, &path))
            .map_err(|e| e.0),
        Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
        Err(e) => Err(e.to_string()),
    };
    let vm = match summary {
        Ok((vm, _, disks)) => EsxiVm {
            vmx: path,
            name: vm.name,
            guest_os: vm.guest_os,
            vcpus: vm.vcpus,
            memory_mib: vm.memory_mib,
            disks: disks.len() as u32,
            locked,
            error: None,
        },
        Err(error) => EsxiVm {
            name: file
                .name
                .get(..file.name.len() - ".vmx".len())
                .unwrap_or(&file.name)
                .to_string(),
            vmx: path,
            guest_os: String::new(),
            vcpus: 0,
            memory_mib: 0,
            disks: 0,
            locked,
            error: Some(error),
        },
    };
    Ok(Some(vm))
}

/// ESXi에서 가져오기 만들기 요청
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EsxiImportRequest {
    /// 가져오기 이름
    pub name: String,
    /// 연결(`fingerprint`가 있어야 한다)
    pub connection: EsxiConnection,
    pub datastore: String,
    /// 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`)
    pub vmx: String,
}

/// 데이터스토어 안의 상대 경로이고 `.vmx`로 끝나는지
fn valid_vmx_path(path: &str) -> bool {
    path.ends_with(".vmx")
        && !path.contains('\\')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// ESXi·vCenter 데이터스토어의 VM으로 `ready` 가져오기를 만든다(201). 이 노드가 `.vmx`와 디스크
/// 디스크립터를 읽어 원본 VM과 제안을 만들고, 자격 증명은 이 노드의 메모리에만 둔다(실행 Task도 이
/// 노드에서 돈다). 연결 오류는 browse와 같다(지문이 없으면 409 `untrusted_certificate`). 스냅샷이 있는
/// VM, 다른 데이터스토어의 디스크, 통째로 받을 수 없는 디스크는 400. 잠금·일시 중단 파일이 있으면 경고만
/// 한다.
#[utoipa::path(post, path = "/vm-imports/esxi", tag = "vm-imports",
    request_body = EsxiImportRequest,
    responses((status = 201, body = Resource<VmImportSpec, VmImportStatus>), (status = "4XX", body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_esxi(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<EsxiImportRequest>,
) -> Result<(StatusCode, Json<VmImport>), ApiError> {
    check_new(&state, &principal, &req.name)?;
    let EsxiImportRequest {
        name,
        connection: mut conn,
        datastore,
        vmx: vmx_path,
    } = req;
    check_fingerprint(&mut conn)?;
    if datastore.is_empty() || !valid_vmx_path(&vmx_path) {
        return Err(ApiError::bad_request(format!(
            "invalid .vmx path {vmx_path:?} on datastore {datastore:?}"
        )));
    }
    let text = esxi::get_small(&conn, &datastore, &vmx_path, MAX_TEXT_BYTES)
        .await
        .map_err(esxi_error)?;
    // 지문이 맞아 읽었으므로 지문은 있고 모양도 맞다.
    let fingerprint = conn
        .fingerprint
        .as_deref()
        .and_then(normalize_fingerprint)
        .ok_or_else(|| ApiError::bad_request("invalid certificate fingerprint"))?;
    conn.fingerprint = Some(fingerprint.clone());
    let parsed = vmx::parse_vmx(&text).map_err(vmx_error)?;
    let (mut vm, mut warnings, disks) =
        vmx::vmx_to_source(&parsed, &vmx_path).map_err(vmx_error)?;
    for disk in disks {
        let path = vmx::disk_path(&vmx_path, &datastore, &disk.path).map_err(vmx_error)?;
        let shown = format!("[{datastore}] {path}");
        let text = match esxi::get_small(&conn, &datastore, &path, MAX_TEXT_BYTES).await {
            Ok(text) => text,
            // 크기 제한을 넘었다: 디스크립터가 따로 없는 VMDK(스파스·호스트형)다.
            Err(EsxiError::TooLarge { .. }) => {
                return Err(ApiError::bad_request(format!(
                    "{shown} is not a VMDK descriptor; only VMFS and flat disks can be imported"
                )));
            }
            Err(e) => return Err(esxi_error(e)),
        };
        let (file, capacity_bytes) = vmx::parse_descriptor(&text)
            .and_then(|d| vmx::check_flat(&d))
            .map_err(|e| ApiError::bad_request(format!("{shown}: {e}")))?;
        // extent는 디스크립터와 같은 디렉터리에 있다.
        let file = match path.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{file}"),
            None => file,
        };
        vm.disks.push(SourceDisk {
            name: disk.name,
            file,
            capacity_bytes,
            controller: disk.controller,
        });
    }
    let dir = vmx_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let entries = esxi::list(&conn, Some(&datastore), dir)
        .await
        .map_err(esxi_error)?;
    if entries.iter().any(|e| suspend_file(&e.name)) {
        warnings.insert(
            0,
            "the VM is suspended; power it off in VMware before importing (its memory state is not imported)".into(),
        );
    }
    if entries.iter().any(|e| lock_file(&e.name)) {
        warnings.insert(
            0,
            "the VM looks powered on (lock files found); power it off before importing".into(),
        );
    }
    let (_, networks) = state.cluster.list::<NetworkSpec>()?;
    let networks: Vec<String> = networks.into_iter().map(|n| n.name).collect();
    let (proposal, proposal_warnings) = propose(&vm, &networks);
    warnings.extend(proposal_warnings);
    let spec = VmImportSpec {
        node: state.node,
        source: ImportSource::Esxi(EsxiSource {
            host: conn.host.clone(),
            port: conn.port,
            datacenter: conn.datacenter.clone(),
            datastore,
            vmx: vmx_path,
            user: conn.user.clone(),
            fingerprint,
        }),
    };
    let mut import: VmImport = Resource::new(name, spec);
    import.status = VmImportStatus {
        phase: ImportPhase::Ready,
        source_vm: Some(vm),
        proposal: Some(proposal),
        warnings,
        vm: None,
        error: None,
    };
    // 만들자마자 다른 노드로 온 실행 요청의 Task가 자격 증명을 찾을 수 있게 먼저 넣는다. 만들기가
    // 실패하거나 클라이언트가 끊어 이 future가 버려지면 지운다.
    let mut guard = CredentialGuard::insert(state.import_credentials.clone(), import.id, conn);
    import.revision = state.cluster.create(&import).await?;
    guard.armed = false;
    Ok((StatusCode::CREATED, Json(import)))
}

/// 이전 opld가 남긴 가져오기 파일을 정리한다(노드가 시작할 때, API를 띄우기 전). 리소스가 없는
/// `imports/{id}` 디렉터리를 지우고, 이 노드의 `ready` OVA 가져오기인데 원본이 없으면 `failed`로 바꾼다.
/// 이 노드의 `ready` ESXi 가져오기는 메모리의 자격 증명을 잃었으므로 `failed`로 바꾼다(ESXi 가져오기는
/// 실행할 때만 디렉터리를 쓰므로 디렉터리는 보지 않는다).
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
        let esxi = matches!(import.spec.source, ImportSource::Esxi(_));
        if status.phase == ImportPhase::Ready && esxi {
            tracing::warn!(import = %import.name, "the ESXi credentials of the import were lost");
            status.phase = ImportPhase::Failed;
            status.error = Some(CREDENTIALS_LOST.into());
        } else if status.phase == ImportPhase::Ready
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

    /// 만들기 전에 넣은 자격 증명은 만들기를 끝내지 못하면(guard가 그대로 버려지면) 지워진다.
    #[test]
    fn credential_guard_removes_unfinished_credentials() {
        let credentials = Arc::new(ImportCredentials::default());
        let conn = EsxiConnection {
            host: "esxi.example".into(),
            port: 443,
            user: "root".into(),
            password: "pw".into(),
            datacenter: "ha-datacenter".into(),
            fingerprint: None,
        };
        let (failed, created) = (Uuid::now_v7(), Uuid::now_v7());
        let guard = CredentialGuard::insert(credentials.clone(), failed, conn.clone());
        assert!(credentials.get(failed).is_some());
        drop(guard);
        assert!(credentials.get(failed).is_none());
        let mut guard = CredentialGuard::insert(credentials.clone(), created, conn);
        guard.armed = false;
        drop(guard);
        assert!(credentials.get(created).is_some());
    }
}
```

`crates/opl-api/src/vm_imports/vmx.rs`에서

```rust
        let get = |key: &str| vmx.get(&format!("ethernet{n}.{key}"));
        let network =
            get("networkName").ok_or_else(|| invalid(format!("ethernet{n} has no networkName")))?;
        let mac = match get("addressType") {
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-api/src/vm_imports/vmx.rs`에서

```rust
        nics.push(SourceNic {
            network: network.to_string(),
            adapter: get("virtualDev").unwrap_or("e1000").to_ascii_lowercase(),
```

을 다음으로 바꾼다:

```rust
        nics.push(SourceNic {
            network,
            adapter: get("virtualDev").unwrap_or("e1000").to_ascii_lowercase(),
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/vm-imports/ova/check",
        "/watch",
```

을 다음으로 바꾼다:

```rust
        "/vm-imports/ova/check",
        "/vm-imports/esxi",
        "/vm-imports/esxi/browse",
        "/watch",
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust
//! VM 가져오기 API: OVA 올리기 사전 확인, 올리기(해석과 제안), 목록·읽기, 실행 요청의 검사, 지우기와
//! 강제 지우기, 노드 시작 때의 정리.

mod common;

use std::path::PathBuf;

use common::ustar::Tar;
```

을 다음으로 바꾼다:

```rust
//! VM 가져오기 API: OVA 올리기 사전 확인, 올리기(해석과 제안), 목록·읽기, 실행 요청의 검사, 지우기와
//! 강제 지우기, 노드 시작 때의 정리. ESXi 연결(browse)과 ESXi에서 가져오기 만들기.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::esxi::MockEsxi;
use common::ustar::Tar;
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust
use opl_types::vm_import::{
    ImportPhase, ImportSource, OvaSource, VmImport, VmImportSpec, VmImportStatus,
};
```

을 다음으로 바꾼다:

```rust
use opl_types::vm_import::{
    EsxiSource, ImportPhase, ImportSource, OvaSource, VmImport, VmImportSpec, VmImportStatus,
};
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust
                .get::<opl_types::vm::VmSpec>(vm)
                .unwrap()
                .is_some()
        );
    }
}
```

을 다음으로 바꾼다:

```rust
                .get::<opl_types::vm::VmSpec>(vm)
                .unwrap()
                .is_some()
        );
    }
}

// --- ESXi에서 바로 가져오기 ---

const ESXI_USER: &str = "root";
const ESXI_PASSWORD: &str = "S3cret-esxi-pw";
/// 형식은 맞지만 가짜 ESXi의 것이 아닌 지문
const OTHER_FINGERPRINT: &str = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF";

const WEB01_VMX: &str = r#".encoding = "UTF-8"
displayName = "web01"
guestOS = "ubuntu-64"
numvcpus = "2"
cpuid.coresPerSocket = "2"
memSize = "1024"
firmware = "efi"
scsi0.present = "TRUE"
scsi0.virtualDev = "pvscsi"
scsi0:0.present = "TRUE"
scsi0:0.fileName = "web01.vmdk"
sata0.present = "TRUE"
sata0:0.present = "TRUE"
sata0:0.deviceType = "cdrom-image"
sata0:0.fileName = "/vmfs/volumes/datastore1/iso/ubuntu.iso"
ethernet0.present = "TRUE"
ethernet0.networkName = "VM Network"
ethernet0.virtualDev = "vmxnet3"
ethernet0.addressType = "static"
ethernet0.address = "00:50:56:aa:bb:cc"
"#;

fn descriptor(create_type: &str, extent: &str, extra: &str) -> Vec<u8> {
    format!(
        "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n{extra}\
         createType=\"{create_type}\"\n\n# Extent description\n{extent}\n\n\
         # The Disk Data Base\nddb.adapterType = \"lsilogic\"\n"
    )
    .into_bytes()
}

/// 가짜 ESXi의 파일: 데이터스토어 둘, 켜진 VM(`web01`, 잠금 파일), 스냅샷이 있는 VM, `.vmx`를 읽을 수
/// 없는 VM, 다른 데이터스토어에 디스크가 있는 VM, VM이 아닌 디렉터리.
fn esxi_files() -> BTreeMap<String, Vec<u8>> {
    let minimal = |name: &str, disk: &str| {
        format!(
            "displayName = \"{name}\"\nguestOS = \"windows2019srv-64\"\nmemSize = \"2048\"\n\
             numvcpus = \"4\"\nscsi0:0.present = \"TRUE\"\nscsi0:0.fileName = \"{disk}\"\n"
        )
        .into_bytes()
    };
    BTreeMap::from([
        ("datastore1/web01/web01.vmx".into(), WEB01_VMX.into()),
        (
            "datastore1/web01/web01.vmdk".into(),
            descriptor("vmfs", "RW 8192 VMFS \"web01-flat.vmdk\"", ""),
        ),
        ("datastore1/web01/web01-flat.vmdk".into(), vec![0; 4096]),
        ("datastore1/web01/web01.vmx.lck".into(), vec![]),
        ("datastore1/web01/vmware.log".into(), b"log".to_vec()),
        (
            "datastore1/snap/snap.vmx".into(),
            minimal("snap", "snap-000001.vmdk"),
        ),
        (
            "datastore1/snap/snap-000001.vmdk".into(),
            descriptor(
                "vmfsSparse",
                "RW 8192 VMFSSPARSE \"snap-000001-delta.vmdk\"",
                "parentFileNameHint=\"snap.vmdk\"\n",
            ),
        ),
        (
            "datastore1/broken/broken.vmx".into(),
            b"numvcpus = \"2\"\n".to_vec(),
        ),
        (
            "datastore1/far/far.vmx".into(),
            minimal("far", "/vmfs/volumes/datastore2/far/far.vmdk"),
        ),
        ("datastore1/iso/ubuntu.iso".into(), b"iso".to_vec()),
        (
            "datastore2/db01/db01.vmx".into(),
            minimal("db01", "db01.vmdk"),
        ),
        (
            "datastore2/db01/db01.vmdk".into(),
            descriptor("monolithicFlat", "RW 2048 FLAT \"db01-flat.vmdk\" 0", ""),
        ),
    ])
}

async fn mock_esxi() -> MockEsxi {
    MockEsxi::start(esxi_files(), ESXI_USER, ESXI_PASSWORD).await
}

/// 요청 본문의 연결(`EsxiConnection`은 직렬화하지 않으므로 JSON으로 만든다)
fn connection(esxi: &MockEsxi, password: &str, fingerprint: Option<&str>) -> Value {
    let mut conn = json!({
        "host": esxi.addr.ip().to_string(),
        "port": esxi.addr.port(),
        "user": ESXI_USER,
        "password": password,
    });
    if let Some(f) = fingerprint {
        conn["fingerprint"] = json!(f);
    }
    conn
}

fn esxi_import(name: &str, conn: Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": conn, "datastore": datastore, "vmx": vmx })
}

/// 지문이 없으면 409 `untrusted_certificate`와 서버 지문, 다르면 409 `fingerprint_mismatch`와 실제 지문.
/// 둘 다 자격 증명을 보내기 전에 끊는다. 비밀번호가 틀리면 400 `esxi_auth`, 연결할 수 없으면 502
/// `esxi_unreachable`.
#[tokio::test]
async fn browse_needs_a_trusted_fingerprint() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, None),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "untrusted_certificate");
    assert_eq!(body["message"], "verify the ESXi certificate fingerprint");
    assert_eq!(body["details"], json!({ "fingerprint": esxi.fingerprint }));

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(OTHER_FINGERPRINT)),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "fingerprint_mismatch");
    assert_eq!(body["details"], json!({ "fingerprint": esxi.fingerprint }));
    assert!(esxi.requests().is_empty(), "{:?}", esxi.requests());

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, "wrong", Some(&esxi.fingerprint)),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "esxi_auth");

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    conn["port"] = json!(port);
    let (status, body) = admin.post("/vm-imports/esxi/browse", conn).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], "esxi_unreachable");

    // 알 수 없는 필드는 받지 않는다.
    let mut conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    conn["extra"] = json!(1);
    let (status, body) = admin.post("/vm-imports/esxi/browse", conn).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 모양이 틀린 지문은 연결하기 전에 400
    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some("AB:CD")),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid fingerprint \"AB:CD\""),
        "{body}"
    );
}

/// 데이터스토어마다 맨 위 디렉터리의 `.vmx`를 읽어 요약한다. 잠금 파일이 있으면 `locked`, 읽을 수 없는
/// `.vmx`는 그 VM의 `error`, `.vmx`가 없는 디렉터리는 뺀다. 지문은 대소문자·콜론 없이 적어도 된다.
#[tokio::test]
async fn browse_lists_vms_on_every_datastore() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;
    let loose = esxi.fingerprint.replace(':', "").to_lowercase();

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(&loose)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let vm = |vmx: &str, name: &str, guest: &str, cpus: u32, mem: u64, locked: bool| {
        json!({
            "vmx": vmx, "name": name, "guest_os": guest, "vcpus": cpus, "memory_mib": mem,
            "disks": 1, "locked": locked, "error": null,
        })
    };
    assert_eq!(
        body,
        json!({ "datastores": [
            { "name": "datastore1", "vms": [
                {
                    "vmx": "broken/broken.vmx", "name": "broken", "guest_os": "", "vcpus": 0,
                    "memory_mib": 0, "disks": 0, "locked": false,
                    "error": "the .vmx has no memSize",
                },
                vm("far/far.vmx", "far", "windows2019srv-64", 4, 2048, false),
                vm("snap/snap.vmx", "snap", "windows2019srv-64", 4, 2048, false),
                vm("web01/web01.vmx", "web01", "ubuntu-64", 2, 1024, true),
            ]},
            { "name": "datastore2", "vms": [
                vm("db01/db01.vmx", "db01", "windows2019srv-64", 4, 2048, false),
            ]},
        ]})
    );

    // 읽기 사용자는 ESXi에 연결할 수 없다.
    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = viewer
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint)),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// `.vmx`와 디스크립터를 읽어 원본 VM·제안·경고를 담은 `ready` 가져오기를 만든다. 원본에는 비밀번호가
/// 없고(저장소 어디에도 없다), 자격 증명은 이 노드의 메모리에만 둔다.
#[tokio::test]
async fn creates_an_import_from_esxi() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;

    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("web01", conn, "datastore1", "web01/web01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["node"], env.node.to_string());
    assert_eq!(
        body["spec"]["source"],
        json!({ "esxi": {
            "host": "127.0.0.1", "port": esxi.addr.port(), "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": ESXI_USER,
            "fingerprint": esxi.fingerprint,
        }})
    );
    let st = &body["status"];
    assert_eq!(st["phase"], "ready");
    assert_eq!(
        st["source_vm"],
        json!({
            "name": "web01", "guest_os": "ubuntu-64", "firmware": "uefi", "vcpus": 2,
            "cores_per_socket": 2, "memory_mib": 1024,
            "disks": [{
                "name": "disk0", "file": "web01/web01-flat.vmdk",
                "capacity_bytes": 8192 * 512, "controller": "scsi",
            }],
            "nics": [{ "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" }],
        })
    );
    assert_eq!(
        st["proposal"],
        json!({
            "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
            "disk_bus": "virtio-blk", "nic_model": "virtio",
            "networks": { "VM Network": null },
        })
    );
    let warnings: Vec<&str> = st["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(
        warnings[..2],
        [
            "the VM looks powered on (lock files found); power it off before importing",
            "CD-ROM drive is not imported",
        ]
    );

    // 비밀번호는 저장소의 어떤 리소스에도 없다.
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (_, imports) = env.cluster.list::<VmImportSpec>().unwrap();
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let stored = serde_json::to_string(&(imports, tasks)).unwrap();
    assert!(!stored.contains(ESXI_PASSWORD), "{stored}");
    let (_, read) = admin.get("/vm-imports/web01").await;
    assert!(!read.to_string().contains(ESXI_PASSWORD), "{read}");
    // 자격 증명은 이 노드의 메모리에(정규화한 지문과 함께)
    let stored = env.state.import_credentials.get(id).unwrap();
    assert_eq!(stored.password, ESXI_PASSWORD);
    assert_eq!(
        stored.fingerprint.as_deref(),
        Some(esxi.fingerprint.as_str())
    );
    // ESXi 가져오기에는 원본 디렉터리가 없다(실행할 때 만든다).
    assert!(import_dirs(&env).is_empty());

    // 같은 이름은 409
    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("web01", conn, "datastore2", "db01/db01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");

    // vCenter 모양(monolithicFlat)도 된다. 잠금 파일이 없으면 경고도 없다.
    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("db01", conn, "datastore2", "db01/db01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["status"]["source_vm"]["disks"][0]["file"],
        "db01/db01-flat.vmdk"
    );
    assert_eq!(
        body["status"]["source_vm"]["disks"][0]["capacity_bytes"],
        2048 * 512
    );
    assert!(
        !body["status"]["warnings"]
            .to_string()
            .contains("lock files found")
    );
}

/// 켜진 VM은 잠금 파일로 짐작한다: `.lck`로 끝나는 항목, 스왑 파일(`.vswp`, 켜진 VM만 가진다), NFS 잠금
/// (`.lck-`로 시작). 일시 중단된 VM은 `.vmss`를 가진다. browse는 `locked`, 만들기는 경고. 디스크립터가 따로
/// 없는 VMDK(크기 제한을 넘는 바이너리)는 400 "not a VMDK descriptor".
#[tokio::test]
async fn detects_powered_on_vms_and_disks_without_descriptors() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = |name: &str| {
        let vmx = WEB01_VMX.replace("web01", name).into_bytes();
        let descriptor = descriptor("vmfs", &format!("RW 8 VMFS \"{name}-flat.vmdk\""), "");
        [
            (format!("datastore1/{name}/{name}.vmx"), vmx),
            (format!("datastore1/{name}/{name}.vmdk"), descriptor),
            (format!("datastore1/{name}/{name}-flat.vmdk"), vec![0; 4096]),
        ]
    };
    let mut files = BTreeMap::new();
    for name in ["off", "swap", "nfs", "lockdir", "suspended", "sparse"] {
        files.extend(vm(name));
    }
    files.insert("datastore1/swap/swap-3f1a2b.vswp".into(), vec![]);
    files.insert("datastore1/nfs/.lck-0011223344556677".into(), vec![]);
    files.insert("datastore1/lockdir/lockdir.vmx.lck/lock".into(), vec![]);
    files.insert("datastore1/suspended/suspended-5d3c.vmss".into(), vec![]);
    // 디스크립터 없이 데이터가 들어 있는 VMDK(호스트형 스파스)
    files.insert(
        "datastore1/sparse/sparse.vmdk".into(),
        vec![b'K'; 100 << 10],
    );
    let esxi = MockEsxi::start(files, ESXI_USER, ESXI_PASSWORD).await;

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let locked: Vec<(&str, bool)> = body["datastores"][0]["vms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|vm| {
            (
                vm["name"].as_str().unwrap(),
                vm["locked"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        locked,
        [
            ("lockdir", true),
            ("nfs", true),
            ("off", false),
            ("sparse", false),
            ("suspended", true),
            ("swap", true),
        ]
    );

    let powered_on = "the VM looks powered on (lock files found); power it off before importing";
    let suspended = "the VM is suspended; power it off in VMware before importing (its memory state is not imported)";
    for (name, warning) in [
        ("off", None),
        ("swap", Some(powered_on)),
        ("nfs", Some(powered_on)),
        ("lockdir", Some(powered_on)),
        ("suspended", Some(suspended)),
    ] {
        let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
        let (status, body) = admin
            .post(
                "/vm-imports/esxi",
                esxi_import(name, conn, "datastore1", &format!("{name}/{name}.vmx")),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{name}: {body}");
        let warnings = body["status"]["warnings"].as_array().unwrap();
        for w in [powered_on, suspended] {
            assert_eq!(
                warnings.iter().any(|x| x == w),
                warning == Some(w),
                "{name}: {body}"
            );
        }
        if let Some(w) = warning {
            assert_eq!(warnings[0], w, "{name}: {body}");
        }
    }

    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("sparse", conn, "datastore1", "sparse/sparse.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "[datastore1] sparse/sparse.vmdk is not a VMDK descriptor; only VMFS and flat disks can be imported"
    );
}

/// 가져올 수 없는 VM과 잘못된 요청은 가져오기를 만들지 않고, 자격 증명도 남기지 않는다.
#[tokio::test]
async fn esxi_import_rejects_what_it_cannot_import() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;
    let trusted = || connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));

    // 지문이 없으면 browse와 같은 409
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import(
                "web01",
                connection(&esxi, ESXI_PASSWORD, None),
                "datastore1",
                "web01/web01.vmx",
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "untrusted_certificate");
    assert_eq!(body["details"]["fingerprint"], esxi.fingerprint);

    // 모양이 틀린 지문은 연결하기 전에 400
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import(
                "web01",
                connection(&esxi, ESXI_PASSWORD, Some("not-a-fingerprint")),
                "datastore1",
                "web01/web01.vmx",
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid fingerprint"),
        "{body}"
    );

    for (name, vmx, code, want) in [
        (
            "snap",
            "snap/snap.vmx",
            "invalid_request",
            "the VM has snapshots; consolidate or delete them in VMware first",
        ),
        (
            "far",
            "far/far.vmx",
            "invalid_request",
            "is on another datastore",
        ),
        (
            "broken",
            "broken/broken.vmx",
            "invalid_request",
            "the .vmx has no memSize",
        ),
        (
            "gone",
            "gone/gone.vmx",
            "invalid_request",
            "[datastore1] gone/gone.vmx was not found",
        ),
        ("up", "../x.vmx", "invalid_request", "invalid .vmx path"),
        (
            "notvmx",
            "web01/web01.vmdk",
            "invalid_request",
            "invalid .vmx path",
        ),
        ("esxi", "web01/web01.vmx", "invalid_request", "reserved"),
        ("Bad Name", "web01/web01.vmx", "invalid_request", "name"),
    ] {
        let (status, body) = admin
            .post(
                "/vm-imports/esxi",
                esxi_import(name, trusted(), "datastore1", vmx),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{vmx}: {body}");
        assert_eq!(body["code"], code, "{vmx}: {body}");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains(want), "{vmx}: {message}");
    }
    let (_, imports) = env.cluster.list::<VmImportSpec>().unwrap();
    assert!(imports.is_empty());
    assert!(env.state.import_credentials.is_empty());

    // 읽기 사용자는 만들 수 없다.
    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = viewer
        .post(
            "/vm-imports/esxi",
            esxi_import("web01", trusted(), "datastore1", "web01/web01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// 저장소에 ESXi 가져오기를 직접 만든다.
async fn esxi_import_on(env: &Env, name: &str, node: Uuid, phase: ImportPhase) -> VmImport {
    let mut import: VmImport = Resource::new(
        name,
        VmImportSpec {
            node,
            source: ImportSource::Esxi(EsxiSource {
                host: "esxi.example".into(),
                port: 443,
                datacenter: "ha-datacenter".into(),
                datastore: "datastore1".into(),
                vmx: format!("{name}/{name}.vmx"),
                user: "root".into(),
                fingerprint: OTHER_FINGERPRINT.into(),
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

/// 노드가 시작할 때 이 노드의 `ready`·(Task 없는) `importing` ESXi 가져오기는 자격 증명을 잃었으므로
/// `failed`가 된다. 원본 디렉터리가 없어도 "source files are gone"이 아니다. 다른 노드의 것과 끝난 것은
/// 그대로 둔다.
#[tokio::test]
async fn startup_fails_esxi_imports_whose_credentials_are_lost() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let ready = esxi_import_on(&env, "ready", env.node, ImportPhase::Ready).await;
    let cut = esxi_import_on(&env, "cut", env.node, ImportPhase::Importing).await;
    let done = esxi_import_on(&env, "done", env.node, ImportPhase::Done).await;
    let other = esxi_import_on(&env, "other", Uuid::now_v7(), ImportPhase::Ready).await;

    env.start_runner().await;

    let phase = |id: Uuid| {
        let import = env.cluster.get::<VmImportSpec>(id).unwrap().unwrap();
        (import.status.phase, import.status.error)
    };
    let lost = Some("the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again".into());
    assert_eq!(phase(ready.id), (ImportPhase::Failed, lost.clone()));
    assert_eq!(phase(cut.id), (ImportPhase::Failed, lost));
    assert_eq!(phase(done.id), (ImportPhase::Done, None));
    assert_eq!(phase(other.id), (ImportPhase::Ready, None));
}
```

`crates/opl-api/tests/vmx.rs`에서

```rust
        "the .vmx file is larger than 64 KiB"
    );
```

을 다음으로 바꾼다:

```rust
        "the .vmx file is larger than 64 KiB"
    );
}

/// vCenter 분산 스위치의 NIC는 `networkName`이 없다. 포트 그룹 이름(없으면 `dvs:{id}`)을 쓰고 경고한다.
#[test]
fn nics_on_a_distributed_switch_use_the_port_group() {
    let text = "memsize = 1\n\
                ethernet0.present = \"TRUE\"\n\
                ethernet0.dvs.portgroupId = \"dvportgroup-1001\"\n\
                ethernet0.dvs.switchId = \"50 1a 2b\"\n\
                ethernet1.present = \"TRUE\"\n\
                ethernet1.dvs.portgroupId = \"dvportgroup-1002\"\n\
                ethernet1.dvs.portgroupName = \"Prod VLAN 10\"\n\
                ethernet2.present = \"TRUE\"\n\
                ethernet2.networkName = \"VM Network\"\n\
                ethernet2.dvs.portgroupId = \"dvportgroup-1003\"\n";
    let (vm, warnings, _) = source(text).unwrap();
    let networks: Vec<&str> = vm.nics.iter().map(|n| n.network.as_str()).collect();
    assert_eq!(
        networks,
        ["dvs:dvportgroup-1001", "Prod VLAN 10", "VM Network"]
    );
    assert_eq!(
        warnings,
        [
            "ethernet0 is on a distributed switch; map its network when running the import",
            "ethernet1 is on a distributed switch; map its network when running the import",
        ]
    );
}

/// NSX 네트워크의 NIC는 `networkName`도 분산 스위치도 없고 `opaqueNetwork.id`만 있다. `nsx:{id}`를 쓰고
/// 경고한다.
#[test]
fn nics_on_an_nsx_network_use_the_opaque_network_id() {
    let text = "memsize = 1\n\
                ethernet0.present = \"TRUE\"\n\
                ethernet0.opaqueNetwork.id = \"c3a1e2f4-0b1d-4e5f-8a9b-001122334455\"\n\
                ethernet0.opaqueNetwork.type = \"nsx.LogicalSwitch\"\n\
                ethernet1.present = \"TRUE\"\n\
                ethernet1.networkName = \"VM Network\"\n\
                ethernet1.opaqueNetwork.id = \"ignored\"\n";
    let (vm, warnings, _) = source(text).unwrap();
    let networks: Vec<&str> = vm.nics.iter().map(|n| n.network.as_str()).collect();
    assert_eq!(
        networks,
        ["nsx:c3a1e2f4-0b1d-4e5f-8a9b-001122334455", "VM Network"]
    );
    assert_eq!(
        warnings,
        ["ethernet0 is on an NSX network; map its network when running the import"]
    );
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1123개, 실패 0

Run: `cargo test -p opl-api --test vm_imports --test vmx --test openapi`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 407개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(vm-import): ESXi browse·만들기 API와 메모리 속 자격 증명 — POST /vm-imports/esxi/browse(데이터스토어마다 맨 위 디렉터리의 첫 .vmx를 동시 8개로 읽어 요약, 잠금 파일이면 locked, 해석 실패는 그 VM의 error, 디렉터리 500개까지), POST /vm-imports/esxi(.vmx·디스크립터를 읽어 원본 VM·제안·경고 — 잠금 파일 경고, 스냅샷·다른 데이터스토어·디스크립터 없는 VMDK는 400, 원본에는 비밀번호 없음), 오류 코드 untrusted_certificate·fingerprint_mismatch(409, details.fingerprint)·esxi_auth(400)·esxi_unreachable(502), ImportCredentials(AppState, 노드 메모리), 노드 시작 정리가 이 노드의 ready ESXi 가져오기를 failed(자격 증명 사라짐)로, 예약 이름 esxi, 분산 스위치 NIC는 포트 그룹 이름(없으면 dvs:{id})과 경고, schema.d.ts"
```

---

### Task 4: 가져오기 Task의 내려받기 단계

원본이 ESXi면 Task가 그 노드 메모리의 자격 증명으로 디스크마다 여유(용량 + 1 GiB)를 보고 extent를 `imports/{id}/disk.raw`로 내려받아 raw로 볼륨에 옮긴다. 나머지(VM 만들기, `done`, 실패 정리)는 계획 29와 같다. 실행 API는 ESXi면 가장 큰 디스크 용량으로 여유를 보고, 원본 노드에 자격 증명이 없으면 Task 없이 409 `import_failed`와 `failed`. 자격 증명은 `done`·지우기 뒤 지우고, 실패해 `ready`로 돌아가면 남긴다. opld가 같은 자격 증명 저장소를 API와 Task 실행기에 넘긴다.

**Files:**
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/import.rs`
- Modify: `crates/opl-api/src/vm_imports.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/vm_import_tasks.rs`
- Modify: `crates/opl-api/tests/vm_imports.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs`에서

```rust
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::vms::is_stopped;
```

을 다음으로 바꾼다:

```rust
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::vm_imports::ImportCredentials;
use crate::vms::is_stopped;
```

`crates/opl-api/src/tasks.rs`에서

```rust
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
}
```

을 다음으로 바꾼다:

```rust
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
    /// 이 노드가 받은 ESXi 가져오기의 자격 증명. 가져오기 Task가 쓰고, 끝나거나 지우면 지운다. API
    /// (`AppState::import_credentials`)와 같은 것을 넘긴다.
    pub import_credentials: Arc<ImportCredentials>,
}
```

`crates/opl-api/src/tasks.rs`에서

```rust
            firmware: None,
        }
```

을 다음으로 바꾼다:

```rust
            firmware: None,
            import_credentials: Arc::default(),
        }
```

`crates/opl-api/src/tasks/import.rs` 전체:

```rust
//! VM 가져오기 Task(설계 §2.4, ESXi 원본은 §3.4)와 가져오기 지우기 Task. 둘 다 가져오기의 줄에 서서
//! 겹치지 않고, 원본이 있는 노드(ESXi 원본은 자격 증명을 가진 노드)에서 돈다. API가 확인한 뒤 줄에서
//! 기다리는 사이 이름이나 풀이 바뀌었을 수 있으므로 다시 확인한다.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::resource::Resource;
use opl_types::storage::{SourceFormat, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{MacAddr, Vm, VmSpec};
use opl_types::vm_import::{
    EsxiConnection, ImportPhase, ImportRun, ImportSource, SourceDisk, VmImport, VmImportSpec,
    VmImportStatus,
};
use uuid::Uuid;

use super::backup::ready_pool;
use super::clone::{create_vm, made, volume_name};
use super::{Ctx, mark_ready};
use crate::vm_imports::esxi;
use crate::vm_imports::ova::{Ova, extract, parse_ova};
use crate::vm_imports::{
    CREDENTIALS_LOST, DOWNLOADED_DISK, EXTRACTED_DISK, SOURCE_OVA, import_dir, import_spec,
};
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

/// 디스크를 얻는 곳
enum Origin {
    /// 올린 OVA(`source`)에서 추출한다.
    Ova { source: PathBuf, ova: Ova },
    /// ESXi 데이터스토어에서 extent를 받는다.
    Esxi {
        conn: EsxiConnection,
        datastore: String,
    },
}

/// `importing` 가져오기 `id`를 `run`의 값으로 대상 id의 새 VM으로 만든다(설계 §2.4): 원본 디스크마다
/// OVA에서 추출하거나(`.mf`가 있으면 해시 확인) ESXi에서 extent를 받아(§3.4, 이 노드 메모리의 자격 증명)
/// 풀 `run.pool`에 새 볼륨 `{name}-disk{i}`(풀의 기본 형식)를 만들어 옮긴 뒤, 꺼진 VM을 만든다. 끝나면
/// 가져오기를 `done`으로 바꾸고 원본 디렉터리와 자격 증명을 지운다.
/// 원본 MAC을 쓰는데 다른 VM이 그 MAC을 쓰면 디스크를 옮기기 전에 실패한다. 디스크마다 업로드
/// 디렉터리에 추출하거나 받을 여유를 본다. ESXi 자격 증명이 없으면(opld가 다시 시작했다) 바로 실패한다.
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
    let esxi = match &import.spec.source {
        ImportSource::Esxi(source) => {
            let conn = ctx
                .config
                .import_credentials
                .get(id)
                .ok_or(CREDENTIALS_LOST)?;
            Some((conn, source.datastore.clone()))
        }
        ImportSource::Ova(_) => None,
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
    let origin = match esxi {
        Some((conn, datastore)) => {
            // ESXi 가져오기의 디렉터리는 실행할 때 만든다.
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            Origin::Esxi { conn, datastore }
        }
        None => {
            let source = dir.join(SOURCE_OVA);
            let path = source.clone();
            let ova = tokio::task::spawn_blocking(move || parse_ova(&path))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("cannot read the OVA: {e}"))?;
            Origin::Ova { source, ova }
        }
    };

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let mut copies: Vec<Volume> = Vec::with_capacity(src.disks.len());
    for disk in &src.disks {
        let failed = |e: &dyn std::fmt::Display| format!("cannot import disk {:?}: {e}", disk.name);
        let (file, from) = fetch(ctx, &origin, &dir, disk)
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
            .import_volume(&pool, volume.id, &file, from, format)
            .await
            .map_err(|e| failed(&e))?;
        remove_file(&file).await;
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
    ctx.config.import_credentials.remove(id);
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        tracing::warn!(path = %dir.display(), error = %e, "cannot remove the import files");
    }
    Ok(())
}

/// 원본 디스크 `disk`를 가져오기 디렉터리 `dir`에 꺼내 두고 그 파일과 형식을 돌려준다. 꺼내기 전에 업로드
/// 디렉터리에 여유(꺼낼 크기 + 1 GiB)가 있는지 본다. ESXi extent는 씬 디스크도 전체 크기로 받고, 받은
/// 길이가 디스크 용량과 다르면 실패한다.
async fn fetch(
    ctx: &Ctx,
    origin: &Origin,
    dir: &Path,
    disk: &SourceDisk,
) -> Result<(PathBuf, SourceFormat), String> {
    let (path, format) = match origin {
        Origin::Ova { source, ova } => {
            let entry = ova
                .index
                .get(&disk.file)
                .cloned()
                .ok_or_else(|| format!("{:?} is not in the OVA", disk.file))?;
            check_space(dir, entry.size, "extract", &disk.file)?;
            let path = dir.join(EXTRACTED_DISK);
            let digest = ova.manifest.get(&disk.file).cloned();
            let (from, to) = (source.clone(), path.clone());
            tokio::task::spawn_blocking(move || extract(&from, &entry, &to, digest.as_ref()))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            (path, SourceFormat::Vmdk)
        }
        Origin::Esxi { conn, datastore } => {
            check_space(dir, disk.capacity_bytes, "download", &disk.file)?;
            let path = dir.join(DOWNLOADED_DISK);
            esxi::download(conn, datastore, &disk.file, &path, disk.capacity_bytes)
                .await
                .map_err(|e| e.to_string())?;
            (path, SourceFormat::Raw)
        }
    };
    ctx.storage
        .check_import_source(&path, format)
        .await
        .map_err(|e| e.to_string())?;
    Ok((path, format))
}

/// 업로드 디렉터리에 `size` 바이트를 쓰고도 여유가 남는지 본다.
fn check_space(dir: &Path, size: u64, doing: &str, file: &str) -> Result<(), String> {
    let available = fs4::available_space(dir).map_err(|e| e.to_string())?;
    if size.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(format!(
            "not enough space in the upload directory to {doing} {file}"
        ));
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

/// 실패한 가져오기가 만든 볼륨(파일과 리소스)과 추출하거나 받은 파일을 지우고, 가져오기를 `ready`로
/// 되돌려 이유를 남긴다. 원본은 남으므로 값을 바꿔 다시 실행할 수 있다. 다만 ESXi 가져오기인데 이 노드
/// 메모리에 자격 증명이 없으면(opld가 다시 시작했다) 다시 실행해도 소용없으므로 `failed`로 끝낸다.
/// 실행 중이 아니던 가져오기는 그대로 둔다.
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
        ctx.config.import_credentials.remove(id);
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
        remove_file(&dir.join(DOWNLOADED_DISK)).await;
    }
    // 자격 증명을 잃은 ESXi 가져오기(opld가 다시 시작했다)
    let esxi = get_import(ctx, id)
        .ok()
        .flatten()
        .is_some_and(|i| matches!(i.spec.source, ImportSource::Esxi(_)));
    let lost = esxi && ctx.config.import_credentials.get(id).is_none();
    update_import(ctx, id, |s| {
        if s.phase != ImportPhase::Importing {
            return false;
        }
        if lost {
            s.phase = ImportPhase::Failed;
            s.error = Some(CREDENTIALS_LOST.into());
        } else {
            s.phase = ImportPhase::Ready;
            s.error = Some(error.to_string());
        }
        true
    })
    .await;
}

/// 가져오기의 원본 디렉터리와 리소스, ESXi 자격 증명을 지운다. 실행 Task가 끝나지 않았으면 지우지 않는다(`importing`이어도
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
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {
            ctx.config.import_credentials.remove(id);
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}
```

`crates/opl-api/src/vm_imports.rs` 전체:

```rust
//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아내고 opLite VM 설정을 제안한다.
//! 원본은 올린 노드의 `{upload_dir}/imports/{가져오기 id}/source.ova`에 둔다. 실행과 지우기는 그 노드의
//! Task 실행기가 한다. ESXi·vCenter 데이터스토어의 VM은 `.vmx`·디스크립터를 HTTPS로 읽어([`esxi`]·
//! [`vmx`]) 같은 리소스를 만들고, 비밀번호는 요청을 받은 노드의 메모리([`ImportCredentials`])에만 둔다.

pub mod esxi;
pub mod ova;
pub mod ovf;
pub mod vmx;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt, TryStreamExt};
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
    EsxiConnection, EsxiSource, ImportPhase, ImportProposal, ImportRun, ImportSource, OvaSource,
    SATA_PORTS, SourceDisk, SourceVm, VmImport, VmImportSpec, VmImportStatus,
    normalize_fingerprint, propose,
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
use esxi::EsxiError;
use ova::{OvaError, parse_ova, read_index};
use vmx::{MAX_TEXT_BYTES, VmxError};

/// `/vm-imports/ova`·`/vm-imports/esxi` 경로와 겹치므로 가져오기 이름으로 쓸 수 없다.
const RESERVED_NAMES: [&str; 2] = ["ova", "esxi"];
/// 이 노드 메모리에 자격 증명이 없는(opld가 다시 시작했거나 요청이 끊긴) ESXi 가져오기의 오류
pub(crate) const CREDENTIALS_LOST: &str = "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again";
/// browse가 `.vmx`를 동시에 읽는 개수
const BROWSE_CONCURRENCY: usize = 8;
/// browse가 보는 VM 디렉터리의 최대 개수(넘으면 자른다)
const BROWSE_MAX_DIRS: usize = 500;
/// 가져오기 디렉터리 안의 원본 OVA 파일 이름
pub(crate) const SOURCE_OVA: &str = "source.ova";
/// 가져오기 Task가 디스크를 하나씩 추출해 두는 파일 이름(가져오기 디렉터리 안)
pub(crate) const EXTRACTED_DISK: &str = "disk.vmdk";
/// ESXi 가져오기 Task가 디스크(extent)를 하나씩 받아 두는 파일 이름(가져오기 디렉터리 안)
pub(crate) const DOWNLOADED_DISK: &str = "disk.raw";

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

/// 이 노드에 새 가져오기를 만들 수 있는지 본다(권한, 이름, 위트니스 노드). OVA 올리기와 ESXi 가져오기가
/// 함께 쓴다. 확인 뒤에 다른 요청이 이름을 가져가면 저장소에 만들 때 `name_taken`이 난다.
fn check_new(state: &AppState, principal: &Principal, name: &str) -> Result<(), ApiError> {
    state.authorize(principal, Action::Write, VmImportSpec::KIND)?;
    check_name(name)?;
    if RESERVED_NAMES.contains(&name) {
        return Err(ApiError::bad_request(format!(
            "the name {name} is reserved"
        )));
    }
    // 원본은 받은 노드에 남고 가져오기도 그 노드에서 하므로 VM을 돌리는 노드여야 한다.
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
    Ok(())
}

/// OVA를 받기 전의 확인. 사전 확인과 올리기가 함께 쓴다.
async fn check_upload(
    state: &AppState,
    principal: &Principal,
    name: &str,
    size: u64,
) -> Result<(), ApiError> {
    check_new(state, principal, name)?;
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

/// ESXi 가져오기의 자격 증명(가져오기 id → 연결). 비밀번호를 Raft·Task·로그에 남기지 않으려고 요청을
/// 받은 노드의 메모리에만 둔다. opld가 다시 시작하면 사라진다(그 가져오기는 시작 정리가 `failed`로 바꾼다).
#[derive(Default)]
pub struct ImportCredentials(Mutex<HashMap<Uuid, EsxiConnection>>);

impl ImportCredentials {
    pub fn insert(&self, import: Uuid, conn: EsxiConnection) {
        self.0.lock().unwrap().insert(import, conn);
    }

    pub fn get(&self, import: Uuid) -> Option<EsxiConnection> {
        self.0.lock().unwrap().get(&import).cloned()
    }

    pub fn remove(&self, import: Uuid) {
        self.0.lock().unwrap().remove(&import);
    }

    pub fn is_empty(&self) -> bool {
        self.0.lock().unwrap().is_empty()
    }
}

/// 만들기를 끝내지 못한 가져오기의 자격 증명을 지운다. 정상 경로에서는 `armed`를 내린다.
struct CredentialGuard {
    credentials: Arc<ImportCredentials>,
    import: Uuid,
    armed: bool,
}

impl CredentialGuard {
    fn insert(credentials: Arc<ImportCredentials>, import: Uuid, conn: EsxiConnection) -> Self {
        credentials.insert(import, conn);
        CredentialGuard {
            credentials,
            import,
            armed: true,
        }
    }
}

impl Drop for CredentialGuard {
    fn drop(&mut self) {
        if self.armed {
            self.credentials.remove(self.import);
        }
    }
}

/// ESXi 오류를 API 오류로. 지문이 없으면 409 `untrusted_certificate`, 다르면 409 `fingerprint_mismatch`
/// (둘 다 `details.fingerprint`에 서버 인증서의 지문), 인증 실패 400 `esxi_auth`, 연결할 수 없으면 502
/// `esxi_unreachable`. 없는 파일·그 밖의 HTTP 응답·잘못된 응답은 400(메시지에 이유).
pub(crate) fn esxi_error(e: EsxiError) -> ApiError {
    let with_fingerprint = |mut error: ApiError, fingerprint: &str| {
        error.details = Some(serde_json::json!({ "fingerprint": fingerprint }));
        error
    };
    match e {
        EsxiError::Untrusted(actual) => with_fingerprint(
            ApiError::conflict(
                "untrusted_certificate",
                "verify the ESXi certificate fingerprint",
            ),
            &actual,
        ),
        EsxiError::Fingerprint { ref actual, .. } => with_fingerprint(
            ApiError::conflict("fingerprint_mismatch", e.to_string()),
            actual,
        ),
        EsxiError::Auth => ApiError::new(StatusCode::BAD_REQUEST, "esxi_auth", e.to_string()),
        EsxiError::Connect(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "esxi_unreachable", e.to_string())
        }
        EsxiError::NotFound(_)
        | EsxiError::Http(_)
        | EsxiError::TooLarge { .. }
        | EsxiError::Invalid(_) => ApiError::bad_request(e.to_string()),
        EsxiError::Io(e) => ApiError::internal(e),
    }
}

/// 요청의 지문을 정규화한다. 모양이 틀리면 연결하기 전에 400(어떤 인증서와도 맞지 않으므로).
fn check_fingerprint(conn: &mut EsxiConnection) -> Result<(), ApiError> {
    if let Some(f) = &conn.fingerprint {
        let normalized = normalize_fingerprint(f).ok_or_else(|| {
            ApiError::bad_request(format!(
                "invalid fingerprint {f:?}; expected the 64 hex digits of a SHA-256 fingerprint"
            ))
        })?;
        conn.fingerprint = Some(normalized);
    }
    Ok(())
}

/// 켜진 VM이 디렉터리에 남기는 항목인지: 잠금(`.lck`로 끝남, 디렉터리일 수도), NFS 잠금(`.lck-`로
/// 시작), 스왑 파일(`.vswp`, 켜진 동안만 있다). 짐작일 뿐이다.
fn lock_file(name: &str) -> bool {
    name.ends_with(".lck") || name.starts_with(".lck-") || name.ends_with(".vswp")
}

/// 일시 중단된 VM이 디렉터리에 남기는 메모리 상태 파일(`.vmss`)인지
fn suspend_file(name: &str) -> bool {
    name.ends_with(".vmss")
}

fn vmx_error(e: VmxError) -> ApiError {
    ApiError::bad_request(e.0)
}

/// 연결 자체의 문제(인증·지문·연결)인지. browse는 이것만 요청 전체의 실패로 보고, 디렉터리 하나를 읽지
/// 못한 것은 그 디렉터리만 건너뛴다.
fn connection_failed(e: &EsxiError) -> bool {
    matches!(
        e,
        EsxiError::Auth
            | EsxiError::Connect(_)
            | EsxiError::Untrusted(_)
            | EsxiError::Fingerprint { .. }
            | EsxiError::Io(_)
    )
}

/// browse 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiBrowse {
    pub datastores: Vec<EsxiDatastore>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiDatastore {
    pub name: String,
    /// 맨 위 디렉터리마다 첫 `.vmx`(경로 순서)
    pub vms: Vec<EsxiVm>,
}

/// 데이터스토어의 VM 요약. `.vmx`를 해석할 수 없으면 `error`가 있고 나머지는 빈 값이다.
#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiVm {
    /// 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`)
    pub vmx: String,
    pub name: String,
    pub guest_os: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disks: u32,
    /// 디렉터리에 잠금·스왑 파일(`.lck`, `.lck-…`, `.vswp`)이 있거나(켜져 있을 수 있다) 일시 중단 파일
    /// (`.vmss`)이 있다
    pub locked: bool,
    pub error: Option<String>,
}

/// ESXi·vCenter에 연결해 데이터스토어마다 맨 위 디렉터리의 VM(`.vmx`)을 요약한다. 지문이 없으면 409
/// `untrusted_certificate`(`details.fingerprint`), 다르면 409 `fingerprint_mismatch`, 인증 실패 400
/// `esxi_auth`, 연결할 수 없으면 502 `esxi_unreachable`. VM 디렉터리는 500개까지 본다.
#[utoipa::path(post, path = "/vm-imports/esxi/browse", tag = "vm-imports",
    request_body = EsxiConnection,
    responses((status = 200, body = EsxiBrowse), (status = "4XX", body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
pub async fn browse_esxi(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut conn): Json<EsxiConnection>,
) -> Result<Json<EsxiBrowse>, ApiError> {
    state.authorize(&principal, Action::Write, VmImportSpec::KIND)?;
    check_fingerprint(&mut conn)?;
    let datastores = esxi::list(&conn, None, "").await.map_err(esxi_error)?;
    let mut dirs = Vec::new();
    for ds in &datastores {
        match esxi::list(&conn, Some(&ds.name), "").await {
            Ok(entries) => dirs.extend(
                entries
                    .into_iter()
                    .filter(|e| e.is_dir)
                    .map(|e| (ds.name.clone(), e.name)),
            ),
            Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
            Err(e) => {
                tracing::info!(datastore = %ds.name, error = %e, "cannot list an ESXi datastore")
            }
        }
    }
    dirs.truncate(BROWSE_MAX_DIRS);
    let conn = &conn;
    let vms: Vec<Option<(String, EsxiVm)>> = futures_util::stream::iter(dirs)
        .map(|(ds, dir)| async move {
            Ok::<_, ApiError>(browse_dir(conn, &ds, &dir).await?.map(|vm| (ds, vm)))
        })
        .buffered(BROWSE_CONCURRENCY)
        .try_collect()
        .await?;
    let mut vms: Vec<(String, EsxiVm)> = vms.into_iter().flatten().collect();
    let datastores = datastores
        .iter()
        .map(|ds| EsxiDatastore {
            name: ds.name.clone(),
            vms: vms
                .extract_if(.., |(name, _)| *name == ds.name)
                .map(|(_, vm)| vm)
                .collect(),
        })
        .collect();
    Ok(Json(EsxiBrowse { datastores }))
}

/// 디렉터리 `dir`의 첫 `.vmx`를 요약한다. `.vmx`가 없거나 디렉터리를 읽을 수 없으면 None.
async fn browse_dir(
    conn: &EsxiConnection,
    datastore: &str,
    dir: &str,
) -> Result<Option<EsxiVm>, ApiError> {
    let entries = match esxi::list(conn, Some(datastore), dir).await {
        Ok(entries) => entries,
        Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
        Err(e) => {
            tracing::info!(datastore, dir, error = %e, "cannot list an ESXi directory");
            return Ok(None);
        }
    };
    let Some(file) = entries
        .iter()
        .find(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".vmx"))
    else {
        return Ok(None);
    };
    let path = format!("{dir}/{}", file.name);
    let locked = entries
        .iter()
        .any(|e| lock_file(&e.name) || suspend_file(&e.name));
    let summary = match esxi::get_small(conn, datastore, &path, MAX_TEXT_BYTES).await {
        Ok(text) => vmx::parse_vmx(&text)
            .and_then(|v| vmx::vmx_to_source(&v, &path))
            .map_err(|e| e.0),
        Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
        Err(e) => Err(e.to_string()),
    };
    let vm = match summary {
        Ok((vm, _, disks)) => EsxiVm {
            vmx: path,
            name: vm.name,
            guest_os: vm.guest_os,
            vcpus: vm.vcpus,
            memory_mib: vm.memory_mib,
            disks: disks.len() as u32,
            locked,
            error: None,
        },
        Err(error) => EsxiVm {
            name: file
                .name
                .get(..file.name.len() - ".vmx".len())
                .unwrap_or(&file.name)
                .to_string(),
            vmx: path,
            guest_os: String::new(),
            vcpus: 0,
            memory_mib: 0,
            disks: 0,
            locked,
            error: Some(error),
        },
    };
    Ok(Some(vm))
}

/// ESXi에서 가져오기 만들기 요청
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EsxiImportRequest {
    /// 가져오기 이름
    pub name: String,
    /// 연결(`fingerprint`가 있어야 한다)
    pub connection: EsxiConnection,
    pub datastore: String,
    /// 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`)
    pub vmx: String,
}

/// 데이터스토어 안의 상대 경로이고 `.vmx`로 끝나는지
fn valid_vmx_path(path: &str) -> bool {
    path.ends_with(".vmx")
        && !path.contains('\\')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// ESXi·vCenter 데이터스토어의 VM으로 `ready` 가져오기를 만든다(201). 이 노드가 `.vmx`와 디스크
/// 디스크립터를 읽어 원본 VM과 제안을 만들고, 자격 증명은 이 노드의 메모리에만 둔다(실행 Task도 이
/// 노드에서 돈다). 연결 오류는 browse와 같다(지문이 없으면 409 `untrusted_certificate`). 스냅샷이 있는
/// VM, 다른 데이터스토어의 디스크, 통째로 받을 수 없는 디스크는 400. 잠금·일시 중단 파일이 있으면 경고만
/// 한다.
#[utoipa::path(post, path = "/vm-imports/esxi", tag = "vm-imports",
    request_body = EsxiImportRequest,
    responses((status = 201, body = Resource<VmImportSpec, VmImportStatus>), (status = "4XX", body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_esxi(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<EsxiImportRequest>,
) -> Result<(StatusCode, Json<VmImport>), ApiError> {
    check_new(&state, &principal, &req.name)?;
    let EsxiImportRequest {
        name,
        connection: mut conn,
        datastore,
        vmx: vmx_path,
    } = req;
    check_fingerprint(&mut conn)?;
    if datastore.is_empty() || !valid_vmx_path(&vmx_path) {
        return Err(ApiError::bad_request(format!(
            "invalid .vmx path {vmx_path:?} on datastore {datastore:?}"
        )));
    }
    let text = esxi::get_small(&conn, &datastore, &vmx_path, MAX_TEXT_BYTES)
        .await
        .map_err(esxi_error)?;
    // 지문이 맞아 읽었으므로 지문은 있고 모양도 맞다.
    let fingerprint = conn
        .fingerprint
        .as_deref()
        .and_then(normalize_fingerprint)
        .ok_or_else(|| ApiError::bad_request("invalid certificate fingerprint"))?;
    conn.fingerprint = Some(fingerprint.clone());
    let parsed = vmx::parse_vmx(&text).map_err(vmx_error)?;
    let (mut vm, mut warnings, disks) =
        vmx::vmx_to_source(&parsed, &vmx_path).map_err(vmx_error)?;
    for disk in disks {
        let path = vmx::disk_path(&vmx_path, &datastore, &disk.path).map_err(vmx_error)?;
        let shown = format!("[{datastore}] {path}");
        let text = match esxi::get_small(&conn, &datastore, &path, MAX_TEXT_BYTES).await {
            Ok(text) => text,
            // 크기 제한을 넘었다: 디스크립터가 따로 없는 VMDK(스파스·호스트형)다.
            Err(EsxiError::TooLarge { .. }) => {
                return Err(ApiError::bad_request(format!(
                    "{shown} is not a VMDK descriptor; only VMFS and flat disks can be imported"
                )));
            }
            Err(e) => return Err(esxi_error(e)),
        };
        let (file, capacity_bytes) = vmx::parse_descriptor(&text)
            .and_then(|d| vmx::check_flat(&d))
            .map_err(|e| ApiError::bad_request(format!("{shown}: {e}")))?;
        // extent는 디스크립터와 같은 디렉터리에 있다.
        let file = match path.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{file}"),
            None => file,
        };
        vm.disks.push(SourceDisk {
            name: disk.name,
            file,
            capacity_bytes,
            controller: disk.controller,
        });
    }
    let dir = vmx_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let entries = esxi::list(&conn, Some(&datastore), dir)
        .await
        .map_err(esxi_error)?;
    if entries.iter().any(|e| suspend_file(&e.name)) {
        warnings.insert(
            0,
            "the VM is suspended; power it off in VMware before importing (its memory state is not imported)".into(),
        );
    }
    if entries.iter().any(|e| lock_file(&e.name)) {
        warnings.insert(
            0,
            "the VM looks powered on (lock files found); power it off before importing".into(),
        );
    }
    let (_, networks) = state.cluster.list::<NetworkSpec>()?;
    let networks: Vec<String> = networks.into_iter().map(|n| n.name).collect();
    let (proposal, proposal_warnings) = propose(&vm, &networks);
    warnings.extend(proposal_warnings);
    let spec = VmImportSpec {
        node: state.node,
        source: ImportSource::Esxi(EsxiSource {
            host: conn.host.clone(),
            port: conn.port,
            datacenter: conn.datacenter.clone(),
            datastore,
            vmx: vmx_path,
            user: conn.user.clone(),
            fingerprint,
        }),
    };
    let mut import: VmImport = Resource::new(name, spec);
    import.status = VmImportStatus {
        phase: ImportPhase::Ready,
        source_vm: Some(vm),
        proposal: Some(proposal),
        warnings,
        vm: None,
        error: None,
    };
    // 만들자마자 다른 노드로 온 실행 요청의 Task가 자격 증명을 찾을 수 있게 먼저 넣는다. 만들기가
    // 실패하거나 클라이언트가 끊어 이 future가 버려지면 지운다.
    let mut guard = CredentialGuard::insert(state.import_credentials.clone(), import.id, conn);
    import.revision = state.cluster.create(&import).await?;
    guard.armed = false;
    Ok((StatusCode::CREATED, Json(import)))
}

/// 이전 opld가 남긴 가져오기 파일을 정리한다(노드가 시작할 때, API를 띄우기 전). 리소스가 없는
/// `imports/{id}` 디렉터리를 지우고, 이 노드의 `ready` OVA 가져오기인데 원본이 없으면 `failed`로 바꾼다.
/// 이 노드의 `ready` ESXi 가져오기는 메모리의 자격 증명을 잃었으므로 `failed`로 바꾼다(ESXi 가져오기는
/// 실행할 때만 디렉터리를 쓰므로 디렉터리는 보지 않는다).
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
        let esxi = matches!(import.spec.source, ImportSource::Esxi(_));
        if status.phase == ImportPhase::Ready && esxi {
            tracing::warn!(import = %import.name, "the ESXi credentials of the import were lost");
            status.phase = ImportPhase::Failed;
            status.error = Some(CREDENTIALS_LOST.into());
        } else if status.phase == ImportPhase::Ready
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
/// `done`으로 끝내고 409 `import_done`. 원본이 이 노드에 있으면 원본 디렉터리와 ESXi 자격 증명도 지운다
/// (다른 노드의 것은 가져오기를 지울 때 그 노드가 지운다).
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
        state.import_credentials.remove(import.id);
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

/// 원본 디렉터리에 가장 큰 디스크를 추출하고도 여유가 남는지 본다(아니면 507). OVA 원본이 없으면 409
/// `import_failed`. ESXi 원본은 디스크를 통째로 받으므로(씬 디스크도) 가장 큰 디스크의 용량을 본다.
async fn check_extract_space(
    state: &AppState,
    import: &VmImport,
    src: &SourceVm,
) -> Result<(), ApiError> {
    if let ImportSource::Esxi(_) = import.spec.source {
        let largest = src.disks.iter().map(|d| d.capacity_bytes).max();
        return check_upload_space(state, largest.unwrap_or(0)).await;
    }
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

/// 이 노드의 ESXi 가져오기인데 메모리에 자격 증명이 없으면(opld가 다시 시작했다) 가져오기를 `failed`로
/// 바꾸고 409 `import_failed`. 다시 실행해도 소용없으므로 Task를 만들지 않는다. 다른 노드의 것은 그
/// 노드의 Task가 본다.
async fn check_credentials(state: &AppState, import: &VmImport) -> Result<(), ApiError> {
    if !matches!(import.spec.source, ImportSource::Esxi(_))
        || import.spec.node != state.node
        || state.import_credentials.get(import.id).is_some()
    {
        return Ok(());
    }
    let status = VmImportStatus {
        phase: ImportPhase::Failed,
        error: Some(CREDENTIALS_LOST.into()),
        ..import.status.clone()
    };
    if let Err(e) = state
        .cluster
        .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
        .await
    {
        tracing::warn!(import = %import.name, error = %e, "cannot record the import state");
    }
    Err(ApiError::conflict(
        "import_failed",
        format!(
            "vm-import {:?} cannot be run: {CREDENTIALS_LOST}",
            import.name
        ),
    ))
}

/// `ready` 가져오기를 `run`의 값으로 실행할 Task를 돌려준다(202). Task는 원본이 있는 노드에서 돌고, 대상은
/// 미리 정한 새 VM id다. 실행 Task가 끝나지 않았으면 409 `import_busy`, 이미 가져왔으면 `import_done`(끝난 실행
/// Task가 VM을 만들었으면 가져오기를 `done`으로 끝내고), 원본을 쓸 수
/// 없으면 `import_failed`, 원본 노드가 오프라인이면 `node_offline`. 원본 NIC의 네트워크마다 있는 opLite
/// 네트워크로 매핑해야 하고(400), VM 이름과 볼륨 이름 `{name}-disk{i}`가 비어 있어야 한다(409
/// `name_taken`). 풀은 원본 노드에서 쓸 수 있어야 하고(409 `pool_not_on_node`·`pool_unavailable`), 결과
/// VM 스펙은 VM 만들기의 검증을 통과해야 한다(400). 원본 MAC을 쓰면 다른 VM과 겹치지 않아야 한다(409
/// `mac_in_use`). 원본이 이 노드에 있으면 가장 큰 디스크를 추출하거나 받을 여유도 본다(507).
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
    check_credentials(&state, &import).await?;
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

    /// 만들기 전에 넣은 자격 증명은 만들기를 끝내지 못하면(guard가 그대로 버려지면) 지워진다.
    #[test]
    fn credential_guard_removes_unfinished_credentials() {
        let credentials = Arc::new(ImportCredentials::default());
        let conn = EsxiConnection {
            host: "esxi.example".into(),
            port: 443,
            user: "root".into(),
            password: "pw".into(),
            datacenter: "ha-datacenter".into(),
            fingerprint: None,
        };
        let (failed, created) = (Uuid::now_v7(), Uuid::now_v7());
        let guard = CredentialGuard::insert(credentials.clone(), failed, conn.clone());
        assert!(credentials.get(failed).is_some());
        drop(guard);
        assert!(credentials.get(failed).is_none());
        let mut guard = CredentialGuard::insert(credentials.clone(), created, conn);
        guard.armed = false;
        drop(guard);
        assert!(credentials.get(created).is_some());
    }
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use openraft::{BasicNode, ServerState};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig, hash_password};
```

을 다음으로 바꾼다:

```rust
use openraft::{BasicNode, ServerState};
use opl_api::vm_imports::ImportCredentials;
use opl_api::{AppState, TaskRunner, TaskRunnerConfig, hash_password};
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT));
        let runner = TaskRunner::spawn(
```

을 다음으로 바꾼다:

```rust
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT));
        // opld처럼 API와 Task 실행기가 ESXi 자격 증명을 함께 쓴다.
        let credentials: Arc<ImportCredentials> = Arc::default();
        let mut config = runner_config(node, dir.path());
        config.import_credentials = credentials.clone();
        let runner = TaskRunner::spawn(
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            reconciler.handle(),
            runner_config(node, dir.path()),
        )
        .await;
        let state = Arc::new(AppState::new(
            cluster.clone(),
            node,
            hv_dyn,
            dir.path().join("uploads"),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
```

을 다음으로 바꾼다:

```rust
            reconciler.handle(),
            config,
        )
        .await;
        let mut state = AppState::new(cluster.clone(), node, hv_dyn, dir.path().join("uploads"));
        state.import_credentials = credentials;
        let state = Arc::new(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let mut config = runner_config(self.node, self.dir.path());
        tweak(&mut config);
```

을 다음으로 바꾼다:

```rust
        let mut config = runner_config(self.node, self.dir.path());
        config.import_credentials = self.state.import_credentials.clone();
        tweak(&mut config);
```

`crates/opl-api/tests/vm_import_tasks.rs`에서

```rust
//! streamOptimized VMDK를 담은 OVA를 가져와 꺼진 VM과 볼륨을 만든다. `.mf` 해시가 다르거나 VM을 만들지
//! 못하면 만든 볼륨을 지우고 가져오기를 `ready`로 되돌리며, 고친 값으로 다시 실행할 수 있다.
//! qemu-img와 qemu-io가 필요하다.
```

을 다음으로 바꾼다:

```rust
//! streamOptimized VMDK를 담은 OVA를 가져와 꺼진 VM과 볼륨을 만든다. `.mf` 해시가 다르거나 VM을 만들지
//! 못하면 만든 볼륨을 지우고 가져오기를 `ready`로 되돌리며, 고친 값으로 다시 실행할 수 있다. ESXi
//! 가져오기는 가짜 ESXi에서 extent를 받아 같은 일을 한다.
//! qemu-img와 qemu-io가 필요하다.
```

`crates/opl-api/tests/vm_import_tasks.rs`에서

```rust

use std::path::{Path, PathBuf};
```

을 다음으로 바꾼다:

```rust

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
```

`crates/opl-api/tests/vm_import_tasks.rs`에서

```rust

use common::ustar::{Header, Tar};
```

을 다음으로 바꾼다:

```rust

use common::esxi::MockEsxi;
use common::ustar::{Header, Tar};
```

`crates/opl-api/tests/vm_import_tasks.rs`에서

```rust
    assert!(get_volume(&env, "web-disk0").is_none());
    assert_eq!(source_files(&env, &failed), ["source.ova"]);
}
```

을 다음으로 바꾼다:

```rust
    assert!(get_volume(&env, "web-disk0").is_none());
    assert_eq!(source_files(&env, &failed), ["source.ova"]);
}

// --- ESXi에서 바로 가져오기 ---

const ESXI_USER: &str = "root";
const ESXI_PASSWORD: &str = "S3cret-esxi-pw";
const ESXI_MAC: &str = "00:50:56:aa:bb:cc";
const CREDENTIALS_LOST: &str = "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again";

/// 2 vCPU(소켓당 코어 2), 1024 MiB, efi, Linux, SCSI 디스크 `web01.vmdk`, "VM Network"의 vmxnet3 NIC
/// 하나([`ESXI_MAC`])
const WEB01_VMX: &str = r#".encoding = "UTF-8"
displayName = "web01"
guestOS = "ubuntu-64"
numvcpus = "2"
cpuid.coresPerSocket = "2"
memSize = "1024"
firmware = "efi"
scsi0.present = "TRUE"
scsi0.virtualDev = "pvscsi"
scsi0:0.present = "TRUE"
scsi0:0.fileName = "web01.vmdk"
ethernet0.present = "TRUE"
ethernet0.networkName = "VM Network"
ethernet0.virtualDev = "vmxnet3"
ethernet0.addressType = "static"
ethernet0.address = "00:50:56:aa:bb:cc"
"#;

/// 데이터스토어 `datastore1`의 VM `web01`: 디스크립터는 4 MiB(8192 섹터) VMFS 디스크이고, extent
/// `web01-flat.vmdk`는 처음 64 KiB가 `0xaa`, 1 MiB부터 64 KiB가 `0x5a`인 `flat_len` 바이트의 raw다.
async fn web01_esxi(flat_len: usize) -> MockEsxi {
    let mut flat = vec![0u8; flat_len];
    flat[..64 << 10].fill(0xaa);
    flat[MIB as usize..(MIB as usize + (64 << 10))].fill(0x5a);
    let descriptor = "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
                      createType=\"vmfs\"\n\n# Extent description\nRW 8192 VMFS \"web01-flat.vmdk\"\n";
    let files = BTreeMap::from([
        ("datastore1/web01/web01.vmx".to_string(), WEB01_VMX.into()),
        ("datastore1/web01/web01.vmdk".to_string(), descriptor.into()),
        ("datastore1/web01/web01-flat.vmdk".to_string(), flat),
    ]);
    MockEsxi::start(files, ESXI_USER, ESXI_PASSWORD).await
}

/// 가짜 ESXi의 `web01`로 가져오기 `web`을 만든다.
async fn create_esxi(admin: &Client, esxi: &MockEsxi) -> VmImport {
    let body = json!({
        "name": "web",
        "connection": {
            "host": esxi.addr.ip().to_string(),
            "port": esxi.addr.port(),
            "user": ESXI_USER,
            "password": ESXI_PASSWORD,
            "fingerprint": esxi.fingerprint,
        },
        "datastore": "datastore1",
        "vmx": "web01/web01.vmx",
    });
    let (status, body) = admin.post("/vm-imports/esxi", body).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    serde_json::from_value(body).unwrap()
}

/// ESXi 가져오기는 extent를 HTTPS로 받아(기본 인증) raw로 볼륨에 옮기고, 제안대로 원본 MAC을 쓰는 꺼진
/// VM을 만든다. 끝나면 가져오기는 `done`이고, 받은 파일과 메모리의 자격 증명은 없다.
#[tokio::test]
async fn imports_from_esxi_into_a_stopped_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let esxi = web01_esxi(4 * MIB as usize).await;
    let created = create_esxi(&admin, &esxi).await;
    assert!(env.state.import_credentials.get(created.id).is_some());

    let task = run(&admin, run_request()).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = get_vm(&env, "web").unwrap();
    let spec = &vm.spec;
    assert_eq!((spec.cpu.sockets, spec.cpu.cores), (1, 2));
    assert_eq!(spec.memory_mib, 1024);
    assert_eq!(spec.firmware, Firmware::Uefi);
    assert_eq!(spec.power, PowerState::Stopped);
    assert_eq!(spec.disks.len(), 1);
    assert_eq!(spec.disks[0].volume, "web-disk0");
    assert_eq!(spec.disks[0].bus, DiskBus::VirtioBlk);
    assert_eq!(spec.nics[0].network, "lan");
    assert_eq!(spec.nics[0].model, NicModel::Virtio);
    assert_eq!(
        spec.nics[0].mac.map(|m| m.to_string()).as_deref(),
        Some(ESXI_MAC)
    );

    let volume = get_volume(&env, "web-disk0").unwrap();
    assert_eq!(volume.spec.size_bytes, 4 * MIB);
    assert_eq!(volume.status.phase, VolumePhase::Ready);
    let path = pool_dir(&env).join(format!("{}.qcow2", volume.id));
    assert!(qemu_io(&path, "qcow2", "read -P 0xaa 0 64k"));
    assert!(qemu_io(&path, "qcow2", "read -P 0x5a 1M 64k"));
    assert!(qemu_io(&path, "qcow2", "read -P 0 2M 64k"));

    let done = import(&env).unwrap();
    assert_eq!(done.status.phase, ImportPhase::Done);
    assert_eq!(done.status.vm, Some(vm.id));
    assert!(source_files(&env, &done).is_empty());
    assert!(env.state.import_credentials.get(done.id).is_none());

    // extent를 기본 인증으로 받았다.
    let flat = esxi
        .requests()
        .into_iter()
        .find(|r| r.path == "/folder/web01/web01-flat.vmdk")
        .unwrap();
    assert_eq!(flat.method, "GET");
    assert!(flat.query.contains("dsName=datastore1"), "{}", flat.query);
    assert!(flat.authorization.unwrap().starts_with("Basic "));
}

/// 받은 extent의 길이가 디스크립터의 크기와 다르면 실행은 실패한다: 볼륨과 받은 파일을 남기지 않고
/// 가져오기는 `ready`로 돌아가 이유를 남긴다. 자격 증명은 남아 다시 실행할 수 있고, 가져오기를 지우면
/// 메모리에서도 지운다.
#[tokio::test]
async fn esxi_download_of_the_wrong_length_fails_and_leaves_no_volumes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let esxi = web01_esxi(3 * MIB as usize).await;
    let created = create_esxi(&admin, &esxi).await;

    let task = run(&admin, run_request()).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert_eq!(
        error,
        "cannot import disk \"disk0\": downloaded 3145728 bytes of [datastore1] \
         web01/web01-flat.vmdk, expected 4194304"
    );
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Ready);
    assert_eq!(failed.status.error.as_deref(), Some(error));
    assert!(get_vm(&env, "web").is_none());
    assert!(get_volume(&env, "web-disk0").is_none());
    let left: Vec<_> = std::fs::read_dir(pool_dir(&env)).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(source_files(&env, &failed).is_empty());
    assert!(env.state.import_credentials.get(created.id).is_some());

    let (status, task) = admin.delete("/vm-imports/web").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(import(&env).is_none());
    assert!(env.state.import_credentials.is_empty());
}

/// Task가 시작할 때 메모리에 자격 증명이 없으면(실행 요청을 받은 뒤 opld가 다시 시작했다) 실행은 실패하고
/// 가져오기는 `failed`다. 다시 실행해도 소용없으므로 실행 API는 409 `import_failed`.
#[tokio::test]
async fn esxi_import_without_credentials_fails() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let esxi = web01_esxi(4 * MIB as usize).await;
    let created = create_esxi(&admin, &esxi).await;
    // 실행 API는 자격 증명이 있을 때 Task를 만든다. Task가 시작하기 전에 자격 증명을 잃는다.
    env.stop_runner().await;
    let (status, task) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    env.state.import_credentials.remove(created.id);
    env.start_runner().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], CREDENTIALS_LOST);
    let failed = import(&env).unwrap();
    assert_eq!(failed.status.phase, ImportPhase::Failed);
    assert_eq!(failed.status.error.as_deref(), Some(CREDENTIALS_LOST));
    assert!(get_volume(&env, "web-disk0").is_none());
    assert!(
        !esxi
            .requests()
            .iter()
            .any(|r| r.path.ends_with("-flat.vmdk"))
    );

    let (status, body) = admin.post("/vm-imports/web/run", run_request()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "import_failed");
}
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust

/// 저장소에 ESXi 가져오기를 직접 만든다.
```

을 다음으로 바꾼다:

```rust

/// ESXi 가져오기의 실행 요청은 업로드 디렉터리에 가장 큰 디스크를 통째로 받을 여유(용량 + 1 GiB)를 본다
/// (씬 디스크도 전체 크기로 받는다). 모자라면 507이고 가져오기는 `ready`로 남는다. 원본 디렉터리가 없어도
/// 실행할 수 있다.
#[tokio::test]
async fn esxi_run_needs_space_for_the_largest_disk() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    network(&env, "lan").await;
    pool(&env, "local", vec![]).await;
    env.stop_runner().await;
    let available = fs4::available_space(env.dir.path()).unwrap();
    let mut files = esxi_files();
    files.insert(
        "datastore1/big/big.vmx".into(),
        WEB01_VMX.replace("web01", "big").into_bytes(),
    );
    files.insert(
        "datastore1/big/big.vmdk".into(),
        descriptor(
            "vmfs",
            &format!("RW {} VMFS \"big-flat.vmdk\"", available / 512 + 1),
            "",
        ),
    );
    let esxi = MockEsxi::start(files, ESXI_USER, ESXI_PASSWORD).await;
    for (name, vmx) in [("big", "big/big.vmx"), ("web01", "web01/web01.vmx")] {
        let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
        let (status, body) = admin
            .post(
                "/vm-imports/esxi",
                esxi_import(name, conn, "datastore1", vmx),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (status, body) = run(&admin, "big", run_request("big")).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{body}");
    assert_eq!(body["code"], "insufficient_storage");
    assert_eq!(
        import_named(&env, "big").unwrap().status.phase,
        ImportPhase::Ready
    );
    let (status, body) = run(&admin, "web01", run_request("web")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

/// 이 노드의 ESXi 가져오기인데 메모리에 자격 증명이 없으면 실행 요청은 Task를 만들지 않고 409
/// `import_failed`이고, 가져오기는 `failed`가 된다(다시 실행해도 소용없다).
#[tokio::test]
async fn esxi_run_without_credentials_fails_the_import() {
    let env = Env::start().await;
    let admin = env.admin().await;
    network(&env, "lan").await;
    pool(&env, "local", vec![]).await;
    let esxi = mock_esxi().await;
    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("web01", conn, "datastore1", "web01/web01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    env.state.import_credentials.remove(id);

    let lost = "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again";
    let (status, body) = run(&admin, "web01", run_request("web")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "import_failed");
    assert!(body["message"].as_str().unwrap().contains(lost), "{body}");
    let import = import_named(&env, "web01").unwrap();
    assert_eq!(import.status.phase, ImportPhase::Failed);
    assert_eq!(import.status.error.as_deref(), Some(lost));
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    assert!(tasks.is_empty(), "{tasks:?}");
}

/// 저장소에 ESXi 가져오기를 직접 만든다.
```

`crates/opld/src/member.rs`에서

```rust
        runner_config.migration_timeout = config.migration.timeout;
        let runner = TaskRunner::spawn(
```

을 다음으로 바꾼다:

```rust
        runner_config.migration_timeout = config.migration.timeout;
        // ESXi 가져오기의 자격 증명은 API가 받아 두고 가져오기 Task가 쓴다.
        runner_config.import_credentials = state.import_credentials.clone();
        let runner = TaskRunner::spawn(
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1125개, 실패 0

Run: `cargo test -p opl-api --features storage-it --test vm_import_tasks`
Expected: PASS (qemu-img가 필요)

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 407개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(vm-import): ESXi 가져오기 Task의 내려받기 단계 — 원본이 ESXi면 디스크마다 업로드 디렉터리 여유(용량 + 1 GiB)를 보고 extent를 이 노드 메모리의 자격 증명으로 HTTPS로 받아(disk.raw, 길이가 다르면 실패) raw로 볼륨에 옮김, Task 시작 때 자격 증명이 없거나 실패 정리 때 자격 증명이 없으면 가져오기를 failed(자격 증명 사라짐)로, done·지우기에서 자격 증명을 지움, 실행 API의 여유 검사는 ESXi면 가장 큰 디스크 용량, TaskRunnerConfig.import_credentials(opld와 시험 Env가 API와 같은 것을 넘김), 시험(가짜 ESXi로 가져오기·길이 불일치·자격 증명 없음, 실행 507)"
```

---

### Task 5: oplctl ESXi 명령

`oplctl import esxi-browse --host H --user U [--port] [--datacenter] [--fingerprint F] [--password-file P]`(표 `DATASTORE VMX NAME GUEST CPU MEMORY DISKS LOCKED`), `import esxi NAME --host --user --datastore --vmx --fingerprint [--port] [--datacenter] [--password-file]`. 비밀번호는 `--password-file`, `OPL_ESXI_PASSWORD`, 프롬프트 순(명령줄 인자로는 받지 않음). 지문이 없으면 서버가 알려 준 지문으로 다시 실행하라고 안내하고 종료 코드 1. `import list`·`get`의 원본은 `esxi://host/datastore/vmx`.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{
    ApiError, BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, fetch_ca, fingerprint, local,
};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지나 opld가 만드는 VXLAN 네트워크)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// VM NIC에 거는 방화벽 규칙 묶음. nftables가 있는 노드가 적용한다.
    SecurityGroup {
        #[command(subcommand)]
        cmd: SecurityGroupCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    /// VM 백업(백업 풀에 담은 디스크 복사본과 VM 설정)과 복원
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// 백업 작업: 정한 시각(UTC)에 고른 VM들을 백업하고 보존 규칙대로 옛 백업을 지운다.
    BackupJob {
        #[command(subcommand)]
        cmd: BackupJobCmd,
    },
    /// 다른 하이퍼바이저(VMware)의 VM 가져오기(OVA, ESXi·vCenter)
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get { node: String },
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다. 백업을 담는 풀로 표시하거나 끈다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args(["shared", "no_shared", "backups"])))]
struct PoolSet {
    pool: String,
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long, conflicts_with = "no_shared")]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
    /// VM 백업을 담는 풀로 표시한다(true, 디렉터리·NFS 풀만)거나 끈다(false, 백업이 남아 있으면 거절한다).
    #[arg(long, value_name = "true|false")]
    backups: Option<bool>,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`). 주지 않은 값은 보내지 않는다.
fn pool_set_body(set: &PoolSet) -> Value {
    let mut body = json!({});
    if set.shared || set.no_shared {
        body["shared"] = json!(set.shared);
    }
    if let Some(backups) = set.backups {
        body["backups"] = json!(backups);
    }
    body
}

/// `pool set`이 바꾼 것: `shared, backups on`
fn pool_set_summary(set: &PoolSet) -> String {
    let mut parts = Vec::new();
    if set.shared {
        parts.push("shared");
    } else if set.no_shared {
        parts.push("node-local");
    }
    match set.backups {
        Some(true) => parts.push("backups on"),
        Some(false) => parts.push("backups off"),
        None => {}
    }
    parts.join(", ")
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
    /// VM 백업을 담는 풀로 표시한다. 디렉터리·NFS 풀만 된다.
    #[arg(long, conflicts_with_all = ["lvm_thin", "zfs", "rbd", "lvm_shared"])]
    backups: bool,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
        backups,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    if backups {
        spec["backups"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크나 VXLAN 네트워크를 만든다.
    Create(NetworkCreate),
    Delete {
        network: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["bridge", "vxlan"])))]
struct NetworkCreate {
    name: String,
    /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
    #[arg(long)]
    bridge: Option<String>,
    /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
    #[arg(long, conflicts_with = "vxlan")]
    vlan: Option<u16>,
    /// VXLAN 네트워크 식별자(VNI, 1-16777215). opld가 모든 노드에 브리지 oplv<VNI>를 만든다.
    #[arg(long, value_name = "VNI")]
    vxlan: Option<u32>,
    /// VXLAN 네트워크의 MTU(1280-9000, 기본 1450)
    #[arg(long, conflicts_with = "bridge")]
    mtu: Option<u16>,
}

/// `network create`의 요청 본문(`POST /networks`). 주지 않은 값은 보내지 않는다.
fn network_create_body(c: &NetworkCreate) -> Value {
    let mut spec = serde_json::Map::new();
    if let Some(bridge) = &c.bridge {
        spec.insert("bridge".into(), json!(bridge));
    }
    if let Some(vlan) = c.vlan {
        spec.insert("vlan".into(), json!(vlan));
    }
    if let Some(vni) = c.vxlan {
        spec.insert("vxlan".into(), json!({ "vni": vni }));
    }
    if let Some(mtu) = c.mtu {
        spec.insert("mtu".into(), json!(mtu));
    }
    json!({ "name": c.name, "spec": spec })
}

#[derive(Subcommand)]
enum SecurityGroupCmd {
    List,
    Get {
        group: String,
    },
    /// 보안 그룹을 만든다. 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. 맞는 규칙이 없으면
    /// 들어오는 것은 막고 나가는 것은 보낸다.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 규칙 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`
        /// (예: "in accept tcp 22 10.0.0.0/8"). 여러 번 줄 수 있다.
        #[arg(long = "rule", value_name = "RULE", value_parser = parse_rule)]
        rules: Vec<Value>,
    },
    /// 보안 그룹을 지운다. 그룹을 쓰는 VM NIC가 없어야 한다.
    Delete {
        group: String,
    },
    /// 규칙을 더하거나 뺀다. 그룹을 쓰는 노드가 곧바로 적용한다.
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// 규칙 하나를 더한다(RULE은 `security-group create --rule`과 같다).
    Add {
        group: String,
        #[arg(value_parser = parse_rule)]
        rule: Value,
        /// 이 번호(1부터) 자리에 넣는다. 생략하면 맨 끝이다.
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// N번 규칙(1부터, `security-group get`의 순서)을 뺀다.
    Rm { group: String, n: usize },
}

/// `in accept tcp 22 10.0.0.0/8`처럼 쓴다: 방향과 동작, 그 뒤로 프로토콜·포트·원격 CIDR을 이 차례로
/// (모두 생략할 수 있다). 값의 나머지 검사(포트 범위, 프로토콜과 포트·주소의 짝)는 API가 한다.
fn parse_rule(s: &str) -> Result<Value, String> {
    let bad = || {
        format!("invalid rule {s:?}: use in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]")
    };
    let mut words = s.split_whitespace().peekable();
    let direction = words
        .next()
        .filter(|w| matches!(*w, "in" | "out"))
        .ok_or_else(bad)?;
    let action = words
        .next()
        .filter(|w| matches!(*w, "accept" | "drop"))
        .ok_or_else(bad)?;
    let mut rule = json!({ "direction": direction, "action": action });
    if let Some(protocol) = words.next_if(|w| matches!(*w, "tcp" | "udp" | "icmp" | "icmpv6")) {
        rule["protocol"] = json!(protocol);
    }
    if let Some(ports) = words.next_if(|w| is_ports(w)) {
        rule["ports"] = json!(ports);
    }
    if let Some(remote) = words.next_if(|w| w.contains(['.', ':'])) {
        rule["remote"] = json!(remote);
    }
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(rule)
}

/// `22`나 `8000-8080` 모양인지
fn is_ports(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match word.split_once('-') {
        Some((start, end)) => digits(start) && digits(end),
        None => digits(word),
    }
}

/// `security-group create`의 요청 본문(`POST /security-groups`)
fn security_group_create_body(name: &str, description: Option<&str>, rules: &[Value]) -> Value {
    let mut spec = json!({ "rules": rules });
    if let Some(description) = description {
        spec["description"] = json!(description);
    }
    json!({ "name": name, "spec": spec })
}

/// 그룹의 규칙에 `rule`을 `at`번(1부터, 생략하면 맨 끝) 자리에 넣은 목록
fn rules_with(group: &Value, rule: Value, at: Option<usize>) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = at.unwrap_or(rules.len() + 1);
    if at == 0 || at > rules.len() + 1 {
        bail!(
            "cannot add a rule at {at}: security group {} has {} rules (use 1 to {})",
            cell(&group["name"]),
            rules.len(),
            rules.len() + 1
        );
    }
    rules.insert(at - 1, rule);
    Ok(rules)
}

/// 그룹의 규칙에서 `n`번(1부터)을 뺀 목록
fn rules_without(group: &Value, n: usize) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if n == 0 || n > rules.len() {
        bail!(
            "security group {} has no rule {n} ({} rules, numbered from 1)",
            cell(&group["name"]),
            rules.len()
        );
    }
    rules.remove(n - 1);
    Ok(rules)
}

/// 그룹을 읽어 `edit`으로 만든 규칙 목록으로 바꾼다. 읽은 revision을 `If-Match`로 보내므로 그 사이
/// 다른 사람이 바꿨으면 실패한다(규칙 번호가 달라졌을 수 있다).
async fn patch_rules(
    client: &Client,
    group: &str,
    edit: impl FnOnce(&Value) -> anyhow::Result<Vec<Value>>,
) -> anyhow::Result<()> {
    let path = format!("/security-groups/{group}");
    let current = client.get(&path).await?;
    let rules = edit(&current)?;
    let r = client
        .request(Method::PATCH, &path)
        .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
        .json(&json!({ "rules": rules }));
    client.send(r).await?;
    Ok(())
}

/// `security-group list`의 열
const SECURITY_GROUP_HEADERS: &[&str] = &["NAME", "RULES", "DESCRIPTION"];

/// `security-group list`의 한 행
fn security_group_row(g: &Value) -> Vec<String> {
    vec![
        cell(&g["name"]),
        g["spec"]["rules"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        cell(&g["spec"]["description"]),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    /// NIC의 보안 그룹을 바꾼다. 실행 중이어도 곧바로 적용된다. 그룹을 주지 않으면 모두 뺀다.
    NicSg {
        vm: String,
        /// NIC 번호(0부터, `vm get`의 nics 순서)
        index: usize,
        groups: Vec<String>,
    },
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다. `,sg=A+B`를 붙이면 그 NIC에
    /// 보안 그룹을 건다(예: `lan,sg=web+db`).
    #[arg(long = "nic", value_name = "NETWORK[,sg=GROUP+...]", value_parser = parse_nic)]
    nics: Vec<NicArg>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

/// `--nic`으로 받은 네트워크와 보안 그룹
#[derive(Debug, Clone, PartialEq, Eq)]
struct NicArg {
    network: String,
    security_groups: Vec<String>,
}

/// `lan`, `lan,sg=web+db`처럼 쓴다. 그룹이 있는지는 API가 본다.
fn parse_nic(s: &str) -> Result<NicArg, String> {
    let bad = || format!("invalid nic {s:?}: use NETWORK or NETWORK,sg=GROUP+GROUP");
    let (network, groups) = match s.split_once(',') {
        Some((network, option)) => {
            let groups = option.strip_prefix("sg=").ok_or_else(bad)?;
            let groups: Vec<String> = groups.split('+').map(str::to_string).collect();
            if groups.iter().any(|g| g.is_empty() || g.contains(',')) {
                return Err(bad());
            }
            (network, groups)
        }
        None => (s, Vec::new()),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NicArg {
        network: network.into(),
        security_groups: groups,
    })
}

/// `vm create`의 NIC 하나. 그룹이 없으면 `security_groups`를 보내지 않는다.
fn nic_body(nic: &NicArg) -> Value {
    let mut body = json!({ "network": nic.network, "model": "virtio" });
    if !nic.security_groups.is_empty() {
        body["security_groups"] = json!(nic.security_groups);
    }
    body
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum BackupCmd {
    /// VM을 백업한다. 실행 중이면 디스크를 잠깐 스냅샷한 때의 상태(crash-consistent)를 담는다.
    Create {
        vm: String,
        /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
        #[arg(long)]
        pool: String,
        /// 생략하면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
        #[arg(long)]
        name: Option<String>,
    },
    /// 백업 목록. VM을 지운 백업도 보인다.
    List {
        /// 이 VM(이름이나 id)의 백업만. 지운 VM은 백업에 남은 이름으로 찾는다.
        #[arg(long)]
        vm: Option<String>,
    },
    Get {
        backup: String,
    },
    /// 백업 파일과 기록을 지운다. 실패한 백업도 지운다.
    Delete {
        backup: String,
        /// 파일을 지울 노드가 없으면 기록만 지운다(파일은 백업 풀에 남는다).
        #[arg(long)]
        force: bool,
    },
    /// 백업에서 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Restore {
        backup: String,
        /// 새 VM 이름
        #[arg(long)]
        name: String,
        /// 새 디스크를 둘 풀
        #[arg(long)]
        pool: String,
        /// 복원을 실행할 노드(이름이나 id). 생략하면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
        #[arg(long)]
        node: Option<String>,
    },
}

#[derive(Subcommand)]
enum BackupJobCmd {
    /// 백업 작업을 만든다. 정한 요일의 시각(UTC)에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛
    /// 백업을 지운다.
    Create(BackupJobCreate),
    List,
    Get {
        job: String,
    },
    /// 준 값만 바꾼다. 일정이나 보존 규칙의 일부만 주면 나머지는 지금 값을 그대로 둔다.
    Set(BackupJobSet),
    /// 작업만 지운다. 작업이 만든 백업은 남는다.
    Delete {
        job: String,
    },
    /// 지금 실행한다. 리더의 스케줄러가 다음 차례에 시작하며 끝나기를 기다리지 않는다.
    Run {
        job: String,
    },
}

/// 백업할 VM 고르기. 셋 중 하나만 준다.
#[derive(Args)]
struct VmsArgs {
    /// 모든 VM
    #[arg(long)]
    all: bool,
    /// 이 이름의 VM(여러 번 줄 수 있다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
    /// 이 라벨을 모두 가진 VM(여러 번 줄 수 있다)
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    labels: Vec<(String, String)>,
}

/// 보존 규칙. 모두 0이면 지우지 않는다.
#[derive(Args)]
struct KeepArgs {
    /// 최신 N개를 남긴다.
    #[arg(long, value_name = "N")]
    keep_last: Option<u32>,
    /// 그다음 하루(UTC)마다 가장 새 것 하나씩 N일
    #[arg(long, value_name = "N")]
    keep_daily: Option<u32>,
    /// 그다음 ISO 주마다 가장 새 것 하나씩 N주
    #[arg(long, value_name = "N")]
    keep_weekly: Option<u32>,
    /// 그다음 달마다 가장 새 것 하나씩 N달
    #[arg(long, value_name = "N")]
    keep_monthly: Option<u32>,
}

impl KeepArgs {
    fn given(&self) -> bool {
        self.keep_last.is_some()
            || self.keep_daily.is_some()
            || self.keep_weekly.is_some()
            || self.keep_monthly.is_some()
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").required(true).args(["all", "vms", "labels"])))]
struct BackupJobCreate {
    name: String,
    /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
    #[arg(long)]
    pool: String,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").args(["all", "vms", "labels"])))]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "all", "vms", "labels", "pool", "at", "days",
    "keep_last", "keep_daily", "keep_weekly", "keep_monthly", "enabled",
])))]
struct BackupJobSet {
    job: String,
    #[arg(long)]
    pool: Option<String>,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 모든 요일을 주면 매일이다.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 켜거나(다음 실행 시각을 지금부터 다시 정한다) 끈다. 진행 중인 실행은 끝까지 간다.
    #[arg(long, value_name = "true|false")]
    enabled: Option<bool>,
}

/// `--label`의 `KEY=VALUE`. 값에는 `=`가 있어도 된다.
fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err(format!("invalid label {s:?}: use KEY=VALUE")),
    }
}

/// VM 고르기(`vms` 값). 주지 않았으면 `None`이다.
fn vms_value(args: &VmsArgs) -> Option<Value> {
    if args.all {
        Some(json!({ "all": true }))
    } else if !args.vms.is_empty() {
        Some(json!({ "names": args.vms }))
    } else if !args.labels.is_empty() {
        let labels: serde_json::Map<String, Value> = args
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Some(json!({ "labels": labels }))
    } else {
        None
    }
}

fn days_value(days: &[Day]) -> Value {
    json!(days.iter().map(|d| value_name(*d)).collect::<Vec<_>>())
}

/// 보존 규칙(`keep` 값). 주지 않은 단위는 `current`의 값, 거기도 없으면 0이다.
fn keep_value(args: &KeepArgs, current: &Value) -> Value {
    let pick = |arg: Option<u32>, field: &str| {
        arg.map_or_else(|| current[field].as_u64().unwrap_or(0), u64::from)
    };
    json!({
        "last": pick(args.keep_last, "last"),
        "daily": pick(args.keep_daily, "daily"),
        "weekly": pick(args.keep_weekly, "weekly"),
        "monthly": pick(args.keep_monthly, "monthly"),
    })
}

/// `backup-job create`의 요청 본문
fn backup_job_create_body(args: &BackupJobCreate) -> Value {
    json!({
        "name": args.name,
        "spec": {
            "vms": vms_value(&args.vms),
            "pool": args.pool,
            "schedule": { "at": args.at, "days": days_value(&args.days) },
            "keep": keep_value(&args.keep, &Value::Null),
            "enabled": !args.disabled,
        }
    })
}

/// `backup-job set`의 요청 본문(`PATCH /backup-jobs/{key}`). 준 값만 보낸다. 서버는 일정과 보존
/// 규칙을 통째로 바꾸므로 그 일부만 주었으면 나머지는 읽은 작업(`current`)의 값으로 채운다.
fn backup_job_set_body(args: &BackupJobSet, current: &Value) -> Value {
    let mut body = json!({});
    if let Some(vms) = vms_value(&args.vms) {
        body["vms"] = vms;
    }
    if let Some(pool) = &args.pool {
        body["pool"] = json!(pool);
    }
    let schedule = &current["spec"]["schedule"];
    if args.at.is_some() || !args.days.is_empty() {
        body["schedule"] = json!({
            "at": args.at.as_ref().map_or_else(|| schedule["at"].clone(), |at| json!(at)),
            "days": if args.days.is_empty() {
                schedule["days"].as_array().map_or_else(|| json!([]), |d| json!(d))
            } else {
                days_value(&args.days)
            },
        });
    }
    if args.keep.given() {
        body["keep"] = keep_value(&args.keep, &current["spec"]["keep"]);
    }
    if let Some(enabled) = args.enabled {
        body["enabled"] = json!(enabled);
    }
    body
}

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
    /// ESXi·vCenter의 데이터스토어에 있는 VM을 보인다. 비밀번호는 `--password-file`이나
    /// `OPL_ESXI_PASSWORD`에서, 없으면 터미널에서 묻는다. 지문을 주지 않으면 서버 인증서의 지문을 알려 준다.
    EsxiBrowse {
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// ESXi·vCenter의 꺼진 VM을 읽어 VM 설정을 제안받는다. 비밀번호는 `esxi-browse`처럼 받고 연결을
    /// 받은 노드의 메모리에만 남으므로, 그 opld를 다시 시작하면 가져오기를 다시 만들어야 한다.
    Esxi {
        /// 가져오기 이름
        name: String,
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문(`import esxi-browse`가 알려 준다)
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        datastore: String,
        /// 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`)
        #[arg(long)]
        vmx: String,
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

/// ESXi·vCenter 연결. 비밀번호는 인자로 받지 않는다.
#[derive(Args)]
struct EsxiArgs {
    /// ESXi 호스트나 vCenter 주소
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u16,
    #[arg(long)]
    user: String,
    /// vCenter면 데이터센터 이름
    #[arg(long, default_value = "ha-datacenter")]
    datacenter: String,
    /// 비밀번호가 든 파일. 없으면 `OPL_ESXI_PASSWORD`, 그것도 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
}

/// ESXi 비밀번호. `--password-file`, `OPL_ESXI_PASSWORD` 순서로 보고, 둘 다 없으면 터미널에서 묻는다.
fn esxi_password(conn: &EsxiArgs) -> anyhow::Result<String> {
    if let Some(file) = &conn.password_file {
        return read_password(Some(file), "");
    }
    match std::env::var("OPL_ESXI_PASSWORD") {
        Ok(password) => Ok(password),
        Err(_) => Ok(rpassword::prompt_password(format!(
            "password for {}@{}: ",
            conn.user, conn.host
        ))?),
    }
}

/// API의 `EsxiConnection`
fn esxi_connection(conn: &EsxiArgs, fingerprint: Option<&str>, password: &str) -> Value {
    json!({
        "host": conn.host,
        "port": conn.port,
        "user": conn.user,
        "password": password,
        "datacenter": conn.datacenter,
        "fingerprint": fingerprint,
    })
}

/// `import esxi`의 요청 본문
fn esxi_import_body(name: &str, connection: &Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": connection, "datastore": datastore, "vmx": vmx })
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
    List,
    Get { task: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{pool}")).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let token = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client.get(&format!("/storage-pools/{pool}")).await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(set) => {
                client
                    .patch(
                        &format!("/storage-pools/{}", set.pool),
                        &pool_set_body(&set),
                    )
                    .await?;
                writeln!(out, "storage pool {}: {}", set.pool, pool_set_summary(&set))?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => {
                print_json(out, &client.get(&format!("/networks/{network}")).await?)?
            }
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client.delete(&format!("/networks/{network}")).await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::SecurityGroup { cmd } => security_group(&client, cmd, json_out, out).await?,
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{volume}/resize"), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Import { cmd } => import(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
    }
    Ok(())
}

async fn security_group(
    client: &Client,
    cmd: SecurityGroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SecurityGroupCmd::List => {
            let list = client.get("/security-groups").await?;
            print_list(
                out,
                json_out,
                &list,
                SECURITY_GROUP_HEADERS,
                security_group_row,
            )?;
        }
        SecurityGroupCmd::Get { group } => print_json(
            out,
            &client.get(&format!("/security-groups/{group}")).await?,
        )?,
        SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } => {
            let body = security_group_create_body(&name, description.as_deref(), &rules);
            client.post("/security-groups", &body).await?;
            writeln!(out, "security group {name} created")?;
        }
        SecurityGroupCmd::Delete { group } => {
            client.delete(&format!("/security-groups/{group}")).await?;
            writeln!(out, "security group {group} deleted")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Add { group, rule, at },
        } => {
            patch_rules(client, &group, |g| rules_with(g, rule, at)).await?;
            writeln!(out, "security group {group}: rule added")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Rm { group, n },
        } => {
            patch_rules(client, &group, |g| rules_without(g, n)).await?;
            writeln!(out, "security group {group}: rule {n} removed")?;
        }
    }
    Ok(())
}

async fn backup(
    client: &Client,
    cmd: BackupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupCmd::Create { vm, pool, name } => {
            let body = json!({ "pool": pool, "name": name });
            let task = client.post(&format!("/vms/{vm}/backups"), &body).await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            // 이름을 주지 않았으면 서버가 정한 이름을 읽는다.
            let name = match name {
                Some(name) => name,
                None => {
                    let id = cell(&task["spec"]["op"]["backup"]);
                    cell(&client.get(&format!("/backups/{id}")).await?["name"])
                }
            };
            writeln!(out, "backup {name} of vm {vm} created")?;
        }
        BackupCmd::List { vm } => {
            let mut r = client.request(Method::GET, "/backups");
            if let Some(vm) = &vm {
                r = r.query(&[("vm", vm)]);
            }
            let list = client.send(r).await?;
            print_list(out, json_out, &list, BACKUP_HEADERS, backup_row)?;
        }
        BackupCmd::Get { backup } => {
            print_json(out, &client.get(&format!("/backups/{backup}")).await?)?
        }
        BackupCmd::Delete { backup, force } => backup_delete(client, &backup, force, out).await?,
        BackupCmd::Restore {
            backup,
            name,
            pool,
            node,
        } => {
            let body = json!({ "name": name, "pool": pool, "node": node });
            let task = client
                .post(&format!("/backups/{backup}/restore"), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            writeln!(out, "vm {name} restored from backup {backup}")?;
        }
    }
    Ok(())
}

/// 백업을 지운다. `force`로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
async fn backup_delete(
    client: &Client,
    backup: &str,
    force: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut r = client.request(Method::DELETE, &format!("/backups/{backup}"));
    if force {
        r = r.query(&[("force", "true")]);
    }
    let got = client.send(r).await?;
    if let Some(message) = got["message"].as_str() {
        writeln!(out, "{message}")?;
    } else {
        client.wait_task(&got).await?;
        writeln!(out, "backup {backup} deleted")?;
    }
    Ok(())
}

/// `backup list`의 열
const BACKUP_HEADERS: &[&str] = &["NAME", "VM", "POOL", "PHASE", "CREATED", "SIZE"];

/// `backup list`의 한 행. VM은 만든 때의 이름이고(VM을 지웠어도 남는다), SIZE는 백업 파일 크기의 합이다.
fn backup_row(b: &Value) -> Vec<String> {
    let status = &b["status"];
    let created = status["created_at"]
        .as_i64()
        .map_or_else(|| "-".to_string(), unix_time);
    let size = match status["disks"].as_array() {
        Some(disks) if status["phase"] == "ready" => bytes(
            &disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum::<u64>()
                .into(),
        ),
        _ => "-".into(),
    };
    vec![
        cell(&b["name"]),
        cell(&b["spec"]["vm_name"]),
        cell(&b["spec"]["pool"]),
        cell(&status["phase"]),
        created,
        size,
    ]
}

async fn backup_job(
    client: &Client,
    cmd: BackupJobCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupJobCmd::Create(args) => {
            let job = client
                .post("/backup-jobs", &backup_job_create_body(&args))
                .await?;
            if args.disabled {
                writeln!(out, "backup job {} created (disabled)", args.name)?;
            } else {
                let next = job["status"]["next_run"]
                    .as_i64()
                    .map_or_else(|| "-".to_string(), unix_time);
                writeln!(out, "backup job {} created; next run {next}", args.name)?;
            }
        }
        BackupJobCmd::List => {
            let list = client.get("/backup-jobs").await?;
            print_list(out, json_out, &list, BACKUP_JOB_HEADERS, backup_job_row)?;
        }
        BackupJobCmd::Get { job } => {
            print_json(out, &client.get(&format!("/backup-jobs/{job}")).await?)?
        }
        BackupJobCmd::Set(args) => {
            // If-Match 없이 보낸다: 스케줄러가 status를 자주 써서 revision이 곧잘 바뀐다.
            let path = format!("/backup-jobs/{}", args.job);
            let current = client.get(&path).await?;
            client
                .patch(&path, &backup_job_set_body(&args, &current))
                .await?;
            writeln!(out, "backup job {} updated", args.job)?;
        }
        BackupJobCmd::Delete { job } => {
            client.delete(&format!("/backup-jobs/{job}")).await?;
            writeln!(out, "backup job {job} deleted; its backups are kept")?;
        }
        BackupJobCmd::Run { job } => {
            client
                .send(client.request(Method::POST, &format!("/backup-jobs/{job}/run")))
                .await?;
            writeln!(
                out,
                "backup job {job} will run at the scheduler's next check"
            )?;
        }
    }
    Ok(())
}

/// `backup-job list`의 열
const BACKUP_JOB_HEADERS: &[&str] = &[
    "NAME", "POOL", "VMS", "SCHEDULE", "KEEP", "ENABLED", "NEXT", "LAST",
];

/// `backup-job list`의 한 행. 시각은 모두 UTC다. 끈 작업은 다음 실행이 없다.
fn backup_job_row(j: &Value) -> Vec<String> {
    let spec = &j["spec"];
    let status = &j["status"];
    let vms = &spec["vms"];
    let vms = if vms["all"] == true {
        "all".to_string()
    } else if let Some(names) = vms["names"].as_array() {
        names.iter().map(cell).collect::<Vec<_>>().join(",")
    } else if let Some(labels) = vms["labels"].as_object() {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "-".to_string()
    };
    let days = match spec["schedule"]["days"].as_array() {
        Some(days) if !days.is_empty() => days.iter().map(cell).collect::<Vec<_>>().join(","),
        _ => "daily".to_string(),
    };
    let schedule = format!("{days} {} UTC", cell(&spec["schedule"]["at"]));
    let keep: Vec<String> = ["last", "daily", "weekly", "monthly"]
        .iter()
        .filter_map(|unit| match spec["keep"][unit].as_u64() {
            Some(n) if n > 0 => Some(format!("{unit}={n}")),
            _ => None,
        })
        .collect();
    let keep = if keep.is_empty() {
        "all".to_string()
    } else {
        keep.join(",")
    };
    let enabled = spec["enabled"] != false;
    let next = match status["next_run"].as_i64() {
        Some(t) if enabled => unix_time(t),
        _ => "-".to_string(),
    };
    let last = if !status["run"].is_null() {
        "running".to_string()
    } else {
        let r = &status["last_run"];
        match r["finished_at"].as_i64() {
            Some(t) => format!(
                "{} ({} ok, {} failed)",
                unix_time(t),
                cell(&r["ok"]),
                cell(&r["failed"])
            ),
            None => "-".to_string(),
        }
    };
    vec![
        cell(&j["name"]),
        cell(&spec["pool"]),
        vms,
        schedule,
        keep,
        enabled.to_string(),
        next,
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
        ImportCmd::EsxiBrowse { conn, fingerprint } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, fingerprint.as_deref(), &password);
            esxi_browse(client, &connection, json_out, out).await?;
        }
        ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, Some(&fingerprint), &password);
            let body = esxi_import_body(&name, &connection, &datastore, &vmx);
            esxi_create(client, &body, out).await?;
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

/// ESXi 연결 오류. 믿을 지문이 없으면 서버 인증서의 지문을 확인하고 다시 하라고 알린다.
fn esxi_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.code == "untrusted_certificate" => {
            let fingerprint = cell(&api.details["fingerprint"]);
            anyhow::anyhow!(
                "the ESXi certificate fingerprint is {fingerprint}; check it and rerun with --fingerprint {fingerprint}"
            )
        }
        _ => e,
    }
}

/// `import esxi-browse`: 데이터스토어마다 VM 표
async fn esxi_browse(
    client: &Client,
    connection: &Value,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let got = client
        .post("/vm-imports/esxi/browse", connection)
        .await
        .map_err(esxi_error)?;
    if json_out {
        return Ok(print_json(out, &got)?);
    }
    let mut rows = Vec::new();
    for ds in got["datastores"].as_array().into_iter().flatten() {
        for vm in ds["vms"].as_array().into_iter().flatten() {
            rows.push(esxi_vm_row(&ds["name"], vm));
        }
    }
    table(out, ESXI_VM_HEADERS, &rows)?;
    Ok(())
}

/// `import esxi-browse`의 열
const ESXI_VM_HEADERS: &[&str] = &[
    "DATASTORE",
    "VMX",
    "NAME",
    "GUEST",
    "CPU",
    "MEMORY",
    "DISKS",
    "LOCKED",
];

/// `import esxi-browse`의 한 행. `.vmx`를 읽지 못한 VM은 NAME 자리에 오류를 보인다.
fn esxi_vm_row(datastore: &Value, vm: &Value) -> Vec<String> {
    let (name, guest, cpu, memory, disks) = match vm["error"].as_str() {
        Some(error) => (
            format!("error: {error}"),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ),
        None => (
            cell(&vm["name"]),
            cell(&vm["guest_os"]),
            cell(&vm["vcpus"]),
            bytes(&vm["memory_mib"].as_u64().map(|m| m << 20).into()),
            cell(&vm["disks"]),
        ),
    };
    vec![
        cell(datastore),
        cell(&vm["vmx"]),
        name,
        guest,
        cpu,
        memory,
        disks,
        cell(&vm["locked"]),
    ]
}

/// `import esxi`: 가져오기를 만든다.
async fn esxi_create(
    client: &Client,
    body: &Value,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    client
        .post("/vm-imports/esxi", body)
        .await
        .map_err(esxi_error)?;
    writeln!(
        out,
        "vm-import {name} created; review it with `oplctl import get {name}`",
        name = cell(&body["name"])
    )?;
    Ok(())
}

/// ESXi 원본의 표시(`esxi://호스트/데이터스토어/.vmx`). OVA 원본이면 `None`이다.
fn esxi_source(i: &Value) -> Option<String> {
    let esxi = &i["spec"]["source"]["esxi"];
    esxi.is_object().then(|| {
        format!(
            "esxi://{}/{}/{}",
            cell(&esxi["host"]),
            cell(&esxi["datastore"]),
            cell(&esxi["vmx"])
        )
    })
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
/// ESXi 원본이면 `esxi://호스트/데이터스토어/.vmx`다.
fn import_row(i: &Value, nodes: &[Value], vms: &[Value]) -> Vec<String> {
    let status = &i["status"];
    let source = match (esxi_source(i), status["source_vm"]["name"].as_str()) {
        (Some(esxi), _) => esxi,
        (None, Some(name)) => name.to_string(),
        (None, None) => cell(&i["spec"]["source"]["ova"]["file_name"]),
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
    match esxi_source(i) {
        Some(esxi) => writeln!(out, "source: {esxi}")?,
        None => writeln!(
            out,
            "source: {} ({})",
            cell(&ova["file_name"]),
            bytes(&ova["size_bytes"])
        )?,
    }
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

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            print_cluster_status(out, &view)?;
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"],
                vm_row,
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args.nics.iter().map(nic_body).collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::NicSg { vm, index, groups } => {
            put_spec(client, &vm, |current| {
                nic_groups_update(current, index, &groups)
            })
            .await?;
            if groups.is_empty() {
                writeln!(out, "vm {vm}: NIC {index} has no security groups")?;
            } else {
                writeln!(
                    out,
                    "vm {vm}: NIC {index} security groups {}",
                    groups.join(", ")
                )?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(&format!("/vms/{vm}/migrate"), &json!({ "node": node }))
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{vm}")).await?;
                let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{vm}")).await?;
                    let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{vm}/snapshots/{}", cell(&s["id"]));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client.post(&format!("/vms/{vm}/clone"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client.post(&format!("/vms/{vm}/snapshots"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current = client.get(&format!("/vms/{vm}")).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{vm}/snapshots/{name}/revert");
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{vm}/snapshots/{name}"))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", args.vm);
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    put_spec(client, vm, |current| {
        Ok(spec_update(current, field, value.clone()))
    })
    .await
}

/// VM `vm`을 읽어 `body`(읽은 VM → 수정 본문)로 만든 본문을 그 revision으로(`If-Match`) 다시 쓴다.
async fn put_spec(
    client: &Client,
    vm: &str,
    body: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<()> {
    let path = format!("/vms/{vm}");
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body(&current)?);
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// VM 수정 본문: NIC `index`(0부터)의 보안 그룹만 `groups`로 바꾼다.
fn nic_groups_update(vm: &Value, index: usize, groups: &[String]) -> anyhow::Result<Value> {
    let mut nics = vm["spec"]["nics"].as_array().cloned().unwrap_or_default();
    let count = nics.len();
    let Some(nic) = nics.get_mut(index) else {
        bail!(
            "VM {} has no NIC {index} ({count} NICs, numbered from 0)",
            cell(&vm["name"])
        );
    };
    nic["security_groups"] = json!(groups);
    Ok(spec_update(vm, "nics", Value::Array(nics)))
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "UNDERLAY",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
        cell(&n["status"]["underlay"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `network list`의 열
const NETWORK_HEADERS: &[&str] = &["NAME", "TYPE", "BRIDGE", "VLAN/VNI", "MTU"];

/// VXLAN 네트워크의 기본 MTU(서버의 `DEFAULT_VXLAN_MTU`)
const DEFAULT_VXLAN_MTU: u64 = 1450;

/// `network list`의 한 행. VXLAN 네트워크의 브리지는 opld가 만드는 `oplv<vni>`다.
fn network_row(n: &Value) -> Vec<String> {
    let spec = &n["spec"];
    match spec["vxlan"]["vni"].as_u64() {
        Some(vni) => vec![
            cell(&n["name"]),
            "vxlan".into(),
            format!("oplv{vni}"),
            vni.to_string(),
            spec["mtu"]
                .as_u64()
                .unwrap_or(DEFAULT_VXLAN_MTU)
                .to_string(),
        ],
        None => vec![
            cell(&n["name"]),
            "bridge".into(),
            cell(&spec["bridge"]),
            cell(&spec["vlan"]),
            "-".into(),
        ],
    }
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|m| {
            vec![
                cell(&m["name"]),
                cell(&m["id"]),
                cell(&m["address"]),
                if m["voter"] == true {
                    "voter"
                } else {
                    "learner"
                }
                .into(),
                cell(&m["online"]),
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    // 백업 풀만 적는다(옛 서버와 끈 풀은 보내지 않는다).
    if pool["spec"]["backups"] == true {
        text.push_str("backups: true\n");
    }
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs, ca_pem: Option<String>) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&json!({ "username": args.user, "password": password }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool.clone(), pool_set_body(&s), pool_set_summary(&s)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }), "shared".into())
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            (
                "nas".into(),
                json!({ "shared": false }),
                "node-local".into()
            )
        );
        // 백업 표시는 따로 바꾸거나 공유 여부와 함께 바꾼다. 주지 않은 값은 보내지 않는다.
        assert_eq!(
            set(&["--backups", "true"]).unwrap(),
            (
                "nas".into(),
                json!({ "backups": true }),
                "backups on".into()
            )
        );
        assert_eq!(
            set(&["--no-shared", "--backups", "false"]).unwrap().1,
            json!({ "shared": false, "backups": false })
        );
        assert_eq!(
            set(&["--shared", "--backups", "false"]).unwrap().2,
            "shared, backups off"
        );
        // 하나는 꼭, 공유와 노드 로컬은 함께 못 준다, 백업은 true나 false
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
        assert!(set(&["--backups"]).is_err());
        assert!(set(&["--backups", "yes"]).is_err());
    }

    #[test]
    fn pool_create_marks_backup_pools() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "bk"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            body(&["--path", "/srv/bk", "--backups"]).unwrap()["spec"],
            json!({ "backend": { "kind": "dir", "path": "/srv/bk" }, "nodes": [], "backups": true })
        );
        assert_eq!(
            body(&["--nfs", "nas:/bk", "--backups"]).unwrap()["spec"]["backups"],
            json!(true)
        );
        // 끈 풀은 보내지 않는다.
        assert!(body(&["--path", "/srv/bk"]).unwrap()["spec"]["backups"].is_null());
        // 블록 풀은 백업을 담지 못한다.
        for args in [
            &["--lvm-thin", "vg0/tp", "--backups"][..],
            &["--zfs", "tank/opl", "--backups"],
            &["--rbd", "rbd", "--backups"],
            &["--lvm-shared", "san", "--backups"],
        ] {
            assert!(body(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn describes_a_backup_pool() {
        let pool = json!({
            "name": "bk",
            "spec": { "backend": { "kind": "dir", "path": "/srv/bk" }, "backups": true }
        });
        assert_eq!(
            describe_pool(&pool),
            "pool: bk\nkind: dir\nlocation: /srv/bk\nshared: -\nbackups: true\n"
        );
    }

    #[test]
    fn parses_backup_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Backup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk"]).unwrap(),
            BackupCmd::Create { vm, pool, name: None } if vm == "web" && pool == "bk"
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--name", "before-upgrade"]).unwrap(),
            BackupCmd::Create { name: Some(n), .. } if n == "before-upgrade"
        ));
        assert!(matches!(
            parse(&["list", "--vm", "web"]).unwrap(),
            BackupCmd::List { vm: Some(v) } if v == "web"
        ));
        assert!(matches!(
            parse(&["restore", "web-1", "--name", "web2", "--pool", "local", "--node", "n1"])
                .unwrap(),
            BackupCmd::Restore { backup, name, pool, node: Some(node) }
                if backup == "web-1" && name == "web2" && pool == "local" && node == "n1"
        ));
        // 백업 풀과 복원의 새 이름·풀은 꼭 준다.
        for args in [
            &["create", "web"][..],
            &["restore", "web-1", "--pool", "local"],
            &["restore", "web-1", "--name", "web2"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_list_shows_the_vm_name_and_stored_size() {
        let ready = json!({
            "name": "web-20261104-030000",
            "spec": { "vm": "x", "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "disks": [
                    { "name": "disk0", "stored_bytes": 3u64 << 30 },
                    { "name": "disk1", "stored_bytes": 1u64 << 29 }
                ]
            }
        });
        assert_eq!(
            backup_row(&ready),
            [
                "web-20261104-030000",
                "web",
                "bk",
                "ready",
                "2026-11-04 03:00 UTC",
                "3.5GiB"
            ]
        );
        // 만드는 중이거나 실패한 백업은 크기가 없다.
        let pending = json!({
            "name": "b", "spec": { "vm_name": "web", "pool": "bk" },
            "status": { "phase": "pending", "created_at": null, "disks": [] }
        });
        assert_eq!(backup_row(&pending)[3..], ["pending", "-", "-"]);
    }

    /// `backup-job create`의 인자를 요청 본문으로
    fn backup_job_create(args: &[&str]) -> Result<Value, clap::Error> {
        let mut argv = vec!["oplctl", "backup-job", "create", "nightly"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::BackupJob {
                cmd: BackupJobCmd::Create(c),
            } => backup_job_create_body(&c),
            _ => unreachable!(),
        })
    }

    #[test]
    fn backup_job_create_builds_the_spec() {
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--label",
                "tier=prod",
                "--label",
                "env=a=b",
                "--at",
                "03:00",
                "--days",
                "mon,wed",
                "--keep-last",
                "2",
                "--keep-weekly",
                "4",
            ])
            .unwrap(),
            json!({
                "name": "nightly",
                "spec": {
                    "vms": { "labels": { "tier": "prod", "env": "a=b" } },
                    "pool": "bk",
                    "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                    "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                    "enabled": true
                }
            })
        );
        // 요일을 주지 않으면 매일, 보존 규칙을 주지 않으면 모두 0(지우지 않음)
        let all =
            backup_job_create(&["--pool", "bk", "--all", "--at", "00:00", "--disabled"]).unwrap();
        assert_eq!(all["spec"]["vms"], json!({ "all": true }));
        assert_eq!(
            all["spec"]["schedule"],
            json!({ "at": "00:00", "days": [] })
        );
        assert_eq!(
            all["spec"]["keep"],
            json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 })
        );
        assert_eq!(all["spec"]["enabled"], json!(false));
        assert_eq!(
            backup_job_create(&["--pool", "bk", "--vm", "web", "--vm", "db", "--at", "03:00"])
                .unwrap()["spec"]["vms"],
            json!({ "names": ["web", "db"] })
        );
        // VM 고르기는 셋 중 꼭 하나, 풀과 시각은 꼭, 라벨은 KEY=VALUE, 요일은 mon..sun
        for args in [
            &["--pool", "bk", "--at", "03:00"][..],
            &["--pool", "bk", "--all", "--vm", "web", "--at", "03:00"],
            &[
                "--pool", "bk", "--vm", "web", "--label", "a=b", "--at", "03:00",
            ],
            &["--all", "--at", "03:00"],
            &["--pool", "bk", "--all"],
            &["--pool", "bk", "--label", "tier", "--at", "03:00"],
            &["--pool", "bk", "--label", "=prod", "--at", "03:00"],
            &["--pool", "bk", "--all", "--at", "03:00", "--days", "monday"],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--keep-last",
                "-1",
            ],
        ] {
            assert!(backup_job_create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_set_changes_only_what_is_given() {
        let current = json!({
            "name": "nightly",
            "revision": 9,
            "spec": {
                "vms": { "all": true }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["sat"] },
                "keep": { "last": 2, "daily": 7, "weekly": 0, "monthly": 0 },
                "enabled": true
            }
        });
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup-job", "set", "nightly"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::BackupJob {
                    cmd: BackupJobCmd::Set(s),
                } => backup_job_set_body(&s, &current),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--enabled", "false"]).unwrap(),
            json!({ "enabled": false })
        );
        assert_eq!(
            set(&["--vm", "web", "--pool", "bk2"]).unwrap(),
            json!({ "vms": { "names": ["web"] }, "pool": "bk2" })
        );
        // 일정과 보존 규칙은 통째로 바뀌므로 주지 않은 부분은 지금 값을 보낸다.
        assert_eq!(
            set(&["--at", "04:30"]).unwrap(),
            json!({ "schedule": { "at": "04:30", "days": ["sat"] } })
        );
        assert_eq!(
            set(&["--days", "sun,mon"]).unwrap(),
            json!({ "schedule": { "at": "03:00", "days": ["sun", "mon"] } })
        );
        assert_eq!(
            set(&["--keep-monthly", "6", "--keep-last", "1"]).unwrap(),
            json!({ "keep": { "last": 1, "daily": 7, "weekly": 0, "monthly": 6 } })
        );
        // 바꿀 것은 꼭 하나, VM 고르기는 하나만, 사용 여부는 true나 false
        for args in [
            &[][..],
            &["--all", "--vm", "web"],
            &["--enabled"],
            &["--enabled", "yes"],
        ] {
            assert!(set(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_list_shows_the_schedule_and_runs() {
        let job = json!({
            "name": "nightly",
            "spec": {
                "vms": { "labels": { "env": "a", "tier": "prod" } }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                "enabled": true
            },
            "status": {
                "next_run": 1_793_761_200, "run": null,
                "last_run": { "started_at": 1_793_674_800, "finished_at": 1_793_675_100, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "db", "error": "boom" }]
            }
        });
        assert_eq!(
            backup_job_row(&job),
            [
                "nightly",
                "bk",
                "env=a,tier=prod",
                "mon,wed 03:00 UTC",
                "last=2,weekly=4",
                "true",
                "2026-11-04 03:00 UTC",
                "2026-11-03 03:05 UTC (3 ok, 1 failed)"
            ]
        );
        // 매일, 보존 규칙 없음, 진행 중
        let mut running = job.clone();
        running["spec"]["vms"] = json!({ "names": ["web", "db"] });
        running["spec"]["schedule"]["days"] = json!([]);
        running["spec"]["keep"] = json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 });
        running["status"]["run"] = json!({ "started_at": 1, "queue": [], "backups": [] });
        assert_eq!(
            backup_job_row(&running)[2..],
            [
                "web,db",
                "daily 03:00 UTC",
                "all",
                "true",
                "2026-11-04 03:00 UTC",
                "running"
            ]
        );
        // 끈 작업은 다음 실행이 없고, 한 번도 돌지 않았으면 마지막 실행도 없다.
        let mut off = job.clone();
        off["spec"]["vms"] = json!({ "all": true });
        off["spec"]["enabled"] = json!(false);
        off["status"]["last_run"] = Value::Null;
        assert_eq!(backup_job_row(&off)[2], "all");
        assert_eq!(backup_job_row(&off)[5..], ["false", "-", "-"]);
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 브리지와 VXLAN 중 정확히 하나. VLAN은 브리지와, MTU는 VXLAN과만. 주지 않은 값은 보내지 않는다.
    #[test]
    fn parses_network_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "network", "create", "lan"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Network {
                    cmd: NetworkCmd::Create(c),
                } => network_create_body(&c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--bridge", "vmbr0"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } })
        );
        assert_eq!(
            create(&["--bridge", "vmbr0", "--vlan", "10"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0", "vlan": 10 } })
        );
        assert_eq!(
            create(&["--vxlan", "100"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 } } })
        );
        assert_eq!(
            create(&["--vxlan", "100", "--mtu", "1400"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 }, "mtu": 1400 } })
        );
        for args in [
            &[][..],
            &["--bridge", "vmbr0", "--vxlan", "100"],
            &["--vxlan", "100", "--vlan", "10"],
            &["--bridge", "vmbr0", "--mtu", "1400"],
            &["--vlan", "10"],
            &["--mtu", "1400"],
            &["--vxlan", "-1"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 종류, 붙는 브리지(VXLAN은 관리 이름), VLAN이나 VNI, VXLAN MTU(없으면 기본값)
    #[test]
    fn network_list_shows_kind_bridge_and_mtu() {
        let rows: Vec<_> = [
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
            json!({ "name": "dmz", "spec": { "bridge": "vmbr0", "vlan": 20 } }),
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 100 } } }),
            json!({ "name": "jumbo", "spec": { "vxlan": { "vni": 7 }, "mtu": 8950 } }),
        ]
        .iter()
        .map(network_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NETWORK_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   TYPE    BRIDGE   VLAN/VNI  MTU\n\
             lan    bridge  vmbr0    -         -\n\
             dmz    bridge  vmbr0    20        -\n\
             ov     vxlan   oplv100  100       1450\n\
             jumbo  vxlan   oplv7    7         8950\n"
        );
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 받은 요청 줄을 `seen`에 남기고 경로에 맞는 JSON을 200으로 돌려주는 작은 HTTP 서버
    async fn fake_api(
        reply: fn(&str) -> Value,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        fake_api_status(move |path| (200, reply(path))).await
    }

    /// `fake_api`와 같고 응답 상태 코드도 정한다.
    async fn fake_api_status(
        reply: impl Fn(&str) -> (u16, Value) + Send + 'static,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(line);
                let (status, body) = reply(&path);
                let body = body.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        (client, seen)
    }

    /// `backup delete --force`는 `?force=true`로 보내고, 기록만 지웠으면 서버의 알림을 보인다.
    /// 아니면 Task를 기다린다.
    #[tokio::test]
    async fn backup_delete_force_shows_the_server_message() {
        let cli = Cli::try_parse_from(["oplctl", "backup", "delete", "b1", "--force"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                cmd: BackupCmd::Delete { force: true, .. }
            }
        ));

        let (client, seen) =
            fake_api(|_| json!({ "message": "backup \"b1\" deleted; its files are left" })).await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", true, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/backups/b1?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup \"b1\" deleted; its files are left\n"
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
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/backups/b1 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");
    }

    /// `backup-job set`은 작업을 읽어 그 값에 맞춰 PATCH하고, `run`은 본문 없이 POST한다.
    #[tokio::test]
    async fn backup_job_set_reads_then_patches_and_run_posts() {
        let (client, seen) = fake_api(|_| {
            json!({
                "name": "nightly", "revision": 3,
                "spec": { "schedule": { "at": "03:00", "days": [] }, "keep": {} },
                "status": {}
            })
        })
        .await;
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "set", "nightly", "--at", "04:00"])
            .unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        let mut out = Vec::new();
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "run", "nightly"]).unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/backup-jobs/nightly HTTP/1.1",
                "PATCH /api/v1/backup-jobs/nightly HTTP/1.1",
                "POST /api/v1/backup-jobs/nightly/run HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup job nightly updated\nbackup job nightly will run at the scheduler's next check\n"
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        a["status"]["underlay"] = json!("10.0.0.1");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        // underlay가 없는 노드(위트니스, 옛 서버)는 `-`다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -         true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 방향과 동작은 꼭, 프로토콜·포트·CIDR은 이 차례로 생략할 수 있다.
    #[test]
    fn parses_rules() {
        assert_eq!(
            parse_rule("in accept tcp 22 10.0.0.0/8"),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "tcp",
                "ports": "22", "remote": "10.0.0.0/8"
            }))
        );
        assert_eq!(
            parse_rule("out drop tcp 25"),
            Ok(json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }))
        );
        assert_eq!(
            parse_rule("in accept icmp"),
            Ok(json!({ "direction": "in", "action": "accept", "protocol": "icmp" }))
        );
        assert_eq!(
            parse_rule("  in   accept udp 8000-8080 "),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "udp", "ports": "8000-8080"
            }))
        );
        assert_eq!(
            parse_rule("out accept 2001:db8::/32"),
            Ok(json!({ "direction": "out", "action": "accept", "remote": "2001:db8::/32" }))
        );
        assert_eq!(
            parse_rule("in drop"),
            Ok(json!({ "direction": "in", "action": "drop" }))
        );
        // 포트 범위와 프로토콜·포트의 짝은 서버가 본다.
        assert_eq!(
            parse_rule("in accept 99999"),
            Ok(json!({ "direction": "in", "action": "accept", "ports": "99999" }))
        );
        for bad in [
            "",
            "in",
            "accept in",
            "up accept",
            "in allow",
            "in accept sctp",
            "in accept tcp 22 10.0.0.0/8 extra",
            "in accept 10.0.0.0/8 tcp",
            "in accept tcp 10.0.0.0/8 22",
            "in accept tcp ssh",
            "in accept tcp 22-",
            "in accept tcp -22",
            "IN accept",
        ] {
            let err = parse_rule(bad).unwrap_err();
            assert!(err.contains("in|out accept|drop"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_security_group_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "security-group"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::SecurityGroup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&[
            "create",
            "web",
            "--description",
            "web servers",
            "--rule",
            "in accept tcp 443",
            "--rule",
            "in accept icmp",
        ])
        .unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [
                    { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" },
                    { "direction": "in", "action": "accept", "protocol": "icmp" },
                ]
            }})
        );
        // 설명이 없으면 보내지 않는다. 규칙 없는 그룹도 된다.
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&["create", "empty"]).unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "empty", "spec": { "rules": [] } })
        );
        assert!(parse(&["create", "web", "--rule", "in maybe"]).is_err());
        assert!(matches!(
            parse(&["rule", "add", "web", "out drop tcp 25", "--at", "2"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Add { group, rule, at: Some(2) } }
                if group == "web" && rule["ports"] == "25"
        ));
        assert!(matches!(
            parse(&["rule", "rm", "web", "3"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Rm { group, n: 3 } } if group == "web"
        ));
        assert!(parse(&["rule", "add", "web", "sideways accept"]).is_err());
        assert!(parse(&["rule", "rm", "web", "first"]).is_err());
        assert!(matches!(
            parse(&["get", "web"]).unwrap(),
            SecurityGroupCmd::Get { group } if group == "web"
        ));
        assert!(matches!(
            parse(&["delete", "web"]).unwrap(),
            SecurityGroupCmd::Delete { group } if group == "web"
        ));
    }

    /// 규칙 번호는 1부터다. 범위 밖이면 아무것도 바꾸지 않고 실패한다.
    #[test]
    fn adds_and_removes_rules_by_number() {
        let rule =
            |n: u16| json!({ "direction": "in", "action": "accept", "ports": n.to_string() });
        let group = json!({ "name": "web", "spec": { "rules": [rule(1), rule(2)] } });
        assert_eq!(
            rules_with(&group, rule(9), None).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(1)).unwrap(),
            vec![rule(9), rule(1), rule(2)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(3)).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        for at in [0, 4] {
            let err = rules_with(&group, rule(9), Some(at))
                .unwrap_err()
                .to_string();
            assert!(err.contains("use 1 to 3"), "{err}");
        }
        assert_eq!(rules_without(&group, 1).unwrap(), vec![rule(2)]);
        assert_eq!(rules_without(&group, 2).unwrap(), vec![rule(1)]);
        for n in [0, 3] {
            let err = rules_without(&group, n).unwrap_err().to_string();
            assert!(err.contains(&format!("has no rule {n}")), "{err}");
        }
        // 규칙이 없는 그룹은 응답에 rules가 비어 있다.
        let empty = json!({ "name": "e", "spec": { "rules": [] } });
        assert_eq!(rules_with(&empty, rule(1), None).unwrap(), vec![rule(1)]);
        assert!(rules_without(&empty, 1).is_err());
    }

    #[test]
    fn security_group_list_shows_rule_counts() {
        let rows: Vec<_> = [
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [{ "direction": "in", "action": "accept" }, { "direction": "out", "action": "drop" }]
            }}),
            json!({ "name": "empty", "spec": { "rules": [] } }),
        ]
        .iter()
        .map(security_group_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, SECURITY_GROUP_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   RULES  DESCRIPTION\n\
             web    2      web servers\n\
             empty  0      -\n"
        );
    }

    /// `--nic NETWORK[,sg=A+B]`. 그룹이 없으면 `security_groups`를 보내지 않는다.
    #[test]
    fn parses_nics_with_security_groups() {
        let nics = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "create", "web"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Vm {
                    cmd: VmCmd::Create(c),
                } => c.nics.iter().map(nic_body).collect::<Vec<_>>(),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            nics(&[
                "--nic",
                "user",
                "--nic",
                "lan,sg=web+db",
                "--nic",
                "dmz,sg=web"
            ])
            .unwrap(),
            vec![
                json!({ "network": "user", "model": "virtio" }),
                json!({ "network": "lan", "model": "virtio", "security_groups": ["web", "db"] }),
                json!({ "network": "dmz", "model": "virtio", "security_groups": ["web"] }),
            ]
        );
        for bad in [
            "",
            ",sg=web",
            "lan,",
            "lan,sg=",
            "lan,sg=web+",
            "lan,sg=+web",
            "lan,group=web",
            "lan,sg=web,sg=db",
        ] {
            assert!(nics(&["--nic", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_vm_nic_sg() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "1", "a", "b"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { vm, index: 1, groups } }
                if vm == "web" && groups == ["a", "b"]
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "0"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { index: 0, groups, .. } } if groups.is_empty()
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "-1"]).is_err());
    }

    /// 그 NIC의 그룹만 바꾸고 나머지(다른 NIC, 다른 필드, 라벨)는 그대로 둔다.
    #[test]
    fn nic_groups_update_changes_one_nic() {
        let vm = json!({
            "name": "web",
            "revision": 4,
            "labels": { "team": "a" },
            "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["old"] },
                ],
            },
        });
        let groups = ["web".to_string(), "db".to_string()];
        assert_eq!(
            nic_groups_update(&vm, 1, &groups).unwrap(),
            json!({ "labels": { "team": "a" }, "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["web", "db"] },
                ],
            }})
        );
        assert_eq!(
            nic_groups_update(&vm, 1, &[]).unwrap()["spec"]["nics"][1]["security_groups"],
            json!([])
        );
        let err = nic_groups_update(&vm, 2, &groups).unwrap_err().to_string();
        assert_eq!(err, "VM web has no NIC 2 (2 NICs, numbered from 0)");
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
    fn parses_import_esxi_commands() {
        let ImportCmd::EsxiBrowse { conn, fingerprint } =
            parse_import(&["esxi-browse", "--host", "esx1", "--user", "root"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fingerprint, None);
        // 포트와 데이터센터는 ESXi 기본값이다.
        assert_eq!(
            esxi_connection(&conn, fingerprint.as_deref(), "secret"),
            json!({
                "host": "esx1", "port": 443, "user": "root", "password": "secret",
                "datacenter": "ha-datacenter", "fingerprint": null
            })
        );
        let ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } = parse_import(&[
            "esxi",
            "web01",
            "--host",
            "vc1",
            "--port",
            "8443",
            "--user",
            "admin@vsphere.local",
            "--datacenter",
            "DC1",
            "--datastore",
            "datastore1",
            "--vmx",
            "web01/web01.vmx",
            "--fingerprint",
            "AB:CD",
        ])
        .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            esxi_import_body(
                &name,
                &esxi_connection(&conn, Some(&fingerprint), "pw"),
                &datastore,
                &vmx
            ),
            json!({
                "name": "web01",
                "connection": {
                    "host": "vc1", "port": 8443, "user": "admin@vsphere.local", "password": "pw",
                    "datacenter": "DC1", "fingerprint": "AB:CD"
                },
                "datastore": "datastore1",
                "vmx": "web01/web01.vmx"
            })
        );
        // 만들기에는 지문·데이터스토어·.vmx가 꼭 있어야 하고, 비밀번호는 인자로 받지 않는다.
        let full = [
            "esxi",
            "web01",
            "--host",
            "esx1",
            "--user",
            "root",
            "--datastore",
            "ds",
            "--vmx",
            "a/a.vmx",
            "--fingerprint",
            "AB",
        ];
        assert!(parse_import(&full).is_ok());
        for skip in ["--datastore", "--vmx", "--fingerprint", "--host", "--user"] {
            let i = full.iter().position(|a| *a == skip).unwrap();
            let args: Vec<&str> = full[..i].iter().chain(&full[i + 2..]).copied().collect();
            assert!(parse_import(&args).is_err(), "{args:?}");
        }
        let mut with_password = full.to_vec();
        with_password.extend(["--password", "x"]);
        assert!(parse_import(&with_password).is_err());
        assert!(parse_import(&["esxi-browse", "--host", "esx1"]).is_err());

        // 비밀번호 파일(끝의 줄바꿈은 뺀다)
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("esxi-password");
        std::fs::write(&file, "VMware1!\n").unwrap();
        let mut with_file = full.to_vec();
        with_file.extend(["--password-file", file.to_str().unwrap()]);
        let ImportCmd::Esxi { conn, .. } = parse_import(&with_file).unwrap() else {
            unreachable!()
        };
        assert_eq!(esxi_password(&conn).unwrap(), "VMware1!");
    }

    fn sample_browse() -> Value {
        json!({
            "datastores": [
                {
                    "name": "datastore1",
                    "vms": [
                        {
                            "vmx": "web01/web01.vmx", "name": "web01", "guest_os": "ubuntu-64",
                            "vcpus": 2, "memory_mib": 1024, "disks": 1, "locked": false, "error": null
                        },
                        {
                            "vmx": "db/db.vmx", "name": "db", "guest_os": "", "vcpus": 0,
                            "memory_mib": 0, "disks": 0, "locked": true,
                            "error": "memsize is missing"
                        }
                    ]
                },
                { "name": "empty", "vms": [] }
            ]
        })
    }

    /// `import esxi-browse`는 연결을 보내고 데이터스토어의 VM을 표로 보인다. 읽지 못한 VM은 NAME
    /// 자리에 오류를 보인다.
    #[tokio::test]
    async fn import_esxi_browse_shows_the_vms() {
        let (client, seen) = fake_api(|_| sample_browse()).await;
        let mut out = Vec::new();
        let conn = json!({ "host": "esx1" });
        esxi_browse(&client, &conn, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi/browse HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
DATASTORE   VMX              NAME                       GUEST      CPU  MEMORY  DISKS  LOCKED
datastore1  web01/web01.vmx  web01                      ubuntu-64  2    1.0GiB  1      false
datastore1  db/db.vmx        error: memsize is missing  -          -    -       -      true
"
        );
        // --json이면 응답 그대로
        let mut out = Vec::new();
        esxi_browse(&client, &conn, true, &mut out).await.unwrap();
        let printed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed, sample_browse());
    }

    /// 지문을 주지 않았거나(409 `untrusted_certificate`) 틀리면 서버 인증서의 지문으로 다시 하라고 알린다.
    #[tokio::test]
    async fn import_esxi_explains_an_untrusted_certificate() {
        let (client, _) = fake_api_status(|_| {
            (
                409,
                json!({
                    "code": "untrusted_certificate",
                    "message": "verify the ESXi certificate fingerprint",
                    "details": { "fingerprint": "AB:CD:EF" }
                }),
            )
        })
        .await;
        let conn = json!({ "host": "esx1" });
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        let body = esxi_import_body("web01", &conn, "ds", "a/a.vmx");
        let err = esxi_create(&client, &body, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        // 다른 오류는 서버의 메시지와 코드 그대로
        let (client, _) = fake_api_status(|_| {
            (
                400,
                json!({ "code": "esxi_auth", "message": "ESXi rejected the user name or password" }),
            )
        })
        .await;
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ESXi rejected the user name or password (esxi_auth)"
        );
    }

    /// `import esxi`는 가져오기를 만들고 검토 명령을 알린다.
    #[tokio::test]
    async fn import_esxi_creates_the_import() {
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let mut out = Vec::new();
        let body = esxi_import_body("web01", &json!({}), "ds", "a/a.vmx");
        esxi_create(&client, &body, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 created; review it with `oplctl import get web01`\n"
        );
    }

    fn sample_esxi_import() -> Value {
        let mut i = sample_import();
        i["spec"]["source"] = json!({ "esxi": {
            "host": "esx1.example.com", "port": 443, "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": "root",
            "fingerprint": "AB:CD"
        } });
        i
    }

    /// ESXi에서 가져오면 SOURCE는 `esxi://호스트/데이터스토어/.vmx`다.
    #[test]
    fn import_list_and_get_show_an_esxi_source() {
        let url = "esxi://esx1.example.com/datastore1/web01/web01.vmx";
        assert_eq!(import_row(&sample_esxi_import(), &[], &[])[2], url);
        let mut out = Vec::new();
        print_import(&mut out, &sample_esxi_import(), &[], &[]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!(
                "vm-import: web01\nnode: n-1\nsource: {url}\nphase: ready\n"
            )),
            "{text}"
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`crates/oplctl/src/client.rs`에서

```rust
const CA_TIMEOUT: Duration = Duration::from_secs(30);
```

을 다음으로 바꾼다:

```rust
const CA_TIMEOUT: Duration = Duration::from_secs(30);

/// API 오류 응답. 보일 때는 `메시지 (코드)`이고, 명령이 코드와 `details`를 보고 더 알릴 수 있다.
#[derive(Debug)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for ApiError {}
```

`crates/oplctl/src/client.rs`에서

```rust
        match (body["message"].as_str(), body["code"].as_str()) {
            (Some(message), Some(code)) => bail!("{message} ({code})"),
            _ => bail!("{status}: {body}"),
```

을 다음으로 바꾼다:

```rust
        match (body["message"].as_str(), body["code"].as_str()) {
            (Some(message), Some(code)) => Err(ApiError {
                code: code.into(),
                message: message.into(),
                details: body["details"].clone(),
            }
            .into()),
            _ => bail!("{status}: {body}"),
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1130개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): ESXi에서 가져오기 명령 — import esxi-browse(--host·--user·--port 443·--datacenter ha-datacenter·--fingerprint, 비밀번호는 OPL_ESXI_PASSWORD 또는 터미널 프롬프트, 표 DATASTORE VMX NAME GUEST CPU MEMORY DISKS LOCKED와 읽지 못한 VM은 NAME 자리에 오류)·import esxi NAME(--datastore·--vmx·--fingerprint 필수), 지문이 없으면(409 untrusted_certificate) 서버 인증서 지문으로 다시 하라는 안내, import list·get의 SOURCE는 ESXi면 esxi://호스트/데이터스토어/.vmx, API 오류를 코드·details가 있는 ApiError로"
```

---

### Task 6: 웹 ESXi 가져오기 대화상자

`/imports`의 "ESXi에서 가져오기"(쓰기 권한): 연결(호스트·포트·사용자·비밀번호·데이터센터) → 서버가 알려 준 지문을 보여 주고 신뢰(뒤로 가능, 같은 host:port로 다시 연결하면 그 지문을 함께 보냄) → VM 목록(잠김 경고, 오류 행은 고를 수 없음, 뒤로) → 이름 → 만들기 → 가져오기 화면. 인증·연결 오류는 연결 단계로, 만들기 때 지문이 바뀌었으면 처음부터. 비밀번호는 컴포넌트 상태에만. 목록·상세의 ESXi 원본 표시, 자격 증명이 사라진 안내, 상세 화면 문구를 원본 종류와 무관하게, en·ko.

**Files:**
- Create: `web/src/imports/EsxiImportDialog.test.tsx`
- Create: `web/src/imports/EsxiImportDialog.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/imports/ImportPage.test.tsx`
- Modify: `web/src/imports/ImportPage.tsx`
- Modify: `web/src/imports/ImportsPage.test.tsx`
- Modify: `web/src/imports/ImportsPage.tsx`
- Modify: `web/src/imports/imports.test.ts`
- Modify: `web/src/imports/imports.ts`
- Modify: `web/src/test/fixtures.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json`에서

```json
    "import_done": "The VM has already been imported.",
    "import_failed": "The import cannot run: {{message}}"
  },
```

을 다음으로 바꾼다:

```json
    "import_done": "The VM has already been imported.",
    "import_failed": "The import cannot run: {{message}}",
    "untrusted_certificate": "Check the ESXi certificate fingerprint and trust it to continue.",
    "fingerprint_mismatch": "The ESXi certificate changed since you trusted it. Connect again and check the new fingerprint.",
    "esxi_auth": "ESXi rejected the user name or password. Check them and try again.",
    "esxi_unreachable": "Cannot reach the ESXi host ({{message}})."
  },
```

`web/src/i18n/en.json`에서

```json
  "imports": {
    "pageHint": "Bring VMware VMs into opLite from OVA files. Upload an OVA, review the settings proposed from the source VM, then run the import to create a stopped VM.",
    "empty": "No imports.",
```

을 다음으로 바꾼다:

```json
  "imports": {
    "pageHint": "Bring VMware VMs into opLite from OVA files or straight from ESXi or vCenter. Review the settings proposed from the source VM, then run the import to create a stopped VM.",
    "empty": "No imports.",
```

`web/src/i18n/en.json`에서

```json
    "pool": "Storage pool for the disks",
    "poolHint": "Only pools usable on {{node}}, where the OVA is, are listed.",
    "sockets": "Sockets",
```

을 다음으로 바꾼다:

```json
    "pool": "Storage pool for the disks",
    "poolHint": "Only pools usable on {{node}}, where the import runs, are listed.",
    "sockets": "Sockets",
```

`web/src/i18n/en.json`에서

```json
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
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "Delete import {{name}}?",
    "deleteDescription": "Deletes the import and its files on {{node}}. The source VM and any VM already imported are kept.",
    "forceHint": "{{error}} To forget the import and leave its files on that node, run oplctl import delete {{name}} --force.",
    "codes": {
      "node_offline": "The node running the import is offline or has left the cluster.",
      "mac_in_use": "Another VM uses a source MAC address. Turn off \"Keep the source MAC addresses\" to give the interfaces new MAC addresses."
    },
    "errors": {
      "reserved": "The names ova and esxi are reserved.",
      "exists": "An import with this name already exists.",
```

`web/src/i18n/en.json`에서

```json
      "network": "Choose a network.",
      "poolNotOnNode": "This storage pool is not available on the node holding the OVA.",
      "poolUnavailable": "This storage pool is not ready on the node holding the OVA.",
      "sockets": "Enter 1 to 65535 sockets.",
      "nameTaken": "Name in use: {{message}}"
    }
  },
```

을 다음으로 바꾼다:

```json
      "network": "Choose a network.",
      "poolNotOnNode": "This storage pool is not available on the node running the import.",
      "poolUnavailable": "This storage pool is not ready on the node running the import.",
      "sockets": "Enter 1 to 65535 sockets.",
      "nameTaken": "Name in use: {{message}}",
      "host": "Enter the ESXi or vCenter host.",
      "port": "Enter a port from 1 to 65535.",
      "user": "Enter the user name.",
      "datacenter": "Enter the datacenter.",
      "vm": "Choose a VM."
    },
    "esxi": "Import from ESXi",
    "esxiTitle": "Import from ESXi",
    "esxiDescription": "The connection is made from the node you are connected to ({{node}}), and the import runs there. The password stays only in that node's memory: if opld restarts there, create the import again.",
    "host": "Host",
    "port": "Port",
    "user": "User",
    "password": "Password",
    "datacenter": "Datacenter",
    "datacenterHint": "ha-datacenter for an ESXi host; for vCenter, the datacenter name.",
    "connect": "Connect",
    "untrusted": "{{host}} presented a certificate that is not trusted yet. Check that this SHA-256 fingerprint matches the certificate of the ESXi host before you continue.",
    "fingerprint": "Certificate fingerprint (SHA-256)",
    "trust": "Trust and continue",
    "vms": "Virtual machines",
    "noVms": "No VMs were found in the top-level folders of the datastores.",
    "datastore": "Datastore",
    "locked": "The VM looks powered on (lock files found). Power it off before importing.",
    "next": "Next",
    "back": "Back",
    "chosenVm": "Source",
    "esxiSource": "ESXi",
    "diskFileEsxi": "File on the datastore",
    "credentialsLost": "The ESXi password is no longer on this node (opld restarted or the request was interrupted). Delete this import and create it again."
  },
```

`web/src/i18n/ko.json`에서

```json
    "import_done": "이미 가져온 VM입니다.",
    "import_failed": "가져오기를 실행할 수 없습니다: {{message}}"
  },
```

을 다음으로 바꾼다:

```json
    "import_done": "이미 가져온 VM입니다.",
    "import_failed": "가져오기를 실행할 수 없습니다: {{message}}",
    "untrusted_certificate": "ESXi 인증서 지문을 확인하고 믿기로 해야 계속할 수 있습니다.",
    "fingerprint_mismatch": "믿기로 한 뒤에 ESXi 인증서가 바뀌었습니다. 다시 연결해 새 지문을 확인하세요.",
    "esxi_auth": "ESXi가 사용자 이름이나 비밀번호를 거절했습니다. 확인하고 다시 시도하세요.",
    "esxi_unreachable": "ESXi 호스트에 연결할 수 없습니다({{message}})."
  },
```

`web/src/i18n/ko.json`에서

```json
  "imports": {
    "pageHint": "OVA 파일로 VMware VM을 opLite로 가져옵니다. OVA를 올리고 원본 VM에서 제안한 설정을 확인한 뒤 실행하면 꺼진 VM이 만들어집니다.",
    "empty": "가져오기가 없습니다.",
```

을 다음으로 바꾼다:

```json
  "imports": {
    "pageHint": "OVA 파일이나 ESXi·vCenter에서 바로 VMware VM을 opLite로 가져옵니다. 원본 VM에서 제안한 설정을 확인한 뒤 실행하면 꺼진 VM이 만들어집니다.",
    "empty": "가져오기가 없습니다.",
```

`web/src/i18n/ko.json`에서

```json
    "pool": "디스크를 둘 스토리지 풀",
    "poolHint": "OVA가 있는 {{node}}에서 쓸 수 있는 풀만 보입니다.",
    "sockets": "소켓",
```

을 다음으로 바꾼다:

```json
    "pool": "디스크를 둘 스토리지 풀",
    "poolHint": "가져오기가 도는 {{node}}에서 쓸 수 있는 풀만 보입니다.",
    "sockets": "소켓",
```

`web/src/i18n/ko.json`에서

```json
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
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "가져오기 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "{{node}}에 있는 가져오기 파일을 지웁니다. 원본 VM과 이미 가져온 VM은 남습니다.",
    "forceHint": "{{error}} 파일을 그 노드에 남기고 가져오기만 지우려면 oplctl import delete {{name}} --force를 실행하세요.",
    "codes": {
      "node_offline": "가져오기를 하는 노드가 오프라인이거나 클러스터에서 빠졌습니다.",
      "mac_in_use": "다른 VM이 원본 MAC 주소를 쓰고 있습니다. \"원본 MAC 주소 유지\"를 끄면 새 MAC 주소를 받습니다."
    },
    "errors": {
      "reserved": "ova와 esxi는 예약된 이름입니다.",
      "exists": "같은 이름의 가져오기가 이미 있습니다.",
```

`web/src/i18n/ko.json`에서

```json
      "network": "네트워크를 고르세요.",
      "poolNotOnNode": "OVA가 있는 노드에서 쓸 수 없는 스토리지 풀입니다.",
      "poolUnavailable": "OVA가 있는 노드에서 준비되지 않은 스토리지 풀입니다.",
      "sockets": "소켓 수는 1에서 65535 사이여야 합니다.",
      "nameTaken": "이미 쓰는 이름입니다: {{message}}"
    }
  },
```

을 다음으로 바꾼다:

```json
      "network": "네트워크를 고르세요.",
      "poolNotOnNode": "가져오기를 하는 노드에서 쓸 수 없는 스토리지 풀입니다.",
      "poolUnavailable": "가져오기를 하는 노드에서 준비되지 않은 스토리지 풀입니다.",
      "sockets": "소켓 수는 1에서 65535 사이여야 합니다.",
      "nameTaken": "이미 쓰는 이름입니다: {{message}}",
      "host": "ESXi나 vCenter 호스트를 입력하세요.",
      "port": "포트는 1에서 65535 사이여야 합니다.",
      "user": "사용자 이름을 입력하세요.",
      "datacenter": "데이터센터를 입력하세요.",
      "vm": "VM을 고르세요."
    },
    "esxi": "ESXi에서 가져오기",
    "esxiTitle": "ESXi에서 가져오기",
    "esxiDescription": "지금 접속한 노드({{node}})에서 연결하고 가져오기도 그 노드에서 돕니다. 비밀번호는 그 노드의 메모리에만 있으므로 그곳의 opld가 다시 시작하면 가져오기를 다시 만들어야 합니다.",
    "host": "호스트",
    "port": "포트",
    "user": "사용자",
    "password": "비밀번호",
    "datacenter": "데이터센터",
    "datacenterHint": "ESXi 호스트는 ha-datacenter, vCenter는 데이터센터 이름입니다.",
    "connect": "연결",
    "untrusted": "{{host}}의 인증서는 아직 믿지 않은 인증서입니다. 계속하기 전에 이 SHA-256 지문이 ESXi 호스트의 인증서와 같은지 확인하세요.",
    "fingerprint": "인증서 지문(SHA-256)",
    "trust": "이 지문을 믿고 계속",
    "vms": "가상 머신",
    "noVms": "데이터스토어의 맨 위 폴더에서 VM을 찾지 못했습니다.",
    "datastore": "데이터스토어",
    "locked": "VM이 켜져 있는 것 같습니다(잠금 파일이 있음). 가져오기 전에 끄세요.",
    "next": "다음",
    "back": "뒤로",
    "chosenVm": "원본",
    "esxiSource": "ESXi",
    "diskFileEsxi": "데이터스토어의 파일",
    "credentialsLost": "ESXi 비밀번호가 이제 이 노드에 없습니다(opld가 다시 시작했거나 요청이 끊겼습니다). 이 가져오기를 지우고 다시 만드세요."
  },
```

`web/src/imports/EsxiImportDialog.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ImportsPage } from "@/imports/ImportsPage";
import { esxiImport, FINGERPRINT, NODE, vmImport } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const PASSWORD = "s3cret-Pa55";

const BROWSE = {
  datastores: [
    {
      name: "datastore1",
      vms: [
        {
          vmx: "web01/web01.vmx",
          name: "Web Server 01",
          guest_os: "ubuntu-64",
          vcpus: 2,
          memory_mib: 2048,
          disks: 1,
          locked: true,
          error: null,
        },
        {
          vmx: "broken/broken.vmx",
          name: "",
          guest_os: "",
          vcpus: 0,
          memory_mib: 0,
          disks: 0,
          locked: false,
          error: "ethernet0 has no networkName",
        },
      ],
    },
    {
      name: "datastore2",
      vms: [
        {
          vmx: "db/db.vmx",
          name: "db",
          guest_os: "windows2019srv-64",
          vcpus: 4,
          memory_mib: 8192,
          disks: 2,
          locked: false,
          error: null,
        },
      ],
    },
  ],
};

/**
 * 요청을 모두 기록하는 가짜 서버. 실제 서버처럼 browse는 지문이 없으면 409 `untrusted_certificate`, 비밀번호가
 * 틀리면 400 `esxi_auth`다. `create`는 만들기 응답을 바꾼다.
 */
function mockEsxi(create?: () => Response) {
  const requests: { url: string; body: unknown }[] = [];
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
    http.post(apiUrl("/vm-imports/esxi/browse"), async ({ request }) => {
      const body = (await request.json()) as { fingerprint?: string; password: string };
      requests.push({ url: request.url, body });
      if (!body.fingerprint) {
        return HttpResponse.json(
          {
            code: "untrusted_certificate",
            message: "verify the ESXi certificate fingerprint",
            details: { fingerprint: FINGERPRINT },
          },
          { status: 409 },
        );
      }
      if (body.password !== PASSWORD) {
        return HttpResponse.json(
          { code: "esxi_auth", message: "ESXi rejected the user name or password" },
          { status: 400 },
        );
      }
      return HttpResponse.json(BROWSE);
    }),
    http.post(apiUrl("/vm-imports/esxi"), async ({ request }) => {
      requests.push({ url: request.url, body: await request.json() });
      return create?.() ?? HttpResponse.json(esxiImport("web-server-01", 9), { status: 201 });
    }),
  );
  return requests;
}

async function openDialog() {
  await userEvent.click(await screen.findByRole("button", { name: "Import from ESXi" }));
  return screen.findByRole("dialog", { name: "Import from ESXi" });
}

async function connect(dialog: HTMLElement, password = PASSWORD) {
  await userEvent.type(within(dialog).getByLabelText("Host"), "esxi1.lab");
  await userEvent.type(within(dialog).getByLabelText("User"), "root");
  await userEvent.type(within(dialog).getByLabelText("Password"), password);
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
}

/** 연결 → 지문 믿기 → VM 목록 */
async function connectAndTrust(dialog: HTMLElement) {
  await connect(dialog);
  await userEvent.click(await within(dialog).findByRole("button", { name: "Trust and continue" }));
  return within(dialog).findByRole("table", { name: "Virtual machines" });
}

it("connects, confirms the fingerprint, picks a VM and creates the import", async () => {
  mockLists({ "vm-import": [vmImport("db", 1)], node: [NODE] });
  const requests = mockEsxi();
  const { router } = renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
  // 기본값이 채워져 있다.
  expect(within(dialog).getByLabelText("Port")).toHaveValue("443");
  expect(within(dialog).getByLabelText("Datacenter")).toHaveValue("ha-datacenter");
  // 브라우저가 저장된 비밀번호를 채우지 않는다.
  expect(within(dialog).getByLabelText("Password")).toHaveAttribute("autocomplete", "new-password");
  // 빈 칸은 보내지 않는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(within(dialog).getByText("Enter the ESXi or vCenter host.")).toBeVisible();
  expect(within(dialog).getByText("Enter the user name.")).toBeVisible();
  expect(requests).toEqual([]);

  // 지문 없이 연결하면 서버가 알려 준 지문을 보이고 믿을지 묻는다.
  await connect(dialog);
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  expect(requests[0]!.body).toEqual({
    host: "esxi1.lab",
    port: 443,
    user: "root",
    password: PASSWORD,
    datacenter: "ha-datacenter",
  });
  await userEvent.click(within(dialog).getByRole("button", { name: "Trust and continue" }));

  // VM 목록: 잠금 경고, 읽지 못한 VM은 고를 수 없다.
  const table = await within(dialog).findByRole("table", { name: "Virtual machines" });
  expect(requests[1]!.body).toEqual({
    host: "esxi1.lab",
    port: 443,
    user: "root",
    password: PASSWORD,
    datacenter: "ha-datacenter",
    fingerprint: FINGERPRINT,
  });
  // 라디오 이름에 데이터스토어, 설명에 잠금 경고와 읽지 못한 이유
  const web = within(table).getByRole("radio", { name: "Web Server 01 (datastore1)" });
  expect(web.closest("tr")).toHaveTextContent("datastore1Web Server 01ubuntu-6422 GiB1");
  expect(within(web.closest("tr")!).getByLabelText(/looks powered on/)).toBeInTheDocument();
  expect(web).toHaveAccessibleDescription(/looks powered on/);
  const broken = within(table).getByRole("radio", { name: "broken/broken.vmx (datastore1)" });
  expect(broken).toBeDisabled();
  expect(broken.closest("tr")).toHaveTextContent("ethernet0 has no networkName");
  expect(broken).toHaveAccessibleDescription("ethernet0 has no networkName");
  const db = within(table).getByRole("radio", { name: "db (datastore2)" });
  expect(db).toBeEnabled();
  expect(db).not.toHaveAccessibleDescription();

  // 고르지 않으면 넘어가지 않는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  expect(within(dialog).getByRole("alert")).toHaveTextContent("Choose a VM.");
  await userEvent.click(web);
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));

  // 이름은 VM 이름에서 이름 규칙으로
  const name = await within(dialog).findByLabelText("Name");
  expect(name).toHaveValue("web-server-01");
  expect(within(dialog).getByText("datastore1 / web01/web01.vmx")).toBeInTheDocument();
  await userEvent.clear(name);
  await userEvent.type(name, "esxi");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("The names ova and esxi are reserved.")).toBeVisible();
  await userEvent.clear(name);
  await userEvent.type(name, "web-server-01");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));

  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/imports/web-server-01"));
  expect(requests).toHaveLength(3);
  expect(new URL(requests[2]!.url).pathname).toBe("/api/v1/vm-imports/esxi");
  expect(requests[2]!.body).toEqual({
    name: "web-server-01",
    connection: {
      host: "esxi1.lab",
      port: 443,
      user: "root",
      password: PASSWORD,
      datacenter: "ha-datacenter",
      fingerprint: FINGERPRINT,
    },
    datastore: "datastore1",
    vmx: "web01/web01.vmx",
  });
  // 비밀번호는 요청 본문에만 있다(주소·브라우저 저장소에 남기지 않는다).
  for (const r of requests) {
    expect(r.url).not.toContain(PASSWORD);
  }
  for (const storage of [localStorage, sessionStorage]) {
    expect(JSON.stringify({ ...storage })).not.toContain(PASSWORD);
  }
});

it("goes back to the VM list from the name step and keeps an edited name", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  mockEsxi();
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connectAndTrust(dialog);
  await userEvent.click(within(dialog).getByRole("radio", { name: "db (datastore2)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  const name = await within(dialog).findByLabelText("Name");
  expect(name).toHaveValue("db");
  await userEvent.type(name, "-prod");
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  expect(within(dialog).getByRole("radio", { name: "db (datastore2)" })).toBeChecked();
  // 같은 VM이면 고친 이름이 남는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  expect(await within(dialog).findByLabelText("Name")).toHaveValue("db-prod");
  // 다른 VM을 고르면 그 VM에서 다시 제안한다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  await userEvent.click(within(dialog).getByRole("radio", { name: "Web Server 01 (datastore1)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  expect(await within(dialog).findByLabelText("Name")).toHaveValue("web-server-01");
});

it("goes back to the connection step from the fingerprint without trusting it", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const requests = mockEsxi();
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connect(dialog);
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  // 연결 단계로 돌아가고 입력은 남는다. 믿지 않았으므로 다시 연결하면 지문을 다시 묻는다.
  expect(within(dialog).getByLabelText("Host")).toHaveValue("esxi1.lab");
  expect(within(dialog).queryByText(FINGERPRINT)).not.toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  expect(requests.map((r) => (r.body as { fingerprint?: string }).fingerprint)).toEqual([
    undefined,
    undefined,
  ]);
});

it("returns to the connection step on an auth error and reuses the trusted fingerprint", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const requests = mockEsxi();
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  // 처음엔 지문이 없어 untrusted → 믿으면 비밀번호가 틀려 esxi_auth → 연결 단계에 이유를 보인다.
  await connect(dialog, "wrong");
  await userEvent.click(await within(dialog).findByRole("button", { name: "Trust and continue" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "ESXi rejected the user name or password. Check them and try again.",
  );
  const password = within(dialog).getByLabelText("Password");
  // 비밀번호를 고쳐 다시 연결하면 믿은 지문을 함께 보낸다(다시 묻지 않는다).
  await userEvent.clear(password);
  await userEvent.type(password, PASSWORD);
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByRole("table", { name: "Virtual machines" })).toBeVisible();
  expect(requests.map((r) => (r.body as { fingerprint?: string }).fingerprint)).toEqual([
    undefined,
    FINGERPRINT,
    FINGERPRINT,
  ]);

  // 목록에서 뒤로 가면 연결 단계다. 호스트를 바꾸면 믿은 지문을 버린다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  const host = within(dialog).getByLabelText("Host");
  await userEvent.clear(host);
  await userEvent.type(host, "esxi2.lab");
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  expect(requests[3]!.body).toEqual({
    host: "esxi2.lab",
    port: 443,
    user: "root",
    password: PASSWORD,
    datacenter: "ha-datacenter",
  });
});

it("reconnects when the certificate changes before the import is created", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const requests = mockEsxi(() =>
    HttpResponse.json(
      {
        code: "fingerprint_mismatch",
        message: "certificate fingerprint mismatch",
        details: { fingerprint: "AA:BB" },
      },
      { status: 409 },
    ),
  );
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connectAndTrust(dialog);
  await userEvent.click(within(dialog).getByRole("radio", { name: "db (datastore2)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  await userEvent.click(await within(dialog).findByRole("button", { name: "Create" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The ESXi certificate changed since you trusted it. Connect again and check the new fingerprint.",
  );
  expect(within(dialog).getByLabelText("Host")).toHaveValue("esxi1.lab");
  // 믿은 지문을 버렸으므로 다시 연결하면 새 지문을 묻는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByRole("button", { name: "Trust and continue" })).toBeVisible();
  expect((requests.at(-1)!.body as { fingerprint?: string }).fingerprint).toBeUndefined();
});

it("shows a taken name on the name field", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  mockEsxi(() =>
    HttpResponse.json(
      { code: "name_taken", message: 'vm-import "db" already exists' },
      { status: 409 },
    ),
  );
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connectAndTrust(dialog);
  await userEvent.click(within(dialog).getByRole("radio", { name: "db (datastore2)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  await userEvent.click(await within(dialog).findByRole("button", { name: "Create" }));
  const name = within(dialog).getByLabelText("Name");
  await vi.waitFor(() =>
    expect(name).toHaveAccessibleDescription("An import with this name already exists."),
  );
  expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
});

it("says why the connection failed", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const failures = [
    {
      status: 400,
      code: "esxi_auth",
      message: "ESXi rejected the user name or password",
      text: "ESXi rejected the user name or password. Check them and try again.",
    },
    {
      status: 502,
      code: "esxi_unreachable",
      message: "cannot connect to esxi1.lab:443: connection refused",
      text: "Cannot reach the ESXi host (cannot connect to esxi1.lab:443: connection refused).",
    },
    {
      status: 409,
      code: "fingerprint_mismatch",
      message: "certificate fingerprint mismatch",
      text: "The ESXi certificate changed since you trusted it. Connect again and check the new fingerprint.",
    },
  ];
  let next = 0;
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
    http.post(apiUrl("/vm-imports/esxi/browse"), () => {
      const f = failures[next++]!;
      return HttpResponse.json(
        { code: f.code, message: f.message, details: { fingerprint: FINGERPRINT } },
        { status: f.status },
      );
    }),
  );
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connect(dialog);
  for (const f of failures) {
    if (f !== failures[0]) {
      await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
    }
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(f.text);
    // 연결 칸은 그대로 남아 고쳐서 다시 연결할 수 있다.
    expect(within(dialog).getByLabelText("Password")).toHaveValue(PASSWORD);
  }
});

it("hides the ESXi import from viewers", async () => {
  mockLists({ "vm-import": [vmImport("web01", 1)] });
  renderPage(<ImportsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("web01")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Import from ESXi" })).not.toBeInTheDocument();
});

it("shows the ESXi source and explains lost credentials", async () => {
  const lost = esxiImport("web01", 1, {
    phase: "failed",
    error:
      "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again",
  });
  mockLists({ "vm-import": [lost], node: [NODE] });
  renderPage(<ImportsPage />);
  const table = await screen.findByRole("table", { name: "Imports" });
  const row = (await within(table).findByText("web01")).closest("tr")!;
  expect(row).toHaveTextContent("Web 01esxi1.lab / datastore1 / web01/web01.vmx");
  expect(row).toHaveTextContent(
    "The ESXi password is no longer on this node (opld restarted or the request was interrupted). Delete this import and create it again.",
  );
});
```

`web/src/imports/EsxiImportDialog.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { TriangleAlert } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
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
import {
  esxiConnection,
  esxiImportName,
  importNameError,
  validateConnection,
  type ConnectionForm,
} from "@/imports/imports";
import { formatMib } from "@/lib/format";

type EsxiVm = components["schemas"]["EsxiVm"];

/** 연결 → (처음 보는 인증서면) 지문 확인 → VM 고르기 → 이름 */
type Step = "connect" | "trust" | "choose" | "name";

/** 잠긴 VM 라디오가 설명으로 가리키는 요소 */
const LOCKED_ID = "esxi-vm-locked";

/** 목록의 VM 한 행. 키는 `데이터스토어/.vmx 경로`다. */
interface Row {
  key: string;
  datastore: string;
  vm: EsxiVm;
}

/** 믿기로 한 인증서. 같은 호스트·포트로 다시 연결하면 지문을 함께 보낸다. */
interface Trusted {
  host: string;
  port: string;
  fingerprint: string;
}

function hasCode(error: unknown, code: string): boolean {
  return error instanceof ApiError && error.code === code;
}

/** 서버가 `untrusted_certificate`의 details로 알려 준 지문 */
function offeredFingerprint(error: unknown): string | undefined {
  if (error instanceof ApiError && error.code === "untrusted_certificate") {
    const fingerprint = (error.details as { fingerprint?: unknown } | undefined)?.fingerprint;
    return typeof fingerprint === "string" ? fingerprint : undefined;
  }
  return undefined;
}

/**
 * ESXi·vCenter에 연결해 VM을 고르고 가져오기를 만든다. 비밀번호는 이 컴포넌트의 상태에만 있고 요청
 * 본문으로만 보낸다(서버는 자격 증명을 접속한 노드의 메모리에만 둔다).
 */
export function EsxiImportDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const imports = useList("vm-import");
  const nodes = useList("node");
  // 연결과 가져오기는 접속한 노드에서 돈다(OVA 올리기와 같은 질의).
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
  const [step, setStep] = useState<Step>("connect");
  const [form, setForm] = useState<ConnectionForm>({
    host: "",
    port: "443",
    user: "",
    password: "",
    datacenter: "ha-datacenter",
  });
  // 확인을 기다리는 지문, 믿기로 한 인증서, 목록을 읽은(만들기에 쓸) 지문
  const [offered, setOffered] = useState("");
  const [trusted, setTrusted] = useState<Trusted | null>(null);
  const [fingerprint, setFingerprint] = useState("");
  const [rows, setRows] = useState<Row[]>([]);
  const [chosen, setChosen] = useState("");
  const [name, setName] = useState("");
  // 이름을 제안한 VM. 고른 VM이 바뀔 때만 다시 제안해 사용자가 고친 이름을 지킨다.
  const [namedFor, setNamedFor] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const set = (patch: Partial<ConnectionForm>) => setForm((f) => ({ ...f, ...patch }));
  const row = rows.find((r) => r.key === chosen);

  /** 연결 단계로 돌아간다. 읽은 목록과 고른 VM은 버린다. */
  function backToConnect() {
    setRows([]);
    setChosen("");
    setFingerprint("");
    setErrors({});
    setStep("connect");
  }

  async function browse(trust?: string) {
    try {
      const found = await call(
        api.POST("/vm-imports/esxi/browse", { body: esxiConnection(form, trust) }),
      );
      setRows(
        found.datastores.flatMap((ds) =>
          ds.vms.map((vm) => ({ key: `${ds.name}/${vm.vmx}`, datastore: ds.name, vm })),
        ),
      );
      setFingerprint(trust ?? "");
      setStep("choose");
    } catch (error) {
      const offer = offeredFingerprint(error);
      if (offer && !trust) {
        setOffered(offer);
        setStep("trust");
        return;
      }
      if (hasCode(error, "fingerprint_mismatch")) {
        // 인증서가 바뀌었다. 다시 연결해 새 지문을 확인한다.
        setTrusted(null);
      }
      // 인증·연결 오류도 연결 단계에서 고쳐 다시 연결한다(오류는 폼 위에 보인다).
      backToConnect();
      throw error;
    }
  }

  async function submit() {
    switch (step) {
      case "connect": {
        const found = validateConnection(form);
        setErrors(found);
        if (Object.keys(found).length === 0) {
          // 같은 호스트·포트의 인증서를 이미 믿었으면 지문을 함께 보낸다.
          const same = trusted?.host === form.host.trim() && trusted.port === form.port;
          await browse(same ? trusted.fingerprint : undefined);
        }
        return false;
      }
      case "trust":
        setTrusted({ host: form.host.trim(), port: form.port, fingerprint: offered });
        await browse(offered);
        return false;
      case "choose":
        if (!row) {
          setErrors({ vm: "imports.errors.vm" });
          return false;
        }
        setErrors({});
        if (namedFor !== row.key) {
          setName(esxiImportName(row.vm));
          setNamedFor(row.key);
        }
        setStep("name");
        return false;
      case "name": {
        const nameError = importNameError(name, imports.data?.map((i) => i.name) ?? []);
        setErrors(nameError ? { name: nameError } : {});
        if (nameError || !row) {
          return false;
        }
        let imp;
        try {
          imp = await call(
            api.POST("/vm-imports/esxi", {
              body: {
                name,
                connection: esxiConnection(form, fingerprint),
                datastore: row.datastore,
                vmx: row.vm.vmx,
              },
            }),
          );
        } catch (error) {
          if (hasCode(error, "name_taken")) {
            setErrors({ name: "imports.errors.exists" });
            return false;
          }
          if (hasCode(error, "fingerprint_mismatch")) {
            // 목록을 읽은 뒤 인증서가 바뀌었다. 다시 연결해 새 지문을 확인한다.
            setTrusted(null);
            backToConnect();
          }
          throw error;
        }
        void navigate(`/imports/${encodeURIComponent(imp.name)}`);
      }
    }
  }

  const submitLabel = {
    connect: t("imports.connect"),
    trust: t("imports.trust"),
    choose: t("imports.next"),
    name: t("common.create"),
  }[step];

  return (
    <FormDialog
      title={t("imports.esxiTitle")}
      description={t("imports.esxiDescription", { node: here })}
      submitLabel={submitLabel}
      onClose={onClose}
      onSubmit={submit}
    >
      {cluster.isError && (
        <p role="alert" className="text-sm text-destructive">
          {t("uploads.noNode", { error: errorMessage(t, cluster.error) })}
        </p>
      )}
      {step === "connect" && (
        <>
          <Field id="esxi-host" label={t("imports.host")} error={errors.host}>
            {(a11y) => (
              <Input
                id="esxi-host"
                value={form.host}
                onChange={(e) => set({ host: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id="esxi-port" label={t("imports.port")} error={errors.port}>
            {(a11y) => (
              <Input
                id="esxi-port"
                inputMode="numeric"
                value={form.port}
                onChange={(e) => set({ port: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id="esxi-user" label={t("imports.user")} error={errors.user}>
            {(a11y) => (
              <Input
                id="esxi-user"
                autoComplete="off"
                value={form.user}
                onChange={(e) => set({ user: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field id="esxi-password" label={t("imports.password")}>
            <Input
              id="esxi-password"
              type="password"
              autoComplete="new-password"
              value={form.password}
              onChange={(e) => set({ password: e.target.value })}
            />
          </Field>
          <Field
            id="esxi-datacenter"
            label={t("imports.datacenter")}
            error={errors.datacenter}
            hint={t("imports.datacenterHint")}
          >
            {(a11y) => (
              <Input
                id="esxi-datacenter"
                value={form.datacenter}
                onChange={(e) => set({ datacenter: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {step === "trust" && (
        <div className="flex flex-col gap-2 text-sm">
          <p>{t("imports.untrusted", { host: form.host.trim() })}</p>
          <p className="text-muted-foreground">{t("imports.fingerprint")}</p>
          <code className="rounded bg-muted p-2 font-mono text-xs break-all">{offered}</code>
          <Button type="button" variant="outline" className="self-start" onClick={backToConnect}>
            {t("imports.back")}
          </Button>
        </div>
      )}
      {step === "choose" && (
        <div className="flex flex-col gap-1.5">
          {rows.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t("imports.noVms")}</p>
          ) : (
            <Table aria-label={t("imports.vms")}>
              <TableHeader>
                <TableRow>
                  <TableHead />
                  <TableHead>{t("imports.datastore")}</TableHead>
                  <TableHead>{t("common.name")}</TableHead>
                  <TableHead>{t("imports.guestOs")}</TableHead>
                  <TableHead>{t("vms.cpu")}</TableHead>
                  <TableHead>{t("vms.memory")}</TableHead>
                  <TableHead>{t("vms.disks")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map(({ key, datastore, vm }, i) => {
                  // 잠금 경고와 읽지 못한 이유는 라디오의 설명이다.
                  const errorId = `esxi-vm-${i}-error`;
                  const described = [vm.locked && LOCKED_ID, vm.error && errorId]
                    .filter(Boolean)
                    .join(" ");
                  return (
                    <TableRow key={key}>
                      <TableCell>
                        {/* 읽지 못한 VM은 고를 수 없다 */}
                        <input
                          type="radio"
                          name="esxi-vm"
                          aria-label={`${vm.name || vm.vmx} (${datastore})`}
                          aria-describedby={described || undefined}
                          disabled={!!vm.error}
                          checked={chosen === key}
                          onChange={() => setChosen(key)}
                        />
                      </TableCell>
                      <TableCell>{datastore}</TableCell>
                      <TableCell className="whitespace-normal">
                        <span className="inline-flex items-center gap-1">
                          {vm.name || vm.vmx}
                          {vm.locked && (
                            <TriangleAlert
                              role="img"
                              aria-label={t("imports.locked")}
                              className="size-4 text-amber-600"
                            />
                          )}
                        </span>
                        {vm.error && (
                          <div
                            id={errorId}
                            className="max-w-xs text-xs break-words text-destructive"
                          >
                            {vm.error}
                          </div>
                        )}
                      </TableCell>
                      <TableCell>{vm.error ? "" : vm.guest_os}</TableCell>
                      <TableCell>{vm.error ? "" : vm.vcpus}</TableCell>
                      <TableCell>{vm.error ? "" : formatMib(vm.memory_mib)}</TableCell>
                      <TableCell>{vm.error ? "" : vm.disks}</TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}
          {/* 잠긴 VM 라디오의 설명(아이콘의 이름은 설명으로 읽히지 않을 수 있다) */}
          <span id={LOCKED_ID} hidden>
            {t("imports.locked")}
          </span>
          {errors.vm && (
            <p role="alert" className="text-xs text-destructive">
              {t(errors.vm)}
            </p>
          )}
          <Button type="button" variant="outline" className="self-start" onClick={backToConnect}>
            {t("imports.back")}
          </Button>
        </div>
      )}
      {step === "name" && row && (
        <>
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">{t("imports.chosenVm")}</dt>
            <dd className="break-all">{`${row.datastore} / ${row.vm.vmx}`}</dd>
          </dl>
          <Field id="esxi-name" label={t("common.name")} error={errors.name}>
            {(a11y) => (
              <Input
                id="esxi-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Button
            type="button"
            variant="outline"
            className="self-start"
            onClick={() => {
              setErrors({});
              setStep("choose");
            }}
          >
            {t("imports.back")}
          </Button>
        </>
      )}
    </FormDialog>
  );
}
```

`web/src/imports/ImportPage.test.tsx` 전체:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Network, VmImport } from "@/api/types";
import { ImportPage } from "@/imports/ImportPage";
import { esxiImport, NODE, pool, task, vm, vmImport, volume } from "@/test/fixtures";
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

it("shows an ESXi source and explains lost credentials", async () => {
  const lost = esxiImport("web01", 1, {
    phase: "failed",
    error:
      "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again",
  });
  mockLists({ "vm-import": [lost], node: [NODE] });
  open(lost);
  expect(
    await screen.findByText("esxi1.lab / datastore1 / web01/web01.vmx, node-a"),
  ).toBeInTheDocument();
  expect(
    screen.getByText(
      "The source cannot be used: The ESXi password is no longer on this node (opld restarted or the request was interrupted). Delete this import and create it again.",
    ),
  ).toBeInTheDocument();
  const disks = screen.getByRole("table", { name: "Disks" });
  expect(within(disks).getByRole("columnheader", { name: "File on the datastore" })).toBeVisible();
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

it("explains server errors in terms of the node running the import", async () => {
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
    await screen.findByText("This storage pool is not available on the node running the import."),
  ).toBeVisible();
  code = "node_offline";
  await userEvent.click(screen.getByRole("button", { name: "Import" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The node running the import is offline or has left the cluster.",
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
  expect(dialog).toHaveTextContent(
    "Deletes the import and its files on node-a. The source VM and any VM already imported are kept.",
  );
  // 원본 노드가 없으면 웹에서는 지울 수 없다. 리소스만 지우는 명령을 알린다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(
    await within(dialog).findByText(
      "The node running the import is offline or has left the cluster. To forget the import and leave its files on that node, run oplctl import delete web01 --force.",
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

`web/src/imports/ImportPage.tsx`에서

```tsx
import { codeMessage, errorMessage } from "@/i18n/errors";
import { runForm, sourceNetworks, toRun, validateRun, type RunForm } from "@/imports/imports";
import { ImportedVm, ImportPhaseBadge } from "@/imports/ImportsPage";
import { formatBytes, formatMib } from "@/lib/format";
```

을 다음으로 바꾼다:

```tsx
import { codeMessage, errorMessage } from "@/i18n/errors";
import {
  runForm,
  sourceNetworks,
  sourceText,
  toRun,
  validateRun,
  type RunForm,
} from "@/imports/imports";
import { ImportedVm, ImportPhaseBadge, useImportError } from "@/imports/ImportsPage";
import { formatBytes, formatMib } from "@/lib/format";
```

`web/src/imports/ImportPage.tsx`에서

```tsx
  const ova = "ova" in imp.spec.source ? imp.spec.source.ova : undefined;
  const { phase, error, warnings, source_vm: src } = imp.status;
```

을 다음으로 바꾼다:

```tsx
  const ova = "ova" in imp.spec.source ? imp.spec.source.ova : undefined;
  const { phase, warnings, source_vm: src } = imp.status;
  const error = useImportError(imp);
```

`web/src/imports/ImportPage.tsx`에서

```tsx
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">{t("imports.fileName")}</dt>
            <dd>
              {ova && `${ova.file_name} (${formatBytes(ova.size_bytes)}), `}
              {node}
            </dd>
```

을 다음으로 바꾼다:

```tsx
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">
              {t(ova ? "imports.fileName" : "imports.esxiSource")}
            </dt>
            <dd>
              {ova ? `${ova.file_name} (${formatBytes(ova.size_bytes)})` : sourceText(imp)}
              {`, ${node}`}
            </dd>
```

`web/src/imports/ImportPage.tsx`에서

```tsx
                    <TableHead>{t("common.name")}</TableHead>
                    <TableHead>{t("imports.diskFile")}</TableHead>
                    <TableHead>{t("imports.capacity")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
                    <TableHead>{t("common.name")}</TableHead>
                    <TableHead>{t(ova ? "imports.diskFile" : "imports.diskFileEsxi")}</TableHead>
                    <TableHead>{t("imports.capacity")}</TableHead>
```

`web/src/imports/ImportsPage.test.tsx`에서

```tsx
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("The name ova is reserved.")).toBeVisible();
```

을 다음으로 바꾼다:

```tsx
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("The names ova and esxi are reserved.")).toBeVisible();
```

`web/src/imports/ImportsPage.tsx` 전체:

```tsx
import { useQuery } from "@tanstack/react-query";
import { Server, Upload } from "lucide-react";
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
import { EsxiImportDialog } from "@/imports/EsxiImportDialog";
import { CREDENTIALS_LOST, importNameError, sourceText } from "@/imports/imports";
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

/** 가져오기의 오류. ESXi 자격 증명이 사라진 오류는 무엇을 할지 알려 주는 문장으로 바꾼다. */
export function useImportError(imp: VmImport): string | null | undefined {
  const { t } = useTranslation();
  const error = imp.status.error;
  return error === CREDENTIALS_LOST ? t("imports.credentialsLost") : error;
}

/** 목록의 상태 아래 오류 */
function ImportError({ imp }: { imp: VmImport }) {
  const error = useImportError(imp);
  return error ? (
    <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">{error}</div>
  ) : null;
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
  const [esxi, setEsxi] = useState(false);
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
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => setEsxi(true)}>
              <Server />
              {t("imports.esxi")}
            </Button>
            <Button onClick={() => setUploading(true)}>
              <Upload />
              {t("imports.upload")}
            </Button>
          </div>
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
                <div className="text-xs text-muted-foreground">{sourceText(imp)}</div>
              </TableCell>
              <TableCell>
                <ImportPhaseBadge phase={imp.status.phase} />
                <ImportError imp={imp} />
              </TableCell>
              <TableCell>
                <ImportedVm imp={imp} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {uploading && <OvaUploadDialog onClose={() => setUploading(false)} />}
      {esxi && <EsxiImportDialog onClose={() => setEsxi(false)} />}
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

`web/src/imports/imports.test.ts`에서

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
```

을 다음으로 바꾼다:

```ts
import {
  CREDENTIALS_LOST,
  esxiConnection,
  esxiImportName,
  importNameError,
  nameFromVm,
  runForm,
  sourceNetworks,
  sourceText,
  toRun,
  validateConnection,
  validateRun,
  volumeNames,
  type ConnectionForm,
} from "@/imports/imports";
import { esxiImport, vmImport } from "@/test/fixtures";
```

`web/src/imports/imports.test.ts`에서

```ts
  expect(importNameError("ova", [])).toBe("imports.errors.reserved");
  expect(importNameError("db", ["db"])).toBe("imports.errors.exists");
```

을 다음으로 바꾼다:

```ts
  expect(importNameError("ova", [])).toBe("imports.errors.reserved");
  expect(importNameError("esxi", [])).toBe("imports.errors.reserved");
  expect(importNameError("db", ["db"])).toBe("imports.errors.exists");
```

`web/src/imports/imports.test.ts`에서

```ts
    keep_macs: false,
  });
});
```

을 다음으로 바꾼다:

```ts
    keep_macs: false,
  });
});

it("names an import after the source VM", () => {
  expect(nameFromVm("Web Server 01")).toBe("web-server-01");
  // 점도 이름 규칙에 맞춘다(파일 이름처럼 확장자로 보지 않는다).
  expect(nameFromVm("db.example.com")).toBe("db-example-com");
  expect(nameFromVm("--A_b--")).toBe("a-b");
  expect(nameFromVm(`${"x".repeat(62)}-yy`)).toBe("x".repeat(62));
  expect(nameFromVm("웹")).toBe("");
});

it("names an ESXi import from the VM, then its folder", () => {
  expect(esxiImportName({ name: "Web Server 01", vmx: "web01/web01.vmx" })).toBe("web-server-01");
  expect(esxiImportName({ name: "웹 서버", vmx: "Web 01/x.vmx" })).toBe("web-01");
  expect(esxiImportName({ name: "", vmx: "dir/x.vmx" })).toBe("dir");
  expect(esxiImportName({ name: "웹", vmx: "서버/x.vmx" })).toBe("esxi-import");
  expect(esxiImportName({ name: "", vmx: "x.vmx" })).toBe("esxi-import");
});

it("shows where the source is", () => {
  expect(sourceText(vmImport("web01", 1))).toBe("web01.ova");
  expect(sourceText(esxiImport("web01", 1))).toBe("esxi1.lab / datastore1 / web01/web01.vmx");
  // 서버의 문구와 같아야 화면이 안내로 바꾼다.
  expect(CREDENTIALS_LOST).toBe(
    "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again",
  );
});

const connection: ConnectionForm = {
  host: " esxi1.lab ",
  port: "443",
  user: "root",
  password: "s3cret",
  datacenter: "ha-datacenter",
};

it("checks the ESXi connection form", () => {
  expect(validateConnection(connection)).toEqual({});
  expect(
    validateConnection({ host: " ", port: "0", user: "", password: "", datacenter: "" }),
  ).toEqual({
    host: "imports.errors.host",
    port: "imports.errors.port",
    user: "imports.errors.user",
    datacenter: "imports.errors.datacenter",
  });
  expect(validateConnection({ ...connection, port: "65536" })).toEqual({
    port: "imports.errors.port",
  });
  // 비밀번호는 비어 있어도 된다(서버가 거절하면 esxi_auth).
  expect(validateConnection({ ...connection, password: "" })).toEqual({});
});

it("builds the connection body with every field", () => {
  expect(esxiConnection(connection)).toEqual({
    host: "esxi1.lab",
    port: 443,
    user: "root",
    password: "s3cret",
    datacenter: "ha-datacenter",
  });
  expect(esxiConnection(connection, "AB:CD")).toEqual({
    host: "esxi1.lab",
    port: 443,
    user: "root",
    password: "s3cret",
    datacenter: "ha-datacenter",
    fingerprint: "AB:CD",
  });
});
```

`web/src/imports/imports.ts`에서

```ts

/** `/vm-imports/ova` 경로와 겹쳐 서버가 가져오기 이름으로 받지 않는다. */
export const RESERVED_IMPORT = "ova";
/** 서버의 `cores`·`sockets`(u16)와 `memory_mib`(u32)의 최댓값 */
```

을 다음으로 바꾼다:

```ts

/** `/vm-imports/ova`·`/vm-imports/esxi` 경로와 겹쳐 서버가 가져오기 이름으로 받지 않는다. */
export const RESERVED_IMPORTS = ["ova", "esxi"];
/** 서버의 `CREDENTIALS_LOST`. 원본 노드 메모리에 ESXi 자격 증명이 없는(opld가 다시 시작했거나 요청이 끊긴) 가져오기의 오류다. */
export const CREDENTIALS_LOST =
  "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again";
/** 서버의 `cores`·`sockets`(u16)와 `memory_mib`(u32)의 최댓값 */
```

`web/src/imports/imports.ts`에서

```ts
  }
  if (name === RESERVED_IMPORT) {
    return "imports.errors.reserved";
```

을 다음으로 바꾼다:

```ts
  }
  if (RESERVED_IMPORTS.includes(name)) {
    return "imports.errors.reserved";
```

`web/src/imports/imports.ts`에서

```ts
  return undefined;
}
```

을 다음으로 바꾼다:

```ts
  return undefined;
}

/** VM 이름에서 이름 규칙에 맞춘 가져오기 이름. 맞출 수 없으면 빈 문자열이다. */
export function nameFromVm(vm: string): string {
  return vm
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, MAX_NAME)
    .replace(/-+$/, "");
}

/**
 * ESXi VM으로 만들 가져오기 이름: VM 이름에서, 맞출 수 없으면 `.vmx`가 있는 디렉터리 이름에서, 그래도
 * 안 되면 `esxi-import`.
 */
export function esxiImportName(vm: { name: string; vmx: string }): string {
  const dir = vm.vmx.split("/").slice(0, -1).pop() ?? "";
  return nameFromVm(vm.name) || nameFromVm(dir) || "esxi-import";
}

/** 원본이 어디 있는지: OVA면 파일 이름, ESXi면 `호스트 / 데이터스토어 / .vmx 경로` */
export function sourceText(imp: VmImport): string {
  const source = imp.spec.source;
  if ("ova" in source) {
    return source.ova.file_name;
  }
  return `${source.esxi.host} / ${source.esxi.datastore} / ${source.esxi.vmx}`;
}

/** ESXi 연결 폼. 포트는 입력 그대로의 문자열이다. 비밀번호는 이 상태에만 있고 저장하지 않는다. */
export interface ConnectionForm {
  host: string;
  port: string;
  user: string;
  password: string;
  datacenter: string;
}

/** ESXi 연결 폼의 입력 오류: 필드 → 번역 키. 비밀번호는 비어 있어도 된다. */
export function validateConnection(form: ConnectionForm): Record<string, string> {
  const errors: Record<string, string> = {};
  if (form.host.trim() === "") {
    errors.host = "imports.errors.host";
  }
  if (!intIn(form.port, 1, 65535)) {
    errors.port = "imports.errors.port";
  }
  if (form.user === "") {
    errors.user = "imports.errors.user";
  }
  if (form.datacenter.trim() === "") {
    errors.datacenter = "imports.errors.datacenter";
  }
  return errors;
}

/** 연결 요청 본문. 포트와 데이터센터도 늘 보낸다. 지문은 믿기로 한 뒤에만 붙인다. */
export function esxiConnection(
  form: ConnectionForm,
  fingerprint?: string,
): Schemas["EsxiConnection"] {
  return {
    host: form.host.trim(),
    port: Number(form.port),
    user: form.user,
    password: form.password,
    datacenter: form.datacenter.trim(),
    ...(fingerprint ? { fingerprint } : {}),
  };
}
```

`web/src/test/fixtures.ts`에서

```ts
      ...status,
    },
  };
}
```

을 다음으로 바꾼다:

```ts
      ...status,
    },
  };
}

/** ESXi에서 만든 가져오기. 원본 VM과 제안은 `vmImport`와 같다. */
export function esxiImport(
  name: string,
  n: number,
  status: Partial<VmImport["status"]> = {},
): VmImport {
  const imp = vmImport(name, n, status);
  return {
    ...imp,
    spec: {
      node: "n1",
      source: {
        esxi: {
          host: "esxi1.lab",
          port: 443,
          datacenter: "ha-datacenter",
          datastore: "datastore1",
          vmx: `${name}/${name}.vmx`,
          user: "root",
          fingerprint: FINGERPRINT,
        },
      },
    },
  };
}

/** 가짜 ESXi 인증서 지문 */
export const FINGERPRINT = Array.from({ length: 32 }, (_, i) =>
  (i * 7 + 16).toString(16).toUpperCase().padStart(2, "0"),
).join(":");
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 422개

Run: `pnpm -C web build`
Expected: 성공

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): ESXi에서 가져오기 대화상자 — /imports의 \"ESXi에서 가져오기\"(쓰기 권한), 연결(호스트·포트 443·사용자·비밀번호·데이터센터 ha-datacenter, 포트·데이터센터도 늘 보냄) → untrusted_certificate면 서버가 알려 준 지문을 보이고 \"이 지문을 믿고 계속\" → VM 표(데이터스토어·이름·게스트·CPU·메모리·디스크, 잠금 경고 아이콘, 읽지 못한 VM은 고를 수 없음) → VM 이름에서 이름 규칙으로 만든 가져오기 이름(뒤로 가기) → 만들기 → /imports/:name, 비밀번호는 컴포넌트 상태와 요청 본문에만, fingerprint_mismatch면 연결 단계로, 예약 이름 esxi, 목록·상세의 원본 표시 host / datastore / vmx와 데이터스토어의 파일, 자격 증명 사라짐 오류는 안내 문장으로, 오류 코드 문구 untrusted_certificate·fingerprint_mismatch·esxi_auth·esxi_unreachable, ko·en"
```

---

### Task 7: 실제 시험과 문서

`crates/opld/tests/vm_import.rs`에 `esxi_import_downloads_the_disk_and_restart_loses_credentials`(기능 `qemu-it`): 시험 안에서 가짜 ESXi(`web01`, efi, 2 vCPU, vmxnet3 `VM Network`, 고정 MAC, vmfs 디스크립터와 표식을 쓴 4 MiB flat)를 띄워 `import esxi-browse`(지문 없음 → 안내, 이때 서버는 Authorization을 받지 않음; 지문과 함께 → 표) → `import esxi` → `import get`(제안, 리소스 어디에도 비밀번호 없음) → `import run --net lan` → 멈춘 VM·볼륨 표식·MAC·`done`; opld를 다시 시작하면 남은 ESXi 가져오기가 `failed`. README "VMware에서 가져오기"에 ESXi·vCenter 절, 설계 "구현(계획 30)" 메모와 알려진 틈, 처음 분해 문서(하위 프로젝트 8 완료).

**Files:**
- Modify: `README.md`
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/tests/vm_import.rs`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-04-vmware-import-design.md`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown

VMware VM을 OVA 파일로 내보내 opLite로 가져온다. 가져오기(`vm-import`)는 세 단계다: OVA를 올리면 opld가 OVF를 읽어
원본 VM(CPU, 메모리, 펌웨어, 디스크, NIC)과 opLite VM 제안을 보이고, 사용자가 이름·풀·네트워크 매핑과 고칠 값을 정해
실행하면 디스크를 볼륨으로 옮겨 **꺼진** 새 VM을 만든다. 실행이 실패하면 원본이 남으므로 값을 바꿔 다시 실행한다.
```

을 다음으로 바꾼다:

```markdown

VMware VM을 OVA 파일로 내보내거나 ESXi·vCenter 데이터스토어에서 바로 읽어 opLite로 가져온다. 가져오기(`vm-import`)는
세 단계다: OVA를 올리거나 ESXi의 VM을 고르면 opld가 OVF(또는 `.vmx`와 디스크 디스크립터)를 읽어 원본 VM(CPU, 메모리,
펌웨어, 디스크, NIC)과 opLite VM 제안을 보이고, 사용자가 이름·풀·네트워크 매핑과 고칠 값을 정해 실행하면 디스크를
볼륨으로 옮겨 **꺼진** 새 VM을 만든다. 실행이 실패하면 원본이 남으므로 값을 바꿔 다시 실행한다.
```

`README.md`에서

````markdown
oplctl import delete web01                      # 가져오기와 남은 원본 파일을 지운다(만든 VM은 남는다)
```

- **제안.** 게스트 OS(`vmw:osType`)가 Windows(`win…`)면 디스크 `sata`·NIC `e1000e`(Windows가 드라이버를 갖고 있다),
````

을 다음으로 바꾼다:

````markdown
oplctl import delete web01                      # 가져오기와 남은 원본 파일을 지운다(만든 VM은 남는다)
```

**ESXi·vCenter에서 바로 가져오기.** 내보내기 없이 opld가 ESXi(또는 vCenter)의 데이터스토어 브라우저
(`https://호스트/folder`)에서 `.vmx`와 디스크를 HTTPS로 읽는다. VMware에서 VM을 **끄고**, 스냅샷이 있으면 통합하거나
지운다(스냅샷 디스크는 400 "the VM has snapshots; consolidate or delete them in VMware first", RDM도 400). 디스크는
VMFS·flat VMDK(디스크립터 + `-flat.vmdk`)여야 한다. 아래의 제안·네트워크 매핑·MAC·상태 등은 OVA와 같다.

```bash
export OPL_ESXI_PASSWORD='…'                    # 또는 --password-file, 둘 다 없으면 터미널에서 묻는다(인자로는 받지 않는다)
oplctl import esxi-browse --host esx1.example.com --user root
# error: the ESXi certificate fingerprint is AB:CD:…; check it and rerun with --fingerprint AB:CD:…
oplctl import esxi-browse --host esx1.example.com --user root --fingerprint AB:CD:…
# DATASTORE   VMX              NAME   GUEST      CPU  MEMORY  DISKS  LOCKED
# datastore1  web01/web01.vmx  web01  ubuntu-64  2    1.0GiB  1      false
oplctl import esxi web01 --host esx1.example.com --user root --fingerprint AB:CD:… \
  --datastore datastore1 --vmx web01/web01.vmx
oplctl import get web01                         # SOURCE: esxi://esx1.example.com/datastore1/web01/web01.vmx
oplctl import run web01 --name web --pool local --net lan     # 디스크를 내려받아 볼륨으로 옮긴다
```

- **인증서 지문.** ESXi 인증서는 대개 자체 서명이라 시스템 CA로 검증하지 않고 SHA-256 지문을 고정한다. 지문 없이
  연결하면 opld가 서버 인증서의 지문을 알려 준다(409 `untrusted_certificate`). ESXi 호스트에서 직접 얻은 지문
  (예: ESXi 셸의 `openssl x509 -in /etc/vmware/ssl/rui.crt -noout -fingerprint -sha256`)과 같은지 확인한 뒤
  `--fingerprint`로 다시 한다. 지문이 다르면 비밀번호를 보내기 전에 끊는다(409 `fingerprint_mismatch`). 사용자
  이름·비밀번호가 틀리면 400 `esxi_auth`, 연결할 수 없으면 502 `esxi_unreachable`, 지문 모양이 틀리면(SHA-256 16진 64자가
  아니면) 연결하기 전에 400이다.
- **찾기.** `esxi-browse`는 데이터스토어마다 맨 위 디렉터리의 첫 `.vmx`를 읽어(500개까지) 보인다. 읽지 못한 VM은 NAME
  자리에 이유가 나온다. `LOCKED`가 `true`면 VM 디렉터리에 잠금 파일(`.lck`로 끝나는 항목, NFS의 `.lck-…`)이나 켜진
  VM만 가지는 스왑 파일(`.vswp`)이 있다 — 켜져 있을 수 있으니 끈다(만들 때도 경고한다). 이 파일들로 짐작할 뿐이므로
  켜진 VM을 가져오지 않게 직접 확인한다. **일시 중단된 VM**(디렉터리에 `.vmss`)도 `LOCKED`가 `true`이고 만들 때 경고한다
  ("the VM is suspended; power it off in VMware before importing (its memory state is not imported)") — 메모리
  상태는 가져오지 않으므로 VMware에서 다시 켠 뒤 게스트를 정상 종료한다. 목록을 읽지 못한 데이터스토어·폴더는 오류 없이
  건너뛴다 — 보여야 할 VM이 없으면 opld 로그를 본다. ESXi `/folder` 페이지의 모양은 문서와 화면을 보고 가짜 ESXi로
  흉내 내 시험했을 뿐 실제 ESXi에서는 시험하지 않았다.
- **씬 디스크와 여유.** `/folder`는 디스크를 통째로 주므로 씬 디스크도 **전체 크기로** 내려받는다. 실행 노드의 업로드
  디렉터리(`{data_dir}/uploads`)에 가장 큰 디스크 용량 + 1 GiB의 여유가 필요하고(아니면 507; 작업도 디스크마다 다시
  본다), 시간도 전체 크기만큼 걸린다. 디스크마다 `disk.raw`로 받아 볼륨으로 옮기고 지운다(풀에는 0이 아닌 부분만
  남는다). 받은 길이가 디스크립터와 다르면 실패한다. 내려받기는 취소할 수 없고, 실패하면 처음부터 다시 받는다. 전체
  시간 제한은 없어(응답·조각마다 60초만 기다린다) ESXi가 바이트를 아주 느리게 보내면 작업이 오래 붙잡힌다.
- **비밀번호는 받은 노드의 메모리에만.** 비밀번호는 클러스터 상태·작업·로그에 남지 않고 `import esxi`를 받은 노드의
  메모리에만 있다. 실행 작업도 그 노드에서 돌며(실행 요청은 어느 노드로 보내도 된다) 끝나거나 가져오기를 지우면 잊는다.
  그 노드의 **opld를 다시 시작하면** 끝나지 않은 ESXi 가져오기는 `failed`("the ESXi credentials are no longer on
  this node (opld restarted or the request was interrupted); create the import again")가 된다 — 지우고 다시 만든다.
  그 노드가 꺼져 있을 때 `--force`로 지운 가져오기의 비밀번호는 그 opld가 다시 시작할 때까지 메모리에 남는다(쓰이지는
  않는다). ESXi 비밀번호를 바꾸면 남은 가져오기는 옛 비밀번호를 쓰므로 지우고 다시 만든다.
- **vCenter.** `--host`에 vCenter 주소, `--datacenter`에 데이터센터 이름(ESXi 단독 호스트는 기본값 `ha-datacenter`)을 준다.
  vCenter의 `/folder` 페이지는 ESXi의 것과 같은 모양이라고 보고 만들었고 실제 vCenter에서는 아직 시험하지 않았다.
- **분산 스위치·NSX.** vCenter 분산 스위치(VDS)의 NIC는 원본 네트워크가 포트 그룹 이름으로, `.vmx`에 이름이 없으면(보통은
  없다) `dvs:{포트 그룹 id}`로 보이고, NSX 네트워크의 NIC는 `nsx:{id}`로 보인다. 둘 다 경고가 나오고, 실행 때
  `--net "dvs:dvportgroup-1001=lan"`처럼 매핑한다.
- **권한과 연결.** 가져오기 쓰기 권한(operator 이상)이 있으면 opld 노드에서 아무 호스트:포트로 TLS 연결을 시도하게 할 수
  있다(지문을 믿기 전에도 인증서를 받으려고 연결한다). 관리자는 operator 권한을 그만큼 믿어야 한다(연결할 곳의 허용
  목록은 아직 없다).
- `ova`·`esxi`는 예약 이름이라 예약 전에 그 이름으로 만든 옛 가져오기는 이름이 아니라 id로 다룬다
  (`oplctl import get <id>`).
- CD-ROM·플로피·USB·사운드는 경고로 보이고 가져오지 않는다.

- **제안.** 게스트 OS(`vmw:osType`)가 Windows(`win…`)면 디스크 `sata`·NIC `e1000e`(Windows가 드라이버를 갖고 있다),
````

`README.md`에서

```markdown
  `node_offline`). 위트니스 노드에는 올릴 수 없다(400). 실행이 끝나면 원본을 지운다. 원본 노드가 클러스터에서 빠졌거나
  꺼진 채 돌아오지 않으면 `oplctl import delete web01 --force`로 기록만 지운다(원본 파일은 그 노드에 남는다).
- **볼륨.** 디스크마다 OVA에서 꺼내(`.mf`가 있으면 SHA-1·SHA-256·SHA-384·SHA-512를 확인한다; 모르는 알고리즘이거나
```

을 다음으로 바꾼다:

```markdown
  `node_offline`). 위트니스 노드에는 올릴 수 없다(400). 실행이 끝나면 원본을 지운다. 원본 노드가 클러스터에서 빠졌거나
  꺼진 채 돌아오지 않으면 `oplctl import delete web01 --force`로 기록만 지운다(원본 파일은 그 노드에 남는다). ESXi
  가져오기도 `import esxi`를 받은 노드에서 실행한다.
- **볼륨.** 디스크마다 OVA에서 꺼내(`.mf`가 있으면 SHA-1·SHA-256·SHA-384·SHA-512를 확인한다; 모르는 알고리즘이거나
```

`README.md`에서

```markdown
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
```

을 다음으로 바꾼다:

```markdown
- **아키텍처.** 가져온 VM은 x86 `q35`다. aarch64(macOS) 노드에도 가져올 수는 있지만 켜려면 x86_64 노드가 필요하다.
- 웹은 사이드바 "가져오기" 화면에서 한다(목록, "OVA 올리기" — 진행률, 이름은 파일 이름에서; "ESXi에서 가져오기" —
  호스트·포트·사용자·비밀번호·데이터센터로 연결 → 처음 보는 인증서면 지문을 보이고 "이 지문을 믿고 계속" → VM 고르기
  (잠금 경고, 읽지 못한 VM은 고를 수 없음) → 가져오기 이름 → 만들기, 비밀번호는 브라우저에 저장하지 않는다; 항목 화면 —
  원본 요약·경고, 이름·풀·CPU·메모리·펌웨어·디스크 버스·NIC 모델·네트워크 매핑·MAC 유지, 실행과 진행, 만든 VM 링크,
  지우기). 강제 지우기는 `oplctl`로 한다. API는 `POST /api/v1/vm-imports/ova/check?name=&size=`,
  `POST /api/v1/vm-imports/ova?name=`(본문 OVA, 201), `POST /api/v1/vm-imports/esxi/browse`(본문 연결
  `{"host", "port", "user", "password", "datacenter", "fingerprint"}`), `POST /api/v1/vm-imports/esxi`(`{"name", "connection",
  "datastore", "vmx"}`, 201), `GET /api/v1/vm-imports`, `GET`·`DELETE /api/v1/vm-imports/{name}`(`?force=true`),
  `POST /api/v1/vm-imports/{name}/run`(`{"name": …, "pool": …, "networks": {…}, …}`, 202 작업)이고, 읽기는 모든 사용자,
  올리기·ESXi 연결·실행·지우기는 operator 이상이다.
- **한계.** 실행 중인 VMware VM 옮기기, 증분 동기화, VMware 스냅샷 체인, VMware Tools 제거·virtio 드라이버 넣기(게스트에서
  한다), OVA 서명(`.cert`, 확인하지 않고 경고), 압축·분할 디스크, 여러 VM이 든 OVA, VM 일괄 가져오기, VDDK·NFC, 내려받기
  이어 받기·취소·전체 시간 제한은 없다. ESXi에서는 디스크립터가 따로 없는 VMDK·여러 extent·RDM을 받지 않는다.
- **남을 수 있는 것.** 실행이 볼륨을 다 만든 뒤 VM을 만들기 전에 opld가 죽으면 `{VM 이름}-disk{i}` 볼륨이 남는다(복제·복원과
```

`crates/opld/Cargo.toml`에서

```toml
oplctl.workspace = true
reqwest.workspace = true
serde_json.workspace = true
```

을 다음으로 바꾼다:

```toml
oplctl.workspace = true
rcgen.workspace = true
reqwest.workspace = true
rustls.workspace = true
rustls-pki-types.workspace = true
serde_json.workspace = true
```

`crates/opld/tests/vm_import.rs`에서

```rust
//! 만든다. 볼륨을 `qemu-img convert`로 raw로 빼서 표식이 있는지 본다. `.mf` 해시가 틀린 OVA는 실행이
//! 실패하고 볼륨을 남기지 않는다. 가져온 VM은 켜지 않는다(게스트 OS가 없고, aarch64 macOS 노드는 x86
//! 게스트를 돌리지 못한다).
//! `cargo test -p opld --features qemu-it --test vm_import`
```

을 다음으로 바꾼다:

```rust
//! 만든다. 볼륨을 `qemu-img convert`로 raw로 빼서 표식이 있는지 본다. `.mf` 해시가 틀린 OVA는 실행이
//! 실패하고 볼륨을 남기지 않는다. 시험 안에 띄운 가짜 ESXi(`/folder` 데이터스토어 브라우저)에서는
//! `oplctl import esxi-browse`·`import esxi`·`import run`으로 바로 가져오고, opld를 다시 시작하면 남은 ESXi
//! 가져오기가 자격 증명을 잃어 `failed`가 된다. 가져온 VM은 켜지 않는다(게스트 OS가 없고, aarch64 macOS
//! 노드는 x86 게스트를 돌리지 못한다).
//! `cargo test -p opld --features qemu-it --test vm_import`
```

`crates/opld/tests/vm_import.rs`에서

```rust
mod common;
#[allow(dead_code)]
```

을 다음으로 바꾼다:

```rust
mod common;
#[path = "../../opl-api/tests/common/esxi.rs"]
mod esxi;
#[allow(dead_code)]
```

`crates/opld/tests/vm_import.rs`에서

```rust

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use serde_json::{Value, json};
```

을 다음으로 바꾼다:

```rust

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Daemon, eventually};
use esxi::MockEsxi;
use serde_json::{Value, json};
```

`crates/opld/tests/vm_import.rs`에서

```rust
    assert!(!import_dir(&d, &import).exists(), "source directory left");
    d.stop();
}
```

을 다음으로 바꾼다:

```rust
    assert!(!import_dir(&d, &import).exists(), "source directory left");
    d.stop();
}

// --- ESXi에서 바로 가져오기 ---

const ESXI_USER: &str = "root";
const ESXI_PASSWORD: &str = "S3cret-esxi-pw";
const ESXI_MAC: &str = "00:50:56:aa:bb:cc";

/// 2 vCPU(소켓당 코어 2), 1024 MiB, efi, Linux, SCSI 디스크 `web01.vmdk`, "VM Network"의 vmxnet3 NIC
/// 하나([`ESXI_MAC`])
const WEB01_VMX: &str = r#".encoding = "UTF-8"
displayName = "web01"
guestOS = "ubuntu-64"
numvcpus = "2"
cpuid.coresPerSocket = "2"
memSize = "1024"
firmware = "efi"
scsi0.present = "TRUE"
scsi0.virtualDev = "pvscsi"
scsi0:0.present = "TRUE"
scsi0:0.fileName = "web01.vmdk"
ethernet0.present = "TRUE"
ethernet0.networkName = "VM Network"
ethernet0.virtualDev = "vmxnet3"
ethernet0.addressType = "static"
ethernet0.address = "00:50:56:aa:bb:cc"
"#;

/// 데이터스토어 `datastore1`의 VM `web01`: 디스크립터는 4 MiB(8192 섹터) VMFS 디스크이고, extent
/// `web01-flat.vmdk`는 앞 `MARK` 바이트가 `0xcd`인 4 MiB raw다.
async fn web01_esxi() -> MockEsxi {
    let mut flat = vec![0u8; DISK_BYTES];
    flat[..MARK].fill(0xcd);
    let descriptor = "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
                      createType=\"vmfs\"\n\n# Extent description\nRW 8192 VMFS \"web01-flat.vmdk\"\n";
    let files = BTreeMap::from([
        ("datastore1/web01/web01.vmx".to_string(), WEB01_VMX.into()),
        ("datastore1/web01/web01.vmdk".to_string(), descriptor.into()),
        ("datastore1/web01/web01-flat.vmdk".to_string(), flat),
    ]);
    MockEsxi::start(files, ESXI_USER, ESXI_PASSWORD).await
}

/// oplctl에 줄 ESXi 비밀번호 파일(`--password-file`). oplctl이 이 프로세스 안에서 돌므로 환경 변수
/// (`OPL_ESXI_PASSWORD`)는 바꾸지 않는다.
fn esxi_password_file(d: &Daemon) -> String {
    let file = d.path("esxi-password");
    std::fs::write(&file, ESXI_PASSWORD).unwrap();
    file.to_str().unwrap().to_string()
}

/// `import esxi`(가짜 ESXi의 `datastore1/web01/web01.vmx`)
async fn create_esxi(d: &Daemon, esxi: &MockEsxi, name: &str) {
    let (host, port) = (esxi.addr.ip().to_string(), esxi.addr.port().to_string());
    let password_file = esxi_password_file(d);
    let out = d
        .ctl(&[
            "import",
            "esxi",
            name,
            "--host",
            &host,
            "--port",
            &port,
            "--user",
            ESXI_USER,
            "--password-file",
            &password_file,
            "--datastore",
            "datastore1",
            "--vmx",
            "web01/web01.vmx",
            "--fingerprint",
            &esxi.fingerprint,
        ])
        .await
        .unwrap();
    assert_eq!(
        out,
        format!("vm-import {name} created; review it with `oplctl import get {name}`\n")
    );
}

/// 가짜 ESXi에서 바로 가져온다: 지문 없이 browse하면 서버 인증서 지문을 알려 주고, 그 지문으로 browse하면
/// `web01`이 보이고, 만들면 `.vmx`에서 제안이 나오고(비밀번호는 리소스에 없다), 실행하면 extent를 내려받아
/// 원본 MAC을 쓰는 꺼진 VM과 볼륨을 만든다. 그 뒤 하나 더 만들어 두고 opld를 다시 시작하면 그 가져오기는
/// 메모리의 자격 증명을 잃어 `failed`가 된다.
#[tokio::test]
async fn esxi_import_downloads_the_disk_and_restart_loses_credentials() {
    let mut d = Daemon::new("node1", "");
    setup(&mut d).await;
    let esxi = web01_esxi().await;
    let (host, port) = (esxi.addr.ip().to_string(), esxi.addr.port().to_string());
    let password_file = esxi_password_file(&d);
    let browse = [
        "import",
        "esxi-browse",
        "--host",
        &host,
        "--port",
        &port,
        "--user",
        ESXI_USER,
        "--password-file",
        &password_file,
    ];

    // 지문을 주지 않으면 서버 인증서의 지문을 알려 주고, ESXi에는 자격 증명을 보내지 않는다.
    let err = d.ctl(&browse).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "the ESXi certificate fingerprint is {0}; check it and rerun with --fingerprint {0}",
            esxi.fingerprint
        )
    );
    assert!(
        esxi.requests().iter().all(|r| r.authorization.is_none()),
        "{:?}",
        esxi.requests()
    );
    let mut with_fingerprint = browse.to_vec();
    with_fingerprint.extend(["--fingerprint", &esxi.fingerprint]);
    let table = d.ctl(&with_fingerprint).await.unwrap();
    let rows: Vec<Vec<&str>> = table
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(rows.len(), 2, "{table}");
    assert_eq!(
        rows[0],
        [
            "DATASTORE",
            "VMX",
            "NAME",
            "GUEST",
            "CPU",
            "MEMORY",
            "DISKS",
            "LOCKED"
        ]
    );
    assert_eq!(
        rows[1][..5],
        ["datastore1", "web01/web01.vmx", "web01", "ubuntu-64", "2"]
    );

    create_esxi(&d, &esxi, "web01").await;
    let import = d.ctl_json(&["import", "get", "web01"]).await;
    assert_eq!(import["status"]["phase"], "ready", "{import}");
    let source = &import["spec"]["source"]["esxi"];
    assert_eq!(
        (
            &source["host"],
            &source["datastore"],
            &source["vmx"],
            &source["user"]
        ),
        (
            &json!(host),
            &json!("datastore1"),
            &json!("web01/web01.vmx"),
            &json!(ESXI_USER)
        ),
        "{import}"
    );
    assert_eq!(source["fingerprint"], esxi.fingerprint.as_str(), "{import}");
    assert!(!import.to_string().contains(ESXI_PASSWORD), "{import}");
    assert_eq!(
        import["status"]["source_vm"]["disks"][0]["capacity_bytes"], DISK_BYTES,
        "{import}"
    );
    let text = d.ctl(&["import", "get", "web01"]).await.unwrap();
    assert!(
        text.contains(
            "proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs\n"
        ),
        "{text}"
    );
    let list = d.ctl(&["import", "list"]).await.unwrap();
    let source_url = format!("esxi://{host}/datastore1/web01/web01.vmx");
    assert!(
        list.lines()
            .nth(1)
            .is_some_and(|l| l.split_whitespace().collect::<Vec<_>>()
                == ["web01", "node1", source_url.as_str(), "ready", "-"]),
        "{list}"
    );

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
    assert_eq!(spec["memory_mib"], 1024, "{vm}");
    assert_eq!(
        (&spec["cpu"]["sockets"], &spec["cpu"]["cores"]),
        (&json!(1), &json!(2))
    );
    assert_eq!(spec["disks"][0]["volume"], "web-disk0", "{vm}");
    assert_eq!(spec["disks"][0]["bus"], "virtio-blk", "{vm}");
    let nics = spec["nics"].as_array().unwrap();
    assert_eq!(nics.len(), 1, "{vm}");
    assert_eq!(nics[0]["network"], "lan", "{vm}");
    assert_eq!(nics[0]["model"], "virtio", "{vm}");
    assert_eq!(nics[0]["mac"], ESXI_MAC, "{vm}");

    // 볼륨에 extent의 내용이 있다. extent는 비밀번호와 함께 한 번 내려받았다.
    let volume = d.ctl_json(&["volume", "get", "web-disk0"]).await;
    assert_eq!(volume["spec"]["size_bytes"], DISK_BYTES, "{volume}");
    let file = volume_file(&d, "web-disk0").await;
    assert_eq!(head(&d, &file), vec![0xcd; MARK]);
    let downloads: Vec<_> = esxi
        .requests()
        .into_iter()
        .filter(|r| r.path == "/folder/web01/web01-flat.vmdk")
        .collect();
    assert_eq!(downloads.len(), 1, "{downloads:?}");
    assert_eq!(
        downloads[0].authorization.as_deref(),
        Some(opl_api::vm_imports::esxi::basic_auth(ESXI_USER, ESXI_PASSWORD).as_str())
    );

    let done = d.ctl_json(&["import", "get", "web01"]).await;
    assert_eq!(done["status"]["phase"], "done", "{done}");
    assert_eq!(done["status"]["vm"], vm["id"], "{done}");
    assert!(done["status"]["error"].is_null(), "{done}");
    assert!(!import_dir(&d, &done).exists(), "import directory left");

    // 하나 더 만들어 두고 opld를 다시 시작하면 메모리의 비밀번호가 사라져 그 가져오기는 실패한다.
    create_esxi(&d, &esxi, "web02").await;
    d.stop();
    d.start().await;
    eventually("web02 failed after the restart", || async {
        d.ctl_json(&["import", "get", "web02"]).await["status"]["phase"] == "failed"
    })
    .await;
    let failed = d.ctl_json(&["import", "get", "web02"]).await;
    assert_eq!(
        failed["status"]["error"],
        "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again",
        "{failed}"
    );
    // 끝난 가져오기는 그대로다.
    let done = d.ctl_json(&["import", "get", "web01"]).await;
    assert_eq!(done["status"]["phase"], "done", "{done}");

    d.ctl(&["import", "delete", "web02"]).await.unwrap();
    d.ctl(&["import", "delete", "web01"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "web-disk0"]).await.unwrap();
    d.stop();
}
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 8은 계획 29(OVA 가져오기: `VmImport` 리소스, OVA 올리기와 손으로 읽는 tar·OVF 해석, 원본 VM에서 opLite VM 제안(게스트 OS로 디스크 버스·NIC 모델, 코어 나누기, 경고), 사용자가 고친 값으로 디스크를 `.mf` 해시를 보며 추출해 볼륨으로 옮기고 꺼진 VM을 만드는 `VmImport` Task, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 30(ESXi·vCenter에서 바로 가져오기: 지문을 확인하는 HTTPS 연결, 데이터스토어의 `.vmx`·디스크립터 해석, 같은 `VmImport`와 Task, 예정)으로 나눴다(`2026-11-04-vmware-import-design.md`). 하위 프로젝트 8은 진행 중이다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 8은 계획 29(OVA 가져오기: `VmImport` 리소스, OVA 올리기와 손으로 읽는 tar·OVF 해석, 원본 VM에서 opLite VM 제안(게스트 OS로 디스크 버스·NIC 모델, 코어 나누기, 경고), 사용자가 고친 값으로 디스크를 `.mf` 해시를 보며 추출해 볼륨으로 옮기고 꺼진 VM을 만드는 `VmImport` Task, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 30(ESXi·vCenter에서 바로 가져오기: 서버 인증서 지문을 고정한 HTTPS 연결과 데이터스토어 브라우저(`/folder`) 목록, 데이터스토어의 `.vmx`·VMDK 디스크립터 해석, 비밀번호는 받은 노드의 메모리에만, 같은 `VmImport`와 Task에 extent 내려받기 단계, API·oplctl·웹, 가짜 ESXi로 실제 시험, 완료)으로 나눴다(`2026-11-04-vmware-import-design.md`). 하위 프로젝트 8은 끝났다. 실행 중인 VM 옮기기, 증분 동기화, VMware 스냅샷 체인, VDDK·NFC, 게스트 안 드라이버 바꾸기는 넣지 않았다.
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown
- 인증서는 시스템 CA로 검증하지 않는다(ESXi는 거의 자체 서명이다). 대신 TLS 핸드셰이크 안에서 서버 인증서의 SHA-256 지문을 `fingerprint`와 비교한다. 지문이 다르면 자격 증명을 보내기 전에 끊는다. `fingerprint`가 없으면 연결만 해 보고 409 `untrusted_certificate`(`details.fingerprint`)를 돌려준다. 사용자가 지문을 확인해 다시 보낸다(웹은 확인 대화상자).
- 비밀번호는 Raft 상태·Task·로그에 남기지 않는다. 요청을 받은 노드의 메모리(`ImportCredentials`, 가져오기 id → 연결)에만 둔다. 그 노드가 다시 시작하면 그 노드의 ESXi 가져오기는 실행할 수 없다(`failed`, "credentials were lost when opld restarted; create the import again").
```

을 다음으로 바꾼다:

```markdown
- 인증서는 시스템 CA로 검증하지 않는다(ESXi는 거의 자체 서명이다). 대신 TLS 핸드셰이크 안에서 서버 인증서의 SHA-256 지문을 `fingerprint`와 비교한다. 지문이 다르면 자격 증명을 보내기 전에 끊는다. `fingerprint`가 없으면 연결만 해 보고 409 `untrusted_certificate`(`details.fingerprint`)를 돌려준다. 사용자가 지문을 확인해 다시 보낸다(웹은 확인 대화상자).
- 비밀번호는 Raft 상태·Task·로그에 남기지 않는다. 요청을 받은 노드의 메모리(`ImportCredentials`, 가져오기 id → 연결)에만 둔다. 그 노드가 다시 시작하면 그 노드의 ESXi 가져오기는 실행할 수 없다(`failed`, "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again").

구현(계획 30): 타입은 `opl_types::vm_import`의 `ImportSource::Esxi(EsxiSource { host, port, datacenter, datastore, vmx(데이터스토어 안 경로 "web01/web01.vmx"), user, fingerprint(정규화한 지문) })`(JSON `{"esxi": {…}}`, 비밀번호 없음)와 요청 본문용 `EsxiConnection`(`deny_unknown_fields`, Deserialize만 — oplctl·시험은 JSON을 직접 만든다, Debug에서 비밀번호를 가림), `normalize_fingerprint`(SHA-256 hex 64자, 콜론·공백 허용, 대소문자 무시 → 대문자 콜론 구분 `AB:CD:…`, ESXi 화면과 같은 모양). 클라이언트는 opl-api `vm_imports/esxi.rs`이고 새 패키지 없이 이미 Cargo.lock에 있던 `rustls`(ring)·`tokio-rustls`·`hyper` 1·`hyper-util`·`http-body-util`을 워크스페이스 직접 의존성으로 올려 만든다(reqwest의 TLS 기능은 그대로). TCP 연결 15초 → 사용자 정의 `ServerCertVerifier`가 끝 인증서 DER의 SHA-256을 기대 지문과 비교하고(다르면 핸드셰이크 실패 — Authorization을 보내기 전, 지문이 없으면 `Untrusted(실제 지문)`), 서명 검증은 ring provider의 `verify_tls12/13_signature`, SNI는 host(IP면 없음, `[::1]`처럼 괄호로 적은 IPv6는 괄호를 떼고 연결하고 Host 헤더에만 괄호). 요청마다 새 HTTP/1.1 연결(`hyper::client::conn::http1`)과 `Authorization: Basic`, 응답·본문 조각마다 60초 시간 제한. 401 → `Auth`("ESXi rejected the user name or password"), 404 → `NotFound`, 그 밖 4xx/5xx → `Http`, 연결·TLS 실패 → `Connect`, 지문 불일치 → `Fingerprint { expected, actual }`. API 오류는 `untrusted_certificate`·`fingerprint_mismatch` 409(`details.fingerprint` = 서버 인증서의 지문), `esxi_auth` 400, `esxi_unreachable` 502, 없는 파일·그 밖의 HTTP 응답·크기 초과(`TooLarge { path, limit }`)·잘못된 응답 400. 모양이 틀린 지문은 browse·만들기 모두 연결하기 전에 400 `invalid_request`("invalid fingerprint …"). 자격 증명은 crate 안 `ImportCredentials`(`AppState.import_credentials`, `Mutex<HashMap<가져오기 id, EsxiConnection>>`, opld가 API와 `TaskRunnerConfig`에 같은 `Arc`를 넘긴다)에만 있고, 가져오기 리소스를 만들기 전에 넣는다(만들자마자 다른 노드로 온 실행 요청의 Task도 찾게) — 만들기가 실패하거나 요청 future가 버려지면 drop guard(`CredentialGuard`)가 지운다. 노드 시작 정리(`vm_imports::sweep`)는 이 노드의 `ready` ESXi 가져오기를 `failed` + 위 오류로 바꾼다("source files are gone" 규칙은 OVA만 — ESXi 가져오기는 실행할 때만 `imports/{id}` 디렉터리를 쓰므로 디렉터리는 보지 않는다).
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown
`POST /vm-imports/esxi/browse`(본문: 연결) → 데이터스토어 목록(`/folder?dcPath=…`)과 각 데이터스토어 맨 위 디렉터리의 `.vmx` 경로. 각 `.vmx`를 읽어(동시 8개, 전체 500개까지) `{ datastore, vmx, name, guest_os, vcpus, memory_mib, disks, locked }`를 돌려준다. `locked`는 디렉터리에 `.lck` 항목이 있다는 뜻(켜져 있을 수 있음, 웹은 경고).
```

을 다음으로 바꾼다:

```markdown
`POST /vm-imports/esxi/browse`(본문: 연결) → 데이터스토어 목록(`/folder?dcPath=…`)과 각 데이터스토어 맨 위 디렉터리의 `.vmx` 경로. 각 `.vmx`를 읽어(동시 8개, 전체 500개까지) `{ datastore, vmx, name, guest_os, vcpus, memory_mib, disks, locked }`를 돌려준다. `locked`는 디렉터리에 `.lck` 항목이 있다는 뜻(켜져 있을 수 있음, 웹은 경고).

구현(계획 30): `esxi::list(conn, Option<데이터스토어>, dir)`는 `/folder?dcPath=…`(데이터스토어 목록)·`/folder/{경로}?dcPath=…&dsName=…`(퍼센트 인코딩, `/` 유지)의 HTML(1 MiB까지)에서 `<a href>`만 본다: href가 `/folder`로 시작하고 지금 경로 바로 아래인 것(위로 가는 "Parent Datacenter"·"Parent Directory" 제외), HTML 엔티티(`;`는 `&` 뒤 10바이트 안에서만 찾아 큰 입력에도 한 번만 훑는다)·퍼센트 인코딩(쿼리 값의 `+`는 공백)을 풀고 공백 이름도 받는다, 중복은 HashSet으로 뺀다. 목록에는 파일·디렉터리 구분이 없어 아는 확장자(`.vmx`·`.vmdk`·`.nvram`·`.log`·`.lck`·`.vmsd`·`.vmxf`·`.vswp`·`.vmss`·`.vmsn`·`.vmem` 등 20개)는 파일, 그 밖은 디렉터리로 보고, 디렉터리로 본 것을 읽었는데 HTML이 아니면 건너뛴다. browse(`POST /vm-imports/esxi/browse`, `Kind::VmImport` 쓰기)는 데이터스토어마다 맨 위 디렉터리를 보고(합쳐 500개까지 — 넘으면 조용히 자른다), 디렉터리마다 첫 `.vmx`를 동시 8개로 `get_small`(64 KiB까지)해 `vmx_to_source`로 요약한다(`EsxiVm { vmx, name, guest_os, vcpus, memory_mib, disks, locked, error }`; 해석 실패·없는 파일은 그 VM의 `error`, 연결·인증·지문 오류는 요청 전체의 오류). 켜짐은 디렉터리의 항목으로 짐작한다: 이름이 `.lck`로 끝나거나(디렉터리 `.vmx.lck`여도) `.lck-`로 시작하거나(NFS 잠금) `.vswp`로 끝나면(켜진 VM만 가지는 스왑 파일, VMFS·NFS 모두) `locked`이고, 일시 중단된 VM의 `.vmss`가 있어도 `locked`다. 목록을 읽지 못한 데이터스토어·디렉터리는 건너뛰고 로그(info)에만 남긴다.
```

`docs/superpowers/specs/2026-11-04-vmware-import-design.md`에서

```markdown

### 3.4 실행

같은 `POST /vm-imports/{key}/run`과 Task `VmImport`. 디스크를 얻는 1단계만 다르다: extent를 HTTPS로 받아 `disk.raw`로 쓰고(스트리밍, 받은 바이트 수가 디스크립터 크기와 같아야 한다) raw로 `import_volume`. 씬 디스크도 전체 크기로 내려오므로 업로드 디렉터리 여유는 디스크 크기만큼 필요하다(`qemu-img convert`가 0을 건너뛰어 풀에는 쓴 만큼만 남는다). 지금의 볼륨 경로·`Made` 정리는 같다.
```

을 다음으로 바꾼다:

```markdown

구현(계획 30): opl-api `vm_imports/vmx.rs`(순수). `parse_vmx`는 64 KiB까지, 줄마다 `key = "value"`(따옴표 없는 값도, `#` 주석), 키는 소문자로 맞추고 VMware 이스케이프 `|XX`(16진 바이트)를 푼다. `vmx_to_source(&Vmx, vmx 경로)`는 `displayName`(없으면 파일 이름), `guestOS`, `numvcpus`(기본 1), `cpuid.coresPerSocket`(기본 1), `memSize`(필수), `firmware`(`efi` → UEFI)와 `present = "TRUE"`인 `scsiN:M`·`sataN:M`·`nvmeN:M`·`ideN:M` 디스크(`deviceType`이 없거나 `disk`·`scsi-hardDisk`·`ata-hardDisk`, `fileName`이 `.vmdk`), CD-ROM은 경고 "CD-ROM drive is not imported"(한 번), 그 밖 종류는 "… is not imported", 플로피·USB·사운드 경고. 디스크 순서는 `bios.hddOrder`에 적힌 것이 먼저, 그 밖은 ide·sata·scsi·nvme(부트 순서를 지키려고) → 버스 → 유닛이고, `hddOrder` 없이 컨트롤러가 둘 이상이면 OVA와 같은 경고. NIC는 `ethernetN.present = "TRUE"`(N 순): `networkName`, 없으면 vCenter 분산 스위치의 `dvs.portgroupName`(없으면 `dvs:{portgroupId}`) + 경고 "ethernetN is on a distributed switch; map its network when running the import", 그것도 없고 NSX의 `opaqueNetwork.id`가 있으면 `nsx:{id}` + 경고 "ethernetN is on an NSX network; map its network when running the import", 모두 없으면 400; `virtualDev` 기본 `e1000`; MAC은 `addressType = "static"`이면 `address`, 아니면 `generatedAddress`. 디스크 32개·NIC 16개 제한(OVA와 같은 문구 모양). `parse_descriptor`(64 KiB, 바이너리면 "not a VMDK descriptor")·`check_flat`(`vmfs`·`monolithicFlat`이고 offset 0인 extent 하나 → (extent 이름, 섹터 × 512); `parentFileNameHint` 또는 델타·스파스 종류 → "the VM has snapshots; consolidate or delete them in VMware first", RDM → "raw device mappings are not supported", 그 밖 "unsupported VMDK type X", extent 이름에 `/`·`\` 금지, 이름 전체가 `.`·`..`이면 금지 — 이름 안의 `..`는 된다), `disk_path`(`.` 조각은 뺀다; `.vmx` 디렉터리 기준 상대 경로 또는 `/vmfs/volumes/{같은 데이터스토어}/…`; 다른 데이터스토어 "disk X is on another datastore", UUID 볼륨 경로·`..`는 400). 만들기 API(`POST /vm-imports/esxi`, 본문 `{ name, connection, datastore, vmx }`, 지문 필수)는 OVA와 같은 이름 검사(예약 이름 `ova`·`esxi`, 위트니스 400, `name_taken`)를 하고, `.vmx` → 디스크마다 디스크립터를 읽어 `SourceDisk { file: 데이터스토어 안 extent 경로 "web01/web01-flat.vmdk", capacity_bytes: extent 바이트 }`를 채운 뒤 `propose`한다(SATA 개수 검사 때문에 디스크를 채운 다음). 디스크립터가 64 KiB를 넘으면(`TooLarge`만 — 디스크립터가 따로 없는 스파스·호스트형 VMDK) 400 "… is not a VMDK descriptor; only VMFS and flat disks can be imported". `.vmx` 디렉터리에 위의 잠금·스왑 항목이 있으면 맨 앞 경고 "the VM looks powered on (lock files found); power it off before importing", `.vmss`가 있으면 경고 "the VM is suspended; power it off in VMware before importing (its memory state is not imported)"(둘 다면 잠금 경고가 먼저). `spec.node`는 받은 노드다.

### 3.4 실행

같은 `POST /vm-imports/{key}/run`과 Task `VmImport`. 디스크를 얻는 1단계만 다르다: extent를 HTTPS로 받아 `disk.raw`로 쓰고(스트리밍, 받은 바이트 수가 디스크립터 크기와 같아야 한다) raw로 `import_volume`. 씬 디스크도 전체 크기로 내려오므로 업로드 디렉터리 여유는 디스크 크기만큼 필요하다(`qemu-img convert`가 0을 건너뛰어 풀에는 쓴 만큼만 남는다). 지금의 볼륨 경로·`Made` 정리는 같다.

구현(계획 30): 실행 API는 계획 29 그대로이고, 여유 검사만 ESXi면 가장 큰 디스크의 `capacity_bytes` + `UPLOAD_RESERVE_BYTES`(507). 실행 요청은 어느 노드로 와도 되고 Task는 원본 노드(`spec.node`)에서 돌며 그 노드 메모리의 자격 증명을 쓴다. 요청을 받은 노드가 원본 노드인데 자격 증명이 없으면 Task를 만들지 않고 가져오기를 `failed` + `CREDENTIALS_LOST`로 바꾸고 409 `import_failed`. `tasks/import.rs`의 ESXi 경로: Task 시작 때 자격 증명이 없으면 실패하고 정리가 가져오기를 `failed` + `CREDENTIALS_LOST`로 바꾼다(다시 실행해도 소용없다). 디스크마다 업로드 디렉터리 여유(용량 + 1 GiB) → `esxi::download`(스트리밍으로 `imports/{id}/disk.raw`에 쓰고 fsync, 받은 길이가 다르면 파일을 지우고 `downloaded N bytes of [ds] path, expected M`) → `check_import_source(Raw)` → 볼륨(`SourceFormat::Raw`로 `import_volume`) → `disk.raw` 지움. VM 만들기·`done`·실패 정리·MAC 확인은 OVA와 같은 코드다. 자격 증명은 `done`, 지우기 Task, 끝난 가져오기를 `done`으로 맞출 때 지우고, 실패해 `ready`로 돌아가면 남긴다(값을 바꿔 다시 실행한다). 내려받기 중 취소는 없다(Task 취소가 없다).

구현(계획 30) oplctl·웹: `oplctl import esxi-browse --host H --user U [--port 443] [--datacenter ha-datacenter] [--fingerprint F]`(표 `DATASTORE VMX NAME GUEST CPU MEMORY DISKS LOCKED`, 읽지 못한 VM은 NAME 자리에 `error: …`; 409 `untrusted_certificate`면 "the ESXi certificate fingerprint is AB:…; check it and rerun with --fingerprint AB:…", 종료 코드 1), `import esxi NAME --host --user --datastore --vmx --fingerprint [--port] [--datacenter]`(출력 ``vm-import NAME created; review it with `oplctl import get NAME` ``). 비밀번호는 인자로 받지 않고 `--password-file`, 없으면 `OPL_ESXI_PASSWORD`, 그것도 없으면 터미널 프롬프트(`rpassword`). `import list`·`get`의 SOURCE는 `esxi://host/datastore/vmx`. API 오류는 oplctl 안에서 코드·details가 있는 `ApiError`다. 웹은 `/imports`의 "ESXi에서 가져오기"(쓰기 권한) 대화상자 `EsxiImportDialog`: 연결(호스트·포트·사용자·비밀번호·데이터센터 — 포트·데이터센터도 늘 보낸다) → `untrusted_certificate`면 서버가 알려 준 지문을 보이고 "이 지문을 믿고 계속" → VM 표(잠금 경고 아이콘, 읽지 못한 VM은 고를 수 없음) → VM 이름에서 이름 규칙으로 만든 가져오기 이름(맞출 수 없으면 `.vmx` 디렉터리 이름, 그래도 안 되면 `esxi-import`; 사용자가 고친 이름은 고른 VM이 바뀔 때만 다시 제안) → 만들기 → `/imports/:name`. 지문을 믿은 뒤의 인증·연결 오류, 만들기의 `fingerprint_mismatch`, 목록의 "뒤로"는 연결 단계로 돌아가고, 믿은 `{host, port, 지문}`은 같은 host:port로 다시 연결할 때 함께 보낸다(`fingerprint_mismatch`면 버린다). 만들기의 `name_taken`은 이름 칸 오류. 비밀번호는 컴포넌트 상태와 요청 본문에만 있다. 목록·상세의 원본 표시는 `host / datastore / vmx`, 자격 증명 사라짐 오류는 안내 문장으로, 오류 코드 문구 ko·en.

구현(계획 30) 시험: opl-types 단위(직렬화, 지문 정규화 표), opl-api `vmx.rs` 단위(ESXi 7·8 모양 `.vmx` 견본 `tests/fixtures/esxi{7,8}.vmx`, 디스크 순서·`hddOrder`, MAC, 제한, 디스크립터 표, 경로), 가짜 ESXi `crates/opl-api/tests/common/esxi.rs`(axum + `opl_peer::tls::TlsListener` + rcgen 자체 서명, ESXi 7/8 모양 `/folder` HTML, 기본 인증·401, 요청 기록 — opld 시험도 `#[path]`로 쓴다), `tests/esxi.rs`(probe, 목록·파일·내려받기, 틀린 지문이면 서버가 Authorization을 받지 않음, 401·404, 크기 제한, 길이 불일치면 파일 없음, 목록 해석), `tests/vm_imports.rs`(browse·만들기 오류와 details, 리소스 JSON에 비밀번호 문자열이 없음, 스냅샷·다른 데이터스토어 400, sweep), `tests/vm_import_tasks.rs`(`storage-it`: 가져오기·길이 불일치·자격 증명 없음·실행 507), oplctl 단위, 웹 시험. 실제 시험 `crates/opld/tests/vm_import.rs`의 `esxi_import_downloads_the_disk_and_restart_loses_credentials`(`qemu-it`; opld dev-dependencies에 rcgen·rustls·rustls-pki-types, 비밀번호는 `--password-file`로 — oplctl이 시험 프로세스 안에서 돌므로 환경 변수를 바꾸지 않는다; CI linux-kvm은 `-p opld`의 모든 시험을 돌리므로 바꿀 것이 없다): 시험 안의 가짜 ESXi에 `datastore1/web01/`(`.vmx` — 2 vCPU·소켓당 코어 2·1024 MiB·efi·`ubuntu-64`·`scsi0:0` `web01.vmdk`·"VM Network"의 vmxnet3 NIC와 static MAC `00:50:56:aa:bb:cc`; 디스크립터 `createType="vmfs"`·`RW 8192 VMFS "web01-flat.vmdk"`; 앞 64 KiB에 표식을 쓴 4 MiB raw extent). `import esxi-browse` 지문 없이 → 지문 안내 오류이고 가짜 ESXi는 Authorization을 하나도 받지 않음, 지문과 함께 → 표에 `datastore1 web01/web01.vmx web01 ubuntu-64 2` → `import esxi web01` → `import get`(phase `ready`, `spec.source.esxi`에 host·datastore·vmx·user·지문, 리소스에 비밀번호 없음, 디스크 4 MiB, 제안 `1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs`), 목록 SOURCE `esxi://…` → `import run web01 --name web --pool local --net lan` → 꺼진 VM(uefi, 1×2, `web-disk0` virtio-blk, `lan` virtio, MAC `00:50:56:aa:bb:cc`), 볼륨 4 MiB이고 앞 64 KiB가 표식, extent를 기본 인증으로 한 번 받음, `done`·`vm`, 가져오기 디렉터리 없음. 그 뒤 `web02`를 만들어 두고 opld를 멈췄다 다시 켜면 `web02`는 `failed` + "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again"이고 `web01`은 `done` 그대로다.

알려진 틈(계획 30): 비밀번호는 받은 노드의 메모리에만 있으므로 그 opld가 다시 시작하면 그 노드의 끝나지 않은 ESXi 가져오기는 모두 `failed`가 된다(지우고 다시 만든다). 원본 노드가 오프라인일 때 `--force`로 지우면 리소스만 지우므로 비밀번호는 그 노드의 메모리에 그 opld가 다시 시작할 때까지 남는다(쓰이지는 않는다). ESXi 비밀번호를 바꾸면 남은 가져오기는 옛 비밀번호를 쓰므로 지우고 다시 만든다. 내려받기는 취소할 수 없고(Task 취소가 없다), 끊기면 처음부터 다시 받는다(이어 받기 없음). 내려받기 전체 시간 제한이 없어(응답·본문 조각마다 60초만) 바이트가 아주 느리게 오면 Task가 오래 붙잡힌다. 씬 디스크도 전체 크기로 내려오므로 업로드 디렉터리에 가장 큰 디스크 용량 + 1 GiB의 여유가 있어야 하고 시간도 전체 크기만큼 걸린다. ESXi·vCenter의 `/folder` HTML은 문서·화면을 보고 만든 가짜 ESXi로만 시험했다 — 실제 ESXi·vCenter에서는 시험하지 않았고, 특히 vCenter(`dcPath`에 데이터센터 이름, 데이터스토어가 여러 호스트에 걸침)는 ESXi 모양을 따른다고 가정했다. 전원 상태는 `.lck`·`.lck-`·`.vswp`(일시 중단은 `.vmss`) 항목으로 짐작할 뿐이라(경고만) 켜진 VM도 가져올 수 있고 그러면 디스크가 일관되지 않다. 분산 스위치 NIC는 포트 그룹 이름으로 보이지만 `.vmx`에는 보통 이름이 없어 `dvs:{portgroupId}`로 보이고, NSX NIC는 `nsx:{id}`로 보이므로 실행 때 매핑해야 한다. 가져오기 쓰기 권한(operator)이 있으면 노드에서 아무 host:port로 TLS 연결을 시도하게 할 수 있다 — 관리자는 operator 권한을 그만큼 믿어야 한다(연결할 곳의 허용 목록은 나중). `ova`·`esxi`는 예약 이름이라 예약 전에 그 이름으로 만든 옛 가져오기는 id로만 다룬다. browse는 데이터스토어 맨 위 디렉터리만, 디렉터리마다 첫 `.vmx`만, 합쳐 500개까지 보고 넘으면 경고 없이 자르며, 목록을 읽지 못한 데이터스토어·디렉터리는 로그에만 남기고 건너뛴다. 디스크립터가 따로 없는 VMDK(호스트형 스파스)·여러 extent·RDM·스냅샷 체인은 받지 않는다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1130개, 실패 0

Run: `cargo test -p opld --features qemu-it --test vm_import`
Expected: PASS 3개(이 Mac, 실제 opld·qemu-img)

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock README.md crates docs
git commit -m "test(opld): ESXi에서 바로 가져오기 E2E와 문서 — 실제 opld와 시험 안의 가짜 ESXi로 import esxi-browse(지문 없으면 지문 안내 오류이고 자격 증명을 보내지 않음, 지문과 함께 표에 web01)·import esxi·import get(제안, 리소스에 비밀번호 없음)·import run(extent를 내려받아 꺼진 VM, 볼륨 표식, 원본 MAC, done), opld를 다시 시작하면 남은 ESXi 가져오기가 failed(자격 증명 사라짐), opld dev-dependencies에 rcgen·rustls·rustls-pki-types, README \"VMware에서 가져오기\"에 ESXi·vCenter에서 바로 가져오기(지문 확인, 끄기·스냅샷 통합, 씬 디스크 전체 크기와 여유, 분산 스위치 매핑, 비밀번호는 받은 노드 메모리에만, vCenter 데이터센터 이름과 실제 vCenter 미시험, oplctl·웹), 설계 \"구현(계획 30)\" 메모와 알려진 틈, 하위 프로젝트 8 완료"
```

---

## 완료 기준 (계획 30)

- ESXi(가짜 서버)에 지문 없이 연결하면 서버 지문을 알려 주고 자격 증명은 보내지 않으며, 확인한 지문으로 연결하면 데이터스토어의 VM 목록이 나온다.
- ESXi VM으로 가져오기를 만들면 원본 VM·제안·경고가 있는 `ready` 가져오기가 생기고, 리소스·Task 어디에도 비밀번호가 없다. 스냅샷·RDM·다른 데이터스토어 디스크는 400이다.
- 실행하면 디스크가 HTTPS로 내려받아져 풀의 볼륨이 되고(내용이 같다) 원본 MAC을 가진 멈춘 VM이 만들어지며, 가져오기는 `done`이 되고 자격 증명이 지워진다.
- opld가 다시 시작하면 그 노드의 ESXi 가져오기는 `failed`가 되고 다시 만들라는 안내가 나온다.
- 이 Mac과 Linux CI에서 `opl-api` `vm_import_tasks`(storage-it)와 `opld` `vm_import`(qemu-it) 시험이 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`, 웹 `typecheck`·`lint`·`test`·`build`가 통과하고 `gen:api` 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **실제 ESXi·vCenter로 시험하지 않았다.** `/folder` 페이지 모양은 ESXi 7/8을 본뜬 가짜 서버로만 확인했다.
- **씬 디스크도 전체 크기로 내려받고**, 내려받기에는 전체 시간 제한·취소·이어받기가 없다.
- **operator는 노드에서 임의 host:port로 TLS 연결을 시도할 수 있다**(허용 목록은 나중).
- 그 밖의 제한은 설계 문서 §3과 "알려진 틈(계획 30)", README "VMware에서 가져오기" 절에 있다.

## 실행 후 기록 (2026-11-05)

7개 Task를 계획대로 적용했다. Task마다 검증된 단계와 트리가 같았고, 생성 파일 `Cargo.lock`·`schema.d.ts`도 같았다. 실행 전에 계획 문서를 줄 단위 적용으로 처음부터 끝까지 적용해, 각 단계의 트리와 같음을 확인했다. 계획 문서를 만들기 전에 스테이지 작업에서 리뷰를 두 차례 돌리고 CI에서 Linux로 확인해 고쳤다. 고친 내용은 위 Task의 코드에 이미 들어 있다.

- **1차 리뷰에서 고친 것:**
  - 켜짐 감지: `.lck`만으로는 VMFS에서 거의 걸리지 않아 `.vswp`·`.lck-`를 더함.
  - 자격 증명이 남는 문제: 만들기 실패나 요청 중단에도 지우도록 drop guard를 두고, 자격 증명 없이 실행하면 Task 없이 409로 `failed` 처리.
  - 목록 해석의 HTML 엔티티 처리가 입력 크기의 제곱으로 느려질 수 있던 것과 쿼리 값의 `+`.
  - `TooLarge` 오류 변형.
  - 지문 형식 검사(400).
  - NSX NIC(`nsx:{id}`).
  - `..` 검사.
  - 시험의 `unsafe set_var` 대신 `--password-file`.
  - 웹: 신뢰 단계에서 막히던 것, 만들기 때 지문 불일치, 뒤로 가기, OVA를 전제한 문구를 중립으로, 접근성.
  - 문서: 시간 제한·취소 없음, 씬 디스크 전체 크기, operator의 임의 host:port 연결.
- **2차 리뷰:** 남은 중요한 문제는 없었다. 자격 증명 사라짐 문구를 중립으로 바꾸고, `.vmss`를 켜짐 감지에 넣고, 신뢰 단계에 뒤로 가기를 두고, IPv6 호스트를 대괄호로 감싸고, 경로의 `.` 조각을 처리했다.
- **CI:** Linux CI에서 원래 있던 opld 스냅샷 시험(실행 중 VM의 QMP `query-jobs` 시간 초과)이 한 번 실패했다. 다시 돌려 통과했고, 이 계획과는 관계없다.

**테스트:**
- macOS `cargo test --workspace` 1130개
- `opl-api` `storage-it` `vm_import_tasks` 7개
- `opld` `qemu-it` `vm_import` 3개(실제 opld·qemu-img, 가짜 ESXi 포함)
- 웹 Vitest 422개와 빌드
- `fmt`·`clippy -D warnings`(모든 기능), `cargo deny`

Linux CI에서도 같은 시험이 통과했다.

## 다음 계획으로 넘길 사항

- 하위 프로젝트 8(VMware 마이그레이션 마법사)은 이 계획으로 끝났다. 다음은 하위 프로젝트 9(설치 ISO, 업데이트 채널, 라이선스, 텔레메트리)다.
- 남은 가져오기 과제:
  - 실제 ESXi·vCenter에서의 시험.
  - 씬 디스크를 성기게 쓰기.
  - 내려받기 진행률·취소·전체 시간 제한·이어받기.
  - vCenter 데이터센터 이름 찾기.
  - 연결 대상 허용 목록.
  - 켜짐 감지를 API로 하기.
  - 스냅샷이 있는 VM 통합 안내.
  - 계획 29에서 넘어온 과제: OVA 서명 검증, `failed` 가져오기 살리기, 풀 용량 미리 보기, 웹 강제 지우기.
