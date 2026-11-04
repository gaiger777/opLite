# opLite VMware 가져오기 설계 (하위 프로젝트 8: 계획 29·30)

작성일: 2026-11-04
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해 §1.1, 이미지 가져오기 §4·§6), `2026-11-04-backup-design.md`(새 VM을 만드는 Task, 미리 정한 VM id)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 8은 VMware 마이그레이션 마법사(Proxmox Import Wizard, VMware Converter)다. 1차 목표 시장이 VMware 이탈 기업이므로 VMware VM을 opLite VM으로 옮기는 길이 제품의 입구다. 지금은 디스크 하나(monolithicSparse·streamOptimized VMDK)를 볼륨으로 올리는 길만 있다. 사용자가 VM 설정(CPU, 메모리, 펌웨어, 디스크 순서, NIC와 MAC)을 손으로 다시 만들어야 하고, vSphere가 내보내는 OVA·OVF와 ESXi 데이터스토어의 디스크(디스크립터 + `-flat.vmdk`)는 그대로 쓸 수 없다.

| 계획 | 내용 |
|---|---|
| 29 | OVA 가져오기: `VmImport` 리소스, OVA 올리기와 OVF 해석, VM 제안(장치 매핑·경고), 사용자가 고친 값으로 가져오기 Task(디스크 추출·변환·VM 만들기), 지우기, API·oplctl·웹 마법사, 실제 시험 |
| 30 | ESXi에서 바로 가져오기: ESXi·vCenter 연결(인증서 지문 확인), 데이터스토어의 VM 찾기와 `.vmx` 해석, 같은 `VmImport`로 제안, 디스크를 HTTPS로 내려받아 가져오기, 가짜 ESXi 서버로 시험 |

넣지 않는 것: 실행 중인 VMware VM 옮기기(가져오기 전에 VMware에서 끈다), 증분·반복 동기화(warm migration), VMware 스냅샷 체인 가져오기(먼저 통합해야 한다), VMware Tools 제거와 virtio 드라이버 주입(게스트 안에서 할 일, README에 안내), VDDK·NFC 프로토콜, OVF 디스크 압축(`ovf:compression`)과 분할(`ovf:chunkSize`), 여러 VM이 든 OVA(`VirtualSystemCollection`), CD-ROM·플로피·USB·직렬 포트·사운드 장치, VM 일괄 가져오기.

### 견준 길

- **입력: OVA 먼저, ESXi 직접은 다음(채택) vs ESXi 직접만.** OVA는 네트워크·인증 없이 시험할 수 있고 vSphere·Workstation·다른 제품이 모두 만든다. ESXi 직접 가져오기는 내보내기 단계를 없애 주지만 실제 ESXi 없이는 가짜 서버로만 시험한다. 둘 다 같은 `VmImport` 리소스·제안·Task를 쓰고 디스크를 얻는 부분만 다르다.
- **ESXi 접근: 데이터스토어 HTTP(`/folder`)(채택) vs vSphere SOAP API·VDDK.** `/folder`는 ESXi와 vCenter가 모두 내놓는 단순한 HTTPS 파일 접근이다(기본 인증, 디렉터리는 HTML 목록, 파일은 그대로). SOAP·VDDK는 큰 의존성(WSDL, 독점 라이브러리)이 필요하다. 전원 상태·스냅샷 트리 같은 정보는 못 얻으므로 잠금 파일과 디스크립터로 짐작한다.
- **장치: 지금 VmSpec 안에서 매핑(채택) vs VMware 장치(vmxnet3, pvscsi, LSI) 흉내.** QEMU에 vmxnet3·pvscsi가 있지만 느리고 opLite가 쓰지 않는 장치다. Windows는 SATA(AHCI)와 e1000e 드라이버를 기본으로 갖고 있고 Linux는 virtio 드라이버를 커널에 갖고 있다. 그래서 게스트 OS로 기본값을 고르고 사용자가 바꿀 수 있게 한다.
- **진행 방식: 올리기·분석 → 제안 확인 → 실행(채택) vs 한 번에.** 네트워크 매핑처럼 사용자가 정해야 하는 값이 있고, 큰 OVA를 다시 올리지 않고 설정만 바꿔 재시도할 수 있어야 한다.

## 2. OVA 가져오기 (계획 29)

### 2.1 `VmImport` 리소스

```yaml
kind: vm-import
name: web01                        # 사용자가 정한다(웹은 파일 이름에서)
spec:
  node: <노드 id>                   # 원본 파일이 있는 노드(올린 노드)
  source: { ova: { file_name: web01.ova, size_bytes: ... } }
                                   # 계획 30: { esxi: { host, port, datacenter, datastore, vmx } }
status:
  phase: ready | importing | done | failed
  source_vm:                       # 원본 VM에서 읽은 것(표시용)
    name: web01
    guest_os: windows9_64Guest     # OVF vmw:osType 또는 vmx guestOS
    firmware: efi | bios
    vcpus: 4
    cores_per_socket: 2
    memory_mib: 8192
    disks: [ { name: disk0, file: web01-disk1.vmdk, capacity_bytes: ..., controller: scsi|sata|ide|nvme } ]
    nics:  [ { network: "VM Network", adapter: vmxnet3, mac: "00:50:56:..." } ]
  proposal: { VmSpec 초안 }          # 아래 매핑 규칙으로 만든 값, 볼륨 자리는 비어 있다
  warnings: [ "CD-ROM drive is not imported", ... ]
  vm: null                         # done이면 만든 VM id
  error: null                      # 마지막 실행이 실패한 이유(phase는 ready로 돌아간다)
```

- `Kind::VmImport`(`vm-import`, watch 종류에도 있다). 이름 규칙은 다른 리소스와 같다.
- 원본 파일은 `{data_dir}/uploads/imports/{import id}/`에 둔다(`source.ova`, 실행 중에는 추출한 디스크 하나). 지금의 업로드 정리(끝나지 않은 `VolumeImport`가 없는 파일 지우기)는 이 하위 디렉터리를 건드리지 않는다. 대신 노드가 시작할 때 리소스가 없는 `imports/{id}` 디렉터리를 지우고, 이 노드의 리소스인데 디렉터리가 없으면 `failed`("source files are gone")로 바꾼다.
- 실행이 실패하면 phase는 `ready`로 돌아가고 `error`를 남긴다(원본은 남으므로 값을 바꿔 다시 실행한다). `failed`는 원본을 쓸 수 없을 때만 쓴다.

구현(계획 29): `opl_types::vm_import`의 `VmImport = Resource<VmImportSpec { node, source: ImportSource }, VmImportStatus { phase: ImportPhase(Ready(기본)|Importing|Done|Failed), source_vm: Option<SourceVm>, proposal: Option<ImportProposal>, warnings, vm: Option<Uuid>, error: Option<String> }>`(빈 값은 빼고 쓴다), `Kind::VmImport`(`vm-import`, watch 종류에도 있다). `ImportSource`는 외부 태그 enum이고 지금은 `Ova(OvaSource { file_name, size_bytes })`(JSON `{"ova": {…}}`)뿐이다 — 계획 30이 `esxi` 변형을 더한다. `SourceVm { name, guest_os, firmware, vcpus, cores_per_socket, memory_mib, disks: Vec<SourceDisk { name: "disk{i}", file(tar 항목 이름), capacity_bytes, controller }>, nics: Vec<SourceNic { network, adapter(소문자), mac: Option<MacAddr> }> }`. 제안은 VmSpec 초안 대신 `ImportProposal { sockets, cores, memory_mib, firmware, disk_bus, nic_model, networks: BTreeMap<원본 네트워크, Option<opLite 네트워크>> }`이고, 실행 때 VmSpec을 만든다(§2.4). 이름 `ova`는 예약(400, 경로 `/vm-imports/ova`와 겹친다). 원본은 `{upload_dir}/imports/{id}/source.ova`(`upload_dir` = `{data_dir}/uploads`), 실행 중 추출 파일은 같은 디렉터리의 `disk.vmdk`다. 노드 시작 때 정리(`vm_imports::sweep`, Task 실행기의 업로드 sweep 안에서 API를 띄우기 전)는 리소스가 없는 `imports/*`를 지우고, 이 노드의 가져오기 가운데 끝나지 않은 `VmImport` Task가 없는 `importing`을 `ready` + "the import was interrupted"로(그 가져오기의 Task 대상 VM이 이미 있으면 — VM을 만든 뒤 `done`을 쓰기 전에 죽었다 — `done` + `vm`으로 끝내고 원본 디렉터리를 지운다; 끝나지 않은 Task가 있으면 실행기가 그 Task를 실패로 끝내며 정리한다), `source.ova`가 없는 `ready`를 `failed` + "source files are gone"으로 바꾼다. 볼륨 업로드 sweep은 `imports` 디렉터리를 건드리지 않는다.

### 2.2 올리기와 OVF 해석

- `POST /vm-imports/ova/check?name=&size=`: 이름, 권한(VM 쓰기), 이 노드가 VM을 돌리는 노드인지(위트니스는 400), 업로드 디렉터리 여유(`size + 1 GiB`, 아니면 507)를 본다. 볼륨 가져오기 검사와 같은 모양이다.
- `POST /vm-imports/ova?name=`: 본문이 OVA(`application/octet-stream`, `Content-Length` 필수). 지금의 볼륨 올리기처럼 파일로 받아 fsync한 뒤, 같은 요청 안에서 해석하고 `ready` 리소스를 만들어 201로 돌려준다. 해석이 실패하면 400(`invalid_ova`, 이유를 메시지에)과 함께 파일을 지운다. 연결이 끊기면 파일을 지운다.
- OVA는 tar(ustar)다. 헤더만 훑어 항목 표(이름 → 파일 안 위치·크기)를 만든다. 규칙:
  - `.ovf`가 정확히 하나여야 하고 4 MiB 이하여야 한다. `.mf`는 없거나 하나, `.cert`는 무시한다(서명 검증은 하지 않는다, 경고).
  - OVF의 `References/File`이 가리키는 파일이 모두 tar 안에 있어야 한다. `ovf:compression`·`ovf:chunkSize`가 있으면 400.
  - 디스크 파일은 VMDK(streamOptimized·monolithicSparse)여야 한다. 실행 때 지금의 `check_import_source`로 다시 본다.
- OVF는 XML DOM 파서(`roxmltree`, DTD·외부 엔티티를 처리하지 않는다)로 읽는다. 읽는 것:
  - `VirtualSystem` 하나(`VirtualSystemCollection`이면 400 "only single-VM OVA files are supported"). 이름은 `Name` 또는 `ovf:id`.
  - `OperatingSystemSection`의 `vmw:osType`(없으면 `ovf:id`의 CIM 번호로 Windows 여부만).
  - `VirtualHardwareSection`의 `Item`/`StorageItem`/`EthernetPortItem`: ResourceType 3(CPU, `VirtualQuantity`, `vmw:CoresPerSocket`), 4(메모리, `AllocationUnits` 반영), 5·6·20(IDE·SCSI·SATA 컨트롤러), 17(디스크, `HostResource` → `DiskSection` → 파일, `capacity`·`capacityAllocationUnits`), 10(NIC, `Connection`, `ResourceSubType`, `Address`). 15·16(CD-ROM·플로피), 23(USB), 그 밖은 경고로만 남긴다.
  - `vmw:Config`의 `firmware`(`efi`면 UEFI).
  - 디스크 순서는 OVF에 컨트롤러가 나온 순서, 그 안에서 `AddressOnParent` 순서다.
- `.mf`가 있으면 해석 때는 형식만 보고, 실행 때 디스크를 추출하면서 SHA-256(또는 SHA-1)을 비교한다. 다르면 실행 실패.

구현(계획 29): opl-api `vm_imports/ova.rs`·`ovf.rs`(순수·동기, 새 의존성 `roxmltree`·`sha1`). tar는 손으로 읽는다: 512바이트 헤더(체크섬 확인), 이름은 ustar `prefix/name`, 크기는 8진수 또는 base-256, `0`·`\0`은 파일, `5`(디렉터리)·`g`(pax 전역)는 건너뛰고, `x`(pax)의 `path`·`size`와 `L`(GNU 긴 이름)은 다음 파일·디렉터리 항목에 쓴다(사이의 다른 pax·`L` 헤더는 가져가지 않는다; 둘 다 있으면 pax `path`). 그 밖의 종류, 겹친 이름, 파일을 넘는 크기, 4 KiB를 넘는 이름, 10 000개를 넘는 헤더는 400. 끝은 0 블록 또는 파일 끝. 항목 표는 tar 순서의 `Vec`과 이름 색인(`HashMap`)이다. `parse_ova`는 `.ovf` 정확히 하나(4 MiB 이하, UTF-8), `.mf` 0~1개(1 MiB 이하; 줄 `SHA256(file)= hex`처럼 `SHA1`·`SHA256`·`SHA384`·`SHA512`, 공백 차이 허용; 모르는 알고리즘의 줄은 오류가 아니라 그 디스크를 확인하지 않고 경고 "the manifest uses an unsupported digest for X; it is not verified", `.mf`에 없는 디스크는 경고 "X is not listed in the manifest; it is not verified"), `.cert`가 있으면 경고 "the OVA signature (.cert) is not verified", OVF `References`의 파일이 모두 tar에 있어야 한다. OVF는 요소·속성을 로컬 이름으로 비교한다(OVF 1.x·2.x와 도구마다 접두어가 다르다). `vmw:osType`이 없으면 CIM 번호가 Windows일 때 `windows`, 아니면 `Description` 또는 `other`. 메모리 단위는 `byte * 2^N`·`byte * N`·`MegaBytes` 등, 디스크 `ovf:format`에 `vmdk`가 없으면 400(VMDK 하위 형식은 실행 때 `check_import_source`가 본다). 컨트롤러 종류는 ResourceType 5 ide, 6 scsi, 20 sata(하위 종류에 `nvme`가 있으면 nvme). 디스크 순서는 컨트롤러 종류가 처음 나온 순서, 같은 종류는 `rasd:Address` 순서(없으면 나온 순서), 그 안에서 `AddressOnParent` 순서이고, 디스크가 둘 이상의 컨트롤러에 걸쳐 있으면 경고 "disks are on more than one controller; check that disk0 is the boot disk". 디스크가 32개, NIC가 `MAX_NICS`(16)개를 넘으면 400. 화면(24)과 VMCI는 조용히 빼고, 플로피·CD-ROM·USB와 그 밖의 장치는 경고("… is not imported"). 해석 경고는 겹치지 않게 50개까지이고 넘으면 마지막에 "… and N more". API는 opl-api `vm_imports.rs`: `POST /vm-imports/ova/check?name&size`(204), `POST /vm-imports/ova?name[&file_name]`(201, `file_name` 기본 `{name}.ova` — oplctl·웹은 실제 파일 이름을 보낸다). 둘 다 `Kind::VmImport` 쓰기 권한, 이름 검증·409 `name_taken`, 위트니스 400, 여유 `size + UPLOAD_RESERVE_BYTES`(507)를 본다. 받기는 볼륨 업로드의 `receive`를 쓰고, `spawn_blocking`으로 해석한 뒤 그때의 네트워크 이름으로 `propose`해 해석 경고와 제안 경고를 `warnings`에 넣는다. 해석 실패(`OvaError::Invalid`)는 400 `invalid_ova`, 읽기 실패(`Io`)는 500이고, 실패하거나 연결이 끊기면(future가 버려지면) 디렉터리를 지운다. `GET /vm-imports`, `GET /vm-imports/{key}`는 모든 사용자.

### 2.3 제안(매핑 규칙)

순수 함수 `propose(source_vm) -> (VmSpec 초안, warnings)`:

| 원본 | opLite |
|---|---|
| vCPU n, 소켓당 코어 c | `cpu.sockets = n / c`, `cores = c`(나누어떨어지지 않으면 `sockets = 1, cores = n`), `model: host` |
| 메모리 | `memory_mib`(최소값보다 작으면 400이 아니라 경고 후 최소값) |
| `firmware = efi` / 그 밖 | `uefi` / `bios`. UEFI 변수는 옮기지 않는다(게스트는 기본 부트 경로로 켜진다, 경고) |
| 게스트 OS가 Windows(`osType`이 `win`으로 시작) | 디스크 `sata`, NIC `e1000e` |
| 그 밖 | 디스크 `virtio-blk`, NIC `virtio` |
| 디스크 순서 | 첫 디스크 `boot_order: 1` |
| NIC MAC | 그대로 둔다(게스트의 고정 IP·라이선스가 MAC에 묶인 경우가 많다). VMware에서 원본을 꺼 두라고 경고 |
| NIC 네트워크 | 이름이 같은 opLite 네트워크가 있으면 그것, 없으면 비워 둔다(실행 때 필수) |
| 머신 | `q35` |

SATA 디스크가 6개를 넘으면 제안은 `virtio-blk`로 하고 경고한다(q35 AHCI 포트 수).

구현(계획 29): `opl_types::vm_import::{is_windows, propose(&SourceVm, &[있는 네트워크 이름]) -> (ImportProposal, 경고)}`. 소켓당 코어 `c`가 0이거나 vCPU를 나누지 못하면 `1 × n`. 메모리가 최소값(`MIN_MEMORY_MIB`)보다 작으면 최소값 + 경고, u32에 들어가지 않으면 최소값 + 경고 "memory is too large (…); set memory_mib when running the import". UEFI면 경고 "UEFI boot entries (NVRAM) are not imported; the guest boots from its default boot path". SATA 포트는 6개로 보고 디스크가 7개 이상이면 `virtio-blk` + 경고. 원본 NIC에 MAC이 하나라도 있으면 경고 "power off the VMware VM before starting the imported one; it keeps the same MAC addresses". 네트워크는 이름이 같은 opLite 네트워크가 있으면 그것, 없으면 `null`(실행 때 필수).

### 2.4 실행(`POST /vm-imports/{key}/run`)

본문:

```json
{
  "name": "web01", "pool": "local",
  "networks": { "VM Network": "lan" },
  "disk_bus": "sata", "nic_model": "e1000e",
  "firmware": "uefi", "sockets": 2, "cores": 2, "memory_mib": 8192,
  "keep_macs": true
}
```

- `name`·`pool`·원본 NIC마다 `networks`는 필수, 나머지는 제안 값이 기본. 결과 VmSpec은 지금의 VM 만들기 검사를 그대로 거친다(이름 409 `name_taken`, 없는 네트워크 400, MAC이 다른 VM과 겹치면 409, `keep_macs: false`면 새 MAC).
- 풀은 원본이 있는 노드에서 쓸 수 있어야 한다(아니면 409 `pool_not_on_node`, 지금의 `check_pool_on`). 업로드 디렉터리에 가장 큰 디스크를 추출할 여유가 있어야 한다(아니면 507).
- `ready`가 아니면 409 `import_busy`(실행 중) 또는 `import_done`.
- 리소스를 `importing`으로 바꾸고 Task `VmImport { import, name, pool, ... }`(대상은 미리 정한 새 VM id, 노드는 `spec.node`)를 만들어 202로 돌려준다.

Task(가져오기 노드, 시간 제한 없음 — 마이그레이션·백업과 같다):
1. 디스크마다: tar에서 `disk.vmdk`로 추출(`.mf` 해시 확인) → `check_import_source` → 볼륨 `{VM 이름}-disk{i}` 만들고 `import_volume` → 추출 파일 지움 → 볼륨 `ready`. 만든 볼륨은 지금의 `Made` 기록으로 남겨 실패 때 지운다.
2. VM을 멈춘 상태로 만든다(`node: None`, 복제·복원과 같은 `create_vm`).
3. 가져오기를 `done`(`vm` id)으로 바꾸고 원본 디렉터리를 지운다.
실패하면 만든 볼륨·VM을 지우고 가져오기를 `ready` + `error`로 되돌린다.

구현(계획 29): `ImportRun { name, pool, networks(생략하면 빈 표), disk_bus?, nic_model?, firmware?, sockets?, cores?, memory_mib?, keep_macs(기본 true) }`(`deny_unknown_fields`). 실행 API는 `Kind::VmImport`·VM·볼륨 쓰기 권한을 보고, 이 가져오기의 끝나지 않은 Task(실행·지우기)가 있으면 409 `import_busy`(`importing`이어도 끝나지 않은 Task가 없으면 — 원본 노드가 없어져 janitor가 Task를 실패로 끝내 정리가 돌지 않았다 — 쉬는 것으로 본다), `done`이면 `import_done`, `failed`(또는 원본 VM이 없음, 원본 파일이 없음)면 `import_failed`, 원본 노드가 없거나 오프라인이면 `node_offline`(백업의 `check_online`). `ready`이거나 쉬는 `importing`인데 이 가져오기의 끝난 실행 Task의 대상 VM이 있으면(VM을 만든 뒤 `done`으로 바꾸지 못했다 — 시작 정리의 `made_vm`과 같은 판단) 가져오기를 `done` + `vm`으로 끝내고(원본이 이 노드에 있으면 원본 디렉터리도 지운다, 다른 노드의 것은 지우기 Task가 지운다) 409 `import_done`. 그다음 VM 이름(409 `name_taken`)과 볼륨 이름 `{name}-disk{i}`(409 `name_taken`)를 보고, VM 스펙을 `vm_imports::import_spec(&SourceVm, &ImportProposal, &ImportRun, 볼륨 이름들)`로 만든다 — 실행 값이 없으면 제안 값, cpu 모델 `host`, 머신 `q35`, 첫 디스크 `boot_order: 1`, NIC는 매핑한 네트워크(요청에 없으면 제안의 같은 이름 매핑)와 `keep_macs`면 원본 MAC(없으면 만들 때 새 MAC), 보안 그룹 없음, 꺼짐, 노드·예약·HA 없음. 매핑도 제안도 없으면 400 `no network mapping for "X"`, `sata`인데 디스크가 6개를 넘으면 400, 스펙 검증 실패와 없는 네트워크·풀은 400, 풀은 원본 노드에서 써야 하고(`check_pool_on`: 409 `pool_not_on_node`·`pool_unavailable`), 원본이 이 노드에 있으면 OVA 안 가장 큰 디스크 파일 크기 + `UPLOAD_RESERVE_BYTES`의 여유(507), 원본 MAC이 다른 VM과 겹치면 409 `mac_in_use`. 그다음 revision을 검사해 `importing`(error 비움)으로 쓰고(겹치면 409 `import_busy`) Task `TaskOp::VmImport { import, run }`(`vm-import`, 대상 = 미리 정한 새 VM id, 노드 = `spec.node`)를 만든다. Task를 만들지 못하면 `ready`로 되돌린다. Task는 opl-api `tasks/import.rs`: VM 큐가 아니라 가져오기 id 큐(같은 가져오기의 실행·지우기가 겹치지 않는다), 시간 제한 없음. VM 이름·풀과 `keep_macs`면 원본 MAC 겹침(디스크를 옮기기 전에 실패)을 다시 보고 `parse_ova`로 index와 `.mf`를 다시 읽어 디스크마다 업로드 디렉터리 여유(항목 크기 + `UPLOAD_RESERVE_BYTES`, 아니면 "not enough space in the upload directory to extract {file}")를 보고 `extract`(복사·fsync하며 해시, 다르면 추출 파일을 지우고 "checksum mismatch for {file}") → `check_import_source(Vmdk)` → 볼륨(크기는 OVF의 capacity로 만들고 `ready`로 바꿀 때 `mark_ready`가 실제 가상 크기로 고친다, 형식은 풀의 기본 형식 `default_format`, `status.node`는 이 노드, 이름은 복제의 `volume_name`)을 `Made`에 먼저 적고 만들고 `import_volume` → 추출 파일 지움. 볼륨은 VM을 만들기 바로 전에 `ready`로 바꾸고, VM은 복제의 `create_vm`으로 만든다. 끝나면 가져오기 `done` + `vm` + error 비움, 원본 디렉터리를 지운다. 실패 정리(`import::discard`)는 대상 VM이 있는지 읽지 못하면 읽을 때까지 다시 시도하고(있는 VM의 볼륨을 지우지 않게), 대상 VM이 이미 있으면(VM을 만든 뒤 opld가 죽었다) 볼륨·VM을 두고 가져오기를 `done` + `vm`으로 끝내며 원본 디렉터리를 지우고, 아니면 복제의 정리로 만든 볼륨·VM을 지우고 추출 파일을 지우고, 가져오기가 `importing`이면 `ready` + error(Task 오류 그대로, 예: `cannot import disk "disk1": checksum mismatch for web-disk2.vmdk`)로 되돌린다.

### 2.5 그 밖의 API·oplctl·웹

- `GET /vm-imports`, `GET /vm-imports/{key}`(모두 읽기), `DELETE /vm-imports/{key}` → Task `VmImportDelete`(원본 노드에서 디렉터리를 지우고 리소스를 지운다, `importing`이면 409 `import_busy`). 원본 노드가 클러스터에서 빠졌거나 오프라인이면 `?force=true`로 리소스만 지운다(백업 지우기와 같다).
- 권한: 올리기·실행·지우기는 VM 쓰기(operator 이상).
- oplctl: `import ova FILE --name N`(검사 → 올리기, 진행률), `import list`, `import get N`(원본·제안·경고 표), `import run N --name --pool --net "VM Network=lan" [--disk-bus] [--nic-model] [--firmware] [--sockets] [--cores] [--memory] [--new-macs]`(Task 기다림), `import delete N [--force]`.
- 웹: 사이드바 "가져오기" 페이지. 목록과 "OVA 올리기"(진행률). 항목을 열면 마법사: ① 원본 요약과 경고 ② 이름·풀·CPU·메모리·펌웨어·디스크 버스·NIC 모델 ③ 네트워크 매핑(원본 네트워크마다 선택) ④ 확인 → 실행, Task 진행 → 만든 VM 링크. 올리기는 지금의 볼륨 올리기처럼 UI를 내준 노드로 간다(그 노드에 원본이 남는다).

구현(계획 29): `DELETE /vm-imports/{key}` → 202 Task `vm-import-delete`(`TaskOp::VmImportDelete`, 대상 = 가져오기 id, 원본 노드, 가져오기 큐; 디렉터리를 지우고 리소스를 지운다, 디렉터리가 없어도 된다). 이 가져오기의 끝나지 않은 Task가 있으면(`force`여도, phase는 보지 않는다) 409 `import_busy`, 원본 노드가 없거나 오프라인이면 409 `node_offline`이고 `?force=true`면 리소스만 지우고 200 `VmImportForgotten { message }`("… its files are left on node …", 백업 지우기의 force와 같다). 만든 VM과 볼륨은 남는다. 권한: 올리기·실행·지우기는 `RoleAuthorizer`의 `Kind::VmImport` 쓰기(operator 이상; 실행은 VM·볼륨 쓰기도), 읽기는 모두. OpenAPI 태그 `vm-imports`, 웹 `schema.d.ts`. oplctl `import ova FILE --name N`(크기로 `check` → 실제 파일 이름과 함께 올림, 진행률 표시는 없다 — 볼륨 올리기와 같다; 출력 ``vm-import N uploaded; review it with `oplctl import get N` ``), `import list`(열 `NAME NODE SOURCE PHASE VM`, SOURCE는 원본 VM 이름 또는 파일 이름), `import get N`(머리 줄, `source vm: …`, `proposal: S sockets x C cores, M MiB, FW, BUS disks, MODEL NICs`, 디스크 표 `DISK FILE SIZE CONTROLLER`, NIC 표 `NETWORK ADAPTER MAC PROPOSED`, 경고), `import run N --name V --pool P [--net SOURCE=NETWORK | --net NETWORK(원본 네트워크가 하나일 때)]… [--disk-bus] [--nic-model] [--firmware] [--sockets] [--cores] [--memory] [--new-macs]`(Task를 24시간 `IMPORT_WAIT`까지 기다림, 출력 "vm V imported"), `import delete N [--force]`(출력 "vm-import N deleted" 또는 서버 알림). 웹은 사이드바 "가져오기"(VM 아래): `/imports`(목록: 이름·노드·원본 VM·상태와 오류·만든 VM 링크, "OVA 올리기" 대화상자 — 파일 이름에서 확장자를 빼고 이름 규칙에 맞춘 이름 제안, 사전 확인 뒤 볼륨 업로드와 같은 진행률 알림(`storage/upload.ts`를 함께 쓰게 나눔), 끝나면 항목을 연다), `/imports/:name`(원본 요약·디스크와 NIC 표·경고·오류, 제안으로 채운 한 페이지 폼 — 이름·풀·소켓·코어·메모리·펌웨어·디스크 버스·NIC 모델·원본 네트워크마다 필수 매핑·MAC 유지, 실행 → Task 진행 → VM 링크, 지우기 — `importing`이어도 누를 수 있고 서버가 `import_busy`로 정한다, 실행을 보내는 동안만 막는다; `name_taken`은 서버 메시지를 `imports.errors.nameTaken`에 끼워 이름 칸에 보인다). 강제 지우기는 웹에 없다(안내만). watch 종류·권한·Task 종류 문구 `vm-import`·`vm-import-delete`, ko·en.

구현(계획 29) 시험: opl-types 단위(직렬화 모양, 제안 표), opl-api 단위(tar 항목 표, vSphere 2.x·Workstation 1.x 모양 OVF 견본 `tests/fixtures/*.ovf`, 거절, `.mf`, `extract` 해시)와 시험용 ustar 작성기 `crates/opl-api/tests/common/ustar.rs`(opld 시험도 `#[path]`로 쓴다), `tests/vm_imports.rs`(API: 올리기·검사·sweep·실행 검사·Task 없는 `importing`의 실행·지우기·지우기, 대상 VM이 있는 가져오기를 `done`으로 끝내는 시작 정리와 실행 요청), `tests/vm_import_tasks.rs`(`storage-it`, MockHypervisor·dir 풀·`qemu-img`로 만든 streamOptimized VMDK: 가져오기·`.mf` 불일치 정리·MAC 겹침으로 일찍 실패한 뒤 고친 값으로 재실행·추출 공간 부족·지우기; CI `check (ubuntu)` storage-it 단계), oplctl 단위(인자 → 본문, 표 출력, 작은 시험 서버), 웹 시험. 실제 시험 `crates/opld/tests/vm_import.rs`(`qemu-it`, macOS HVF와 CI linux-kvm — linux-kvm 작업은 `-p opld`의 모든 시험을 돌리므로 목록을 바꿀 필요가 없다): opld 하나, dir 풀 `local`, 네트워크 `lan`(브리지 `br0` — VM을 켜지 않으므로 없어도 된다). 시험이 4 MiB raw 디스크 앞 64 KiB에 표식을 쓰고 `qemu-img convert -O vmdk -o subformat=streamOptimized`, vSphere 모양 OVF(Linux `ubuntu64Guest`, 2 vCPU·소켓당 코어 2, 1024 MiB, efi, SCSI 디스크, "VM Network"의 vmxnet3 NIC와 MAC), SHA256 `.mf`를 ustar로 묶는다. `oplctl import ova` → `import get`(phase `ready`, 파일 이름·크기, 제안 `1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs`, 매핑 없음, NVRAM·MAC 경고, `source.ova` 있음) → `import run --net lan` → VM `web`(꺼짐, uefi, q35, 1×2, 디스크 하나 `web-disk0` virtio-blk boot 1, NIC `lan` virtio 원본 MAC), 볼륨 크기 4 MiB이고 `qemu-img convert -O raw`로 뺀 앞 64 KiB가 표식, 가져오기 `done`·`vm` = VM id, 원본 디렉터리 없음, 목록 행, 지워도 VM과 볼륨이 남음. 두 번째 시험: `.mf`의 디스크 해시를 틀리게 적은 OVA는 올리기는 되고 `import run --net "VM Network=lan"`이 "checksum mismatch for web01-disk1.vmdk"로 실패, 가져오기 `ready` + 같은 error, VM·볼륨 없음, 풀 디렉터리 비어 있음, 원본 디렉터리에 `source.ova`만, 지우면 디렉터리가 사라진다. VM은 켜지 않는다(게스트 OS가 없고 aarch64 macOS 노드는 x86 게스트를 돌리지 못한다).

알려진 틈(계획 29): 볼륨을 `ready`로 만든 뒤 VM을 만들기 전에 opld가 죽으면 볼륨이 남는다(복제·복원과 같다 — `oplctl volume delete`). 원본은 올린 노드에만 있으므로 그 노드가 꺼져 있으면 실행할 수 없고, 노드가 빠지면 force 지우기로 기록만 지우며 파일은 그 노드에 남는다. 실행에는 업로드 디렉터리에 원본 OVA와 추출한 디스크 하나가 함께 있어야 한다(실행 API는 원본 노드에서 받았을 때 가장 큰 디스크 기준으로, Task는 디스크마다 다시 본다). 풀 용량은 미리 보지 않는다(복제·복원과 같다 — 모자라면 옮기다 실패하고 정리한다). 노드가 시작할 때의 정리와 그사이 들어온 실행 요청이 짧게 겹치면 실행이 `ready`로 되돌려질 수 있다(다시 실행하면 된다). `failed` 가져오기는 다시 쓸 수 없다(지우고 다시 올린다). OVA 서명(`.cert`)은 확인하지 않는다. oplctl 올리기는 진행률을 보이지 않는다. MAC 겹침·이름은 Task가 확인한 뒤 VM을 만들기 전에 바뀔 수 있다(VM을 만들 때 실패하고 정리한다). 올리기에서 가져오기 리소스를 저장소에 만드는 동안 클라이언트가 끊으면(만들기는 됐는데 응답 전에 요청이 버려지면) 올리기 정리가 원본 디렉터리를 지워 `source.ova` 없는 `ready` 가져오기가 남을 수 있다(실행은 원본 노드에서 받으면 409 `import_failed`, 노드가 다시 시작할 때 `failed`가 된다 — 지우고 다시 올린다).

## 3. ESXi에서 바로 가져오기 (계획 30)

### 3.1 연결

`EsxiConnection { host, port: 443, user, password, datacenter: "ha-datacenter", fingerprint: Option<String> }`(vCenter면 `datacenter`에 데이터센터 이름).

- 인증서는 시스템 CA로 검증하지 않는다(ESXi는 거의 자체 서명이다). 대신 TLS 핸드셰이크 안에서 서버 인증서의 SHA-256 지문을 `fingerprint`와 비교한다. 지문이 다르면 자격 증명을 보내기 전에 끊는다. `fingerprint`가 없으면 연결만 해 보고 409 `untrusted_certificate`(`details.fingerprint`)를 돌려준다. 사용자가 지문을 확인해 다시 보낸다(웹은 확인 대화상자).
- 비밀번호는 Raft 상태·Task·로그에 남기지 않는다. 요청을 받은 노드의 메모리(`ImportCredentials`, 가져오기 id → 연결)에만 둔다. 그 노드가 다시 시작하면 그 노드의 ESXi 가져오기는 실행할 수 없다(`failed`, "credentials were lost when opld restarted; create the import again").

### 3.2 VM 찾기

`POST /vm-imports/esxi/browse`(본문: 연결) → 데이터스토어 목록(`/folder?dcPath=…`)과 각 데이터스토어 맨 위 디렉터리의 `.vmx` 경로. 각 `.vmx`를 읽어(동시 8개, 전체 500개까지) `{ datastore, vmx, name, guest_os, vcpus, memory_mib, disks, locked }`를 돌려준다. `locked`는 디렉터리에 `.lck` 항목이 있다는 뜻(켜져 있을 수 있음, 웹은 경고).

### 3.3 가져오기 만들기

`POST /vm-imports/esxi`(본문: `{ name, connection, datastore, vmx }`) → 받은 노드에서 `.vmx`와 디스크 디스크립터를 읽어 `source_vm`·제안을 만들고 `ready` 리소스(201).

- `.vmx`(`key = "value"` 줄): `displayName`, `guestOS`, `numvcpus`, `cpuid.coresPerSocket`, `memSize`, `firmware`, `scsiN:M.fileName`/`sataN:M`/`nvmeN:M`/`ideN:M`(`deviceType`이 CD-ROM이면 경고로), `scsiN.virtualDev`, `ethernetN.virtualDev`·`networkName`·`address`(`addressType = "static"`) 또는 `generatedAddress`, `present = "FALSE"`인 장치는 뺀다.
- 디스크 디스크립터(`.vmdk`, 텍스트): `createType`이 `vmfs`·`monolithicFlat`이면 extent(`RW n VMFS "x-flat.vmdk"`)가 하나여야 한다. `parentFileNameHint`가 있으면(스냅샷) 400 "the VM has snapshots; consolidate or delete them in VMware first". 그 밖의 `createType`(`vmfsSparse`·`seSparse` 델타, `vmfsRDM` 등)도 400. extent 이름은 같은 디렉터리의 파일 이름이어야 한다(`/`·`..` 금지).
- 같은 2.3 매핑을 쓴다.

### 3.4 실행

같은 `POST /vm-imports/{key}/run`과 Task `VmImport`. 디스크를 얻는 1단계만 다르다: extent를 HTTPS로 받아 `disk.raw`로 쓰고(스트리밍, 받은 바이트 수가 디스크립터 크기와 같아야 한다) raw로 `import_volume`. 씬 디스크도 전체 크기로 내려오므로 업로드 디렉터리 여유는 디스크 크기만큼 필요하다(`qemu-img convert`가 0을 건너뛰어 풀에는 쓴 만큼만 남는다). 지금의 볼륨 경로·`Made` 정리는 같다.

## 4. 오류와 경계

- 원본 VMware VM이 켜진 채 가져오면 디스크가 일관되지 않다. OVA는 vSphere가 끈 VM만 내보내므로 해당이 없고, ESXi 직접은 잠금 파일로 경고만 한다(README: 가져오기 전에 끄기).
- 가져온 Windows VM을 virtio로 바꾸려면 게스트에 virtio 드라이버를 먼저 넣어야 한다(README에 순서 안내). VMware Tools는 게스트에서 지운다.
- UEFI 게스트는 NVRAM 부트 항목을 잃는다. 대부분 `\EFI\BOOT\BOOTX64.EFI`나 OS 기본 경로로 켜지지만, 안 되면 펌웨어 메뉴에서 부트 항목을 고른다(README).
- opld가 볼륨을 만든 뒤 VM을 만들기 전에 죽으면 볼륨이 남는다(백업 복원과 같은 알려진 틈, Task 실패 처리로 정리하는 지금 방식을 따른다).
- macOS(aarch64) 노드는 x86 게스트를 가져올 수는 있어도 켤 수 없다(지금의 머신 검사가 시작 때 막는다).

## 5. 시험

| 층 | 무엇 | 어디 |
|---|---|---|
| 단위 | tar 항목 표, OVF 해석(vSphere·Workstation 모양 OVF 견본), `.mf` 형식, 제안 매핑(Windows·Linux, 코어 나누기, SATA 6개 초과), `.vmx`·디스크립터 해석, 지문 비교 | macOS, CI |
| API | 올리기 검사(이름·여유·위트니스), 잘못된 OVA 400과 파일 정리, 실행 검사(네트워크 매핑 누락·풀·MAC 겹침·바쁨), 지우기·강제 지우기, 연결 지문 409 | macOS, CI |
| Task | Mock 하이퍼바이저·dir 풀로 OVA(테스트에서 `qemu-img`로 만든 streamOptimized VMDK + OVF를 tar로) 가져오기, `.mf` 불일치 실패와 정리·재실행, 가짜 ESXi(axum + rcgen 자체 서명 TLS, 기본 인증, `/folder` HTML)에서 가져오기, 잘못된 지문에 자격 증명을 보내지 않음 | macOS, CI(`storage-it`) |
| 실제 | opld 하나·실제 QEMU: 표식을 쓴 디스크로 만든 OVA를 가져와 VM 만들기 → 볼륨 내용 확인, 가짜 ESXi에서 같은 확인 | macOS, CI(KVM) |

## 6. 계획 나누기

- 계획 29: `VmImport` 타입 → tar 읽기·OVF 해석·제안(순수 함수) → 올리기 API와 원본 디렉터리·노드 시작 정리 → 실행 API와 `VmImport` Task·지우기 → oplctl → 웹 마법사 → 실제 시험·문서.
- 계획 30: `.vmx`·디스크립터 해석 → 지문을 확인하는 HTTPS 클라이언트와 `/folder` 목록 해석 → browse·만들기 API와 메모리 속 자격 증명 → Task의 내려받기 단계 → oplctl·웹(연결·지문 확인·VM 고르기) → 가짜 ESXi 시험·문서.
