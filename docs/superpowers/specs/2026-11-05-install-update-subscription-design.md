# opLite 설치·업데이트·구독 설계 (하위 프로젝트 9: 계획 31·32·33)

작성일: 2026-11-05
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해 §1.1), `2026-10-28-api-security-design.md`(역할·감사), `2026-11-04-backup-design.md`(리더에서 도는 스케줄러 모양)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 9는 설치 ISO, 업데이트 채널, 라이선스, 텔레메트리다(Proxmox 구독 저장소, VMware vLCM). 지금 opLite는 `cargo run`으로만 돈다. 서버에 올리려면 사용자가 직접 빌드하고, 서비스 파일·설정·브리지를 손으로 만들고, 새 버전은 다시 빌드해 바꿔 넣어야 한다. 이 하위 프로젝트는 opLite를 "ISO로 설치하고, 웹에서 업데이트를 보고 적용하고, 구독 상태를 보는" 제품으로 만든다.

| 계획 | 내용 |
|---|---|
| 31 | 패키지와 설치 ISO: Debian 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛), 설치 ISO(Debian netinst에 preseed와 패키지를 더함, 설치가 `vmbr0` 브리지와 `opld.toml`을 만듦), CI에서 패키지 설치 시험, 별도 워크플로에서 ISO 무인 설치 E2E |
| 32 | 버전과 업데이트: 빌드 버전과 노드별 버전 표시, 오래된 노드의 합류 거절, 클러스터 설정 `Settings`와 업데이트 채널(`stable`·`testing`), 노드의 APT 소스 관리, 업데이트 확인(노드별 업그레이드할 패키지), 업그레이드 Task(`systemd-run`으로 돌려 opld 재시작을 견딤, 클러스터에 하나씩), APT 저장소를 만드는 스크립트, API·oplctl·웹 |
| 33 | 구독과 텔레메트리: Ed25519로 서명한 오프라인 구독 키, 구독 상태(노드 수·만료), `enterprise` 채널(구독이 있어야 하고 APT 인증을 씀), 웹 안내 배너, 키 발급 도구, 옵트인 텔레메트리(기본 꺼짐, 보낼 내용 미리 보기, 리더가 하루 한 번), API·oplctl·웹 |

넣지 않는 것: 오프라인 설치(DVD 크기 ISO, 모든 의존 패키지 포함 — 설치에는 Debian 미러가 필요하다), ZFS 루트와 소프트웨어 RAID 설치 선택지, arm64 ISO(패키지는 나중에 arm64도), 기능 막기(구독이 없어도 모든 기능이 돈다), 온라인 구독 확인·활성화 서버, 구독 키의 클러스터 묶기(하드웨어 지문), 텔레메트리 수집 서버, 자동 업그레이드(사람이 누른다), 노드 비우기(유지보수 모드)와 자동 롤링 업그레이드, 커널·펌웨어 관리, 업데이트 되돌리기.

### 견준 길

- **배포 기반: Debian 패키지와 APT 저장소(채택) vs 자체 이미지(불변 OS, A/B 파티션) vs 단일 바이너리 내려받기.** Proxmox가 Debian 위에서 같은 길로 십 년 넘게 업데이트를 했다. QEMU·OVMF·LVM·ZFS·Ceph·nftables 같은 의존성을 배포판이 보안 업데이트까지 맡는다. 불변 OS는 되돌리기가 쉽지만 스토리지 도구(LVM, ZFS 모듈, iSCSI)와 맞추는 일이 크고, 단일 바이너리는 의존성 업데이트를 사용자에게 넘긴다.
- **Debian 버전: 13(trixie)(채택) vs 12(bookworm).** 2026년 11월에 13이 안정판이다. QEMU·nftables가 더 새롭고 지원 기간이 길다. 의존성 이름이 다르면 패키지 control 파일만 고친다.
- **설치 ISO: netinst + preseed + 우리 패키지(채택) vs live-build·자체 설치 프로그램.** netinst ISO를 풀어 preseed 파일, 우리 패키지, 부트 메뉴 항목만 더하고 `xorriso`로 다시 묶는다. Debian 설치 프로그램의 디스크·네트워크 화면을 그대로 쓰므로 새로 만들 것이 적다. 설치에 미러가 필요한 것은 한계로 적는다.
- **패키지 만들기: `dpkg-deb`와 손으로 쓴 control·maintainer 스크립트(채택) vs `cargo-deb`·debhelper.** 파일이 몇 개뿐이라 도구 의존성을 늘리지 않는다. 스크립트가 무엇을 하는지 저장소에서 바로 읽힌다.
- **업그레이드 실행: 노드의 opld가 `systemd-run`으로 `apt-get`을 돌림(채택) vs 사용자가 셸에서.** 웹에서 버튼 하나로 하는 것이 제품의 목표다. 패키지 postinst가 opld를 다시 시작하므로 apt는 opld 밖의 일시 유닛에서 돌고, 다시 시작한 opld가 그 유닛을 이어서 지켜본다. VM의 QEMU는 VM마다 자기 systemd scope(`opl-vm-<id>.scope`)에 있어 opld 재시작에 꺼지지 않고, 다시 시작한 opld가 실행 중인 VM을 이어 맡는다(노드 코어 설계).
- **구독 확인: 내장 공개 키로 오프라인 서명 검증(채택) vs 활성화 서버.** 폐쇄망 고객이 많고, opLite는 Apache-2.0이라 구독은 지원·엔터프라이즈 저장소에 대한 권리이지 기능 잠금이 아니다. 서버가 없으니 키를 여러 클러스터에 쓰는 것은 막지 못한다(한계로 적는다).
- **텔레메트리: 옵트인·미리 보기(채택) vs 기본 켜짐.** 온프레미스 가상화 고객은 밖으로 나가는 연결에 민감하다. 기본으로 꺼 두고, 켜기 전에 보낼 JSON을 그대로 보여 준다.

## 2. 패키지와 설치 ISO (계획 31)

### 2.1 Debian 패키지 `oplite`

`packaging/deb/build-deb.sh <version> <out dir>`이 이미 빌드한 릴리스 바이너리(`target/release/opld`를 `web-ui` 기능으로, `target/release/oplctl`)로 `oplite_<version>_amd64.deb`를 만든다(`dpkg-deb --build --root-owner-group`). 저장소에는 `packaging/deb/` 아래 control 템플릿, maintainer 스크립트, systemd 유닛, 설정 예시를 둔다.

| 경로 | 내용 |
|---|---|
| `/usr/bin/opld`, `/usr/bin/oplctl` | 바이너리 |
| `/lib/systemd/system/opld.service` | 아래 유닛 |
| `/usr/share/oplite/opld.toml.example` | README의 Linux 설정 예시와 같은 주석 달린 설정 |
| `/usr/share/doc/oplite/copyright` | Apache-2.0 |

- `opld.service`: `Type=simple`, `ExecStart=/usr/bin/opld --config /etc/opl/opld.toml`, `ConditionPathExists=/etc/opl/opld.toml`(설정이 없으면 실패 반복 대신 건너뜀), `Restart=on-failure`, `RestartSec=5`, `KillMode=process`(QEMU는 자기 scope에 있지만 혹시 같은 cgroup에 남은 자식이 있어도 opld만 멈춘다), `LimitNOFILE=1048576`, `After=network-online.target`, `Wants=network-online.target`.
- control: `Package: oplite`, `Architecture: amd64`, `Depends: qemu-system-x86, qemu-utils, ovmf, iproute2, nftables, systemd`, `Recommends: nfs-common, lvm2, thin-provisioning-tools, open-iscsi, lvm2-lockd, sanlock, ceph-common`, `Section: admin`, `Priority: optional`, `Maintainer`·`Homepage`는 저장소 값. `Version`은 인자 그대로(개발 빌드는 `0.1.0~git20261105.abc1234`처럼 `~`로 정식 버전보다 낮게).
- postinst `configure`: `systemctl daemon-reload`, 처음 설치면 `systemctl enable opld.service`(시작은 설정이 있을 때 유닛 조건이 정함 — `systemctl start`를 부르고 실패는 무시), 업그레이드면 `systemctl try-restart opld.service`.
- prerm `remove`: `systemctl stop opld.service`(VM은 scope에 남는다 — 패키지를 지워도 VM은 돌고, 다시 설치한 opld가 이어 맡는다), `upgrade`에서는 멈추지 않는다(postinst가 다시 시작).
- postrm: `purge`여도 `/var/lib/opl`(Raft 데이터, 풀, 볼륨)와 `/etc/opl`을 지우지 않는다. 데이터를 지우는 것은 관리자의 일이다. `daemon-reload`만 한다.
- 빌드 버전: `build-deb.sh`가 `OPL_VERSION=<version>`으로 빌드하게 README·CI가 같은 명령을 쓴다(계획 32의 빌드 버전, 계획 31에서는 `opld --version`·`oplctl --version`이 이 값을 보인다 — `clap` `version`에 `opl_types::version::BUILD`).

### 2.2 설치 ISO

`packaging/iso/build-iso.sh --netinst <debian netinst.iso> --deb <oplite.deb> --out <oplite.iso> [--unattended <test.cfg>]`.

1. netinst ISO의 SHA-256을 `packaging/iso/debian.lock`(URL과 해시, Debian 13 amd64 netinst 한 판)과 비교한다. CI는 이 파일로 내려받는다.
2. `xorriso -osirrox`로 풀고 `/oplite/`에 패키지, `/preseed/oplite.cfg`, `/preseed/oplite-setup.sh`를 넣는다.
3. 부트 메뉴(BIOS `isolinux/txt.cfg`, UEFI `boot/grub/grub.cfg`) 맨 위에 "Install opLite" 항목을 더해 기본으로 한다: `auto=true priority=high preseed/file=/cdrom/preseed/oplite.cfg`. 원래 항목은 그대로 남는다.
4. `xorriso -indev <netinst> -outdev <out> -map <dir> / -boot_image any replay`로 원래 ISO의 BIOS·UEFI 하이브리드 부트를 유지해 다시 묶는다. 볼륨 이름은 `OPLITE`.

preseed(`oplite.cfg`)는 질문을 줄이되 다음은 사람에게 묻는다: 언어·키보드(기본 en_US·us), 네트워크(DHCP, 실패하면 수동), 호스트 이름, root 비밀번호, 설치 디스크. 나머지는 정한다: 파티션은 고른 디스크 전체에 LVM(`atomic` 레시피 — `/`, swap), 미러 `deb.debian.org`, 패키지 `standard`·`ssh-server`, 일반 사용자 만들지 않음, `popularity-contest` 끔, GRUB은 고른 디스크에.

`preseed/late_command`가 `oplite-setup.sh`를 대상 시스템에서 돌린다.
- 패키지 `/cdrom/oplite/*.deb`를 `/target/var/cache/oplite/`로 복사하고 `in-target apt-get install -y ifupdown2 /var/cache/oplite/oplite_*.deb`.
- 네트워크: 설치가 만든 `/etc/network/interfaces`의 기본 경로 인터페이스(DHCP면 `inet dhcp`, 정적이면 주소·게이트웨이 그대로)를 `vmbr0`로 옮긴다: `auto vmbr0`, `iface vmbr0 inet …`, `bridge-ports <nic>`, `bridge-stp off`, `bridge-fd 0`, `bridge-vlan-aware yes`, `bridge-vids 2-4094`, NIC는 `iface <nic> inet manual`. 인터페이스를 못 찾으면 바꾸지 않고 `/root/oplite-setup.log`에 남긴다.
- `/etc/opl/opld.toml`: `name = "<호스트 이름>"`, `address = "<vmbr0이 될 주소>"`(정적이면 그 주소, DHCP면 설치 때 받은 주소 — DHCP 주소가 바뀌면 고쳐야 한다고 파일 주석과 README에), 나머지는 예시 주석.
- `/etc/issue.d/oplite.issue`: 콘솔 로그인 화면에 "opLite: https://\4{vmbr0}:8006/ — 첫 노드는 `oplctl cluster init --name <이름>`, 다른 노드는 `oplctl cluster join <URL>`".

`--unattended <cfg>`(시험 전용)는 묻는 값을 모두 정한 preseed(호스트 이름 `opl-ci`, root 비밀번호, 첫 디스크, DHCP)를 추가로 넣고, 부트 메뉴 시간 제한을 1초로, 그리고 첫 부팅에 한 번 도는 유닛 `oplite-ci-init.service`가 `oplctl cluster init --name ci`의 출력을 직렬 콘솔(`/dev/ttyS0`)에 쓰게 한다. 커널 명령줄에 `console=ttyS0`를 더한다.

### 2.3 시험

- 셸 스크립트는 `shellcheck`로 본다(CI).
- CI `ci.yml`에 `package` 작업(ubuntu): 릴리스 빌드(`web-ui`, 웹 빌드 포함) → `build-deb.sh` → `dpkg-deb --info`·`--contents`로 경로 확인 → `docker run debian:13`에서 `apt-get install ./oplite_*.deb`(의존성까지 받음), `opld --version`·`oplctl --version`이 그 버전, `systemd-analyze verify /lib/systemd/system/opld.service`, 설정 없이 유닛 조건이 거짓인지. 패키지는 아티팩트로 올린다.
- 새 워크플로 `iso.yml`(수동 실행, 주 1회, `ci-probe/**`·`v*` 태그 push): 패키지 → netinst 내려받기(`debian.lock` 해시 확인) → `build-iso.sh --unattended` → KVM으로 빈 디스크 20 GiB에 무인 설치(`-boot d`, 직렬 콘솔을 파일로, 시간 제한 40분) → 디스크로 다시 부팅 → 직렬 로그에서 `cluster init` 출력(CA 지문, admin 비밀번호)을 찾고, QEMU user 네트워크의 `hostfwd`로 연 8006에 `oplctl login`과 `oplctl node list`(노드 `opl-ci`, `online`)와 `oplctl network list`가 되는지, `ip -d link show vmbr0`는 직렬 콘솔이 없으므로 대신 노드 상태의 `bridges`에 `vmbr0`이 있는지 본다. 실패하면 직렬 로그를 아티팩트로 올린다.
- macOS에서는 `dpkg-deb`·`xorriso`가 없어 스크립트 실행 시험을 하지 않는다(CI만).

### 2.4 알려진 한계

설치에 인터넷(Debian 미러)이 필요하다. DHCP로 설치하면 `address`가 그 순간의 주소다. ZFS·RAID 루트, arm64 ISO, 설치 화면에서 클러스터 만들기·합류는 없다. 패키지를 지워도 데이터는 남는다.

## 3. 버전과 업데이트 (계획 32)

### 3.1 빌드 버전과 노드 버전

- `opl_types::version::BUILD: &str` = `option_env!("OPL_VERSION")`, 없으면 `CARGO_PKG_VERSION`. opld·oplctl의 `--version`, API `GET /api/v1/version`(인증 없이, `{ "version": … }` — 로그인 화면과 oplctl이 서버 버전을 본다).
- `NodeStatus.version: Option<String>`(옛 노드는 None, 빼고 쓴다). 노드가 등록·상태 갱신 때 `BUILD`를 쓴다.
- `oplctl node list`에 `VERSION`, 웹 노드 표에 버전 열. 노드 버전이 둘 이상이면 웹 대시보드와 `oplctl cluster status`에 "nodes run different opLite versions (…); finish the upgrade on every node".
- 합류: 합류 요청에 `version`을 넣는다. 받는 노드는 합류하는 노드의 버전이 클러스터에서 가장 오래된 노드보다 오래되었으면 409 `version_too_old`("this node runs X; the cluster runs Y or newer; upgrade it first"). 버전 비교는 Debian 버전 규칙(`~`가 가장 낮음)의 작은 구현(`opl_types::version::compare`)으로 한다. 버전이 없는 요청(옛 oplctl·opld)은 받는다.

### 3.2 클러스터 설정 `Settings`

```yaml
kind: settings
name: cluster          # 하나만 있다
spec:
  updates: { channel: stable }           # stable | testing (계획 33: enterprise)
status: {}
```

- `Kind::Settings`(`settings`, watch 종류에도). 리소스가 없으면 기본값으로 본다(옛 클러스터를 옮길 일이 없다). 처음 쓰기에서 만든다.
- `GET /api/v1/settings`(로그인한 누구나), `PATCH /api/v1/settings`(admin, JSON merge, 알 수 없는 필드 400). 감사 로그에 남는다.

### 3.3 노드의 APT 소스와 업데이트 확인

opld 설정 `[updates]`:

```toml
[updates]
enabled = true                 # Linux에서 /usr/bin/apt-get이 있으면 켜짐, 아니면(macOS) 꺼짐
check_interval = "24h"
apt_url = ""                   # 비면 빌드 값 OPL_APT_URL, 그것도 비면 소스 파일을 쓰지 않음
sources_file = "/etc/apt/sources.list.d/oplite.sources"
apt_get = "/usr/bin/apt-get"   # 시험이 가짜 스크립트로 바꾼다
systemd_run = "/usr/bin/systemd-run"
systemctl = "/usr/bin/systemctl"
log_dir = "/var/log/opl"
```

- 소스 파일: `apt_url`이 있으면 opld가 시작할 때와 `Settings`가 바뀔 때 deb822 형식으로 쓴다(`Types: deb`, `URIs: <apt_url>`, `Suites: trixie`(빌드 때 정한 코드명), `Components: <channel>`, `Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg`). 첫 줄 `# managed by opld; edits are overwritten`. 이 표시가 없는 파일이면 건드리지 않고 경고를 남긴다(관리자가 직접 관리). 패키지는 저장소 공개 키를 `/usr/share/keyrings/oplite-archive-keyring.gpg`로 싣는다(빌드 때 `OPL_APT_KEY`로 받은 파일, 없으면 싣지 않음).
- 확인: `check_interval`마다(시작 후 처음은 1분 뒤), 그리고 `POST /api/v1/nodes/{name}/updates/refresh`(admin, 그 노드로 전달)로: `apt-get update -q` → `apt-get -s -q dist-upgrade`(LANG=C) 출력의 `Inst <pkg> [<cur>] (<new> <suite> [<arch>])` 줄을 해석한다. 결과를 `NodeStatus.updates`에 쓴다: `{ checked_at, packages: [{ name, current, candidate }] (최대 500개, 넘으면 truncated: true), error: Option<String> }`. 동시에 하나만 돈다(apt 잠금 실패는 error).
- 업그레이드 중이면 확인을 건너뛴다.

### 3.4 업그레이드 Task

- `POST /api/v1/nodes/{name}/upgrade`(admin) → `TaskOp::NodeUpgrade { node }`. 거절: 업데이트가 꺼진 노드 400 `updates_unsupported`, 오프라인 409 `node_offline`, 클러스터에 끝나지 않은 `NodeUpgrade`가 있으면 409 `upgrade_in_progress`(한 번에 한 노드 — 나머지 노드가 과반수와 서비스를 지킨다).
- 그 노드의 Task 실행기가: `systemd-run --unit=opl-upgrade-<task id 앞 8자> --collect --property=Type=exec --setenv=DEBIAN_FRONTEND=noninteractive --setenv=LANG=C /bin/sh -c 'apt-get update -q && apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade' `, 출력은 `StandardOutput=append:<log_dir>/upgrade-<task id>.log`. 그 뒤 2초마다 `systemctl show -p ActiveState -p Result <unit>`를 본다. `inactive`·`Result=success`면 성공, `failed`면 실패(로그 끝 20줄을 Task 오류에).
- opld가 업그레이드로 다시 시작하면(postinst `try-restart`): 노드 시작 때 이 노드의 끝나지 않은 `NodeUpgrade` Task는 실패로 정리하지 않고(다른 Task 종류와 다른 점) 실행기가 같은 유닛을 다시 지켜본다. 유닛이 이미 사라졌으면(`--collect`) 로그 파일의 마지막 줄과 `dpkg --audit`가 비었는지로 판단한다 — 비었으면 성공, 아니면 "the upgrade was interrupted; run `dpkg --configure -a` on the node" 실패.
- 끝나면 업데이트 확인을 한 번 돌리고 `NodeStatus.version`이 새 값이 된다.
- 시간 제한은 없다(큰 업그레이드는 오래 걸린다). 취소도 없다(apt를 중간에 끊으면 더 나쁘다).

### 3.5 APT 저장소 만들기

`packaging/repo/build-repo.sh <repo dir> <channel> <deb…>`: `pool/<channel>/`에 패키지를 넣고 `apt-ftparchive`로 `dists/trixie/<channel>/binary-amd64/Packages(.gz)`와 `Release`를 만들어 `gpg --clearsign`(InRelease)·`--detach-sign`(Release.gpg)으로 서명한다(키는 `OPL_REPO_GPG_KEY_ID`, GPG 홈은 호출하는 쪽). 채널을 하나씩 갱신하고 다른 채널의 `Packages`는 남긴다. 저장소를 어디에 올릴지(웹 서버)는 이 설계 밖이다(README에 레이아웃만).

### 3.6 oplctl과 웹

- `oplctl node updates <node> [--refresh]`: 확인 시각, 패키지 표(PACKAGE CURRENT CANDIDATE), 오류. `oplctl node upgrade <node> [--wait]`. `oplctl settings get`, `oplctl settings set updates.channel=testing`.
- 웹: 노드 표에 버전과 업데이트 수 배지, 노드 상세에 "업데이트" 탭(패키지 표, 다시 확인, 업그레이드 버튼과 확인 대화상자 — "VM은 계속 돌지만 opld가 잠깐 다시 시작합니다. 다른 노드는 이 업그레이드가 끝난 뒤에"), 새 설정 페이지 `/settings`(업데이트 채널). Task 목록에 `node-upgrade`.

### 3.7 시험

- 단위: 버전 비교(`~`, 에포크, 숫자·문자 섞임), `apt-get -s` 출력 해석(실제 출력 견본), deb822 파일 만들기와 관리 표시 확인.
- API 시험: 가짜 `apt-get`·`systemd-run`·`systemctl` 스크립트(임시 디렉터리, 상태를 파일로 흉내)로 확인 → `NodeStatus.updates`, 업그레이드 Task 성공·실패·하나씩, opld를 다시 시작한 것처럼 실행기를 새로 띄워 같은 유닛을 이어 보기, 유닛이 사라진 뒤 판단, 합류 버전 거절.
- CI `package` 작업에 저장소 시험: 임시 GPG 키로 `build-repo.sh` → 컨테이너에서 `file://` 저장소와 `Signed-By`로 `apt-get install oplite`, 버전을 올린 두 번째 패키지를 `testing`에 넣고 소스를 `testing`으로 바꾸면 `apt-get -s dist-upgrade`가 그 버전을 보이는지(opld가 쓰는 해석 함수와 같은 줄 모양인지는 단위 시험의 견본으로).
- `iso.yml`의 E2E는 버전 표시까지만(실제 업그레이드는 가짜 스크립트 시험으로).

## 4. 구독과 텔레메트리 (계획 33)

### 4.1 구독 키

- 형식: `opl1.<payload>.<signature>`. `payload`는 JSON의 base64url(패딩 없음), `signature`는 `opl1.<payload>` 바이트에 대한 Ed25519 서명의 base64url.
- JSON: `{ "id": "<문자열, 1~64자>", "customer": "<1~200자>", "level": "standard" | "premium", "nodes": <위트니스가 아닌 노드 수 한도, 1 이상>, "issued_at": <유닉스 초>, "expires_at": <유닉스 초> }`. 모르는 필드는 무시한다(나중 판이 더할 수 있게).
- 검증: `ring::signature::ED25519`(이미 의존성에 있다)로 내장 공개 키 목록과 비교. 목록은 빌드 값 `OPL_LICENSE_KEYS`(base64 공개 키를 쉼표로), 없으면 저장소의 개발 키 하나(`crates/opl-types/keys/dev-license.pub`). 개발 키로 검증된 키는 상태에 `development: true`가 붙고 웹이 "개발용 키"로 보인다. 개발 키의 비밀 키는 시험 견본(`crates/opl-types/tests/fixtures/dev-license.key`)으로 저장소에 있다 — 정식 빌드는 반드시 `OPL_LICENSE_KEYS`를 준다(README와 패키지 CI가 정식 빌드에서 이 값이 없으면 실패).
- 발급 도구: `opl-license`(새 바이너리, `crates/opl-api/src/bin/`, 패키지에 넣지 않음) `keygen --out <파일>`(PKCS#8 비밀 키와 공개 키 base64 출력), `issue --key <파일> --id … --customer … --level … --nodes … --days …`.
- 저장: `Settings.spec.subscription: Option<String>`(키 문자열). API로 내보낼 때는 키 문자열 대신 상태만.

### 4.2 구독 상태와 효과

- `GET /api/v1/subscription`(로그인한 누구나): `{ state: none | active | expired | invalid | over_limit, id, customer, level, nodes, used_nodes, expires_at, development }`. 상태는 읽을 때 계산한다(현재 시각, 위트니스가 아닌 노드 수). `invalid`는 저장된 키가 지금 빌드의 공개 키로 검증되지 않을 때(빌드가 바뀐 경우).
- `PUT /api/v1/subscription { key }`(admin): 검증 실패 400 `invalid_subscription_key`(이유: 형식, 서명, 만료). 만료된 키는 받지 않는다. `DELETE /api/v1/subscription`(admin). 감사 로그에 키 id만.
- 기능은 막지 않는다. 효과는 셋이다.
  1. 웹: 상태가 `active`가 아니면 대시보드 위에 닫을 수 있는 안내(닫으면 그 브라우저에서 7일 동안 숨김 — localStorage), `expired`·`over_limit`는 경고 색. 만료 30일 전부터 "곧 만료".
  2. `enterprise` 채널: `PATCH /settings`로 `updates.channel = enterprise`는 `active`일 때만(아니면 400 `subscription_required`). 노드는 소스 파일의 `Components: enterprise`와 함께 `/etc/apt/auth.conf.d/oplite.conf`(0600, `machine <apt_url의 호스트> login <id> password <키의 SHA-256 16진수>`)를 쓴다. 다른 채널로 바꾸거나 구독을 지우면 그 파일을 지운다. 구독이 끝나도 채널은 그대로 두고, 저장소가 401을 주면 업데이트 확인 오류에 보인다. 저장소 쪽 인증 확인은 이 설계 밖(README에 방식만).
  3. `oplctl subscription`·웹 설정 페이지에 상태 표시.

### 4.3 텔레메트리

- `Settings.spec.telemetry: { enabled: false, endpoint: Option<String> }`, `Settings.status.telemetry: { id, last_sent_at, last_error }`. `endpoint`가 비면 빌드 값 `OPL_TELEMETRY_URL`, 그것도 비면 켤 수 없다(400 `telemetry_endpoint_required`). `https://`만.
- `id`: 처음 켤 때 만드는 무작위 UUID v4. 클러스터 이름·CA와 관계없다. 끄면 그대로 두고, `oplctl telemetry reset-id`로 새로 만든다.
- 리더의 `TelemetryReporter`(백업 작업 스케줄러와 같은 모양): 켜져 있으면 마지막 전송에서 24시간(opld 설정 `[telemetry] interval`) 지나면 보낸다. 실패하면 1시간 뒤 다시. 리더가 바뀌어도 `last_sent_at`이 Raft에 있어 두 번 보내지 않는다. `POST <endpoint>`, `Content-Type: application/json`, 시간 제한 30초, 프록시는 `HTTPS_PROXY` 환경 변수.
- 보내는 내용(`schema: 1`): `id`, `sent_at`, `versions`(버전별 노드 수), `nodes`(`total`, `witness`), `arch`·`hypervisor`별 노드 수, CPU 합과 메모리 합(GiB, 정수), `vms`(`total`, `running`, `ha`), 풀 종류별 수, 네트워크 종류별 수, 보안 그룹 수, 백업 작업 수, 가져오기 수(원본 종류별), 구독 `level`(없으면 `none`). 이름, 주소, MAC, 키, 사용자, 고객 이름은 넣지 않는다.
- `GET /api/v1/telemetry/preview`(admin): 지금 보낼 JSON 그대로(꺼져 있어도). 웹 설정 페이지가 켜기 전에 이것을 보인다.
- `PATCH /settings`의 `telemetry` 바꾸기는 admin, 감사 로그.

### 4.4 oplctl과 웹

- `oplctl subscription show|set <key|--file>|remove`, `oplctl telemetry show|enable [--endpoint]|disable|preview|reset-id`, `opl-license keygen|issue`.
- 웹 `/settings`: 구독(상태 카드, 키 넣기·지우기), 업데이트 채널(enterprise는 구독이 있을 때만 고를 수 있음), 텔레메트리(켜기 전 미리 보기 JSON, 마지막 전송·오류). 대시보드 안내 배너.

### 4.5 시험

- 단위: 키 해석·서명 검증(개발 키 견본, 바뀐 바이트, 다른 키, 만료, 모르는 필드), 상태 계산(노드 수·위트니스 제외·30일), 텔레메트리 내용(이름·주소가 없음을 확인).
- API: 구독 넣기·지우기·권한·상태, enterprise 채널 거절과 auth 파일 쓰기·지우기(임시 경로), 텔레메트리 켜기 거절(끝점 없음), 미리 보기, 시험 안의 HTTPS 서버(계획 30의 자체 서명 서버 도우미, 리포터 설정의 추가 루트 인증서 — API·opld 설정에는 없고 시험만 넘긴다)로 리포터가 한 번 보내고 다음은 시간이 지나야 보내는지, 실패 뒤 다시.
- opld E2E(`cargo test -p opld`): 실제 opld 둘에서 구독 넣기 → 상태 `active`, 텔레메트리 켜기 → 리더가 보냄(가짜 끝점), 리더를 끄면 다른 노드가 이어서 보내되 24시간 전에는 다시 보내지 않음.
- 웹 Vitest: 설정 페이지, 배너와 숨기기, 업데이트 탭.

## 5. 오류

| 코드 | HTTP | 언제 |
|---|---|---|
| `version_too_old` | 409 | 클러스터보다 오래된 노드의 합류 |
| `updates_unsupported` | 400 | 업데이트가 꺼진 노드(macOS, apt 없음)의 확인·업그레이드 |
| `upgrade_in_progress` | 409 | 다른 노드 업그레이드가 끝나지 않음 |
| `subscription_required` | 400 | 구독 없이 enterprise 채널 |
| `invalid_subscription_key` | 400 | 형식·서명·만료 |
| `telemetry_endpoint_required` | 400 | 끝점 없이 텔레메트리 켜기 |

## 6. 나누기

- 계획 31: 2장 전체와 `opl_types::version::BUILD`(`--version`만). Rust 변경은 작고, 나머지는 `packaging/`과 CI다. 스테이지 검증은 macOS에서 Rust·웹, 패키지·ISO는 CI 프로브에서(`iso.yml`도 프로브 브랜치에서 돈다).
- 계획 32: 3장. 계획 31의 패키지에 소스 파일·키링 자리를 더한다.
- 계획 33: 4장. 계획 32의 `Settings`와 소스 파일 쓰기를 넓힌다.

## 7. 알려진 틈

- 구독 키를 여러 클러스터에 써도 막지 못한다(오프라인 검증).
- 소스에서 빌드한 opLite는 자기 공개 키를 넣어 아무 키나 만들 수 있다 — 구독은 기능 잠금이 아니므로 의도한 것이다.
- 업그레이드 중 노드 전원이 꺼지면 dpkg가 반쯤 설정된 채 남을 수 있다(실패 Task가 `dpkg --configure -a`를 안내).
- 업그레이드 순서와 VM 비우기는 관리자가 정한다(자동 롤링 없음).
- 저장소 호스팅, enterprise 저장소의 인증 확인, 텔레메트리 수집 서버는 opLite 밖이다.
