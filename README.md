# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지)

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

풀은 두 종류다. **디렉터리 풀**(`--path`)은 노드의 디렉터리이고, **NFS 풀**(`--nfs`)은 NFS 서버의 내보내기를
opld가 노드마다 마운트한 것이다. 어느 쪽이든 볼륨은 그 안의 qcow2·raw 파일이다.

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
```

`nfs-it` 시험은 opld가 내보내기를 마운트하고, 그 풀에 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면
언마운트하는지 본다(CI `linux-kvm` 작업이 `/srv/opl-nfs`를 내보낸다). **내보내기는 버려도 되는 디렉터리**를 쓴다.
시험은 실패해도 임시 디렉터리를 지우기 전에 opld를 멈추고 마운트를 풀며, 풀지 못하면 임시 디렉터리를 지우지 않고
남긴다(그 경로를 출력한다). NFS 서버를 막 시작했으면 잠금 유예(약 90초)
때문에 첫 실행이 느릴 수 있다.

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
