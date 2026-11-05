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
- postinst `configure`: `systemctl daemon-reload`, 처음 설치면 `systemctl enable opld.service`(시작은 설정이 있을 때 유닛 조건이 정함 — `systemctl start`를 부르고 실패는 무시), 업그레이드·재설정(`abort-remove`·`abort-deconfigure` 포함)이면 `systemctl is-enabled`일 때 `systemctl restart opld.service`(debhelper와 같다).
- prerm `remove`: `systemctl stop opld.service`(VM은 scope에 남는다 — 패키지를 지워도 VM은 돌고, 다시 설치한 opld가 이어 맡는다), `upgrade`에서는 멈추지 않는다(postinst가 다시 시작).
- postrm: `purge`여도 `/var/lib/opl`(Raft 데이터, 풀, 볼륨)와 `/etc/opl`을 지우지 않는다. 데이터를 지우는 것은 관리자의 일이다. `daemon-reload`만 한다.
- 빌드 버전: `build-deb.sh`가 `OPL_VERSION=<version>`으로 빌드하게 README·CI가 같은 명령을 쓴다(계획 32의 빌드 버전, 계획 31에서는 `opld --version`·`oplctl --version`이 이 값을 보인다 — `clap` `version`에 `opl_types::version::BUILD`).

### 2.2 설치 ISO

`packaging/iso/build-iso.sh --netinst <debian netinst.iso> --deb <oplite.deb> --out <oplite.iso> [--unattended <test.cfg>]`.

1. netinst ISO의 SHA-256을 `packaging/iso/debian.lock`(URL과 해시, Debian 13 amd64 netinst 한 판)과 비교한다. CI는 이 파일로 내려받는다.
2. `xorriso -osirrox`로 풀고 `/oplite/`에 패키지, `/preseed/oplite.cfg`, `/preseed/oplite-setup.sh`를 넣는다.
3. 부트 메뉴(BIOS `isolinux/txt.cfg`, UEFI `boot/grub/grub.cfg`) 맨 위에 "Install opLite" 항목을 더해 기본으로 한다: `auto=true priority=high preseed/file=/cdrom/preseed/oplite.cfg`. 원래 항목은 그대로 남는다.
4. `xorriso -indev <netinst> -outdev <out> -map <dir> / -boot_image any replay`로 원래 ISO의 BIOS·UEFI 하이브리드 부트를 유지해 다시 묶는다. 볼륨 이름은 `OPLITE`.

preseed(`oplite.cfg`)는 질문을 줄이되 다음은 사람에게 묻는다: 네트워크(필요하면 — DHCP가 실패하면 수동), 호스트 이름, 도메인, root 비밀번호, 설치 디스크. 나머지는 정한다: 언어·키보드(en_US·us), 시간대 UTC(설치 뒤 `timedatectl set-timezone`), 파티션은 고른 디스크 전체에 LVM(`atomic` 레시피 — `/`, swap), 미러 `deb.debian.org`, 패키지 `standard`·`ssh-server`, 일반 사용자 만들지 않음, `popularity-contest` 끔, GRUB은 고른 디스크에.

`preseed/late_command`가 `oplite-setup.sh`를 대상 시스템에서 돌린다.
- 패키지 `/cdrom/oplite/*.deb`를 `/target/var/cache/oplite/`로 복사하고 `in-target apt-get install -y ifupdown2 /var/cache/oplite/oplite_*.deb`.
- 네트워크: 설치가 만든 `/etc/network/interfaces`의 기본 경로 인터페이스(DHCP면 `inet dhcp`, 정적이면 주소·게이트웨이 그대로)를 `vmbr0`로 옮긴다: `auto vmbr0`, `iface vmbr0 inet …`, `bridge-ports <nic>`, `bridge-stp off`, `bridge-fd 0`, `bridge-vlan-aware yes`, `bridge-vids 2-4094`, NIC는 `iface <nic> inet manual`. 인터페이스를 못 찾으면 바꾸지 않고 `/root/oplite-setup.log`에 남긴다.
- `/etc/opl/opld.toml`: `name = "<호스트 이름>"`, `address = "<vmbr0이 될 주소>"`(정적이면 그 주소, DHCP면 설치 때 받은 주소 — DHCP 주소가 바뀌면 고쳐야 한다고 파일 주석과 README에), 나머지는 예시 주석.
- `/etc/issue.d/oplite.issue`: 콘솔 로그인 화면에 "opLite: https://\4{vmbr0}:8006/ — 첫 노드는 `oplctl cluster init --name <이름>`, 다른 노드는 `oplctl cluster join <URL>`".

`--unattended <cfg>`(시험 전용)는 묻는 값을 모두 정한 preseed(호스트 이름 `opl-ci`, root 비밀번호, 첫 디스크, DHCP)를 추가로 넣고, BIOS는 메뉴(vesamenu) 없이 직렬 콘솔로 opLite 항목을 바로 부팅하고 UEFI는 부트 메뉴 시간 제한을 1초로, 그리고 첫 부팅에 한 번 도는 유닛 `oplite-ci-init.service`가 `oplctl cluster init --name ci`의 출력을 직렬 콘솔(`/dev/ttyS0`)에 쓰게 한다. 커널 명령줄에 `console=ttyS0`를 더한다.

### 2.3 시험

- 셸 스크립트는 `shellcheck`로 본다(CI).
- CI `ci.yml`에 `package` 작업(ubuntu): 릴리스 빌드(`web-ui`, 웹 빌드 포함) → `build-deb.sh` → `dpkg-deb --info`·`--contents`로 경로 확인 → `docker run debian:13`에서 `apt-get install ./oplite_*.deb`(의존성까지 받음), `opld --version`·`oplctl --version`이 그 버전, `systemd-analyze verify /lib/systemd/system/opld.service`, 설정 없이 유닛 조건이 거짓인지. 패키지는 아티팩트로 올린다.
- 새 워크플로 `iso.yml`(수동 실행, 주 1회, `ci-probe/**`·`v*` 태그 push): 패키지 → netinst 내려받기(`debian.lock` 해시 확인) → `build-iso.sh --unattended` → KVM으로 빈 디스크 20 GiB에 무인 설치(`-boot d`, 직렬 콘솔을 파일로, 시간 제한 40분) → 디스크로 다시 부팅 → 직렬 로그에서 `cluster init` 출력(CA 지문, admin 비밀번호)을 찾고, QEMU user 네트워크의 `hostfwd`로 연 8006에 `oplctl login`과 `oplctl node list`(노드 `opl-ci`, `online`)와 `oplctl network list`가 되는지, `ip -d link show vmbr0`는 직렬 콘솔이 없으므로 대신 노드 상태의 `bridges`에 `vmbr0`이 있는지 본다. 실패하면 직렬 로그를 아티팩트로 올린다.
- macOS에서는 `dpkg-deb`·`xorriso`가 없어 스크립트 실행 시험을 하지 않는다(CI만).

### 2.4 알려진 한계

설치에 인터넷(Debian 미러)이 필요하다. DHCP로 설치하면 `address`가 그 순간의 주소다. ZFS·RAID 루트, arm64 ISO, 설치 화면에서 클러스터 만들기·합류는 없다. 패키지를 지워도 데이터는 남는다.

### 2.5 구현(계획 31)

- 빌드 버전: `opl_types::version::BUILD`(빌드할 때 `OPL_VERSION`, 없거나 비었으면 `CARGO_PKG_VERSION`), opld·oplctl `--version`이 `<바이너리> <BUILD>`. oplctl이 opl-types에 의존한다. 시험은 opl-types 단위와 각 크레이트 `tests/version.rs`이고, 기대값을 `BUILD`가 아니라 시험을 빌드할 때의 `OPL_VERSION`으로 따로 구한다. CI `package`가 `OPL_VERSION=9.9.9~t`로 이 시험을 다시 돌려 clap 기본 `version`으로 되돌리면 실패한다.
- 패키지 `packaging/deb/`: `build-deb.sh <version> <out dir>`(`umask 022`, `DEBIAN/md5sums`, `opld --version`·`oplctl --version`이 `<바이너리> <version>`이 아니면 `OPL_VERSION`으로 다시 빌드하라는 안내와 함께 1, 바이너리는 기본 `target/release`, `OPL_BIN_DIR`로 바꿈, 만든 경로 `<out dir>/oplite_<version>_amd64.deb`를 출력, `dpkg-deb --build --root-owner-group -Zxz`, `Installed-Size`는 `du -sk`), `control.in`(`@VERSION@`·`@INSTALLED_SIZE@`), `copyright`, `opld.service`, `opld.toml.example`, `postinst`·`prerm`·`postrm`. 설계와 다른 점:
  - 유닛 경로는 `/usr/lib/systemd/system/opld.service`(merged /usr인 Debian 13의 자리). 유닛에 `Description=opLite node daemon`, `Documentation=https://github.com/gaiger777/opLite`, `WantedBy=multi-user.target`.
  - `Depends` 앞에 `libc6 (>= 2.39), libssl3t64`(ubuntu-24.04의 glibc로 빌드하고 native-tls로 OpenSSL에 링크한다). `Maintainer: opLite developers <gaiger777@gmail.com>`, `Homepage: https://github.com/gaiger777/opLite`.
  - maintainer 스크립트는 systemd가 도는지를 `${OPL_SYSTEMD_DIR:-/run/systemd/system}`로 본다. 돌지 않으면(컨테이너, 설치 프로그램의 in-target chroot) postinst는 처음 설치일 때 `systemctl enable`(링크만)만 하고 나머지 `systemctl` 호출을 건너뛴다. 업그레이드·재설정(remove 뒤 다시 설치, `abort-remove`·`abort-deconfigure`)은 `try-restart`가 아니라 enable돼 있으면 `restart`다(멈춘 opld도 다시 시작한다. VM은 scope에 있어 영향이 없다). 모든 호출은 `|| true`. prerm은 `remove`와 `deconfigure`에서 stop. postrm은 `purge`에서 `systemctl disable`과 `rm -f ${OPL_SYSTEMD_ETC:-/etc/systemd/system}/multi-user.target.wants/opld.service`(링크 정리)도 한다.
  - `opld.toml.example`은 README "실행 (Linux, KVM)"의 설정과 같고 맨 위에 복사 안내 주석.
- ISO `packaging/iso/`: `build-iso.sh --netinst <iso> --deb <deb> --out <iso> [--unattended]`(bash). `--unattended`는 인자 없는 플래그이고 저장소의 `preseed/unattended.cfg`를 쓴다. `--deb`의 파일 이름이 `oplite_*_amd64.deb`가 아니면 2(설치가 그 이름을 찾는다). 끝에 출력 경로를 찍는다. `OPLITE_ISO_LIB=1`로 source하면 함수만(`lock_value`, `verify_netinst`, `patch_isolinux`, `patch_grub`, `check_patched`, `sums_update`, `md5_update`, `sha256_update`). `debian.lock`은 셸 `KEY=value`(`DEBIAN_RELEASE=13.7.0`, `DEBIAN_CODENAME=trixie`, `DEBIAN_NETINST_URL`, `DEBIAN_NETINST_SHA256`)이고, 13.7.0이 아직 `cdimage/archive/`에 없어 `release/` URL이다(다음 판이 나오면 404가 되므로 `cdimage/archive/<판>/`에서 받는다 — CI가 그렇게 한다. lock은 archive URL로 바꾸거나 판을 올린다). 해시는 `sha256sum`, 없으면 `shasum -a 256`.
  - BIOS 메뉴: `isolinux/txt.cfg`의 첫 `label` 앞에 `label oplite`(kernel·append는 install 항목에서 복사, preseed 인자는 `---` 앞)와 `menu default`를 넣고, `gtk.cfg`의 `menu default`를 뺀다. `menu.cfg`가 `gtk.cfg`를 먼저 include하므로 opLite 항목은 기본이지만 두 번째 줄이다. `isolinux.cfg`의 `default vesamenu.c32`는 바꾸면 메뉴가 사라지므로 그대로 둔다. unattended면 `isolinux.cfg`를 메뉴 없이 새로 쓴다: `serial 0 115200`, `default oplite`, `prompt 0`, `timeout 1`, `label oplite`(txt.cfg 항목과 같은 kernel·append). CI 프로브에서 vesamenu가 화면 없이(`-display none`) 기다려 BIOS 설치가 직렬 출력 하나 없이 시간 제한에 걸렸다.
  - UEFI 메뉴: `boot/grub/grub.cfg`의 첫 `menuentry` 앞에 `set default=0`, unattended면 `set timeout=1`, `menuentry 'Install opLite'`(kernel·initrd는 텍스트 install 항목에서).
  - 패치 뒤 `check_patched`가 `label oplite`·`Install opLite`(unattended면 `isolinux.cfg` 첫 줄 `serial 0 115200`·`default oplite`와 `set timeout=1`)를 확인하고 없으면 실패한다. 13.7.0 ISO의 실제 `isolinux.cfg`·`txt.cfg`·`gtk.cfg`·`grub.cfg`가 시험 견본(`packaging/tests/fixtures/iso/`)이다.
  - 다시 묶기(Debian 위키 RepackBootableISO): ISO 전체를 풀지 않고 고칠 파일(`isolinux/txt.cfg`·`gtk.cfg`·`isolinux.cfg`, `boot/grub/grub.cfg`, `md5sum.txt`)만 `-extract`로 꺼낸다. `md5sum.txt`는 원래 목록에서 `grub.cfg`의 합을 바꾸고 새 파일(`oplite/`, `preseed/`)을 더한다(`isolinux/`는 원래처럼 없다). `sha256sum.txt`는 따로 꺼내 보고 있으면 같은 방식으로 고쳐 얹고, 없으면 건너뛴다. 그다음 `xorriso -indev <netinst> -outdev <out> -overwrite on -map <dir>/oplite /oplite -map <dir>/preseed /preseed -map <고친 파일> /<경로> … -boot_image any replay -volid OPLITE`. 부트 이미지와 `boot.cat`은 원래 것을 그대로 둔다(트리 전체를 `/`에 `-map`하면 El Torito 부트 이미지가 ISO의 데이터 파일이 아니게 되어 실패했다). EXIT trap은 전역 `ISO_WORK`를 지운다.
  - 부트 인자: 일반 `auto=true priority=high preseed/file=/cdrom/preseed/oplite.cfg`, unattended `auto=true priority=critical preseed/file=/cdrom/preseed/unattended.cfg console=ttyS0,115200n8`.
- preseed: `preseed/oplite.cfg`(위 §2.2, `partman-auto-lvm/guided_size max`, 시간대 `Etc/UTC`·`clock-setup/utc`, `apt-setup/cdrom/set-first`·`set-next`·`set-failed` false(APT 소스에 설치 CD를 넣지 않는다), 마지막 `preseed/late_command string sh /cdrom/preseed/oplite-setup.sh /target`), `grub-installer/bootdev`는 정하지 않는다. `preseed/unattended.cfg`(첫 줄 `preseed/include string oplite.cfg`, 호스트 `opl-ci`, root 비밀번호 `opl-ci-root`(시험 전용), `partman-auto/disk`·`grub-installer/bootdev` `/dev/vda`, NTP 끔, `debian-installer/exit/poweroff`로 끝나면 전원 끔). 설계와 다른 점:
  - 언어·키보드(en_US·us)는 묻지 않고 preseed로 정한다.
  - 묻는 것은 네트워크(필요하면)·호스트 이름·도메인·root 비밀번호·디스크다.
  - 시간대는 UTC다(설치 뒤 `timedatectl set-timezone`).
  - unattended BIOS는 vesamenu 없이 직렬 콘솔로 뜬다.
- `preseed/oplite-setup.sh <target>`(POSIX sh, 로그 `<target>/root/oplite-setup.log`): `install_package`(`/cdrom/oplite/oplite_*.deb`를 `<target>/var/cache/oplite/`로 복사해 `in-target --pass-stdout sh -c 'DEBIAN_FRONTEND=noninteractive apt-get -y -q -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install ifupdown2 isc-dhcp-client /var/cache/oplite/<deb> </dev/null 2>&1'`. in-target의 stdin은 설치 프로그램의 debconf 통로라 apt·dpkg가 거기서 답을 기다리면 무인 설치가 멈추므로 질문 없이 stdin 없이 돌린다. 그동안 대상에 `policy-rc.d`(exit 101)를 두고 끝나면(실패해도) 지운다. `<target>/run/systemd/system`이 있으면 로그에 남긴다), `default_nic`·`convert_interfaces <in> <out>`(awk; `inet dhcp`이거나 `gateway`가 있는 lo 아닌 첫 `inet` 블록, 없으면 lo 아닌 첫 `inet static` 블록, 그 블록과 같은 NIC의 `inet6` 블록을 `vmbr0`으로, `allow-hotplug`는 `auto`로, 못 찾으면 그대로 쓰고 2), `setup_network <target>`(late_command는 netcfg가 설정을 `/target`으로 복사하는 `55netcfg-copy-config`보다 먼저 돌므로 원본은 설치 환경의 `/etc/network/interfaces`이고 lo 아닌 iface가 없으면 대상의 것, 결과는 두 곳 모두에 쓴다. 설치 환경 루트는 `OPLITE_INSTALLER_ROOT`), `node_name <target>`(설치 환경, 없으면 대상의 `/etc/hostname`을 소문자로), `valid_name`(소문자·숫자·`-`, 1~63자, 앞뒤 `-` 아님), `primary_address`(vmbr0의 정적 주소, `/prefix` 뗌), `current_address <nic>`(DHCP면 설치 환경의 `ip -4 -o addr show dev`), `write_opld_toml <out> <name> <address> [dhcp]`(영어 주석: 설치가 쓴 파일과 예시 위치, 클러스터에 들어간 뒤에는 주소를 바꿀 수 없다는 것, DHCP면 `# This address came from DHCP.`. 이름이 규칙에 맞지 않거나 주소가 비면 그 줄을 주석으로 두고 로그를 남긴다), `write_issue`(`/etc/issue.d/oplite.issue`, 영어 ASCII 네 줄: 웹 UI 주소 `https://\4{vmbr0}:8006/`, 첫 노드 `oplctl cluster init`, 다른 노드 `oplctl cluster join-token`과 `oplctl cluster join`). `/cdrom/preseed/oplite-ci-init.service`가 있으면 복사하고 `in-target systemctl enable`. `main`은 단계(`install-package`, `network`, `config`, `ci-init`)마다 `step`으로 시작·끝을 로그와 설치 환경 콘솔(`/dev/console`, 무인 설치에서는 직렬)에 `oplite-setup: start|done <단계>`로 쓰고, 실패하면 EXIT trap이 `oplite-setup: FAILED <단계> (exit N)`과 그 단계 출력 끝 30줄을 콘솔에 쓴다(CI 프로브에서 late_command가 직렬에 아무것도 남기지 않고 멈췄다). 시험이 바꾸는 경로: `OPLITE_CDROM`, `OPLITE_CONSOLE`, `OPLITE_INSTALLER_ROOT`. `OPLITE_SETUP_LIB=1`이면 함수만.
- `preseed/oplite-ci-init.service`(시험 전용 oneshot, `After=`·`Requires=opld.service`, `ConditionPathExists=!/var/lib/oplite-ci-init.done`): 제어 소켓 `/run/opl/opld.sock`(opld 기본 `run_dir`, oplctl `--socket` 기본값)을 2초 간격 60번까지 기다리고 `oplctl cluster init --name ci`, 성공했을 때만 완료 표시(`&&` 사슬, 셸의 `$`는 `$$`). 먼저 `ExecStartPre=-/bin/cat /root/oplite-setup.log`로 설치 로그를 찍는다. 출력은 `/dev/ttyS0`.
- 시험(`packaging/tests/`, bash, macOS에서도 돈다): `build-deb-test.sh`(가짜 `opld`·`oplctl`·`dpkg-deb`로 버전이 같으면 만들고 다르면 1, 4경우), `maint-test.sh`(가짜 `systemctl`로 처음 설치(systemd 있음·없음)·업그레이드(enable 여부)·remove 뒤 configure·abort-remove·abort-deconfigure·remove·deconfigure·purge(링크 지움) 등의 호출 목록, 26경우), `setup-test.sh`(함수를 `OPL_SH`(기본 `sh`)로 source해 부른다. `fixtures/interfaces-{dhcp,static,static-nogw,hotplug,none,ipv6}.{in,out}`, `fixtures/opld-{static,dhcp,badname,noaddr}.toml`, `fixtures/oplite.issue`를 `diff -u`로 비교, ASCII 확인, 이름 규칙, 설치 환경·대상 interfaces와 호스트 이름, 단계 콘솔 기록과 실패, 가짜 `in-target`으로 `install_package`(apt 명령·`policy-rc.d`), 가짜 `in-target`·`ip`로 `main` 전체, 64경우), `iso-test.sh`(인자 검사(`--deb` 이름 포함)·해시 확인·`debian.lock` 모양, 메뉴 패치 견본 비교(일반·unattended), `md5_update`·`sha256_update`, 가짜 `xorriso`·`sha256sum`으로 `main`의 꺼내기·다시 묶기 인자, 얹은 `md5sum.txt`, `sha256sum.txt`가 있을 때와 없을 때, 실패 때 종료 코드·작업 디렉터리 정리, 39경우). `deb-test.sh <deb>`는 Debian 13 컨테이너 안에서만 돈다(설치, `--version`, `md5sums`·`dpkg --verify`, `systemd-analyze verify`, 경로, enable 링크, `dpkg --purge` 뒤 링크 없음·`/var/lib/opl`·`/etc/opl` 남음).
- CI: `ci.yml`에 작업 `shell`(`shopt -s globstar`로 `shellcheck packaging/**/*.sh`와 maintainer 스크립트, 위 네 시험, busybox를 설치해 `OPL_SH="busybox sh" OPL_AWK="busybox awk"`로 `setup-test.sh`를 한 번 더)과 `package`(composite action `.github/actions/build-deb`(`id: build`, 경로는 `steps.build.outputs.deb`) → `dpkg-deb --info`·`--contents` → `docker run debian:13 … deb-test.sh` → `OPL_VERSION=9.9.9~t cargo test -p opl-types -p opld -p oplctl --lib --test version` → 아티팩트 `oplite-deb`). `.github/actions/build-deb`는 pnpm·node 22로 웹 빌드, `OPL_VERSION=<Cargo.toml 워크스페이스 버전>~ci<run 번호>`를 `GITHUB_ENV`에 두고 `cargo build --release -p opld --features web-ui -p oplctl`, `build-deb.sh "$OPL_VERSION" dist`, 출력 `deb`. `.github/workflows/iso.yml`의 작업 `iso-e2e`(행렬 `firmware: [bios, uefi]`, `fail-fast: false`, 100분; 수동 실행, 월요일 03:00 UTC, `ci-probe/**`·`v*` 태그 push. uefi는 설치와 부팅 둘 다 `OVMF_CODE_4M.fd`(없으면 `OVMF_CODE.fd`)를 읽기 전용 pflash로, VARS는 작업 디렉터리의 복사본으로): 패키지 → netinst(`actions/cache`, `release/` URL이 404면 `cdimage/archive/<판>/`, `sha256sum -c`) → `build-iso.sh --unattended` → 20 GiB qcow2에 KVM 무인 설치(`-no-reboot`, 직렬 `install.log`. QEMU를 백그라운드로 띄우고 30초마다 보아 끝나면 다음, 직렬에 `oplite-setup: FAILED`가 보이거나 40분이 지나면 QEMU 모니터로 `screendump`(`install-screen.ppm`)를 남기고 실패) → 디스크로 부팅(`hostfwd=tcp::18006-:8006`, 직렬 `boot.log`) → 직렬 로그에서 CA 지문·admin 비밀번호(15분, 그동안 QEMU가 끝나거나 `error:` 줄·유닛 실패가 보이면 바로 실패) → `oplctl login`·`node list`(`opl-ci` online)·`node get opl-ci`의 `bridges`에 `vmbr0`·`network list` → 로그 아티팩트 `iso-e2e-logs-<firmware>`.

## 3. 버전과 업데이트 (계획 32)

### 3.1 빌드 버전과 노드 버전

- `opl_types::version::BUILD: &str` = `option_env!("OPL_VERSION")`, 없으면 `CARGO_PKG_VERSION`. opld·oplctl의 `--version`, API `GET /api/v1/version`(인증 없이, `{ "version": … }` — 로그인 화면과 oplctl이 서버 버전을 본다).
- `NodeStatus.version: Option<String>`(옛 노드는 None, 빼고 쓴다). 노드가 등록·상태 갱신 때 `BUILD`를 쓴다.
- `oplctl node list`에 `VERSION`, 웹 노드 표에 버전 열. 노드 버전이 둘 이상이면 웹 대시보드와 `oplctl cluster status`에 "nodes run different opLite versions (…); finish the upgrade on every node".
- 합류: 합류 요청에 `version`을 넣는다. 받는 노드는 합류하는 노드의 버전이 클러스터에서 가장 오래된 노드보다 오래되었으면 409 `version_too_old`("this node runs X; the cluster runs Y or newer; upgrade it first"). 버전 비교는 Debian 버전 규칙(`~`가 가장 낮음)의 작은 구현(`opl_types::version::compare`)으로 한다. 버전이 없는 요청(옛 opld)은 클러스터에 버전을 적은 노드가 하나도 없을 때만 받는다(구현에서 바꿈, §3.8).

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
- opld가 업그레이드로 다시 시작하면(postinst `restart`): 노드 시작 때 이 노드의 끝나지 않은 `NodeUpgrade` Task는 실패로 정리하지 않고(다른 Task 종류와 다른 점) 실행기가 같은 유닛을 다시 지켜본다. 유닛이 이미 사라졌으면(`--collect`) 로그 파일의 마지막 줄과 `dpkg --audit`가 비었는지로 판단한다 — 비었으면 성공, 아니면 "the upgrade was interrupted; run `dpkg --configure -a` on the node" 실패.
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

### 3.8 구현(계획 32)

- 버전: `opl_types::version::{compare, SUITE, APT_URL}`. `compare`는 Debian 규칙(에포크, 업스트림, 마지막 `-` 뒤 리비전, `~` < 끝 < 문자 < 그 밖, 숫자는 앞 0을 무시한 수 비교)이고 이상한 문자열에도 패닉하지 않는다. `SUITE = "trixie"`, `APT_URL`은 빌드 때 `OPL_APT_URL`(없으면 ""). `GET /api/v1/version`(`opl_api::cluster::get_version`, 인증 없음) → `VersionInfo { version }`. `NodeStatus.version`은 `register_node`·`register_witness`가 `BUILD`로 쓴다(등록 때, 그리고 업데이트가 켜진 노드는 확인·`UpdateAgent` 시작 때도 — opld가 다시 시작하면 새 값). 합류를 받는 쪽도 Node를 만들거나 다시 쓸 때 요청의 `version`을 적어, 등록 전에도 버전 검사와 옛 노드 게이트가 그 노드를 센다. `GET /cluster`의 `MemberView.version`도 더했다(웹 대시보드 경고가 쓴다).
- 합류: `JoinRequest.version`(`opld join::request`가 `BUILD`를 보냄). 토큰 검사 뒤(토큰을 쓰기 전), 이름 검사 앞에서 클러스터 노드(위트니스·오프라인 포함, 합류하는 노드 자신의 옛 기록은 뺌) 중 버전이 있는 가장 낮은 것과 비교한다. 설계와 다른 점: 버전을 보내지 않는 요청도 버전을 적은 노드가 있으면 409 `version_too_old`("this node runs an unknown version; the cluster runs Y or newer; upgrade it first")다. 옛 opld는 새 리소스·Task 종류가 든 Raft 항목을 풀지 못하기 때문이다(노드 코어 설계의 호환 규칙). 버전을 적은 노드가 없는 옛 클러스터는 받는다.
- 옛 노드 게이트: 같은 이유로 버전을 적지 않은 노드가 하나라도 있으면 새 종류를 쓰는 `PATCH /settings`·`POST /nodes/{key}/updates/refresh`·`POST /nodes/{key}/upgrade`가 409 `cluster_upgrading`("some nodes run an older opLite (a, b); finish the upgrade on every node first" — 버전 없는 노드 이름을 이름 순으로)이다(`opl_api::cluster::require_upgraded`). 계획 32 전 클러스터의 첫 업그레이드는 노드마다 손으로 apt를 돌린다.
- `Settings`(`opl_types::settings`, 이름은 언제나 `SETTINGS_NAME = "cluster"`): 리소스가 없으면 `GET`이 nil id·revision 0·기본 spec의 리소스 모양을 돌려준다(`opl_api::settings::current`, `UpdateAgent`도 이것으로 읽는다). 설계와 다른 점:
  - `PATCH`는 JSON merge patch가 아니라 기존 Patch 관례다: `SettingsPatch { updates: Option<{ channel: Option<Channel> }> }`(`deny_unknown_fields`). `{}`는 400, `{"updates":{}}`는 쓰지 않고 200. 없으면 `create`, 있으면 `update_spec`. `If-Match`가 있으면 그 revision(없는 설정은 0)이 아니면 412(`If-Match: 0`으로 함께 만들다 진 요청도 409 `name_taken`이 아니라 412), 없으면 `RevisionMismatch`·`NameTaken`에 10번 다시 한 뒤 409 `busy`. 같은 값이면 쓰지 않는다.
  - 감사 로그가 코드베이스에 없으므로 남기지 않는다.
- `[updates]`(opld `UpdatesSection`): 설계의 키에 `first_check`(기본 "1m"), `dpkg`(`/usr/bin/dpkg`), `poll`(기본 "2s")을 더했다. `enabled`가 없으면 `apt_get`이 있을 때 켜진다. 꺼지면 `opl_api::UpdatesConfig`·`Updates`가 아예 없다(`TaskRunnerConfig.updates: None`). 같은 `Arc<Updates>`를 Task 실행기와 `UpdateAgent`가 쓴다. opld 전체 프로세스 시험의 데몬은 `enabled = false`다(Ubuntu CI 러너에 진짜 `apt-get`이 있다).
- `NodeStatus.updates: Option<NodeUpdates { checked_at: Option<u64>(유닉스 초 — 다른 상태 시각과 같다), packages: [PackageUpdate { name, current: Option, candidate }], truncated, error }>`. 업데이트가 켜진 노드는 등록 때 `Some`(있던 결과는 `record_node`가 남김), 꺼진 노드·위트니스는 None이고 None이 `updates_unsupported`의 뜻이다. 쓰기는 노드 상태를 revision 확인하며 다시 쓴다.
- `opl_api::updates`: `Updates::check`(`apt-get update -q` → `LANG=C apt-get -s -q dist-upgrade`의 `Inst` 줄 — 둘 다 `-o DPkg::Lock::Timeout=60`으로 다른 apt의 잠금을 60초까지 기다림, `[<cur>]`이 없으면 새 의존성으로 current None, 500개 넘으면 truncated; 실패는 명령과 stderr 끝 5줄; 명령은 `kill_on_drop`; 주기 확인은 잠금이 잡혀 있으면 기다리지 않고 "another update check is running", 다시 확인 Task는 잠금 줄에 서지 않고 `poll`마다 다시 해 보며, 이 노드의 업그레이드가 잠금을 잡고 있으면 확인하지 않고 성공한다 — 업그레이드가 끝날 때 확인한다), `sources_contents`·`write_sources`(`<파일>.dpkg-tmp`에 0644로 써서 `fsync` 뒤 바꿔 넣고 디렉터리도 `fsync`, 같으면 `Unchanged`, 관리 표시 없는 파일은 `Unmanaged`와 `warn!` — 비었거나 공백뿐인 파일은 관리하는 것으로 봄, 저장소 공개 키(`UpdatesConfig.keyring`, 기본 `/usr/share/keyrings/oplite-archive-keyring.gpg`)가 없으면 쓰지 않고 관리하던 파일은 지움), `UpdateAgent`(시작과 `Settings` watch 이벤트 때 소스 파일, `first_check` 뒤 처음·그 뒤 `check_interval`마다 확인, 채널이 바뀌어 소스 파일을 새로 썼으면 곧 확인, 이 노드의 `node-upgrade`가 끝나지 않았으면 건너뜀, 시작할 때와 확인할 때 이 노드 상태의 `version`·`updates`가 비었으면 되살림). 패키지의 postrm은 `remove`·`purge` 때 관리 표시가 있는 소스 파일을 지운다.
- 다시 확인: 설계는 "그 노드로 전달"이지만 노드 사이 HTTP 전달 도우미가 없으므로 Task다. `POST /api/v1/nodes/{key}/updates/refresh` → `TaskOp::NodeUpdateCheck`(`node-update-check`, 대상·노드 = 그 노드), 202 Task. 거절 순서 403(admin만 — `Kind::Node` Write) → 404 → 400 `updates_unsupported` → 409 `node_offline` → 409 `cluster_upgrading` → 그 노드를 업그레이드하는 중이면 409 `upgrade_in_progress`. 그 노드에 끝나지 않은 확인이 있으면 그 Task를 돌려준다. 확인이 실패하면(결과에 `error`) Task도 실패한다.
- 업그레이드: `TaskOp::NodeUpgrade`(`node-upgrade`)는 설계의 `{ node }` 필드 없이 Task의 대상·노드가 그 노드다. `POST /api/v1/nodes/{key}/upgrade`(`updates::upgrade_node`) 거절 순서 403 → 404 → 400 → 409 `node_offline` → 409 `cluster_upgrading` → 409 `upgrade_in_progress`("node "X" is still being upgraded; upgrade one node at a time"). 검사와 만들기는 프로세스 안 잠금으로 감싸고, 다른 노드로 함께 온 요청은 실행기가 막는다: running을 쓴 뒤 다른 running `node-upgrade`가 보이면 "upgrade_in_progress: node "X" is being upgraded"로 실패(running 쓰기는 Raft 순서로 적용되므로 둘 다 지나가지 않는다). 실행(`Updates::upgrade(cluster, node, task, resume)`)은 확인 잠금을 잡고(apt 잠금을 다투지 않게, 끝난 뒤의 확인까지 잡은 채) `log_dir`를 만들고 `systemd-run --unit=opl-upgrade-<task id 32자 simple> --collect --property=Type=exec --property=StandardOutput=append:<log> --property=StandardError=append:<log> --setenv=DEBIAN_FRONTEND=noninteractive --setenv=LANG=C /bin/sh -c 'echo "opl-upgrade: start"; … update -q -o DPkg::Lock::Timeout=600 && … -y -q -o DPkg::Lock::Timeout=600 … dist-upgrade; s=$?; echo "opl-upgrade: exit $s"; exit $s'`. 설계와 다른 점:
  - 명령 처음에 시작 줄, 끝에 종료 상태 줄을 남겨, 유닛이 사라졌을 때(`ActiveState=inactive`이고 `Result`가 비었거나 `LoadState=not-found`) 로그로 판단한다: 시작·종료 줄이 있으면 그 코드, 시작 줄만 있으면 `dpkg --audit`가 무언가 알리면 "the upgrade was interrupted; run "dpkg --configure -a" on the node", 비었으면 "the upgrade was interrupted; start it again"(설계의 "audit가 비면 성공"은 끊긴 apt를 성공으로 보므로 바꿈), 시작 줄이 없으면 이어 볼 때는 유닛을 지금 띄우고 아니면 "the upgrade did not start; try again".
  - `systemctl show`는 `-p LoadState`도 묻고, `systemctl` 자체가 실패하면 다음 `poll`에 다시 본다.
  - 실행기 첫 패스(`resume = true`)에서 running인 `node-upgrade`는 실패로 정리하지 않고 task id로 유닛 이름을 다시 만들어 이어 본다. 시간 제한 없음(`unlimited`), 취소 없음. 끝나면 성공·실패 모두 잠금을 잡은 채 확인을 한 번 한다.
- 저장소(`packaging/repo/build-repo.sh`): 설계에 더해 채널을 `stable|testing|enterprise`만 받고(그 밖은 종료 2), Release에 `Origin`·`Label: opLite`, `Components`는 `Packages`가 있는 채널만 고정 순서(stable testing enterprise), `Release.gpg`는 armor다. pool의 옛 패키지를 지우지 않으므로 옛 버전도 그 채널 `Packages`에 남는다. 이미 올린 이름에 내용이 다른 deb는 "version already published"로 거절(같으면 넘어감)하고, 새 색인·Release는 `dists/.trixie.new`에서 만들어 두 서명이 끝난 뒤 옮긴다(`InRelease`가 마지막). 도구 경로는 `APT_FTPARCHIVE`·`GPG`(시험용). `build-deb.sh`는 `OPL_APT_KEY` 파일을 `/usr/share/keyrings/oplite-archive-keyring.gpg`(0644)로 싣고, 없는 파일이면 1, armor 키면 "OPL_APT_KEY must be a binary keyring (gpg --export, not --armor)"로 1. CI의 패키지는 키링·`OPL_APT_URL` 없이 만든다.
- oplctl: `node list`의 `VERSION`(ADDRESS 다음), `cluster status` 끝의 혼합 버전 경고(버전 순, 중복 없이, 버전 모르는 멤버 제외), `settings get|set`(`-o json`이면 리소스, 키는 `updates.channel`만), `node updates <node> [--refresh]`(`checked:`·`error:`·표·`no updates available`·잘림 줄, 꺼진 노드는 "node X does not check for updates"), `node upgrade <node> [--wait]`(기다림은 24시간까지).
- 웹: 설계의 노드 상세 "업데이트" 탭 대신(노드 상세 페이지가 없다) 노드 표의 버전 열·업데이트 배지와 `NodeUpdatesDialog`(배지는 누구나 읽기 전용으로 열고, 행 메뉴 "업데이트…"·다시 확인·업그레이드는 admin). `/settings`(`SettingsPage`, `If-Match` PATCH), 로그인 화면 아래 서버 버전, 대시보드 혼합 버전 경고, watch 종류 `settings`(목록 캐시가 아니라 `useSettings()` 캐시), Task 라벨, 오류 문구.
- 시험: opl-types 단위(비교 표, 설정 직렬화, 노드 상태), opl-api `tests/cluster.rs`(버전 API, 합류 거절), `tests/settings.rs`, `tests/updates.rs`(가짜 `apt-get`·`systemd-run`·`systemctl`·`dpkg` — `tests/common/fake_apt.rs` `FakeApt`, `unit.state`가 active·success·failed·gone; 확인·실패·꺼진 노드·같은 Task, 채널을 바꾸면 소스 파일, 업그레이드 인자·성공·실패·한 번에 하나·이어 보기·사라짐과 audit), `parse_simulate`의 실제 Debian 13 출력 견본, oplctl `fake_api`, 웹 vitest, 셸 `build-repo-test.sh`(macOS, 가짜 도구)와 `repo-test.sh`(CI `package`의 Debian 13 컨테이너: 임시 키, stable·testing, `gpgv`, `file://`+`Signed-By` 설치, testing으로 바꾸면 `Inst oplite [<v>] (<v>+1 `). `iso.yml`은 설치한 노드의 `oplctl node list` VERSION과 `status.version`이 빌드 버전(`OPL_VERSION`)인지 본다.
- 알려진 틈:
  - 업그레이드는 opld를 다시 시작할 뿐 노드를 다시 부팅하지 않는다. 커널 업데이트 뒤 부팅과 VM 비우기는 관리자의 일이다.
  - 업그레이드하는 노드에 붙은 oplctl `--wait`·웹은 opld가 다시 시작할 때 연결이 끊길 수 있다(업그레이드는 계속).
  - 업데이트 확인은 Debian 패키지 전체를 보이고, `testing`에서 `stable`로 돌아가도 이미 올린 버전을 내리지 않는다.
  - 업그레이드 로그(`/var/log/opl/upgrade-*.log`)는 지우지 않는다.
  - 이 필드를 모르는 옛 opld가 리더이면 노드 상태를 다시 쓰며 `version`·`updates`를 지울 수 있다. 업데이트가 켜진 노드는 `UpdateAgent`가 시작·확인 때 되살리지만, 꺼진 노드(macOS, 위트니스)는 opld가 다시 시작할 때까지 버전이 비어 보일 수 있다. 그동안 버전이 빈 노드 때문에 `cluster_upgrading` 게이트가 닫힌다(설정 바꾸기·다시 확인·업그레이드가 409).
  - 저장소 Release에 `Valid-Until`을 넣지 않는다(의도). 넣으면 운영자가 주기적으로 다시 서명해야 하고, 놓치면 모든 노드의 확인이 실패한다. 대신 오래된 서명 색인을 다시 내미는 공격(freeze)은 막지 못한다.
  - 저장소를 웹 서버에 올릴 때는 `pool/`·`Packages`를 먼저, `Release`·`Release.gpg` 다음, `InRelease`를 마지막에 올려야 apt가 반쯤 올린 저장소를 읽지 않는다(`build-repo.sh`는 로컬 디렉터리에서 이 순서로 바꿔 넣는다).

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
| `upgrade_in_progress` | 409 | 다른 노드 업그레이드가 끝나지 않음(그 노드를 업그레이드하는 중의 다시 확인도) |
| `cluster_upgrading` | 409 | 버전을 적지 않은 옛 노드가 남은 클러스터에서 설정 바꾸기·다시 확인·업그레이드(계획 32 구현에서 더함) |
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
