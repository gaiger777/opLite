# 가상 머신

설치 매체·cloud-init, 템플릿·링크드 클론, 이미지, vTPM·Secure Boot, 게스트 에이전트, 무중단 유형 변경, CPU 모델과 플래그, NUMA·전용 CPU·hugepages, 라이브 마이그레이션, 만들기 전 미리 확인. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [설치 매체와 cloud-init](#설치-매체와-cloud-init)
- [템플릿과 링크드 클론](#템플릿과-링크드-클론)
- [이미지 라이브러리](#이미지-라이브러리)
- [이미지 업로드와 머신 종류](#이미지-업로드와-머신-종류)
- [vTPM과 Secure Boot](#vtpm과-secure-boot)
- [게스트 에이전트](#게스트-에이전트)
- [VM 무중단 유형 변경](#vm-무중단-유형-변경)
- [CPU 모델과 플래그](#cpu-모델과-플래그)
- [취약점 때문에 CPU 기능 숨기기](#취약점-때문에-cpu-기능-숨기기)
- [NUMA·전용 CPU·hugepages](#numa전용-cpuhugepages)
- [라이브 마이그레이션](#라이브-마이그레이션)
- [만들기 전 미리 확인](#만들기-전-미리-확인)

## 설치 매체와 cloud-init

OS를 설치하는 두 가지 길을 준다: **설치 ISO**를 VM의 CD/DVD 드라이브에 넣어 부팅하거나, **클라우드 이미지**를 디스크로
받고 **cloud-init**(NoCloud 씨앗)으로 사용자·SSH 키·주소를 넣는다(Proxmox VE의 ISO 저장소·CD/DVD 드라이브·cloud-init
드라이브, vSphere의 데이터스토어 ISO·게스트 사용자 지정과 같은 자리). 모든 노드가 이 버전이어야 한다(노드가 알리는 기능
`install-media` — 아니면 ISO 볼륨·받기 Task·CD-ROM·cloud-init이 든 VM 스펙·`POST /vms/{key}/cdrom`은 **409**
`cluster_upgrading`). CD-ROM도 cloud-init도 없는 VM의 스펙과 QEMU 명령줄은 그대로다. 설계는
[설치 매체와 cloud-init 설계](../superpowers/specs/2026-11-17-install-media-cloud-init-design.md).

**ISO 이미지.** 내용 종류가 `iso`인 raw 볼륨이다. **디렉터리·NFS 풀에만** 둔다(블록 풀은 400). 만드는 길은 둘이다.

```bash
orbivisorctl iso upload alpine --pool nas --file alpine-virt-3.20.3-x86_64.iso   # 접속한 노드로 올린다
orbivisorctl iso download ubuntu-24-04 --pool nas \
  --url https://releases.ubuntu.com/24.04/ubuntu-24.04.1-live-server-amd64.iso --sha256 <HEX>
orbivisorctl iso list                      # NAME POOL SIZE NODE USED-BY
orbivisorctl iso delete alpine
```

- 내용을 확인한다: 32 KiB 뒤에 `CD001`(ISO 9660)이나 `BEA01`(UDF — Windows 설치 매체)이 없으면 Task가
  `not an ISO 9660 or UDF image`로 실패하고 볼륨을 지운다. 크기 상한은 따로 없다(업로드 디렉터리의 여유 공간과 풀 용량).
- **URL에서 받기**(`iso download`, 디스크 이미지는 `volume download`, 웹 "URL에서 받기", `POST /volumes/download` → Task
  `volume-download`)는 풀을 쓸 수 있는 노드가 받는다 — **그 노드가 URL에 닿아야 한다**. `http`·`https`만, 리디렉션 10번,
  연결 30초·읽기 쉼 120초, 진행률은 Task에(`downloading 1.2 GiB / 2.5 GiB`). `--sha256`을 주면 받은 파일이 같아야 한다
  (다르면 실패하고 지운다). 프록시는 노드의 `HTTPS_PROXY`·`HTTP_PROXY`·`NO_PROXY` 환경 변수다(systemd 유닛에
  `Environment=`로). **볼륨 쓰기 권한(operator 이상)을 가진 사용자만** 받기를 쓸 수 있고, 노드는 기본으로 루프백·링크 로컬
  (클라우드 메타데이터 169.254.169.254 포함)·사설망·공유(100.64/10)·`0.0.0.0/8`·시험망(198.18/15)·예약(240/4)·멀티캐스트
  주소에서는 받지 않는다(이름이 그런 주소로 풀려도, 리디렉션의 어느 단계여도, IPv4를 담은 IPv6 — `::ffff:a.b.c.d`·`::a.b.c.d`·
  NAT64 `64:ff9b::/96`·6to4 `2002::/16` — 여도 — `address 10.0.0.9 is loopback, link-local or private`). 사내 미러가 사설 주소면 opld 설정 `[download]`의
  `allow_private = true`로 켠다. 프록시를 쓰면 대상 주소는 프록시가 푼다.
  URL의 `user:password@`는 `***@`로, 쿼리 문자열(서명·토큰)은 `?***`로 감사 기록·오류 문구·Task 응답(`GET /tasks`, watch)에서
  가린다(저장소의 Task에는 원래 URL이 남는다 — 아래 한계).
- **여러 VM이 함께 쓴다**: CD 매체는 늘 읽기 전용으로 열므로 한 ISO를 여러 VM에 넣을 수 있다. 디스크로는 붙이지 못한다
  (400 `attach it as a CD-ROM`). 크기를 바꾸지 못한다.
- **지우기**: VM의 CD-ROM 드라이브에 있으면(스펙, 또는 실행 중 빼기를 아직 마치지 못한 매체) **409** `volume_in_use`
  (`volume "alpine" is in the CD-ROM drive of VM "web"`), 스냅샷의 VM 스펙이 가리키면 **409** `volume_in_snapshot`
  (되돌리기가 그 매체로 켠다). 백업은 막지 않는다(복원이 드라이브를 비운다). 확인한 뒤 다른 노드에서 그 ISO를 넣었거나 그
  매체가 든 스냅샷을 찍었으면 지우기 Task가 다시 보고 같은 문구로 실패한다. 지우기 Task가 끝나지 않은 ISO는 CD에 넣지 못한다
  (**409** `volume_in_use` `ISO "alpine" is being deleted (task …)`).
- 웹: 가상 머신 › 이미지·ISO › ISO 이미지 보기(풀별 표, 올리기·URL에서 받기·지우기, 넣은 VM). 볼륨 화면은 ISO를 빼고 보이며 "ISO 이미지도 보기"로
  함께 본다.

**CD/DVD 드라이브.** VM마다 4개까지(`spec.cdroms`, 드라이브 번호 = 순번 `cdrom0`…). 빈 드라이브도 된다.

```bash
orbivisorctl vm create web --disk web-root --cdrom alpine --cdrom none
orbivisorctl vm cdrom insert web ubuntu-24-04 --drive 1     # 실행 중에도
orbivisorctl vm cdrom eject web                             # 드라이브 0을 비운다
orbivisorctl vm cdrom add web --boot-order 1                # 꺼진 VM만: 빈 드라이브를 끝에
orbivisorctl vm cdrom remove web --drive 1                  # 꺼진 VM만
```

- q35는 내장 AHCI의 `ide-cd`(Windows 설치 프로그램이 드라이버 없이 읽음)이고 SATA 포트 6개를 SATA 디스크 → CD-ROM →
  cloud-init 드라이브가 나눠 쓴다(넘치면 400). virt(aarch64)는 `virtio-scsi-pci` + `scsi-cd`다. 컨트롤러와 드라이브는
  명령줄 끝에 두어 CD를 더해도 디스크·NIC의 PCI 주소가 바뀌지 않는다.
- **실행 중 넣기·빼기**: `vm cdrom insert|eject`, 웹 VM 상세 "하드웨어" 탭의 CD/DVD 표, 또는 스펙의 매체만 바꾼 PUT이
  핫플러그 Task(`vm-hotplug`)로 도는 QEMU의 매체를 바꾼다. 트레이를 **강제로** 열므로 게스트가 잠가도 바뀐다(게스트에 매체
  바뀜이 알려진다). 핫플러그 배치 없이 켠 VM도 된다. 실행 중에 드라이브를 더하거나 빼면 **409** `vm_running`이다.
  매체를 바꾸는 중(스펙 ≠ 도는 QEMU)이면 `vm get`과 웹에 `<- changing from …`/"바꾸는 중"으로 보이고 마이그레이션은 먼저
  `vm hotplug`를 하라며 막힌다.

**부팅 순서.** 장치마다 `boot_order`(디스크와 CD-ROM에서 겹치지 않게). 기본은 **디스크 다음 CD-ROM**이다: 아무 장치에도
값이 없으면 디스크를 순서대로 1, 2, …, 그 뒤 CD-ROM; 값이 CD-ROM에만 있으면 디스크를 그 뒤에 차례로 두고(CD로 설치하고
매체를 뺀 UEFI VM이 디스크로 켜진다) 값 없는 CD-ROM은 그다음; 디스크에 값이 있으면 값 없는 디스크는 부팅하지 않고 값 없는
CD-ROM은 가장 큰 값 뒤에 선다. 빈 디스크는 펌웨어가 건너뛰므로 처음에는 CD의 설치 프로그램이, 설치 뒤에는 디스크의 시스템이 켜진다
— "한 번만 CD로 부팅" 없이 설치 흐름이 된다(설치 뒤 매체를 빼 두면 된다). `--boot cdrom,disk`(목록 순서대로 1, 2, …,
목록에 없는 디스크는 부팅하지 않음 — 디스크를 하나도 적지 않으면 디스크는 목록 뒤)와 웹 VM 편집의 "부팅 순서" 카드(위·아래, "부팅 안 함")로 바꾼다. OVMF·aarch64 EDK2는
순서에 든 장치만 부팅하므로 CD-ROM에는 늘 순서를 준다. 실행 중 바꾼 CD-ROM 순서는 보류(`cdroms[0].boot_order`)로 남아
다음 재시작에 적용되고, 디스크 순서는 지금처럼 409다. 실행 중 붙이는 디스크의 `boot_order`가 도는 VM의 CD-ROM이 받은
순서와 겹치면 409 `vm_running`(`boot_order 2 is already used by cdrom0 in the running VM …`)이다. `-boot once=d`는
SeaBIOS만 따르므로 쓰지 않는다.

**노드 로컬 풀의 ISO는 VM을 그 노드에 묶는다.** 스케줄러·균형 조정기·비우기는 그 노드에만 둔다. 실행 중 마이그레이션은
**409** `cdrom_local`(`… eject it or keep ISOs in a shared pool`)이고, 노드 비우기는 그 VM에서 같은 이유로 실패한다 — 매체를
몰래 빼지 않는다. HA VM과 복제(계획 55) VM에는 처음부터 공유 풀 ISO만 넣는다(400, 다른 노드에서 켜지 못하는 일을 막음).
공유 풀(NFS, `shared` 디렉터리 풀) ISO는 어디서나 쓰고 마이그레이션 양쪽 QEMU가 같은 파일을 함께 읽는다.

**cloud-init.** `spec.cloud_init`이 있으면 Orbivisor가 NoCloud 씨앗(ISO 9660 + Joliet, 볼륨 라벨 `cidata`, 파일
`meta-data`·`user-data`·`network-config`·`vendor-data`)을 VM 실행 디렉터리의 `cidata.iso`로 만들어 읽기 전용 CD로 단다(부팅
장치가 아니다). 볼륨이 아니므로 노드·풀에 남는 것이 없고, 어느 노드에서 켜든(새로 켜기·마이그레이션 받기·HA 복구·스냅샷
되돌리기) 그 자리에서 다시 만든다.

```bash
orbivisorctl volume download noble --pool local --source-format qcow2 \
  --url https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img --sha256 <HEX>
orbivisorctl volume resize noble --size 20G
orbivisorctl vm create web --disk noble --nic lan --cloud-init --ci-user orbi \
  --ci-ssh-key @~/.ssh/id_ed25519.pub --ci-ip 10.0.0.5/24,gw=10.0.0.1 --ci-dns 10.0.0.2 --ci-search lab
orbivisorctl vm set web --ci-hostname web.lab && orbivisorctl vm reboot web
```

- 필드: `hostname`(없으면 VM 이름, FQDN이면 `fqdn`도), `user`(배포판 기본 사용자의 새 이름, `root`는 안 됨), `password`,
  `ssh_keys`(32개까지), `networks`(NIC 순번마다 IPv4 `dhcp`|`static`, IPv6 `dhcp`|`slaac`|`static`), `dns`(서버 3개·검색 6개),
  `user_data`·`vendor_data`(각 64 KiB). CLI는 `--ci-hostname`·`--ci-user`·`--ci-password-file|--ci-password-prompt|--ci-password-hash`·
  `--ci-ssh-key KEY|@FILE`·`--ci-ip [nicN=]…`·`--ci-ip6`·`--ci-dns`·`--ci-search`·`--ci-user-data`·`--ci-vendor-data`, `vm set`은
  준 값만 바꾸고 `--no-cloud-init`·`--ci-no-password`·`--ci-clear-ssh-keys`로 끄거나 지운다. 웹은 VM 편집의 "cloud-init" 카드.
- 네트워크는 **network-config v1**(cloud-init의 모든 렌더러·옛 판·cloudbase-init이 읽음)이고 MAC으로 맞춰 `eth{i}`로
  이름을 바꾼다. 주소도 DNS도 주지 않으면 파일을 넣지 않는다(cloud-init 기본 — 첫 NIC DHCP). 하나라도 주면 모든 NIC를 적는다
  (주지 않은 NIC는 IPv4 DHCP). IPv6만 준 NIC(`--ci-ip6`만, 웹 IPv4 "없음(IPv6만)")에는 IPv4를 두지 않는다 — DHCPv4도 쓰려면
  `--ci-ip nicN=dhcp`.
- `user_data`를 주면 **만든 user-data 대신** 그대로 쓴다(Proxmox `cicustom`) — 이때 `user`·`password`·`ssh_keys`는 쓰이지
  않고 API가 `orbivisor-warning` 헤더로, CLI는 stderr로, 웹은 폼에서 알린다. `vendor_data`는 만든 user-data와 함께 돈다
  (패키지·`runcmd`는 여기에). **`user_data`·`vendor_data`는 그대로 저장되고 VM 쓰기 권한자에게는 보인다**(고쳐 보내야
  하므로). 읽기만 하는 사용자(viewer)에게는 VM·스냅샷·백업 응답과 watch에서 `********`로 가리며, PUT에 `********`를 되보내면
  저장된 값을 둔다. watch 스트림은 연결할 때의 권한으로 가리므로 사용자의 역할이 바뀌면 다시 연결해야 가림이 바뀐다. 그래도
  비밀은 user-data에 넣지 말고 SSH 키·해시를 쓴다.
- **instance-id는 내용의 해시다**(`iid-` + 16자): 설정을 바꾸면 instance-id가 바뀌어 cloud-init이 인스턴스마다 하는 일을 다시
  한다(사용자·키·호스트 이름·네트워크, **SSH 호스트 키도 다시 만든다** — 클라이언트의 known_hosts 경고). 같으면 다시 켜도
  다시 하지 않는다. 바꾼 설정은 **다음 QEMU 시작**(`vm reboot`·끄고 켜기·마이그레이션 받는 쪽)에 적용되고, 그동안 보류
  `cloud_init`으로 보인다. 게스트 안의 재부팅(같은 QEMU)은 옛 씨앗을 본다. 실행 중 cloud-init을 바꾸거나 켜고 끄면(호스트
  이름 없는 VM의 이름 바꾸기도) 재시작할 때까지 마이그레이션이 막힌다(**409** `restart_required`
  `cloud-init changed while running; restart the VM before migrating it` — 받는 쪽은 씨앗을 지금 스펙으로 다시 만든다). 노드
  비우기의 결과와 HA 로그도 이 문구를 그대로 보인다. 실행 중 붙인 NIC는 다음 재시작 때 씨앗에 들어가며(보류 `cloud_init`)
  마이그레이션을 막지 않는다(받는 쪽은 켤 때 단 NIC로 같은 씨앗을 만든다).
- Windows: cloudbase-init은 NoCloud를 읽으므로 같은 씨앗을 쓸 수 있다(사용자 이름·비밀번호·키·호스트 이름). sysprep 응답
  파일은 만들지 않는다.

**비밀번호.** 평문은 API가 **SHA-512 crypt**(`$6$`, 무작위 솔트, 5000회)로 바꿔 해시만 저장한다. cloud-init이 해시로 보는
모양(`$6$`·`$5$`·`$1$`·`$2a$`·`$2y$`, 예: `mkpasswd -m sha-512`)이면 그대로 둔다. yescrypt(`$y$` — 요즘 배포판 `mkpasswd`의
기본)와 bcrypt `$2b$`는 cloud-init이 평문으로 설정해 버리므로 **400**이다(`--ci-password-hash`도 받지 않음 — 같은 까닭으로
평문 비밀번호도 `$y$`·`$2b$`로 시작하면 400 `plain passwords may not start with $y$ or $2b$`). 모양이 어긋난
값(솔트·해시 길이)은 평문으로 보아 해시한다. 응답(VM·스냅샷·백업, watch)에는 해시도 내보내지 않고 `********`로 가린다 — PUT에 `********`를 되보내면
저장된 해시를 그대로 둔다(웹·CLI가 이렇게 한다). 해시는 Raft·스냅샷·백업 매니페스트에 남으므로 **SSH 키를 권한다**.

**다른 기능과.** 스냅샷·백업·복제(계획 55)·디스크 미러는 디스크(`disk{i}`)만 다루고 CD·씨앗은 담지 않는다. 스냅샷 되돌리기는
기록된 매체·설정으로 켠다. 백업 복원은 그 클러스터에 없거나 쓸 수 없는 ISO의 드라이브를 비우고 Task 경고(`CD-ROM drive 0:
ISO "alpine" is not available; the drive is empty`)를 남긴다. cloud-init은 그대로 옮기며 새 VM id라 instance-id가 바뀌어
다시 적용된다. VM 복제(clone)는 같은 ISO를 가리키고, `hostname`이 없으면 새 이름이 호스트 이름이다. **복제·복원한 VM은
cloud-init의 고정 IP와 호스트 이름을 그대로 가진다** — 원본과 겹치므로 Task가 경고(`the clone keeps the cloud-init static IP
addresses 10.0.0.5/24 and host name "web-01"; change them before starting it`)를 남기고, 켜기 전에 `vm set --ci-ip …
--ci-hostname …`으로 바꾼다. 원격 클러스터에 백업을 복제할 때 받는 쪽이 같은 버전이지만 기능(`install-media`)이 모자란 노드가
있으면(방어용 — 받기는 옛 버전 노드가 있으면 409 `cluster_upgrading`) CD-ROM 드라이브와 cloud-init을 버린다. 이미 있는
사본을 확인할 때는 원격 응답이 가린 비밀(`********`)과 받는 쪽이 버린 필드(복제 대상 등)를 같은 규칙으로 견준다. VMware·OVA 가져오기는 CD 장치를 가져오지 않는다.

**넣지 않은 것(나중).** 블록 풀(LVM-thin·ZFS·RBD·공유 LVM)의 ISO, 이어 받기 업로드, 한 번만 CD로 부팅, NIC(PXE) 부팅 순서,
ConfigDrive2, 패키지 설치 필드(`vendor_data`로 한다), 클라우드 이미지 템플릿·연결 복제, Windows 응답 파일(autounattend).

**한계.** SeaBIOS(q35 BIOS)는 부팅 순서에 없는 씨앗 드라이브의 첫 4096바이트도 읽어 본다(부팅하지는 않음; `-boot
strict=on`은 모든 VM 명령줄을 바꾸므로 쓰지 않음). 받기 Task의 원래 URL(`user:password@`·쿼리)은 응답에서는 가리지만
클러스터 저장소에는 남으므로 오래 쓰는 비밀이 든 URL은 피한다.

## 템플릿과 링크드 클론

VM을 **템플릿**(켤 수 없는 원본)으로 바꾸고, 템플릿에서 디스크를 통째로 복사하지 않고 기반 디스크를 함께 쓰는
**링크드 클론**(연결 복제)으로 VM을 찍어 낸다(Proxmox VE의 "Convert to template"·linked clone, vSphere의 템플릿·
linked clone과 같은 자리). 모든 노드가 이 버전이어야 한다(노드가 알리는 기능 `templates` — 아니면 `template: true`인 VM
스펙, `base`가 든 볼륨, 연결·덮어쓰기를 쓴 배포, 체크섬 파일·SHA-512·템플릿 초안을 쓴 받기, `volume-flatten`·
`vm-untemplate` Task, 관리자 카탈로그 항목은 **409** `cluster_upgrading`). 템플릿도 연결 복제도 없는 VM·볼륨·Task의
JSON은 그대로이고, 새 옵션 없는 `vm clone`은 이전과 같은 Task를 만든다. 설계는
[템플릿·연결 복제·이미지 라이브러리 설계](../superpowers/specs/2026-11-17-templates-linked-clones-design.md).

**템플릿으로 바꾸기·되돌리기.** 템플릿은 새 리소스가 아니라 `spec.template: true`인 VM이다 — VM 목록·watch·권한·감사·
백업이 그대로 다룬다.

```bash
orbivisorctl template convert ubuntu-base        # 꺼진 VM → 템플릿(POST /vms/{key}/template, 200)
orbivisorctl template list                       # NAME DISKS POOL CLONES
orbivisorctl template unconvert ubuntu-base      # 템플릿 → 보통 VM(DELETE /vms/{key}/template, 202 Task vm-untemplate)
orbivisorctl template delete ubuntu-base         # 되돌리기 Task를 기다린 뒤 VM을 지운다(디스크 볼륨은 남는다)
```

- **바꾸기 조건**: 꺼져 있고(**409** `vm_running`), 마이그레이션·스냅샷·복제·백업·평탄화 Task가 없고(**409** `vm_busy`),
  스냅샷이 없고(**409** `vm_has_snapshots` — 되돌리기가 기반을 바꾸면 안 됨), `ha`·`replication`·`snapshot_schedule`이
  없고(**400**, 지우고 다시), 디스크가 모두 준비된 디스크 볼륨이며 연결 복제가 아니어야 한다(**400** `volume … is a linked
  clone; flatten it before making a template` — 사슬은 1단). 저장소에는 아무것도 하지 않는다(기반 스냅샷은 첫 연결 복제가
  만든다). `POST /vms`에 `template: true`를 주어 처음부터 템플릿으로 만들 수도 있다(같은 규칙).
- **불변 규칙**(템플릿인 동안): 켜기·재시작·마이그레이션·복제(`replicate`)·스냅샷 만들기·스냅샷 되돌리기·CD 넣기는
  **409** `vm_is_template`(`VM "tmpl" is a template; deploy a VM from it or convert it back`). `PUT`이 디스크 목록(볼륨·
  버스·개수)을 바꾸면 **409** `vm_is_template`, `ha`·`replication`·`snapshot_schedule`은 **400**이다. CPU·메모리·NIC·
  cloud-init·CD·부팅 순서는 바꿀 수 있다(배포가 그때의 스펙을 복사할 뿐 기반 데이터와 무관). 템플릿 디스크 볼륨의 크기
  바꾸기·지우기는 **409** `volume_is_template`(`volume … is a disk of template "tmpl"`). 템플릿 표시는 이 두 경로로만
  바뀐다 — `PUT /vms/{key}`는 저장된 `template`을 그대로 두므로 이 칸을 모르는 옛 클라이언트가 템플릿을 풀지 않는다.
  템플릿을 바로 지우면(`DELETE /vms/{key}`) **409** `vm_is_template`이다 — 되돌리기 Task가 블록 백엔드의 기반 스냅샷
  (`opl-base`)을 정리하게 한다(`template delete`·웹 "삭제"가 이렇게 한다).
- **되돌리기**: 연결 복제가 남아 있으면 **409** `template_in_use`(`template "tmpl" has 3 linked clone(s) (web-1, web-2,
  …); delete or flatten them first`). Task는 디스크마다 기반 스냅샷을 풀고(RBD `opl-base` 보호 풀기·지우기, ZFS
  `@opl-base` 지우기, 파일·LVM-thin은 할 일 없음) 마지막에 `template: false`를 쓴다. 중간에 끊기면 다시 하면 된다. 다른
  노드에서 동시에 배포가 시작됐으면 Raft 순서에서 뒤에 쓴 쪽이 물러난다(배포는 `template "tmpl" was converted back to a
  VM`로 실패, 되돌리기는 표시를 되돌리고 `template_in_use` 문구로 실패 — 그새 스펙이 바뀌었으면 덮지 않고 `VM "x" was left
  as a regular VM …; make it a template again`). 끝나지 않은 되돌리기 Task가 있으면 켜기·PUT·CD·둘째 되돌리기는 **409**
  `vm_busy`다. 빠진 노드의 로컬 풀에 묶인 템플릿도 되돌려 지울 수 있다(그 디스크의 기반 풀기를 건너뜀).
- reconciler는 템플릿을 켜지 않는다(`power: running`이어도 `phase: stopped`와 오류 `TEMPLATE_NOT_STARTED`). 스케줄러·
  균형 조정기·HA·노드 비우기는 템플릿을 후보에서 뺀다.

**배포.** `template deploy`(`POST /vms/{key}/clone`을 넓힌 것 — 새 경로는 없다)가 새 VM을 만든다.

```bash
orbivisorctl template deploy ubuntu-base --name web-1 --start
orbivisorctl template deploy ubuntu-base --name 'web-{n}' --count 5 --first 10 --linked \
  --node node-b --cpus 4 --memory 8192 --network nic0=prod --disk-size disk0=40G \
  --ci-user orbi --ci-ssh-key @~/.ssh/id_ed25519.pub --start
orbivisorctl template deploy ubuntu-base --name db-1 --full --pool fast   # 다른 풀에 전체 복사
orbivisorctl vm clone ubuntu-base web-9 --linked                        # vm clone도 --linked|--full
```

- **덮어쓰기**: 이름·노드(`--node`, 새 VM을 켤 곳 — 디스크가 닿아야 한다: 공유 풀이거나 그 로컬 풀의 노드, 아니면 **400**
  `node "b" cannot reach pool "local" of template "tmpl"; deploy on node "a" and migrate the VM`)·풀(전체 복사만)·vCPU
  (소켓 하나)·메모리·NIC별 네트워크(`--network [nicN=]NET`, NIC 수는 템플릿과 같다)·디스크 크기(`--disk-size [diskN=]SIZE`,
  늘리기만 — 줄이면 400, 512의 배수)·cloud-init(`--ci-*`는 [설치 매체와 cloud-init](#설치-매체와-cloud-init)과 같은 이름,
  준 칸만 바꾼다; 평문 비밀번호는 API가 SHA-512 crypt로 바꾸고 Task 응답·watch에서 가린다)·`--start`. API 본문은
  `linked`·`node`·`vcpus`·`memory_mib`·`nics`(순번별, null은 그대로)·`disks`(순번별 바이트)·`cloud_init`·`start`다.
  템플릿이 아닌 VM의 복제에도 덮어쓰기는 쓸 수 있다.
- **연결/전체**: 기본(`linked` 생략)은 템플릿이면 **되는 디스크는 연결 복제, 안 되는 디스크는 전체 복사**하고 디스크마다
  Task 경고를 남긴다(`disk1: pool "san" (lvm-shared) cannot hold linked clones; made a full copy`). `--linked`(`true`)는
  하나라도 안 되면 **400**(다른 풀, 미지원 백엔드), `--full`(`false`)은 전체 복사다. `linked: true`는 템플릿에서만 된다
  (`linked clones can be made only from templates`). 연결 복제는 **템플릿 디스크와 같은 풀**에만 만든다.

  | 백엔드 | 연결 복제 | 평탄화 |
  |---|---|---|
  | 디렉터리·NFS | qcow2 겹층, **상대 경로** backing(`qemu-img create -b {기반}.{fmt}` — 노드마다 NFS 마운트 지점이 달라도 열림) | `qemu-img rebase -b ""`(안전 모드 — 끊겨도 옛 사슬 그대로). 복제 VM에 스냅샷이 있으면 409 `vm_has_snapshots` |
  | LVM-thin | thin 스냅샷(`lvcreate -s`), raw | 할 일 없음(thin 스냅샷은 원본과 독립 — 기록만 지움) |
  | ZFS | `@opl-base` 스냅샷의 `zfs clone`, raw | `zfs send` → `zfs recv`로 새 zvol(복제의 스냅샷도 함께) → 원본 지우기 → 이름 바꾸기 |
  | Ceph RBD | 보호한 `opl-base` 스냅샷의 `rbd clone`, raw | `rbd flatten` |
  | 공유 LVM(iSCSI·FC) | 지원 안 함 → 전체 복사 + 경고(`--linked`면 400) | — |

  QEMU 쪽은 바뀌지 않는다 — 겹층 qcow2는 헤더의 backing을 QEMU가 읽기 전용으로 열고, RBD clone·ZFS clone·thin 스냅샷은
  보통 볼륨처럼 열린다.
- **켜기**: `--start`면 Task가 스케줄러로 놓을 노드를 골라(`--node`면 그 노드) 켠 채 만든다. 놓지 못하면 꺼진 채 만들고
  경고(`node "a" is in maintenance; start "web-1" later` 등)를 남긴다. CLI는 VM이 `running`이 될 때까지(5분) 기다려
  `vm web-1: running on NODE`를 쓰고, 경고는 stderr의 `warning: vm web-1: …`다. 기본으로 Task를 기다리며 `--no-wait`가
  있다. 템플릿에 고정 IP·호스트 이름 cloud-init이 있으면 계획 57처럼 겹친다는 경고를 남긴다(배포 때 새로 준 값은 보지 않음).
- **여러 대 배포**(`--count 1~100`, 웹 배포 창의 "개수")는 클라이언트가 단건 API를 차례로 불러 Task들을 함께 기다린다.
  이름에 `{n}`이 있어야 하고(`--first`부터, 기본 1), cloud-init 호스트 이름을 비우면 VM 이름이 호스트 이름이다. 2대 이상에
  `--ci-hostname`·정적 `--ci-ip`/`--ci-ip6`은 거절한다(같은 값이 겹침). 하나가 API에서 거절되면 더 만들지 않고 이미 만든
  Task를 마저 기다린 뒤 실패한다. 서버 쪽 일괄 Task·IP 풀은 없다.
- 표시: `vm list`는 템플릿의 PHASE 칸에 `template`을, `vm get`은 stderr에 `template: 2 linked clone(s)`·
  `disk0: linked clone of tmpl-root`를, `volume list`는 `BASE` 열(기반 볼륨 이름)을 보인다. `volume get`의 `spec.base`는
  기반 볼륨 id다(같은 이름으로 다시 만든 볼륨을 기반으로 오인하지 않음).

**기반 지우기 거절.** 연결 복제가 남은 볼륨을 지우면 **409** `volume_has_clones`(`volume "tmpl-disk0" is the base of 2
linked clone(s) (web-1-disk0, …); delete or flatten them first`) — 템플릿 디스크라면 그보다 먼저 `volume_is_template`이다.
배포 Task는 복제 볼륨 기록(`base`)을 저장소 작업 **전에** 쓰고, 지우기 Task는 지우기 직전에 다시 보므로 다른 노드의
배포와 엇갈려도 기반을 지우지 않는다. 백엔드도 막는다(ZFS `destroy`·RBD `snap unprotect`가 자식이 있으면 실패). `base`는
배포 Task만 쓴다(`POST /volumes`의 `base`는 400 `base is set only by deploying from a template`).

**평탄화.** 연결 복제를 기반에서 떼어 독립 볼륨으로 만든다.

```bash
orbivisorctl vm stop web-1
orbivisorctl volume flatten web-1-disk0          # POST /volumes/{key}/flatten → 202 Task volume-flatten
```

- 쓰는 VM이 꺼져 있어야 한다(**409** `vm_running` — 실행 중 평탄화는 아직 없다). 그 VM에 스냅샷·백업·마이그레이션 Task가
  있으면 **409** `vm_busy`, 연결 복제가 아니면 **400** `volume … is not a linked clone`, 풀 여유가 기반 할당량보다 작으면
  **409** `no_capacity`(둘 중 하나라도 모르면 보지 않음). 평탄화 중에는 그 VM을 켜지 않고 기다리며(`VolumeFlattening`),
  핫플러그·마이그레이션은 **409** `vm_busy`(`a disk of VM … is being flattened`)다. Task 시간 제한은 없고 진행률은
  단계만 보인다.
- 저장소 작업이 끝났는데 기록 전에 끊겨도 다시 하면 할 일 없이 성공하고 기록만 고친다.

**다른 기능과.**

- **라이브 마이그레이션**: 공유 풀(NFS·공유 디렉터리·RBD)의 연결 복제는 디스크를 복사하지 않고 양쪽 QEMU가 같은 기반을
  읽는다(사슬 그대로). **로컬 디스크 마이그레이션**은 대상 노드의 새 볼륨으로 사슬 전체를 미러하므로 옮긴 디스크는 독립
  볼륨이다(`base` 지워짐 — 마이그레이션이 곧 평탄화). 템플릿은 옮기지 못한다(노드·풀 옮기기는 나중).
- **로컬 디스크 복제(계획 55)**: 연결 복제 디스크가 있는 VM에 `replication`을 켜면 **400** `volume … is a linked clone of
  …; flatten it before enabling replication` — 먼저 평탄화한다. 템플릿은 복제 대상이 될 수 없다.
- **백업·복원**: 백업은 사슬 전체를 읽으므로 늘 전체 데이터이고, 복원(원격·DR 포함)은 독립 볼륨을 만든다. 템플릿의 백업은
  `template: true`를 그대로 담아 복원하면 템플릿이다(받는 쪽에 `templates`를 모르는 노드가 있으면 표시를 버림). 템플릿과
  연결 복제 디스크가 있는 VM의 백업은 증분 비트맵을 만들지 않는다(꺼진 VM의 비트맵이 기반 qcow2를 바꾸지 않게).
- **HA**: 템플릿은 HA를 가질 수 없다. 공유 풀 연결 복제는 기반도 같은 공유 풀이므로 다른 노드에서 그대로 복구된다. 로컬
  풀 연결 복제의 HA는 복제(계획 55)가 필요하므로 평탄화 뒤에만 된다.
- **스냅샷**: 연결 복제 VM의 스냅샷·되돌리기는 백엔드마다 그대로 되고 복제 볼륨만 바꾼다. 스냅샷에서 복제(`--snapshot`)는
  전체 복사다. 템플릿은 스냅샷이 없다.
- **노드 비우기**: 실행 중 VM만 옮기므로 템플릿은 남는다. 로컬 풀 템플릿의 배포는 그 노드에서 도는 Task라 노드가 꺼져
  있으면 배포하지 못한다.
- **cloud-init·ISO(계획 57)**: 템플릿의 CD는 그대로 복사되고(같은 ISO), 템플릿은 켜지지 않으므로 씨앗을 만들지 않는다.
  배포한 VM은 새 id라 instance-id가 달라 cloud-init이 처음부터 돈다.

**오류 코드.**

| 코드 | 때 |
|---|---|
| 409 `vm_is_template` | 템플릿 켜기·재시작·마이그레이션·복제·스냅샷·CD 넣기·디스크 목록 바꾸기·바로 지우기 |
| 409 `volume_is_template` | 템플릿 디스크 볼륨의 크기 바꾸기·지우기 |
| 409 `template_in_use` | 연결 복제가 남은 템플릿 되돌리기 |
| 409 `volume_has_clones` | 연결 복제의 기반 볼륨 지우기 |
| 409 `cluster_upgrading` | `templates` 기능이 없는 노드가 있을 때 새 칸·새 Task |

웹: 가상 머신 › 템플릿 탭(이름·디스크·풀·연결 복제 수, "배포"·"VM으로 되돌리기"·"삭제"), 배포 창(개수와 `{n}` 미리 보기, 디스크
복제 방식 — 되는 디스크만 연결·모두 연결·전체 복사 — 와 디스크별 가능 여부, 노드·풀, vCPU·메모리, NIC별 네트워크, 디스크
크기, cloud-init, "배포 뒤 켜기"; VM마다 Task 알림), VM 목록 행 메뉴와 VM 상세 동작 메뉴(⋯)의 "템플릿으로 바꾸기"(안 되면 이유)와 "템플릿만 보기"
칩, 볼륨 화면의 "기반" 열과 "평탄화".

**넣지 않은 것(나중).** 실행 중 평탄화(`block-stream`), 노드마다 기반 사본을 두는 "기반 캐시"(로컬 풀의 다른 노드 연결
배포), 템플릿 옮기기(노드·풀), 연결 복제의 연결 복제(사슬 2단 이상), 연결 복제 VM의 로컬 디스크 복제, 서버 쪽 대량
배포와 IP 풀, 배포 때 NIC 더하기·빼기, OVF 템플릿 내보내기, Windows sysprep.

**한계.** RBD는 평탄화한 복제가 평탄화 전에 찍은 스냅샷이 여전히 부모를 가리키므로, 그 스냅샷이 남은 동안 템플릿
되돌리기가 기반 보호를 풀지 못해 실패한다(오류 문구가 안내 — 데이터는 안전, Raft에 없는 관계라 API가 미리 막지 못함).
ZFS 평탄화는 복제 크기만큼 공간이 더 들고, LVM-thin 연결 복제는 thin pool 과할당을 늘린다.

## 이미지 라이브러리

잘 알려진 클라우드 이미지의 목록(카탈로그)에서 고르면 풀에 받아 체크섬을 확인하고, 원하면 **받기 → 템플릿을 한 번에**
만든다(vSphere 콘텐츠 라이브러리와 비슷한 자리, 구독은 없음). 받기는 [설치 매체와 cloud-init](#설치-매체와-cloud-init)의
`volume-download` Task이므로 같은 규칙(받는 노드가 URL에 닿아야 함, 사설 주소 거절, 프록시, URL 가림)을 따르고,
`install-media`와 `templates` 기능이 모두 있어야 한다.

```bash
orbivisorctl image list --arch x86_64      # ID ARCH OS VERSION FIRMWARE SOURCE VOLUMES
orbivisorctl image get ubuntu-24.04 --pool nas                     # 받기만(볼륨 ubuntu-24-04)
orbivisorctl image get debian-12 --pool nas --template debian-tmpl --network lan \
  --cpus 2 --memory 2048 --size 20G --ci-user orbi --ci-ssh-key @~/.ssh/id_ed25519.pub
orbivisorctl template deploy debian-tmpl --name 'app-{n}' --count 3 --start
```

- **내장 항목**(Orbivisor 판마다 갱신, 각각 x86_64·aarch64): `ubuntu-24.04`, `ubuntu-22.04`, `debian-12`(genericcloud),
  `rocky-9`, `alma-9`(GenericCloud), `fedora-45`(Cloud Base Generic), `alpine-3.20`(nocloud cloudinit). "release·latest"
  경로와 **같은 디렉터리의 체크섬 파일**(`SHA256SUMS`·`SHA512SUMS`·`CHECKSUM`·`.sha512`)을 써서 고정 해시가 낡지
  않는다. 펌웨어는 Alpine x86_64(BIOS)만 BIOS, 나머지는 UEFI다. 항목은 `(id, 아키텍처)`로 구분한다.
- **관리자 항목**(설정 쓰기 권한, admin): 클러스터 설정(`image_catalog`)에 200개까지 둔다(Raft·백업 그대로).

  ```bash
  orbivisorctl image catalog add corp-ubuntu --os ubuntu --version 24.04 --arch x86_64 \
    --url https://mirror.corp/ubuntu/noble-server-cloudimg-amd64.img \
    --checksum-url https://mirror.corp/ubuntu/SHA256SUMS --default-user ubuntu --min-size 3584M
  orbivisorctl image catalog remove corp-ubuntu --arch x86_64     # --arch 없으면 그 id의 모든 아키텍처
  ```

  체크섬은 `--sha256`·`--sha512`·`--checksum-url` 가운데 꼭 하나다. 내장 id는 쓸 수 없고(400), 같은 `(id, arch)`는
  **409** `already_exists`, URL은 `http`·`https`만이고 사용자 정보(`user:password@`)는 400이다. id는 `.`을 `-`로 바꾸면
  리소스 이름이 되는 57자까지다. 관리자가 아닌 사용자에게는 `GET /image-catalog`·`GET /settings`가 URL의 쿼리 문자열을
  가린다. 항목을 지워도 받은 볼륨과 템플릿은 그대로다.
- **받기**: 볼륨 이름은 기본으로 id의 `.`을 `-`로(aarch64면 `-arm64`를 붙임, 예 `ubuntu-24-04`), 아키텍처는 기본으로
  풀에서 받는 노드의 것이다. 진행률 단계는 `checksum` → `downloading` → `verifying` → `importing`(→ `template`)이다.
  `image list`의 VOLUMES는 같은 URL에서 이미 받은 준비된 볼륨이다.
- **받기 → 템플릿**: `--template NAME --network NET`(+ `--cpus` 기본 2, `--memory` 기본 2048, `--size`는 이미지보다 클
  때만 늘림, `--ci-user`·`--ci-ssh-key`·`--ci-password-*`)이면 가져오기가 끝난 뒤 템플릿 VM을 만든다: 아키텍처로
  `q35`/`virt`, 항목의 펌웨어, `virtio-blk` 디스크 하나(부팅 1), NIC 하나, cloud-init. 호스트 이름·정적 IP는 400이다
  (배포 때 준다). API가 VM 만들기와 같은 검사를 미리 한다(이름 **409** `name_taken`, 아키텍처가 맞는 노드가 없으면 **400**
  `machine_arch_mismatch`). 템플릿 단계가 실패하면(그새 이름이 쓰였거나 opld가 재시작) **받은 볼륨은 지우지 않고**
  `…; volume "ubuntu-24-04" is ready; create the template from it with vm create and template convert`로 알린다.
  가져오기 전 실패(체크섬 등)는 볼륨을 지운다. 웹은 가상 머신 › 이미지·ISO › 이미지 라이브러리 보기(아키텍처 거름, 항목별 "받기"·"받은 뒤 템플릿
  만들기", 진행률, 관리자 항목 더하기·지우기).

**체크섬 확인.** `volume download`·`iso download`(`POST /volumes/download`)에도 `--sha512 HEX`와
`--checksum-url URL`이 생겼다(`--sha256`과 셋 중 하나, 없으면 확인하지 않음).

```bash
orbivisorctl volume download noble --pool nas --source-format qcow2 \
  --url https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img \
  --checksum-url https://cloud-images.ubuntu.com/releases/24.04/release/SHA256SUMS
```

- `checksum_url`이면 Task가 먼저 그 파일을 이미지와 같은 클라이언트·SSRF 규칙으로 1 MiB까지 받아 이미지 URL의 마지막
  경로 조각(파일 이름)의 줄을 찾는다: GNU `해시  이름`·`해시 *이름`, BSD `SHA256 (이름) = 해시`, Fedora·Rocky의
  clearsign 머리·꼬리, Alpine의 해시 하나뿐인 파일. 64자는 SHA-256, 128자는 SHA-512다. 줄이 없으면 `checksum file … has
  no entry for …`, 다르면 `checksum mismatch: expected … (from …), got …`로 실패하고 볼륨을 지운다.
- 보증 범위: 손상·잘린 받기·미러 오류는 잡지만 배포처 자체의 변조는 못 잡는다(GPG 서명 확인은 나중). 배포처가 이미지와
  SUMS를 바꾸는 사이에 받으면 불일치로 실패한다 — 다시 받으면 된다.

**링크 확인.** `.github/workflows/catalog.yml`이 주 1회(와 수동) 내장 항목마다 이미지 URL이 답하는지와 체크섬 파일에 그
이미지의 줄이 있는지 본다(받지는 않음, `cargo test -p oplctl --features catalog-it --test catalog`). 바깥 서버 사정으로
푸시 CI를 깨지 않게 따로 돈다. 실패하면 `BUILTIN`을 고친다.

**넣지 않은 것(나중).** 카탈로그 원격 피드·구독, 원격 클러스터 사이 템플릿 동기화, 체크섬 파일의 GPG 서명 확인.

## 이미지 업로드와 머신 종류

- **업로드 사전 확인.** 웹과 `orbivisorctl volume import`는 파일을 보내기 전에 받을 수 있는지 먼저 묻는다
  (`POST /api/v1/volumes/import/check`): 권한, 이름, 풀이 있는지, 풀이 접속한 노드에서 준비됐는지, 같은 이름의 볼륨이
  있는지, 파일 크기 + 1 GiB의 빈 공간이 있는지. 거절되면 파일을 보내지 않고 이유를 보인다.
- **처리 대기.** 웹에서 파일을 다 보낸 뒤 서버가 2분 동안 응답하지 않으면 "처리 중" 알림이 "<이름>: 서버
  응답이 없습니다. 볼륨 목록에서 결과를 확인하세요."로 바뀌고 닫을 수 있다. 업로드는 끊지 않고 서버에서 계속된다.
  나중에 응답이 오면 평소처럼 결과를 알린다.
- **ISO 이미지·URL에서 받기.** ISO 올리기(`content=iso`)와 URL에서 이미지 받기는 "설치 매체와 cloud-init"에 있다.
- **머신 기본값.** 노드는 시작할 때 자기 아키텍처(x86_64·aarch64)를 등록한다(노드 목록의 "아키텍처" 열).
  머신 종류를 정하지 않으면 웹과 `orbivisorctl vm create`는 지정한 노드의 아키텍처를, 노드를 정하지 않았으면 클러스터
  노드가 모두 같은 아키텍처일 때 그것을 쓴다(x86_64 → `q35`, aarch64 → `virt`). 아키텍처가 섞인 클러스터에서
  노드를 정하지 않으면 orbivisorctl은 `--machine q35` 또는 `--machine virt`를 달라는 오류를 내고, 웹은 `q35`를 고른다.
- **배치.** 자동 배치는 머신 종류와 아키텍처가 같은 노드만 고른다. 지정한 노드와 머신이 맞지 않으면 **400**
  `machine_arch_mismatch`다(VM을 시작할 때도 확인한다). 버전이 섞인 클러스터는 지원하지 않으므로 모든 노드를 함께
  올린다. 새 orbivisord로 다시 시작해 아키텍처를 등록하기 전의 노드는 자동 배치 후보가 아니다(직접 지정하면 쓸 수 있다).

## vTPM과 Secure Boot

VM에 **TPM 2.0**(VM마다 swtpm 하나)과 **Secure Boot**(Microsoft 키를 넣은 UEFI 변수)를 단다 — Windows 11, BitLocker,
LUKS의 TPM 봉인, 서명된 부트로더만 켜기에 쓴다(Proxmox VE의 `tpmstate0`·`pre-enrolled-keys`, vSphere의 vTPM·Secure Boot와
같은 자리). 모든 노드가 이 버전이어야 한다(노드가 알리는 기능 `vtpm` — 아니면 `tpm`·`secure_boot`가 든 VM 스펙 쓰기,
`vm-firmware-reset` Task, TPM 상태가 든 백업 받기·복원은 **409** `cluster_upgrading`). 두 칸이 없는 VM의 JSON과 QEMU
명령줄은 그대로다. 설계는 [vTPM·Secure Boot·게스트 에이전트 설계](../superpowers/specs/2026-11-18-vtpm-secure-boot-guest-agent-design.md).

**필요한 것.**

- **UEFI**: `tpm`·`secure_boot`는 `firmware: uefi` VM만 쓸 수 있다(**400** `tpm needs UEFI firmware`·`secure_boot needs
  UEFI firmware`). 템플릿도 `tpm`을 가질 수 있다(배포한 VM은 새 TPM — 아래).
- **swtpm**: 노드에 `swtpm`과 `swtpm-tools`(`swtpm_setup`)가 있어야 TPM VM을 켠다. 패키지는 둘을 `Recommends`로만 둔다
  (TPM을 쓰지 않는 노드도 있다). 다른 경로면 `[hypervisor] swtpm`·`swtpm_setup`.
- **Secure Boot 펌웨어**: QEMU 펌웨어 descriptor(`/etc/qemu/firmware`, `/usr/share/qemu/firmware`)에서 `secure-boot`과
  `enrolled-keys`가 있는(x86_64는 `requires-smm`도) UEFI flash 펌웨어를 찾는다 — Debian·Ubuntu `ovmf`면
  `OVMF_CODE_4M.secboot.fd`·`OVMF_VARS_4M.ms.fd`, aarch64는 배포판의 AAVMF descriptor에 `enrolled-keys`가 있을 때만이다.
  Homebrew QEMU의 secure descriptor에는 `enrolled-keys`가 없으므로 macOS 노드는 Secure Boot를 못 한다(swtpm도 없어 TPM도
  없다). 다른 파일을 쓰려면 `[hypervisor] secure_boot_code`·`secure_boot_vars`(둘 다 주거나 둘 다 생략). 보통 UEFI VM의
  `uefi_code`·`uefi_vars`는 바뀌지 않는다.
- **노드 능력**: orbivisord가 시작할 때 노드 기록에 `firmware_caps { tpm, secure_boot }`를 쓴다. `tpm`은 swtpm이 TPM 2.0을
  알리고(`swtpm socket --print-capabilities`), QEMU에 `-tpmdev emulator`가 있고, 실제 상태·실행 디렉터리에서 swtpm을 VM과
  같은 인자로 띄워 볼 수 있을 때다. 아니면 이유를 경고 `TPM is off on this node: …`로 남긴다. `secure_boot`는 찾은
  펌웨어 파일이 있을 때다. 스케줄러·HA 대상·균형 조정기·마이그레이션 대상은 능력 없는 노드를 거른다(`node "x" has no
  swtpm for the VM's TPM (install swtpm and swtpm-tools)`, `node "x" has no Secure Boot firmware for the VM (install ovmf
  with Secure Boot keys)`). 능력을 알리지 않은 노드(옛 노드)는 능력 없음이다. `node list`의 `TPM`·`SB` 열, `node get`
  stderr의 `firmware: tpm yes, secure boot no`, 웹 노드 목록의 "펌웨어" 열로 본다.
- **AppArmor**: Ubuntu·Debian의 swtpm은 AppArmor 프로필(`/usr/bin/swtpm`)에 묶여 있다. 패키지는 기본 경로(`data_dir =
  /var/lib/opl`, `run_dir = /run/opl`)의 규칙 `/etc/apparmor.d/abstractions/orbivisor-swtpm`(conffile)을 싣고, postinst가
  `/etc/apparmor.d/local/usr.bin.swtpm`에 `include <abstractions/orbivisor-swtpm>`을 한 번 더한 뒤 프로필을 다시 읽힌다
  (purge는 그 줄만 뺀다). **`data_dir`·`run_dir`을 바꿨으면** 그 경로로 같은 규칙을 local 파일에 더하고 다시 읽힌 뒤
  orbivisord를 다시 시작한다. 막히면 VM 켜기가 실패하는 대신 노드가 `TPM no`를 알린다(위 경고).

  ```
  # /etc/apparmor.d/local/usr.bin.swtpm (data_dir = /srv/opl, run_dir = /run/opl일 때)
  /srv/opl/vm/*/tpm/ rw,
  /srv/opl/vm/*/tpm/** rwk,
  /run/opl/vm/*/ r,
  /run/opl/vm/*/tpm.sock rw,
  /run/opl/vm/*/swtpm.pid rw,
  /run/opl/vm/*/swtpm.log rw,
  ```

  ```bash
  sudo apparmor_parser -r /etc/apparmor.d/usr.bin.swtpm && sudo systemctl restart orbivisord
  ```

**켜고 끄기.**

```bash
orbivisorctl vm create win --firmware uefi --machine q35 --tpm on --secure-boot on --disk win-disk0 --network lan
orbivisorctl vm set web --tpm on                   # 실행 중이면 다음 재시작 때 붙는다(보류 tpm: off -> on)
orbivisorctl vm set web --tpm off                  # TPM 장치만 뗀다. 상태 파일은 남아 다시 켜면 같은 TPM이다
orbivisorctl vm get win                            # stderr: tpm: on (state on node node1, copies: node2) / secure boot: on
```

- API는 VM 스펙의 `tpm: true`·`secure_boot: true`다(`POST /vms`, `PUT /vms/{key}`). 끄면 칸을 뺀다.
- **TPM**: q35는 `tpm-crb`, virt는 `tpm-tis-device`이고 명령줄 끝에 붙어 디스크·NIC의 PCI 주소는 바뀌지 않는다. 실행 중에
  더하거나 빼면 [VM 무중단 유형 변경](#vm-무중단-유형-변경)의 보류(`tpm`, 재시작)다. 상태를 지우는 길은 초기화와 VM 지우기
  뿐이다.
- **Secure Boot**: 꺼진 VM에서만 바꾼다(**409** `vm_running` — 펌웨어와 SMM이 바뀐다). x86_64 q35는 `smm=on`과 secure
  pflash를 쓴다. 처음 켤 때 MS 키를 넣은 변수 템플릿에서 변수를 만든다. **이미 UEFI 변수가 있는 VM**(한 번 켠 VM,
  `status.firmware_node`가 있음)은 변수가 키 없이(또는 키와 함께) 만들어졌으므로 `PUT`만으로 바꾸면 **409**
  `firmware_reset_required`(`VM "win" has UEFI variables made for Secure Boot off; reset them together with the change (vm
  set win --secure-boot on --reset-uefi-vars, boot entries are lost) …`)다. 변수 초기화와 함께 바꾼다(아래). 몰래
  초기화하지 않는다.

**Windows 11 미리 채우기.** 스펙에 OS 종류 칸은 없고 클라이언트가 값을 채운다.

```bash
orbivisorctl vm create win11 --preset windows11 --disk win11-disk0 --cdrom win11.iso --network lan
# note: the windows11 preset uses SATA disks and an e1000e NIC so Windows installs without drivers; …
```

q35·UEFI(다른 `--machine`·`--firmware`는 거절), `tpm`·`secure_boot`·`guest_agent: {}`, 소켓 1 × 코어 2, 4096 MiB(따로
주지 않았을 때), 디스크 버스 SATA, NIC e1000e다 — 드라이버 없이 설치되게 한 것이고, VirtIO 드라이버를 넣은 뒤 VirtIO로
바꾸라고 알린다. 따로 준 값(`--cores`, `--tpm off` 등)이 이긴다. CLI는 볼륨을 만들지 않으므로 `--disk`로 준 볼륨의 버스만
바꾼다. 웹 새 VM 폼의 "Windows 11" 프리셋은 64 GiB 디스크까지 채운다.

**초기화와 Secure Boot 바꾸기.** 꺼진 VM의 UEFI 변수(부팅 항목)나 TPM 상태를 지운다. 다음 켜기에서 새로 만든다.

```bash
orbivisorctl vm firmware reset win --uefi-vars           # POST /vms/{key}/firmware/reset {"uefi_vars": true} → 202 Task
orbivisorctl vm firmware reset win --tpm                 # VM 이름을 쳐서 확인(터미널이 아니면 --yes), 새 TPM
orbivisorctl vm set win --secure-boot on --reset-uefi-vars   # {"uefi_vars": true, "secure_boot": true} 한 요청
```

- Task `vm-firmware-reset`이 VM의 `firmware_epoch`(회차)를 1 올리고 그 노드의 파일을 지운다. TPM을 지우면 `tpm_sha256`·
  사본을 지우고, 다른 온라인 노드에도 옛 TPM 상태를 지우라고 알린다(오프라인 노드의 낡은 파일은 회차가 달라 그 노드에서
  다음에 켜거나 30분마다의 정리에서 지운다). 회차가 다른 노드의 낡은 변수·TPM은 쓰이지 않는다.
- **TPM 초기화는 되돌릴 수 없다**: TPM에 봉인한 비밀(BitLocker 키, LUKS TPM 슬롯, 자격 증명)을 잃고 게스트가 복구 키를
  묻는다. CLI는 그래서 VM 이름을 묻는다(웹 확인 창도 이름을 입력해야 함).
- `secure_boot`(선택, `uefi_vars`가 있어야 함)를 주면 Task가 스펙의 `secure_boot`도 함께 바꾼다. `vm set --secure-boot …
  --reset-uefi-vars`가 이 요청을 보내 Task를 기다린 뒤, 다른 변경이 있을 때만 `PUT`한다(`vm win: UEFI variables reset,
  Secure Boot on`). Task가 끝날 때까지 켜기·재시작·켜는 `PUT`은 **409** `vm_busy`라 낡은 변수로 켜지지 않는다. 변수가 아직
  없는 VM은 `vm set --secure-boot on`(PUT)으로 바로 바뀐다.
- 거절: 아무것도 고르지 않음·`secure_boot`만 줌(**400**), 꺼져 있지 않음(**409** `vm_running`), 켜는 중·끄는 중이거나
  전원·마이그레이션·스냅샷·복제·백업 Task가 있음(**409** `vm_busy`), TPM 상태가 있는 VM에서 `firmware_node`가 오프라인인데
  변수만·TPM만 초기화(**409** `node_unavailable` — 그 노드의 변수가 엇갈리므로 둘 다 초기화하거나 그 노드가 돌아온 뒤).
  Task는 온라인 `firmware_node`에서, 없으면 디스크를 가진 노드에서 돈다. 권한은 `Write` Vm, 감사 설명은 `vm firmware reset
  win (uefi vars, tpm)`(+ `, secure boot on`).

**TPM 상태는 어디에 있고 어떻게 따라다니나.** TPM 상태는 공유 풀이 아니라 **VM을 마지막으로 돌린 노드의 로컬 파일**
`{data_dir}/vm/{id}/tpm/tpm2-00.permall`(디렉터리 0700, 파일 0600)이다. Raft에는 비밀을 두지 않고 해시와 위치만 둔다:
`status.tpm_node`(TPM 상태의 노드), `status.tpm_sha256`(지금 상태의 SHA-256), `status.firmware_copies`(같은 해시의 사본을
가진 다른 노드), `status.firmware_epoch`. UEFI 변수의 `status.firmware_node`([라이브 마이그레이션](#라이브-마이그레이션)의
"UEFI 변수")와 같은 방식이지만 **엄격하다** — 변수를 잃으면 부팅 항목만 잃지만 TPM을 잃으면 봉인한 키를 잃으므로, 지금
TPM 상태를 얻지 못하면 **켜지 않는다**.

- **swtpm**: QEMU처럼 VM마다 떼어 낸 데몬(`--daemon --terminate`, systemd면 `opl-tpm-{id}.scope`)이라 orbivisord를 다시
  시작해도 TPM은 VM과 함께 돈다. QEMU가 끝나면 swtpm도 끝난다. 처음 켤 때 `swtpm_setup`이 EK와 인증서를 만든다.
- **켜기**: 이 노드 파일의 해시가 같거나 이 노드가 `tpm_node`면 그 파일로, 아니면 `tpm_node`에서, 그 노드가 오프라인이거나
  받지 못하면 온라인 사본 노드에서 받아(해시 확인) 켠다. 기록된 상태가 없으면(처음 켜기·복제·TPM 초기화 뒤) 새로 만든다.
  어디서도 얻지 못하면 켜지 않고 `last_error`(`the TPM state of VM "win" is on node "a" (offline) and no other node holds a
  current copy; start it when "a" is back, or reset the TPM with vm firmware reset win --tpm`)를 남긴 채 다시 본다. 꺼진 VM의
  `vm start`는 이 경우 미리 **409** `tpm_state_unavailable`이다.
- **사본**: `tpm_node`의 작업자가 켤 때·멈출 때와 도는 동안 60초마다 해시를 다시 적고, 바뀌었으면 사본을 비운 뒤 사본
  대상에 변수+TPM을 밀어 둔다(노드 사이 연결, 해시 확인). 사본 대상(셋까지)은 [로컬 디스크 복제](ha.md#로컬-디스크-복제와-ha)
  대상이 먼저이고, 그다음은 HA VM이면 TPM 능력이 있는 다른 HA 가능 노드(HA 그룹 우선순위 순)다. **HA도 복제도 없는 VM은
  사본을 두지 않는다.** `vm get`의 `tpm:` 줄과 웹 VM 요약의 "보안" 카드가 상태와 사본 노드를 보인다.
- **HA**: `tpm_node`가 죽은 TPM VM은 **사본 노드에서만** 복구한다. 사본 노드가 없거나 모두 오프라인이면 켜지 않고 위 문구를
  남긴다 — 새 TPM으로 몰래 켜면 게스트가 복구 키를 묻고 멈추므로 사람이 고르게 한다(노드가 돌아오기를 기다리거나 `vm
  firmware reset --tpm`). [HA VM과 자기 펜싱](ha.md#ha-vm과-자기-펜싱)의 "죽은 노드에만 있던 UEFI 변수는 잃는다"는 TPM VM에선
  사본으로 줄어든다.
- **라이브 마이그레이션·로컬 디스크 마이그레이션**: QEMU가 swtpm 상태를 마이그레이션 스트림에 실어 대상 노드의 새 swtpm에
  넣는다(대상에 TPM 능력이 없으면 거른다). Secure Boot의 pflash도 함께 간다. 확정이 `tpm_node`를 대상으로 바꾸고 사본을
  비우며, 대상이 곧 해시를 다시 적고 사본을 밀어 둔다. 원본에 남은 파일은 낡은 사본이다. 노드 비우기도 같은 길이다.
- **사본 지연**: 사본은 마지막 밀어 두기(60초 주기) 시점이다. 그 사이 TPM에 쓴 것(NV 카운터, 그때 만든 키)은 HA 복구나
  사본으로 켜기에서 없어질 수 있다 — 복제 회차의 디스크와 TPM 시점이 다를 수 있는 것과 같다(TPM 영구 상태는 드물게 바뀐다).

**스냅샷·백업·복제.**

- **디스크 스냅샷**: 변수 복사본 옆에 TPM 복사본 `{data_dir}/vm/{id}/snapshots/{snap}.tpm`을 둔다(`snapshot.status.tpm_node`
  가 그 노드). 되돌리기가 함께 돌려놓는다. 다른 노드에서 되돌리면 디스크·전원을 건드리기 전에 `the TPM copy of snapshot
  "a" is on node "n"; revert it while the VM is on that node`로 실패한다. 메모리 스냅샷은 vmstate에 TPM 상태가 들어 있다
  (TPM·Secure Boot·에이전트 채널이 찍을 때와 같아야 되돌린다). 스냅샷을 지우면 다른 노드의 TPM 복사본도 지운다.
- **백업**: 백업 디렉터리에 `tpm2-00.permall`(0600)을 넣고 `tpm_state: true`로 적는다(증분 백업에도 늘 — 작다). TPM
  상태를 얻지 못하면 TPM 없이 담지 않고 실패한다. **복원**은 새 VM에 그 TPM을 두므로 BitLocker VM도 복구 키 없이 켜진다.
  백업 저장소를 읽을 수 있으면 TPM 상태도 읽을 수 있다(암호화는 없음 — [백업과 복원](backup.md#백업과-복원)의 한계). 원격 클러스터
  받기·복원은 `vtpm`이 없는 노드가 있으면 버리지 않고 **409** `cluster_upgrading`이다. Orbivisor 없이 열 때는 백업의
  `tpm2-00.permall`을 빈 디렉터리에 두고 `swtpm socket --tpm2 --tpmstate dir=그디렉터리 --ctrl type=unixio,path=tpm.sock`을
  띄워 QEMU `-chardev socket,id=chrtpm,path=tpm.sock -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-crb,tpmdev=tpm0`.
- **복제(`vm clone`)·템플릿 배포·스냅샷에서 복제**: 새 VM은 **늘 새 TPM**이다(EK와 봉인한 비밀이 겹치지 않게 — 템플릿은
  일반화된 원본이다). UEFI 변수와 Secure Boot 키는 복사한다. 원본에 TPM 상태가 있었으면 Task 경고 `VM "web-1" got a new
  TPM; secrets sealed to the TPM of "tmpl" do not carry over`. TPM까지 옮기려면 백업 → 복원을 쓴다.
- **VM 지우기**: 노드마다 30분마다 `{data_dir}/vm/{id}` 가운데 VM이 없어진 디렉터리(1시간 넘음, 그 VM의 Task 없음, 리더와
  맞춘 클러스터 기록으로 확인)를 지운다. 거쳐 간 노드의 변수·TPM 비밀이 남지 않는다.

**버전이 섞인 클러스터.** `vtpm`을 모르는 노드가 있으면 위 409이고, 옛 노드는 `tpm`·`secure_boot`가 든 VM 스펙을 풀지
못하므로 그런 VM을 먼저 만들지 않는다. 업그레이드한 노드의 기존 UEFI VM은 변수를 그대로 쓴다. 기존 VM에 TPM을 켜면 처음
켤 때 새 TPM을 만든다.

**오류 코드.**

| 코드 | 때 |
|---|---|
| 400 | UEFI가 아닌 VM의 `tpm`·`secure_boot`, 초기화할 것이 없음, `uefi_vars` 없는 `secure_boot` |
| 409 `firmware_reset_required` | UEFI 변수가 있는 VM의 Secure Boot를 `PUT`으로 바꿈(`--reset-uefi-vars`로) |
| 409 `tpm_state_unavailable` | 꺼진 TPM VM을 켜는데 고른 노드가 지금 TPM 상태를 얻을 수 없음 |
| 409 `vm_running` | 실행 중 Secure Boot 바꾸기·초기화 |
| 409 `vm_busy` | 초기화 중 켜기, 다른 Task 중 초기화 |
| 409 `node_unavailable` | TPM VM의 `firmware_node`가 오프라인인데 변수만·TPM만 초기화 |
| 409 `cluster_upgrading` | `vtpm` 기능이 없는 노드가 있을 때 TPM·Secure Boot·초기화·TPM 백업 받기·복원 |

웹: VM 폼의 "보안" 카드(TPM 2.0, Secure Boot — 노드 능력이 없으면 막고 이유, 변수가 있는 VM의 Secure Boot는 초기화 창으로
안내), 새 VM 폼의 "Windows 11" 단추, VM 요약의 "보안" 카드(TPM 상태의 노드·사본, "Secure Boot 켜기/끄기"), "UEFI 변수·TPM
초기화" 확인 창(VM 이름 입력), 노드 목록의 "펌웨어" 열.

**넣지 않은 것(나중).** TPM 1.2, 호스트 TPM 통과(passthrough), 공유 풀 위의 TPM 상태 볼륨, swtpm 상태·백업 암호화, 사용자
Secure Boot 키(PK/KEK/db) 등록·dbx 갱신, 복제 때 TPM 복사, 측정 부팅 증명, Hyper-V enlightenment, VirtIO 드라이버 ISO.

**한계.**

- 노드 능력은 orbivisord가 시작할 때만 본다 — swtpm·ovmf를 나중에 설치하거나 AppArmor 규칙을 고쳤으면 orbivisord를 다시
  시작한다.
- 노드가 이미 정해진 VM(한 번 켠 VM)의 `vm start`는 스케줄러를 거치지 않으므로, 능력 없는 노드에서는 409 대신 켜다
  실패해 `last_error`에 이유를 남긴다.
- macOS 노드는 swtpm이 없어 TPM VM을 켜지 못하고(Homebrew에 swtpm 없음), Secure Boot 펌웨어도 찾지 못한다.
- 사본은 60초 시점이다(위 "사본 지연"). 첫 해시 기록(멈출 때·60초) 전에 노드가 죽으면 다른 노드는 새 TPM으로 켠다.
- 사본은 HA·복제 VM에만 있다 — 그 밖의 TPM VM은 `tpm_node`가 꺼져 있으면 켜지 못한다.

## 게스트 에이전트

VM 안의 **QEMU 게스트 에이전트**(`qemu-guest-agent`)와 이야기해 게스트 정보(OS·호스트 이름·IP·파일 시스템)를 보이고,
에이전트로 끄고, 스냅샷·백업·복제 때 파일 시스템을 얼리고(fs-freeze), 게스트 사용자 비밀번호를 바꾼다(Proxmox VE의 QEMU
Guest Agent, VMware Tools와 같은 자리). 모든 노드가 이 버전이어야 한다(기능 `guest-agent` — 아니면 `guest_agent`가 든 VM
스펙 쓰기와 받기·복원, 비밀번호는 **409** `cluster_upgrading`). 게스트 안에 `qemu-guest-agent`가 돌아야 한다(cloud-init이면
`vendor_data`의 `packages: [qemu-guest-agent]`, Windows는 VirtIO 드라이버 ISO의 guest agent).

```bash
orbivisorctl vm set web --guest-agent on                  # 채널(virtio-serial org.qemu.guest_agent.0)은 다음 재시작 때 붙는다
orbivisorctl vm set web --agent-freeze off                # 스냅샷·백업·복제 때 얼리지 않기(기본 on)
orbivisorctl vm set web --agent-shutdown acpi             # 끌 때 늘 ACPI(기본 agent)
orbivisorctl vm guest web                                 # GET /vms/{key}/guest
orbivisorctl vm guest web --refresh                       # 지금 묻기(?refresh=true)
orbivisorctl vm guest password web --user root            # 터미널이면 두 번 묻기, 아니면 표준 입력의 첫 줄
```

- **스펙**: `guest_agent: { freeze_fs: true, shutdown: "agent" | "acpi" }`, 둘 다 기본값이면 `{}`이다. 끄면(`--guest-agent
  off`) 칸을 뺀다. 채널은 장치라 실행 중에 붙이거나 떼면 보류(`guest_agent`, 재시작)이고 `freeze_fs`·`shutdown`은 바로
  적용된다. HA 게스트 감시(`ha.guest_watch`)만 있는 VM도 같은 채널을 쓰므로 게스트 정보가 보인다.
- **에이전트로 끄기**: `shutdown: agent`이고 이 QEMU에서 에이전트가 답한 적이 있으면 `vm stop`·`vm reboot` 등 노드가
  VM을 끄는 길이 `guest-shutdown`을 보낸다(ACPI 단추를 무시하는 게스트도 꺼진다). 채널이 없거나 답하지 않거나 오류면 곧바로 ACPI로
  넘어가고, 나머지는 지금처럼 `acpi_timeout` 뒤 강제 종료다.

**게스트 정보.** VM을 돌리는 노드가 에이전트에 `guest-info`·`guest-get-osinfo`·`guest-get-host-name`·
`guest-network-get-interfaces`·`guest-get-fsinfo`를 60초마다 물어 **노드 메모리**에 둔다(Raft에 쓰지 않는다 — 지표와 같다).
API는 VM이 도는 노드에서 읽는다(다른 노드면 노드 사이 연결, 3초 제한).

```
os: Ubuntu 24.04.1 LTS (kernel 6.8.0-45-generic)
hostname: web
agent: 8.2.2
collected: 2026-11-18 06:30:05 UTC (42s ago)

NIC   MAC                ADDRESSES
lo    00:00:00:00:00:00  127.0.0.1/8, ::1/128
ens3  52:54:00:12:34:56  10.0.0.21/24, fe80::5054:ff:fe12:3456/64

MOUNT  TYPE  USED    SIZE
/      ext4  2.1GiB  19.2GiB
```

- `GET /vms/{key}/guest[?refresh=true]`(`Read` Vm) → **200** `{ state, info? }`. `state`는 `answered`(`info`가 있음),
  `no_agent`(채널이 없거나 에이전트가 답한 적 없음), `not_running`, `unreachable`(VM의 노드가 답하지 않음), `unsupported`(VM의
  노드가 옛 버전)이고, 에이전트 문제로 오류를 돌려주지 않는다. `info`는 `collected_at`·`agent_version`·`os {id, name, version,
  kernel}`·`hostname`·`interfaces[] {name, mac, addresses[] {ip, prefix}}`·`filesystems[] {mountpoint, fstype, used_bytes,
  total_bytes}`이다. 게스트가 보낸 값은 믿지 않고 자른다(NIC 64, NIC마다 주소 32, 파일 시스템 64, 문자열 256바이트).
- `refresh=true`는 지금 묻는다(VM마다 5초에 한 번, 다른 노드면 25초 제한). 에이전트가 답하지 않으면 앞 정보를 둔다. VM이
  꺼지거나 노드를 떠나면 정보를 지운다.
- 웹: VM 요약의 "게스트 에이전트" 카드와 "게스트" 탭(OS·호스트 이름·IP·파일 시스템 사용량, 수집 시각, "새로 고치기"),
  60초마다 다시 읽는다.

**게스트 비밀번호.** `POST /vms/{key}/guest/password {user, password}`(`Write` Vm — 게스트 root와 같은 무게) → **200**
`{user}`. 에이전트의 `guest-set-user-password`로 바꾼다. user는 1~64바이트(제어 문자 없음), password는 1~256바이트다
(**400**). 비밀번호는 저장하지 않고 로그·감사·오류 문구에 넣지 않는다 — 감사에는 `vm guest password web user root`만
남는다. CLI는 인자로 받지 않는다. VM이 돌지 않으면 **409** `vm_not_running`, 채널이 없거나(`VM "web" has no guest agent
channel; turn on guest_agent and restart the VM`) 에이전트가 답하지 않으면 **409** `guest_agent_unavailable`, VM의 노드에
닿지 않으면 **503** `node_unreachable`. 웹은 "게스트 비밀번호 설정" 창(Windows 게스트면 `Administrator`, 아니면 `root`로
채움).

**fs-freeze.** `freeze_fs`(기본 on)인 실행 중 VM은 디스크 스냅샷(메모리 없음)·백업(전체·증분)·[로컬 디스크
복제](ha.md#로컬-디스크-복제와-ha) 회차가 시점을 고정하는 순간 바로 앞에서 `guest-fsfreeze-freeze`, 바로 뒤에서 `thaw`를
보낸다(전송 내내 얼리지 않는다) — 그 시점은 crash-consistent가 아니라 파일 시스템이 일관된 시점이다(Windows는 VSS).

- 얼리지 못하면(에이전트 없음, 10초 시간 초과, 오류) Task를 실패시키지 않고 경고 `guest agent did not freeze file systems
  (…); the snapshot is crash-consistent`를 남긴다.
- **블록 스냅샷 디스크**(LVM-thin·ZFS·RBD·공유 LVM)의 스냅샷은 VM을 잠깐 멈추고 저장소로 찍으므로 얼리지 않는다(멈춘 게스트의
  에이전트는 답하지 않음) — 경고 `… (lvm-thin disks are snapshotted with the VM paused); the snapshot is crash-consistent`.
- 녹이기는 실패해도 다시 해 보고, orbivisord가 그사이 죽어도 다시 붙을 때 표시 파일(`{run_dir}/vm/{id}/fsfrozen`)을 보고
  녹인다.
- 메모리 스냅샷은 얼리지 않는다(메모리째 담는다).

**`guest-exec`는 없다.** API 권한만으로 게스트 안 root 명령이 되므로 넣지 않았다(`guest-file-*`도). 콘솔과 cloud-init을
쓴다.

**넣지 않은 것(나중).** `guest-exec`(관리자 전용이라도 감사·출력·시간 제한 설계가 따로 필요), VM 목록의 IP 열, `guest-fstrim`.

## VM 무중단 유형 변경

실행 중인 VM의 vCPU·메모리를 늘리거나 줄이고 디스크·NIC를 붙이거나 뗀다(Proxmox VE의 hotplug, VMware의 CPU·메모리
hot-add와 같은 자리). 따로 명령이 있는 것이 아니라 실행 중인 VM의 스펙을 바꾸면(`orbivisorctl vm set`, 웹 VM 편집,
`PUT /api/v1/vms/{key}`) 그 VM의 노드가 **핫플러그 Task**(`vm-hotplug`)로 도는 QEMU를 스펙에 맞춘다. 실행 중에 할 수 없는
것은 스펙에만 저장하고 **다음 재시작 때 적용할 것**(보류)으로 VM에 남는다. 모든 노드가 이 버전이어야 한다(노드가 알리는
기능 `hotplug`, 아래 "업그레이드"). CD-ROM 매체 넣기·빼기도 같은 Task로 실행 중에 된다("설치 매체와 cloud-init").

| 바꾸는 것 | 실행 중에 바로 | 다음 재시작 때 |
|---|---|---|
| vCPU | x86_64 Linux 노드에서 상한(`cpu.max_vcpus`) 안의 `cpu.cores` — 켤 때 수 아래로는 못 줄임 | aarch64 노드(QEMU `virt`에 CPU 핫플러그가 없다), macOS 노드, 소켓 수·CPU 모델·상한 바꾸기 |
| 메모리 | virtio-mem이 있는 Linux 노드에서 상한(`max_memory_mib`) 안, 128 MiB 단위 — 켤 때 크기 아래로는 못 줄임 | macOS 노드, svc01 같은 aarch64 QEMU 8.2(virtio-mem은 aarch64에서 QEMU 9.0부터), 상한 바꾸기 |
| 디스크 | 끝에 붙이기(virtio-blk만)·실행 중 붙인 것 끝에서부터 떼기 — 디스크·NIC 합쳐 8개까지 | 하지 않는다: 켤 때 단 디스크 떼기·가운데 떼기·순서·버스·부팅 순서 바꾸기는 **409** `vm_running`(먼저 끈다) |
| NIC | 끝에 붙이기·실행 중 붙인 것 끝에서부터 떼기, 보안 그룹 바꾸기 | 하지 않는다: 켤 때 단 NIC 떼기·네트워크·모델·MAC·순서 바꾸기는 **409** `vm_running` |
| 펌웨어·머신 | — | 늘 |

```bash
orbivisorctl vm create web --cores 2 --max-vcpus 8 --memory 4096 --max-memory 16384 --disk root --nic lan --start
orbivisorctl vm set web --cores 4 --memory 8192   # Task를 기다려 결과를 알린다
# vm web: hardware updated
# vm web: applied while running
orbivisorctl vm set web --memory 32768
# vm web: hardware updated
# vm web: pending until restart:
#   memory_mib: 8192 -> 32768 (limit)
orbivisorctl vm disk add web data                 # 끝에 붙임(--bus virtio-blk|sata --boot-order N), 꺼진 VM에도 된다
orbivisorctl vm disk remove web data
orbivisorctl vm nic add web lan,sg=web --model virtio   # NETWORK[,sg=A+B] [--mac MAC]
orbivisorctl vm nic remove web 1                  # 순번(0부터)이나 MAC
orbivisorctl vm hotplug web                       # 실패한 변경을 다시 해 본다(actions/hotplug)
orbivisorctl vm get web                           # 표준 출력은 JSON, 하드웨어 줄과 보류는 표준 오류
# hardware: vcpus 4/8, memory 8192/16384 MiB, hotplug ports 1/8 used
# pending:
#   memory_mib: 8192 -> 32768 (limit)
```

`vm set`·`vm disk`·`vm nic`은 `--no-wait`면 Task를 기다리지 않는다(`vm web: hotplug task <ID> started`). Task가 실패하면
그 이유로 끝난다(종료 코드 1). 상한은 `vm set --max-vcpus N | --no-max-vcpus`, `--max-memory MiB | --no-max-memory`로
바꾸고 다음 시작부터 쓴다.

웹은 VM 만들기·편집의 일반 절에 "최대 vCPU"·"최대 메모리 (MiB)"(비우면 없음)가 있고, 실행 중인 VM의 편집은 vCPU 코어·
메모리·상한을 그대로 열어 두며("최대값 안의 vCPU 코어와 메모리는 노드가 지원하면 곧바로…") 디스크·NIC 절은 끝에 붙이기와
실행 중 붙인 마지막 것 떼기만 연다(켤 때 단 장치는 잠금, 핫플러그 없이 켠 VM이면 "한 번 다시 켜야" 안내). 저장하면
"<VM> 하드웨어 바꾸기" Task 알림이 뜬다(보류가 있으면 경고와 함께 끝난다). VM 상세는 머리에 "재시작 필요" 배지, 요약에
vCPU `4 (최대 8)`·메모리 `8 GiB (최대 16 GiB)`·핫플러그 포트, "다음 재시작 때 적용" 목록(항목·지금 값 → 바랄 값·이유,
실패면 그 까닭과 "다시 적용" 단추), 하드웨어 탭의 "실행 중 붙임" 표시를 보인다. 노드 표의 하이퍼바이저 칸에 "핫플러그:
vCPU 예/아니오, 메모리 예/아니오"가 있다. API는 `PUT /api/v1/vms/{key}`(**200** VM, Task를 만들었으면 응답 헤더
`orbivisor-task: <Task id>` — CORS로도 읽힌다), `POST /api/v1/vms/{key}/actions/hotplug`(**202** Task, 운영자 권한),
VM `spec.cpu.max_vcpus`·`spec.max_memory_mib`, `status.hardware`(아래), Node `status.hotplug`다.

- **상한.** `cpu.max_vcpus`는 `sockets × cores` 이상·`sockets`의 배수·256 이하, `max_memory_mib`는 `memory_mib` 이상·512 GiB
  (524288 MiB) 이하이고 있으면 둘 다 128 MiB 배수다(아니면 **400**). 상한을 두면 QEMU를 그 자리(빈 vCPU 슬롯, virtio-mem 장치)와 함께
  켠다. 배치·용량은 **지금 값으로** 센다(상한을 예약하지 않는다 — VMware의 hot-add와 같다). 그래서 실행 중 메모리를 바로
  늘릴 때 그 노드의 미할당 메모리가 늘리는 양보다 적으면 **409** `no_capacity`("node X has only N MiB of unallocated memory;
  VM Y needs M MiB more")이고 스펙은 그대로다. HA 용량 예약은 다른 스펙 변경과 같다.
- **vCPU 손잡이는 `cores`.** 상한을 둔 VM은 소켓 `S`개에 소켓마다 `max / S`개 자리로 켜고, 켜진 vCPU는 앞에서부터 찬다.
  실행 중에는 `sockets`를 그대로 두고 `cores`만 바꾼다(소켓을 바꾸면 다음 재시작).
- **게스트 요건.** 늘린 메모리는 게스트의 virtio-mem 드라이버가 받아야 보인다(Linux 5.8 이상, Windows는 virtio-win
  드라이버) — 드라이버가 없어도 QEMU는 요청을 받으므로 Task는 성공하고 기록은 요청한 크기다. 메모리 줄이기, x86 vCPU 떼기,
  q35에서 디스크·NIC 떼기(ACPI PCIe 핫플러그)는 **게스트가 내려 줘야** 끝난다 — 60초 안에 끝나지 않으면 Task가 실패한다
  (아래). QEMU 8.2의 q35는 게스트 OS가 ACPI CPU 핫플러그를 켜기 전에는(OS가 아직 없거나 옛 OS) vCPU 떼기를 곧바로
  거절한다 — vCPU 수는 그대로이고 Task가 실패하며 `failed` 보류("the guest does not support vCPU removal yet; restart the VM
  to apply")가 된다(떼는 중이 아니므로 옮길 수 있다). aarch64 `virt`의 디스크·NIC 떼기는 게스트 없이 곧바로 끝난다.
- **보류.** 실행 중에 적용하지 못한 것은 `status.hardware.pending`에 항목(`vcpus`·`cpu`·`cpu.max_vcpus`·`memory_mib`·
  `max_memory_mib`·`firmware`·`machine`)과 도는 값·바랄 값·이유로 남는다. 이유는 `restart`(켤 때만 정해짐: 토폴로지·모델·
  상한·펌웨어·머신), `unsupported`(이 노드가 못 함: CPU 핫플러그·virtio-mem 없음, 핫플러그 없이 켬), `limit`(켤 때 정한 범위
  밖: 상한 위, 켤 때 값 아래), `failed`(해 봤으나 실패 — `detail`에 까닭)다. 보류는 핫플러그 Task가 끝날 때마다 다시 세고,
  **VM을 다시 켜면**(`vm reboot`, 끄고 켜기, HA 복구) 사라진다. 게스트 안에서 재부팅하면 QEMU가 그대로라 적용되지 않는다.
- **실패와 되돌림.** Task는 첫 실패에서 멈춘다. vCPU·메모리는 닿은 값을 기록하고 그 항목을 `failed` 보류로 남긴다(예:
  "the guest released only 2048 of 4096 MiB within 60s", "the guest did not release vCPU 7 within 60s"). 디스크·NIC를
  붙이지 못하면 붙이던 장치를 걷어 내고(볼륨·tap도) **스펙의 디스크·NIC 목록을 도는 장치로 되돌린다**("disk \"data\" was
  not added: …; the change was undone"). 떼기가 시간(60초)을 넘기면 스펙은 **뗀 그대로** 두고, 장치는 QEMU에 남아 기록의
  `status.hardware.unplugging`(QEMU 장치 id)과 `failed` 보류("the guest has not released disk \"data\" yet; run `vm hotplug`
  again")가 된다 — 게스트가 내려 준 뒤 `vm hotplug`(웹 "다시 적용")가 떼기를 마치고 볼륨을 끈다. 그동안 그 장치는 방화벽
  규칙도 그대로이고(같은 순번에 새 NIC가 스펙에 있어도 tap은 붙은 NIC의 규칙), 그 디스크의 볼륨은 쓰는 중이라 지우기·다른
  VM에 붙이기·크기 늘리기가 **409** `volume_in_use`("volume \"data\" is still being released by VM \"web\"; run `vm hotplug`
  or restart the VM")이며 유휴 볼륨 정리도 끄지 않고, 디스크·NIC 목록을 바꾸는 PUT은 **409** `vm_busy`("the guest is still
  releasing disk \"data\" from VM \"web\"; run `vm hotplug` first" — CPU·메모리만 바꾸기는 된다), VM은 옮길 수 없다(아래).
  게스트가 내려 주지 않은 vCPU도 같다(`unplugging`의 `cpu{i}`). `device_del`을 보낸 뒤 QMP가 끊기거나 답하지 않은 떼기도
  QEMU가 받았을 수 있어 같은 길이다. 앞서 중단된 Task가 붙여 놓고 기록하지 못한 장치는 다음 Task가 그대로 기록한다(장치
  없이 남은 블록 노드·백엔드는 지우고 다시 붙인다). 장치가 빠진 뒤 블록 노드·tap을 정리하지 못한 것은 경고만 남긴다(떼기는 된
  것이다). 실패한 vCPU·메모리는 게스트가 드라이버를 올린 뒤 `vm hotplug`로 다시 해 본다. Task 도중 VM이 다시 켜지거나
  옮겨지면(노드·pid가 바뀜) 아무것도 되돌리지 않고 실패한다("the VM was restarted or moved while its hardware was being
  changed; nothing was undone").
- **노드 능력.** orbivisord가 시작할 때 한 번 정해 `node get`의 `status.hotplug`에 알린다(이 필드를 모르는 옛 리더가 지우면
  노드의 주기 검사가 다시 쓴다): `cpu`는 x86_64 Linux 노드(macOS의 HVF는 아니다), `memory`는
  `qemu-system-* -device help`에 `virtio-mem-pci`가 있는 노드(macOS Homebrew QEMU에는 없다). 디스크·NIC는 이 버전의 모든
  노드가 한다(빈 PCIe 루트 포트 8개 — macOS 노드는 디스크·NIC만).
- **마이그레이션.** 대상 노드는 원본의 **도는 하드웨어**(`status.hardware` — 켤 때 CPU·메모리·펌웨어·머신과 붙인 장치)로
  QEMU를 켜므로 실행 중에 바꾼 VM도 옮길 수 있다. 대상이 그 하드웨어를 못 돌리면(핫플러그 배치로 켰는데 `hotplug`를 모름,
  붙인 vCPU·CPU 상한인데 CPU 핫플러그 없음, virtio-mem인데 없음) **409** `migration_unsupported`("cannot migrate VM \"web\":
  node \"node2\" has no virtio-mem")이고, 균형 조정기·비우기도 그런 노드를 고르지 않는다. 핫플러그 Task가 끝나지 않았거나
  스펙의 디스크·NIC가 도는 장치와 다르면(게스트가 아직 내려 주지 않은 장치) 마이그레이션은 **409** `vm_busy`("…; run `vm
  hotplug` or restart it"), 옮기는 중의 하드웨어 바꾸기도 **409** `vm_busy`다. 원본 노드는 보내기 전에 도는 QEMU의 장치
  (`/machine/peripheral`)를 기록과 견주고 다르면 대상 QEMU를 켜거나 디스크를 복사하기 전에 멈춘다("the running QEMU devices
  do not match the VM's hardware record: …"). 균형 조정기는 하드웨어 변경이 끝나지 않은 VM을 옮기지 않고, 대상의 미할당
  메모리를 도는 메모리(줄이기가 실패했으면 스펙보다 큼)와 견준다.
- **메모리 스냅샷.** 메모리를 담은 스냅샷은 되돌릴 때 같은 장치로 켜야 한다. 그래서 실행 중에 바꾼 하드웨어나 보류가
  있거나 핫플러그 Task가 끝나지 않은 VM은 `memory`를 주지 않은 스냅샷과 예약 스냅샷을 디스크만 찍고, `memory: true`는
  **409** `snapshot_unsupported`("…; restart it to take a snapshot with memory")다. 메모리 스냅샷은 찍을 때의 하드웨어를
  적어 두고 되돌리기는 그 하드웨어로 켠다. `hotplug`를 모르는 노드가 들어와 있는 동안 핫플러그 배치로 도는 VM은 하드웨어를
  적을 수 없어 같은 규칙으로 디스크만 찍는다(`memory: true`는 **409** `snapshot_unsupported`). 웹의 스냅샷 대화상자도 이런
  VM에는 메모리를 묻지 않고 까닭을 적으며, 서버가 스냅샷을 거절하면(`snapshot_unsupported`·`vm_busy` — 화면의 VM이 늦어
  핫플러그 Task를 모를 때 등) 서버의 까닭을 그대로 보인다.
- **다른 작업과.** 스냅샷·되돌리기·복제·백업 Task가 끝나지 않은 동안의 하드웨어 바꾸기(PUT·`vm hotplug`)는 **409**
  `vm_busy`이고, 디스크를 바꾸는 PUT은 그 VM 디스크의 크기 늘리기 Task가 끝나지 않았어도 **409** `vm_busy`다. 이미 만든
  핫플러그 Task는 그 VM의 줄에서 앞의 Task(크기 늘리기 등)가 끝난 뒤 돈다. `vm hotplug`도 메모리를 바로 늘리면 노드의
  미할당 메모리를 본다(**409** `no_capacity`). 켜거나 끄는 중의 하드웨어 바꾸기는 **409** `vm_busy`("VM \"web\" is starting or stopping; change its hardware when it is running or
  stopped")이고, 앞의 핫플러그 Task가 끝나지 않았으면 하드웨어 바꾸기도 **409** `vm_busy`("a hardware change of VM \"web\"
  is still being applied (task …); try again when it finishes")다 — CLI는 `vm_busy`를 1초마다 30번까지 다시 해 본다. 같은
  PUT으로 끄는(`power: stopped`) VM의 하드웨어 변경과, 켜져 있어야 하지만 QEMU가 없는 VM(실패·다시 켜기 대기)의 CPU·메모리
  변경은 Task 없이 다음 시작부터 쓴다(끄면서 바꾸기도 앞의 핫플러그 Task를 기다린다). 그렇게 디스크·NIC를 바꾸며 끈 VM을
  QEMU가 끝나기 전에 다시 켜면(PUT·`vm start`) **409** `vm_busy`("VM \"web\" is stopping with changed devices; start it
  after it stops")다. 켜는 동안 Task 없이 받은
  CPU·메모리 변경은 켠 뒤 보류(`restart`, "changed while the VM was starting; restart it or run `vm hotplug`")로 남는다. 웹은 이런 409의 서버 문구를 그대로 보인다. 실행 중 붙인 디스크는 dirty bitmap이 없어 다음 증분 백업이 그 디스크를 전체로 담는다. 크기 늘리기·백업·
  디스크 통계·방화벽은 그대로다.
- **빈 포트.** 실행 중 붙이는 디스크·NIC는 켤 때 둔 빈 PCIe 루트 포트(`hp0`~`hp7`, 슬롯 0x1e)에 들어간다. 다 쓰면 **409**
  `hotplug_unavailable`("no free hotplug port (8 in use); restart VM \"web\" to add more disks or NICs while it runs")이고,
  다시 켜면 붙인 장치가 보통 장치가 되어 포트가 빈다. **다시 켠 뒤 게스트의 NIC 이름이 바뀔 수 있다**(꺼진 VM에 디스크·NIC를
  더할 때와 같다 — 장치의 PCI 주소가 스펙 순서로 다시 정해진다).
- **업그레이드.** 모든 노드(위트니스 포함)가 `hotplug`를 알아야 상한을 쓸 수 있고(아니면 **409** `cluster_upgrading`, 받은
  백업의 상한은 버린다) 실행 중 적용을 한다. 그 전에는 지금처럼 실행 중 바꾼 CPU·메모리·펌웨어는 다음 시작부터 쓰고 디스크·
  NIC는 **409** `vm_running`이다. 모든 노드를 올리기 전에 켠 VM은 핫플러그 없이 켠 것이라(`vm get`의 `hardware: no hotplug
  (started without it)`) **한 번 다시 켜야** 디스크·NIC를 붙이고 뗄 수 있다. 계획 54 전 버전에서 실행 중에 CPU·메모리·
  펌웨어를 바꾼 VM은 새 노드가 다시 붙을 때 스펙으로 기록(`legacy`)하므로 기록이 도는 QEMU와 다를 수 있다 — 올린 뒤 그런
  VM은 한 번 다시 켜는 것이 좋다. 노드는 켠 VM의 하드웨어를 VM 실행 디렉터리(`hardware.json`)에도 남겨, 기록 없이 다시
  붙을 때 그것을 쓴다. 이 버전의 노드가 켠(하드웨어 기록이 있는) VM은 업그레이드 중에도 방화벽 없는 NIC의 네트워크·모델을
  실행 중에 바꾸는 것이 **409** `vm_running`이다(전에는 스펙만 바뀌어 마이그레이션 대상이 새 네트워크로 켜졌다).
- **한계.** 켤 때 단 디스크·NIC는 실행 중에 뗄 수 없다(지금 VM의 PCI 주소를 바꾸지 않으려고 부팅 장치는 루트 포트에 두지
  않는다). aarch64의 CPU 핫플러그, virtio-scsi 디스크 붙이기는 없다. 떼기 시간 제한(60초)을 넘긴 뒤 게스트가 내려 준 장치는
  다음 핫플러그 Task(`vm hotplug`) 전까지 기록의 `unplugging`에 남고, 그 떼기를 `vm hotplug`로 마저 해야 그 자리에 다시
  붙이거나 디스크·NIC 목록을 바꿀 수 있다. 실행 디렉터리의 `hardware.json`은 켤 때의 모양만이라, 기록 없이 다시 붙은
  노드는 그 뒤 실행 중에 바꾼 것을 모른다. 최대 메모리가 호스트 CPU의 물리 주소 폭을 넘으면
  QEMU가 켜지지 않는다(`maxmem` — 상한을 512 GiB로 둔 까닭).

## CPU 모델과 플래그

게스트가 보는 CPU(CPUID — 모델 이름, 기능 플래그)를 VM마다 정한다(Proxmox VE의 `cpu:`, OpenStack의 `cpu_mode`·
`cpu_model_extra_flags`와 같은 자리, 클러스터 공통 CPU는 vSphere EVC에 해당하는 [클러스터 CPU 기준](cluster.md#클러스터-cpu-기준)).
**모델 목록은 Orbivisor에 박혀 있지 않다.** 노드마다 자기 QEMU에 QMP로 물어(`query-cpu-definitions`, 모델마다 정적 확장,
`-cpu help`의 플래그 이름) 결과를 클러스터에 알리므로, 노드의 QEMU를 올리면 새 모델이 Orbivisor 업데이트 없이 들어온다.
`host`·`max`(플래그 없음) 밖의 모델·플래그는 모든 노드가 이 버전이어야 쓴다(노드가 알리는 기능 `cpu-models`, 아니면
**409** `cluster_upgrading`).

| 모델 | 뜻 | 쓰임 |
|---|---|---|
| `host`(기본) | 노드 CPU 그대로(`-cpu host`, TCG 노드는 `max`) | 성능이 먼저인 VM. 어디로든 옮기지만 CPU가 다른 노드로 옮길 때 경고만 한다(아래) |
| `max` | 이 노드의 QEMU·가속기가 줄 수 있는 모든 것 | 시험용. 뜻이 QEMU 버전마다 바뀐다 |
| `cluster` | 켤 때의 클러스터 기준 모델·플래그. 기준이 없으면 `host`처럼 켠다 | 세대가 섞인 클러스터에서 어디로든 안전하게 옮길 VM |
| `x86-64-v2`·`x86-64-v2-aes`·`x86-64-v3`·`x86-64-v4` | x86-64 psABI 수준 = `qemu64-v1` + 그 수준의 플래그(Proxmox VE 8과 같은 정의) | 벤더가 섞인(Intel + AMD) 클러스터, 배포판이 요구하는 수준(RHEL 9는 v2). x86_64만 |
| QEMU 모델(`Icelake-Server-v7`, `EPYC-Milan-v2`, aarch64 `neoverse-n1` …) | 노드 QEMU가 아는 이름 그대로 | 정해진 CPU 세대를 고정할 VM |

- **버전 고정.** 버전 없는 이름(`Icelake-Server`)은 QEMU 별칭이고 뜻은 QEMU가 머신 유형마다 정한다(지금 `q35`에서는
  가장 옛 `-v1`). 그래서 API가 만들 때·바꿀 때 별칭을 **버전 모델로 바꿔 저장한다**(`Icelake-Server` → `Icelake-Server-v1`,
  `Icelake-Server-noTSX` → `-v2`). 응답과 감사 설명이 고정한 이름을 적는다(`vm create web cpu=Icelake-Server-v1 (from
  Icelake-Server)`). 버전 이름의 정의는 QEMU가 바꾸지 않으므로 한 노드의 QEMU를 먼저 올려도 그 노드로 옮긴 VM이 같은
  CPUID로 켜진다. 노드마다 QEMU 버전이 달라 별칭의 뜻이 갈리면 **400** `cpu_model_ambiguous`(`details.candidates`)이고 버전
  이름을 직접 준다. 대소문자는 가리지 않고 노드가 알린 표기로 저장한다.
- **이름 확인.** 어느 노드도 모르는 이름은 **400** `unknown_cpu_model`(`details.similar`에 비슷한 이름 셋), 다른 아키텍처의
  모델(aarch64 VM에 `x86-64-v3`, x86 VM에 `neoverse-n1`)은 **400** `cpu_model_arch`다. 어느 노드도 켤 수 없는 모델은 꺼진 VM으로
  받지만 켜기는 **409** `no_capacity`에 노드마다의 까닭(`node "n1" cannot run CPU model Broadwell-v1 (missing hle, rtm)`)이다.
- **`enforce`.** QEMU 모델과 `x86-64-v*`는 x86_64에서 `enforce`로 켠다. 노드가 못 주는 기능이 하나라도 있으면 QEMU가
  다른 CPUID로 조용히 켜는 대신 켜지 않는다. 스케줄러가 노드 카탈로그로 먼저 거르므로 보통은 거기까지 가지 않는다.
- **플래그.** `-이름`은 게스트에게 숨기고 `+이름`은 켠다(`cpu.flags: ["-avx512vbmi", "+pcid"]`). 이름은 노드 QEMU가 아는
  CPUID 플래그(`orbivisorctl cpu flags`)이고 소문자, `_`는 `-`, `sse4_1`은 `sse4.1`로 맞춘다. 64개까지, 같은 이름 두 번
  (`+aes`와 `-aes`도)은 **400** `invalid_cpu_flags`, 모르는 이름은 **400** `unknown_cpu_flag`다. `+`는 그 노드가 줄 수 있어야
  켠다(아니면 그 노드는 후보가 아니다). **켜기(`+`)는 루트 `cluster.admin`이 아니면 허용 목록만**이다: `aes`, `amd-no-ssb`,
  `amd-ssbd`, `hv-evmcs`, `hv-tlbflush`, `ibpb`, `md-clear`, `pcid`, `pdpe1gb`, `spec-ctrl`, `ssbd`, `virt-ssbd`(Proxmox VE가
  VM마다 허용하는 목록과 같다 — 보안 완화·1 GiB 페이지·AES·Hyper-V 둘). 그 밖의 `+`는 **403** `cpu_flag_restricted`
  (`details.flags`·`allowed`)다 — 라이브 마이그레이션을 막는 `invtsc`나 중첩 가상화 `vmx`·`svm`처럼 노드 관리에 닿는 플래그를
  폴더 사용자가 켜지 못하게. 숨기기(`-`)는 누구나 아무 이름이나 된다. 이미 저장된 플래그는 다시 보지 않으므로 관리자가 켠
  플래그가 든 VM도 사용자가 고칠 수 있다(VM 만들기·고치기·복제·템플릿 배포·가져오기·이미지 템플릿이 같다). 관리자가 켠
  `+invtsc`(QEMU가 `tsc-frequency` 없이 마이그레이션을 막음)로 도는 VM은 라이브 마이그레이션할 수 없다: `vm migrate`는 **409**
  `migration_unsupported`이고 비우기·균형 조정·HA 검사·호스트 종료의 옮기기는 그 VM을 건너뛰며 까닭을 적는다(전원을 껐다
  다른 노드에서 켜는 HA 복구는 된다). `cpu flags`의 NOTE와 `/cpu/flags`의 `restricted`·`blocks_migration`이 그 플래그를 알린다.
  모델을 못 쓰는 까닭이 플래그 하나뿐이면 그 플래그를 숨겨서 켤 수 있다
  (`Icelake-Server-v7 -avx512vbmi`). 클러스터 숨김 목록(설정 `cpu.mask`, 아래 따라 하기)은 `host`를 포함한 모든 VM에 걸리고
  VM의 `+`보다 이긴다 — 숨김 목록에 든 이름을 `+`로 저장하면 **400** `cpu_flag_masked`다. QEMU를 차례로 올리는 중이라 어떤
  노드의 QEMU가 숨김 목록의 이름을 모르면 그 노드는 그 이름을 빼고 켠다(그 QEMU는 그 기능을 게스트에 주지 못한다). 뺀 이름은
  켤 때 기록(`cpu_resolved.dropped_mask`)에 남고, 그렇게 켠 `host`·`max` VM은 그 이름을 아는 노드나 카탈로그가 없는 노드로
  라이브 마이그레이션하지 않으며 그런 노드에서 메모리 스냅샷을 되돌리지도 않는다(대상의 `-cpu host`가 숨긴 기능을 준다 — 전원을
  껐다 켠다). 같은 노드의 QEMU를 제자리에서 올린 뒤에도 기록이 그 이름을 기억하므로 같다. VM 자신의 플래그는 빼지 않는다: 그 노드의 QEMU가 모르는 이름이면(`unknown flag x`) 그 노드는 후보가 아니다.
- **전원 껐다 켜기 규칙.** CPUID는 QEMU를 띄울 때 정해지고 실행 중에는 바꿀 수 없다. 실행 중인 VM의 모델·플래그를 바꾸거나
  클러스터 기준·숨김 목록이 바뀌면 VM에 보류 `cpu`(이유 `restart`, `detail`에 바뀐 것과 "power cycle required; a reboot inside
  the guest does not apply it")가 생기고, **Orbivisor가 QEMU를 다시 띄울 때**만 적용된다: `vm reboot`(ACPI로 끄고 새 QEMU로
  켬), `vm stop` + `vm start`, HA 복구. `vm reboot`은 같은 노드에서 다시 켜므로 그 노드가 새 CPU를 켤 수 없으면 끄기 전에
  **409** `no_capacity`(`…; migrate VM "x" to a node that can run its CPU, then power cycle it`)다 — 먼저 켤 수 있는 노드로 옮긴다.
  작업자가 스스로 다시 켜는 때(크래시·호스트 SIGTERM·게스트 감시)는 그 노드가 새 CPU를 켤 수 없으면 쓰던 CPU로 다시 켜고 바뀜을
  보류로 남긴다(꺼진 채 남지 않게, [클러스터 CPU 기준](cluster.md#클러스터-cpu-기준)). **게스트 안의 재부팅**(같은 QEMU 프로세스)과 **라이브 마이그레이션**(대상이 원본의
  CPU 기록을 그대로 씀)으로는 적용되지 않는다. 메모리 스냅샷으로 되돌리기는 스냅샷을 찍을 때의 CPU로 켠다(디스크만
  되돌리면 새로 해석한다). 기준·숨김 목록 때문인 보류는 각 노드의 검사 루프(5초)가 다시 셈해 쓴다. 켤 때 실제로 쓴 것은
  `status.hardware.cpu_resolved`(`{model, flags, enforce, source}`)에 남는다.
- **`host`·`max` VM의 마이그레이션.** 오늘처럼 어디로든 옮긴다. 원본 노드가 주는 플래그 가운데 대상 노드에 없는 것이 있으면
  막지 않고 경고한다(`target node "n3" lacks avx512vbmi, amx-tile that the guest may be using`): `vm migrate`가 먼저
  `warning:`으로 찍고, 웹 마이그레이션 대화상자가 띠로 보이고, 감사 detail에 `cpu-warning: …`이 남는다. 그 밖의 모델은
  대상 노드가 VM의 **켤 때 기록된** CPU를 켤 수 있어야 옮긴다(아니면 **409** `no_capacity`와 까닭). 비우기·균형 조정·HA
  되돌아가기도 같은 확인으로 대상을 고른다.

```bash
orbivisorctl cpu models --usable          # MODEL KIND NODES NOTE — 켤 수 있는 노드가 있는 모델, NODES는 "켤 수 있는 수/전체"
orbivisorctl cpu models --node node3      # 그 노드에서 못 켜는 모델의 빠진 플래그(NOTE: missing …)
orbivisorctl cpu flags | grep avx512      # FLAG SUPPORTED NOTE — 줄 수 있는 노드 수, NOTE는 masked·admin only·blocks migration
orbivisorctl vm create db --cores 4 --memory 8192 --disk root --nic lan --cpu-model Icelake-Server --cpu-flags -avx512vbmi
orbivisorctl vm get db                    # 표준 오류에 CPU 줄(모두 host이고 보류가 없으면 줄 없음)
# cpu: 1x4 Icelake-Server-v1 -avx512vbmi
orbivisorctl vm set db --cpu-flags=-avx512vbmi,+pcid   # 목록 전체를 바꾼다. --no-cpu-flags는 모두 지움
orbivisorctl vm set web --cpu-flags -avx512vbmi        # 실행 중이면 보류로 남는다
# vm web: hardware updated
# vm web: pending until restart:
#   cpu: 1x2 host -> 1x2 host -avx512vbmi (restart: -avx512vbmi; power cycle required; a reboot inside the guest does not apply it)
orbivisorctl vm cpu web                   # spec·resolved·running·pending과 노드마다 NODE CAN-RUN MISSING NOTE(까닭·경고)
orbivisorctl vm reboot web                # 전원 껐다 켜기 — 보류 cpu가 적용된다
```

`template deploy`·`image get --template`·`import run`도 `--cpu-model`·`--cpu-flags`·`--no-cpu-flags`를 받는다(API가 Task를
만들기 전에 확인·고정한다). 템플릿은 VM 스펙이라 모델·플래그가 연결·전체 복제로 그대로 가고, 스냅샷에서 복제(`vm clone`)와
백업 복원도 원본 그대로다(받는 클러스터가 모르는 모델이면 꺼진 VM으로 만들어지고 켜기가 409와 까닭이다). VMware 가져오기는
지금처럼 `host`가 기본이다(VMX의 EVC·`featMask`는 읽지 않는다). 웹은 복제·템플릿 배포 대화상자에 접힌 **"CPU 모델과 플래그"**
(원본 값이 요약에 보이고 바꾼 칸만 보낸다)와 가져오기 실행 화면에 같은 칸이 있고, 클러스터 기준이 있으면(자동·고정)
가져오기는 `cluster`를 미리 고른다.

웹은 VM 만들기·편집의 일반 절에 **"CPU 모델"**(찾기 상자 + 묶음 목록: 기본·클러스터 기준·호환 수준·QEMU 모델(버전), 항목마다
"노드 n/m에서 켤 수 있음", 고르면 못 켜는 노드와 빠진 것)과 **"CPU 플래그"**(칩 입력 — 이름을 넣고 Enter나 "숨기기"는 `-`,
"켜기"는 `+`, 칩을 누르면 바뀜, 노드가 모르는 이름은 받지 않음)가 있다. 루트 `cluster.admin`이 아니면 "켜기"는 허용 목록의
이름만 받고, 입력 아래에 켤 수 있는 이름과 까닭을 적으며 자동 완성은 그 이름만 `+이름`으로 제안한다. 켠 `invtsc` 칩에는
"라이브 마이그레이션을 막음"이 붙는다. 별칭은 목록에 없고 별칭 이름으로 찾으면 버전 모델이
나온다. 클러스터 기준이 있으면 새 VM은 `cluster`로 시작한다. VM 상세 요약의 CPU 줄은 스펙과 "지금 … (으)로 실행 중"을,
보류 `cpu`가 있으면 **"전원 껐다 켜기 필요"** 띠와 **"지금 전원 껐다 켜기"** 단추(재시작 확인 대화상자)를 보인다. 마이그레이션
대화상자는 VM의 CPU를 켤 수 없는 노드를 "(VM의 CPU를 켤 수 없음)"으로 막고 까닭을 보인다. 화면 위치는 [웹 화면
둘러보기](web-ui.md#cpu-모델-화면).

API는 VM `spec.cpu.model`(문자열)·`spec.cpu.flags`, `status.hardware.cpu_resolved`, `GET /api/v1/vms/{key}/cpu`(스펙·지금
해석·실행 중·보류, 노드마다 `can_run`·`missing`·`reason`·`warnings`), `GET /api/v1/cpu/models?arch=&node=&usable=`(의사 항목
`host`·`max`·`cluster`(`current`에 지금 기준)·수준과 노드 QEMU 모델), `GET /api/v1/cpu/flags?arch=`다. CPU 칸은 VM 설정 권한
(`vm.config`)이고, `/cpu/models`·`/cpu/flags`는 VM을 만들거나 설정할 수 있는 사용자 누구나 읽되 루트 `cluster.read`가 없으면
노드 이름 없이 "n/m 노드"만 받는다([폴더와 권한](folders.md)).

- **한계.** `host` VM을 CPU가 다른 노드로 옮기는 것은 경고만 하고 막지 않는다. 사용자 정의 모델 객체(Proxmox
  `cpu-models.conf`), `phys-bits`·`hv-vendor-id`·Hyper-V enlightenment, 머신 유형 버전 고정은 없다. aarch64는 QEMU 모델을 TCG
  노드에서만 켜고(KVM·HVF aarch64는 `host`·`max`뿐 — macOS 노드도 같다) `enforce`가 없다. aarch64에서 숨김 이름이 고른 모델에
  없는 속성이면(`neoverse-n1`의 `pauth`) QEMU가 켜지 않는데, aarch64 카탈로그에는 모델별 기능 표가 없어 미리 알지 못한다.
  웹 VM 목록 밖에서(CLI) 보류 VM만 고르는 거름은 없다 — `vm get`의 `cpu running: … (power cycle pending: …)` 줄로 본다.

## 취약점 때문에 CPU 기능 숨기기

CPU 기능 하나 때문에 게스트 소프트웨어가 잘못 동작할 때, 고친 버전이 나올 때까지 그 기능을 게스트에게 숨기는 따라 하기다.
예는 **Java4Shell(CVE-2026-114938)** — JDK 17.0.6 이상이 AVX-512 VBMI가 있는 CPU에서 고르는 Base64 내장 함수(intrinsic)의
버그다. JDK는 시작할 때 CPUID를 보고 내장 함수를 고르므로 게스트에게 `avx512vbmi`를 숨기면 그 코드 경로를 쓰지 않는다.
VM 하나만이면 1·2단계, 클러스터 전체면 1·3단계부터 따라 한다.

1. **노드가 이 기능을 주는지 본다.** 노드 QEMU가 `avx512vbmi`를 주는 노드 수와 게스트가 지금 보는지:

   ```bash
   orbivisorctl cpu flags | grep avx512vbmi
   # avx512vbmi  3/3
   # (게스트 안에서)
   grep -c avx512vbmi /proc/cpuinfo        # vCPU 수만큼(0보다 큼)이면 보이는 것이다
   ```

2. **VM 하나에서 숨기기.** VM 플래그에 `-avx512vbmi`를 더한다. `--cpu-flags`는 목록 전체를 바꾸므로 이미 있는 플래그도 함께
   준다(`vm cpu app1`의 spec 줄). 웹은 VM 편집 › 일반 › "CPU 플래그"에 `avx512vbmi`를 넣고 Enter.

   ```bash
   orbivisorctl vm set app1 --cpu-flags -avx512vbmi
   # vm app1: hardware updated
   # vm app1: pending until restart:
   #   cpu: 1x4 host -> 1x4 host -avx512vbmi (restart: -avx512vbmi; power cycle required; a reboot inside the guest does not apply it)
   ```

3. **클러스터 전체에서 숨기기.** 설정의 숨김 목록 `cpu.mask`에 넣으면 `host`를 포함한 **모든 VM**(앞으로 만들 VM도)에 걸린다.
   이 값도 목록 전체를 바꾸므로 `orbivisorctl cpu baseline`의 `mask:` 줄에 있던 이름을 함께 준다(`cpu.mask=avx512vbmi,amx-tile`).
   루트 `cluster.admin` 권한이 필요하다.

   ```bash
   orbivisorctl settings set cpu.mask=avx512vbmi
   # settings updated
   # 12 running VM(s) keep their CPU until their next power cycle (`vm reboot`; a reboot inside the guest does not apply it)
   ```

   웹은 설정 › 일반 › CPU › **"모든 VM에서 숨기기"**에 `avx512vbmi`를 넣고 **"CPU 설정 저장"**. 저장하면 "실행 중 VM N개는
   전원을 껐다 켤 때까지 지금 CPU 그대로입니다."와 **"목록 보기"** 링크가 뜬다. 노드가 모르는 이름은 **400**
   `unknown_cpu_flag`이고, 감사 로그에 `settings cpu.mask=avx512vbmi`가 남는다. 몇 초 안에 각 노드가 자기 실행 중 VM에 보류
   `cpu`를 쓴다.

4. **보류 VM을 찾는다.** 웹은 VM 목록의 **"CPU 변경 대기"** 보기(주소 `/vms?cpu=pending`, 3단계의 "목록 보기"가 여기로
   간다) — 이름 옆에 "CPU 변경 대기" 배지가 있다. CLI는 VM마다 `vm get`의 CPU 줄로 본다:

   ```bash
   orbivisorctl vm get app2 >/dev/null
   # cpu: 1x2 host
   # cpu running: host (power cycle pending: -avx512vbmi)
   ```

5. **전원을 껐다 켠다.** 웹은 그 보기에서 모두 고르고 일괄 막대의 **"재시작"**(대화상자가 "VM마다 끄고 다시 켭니다(전원 껐다
   켜기). 보류 중인 CPU 변경이 적용됩니다."라고 적는다), VM 하나는 상세의 **"지금 전원 껐다 켜기"**. CLI는 `orbivisorctl vm
   reboot app2`다. 게스트를 내리는 일이므로 서비스는 한 대씩, HA·부하 분산 뒤의 VM부터 돌린다. **게스트 안에서 `reboot`하거나
   VM을 라이브 마이그레이션해도 적용되지 않는다.** 새로 켜는 VM과 HA 복구로 다시 켜지는 VM은 처음부터 숨긴 채로 켜진다.

6. **게스트 안에서 확인한다.**

   ```bash
   grep -c avx512vbmi /proc/cpuinfo        # 0
   ```

   클러스터 쪽에서는 `vm get`에 보류가 없고 VM의 `status.hardware.cpu_resolved.flags`에 `"avx512vbmi": false`가 있다(`vm cpu
   app2`의 running 줄에 `-avx512vbmi`).

7. **JDK를 고친 버전으로 올린다.** 숨기기는 우회일 뿐이다. 고친 JDK로 올린 뒤 숨김을 빼고(`settings set cpu.mask=`, VM은
   `vm set app1 --no-cpu-flags`) 다시 전원을 껐다 켜면 그 기능의 성능을 되찾는다.

- **한계 — CPUID만 숨긴다.** 숨기기는 "이 기능이 있다고 알리지 않기"다. CPU 하드웨어는 그 명령을 여전히 실행할 수 있으므로,
  CPUID를 보지 않고 명령을 바로 쓰는 코드는 막지 못한다. JDK는 CPUID를 보고 내장 함수를 고르므로 이 쓰임새에는 맞다. 다른
  소프트웨어에 쓰기 전에 그것이 CPUID로 기능을 고르는지 확인한다. 게스트 안의 우회(JVM 옵션으로 AVX-512 내장 함수 끄기 등)는
  게스트 쪽 일이고 Orbivisor가 관리하지 않는다.
- 숨김은 모델의 정의보다 이긴다. `Icelake-Server-v7`처럼 그 기능이 정의에 든 모델도 `avx512vbmi=off`로 켜므로 모델은 바꾸지
  않아도 된다. 숨긴 VM도 지금처럼 다른 노드로 옮길 수 있다.

## NUMA·전용 CPU·hugepages

VM의 vCPU가 어느 호스트 CPU에서 돌고 메모리가 어느 호스트 NUMA 노드에 놓이는지를 정한다(OpenStack의 `hw:cpu_policy`·
`hw:numa_nodes`·`hw:mem_page_size`, Proxmox VE의 `numa`·`affinity`·`hugepages`, vSphere의 Latency Sensitivity와 같은 자리).
**아무것도 주지 않은 VM은 지금 그대로다** — 명령줄·`systemd-run` 인자·스펙과 기록 JSON이 바이트까지 같다. 노드 쪽 준비(어느
CPU를 전용으로 둘지, hugepages 예약)는 [호스트 준비: 예약·전용 CPU와 hugepages](cluster.md#호스트-준비-예약전용-cpu와-hugepages)에
있다. 새 칸은 모든 노드가 이 버전이어야 쓴다(노드가 알리는 기능 `numa`, 아니면 **409** `cluster_upgrading`).

| 칸 | 값 | 뜻 |
|---|---|---|
| `cpu.policy` | `shared`(기본)·`dedicated` | 공유는 노드의 공유 CPU에서 다른 VM과 나눠 돈다. 전용은 **vCPU 자리마다 노드의 전용 CPU 하나를 독점**한다 |
| `cpu.thread_policy` | `prefer`(기본)·`isolate`·`require` | 전용 VM만. `prefer`는 코어의 형제 스레드를 먼저 채우고, `isolate`는 vCPU마다 코어 하나(형제는 비워 두고 쓴 것으로 셈), `require`는 vCPU를 형제 짝으로(SMT 호스트만, 소켓당 vCPU 자리가 짝수) |
| `cpu.emulator` | `shared`(기본)·`isolate` | 전용 VM만. QEMU 자신의 스레드(I/O·관리)를 노드의 공유 CPU에 둘지, 전용 CPU 하나를 더 잡아 거기 둘지 |
| `numa.nodes` | `auto`·1~8·`[{"cpus":8,"memory_mib":16384}, …]` | 게스트가 보는 NUMA 노드(vNUMA). 노드는 **소켓 단위**라 개수는 `sockets`를 나눠야 하고, 직접 목록의 `cpus`는 vCPU 자리 수(핫플러그 상한 기준, 소켓당 자리의 배수), `memory_mib`는 **켤 때 나눔**(합 = `memory_mib`; 메모리 상한이 있는 VM은 합 ≤ 메모리이고 남는 것은 다음 켜기에 노드마다 고르게 더함) |
| `numa.memory` | `strict`·`preferred` | 게스트 노드의 메모리를 맞춘 호스트 노드에 묶는다(QEMU `policy=bind`·`preferred`). 주면 공유 VM도 묶는다. `preferred`는 넘치면 커널이 다른 노드에서 주므로 노드 고르기에서 거절 사유가 아니고(덜 찬 호스트 노드를 고를 뿐) 장부의 묶인 메모리에도 세지 않는다 |
| `hugepages` | `2M`·`1G`·`any` | 게스트 메모리를 노드가 예약한 hugepages로 쓴다(memfd hugetlb + 미리 잡기). `any`는 노드에 있는 큰 것부터 |

- **언제 쓰나.** 지연에 민감한 VM(DB·NFV·실시간 처리)은 `dedicated`로 다른 VM과 pCPU를 다투지 않게 한다. 호스트 NUMA 노드보다
  큰 VM(vCPU나 메모리가 한 소켓보다 많음)은 게스트가 NUMA를 알도록 `numa`를 준다. 메모리가 큰 VM은 `hugepages`로 TLB 비용을
  줄인다. 셋은 따로 써도 되고 함께 써도 된다.
- **`auto`와 소켓.** 게스트 노드는 소켓을 나눠 갖는다(노드 i가 앞 소켓부터 차례로). 그래서 **소켓이 하나인 VM은 늘 노드 하나**다 —
  나누려면 `--sockets`를 늘린다. `auto`는 소켓 수의 약수 가운데 노드에 맞는 가장 작은 수를 고른다: 묶는 VM(아래)은 호스트에
  실제로 놓을 수 있는 수, 묶지 않는 VM은 게스트 노드 하나가 가장 작은 호스트 노드(CPU 수·hugepages 밖 메모리)에 들어가는 수다
  (VMware처럼 호스트 노드보다 클 때만 나눈다). 호스트 노드가 하나면 1이다. 메모리는 고르게 나누고 남는 것을 마지막 노드에
  준다 — 노드마다 2 MiB의 배수(1G hugepages면 1024 MiB의 배수)여야 한다(아니면 **400** `invalid_numa`).
- **묶는 VM.** 전용, `hugepages`, `numa.memory` 가운데 하나라도 있으면 "묶는 VM"이다. 게스트 노드마다 **서로 다른** 호스트 NUMA
  노드 하나를 고르고(OpenStack과 같다), 그 호스트 노드의 전용 CPU·hugepages·메모리에서 잡는다. `numa`를 주지 않은 전용·
  hugepages VM은 `auto`로 보고 메모리 묶기는 `strict`다(OpenStack도 이런 VM에 NUMA 토폴로지를 준다). 고르는 순서는 호스트 노드
  사이 거리가 짧은 것 → 맞춘 뒤 남는 것이 적은 것(채우기, 조각을 줄임) → 번호이고, 한 호스트 노드 안에서는 L3 캐시 묶음을 적게
  걸치는 CPU다. 게스트가 보는 노드 사이 거리(ACPI SLIT)는 맞춘 호스트 노드 사이 거리다. 묶는 공유 VM은 CPU를 잡지 않고 vCPU를
  그 호스트 노드의 공유 CPU에 둔다.
- **전용 CPU는 핫플러그 상한까지 잡는다.** `max_vcpus`가 있는 전용 VM은 켤 때 상한만큼 pCPU를 잡는다 — 지금 꺼진 자리도 전용
  CPU를 쓴다. 그래야 실행 중 vCPU를 붙일 때(`vm set --cores`) 새 pCPU를 구하다 실패하는 일이 없다. 붙인 vCPU는 그 자리의
  pCPU에 고정되고(실패하면 `device_del`로 되돌리고 핫플러그 단계가 실패로 남는다), 뺀 자리의 pCPU는 그대로 잡혀 있다.
  hugepages는 부팅 메모리(`memory_mib`)만 미리 잡는다. 메모리 핫플러그(virtio-mem)는 게스트 노드마다 장치 하나씩이고, 2M
  hugepages면 꽂을 때 미리 잡아 모자라면 그 핫플러그가 실패로 남는다. **`1G` + 메모리 핫플러그(`max_memory_mib`)는 400**이다
  (virtio-mem 블록이 2 MiB라서). `any` + 메모리 핫플러그는 2M으로 고른다 — 그 노드가 메모리 핫플러그로 켤 때만이다. QEMU가
  7.1보다 옛것이라 virtio-mem이 꽂을 때 미리 잡지 못하는 노드(`node get`의 `status.hotplug.memory_prealloc`이 없음, 예: Ubuntu
  22.04의 QEMU 6.2)는 hugepages VM을 **메모리 핫플러그 없이** 켜고(상한 = 메모리, 메모리 바꾸기는 보류 `unsupported` "this node's
  QEMU cannot preallocate hugepages for virtio-mem (needs QEMU 7.1)"), 그런 노드로는 hugepages + virtio-mem으로 도는 VM을 옮기지
  않는다. 꽂은 메모리는 부팅 메모리와 같은 호스트 NUMA 노드에 묶이므로 거기에 센다: 묶는 VM의 메모리를 늘릴 때 그 호스트 노드에
  들지 않으면 **409** `no_capacity`("VM "db" cannot grow its memory to 40960 MiB: node "n1" has … MiB of unbound memory on host
  NUMA node 0 …")이고, 마이그레이션 대상은 꽂은 메모리까지 잡는다.
- **직접 목록과 메모리 늘리기.** 노드 목록의 `memory_mib`는 켤 때 나눔이다. 메모리 상한이 있는 VM은 실행 중 메모리를 늘려도
  목록을 고치지 않아도 되고(늘린 것은 virtio-mem이 노드마다 꽂는다) 보류 `numa`가 생기지 않는다. 다음 전원 껐다 켜기 때 남는
  것(메모리 − 목록 합)을 virtio-mem과 같이 2 MiB 단위로 노드마다 고르게(나머지는 노드 0) 더해 켠다. 목록 자체를 바꾸면 보류다.
- **권한과 할당량.** 전용 정책이나 `hugepages`가 있는 VM을 만들거나, 그 칸(정책·스레드·에뮬레이터·hugepages)을 바꾸거나, 그런
  VM을 복제·배포·가져오기·복원하려면 그 폴더에서 권한 **`vm.dedicated`**가 있어야 한다(**403** `dedicated CPUs and hugepages
  need the vm.dedicated privilege`). 내장 `operator`에는 없다 — 노드의 드문 자원이라 관리자가 따로 준다. 이미 전용인 VM의 vCPU
  수·메모리는 `vm.config`로 바꾼다. 그런 VM을 다른 폴더로 옮기는 것(`POST /folders/{id}/members`)도 옮길 폴더에 `vm.dedicated`가
  있어야 한다(`inventory.move`만으로는 안 됨). vNUMA·`numa.memory`만 쓰는 공유 VM은 권한이 들지 않는다. 폴더 할당량 **`dedicated_vcpus`**가
  전용 VM의 `max_vcpus`(+ `emulator isolate`면 1)를 전원과 상관없이 센다([폴더와 권한](folders.md#할당량)). hugepages 메모리는
  이미 `memory_mib` 할당량에 든다.
- **전원 껐다 켜기 규칙.** 모양(게스트 노드·페이지 크기·정책)은 QEMU를 띄울 때 정해지고 실행 중에는 바꿀 수 없다. 실행 중인
  VM의 `cpu.policy`·`thread_policy`·`emulator`·`numa`·`hugepages`를 바꾸면 보류 `numa`(이유 `restart`, `detail`에 바뀐 것 —
  `shared → dedicated`, `hugepages 1G → 2M`, `NUMA 2 nodes → 1 node` … — 과 "power cycle required; a reboot inside the guest
  does not apply it")가 생기고 **Orbivisor가 QEMU를 다시 띄울 때**(`vm reboot`, `vm stop` + `vm start`, HA 복구)만 적용된다.
  게스트 안의 재부팅과 라이브 마이그레이션으로는 적용되지 않는다(CPU 모델의 보류 `cpu`와 같다, [CPU 모델과
  플래그](#cpu-모델과-플래그)). `vm reboot`은 같은 노드에서 다시 켜므로 그 노드에 새 모양의 자리가 없으면 끄기 전에 **409**
  `no_capacity`다. 작업자가 스스로 다시 켜는 때(크래시·게스트 감시)는 새 모양이 안 맞으면 꺼지기 전 모양으로 켜고 보류를 남긴다.
- **켤 때 정하고 기록한다.** 어느 pCPU·어느 호스트 노드는 VM을 켜는 노드가 정해 `status.hardware.numa`(게스트가 보는 모양 —
  노드마다 소켓·메모리, 거리, 페이지, 정책)와 `status.hardware.host_placement`(그 노드에만 맞는 것 — vCPU 자리마다 pCPU,
  에뮬레이터 CPU, 게스트 노드마다 호스트 노드, `isolate`가 비워 둔 형제)에 남긴다. 고정은 `sched_setaffinity`로 vCPU 스레드마다,
  나머지 스레드는 에뮬레이터 집합에 걸고 systemd 스코프의 `AllowedCPUs`가 상한을 쥔다. 누가 `taskset`으로 바꾸면 노드의 검사
  루프가 30초 안에 기록대로 되돌리고 경고 로그를 남긴다. 켜지 못하면(`failed to start: cannot pin vCPU 3 to CPU 7: …`) QEMU를
  끈다. hugepages·전용 VM은 메모리를 켤 때 모두 잡으므로 큰 VM은 켜는 데 시간이 걸린다(시간 제한 30초 + 미리 잡는 GiB마다 1초).
- **노드 고르기.** 스케줄러가 노드마다 전용 CPU·hugepages·NUMA 모양이 맞는지 보고(묶는 VM은 토폴로지가 있는 Linux 노드만),
  모두 안 되면 **409** `no_node`에 노드마다의 까닭을 준다(`node "n1" has 2 and 3 free dedicated CPUs on host NUMA nodes 0 and 1;
  the VM needs 4 on one`, `node "n2" has no 1G hugepages`, `node "n3" cannot pin vCPUs (no topology)`). 두 API 노드가 같은 노드의
  마지막 전용 CPU에 동시에 VM을 놓으면 Raft가 커밋 순서로 하나만 받고(**409** `no_capacity`, `details {node, resource, limit,
  used}`), 켜기는 그 노드를 빼고 한 번 다시 고른다. macOS 노드는 토폴로지가 없어 공유 vNUMA VM만 켠다.
- **마이그레이션·HA·균형 조정.** 라이브 마이그레이션은 게스트 모양·페이지 크기·거리를 **켤 때 기록 그대로** 두고 대상 노드가
  pCPU·호스트 노드만 새로 고른다. 받는 QEMU가 메모리를 모두 미리 잡은 뒤 받으므로 옮기는 중 hugepages가 모자라 게스트가 죽는
  일은 없다(대신 받는 쪽 준비가 길다). 대상에 자리가 없으면 **409** `migration_unsupported`(`node "n2" cannot place the VM: …`)이고
  `vm migrate`는 대상의 까닭을 `warning:`으로 먼저 찍는다. 비우기·호스트 종료의 옮기기도 같은 확인으로 대상을 고르고, 모든 후보가
  안 되면 첫 까닭을 적는다. **HA 복구는 다시 켜기**라 스펙으로 새로 모양을 정한다(`auto`·`any`도 다시 고름). **균형 조정기는 전용
  VM을 옮기지 않는다**(자기 pCPU에서 돌아 다른 VM과 다투지 않으므로 옮겨 얻는 것이 없다) — 공유 vNUMA VM은 지금처럼 옮긴다.
  배치 규칙(계획 50)은 그대로 먼저 거른다. 메모리 스냅샷 되돌리기는 찍을 때의 모양 그대로 켠다.

```bash
# 전용 4 vCPU(코어 독점), 에뮬레이터도 전용 CPU 하나, 1G hugepages, 게스트 NUMA 노드 둘
orbivisorctl vm create db --sockets 2 --cores 2 --memory 16384 --disk db-root --nic lan \
  --cpu-policy dedicated --cpu-threads isolate --emulator-threads isolate --numa 2 --hugepages 1G
orbivisorctl vm start db
orbivisorctl vm get db >/dev/null        # 표준 오류에 NUMA 줄(모두 기본이고 기록·보류가 없으면 줄 없음)
# numa: 2 nodes x (2 vCPU, 8 GiB), dedicated (isolate threads, isolated emulator), hugepages 1G
# numa running: 2 nodes x (2 vCPU, 8 GiB), dedicated, hugepages 1G, memory strict
# placement: host nodes 0,1, vCPU 0-3 -> CPU 4,6,20,22, emulator 8
orbivisorctl vm numa db                  # spec·running·placement·pending과 노드마다 NODE FITS PLAN REASON
# 공유 VM을 호스트 NUMA 노드보다 크게: 소켓 넷, 게스트 노드 자동, 메모리는 선호로 묶기
orbivisorctl vm create big --sockets 4 --cores 8 --memory 262144 --disk big-root --nic lan --numa auto --numa-memory preferred
# 노드마다 직접(vCPU 자리:MiB) — 소켓 둘 × 코어 4면 노드마다 자리 4의 배수
orbivisorctl vm create web --sockets 2 --cores 4 --memory 12288 --disk web-root --nic lan --numa 4:4096,4:8192
orbivisorctl vm set web --hugepages 2M   # 실행 중이면 보류로 남는다
# vm web: hardware updated
# vm web: pending until restart:
#   numa: 2 nodes -> 2 nodes, hugepages 2M, memory strict (restart: hugepages none → 2M, memory none → strict; power cycle required; a reboot inside the guest does not apply it)
orbivisorctl vm reboot web               # 전원 껐다 켜기 — 보류 numa가 적용된다
orbivisorctl vm set web --cpu-policy shared --numa none --hugepages none   # 모두 기본으로(shared는 스레드·에뮬레이터도 지움)
```

`--numa`는 `none`·`auto`·노드 수·`CPUS:MIB,…`, `--numa-memory`만 주면 vNUMA가 없을 때 `auto`로 만든다. `template deploy`·
`vm clone`·`import run`·`image get --template`도 같은 여섯 플래그를 받는다(복제·배포의 `--numa none`·`--hugepages none`은 원본의
값을 지운다). 템플릿은 VM 스펙이라 정책·모양이 복제로 그대로 가고, 백업 복원·원격 복제도 스펙 그대로다(받는 클러스터에 맞는
노드가 없으면 꺼진 VM으로 만들어지고 켜기가 409와 까닭이다). VMware 가져오기는 공유·vNUMA 없음이 기본이다(VMX의 `numa.*`·
Latency Sensitivity는 읽지 않는다).

웹은 VM 만들기·편집 › 일반의 접힌 **"CPU 배치와 NUMA"**에 소켓 수, CPU 정책, (전용이면) SMT 스레드·에뮬레이터 스레드, 게스트 NUMA
노드(없음·자동·개수·노드마다 직접), 호스트 메모리 묶기, Hugepages가 있다. 게스트 노드 개수는 소켓 수의 약수만 보이고, 소켓이
하나면 "소켓을 늘리세요"라고 알린다. `vm.dedicated`가 없으면 전용·hugepages를 새로 고를 수 없고 까닭이 보인다. 아래에 "이 VM의
전용 CPU·hugepages가 들어갈 노드"(어림)와, 실행 중 VM이면 "Orbivisor가 전원을 껐다 켤 때 적용"이 보인다. 복제·배포 대화상자와
가져오기 실행에도 같은 칸이 접혀 있다. VM 상세 › 요약의 **"CPU 배치"**가 설정·켤 때 모양·호스트 NUMA 노드·에뮬레이터 CPU·비워
둔 형제와 펼치는 vCPU 고정 표(vCPU 자리 → 게스트 노드 → 호스트 노드 → 호스트 CPU)를 보이고, 보류 `numa`는 "전원 껐다 켜기
필요" 띠와 VM 목록의 "CPU 변경 대기" 보기에 함께 든다. 마이그레이션 대화상자는 둘 수 없는 노드를 "(VM의 CPU·메모리를 둘 수
없음)"으로 막는다. 화면 위치는 [웹 화면 둘러보기](web-ui.md#numa-화면).

API는 VM `spec.cpu.policy`·`thread_policy`·`emulator`, `spec.numa`, `spec.hugepages`, `status.hardware.numa`·`host_placement`,
`GET /api/v1/vms/{key}/numa`(`spec`·`wanted`·`running`·`placement`·`pending`과 노드마다 `fits`·`reason`·`plan {host_nodes, pcpus,
page}` — 루트 `cluster.read`가 없으면 `placement`·`nodes`가 비어 노드 이름·pCPU 번호가 보이지 않는다)다. 오류는 **400**
`invalid_numa`(소켓을 나누지 않는 노드 수, 합이 맞지 않는 직접 목록, 공유 VM의 스레드·에뮬레이터 정책, `1G` + 메모리 핫플러그,
`require`인데 소켓당 자리가 홀수 …), **403**(`vm.dedicated`), **409** `cluster_upgrading`·`no_node`·`no_capacity`·
`migration_unsupported`·`quota_exceeded`(`dedicated_vcpus`)다. 감사 설명 예: `vm create db cpu.policy=dedicated numa=2
hugepages=1G`.

- **한계.** `mixed` 정책(한 VM 안에 전용·공유 vCPU 섞기)·실시간 vCPU·게스트에 SMT 보이기(게스트 스레드는 늘 1)·iothread와 그
  고정·vhost 일꾼 따로 묶기·장치 통과의 NUMA 친화는 없다. 핫플러그 VM은 켤 때 vCPU가 소켓 0부터 채워지므로 켤 때 vCPU가 앞
  게스트 노드에 몰린다. HA 용량 예약(`ha_capacity`)은 메모리만 보고 전용 CPU를 세지 않는다. 게스트 노드 둘 이상에서 실행 중
  virtio-mem 크기 바꾸기는 명령줄 시험만 했고 실제 KVM으로는 보지 않았다. 웹의 "들어갈 노드"는 노드마다 남은 수만 보는 어림이고
  호스트 노드 나눔은 켤 때 서버가 본다.

## 라이브 마이그레이션

실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(`qm migrate`·vMotion). 원본 노드의 작업(Task)이 지휘하고, 메모리는
두 QEMU가 노드 인증서로 TLS를 맺어 직접 주고받는다. 대상 QEMU는 멈춘 채 받고, 받기를 확인한 뒤 클러스터 기록의
VM 노드를 대상으로 바꾸는 순간 확정된다. 그 뒤에만 대상에서 VM이 이어서 돈다(두 QEMU가 함께 돌지 않는다). 공유 풀의
디스크는 두 노드가 함께 열고, 노드 로컬 풀(공유가 아닌 디렉터리 풀, LVM-thin, ZFS)의 디스크는 대상 노드에 새로 만든
볼륨으로 복사한다(`qm migrate --online --with-local-disks`·Storage vMotion — 아래 "로컬 디스크 복사").
대상 노드에 VM의 복제본이 있으면 마지막 복제 뒤 바뀐 것만 보낸다(아래 "로컬 디스크 복제와 HA").
CD-ROM에 노드 로컬 풀의 ISO가 있으면 옮기지 않는다(409 `cdrom_local` — "설치 매체와 cloud-init").

```bash
orbivisorctl vm migrate web node2        # 끝날 때까지 기다린다: "vm web: migrated to node2"
orbivisorctl vm migrate web node2 --pool fast  # 로컬 디스크를 node2의 풀 fast에 복사한다(없으면 같은 이름의 풀)
orbivisorctl vm list                     # 옮기는 동안 PHASE migrating, NODE "원본 -> 대상"
```

웹은 VM 상세의 "마이그레이션"(실행 중일 때)에서 대상 노드를 고른다(로컬 디스크가 있으면 복사할 디스크 목록과
"대상 풀" — 고른 노드에 같은 이름의 풀이 없으면 대상 풀을 고르게 한다). 클러스터 지도에서 켜진 VM 타일을 노드로 끌거나 `m`으로
고르면 그 노드를 고른 같은 대화상자가 열린다([클러스터 지도 화면](web-ui.md#클러스터-지도-화면)). API는 `POST /api/v1/vms/{key}/migrate`(`{"node": "이름 또는 id", "pool"?: "풀"}`, 운영자 권한, 202와
작업)다.

```toml
# /etc/opl/opld.toml (노드마다, 생략하면 아래 기본값)
[migration]
ports = "60000-60099"     # 받는 쪽 QEMU가 듣는 TCP 포트 범위(둘 다 포함)
bandwidth_mib = 0         # 보내는 대역폭 상한(MiB/s), 0이면 QEMU 기본값. 로컬 디스크 복사에도 디스크 수로 나눠 쓴다
timeout = "30m"           # 이 시간 안에 끝나지 않으면 취소하고 원본을 계속 돌린다("1h30m", "90s")
disk_timeout = "12h"      # 로컬 디스크 복사가 따라잡기(ready)까지의 한도. 메모리 단계는 timeout을 따로 쓴다
```

- **요구 사항.** Linux/KVM 노드끼리만 된다(macOS(HVF) 노드는 원본도 대상도 안 된다). 공유 풀(NFS, `--shared`
  디렉터리 풀, Ceph RBD, 공유 LVM)의 디스크는 그대로 쓰고, 노드 로컬 풀의 디스크는 복사한다(아래). 두 노드의 **QEMU
  버전이 같고 CPU가 같아야** 한다(`-cpu host`. 버전이 다르면 작업이 실패하고, CPU는 확인하지 않으므로 다르면
  실패하거나 게스트가 멈출 수 있다). 모든 노드를 이 기능이 있는 orbivisord로 올린 뒤 쓴다.
- **방화벽.** 노드 사이에 `[migration] ports` 범위를 연다. 받는 쪽은 `cluster_listen`의 IP(기본 모든 주소)에서
  범위의 빈 포트를 고르고(모두 쓰는 중이면 "no free migration port in …"으로 실패한다), 원본은 대상의 `address`로
  접속한다. 로컬 디스크를 복사하면 범위에서 포트를 둘 쓴다(디스크 복사의 NBD와 메모리).
- **TLS.** orbivisord가 시작할 때 노드 인증서·키와 클러스터 CA를 `{data_dir}/migration-tls/`(기본
  `/var/lib/opl/migration-tls`, 0700, 키 0600)에 쓰고 QEMU가 그것으로 서로를 확인한다. 이 기능 전에 클러스터에
  들어온 노드의 인증서는 키 용도에 keyEncipherment가 없어 QEMU 8.2 이하에서 TLS가 실패한다(orbivisord가 시작할 때
  경고한다). 그 노드를 클러스터에서 뺐다가 다시 합류시켜 새 인증서를 받는다.
- **거절(409).** 실행 중이 아님 `vm_not_running`, 로컬 디스크가 있는데 VM에 스냅샷이 있거나 macOS 노드
  `migration_unsupported`, 로컬 디스크가 있는데 디스크 복사를 모르는 노드(위트니스 포함)가 있음 `cluster_upgrading`,
  대상이 오프라인·유지보수 중 `node_unavailable`, 대상에서 풀·브리지가 준비되지 않았거나 로컬 디스크의 대상 풀을 그
  노드에서 쓸 수 없거나("node … cannot use storage pool … for disk …") 남은 메모리가 모자람 `no_capacity`, 다른
  작업(스냅샷·되돌리기·크기 늘리기 등) 중 `vm_busy`. 같은 노드나 로컬 디스크가 없는데 `pool`을 주면 **400**, 없는
  `pool`은 **404**, 아키텍처가 다르면 **400** `machine_arch_mismatch`.
- **실행 중 바꾼 하드웨어.** 대상 QEMU는 원본의 도는 하드웨어(실행 중 붙인 vCPU·메모리·디스크·NIC 포함)로 켠다. 대상이
  그 하드웨어를 못 돌리면 **409** `migration_unsupported`, 핫플러그 Task 중이면 `vm_busy`다 —
  [VM 무중단 유형 변경](#vm-무중단-유형-변경).
- **옮기는 동안.** VM은 `migrating`이고 켜기·끄기·재시작·스냅샷·복제·편집·지우기가 모두 **409** `vm_busy`다.
  콘솔은 열 수 있다(넘어가는 순간 끊겼다가 대상 노드로 다시 연결된다). 취소는 없다. 수렴하지 않으면 `auto-converge`가 게스트를 늦추고 시간 제한이 지나면 실패한다.
- **실패.** 확정 전에 실패하면(대상 거절, 연결·TLS 실패, 시간 제한) 대상 QEMU를 끄고 원본이 계속 돈다. 작업
  오류에 QEMU의 이유가 남는다. 중간에 orbivisord가 죽어도 다시 뜰 때 원본을 되살리거나(확정 전) 남은 원본·받는 QEMU를
  끈다(확정 뒤, 시작 때와 30분마다).
- **UEFI 변수.** QEMU가 변수(pflash)도 함께 옮겨 대상 노드의 변수 파일에 쓴다. 변수 파일은 노드마다
  `{data_dir}/vm/{id}/uefi-vars.fd`에 있고, 클러스터 기록이 지금 파일을 가진 노드(`status.firmware_node`)를 안다.
  멈춘 채 다른 노드로 옮긴 VM(웹 VM 편집이나 API `PUT /api/v1/vms/{key}`의 `node`)은 켤 때 그 노드에서 노드 사이
  연결로 변수를 받아 온다. 스냅샷·복제도 변수를 복사하기 전에 받아 온다. 그 노드가 꺼졌거나 죽었거나 받지 못하면
  경고를 남기고 켜는 노드의 파일(없으면 템플릿)로 켠다 — 그 노드에만 있던 변수(부팅 순서 등)는 잃는다. 옛 노드의
  파일은 지우지 않는다. 모든 노드가 같은 펌웨어 빌드(같은 크기의 UEFI 변수 템플릿, `[hypervisor] uefi_vars`)를
  써야 한다 — 받은 변수 파일의 크기가 켜는 노드의 템플릿과 다르면 버리고 같은 경고 뒤 그 노드의 파일로 켠다.
  한계: 스냅샷의 변수 복사본은 받아 오지 않으므로, 스냅샷에서 복제하는 작업이 스냅샷을 찍은 노드가 아닌 곳에서
  돌면 새 VM은 템플릿으로 켜고, 되돌리기가 그런 노드에서 돌면 변수는 되돌리지 않고 지금 것을 둔다(경고). VM을
  지우면 거쳐 간 노드의 변수 파일은 그 노드의 정리가 지운다(계획 59). TPM VM은 TPM 상태도 함께 옮겨지고, 꺼진 TPM VM은
  지금 TPM 상태를 얻지 못하면 켜지 않는다([vTPM과 Secure Boot](#vtpm과-secure-boot)).
- **공유 LVM.** 옮기는 동안 디스크 LV를 공유 잠금(`lvchange -asy`)으로 바꾸고, 대상이 넘겨받은 뒤 다시 배타로
  바꾼다.
- **로컬 디스크 복사.** 노드 로컬 풀의 디스크는 대상 노드의 같은 풀(그 풀을 대상 노드에서도 쓸 수 있어야 한다 —
  노드마다 같은 VG·thin pool·데이터셋·디렉터리)에, `--pool`(API `pool`)을 주면 대상 노드의 그 풀에 복사한다(공유
  풀도 고를 수 있다 — 로컬에서 공유로 옮기며 마이그레이션). 디스크마다 다른 풀은 고를 수 없다. 공유 풀의 디스크는
  그대로 둔다.
  - **흐름.** 원본 작업이 대상 풀마다 복사할 디스크의 할당된 크기의 합이 대상 노드가 알린 남은 공간 이하인지
    보고("not enough space in storage pool …", 대상 노드가 그 풀 상태를 알리지 않았으면 건너뛴다), 대상 노드가 같은
    볼륨 id로 원본과 같은 가상 크기의 볼륨을 만들어(블록 풀이면 raw, 디렉터리·NFS 풀이면 지금 형식 그대로) 멈춘
    받는 QEMU에서 TLS NBD로 내보낸다. 원본 QEMU가 디스크마다
    `blockdev-mirror`로 복사하고(게스트는 계속 돈다 — 복사 중 바뀐 블록도 따라간다), 모두 따라잡으면 메모리를 보낸다.
    게스트가 멈춘 스위치오버 직전(`pause-before-switchover`)에 미러를 끝내므로 대상 디스크가 원본과 같다. 확정 뒤 볼륨
    기록(풀·형식·노드)을 대상으로 고치고 원본 노드의 볼륨을 지운다. 볼륨의 이름과 id는 그대로다.
  - **진행률.** 작업의 `status.progress`(`stage`가 `copying-disks`면 복사한 바이트와 전체, `migrating-memory`)를 첫
    변화는 곧바로, 그 뒤 바뀌면 5초에 한 번까지 쓴다(작업이 이미 끝났으면 쓰지 않는다). `orbivisorctl vm migrate`는 stderr에
    `vm web: copying disks 37% (14.8 GiB of 40.0 GiB)`·`vm web: migrating memory`를 알리고 24시간까지 기다린다(넘으면
    기다리기만 그만두므로 `orbivisorctl task get <ID>`로 본다). 웹은 VM 상세와 작업 목록에 진행률을 보인다. 취소는 없다.
    복사가 몇 시간 걸릴 수 있고 그동안 VM은 `migrating`이라 끄기·스냅샷·백업이 **409** `vm_busy`다.
  - **실패.** 공간 부족, 대상 풀이 크기를 올려 게스트가 보는 크기가 바뀜("disk … would change size on pool …"), 대상에
    같은 볼륨이 이미 있음("volume … already exists on node … in pool …; is the pool really node-local?" — 그 파일은 지우지
    않는다), 미러 오류, `disk_timeout`("disk copy did not finish within 12h"), 10분 동안 복사량이 늘지 않음("disk copy
    made no progress for 10m"), 그 밖의 마이그레이션 실패는 확정 전이므로 원본이 계속 돌고 대상이 만든 볼륨을 지운다.
    노드마다 `{data_dir}/migration-disks/{작업}.json`에 그 노드가 만들거나 지울 볼륨을 적어 두고, orbivisord가 중간에 죽어도
    다시 뜰 때(그리고 30분마다) 그 기록으로 남은 사본을 지운다(확정 전이면 대상의 사본, 확정 뒤면 원본, 다른 풀로 옮겨
    온 VM의 옛 풀 사본 — 이것은 VM이 도는 동안에도 지운다). 대상 노드는 만든 볼륨만 지운다 — 만들다 멈췄거나 이미
    있던 볼륨은 기록에서만 뺀다. 같은 VM의 옛 사본(같은 볼륨·풀)을 지우지 못해 기록이 남았으면 받지 않는다("an
    earlier copy of disk … in pool … is not cleaned up on this node yet …"). 원본 orbivisord가 복사 중에 죽으면 다시 뜬
    원본이 남은 미러를 치운 뒤 마이그레이션 표시를 지운다 — QEMU에 붙지 못하거나 세 번 잇달아 치우지 못하면 경고
    뒤 표시만 지운다(남은 미러는 대상의 정리가 받는 QEMU를 끄면 끝난다).
  - **스냅샷·비트맵·백업.** 복사는 지금 내용만 옮기므로 로컬 디스크가 있는 VM에 스냅샷이 있으면 옮기지 못한다(먼저
    지운다). 증분 백업의 dirty bitmap은 함께 옮긴다(qcow2 → raw로 바뀌는 디스크의 비트맵은 지우므로 다음 백업은 전체).
    백업 파일은 옮기지 않는다 — 백업 풀이 공유면 대상에서 체인을 이어 가고, 원본 노드의 로컬 백업 풀이면 다음 백업이
    전체다.
  - **버전.** 모든 노드(위트니스 포함)가 디스크 복사를 아는 orbivisord여야 한다(노드 기능 `storage-migration`, 아니면
    **409** `cluster_upgrading`). 공유 풀만의 마이그레이션은 롤링 업그레이드 중에도 된다.
  - **한계.** 취소가 없다(`disk_timeout`이나 10분 정체에서 실패로 끝난다). 대상 풀은 하나다(같은 이름 또는 `pool`).
    백엔드가 크기를 다르게 올리면(4 MiB 배수가 아닌 디스크를 LVM-thin으로) 옮기지 못한다. 공간 확인은 할당된 크기 기준이라
    복사 중 원본이 늘면 대상 풀이 차서 실패할 수 있다. 미러는 게스트 쓰기를 기다리지 않으므로(`background`) 쓰기가
    아주 많은 VM은 스위치오버 때 멈추는 시간이 길어진다. 복사는 클러스터 주소(노드 사이 망)로 간다. 꺼진 VM의 디스크
    옮기기와 한 노드 안에서 디스크만 다른 풀로 옮기기는 없다. 확정 뒤 원본 볼륨 지우기에 실패하면 다음 정리(30분)까지
    원본 노드에 남는다.

## 만들기 전 미리 확인

VM을 만들지 않고 서버가 **만들기(`POST /vms`)와 같은 검사**를 돌려 문제를 모아 준다(계획 71). 웹의 [VM 빌더](web-ui.md#vm-빌더-화면)가 값을
바꿀 때마다(0.6초 쉬면) 이것을 부르고, CLI는 `vm create … --dry-run`이다. 아무것도 쓰지 않고 잠그지 않으며 감사 로그에 적지 않는다.

```bash
orbivisorctl vm create web --memory 4096 --disk web-disk0 --nic lan --node node2 --dry-run
# AT       CODE        MESSAGE
# disks.0  pool_space  warning: storage pool "local" has … bytes free, less than the … bytes of volume "web-disk0"
# ok: node node2, cpu host, machine q35
orbivisorctl -o json vm create web --disk web-disk0 --nic lan --dry-run   # 응답 그대로
```

- **CLI.** `--dry-run`은 `vm create`의 다른 플래그로 짠 같은 본문을 `POST /api/v1/vm-checks`로만 보낸다. 문제가 있으면 `AT  CODE  MESSAGE` 표
  (경고는 `MESSAGE` 앞에 `warning: `), 오류가 없으면 `ok: node …, cpu …, machine …`이다. 오류가 있으면 표 뒤
  `error: the check found N problem(s); the VM would not be created`와 끝 코드 1, 경고만이면 0이다. `--start`와 함께 주면 켠 채(`power: running`)로
  확인해 놓을 노드·용량·hard 배치 규칙을 보고 고를 노드를 `ok: node …`에 보인다(켜지는 않는다). `--start`가 없으면 만들기처럼 꺼진 채라 노드를
  주지 않았으면 `ok: node -`다. 미리 확인 API가 없는 옛 서버에는 `the server does not support --dry-run (POST /vm-checks); upgrade it`,
  아직 올리지 않은 노드가 있는 클러스터(409 `cluster_upgrading`)에는 `the cluster does not support --dry-run until every node is upgraded: …`로 끝난다.
  CLI의 `vm create`는 있는 볼륨만 붙이므로 아래 `volumes`·`rules`는 늘 비어 있다.
- **API.** `POST /api/v1/vm-checks`. 권한은 만들기와 같다 — 그 폴더(spec `folder`)의 `vm.create`.

```json
{
  "name": "web",
  "labels": {},
  "spec": { "cpu": { "sockets": 1, "cores": 2 }, "memory_mib": 4096, "power": "running",
            "disks": [{ "volume": "web-disk0" }], "nics": [{ "network": "lan" }] },
  "volumes": [{ "name": "web-disk0", "spec": { "pool": "local", "size_bytes": 21474836480, "format": "qcow2" } }],
  "rules": [{ "name": "web-zone", "spec": { "kind": "node-affinity", "vms": ["web"], "node_tag": "zone=rack-a" } }]
}
```

  `spec`은 `POST /vms`의 spec 그대로다(`machine`을 빼면 서버가 채운다). `volumes`는 VM보다 먼저 만들 볼륨(`POST /volumes`의 본문 —
  디스크가 이름으로 가리키면 준비된 볼륨으로 얹어 본다), `rules`는 먼저 만들 배치 규칙(`POST /placement-rules`의 본문 — 예: 빌더의 장애
  영역 규칙)이다. 둘 다 생략할 수 있고 각각 16개까지다(넘으면 400). 모르는 칸은 400이다. cloud-init 평문 비밀번호는 검증만 하고
  해시하지 않는다(가린 값 `********`은 만들기처럼 400).
- **응답은 문제가 있어도 200이다**:

```json
{
  "ok": true,
  "problems": [{ "at": "disks.0", "code": "pool_space", "message": "storage pool \"local\" has … bytes free, …", "level": "warn" }],
  "node": "node2",
  "machine": "q35",
  "cpu_model": "x86-64-v3",
  "ips": [[]]
}
```

  `ok`는 `level: "error"`인 문제가 없다는 뜻이다(경고 `warn`은 있어도 된다 — 지금 경고는 만들 볼륨의 노드가 알린 풀 남은 용량이 새 디스크보다
  적은 `pool_space` 하나). `code`는 만들기가 낼 오류 코드(`forbidden`·`quota_exceeded`·`no_node`·`placement_rule`·`name_taken` 등)이고
  `message`는 그 글이다. `node`는 준 노드나 켜진 채 노드 없이 만들 때 스케줄러가 고를 노드의 이름이고(**예상** — 확인과 만들기 사이 부하가
  바뀌면 다르게 고를 수 있고, 꺼진 채 만드는 VM에도 나올 수 있다 — 켤 때 다시 고른다), 준비 단계를 지나지 못했거나 노드가 정해지지 않았거나 루트
  `cluster.read`가 없으면 `null`이다. `machine`(채운 머신)·`cpu_model`(별칭을
  버전 모델로 고정한 것)·`ips`(NIC마다 받을 SDN 주소 — 예상이고 잡아 두지 않음)는 준비 단계를 지났을 때만 채우고, 아니면 `null`·빈 목록이다.
- **`at` 자리**(웹 빌더의 칸이자 폼 오류 열쇠의 꼴): `name`, `compute`, `disks`·`disks.N`, `cdroms`·`cdroms.N`, `nics`·`nics.N`, `cloudInit`,
  `placement`, `protection`, `replication`, `quota`, `spec`(그 밖). `N`은 요청 spec의 `disks`·`cdroms`·`nics` 차례다. 만들 볼륨의 문제는 그 볼륨을
  가리키는 `disks.N`(없으면 `disks`), 만들 규칙의 문제는 `placement`에 붙는다. 같은 `at`·`code`는 하나로 모은다.
- **검사 차례.** ① 이름(형식·같은 이름 VM `name_taken`)과 머신 채우기 ② 폴더의 `vm.create` — 없으면 **403**(폴더를 읽을 수도 없으면 404)으로
  끝난다(만들 수 없는 곳의 규칙·노드를 알려 주지 않음). 폴더를 주지 않고 루트 `vm.create`도 없으면 400 `folder_required` ③ **모으는** 검사(하나가
  실패해도 다음을 봄): 네트워크·볼륨·ISO·복제 풀의 첨부 권한, CPU 플래그·NUMA 권한, 만들 볼륨마다 풀과 `storage.allocate`를 먼저(없는 풀도 거절처럼 여기서
  멈춤), 그다음 `POST /volumes`의 나머지 검사(이름·`name_taken`·크기·형식·풀 노드)와 풀 남은 용량 경고, 만들 규칙마다 배치 규칙 만들기의 검사(`placement-rule` 쓰기 권한·이름·모양·모순 —
  맞는 노드가 없는 영역 규칙은 `rule_conflict`와 아래 `placement_rule`이 함께 나온다). 여기까지 **권한 거절**(403, 또는 읽을 수 없어 404)이
  하나라도 있으면 만들기처럼 거기서 멈춘다: 이름 문제와 거절만 돌려주고 `node`·`machine`·`cpu_model`은 `null`, `ips`는 빈 목록이다(권한 없는
  볼륨·규칙은 아래 준비 단계에 얹지도 않는다 — 볼 수 없는 VM·주소가 드러나지 않게). 거절이 없으면 폴더 할당량(만들 볼륨 크기를 더해)을 함께
  모으고 ④ 만들 볼륨·규칙을 얹은 준비 단계(MAC·SDN 주소·CPU 모델·볼륨·ISO·노드 조건·hard 배치 규칙·스케줄러·HA 용량 — 만들기의 `prepare`와 같은 함수)는 **첫 오류
  하나**만 준다.
- **거절.** 401(로그인 없음), 400(본문을 풀 수 없음·`folder_required`), 403·404(위), **409 `cluster_upgrading`** — 노드 하나라도(위트니스 포함)
  기능 `vm-check`를 알리지 않으면(롤링 업그레이드 중). 웹은 이때 "모든 노드를 올린 뒤에" 안내를 보이고 만들기는 그대로 된다. 질의 꼴
  `POST /vms?dry_run=true`가 아니라 따로 된 경로인 까닭은 옛 노드가 질의를 모르고 진짜 VM을 만들기 때문이다.
- **한계.** 그 순간의 답이다(잡아 두지 않음 — 확인과 만들기 사이 바뀌면 만들기가 알린다). 템플릿 배포(`POST /vms/{key}/clone`)의 미리 확인은
  없다. 준비 단계 문제는 첫 하나다.
