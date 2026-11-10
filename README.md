# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 처음 분해한 하위 프로젝트 1~10 완료 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트, 구독과 텔레메트리, 백업·백업 작업, VMware 가져오기, 감사 로그, LDAP·OIDC, 원격 클러스터·원격 복제), 하위 프로젝트 11 노드 유지보수 모드(노드 비우기) 완료, 하위 프로젝트 12 증분 백업·증분 복제(계획 40·41) 완료

## 실행 (macOS, Apple Silicon)

```bash
brew install qemu
mkdir -p /tmp/opl
cat > /tmp/opl/node1.toml <<'TOML'
name = "node1"
address = "127.0.0.1"          # 다른 노드와 클라이언트가 이 노드에 닿는 호스트
data_dir = "/tmp/opl/node1"
run_dir = "/tmp/opl/run1"
TOML
cargo run -p opld -- --config /tmp/opl/node1.toml &       # 제어 소켓: /tmp/opl/run1/opld.sock
cargo run -p oplctl -- cluster init --name dev --socket /tmp/opl/run1/opld.sock
# 출력된 admin 비밀번호와 CA 지문으로 로그인한다(API: https://127.0.0.1:8006).
cargo run -p oplctl -- login --server https://127.0.0.1:8006 --ca-fingerprint sha256:...
mkdir -p /tmp/opl/pool
cargo run -p oplctl -- pool create local --path /tmp/opl/pool
cargo run -p oplctl -- volume create root --pool local --size 1G
cargo run -p oplctl -- vm create web --memory 512 --disk root --nic user --start
cargo run -p oplctl -- vm console web            # 출력된 vnc:// 주소를 VNC 뷰어로 연다
cargo run -p oplctl -- vm snapshot create web before --description "업데이트 전"   # 실행 중이면 메모리 포함(--no-memory: 디스크만)
cargo run -p oplctl -- vm snapshot list web      # 나무 모양, 현재 위치 *
cargo run -p oplctl -- vm clone web web-copy --snapshot before   # 스냅샷에서 새 VM(꺼진 VM은 --snapshot 없이 지금 상태)
cargo run -p oplctl -- vm snapshot revert web before
cargo run -p oplctl -- vm snapshot delete web before
cargo run -p oplctl -- vm snapshot schedule web --every daily --keep 7   # 예약 스냅샷(--memory, 끄기 --off)
cargo run -p oplctl -- vm snapshot schedule web  # 지금 예약과 마지막 예약 실패
cargo run -p oplctl -- volume resize root --size 2G   # 늘리기만, 실행 중에도 된다
```

같은 Mac에 노드를 더하려면 포트와 디렉터리를 바꿔 opld를 하나 더 띄우고, 조인 URL로 합류한다.

```bash
cat > /tmp/opl/node2.toml <<'TOML'
name = "node2"
address = "127.0.0.1"
listen = "127.0.0.1:8016"
cluster_listen = "127.0.0.1:8017"
data_dir = "/tmp/opl/node2"
run_dir = "/tmp/opl/run2"
TOML
cargo run -p opld -- --config /tmp/opl/node2.toml &
cargo run -p oplctl -- cluster join "$(cargo run -q -p oplctl -- cluster join-token)" \
    --socket /tmp/opl/run2/opld.sock
cargo run -p oplctl -- cluster status
```

API 문서(OpenAPI)는 `https://127.0.0.1:8006/api/v1/openapi.json`에 있다.

## 설치 (ISO와 패키지)

서버에는 설치 ISO로 Debian 13(trixie)과 opLite를 함께 설치한다. ISO는 Debian netinst에 패키지 `oplite`와
preseed를 더한 것이다. 만들기는 Linux(amd64)에서 한다(`dpkg-deb`, `xorriso`가 필요하다).

```bash
sudo apt install xorriso libssl-dev pkg-config     # 그리고 Rust, Node.js 22와 pnpm
# 1. 패키지. 버전은 바이너리(--version)와 패키지에 같은 값을 쓴다(다르면 build-deb.sh가 멈춘다).
pnpm -C web install --frozen-lockfile && pnpm -C web build
#    구독 공개 키(OPL_LICENSE_KEYS) 없이 빌드하면 저장소의 개발 키를 믿는 개발 패키지다("구독과 텔레메트리").
OPL_VERSION=0.1.0 cargo build --release -p opld --features web-ui -p oplctl
OPL_DEV_LICENSE=1 packaging/deb/build-deb.sh 0.1.0 dist   # dist/oplite_0.1.0_amd64.deb
# 2. netinst. 판과 SHA-256은 packaging/iso/debian.lock에 있고, build-iso.sh가 해시를 확인한다.
#    그 판보다 새 판이 나와 URL이 404면 같은 파일을 cdimage/archive/<판>/amd64/iso-cd/에서 받는다.
. packaging/iso/debian.lock && curl -fLo netinst.iso "$DEBIAN_NETINST_URL"
# 3. ISO. 기본 항목 "Install opLite"가 더해진다(BIOS·UEFI, 원래 항목은 남는다).
packaging/iso/build-iso.sh --netinst netinst.iso --deb dist/oplite_0.1.0_amd64.deb --out oplite.iso
```

`oplite.iso`를 USB(`dd`)나 가상 CD로 부팅한다. "Install opLite"가 기본으로 골라져 있다. UEFI 메뉴에서는 맨 위
줄이고, BIOS 메뉴에서는 "Graphical install" 다음 두 번째 줄이다. 설치 화면은 다음만 묻는다: 네트워크(DHCP가
실패하면 수동), 호스트 이름, 도메인, root 비밀번호, 설치 디스크(디스크가 여럿이면 GRUB을 넣을 디스크도).
나머지는 정해져 있다: 언어·키보드 en_US·us, 시간대 UTC(설치한 뒤 `timedatectl set-timezone Asia/Seoul`처럼 바꾼다),
고른 디스크 전체에 LVM(`/`와 swap), 미러 `deb.debian.org`, 패키지 standard·ssh-server, 일반 사용자 없음.
**고른 디스크의 내용은 묻지 않고 지운다.**

설치 끝에 `oplite-setup.sh`가 돈다(로그는 대상 시스템의 `/root/oplite-setup.log`).

- 패키지 `oplite`, `ifupdown2`, `isc-dhcp-client`(`vmbr0`의 DHCP)를 설치한다(의존 패키지는 미러에서 받는다).
- 기본 경로 인터페이스(DHCP이거나 게이트웨이가 있는 것, 없으면 첫 고정 주소)를 VLAN-aware 브리지 `vmbr0`으로 옮긴다.
  주소·게이트웨이·DNS는 `vmbr0`으로 가고, NIC는 `iface <nic> inet manual`로 `bridge-ports`가 된다(`bridge-vids 2-4094`).
  설치 프로그램이 쓴 `/etc/network/interfaces`를 바꿔 설치 환경과 대상 시스템 두 곳에 쓴다(설치 프로그램은 이 파일을
  나중에 대상으로 복사한다). 인터페이스를 찾지 못하면 바꾸지 않는다.
- `/etc/opl/opld.toml`에 `name`(호스트 이름을 소문자로)과 `address`(`vmbr0`의 주소)를 쓴다. 호스트 이름이 노드 이름
  규칙(소문자·숫자·`-`, 1~63자)에 맞지 않거나 주소를 찾지 못하면 그 줄을 주석으로 둔다. 그때 opld는 설정 오류로
  시작하지 못하므로(`journalctl -u opld`) 값을 채우고 `systemctl restart opld`를 한다. 다른 설정은
  `/usr/share/oplite/opld.toml.example`을 본다.
- 콘솔 로그인 화면에 웹 UI 주소와 init·join 명령을 영어로 보인다(`/etc/issue.d/oplite.issue`). `opld.toml`의 주석도 영어다.
- `opld.service`를 켠다. 다시 부팅하면 opld가 돈다(`systemctl status opld`, `journalctl -u opld`).

설치한 시스템의 root는 SSH로 비밀번호 로그인을 할 수 없다(Debian 기본 `PermitRootLogin prohibit-password`).
콘솔에서 root로 로그인해 `adduser <이름>`으로 일반 사용자를 만들거나 `/root/.ssh/authorized_keys`에 공개 키를 넣는다.

클러스터는 설치한 뒤에 만든다. 첫 노드에서 `cluster init`을 하고, 다른 노드는 그 클러스터에 합류한다.

```bash
# 첫 노드(root)
oplctl cluster init --name prod                  # admin 비밀번호(한 번만 나온다), CA 지문, 합류 URL
oplctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:...
oplctl cluster join-token                        # 합류 URL은 한 번 쓰고 1시간 간다. 노드마다 새로 만든다
# 다른 노드(root)
oplctl cluster join 'opl-join://…'
```

웹 UI는 `https://<노드 주소>:8006/`이다(클러스터 화면이 합류 명령을 만들어 준다).

**DHCP 주소.** 설치를 DHCP로 하면 `address`는 설치할 때 받은 주소이고 파일에 그렇다는 주석이 붙는다. 다른 노드와
클라이언트가 이 주소로 노드에 닿고, 노드가 클러스터에 들어간 뒤에는 주소를 바꿀 수 없다(노드 인증서에 들어 있다).
DHCP 서버에서 주소를 예약하거나 고정 주소를 쓴다. `cluster init`·`join` 전이라면 `/etc/network/interfaces`의
`vmbr0`과 `opld.toml`의 `address`를 고치고 `systemctl restart opld`를 해도 된다.

**패키지만 설치하기.** Debian 13(amd64)이 이미 있으면 패키지만 넣는다. 브리지는 관리자가 만든다("실행 (Linux, KVM)").

```bash
sudo apt install ./oplite_0.1.0_amd64.deb         # 의존 패키지(QEMU, OVMF, nftables 등)도 받는다
sudo mkdir -p /etc/opl
sudo cp /usr/share/oplite/opld.toml.example /etc/opl/opld.toml   # name·address를 고친다
sudo systemctl start opld                         # 설정이 없으면 유닛은 시작하지 않고 건너뛴다
```

패키지는 `/usr/bin/opld`·`oplctl`(웹 UI 포함), `/usr/lib/systemd/system/opld.service`,
`/usr/share/oplite/opld.toml.example`을 설치한다. 업그레이드(`apt install`로 새 판)는 opld가 enable돼 있으면 다시 시작한다.
VM의 QEMU는 VM마다 자기 scope에 있어 opld가 멈추거나 다시 시작해도 꺼지지 않는다.

**지우기.** `apt remove oplite`는 opld를 멈추고, `apt purge oplite`는 서비스 링크까지 지운다. 어느 쪽도
`/var/lib/opl`(클러스터 데이터, 풀, 볼륨)과 `/etc/opl`을 지우지 않는다. 데이터를 지우는 것은 관리자의 일이다.

한계:

- 설치에 인터넷(Debian 미러)이 필요하다. 모든 패키지를 담은 오프라인 ISO는 없다.
- amd64만 있다(ISO와 패키지).
- 루트 파일 시스템은 LVM 하나다. ZFS 루트와 소프트웨어 RAID 설치 선택지는 없다.
- 설치 화면에서 클러스터를 만들거나 합류하지 않는다(설치한 뒤 위 명령으로).

## 업데이트

패키지로 설치한 노드는 opLite APT 저장소에서 업데이트를 확인하고, 관리자가 노드를 **하나씩** 업그레이드한다.
클러스터 설정의 **채널**(`stable`·`testing`, 구독이 있으면 `enterprise`)이 어느 저장소 구성 요소를 따를지 정한다.

```bash
oplctl settings get                              # updates.channel: stable
oplctl settings set updates.channel=testing      # admin. 모든 노드의 소스 파일이 바뀐다
oplctl node list                                 # VERSION 열(ADDRESS 다음)
oplctl node updates node1                        # 마지막 확인 시각, PACKAGE CURRENT CANDIDATE 표, 확인 실패 이유
oplctl node updates node1 --refresh              # 지금 다시 확인하고(작업을 기다린다) 결과를 보인다
oplctl node upgrade node1                        # upgrade of node1 started (task …); follow it with `oplctl task get …`
oplctl node upgrade node1 --wait                 # 끝날 때까지 기다린다: "node node1 upgraded"
oplctl cluster status                            # 버전이 섞여 있으면 경고
```

- **채널.** 설정은 클러스터에 하나뿐인 `Settings` 리소스(이름 `cluster`)다. `GET /api/v1/settings`는 로그인한 누구나,
  `PATCH /api/v1/settings`(`{"updates":{"channel":"testing"}}`, 다른 패치 API처럼 `If-Match`를 받는다)는 admin만
  한다. 한 번도 바꾸지 않았으면 기본값(`stable`)이다. 웹은 "설정" 화면(admin만 저장, 그 밖은 읽기만).
- **소스 파일.** 저장소 주소는 opld 설정 `[updates] apt_url`이고, 비면 빌드할 때 넣은 `OPL_APT_URL`이다. 둘 다 비면
  opld는 소스 파일을 쓰지 않는다(관리자가 apt 소스를 직접 둔다). 주소가 있으면 opld가 시작할 때와 채널이 바뀔 때
  `/etc/apt/sources.list.d/oplite.sources`를 쓴다:

  ```
  # managed by opld; edits are overwritten
  Types: deb
  URIs: https://apt.example.com/oplite
  Suites: trixie
  Components: stable
  Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg
  ```

  첫 줄의 표시가 없는 파일은 opld가 건드리지 않고 로그에 경고만 남긴다(비었거나 공백뿐인 파일은 opld 것으로 본다).
  소스를 직접 관리하려면 첫 줄을 지운다. 저장소 공개 키(`Signed-By`의 키링)가 없으면 apt가 저장소를 거절하므로
  opld는 소스 파일을 쓰지 않고(경고) 자기가 쓴 파일은 지운다. 패키지를 지우면(`remove`·`purge`) postrm이 표시가
  있는 소스 파일을 지운다.
- **확인.** 노드마다 opld가 시작하고 1분 뒤, 그 뒤 24시간마다 `apt-get update -q`와 `apt-get -s -q dist-upgrade`
  (설치하지 않고 흉내만)로 올릴 수 있는 패키지를 찾아 노드 상태(`node get`의 `status.updates`)에 쓴다. opLite만이
  아니라 Debian 패키지(보안 업데이트 등)도 보인다. 500개까지 담고 넘으면 잘렸다고 표시한다. 다른 apt(무인 업그레이드
  등)의 잠금은 `DPkg::Lock::Timeout=60`초까지 기다린다. 실패하면 이유(명령과 stderr 끝 5줄)를 남긴다. `--refresh`와
  웹의 "다시 확인"은 그 노드에서 도는 작업 `node-update-check`다(admin. 끝나지 않은 확인이 있으면 새로 만들지 않고 그
  작업을 돌려준다. 주기 확인이 도는 중이면 끝나기를 기다렸다가 확인하고, 그사이 업그레이드가 시작되면 확인하지 않고
  끝난다). 채널을 바꿔 소스 파일이 바뀌면 다음 주기를 기다리지 않고 곧 확인한다. 업그레이드 중에는 주기 확인을
  건너뛰고 다시 확인은 **409** `upgrade_in_progress`다(업그레이드가 apt 잠금을 잡고 있다).
- **업그레이드.** `node upgrade`(admin)는 그 노드에서 작업 `node-upgrade`를 돌린다. 작업은 opld 밖의 systemd 유닛
  `opl-upgrade-<작업 ID 32자(하이픈 없이)>`(`systemd-run`)에서 `apt-get update`와 `apt-get dist-upgrade`(바뀐 설정
  파일은 지금 것을 둔다. 무인 업그레이드 같은 다른 apt의 잠금은 `DPkg::Lock::Timeout=600`초까지 기다린다)를 돌리고
  유닛을 지켜본다. 로그는 `/var/log/opl/upgrade-<작업 ID>.log`이고 처음에 `opl-upgrade: start`, 끝에
  `opl-upgrade: exit <코드>` 줄이 있다. opLite 패키지가 바뀌면 postinst가 opld를 다시 시작하고, 다시 시작한 opld가
  같은 유닛을 이어 지켜본다. VM은 자기 scope에 있어 계속 돈다. 실패하면 작업 오류에 로그 끝 20줄이 있다. 유닛이
  끝나고 치워졌으면 로그로 판단한다: 종료 줄이 있으면 그 코드, 시작 줄만 있으면(도중에 노드가 꺼짐) dpkg가 반쯤
  설정된 채 남았을 때 "the upgrade was interrupted; run "dpkg --configure -a" on the node", 아니면 "the upgrade was
  interrupted; start it again", 시작 줄도 없으면 이어 볼 때는 유닛을 지금 띄우고(apt는 다시 돌려도 된다) 아니면
  "the upgrade did not start; try again"으로 실패한다. 업그레이드하는 동안 그 노드의 업데이트 확인은 쉰다. 시간 제한과
  취소는 없다(apt를 중간에 끊으면 더 나쁘다). 끝나면(성공·실패 모두) 업데이트를 다시 확인하고, 새 버전은 다시 시작한
  opld가 노드 상태에 쓴다.
- **거절.** 클러스터 어디든 끝나지 않은 업그레이드가 있으면 **409** `upgrade_in_progress`(한 번에 한 노드. 다른
  노드로 함께 온 두 요청이 모두 지나가도 나중에 시작한 작업이 "upgrade_in_progress: node "X" is being upgraded"로
  실패한다), 오프라인 노드는 **409** `node_offline`, 업데이트를 확인하지 않는 노드(macOS, apt가 없는 노드, 위트니스)는
  **400** `updates_unsupported`다. 버전을 적지 않은 옛 노드(계획 32 전의 opld)가 남아 있으면 설정 바꾸기·다시
  확인·업그레이드는 **409** `cluster_upgrading`("some nodes run an older opLite (node2, node3); …")이다: 옛 노드는
  모르는 리소스 종류와 작업 종류가 든 Raft 항목을 풀지 못한다. 그래서 계획 32 전 클러스터의 첫 업그레이드는 노드마다
  손으로(`apt-get update && apt-get dist-upgrade`) 하고, 모든 노드가 버전을 적은 뒤부터 `node upgrade`를 쓴다.
- **`--wait`.** 업그레이드하는 노드의 API에 로그인한 oplctl은 opld가 다시 시작할 때 연결 오류로 끝날 수 있다.
  업그레이드는 계속된다. 다른 노드에 로그인해 두거나 `oplctl task get <ID>`로 다시 본다.
- **합류.** 클러스터에서 가장 낮은 노드 버전보다 낮은 opld는 합류할 수 없다(**409** `version_too_old`: "this node runs
  X; the cluster runs Y or newer; upgrade it first"). 버전을 보내지 않는 옛 opld도 버전을 적은 노드가 있는 클러스터에는
  합류할 수 없다("this node runs an unknown version; …"). 새 노드는 먼저 클러스터와 같은 버전으로 올린다. 합류할 때
  보낸 버전은 그 노드가 등록하기 전부터 노드 상태에 적힌다.

**롤링 순서.** opLite는 노드를 자동으로 돌아가며 올리지 않고 노드를 다시 부팅하지도 않는다. 한 노드씩:

1. `oplctl node updates <노드> --refresh`로 무엇이 바뀌는지 본다.
2. 커널처럼 다시 부팅해야 하는 업데이트가 있으면 `oplctl node drain <노드> --wait`로 노드를 유지보수 모드로 두고
   VM을 다른 노드로 옮긴다("노드 유지보수와 비우기" — 어느 노드에서 해도 된다). 비우기가 처음 들어간 버전으로 올리는
   동안은 옛 노드가 비우기를 몰라 쓸 수 없으므로(`cluster_upgrading`) 그때는 VM을 옮기고(`vm migrate`) 유지보수 모드로
   둔다(`oplctl node maintenance <노드> on` 또는 웹 "유지보수 시작"). 그 뒤의 업그레이드에서는 버전이 섞여도 된다.
3. `oplctl node upgrade <노드> --wait`(다른 노드에 로그인해서). 필요하면 노드를 다시 부팅하고 유지보수 모드를
   끈다(`oplctl node maintenance <노드> off`).
4. `oplctl node list`에서 그 노드의 VERSION과 ONLINE을 확인하고 다음 노드로 간다.

모든 노드가 같은 버전이 될 때까지 `cluster status`와 웹 대시보드에 "nodes run different opLite versions (…); finish
the upgrade on every node" 경고가 보인다. 웹은 노드 표의 업데이트 배지(올릴 패키지 수, 확인 실패면 경고)를 누르면
같은 정보와 "다시 확인"·"업그레이드" 버튼(admin)이 있는 대화상자를 연다. 로그인 화면 아래에 서버 버전
(`GET /api/v1/version`, 인증 없음)이 보인다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[updates]
# enabled = true                 # 없으면 apt_get이 있을 때(Debian) 켜진다. macOS는 꺼진다
check_interval = "24h"           # 업데이트 확인 간격
first_check = "1m"               # opld가 시작하고 첫 확인까지
apt_url = ""                     # 비면 빌드 값 OPL_APT_URL. 그것도 비면 소스 파일을 쓰지 않는다
sources_file = "/etc/apt/sources.list.d/oplite.sources"
apt_get = "/usr/bin/apt-get"
systemd_run = "/usr/bin/systemd-run"
systemctl = "/usr/bin/systemctl"
dpkg = "/usr/bin/dpkg"
log_dir = "/var/log/opl"         # 업그레이드 로그
poll = "2s"                      # 업그레이드 유닛을 다시 보는 간격
```

**저장소 만들기.** 저장소는 서명한 정적 파일 트리이고, 올릴 웹 서버는 opLite 밖이다. 패키지에는 저장소 주소와 서명
공개 키를 빌드할 때 넣는다(키를 넣지 않은 패키지는 `Signed-By`의 키링이 없어 apt가 저장소를 거절한다).

```bash
# 서명 키(한 번). GPG 홈은 비밀 키를 둘 곳으로(GNUPGHOME).
gpg --quick-generate-key 'opLite archive <apt@example.com>' ed25519 sign never
KEY_ID=$(gpg --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
gpg --export "$KEY_ID" > oplite-archive-keyring.gpg
# 패키지: 저장소 주소(바이너리에)와 공개 키(/usr/share/keyrings/oplite-archive-keyring.gpg).
# 구독 공개 키 OPL_LICENSE_KEYS는 바이너리와 build-deb.sh에 함께 준다("구독과 텔레메트리"). export하지 않고
# 두 명령 앞에만 준다: 같은 셸의 cargo test가 개발 키로 서명한 시험 키를 풀지 못해 실패한다.
KEYS=<opl-license keygen이 출력한 공개 키>
OPL_LICENSE_KEYS=$KEYS OPL_VERSION=0.2.0 OPL_APT_URL=https://apt.example.com/oplite \
    cargo build --release -p opld --features web-ui -p oplctl
OPL_LICENSE_KEYS=$KEYS OPL_APT_KEY=oplite-archive-keyring.gpg packaging/deb/build-deb.sh 0.2.0 dist
# 저장소: 채널(stable|testing|enterprise)에 패키지를 더하고 색인을 다시 만들어 서명한다(apt-ftparchive, apt-utils)
OPL_REPO_GPG_KEY_ID=$KEY_ID packaging/repo/build-repo.sh repo testing dist/oplite_0.2.0_amd64.deb
# testing에서 확인한 뒤 같은 패키지를 stable에도
OPL_REPO_GPG_KEY_ID=$KEY_ID packaging/repo/build-repo.sh repo stable dist/oplite_0.2.0_amd64.deb
```

레이아웃: `pool/<채널>/*.deb`, `dists/trixie/<채널>/binary-amd64/Packages(.gz)`, `dists/trixie/Release`
(있는 모든 채널을 묶는다)와 서명 `InRelease`·`Release.gpg`. `repo/`를 `apt_url`이 가리키는 곳에 그대로 올린다.
`build-repo.sh`는 고른 채널의 `Packages`만 다시 만들고 다른 채널은 그대로 둔다. pool의 옛 패키지는 지우지 않으므로
옛 버전도 색인에 남는다(apt는 가장 새 버전을 고른다). 이미 올린 이름에 내용이 다른 deb는 거절한다("version already
published") — 같은 버전을 다시 빌드했으면 버전을 올린다. 새 색인과 Release는 옆 디렉터리에서 만들어 두 서명이 다
끝난 뒤에 옮기므로(`InRelease`가 마지막) 서명이 실패해도 저장소는 옛 색인 그대로다. 웹 서버에 올릴 때도 같은 순서로
`pool/`과 `Packages`를 먼저, `Release`·`Release.gpg`를 다음, `InRelease`를 마지막에 올린다. `OPL_APT_KEY`는
`gpg --export`의 바이너리 키링이어야 한다(armor면 `build-deb.sh`가 거절한다). Release에 `Valid-Until`은 넣지 않는다:
넣으면 저장소를 주기적으로 다시 서명해야 하고, 그러지 못하면 모든 노드의 확인이 실패한다. `enterprise` 채널은
구독이 있는 클러스터가 쓴다(아래).

## 구독과 텔레메트리

구독은 기능을 막지 않는다. 키가 없거나 만료돼도 모든 기능이 돈다. 구독 키는 지원과 `enterprise` 업데이트 채널에
대한 권리이고, 노드가 빌드에 들어 있는 공개 키로 오프라인 검증한다(어떤 서버에도 묻지 않는다).

```bash
oplctl subscription show                         # state: active, customer, level, nodes: 2/3(사용/한도), expires: 2027-11-05
oplctl subscription set opl1.eyJpZCI6…           # admin. 키를 넣거나 바꾸고 상태를 보인다
oplctl subscription set --file key.txt           # 파일에서(끝의 줄바꿈은 다듬는다)
oplctl subscription set --file - < key.txt       # 표준 입력에서. 키를 명령줄(셸 기록, ps)에 두지 않으려면 --file을 쓴다
oplctl subscription remove                       # admin
oplctl settings set updates.channel=enterprise   # admin. 구독이 active일 때만
```

- **키.** `opl1.<payload>.<signature>` 한 줄이다. `payload`는 JSON
  `{"id","customer","level":"standard"|"premium","nodes","issued_at","expires_at"}`(시각은 유닉스 초, 모르는 필드는
  무시)의 base64url(패딩 없음), `signature`는 `opl1.<payload>` 바이트에 대한 Ed25519 서명의 base64url이다. `id`는
  영문자·숫자·`.`·`_`·`-`로 된 1~64자(영문자나 숫자로 시작, APT 인증 파일의 로그인이 된다), `customer`는 1~200자,
  `nodes`(위트니스가 아닌 노드 수 한도)는 1 이상, `expires_at`은 `issued_at`보다 뒤이고 9999-12-31T23:59:59Z
  (`253402300799`)를 넘지 않는다.
- **상태.** `GET /api/v1/subscription`(로그인한 누구나)이 읽을 때마다 계산한다: 키가 없으면 `none`, 형식이나 서명이
  맞지 않으면 `invalid`(공개 키가 다른 빌드로 바꾼 경우), 만료됐으면 `expired`, 위트니스가 아닌 노드가 한도보다 많으면
  `over_limit`(같으면 괜찮다), 그 밖은 `active`다. 저장소의 개발 키로 검증된 키는 `development: true`이고 oplctl은
  `state: active (development key)`, 웹은 "개발용 키"로 보인다. `PUT /api/v1/subscription`(`{"key":"opl1.…"}`)과
  `DELETE /api/v1/subscription`은 admin만 한다. 형식·서명이 틀리거나 이미 만료된 키는 **400**
  `invalid_subscription_key`이고 이유가 붙는다(`malformed subscription key: …`, `the subscription key signature does
  not match this build`, `the subscription key expired on 2026-01-01`). 키 문자열은 클러스터 설정
  (`Settings.spec.subscription`)에 저장하지만 API(`GET`·`PATCH /api/v1/settings`, watch)로는 내보내지 않는다. 감사
  로그(`detail` `id=<키 id>`, "감사 로그")와 opld 로그에는 키 id만 남는다.
- **웹.** "설정" 화면의 구독 카드가 상태·고객·수준·노드(사용/한도)·만료일을 보이고, admin은 키를 붙여 넣거나 지운다.
  상태가 `active`가 아니거나 만료 30일 전부터 대시보드 제목 아래에 안내가 보인다(`expired`·`over_limit`는 경고 색,
  그 밖은 정보 색). "7일 동안 숨기기"를 누르면 그 브라우저에서 그 안내만 7일 동안 숨긴다(예: "구독 없음"을 닫은 뒤
  구독이 만료되면 만료 안내는 보인다). 웹은 설정이 바뀌거나, 노드가 합류하거나 떠나거나, 다시 연결할 때와 한
  시간마다 상태를 다시 읽는다. 만료일은 UTC 날짜다.
- **enterprise 채널.** `updates.channel=enterprise`는 구독이 `active`일 때만 바꿀 수 있다(아니면 **400**
  `subscription_required`, 이미 enterprise여도 다시 보내면 검사한다). 채널이 enterprise이고 저장된 키의 서명이 맞으면
  노드마다 opld가 소스 파일(`Components: enterprise`)과 함께 APT 인증 파일 `/etc/apt/auth.conf.d/oplite.conf`(0600)를
  쓴다:

  ```
  # managed by opld; edits are overwritten
  machine apt.example.com login <구독 id> password <키 문자열의 SHA-256 16진수>
  ```

  `machine`은 `apt_url`의 호스트(포트가 있으면 `호스트:포트`)다. 프로토콜 없는 `machine` 항목을 apt는 https에만 쓰므로
  `apt_url`이 `http://`면 인증이 가지 않는다. 다른 채널로 바꾸거나 키를 지우면 opld가 쓴 파일을 지운다(첫 줄의 표시가
  없는 파일은 소스 파일처럼 건드리지 않고 경고만). 구독이 만료되거나 노드 수를 넘어도 채널과 인증 파일은 그대로다.
  저장소가 거절하면(401 등) `node updates`의 확인 오류에 보인다. 저장소 쪽에서 인증을 확인하는 일(예: 웹 서버가
  사용자 이름을 구독 id로, 비밀번호를 발급한 키의 SHA-256으로 확인하고 만료된 구독을 거절)은 opLite 밖이다.
- **옛 노드.** 구독 넣기·지우기, enterprise 채널, 텔레메트리 바꾸기(`reset-id` 포함)는 모든 노드(위트니스 포함)가 이 opld 빌드와
  같거나 새 버전일 때만 된다. 아니면 **409** `cluster_upgrading`("some nodes run an older opLite (node3); …"):
  옛 opld는 이 새 값이 든 설정을 풀지 못한다. 롤링 업그레이드를 끝낸 뒤 쓴다.

**키 발급.** 발급 도구 `opl-license`는 패키지에 넣지 않는다. 발급하는 쪽에서 저장소를 빌드해 쓴다.

```bash
cargo build --release -p opl-api --bin opl-license
# 발급 키 쌍(한 번). 비밀 키(PKCS#8 base64 한 줄)는 --out 파일(0600, 있으면 거절)에, 공개 키 base64는 표준 출력에
target/release/opl-license keygen --out license.key
# 키 발급: 표준 출력에 opl1.… 한 줄(--issued-at <유닉스 초>로 발급 시각을 정할 수 있다. 기본은 지금)
target/release/opl-license issue --key license.key --id acme-2026 --customer 'Acme Corp' \
    --level premium --nodes 3 --days 365
# 비밀 키 파일의 공개 키(OPL_LICENSE_KEYS의 항목)를 다시 보기
target/release/opl-license pubkey --key license.key
```

`issue`는 `--nodes`·`--days`가 1 이상인지 먼저 보고("opl-license: --days must be at least 1"), 내용이 키 규칙에 맞지
않으면 "opl-license: invalid license: id must be …"처럼 멈춘다. `keygen`은 쓰다가 실패하면 만든 파일을 지운다.

빌드 값 `OPL_LICENSE_KEYS`(base64 공개 키를 쉼표로)를 주고 빌드한 opld만 그 키로 서명한 구독 키를 믿는다. 주지
않으면 저장소의 개발 키(`crates/opl-types/keys/dev-license.pub`) 하나를 믿는데, 그 비밀 키가 시험 견본
(`crates/opl-types/tests/fixtures/dev-license.key`)으로 저장소에 있어 누구나 키를 만들 수 있다. 그래서 정식 패키지는
반드시 `OPL_LICENSE_KEYS`를 주고(위 "저장소 만들기"처럼 `cargo build`와 `build-deb.sh`에 함께), `build-deb.sh`는 이 값이
없으면 `OPL_DEV_LICENSE=1`(개발 패키지, CI의 패키지가 이렇다)이 아닌 한 "build-deb: OPL_LICENSE_KEYS is not set; …"로
멈춘다. 값이 있으면 세 가지를 더 본다: 쉼표로 나눈 항목마다 정규 base64(44자, `=` 끝)로 푼 32바이트 Ed25519 공개
키인지("build-deb: OPL_LICENSE_KEYS entry is not a base64 Ed25519 public key: …")와 그런 항목이 하나 이상인지(공백·쉼표뿐이면
"build-deb: OPL_LICENSE_KEYS has no keys"), 개발 키가 들어 있지 않은지("… contains the development
key; use OPL_DEV_LICENSE=1 for a development package"), opld 바이너리에 그 값이 그대로 들어 있는지(같은 값으로
빌드했는지, "build-deb: opld was not built with this OPL_LICENSE_KEYS"). 그래도 개발 키를 넣고 빌드한 바이너리는 그
키로 검증한 구독을 개발용으로 보인다. 빌드 값이 공백뿐이면 없는 것과 같지만(개발 키), 쉼표처럼 공백이 아닌 글자가 있는데
풀리는 키가 없으면 개발 키로 떨어지지 않고 어떤 구독 키도 믿지 않는다. 발급 키를
바꿀 때는 옛 공개 키와 새 공개 키를 함께 넣으면 이미 낸 키도 계속 풀린다. 이 키를 믿지 않는 빌드로 올리면 저장된 키는
`invalid`가 된다.

**텔레메트리.** 기본으로 꺼져 있다. 켜면 클러스터 리더가 하루에 한 번 사용 보고(수와 버전만)를 보낸다.

```bash
oplctl telemetry preview                           # admin. 지금 보낼 JSON 그대로(꺼져 있어도)
oplctl telemetry enable                            # admin. 저장된 끝점, 없으면 빌드 기본 끝점(OPL_TELEMETRY_URL)으로
oplctl telemetry enable --endpoint https://telemetry.example.com/v1/report
oplctl telemetry show                              # enabled, endpoint, id, last sent, last error
oplctl telemetry disable
oplctl telemetry reset-id                          # 보고 id를 새로 만든다
```

- **설정.** `PATCH /api/v1/settings`의 `{"telemetry":{"enabled":true,"endpoint":"https://…"}}`(admin)다. 끝점은
  호스트가 있는 `https://` 주소여야 하고 사용자 정보(`user:pass@`)를 넣을 수 없다(설정은 로그인한 누구나 읽는다.
  아니면 **400** `invalid_request`). `""`를 보내면 지워 빌드 값
  `OPL_TELEMETRY_URL`을 쓴다. 켰는데 끝점도 빌드 값도 없으면 **400** `telemetry_endpoint_required`. 처음 켤 때 무작위
  UUID v4 보고 id를 만든다(클러스터 이름·CA와 관계없다). 꺼도 id는 남고, `POST /api/v1/telemetry/reset-id`(admin)가
  새로 만든다. 웹 "설정" 화면은 "켜기…"를 누르면 보낼 JSON(`GET /api/v1/telemetry/preview`, admin)을 대화상자로
  보이고 확인해야 켠다(끝점 입력란에 저장하지 않은 값이 있으면 함께 보낸다).
- **보내기.** 리더가 `[telemetry] tick`마다 확인해, 켜져 있고 마지막 전송(`status.telemetry.last_sent_at`)에서
  `interval`이 지났으면 끝점에 JSON을 `POST`한다(시간 제한 30초, https만, 리다이렉트는 따르지 않는다). 2xx면
  `last_sent_at`을 적고 `last_error`를 지운다. 아니면 `last_error`("HTTP 500 Internal Server Error", 3xx면 "HTTP 307
  Temporary Redirect" 같은 상태, 또는 연결 오류)를 적고 `retry` 뒤에 다시 보낸다. 켜져 있는데 설정 끝점도 빌드 기본
  끝점도 없으면(다른 빌드가 켠 경우) `last_error`가 "no telemetry endpoint: set telemetry.endpoint"다. 마지막 전송
  시각은 Raft에 있어 리더가 바뀌어도 `interval` 전에는 다시 보내지 않는다. 보낸 뒤 그 기록을 쓰지 못해도 같은 리더는
  메모리의 전송 시각으로 다시 보내지 않는다. 다만 다시 보낼 시각과 그 전송 시각은 리더의 메모리에 있어, 실패 뒤
  리더가 바뀌면 새 리더가 곧 다시 보낼 수 있고, 보낸 뒤 기록하기 전에 리더가 바뀌면 새 리더가 한 번 더 보낼 수 있다.
  이 빌드보다 오래된 노드가 남아 있으면 보내지 않는다.
- **프록시.** reqwest가 `HTTPS_PROXY` 환경 변수를 따른다. 패키지의 opld는 systemd 서비스이므로
  `systemctl edit opld`로 덮어쓰기 파일에 다음을 넣고 `systemctl restart opld` 한다:

  ```ini
  [Service]
  Environment=HTTPS_PROXY=http://proxy.example.com:3128
  ```

- **보내는 내용**(`schema` 1). 이름, 주소, MAC, 키, 사용자, 고객 이름은 넣지 않는다:

  ```json
  {
    "schema": 1,
    "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
    "sent_at": 1793923200,
    "versions": { "0.2.0": 3 },
    "nodes": { "total": 3, "witness": 1 },
    "arch": { "x86_64": 2 },
    "hypervisor": { "qemu-kvm": 2 },
    "cpus": 64,
    "memory_gib": 251,
    "vms": { "total": 40, "running": 31, "ha": 6 },
    "pools": { "dir": 1, "nfs": 1, "rbd": 1 },
    "networks": { "bridge": 1, "vlan": 2, "vxlan": 1 },
    "security_groups": 3,
    "backup_jobs": 2,
    "imports": { "esxi": 5, "ova": 1 },
    "subscription": "premium"
  }
  ```

  `versions`는 버전별 노드 수(위트니스 포함, 버전을 모르면 `unknown`), `arch`·`hypervisor`·`cpus`·`memory_gib`(MiB 합을
  GiB로 내림)는 위트니스를 뺀 노드, `vms.running`은 지금 도는 VM, `vms.ha`는 HA를 켠 VM이다. `pools`는 백엔드 종류
  (`dir`, `nfs`, `lvm-thin`, `zfs`, `rbd`, `lvm-shared`), `networks`는 VXLAN이면 `vxlan`, 아니고 VLAN 태그가 있으면
  `vlan`, 그 밖은 `bridge`, `imports`는 가져오기의 원본 종류(`ova`, `esxi`)별 수다. `subscription`은 서명이 맞는
  구독 키의 수준(만료돼도), 키가 없거나 서명이 맞지 않으면 `none`이다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[telemetry]
tick = "60s"         # 리더가 보낼 때가 됐는지 보는 간격
interval = "24h"     # 보고 간격
retry = "1h"         # 보내지 못한 뒤 다시 보내기까지
# ca_file = "/etc/opl/telemetry-ca.pem"   # 시스템 루트에 더해 믿는 CA 인증서(PEM, 여럿 가능). 사설 수집 서버에 쓴다.
#                                         # 읽지 못하거나 인증서가 없으면 opld가 시작하지 않는다
```

수집 서버는 opLite 밖이다. 끝점의 기본값은 빌드할 때 `OPL_TELEMETRY_URL`로 넣는다(없으면 기본 끝점이 없다).

## 실행 (Linux, KVM)

opld는 root로 돈다(VM NIC의 tap 장치를 만든다). QEMU는 VM마다 `opl-vm-<id>.scope`로 감싼다.
KVM(`/dev/kvm`)이 없으면 TCG로 돌고 경고를 남긴다.

```bash
sudo apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils iproute2 nftables   # nftables는 VM 방화벽
# 브리지는 관리자가 미리 만든다. VLAN 태그를 쓰려면 VLAN-aware여야 한다.
sudo ip link add vmbr0 type bridge vlan_filtering 1
sudo ip link set eth1 master vmbr0 && sudo ip link set vmbr0 up
sudo mkdir -p /etc/opl
sudo tee /etc/opl/opld.toml >/dev/null <<'TOML'
name = "node1"
address = "10.0.0.11"
# [hypervisor]
# accel = "auto"          # auto | kvm | tcg
# systemd_scope = true
# [janitor]
# task_max_age_days = 7     # 끝난 작업(Task)을 남기는 날수(만든 때부터)
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
# [backup]
# tick_secs = 60          # 백업 작업을 확인하는 간격(초, 리더 노드)
# max_concurrent = 2      # 노드마다 동시에 도는 백업 작업(Task)의 최대 수
# [pools]
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
TOML
sudo opld --config /etc/opl/opld.toml &
sudo oplctl cluster init --name prod
oplctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:...
oplctl network create lan --bridge vmbr0 --vlan 10   # VLAN 10을 태그 없이 주고받는 NIC
# 풀과 볼륨은 macOS와 같다(/var/lib/opl/pool 등).
oplctl vm create web --memory 1024 --disk root --nic lan --start
```

브리지를 쓰는 VM은 그 브리지가 있는 노드에만 배치된다(`--node`로 노드를 직접 정하면 확인하지 않는다).
노드는 브리지 목록을 시작할 때와 1분마다 알린다.

## VXLAN 네트워크

노드 사이를 L2로 잇는 네트워크를 opld가 만든다(Proxmox SDN의 VXLAN 영역과 같은 자리). 물리 스위치에 VLAN을
설정하지 않아도 여러 노드의 VM이 한 네트워크에 있다. 네트워크마다 모든 노드(위트니스와 macOS 노드 빼고)에 브리지
`oplv<VNI>`와 VXLAN 장치 `oplx<VNI>`(UDP 4789)를 만들고, 다른 노드의 underlay 주소를 FDB에 넣어 브로드캐스트를
복제한다(유니캐스트 헤드엔드 복제, 멀티캐스트·EVPN 없음). VM NIC의 tap은 그 브리지에 붙는다.

```bash
oplctl network create ov --vxlan 1000              # MTU 기본 1450
oplctl network create jumbo --vxlan 1001 --mtu 8950 # underlay가 9000을 실을 때
oplctl network list
# NAME   TYPE    BRIDGE    VLAN/VNI  MTU
# lan    bridge  vmbr0     10        -
# ov     vxlan   oplv1000  1000      1450
# jumbo  vxlan   oplv1001  1001      8950
oplctl node list                                   # UNDERLAY 열: VXLAN 패킷을 주고받는 노드 주소
oplctl vm create web --memory 1024 --disk root --nic ov --start
```

웹은 네트워크 만들기에서 종류(브리지·VXLAN)를 고르고, 목록에 종류·브리지·VLAN/VNI·MTU를, 노드 목록에
"언더레이 주소"를 보인다. API는 `POST /api/v1/networks`의 `{"spec": {"vxlan": {"vni": 1000}, "mtu": 1450}}`
(`mtu`는 선택), Node `status.underlay`다.

```toml
# /etc/opl/opld.toml (노드마다, 선택)
[network]
underlay = "10.1.0.11"    # VXLAN 패킷을 주고받을 이 노드의 주소. 없으면 address가 IP면 그것, 이름이면 푼 주소(loopback·link-local 빼고 IPv4 먼저)
```

- **규칙.** `bridge`(와 `vlan`)과 `vxlan`(과 `mtu`) 중 하나만 쓴다(400). VNI는 1~16777215이고 클러스터 안에서
  겹치면 **409** `vni_in_use`("VNI 1000 is used by network \"ov\"", 같은 VNI를 동시에 만드는 두 요청은 막지 않는다
  — 그러면 두 네트워크의 VM이 같은 `oplv<VNI>`에 붙어 L2를 나누고, 브리지·VXLAN 장치·tap의 MTU는 이름이 앞선
  네트워크의 것이다). MTU는 1280~9000. `oplv<숫자>`·`oplx<숫자>` 모양의 이름은
  `--bridge`로 쓸 수 없다(400, "… is reserved for opLite VXLAN networks"). 네트워크는 바꿀 수 없고(지우고 다시 만든다),
  VM이 쓰는 네트워크는 지울 수 없다(409 `network_in_use`).
- **업그레이드 먼저.** VXLAN 네트워크를 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 opld는 새 모양의 네트워크를 읽지 못해 네트워크 목록부터 실패한다.
- **언제 생기나.** opld는 시작할 때, Network·Node가 바뀔 때(1초 모아 한 번), 그리고 60초마다 장치를 맞춘다. 장치를
  만들거나 지우면 곧바로 브리지 목록을 알리므로 새 네트워크를 몇 초 안에 쓸 수 있다. 스케줄러는 브리지 네트워크와
  같이 `oplv<VNI>`가 있는 노드에만 VM을 둔다. underlay가 없는 노드(이름을 풀지 못하면 경고)에는 장치를 만들지
  않으므로 그 네트워크의 VM이 가지 않는다. 다시 시작할 때 이름을 풀지 못하면 기록된 underlay를 그대로 쓰고, 이 노드의
  underlay를 모르는 동안에는 장치를 고치지도 지우지도 않는다. 장치 만들기가 실패하면 경고를 남기고 다음 차례에 다시
  하며, 한 네트워크의 실패가 다른 네트워크를 막지 않는다. 지운 네트워크의 장치는 opld가 지운다(브리지에 tap 따위
  포트가 남아 있으면 두 장치를 남기고 경고한다). opld를 멈춰도 장치는 남는다(도는 VM이 계속 쓴다). `cluster leave`로
  클러스터에서 빠지면 이 노드의 장치를 지운다.
- **소유 표시.** opld는 자기가 만든 두 장치의 alias를 `opl:<클러스터 태그>`(클러스터 CA 지문의 앞 12자)로 두고
  그런 장치만 고치거나 지운다. `oplv<숫자>`·`oplx<숫자>` 모양의 이름은 opLite가 쓰는 예약 이름이다. 그 모양이라도
  alias가 다른 장치(관리자가 만든 것, 같은 호스트의 다른 클러스터 것)나 포트가 있거나 올라가 있는 alias 없는 장치는
  건드리지 않는다(그 네트워크만 오류로 남는다). alias 없이 내려가 있고 포트도 없는 장치는 opld가 만들다 멈춘 것으로
  보고 다시 만든다. 노드의 브리지 목록에는 이 클러스터의 `oplv<VNI>`만 오르고, 다른 `oplv…`에는 VM tap을 붙이지
  않는다. `ip -d link show oplx1000`으로 id·local·dstport·alias를 볼 수 있다.
- **MTU.** 기본 1450은 underlay 1500에서 VXLAN 머리 50바이트를 뺀 값이다. underlay는 `MTU + 50`을 실어야 한다
  (점보 프레임이면 `--mtu 8950`). IPv6 underlay는 바깥 머리가 70바이트라 `MTU + 70`(기본 1450이면 1520)이다. 브리지·
  VXLAN 장치·tap이 그 MTU를 쓰고, virtio NIC는 `host_mtu`로 게스트에게도 알린다. e1000e NIC는 게스트 안에서 MTU를
  맞춘다. tap MTU는 관리자가 만든 브리지에서도 그 브리지의 MTU를 따른다(예: MTU 9000인 `vmbr0`이면 tap도 9000).
- **underlay.** 노드 사이에 **UDP 4789**를 연다. VXLAN은 암호화·인증이 없으므로 underlay는 믿을 수 있는 전용 망이어야
  한다(아무나 UDP 4789로 VM 네트워크에 패킷을 넣을 수 있다). 모든 노드의 underlay는 같은 주소 체계(IPv4끼리나 IPv6끼리)
  여야 한다. 다른 체계의 노드는 FDB에 넣지 않고 경고한다. underlay를 바꾸고 opld를 다시 띄우면 장치를 다시 만들고
  다른 노드가 FDB를 고친다.
- **한계.** IPAM·DHCP·라우팅·SNAT, EVPN·BGP, 암호화는 없다(게스트 주소는 게스트나 VM이 둔 DHCP 서버가 정한다).
  노드 사이 브로드캐스트를 노드 수만큼 복제하므로 수십 노드까지가 알맞다. 브리지 `oplv<VNI>`를 다시 만들면(누가
  지웠을 때 등) 이미 도는 VM의 tap은 다시 붙지 않는다 — VM을 껐다 켠다. 한 호스트에서 두 클러스터가 같은 VNI를
  쓰면 서로 방해할 수 있다(장치 이름이 겹쳐 나중 쪽의 그 네트워크는 장치를 만들지 못한다) — 클러스터마다 VNI를 나눈다.

## VM 방화벽 (보안 그룹)

보안 그룹은 VM NIC에 거는 규칙 목록이다(Proxmox의 VM 방화벽, 클라우드의 security group과 같은 자리). 그룹을 건
NIC는 방화벽이 켜져 규칙에 맞는 것만 들어오고, 노드의 opld가 nftables bridge family 규칙으로 tap에서 거른다.
그룹을 걸지 않은 NIC는 지금처럼 거르지 않는다.

```bash
oplctl security-group create web --description "web servers" \
  --rule "in accept tcp 80" --rule "in accept tcp 22 10.0.0.0/8" --rule "out drop tcp 25"
oplctl security-group rule add web "in accept icmp"          # 맨 끝에 더한다
oplctl security-group rule add web "in drop tcp 22 10.0.9.0/24" --at 1   # 1번 자리에 넣는다
oplctl security-group rule rm web 3                          # 3번 규칙(1부터)을 뺀다
oplctl security-group list
# NAME  RULES  DESCRIPTION
# web   4      web servers
oplctl security-group get web                                # 규칙을 순서대로(JSON)
oplctl vm create web1 --memory 1024 --disk root --nic lan,sg=web+ssh --start   # NIC에 그룹 둘(web, ssh)
oplctl vm nic-sg web1 0 web                                  # 0번 NIC의 그룹을 web 하나로(실행 중에도 된다)
oplctl vm nic-sg web1 0                                      # 0번 NIC의 그룹을 모두 뺀다(방화벽 끔)
oplctl security-group delete web                             # 그룹을 쓰는 NIC가 없어야 한다
```

웹은 "보안 그룹" 메뉴에서 그룹을 만들고 지우며, 그룹 화면의 규칙 표에서 방향·동작·프로토콜·포트·원격 CIDR을
고치고 순서를 바꿔 저장한다. VM 편집의 NIC마다 그룹을 고른다. API는 `/api/v1/security-groups`(목록·만들기·읽기·
지우기)와 `PATCH /api/v1/security-groups/{name}`(`{"description": …, "rules": […]}` 중 보낸 것만, `If-Match`로
revision), VM NIC의 `security_groups: ["web"]`, Node `status.firewall`이다.

- **규칙.** `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`. `in`은 VM으로 들어오는 것, `out`은 VM에서
  나가는 것이다. 프로토콜을 빼면 모든 프로토콜이다. `PORTS`는 목적지 포트 `22`나 `8000-8080`(tcp·udp에서만),
  `CIDR`은 `in`이면 보낸 쪽, `out`이면 받는 쪽 주소다(`10.0.0.0/8`, `2001:db8::/32`, 접두사를 빼면 주소 하나,
  `0.0.0.0/0`은 IPv4 전체). IPv4 CIDR은 IPv4 패킷에만, IPv6 CIDR은 IPv6에만 맞는다. `icmp`는 IPv4 CIDR과만,
  `icmpv6`는 IPv6 CIDR과만 쓴다. 그룹당 규칙 200개, NIC당 그룹 8개(겹치면 안 된다)까지다. 잘못된 규칙은 400
  (`rule 3: …`처럼 번호를 붙인다), VM이 쓰는 그룹 지우기는 409 `security_group_in_use`다.
- **순서와 기본 정책.** NIC의 그룹 순서대로, 그룹 안에서는 적힌 순서대로 보고 **처음 맞는 규칙**을 따른다. 맞는
  규칙이 없으면 **들어오기는 막고 나가기는 보낸다**. 그래서 규칙 없는 그룹 하나만 걸면 "들어오기 모두 막기, 나가기
  모두 허용"이다. 같은 브리지의 방화벽 VM 둘 사이 패킷은 보낸 VM의 `out`과 받는 VM의 `in`을 모두 통과해야 한다
  (`out accept`는 끝이 아니라 받는 쪽 검사로 넘긴다). 노드 자신과 주고받는 것(브리지에 노드 주소가 있을 때)도 같다.
- **늘 허용되는 것.** 이미 맺은 연결의 응답(conntrack `established,related`, 그래서 나가는 연결의 응답은 `in`
  규칙이 없어도 들어온다), ARP, IPv6 이웃 찾기(neighbor solicit·advert, router advert), DHCP 응답(UDP 67→68,
  547→546). 잘못된 연결 상태(`invalid`)의 패킷과 **NIC의 MAC이 아닌 보낸 MAC**(위조)은 늘 막는다.
- **NIC에 걸기.** 브리지·VXLAN 네트워크 NIC에만 건다(`user` 네트워크는 400). 없는 그룹은 400이다(VM 복제도
  원본 스펙의 그룹이 모두 있어야 한다). 노드를 정해 둔 VM에 그룹을 걸면 그 노드에 방화벽이 있어야 한다(409
  `firewall_unsupported`). 실행 중인 VM의 그룹을 바꾸거나(`vm nic-sg`, 웹 VM 편집, `PUT /api/v1/vms/{name}`) 그룹
  규칙을 바꾸면 VM을 다시 켜지 않아도 몇 초 안에 적용된다. 다만 실행 중인 VM은 그룹이 없어도 NIC를 더하거나 빼거나
  순서·MAC을 바꿀 수 없고, 그룹을 건 NIC가 있는(바꾸기 전이나 뒤) VM은 실행 중에 **그룹만** 바꿀 수 있다 — 먼저
  끈다(409 `vm_running`; 규칙은 NIC 순번과 MAC으로 tap을 찾으므로 도는 tap과 어긋나지 않게). 그룹이 없는 VM의
  네트워크·모델 변경은 지금처럼 다음 시작부터 적용된다.
- **누가 바꾸나.** 그룹을 만들고 바꾸고 지우는 것은 admin만이다. VM NIC에 그룹을 걸거나 떼는 것은 VM 쓰기
  권한(operator 포함)이라 operator도 VM의 방화벽을 끌 수 있다(그룹을 모두 떼면 거르지 않는다).
- **실시간 적용.** 노드마다 opld가 클러스터의 모든 방화벽 NIC를 테이블 `bridge opl_<클러스터 태그>`(CA 지문 앞 12자)
  하나로 만들어 `nft -f`로 한 번에 바꾼다(바꾸는 동안 규칙이 빈 순간이 없다). opld 시작, VM·보안 그룹이 바뀔 때
  (0.5초 모아 한 번), 그리고 60초마다(누가 테이블을 지웠어도 되살린다) 넣는다. 그룹을 건 VM은 켜기(마이그레이션
  받기 포함) 직전에 규칙을 넣고, 넣지 못하면 켜지 않는다("failed to start: firewall: …", 방화벽 없이 도는 순간이
  없다). 이미 도는 VM에 다시 붙거나 멈춘 채 남은 VM을 이어서 실행할 때(opld를 다시 시작한 뒤 등)도 먼저 넣는다. 방화벽 NIC가 하나도
  없으면 테이블을 지운다. opld를 멈춰도 규칙은 남고(도는 VM을 계속 지킨다), `cluster leave`로 빠지면 지운다.
  `nft list table bridge opl_<태그>`로 볼 수 있다(NIC마다 체인 `o_<tap>`·`i_<tap>`). 없는 그룹을 건 NIC(그룹을
  저장소에서 바로 지운 때 등)는 들어오기와 나가기를 모두 막는다.
- **비는 때와 실패.** 누가 `nft flush ruleset`을 하거나 `nftables.service`를 다시 읽으면(`systemctl reload
  nftables`, 보통 `flush ruleset`으로 시작한다) 테이블이 사라져 **다음 60초 차례까지 그 노드의 방화벽 VM이 열린다.**
  nftables 설정을 다시 읽은 뒤에는 opld를 다시 시작하면 곧바로 되살린다. 에이전트의 규칙 넣기 실패(VM 켜기 직전
  확인 말고)는 opld 로그에 경고로만 남고 API·웹에는 보이지 않는다 — 그동안은 마지막으로 넣은 규칙이 남는다.
- **노드 요구.** Linux, `nftables`(`nft` 명령), bridge family conntrack이 되는 커널 5.3 이상, root로 도는 opld.
  opld는 시작할 때 이 클러스터의 테이블을 실제로 넣어 보아(`table bridge opl_<태그> {}` — 있는 규칙은 그대로 둔다)
  되면 `node get`의 `status.firewall: true`를 알리고, 안 되면(nftables 없음, root 아님) 경고와 함께 `false`다.
  이 시험은 시작 때 한 번만 하므로, 실패했다면 nftables를 고친 뒤 **opld를 다시 시작해야** 방화벽이 켜진다.
  스케줄러는 그룹을 건 VM을 `true`인 노드에만 둔다. 위트니스와 macOS 노드는 방화벽이 없다.
- **업그레이드 먼저.** 보안 그룹을 VM NIC에 걸기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 opld는 `security_groups`가 있는 NIC를 읽지 못한다.
- **br_netfilter와 호스트 iptables.** 이 규칙은 브리지 단계에서 걸러 br_netfilter가 켜져 있어도 그대로 적용된다.
  다만 br_netfilter가 켜져 있고 `net.bridge.bridge-nf-call-iptables`가 1이면 브리지를 지나는 VM 트래픽이 호스트의
  iptables(`FORWARD`)도 지난다. Docker처럼 `FORWARD` 기본 정책을 DROP으로 두는 프로그램이 있으면 보안 그룹이 허용한
  VM 트래픽도 막힐 수 있다 — 그 sysctl을 0으로 두거나 iptables에서 VM 브리지를 허용한다.
- **한계.** 걸러내는 것은 L3·L4 주소·포트와 보낸 MAC이다. 보낸 IP 위조(MAC은 맞고 IP만 다른 것)와 ARP 위조는
  막지 않는다. 규칙에서 허용을 빼도 이미 맺은 연결은 끊기지 않는다(conntrack 항목이 끝날 때까지 간다). 다른 그룹을
  원격으로 가리키는 규칙, 로그·카운터, 속도 제한, 기본 정책 바꾸기는 없다.

## 스토리지 풀과 NFS

파일 풀은 두 종류다. **디렉터리 풀**(`--path`)은 노드의 디렉터리이고, **NFS 풀**(`--nfs`)은 NFS 서버의 내보내기를
opld가 노드마다 마운트한 것이다. 어느 쪽이든 볼륨은 그 안의 qcow2·raw 파일이다. 볼륨이 블록 장치인
**LVM-thin 풀**(`--lvm-thin`)과 **ZFS 풀**(`--zfs`)은 [아래](#lvm-thin-풀)에, 볼륨이 Ceph RBD 이미지인
**Ceph RBD 풀**(`--rbd`)은 [그 뒤](#ceph-rbd-풀)에, 볼륨이 공유 LUN 위의 LV인 **공유 LVM 풀**(`--lvm-shared`)은
[마지막](#공유-lvmiscsi-풀)에 있다.

```bash
sudo apt install nfs-common                          # 노드마다 NFS 클라이언트(mount.nfs)
oplctl pool create nas --nfs nas.lan:/srv/vm         # IPv6 서버는 --nfs '[fd00::5]:/srv/vm'
oplctl pool create nas2 --nfs 10.0.0.5:/srv/vm --nfs-options vers=4.2,hard   # mount -o nosuid,nodev,noexec,retry=0 뒤에 붙는다
oplctl pool create shared --path /mnt/vm --shared    # opLite 밖에서 마운트한 공유 디렉터리(모든 노드가 같은 파일을 본다)
oplctl pool set local --shared                       # 디렉터리 풀을 공유로(--no-shared는 노드 로컬로)
oplctl pool list                                     # NAME KIND LOCATION NODES READY(준비된 노드/쓸 수 있는 노드)
oplctl pool get nas                                  # 노드별 준비·용량·오류(-o json: 풀 리소스 그대로)
                                                     # 꺼진 노드는 node offline, 목록의 없는 노드는 node not found
```

- **NFS는 Linux 노드에서 root인 opld만** 마운트한다. 내보내기는 opld(root)가 쓸 수 있어야 한다(예: `no_root_squash`).
  macOS 노드는 NFS 풀을 마운트하지 않고 "nfs pools are not supported on macOS"로 알린다. 옵션을 주지 않으면 커널
  기본값으로 마운트한다.
- **마운트 옵션.** opld는 늘 `nosuid,nodev,noexec,retry=0`을 붙이고 `--nfs-options`를 그 뒤에 붙인다(`retry=0`은
  시간 제한으로 `mount`를 끝낸 뒤 mount.nfs가 계속 다시 시도하지 않게 한다. 다시 시도는 opld가 한다). 마운트의 성격을 바꾸는
  옵션(`bind`, `rbind`, `remount`, `move`, `suid`, `dev`, `exec`, `helper`, `bg`, `x-*`·`X-mount.*`)은 **400**으로 받지 않는다.
- **마운트 위치.** `{data_dir}/pools/{풀 id}`. opld가 내려가도 마운트는 그대로 두고, 다시 올라오면 그대로 쓴다.
  풀을 지우면(볼륨이 남아 있으면 지금처럼 409) 각 노드가 언마운트하고 마운트 지점을 지운다. 그 풀의 디스크를 쓰는
  VM이 그 노드에서 돌고 있으면 VM이 멈출 때까지 미룬다(`umount -f`는 쓰지 않는다). 풀을 쓸 노드는 만들 때 `--node`로
  정한다(생략하면 모든 노드).
- **주의: `{data_dir}`를 통째로 지우지 않는다.** 마운트된 NFS 풀이 있는 채로 `rm -rf /var/lib/opl`을 하면 마운트를
  따라 들어가 NFS 내보내기의 볼륨 파일까지 지운다. 노드를 클러스터에서 빼거나 opLite를 지워도 `{data_dir}/pools` 아래의
  NFS 마운트는 그대로 남는다. 먼저 opld를 멈추고 `{data_dir}/pools` 아래를 `umount`한 뒤 지운다.
- **NFS 풀은 늘 공유 풀이다.** API에서 `shared`를 적지 않으면 `true`가 되고, `false`면 **400** `invalid_request`다.
  opld 밖에서(fstab 등) 마운트한 공유 디렉터리는 디렉터리 풀을 공유로 만든다: `oplctl pool create --path ... --shared`
  (API `"shared": true`, 웹은 "공유 스토리지" 체크). `--shared`는 `--nfs`와 함께 쓰지 않는다.
- **공유 여부 바꾸기.** 디렉터리 풀만 `oplctl pool set POOL --shared|--no-shared`(관리자)로 바꾼다(API
  `PATCH /api/v1/storage-pools/{key}` `{"shared": bool}`, `If-Match` 선택. 웹은 스토리지 풀 목록의 "공유" 스위치와
  확인 대화상자). 다른 풀은 같은 값이어도 **400** `invalid_request`다(NFS·RBD·공유 LVM은 늘 공유, LVM-thin·ZFS는
  늘 로컬). 같은 값이면 바꾸지 않고 200이다. 로컬 → 공유는 언제나 된다(그 경로가 풀의 모든 노드에서 같은 공유 파일
  시스템인지는 관리자가 보증한다). 공유 → 로컬은 그 풀의 볼륨을 쓰는 VM이 볼륨을 만든 노드가 아닌 노드에 있거나
  마이그레이션 중이면 **409** `pool_in_use`(VM 이름)다. VM을 볼륨의 노드로 되돌린 뒤 다시 한다. 로컬이 되면 볼륨은
  만든 노드에 있는 것으로 돌아간다.
- **노드별 풀 상태.** 노드는 자기가 쓸 수 있는 풀마다 준비 여부·용량·오류를 노드 상태(`status.pools`)에 알린다.
  15초마다(`[pools] refresh_secs`), 그리고 풀을 만들거나 지우면 곧바로 확인한다. 디렉터리 풀은 디렉터리가 있고
  쓸 수 있어야(작은 파일을 만들고 지워 본다) 준비된다. NFS 풀은 마운트(30초 제한)와 쓰기 확인(10초 제한)이 되어야
  준비되고, 실패하면 `mount`의 오류나 "nfs server not responding"을 남긴다. 마운트는 풀마다 따로 돌아(진행 중이면
  "mount in progress") 멈춘 NFS 서버 하나가 다른 풀의 상태를 늦추지 않고, 실패한 마운트는 15초부터 두 배씩(최대 5분)
  기다렸다가 다시 시도한다. 상태에는 어느 풀의 것인지(`pool_id`)가 함께 적혀, 같은 이름으로 다시 만든 풀에 옛 상태를
  쓰지 않는다. opld를 다시 시작해도 적힌 풀 상태는 지우지 않는다. 웹의 스토리지 풀 화면과 노드 목록에서도 보인다
  (꺼진 노드는 "오프라인", 용량은 온라인이고 준비된 노드의 값).
- **배치 규칙.** 자동 배치는 VM의 디스크 풀을 쓸 수 있는 노드만 고른다. NFS 풀은 준비됐다고 알린 노드여야 하고,
  디렉터리 풀은 준비되지 않았다고 알린 노드만 빠진다(상태를 아직 알리지 않은 노드는 쓸 수 있다고 본다. 화면에는
  그 노드가 "아직 알린 상태 없음"으로 보인다). 공유가
  아닌 풀의 볼륨은 그 파일이 있는 노드(볼륨을 만든 노드)에만 배치된다. 전에는 노드 목록이
  빈 디렉터리 풀이면 파일이 없는 노드를 고를 수 있었다. 공유 풀은 준비된 어느 노드든 된다.
- **볼륨 작업 노드.** 볼륨 만들기는 접속한 노드에서 풀이 준비됐으면 그 노드, 아니면 풀이 준비된 온라인 노드 가운데
  하나에서 한다. 그런 노드가 없으면 **409** `pool_unavailable`이다. 공유가 아닌 풀의 볼륨 지우기·크기 늘리기는
  볼륨이 있는 노드에서 한다. 그 노드가 클러스터에서 빠졌으면 접속한 노드에서 한다(파일이 없으니 지우기는 리소스만
  지운다). 배치된 적 없는 VM의 스냅샷 작업은 첫 디스크가 공유 풀이면 볼륨을 만든 노드가 아니라 풀이 준비된 노드에서 한다. 이미지 가져오기와 복제는 풀이 그 노드의 목록에 없으면 지금처럼 `pool_not_on_node`,
  목록에는 있지만 준비되지 않았으면 `pool_unavailable`(노드가 알린 오류)이다. 웹의 업로드·복제·볼륨 만들기
  대화상자는 그 노드에서(볼륨 만들기는 어느 온라인 노드에서든) 쓸 수 있는 풀만 보인다.
- **VM 시작.** 노드가 준비되지 않았다고 알린 풀이나 이 노드에 실제로 마운트되지 않은 NFS 풀의 디스크가 있으면
  VM은 꺼지지 않고 `starting`에 머물며(`last_error`에 이유) 잠시 뒤 다시 시도한다. 일시 오류라 실패 횟수로 세지
  않으므로 NFS 서버가 돌아오면 시작한다. 드라이버도 마운트되지 않은 NFS 풀(빈 마운트 지점)에는 볼륨을 만들거나
  읽지 않고, 풀의 파일시스템 호출이 30초 안에 끝나지 않으면(멈춘 hard 마운트) 기다리지 않고 일시 오류로 돌려준다.
  NFS를 지원하지 않는 노드(macOS)에 놓인 NFS 풀 디스크의 VM은 기다려도 시작할 수 없으므로 꺼지고 `last_error`에 이유가 남는다.
- **올릴 때.** 새 opld는 첫 확인(시작 직후)에서 풀 상태를 채운다. 디렉터리 풀은 그 전에도 쓸 수 있다고 보지만,
  디렉터리가 없는 노드는 첫 확인 뒤 그 풀이 준비되지 않은 것으로 나온다. 공유가 아닌 풀의 VM은 이제 볼륨이 있는
  노드에만 배치된다. 공유 스토리지 위의 디렉터리 풀은 공유여야 VM이 다른 노드로 갈 수 있다. 기존 풀은
  `oplctl pool set POOL --shared`로 바꾼다.

## LVM-thin 풀

**LVM-thin 풀**(`--lvm-thin VG/THINPOOL`)은 노드의 LVM thin pool이다. 볼륨마다 thin LV `opl-{볼륨 id}`를 만들고
VM에는 블록 장치(`/dev/{VG}/opl-{볼륨 id}`, QEMU `host_device`)로 붙인다.

```bash
# VG와 thin pool은 관리자가 노드마다 같은 이름으로 미리 만든다(opLite는 만들지 않는다).
sudo apt install lvm2 thin-provisioning-tools
sudo vgcreate vg0 /dev/sdb
sudo lvcreate -T -L 500G vg0/tp
oplctl pool create fast --lvm-thin vg0/tp            # --node로 쓸 노드를 정한다(생략하면 모든 노드)
oplctl volume create db --pool fast --size 20G       # --format을 생략하면 raw(디렉터리·NFS 풀은 qcow2)
oplctl pool get fast                                 # 노드별 준비·thin pool 크기·남은 공간·오류
```

- **노드 로컬.** LV는 그 노드에만 있으므로 VM은 볼륨이 있는 노드에서만 돈다. `--shared`와 함께 쓸 수 없고 API의
  `"shared": true`는 **400** `invalid_request`다. VG·thin pool 이름은 `A-Za-z0-9+_.-`의 1–127자이고 `-`로 시작하지 않는다. LVM이 받지 않는 이름(`.`·`..`,
  `snapshot`·`pvmove`로 시작, `_tdata`·`_tmeta` 같은 LVM 내부 이름 조각을 담은 것)도 안 된다.
- **raw만.** LVM-thin 풀의 볼륨은 raw다. qcow2 볼륨을 만들거나 가져오면 **400** `invalid_request`("lvm-thin pools
  hold raw volumes"). `oplctl volume create`/`import`와 웹은 풀 종류로 형식을 정한다. API의 가져오기(`/volumes/import`,
  `/volumes/import/check`)도 `format`을 생략하면 풀 종류로 정한다(LVM-thin은 raw, 그 밖은 qcow2). 가져온 이미지는
  raw로 바뀐다.
- **업그레이드 먼저.** LVM-thin(또는 NFS) 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 opld는 모르는 종류의 풀을 읽지 못해 풀 목록부터 실패한다.
- **노드별 상태.** opld는 15초마다(`[pools] refresh_secs`) `lvs`로 thin pool을 확인해(10초 제한) 크기와 사용률로 용량을 알린다. VG나
  thin pool이 없거나 thin pool이 아니면 준비되지 않은 것이고(`"vg0/tp is not a thin pool"`, `lvs`의 오류,
  `"lvs timed out"`), NFS 풀처럼 준비됐다고 알린 노드에만 VM과 볼륨이 간다. LVM 명령은 opld(root)가 부르고 하나에
  60초를 넘으면 실패로 본다. 이때 `lvs` 말고 LV를 바꾸는 명령(`lvcreate`·`lvremove` 등)은 중간에 죽이면 LVM
  메타데이터가 어긋날 수 있으므로 죽이지 않고 뒤에서 끝나게 둔다. 그동안 같은 LV를 바꾸는 명령은 띄우지 않고
  잠시 뒤 다시 시도할 오류로 끝낸다.
- **macOS 미지원.** macOS 노드는 "lvm-thin pools are not supported on macOS"로 알리고, 그 노드에 놓인 LVM 디스크의
  VM은 이유와 함께 꺼진다.
- **VM 시작.** 비활성인 LV(노드를 다시 켠 뒤 등)는 VM을 시작할 때 opld가 활성화한다(`lvchange -ay -K`). 장치는
  `/dev/{VG}/{LV}`, 그 링크가 없으면 `/dev/mapper`의 노드를 쓴다. 활성화나 LVM 명령이 실패해도 VM을 끄지 않고
  잠시 뒤 다시 시도한다(볼륨이 없거나 정의가 틀렸을 때만 이유와 함께 끈다).
- **스냅샷.** VM 스냅샷은 thin 스냅샷 LV(`opl-{볼륨 id}-s-{스냅샷 id}`)다. VM의 디스크가 모두 qcow2(디렉터리·NFS
  풀의 내부 스냅샷)이거나 모두 LVM-thin이어야 하고, 섞이면 **409** `snapshot_unsupported`("disks use different
  snapshot methods")다. LVM-thin 디스크의 VM은 **메모리 스냅샷이 없다**: `memory`를 생략하면 디스크만 찍고, `true`면
  **409** `snapshot_unsupported`다. 예약 스냅샷은 `memory: true`여도 이런 VM은 디스크만 찍는다(웹은 메모리 선택을
  숨긴다).
- **실행 중 스냅샷의 짧은 멈춤.** 실행 중인 VM은 QMP `stop`으로 잠깐 멈춰 블록 I/O를 비운 뒤 디스크마다 `lvcreate`
  하고 `cont`로 다시 돌린다(디스크 수만큼 걸린다. 멈춘 시간은 opld 로그의 `paused_ms`). 하나라도 실패하면 먼저
  VM을 다시 돌리고 만든 스냅샷 LV를 지운다. 디스크 찍기가 시간을 넘겼으면(120초 한도나 LVM 명령의 60초) 찍기가
  끝난 디스크의 스냅샷 LV만 지우고, 아직 만들어지고 있을 수 있는 나머지는 남긴다(Task 오류에 태그가 나오고,
  다시 돌리지 못했으면 "; resuming the VM failed: …"가 붙는다). 남긴 것은 opld가 60초 뒤부터 60초 간격으로 열
  번까지 다시 지워 보고, 그래도 남은 것(스냅샷이 없는 opLite 태그)은 opld가 다시 시작할 때 지운다. 멈춘 채 두지 않는다: 디스크 찍기는 120초를 넘기면 실패로 끝내고,
  `cont`는 일시 오류면 1초 간격으로 세 번까지 해 보며(VM이 꺼졌으면 할 것이 없다), Task가 시간을 넘겨 버려져도
  뒤에서 다시 돌린다. 디스크는 찍었는데 다시 돌리지 못하면 스냅샷은 그대로 두고 Task만 실패로 끝낸다("snapshot
  taken; resuming the VM failed: …"). opld가 멈춘 채 죽었거나 다시 돌리지 못한 VM은 VM 실행 디렉터리의
  표시(`paused-by-opl`)를 보고 다시 붙을 때 이어서 실행한다(디스크를 다시 해석하지 않으므로 LVM이 응답하지 않아도
  된다. 실행 중인 VM에 남은 표시는 상태를 볼 때 지운다).
  하이퍼바이저가 실행 중이라고 알리는 VM만 멈춘다: 일시정지됐거나 QEMU가 스스로 멈춘 VM(`io-error`,
  `guest-panicked` 등)은 멈추지도 다시 돌리지도 않고 디스크만 찍고, QMP로 다룰 수 없는 VM은 디스크를 건드리기
  전에 실패한다.
- **되돌리기·지우기·복제·크기.** 되돌리기는 지금처럼 VM을 강제로 끈 뒤 스냅샷 LV에서 볼륨 LV를 새로 만든다(앞서
  실패한 되돌리기가 남긴 `opl-{볼륨 id}-revert-…` LV는 먼저 지운다. 한 단계가 시간을 넘기면 `lvs`로 다시 보고
  끝났으면 이어서 한다. 새 볼륨 LV는 활성화 건너뛰기 표시를 지운다(`lvchange -kn`)). 시간을 넘긴 이름 바꾸기가
  되돌리기가 실패한 뒤에 끝나 볼륨 LV가 `-revert-` 이름으로만 남으면, opld가 그 LV를 쓸 때(VM 시작, 되돌리기,
  스냅샷, 지우기 등) 이름을 되돌린다. 그 LV의 명령이 아직 돌고 있으면 잠시 뒤 다시 시도하고(VM을 끄지 않는다),
  `-revert-` LV가 여럿이면 어느 것인지 모르므로 손대지 않고 오류로 알린다(관리자가 맞는 것을 `opl-{볼륨 id}`로
  `lvrename`한다). 스냅샷 지우기는 VM이 돌고 있어도 된다. 볼륨을
  지우면 볼륨 LV를 먼저 지우고(쓰고 있으면 실패하고 스냅샷은 남는다) 그 스냅샷 LV와 남은 `-revert-` LV도 함께
  지운다(볼륨 LV가 `-revert-` 이름으로만 남았으면 위처럼 이름을 되돌린 뒤 볼륨으로 지운다). 스냅샷에서 복제하면 대상 풀이 LVM-thin이면 raw, 디렉터리·NFS 풀이면 스냅샷을 담을 수 있게 qcow2로
  만들고, 대상은 스냅샷 때의 크기로 만든다. qcow2 디스크의 스냅샷에서 복제하면 `qemu-img`가 지금 볼륨 크기만큼만
  읽는다(볼륨을 늘린 뒤 찍은 스냅샷을, 그보다 작던 때의 스냅샷으로 되돌린 볼륨에서 복제하면 늘린 부분이 빠진다). 새 LV에 써 넣을 때 thin pool이 새 블록을 0으로 채우면
  (`lv_attr`의 `z`, 기본값) 0인 부분은 쓰지 않고, `lvcreate -Zn`으로 만든 thin pool이면 모두 쓴다. 크기 늘리기는
  실행 중이어도 되고 `lvextend` 뒤 QEMU에 LV의 실제 크기(extent 단위로 올라간다)를 알린다(LV가 이미 그 크기
  이상이면 `lvextend`는 건너뛴다). 기록되는 볼륨 크기도 그 크기다.
- **thin pool이 가득 차면** 게스트의 쓰기가 실패한다. opLite는 thin pool을 늘리지 않으므로 노드별 남은 공간을 보고
  관리자가 늘린다(LVM의 `thin_pool_autoextend_threshold`도 쓸 수 있다).
- **호스트 LVM이 게스트 디스크를 읽지 않게.** 게스트가 디스크에 LVM을 쓰면 호스트의 LVM이 볼륨 LV에서 게스트의
  PV·VG를 찾아 활성화할 수 있다. lvm2 2.03의 기본값 `scan_lvs = 0`이면 LV를 보지 않으므로 그대로 두고, 바꿨다면
  `/etc/lvm/lvm.conf`의 `global_filter`로 `/dev/{VG}/opl-*`를 뺀다([ZFS 풀](#zfs-풀)의 zvol도 같다).

## ZFS 풀

**ZFS 풀**(`--zfs POOL/DATASET`)은 노드의 ZFS 데이터셋이다. 볼륨마다 그 바로 아래에 sparse zvol
`opl-{볼륨 id}`를 만들고(`volmode=dev`, `snapdev=visible`, `com.sun:auto-snapshot=false`) VM에는 블록 장치(`/dev/zvol/{DATASET}/opl-{볼륨 id}`,
QEMU `host_device`)로 붙인다. 블록 장치라는 점, 스냅샷 규칙, 실행 중 스냅샷의 짧은 멈춤은 [LVM-thin 풀](#lvm-thin-풀)과
같다. 다른 점만 적는다.

```bash
# zpool과 데이터셋은 관리자가 노드마다 같은 이름으로 미리 만든다(opLite는 만들지 않는다).
sudo apt install zfsutils-linux
sudo zpool create tank /dev/sdb
sudo zfs create tank/opl
oplctl pool create z --zfs tank/opl                  # --node로 쓸 노드를 정한다(생략하면 모든 노드)
oplctl volume create db --pool z --size 20G          # --format을 생략하면 raw
oplctl pool get z                                    # 노드별 준비·데이터셋 크기·남은 공간·오류
```

- **노드 로컬, raw만.** zvol은 그 노드에만 있으므로 VM은 볼륨이 있는 노드에서만 돈다. `--shared`와 함께 쓸 수
  없고 API의 `"shared": true`는 **400** `invalid_request`다. 볼륨은 raw이고 qcow2 볼륨을 만들거나 가져오면 **400**
  ("zfs pools hold raw volumes"). `oplctl`·웹·API 가져오기는 형식을 생략하면 raw로 정한다. 데이터셋 이름은 `/`로
  나뉜 구성 요소가 `A-Za-z0-9_.:-`로 되고(비거나 `.`·`..`이거나 `-`로 시작하면 안 된다) 맨 앞(풀 이름)이 영문자로
  시작하는 177자 이하다(zvol 스냅샷 이름 `{DATASET}/opl-{볼륨 id}@{스냅샷 id}`가 ZFS 이름 상한 255자 안에 들게).
  볼륨 크기는 1 MiB 배수로 올린다.
- **업그레이드 먼저.** ZFS 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 opld는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** opld는 15초마다 `zfs get -Hp -o value available,used,type`으로 데이터셋을 확인해(10초 제한)
  `available`+`used`를 크기로, `available`을 남은 공간으로 알린다. 데이터셋이 없으면(`zfs`의 오류), zvol이면
  (`"tank/opl is a volume, not a filesystem dataset"`), 시간을 넘기면(`"zfs timed out"`) 준비되지 않은 것이고,
  준비됐다고 알린 노드에만 VM과 볼륨이 간다. ZFS 명령은 opld(root)가 `LC_ALL=C`로 부르고 하나에 60초를 넘으면
  실패로 본다. LVM처럼 읽기(`zfs list`·`zfs get`) 말고 바꾸는 명령은 죽이지 않고 뒤에서 끝나게 두며, 그동안 같은
  zvol을 바꾸는 명령은 띄우지 않고 잠시 뒤 다시 시도할 오류로 끝낸다.
- **macOS 미지원.** macOS 노드는 "zfs pools are not supported on macOS"로 알리고, 그 노드에 놓인 ZFS 디스크의
  VM은 이유와 함께 꺼진다.
- **VM 시작.** zvol은 늘 활성이므로 활성화할 것이 없다. 장치 노드가 아직 없으면(udev) VM을 끄지 않고 잠시 뒤 다시
  시도한다. 새 zvol은 만든 뒤 udev가 장치를 다 다루기를 기다리고(`udevadm settle`, 10초. udev가 없거나 시간을
  넘기면 기록만 하고 장치 노드가 잠시 사라지지 않는지 두 번 본다) 장치 노드를 10초까지 기다리며, 생기지 않으면
  지우고 실패한다. 되돌린 뒤와 스냅샷·볼륨을 지우기 전에도 udev를 기다린다(스냅샷을 찍은 뒤에는 VM이 멈춘 시간을
  늘리지 않게 기다리지 않는다).
- **스냅샷.** VM 스냅샷은 ZFS 스냅샷 `{DATASET}/opl-{볼륨 id}@{스냅샷 id}`다. VM의 디스크는 모두 qcow2이거나 모두
  LVM-thin이거나 모두 ZFS여야 한다(LVM-thin과 ZFS가 섞여도 **409** "disks use different snapshot methods").
  메모리 스냅샷은 없다(`memory: true`면 **409**, 예약은 디스크만). 실행 중인 VM은 LVM-thin처럼 잠깐 멈추고, 같은
  풀의 디스크를 `zfs snapshot` 한 명령으로 함께(원자적으로) 찍는다. 스냅샷 지우기는 VM이 돌고 있어도 된다.
  찍기가 시간을 넘겨 남긴 스냅샷은 LVM-thin처럼 뒤에서 다시 지우고, 남은 것은 opld가 다시 시작할 때 지운다.
  Task 제한 시간에 걸리거나 opld가 다시 시작해 중단된 스냅샷 Task도 같다. 뒤에서 지울 때 그 zvol에 시간을 넘긴
  명령이 아직 돌고 있으면 스냅샷이 곧 생길 수 있으므로 지운 것으로 보지 않고 다음 차례에 다시 해 본다.
- **가장 최근 스냅샷으로만 되돌린다.** `zfs rollback`은 더 나중의 스냅샷을 지워야 하므로 opLite는 그렇게 하지
  않는다. ZFS 디스크가 있는 스냅샷이 VM의 가장 최근 스냅샷(만든 시각, 같으면 id 순, 만드는 중인 것 포함)이
  아니면 **409** `snapshot_unsupported`("zfs disks can only revert to the latest snapshot")로 Task를 만들기 전에
  거절한다. 더 나중의 스냅샷이 같은 ZFS 볼륨을 담지 않았으면(그 뒤에 디스크를 바꿨다) 막지 않는다. Task도 VM을
  끄기 전에 디스크마다 가장 최근 ZFS 스냅샷이 그것인지 다시 보고, 아니면 VM을 그대로 두고 더 최근 스냅샷의
  이름(opLite 밖에서 만든 것이면 그 ZFS 이름, 시간을 넘긴 찍기가 남겨 opld가 뒤에서 지우는 것이면 그렇다고 —
  잠시 뒤 다시 하거나 opld를 다시 시작한다)과 함께 실패한다. 드라이버도 한 번 더 본다. 웹은
  그런 스냅샷의 되돌리기 버튼을 이유와 함께 막는다. 더 오래된 스냅샷으로 가려면 그 뒤의 스냅샷을 먼저 지운다.
  되돌리기는 지금처럼 VM을 강제로 끈 뒤 하고, `zfs rollback` 앞뒤로 zvol 장치의 커널 버퍼를 비운다(`blockdev
  --flushbufs`. 비우지 못하면 옛 내용이 읽힐 수 있으므로 Task가 실패하고, 앞에서 못 비우면 되돌리지 않는다).
- **자동 스냅샷 도구 주의.** opl zvol에 opLite 밖에서 스냅샷이 생기면 그보다 앞의 스냅샷으로는 되돌릴 수 없다.
  zvol에 `com.sun:auto-snapshot=false`를 두므로 이 속성을 따르는 zfs-auto-snapshot은 찍지 않는다. sanoid처럼 이
  속성을 보지 않는 도구는 설정에서 이 데이터셋을 빼거나(`autosnap = no`) 재귀 설정이 opl zvol에 닿지 않게 한다.
- **호스트 LVM이 게스트 디스크를 읽지 않게.** 게스트가 디스크에 LVM을 쓰면 호스트의 LVM이 zvol 장치(`/dev/zd*`)에서
  게스트의 PV·VG를 찾아 활성화할 수 있다. `/etc/lvm/lvm.conf`의 `devices` 절에 `global_filter = [ "r|^/dev/zd.*|" ]`를
  둔다(필터가 이미 있으면 맨 앞에 더한다). LVM-thin 풀의 `/dev/{VG}/opl-*`도 같은 이유로 뺀다([위](#lvm-thin-풀)).
- **지우기·복제·크기.** 볼륨을 지우면 zvol을 스냅샷과 함께 지운다(`zfs destroy -r`, VM이 쓰고 있으면 실패한다.
  방금 닫힌 장치를 udev 등이 아직 열고 있어 "busy"면 1초 간격으로 세 번까지 다시 해 본다. 스냅샷 지우기도 같다).
  시간을 넘긴 만들기가 아직 돌고 있어 zvol이 없으면 볼륨을 지운 것으로 보지 않고 잠시 뒤 다시 해 본다(LVM-thin도 같다).
  스냅샷에서 복제하면 스냅샷 장치(`/dev/zvol/…@{스냅샷 id}`)를 읽어 대상 풀이 블록 풀(ZFS·LVM-thin)이면 raw,
  디렉터리·NFS 풀이면 qcow2로 만들고, 대상은 스냅샷 때의 크기로 만든다(`zfs send`가 아니라 `qemu-img convert`로
  복사한다). 크기 늘리기는 실행 중이어도 되고 `zfs set volsize` 뒤 QEMU에 zvol의 실제 크기(1 MiB 단위로
  올라간다)를 알린다(이미 그 크기 이상이면 건너뛴다).
- **데이터셋이 가득 차면** 게스트의 쓰기가 실패한다(sparse zvol은 미리 자리를 잡지 않는다). 노드별 남은 공간을
  보고 관리자가 zpool을 늘린다.

## Ceph RBD 풀

**Ceph RBD 풀**(`--rbd POOL[/NAMESPACE]`)은 Ceph 클러스터의 풀(과 그 안의 RBD 네임스페이스)이다. 볼륨마다 raw RBD
이미지 `opl-{볼륨 id}`를 만들고(`--image-format 2`) QEMU가 `rbd` 블록 드라이버로 Ceph에 직접 붙는다(커널 매핑
`rbd map`을 하지 않으므로 노드에 블록 장치가 생기지 않는다). 모든 노드가 같은 이미지를 보는 **공유 풀**이다. 스냅샷
규칙과 실행 중 스냅샷의 짧은 멈춤은 [LVM-thin 풀](#lvm-thin-풀)과 같다. 다른 점만 적는다.

```bash
# Ceph 클러스터와 풀은 관리자가 미리 만든다(opLite는 만들지 않는다).
ceph osd pool create rbd && rbd pool init rbd
# 노드마다: rbd·ceph 명령과 QEMU의 rbd 블록 드라이버, ceph.conf와 키링
sudo apt install ceph-common qemu-block-extra
sudo cp ceph.conf ceph.client.admin.keyring /etc/ceph/
oplctl pool create ceph --rbd rbd                    # client.admin, /etc/ceph/ceph.conf
oplctl pool create vms --rbd rbd/opl --rbd-user opl --rbd-conf /etc/ceph/opl.conf   # 네임스페이스·사용자·conf
oplctl volume create db --pool ceph --size 20G       # --format을 생략하면 raw
oplctl pool get ceph                                 # 노드별 준비·Ceph 풀 크기·남은 공간·오류, user·conf
```

- **공유, raw만.** VM은 풀이 준비된 어느 노드에서나 돈다(볼륨을 만든 노드에 묶이지 않는다). `--shared`는 필요 없고
  함께 쓸 수 없으며 API의 `"shared": false`는 **400** `invalid_request`("rbd pools are always shared")다. 쓸 노드는
  `--node`로 정한다(생략하면 모든 노드). 볼륨은 raw이고 qcow2 볼륨을 만들거나 가져오면 **400**("rbd pools hold raw
  volumes"). `oplctl`·웹·API 가져오기는 형식을 생략하면 raw로 정한다. 볼륨 크기는 1 MiB 배수로 올린다.
- **이름과 접속.** Ceph 풀·네임스페이스·사용자는 `A-Za-z0-9_.-`의 1–63자이고 `-`로 시작하지 않는다. 사용자는
  `client.` 없이 적는다(`client.opl`이 아니라 `opl`. 생략하면 `admin`. `client.`로 시작하면 **400** "write the Ceph user
  without client."). `--rbd-conf`는 노드의 ceph.conf 절대
  경로이고(제어 문자·`,`·`:`·`=` 없이, 생략하면 Ceph 기본 `/etc/ceph/ceph.conf`) 키링은 그 conf가 가리키는 곳(기본
  `/etc/ceph/ceph.client.{사용자}.keyring`)에 노드마다 둔다. opld와 opld가 띄우는 QEMU가 같은 사용자로 읽는다. 사용자는
  그 풀의 이미지를 만들고 지우고 스냅샷할 수 있어야 하고 `ceph df`를 읽을 수 있어야 한다. `admin`이 아닌 사용자는
  `ceph auth get-or-create client.opl mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd'`로
  만든다(CI가 이 caps의 사용자로 네임스페이스와 명시적 conf를 써서 시험한다).
- **업그레이드 먼저.** RBD 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 opld는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** opld는 먼저 VM을 띄우는 QEMU에 rbd 블록 드라이버가 있는지 한 번 본다(없으면 `"qemu has no rbd
  block driver"` — Ubuntu는 `qemu-block-extra`). 그다음 15초마다 `rbd ls`로 풀(네임스페이스)에 닿는지 보고
  `ceph df detail`에서 그 풀의 `max_avail`+`stored`를 크기로, `max_avail`을 남은 공간으로 알린다(Ceph 풀에 바이트
  쿼터가 있으면 남은 공간은 쿼터에서 `stored`를 뺀 것까지다. 같은 Ceph 풀을 네임스페이스만 달리해 여러 opLite 풀이
  나눠 쓰면 각 풀이 Ceph 풀 전체의 크기·남은 공간을 보인다. 합쳐 10초 제한,
  넘으면 `"rbd timed out"`). 실패하면 `rbd`·`ceph`의 오류 첫 줄이 이유로 남고, 준비됐다고 알린 노드에만 VM과
  볼륨 작업이 간다. RBD 명령은 `LC_ALL=C`로 부르고 하나에 60초(이미지를 지우는 `rbd snap purge`·`rbd rm`과 되돌리는
  `rbd snap rollback`은 이미지 크기에 따라 오래 걸리므로 600초)를 넘으면 실패로 본다. LVM처럼 읽기 말고 바꾸는 명령은
  죽이지 않고 뒤에서 끝나게 두며, 그동안 이 노드에서 같은 이미지를 바꾸는 명령은 띄우지 않고 잠시 뒤 다시 시도할
  오류로 끝낸다. 이 표시는 노드마다 따로이므로 RBD 볼륨의 만들기·지우기·크기 늘리기(쓰는 VM이 없을 때) Task는 요청을
  받은 노드가 아니라 늘 풀이 준비된 온라인 노드 가운데 id가 가장 작은 노드가 한다.
- **macOS 미지원.** Homebrew QEMU에는 rbd 드라이버가 없다. macOS 노드는 "rbd pools are not supported on macOS"로
  알리고, 그 노드에 놓인 RBD 디스크의 VM은 이유와 함께 꺼진다.
- **VM 시작.** 활성화할 것이 없고 이미지가 있는지(`rbd info`)만 본다. 이미지가 없으면 VM을 이유와 함께 끄고,
  Ceph에 닿지 않는 등 `rbd`가 실패하면 끄지 않고 잠시 뒤 다시 시도한다. 이 노드에서 그 이미지를 바꾸는 명령(시간을
  넘긴 되돌리기 등)이 아직 돌고 있어도 잠시 뒤 다시 시도한다.
- **스냅샷.** VM 스냅샷은 RBD 스냅샷 `opl-{볼륨 id}@{스냅샷 id}`다. VM의 디스크는 모두 qcow2이거나 모두
  LVM-thin이거나 모두 ZFS이거나 모두 RBD여야 한다(블록 풀끼리 섞여도 **409** "disks use different snapshot
  methods"). 메모리 스냅샷은 없다(`memory: true`면 **409**, 예약은 디스크만). 실행 중인 VM은 LVM-thin처럼 잠깐
  멈추고 디스크를 하나씩 찍는다(RBD에는 여러 이미지의 원자적 스냅샷이 없지만 VM이 멈춘 동안 찍으므로 VM에게는
  한 시점이다). 스냅샷 지우기는 VM이 돌고 있어도 된다. 찍기가 시간을 넘겨 남긴 스냅샷은 LVM-thin처럼 뒤에서 다시
  지우고, 남은 것은 opld가 시작할 때와 그 뒤 30분마다 지운다. 공유 풀이므로 이 정리는 풀을 쓸 수 있는 온라인 노드
  가운데 id가 가장 작은 노드 하나가 맡고, 다른 노드가 막 찍은 스냅샷을 건드리지 않게 만든 지 30분이 지난 것만
  지운다.
- **아무 스냅샷으로나 되돌린다.** ZFS와 달리 더 오래된 스냅샷으로도 되돌릴 수 있고 그 뒤의 스냅샷은 남는다(다시
  더 최근 스냅샷으로 갈 수 있다). 되돌리기는 지금처럼 VM을 강제로 끈 뒤 `rbd snap rollback`을 한다. 이미지를 연
  클라이언트(다른 노드의 QEMU 등)가 있으면 잠시 뒤 다시 해 볼 오류로 끝난다.
- **지우기·복제·크기.** 볼륨을 지우면 이미지를 스냅샷과 함께 지운다(`rbd snap purge`, `rbd rm`). 이미지를 연
  클라이언트가 있으면(`rbd status`) 스냅샷도 지우지 않고 잠시 뒤 다시 해 볼 오류로 끝난다. 시간을 넘긴 만들기가 아직 돌고 있어 이미지가 없으면 볼륨을 지운 것으로 보지 않고 잠시 뒤 다시 해 본다.
  풀을 지워도 opLite 리소스만 지우고 Ceph 풀과 그 안의 다른 이미지는 그대로 둔다. 복제·복사는 `rbd clone`이 아니라
  `qemu-img convert`로 하는 전체 복사다(QEMU의 rbd 드라이버로 사용자·conf·네임스페이스를 넘긴다). 스냅샷에서
  복제하면 그 RBD 스냅샷을 읽어 대상 풀이 블록 풀(RBD·LVM-thin·ZFS)이면 raw, 디렉터리·NFS 풀이면 qcow2로 만들고,
  대상은 스냅샷 때의 크기로 만든다. 크기 늘리기는 실행 중이어도 되고 `rbd resize` 뒤 QEMU에 새 크기를 알린다(이미
  그 크기 이상이면 건너뛴다).
- **Ceph 풀이 가득 차면** 게스트의 쓰기가 실패한다(이미지는 thin이다). `oplctl pool get`의 남은 공간을 보고
  관리자가 Ceph 클러스터를 늘린다.

## 공유 LVM(iSCSI) 풀

**공유 LVM 풀**(`--lvm-shared VG`)은 모든 노드가 보는 LUN(iSCSI 타깃이나 FC·멀티패스 SAN) 위의 **공유 VG**다.
관리자가 큰 LUN 하나에 공유 VG를 한 번 만들면 opLite가 그 안에 볼륨마다 두꺼운 LV `opl-{볼륨 id}`를 만들고, VM에는
[LVM-thin 풀](#lvm-thin-풀)처럼 블록 장치(`/dev/{VG}/opl-{볼륨 id}`, QEMU `host_device`)로 붙인다. 여러 노드가 같은
VG를 바꾸므로 노드 사이 잠금은 LVM의 **lvmlockd**(sanlock 또는 dlm)가 맡고, LV는 쓰는 노드 하나에서만 켠다(배타
활성화). `--iscsi-portal`·`--iscsi-target`을 주면 opld가 노드마다 그 타깃에 로그인한다. 모든 노드가 같은 LV를 보는
**공유 풀**이고 **스냅샷이 없다**.

관리자가 미리 할 일(opLite는 타깃·LUN·VG를 만들지 않고 잠금 데몬을 설치하지 않는다):

```bash
# 1. 스토리지: LUN 하나를 모든 노드에 내보낸다. iSCSI면 타깃의 이니시에이터 ACL로 노드만 들인다(CHAP은 지원하지 않는다).
# 2. 노드마다: 잠금 데몬과 iSCSI 이니시에이터. sanlock은 watchdog이 필요하다(하드웨어가 없으면 softdog).
sudo apt install lvm2-lockd sanlock open-iscsi
sudo modprobe softdog                                # 하드웨어 watchdog이 없을 때만(부팅 때 올라오게 둔다)
sudo sed -i 's/^\s*#\?\s*use_lvmlockd = .*/\tuse_lvmlockd = 1/' /etc/lvm/lvm.conf   # global 절의 use_lvmlockd = 1
printf 'local {\n\thost_id = 1\n}\n' | sudo tee /etc/lvm/lvmlocal.conf   # host_id는 노드마다 달라야 한다(1–2000)
sudo systemctl enable --now wdmd sanlock lvmlockd
# 3. 한 노드에서 한 번: LUN에 로그인해 공유 VG를 만든다(잠금 시작까지 수십 초 걸릴 수 있다).
sudo iscsiadm -m discovery -t sendtargets -p san.lan
sudo iscsiadm -m node -T iqn.2003-01.org.example:storage -p san.lan --login
sudo vgcreate --shared san /dev/disk/by-path/ip-10.0.0.9:3260-iscsi-iqn.2003-01.org.example:storage-lun-0
# 그다음은 opLite가 한다: 노드마다 로그인하고 잠금을 시작한다(vgchange --lockstart).
oplctl pool create san --lvm-shared san --iscsi-portal san.lan:3260 --iscsi-target iqn.2003-01.org.example:storage
oplctl pool create fc --lvm-shared fcvg              # 노드가 이미 LUN을 보면(FC 등) iSCSI 옵션 없이
oplctl volume create db --pool san --size 20G        # --format을 생략하면 raw
oplctl pool list                                     # LOCATION: san @ san.lan:3260 iqn.2003-01.org.example:storage
oplctl pool get san                                  # 노드별 준비·VG 크기·남은 공간·오류
```

- **공유, raw만, 스냅샷 없음.** VM은 풀이 준비된 어느 노드에서나 돈다. `--shared`는 필요 없고 함께 쓸 수 없으며
  API의 `"shared": false`는 **400** `invalid_request`("lvm-shared pools are always shared")다. 볼륨은 raw이고 qcow2
  볼륨을 만들거나 가져오면 **400**("lvm-shared pools hold raw volumes"). `oplctl`·웹·API 가져오기는 형식을 생략하면
  raw로 정한다. 공유 LVM 디스크가 있는 VM의 스냅샷은 **409** `snapshot_unsupported`("disks in storage pool … cannot
  be snapshotted")이고 그런 VM의 예약 스냅샷도 실패한다(웹은 스냅샷 버튼을 이유와 함께 막고 예약을 켜면 알린다).
  그래서 복제는 멈춘 VM의 지금 상태로만 한다.
- **이름과 접속.** VG 이름은 [LVM-thin 풀](#lvm-thin-풀)과 같은 규칙이다(틀리면 **400** "invalid lvm name …"). 포털은
  `호스트[:포트]`이고 IPv6 주소는 대괄호로 감싼다(`[fd00::5]:3260`, 포트 1–65535). 타깃은 `iqn.`·`eui.`·`naa.`로
  시작하고 소문자·숫자·`.`·`:`·`-`로 된 223자 이하다. 틀리면 **400**("invalid iscsi portal …", "invalid iscsi
  target …"). `--iscsi-portal`과 `--iscsi-target`은 함께 쓴다.
- **업그레이드 먼저.** 공유 LVM 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 opld는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** opld(root)는 15초마다 풀을 준비한다: iSCSI 타깃이 있으면 그 타깃의 세션이 없을 때 `iscsiadm`으로
  찾아 로그인하고(`-m discovery -t sendtargets`, `-m node --login`, 그 뒤 `udevadm settle`. 세션은 타깃 이름으로 찾는다),
  `vgchange --lockstart`로 VG 잠금을 시작한 뒤(이미 시작했으면 곧바로 끝난다) `vgs`(10초 제한)로 VG를 본다. 잠금
  종류가 `sanlock`·`dlm`이면 준비되고 VG의 크기·남은 공간을 알린다. 잠금이 없는 VG는 `"volume group san is not a
  shared (lvmlockd) VG"`, 그 밖의 실패는 명령의 오류 첫 줄이다(`vgs`가 시간을 넘기면 `"vgs timed out"`). 로그인과
  처음 잠금 시작은 sanlock 임대 때문에 오래 걸릴 수 있으므로 죽이지 않는다: 10초 안에 끝나지 않으면 `"iscsi login or
  vg lock start in progress"`로 알리고 다음 확인에서 결과를 거둔다. 풀을 지워도 로그아웃하거나 잠금을 멈추지 않는다.
- **macOS 미지원.** macOS 노드는 "lvm-shared pools are not supported on macOS"로 알리고, 그 노드에 놓인 공유 LVM
  디스크의 VM은 이유와 함께 꺼진다.
- **볼륨 만들기.** LV는 켜지 않고 만든 뒤(`lvcreate -an -Zn -Wn`, 크기는 1 MiB 배수로 올린다) 이 노드에서 배타
  활성화해 `blkdiscard -z`로 모두 0으로 채우고 끈다. 0 채우기는 크기에 비례해 걸리므로 시간 제한이 없다(SAN이 WRITE
  SAME·UNMAP 오프로드를 하면 빠르다). 켜거나 채우지 못하면 LV를 지운다. 두꺼운 LV는 만들 때 자리를 잡으므로 VG가
  모자라면 만들기가 실패하고 게스트의 쓰기가 나중에 실패하지는 않는다. 가져오기·복사의 대상도 이렇게 만든 뒤
  `qemu-img convert -n --target-is-zero`로 써 넣고 끈다.
- **활성화의 수명.** LV는 쓰는 동안만 그 노드에서 켜져 있다. VM을 시작할 때 opld가 디스크 LV를 배타 활성화하고
  (`lvchange -aey`), QEMU가 끝나면(멈춤·충돌·포기) 그리고 시작이 실패하면 끈다(`-an`. 끄지 못하면 경고만 남긴다).
  켜진 채 남은 LV(opld가 그 전에 내려갔을 때 등)는 opld가 시작할 때와 그 뒤 30분마다 끈다. 이때 이 노드에서
  실행 중이거나 시작하려는 VM의 디스크, 이 노드에서 끝나지 않은 작업의 볼륨, 만드는 중인 볼륨은 그대로 둔다.
- **다른 노드가 쓰는 중.** 다른 노드가 켜 둔 LV를 켜거나 지우거나 늘리려 하면 lvmlockd가 거절하고, opLite는 잠시
  뒤 다시 해 볼 오류로 끝낸다("timed out waiting for another node to stop using the lv", VG 잠금이면 "… to release
  the shared vg lock"). VM은 꺼지지 않고 `starting`에서 다시 시도한다. **노드가 죽으면** 그 노드가 켜 둔 LV는 sanlock
  임대가 끝날 때까지(기본 수 분) 다른 노드에서 쓸 수 없다. 한 노드에서 **끄기가 실패하면**(경고만 남긴다) 그
  노드의 정리(opld 시작 때와 30분마다)가 끌 때까지 다른 노드에서 그 볼륨을 켜거나 늘리거나 지울 수 없다(같은 일시
  오류).
- **볼륨 작업 노드.** LVM 명령은 LVM-thin처럼 하나에 60초를 넘으면 실패로 보되 LV를 바꾸는 명령은 죽이지 않고, 그
  표시가 노드마다 따로이므로 공유 LVM 볼륨의 만들기·지우기·크기 늘리기(쓰는 VM이 없을 때) 작업은 [Ceph RBD
  풀](#ceph-rbd-풀)처럼 풀이 준비된 온라인 노드 가운데 id가 가장 작은 노드가 한다. 가져오기는 올린 파일이 있는 노드,
  실행 중인 VM의 디스크 크기 늘리기는 VM의 노드에서 한다.
- **지우기·복제·크기.** 볼륨을 지우면 LV를 지운다(`lvremove`). 시간을 넘긴 만들기가 아직 돌고 있어 LV가 없으면 지운
  것으로 보지 않고 잠시 뒤 다시 해 본다. 풀을 지워도 opLite 리소스만 지우고 VG와 그 안의 다른 LV는 그대로 둔다.
  복제·복사는 원본 LV를 켜서 읽고 끝나면 끈다. 대상 풀이 블록 풀이면 raw, 디렉터리·NFS 풀이면 qcow2로 만든다.
  크기 늘리기는 실행 중이어도 된다. `lvextend` 뒤 늘린 부분을 `blkdiscard -z`로 0으로 채우고(새 익스텐트에 지운
  볼륨의 옛 데이터가 남아 있을 수 있다. 시간 제한 없음) 실행 중이면 QEMU에 새 크기를 알린다. 꺼진 볼륨은 채우는
  동안만 배타 활성화한다. 늘리기 전 크기를 LV 태그(`opl-zero-from-…`)로 적어 두고 다 채우면 뗀다. 0 채우기가
  실패하면 작업이 실패하므로 **크기 늘리기가 실패하면 다시 한다**: LV가 이미 그 크기여도 남은 부분을 다시 채운다.
  다시 하기 전에 VM을 시작하거나 그 볼륨을 복제·복사하면 LV를 켤 때 남은 부분을 먼저 마저 채우고 태그를 뗀다(옛
  데이터를 내주지 않는다). 이 0 채우기도 남은 크기에 비례하므로 **0 채우기가 실패한 볼륨의 VM은 다음 시작이 오래
  걸릴 수 있다**. 크기 늘리기 작업이 실행 중인(`running`) 디스크의 VM은 켜지 않고 `starting`에서 기다리고(아직
  시작하지 않은(`pending`) 작업은 막지 않는다), VM이 켜지거나
  꺼지는 중이면 그 디스크의 크기 늘리기는 409 `vm_busy`다(모든 풀이 같다). 크기 늘리기 작업을 하던 **노드가
  죽으면** 그 작업은 그 노드가 돌아오거나 클러스터에서 빠질 때까지 끝나지 않으므로, 그동안 그 디스크의 VM은
  시작하지 않고 `starting`에서 기다린다(오프라인 노드의 아직 시작하지 않은 작업은 약 1분 뒤 실패로 정리된다).
- **호스트 LVM이 게스트 디스크를 읽지 않게.** [LVM-thin 풀](#lvm-thin-풀)과 같다(lvm2 기본값 `scan_lvs = 0`이면
  그대로 둔다).

## 라이브 마이그레이션

실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(`qm migrate`·vMotion). 원본 노드의 작업(Task)이 지휘하고, 메모리는
두 QEMU가 노드 인증서로 TLS를 맺어 직접 주고받는다. 대상 QEMU는 멈춘 채 받고, 받기를 확인한 뒤 클러스터 기록의
VM 노드를 대상으로 바꾸는 순간 확정된다. 그 뒤에만 대상에서 VM이 이어서 돈다(두 QEMU가 함께 돌지 않는다).

```bash
oplctl vm migrate web node2              # 끝날 때까지 기다린다: "vm web: migrated to node2"
oplctl vm list                           # 옮기는 동안 PHASE migrating, NODE "원본 -> 대상"
```

웹은 VM 상세의 "마이그레이션"(실행 중일 때)에서 대상 노드를 고른다. API는 `POST /api/v1/vms/{key}/migrate`
(`{"node": "이름 또는 id"}`, 운영자 권한, 202와 작업)다.

```toml
# /etc/opl/opld.toml (노드마다, 생략하면 아래 기본값)
[migration]
ports = "60000-60099"     # 받는 쪽 QEMU가 듣는 TCP 포트 범위(둘 다 포함)
bandwidth_mib = 0         # 보내는 대역폭 상한(MiB/s), 0이면 QEMU 기본값
timeout = "30m"           # 이 시간 안에 끝나지 않으면 취소하고 원본을 계속 돌린다("1h30m", "90s")
```

- **요구 사항.** Linux/KVM 노드끼리만 된다(macOS(HVF) 노드는 원본도 대상도 안 된다). 디스크가 **모두 공유 풀**
  (NFS, `--shared` 디렉터리 풀, Ceph RBD, 공유 LVM)에 있어야 한다(디스크 복사는 하지 않는다). 두 노드의 **QEMU
  버전이 같고 CPU가 같아야** 한다(`-cpu host`. 버전이 다르면 작업이 실패하고, CPU는 확인하지 않으므로 다르면
  실패하거나 게스트가 멈출 수 있다). 모든 노드를 이 기능이 있는 opld로 올린 뒤 쓴다.
- **방화벽.** 노드 사이에 `[migration] ports` 범위를 연다. 받는 쪽은 `cluster_listen`의 IP(기본 모든 주소)에서
  범위의 빈 포트를 고르고(모두 쓰는 중이면 "no free migration port in …"으로 실패한다), 원본은 대상의 `address`로
  접속한다.
- **TLS.** opld가 시작할 때 노드 인증서·키와 클러스터 CA를 `{data_dir}/migration-tls/`(기본
  `/var/lib/opl/migration-tls`, 0700, 키 0600)에 쓰고 QEMU가 그것으로 서로를 확인한다. 이 기능 전에 클러스터에
  들어온 노드의 인증서는 키 용도에 keyEncipherment가 없어 QEMU 8.2 이하에서 TLS가 실패한다(opld가 시작할 때
  경고한다). 그 노드를 클러스터에서 뺐다가 다시 합류시켜 새 인증서를 받는다.
- **거절(409).** 실행 중이 아님 `vm_not_running`, 공유 풀이 아닌 디스크나 macOS 노드 `migration_unsupported`,
  대상이 오프라인·유지보수 중 `node_unavailable`, 대상에서 풀·브리지가 준비되지 않았거나 남은 메모리가 모자람
  `no_capacity`, 다른 작업(스냅샷·되돌리기·크기 늘리기 등) 중 `vm_busy`. 같은 노드면 **400**, 아키텍처가 다르면
  **400** `machine_arch_mismatch`.
- **옮기는 동안.** VM은 `migrating`이고 켜기·끄기·재시작·스냅샷·복제·편집·지우기가 모두 **409** `vm_busy`다.
  콘솔은 열 수 있다(넘어가는 순간 끊겼다가 대상 노드로 다시 연결된다). 취소는 없다. 수렴하지 않으면 `auto-converge`가 게스트를 늦추고 시간 제한이 지나면 실패한다.
- **실패.** 확정 전에 실패하면(대상 거절, 연결·TLS 실패, 시간 제한) 대상 QEMU를 끄고 원본이 계속 돈다. 작업
  오류에 QEMU의 이유가 남는다. 중간에 opld가 죽어도 다시 뜰 때 원본을 되살리거나(확정 전) 남은 원본·받는 QEMU를
  끈다(확정 뒤, 시작 때와 30분마다).
- **UEFI 변수.** QEMU가 변수(pflash)도 함께 옮겨 대상 노드의 변수 파일에 쓴다. 변수 파일은 노드마다
  `{data_dir}/vm/{id}/uefi-vars.fd`에 있고, 클러스터 기록이 지금 파일을 가진 노드(`status.firmware_node`)를 안다.
  멈춘 채 다른 노드로 옮긴 VM(웹 VM 편집이나 API `PUT /api/v1/vms/{key}`의 `node`)은 켤 때 그 노드에서 노드 사이
  연결로 변수를 받아 온다. 스냅샷·복제도 변수를 복사하기 전에 받아 온다. 그 노드가 꺼졌거나 죽었거나 받지 못하면
  경고를 남기고 켜는 노드의 파일(없으면 템플릿)로 켠다 — 그 노드에만 있던 변수(부팅 순서 등)는 잃는다. 옛 노드의
  파일은 지우지 않는다. 모든 노드가 같은 펌웨어 빌드(같은 크기의 UEFI 변수 템플릿, `[hypervisor] uefi_vars`)를
  써야 한다 — 받은 변수 파일의 크기가 켜는 노드의 템플릿과 다르면 버리고 같은 경고 뒤 그 노드의 파일로 켠다.
  한계: 스냅샷의 변수 복사본은 받아 오지 않으므로, 스냅샷에서 복제하는 작업이 스냅샷을 찍은 노드가 아닌 곳에서
  돌면 새 VM은 템플릿으로 켜고, 되돌리기가 그런 노드에서 돌면 변수는 되돌리지 않고 지금 것을 둔다(경고). VM이
  거쳐 간 다른 노드의 변수 파일은 VM을 지워도 그 노드에 남는다.
- **공유 LVM.** 옮기는 동안 디스크 LV를 공유 잠금(`lvchange -asy`)으로 바꾸고, 대상이 넘겨받은 뒤 다시 배타로
  바꾼다.

## 위트니스 노드

2노드 클러스터는 한 노드만 죽어도 과반수를 잃어 아무것도 바꿀 수 없다. 위트니스는 VM을 돌리지 않고 Raft 투표만
하는 opld다(vSAN 위트니스, corosync-qnetd와 같은 자리). **일반 노드 둘과 위트니스 하나**를 권한다. 투표 멤버가
셋이므로 어느 한 노드가 죽어도 클러스터가 계속 돈다. QEMU가 필요 없으므로 작은 기계나 VM(이 클러스터 밖)에 둔다.

```bash
sudo tee /etc/opl/opld.toml >/dev/null <<'TOML'
name = "wit"
address = "10.0.0.13"
witness = true
TOML
sudo opld --config /etc/opl/opld.toml &
oplctl cluster join-token                        # 일반 노드의 API에 로그인한 곳에서
sudo oplctl cluster join 'opl-join://…'          # 위트니스에서
oplctl cluster status                            # WITNESS 열이 true, 경고 없음
```

- **합류만 된다.** 위트니스에서 `cluster init`은 거절된다("a witness cannot start a cluster; join an existing
  cluster instead"). 역할은 합류할 때 정해지고 바뀌지 않는다. 바꾸려면 `cluster leave` 뒤 설정을 고치고 opld를
  다시 띄워 다시 합류한다. 멤버인 채 `witness` 값만 바꿔 다시 띄우면 opld는 시작하지 않는다("this node joined
  as a witness but opld.toml says witness = false; leave the cluster and join again to change the role", 반대도
  같다).
- **탈퇴.** 탈퇴·`remove-node`는 일반 노드와 같다. 다만 빠진 뒤 남는 투표 멤버가 모두 위트니스면 위트니스는
  리더가 될 수 없어 클러스터가 멈추므로 `cluster leave`를 거절한다("the remaining voting members are all
  witnesses, which cannot lead; add another non-witness node first"). 위트니스 자신은 일반 노드가 남으면 빠질
  수 있다.
- **도는 것과 돌지 않는 것.** 클러스터 저장소와 노드 사이 연결(`cluster_listen`, 기본 8007)만 돈다. HTTPS API(8006)·
  웹 UI를 열지 않고, VM·Task·스토리지 풀·마이그레이션·예약 스냅샷·정리를 하지 않는다. 그래서 위트니스에서는
  `cluster join`·`leave`(제어 소켓)만 쓰고, 상태는 일반 노드의 API로 본다. 방화벽은 노드 사이에 8007만 연다.
- **설정.** `[hypervisor]`·`[pools]`·`[migration]`은 쓰지 않는다. 다만 `[hypervisor]` 값의 검사는 그대로 거치므로
  (예: macOS에서 `accel = "kvm"`, `systemd_scope = true`) 잘못된 값이 있으면 시작하지 않는다. `qemu_binary`는
  찾지 않는다.
- **리더가 되지 않는다.** 투표는 하지만 선거에 나서지 않는다. 리더의 일(노드 감시, 정리, 예약 스냅샷)은 늘 일반
  노드가 한다. 리더가 죽으면 남은 일반 노드가 위트니스의 표를 받아 리더가 된다.
- **배치에서 빠진다.** 스케줄러, 작업 노드 선택, 마이그레이션 대상, 풀의 노드 목록이 위트니스를 세지 않는다. VM의
  노드나 마이그레이션 대상으로 위트니스를 지정하면 **409** `node_unavailable`이다.
- **표시.** `oplctl node list`·`cluster status`에 `WITNESS` 열(`true`/`false`), `node get`에 `spec.witness`가
  있다. 웹은 노드 목록·클러스터 멤버·대시보드에 "위트니스" 배지를 붙인다. `cluster status`는 투표 멤버가 둘이면
  2노드 경고("warning: a two-node cluster stops accepting changes when either node fails; add a third node")를,
  투표 멤버가 셋 이상인데 위트니스가 아닌 노드가 하나뿐이면 "warning: HA needs at least two non-witness
  nodes"를 보인다.
  위트니스의 `ADDRESS`(`node list`·`cluster status`)는 설정의 API 주소(`address:listen 포트`)이지만 위트니스는
  그 포트를 열지 않는다.

## HA VM과 자기 펜싱

HA VM은 노드가 클러스터 과반수와 끊기면 그 노드가 스스로 끈다(자기 펜싱, Proxmox HA의 watchdog 펜싱과 같은
자리). 노드는 리더에게 몇 초마다 **리스**를 갱신하고, 마지막으로 성공한 갱신 뒤 `fence_after_secs`가 지나면 그
노드의 HA VM을 강제로 끈다. opld 자신이 멈추거나 멈춰 서도 끄도록 Linux watchdog 장치를 쥐고 있다가, 리스를 잃은
채 HA VM이 돌면 쓰다듬기를 멈춰 노드를 재시작시킨다. 리더는 노드가 스스로 펜싱을 마쳤을 시간(`recover_after_secs`)이
지나도록 리스가 돌아오지 않으면 그 노드의 HA VM을 다른 노드로 옮겨 켠다(**복구**, 아래).

```bash
oplctl vm create web --memory 1024 --disk root --ha --restart on-failure --start
oplctl vm set web --no-ha            # 끄기. 실행 중이어도 된다.
oplctl vm set web --ha               # 켜기. VM이 멈춰 있을 때만 된다(실행 중이면 409 vm_running).
oplctl vm list                       # HA 열(true/false)
oplctl node list                     # FENCING 열: watchdog · process · -(꺼짐)
oplctl cluster status                # 멤버마다 FENCING 열
oplctl task list                     # 복구 기록: OP "vm-ha-recover (from <죽은 노드 id>)"
```

웹은 VM 만들기·편집의 "고가용성(HA)" 스위치(그 노드의 펜싱이 꺼져 있거나, 실행 중이고 HA가 꺼진 VM이면 이유와
함께 켜기를 막는다), VM 목록·상세의 HA 배지, 노드·클러스터·대시보드 노드 표의 "펜싱" 열, 작업 목록의
"web HA 복구 (노드 node-a에서)"다. API는 VM `spec.ha`(생성·`PUT /api/v1/vms/{key}`),
Node `status.ha`(`{"fencing": "watchdog"|"process", "recover_after_secs": …}`, 펜싱이 꺼져 있으면 없다),
`GET /api/v1/cluster`의 멤버 `fencing`(꺼져 있으면 null)이다. 같은 노드에서의 크래시 재시작은 지금처럼
`restart`가 정하므로 HA VM에는 `on-failure`를 권한다.

```toml
# /etc/opl/opld.toml (노드마다, 생략하면 아래 기본값)
[ha]
fencing = "watchdog"              # watchdog | process | off (기본: Linux watchdog, macOS off)
watchdog_device = "/dev/watchdog" # Linux watchdog 장치(하드웨어 또는 softdog)
watchdog_timeout_secs = 30        # 장치에 정하는 제한 시간(5~3600초, WDIOC_SETTIMEOUT)
fence_after_secs = 30             # 마지막 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다(1초 이상)
```

- **펜싱 방식.** `watchdog`: opld가 HA VM을 끄고, opld가 멈추면 watchdog가 노드를 재시작한다(운영용). `process`:
  opld가 끄는 것만 한다 — opld가 죽거나 멈추면 아무것도 끄지 않으므로 **시험·개발용**이다(macOS 노드도 쓸 수 있다).
  `off`: 이 노드는 HA VM을 돌리지 않는다. Linux가 아니면 `watchdog`는 설정 오류다. 위트니스는 `[ha]`를 쓰지 않는다.
- **HA 가능 노드.** 위트니스가 아니고 펜싱이 켜진 노드만 HA VM을 돌린다. HA VM의 노드(`--node`, 편집)나
  마이그레이션 대상이 펜싱이 꺼진 노드면 **409** `ha_unsupported`, 노드를 정하지 않은 HA VM은 스케줄러가 HA 가능
  노드만 고른다(없으면 **409** `no_node`). 노드는 등록할 때마다 `[ha]`를 `status.ha`에 쓴다. `recover_after_secs`는
  `fence_after_secs + watchdog_timeout_secs(watchdog일 때) + 5`(기본 65초)로, 리더가 이만큼 기다린 뒤 옮긴다.
- **HA는 멈춘 VM에서만 켠다.** 만들 때 `--ha`는 그대로 되지만, 이미 있는 VM의 HA를 켜는 수정(`vm set --ha`, 웹
  편집, `PUT`의 `ha` false → true)은 VM이 멈춰 있을 때만 된다(원하는 상태가 stopped이고 phase가 stopped·failed).
  아니면 **409** `vm_running`("cannot turn on HA for VM "web" while it is running; turn on HA while the VM is
  stopped"). 끄기는 언제든 된다. 노드가 과반수와 끊긴 동안 실행 중인 VM의 HA를 켜면 그 노드는 HA인 줄 모르고
  끄지 않는데 리더는 그 VM을 다른 노드에서 켜 둘이 함께 돌 수 있기 때문이다. 실행 중인 VM은 정지하고 HA를 켠 뒤
  다시 시작한다.
- **리스.** 갱신 간격은 `fence_after_secs`의 10분의 1(0.2초 이상 2초 이하, 기본 2초)이다. 리더는 아직 과반수의
  리더인지 확인한 뒤에만 갱신을 받아 준다. 그래서 과반수와 끊긴 노드는 물론, 고립된 리더 자신도 갱신하지 못한다.
  리스는 opld가 시작한 때부터 센다(다시 시작한 직후 곧바로 끄지 않는다).
- **리스를 잃으면.** `fence_after_secs`가 지나면 그 노드의 HA VM을 강제로 끈다(실패로 세지 않고 원하는 상태는
  running 그대로, status "fenced: lost the cluster majority"). HA 아닌 VM은 그대로 돈다. 리더가 옮기기 전에 리스가
  돌아오면 그 노드에서 다시 켠다. 리스 없이는 HA VM을 켜지도, 멈춘 채 받은 마이그레이션을 이어서 실행하지도
  않는다("waiting for the cluster majority before starting an HA VM", 실패로 세지 않고 다시 본다). 켜기 바로 앞에서 리스와 VM을 다시 보아,
  그 사이 다른 노드로 배정됐거나 멈추라고 했으면 켜지 않는다. 짧은 네트워크 장애라도 `fence_after_secs`
  보다 길면 HA VM이 꺼진다.
- **watchdog.** 이 노드에서 HA VM을 켜거나 이미 도는 HA VM에 붙을 때 장치를 처음 열고(그 전에는 열지 않는다 —
  HA VM이 없는 Linux 노드는 장치를 건드리지 않는다), 그 뒤로 opld가 끝날 때까지 쥔 채 1초마다 쓰다듬는다. 리스가
  끝났는데 HA VM이 돌면 쓰다듬기를 멈춘다. opld가 제한 시간 안에 HA VM을 끄면 다시 쓰다듬으므로 노드는 재시작되지
  않고, opld가 멈춰 끄지 못하면 노드가 재시작된다. 장치를 열지 못하면(없음, 다른 프로세스가 쥠) HA VM을 켜지 않고
  status에 "cannot open the watchdog device: …"를 남긴다. 장치가 요청보다 긴 제한 시간을 고르거나 제한 시간을 정하지
  못하면 HA VM에 쓰지 않는다(status "the watchdog device is unusable: …", 짧은 값을 고르면 경고만 남기고 쓴다). 그런
  장치도 이미 열어 타이머가 돌므로 닫지 않고 쥔 채 같은 규칙으로 쓰다듬고, 정상 종료 때 magic close로 멈춘다.
- **opld를 멈추면 노드가 재시작될 수 있다.** 정상 종료(SIGTERM) 때 이 노드에 도는 HA VM이 없으면 watchdog를 멈추고
  닫는다(magic close). **도는 HA VM이 있으면 그대로 둔다**(경고 "HA VMs are running; leaving the watchdog armed …") —
  QEMU는 opld 없이도 계속 돌므로, opld가 `watchdog_timeout_secs` 안에 다시 떠 리스를 갱신하지 못하면 노드가
  재시작된다. 업그레이드·점검으로 opld를 오래 멈출 때는 먼저 HA VM을 끄거나 다른 노드로 옮기거나(`node drain`, 아래 "노드
  유지보수와 비우기") `vm set --no-ha`를 한다. 펜싱을 `off`로 바꿔 다시 띄운 노드의 이미 도는 HA VM은 끄지 않고
  status에 "HA is not available on this node"만 남긴다(멈춘 HA VM은 켜지 않는다).
- **watchdog 준비(Linux).** 하드웨어 watchdog(`iTCO_wdt`, IPMI 등)가 없으면 `softdog`를 올린다:
  `echo softdog | sudo tee /etc/modules-load.d/softdog.conf && sudo modprobe softdog`.
  `ls /sys/class/watchdog/`와 `cat /sys/class/watchdog/watchdog0/identity`로 장치를 확인한다. watchdog 장치는 한
  프로세스만 열 수 있다. **공유 LVM 풀의 sanlock `wdmd`가 이미 쥔 장치는 쓸 수 없으므로** 그 노드에는 다른 장치(예:
  하드웨어 watchdog를 `watchdog_device = "/dev/watchdog1"`로)가 있어야 한다. softdog는 장치를 하나만 만든다.
  **`nowayout` 장치는 쓰지 않는다** — magic close가 듣지 않아 HA VM이 없어도 opld를 멈추면 노드가 재시작된다
  (`cat /sys/class/watchdog/watchdog0/nowayout`이 0이어야 한다. 커널 `CONFIG_WATCHDOG_NOWAYOUT`이나 softdog의
  `nowayout=1`이면 1이다).
- **한계.** 라이브 마이그레이션이 진행 중인(보류 중인) HA VM은 리스를 잃어도 마이그레이션이 끝나거나 풀릴 때까지
  끄지 않는다(`process`에서는 긴 마이그레이션이 `fence_after_secs`를 넘길 수 있다. `watchdog`면 쓰다듬기를 멈추므로
  노드가 재시작된다). 실패한 마이그레이션이 원본을 멈춘(`paused`) 채로 남기면 HA VM은 이어서 실행되지 않는다(드묾 —
  끄고 다시 켠다).

**복구.** 리더(위트니스가 아닌 노드)가 1초마다 노드의 리스를 보고 다음 노드의 HA VM을 옮긴다.

- **언제.** 리스를 마지막으로 갱신한 뒤 그 노드의 `recover_after_secs`(기본 65초)가 지난 노드. 리스는 리더의
  메모리에만 있으므로 리더가 바뀌면 새 리더는 리더가 된 때부터 다시 센다. 그래서 노드가 죽고 VM이 다른 노드에서
  켜지기까지는 대략 `recover_after_secs` + 1초에 부팅 시간이다. 리더도 함께 죽었으면 선거 시간이 더해지고, 디스크
  잠금이 남아 있으면(아래) 그만큼 더 걸린다. `fencing = "process"`, `fence_after_secs = 2`(recover_after 7초)인
  자동 시험에서는 수십 초다. 펜싱이 꺼진 노드(`status.ha` 없음)의 VM은 옮기지 않는다.
- **무엇을.** 그 노드에 배정되고(`spec.node`) HA이고 원하는 상태가 running인 VM을 이름 순서로 옮긴다. 게스트가
  스스로 끄거나 사용자가 정지한 VM, HA 아닌 VM은 그 노드에 남는다. 옮기는 동안 리더는 그 노드의 리스 갱신을
  거부한다(펜싱 중 표시) — 그 노드가 돌아와 낡은 배정을 보고 VM을 켜지 않게 하려는 것이다. 복구 기록의 결과를
  모르면(시간 초과 — 나중에 커밋될 수 있다) 결과를 알 때까지 같은 기록을 다시 보내며 거부를 이어 가고, 리더가
  바뀌면 푼다.
- **어디로.** 스케줄러와 같은 규칙(온라인, 유지보수 아님, 위트니스 아님, 아키텍처·디스크 풀·브리지, 미할당 메모리가
  가장 많은 노드)에 더해 HA 가능 노드이고 리스가 살아 있는 노드다. 죽은 노드와 진행 중이던 마이그레이션의 대상
  노드는 뺀다. 여러 VM을 옮기면 앞서 옮긴 VM의 메모리도 센다. 그런 노드가 없으면 VM은 그대로 두고 status에
  "HA cannot find a node for this VM"만 남긴 뒤 1초마다 다시 본다.
- **어떻게.** status를 먼저 비우고(phase stopped, "recovering: node node-a failed") 그 다음 `spec.node`를 대상
  노드로 바꾼다(둘 다 revision 검사, 그 사이 바뀌었으면 1초 뒤 다시). 대상 노드의 reconciler가 평소처럼 새로
  켠다. 죽은 노드가 맡았던 그 VM의 끝나지 않은 작업은 "node node-a failed; the VM was recovered by HA"로 실패
  처리하고(그래야 `vm_busy`가 풀린다), 기록 작업 `vm-ha-recover`(대상 VM, 노드 = 옮긴 노드, `from` = 죽은 노드)를
  처음부터 성공 상태로 남긴다. 작업 실행기는 이것을 실행하지 않는다.
- **이미지 잠금 기다리기.** 죽은 노드의 QEMU가 쥐었던 디스크 잠금은 곧바로 풀리지 않을 수 있다(NFSv4 임대 약 90초,
  공유 LVM의 sanlock 임대). QEMU 시작이 이미지 잠금 오류(`Failed to get "write" lock` / `Is another process using
  the image`)로 실패하면 실패로 세지 않고 2초마다 다시 켠다(phase starting, "disk image is locked by
  another process; retrying"). **이 규칙은 HA 아닌 VM에도 적용된다** — 다른 프로세스가 이미지를 쥔 동안에는 실패
  (`failed`)로 끝나지 않고 계속 다시 시도하므로, 멈추려면 VM을 정지한다. 공유 LVM은 전부터 다시 시도하는 오류였다.
- **죽은 노드가 돌아오면.** 펜싱 중 표시가 풀린 뒤의 첫 리스 갱신이 복구 기록까지 적용된 뒤에야 HA VM을 켜므로,
  옮겨 간 VM은 이미 다른 노드의 것으로 보고 켜지 않는다(낡은 기록으로 status를 덮지도 않는다). 그 노드의 실행기는
  실패 처리된 작업을 다시 하지 않는다. 그 노드에 남은 HA 아닌 VM은 평소처럼 그 노드가 다시 맞춘다. 옮긴 VM은
  원래 노드로 되돌아가지 않는다(필요하면 마이그레이션한다).
- **한계.** 죽은 노드에만 있던 UEFI 변수는 잃는다 — 대상 노드는 변수를 그 노드에서 받으려다 실패하면 경고를 남기고
  자기 노드의 옛 파일(없으면 템플릿)로 켠다. HA 아닌 VM은 노드가 돌아올 때까지 그 노드에 묶여 있다. Ceph RBD에는
  디스크 잠금이 없어 펜싱만 믿는다(opLite는 RBD 배타 잠금·blocklist를 쓰지 않는다). NFSv3에서는 죽은 노드의 잠금이
  그 노드가 돌아올 때까지 남을 수 있고, 그동안 VM은 "disk image is locked …; retrying"으로 기다린다. `process`
  펜싱 노드는 opld만 멈추면(QEMU는 남음) HA VM을 끄지 못하므로 복구가 VM을 둘 돌게 할 수 있다(시험·개발용).
  복구 도중 리더가 다른 노드로 바뀌면 죽은 노드에 남은 그 VM의 작업 실패 처리와 기록 작업 `vm-ha-recover`가 빠질 수
  있다(VM은 옮겨진다). 끝나지 않은 작업은 그 노드가 돌아오거나 제거될 때 정리된다.

**수동 watchdog 시험(Linux, root, 직접 할 때만 — 자동 시험·CI는 진짜 watchdog를 쓰지 않는다).** 버려도 되는 시험
노드에서 한다. `soft_noboot=1`이면 softdog는 제한 시간이 지나도 재시작하지 않고 커널 로그에
"softdog: Triggered - Reboot ignored"만 남긴다. 다른 프로세스(`wdmd` 등)가 watchdog를 쓰지 않는지 먼저 본다.

```bash
sudo modprobe -r softdog; sudo modprobe softdog soft_noboot=1   # 이미 올라와 있으면 내렸다가 다시
cat /sys/class/watchdog/watchdog0/identity                      # Software Watchdog
# opld.toml: [ha] fencing = "watchdog", watchdog_timeout_secs = 10, fence_after_secs = 10
sudo opld --config /etc/opl/opld.toml &
oplctl vm create hatest --memory 256 --ha --start
cat /sys/class/watchdog/watchdog0/state                         # active (opld가 열었다)
# 1) opld가 멈춰 서면: 쓰다듬기가 멈춰 약 10초 뒤 "Triggered - Reboot ignored"
sudo kill -STOP "$(pidof opld)"; sleep 15; sudo dmesg | grep softdog; sudo kill -CONT "$(pidof opld)"
# 2) HA VM이 돌 때 정상 종료: 경고 로그와 "watchdog did not stop!", 약 10초 뒤 Triggered
sudo kill -TERM "$(pidof opld)"; sleep 15; sudo dmesg | tail
# 3) HA VM이 없을 때 정상 종료: magic close로 state가 inactive, Triggered가 없다
sudo opld --config /etc/opl/opld.toml &                         # 다시 띄우면 도는 hatest에 붙어 장치를 다시 연다
oplctl vm stop hatest; oplctl vm list                           # PHASE stopped를 확인한 뒤
sudo kill -TERM "$(pidof opld)"; sleep 15; cat /sys/class/watchdog/watchdog0/state
# 4) (셋 이상의 클러스터) 이 노드와 다른 멤버 사이의 cluster_listen(8007)을 막으면 약 10초 뒤 HA VM이
#    "fenced: lost the cluster majority"로 꺼지고 Triggered는 없다. 풀면 리스가 돌아와 다시 켜진다.
sudo modprobe -r softdog                                        # 끝나면
```

## 노드 유지보수와 비우기

커널·펌웨어를 올리거나 하드웨어를 손보기 전에 노드를 **유지보수 모드**로 두고 실행 중인 VM을 다른 노드로 옮긴다
(Proxmox의 노드 유지보수, vSphere의 유지보수 모드와 같은 자리). 유지보수 노드에는 새 VM을 놓지 않는다: 스케줄러의
자동 배치, HA 복구 대상, 마이그레이션 대상에서 빠진다. **비우기**는 노드를 유지보수로 바꾸고 그 노드의 실행 중인
VM을 스케줄러가 고른 노드로 하나씩 라이브 마이그레이션한다.

```bash
oplctl node drain node1 --wait           # 유지보수로 바꾸고 모두 옮길 때까지 기다린다: "node node1 drained"
oplctl node drain node1                  # 기다리지 않는다: "drain of node1 started (task …); follow it with `oplctl task get …`"
oplctl node list                         # MAINTENANCE 열
oplctl node maintenance node1 off        # 유지보수 끝: "node node1: maintenance off"
oplctl node maintenance node1 on         # VM은 옮기지 않고 배치만 막는다
```

웹은 노드 화면 행 메뉴의 "노드 비우기…"(확인 대화상자에 옮길 실행 중 VM 수와 로컬 디스크 안내, 그 뒤 작업을
따라감 — 오프라인·macOS(HVF)·비우는 중인 노드는 이유와 함께 막힘)와 "유지보수 시작/끝"이다. API는 `POST /api/v1/nodes/{key}/drain`(본문 없음, admin, 202와 작업
`node-drain`)이고, 유지보수 켜기·끄기는 노드 수정 `PUT /api/v1/nodes/{key}`의 `spec.maintenance`(admin, If-Match)다.

**절차.** 한 노드씩:

1. `oplctl node drain <노드> --wait`(어느 노드에서 해도 된다 — 옮기는 일은 비우는 노드의 실행기가 한다). 실패하면
   옮기지 못한 VM과 이유가 나온다 — 그 VM을 끄거나 손으로 옮긴 뒤 다시 `drain`한다.
2. 노드를 손본다(업그레이드 `oplctl node upgrade <노드> --wait` — 다른 노드에 로그인해서, 커널 업데이트, 재부팅,
   하드웨어). 비우기가 성공했으면 그 노드에 HA VM이 없으므로 opld를 멈춰도 watchdog이 꺼져 재시작되지 않는다(위
   "HA VM과 자기 펜싱").
3. `oplctl node list`에서 ONLINE을 확인하고 `oplctl node maintenance <노드> off`. 옮긴 VM은 돌아오지 않는다(필요하면
   `vm migrate`).

- **비우기 요청.** 거절: 노드가 없으면 **404**, 위트니스(`node "X" is a witness node and runs no VMs`)나 오프라인
  노드(`node "X" is offline; HA recovers the HA VMs of an offline node` — 꺼진 노드의 HA VM은 HA 복구가 맡는다)는
  **409** `node_unavailable`, 그 노드의 비우기 작업이 끝나지 않았으면 **409** `node_busy`, 비우기를 모르는 노드(위트니스
  포함)가 남았으면 **409** `cluster_upgrading`("some nodes do not support node drain yet (…); upgrade them first"). 버전이
  아니라 노드가 등록할 때 적는 기능 표시(`status.features`의 `node-drain`)를 보므로, 비우기가 처음 들어간 버전으로 올리는
  동안은 못 쓰고(그때는 `vm migrate`와 유지보수 시작으로) 그 뒤의 롤링 업그레이드에서는 버전이 섞여도 된다. 옛 버전
  리더가 노드 상태를 다시 쓰며 이 표시를 지워도 노드가 1분마다 다시 적으므로 다음 주기에 되살아난다 — 새 버전 노드의
  이름이 `cluster_upgrading`에 나오면 1분쯤 뒤 다시 하거나 그 노드의 opld를 다시 시작한다. 노드를
  유지보수로 바꾼 뒤(이미면 그대로) 작업을 만든다. 작업이 실패해도 노드는 유지보수로 남는다.
- **무엇을, 어떻게.** 비우는 노드의 실행기가 그 노드에 배정되고(`spec.node`) 원하는 상태가 running인 VM을 이름 순서로
  한 번에 하나씩 옮긴다. VM에 다른 작업(전원·스냅샷·마이그레이션 등)이 있으면 끝나기를 `[migration] timeout`(기본 30분)까지
  기다린다. 대상은 VM 만들기의 스케줄러와 같은 규칙(온라인, 유지보수·위트니스 아님, 아키텍처·디스크 풀·브리지·방화벽, HA
  VM이면 HA 가능 노드)을 지나고 macOS(HVF) 노드가 아니며 남은 메모리가 VM 메모리 이상인 노드 가운데 미할당 메모리가 가장
  많은 노드(같으면 id가 작은 노드)다. 마이그레이션은 `vm migrate`와 같은 확인을 거친 보통의 `vm-migrate` 작업이고(작업
  목록에 하나씩 보인다), 비우기는 그것이 끝나기를 기다린 뒤(`[migration] timeout`의 두 배와 1분까지 — 넘으면 `migration
  task did not finish`) 다음 VM으로 간다. 그 사이 VM이 꺼지거나 지워지거나 다른 노드로 갔으면 넘어가고(다시 나타나면
  세 번까지 다시 보고, 그 뒤에도 다시 나타나면 `busy`), 확인과 작업 만들기 사이에 다른 작업이 끼면 한 번 더 기다려 다시
  한다. 비우기 작업에는 일반 작업의 시간 제한(600초)이 없다.
- **성공은 노드가 빈 것이다.** 옮길 VM이 없어도 이 노드로 들어오던 마이그레이션(비우기 전에 시작한 `vm migrate`)이
  있으면 끝나기를 `[migration] timeout`까지 기다려(넘으면 `busy`) 들어온 VM도 옮긴다. 비우기가 옮긴 VM이 비우기 중에
  다시 들어오면 한 번 더 옮기지 않고 `moved back during the drain`으로 남긴다.
- **옮기지 못한 VM.** 디스크가 공유 풀이 아닌 VM(`disks on a local pool`), macOS(HVF) 노드(`hvf node`), 받을 노드가 없음
  (`no node with capacity`), 바쁨(`busy` — VM의 다른 작업이나 이 노드로 들어오는 마이그레이션이 `[migration] timeout`까지
  끝나지 않음, 다시 기다린 뒤에도 마이그레이션 작업 만들기가 다른 작업과 겹침, 사라졌다 다시 나타나기를 세 번 넘게 함),
  마이그레이션 작업이 끝나지 않음(`migration task did not finish`), 다시 들어옴(`moved back during the drain`),
  마이그레이션 실패(그 작업의 오류)는 그 노드에 남기고 다음 VM으로 간다. 끝에 작업이 "N VM(s) were not moved: db (disks
  on a local pool), …"로 실패한다(`--wait`면 이 오류와 종료 코드 1). 꺼진 VM은 옮기지 않는다 — 노드에 배정된 채 남고, 켤 때 다른 노드로 간다(아래).
- **유지보수를 끄면 멈춘다.** 비우기는 VM마다(VM이 자리 잡기를 기다린 뒤 마이그레이션을 시작하기 직전에도, 들어오는
  마이그레이션을 기다리는 동안에도) 노드를 다시 읽어 유지보수가 끝났으면 "drain stopped: maintenance ended"로
  실패한다(옮기는 중인 마이그레이션은 끝까지 간다). 그래서 `oplctl node maintenance <노드> off`는 끝나지 않은 비우기가
  있으면 stderr에 `warning: the running drain of X stops (task T)`를 알리고, 웹은 "유지보수 끝내기" 전에 "끝내면 진행 중인
  비우기가 멈춥니다"로 묻는다. opld가 비우기 도중 다시 시작하면 작업은 "interrupted"로 실패한다. 노드는 유지보수로
  남으므로 다시 `drain`하면 남은 VM부터 옮긴다.
- **유지보수 노드에 고정된 VM 켜기.** 꺼진 VM은 노드 배정을 그대로 두므로, 유지보수 노드에 배정된 꺼진 VM을 켜면
  스케줄러가 다른 노드를 골라 VM의 노드를 바꾸고 그곳에서 켠다(UEFI 변수는 옛 노드에서 받아 온다(옛 노드가 꺼져
  있으면 경고 뒤 켜는 노드의 파일·템플릿) — "라이브 마이그레이션"). 고를 노드가 없으면(디스크가 그 노드의 로컬
  풀에만 있는 등) **409** `node_unavailable`("node "X" is in maintenance and no other node can run this VM; end
  maintenance first"). 아직 그 노드에서 돌거나 멈추는 중인 VM은
  옮기지 않는다. VM 만들기·편집에서 유지보수 노드를 새로 고르면 **409** `node_unavailable`("… is in maintenance and
  cannot take VMs; end maintenance first")이고, 이미 그 노드에 있는 VM의 다른 편집은 막지 않는다.
- **한계.** 로컬 디스크 VM은 옮기지 못한다(디스크 복사 마이그레이션이 없다 — 끄거나 공유 풀로 옮긴다). 한 번에
  하나씩 옮기고, 진행률은 각 `vm-migrate` 작업으로 본다. 유지보수가 끝나도 VM을 원래 노드로 되돌리지 않는다. 옮기지
  못한 VM을 끄는 선택과 롤링 업그레이드 자동화(노드를 차례로 비우고 올리고 재부팅)는 없다. 볼륨 만들기·백업 같은
  작업의 노드 고르기는 유지보수를 보지 않는다.

## 백업과 복원

백업은 VM 전체(디스크, UEFI 변수, VM 스펙)를 다른 저장소에 떠 둔 것이다. 스냅샷과 달리 VM 디스크의 저장소를
잃거나 VM을 지워도 남고, 언제든 **새 VM**으로 되살린다. 백업은 `backups`를 켠 **디렉터리·NFS 풀**(백업 풀)에 둔다.

```bash
oplctl pool create bk --nfs nas.lan:/srv/backup --backups    # 백업 풀(디렉터리 풀이면 --path /srv/backup)
oplctl pool set local --backups true                         # 있는 dir·NFS 풀을 백업 풀로(false는 끄기)
oplctl backup create web --pool bk                           # 이름은 web-20261104-030000(UTC)처럼 정해진다
oplctl backup create web --pool bk --name before-upgrade
oplctl backup create web --pool bk --incremental             # 할 수 있으면 증분(아래 "증분 백업")
# backup web-20261105-030000 of vm web created (incremental)     할 수 없었으면 (full: <이유>)
oplctl backup list                                           # --vm web이면 지금 web인 VM과 web이라는 이름으로 남은(지운 VM) 백업
# NAME                 VM   POOL  PHASE  CREATED               SIZE      TYPE  ORIGIN  REPLICAS
# before-upgrade       web  bk    ready  2026-11-04 03:00 UTC  412.5MiB  full  -       -
# web-20261105-030000  web  bk    ready  2026-11-05 03:00 UTC  18.2MiB   incr  -       -
oplctl backup get before-upgrade                             # 디스크별 크기, UEFI 변수, 오류, 증분이면 부모와 체인(JSON)
oplctl backup restore before-upgrade --name web2 --pool local   # 새 VM web2(꺼진 채), --node로 노드를 정할 수 있다
oplctl backup delete before-upgrade                          # 파일과 리소스를 지운다(실패한 백업도)
```

웹은 사이드바의 "백업" 화면(모든 백업, VM이 지워진 백업 포함)과 VM 상세의 "백업" 탭(지금 백업, 복원)에서 하고,
스토리지 풀 화면의 "백업 저장" 스위치로 백업 풀을 정한다. API는 `POST /api/v1/vms/{name}/backups`
(`{"pool": …, "name": …, "incremental": …}` — `incremental`은 기본 false), `GET /api/v1/backups[?vm=]`,
`GET`·`DELETE /api/v1/backups/{name}`, `POST /api/v1/backups/{name}/restore`(`{"name": …, "pool": …, "node": …}`)이고
만들기·지우기·복원은 작업(Task)을 돌려준다.
`DELETE /api/v1/backups/{name}?force=true`(`oplctl backup delete NAME --force`)는 파일을 지울 노드가 없을 때 리소스만
지우고 200(`{"message": …}`)이다.

- **백업 풀.** dir·NFS 풀만 백업을 담는다(다른 풀은 400). 백업 풀에도 지금처럼 VM 디스크를 둘 수 있다. 백업이 남은
  풀은 지울 수 없고 `backups`를 끄거나 `shared`를 바꿀 수도 없다(409 `pool_in_use`, "holds backups"). 백업 디렉터리를
  다른 서버에 두려면 NFS 풀을 쓴다 — 공유가 아닌 디렉터리 풀은 노드 로컬이라 그 노드의 디스크를 잃으면 함께 잃는다.
  공유가 아닌 풀의 백업은 쓴 노드를 기억하고(`status.node`) 그 노드에서만 복원하고 지운다: 복원에서 다른 `node`를
  주면 409 `backup_on_other_node`, 그 노드가 오프라인이면 409 `node_offline`, 대상 풀을 그 노드에서 쓸 수 없으면 409
  `pool_not_on_node`다. 그 노드를 잃었으면 `oplctl backup delete NAME --force`(`DELETE …?force=true`)로 리소스만
  지운다(파일은 남는다). 풀째 지워진
  백업(저장소에서 풀이 사라짐)은 보통의 지우기가 리소스만 지운다.
- **실행 중·꺼진 VM.** 실행 중인 VM은 디스크마다 메모리 없는 임시 스냅샷(이름은 하이픈 없는 백업 id)을 찍어 그때의
  내용을 복사하고 끝나면 지운다. VM은 계속 돈다(블록 풀 디스크는 스냅샷을 찍는 동안만 잠깐 멈춘다). 그래서 실행
  중인 VM의 디스크는 모두 스냅샷을 지원해야 한다 — dir·NFS 풀의 raw 디스크나 공유 LVM 풀 디스크가 있으면 409
  `vm_running`("stop it to back it up")이고, VM을 끄면 된다. 스냅샷처럼 방식이 다른 디스크(qcow2와 블록 풀, 서로 다른
  블록 백엔드)가 섞여도 409 `snapshot_unsupported`다. 블록 풀 디스크를 위해 멈춘 VM을 이어서 실행하지 못하면 한 번 더
  해 보고, 그래도 안 되면 디스크를 읽지 않고 백업이 실패한다. 백업 작업이 도는 동안 그 VM의 시작·정지·수정·지우기와
  마이그레이션은 409 `vm_busy`다(강제 정지는 된다). 작업은 VM의 노드에서 돈다. 배치된 적 없는 꺼진 VM은 백업 풀과
  디스크를 모두 읽을 수 있는 온라인 노드에서 한다(없으면 409 `no_node`).
- **crash-consistent.** 백업은 그 순간에 전원을 뽑은 것과 같은 디스크다(게스트 파일 시스템을 얼리지 않는다). 파일
  시스템 저널은 되살아나지만, 데이터베이스처럼 메모리에 쓰기를 쥐는 앱은 게스트 안에서 덤프를 떠 두거나 백업 동안
  쓰기를 멈춰 일관성을 맞춘다. 꺼진 VM의 백업은 일관적이다. 디스크 파일은 `qemu-img`가 닫을 때 디스크에 내려
  쓰고(`-t writeback`), `vars.fd`와 `manifest.json`은 fsync한 뒤 이름을 바꾸고 백업 디렉터리도 fsync한 다음에야
  `ready`가 된다.
- **복원.** 늘 새 VM이다(기존 VM을 덮어쓰지 않는다). 디스크마다 대상 풀에 새 볼륨 `{새 이름}-disk0`, `-disk1`…을
  만든다(블록 풀이면 raw, 아니면 qcow2). 새 VM은 백업 때의 VM 스펙(CPU·메모리·NIC·펌웨어 등)과 UEFI 변수를 받고, 새
  MAC, 꺼진 채, 노드·예약 스냅샷 없음, HA 끔이다. VM 스펙은 클러스터의 백업 기록에서 가져오고 `manifest.json`이
  그 기록(VM 스펙, 디스크 수)과 다르면 실패한다. 백업 디렉터리의 디스크·`vars.fd`는 일반 파일이어야 한다(심볼릭
  링크는 거절). 스펙의 네트워크와 보안 그룹이 지금 있어야 한다(400). 복원은 백업
  풀과 대상 풀을 함께 쓸 수 있는 노드에서 돈다. 실패하면 만든 볼륨을 지운다. 이름을 원래대로 돌리려면 원본을
  지운 뒤 그 이름으로 복원한다.
- **권한.** 읽기는 모든 사용자, 만들기·지우기·복원은 operator 이상이다(VM이 지워진 백업도 operator가 지운다).
- **오래 걸린다.** 진행률은 없고 작업에 시간 제한이 없다. `oplctl backup create`·`restore`는 24시간까지 기다리고,
  그 뒤에도 작업은 돈다(`oplctl task get`). 전체 백업 하나는 디스크 크기만큼 읽고 압축해 쓴다(증분 백업은 바뀐
  블록만 — 아래).

**증분 백업.** 실행 중인 VM은 QEMU dirty bitmap으로 디스크마다 바뀐 블록을 따라간다. `incremental`(`backup create
--incremental`, 웹 "지금 백업"의 "증분(가능하면)", 백업 작업의 `full_every`)로 청하면 같은 풀에 있는 그 VM의 바로
앞 백업을 **부모**로, 그 뒤로 바뀐 블록만 담은 백업을 만든다. 청하지 않으면 지금처럼 전체 백업이다.

- **비트맵 규칙.** 백업이 성공할 때마다(전체든 증분이든, 청하지 않은 백업도) 디스크마다 비트맵 `opl-<백업 id(하이픈
  없음)>`을 그 백업의 시점에 만들고 다른 `opl-` 비트맵을 지운다. 그래서 VM마다 "마지막으로 성공한 백업 뒤로 바뀐 블록"
  하나만 있고, 그 백업만 다음 증분의 부모가 된다. 비트맵을 만들지 못해도(경고) 백업은 성공하고 다음은 전체다. 실패한
  백업은 새 비트맵을 지우고 앞 비트맵을 남긴다(다음 시도도 같은 부모에서 증분). 증분 대상 파일을 닫지 못하면(QEMU가 닫을
  때 파일의 메타데이터를 쓴다) 그 증분은 실패다. qcow2 디스크(dir·NFS 풀)의 비트맵은 qcow2 파일에 저장돼 VM을 다시 켜도
  남는다(QEMU가 정상으로 끝날 때 저장한다). 블록 풀 디스크(raw — LVM-thin·ZFS·RBD·공유 LVM(iSCSI))의 비트맵은 QEMU
  안에만 있어 VM을 다시 켜면 사라지므로 다시 켠 뒤 첫 백업은 전체다. 라이브 마이그레이션은 비트맵을 함께 옮긴다(qcow2의
  것은 공유 이미지로, raw 디스크의 것은 마이그레이션 스트림으로 — raw 디스크에 비트맵이 있을 때만 QEMU의 `dirty-bitmaps`
  능력을 켠다). 꺼진 VM을 백업하면 qcow2 디스크에만 비트맵을 둔다. QEMU가 비정상으로 끝나면(SIGKILL, 호스트 장애 — HA
  복구 포함) qcow2 파일의 비트맵은 inconsistent로 남는다. 그런 비트맵은 쓸 수 없고 마이그레이션·디스크 크기 늘리기를
  막으므로 opld가 VM을 켤 때(그리고 마이그레이션·실행 중 크기 늘리기 전에) 지운다 — 다음 백업은 전체(`no dirty bitmap on
  disk{i}`)다. 꺼진 qcow2 디스크의 크기를 늘리면 비트맵도 지운다(크기가 바뀌면 어차피 전체).
- **꺼진 VM은 늘 전체다.** 증분은 실행 중인 VM만 한다. 그 밖에 증분을 할 수 없으면 오류 없이 전체로 하고 이유를
  `status.full_reason`(`backup get`, 웹 "전체로 한 이유")에 적는다: `VM is stopped`, `no earlier backup of this VM in
  pool "P"`, `no dirty bitmap on disk{i}`(그 디스크에 비트맵이 없음 — 블록 풀 VM을 다시 켰거나 디스크를 더함),
  `dirty bitmap on disk{i} is inconsistent`(비트맵이 기록하지 않거나, 비정상 종료 뒤 지우지 못함 — 비정상 종료·HA
  복구 뒤에는 VM을 켤 때 지우므로 보통 `no dirty bitmap on disk{i}`가 보인다), `dirty bitmaps do not match a ready
  backup in pool "P"`(마지막 백업이 다른 풀이거나 지워짐 — 같은 VM을 두 풀에 번갈아 백업하면 늘 전체다), `disks
  changed since backup "B"`(디스크를 바꾸거나 크기를 늘림), `chain already has N incremental backup(s)`(백업 작업의
  `full_every`), `some nodes do not support incremental backups yet`(아래 업그레이드), `backup "P" is stored on another
  node`·`files of backup "P" are missing on this node`(아래 다른 노드의 부모), `backup "P" is being deleted`(부모를 지우는
  작업이 이미 돌고 있음). 스냅샷 되돌리기는 디스크를 되돌리기 전에 비트맵을 지우므로 그 뒤 첫 백업은 전체다. 증분을
  만드는 사이 부모가 지워지면 그 증분은 실패하고("parent backup "P" was deleted during the backup") 비트맵을 지워
  다음은 전체다.
- **다른 노드의 부모.** 공유가 아닌(디렉터리) 백업 풀의 백업은 쓴 노드에만 있다(`status.node`). VM이 다른 노드로
  옮겨졌으면 비트맵은 맞아도 부모 파일이 그 노드에 없어 체인을 복원할 수 없으므로 전체로 한다(위 이유). 백업 풀이
  NFS(공유)면 어느 노드에서나 이어서 증분한다.
- **체인.** 증분 백업은 부모 없이는 복원할 수 없다. 부모를 거슬러 전체 백업까지가 그 백업의 **체인**이다. 목록의
  `TYPE`은 `full`·`incr`(웹 "종류" 배지와 "부모:" 링크)이고, 증분의 `SIZE`는 자기 파일 크기다. `backup get`은 증분
  백업에 `chain: {"backups": [전체부터 이것까지 이름], "stored_bytes": 체인 파일 크기의 합}`을 더하고(다 만들지 않은
  백업이 체인에 있거나 목록에 없는 조상이 있으면 `stored_bytes`는 없다 — 목록에 없는 조상은 id로 적는다), 웹은 크기
  아래 "체인 <크기>"를 보인다(같은 경우 보이지 않는다). 복원은 체인의 모든 백업이 같은 풀에 `ready`로 있고 디스크
  수·크기가 맞아야 한다(아니면 작업 실패 "backup chain is broken: …"). 체인 중간에서도 복원한다(그 시점의 디스크).
  체인을 원격으로 복제했으면 `chain.replicas`에 원격 사본마다 원격에 있는 체인 백업 수가 붙는다
  (아래 "증분 백업 복제"의 "진행").
- **지우기는 자식부터.** 다른 백업의 부모인 백업을 지우면 **409** `backup_in_use`("backup "B" is the parent of N
  incremental backup(s); delete them first", `--force`여도)다. 가장 새 증분부터 지운다. 만드는 중인 증분도 부모를
  막고, 실패한 증분은 막지 않는다. 병합(체인 줄이기)은 없다.
- **opLite 밖에서 디스크를 고쳤으면**(VM을 끄고 `qemu-img`·`qemu-io`·`guestfish`로 쓰기, 볼륨 파일 바꾸기) 비트맵이
  그 쓰기를 모르므로 다음 백업을 `incremental` 없이(전체로) 한다. 그 뒤로는 다시 증분이 맞다. qcow2 디스크에 비트맵이
  있으면 `qemu-img snapshot -a`(되돌리기)가 "Device has active dirty bitmaps"로 실패한다 — opLite의 스냅샷 되돌리기를
  쓰거나 먼저 `qemu-img bitmap --remove <파일> opl-<id>`로 지운다(그 뒤 첫 백업은 전체).
- **업그레이드 중.** 클러스터의 모든 노드(위트니스 포함)에 증분 백업 기능이 있어야 `incremental`을 받는다(아니면
  **409** `cluster_upgrading`). 기능이 없는 노드가 하나라도 있으면 백업은 비트맵을 만들지 않고 증분 요청은 전체로 한다
  (옛 노드는 되돌리기 전에 비트맵을 지우지 못하므로). 증분 백업의 복원은 복원하는 노드에 그 기능이 있어야 한다(**409**
  `cluster_upgrading` "node "X" does not support incremental backups yet; upgrade it first").
- **원격 복제.** 증분 백업도 원격으로 복제한다. 복제는 체인을 전체 백업부터 차례로 보내고(prod에 `ready`로 적힌 앞
  백업은 건너뛴다) 원격의 사본도 같은 부모 링크의 체인이 된다. 백업 작업에 `full_every`와 `replicate`를 함께 둘 수 있다.
  원격의 모든 노드에 증분 복제 기능이 있어야 한다(아래 "원격 복제(DR)"의 "증분 백업 복제").

**파일 형식과 opLite 없이 열기.** 백업 하나는 백업 풀 루트의 `backups/{백업 id}/` 디렉터리다(id는 `oplctl backup
get NAME`의 `id`; NFS 풀이면 NFS 서버의 내보내기 아래 같은 경로). 그 안에 디스크마다 압축 qcow2 `disk0.qcow2`,
`disk1.qcow2`…(backing file 없음, 스냅샷 없음), UEFI VM이면 변수 파일 `vars.fd`, 그리고 `manifest.json`(형식 버전
`version: 1`, 백업 id, VM id·이름, VM 스펙 전체, 디스크 목록과 크기, 만든 시각)이 있다. 증분 백업의 매니페스트는
`version: 2`이고 부모 백업 id `parent`가 더 있다. 표준 qcow2라 opLite 없이도 연다:

```bash
qemu-img info backups/<id>/disk0.qcow2                                  # 크기 확인
qemu-img convert -O raw backups/<id>/disk0.qcow2 disk0.raw              # raw 이미지로(dd로 디스크에 써도 된다)
qemu-system-x86_64 -m 2048 -drive file=backups/<id>/disk0.qcow2,snapshot=on   # 원본을 바꾸지 않고 켜 보기
jq .vm_spec backups/<id>/manifest.json                                 # CPU·메모리·NIC 등 VM 설정
```

UEFI VM은 OVMF(arm64는 AAVMF) 코드 파일과 함께 `vars.fd`의 사본을 `-drive if=pflash`로 준다. 형식 버전 1·2가 아닌
매니페스트는 복원하지 않는다(작업이 실패한다).

증분 백업의 디스크 파일은 바뀐 블록만 담고 헤더에 부모(backing file)가 **없다** — 혼자 열면 바뀌지 않은 곳이 0으로
보인다. 체인은 `manifest.json`의 `parent`로만 이어지므로, 열 때 `json:` 이미지 옵션으로 부모를 직접 적는다(자식이
바깥, 맨 끝 전체 백업은 `"backing": null`). 증분이 여럿이면 `backing` 안에 차례로 겹친다:

```bash
jq -r .parent backups/<증분 id>/manifest.json          # 부모 id(전체 백업이면 null)
CHAIN='json:{"driver":"qcow2","file":{"driver":"file","filename":"backups/<증분 id>/disk0.qcow2"},
  "backing":{"driver":"qcow2","file":{"driver":"file","filename":"backups/<전체 id>/disk0.qcow2"},"backing":null}}'
qemu-img convert -O qcow2 "$CHAIN" disk0-full.qcow2   # 체인을 이미지 하나로(이것을 위처럼 연다)
qemu-img compare "$CHAIN" disk0-full.qcow2            # Images are identical.
```

**백업 작업(예약 백업과 보존).** 백업 작업은 정한 VM들을 정한 시각에 백업 풀로 백업하고, 보존 규칙에 맞지 않는
오래된 백업을 지운다.

```bash
oplctl backup-job create nightly --pool bk --all --at 03:00 --keep-daily 7 --keep-weekly 4   # 모든 VM, 매일 03:00 UTC
oplctl backup-job create prod --pool bk --label tier=prod --at 01:30 --days mon,thu --keep-last 3
oplctl backup-job create db --pool bk --vm db --vm db2 --at 02:00 --keep-last 2 --keep-monthly 6 --disabled
oplctl backup-job create weekly-full --pool bk --all --at 03:00 --keep-daily 14 --full-every 6   # 매일 증분, 증분 6개마다 전체
oplctl backup-job list
# NAME         POOL  VMS  SCHEDULE                       KEEP              ENABLED  NEXT                  LAST
# nightly      bk    all  daily 03:00 UTC                daily=7,weekly=4  true     2026-11-05 03:00 UTC  2026-11-04 03:00 UTC (12 ok, 1 failed)
# weekly-full  bk    all  daily 03:00 UTC, full every 6  daily=14          true     2026-11-05 03:00 UTC  -
oplctl backup-job get nightly                  # 일정·보존·다음 실행·마지막 실행 결과와 VM별 오류(JSON)
oplctl backup-job set prod --at 04:00 --keep-last 5 --enabled true   # 준 값만 바꾼다(보존은 준 단위만, 나머지는 그대로)
oplctl backup-job set prod --full-every 0      # 늘 전체 백업으로
oplctl backup-job run nightly                  # 지금 실행(스케줄러의 다음 확인 때 시작한다)
oplctl backup-job delete nightly               # 작업만 지운다. 만든 백업은 남는다
```

- **대상.** `--all`(모든 VM), `--vm NAME`(여러 번), `--label KEY=VALUE`(여러 번이면 모두 맞는 VM) 중 하나다. VM은
  실행할 때마다 다시 고르므로 나중에 만든 VM과 라벨을 붙인 VM도 들어간다.
- **일정은 UTC다.** `--at HH:MM`(UTC)에, `--days`(`mon`…`sun`)를 주면 그 요일에만 돈다(없으면 매일). 리더 노드가
  1분마다 확인하므로 정한 시각에서 1분쯤 늦게 시작할 수 있다. 리더가 꺼져 있던 사이 놓친 실행은 여러 번이어도 한
  번만 하고, 리더가 바뀌면 새 리더가 진행 중인 실행을 이어 간다. 작업을 끄면(`--disabled`, `set --enabled false`)
  예약 실행도 지금 실행도 하지 않는다(진행 중인 실행은 이미 요청한 백업만 끝까지 기다린다).
- **한 번에 하나씩.** 실행은 대상 VM을 이름 순으로 하나씩 백업 작업(Task)으로 요청한다. 한 노드에서 동시에 도는
  백업은 opld 설정 `[backup] max_concurrent`(기본 2, 손으로 만든 백업도 센다)까지이고 나머지는 차례를 기다린다.
  이미 백업 중인 VM, 실행 중이라 백업할 수 없는 VM(위의 409와 같은 이유), `--vm`으로 골랐는데 없는 VM은 건너뛰고
  작업의 `errors`에 남긴다. 실행 도중 지운 VM은 그냥 빠진다. 작업이 만든 백업의 이름은 보통처럼
  `{VM 이름}-{UTC 시각}`(겹치면 `-2`…, 63자를 넘지 않게 VM 이름 부분을 줄인다)이고, 백업의
  `spec.job`(`oplctl backup get`)과 웹 백업 목록의 작업 열에 작업이 보인다.
- **마지막 실행.** 큐가 비고 이번에 만든 백업이 모두 끝나면 `last_run`에 시작·끝 시각과 성공(`ok`)·실패(`failed`,
  실패한 백업과 백업하지 못한 VM) 수를 적는다. 백업하던 노드가 죽어 백업 작업(Task)이 끝나지 못한 백업도 실패로
  센다. 실행 중에 `backup-job run`은 409 `job_running`이다. 일정을 바꿔도 이미 요청한 지금 실행은 그대로 돈다.
- **보존 규칙(Proxmox와 같다).** 실행이 끝나면 VM마다 그 작업이 만든 `ready` 백업을 최신부터 보며 `--keep-last N`개를
  먼저 남기고, 그다음 `--keep-daily`(UTC 날짜)·`--keep-weekly`(ISO 주)·`--keep-monthly`(달)마다 그 기간의 가장 새
  백업을 하나씩 N개 기간까지 남긴다(어느 것이 더 새 것인지는 만든 순서로 정하고, 기간은 만든 시각으로 나눈다).
  남지 않은 백업은 지운다. 예: `--keep-last 2 --keep-daily 7`이면 가장 새 백업 둘과, 지난 7일(백업이 있는 날
  기준) 각각의 마지막 백업이 남는다. 보존 값을 모두 0으로 두면(기본) 지우지 않는다.
  그 작업의 실패한 백업은 같은 VM의 더 새 성공한 백업이 생기면 지운다. 손으로 만든 백업과 다른 작업의 백업은
  건드리지 않는다. 실행이 끝난 뒤에는 스케줄러가 확인할 때마다 보존 규칙을 다시 보므로, 그때 지우지 못한
  백업(복제·복원 중이라 `backup_busy`)은 그 일이 끝나면 지우고, 실행 뒤에 보존 값을 줄이면 다음 확인 때 지운다.
  지우기 Task가 실패한 백업은 다음 실행이 끝날 때 다시 지운다.
  남는 백업(실패하지 않은 모든 백업 — 다른 작업·손으로 만든 증분도)의 조상은 보존 규칙이 지울 것이어도 남긴다(그
  체인으로 복원해야 하므로). 지울 것은 자식부터 지우고, 자식이 남아 `backup_in_use`인 부모는 다음 확인 때 지운다.
  그래서 증분을 쓰면 보존 값보다 백업이 더 남을 수 있다 — 작업이 만든 체인은 `full_every`가 상한이다(수동
  `--incremental`은 상한 없음).
- **증분 작업(`full_every`).** `--full-every N`(API `full_every`, 웹 "전체 백업 사이의 증분 백업 수")이 0보다 크면
  작업은 백업을 증분으로 청하고(위 "증분 백업"), 체인에 증분이 N개 쌓이면 다음은 전체로 해 새 체인을 시작한다(이유
  `chain already has N incremental backup(s)`). 예: 매일 + `--full-every 6`이면 주마다 전체 하나와 증분 여섯이다. 할 수
  없으면(꺼진 VM 등) 그 백업만 전체다. 0(기본)이면 늘 전체다. 모든 노드에 증분 백업 기능이 있어야 하고(409
  `cluster_upgrading`), `--replicate`와 함께 두면 증분 백업도 체인 순서로 원격에 보낸다(아래 "원격 복제(DR)").
  `backup-job list`의 `SCHEDULE` 칸에는 `, full every N`, 웹 작업 표의 일정 칸에는 "증분, 증분 n개마다 전체"가 보인다.
- **작업을 지워도 백업은 남는다.** 그 백업은 더는 보존 규칙으로 지워지지 않으니 `oplctl backup delete`로 지운다.
- 웹은 사이드바 "백업" 아래 "백업 작업" 화면(만들기·편집·지우기·지금 실행, 다음·마지막 실행 시각은 UTC로 표시)에서
  한다. API는 `GET`·`POST /api/v1/backup-jobs`, `GET`·`PATCH`·`DELETE /api/v1/backup-jobs/{name}`,
  `POST /api/v1/backup-jobs/{name}/run`이고, 읽기는 모든 사용자, 쓰기는 operator 이상이다. 스케줄러의 확인 간격은
  `[backup] tick_secs`(기본 60초)다.

- **한계.** 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존 VM 덮어쓰기는 없다.
  다른 opLite 클러스터로 사본을 보내는 것은 아래 "원격 복제(DR)"다. 증분 백업은: 꺼진 VM은 늘 전체(매일 백업하는
  꺼진 VM은 매일 전체를 쓴다), 블록 풀 VM은 다시 켠 뒤 첫 백업이 전체, VM마다 비트맵이 하나라 두 풀에 번갈아 백업하면
  늘 전체, 체인 병합과 중간 백업 지우기 없음, 증분 파일만으로는 복원할 수 없음(opLite 밖에서는 위처럼 체인을 적어
  연다).
- **남을 수 있는 것.** 작업을 만들기 전에 실패하거나 작업이 사라져 `pending`에 머문 백업은 저절로 치워지지 않는다 —
  `oplctl backup delete`로 지운다. 실행 중 백업이 임시 스냅샷을 지우지 못하면 qcow2 디스크에 그 태그가 남을 수 있다
  (블록 풀 디스크는 opld가 나중에 지운다; qcow2는 VM을 끄고 `qemu-img snapshot -d <태그> <파일>`). 복원이 볼륨을
  다 만든 뒤 VM을 만들기 전에 opld가 죽으면 `{새 이름}-disk{i}` 볼륨이 남는다(복제도 같다) — `oplctl volume delete`로
  지운다. 백업 디렉터리의 파일 작업에는 시간 제한이 없어, NFS 서버가 멈추면(hard 마운트) 백업·지우기·복원 작업이
  서버가 돌아올 때까지 멈춘다.

## VMware에서 가져오기

VMware VM을 OVA 파일로 내보내거나 ESXi·vCenter 데이터스토어에서 바로 읽어 opLite로 가져온다. 가져오기(`vm-import`)는
세 단계다: OVA를 올리거나 ESXi의 VM을 고르면 opld가 OVF(또는 `.vmx`와 디스크 디스크립터)를 읽어 원본 VM(CPU, 메모리,
펌웨어, 디스크, NIC)과 opLite VM 제안을 보이고, 사용자가 이름·풀·네트워크 매핑과 고칠 값을 정해 실행하면 디스크를
볼륨으로 옮겨 **꺼진** 새 VM을 만든다. 실행이 실패하면 원본이 남으므로 값을 바꿔 다시 실행한다.

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
  꺼진 채 돌아오지 않으면 `oplctl import delete web01 --force`로 기록만 지운다(원본 파일은 그 노드에 남는다). ESXi
  가져오기도 `import esxi`를 받은 노드에서 실행한다.
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
  같다) — `oplctl volume delete`로 지운다.

## 웹 UI

opld를 `web-ui` 기능으로 빌드하면 API와 같은 주소(`https://<노드>:8006/`)에서 웹 UI가 열린다.
Node.js 22.12 이상과 pnpm이 필요하다.

```bash
pnpm -C web install
pnpm -C web build                                   # web/dist
cargo build -p opld --release --features web-ui     # web/dist를 바이너리에 넣는다
```

브라우저는 클러스터 CA를 모르므로 처음 접속하면 인증서 경고가 뜬다. CA를 내려받아
(`curl -k https://<노드>:8006/api/v1/cluster/ca -o opl-ca.pem`, `openssl x509 -in opl-ca.pem -noout
-fingerprint -sha256`의 지문을 `cluster init` 출력과 비교) 운영체제나 브라우저의 신뢰할 인증서로
가져오면 경고가 사라진다. 로그인한 뒤에는 클러스터 화면에서 CA를 내려받고 지문을 볼 수 있다.

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·증분·복원·지우기·원격으로 복제)과 백업 작업(증분 주기·원격 복제), VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 비우기, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 원격 클러스터(상태 카드, 원격 VM 목록과 동작), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
그 노드에서 쓸 수 있는 풀에만 가져올 수 있다.

- **작업 보존.** 끝난(성공·실패) 작업은 만든 지 7일이 넘으면 지워지고, 그래도 1000개를 넘으면 오래된 것부터
  지워진다(opld 설정 `[janitor]`의 `task_max_age_days`·`task_max_finished`, 리더 노드가 지운다). 어느 쪽이든 끝난
  뒤 10분은 남겨 결과를 볼 수 있게 한다. 처리 중인 작업은 지우지 않는다. 작업 화면과 VM 상세의 작업 탭은 50개씩 페이지로 나눠 보여 준다.
- **막힌 동작과 이유.** 할 수 없는 메뉴 항목·버튼은 숨기지 않고 막힌 채로 이유를 보인다(예: "VM이 정지되어
  있습니다.", "내 계정은 다른 관리자가 삭제합니다."). 키보드로 닿고 화면 읽기 프로그램이 이유를 읽는다. VM에 처리
  중인 시작·정지·재시작 작업이 있으면 웹에서는 시작·정지·재시작이 막힌다(강제 정지는 된다. CLI·API에서는 새 요청이
  앞 작업을 대체한다).
- **캐시.** 웹 파일에는 내용 해시로 만든 ETag가 붙는다. `index.html`은 매번 다시 확인하지만 바뀌지 않았으면
  **304**로 본문 없이 끝난다.

## 로그인과 세션

- **시도 제한.** 같은 사용자 이름·IP로 5번 연속 실패하면 30초, 같은 IP에서 (사용자 이름과 상관없이) 20번
  연속 실패하면 30초 동안 막힌다. 풀린 뒤에도 실패가 이어지면 실패할 때마다 막는 시간이 두 배가 된다(최대 15분).
  막힌 동안에는 맞는 비밀번호도 **429** `too_many_attempts`("try again in N seconds", `Retry-After` 헤더)를
  받는다. 로그인에 성공하면 그 사용자 이름·IP의 기록은 지워지지만, IP의 기록에서는 그 시도 하나만 빠진다(자기
  계정으로 로그인해 IP의 실패 수를 지울 수 없다). 맞는 로그인이 IP를 새로 잠그지는 않는다. 마지막 실패 뒤 15분이 넘게 지나면 기록이 지워진다. IPv6 주소는
  /64 단위로 센다. 자기 비밀번호를 바꿀 때의 현재
  비밀번호 확인도 같은 기록으로 센다. 노드마다 따로 세고, opld를 다시 시작하면 기록이 사라진다.
- **프록시 뒤.** IP는 TLS 연결의 주소다(`X-Forwarded-For`는 보지 않는다). 리버스 프록시 뒤에 두면 모든 요청이
  프록시 IP로 보이므로, 한 사람이 20번 틀리면 프록시를 거치는 모든 사용자가 함께 막힌다.
- **비밀번호 변경.** 비밀번호를 바꾸면(본인이든 admin이든) 모든 노드에서 그 사용자의 로그인 세션이 끝나 다시
  로그인해야 한다. 역할만 바꾸면 세션은 그대로다. **API 토큰은 끊기지 않는다.** 토큰이 새었을 수 있으면
  `oplctl token list`·`oplctl token delete`(웹: API 토큰 화면)로 따로 지운다. 열려 있는 콘솔(WebSocket)은 연결할
  때 한 번 인증하므로 비밀번호를 바꿔도 끊기지 않는다.

## 외부 인증(LDAP)

인증 영역(`Realm`)을 만들면 로컬 사용자 말고도 바깥 LDAP 디렉터리(OpenLDAP, Active Directory 등)의 계정으로
로그인할 수 있다(OpenID Connect IdP로 로그인하는 OIDC 영역은 다음 절 "SSO(OIDC)"). 영역은 admin만 보고
만들고 고친다(operator·viewer는 읽기도 **403**). 영역 이름은 리소스 이름 규칙(1~63자의 영문 소문자·숫자·`-`)이고
`local`은 로컬 사용자를 뜻하는 예약 이름이다. UUID로 읽히는 이름(예: `0192f0c1-5d2e-7000-8000-000000000000`)도
API가 id로 읽어 이름으로 부를 수 없으므로 **400**이다.

- **외부 사용자.** 영역 `corp`에 `alice`로 처음 로그인하면 opLite 사용자 `alice@corp`가 생긴다(로컬 사용자 `alice`와는
  다른 사용자다). 이름의 사용자 부분은 소문자로 바꾸고(디렉터리에도 소문자로 찾는다) 1~64자의 `a-z`·`0-9`·`.`·`_`·`-`만
  된다. 이 형식이 아닌 계정은 로그인할 수 없다(**401**). 비밀번호는 opLite에 저장하지 않고 늘 디렉터리가 확인한다.
  외부 사용자의 비밀번호를 바꾸려 하면 **400** `external_user`다(웹에는 그 메뉴가 없다). 외부 사용자도 API 토큰을
  만들 수 있다. 외부 사용자를 지우면(`oplctl user delete alice@corp`) 세션과 토큰이 함께 끝나고, 디렉터리가 받아 주면
  다음 로그인에 다시 생긴다.
- **역할 매핑.** 로그인할 때마다 사용자 항목의 그룹 속성(기본 `memberOf`) 값들을 영역의 그룹 → 역할 표와 비교한다.
  앞뒤 공백을 떼고 ASCII 대소문자만 무시한 문자열 비교다(DN을 정규화하지 않으므로 `cn=ops, dc=example`과
  `cn=ops,dc=example`은 다르다 — `oplctl realm test`가 보이는 값을 그대로 쓴다). 여러 그룹이 맞으면 가장 높은 역할
  (admin > operator > viewer), 맞는 것이 없으면 기본 역할, 그것도 없으면 **403** `no_role`("no role is mapped for this
  user")이다. 정해진 역할이 지금과 다르면 사용자를 고치고 그 사용자의 다른 로그인 세션을 끝낸다(같으면 아무것도 쓰지
  않는다). admin이 외부 사용자의 역할을 바꿀 수는 있지만 다음 로그인에 매핑으로 다시 정해진다.

```bash
# OpenLDAP(memberof 오버레이): 서비스 계정으로 찾는다. --group은 <그룹 DN>=<role>(마지막 =에서 나눔, 여러 번)
oplctl realm add-ldap corp --display "Corp LDAP" \
  --url ldap://ldap.example.com --base-dn ou=people,dc=example,dc=com \
  --bind-dn cn=oplite,ou=services,dc=example,dc=com --bind-password-file /root/corp-bind.pw \
  --ca-file /etc/ssl/certs/corp-ca.pem \
  --group cn=admins,ou=groups,dc=example,dc=com=admin \
  --group cn=ops,ou=groups,dc=example,dc=com=operator --default-role viewer
# Active Directory: sAMAccountName으로 찾는다(그룹은 memberOf 그대로)
oplctl realm add-ldap ad --url ldaps://dc1.corp.example.com --base-dn "DC=corp,DC=example,DC=com" \
  --bind-dn "CN=svc-oplite,OU=Service Accounts,DC=corp,DC=example,DC=com" --bind-password-file /root/ad-bind.pw \
  --user-filter "(&(objectClass=user)(sAMAccountName={username}))" \
  --group "CN=oplite-admins,OU=Groups,DC=corp,DC=example,DC=com=admin"
oplctl realm list
# NAME  DISPLAY    KIND  URL
# corp  Corp LDAP  ldap  ldap://ldap.example.com
oplctl realm show corp            # 설정(bind password: set|none, ca: custom (PEM)|system roots, 그룹 표, 기본 역할)
oplctl realm test corp --user alice   # 비밀번호는 터미널에서 묻는다(--password-file <파일|->)
# STEP          RESULT  MESSAGE
# connect       ok      connected to ldap://ldap.example.com with StartTLS
# service_bind  ok      bound as cn=oplite,ou=services,dc=example,dc=com
# search        ok      found uid=alice,ou=people,dc=example,dc=com
# user_bind     ok      bound as uid=alice,ou=people,dc=example,dc=com
# groups        ok      2 value(s) of memberOf
# dn: uid=alice,ou=people,dc=example,dc=com
# groups: cn=admins,ou=groups,dc=example,dc=com, cn=staff,ou=groups,dc=example,dc=com
# role: admin
oplctl realm set-ldap corp --timeout 5            # 준 것만 바꾼다(읽은 revision으로 — 그 사이 바뀌었으면 412)
oplctl realm set-ldap corp --bind-password-file /root/corp-bind.pw   # 새 서비스 계정 비밀번호
# URL·바인드 DN·CA를 바꿀 때는 비밀번호도 다시 준다(주지 않으면 서버가 400)
oplctl realm set-ldap corp --url ldaps://ldap2.example.com --bind-password-file /root/corp-bind.pw
oplctl realm set-ldap corp --clear-bind-dn          # 익명 검색으로(저장된 비밀번호도 지움)
oplctl realm set-ldap corp --clear-ca --clear-default-role --clear-groups   # 시스템 루트, 기본 역할·그룹 표 없음
oplctl realm remove corp
oplctl login --server https://10.0.0.11:8006 --realm corp --user alice   # 기본 --realm local
```

- **설정 항목**(API `backend`의 필드, oplctl 선택):
  - `url`(`--url`): `ldap://호스트[:포트]` 또는 `ldaps://호스트[:포트]`. 서버 하나만 쓴다.
  - `base_dn`(`--base-dn`): 사용자를 찾을 기준 DN(그 아래 전체, subtree).
  - `bind_dn`·`bind_password`(`--bind-dn`, `--bind-password-file <파일|->`): 사용자를 찾을 서비스 계정. 둘은 함께 있어야
    한다 — `bind_dn`에 비밀번호가 없거나 빈 글이면 **400**("bind_dn needs bind_password; remove bind_dn for an anonymous
    search"), 비밀번호만 있어도 **400**이고, 둘 다 없을 때만 익명으로 찾는다. 비밀번호는 API 응답에 없고
    `has_bind_password`로 있는지만 보인다. 바꾸기(`PUT`)에서 필드를 빼면 저장된 값을 두는데, `url`·`bind_dn`·`ca_pem`이
    모두 그대로일 때만이다: 하나라도 바꾸면서 빼면 **400** `invalid_request`("bind_password must be given again when url,
    bind_dn or ca_pem changes" — 저장된 비밀번호를 다른 서버나 계정으로 보내지 않는다). `""`이면 지우므로 `bind_dn`도 함께
    빼야 한다(`--clear-bind-dn`, 웹은 "저장된 비밀번호 지우기"와 바인드 DN 비우기).
  - 서버는 저장·비교하기 전에 `url`·`bind_dn`의 앞뒤 공백을 떼고, `ca_pem`은 CRLF를 LF로 바꾼 뒤 앞뒤 공백을 뗀다(만들기·바꾸기
    모두). 그래서 끝 줄바꿈이 있는 CA 파일(`--ca-file`)로 만든 영역을 웹에서 고쳐도 같은 CA로 보고 비밀번호를 그대로 둔다.
  - `user_filter`(`--user-filter`, 기본 `(&(objectClass=person)(uid={username}))`): `{username}`이 정확히 한 번 있어야
    하고, 로그인 이름을 RFC 4515로 이스케이프해 넣는다. Active Directory는 `(&(objectClass=user)(sAMAccountName={username}))`,
    UPN으로 찾으려면 `(userPrincipalName={username}@corp.example.com)`처럼 쓴다. 로그인할 수 있는 사람을 줄이는 조건도
    필터에 넣는다(예: AD의 `(memberOf:1.2.840.113556.1.4.1941:=CN=oplite,…)`).
  - `group_attr`(`--group-attr`, 기본 `memberOf`): 그룹 DN을 담은 사용자 속성. 서비스 계정(또는 익명)이 읽을 수 있어야 한다.
    OpenLDAP는 memberof 오버레이가 있어야 `memberOf`가 생긴다.
  - `ca_pem`(`--ca-file`): 서버 인증서를 확인할 CA(PEM). 있으면 그 인증서들만 믿고, 없으면 시스템 루트를 쓴다.
  - `timeout_secs`(`--timeout`, 기본 10, 1~30): 연결과 LDAP 작업마다의 시간 제한(초). 확인 전체(연결·바인드·검색·바인드)는
    `timeout_secs × 4 + 1`초(최대 121초) 안에 끝나고, 넘으면 **503**이다(시험 동작은 그 단계가 "timed out").
  - `display`(`--display`): 로그인 화면에 보일 이름. `roles`: `groups`(`--group`, `set-ldap`에서 주면 표 전체를 바꾼다)와
    `default`(`--default-role`).
  - `set-ldap`은 영역을 읽어 준 것만 바꾼다. 없애는 선택은 `--clear-bind-dn`(바인드 DN과 저장된 비밀번호 — 익명 검색),
    `--clear-ca`(시스템 루트), `--clear-default-role`, `--clear-groups`이고, 같은 값을 정하는 선택과 함께 줄 수 없다.
    `--clear-bind-password`는 바인드 DN이 남으면 보내지 않고 오류다("--clear-bind-password needs --clear-bind-dn (or give
    --bind-password-file)"). URL·바인드 DN·CA를 바꾸면서 `--bind-password-file`을 주지 않으면 서버의 **400**을 그대로 보인다.
  - 나쁜 값(형식이 아닌 URL, 빈 기준 DN, `{username}`이 없거나 둘, 시간 제한 범위 밖, 빈 그룹 DN 등)과 이름 `local`은 **400**
    `invalid_request`다.
- **TLS.** 평문으로는 바인드하지 않는다. `ldap://`는 늘 StartTLS를 쓰고(서버가 StartTLS를 하지 못하면 연결 실패),
  `ldaps://`는 처음부터 TLS다. 서버 인증서는 CA와 **호스트 이름**을 모두 확인한다: URL의 호스트(이름이나 IP)가 인증서의
  SAN에 있어야 한다. 시스템 루트는 노드의 인증서 저장소(Debian: `ca-certificates`)이고, 읽지 못하면 믿는 루트가 없어 모든
  연결이 실패한다. 사설 CA면 `--ca-file`을 준다.
- **로그인 순서.** 빈 비밀번호는 디렉터리에 묻지 않고 거절한다(익명 바인드로 통과하는 것을 막는다). 연결 → 서비스 계정
  바인드(없으면 생략) → `base_dn` 아래에서 `user_filter`로 찾기(`group_attr`만 읽음, 크기 제한 2·시간 제한 `timeout_secs`;
  서버가 크기 제한을 지키지 않아도 둘째 항목에서 멈추고 검색을 버린다) → 찾은 항목이 정확히 하나가 아니면(결과 코드 4
  sizeLimitExceeded도 여럿으로 봄) 실패 → 그 DN과 비밀번호로 바인드 → 그룹 값으로 역할 → 사용자 만들기·고치기 → 세션.
  끝나면 연결을 끊는다(unbind). 사용자 확인이 자격 증명 실패로 끝나면 시작부터 적어도 300ms 뒤에 답한다. 디렉터리가
  300ms 안에 답하면 없는 사용자와 틀린 비밀번호의 응답 시간 차가 가려지지만, 그보다 느린 디렉터리에서는 차이가 남을 수
  있다(시험 동작은 기다리지 않는다).
- **오류.** 없는 사용자, 여러 항목, 틀린 비밀번호(LDAP 결과 49)는 모두 로컬과 같은 **401** `invalid_credentials`다.
  연결·TLS 실패, 믿지 않는 인증서, 서비스 계정 바인드·검색 실패, 사용자 바인드의 49가 아닌 실패는 **503**
  `realm_unavailable`("the directory of realm corp is unavailable")이고 원인은 opld 로그(`journalctl -u opld`)에 경고로
  남는다. 모르는 영역은 **400** `invalid_request`, 매핑되는 역할이 없으면 **403** `no_role`이다. 롤링 업그레이드 중(옛 opld
  노드가 있을 때) 영역 로그인과 영역 쓰기는 **409** `cluster_upgrading`이다(로컬 로그인은 된다).
- **시도 제한.** 로컬 로그인과 같은 기록이고 키는 외부 사용자 이름(`alice@corp`)과 IP다. **401뿐 아니라 403 `no_role`과
  503 `realm_unavailable`도 실패로 센다**: 디렉터리가 멈춘 동안 되풀이해 로그인하면 잠시 막힐 수 있다. 형식이 아닌 이름은
  IP만 센다.
- **감사 로그.** 영역 로그인의 `detail`은 `username=alice realm=corp`이다(이름 형식이 아니면 `<invalid>`). 영역 만들기·고치기·
  지우기·시험은 다른 바꾸는 요청처럼 적히고, 서비스 계정·사용자 비밀번호는 어디에도 넣지 않는다.
- **시험 동작.** `oplctl realm test`(웹: 영역의 "시험", API `POST /api/v1/realms/{key}/actions/test {"username","password"}`,
  admin)는 로그인과 같은 순서를 `connect`·`service_bind`·`search`·`user_bind`·`groups` 단계로 돌려 단계마다 성공·실패와
  내용, 찾은 DN, 그룹 값, 정해질 역할을 보인다. 실패한 단계에서 멈춘다. 사용자·세션을 만들지 않고 시도 제한에도 세지
  않으며 비밀번호는 응답에 없다. oplctl은 실패한 단계가 있으면 오류로 끝난다(역할만 없으면 성공 — `role: none`).
- **웹.** 로그인 화면은 `local` 말고도 영역이 있으면 영역 고르기를 보인다(기본 "로컬 사용자", 영역은 표시 이름이나 이름).
  "인증 영역" 메뉴(admin에게만, 사용자 다음): 목록(이름, 표시 이름, 종류, URL), LDAP 영역 만들기·고치기(바인드 비밀번호는
  비워 두면 그대로, "저장된 비밀번호 지우기"로 지움 — 서버와 같은 규칙으로 URL·바인드 DN·CA를 바꾸면 비밀번호를 다시
  넣으라고(바인드 DN만 비웠으면 "저장된 비밀번호 지우기"를 고르라고), 바인드 DN에 비밀번호가 없거나 지우기에 바인드 DN이
  남으면 저장 전에 알린다, 그룹 → 역할 표 더하기·빼기,
  기본 역할), 지우기, 시험 대화상자(단계 표,
  DN, 그룹, 정해질 역할). 사용자 목록에 영역 열(로컬 사용자는 `local`)이 있다.
- **API.** `GET`·`POST /api/v1/realms`, `GET`·`PUT`·`DELETE /api/v1/realms/{key}`(`PUT`은 통째로 바꾸고 `If-Match`를 따른다,
  이름은 바꿀 수 없다), 로그인 전 화면용 `GET /api/v1/auth/realms`(인증 없음, `[{"name","display","kind"}]`, `local`이 먼저이고
  나머지는 이름 순), `POST /api/v1/auth/login {"username","password","realm"}`(`realm`이 없거나 `local`이면 로컬 로그인,
  영역 로그인의 `username`은 `@영역` 없는 이름). 영역은 watch로 내보내지 않는다(서비스 계정 비밀번호가 들어 있다).
- **영역을 지울 때.** 그 영역의 사용자는 로그인할 수 없고(모르는 영역 **400**), 모든 노드에서 그 사용자들의 세션과 API 토큰이
  다음 요청부터 **401**이다. `alice@corp` 같은 사용자와 토큰은 남으므로 같은 이름의 영역을 다시 만들면 다시 쓸 수 있다
  — 새 영역의 디렉터리·매핑과 상관없이 **옛 세션(12시간 안)과 토큰이 지울 때의 역할 그대로 되살아난다**(아래 한계).
  완전히 없애려면 사용자도 지운다. 이미 열린 콘솔(WebSocket)은 끊기지 않는다.

한계:

- 디렉터리는 로그인할 때만 본다. 디렉터리에서 계정을 막거나 지우거나 그룹을 빼도 이미 있는 세션(최대 12시간)과 API 토큰은
  남고 역할은 다음 로그인에 바뀐다. 바로 막으려면 opLite에서 그 사용자를 지운다(토큰도 함께 지워진다).
- 그룹은 사용자 항목의 속성 값만 본다. 그룹 항목을 찾는 방식(`posixGroup`의 `memberUid` 등)과 중첩 그룹 풀기(AD의
  `memberOf`는 직접 속한 그룹만)는 없다. DN은 문자열로만 비교한다.
- 서버 주소는 하나다(장애 조치·여러 서버 없음). 참조(referral)는 따라가지 않는다. 단순 바인드만 한다(SASL·Kerberos·클라이언트
  인증서 없음).
- 서비스 계정 비밀번호는 Raft에 평문으로 있다(구독 키와 같이 노드의 root만 읽는다).
- 영역을 지우고 같은 이름으로 다시 만들면(다른 디렉터리를 가리켜도) 옛 영역 사용자의 세션과 API 토큰이 지울 때의 역할로
  다시 동작한다. 영역을 다른 용도로 다시 쓰기 전에는 그 영역의 사용자(`oplctl user list`의 `<사용자>@<영역>`)를 먼저 지운다.
- 같은 사용자가 거의 동시에 두 번 로그인하고 그 사이 디렉터리의 그룹이 바뀌면, 옛 그룹을 읽은 로그인이 나중에 써서 역할이
  옛 것으로 되돌아갈 수 있다(드문 경쟁 — 다음 로그인에 바로잡힌다).
- 사용자 부분이 `a-z`·`0-9`·`.`·`_`·`-`가 아닌 계정(공백·`@`·한글 등)은 로그인할 수 없다. 사용자·그룹 동기화, 외부 사용자의
  2단계 인증은 없다. IdP로 로그인하는 영역은 아래 "SSO(OIDC)".

## SSO(OIDC)

OIDC 영역을 만들면 사용자가 OpenID Connect IdP(Keycloak, Okta, Microsoft Entra ID 등)에서 로그인하고 opLite로
돌아온다(인가 코드 흐름과 PKCE). 영역의 공통 규칙은 "외부 인증(LDAP)"과 같다: admin만 보고 만들고 고치며, 이름 규칙,
외부 사용자 `<사용자>@<영역>`, 로그인할 때마다 그룹 → 역할 매핑(앞뒤 공백을 떼고 ASCII 대소문자 무시, 가장 높은 역할,
기본 역할, 없으면 `no_role`), 역할이 바뀌면 그 사용자의 다른 세션이 끝남, 외부 사용자의 API 토큰, 영역을 지울 때가 모두
같다. 그룹 값은 그룹 클레임의 문자열이다. OIDC 영역 사용자는 비밀번호로 로그인하지 않는다: `POST /auth/login`에 OIDC
영역을 주면 이름 모양과 관계없이(이메일 모양 이름도) 늘 **400** `invalid_request`("realm okta signs in through its
identity provider" — 시도 제한에 세지 않음)다.

- **이름.** OIDC 영역의 사용자 부분은 이름 클레임을 소문자로 바꾼 것이고 1~128자의 `a-z`·`0-9`·`.`·`_`·`+`·`-`와 안쪽
  `@` 하나까지다(이메일 모양 — 앞뒤가 비어 있지 않음, 공백·`/`·`:`·`=` 없음). 그래서 Okta·Entra ID의 `preferred_username`
  `Alice@Corp.com`은 그대로 opLite 사용자 `alice@corp.com@okta`가 된다(영역은 마지막 `@` 뒤). LDAP 영역의 규칙(1~64자,
  `@` 없음)은 그대로다.
- **IdP 사용자에 묶기.** 외부 사용자를 처음 만들 때 IdP의 사용자(`"<issuer> <sub>"`)를 적어 둔다(`subject`). 뒤에 같은
  이름으로 로그인한 사람의 `sub`가 다르면(IdP에서 이름을 다른 사람에게 다시 준 경우 등) `sso_failed`로 거절하고 사용자는
  그대로 둔다(opld 로그에 그 이름과 "subject mismatch"). `subject`가 없는 사용자(이 기능 전에 만들어졌거나 LDAP 영역이었던
  이름)는 다음 OIDC 로그인의 것을 적는다.

- **IdP에 등록.** IdP에 opLite를 웹 애플리케이션(인가 코드 흐름)으로 등록한다.
  - 리디렉트 URI는 `https://<브라우저가 연 노드 주소>/api/v1/auth/oidc/callback`이다(예:
    `https://10.0.0.11:8006/api/v1/auth/oidc/callback`). opld는 로그인을 시작할 때 요청의 `Host` 헤더로 이 주소를 만들므로
    사용자가 들어오는 주소(노드마다, 이름과 IP도 따로)를 모두 등록한다. 리버스 프록시 뒤라면 프록시가 `Host`로 넘기는
    주소를 등록한다. 웹의 영역 대화상자가 지금 노드의 값을 복사 버튼과 함께 보이고, `oplctl realm show`는 노드 주소 뒤에
    붙일 경로(`callback path`)를 보인다.
  - 클라이언트는 기밀 클라이언트(클라이언트 비밀, 토큰 끝점에 `client_secret_basic`)나 공개 클라이언트(비밀 없음)다.
    PKCE(S256)는 둘 다 늘 쓴다.
  - 사용자 이름 클레임(기본 `preferred_username`)은 위 이름 규칙에 맞는 문자열이어야 한다(이메일 모양도 된다). 그룹
    클레임(기본 `groups`)은 문자열 배열이다(없으면 그룹 없음).
  - 노드도 IdP에 직접 닿아야 한다(discovery·JWKS·토큰·userinfo 끝점, https만). 브라우저는 IdP와 노드 둘 다에 닿아야 한다.

```bash
# Keycloak(영역 corp): 클라이언트 oplite(Client authentication 켬 — 끄면 공개 클라이언트), Valid redirect URIs에
# 위 주소. 클라이언트 범위에 "Group Membership" 매퍼(Token Claim Name groups, Add to ID token 켬, Full group path
# 끔 — 켜면 값이 /admins 꼴). preferred_username은 Keycloak 사용자 이름 그대로다.
oplctl realm add-oidc kc --display "Keycloak" \
  --issuer https://kc.example.com/realms/corp --client-id oplite --client-secret-file /root/kc-secret \
  --group admins=admin --group ops=operator --default-role viewer
# Okta(사용자 정의 인가 서버 default): 앱(OIDC Web Application)의 Sign-in redirect URIs에 위 주소. 인가 서버의 Claims에
# groups(Groups 필터, ID Token·Always). preferred_username은 로그인 이메일이라 사용자는 alice@example.com@okta가 된다.
oplctl realm add-oidc okta --display "Okta" \
  --issuer https://example.okta.com/oauth2/default --client-id 0oa1b2c3d4 --client-secret-file /root/okta-secret \
  --group opl-admins=admin --group opl-operators=operator
# Microsoft Entra ID: 앱 등록(웹 플랫폼)의 리디렉트 URI에 위 주소, "인증서 및 비밀"의 클라이언트 비밀, "토큰 구성"의
# 그룹 클레임 추가(보안 그룹 — ID 토큰의 groups에 그룹 개체 ID가 들어간다). preferred_username은 UPN이라 사용자는
# alice@contoso.com@entra가 된다. 그룹이 200개를 넘는 사용자는 groups 클레임이 오지 않아 그룹이 없다.
oplctl realm add-oidc entra --issuer https://login.microsoftonline.com/<테넌트 ID>/v2.0 \
  --client-id <애플리케이션 ID> --client-secret-file /root/entra-secret \
  --group 5f2c9a1e-0000-4000-8000-000000000001=admin
oplctl realm list
# NAME  DISPLAY   KIND  URL
# kc    Keycloak  oidc  https://kc.example.com/realms/corp
oplctl realm show kc       # issuer·client id·client secret: set|none·scopes·클레임·ca·timeout과 callback path
oplctl realm test kc       # OIDC는 사용자·비밀번호 없이 IdP만 확인한다(--user는 경고하고 무시)
# STEP       RESULT  MESSAGE
# discovery  ok      issuer https://kc.example.com/realms/corp, authorization endpoint …, token endpoint …
# jwks       ok      1 RS256 and 0 ES256 signing keys
oplctl realm set-oidc kc --timeout 5                  # 준 것만 바꾼다(읽은 revision으로 — 그 사이 바뀌었으면 412)
oplctl realm set-oidc kc --client-secret-file /root/kc-secret   # 새 클라이언트 비밀
# issuer·client ID·CA를 바꿀 때는 비밀도 다시 준다(주지 않으면 서버가 400)
oplctl realm set-oidc kc --issuer https://sso.example.com/realms/corp --client-secret-file /root/kc-secret
oplctl realm set-oidc kc --clear-client-secret        # 공개 클라이언트로(IdP 쪽도 바꾼다)
oplctl realm set-oidc kc --scope openid --scope profile --scope groups   # scope 목록 전체를 바꾼다
oplctl realm set-oidc kc --clear-ca --clear-default-role --clear-groups
```

- **설정 항목**(API `backend`의 필드 — `"kind": "oidc"`, oplctl 선택):
  - `issuer`(`--issuer`): IdP의 issuer. 호스트가 있는 `https://` URL이고 쿼리·조각·사용자 정보·공백이 없어야 한다.
    discovery 문서(`<issuer>/.well-known/openid-configuration` — issuer 끝의 `/`는 하나로 합친다)의 `issuer`, ID 토큰의
    `iss`와 **글자 그대로** 같아야 하므로 끝 `/`까지 IdP가 알리는 대로 쓴다(Keycloak `https://<호스트>/realms/<영역>` —
    옛 판은 `/auth/realms/<영역>`, Okta `https://<조직>.okta.com/oauth2/default`, Entra ID
    `https://login.microsoftonline.com/<테넌트 ID>/v2.0`).
  - `client_id`(`--client-id`): 비어 있으면 안 된다.
  - `client_secret`(`--client-secret-file <파일|->`): 없으면 공개 클라이언트. API 응답에 없고 `has_client_secret`으로 있는지만
    보인다. 바꾸기(`PUT`)에서 빼면 저장된 값을 두는데 `issuer`·`client_id`·`ca_pem`이 모두 그대로일 때만이다: 하나라도
    바꾸면서 빼면 **400** `invalid_request`("client_secret must be given again when issuer, client_id or ca_pem changes" —
    저장된 비밀을 다른 IdP·클라이언트로 보내지 않는다). 저장된 비밀이 없으면(공개 클라이언트) 이 규칙은 없다. `""`이면
    지운다(`--clear-client-secret`, 웹은 "저장된 비밀 지우기(공개 클라이언트)").
  - 서버는 저장·비교 전에 `issuer`·`client_id`의 앞뒤 공백을 떼고 `ca_pem`은 LDAP처럼 다듬는다(CRLF → LF, 앞뒤 공백).
  - `scopes`(`--scope`, 여러 번, 기본 `openid` `profile` `email`): `openid`가 있어야 하고 값마다 공백 없는 비어 있지 않은 글.
    authorization 요청에 공백으로 이어 보낸다. `set-oidc`에서 주면 목록 전체를 바꾼다.
  - `username_claim`(`--username-claim`, 기본 `preferred_username`), `groups_claim`(`--groups-claim`, 기본 `groups`):
    비어 있으면 안 된다.
  - `ca_pem`(`--ca-file`): IdP 인증서를 확인할 CA(PEM). 있으면 그 인증서들만 믿고, 없으면 시스템 루트를 쓴다. 호스트
    이름도 확인한다.
  - `timeout_secs`(`--timeout`, 기본 10, 1~30): IdP 요청마다의 시간 제한(초).
  - `display`(`--display`): 로그인 화면 버튼의 이름. `roles`: `groups`(`--group <그룹 클레임 값>=<role>`, 여러 번)와
    `default`(`--default-role`).
  - Entra ID 메모: 클레임 변환(예: `ExtractMailPrefix`로 이메일 앞부분만)을 쓰려면 앱 매니페스트에
    `acceptMappedClaims: true`가 필요하고, 앱 전용 서명 키는 discovery 주소에 `?appid=`가 붙어야 하는데 opLite는
    `<issuer>/.well-known/openid-configuration`만 읽으므로 맞지 않는다 — 쓰지 않는다. 이메일 모양 이름을 그대로 쓰므로 보통은
    변환이 필요 없다.
  - `set-oidc`는 영역을 읽어 준 것만 바꾸고, 없애는 선택은 `--clear-client-secret`·`--clear-ca`·`--clear-default-role`·
    `--clear-groups`다(같은 값을 정하는 선택과 함께 줄 수 없다). `set-ldap`·`set-oidc`는 종류가 다른 영역을 거절한다
    ("realm kc is not an LDAP realm (its kind is oidc)"). 나쁜 값은 **400** `invalid_request`다.
- **흐름.**
  1. 로그인 화면의 "<표시 이름>(으)로 로그인"이 `GET /api/v1/auth/oidc/<영역>/start`로 간다(페이지 이동). 노드는 IdP의
     discovery 문서를 읽고(노드 메모리에 10분), 요청의 `Host`로 리디렉트 URI를 만들고, 무작위 `state`·`nonce`·PKCE 검증자와
     그 리디렉트 URI를 로그인 상태 쿠키에 넣은 뒤 IdP의 authorization endpoint로 302한다(`response_type=code`,
     `client_id`, `redirect_uri`, `scope`, `state`, `nonce`, `code_challenge`, `code_challenge_method=S256`).
  2. 사용자가 IdP에서 로그인하면 IdP가 `/api/v1/auth/oidc/callback?code=…&state=…`로 돌려보낸다. IdP는 시작할 때
     보낸 리디렉트 URI(그 주소)로 돌려보내는데, 상태가 쿠키에 있으므로 그 주소 뒤의 어느 노드가 받아도 된다(노드들 앞의
     로드 밸런서·DNS 이름).
  3. 콜백은 쿠키를 확인하고(서명, 10분 안, 쿼리의 `state`와 같음), 인가 응답의 `iss`를 본다(RFC 9207: discovery 문서가
     `authorization_response_iss_parameter_supported: true`면 있어야 하고, 있으면 늘 issuer와 같아야 한다 — 다른 IdP의
     응답을 섞는 공격 막기). 토큰 끝점에 코드·쿠키의 `redirect_uri`·`code_verifier`·`client_id`를 폼으로 보내(기밀
     클라이언트는 `client_secret_basic`) ID 토큰을 받는다. 콜백은 `Host`를 보지 않는다.
  4. ID 토큰을 확인한다: 서명은 RS256(RSA 2048~8192비트)·ES256(P-256)만이고 `none`·`HS256` 등은 거절, 헤더에 `crit`가
     있으면 거절, 키는 IdP의 JWKS(노드 메모리에 10분, `use`가 있으면 `sig`만; 모르는 `kid`면 다시 받는데 마지막으로 받은 지
     10초가 지났을 때만 — IdP 키 교체를 따라감; 다시 받아도 그 `kid`가 없으면 `kid` 없이 내보인 키로 확인), `iss`가 issuer와
     같음, `aud`에 client ID(여럿이거나 `azp`가 있으면 `azp`가 client ID), `exp`·`iat`·(있으면) `nbf`는 5분 오차, `nonce`가
     쿠키의 것과 같음, `sub`가 있음.
  5. 이름·그룹 클레임 가운데 ID 토큰에 없는 것이 있고 IdP에 userinfo 끝점이 있으면 액세스 토큰으로 userinfo를 한 번 읽어
     빈 것만 채운다(userinfo의 `sub`가 ID 토큰과 다르면 실패). 이름 → `<이름>@<영역>`, 그룹 → 역할, 사용자 만들기·고치기
     (LDAP과 같음, IdP 사용자에 묶기) → 세션 쿠키(로컬 로그인과 같은 12시간 세션), 로그인 상태 쿠키 지우기 → `/`로 302.
     로그인 전에 열려던 주소로는 돌아가지 않는다.
- **IdP 요청.** discovery·JWKS를 받지 못하면 그 실패를 같은 영역(같은 revision)에서 15초 동안 기억해 다시 받지 않고 같은
  오류를 낸다(막힌 IdP로 요청을 쏟지 않게). 같은 영역을 동시에 여러 로그인이 물어도 한 번만 받는다. IdP 응답 본문은 1 MiB까지만
  읽는다(넘으면 실패).
- **쿠키.** 로그인 상태 쿠키 `opl_oidc`의 값은 `{영역, state, nonce, PKCE 검증자, 리디렉트 URI, 만든 시각}` JSON의 base64url과 그 HMAC-SHA256
  이다. HMAC 키는 클러스터 CA 비밀 키(모든 노드에 있음)에서 HKDF-SHA256(정보 `opl-oidc-state`)으로 만들므로 새로 저장하는
  값 없이 어느 노드나 같은 키를 얻는다. 속성은 `HttpOnly; Secure; SameSite=Lax; Path=/api/v1/auth/oidc; Max-Age=600`이다
  (IdP에서 돌아오는 최상위 이동에 쿠키가 가도록 `Lax`). 콜백은 성공·실패 모두 이 쿠키를 지운다. 브라우저가 쿠키를 막으면
  SSO 로그인은 늘 실패한다.
- **오류.** 실패하면 `/login?error=<코드>`로 돌아가고 로그인 화면이 문구를 보인다. 원인은 opld 로그에 경고("an OIDC login
  failed" 또는 "cannot start an OIDC login", `journalctl -u opld`)로 남는다.
  - `sso_failed`: 쿠키 없음·서명이 틀림·10분 지남·`state` 다름, IdP가 `error`로 돌려보냄(사용자가 거절 등), 인가 응답의
    `iss`가 다르거나(지원한다는 IdP인데) 없음, 토큰 끝점이 코드를 거절함(4xx `invalid_grant` — 코드가 틀렸거나 이미 썼거나
    만료), ID 토큰 확인 실패, userinfo가 액세스 토큰을 거절함(401·403), 이름 클레임이 없거나 글이 아니거나 이름 규칙에 맞지
    않음, 그룹 클레임이 문자열 배열이 아님, userinfo `sub` 다름, 같은 이름의 사용자가 다른 IdP 사용자에 묶여 있음, 시작할 때
    OIDC 영역이 아니거나 없음, 롤링 업그레이드 중(옛 opld 노드가 있을 때 — LDAP의 409 대신).
  - `no_role`: 매핑되는 역할이 없음.
  - `realm_unavailable`: IdP에 닿지 못함(연결·TLS·믿지 않는 인증서·시간 제한) 또는 IdP 끝점(discovery·JWKS·토큰·userinfo)이
    2xx가 아닌 응답(위의 `invalid_grant`·userinfo 401·403 빼고 — 토큰 끝점의 `invalid_client` 같은 클라이언트 인증 거절도
    여기다), 시작이나 콜백에서 discovery 문서를 읽지 못하거나 맞지 않음(issuer 다름, https가 아닌 끝점).
  - 시작 요청의 `Host` 헤더가 없거나 호스트 글자가 아니면 **400**(JSON)이다. 콜백은 `Host`를 보지 않으므로 늘 302이고
    로그인 상태 쿠키를 지운다.
- **시도 제한.** OIDC 로그인(시작·콜백)은 로그인 시도 제한을 쓰지 않는다. 맞힐 비밀이 없기 때문이다: `state`는 서명된
  쿠키와 같아야 하고, 코드는 IdP가 한 번만 받으며 PKCE 검증자에 묶인다. 같은 NAT·프록시 뒤의 정상 실패(쿠키 만료 등)가
  그 주소의 모든 비밀번호 로그인을 막지 않게 한다.
- **감사 로그.** 콜백 `GET /auth/oidc/callback`을 적는다(다른 `GET`처럼 빼지 않음). `path`는 쿼리를 뺀
  `/auth/oidc/callback`이다. `detail`은 성공이면 `realm=kc username=alice`(사용자는 그 외부 사용자), 실패면
  `realm=kc error=no_role`이고 쿠키를 확인하기 전 실패는 `realm=<unknown>`이다. 실패 원인은 적지 않고 코드만이다(원인은 opld
  로그). 쿼리(코드·`state`)와 토큰은 어디에도 적지 않는다. 시작은 적지 않는다.
- **시험 동작.** `oplctl realm test <영역>`(웹: 영역의 "시험", API `POST /api/v1/realms/{key}/actions/test`를 본문 없이)은
  `discovery`(문서 받기, issuer 같음, 끝점 https)와 `jwks`(쓸 수 있는 RS256·ES256 키 수, 없으면 실패) 단계를 캐시 없이
  돌린다. 아무도 로그인하지 않으므로 클라이언트 비밀·리디렉트 URI·클레임은 확인하지 않는다 — 처음 한 번은 브라우저로
  로그인해 본다. LDAP 영역 시험은 `--user`(API `username`·`password`)가 없으면 **400**이다.
- **웹.** 로그인 화면은 OIDC 영역마다 아이디·비밀번호 폼 아래 "또는" 뒤에 "<표시 이름 또는 이름>(으)로 로그인" 버튼을
  보이고, 영역 고르기에는 로컬과 LDAP 영역만 있다. "인증 영역" 화면은 만들기에서 종류(LDAP/OIDC)를 고르고(고치기에서는
  종류를 바꾸지 않음), OIDC 폼(issuer, 클라이언트 ID, 클라이언트 비밀 — 비워 두면 그대로, issuer·클라이언트 ID·CA를
  바꾸면 다시 넣으라고 알림, scope는 공백으로 나눈 한 줄, 클레임, CA, 시간 제한, 그룹 → 역할, 기본 역할)과 이 노드의
  리디렉트 URI(복사 버튼), 입력 없는 시험(단계 표)이 있다. 목록의 URL 열은 issuer다.
- **API.** 영역 API는 LDAP과 같고(`backend`의 `kind`가 `oidc`, 응답은 `has_client_secret`), `GET /api/v1/auth/realms`의
  `kind`가 `oidc`다. 로그인 흐름은 `GET /api/v1/auth/oidc/{realm}/start`와 `GET /api/v1/auth/oidc/callback`(둘 다 인증
  없음, 302)이다.
- **oplctl.** CLI는 브라우저 SSO를 하지 않는다: `oplctl login --realm <OIDC 영역>`은 비밀번호를 묻기 전에 "sign in to kc in
  the web UI, create an API token there and run `oplctl login --token-file`"로 끝난다(종료 1). SSO 사용자는 웹으로 로그인해
  API 토큰 화면에서 토큰을 만들고 파일에 넣은 뒤 `oplctl login --token-file`로 저장한다:

  ```bash
  oplctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:... --token-file /root/opl-token
  # logged in to https://10.0.0.11:8006 as alice@example.com@okta
  ```

  `--token-file <파일|->`(`-`면 표준 입력)는 `--user`·`--realm`·`--password-file`과 함께 줄 수 없다. 서버 CA는 비밀번호 로그인처럼
  `--ca-fingerprint`로 고정하고(같은 서버에 다시 로그인하면 저장한 CA), 그 토큰으로 `GET /api/v1/auth/whoami`를 불러
  확인한 뒤(받지 않으면 저장하지 않음) 로그인 정보 파일에 넣는다. 다시 로그인할 때 이전 토큰은 새 토큰과 다르고 이 CLI의
  비밀번호 로그인이 만든 토큰(이름 `oplctl-…`)일 때만 서버에서 지운다 — `--token-file`로 넣은 웹 토큰은 두므로 필요 없으면
  웹이나 `oplctl token delete`로 지운다. `oplctl logout`도 같다: login이 만든 토큰(`oplctl-…`)만 서버에서 지우고, 웹에서
  만든 토큰이면 로컬 설정에서만 지운 뒤 "the token stays valid on the server; delete it in the web UI"라고 알린다.

한계:

- 로그아웃은 opLite 세션만 끝낸다(IdP 로그아웃·RP-initiated·back-channel 로그아웃 없음). IdP에 아직 로그인되어 있으면 다시
  누르면 바로 들어온다.
- IdP는 로그인할 때만 본다. IdP에서 계정을 막거나 그룹을 빼도 이미 있는 세션(최대 12시간)과 API 토큰은 남고 역할은 다음
  로그인에 바뀐다. 바로 막으려면 opLite에서 그 사용자를 지운다.
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 주소 수만큼 등록해야 한다. 리버스 프록시 뒤면 프록시가 넘기는 `Host`의 주소가
  리디렉트 URI가 되고 scheme은 늘 `https`다.
- 외부 사용자는 처음 로그인한 IdP 사용자(`sub`)에 묶인다: IdP에서 계정을 지우고 같은 이름으로 다시 만들면(새 `sub`) 로그인이
  `sso_failed`다 — 같은 사람이면 opLite에서 그 사용자를 지운 뒤(토큰도 지워짐) 다시 로그인한다. 영역의 issuer를 바꾸면
  모든 사용자가 같은 이유로 거절된다. Entra ID는 `sub`가 앱 등록마다 다르므로 앱 등록을 바꾸면(새 `client_id`) `sub`가
  모두 바뀌어 기존 사용자가 모두 `sso_failed`다 — 그 사용자들을 지워야 한다. `subject`가 없는 옛 사용자는 그 이름으로 처음 OIDC 로그인한 사람에게 묶인다.
- discovery·JWKS 받기가 실패하면 IdP가 돌아와도 15초 동안은 같은 오류다.
- discovery·JWKS는 노드마다 10분 둔다: IdP에서 지운 서명 키도 그동안은 그 노드에서 믿는다. 영역을 고치면 바로 새로 받는다.
- 이름 규칙(1~128자, 공백·한글 등 없음)에 맞지 않는 이름 클레임은 쓸 수 없다. 그룹은 클레임 값만 보고(IdP의 그룹 API를
  부르지 않음) Entra ID의 그룹 초과(groups 대신 `_claim_names`)는 그룹 없음이다.
- 클라이언트 인증은 `client_secret_basic`과 공개 클라이언트뿐이다(`client_secret_post`·`private_key_jwt`·mTLS 없음).
  `response_mode=form_post`, 요청 객체·PAR, 서명 알고리즘 RS256·ES256 밖(PS256·EdDSA 등)과 암호화한 ID 토큰은 없다.
- 클라이언트 비밀은 Raft에 평문으로 있다(서비스 계정 비밀번호와 같음). oplctl은 브라우저 SSO를 하지 않는다(웹에서 만든 토큰을
  `oplctl login --token-file`로).

## 감사 로그

노드마다 그 노드가 받은 바꾸는 API 요청(`POST`·`PUT`·`PATCH`·`DELETE`, 성공·실패와 401·403 포함), 로그인 성공·실패,
로그아웃, VM 콘솔 열기(`GET /api/v1/vms/{key}/console`), OIDC 로그인 콜백(`GET /api/v1/auth/oidc/callback`)을 한 줄 JSON으로 적는다. 다른 `GET`(감사 로그 읽기도),
웹 정적 파일, 노드 사이 RPC, 제어 소켓(`cluster init`·`join`·`leave`), 맞는 라우트가 없는 요청은 적지 않는다.
요청 본문은 적지 않는다.

```bash
oplctl audit                                     # admin. 모든 노드의 지난 24시간, 새 것부터 200개
oplctl audit --since 1h --user alice             # --since: 90s·30m·1h·7d(지금부터 거슬러) 또는 RFC 3339
oplctl audit --since 2026-11-06T00:00:00Z --until 2026-11-07T00:00:00Z --node node1 --limit 1000
oplctl audit --action web-1                      # 동작이나 경로에 든 문자열(VM 동작은 경로로 가른다)
# TIME                  NODE   USER   IP        ACTION                            TARGET     STATUS
# 2026-11-06T09:30:05Z  node1  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202
# 2026-11-06T09:29:58Z  node2  -      10.0.0.9  POST /auth/login                  username=bob  401
oplctl -o json audit --since 7d                  # 응답 그대로(항목의 모든 필드)
```

- **항목.** `{"id","time","node","user","user_id","role","auth","ip","method","path","action","target","status",
  "duration_ms","detail"}`. `id`는 UUID v7, `time`은 유닉스 밀리초, `node`는 적은 노드의 이름이다. `user`·`user_id`·
  `role`은 인증 전이거나 실패하면 `null`, `auth`는 `session`·`token`·`none`(`Authorization: Bearer`가 있으면 인증에
  실패해도 `token`)이다. `ip`는 시도 제한과 같은 클라이언트 IP(TLS 연결의 주소, "로그인과 세션"), `path`는
  `/api/v1` 아래의 실제 경로이고 쿼리는 넣지 않는다(쿼리에 비밀이 올 수 있다). `action`은 `"<메서드> <라우트
  템플릿>"`(예: `POST /vms/{key}/actions/{action}`, `DELETE /users/{key}`, `POST /auth/login`)이라 새 API도 저절로
  이름을 가진다. `target`은 템플릿의 첫 조각과 첫 매개변수 값(`{"kind":"vms","key":"web-1"}`)이고, 매개변수가 없는
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다, 영역 로그인은 뒤에 ` realm=<영역>`), OIDC 콜백은 `realm=<영역> username=<이름>` 또는 `realm=<영역> error=<코드>`, 원격 클러스터 중계는 `remote=<이름> <메서드> /<경로>`, 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
  `detail`·`user`는 512바이트까지만 적는다(넘으면 글자 경계에서 잘라 끝에 `…`).
- **파일.** `{data_dir}/audit/audit.log`(패키지는 `/var/lib/opl/audit/audit.log`). 디렉터리는 0700, 파일은 0600이다.
  요청은 디스크를 기다리지 않는다: 쓰기 스레드가 덧붙이고 1초마다 묶어 `fsync`한다. 줄을 더하면 `max_file_mib`를
  넘을 때 `audit.log` → `audit.log.1` → `.2` …로 밀고, 지금 파일을 포함해 `max_files`개를 넘는 가장 오래된 것을 지운다.
  쓰기가 밀려 대기열(1만 개)이 차면 항목을 버리고, 다음 항목 앞에 `"action":"audit.dropped"`, `"detail":"<N> entries
  dropped"` 항목을 적는다. 인증 없는 항목(인증 실패·로그인 실패·익명, `user`가 `null`)은 IP마다 1분에 60개까지만
  적고(IPv6는 /64 네트워크 하나로 센다), 넘친 것은 그 1분 창이 끝나고 1초 안에(또는 opld를 멈출 때)
  `"action":"audit.suppressed"`, `"ip"`(IPv6면 `<네트워크>/64`), `"detail":"<N> unauthenticated requests
  not recorded since <창 시작 RFC 3339>"` 항목으로 수만 남긴다(IP는 1만 개까지 세고, 넘치면 가장 오래된 것의
  수를 바로 적고 버린다). 인증된 요청은 한도가 없다. opld가 열 때 디렉터리는 0700, 파일은 0600으로 다시 맞추고,
  `audit.log`가 심볼릭 링크면 열지 않으며(opld가 시작하지 않음), `max_files`를 줄였으면 넘는 옛 파일을 지운다.
- **journald.** 같은 JSON을 tracing 대상 `opl_audit`(INFO)로도 내보낸다. 패키지의 opld는 systemd 서비스라 journald에
  `… INFO opl_audit: {"id":…}` 줄로 남는다(`journalctl -u opld -g opl_audit`). `RUST_LOG`를 바꿀 때는 `opl_audit=info`가
  남게 한다(파일은 `RUST_LOG`와 상관없다). opld는 로그가 터미널로 갈 때만 색 코드를 넣으므로 journald 줄에는 색 코드가
  없다(`NO_COLOR`를 따로 줄 필요가 없다). 밖으로 보내려면 rsyslog로 전달한다. Debian 13은 rsyslog가 기본으로 없다
  (`apt install rsyslog`):

  ```
  # /etc/rsyslog.d/60-opl-audit.conf
  # rsyslog가 이미 journald에서 받으면(/var/log/syslog에 opld 줄이 보이면) 다음 module 줄은 뺀다.
  module(load="imjournal" StateFile="imjournal.state")
  if $programname == 'opld' and $msg contains 'opl_audit:' then {
      action(type="omfwd" target="siem.example.com" port="514" protocol="tcp")
  }
  ```

  `systemctl restart rsyslog` 뒤 바꾸는 요청 하나를 보내 수집 서버에 줄이 오는지 본다.
- **읽기.** `GET /api/v1/audit?since&until&user&node&action&limit`(admin만, 다른 역할은 **403**)는 받은 노드가
  위트니스가 아닌 모든 멤버의 감사 로그를 읽어(자기 파일은 직접, 다른 노드는 노드 사이 RPC로 노드마다 5초 제한)
  시간 내림차순으로 합친다. `since`·`until`은 유닉스 밀리초나 RFC 3339(기본: 지금까지 24시간, 양 끝 포함), `user`·
  `node`는 정확히 같은 이름, `action`은 `action`이나 `path`에 든 문자열이다. `limit`은 1~5000(기본 200), 아니거나 시각이
  틀리면 **400** `invalid_request`다. 응답은 `{"entries":[…],"unreachable":["node3"],"disabled":["node4"],"truncated":false}`:
  읽지 못한 노드(꺼짐, 제한 시간, 이 기능이 없는 옛 opld)는 `unreachable`에, 감사 로그를 끈(`[audit] enabled = false`)
  노드는 `disabled`에 이름 순으로, 조건에 맞는 항목이 `limit`보다 많으면 `truncated: true`다. 같은 항목은 한 번만 온다
  (읽는 중에 회전해도). oplctl은 표 뒤 stderr에 `warning: no audit records from: node3`, `note: the audit log is disabled
  on: node4`, `note: more entries match; narrow the range or raise --limit`을 쓴다(`-o json`이면 쓰지 않는다). 표의
  `TARGET`은 대상이 없으면 `detail`(로그인의 `username=…`, `audit.dropped`의 수)이다. 기간 `--since`(`1h` 등)와 `--until`은
  함께 줄 수 없다(`a relative --since cannot be combined with --until; give a time` — `--since`에 시각을 준다).
- **웹.** "감사 로그" 메뉴(admin에게만 보인다): 시간 범위(지난 1시간·24시간·7일, 기본 24시간), 사용자·동작 입력(적용을
  눌러야 거른다), 표(시각은 브라우저의 지역 시간, 대상이 없으면 `detail`, 4xx·5xx 상태는 경고 색), 읽지 못한 노드 경고,
  감사 로그를 끈 노드 안내와 잘림 안내, 행을 누르면 항목 JSON. 자동으로 다시 읽지 않는다(새로고침 버튼). 다시 읽기가
  실패하면 마지막 표를 두고 위에 오류를 보인다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[audit]
enabled = true       # 끄면 이 노드는 파일도 journald 줄도 남기지 않는다
max_file_mib = 16    # 한 파일의 최대 크기(MiB, 1~256 — 읽을 때 파일을 통째로 메모리에 올린다)
max_files = 8        # 지금 파일을 포함해 남기는 파일 수(1 이상). 0이면 opld가 시작하지 않는다
```

감사 디렉터리를 만들거나 파일을 열지 못하면 opld가 시작하지 않는다. opld를 멈출 때는 API를 닫은 뒤 남은 항목을 쓰고
`fsync`한다.

- **한계.** 꺼진 노드의 기록은 그 노드가 돌아와야 보이고, 디스크를 잃으면 사라진다(journald 전달로 보완한다). 변조
  방지(해시 체인·서명)는 없어 노드의 root는 파일을 고칠 수 있다. 롤링 업그레이드 중 옛 opld 노드는 `unreachable`이다.
  opld가 갑자기 죽으면 대기열에 남은 항목을, 전원이 나가면 마지막 1초쯤을 잃을 수 있다. 버린 항목은 수만 남는다.
  파일 쓰기가 실패하면(디스크가 참 등) opld 로그에 경고만 남기고 요청은 그대로 처리한다. 리버스 프록시 뒤에서는
  `ip`가 프록시 주소다. 콘솔은 연 것만 적고 닫은 때와 입력은 적지 않는다. 알림과 날짜로 지우기는 없다(크기와 개수로만
  지운다). 위트니스 노드는 API가 없어 감사 로그도 없다. 인증 없는 요청은 IP(IPv6는 /64)마다 1분에 60개까지만 적고 나머지는
  창이 끝나고 1초 안에 적히는 `audit.suppressed`의 수만 남는다(같은 IP에서 많은 로그인 실패가 오면 그 시각·이름은
  다 남지 않는다). 각 문자열 필드는 512바이트까지만 적는다. 로그인의 사용자 이름 칸에 비밀번호를 잘못 넣으면 이름 형식(1~63자의 영문 소문자·숫자·`-`)이면
  `username=<그 값>`으로 그대로 적힌다. 감사 로그를 끈 노드는 `disabled`로 보일 뿐 그 노드의 요청은 어디에도 남지 않는다.
  `max_file_mib`는 256까지이고, 읽을 때 파일 하나를 통째로 메모리에 올린다(한 번 읽기에 최대 `max_file_mib`쯤).

## 원격 클러스터

다른 opLite 클러스터를 원격(`Remote`)으로 등록하면 이 클러스터에서 그 클러스터의 상태(노드·VM 수, 버전, CPU·메모리)를
보고, 그 클러스터의 API를 이 클러스터를 거쳐 부를 수 있다(VM 목록, 시작·정지 등). 두 클러스터는 대등하다: 어느 쪽도
다른 쪽의 멤버가 아니고 Raft를 함께 쓰지 않는다. 서로 보려면 양쪽에서 따로 등록한다.

- **권한은 두 번.** 원격은 이 클러스터의 리소스라 로그인한 누구나(viewer 이상) 보고, admin만 등록·고치기·지우기·새로
  모으기를 한다(operator도 **403**). 원격 클러스터 쪽에서 할 수 있는 일은 등록할 때 넣은 그 클러스터의 API 토큰 주인의
  역할이 다시 정한다. 원격 클러스터에서 viewer 사용자로 만든 토큰을 등록하면 이 클러스터의 admin도 그곳에서는 읽기만
  한다(바꾸는 요청은 원격의 **403**이 그대로 온다). API 토큰은 만든 사용자의 역할을 그대로 가지므로, 원격에 필요한 만큼의
  역할(VM을 시작·정지하려면 보통 operator — admin 토큰은 쓰지 않는다)로 사용자를 따로 만들고 그 사용자로 토큰을 만든다.
- **지문으로 믿기.** 원격과의 TLS는 원격 클러스터의 CA 하나만 믿는다(시스템 루트는 쓰지 않는다). 그 CA는 등록할 때
  원격의 `GET /api/v1/cluster/ca`에서 받아 준 지문(`sha256:…`)과 같고 정확히 인증서 하나인지 확인한 뒤 원격과 함께
  저장한다(덧붙인 다른 CA를 믿게 되지 않게 둘 이상이면 받지 않는다). 그 뒤로 노드는 연결할 때 CA를 다시 받지 않고 저장한
  것만 믿는다(재시작한 노드도). 지문은 원격 클러스터 노드의 `oplctl cluster status`(`CA fingerprint:` 줄)나 웹 클러스터
  화면, `cluster init` 출력에서 확인한다. 호스트 이름은 확인하지 않는다(노드 인증서의 이름은 노드마다 다르므로 그 CA가
  서명했는지와 기간만 본다). 원격 클러스터의 CA가 바뀌면(클러스터를 새로 만듦) `oplctl remote set <이름> --ca-fingerprint
  <새 지문> --token-file <파일>`로 고칠 때까지 닿지 않는다.
- **등록할 때의 확인**(이 순서, 하나라도 실패하면 저장하지 않는다): 입력 검사 **400**(`url`은 `https://<호스트>[:포트]` —
  경로·쿼리·조각·사용자 정보가 없고 끝 `/`는 뗀다, 지문은 대소문자와 `AB:CD:…` 꼴도 받아 `sha256:<소문자 64자>`로, 토큰은
  `opl_<id>_<비밀>` 모양, 이름은 리소스 이름 규칙이고 `probe`는 쓸 수 없다) → 롤링 업그레이드 중이면 **409**
  `cluster_upgrading` → 원격의 CA를 받지 못하면 **502** `remote_unreachable` → 지문이 다르면 **400**
  `remote_fingerprint_mismatch`(메시지에 받은 지문) → 받은 CA가 이 클러스터의 CA면 **400** `remote_is_local` → 그 CA로
  연결해 토큰으로 원격의 `GET /api/v1/auth/whoami`를 읽어 **401**이면 **400** `remote_unauthorized`(그 밖의 실패는 **502**
  `remote_unreachable`) → 저장하고 곧 상태를 한 번 모아(10초 제한) 응답에 넣는다. CA 받기부터 whoami까지 모두 30초 안에
  끝나야 한다(넘으면 **502** `remote_unreachable` "timed out checking the remote cluster" — `probe`의 CA 받기도 같다).
  검증 없이 CA를 받는 것(등록·`probe`)은 아무 주소에나 닿으므로 오류에 원격이 준 말을 옮기지 않는다: 2xx가 아니면 HTTP
  상태 코드만(`HTTP 404: Not Found`), 연결 실패는 `cannot connect`, TLS 실패는 `TLS handshake failed`, 2xx지만 PEM
  인증서가 아니면 `not an opLite cluster`.
- **토큰은 나가지 않는다.** 응답에는 토큰 대신 토큰의 id(`token_id` — 원격의 `oplctl token list`·API 토큰 화면에서 어느
  토큰인지 알아보게)만 있고, 원격은 watch로도 내보내지 않는다(웹은 10초마다 다시 읽는다). 바꾸기(`PUT`)에서 `token`을 빼면
  `url`·`ca_fingerprint`(정규화한 값)가 모두 그대로일 때만 저장된 토큰을 두고, 하나라도 바뀌면 **400** "token must be given
  again when url or ca_fingerprint changes"다(저장된 토큰을 다른 곳으로 보내지 않는다). `url`이나 `ca_fingerprint`가 바뀌면
  등록 때와 같은 확인(CA를 다시 받아 저장)을 하고, 토큰만 바뀌면 저장한 CA로 토큰만 확인한다. 이름은 바꿀 수 없다.
  `If-Match`가 다르면 **412**지만, 확인하는 사이에 상태만 바뀌었으면(상태 수집이 revision을 올림) 다시 확인하지 않고 새
  revision에 쓴다 — **412**는 그 사이 spec(주소·지문·토큰)이 바뀐 때만이다. 원격을 지워도 원격 클러스터의 토큰은 남으므로
  그곳에서 따로 지운다.
- **노드에서 원격으로.** 원격에 연결하는 것은 브라우저가 아니라 이 클러스터의 노드다(상태는 리더가, 중계는 요청을 받은
  노드가). 모든 노드가 원격 URL에 닿아야 한다. 연결은 요청마다 새로 열고(HTTP/1.1), 연결·TLS 10초, 요청을 다 보낸 뒤
  응답 헤더 10초, 본문 조각 사이 60초 제한이며 리다이렉트는 따르지 않는다. CA는 등록 때 저장한 것을 쓴다.

```bash
# 원격 클러스터(dr)에서: 이 클러스터가 쓸 사용자(읽기만 시키려면 --role viewer)
oplctl user create prod-link --role operator
# 그 사용자로 로그인해 API 토큰을 만든다(웹이면 그 사용자로 로그인해 API 토큰 화면에서). 토큰은 다시 볼 수 없다.
oplctl login --server https://10.0.1.11:8006 --ca-fingerprint sha256:… --user prod-link
oplctl token create prod > /root/dr-token      # 이 파일을 prod 쪽으로 옮긴다
oplctl cluster status                          # CA fingerprint: sha256:… — 아래에서 보이는 지문과 비교한다

# 이 클러스터(prod)에서(admin)
oplctl remote add dr --url https://10.0.1.11:8006 --token-file /root/dr-token
# the remote cluster CA fingerprint is sha256:3f9a…
# Is this the CA of the remote cluster? [y/N] y
# remote dr added (reachable)
oplctl remote add dr --url https://10.0.1.11:8006 --ca-fingerprint sha256:3f9a… --token-file -   # 지문을 미리 알면
oplctl remote list
# NAME  CLUSTER  VERSION  NODES  VMS    STATUS
# dr    dr       0.37.0   3/3    12/15  reachable
oplctl remote show dr       # url·ca fingerprint·token id·status·changed at·cluster·version·nodes·vms·cpus·memory
oplctl remote refresh dr    # 곧 다시 모아 show처럼 보인다(admin)
oplctl remote vms dr        # 원격의 VM 목록(vm list와 같은 표)
oplctl remote vm dr start web          # 시작
oplctl remote vm dr stop web           # ACPI 종료(로컬 vm stop과 같음)
oplctl remote vm dr stop web --force   # 곧바로 끄기(로컬 vm stop --force와 같음)
oplctl remote set dr --url https://10.0.1.12:8006 --token-file /root/dr-token   # 주소·지문을 바꾸면 토큰도 다시
oplctl remote set dr --token-file /root/new-token    # 새 토큰만(주지 않은 것은 그대로, 읽은 revision으로 — 그 사이 다른 admin이 바꿨으면 412)
oplctl remote remove dr
```

- **oplctl.** `remote add`에 `--ca-fingerprint`가 없으면 oplctl이 직접(인증서를 검증하지 않고) 원격의 CA를 받아 지문을 보이고,
  `--yes`면 그대로 믿고, 터미널이면 `[y/N]`로 묻고(`y`·`yes`만 계속), 터미널이 아니면(스크립트) "check the fingerprint
  (`oplctl cluster status` on a node of the remote cluster) and run again with --ca-fingerprint sha256:…"로 끝난다(종료 1).
  `--token-file -`이면 표준 입력에서 읽고 앞뒤 공백을 뗀다 — 그때는 표준 입력으로 지문 확인을 물을 수 없으므로
  `--ca-fingerprint`나 `--yes`가 없으면 토큰을 읽거나 원격에 연결하기 전에 "reading the token from stdin needs
  --ca-fingerprint or --yes"로 끝난다. `remote vm`은 원격의 Task를 기다리지 않고 "vm web on remote dr: stop requested (task
  <id>)"(`--force`면 `force stop requested`)만 보인다(결과는 `remote vms`로). `-o json`이면 API 응답 그대로다.
- **상태 수집.** 리더가 5초마다(`tick`) 모을 때가 된 원격을 찾아, 원격마다 30초마다(`interval`) 원격의 `GET /cluster`,
  `/nodes`, `/vms`를 읽는다(원격들을 동시에, 각각 10초 제한 — 그래서 원격 토큰은 적어도 viewer여야 한다). 상태는
  `cluster_name`, `version`(위트니스를 포함한 노드 버전 가운데 가장 낮은 것), `nodes{total, online}`·`cpus`·`memory_gib`
  (위트니스를 뺀 노드), `vms{total, running}`이다. 읽지 못하면 `reachable: false`와 `error`(이유)만 남고 나머지는 비운다.
  모은 것이 저장된 것과 같으면 쓰지 않는다(Raft 부하) — 그래서 `checked_at`은 마지막 확인 시각이 아니라 상태가 마지막으로
  **바뀐** 시각이다(oplctl `changed at`). 리더가 바뀌면 새 리더가 곧 모은다. `oplctl remote refresh`(웹 "새로고침", API
  `POST /remotes/{key}/actions/refresh`)는 기다리지 않고 지금 모은다(10초 제한). 모은 상태를 적지 못하면(그 사이 수집기가
  먼저 적음) 모은 것 대신 저장된 원격을 돌려준다(등록·바꾸기의 응답도 같다).
- **중계.** `/api/v1/remotes/{key}/api/{*path}`(모든 메서드)는 그 원격의 `/api/v1/{path}`로 메서드·쿼리·본문을 저장한
  토큰(`Authorization: Bearer`)으로 보내고, 상태와 본문을 그대로 흘려 돌려준다(본문 한도 없음 — 업로드·내려받기도 된다).
  - 권한: 아래 허용 목록의 경로를 `GET`·`HEAD`로 읽는 것은 원격을 읽을 수 있는 누구나(viewer 이상), 그 밖은 메서드와
    관계없이 이 클러스터의 admin이다. 그다음 원격이 토큰 주인의 역할로 다시 확인한다. 원격 토큰이 읽을 수 있더라도 사용자·
    토큰·인증 영역·감사 로그·텔레메트리처럼 계정과 보안에 관한 것과 원격의 원격(`remotes` — 중계 사슬)은 이 클러스터의
    viewer·operator가 읽지 못하게 한다. 첫 조각은 퍼센트 인코딩을 풀어 본다(`%75sers`도 `users`).

    | 경로(`{*path}`) | `GET`·`HEAD` | 그 밖의 메서드 |
    |---|---|---|
    | 첫 조각이 `vms`·`volumes`·`storage-pools`·`networks`·`security-groups`·`snapshots`·`backups`·`backup-jobs`·`vm-imports`·`nodes`·`tasks`·`cluster`·`version`·`settings`·`subscription`, 또는 정확히 `auth/whoami` | viewer 이상 | admin |
    | 그 밖(`users`·`tokens`·`realms`·`audit`·`telemetry`·`remotes`·다른 `auth/*`·모르는 조각) | admin | admin |

  - 보내는 헤더는 `Content-Type`·`Content-Length`·`If-Match`·`Accept`뿐이다(쿠키·`Origin`·원래 `Authorization` 등은 보내지
    않고 `Host`는 원격 주소). 돌려주는 헤더는 `Content-Type`·`Content-Length`·`ETag`·`Retry-After`·`Content-Disposition`뿐이고,
    원격이 보낸 HTML·SVG 따위가 이 클러스터 출처의 페이지로 실행되지 않게 `Content-Type`은 `application/json`(매개변수
    허용)일 때만 그대로, 그 밖(없음 포함)은 `application/octet-stream`으로 하며 늘 `X-Content-Type-Options: nosniff`와
    `Content-Security-Policy: default-src 'none'; sandbox`를 붙인다.
  - 경로 조각에 `.`·`..`·빈 조각이 있으면(퍼센트 인코딩 `%2e` 등도) **400** `invalid_request`, `watch`로 시작하는 경로와 VM
    콘솔(`vms/<VM>/console`, WebSocket)은 **404** `remote_path_unsupported`, 원격에 닿지 못하면(연결·TLS·시간 제한) **502**
    `remote_unreachable`이다. 원격이 저장된 토큰을 거절하면(**401** — 원격에서 토큰을 지움 등) 본문을 버리고 **502**
    `remote_unauthorized` "the remote cluster refused the stored token; update it with `oplctl remote set --token-file`"다
    (이 클러스터의 로그인 문제로 보이지 않게 — oplctl은 이 메시지를 그대로 보이고 웹은 로그아웃하지 않는다). 원격의 그 밖의
    오류 응답(**403** 포함 **4xx**·**5xx**)은 그대로 온다.
  - 감사: 중계한 바꾸는 요청은 이 클러스터의 감사 로그에 `action` `"<METHOD> /remotes/{key}/api/{*path}"`, `target`
    `remotes/<키>`, `detail` `remote=<이름> <METHOD> /<경로>`로 남고, 원격 클러스터의 감사 로그에는 토큰 주인의 요청으로 남는다.
  - 중계는 API 문서(OpenAPI)에 넣지 않는다(원격 API가 그대로 보이므로). 원격 쪽 API는 원격의 `openapi.json`을 본다.
- **웹.** "원격 클러스터" 메뉴(모두에게): 원격마다 카드(이름, 연결됨·연결 안 됨, URL, 클러스터 이름, 버전, 노드
  online/total, VM running/total, CPU, 메모리, 닿지 않으면 오류 문구), 10초마다 다시 읽는다. admin은 추가(이름, URL을 넣고
  "CA 받기" — 브라우저는 다른 출처를 읽지 못하므로 노드가 받은 지문을 보인다, 지문 확인 체크, API 토큰), 고치기(토큰을
  비우면 그대로, URL·CA가 바뀌면 다시 넣으라고 알림), 지우기, 새로고침을 한다. 카드의 이름을 누르면 그 원격의 VM 목록
  (중계 `GET /vms`, 10초마다)이고, admin은 VM마다 시작·정지(ACPI)·강제 정지(확인 뒤)를 요청한다(oplctl `remote vm
  start|stop [--force]`와 같은 이름, 원격 쪽 Task는 지켜보지 않고 요청했다고만 알린다). 중계의 **401**은 이 클러스터의
  세션이 끝난 것이므로 다른 화면처럼 로그인으로 돌아간다.
- **API.** `GET`·`POST /api/v1/remotes`, `GET`·`PUT`(`If-Match` 선택)·`DELETE /api/v1/remotes/{key}`(본문
  `{"name","url","ca_fingerprint","token"}` — `PUT`의 `name`은 생략하거나 지금 이름), `POST /api/v1/remotes/probe {"url"}`
  (admin — 인증서를 검증하지 않고 CA를 받아 정규화한 `url`과 `ca_fingerprint`를 돌려준다, 받지 못하거나 30초를 넘으면
  **502** `remote_unreachable`),
  `POST /api/v1/remotes/{key}/actions/refresh`(admin), 그리고 위의 중계. 응답은 `{"id","name","revision","url",
  "ca_fingerprint","token_id","status":{"reachable","checked_at","error","cluster_name","version","nodes":{"total","online"},
  "vms":{"total","running"},"cpus","memory_gib"}}`이다.

설정(노드마다, 생략하면 아래 기본값 — 리더인 노드의 값을 쓴다):

```toml
[remotes]
interval = "30s"   # 원격마다 상태를 모으는 간격
tick = "5s"        # 모을 때가 된 원격을 찾는 간격
```

- **한계.**
  - 원격 토큰은 Raft에 평문으로 있다(구독 키·LDAP 서비스 계정 비밀번호·OIDC 클라이언트 비밀과 같음 — Raft 데이터는 노드의
    root만 읽는다). 위트니스 노드와 데이터 디렉터리 백업에도 그대로 있다. 이 클러스터의 노드 하나(위트니스 포함)나 그
    백업을 가진 사람은 원격 클러스터를 그 토큰의 역할로 다룰 수 있으므로, 원격에는 필요한 만큼의 역할(보통 operator)
    사용자의 토큰을 등록한다.
  - watch(실시간 갱신)와 VM 콘솔은 중계하지 않는다. 원격 VM은 10초마다 다시 읽어 보이고, 콘솔은 원격 클러스터에 직접 연다.
  - 원격 URL은 노드 하나의 주소다. 그 노드가 꺼지면 원격의 다른 노드가 살아 있어도 `reachable: false`이므로 주소를 바꾼다
    (`remote set --url` — 토큰도 다시).
  - 원격 클러스터의 CA가 바뀌면(클러스터를 새로 만듦) `remote set --ca-fingerprint <새 지문> --token-file`로 고칠 때까지
    닿지 않는다(노드는 저장한 CA만 믿는다).
  - `checked_at`은 상태가 바뀐 시각이라 "마지막 확인"을 알 수 없다(`reachable`·`error`만 믿는다). 원격 Task(시작·정지의
    결과)는 지켜보지 않는다. 원격 쪽 감사 기록은 토큰 주인으로 남아 이 클러스터의 누가 요청했는지는 이 클러스터의 감사
    로그에만 있다.
  - 웹의 고치기는 `If-Match` 없이 보낸다(상태 수집이 revision을 올리므로) — 두 admin이 동시에 고치면 나중 것이 남는다.
  - 중계는 원격의 **401**을 모두 **502** `remote_unauthorized`로 바꾸므로, 토큰 거절이 아닌 401(중계한 `auth/login`에 틀린
    비밀번호 따위)도 그렇게 보인다. `auth/login`은 허용 목록 밖이라 이 클러스터의 admin만 닿으므로 받아들인 한계다.

## 원격 복제(DR)

백업을 등록한 원격 클러스터(위 "원격 클러스터")의 백업 풀로 복사해 두면, 주 사이트(클러스터 prod)를 통째로 잃어도
DR 사이트(클러스터 dr)에서 그 사본을 복원해 VM을 되살린다. 사본은 dr의 **보통 백업**이다: dr의 `oplctl backup
list`·웹 백업 화면에 보이고, dr에서 복원하고 지운다. 백업 작업이 만든 백업은 저절로(`replicate`), 아무 백업이나 손으로
(`backup replicate`) 보낸다. prod에서 원본을 지우면(사용자나 보존 규칙) dr의 사본도 지운다.

- **보내는 길.** 복제는 prod의 Task(`backup-replicate`)가 한다. 백업 파일이 있는 노드(공유가 아닌 풀은 백업을 쓴
  노드, NFS면 그 풀을 쓸 수 있는 노드 — 지우기와 같다)가 원격 등록의 URL·CA·토큰으로 dr의 받기 API를 부른다:
  `POST /backups/receive`(매니페스트와 출처)로 dr에 `receiving` 백업을 만들고, 디스크 파일(`disk0.qcow2`…)과
  `vars.fd`(UEFI)를 파일마다 읽으면서 SHA-256을 계산해 `PUT /backups/{id}/files/{file}`로 흘려 보내고(본문을
  메모리에 모으지 않는다), dr이 받은 내용의 SHA-256과 크기를 돌려주면 보낸 것과 비교해 다르면 실패한다. 다 보내면
  `POST /backups/{id}/actions/complete`로 dr이 `manifest.json`을 써 사본이 `ready`가 된다. 원본이 `ready`여야
  보낸다(**409** `backup_not_ready`). Task에 시간 제한은 없고(디스크 크기만큼 걸린다), 원격 연결의 본문 조각 사이
  60초 제한만 있다. dr은 받는 동안 128 MiB마다 디스크에 내려 쓰고 끝에 한 번 더 내려 쓴 뒤 답하므로, prod는 파일을 다
  보낸 뒤 그 응답을 120초까지 기다린다(다른 요청은 10초). dr은 파일마다 받은 SHA-256을 사본의 디스크
  (`status.disks[].sha256`)에, 마지막으로 받은 시각을 `status.received_at`에 적고, prod는 맞게 보낸 파일의 SHA-256을
  원본의 디스크(`status.disks[].sha256`)에 적는다. dr은 본문 조각이 60초 넘게 오지 않으면 그 파일을 버린다(멈춘
  업로드 — 복제 Task는 실패하고 다시 보낼 수 있다). 진행률은 없다(증분 백업은 체인의 몇 백업을 보냈는지만 — 아래
  "증분 백업 복제").
- **dr 쪽 준비.**
  - **백업 풀.** 사본을 둘 dr의 백업 풀(`backups`를 켠 dir·NFS 풀). 백업 풀이 아니거나 없으면 dr이 **400**이고,
    남은 공간이 원본 디스크 파일 크기의 합보다 작으면 **507** `insufficient_storage`다.
  - **operator 토큰.** prod에 dr을 등록할 때 쓴 토큰의 주인이 dr에서 operator 이상이어야 한다(받기와 원격 사본 지우기가
    백업 쓰기). viewer 토큰이면 dr의 **403**이 복제 Task의 오류와 사본 기록에 남는다. admin 토큰은 쓰지 않는다. 이
    토큰은 dr의 백업을 지울 수 있다(아래 "한계").
  - **같은 버전.** 두 클러스터는 같은 opLite 버전이어야 한다. prod가 보내는 매니페스트·출처에 dr이 모르는 필드가
    있으면 dr이 **400**으로 거절한다(업그레이드는 dr부터).
  - **파일을 받는 노드.** dr에서 파일을 쓰는 것은 받기 요청을 받은 노드, 곧 prod에 등록한 원격 URL의 노드다. 그래서
    백업 풀이 NFS면 아무 노드나 되지만, 공유가 아닌 디렉터리 풀이면 그 풀이 있는 노드의 주소로 원격을 등록해야
    한다(아니면 **409** `pool_not_on_node` — "register this cluster as a remote with the URL of a node holding the
    pool, or use a shared (NFS) backup pool"). 그 노드를 잃으면 사본도 잃으므로 DR에는 NFS 백업 풀이 낫다. 받은 사본은
    dir 풀이면 그 노드의 것이라(`status.node`) 그 노드에서 복원한다.
  - **복원할 곳.** VM 디스크를 둘 풀, 그리고 prod VM의 NIC가 쓰는 **네트워크와 보안 그룹을 같은 이름으로** 미리
    만들어 둔다. 복원은 백업 때의 VM 스펙을 그대로 쓰므로 없으면 **400**이다(복원에서 바꿀 수 없다).
- **사본의 이름과 출처.** 사본의 이름은 원본 이름이고 dr에 이미 있으면 `-2`, `-3`…을 붙인다. `spec.origin`에 보낸
  클러스터 이름(`cluster`), 그 CA 지문(`ca_fingerprint`), 원본 id(`backup`), 원본을 만든 백업 작업 이름(`job`, 손으로
  만든 백업이면 없음)이 있다. dr의 `oplctl backup list`는 `ORIGIN` 열에 `<보낸 클러스터>/<원본 id 앞 8자>`, 웹은 이름
  옆에 "<클러스터>에서 받음" 배지(툴팁과 화면 읽기에 원본 id·작업)를 보인다. 사본에는 `spec.job`이 없으므로 dr의 백업
  작업 보존 규칙은 사본을 지우지 않는다(사본은 prod의 보존 규칙이 원본을 지울 때 함께 지워진다). 같은 원본(CA 지문과
  원본 id)의 사본은 dr의 백업 풀마다 하나뿐이다(같은 원본을 dr의 다른 풀로 보내면 다른 사본이다).
- **사본 상태.** prod의 원본 `status.replicas`에 (원격, 풀)마다 `{remote, pool, backup(dr의 사본 id), state, error, at}`이
  있다. `state`는 `copying`(보내는 중) → `ready` 또는 `failed`(`error`에 이유)다. `oplctl backup list`의 `REPLICAS` 열은
  `<원격>:<상태>`(쉼표로), 웹 백업 표의 "원격 사본" 열은 사본마다 상태 아이콘과 원격 이름(툴팁에 원격/풀·상태·오류,
  화면 읽기에 원격/풀·상태)이고 실패한 사본은 그 아래에 "원격 / 풀: 오류"를 글로 보인다. `oplctl backup get`(JSON)에서
  전체를 본다. 이미 `ready`인 사본을 다시 확인하는 동안(아래 "다시 하기") 기록은 `ready` 그대로다: 확인이 성공하면
  `at`만 고치고, dr에 닿지 못하면 `ready`로 둔 채 `error`에 이유를 적는다(복제 Task는 실패, 다음 실행이 다시
  확인한다). dr에 없으면(404) `copying`으로 바꿔 다시 보내고, 다른 사본이거나 해시가 다르면 `failed`다.
- **다시 하기.** 같은 백업을 같은 원격·풀로 다시 보내면 dr의 사본으로 이어 간다. 복제 Task는 먼저 사본 기록의 id를 dr에
  묻고(`GET /backups/{id}` — **404**면 dr에서 지운 것이므로 새로 받기부터 한다), 기록이 없으면 받기를 시작해 dr이
  **409** `replica_exists`로 알려 주는 사본을 읽는다. 그 사본이 이 백업의 사본인지 — 출처의 원본 id, 요청한 풀, 부모
  사본(증분이면 dr의 부모 사본 id, 전체면 없음), VM 스펙, 디스크 이름과 크기, UEFI 변수 — 확인하고, 다르면 "a different
  backup claims to be the copy of <원본 id> on remote …; delete it there"로 실패한다(dr에서 그 백업을 지운다). 사본이
  `ready`면 원본에 적힌 디스크 해시(보낼 때 적은 것 — 해시가 없는 옛 백업만 원본 파일을 읽어 계산해 적는다)와 dr이 받은
  해시를 비교해 모두 같을 때만 보내지 않고 성공하고, 해시가 없거나 다르면 "… is ready but its files do not match …;
  delete it there and replicate again"으로 실패한다(ready 사본은 파일을 다시 받지 않는다). 아직 `receiving`이면 파일을
  모두 다시 보낸다(같은 파일·반쯤 받은 `.part`는 덮는다). 보내다 멈춘 사본(`receiving`)은 dr의 정리 작업이 만든 지,
  그리고 마지막으로 파일을 받은 지 24시간이 지나면 파일과 함께 지운다. 백업 작업은 실행마다 한 번 실패한 사본을 다시
  보내고 ready 사본도 dr에 있는지 확인한다(아래).

```bash
# dr에서: 백업 풀(dir 풀이면 prod에 등록한 URL의 노드에), 같은 이름의 네트워크·보안 그룹, operator 토큰(위 "원격 클러스터")
oplctl pool create dr-bk --nfs nas2.lan:/srv/dr-backup --backups

# prod에서: dr을 원격으로 등록한 뒤
oplctl backup replicate before-upgrade --remote dr --pool dr-bk          # Task id와 따라가는 방법을 보이고 끝난다
oplctl backup replicate before-upgrade --remote dr --pool dr-bk --wait   # 끝날 때까지(24시간까지) 기다린다
oplctl backup-job create nightly --pool bk --all --at 03:00 --keep-daily 7 --replicate dr/dr-bk
oplctl backup-job set db --replicate dr/dr-bk       # 있는 작업에 켜기(대상 바꾸기도 같다)
oplctl backup-job set db --no-replicate             # 끄기(이미 보낸 사본은 남는다)
oplctl backup list
# NAME                 VM   POOL  PHASE  CREATED               SIZE      TYPE  ORIGIN  REPLICAS
# web-20261107-030000  web  bk    ready  2026-11-07 03:00 UTC  412.5MiB  full  -       dr:ready
oplctl backup delete web-20261107-030000            # dr의 사본도 지운다

# dr에서
oplctl backup list
# NAME                 VM   POOL   PHASE  CREATED               SIZE      TYPE  ORIGIN         REPLICAS
# web-20261107-030000  web  dr-bk  ready  2026-11-07 03:00 UTC  412.5MiB  full  prod/019a5b1c  -
```

- **수동 복제.** `oplctl backup replicate <백업> --remote <원격> --pool <원격의 백업 풀> [--wait]`(API `POST
  /api/v1/backups/{key}/actions/replicate {"remote", "pool"}`, operator 이상, **202** Task). 원격이 등록되지 않았으면
  **404**, 원본이 `ready`가 아니면 **409** `backup_not_ready`, 지우는 중이거나 같은 전체 백업에서 시작한 체인의 어느
  백업이든(이 백업, 조상, 자식, 형제 — 전체 백업이면 그 증분들) 같은 원격으로 보내는 Task가 끝나지 않았으면 **409**
  `backup_busy`(복제 중인 그 백업의 이름), 파일이 있는 노드가 오프라인이면 **409**
  `node_offline`이다. 웹은 백업 표(백업 화면·VM 상세의 백업 탭)의 "원격으로 복제" 버튼(ready 백업, operator 이상):
  원격을 고르면 그 원격의 백업 풀을 중계(`GET /storage-pools`)로 읽어 고르게 하고, 보낸 뒤 Task를 따라간다. 복제 중인
  백업을 지우면 **409** `backup_busy`("being replicated")다(보내는 동안 파일을 지우지 않게).
- **작업 복제.** 백업 작업에 `replicate: {"remote", "pool"}`(`backup-job create|set --replicate <원격>/<풀>`, 웹 백업
  작업의 "원격 클러스터로 복제")을 주면, 리더의 스케줄러가 확인할 때마다(`[backup] tick_secs`) 그 작업이 만든 `ready`
  백업 가운데 그 (원격, 풀) 사본 기록이 없는 것을 만든 순서로 복제 Task로 보낸다 — 켜기 전에 만든 그 작업의 백업도
  보낸다. 기록이 마지막 실행을 시작하기 전의 것이면 상태와 관계없이 다시 보내므로, `failed`(또는 복제 Task 없이
  `copying`으로 남은) 사본은 실행마다 한 번 다시 보내고, `ready` 사본도 실행마다 한 번 복제 Task가 dr에 물어 같은
  파일이 있으면 기록 시각만 고치고 dr에서 지웠으면 다시 보낸다(dr에 묻고 적힌 해시를 비교할 뿐 원본 파일을 다시 읽지
  않는다). 복제 Task는
  노드마다 하나씩, 그 노드의 백업 Task와 합쳐 `[backup] max_concurrent`(손으로 만든 백업·복제도 센다)까지이고, 백업
  Task도 복제 Task를 센다. 꺼 둔 작업은 보내지 않는다. 그 작업의 보존 규칙이 지울 백업은 확인·복제하지 않고, 실행이
  끝날 때 지울 백업이 복제 중이라 지우지 못하면(`backup_busy`) 확인할 때마다 다시 해서 복제 Task가 끝나면 지운다.
  `PATCH /api/v1/backup-jobs/{name}`의 `replicate`는 객체면 바꾸고 `null`이면 끄고 없으면 그대로다
  (`--no-replicate`가 `null`). 등록되지 않은 원격은 **400**이고 원격의 풀은 보낼 때 dr이 확인한다(틀리면 Task 실패).
  `full_every`가 있는 작업의 증분 백업도 보낸다(아래 "증분 백업 복제" — 만든 순서로 보내므로 증분의 Task는 보통 앞
  백업이 이미 `ready`라 그 증분 파일만 보낸다).
- **증분 백업 복제.** 증분 백업 X를 보내면 복제 Task가 X의 체인(위 "백업과 복원"의 "증분 백업" — 전체 백업부터
  X까지)을 앞에서부터 차례로 본다. X가 아닌 백업은 그 (원격, 풀) 사본 기록이 `ready`이고 사본 id가 있으면 dr에 묻지
  않고 건너뛰고, 아니면 위와 같이 보낸다. 앞 백업이 끝나야 다음을 보내고, X는 늘 보낸다(이미 `ready`면 위 "다시
  하기"처럼 확인만). 그래서 체인을 처음 보낼 때는 전체 백업부터 모두, 그 뒤에는 새 증분 파일만 간다. 증분은 dr에 있는
  부모 사본의 id와 함께 보내고(받기 요청의 `parent`), dr의 사본은 그 부모 사본을 `status.parent`로 가리킨다 — dr에서도
  보통 증분 백업처럼 체인으로 복원하고(`TYPE`이 `incr`), 부모 사본은 자식 사본이 있는 동안 지울 수 없다(**409**
  `backup_in_use`). dr이 쓰는 매니페스트는 버전 2이고 `backup`·`parent`가 dr의 id다.
  - **원격 기능.** 체인에 증분이 있으면 보내기 전에 dr의 `GET /nodes`로 모든 노드(위트니스 포함)에 증분 복제 기능
    (`incremental-replication`)이 있는지 보고, 없으면(노드 목록이 비어도) Task가 `remote cluster "dr" does not support
    incremental backups; upgrade it`로 실패한다(dr부터 업그레이드). 계획 40 노드는 증분 백업을 알아도 증분 받기
    (`parent`가 있는 받기 요청)를 모르므로 업그레이드해야 한다. 전체 백업만 보낼 때는 보지 않는다.
  - **부모 사본이 없으면 다시 보낸다.** dr에서 부모 사본을 지웠거나 맞지 않으면(같은 풀에 `ready`가 아님, 다른 원본·CA
    지문, 공유가 아닌 풀의 다른 노드, 지우는 중) dr이 **409** `replica_parent_missing`으로 거절한다(받기 요청, 드물게
    완료 요청 — 받는 동안 없어짐). Task는 그 부모의 사본 기록을 `failed`로 바꾸고(사본 id는 둔다 — 다음에 dr에 물어 보고
    없으면 새로 받는다) 체인을 처음부터 한 번 더 본다. 이번에는 `ready`로 적힌 앞 백업도 건너뛰지 않고 위 "다시
    하기"처럼 dr에 물어 확인하므로(없으면 다시 보낸다) 조상의 사본까지 지웠어도 한 Task가 체인 끝까지 다시 보낸다. 한
    Task에서 한 번만 하고, 또 거절되면 실패한다.
  - **실패.** 체인의 백업이 없거나 `ready`가 아니면 "backup chain is broken: …", 앞 백업을 보내지 못하면 `cannot
    replicate backup "<이름>" of the chain: <이유>`로 실패하고 그 백업과 X의 사본 기록이 `failed`와 이유가 된다(기록이
    `ready`였으면 — 위 "다시 하기"의 확인 — `ready`로 두고 `error`만 적는다; 다시 복제하면 이미 `ready`인 앞 백업은
    건너뛴다). 체인 중간 백업이 마지막 `replica_parent_missing`으로 끝나도 그 기록은 `copying`으로 남지 않고 `failed`와
    이유다. opld가 다시 시작해 Task가 중단되면(`interrupted: …`) 정리가 X와 함께 체인에서 `copying`으로 적힌 앞 백업의
    기록도 `failed`와 그 이유로 바꾼다(`ready`·`failed` 기록은 그대로). 체인은 같은 풀의 같은 노드에 있으므로(공유가
    아닌 풀) Task는 X의 노드에서 돈다.
  - **진행.** Task에 진행 메시지는 없다(opld 로그에 체인 백업마다 한 줄). 대신 체인 백업들의 사본 기록으로 계산한
    상태를 보인다 — 체인의 어느 백업이든 그 (원격, 풀) 사본 기록이 있으면 복제 중이 아니어도 보인다: `oplctl backup
    get <증분>`의 `chain.replicas`에 (원격, 풀)마다 `{"remote", "pool", "progress": "k of n copied"}`(체인 n개 가운데
    원격에 있는 백업 k개 — 그 사본 기록이 `ready`이고 사본 id가 있는 것; 체인을 전체 백업까지 따라갈 수 있을 때만),
    웹 백업 표의 "원격 사본" 열에 증분이면 "<원격> / <풀>: 체인 n개 중 k개가 원격에 있음"(다 있으면 숨긴다).
- **지우기 전파.** prod에서 원본을 지우는 Task(`backup delete`, 백업 작업의 보존 규칙)는 원본 파일을 먼저 지우고(지우지
  못하면 Task가 실패하고 dr은 건드리지 않는다), 그다음 `replicas`의 사본 id가 있는 사본마다(`failed`도 — 받다 만 사본이
  dr에 있다) dr의 `DELETE /backups/{id}`를 부른다(dr이 지우기 Task를 시작한다 — 끝나기를 기다리지 않는다, **404**는 이미
  지운 것으로 성공). dr에 닿지 못하거나 거절하면 원본은 그대로 지우고, Task는 성공하되 `error`에 "backup deleted, but
  remote copies may be left: …" 경고를 남긴다(`oplctl backup delete`는 `warning: …` 줄로, 웹은 닫을 때까지 남는 경고
  알림으로 보인다). 그 사본은 dr에서 `ORIGIN`으로 찾아 지운다. 원본은 자식부터 지우므로 dr의 사본도 자식부터 지워진다.
  dr이 부모 사본 지우기를 **409** `backup_in_use`로 거절하면(자식 사본의 dr 지우기 Task가 아직 끝나지 않았다) 다시
  요청하며 60초까지 기다리고, 그래도 거절되면(자식 사본을 지우지 못했다) 같은 경고다 — dr에서 자식부터 지운다. `backup
  delete --force`(파일을 지울 노드가 없어 리소스만 지움)는 원격을 건드리지 않는다. dr에서 사본을 지우면 백업 작업이 다음
  실행에 다시 보낸다(위).
- **업그레이드 중.** 복제·받기·`replicate`가 있는 작업 만들기·고치기는 모든 노드가 이 버전이어야 하고(아니면 **409**
  `cluster_upgrading`), 스케줄러는 모두 올라올 때까지 보내지 않는다. 받는 dr도 이 버전이어야 한다(모르는 필드는
  **400**, 증분 백업은 위 "원격 기능").
- **받기 API(dr 쪽, 백업 쓰기 — operator 이상).** 보통은 prod의 복제 Task만 부른다.
  - `POST /api/v1/backups/receive {"name"?, "pool", "manifest", "origin", "parent"?}` → **201** `{"id", "name"}`.
    `parent`는 증분 백업일 때 이 클러스터에 있는 부모 사본의 id다(전체 백업이면 뺀다). 풀이 없거나 백업 풀이 아니거나
    매니페스트가 올바르지 않으면(형식 버전 — `parent`가 없으면 버전 1에 매니페스트 `parent` 없음, 있으면 버전 2에
    매니페스트 `parent` 있음 —, VM 이름·스펙, 디스크 수가 스펙과 같고 디스크 i가 `disk{i}`·`disk{i}.qcow2`) **400**, 이
    노드에서 풀을 쓸 수 없으면 **409** `pool_not_on_node`·`pool_unavailable`, 같은 원본의 사본이 같은 풀에 있으면
    **409** `replica_exists`(`details.backup`이 그 id), `parent`의 사본이 같은 풀에 `ready`가 아니거나 출처(CA 지문,
    원본 id가 매니페스트의 `parent`)가 맞지 않거나 공유가 아닌 풀이면 이 노드의 것이 아니거나 지우는 중이면 **409**
    `replica_parent_missing`(`details.parent`가 그 id), `parent`가 있는데 증분 복제 기능(`incremental-replication`)이
    없는 노드가 있으면 **409** `cluster_upgrading`, 풀의 남은 공간이 디스크 파일 크기의 합보다 작으면 **507**
    `insufficient_storage`.
  - `PUT /api/v1/backups/{key}/files/{file}`(본문은 파일 그대로, `Content-Length` 필수 — 없으면 **411**) → **200**
    `{"sha256", "size"}`. `file`은 매니페스트의 디스크 파일이나 UEFI면 `vars.fd`뿐(그 밖은 **400**)이고,
    `Content-Length`는 디스크 파일이면 매니페스트의 `stored_bytes`와 같아야, `vars.fd`면 64 MiB까지다(아니면 본문을
    읽기 전에 **400**). `{file}.part`에 받으며 128 MiB마다 내려 쓰고, 끝에 fsync한 뒤 이름을 바꾸고, 디스크 파일의 해시와
    받은 시각을 사본 상태에 적는다. 본문 조각이 60초 넘게 오지 않으면 받던 `.part`를 지우고 **400** `invalid_request`
    ("the upload stalled")다(그 파일을 곧바로 다시 보낼 수 있다). 받는 중이 아니면 **409** `backup_not_receiving`, 다른 노드가 받는 백업이면
    **409** `backup_on_other_node`, 같은 파일을 이 노드가 받고 있으면 **409** `backup_busy`.
  - `POST /api/v1/backups/{key}/actions/complete` → **200** 백업. 디스크 파일의 크기가 매니페스트와 같고 `vars.fd`가
    있어야 한다(아니면 **409** `backup_incomplete`, `details.files`). 지우는 Task가 걸려 있으면 **409** `backup_busy`.
    증분 사본이면 부모 사본을 다시 보고 없어졌거나 `ready`가 아니거나 지우는 중이면 받기와 같은 **409**
    `replica_parent_missing`이다. `manifest.json`의 `backup`은 dr의 사본 id로 쓴다(증분이면 버전 2에 `parent`도 dr의 부모
    사본 id).
  - dr의 감사 로그에는 이 요청들이 토큰 주인의 것으로 남는다. `receiving` 백업은 복원할 수 없고, 지우면 받은 파일도
    지운다.

**DR 절차.**

1. dr에 백업 풀(NFS가 좋다), VM 디스크 풀, prod와 같은 이름의 네트워크·보안 그룹, prod가 쓸 operator 사용자와 그 API
   토큰을 만든다.
2. prod에 dr을 원격으로 등록하고(`oplctl remote add dr …` — dir 백업 풀이면 그 노드의 URL), 백업 작업에
   `--replicate dr/<풀>`을 준다. `oplctl backup list`의 `REPLICAS`가 `dr:ready`인지, 실패면 `error`를 본다.
3. prod를 잃으면 dr에서 `oplctl backup list`(웹 백업 화면)로 `ORIGIN`이 `prod/…`인 사본을 골라 `oplctl backup restore
   <사본> --name <VM> --pool <풀>`로 복원하고 시작한다. 새 VM은 새 MAC이다(MAC으로 묶은 DHCP 예약·라이선스는 다시
   맞춘다). 사본이 원본의 마지막 백업 시각까지의 데이터다(RPO는 백업 주기).
4. dr에서 돌린 VM을 prod로 되돌리려면 같은 방법을 거꾸로(dr에 prod를 원격으로 등록하고 백업을 복제해 prod에서
   복원) 한다. prod를 새로 만들었으면 옛 원본이 없어 dr의 옛 사본은 보존 규칙으로 지워지지 않으므로 dr에서 지운다.

- **한계.**
  - 백업 파일을 그대로 보낸다(전체 백업은 디스크 전체, 증분 백업은 바뀐 블록만 — 중복 제거·압축 추가 없음). 증분
    체인을 처음 보내면 전체 백업부터 모두 간다. 대역폭 제한이 없어 큰 백업은 링크를 채운다(스케줄러는 노드마다 복제
    하나). 진행률이 없다(증분 체인은 원격에 있는 백업 수만).
  - 증분 복제의 건너뛰기는 prod의 사본 기록만 믿는다: 앞 백업의 기록이 `ready`면 dr에 묻지 않으므로, dr에서 (자식
    사본이 없는) 부모 사본을 지웠으면 다음 증분을 보낼 때 dr의 `replica_parent_missing`을 받고서야 알고 체인을 처음부터
    확인하며 다시 보낸다(한 Task에서 한 번).
  - 보내는 동안 멈추면 처음부터 다시 보낸다(파일 단위로 다시 쓴다 — 이어 보내기 없음). 파일 하나를 24시간 넘게
    보내면(마지막으로 받은 파일 뒤 24시간) dr이 그 `receiving` 사본을 지우므로 그런 복제는 끝나지 못한다.
  - 원본을 지울 때 dr에 닿지 못하거나(꺼짐·토큰 거절) `force`로 지우면 사본이 dr에 남는다. dr에서 사본을 지우면 prod는
    다음 작업 실행(손으로 만든 백업이면 다음 `oplctl backup replicate`)에서야 알고 다시 보낸다.
  - 원본을 지울 때 dr이 부모 사본 지우기를 `backup_in_use`로 거절하면 사본마다 60초까지 기다리므로, 원격 사본이 많은
    백업의 지우기 Task는 시간 제한(600초)을 넘을 수 있다(다시 지우면 된다).
  - **prod 쪽 권한이 dr의 백업에 닿는다.** prod에 등록한 operator 토큰은 dr의 모든 백업을 지울 수 있고, prod에서
    원본을 지우면(사용자·보존 규칙) dr의 사본도 곧바로 지워진다 — prod를 빼앗긴 사람이나 실수가 DR 사본까지 없앨 수
    있다(지우기 지연·보관 잠금은 없다). 오래 지켜야 할 것은 dr 쪽에서 따로 보관한다(예: 사본을 복원한 VM을 dr의 백업
    작업으로 백업). 반대로 dr의 operator는 출처를 꾸민 사본을 만들 수 있다(받는 쪽은 출처를 확인하지 않는다). prod는
    사본의 원본 id·풀·스펙·디스크를 확인하고 ready 사본은 해시를 비교하므로 다른 내용의 사본에 붙지는 않지만, 그런
    사본이 있으면 복제가 실패하므로 dr에서 지워야 한다.
  - 원격 토큰이 Raft에 평문인 것, 원격 URL이 노드 하나인 것 등 "원격 클러스터"의 한계를 그대로 가진다. 복원은 VM
    스펙을 바꾸지 못하므로 네트워크·보안 그룹 이름을 dr에 맞춰 둔다. 자동 장애 조치·복구 계획 실행·연속(디스크 단위)
    복제는 없다.

## 이미지 업로드와 머신 종류

- **업로드 사전 확인.** 웹과 `oplctl volume import`는 파일을 보내기 전에 받을 수 있는지 먼저 묻는다
  (`POST /api/v1/volumes/import/check`): 권한, 이름, 풀이 있는지, 풀이 접속한 노드에서 준비됐는지, 같은 이름의 볼륨이
  있는지, 파일 크기 + 1 GiB의 빈 공간이 있는지. 거절되면 파일을 보내지 않고 이유를 보인다.
- **처리 대기.** 웹에서 파일을 다 보낸 뒤 서버가 2분 동안 응답하지 않으면 "처리 중" 알림이 "<이름>: 서버
  응답이 없습니다. 볼륨 목록에서 결과를 확인하세요."로 바뀌고 닫을 수 있다. 업로드는 끊지 않고 서버에서 계속된다.
  나중에 응답이 오면 평소처럼 결과를 알린다.
- **머신 기본값.** 노드는 시작할 때 자기 아키텍처(x86_64·aarch64)를 등록한다(노드 목록의 "아키텍처" 열).
  머신 종류를 정하지 않으면 웹과 `oplctl vm create`는 지정한 노드의 아키텍처를, 노드를 정하지 않았으면 클러스터
  노드가 모두 같은 아키텍처일 때 그것을 쓴다(x86_64 → `q35`, aarch64 → `virt`). 아키텍처가 섞인 클러스터에서
  노드를 정하지 않으면 oplctl은 `--machine q35` 또는 `--machine virt`를 달라는 오류를 내고, 웹은 `q35`를 고른다.
- **배치.** 자동 배치는 머신 종류와 아키텍처가 같은 노드만 고른다. 지정한 노드와 머신이 맞지 않으면 **400**
  `machine_arch_mismatch`다(VM을 시작할 때도 확인한다). 버전이 섞인 클러스터는 지원하지 않으므로 모든 노드를 함께
  올린다. 새 opld로 다시 시작해 아키텍처를 등록하기 전의 노드는 자동 배치 후보가 아니다(직접 지정하면 쓸 수 있다).

## 개발

Linux에서 빌드하려면 `libssl-dev`와 `pkg-config`가 필요하다(클라이언트 TLS가 native-tls다).

```bash
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: 위 apt 패키지)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (macOS hvf, Linux KVM/TCG)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
cargo test -p opl-reconciler --features qemu-it --test qemu_restart # reconciler: opld 재시작 후 재연결
cargo test -p opl-api --features storage-it --test volumes          # API: 볼륨 생성·가져오기 (qemu-img)
cargo test -p opld --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
cargo test -p opld --features qemu-it --test backup                 # E2E: 실행 중 백업 → VM 지우기 → 복원(qemu-img, qemu-io 필요)
cargo test -p opld --features qemu-it --test vm_import              # E2E: OVA 올리기 → 제안 → 가져오기 → 볼륨 내용(qemu-img 필요)
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
# Linux, root: 실제 tap과 브리지
# root가 만든 빌드 결과가 ./target에 섞이지 않게 따로 둔다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opl-driver --features qemu-it,net-it   # tap·브리지·VLAN, 이름공간 사이 VXLAN, nft 방화벽(nftables·python3 필요)
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test vxlan   # E2E: VXLAN 네트워크
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test firewall  # E2E: VM 방화벽(nftables 필요)
# Linux, root: 실제 NFS 풀. nfs-kernel-server로 127.0.0.1에 rw,no_root_squash로 내보낸 디렉터리가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_NFS_EXPORT=/srv/opl-nfs cargo test -p opld --features nfs-it --test nfs
# Linux, root: 실제 LVM-thin 풀. 테스트가 LV를 만들고 지워도 되는 VG/thin pool과 lvm2, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM=opltest/tp cargo test -p opl-driver --features lvm-it --test lvm_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM=opltest/tp cargo test -p opld --features lvm-it --test lvm
# Linux, root: 실제 ZFS 풀. 테스트가 zvol을 만들고 지워도 되는 데이터셋과 zfsutils-linux, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opl-driver --features zfs-it --test zfs_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opld --features zfs-it --test zfs
# Linux, root: 실제 Ceph RBD 풀. 테스트가 이미지를 만들고 지워도 되는 Ceph 풀과 ceph-common, qemu-block-extra, qemu-io,
# client.admin으로 접속할 수 있는 /etc/ceph/ceph.conf·키링이 필요하다. 제한된 사용자 시험은 아래 세 변수가 있을 때만 돈다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd \
  OPL_TEST_RBD_USER=opltest OPL_TEST_RBD_NAMESPACE=opl OPL_TEST_RBD_CONF=/etc/opl-ceph/ceph.conf \
  cargo test -p opl-driver --features rbd-it --test rbd_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd cargo test -p opld --features rbd-it --test rbd
# Linux, root: 실제 공유 LVM 풀. 테스트가 LV를 만들고 지워도 되는, 잠금을 시작한 공유(lvmlockd) VG와 lvm2-lockd, sanlock,
# qemu-io가 필요하다. OPL_TEST_ISCSI(포털,타깃)가 있으면 opld 시험은 VG가 그 타깃의 LUN 위에 있다고 본다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM_SHARED=oplshared \
  cargo test -p opl-driver --features lvm-shared-it --test lvm_shared_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM_SHARED=oplshared \
  OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p opld --features lvm-shared-it --test lvm_shared
# 실제 라이브 마이그레이션: 한 호스트의 opld 둘, 공유 디렉터리 풀(macOS는 TCG로 돈다). nfs-it·rbd-it·lvm-shared-it와
# 그 환경 변수를 함께 주면(Linux, root) 그 풀로도 옮긴다.
cargo test -p opld --features migration-it --test migration
# 실제 HA 복구: 한 호스트의 opld 둘(process 펜싱)과 위트니스, 공유 디렉터리 풀(macOS는 TCG로 돈다).
cargo test -p opld --features ha-it --test ha
# Linux(Ubuntu·Debian), root: 실제 OpenLDAP. ldap-setup.sh가 이 기계의 slapd 설정과 디렉터리를 지우고 늘 새 데이터베이스로
# 다시 설정한다(다시 돌려도 된다 — 옛 데이터베이스는 /var/backups로 옮겨짐). 버려도 되는 기계에서만: CI(`CI=true`)가 아니면
# OPL_LDAP_SETUP_WIPE=1을 주어야 돌고, 없으면 설명을 찍고 끝난다(CI `ldap` 작업).
# 스크립트와 시험이 같은 값을 읽는다(아래는 CI ldap 작업의 값. 스크립트는 없으면 이 값을 쓰지만 시험은 모두 있어야 한다).
export OPL_TEST_LDAP_URL=ldaps://127.0.0.1:636 OPL_TEST_LDAP_STARTTLS_URL=ldap://127.0.0.1:389 \
  OPL_TEST_LDAP_CA=/tmp/opl-test-ldap-ca.pem OPL_TEST_LDAP_BASE_DN=dc=example,dc=com \
  OPL_TEST_LDAP_BIND_DN=cn=admin,dc=example,dc=com OPL_TEST_LDAP_BIND_PASSWORD=opl-admin-pw \
  OPL_TEST_LDAP_USER=alice OPL_TEST_LDAP_USER_PASSWORD=alice-pw OPL_TEST_LDAP_OTHER_USER=bob \
  OPL_TEST_LDAP_OTHER_PASSWORD=bob-pw OPL_TEST_LDAP_GROUP=cn=admins,ou=groups,dc=example,dc=com
sudo -E OPL_LDAP_SETUP_WIPE=1 sh .github/scripts/ldap-setup.sh
cargo test -p opl-api --features ldap-it --test ldap_it
```

`nfs-it` 시험은 opld가 내보내기를 마운트하고, 그 풀에 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면
언마운트하는지 본다(CI `linux-kvm` 작업이 `/srv/opl-nfs`를 내보낸다). **내보내기는 버려도 되는 디렉터리**를 쓴다.
시험은 실패해도 임시 디렉터리를 지우기 전에 opld를 멈추고 마운트를 풀며, 풀지 못하면 임시 디렉터리를 지우지 않고
남긴다(그 경로를 출력한다). NFS 서버를 막 시작했으면 잠금 유예(약 90초)
때문에 첫 실행이 느릴 수 있다.

`lvm-it` 시험은 `OPL_TEST_LVM`의 thin pool에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기·가져오기·스냅샷·
되돌리기·복사·지우기를, opld 시험은 LVM 디스크로 VM을 띄워 실행 중 스냅샷, 멈춘 뒤 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 VG**를 쓴다. CI `linux-kvm` 작업은 루프 장치 위에 VG `opltest`와 thin pool `tp`를 둔다:

```bash
truncate -s 2G /tmp/opltest.img
LOOP=$(sudo losetup --find --show /tmp/opltest.img)
sudo pvcreate -y "$LOOP" && sudo vgcreate opltest "$LOOP" && sudo lvcreate -y -T -L 1G opltest/tp
# 끝나면: sudo vgremove -y opltest && sudo pvremove "$LOOP" && sudo losetup -d "$LOOP"
```

`zfs-it` 시험은 `OPL_TEST_ZFS`의 데이터셋에 `opl-` zvol을 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(가장 최근으로만 되돌리기, 여러 볼륨을 한 명령으로)·복사·지우기를, opld 시험은 ZFS 디스크로 VM을
띄워 실행 중 스냅샷, 최근이 아닌 스냅샷 되돌리기의 409, 멈춘 뒤 최근 스냅샷으로 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 데이터셋**을 쓴다. CI `linux-kvm` 작업은 파일 위에 zpool `opltestz`와 데이터셋
`opltestz/opl`을 둔다:

```bash
sudo modprobe zfs
truncate -s 2G /tmp/opltestz.img
sudo zpool create opltestz /tmp/opltestz.img && sudo zfs create opltestz/opl
# 끝나면: sudo zpool destroy opltestz && rm /tmp/opltestz.img
```

`rbd-it` 시험은 `OPL_TEST_RBD`의 Ceph 풀에 `opl-` 이미지를 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(더 오래된 스냅샷으로 되돌리기)·백엔드 안팎의 복사·지우기와 제한된 사용자·네임스페이스·명시적
conf로 접속하기를, opld 시험은 RBD 디스크로 VM을 띄워
실행 중 스냅샷, 메모리 스냅샷의 409, 멈춘 뒤 앞 스냅샷으로 되돌렸다가 다시 최근 스냅샷으로 되돌리기, 디렉터리
풀로 복제, 지우기를 본다. **버려도 되는 Ceph 풀**을 쓴다. CI `linux-kvm` 작업은 microceph로 노드 하나짜리 Ceph를
띄우고 풀 `rbd`를 둔다(준비에 1–2분):

```bash
sudo apt install ceph-common qemu-block-extra
sudo snap install microceph --channel squid/stable && sudo microceph cluster bootstrap
sudo snap unalias microceph                          # 별칭(/snap/bin/rbd 등)은 snap에 갇혀 /etc/opl-ceph·/tmp를 못 본다
sudo mkdir -p /etc/ceph && sudo ln -sf /var/snap/microceph/current/conf/ceph.conf /etc/ceph/ceph.conf
sudo sh -c 'ln -sf /var/snap/microceph/current/conf/*.keyring /etc/ceph/'
sudo ceph config set global mon_allow_pool_size_one true && sudo ceph config set global osd_pool_default_size 1
sudo microceph disk add loop,4G,1                     # OSD가 올라올 때까지 기다린다(sudo ceph osd stat)
sudo ceph osd pool create rbd 32 && sudo ceph osd pool set rbd size 1 --yes-i-really-mean-it
sudo rbd pool init rbd                                # PG가 active가 된 뒤(sudo ceph -s)
# 제한된 사용자 시험: 네임스페이스와 사용자, 그 키링만 가리키는 conf
sudo rbd namespace create rbd/opl && sudo mkdir -p /etc/opl-ceph
sudo ceph auth get-or-create client.opltest mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd' \
  | sudo tee /etc/opl-ceph/opltest.keyring >/dev/null
{ sudo cat /etc/ceph/ceph.conf; printf '\n[client.opltest]\nkeyring = /etc/opl-ceph/opltest.keyring\n'; } \
  | sudo tee /etc/opl-ceph/ceph.conf >/dev/null
# 끝나면: sudo snap remove --purge microceph
```

`lvm-shared-it` 시험은 `OPL_TEST_LVM_SHARED`의 공유 VG에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기(0
채우기)·배타 활성화와 끄기·연 장치는 끌 수 없음·크기(늘린 부분의 0 채우기)·가져오기와 실패한 가져오기 정리·백엔드 안팎의 복사·지우기를,
opld 시험은 opld가 iSCSI 로그인과 잠금 시작부터 해서 풀을 준비하는지(`OPL_TEST_ISCSI`가 있으면 시험이 먼저 잠금을
멈추고 로그아웃한다), 공유 LVM 디스크로 VM을 켜면 LV가 켜지고 멈추면 꺼지는지, 스냅샷의 409, opld가 다시 뜰 때 남은
활성화 정리(실행 중인 VM의 LV는 그대로), 디렉터리 풀과 같은 풀로 복제, 지우기를 본다. **버려도 되는 VG**를 쓴다. CI
`linux-kvm` 작업은 targetcli(LIO)로 루프백 iSCSI 타깃을 만들어 로그인하고 그 LUN 위에 공유 VG `oplshared`를 둔다
(lvmlockd는 호스트 전체에 켜지지만 로컬 VG인 LVM-thin 시험의 `opltest`는 그대로 쓴다):

```bash
sudo apt install targetcli-fb open-iscsi lvm2-lockd sanlock
IQN=iqn.2003-01.org.linux-iscsi.opl:shared
sudo modprobe target_core_mod && sudo modprobe iscsi_target_mod && sudo systemctl start iscsid
truncate -s 2G /tmp/oplshared.img
sudo targetcli /backstores/fileio create oplshared /tmp/oplshared.img
sudo targetcli /iscsi create "$IQN"                  # 포털 0.0.0.0:3260이 함께 생긴다
sudo targetcli "/iscsi/$IQN/tpg1/luns" create /backstores/fileio/oplshared
sudo targetcli "/iscsi/$IQN/tpg1" set attribute authentication=0 demo_mode_write_protect=0 generate_node_acls=1 cache_dynamic_acls=1
sudo iscsiadm -m discovery -t sendtargets -p 127.0.0.1 && sudo iscsiadm -m node -T "$IQN" -p 127.0.0.1 --login
sudo udevadm settle
sudo modprobe softdog
sudo sed -i 's/^\s*#\?\s*use_lvmlockd = .*/\tuse_lvmlockd = 1/' /etc/lvm/lvm.conf
printf 'local {\n\thost_id = 1\n}\n' | sudo tee /etc/lvm/lvmlocal.conf
sudo systemctl start wdmd sanlock lvmlockd
sudo vgcreate --shared oplshared "/dev/disk/by-path/ip-127.0.0.1:3260-iscsi-$IQN-lun-0"   # 잠금 시작까지 20–30초
# 끝나면: sudo vgremove -y oplshared && sudo iscsiadm -m node -T "$IQN" -p 127.0.0.1 --logout
#         && sudo targetcli /iscsi delete "$IQN" && sudo targetcli /backstores/fileio delete oplshared
#         (lvm.conf의 use_lvmlockd를 0으로 되돌린다)
```

웹 UI (`web/`):

```bash
pnpm -C web install
pnpm -C web dev              # http://localhost:5173, /api는 OPLD_URL(기본 https://localhost:8006)로 넘긴다
pnpm -C web test             # 단위·컴포넌트 테스트 (Vitest)
pnpm -C web typecheck && pnpm -C web lint
pnpm -C web gen:api          # API를 바꾼 뒤 src/api/schema.d.ts를 다시 만든다(CI가 확인한다)
pnpm -C web exec playwright install chromium
pnpm -C web build && cargo test -p opld --features web-it --test web   # E2E: 브라우저 → 업로드 → VM → 콘솔 → 정리
```

`cargo test --workspace --all-features`는 `web-it`도 켜므로 위 준비(빌드한 `web/dist`, pnpm, Chromium)가
필요하다.

systemd가 있는 Linux에서는 `OPL_TEST_SYSTEMD=1`을 주면 QEMU 테스트가 systemd 스코프로 돈다(CI의
`linux-kvm` 작업). macOS에서는 Docker의 privileged Linux 컨테이너(`--init`)로 Linux 테스트를 TCG로
돌릴 수 있다.

## 문서

- [1차 하위 프로젝트 설계: 노드 코어](docs/superpowers/specs/2026-10-20-opl-node-core-design.md)
- [하위 프로젝트 2 설계: 웹 UI](docs/superpowers/specs/2026-10-23-web-ui-design.md)
- [하위 프로젝트 3 설계: VM 스냅샷](docs/superpowers/specs/2026-10-26-snapshots-design.md)
- [클러스터·Task 견고성 설계 (계획 12)](docs/superpowers/specs/2026-10-27-robustness-design.md)
- [API·보안 보완 설계 (계획 13)](docs/superpowers/specs/2026-10-28-api-security-design.md)
- [웹 정리 설계 (계획 14)](docs/superpowers/specs/2026-10-28-web-cleanup-design.md)
- [공유 스토리지 설계 1: 백엔드 구조와 NFS (계획 15)](docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md)
- [공유 스토리지 설계 2: 블록 백엔드 바탕과 LVM-thin (계획 16)](docs/superpowers/specs/2026-10-29-shared-storage-lvm-thin-design.md)
- [공유 스토리지 설계 3: ZFS zvol (계획 17)](docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md)
- [공유 스토리지 설계 4: Ceph RBD (계획 18)](docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md)
- [공유 스토리지 설계 5: iSCSI와 공유 LVM (계획 19)](docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md)
- [라이브 마이그레이션 설계 (하위 프로젝트 4: 계획 20·21)](docs/superpowers/specs/2026-10-30-live-migration-design.md)
- [HA·펜싱·위트니스 설계 (하위 프로젝트 5: 계획 22·23·24)](docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md)
- [네트워크·SDN 설계 (하위 프로젝트 6: 계획 25·26)](docs/superpowers/specs/2026-11-03-network-sdn-design.md)
- [백업 설계 (하위 프로젝트 7: 계획 27·28)](docs/superpowers/specs/2026-11-04-backup-design.md)
- [VMware 가져오기 설계 (하위 프로젝트 8: 계획 29·30)](docs/superpowers/specs/2026-11-04-vmware-import-design.md)
- [설치·업데이트·구독 설계 (하위 프로젝트 9: 계획 31·32·33)](docs/superpowers/specs/2026-11-05-install-update-subscription-design.md)
- [엔터프라이즈 설계 (하위 프로젝트 10: 계획 34~38)](docs/superpowers/specs/2026-11-06-enterprise-design.md)
- [노드 유지보수(노드 비우기) 설계 (하위 프로젝트 11: 계획 39)](docs/superpowers/specs/2026-11-09-node-maintenance-design.md)
- [증분 백업·증분 복제 설계 (하위 프로젝트 12: 계획 40·41)](docs/superpowers/specs/2026-11-09-incremental-backup-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
- [계획 2: 하이퍼바이저·스토리지 드라이버](docs/superpowers/plans/2026-10-20-plan2-opl-driver.md)
- [계획 3: 클러스터 저장소](docs/superpowers/plans/2026-10-21-plan3-opl-cluster.md)
- [계획 4: reconciler](docs/superpowers/plans/2026-10-21-plan4-opl-reconciler.md)
- [계획 5: API, opld, oplctl](docs/superpowers/plans/2026-10-21-plan5-api-opld-oplctl.md)
- [계획 6: 클러스터 부트스트랩, 노드 간 통신](docs/superpowers/plans/2026-10-22-plan6-cluster-bootstrap.md)
- [계획 7: Linux/KVM, 브리지 네트워크](docs/superpowers/plans/2026-10-22-plan7-kvm-bridge.md)
- [계획 8: 웹 UI 기반과 가상 머신 화면](docs/superpowers/plans/2026-10-23-plan8-web-ui.md)
- [계획 9: 웹 UI 관리 화면](docs/superpowers/plans/2026-10-26-plan9-web-ui-admin.md)
- [계획 10: VM 스냅샷과 볼륨 크기 늘리기](docs/superpowers/plans/2026-10-26-plan10-snapshots.md)
- [계획 11: 스냅샷에서 VM 복제와 예약 스냅샷](docs/superpowers/plans/2026-10-27-plan11-clone-schedule.md)
- [계획 12: 클러스터·Task 견고성](docs/superpowers/plans/2026-10-27-plan12-robustness.md)
- [계획 13: API·보안 보완](docs/superpowers/plans/2026-10-28-plan13-api-security.md)
- [계획 14: 웹 정리](docs/superpowers/plans/2026-10-28-plan14-web-cleanup.md)
- [계획 15: 공유 스토리지 1 — 백엔드 구조와 NFS](docs/superpowers/plans/2026-10-28-plan15-shared-storage-nfs.md)
- [계획 16: 공유 스토리지 2 — 블록 백엔드 바탕과 LVM-thin](docs/superpowers/plans/2026-10-29-plan16-shared-storage-lvm-thin.md)
- [계획 17: 공유 스토리지 3 — ZFS zvol](docs/superpowers/plans/2026-10-29-plan17-shared-storage-zfs.md)
- [계획 18: 공유 스토리지 4 — Ceph RBD](docs/superpowers/plans/2026-10-29-plan18-shared-storage-ceph-rbd.md)
- [계획 19: 공유 스토리지 5 — iSCSI와 공유 LVM](docs/superpowers/plans/2026-10-30-plan19-shared-storage-iscsi-lvm.md)
- [계획 20: 라이브 마이그레이션](docs/superpowers/plans/2026-10-30-plan20-live-migration.md)
- [계획 21: UEFI 변수 따라가기와 풀의 공유 설정 바꾸기](docs/superpowers/plans/2026-10-31-plan21-uefi-vars-pool-shared.md)
- [계획 22: 위트니스 노드](docs/superpowers/plans/2026-11-02-plan22-witness.md)
- [계획 23: HA 리스와 자기 펜싱](docs/superpowers/plans/2026-11-02-plan23-ha-lease-fencing.md)
- [계획 24: HA 복구](docs/superpowers/plans/2026-11-02-plan24-ha-recovery.md)
- [계획 25: VXLAN 네트워크](docs/superpowers/plans/2026-11-03-plan25-vxlan.md)
- [계획 26: VM 방화벽](docs/superpowers/plans/2026-11-03-plan26-vm-firewall.md)
- [계획 27: 백업과 복원](docs/superpowers/plans/2026-11-04-plan27-backup-restore.md)
- [계획 28: 백업 작업과 보존](docs/superpowers/plans/2026-11-04-plan28-backup-jobs.md)
- [계획 29: OVA 가져오기](docs/superpowers/plans/2026-11-04-plan29-ova-import.md)
- [계획 30: ESXi에서 바로 가져오기](docs/superpowers/plans/2026-11-04-plan30-esxi-import.md)
- [계획 31: 패키지와 설치 ISO](docs/superpowers/plans/2026-11-05-plan31-packaging-iso.md)
- [계획 32: 버전과 업데이트 채널](docs/superpowers/plans/2026-11-05-plan32-updates.md)
- [계획 33: 구독과 텔레메트리](docs/superpowers/plans/2026-11-05-plan33-subscription.md)
- [계획 34: 감사 로그](docs/superpowers/plans/2026-11-06-plan34-audit-log.md)
- [계획 35: 인증 영역과 LDAP](docs/superpowers/plans/2026-11-06-plan35-ldap-realms.md)
- [계획 36: OIDC SSO](docs/superpowers/plans/2026-11-07-plan36-oidc-sso.md)
- [계획 37: 원격 클러스터](docs/superpowers/plans/2026-11-08-plan37-remote-clusters.md)
- [계획 38: 원격 복제(DR)](docs/superpowers/plans/2026-11-08-plan38-remote-replication.md)
- [계획 39: 노드 유지보수·비우기](docs/superpowers/plans/2026-11-09-plan39-node-drain.md)
- [계획 40: 증분 백업](docs/superpowers/plans/2026-11-09-plan40-incremental-backup.md)
