# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀)

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

## 실행 (Linux, KVM)

opld는 root로 돈다(VM NIC의 tap 장치를 만든다). QEMU는 VM마다 `opl-vm-<id>.scope`로 감싼다.
KVM(`/dev/kvm`)이 없으면 TCG로 돌고 경고를 남긴다.

```bash
sudo apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils iproute2
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
  노드에만 배치된다. 공유 스토리지 위의 디렉터리 풀은 `--shared`로 만들어야 VM이 다른 노드로 갈 수 있다. 풀을 바꾸는
  API는 없으므로 기존 풀을 공유로 바꾸려면 볼륨을 다른 풀로 옮기고(예: 복제) 풀을 다시 만든다.

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

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
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
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
# Linux, root: 실제 tap과 브리지
# root가 만든 빌드 결과가 ./target에 섞이지 않게 따로 둔다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opl-driver --features qemu-it,net-it   # tap·브리지·VLAN
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
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
