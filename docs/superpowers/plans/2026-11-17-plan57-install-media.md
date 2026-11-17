# Orbivisor 계획 57: 설치 매체와 cloud-init 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ISO 이미지를 올리거나 URL에서 받아 VM의 CD/DVD 드라이브에 넣고(실행 중 넣기·빼기), 장치별 부팅 순서로 설치하며, Orbivisor가 만드는 cloud-init NoCloud 씨앗으로 클라우드 이미지 VM의 사용자·비밀번호·SSH 키·호스트 이름·고정 주소를 설정한다.

**Architecture:**
- **ISO는 내용 종류:** 형식은 raw 그대로, 볼륨의 `content: iso`로 구분한다(디렉터리·NFS 풀만). 올리기와 새 Task `volume-download`(URL, SHA-256, 진행률)로 만들며, 같은 Task로 클라우드 디스크 이미지도 받는다.
- **CD-ROM과 부팅 순서:** VM마다 CD 드라이브 4개까지(q35 `ide-cd`, virt `scsi-cd`), 빈 드라이브 허용, 실행 중 QMP로 매체 넣기·빼기(트레이 강제). 부팅 순서는 장치마다 `boot_order`, 기본은 디스크 → CD.
- **cloud-init 씨앗:** 스펙에서 결정적으로 `meta-data`·`user-data`·`network-config`(v1)·`vendor-data`를 렌더하고 자체 ISO 9660(Joliet, 고정 날짜) 쓰개로 실행 디렉터리에 `cidata` 씨앗을 켤 때마다 만든다. 비밀번호는 SHA-512 crypt 해시만 저장하고 응답에서는 가린다.

**Tech Stack:** Rust 2024(MSRV 1.88), QEMU `ide-cd`·`scsi-cd`·`blockdev-change-medium`·`blockdev-open-tray`, SHA-512 crypt(자체), ISO 9660 + Joliet(자체), reqwest 받기, axum·utoipa, React. 새로 받는 크레이트는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-17-install-media-cloud-init-design.md`
**선행 계획:** 계획 1~56 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| ISO 볼륨(올리기·URL 받기·목록·지우기), 디스크 이미지 URL 받기 | 블록 풀(RBD·LVM)의 ISO, 이어 받기 업로드 |
| VM의 CD-ROM 4개, 빈 드라이브, 실행 중 넣기·빼기 | 한 번만 CD로 부팅, NIC(PXE) 부팅 순서 |
| 장치별 부팅 순서 | ConfigDrive2, Windows 응답 파일 |
| cloud-init NoCloud(사용자·비밀번호·SSH 키·호스트 이름·NIC별 IPv4/IPv6·DNS·user-data·vendor-data) | 패키지 설치 필드, 클라우드 이미지 템플릿 |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 변경과 단위·통합 시험, 실제 QEMU 시험(게스트 OS 없음, `qemu-it`), 웹 시험.
- **배포 서버 svc01(Linux aarch64):** 같은 시험(`storage-it`의 `--test downloads` 포함).
- **GitHub(Ubuntu 24.04 x86_64):** 실제 게스트 시험 `install-media` 작업(Alpine ISO BIOS·UEFI 부팅, Ubuntu 클라우드 이미지 cloud-init), 그 밖에 linux-kvm·ldap·package·ceph.
- **테스트 VM:** 두 노드에 배포한 뒤 `install-media` 기능과 화면만 확인한다.

## 설계 결정

- **옛 노드가 읽을 수 있는 것만 쓴다:** `content: iso` 볼륨, `volume-download` Task, `cdroms`·`cloud_init`이 든 VM, 매체 바꾸기는 기능 `install-media`가 가린다. 빈 `cdroms`·없는 `cloud_init`·`content: disk`는 쓰지 않아 기존 JSON과 QEMU 명령줄은 그대로다.
- **비밀번호는 해시만:** 평문은 받자마자 SHA-512 crypt로 바꾸고, 응답·watch에서는 `********`로 가린다.
- **씨앗은 볼륨이 아니다:** 켤 때마다 실행 디렉터리에 다시 만들어 마이그레이션·HA·백업이 따로 다룰 것이 없다. 받는 쪽도 같은 바이트를 만든다.
- **로컬 ISO가 든 VM은 옮기지 않는다:** 몰래 매체를 빼지 않고 409로 공유 풀에 두라고 알린다.

## Task

각 Task는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 단계마다 `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 바뀐 크레이트의 시험을 통과한 뒤 커밋한다.

- [x] **Task 1: 설치 매체·cloud-init 타입·검증·기능과 CD-ROM 핫플러그 계산(opl-types)** — `Cdrom`·`CloudInit`·`BootOrder`·볼륨 `content: iso`, 기능 `install-media`, 스펙 검증, 실행 중 CD 매체 바꾸기 계산. 새 필드를 쓰는 모든 크레이트의 구조체 리터럴을 함께 맞춘다(76개 파일).
- [x] **Task 2: cloud-init 씨앗 렌더·SHA-512 crypt·ISO 9660 쓰개(opl-types·opl-qemu)** — `meta-data`·`user-data`·`network-config` v1·`vendor-data`를 결정적으로 렌더하고, 자체 SHA-512 crypt와 Joliet ISO 9660 쓰개(고정 날짜)로 `cidata` 씨앗을 만든다.
- [x] **Task 3: CD-ROM·cloud-init 씨앗 명령줄, QMP 매체 넣기·빼기와 실제 QEMU 시험(opl-qemu)** — q35 `ide-cd`, virt `scsi-cd`, 장치별 `bootindex`, `blockdev-open-tray`(강제)·`blockdev-change-medium`·`blockdev-remove-medium`, `qemu-it` 시험.
- [x] **Task 4: cloud-init 씨앗 쓰기, CD 매체 풀기·바꾸기와 부팅 순서(opl-driver·opl-reconciler)** — 켤 때마다 실행 디렉터리에 씨앗을 다시 쓰고, CD 볼륨을 경로로 풀고, 실행 중 매체 차이를 QMP로 맞춘다.
- [x] **Task 5: ISO 볼륨과 URL에서 받기(opl-api)** — ISO 올리기·목록·지우기, `volume-download` Task(URL, SHA-256, 진행률, 크기 제한), 디스크 이미지 받기, `[downloads]` 설정, `storage-it`의 `--test downloads`.
- [x] **Task 6: VM의 CD-ROM·cloud-init API, 실행 중 매체 바꾸기, 로컬 ISO 이동 규칙, 비밀번호 가림(opl-api)** — `/vms/{id}/cdroms/{n}` 넣기·빼기, 비밀번호 해시·응답 가림, 로컬 ISO가 든 VM의 마이그레이션·HA 409.
- [x] **Task 7: ISO·CD-ROM·부팅 순서·cloud-init CLI, opld 실제 시험과 CI install-media 작업(oplctl·opld·CI)** — `orbivisorctl iso …`, `vm cdrom insert|eject`, `vm create|set --cdrom/--boot/--ci-…`, opld `install_media` 실제 게스트 시험(Alpine ISO BIOS·UEFI, Ubuntu 클라우드 이미지)과 GitHub `install-media` 작업. 시험 도우미 `free_port`가 임시 포트 범위 밖(20000~31999)에서 프로세스마다 다른 구간을 쓰도록 고친다.
- [x] **Task 8: 웹 화면, README와 설계 문서** — ISO 이미지 화면·URL 받기, VM 폼의 CD/DVD·부팅 순서·cloud-init, VM 상세 CD 넣기·빼기, README 사용법, 설계 문서 `2026-11-17-install-media-cloud-init-design.md`.

## 완료 기준 (계획 57)

- `orbivisorctl iso upload|download`로 ISO를 두고, CD 드라이브가 있는 VM이 BIOS·UEFI에서 CD로 설치 매체를 읽으며(GitHub `install-media` 작업, Alpine virt ISO), 실행 중 `vm cdrom insert|eject`가 매체를 바꾼다.
- 클라우드 이미지 VM이 cloud-init 씨앗으로 호스트 이름·사용자·비밀번호 해시·SSH 키·고정 주소를 적용하고, `vm set --ci-…` 뒤 다시 켜면 새 값이 적용된다(GitHub `install-media` 작업, Ubuntu 24.04).
- 웹에서 ISO 이미지 화면·URL 받기, VM 폼의 CD/DVD·부팅 순서·cloud-init, VM 상세의 CD 넣기·빼기를 쓸 수 있다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 시험이 이 Mac과 svc01에서, linux-kvm·install-media·ldap·package가 GitHub에서 통과한다.

## 알고 받아들인 제한

- SeaBIOS(q35 BIOS)는 씨앗 드라이브의 첫 4096바이트를 읽는다(부팅하지는 않는다). UEFI는 읽지 않는다.
- 실행 중 cloud-init을 바꾼 VM은 다시 켜기 전에는 옮기지 않는다. 렌더 결과가 바뀌는 업그레이드 중에는 cloud-init VM 마이그레이션이 거절될 수 있다(다시 켜면 풀림).
- 받기 Task의 URL은 응답·watch에서만 가리고 Raft와 그 백업에는 그대로 남는다.
- ISO는 디렉터리·NFS 풀에만 둔다.
EOF

## 실행 후 기록 (2026-11-17)

- 8개 Task를 단계 커밋으로 구현했고, 리뷰를 두 번 받아 고친 내용은 각 단계 커밋에 합쳤다.
- svc01에서 일반 시험(fmt·clippy·`cargo test --workspace`·starve·scale 100·storage-it)이 통과했다(시험 2897개).
- GitHub 탐침(커밋 6737253)에서 install-media·linux-kvm·ceph·ldap·package·iso-e2e(bios·uefi) 작업이 모두 통과했다. install-media 작업은 Alpine ISO를 BIOS·UEFI에서 부팅하고, Ubuntu 24.04 클라우드 이미지에 cloud-init을 적용했다.
- 마지막 탐침 뒤 Mac이 재시작되어 작업 폴더와 원래 생성한 계획 문서(Task별 전체 코드)를 잃었다. GitHub에 남은 탐침 커밋에서 단계 커밋 8개를 그대로 되살렸다(트리가 검증한 단계 커밋과 같다). 이 문서는 Task 목록과 결과만 적은 요약본이다.
- **테스트 VM 배포(svc01·svc02):** svc02 → svc01 순서로 바이너리만 바꿨다(유닛은 같아 그대로). 두 노드 모두 sha256이 빌드와 같고, 헬스 `ok`, 두 voter 모두 online, 기능에 `install-media`가 있다. `iso list`는 빈 목록, `/isos` 화면 경로는 새 웹 빌드(index-D4JY2sMM.js)를 돌려준다. svc02의 test-vm은 QEMU PID 916 그대로 실행 중이다. 저널에는 기존 경고(단일 클러스터 링크, KVM 없음)뿐이고 ERROR·panic은 없다.
- Mac 재시작 뒤 Claude Code 프로세스가 LAN에 닿지 않아(Dripper.app 아래 실행), launchd로 띄운 SSH 터널로 svc01·svc02에 접속했다.
