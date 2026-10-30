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

구현: 하위 프로젝트 4는 계획 20(공유 풀 위의 라이브 마이그레이션, QEMU TLS 마이그레이션·노드 사이 RPC·`VmMigrate` Task)과 계획 21(UEFI 변수가 멈춘 채 옮긴 VM을 따라감, 디렉터리 풀의 `shared` 바꾸기)로 나눴다(`2026-10-30-live-migration-design.md`). 디스크까지 복사하는 마이그레이션과 macOS(HVF) 노드는 넣지 않았다.

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
| `opl-types` | 리소스 모델(VmSpec, VolumeSpec 등), serde 스키마 | 공개 |
| `opl-qemu` | QMP 클라이언트 + QEMU 커맨드라인 생성기 | 공개 |
| `opl-driver` | `Hypervisor`/`Storage`/`HostNet`(tap·브리지) 트레이트, Mock·QEMU(hvf/KVM/TCG)·iproute2 구현 | 공개 |
| `opl-cluster` | openraft 상태 머신, 리소스 저장소, watch 스트림, 리더 전달 | 공개 |
| `opl-peer` | 노드 간 통신(gRPC over mTLS: Raft RPC, 리더 전달, 생존 확인, 콘솔 중계), 클러스터 PKI | 공개 |
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
- 저장 형식 버전(`schema_version`)은 봉투가 아니라 저장 계층에 둔다. 노드의 redb 파일(`meta` 테이블)과 Raft 스냅샷 본문에 기록하고, 지원하지 않는 버전의 파일이나 스냅샷은 열지 않는다. 형식이 바뀌면 버전을 올리고 마이그레이션을 추가한다.
- 식별자는 숫자 VMID가 아니라 UUID를 쓴다. 다중 클러스터를 병합해도 충돌하지 않게 하기 위해서다.
- **저장 형식 호환성 규칙:** Raft에 저장되는 타입(spec/status)에 필드를 추가할 때는 반드시 `#[serde(default)]`를 붙인다. 새 필드는 클러스터의 모든 노드가 업그레이드되어 클러스터 기능 레벨이 올라간 뒤에만 기록할 수 있다. 이 규칙 덕분에 구 버전 노드는 새 필드를 만나지 않으므로, spec 타입의 `deny_unknown_fields`(API 오타 거부)를 유지할 수 있다.
- Raft 로그 엔트리(`Command`)는 버전 필드 없이 serde 태그로 구분한다. 모르는 명령은 무시하지 않고 오류로 처리하므로, 새 명령도 클러스터 기능 레벨이 올라간 뒤에만 기록한다.
- Raft 노드 id는 `Node` 리소스 id와 같은 UUID다(`opl_cluster::types::NodeId`).

### 3.2 리소스 목록

| 리소스 | spec | status |
|---|---|---|
| `Node` | 이름, 주소, 유지보수 모드 | 온라인 여부, CPU/메모리 용량, 하이퍼바이저 종류(`qemu-kvm`/`qemu-tcg`/`qemu-hvf`), Linux 브리지 목록(VLAN-aware 여부) |
| `Vm` | vCPU, 메모리, 펌웨어(bios/uefi), 머신 타입, 디스크·NIC, `power`, `node`(선택), `restart` 정책 | 실제 전원 상태, 실행 노드, QEMU PID, 오류 조건 |
| `StoragePool` | 종류(`dir`), 경로, 사용할 노드, `shared: bool` | 용량, 사용량 |
| `Volume` | 소속 풀, 크기, 형식(qcow2/raw) | 준비 상태(Pending/Ready), 할당 크기, 파일을 만드는 노드 |
| `Network` | 브리지 이름, VLAN 태그(선택). 이름 `user`는 예약(QEMU NAT, 리소스 없음) | — |
| `Task` | 작업 종류, 대상 리소스, 실행 노드 | 상태(Pending/Running/Succeeded/Failed), 오류 |
| `User`, `ApiToken` | 사용자 이름, 비밀번호 해시(argon2), 역할 / 주인, 토큰 비밀 값의 SHA-256 | — |
| `Cluster` | 클러스터 이름(리소스 이름), CA 인증서와 개인키(API로 내보내지 않는다) | — |
| `JoinToken` | 비밀 값의 SHA-256, 만료 시각 | — |

VM이 붙인 볼륨과 NIC의 MAC 주소는 클러스터 안에서 VM 하나만 쓸 수 있다. 여러 노드의 API가 동시에 써도 겹치지 않도록 상태 머신이 커밋 순서대로 확인한다(`Rejection::InUse`, API는 409).

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

`node`가 비어 있는 VM을 시작하면 스케줄러가 노드를 고른다. 대상은 온라인 상태이고, 유지보수 모드가 아니며, 디스크가 있는 풀에 접근할 수 있고, NIC가 쓰는 네트워크의 브리지가 있는(VLAN을 쓰면 VLAN-aware인) 노드다. 이 중 미할당 메모리가 가장 많은 노드를 선택한다. 선택 결과는 `spec.node`에 기록한다. 리소스 오버커밋 정책과 DRS급 재배치는 범위 밖이다.

## 4. API

- 경로는 `/api/v1/{resource}`이며, 목록·조회·생성·수정·삭제를 제공한다. 경로의 `{id}` 자리에는 id나 이름을 쓸 수 있다(UUID 모양의 이름은 받지 않는다). 목록은 `{revision, items}`를 돌려준다.
- 동작은 `POST /api/v1/vms/{id}/actions/{start|stop|reboot}`이다. 응답은 `202 Accepted`와 `Task`다.
  - `start`/`stop`은 `spec.power`를 바꾸고, 그 결과를 지켜볼 Task를 만든다. 대상 노드의 Task 실행기가 VM이 원하는 상태에 도달하면 Task를 끝낸다.
  - `stop?mode=force`는 reconciler의 VM별 작업자 큐로 강제 종료를 보낸다. `reboot`은 원하는 상태를 stopped로 바꿨다가 꺼지면 running으로 되돌린다. 그 사이 같은 VM에 새 요청이 오면 그것을 따른다.
- 볼륨 생성·삭제와 이미지 가져오기(`POST /api/v1/volumes/import`, 본문이 이미지)도 Task다. 파일이 준비되기 전의 볼륨은 VM에 붙일 수 없다.
- 감시: `GET /api/v1/watch?kinds=vm,task&since=<revision>` (WebSocket). 목록의 revision 다음 변경부터 받는다. 이어 받을 수 없으면 오류 메시지를 보내고 닫으며, 클라이언트는 목록을 다시 읽는다. 사용자와 토큰은 감시할 수 없다.
- 콘솔: `GET /api/v1/vms/{id}/console` (WebSocket). 실행 노드의 QEMU VNC로 프록시한다. 다른 노드에서 요청하면 노드 간 gRPC로 중계한다.
- 클러스터: `GET /api/v1/cluster`(이름, CA 지문, 리더, 멤버), `GET /api/v1/cluster/ca`(인증 없음), `POST /api/v1/cluster/join-tokens`(관리자), `POST /api/v1/cluster/join`(조인 토큰으로 인증), `DELETE /api/v1/nodes/{id}`(오프라인 노드만).
- 팔로워가 받은 쓰기는 리더로 전달된다. 응답은 그 쓰기가 이 노드에도 적용된 뒤에 오므로, 같은 노드에서 곧바로 읽으면 결과가 보인다.
- OpenAPI 스펙은 `utoipa`로 코드에서 생성해 `GET /api/v1/openapi.json`으로 내보낸다. 라우터와 같은 정의에서 만든다.
- 인증: 로컬 사용자(세션 쿠키, 웹 UI용)와 API 토큰(Bearer, CLI용)을 지원한다. 토큰은 `opl_<id>_<비밀 값>`이고 비밀 값의 SHA-256만 저장한다. 쿠키로 인증한 요청에 `Origin`이 있으면 서버와 같아야 한다. 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고, 바꾸면 그 사용자의 세션이 끝난다. 모든 핸들러는 `Authorizer::check(principal, action, resource)`를 거친다. 1차 구현은 `admin`/`operator`/`viewer` 고정 역할이다.
- 오류 응답은 `{ "code": "...", "message": "...", "details": {...} }` 형식으로 통일한다. Raft 과반수가 없으면 `503 cluster_no_quorum`, revision이 다르면 `412 revision_mismatch`, 이름이 겹치면 `409 name_taken`을 반환한다.

## 5. VM 라이프사이클

### 5.1 시작 흐름

```
클라이언트 ──POST /vms/web-01/actions/start──▶ opl-api (임의의 노드)
  1. 인증·권한 확인 → spec.power = running, node가 비어 있으면 스케줄러가 선택
  2. spec 변경을 Raft에 커밋 → Task(Pending, node = 대상 노드)를 따로 커밋 → 202 + task
                                │ 커밋 스트림
                                ▼
대상 노드의 reconciler
  3. desired(running) ≠ actual(stopped) → Hypervisor::start
  4. 커맨드라인 생성 → QEMU 실행(-S, -daemonize) → QMP 연결 → qmp_capabilities → cont
  5. status(running, pid)를 Raft에 기록
대상 노드의 Task 실행기
  6. VM의 power와 phase가 모두 running이 되면 Task Succeeded (reconciler가 포기하면 Failed)
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
| `QemuHypervisor` | Linux(KVM, 없으면 TCG), macOS(hvf) — `HostProfile`로 구분 | 제품, 개발·통합 테스트 |
| `MockHypervisor` | 전체 | 단위·컴포넌트 테스트, 장애 주입(`fail_next_start`, `crash`, `guest_shutdown`) |

QEMU 커맨드라인 생성은 순수 함수 `fn build_cmdline(&VmRuntimeSpec, &HostProfile, &RuntimePaths) -> Result<Vec<String>, CmdlineError>`로 만들고, `insta` 스냅샷으로 검증한다.

### 5.3 reconciler 규칙

- **트리거:** Raft 커밋 이벤트, QMP 이벤트, 30초 주기 전체 재동기화.
- **VM별 직렬화:** VM마다 작업 큐를 하나 두고, 서로 다른 VM은 병렬로 처리한다.
- **멱등성:** 모든 단계는 다시 실행해도 안전하다. 이미 원하는 상태면 아무것도 하지 않는다.
- **종료:** 원하는 상태가 `stopped`면 ACPI 종료를 요청하고, `acpi_timeout`(기본 60초) 안에 꺼지지 않으면 강제 종료한다.
- **게스트 내부 종료:** reconciler가 요청하지 않은 종료 이벤트가 오면(원하는 상태가 `running`인데 꺼짐) `spec.power`를 `stopped`로 바꾼다. QMP의 `guest_initiated`는 opld가 요청한 ACPI 종료에서도 true이므로 그것만으로 판단하지 않는다. 우리가 요청하지 않은 `guest_initiated: false` 종료(호스트의 SIGTERM 등)는 게스트의 결정이 아니므로 원하는 상태를 바꾸지 않는다. 방금까지 돌던 VM이 사라졌는데 이벤트가 아직 없으면 잠시 기다려, 게스트가 끈 VM을 다시 켜지 않게 한다.
- **크래시와 기동 실패:** `restart: on-failure`이면 지수 백오프로 재시작하고, 10분 안에 5회 실패하면 포기한다. `restart: never`의 크래시, 영구 기동 오류(이미지 없음, 잘못된 참조 등), 포기는 모두 `spec.power`를 `stopped`로 바꾸고 status를 `Failed`와 오류 메시지로 남긴다. 사용자가 다시 시작하면 처음부터 센다.
- **응답 없는 VM:** 살아 있지만 QMP로 다룰 수 없는 VM은 끄지 않고 `start()`로 다시 붙기를 계속 시도하며 status에 그 사실을 남긴다. 원하는 상태가 stopped면 ACPI 요청이 실패해도 `acpi_timeout` 뒤 강제 종료한다.
- **배정되지 않은 VM:** 이 노드에서 돌지만 이 노드에 배정되지 않은(삭제되었거나 다른 노드로 옮겨진) VM은 끄지 않고 경고만 남긴다. 저장소가 비거나 뒤처져도 VM을 죽이지 않기 위해서다. 실행 중인 VM의 삭제와 노드 변경은 API가 거부한다.
- **status 쓰기:** reconciler는 자기 노드의 VM status만 기록한다. Raft 과반수가 없어 쓰지 못하면 실제 상태에서 status를 다시 계산해 회복될 때까지 다시 쓴다(디스크 버퍼는 두지 않는다).

### 5.4 opld 재시작과 크래시 복구

- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서는 `systemd-run --scope --unit=opl-vm-<id>`로 감싸 VM마다 cgroup을 나눈다(`[hypervisor] systemd_scope`, systemd가 없는 환경에서는 끈다). cgroup 자원 제한은 아직 걸지 않는다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다. 구현: 변수 파일은 노드 로컬에 그대로 둔다. 라이브 마이그레이션에서는 QEMU가 pflash 내용을 옮겨 대상 노드의 파일에 쓰고(계획 20, CI에서 확인), 멈춘 채 옮긴 VM은 시작할 때 변수를 가진 노드(`VmStatus.firmware_node`)에서 노드 사이 RPC로 받아 온다(계획 21에서 구현, 라이브 마이그레이션 설계 §5). 그 노드가 죽었으면 템플릿으로 켠다.
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
- `opld`가 시작되면 런타임 디렉터리를 훑어 살아 있는 QEMU에 QMP로 다시 연결한다. 그 뒤 Raft의 원하는 상태와 비교해 조정한다.
- QEMU의 생존 여부는 QEMU가 수명 내내 잡고 있는 pidfile의 fcntl 잠금으로 판정한다(QMP 응답 여부와 무관하며, SIGSTOP 상태에서도 정확하다). 잠금이 풀린 런타임 디렉터리만 지우고, 잠금 소유자 pid(F_GETLK)를 확인한 뒤에만 강제 종료한다. 살아 있지만 QMP에 붙지 못한 VM은 등록하지 않고 오류를 기록하며, 이후 `start()`가 재부착을 시도한다.
- macOS에서는 `-daemonize`의 fork가 Objective-C 런타임 검사에 걸려 크래시할 수 있으므로 `OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES`로 실행한다.
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
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다. 구현: 공유 스토리지 설계(계획 15~19)부터 배치와 Task 노드 선택에 쓰고(`is_shared`, NFS·RBD·공유 LVM은 늘 공유), 라이브 마이그레이션(계획 20)은 디스크가 모두 공유 풀일 때만 한다(아니면 409 `migration_unsupported`, 디스크 복사는 하지 않는다). 디렉터리 풀의 `shared` 바꾸기는 계획 21에서 구현했다(`PATCH /api/v1/storage-pools/{key}`, `oplctl pool set`, 공유 → 로컬은 다른 노드의 VM이 쓰면 409 `pool_in_use`).
- 사용 중인 Volume은 삭제할 수 없다(409).

## 7. 네트워크 (1차)

- `user`: QEMU user-mode NAT. macOS 개발과 데모용이다. 리소스 없이 예약된 이름으로 쓴다.
- 브리지 네트워크(`Network` 리소스): 관리자가 미리 만든 Linux 브리지에 tap 장치를 연결한다. VLAN 태그는 선택 사항이고, 쓰려면 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다. tap은 그 VLAN만 태그 없이 주고받는다(pvid untagged, 브리지 기본 VLAN 제거).
- tap은 opld가 QEMU를 띄우기 전에 iproute2(`ip`, `bridge`)로 만들고, QEMU가 끝나면(정상 종료, 크래시, 강제 종료, 기동 실패) 지운다. 이름은 VM id의 뒷부분과 NIC 순번(`op<10자리>n<순번>`, 15자 이하)이라 opld가 다시 시작해도 VM의 tap을 찾을 수 있다. 그래서 Linux의 opld는 root(CAP_NET_ADMIN)로 돈다. VM 하나의 NIC는 16개까지다.
- 노드는 브리지 목록을 시작할 때와 1분마다 알리고(바뀔 때만 기록), 스케줄러가 쓴다.
- 브리지 생성과 관리, SDN, 방화벽은 하위 프로젝트 6에서 다룬다.

## 8. 클러스터 부트스트랩과 보안

```
node1$ oplctl cluster init --name prod          # 이 노드의 opld에 제어 소켓으로 요청한다
       → 클러스터 CA 생성, node1 인증서 발급, 1인 Raft 클러스터 시작, admin 계정 생성
       → admin 비밀번호(한 번만), CA 지문, 조인 URL 출력: opl-join://node1:8006?token=…&ca=sha256:…

node2$ oplctl cluster join 'opl-join://…'
       → CA 지문 검증 → 토큰으로 CSR 서명 받기 → learner로 합류 → 로그 따라잡은 뒤 voter로 승격
```

- 노드 인증서(`{data_dir}/pki`)가 없는 opld는 클러스터에 속하지 않은 노드다. 제어 소켓(`{run_dir}/opld.sock`, 0600)만 열고 `init`이나 `join`을 기다린다. init은 인증서를 마지막에 저장하므로, 도중에 실패하면 노드는 미초기화로 남고 남은 클러스터 저장소는 다음 시도에서 지운다. join은 learner가 되자마자(승격 전에) 저장하므로, 그 뒤에 실패한 노드는 멤버로 남고 다시 시작할 때마다 승격을 요청한다.
- 조인 토큰은 일회용이고 유효 시간이 정해져 있다(기본 1시간, 최대 7일). Raft에는 해시만 저장한다. 더 필요하면 관리자가 `oplctl cluster join-token`으로 만든다. 합류를 받는 노드는 토큰을 지워 한 번만 쓰게 하고, CSR에서 공개키만 써서 서명한다(이름, 용도, 기간은 서명하는 쪽이 정한다). 노드 인증서의 CN은 노드 id다.
- CA 개인키는 Raft에 복제한다(`Cluster` 리소스). 어느 노드든 합류를 받을 수 있고, 첫 노드가 죽어도 노드를 더할 수 있다. Proxmox(pmxcfs)와 같은 방식이다.
- 노드 간 통신(기본 8007)은 gRPC(tonic) over mTLS(rustls)다. `.proto` 대신 Rust 타입을 serde JSON으로 보내므로 `protoc`가 필요 없다. 서비스는 Raft RPC, 리더 전달, 생존 확인(Ping), 콘솔 중계다. 서버는 상대 인증서의 노드 id가 지금 멤버인지 확인해 제거된 노드를 막는다.
- 외부 API(기본 8006)는 클러스터 CA가 서명한 노드 인증서로 HTTPS를 연다. `oplctl`은 처음 로그인할 때 CA 지문(`--ca-fingerprint`)으로 서버를 확인하고 CA를 저장한다. 클라이언트 쪽 TLS는 OS 구현(native-tls)이다. macOS는 유효 기간이 825일을 넘는 서버 인증서를 거부하므로 노드 인증서는 824일짜리이고, 만료 전에 갱신해야 한다. 사용자 인증서로 교체하는 것은 나중에 한다. 세션 쿠키에는 `Secure`를 붙인다.
- 리더는 2초마다 다른 노드에 Ping을 보내 10초 동안 응답이 없으면 `NodeStatus.online`을 false로 기록한다(바뀔 때만 기록한다). 스케줄러가 이 값을 본다.
- `oplctl cluster leave`(정상 탈퇴)는 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다. Node 리소스를 지우고 멤버십에서 빠진 뒤, 클러스터 저장소와 노드 인증서를 지우고 미초기화 상태로 돌아간다. `oplctl cluster remove-node --force`(죽은 노드 제거)는 오프라인 노드만 뺀다. 멤버십 변경은 openraft의 joint consensus로 처리한다.
- 2노드 클러스터는 만들 수 있지만 `oplctl cluster status`가 경고를 표시한다. 위트니스는 하위 프로젝트 5에서 다룬다.

## 9. 범위 밖 (1차)

HA와 펜싱, 라이브 마이그레이션, 스냅샷과 백업, 공유 스토리지, 브리지 관리와 SDN, 웹 UI, LDAP/SSO, 세분화된 RBAC, 라이선스 시스템, 설치 ISO, 메트릭 수집과 저장.

## 10. 테스트 전략

| 계층 | 대상 | 도구 | 환경 |
|---|---|---|---|
| 단위 | 커맨드라인 생성, QMP 파싱, 스케줄러, 상태 머신 적용 | `cargo test`, `insta` | macOS, CI |
| 컴포넌트 | reconciler + Mock 드라이버: 이벤트 누락·중복·순서 바뀜 | tokio 테스트, 짧게 줄인 시간 설정 | macOS, CI |
| 클러스터 | 한 프로세스 안의 3노드(인메모리 네트워크), 리더 장애, 네트워크 분할 | openraft 테스트 하네스 | macOS, CI |
| 통합 | 실제 QEMU: 기동, QMP, 종료, opld 재시작 후 재연결 | `--features qemu-it` | macOS(hvf), Linux(KVM/TCG) |
| 통합 | 실제 tap·브리지·VLAN, QEMU NIC 연결과 정리, systemd 스코프 | `--features qemu-it,net-it`(root), `OPL_TEST_SYSTEMD=1` | Linux |
| E2E | oplctl로 VM 생성 → 콘솔 → opld kill -9 → 재연결 (opld 프로세스 + oplctl 라이브러리) | `--features qemu-it` | macOS(hvf) (M5) |
| E2E | oplctl로 클러스터 구성 → 아무 노드로 쓰기 → 리더 kill -9 → 노드 제거·탈퇴·재합류 (opld 프로세스 3개) | `cargo test` | macOS, CI (M6) |
| E2E | oplctl로 네트워크 생성 → 브리지 NIC VM 시작 → tap·VLAN 확인 → 끄면 tap 제거, 위 E2E를 Linux에서 | `--features qemu-it,net-it`(root) | Linux: Docker 컨테이너(TCG), CI(KVM) (M7) |

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

M0부터 M6까지는 macOS에서 진행한다. M7은 Linux에서 확인한다. 개발 Mac(M1)은 중첩 가상화가 없어 로컬은 privileged Docker 컨테이너에서 TCG로, KVM은 CI(GitHub Linux 러너)에서 확인한다.

## 12. 주요 의존성

| 용도 | 크레이트 |
|---|---|
| 비동기 런타임 | `tokio` |
| HTTP/WebSocket | `axum`, `tower` |
| 노드 간 RPC | `tonic`(수동 서비스 정의, serde JSON 메시지) |
| 합의 | `openraft` |
| 내장 저장소 | `redb` |
| TLS/PKI | `rustls`(ring), `rcgen`, 클라이언트는 `native-tls` |
| 직렬화 | `serde`, `serde_json`, `serde_yaml` |
| OpenAPI | `utoipa` |
| 오류 | `thiserror`(라이브러리), `anyhow`(바이너리) |
| CLI | `clap` |
| 로깅 | `tracing`, `tracing-subscriber` |
| 비밀번호 해시 | `argon2` |
| 테스트 | `insta`, `proptest` |

라이선스 정책: 공개 코어는 **Apache-2.0**으로 배포한다(2026-10-20 결정). 의존성은 Apache-2.0과 호환되는 라이선스(MIT, BSD, ISC 등)만 허용하고, CI에서 `cargo-deny`로 검사한다. QEMU(GPL)는 별도 프로세스로 실행하므로 링크하지 않는다.
