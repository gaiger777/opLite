# Orbivisor 제어부 가용성과 대규모 클러스터 설계 (하위 프로젝트 15: 계획 45~56)

작성일: 2026-11-11
상위 문서:
- `2026-10-20-opl-node-core-design.md`(노드 코어, Raft)
- `2026-11-02-ha-fencing-witness-design.md`(HA 리스·자기 펜싱·위트니스)
- `2026-11-11-monitoring-ui-design.md`(지표 — 계획 44)

## 1. 목표와 범위

사용자 요청(2026-11-11):
- 지표 수집(계획 44)이 끝나면 가용성을 먼저 한다.
- Proxmox VE와 같은 가용성으로 간다.
- "수백 대 노드를 추가해도 속도에 지장 없게 마스터 선출이든 뭐든" 설계에 넣는다.
- VIP를 둘 수 있어야 한다(OpenStack의 HAProxy처럼 한 노드가 문제여도 다른 노드로 서비스).
- 실행 중인 VM의 유형(vCPU·메모리·디스크·NIC)을 끄지 않고 바꾼다.
- 최적 배치(VMware DRS처럼 실제 부하를 보고 VM을 놓고 옮김)와 빠른 HA(노드가 죽은 뒤 VM이 다른 노드에서 다시 켜질 때까지의 시간을 줄임)를 넣는다.
- HA가 여러 네트워크를 골라 감시하고, SAN 등 스토리지 네트워크가 끊긴 것과 본딩된 네트워크의 포트 하나하나를 검사한다. 노드 전원 끄기(IPMI 또는 다른 방법)로 펜싱한다.

이 문서는 그 전체를 정하고, 계획 45를 자세히, 46~56을 경계까지 정한다. 하위 프로젝트 14의 나머지(차트·3D·끌어 놓는 VM 만들기)는 이 하위 프로젝트 다음으로 미루고 계획 번호를 그때 다시 매긴다(지표 설계 §1의 45~47은 57~59가 된다).

| 계획 | 내용 |
|---|---|
| 45 | 런타임 격리와 systemd watchdog: HA·Raft·API·작업을 서로 굶기지 않게 실행기를 나누고, Raft 저장소의 동기 쓰기를 비동기 작업자에서 빼고, `Type=notify`와 `WatchdogSec` |
| 46 | 헬스 체크·API 이름·내장 VIP·앞단 프록시: `GET /api/v1/health`, 클러스터 설정의 추가 인증서 이름(VIP·DNS), 선택 기능 내장 VIP, 세션 공유·믿는 프록시·허용 출처·UI 따로 내보내기(쿠버네티스 포털), README의 HAProxy·keepalived·쿠버네티스 예 |
| 47 | 대규모 1 — 리더 짐 덜기: 투표 멤버 상한(기본 5)과 자동 승격·강등, 장애 영역(`zone`), 리스 갱신의 리더 확인 묶기, 노드 감시를 리스로 바꿈, 모으기 동시성 상한, 규모 시험 틀 |
| 48 | 대규모 2 — 관찰 노드: 투표·learner가 아닌 노드는 Raft 밖에서 투표 멤버 하나로부터 상태를 스트림으로 받음(리더가 수백 노드에 복제하지 않음) |
| 49 | 클러스터 통신 다중 링크: 노드마다 주소 여러 개, 끊기면 다음 링크, 인증서 이름 |
| 50 | 배치 규칙과 HA 정책: 모든 VM에 affinity·anti-affinity·soft-affinity·soft-anti-affinity(VM끼리, VM과 노드), 노드 우선순위, 재시작·옮기기 횟수, 노드 정상 종료 때의 정책, 되돌아가기 |
| 51 | 빠른 HA와 노드 전원 끄기: 복구 시간 목표(기본 약 65초 → 약 20초), 짧은 리스·watchdog, 전원 끄기 펜싱(IPMI·Redfish·관리자 명령)으로 기다림 없이 복구, 미리 고른 복구 노드, 병렬·우선순위 재시작, HA 용량 예약, 게스트 멈춤 감지 |
| 52 | HA 상태 검사: HA가 감시할 네트워크 고르기(관리·스토리지·VM), 스토리지 네트워크와 스토리지 경로 끊김 감지 후 VM 옮기기, 본딩 포트별 검사와 경고 |
| 53 | 최적 배치: 실제 부하(계획 44 지표)로 점수를 매겨 VM을 놓기, 리더의 균형 조정기가 라이브 마이그레이션으로 옮기기(끔·추천·자동) |
| 54 | VM 무중단 유형 변경: 실행 중 vCPU 늘리기(CPU 핫플러그)·메모리 늘리기(virtio-mem)·디스크와 NIC 붙이기·떼기, 다음 재시작 때 적용할 변경의 표시 |
| 55 | 로컬 디스크 HA: 노드 사이 주기적 증분 복제와 복제본으로 HA 복구 |
| 56 | Ceph 자동 구성: 노드에 Ceph mon·mgr·OSD 설치와 상태 화면 |

넣지 않는 것:
- VM 무중단 이중화(VMware FT, QEMU COLO): 복구 시간 0은 빠른 HA의 범위 밖이다
- 쓰지 않는 노드 끄기(VMware DPM)
- 데몬을 여러 프로세스로 나누기(§1.1의 "견준 길")
- 클러스터 사이 연합(federation)
- 관리 네트워크와 VM 네트워크의 자동 분리 구성
- SDN(OVN 백엔드의 VNet·서브넷·분산 라우터·유동 IP·부하 분산): 하위 프로젝트 16으로 따로 설계한다

### 1.1 견준 길

**실행기 분리(채택) vs 프로세스 분리(Proxmox: `pveproxy`·`pvedaemon`·`pve-ha-lrm`·`watchdog-mux`).**
- 지금의 실제 위험은 "API·작업·디스크 쓰기가 HA 리스 갱신과 watchdog 쓰다듬기를 늦춰 멀쩡한 노드가 재시작된다"이다(§2).
  - 이것은 같은 실행기의 작업자 스레드를 함께 쓰기 때문이고, 실행기를 나누면 막힌다.
- Proxmox가 프로세스를 나눈 큰 이유는 Perl 데몬이 단일 스레드라서다.
- 프로세스를 나누면 API 프로세스가 클러스터 상태를 읽고 쓰는 길(로컬 RPC로 저장소 전체를 다시 감싸기)이 새로 생긴다. 지금 opl-api는 `Cluster`를 직접 쓴다.
- 남는 차이는 메모리 누수·panic 하나가 데몬 전체를 다시 시작시키는 것이다. 이것은 다음 셋이 받친다.
  - `KillMode=process`로 VM이 계속 돈다.
  - 5초 안에 다시 시작한다.
  - HA 리스의 유예(`fence_after` 30초)가 있다.
- 계획 45 뒤에 장애 주입 시험으로 다시 본다.

**관찰 노드(채택, 계획 48) vs openraft learner로 수백 노드 vs gossip(SWIM) 멤버십.**
- learner도 리더가 하트비트(200 ms)와 로그를 보내므로 노드 수만큼 리더 일이 는다. 노드 300대면 하트비트만 초당 1500개다.
- gossip은 생존 감지에는 좋지만 상태 복제를 하지 않고, 우리는 이미 리스로 생존을 안다.
- 관찰 노드는 etcd gRPC proxy·Consul 클라이언트처럼 투표 멤버 하나에서 "스냅샷 + 변경 스트림"을 받는다.
  - 리더의 일은 투표 멤버 수(5)에만 비례한다.
  - 지금의 `/watch`가 이미 리비전이 붙은 변경 스트림이라 바탕이 있다.

**투표 멤버 상한 5(채택) vs 모든 노드 투표(지금).**
- 쓰기 확정·선거가 과반에 기다리므로 투표 멤버가 늘수록 느려지고 표가 갈린다.
- etcd·Consul도 3·5를 권한다.
- 5대면 2대가 동시에 죽어도 쓴다. 7은 설정으로 고를 수 있게 한다.

**리스 기반 생존 확인(채택) vs 리더의 ping(지금 `NodeMonitor`).**
- 모든 노드는 이미 2초마다 리더에 리스를 갱신한다(HA 펜싱을 끈 노드도 하게 한다).
- ping과 리스가 같은 정보를 두 번 나른다. 리스 하나로 줄인다.

**리스 갱신의 리더 확인 묶기(채택).**
- 지금 갱신 하나마다 리더가 과반 확인(`confirm_leader`)을 두 번 한다.
- 노드 300대가 2초마다 갱신하면 초당 300번의 과반 왕복이다.
- 같은 순간에 온 갱신은 확인 한 번을 함께 쓴다. 확인 시작 뒤에 도착한 요청은 다음 확인을 기다린다. 그래야 "보내기 전 시각 ≤ 확인 시각" 규칙이 지켜진다.

## 2. 지금 있는 것 (계획 44 끝)

- **실행기:** `#[tokio::main]` 멀티 스레드 하나(`opld/src/main.rs`)에서 모두가 `tokio::spawn`으로 돈다(`member.rs`).
  - Raft(openraft 0.9, `storage-v2`)
  - 피어 gRPC 서버
  - API(axum)
  - `Reconciler`, `TaskRunner`
  - `HaGuard`(리스 갱신 2초, watchdog 1초), `NodeMonitor`(리더가 2초마다 ping, 10초면 오프라인), `HaManager`(리더, 1초)
  - 스케줄러들, 지표 측정기
- **동기 쓰기:** Raft 로그·상태 기계 저장소(redb)의 `begin_write`·`commit`(fsync)이 `async fn` 안에서 동기로 돈다(`opl-cluster/src/log_store.rs`, `state_machine.rs`). `HaGuard`는 `std::sync::Mutex`를 쥔 채 watchdog `write(2)`를 한다.
- **Raft 설정:** 하트비트 200 ms, 선거 1000~2000 ms(`member.rs` `raft_config`)이다.
  - 구성원은 `BasicNode { addr }`(주소 하나, `address:8007`)이다.
  - 위트니스가 아닌 노드는 모두 투표 멤버로 승격한다(`promote_until_voter`, 상한 없음).
- **리스:** `HaGuard::renew_loop`가 `Cluster::renew_lease`로 리더에 갱신한다(리더가 아니면 피어 RPC로 전달). 리더의 `LeaseTable::renew`는 갱신마다 `confirm_leader`를 두 번 한다(`opl-cluster/src/lease.rs`).
  - `recover_after_secs` = `fence_after`(30) + watchdog 제한(30) + 5다.
- **인증서:** 노드 인증서 이름은 `address`와 localhost뿐이다(`config.rs` `cert_names`).
- **systemd·헬스 체크:**
  - `Type=simple`, `Restart=on-failure`, `RestartSec=5`, `KillMode=process`이고 `WatchdogSec`·`sd_notify`는 없다.
  - 헬스 체크 엔드포인트는 없다. 인증 없는 `GET /api/v1/version`만 있다.
- **시험:**
  - 실제 프로세스 시험(`opld/tests/common`, kill -9, `cluster.rs`의 리더 장애)이 있다.
  - `ha-it`가 두 노드 + 위트니스, `fencing = "process"`로 돈다.
  - 프로세스 안 시험망 `memnet`(`isolate`·`block_raft`·`heal`)이 있다.
  - SIGSTOP·지연 주입·대규모 시험은 없다.

## 3. 설계 — 계획 45 런타임 격리와 systemd watchdog

### 3.1 실행기 셋

`main`이 실행기를 셋 만든다(`tokio::runtime::Builder`). 각자 이름 붙은 스레드를 쓴다.

| 실행기 | 스레드 | 도는 것 |
|---|---|---|
| `core` | 멀티 스레드 2개(`orbivisor-core`) | Raft(openraft 작업, 피어 gRPC 서버), `HaGuard`, `HaManager`, 리스 처리, 노드 감시 |
| `api` | 멀티 스레드, CPU 수의 절반(최소 2, `orbivisor-api`) | API(axum, WebSocket 콘솔·watch), 감사 로그 쓰기 |
| `work` | 멀티 스레드, CPU 수의 절반(최소 2, `orbivisor-work`) | `Reconciler`, `TaskRunner`, 스케줄러, 정리, 지표 측정기, 풀 마운트, 텔레메트리, 원격 확인 |

- 실행기 사이 호출은 지금처럼 `Arc`로 나눈 객체(`Cluster`, `Peers`, 저장소)의 메서드다.
  - tokio의 채널·`Notify`·`watch`는 실행기를 넘어 동작한다.
  - 다른 실행기의 작업을 기다리는 곳(예: API가 Raft 쓰기를 기다림)은 그 실행기에 `spawn`한 작업의 `JoinHandle`을 기다리거나, 원래처럼 채널로 기다린다.
  - openraft의 내부 작업은 `Raft::new`를 부른 실행기(`core`)에 생긴다.
- 실행기 크기는 설정 `[runtime] api_threads`, `work_threads`로 바꿀 수 있다. `core`는 2로 고정이다.
- `HaGuard`의 watchdog 쓰다듬기는 `core` 실행기와도 떼어 전용 OS 스레드(`orbivisor-watchdog`)에서 1초마다 한다.
  - 스레드는 `HaGuard`가 정한 상태(`fresh`, HA VM 있음)를 `AtomicU64`(마지막 갱신 시각)·`AtomicBool`로 읽는다.
  - 잠금을 쥔 채 장치에 쓰지 않는다.
  - 상태 판단은 지금 규칙 그대로다.

### 3.2 Raft 저장소의 동기 쓰기

- `log_store.rs`의 `append`·`save_vote`·`truncate`·`purge`와 `state_machine.rs`의 `apply`·`install_snapshot`이 redb 쓰기 트랜잭션을 연다. 이것을 전용 저장소 스레드(`orbivisor-store`, 하나)로 옮긴다.
  - async 쪽은 `oneshot`으로 결과를 기다린다.
  - 순서는 스레드 하나의 큐가 지킨다. openraft의 `append` 콜백(`LogFlushed`)도 그 스레드가 커밋 뒤 부른다.
- 읽기(`try_get_log_entries`, 상태 읽기)는 redb 읽기 트랜잭션이라 지금처럼 두되, 큰 범위 읽기(스냅샷 만들기)는 `spawn_blocking`으로 옮긴다.
- 효과: 디스크가 느려도 `core` 실행기의 하트비트·리스 처리는 계속 돈다. 쓰기가 늦으면 Raft 커밋이 늦어질 뿐이다.

### 3.3 systemd watchdog

- 유닛을 `Type=notify`, `WatchdogSec=30s`, `NotifyAccess=main`으로 바꾼다.
  - `TimeoutStartSec`은 지금 기본(90초)을 넘지 않게 둔다.
  - 시작이 끝나면(API가 듣기 시작) `READY=1`을 보낸다.
- `sd_notify`는 `$NOTIFY_SOCKET`(유닉스 데이터그램)에 직접 쓴다(새 크레이트 없음, `std::os::unix::net::UnixDatagram`). 변수가 없으면(시험·macOS) 아무것도 안 한다.
- **살아 있음 판단:** 전용 스레드가 10초마다 세 실행기에 각각 빈 작업을 보내 2초 안에 돌아오는지 본다. 셋 다 돌아온 회차에만 `WATCHDOG=1`을 보낸다.
  - 한 실행기가 20초 넘게 막히면 쓰다듬기가 멈춰, `WatchdogSec` 30초에 systemd가 데몬을 SIGABRT로 끝내고 다시 시작한다(`Restart=on-failure`).
  - `KillMode=process`라 VM은 그대로 돈다.
  - 느린 것은 경고로 남긴다.
- **HA watchdog과의 관계:**
  - 하드웨어/softdog watchdog(노드 재시작)은 HA VM이 있는데 리스를 잃었을 때의 것이고 그대로 둔다.
  - systemd watchdog(데몬 재시작)은 데몬이 멈췄을 때의 것이다.
  - 데몬이 멈춰 재시작되는 동안 HA VM이 있으면 장치 쓰다듬기도 멈춘다. 이것은 지금도 같고, 재시작이 장치 제한(30초)보다 빨라야 노드가 재부팅되지 않는다. 그래서 systemd 재시작 간격(5초)과 시작 시간을 시험으로 잰다.

### 3.4 시험

- **실행기 배치:** 각 서비스가 어느 실행기에서 도는지 `tokio::runtime::Handle::current()`의 이름(스레드 이름)으로 확인하는 단위 시험.
- **굶김 시험(`opld` 실제 프로세스):** 시험 전용 숨은 API(`[test] block_api_ms`, 시험 빌드에서만)로 `api` 실행기의 모든 스레드를 10초 동안 동기 sleep으로 막는다. 그동안 다음을 확인한다.
  - HA 리스가 계속 갱신된다(`cluster status`의 리스 나이 < 3초).
  - Raft 리더가 바뀌지 않는다.
- **저장소 지연 시험(`opl-cluster`):** 가짜 저장소 쓰기가 3초 걸려도 하트비트·`confirm_leader`가 시간 안에 돈다.
- **systemd 알림:** 가짜 `NOTIFY_SOCKET`으로 `READY=1`과 `WATCHDOG=1`이 오고, 실행기 하나를 막으면 `WATCHDOG=1`이 멈추는지 본다.
- **Linux CI:** 실제 systemd로 유닛을 띄우는 시험은 `package` 작업의 Debian 13 컨테이너에 systemd가 없어 하지 않는다. 테스트 VM(UTM)에서 손으로 확인하고 결과를 §11에 적는다.

## 4. 설계 — 계획 46 헬스 체크·API 이름·내장 VIP

- **`GET /api/v1/health`**(인증 없음):
  - 200 `{ "status": "ok", "node": …, "leader": …, "quorum": true, "lease": "fresh"|"none", "runtimes": "ok" }`, 아니면 503(같은 모양, 실패한 항목)이다.
  - 실패 조건:
    - Raft 리더를 모름, 또는 이 노드 적용 인덱스가 리더의 커밋보다 5초 넘게 뒤처짐
    - HA 펜싱을 쓰는 노드인데 리스가 `fresh`가 아님
    - 실행기 확인(§3.3)이 마지막 회차에 실패
  - 위트니스는 API가 없으므로 해당 없음이다.
  - `?ready`가 없으면 위 조건, `?live`면 프로세스가 답하는지만 본다(keepalived·HAProxy·쿠버네티스식 둘).
- **API 이름:**
  - 클러스터 설정 `api_names: [String]`(예: `orbivisor.example.com`, `192.168.0.200`)을 둔다. 관리자만 바꾼다.
  - 각 노드가 바뀐 설정을 보면 자기 노드 인증서를 그 이름을 더해 다시 발급받고(클러스터 CA, 지금의 노드 인증서 발급 흐름), API 리스너의 인증서를 끊김 없이 바꾼다(`rustls` 인증서 해석기를 바꿔 끼움).
  - 피어 mTLS 인증서는 그대로다.
- **내장 VIP(선택, Linux):** `[vip] address = "192.168.0.200/24"`, `interface = "eth0"`.
  - Raft 리더가 아니라 "VIP 리스"를 가진 노드가 VIP를 갖는다. Raft에 `VipHolder { node, until }`을 두고 리더가 정하며, 건강한(헬스 200) 노드 중 하나다.
  - 가진 노드는 `ip addr add`와 gratuitous ARP(`arping -U` 또는 raw socket)로 알린다.
  - 리스를 잃으면 즉시 `ip addr del`한다. 리스가 끝나기 전에 스스로 내려놓으므로 두 노드가 같은 VIP를 갖는 시간이 없다. 리스 만료는 리더 시계로 판단한다.
  - 리더가 바뀌어도 VIP 리스가 살아 있으면 옮기지 않는다(불필요한 이동 없음).
  - macOS는 지원하지 않는다.
  - 후보는 `api` 역할 노드 중 헬스 200인 노드다. 리더가 바뀌거나 노드가 더해져도 VIP 리스가 살아 있으면 그대로다.
  - 기본 리스 3초, 1초마다 갱신. 가진 노드가 죽으면 약 3~4초 뒤 다른 노드로 옮겨진다. 리더와 끊긴(격리된) 노드는 갱신하지 못하므로 리스가 끝나기 전에 스스로 내려놓는다.
  - VIP 여러 개(`[vip] addresses = [..]`, 사용자 질문 2026-11-11 "자동으로 VIP가 옮겨 다니는 방법"): 리더가 VIP마다 다른 노드를 골라 짐을 나누고, DNS에 VIP들을 모두 적어 둔다. 한 노드가 죽으면 그 VIP만 다른 노드로 간다.
  - 같은 L2 서브넷 안에서만 된다. 랙·서브넷을 넘으면 **BGP 모드**(선택, 계획 49 다중 링크 뒤): API 노드마다 FRR로 VIP `/32`를 헬스 200일 때만 광고하는 애니캐스트. 라우터가 ECMP로 나눠 주고, 죽은 노드는 BGP 세션(BFD)이 끊겨 1초 안팎에 빠진다.
- **앞단 프록시·바깥 포털(사용자 요청 2026-11-11: 웹 포털을 쿠버네티스 pod로 올리고 HAProxy로 API를 부름):**
  - 지금 제약: 웹은 같은 출처의 `/api/v1`을 부르고, 쿠키 인증 요청은 `Origin`의 호스트가 `Host`와 같아야 한다. 로그인 세션은 그 노드의 메모리에만 있다(API 토큰은 Raft에 있어 모든 노드에서 됨). 감사 기록의 요청 주소는 프록시 주소가 된다.
  - **세션 공유:** 로그인 세션을 Raft(`Session` 리소스, 만료와 함께)에 두어 어느 노드로 가도 이어진다. 프록시의 고정 연결(sticky)이 필요 없어지고 노드가 죽어도 다시 로그인하지 않는다. 쓰기 부담을 줄이려고 만들기·지우기만 Raft에 쓰고 마지막 사용 시각은 노드 메모리에서 묶어 가끔 쓴다.
  - **믿는 프록시:** 설정 `[api] trusted_proxies = ["10.0.0.0/8", …]`. 그 주소에서 온 요청만 `X-Forwarded-For`(감사·로그인 제한의 요청 주소)와 `X-Forwarded-Host`·`X-Forwarded-Proto`(같은 출처 검사, OIDC 콜백 주소)를 믿는다.
  - **허용 출처:** 클러스터 설정 `web_origins: ["https://portal.example.com"]`. 포털이 API와 다른 호스트일 때 그 출처의 쿠키 요청을 받는다(CORS 응답, `SameSite=None; Secure` 쿠키). 기본은 비어 있어 지금처럼 같은 출처만.
  - **UI 따로 내보내기:** 릴리스마다 웹 정적 파일 묶음(`orbivisor-web-<v>.tar.gz`)과 nginx 컨테이너 예(`packaging/portal/`)를 만든다. 노드의 내장 UI는 설정 `[web] enabled = false`로 끌 수 있다.
  - **CA 내보내기:** `orbivisorctl cluster ca`가 클러스터 CA 인증서(PEM)를 찍는다(프록시가 노드 인증서를 검증할 때 씀).
  - **API 노드(사용자 질문 2026-11-11: 30대 이상이면 HAProxy가 어느 노드를 불러야 하나):**
    - 모든 노드가 API를 갖고, 읽기는 받은 노드의 복제본에서, 쓰기는 리더로 전달된다(`Cluster::write`의 forwarder). 그래서 앞단은 노드 몇 대만 부르면 된다.
    - 노드 스펙 `roles: [api]`(기본: 투표 멤버면 api)로 앞단이 부를 "API 노드"를 정한다. 권장 3~5대, 장애 영역(`zone`)마다 고르게, 관찰 노드(계획 48)·위트니스는 뺀다(관찰 노드는 상태가 한 단계 늦게 온다).
    - `GET /api/v1/health?ready`는 유지보수(비우기) 중이거나 `api` 역할이 아닌 노드에서 503을 돌려 앞단이 저절로 뺀다. `?live`는 그대로.
    - `GET /api/v1/cluster/api-endpoints`(인증 필요)와 `orbivisorctl cluster api-endpoints [-o haproxy]`가 API 노드 목록(주소·이름·zone)과 HAProxy `server` 줄을 찍는다. 노드를 더하거나 빼면 다시 만들거나 HAProxy 런타임 API·DNS(`server-template`)로 바꾼다.
- **README:**
  - HAProxy(TLS 다시 암호화, `option httpchk GET /api/v1/health`, WebSocket 시간 제한)와 keepalived(헬스 체크 스크립트) 예
  - 쿠버네티스에 포털(nginx)과 HAProxy를 두고 같은 호스트 이름의 `/`는 포털, `/api/`는 노드로 보내는 예
  - `api_names`, `trusted_proxies`, `web_origins` 설정
  - 내장 VIP

## 5. 설계 — 계획 47 대규모 1: 리더 짐 덜기

- **투표 멤버 상한:** 클러스터 설정 `voters: 3|5|7`(기본 5, 노드가 그보다 적으면 모두)을 둔다. 리더의 `VoterKeeper`(1초마다)가 다음을 한다.
  - 투표 멤버가 상한보다 적고 건강한 learner가 있으면 승격한다(`AddVoterIds`).
  - 상한보다 많으면 장애 영역이 겹치는 것부터 강등한다(`RemoveVoters` → learner로 남김).
  - 오프라인(리스 끊김 30초)인 투표 멤버는 learner로 내리고 다른 노드를 올린다. 단, 한 번에 하나씩, 과반이 살아 있을 때만 한다.
  - 고를 때 노드 레이블 `zone`(노드 설정 `[node] zone`)이 서로 다른 노드를 먼저 고른다. 위트니스는 늘 투표 멤버이고 상한에 센다.
- **노드 감시를 리스로:**
  - `NodeMonitor`의 ping을 없애고, `NodeStatus.online`은 리더가 리스 표에서 판단한다(10초 넘게 갱신이 없으면 오프라인).
  - HA 펜싱을 끈 노드도 리스를 갱신한다(펜싱 동작 없이 생존 표시만).
- **리스 확인 묶기:** `LeaseTable::renew`의 `confirm_leader`를 공유 확인 하나로 묶는다. 진행 중인 확인 뒤에 온 요청은 다음 확인을 함께 기다린다.
- **모으기 상한:** 지표·감사 로그의 노드 모으기를 동시에 32개까지로 둔다(`buffer_unordered`).
- **규모 시험 틀(`opl-cluster/tests/scale.rs`, 기능 `scale-it`):** `memnet`으로 노드 300개(투표 5, 나머지 learner — 계획 48 뒤에는 관찰 노드)를 한 프로세스에 띄워 다음을 잰다. 기준을 넘으면 실패한다.
  - 리더 장애 뒤 새 리더까지: < 3초
  - 쓰기 p99: 투표 5대 기준 < 50 ms
  - 노드 하나 추가: < 2초
  - 리더의 초당 RPC 수
  - CI에서는 노드 100개로 줄여 돈다.

## 6. 설계 — 계획 48 대규모 2: 관찰 노드

- 투표 멤버와 learner(상한 + 대기 2대)가 아닌 노드는 Raft 구성원이 아니다(관찰 노드). 관찰 노드는 다음과 같이 동작한다.
  - 투표 멤버 하나에 피어 RPC `Follow { since_revision }`(서버 스트리밍)로 붙어 스냅샷(처음·뒤처졌을 때) + 변경을 받아 자기 저장소에 적용한다. 지금 `/watch`의 변경 형식을 쓴다.
  - 쓰기는 지금처럼 리더로 전달한다(`LeaderRequest::Write`).
  - 읽기는 자기 복제본으로 답한다. 리비전으로 "최소 이 리비전 이후" 읽기를 지원한다. 쓰기 뒤 자기 쓰기를 읽으려면 그 리비전까지 기다린다.
  - 붙은 투표 멤버가 죽으면 다른 투표 멤버로 다시 붙는다(마지막 리비전부터).
- 리더의 일은 투표 멤버·learner 수에만 비례하고, 관찰 노드 수와 무관하다.
- 투표 멤버가 줄면 `VoterKeeper`가 관찰 노드를 learner로(따라잡은 뒤 투표로) 올린다.
- 혼합 버전: 관찰 노드 기능은 노드 기능 `observer`로 가린다. 모든 노드가 지원할 때까지 learner로 남긴다.

## 7. 설계 — 계획 49 클러스터 통신 다중 링크

- 노드 설정 `cluster_addresses = ["10.0.0.11", "10.1.0.11"]`(앞이 우선, 없으면 `address` 하나)을 둔다.
- Raft 구성원 노드 타입을 `BasicNode`에서 `OrbNode { addrs: Vec<String> }`으로 바꾼다.
  - openraft 노드 타입 변경은 로그·스냅샷 형식 변경이라, serde가 옛 `{"addr": …}`도 읽게 한다(`addr` → `addrs[0]`).
  - 노드 기능 `multi-link`로 가린다.
- `Peers`는 노드마다 링크 목록을 두고 처음 링크로 연결한다.
  - 실패하거나 RPC가 시간 초과되면 다음 링크로 넘어가고, 30초마다 우선 링크를 다시 시도한다.
  - Raft 하트비트도 같은 길을 쓴다.
- 인증서 이름에 모든 `cluster_addresses`를 넣는다.
- 시험: `memnet`에 링크별 끊기를 더하고, 실제 프로세스 시험은 127.0.0.1·127.0.0.2 두 주소로 한다.

## 8. 설계 — 계획 50 배치 규칙과 HA 정책

- **VM 스펙:**
  - `ha` bool을 `ha: { state: started|stopped|ignored, group?, max_restart: 1, max_relocate: 1 }`로 넓힌다. 옛 bool도 읽는다.
- **HA 그룹(클러스터 리소스 `HaGroup`):** `nodes: [{ node, priority }]`, `restricted`, `nofailback`.
- **배치 규칙(클러스터 리소스 `PlacementRule`):** HA VM만이 아니라 모든 VM에 적용한다(사용자 요청 2026-11-11).
  - `{ name, kind, vms: [..] | vm_tag, nodes?: [..] | node_tag, weight?: 1~100, enabled }`
  - VM끼리 규칙:
    - `affinity`: 반드시 같은 노드
    - `anti-affinity`: 반드시 서로 다른 노드
    - `soft-affinity`: 되도록 같은 노드
    - `soft-anti-affinity`: 되도록 서로 다른 노드
  - VM과 노드 규칙: `node-affinity`(반드시 이 노드들 중), `node-anti-affinity`(이 노드들은 안 됨), 그리고 각각의 `soft-` 형태
  - VM을 하나씩 적지 않고 태그(`vm_tag`, `node_tag`)로 묶을 수 있다. 노드에 `tags`를 둔다(장애 영역 `zone`도 태그처럼 쓸 수 있음).
- **강제 방식:**
  - 반드시(hard) 규칙은 스케줄러의 조건이다. 맞는 노드가 없으면 시작·생성·마이그레이션을 409 `placement_rule`(어긴 규칙 이름)로 거절한다. 관리자의 마이그레이션은 `--force`로 넘을 수 있고 감사 기록에 남는다.
  - 되도록(soft) 규칙은 점수 벌점(`weight`)이다. 어길 수밖에 없으면 어기고 VM 상태에 `rule_violations`로 보인다.
  - 규칙끼리 모순(같은 두 VM에 affinity와 anti-affinity)은 만들 때 422로 거절한다.
  - `affinity`로 묶인 VM은 함께 놓고 함께 옮긴다(한 VM만 옮기는 마이그레이션은 거절, 그룹 옮기기는 계획 53 균형 조정기와 비우기가 함).
  - HA 복구: hard 규칙을 지킬 수 없으면 기본은 켜지 않고 `error`로 둔다. 규칙에 `ha_may_violate: true`면 어기고 켠 뒤 경고하고, 자리가 나면 균형 조정기가 바로잡는다.
  - 처음 놓기, HA 복구, 비우기(drain), 균형 조정기(계획 53)가 같은 조건·점수 함수를 쓴다.
- **노드 정상 종료 정책(클러스터 설정 `shutdown_policy`):**
  - `conditional`(기본): 재부팅이면 그대로 둠, 꺼짐이면 옮김
  - `freeze`
  - `failover`
  - `migrate`: 비우기를 써서 미리 옮김
- **동작:**
  - `HaManager`가 복구·되돌아가기 때 그룹 우선순위와 규칙으로 노드를 고른다.
  - 같은 노드 재시작이 `max_restart`를 넘으면 다른 노드로 옮기고, 그것도 `max_relocate`를 넘으면 `error` 상태로 둔다.
- 웹·oplctl·API가 함께 바뀐다.

## 8.1 설계 — 계획 51 빠른 HA와 노드 전원 끄기

- **지금(계획 44 끝):** 노드가 죽으면 리더의 `HaManager`가 `recover_after_secs` = `fence_after`(30) + watchdog 제한(30) + 5 = **약 65초** 뒤에 다른 노드에서 VM을 켠다(`opl-api/src/ha.rs`).
  - 리더가 바뀌면 새 리더는 리스 기준점을 다시 잡아 그만큼 더 기다린다(`base_at`).
  - 견줌: Proxmox VE는 리스 60초 + watchdog으로 보통 2분 안팎, VMware HA는 감지 십수 초 + 게스트 부팅이다.
- **목표:** 기본 설정에서 노드 장애 → VM이 다른 노드에서 켜지기 시작할 때까지 **약 20초**, 대역 외 펜싱을 쓰면 **약 10초**다. 게스트 OS 부팅 시간은 따로다.
- **짧은 리스와 watchdog:**
  - 기본값을 `fence_after_secs` 30 → 10, `watchdog_timeout_secs` 30 → 10으로 낮춘다(리스 갱신 1초). `recover_after` = 10 + 10 + 2.
  - 계획 45(실행기 격리)와 49(다중 링크)가 멀쩡한 노드의 잘못된 펜싱을 막아 주므로 그 뒤에 한다. 둘 중 하나를 끈 설정이면 시작 로그에 경고하고 README에 옛 값(30·30)을 권한다.
  - 프로필 `[ha] profile = "fast"|"safe"`로 묶고, 개별 값이 있으면 개별 값이 이긴다.
- **노드 전원 끄기 펜싱(선택):**
  - 노드 스펙 `fence: [{ kind, … }]`이고 적은 순서대로 시도한다. 비밀번호는 지금의 비밀값 저장 방식으로 둔다.
    - `ipmi { address, user, password_secret, lanplus: true }`: `ipmitool chassis power off`, 확인은 `chassis power status`
    - `redfish { url, user, password_secret, system_id? }`: `ComputerSystem.Reset` `ForceOff`, 확인은 `PowerState`
    - `command { off, status }`: 관리자가 준 명령(PDU의 SNMP, iLO·iDRAC 전용 도구, 가상 환경의 API 등). 노드 이름·주소를 환경 변수로 넘기고, `status`가 `off`를 찍으면 꺼진 것이다.
  - 펜싱은 리더가 하고, 그 노드가 아닌 다른 노드(리더)에서 BMC에 닿아야 한다. 각 노드는 시작 때와 1시간마다 자기 `fence`의 `status`를 리더 대신 이웃 노드 하나에 시켜 확인하고, 실패하면 노드 상태에 `fence_unreachable` 경고를 둔다.
  - 리스가 `fence_after`를 넘으면 리더가 그 노드의 전원을 끄고, 꺼짐을 확인하면 watchdog 제한을 기다리지 않고 바로 복구한다.
  - 끄기에 실패하거나 확인이 안 되면 다음 방법을 시도하고, 모두 안 되면 지금처럼 watchdog 제한을 기다린다(안전 쪽으로 물러남).
  - 계획 52의 "노드 전원 끄기" 대응도 이 경로를 쓴다.
  - `orbivisorctl node fence-test <node>`는 `status`만 부른다(전원은 건드리지 않음).
- **미리 고른 복구 노드:**
  - 리더가 10초마다 HA VM마다 "이 노드가 죽으면 갈 곳"을 계산해 둔다(메모리에, Raft 아님). 지금의 스케줄러와 계획 50 규칙을 쓰고, 계획 53 뒤에는 그 점수 함수로 바뀐다.
  - 복구 때 스케줄링 계산을 기다리지 않는다. 계산해 둔 곳이 맞지 않으면 그 자리에서 다시 고른다.
- **병렬·우선순위 재시작:**
  - VM 스펙 `ha.priority`(0~100, 기본 50)가 높은 VM부터 켠다.
  - 대상 노드마다 동시 시작 수 상한(`ha_start_concurrency`, 기본 4)을 두어 한 노드에 시작이 몰려 디스크가 막히지 않게 한다.
- **HA 용량 예약(admission control):**
  - 클러스터 설정 `ha_reserve`: `none`(기본) | `nodes: N` | `percent: P`이다.
  - 켜면 HA VM 시작·생성·옮기기 때 "노드 N대가 죽어도 모든 HA VM이 들어갈 자리"가 남는지 검사하고, 없으면 409 `ha_capacity`로 거절한다.
  - 장애 뒤 자리가 없어 복구 못 하는 일을 미리 막는다(VMware HA admission control).
- **같은 노드 안 빠른 재시작:** QEMU가 죽은 HA VM은 노드 장애로 보지 않고 그 노드의 `Reconciler`가 1초 안에 다시 켠다. 계획 50의 `max_restart`를 넘으면 다른 노드로 옮긴다.
- **게스트 멈춤 감지(선택, VMware VM Monitoring):**
  - VM 스펙 `ha.guest_watch: { timeout_secs: 60 }`이다. 게스트 에이전트(`guest-ping`)가 그 시간 넘게 답하지 않으면 강제 재시작한다.
  - 에이전트가 처음부터 응답한 적 없는 VM에는 적용하지 않는다.
- **시험:**
  - `ha-it`에 복구 시간 측정 시험을 둔다. kill -9, 네트워크 끊기(`isolate`), SIGSTOP 세 가지에서 리스 끊김부터 대상 노드 QEMU 시작까지를 재고 기본 프로필에서 25초 이하인지 본다.
  - 가짜 Redfish 서버, 가짜 `ipmitool`·`command` 스크립트로 전원 끄기의 성공·실패·확인 안 됨·다음 방법으로 넘어가기를 시험한다.
  - 용량 예약은 스케줄러 단위 시험이다.

## 8.2 설계 — 계획 52 HA 상태 검사(네트워크·스토리지·본딩 포트)

- **지금:** HA가 보는 것은 리스(관리 네트워크로 리더에 닿는가) 하나다. 스토리지 네트워크(NFS, iSCSI·FC SAN의 `LvmShared`, Ceph RBD)가 끊겨 VM 디스크 I/O가 멈춰도 리스가 살아 있으면 아무 일도 하지 않는다. 본딩의 포트 하나가 죽어도 알 길이 없다.
- **견줌:** VMware의 VM Component Protection(APD·PDL 때 VM 재시작)과 HA 네트워크 격리 검사, Pacemaker의 `ethmonitor`·스토리지 감시와 같은 일을 한다. Proxmox VE에는 없다.
- **감시 네트워크 고르기(클러스터 리소스 `HaCheck`):**
  - `{ name, role: management|storage|vm, target, on_fail, grace_secs }`이다. 노드마다 다르면 `nodes: { <node>: target }`으로 덮는다.
  - `target`:
    - `link { iface }`: 인터페이스(본딩이면 본딩)의 링크·carrier
    - `ping { addresses: [..], via?: iface, need: 1 }`: 게이트웨이·스토리지 대상에 ICMP(관리 네트워크 격리 판단용)
    - `pool { name }`: 스토리지 풀의 경로 검사. NFS는 마운트에서 시간 제한 있는 `stat`·작은 파일 읽기, `LvmShared`는 iSCSI 세션 상태와 multipath 경로 수, RBD는 `rados` 클러스터 응답
  - 계획 49의 클러스터 링크도 자동으로 `role: management` 검사가 된다.
  - 웹·oplctl에서 노드의 인터페이스 목록을 보고 고른다(노드 상태에 인터페이스·본딩·IP를 보고하도록 늘림).
- **본딩 포트별 검사:**
  - 노드가 `/sys/class/net/<bond>/bonding/slaves`와 각 포트의 `operstate`·`carrier`·`speed`, LACP(802.3ad)이면 `/proc/net/bonding/<bond>`의 포트별 aggregator id·partner 정보를 5초마다 읽는다.
  - 포트 상태는 `up`, `down`, `lacp_mismatch`(다른 aggregator에 묶임 또는 상대가 없음), `speed_mismatch`다.
  - 본딩 전체가 살아 있고 포트 일부만 나쁘면 `degraded`로 경고만 한다(이벤트, 웹 노드 화면의 빨간 포트, 감사 기록). 모든 포트가 죽으면 그 `link` 검사가 실패한다.
- **대응(`on_fail`):**
  - `warn`: 경고만
  - `migrate`: 영향받는 VM을 다른 노드로 라이브 마이그레이션(관리·마이그레이션 네트워크가 살아 있을 때, VM 네트워크 검사 실패에 알맞음)
  - `restart_elsewhere`: 영향받는 VM을 이 노드에서 끄고 다른 노드에서 켬(스토리지 끊김에 알맞음, 디스크 I/O가 멈춰 라이브 마이그레이션이 안 되므로)
  - `fence`: 노드 전원 끄기(계획 51) 뒤 모든 HA VM 복구
  - 기본값: `storage` → `restart_elsewhere`, `vm` → `migrate`, `management` → `warn`(관리 네트워크 끊김은 리스 경로가 따로 처리)
- **영향받는 VM:** `storage` 검사는 그 풀에 디스크가 있는 VM, `vm` 검사는 그 인터페이스의 브리지를 쓰는 NIC가 있는 VM이다. HA가 켜진 VM에만 자동 대응하고, 나머지는 경고만 한다.
- **판단과 안전:**
  - 노드가 검사 결과를 리스 갱신에 실어 보내고, 리더가 `grace_secs`(기본 10초) 넘게 실패한 검사에 대응한다.
  - 같은 검사가 클러스터 노드 과반에서 함께 실패하면 노드 문제가 아니라 공유 장비(스토리지 자체, 스위치) 문제로 보고 경고만 한다. 모든 노드에서 VM을 내리는 일을 막는다.
  - `restart_elsewhere`는 이중 쓰기를 막으려고 먼저 그 노드가 QEMU를 강제로 끄고(`kill -9`) 꺼짐을 리더에 알린 뒤 다른 노드에서 켠다. 노드가 답하지 않으면 전원 끄기 펜싱(계획 51)이 성공한 뒤에만 켜고, 펜싱 방법이 없으면 경고만 한다.
  - 대응한 VM은 계획 50의 `max_relocate`에 셈한다.
  - 대상 노드도 같은 검사가 통과한 노드만 고른다.
- **시험:**
  - 고정 `/sys`·`/proc` 트리로 본딩 파서 시험(active-backup, 802.3ad의 정상·포트 down·aggregator 어긋남)
  - 가짜 검사 결과로 리더 대응 시험(grace, 과반 실패 때 경고만, restart_elsewhere의 끄기 확인 순서, 펜싱 없음)
  - Linux CI에서 dummy·veth 인터페이스로 `link` 검사, 테스트 VM(UTM)에서 본딩과 NFS 끊기를 손으로 확인

## 8.3 설계 — 계획 53 최적 배치

- **지금:** VM을 켤 때 `spec.node`가 비어 있으면 스케줄러가 조건(아키텍처, 풀, 브리지, 방화벽, HA 펜싱)에 맞는 노드 중 할당되지 않은 메모리가 가장 많은 노드를 고른다(`opl-api/src/scheduler.rs`). 실제 사용량은 보지 않고, 켠 뒤에는 옮기지 않는다.
- **부하 요약:**
  - 노드는 리스 갱신(1초)에 작은 부하 요약을 실어 보낸다: 지난 1분·5분 CPU 사용률, 사용 메모리, VM마다 CPU·메모리 사용(계획 44 측정기 값).
  - 리더는 이를 메모리에만 둔다(Raft에 쓰지 않음). 수백 노드에서도 리더가 노드에 묻지 않으므로 늘어나는 비용이 없다(계획 47과 같은 길).
- **점수:**
  - 조건을 통과한 노드마다 점수 = CPU 여유(5분 평균) × 가중치 + 메모리 여유(실제 사용과 할당 중 큰 값) × 가중치 − 규칙 위반 벌점(계획 50의 soft 규칙)이다. strict 규칙과 `ha_reserve`는 조건으로 거른다.
  - 클러스터 설정 `placement.policy = "balance"(기본, 고르게)|"pack"(적은 노드에 모음)`이다.
  - 처음 놓기, HA 복구 노드, 노드 비우기(drain) 대상이 모두 같은 점수 함수를 쓴다.
- **균형 조정기(리더, VMware DRS):**
  - 5분마다 노드 부하의 불균형(노드 CPU·메모리 사용률의 표준편차)을 계산한다. `placement.threshold`(기본 0.15)를 넘으면 불균형을 가장 많이 줄이는 옮기기를 욕심쟁이 방식으로 고른다. 비용은 VM 메모리 크기이고, 효과 대비 비용이 낮은 것만 고른다.
  - 모드 `placement.mode = "off"|"recommend"(기본)|"auto"`이다.
    - recommend: 추천 목록(`GET /placement/recommendations`)을 웹·`orbivisorctl placement`로 보이고, 누르면 마이그레이션 Task를 만든다.
    - auto: 직접 라이브 마이그레이션 Task를 만든다.
  - 안전 장치:
    - 동시 옮기기 2개
    - VM마다 1시간 안에 다시 옮기지 않음
    - `placement.pinned`인 VM, 로컬 디스크 VM(디스크 옮기기가 필요함), 백업·스냅샷 중인 VM은 옮기지 않음
    - 유지보수(비우기) 중인 노드가 있으면 쉼(비우기와 겹치지 않게)
- **시험:**
  - 점수·불균형·욕심쟁이 고르기의 단위 시험
  - `MockHypervisor`와 가짜 부하로 recommend·auto의 Task 생성 시험, 안전 장치 시험
  - 규모 시험 틀(계획 47)에서 300 노드 부하 요약이 리더 CPU에 미치는 영향 측정

## 8.4 설계 — 계획 54 VM 무중단 유형 변경

- **지금:**
  - 실행 중에 바꿀 수 있는 것: 디스크 늘리기(`volume resize`), 보안 그룹, HA 끄기
  - 실행 중 막히는 것: 디스크 구성, NIC 개수·순서·MAC은 409 `vm_running`
  - vCPU·메모리 변경: 스펙만 바뀌고 다음 시작 때 적용된다.
- **vCPU:**
  - QEMU를 `-smp <지금>,maxcpus=<상한>`으로 켠다. 상한은 VM 스펙 `cpu.max_vcpus`이고, 기본은 지금 값(핫플러그 꺼짐)이다.
  - 늘리기는 QMP `device_add`(x86 `*-x86_64-cpu`, 소켓·코어·스레드 id는 `query-hotpluggable-cpus`로)다.
  - 줄이기는 `device_del`(게스트가 내려 줘야 함, 시간 제한 뒤 실패 보고)이다.
  - ARM(`virt`)의 vCPU 핫플러그는 QEMU 버전에 따라 없어서, 노드 능력으로 가리고 없으면 "다음 재시작 때 적용"이다.
- **메모리:**
  - `virtio-mem` 장치(`-object memory-backend-ram` + `-device virtio-mem-pci`, 상한 `memory.max_mib`)로 `qom-set requested-size` 해서 늘리고 줄인다.
  - pc-dimm보다 줄이기가 안정적이고 x86·ARM 모두 된다.
  - 게스트 드라이버가 필요하다(Linux 5.8+, Windows는 virtio-win 드라이버). 드라이버가 없으면 늘린 메모리가 보이지 않는다(README에 적음).
- **디스크·NIC:**
  - 붙이기·떼기는 `blockdev-add`+`device_add`(virtio-blk/virtio-scsi), `netdev_add`+`device_add`(virtio-net)다.
  - tap·방화벽 규칙은 붙인 NIC 순번으로 만들고, 떼면 지운다.
  - 지금의 `vm_running` 제한을 이 경로로 바꾼다. 순서 바꾸기·MAC 바꾸기는 여전히 끈 뒤에만 된다.
- **적용 방식:**
  - `PUT /vms/{key}`가 실행 중이면 바꿀 수 있는 것은 바로 적용(Task `vm-hotplug`)한다.
  - 그렇지 않은 것은 VM `status.pending`(다음 재시작 때 적용할 변경)으로 보인다(Proxmox의 "pending changes").
  - 웹 VM 상세에 "재시작 필요" 배지를 둔다.
  - 마이그레이션 대상 QEMU는 지금 크기(핫플러그된 장치 포함)로 켠다.
- **유형(flavor) 바꾸기:** 견본(작게·보통·크게, 계획 59)을 고르면 위 경로로 vCPU·메모리를 바꾼다.
- **시험:**
  - 가짜 QMP 시험
  - `qemu-it`(실제 QEMU에서 `query-hotpluggable-cpus`·`virtio-mem` 크기)
  - Linux CI linux-kvm에서 게스트가 있는 시험은 하지 않고, 테스트 VM(UTM)에서 손으로 확인한다.

## 9. 설계 — 계획 55 로컬 디스크 HA

- VM 스펙 `replication: [{ target_node, schedule: "*/15" }]`을 둔다. 증분 복제(계획 41)의 dirty bitmap·전송을 클러스터 안 노드로 돌려 대상 노드의 같은 이름 풀에 복제본 볼륨을 둔다.
- HA 복구 때 로컬 디스크 VM은 마지막 복제본이 있는 노드에서 그 복제본으로 켠다. 잃는 데이터는 마지막 복제 이후다(README에 분명히 적음).
- 마이그레이션은 복제본이 있으면 마지막 증분만 보낸다.

## 10. 설계 — 계획 56 Ceph 자동 구성

- `orbivisorctl ceph init|mon add|osd add <disk>`와 웹 마법사로 노드에 Ceph(Debian 패키지)를 설치·구성하고, RBD 풀을 Orbivisor 스토리지 풀로 바로 등록한다.
- 상태 화면(health, OSD, PG)을 둔다.
- 범위가 커서 계획을 쓸 때 따로 설계 문서를 둘 수 있다.

## 11. 구현(계획 45)

§12의 네 단계를 커밋 넷으로 했다. 설계 §3과 다르거나 설계가 정하지 않아 정한 것을 적는다.

**실행기 셋(§3.1).**
- `opld/src/runtime.rs`: `Runtimes::new(api_threads, work_threads)`, 복제할 수 있는 `Handles { core, api, work }`,
  `runtime::on(&Handle, || …)`(그 실행기의 문맥에서 동기로 띄움), `runtime::run_on(&Handle, fut)`(그 실행기에서 돌리고
  기다림). `main`은 `#[tokio::main]` 대신 `runtimes.core.block_on(run(…))`이라 주 루프가 띄우는 작업과 **제어 소켓은
  `core`**다. 다만 주 루프 자신(close·leave·join을 기다리고 부르는 곳)은 `core` 작업자가 아닌 **주 스레드의
  `block_on`**에서 돌므로 `Liveness`의 실행기 확인이 보지 못한다(주 루프가 막혀도 `WATCHDOG=1`은 계속 간다).
- 설정 `[runtime] api_threads`·`work_threads`(기본 CPU 수의 절반, 최소 2, 0은 거부). `core`는 `CORE_THREADS` = 2.
- 배치는 표대로이고 설계가 적지 않은 것은 이렇게 두었다: TLS 받기·핸드셰이크는 `api`, `QemuHypervisor::new`(다시
  붙는 QMP·모니터 작업)·업데이트·VXLAN·방화벽·Node 등록 루프는 `work`, 승격은 `core`.
- **피어 gRPC 서버를 나눔(설계와 다름).** 설계는 피어 서버 전체를 `core`에 두었지만, 그러면 들어오는 라이브
  마이그레이션(받은 VM의 QMP·모니터 작업까지), 콘솔 VNC 중계, UEFI 변수 보내기가 `core`에서 돈다.
  `PeerService::with_work(Handle)`로 이것들과 감사 로그·지표 읽기를 `work`에서 돌리고, Raft·리스·전달·ping만
  `core`에 남겼다. 위트니스는 `work` 핸들을 넘기지 않는다(그런 요청을 받지 않는다). 처리기에서 panic하면 다시
  던지지 않고 오류 로그와 `internal`로 답한다(같은 HTTP/2 연결의 Raft RPC를 함께 끊지 않게).
- **남은 `core` 부담.** 나눈 것은 처리기뿐이다. 노드 간 서버의 연결마다의 TLS·HTTP/2 작업은 `core`에 있어서 콘솔
  VNC 바이트·UEFI 변수 조각을 틀에 넣고 암호화하는 일은 `core` 스레드가 한다. 팔로어가 리더에게 전달하는 큰 API
  쓰기도 control 연결(`core`)로 가므로 `core`에서 암호화된다. 대량 전송용 포트를 따로 두는 것은 다중 링크(계획
  49)에서 다룬다.
- **다른 노드로 가는 연결도 나눔(설계가 정하지 않음).** tonic 채널은 연결 작업(요청 버퍼, HTTP/2 연결)을 처음
  부른 쪽 실행기에 띄우므로, `work`의 Node 등록이나 `api`의 지표 모으기가 먼저 연 연결이면 Raft 하트비트와 리스
  갱신 전달이 그 실행기에 기댄다. `Peers::with_runtimes(control, bulk)`가 주소마다 채널을 둘 두고
  `Endpoint::executor`로 연결 작업을 정한 실행기에 띄운다: Raft(`GrpcNetwork`)·리더 전달(리스 갱신, 쓰기 전달)·ping은
  control(`core`), 콘솔·마이그레이션 보내기·UEFI 변수·감사 로그·지표 읽기는 bulk(`work`). 실행기를 주지 않으면(시험,
  oplctl) 채널 하나를 처음 부른 쪽에서 돌린다. 시험은 따로 만든 실행기가 먼저 연 control 채널이 그 실행기가 끝난
  뒤에도 되는지 본다.
- 배치 시험은 스레드 이름이 아니라 실행기별 남은 작업 수로 본다: 따로 만든 current-thread 실행기에서 단일 노드
  멤버를 띄우면 부른 쪽에 남은 작업이 0이고 세 실행기 모두 작업이 있으며, 닫으면 10초 안에 모두 0이다. 스레드
  이름은 `runtime.rs` 시험이 본다.

**watchdog 스레드와 저장소 스레드(§3.1·§3.2).**
- `HaGuard`: watchdog 장치를 쓸 때만 OS 스레드 `orbivisor-watchdog`을 띄운다. 스레드는 잠금 없이
  `AtomicU64`(마지막으로 성공한 갱신을 보내기 전 시각)와 `AtomicBool`(HA VM 있음)을 읽고 **스스로 시간을 재어**
  `fresh`를 계산한다(실행기가 멈춰 갱신 루프가 돌지 않아도 리스가 끝나면 규칙대로 쓰다듬기를 멈춤). 닫을 때
  스레드에서 장치를 돌려받아 잠금 밖에서 magic close한다.
- opl-cluster `writer.rs` `StoreWriter`: OS 스레드 `orbivisor-store` 하나, `run()`은 결과를 `oneshot`으로 기다리고
  `submit()`은 기다리지 않는다. 스레드는 데이터베이스를 약한 참조로 쥐어 저장소를 놓으면 닫힌다. 오류
  `StoreError::Thread`·`WriterStopped`·`WriterPanicked`. 쓰기가 panic하면 그 쓰기는 `WriterPanicked`이고 오류 로그를
  남긴 뒤 스레드를 멈춘다(메모리의 Raft 상태가 디스크와 어긋났을 수 있으므로 계속 쓰지 않는다. 뒤의 쓰기는
  `WriterStopped`이고 openraft가 그 노드의 Raft를 멈춘다).
- **Raft 멈춤 감시.** openraft가 Raft를 멈춰도(`metrics().running_state`가 `Err`) 아무도 보지 않으면 HA VM이 없는
  노드는 API만 답하는 좀비로 남는다. `Cluster::stopped()`(Raft가 멈추면 이유와 함께 끝나는 future)를
  `Member::start`·`start_witness`가 `core`의 작은 감시 작업으로 기다리다 `"Raft stopped: …"`를 서버가 멈출 때와 같은
  채널로 보낸다. 주 루프가 0이 아닌 코드로 끝나고 `Restart=on-failure`가 다시 띄운다. `close`는 저장소를 닫기 전에
  감시를 멈춰 정상 종료는 알리지 않는다. 시험: 저장소 쓰기를 panic시키면 `stopped`가 저장소 오류로 끝남(opl-cluster,
  `#[cfg(test)]` 훅), 멤버의 저장소를 닫으면 `failed`가 `"Raft stopped: "`로 끝남(opld).
- `orbivisor-watchdog` 스레드는 새 장치를 받으면 한 간격을 기다리지 않고 곧바로 한 번 쓰다듬는다(규칙은 같다).
- `HaGuard::close` 뒤의 `arm()`은 장치를 열지 않고 `ArmError::Closed`다(쓰다듬을 스레드가 없어 연 장치가 그냥 닫히면
  타이머가 돈다). 갱신 루프는 쓰다듬기 스레드가 쓰는 시각을 async 시각보다 먼저 재어 스레드의 마감이 `lease`의
  마감보다 늦지 않다.
- `append`는 **커밋 뒤에** 돌아오고 `LogFlushed` 콜백도 저장소 스레드가 커밋 뒤에 부른다. openraft 0.9.25는
  `append`가 돌아온 직후 그 엔트리를 읽을 수 있어야 하기 때문이다. `save_committed`는 기다리지 않고 큐에 넣는다.
  스냅샷을 만드는 큰 읽기는 `spawn_blocking`이다.
- **"쓰기가 늦으면 Raft 커밋이 늦어질 뿐"은 완전히 맞지 않는다.** openraft 0.9.25의 Raft 루프는 자기 로그 쓰기를
  기다린다. 그래서 한 노드의 디스크가 느리면 그 노드의 Raft 루프가 멈춘다. 저장소 스레드는 fsync가 실행기 스레드를
  막지 않게 할 뿐이다. 느린 노드가 팔로어면 그 노드만 멈칫한다(선거를 시작할 수 있다). **리더**면 리더의 Raft 루프가
  멈춰 리스 갱신의 리더 확인(`confirm_leader`)이 실패하므로 모든 노드의 리스 갱신이 실패하고, 하트비트가 끊긴
  팔로어가 선거로 새 리더를 뽑은 뒤 회복한다. 저장소 지연 시험은 느린 팔로워의 선거를 끄고(그 노드의 Raft 루프는
  멈추므로) 리더의 쓰기와 리스 갱신(`confirm_leader`)이 500 ms 안에 돌고 리더·임기가 그대로인지 본다.

**systemd 알림(§3.3).**
- `opld/src/notify.rs`: `Notifier::from_env()`·`notify(state)`(`$NOTIFY_SOCKET`, `@`는 Linux 추상 소켓, 없거나 비면
  아무것도 안 함, 보내기 실패는 경고), `Liveness::spawn(Handles, Notifier)`. 확인은 OS 스레드 **`orbivisor-notify`**
  (설계는 이름을 정하지 않았다)가 시작 때 한 번, 그 뒤 10초마다 한다. 늦은 실행기 이름을 경고로 남기고, 마지막
  회차의 결과는 `Liveness::slow()`로 읽는다(계획 46 헬스 체크가 쓴다). systemd가 `WATCHDOG_USEC`을 주면 간격은 그
  1/3(1초~10초), 기한은 2초와 간격의 절반 중 짧은 것이다.
- **`work`가 늦으면 쓰다듬기를 멈추지 않는다(설계와 다름).** `WATCHDOG=1`은 `core`와 `api`가 답한 회차에 보내고,
  `work`만 늦으면 경고만 남긴다. systemd가 데몬을 끝내면 새 프로세스가 HA watchdog 장치의 제한 시간 안에 다시
  arm해야 하는데, VM·작업이 바쁜 것만으로 그 위험을 지지 않는다. 유닛은 `WatchdogSignal=SIGKILL`이다(기본
  SIGABRT의 코어 덤프가 재시작을 늦추지 않게, 진단은 느린 회차의 경고 로그로).
- **여유 계산.** HA VM이 도는 노드에서 `api`가 막혀 systemd가 데몬을 끝내는 경우, 리스 갱신(`core`)은 돌고 있으므로
  `orbivisor-watchdog`이 죽기 직전까지 장치를 쓰다듬는다. 그 뒤 장치 제한 30초 안에 `RestartSec` 5초, 시작, 첫 리스
  갱신과 arm이 끝나야 한다(남는 시간 약 25초 − 시작·리스 갱신 시간). 죽은 프로세스가 닫은 장치의 타이머는 그대로
  돌고, 새 프로세스가 장치를 다시 열면 Linux watchdog 드라이버가 타이머를 다시 시작한다(열 때 `start`, 이미 돌면
  `ping`). 그래서 마감은 마지막 쓰다듬기에서 30초이고, 그 안에 차례로 `RestartSec`, 프로세스 시작, `Cluster::open`,
  `QemuHypervisor::new`(도는 VM마다 차례로 QMP에 다시 붙음, VM 수에 비례하고 붙지 못하는 VM은 하나에
  `ATTACH_TIMEOUT` 5초까지), `HaGuard::spawn`의 첫 리스 갱신(리더가 있어야 한다), reconciler 작업자의 `attach_ha`
  (`arm`)가 끝나야 한다. `core`가 막힌 경우는 리스 갱신도 멈추므로
  `fence_after` 뒤 쓰다듬기가 멈추고, systemd는 마지막 `WATCHDOG=1`에서 30초 뒤(막힌 때에서 최대 30초 + 확인 간격
  10초) 데몬을 끝낸다. 이때 장치 제한(30초)에서 `WatchdogSec`의 30초를 빼면 여유가 없으므로 노드 재시작은 리스를
  잃은 노드의 펜싱으로 본다. 그래서 `work`만 늦을 때는 끝내지 않고, 끝낼 때는 SIGKILL로 바로 끝낸다.
- **`READY=1`을 보내는 때(§3.3 본문의 "API가 듣기 시작하면"을 바로잡음).** 클러스터에 속하지 않은 노드는 API가 없으므로
  제어 소켓을 열었을 때, 멤버는 Task 실행기가 이전 프로세스의 고아를 정리하기 직전과 `Member::start`가 돌아온 뒤(API가
  들음, init·join·다시 시작마다 다시 보내도 해가 없다), 위트니스는 노드 간 서버가 떴을 때다. 고아 정리(`sweep_orphans`)는
  정족수가 없으면 지우기·상태 쓰기마다 10초까지 기다리므로, 클러스터 전체를 다시 켤 때 `Type=notify`의 시작 제한(90초)을
  넘겨 systemd가 다시 띄울 수 있다. 정리를 Task 실행기의 뒤 작업으로 옮기지 않은 것은 업로드 파일과 가져오기 디렉터리
  정리가 revision으로 거르지 않고 "API를 띄우기 전"에 기대기 때문이다(API가 막 받는 업로드를 고아로 지울 수 있다). `STOPPING=1`은 보내지 않는다. `NOTIFY_SOCKET`은 환경에서 지우지
  않는다(`NotifyAccess=main`이라 QEMU 같은 자식이 보내도 systemd가 무시한다).
- 유닛 `Type=notify`·`NotifyAccess=main`·`WatchdogSec=30s`, `TimeoutStartSec`은 기본 그대로. `build-deb-test.sh`가
  유닛 값을 확인한다.
- `runtime::run_on`은 작업이 취소되면(실행기가 끝날 때) `JoinError::into_panic`에서 panic한다. 대상 실행기가 먼저
  끝날 수 있는 곳에서는 쓰지 않는다(문서 주석). 실행기 확인은 `run_on`을 쓰지 않고 채널로 기다리므로 문제가 없다.

**굶김 시험(§3.4).**
- 시험 전용 기능 `starve-it`(opld). 켜면 설정 `[test] block_api_ms`와 제어 소켓(`core`)의 숨은 경로 둘이 생긴다:
  `POST /v1/test/block-api`(`api` 실행기의 작업자 수만큼 작업을 보내 모두 `block_api_ms` 동안 동기 sleep, 모두 막힌
  뒤 답함)와 `GET /v1/test/raft`(이 노드가 아는 리더·임기와, 리더면 노드별 리스 나이). 배포 빌드는 `[test]`를
  모르는 키로 거부하고 경로도 없다. 설계는 막기를 설정만으로 적었지만, 클러스터를 다 만든 뒤 막아야 하므로
  제어 소켓 요청으로 시작한다.
- **리스 나이는 `cluster status`에 없다(설계와 다름).** 리스 표는 리더의 메모리에만 있고 API로 내보내지 않는다.
  API를 바꾸지 않고 시험 경로로 읽는다(사용자에게 보이는 리스 나이는 계획 46의 헬스 체크·계획 47의 리스 기반
  노드 감시에서 정한다).
- 시험 `opld/tests/starve.rs`(CI `check` 작업의 Linux·macOS에서 돈다): opld 셋(`fencing = "process"`,
  `fence_after_secs = 10`이라 갱신 간격 1초), 리더 node1의 `api`를 **15초**(설계는 10초) 막는다. 10초 간격·2초
  기한의 실행기 확인이 적어도 한 번은 막힌 동안에 들게 하려는 것이다. 그동안 리더의 API는 답하지 않고, 리더·
  임기가 그대로이며, 세 노드의 리스 나이가 3초 미만이고, node2의 API로 한 쓰기가 커밋되고, `WATCHDOG=1`이 오지
  않는다. 풀리면 API와 `WATCHDOG=1`이 돌아오고 로그에 `api`가 늦었다는 경고가 있다.

**배포 뒤 확인(테스트 VM, UTM).** 실제 systemd로 띄우는 시험은 CI에 없다(§3.4). 패키지를 설치한 Debian 13
VM에서 손으로 확인하고 결과를 적는다.

| 확인 | 결과 |
|---|---|
| `systemctl start opld` 뒤 `active (running)`(미초기화 노드와 멤버 둘 다), `systemctl show opld -p WatchdogUSec`이 30s | 멤버 두 노드에서 확인(2026-11-12): `Type=notify`·`WatchdogUSec=30s`·`WatchdogSignal=9`, Starting→Started 약 11 ms, `WatchdogTimestamp`가 10초마다 바뀜. 미초기화 노드는 실제 프로세스 시험(`notify.rs`)으로만 |
| `kill -STOP`으로 orbivisord를 세우면 약 30초 뒤 SIGKILL로 끝나고 5초 뒤 다시 뜸, 도는 VM은 그대로 | 확인(svc01, 그때 리더): 마지막 `WATCHDOG=1` 뒤 30.5초에 `Watchdog timeout`·SIGKILL, 5초 뒤 새 프로세스 `Started`(SIGSTOP부터 29.9초). 다른 노드가 리더가 되고 20초 안에 두 노드 온라인, 다른 노드의 VM(QEMU PID) 그대로 |
| 위 재시작(SIGKILL부터 리스 갱신까지)이 HA watchdog 제한(30초)보다 짧아 HA VM이 있는 노드가 재부팅되지 않음 | 못 함: 테스트 VM에 watchdog 장치(softdog)와 HA VM이 없다. 참고: 프로세스 전체가 멈추면(SIGSTOP) `orbivisor-watchdog`도 멈추므로 HA VM이 있는 노드는 장치 제한에 재부팅되는 것이 맞다(리스를 보장할 수 없음). 이 행은 실행기 하나만 막혀 systemd가 죽인 경우의 여유다 |
| 정족수가 없을 때(다른 노드를 모두 끈 채) `systemctl restart opld`가 `active (running)`이 되기까지의 시간(90초보다 짧음) | 못 함(두 노드를 다 꺼야 함). 실제 프로세스 시험(`notify.rs`)이 `READY=1`이 고아 정리 전에 오는 것을 본다 |
| VM 수에 따른 다시 arm 시간(VM 수십 개를 띄운 노드에서 SIGKILL부터 `watchdog armed` 로그까지, HA watchdog 제한 30초보다 짧음) | 못 함(HA VM·장치 없음, VM 하나) |
| `orbivisor-core`·`-api`·`-work`·`-store`·`-notify`(HA VM이 있으면 `-watchdog`) 스레드(`ps -T -p $(pidof orbivisord)`, 이름은 15자로 잘려 `orbivisor-watch`·`orbivisor-notif`) | 확인: core 2, api 2, work 4(4코어), store, notif. `fencing = watchdog`이면 HA VM이 없어도 `orbivisor-watch` 스레드가 있다(장치는 HA VM을 arm할 때만 연다) |

## 12. 구현 단계 (계획 45)

1. **실행기 셋:** `main`의 실행기 셋과 설정 `[runtime]`, 서비스 배치(`member.rs`), 실행기 이름 시험
2. **watchdog 스레드와 저장소 스레드:** `HaGuard`의 쓰다듬기 스레드(원자값), redb 쓰기 스레드, 저장소 지연 시험
3. **systemd 알림:** `sd_notify`(`READY=1`, 실행기 확인 뒤 `WATCHDOG=1`), 유닛 `Type=notify`·`WatchdogSec`, 패키징 시험
4. **굶김 시험과 문서:** 시험 빌드의 실행기 막기, opld 실제 프로세스 굶김 시험, README "제어부 가용성" 절, 이 문서 §11

## 13. 구현(계획 46)

다섯 단계를 커밋 다섯으로 했다(1 헬스 체크와 API 노드 역할, 2 세션 공유, 3 믿는 프록시와 허용 출처, 4 API 이름과
인증서 다시 발급, 5 내장 VIP·UI 따로 내보내기·문서). 설계 §4와 다르거나 설계가 정하지 않아 정한 것을 적는다.

**헬스 체크와 API 노드(§4).**
- 응답은 `{status, node, leader, quorum, apply_lag_ms, lease: fresh|expired|none, runtimes: ok|slow, maintenance,
  api, failed: [quorum|apply_lag|lease|runtimes|maintenance|role]}`이다. `?live`는 늘 200이고(저장소를 읽지 못해도)
  `failed`가 비어 있다. 로그인하지 않은 요청에는 `{status, failed}`만 답한다(나머지는 OpenAPI에서 선택 필드, 로그인한
  세션·토큰이면 전체).
- **적용 지연은 어림이다(설계와 다름).** openraft 0.9는 팔로어에 리더의 커밋 인덱스를 알려 주지 않는다. 그래서
  "이 노드 로그에 보인 항목이 적용되지 않은 채 지난 시간"을 잰다(`Cluster::health()`, 5초 넘으면 실패). 리더는
  `millis_since_quorum_ack` ≤ 5초여야 `quorum`이다.
- 노드 스펙 `roles: Option<Vec<NodeRole>>`(없으면 기본: 위트니스가 아니면 `api`, 기본이면 쓰지 않아 옛 노드가 읽음).
  설계의 "투표 멤버면 api"는 지금 모든 비위트니스가 투표 멤버라 같다. 바꾸려면 모든 노드가 기능 `node-roles`를
  보고해야 한다(아니면 409 `cluster_upgrading`). 위트니스는 `api`가 될 수 없다.
- `GET /cluster/api-endpoints`의 zone은 노드 레이블 `zone`이다. `orbivisorctl cluster api-endpoints -o haproxy`의
  `server` 줄은 `check ssl verify required ca-file /etc/haproxy/orbivisor-ca.pem`이다(`-o haproxy`는 이 명령에만).

**세션 공유(§4).**
- `Session` 리소스(`SessionSpec { user, epoch, expires_at }`, `SessionStatus { last_used }`), 이름은 쿠키 값의
  SHA-256 hex(원문은 Raft에 없다). 계정 종류라 관리자만 읽고 API로 내보내지 않으며 watch하지 않는다.
- 만들기·지우기는 Raft 쓰기, 마지막 사용 시각은 세션마다 노드에서 60초에 한 번까지 뒤에서 쓴다. 리더의 정리 작업이
  만료·주인 없음·세대(epoch) 지남을 한 번에 200개까지 지운다. 인증은 노드 메모리 먼저, 그다음 Raft다.
- 기능 `shared-sessions`가 없는 노드가 남았으면(롤링 업그레이드) 지금처럼 노드 메모리 세션이다.
- 한 사용자의 Raft 세션은 32개까지다(만들기 전에 만료가 이른 것부터 지운다).
- **다른 노드의 메모리 세션 로그아웃.** 로그아웃한 쿠키가 이 노드의 메모리·Raft 어디에도 없으면 다른 노드의 메모리
  세션일 수 있으므로 그 해시 이름으로 이미 끝난 `Session`(주인 nil, 만료 지금)을 "로그아웃 표시"로 쓴다(모든 노드가
  `shared-sessions`를 알고 정족수가 있을 때만, 쿠키 모양이 맞을 때만, 표시 2천 개까지, 못 쓰면 경고). 표시를 쓰는
  로그아웃은 IP별 로그인 실패 제한(`AttemptLimiter::begin_ip`)에 실패로 세고, 막혔으면 표시 없이 쿠키만 지운다(204,
  알려 주지 않음). 메모리 세션은
  쓸 때마다 같은 이름의 끝난 Raft 세션이 있으면 거절하고 지운다. 정리 작업은 표시를 세션 수명(12시간) 뒤 지운다.
- 알려진 한계: 정족수 없는 소수 쪽 노드는 그 노드의 옛 사용자 정보로 로그인을 받는다(바뀐 비밀번호가 따라잡을 때까지).
  로그인 시도 제한은 노드마다 따로 센다(README).
- **정족수 없는 로그인(가용성 회귀 고침).** 세션 만들기가 Raft 쓰기라 정족수가 없으면 로그인이 503이 되었다(전에는
  로컬 사용자 로그인이 정족수 없이 됐다). 정족수가 없거나(`health().quorum` false) 쓰기가 `Timeout`·`Unreachable`·
  `NotLeader`면 그 노드의 메모리 세션으로 로그인한다(경고 로그, 그 세션은 그 노드에서만).

**믿는 프록시와 허용 출처(§4).**
- `[api] trusted_proxies`(CIDR). `X-Forwarded-For`는 오른쪽부터 보며 믿지 않는 첫 주소, `X-Forwarded-Host`·
  `X-Forwarded-Proto`는 맨 오른쪽 값을 쓴다.
- `web_origins`(관리자, 32개까지, 소문자·기본 포트 뺌). 비우지 않으려면 모든 노드가 현재 빌드여야 한다. CORS는
  preflight 204, 메서드 GET·POST·PUT·PATCH·DELETE, max-age 600, Expose-Headers 없음이다. 허용 출처에서 온
  로그인·로그아웃만 세션 쿠키를 `SameSite=None; Secure`로 준다. 허용 출처는 `https://`만(개발용 `http://localhost`·
  `127.0.0.1`·`[::1]`은 예외).
- 로그아웃도 쿠키 요청과 같은 출처 검사를 한다. 허용 출처를 쓰면(쿠키가 `SameSite=None`일 수 있음) 쿠키로 인증한
  GET·HEAD가 아닌 요청과 WebSocket 업그레이드가 `Origin` 없이 `Sec-Fetch-Site`가 있는데 `same-origin`·`same-site`·
  `none`이 아니면 거절한다(`Origin` 없는 다른 사이트 요청을 막는다). 그 헤더를 보내는 브라우저는 쓰기에 `Origin`도
  보내므로, 둘 다 없는 비브라우저 클라이언트(orbivisorctl 쿠키 로그인 → `POST /tokens` → 로그아웃, curl)는 통과한다.
- **남은 것.** 다른 호스트의 포털에서 OIDC로 로그인하면 끝난 뒤 API 호스트의 `/`로 돌아간다(같은 호스트 이름 구성은
  영향 없음). README에 적었다.

**API 이름(§4).**
- `api_names`(관리자, 32개까지, IP는 표준형, DNS는 소문자·끝 점 뺌, 와일드카드·포트·괄호·숫자뿐인 끝 라벨 거절).
- **다시 발급은 노드가 스스로 한다.** 노드 인증서는 합류 요청을 받은 노드가 Raft의 클러스터 정보에 있는 CA 키로
  CSR에 서명한다. 같은 방법으로 각 노드의 `ApiCertAgent`가 새 키와 CSR을 만들어 `ClusterCa::sign_api`로 자기 API
  인증서를 서명한다(이름: 노드 인증서의 이름 + `api_names`, 다른 노드에 요청하지 않음). API 인증서는 메모리에만 둔다
  (다시 시작하면 리스너를 열기 전에 다시 만든다). 설정이 바뀌면 곧바로, 실패하면 10초마다 다시 한다.
- **API 인증서는 노드 신원과 뗀다.** 서버 인증 EKU만 두고 CN은 `orbivisor-api`다. 피어 mTLS는 클라이언트 인증 EKU와
  CN의 노드 id를 보므로 API 키가 새어도 피어 인증서로 쓰지 못한다(시험: API 인증서로 피어 연결이 거절됨). `api_names`가
  비면 리스너는 예전처럼 노드 인증서를 쓴다.
- **관리자 인증서.** opld 설정 `[api] tls_cert`·`tls_key`(PEM, 둘 다)가 있으면 API 리스너가 그것을 쓰고(중간 인증서를
  이어 붙여도 됨) `ApiCertAgent`를 띄우지 않는다. 읽지 못하면 시작 오류다. 클러스터 CA는 모든 노드에 키가 있고 이름
  제한이 없으므로 브라우저·OS 신뢰 저장소에 넣지 말고, 브라우저용으로는 HAProxy에서 공인 인증서로 끝내거나 이것을
  쓴다고 README에 적었다. 이름 제한이 있는 중간 CA는 뒤 계획으로 미뤘다. 파일은 시작할 때만 읽는다(갱신 뒤
  `systemctl restart opld`, README). `orbivisorctl login --system-ca`는 클러스터 CA를 핀으로 두지 않고 OS 신뢰
  저장소로 서버를 확인하며(클라이언트가 이미 native-tls라 핀이 없으면 OS 저장소, 새 의존성 없음) 설정 파일의
  `system_ca`로 다음 명령도 같게 한다.

**내장 VIP(§4).**
- **Raft에는 주인만 둔다(설계와 다름).** 설계는 `VipHolder { node, until }`을 Raft에 두었지만, 그러면 VIP마다 1초에
  한 번 Raft 쓰기가 생기고 노드가 `until`(리더 벽시계)을 자기 시계와 비교할 수 없다. HA 리스와 같은 모양으로 나눴다:
  Raft의 `VipHolder` 리소스(`Kind::VipHolder`, 이름은 VIP의 IP, `VipHolderSpec { node }`)는 주인이 바뀔 때만 리더가
  쓰고, 리스는 리더 메모리의 VIP 리스 표(`VipLeases { term, renewed: NodeId → VipRenewal { at, vips } }`)에 있다.
- **갱신.** 노드는 헬스 체크(`?ready`)가 200일 때만 1초마다 `LeaderRequest::RenewVip { node, vips }`를 보낸다(`vips`는
  그 노드의 `[vip] addresses`의 IP). 리더는 `RenewLease`처럼 과반수의 리더인지 확인하고 받은 시각을 적고 자기 적용
  인덱스를 돌려준다. 노드는 그 인덱스까지 적용된 뒤(`Cluster::renew_vip`) 보내기 전에 잰 시각에서 `3초 − 0.5초`까지
  VIP를 쥔다. 갱신 제한 시간은 그 마감까지라 답이 늦으면 마감에 뗀다. 헬스가 503이면 곧바로 뗀다.
- **배정(`VipManager`, 리더).** 1초마다(HA 복구처럼 장벽 뒤에 읽음) 리스가 살아 있는 노드(마지막 갱신에서 3초 미만,
  기준점은 그 임기에서 리더인 것을 처음 본 때)를 후보로 `plan`한다: 주인이 쥐었을 수 있으면(리스가 살아 있고 그 VIP를
  알렸거나, 리더가 된 지 3초가 안 되어 아직 갱신이 없음) 그대로, 아니면 그 VIP를 가질 수 있는 후보 가운데 VIP가 가장
  적은 노드(같으면 id가 작은 노드). 옛 주인에게서 옮길 때는 옛 주인의 VIP 갱신을 막고(`begin_vip_fence`, `Fenced`로
  거부) 리스를 다시 본 뒤 쓴다. 쓰기의 결과를 알면(`Ok`·`Rejected`·`NotLeader`) 풀고, 모르면 그 쓰기가 보이거나
  임기가 바뀔 때까지 쥔다. 그래서 펜싱이 풀린 뒤의 갱신이 돌려주는 인덱스에 옮긴 결과가 들어 있다.
- 설계의 "리더가 고르는 후보는 `api` 역할·헬스 200"은 노드가 헬스 200일 때만 갱신하는 것으로 했다(리더는 노드의
  헬스를 직접 보지 못한다).
- **VIP 여러 개.** 각 노드가 가질 수 있는 VIP는 그 노드의 설정이 정한다(갱신에 실어 보냄). 모든 API 노드에 같게 두는
  것을 권한다.
- **노드 쪽(`VipAgent`).** `core` 실행기에서 돈다(api·work가 바빠도 제때 뗀다). 시작할 때 설정의 VIP를 모두 뗀다
  (kill -9로 죽은 지난 프로세스가 남긴 것). 붙이기는 `ip addr add <vip>/<prefix> dev <interface>`(이미 있으면 성공),
  떼기는 `ip addr del`(없으면 성공), 알림은 `arping -U -c 3 -I <interface> <ip>`(세 번)를 기다리지 않고 띄운다(실패는
  경고). VIP마다 알림 작업을 쥐고 떼면 멈춘다(늦은 알림이 새 주인을 가리지 않게). 정상 종료는 API보다 먼저 VIP를
  뗀다. 패키지는 `iputils-arping`을 Recommends로 둔다. 루프 맨 앞에서 리스 마감이 이미 지났으면(늦게 깸) 갱신보다 먼저
  VIP를 떼고, 마감까지 남은 시간이 없으면 갱신하지 않는다. 갱신 5번에 한 번 쥔 VIP를 다시 `add`해(`VipNet::add`는
  새로 붙였으면 true) 밖에서(`ifreload`·브리지 다시 시작·networkd) 지워진 주소를 되살리고 다시 알린다.
- **업그레이드.** 노드 기능 `vip`을 더했다. `VipManager`는 모든 노드(위트니스 포함)가 `vip`을 보고할 때만
  `VipHolder`를 쓴다(옛 노드는 `vip-holder` 로그 항목을 풀지 못해 리더가 바뀌면 모든 Raft 쓰기가 멈춘다). 에이전트도
  기능이 모자라면 경고하고 갱신하지 않는다.
- **남은 VIP 정리.** 붙인 VIP(인터페이스, 주소/접두 길이)를 `{data_dir}/vips`에 붙이기 전에 적고(적지 못하면 붙이지
  않음) 뗀 뒤 지운다(새 파일에 써서 바꿔 끼움). opld는 시작할 때 `Member::start` 전에 기록의 모든 주소를 지금 설정과
  관계없이 기록한 접두 길이 그대로 뗀다(접두 길이가 달라 `del`이 "Cannot assign requested address"로 실패하고 성공으로
  넘어가는 일이 없다). 유닛의 `ExecStopPost=/usr/bin/orbivisord --config /etc/opl/opld.toml --release-vips`가 SIGKILL
  뒤에도 같은 일을 한다(설정의 `data_dir`만 읽고, 설정·네트워크 오류도 0으로 끝나 재시작을 막지 않는다). 데이터 디렉터리
  잠금(`opld.lock`)을 막지 않고 잡아 보고, 다른 프로세스(돌고 있는 데몬)가 쥐고 있으면 아무것도 떼지 않는다. 인터페이스가
  없어(`Cannot find device`) 떼지 못한 항목은 경고하고 기록에서 뺀다.
- 설정 `[vip] addresses = ["a.b.c.d/nn", …]`(IPv4만, 접두사가 없으면 /32), `interface`. Linux가 아니거나 위트니스이거나
  `interface`가 없거나 같은 IP가 두 번이면 설정 오류다. IPv6 VIP는 gratuitous ARP 대신 이웃 광고가 필요해 뺐다.
- **알려진 한계.** 두 노드가 같은 VIP를 갖지 않는 것은 orbivisord가 도는 동안의 성질이다. 겹칠 수 있는 때: kill -9·
  SIGKILL로 죽은 뒤 재시작이나 `ExecStopPost`가 떼기까지, 프로세스·VM이 멈춘 뒤 watchdog SIGKILL까지, 호스트 절전
  중(리스 마감을 재는 단조 시계가 멈춘다). 설정에서 주소를 뺀 경우는 기록으로 뗀다. 헬스의 적용 지연은 리더보다
  뒤처진 노드를 잡지 못하는 어림이고, 실행기 확인이 한 번 늦으면 10초 동안 503이 되어 VIP가 옮겨 갈 수 있다. VIP 갱신은
  갱신마다 리더가 `confirm_leader`를 하므로 노드 수의 제곱만큼 메시지가 든다(계획 47의 리스 확인 묶기에서 함께 줄인다).
  설정에서 뺀 VIP의 `VipHolder`는 남는다(해롭지 않음). BGP 모드(§4)는 계획 49 뒤다.
- 시험: `plan`·`wanted`·`renew_limit`·주소 파싱·명령 인자·VIP 기록은 단위 시험(macOS에서도 돈다), opl-cluster VIP 리스
  표(갱신·고립·펜싱), opl-api `VipManager`·`VipAgent`(가짜 노드 갱신과 가짜 `VipNet`, 리스 2초 — 바쁜 CI에서 갱신이
  잠깐 밀려도 "옮기지 않음"이 깨지지 않게; 옛 노드가 있으면 쓰지 않음, 늦게 깨면 먼저 뗌, 기록), `--release-vips`가
  오류에도 0으로 끝남(opld 프로세스), Linux CI(root, `net-it`)에서 opl-driver가 이름공간의 dummy 인터페이스에 붙이고
  떼기, opld 둘과 위트니스가 각자의 dummy 인터페이스로 VIP를 두 노드가 함께 갖는 때가 없고(헬스가 잠깐 503이 되어
  주인이 한 번 바뀌는 것은 받아들인다) 멈추면 옮겨 가고 kill -9 뒤 남은 VIP를 다시 시작할 때와 프로세스 없이
  `--release-vips`로 떼기.

**UI 따로 내보내기(§4).**
- 설정 `[web] enabled = false`면 API 라우터만 연다(`/api/` 밖은 404, 보안 헤더 없음).
- `packaging/portal/build-web.sh <version> <out dir>`가 `web/dist`(또는 `OPL_WEB_DIST`)로 `orbivisor-web-<version>.tar.gz`
  (안은 `orbivisor-web-<version>/`, 소유자 0)를 만든다. `nginx.conf`(노드와 같은 보안 헤더, `/assets/` 오래 캐시,
  화면 주소는 `index.html`, `/api/`는 404)와 `Dockerfile`(nginx-unprivileged, 8080) 예. 릴리스 작업 흐름이 아직
  없으므로 묶음은 이 스크립트로 만든다. `packaging/tests/portal-test.sh`가 CI shell 작업에서 돈다.

**배포 뒤 확인(테스트 VM, UTM).** CI는 한 호스트의 dummy 인터페이스로만 VIP를 본다. 실제 L2에서 손으로 확인하고
결과를 적는다.

| 확인 | 결과 |
|---|---|
| 내장 VIP: 주인 노드의 전원을 끈 뒤 다른 노드가 VIP를 갖기까지(`ping -i 0.2 <VIP>`가 끊긴 시간, 3~4초 기대) | 아직: 테스트 VM은 실제 LAN이라 주소 충돌을 피해 하지 않음. Linux CI `net-it`(dummy 인터페이스)에서 kill -9 뒤 첫 노드로 돌아오기까지 7.5~8.9초(재선출 포함) |
| 내장 VIP: `systemctl stop opld` 뒤 옮겨 가기까지, 주인의 cluster_listen을 막았을 때 주인이 스스로 떼는지(`ip addr`) | 아직 |
| 내장 VIP: 이웃(다른 호스트)의 ARP 캐시가 새 주인의 MAC으로 바뀌는지(`ip neigh`, `arping` 있음·없음) | 아직 |
| HAProxy: `option httpchk GET /api/v1/health`로 유지보수·`roles none` 노드가 DOWN이 되고(약 `inter × fall`) 돌아오면 UP | 일부(2026-11-12, 테스트 VM): HAProxy 없이 `curl`로 헬스가 로그인 없이 `{status, failed}` 200, `roles none`이면 503 `failed: ["role"]`(`?live`는 200), `default`로 돌리면 200. `cluster api-endpoints -o haproxy` 출력 확인. 실제 HAProxy는 아직 |
| HAProxy 뒤에서 로그인·콘솔(WebSocket)·노드 하나를 끈 뒤에도 세션이 이어지는지, 감사 로그의 요청 주소가 클라이언트 주소인지 | 일부(테스트 VM): svc01에서 로그인한 쿠키로 svc02 API 200, svc02에서 로그아웃하면 두 노드 모두 401. `api_names`를 넣으면 2초 안에 재시작 없이 두 노드가 그 SAN(CN `orbivisor-api`)으로, 비우면 원래 인증서로. HAProxy·콘솔·감사 주소는 아직 |

## 14. 구현(계획 47)

네 단계를 커밋 넷으로 했다(1 리스 확인 묶기와 모으기 상한, 2 노드 감시를 리스로, 3 투표 멤버 상한과 VoterKeeper,
4 규모 시험과 문서). 설계 §5와 다르거나 설계가 정하지 않아 정한 것을 적는다.

**리스 확인 묶기와 모으기 상한(§5).**
- opl-cluster `ConfirmBatch` 하나를 HA 리스(`LeaseTable::renew`)와 VIP 리스(`VipTable::renew`)가 함께 쓴다. 요청은
  아직 시작하지 않은 회차에 붙고, 배경 작업이 그 회차를 가져간 뒤에야 확인(`confirm_leader`)을 시작한다. 그래서 요청은
  자기가 온 뒤 시작한 확인의 결과만 쓰고(보내기 전 시각 ≤ 확인 시각), 확인을 많아야 두 번 기다린다. 회차의 마감은
  기다리는 요청 가운데 가장 늦은 것이고 각 요청은 자기 마감에 끝난다. 갱신마다 앞뒤 두 번 확인하는 것(기록 전 리더
  확인, 펜싱 검사 뒤 돌려줄 인덱스)은 그대로다.
- **묶기의 효과는 확인 왕복이 길 때만 난다(잰 값).** 노드 300대가 2초마다 갱신하면 갱신은 초당 150개, 평균 6.7 ms
  간격이다. 확인 왕복이 그보다 짧으면 회차에 요청이 하나씩만 붙는다. 규모 시험에서 갱신 하나가 쓰는 투표 멤버 RPC는
  노드 100(시험 빌드)에서 8.1(묶이지 않음 = 확인 2번 × 투표 멤버 넷), 노드 300 시험 빌드에서 2.2~2.6, 배포 빌드에서
  6.4였다. 갱신이 한꺼번에 몰릴 때(리더가 바뀐 직후, 네트워크가 돌아온 직후)와 왕복이 긴 실제 네트워크에서 줄어든다.
- 확인 작업은 `Cluster::open` 때 잡은 실행기 핸들(`core`)에 띄운다(요청한 쪽의 실행기가 아니다). 확인 작업이 제대로
  끝나지 않으면(확인 함수 panic, 실행기가 멈춰 작업이 도중에 또는 돌기 전에 버려짐) drop 가드가 `running`을 되돌리고 다음
  확인을 기다리던 요청에 오류를 준다(그러지 않으면 뒤의 갱신이 모두 제한 시간까지 기다린다). 가드는 정상으로 끝날 때만
  `finished`를 세우고, 띄우기 전에 만들어 작업으로 넘긴다(2차 리뷰).
- 지표·감사 로그 모으기는 `fan_out::gather`(동시에 32개)다. 설계대로 `buffer_unordered`로 돌리되 순번을 붙여 끝난 뒤
  정렬한다(느린 노드 하나가 다음 묶음을 막지 않고, 결과는 노드 순서로 남아 같은 시각의 지표 표본 순서가 바뀌지 않는다).
  처음에 쓴 `buffered`는 첫 노드가 느리면 그 뒤가 시작하지 못했다.

**노드 감시를 리스로(§5).**
- 노드 기능 `lease-liveness`. 모든 Raft 구성원(위트니스 포함)이 보고하면 `NodeMonitor`가 ping 없이 리더의 리스 표로
  `NodeStatus.online`을 정하고, 아니면(롤링 업그레이드) 예전 ping 감시다. 구성원이 아닌 노드 기록(빠진 옛 노드의 남은
  기록)은 판단에서 뺀다(그런 노드는 어느 쪽이든 오프라인이다). 그래도 ping으로 보면 임기마다 한 번 막는 노드 이름을
  경고한다. HA 펜싱을 끈 노드와 위트니스는
  `renew_liveness`(opld `LIVENESS_RENEW` 2초, `core` 실행기)로 펜싱 없이 리스만 갱신하고, 정상 종료는 오프라인 기록
  전에 그것을 멈춘다.
- 판단: 리더 자신은 늘 온라인, Raft 구성원이 아니면 오프라인. 온라인으로 기록된 노드는 "마지막 갱신"과 "이 임기에
  리스 모드를 시작한 때" 가운데 늦은 것에서 10초가 지나면 오프라인이다(새 리더는 한 리스를 기다린다). 오프라인으로
  기록된 노드는 그 기록(리소스 revision)을 본 뒤에 받은 갱신으로만 돌아온다(정상 종료한 노드가 옛 갱신으로 되살아나지
  않게). 쓰기 직전에 그 노드의 `online`이 목록을 읽은 뒤 바뀌었으면 건너뛴다(ping 모드도).
- HA 복구 중(`Fenced`)인 노드의 갱신은 거절되므로 그동안 돌아와도 오프라인이다.
- ping 감시가 없어지면 리더가 2초마다 노드마다 보내던 gRPC `Ping`(노드 300대에서 초당 150)이 사라진다. 모든 노드가
  업그레이드된 뒤의 일이다. 규모 시험은 opl-cluster 안에 있어 opl-peer의 `NodeMonitor`를 띄우지 않는다(리스 갱신은
  모든 노드가 한다).

**투표 멤버 상한과 VoterKeeper(§5).**
- 클러스터 설정 `voters: Option<u8>`(3·5·7, 없으면 `DEFAULT_VOTERS` 5, 위트니스도 센다). 옛 노드가 풀지 못하므로
  바꾸려면 모든 노드가 현재 빌드여야 한다. `orbivisorctl settings set voters=N`, `node list`의 `VOTER` 열.
- 노드 설정 `[node] zone`은 등록 때 앞뒤 공백을 잘라 노드 레이블 `zone`으로 적는다(없으면 레이블을 건드리지 않아 API로
  둔 값이 남는다). OpenAPI의 `voters`는 3·5·7(최소 3, 최대 7)로 적었다.
- `VoterKeeper`(위트니스가 아닌 노드가 `core`에서 띄우고 리더일 때만 일함, 1초): 임기마다 한 번 `read_barrier` 뒤 Node 목록을
  읽고 순수 함수 `voters::plan`이 틱마다 바꿀 것 하나를 고른다. 순서: 위트니스 learner 승격 → 상한보다 적으면 건강하고
  (리스 10초 안) 따라잡은(리더의 마지막 로그에서 64항목 안) learner를 투표 멤버가 적은 zone부터 승격 → 많으면 강등
  (오프라인, 살아 있지 않은 것, 붐비는 zone, 건강하지 않은 것, 나중에 들어온 것 순, 새 임기의 처음 10초는 하지 않음) →
  상한만큼이면 오프라인(리스 30초) 투표 멤버를 올릴 수 있는 learner로 교체. 리더와 위트니스는 내리지 않는다.
- **오프라인 교체는 대신할 노드가 있을 때만, 변경 한 번으로(1차 리뷰).** 처음에는 오프라인 투표 멤버를 먼저 내리고 다음
  틱에 다른 노드를 올렸다. 그러면 노드 둘과 위트니스·노드 셋에서 B가 30초 넘게 꺼진 동안 내려지고, 그 뒤 A가 죽으면 B와
  위트니스로 정족수가 안 된다. 지금은 올릴 수 있는 learner가 있을 때만 `ReplaceAllVoters(투표 멤버 − 죽은 노드 + 대체)`
  한 번으로 바꾸고, 노드가 상한 이하이면 오프라인이라는 이유로 내리지 않는다(계획 47 전과 같이 모두 투표 멤버).
- **과반은 최근의 생존으로 센다(1차 리뷰).** 처음에는 "건강"(리스 10초, 새 리더는 기준점 유예)으로 과반을 셌다. 새
  리더에게는 죽은 투표 멤버도 유예 동안 건강해 보여, 살아 있는 투표 멤버를 내리는 변경이 나면 바꾼 뒤의 구성에 살아 있는
  과반이 없어 joint가 커밋되지 못하고 클러스터 쓰기가 멈출 수 있었다. openraft 0.9.25의 `RaftMetrics`에는 팔로워별 최근
  응답·하트비트 시각이 없다(`replication`은 맞춘 로그 id뿐). 그래서 "살아 있음"은 이 임기에 이 리더가 실제로 받은 리스
  갱신이 `alive_within`(3초, 갱신 간격 2초) 안인 것이다(리더 자신은 늘 참, 기준점 유예 없음). 바꾸기 전과 뒤의 구성(joint의
  두 구성) 모두에서 살아 있는 투표 멤버가 과반일 때만 바꾸고, 새 임기의 처음 `healthy_within`(10초) 동안은 개수 때문에
  내리지 않는다.
- **끊긴 변경을 마무리한다(1차 리뷰).** openraft의 `change_membership`은 joint를 커밋한 뒤 uniform을 제안하는 두 단계라,
  변경 제한 시간(`tokio::time::timeout`), 종료 취소, 리더 교체로 그 사이에 끊기면 joint가 남는다(openraft 문서: "If it
  loses leadership or crashed before committing the second uniform config log, the cluster is left in the joint config").
  처음 키퍼는 joint를 보면 매번 돌아가 영원히 멈췄다. 지금은 키퍼 상태에 끝을 보지 못한 변경과 본 joint(로그 id, 처음 본
  때)를 두고, 이 키퍼의 변경이 실패·시간 초과했거나, joint가 이전 임기의 것이거나(그 리더의 두 번째 단계는 오지 않는다),
  `change_timeout` 넘게 그대로이면 `change_membership(ChangeMembers::AddVoterIds(빈 집합), retain)`으로 마무리한다. 어느
  때든 joint가 바뀌지 않은 채 간격 두 번 넘게 보였어야 한다(살아 있는 두 번째 단계가 먼저 끝나게, 2차 리뷰).
  openraft 0.9.25 `Membership::change`의 `AddVoterIds`는 목표를 "마지막 구성 ∪ 더하는 것"으로 두고 `next_coherent`의
  `find_coherent`는 목표를 품은 joint에서 그 목표 하나(uniform)를 주므로 첫 단계 하나로 joint가 끝난다(blocking
  `change_membership`은 첫 단계가 uniform이면 바로 돌아온다). joint가 아직 커밋되지 않았으면 `InProgress`로 거절되고 다음
  틱에 다시 본다. 시험은 memnet `MemRouter::lose_replies`(보낸 RPC는 닿고 답만 사라짐)로 팔로워에 joint만 남기고 리더를
  멈춘다.
- **지운 노드가 learner로 남지 않는다(2차 리뷰).** 마무리는 빠지는 투표 멤버를 learner로 남긴다(`retain`). 노드를 지우는
  변경(`LeaderRequest::Remove`, API의 노드 삭제와 `cluster leave`, `retain = false`)의 두 번째 단계보다 키퍼가 먼저
  마무리하면 늦은 두 번째 단계는 할 일이 없어 Node 기록 없는 learner가 남았다. `Remove`는 성공한 뒤 멤버십을 다시 읽어 그
  노드가 learner로 남았으면 `RemoveNodes`로 지운다. 요청 자체가 끊겨 실패했으면 다시 하면(같은 요청이 learner를 지운다)
  된다. 키퍼는 바꾸기 직전에 지표를 다시 읽어 구성(로그 id)·임기·리더가 계획을 세운 때와 다르면 건너뛰고, 마무리 실패
  경고는 joint마다 한 번 남긴다.
- **`InProgress`면 다시 계획한다.** 다른 변경(위트니스 자기 승격, API의 노드 제거, 노드 추가)이 커밋 중이면 같은 변경을
  다시 시도하지 않고 다음 틱에 최신 구성으로 계획을 다시 세운다.
- 업그레이드: 노드 기능 `voter-limit`. 모든 Raft 구성원의 Node가 그것을 보고해야 움직인다. 합류하자마자 죽어 등록하지
  못한 learner(Node 기록이 없거나 기능·버전이 비었고 이 임기에 리스도 없다)는 빼고 본다(1차 리뷰: 그런 노드 하나가 키퍼를
  영원히 멈췄다). 그래도 막히면 임기마다 한 번 막는 노드 이름을 경고한다. 합류·재시작한 노드는 같은 규칙(공용
  `opl_cluster::leader_manages_voters`, 자기 자신은 빼고, 리스 표를 모르므로 갱신한 멤버는 없다고 봄)으로 리더가 고른다고
  보면 승격을 요청하지 않고 learner로 기다리고, 아니면 예전처럼 요청한다(지금의 `promote_until_voter`). 2차 리뷰: 처음에는
  노드 쪽이 등록 못 한 learner를 빼지 않아, 그런 learner가 있으면 키퍼는 움직이는데 노드가 스스로 올라가 승격·강등이
  반복될 수 있었다. 노드 쪽은 키퍼보다 덜 막지 않으므로 그 반대(키퍼가 막혔는데 노드가 기다림)만 잠시 생길 수 있다.

**규모 시험(§5).**
- `opl-cluster/tests/scale.rs`(기능 `scale-it`, 노드 수 `OPL_SCALE_NODES`, 기본 300). memnet으로 노드를 한 프로세스에
  띄우고 투표 멤버 다섯(노드 1~5로 초기화)에 나머지를 learner로 더한 뒤, 모든 노드가 Node(zone 셋, `voter-limit`)를
  등록하고 2초마다 리스를 갱신하며(시작 시각을 간격 안에서 고르게) `VoterKeeper`를 띄운 채로 잰다. Raft 설정은 opld와
  같다(하트비트 200 ms, 선거 1~2초). 리더의 RPC는 시험의 네트워크·전달자 감싸개가 센다(보낸 Raft RPC와 전달받은 요청).
  CI `check` 작업(Linux·macOS)이 노드 100개로 돈다(단계 제한 10분, 개발 Mac에서 15초쯤). 리더 장애를 기다리는 것은 10초면
  그 자리에서 실패한다. 재는 동안 투표 멤버가 처음의 다섯 그대로인지도 확인한다.
- **저장소를 메모리에 둔다(설계에 없음).** `ClusterConfig.in_memory`(redb `InMemoryBackend`, fsync 없음)를 더했다. 시험용이라
  문서에서 숨기고(`#[doc(hidden)]`) 켜면 `Cluster::open`이 경고를 남긴다(다시 켜면 Raft 로그와 투표가 사라진다). 처음에
  파일 저장소로 노드 100개를 띄우자 쓰기 p50 541 ms, p99 1.01초였다. 노드 100개의 fsync(macOS `F_FULLFSYNC`)가 디스크
  하나에서 줄을 서기 때문이다(실제 노드는 디스크가 따로다). 투표 멤버 다섯만 파일에 두어도 노드 여섯에서 p50 45 ms,
  p99 64 ms였다(노드 300에서 p50 53 ms, p99 67 ms: 노드 수의 몫은 몇 ms). 그래서 시험의 쓰기 p99는 Raft와 리더의 일만
  재고, 실제 노드의 쓰기 지연에는 그 노드 디스크의 fsync 시간이 더해진다(README에 적었다).
- **시험 빌드에서 redb를 최적화한다.** 메모리 저장소로 노드 300개면 시험 빌드에서 쓰기 p50 104 ms, p99 197~382 ms였다.
  최적화하지 않은 redb가 커밋마다 새 페이지를 0으로 채우는 데(`zero_filled_arc`) CPU를 썼다(macOS `sample`). 워크스페이스
  `[profile.dev.package.redb] opt-level = 3`로 p50 18 ms, p99 32~33 ms가 되었다(배포 빌드는 원래 최적화한다).
- **리더 장애 기준을 5초로 했다(설계 3초).** openraft 0.9.25는 리더가 있던 팔로워가 마지막 하트비트 뒤 리더
  리스(`election_timeout_max`, 2초)와 선거 제한 시간(1~2초 무작위)을 더 기다린 뒤에야 선거를 시작한다(`RaftCore::
  handle_tick_election`). 그래서 opld의 Raft 설정으로는 노드 수와 관계없이 3~4초다: 노드 여섯 3.3~3.4초, 100개 3.3~3.4초,
  300개 3.2~3.5초(배포 빌드 3.3초). 당선 뒤 자기 임기의 항목을 적용하기까지는 노드 300에서도 300 ms 안이었다. 기준은 이론상
  최대 4초에 투표·확정과 바쁜 CI의 여유 1초를 더한 5초다(`OPL_SCALE_FAILOVER_MS`로 바꿀 수 있고 CI는 기본값 그대로다). 3초 아래로 줄이려면 선거 시간을 줄여야 하고(바쁜 노드에서
  불필요한 선거가 는다) 그것은 빠른 HA(계획 51)에서 다룬다.
- **리더 RPC 기준은 노드당 초당 10이다.** 설계는 기준 없이 재라고만 했다. 노드 하나가 리더에 주는 RPC의 위쪽: 하트비트 5(200
  ms), 리스 갱신 전달 0.5, 그 갱신의 리더 확인을 묶지 못했을 때 2번 × 투표 멤버 넷 = 4, 합 9.5. 잰 값(초당, 아래 표는
  2차 리뷰 전 지연 없이 잰 것):

  | | 노드 100 | 노드 300 | 노드 300(배포 빌드) |
  |---|---|---|---|
  | 리더 RPC 합 | 760~780 | 1,480~1,540 | 2,120 |
  | learner에게(하트비트) | 300~320 | 940~1,000 | 1,000 |
  | 투표 멤버에게(하트비트·리더 확인) | 410 | 330~400 | 970 |
  | 전달받은 리스 갱신 | 50 | 140~150 | 150 |

  투표 멤버에게 간 RPC는 "묶지 않은 최악" = 하트비트(팔로워 넷 × 초당 5) + 리스 갱신 수(전달받은 것과 리더 자신의 것)
  × 확인 2번 × 투표 멤버 수(5)를 넘지 않아야 하고, 노드 300부터는 그 0.6배 아래여야 한다(1차 리뷰: 노드당 기준만으로는 묶기가
  깨진 것을 잡지 못했다). 최악은 확인 하나가 팔로워 넷에게 가는 것을 투표 멤버 다섯으로 세어 여유를 두었다: 묶지 않게
  고친 빌드는 노드 300에서 팔로워 넷으로 센 값(1,154)을 넘은 1,162였다(다섯으로 세면 1,435의 0.81이라 비율 검사에서
  실패한다). 지연 없이 잰 값은 노드 100에서 410(최악 520), 노드 300 시험 빌드에서 385(최악 1,510, 0.25), 배포 빌드에서
  970(약 0.64)이었다. 확인 왕복이 한 컴퓨터 안에서는 갱신 간격보다 짧아 묶이는 정도가 기계·빌드 속도에 좌우되었고, 비율
  검사는 노드 300에서만 돌아 CI(노드 100)는 묶기가 깨져도 몰랐다.
- **확인 묶기 검사는 지연을 넣어 결정적으로 한다(2차 리뷰).** 리더 RPC를 재는 5초 동안만 시험의 네트워크가 투표 멤버로
  가는 `append_entries`(하트비트·리더 확인)를 20 ms 늦춘다. 확인은 한 번에 하나라 초당 많아야 50번이고, 노드 100에서도
  확인 요청(갱신 50/s × 2)보다 적다. 그래서 투표 멤버 RPC는 묶지 않은 최악의 0.42를 넘을 수 없고 기계·빌드와 관계없다.
  검사(최악의 0.6 미만)는 노드 수와 관계없이 돌아 CI의 노드 100에서도 검사한다. 잰 값: 노드 100 196(최악 519, 0.38, 배포
  빌드도 0.38), 노드 300 158~162(최악 1,575~1,641, 0.10). 확인을 묶지 않게 고친 빌드는 노드 100 411(0.79), 노드 300
  1,204(0.80)로 실패했다. 지연을 넣은 구간의 리더 RPC 합은 노드 100 570, 노드 300 1,320이다(노드당 10 미만 검사도 이
  구간 값). 쓰기 지연·노드 추가·리더 장애는 지연 없이 잰다.
  learner 하나에 가는 하트비트는 초당 약 3.4였다(설정은 200 ms). 노드 300에서 리더 RPC의 3분의 2가 learner 하트비트이고
  이것은 관찰 노드(계획 48)가 없앤다.
- **CI의 쓰기 p99.** 기준(50 ms)은 환경 변수 `OPL_SCALE_P99_MS`로 바꿀 수 있고 CI는 200 ms로 둔다. CI 러너는 코어가 3~4개라
  노드 100개를 한 프로세스에서 돌리면 쓰기 지연이 CPU에 묶여 흔들린다(개발 Mac 10코어에서 노드 100은 8~11 ms). CI의 값은
  쓰기가 멈추거나 노드 수에 따라 크게 느려지는 것만 잡는다. 다른 기준(리더 RPC와 확인 묶기 비율, 노드 추가, 리더 장애)은
  CI도 같다.
- 나머지 잰 값(개발 Mac, Apple Silicon 10코어, 시험 빌드): 쓰기 지연(리더에서 하나씩 200번) p99 노드 6에서 0.9 ms,
  100에서 9~11 ms, 300에서 32~33 ms(배포 빌드 17 ms). 노드 수에 거의 비례하는 이 몫은 쓰기마다 리더가 learner 모두에
  복제하는 일과, 한 프로세스의 learner 모두가 그 항목을 쓰고 적용하는 CPU다. 노드 하나 추가(learner로 따라잡기까지)
  노드 100에서 9~21 ms, 300에서 65~150 ms. 노드 300개를 띄우는 데 40초(learner를 하나씩 더함). 실패한 리스 갱신은 없었다.
- **느린 디스크 노드.** openraft 0.9.25는 자기 로그 쓰기가 끝나기를 기다리므로(§11) 디스크가 아주 느린 투표 멤버는
  과반 확정을 늦출 수 있다. 느린 디스크를 재어 투표에서 빼는 규칙은 넣지 않았다. 그런 노드가 리스를 30초 넘게 갱신하지
  못하면 `VoterKeeper`가 올릴 수 있는 learner로 바꾸는 것까지만 된다(리더는 내리지 않는다). 쓰기 지연을 재어 내리거나 리더를
  넘기는 규칙은 뒤 계획으로 미룬다.
