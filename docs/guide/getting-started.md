# 시작하기

macOS·Linux에서 실행, 설치 ISO와 패키지, 업데이트, 이름 바꾸기. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [실행 (macOS, Apple Silicon)](#실행-macos-apple-silicon)
- [실행 (Linux, KVM)](#실행-linux-kvm)
- [설치 (ISO와 패키지)](#설치-iso와-패키지)
- [업데이트](#업데이트)
- [이름 바꾸기 (opLite → Orbivisor)](#이름-바꾸기-oplite--orbivisor)

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

실행 중인 VM에도 디스크·NIC를 붙이고 뗄 수 있다(`vm disk add`, `vm nic add` — macOS 노드에서 vCPU·메모리 바꾸기는 다음
재시작 때 적용). 실행 중에 하드웨어를 바꾼 VM의 스냅샷은 다시 켤 때까지 디스크만 찍는다 —
[VM 무중단 유형 변경](vms.md#vm-무중단-유형-변경).

같은 Mac에 노드를 더하려면 포트와 디렉터리를 바꿔 orbivisord를 하나 더 띄우고, 조인 URL로 합류한다.

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

## 실행 (Linux, KVM)

orbivisord는 root로 돈다(VM NIC의 tap 장치를 만든다). QEMU는 VM마다 `opl-vm-<id>.scope`로 감싼다.
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
# [runtime]
# api_threads = 4           # API 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
# work_threads = 4          # VM·작업 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
TOML
sudo orbivisord --config /etc/opl/opld.toml &
sudo orbivisorctl cluster init --name prod
orbivisorctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:...
orbivisorctl network create lan --bridge vmbr0 --vlan 10  # VLAN 10을 태그 없이 주고받는 NIC
# 풀과 볼륨은 macOS와 같다(/var/lib/opl/pool 등).
orbivisorctl vm create web --memory 1024 --disk root --nic lan --start
```

브리지를 쓰는 VM은 그 브리지가 있는 노드에만 배치된다(`--node`로 노드를 직접 정하면 확인하지 않는다).
노드는 브리지 목록을 시작할 때와 1분마다 알린다.

## 설치 (ISO와 패키지)

서버에는 설치 ISO로 Debian 13(trixie)과 Orbivisor를 함께 설치한다. ISO는 Debian netinst에 패키지 `orbivisor`와
preseed를 더한 것이다. 만들기는 Linux(amd64)에서 한다(`dpkg-deb`, `xorriso`가 필요하다).

```bash
sudo apt install xorriso libssl-dev pkg-config     # 그리고 Rust, Node.js 22와 pnpm
# 1. 패키지. 버전은 바이너리(--version)와 패키지에 같은 값을 쓴다(다르면 build-deb.sh가 멈춘다).
pnpm -C web install --frozen-lockfile && pnpm -C web build
#    구독 공개 키(OPL_LICENSE_KEYS) 없이 빌드하면 저장소의 개발 키를 믿는 개발 패키지다("구독과 텔레메트리").
OPL_VERSION=0.1.0 cargo build --release -p opld --features web-ui -p oplctl
OPL_DEV_LICENSE=1 packaging/deb/build-deb.sh 0.1.0 dist   # dist/orbivisor_0.1.0_amd64.deb와 전환 패키지("이름 바꾸기")
# 2. netinst. 판과 SHA-256은 packaging/iso/debian.lock에 있고, build-iso.sh가 해시를 확인한다.
#    그 판보다 새 판이 나와 URL이 404면 같은 파일을 cdimage/archive/<판>/amd64/iso-cd/에서 받는다.
. packaging/iso/debian.lock && curl -fLo netinst.iso "$DEBIAN_NETINST_URL"
# 3. ISO. 기본 항목 "Install Orbivisor"가 더해진다(BIOS·UEFI, 원래 항목은 남는다).
packaging/iso/build-iso.sh --netinst netinst.iso --deb dist/orbivisor_0.1.0_amd64.deb --out orbivisor.iso
```

`orbivisor.iso`를 USB(`dd`)나 가상 CD로 부팅한다. "Install Orbivisor"가 기본으로 골라져 있다. UEFI 메뉴에서는 맨 위
줄이고, BIOS 메뉴에서는 "Graphical install" 다음 두 번째 줄이다. 설치 화면은 다음만 묻는다: 네트워크(DHCP가
실패하면 수동), 호스트 이름, 도메인, root 비밀번호, 설치 디스크(디스크가 여럿이면 GRUB을 넣을 디스크도).
나머지는 정해져 있다: 언어·키보드 en_US·us, 시간대 UTC(설치한 뒤 `timedatectl set-timezone Asia/Seoul`처럼 바꾼다),
고른 디스크 전체에 LVM(`/`와 swap), 미러 `deb.debian.org`, 패키지 standard·ssh-server, 일반 사용자 없음.
**고른 디스크의 내용은 묻지 않고 지운다.**

설치 끝에 `orbivisor-setup.sh`가 돈다(로그는 대상 시스템의 `/root/orbivisor-setup.log`).

- 패키지 `orbivisor`, `ifupdown2`, `isc-dhcp-client`(`vmbr0`의 DHCP)를 설치한다(의존 패키지는 미러에서 받는다).
- 기본 경로 인터페이스(DHCP이거나 게이트웨이가 있는 것, 없으면 첫 고정 주소)를 VLAN-aware 브리지 `vmbr0`으로 옮긴다.
  주소·게이트웨이·DNS는 `vmbr0`으로 가고, NIC는 `iface <nic> inet manual`로 `bridge-ports`가 된다(`bridge-vids 2-4094`).
  설치 프로그램이 쓴 `/etc/network/interfaces`를 바꿔 설치 환경과 대상 시스템 두 곳에 쓴다(설치 프로그램은 이 파일을
  나중에 대상으로 복사한다). 인터페이스를 찾지 못하면 바꾸지 않는다.
- `/etc/opl/opld.toml`에 `name`(호스트 이름을 소문자로)과 `address`(`vmbr0`의 주소)를 쓴다. 호스트 이름이 노드 이름
  규칙(소문자·숫자·`-`, 1~63자)에 맞지 않거나 주소를 찾지 못하면 그 줄을 주석으로 둔다. 그때 orbivisord는 설정 오류로
  시작하지 못하므로(`journalctl -u opld`) 값을 채우고 `systemctl restart opld`를 한다. 다른 설정은
  `/usr/share/orbivisor/opld.toml.example`을 본다.
- 콘솔 로그인 화면에 웹 UI 주소와 init·join 명령을 영어로 보인다(`/etc/issue.d/orbivisor.issue`). `opld.toml`의 주석도 영어다.
- `opld.service`를 켠다. 다시 부팅하면 orbivisord가 돈다(`systemctl status opld`, `journalctl -u opld`).

설치한 시스템의 root는 SSH로 비밀번호 로그인을 할 수 없다(Debian 기본 `PermitRootLogin prohibit-password`).
콘솔에서 root로 로그인해 `adduser <이름>`으로 일반 사용자를 만들거나 `/root/.ssh/authorized_keys`에 공개 키를 넣는다.

클러스터는 설치한 뒤에 만든다. 첫 노드에서 `cluster init`을 하고, 다른 노드는 그 클러스터에 합류한다.

```bash
# 첫 노드(root)
orbivisorctl cluster init --name prod            # admin 비밀번호(한 번만 나온다), CA 지문, 합류 URL
orbivisorctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:...
orbivisorctl cluster join-token                  # 합류 URL은 한 번 쓰고 1시간 간다. 노드마다 새로 만든다
# 다른 노드(root)
orbivisorctl cluster join 'opl-join://…'
```

웹 UI는 `https://<노드 주소>:8006/`이다(클러스터 화면이 합류 명령을 만들어 준다).

**DHCP 주소.** 설치를 DHCP로 하면 `address`는 설치할 때 받은 주소이고 파일에 그렇다는 주석이 붙는다. 다른 노드와
클라이언트가 이 주소로 노드에 닿고, 노드가 클러스터에 들어간 뒤에는 주소를 바꿀 수 없다(노드 인증서에 들어 있다).
DHCP 서버에서 주소를 예약하거나 고정 주소를 쓴다. `cluster init`·`join` 전이라면 `/etc/network/interfaces`의
`vmbr0`과 `opld.toml`의 `address`를 고치고 `systemctl restart opld`를 해도 된다.

**패키지만 설치하기.** Debian 13(amd64)이 이미 있으면 패키지만 넣는다. 브리지는 관리자가 만든다("실행 (Linux, KVM)").

```bash
sudo apt install ./orbivisor_0.1.0_amd64.deb   # 의존 패키지(QEMU, OVMF, nftables 등)도 받는다
sudo mkdir -p /etc/opl
sudo cp /usr/share/orbivisor/opld.toml.example /etc/opl/opld.toml  # name·address를 고친다
sudo systemctl start opld                         # 설정이 없으면 유닛은 시작하지 않고 건너뛴다
```

패키지는 `/usr/bin/orbivisord`·`orbivisorctl`(웹 UI 포함)과 옛 이름의 링크 `opld`·`oplctl`,
`/usr/lib/systemd/system/opld.service`와 별칭 `orbivisord.service`, `/usr/share/orbivisor/opld.toml.example`을
설치한다. 유닛 이름은 `opld`다(`systemctl status orbivisord`도 되지만 로그는 `journalctl -u opld` — "이름 바꾸기"). 업그레이드(`apt install`로 새 판)는 orbivisord가 enable돼 있으면 다시 시작한다.
옛 이름의 패키지가 설치된 노드를 deb 파일로 옮길 때는 전환 패키지도 함께 설치한다
(`sudo apt install ./orbivisor_0.1.0_amd64.deb ./oplite_0.1.0_amd64.deb` — "이름 바꾸기").
옛 패키지가 `rc`(지워졌지만 설정이 남음)로 남아 있으면(`dpkg -l oplite`) 그것을 purge하지 않는다 — 옛 postrm이
opld를 disable해 다음 재부팅에 orbivisord가 시작하지 않는다. purge했다면 `sudo systemctl enable opld`("이름 바꾸기").
VM의 QEMU는 VM마다 자기 scope에 있어 orbivisord가 멈추거나 다시 시작해도 꺼지지 않는다.

**지우기.** `apt remove orbivisor`는 orbivisord를 멈추고, `apt purge orbivisor`는 서비스 링크까지 지운다. 어느 쪽도
`/var/lib/opl`(클러스터 데이터, 풀, 볼륨)과 `/etc/opl`을 지우지 않는다. 데이터를 지우는 것은 관리자의 일이다.

한계:

- 설치에 인터넷(Debian 미러)이 필요하다. 모든 패키지를 담은 오프라인 ISO는 없다.
- amd64만 있다(ISO와 패키지).
- 루트 파일 시스템은 LVM 하나다. ZFS 루트와 소프트웨어 RAID 설치 선택지는 없다.
- 설치 화면에서 클러스터를 만들거나 합류하지 않는다(설치한 뒤 위 명령으로).

## 업데이트

패키지로 설치한 노드는 Orbivisor APT 저장소에서 업데이트를 확인하고, 관리자가 노드를 **하나씩** 업그레이드한다.
클러스터 설정의 **채널**(`stable`·`testing`, 구독이 있으면 `enterprise`)이 어느 저장소 구성 요소를 따를지 정한다.

```bash
orbivisorctl settings get                        # updates.channel: stable
orbivisorctl settings set updates.channel=testing  # admin. 모든 노드의 소스 파일이 바뀐다
orbivisorctl node list                           # VERSION 열(ADDRESS 다음)
orbivisorctl node updates node1                  # 마지막 확인 시각, PACKAGE CURRENT CANDIDATE 표, 확인 실패 이유
orbivisorctl node updates node1 --refresh        # 지금 다시 확인하고(작업을 기다린다) 결과를 보인다
orbivisorctl node upgrade node1                  # upgrade of node1 started (task …); follow it with `orbivisorctl task get …`
orbivisorctl node upgrade node1 --wait           # 끝날 때까지 기다린다: "node node1 upgraded"
orbivisorctl cluster status                      # 버전이 섞여 있으면 경고
```

- **채널.** 설정은 클러스터에 하나뿐인 `Settings` 리소스(이름 `cluster`)다. `GET /api/v1/settings`는 로그인한 누구나,
  `PATCH /api/v1/settings`(`{"updates":{"channel":"testing"}}`, 다른 패치 API처럼 `If-Match`를 받는다)는 admin만
  한다. 한 번도 바꾸지 않았으면 기본값(`stable`)이다. 웹은 설정 › 일반 탭(admin만 저장, 그 밖은 읽기만).
- **소스 파일.** 저장소 주소는 orbivisord 설정 `[updates] apt_url`이고, 비면 빌드할 때 넣은 `OPL_APT_URL`이다. 둘 다 비면
  orbivisord는 소스 파일을 쓰지 않는다(관리자가 apt 소스를 직접 둔다). 주소가 있으면 orbivisord가 시작할 때와 채널이 바뀔 때
  `/etc/apt/sources.list.d/oplite.sources`를 쓴다:

  ```
  # managed by opld; edits are overwritten
  Types: deb
  URIs: https://apt.example.com/oplite
  Suites: trixie
  Components: stable
  Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg
  ```

  첫 줄의 표시가 없는 파일은 orbivisord가 건드리지 않고 로그에 경고만 남긴다(비었거나 공백뿐인 파일은 orbivisord 것으로 본다).
  소스를 직접 관리하려면 첫 줄을 지운다. 저장소 공개 키(`Signed-By`의 키링)가 없으면 apt가 저장소를 거절하므로
  orbivisord는 소스 파일을 쓰지 않고(경고) 자기가 쓴 파일은 지운다. 패키지를 지우면(`remove`·`purge`) postrm이 표시가
  있는 소스 파일을 지운다.
- **확인.** 노드마다 orbivisord가 시작하고 1분 뒤, 그 뒤 24시간마다 `apt-get update -q`와 `apt-get -s -q dist-upgrade`
  (설치하지 않고 흉내만)로 올릴 수 있는 패키지를 찾아 노드 상태(`node get`의 `status.updates`)에 쓴다. Orbivisor만이
  아니라 Debian 패키지(보안 업데이트 등)도 보인다. 500개까지 담고 넘으면 잘렸다고 표시한다. 다른 apt(무인 업그레이드
  등)의 잠금은 `DPkg::Lock::Timeout=60`초까지 기다린다. 실패하면 이유(명령과 stderr 끝 5줄)를 남긴다. `--refresh`와
  웹의 "다시 확인"은 그 노드에서 도는 작업 `node-update-check`다(admin. 끝나지 않은 확인이 있으면 새로 만들지 않고 그
  작업을 돌려준다. 주기 확인이 도는 중이면 끝나기를 기다렸다가 확인하고, 그사이 업그레이드가 시작되면 확인하지 않고
  끝난다). 채널을 바꿔 소스 파일이 바뀌면 다음 주기를 기다리지 않고 곧 확인한다. 업그레이드 중에는 주기 확인을
  건너뛰고 다시 확인은 **409** `upgrade_in_progress`다(업그레이드가 apt 잠금을 잡고 있다).
- **업그레이드.** `node upgrade`(admin)는 그 노드에서 작업 `node-upgrade`를 돌린다. 작업은 orbivisord 밖의 systemd 유닛
  `opl-upgrade-<작업 ID 32자(하이픈 없이)>`(`systemd-run`)에서 `apt-get update`와 `apt-get dist-upgrade`(바뀐 설정
  파일은 지금 것을 둔다. 무인 업그레이드 같은 다른 apt의 잠금은 `DPkg::Lock::Timeout=600`초까지 기다린다)를 돌리고
  유닛을 지켜본다. 로그는 `/var/log/opl/upgrade-<작업 ID>.log`이고 처음에 `opl-upgrade: start`, 끝에
  `opl-upgrade: exit <코드>` 줄이 있다. Orbivisor 패키지가 바뀌면 postinst가 orbivisord를 다시 시작하고, 다시 시작한 orbivisord가
  같은 유닛을 이어 지켜본다. VM은 자기 scope에 있어 계속 돈다. 실패하면 작업 오류에 로그 끝 20줄이 있다. 유닛이
  끝나고 치워졌으면 로그로 판단한다: 종료 줄이 있으면 그 코드, 시작 줄만 있으면(도중에 노드가 꺼짐) dpkg가 반쯤
  설정된 채 남았을 때 "the upgrade was interrupted; run "dpkg --configure -a" on the node", 아니면 "the upgrade was
  interrupted; start it again", 시작 줄도 없으면 이어 볼 때는 유닛을 지금 띄우고(apt는 다시 돌려도 된다) 아니면
  "the upgrade did not start; try again"으로 실패한다. 업그레이드하는 동안 그 노드의 업데이트 확인은 쉰다. 시간 제한과
  취소는 없다(apt를 중간에 끊으면 더 나쁘다). 끝나면(성공·실패 모두) 업데이트를 다시 확인하고, 새 버전은 다시 시작한
  orbivisord가 노드 상태에 쓴다.
- **거절.** 클러스터 어디든 끝나지 않은 업그레이드가 있으면 **409** `upgrade_in_progress`(한 번에 한 노드. 다른
  노드로 함께 온 두 요청이 모두 지나가도 나중에 시작한 작업이 "upgrade_in_progress: node "X" is being upgraded"로
  실패한다), 오프라인 노드는 **409** `node_offline`, 업데이트를 확인하지 않는 노드(macOS, apt가 없는 노드, 위트니스)는
  **400** `updates_unsupported`다. 버전을 적지 않은 옛 노드(계획 32 전의 orbivisord)가 남아 있으면 설정 바꾸기·다시
  확인·업그레이드는 **409** `cluster_upgrading`("some nodes run an older Orbivisor (node2, node3); …")이다: 옛 노드는
  모르는 리소스 종류와 작업 종류가 든 Raft 항목을 풀지 못한다. 그래서 계획 32 전 클러스터의 첫 업그레이드는 노드마다
  손으로(`apt-get update && apt-get dist-upgrade`) 하고, 모든 노드가 버전을 적은 뒤부터 `node upgrade`를 쓴다.
- **`--wait`.** 업그레이드하는 노드의 API에 로그인한 orbivisorctl은 orbivisord가 다시 시작할 때 연결 오류로 끝날 수 있다.
  업그레이드는 계속된다. 다른 노드에 로그인해 두거나 `orbivisorctl task get <ID>`로 다시 본다.
- **합류.** 클러스터에서 가장 낮은 노드 버전보다 낮은 orbivisord는 합류할 수 없다(**409** `version_too_old`: "this node runs
  X; the cluster runs Y or newer; upgrade it first"). 버전을 보내지 않는 옛 orbivisord도 버전을 적은 노드가 있는 클러스터에는
  합류할 수 없다("this node runs an unknown version; …"). 새 노드는 먼저 클러스터와 같은 버전으로 올린다. 합류할 때
  보낸 버전은 그 노드가 등록하기 전부터 노드 상태에 적힌다.

**롤링 순서.** Orbivisor는 노드를 자동으로 돌아가며 올리지 않고 노드를 다시 부팅하지도 않는다. 한 노드씩:

1. `orbivisorctl node updates <노드> --refresh`로 무엇이 바뀌는지 본다.
2. 커널처럼 다시 부팅해야 하는 업데이트가 있으면 `orbivisorctl node drain <노드> --wait`로 노드를 유지보수 모드로 두고
   VM을 다른 노드로 옮긴다("노드 유지보수와 비우기" — 어느 노드에서 해도 된다). 비우기가 처음 들어간 버전으로 올리는
   동안은 옛 노드가 비우기를 몰라 쓸 수 없으므로(`cluster_upgrading`) 그때는 VM을 옮기고(`vm migrate`) 유지보수 모드로
   둔다(`orbivisorctl node maintenance <노드> on` 또는 웹 "유지보수 시작"). 그 뒤의 업그레이드에서는 버전이 섞여도 된다.
3. `orbivisorctl node upgrade <노드> --wait`(다른 노드에 로그인해서). 필요하면 노드를 다시 부팅하고 유지보수 모드를
   끈다(`orbivisorctl node maintenance <노드> off`).
4. `orbivisorctl node list`에서 그 노드의 VERSION과 ONLINE을 확인하고 다음 노드로 간다.

모든 노드가 같은 버전이 될 때까지 `cluster status`와 웹 홈에 "nodes run different Orbivisor versions (…); finish
the upgrade on every node" 경고가 보인다. 웹은 노드 표의 업데이트 배지(올릴 패키지 수, 확인 실패면 경고)를 누르면
같은 정보와 "다시 확인"·"업그레이드" 버튼(admin)이 있는 대화상자를 연다. 로그인 화면 아래에 서버 버전
(`GET /api/v1/version`, 인증 없음)이 보인다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[updates]
# enabled = true                 # 없으면 apt_get이 있을 때(Debian) 켜진다. macOS는 꺼진다
check_interval = "24h"           # 업데이트 확인 간격
first_check = "1m"               # orbivisord가 시작하고 첫 확인까지
apt_url = ""                     # 비면 빌드 값 OPL_APT_URL. 그것도 비면 소스 파일을 쓰지 않는다
sources_file = "/etc/apt/sources.list.d/oplite.sources"
apt_get = "/usr/bin/apt-get"
systemd_run = "/usr/bin/systemd-run"
systemctl = "/usr/bin/systemctl"
dpkg = "/usr/bin/dpkg"
log_dir = "/var/log/opl"         # 업그레이드 로그
poll = "2s"                      # 업그레이드 유닛을 다시 보는 간격
```

**저장소 만들기.** 저장소는 서명한 정적 파일 트리이고, 올릴 웹 서버는 Orbivisor 밖이다. 패키지에는 저장소 주소와 서명
공개 키를 빌드할 때 넣는다(키를 넣지 않은 패키지는 `Signed-By`의 키링이 없어 apt가 저장소를 거절한다).

```bash
# 서명 키(한 번). GPG 홈은 비밀 키를 둘 곳으로(GNUPGHOME).
gpg --quick-generate-key 'Orbivisor archive <apt@example.com>' ed25519 sign never
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
OPL_REPO_GPG_KEY_ID=$KEY_ID packaging/repo/build-repo.sh repo testing dist/*_0.2.0_amd64.deb   # orbivisor와 전환 패키지 둘
# testing에서 확인한 뒤 같은 패키지를 stable에도
OPL_REPO_GPG_KEY_ID=$KEY_ID packaging/repo/build-repo.sh repo stable dist/*_0.2.0_amd64.deb
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

## 이름 바꾸기 (opLite → Orbivisor)

제품 이름이 계획 43에서 Orbivisor로 바뀌었다. 바뀐 것은 사람이 보는 이름(패키지, 바이너리, 웹, ISO, CLI 도움말과
메시지)뿐이고, 노드에 저장된 상태와 노드 사이에 주고받는 이름은 그대로다. 그래서 이미 설치한 노드도 보통의 업그레이드
(`orbivisorctl node upgrade`, 웹 "업그레이드" — 위 "업데이트")로 한 노드씩 넘어가고, 버전이 섞인 채로도 돈다.

| 무엇 | 옛 이름 | 지금 |
|---|---|---|
| Debian 패키지 | `oplite` | `orbivisor`(옛 이름은 빈 전환 패키지) |
| 노드 데몬 | `opld` | `orbivisord`(링크 `/usr/bin/opld`도 있다) |
| 명령줄 | `oplctl` | `orbivisorctl`(링크 `/usr/bin/oplctl`도 있다) |
| systemd 유닛 | `opld.service` | 그대로(별칭 `orbivisord.service`) |
| 설치 ISO | `oplite.iso`, "Install opLite" | `orbivisor.iso`, "Install Orbivisor" |
| 예시 설정 | `/usr/share/oplite/opld.toml.example` | `/usr/share/orbivisor/opld.toml.example` |
| 콘솔 안내 | `/etc/issue.d/oplite.issue` | `/etc/issue.d/orbivisor.issue`(새 설치) |

- **옛 이름도 된다.** `opld …`·`oplctl …`(링크), `systemctl … opld`, `/etc/opl/opld.toml`, `/run/opl/opld.sock`,
  `~/.config/opl/oplctl.json`(두 CLI가 같은 로그인을 본다), `OPL_*` 환경 변수를 쓰는 스크립트는 그대로 돈다. 링크
  이름으로 불러도 `--version`은 `orbivisord <버전>`·`orbivisorctl <버전>`이다(도움말의 사용법 줄만 부른 이름).
- **유닛 이름은 `opld`다.** `systemctl status|restart|stop orbivisord`는 별칭으로 되지만, 로그는 `journalctl -u opld`로
  본다(`-u orbivisord`는 빈 결과). 덮어쓰기 파일도 `systemctl edit opld`로 만든다(`/etc/systemd/system/opld.service.d/`).
  journald의 식별자(`SYSLOG_IDENTIFIER`, rsyslog의 `$programname`)는 프로세스 이름 `orbivisord`가 된다: 넘어간 노드는
  `journalctl -t opld`가 아니라 `journalctl -t orbivisord`다.
- **넘어가는 업그레이드.** 새 판의 `oplite`는 파일 없이 `orbivisor`에 의존하는 전환 패키지라, `apt-get dist-upgrade`가
  `orbivisor`를 새로 설치하고 옛 패키지의 파일을 넘겨받는다. 그래서 `node updates`와 웹의 업데이트 목록에 `orbivisor`
  (지금 버전 없음)와 `oplite` 두 줄이 보인다. 설치 뒤 `orbivisor`의 postinst가 enable된 `opld.service`를 다시 시작한다.
  `orbivisor`는 새로 설치하는 패키지이므로 apt가 그 Recommends(`nfs-common`, `lvm2`, `thin-provisioning-tools`,
  `open-iscsi`, `lvm2-lockd`, `sanlock`, `ceph-common`)도 설치한다 — 관리자가 Recommends를 지운 노드에서는 다시 설치된다
  (원하지 않으면 업그레이드 뒤 다시 지운다. 그 기능의 풀을 쓰지 않으면 없어도 된다).
  관리자가 꺼 둔(disable) opld는 넘어간 뒤에도 꺼진 채다. 저장소 없이 deb 파일로 옮길 때는 두 파일을 함께 설치한다:
  `sudo apt install ./orbivisor_0.1.0_amd64.deb ./oplite_0.1.0_amd64.deb`. 새 deb 하나만 설치하면 apt가 옛 패키지를
  지우면서 opld를 멈추고 opld가 쓴 소스 파일(`oplite.sources`)을 지운다 — 데몬은 다시 켜지지만 그동안 멈춘다.
  그렇게 해서(또는 `apt remove`로) 옛 패키지가 `rc` 상태로 남았으면 purge하지 않는다(`apt purge`, `apt autoremove --purge`,
  `dpkg --purge` 정리 포함): 옛 `postrm purge`가 opld를 disable하고 enable 링크를 지워 다음 재부팅에 orbivisord가
  시작하지 않는다. `dpkg -l oplite`가 `rc`면 그대로 두고, 이미 purge했다면 `sudo systemctl enable opld`를 한다.
- **전환 패키지 지우기.** 넘어간 뒤 `apt-get remove oplite`는 안전하다(파일·유지 스크립트가 없고 `orbivisor`가 의존하지
  않는다). `apt autoremove`가 전환 패키지를 스스로 지울 수도 있는데 이것도 안전하다. 저장소에는 판마다 `orbivisor`와
  함께 낸다(`build-repo.sh`에 둘 다 — 위 "저장소 만들기").
- **APT 저장소.** 소스·인증·키 파일 이름(`/etc/apt/sources.list.d/oplite.sources`, `/etc/apt/auth.conf.d/oplite.conf`,
  `/usr/share/keyrings/oplite-archive-keyring.gpg`)과 Release의 `Origin: opLite`·`Label: opLite`는 그대로다. apt는
  `Origin`·`Label`이 바뀌면 `apt-get update`를 오류로 끝내므로 바꾸면 노드가 새 판을 받지 못한다. 저장소 주소를 새로
  정하려면 옛 주소에서도 같은 색인을 낸다(옛 판이 쓴 소스 파일로 새 판을 받는다).
- **이미 설치한 노드의 콘솔 안내.** ISO가 쓴 `/etc/issue.d/oplite.issue`와 `/etc/opl/opld.toml` 첫 줄의 주석(예시 경로
  `/usr/share/oplite/opld.toml.example` — 이제 `/usr/share/orbivisor/`)은 패키지 파일이 아니라 옛 이름으로 남는다.
  안내의 `oplctl` 명령은 링크로 그대로 된다. 바꾸려면 노드에서(root) 한 번:
  `sed 's/opLite/Orbivisor/; s/oplctl/orbivisorctl/g' /etc/issue.d/oplite.issue > /etc/issue.d/orbivisor.issue && rm /etc/issue.d/oplite.issue`
- **그대로 둔 내부 이름.** 바꾸면 노드 위의 상태나 다른 노드와 맞지 않게 되는 이름은 옛 이름에서 왔어도 그대로다:
  데이터·설정·런타임 경로(`/var/lib/opl`, `/etc/opl`, `/run/opl`, `/var/log/opl`), 크레이트 이름(`cargo build -p opld`,
  `-p oplctl`), VM scope `opl-vm-<id>.scope`와 업그레이드 유닛 `opl-upgrade-<작업>`, 장치 이름(브리지 `oplv<VNI>`, VXLAN
  `oplx<VNI>`, LV·zvol·RBD `opl-…`), nft 테이블 `opl_<태그>`, 백업 비트맵, 쿠키 `opl_session`, 토큰·키 접두사(`opl_`,
  `oplj_`, `opl1.`, `opl-join://`, 로그인 토큰 이름 `oplctl-`), 감사 로그 대상 `opl_audit`, 지금 클러스터 CA의 CN
  ("opLite cluster X CA" — 새로 만드는 클러스터만 "Orbivisor cluster X CA"). 롤링 업그레이드 중에는 옛 노드의 오류
  문구가 옛 이름으로 보일 수 있다.
