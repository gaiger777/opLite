# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷)

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
  (`POST /api/v1/volumes/import/check`): 권한, 이름, 풀이 있는지, 풀이 접속한 노드에 있는지, 같은 이름의 볼륨이
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
