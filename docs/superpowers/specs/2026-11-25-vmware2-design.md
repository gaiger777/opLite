# VMware 이전 2 설계 — vCenter 일괄 가져오기, CBT로 내리는 시간을 줄인 따뜻한 이전, OVA/OVF 내보내기 (계획 72)

작성일: 2026-11-25
상위 문서: `2026-11-04-vmware-import-design.md`(계획 29·30 — `VmImport`, OVA·OVF 해석, ESXi `/folder` 내려받기, 지문 고정 HTTPS, 제안 규칙 `propose`), `2026-11-23-backup-store2-design.md`(계획 68 — NBD 클라이언트, `qemu-nbd`, 가짜 S3, 비밀을 Raft에 두고 보기에서 빼기, 이어 가는 Task), `2026-11-25-dnd-builder-design.md`(계획 71 — `POST /vm-checks` 미리 확인과 "권한 먼저, 첫 거절에서 멈춤"), `2026-11-18-folders-permissions-quotas-2fa-design.md`(계획 62 — 폴더·권한·할당량), `2026-11-18-ui-ux-redesign-design.md`(계획 61 화면 규칙)
기준 트리: 계획 71 끝(`d2d043f6`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 §18에 덧붙인다. 로드맵의 "VMware 마이그레이션 2"(vCenter 일괄, CBT 따뜻한 이전, OVA 내보내기)를 이 문서가 맡는다.

## 1. 목적

계획 29·30은 VMware VM을 **한 대씩, 꺼진 채로** 옮긴다: OVA를 올리거나 ESXi 데이터스토어에서 `.vmx`·flat extent를 통째로 받는다. VMware를 떠나는 회사의 실제 일은 다르다 — 수십·수백 대를 **물결(wave)로 나눠** 옮기고, 업무 VM은 **몇 분 안의 점검 시간**만 허락되며, 문제가 생기면 **원래 VM으로 바로 돌아가야** 한다. 그리고 떠난 뒤에도 일부 VM을 VMware(또는 다른 제품)로 **돌려보낼 길**이 있어야 이탈 결정을 쉽게 내린다. 계획 72는 셋을 더한다.

- **vCenter 일괄 가져오기**: 저장해 둔 vCenter(또는 ESXi) 연결로 인벤토리(데이터센터·폴더·클러스터·호스트·VM)를 나무로 보고, VM을 여러 대 고르고, **매핑 규칙**(네트워크 → 우리 네트워크, 데이터스토어 → 우리 풀, VMware 폴더 → 우리 폴더, 노드)을 한 번 정해, VM마다 **미리 확인**(계획 71의 만들기 검사 그대로 + VMware 쪽 검사)을 보고 시작한다. 동시 수(계획·호스트·노드당)를 지키며 VM마다 Task가 진행률을 보이고, opld가 다시 시작해도 이어 간다.
- **따뜻한 이전(warm migration)**: 원본이 **켜진 채** 스냅샷 위에서 전체 복사를 하고, 그동안 바뀐 블록만 **CBT(changed block tracking)** 의 `QueryChangedDiskAreas`로 골라 반복해 옮긴다. 전환(cutover) 때만 원본을 끄고, 마지막 델타(보통 수십 MB~수 GB)를 옮기고, 우리 VM을 켠다. 내리는 시간 = 게스트 종료 + 마지막 델타 + 부팅. 전환이 실패하면 **원본을 다시 켜서 자동으로 되돌린다**. 원본은 절대 지우지 않는다.
- **OVA/OVF 내보내기**: Orbivisor VM을 vSphere·Workstation·VirtualBox가 받는 OVF 패키지(서술자 + streamOptimized VMDK + SHA-256 매니페스트)로 풀에 쓰고, 브라우저·CLI는 그것을 **하나의 OVA로 흘려** 받는다.

## 2. 범위

| 포함 | 제외 |
|---|---|
| 저장하는 원본 연결 `VmwareSource`(vCenter·ESXi, 지문 고정, 비밀번호는 Raft에 두고 보기·watch·감사에서 뺌) | VMware Cloud Director·HCX·NSX 인벤토리, Hyper-V·Nutanix 원본 |
| vSphere SOAP(vim25) 클라이언트 — 세션, 인벤토리, 스냅샷, CBT, 전원, 설정 바꾸기, 호스트 HTTP 티켓 — 손으로 쓴 XML, 새 의존성 없음 | WSDL 코드 생성, vSphere Automation REST, VDDK를 Orbivisor에 묶어 배포(독점 — 운영자가 설치하면 쓰는 선택 길만) |
| 인벤토리 나무와 찾기·거르기, VM 여러 대 고르기, 매핑 규칙(네트워크·데이터스토어·폴더·노드·장치·MAC), VM마다 덮어쓰기 | 규칙 언어(정규식·태그 식), vSphere 태그·사용자 정의 속성 옮기기 |
| VM마다 미리 확인: 계획 71 `vm-checks` 검사 + VMware 쪽 검사(스냅샷·독립 디스크·RDM·vSAN/vVols·FT·CBT·Tools·하드웨어 버전) | 미리 확인 결과 예약(잡아 두기) |
| 차가운 이전(꺼진 VM, 스냅샷 없음)과 따뜻한 이전(켜진 VM, CBT 회차), 동시 수·호스트당·노드당·대역 한도, 이어 가는 Task | 메모리 상태 옮기기(실행 중 무중단 — vMotion 같은 것), 공유 디스크(multi-writer) VM |
| 전환: 게스트 종료(또는 전원 끄기)·마지막 델타·VM 만들기·켜기, 예약 전환(점검 창), 실패 때 자동 되돌림, 켠 뒤 손 되돌리기(revert), 마무리(우리 스냅샷·CBT 정리) | 시험 부팅(동기화 중 대상 VM을 격리해 켜 보기 — §16), 여러 VM의 순서 묶음(앱 그룹 전환) |
| 게스트 맞추기: OS로 장치 고르기(지금 규칙), RHEL 계열 initramfs 경고와 "원본에서 고치면 다음 델타가 옮긴다", virtio-win ISO를 CD로 붙이기, MAC 그대로, Tools가 알린 IP를 SDN 주소로 예약 | 게스트 안에서 명령 실행(VMware Guest Operations·libguestfs·virt-v2v), VMware Tools 지우기, pvscsi·vmxnet3 흉내 장치(§15·§16) |
| OVA/OVF 내보내기(꺼진 VM), dir·NFS 풀에 OVF 폴더, 흘려 보내는 OVA 내려받기 링크, `vm export` CLI | 켜진 VM 내보내기(스냅샷에서 — §16), 백업에서 내보내기, OVF 서명(`.cert`), 압축 디스크(`ovf:compression`) |
| API·OpenAPI·`schema.d.ts`, `orbivisorctl vmware …`·`vm export`·`export …`, 가져오기 탭 안 보기 셋, VM 메뉴의 내보내기 | 새 사이드바 항목·탭 |
| 가짜 vSphere(시험·데모용, 기능 `fake-vsphere`), GitHub `vmware-sim` 작업(govmomi `vcsim` 컨테이너 + `govc import.ova`) | 실제 vCenter·ESXi CI(라이선스·하드웨어 — 손 확인 절차만) |

## 3. 지금 있는 것 (계획 71 끝)

- **가져오기 리소스**(`crates/opl-types/src/vm_import.rs`): `VmImport { spec: { node, source: Ova | Esxi }, status: { phase: ready|importing|done|failed, source_vm: SourceVm, proposal: ImportProposal, warnings, vm, error } }`, 실행 본문 `ImportRun`(이름·풀·네트워크 매핑·디스크 버스·NIC 모델·펌웨어·CPU·메모리·`ips`(SDN)·`keep_macs`·`folder`·CPU 모델·NUMA 칸). 제안 `propose(&SourceVm, 네트워크 이름)` — Windows면 SATA·e1000e, 그 밖은 virtio-blk·virtio, SATA 6개 넘으면 virtio-blk, MAC 유지 경고.
- **API**(`crates/opl-api/src/vm_imports.rs` 1455줄, `vm_imports/{ova,ovf,vmx,esxi}.rs`): OVA 올리기·해석(손으로 읽는 ustar, `roxmltree` OVF, `.mf` SHA), ESXi `/folder` 목록·`.vmx`·디스크립터 해석(스냅샷·RDM 거절), `import_spec`(→ VmSpec), 실행 `run_import`(권한·이름·MAC·풀·여유) → Task `VmImport`(`tasks/import.rs`: 업로드 디렉터리에 디스크를 **통째로** 받고 `import_volume`(`qemu-img convert`) → `create_vm`), 지우기, 노드 시작 sweep. 권한: 루트 `vm.create`(`authz_routes.rs:497-505`).
- **ESXi 클라이언트**(`vm_imports/esxi.rs`): `hyper` 1 HTTP/1.1, 요청마다 새 연결, `rustls` + 끝 인증서 SHA-256 지문을 비교하는 `ServerCertVerifier`(다르면 자격 증명을 보내기 전에 끊음), 기본 인증, 60초 조각 한도. GET만 있고 Range·POST·세션 쿠키는 없다. 비밀번호는 받은 노드 메모리(`ImportCredentials`)에만 — 다시 시작하면 `failed`.
- **가짜 ESXi**(`crates/opl-api/tests/common/esxi.rs`): axum + 자체 서명 TLS, `/folder` HTML, 기본 인증, 요청 기록. opld 시험이 `#[path]`로 쓴다.
- **NBD**(`crates/opl-store/src/nbd/`): 읽기 전용 클라이언트(`NbdClient::connect`·`read`·`block_status`·`extents`, 구조화 응답, `base:allocation`·`qemu:dirty-bitmap:*`)와 서버. `opl-driver/src/storage.rs`의 `nbd_source`(볼륨을 `qemu-nbd --read-only`로 유닉스 소켓에)와 `export_volume`(`qemu-img convert -c -O qcow2`).
- **볼륨**: `VolumePhase { Pending, Ready }`. 노드 시작 sweep(`tasks.rs:1765` `sweep_orphans`)은 그 노드의 **Task 없는 Pending 볼륨을 지운다** — 동기화 중인 대상 볼륨은 여기서 빼야 한다(§9.6).
- **이어 가는 Task**: `NodeUpgrade`·`NetworkMigrate`·`BackupLiveRestore`만(`tasks.rs:762`), 시간 제한 없는 Task 목록(`tasks.rs:790`). 리더 루프 표(아키텍처 §4) — `ReplicationScheduler`(원본 노드당 2개까지)가 가장 가까운 꼴.
- **비밀**: 원격 토큰·S3 비밀 키·저장소 키는 Raft 스펙 평문이고 API 보기·watch·감사는 `redact_json`(`backup_stores.rs:106`, `network_migrations.rs:109`)으로 뺀다.
- **미리 확인**(`crates/opl-api/src/vm_checks.rs`): `check_vm` — 권한 먼저(첫 거절에서 멈춤, 계획 71 리뷰 2 교훈), 독립 검사를 모으고 `prepare_with(Planned)`의 첫 오류를 칸(`area_of`)에. 만들 볼륨을 얹어 볼 수 있다(`MAX_PLANNED_VOLUMES` 16).
- **파일 내려받기 링크**: 파일 복원(계획 69) `POST …/download-link` → 짧게 사는 토큰 URL(`authz_routes.rs:442-447`).
- **웹**: 가상 머신 영역의 "가져오기" 탭(`nav.ts:65` `/imports`, 보기 없음), `web/src/imports/`(목록·OVA 올리기·`EsxiImportDialog`·`ImportPage` 실행 폼).
- **권한 이름**(`opl-types/src/authz.rs`): `vm.create`·…·`backup.file_restore`(마지막, operator에 없음). 기능 표 `FEATURES`의 마지막은 `vm-check`. `Kind::ALL` 39개.

## 4. 확인한 사실 (vSphere·CBT·스냅샷·OVF)

공개 문서(vSphere Web Services API 참조, VDDK 프로그래밍 안내, DMTF DSP0243 OVF 2.x, govmomi·virt-v2v·nbdkit 문서)로 확인한 것과, **실제 vCenter 없이 확인할 수 없는 것**을 나눠 적는다. 확인하지 못한 것은 "가정"으로 표시하고 §14에서 어떻게 메우는지 적는다.

- **SOAP vim25가 유일하게 다 되는 길이다.** vSphere Automation REST(`/api/vcenter/vm`)는 인벤토리는 쉽지만 CBT(`QueryChangedDiskAreas`)·스냅샷 세부·티켓이 없고 ESXi 단독 호스트에는 없다. SOAP는 vCenter(`apiType = VirtualCenter`)와 ESXi(`HostAgent`)에서 같은 메서드를 낸다. 필요한 메서드는 스무 개 남짓이라 WSDL 코드 생성 없이 XML 틀로 쓴다(govmomi·pyVmomi가 쓰는 같은 봉투 — `urn:vim25`, `xsi:type`, `ManagedObjectReference type="VirtualMachine"`).
- **세션**: `RetrieveServiceContent` → `SessionManager.Login(userName, password)` → 응답 쿠키 `vmware_soap_session`. 세션은 30분 쉬면 끊긴다(vCenter 기본) — 클라이언트가 `NotAuthenticated` 결함이면 한 번 다시 로그인한다. 끝나면 `Logout`.
- **인벤토리**: `ViewManager.CreateContainerView(rootFolder, [종류], recursive)` + `PropertyCollector.RetrievePropertiesEx`(쪽마다 `ContinueRetrievePropertiesEx`, `maxObjects` 500)로 VM·폴더·데이터센터·클러스터·호스트·데이터스토어·네트워크(표준 포트 그룹·DVS 포트 그룹·NSX opaque)의 필요한 속성만 한 번에 읽는다. VM 속성: `name`, `parent`, `config.uuid`·`instanceUuid`·`guestId`·`guestFullName`·`firmware`·`version`(vmx-NN)·`template`·`changeTrackingEnabled`·`hardware.numCPU`·`numCoresPerSocket`·`memoryMB`·`hardware.device`(디스크 backing `fileName`·`diskMode`·`thinProvisioned`·`changeId`, 컨트롤러, NIC backing — 포트 그룹 키), `runtime.powerState`·`host`·`faultToleranceState`, `snapshot`(나무), `guest.toolsRunningStatus`·`guest.net`(MAC·IP), `summary.storage.committed`, `datastore`, `resourcePool`. 분산 스위치 포트 그룹은 **키 → 이름**을 인벤토리에서 풀 수 있다(계획 30의 `.vmx`는 이름을 몰라 `dvs:{id}`로 보였다).
- **CBT**: `ReconfigVM_Task(changeTrackingEnabled = true)`는 다음 **stun/unstun**(스냅샷 만들기·지우기, 전원 켜기)부터 켜진다. 하드웨어 버전 7 이상만. `QueryChangedDiskAreas(vm, snapshot, deviceKey, startOffset, changeId)`는 `changeId` 이후 그 스냅샷까지 바뀐 구역 목록을 주고(한 번에 디스크 일부만 — `startOffset + length`로 이어 부른다), `changeId = "*"`이면 **할당된 구역 전체**를 준다(씬 디스크의 빈 곳을 건너뜀; NFS 데이터스토어는 전체를 줄 수 있다). `changeId`는 스냅샷의 디스크 backing에 적힌 값이고 그 스냅샷을 지운 뒤에도 다음 질의에 쓸 수 있다(백업 제품이 매번 스냅샷을 지우고 다음 번에 저장한 `changeId`를 쓴다). Storage vMotion·디스크 크기 바꾸기·CBT 끄기·`ctk` 파일 손상이면 옛 `changeId`가 무효가 되고 질의가 결함(`FileFault` 등)을 낸다 → 그 디스크를 `"*"`로 다시 전체 복사한다.
- **스냅샷과 기반 디스크**: 스냅샷이 없을 때 VM은 기반 디스크(`x.vmdk` → `x-flat.vmdk`)에 쓴다. 스냅샷 S1을 만들면 기반은 **얼고**(S1 시점 내용) 쓰기는 델타로 간다. 자식 S2가 있을 때 S1을 지우면(`RemoveSnapshot_Task(removeChildren=false, consolidate=true)`) S1의 델타가 **기반으로 합쳐지고** 기반은 이제 S2 시점 내용으로 다시 언다. 그래서 "S(n+1) 만들기 → S(n) 지우기"를 하면 **기반 flat 파일이 늘 가장 최근 스냅샷 시점의 일관된(crash-consistent) 내용**이다 — VDDK 없이 델타 체인(vmfsSparse·seSparse 형식)을 해석하지 않고 바뀐 구역을 기반 파일에서 그대로 읽을 수 있다(§9.2의 핵심). 이 계산은 VM에 우리 스냅샷만 있을 때만 맞으므로 **사용자 스냅샷이 있는 VM은 받지 않는다**(계획 30과 같다).
- **데이터스토어 파일 읽기**: `/folder/{경로}?dcPath=&dsName=`는 HTTP `Range`를 받는다(virt-v2v의 vCenter HTTPS 입력이 nbdkit curl 플러그인으로 이 URL을 범위 읽기로 쓴다). vCenter의 `/folder`는 호스트로 중계해 느리므로, vCenter면 `SessionManager.AcquireGenericServiceTicket(httpGet, https://{호스트}/folder/…)`로 받은 티켓(쿠키 `vmware_cgi_ticket`)으로 **VM이 있는 ESXi 호스트에 바로** 붙는다(govc `datastore.download`가 쓰는 길). 호스트 인증서는 vCenter가 아는 `HostSystem.config.certificate`(PEM)의 SHA-256으로 고정한다 — vCenter 지문을 믿었으니 그 vCenter가 알려 준 호스트 인증서를 믿는 사슬이다. **가정(실제 시험 못 함)**: (1) 켜진 VM의 스냅샷 아래 기반 flat 파일을 `/folder`로 읽을 수 있다(VMFS는 기반을 읽기 전용 다중 읽기 잠금으로 연다 — vmkfstools 복사는 되는 것으로 알려짐), (2) 티켓은 한 번 쓰면 끝날 수 있다 — 요청마다 새로 받는다고 보고 짠다, (3) 티켓이 거절되면 vCenter `/folder`로 내려간다.
- **vSAN·vVols**: 파일 시스템이 아니라 기반 flat 파일이 `/folder`로 보이지 않는다(객체). 이 데이터스토어의 디스크는 선택 길(VDDK)로만 읽는다 — 미리 확인이 "VDDK 없이 읽을 수 없음"으로 막는다.
- **VDDK(선택)**: VMware 독점 라이브러리(무료 내려받기, 재배포 금지). nbdkit의 `vddk` 플러그인(`nbdkit vddk libdir=… server=… user=… password=+파일 thumbprint=… vm=moref=vm-42 snapshot=snapshot-7 file="[ds] web/web.vmdk" transports=nbdssl:nbd`)이 스냅샷 시점의 논리 디스크를 NBD로 내놓는다 — 우리 NBD 클라이언트가 그대로 읽는다. VDDK 썸프린트는 **SHA-1**이다(vCenter 인증서의 SHA-1도 연결 때 같이 잰다). 설치·라이선스는 운영자 몫이고 CI에서는 nbdkit `file` 플러그인으로 배선만 본다.
- **무료 ESXi(vSphere Hypervisor)**: API가 읽기 전용이라 스냅샷·설정 바꾸기가 거절된다(`RestrictedVersion`) → 따뜻한 이전 불가, 차가운 이전(읽기만)은 된다. 미리 확인이 알린다.
- **전원**: `ShutdownGuest`(Tools 필요, 즉시 돌아옴 — `runtime.powerState`를 본다), `PowerOffVM_Task`, `PowerOnVM_Task`. FT 켠 VM은 스냅샷 불가. 템플릿은 켤 수 없고 스냅샷 불가 → 차가운 이전만.
- **정지(quiesce)**: 스냅샷의 `quiesce=true`는 Tools와 VSS가 필요하고 stun이 길어진다. 중간 회차는 crash-consistent여도 되고 **마지막 회차는 원본을 끈 뒤라 일관**하므로 늘 `quiesce=false, memory=false`다.
- **OVF**: DSP0243 — OVA는 ustar이고 서술자(`.ovf`)가 맨 앞, 매니페스트(`.mf`)·인증서는 서술자 바로 뒤나 끝, 나머지는 `References` 차례. vSphere가 받는 디스크 형식은 `http://www.vmware.com/interfaces/specifications/vmdk.html#streamOptimized`. `qemu-img convert -O vmdk -o subformat=streamOptimized`가 vSphere·ovftool이 받는 파일을 만든다(packer·여러 이미지 도구가 쓰는 길, QEMU 2.x 이후 끝 표식 고침). 하드웨어 버전 `vmx-14`(ESXi 6.7+)를 기본으로, `vmw:Config`(`firmware=efi`, `bootOptions.efiSecureBootEnabled`), 디스크 컨트롤러(SCSI `VirtualSCSI`(pvscsi)·SATA `vmware.sata.ahci`), NIC(`VmxNet3`·`E1000e`).
- **vcsim**(govmomi의 vCenter 흉내, Apache 2.0, 컨테이너 `vmware/vcsim`): govmomi가 실제 WSDL 타입으로 만든 응답을 내므로 **XML 봉투·네임스페이스·`xsi:type`·결함 모양**을 실제와 같은 꼴로 시험할 수 있다. 인벤토리·로그인·스냅샷·전원·`ReconfigVM`·데이터스토어 파일·OVF 가져오기(`govc import.ova`)가 된다. 디스크 내용·CBT 의미는 흉내가 얕다(`QueryChangedDiskAreas`가 없거나 빈 답일 수 있음 — 스테이지 2가 확인해 §18에 적는다). 그래서 **의미**(스냅샷 체인·합치기·CBT 구역·쓰기 중 회차)는 우리 가짜 vSphere가, **꼴**은 vcsim이 맡는다.

## 5. 전체 구조

```
가상 머신 › 가져오기 탭   [가져오기 | VMware 이전 | VMware 원본 | 내보내기]   ← 탭 안 보기(nav.ts views), 사이드바 그대로
   /imports            /vmware-migrations   /vmware-sources   /vm-exports      (늦게 읽는 조각 web/src/vmware/, web/src/exports/)

API(opl-api)                                        Raft 리소스(새 종류 넷)
  vmware_sources.rs   ── 연결·지문·찾아보기 ──→     vmware-source        { endpoint, user, password(가림), fingerprint, transport }
  vmware_migrations.rs── 계획·매핑·미리 확인 ─→     vmware-migration     { source, mode, rules, limits, sync, cutover } / status 요약
                                                    vmware-vm            VM 하나의 이전 상태(디스크마다 changeId·복사량·스냅샷·대상)
  vm_exports.rs       ── 내보내기·내려받기 ──→     vm-export            { vm, pool, options } / status(파일·크기·해시)
  vmware/ (클라이언트)  soap.rs 봉투·결함 · client.rs 세션·Task 기다리기 · inventory.rs · cbt.rs
                      reader.rs DiskReader { Folder(호스트 티켓 | vCenter /folder), Vddk(nbdkit) } · fake.rs(기능 fake-vsphere)

리더: VmwareScheduler(15초) — 계획마다 동시 수·호스트당·노드당·대역을 보며 vmware-sync Task를 만들고, 동기화 주기·예약 전환을 챙긴다
노드: Task vmware-sync(전체·델타, 이어 감) · vmware-cutover(이어 감) · vmware-revert · vmware-cleanup · vm-export · vm-export-delete

복사 길:  ESXi 호스트 ──HTTPS Range(티켓)── opld(대상 노드) ──NBD 쓰기── qemu-nbd ── 대상 볼륨(Pending, 어느 풀이든)
                         └ 또는 nbdkit-vddk ─NBD 읽기─┘
내보내기: 볼륨 ── qemu-img convert -O vmdk streamOptimized ──→ {풀}/orbivisor-exports/{id}/{vm}.ovf·.mf·-disk{i}.vmdk
          내려받기 = 그 파일들을 ustar로 흘려 보냄(크기를 미리 알아 Content-Length)
```

- **계획 29·30은 그대로 둔다.** `VmImport`(OVA·ESXi 한 대)와 그 API·화면·시험은 바뀌지 않는다. 새 일괄·따뜻한 이전은 새 리소스이고, 제안 규칙(`propose`)·`import_spec`의 VmSpec 만들기·`create_vm`을 함께 쓴다(§8.4).
- **복사는 업로드 디렉터리를 거치지 않는다.** 계획 30은 디스크를 통째로 `disk.raw`에 받은 뒤 `qemu-img convert`했다(디스크 크기만큼 여유, 두 번 씀). 계획 72는 대상 볼륨을 처음부터 만들어 `qemu-nbd`로 열고 받은 구역을 **바로 그 자리에** 쓴다 — 델타 회차가 같은 볼륨에 덮어써야 하므로 이 길이 필요하고, 여유 문제도 사라진다.
- **모든 회차는 다시 해도 같다(idempotent).** 한 회차 동안 원본 기반 파일은 얼어 있으므로, 회차를 중간에 끊고 처음부터 다시 해도 같은 바이트를 같은 자리에 쓴다. 그래서 이어 가기가 단순하다(§9.5).

## 6. 타입 (opl-types)

새 파일 `crates/opl-types/src/vmware.rs`(원본·계획·VM 항목)와 `vm_export.rs`. 모든 새 타입은 `deny_unknown_fields`(요청)·`ToSchema`, 빈 값은 쓰지 않는다. 지금 타입의 직렬화는 그대로다(바이트 호환 시험).

- **`VmwareSource = Resource<VmwareSourceSpec, VmwareSourceStatus>`**(`vmware-source`):
  - `VmwareSourceSpec { host, port: 443, user, password: Secret(String — Raft에만, `Debug`·보기·watch·감사에서 `<redacted>`), fingerprint(대문자 콜론 SHA-256, 필수), transport: Folder(기본) | Vddk { libdir?: 노드 설정 기본, thumbprint_sha1(만들 때 잼) }, datacenter_hint?: 문자열(ESXi `ha-datacenter`) }`.
  - `VmwareSourceStatus { kind: VCenter | Esxi(about.apiType), version(about.version·build), instance_uuid, checked_at, error?, restricted(무료 ESXi) }` — 만들기·"다시 확인"이 채운다.
- **`VmwareMigration = Resource<MigrationSpec, MigrationStatus>`**(`vmware-migration`):
  - `MigrationSpec { source: Uuid, mode: Warm | Cold, rules: MigrationRules, limits: MigrationLimits, sync: SyncPolicy, cutover: CutoverPolicy, folder?: 계획 리소스가 사는 폴더(권한용) }`.
  - `MigrationRules { networks: Vec<{ from: 원본 네트워크(이름 또는 `moref:dvportgroup-12`), to: 우리 네트워크 }>, datastores: Vec<{ from: 데이터스토어 이름, to: 풀 }>, default_pool?, folders: FolderRule(Flat { into?: 폴더 } | Mirror { under?: 폴더 } — Mirror는 VMware VM 폴더 경로를 그 아래에 만든다), node: Auto | Node(id), devices: Auto(지금 `propose`) | Fixed { disk_bus, nic_model }, keep_macs: true, reserve_ips: true(SDN이면 Tools IP 예약), driver_iso?: Windows VM에 CD로 붙일 ISO 볼륨, start_after: true, cpu_model? }`.
  - `MigrationLimits { max_parallel: 4, max_per_host: 2, max_per_node: 2, streams_per_disk: 4, bandwidth_mib_s?: u32 }`.
  - `SyncPolicy { interval_min: 60(따뜻한 이전의 델타 주기, 5~1440), stop_after_days?: 14 }`.
  - `CutoverPolicy { at?: RFC 3339 시각(예약 전환 — 그 시각에 `ready` VM 모두), shutdown: Guest(기본, Tools 없으면 막힘) | PowerOff, shutdown_timeout_s: 600, protect_source: true(§11.3), max_final_delta_gib?: 막는 한도 }`.
  - `MigrationStatus { phase: Draft | Running | Paused | Finished | Cancelled, counts: { 상태별 VM 수 }, error? }` — 요약만(진행은 항목·Task에).
- **`VmwareVm = Resource<VmwareVmSpec, VmwareVmStatus>`**(`vmware-vm`, 이름 `{계획}-{원본 이름 다듬음}`): 계획 하나의 VM 하나. 따로 둔 까닭은 동시 Task가 한 리소스를 서로 고쳐 revision이 부딪치지 않게, 권한(대상 폴더)이 VM마다 다르게, watch가 VM마다 오게(§15).
  - `VmwareVmSpec { migration, source_ref: "vm-123"(moref), instance_uuid(moref가 바뀌어도 같은 VM인지), target_name, overrides: Option<ImportRun 꼴 부분 값>, node?: 고른 노드(미리 확인·시작 때 정함) }`.
  - `VmwareVmStatus { state: VmwareVmState, source: SourceVm(계획 29 타입 재사용 + `guest_ips`, `host`, `tools`, `power`), disks: Vec<SyncDisk { key: 장치 키, file: "[ds] web/web-flat.vmdk", capacity_bytes, volume: Uuid, change_id?, copied_bytes, rounds, last_delta_bytes }>, snapshot?: { moref, name, created_at }, cbt_enabled_by_us, rounds, last_sync_at?, next_sync_at?, estimate_s?(마지막 델타 바이트 ÷ 잰 속도), target_vm?: Uuid, source_changes: { powered_off_by_us, nics_disconnected: Vec<장치 키>, annotation_added }, warnings, error?, history: Vec<{ at, event }>(최근 20개) }`.
  - `VmwareVmState`: `Pending`(미리 확인 통과, 차례 기다림) → `Copying`(전체 복사) → `Syncing`(따뜻한 이전: 델타 주기 사이) | `Ready`(차가운 이전: 복사 끝, 전환 기다림; 따뜻한 이전도 첫 델타 뒤 `Ready`로 보임 — `Syncing`과 같은 뜻의 화면 이름) → `CuttingOver { step }` → `Done`(대상 VM 켜짐·원본 꺼짐, 되돌릴 수 있음) → `Finalized`(우리 스냅샷·CBT 정리, 되돌릴 수 없음). 곁가지: `Failed`(다시 하기 가능, 이유), `RolledBack`(전환 실패 → 원본 다시 켬, 다시 동기화로), `Reverted`(Done 뒤 손 되돌리기), `Cancelled`(우리 스냅샷 지움, 대상 볼륨 지움 — 원본 그대로).
  - `CutoverStep`: `Preparing` → `StoppingSource` → `FinalSync` → `CreatingVm` → `StartingVm`(이 단계가 끝나면 `Done`). 각 걸음은 Raft에 먼저 적고 한다(이어 가기·되돌리기가 어디까지 했는지 안다 — §10).
- **`VmExport = Resource<VmExportSpec, VmExportStatus>`**(`vm-export`): `VmExportSpec { vm: Uuid, pool: 이름(dir·NFS), guest_os?: VMware guestId, hw_version: 14, keep_macs: false, node(쓴 노드) }`, `VmExportStatus { phase: Pending | Writing | Ready | Failed, files: Vec<{ name, size, sha256 }>, ova_bytes, warnings, error? }`.
- **Task**(`TaskOp`): `VmwareSync { vm: Uuid, full: bool }`(`vmware-sync`), `VmwareCutover { vm }`(`vmware-cutover`), `VmwareRevert { vm }`(`vmware-revert`), `VmwareCleanup { vm, delete_target: bool }`(`vmware-cleanup` — 취소·마무리), `VmExport { export }`(`vm-export`), `VmExportDelete`(`vm-export-delete`). 줄(queue)은 `vmware-vm` id(같은 VM의 Task가 겹치지 않음). `VmwareSync`·`VmwareCutover`는 **이어 가는 Task**(`tasks.rs` 목록에 더함), 이전·내보내기 Task는 시간 제한 없음(대신 멈춤 한도 §9.5).
- **기능**: `vmware-migrate`(원본·계획·항목·Task), `vm-export`(내보내기). `FEATURES` 끝에 둘. 새 종류는 `Kind::ALL` 39 → 43, `WATCHABLE`에 넷(원본은 가린 JSON).
- **권한**: 새 `vm.export`(`VmExport` — VM 디스크 전체가 클러스터 밖으로 나가므로 `backup.file_restore`처럼 operator에 넣지 않고 admin이 준다). 원본 연결은 `cluster.admin`(vCenter 관리자 비밀번호를 든다 — 원격과 같음), 찾아보기·계획은 루트 `vm.create`, 항목의 전환·되돌리기는 대상 폴더의 `vm.create` + `vm.power`.

## 7. vSphere 클라이언트 (`crates/opl-api/src/vmware/`)

### 7.1 연결과 세션

- `esxi.rs`의 지문 고정 TLS 연결(`connect`, `PinnedCert`)을 `vmware/http.rs`로 꺼내 **연결 재사용**(HTTP/1.1 keep-alive, 연결 둘까지 — SOAP 하나·파일 읽기 여럿)과 **POST·Range·쿠키**를 더한다. 계획 30 코드는 이 모듈을 부르게만 바꾼다(동작 그대로, 시험 그대로).
- `Client::login(source) → Session`: `RetrieveServiceContent` → `about.apiType`·`instanceUuid`·`version` → `Login`. 쿠키를 들고, `NotAuthenticated` 결함이면 한 번 다시 로그인. 세션은 Task 하나·API 요청 하나가 쓰고 `Drop`에서 `Logout`을 띄워 보낸다(끝까지 기다리지 않음). 노드마다 원본별 세션 캐시(5분 쉬면 닫음)를 두어 찾아보기·회차마다 로그인하지 않는다.
- 결함(`soapenv:Fault` → `detail`의 `xsi:type`)은 `VsphereError::{Auth, NoPermission{privilege}, InvalidState, FileFault, Restricted, NotFound, Fault{type, message}, Http, Connect, Fingerprint}`로 나누고 API 코드는 `vmware_auth` 400, `vmware_permission` 403-아님 400(우리 권한과 헷갈리지 않게), `vmware_unreachable` 502, `untrusted_certificate`·`fingerprint_mismatch` 409(계획 30과 같음).
- 한도: 응답 64 MiB(인벤토리 쪽마다), 요청마다 60초, 파일 읽기 조각마다 60초.

### 7.2 쓰는 메서드

| 무엇 | 메서드 |
|---|---|
| 세션 | `RetrieveServiceContent`, `Login`, `Logout`, `AcquireGenericServiceTicket` |
| 인벤토리 | `CreateContainerView`, `DestroyView`, `RetrievePropertiesEx`, `ContinueRetrievePropertiesEx` |
| Task | `RetrievePropertiesEx(Task, info.state·progress·error·result)`를 1초 → 5초 간격으로(최대 Task마다 한도 — 스냅샷 지우기 4시간) |
| CBT·스냅샷 | `ReconfigVM_Task`(`changeTrackingEnabled`, `annotation`, NIC `connectable.startConnected`), `CreateSnapshot_Task`, `RemoveSnapshot_Task`, `QueryChangedDiskAreas` |
| 전원 | `ShutdownGuest`, `PowerOffVM_Task`, `PowerOnVM_Task` |
| 파일 정보 | `HostDatastoreBrowser.SearchDatastore_Task`(flat 파일 크기·수정 시각 — 차가운 이전의 "그새 켜졌나") |

XML은 `vmware/soap.rs`의 작은 쓰개(이스케이프·`ManagedObjectReference`·`xsi:type` 넣기)와 `roxmltree` 읽개(로컬 이름 비교 — 계획 29 OVF와 같은 규칙)로 다룬다. 메서드마다 요청 틀과 응답 해석 함수 한 쌍이고, 견본 XML(`tests/fixtures/vsphere/*.xml` — vcsim과 govmomi 시험 자료 모양을 따른 것)로 단위 시험한다.

### 7.3 인벤토리

`inventory(session) → Inventory { datacenters, folders, clusters, hosts, datastores, networks, vms }` — 종류마다 `RetrievePropertiesEx` 한 번(쪽 나눔)이고 나무는 `parent`로 잇는다. VM 요약 `InvVm { moref, name, path("DC/vm/폴더/…"), host, cluster, power, guest_id, firmware, hw_version, cpus, cores_per_socket, memory_mib, disks[{ key, file, capacity, mode, thin, datastore, controller }], nics[{ key, network(이름 — DVS는 포트 그룹 이름), adapter, mac }], snapshots: 개수와 우리 것인지, cbt, tools, ft, template, guest_ips, committed_bytes }`. 1 000대 인벤토리는 쪽 둘 — vcsim 기본 인벤토리(2 데이터센터·호스트 8·VM 32)와 가짜 vSphere 큰 판(VM 2 000)으로 시간·메모리를 잰다(§18).
`SourceVm`은 `InvVm`에서 만든다(계획 29 타입) — 그래서 `propose`·`import_spec`를 그대로 쓴다. 디스크 순서는 컨트롤러 종류(ide·sata·scsi·nvme) → 버스 → 유닛(계획 30 `.vmx` 규칙과 같다, `bios.hddOrder`는 `extraConfig`에서).

### 7.4 디스크 읽기 (`DiskReader`)

```rust
trait DiskReader { async fn read(&mut self, offset: u64, len: u32) -> Result<Bytes>; fn size(&self) -> u64; }
```

- **`FolderReader`(기본, 순수 Rust)**: 대상 파일은 디스크 backing `fileName`(`[ds] web/web.vmdk`)의 디스크립터를 읽어(계획 30 `parse_descriptor`·`check_flat`) 얻은 flat extent(`web-flat.vmdk`). vCenter면 범위 읽기마다 호스트 티켓을 받아 VM이 있는 ESXi 호스트(`runtime.host`의 관리 주소)에 `GET /folder/…` + `Range: bytes=a-b` + `Cookie: vmware_cgi_ticket=…`, 호스트 인증서는 `HostSystem.config.certificate`로 고정. 티켓이 거절되거나(401·403) 호스트에 닿지 않으면 그 Task 동안 vCenter `/folder`(같은 Range)로 내려가고 경고를 남긴다. ESXi 원본은 그 호스트에 바로(세션 쿠키). 읽기는 4 MiB 조각, 디스크마다 `streams_per_disk`개를 동시에.
- **`VddkReader`(선택)**: 노드 설정 `[vmware] nbdkit = "/usr/sbin/nbdkit"`, `vddk_libdir = "/opt/vmware-vix-disklib-distrib"`가 있고 원본 `transport = vddk`일 때. nbdkit을 유닉스 소켓(0600)으로 띄우고(비밀번호는 `password=-`로 표준 입력 — argv·환경에 비밀 없음), 우리 `NbdClient`로 읽는다. 스냅샷 시점의 논리 디스크를 주므로 vSAN·vVols도 된다. 읽는 구역은 같은 CBT 목록이다.
- 둘 다 0만 든 조각은 쓰지 않고 `NBD_CMD_WRITE_ZEROES`(씬 대상이 씬으로 남음).

### 7.5 가짜 vSphere (`vmware/fake.rs`, 기능 `fake-vsphere`)

시험·데모용 서버(axum + 자체 서명 TLS — 가짜 ESXi와 같은 바탕). 계획 68의 가짜 S3처럼 crate 기능이라 opl-api 시험·opld 시험·웹 E2E가 함께 쓰고, 개발용 실행 파일 `fake-vsphere`(`required-features`)가 svc01·브라우저 시험에 띄운다.

- 상태: 데이터센터·폴더·클러스터·호스트 둘·데이터스토어(VMFS 둘, vSAN 하나)·표준·DVS 포트 그룹·VM들. 디스크는 메모리(또는 임시 파일) **기반 + 델타 체인**이고 스냅샷 만들기·지우기(합치기)·CBT(`changeId` = `"52 …/N"` 꼴, 64 KiB 단위로 바뀐 블록 기록)를 **실제 의미대로** 흉내 낸다.
- 시험 손잡이: `write(vm, disk, offset, bytes)`(켜진 VM의 게스트 쓰기 — 델타로 감), `power`, 결함 주입(다음 `RemoveSnapshot` 실패, CBT 초기화, 범위 읽기 중 끊기, 느리게, 티켓 거절, 세션 만료, `ShutdownGuest` 무시), 요청 기록(Authorization·쿠키·티켓이 어디로 갔는지).
- 메서드 표 §7.2 전부와 `/folder` Range, 호스트 티켓. 응답 XML은 vcsim 응답을 견본 삼아 같은 네임스페이스·`xsi:type`으로 쓴다(§14의 vcsim 시험이 둘이 같은 꼴인지 본다).

## 8. 원본·인벤토리·이전 계획·미리 확인

### 8.1 원본 연결

- `POST /vmware-sources`(`cluster.admin`, 기능): 본문 `{ name, host, port?, user, password, fingerprint?, transport? }`. 지문이 없으면 연결만 해 409 `untrusted_certificate`(`details.fingerprint` — 계획 30과 같은 흐름), 있으면 로그인 → `status`를 채워 201. 비밀번호는 응답·watch·감사에 없다(`redact_json` 꼴). `transport = vddk`면 연결 때 SHA-1 썸프린트도 재어 넣는다.
- `GET /vmware-sources[/{key}]`(`cluster.read`), `PATCH`(비밀번호·지문·사용자·전송 — 쓰는 계획이 있어도 됨, 다음 Task부터), `DELETE`(쓰는 계획이 있으면 409 `source_in_use`), `POST /vmware-sources/{key}/actions/check`(다시 로그인·버전).
- 계획 30의 `EsxiConnection` 한 번 쓰기 흐름은 그대로다(ESXi 한 대를 연결 저장 없이 가져오기).

### 8.2 찾아보기

`GET /vmware-sources/{key}/inventory?refresh=`(루트 `vm.create`): §7.3 결과를 노드 메모리에 60초 담아 두고(같은 노드의 다시 그리기·거르기가 vCenter를 다시 두드리지 않게) 나무로 준다. 웹은 나무 + 표(찾기·전원·OS·크기·스냅샷·CBT·Tools·이미 계획에 든 VM 표시). 비밀은 응답에 없다.

### 8.3 이전 계획 만들기

`POST /vmware-migrations`(루트 `vm.create`, 기능): `{ name, source, mode, rules, limits?, sync?, cutover?, vms: [{ ref, target_name?, overrides? }] }`(VM 200대까지 — 넘으면 계획을 나눈다). 같은 VM(`instance_uuid`)이 끝나지 않은 다른 계획에 있으면 409 `vm_in_migration`. 만들면 `phase: Draft`, 항목마다 `vmware-vm`(`Pending` 아님 — `Draft` 계획의 항목은 상태 없이 미리 확인만). `PATCH`는 `Draft`·`Paused`에서 규칙·한도·주기·전환 정책·VM 더하기/빼기(시작한 VM은 못 뺌 — 취소). `POST …/actions/{start|pause|resume|cancel}`, `DELETE`(끝났거나 취소된 계획만, 항목도 지움 — 만든 VM·볼륨·원본은 그대로).

### 8.4 매핑 → VmSpec

VM마다 `SourceVm` + 규칙 + 덮어쓰기 → `ImportRun`(계획 29 타입) → `import_spec`(지금 함수) → VmSpec. 규칙 차례: 덮어쓰기 > 규칙 > `propose`. 데이터스토어 → 풀은 **디스크마다**(디스크가 다른 데이터스토어에 있으면 다른 풀 — `ImportRun.pool` 하나로는 안 되므로 `import_spec`에 디스크별 풀 표를 받는 판 `import_spec_with(pools: &[String])`을 더하고 지금 함수는 같은 풀 표로 부른다). 폴더 `Mirror`는 원본 VM 폴더 경로의 우리 폴더를 만들 계획으로(만들기는 시작 때, `inventory.folder` 권한). 노드 `Auto`는 시작할 때 고른다(풀이 보이고 메모리 여유 — 계획 71 `prepare_with`가 고르는 노드, 켜진 채 만들 때와 같은 `schedule_with`).

### 8.5 미리 확인

`POST /vmware-migrations/{key}/preflight`(또는 `vms` 일부) → 200 `{ vms: [{ ref, level: ok|warn|blocked, problems[{ at, code, message }], node?, spec_preview }] }`. VM마다:

1. **우리 쪽**: 계획 71 `vm_checks::check_vm`을 만들 VmSpec과 만들 볼륨(디스크마다 `Planned` 볼륨 — 크기 = capacity, 풀)·폴더로 부른다. 권한 먼저·첫 거절에서 멈춤(계획 71 리뷰 2 교훈 그대로 — 숨은 풀·네트워크 성질이 새지 않게). 할당량·풀 용량·이름·MAC 겹침·네트워크 매핑 없음·SATA 개수·CPU 모델·노드.
2. **VMware 쪽**(인벤토리 값, 원본 호출 없음): 사용자 스냅샷 있음(blocked "consolidate or delete snapshots first" — 따뜻한·차가운 둘 다, 계획 30과 같음), 독립 디스크(`independent_*` — 스냅샷이 건너뛰므로 따뜻한 이전 blocked, 차가운 이전은 꺼져 있으면 ok), RDM blocked, vSAN·vVols 디스크(전송 `folder`면 blocked "needs the VDDK transport"), FT 켬(따뜻한 blocked), 템플릿(차가운만), 하드웨어 버전 < 7(따뜻한 blocked), 무료 ESXi(`restricted` — 따뜻한 blocked), 켜져 있는데 차가운 계획(blocked "power it off or use a warm migration"), Tools 없음 + `shutdown: guest`(warn → 전환 때 막힘), 멀티라이터 디스크 blocked, 디스크 32개·NIC 16개 넘음(계획 29 한도), 같은 VM이 다른 계획에(blocked).
3. **게스트 힌트**(warn): §11의 RHEL 계열 initramfs, Windows SATA·e1000e와 드라이버 ISO, UEFI NVRAM(계획 29 경고), 정적 IP와 NIC 바뀜.

`start`는 미리 확인을 다시 돌려 `blocked` VM은 건너뛰고(그 항목 `Failed` + 이유), 나머지를 `Pending`으로 둔다. CLI `vmware migration preflight`·웹 표가 같은 응답을 쓴다.

## 9. 복사 엔진 — 전체 복사와 CBT 델타

### 9.1 대상 볼륨과 쓰기 길

- 시작한 VM의 첫 `vmware-sync` Task가 디스크마다 볼륨 `{target_name}-disk{i}`(풀의 기본 형식, 크기 = capacity, `Pending`, 레이블 `orbivisor.io/vmware-vm={항목 id}`)를 만들어 `status.disks[].volume`에 먼저 적는다.
- 쓰기: `opl-driver` `nbd_target(qemu_nbd, src, socket)` — `nbd_source`의 쓰기 판(`--read-only` 없음, `--cache=none --aio=native`(파일), `--discard=unmap --detect-zeroes=unmap`, 연결 하나). `opl-store` `NbdClient`에 `write`·`write_zeroes`(`NBD_CMD_FLAG_NO_HOLE` 없이 — 씬 유지)·`flush`를 더한다. RBD·LVM·ZFS 풀도 `qemu-nbd`가 같은 인자로 연다(계획 68이 읽기로 확인한 원본 인자).
- 회차 끝마다 `flush` → `qemu-nbd` 닫기. 대상 볼륨은 전환 전까지 `Pending`이라 어떤 VM에도 붙지 않는다. 노드 시작 sweep은 `orbivisor.io/vmware-vm` 레이블의 볼륨을 **그 항목이 살아 있으면** 지우지 않는다(§3).

### 9.2 회차(round) 알고리즘

```
첫 회차(full):
  1. CBT가 꺼져 있으면 ReconfigVM(changeTrackingEnabled=true), cbt_enabled_by_us = true   [Raft에 먼저 적음]
  2. CreateSnapshot(S1, memory=false, quiesce=false)  → 기반이 얼고 CBT가 켜짐            [snapshot = S1]
  3. 디스크마다 QueryChangedDiskAreas(S1, key, 0.., "*") → 할당된 구역
  4. 구역을 4 MiB 조각으로 기반 flat에서 읽어 대상에 씀(0 조각은 write_zeroes)            [copied_bytes 체크포인트]
  5. change_id[d] = S1 디스크 backing의 changeId                                          [Raft]
다음 회차(delta, n → n+1):
  1. CreateSnapshot(S(n+1))
  2. RemoveSnapshot(S(n), consolidate)  → 기반 = S(n+1) 시점                              [snapshot = S(n+1)]
  3. QueryChangedDiskAreas(S(n+1), key, 0.., change_id[d]) → 바뀐 구역
  4. 읽고 씀 (같음)
  5. change_id[d] = S(n+1)의 changeId, last_delta_bytes, 잰 속도 → estimate_s
우리 스냅샷이 하나도 없으면(되돌림 뒤 등) 1단계 만들기만 하고 2단계는 건너뛴다 — 저장한 change_id로 그대로 잇는다.
차가운 이전(꺼진 VM): 스냅샷·CBT 없이 기반 전체를 읽는다(0 조각 건너뜀). CBT가 이미 켜져 있고 changeId가 있으면 "*"로 빈 곳을 건너뛴다.
  복사 시작 때 flat 파일의 수정 시각(SearchDatastore_Task)을 적고, 전환 때 다시 봐서 달라졌으면(그새 켜졌다) 다시 전체 복사를 요구한다.
```

- 회차 1-2단계 사이에 스냅샷이 둘이 되는 짧은 때가 있다(지우기 전). 지우기가 실패하면 회차를 멈추고 `Failed`("cannot remove snapshot S(n): …; the VM keeps two Orbivisor snapshots") — 다음 시도는 두 스냅샷을 알고(`snapshot`·`pending_snapshot` 둘 다 Raft에) 먼저 옛 것을 지운다.
- 스냅샷 이름 `orbivisor-migration-{항목 id 앞 8자}-{n}`, 설명에 계획 이름·클러스터 이름. 우리 것인지는 Raft의 moref로만 판단한다(이름은 표시).
- `changeId`가 무효(결함)면 그 디스크만 `"*"` 전체로 다시(경고 "changed block tracking was reset on disk N; copying it again in full"). 디스크 capacity가 커지면 대상 볼륨을 늘리고(지금 볼륨 늘리기) `"*"` 전체. 디스크 수·키가 바뀌면 `Failed` "the VM's disks changed; cancel and start this VM again".
- 사용자 스냅샷이 생기면(백업 제품 등) 회차를 건너뛰고 경고 — 우리 스냅샷만 남을 때 이어 간다. 계획 문서·가이드: 이전 동안 그 VM의 VMware 백업 작업을 멈추라.

### 9.3 스케줄러 (`VmwareScheduler`, 리더 루프 15초)

- 계획이 `Running`이면 항목 상태를 보고 Task를 만든다: `Pending` → `vmware-sync{full}`(동시 수 안에서), `Syncing`이고 `next_sync_at`이 지남 → `vmware-sync{delta}`, 예약 전환 시각이 지남 → `Ready`·`Syncing` 항목마다 `vmware-cutover`(같은 동시 수).
- 한도: 계획의 끝나지 않은 `vmware-sync`·`vmware-cutover` 수 ≤ `max_parallel`, 같은 ESXi 호스트(`source.host`) ≤ `max_per_host`, 같은 대상 노드 ≤ `max_per_node`. 전체 복사가 델타보다 먼저 줄을 기다리지 않게 **델타·전환이 먼저**(이미 내려 둔 VM의 주기를 지킴).
- `stop_after_days`가 지나도록 전환하지 않은 따뜻한 항목은 동기화를 멈추고 경고(원본에 스냅샷이 오래 남지 않게 — 멈출 때 우리 스냅샷을 지우고 다시 시작하면 `"*"` 대신 changeId로 이어 간다).
- 진행은 Raft(`vmware-vm`)에 있으므로 리더가 바뀌어도 다음 틱에 잇는다(백업 작업과 같은 규칙).

### 9.4 대역·속도

`bandwidth_mib_s`가 있으면 계획의 Task들이 나눠 쓰는 토큰 통(Task마다 몫 = 한도 ÷ 동시 수, 노드 안 공유). 진행률은 Task `progress`(단계 `snapshot`·`copying`·`consolidating`, 바이트/전체). 화면의 남은 시간은 잰 속도로.

### 9.5 이어 가기와 멈춤

- `vmware-sync`는 이어 가는 Task다. 다시 뜬 opld는 Raft의 `snapshot`·`pending_snapshot`·`copied_bytes`(전체 복사는 512 MiB마다·30초마다 디스크별 체크포인트 — 구역 목록 안 위치)로 걸음을 정한다: 스냅샷을 만들던 중이면 원본 스냅샷 나무에서 우리 이름의 것을 찾아 적고, 복사 중이면 체크포인트부터(얼어 있는 기반이라 같은 내용), 합치기 중이면 그 vSphere Task를 다시 기다린다.
- 시간 제한은 없고 **멈춤 한도**만: 바이트가 10분 늘지 않으면 실패(다음 틱에 다시 — 항목 `error`에 남기고 3번 연달아면 `Failed`).
- 원본 노드(대상 노드)가 오프라인이면 janitor가 Pending Task를 실패로 끝내고(지금 규칙), 스케줄러는 노드가 돌아올 때까지 그 항목을 건너뛴다(대상 볼륨이 그 노드의 로컬 풀일 수 있다).

## 10. 전환·되돌리기·마무리

### 10.1 전환 (`vmware-cutover`, 이어 가는 Task)

`POST /vmware-vms/{key}/cutover`(대상 폴더 `vm.create` + `vm.power`) 또는 예약 시각. `Ready`·`Syncing`만(아니면 409 `not_ready`). 미리 확인(§8.5 1단계)을 다시 돌려 막히면 시작하지 않는다(원본을 건드리기 전에 실패).

| 걸음 | 하는 일 | 실패하면 |
|---|---|---|
| `Preparing` | 따뜻한 이전: 마지막 델타를 **켜진 채** 한 번 더(크기를 줄임 — `max_final_delta_gib` 넘으면 멈추고 알림). 차가운 이전: 원본이 꺼져 있고 flat 수정 시각이 복사 때와 같은지 | 원본 그대로, `Syncing`·`Ready`로(되돌릴 것 없음) |
| `StoppingSource` | `protect_source`면 원본 NIC `startConnected=false`·설명 "migrating to Orbivisor …"(되돌릴 값 Raft에 먼저), `ShutdownGuest`(→ `shutdown_timeout_s` 기다림) 또는 `PowerOffVM`, `powered_off_by_us` | 시간 넘김(게스트가 안 꺼짐): 원본을 그대로 두고(켜져 있으면 켜진 채) NIC 설정 되돌림, `Syncing` + 오류 |
| `FinalSync` | (차가운 이전은 건너뜀) 꺼진 원본에 S(final) → S(n) 지우기 → 바뀐 구역 → 대상에 씀 → S(final) 지우기(기반 = 마지막 내용, 원본 디스크에 우리 스냅샷 없음) | 자동 되돌림(§10.2) |
| `CreatingVm` | 볼륨을 `Ready`로, `create_vm`(VmSpec — 매핑·MAC·폴더·`driver_iso` CD), `target_vm` | 자동 되돌림(만든 VM 지움, 볼륨은 남김 — 다시 하면 쓴다) |
| `StartingVm` | `start_after`면 켜기 요청 → `Running` 보임(5분) | 자동 되돌림(대상 VM 끄고 지움) |
| → `Done` | 원본은 꺼진 채, 대상 켜짐. `history`에 내린 시간(StoppingSource 시작 → StartingVm 끝) | — |

### 10.2 자동 되돌림(`RolledBack`)

`StoppingSource` 이후 실패면: 대상 VM을 만들었으면 끄고 지움(볼륨은 `Pending`으로 되돌려 남김) → 원본을 `powered_off_by_us`면 켬(`PowerOnVM`) → NIC·설명 되돌림 → 상태 `RolledBack`(=`Syncing`과 같이 다음 회차가 이어 감 — 원본이 다시 켜져 바뀐 것은 다음 델타가 옮긴다). 되돌림 자체가 실패하면(vCenter 닿지 않음) 상태를 `Failed`로 두고 **어느 걸음까지 했는지**와 손으로 할 일("power on web01 in vCenter; its NICs are disconnected at power-on")을 오류에 적는다. 원본을 지우는 걸음은 어디에도 없다.

### 10.3 켠 뒤 되돌리기(`revert`)와 마무리(`finalize`)

- `POST /vmware-vms/{key}/revert`(Done만, 본문 `{ confirm: "<대상 VM 이름>" }`): 대상 VM 끄기 → 원본 NIC·설명 되돌림 → 원본 켜기 → `Reverted`. **대상에서 쓴 데이터는 원본으로 가지 않는다**(경고 문구, 웹 확인 대화상자). 대상 VM과 볼륨은 지우지 않고 꺼진 채 남긴다(사용자가 지움). 다시 이전하려면 그 VM을 새 계획에 넣는다.
- `POST /vmware-vms/{key}/finalize`(Done만, 또는 계획 `finalize_after_days` 뒤 자동은 넣지 않음 — 손으로): `vmware-cleanup` — 원본에 남은 우리 스냅샷이 있으면 지움, `cbt_enabled_by_us`면 CBT 끄기(다음 stun부터), `protect_source`의 NIC 설정은 **그대로**(원본을 실수로 켜도 같은 MAC·IP가 네트워크에 붙지 않게 — 가이드에 적음) → `Finalized`.
- `cancel`(`Pending`~`Syncing`·`Failed`·`RolledBack`): 우리 스냅샷 지움, CBT 되돌림, 대상 볼륨 지움 → `Cancelled`. 원본 그대로.

## 11. 게스트 맞추기·MAC/IP·원본 안전

- **장치는 지금 규칙**(`propose`): Windows → SATA·e1000e(드라이버 없이 켜짐 — Proxmox 가져오기 마법사와 같은 선택), Linux → virtio-blk·virtio. `rules.devices = Fixed`나 VM 덮어쓰기로 바꾼다.
- **Windows 드라이버 ISO**: `rules.driver_iso`(virtio-win ISO 볼륨 — 계획 57 ISO)가 있으면 Windows VM에 CD로 붙인다. 가이드: 켠 뒤 드라이버를 깔고 디스크·NIC를 virtio로 바꾸기(계획 54 미뤄진 변경).
- **Linux initramfs**: RHEL 계열(`guestId`가 `rhel*`·`centos*`·`oracleLinux*`·`rocky*`·`alma*`) 게스트는 dracut `hostonly`라 initramfs에 virtio 모듈이 없을 수 있다 — 미리 확인 warn: "run `dracut -f --add-drivers 'virtio_blk virtio_net virtio_pci'` (or `--no-hostonly`) in the guest before cutover". **따뜻한 이전이면 원본에서 고친 것이 다음 델타로 옮겨진다** — 게스트를 건드리는 도구 없이 이 문제를 푸는 길이고, 가이드의 전환 전 점검표 첫 줄이다. 고치지 않고 전환해 켜지지 않으면 `revert` 또는 VM을 `sata`로 바꿔 다시 켠다(가이드).
- **MAC**: `keep_macs: true`(기본, 계획 29와 같다) — DHCP 예약·라이선스가 MAC에 묶인 경우를 지킨다. 대상 VM을 만들기 전에 원본이 꺼져 있음을 확인하므로(전환 걸음) 같은 MAC이 둘 켜지지 않는다.
- **IP**: Tools가 알린 `guest.net`(MAC → IP)을 `source.guest_ips`에 적어 화면에 보이고, 대상 네트워크가 SDN이고 `reserve_ips`면 그 IP를 `ImportRun.ips`로 IPAM 예약(서브넷 밖이면 warn). 게스트 안 정적 IP는 NIC 종류가 바뀌어 장치 이름(`ens192` → `enp1s0`)·Windows 어댑터가 새것이 되면 적용되지 않을 수 있다 — 미리 확인 warn과 가이드(원본에서 MAC으로 묶는 설정을 미리 바꾸면 델타로 옮겨진다).
- **원본 안전 규칙**(시험으로 지킴): (1) 원본 VM·디스크·사용자 스냅샷을 지우거나 바꾸는 호출이 코드에 없다 — 원본에 하는 호출은 §7.2 표뿐이고 `RemoveSnapshot`은 Raft에 적힌 우리 moref에만, (2) 원본을 끄는 것은 전환 걸음뿐, (3) 바꾼 원본 설정(CBT·NIC `startConnected`·설명)은 바꾸기 전에 옛 값을 Raft에 적고 되돌림·취소가 되돌린다, (4) 가짜 vSphere 시험이 모든 경로 끝에 원본 디스크 바이트·스냅샷 나무가 그대로인지 본다.

## 12. OVA/OVF 내보내기

### 12.1 요청과 Task

- `POST /vms/{key}/export`(그 VM의 `vm.export`, 기능 `vm-export`): `{ pool, guest_os?, hw_version?, keep_macs? }` → 202 `{ export, task }`. VM이 꺼져 있어야 한다(409 `vm_running` — 켜진 VM은 §16), 템플릿도 된다. 풀은 dir·NFS(파일을 둘 수 있는 풀, 아니면 400 `pool_not_file`)이고 VM 디스크 합 + 1 GiB 여유(507). 내보내는 동안 VM 켜기·디스크 바꾸기는 409 `vm_exporting`(VM 줄에 Task — 지금 줄 규칙).
- Task `vm-export`(풀이 보이는 노드 — VM 디스크 풀과 내보낼 풀이 둘 다 보이는 노드를 고름): 디렉터리 `{풀}/orbivisor-exports/{export id}/`에 디스크마다 `qemu-img convert -O vmdk -o subformat=streamOptimized,adapter_type=lsilogic,hwversion=…` (`opl-driver` `export_vmdk` — `export_volume`의 VMDK 판, 볼륨 원본 인자 그대로) → 쓴 파일을 다시 읽어 SHA-256 → OVF 서술자 `{vm}.ovf` → 매니페스트 `{vm}.mf`(`SHA256(file)= hex`) → `Ready`(파일·크기·해시). 실패하면 디렉터리를 지우고 `Failed`.
- `GET /vm-exports[/{key}]`, `DELETE /vm-exports/{key}`(Task `vm-export-delete` — 디렉터리 지움).

### 12.2 OVF 서술자 (`vm_imports/ovf_write.rs`, 순수)

OVF 1.0 봉투(ovftool·vSphere·VirtualBox가 모두 받는 판, `vmw` 확장): `References`(디스크 파일·크기), `DiskSection`(capacity 바이트, `ovf:format` streamOptimized, `populatedSize`), `NetworkSection`(우리 네트워크 이름), `VirtualSystem`: `OperatingSystemSection`(`vmw:osType` = `guest_os` 또는 만든 가져오기의 원본 `guest_os`, 없으면 `otherGuest64` + 경고), `VirtualHardwareSection`(`vmx-{hw_version}`, CPU 3(`vmw:CoresPerSocket`), 메모리 4, 컨트롤러 — 디스크 버스 virtio-blk → SCSI `VirtualSCSI`(pvscsi, Linux 커널에 있음), sata → SATA AHCI, 디스크 17, NIC 10 — virtio → `VmxNet3`, e1000e → `E1000e`, `keep_macs`면 MAC과 `vmw:Config addressType=manual`), `vmw:Config`(`firmware=efi`, Secure Boot면 `bootOptions.efiSecureBootEnabled=true`)). 넣지 않는 것은 경고: vTPM, CD-ROM, cloud-init 씨앗, NUMA·전용 CPU·hugepages, HA·배치·복제, 보안 그룹·SDN 주소. **같은 저장소의 OVF 읽개(계획 29 `ovf.rs`)가 쓴 것을 다시 읽어 같은 `SourceVm`이 나오는지**가 단위 시험이다.

### 12.3 내려받기

- `POST /vm-exports/{key}/download-link`(`vm.export`) → 10분 사는 토큰 URL `GET /vm-exports/{key}/download/{token}`(파일 복원 링크와 같은 꼴, 감사 `vm-export download web (OVA, 3.2 GiB)`). 응답은 `application/x-tar`, `Content-Disposition: attachment; filename="{vm}.ova"`, ustar를 그 자리에서 흘림: `.ovf` → `.mf` → 디스크 차례(DSP0243), 크기를 알므로 정확한 `Content-Length`. 받는 노드가 그 풀을 못 보면(로컬 dir 풀) 그 노드로 중계(`proxy.rs`).
- `?format=ovf&file=NAME`은 파일 하나(OVF 폴더로 받기 — NFS를 ESXi가 마운트하면 그대로 "OVF 템플릿 배포"를 쓸 수도 있다).
- CLI `vm export NAME --pool P [--guest-os ID] [--hw-version N] [--keep-macs] [--download FILE.ova] [--wait]`, `export list|get|download NAME [-o FILE]|delete`.

## 13. 권한·감사·버전 섞임·API·CLI·웹

| 것 | 권한 | 없을 때 |
|---|---|---|
| 원본 만들기·바꾸기·지우기 | 루트 `cluster.admin` | 403 |
| 원본 보기 | 루트 `cluster.read`(비밀 뺀 보기) | 목록에 없음 |
| 인벤토리 찾아보기·계획 만들기·미리 확인 | 루트 `vm.create` | 403 |
| 계획 시작·멈춤·취소 | 루트 `vm.create` + 각 항목의 대상 폴더 `vm.create`(시작 때 항목마다 — 없으면 그 항목 `Failed` "no permission") | 403 |
| 전환·되돌리기·마무리 | 대상 폴더 `vm.create` + `vm.power` | 403 |
| 내보내기·내려받기 | 그 VM의 `vm.export`(+ 풀 `storage.allocate`) | 403 |

- **감사**: `vmware-source create vc1 (vcenter.example.com, user migr@vsphere.local, fingerprint AB:…)`, `vmware-migration start wave1 (warm, 24 VMs)`, `vmware-vm cutover wave1-web01 (downtime 4m12s)`, `revert`, `cancel`, `vm export web → pool nfs1`, `vm-export download web`. 비밀번호·티켓은 적지 않는다.
- **할당량**: 시작 때 VM마다 볼륨 크기 합(`storage_bytes`)을 대상 폴더에서 미리 본다(미리 확인 1단계), VM 수·vCPU·메모리는 `CreatingVm`에서 지금 만들기 검사가 본다(넘으면 자동 되돌림 + 이유).
- **버전 섞임**: 새 경로는 `vmware-migrate`·`vm-export`가 없는 노드가 있으면 409 `cluster_upgrading`. 새 종류는 기능이 생긴 뒤에만 만들어지므로 옛 노드가 모르는 리소스를 볼 일이 없다(계획 68 `backup-store`와 같다). `VmImport`·`ImportRun`은 바이트 그대로.
- **API**(OpenAPI 태그 `vmware`·`vm-exports`): `/vmware-sources`(GET·POST, `{key}` GET·PATCH·DELETE, `{key}/actions/check`, `{key}/inventory`), `/vmware-migrations`(GET·POST, `{key}` GET·PATCH·DELETE, `{key}/preflight`, `{key}/actions/{start|pause|resume|cancel}`), `/vmware-vms`(GET`?migration=`, `{key}` GET, `{key}/{cutover|revert|finalize|cancel|retry}`), `/vms/{key}/export`, `/vm-exports`(GET, `{key}` GET·DELETE, `{key}/download-link`, `{key}/download/{token}`). 경로표(`authz_routes.rs`)에 모두, 경로표 시험이 빠진 경로를 잡는다.
- **CLI**(`orbivisorctl`): `vmware source add NAME --host --user [--password-file F | OPL_VMWARE_PASSWORD | 프롬프트] [--fingerprint] [--vddk]`·`list`·`get`·`set`·`check`·`delete`·`browse NAME [--path DC/vm/…] [--json]`(표 `PATH NAME POWER OS CPU MEMORY DISKS SNAPSHOTS CBT`), `vmware migration create NAME --source S --warm|--cold --vm PATH… [--net FROM=TO]… [--datastore FROM=POOL]… [--pool P] [--folders mirror|flat] [--folder F] [--node N] [--parallel 4] [--per-host 2] [--sync-every 60m] [--cutover-at TIME] [--shutdown guest|power-off] [--driver-iso ISO]`, `preflight NAME`(표 `VM LEVEL PROBLEMS` — blocked면 끝 코드 1), `start|pause|resume|cancel|delete NAME`, `list`, `get NAME`(VM 표 `VM STATE ROUND COPIED LAST-DELTA ESTIMATE NEXT-SYNC`), `vmware vm cutover|revert|finalize|cancel|retry NAME [--wait]`, `vm export …`, `export …`(§12.3).
- **웹**(가져오기 탭 안 보기 — 계획 61 규칙): `/imports`(지금 그대로) | `/vmware-migrations`(목록 → 상세 `/vmware-migrations/:name`: VM 표 — 상태 칩·회차·복사 막대·마지막 델타·예상 내릴 시간·다음 동기화, 고른 VM 일괄 "전환"·"취소", 한 줄 서랍 — 디스크별 진행·history·경고·오류·되돌리기·마무리, 전환 확인 대화상자는 예상 내릴 시간과 원본 처리(종료 방식·NIC 끊기)를 보임, revert는 VM 이름 입력) | `/vmware-sources`(연결 목록·더하기 대화상자 — 지문 확인 흐름은 `EsxiImportDialog`와 같은 꼴 · 다시 확인) | `/vm-exports`(목록·내려받기·지우기). 새 계획 마법사(`/vmware-migrations/-new`): ① 원본 ② 인벤토리 나무 + 표에서 VM 고르기(찾기·거르기, 이미 계획에 든 VM 흐림) ③ 매핑(원본 네트워크·데이터스토어마다 고르기 — 고른 VM이 쓰는 것만, 폴더 규칙, 노드, 장치, 드라이버 ISO) ④ 한도·동기화 주기·전환 정책 ⑤ 미리 확인 표(VM마다 칩, 문제 펼침, blocked는 빼고 시작 가능) → 만들고 시작. VM 상세 ⋯ 메뉴에 "OVA로 내보내기…"(꺼진 VM, `vm.export`) → 풀·OS·MAC 대화상자 → Task → 내려받기 단추. 화면은 늦게 읽는 조각 `web/src/vmware/`·`web/src/exports/`, 문구는 늦게 받는 묶음(계획 70 규약), 첫 화면 묶음 증가는 보기 이름·팔레트 별칭뿐(+1 KB 밑).

## 14. 시험과 CI

- **단위(opl-types·opl-api, 이 Mac)**: 새 타입 직렬화·가림(`Debug`·보기 JSON에 비밀번호 문자열 없음), 지금 `VmImport`·`ImportRun` 바이트 그대로; SOAP 요청 틀(견본과 바이트 비교)·응답 해석(견본 XML: 인벤토리 쪽 나눔, 스냅샷 나무, `DiskChangeInfo`, 결함 종류), 인벤토리 → `SourceVm`(디스크 순서·DVS 이름), 매핑 차례(덮어쓰기 > 규칙 > 제안)·디스크별 풀, 미리 확인 VMware 쪽 표(줄마다), 회차 계획 순수 함수(구역 묶기·0 조각·체크포인트에서 이어 가기), 스케줄러 순수 함수(한도 셋·델타 먼저·예약 전환), 전환 상태 기계(걸음마다 실패 → 되돌림 걸음 목록), OVF 쓰개 ↔ 읽개 왕복, ustar 흘림 크기 = 실제 바이트.
- **가짜 vSphere 시험(opl-api `--features fake-vsphere`)**: 로그인·세션 만료 다시 로그인·지문 틀리면 비밀번호를 보내지 않음, 인벤토리, 호스트 티켓 길과 vCenter `/folder`로 내려가기, **따뜻한 이전 정확성**: 4 MiB × 디스크 둘 VM에 게스트 쓰기를 섞으며(전체 복사 중·회차 사이·회차 중) 3회차 → 전환 → 대상 볼륨 바이트 = 원본 최종 바이트, 바뀐 구역만 읽었는지(요청 기록), CBT 초기화 → 그 디스크만 전체, 스냅샷 지우기 실패 → 다음 시도가 둘을 정리, 범위 읽기 중 끊기 → 이어 감, opld 다시 시작 흉내(Task 중간 버리기) → 체크포인트부터, 전환 걸음마다 실패 주입 → 원본 다시 켜짐·NIC 되돌림·대상 VM 없음, `ShutdownGuest` 무시 → 시간 넘김 → 원본 켜진 채, revert, cancel, finalize(CBT 끄기), 사용자 스냅샷 생김 → 회차 건너뜀, **모든 시험 끝에 원본 디스크·사용자 스냅샷 그대로**. 권한 행렬(`insta`), 미리 확인이 숨은 풀·네트워크를 새지 않음(계획 71 리뷰 2 시험 꼴), 409 `cluster_upgrading`, 감사에 비밀 없음.
- **NBD 쓰기(opl-store, `storage-it`)**: `qemu-nbd` 쓰기 대상에 `write`·`write_zeroes`·`flush` → `qemu-img compare`, 씬 유지(`qemu-img map`), qcow2·raw.
- **실제 시험(opld `qemu-it`, 이 Mac HVF·svc01)**: opld 둘 + 가짜 vSphere(같은 프로세스): `vmware source add` → `browse` → `migration create --warm` → `preflight` → `start` → 쓰기 섞기 → 회차 둘 → `cutover --wait` → VM 켜짐(디스크 없는 부팅이라 펌웨어까지만 — 게스트 OS 없음) → 볼륨 `qemu-img convert -O raw` 바이트 = 원본, 원본 꺼짐·NIC `startConnected=false`; `revert` → 원본 켜짐; 차가운 이전 한 대; `vm export` → OVA 내려받기 → **계획 29 OVA 가져오기로 다시 가져와** 디스크 바이트 같음(왕복).
- **GitHub `vmware-sim`(새 작업, ubuntu-24.04)**: `vmware/vcsim`(핀한 판 — 스테이지 2가 고르고 §18에) 컨테이너와 `govc`(같은 govmomi 판): `cargo test -p opl-api --features vcsim-it --test vcsim` — 우리 클라이언트로 로그인·인벤토리(vcsim 기본 인벤토리의 모든 VM이 `SourceVm`으로 풀림)·`ReconfigVM` CBT·스냅샷 만들기·지우기·전원·티켓·`/folder` 파일 읽기·`QueryChangedDiskAreas`(vcsim이 답하면 꼴 확인, 없으면 결함 모양을 기록 — §18), 가짜 vSphere와 vcsim이 같은 요청에 같은 꼴(요소 이름·`xsi:type`)을 내는지 견줌; 그리고 우리 `vm export`가 만든 OVA를 `govc import.ova`로 vcsim에 넣어 govmomi의 OVF 해석이 받는지. `nbdkit`(우분투 꾸러미)로 `VddkReader` 배선(`file` 플러그인을 vddk 자리에) 시험도 이 작업에서.
- **웹(vitest + msw)**: 마법사(원본 → 나무 고르기·찾기 → 매핑이 고른 VM의 네트워크·데이터스토어만 → 미리 확인 칩·blocked 빼고 시작), 계획 상세(상태 칩·진행·전환 확인 대화상자의 예상 시간·revert 이름 입력·watch로 바뀜), 원본 대화상자(지문 흐름·비밀번호가 기록·저장소에 없음), 내보내기 대화상자·내려받기 링크, 권한 없으면 단추 없음, 409 업그레이드 안내, axe·대비 두 테마, 390px 가로 스크롤 없음, `routes.test`·`nav.test`(가져오기 탭 보기 넷)·`i18n.test`.
- **브라우저 E2E(Playwright, opld `web-it`)**: `web/e2e/vmware.spec.ts` — 가짜 vSphere 실행 파일을 띄워 원본 더하기(지문 확인) → 마법사로 VM 하나 따뜻한 이전 → 진행이 `Ready` → 전환 → VM 링크 → 지움; VM 내보내기 → 내려받기(받은 파일 크기·첫 512바이트가 ustar `.ovf` 머리).
- **할 수 없는 것과 대신**: 실제 vCenter·ESXi(실험실에 없음 — 가이드의 손 확인 절차: 작은 Linux VM 하나로 원본 더하기·따뜻한 이전·전환·되돌리기, `/folder` Range가 켜진 VM의 기반 flat을 읽는지, 티켓 길, 무료 ESXi 거절 문구 — 결과는 사용자가 해 보면 §18·가이드에 적는다), 실제 VDDK(독점 — 배선만), 옮긴 Windows·RHEL 게스트가 실제로 켜지는지(실험실은 aarch64 TCG이고 VMware 게스트가 없다 — 바이트 같음까지만), 실제 ovftool·vSphere로 OVA 받기(vcsim `govc import.ova`가 대신, 손 확인 절차), 속도(가짜 vSphere 루프백 수치만 §18에 — 실제 망 속도는 손 확인).
- **svc01**: 일반 시험 묶음 전체(서버·웹·opld `qemu-it` 실제 시험 포함). **GitHub**: `check`·`web`·`vmware-sim` 통과, `linux-kvm`은 opld 시험 전부를 도므로 새 실제 시험이 거기서도 돈다(탐침으로 확인), `ldap`·`package`는 바뀐 것이 없다.

## 15. 견준 길

- **vSphere API: SOAP 손 XML(채택) / vSphere Automation REST / WSDL 코드 생성 / govmomi를 옆 프로세스로.** REST는 CBT·티켓·ESXi 단독이 없다. WSDL 생성은 수만 줄 타입과 빌드 시간이 들고 쓰는 것은 스무 메서드다. govmomi(Go) 옆 프로세스는 배포물·언어가 하나 는다. 손 XML의 대가 — 꼴 실수 위험 — 는 vcsim 시험(govmomi가 실제 WSDL로 만든 상대)이 잡는다.
- **디스크 읽기: 스냅샷을 지워 기반을 최신으로 두고 `/folder` 범위 읽기(채택, 기본) / VDDK만 / NFC 내보내기(`ExportVm`) / 델타 체인(seSparse) 해석.** VDDK는 독점·재배포 금지라 묶을 수 없다 — 운영자가 깔면 쓰는 선택 길로 둔다(vSAN·vVols). NFC는 디스크 전체를 순서대로만 주어 델타에 쓸 수 없다. seSparse 해석은 문서가 없다. "S(n+1) 만들고 S(n) 지우기"는 백업 제품이 늘 하는 동작 둘로 기반을 늘 일관된 최신으로 둔다. 대가: 회차마다 합치기 I/O(델타 크기만큼)를 원본 데이터스토어에 주고, 사용자 스냅샷이 있는 VM은 받지 않으며, 켜진 VM의 기반 flat 읽기가 실제로 되는지는 손 확인이 남는다(안 되면 VDDK 길).
- **쓰기: 대상 볼륨을 `qemu-nbd`로 열고 그 자리 쓰기(채택) / 업로드 디렉터리에 raw로 받고 `qemu-img convert`(계획 30) / 풀마다 직접 쓰기.** 델타는 같은 볼륨에 덮어써야 하고, 디스크 크기만큼 임시 공간도 필요 없어진다. `qemu-nbd`는 모든 풀 형식(qcow2·raw·RBD·LVM·ZFS)을 이미 계획 68에서 연다.
- **진행 자리: VM마다 리소스 `vmware-vm`(채택) / 계획 리소스 하나 안 목록 / Task 진행만.** 목록 하나는 동시 Task가 revision을 다툰다. Task만으로는 회차를 넘는 상태(changeId·스냅샷 moref·바꾼 원본 설정)가 없어진다.
- **회차 일정: 리더 스케줄러가 회차마다 Task(채택) / VM마다 며칠 도는 긴 Task 하나.** 긴 Task는 동시 수 자리를 전환까지 붙잡아 일괄 처리량을 막고, 다시 시작·리더 바뀜에 약하다. 회차 Task는 짧고 다시 해도 같다(§5).
- **자격 증명: Raft에 두고 가림(채택) / 받은 노드 메모리(계획 30) / 외부 비밀 저장소.** 따뜻한 이전은 며칠 가고 회차 Task가 어느 노드에서든 돈다 — 메모리만으로는 다시 시작할 때마다 끊긴다. 원격 토큰·S3 비밀과 같은 자리·같은 가림이다.
- **게스트 맞추기: 안전한 장치 기본값 + 원본에서 고치면 델타가 옮김 + 드라이버 ISO(채택) / virt-v2v·libguestfs로 디스크 고치기 / VMware Guest Operations로 게스트 안 명령 / pvscsi·vmxnet3 흉내.** virt-v2v는 큰 의존성(libguestfs 어플라이언스)이고 aarch64 노드에서 x86 게스트를 다루기 어렵다. 게스트 안 명령은 게스트 자격 증명을 받아야 하고 고객 VM 안에서 우리가 코드를 돌린다. pvscsi·vmxnet3는 VmSpec 장치 종류가 늘고 RHEL QEMU에 없다(§16 후보). 따뜻한 이전은 "원본에서 고치고 다음 델타"라는 길을 공짜로 준다.
- **내보내기 형식: 풀에 OVF 폴더 + 흘리는 OVA(채택) / OVA 파일을 풀에 / 볼륨으로.** OVA 파일을 쓰면 디스크를 두 번 써야(VMDK → tar) 하고 공간이 두 배다. 폴더는 NFS로 ESXi가 바로 쓸 수 있고, 흘리는 tar는 크기를 미리 알아 공간 없이 OVA를 준다.
- **VMDK 만들기: `qemu-img convert` streamOptimized(채택) / 직접 짠 deflate 쓰개.** 이미 쓰는 도구이고 vSphere가 받는다고 알려져 있다. 직접 쓰개는 흘리며 해시를 잴 수 있지만 꼴 실수 위험이 크다. 대가: 한 번 더 읽어 해시를 잰다.
- **화면 자리: 가져오기 탭 안 보기(채택) / 새 사이드바 항목 "이전".** 계획 61 규칙. 이전·내보내기는 가져오기 탭이 맡던 일의 확장이다.

## 16. 나중 할 일

- 시험 부팅: 동기화 중 대상 볼륨의 스냅샷으로 격리된(NIC 끊은) VM을 켜 보고 버림 — 전환 전 부팅 확인.
- 켜진 VM 내보내기(볼륨 스냅샷에서), 백업에서 내보내기, OVF 서명(`.cert`), 다른 제품 원본(Hyper-V VHDX·Nutanix).
- 앱 그룹 전환(여러 VM 차례·의존), 전환 전후 훅(스크립트·웹훅), 원본 NIC 끊기 대신 포트 그룹 바꾸기.
- 게스트 안 맞추기(VMware Guest Operations로 dracut·virtio-win 설치 — 게스트 자격 증명을 받는 선택 기능), pvscsi·vmxnet3 흉내 장치(QEMU 능력 탐침 + VmSpec 장치 종류).
- vSphere 태그·사용자 정의 속성 → 레이블, 리소스 풀·DRS 규칙 → 배치 규칙, VMware 권한 → 폴더 grant.
- 실제 vCenter로 한 손 확인 결과를 가이드·CI에 반영(가능하면 vCenter 평가판 실험실).

## 17. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다(`cargo test`·`clippy`·`fmt`, 웹 스테이지는 `pnpm typecheck`·`lint`·`test`). 실제 VMware에 닿는 부분은 모두 가짜 vSphere 시험 뒤에 있고, vcsim 시험은 스테이지 2가 더하는 GitHub 작업에서 돈다. 화면은 스테이지 7에서 열린다.

1. **feat(types): VMware 이전 2 타입 — 원본 연결·이전 계획·VM 항목·VM 내보내기 리소스와 Task, 기능 vmware-migrate·vm-export, 권한 vm.export** — `crates/opl-types/src/{vmware,vm_export}.rs`(+ 직렬화·가림·바이트 호환 시험), `resource.rs` `Kind` 넷(`ALL` 43, `WATCHABLE`), `task.rs` Task 여섯, `node.rs` `FEATURES` 둘, `authz.rs` `VmExport`(역할 표 — operator에 없음), `opl-cluster` 종류 등록, 새 종류 만들기는 기능 게이트 전까지 API에서 막음.
2. **feat(api,ci): vSphere SOAP 클라이언트와 가짜 vSphere — 세션·인벤토리·스냅샷·CBT 바뀐 구역·전원·호스트 티켓 범위 읽기, nbdkit-vddk 읽기(선택), GitHub vmware-sim 작업** — `crates/opl-api/src/vmware/{mod,http,soap,client,inventory,cbt,reader,fake}.rs`(`esxi.rs` 연결을 `http.rs`로 꺼냄 — 계획 30 시험 그대로), `tests/fixtures/vsphere/*.xml`, 기능 `fake-vsphere`·`vcsim-it`, 실행 파일 `fake-vsphere`, `tests/vsphere.rs`(가짜 vSphere), `tests/vcsim.rs`, `.github/workflows/ci.yml` `vmware-sim`(vcsim·govc·nbdkit), 노드 설정 `[vmware]`.
3. **feat(api,cli): VMware 원본 연결·인벤토리 찾아보기·이전 계획과 매핑 규칙·VM마다 미리 확인 API, vmware source·migration CLI** — `vmware_sources.rs`(가림·지문 흐름·세션 캐시·인벤토리 60초 캐시), `vmware_migrations.rs`(만들기·바꾸기·지우기·시작/멈춤/취소의 상태만 — Task는 스테이지 4), `vm_imports.rs` `import_spec_with`(디스크별 풀), `vm_checks.rs`를 부르는 미리 확인 + VMware 쪽 표, 경로표·감사·`WATCHABLE` 가림, OpenAPI·`web/src/api/schema.d.ts`, `oplctl vmware source …`·`vmware migration create|preflight|list|get|start|pause|resume|cancel|delete`, 시험(가짜 vSphere·권한 행렬·숨은 것 새지 않음·409 `cluster_upgrading`).
4. **feat(api,store,driver): VMware 전체 복사와 CBT 델타 동기화 — NBD 쓰기·qemu-nbd 쓰기 대상, 이어 가는 vmware-sync Task, 이전 스케줄러(동시 수·호스트당·노드당·대역), 차가운 이전** — `opl-store` `NbdClient::{write,write_zeroes,flush}`(+ 시험), `opl-driver` `nbd_target`, `tasks/vmware.rs`(회차 알고리즘·체크포인트·멈춤 한도·CBT 초기화·디스크 늘림), `tasks.rs` 이어 가는 목록·시간 제한 없음 목록·sweep이 항목 볼륨을 남김, `vmware_scheduler.rs`(리더 루프), `oplctl vmware vm retry|cancel`, 가짜 vSphere 정확성 시험(쓰기 섞은 회차·끊김·다시 시작·사용자 스냅샷).
5. **feat(api,cli): VMware 전환·되돌리기·마무리 — 원본 끄기·마지막 델타·VM 만들기·켜기, 걸음마다 실패 때 자동 되돌림, 원본 보호·예약 전환·내린 시간 기록** — `tasks/vmware.rs` 전환 상태 기계·`vmware-revert`·`vmware-cleanup`, `vmware_migrations.rs`의 `/vmware-vms/{key}/{cutover|revert|finalize|cancel|retry}`, 스케줄러의 예약 전환, IP 예약(`reserve_ips`)·드라이버 ISO, 감사, `oplctl vmware vm cutover|revert|finalize`, 시험(걸음마다 실패 주입 → 원본 다시 켜짐·NIC 되돌림, 시간 넘김, revert 확인, 원본 안전 단언).
6. **feat(api,driver,cli): VM을 OVA/OVF로 내보내기 — streamOptimized VMDK·OVF 서술자·SHA-256 매니페스트를 풀에, 흘려 보내는 OVA 내려받기 링크, vm export·export CLI** — `opl-driver` `export_vmdk`, `vm_imports/ovf_write.rs`(+ 읽개 왕복 시험), `vm_exports.rs`(요청·링크·흘리는 ustar·중계), `tasks/export.rs`(`vm-export`·`vm-export-delete`), 경로표·감사·OpenAPI·`schema.d.ts`, `oplctl vm export`·`export list|get|download|delete`, `vmware-sim` 작업에 `govc import.ova` 단계, 시험(`storage-it` 내보내기 → 계획 29 OVA 가져오기 왕복).
7. **feat(web): 가져오기 탭의 VMware 이전·원본·내보내기 보기 — 원본 연결과 지문, 인벤토리 나무에서 고르기·매핑·미리 확인 마법사, VM별 진행·전환·되돌리기, VM의 OVA 내보내기** — `nav.ts` 가져오기 탭 보기 넷, `App.tsx` 늦은 경로, `web/src/vmware/{SourcesPage,SourceDialog,MigrationsPage,MigrationWizard,MigrationPage,VmDrawer,CutoverDialog,RevertDialog,inventory.ts,i18n}`, `web/src/exports/{ExportsPage,ExportDialog,i18n}`, VM 상세 ⋯ 메뉴, 팔레트 별칭, vitest(§14 웹)·axe·대비·휴대폰 폭, 번들 기록.
8. **test(opld,web),docs: VMware 이전 실제 시험(가짜 vSphere·QEMU)과 브라우저 E2E, 가이드·아키텍처·설계 스테이지 보고·계획 72 실행 기록** — `crates/opld/tests/vmware_migration.rs`(`qemu-it` — 따뜻한·차가운·revert·내보내기 왕복), `web/e2e/vmware.spec.ts`, `crates/opld/tests/web.rs`(가짜 vSphere 띄움), `docs/guide/backup.md`의 "VMware에서 가져오기" 절(일괄·따뜻한 이전·전환 점검표·게스트 맞추기·실제 vCenter 손 확인 절차·한계), `docs/guide/vms.md`(내보내기), `docs/guide/access.md`(`vm.export`), `docs/guide/web-ui.md`(가져오기 탭 보기), `docs/guide/development.md`(시험 명령·`vmware-sim`), `docs/architecture.md`(리소스 43종·리더 루프·이어 가는 Task·보안 표), `docs/index.md`, README 한 줄, 계획 문서.

**함께 할 수 있는 것**: 스테이지 1 뒤에 스테이지 2(클라이언트·가짜)와 스테이지 6(내보내기 — vSphere와 무관)이 서로 닿지 않는다. 스테이지 4의 NBD 쓰기·`nbd_target` 부분은 스테이지 3과 함께 해도 된다. 스테이지 3 → 4 → 5는 차례대로. 스테이지 7은 3·5·6의 `schema.d.ts` 뒤(msw로 먼저 시작할 수는 있다), 스테이지 8은 마지막.

## 18. 스테이지 보고 (설계와 다른 것·결정)

(구현하면서 스테이지마다 채운다.)

### 스테이지 1

- **타입 이름**: OpenAPI 스키마 이름이 계획 67(`MigrationMode`·`MigrationPhase`·`MigrationStatus`)과 겹치지 않게 새 타입은 모두
  `Vmware…`(`VmwareMigrationSpec`·`VmwareRules`·`VmwareLimits`·`VmwareSyncPolicy`·`VmwareCutoverPolicy`·`VmwareSyncDisk`·
  `VmwareSnapshotRef`·`VmwareSourceChanges` 등)와 `VmExport…`로 지었다. 비밀번호는 `vmware::Secret`(JSON은 문자열 그대로,
  `Debug`는 `<redacted>`)이고 보기·watch는 `redact_source_json`, 감사는 `VmwareSourceSpec::audit_summary`를 쓴다.
- **Task 칸**: 이전 Task에 `vm` 칸을 두지 않았다 — 대상 `spec.target`이 `vmware-vm` 항목 id다(계획 67 `NetworkMigrate`와 같은
  꼴, 줄도 대상). `vmware-sync {full}`·`vmware-cleanup {delete_target}`은 거짓이면 쓰지 않는다. `vm-export {export}`의 대상은
  VM(VM 줄), `vm-export-delete`의 대상은 내보내기다. 실행은 "not available yet" 실패(스테이지 4·5·6이 바꾼다). 이어 가는
  목록·시간 제한 없는 목록·줄은 설계대로 스테이지 4·6이 더한다.
- **항목 상태**: `CuttingOver { step }` 대신 `state: cutting-over` + 따로 `cutover_step` 칸(상태가 늘 문자열 하나라 거르기·
  `counts`·화면이 쉽다). 기본 상태 `draft`(시작 전 계획의 항목)를 더했다. `pending_snapshot`(§9.2)과 설명 되돌리기에 쓸
  `source_changes.old_annotation`을 더했다. `overrides`는 `VmwareOverrides`(`ImportRun` 부분 값 + 디스크 순번별 `pools`).
- **규칙 모양**: `node: Auto | Node(id)` → `node?: Uuid`(없으면 자동), `devices: Auto | Fixed` → `devices?: {disk_bus, nic_model}`,
  `folders`는 `{kind: flat, into?}` | `{kind: mirror, under?}`. `sync.stop_after_days`는 숫자(기본 14, 0은 멈추지 않음),
  `cutover.at`은 유닉스 초(API·CLI가 RFC 3339를 받아 바꾼다 — 스테이지 3). 한도·주기·전환 정책은 기본값도 늘 적는다(빈 값만
  뺀다). 검증 범위: 동시 수·호스트당·노드당 1~64, 디스크당 흐름 1~16, 주기 5~1440분, 종료 기다림 30~3600초.
- **겹침**: Raft 독점 키 `vmware-vm`(`instance_uuid`)를 더했다 — 한 원본 VM은 끝나지 않은 항목 하나에만(`finalized`·`reverted`·
  `cancelled`는 놓아 줌). §8.3의 409 `vm_in_migration`을 상태 머신이 원자적으로 지킨다(스테이지 3은 `InUse{field:"vmware-vm"}`를
  그 코드로 바꾸면 된다). vCenter 둘에 같은 instanceUuid는 없다고 본다.
- **권한 갈래(`kind_privilege`)**: 원본 읽기 `cluster.read`·쓰기 `cluster.admin`, 계획·항목·내보내기 읽기 `system.read`, 내보내기
  쓰기·동작 `vm.export`. 계획·항목의 종류 단위 쓰기는 `cluster.admin`으로 두었다 — 내장 역할 동치 시험(옛 `RoleAuthorizer`는
  operator 쓰기를 VM·볼륨·백업·작업·가져오기에만 준다) 때문이고, 설계 §13의 실제 확인(루트·대상 폴더 `vm.create`·`vm.power`)은
  스테이지 3 경로표가 권한을 이름으로 본다. 폴더 트리(`FolderKind`)에는 아직 넣지 않았다(계획 `folder`의 권한 모델과 함께
  스테이지 3).
- **`vm.export` 쓰기 막기**: 역할·토큰 범위에 넣는 쓰기는 기능 `vm-export`가 없는 노드가 있으면 409만 한다. 계획 69처럼 API
  전까지 400으로 막지 않았다(이름뿐인 권한이라 줄 곳이 없어도 해가 없고 스테이지 6이 풀 일이 없다).
- **watch**: `vmware-source`만 더했다(루트, 이벤트에서 `password`를 뺌). 계획·항목·내보내기는 폴더·VM 권한 거르기가 들어
  스테이지 3·6이 더한다. 새 종류를 만드는 API는 아직 없으므로 막을 경로가 없다.

### 스테이지 2

- **vcsim 판과 받는 길**: govmomi `v0.56.2`(2026-10-26 릴리스)의 `vcsim`·`govc` 실행 파일(Linux x86_64)을 판과 SHA-256을
  박아 받는다 — §14의 "컨테이너" 대신(컨테이너 내려받기 한도·이미지 태그 바뀜을 피하고, 같은 실행 파일을 개발 Mac에서도
  띄워 꼴을 맞춰 볼 수 있다). GitHub `vmware-sim` 작업이 `vcsim -l 127.0.0.1:8989 -username user -password pass`를 띄우고
  `cargo test -p opl-api --features vcsim-it --test vcsim`을 돈다. 스테이지 6은 같은 작업에 `govc import.ova` 단계를 더한다
  (`govc`가 PATH에, `GOVC_URL`·`GOVC_INSECURE`가 작업 환경에 있다).
- **vcsim이 하는 것·안 하는 것(v0.56.2로 확인)**: 로그인(`InvalidLogin` 결함)·세션 쿠키·`NotAuthenticated`(faultstring이 빈
  값 — 형식 이름을 메시지로 쓴다), `CreateContainerView`+`RetrievePropertiesEx`(`maxObjects` 쪽 나눔·토큰·
  `ContinueRetrievePropertiesEx`, 하위 형식 맞춤 — `ComputeResource`가 클러스터를, `Network`가 분산 포트 그룹을 덮는다),
  `ReconfigVM_Task`(`changeTrackingEnabled`·`annotation`·NIC `connectable.startConnected` 편집 반영), 스냅샷 만들기·지우기,
  `PowerOn/OffVM_Task`(두 번 끄기는 Task 결함 `InvalidPowerState`), `ShutdownGuest`(받음), `AcquireGenericServiceTicket`
  (`hostName`은 vcsim 자신 `127.0.0.1:8989`, 티켓 검사 없음), `/folder` GET `Range`(206·`Content-Range`, 인증 없이도 200)와
  PUT. **하지 않는 것**: `QueryChangedDiskAreas`는 `MethodNotFound` 결함(우리는 `VsphereError::NotSupported`), 스냅샷 디스크
  backing에 `changeId`가 없고 델타 파일·`parent` 체인도 없다(스냅샷 뒤에도 `fileName`이 기반), 기반 flat 파일은 0바이트(시험이
  PUT으로 채운다), 분산 포트 그룹의 `config.uplink`를 주지 않는다(업링크 포트 그룹이 네트워크 목록에 남는다 — 실제 vCenter는
  `uplink = true`로 뺀다), 스냅샷 `state`를 늘 `poweredOff`로 적는다. 설정 안 된 속성(`changeTrackingEnabled`·`snapshot`·
  `annotation`)은 `propSet`에서 아예 빠진다 — 우리 읽개는 빠진 값을 기본값으로 본다. 호스트 `config.certificate`는
  `ArrayOfByte`(`<byte>` 요소, 부호 있는 10진)인 PEM이고 vcsim 서버 인증서와 같다. **`xsi:type` 접두어**가 govmomi는
  `_XMLSchema-instance:type`이다 — 읽개는 접두어가 아니라 네임스페이스 URI로 찾는다(실제 vCenter는 `xsi:`).
- **가짜와 vcsim이 같은 꼴인지**: `tests/vcsim.rs`가 같은 VM 속성 요청에 대해 값의 `xsi:type`이 같은지, 디스크·NIC 장치의
  요소 나무(이름·`xsi:type`)에서 가짜 것이 vcsim 것의 부분 집합인지 본다(가짜만 주는 것은 CBT `changeId`·`parent` 체인뿐).
  견본 XML(`tests/fixtures/vsphere/`)은 vcsim 응답을 그대로 잡은 것(`retrieve-service-content`·`inventory-page1/2`)과 실제
  vCenter·ESXi 꼴을 손으로 줄인 것(`vcenter-vm` — 스냅샷 둘·디스크 셋·hddOrder·NSX NIC, `esxi-service-content`,
  `query-changed-disk-areas`)이다.
- **오류 갈래 더함**: `VsphereError`에 `NotSupported`(`MethodNotFound`·`NotSupported`), `Untrusted{sha256, sha1}`(VDDK
  썸프린트도 함께 — 409 `untrusted_certificate`의 `details.thumbprint_sha1`), `Invalid`(응답 꼴 틀림), `Timeout`(vSphere Task
  한도)을 더했다. API 코드: 무료 ESXi 400 `vmware_restricted`, 그 밖의 결함 400 `vmware_error`, `Invalid`·`Timeout`·HTTP
  오류 502 `vmware_unreachable`(§7.1의 나머지는 설계대로). 상태·파일 결함은 흔한 하위 형식 표로 나눈다(`ToolsUnavailable`·
  `TaskInProgress` → `InvalidState`, `GenericVmConfigFault`·`CannotAccessFile` → `FileFault`).
- **SOAPAction**: 첫 `RetrieveServiceContent`는 판 없는 `urn:vim25`로, 그다음은 서버가 알린 `about.apiVersion`
  (`urn:vim25/8.0.2.0`)으로 보낸다(서버가 모르는 판을 보내지 않게).
- **세션**: `Session`을 `Arc`로 여러 읽개가 나눠 쓴다. `Drop`이 `Logout`을 띄워 보낸다. §7.1의 "노드마다 원본별 세션
  캐시(5분)"는 API 쪽 소유라 스테이지 3으로 미뤘다. vSphere Task 기다리기 간격은 `Session::with_poll`(기본 1초 → 5초).
- **연결 재사용**: `HttpClient`가 다 읽은 연결을 쉬는 줄(4개, 30초)에 두고 다시 쓴다. 다시 쓴 연결이 보내기 전에 닫혔으면 새
  연결로 한 번 더 보낸다(보낸 요청은 다시 보내지 않는다 — SOAP 쓰기는 되풀이하면 안 된다). 계획 30 `esxi.rs`는 지문 고정 TLS만
  `vmware/http.rs`를 부르고 요청마다 새 연결 그대로다(시험 그대로 통과).
- **호스트로 바로 읽기**: `HostRoute{name, fingerprint}`의 이름은 `호스트[:포트]`(가짜는 호스트를 `127.0.0.1:포트`로 알린다).
  티켓 URL은 `https://{호스트 이름}/folder/…`, 범위마다 새 티켓. 호스트가 티켓을 거절(401·403)하거나, 연결이 세 번 실패하거나,
  호스트 인증서가 vCenter가 알려 준 것과 다르면 그 읽개는 vCenter `/folder`(세션 쿠키)로 내려가 경고를 남긴다. 세션 쿠키 읽기가
  401이면 한 번 다시 로그인한다. 끊긴 범위 읽기(짧은 본문·연결 끊김)는 세 번까지 다시 한다.
- **`dcPath`**: 인벤토리의 `datacenter` 칸은 데이터센터 **경로**(폴더 안이면 `Europe/DC2`)다 — `/folder`의 `dcPath`가 받는 값.
- **VDDK 읽개**: `VddkReader` 대신 `NbdkitReader` 하나(`open_vddk(VddkParams)`·아무 플러그인이나 `spawn`) — CI가 `file`
  플러그인으로 같은 배선을 본다. 소켓은 `{run_dir}/vddk-{무작위}/nbd.sock`(디렉터리 0700), 비밀번호는 `password=-`로 표준
  입력 첫 줄, Linux는 `--exit-with-parent`, 버리면 nbdkit을 끝내고 디렉터리를 지운다. 노드 설정 `[vmware] nbdkit`·
  `vddk_libdir`(절대 경로)는 `AppState.vmware`(`VmwareNodeConfig`)로 간다 — Task 실행기 설정으로 옮기는 것은 `tasks.rs`를
  고치는 스테이지 4.
- **가짜 vSphere**: `vmware/fake.rs`(기능 `fake-vsphere`, 실행 파일 `fake-vsphere` — 연결 정보 JSON 한 줄을 찍는다). vCenter
  모드는 호스트마다 인증서가 다른 TLS 서버를 띄운다. 디스크는 64 KiB 블록 층(기반 + 스냅샷마다 델타), 스냅샷 지우기는 그 위
  델타를 아래 층으로 합친다. CBT는 켠 뒤 다음 stun부터 돌고 스냅샷마다 `changeId = "52 …-…/{n}"`, `QueryChangedDiskAreas`는
  구간 `[changeId의 n, 스냅샷의 n)`의 블록(또는 `"*"`면 스냅샷 시점에 할당된 블록)을 창 단위로 준다. 처음 디스크는 블록 셋 중
  둘만 채운다(씬 디스크의 빈 곳). 꺼진 VM에 쓰면(스냅샷 없이 기반) flat 파일의 수정 시각이 바뀐다(`SearchDatastore_Task`).
- **시험 단계**: `tests/vsphere.rs`는 기능이 있어야 빌드되므로(`required-features`) CI `check`에 `cargo test -p opl-api
  --features fake-vsphere --test vsphere` 단계를 더했다(시스템 도구 없음). 따뜻한 이전 회차 시험(전체 복사 → 델타 둘 → 끄고
  마지막 델타, 사이마다 게스트 쓰기)이 클라이언트만으로 대상 바이트 = 원본 최종 바이트, 델타 회차가 바뀐 블록만 티켓으로
  호스트에서 읽음을 본다 — 스테이지 4 회차 알고리즘의 본보기다.
- **잰 것**: 가짜 vSphere VM 2 000대 인벤토리(객체 2 035개, 쪽 다섯) — 개발 Mac(디버그 빌드) 1.1초. 실제 vCenter 수치는 손 확인.

### 스테이지 3

- **권한 모델**: 계획(`vmware-migration`)·항목(`vmware-vm`)은 **루트 종류**로 두었다(폴더 트리 `FolderKind`에 넣지 않음). 읽기는 루트
  `system.read`, 만들기·바꾸기·지우기·미리 확인·시작/멈춤/이어 가기/취소는 경로표의 루트 `vm.create`(§13 표 그대로 —
  `kind_privilege`의 `cluster.admin` 쓰기는 핸들러가 부르지 않는다). 그래서 `VmwareMigrationSpec.folder`는 쓰지 않는다(API가 늘
  비워 만든다 — 폴더에 사는 계획은 나중 일). 규칙·덮어쓰기가 가리키는 폴더는 만들기·바꾸기 때 그 폴더의 `vm.create`(Mirror는
  `under`의 `inventory.folder`)를 보고, 미리 확인이 VM마다 대상 폴더의 `vm.create`를 다시 본다. watch는 둘을 루트 종류로 더했다
  (비밀 없음).
- **원본 연결 API**: 본문은 `{name, host, port?, user, password, fingerprint?, vddk?: {libdir?}, datacenter_hint?}`(전송은
  `vddk` 칸 — 썸프린트는 서버가 잰다). 지문이 없으면 다른 칸을 먼저 검증한 뒤 연결만 해 409 `untrusted_certificate`
  (`details.fingerprint`·`thumbprint_sha1`, 로그인 없음). `PATCH`는 주어진 칸만 바꾸고 **다시 로그인해 본 뒤에만** 저장한다(틀린
  비밀번호는 저장되지 않는다, `vddk: null`이면 `/folder`로). `actions/check`는 캐시를 버리고 다시 로그인해 `status`를 적고,
  실패하면 `status.error`를 적은 뒤 그 오류를 돌려준다. 보기는 `VmwareSourceSpecView`(비밀번호 칸 없음). 무료 ESXi의
  `status.restricted`는 만들 때 알 수 없어 거짓으로 둔다(스테이지 4가 `RestrictedVersion`을 받으면 적는다).
- **캐시**: `AppState.vmware_cache`(`VsphereCache`, `Arc`) — 원본 id별 세션(5분 쉬면 버림 → `Logout`)과 인벤토리(60초). 열쇠의
  스펙이 지금 스펙과 다르면 쓰지 않고, 바꾸기·지우기·다시 확인이 버린다. 읽기가 실패하면 그 세션을 버린다. 30초마다 훑는 작업은
  첫 세션을 둘 때 한 번 띄우고 캐시가 없어지면 끝난다. 시험이 시간을 줄인다(`VsphereCache::new`).
- **찾아보기 응답**: `{fetched_at, inventory, migrating}` — `Inventory` 타입들에 `ToSchema`를 붙였다. `migrating`(끝나지 않은
  항목의 instanceUuid·계획·항목 이름)은 항목을 읽을 수 있을 때만 채운다.
- **계획 만들기**: VM은 `ref`(moref·인벤토리 경로·instanceUuid)로 고른다(없으면 400 `vmware_vm_not_found`). 대상 이름은
  `target_name` 또는 원본 이름을 다듬은 것, 계획 안에서 겹치면 400 `name_taken`. 항목 이름이 겹치면 `-2`… 를 붙인다. 항목은
  `draft` 상태에 인벤토리의 `source`를 적고(디스크 표 `disks`는 스테이지 4가 채운다), 하나라도 만들지 못하면 만든 항목과 계획을
  지운다. Raft 독점 키 거절은 409 `vm_in_migration`(그 VM을 가진 계획·항목 이름을 알린다). `cutover.at`은 API에서도 유닉스
  초이고 CLI가 RFC 3339를 바꾼다.
- **매핑**: `import_spec_with`는 더하지 않았다 — 지금 `import_spec`은 풀을 받지 않고(볼륨 이름만) 디스크별 풀은 만들 볼륨 목록
  (`vmware_migrations/plan.rs` `map_vm` — `{대상}-disk{i}`, 크기 = 원본 용량, 풀의 기본 형식, 레이블 `orbivisor.io/vmware-vm`)에
  있다. 네트워크는 덮어쓰기(원본 이름) > 규칙(이름 또는 `moref:{키}`) > 제안(**묻는 주체가 볼 수 있는** 같은 이름 네트워크만 —
  숨은 네트워크를 고르지 않게). `reserve_ips`의 Tools IP 예약은 스테이지 5(덮어쓰기 `ips`만 쓴다). Windows + `driver_iso`면 CD를
  붙인다. `start_after`면 켜진 채 만드는 VM으로 확인한다(스케줄러가 노드를 고른다). 제안의 "원본을 끄고 켜라"(MAC) 경고는
  전환이 끄므로 뺐다.
- **머신**: 미리 확인은 `import_spec`의 `q35`를 빼고 만들기처럼 노드 아키텍처로 채운다(x86 클러스터는 q35, aarch64 실험실은
  virt — 그러면 Windows의 SATA는 막힌다). 스테이지 5의 VM 만들기도 같게 채워야 미리 확인과 맞다.
- **미리 확인 차례(VM마다)**: 매핑을 풀지 못하면(네트워크 `network_unmapped`·풀 `pool_unmapped`·SATA 개수) 대상 폴더 권한을 먼저
  보고(거절이면 그것만) 매핑 문제를 낸다. 풀면 계획 71 `vm_checks::check_planned`(핸들러에서 꺼낸 안쪽 — 대상 폴더 `vm.create`
  먼저, 첫 거절에서 멈춤, 만들 볼륨 16개 한도 없이)를 부르고, 거절이 없을 때만 제안 경고·게스트 힌트·Mirror 폴더 알림
  (`folder_created`)을 더한다. VMware 쪽 표(`source`·`disks.N` 자리, 인벤토리 값만)는 늘 더한다(우리 숨은 것이 아니다). 자리는
  계획 71의 것에 `folder`·`source`·`guest`를 더했다. `node`는 노드 이름을 볼 수 있을 때만, `spec_preview`는 우리 쪽을 통과했을
  때만. 미리 확인 끝점은 노드 캐시를 쓰고(`refresh`로 다시), 시작은 늘 다시 읽는다.
- **VMware 쪽 코드**: `vmware_vm_replaced`(instanceUuid 바뀜)·`vmware_vm_gone`·`vmware_snapshots`(우리 moref 아닌 스냅샷)·`vmware_ft`·
  `vmware_template`·`vmware_hw_version`·`vmware_restricted`(따뜻한만)·`vmware_powered_on`(차가운만)·`vmware_no_tools`(경고 —
  따뜻한 + 게스트 종료)·`vmware_too_many_disks`(32)·`vmware_no_disks`·`vmware_rdm`·`vmware_disk_backing`(flat이 아님)·
  `vmware_multi_writer`·`vmware_independent_disk`(따뜻한만)·`vmware_needs_vddk`(vSAN·vVols + `/folder`). 게스트 힌트:
  `guest_initramfs`(RHEL 계열 + virtio-blk)·`guest_drivers`(Windows, ISO 없음)·`guest_nic_renamed`(Tools IP가 있고 NIC 모델이 바뀜).
  NIC 16개 한도는 VmSpec 검증(우리 쪽)이 잡는다.
- **상태 동작**: `start`(`draft`만 — 409 `migration_not_draft`)는 계획을 먼저 `running`으로(revision 확인 — 겹친 요청은 409
  `migration_busy`) 바꾸고 막힌 항목은 `failed`(`error`에 코드·이유), 통과는 `pending`(+ 고른 노드를 `spec.node`에 — 고르지
  못했으면 비움, 경고는 `warnings`)로 둔다. `pause`·`resume`은 `running`↔`paused`(409 `migration_state`), 멈춘 사이 더한 VM은
  `resume`이 미리 확인한다. `cancel`은 원본·우리 쪽에 남긴 것이 없는 항목(`draft`·`pending`·`failed`이고 스냅샷·CBT·볼륨·대상
  VM·바꾼 원본 설정 없음)만 곧 `cancelled`로 둔다. `PATCH`는 `draft`·`paused`만(409 `migration_running`), 시작한 VM 빼기는 409
  `vm_started`. `DELETE`는 `draft`이거나 끝난 계획이고 모든 항목이 끝났거나 시작하지 않았을 때만(409 `migration_active`). 항목은
  이름·id·moref·instanceUuid·원본 VM 이름으로 고른다.
- **CLI**: `vmware source add|list|get|set|check|delete|browse`, `vmware migration create|preflight|list|get|start|pause|resume|cancel|
  delete`(`oplctl/src/vmware.rs`). `source add`에 지문이 없으면 비밀번호를 묻지 않고(자리 값 `-`) 서버 지문을 알린다. `--sync-every`는
  `45m`·`2h`·분, `--cutover-at`은 RFC 3339, `--folders mirror|flat --folder 경로`, `--new-macs`·`--no-start`(설계의
  `keep_macs`·`start_after` 끄기). `preflight`에 막힌 VM이 있으면 끝 코드 1. `vmware vm …`(전환·되돌리기 등)은 스테이지 4·5.

### 스테이지 4

- **쓰기 길**: `opl-store` `NbdClient::{write, write_zeroes, flush, can_write_zeroes}`(읽기 전용 내보내기에는 보내지 않음, 서버가
  `WRITE_ZEROES`를 모르면 0 바이트를 씀). `opl-driver` `nbd_target`·`NbdTarget`과 `Storage::open_target`(기본 = `qemu-nbd`, 모든 풀
  형식). `qemu-nbd` 인자는 `--shared=1 --persistent --discard=unmap --detect-zeroes=unmap --image-opts …`이고 **캐시는 기본
  (writeback)** 이다 — §9.1의 `--cache=none --aio=native`는 O_DIRECT를 받지 않는 파일 시스템(tmpfs, 옛 ZFS 데이터셋)에서 열리지
  않으므로 빼고, 체크포인트마다·디스크 끝에 클라이언트 `FLUSH` 뒤 SIGTERM으로 닫는다(`--persistent`는 띄웠는지 보는 탐침 연결이
  서버를 끝내지 않게). 시험의 가짜 스토리지는 `open_target`을 같은 프로세스 NBD 서버로 바꾼다 — 그래서 `opl-store` 서버가 쓰는
  원본(`ExportSource::writable`·`write`·`write_zeroes`·`flush`, 메모리 `MemoryTarget`)도 받게 했다(백업 내보내기는 그대로 읽기
  전용).
- **회차 표시**: 회차 n이 진행 중인지는 `snapshot.name == snapshot_name(id, rounds + 1)`로, 디스크가 이번 회차를 끝냈는지는
  `disk.rounds >= n`으로 안다(디스크 `change_id`·`rounds`·`last_delta_bytes`를 함께 적는다). 스냅샷을 만들고 적기 전에 끊겼으면
  원본 스냅샷 나무에서 이번 이름의 것을 찾아 쓴다. 체크포인트 `copied_bytes`는 구역 목록 안의 위치이고 조각을 다 쓰고 `FLUSH`한
  뒤에만 적는다(64 KiB·512 MiB·30초 — 시험이 바이트 간격을 줄인다).
- **구역 잇기**: 사이가 256 KiB 이하인 CBT 구역은 하나로 읽는다(`cbt::coalesce` — 작은 범위마다 티켓·HTTPS 요청이 드는 것을 줄임).
  사이는 얼어 있는 기반의 바뀌지 않은 내용이라 다시 써도 대상이 바뀌지 않는다. 그래서 `/folder`로 읽는 바이트가 바뀐 블록보다
  조금 많을 수 있다(가짜 vSphere의 빈 블록 하나짜리 사이는 모두 이어진다).
- **실패**: 다시 해도 같은 것(인증·권한·지문·무료 ESXi, VM이 없어짐·바뀜, 디스크 수·키가 바뀜·줄어듦, 켜진 VM의 차가운 이전,
  차가운 이전의 사용자 스냅샷, 스냅샷 지우기 실패, 볼륨 이름 겹침)은 곧 `failed`, 나머지는 항목에 `error`와 `sync_failures`를 적고
  상태를 둔 채 스케줄러가 다음 틱에 체크포인트부터 다시 한다(연달아 3번 — `MAX_SYNC_FAILURES` — 이면 `failed`). 무료 ESXi
  (`RestrictedVersion`)면 원본 연결 `status.restricted`를 적는다. 바이트가 10분(`vmware_stall`) 늘지 않으면 조각 하나의 읽기·쓰기가
  끝나지 않은 것으로 보고 실패한다.
- **타입 더함**(빈 값은 쓰지 않음 — 옛 JSON 그대로): `VmwareSyncDisk.source_mtime`(차가운 이전의 flat 수정 시각 —
  `SearchDatastore_Task`, 읽지 못하면 경고), `VmwareVmStatus.started_at`(`stop_after_days`를 셈), `sync_failures`.
- **`stop_after_days`**: 스케줄러가 아니라 동기화 Task가 본다 — 지났으면 회차 대신 우리 스냅샷을 지우고 `next_sync_at`을 비운 뒤
  경고(changeId는 남김). `POST /vmware-vms/{key}/retry`가 다시 시작하고 날을 새로 센다.
- **차가운 이전**: 디스크 전체를 읽는다(0 조각은 `WRITE_ZEROES`). §9.2의 "CBT가 켜져 있으면 `*`로 빈 곳 건너뛰기"는 하지 않았다
  (스냅샷 없는 `QueryChangedDiskAreas`가 실제 vSphere에서 되는지 확인하지 못함). 끝에 원본이 아직 꺼져 있는지 본다. 상태는
  `ready`(따뜻한 이전은 `syncing`).
- **대역**: 계획의 `bandwidth_mib_s`를 Task마다 `한도 ÷ max_parallel`로 나눠 그 Task 안에서 지킨다(노드 안 공유 토큰 통은 넣지
  않았다). 진행률은 디스크마다(`copying disk N`, 이번 회차 구역 바이트).
- **스케줄러**(`vmware_scheduler.rs`, 15초, 리더만, 기능 `vmware-migrate`가 모든 노드에 있을 때): `pending` → `vmware-sync{full}`,
  `copying`(끊김·실패) → 다시, `syncing`·`rolled-back`이고 `next_sync_at` 지남 → 델타. 델타 → 이어 가기 → 새 복사 차례. 한도는 그
  계획의 `max_parallel`과, 모든 계획의 Task를 센 같은 ESXi 호스트·같은 노드 수. 노드는 항목 `spec.node` > 규칙 `node` > 디스크
  풀을 모두 쓸 수 있는 온라인 노드 중 VMware Task가 가장 적은 것(고른 노드는 항목 스펙에 먼저 적는다). `cancelled` 계획은 남긴
  것이 있는 항목마다 `vmware-cleanup{delete_target}`(세 번 실패하면 손으로), 없는 항목은 곧 `cancelled`. 모든 항목이 끝난
  `running` 계획은 `finished`. 예약 전환은 스테이지 5.
- **정리 Task**: `vmware-cleanup`을 이 스테이지에서 넣었다(취소에 필요). 우리 스냅샷(Raft moref만)·우리가 켠 CBT·적어 둔 원본
  설정(NIC `startConnected`·설명)을 되돌리고, `delete_target`이면 대상 볼륨을 지워 `cancelled`, 아니면 `finalized`(마무리 API는
  스테이지 5). 원본 전원과 대상 VM은 건드리지 않는다(스테이지 5). 실패하면 항목을 `failed`로 두고 다시 취소하면 남은 것부터 한다.
- **API·CLI**: `POST /vmware-vms/{key}/retry`·`/cancel`(루트 `vm.create`, 기능) → `ItemAction{item, task?}`. 다시 하기는 계획이
  `running`·`paused`일 때 `failed`(시작 전이면 미리 확인부터, 복사를 시작했으면 체크포인트부터)와 멈춘 동기화만, 그 밖 409
  `vmware_vm_state`·`migration_state`, Task가 돌면 409 `migration_busy`. 취소는 남긴 것이 없으면 곧 `cancelled`, 있으면 항목
  노드(오프라인이면 이 노드)에 정리 Task(항목 줄 — 도는 동기화 뒤). `orbivisorctl vmware vm retry|cancel NAME [--wait]`.
- **Task 실행기**: `vmware-sync`·`vmware-cleanup`은 항목 줄·시간 제한 없음·이어 가는 Task. 노드 시작 sweep은 레이블
  `orbivisor.io/vmware-vm`의 항목이 끝나지 않았고 그 디스크 표에 든 `pending` 볼륨을 지우지 않는다(취소된 항목의 볼륨은 정리 Task가
  지운다). 동기화 Task는 API와 같은 노드 세션·인벤토리 캐시(`TaskRunnerConfig.api`의 `vmware_cache`)를 쓰고, 회차마다 VM 하나를
  새로 읽는다(`inventory::refresh_vm`). VDDK 길은 디스크마다 nbdkit 하나(스냅샷 moref)다.
- **잰 것**(개발 Mac, 디버그 빌드, 가짜 vSphere 루프백 + 가짜 스토리지): 256 MiB 디스크(할당 2/3) 전체 복사 107 MiB/s(2.4초),
  64 KiB 블록 16개 델타 0.28초. 실제 `qemu-nbd` 쓰기(디렉터리 풀 qcow2, `storage-it`)도 이 Mac에서 바이트가 같다. 실제 vCenter
  수치는 손 확인.

### 스테이지 5

- **걸음과 상태**: 전환 Task가 시작할 때 항목을 `cutting-over`·`preparing`으로 적고, 걸음마다 `cutover_step`을 먼저 적은 뒤 한다
  (이어 갈 때 그 걸음부터 — 걸음은 모두 다시 해도 같게 짰다). 받는 상태는 `ready`·`syncing`에 **`rolled-back`을 더했다**(되돌린
  항목을 다시 전환할 수 있게 — 켜진 채 델타가 그새 바뀐 것을 옮긴다). 타입에 더한 칸(빈 값은 쓰지 않음): `source_stopped_at`(내린
  시간의 시작), `downtime_s`, `cutover_requested_at`(예약 전환을 그 시각 뒤 한 번만 — 실패해 되돌린 항목을 점검 창마다 다시 끄지
  않게, 손 전환도 적는다).
- **실패 갈래**: `preparing`·`stopping-source` 실패는 §10.1 표대로 바꾼 원본 설정을 되돌리고(우리가 껐고 지금 꺼져 있으면 켬) 앞 상태
  (따뜻한 `syncing`·차가운 `ready`)와 오류로 둔다 — 게스트 종료 시간 넘김(`vmware_shutdown_timeout`)도 이 갈래다(원본은 켜진 채).
  `final-sync` 이후는 자동 되돌림 → `rolled-back`(따뜻한 이전은 `next_sync_at` = 지금 — 원본이 다시 켜져 바뀐 것을 곧 옮김). 되돌림은
  **NIC·설명을 먼저 되돌린 뒤 원본을 켠다**(§10.2의 "켬 → NIC 되돌림" 차례를 바꿈 — `startConnected`는 켤 때 보는 값이라 켜기 전에
  되돌려야 원본이 연결된 채 켜진다). 되돌림이 실패하면 `failed` + 걸음을 남기고 손으로 할 일("stop and delete Orbivisor VM …, then
  turn "Connect At Power On" back on …, then power on web01 in vCenter")을 오류에 적는다. 그 항목의 **`retry`는 되돌림을 다시 하는
  `vmware-cutover` Task**를 만든다(취소도 된다 — 정리 Task가 대상 VM을 지우고 원본 설정·전원을 되돌린다).
- **다시 미리 확인**: §10.1의 "§8.5 1단계를 다시"는 그대로 하지 않았다 — 대상 볼륨이 이미 있어 `check_planned`의 만들 볼륨 검사가
  이름 겹침으로 막고, 예약 전환에는 묻는 주체가 없다. 대신 (1) API(`POST …/cutover`)가 대상 폴더 `vm.create`·`vm.power`, 대상 이름,
  **원본 VM을 새로 읽은**(`refresh_vm` — 캐시의 인벤토리는 60초 묵음) VMware 쪽 표를 보고 409 `name_taken`·`preflight_blocked`
  (`details.problems`)로 막고, (2) Task의 `preparing`이 주체 없이 볼 수 있는 것(VMware 쪽 표·Tools(게스트 종료면 없을 때 막음)·대상
  이름·원본 MAC이 다른 VM에 쓰임)을 원본 호출 전에 다시 본다. 할당량·용량·주소는 VM을 만들 때 상태 머신이 보고 넘으면 자동
  되돌림이다(§13의 "할당량 → 자동 되돌림"과 같다).
- **`max_final_delta_gib`**: `preparing`의 켜진 채 델타(끄기 직전 회차)가 이 크기를 넘으면 원본을 끄지 않고 멈춘다
  (`final_delta_too_large`) — 마지막 델타는 그보다 작으므로 "원본이 너무 빨리 바뀐다"는 알림이다.
- **원본 보호**: 설명은 `Migrated to Orbivisor VM "web01" by migration "w1". Keep this VM powered off; …` 뒤에 옛 설명을 남긴다(옛
  값은 Raft `old_annotation`에 먼저). NIC는 지금 `startConnected`가 켜진 것만 끊고 그 키를 먼저 적는다. `powered_off_by_us`도 끄기 전에
  적고, 되돌림은 원본이 **지금 꺼져 있을 때만** 켠다(시간 넘김 뒤 켜진 원본에 `PowerOn`을 보내지 않음). `ShutdownGuest`의
  `InvalidPowerState`(그새 꺼짐)만 넘어가고 Tools 없음 등은 실패다.
- **VM 만들기**: `plan::map_vm`을 다시 부르고(제안 후보 네트워크 = 모든 네트워크 — Task에는 주체가 없다) 머신은
  `vms::default_machine`(동기화 노드의 아키텍처 — 미리 확인과 같게), **노드는 규칙의 노드 > 동기화 노드**(대상 볼륨이 그 노드의
  로컬 풀일 수 있고, 노드 없는 VM은 아무도 켜지 않는다). Mirror 폴더는 `inventory.folder`를 만들 때 본 대로 원본 경로의 VM 폴더를
  찾거나 만든다(`folders` 기능이 있을 때). `reserve_ips`(덮어쓰기 `ips`가 없을 때): SDN 네트워크 NIC마다 그 MAC의 Tools IP 중
  서브넷 안의 것(체계마다 하나), 밖은 Task 경고 — 그새 쓰인 주소는 IPAM이 다른 주소를 주고 경고(덮어쓰기 `ips`는 엄격 — 쓰였으면
  실패 → 되돌림). VM id는 만들기 전에 `target_vm`에 적고(이어 갈 때 다시 만들지 않음), 볼륨은 만들기 바로 전에 `ready`.
- **켜기·내린 시간**: `start_after`면 켜고 `running`을 5분(`vmware_start_timeout`) 기다린다(꺼진 채 만들면 VM을 만든 때까지가 내린
  시간). `done`에 `downtime_s`와 history "cut over; downtime 4m12s", 감사 로그에 노드의 일 `vmware-vm.cutover`("vmware-vm cutover
  w1-web01 (downtime 4m12s)")·`vmware-vm.rollback`·`vmware-vm.revert`(요청 감사는 "vmware-vm cutover …"만 — 내린 시간은 끝나야
  안다).
- **되돌리기·마무리**: `POST …/revert {confirm}`은 대상 VM의 **지금 이름**과 같아야 한다(400 `confirm_mismatch`). Task는 대상 VM을
  끄고(지우지 않음) NIC·설명을 되돌리고 원본을 켜고(우리가 끄지 않았어도 — 되돌리기는 원본을 쓰는 상태로) 남은 우리 스냅샷·우리가
  켠 CBT를 치운다 → `reverted`(끝난 상태 — 같은 VM을 새 계획에 넣을 수 있다). 실패하면 `done`에 오류(다시 하면 남은 것부터).
  `finalize`는 `vmware-cleanup{delete_target:false}`이고 **원본 보호(NIC·설명)를 그대로 둔다**(스테이지 4의 정리는 둘 다 되돌렸다 —
  §10.3대로 바꿈). 취소(`delete_target`)는 대상 VM이 남았으면 먼저 지우고 원본 설정·전원도 되돌린다.
- **경로·권한**: `/vmware-vms/{key}/{cutover|revert|finalize}`는 경로표 `Handler`(핸들러가 루트 `system.read` 뒤 대상 폴더의
  `vm.create`·`vm.power`)이고 답은 `ItemAction{item, task}`. 계획이 `running`·`paused`일 때만(409 `migration_state`), 상태가 아니면 409
  `not_ready`(전환)·`vmware_vm_state`(되돌리기·마무리), Task가 돌면 409 `migration_busy`. Task는 항목의 노드(오프라인이면 이 노드).
- **예약 전환**: 스케줄러가 `cutover.at`이 지난 `running` 계획의 `ready`·`syncing`(회차 ≥ 1) 항목을 델타와 같은 차례(0)로 같은 한도
  안에서 항목의 노드에 전환한다(노드가 오프라인이면 건너뜀). 요청 시각을 항목에 먼저 적어 한 번만 한다. 예약 전환에는 API의 다시
  확인이 없고 Task의 `preparing` 검사만 있다.
- **실행기**: `vmware-cutover`·`vmware-revert`를 이어 가는 Task·시간 제한 없는 Task 목록에 더했다(걸음마다 한도: 게스트 종료
  `shutdown_timeout_s`, 켜기 5분, 조각 멈춤 `vmware_stall`). 시험용 `TaskRunnerConfig.vmware_shutdown_limit`(종료 기다림 상한).
- **가짜 vSphere**: `fail_later(method, skip, fault)`(앞 호출 `skip`번은 지나가게 — 켜진 채 델타의 스냅샷 지우기는 되고 마지막 델타의
  지우기가 실패하는 주입). 시험 `tests/vmware_cutover.rs`(qemu 없이, `PATH=/usr/bin:/bin`): 따뜻한 전환 → 바이트 같음·원본 꺼짐·보호·
  감사 → revert(이름 틀림 400), 차가운 전환(flat 바뀜 → 다시 복사)·finalize, 사용자 스냅샷 → 409·Task도 막음, 걸음마다 실패 주입
  (`preparing`·설명 바꾸기·게스트 종료 시간 넘김·`final-sync`·켜기) → 원본 켜짐·NIC·설명 되돌림·대상 VM 없음·볼륨 `pending`,
  할당량 넘음 → 되돌림, 되돌림 실패 → 손으로 할 일 → `retry`, 예약 전환. 모든 시험 끝에 원본 디스크 바이트(시작 + 게스트 쓰기)와
  사용자 스냅샷이 그대로인지 본다.

### 스테이지 6

- **응답과 이름**: `POST /vms/{key}/export`는 202 `{export, task}`(`VmExportStarted`)다. 요청에 `name`을 더했다(없으면 백업과 같은
  `{VM}-{yyyymmdd-HHMMSS}`). 요청 칸은 `pool`·`name?`·`guest_os?`·`hw_version?`·`keep_macs`.
- **링크**: 파일 복원의 메모리 토큰 대신 **서명 링크**다 — `{만료}.{사용자}.{HMAC}`, 키는 클러스터 CA 키에서 HKDF(info
  `orbivisor vm-export download link v1`, OIDC 상태 키와 다름)로 뽑아 어느 노드든 확인한다. 10분, 사용자에 묶임(남이면 403,
  틀리거나 지나면 410 `link_expired`), 한 번만 쓰는 것은 아니다(내보내기에는 세션 노드가 없고, NFS 풀이면 어느 노드든 흘릴 수
  있다). 내려받기 경로도 경로표가 `vm.export`를 다시 본다. 감사에는 토큰 대신 `/download/-`를 적는다.
- **중계**: 파일이 이 노드에서 보이지 않으면(쓴 노드가 아니고 공유 풀이 여기서 준비되지 않음) 새 노드 사이 흐름
  `ExportDownload`로 쓴 노드(`spec.node`)에서 받는다 — `proxy.rs`가 HTTP 중계가 아니라서 파일 복원 흐름과 같은 조각
  꼴(`FileRestoreChunk`)을 쓰는 gRPC를 더했다. 옛 노드는 기능 `vm-export` 게이트 때문에 내보내기를 가질 수 없고, 모르면 409
  `cluster_upgrading`이다.
- **tar**: 8 GiB를 넘는 파일은 pax `size=` 머리와 base-256 크기 칸을 함께 쓴다(계획 29 읽개·GNU·Go 모두 읽는다). `mtime`은
  서술자 파일의 수정 시각. `?format=ovf&file=NAME`은 파일 하나(`.ovf`는 `application/xml`).
- **OVF**: OVF 1.0 봉투(`ovf:version="1.0" xml:lang="en-US"`), `rasd`·`vssd` 요소는 CIM 스키마 차례(알파벳)로, `vmw:` 확장은 그 뒤.
  pvscsi 하나에 디스크 15개(단위 7 건너뜀), AHCI 하나에 30개. 디스크 버스가 섞인 VM은 컨트롤러 종류 차례가 VM 디스크 차례와
  다를 수 있다(읽개가 "more than one controller" 경고 — 부팅 디스크는 첫 컨트롤러 종류의 첫 디스크로 남는다).
  `OperatingSystemSection ovf:id`는 guestId에서 고른 CIM 번호(Windows 103/105, Linux 101/36, 그 밖 1 — VirtualBox가 본다).
  `populatedSize`는 볼륨의 할당 크기(알 때만). BIOS VM은 `firmware=bios`. `keep_macs`면 `rasd:Address` + `vmw:Config
  addressType=manual`(vSphere가 이것으로 MAC을 지키는지는 실제 vSphere에서 확인하지 못함 — 가정).
- **guestId**: 요청 값, 없으면 그 VM을 만든 가져오기(`VmImport.status.vm`)의 원본 `guest_os`가 guestId 꼴(`…Guest`)일 때만, 아니면
  `otherGuest64` + 경고. 경고: guestId 모름, aarch64 VM, vTPM, CD-ROM, cloud-init, NUMA·전용 CPU·hugepages, 핫플러그 한도,
  HA·배치·복제, 보안 그룹·SDN 주소·NIC 속도 제한, MAC 없는 NIC.
- **막기**: 쓰는 동안 VM 켜기·스펙 바꾸기·지우기·마이그레이션은 409 `vm_exporting`(`check_no_snapshot_task` 앞에서 — 그 검사를
  부르는 모든 길). 지우기 중 다른 Task는 409 `export_busy`, 덜 쓴·실패한 내보내기의 링크는 409 `export_not_ready`. 공간은 노드가
  알린 풀의 남은 바이트와 디스크 할당 크기(모르면 가상 크기) 합 + 1 GiB를 견주고(507), 노드가 크기를 모르면 막지 않는다.
  내보내기가 남은 풀은 지울 수 없다(409 `pool_in_use`).
- **권한**: 경로표 객체 `Export`(이름은 `Privilege::VmExport`와 겹치지 않게) — 주인은 내보낸 VM(지운 VM이면 루트), `folder_kind`에
  `vm-export`를 넣었다. `vm-export` Task의 주인은 VM, `vm-export-delete`는 객체 없는 Task(`cluster.read`)로 둔다.
- **Task**: `vm-export`는 VM 줄·시간 제한 없음·이어 가지 않음(다시 시작으로 끊기면 실패 → 디렉터리 지움). 내보내기 디렉터리를 쓴 뒤
  파일·디렉터리·`orbivisor-exports`·풀 루트를 fsync한다. `vm-export-delete`는 내보내기 줄, 풀이 없어졌으면 리소스만 지운다.
- **watch**: `vm-export`는 아직 `WATCHABLE`에 넣지 않았다(폴더 거르기·주인 표 이벤트가 필요 — 스테이지 7이 필요하면 더한다;
  그때까지 화면은 Task watch와 목록 다시 읽기).
- **CLI**: `export download`는 전역 `-o`(출력 형식)와 겹치지 않게 `-O FILE`(파일 복원 `get`과 같다), `--file NAME`은 파일 하나.
  `vm export --download FILE`은 `--wait`를 품는다.
- **CI**: `govc import.ova` 단계는 스테이지 2의 `vmware-sim` 작업에 붙인다(`OPL_EXPORT_OVA_OUT`로 storage-it 왕복 시험이 OVA를 남김).

For stage 7 (web): API = `POST /vms/{key}/export` body `ExportVmRequest`, 202 `VmExportStarted`; list/get `VmExport`; `DELETE` → Task;
  `POST /vm-exports/{key}/download-link` → `DownloadLink{url, expires_in}` (url relative to /api/v1, like file restore) — navigate
  the browser to `/api/v1{url}` (cookie auth works; same user). Errors: `vm_running`, `pool_not_file`, `insufficient_storage`(507),
  `vm_exporting`, `export_busy`, `export_not_ready`, `cluster_upgrading`. Only dir/nfs pools qualify. No watch for vm-export yet.
  `web/src/auth/permissions.ts` still lacks "vm.export".
For stage 8: docs (vms.md export section, access.md vm.export, development.md test commands incl. storage-it round trip), real test
  in opld `qemu-it` can reuse the `an_exported_ova_imports_back_with_the_same_disk` flow; guide must say MAC/addressType and vSphere
  import are unverified on real vSphere.

### 스테이지 7

- **스테이지 5 끝점**: 웹을 스테이지 5와 함께 짜서 `schema.d.ts`에 `POST /vmware-vms/{key}/{cutover|revert|finalize}`가 아직 없다.
  `web/src/vmware/api5.ts`가 설계대로 손으로 적은 경로 타입(응답은 `retry`·`cancel`과 같은 `ItemAction`으로 가정, `revert` 본문
  `{confirm: "<대상 VM 이름>"}`)으로 같은 `api` 클라이언트(401·연결 오류 처리 그대로)를 부른다. 스테이지 5를 합치고 `gen:api`
  하면 이 파일을 지우고 `api.POST`를 바로 쓴다(응답 모양이 다르면 그때 맞춘다). 시험은 msw.
- **watch**: `vmware-source`·`vmware-migration`·`vmware-vm`은 `VIEW_KINDS`(사건이 오면 `["vmware-sources"]`·`["vmware-migrations"]`·
  `["vmware-vms"]` 질의를 다시 읽음 — 원본은 비밀번호를 뺀 보기, 항목은 계획마다 `GET /vmware-vms?migration=`). `vm-export`는 watch
  종류가 아니라서 Task 사건마다 `["vm-exports"]`를 다시 읽는다(`REFRESH_ON.task`, 보는 화면이 있을 때만).
- **VM 메뉴의 내보내기**: VM 상세 ⋯의 "OVA로 내보내기…"(꺼진 VM·템플릿, 그 VM의 `vm.export` — 켜져 있으면 막힌 항목과 까닭)는
  대화상자를 그 자리에서 열지 않고 **내보내기 보기 `/vm-exports?export=<VM>`** 로 가서 연다. 메뉴(첫 화면 묶음의 `VmMenu` 조각)가
  내보내기 조각을 늦게 읽으면 그 `import()`와 미리 받기 목록만으로 +550 B였다. 내보내기의 진행·내려받기가 그 보기에 있어 흐름도
  짧다. 목록 행의 ⋯에는 넣지 않았다(설계 §13대로 상세만).
- **조각**: VMware 보기 넷은 한 조각 `vmware/pages`(gzip 15.2 KB — 경로마다 나누면 Radix 공용 조각이 갈라져 첫 화면이 +550 B),
  내보내기 보기와 대화상자는 `exports/pages`(3.1 KB). 문구는 늦게 받는 묶음 `vmware/i18n`(en 4.5 KB·ko 5.0 KB)·`exports/i18n`
  (0.96·1.19 KB) — 맨 위 키 하나(`vmware`·`vmExports`). 오류 문구도 그 안(`vmware.errors.<code>`·`vmExports.errors.<code>`)이라
  대화상자에 던지기 전에 `localized()`가 문장으로 바꾼다(코드에 `vmware:` 머리를 붙여 공통 `errors.<code>`를 건너뜀).
- **첫 화면 묶음**: 479,456 B → **480,433 B(+977 B, 1 KB 밑)** — 보기 이름·팔레트 낱말·Task 이름 여섯·`vms.action.export`·
  `errors.vm_exporting`(VM 켜기가 낼 수 있음)·권한·watch 종류·경로 표. 지키느라: 조각 둘로 묶음, 메뉴는 주소로 감, 새 Tailwind
  유틸 대신 이미 있는 것(CSS 0 B), 나무 보기 전환은 공용 `ToggleGroup` 대신 단추 둘.
- **마법사**: ⑤ 확인 단계에 들어갈 때 `draft` 계획을 만든다(그래야 서버 미리 확인을 부를 수 있다). 뒤로 갔다 오면 새로 만들지 않고
  `PATCH`(규칙·한도·주기·전환 정책 + `add_vms`·`remove_vms`)로 맞춘 뒤 다시 확인한다 — 초안이 생긴 뒤 이름·원본·방식은 잠근다.
  "막힌 VM은 빼고 시작"은 시작 전에 막힌 항목을 `remove_vms`로 **계획에서 뺀다**(서버 `start`는 막힌 항목을 `failed`로 남기지만,
  빼면 다른 계획에 넣을 수 있다). "초안으로 두기"는 상세로 간다(상세의 "확인"·"시작"). 매핑 단계는 디스크를 둘 풀(데이터스토어마다
  또는 기본 풀)만 요구하고, 네트워크는 같은 이름을 미리 고르되 비워도 된다(미리 확인이 `network_unmapped`로 알림). 규칙에는 고른
  VM이 쓰는 네트워크·데이터스토어의 매핑만 넣고, 비운 칸(`bandwidth_mib_s`·`at`·`max_final_delta_gib`·`default_pool`·`node`·
  `devices`·`driver_iso`)은 보내지 않는다(서버 기본값). `cutover.at`은 브라우저 지역 시각 → 유닉스 초.
- **나무**: "VM과 폴더"(데이터센터 › VM 폴더 — `vm` 폴더는 데이터센터와 같게) | "호스트와 클러스터"(데이터센터 › 클러스터 › 호스트,
  클러스터 없는 호스트는 데이터센터 바로 아래) 두 보기, 마디마다 VM 수, 고른 마디 아래만 표에. ARIA `tree`가 아니라 `aria-pressed`
  단추 목록이다(키보드는 Tab — 수백 VM이면 찾기 칸이 주 길). 찾기는 이름·경로·OS·호스트·Tools IP, 전원 거르기, 이미 끝나지 않은
  계획에 든 VM(`migrating`)은 흐리게·고를 수 없음·까닭을 설명으로. 사용자 스냅샷 수는 우리 이름(`orbivisor-migration-`)을 뺀다.
- **권한(단추)**: 계획 동작·다시 하기·취소는 루트 `vm.create`, 전환·되돌리기·마무리는 대상 폴더(덮어쓰기 `folder` > 규칙
  `into`/`under`)의 `vm.create` + `vm.power`, 원본 바꾸기는 루트 `cluster.admin`, 내보내기·내려받기·지우기는 그 VM의 `vm.export`
  (VM을 지웠으면 루트). 보기 "VMware 원본"은 루트 `cluster.read`, "VMware 이전"은 루트 `system.read`가 있어야 보인다.
- **전환 대화상자**: 고른 항목마다 예상 내릴 시간(`estimate_s`)·마지막 델타, 여럿이면 합, 원본 처리(게스트 종료와 기다림·전원 끄기,
  `protect_source`면 NIC 끊김, `start_after`)를 보이고 항목마다 차례로 요청한다(일부 실패는 알림, 모두 실패면 대화상자 오류).
  되돌리기는 대상 VM 이름을 그대로 적어야 진행한다(공용 `ConfirmDialog` `typeToConfirm`).
- **원본 대화상자**: 계획 30 `EsxiImportDialog`와 같은 흐름 — 지문 없이 보내 409 `untrusted_certificate`면 SHA-256 지문과 SHA-1
  썸프린트(VDDK)를 보이고 "믿고 연결"로 다시 보낸다. 고치기는 바뀐 칸만 `PATCH`(비밀번호는 비우면 그대로). 비밀번호는 컴포넌트
  상태와 요청 본문에만 — 시험이 `localStorage`·`sessionStorage`·질의 캐시에 없음을 본다.
- **휴대폰 폭·대비**: jsdom은 배치를 재지 못해 390 px 가로 넘침은 브라우저 E2E(`builder.spec.ts`의 새 시험 — 네 보기를 밝은·어둠
  테마로)에서 본다. 그 시험이 셸의 보기 전환(`AreaTabs` `ViewSwitch`, 보기 넷이면 약 440 px)이 문서를 가로로 넘기는 것을 잡아,
  보기 전환 줄만 가로로 넘기게(`max-w-full overflow-x-auto`) 고쳤다(네트워크 탭의 보기 일곱도 같은 문제였다). 새 색 토큰은 없다(상태 칩·`Badge` 기존 토큰 — `tokens.test.ts`가 대비를 본다). axe는 화면 시험마다.
- **스테이지 8이 할 것**: 스테이지 5 합친 뒤 `gen:api` → `api5.ts` 지우기, `web/e2e/vmware.spec.ts`(가짜 vSphere로 원본 더하기 →
  마법사 → `Ready` → 전환 → VM 링크, 내보내기 → 내려받기), `docs/guide/web-ui.md`(가져오기 탭 보기 넷·VM 메뉴의 내보내기가 내보내기
  보기로 감).

### 스테이지 7 맞춤 (스테이지 5를 합친 뒤)

- `api5.ts`를 지우고 생성된 `POST /vmware-vms/{key}/{cutover|revert|finalize}`를 바로 부른다(답 `ItemAction`은 가정과 같았다).
- 전환 단추는 `rolled-back`에도 켜진다(스테이지 5가 받는 상태). 409 `preflight_blocked`는 `details.problems`의 `자리: 까닭`을 문구
  뒤에 잇고, `not_ready`·`confirm_mismatch`·`name_taken`(서버 설명 포함) 문구를 더했다.
- **되돌리기 확인 이름은 대상 VM의 지금 이름**이다(서버가 `target_vm`의 지금 이름과 견준다 — 전환 뒤 VM 이름을 바꿨으면
  `target_name`으로는 400 `confirm_mismatch`). VM 목록에서 찾고, 없으면 `target_name`.
- `failed` + `cutover_step` = 자동 되돌림 실패: 칩 "되돌림 실패: <걸음>", 서랍의 오류 위에 "손으로 하거나 다시 하기" 머리(오류에
  서버가 적은 손으로 할 일), 단추 "되돌림 다시 하기"(`retry`가 되돌림 Task를 만든다).
- 새 칸: 서랍에 전환 요청·원본 끈 시각·내린 시간(`cutover_requested_at`·`source_stopped_at`·`downtime_s`), 표의 예상 내릴 시간
  칸은 끝난 항목이면 실제 내린 시간("4m 12s(실제)"). history의 "cut over; downtime …"는 서버 글 그대로.
- 첫 화면 묶음 480,433 → 480,434 B.
