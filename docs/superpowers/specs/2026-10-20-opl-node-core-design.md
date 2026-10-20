# opLite 1차 하위 프로젝트 설계: 노드 코어 (클러스터 상태 + VM 라이프사이클)

- 작성일: 2026-10-20
- 상태: 설계 승인됨, 구현 계획 작성 전
- 범위: 전체 제품 중 첫 번째 하위 프로젝트

## 1. 제품 맥락

opLite는 Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼이다. Proxmox VE, VMware vSphere와 같은 영역의 제품이다.

| 항목 | 결정 |
|---|---|
| 1차 목표 시장 | VMware 이탈 기업(중소·중견), 3~32노드 온프레미스 클러스터 |
| 사업 모델 | 오픈코어: 코어 공개 + 엔터프라이즈 플러그인과 기술지원 구독 |
| 팀과 일정 | 소규모 팀(3~8명), 12개월 안에 파일럿 고객 확보 |
| 개발 환경 | 당분간 macOS(Apple Silicon)만 사용. Linux/KVM 환경은 나중에 확보 |
| 아키텍처 | 대칭형 노드 + 내장 Raft (별도 관리 서버 없음) |
| 하이퍼바이저 | QEMU/KVM을 QMP로 직접 제어 (libvirt 사용 안 함) |

### 1.1 전체 제품 분해

| # | 하위 프로젝트 | 대응 기능 (Proxmox / VMware) |
|---|---|---|
| 1 | **노드 코어: 클러스터 상태 + VM 라이프사이클 (이 문서)** | qemu-server, pmxcfs / hostd |
| 2 | 웹 UI | PVE GUI / vSphere Client |
| 3 | 공유 스토리지와 스냅샷 (NFS, iSCSI, Ceph, LVM, ZFS) | pve-storage / VMFS, vSAN |
| 4 | 라이브 마이그레이션 | qm migrate / vMotion |
| 5 | HA와 펜싱, 2노드 위트니스 | ha-manager / vSphere HA |
| 6 | 네트워크와 SDN (브리지 관리, VXLAN, 방화벽) | pve-network / NSX |
| 7 | 백업 | PBS / VADP |
| 8 | VMware 마이그레이션 마법사 | Import Wizard |
| 9 | 설치 ISO, 업데이트 채널, 라이선스, 텔레메트리 | 구독 저장소 / vLCM |
| 10 | 엔터프라이즈 플러그인 (LDAP/SSO, 감사, 다중 클러스터 관리, 고급 DR) | — / vCenter 기능 |

하위 프로젝트마다 별도로 설계 → 계획 → 구현 사이클을 거친다. 위 순서가 구현 권장 순서다.

### 1.2 검토했지만 채택하지 않은 대안

- **중앙 관리 서버 + 경량 에이전트 (vCenter 방식):** 관리 서버 자체의 HA를 따로 풀어야 하고 설치 단계가 늘어난다. 이 복잡함이 VMware를 떠나는 이유 중 하나다.
- **Kubernetes 기반 (KubeVirt 방식):** 운영 복잡도가 목표 고객과 맞지 않는다.
- **Cloud Hypervisor (Rust VMM):** Windows와 레거시 장치 지원이 부족하다. 대신 `Hypervisor` 트레이트를 통해 나중에 경량 VM 백엔드로 추가할 수 있게 열어 둔다.

## 2. 아키텍처와 프로세스 모델

```
┌──────────────────── 노드 (Linux 호스트, Debian 기반 어플라이언스) ────────────────────┐
│                                                                                      │
│  opld (단일 바이너리, tokio)                                                          │
│  ├─ api        : HTTPS REST + WebSocket (axum)   ← UI/CLI/Terraform이 접속             │
│  ├─ cluster    : openraft 상태 머신 (redb 저장)  ← 클러스터 설정의 단일 진실 원천      │
│  ├─ reconciler : 원하는 상태와 실제 상태를 맞추는 조정 루프 (노드별)                   │
│  └─ drivers    : Hypervisor / Storage / Network 트레이트 구현체                        │
│                                                                                      │
│  qemu-system-* (VM 1개당 프로세스 1개, systemd scope로 격리, QMP 소켓으로 제어)       │
└──────────────────────────────────────────────────────────────────────────────────────┘
          ▲ 노드 간 통신: gRPC(tonic) over mTLS — Raft 메시지 + 노드 간 작업 전달
```

### 2.1 핵심 원칙

1. **선언형 + 조정 루프.** API는 원하는 상태(spec)를 Raft에 기록하기만 한다. 각 노드의 reconciler가 자기 노드에 배정된 리소스를 실제 상태로 맞춘다.
2. **작업(Task) 모델.** 오래 걸리는 작업은 모두 Task ID를 반환하고, 진행률과 로그를 WebSocket으로 스트리밍한다.
3. **Raft에는 설정만 넣는다.** 메트릭 같은 고빈도 데이터는 노드 로컬에 저장하고, 노드 간 조회는 gRPC로 한다.
4. **단일 노드도 1인 클러스터다.** 단일 노드 모드라는 별도 코드 경로를 두지 않는다.

### 2.2 크레이트 구조 (Cargo 워크스페이스)

| 크레이트 | 역할 | 공개 여부 |
|---|---|---|
| `opl-types` | 리소스 모델(VmSpec, NodeId 등), serde 스키마 | 공개 |
| `opl-qemu` | QMP 클라이언트 + QEMU 커맨드라인 생성기 | 공개 |
| `opl-driver` | `Hypervisor`/`Storage`/`Network` 트레이트, Mock·QemuHvf·QemuKvm 구현 | 공개 |
| `opl-cluster` | openraft 상태 머신, 리소스 저장소, watch 스트림 | 공개 |
| `opl-reconciler` | 조정 루프, VM별 작업 큐 | 공개 |
| `opl-api` | REST/WebSocket, 인증, `Authorizer` 훅 | 공개 |
| `opld` | 데몬 바이너리 조립 | 공개 |
| `oplctl` | CLI | 공개 |
| `opl-ent-*` | 엔터프라이즈 플러그인 | 비공개 (별도 저장소) |

오픈코어 경계는 크레이트 단위로 나눈다. 엔터프라이즈 기능은 공개 크레이트가 정의한 트레이트(`Authorizer`, `AuthProvider` 등)를 구현하는 비공개 크레이트로 제공하고, `opld` 빌드 시 기능 플래그로 연결한다. 공개 크레이트는 비공개 크레이트에 의존하지 않는다.

## 3. 리소스 데이터 모델

### 3.1 공통 봉투

```rust
struct Resource<S, T> {
    id: Uuid,                          // UUIDv7, 변경 불가
    name: String,                      // 종류별로 클러스터 내 유일
    revision: u64,                     // 마지막 수정의 Raft 커밋 인덱스
    labels: BTreeMap<String, String>,
    spec: S,                           // 원하는 상태: API로만 수정
    status: T,                         // 실제 상태: reconciler만 기록
}
```

- spec과 status를 분리해 사용자 쓰기와 시스템 쓰기가 충돌하지 않게 한다.
- `revision`은 낙관적 동시성 제어에 쓴다. 클라이언트가 `If-Match: <revision>`을 보냈는데 값이 다르면 412를 반환한다.
- 저장 형식에는 `schema_version`을 둔다. 업그레이드 시 Raft 스냅샷을 마이그레이션하기 위해서다.
- 식별자는 숫자 VMID가 아니라 UUID를 쓴다. 다중 클러스터를 병합해도 충돌하지 않게 하기 위해서다.
- **저장 형식 호환성 규칙:** Raft에 저장되는 타입(spec/status)에 필드를 추가할 때는 반드시 `#[serde(default)]`를 붙인다. 새 필드는 클러스터의 모든 노드가 업그레이드되어 클러스터 기능 레벨이 올라간 뒤에만 기록할 수 있다. 이 규칙 덕분에 구 버전 노드는 새 필드를 만나지 않으므로, spec 타입의 `deny_unknown_fields`(API 오타 거부)를 유지할 수 있다.
- `schema_version`을 봉투 안에 둘지 저장 계층(Raft 엔트리 헤더)에 둘지는 계획 3(opl-cluster)에서 결정한다.

### 3.2 리소스 목록

| 리소스 | spec | status |
|---|---|---|
| `Node` | 이름, 주소, 유지보수 모드 | 온라인 여부, CPU/메모리 용량, 하이퍼바이저 종류와 기능 |
| `Vm` | vCPU, 메모리, 펌웨어(bios/uefi), 머신 타입, 디스크·NIC, `power`, `node`(선택), `restart` 정책 | 실제 전원 상태, 실행 노드, QEMU PID, 오류 조건 |
| `StoragePool` | 종류(`dir`), 경로, 사용할 노드, `shared: bool` | 용량, 사용량 |
| `Volume` | 소속 풀, 크기, 형식(qcow2/raw) | 할당 크기, 연결된 VM |
| `Network` | 종류(`user`/`bridge`), 브리지 이름, VLAN 태그 | — |
| `Task` | 종류, 대상 리소스 | 상태(Pending/Running/Succeeded/Failed), 진행률, 로그 위치 |
| `User`, `ApiToken` | 사용자 이름, 비밀번호 해시(argon2), 토큰 해시, 역할 | 마지막 사용 시각 |

`Vm` spec 예시:

```yaml
name: web-01
spec:
  cpu: { sockets: 1, cores: 4, model: host }
  memory_mib: 8192
  firmware: uefi
  machine: q35            # macOS(hvf)에서는 virt
  disks:
    - { volume: web-01-root, bus: virtio-blk, boot_order: 1 }
  nics:
    - { network: lan, model: virtio }   # mac을 생략하면 자동 할당
  power: running          # running | stopped
  restart: on-failure     # never | on-failure
  node: null              # null이면 스케줄러가 배치
```

### 3.3 스케줄러 (1차)

`node`가 비어 있는 VM을 시작하면 스케줄러가 노드를 고른다. 대상은 온라인 상태이고, 유지보수 모드가 아니며, 디스크가 있는 풀에 접근할 수 있는 노드다. 이 중 미할당 메모리가 가장 많은 노드를 선택한다. 선택 결과는 `spec.node`에 기록한다. 리소스 오버커밋 정책과 DRS급 재배치는 범위 밖이다.

## 4. API

- 경로는 `/api/v1/{resource}`이며, 목록·조회·생성·수정·삭제를 제공한다.
- 동작은 `POST /api/v1/vms/{id}/actions/{start|stop|reboot}`이다. 응답은 `202 Accepted`와 `Task`다.
  - `start`/`stop`은 `spec.power`를 바꾸는 것뿐이다.
  - `stop?mode=force`와 `reboot`은 일회성 명령이다. Task로 대상 노드에 전달한다.
- 감시: `GET /api/v1/watch?kinds=vm,task` (WebSocket). Raft 커밋 스트림을 구독한다. 연결이 끊긴 뒤에는 `since=<revision>`으로 이어 받는다.
- 콘솔: `GET /api/v1/vms/{id}/console` (WebSocket). 실행 노드의 QEMU VNC로 프록시한다. 다른 노드에서 요청하면 노드 간 gRPC로 중계한다.
- OpenAPI 스펙은 `utoipa`로 코드에서 생성한다.
- 인증: 로컬 사용자(세션 쿠키)와 API 토큰(Bearer)을 지원한다. 모든 핸들러는 `Authorizer::check(principal, action, resource)`를 거친다. 1차 구현은 `admin`/`operator`/`viewer` 고정 역할이다.
- 오류 응답은 `{ "code": "...", "message": "...", "details": {...} }` 형식으로 통일한다. Raft 과반수가 없으면 `503 cluster_no_quorum`을 반환한다.

## 5. VM 라이프사이클

### 5.1 시작 흐름

```
클라이언트 ──POST /vms/web-01/actions/start──▶ opl-api (임의의 노드)
  1. 인증·권한 확인 → spec.power = running, node가 비어 있으면 스케줄러가 선택
  2. Raft에 제안 → 과반수 커밋 → Task(Pending) 생성 → 202 + task_id
                                  │ 커밋 스트림
                                  ▼
대상 노드의 reconciler
  3. desired(running) ≠ actual(stopped) → Hypervisor::start
  4. 커맨드라인 생성 → systemd scope로 QEMU 실행(-S) → QMP 연결 → qmp_capabilities → cont
  5. status(running, pid)를 Raft에 기록 → Task Succeeded
```

### 5.2 드라이버 트레이트

```rust
#[async_trait]
trait Hypervisor: Send + Sync {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError>;       // 이미 실행 중이면 성공
    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError>;  // Acpi | Force, 실행 중이 아니면 성공
    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError>;
    async fn list(&self) -> Result<Vec<ActualState>, DriverError>;
    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError>; // VNC 유닉스 소켓
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;                // Started | Stopped | Crashed
}
```

| 구현 | 플랫폼 | 용도 |
|---|---|---|
| `QemuHypervisor` | Linux(KVM), macOS(hvf) — `HostProfile`로 구분 | 제품, 개발·통합 테스트 |
| `MockHypervisor` | 전체 | 단위·컴포넌트 테스트, 장애 주입(`fail_next_start`, `crash`, `guest_shutdown`) |

QEMU 커맨드라인 생성은 순수 함수 `fn build_cmdline(&VmRuntimeSpec, &HostProfile, &RuntimePaths) -> Result<Vec<String>, CmdlineError>`로 만들고, `insta` 스냅샷으로 검증한다.

### 5.3 reconciler 규칙

- **트리거:** Raft 커밋 이벤트, QMP 이벤트, 30초 주기 전체 재동기화.
- **VM별 직렬화:** VM마다 작업 큐를 하나 두고, 서로 다른 VM은 병렬로 처리한다.
- **멱등성:** 모든 단계는 다시 실행해도 안전하다. 이미 원하는 상태면 아무것도 하지 않는다.
- **게스트 내부 종료:** QMP `SHUTDOWN` 이벤트가 게스트가 시작한 종료이면 `spec.power`를 `stopped`로 바꾼다.
- **크래시:** QEMU가 비정상 종료하면 status에 오류 조건을 기록한다. `restart: on-failure`이면 지수 백오프로 재시작하고, 10분 안에 5회 실패하면 중단한다.
- **status 쓰기:** reconciler는 자기 노드의 VM status만 기록한다. Raft 과반수가 없을 때는 로컬에 버퍼링했다가 회복되면 다시 기록한다.

### 5.4 opld 재시작과 크래시 복구

- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서 cgroup 자원 제어를 위해 `systemd-run --scope`로 감싸는 것은 M7에서 추가한다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다.
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
- `opld`가 시작되면 런타임 디렉터리를 훑어 살아 있는 QEMU에 QMP로 다시 연결한다. 그 뒤 Raft의 원하는 상태와 비교해 조정한다.
- 소켓 연결 자체가 거부될 때만 죽은 VM으로 보고 런타임 디렉터리를 지운다. 그 밖의 오류나 타임아웃에서는 살아 있는 VM일 수 있으므로 아무것도 지우지 않고 오류를 기록한다.
- 이 설계 덕분에 `opld`를 업그레이드하거나 재시작해도 실행 중인 VM은 중단되지 않는다.

### 5.5 오류 처리

- `DriverError`는 `thiserror` 열거형이다. `is_transient()`로 재시도 가능 여부를 구분한다.
  - 일시적 오류(QMP 타임아웃, 소켓 일시 오류): 지수 백오프로 재시도한다.
  - 영구 오류(이미지 없음, 메모리 부족, 잘못된 spec): status 조건에 기록하고 Task를 Failed로 끝낸다.
- Raft 과반수를 잃으면 API 쓰기는 503이 되지만, 실행 중인 VM은 유지된다. 클러스터 장애가 워크로드 장애로 번지지 않게 하기 위해서다.

## 6. 스토리지 (1차: `dir` 풀)

```rust
#[async_trait]
trait Storage: Send + Sync {
    async fn create_volume(&self, pool: &PoolSpec, vol: &VolumeSpec) -> Result<VolumePath, DriverError>;
    async fn delete_volume(&self, pool: &PoolSpec, id: Uuid) -> Result<(), DriverError>;
    async fn import(&self, pool: &PoolSpec, src: &Path, fmt: ImageFormat) -> Result<VolumePath, DriverError>;
    async fn capacity(&self, pool: &PoolSpec) -> Result<Capacity, DriverError>;
}
```

- `dir` 풀은 로컬 디렉터리에 qcow2/raw 파일을 둔다. 작업은 `qemu-img`로 처리한다.
- `import`는 `qemu-img convert`로 VMDK, qcow2, raw 이미지를 가져온다.
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다.
- 사용 중인 Volume은 삭제할 수 없다(409).

## 7. 네트워크 (1차)

- `user`: QEMU user-mode NAT. macOS 개발과 데모용이다.
- `bridge`: 관리자가 미리 만든 Linux 브리지에 tap 장치를 연결한다. VLAN 태그는 선택 사항이다.
- 브리지 생성과 관리, SDN, 방화벽은 하위 프로젝트 6에서 다룬다.

## 8. 클러스터 부트스트랩과 보안

```
node1$ oplctl cluster init --name prod
       → 클러스터 CA 생성, node1 인증서 발급, 1인 Raft 클러스터 시작, admin 계정 생성
       → 조인 토큰 출력: opl-join://node1:8006?token=…&ca_fp=sha256:…

node2$ oplctl cluster join 'opl-join://…'
       → CA 지문 검증 → 토큰으로 CSR 서명 받기 → learner로 합류 → 로그 따라잡은 뒤 voter로 승격
```

- 조인 토큰은 일회용이고 유효 시간이 정해져 있다(기본 1시간). Raft에는 해시만 저장한다.
- 노드 간 통신은 모두 mTLS(rustls)를 쓴다. 외부 API(8006)는 기본적으로 클러스터 CA 인증서를 쓰고, 사용자 인증서로 교체할 수 있다.
- `oplctl cluster leave`(정상 탈퇴)와 `oplctl cluster remove-node --force`(죽은 노드 제거)를 지원한다. 멤버십 변경은 openraft의 joint consensus로 처리한다.
- 2노드 클러스터는 생성할 수 있지만 경고를 표시한다. 위트니스는 하위 프로젝트 5에서 다룬다.

## 9. 범위 밖 (1차)

HA와 펜싱, 라이브 마이그레이션, 스냅샷과 백업, 공유 스토리지, 브리지 관리와 SDN, 웹 UI, LDAP/SSO, 세분화된 RBAC, 라이선스 시스템, 설치 ISO, 메트릭 수집과 저장.

## 10. 테스트 전략

| 계층 | 대상 | 도구 | 환경 |
|---|---|---|---|
| 단위 | 커맨드라인 생성, QMP 파싱, 스케줄러, 상태 머신 적용 | `cargo test`, `insta` | macOS, CI |
| 컴포넌트 | reconciler + Mock 드라이버: 이벤트 누락·중복·순서 바뀜 | tokio 테스트, 가짜 시계 | macOS, CI |
| 클러스터 | 한 프로세스 안의 3노드(인메모리 네트워크), 리더 장애, 네트워크 분할 | openraft 테스트 하네스 | macOS, CI |
| 통합 | 실제 QEMU: 기동, QMP, 종료, opld 재시작 후 재연결 | `--features qemu-it` | macOS(hvf), Linux |
| E2E | oplctl로 클러스터 구성 → VM 생성 → 콘솔 → 노드 제거 | 셸 스크립트 + 소형 게스트 이미지 | Linux (M7) |

반드시 회귀 테스트로 고정할 불변식:

1. API가 성공(2xx)을 반환했다면 해당 변경은 Raft에 커밋된 것이다.
2. `opld`를 재시작해도(`kill -9` 포함) 실행 중인 VM은 중단되지 않고, 재시작 후 다시 연결된다.
3. 같은 VM에 대한 드라이버 호출은 동시에 실행되지 않는다.
4. Raft 과반수를 잃어도 실행 중인 VM은 중지되지 않는다.

## 11. 마일스톤

| M | 결과물 | 완료 기준 |
|---|---|---|
| M0 | 워크스페이스 뼈대, CI(fmt/clippy/test), `opl-types` | macOS에서 `cargo test` 통과 |
| M1 | `opl-qemu`: QMP 클라이언트 + 커맨드라인 생성기 | macOS에서 aarch64 게스트 기동, 상태 조회, 종료 |
| M2 | `opl-driver`: 트레이트 + Mock + QemuHvf + `dir` 풀 | Volume 생성, VMDK 가져오기 통합 테스트 통과 |
| M3 | `opl-cluster`: openraft + redb, 리소스 CRUD, watch | 3노드 인메모리 클러스터에서 리더 장애 후에도 쓰기 지속 |
| M4 | `opl-reconciler`: 조정 루프, VM별 큐, 재연결 | `opld` kill -9 후 VM 유지와 재연결 확인 |
| M5 | `opl-api` + `oplctl`: REST, Task, 콘솔, 토큰 인증 | CLI로 VM 생성부터 콘솔 접속까지 수행 |
| M6 | 클러스터 부트스트랩: CA, join, leave, mTLS | macOS에서 프로세스 3개로 클러스터 구성 |
| M7 | `QemuKvm` + bridge 네트워크 | Linux에서 E2E 통과 |

M0부터 M6까지는 macOS에서 진행한다. M7은 Linux/KVM 환경이 필요하다.

## 12. 주요 의존성

| 용도 | 크레이트 |
|---|---|
| 비동기 런타임 | `tokio` |
| HTTP/WebSocket | `axum`, `tower` |
| 노드 간 RPC | `tonic`, `prost` |
| 합의 | `openraft` |
| 내장 저장소 | `redb` |
| TLS/PKI | `rustls`, `rcgen` |
| 직렬화 | `serde`, `serde_json`, `serde_yaml` |
| OpenAPI | `utoipa` |
| 오류 | `thiserror`(라이브러리), `anyhow`(바이너리) |
| CLI | `clap` |
| 로깅 | `tracing`, `tracing-subscriber` |
| 비밀번호 해시 | `argon2` |
| 테스트 | `insta`, `proptest` |

라이선스 정책: 공개 코어는 **Apache-2.0**으로 배포한다(2026-10-20 결정). 의존성은 Apache-2.0과 호환되는 라이선스(MIT, BSD, ISC 등)만 허용하고, CI에서 `cargo-deny`로 검사한다. QEMU(GPL)는 별도 프로세스로 실행하므로 링크하지 않는다.
