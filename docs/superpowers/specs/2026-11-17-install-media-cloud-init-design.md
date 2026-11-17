# 설치 매체와 cloud-init 설계 (계획 57)

작성일: 2026-11-17
상위 문서: `2026-10-20-opl-node-core-design.md`(VM 스펙·QEMU 명령줄·드라이버·볼륨 가져오기), `2026-10-28-shared-storage-nfs-design.md`(공유 풀), `2026-10-26-snapshots-design.md`, `2026-10-30-live-migration-design.md`, `2026-11-04-backup-design.md`, `2026-11-04-vmware-import-design.md`, `2026-10-23-web-ui-design.md`, `2026-11-11-availability-scale-design.md` §21(구현 계획 54 — 하드웨어 기록·핫플러그 Task·`devices_match`)·§22(구현 계획 55 — 로컬 디스크 복제)
기준 트리: 계획 56 끝(`aff19c9`). 이 문서는 계획 57의 스테이지 1~8(`829e43e`…)이 실제로 정한 것을 적는다 — 스테이지 목록의 "정한 것"과 각 스테이지가 다르게 고른 것을 함께.

## 1. 목적

2026-11-16의 Proxmox VE·VMware 격차 분석의 첫 항목은 "OS를 설치할 표준 경로가 없다"였다. 계획 56까지 Orbivisor VM에 OS를 넣는 길은 이미 설치된 디스크 이미지를 올리거나(볼륨 가져오기) VMware·OVA에서 가져오기뿐이었다. CD-ROM 장치가 없었고(`cdrom`·`ide-cd`·`scsi-cd` grep 0건), 부팅 순서는 디스크의 `boot_order`뿐이었고, cloud-init도 URL에서 이미지를 받는 길도 없었다.

| | Proxmox VE 8 | vSphere 8 | Orbivisor 계획 57 |
|---|---|---|---|
| ISO 보관 | 저장소 내용 종류 `iso`(파일 저장소만) | 데이터스토어의 ISO, 콘텐츠 라이브러리 | 내용 종류 `iso`인 raw 볼륨, 디렉터리·NFS 풀만 (§3) |
| ISO 올리기·받기 | 웹 업로드, "Download from URL"(체크섬) | 데이터스토어 브라우저 업로드, 라이브러리 URL 가져오기 | 업로드(`content=iso`), `POST /volumes/download`(SHA-256, 진행률) (§3·§4) |
| CD/DVD 드라이브 | VM마다 IDE/SATA/SCSI CD, 실행 중 매체 바꾸기 | VM마다 CD/DVD, 실행 중 연결·끊기 | VM마다 4개, q35 `ide-cd`·virt `scsi-cd`, 실행 중 넣기·빼기(트레이 강제) (§5·§6) |
| 부팅 순서 | 장치 목록 `boot: order=…` | BIOS/EFI 부팅 순서, "다음 부팅 때 설정 화면" | 장치마다 `boot_order`, 기본 디스크 → CD (§7) |
| 게스트 초기 설정 | cloud-init 드라이브(사용자·비밀번호·SSH 키·IP·DNS, `cicustom`) | 게스트 사용자 지정(VMware Tools·cloud-init) | NoCloud 씨앗(사용자·비밀번호·SSH 키·호스트 이름·NIC별 IPv4/IPv6·DNS·user-data·vendor-data) (§8~§10) |
| 비밀번호 저장 | 해시(`$5$`) | 사용자 지정 명세에 암호화 | SHA-512 crypt 해시만, 응답은 `********` (§9) |

## 2. 범위

**넣은 것:** ISO 볼륨(올리기·URL 받기·목록·지우기), VM의 CD-ROM 드라이브(빈 드라이브, 실행 중 넣기·빼기), 장치별 부팅 순서, Orbivisor가 만드는 cloud-init NoCloud 씨앗, 일반 디스크 이미지(클라우드 이미지)의 URL 받기(같은 Task), API·CLI·웹.

**넣지 않은 것(§14):** 블록 풀의 ISO, 이어 받기 업로드, 한 번만 CD로 부팅, NIC(PXE) 부팅 순서, ConfigDrive2, 패키지 설치 필드, 클라우드 이미지 템플릿, Windows sysprep 응답 파일(cloudbase-init은 NoCloud를 읽으므로 같은 씨앗이 쓰인다).

**기능 이름 `install-media`**(`opl_types::node::INSTALL_MEDIA`, `FEATURES` 끝). 모든 노드가 알릴 때만 `content: iso` 볼륨 만들기, Task `volume-download`, `cdroms`·`cloud_init`이 비지 않은 VM 스펙 쓰기, `POST /vms/{key}/cdrom`을 받고 아니면 409 `cluster_upgrading` — 옛 노드는 `deny_unknown_fields`라 새 필드가 든 Raft의 VM·볼륨을 풀지 못한다. 빈 `cdroms`·없는 `cloud_init`·`content: disk`는 쓰지 않으므로(`skip_serializing_if`) 기존 VM·볼륨의 JSON과 QEMU 명령줄은 그대로다. 디스크 받기도 새 Task라 같은 기능으로 가린다. `install-media` 검사는 검증보다 먼저다(S6).

## 3. ISO 볼륨

- `VolumeSpec.content: VolumeContent::{Disk(기본), Iso}`(kebab, 디스크면 생략). `ImageFormat`에 `Iso`를 더하지 않는다 — 형식을 보는 곳(스냅샷·백업·마이그레이션·복제·`disk_location`)이 모두 raw로 그대로 다룬다. 파일 이름도 `{id}.raw`.
- **디렉터리·NFS 풀만**(`pool_holds_isos`). 블록 풀 400 `"ISO images can be kept only in dir or nfs pools"`, `format: qcow2` 400 `"ISO volumes are raw"`, 업로드 `source_format`은 raw만 400 `"upload ISO images as raw"`. `POST /volumes`로 만들기 400, 크기 늘리기 400, VM 디스크로 붙이기 400 `"…is an ISO image; attach it as a CD-ROM"`, CD에 디스크 볼륨 400 `"…is not an ISO image"`.
- **내용 검사**: 가져오기 Task가 변환 전에 원본 섹터 16(32 KiB)에서 `CD001`(ISO 9660)이나 `BEA01`(UDF)을 찾는다(`opl_driver::is_iso_image`, 읽는 길이 `ISO_HEADER_LEN` = 32768 + 6). 없으면 `"not an ISO 9660 or UDF image"`로 실패하고 볼륨을 지운다. 크기 상한은 업로드 디렉터리의 여유 공간과 풀 용량뿐이다.
- **여러 VM이 함께 쓴다**: CD 매체는 늘 `read-only=on`이므로 "볼륨은 VM 하나에만" 검사(`prepare`)에서 CD 매체를 뺀다.
- **지우기**: `vm_uses_volume`이 스펙과 `status.hardware`(빼기를 마치지 못한 매체)의 `cdroms[].iso`를 본다 → 409 `volume_in_use` `"volume \"alpine\" is in the CD-ROM drive of VM \"web\" (and N more)"`. 스냅샷 `vm_spec`의 CD가 가리키면 409 `volume_in_snapshot`. 백업은 막지 않는다(§11).
- 목록 API는 따로 없다: `GET /volumes`의 `content`로 CLI(`iso list` — `NAME POOL SIZE NODE USED-BY`)와 웹(ISO 화면)이 거른다.
- 볼륨 이름 `download`는 `import`처럼 예약이다(`/volumes/download` 경로와 겹침 — S5).

## 4. URL에서 받기 (`volume-download`)

- `POST /volumes/download` `VolumeDownloadRequest { name, pool, url, content(기본 disk), sha256?, source_format?(디스크만, 기본 raw), format? }` → 202 Task. 이름 규칙·409 `name_taken`, 풀 없음 400, ISO 규칙, `http`·`https`만(`reqwest::Url`), `sha256`은 64자 16진(대소문자 받음, 소문자로 저장). 노드는 `pool_node`(풀을 쓸 수 있는 온라인 노드, 로컬 풀이면 그 노드).
- `TaskOp::VolumeDownload { url, sha256?, source_format }`, 대상 = 볼륨(`size_bytes: 0`으로 먼저 만들고 끝에 실제 크기). 시간 제한 없는 Task.
- 실행(`opl-api/src/tasks/download.rs`): reqwest(연결 30초 `DOWNLOAD_CONNECT_TIMEOUT`, 읽기 쉼 120초 `DOWNLOAD_READ_TIMEOUT`, 리디렉션 10번, 시스템 CA, User-Agent `orbivisor/{버전}`, 프록시는 reqwest 기본 — `HTTPS_PROXY` 등 환경 변수) → 2xx 아니면 `"GET {url}: HTTP {status}"` → `Content-Length`가 있으면 여유 공간 검사 → `{upload_dir}/{volume-id}.download`에 쓰며 SHA-256과 진행률(`downloading`, 1초마다; 길이를 모르면 `total_bytes` 0; 길이를 알든 모르든 64 MiB마다 남은 길이(모르면 64 MiB)만큼 여유 공간을 다시 — 1차 리뷰) → `sync_all` → `verifying`(다르면 `"checksum mismatch: expected …, got …"`) → `importing`(업로드와 같은 가져오기 함수: 원본 검사·ISO 검사·`qemu-img convert`) → 받은 파일 지움. 실패하면 받은 파일과 볼륨 리소스를 지운다. 시작 때의 업로드 디렉터리 정리는 끝나지 않은 `VolumeDownload`의 `.download`를 남긴다.
- 감사 설명 `volume download NAME from URL (sha256 3f2a…)`. URL의 `user:password@`는 `***@`로, 쿼리 문자열은 `?***`로 가린다(`redact_url`) — 감사, Task 오류 문구(reqwest `without_url`), 그리고 Task를 내보내는 모든 길(`GET /tasks`·`/tasks/{key}`·받기 202 응답·watch 이벤트 — `volumes::public_task`·`redact_task_json`). Raft의 Task는 원래 URL을 그대로 둔다(실행이 쓴다).
- **SSRF**: opld `[download] allow_private`(기본 false)가 꺼져 있으면 루프백·지정 안 함·사설망(10/8·172.16/12·192.168/16·fc00::/7)·링크 로컬(169.254/16 — 클라우드 메타데이터 포함 — ·fe80::/10)·공유 주소(100.64/10)·`0.0.0.0/8`·시험망(198.18/15)·예약(240/4, 브로드캐스트 포함)·멀티캐스트(224/4·ff00::/8)로 가지 않는다. IPv4를 담은 IPv6(사상 `::ffff:a.b.c.d`, 호환 `::a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`)는 담은 IPv4로 본다(`embedded_ipv4` — 2차 리뷰). 이름은 reqwest의 이름 해석기(`PublicResolver`)가 밖의 주소만 돌려주고(모두 막히면 실패), IP로 적은 주소는 처음과 리디렉션 정책에서 단계마다 본다. 실패 문구 `"GET {url}: address {ip} is loopback, link-local or private; set [download] allow_private = true to download from it"`. 환경 변수로 준 프록시의 이름은 사설 주소여도 해석한다(대상 해석은 프록시가 함). 시험은 127.0.0.1 서버를 쓰므로 허용으로 켠다(1차 리뷰).

## 5. CD-ROM 드라이브

- `VmSpec.cdroms: Vec<CdromSpec>`(비면 생략), `CdromSpec { iso?: 볼륨 이름(없으면 빈 드라이브), boot_order? }`(`deny_unknown_fields`). 드라이브 번호 = 순번(`cdrom0`…). `MAX_CDROMS` 4(`TooManyCdroms`).
- **버스**: q35 → 내장 AHCI의 `ide-cd,bus=ide.{n}`(Windows 설치 프로그램이 드라이버 없이 읽음). AHCI 6포트(`Q35_SATA_PORTS`)를 SATA 디스크 → CD-ROM → 씨앗 드라이브 순으로 나누고 넘치면 `TooManySataDevices`(`"q35 has 6 SATA ports: …"`, CD·cloud-init이 있을 때만 검사, cloud-init이 없으면 그 부분 생략). virt → 새 `virtio-scsi-pci,id=scsi0` + `scsi-cd,bus=scsi0.0`(EDK2 ArmVirtQemu가 부팅함). 컨트롤러·드라이브는 CD나 씨앗이 있을 때만, 명령줄 끝(virtio-mem 뒤)에 둔다 — CD를 더해도 디스크·NIC의 PCI 주소가 바뀌지 않는다. CD 없는 VM의 명령줄은 계획 56과 같다.
- QEMU 이름: 드라이브 `cd{i}-dev`, 매체 노드 `cd{i}-file`(file)·`cd{i}`(raw, 둘 다 `read-only=on`), 씨앗 `cidata-dev`·`cidata-file`·`cidata`. 빈 드라이브는 `-blockdev`도 `drive=`도 없다. `disk` 접두가 아니어서 스냅샷(`snapshot_save`의 `devices`)·증분 백업·복제·디스크 미러가 저절로 건너뛴다.
- 계획 54 기록: `RunningHardware.cdroms`(켤 때 스펙 그대로, 매체를 바꾸면 핫플러그 Task가 고침)와 `RunningHardware.cloud_init`(켤 때 씨앗의 instance-id). `devices_match`는 드라이브 수·매체와 씨앗 유무를 견주고(boot_order는 보류일 뿐이라 견주지 않음), `qemu_devices`·드라이버 `tracked_device`에 `cd{i}-dev`·`cidata-dev`(컨트롤러 `scsi0`는 견주지 않음), `hardware_changed`는 `cdroms`·`cloud_init`도 본다.
- 마이그레이션 받는 쪽: 다시 붙기·받기는 CD 목록을 `status.hardware.cdroms`에서 풀고(보통 시작은 스펙에서), `apply_hardware`는 기록에 CD가 있고 디스크·드라이브 수가 맞을 때만 부팅 순서를 기록으로 다시 계산하며 기록에 씨앗이 없으면 씨앗을 뺀다(S4).

## 6. 실행 중 넣기·빼기

- `Hypervisor::change_medium(id, index, Option<&RuntimeMedium>)`(`RuntimeMedium { path }` — 파일 풀만). 실제 드라이버는 먼저 `query-block`으로 같은 파일이 이미 있거나 이미 빈 드라이브면 매체는 그대로 두고, 트레이가 열려 있으면 닫기만 한다(`query_tray_open`). 아니면 `opl_qemu::change_medium`: 새 매체의 노드 둘을 **먼저** 쓰지 않은 이름(켤 때의 `cd{i}`이나 `cd{i}-{n}`)으로 `blockdev-add`(열지 못하면 넣은 매체 그대로) → `blockdev-open-tray {force: true}` → `blockdev-remove-medium` → `blockdev-insert-medium` → 옛 노드(넣었던 매체와 앞서 남은 것) `blockdev-del` → `blockdev-close-tray`. 빼기는 `open-tray(force)` → `remove-medium` → 옛 노드 `blockdev-del` → `close-tray`(빈 드라이브로 남김). 옛 노드를 지우지 못해도 트레이는 닫는다(남은 노드는 다음 바꾸기가 지운다). 넣기가 실패하면 연 노드를 지우고 트레이를 닫고 첫 오류(1차 리뷰). 전체 30초(`MEDIUM_TIMEOUT`). 없는 드라이브는 `InvalidSpec("the VM has no CD-ROM drive {i}")`. 이 QMP 명령은 QEMU 2.8~2.12부터 있어 Ubuntu 24.04의 8.2에서도 된다.
- 핫플러그 계획: `Step::ChangeMedium { index }`(vCPU·메모리 뒤, 붙이기 앞, 루트 포트를 쓰지 않으므로 핫플러그 배치 없이 켠 VM도 됨). 드라이브 수가 다르면 `DeviceError::Changed("add or remove CD-ROM drives")` → 409 `vm_running`(`check_hot_change`). CD `boot_order`만 바뀌면 보류 `cdroms[i].boot_order`(Restart). 핫플러그 Task는 `resolve::runtime_medium`으로 풀고 `change_medium` 뒤 기록에는 `iso`만 쓴다(부팅 순서 보류를 지키려고 — S6). 실패는 Task 실패, 기록 그대로, 보류 `cdroms[i]`(Failed). 켜는 동안 바뀐 매체는 보류 `cdroms[i]`(`"changed while the VM was starting; run `vm hotplug`"`), 드라이브 수가 바뀌면 보류 `cdroms`. 빼기·바꾸기에 옛 ISO를 비활성화하지 않는다(파일 풀은 활성화가 할 일이 없음 — S6).
- API 편의 `POST /vms/{key}/cdrom` `CdromMediaRequest { drive(기본 0), iso? }`: 그 드라이브의 매체만 바꿔 PUT과 같은 길(`write_spec` — 같은 검사·잠금, 실행 중이면 `vm-hotplug` Task와 헤더 `orbivisor-task`)로 쓴다. 없는 드라이브 400 `"VM … has no CD-ROM drive N"`, 응답 200 VM, 감사 `vm cdrom web drive 0: insert ubuntu-24-04`/`eject`. 드라이브 더하기·빼기는 PUT(멈춘 VM).

## 7. 부팅 순서

- 장치마다 `boot_order`(디스크는 기존 필드, CD는 `CdromSpec.boot_order`). 겹침 검사를 디스크와 CD 전체로(`"boot_order {n} is used by more than one disk or CD-ROM drive"`). 씨앗 드라이브는 부팅 장치가 아니다.
- **실제 순서 `opl_types::vm::effective_boot(disks, cdroms)`**: CD가 없으면 그대로(지금 VM의 명령줄이 바뀌지 않음). CD가 있으면 — 아무 장치에도 값이 없을 때 디스크 1, 2, … 그 뒤 CD; 값이 CD에만 있으면 디스크를 가장 큰 값 뒤에 차례로, 그 뒤 값 없는 CD(CD로 설치하고 매체를 뺀 UEFI VM이 디스크로 켜지게 — 1차 리뷰); 디스크에 값이 있으면 값 없는 디스크는 그대로 없음이고 값 없는 CD는 가장 큰 값 + 1부터 드라이브 순. 255를 넘으면 400 `BootOrderOverflow`(`"boot_order leaves no room for the CD-ROM drives after it"`). 그래서 기본은 **디스크 → CD**(Proxmox 기본과 같음): 빈 디스크는 펌웨어가 건너뛰어 설치 프로그램이 뜨고 설치 뒤에는 디스크로 켜진다. OVMF·aarch64 EDK2는 `bootindex`가 있는 장치만 부팅하므로(fw_cfg `bootorder`) CD에는 늘 값을 준다. 웹 `vms/boot.ts` `effectiveBoot`이 같은 규칙과 같은 시험 값을 쓴다.
- resolve가 `RuntimeDisk.boot_order`·`RuntimeCdrom.boot_order`를 `effective_boot`로 채우고 명령줄이 `bootindex`로 쓴다. 실행 중 CD 순서 바꾸기는 보류 → `vm reboot`(새 QEMU)에서 적용, 디스크 순서는 지금처럼 409. 실행 중 붙이는 디스크의 `boot_order`가 도는 QEMU의 실제 순서(켤 때 디스크·CD로 셈한 값 — CD에는 값이 없어도 매겨짐)와 겹치면 409 `vm_running` `"boot_order N is already used by cdrom0 in the running VM \"web\"; pick another or set it after a restart"`(QEMU `device_add`가 겹치는 `bootindex`를 거절 — 1차 리뷰).
- CLI `--boot disk0,cdrom0`(`disk`=`disk0`, `cdrom`=`cdrom0`, 목록 순서대로 1, 2, …, 목록에 없는 장치는 없음). 웹은 "부팅 순서" 카드에서 디스크·CD를 한 목록으로 위·아래로 옮기고 디스크는 "부팅 안 함"으로 뺀다 — 한 번 바꾸면 목록 순서대로 1, 2, …를 적는다(CD는 늘 부팅 장치). 실행 중인 VM은 웹에서 이 카드를 잠근다(디스크 순서는 409이고 CD 순서만 바꾸는 길은 CLI·API에 남김 — S8).
- "한 번만 CD로 부팅"은 없다: `-boot once=d`는 SeaBIOS만 따른다.

## 8. cloud-init 스펙

- `VmSpec.cloud_init: Option<CloudInit>`(`opl-types/src/cloud_init.rs`): `hostname`, `user`, `password`, `ssh_keys`, `networks: Vec<CloudInitNet { ipv4?, ipv6? }>`(순번 = NIC 순번), `dns: { servers, search }`, `user_data`, `vendor_data`(모두 생략 가능, `deny_unknown_fields`). `IpConfig`(`tag = "mode"`): `dhcp` / `slaac`(ipv6만) / `static { address: "주소/접두", gateway? }`.
- 검증 `CloudInit::validate(nics)`(문구는 `cloud_init.` 접두): `hostname`은 IP가 아닌 RFC 1123 이름·FQDN 253자(`valid_host`), `user`는 `[a-z_][a-z0-9_-]{0,31}`이고 `root` 거절, 키 32개·한 줄·알려진 접두·합 16 KiB, `networks` ≤ NIC 수, 고정 주소는 자체 `addr/prefix` 해석(보안 그룹 `Cidr`는 호스트 비트를 거절하므로 쓰지 않음 — S1)과 칸의 계열, gateway 같은 계열, DNS 서버 3·검색 6, `user_data`·`vendor_data` 각 64 KiB. 빈 비밀번호 거절, 평문 256바이트 넘으면 거절(crypt 해시·마스크는 통과).
- `user_data`가 있으면 생성한 user-data 대신 그대로 쓴다(`cicustom user=`) — `user`·`password`·`ssh_keys`는 쓰이지 않고, create·update·CD 엔드포인트가 새 응답 헤더 **`orbivisor-warning`**(`revision::WARNING`, CORS 노출)으로 알린다. CLI는 stderr에, 웹은 폼에서 스스로 보인다. `vendor_data`는 vendor-data 파일로 따로.

## 9. 비밀번호

- 입력이 cloud-init이 해시로 보는 crypt 해시의 엄격한 모양(`is_crypt_hash` — `$6$`·`$5$`는 `[rounds=N$]` + 솔트 1~16자 + 해시 86·43자, `$1$`, `$2a$`·`$2y$`; 문자는 `[./0-9A-Za-z]`)이면 그대로, yescrypt `$y$`·bcrypt `$2b$`는 cloud-init이 평문으로 설정해 버리므로 400 `"cloud_init.password: cloud-init cannot use yescrypt/bcrypt-2b hashes; use a SHA-512 crypt hash ($6$, e.g. `mkpasswd -m sha-512`) or a plain password; plain passwords may not start with $y$ or $2b$"`(평문이어도 그 접두면 거절 — 2차 리뷰), 모양이 어긋난 값은 평문으로 보아 256바이트 상한을 받는다(너무 큰 값으로 씨앗이 1 MiB를 넘지 않게 — 1차 리뷰). 그 밖은 아니면 API가 **SHA-512 crypt**(`opl_types::crypt::sha512_crypt`, Drepper 명세를 `ring::digest::SHA512`로, `$6$` + `getrandom` 솔트 16자 `CRYPT_SALT_CHARS`, 5000회)로 바꿔 저장한다. cloud-init은 `password:`가 해시면 `chpasswd -e`로 넣는다.
- **응답에서 가림**(watch 주석의 규칙 — 해시도 내보내지 않음): VM·스냅샷·백업의 조회·목록·PUT/POST 응답, `POST /vms/{key}/cdrom`, 백업 받기 완료, watch 이벤트에서 `"********"`(`PASSWORD_MASK`). 응답 타입은 직접 가리고(`mask_password`·`public_vm`·`backups::mask_backup`), watch 스트림만 순수 `redact_vm_json(kind, &mut Value)`를 쓴다(S6).
- **user-data·vendor-data**는 저장되고 VM 쓰기 권한자에게는 그대로 보인다(고쳐 보내야 한다). 쓰기 권한이 없는 주체(viewer)에게는 VM·스냅샷·백업의 조회·목록과 watch 이벤트에서 `"********"`로 가린다(`vms::mask_spec`). PUT의 값이 MASK면 저장된 값을 두고, 저장된 것이 없으면 400 `"cloud_init.user_data: give the user_data itself"`(1차 리뷰). watch 스트림은 쓰기 권한을 구독할 때 한 번 본다 — 역할이 바뀌면 다시 연결해야 가림이 바뀐다.
- **되보내기**: PUT의 `password`가 MASK면 저장된 해시를 둔다(웹·CLI가 받은 스펙을 고쳐 되보내는 길). 저장된 것이 없으면 400 `"cloud_init.password: give a new password"`. 없음은 지움. 스냅샷 되돌리기·백업 복원은 Raft 안의 해시를 그대로 옮긴다. 웹은 저장된 비밀번호를 "설정됨 · 바꾸기·지우기"로 보이고 바꾸지 않으면 MASK를, CLI `vm set`도 받은 MASK를 그대로 보낸다.

## 10. NoCloud 씨앗

- 순수 `cloud_init::render(vm_id, vm_name, ci, nics) -> CloudInitSeed { instance_id, meta_data, user_data, vendor_data?, network_config? }`(`VmRuntimeSpec.cloud_init`).
- **YAML은 JSON으로 쓴다**(JSON은 YAML의 부분집합이고 PyYAML이 읽는다 — 따옴표·특수 문자를 손으로 다루지 않음). user-data만 첫 줄 `#cloud-config`.
- meta-data `{"instance-id": "iid-{16 hex}", "local-hostname": "{첫 라벨}"}`. instance-id = SHA-256(VM id + local-hostname + user-data + vendor-data + network-config)의 앞 16자(S2 — 호스트 이름도 해시에 넣음). 내용이 바뀌면 바뀌어 cloud-init이 인스턴스마다 하는 일(사용자·키·호스트 이름·네트워크, SSH 호스트 키 다시 만들기)을 다시 하고, 같으면 다시 켜도 하지 않는다.
- user-data(생성): `hostname`, 점이 있으면 `fqdn`, `manage_etc_hosts: true`, `user`가 있으면 `user: NAME`(배포판 기본 사용자의 이름을 바꿈), 비밀번호가 있으면 `password`·`chpasswd: {expire: false}`·`ssh_pwauth: true`(없으면 `ssh_pwauth`를 적지 않음), 키가 있으면 `ssh_authorized_keys`.
- network-config **v1**: NIC마다 `{"type": "physical", "name": "eth{i}", "mac_address": …, "subnets": [dhcp|static, dhcp6|ipv6_slaac|static6]}` + `nameserver`. 주소도 DNS도 없으면 파일을 넣지 않는다(cloud-init 기본 — 첫 NIC DHCP). DNS만 있어도 모든 NIC IPv4 DHCP로 쓴다(S2). 항목이 없거나 빈 NIC는 IPv4 DHCP이고, IPv6만 설정한 NIC에는 IPv4 subnet을 두지 않는다(IPv6 전용 망에서 DHCPv4를 기다리지 않게 — 1차 리뷰; CLI는 `--ci-ip6`만 준 NIC를 `no ipv4`로 보이고 웹 IPv4 칸에 "없음(IPv6만)"). MAC 없는 NIC는 `mac_address`를 생략한다.
- **ISO 9660 쓰개** `opl_qemu::iso9660::build(volume_id, files)`(새 크레이트 없음): 뿌리 디렉터리 하나에 파일만(이름 `[A-Za-z0-9._-]` 1~64자, 30개·1 MiB까지 — `IsoError::{BadName, TooManyFiles, TooLarge, BadVolumeId}`), PVD(볼륨 id `CIDATA`, 8.3 이름) + **Joliet** SVD(UCS-2 이름 그대로 `meta-data`, 라벨 `cidata`) + 종료 기술자, 경로 표 넷, 뿌리 디렉터리 둘(크기는 맞춰 늘림), 파일 데이터. 날짜는 고정(2000-01-01) — 같은 입력이면 같은 바이트. `cidata_files(seed)`가 파일 목록.
- **씨앗 파일의 자리**: 볼륨이 아니라 VM 실행 디렉터리의 `cidata.iso`(`RuntimePaths.cidata`). `QemuDriver::launch`가 QEMU를 띄우기 전에 임시 파일 → rename, 0600으로 쓰고 씨앗이 없으면 남은 파일을 지운다 — 새로 켜기·마이그레이션 받기·HA 복구·복제본 복구·스냅샷 되돌리기 모두 `launch`를 지나므로 어느 노드에서 켜든 그 자리에서 다시 만들고, 노드·풀에 남는 것이 없다. 렌더는 노드의 resolve가 지금 스펙의 cloud-init으로 한다. NIC 입력(MAC)은 새로 켤 때 스펙의 NIC, 다시 붙거나 마이그레이션을 받을 때는 기록의 켤 때 단 NIC(`RunningHardware::boot_nics` — 기록의 앞 `boot_nics`개, 켤 때 단 NIC는 실행 중에 떼거나 바꾸지 못함)다 — 실행 중 붙인 NIC는 원본 씨앗에 없으므로 받는 쪽도 넣지 않는다(2차 리뷰). → **cloud-init 변경은 다음 QEMU 시작에 적용**(보류 `cloud_init`, running/wanted = instance-id 또는 `none`, Restart). 게스트 안의 재부팅은 옛 씨앗을 본다. `cloud_init`을 켜거나 끄면(드라이브가 생기거나 없어짐) 재시작 전에는 `devices_match`가 거짓이라 마이그레이션이 막힌다.
- 핫플러그 계산이 씨앗 instance-id를 셈하려면 VM id·이름이 필요해 서명이 바뀌었다: `RunningHardware::at_start(id, name, spec, caps, enabled)`, `plan(id, name, spec, hw, caps)`, `plan_resources(id, name, …)`, `pending(id, name, spec, hw, caps, failed)`(S2 — 설계와 다름).

## 11. 다른 기능과의 관계

- **배치**: 스케줄러 `reaches_disks`가 `cdroms[].iso`도 본다(로컬 풀 ISO는 그 노드에서만) — 시작·`node_fits`·균형 조정기·비우기가 그대로 그 노드에 둔다. 고정된 노드가 ISO 풀을 쓸 수 있어야 한다(400 `"node … cannot use ISO …"`, 그 노드에 같은 ISO로 이미 저장돼 있으면 다시 보지 않음 — S6).
- **마이그레이션**: 실행 중 VM의 매체가 공유가 아닌 풀에 있으면 409 **`cdrom_local`**(`local_cdrom` — 기록의 매체를 봄). 비우기는 대상 고르기 전에 묶음의 모든 VM을 확인하고 같은 문구로 실패한다. 매체를 옮겨 주지 않는다(설치 매체를 몰래 빼는 것보다 거절이 안전). 공유 풀 ISO는 양쪽 QEMU가 같은 파일을 읽기 전용으로 함께 연다. 매체를 바꾸는 중이면 `devices_match` 거짓 → `vm hotplug`를 먼저. 실행 중 cloud-init을 바꿨거나 호스트 이름 없는 VM의 이름을 바꿨으면(스펙으로 만들 씨앗의 instance-id ≠ 기록 — `hotplug::seed_matches`) 보내는 API가 409 **`restart_required`** `"cannot migrate VM …: cloud-init changed while running; restart the VM before migrating it"`(기다려도 풀리지 않으므로 `vm_busy`가 아님 — 비우기 결과와 HA 로그가 이 문구를 그대로 보임, 2차 리뷰), 균형 조정기는 옮기지 않고, 받는 쪽 `resolve`도 `ResolveError::CloudInitChanged`로 거절한다(1차 리뷰). `seed_matches`는 씨앗을 스펙의 cloud-init과 켤 때 단 NIC로 셈하므로 networks가 있는 VM에 NIC를 핫플러그해도 막지 않는다(새 NIC는 보류 `cloud_init`으로 다음 재시작에 — 2차 리뷰).
- **HA·복제**: `ha_active()`이거나 `replication`이 있는 VM의 스펙에 로컬 풀 ISO를 넣으면 400 `"HA and replicated VMs can use ISOs only from shared pools; …"`. 빈 드라이브와 공유 풀 ISO는 된다. 웹은 그런 VM에 공유 풀 ISO만 보인다.
- **스냅샷**: 블록 노드가 `disk{i}`만이라 CD·씨앗은 담지 않는다. `vm_spec`·`hardware`에 `cdroms`·`cloud_init`이 들어가 되돌리기가 그 매체·설정으로 켠다(그 ISO는 스냅샷이 있는 동안 지울 수 없음). 메모리 스냅샷은 읽기 전용 CD를 빼고 담고 되돌리기는 기록의 같은 장치로 켠다.
- **백업**: 디스크만 담고 매니페스트의 `vm_spec`은 그대로. 복원(원격 복제·DR 복원 포함)은 `cdroms[].iso`가 없거나 ISO가 아니거나 복원 노드에서 쓸 수 없으면 그 드라이브를 비우고(드라이브는 남김) Task 경고 `"CD-ROM drive {i}: ISO {iso:?} is not available; the drive is empty"`를 단 채 성공한다. `cloud_init`은 그대로(새 VM id라 instance-id가 바뀌어 다시 적용) — 고정 IP나 호스트 이름이 있으면 Task 경고 `"the restored VM keeps the cloud-init static IP addresses … and host name …; change them before starting it"`. 원격 클러스터가 받을 때(`backup_receive`) `install-media`를 모르는 노드가 있으면 `cdroms`·`cloud_init`을 버리고 디스크에 부팅 순서가 없으면 첫 디스크를 1로 둔다(`drop_install_media`, 로그 경고 — 1차 리뷰). 받기는 `require_current`라 옛 버전 노드가 있으면 409이므로 같은 버전이지만 기능이 모자란 노드가 있을 때만의 방어다. 보내는 쪽의 사본 확인(`replicate::check_copy`)은 원본 스펙을 받는 쪽과 같은 규칙으로 바꿔 견준다(`expected_spec` — 양쪽 cloud-init 비밀 가림, `replication` 버림, 사본에 없는 핫플러그 상한·CD-ROM·cloud-init 버림): 원격 응답은 비밀번호를 가리므로 그대로 견주면 cloud-init 비밀번호가 있는 VM의 이어 하기·다시 하기·`replica_exists`가 늘 실패했다(2차 리뷰).
- **VM 복제(clone)**: 같은 ISO를 가리키고 `cloud_init`도 그대로 — 새 id·이름이라 instance-id가 다르고, `hostname`이 없으면 새 이름. 고정 IP·호스트 이름은 그대로 옮기고 Task 경고 `"the clone keeps the cloud-init static IP addresses 10.0.0.5/24 and host name \"web-01\"; change them before starting it"`로 알린다(1차 리뷰).
- **ISO 지우기와 붙이기의 엇갈림**: `DELETE /volumes/{key}`는 확인과 Task 만들기를 `vm_lock`으로 VM 쓰기와 함께 잠그고, 지우기 Task가 ISO면 지우기 전에 CD 사용과 그 매체가 든 스냅샷을 다시 본다(다른 노드의 요청이 그새 넣었으면 같은 `in the CD-ROM drive`·`used by snapshot` 문구로 실패 — 1차·2차 리뷰). 끝나지 않은 `VolumeDelete` Task가 있는 ISO는 CD에 넣지 못한다(`prepare`의 409 `volume_in_use` `"ISO … is being deleted (task …)"` — 2차 리뷰).
- **로컬 디스크 복제(계획 55)**: `disk{i}`만, 씨앗은 받는 쪽에서 다시 만듦, 로컬 ISO는 처음부터 막음.
- **VMware·OVA 가져오기**: CD 장치는 가져오지 않는다. 내보내기 없음.

## 12. API·CLI·웹

- **API**: `POST /volumes/import`·`/import/check`의 쿼리 `content`, `POST /volumes/download`, ISO 거절(`POST /volumes`·resize), `DELETE`의 사용 검사, VM 스펙의 `cdroms`·`cloud_init`(기존 `POST /vms`·`PUT /vms/{key}`), `prepare` 검사 순서(기능 → 검증 → 디스크(ISO 거절) → CD(없음 400·준비 안 됨 409 `volume_not_ready`·ISO여야 400·HA/복제 + 로컬 ISO 400·지정 노드) → 비밀번호 해시), 새 `POST /vms/{key}/cdrom`, 마이그레이션 409 `cdrom_local`, 응답 가림, 헤더 `orbivisor-warning`. Task `volume-download`, 매체 바꾸기는 기존 `vm-hotplug`.
- **CLI**(oplctl `media.rs`): `iso list|upload|download|delete`, `volume download --source-format --format`, `vm create --cdrom ISO|none --boot LIST --cloud-init --ci-*`, `vm set --boot --ci-* --no-cloud-init --ci-no-password --ci-clear-ssh-keys`, `vm cdrom insert|eject|add|remove`, `vm get`의 `cdroms:`·`boot:`·`cloud-init:` 줄(매체 바뀌는 중 `<- changing from Y`). `vm cdrom remove`는 `--drive`가 없으면 마지막 드라이브, `--ci-ssh-key`는 더하기(`--ci-clear-ssh-keys`로 비움), `--ci-dns`·`--ci-search`는 목록 전체 교체, `--ci-ip`/`--ci-ip6`는 그 NIC만 바꾼다. 평문 비밀번호는 서버가 해시하고 `--ci-password-hash`는 로컬에서 `is_crypt_hash`로 확인한다(S7).
- **웹**(S8): 메뉴 "ISO 이미지"(`/isos`, 아이콘 `Disc`) — 이름·풀·크기·상태(받는 중이면 진행률 막대와 `downloading`·`verifying`·`importing` 단계)·노드(노드 로컬 풀만)·넣은 VM, 올리기(`UploadDialog`의 `content: "iso"` — `.iso`, 디렉터리·NFS 풀만, 형식 고르기 없음), URL에서 받기(`isos/DownloadDialog.tsx` — URL 끝에서 이름 제안, SHA-256 선택), 지우기(넣은 VM이 있으면 단추를 막음). 볼륨 화면은 ISO를 빼고 보이며 "ISO 이미지도 보기" 칩과 디스크 이미지 "URL에서 받기"(원본 형식·형식)가 있다. VM 폼에 `CdromsSection`(4개까지, ISO 고르기·비우기·더하기·빼기 — 실행 중에는 매체만), `BootOrderSection`, `CloudInitSection`(켜기, 호스트 이름(빈칸 = VM 이름), 사용자, 비밀번호(저장돼 있으면 "설정됨"), SSH 키(여러 줄), NIC마다 IPv4 DHCP/정적·IPv6 없음/DHCPv6/SLAAC/정적, DNS·검색 도메인, "고급"의 user-data·vendor-data와 덮어쓰기 경고). 저장된 `networks`가 있으면 모두 DHCP라도 그대로 보낸다 — 다른 칸만 고쳐도 network-config가 사라져 instance-id가 바뀌지 않게(S8). NIC를 빼면 그 NIC의 cloud-init 주소도 뺀다. VM 상세: 요약에 부팅 순서 줄(`disk0, cdrom0`)과 cloud-init 요약(사용자·키 수·비밀번호·주소, instance id, 보류면 "다음 재시작 때 적용"), "하드웨어" 탭의 CD/DVD 표(드라이브마다 매체·부팅 순서·넣기(ISO 고르기)·빼기 — 실행 중에도, `POST /vms/{key}/cdrom` + `orbivisor-task` 추적, 바꾸는 중 표시). 보류 이름 `cdroms[i]`·`cdroms[i].boot_order`·`cdroms`·`cloud_init`(`vms/hotplug.ts` `pendingFieldLabel`), Task 이름 `tasks.op.volume-download`, 오류 `cdrom_local`·`restart_required`. 받기 URL에서 이름을 제안할 때 잘못된 퍼센트 인코딩은 그대로 쓴다(`nameFromUrl` — 2차 리뷰).
- **opld 설정**: 새 절은 없다. 받기 시간 상수는 opl-api, 업로드 디렉터리는 `TaskRunnerConfig.upload_dir`(`{data_dir}/uploads`).

## 13. 시험과 CI

- 순수(어디서나): 타입·검증·serde(옛 JSON이 그대로 풀리고 다시 쓰면 같음), `effective_boot`(Rust·웹 같은 값), `sha512_crypt`(Drepper 명세 시험 값 다섯, `openssl passwd -6`이 있으면 무작위 견줌), `render`(네 파일의 정확한 문자열, instance-id), ISO 쓰개(시험 읽개 왕복, 고정 바이트, `bsdtar`가 있으면 다른 구현으로 읽기), 명령줄, 핫플러그 계획·`devices_match`·`qemu_devices`, 응답 가림, CLI 해석, 웹 폼·화면.
- `MockHypervisor` + API·reconciler 시험: 매체 넣기·빼기 Task와 기록, 마이그레이션 거절·받는 쪽 `apply_hardware`, 업로드·받기(127.0.0.1의 시험 HTTP 서버 — 밖으로 나가는 연결 없음, `storage-it`인 `tests/downloads.rs`는 CI `check`의 storage 단계에 `--test downloads`), 지우기 거절, 스냅샷·백업·복제 처리, 해시가 응답 어디에도 없음.
- 실제 QEMU(게스트 OS 없음 — 이 Mac·svc01·linux-kvm, `qemu-it`): `opl-qemu/tests/cdrom.rs`(CD 둘 + 씨앗, `query-block`의 매체·`ro`·`removable`, 넣기·빼기와 `DEVICE_TRAY_MOVED`, `qom-get bootindex`, `cont` 뒤 펌웨어가 CD를 읽음 — `rd_bytes > 0` 120초까지), opl-driver `tests/qemu_driver.rs`(씨앗 파일·`change_medium`), opld `tests/install_media.rs`(CLI로 ISO 올리기 → VM 만들기 → 켜기 → 기록 → 넣기·빼기 → 지우기 409), `migration-it` `a_vm_with_a_cdrom_and_cloud_init_migrates`(공유 ISO 옮김, 로컬 ISO 409).
- GitHub 러너만(실제 게스트): opld 기능 `install-media-it`의 `mod guest` — (가) Alpine virt 3.20.3 ISO를 127.0.0.1 HTTP 서버로 `iso download`(SHA-256) → 빈 디스크 + CD VM(BIOS·UEFI)이 CD에서 8 MiB 넘게 읽는지, (나) Ubuntu 24.04 클라우드 이미지 `volume download --source-format qcow2` → 정적 주소(slirp 10.0.2.15/24)·사용자·해시·키·`vendor_data`의 `runcmd` 보고(10.0.2.2) → hostname·사용자·shadow 해시·키·netplan 확인, `vm set --ci-hostname` + `vm reboot` 뒤 두 번째 보고. CI 작업 `install-media`(ceph 뒤·ldap 앞, ubuntu-24.04, 30분, 이미지는 받는 단계에서 `sha256sum -c`).
- **사전 확인(이 Mac, Homebrew QEMU 11.1)**: aarch64 `virt`(HVF) + EDK2의 `scsi-cd`는 `cont` 15초 뒤 `rd_bytes` 47104, q35(TCG) + OVMF의 `ide-cd`는 67584, SeaBIOS는 4096. 빈 드라이브는 `inserted` 없이 `removable: true`, `qom-get … bootindex`는 준 값 또는 -1.

## 14. 스테이지 보고 (설계와 다른 것·결정)

- S1: 고정 주소를 `Cidr`가 아니라 자체 `addr/prefix`로 해석(인터페이스 주소에는 호스트 비트가 있다). `TooManySataDevices`는 CD·cloud-init이 있을 때만 검사. ChangeMedium 실행·받기 Task는 뒤 스테이지까지 "this node cannot … yet"으로 실패.
- S2: instance-id 해시에 local-hostname을 넣음. DNS만 있어도 network-config를 씀. 핫플러그 계산 서명에 VM id·이름(§10).
- S3: **SeaBIOS(q35 BIOS)는 씨앗 드라이브의 첫 4096바이트를 읽는다**(부팅 순서에 없는 드라이브도 시도). `-boot strict=on`은 모든 VM의 명령줄을 바꾸므로 쓰지 않았고, "씨앗을 읽지 않음"은 UEFI에서만 단언한다.
- S4: `change_medium`의 "같은 매체면 할 일 없음"은 드라이버가 `query-block`으로. `apply_hardware`의 CD 부팅 순서·씨앗 다루기(§5).
- S5: `vm_uses_volume`이 이미 CD 매체(스펙·기록)를 봄, 이름 `download` 예약, 오류 문구에서 URL 비밀번호 가림.
- S6: §6·§9·§11의 "S6" 표시들(매체 바꾸기 뒤 기록에는 `iso`만, 고정 노드 확인 건너뛰기, 비우기 전 확인, 가림을 응답 타입에 직접, `install-media` 검사를 검증 전에). qga 시험의 경합을 고침(macOS fork가 듣기 소켓을 잠깐 쥠).
- S7: CLI의 더하기·바꾸기 규칙(§12). 새 CI 작업은 `check`·`web`·`shell` 작업에 줄을 더하지 않아 프로브 `mk.py`를 고칠 것이 없다.
- S8: 웹 부팅 순서 카드는 실행 중 잠금, 저장된 `networks` 보존, CD 드라이브 번호는 0부터(API·CLI와 같음). 디스크 행의 부팅 순서 칸은 부팅 순서 카드로 옮겼다.
- **실제 게스트 시험 결과**: GitHub 프로브 1(2026-11-17, ubuntu-24.04 x86_64 KVM, plan57 S7 커밋): ceph·install-media·linux-kvm·ldap·package·ISO 모두 통과(첫 실행). install-media: Alpine virt 3.20.3 ISO — BIOS VM은 cd0에서 20,557,986바이트, UEFI VM은 20,860,928바이트를 읽음(8 MiB 기준 통과). Ubuntu 24.04 클라우드 이미지 — cloud-init이 Joliet cidata를 찾아 호스트 이름 ci-probe, 사용자 orbi, $6$ 해시, SSH 키, eth0 고정 10.0.2.15/24·기본 경로 10.0.2.2·DNS 10.0.2.3을 적용(netplan set-name eth0, dhcp4 없음). vm set --ci-hostname ci-probe2 → vm reboot 뒤 instance-id가 iid-edc5a80913c12962 → iid-34131193845df658로 바뀌고 호스트 이름 ci-probe2 적용.
- svc01(aarch64 QEMU 8.2 TCG): opl-qemu cdrom 2·hotplug 4·qemu_boot 3, opl-driver qemu_driver 19, opld install_media(게스트 없는 부분) 1, migration-it 9 통과.

### 알려진 한계

- **SeaBIOS는 씨앗 드라이브의 첫 4096바이트를 읽는다**(위 S3). 부팅하지는 않으며 UEFI 펌웨어는 읽지 않는다.
- **실행 중 cloud-init을 바꾼 VM은 재시작 전에는 옮기지 않는다**(§11 마이그레이션 — 1차 리뷰가 "옮기되 게스트가 새 씨앗을 봄"을 막음). 다시 켜면(`vm reboot`) 새 씨앗과 기록이 맞아 옮길 수 있다.
- **받는 쪽이 씨앗을 다시 렌더하므로 렌더 결과를 바꾸는 업그레이드 중에는 cloud-init VM 마이그레이션이 거절될 수 있다**(재시작하면 풀림). 씨앗 바이트를 기록에 담지 않고 instance-id만 견준다(2차 리뷰).
- **받기 Task의 URL은 Raft에 그대로 있다**: 응답·watch에서는 가리지만(§4) 저장소와 백업된 Raft 스냅샷에는 사용자 정보·쿼리가 남는다.

## 15. 견준 길

- **`ImageFormat::Iso` vs 내용 종류(채택).** 형식을 더하면 형식을 보는 곳(스냅샷 가능 여부·백업·마이그레이션 복사·복제·`disk_location`·`qemu-img` 인자)을 모두 고쳐야 한다. ISO는 raw 파일 그대로이므로 형식은 raw이고 "무엇에 쓰는지"만 다르다.
- **씨앗을 볼륨으로 vs 실행 디렉터리 파일(채택).** Proxmox는 cloud-init 드라이브를 저장소의 디스크로 만든다. 볼륨이면 마이그레이션·HA·복제·백업·지우기마다 그 볼륨을 옮기고 지우는 길을 더해야 하고, 로컬 풀이면 노드에 묶인다. 씨앗은 스펙에서 결정적으로 만들어지므로 켤 때마다 그 자리에서 다시 만든다.
- **`genisoimage`·`cloud-localds` vs 자체 쓰개(채택).** 노드마다 패키지 의존이 늘고 출력이 판마다 다를 수 있다(날짜). 자체 쓰개는 수백 줄이고 고정 날짜로 마이그레이션 양쪽이 같은 바이트를 만든다.
- **Rock Ridge vs Joliet(채택).** 둘 다 긴 소문자 이름을 주지만 Joliet이 더 짧게 되고 Linux iso9660 드라이버·blkid가 Joliet 이름과 라벨을 쓴다.
- **network-config v2 vs v1(채택).** v1은 cloud-init의 모든 렌더러·옛 판·cloudbase-init이 읽고 Proxmox도 쓴다.
- **`-boot once=d` vs 부팅 순서(채택).** SeaBIOS만 따르고 OVMF·aarch64 EDK2는 `bootindex`만 본다. 기본 디스크 → CD로 같은 설치 흐름이 된다.
- **평문 비밀번호 저장 vs 해시(채택).** 평문은 Raft·스냅샷·백업 매니페스트·원격 복제로 퍼진다. cloud-init은 해시를 그대로 받는다.
- **로컬 ISO가 든 VM 마이그레이션: 매체를 옮김·빼고 옮김 vs 거절(채택).** 설치 중인 매체를 몰래 빼면 게스트가 설치 중 깨지고, 옮기려면 ISO 복사가 필요하다. 공유 풀에 두라고 알린다.

## 16. 나중 할 일

블록 풀 ISO(RBD는 읽기 전용 공유가 됨), 이어 받기 업로드(tus·조각), 한 번만 CD로(QOM `bootindex` 실행 중 바꾸기 + 재시작), NIC 부팅(PXE)과 부팅 순서의 NIC, ConfigDrive2, 클라우드 이미지 템플릿과 연결 복제, 패키지 필드, 콘텐츠 라이브러리의 원격 클러스터 간 동기화, Windows 응답 파일(autounattend) 씨앗, 받기 Task의 URL 자격 증명을 Raft 밖에(노드 로컬 비밀 저장소) 두기.
