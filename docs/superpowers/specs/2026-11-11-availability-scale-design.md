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

## 15. 구현(계획 48)

다섯 단계를 커밋 다섯으로 했다(1 백엔드 틈과 공통 적용 리비전, 2 Follow 피드와 복제본, 3 gRPC Follow·관찰 노드 목록·API,
4 전환·VoterKeeper·opld, 5 규모 시험과 문서). 설계 §6과 다르거나 설계가 정하지 않아 정한 것을 적는다.

**역할과 수.**
- 대기 learner는 상수 `STANDBY_LEARNERS = 2`다(설정으로 두는 것은 나중). 위트니스는 투표 멤버로 센다. 투표 멤버와 대기
  learner가 아닌 노드가 관찰 노드다.
- 가입은 지금처럼 learner로 하고 따라잡은 뒤 키퍼가 관찰 노드로 내린다. 처음부터 관찰 노드로 가입하는 것은 나중이다.
- 바꿀 의도는 클러스터 정보의 `ClusterStatus.observers`(노드 id → 클러스터 주소)다. 리더의 `VoterKeeper`만 쓰고, 옛
  노드는 모르는 필드를 무시한다(비면 쓰지 않는다). `Members.observers`는 그 가운데 Raft 멤버십에 없는 것이다(내려가는
  중인 노드는 멤버로 센다). 노드 기능 `observer`를 모든 Raft 구성원과 관찰 노드가 알릴 때만 내린다.

**리비전과 적용 표시.**
- 리비전은 Raft 로그 인덱스(META `last_applied`)다. blank·멤버십·거절된 명령도 `last_applied`를 올리지만 감시 이벤트를
  내지 않아 리비전에 틈이 있다. 그래서 `WatchHub`는 적용한 항목마다 `FeedBatch { events, applied: LogId, membership }`를
  쌓는다(이벤트 없는 항목도 적용 표시로). 공개 `Watch`는 이벤트로 펼치고, 기록(`HISTORY` 1024)은 이제 묶음 수다.
- `Cluster::revision()`·`wait_revision(rev, timeout)`은 `WatchHub` 안의 적용 위치 채널을 본다. 쓰기 전달·`wait_local`·
  `renew_lease`·`renew_vip`의 적용 기다림(`raft.wait().applied_index_at_least`)을 이것으로 바꿔 관찰 노드에서도 같다.
  리스 기다림이 blank 항목에서 멈추지 않는다.
- `Cluster`는 저장소, Raft를 다시 만드는 팩토리, `NodeHandle`(백엔드 `Member { raft, confirm } | Observer(Replica)`와 리스
  표)을 갖는다. `Cluster::raft()`는 `Option`이고, 관찰 노드에서 `read_barrier`·리더 일·`initialize`·멤버십 변경은
  `NotLeader`(아는 리더를 담음), `leases()`·`vip_leases()`는 빈 값이다. memnet은 노드의 `NodeHandle`을 등록해 RPC마다
  지금 백엔드를 읽는다.

**Follow 피드(§6의 `Follow { since_revision }`).**
- 프레임은 `Hello { role, leader, voters }`, `SnapshotPart { objects }`, `SnapshotEnd { applied, membership }`,
  `Batch { events, applied, membership }`, `Progress { applied, leader, leader_addr }`(JSON, 태그 `frame`)다. 모든 `Batch`·
  `Progress`가 적용한 `LogId`를 싣는다(리비전 틈 때문에, 또 다시 Raft 구성원이 될 때 로그를 이어 쓰게). `/watch`의
  이벤트 형식을 그대로 쓴다.
- 서버(`Cluster::follow(caller, since)`)는 `since`가 기록보다 오래면 지금 상태의 스냅샷(리소스 256개 또는 JSON 약 4 MiB씩
  조각, gRPC 메시지 한도 64 MiB)부터, 자기가 스냅샷을 설치하면(`Reset`) 다시 스냅샷을 보낸다. 1초마다 `Progress`다
  (스냅샷을 뜨는 동안에도 보낸다 — 큰 상태를 읽는 동안 받는 쪽이 진행이 없다고 끊지 않게).
  이벤트 JSON이 약 4 MiB를 넘는 묶음은 앞부분을 `BatchPart { events }`로 나누고 마지막 `Batch`가 적용 위치를 실어,
  관찰 노드는 마지막 프레임에서 한 번에 적용한다(끊기면 앞부분을 버린다).
- `since`가 서버의 리비전보다 앞선 호출자(다른 투표 멤버에서 옮겨 온 관찰 노드)는 받지 않는다(`Hello`만 보내고 닫음).
  받으면 뒤처진 서버가 그 사이 리더의 스냅샷(관찰 노드보다 낮을 수 있음)을 설치할 때 관찰 노드가 그것으로 바뀌어
  리비전이 뒤로 가고, 이미 돌려준 리비전 읽기·리스 갱신이 거짓이 된다(옮긴 VM이 다시 자기 것으로 보여 이중 실행
  위험). 투표 멤버 자신은 커밋보다 낮은 스냅샷을 설치하지 않는다(openraft 0.9.25 `install_full_snapshot`). 복제본도
  자기보다 낮은 스냅샷은 설치하지 않는다(이중 방어).
  서버는 보내고 읽히지 않은 프레임이 16개면 보내기를 멈추고, 그동안 서버의 피드가 기록(`HISTORY` 1024 묶음)보다
  뒤처지면 `Lagged`로 끝나 느린 관찰 노드는 다시 붙는다. 같은 묶음은 스트림이 몇이든 한 번만 프레임으로 나눈다
  (이벤트 복제와 JSON 크기 재기를 관찰 노드마다 하지 않게). 구성원이 아닌 서버는
  `Hello`만 보내고 닫고(다른 데로 가라), Raft가 멈추거나 서버가 멤버십에서 빠지면 끝난다. `Hello.role`은 서버가 보는
  부른 쪽의 역할이다.
- gRPC는 서버 스트리밍 `Follow`(`FollowRequest { since }` → `FollowFrame` JSON, 제어 채널)이고 서버는 `core` 실행기에서
  답한다. `authorize`는 관찰 노드 목록의 노드도 받고 목록과 멤버십 모두에서 빠진 노드는 거절한다. 열린 Follow
  스트림도 프레임마다 다시 본다(`FollowGate`): 부른 쪽이 Raft 멤버십이나 관찰 노드 목록에 있어야 하고, 목록은 그
  스트림의 프레임에 실린 클러스터 정보의 것을 먼저 본다(노드의 목록 캐시는 따로 갱신되어 늦을 수 있다). 그래서 지운
  관찰 노드는 자기를 목록에서 빼는 묶음부터 받지 못하고 스트림이 끝나 그 뒤의 쓰기(CA 키 같은 비밀 포함)를 받지
  못한다. 관찰 노드에 온 Raft RPC는 `failed_precondition`이다.

**복제본.**
- 관찰 노드는 리더가 아닌 투표 멤버 가운데 자기와의 rendezvous 해시가 가장 큰 것에 붙는다(관찰 노드가 고르게 퍼진다,
  없으면 리더). 리더에 붙었고 다른 투표 멤버가 있으면 옮긴다(다른 투표 멤버가 모두 실패해 리더에 온 경우는 머문다).
  오류·끝·3초 진행 없음(`FOLLOW_STALL`)이면 자기 리비전부터 다시 붙고, 진행 없이 실패한 노드는 다음 고르기에서 건너뛴다.
- 살아 있어 `Progress`는 보내지만 리더를 따라가지 못하는 투표 멤버도 떠나 건너뛴다(리스 갱신이 계속 실패해 스스로
  펜싱하지 않게): 전달한 쓰기·리스 갱신이 돌려준 리더의 인덱스를 붙은 노드가 3초(`UPSTREAM_LAG_MAX`) 넘게 알리지
  않으면(`Progress`·묶음·스냅샷의 적용 위치 — 이 노드의 적용이 아니라 붙은 노드가 알린 것으로 재, 이 노드의 디스크가
  느린 것을 붙은 노드 탓으로 돌리지 않는다). 전달 경로의 적용 기다림이 시간 초과하면 곧바로 다시 본다(그때 붙은 노드가
  그 인덱스를 알렸으면 머문다). 이 기다림은 남은 시간의 90%에서 끝내, 같은 제한 시간을 건 리스 갱신의 바깥
  `timeout`이 future를 버리기 전에 닿는다. 스냅샷 조각을 받는 동안(첫 `SnapshotPart`부터 `SnapshotEnd`까지)과 나눈
  묶음을 받는 동안은 재지 않고, 스냅샷을 설치하면 새로 잰다(스냅샷이 3초 넘게 걸려도 끊고 다른 투표 멤버에서 처음부터
  다시 받기를 되풀이하지 않게). 헬스의 `apply_lag`도 그 인덱스를 본다.
- 스냅샷 조각은 `SnapshotEnd`까지, 나눈 묶음은 `Batch`까지 이어 와야 한다(서버는 그 사이에 다른 프레임을 넣지
  않는다). 아니면 모은 것을 버리고 다시 붙는다. 자기와 같은 리비전의 스냅샷은 설치하지 않는다(헛된 `Reset`이 없게).
- 투표 멤버마다 적용 묶음의 경계가 달라 옮긴 뒤 첫 묶음이 이미 적용한 리비전의 이벤트를 품을 수 있다. 그 이벤트는
  쓰지도 감시에 내지도 않는다(적용 위치는 묶음의 것).
- `apply_feed`는 리소스·이름·`last_applied`·멤버십을 한 쓰기 트랜잭션에 쓰고, 같은 묶음을 저장소 스레드에서 로컬
  `WatchHub`에 낸다(감시 이벤트가 투표 멤버와 같다). 스냅샷은 기존 상태 머신 설치와 같다.
- 쓰기는 리더로 바로 전달하고 돌려받은 리비전을 `wait_revision`으로 기다린다(보통 자기 쓰기를 곧바로 읽는다, 아래
  "쓰기 뒤 읽기").

**읽기와 헬스.**
- 기본 읽기는 지금 팔로워처럼 로컬이다. 요청 헤더 `Orbivisor-Min-Revision`이 있으면 그 리비전까지 5초 기다리고 넘으면
  503 `revision_unavailable`이다. `/api/v1`의 응답에 `Orbivisor-Revision`(답할 때 그 노드가 적용한 리비전)을 붙이고, 허용
  출처의 CORS 응답은 `Access-Control-Expose-Headers: orbivisor-revision`으로 그것을 포털 스크립트에 보인다. 기다림은
  인증(`Principal` 추출)에 들어 있다: 기다린 뒤 다시 인증하므로 다른 노드에서 막 만든 자격 증명(토큰·사용자·비밀번호)도
  그 리비전을 주면 받고, 기다리는 사이 폐기된 것은 거절한다. 인증되지 않은 요청은 이 노드가 있다고 아는 리비전
  (`Cluster::known_revision`: 멤버는 받은 로그의 마지막 인덱스, 관찰 노드는 붙은 노드가 알렸거나 리더가 돌려준
  인덱스)일 때만 기다리고(없는 리비전으로 연결을 붙잡지 못하게) 응답 헤더도 없다. 헬스는 헤더의 오류(400·503)를
  짧은 응답으로 바꾸지 않고 그대로 돌려준다.
- 쓰기 뒤 읽기: 전달한 쓰기는 그 리비전이 이 노드에 적용되기를 쓰기 제한 시간까지 기다리지만, 넘어도 쓰기는 됐으므로
  성공으로 돌려준다(그 뒤의 읽기가 잠깐 옛 값일 수 있다). 그래서 "보통 곧바로, 늦으면 쓴 리소스의 `revision`을
  `Orbivisor-Min-Revision`으로"다. 쓰기 응답의 `Orbivisor-Revision`도 그때 적용한 리비전이라 쓰기의 리비전보다 낮을 수 있다.
  리스 갱신은 다르다: 적용까지 확인되어야 성공이다.
- 관찰 노드의 헬스는 붙은 투표 멤버의 진행이 5초(`QUORUM_ACK_MAX`) 안에 왔고 리더를 알면 `quorum`이고, `apply_lag`는 붙은
  노드가 알린 적용 인덱스와 리더가 돌려준 인덱스 가운데 큰 것을 아직 적용하지 못한 시간이다.
- API `MemberView.role`(voter·learner·observer), `orbivisorctl cluster status`의 ROLE, 웹 클러스터 화면의 역할이 관찰
  노드를 따로 보인다. 가입의 `via`는 투표 멤버 주소다. 콘솔·펌웨어·마이그레이션의 노드 주소 찾기, 감사 로그·지표 모으기와
  janitor가 관찰 노드도 센다.

**전환.**
- 디스크는 `cluster.redb` 하나에 META `node_mode`(member|observer)다. `become_observer`는 이 노드가 joint의 어느 구성에서도
  투표 멤버가 아니고 그 멤버십이 적용됐을 때만 Raft를 끄고, 끈 뒤 다시 확인한다(그 사이 투표 멤버가 됐으면 Raft를 다시
  띄움). 그다음 한 트랜잭션에서 모드를 적고 LOG 표와 저장된 커밋 위치를 지우고 투표는 남긴 뒤 적용 위치부터 복제본으로
  따른다. 물러난 Raft의 늦은 적용은 저장소 스레드에서 세우는 표시(`StoreError::Retired`)로 막는다.
- `become_member`는 복제본을 멈추고 지금 상태 전체를 스냅샷으로 저장한 뒤 모드를 멤버로 적고 Raft를 띄운다. 로그가 비어
  있으므로 openraft 0.9.25가 시작할 때 마지막 로그를 적용 위치까지 지운 것(purge)으로 메우고(`StorageHelper::
  get_initial_state`) 리더가 그 뒤의 로그를 보낸다. 띄우지 못하면 관찰 노드로 돌아간다. `stopped()`는 계획된 전환을
  멈춤으로 보지 않는다.
- 다시 멤버가 되기 전에 관찰 노드가 되며 지운 로그의 마지막 인덱스(META `dropped_log`)까지 복제본으로 적용한다. 키퍼는
  목록에 쓴 뒤 `RemoveNodes`하므로 learner는 멤버십에서 빠지기 전에 관찰 노드가 될 수 있고, 빠지지 않은 채 다시 멤버가
  되면(내리던 중에 다시 올리기, `AddVoter`의 다시 보기와 멤버십 변경 사이의 목록 쓰기) 리더는 그 노드의 진행
  (`matching`, 노드가 받았다고 답한 로그)을 그대로 쥔다. 다시 띄운 Raft의 로그가 그보다 짧으면 openraft 0.9.25 리더가
  로그가 뒤로 간 것으로 본다(`ProgressEntry::update_conflicting`, 디버그 빌드는 `follower log reversion` panic). 노드가
  "멤버십에서 빠진 것을 본 뒤에야 관찰 노드가 되게" 할 수는 없다: openraft 리더는 빠진 노드의 진행을 그 멤버십을
  덧붙일 때 버리고(`rebuild_progresses`) 그 노드에 그 항목을 보내지 않는다. 지운 로그까지 적용하면 다시 띄운 로그
  (적용 위치까지 purge로 메우고 커밋으로 봄)가 리더의 `matching`을 덮는다.
- `RoleKeeper`(모든 노드, `core`, 1초)가 목록에 맞춘다: learner인데 목록에 있으면 관찰 노드, 관찰 노드인데 목록에 없고
  저장된 멤버십에 있으면 멤버, 투표 멤버는 그대로. 대비책은 설계가 생각한 "리더를 모름" 대신 "멤버 모드 비투표 노드의
  적용이 30초 멈춤"이다(openraft 0.9.25는 다시 시작한 learner가 옛 리더를 계속 알려 "리더 없음"이 생기지 않는다). 그러면
  투표 멤버에게 Follow로 `Hello.role`을 묻고 관찰 노드라면 따른다(꺼진 동안 내려진 노드).
- `VoterKeeper`: 대기 learner가 둘보다 많고 모든 노드가 `observer`를 알리면 건강한 learner 하나(대기가 많은 zone, 따라잡지
  못한 것, 나중에 들어온 것 순)를 목록에 쓰고(`UpdateStatusIf`) 두 틱 넘게 지나 그 노드에 복제됐거나 `change_timeout`이
  지난 뒤 `RemoveNodes`한다. 투표 멤버가 줄어 대기가 모자라면 건강한 관찰 노드를 `AddLearner` 뒤 목록에서 빼고(중간에
  끊기면 "목록에 있는 learner"가 되어 다시 내려간다 — 멤버십에도 목록에도 없는 노드가 생기지 않는다), 따라잡으면 보통의
  승격으로 투표 멤버가 된다. 새 임기의 처음 10초는 관찰 노드 전환을 하지 않는다. 리더는 목록에 있는 노드의 `AddVoter`를
  거절하고(따라잡기를 기다린 뒤 멤버십을 바꾸기 바로 앞에서 다시 본다), `LeaderRequest::Remove`는 목록에서도 뺀다. 고아(멤버십에도 목록에도 없는 Node 기록)는 고치지 않는다(지운
  노드가 되살아나지 않게).
- opld는 저장된 모드로 연다(`Cluster::open_node`). 관찰 노드는 승격을 요청하지 않고, `leave`·다시 가입이 관찰 모드를
  처리한다.
- 계획 48 전 바이너리는 `node_mode`를 몰라 관찰 노드의 저장소(LOG 비움)를 빈 로그로 연다. 옛 코드에 거절을 넣을 수
  없으므로 README에 "관찰 노드를 옛 버전으로 내리기 전에 클러스터에서 빼고 다시 합류"를 적었다.

**리더 일의 예외(§6 "리더의 일은 투표 멤버·learner 수에만 비례").**
- HA·VIP 리스 갱신은 관찰 노드도 지금처럼 리더로 전달한다. 펜싱의 안전이 리더의 리스 표와 갱신마다의 과반 확인에 기대기
  때문이다. 그래서 리더에는 노드당 초당 약 0.5 RPC(2초마다 갱신)와 그 리더 확인이 남는다. 확인은 `ConfirmBatch`로 묶여
  노드 수보다 천천히 는다(아래 잰 값). Raft RPC(하트비트·복제)는 관찰 노드 수와 관계없다. 갱신을 투표 멤버가 묶어 리더에
  전달하는 것은 나중이다.

**규모 시험(§5·§6).**
- `opl-cluster/tests/scale.rs`는 투표 멤버 다섯과 대기 learner 둘을 Raft로 띄우고 나머지를 관찰 노드로 연다. 관찰 노드는
  learner로 따라잡은 뒤 내리지 않고 빈 메모리 저장소로 바로 연다(시작 시간을 줄인다, 노드 300을 띄우는 데 계획 47의 40초 →
  2초). 그러려면 저장소에 멤버십이 없는 관찰 노드가 붙을 곳을 알아야 해서 시험용 `ClusterConfig::follow_seed`(문서에서
  숨김)를 더했다(저장된 멤버십에 투표 멤버가 없을 때만 쓴다). 클러스터 정보의 관찰 노드 목록에도 넣고 모든 노드가
  `VoterKeeper`·`RoleKeeper`를 띄운다. 재는 동안 투표 멤버와 대기 learner가 처음 그대로인지 확인한다.
- 기준: 리더가 비투표 구성원에게 보내는 Raft RPC 초당 15 미만(노드 수와 관계없이), 리더의 초당 RPC 합 35 + 노드당 4.5
  미만(리스 갱신 전달 0.5 + 그 확인을 묶지 못했을 때 2번 × 투표 멤버 넷 = 4; 계획 47은 learner 하트비트 5를 더해 노드당
  10), 확인 묶기 비율(§14) 0.6 미만, 쓰기 p99 50 ms 미만(`OPL_SCALE_P99_MS`, CI 200 ms), 관찰 노드의 뒤처짐 p99 500 ms
  미만(`FOLLOW_STALENESS_P99`, 리더가 쓰기를 마친 뒤 관찰 노드 하나하나가 적용하기까지, 쓰기 100개 × 관찰 노드), 노드 추가 2초
  미만, 투표 멤버 장애 뒤 모든 관찰 노드가 새 쓰기를 받기까지 5초 미만, 리더 장애 5초 미만.
- 투표 멤버 장애: 시험의 Follow 통로가 관찰 노드마다 붙은 투표 멤버를 적고, 관찰 노드가 가장 많이 붙은 리더 아닌 투표
  멤버를 끊은 뒤 곧바로 쓴다. 끊긴 노드는 그 쓰기를 받지 못하므로 모든 관찰 노드가 그것을 적용했다면 다시 붙은 것이다.
  memnet은 끊긴 노드의 다음 프레임(1초마다 `Progress`)에서 연결을 끊으므로 1초 안에 알아챈다. 실제 gRPC에서 연결이 끊기지
  않고 멈추면 3초(`FOLLOW_STALL`)에 알아채므로 3초 남짓이다(시험이 재지 않은 값).
- **작은 노드 수의 묶기 검사를 고쳤다.** 계획 47의 시험은 리더 RPC를 재는 동안 투표 멤버로 가는 `append_entries`에 20 ms
  지연을 넣었다. 확인은 한 번에 하나라 초당 많아야 50번인데 확인 요청은 초당 노드 수(갱신 n/2 × 2)라, 노드 50 미만에서는
  묶일 까닭이 없어 노드 30에서 비율 0.79(134 대 169)로 실패했다(계획 47의 바탕 `9b8de4b`에서도 같음). 지연을 2초/n(20 ms
  이상)으로 바꿨다: 확인은 초당 많아야 n/2번이라 확인 요청의 절반 이하로 묶이고, 투표 멤버 RPC는 많아야 하트비트 20 +
  확인 4 × n/2, 묶지 않은 최악(20 + 10 × (n/2 + 0.5))의 (20 + 2n) / (25 + 5n)이다(노드 20에서 0.48, 30에서 0.46, 100에서
  0.42, 노드 100부터 지연 20 ms라 300에서 0.14). 노드 100·300은 계획 47과 같은 지연이다. 기준(0.6)은 그대로다. 지연이
  하트비트 간격(200 ms)에 가까워지면 선거가 나므로(노드 8에서 지연 250 ms로 리스 갱신 14번 실패, 노드 추가 실패) 노드
  수는 20 이상이어야 한다(`MIN_NODES`, 전에는 투표 멤버보다 많기만 하면 됐다). 확인을 묶지 않게 고친 빌드(갱신마다
  `confirm_leader`)는 노드 30에서 0.79, 100에서 0.80으로 실패했다.
- 잰 값(개발 Mac, Apple Silicon 10코어, 시험 빌드, 두 번씩):

  | | 노드 30 | 노드 100 | 노드 300 |
  |---|---|---|---|
  | 리더 RPC 합(지연을 넣은 5초) | 93(노드당 3.1) | 254~255(2.5) | 352~355(1.2) |
  | 비투표 구성원(대기 learner 둘)에게 | 7 | 7 | 7 |
  | 투표 멤버에게(묶지 않은 최악, 비율) | 72(171, 0.42) | 198(523~525, 0.38) | 200~201(1,479~1,499, 0.13~0.14) |
  | 전달받은 리스 갱신 | 15 | 50 | 145~149 |
  | 쓰기 p99 | 1.7~1.8 ms | 4.2~4.4 ms | 11.7~12.6 ms |
  | 관찰 노드 뒤처짐 p50 / p99 | 0.45 / 0.8~0.85 ms | 1.2~1.4 / 2.3~2.4 ms | 4.1~4.3 / 6.8~7.1 ms |
  | 노드 추가 | 1 ms | 3 ms | 5~8 ms |
  | 투표 멤버 장애 뒤 모든 관찰 노드가 새 쓰기를 받기까지(붙어 있던 관찰 노드) | 0.2초(10) | 1.2초(24) | 1.2초(82) |
  | 리더 장애 뒤 새 리더 | 3.2초 | 3.3~3.6초 | 3.4~3.6초 |
  | 노드를 모두 띄우기 | 0.16초 | 0.3초 | 2.0초 |

  계획 47(나머지가 learner)의 같은 구간 리더 RPC 합은 노드 100에서 570, 노드 300에서 1,320(노드당 5.7·4.4)이었고 그
  3분의 2가 learner 하트비트였다. 쓰기 p99도 노드 300에서 31~33 ms였던 것이 12 ms 남짓이 되었다(리더가 쓰기마다 learner
  모두에 복제하고 한 프로세스의 learner 모두가 그것을 쓰던 일이 관찰 노드 몫의 적용만 남았다). 실패한 리스 갱신은 없었다.

**남은 것.**
- 죽어서 교체된 투표 멤버는 건강하지 않은 learner로 남는다(건강한 learner만 내린다). 막 된 리더가 목록 쓰기를 적용하기
  전이면 목록의 노드를 승격할 수 있다. `AddVoter`의 다시 보기와 멤버십 변경 사이에 목록에 오른 노드는 Raft를 끈
  관찰 노드인 채 투표 멤버가 될 수 있다(키퍼가 목록에서 빼면 다시 멤버가 된다 — 그동안 투표 멤버 하나가 응답하지
  않는다).
- 리스 갱신을 투표 멤버가 묶어 리더에 전달하기, 처음부터 관찰 노드로 가입하기, 대기 learner 수를 설정으로 두기.

## 16. 구현(계획 49)

다섯 단계를 커밋 다섯으로 했다(1 `OrbNode`와 TypeConfig, 2 링크 고르기와 넘어가기, 3 노드 설정·인증서·합류, 4 링크
맞추기와 `multi-link`, 5 API·웹·실제 프로세스 시험·문서). 설계 §7과 다르거나 설계가 정하지 않아 정한 것을 적는다.

**노드 타입(`OrbNode`).**
- `OrbNode { addrs: Vec<String> }`(각 항목 `host:port`, 앞이 우선)이고 serde는 손으로 썼다: 늘 `addr`(= `addrs[0]`)를
  쓰고 링크가 둘 이상일 때만 `addrs`를 더 쓴다. 링크 하나면 `BasicNode`와 바이트까지 같아 이미 쓴 로그·스냅샷과 옛
  노드가 그대로다. openraft 0.9.25의 `BasicNode`는 `deny_unknown_fields`가 없어 옛 노드는 여러 링크 항목을 첫 링크
  하나로 읽는다. 읽을 때는 `addrs`가 있고 비지 않으면 그것, 아니면 `[addr]`(빈 `addr`이고 `addrs`도 없으면 링크 없는
  `default()`). 계획 48 형식으로 쓴 멤버십 로그 항목·`StoredMembership`·스냅샷 메타의 고정 JSON을 읽는 시험이 있다.
- `OrbNode::addr()`(첫 링크)가 이 계획 전의 "그 노드의 주소"다. 주소 문자열을 받는 기존 API(`Members`의 값,
  `NotLeader.leader_addr`, `Forwarder`, `FollowClient`, `Peers`의 `addr: &str`, `ClusterStatus.observers`의 값)는 바꾸지
  않고 첫 링크를 노드의 열쇠로 쓴다. 링크 목록은 `Members.links`와 링크 책이 따로 싣는다.
- 설계는 "`multi-link` 기능으로 가린다"였다. 노드 타입 자체는 가리지 않고(링크 하나면 형식이 같으므로) 여러 링크를
  멤버십에 넣는 일(링크 맞추기, 여러 링크로 합류)만 가린다.

**링크 목록의 출처.**
- Raft 구성원은 멤버십의 `OrbNode.addrs`, 관찰 노드는 새 `ClusterStatus.observer_links: BTreeMap<Uuid, Vec<String>>`
  (링크가 둘 이상인 관찰 노드만, 비면 쓰지 않음, 옛 노드는 무시; OpenAPI에는 없음)다. 노드가 원하는 링크는 새
  `NodeStatus.cluster_addresses`(`host:port`, 등록 때 노드가 링크 하나여도 씀)다. `NodeSpec`은 `deny_unknown_fields`라
  건드리지 않았다. 옛 리더가 Node status를 다시 쓰며 `cluster_addresses`를 떨어뜨리면 `refresh_features`가 되살린다.
  등록·`refresh_features`는 호스트가 지금 노드 인증서의 이름에 있는 링크만 적는다(`announced_links`; 다시 서명이 실패해
  이름이 빠진 링크로는 다른 노드가 TLS로 닿지 못하므로 경고만, 리뷰; 경고는 빠진 링크 집합이 바뀔 때만 — `refresh_features`가
  60초마다 부르므로, 2차 리뷰). 다시 합류하면 API가 그 노드의 기록을 새 링크로
  둔다(옛 값이 남으면 링크 맞추기가 멤버십을 옛 링크로 되돌렸다, 리뷰).
- `initialize`(새 클러스터)와 `AddLearner`(합류)는 받은 링크를 그대로 쓴다(`LeaderRequest::AddLearner.addrs`, 링크
  하나면 생략해 옛 형식 그대로; 옛 리더는 모르는 필드를 무시하고 첫 링크만 쓴다). 첫 링크가 죽어 있으면 learner가
  로그를 받지 못해 등록·링크 맞추기까지 갈 수 없으므로 합류에서 바로 실어야 한다.
- `Cluster::link_book()`(첫 링크 → 링크 목록, 링크 하나인 노드는 넣지 않음)은 적용된 멤버십과 관찰 노드 목록으로
  `track_observers`가 고친다(`FeedBatch.membership`이 있을 때, Reset·Lagged 뒤 다시 읽을 때, 목록이 바뀔 때). opld는
  `open_node` 뒤 `peers.set_link_book(cluster.link_book())`을 부른다(관찰 노드·위트니스 포함). Raft의 `GrpcConnection`은
  책을 붙이면 책의 목록을, 붙이기 전에는 `new_client`가 받은 `OrbNode.addrs`를 쓴다(책을 붙이기 전에도 여러 링크).
  처음에는 늘 `OrbNode.addrs`를 썼는데, 멤버십이 바뀌는 사이 책(적용된 멤버십)과 목록이 달라 같은 노드의 선택 상태가
  호출마다 처음부터 다시 세어졌다(리뷰). 그래서 로그의 멤버십에만 있는 새 링크는 적용된 뒤에 쓰인다.

**넘어가기 규칙(`opl-cluster/src/links.rs`, Peers와 memnet이 함께 씀).**
- 첫 링크를 열쇠로 노드마다 `{ links, active, last_ok, timeouts, switched_at, trial }`이다(`LinkSelector`). 연결
  실패(`Connect`)면 곧바로, 제한 시간 초과(`Timeout`)면 지금 링크에서 연속 두 번이거나 그 링크의 마지막 성공(또는 그
  링크로 넘어온 때)에서 400 ms(`LINK_DEAD_AFTER`)가 지났을 때(먼저인 것) 다음 링크로 넘어가고, 모두 실패하면 돌아가며
  시도한다. 처음에는 "마지막 성공에서 1초"였는데, 하트비트 200 ms를 더하면 조용히 죽은 링크에서 1.2~1.4초 동안 따르는
  노드에 하트비트가 없어 선거 제한 시간(1~2초)에 걸렸다(pre-vote가 없어 리더가 물러남, 리뷰). **넘어가기 상한
  (`LINK_DEAD_AFTER` + 하트비트 간격)은 `election_timeout_min − heartbeat_interval`보다 짧아야 한다**(400 + 200 = 600 ms <
  1000 − 200 = 800 ms). 하트비트 하나가 늦은 살아 있는 링크(초과 한 번, 400 ms 안)는 버리지 않는다.
- 우선 링크가 아닌 동안 30초(`LINK_RETRY`)마다 다음 호출 **하나**를 우선 링크로 보내고(시험 호출) 성공하면 돌아오며
  실패하면 머물러 다시 잰다. 시험 호출은 control 채널의 짧은 호출(`Counts::All`: Raft RPC·ping·리스 갱신·Follow 열기)만
  보낸다(`LinkSelector::current`; 처음에는 콘솔·마이그레이션·쓰기도 시험 호출이 될 수 있어 죽은 우선 링크에서 오래
  걸렸다, 리뷰). 우선 링크로 돌아가는 것은 시험 호출의 성공이거나 지금 링크로 넘어온(마지막 시험을 보낸) 뒤 보낸 호출의
  성공일 때만이다(`Pick.sent`; 넘어가기 전에 보낸 호출의 늦은 성공이 죽은 링크로 되돌리던 것, 리뷰). 따로 도는 링크
  ping은 두지 않았다(memnet과 같은 코드로 시험하려고).
- **연결 실패는 같은 호출 안에서 다시 보낸다(리뷰).** Peers는 요청이 나가지 않았음이 확실한 호출만 다시 고른 다른
  링크로 한 번 더 보낸다(`not_sent`): 상태의 오류 원인 사슬에 `tonic::ConnectError`(TCP 연결·TLS 핸드셰이크 실패, 연결
  제한 시간 포함)가 있을 때, 링크로 채널을 만들지 못했을 때, 아래의 다른 노드 거절. 시험 호출이든 아니든 같다(시험
  호출이 실패해도 그 호출은 지금 링크로 성공한다). 처음에는 모든 `Unavailable`을 다시 보냈는데, tonic 0.14.6은 이미
  보낸 진행 중 호출도 keepalive ping 만료(`hyper::Error::is_timeout`)면 `Unavailable`로 끝내므로(`status.rs`
  `from_hyper_error`) 받은 쓰기가 두 번 갈 수 있었다(2차 리뷰). `tonic::transport::Error`는 채널의 모든 오류(보낸 뒤의 h2·IO
  오류 포함)를 감싸 쓰지 않았다. 링크 고르기에는 지금처럼 모든 `Unavailable`을 연결 실패로 센다(다음 호출은 다음 링크).
  시험: 쓰기를 받고 답을 버리며 연결은 열어 두는 중계 → keepalive 만료 뒤 오류, 둘째 링크로 연결 없음
  (`a_sent_write_that_loses_its_connection_is_not_sent_again`, 약 30초). 제한 시간 초과는 상대가 받았을 수 있어 다시
  보내지 않는다. 요청 스트림을 한 번만 보낼 수 있는 콘솔은 다시 보내지 않는다. 서버가 내려가는 중이라 답한
  `Unavailable`("shutting down")은 연결 실패로 세지만 다시 보내지 않는다. memnet은 다시 보내지 않는다(끊긴 링크는 연결
  실패로 넘기기만).
- 넘어가면 `warn`, 돌아오면 `info` 로그다. 헬스·지표에는 내지 않는다(계획 52의 관리망 검사와 함께 나중).
- 시간은 `tokio::time::Instant`(멈춘 시계 단위 시험). `ClusterConfig.link_retry`·`Peers::with_link_retry`는 시험 전용이다
  (memnet 시험 2.5초).

**Peers(opl-peer)가 실패를 보는 곳.**
- 채널 캐시는 `(Lane, 링크)`마다다(링크를 바꾸면 다른 TCP 연결). 호출마다 링크를 고르고 결과를 공통 도우미(`Attempt`)로
  알린다: `Unavailable`은 연결 실패, `DeadlineExceeded`·`Cancelled`(tonic 자기 제한 시간)는 초과, `Unknown`·`Internal`은
  세지 않음, 결과 없이 버려진 호출(부른 쪽의 `timeout`)은 가드가 초과로 센다. openraft가 다른 까닭(복제 스트림 다시
  만들기)으로 버린 호출도 초과로 세지만 "연속 두 번 또는 400 ms" 기준이 대개 가린다.
- **대상 노드 확인(리뷰).** 노드 A의 링크에 B의 주소가 들어가면(오타, 다시 쓴 IP) 리더가 A에게 보낼 append·vote를 B가
  받아 이중으로 셀 수 있었다. 대상 id를 아는 호출(Raft RPC의 `GrpcConnection`, 리더 전달·리스 갱신의 `Forwarder`, Follow)은
  gRPC 메타데이터 `orbivisor-target: <노드 uuid>`를 싣고, 서버는 있으면 자기 id와 비교해 다르면 처리하지 않고
  `permission_denied`로 답하며 같은 키에 자기 id를 싣는다(옛 노드는 싣지 않으므로 받는다). 클라이언트는 그 답을 연결
  실패로 세어 다음 링크로 다시 보낸다. ping(생존 확인 모니터)과 bulk 호출 가운데 부르는 곳이 id를 아는 `uefi_vars`·
  `read_audit`·`read_metrics`도 싣는다(2차 리뷰: 싣지 않은 ping이 다른 노드의 답으로 잘못된 링크를 성공으로 셌다). 싣지
  않는 것: opld의 `AddVoter` 요청(대상 id를 모름), 마이그레이션 준비·기다리기·취소와 콘솔(부르는 곳이 주소만 넘김).
  설정 쪽에서도 막는다: 합류는 다른 구성원·관찰 노드의 링크와 겹치는 링크를 400으로, 링크 맞추기는 그런 노드를 건너뛰고
  한 번 경고한다(`link_clashes`).
- 초과까지 세는 것: Raft RPC(openraft `hard_ttl`, `append_entries`는 하트비트 간격 200 ms), ping, `RenewLease`·`RenewVip`
  (gRPC 제한 시간을 90초에서 3초 `LEASE_RPC_TIMEOUT`로 — HA 리스 갱신의 바깥 `timeout`은 `fence_after`가 20초 이상이면
  4초라 Peers가 초과를 보고, 더 짧은 `fence_after`면 바깥이 먼저 버려 가드가 초과로 센다), Follow
  열기(응답 헤더까지 3초 `FOLLOW_OPEN_TIMEOUT`). 쓰기·`AddVoter` 같은 긴 리더 요청은 연결 실패만 센다(따라잡기 60초를
  실패로 보지 않게).
- ping과 리더 요청은 Peers 안에서도 제한 시간(`within`)으로 끝낸다: 연결은 받고 답하지 않는 리스너에서 TLS
  핸드셰이크가 tonic 제한 시간 밖에서 멈췄다.
- **연결 제한 시간 3초 → 2초(단계 5).** 실제 프로세스 시험에서 우선 링크가 SYN에 답하지 않는 macOS(127.0.0.2)의 막
  합류한 노드가 등록 쓰기를 전달하지 못해 합류가 실패했다. 쓰기 전달은 연결 실패만 세는데, 전달자의 시도 제한 시간
  (`FORWARD_ATTEMPT_TIMEOUT` 3초)과 연결 제한 시간(3초)이 같아 연결 실패가 나기 전에 시도가 버려졌고(버려진 긴 요청은
  세지 않음) 같은 링크로 다시 보내기만 되풀이했다. 합류 직후의 노드는 아직 리스 갱신·ping을 보내지 않아 다른 호출로도
  넘어가지 못했다. 연결 제한 시간을 2초로 줄여 답 없는 링크가 시도 안에서 연결 실패(`Unavailable`)로 끝나게 했고,
  리뷰 뒤로는 그 쓰기가 같은 시도 안에서 둘째 링크로 다시 가 성공한다(`opl-peer/tests/grpc.rs`의
  `a_forwarded_write_fails_over_from_a_link_that_never_connects`, 연결 제한 시간이 3초면 실패). 연결은 받지만
  TLS 핸드셰이크에서 멈추는 링크로 보낸 쓰기는 여전히 넘어가지 못한다(실제 끊긴 망은 연결을 받지 않고, 리스 갱신·ping이
  같은 노드의 링크를 넘기므로 남겨 둠). 그런 링크는 연결 실패만 세는 호출(쓰기·`AddVoter`·bulk 호출)로는 실패로
  세어지지 않는다(제한 시간 초과를 세지 않음) — 초과까지 세는 control 채널의 짧은 호출(Raft·ping·리스 갱신·Follow
  열기)이 넘긴다.
- 이미 기다리던 긴 요청은 죽은 연결에서 keepalive(10초 + 20초)까지 걸릴 수 있다(다음 호출은 새 링크).

**memnet.** 노드를 링크와 함께 등록하고(멤버십의 `OrbNode`로 링크 → 노드), `MemRouter::cut_link(from, link)`·`sent(link)`
(그 링크로 간 RPC 수), `heal(id)`이 끊긴 링크도 푼다. `MemForwarder`·`MemFollow`는 보내는 노드의 링크 책, `MemConnection`
(Raft)은 링크 책이 아니라 `new_client`가 받은 멤버십의 `OrbNode.addrs`에서 보내는 노드의 `LinkSelector`로 링크를 고르고
끊긴 링크면 연결 실패다(Peers의 `GrpcConnection`과 달리 책을 읽지 않음). memnet은 같은 호출 안에서 다시 보내지 않는다.

**링크 맞추기와 `multi-link`.**
- 노드 기능 `multi-link`(`FEATURES` 끝, `opl_cluster::MULTI_LINK`). 리더의 `VoterKeeper` 틱에서 joint가 아니고 `plan`이
  바꿀 것이 없고 살아 있는 투표 멤버가 과반일 때, 모든 Node(위트니스·관찰 노드 포함)가 Node 기록과 `multi-link`를 가질
  때만, NodeId 순으로 한 틱에 한 노드(`link_fix`): Raft 구성원의 `addrs`가 그 노드의 `status.cluster_addresses`(비지 않을
  때)와 다르면 `change_membership(SetNodes { id: OrbNode }, false)`(`change_timeout` 안, 앞선 변경이 커밋 중이면 다음 틱),
  관찰 노드면 `observer_links`를 `UpdateStatusIf`로 고친다(첫 링크가 바뀌면 `observers` 값도). 첫 링크를 바꾸는 것도
  허용한다 — 같은 노드 id이고 그 노드가 스스로 적은 값이라 openraft가 경고하는 "다른 노드를 가리키는 주소 바꾸기"가
  아니다. 그래서 업그레이드 뒤 설정의 링크가 멤버십 주소와 다른 기존 노드(예: `address`를 바꾸고 다시 시작한 노드)는 첫
  링크도 바뀐다(README의 업그레이드 주의).
- openraft 0.9.25의 `SetNodes`는 투표 멤버 집합이 그대로라 joint 없이 항목 하나로 커밋한다(`membership.rs`의
  `Membership::change` — `SetNodes`는 노드 값만 바꾸고 구성은 그대로 둔다). 그래서 `same_config`·`finish_joint`와 부딪히지 않는다
  (joint 구성이 생기지 않음). 멤버십이 바뀌면 리더가 복제 스트림을 다시 만들며 `new_client`를 새 노드 값으로 부른다.
- 좁은 경합(2차 리뷰): openraft의 `SetNodes`는 멤버십에 없는 노드를 더한다. 키퍼가 `same_config`를 본 뒤
  `change_membership`이 커밋되기 전에 노드 제거(`Remove`, API의 노드 삭제·`leave`)가 먼저 커밋되면 막 지운 노드가 learner로
  되살아날 수 있다. 키퍼는 `Done` 뒤 그 노드가 투표 멤버였는데 아니게 됐거나 Node 기록이 없으면 warn("updated the cluster
  links of a node that was removed meanwhile")만 한다. 되살아난 learner를 저절로 지우는 고아 정리는 없어 그 경고를 보고
  손으로 다시 뺀다(Node 기록 삭제가 `Remove` 뒤라 그 사이에 본 경우는 경고도 놓칠 수 있다).
- 키퍼의 `ToObserver`는 `addrs`를 `observer_links`에, learner로 되돌리기(`return_observer`)는 `observer_links`(없으면
  `observers` 값 하나)로 `OrbNode`를 만든다. `LeaderRequest::Remove`는 `observer_links`에서도 뺀다. `observer_nodes(status)`가
  관찰 노드 캐시·링크 책·`Members.links`에 관찰 노드의 실제 링크를 준다.

**설정·인증서·합류.**
- opld 최상위 `cluster_addresses`(호스트만, 포트·중복·잘못된 모양 거절; 호스트 규칙은 API 합류와 같은
  `opl_types::host::valid_host`이고 중복은 값으로 — IP는 같은 주소, 이름은 대소문자 없이, 리뷰. `address`도 같은 규칙이라
  끝의 점·멀티캐스트 같은 값은 이제 거절되어 그런 `address`로 돌던 노드는 업그레이드 뒤 시작하지 않는다 — README
  업그레이드 주의에 올리기 전에 확인하라고 적었다, 2차 리뷰), `Config::cluster_links()`(없거나 비면
  `[address:포트]`), `cluster_address()`는 첫 링크, `cert_names()`는 `address`·모든 링크 호스트·localhost 셋.
  `cluster_listen`이 `0.0.0.0`·`::`가 아닌데 링크가 둘 이상이면 시작 로그에 경고만(시험이 일부러 그렇게 쓴다).
- 합류: opld `join.rs`는 링크가 둘 이상일 때만 `JoinRequest.cluster_addresses`를 보낸다(옛 API 노드의
  `deny_unknown_fields`를 건드리지 않게). API는 첫 것이 `cluster_address`와 같고 겹치지 않으며 모두 SAN에 넣을 수 있는
  호스트인지 보고(아니면 400), 둘 이상이면 `multi-link`를 요구한다(409 `cluster_upgrading`; 다시 합류하는 노드 자신의 옛
  Node 기록은 세지 않음). 겹침은 값으로 보고(`same_link`), 다른 노드의 링크와 겹치면 400이다(이미 멤버·이름 검사 뒤).
  다른 노드의 Node 기록이 원하는(`status.cluster_addresses`, 링크 맞추기가 아직 멤버십에 넣지 않은) 링크와 겹쳐도 400이다(2차
  리뷰). 다시 합류 때 주소 비교는 링크 목록 전체다. 옛 API 노드는 `cluster_addresses`를 모르는 필드로 거절하므로, opld는 그
  거절을 "모든 노드를 먼저 올리라"는 메시지로 바꿔 보인다.
- 다시 서명: opld가 Restart에서 저장소를 연 뒤·노드 간 서버를 띄우기 전에(위트니스 포함) 노드 인증서에 빠진 이름이
  있으면 저장소의 클러스터 CA로 **같은 키**에 새로 서명해(`pki::csr_for_key`) `pki` 디렉터리에 저장하고 서버·마이그레이션
  TLS·API 인증서에 쓴다. CA가 아직 없거나 실패하면 경고만. 클라이언트 신원은 옛 인증서 그대로다(이름이 아니라 CA와 CN의
  노드 id로 확인됨).
- 마이그레이션의 QEMU 스트림 호스트는 대상 노드의 지금 링크(`Peers::active_link(addr)`)의 호스트다. 받을 준비 요청
  (`migrate_incoming`)이 성공한 뒤에 읽는다(그 전에는 아직 죽은 줄 모르는 첫 링크일 수 있었다, 리뷰; 그 요청은 연결
  실패면 다른 링크로 다시 가므로 성공한 뒤의 지금 링크는 닿은 링크다).

**보이기(단계 5).** API `MemberView.cluster_addresses`(모든 링크, 첫 것 = `cluster_address`; `Members.links`에 없으면
`[cluster_address]`), `orbivisorctl cluster status` 표는 그대로(`-o json`에 보임), 웹 클러스터 화면은 클러스터 주소 아래에
둘째 링크부터 작게.

**실제 프로세스 시험(`opld/tests/cluster.rs`).**
- 세 노드가 `cluster_addresses = ["127.0.0.2", "127.0.0.1"]`, `cluster_listen = "127.0.0.1:{port}"`로 우선 링크가 죽은 채
  init·합류·승격·쓰기 전달을 하고, 모든 멤버의 `cluster_addresses`가 두 링크이며 노드 인증서에 127.0.0.2가 있고, 리더 kill -9
  뒤 남은 둘이 새 리더를 뽑아 쓰기를 잇는다. macOS의 lo0에는 127.0.0.1만 있어 127.0.0.2로의 연결은 응답 없이 시간
  초과이고, Linux는 127/8 전체가 로컬이라 듣지 않는 포트는 곧바로 거절이다. 두 OS 모두 sudo 없이 "죽은 우선 링크"를
  본다(시험 약 25초, macOS). 합류 때 리더의 첫 `append_entries`들은 200 ms에 끊기다 1초 안팎에 둘째 링크로 넘어갔다(리뷰 뒤의 규칙으로는 연속
  두 번째 초과, 약 400 ms).
- 링크 하나로 만든 노드에 `cluster_addresses`를 더해 다시 시작하면 인증서가 같은 키로 다시 서명되고(127.0.0.2 포함)
  리더(그 노드 자신)가 멤버십을 두 링크로 고친다.
- 링크를 도중에 끊고 되살리는 것은 memnet(`opl-cluster/tests/links.rs`: 모든 노드 사이 첫 링크를 끊어도 리더·쓰기·리스
  갱신·관찰 노드 Follow가 계속, 리더를 죽이면 둘째 링크로 새 리더, 풀면 재시도 간격 뒤 첫 링크로, 링크 맞추기의 여러
  경우)과 gRPC(`opl-peer/tests/grpc.rs`: 닫힌 포트·받고 답하지 않는 리스너·SYN에 답 없는 주소) 시험이 본다.

**남은 것.**
- 링크 상태를 헬스·지표·웹에 보이기(계획 52의 관리망 검사와 함께), 링크별 백그라운드 ping, 대량 전송(콘솔·마이그레이션
  준비)용 링크를 따로 고르기.
- TLS 핸드셰이크에서 멈추는 링크로 보낸 긴 리더 요청은 다른 호출이 링크를 넘길 때까지 넘어가지 못한다.

## 17. 구현(계획 50)

일곱 단계를 커밋 일곱으로 했다(1 `ha` 구조와 호환, 2 배치 함수와 HA 그룹·배치 규칙 API, 3 규칙 강제 — 생성·시작·마이그레이션·
비우기, 4 HA 복구·되돌아가기·`rule_violations`, 5 재시작·옮기기 횟수, 6 노드 정상 종료 정책, 7 웹·문서). 설계 §8과 다르거나
설계가 정하지 않아 정한 것을 적는다.

**`ha` 모양과 호환(단계 1).**
- `VmSpec`은 `deny_unknown_fields`이고 저장소는 읽을 때 타입으로 풀므로(목록 하나가 실패하면 전체가 오류) 새 모양을 옛 노드가
  읽지 못하면 그 VM이 든 목록이 모두 깨진다. 그래서 `VmSpec.ha: Option<VmHa>`의 serde를 손으로 썼다(`ha_compat`): 읽기는
  `false` → 없음, `true` → 기본 정책, 객체 → 그 값(객체 안은 `deny_unknown_fields`, 빠진 값은 기본 — `{}`도 기본 정책). 쓰기는
  없음 → `false`, 기본 정책 → `true`, 아니면 객체. 이 계획 전의 VM과 단순 HA VM은 바이트까지 옛 모양이다.
- `VmHa { state: HaState(started|stopped|ignored), group, max_restart, max_relocate }`(횟수 0~10, `MAX_HA_COUNT`, 기본 1·1,
  `ValidationError::HaCount`). OpenAPI는 `HaSetting = oneOf [boolean, VmHa]`이고 웹은 둘 다 읽는다(`haEnabled`·`haPolicy`).
- `VmSpec::ha_active()`(정책이 있고 `ignored`가 아님)가 옛 `spec.ha`의 자리다: 펜싱, HA 가능 노드 조건, 복구 대상, 백업·
  스냅샷·마이그레이션 등의 HA 분기. 없음·`ignored` → `started`·`stopped`는 HA 켜기와 같아 VM이 멈춰 있을 때만(409 `vm_running`).
- 기본과 다른 객체는 `require_feature("ha-placement")`(409 `cluster_upgrading`). 기능 `ha-placement`(`FEATURES` 끝,
  `opl_types::node::HA_PLACEMENT`)는 새 종류 쓰기, 기본이 아닌 `ha`, `shutdown_policy`, reconciler의 옮기기 요청이 본다.
- `VmStatus.rule_violations`·`VmStatus.ha: Option<VmHaStatus { relocate_from, relocations, relocated_from, error }>`(비면 쓰지 않음)는
  `deny_unknown_fields`가 없어 가리지 않는다. reconciler `write_status`는 `rule_violations`를 늘 저장소 값에서, `ha`는 이
  작업자가 바꾸지 않았으면 저장소 값에서 옮겨 쓰고(`keep_ha`) `write_power_off`도 지킨다.

**`state`의 뜻.** 켜고 끄기는 지금처럼 `spec.power` 하나다(둘을 두면 어긋난다). `started` = 지금의 HA, `stopped` = 노드 장애
때 옮기기만 하고 켜지 않음(복구 spec 쓰기에서 `power = Stopped`, 꺼진 `stopped` VM도 옮김 — Proxmox `stopped`와 같은 결과),
`ignored` = HA를 잠시 끔(펜싱·복구·횟수·HA 가능 노드 조건 모두 없음, 설정은 남김).

**HA 그룹·배치 규칙(단계 2).**
- 새 종류 `Kind::HaGroup`(`ha-group`, `HaGroupSpec { nodes: [{ node, priority 0~1000 }], restricted, nofailback }`)과
  `Kind::PlacementRule`(`placement-rule`, `PlacementRuleSpec { kind, vms | vm_tag, nodes | node_tag, weight 1~100(soft만, 없으면
  50), enabled(기본 true), ha_may_violate(hard만) }`). 둘 다 `deny_unknown_fields`, 이름은 리소스 이름, 쓰기는 관리자만(운영자
  목록에 넣지 않음), 읽기는 모두. API `/ha-groups`·`/placement-rules`(목록·조회·`POST`·통째로 바꾸는 `PUT`·`DELETE`), watch
  종류, 원격 중계 허용 목록에 더했다. 만들기·고치기는 `require_feature`, 삭제는 기능을 보지 않는다.
- **노드 태그는 노드 레이블이다(설계와 다름).** 설계는 "노드에 `tags`"였으나 `NodeSpec`이 `deny_unknown_fields`라 필드를 더하면
  옛 노드가 Node 기록을 풀지 못한다. 노드 레이블(`Resource.labels`, `[node] zone`이 `zone`이 됨)을 태그로 쓰고 `vm_tag`·`node_tag`는
  `"key=value"` 하나(`parse_tag`, 그 레이블 값이 같은 것)다. CLI `node label`, 웹 노드 화면의 레이블 편집을 더했다.
- 검증(400): VM은 `vms`/`vm_tag` 중 하나(이름 목록이면 VM끼리 규칙은 둘 이상), VM끼리 규칙은 노드를 주지 않음, 노드 규칙은
  `nodes`/`node_tag` 중 하나, 규칙의 `nodes`·그룹 노드는 있는 노드여야 함. 규칙의 VM 이름은 아직 없어도 된다. VM의 `ha.group`은
  있는 그룹이어야 하고(400), VM이 쓰는 그룹 삭제는 409 `group_in_use`.
- 공용 함수(`opl-api/src/placement.rs`): `Assign { place, unplace }`(자리 덮어쓰기), `Candidate { node, priority, penalty,
  free_mib }`, `candidates(&Vm, &Inventory, &Assign)`(노드 조건·hard 통과만, 나은 것부터), `best`, `compare`(그룹 우선순위 →
  벌점 → 미할당 메모리 → id), `score`, `hard_violations`(restricted 그룹 밖이면 `ha-group:<이름>`), `soft_violations`,
  `affinity_partners`, `conflicts`. "자리 잡은 VM" = `spec.power == Running`이고 `spec.node`가 있는 VM, 마이그레이션 중이면
  원본과 대상 둘 다. HA 그룹 우선순위는 `ha_active()` VM에만 쓴다. `Inventory`가 규칙·그룹도 읽는다.
- 모순(422 `rule_conflict`, `details.rules`): 켜진 hard 규칙만, 태그는 지금 레이블로 풀어 (a) 같은 두 VM이 `affinity` 묶음
  (추이적)과 `anti-affinity`에 함께, (b) 한 `affinity` 묶음의 허용 노드가 빔. 새(고친) 규칙이 든 모순만 거절한다(이미 있던
  모순으로 다른 규칙 저장을 막지 않음). 그 뒤 레이블이 바뀌어 생긴 모순은 놓을 곳 없음(409)·위반으로 보인다.

**강제(단계 3).**
- `scheduler::schedule(&Vm, &Inventory) -> Result<Uuid, ScheduleError::{NoNode, PlacementRule(규칙)}>`: 노드 조건부터 맞는
  노드가 없으면 `no_node`, 조건은 맞는데 hard 규칙 때문이면 409 `placement_rule`(`details.rules`). `prepare`·`schedule`은
  `VmSpec` 대신 VM(이름·레이블 필요)을 받는다.
- 노드를 준 켜진 VM의 생성·수정은 그 노드가 hard 규칙을 어기면 409(이미 그 노드에 자리 잡은 VM의 다른 고치기는 막지 않음),
  규칙을 어기는 노드에 붙은 꺼진 VM의 시작은 다른 노드를 고른다(유지보수 때와 같이, 없으면 409).
- 마이그레이션 `start_migration(…, RuleCheck::{Enforce, Force, Group(묶음)})`: 노드 조건·메모리 뒤 hard 규칙, 자리 잡은 hard
  affinity 상대가 있으면 혼자 옮기지 못함. `MigrateRequest.force`는 배치 규칙 쓰기 권한(관리자)만이고 감사 detail
  `force rules=<이름들>`. 비우기는 한 번에 첫 VM 하나를 그 노드의 affinity 묶음(추이적)과 함께, 묶음 전체를 받을 노드(메모리 합,
  묶음을 둔 `Assign`으로 hard 검사)로 옮기고, 받을 곳이 없으면 `placement rule …`로 남긴다. 규칙에 맞는 노드가 메모리 때문에만
  막혔으면(앞 구성원이 옮겨 간 노드에 자리가 없는 짝) 규칙이 아니라 `no node with capacity`다(2차 리뷰). 디스크 막힘(저장소
  마이그레이션 기능·스냅샷)은 묶음의 모든 구성원에서 보고 하나라도 막히면 묶음 전체를 남기며(막은 구성원은 그 이유, 나머지는
  `affinity partner X: <이유>`, 2차 리뷰), 이미 옮기지 못한 VM이 묶음에 있으면 나머지도 `affinity partner X could not move`로
  옮기지 않는다(1차 리뷰). 앞 구성원이 옮겨진 뒤 마이그레이션이 실패한 구성원은 남고(앞 구성원을 되돌리지 않음) 갈라짐은
  `rule_violations`에 보인다(two_nodes 시험). 배치 규칙 만들기·고치기는 VM 만들기와 같은 `vm_lock`
  안에서 검사하고 쓴다 — `vm_lock`은 API 노드마다라 서로 다른 API 노드로 보낸 모순 규칙의 동시 만들기는 여전히 둘 다 저장될 수
  있다(그 뒤 놓을 곳 없음·위반으로 보임). 스냅샷 되돌리기는 지금 VM의 `ha`로 검증하고(Task가 지금 것을 지킴), 복제·복원·가져오기가
  만드는 VM의 `ha.group`이 지워진 그룹이면 그룹만 빼고 경고한다. 그룹 확인과 VM 쓰기 사이에 그룹이 지워지는 좁은 경합은 남는다
  (그 VM은 그룹 없는 VM처럼 다뤄져 해롭지 않다). 받은 백업의 `vm_spec`에 기본이 아닌 `ha`가 있고 `ha-placement`를 모르는
  노드가 있으면 `true`(기본 정책)로 낮춰 저장한다. `orbivisorctl vm set --ha`는 `ignored` 정책을 `started`로 다시 켠다.

**HA 복구·되돌아가기·`rule_violations`(단계 4).**
- 복구 대상은 `placement::candidates` 순서(그룹 우선순위 → 벌점 → 메모리) + 리스가 살아 있는 노드, 죽은 노드·받던 마이그레이션
  노드 제외. 죽은 노드의 VM은 자리로 세지 않고, 앞에서 옮긴 VM은 `Assign`으로 다음 VM의 규칙·메모리에 센다(같은 죽은 노드의
  affinity 상대는 첫 VM을 따라감). hard 규칙에 막히면 켜지 않고 `last_error`·`ha.error`("HA cannot place this VM: placement
  rule …")를 남기고 틱마다 다시 본다. 막는 규칙이 모두 `ha_may_violate`면 위반이 가장 적은 노드에 켜고 warn. 옮기면 `ha.error`를
  지운다.
- 되돌아가기(`fail_back`): 복구할 노드와 결과 모르는 쓰기가 없을 때, 더 높은 그룹 우선순위·hard 통과·`failback_after`(기본
  30초, `HaManagerConfig.failback_after`) 넘게 리스가 산 노드로 `start_migration(RuleCheck::Enforce)`. 클러스터에 Task 하나,
  끝나기를 기다리고, 실패한 VM은 이 리더가 다시 하지 않는다. `start_migration`이 거절한 VM은 60초(`FAILBACK_RETRY`) 뒤에 다시
  보고, 옮기기 창 안에 `relocations` 기록이 있는 VM(재시작 횟수를 넘겨 옮겨 온 VM)은 되돌리지 않는다(1차 리뷰). 창이 지나
  `relocated_from`(옮기기가 기록과 함께 쓰는, 마지막으로 옮겨 떠난 노드 — Proxmox `failed_nodes`와 비슷)으로 되돌아가면
  마이그레이션 Task를 만든 뒤 옮기기 한 번으로 센다(`relocations`에 지금 시각, `relocated_from` 지움 — 되돌아간 노드에서 다시
  크래시하면 기본값에서 `error`, 2차 리뷰). 다른 노드로의 되돌아가기는 세지 않는다. 리스를 처음 본 때는 리더의 메모리에만 있어 리더가 바뀌면 다시 센다.
- `rule_violations`: 리더의 `HaManager` 틱마다 자리 잡은 VM의 hard·soft 위반을 계산해 바뀐 VM만 `UpdateStatusIf`(충돌은 다음
  틱). 꺼진 VM은 비운다.
- 실제 프로세스 시험(`ha-it`): 그룹 `{A:1, B:2}`의 HA VM이 B에 놓이고 B를 kill -9 하면 A로 복구, B를 다시 띄우면 되돌아감
  (마이그레이션 포트 49630-49649, HVF면 되돌아가기 부분은 건너뜀).

**재시작·옮기기 횟수(단계 5).**
- reconciler: `started` HA VM의 크래시는 `restart` 정책과 관계없이 다시 켠다(게스트가 스스로 끈 것은 끔). 기존 창(600초)
  안의 크래시가 `max_restart`를 넘으면 `status.ha.relocate_from = 이 노드`·phase Failed를 쓰고, 요청이 저장소에 있는 동안 켜지
  않고 리더의 status를 덮지 않는다. 꺼지면 요청을 지우고 다시 켤 때 `ha.error`도 지운다. 모든 노드가 `ha-placement`일 때만
  (`ha_placement_everywhere`, 크래시 때 노드 목록을 읽음 — 옛 리더는 `relocate_from`을 모름), 아니면 지금의 백오프·포기.
  `FailureTracker::record_up_to`.
- 리더: 원본 리스가 살아 있을 때만(죽은 노드면 노드 장애 복구가 맡음) 그 노드를 뺀 `candidates`로 옮기고(펜싱 없이 status 뒤
  spec, `relocate_from`은 받은 노드가 켤 때 지움) `relocations`(유닉스 초, 600초 창)에 더한다. 창 안에서 이미 `max_relocate`번이면
  spec `power = Stopped`를 먼저 쓴 뒤 status Failed·`ha.error = "gave up after N relocations"`(spec을 먼저: 요청을 먼저 지우면
  그 노드가 다시 켤 수 있음). 노드 장애 복구는 요청을 지우고 세지 않는다. 사용자 재시작은 `relocations`를 지우지 않는다(창이
  지나면 사라짐).
- 옮기기 창(`RELOCATE_WINDOW`)은 리더의 고정 600초, 재시작 창은 각 노드 reconciler의 재시작 백오프 창(`restart.window`, 기본
  600초)이다. reconciler의 status 다시 쓰기는 이 작업자가 바꾼 `ha` 필드만 저장된 값 위에 다시 적용한다(`merge_ha` — 그 사이
  리더가 쓴 포기 오류를 덮지 않음, 1차 리뷰).
- `ha-it`: A의 HA VM QEMU를 두 번 kill → B로 옮겨 켜짐.

**노드 정상 종료 정책(단계 6).**
- `SettingsSpec.shutdown_policy: Option<ShutdownPolicy>`(`conditional`은 `None`으로 저장, 바꿀 때 `require_feature`),
  `NodeStatus.shutdown: Option<NodeShutdown { action, maintenance, at }>`, `NodeStatus::frozen()`(freeze 표시 + 오프라인).
- 판별은 opld `shutdown.rs`: SIGTERM 때 `systemctl list-jobs --no-legend --plain`(5초 제한)에서 `reboot.target`·`kexec.target`·
  `soft-reboot.target`의 start 작업이면 재부팅, `poweroff.target`·`halt.target`(또는 `shutdown.target`만)이면 끄기, 없거나 실패하면(macOS 포함) 호스트
  종료 아님. `systemctl stop`·업그레이드 재시작은 지금 그대로다.
- `Member::prepare_shutdown()`(`close` 전, API·실행기가 도는 동안): 표시를 쓰고, `migrate`면 유지보수로 바꾸고
  (`maintenance: true`) 비우기 Task를 `SHUTDOWN_MIGRATE_LIMIT`(600초)까지 기다린다. `Services::shutdown`은 reconciler를 멈춘 뒤·
  `ha.close()` 전에 이 노드의 활성 HA VM을 ACPI(`acpi_timeout`) → 강제로 끄고 `set_running(false)`(spec은 그대로 Running). 표시
  쓰기가 실패하면 경고하고 failover처럼 끈다. 리더는 얼린 노드를 복구하지 않는다(틱 필터와 `recover_node` 두 곳). 다시 등록할 때
  표시를 지우고 정책이 켠 유지보수만 끈다(노드가 꺼진 동안 관리자가 다시 켠 유지보수도 표시가 `maintenance: true`면 끈다 —
  README). `POST /nodes/{key}/unfreeze`(관리자, 온라인이면 409 `node_online`). 표시가 있는 노드는 `node_fits_with`가 거절한다
  ("is shutting down": freeze·failover는 `mark_offline`까지 온라인·리스가 살아 있어 복구·옮기기·되돌아가기·비우기·스케줄 대상이
  되던 것, 1차 리뷰). 그런 노드로의 마이그레이션은 409 `no_capacity`가 아니라 `node_unavailable`이다. `prepare_shutdown`은 표시를
  쓰기 전에 등록 작업을 멈추고 기다린다(과반수 없이 시작해 재시도하던 등록이 뒤늦게 성공해 표시를 지우고 유지보수를 끄지 않게,
  2차 리뷰).
- systemd: QEMU 스코프에 `--property=Before=opld.service`(멈출 때는 역순이라 opld가 끝난 뒤 스코프가 멈춤 — 정책이 VM이 살아
  있을 때 돈다)와 `--property=After=network-online.target remote-fs.target`(망·원격 파일시스템보다 먼저 멈춤),
  `opld.service`에 `TimeoutStopSec=15min`(보통의 `systemctl stop`·`restart`에도 적용 — README). 스코프의 `Before`는 Linux CI(`OPL_TEST_SYSTEMD=1`)의 `systemctl show -p
  Before`가 본다(macOS에서는 확인하지 못함). 이 계획 전에 켠 VM의 스코프에는 속성이 없다(다시 켜야 적용, README).
- 시험 고리 `[test] host_shutdown = "reboot"|"poweroff"`(`starve-it`·`ha-it` 빌드)가 판별을 대신한다. `ha-it`: poweroff +
  `failover`면 A가 끝나기 전에 HA VM이 꺼지고 B에서 켜짐, reboot + 기본이면 `recover_after`가 지나도 복구 안 됨·A가 돌아오면
  A에서 켜짐.

**보이기(단계 7).**
- CLI: `ha-group list|get|create|set|delete`(`--node NAME[:PRIORITY]`), `placement-rule list|get|create|set|enable|disable|delete`,
  `vm create|set`의 `--ha-state`·`--ha-group`·`--max-restart`·`--max-relocate`(하나라도 주면 정책 객체, 모두 기본이면 `true`),
  `vm migrate --force`, `node label`, `node unfreeze`, `node list` SHUTDOWN 열, `settings set shutdown_policy=…`.
- 웹: "HA 그룹"(노드마다 체크와 우선순위)·"배치 규칙"(종류, VM 이름·레이블, 노드 체크·레이블, 벌점, `ha_may_violate`, 켜기) 화면은
  백업 작업 화면 모양이고 누구나 읽고 admin만 바꾼다(422·409는 `errors.rule_conflict`·`group_in_use`·`placement_rule` 문구).
  watch 종류 `ha-group`·`placement-rule`을 늘 구독한다. VM 폼의 HA 절은 스위치(정책 있음) 아래 상태·그룹·횟수를 보이고
  `haSetting`이 서버와 같이 기본 정책을 `true`로 보낸다 — 단계 1~6 사이 웹 폼이 정책 객체를 bool로 덮던 것이 없어졌다. 실행 중이고
  HA가 활성이 아닌 VM은 상태 고르기에서 `started`·`stopped`가 막힌다. VM 상세 요약에 고가용성(상태·횟수)·HA 그룹·어긴 배치
  규칙·HA 오류. 마이그레이션 대화상자의 "배치 규칙 무시"는 배치 규칙 쓰기 권한(admin)에게만 보이는 체크(`force: true`).
  노드 화면에 레이블 열과 "레이블 편집"(`PUT /nodes/{key}`의 `labels`, 저장할 때 실시간 목록의 revision으로 If-Match, 연 사이
  레이블이 바뀌었으면 저장하지 않고 다시 열라는 오류 — 2차 리뷰), "얼림" 배지(`frozen()`과 같은 조건)와
  "얼림 풀기", 설정에 "호스트 종료 때 HA VM".
- README "배치 규칙과 HA 정책" 절과 HA·비우기·웹 UI 절의 고친 문장. 기본 `conditional` 때문에 업그레이드 뒤 노드를 재부팅하면
  HA VM이 다른 노드로 옮겨지지 않고 그 노드에서 다시 켜지는 동작 변화를 적었다.

**남은 것.**
- 균형 조정기의 위반 바로잡기와 묶음 옮기기 API(계획 53 — `Candidate.free_mib` 자리를 부하 점수로 바꾸고 같은 함수를 부름),
  미리 고른 복구 노드와 종료 failover의 빠른 복구(계획 51), 꺼진 `started` HA VM의 노드 옮기기, 호스트 종료 때 HA 아닌 VM의
  ACPI 종료.
- 웹은 새 watch 종류 두 개를 늘 읽으므로, 앞단 프록시가 업그레이드 중 옛 API 노드로 요청을 보내면 그 노드의 404로 실시간
  연결이 다시 시도를 되풀이한다(같은 노드의 내장 웹 UI는 해당 없음; 이전 계획의 새 종류도 같았다).

## 18. 구현(계획 51)

일곱 단계를 커밋 일곱으로 했다(1 빠른 리스 프로필과 복구 시간 측정, 2 HA 정책 필드·클러스터 설정과 HA 용량 예약, 3 우선순위·
병렬 복구와 미리 고른 복구 노드, 4 펜싱 설정 `NodeFence`·전원 끄기 실행기·`fence-test`, 5 리더의 전원 끄기 펜싱과 이웃 확인,
6 같은 노드 빠른 재시작과 게스트 멈춤 감지, 7 웹·문서). 설계 §8.1과 다르거나 설계가 정하지 않아 정한 것을 적는다.

**프로필과 경고(단계 1).**
- opld `[ha] profile = "fast"|"safe"`(기본 `fast`): `fast` = `fence_after_secs` 10·`watchdog_timeout_secs` 10, `safe` = 30·30(옛
  값). 두 값은 `Option<u64>`이고 적으면 프로필을 이긴다(`HaSection::fence_after_secs()`·`watchdog_timeout_secs()`가 실제 값).
  `RECOVER_MARGIN_SECS` 5 → 2(두 프로필 모두). 갱신 간격 규칙(`fence_after/10`, 0.2~2초)은 그대로라 `fast`면 1초. 그래서
  `recover_after`는 `fast` watchdog 22초·process 12초, `safe` 62초(process 32초). **업그레이드하면 `[ha]`를 적지 않은 노드가
  `fast`가 된다**(README에 적고 옛 동작은 `profile = "safe"`).
- 설계의 "둘 중 하나를 끈 설정"의 뜻: 계획 45 실행기 격리는 끄는 설정이 없어 늘 켜진 것으로 본다. 실제 `fence_after`가 30초
  미만이고 펜싱이 켜졌을 때 (a) 클러스터 링크가 하나(`cluster_links().len() < 2`, 계획 49를 안 씀), (b) `fencing = "watchdog"`인데
  `NOTIFY_SOCKET`이 없음(systemd 밖)이면 시작 로그에 `warn`하고 `profile = "safe"`를 권한다(`opld::config::ha_warnings`, 시작은
  막지 않음).
- `NodeHa.fence_after_secs: Option<u64>`(비면 쓰지 않음, 계획 51 노드인지와 리더의 전원 끄기 시점에 씀).
- 유닛: `RestartSec=5` → `2`, `[Unit] StartLimitIntervalSec=60`·`StartLimitBurst=10`(2초 간격이 기본 시작 제한 10초 5번에 걸리지
  않게). systemd `WatchdogSec=30s`는 그대로다 — `core`가 `fence_after`(10초)보다 오래 막히면 리스가 먼저 끝나 HA 펜싱이 데몬 재시작보다
  먼저 온다. 장치 제한 10초에서 데몬 재시작이 다시 arm하기까지의 여유는 `RestartSec`을 빼고 약 8초다(README "HA watchdog과의 관계").
- 시험 고리: opl-peer 기능 `fault-it`(전역 `AtomicBool`, 켜지면 `Peers`의 모든 보내기와 `PeerService::authorize`의 모든 받기가
  `unavailable`, `opl_peer::fault::{isolate, heal, isolated}`), `ha-it` 빌드의 제어 소켓 `POST /v1/test/isolate`·`/v1/test/heal`.
  실제 프로세스를 망에서 떼는 방법이 없어서다.
- **잰 복구 시간**(`ha-it` `recovery_takes_at_most_25s_with_the_default_profile`, 이 Mac, `fast` + process 펜싱, 디스크 없는 VM,
  리스 끊김부터 대상 노드 QEMU 시작까지): kill -9 12.3초, 망 끊기(`isolate`) 12.4초, SIGSTOP 12.8초. 시험은 25초 이하를 본다.
  회차마다 리더가 아닌 노드를 장애 노드로 고르고 VM을 새로 만든다(같은 호스트의 공유 디렉터리 디스크는 SIGSTOP한 QEMU가 이미지
  잠금을 쥐어 다른 노드가 켤 수 없으므로 디스크 없는 VM). 전원 끄기 펜싱 시험(단계 5)은 `recover_after`를 310초로 늘린 노드를
  망에서 떼어 `fence_after`(10초) + 끄기·확인 뒤 옮기는지 본다.

**정책 필드와 클러스터 설정(단계 2).**
- `VmHa.priority: u8`(0~100, 기본 50, 클수록 먼저)와 `VmHa.guest_watch: Option<GuestWatch { timeout_secs: u32 }>`(30~3600, 기본 60
  — 리뷰 뒤 최솟값 10 → 30). 둘 다 **기본값이면 쓰지 않아** 기본 정책은 계속 `true`, 계획 50의 정책 객체는 바이트까지 그대로다. 범위 밖은
  `ValidationError::HaPriority`·`GuestWatch`(400), 기본이 아니면 `require_feature(FAST_HA)`(409 `cluster_upgrading`).
- 기능 이름 `fast-ha`(`FEATURES` 끝, `opl_types::node::FAST_HA`) 하나가 이 계획의 새 모양을 모두 가린다: 기본이 아닌
  `ha.priority`·`ha.guest_watch`, 설정 `ha_reserve`·`ha_start_concurrency`, 새 종류 `NodeFence` 쓰기, 기록 Task `node-power-off`,
  이웃 확인 RPC 부르기. `NodeHa.fence_after_secs`·`NodeStatus.fence_check`는 옛 노드가 무시하므로 가리지 않는다. 받은 백업의
  `vm_spec`은 `fast-ha`를 모르는 노드가 있으면 우선순위·감시만 기본으로 낮춘다.
- **용량 예약은 클러스터 설정이다**(설계는 위치를 정하지 않음): `SettingsSpec.ha_reserve: Option<HaReserve::{Nodes(1~8), Percent(1~90)}>`
  (없음 = `none`, 쓰지 않음)와 `ha_start_concurrency: Option<u8>`(1~32, 없으면 4, 쓰지 않음). 리더가 쓰는 값이라 노드마다의
  `[ha]`가 아니다. `SettingsPatch`의 `ha_reserve`는 `"none"`으로 지운다.
- **용량 셈(설계와 다름).** 설계는 "노드 N대가 죽어도 모든 HA VM이 들어갈 자리"였으나 조합을 다 보지 않고 메모리 합으로 센다
  (VMware 백분율 방식): 대상 노드 = HA 가능·온라인·유지보수 아님·종료 표시 없음, `capacity` = 그 노드들의 메모리 합, `reserve` =
  `nodes: N`이면 가장 큰 N대의 합(N이 노드 수보다 크면 전부), `percent: P`면 `capacity × P / 100`, `used` = 그 노드들에 자리 잡은
  **모든** VM(HA 아닌 VM 포함, 마이그레이션 중이면 원본 한 번)의 메모리 합. `started` HA VM을 켜는 요청만 `used + vm ≤ capacity −
  reserve`를 보고 넘으면 409 `ha_capacity`(`details { capacity_mib, reserve_mib, used_mib, need_mib }`): 켠 채 만들기, 꺼진 →
  켜짐 고치기, 켜진 HA VM 메모리 늘리기, 꺼진 VM start. **마이그레이션("옮기기")은 합이 바뀌지 않아 보지 않는다**(설계와 다름),
  HA 복구·되돌아가기도 보지 않는다. 함수 `opl_api::scheduler::ha_capacity(inv, settings, vm) -> Result<(), HaCapacity>`(단위 시험).

**우선순위·병렬 복구와 미리 고른 노드(단계 3).**
- `recover_node`가 VM을 `priority` 높은 것 → 이름 순으로 옮긴다(`recovery_order`). 대상 노드마다 "이 리더가 옮겼고 아직 그
  노드에서 `Running`이 아닌" VM이 `ha_start_concurrency`개면 그 노드로는 더 옮기지 않는다. 미리 고른 대상이 상한이면 그 자리에서
  `pick`으로 다시 고르고(다른 노드가 나오면 그리로 옮긴다), 고른 노드가 상한이면 다음 후보 노드는 쓰지 않는다(그 VM은 죽은
  노드에 남아 다음 틱에 다시 본다 — 그 노드에서 순서를 지킴). 진행 중 기록은 리더 메모리 `Starting { target → { vm → 옮긴 때 } }`이고
  그 노드에서 Running, 다른 노드·꺼짐, 120초(`HA_START_TIMEOUT`)면 뺀다. 리더가 바뀌면 비어 한 번 넘칠 수 있다(받아들임).
  상한은 틱마다 설정에서 읽고 읽지 못하면 그 틱은 복구하지 않는다.
- 미리 고르기: 복구할 노드가 없는 틱에서 `HaManagerConfig.plan_every`(10초)마다 노드 N마다 `Assign`에서 N의 VM을 빼고 N의
  `recoverable` VM을 같은 순서로 `pick`(eligible = N 아님·리스 살아 있음, `placement::best` — 계획 53이 점수를 바꾸면 따라감)해
  `Plans { at, targets }`(메모리, Raft 아님)에 둔다. 복구 때 계획이 있고(30초 = `plan_every`×3보다 새 것) 그 대상이 지금도
  eligible·`node_fits`·`hard_violations` 없음·상한에 걸리지 않음이면 그대로 쓰고, 아니면 그 자리에서 `pick`. debug 로그에 계획을
  쓴 수·다시 고른 수. 설계의 "복구 때 스케줄링 계산을 기다리지 않는다"는 지금 규모에서 `pick` 자체가 밀리초라 시간 차이는 작고,
  주로 같은 결과를 미리 확인해 두는 자리다(계획 53의 무거운 점수 함수에서 의미가 커짐).

**펜싱 설정과 실행기(단계 4).**
- **자리 — 새 종류 `NodeFence`(설계와 다름).** 설계는 노드 스펙 `fence`였으나 `NodeSpec`이 `deny_unknown_fields`라 옛 노드가
  Node 기록을 풀지 못해 노드 목록 전체가 깨진다(계획 50 노드 태그와 같은 문제). `Kind::NodeFence`(`node-fence`, id = 노드 id,
  이름 = 노드 이름), `NodeFenceSpec { methods: Vec<FenceMethod> }`(1~4개, `deny_unknown_fields`), `FenceMethod::{Ipmi, Redfish,
  Command}`(`kind`로 나뉨). 노드 삭제가 함께 지운다(있을 때만).
- **비밀값 — `password_secret` 대신 `password`(설계와 다름).** 따로 된 비밀 저장소가 없어 realm·원격과 같은 지금 방식: 스펙에
  그대로 두되 API는 내보내지 않고 `has_password`만, watch로도 내보내지 않고 `Debug`가 가린다. `PUT`에서 빼면 같은 종류·주소·
  사용자의 저장된 값을 유지(대상을 바꾸고 빼면 400), 빈 글이면 지운다. 읽기·쓰기 모두 관리자, 위트니스는 400, 쓰기는
  `require_feature(FAST_HA)`.
- 방법: `ipmi { address, port = 623, user, password, lanplus = true }` → `ipmitool [-I lanplus] -H -p -U -E chassis power
  off|status`(비밀번호는 `IPMI_PASSWORD`), "Chassis Power is off"면 꺼짐. `redfish { url, user, password, system_id?, ca_pem?,
  insecure = false }` → Basic 인증, `system_id`가 없으면 `/redfish/v1/Systems`의 멤버가 하나일 때 그것, `ForceOff`, `PowerState`;
  `http://`도 받음(가짜 서버 시험). `command { off, status }` → `/bin/sh -c`, 환경 `ORBIVISOR_NODE_NAME`·`_ID`·`_ADDRESS`, `status`의
  첫 줄이 `off`/`on`(그 밖은 모름).
- 실행기 `opl_api::power_fence`: 트레이트 `PowerFence { off, status }`, 실제 `Fencer { ipmitool, timeout }`(명령·HTTP 하나 15초
  `FENCE_CALL_TIMEOUT`, `kill_on_drop`), `power_off(fence, node, spec) -> PowerOff::{Off { method }, Failed(Vec<String>)}`(방법을
  차례로: off → 1초마다 status를 15초 `FENCE_CONFIRM_TIMEOUT`까지 → 꺼짐이면 끝, 아니면 다음), `check(…)`. `AppState.fence`, opld
  `[ha] ipmitool`(기본 PATH의 `ipmitool`). 새 크레이트 없이 `reqwest`·`tokio::process`.
- API `GET|PUT|DELETE /nodes/{key}/fence`(응답 `NodeFenceView`), `POST /nodes/{key}/fence-test`(→ `{ checked_by, methods }`, API
  노드가 그 노드가 아니면 스스로, 그 노드면 이웃 RPC; 409 `no_fence_checker`, 502 `fence_check_failed`). 피어 RPC `FenceStatus`
  (`FenceSource`, `PeerService::with_fence`, `work` 실행기; 옛 노드는 None).
- 걱정이던 `command` 방법의 손자 프로세스는 리뷰 뒤 프로세스 그룹으로 끝낸다(아래).

**리더의 전원 끄기와 이웃 확인(단계 5).**
- `HaManager`의 메모리 시도 기록(노드 → 시작·임기·결과). 틱에서 노드가 HA 가능·`NodeFence` 있음·`NodeHa.fence_after_secs`가 있음
  (계획 51 노드)·얼리지 않음·켜기로 된(`spec.power == Running`) HA 활성 VM 있음·`lease_age ≥ fence_after`면 한 번 `power_off`를 따로 `tokio::spawn`한다(틱을
  막지 않음). `Off`면 확인 시각을 적고 노드는 "꺼짐 확인 뒤 받은 갱신이 없으면" 죽은 것으로 본다 — `dead()`가 `expired`
  (`lease_age ≥ recover_after`) `|| (확인됨 && 마지막 갱신 < 확인 시각)`이고 `recover_node`의 다시 보기도 같다. `Failed`면 warn과 기록 Task,
  이 임기에는 다시 시도하지 않고 `recover_after`(watchdog 기다림)로 물러난다. 리스가 다시 살아나면 기록을 지우고, 임기가 바뀌거나
  리더를 잃으면 비운다. 끄는 중·끈 노드는 복구 대상·미리 고르기·동시 시작 계산에서 뺀다.
- 기록 Task `TaskOp::NodePowerOff { method: Option<FenceKind>, reason }`(target = 노드, `succeeded`/`failed`, 실패 이유는
  `status.error`에 `; `로, 실행하지 않음), 모든 노드가 `fast-ha`일 때만 쓴다.
- `HaManager::power_off(node, reason)`(pub, 채널로 루프에 넣음): 계획 52의 상태 검사가 부르고 확인되면 같은 경로로 복구된다.
  자기 자신·`NodeFence` 없는 노드·이 임기에 이미 시도한 노드면 로그만, 리더가 아니면 버린다. 이 계획에서는 opl-api 시험만
  부른다.
- 이웃 확인: opld `FenceWatch`(`work`)가 등록 뒤 곧바로, 그 뒤 1시간(`FENCE_CHECK_INTERVAL`)마다, 자기 `NodeFence` revision이
  바뀌면(60초마다 봄) 이웃 하나(`opl_types::fence::neighbour`: 온라인·위트니스 아님·`fast-ha`인 다른 노드 가운데 id 순으로 다음,
  끝이면 처음)에게 RPC를 보내 `NodeStatus.fence_check: Option<FenceCheck { at, by, error }>`에 `UpdateStatusIf`로 쓴다. 모든 방법이
  읽지 못했으면 `error`가 있고 이것이 `fence_unreachable`(`NodeStatus::fence_unreachable()`)다. 이웃이 답하지 않으면 기록 없이
  1분 뒤 다시. `NodeFence`가 없으면 `None`, 이웃이 없으면 확인하지 않음. 등록(`record_node`)은 이 필드를 지킨다. 설계는
  "시작 때"였으나 등록 뒤(노드 목록·피어가 있어야 함)다.
- 시험 고리 `[test] recover_margin_secs`(그 노드의 `recover_after`를 늘려 전원 끄기가 기다림 없이 복구하는지 봄).

**같은 노드 빠른 재시작과 게스트 감시(단계 6).**
- `started` HA VM의 창 안 첫 크래시는 지연 0으로 다시 켠다(`FailureTracker::record_first_free`, 둘째부터 지금 백오프). 설계의
  "1초 안에"는 감지(QMP 이벤트) + 0초다. 시험은 `MockHypervisor`와 멈춘 시간으로 "크래시 → 시작 호출 < 1초".
- `guest_watch`가 있는 VM만 QGA 채널을 단다(`VmRuntimeSpec.guest_agent`, `resolve.rs`의 공통 함수, 받는 쪽 같음): `-chardev
  socket,id=qga0,path=<run>/vm/<id>/qga.sock,server=on,wait=off -device virtio-serial-pci,id=qgaser -device
  virtserialport,bus=qgaser.0,chardev=qga0,name=org.qemu.guest_agent.0`(두 아키텍처 모두 `virtio-serial-pci`, NIC 뒤·`-vnc` 앞).
  모든 VM에 달면 장치가 바뀌어 옛 노드와의 마이그레이션이 깨지기 때문이다. 같은 까닭으로 감시는 멈춘 VM에서만 켜고 끈다(리뷰 뒤,
  아래).
- `opl_qemu::qga::ping(path, timeout)`(`guest-sync-delimited` 뒤 `guest-ping`), `Hypervisor::guest_ping(id) -> Result<bool>`(QEMU
  3초, 채널 없으면 `InvalidSpec`), `MockHypervisor::set_guest_agent(id, GuestAnswer)`. 작업자가 `started`+`guest_watch` VM이 QMP
  `running`일 때만 `ReconcilerConfig.guest_ping_interval`(5초)마다 부르고, 이 QEMU에서 한 번 답한 뒤에야 감시한다(메모리). 마지막
  답에서 `timeout_secs`가 넘으면 `last_error = "guest agent did not answer for Ns; restarting"`, 강제 끄기 = 크래시 한 번
  (`max_restart`를 넘으면 계획 50의 옮기기). 일시 정지·마이그레이션 중에는 세지 않고, pid가 바뀌거나 꺼지면 초기화한다.

**보이기(단계 7).**
- CLI(단계 2~5): `settings set ha_reserve=none|nodes:N|percent:P`·`ha_start_concurrency=N`, `vm create|set --ha-priority N
  --guest-watch SECS|--no-guest-watch`, `node fence get|set -f FILE(- = stdin)|delete <node>`, `node fence-test <node>`,
  `node list`의 `FENCE` 열(`-`·`ok`·`unreachable`).
- 웹: 노드 화면 "펜싱" 열 아래 `status.ha.recover_after_secs`("N초 뒤 복구")와 `fence_check.error`가 있으면 "전원 펜싱 닿지
  않음" 배지(제목에 이유). 노드 메뉴 "전원 끄기 펜싱…"(위트니스 제외, admin — 노드 쓰기 권한이 admin뿐): 방법 목록 더하기·빼기·
  위아래, 종류별 칸, 비밀번호는 비우면 저장된 값 유지(`has_password`면 안내), 저장 `PUT`(연 설정의 revision으로 If-Match), "펜싱
  지우기" `DELETE`, "시험" = `fence-test` 결과 표(저장된 설정만 시험하므로 고친 뒤에는 막음, 404면 빈 목록으로 시작). 지우기는
  확인 대화상자 없이 곧바로다(설정만 지우고 다시 만들 수 있음). VM 폼 HA 절에 "복구 우선순위"(0~100)·"게스트가 멈추면 다시
  켜기"(스위치 + 초, 30~3600, 끄면 초는 남김)와 폼 검증, VM 상세 요약에 "HA 복구 우선순위"·"게스트 에이전트 감시". 설정 "HA 용량"
  절(예약 없음·가장 큰 노드 N대·메모리 P%, 대상 노드당 동시 시작 — 저장은 두 값을 함께 보냄). `ha_capacity`는 `errorMessage`가
  `details`로 "… {need}가 필요한데 예약 {reserve}를 빼면 {capacity} 가운데 {free}만 남았습니다." 문구를 만든다(`details`가 없으면
  앞 문장만). Task 이름 `node-power-off`, `nodes.fence.errors`의 `no_fence_checker`·`fence_check_failed` 문구.
- README "HA VM과 자기 펜싱": 프로필·기본값 표, 업그레이드 때 기본이 바뀌는 것, 시작 경고 두 가지, 갱신 1초, 전원 끄기 펜싱(종류·
  환경 변수·`ipmitool`·비밀값·순서·실패하면 watchdog 기다림·리더가 BMC에 닿아야 함·이웃 확인·`fence-test`), 우선순위·동시 시작·
  미리 고른 노드, 용량 예약(셈, HA 아닌 VM도 셈, 마이그레이션 안 봄), 같은 노드 빠른 재시작, 게스트 감시. "제어부 가용성"의
  `RestartSec=2`와 장치 제한 10초 안의 다시 arm 여유. 30·30·65초가 적힌 곳을 고쳤다.
- `ha-it` `an_ha_vm_recovers_to_the_next_group_node_and_fails_back`의 정리 단계가 되돌아가기 마이그레이션 Task가 끝나기 전에
  `vm stop`을 보내 `vm_busy`로 한 번 실패했다 — VM이 B에서 돈 뒤에도 그 Task가 `succeeded`가 될 때까지 기다리게 했다(시험만).

**1차 리뷰 뒤 고친 것.**
- 리스 값을 키운 노드(`fast` → `safe`, process → watchdog, 펜싱을 새로 켬)는 키운 `NodeHa`를 노드 기록에 쓰기 전에는 `HaGuard`가
  `arm`을 거절한다(`ArmError::Unrecorded` — HA VM을 켜거나 붙이지 않고 다시 본다, `opld::bootstrap::ha_raised`; 2차 리뷰 뒤로는
  `status.ha`만 먼저 쓰면 풂 — 아래).
  리더가 옛 짧은 `recover_after`로 옮기는 동안 이 노드가 HA VM을 이어 돌리지 않게 한다. 같거나 작아지면 지금처럼.
- 게스트 감시: QMP `RESET`과 QEMU가 산 채 받은 `SHUTDOWN`을 `EventKind::Reset`으로, `STOP`·`RESUME`을 `EventKind::PauseChanged`로
  알린다(QGA 소켓이 있는 VM만 — 다른 VM의 이벤트 흐름은 그대로). 작업자는 `Reset`이면 `answered_once`를 지우고(에이전트가 다시 답할
  때까지 감시 안 함) 둘 다 `seen_at`을 비운다. 놓친 이벤트에 대비해 최솟값 30초. 게스트 이벤트는 마이그레이션 보류 중의 이벤트
  자리를 덮지 않고, 작업자 없는 VM의 HA 실행 표시도 바꾸지 않는다.
- 켜진 VM의 수정에서 `guest_watch`의 있음/없음이 바뀌면 409 `vm_running`(HA를 통째로 끄는 것도 — 장치가 바뀐다; 감시를 둔 채
  HA만 멈추려면 `state: ignored`). 시간만 바꾸기는 된다.
- 전원 끄기는 켜기로 된 HA 활성 VM이 있는 노드만(꺼진 `state: stopped` VM만 있는 노드는 `recover_after` 뒤 자리만 옮김). 끄기
  요청이 실패해도 같은 방법으로 상태를 한 번 읽어 `Off`면 꺼진 것(이미 꺼진 노드에 409를 주는 BMC, 끊긴 호스트).
- 미리 고르기 계산은 `spawn_blocking`(복제한 `Inventory`·`Leases`)으로 core 실행기 밖에서 하고 결과는 다음 틱에 받는다(계산 중엔
  새로 시작하지 않음, `Planner`). 노드별 사용 메모리를 계획 한 번에 미리 세는 것은 `placement` 공용 함수를 바꿔야 해 하지 않았다.
- 명령(ipmitool·command)은 자기 프로세스 그룹(`process_group(0)`)에서 돌고, 제한 시간으로 버려지면 그룹째 `killpg(SIGKILL)`한다.
  끝남은 `wait()`로 보고 출력은 그 뒤 1초만 기다린다(파이프를 쥔 백그라운드 자식). `PR_SET_PDEATHSIG`는 쓰지 않았다 — `pre_exec`
  (unsafe)가 필요하고, tokio에서는 부른 스레드가 끝날 때 신호가 가며, 직계 자식만 덮는다.
- Redfish: 프록시 환경 변수를 따르지 않음(`no_proxy`), URL에 자격 증명이면 400, `Members[].@odata.id`가 `url`과 다른 출처면 오류.
  `command` 방법의 실패 출력은 오류(작업·`fence_check`·`fence-test`)에 담지 않고 종료 상태만(출력은 경고 로그). 오류 문구는 2차
  리뷰 뒤 더 줄였다(아래).
- 이웃 확인이 3번 이어 실패하면 `fence_check.error = "no neighbour could check the fence: …"`(`by`는 빈 글). 부탁할 이웃이 없는
  것은 세지 않는다.
- `ha_capacity`의 `used`에 유지보수(비우기)·종료 중(얼리기 빼고) HA 가능 노드의 켜진 `started` HA VM을 센다(곧 옮겨 옴).
- 복구 로그는 실제로 옮긴 틱에만 warn(아니면 debug). 웹 펜싱 대화상자 "저장된 비밀번호 지우기", `node fence set -f`가 `get --json`
  출력을 받음. README: `command`가 어디서 도는지(`off` 리더, `status` 이웃·API 노드·리더), 재시작 여유의 `ExecStopPost`, process
  펜싱 여유 2초, 게스트 재부팅.

**2차 리뷰 뒤 고친 것.**
- 리스 값을 키운 노드는 전체 등록(`register_node`, underlay 이름 풀기 포함)을 기다리지 않고 `status.ha`만 먼저 쓴 뒤
  붙들기를 푼다(`opld::bootstrap::record_raised_ha`, 시도마다 2초 제한, 0.5초마다 다시). 등록의 underlay 이름 풀기에도 2초
  제한. 이전 프로세스가 켜 둔 watchdog을 다시 arm하지 못한 채 등록이 DNS에 묶여 호스트가 재시작되지 않게 한다.
- 복구·미리 고르기 대상은 리스 나이 < 그 노드의 `fence_after`(알리지 않는 옛 노드는 `recover_after`)인 노드만(`ready`). 두 노드가
  함께 죽고 한쪽만 꺼짐을 확인했을 때 다른 죽은 노드(아직 `recover_after` 전)로 옮기지 않는다.
- 리스 만료 전원 끄기는 리더 자신의 리스(`leases.renewed[me]`)가 `fence_after`보다 새로울 때만 한다 — 갱신 기록이 없으면 끄지
  않는다(리더 쪽 문제로 갱신이 멈추면 모든 노드의 리스가 함께 끝나 보임). 한 임기에 끄기를 시작하는 노드는 HA 가능 노드의 소수
  (`(n - 1) / 2`, 적어도 1)까지(`power_off(node, reason)` 요청도 셈), 넘으면 경고 한 번 뒤 watchdog 기다림.
- 끄기 실패가 watchdog 기다림으로 이미 옮긴 뒤에 오면 경고·기록 Task 오류가 "power off failed; the node's VMs were already
  recovered after the watchdog wait: …".
- 메모리 스냅샷의 `guest_watch` 있음/없음이 지금과 다르면 되돌리기를 409 `snapshot_mismatch`로 거절(QGA 장치가 달라
  `snapshot-load`가 실패하므로).
- `command` 방법은 자식 환경을 비우고 `PATH`와 `ORBIVISOR_NODE_*`만 넘긴다. 이 방법이 admin에게 모든 노드의 root 명령 실행을 주는
  셈임을 README에 적었다.
- Redfish `system_id`는 `[A-Za-z0-9._-]`만(`.`·`..` 거절), `url`은 scheme·호스트·포트뿐(경로·쿼리·조각 400, `FenceError::UrlPath`).
- 실행기 오류(`fence_check.error`·`node-power-off` Task·`fence-test`)는 방법 종류와 일반 이유만: `ipmitool`·`command`는 종료 상태,
  Redfish는 HTTP 상태나 "connection failed"·"timed out", 시스템 찾기 오류도 BMC 호스트 없이. 호스트·stderr는 경고 로그에만.
  `fence-test`(admin 전용)도 같은 실행기라 같은 문구다(자세한 것은 그 노드의 로그).
- 비밀번호 유지 조건(포트, Redfish `ca_pem`·`insecure`)을 400 문구와 README에, `oplctl node fence set -f`가 입력 파일의 `revision`을
  `If-Match`로 보낸다. README에 `ha_capacity` 거절 경우(켜진 VM의 HA를 `started`로)와 전원 끄기 시간(방법 하나 최대 약 45초,
  watchdog 기다림과 나란히)을 더했다.

**남은 것.**
- 리스 갱신이 1초로 늘어 커진 리더 부하로 계획 47의 규모 시험 표를 다시 잰다. 미리 고르기 계산(HA VM × 노드 × VM)의 시간도
  svc01 규모 시험 뒤 잰다.
- 복구 우선순위는 한 노드 안에서만 지킨다 — 여러 노드가 함께 죽으면 노드 순서대로 옮긴다.
- 게스트 감시의 `guest_ping`(최대 3초)은 그 VM의 작업자를 붙잡는다(그동안 그 VM의 다른 맞추기가 늦다).
- 리뷰가 짚은 CI의 `fault-it`(`isolate`) 시험 문제는 건너뛰었다(이 계획의 검증은 `ha-it`을 Mac에서 돌림).
- 계획 52의 상태 검사 → `HaManager::power_off`, 계획 53 점수로 미리 고른 노드, 꺼진 `started` HA VM의 노드 옮기기, 종료
  failover의 빠른 복구.
- 실제 BMC(IPMI·Redfish)와 진짜 watchdog 10초에서의 데몬 재시작 여유는 자동 시험이 없다(가짜 `ipmitool`·Redfish 서버·
  `process` 펜싱만).

## 19. 구현(계획 52)

일곱 단계를 커밋 일곱으로 했다(1 종류·결과 타입과 리스 갱신에 싣기, 2 `HaCheck` API와 CLI, 3 노드 쪽 probe(opl-driver), 4 노드
검사기와 인터페이스·결과 보고(opld), 5 리더 판단 — grace·과반·경고·마이그레이션·전원 끄기와 대상 거르기, 6 `restart_elsewhere`,
7 웹·문서). 설계 §8.2와 다르거나 설계가 정하지 않아 정한 것을 적는다.

**모양과 기능 이름(단계 1·2).**
- 기능 이름 `ha-checks`(`FEATURES` 끝, `opl_types::node::HA_CHECKS`)가 새 모양을 가린다: `HaCheck` 만들기·바꾸기(지우기는 보지
  않음), 기록 Task `ha-check`·`bond-degraded`, 리더의 경고 아닌 대응과 `VmHaStatus.evict` 쓰기. 리스 갱신의 `checks`와
  `NodeStatus.interfaces`·`checks`는 옛 노드가 무시하므로 가리지 않는다. 기능이 모자라면 리더는 대응 대신 임기마다 warn 로그 한 번.
- 새 종류 `HaCheck`(`ha-check`, 이름 = 검사 이름, status 비움). `HaCheckSpec { role, target, on_fail?, grace_secs = 10, nodes,
  enabled = true }`(`deny_unknown_fields`, 기본값은 쓰지 않음). `HaCheckTarget::{Link { iface }, Ping { addresses, via?, need = 1 },
  Pool { name }}`(`kind`로 나뉨). **설계와 다름:** `nodes: { <node>: target }`의 키는 노드 이름이고 덮기 대상의 종류는 `target`과
  같아야 한다(400). 역할별 허용 대응(`management`: warn·fence, `storage`: warn·restart_elsewhere·fence, `vm`: 넷 다) — **`storage`의
  `migrate`는 거절**한다(디스크 I/O가 멈춘 VM은 라이브 마이그레이션이 끝나지 않는다). 검증: grace 10~600(검사 간격 5초의 두
  배 — 한 번의 실패로 대응하지 않게; 기본도 10이라 직렬화 모양은 그대로), 인터페이스 이름
  `valid_ifname`, 주소 1~8개·겹침 없음, `need` 1~주소 수, `pool`은 `storage`만. API가 더 보는 것: 덮기 노드 이름 있음, 풀 있음·
  `is_shared`(본 대상과 덮기 모두), `cluster-link-`로 시작하는 이름 거절, 64개 상한(`MAX_HA_CHECKS`, 리스 갱신 크기를 묶음,
  만들기만·잠금 없음).
- **HaCheck는 watch 종류가 아니다**(설계가 정하지 않음): 웹이 늘 구독하는 새 종류는 업그레이드 중 옛 API 노드에서 404가 되풀이된다
  (계획 50 §17). 웹은 목록을 읽고 바꾼 뒤 다시 읽는다. 원격 중계의 읽기 허용 목록에는 넣었다.
- 결과 `CheckReport { name, state, error?(200바이트), since?(노드 시계, 화면용) }`, `CheckState::{Ok, Degraded, Failing, Unknown}`.
  `Degraded`는 통과(본딩 일부 포트·multipath 일부 경로), `Unknown`은 이 노드에 해당 없음(과반 셈·대상 거르기에서 빠짐). 뒤 버전이
  더할 상태·인터페이스 종류는 `Unknown`·`InterfaceKind::Other`로 읽는다(`#[serde(other)]` — 옛 리더가 리스 갱신을 거절하지 않게).
  `BondState`·`PortState`에도 `Unknown`(`#[serde(other)]`)을 더해 뒤 버전의 상태를 "모름"으로 읽는다(알림·웹에서 이상이 아님).
- **결과가 가는 길 — 리스와 화면용 노드 상태 둘(설계는 리스만).** `LeaderRequest::RenewLease { node, checks }`(비면 옛 형식과 바이트까지
  같음, 옛 리더는 버림). 노드는 `Cluster::set_lease_checks`로 최신 결과를 두고 `renew_lease`가 매번 싣는다(두 갱신 경로 그대로).
  리더 `LeaseTable::renew`가 같은 잠금에서 `Leases.checks`에 통째로 넣고(128개까지, 빈 목록이면 항목 지움) 임기가 바뀌면 비운다.
  리더는 판단에 리스의 것만 쓴다(1초마다 새로움, Raft 쓰기 없음). 화면용 `NodeStatus.checks`·`interfaces`는 저장된 값과 다를 때만
  쓰고, 등록(`record_node`)이 두 필드를 지킨다.

**노드 쪽(단계 3·4).**
- probe는 opl-driver 새 `hostcheck`(라이브러리라 Linux 실제 시험 `tests/host_check.rs`(`net-it`)를 둘 수 있음): `NetFs { sys, proc }`
  (시험 빌드 `ha-it`의 `[test] health_root`가 가짜 트리로 바꿈), `link_state`, `bond_status`·`parse_proc_bonding`, `parse_ip_addr`·
  `interfaces`·`refresh_interfaces`, `ping_args`·`ping`, `probe_dir`, `iscsi_logged_in`, `pv_paths`·`parse_pvs`·`pvs`, `rados_args`·
  `rados_result`·`rados`. 모두 cfg 없이 컴파일되고 Linux 판단은 `cfg!(target_os = "linux")` 값이다. 명령은 `tokio::process` +
  `LC_ALL=C` + `kill_on_drop` + 제한 시간, 새 크레이트 없음.
- 본딩 포트 판정: `down`(carrier 0·MII down·operstate down), 802.3ad에서 활성 aggregator가 아니거나 상대 MAC이 0이면
  `lacp_mismatch`, 올라온 포트 가운데 가장 빠른 것보다 느리면 `speed_mismatch`. 본딩은 모두 `up` → `up`, 일부 → `degraded`, 링크가
  있는 포트가 없음 → `down`(설계의 "올라온 포트 없음"을 정함: LACP 상대가 없는 802.3ad는 모든 포트가 `lacp_mismatch`여도 링크가
  있으면 `degraded`). 블록 장치는 `/sys/class/block`, SCSI 상태가 없는 장치는 있으면 running으로 본다. dm 슬레이브가 또 dm이면(multipath
  위의 파티션) 4단까지 따라 내려간다.
- 링크 검사의 없는 인터페이스: probe는 `unknown`("interface X not found on this node")이고 검사기가 그 이름을 본 적이 있으면(저장된
  `NodeStatus.interfaces`로 시작 — 다시 시작해도 남음, `ip` 목록이나 앞 회차) `failing`이다 — 이름이 다른 노드 하나만 `failing`이
  되어 과반 보호 없이 VM을 옮기지 않게. 핑의 `via`도 같다. API는 이름을 거절하지 않는다(노드마다 다르면 `nodes`로 덮음).
- 브리지의 링크는 업링크로 본다(`bridge_uplinks`): `brif`의 물리(`device`)·본딩·쌓은 장치(`lower_*`, VLAN) 포트만 세고(tap
  `tun_flags`·veth·`oplx*`는 뺌) 링크가 있는 것이 없으면 `failing`, 그런 포트가 없으면 브리지 carrier(tap이 carrier를 지키므로).
- 핑은 `-c 2 -i 0.2`(하나라도 답하면 통과, 전체 시간은 그대로). 블록 장치 상태는 SCSI `running`과 NVMe `live`, PV 이름은 링크를
  따라간다(`/dev/disk/by-id/…`). 자동 검사는 마지막으로 짝지은 인터페이스를 기억해 주소를 잃어도 계속 검사한다.
- 명령 probe(`ping`·`rados`)는 실행 파일이 없거나(`NotFound`) 실행할 수 없으면(`PermissionDenied`) `ProbeError::Unavailable` →
  `unknown`("ping not found: install iputils-ping"). 패키지 `Depends`에 `iputils-ping`. RBD는 stderr에 `error stat-ing`으로 시작하고
  `(2)`가 있는 줄이 있을 때만 통과(다른 ENOENT — 지운 풀·없는 keyring — 는 실패), 오류 문구는 마지막 줄에서 시각·스레드 id를 뗀 것,
  `RBD_PROBE_TIMEOUT` 8초(안쪽 3 + 3초보다 넉넉히).
- `StopMode::Kill`(새 변형): QMP 없이 곧바로 SIGKILL, 10초(`KILL_TIMEOUT`, 큰 VM의 메모리 풀기 — `Force`의 SIGKILL 뒤 기다림도 같은
  상수) 안에 끝나지 않으면 `Timeout`(D 상태 QEMU). `EVICT_TIMEOUT` 20초보다 짧다. 모의 드라이버의 Kill은 실제 QEMU처럼 `Crashed`
  이벤트를 보내고 `set_unkillable`이면 `Timeout`.
- opld 새 `checks.rs` `HealthChecker`(`work` 실행기, 위트니스 제외): 5초(`CHECK_INTERVAL`)마다 켜진 `HaCheck`와 자동 검사를 이 노드의
  대상(`target_for(내 이름)`)으로 동시에 돌린다. 인터페이스 목록은 30초(`INTERFACES_EVERY`)마다 `ip -j -d addr show`, 그 사이는
  sysfs만. `ip`가 없으면(macOS) 기록된 목록을 둔다. 노드 상태에는 64개까지(넘치면 주소가 있는 것을 먼저) 쓰고 자동 검사의 짝
  찾기는 자르기 전 목록에서 한다. 핑은 시스템 `ping`(`[ha] ping`, macOS의 `via`·IPv6는 `failing`). 풀 검사:
  공유 dir·NFS는 디렉터리 `stat` + `.orbivisor-health-<노드 id>`를 새로 열어 64바이트 읽기를 3초 안에(넘으면 `failing` "not
  responding", 그 작업이 끝날 때까지 새로 띄우지 않음 — `DirProbe` 트레이트로 바꿔 끼움), lvm-shared는 iSCSI 세션과 multipath
  경로(sysfs, `pvs`는 60초마다), RBD는 `rados … stat`(8초). 지원하지 않는 OS의 NFS·lvm-shared·RBD 검사는 `unknown`. `since`는 상태가
  바뀔 때 새로, ok·unknown이면 비움.
- **자동 관리 검사는 리소스가 아니라 노드의 보고다**(설계는 "자동으로 검사가 된다"): `cluster_hosts()`마다 `cluster-link-<n>`(1부터),
  역할 `management`, 그 호스트의 IP를 가진 인터페이스의 `link`, 늘 경고. 이름은 시작 때 2초 제한으로 풀고 실패하면 다음 회차에
  다시, IP를 가진 인터페이스가 없으면 `unknown`. 리더는 `AUTO_PREFIX`로 알아본다.
- **본딩 알림 = 감사 + 기록 Task(설계의 "이벤트").** 이벤트 저장소가 없어 계획 51처럼 실행하지 않는 끝난 Task를 쓴다: 본딩 상태가
  바뀌면 그 노드의 감사 로그에 `node.bond-degraded`·`node.bond-down`·`node.bond-recovered`(새 `opl_api::audit::system_entry`, API를
  끈 노드는 적지 않음), `degraded`·`down`이 되면 기록 Task `bond-degraded { bond, ports }`(모든 노드가 `ha-checks`일 때만). 새 상태가
  세 회차(15초) 이어져야 바뀐 것으로 보고(`BondWatch`), 저하·끊김의 감사·Task는 본딩마다 10분에 한 번까지(그 사이는 로그만,
  `degraded → down`은 간격과 상관없이), 회복은 그 저하·끊김을 알렸을 때만 알린다. 시작 때 이미 `up`이 아닌 본딩도 같은 규칙으로 한 번
  알린다.
- CLI `node interfaces`·`node checks`, `node list`의 `CHECKS` 열(`-`·`ok`·`degraded`·`failing:a,b`).

**리더 판단(단계 5·6).**
- opl-api 새 `ha_health.rs`: `Health::load`(켜진 HaCheck + 리스 결과), `CheckWatch`(리더 메모리: (노드, 검사)마다 실패 시작·대응함,
  마이그레이션 중·거절·끄기 요청), 순수 `decide(…) -> Vec<Decision>`과 `react`. grace는 리더 시계로만 재고 리더를 잃거나 임기가
  바뀌면 비운다(새 리더는 grace를 처음부터 — 안전 쪽). 한 episode에 한 번 대응한다. 노드 장애를 복구하는 틱에는 판단하지 않는다.
- **과반 실패 = 공유 장비 문제:** 같은 검사를 ok·degraded·failing으로 보고한 노드 가운데 `failing × 2 > reporting`이면 모든 노드에서
  경고만(기록 Task 한 번 "failing on k of n nodes; treating it as a shared failure"). 설계의 "클러스터 노드 과반"을 "보고한 노드
  과반"으로 정했다(검사가 해당 없는 노드·옛 노드를 빼려고). 과반 실패 동안은 노드별 실패를 처리한 것으로 두지 않고, 끝나면 남은
  실패의 grace를 다시 잰다.
- 영향받는 VM(`affects`·`affected`): `storage` + `pool`은 그 풀의 디스크, `storage` + 링크·핑은 공유 풀 디스크 하나라도, `vm` + 링크는
  `bridges_behind`(인터페이스 자신·`master`를 따라간 브리지, 그 인터페이스나 위가 underlay IP를 가지면 VXLAN 브리지 모두 — 관리
  `oplv*` 브리지는 interfaces에 없어 따로 표시), `vm` + 핑은 `via`가 있으면 링크처럼, 없으면 NIC가 있는 모든 VM, `management`는
  그 노드의 HA VM(기록용). 자동 대응은 `ha_active()` VM만, 나머지는 기록 문구("also affects N VMs without HA")에만.
- `migrate`: 이름 순, 원본 노드마다 진행 중 `ha_start_concurrency`개까지, 여러 틱에 걸침. 창 안 `relocations ≥ max_relocate`면
  옮기지 않고 `ha.error`(이 검사가 동시 상한보다 먼저), 옮기면 `relocations`·`relocated_from`을 더함(되돌아가기가 곧바로 되돌리지
  않게), 거절은 VM마다 60초 뒤 다시.
- `fence`: 새 `PowerFencing::request(node, reason)`(내부 대기열, `fence_power`가 채널 요청과 함께 비움 — 임기 예산·자기 자신 규칙
  그대로, 리더 자신이면 실패 Task). 확인되면 노드 장애 복구가 모든 HA VM을 옮긴다 — **`max_relocate`에 세지 않는다**(설계는 "대응한
  VM은 `max_relocate`에 셈"이나 노드 장애 복구와 같게 둠).
- **`restart_elsewhere`(설계의 "꺼짐을 리더에 알린 뒤"를 VM status로):** 리더가 창 안 `max_relocate`를 **먼저** 확인해 닿은 VM은
  끄지 않고 `ha.error`만(꺼서 `give_up`으로 멈추게 하지 않음 — 스토리지가 돌아오면 그대로 산다), 아니면 `VmHaStatus.evict =
  HaEvict { from, check, reason, at }`. 요청 전에 `relocate_vm`과 같은 조건(그 노드 아님·`ready`·`!health.blocks`)으로 `pick`해
  받을 노드가 없으면 쓰지 않는다(VM마다 경고 한 번 — 끄면 켤 곳 없이 꺼진 채 남는다). 앞 VM과 이미 요청한 VM은 `Assign`에 넣고
  센다. 받을 노드가 없어 아무것도 요청하지 않은 틱은 기록 Task를 쓰지 않는다. 한 노드에 내는 요청 수의 상한은 없다(영향받는 HA VM
  모두 — 남은 것). 그 노드의 reconciler가 `StopMode::Kill` 뒤 파일 볼륨을 `fsync`하고(새 `Storage::sync_volume` — 파일 풀만, 5초
  `EVICT_SYNC_TIMEOUT`, 끝나지 않은 작업은 새로 띄우지 않음 — SIGKILL 뒤 이 노드 커널에 남은 NFS 쓰기(hard 마운트 재시도 포함)가
  다른 노드에서 켠 VM의 이미지를 덮지 않게; 실패하면 `relocate_from`을 쓰지 않으므로 NFS 서버에 닿지 못하면 20초 넘김의 전원 끄기로
  끝난다, `cache.direct`는 바꾸지 않음) 디스크 볼륨을 끄고(`deactivate_disks` — lvm-shared의 배타 활성을 풀어야 받는 노드가 켬, 계획 50의 재시작 옮기기와 같은 순서) phase
  `Failed`·`relocate_from = 나`·`evict` 지움(`merge_ha`가 두 필드를 작업자의 변경으로 다시 적용, 우리가 끈 것이라 크래시로 세지
  않음, 도는 QEMU를 보면 끄기 전에 볼륨을 켜진 것으로 둠 — opld 재시작 뒤 새 작업자도 끔). 끄기나 내보내기·볼륨 끄기가 실패하면
  `last_error`만 쓰고(껐으면 phase `Failed`·pid 없음) 다시(계획 50의 크래시 경로는 볼륨 끄기 실패를 경고만 하지만 여기서는
  `relocate_from`을 쓰지 않는다 — 배타 활성이 남은 채 옮기면 받는 노드가 켜지 못하므로 20초 넘김의 전원 끄기로 간다). 리더의 기존
  `relocate`가 옮긴다(`max_relocate` 셈, `relocate_vm`의 문구는 "HA check …" `last_error`를 이어 씀). 20초(`EVICT_TIMEOUT`) 안에
  거두지 않으면 이유를 다시 본다: 검사가 아직 있고 켜져 있으며 대응이 아직 `restart_elsewhere`이고 모든 노드가 기능을 알며
  `evict.from`에서 실패하고 공유 실패가 아니면 **노드마다 한 번에**(그 노드의 넘김 VM을 모아 기록 Task 하나·전원 끄기 요청 하나,
  임기 안 노드마다 한 번 — 뒤 요청은 넘긴 것으로만 표시) `NodeFence`가 있으면 `PowerFencing::request` → 노드 장애 복구, 없으면
  경고 + 실패 기록 Task, 요청은 남김. 이유가 사라졌으면(회복·공유 실패로 바뀜·검사를 지우거나 끄거나 대응을 바꿈) 리더가 `evict`를
  지운다(넘긴 요청도 틱마다 봄). `max_relocate` 때문에 남긴 `ha.error`는 그 노드의 검사 실패가 끝나면 리더가 지운다. 진행 중
  요청 목록은 틱마다 VM status에서 다시 만든다(새 리더도 이어 셈). 사용자가 끄거나 HA를 끄거나 다른 노드를 가리키는 `evict`는
  reconciler가 지운다. `evict`가 있는 VM은 되돌아가기가 건너뛰고 `start_migration`(사용자 마이그레이션)은 **409** `vm_busy`다.
- 기록 Task 문구의 전원 끄기는 요청을 넣었다는 뜻으로 "requested a power-off of the node"다(`fence`·20초 넘김).
- **대상 거르기:** `Health::blocks(vm, node)` = 그 노드가 `failing`으로 보고한 켜진 `storage`·`vm` 검사 가운데 그 VM에 영향을 주는 것이
  있음(grace와 상관없이, 과반이어도). `recover_vm`·`relocate_vm`의 `eligible`, `Failback::target`, `migrate` 대상에 더했다(미리
  고르기 `plan`에는 넣지 않음 — 쓸 때 `eligible`로 다시 확인). 보고 없는 노드·`unknown`은 막지 않는다.
- 대응 Task `TaskOp::HaCheck { check, action, vms, detail }`(target = 노드, succeeded/failed, 실행하지 않음).

**보이기(단계 7).**
- 웹 "HA 검사" 화면(`web/src/ha-checks/`, HA 그룹 다음 메뉴): 목록(이름·역할·대상·대응(역할 기본 표시)·grace·노드별 대상·켜짐),
  admin의 만들기·편집 대화상자 — 역할에 따라 대상 종류(`pool`은 storage만)·대응(허용만)을 고르고, 인터페이스는 모든 노드
  `status.interfaces` 이름의 합집합 `datalist`(직접 입력도), 풀은 공유 풀, 노드별 대상 줄(위트니스 제외, 같은 종류), 서버와 같은
  폼 검사(`checks.ts` `checkErrors`). 스펙 비교는 서버가 쓰지 않는 기본값·키 순서와 상관없이 한다. 바꾼 뒤 목록을 다시 읽는다.
- 노드 화면 "상태 검사" 열: 실패 검사 수 배지(제목에 실패·degraded 검사의 이름과 오류), 아니면 "일부 이상"·"정상". **설계에 더한 것:**
  이 배지를 누르면(읽기 권한만 있어도 — 노드 메뉴는 admin만 보이므로) "네트워크 인터페이스" 대화상자가 열리고, 대화상자 아래에 그
  노드의 검사 결과 표(상태·시작 시각·오류)도 둔다. 노드 메뉴 "네트워크 인터페이스…"(위트니스 제외)는 같은 대화상자다. 본딩은 모드와
  `degraded`·`down` 배지, 포트는 아래 줄이고 `up`이 아니면 빨간 글자와 상태 문구(aggregator id).
- 작업 목록: `ha-check`는 "node-b의 HA 검사 nas: 다른 노드에서 다시 켜기", `bond-degraded`는 "node-b의 본딩 bond0 포트 이상"이고,
  기록 Task의 설명(`detail`, 옮긴 VM, 본딩 포트 상태)을 이름 아래 작은 줄로 보인다(실패면 오류 칸과 같으므로 되풀이하지 않음).
- README 새 절 "HA 상태 검사"(검사·역할·대상·노드별 대상·자동 검사, 판정 규칙, 대응과 순서, 대상 거르기, 본딩 알림, 업그레이드 중,
  한계)와 "HA VM과 자기 펜싱"·"배치 규칙과 HA 정책"에서 이어지는 한 줄, `[ha] ping`.

**손으로 확인할 것.** 테스트 VM(UTM)에서: 본딩 포트 하나 뽑기(`degraded`·감사·기록 Task·빨간 포트), LACP 어긋남(상대 스위치 설정),
NFS 서버 멈춤(`not responding` → `restart_elsewhere`, D 상태 QEMU면 전원 끄기 펜싱), iSCSI multipath 경로 하나 끊기(`degraded`).
자동 시험은 가짜 sysfs·procfs 트리, 본딩 고정 글, 가짜 명령, `ha-it`의 가짜 carrier(이 Mac에서 끊고 다른 노드에서 켜지기까지 약
17초, grace 10초)와 Linux CI의 veth·dummy·본딩 링크 판정뿐이다.

**남은 것.**
- 위 손 확인 결과.
- 계획 53 균형 조정기가 `Health::blocks`를 대상 조건으로 쓴다.
- 리스 갱신에 결과가 실려 커진 크기로 계획 47의 규모 시험 표를 다시 잰다.
- 노드 장애를 복구하는 동안은 검사 대응을 하지 않아 `restart_elsewhere`의 20초 넘김 처리가 늦을 수 있다. 같은 막힌 디렉터리를
  가리키는 검사가 여럿이면 probe 작업도 여럿이고, lvm-shared의 `pvs`(5초 제한)가 한 회차를 늘릴 수 있다 — `pvs`를 VG마다 따로
  돌리기와 SIGKILL에도 끝나지 않는 D 상태 QEMU 따라가기는 하지 않았다.
- `restart_elsewhere`에 한 노드·클러스터의 동시 요청 상한이 없다(영향받는 HA VM 모두를 한 번에 끈다).
- `migrate` 대응은 시작한 마이그레이션을 `max_relocate`에 세므로 실패한 마이그레이션도 센다.
- 노드가 저장소의 검사 목록(`HaCheck`)을 읽지 못한 회차에는 지난 결과를 다시 보내는데, 리더는 결과가 언제 만든 것인지 모른다(결과에 시각 없음).
- 종료 때 막힌 NFS probe 작업은 기다리지 않고 남긴다(`pools.rs`의 확인과 같음).
- 공유 디렉터리 풀의 마운트가 사라지면 probe가 마운트 지점(루트 파일시스템)에 써서 통과하고, 읽기 전용 export는 늘 실패한다.
- `restart_elsewhere`로 끈 VM은 받을 노드가 사라지면 원래 노드가 회복해도 다른 노드를 기다린다(계획 50의 재시작 옮기기와 같음).
- 웹·노드 메뉴의 브리지 업링크 판정, NVMe `live`, `fsync` 내보내기의 실제 NFS 동작은 손으로 확인한다.
