# 클러스터 운영

제어부 가용성, 앞단 프록시·VIP, API 인증서(ACME), 대규모 클러스터, 다중 링크, 노드 유지보수, 클러스터 CPU 기준, 호스트 준비(예약·전용 CPU와 hugepages), 지표 기록·대시보드, 알림, Prometheus. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [제어부 가용성](#제어부-가용성)
- [앞단 프록시와 VIP (API 가용성)](#앞단-프록시와-vip-api-가용성)
- [API 인증서(ACME)](#api-인증서acme)
- [대규모 클러스터](#대규모-클러스터)
- [클러스터 통신 다중 링크](#클러스터-통신-다중-링크)
- [노드 유지보수와 비우기](#노드-유지보수와-비우기)
- [클러스터 CPU 기준](#클러스터-cpu-기준)
- [호스트 준비: 예약·전용 CPU와 hugepages](#호스트-준비-예약전용-cpu와-hugepages)
- [지표](#지표)
- [알림](#알림)
- [Prometheus](#prometheus)

## 제어부 가용성

API·작업·느린 디스크가 Raft 하트비트와 HA 리스 갱신을 늦춰 멀쩡한 노드가 펜싱되지 않게, orbivisord는 한
프로세스 안에서 일을 서로 다른 스레드에 나눈다([설계 문서](../superpowers/specs/2026-11-11-availability-scale-design.md) §3·§11).

| 실행기·스레드 | 스레드 이름 | 도는 것 |
|---|---|---|
| `core`(2개 고정) | `orbivisor-core` | Raft, 노드 간 서버(모든 연결의 TLS·HTTP/2 처리와 Raft·리스·전달 요청), 다른 노드로 가는 Raft·리스·전달 연결, HA 리스 갱신, HA 복구(리더), 노드 감시, 제어 소켓 |
| `api` | `orbivisor-api` | HTTPS API(웹 콘솔·watch 포함) |
| `work` | `orbivisor-work` | reconciler, 작업(Task) 실행기, 스케줄러, 정리, 지표, 풀, 텔레메트리, 원격 확인, 업데이트, 노드 간 서버가 받은 무거운 요청의 처리기(들어오는 마이그레이션, 콘솔 중계, UEFI 변수, 감사 로그·지표 읽기), 다른 노드로 가는 콘솔·마이그레이션·감사·지표 연결 |
| OS 스레드 | `orbivisor-watchdog` | HA watchdog 장치 쓰다듬기(1초마다, 실행기가 막혀도 리스 규칙대로) |
| OS 스레드 | `orbivisor-store` | 클러스터 저장소(redb)의 쓰기와 fsync, 차례대로 하나씩 |
| OS 스레드 | `orbivisor-notify` | 실행기 확인과 systemd 알림 |

```toml
# /etc/orbivisor/orbivisord.toml (노드마다, 생략하면 아래 기본값)
[runtime]
# api_threads = 4     # api 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
# work_threads = 4    # work 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
```

- **노드 간 연결.** 노드 간 서버의 연결마다의 TLS·HTTP/2 처리는 `core`에서 돈다. 그래서 콘솔의 VNC 바이트와
  UEFI 변수 조각을 틀에 넣고 암호화하는 일은 `core` 스레드가 하고, `work`에서는 그 요청의 처리기만 돈다. 팔로어가
  리더에게 전달하는 큰 API 쓰기도 `core`의 연결로 가므로 `core`에서 암호화된다. 다른
  노드로 가는 연결은 둘로 나눈다: Raft·리스 갱신·리더 전달·생존 확인은 `core`의 연결, 콘솔·마이그레이션 보내기·UEFI
  변수·감사 로그·지표 읽기는 `work`의 연결이다(처음 연 쪽 실행기와 관계없이). 노드 간 연결을 여러 망으로 잇는
  것은 아래 [클러스터 통신 다중 링크](#클러스터-통신-다중-링크)(계획 49)다. 대량 전송용 포트를 따로 두는 것은 아직 없다.
- **systemd watchdog.** 유닛(`orbivisord.service`)은 `Type=notify`, `NotifyAccess=main`, `WatchdogSec=30s`,
  `WatchdogSignal=SIGKILL`이다. orbivisord는 시작이 끝나면 `READY=1`을 보낸다(클러스터에 속하지 않은 노드는 제어
  소켓을 열었을 때, 멤버는 작업 실행기가 이전 프로세스의 고아를 정리하기 직전과 API가 듣기 시작했을 때, 위트니스는
  노드 간 서버가 떴을 때). 고아 정리는 정족수가 없으면 지우기마다 10초까지 기다리므로, 클러스터 전체를 다시 켤 때
  시작 제한(90초)에 걸리지 않게 그 전에 보낸다. `orbivisor-notify` 스레드가 10초(`WATCHDOG_USEC`의 1/3, 1초~10초)마다
  세 실행기에 빈 작업을 보내 `core`와 `api`가 2초(간격의 절반이 더 짧으면 그만큼) 안에 돌리면 `WATCHDOG=1`을 보낸다.
  늦으면 보내지 않고 경고("runtimes did not run a task within 2s; not petting the systemd watchdog", 늦은 실행기
  이름)를 남긴다. `work`만 늦으면 경고("…; still petting the systemd watchdog")만 남기고 계속 보낸다(데몬을 다시
  띄우는 동안 HA VM이 있는 노드가 재부팅될 수 있으므로 리스와 API가 도는 한 끄지 않는다). `core`나 `api`가 20초 넘게
  막히면 쓰다듬기가 30초 넘게 끊겨 systemd가 orbivisord를 SIGKILL로 끝내고(코어 덤프로 늦어지지 않게, 진단은 경고
  로그로) 2초 뒤 다시 띄운다(`Restart=on-failure`, `RestartSec=2`. 60초에 10번까지 — `StartLimitIntervalSec=60`·
  `StartLimitBurst=10`). `KillMode=process`라 VM은 그대로 돌고, 다시 뜬 orbivisord가 VM에
  다시 붙는다. `systemctl status orbivisord`가 `activating`에 머물면 `READY=1`이 아직 오지 않은 것이다(로그를 본다).
- **HA watchdog과의 관계.** HA watchdog 장치(노드 재시작)는 HA VM이 도는데 리스를 잃었을 때의 것이고, systemd
  watchdog(데몬 재시작)은 데몬이 멈췄을 때의 것이다. 데몬이 다시 시작되는 동안 HA VM이 있으면 장치 쓰다듬기도
  멈춘다(죽은 프로세스가 닫은 장치의 타이머는 그대로 돈다). 새 프로세스가 장치를 다시 열면(arm) Linux 드라이버가
  타이머를 다시 시작하므로, 마지막 쓰다듬기부터 다시 arm하기까지가 `watchdog_timeout_secs`(기본 `fast` 10초, `safe` 30초)보다
  짧아야 노드가 재시작되지 않는다. 그 사이에 드는 것: 죽은 프로세스 뒤의 `ExecStopPost`(`orbivisord --release-vips`, VIP 떼기),
  `RestartSec`(2초), 프로세스 시작과 클러스터 저장소 열기, 도는 VM마다
  차례로 QMP에 다시 붙기(VM 수에 비례, 붙지 못하는 VM은 하나에 5초까지), 첫 리스 갱신(리더가 있어야 한다),
  reconciler가 도는 HA VM에 붙으며 하는 arm. `fast`에서는 `RestartSec`을 빼면 약 8초가 남고 나머지가 그 안에 들어야 하므로 HA VM이
  많은 노드는 배포 뒤 이
  시간을 재 보고, 넘으면 `profile = "safe"`(또는 `watchdog_timeout_secs`만 늘림)를 둔다. systemd의 `WatchdogSec=30s`는 그대로다 —
  `core`가 `fence_after_secs`(10초)보다 오래 막히면 리스가 먼저 끝나 HA VM 펜싱(장치 제한 10초 뒤 노드 재시작)이 데몬 재시작보다
  먼저 온다.
- **Raft가 멈추면.** 저장소 쓰기가 실패하거나 저장소 스레드가 멈춰 Raft(openraft)가 멈추면 orbivisord는
  "Raft stopped: …"로 0이 아닌 코드로 끝나고 systemd가 다시 띄운다(API만 답하는 채로 남지 않는다).
- **느린 디스크.** 저장소 쓰기는 `orbivisor-store` 스레드에서 하므로 fsync가 실행기 스레드를 막지 않는다. 다만
  Raft(openraft)는 자기 로그 쓰기가 끝나기를 기다리므로, 디스크가 아주 느린 노드의 Raft 루프는 멈춘다. 그 노드가
  팔로어면 그 노드만 멈칫하고(선거를 시작할 수 있다) 다른 노드는 영향을 받지 않는다. 그 노드가 **리더**면 리더의
  Raft 루프가 멈춰 리스 갱신에 필요한 리더 확인(`confirm_leader`)이 실패하므로 **모든 노드의 리스 갱신이 실패**하고,
  다른 노드가 선거로 리더가 된 뒤에 회복한다.
- **끄기.** 장애를 조사하느라 systemd watchdog을 잠시 끄려면 `systemctl edit orbivisord`로 `[Service]`에 `WatchdogSec=0`을
  둔다.
- **시험.** `cargo test -p orbivisord --features starve-it --test starve`가 orbivisord 셋의 리더의 `api` 실행기를 15초 동안 막고
  그동안 리더·임기가 그대로이고 모든 노드의 리스 나이가 3초 미만이며 `WATCHDOG=1`이 멈추는지 본다. `starve-it`
  기능은 시험 빌드에만 있는 고리(`[test] block_api_ms`, 제어 소켓의 `/v1/test/…`)를 넣으므로 배포 빌드에 켜지 않는다.

## 앞단 프록시와 VIP (API 가용성)

모든 노드가 같은 API를 연다: 읽기는 받은 노드의 복제본에서, 쓰기는 리더로 전달된다. 그래서 브라우저·CLI·자동화가
한 노드 주소 대신 **로드 밸런서(HAProxy 등)나 VIP**를 부르게 하면 노드 하나가 죽어도 API가 이어진다. 이 절의 기능은
그 앞단을 위한 것이다([설계 문서](../superpowers/specs/2026-11-11-availability-scale-design.md) §4·§13).

### 헬스 체크

`GET /api/v1/health`는 인증 없이 부른다. 기본(`?ready`)은 이 노드에 요청을 보내도 되는지 보고 200이나 503(같은 모양,
실패한 확인은 `failed`)으로 답한다. `?live`는 프로세스가 답하는지만 보고 늘 200이다(쿠버네티스의 liveness). 로그인하지
않은 요청에는 `status`·`failed`만 답하고, 로그인(세션·API 토큰)하면 노드 id·리더 같은 자세한 상태도 답한다.

```bash
curl -s --cacert ca.pem https://node1:8006/api/v1/health
# {"status":"ok","failed":[]}
curl -s --cacert ca.pem -H "Authorization: Bearer $TOKEN" https://node1:8006/api/v1/health
# {"status":"ok","node":"…","leader":"…","quorum":true,"apply_lag_ms":0,"lease":"none",
#  "runtimes":"ok","maintenance":false,"api":true,"failed":[]}
curl -s --cacert ca.pem 'https://node1:8006/api/v1/health?live'
```

| `failed` 항목 | 뜻 |
|---|---|
| `quorum` | 리더를 모르거나, 리더인데 과반수가 5초 넘게 답하지 않았다 |
| `apply_lag` | 이 노드가 받은 로그를 5초 넘게 적용하지 못했다(어림: 리더보다 뒤처져 아직 받지 못한 로그는 보지 못한다) |
| `lease` | HA 펜싱(`[ha] fencing`)을 쓰는데 리스가 fresh가 아니다 |
| `runtimes` | 마지막 실행기 확인(10초마다)에서 `core`나 `api`가 늦었다("제어부 가용성"). 한 번 늦으면 다음 확인까지 10초 동안 503이다 |
| `maintenance` | 노드가 유지보수(비우기) 중이다 |
| `role` | 노드가 `api` 역할이 아니다 |

### API 노드

앞단이 부를 노드를 노드 역할 `api`로 정한다. 기본은 위트니스가 아닌 모든 노드다. 노드가 많으면 장애 영역마다
고르게 3~5대만 두고 나머지는 `none`으로 둔다(그 노드의 `?ready`가 503이 되어 앞단이 저절로 뺀다). 역할을 바꾸려면
모든 노드가 이 판 이상이어야 한다.

```bash
orbivisorctl node roles node4 none      # "node node4: roles none"
orbivisorctl node roles node4 api
orbivisorctl node roles node4 default   # 기본으로(위트니스가 아니면 api)
orbivisorctl node list                  # ROLES 열
orbivisorctl cluster api-endpoints      # NAME ADDRESS ZONE(노드 레이블 zone, 없으면 -)
orbivisorctl cluster api-endpoints -o haproxy
# server node1 10.0.0.11:8006 check ssl verify required ca-file /etc/haproxy/orbivisor-ca.pem
# server node2 10.0.0.12:8006 check ssl verify required ca-file /etc/haproxy/orbivisor-ca.pem
orbivisorctl cluster ca > /etc/haproxy/orbivisor-ca.pem   # 클러스터 CA(PEM). 프록시가 노드 인증서를 확인한다
```

노드를 더하거나 빼면 `-o haproxy`로 다시 만들어 넣는다(또는 HAProxy 런타임 API·DNS `server-template`). API는
`GET /api/v1/cluster/api-endpoints`(로그인 필요)다.

### 세션 공유

웹 로그인 세션은 Raft(클러스터 저장소)에 있어 앞단이 어느 노드로 보내도 이어진다. 고정 연결(sticky)이 필요 없고, 노드가
죽어도 다시 로그인하지 않는다. 저장하는 것은 쿠키 값의 SHA-256뿐이다. 로그아웃·만료·비밀번호 변경·사용자 비활성화는
모든 노드에 곧바로 반영된다. 정족수가 없을 때(장애 중)는 로컬 사용자 로그인이 그 노드에서만 쓰는 세션으로 된다(경고 로그).
한 사용자의 세션은 32개까지이고, 넘으면 가장 오래된 세션이 끝난다. 그 노드에서만 쓰는 세션(정족수가 없을 때나 옛 노드가
남았을 때 만든 것)을 다른 노드에서 로그아웃하면 Raft에 로그아웃 표시를 남겨 그 노드에서도 끝난다(표시는 12시간 뒤 지운다).
로그아웃은 인증 없이 부르므로 표시는 2,000개까지이고, 표시를 쓰는 로그아웃(어디서도 찾지 못한 쿠키)은 로그인 실패처럼
IP마다 센다. 막힌 IP의 로그아웃은 표시 없이 쿠키만 지운다(응답은 그대로 204).

알려진 한계:
- 정족수가 없는 소수 쪽 노드는 그 노드가 가진 옛 사용자 정보로 로그인을 받는다. 다른 쪽에서 비밀번호를 바꿨거나 사용자를
  비활성화했어도 그 노드가 따라잡을 때까지는 옛 비밀번호로 로그인된다(그 세션은 그 노드에서만).
- 로그인 시도 제한([로그인과 세션](access.md#로그인과-세션))은 노드마다 따로 센다. 앞단이 요청을 여러 노드로 나누면 노드 수만큼 더
  시도할 수 있다.

### 믿는 프록시와 허용 출처

프록시 뒤에서는 연결 주소가 프록시라서 감사 로그와 로그인 시도 제한이 프록시 주소를 본다. 믿는 프록시를 적으면 그
주소에서 온 요청의 `X-Forwarded-For`(클라이언트 주소, 오른쪽부터 믿지 않는 첫 주소)와 `X-Forwarded-Host`·
`X-Forwarded-Proto`(같은 출처 검사, OIDC 콜백 주소)를 믿는다. 적지 않으면 아무 헤더도 믿지 않는다.

```toml
# /etc/orbivisor/orbivisord.toml (API 노드마다)
[api]
trusted_proxies = ["10.0.0.0/24", "192.0.2.10"]
```

웹 포털을 API와 **다른 호스트 이름**에 두면 그 출처를 허용 출처로 적는다(관리자). 그 출처의 쿠키 요청에 CORS로 답하고,
그 출처에서 로그인하면 세션 쿠키가 `SameSite=None; Secure`다. 비우면 지금처럼 같은 출처만이다. 비우지 않으려면 모든
노드가 이 판 이상이어야 한다. 허용 출처는 `https://`만 된다(개발용 `http://localhost`·`127.0.0.1`·`[::1]`은 예외).
허용 출처를 쓰면 쿠키가 다른 사이트의 요청에도 붙을 수 있으므로, 쿠키로 인증한 쓰기 요청(GET·HEAD가 아님)과 WebSocket이
`Origin` 없이 `Sec-Fetch-Site`로 다른 사이트(`same-origin`·`same-site`·`none`이 아님)라고 알리면 받지 않는다. 그 헤더를
보내는 브라우저는 쓰기에 `Origin`도 보내므로, 둘 다 없는 브라우저 밖의 클라이언트(`orbivisorctl login`, curl)는 그대로
된다. 로그아웃도 같은 출처나 허용 출처에서만 받는다.

```bash
orbivisorctl settings set web_origins=https://portal.example.com
orbivisorctl settings set web_origins=          # 같은 출처만
```

다른 호스트의 포털에서 OIDC로 로그인하면 로그인이 끝난 뒤 API 호스트의 `/`로 돌아간다(포털로 돌아가지 않는다). 포털과
API를 같은 호스트 이름에 두면(아래 쿠버네티스 예) 이 문제가 없다.

### API 이름

앞단·VIP의 이름(DNS 이름·IP)을 API 인증서에 더한다(관리자). 노드마다 클러스터 CA로 API 인증서를 다시 서명해 끊김 없이
바꿔 끼운다(노드 간 mTLS 인증서는 그대로). API 인증서는 서버 인증만 되고 CN이 `orbivisor-api`라 키가 새어도 노드 간
mTLS에 쓸 수 없다. **내장 VIP 주소도 여기에 넣어야** 클러스터 CA를 믿는 클라이언트가 VIP로 TLS 확인을 통과한다.

```bash
orbivisorctl settings set api_names=orbivisor.example.com,192.168.0.200
orbivisorctl settings set api_names=            # 노드 인증서로 돌아감
```

`api_names`는 HAProxy·keepalived·내부 클라이언트(`orbivisorctl` 등)가 클러스터 CA를 핀으로 쓸 때를 위한 것이다.
**클러스터 CA를 브라우저나 OS 신뢰 저장소에 넣지 않는다.** 모든 노드가 그 CA 키를 갖고 있고 이름 제한이 없어, 노드 하나가
뚫리면 아무 사이트의 인증서나 만들 수 있다. 브라우저용으로는 HAProxy에서 공인 인증서로 TLS를 끝내거나(아래 예), 노드에
관리자 인증서(공인·사내 CA)를 둔다.

```toml
# /etc/orbivisor/orbivisord.toml (API 노드마다)
[api]
tls_cert = "/etc/orbivisor/api.crt"   # PEM, 중간 인증서를 이어 붙여도 된다
tls_key = "/etc/orbivisor/api.key"    # 둘 다 있어야 한다
```

관리자 인증서가 있으면 API 리스너가 그것을 쓰고 `api_names`로 다시 발급하지 않으며 ACME 인증서도 끼우지 않는다(파일을 읽지 못하면 시작하지 못한다). 공인 인증서를 자동으로 받으려면 아래 [API 인증서(ACME)](#api-인증서acme)를 쓴다.
파일은 시작할 때만 읽는다. 인증서를 갱신하면(certbot 등) `systemctl restart orbivisord`로 다시 읽게 한다(VM은 그대로 돈다,
갱신 훅 예: certbot `--deploy-hook "systemctl restart orbivisord"`).

`orbivisorctl`은 처음 로그인할 때 클러스터 CA를 받아 핀으로 둔다. 관리자 인증서를 둔 노드(또는 공인 인증서로 끝내는
프록시)에는 `--system-ca`로 로그인한다. 그러면 클러스터 CA 대신 OS 신뢰 저장소로 서버를 확인하고, 그 방식을 설정 파일에
저장해 다음 명령과 같은 서버에 다시 로그인할 때도 쓴다(`--ca-fingerprint`로 로그인하면 클러스터 CA로 돌아간다).

```bash
orbivisorctl login --server https://orbivisor.example.com --system-ca
```

### 내장 VIP (Linux)

로드 밸런서 없이 VIP 하나(또는 여럿)를 API 노드 사이로 옮겨 다니게 한다. 같은 L2 서브넷 안에서만 된다.

```toml
# /etc/orbivisor/orbivisord.toml (VIP를 가질 API 노드마다 같게)
[vip]
addresses = ["192.168.0.200/24"]   # 여럿이면 VIP마다 다른 노드가 갖는다(DNS에 모두 적어 짐을 나눈다)
interface = "vmbr0"                # VIP를 붙일 인터페이스
```

- 리더가 VIP마다 주인을 정해 클러스터 저장소에 적는다(`?ready`가 200인 `api` 노드 가운데, VIP가 가장 적은 노드).
  주인은 1초마다 리더에게 VIP 리스(3초)를 갱신하고 `ip addr add`와 gratuitous ARP 세 번(`arping -U -c 3`, 패키지
  `iputils-arping`, 없으면 경고만 하고 이웃의 ARP 캐시가 늦게 바뀐다)으로 알린다. VIP를 떼면 아직 끝나지 않은 알림도
  멈춘다.
- 주인은 5초마다 쥔 VIP를 다시 붙여 본다. `ifreload`·브리지 다시 시작·networkd 다시 읽기 등으로 인터페이스에서 지워졌으면
  다시 붙이고(경고 로그) 다시 알린다.
- 주인이 준비되지 않으면(헬스 503) 곧바로 떼고, 리더와 끊기면 리스가 끝나기 0.5초 전에 스스로 뗀다(멈췄다 늦게 깨면
  갱신하기 전에 먼저 뗀다). 리더는 리스가 끝난 뒤에야(리더 시계) 다른 노드에 주므로 **orbivisord가 도는 동안에는** 두
  노드가 같은 VIP를 갖지 않는다(겹칠 수 있는 경우는 아래 알려진 한계). 주인이 죽으면 약 3~4초 뒤 옮겨 간다.
- 주인이 살아 있으면 리더가 바뀌거나 노드가 더해져도 옮기지 않는다. 새 리더는 리더가 된 뒤 3초 동안 옮기지 않는다.
- 붙인 VIP는 `/var/lib/orbivisor/vips`(`data_dir`)에 적어 둔다. `systemctl stop`은 VIP를 먼저 뗀다. orbivisord가 kill -9·
  watchdog SIGKILL 등으로 죽으면 VIP가 인터페이스에 남고, systemd의 `ExecStopPost=/usr/bin/orbivisord --config
  /etc/orbivisor/orbivisord.toml --release-vips`가 기록한 VIP를 뗀다(다시 시작할 때도 리스를 받기 전에 지금 설정과 관계없이 뗀다).
  `--release-vips`는 데이터 디렉터리 잠금을 쥔 orbivisord가 돌고 있으면 아무것도 떼지 않는다. 인터페이스가 없어져 뗄 수
  없는 기록은 경고하고 지운다.
- 모든 노드(위트니스 포함)가 이 판 이상이어야 VIP를 쓴다(옛 노드는 VIP 주인 기록을 읽지 못한다). 업그레이드 중에는
  경고만 하고 VIP를 붙이지 않는다.
- IPv4만 된다. macOS·위트니스는 `[vip]`을 쓸 수 없다(시작할 때 설정 오류). VIP 주소를 `api_names`에 넣는다.
- keepalived를 쓰면 같은 주소를 `[vip]`에 두지 않는다.

알려진 한계(내장 VIP):
- 두 노드가 같은 VIP를 잠깐 함께 가질 수 있다: orbivisord가 kill -9·SIGKILL로 죽은 뒤 `ExecStopPost`(또는 재시작)가
  떼기 전까지, 프로세스나 VM(노드가 VM이면)이 멈춘 뒤 watchdog이 SIGKILL하기까지, 호스트가 절전한 동안(리스 마감을 재는
  단조 시계가 절전 중 멈춘다). `[vip] addresses`에서 뺀 주소는 기록(`vips`)으로 뗀다.
- 헬스 체크의 `apply_lag`는 어림이라 리더보다 뒤처진 노드를 잡지 못한다. 실행기 확인이 한 번 늦으면 헬스가 10초 동안
  503이 되어 VIP가 다른 노드로 옮겨 갈 수 있다.
- VIP 리스 갱신마다 리더가 과반수의 리더인지 확인하므로(`confirm_leader`) 노드 수의 제곱만큼 메시지가 든다(계획 47의 리스
  확인 묶기에서 함께 줄인다).
- 설정에서 VIP를 빼도 클러스터 저장소의 그 VIP 주인 기록(`VipHolder`)은 남는다(해롭지 않다).

### 내장 웹 UI 끄기

웹 UI를 따로 둔 포털(nginx)이 주면 노드는 API만 연다.

```toml
[web]
enabled = false     # 기본 true. 끄면 /api/ 밖의 경로는 404
```

웹 정적 파일 묶음과 포털 컨테이너 예는 `packaging/portal/`에 있다.

```bash
V=0.1.0   # 노드 패키지와 같은 버전
pnpm -C web build
packaging/portal/build-web.sh "$V" packaging/portal        # packaging/portal/orbivisor-web-$V.tar.gz
docker build --build-arg VERSION="$V" -t "orbivisor-portal:$V" packaging/portal   # nginx(비root), 8080
```

### 예: HAProxy

TLS를 HAProxy에서 끝내고 노드로 다시 암호화해 보낸다. 노드에는 `[api] trusted_proxies`에 HAProxy 주소를 적고, 앞단의
이름은 `api_names`에 넣는다(HAProxy가 노드 인증서의 이름을 확인하지 않으면 없어도 된다).

```haproxy
# /etc/haproxy/haproxy.cfg
defaults
    mode http
    timeout connect 5s
    timeout client  60s
    timeout server  60s
    timeout tunnel  1h          # 콘솔(WebSocket)·watch 스트림
frontend orbivisor
    bind :443 ssl crt /etc/haproxy/orbivisor.example.com.pem
    option forwardfor
    http-request set-header X-Forwarded-Proto https
    http-request set-header X-Forwarded-Host %[req.hdr(host)]
    default_backend orbivisor_api
backend orbivisor_api
    balance leastconn
    option httpchk GET /api/v1/health
    http-check expect status 200
    default-server inter 2s fall 2 rise 2
    # orbivisorctl cluster api-endpoints -o haproxy
    server node1 10.0.0.11:8006 check ssl verify required ca-file /etc/haproxy/orbivisor-ca.pem
    server node2 10.0.0.12:8006 check ssl verify required ca-file /etc/haproxy/orbivisor-ca.pem
    server node3 10.0.0.13:8006 check ssl verify required ca-file /etc/haproxy/orbivisor-ca.pem
```

### 예: keepalived

내장 VIP 대신 keepalived로 VIP를 둘 때. 헬스 체크가 실패한 노드는 우선순위를 잃어 VIP를 넘긴다.

```conf
# /etc/keepalived/keepalived.conf (API 노드마다, priority만 다르게)
vrrp_script orbivisor_ready {
    # 노드 인증서에는 127.0.0.1도 들어 있다. listen을 바꿨으면 그 주소로 부른다.
    script "/usr/bin/curl -sf --max-time 2 --cacert /var/lib/orbivisor/pki/ca.crt https://127.0.0.1:8006/api/v1/health"
    interval 2
    fall 2
    rise 2
    weight -50
}
vrrp_instance orbivisor {
    interface vmbr0
    virtual_router_id 46
    priority 100
    advert_int 1
    virtual_ipaddress {
        192.168.0.200/24
    }
    track_script {
        orbivisor_ready
    }
}
```

### 예: 쿠버네티스(포털 nginx + HAProxy)

포털(nginx) pod가 웹 UI를 주고, 같은 호스트 이름의 `/`는 포털, `/api/`는 노드로 보낸다(같은 출처라 `web_origins`가
필요 없다). 노드에는 `[web] enabled = false`와 HAProxy pod 주소 범위의 `trusted_proxies`를 둔다.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata: {name: orbivisor-portal}
spec:
  replicas: 2
  selector: {matchLabels: {app: orbivisor-portal}}
  template:
    metadata: {labels: {app: orbivisor-portal}}
    spec:
      containers:
        - name: nginx
          image: registry.example.com/orbivisor-portal:0.1.0   # packaging/portal/Dockerfile
          ports: [{containerPort: 8080}]
          readinessProbe: {httpGet: {path: /, port: 8080}}
---
apiVersion: v1
kind: Service
metadata: {name: orbivisor-portal}
spec:
  selector: {app: orbivisor-portal}
  ports: [{port: 8080, targetPort: 8080}]
```

```haproxy
# HAProxy(쿠버네티스 안의 Deployment, ConfigMap) — 위 HAProxy 예의 frontend에 경로 나눔만 더한다
frontend orbivisor
    bind :443 ssl crt /etc/haproxy/tls/orbivisor.example.com.pem
    option forwardfor
    http-request set-header X-Forwarded-Proto https
    use_backend orbivisor_api if { path_beg /api/ }
    default_backend orbivisor_portal
backend orbivisor_portal
    server portal orbivisor-portal.default.svc.cluster.local:8080 check
# backend orbivisor_api는 위 HAProxy 예와 같다(노드 주소는 클러스터 밖의 노드 IP)
```

## API 인증서(ACME)

공개 API·웹 리스너(8006)의 인증서를 Let's Encrypt·ZeroSSL·Google Trust Services·사내 ACME 서버(step-ca 등)에서 받는다(계획 73,
[설계 문서](../superpowers/specs/2026-11-25-declarative-design.md) §11). 주문은 리더가 Raft로 조율하고(인증서 하나에 주문 하나),
노드는 받은 인증서를 다시 시작 없이 끼운다(열린 연결은 그대로, 새 연결부터). 모든 노드가 기능 `acme`를 알아야 한다(409
`cluster_upgrading`). 노드 사이 mTLS(8007) 인증서는 건드리지 않는다.

**인증서 차례.** 노드는 (1) `[api] tls_cert` 파일이 있으면 그것만 쓴다(ACME·`api_names`를 보지 않는다 — 위 [API 이름](#api-이름)),
(2) 아니면 살아 있는 ACME 인증서(`install: api`)를 끼운다 — 이 노드의 노드마다 인증서가 기본이고, 모든 ACME 인증서를 TLS SNI 이름
(정확히 같거나 `*.` 한 레이블)으로 골라 낸다. 그래서 노드 이름 인증서와 VIP 이름 공유 인증서를 함께 낼 수 있다. (3) 끼울 것이 없으면
내부 CA가 서명한 인증서(`api_names`)로 돌아간다. 공인 인증서로 로그인할 때는 `orbivisorctl login --system-ca`다.

### 계정

```bash
orbivisorctl cert account add le --directory letsencrypt --email ops@example.com --agree-tos   # 곧 CA에 등록한다
orbivisorctl cert account add zs --directory zerossl --email ops@example.com --agree-tos \
  --eab-kid KID --eab-hmac-file eab-hmac.txt        # EAB: 파일(base64url, `-`면 ORBIVISOR_ACME_EAB_HMAC)
orbivisorctl cert account add corp --directory https://ca.corp.example/acme/acme/directory --agree-tos \
  --ca-bundle corp-root.pem                         # 사내 ACME: 그 루트만 믿는다
orbivisorctl cert account list
orbivisorctl cert account get le                    # 계정 URL·약관·CA가 주는 프로필
orbivisorctl cert account check le                  # CA에 다시 묻는다
orbivisorctl cert account delete le                 # 쓰는 인증서가 있으면 409 account_in_use
```

- 디렉터리 별명은 `letsencrypt`·`letsencrypt-staging`·`zerossl`·`google`, 그 밖은 `https://` URL이다. 계정 키(ES256)는 서버가 만들어
  Raft에 비밀로 둔다(보기·watch·감사·설정 기록에서 뺀다). 나중에 바꿀 수 있는 것은 `email`·`ca_bundle`뿐이다(디렉터리·EAB는 새 계정).
- 등록 오류: CA가 EAB를 요구하는데 없으면 400 `acme_rejected`, 닿지 않으면 502 `acme_unreachable`, 속도 한도 429.

### 인증서

```bash
orbivisorctl cert add nodes --account le --name '{node}.pve.example.com'        # 노드마다(기본), HTTP-01(기본)
orbivisorctl cert add vip --account le --name api.example.com --shared --dns01 cf   # VIP 이름: 한 번 받아 모든 노드가 낸다
orbivisorctl cert add wild --account le --name '*.example.com' --name example.com --shared --dns01 cf --profile shortlived
orbivisorctl cert list
# NAME   SCOPE     CHALLENGE    NODE        SERIAL  EXPIRES           RENEW             ERROR
# nodes  per-node  http-01      0193a7c2-…  04a1…   2027-02-23 10:00  2027-01-24 08:12
# vip    shared    dns-01 cf    shared      03ff…   2027-02-23 10:02  2027-01-24 11:40
orbivisorctl cert renew vip --wait                  # 곧 주문한다(물러서기를 지움, 그 인증서·노드의 주문이 돌고 있으면 409 acme_order_running)
orbivisorctl cert get vip                           # 가린 JSON(노드마다 발급·다음 갱신·실패 수·오류)
orbivisorctl cert delete vip
```

- **칸.** `--name`(여러 번), `--per-node`(기본)·`--shared`, `--node ID`(노드마다 받을 노드 id, 여러 번 — 없으면 모든 노드),
  `--http01`(기본)·`--dns01 DNS`, `--key p256|p384`(기본 ECDSA P-256), `--profile`, `--days-before N`, `--preferred-chain CN`(고를
  사슬의 루트 CN), `--no-install`(받기만 — 리스너에 끼우지 않음), `--no-precheck`. 리더가 다음 틱(60초)에 주문한다.
- **노드마다와 공유.** 노드마다 인증서는 그 노드에서 주문 Task가 돌고 키가 **그 노드 디스크에만** 있다(`{data_dir}/pki/acme/{인증서
  id}.{일련번호}.key`, 0600 — Raft에 없다. 새 키는 새 사슬을 status에 적은 뒤에야 옛 키를 지우므로, 적기가 실패해도 끼운 인증서는
  그대로다). 이름 자리 `{node}`(노드 이름)·`{node_address}`(노드 주소)는 노드마다 인증서에서만 쓴다. 꺼진 노드는
  건너뛰고 돌아오면 잇는다. **공유** 인증서는 리더(위트니스면 id가 가장 작은 온라인 노드)가 한 번 받고, 키는 Raft status의 비밀이라
  (보기·watch·감사·기록에서 뺀다) 모든 노드가 읽어 끼운다. 같은 이름 묶음을 노드 수만큼 주문하지 않으므로 CA 속도 한도(Let's Encrypt는
  같은 이름 묶음 주에 5장)에 걸리지 않는다 — VIP·로드밸런서 이름은 공유로 받는다.
- **키.** ECDSA P-256·P-384만 된다. `rsa2048`은 400이다(이 빌드의 rcgen은 ring 백엔드라 RSA 키를 만들지 못한다).
- **와일드카드**(`*.`)는 DNS-01만이다.
- **프로필**(`--profile`)은 디렉터리의 `meta.profiles`에 있을 때만 보낸다(Let's Encrypt `classic`·`tlsserver`·`shortlived`).
- **갱신.** 기본은 CA가 알려 주는 갱신 창(ARI, RFC 9773) 안의 무작위 시각이고(설정 `acme.renew_jitter`, 기본 켬), 리더가 하루 한 번 창을
  다시 묻는다. 창이 없으면 유효 기간의 1/3이 남았을 때, `--days-before N`이면 만료 N일 전이다(N일이 받은 인증서의 수명 이상이면 — 예: 6일짜리
  `shortlived`에 7 — 받자마자 다시 주문하지 않게 1/3 규칙으로 하고 그 인증서의 `error`에 알린다). 갱신 주문은 옛 인증서를 `replaces`로
  알린다. 리더 `AcmeRenewer`가 60초마다 할 일(받은 것 없음·이름 바뀜·키 종류 바뀜·갱신 시각 지남)을 보고 Task `acme-order`를 만든다
  (클러스터 전체에 동시 2개). 실패하면 5분 × 2^(n−1)(6시간까지, 속도 한도면 `Retry-After`) 물러서고, 받은 인증서가 살아 있으면 계속 쓴다.
  노드마다 인증서는 실패·물러서기를 **노드마다** 센다(status `node_failures` — 한 노드가 실패해도 다른 노드의 주문은 막히지 않고, 다른
  노드가 성공해도 그 실패는 남는다). `failures`·`error`는 가장 많이 실패한 노드의 요약(`노드 이름: …`)이다. CA가 ARI를 주지 않으면
  하루 한 번만 다시 묻는다.
- 지운 인증서의 노드 키 파일은 각 노드가 한 시간 뒤에 지운다. 키와 사슬이 맞지 않는 인증서는 경고하고 끼우지 않는다.

### HTTP-01

```toml
# /etc/orbivisor/orbivisord.toml (HTTP-01로 받을 노드마다 — 공유 인증서면 모든 노드)
[api]
http_listen = "0.0.0.0:80"     # listen·cluster_listen과 다른 포트
```

- 이 리스너는 `GET /.well-known/acme-challenge/{token}`에만 Raft에 적힌 도전 값으로 답한다 — 그래서 이름이 VIP나 DNS 라운드 로빈으로
  **어느 노드를 가리켜도** 답한다. 그 밖의 요청은 설정 `acme.http_redirect`(기본 켬)면 `308 https://{Host}:{listen 포트}{경로}`, 끄면
  404다. 본문을 읽지 않고, 머리 8 KiB·연결당 요청 하나·10초·동시 256이다. 초당 100 한도는 도전 경로가 아닌 요청에만 건다(누가
  포트 80을 두드려도 CA의 검증 요청은 503을 받지 않는다). 경로에 보이는 ASCII가 아닌 글자가 있으면 400이다. 밖에서 포트 80이 닿아야 한다(방화벽).
- `http_listen`을 켠 노드는 노드 기능에 `http01`을 알린다. HTTP-01 인증서를 만들거나 바꿀 때 대상 노드(공유면 모든 노드)가
  `http01`을 알리지 않으면 400 `http01_unavailable`(노드 이름)이다.
- **스스로 확인.** 주문 전에 노드가 `http://{이름}/.well-known/acme-challenge/{token}`을 스스로 불러 보고, 안 되면 CA에 묻지 않고 실패한다
  (CA의 실패 검증 속도 한도를 아낌 — 이름이 아직 답을 적용하지 않은 다른 노드를 가리킬 수 있어 5초쯤 다시 해 본다). 밖에서만 닿는
  이름이면 `--no-precheck`.
- `acme.http_redirect`·`acme.renew_jitter`는 `orbivisorctl settings set`에 아직 키가 없어 `PATCH /api/v1/settings {"acme":
  {"http_redirect": false}}`나 `Settings` 매니페스트([선언형 운영](declarative.md))로 바꾼다.

### DNS-01

와일드카드와 포트 80이 밖에서 닿지 않는 클러스터는 DNS-01을 쓴다. TXT `_acme-challenge.{이름}`을 공급자에 쓰고 지운다.

```bash
orbivisorctl cert dns add cf --provider cloudflare --secret-file cf-token.txt --zones example.com   # API 토큰(Zone.DNS 편집)
orbivisorctl cert dns add r53 --provider route53 --access-key-id AKIA… --secret-file aws-secret.txt \
  --hosted-zone-id Z123 --region us-east-1
orbivisorctl cert dns add ns1 --provider rfc2136 --server 192.0.2.53:53 --key-name acme-key \
  --algorithm hmac-sha256 --secret-file tsig.b64 --zone example.com                 # DNS UPDATE + TSIG(TCP 53)
orbivisorctl cert dns add pdns --provider pdns --url https://pdns.example.com:8081 --secret-file pdns-key.txt
orbivisorctl cert dns add lego --provider hook --path /etc/orbivisor/acme-hooks/nsupdate.sh
orbivisorctl cert dns list
orbivisorctl cert dns delete cf                     # 쓰는 인증서가 있으면 409 dns_in_use
```

- 비밀(Cloudflare 토큰·Route 53 비밀 키·TSIG 비밀(base64)·PowerDNS API 키)은 인자로 받지 않는다: `--secret-file` > 환경 변수
  `ORBIVISOR_ACME_DNS_SECRET` > 터미널에서 묻기. 저장한 뒤 다시 보이지 않는다. Cloudflare `--zone-id`·Route 53 `--hosted-zone-id`·RFC 2136
  `--zone`은 없으면 이름으로 찾는다. PowerDNS `--server-id` 기본은 `localhost`. API 꼴의 공급자 이름은 `cloudflare`·`route53`·
  `rfc2136`·`power_dns`·`hook`이다.
- **전파 확인.** 도전을 알리기 전에 영역의 권한 서버(NS)들에 직접 TXT를 묻고 모두 보일 때까지 기다린다(기본 180초 —
  `--propagation-timeout`, 끄려면 `--no-propagation-check`). 사내·분리 DNS면 노드 설정으로 물을 서버를 정한다:

  ```toml
  # /etc/orbivisor/orbivisord.toml
  [acme]
  dns_resolver = "10.0.0.53:53"   # 이 서버만 권한 서버로 보고 묻는다(없으면 SOA → NS → A를 찾아 모두)
  ```

- **위임.** `_acme-challenge.web.example.com CNAME web.acme.example.net`이 있으면 CNAME을 따라가 그 이름에 쓴다(쓰기 권한을 좁힌
  영역에 맡김). `--zones`가 있으면 따라간 이름도 그 영역 안이어야 한다(아니면 주문 실패 — 다른 영역의 TXT를 건드리지 않게).
  와일드카드와 기본 이름처럼 같은 TXT 이름의 값 둘은 한 번에 넣는다. Route 53·PowerDNS는 지금 TXT rrset을 읽어 **우리 값만** 더하고
  빼므로(다 비면 rrset을 지움) 같은 이름의 다른 TXT(다른 주문·다른 ACME 클라이언트·SPF)는 남는다. Route 53은 읽은 rrset을 그대로
  DELETE하고 고친 것을 CREATE하는 한 묶음이라 그 사이 바뀌면 거절되어 다시 읽는다. PowerDNS API에는 조건부 쓰기가 없어 읽기와 쓰기
  사이에 다른 쪽이 쓴 값은 잃을 수 있다. 그래서 리더는 같은 DNS 공급자의 같은 TXT 이름을 쓰는 주문을 한 번에 하나만 만든다(노드마다
  인증서에 모든 노드가 같은 이름을 넣은 경우). `cert renew`는 노드마다 Task를 한꺼번에 만들므로 이 경우 한 노드의 검증이 실패할 수
  있다 — 5분 물러서기 뒤 리더가 하나씩 다시 한다. 클러스터 밖의 다른 ACME 클라이언트와의 겹침은 막지 못한다.
- **훅.** `{path} add|remove {fqdn} {value}`(예: `add _acme-challenge.example.com Abc…`)를 **주문을 도는 노드에서** 부른다(노드마다
  인증서는 각 노드, 공유는 고른 노드 — 그러니 모든 노드에 같은 파일을 둔다). 경로는 `/etc/orbivisor/acme-hooks/` 아래 파일만(`..` 없음 —
  만들 때 검사), 주인은 root(또는 orbivisord를 돌리는 사용자), 그룹·다른 사용자가 쓸 수 없고 실행할 수 있어야 한다(부를 때마다 검사).
  환경은 비우고 `PATH`만 주며 60초 한도, 표준 출력·오류는 4 KiB까지만 이 노드 로그(debug)에 남는다. 실패하면 끝 상태와 표준 오류
  **첫 줄**(200자까지)이 인증서 status `error`에 실린다 — `cluster.read` 사용자와 경보 알림이 보므로 첫 줄에 비밀을 찍지 않는다. 그래서
  자격 증명은 파일에서 읽는다. acme.sh·
  lego의 많은 공급자를 이 길로 잇는다. 예(BIND `nsupdate`):

  ```sh
  #!/bin/sh
  # /etc/orbivisor/acme-hooks/nsupdate.sh — root:root 0755, 모든 노드에. 키 파일은 root 0600
  set -eu
  action=$1 fqdn=$2 value=$3
  case "$action" in
    add)    line="update add $fqdn. 60 TXT \"$value\"" ;;
    remove) line="update delete $fqdn. TXT \"$value\"" ;;
    *)      exit 2 ;;
  esac
  nsupdate -k /etc/orbivisor/acme-hooks/tsig.key <<EOT
  server 192.0.2.53
  $line
  send
  EOT
  ```

### 만료 경보

경보 조건 둘이 있다(계획 60 [경보 규칙](#경보-규칙)과 같은 평가·보내기).

| 조건 | 무엇 | 누가 평가 | 대상 |
|---|---|---|---|
| `certificate_expiring {days: 14}` | 이 노드 리스너가 낼 수 있는 인증서(기본 + SNI — 출처가 무엇이든)와 **내부 노드 인증서**(824일, 자동 갱신 없음) 가운데 가장 이른 만료가 `days`일 안(값은 남은 날) | 그 노드 | 노드 |
| `certificate_renew_failed {failures: 3}` | ACME 인증서의 연속 실패가 `failures`번 이상(노드마다 인증서는 노드마다 세고 알림에 그 노드들을 적는다) | **리더** | 인증서 |

`orbivisorctl alert rule create`와 웹 규칙 폼에는 아직 없다. API나 `AlertRule` 매니페스트로 만든다:

```bash
curl --cacert cluster-ca.pem -H "Authorization: Bearer $ORBIVISOR_TOKEN" -H 'Content-Type: application/json' \
  https://node1:8006/api/v1/alert-rules -d '{"name":"cert-expiry","spec":{"condition":
  {"kind":"certificate_expiring","days":21},"severity":"critical","targets":["ops-mail"]}}'
```

### 웹·권한

설정 › 보안 탭의 보기 "인증서"(`/settings/certificates`)가 계정·DNS 공급자·인증서 목록(노드마다 만료·다음 갱신·오류 칩, "지금 갱신",
만들기 대화상자 — HTTP-01이면 `http_listen`이 없는 노드 경고)이다([웹 화면](web-ui.md#설정-기록선언-묶음인증서-화면)). 읽기는 루트
`cluster.read`(가린 보기 — 비밀 칸은 없다), 쓰기·지금 갱신은 루트 `cluster.admin`이다. 감사는 `acme-account create le (letsencrypt,
tos agreed)`·`certificate create …`·`acme-order NAME issued (serial …, until …)` 꼴이고 EAB·계정 키·DNS 비밀·공유 키는 적지 않는다.
API는 `/acme-accounts`·`/acme-dns`·`/certificates`(GET·POST, `{key}` GET·PATCH·DELETE), `/certificates/{key}/actions/renew`,
`/acme-accounts/{key}/actions/check`. 선언형 종류 `AcmeAccount`·`AcmeDns`·`Certificate`로도 다룬다.

### Let's Encrypt 스테이징으로 손 확인

CI는 Pebble·pebble-challtestsrv·BIND로만 시험한다(아래 한계). 실제 CA는 스테이징으로 노드 하나에서 이렇게 본다(이 절차의 결과는 아직
확인하지 않았다):

1. 노드 이름 `node1.example.com`의 공인 A(또는 AAAA) 레코드가 그 노드를 가리키고 밖에서 포트 80이 닿게 한다. `[api] http_listen =
   "0.0.0.0:80"`을 넣고 `systemctl restart orbivisord`, `orbivisorctl -o json node get node1`의 `features`에 `http01`이 있는지 본다.
2. `orbivisorctl cert account add stg --directory letsencrypt-staging --email ops@example.com --agree-tos`.
3. HTTP-01: `orbivisorctl cert add stg-http --account stg --name node1.example.com --node <node1 id>` → `orbivisorctl cert renew stg-http
   --wait` → `cert list`에 일련번호·만료가 보이고 오류가 없다. `openssl s_client -connect node1.example.com:8006 -servername
   node1.example.com </dev/null | openssl x509 -noout -issuer -dates`의 발급자가 `(STAGING)`으로 시작한다(스테이징 루트는 브라우저가 믿지
   않는다 — 맞는 동작).
4. DNS-01 훅: 위 `nsupdate.sh`(또는 쓰는 DNS의 훅)를 노드에 두고 `orbivisorctl cert dns add hook --provider hook --path
   /etc/orbivisor/acme-hooks/nsupdate.sh` → `orbivisorctl cert add stg-dns --account stg --name '*.example.com' --shared --dns01 hook` →
   `cert renew stg-dns --wait` → `openssl s_client -servername x.example.com …`가 와일드카드 인증서를 받는다.
5. `orbivisorctl cert renew stg-http --wait`를 한 번 더 해 새 일련번호가 끼워지는지(열린 연결은 그대로) 본다.
6. 치우기: `cert delete stg-http`·`cert delete stg-dns`·`cert dns delete hook`·`cert account delete stg`, `http_listen` 줄을 지운다
   (`{data_dir}/pki/acme/`의 키 파일은 한 시간 뒤 노드가 지운다. CA 쪽 스테이징 계정은 남는다).

### 한계

- 실제 Let's Encrypt·ZeroSSL·Google과 실제 Cloudflare·Route 53·PowerDNS는 시험하지 않았다 — Pebble 2.10.1(`-strict`, badNonce 5%)·
  challtestsrv·BIND 9(RFC 2136 TSIG)와 가짜 Cloudflare·Route 53·PowerDNS로만 본다.
- RFC 2136은 응답의 TSIG 서명을 확인하지 않고 rcode만 본다(속인 성공 응답은 CA 검증 실패로 끝난다).
- RSA 키, TLS-ALPN-01, 내부 노드 인증서(8007 mTLS, 824일)의 갱신은 없다 — 내부 인증서는 만료 경보만 있다.
- `[api] tls_cert` 손 파일은 시작할 때만 읽는다(ACME와 함께 쓰지 않는다).
- `cert` 명령은 아직 `--reason`이 없다. 인증서 만들기 대화상자(웹)에는 노드 고르기·`preferred_chain`·갱신 정책·`install`이 없다(CLI·매니페스트로).
- 인증서 경보 조건은 CLI 경보 명령·웹 폼에 아직 없다.

## 대규모 클러스터

노드를 수백 대 더해도 리더가 느려지지 않게 하는 것들이다([설계 문서](../superpowers/specs/2026-11-11-availability-scale-design.md)
§5·§6·§14·§15).

```bash
orbivisorctl settings set voters=5   # 투표 멤버 상한 3|5|7(기본 5, 위트니스도 센다)
orbivisorctl node list               # VOTER 열: true 투표 멤버, false learner·관찰 노드, - 멤버십에 없음
orbivisorctl cluster status          # ROLE 열: voter | learner | observer
```

```toml
# /etc/orbivisor/orbivisord.toml (노드마다, 선택)
[node]
zone = "rack-a"   # 장애 영역(랙·전원·방). 노드 레이블 zone으로 등록된다
```

- **투표 멤버 상한.** 쓰기 확정과 선거는 투표 멤버의 과반만 기다린다. 리더의 `VoterKeeper`가 1초마다 투표 멤버를
  상한에 맞춘다: 모자라면 건강하고(10초 안에 갱신) 로그를 따라잡은 learner를 투표 멤버가 적은 zone의 것부터 올리고, 많으면
  오프라인·응답 없는 것, 붐비는 zone의 것부터 내린다. 30초 넘게 리스를 갱신하지 않은 투표 멤버는 올릴 수 있는 learner가
  있을 때만 그 learner로 한 번에 바꾼다. 대신할 노드가 없으면(노드가 상한 이하) 투표 멤버로 남긴다: 노드 둘과 위트니스에서
  꺼진 노드를 내리면 그 뒤 다른 노드가 죽었을 때 정족수가 없다. 한 번에 하나씩, 바꾸기 전과 뒤의 구성 모두에서 살아 있는
  (3초 안에 리더에 리스를 갱신한) 투표 멤버가 과반일 때만 바꾸고, 새 리더는 처음 10초 동안 개수 때문에 내리지 않는다.
  멤버십 변경은 두 단계(중간 구성을 커밋한 뒤 최종 구성)라 그 사이에 리더가 바뀌거나 시간이 지나 끊기면 다음
  `VoterKeeper`가 마무리한다(중간 구성이 바뀌지 않은 채 2초 넘게 보일 때만; 노드 삭제·`cluster leave`가 지운 노드는 그
  요청이 learner로 남지 않게 다시 확인해 지운다). 리더와 위트니스는 내리지 않는다(위트니스는 늘 투표 멤버). 나머지 노드는 대기 learner 둘과
  관찰 노드(아래)이고 VM·API 일은 투표 멤버와 같다. 모든 멤버가 새 버전이 되기 전(롤링 업그레이드)에는 예전처럼 모든 노드가 투표
  멤버로 올라온다(합류하자마자 죽어 등록하지 못한 learner는 기다리지 않는다; 기다리는 동안 막는 노드를 임기마다 한 번
  경고한다). 합류·재시작한 노드가 스스로 승격을 요청할지도 `VoterKeeper`와 같은 규칙으로 정한다. `[node] zone`의 앞뒤
  공백은 잘라 적는다. 웹의 클러스터 지도(인프라 › 노드 › 지도)는 레이블 `zone`마다 바닥 판, 그 안에서 `rack`마다 줄로 노드를
  늘어놓는다([클러스터 지도 화면](web-ui.md#클러스터-지도-화면)).
- **리스로 노드 감시.** 노드의 `online`은 리더가 리스 표로 정한다(마지막 갱신에서 10초 넘으면 오프라인). 모든 노드는
  2초마다 리더에 리스를 갱신한다: HA 펜싱을 켠 노드는 `HaGuard`가, `[ha] fencing = "off"`인 노드와 위트니스는 펜싱 없이
  생존 표시만 갱신한다. 새 리더는 한 리스(10초)를 기다린 뒤 판단한다. 옛 노드가 남아 있으면 예전의 ping 감시를 쓴다
  (클러스터에서 빠진 노드의 남은 기록은 보지 않고, 막는 노드는 임기마다 한 번 경고한다).
- **리더 확인 묶기와 모으기 상한.** 리스 갱신(HA·VIP)마다 하던 리더 확인(과반 왕복)을 같은 때에 온 갱신끼리 나눠 쓴다.
  확인 왕복이 갱신 간격보다 길 때만 많이 줄어든다(갱신이 몰릴 때와 지연이 큰 네트워크에서 효과가 크다; 한 컴퓨터 안의
  규모 시험처럼 왕복이 아주 짧으면 거의 줄지 않는다). 지표·감사 로그를 노드마다 모을 때는 동시에 32개까지 묻고, 끝난
  자리부터 다음 노드를 물어 느린 노드 하나가 나머지를 막지 않는다.
- **관찰 노드(계획 48).** 투표 멤버와 대기 learner 둘(`STANDBY_LEARNERS`) 말고 나머지 노드는 Raft 구성원이 아니다.
  learner에게도 리더가 하트비트(200 ms)와 로그를 보내므로 계획 47까지는 리더의 RPC가 노드 수에 비례했다(노드 300대에서
  초당 약 1,500~2,100). 관찰 노드는 리더가 아닌 투표 멤버 하나(노드마다 해시로 골라 고르게 퍼진다)에 노드 간 gRPC
  `Follow`로 붙어 스냅샷(처음·뒤처졌을 때)과 적용 묶음을 받아 자기 저장소에 쓴다. 그래서 리더의 Raft 일은 투표 멤버와
  learner 수에만 비례한다.
  - 읽기는 자기 복제본에서 한다(팔로워처럼 조금 늦을 수 있다). 쓰기와 HA·VIP 리스 갱신은 리더로 전달하고, 쓰기는 그
    리비전이 자기 복제본에 적용되기를 쓰기 제한 시간 가까이까지 기다린 뒤 돌려주므로 자기가 쓴 것은 보통 곧바로 읽힌다(적용이
    늦어도 쓰기는 성공으로 돌려주므로 잠깐 옛 값이 읽힐 수 있다). 늦은 적용이나 다른 노드에서 쓴 것을 확실히 읽으려면 쓴
    리소스의 `revision`을 요청 헤더 `Orbivisor-Min-Revision: <리비전>`으로 보낸다(그 리비전까지 5초 기다리고, 못
    따라잡으면 503). 응답 헤더 `Orbivisor-Revision`은 답할 때 그 노드가 적용한 리비전이다(쓰기 응답이라도 적용이 늦으면
    그 쓰기의 리비전보다 낮다). 기다린 뒤에는 다시 인증하므로 다른 노드에서 막 만든 토큰·사용자·비밀번호도 그
    리비전을 주면 쓸 수 있고, 기다리는 사이 폐기된 것은 거절된다. 인증되지 않은 요청은 그 노드가 있다고 아는 리비전일
    때만 기다리고 응답 헤더도 없다. 허용 출처(`web_origins`)의 웹 포털도 이 헤더를 읽을 수 있다
    (`Access-Control-Expose-Headers`).
  - 붙은 투표 멤버가 죽거나 3초 넘게 진행(1초마다 오는 `Progress`)이 없으면 다른 투표 멤버로 마지막 리비전부터 다시
    붙는다. 살아 있지만 리더를 따라가지 못하는 투표 멤버(전달한 쓰기·리스 갱신에 리더가 돌려준 리비전을 3초 넘게 알리지
    않는 노드)도 떠난다. 적용 기다림이 제한 시간보다 조금 일찍 끝나면 곧바로 다시 본다. 스냅샷을 받는 동안은 이 검사를
    하지 않아 오래 걸리는 스냅샷도 끝까지 받는다. 자기보다 뒤인 투표 멤버에는 붙지 않는다(상태가 뒤로 가지 않는다).
    기록(묶음 1024개)보다 뒤처졌으면 스냅샷을 다시 받는다. 지운 관찰 노드는 열린 스트림도 자기를 목록에서 빼는 묶음부터
    받지 못하고 끊긴다.
  - 리더의 `VoterKeeper`가 정한다: learner가 대기 둘보다 많고 모든 노드가 기능 `observer`를 알리면 건강한 learner를
    관찰 노드 목록(클러스터 정보)에 적고 멤버십에서 뺀다. 투표 멤버가 죽어 대기 learner가 모자라면 관찰 노드를 learner로
    되돌리고, 따라잡으면 보통의 승격으로 투표 멤버가 된다. 노드마다 `RoleKeeper`가 목록을 보고 Raft를 끄거나 다시 띄우며
    (관찰 노드가 되면 Raft 로그를 지우고 투표 기록은 남긴다. 다시 멤버가 될 때는 지운 로그의 마지막 인덱스까지 따라잡은
    뒤 Raft를 띄운다), 모드는 `cluster.redb`에 적혀 다시 시작해도 그대로다. 꺼진
    동안 내려진 노드는 적용이 30초 멈춘 것을 보고 투표 멤버에게 역할을 물어 관찰 노드가 된다. 새 노드는 지금처럼
    learner로 들어와 따라잡은 뒤 내려간다.
  - 헬스 체크(`GET /api/v1/health`)에서 관찰 노드는 붙은 투표 멤버의 진행이 5초 안에 왔고 리더를 알면 `quorum`이다.
    웹의 클러스터 화면과 `orbivisorctl cluster status`의 역할에 `observer`로 보인다.
  - 예외: HA·VIP 리스 갱신은 펜싱 안전 때문에 관찰 노드도 리더로 보낸다. 리더에는 노드당 초당 약 0.5 RPC(2초마다
    갱신)와 그 리더 확인(묶임)이 남는다.
  - 내리기 주의: 관찰 노드의 `cluster.redb`는 Raft 로그가 비어 있다(모드 `observer`). 계획 48 전 orbivisord는 이 모드를
    몰라 빈 로그로 Raft를 시작하므로 관찰 노드를 옛 버전으로 내리지 않는다. 내려야 하면 먼저 그 노드를 클러스터에서
    빼고(`cluster leave`) 옛 버전으로 다시 합류한다.
- **남은 것.** 리더 장애 뒤 새 리더까지는 노드 수와 관계없이 3~4초다(openraft가 마지막 하트비트 뒤 리더 리스 2초와 선거
  제한 시간 1~2초를 기다린다). 리스 갱신을 투표 멤버가 묶어 리더에 전달하는 것과 처음부터 관찰 노드로 합류하는 것은
  나중이다.
- **규모 시험.** `cargo test -p orbivisor-cluster --features scale-it --test scale -- --nocapture`가 한 프로세스에 노드
  300개(투표 5, 대기 learner 2, 나머지 관찰 노드, `ORBIVISOR_SCALE_NODES`로 바꿈(20 이상), CI는 100)를 띄우고 모든 노드가 리스를
  갱신하며 `VoterKeeper`·`RoleKeeper`를 띄운 채로 잰다. 관찰 노드는 learner로 따라잡지 않고 빈 저장소로 바로 관찰 노드로
  연다(시작 시간을 줄임). 저장소는 메모리에 둔다(한 디스크에 수백 노드의 fsync가 몰리지 않게). 기준과 잰 값(Apple
  Silicon 10코어, 시험 빌드, 두 번):

  | 항목 | 기준 | 노드 30 | 노드 100 | 노드 300 |
  |---|---|---|---|---|
  | 리더의 초당 RPC(하트비트·리스 갱신과 그 확인, 아래 지연을 넣고) | 35 + 노드당 4.5 미만(계획 47은 노드당 10) | 93(노드당 3.1) | 254~255(2.5) | 352~355(1.2) |
  | 그 가운데 비투표 구성원(대기 learner 둘)에게 | 15 미만(노드 수와 관계없이) | 7 | 7 | 7 |
  | 그 가운데 투표 멤버에게(묶지 않은 최악 = 하트비트 + 갱신 수 × 2 × 투표 멤버 수) | 최악의 0.6 미만 | 72(최악 171, 0.42) | 198(최악 523~525, 0.38) | 200~201(최악 1,479~1,499, 0.13~0.14) |
  | 쓰기 지연 p99(리더에서 하나씩) | 50 ms 미만(`ORBIVISOR_SCALE_P99_MS`, CI 200 ms) | 1.7~1.8 ms | 4.2~4.4 ms | 11.7~12.6 ms |
  | 관찰 노드의 뒤처짐 p99(리더가 쓰기를 마친 뒤 관찰 노드마다 적용하기까지) | 500 ms 미만 | 0.8~0.9 ms | 2.3~2.4 ms | 6.8~7.1 ms |
  | 노드 하나 추가(learner로 따라잡기까지) | 2초 미만 | 1 ms | 3 ms | 5~8 ms |
  | 투표 멤버 장애 뒤 모든 관찰 노드가 다시 붙어 새 쓰기를 받기까지 | 5초 미만 | 0.2초(10개가 붙어 있었음) | 1.2초(24개) | 1.2초(82개) |
  | 리더 장애 뒤 새 리더 | 5초 미만(설계 3초, `ORBIVISOR_SCALE_FAILOVER_MS`, §14; 10초면 그 자리에서 실패) | 3.2초 | 3.3~3.6초 | 3.4~3.6초 |

  계획 47(나머지가 learner)의 같은 구간 리더 RPC는 노드 100에서 570, 노드 300에서 1,320이었고 그 3분의 2가 learner
  하트비트였다. 지금 노드 수에 따라 느는 것은 전달받은 리스 갱신(노드당 0.5)과 그 리더 확인뿐이다.

  CI 러너(코어 3~4개)는 노드 100개를 한 프로세스에서 돌리므로 쓰기 지연이 CPU에 묶여 흔들린다. 그래서 CI는 쓰기 p99
  기준만 `ORBIVISOR_SCALE_P99_MS=200`으로 둔다(쓰기가 멈추거나 노드 수에 따라 크게 느려지는 것만 잡는다). 다른 기준은 CI의
  노드 100에서도 같게 검사한다.

  리더 RPC를 재는 5초 동안은 투표 멤버로 가는 `append_entries`(하트비트·리더 확인)에 지연(2초/노드 수, 20 ms 이상 —
  노드 30에서 67 ms, 100부터 20 ms)을 넣는다. 한 컴퓨터 안에서는 확인 왕복이 갱신 간격보다 짧아 묶이는 정도가 기계·빌드
  속도에 따라 달랐다. 지연을 넣으면 확인은 초당 많아야 1/지연번이라 확인 요청(초당 노드 수)의 절반 이하로 묶이고, 묶기를
  빼면 노드 30·100 모두 0.79~0.80으로 실패한다. 계획 47은 노드 수와 관계없이 20 ms였고, 그러면 확인 요청이 초당 50번이
  안 되는 노드 50 미만에서는 묶일 까닭이 없어 노드 30에서 이 검사가 실패했다. 지연이 하트비트 간격(200 ms)에 가까워지면
  선거가 나므로 노드는 20 이상이어야 한다. 쓰기 지연·뒤처짐·노드 추가·장애는 지연 없이 잰다.

  투표 멤버 장애는 시험의 memnet이 끊긴 노드의 다음 프레임(1초마다 `Progress`)에서 연결을 끊어 1초 안에 알아챈다. 실제
  gRPC에서 연결이 끊기지 않고 멈추면 3초 동안 진행이 없는 것으로 알아채므로 다시 붙기까지 3초 남짓이다.

  같은 컴퓨터에서 투표 멤버 다섯을 파일 저장소에 두면 노드 여섯에서도 쓰기 p99가 64 ms다(다섯이 디스크 하나에서 fsync를
  줄 서기 때문, 노드 300에서 67 ms). 실제 노드의 쓰기 지연에는 그 노드 디스크의 fsync 시간이 더해진다.

## 클러스터 통신 다중 링크

노드 사이의 모든 통신(`cluster_listen`으로 가는 Raft·리스 갱신·쓰기 전달·관찰 노드의 Follow·콘솔·마이그레이션 준비)을
망 여럿으로 잇는다. 한 망(스위치·케이블·NIC)이 끊겨도 다른 망으로 클러스터가 그대로 돈다([설계 문서](../superpowers/specs/2026-11-11-availability-scale-design.md)
§7·§16).

```toml
# /etc/orbivisor/orbivisord.toml (노드마다)
address = "10.0.0.11"                           # API 주소와 인증서 이름(그대로)
cluster_listen = "0.0.0.0:8007"                 # 모든 링크에서 받게
cluster_addresses = ["10.0.0.11", "10.1.0.11"]  # 앞이 우선 링크. 호스트만(IP나 DNS 이름), 포트는 cluster_listen의 것
```

- **값.** 없거나 비면 `address` 하나다(계획 49 전과 같다). 호스트만 적고 포트·중복(같은 IP의 다른 표기, 대소문자만 다른
  이름 포함)은 거절한다. 호스트 규칙은 합류 API와 같다(특수 주소·끝의 점·와일드카드 안 됨). `address`도 같은 규칙이다.
  `address`가 목록에 없어도
  된다(예: API는 관리망, 노드 사이는 전용망 둘). `cluster_listen`은 모든 링크에서 받아야 하므로 `0.0.0.0`이나 `::`로
  둔다. 한 주소로 두고 링크가 둘 이상이면 시작할 때 경고만 남긴다.
- **넘어가기.** 노드마다 첫 링크(우선 링크)를 쓴다. 연결이 안 되면 곧바로, 답이 늦으면 그 링크에서 연속 두 번이거나 그
  링크의 마지막 성공에서 400 ms가 지났을 때 다음 링크로 넘어간다(하트비트 하나가 늦은 살아 있는 링크는 버리지 않는다.
  조용히 죽은 링크에서 마지막 성공 뒤 첫 하트비트가 다음 링크로 가기까지 약 600 ms로, 선거 제한 시간(1~2초)에 걸리지
  않는다; 리스 갱신은 3초 제한 시간 한 번). 연결하지 못한 호출은 같은 호출 안에서 다음 링크로 한 번 더 보낸다(보내지
  않았으므로 안전하다; 답이 늦은 호출, 이미 보낸 뒤 연결이 끊긴 호출은 상대가 받았을 수 있어 다시 보내지 않는다). 모두 안 되면 돌아가며 시도한다. 우선 링크가 아닌 동안
  30초마다 짧은 노드 간 호출(Raft·ping·리스 갱신·Follow 열기) 하나를 우선 링크로 보내 보고, 성공하면 돌아간다(그동안 다른
  호출은 지금 링크로; 콘솔·마이그레이션·쓰기 같은 크거나 긴 호출은 늘 지금 링크로). 넘어가면 `warn` 로그("cluster link failed;
  switching to the next link"), 돌아오면 `info` 로그를 남긴다. 링크 상태는 아직 헬스·지표·웹에 없다(나중에 관리망 검사와
  함께). 링크마다 따로 TCP 연결이라, 죽은 연결 위에서 이미 기다리던 긴 요청(투표 멤버 승격의 따라잡기 같은)은 keepalive
  (30초)까지 걸릴 수 있다(다음 호출은 새 링크로 간다).
- **인증서.** 노드 인증서에 `address`, 모든 `cluster_addresses` 호스트, localhost 셋이 들어간다(만들기·합류). 기존 노드에
  링크를 더하고 다시 시작하면 노드가 클러스터 CA로 같은 키에 인증서를 다시 서명해 쓴다("node certificate signed again
  with the new names" — 나갔다 다시 합류할 필요가 없다). 다시 서명하지 못해 인증서에 이름이 없는 링크는 클러스터에
  알리지 않는다(경고 "the node certificate does not name these cluster links").
- **잘못된 링크.** 다른 노드의 주소를 링크로 적으면(오타, 다시 쓴 IP) 그 노드에 갈 요청을 이 노드가 받게 된다. 그래서
  다른 노드의 링크와 겹치는 링크로는 합류하지 못하고(400), 리더도 그런 링크로 멤버십을 고치지 않는다(경고 "not updating
  the cluster links of a node"). 노드 간 호출은 가려는 노드의 id(`orbivisor-target` 메타데이터)를 실어 보내고, 받은 노드가
  다른 노드면 거절해 보내는 쪽이 다음 링크로 넘어간다(옛 노드는 싣지 않으므로 그대로 받는다).
- **보기.** `GET /api/v1/cluster`의 `members[].cluster_addresses`(모든 링크, 첫 것이 `cluster_address`)와
  `orbivisorctl -o json cluster status`에 보인다(표는 우선 링크만). 웹 클러스터 화면은 둘째 링크부터 클러스터 주소 아래
  작게 보인다.
- **마이그레이션.** VM 메모리·디스크 스트림은 대상 노드에 지금 쓰는 링크의 호스트로 간다(우선 링크가 죽었어도 옮긴다).
  그래서 `[migration] ports`를 모든 링크의 망에서 연다.
- **업그레이드.** 노드 기능 `multi-link`다. 모든 노드(위트니스·관찰 노드 포함)가 이 버전이 되기 전에는 링크가 멤버십에
  들어가지 않아 첫 링크 하나로 돈다(옛 노드는 여러 링크 항목을 첫 링크 하나로 읽는다). 모든 노드가 알리면 리더가 1초마다
  노드 하나씩 멤버십(관찰 노드는 관찰 노드 목록)의 링크를 그 노드가 적은 것으로 고친다. 여러 링크로 합류(`cluster
  join`)는 업그레이드가 끝난 뒤에 된다(그 전에는 409 `cluster_upgrading`; 새 노드는 첫 링크가 죽어 있어도 로그를 받도록
  합류 때 모든 링크를 싣는다). 조인 URL의 노드가 아직 옛 버전이면 모르는 필드(`cluster_addresses`)로 거절하고, 합류하는
  노드는 "upgrade every node first"로 알린다 — 모든 노드를 먼저 올리거나 링크 하나로 합류한 뒤 링크를 더한다. **주의:** 이때 설정의 링크가 멤버십의 주소와 다른 기존 노드(예: `address`를 바꾸고 다시
  시작한 노드)는 첫 링크도 그 노드의 설정 값으로 바뀐다. 업그레이드 전에 노드마다 `address`(또는 `cluster_addresses`)가
  다른 노드가 닿는 주소인지 본다. `address`도 이제 합류 API와 같은 호스트 규칙으로 검사하므로(끝의 점·멀티캐스트 등은
  거절되어 노드가 시작하지 않는다) 올리기 전에 확인한다.
- **시험.** `cargo test -p orbivisord --test cluster`가 우선 링크를 아무도 듣지 않는 127.0.0.2로 둔 세 노드(`cluster_listen`은
  127.0.0.1)로 합류·쓰기 전달·리더 kill -9 뒤 새 리더를 확인하고(macOS는 연결이 응답 없이 시간 초과, Linux는 거절이라
  두 가지 모두 "죽은 우선 링크"다), 링크 하나로 만든 노드에 `cluster_addresses`를 더해 다시 시작하면 인증서와 멤버십이
  바뀌는지 본다. 링크를 도중에 끊고 되살리기는 memnet(`cargo test -p orbivisor-cluster --test links`)과 gRPC
  (`cargo test -p orbivisor-peer --test grpc`) 시험이 본다.

## 노드 유지보수와 비우기

커널·펌웨어를 올리거나 하드웨어를 손보기 전에 노드를 **유지보수 모드**로 두고 실행 중인 VM을 다른 노드로 옮긴다
(Proxmox의 노드 유지보수, vSphere의 유지보수 모드와 같은 자리). 유지보수 노드에는 새 VM을 놓지 않는다: 스케줄러의
자동 배치, HA 복구 대상, 마이그레이션 대상에서 빠진다. **비우기**는 노드를 유지보수로 바꾸고 그 노드의 실행 중인
VM을 스케줄러가 고른 노드로 하나씩 라이브 마이그레이션한다. 로컬 디스크 VM은 복제본이 있는 노드를 먼저 고른다(위
"로컬 디스크 복제와 HA").

```bash
orbivisorctl node drain node1 --wait     # 유지보수로 바꾸고 모두 옮길 때까지 기다린다: "node node1 drained"
orbivisorctl node drain node1            # 기다리지 않는다: "drain of node1 started (task …); follow it with `orbivisorctl task get …`"
orbivisorctl node list                   # MAINTENANCE 열
orbivisorctl node maintenance node1 off  # 유지보수 끝: "node node1: maintenance off"
orbivisorctl node maintenance node1 on   # VM은 옮기지 않고 배치만 막는다
```

웹은 노드 화면 행 메뉴의 "노드 비우기…"(확인 대화상자에 옮길 실행 중 VM 수와 로컬 디스크 복사 안내, 그 뒤 작업을
따라감 — 오프라인·macOS(HVF)·비우는 중인 노드는 이유와 함께 막힘)와 "유지보수 시작/끝"이다. API는 `POST /api/v1/nodes/{key}/drain`(본문 없음, admin, 202와 작업
`node-drain`)이고, 유지보수 켜기·끄기는 노드 수정 `PUT /api/v1/nodes/{key}`의 `spec.maintenance`(admin, If-Match)다.

**절차.** 한 노드씩:

1. `orbivisorctl node drain <노드> --wait`(어느 노드에서 해도 된다 — 옮기는 일은 비우는 노드의 실행기가 한다). 실패하면
   옮기지 못한 VM과 이유가 나온다 — 그 VM을 끄거나 손으로 옮긴 뒤 다시 `drain`한다.
2. 노드를 손본다(업그레이드 `orbivisorctl node upgrade <노드> --wait` — 다른 노드에 로그인해서, 커널 업데이트, 재부팅,
   하드웨어). 비우기가 성공했으면 그 노드에 HA VM이 없으므로 orbivisord를 멈춰도 watchdog이 꺼져 재시작되지 않는다(위
   "HA VM과 자기 펜싱").
3. `orbivisorctl node list`에서 ONLINE을 확인하고 `orbivisorctl node maintenance <노드> off`. 옮긴 VM은 돌아오지 않는다(필요하면
   `vm migrate`).

- **비우기 요청.** 거절: 노드가 없으면 **404**, 위트니스(`node "X" is a witness node and runs no VMs`)나 오프라인
  노드(`node "X" is offline; HA recovers the HA VMs of an offline node` — 꺼진 노드의 HA VM은 HA 복구가 맡는다)는
  **409** `node_unavailable`, 그 노드의 비우기 작업이 끝나지 않았으면 **409** `node_busy`, 비우기를 모르는 노드(위트니스
  포함)가 남았으면 **409** `cluster_upgrading`("some nodes do not support node drain yet (…); upgrade them first"). 버전이
  아니라 노드가 등록할 때 적는 기능 표시(`status.features`의 `node-drain`)를 보므로, 비우기가 처음 들어간 버전으로 올리는
  동안은 못 쓰고(그때는 `vm migrate`와 유지보수 시작으로) 그 뒤의 롤링 업그레이드에서는 버전이 섞여도 된다. 옛 버전
  리더가 노드 상태를 다시 쓰며 이 표시를 지워도 노드가 1분마다 다시 적으므로 다음 주기에 되살아난다 — 새 버전 노드의
  이름이 `cluster_upgrading`에 나오면 1분쯤 뒤 다시 하거나 그 노드의 orbivisord를 다시 시작한다. 노드를
  유지보수로 바꾼 뒤(이미면 그대로) 작업을 만든다. 작업이 실패해도 노드는 유지보수로 남는다.
- **무엇을, 어떻게.** 비우는 노드의 실행기가 그 노드에 배정되고(`spec.node`) 원하는 상태가 running인 VM을 이름 순서로
  한 번에 하나씩 옮긴다. VM에 다른 작업(전원·스냅샷·마이그레이션 등)이 있으면 끝나기를 `[migration] timeout`(기본 30분)까지
  기다린다. 대상은 VM 만들기의 스케줄러와 같은 규칙(온라인, 유지보수·위트니스 아님, 아키텍처·디스크 풀·브리지·방화벽, HA
  VM이면 HA 가능 노드)을 지나고 macOS(HVF) 노드가 아니며 남은 메모리가 VM 메모리 이상인 노드 가운데 부하 점수가 가장 좋은
  노드(옮기는 VM 자신의 부하는 모름, 같으면 미할당 메모리가 많은 노드, 그다음 id가 작은 노드 — 위 "최적 배치")다. 비우기
  중에는 균형 조정기가 쉰다. 로컬 디스크 VM은 그 디스크의 같은 이름의 풀을 쓸 수 있는 노드만 고르고 디스크를
  그 풀에 복사한다("라이브 마이그레이션"의 "로컬 디스크 복사" — 비우기는 `pool`을 고르지 않는다). 마이그레이션은 `vm
  migrate`와 같은 확인을 거친 보통의 `vm-migrate` 작업이고(작업 목록에 하나씩 보인다), 비우기는 그것이 끝나기를 기다린
  뒤(`[migration] timeout`의 두 배와 1분까지, 디스크를 복사하면 `disk_timeout`을 더해서 — 넘으면 `migration task did not
  finish`) 다음 VM으로 간다. 그 사이 VM이 꺼지거나 지워지거나 다른 노드로 갔으면 넘어가고(다시 나타나면
  세 번까지 다시 보고, 그 뒤에도 다시 나타나면 `busy`), 확인과 작업 만들기 사이에 다른 작업이 끼면 한 번 더 기다려 다시
  한다. 비우기 작업에는 일반 작업의 시간 제한(600초)이 없다.
- **성공은 노드가 빈 것이다.** 옮길 VM이 없어도 이 노드로 들어오던 마이그레이션(비우기 전에 시작한 `vm migrate`)이
  있으면 끝나기를 `[migration] timeout`까지 기다려(넘으면 `busy`) 들어온 VM도 옮긴다. 비우기가 옮긴 VM이 비우기 중에
  다시 들어오면 한 번 더 옮기지 않고 `moved back during the drain`으로 남긴다.
- **옮기지 못한 VM.** 로컬 디스크 VM인데 디스크 복사를 모르는 노드가 있음(`disks on a local pool (some nodes do not
  support storage migration yet)`)이나 스냅샷이 있음(`disks on a local pool and snapshots`), 로컬 디스크의 같은 이름 풀을
  쓸 수 있는 노드가 없음(`no node can take its local disks` — 디스크 풀만 아니면 받을 노드가 있을 때), macOS(HVF)
  노드(`hvf node`), 받을 노드가 없음(`no node with capacity`), 바쁨(`busy` — VM의 다른 작업이나 이 노드로 들어오는 마이그레이션이 `[migration] timeout`까지
  끝나지 않음, 다시 기다린 뒤에도 마이그레이션 작업 만들기가 다른 작업과 겹침, 사라졌다 다시 나타나기를 세 번 넘게 함),
  마이그레이션 작업이 끝나지 않음(`migration task did not finish`), 다시 들어옴(`moved back during the drain`),
  배치 규칙 때문에 받을 노드가 없음(`placement rule …` — hard affinity로 묶인 같은 노드의 VM은 묶음 전체를 받을 노드로 함께
  옮긴다, "배치 규칙과 HA 정책"), 마이그레이션 실패(그 작업의 오류 — 대상 풀의 공간 부족 등)는 그 노드에 남기고 다음 VM으로 간다. 끝에 작업이 "N VM(s)
  were not moved: db (disks on a local pool and snapshots), …"로 실패한다(`--wait`면 이 오류와 종료 코드 1). 꺼진 VM은 옮기지 않는다 — 노드에 배정된 채 남고, 켤 때 다른 노드로 간다(아래).
- **유지보수를 끄면 멈춘다.** 비우기는 VM마다(VM이 자리 잡기를 기다린 뒤 마이그레이션을 시작하기 직전에도, 들어오는
  마이그레이션을 기다리는 동안에도) 노드를 다시 읽어 유지보수가 끝났으면 "drain stopped: maintenance ended"로
  실패한다(옮기는 중인 마이그레이션은 끝까지 간다). 그래서 `orbivisorctl node maintenance <노드> off`는 끝나지 않은 비우기가
  있으면 stderr에 `warning: the running drain of X stops (task T)`를 알리고, 웹은 "유지보수 끝내기" 전에 "끝내면 진행 중인
  비우기가 멈춥니다"로 묻는다. orbivisord가 비우기 도중 다시 시작하면 작업은 "interrupted"로 실패한다. 노드는 유지보수로
  남으므로 다시 `drain`하면 남은 VM부터 옮긴다.
- **유지보수 노드에 고정된 VM 켜기.** 꺼진 VM은 노드 배정을 그대로 두므로, 유지보수 노드에 배정된 꺼진 VM을 켜면
  스케줄러가 다른 노드를 골라 VM의 노드를 바꾸고 그곳에서 켠다(UEFI 변수는 옛 노드에서 받아 온다(옛 노드가 꺼져
  있으면 경고 뒤 켜는 노드의 파일·템플릿) — "라이브 마이그레이션"). 고를 노드가 없으면(디스크가 그 노드의 로컬
  풀에만 있는 등) **409** `node_unavailable`("node "X" is in maintenance and no other node can run this VM; end
  maintenance first"). 아직 그 노드에서 돌거나 멈추는 중인 VM은
  옮기지 않는다. VM 만들기·편집에서 유지보수 노드를 새로 고르면 **409** `node_unavailable`("… is in maintenance and
  cannot take VMs; end maintenance first")이고, 이미 그 노드에 있는 VM의 다른 편집은 막지 않는다.
- **한계.** 로컬 디스크 VM은 같은 이름의 풀로만 옮기고 스냅샷이 있으면 옮기지 못한다. 디스크 복사는 오래 걸릴 수 있다.
  한 번에 하나씩 옮기고, 진행률은 각 `vm-migrate` 작업으로 본다. 유지보수가 끝나도 VM을 원래 노드로 되돌리지 않는다. 옮기지
  못한 VM을 끄는 선택과 롤링 업그레이드 자동화(노드를 차례로 비우고 올리고 재부팅)는 없다. 볼륨 만들기·백업 같은
  작업의 노드 고르기는 유지보수를 보지 않는다.

## 클러스터 CPU 기준

CPU 세대가 섞인 클러스터에서 모든 노드가 켤 수 있는 공통 CPU를 정한다(vSphere EVC와 같은 자리). 기준은 **모델이 `cluster`인
VM에만** 쓰인다 — `host`·`max`·`x86-64-v*`·QEMU 모델 VM은 기준과 상관없다(클러스터 숨김 목록 `cpu.mask`만 모든 VM에 걸린다,
[취약점 때문에 CPU 기능 숨기기](vms.md#취약점-때문에-cpu-기능-숨기기)). 모델과 플래그 자체는 [CPU 모델과
플래그](vms.md#cpu-모델과-플래그).

| 모드 | 뜻 |
|---|---|
| `off`(기본) | 기준이 없다. `cluster` VM은 `host`처럼 켠다 |
| `auto` | 리더가 x86_64 노드들이 모두 켤 수 있는 가장 좋은 모델을 고른다(아래 셈) |
| 고정(`cpu.pinned`) | 관리자가 아키텍처마다 모델·플래그를 고정한다. 고정이 있는 아키텍처는 모드가 `off`여도 고정을 쓴다(`off`는 auto 셈만 끈다) |

```bash
orbivisorctl settings set cpu.baseline=auto
orbivisorctl cpu baseline
# mode: auto
# auto flags: on
# mask: avx512vbmi
# ARCH    CPU                MODE  LIMITED-BY  CLUSTER-VMS
# x86_64  Skylake-Server-v5  auto  node4       12
orbivisorctl settings set cpu.auto_flags=false           # 모델 밖의 공통 플래그를 더하지 않는다
orbivisorctl settings set cpu.pinned='x86_64:Skylake-Server-v5,-hle;aarch64:neoverse-n1'   # 아키텍처마다 고정(`;`로 여럿)
orbivisorctl settings set cpu.pinned=                    # 고정을 모두 뺀다
orbivisorctl settings set cpu.pinned=x86_64:Icelake-Server-v7 cpu.force=true   # 켤 수 없는 노드가 있어도 고정
orbivisorctl vm create app --cpu-model cluster ...       # 기준을 쓰는 VM
```

웹은 설정 › 일반 › **CPU**(모드 끔·자동·고정, 고정 모델·플래그, "공통 플래그 더하기", "모든 VM에서 숨기기"), 인프라 › 노드 ›
클러스터 보기의 **"CPU 기준"** 카드(아키텍처마다 기준·이전·바뀐 때·셈에 든 노드·기준을 낮춘 노드·경고·`cluster` VM 수와 접힌
모델 × 노드 표), 노드 상세의 **"CPU 모델"**(QEMU·가속기·벤더·호스트 CPU·읽은 시각, 모델 표, "CPU 모델 다시 읽기")이다. 기준이
있으면 새 VM은 `cluster`로 시작한다. API는 `GET /api/v1/cpu/baseline`(루트 `cluster.read`), `PATCH /api/v1/settings {"cpu":
{"baseline", "pinned", "auto_flags", "mask", "force"}}`(루트 `cluster.admin`, 준 칸만 바꿈, 응답에 `affected_running_vms`),
`GET /api/v1/nodes/{key}/cpu`·`POST /api/v1/nodes/{key}/cpu/refresh`(**202** Task `node-cpu-probe`, `cluster.admin`)다.

- **노드가 CPU를 알리는 법.** orbivisord가 시작할 때 자기 QEMU를 VM과 같은 머신·가속기로 한 번 띄워 QMP로 모델·플래그를 읽고
  (x86 TCG에서 0.3초 안쪽), 결과(**카탈로그**)를 해시 이름의 클러스터 리소스 하나로 올린다. 같은 QEMU·CPU·가속기인 노드는 같은
  카탈로그를 나눠 쓰고 노드 상태에는 해시만 있다(x86 카탈로그 약 41 KB, aarch64 약 1 KB). 검사 루프가 5초마다 QEMU 바이너리의
  경로·크기·수정 시각을 보고 바뀌면(apt로 QEMU를 올림) 다시 읽는다. 실패한 읽기는 10분마다 다시 하고, 그동안 앞 카탈로그를
  쓴다(`node cpu`의 `error:`). 리더가 어느 노드도 가리키지 않고 1시간 넘은 카탈로그를 지운다. `orbivisorctl node cpu node3`이
  QEMU 버전·가속기·호스트 CPU·읽은 시각과 모델 표(MODEL USABLE MISSING NOTE)를, `--refresh`가 먼저 다시 읽기를 한다(10초
  안팎). 카탈로그가 없는 노드(처음 읽기가 실패, 옛 노드)는 플래그 없는 `host`·`max` VM만 받는다 — 숨김 목록이 있으면 그
  이름을 QEMU가 아는지 확인할 수 없어 아무 VM도 켜지 않는다(숨김을 빼고 켜서 감춘 기능을 드러내지 않는다).
- **auto 셈(x86_64).** 위트니스가 아닌 이 아키텍처의 **등록된 노드 모두**가 대상이다 — 꺼진 노드·유지보수 노드도 센다(잠깐
  꺼진 노드 때문에 기준이 오르내리지 않게). 카탈로그가 아직 없는 노드는 빼고 경고(`node "x" has no CPU catalog yet and is left
  out`)로 적는다. 노드가 알린 해시의 카탈로그 리소스가 클러스터에 없으면(올리기가 실패했거나 정리 작업과 겹침) 리더는 그 노드가
  **온라인이고 그 읽기가 10분 안일 때만** 그 아키텍처의 auto 기준을 앞 기록 그대로 붙잡고(경고 `… the previous baseline is
  kept for up to 10 minutes`), 그 뒤에는 그 노드를 빼고 셈한다(경고 `node "n5" announced a CPU catalog that is missing from the
  cluster and is left out`, 리더 로그에도). 붙잡기는 그 노드의 아키텍처만이고 auto끼리만이다 — 고정·끔·auto로 바꾸기와 숨김
  목록은 곧바로 적용된다. 노드는 카탈로그를 올리지 못하면 새 해시를 알리지 않고 앞 것을 둔 채 다음 검사 회차에 다시 한다. 후보는 모든 대상 노드가 켤 수 있는 버전 모델(별칭이 아님, migration-safe, deprecated 아님) 가운데 모델 벤더가
  모든 노드의 호스트 벤더와 같은 것이고, 정적 확장의 기능이 가장 많은 것(같으면 이름순 뒤, 곧 높은 버전)을 고른다. 호스트
  벤더가 섞였거나(Intel + AMD) TCG 노드가 있으면 QEMU 모델 후보가 없어 모두 켤 수 있는 가장 높은 `x86-64-v*`, 그것도 없으면
  `qemu64-v1`이다. `auto_flags`(기본 켬)면 모든 노드가 줄 수 있고 QEMU가 어느 모델 정의에든 쓰는 플래그를 더한다. 마지막에
  숨김 목록을 끈다.
- **리더가 기록한다.** 리더가 10초마다(정리 작업의 틱) 다시 셈해 결과를 클러스터 상태에 쓰고, 모든 노드가 켤 때 이 기록만
  읽는다 — 그래서 모든 노드가 같은 기준을 쓴다. 모델·플래그가 바뀌면 기록 Task **`cpu-baseline`**(아키텍처, 이전 → 새 모델·
  플래그, 기준을 낮춘 노드 `limited_by`, 영향 받는 실행 중 `cluster` VM 수)이 활동 › 작업과 알림 센터에 보이고(처음부터 성공인
  기록이다 — 실행되는 작업이 아니다), 감사 로그에 `cpu.baseline`(`cpu baseline x86_64 auto: Icelake-Server-v7 ->
  Skylake-Server-v5 (node n4)`)이 남는다. 기준이 없어지면(끔·고정 빼기·그 아키텍처 노드가 모두 빠짐) 새 값 없는 기록이다.
  노드 목록·경고만 바뀌면 기록은 고치지만 Task는 남기지 않는다. 기준 바뀜에 걸리는 경보 규칙 조건은 없다.
- **약한 노드가 들어오면(auto).** 기준이 내려간다 — 합류를 거부하지 않는다(VMware EVC와 다름). **실행 중인 `cluster` VM은
  그대로다**: 켤 때 기록한 CPU로 계속 돌고, 보류 `cpu`(`Icelake-Server-v7 → Skylake-Server-v5 (cluster baseline changed)`)를
  얻으며, 약한 노드로는 옮겨지지 않고(`vm migrate`는 **409** `no_capacity` `node "n4" cannot run CPU model Icelake-Server-v7
  (missing …)`, 웹 마이그레이션 대화상자는 그 노드를 막음) 나머지 노드 사이에서는 지금처럼 옮겨진다. 전원을 껐다 켜면 새
  기준으로 켜져 어디로든 옮길 수 있다. 새로 켜는 `cluster` VM은 처음부터 새 기준이다. 약한 노드를 빼면 기준이 다시 오를 수 있고
  (같은 기록 Task), 실행 중 VM은 "다음 전원 껐다 켜기 때 더 나은 CPU" 보류를 얻는다.
- **고정에서 약한 노드.** 기준은 바뀌지 않는다. 그 노드는 `cluster` VM을 받지 않고, `cpu baseline`의 경고와 LIMITED-BY,
  노드 상세의 "이 노드는 클러스터 기준 VM을 받지 않습니다" 띠로 보인다. 고정을 설정할 때 켤 수 없는 노드가 있으면 **409**
  `cpu_baseline_unsupported`(`details.nodes`·`warnings`)이고 `cpu.force=true`(웹 "그래도 고정")로 넘는다. 숨김 목록이 빠진
  플래그를 끄면 그 노드도 켤 수 있으므로 409가 아니다.
- **마이그레이션·HA·배치가 기준을 쓰는 법.** 켜기·배치·복제 배치·HA 복구는 **새로 켜는 것**이라 스펙을 대상 노드에서 지금
  기준·숨김 목록으로 해석해 그 노드의 카탈로그로 켤 수 있는지 본다 — 기준이 내려가도 HA 복구는 새 기준으로 어디서든 켠다.
  라이브 마이그레이션·비우기·균형 조정·HA 되돌아가기는 **도는 VM을 옮기는 것**이라 켤 때 기록한 CPU를 대상이 켤 수 있어야
  한다. 비우기는 갈 곳이 없는 VM을 지금처럼 "옮길 곳 없음"으로 남기되, 모든 후보가 CPU 때문에 받지 못하면 "no node with
  capacity" 대신 그 CPU 까닭을 적는다(라이브 마이그레이션을 막는 `+invtsc`로 도는 VM은 `the VM runs with the CPU flags +invtsc
  that QEMU cannot live-migrate; power cycle it on another node instead`). 꺼진 VM이 고정된 노드가 그 CPU를 켤 수 없으면 켜기가
  켤 수 있는 다른 노드를 고르고, 없으면 **409** `no_capacity`와 노드마다의 까닭이다. 스케줄러가 먼저 거르므로 노드가 켜다가
  실패하는 일은 보통 없다(그래도 노드는 켜기 전에 다시 보고, 못 켜면 `failed to start: the VM's node cannot run CPU model …` —
  VM 상태는 노드 이름을 볼 수 없는 폴더 사용자도 읽으므로 노드 이름을 적지 않는다). **작업자가 스스로 다시 켜는 VM**(크래시,
  호스트의 SIGTERM, 게스트 감시가 멈춘 게스트를 다시 켬)은 다르다: 보류된 새 CPU(바뀐 기준·고정, 실행 중에 바꾼 모델)를 그
  노드가 켤 수 없으면 꺼지기 전에 쓰던 CPU(`cpu_resolved`)로 다시 켜고 바뀜은 보류로 남긴다(로그 `restarting the VM with the
  CPU it ran with; the new CPU stays pending`). 사용자가 하지 않은 전원 껐다 켜기 때문에 VM이 꺼진 채 남지 않게 한 것이다. 그
  CPU도 이 노드가 켤 수 있어야 하고(그 사이 QEMU를 올렸으면 안 될 수 있다) 숨긴 기능을 새로 드러내지 않아야 한다 — 아니면
  지금처럼 꺼진다. 새 CPU는 사용자의 전원 껐다 켜기(`vm reboot`, 정지 후 시작, 켤 수 있는 노드로 옮긴 뒤)에서 적용된다.
- **aarch64.** auto 셈을 하지 않는다(QEMU가 aarch64 모델의 버전·못 쓰는 기능·정적 확장을 주지 않는다). 고정만 되고, QEMU
  모델은 모든 대상 노드가 TCG일 때만 받는다(svc01·svc02 같은 `qemu-tcg` 노드 — 예 `aarch64:neoverse-n1`). KVM·HVF aarch64는
  `host`·`max`뿐이라 `cluster` VM은 `host`로 켜고 `cpu baseline`이 `host (no baseline)`로 적는다. aarch64 QEMU 모델에는
  `enforce`가 없고, 숨김 이름이 그 모델에 없는 속성이면(`neoverse-n1`의 `pauth`) QEMU가 켜지 않는다.
- **업그레이드.** 모든 노드(위트니스 포함)가 기능 `cpu-models`를 알릴 때까지 노드는 카탈로그를 올리지 않고 리더는 기준을
  기록하지 않으며, 설정 `cpu`와 `host`·`max` 밖의 모델은 **409** `cluster_upgrading`이다. 그동안 VM은 지금과 바이트까지 같은
  `-cpu host|max`로 켠다. 올린 뒤 숨김 목록을 더하면 업그레이드 전에 켠 VM에도 보류가 생긴다.
- **한계.** 기준은 클러스터에 하나(아키텍처마다)다 — VM별 기준(vSphere per-VM EVC)·폴더 범위 숨김 목록은 없다. VMware VMX의
  EVC 모드는 가져오지 않는다. 노드의 마이크로코드·CPU 취약점 상태(`/sys/devices/system/cpu/vulnerabilities`)는 보이지 않는다.
  노드 목록에 "CPU 모델" 열은 없다(노드 상세에서 본다).

## 호스트 준비: 예약·전용 CPU와 hugepages

[전용 CPU·hugepages VM](vms.md#numa전용-cpuhugepages)을 받을 노드를 준비한다. **아무 설정이 없는 노드는 지금과 같다** — 모든
CPU가 공유이고, VM에 친화·`AllowedCPUs`를 걸지 않는다. 노드는 자기 NUMA 토폴로지를 스스로 읽어 알리므로, 관리자가 정하는 것은
CPU를 어떻게 나눌지와 hugepages를 얼마나 예약할지다. **Orbivisor는 커널 명령줄과 다른 호스트 서비스를 고치지 않는다** — 그것은
아래 길로 관리자가 하고, Orbivisor는 어긋남을 경고로 알린다.

**노드가 알리는 토폴로지.** Linux orbivisord가 등록할 때와 30초마다(검사 루프 여섯 회차, 노드 설정이 바뀐 회차에는 곧바로)
`/sys/devices/system/{cpu,node}`를 읽어 소켓·코어당 스레드·온라인 CPU, 호스트 NUMA 노드마다 CPU·메모리·hugepages 풀(크기마다
전체 수), 노드 사이 거리, SMT 형제 묶음, L3 캐시 묶음, 커널이 아는 페이지 크기, `isolcpus`·`nohz_full`을 노드 상태에 쓴다(바뀔
때만, 2소켓 256 CPU에서 약 1.9 KB). 빈 자원(빈 전용 CPU·빈 hugepages)은 VM을 켤 때마다 바뀌므로 노드 상태에 넣지 않고 VM 기록에서
셈한다. 커널에 NUMA가 없으면 노드 0 하나로 본다. CPU를 오프라인으로 만들거나 hugepages 수를 바꾸면 30초 안에 보인다. macOS
노드·위트니스는 토폴로지가 없다(게스트 vNUMA만 되는 노드). 모든 노드가 기능 `numa`를 알릴 때까지는 읽기만 하고 쓰지 않는다.

### 세 CPU 집합

| 집합 | 무엇이 도나 | 정하는 법 |
|---|---|---|
| **예약**(`reserved_cpus`) | 호스트 OS·orbivisord. VM 스레드는 두지 않는다 | 관리자가 준다. 비우면 없음 |
| **전용**(`dedicated_cpus`) | 전용 VM의 vCPU와 `emulator isolate`의 에뮬레이터 스레드만. 한 pCPU에 한 vCPU | 관리자가 준다. 비우면 이 노드는 전용 VM을 받지 않는다 |
| **공유**(나머지) | 공유 VM 전체와 전용 VM의 `emulator shared` 에뮬레이터 스레드 | 온라인 CPU − 예약 − 전용. 비면 안 된다 |

- 예약이 있으면 orbivisord가 자기 모든 스레드의 친화를 예약 CPU로 둔다(시작할 때와 바뀔 때, 빼면 orbivisord가 뜰 때의 친화로
  되돌림). QEMU·swtpm은 띄울 때 공유 집합(전용 VM은 에뮬레이터 집합)으로 다시 걸므로 예약 CPU에 갇히지 않는다.
- 노드 설정이 없으면 Orbivisor는 VM 스레드의 친화를 건드리지 않는다: QEMU는 orbivisord의 친화를 물려받고, 검사 루프는 고정한 적
  없는 VM을 보지 않는다. 관리자가 `orbivisord.service`의 `CPUAffinity=`나 `taskset`, 컨테이너 cpuset으로 VM을 좁혀 둔 것은 그대로다.
  설정을 지우면 Orbivisor가 고정했던 VM만 한 번 orbivisord가 뜰 때의 친화로 풀고 그 뒤로는 손대지 않는다. cgroup cpuset(컨테이너,
  슬라이스 `AllowedCPUs`)이 자른 친화는 맞은 것으로 본다.
- 공유 집합이 바뀌면 검사 루프가 도는 공유 VM의 스레드 친화와 스코프 `AllowedCPUs`를 곧바로 다시 건다(swtpm은 켤 때의 집합에
  남는다). 전용 VM은 기록된 pCPU 그대로다.
- 검증(**400** `invalid_host_resources`, `node "n1": …`): 온라인 CPU의 부분집합, 예약·전용이 겹치지 않음, 공유가 비지 않음, 예약의
  호스트 노드가 있음, 같은 (크기, 호스트 노드)를 두 번 주지 않음. 토폴로지가 없는 노드(macOS·위트니스·아직 알리지 않음)에는 줄 수
  없다.
- **바꾸기와 도는 VM.** 도는 전용 VM의 pCPU가 새 전용 집합 밖으로 나가면 **409** `host_cpus_in_use`(`details.vms`), 예약을 Orbivisor
  VM이 쓰는 hugepages 아래로 줄이면 **409** `hugepages_in_use`(`details.short`, `2M@0: 100 reserved, 128 used`)다. `--force`(웹 "그래도
  저장")면 쓰고 응답 머리 `orbivisor-warning`에 VM과 밖으로 나간 CPU를 적는다 — 그 VM은 기록된 pCPU에 그대로 있다가 다음 전원 껐다
  켜기 때 새 전용 집합에서 다시 고른다.
- `PUT /nodes/{key}`에서 `host_resources` 칸이 **없으면 그대로 둔다**(이 칸을 모르는 옛 클라이언트가 노드 설정을 조용히 지우지
  않게). 지우기는 `{}`(CLI `--clear`)다. 감사 설명 예: `node set n1 host_resources dedicated=4-31 reserved=0-1 hugepages=2M:1024@0`.

```bash
orbivisorctl node topology node1               # 토폴로지·세 집합·호스트 노드마다 사용·도는 VM의 고정·경고
orbivisorctl node topology node1 --live        # 노드에 직접 물은 빈 메모리·빈/남는 hugepages·orbivisord 친화도
orbivisorctl node resources node1              # 지금 설정(없으면 none)
orbivisorctl node resources node1 --reserved-cpus 0,16,32,48 --dedicated-cpus 4-15,20-31,36-47,52-63 \
  --hugepages 2M:4096@0,2M:4096@1             # 준 칸만 바꾼다(If-Match로 쓴다)
# node node1: dedicated=4-15,20-31,36-47,52-63 reserved=0,16,32,48 hugepages=2M:4096@0,2M:4096@1
orbivisorctl node resources node1 --dedicated-cpus 8-15,24-31,40-47,56-63 --force   # 도는 전용 VM이 있어도
orbivisorctl node resources node1 --clear      # 설정 지우기(공유 = 모든 CPU, 지금과 같다)
```

웹은 인프라 › 노드 › 노드 상세의 **"NUMA 토폴로지"**(격자·호스트 노드 표·거리·경고)와 관리자의 **"호스트 자원 설정"** 대화상자,
노드 목록의 "전용 CPU"·"Hugepages" 열이다([웹 화면 둘러보기](web-ui.md#numa-화면)). API는 `GET /api/v1/nodes/{key}/topology`·
`GET /api/v1/topology/usage`(루트 `cluster.read`), `PUT /api/v1/nodes/{key}`의 `host_resources`(`cluster.admin`, `?force=true`)다.

### hugepages: 부팅 때와 실행 중 예약

Orbivisor VM은 hugepages를 memfd hugetlb로 쓰므로 **hugetlbfs 마운트는 필요 없다**. 노드가 가진 풀(크기·호스트 노드마다 전체
수)을 스케줄러가 세고, VM을 켤 때 미리 잡는다(prealloc). 풀은 두 길로 만든다.

- **부팅 때(커널 명령줄, 1G에 권함).** 1G 페이지는 연속된 1 GiB가 있어야 해서 부팅 뒤에는 조각 때문에 모자라기 쉽다.
  `hugepagesz=1G hugepages=64`(커널이 노드에 고르게 나눔) 또는 노드마다 `hugepagesz=1G hugepages=0:32,1:32`. 2M도 이렇게 해도 된다.
- **실행 중(노드 설정, 2M에 알맞음).** `node resources --hugepages 2M:4096@0,1G:8@1`(크기:페이지 수[@호스트 노드])을 주면
  orbivisord가 시작할 때와 설정이 바뀔 때 `/sys/devices/system/node/node<N>/hugepages/hugepages-<kB>kB/nr_hugepages`(호스트 노드를
  안 정하면 `/sys/kernel/mm/hugepages/…`)에 그 수를 쓴다. 커널이 덜 잡으면 다시 시도하지 않고 경고로 남긴다(다시 하려면 설정을
  바꾸거나 orbivisord를 다시 시작). 설정에서 뺀 예약은 커널의 수를 **그대로 둔다**(다른 쓰임새일 수 있다 — 되돌리려면 직접
  `echo 0 > …/nr_hugepages`). 부팅 때 예약한 수와 같은 값이면 쓰지 않는다.
- **메모리 셈.** hugepages 풀은 보통 VM이 쓸 수 없으므로 스케줄러의 빈 메모리(순위·비우기·마이그레이션 확인)는 노드 메모리에서
  풀을 뺀 값이고, hugepages VM의 메모리는 풀에서 센다. 풀을 크게 잡으면 공유 VM이 들어갈 메모리가 그만큼 준다.

### 호스트 프로세스 떼어 두기: isolcpus와 cgroup

전용 CPU에서 VM의 vCPU만 돌게 하려면 다른 호스트 프로세스와 인터럽트를 그 CPU 밖으로 보내야 한다. Orbivisor가 하는 것과 하지
않는 것:

- **Orbivisor가 하는 것.** VM마다 systemd 스코프(`opl-vm-<id>`)에 `AllowedCPUs=`(cgroup v2 `cpuset.cpus` — 전용 VM은 자기 pCPU ∪
  에뮬레이터 CPU, 공유 VM은 공유 집합)를 걸고, QEMU의 모든 스레드에 `sched_setaffinity`를 건다(vCPU는 자리마다 pCPU 하나, 나머지는
  에뮬레이터 집합). `AllowedCPUs`는 cgroup v2에 `cpuset` 컨트롤러가 있고 `[hypervisor] systemd_scope`가 켜져 있을 때만이다(노드
  토폴로지의 `cpuset`). 없으면 친화만 건다 — root는 친화를 넓힐 수 있으므로 상한은 스코프가 쥔다. 커널 일꾼 스레드(`kvm-nx-lpage-
  recovery`·`vhost-*`, `PF_USER_WORKER` 등)는 커널이 친화를 정하므로 건드리지 않는다(스코프 cpuset 안에 있다). 검사 루프가 30초마다
  스레드 친화를 기록과 견줘 다르면 되돌린다.
- **관리자가 하는 것(권함).**
  1. **모든 서비스를 예약 ∪ 공유로**: `/etc/systemd/system.conf`의 `CPUAffinity=`(재부팅 뒤 PID 1과 모든 서비스가 물려받음).
     orbivisord는 그 안에서 다시 예약 CPU로 좁히고, QEMU는 root로 친화를 넓혀 전용 CPU에 vCPU를 둔다.
  2. **지연이 중요하면 커널에서 떼기**: `isolcpus=domain,managed_irq,<전용>`(스케줄러 부하 분산과 관리 인터럽트에서 뺌)과
     `irqaffinity=<예약>`. 선택으로 `nohz_full=<전용>`·`rcu_nocbs=<전용>`(타이머 틱·RCU 콜백 떼기). `isolcpus`의 CPU는 커널이 부하를
     옮기지 않으므로 고정한 스레드만 돌 곳에만 준다 — 공유 CPU에 주면 공유 VM의 스레드가 한 CPU에 몰린다.
  3. **`system.slice`를 묶지 않는다.** VM 스코프가 `system.slice` 안에 있으므로 `systemctl set-property system.slice AllowedCPUs=`는
     VM까지 묶는다(VM 스코프를 따로 된 슬라이스로 옮기는 것은 하지 않았다).
- **"전용 CPU가 하는 일"과 SMT.** 한 코어의 두 스레드는 실행 장치를 나눠 쓴다. 전용 집합은 형제 짝을 통째로 넣는다(나누면 경고).
  같은 코어의 형제를 다른 VM과도 나누기 싫으면 VM에 `--cpu-threads isolate`(vCPU마다 코어 하나, 형제는 비워 둠)를 준다.

### orbivisord가 보이는 경고

`node topology`의 `warning:` 줄, 노드 상세의 경고 띠에 그대로 보인다. 노드 설정이 있을 때만 셈한다.

| 경고 | 뜻과 할 일 |
|---|---|
| `host processes may run on the dedicated CPUs 4-5; see the host preparation guide` | 그 전용 CPU가 `isolcpus`에 없고 PID 1의 `Cpus_allowed_list`에 들어 있다. 위 1·2를 한다 |
| `dedicated CPUs 7 are offline` (예약도 같은 꼴) | 설정의 CPU가 오프라인이다. 켜거나 설정에서 뺀다 |
| `the dedicated CPUs split SMT sibling groups 0,32 1,33` | 전용 집합이 코어의 형제 짝을 나눈다. 짝을 함께 넣거나 함께 뺀다 |
| `node 0: 2M hugepages 4096 wanted, 3900 reserved` | 실행 중 예약이 모자란다(조각). 수를 줄이거나 부팅 때 예약한다 |
| `nr_hugepages`를 쓰지 못한 까닭, orbivisord 친화를 걸지 못한 까닭 | 권한·커널 문제. 로그와 함께 본다 |
| (`--live`) `live: orbivisord does not run on the reserved CPUs` | orbivisord의 친화가 예약 CPU와 다르다(웹 "orbivisord가 아직 예약 CPU에서 돌지 않습니다") |

### 예: 2소켓 호스트

소켓 2 × 코어 16 × 스레드 2 = 64 CPU, 메모리 512 GiB(호스트 노드마다 256 GiB). Linux 번호는 노드 0이 `0-15,32-47`, 노드 1이
`16-31,48-63`이고 CPU i와 i+32가 형제다(`node topology`로 확인한다 — 번호 매김은 기계마다 다르다).

| 쓰임 | CPU | 메모 |
|---|---|---|
| 예약 | `0,16,32,48` | 소켓마다 코어 하나(형제 포함) — 호스트·orbivisord·인터럽트 |
| 공유 | `1-3,17-19,33-35,49-51` | 소켓마다 코어 셋 — 공유 VM, 전용 VM의 에뮬레이터 |
| 전용 | `4-15,20-31,36-47,52-63` | 소켓마다 코어 열둘(형제 포함, 48 CPU) |
| hugepages | 1G 64장(노드마다 32), 2M 8192장(노드마다 4096) | 1G는 부팅 때, 2M은 실행 중. 보통 메모리로 남는 것은 노드마다 약 216 GiB |

```bash
# 1) 커널 명령줄(/etc/default/grub의 GRUB_CMDLINE_LINUX에 더한 뒤 update-grub, 재부팅)
#    hugepagesz=1G hugepages=0:32,1:32 isolcpus=domain,managed_irq,4-15,20-31,36-47,52-63 irqaffinity=0,16,32,48
# 2) 모든 서비스를 예약 ∪ 공유로(/etc/systemd/system.conf, 재부팅 뒤)
#    CPUAffinity=0-3 16-19 32-35 48-51
# 3) Orbivisor 노드 설정
orbivisorctl node resources node1 --reserved-cpus 0,16,32,48 --dedicated-cpus 4-15,20-31,36-47,52-63 --hugepages 2M:4096@0,2M:4096@1
orbivisorctl node topology node1
# cpus: 0-63 (2 socket(s), 2 thread(s) per core)
# reserved: 0,16,32,48  dedicated: 4-15,20-31,36-47,52-63  shared: 1-3,17-19,33-35,49-51
# isolcpus: 4-15,20-31,36-47,52-63
# HOST-NODE  CPUS         MEMORY   DEDICATED(USED/ALL)  HUGEPAGES(USED/ALL)
# 0          0-15,32-47   256 GiB  -/4-15,36-47         1G 0/32, 2M 0/4096
# 1          16-31,48-63  256 GiB  -/20-31,52-63        1G 0/32, 2M 0/4096
# distances: 10 21 | 21 10
```

이 노드에는 전용 VM의 vCPU가 노드 하나에 24개까지(`prefer`), `isolate`면 코어 단위라 12개까지 들어간다. 게스트 노드가 둘인 VM은
두 호스트 노드에 하나씩 놓인다.

### OpenStack·Proxmox VE와 견주기

| | OpenStack Nova | Proxmox VE 8 | Orbivisor |
|---|---|---|---|
| 노드 CPU 나누기 | `nova.conf` `[compute] cpu_dedicated_set`·`cpu_shared_set` | 없음 | 노드 설정 `reserved_cpus`·`dedicated_cpus`, 공유는 나머지 |
| VM CPU 정책 | flavor `hw:cpu_policy=shared\|dedicated\|mixed` | 없음(`affinity`: QEMU 프로세스 전체의 CPU 목록) | `cpu.policy` `shared`·`dedicated`(mixed 없음) |
| 스레드·에뮬레이터 | `hw:cpu_thread_policy=prefer\|isolate\|require`, `hw:emulator_threads_policy=share\|isolate` | 없음 | `--cpu-threads` 같은 셋, `--emulator-threads shared\|isolate` |
| vNUMA | `hw:numa_nodes`, `hw:numa_cpus.N`·`hw:numa_mem.N` | `numa: 1`, `numaN: cpus=,hostnodes=,memory=,policy=` | `--numa auto\|N\|CPUS:MIB,…`(노드는 소켓 단위) |
| 메모리 묶기 | NUMA VM은 늘 strict | `policy=bind\|preferred\|interleave` | `--numa-memory strict\|preferred`(전용·hugepages는 strict) |
| hugepages | `hw:mem_page_size=small\|large\|any\|2MB\|1GB`, 커널 명령줄로 예약 | `hugepages: 2\|1024\|any`, 켤 때 노드마다 할당·끌 때 반납 | `--hugepages 2M\|1G\|any`, 커널 명령줄이나 노드 설정의 실행 중 예약 |
| 자원 세기 | placement `PCPU`·`VCPU` + NUMATopologyFilter | 없음(사람이 고름) | Raft 받아들이기 검사 + 스케줄러 NUMA 맞춤 + 노드 할당기 |
| 마이그레이션 | 대상에서 고정을 다시 셈 | 같은 설정 그대로 | 게스트 모양은 기록 그대로, 대상이 pCPU를 다시 고름 |

- OpenStack에서 오면: `cpu_dedicated_set` → `--dedicated-cpus`, `cpu_shared_set` → 남는 것(예약을 뺀), flavor 속성 → VM 플래그(값 이름이
  거의 같다 — 에뮬레이터의 `share`는 `shared`). 호스트 예약 메모리(`reserved_host_memory_mb`)에 해당하는 것은 없다.
- Proxmox VE에서 오면: `affinity`는 QEMU 프로세스 전체를 한 CPU 목록에 두는 것이라 vCPU마다 고정하지 않고 다른 VM도 막지 않는다 —
  가장 가까운 것은 `dedicated`다. `hugepages`는 켤 때 할당하지 않고 노드 풀에서 쓴다(1G를 켤 때 할당하면 조각으로 실패하기 쉽고,
  풀이어야 스케줄러가 셀 수 있다). `numaN: hostnodes=`처럼 호스트 노드를 손으로 고르지 않는다 — 켜는 노드가 고른다.

- **한계.** 커널 명령줄·`CPUAffinity=`·IRQ 친화를 Orbivisor가 쓰지 않는다(보고만). VM 스코프를 따로 된 슬라이스로 옮겨 호스트를
  cgroup으로 떼는 것, 호스트 예약 메모리, HA 용량 예약의 전용 CPU 셈, 노드 안 조각 모음(전용 VM 다시 고정), 메모리 대역폭·캐시
  나누기(Intel RDT·AMD PQoS), 자동 NUMA 균형(`numa_balancing`) 상태 보이기는 없다. aarch64 64K 커널의 512M 풀은 세지 않는다(2M·1G만).

## 지표

노드마다 5초마다 그 노드와 그 노드에서 도는 VM의 사용량을 재어 메모리에 1시간(시리즈마다 720점) 두고, 분마다
`{data_dir}/metrics`의 **기록 파일**(노드 로컬, 크기가 정해진 RRD)에 옮겨 1년까지 남긴다(Proxmox VE의 RRD 그래프,
vSphere의 성능 차트와 같은 자리). Raft에는 쓰지 않는다. 다른 노드의 지표는 읽을 때 노드 사이 RPC로 묻는다(감사 로그와
같은 방식). 측정기는 이 값으로 부하 요약(1분·5분 CPU, 메모리, VM마다 CPU·RSS)도 만들어 리스 갱신으로 리더에 보낸다(위
"최적 배치"). 기록 조회·클러스터 요약·Prometheus 설정은 모든 노드가 이 버전이어야 한다(노드가 알리는 기능
`metrics-history` — 옛 노드의 기록 조회와 `prometheus` 설정 바꾸기는 **409** `cluster_upgrading`, 클러스터 요약에서는
`unsupported`). 설계는 [지표 기록·대시보드·알림·Prometheus 설계](../superpowers/specs/2026-11-18-dashboard-metrics-notifications-design.md).

### 측정

```bash
orbivisorctl node stats            # 노드마다 마지막 점
# NODE   TIME                  CPU    MEMORY          DISK READ  DISK WRITE  NET RX     NET TX    DATA FS
# node1  2026-11-11T06:30:05Z  12.3%  6.1GiB/31.2GiB  0B/s       1.2MiB/s    15.4KiB/s  3.1KiB/s  41.0GiB/468.0GiB
orbivisorctl vm stats web          # VM의 마지막 점(마이그레이션 앞뒤 노드의 점을 합친 것)
# TIME                  CPU    MEMORY  DISK READ  DISK WRITE  READ IOPS  WRITE IOPS  NET RX    NET TX
# 2026-11-11T06:30:05Z  48.7%  1.1GiB  0B/s       512.0KiB/s  0.0        12.4        2.0KiB/s  1.1KiB/s
orbivisorctl node stats --watch    # 5초마다 새 점만 다시 읽어 새 표를 이어 찍는다(Ctrl-C로 끝냄, vm stats도)
orbivisorctl -o json vm stats web  # 응답 그대로(메모리에 남은 모든 점)
orbivisorctl -o json node stats --watch  # 회차마다 응답 한 줄(JSON Lines)
```

- **노드 값.** CPU %(모든 코어를 합해 100), 쓰는 메모리/전체(Linux `MemTotal − MemAvailable`, macOS active + wired +
  compressor), 실제 디스크의 읽기·쓰기 바이트/초(Linux `/proc/diskstats`에서 파티션과 `loop`·`ram`·`zram`·`dm-`·`md`·`nbd`·
  `zd`·`rbd` 장치를 뺀 합 — LVM·RAID·ZFS 위의 쓰기를 두 번 세지 않는다, macOS는 없음), 실제 NIC의 받기·보내기
  바이트/초(Linux는 `/sys/class/net/<if>/device`가 있는 장치, macOS는 `en*`), 데이터 디렉터리(`data_dir`)의 파일 시스템
  사용/전체.
- **인터페이스 값(계획 75).** 노드 점에 인터페이스마다 받기·보내기 바이트/초와 직전 점 뒤 늘어난 오류·버림 수가 붙는다
  (`interfaces: [{name, rx_bps, tx_bps, rx_errors, tx_errors, rx_dropped, tx_dropped}]`, `/sys/class/net/<if>/statistics`). 대상은 노드
  상태의 이더넷·본딩·VLAN·브리지(veth·tap·dummy 아님), 노드당 64개까지다. **마지막 점에만** 있고(앞 점의 것은 새 점이 오면 지움)
  기록 파일·클러스터 요약에는 들지 않는다 — 홈 자세히 판의 포트 트래픽이 이것을 읽는다.
- **VM 값.** CPU %는 QEMU 프로세스의 CPU 시간을 vCPU 수(sockets × cores)로 나눈 것이라 vCPU가 모두 바쁘면 100이고
  I/O 스레드 때문에 조금 넘을 수 있다. 메모리는 QEMU 프로세스의 RSS(게스트가 쓰는 양이 아님), 디스크는 QMP
  `query-blockstats`의 모든 VM 디스크 합(바이트/초와 IOPS), 네트워크는 tap NIC 카운터의 합을 게스트 기준으로 바꾼
  것이다(받기 = tap의 보내기). 사용자 모드 NIC(`user`)는 tap이 없어 세지 않고, tap NIC가 없으면(macOS) 네트워크는
  없다. `Running`·`Paused` VM만 잰다.
- **풀 값.** 그 노드에서 준비된(`ready`) 풀의 쓴 양·전체를 분마다 기록한다(풀 마운터가 마지막으로 잰 값). 공유 풀은
  여러 노드가 같은 값을 따로 기록한다.
- **비율과 빈 값.** 비율은 직전 측정과의 차이를 걸린 시간(단조 시계)으로 나눈다. 첫 측정(orbivisord 시작, VM 시작·붙기,
  QEMU pid 바뀜, 카운터가 줄어듦)은 기준만 잡으므로 VM의 첫 점은 켠 뒤 5~10초에 생긴다. 읽지 못한 값은 `null`(표는
  `-`)이다. `query-blockstats`가 1초 안에 답하지 않으면 그 VM의 디스크 값만 `null`이고, 실패 경고는 VM마다 10분에 한 번
  남는다.
- **마지막 점 API.** `GET /api/v1/metrics/nodes?since=`(모든 노드, `{"nodes":[{"node","samples"}],"unreachable","unsupported"}`,
  노드 이름 순), `GET /api/v1/nodes/{key}/metrics?since=`(노드 하나, `{"node","samples"}`), `GET /api/v1/vms/{key}/metrics?since=`
  (`{"vm","samples","unreachable","unsupported"}` — 위트니스를 뺀 모든 노드에 물어 시각 순으로 합친다). 노드 읽기는 노드
  읽기 권한, VM 읽기는 VM 읽기 권한(viewer도 된다)이다. `since`는 그 노드 시계의 유닉스 밀리초 `t`이고 그보다 뒤의
  점만 온다(숫자가 아니면 **400** `invalid_request`). 점의 `t`는 그 노드의 벽시계라 노드 사이 시계 차이가 그대로 보인다.
  다른 노드는 노드 사이 RPC로 3초까지 기다리고, 꺼진 것으로 기록된 노드는 묻지 않는다. 답하지 않거나 꺼진 노드는
  `unreachable`, 지표를 모르는 옛 orbivisord는 `unsupported`에 이름이 들어간다(orbivisorctl은 표 뒤 stderr에
  `warning: no metrics from: …`, `note: these nodes do not report metrics yet: …`를 쓴다). 노드 하나 읽기는 그 노드가
  꺼졌거나(`node "…" is offline`) 닿지 않으면 **503** `node_unreachable`, 옛 노드면 **409** `cluster_upgrading`,
  위트니스면 **409** `node_unavailable`이다. 노드 기능 목록(`status.features`)에 `metrics`가 있다.

### 기록 파일

분마다(`[metrics] flush_secs`) 메모리의 지난 점을 시리즈마다 파일 하나에 옮긴다. 위트니스는 기록하지 않는다.

| 파일 | 내용 | 크기 |
|---|---|---|
| `{data_dir}/metrics/node.rrd` | 이 노드(9칸) | 376 KiB |
| `{data_dir}/metrics/vm/{VM id}.rrd` | 이 노드가 주인인 VM(8칸) | 336 KiB |
| `{data_dir}/metrics/pool/{풀 id}.rrd` | 이 노드에서 준비된 풀(2칸, 풀 id라서 이름을 바꿔도 이어진다) | 92 KiB |

| 층 | 간격 × 칸 | 기간 | 칸마다 |
|---|---|---|---|
| `raw5s` | 5초 × 720 | 1시간 | 값 하나 |
| `min1` | 1분 × 1440 | 1일 | 평균·최대·최소 |
| `min30` | 30분 × 336 | 1주 | 평균·최대·최소 |
| `hour6` | 6시간 × 1460 | 1년 | 평균·최대·최소 |

- **크기가 늘지 않는다.** 파일은 만들 때 전체 크기로 만들고(모르는 값 NaN으로 채움) 그 뒤로는 제자리에서 덮어 쓴다. 칸
  자리는 시각으로 정하므로(칸 번호 = 유닉스 시각 ÷ 간격, 자리 = 번호 mod 칸 수) 모든 노드의 칸 경계가 같다. VM이 100개인
  노드는 약 34 MB다. 디렉터리는 0700, 파일은 0600이다.
- **합치기.** `min1` 칸은 그 분의 5초 점 12개로 만들고, 아는 값이 절반 미만이면 그 칸은 모름이다. `min30`·`hour6` 칸은 닫힐
  때 `min1` 층에서 다시 읽어 만든다(따로 누산기가 없어 재시작 중간에 잃는 것이 없다). 평균은 아는 칸 수로 가중하고, 최대는
  최대의 최대, 최소는 최소의 최소다.
- **빈 칸.** 노드가 꺼졌거나 orbivisord가 멈췄던 구간, 읽지 못한 값은 모름(NaN)으로 남고 API·CLI는 `null`, 차트는 끊어
  그린다. 0과 다르다.
- **전원이 나가도.** 쓰기 순서는 (1) 모든 파일에 새 칸 → (2) 모두 `fdatasync` → (3) 머리(512 B 사본 둘, 번갈아 쓰고 순번과
  SHA-256 검사합을 담음) → 다시 `fdatasync`다. 머리는 데이터가 디스크에 닿은 뒤에만 그 칸을 가리키므로 잃는 것은 마지막
  쓰기 주기(기본 1분)뿐이다. 다만 새 칸은 가장 오래된 칸 자리를 덮으므로, 머리를 쓰기 전에 끊기면 다시 뜬 뒤 층의 가장
  오래된 끝(`raw5s`면 약 1시간 전)에 새 값이 잠깐 보이다가 다음 쓰기가 지운다. 머리 사본이 둘 다 깨졌으면
  `{name}.rrd.corrupt`로 옮기고 새로 만든다(경고).
- **다시 시작.** orbivisord가 뜨면 파일의 `raw5s` 층(지난 1시간)을 메모리로 되돌려 넣는다. 그래서 1시간 차트와 정확한
  p50·p95가 재시작 뒤에도 이어지고, 멈췄던 동안은 `null`이다. 그동안 닫혔어야 할 `min1`·`min30`·`hour6` 칸은 남은 고운
  층에서 메운다. 멈출 때(`systemctl stop`)는 마지막으로 한 번 더 쓴다.
- **시계가 뛰면.** 점의 시각은 노드 벽시계다. 앞으로 뛰면 공백과 같다. 1시간 미만 뒤로 가면 이미 쓴 칸 앞의 점은 파일에
  쓰지 않고(경고 한 번, 메모리 1시간 기록에는 보임) 시계가 따라잡으면 이어 쓴다. 1시간 넘게 뒤로 가면(RTC가 틀렸던
  부팅으로 미래 시각에 쓴 칸) 그 미래 칸을 층마다 지우고 지금부터 다시 쓴다(경고). NTP를 쓰기를 권한다.
- **VM을 따라간다.** VM 기록의 주인은 VM이 도는 노드(`status.node`, 없으면 `spec.node`)다. 각 노드는 10분마다, 그리고 VM이
  이 노드를 떠난 것을 보면 30초 뒤에 자기 `vm/*.rrd`를 훑어, 주인이 다른 노드인 VM의 파일을 그 노드로 보낸다(파일 하나,
  노드 사이 RPC). 받은 노드는 칸 단위로 합치고(자기 칸이 모름일 때만 받은 값을 씀 — 마이그레이션 중 두 노드가 함께 잰
  몇 초를 두 번 세지 않는다) 디스크에 쓴 뒤에 답하고, 그 뒤에만 보낸 노드가 지운다. 받는 노드가 꺼졌거나 옛 버전이거나
  기록을 껐으면 파일을 두고 다음 차례에 다시 한다. 받는 노드가 파일을 받을 수 없다고 거절하면(`Invalid`, 예: 다른 기록
  형식) 거절할 때마다 두 배로(10분부터 하루까지) 늦춰 다시 한다. 보내기는 8개씩 함께 하고 한 차례에 2분까지만 기다리며(남은
  것은 다음 차례), 정리는 기록 쓰기와 따로 돌아 답하지 않는 노드가 분마다 쓰기를 막지 않는다. 보내는 동안 그 파일에 새로
  쓰거나 합친 것이 있으면 지우지 않고 다음 차례에 다시 보낸다. 아직 이 노드에서 재는(마지막 점이 30초 안인) VM은 보내지 않는다.
  라이브·꺼진 마이그레이션, 비우기, HA 복구 뒤 옛 노드가 돌아온 경우, 정해진 노드 바꾸기가 모두 같은 길이다.
- **지우기.** Raft에 없는 VM·풀의 파일은 계획 59의 상태 디렉터리 정리와 같은 규칙으로 지운다: 두 차례 연속 후보이고,
  리더 확인(`read_index`)이 성공하고, 그 id를 대상으로 하는 끝나지 않은 Task가 없을 때다. `node.rrd`는 지우지 않는다. 옆으로
  옮긴 파일(`.corrupt`·`.v{n}`)과 남은 임시 파일(`.tmp`)은 30일이 지나면 지운다(기록 파일 이름 옆의 것만).
- **형식 버전.** 머리에 형식 버전과 칸 목록이 있어, 다음 버전이 칸을 더하면 열 때 새 배치로 옮긴다(같은 칸은 그대로).
  더 새 형식의 파일(orbivisord를 내려 깐 경우)은 `{name}.rrd.v{n}`으로 옮기고 새로 시작한다 — 내려 깔면 기록을 잃는다.

**설정.** `/etc/orbivisor/orbivisord.toml`(노드마다):

```toml
[metrics]
history = true    # false면 파일을 쓰지 않고 메모리 1시간만(조회는 1시간까지만 값이 있다)
flush_secs = 60   # 파일에 옮기는 주기(60~600초). 전원이 나가면 이만큼 잃는다
```

- **디스크 닳음.** VM 파일 하나는 쓰기 주기마다 약 3 × 4 KiB 페이지와 `fdatasync` 둘을 쓴다. VM 100개인 노드는 하루 약
  1.7 GB, 해마다 약 0.6 TB로 보통 SSD(수백 TBW)에는 작고, 파일 크기가 바뀌지 않아 파일 시스템 저널 쓰기는 거의 없다.
  **SD 카드·USB 메모리로 부팅한 노드**는 `flush_secs = 600`(쓰기 약 1/8, 대가로 전원이 나가면 10분을 잃음)이나
  `history = false`를 쓴다. `data_dir`을 SSD에 두면 걱정할 일이 없다.
- **기록을 끈 노드로 옮긴 VM.** 옮기기 전 노드는 그 VM 파일을 지우지 않고 두며, 받는 쪽이 거절할 때마다 10분부터 하루까지
  두 배로 늦춰 다시 보낸다. 나중에 그 노드에서 기록을 켜면 늦어도 하루 안에 넘어간다.

### 기록 조회

```bash
orbivisorctl node stats node1 --range day     # 노드 하나의 하루 요약(--range 없으면 지난 1시간)
# 2026-11-17T09:00:00Z .. 2026-11-18T09:00:00Z (tier min1, step 5m, 288 points)
# FIELD       AVG        MAX        MIN       P50~       P95~
# cpu         12.4%      88.0%      1.2%      9.8%       31.5%
# mem_used    6.1GiB     7.9GiB     5.8GiB    6.0GiB     7.2GiB
# ...
orbivisorctl vm stats web --range week        # VM(주인 노드에 묻는다)
orbivisorctl pool stats nas --range month     # 풀(그 풀이 준비된 노드 하나에 묻는다)
orbivisorctl vm stats web --from 2h           # 2시간 전부터 지금까지. --from·--until은 2h·7d, 유닉스 ms, RFC 3339
orbivisorctl vm stats web --from 2026-11-18T01:00:00Z --until 2026-11-18T03:00:00Z --points 120
orbivisorctl -o json node stats node1 --range year   # 응답 그대로(열 배열)
```

- **범위.** `--range hour|day|week|month|year`(1개월은 30일, 1년은 365일, 지금에서 끝남)이거나 `--from`(+`--until`, 없으면
  지금)이다. `--until` 없는 `--from 1h|24h|1d|7d|30d|365d`는 같은 길이의 `--range`로 보내 서버 시계로 세고, 그 밖의 기간은
  `--until`도 CLI 시계의 지금으로 보낸다(두 시계가 달라도 범위 길이는 그대로). `-2h`처럼 부호가 붙은 기간은 받지 않는다. 범위는 366일까지, `--points`는 1~1000(기본 300)이다. 범위를 주지 않은 `vm stats`·`node stats`(노드 없이)는
  지금처럼 마지막 점이고, `--watch`는 범위와 함께 쓰지 않는다.
- **층 고르기.** 범위가 그 층의 기간 안에 드는 가장 고운 층을 고른다(1시간 → `raw5s`, 1일 → `min1`, 1주 → `min30`, 그 밖 →
  `hour6`, 시작이 층에 남은 기간보다 이르면 한 층 거칠게). 그 뒤 칸을 `points` 안으로 묶는다(평균은 아는 칸의 평균, 최대의
  최대, 최소의 최소). 표의 첫 줄이 고른 층과 한 점의 간격이다.
- **요약 값.** 칸마다 범위 전체의 평균·최대·최소(점 수와 상관없이 고른 층 전체로 셈)와 p50·p95다.
  - **지난 1시간 안의 범위**는 5초 원본으로 셈한 **정확한** 백분위다(`approx: false`, 표 머리 `P50`·`P95`).
  - **더 긴 범위**는 고른 층의 칸 평균으로 셈한 **근사**다(`approx: true`, 표 머리 `P50~`·`P95~`, 웹 `p95 ≈`). 평균으로 셈하므로
    1분보다 짧은 봉우리를 낮게 본다 — 짧은 봉우리는 MAX를 본다.
  - 재시작·마이그레이션 직후처럼 앞쪽 기록이 없으면 `covered_from`이 아는 첫 칸이다(표 끝 `note: no history before …`).
- **API.** `GET /api/v1/nodes/{key}/metrics/history`, `/vms/{key}/metrics/history`, `/storage-pools/{key}/metrics/history`
  (`range=` 또는 `from=`·`until=` 유닉스 ms, `points=`, 둘 다 없으면 지난 1시간 — 그 종류의 읽기 권한). 응답은
  `{"tier","step_ms","start","columns":{"cpu":{"avg":[…],"max":[…],"min":[…]},…},"stats":{…},"pct":{"cpu":{"p50","p95","approx"}},"covered_from"?}`
  이고 점 `i`의 시각은 `start + i × step_ms`, 모름은 `null`이다. 단위는 CPU %, 메모리·파일 시스템·RSS·풀 MiB, 디스크·
  네트워크 바이트/초, IOPS /초다. 노드 기록은 그 노드에, VM 기록은 VM의 주인 노드 **하나에만**, 풀 기록은 그 풀이 준비된
  노드(이 노드 먼저, 그다음 이름 순) 하나에 5초까지 묻는다.
- **오류.** **400** `invalid_request`(`range`와 `from`을 함께, `until`만, 순서가 틀림, 366일 넘음, `points` 범위 밖, 모르는
  매개변수), **404** `not_found`, **409** `cluster_upgrading`(그 노드가 옛 버전 — `node "x" does not keep metrics history yet;
  upgrade it first`), **409** `node_unavailable`(위트니스의 노드 기록), **409** `pool_unavailable`(풀이 준비된 노드가 없음),
  **503** `node_unreachable`(꺼졌거나 5초 안에 답하지 않음 — `node "x" did not answer: …`).

### 클러스터 요약

```bash
orbivisorctl node stats --range day           # 노드 이름 없이 범위를 주면 클러스터 요약
# cluster (3 nodes): 2026-11-17T09:00:00Z .. 2026-11-18T09:00:00Z (tier min1, step 12m, 120 points)
# FIELD       AVG      PEAK
# cpu         18.2%    61.0%
# ...
# NODE    CPUS  CPU    MEMORY           DISK READ  DISK WRITE  NET RX    NET TX
# node1   16    22.1%  24.0GiB/62.0GiB  1.2MiB/s   3.4MiB/s    2.1MiB/s  1.8MiB/s
# ...
# TOP VM  NODE    CPU    MEMORY
# db      node2   71.3%  15.8GiB
# ...
# POOL    KIND    USED     TOTAL    USE
# nas     shared  1.2TiB   4.0TiB   30.0%
```

- `GET /api/v1/metrics/cluster?range=&points=`(기본 120점, 노드 읽기 권한)은 요청을 받은 노드가 위트니스를 뺀 온라인 노드에
  32개씩 함께 묻는다(노드마다 자기 노드 열의 평균과 범위 평균 CPU 상위 VM 10개, 약 5 KB). 리더를 거치지 않는다.
- **클러스터 열**은 노드 열을 칸끼리 더한 것이다: CPU는 코어 수로 가중한 %, 나머지(메모리·디스크·네트워크·파일 시스템)는
  합이다. 최대·최소 열은 없으므로 CLI의 `PEAK`와 웹 범례의 최대·최소는 그 평균 열의 최대·최소다.
- **노드 줄**은 노드마다 범위 평균이고, 20개가 넘으면 범위 평균 CPU 상위 20개다(`node_count`는 더한 노드 수). **상위 VM**은
  모든 노드의 후보를 합친 10개(지운 VM은 뺌), **풀**은 Raft의 지금 값이다(공유 풀은 한 노드의 값, 노드 로컬 풀은 준비된
  노드의 합).
- 같은 조건의 요약은 그 API 노드에서 10초 동안 다시 쓰고, 한 API 노드의 요약은 한 번에 하나만 모은다(홈 대시보드를 연
  사람이 많아도 노드에 묻는 수가 늘지 않는다).
- 답하지 않은 노드는 `unreachable`, 옛 노드는 `unsupported`, 시계가 크게 어긋나 다른 칸 경계로 답한 노드는 `skewed`에 이름이
  들어가고 열·줄에서 빠진다(CLI는 stderr `warning: no metrics history from: …`, `note: these nodes do not keep metrics
  history yet: …`, `warning: left out because their clock is far off: …`). 요약 자체는 노드 문제로 실패하지 않는다.

### 용량 요약

홈의 노드별 할당·사용 표(계획 75, [운영자 대시보드](web-ui.md#운영자-대시보드))가 읽는 값이다. 지표 기록과 달리 Raft의 지금 상태와 배치
부하 요약(리더가 리스로 받은 것)으로 셈한다.

```bash
orbivisorctl capacity
# NODE   ONLINE  SHARED  OVERCOMMIT  DEDICATED  CPU    MEMORY           MEM USED  STORAGE
# svc01  yes     18/10   x1.80       2/4        37.2%  32.0GiB/64.0GiB  16.0GiB   1.0TiB/2.0TiB
# svc02  no      0/14    -           0/0        -      0B/62.0GiB       -         -
#
# POOL  PROVISIONED  USED    TOTAL   VOLUMES  NODES
# ceph  2.1TiB       1.2TiB  4.0TiB  41       2/3
orbivisorctl -o json capacity    # 응답 그대로(NUMA 노드별·hugepages·풀마다)
```

- `SHARED`는 공유 정책 VM의 vCPU / 공유 스레드(스레드 − 예약 − 전용, [호스트 준비](#호스트-준비-예약전용-cpu와-hugepages)), `OVERCOMMIT`은 그 비,
  `DEDICATED`는 도는 전용 VM이 잡은 전용 CPU / 전용 CPU, `CPU`는 지난 1분 노드 전체(모든 스레드) 사용률, `MEMORY`는 자리 잡은 VM(시작·실행·정지 중·옮기는 중)의 메모리 합 / 노드 메모리, `STORAGE`는 로컬 풀의
  프로비전(볼륨 `size_bytes` 합 — 씬 할당) / 전체 합이다. 아래 표는 공유 풀(노드 = 준비된 노드 / 쓰는 노드)이다.
- `GET /api/v1/capacity`(루트 `cluster.read`)는 `{"nodes":[{"node","online","cpu":{threads,reserved,dedicated_total,dedicated_used,
  vcpus_allocated,vcpus_running,vcpus_shared,overcommit?,use_pct?},"memory":{total_mib,reserved_mib,allocated_mib,allocated_hugepages_mib?,
  used_mib?,hugepages,overcommit?},"numa":[…],"storage":[{pool,ready,total_bytes,used_bytes,provisioned_bytes,volumes}]}],"shared_pools":[…],"totals":{…}}`다.
  위트니스는 빼고 오프라인 노드는 넣는다(`totals`는 온라인 노드의 합). 부하 요약이 없으면 `use_pct`·`used_mib`가 빈다. 메모리 `overcommit`은
  보통 메모리의 비다: hugepages 아닌 VM의 할당(`allocated_mib − allocated_hugepages_mib`) ÷ (노드 메모리 − hugepages로 떼어 둔 것) — 보통 VM은
  그 풀을 쓰지 못한다.
- 입력(상태 머신 리비전과 노드 부하)이 같으면 API 노드가 이전 결과를 그대로 준다. 온라인 노드가 모두 기능 `operator-dashboard`를 알아야
  한다(아니면 **409** `cluster_upgrading` — 웹은 지난 홈). 꺼진 옛 노드는 막지 않는다.

### 웹 화면

- **홈.** 건강 띠(노드·VM 수, 울리는 경보) 아래에 운영자 대시보드(지도·자세히 판·노드별 할당·사용 표 — 계획 75), 활성 경보 위젯
  (심각도 순, 30초마다, "모든 경보 보기")·상위 VM(섞인 판이면 지난 홈의 용량·풀 사용률 막대 — 85 %를 넘으면 경고색), **클러스터 부하** 차트 넷(CPU·메모리·디스크·네트워크, 범위 고르기 1시간·1일·1주·1개월·1년),
  **노드 부하**(범위 평균, 20개 넘으면 가장 바쁜 20개)와 **상위 VM** 10개가 있다. 범위를 바꾸면 노드 부하·상위 VM도 그
  범위가 된다(노드 부하는 지난 홈에만). 넣지 못한 노드는 차트 아래 한 줄로 알린다(답하지 않음·옛 버전·시계가 어긋남). 홈의 배치는
  [웹 화면 둘러보기](web-ui.md#홈과-첫-걸음)에 있다.
- **노드 상세**(인프라 › 노드, `/nodes/{이름}`, 노드 목록·노드 부하 위젯의 이름을 누름). 머리(상태, "유지보수"·"비우기" 단추,
  나머지 업데이트·인터페이스·디스크·레이블·펜싱·지우기는 동작 메뉴 ⋯), 요약 카드,
  차트 넷(범위 고르기), 그 노드의 VM 표다. 위트니스는 기록이 없다고 안내한다.
- **VM "지표" 탭**(`/vms/{이름}?tab=metrics`, 상위 VM 위젯의 이름을 누름). CPU·메모리(RSS)·디스크(읽기·쓰기)·네트워크 차트
  넷이다. IOPS는 차트에 없고 CLI·API와 경보 규칙에서 쓴다.
- **차트.** 평균 선과 최대~최소 사이의 옅은 띠를 그리고, 모르는 칸은 끊어 그린다. 범례 표에 칸마다 평균·최대·최소와
  (있으면) p50·p95를 둔다 — 근사면 `p50 ≈`·`p95 ≈`와 "평균값으로 셈해 짧은 봉우리를 낮게 봅니다"를 적고, 앞쪽 기록이 없으면
  "… 전의 기록은 없습니다"를 적는다. 1시간 범위는 5초마다, 그 밖은 60초마다 다시 읽는다. 차트마다 요약을 담은 접근성
  이름과 "표로 보기"(같은 값의 표)가 있고, 밝음·어두움 테마를 따른다. 범위는 주소의 `?range=`에 남는다.

### 한계

- 측정 간격(5초)과 층(5초·1분·30분·6시간)은 바꿀 수 없다. 디스크·NIC마다 나눈 값의 기록(노드 인터페이스는 마지막 점뿐), 게스트 안의 지표(파일 시스템 사용량
  등), 기록 내보내기(CSV), 지표 밀어 보내기(Graphite·InfluxDB), 사용자 정의 대시보드는 없다.
- 1시간보다 긴 범위의 p50·p95는 평균으로 셈한 근사다(분 단위보다 짧은 봉우리를 낮게 봄).
- 마이그레이션 직후 기록이 새 주인 노드로 넘어가기 전(최대 약 10분)에는 기록 조회의 앞쪽이 비어 보인다(`covered_from`).
  그동안 지난 1시간은 마지막 점 API(`vm stats`, 모든 노드에 묻는다)로 이어 보인다.
- 꺼진 노드에 있는 기록은 읽을 수 없다. 죽은 노드에서 HA로 복구한 VM의 옛 기록은 그 노드가 돌아와야 새 주인에게 넘어가고,
  돌아오지 않으면 잃는다. 지운 노드의 노드 기록은 함께 사라진다.
- orbivisord를 내려 깔면(더 옛 형식) 기록 파일을 옆으로 옮기고 새로 시작한다.
- 공유 풀은 그 풀을 쓰는 노드마다 같은 기록을 따로 둔다(파일 92 KiB). 노드 로컬 풀의 기록 조회는 노드 하나의 것이다(노드를
  고르는 매개변수가 없다).
- vCPU 수와 NIC 목록은 지금 VM 스펙에서 읽으므로, 다시 시작해야 적용되는 변경(코어 수, NIC 더하기) 뒤 VM을 다시 시작하기
  전까지는 CPU %가 새 vCPU 수로 나뉘고 네트워크가 `null`일 수 있다.

## 알림

**경보 규칙**(무엇을 언제)과 **알림 대상**(어디로)을 따로 만든다. 규칙이 참인 채 정한 시간이 지나면 그 규칙의 대상들로
알림을 보내고, 풀리면 해제 알림을 보낸다(Proxmox VE의 알림 대상·매처, vSphere의 알람·동작과 같은 자리). 모든 노드가 이
버전이어야 한다(노드가 알리는 기능 `notifications` — 아니면 규칙·대상 만들기·바꾸기·지우기는 **409** `cluster_upgrading`,
목록에 노드 이름). 옛 노드가 섞인 동안 그 노드의 노드 경보는 비어 보인다.

### 알림 대상

```bash
# 메일(SMTP). 비밀번호는 묻는다(터미널이 아니면 표준 입력에서 한 줄)
orbivisorctl notify target create ops-mail smtp --host smtp.example.com --port 587 --tls starttls \
  --user alerts@example.com --password --from alerts@example.com --to ops@example.com --to oncall@example.com
# Slack(들어오는 webhook URL 자체가 비밀이므로 --url -로 묻는다)
orbivisorctl notify target create ops-slack webhook --format slack --url -
# ntfy·Gotify(토큰을 묻는다), 일반 JSON(헤더 값을 묻는다), 사설 CA
orbivisorctl notify target create phone webhook --format ntfy --url https://ntfy.example.com/orbivisor --token
orbivisorctl notify target create hook webhook --url https://hooks.example.com/orbivisor --header X-Api-Key --ca-file corp-ca.pem
orbivisorctl notify target test ops-mail      # 시험 알림을 한 번(꺼진 대상도) — 실패하면 오류로 끝난다
orbivisorctl notify target list               # NAME KIND ENABLED DESTINATION
orbivisorctl notify target get ops-slack      # 비밀은 있는지만(has_password·has_token·헤더 이름), URL은 줄여서
orbivisorctl notify target set ops-mail --to ops@example.com --disable
orbivisorctl notify target delete ops-mail    # 쓰는 규칙이 있으면 409 in_use(규칙 이름)
```

- **SMTP.** `--tls starttls`(기본, `--port` 기본 587)·`tls`(보통 `--port 465`)·`none`(127.0.0.1의 로컬 중계용 평문)이다. 서버 인증서는 OS 신뢰 저장소로
  확인한다. `--user`가 없으면 인증하지 않는다. 받는 주소는 20개까지, 본문은 일반 텍스트, 제목은
  `[Orbivisor {클러스터}] FIRING critical: {규칙} on {대상}`(해제는 `RESOLVED`, 이벤트는 `EVENT`, 시험은 `TEST`)이다.
- **webhook.** HTTPS(또는 HTTP) POST다. `--ca-file`이 있으면 그 CA만 믿고, 없으면 OS 신뢰 저장소다. 사설 주소도 된다(사내
  대상). 헤더는 16개까지(`Host`·`Content-Length`·`Transfer-Encoding`·`Connection`은 안 됨). 형식(`--format`):

| 형식 | 본문 | 비고 |
|---|---|---|
| `json`(기본) | `{"version":1,"cluster","state":"firing\|resolved\|event\|test","severity","rule","subject":{"kind","id","name","node"},"value","threshold","since","at","title","detail"}` | 시각은 유닉스 ms, 없는 칸은 `null` |
| `slack` | `{"text":"*제목*\n본문"}` | Slack 들어오는 webhook, Mattermost·Rocket.Chat도 |
| `teams` | `{"type":"message","attachments":[Adaptive Card]}` | Teams Workflows(Power Automate) webhook |
| `ntfy` | 텍스트 본문 + `Title`·`Priority`(critical 5·warning 4·그 밖 3)·`Tags` 헤더 | `--token`이면 `Authorization: Bearer` |
| `gotify` | `{"title","message","priority"}`(8·5·2) | `--token`은 `X-Gotify-Key` |

- **비밀.** SMTP 비밀번호, webhook 토큰·헤더 값, URL의 경로·질의(Slack·Teams URL은 경로가 비밀이다)는 API 응답에 나오지
  않는다: `has_password`·`has_token`·`header_names`만 보이고 URL은 `https://hooks.slack.com/…`처럼 줄인다. 오류 문구·로그·
  보낸 기록에도 줄인 URL만 쓴다. CLI는 비밀을 명령 인자로 받지 않는다(셸 기록·`ps`에 남지 않게) — 값 없는 `--password`·
  `--token`·`--header NAME`·`--url -`가 묻는다.
- **바꿀 때 비밀 지키기.** `notify target set`(그리고 웹)은 주지 않은 비밀을 보내지 않고 서버가 저장된 값을 지킨다. 다만
  저장된 비밀이 다른 곳으로 가지 않도록 SMTP 서버·포트·사용자·TLS 방식을 바꾸면 비밀번호를, webhook URL을 조금이라도(줄인
  URL에서 경로만 바꿔도) 바꾸거나 CA를 바꾸면 토큰과 헤더 값을 다시 줘야 한다(아니면 **400** `invalid_request` "… must be
  given again …"). `--no-password`·
  `--no-user`·`--no-token`·`--no-ca`·`--rm-header NAME`이 지운다. 대상 이름은 바꾸지 못한다.
- **시험 보내기.** `POST /notification-targets/{key}/test`는 요청을 받은 노드에서 곧바로 한 번 보내고(10초, 다시 하지 않음)
  `{"ok":true}`나 `{"ok":false,"error":"…"}`를 돌려준다. 보낸 기록에 `test`로 남고 감사된다. **보내는 노드가 대상에 닿아야
  한다** — 경보 알림은 경보를 가진 노드가 보내므로(아래), 모든 노드가 메일 서버·webhook에 닿게 한다.

### 경보 규칙

```bash
orbivisorctl alert rule create cpu-hot --node-metric cpu --gt 90 --for 5m --severity critical --target ops-mail
orbivisorctl alert rule create fs-full --node-metric fs-percent --gt 85 --for 10m --target ops-mail --target ops-slack
orbivisorctl alert rule create web-rss --vm-metric mem-rss --gt 7000 --for 15m --vm web --vm api --target ops-slack
orbivisorctl alert rule create pool-full --pool-usage 90 --target ops-mail
orbivisorctl alert rule create node-down --node-offline --severity critical --target ops-mail --target phone
orbivisorctl alert rule create vm-failed --vm-failed --target ops-slack
orbivisorctl alert rule create start-failed --task-failed --op vm-start --op vm-migrate --target ops-slack
orbivisorctl alert rule create ha --ha-recovery --severity critical --target phone
orbivisorctl alert rule create backup --backup-job-failed --target ops-mail
orbivisorctl alert rule create chassis --condition sdn_chassis_down --for 1m --node n2 --target ops-mail
orbivisorctl alert rule add-sdn-defaults --target ops-mail   # 권하는 SDN 규칙 여덟을 한 번에(같은 이름은 그대로 둠)
orbivisorctl alert rule add-link-defaults --target ops-mail  # 권하는 링크 규칙 둘(업링크 포트 끊김·본딩 저하)
orbivisorctl alert rule list                     # NAME ENABLED SEVERITY CONDITION FOR TARGETS
orbivisorctl alert rule set cpu-hot --gt 95      # 준 것만 바꾼다(--gt·--lt만 주면 문턱만)
orbivisorctl alert rule set cpu-hot --disable    # --enable, --all-nodes, --all-vms
orbivisorctl alert rule delete cpu-hot           # 그 규칙의 활성 경보는 알림 없이 사라진다
```

| 조건 | 무엇 | 누가 평가 | 대상 |
|---|---|---|---|
| `--node-metric M --gt\|--lt V` | 노드 지표: `cpu`(%), `mem-percent`, `fs-percent`(%), `disk-read`·`disk-write`·`net-rx`·`net-tx`(바이트/초) | 그 노드 | 노드 |
| `--vm-metric M --gt\|--lt V` | VM 지표: `cpu`(%), `mem-rss`(MiB), 디스크·네트워크(바이트/초), `disk-read-iops`·`disk-write-iops` | VM이 도는 노드 | 실행 중 VM |
| `--pool-usage P` | 풀 사용률이 P %보다 큼 | 노드 로컬 풀은 그 노드, 공유 풀은 그 풀이 준비된 온라인 노드 가운데 id가 가장 작은 노드 | 풀 |
| `--vm-failed` | VM 단계 `failed` | VM의 주인 노드 | VM |
| `--node-offline` | 노드가 오프라인(리스) | **리더** | 노드 |
| `--task-failed [--op OP]…` | Task 실패(이벤트, `--op` 없으면 모든 종류) | Task를 돈 노드 | Task |
| `--ha-recovery` | `vm-ha-recover` Task가 끝남(이벤트, 결과는 본문에) | Task를 돈 노드 | Task |
| `--backup-job-failed` | 백업 작업 실행이 실패(이벤트) | **리더** | 백업 작업 |
| `--condition sdn_quorum_lost` | OVN NB·SB의 리더가 없거나 리더·팔로어인 중앙이 과반 미만(계획 67) | **리더** | SDN |
| `--condition sdn_central_down` | 중앙 노드의 NB·SB가 꺼짐·후보이거나 노드가 오프라인 | **리더** | 노드 |
| `--condition sdn_chassis_down` | 온라인 chassis 노드의 ovn-controller가 꺼짐·구성 중 | **리더** | 노드 |
| `--condition sdn_gateway_unavailable` | 바깥이 있는 라우터에 게이트웨이 후보가 없거나 모두 오프라인 | **리더** | 라우터 |
| `--condition sdn_gateway_failover` | 라우터의 게이트웨이 주인 노드가 바뀜(이벤트, 옛·새 노드) | **리더** | 라우터 |
| `--condition sdn_sync_failed` | OvnSync 오류, 300초 넘게 동기화 없음, 또는 OVN 잠금 표가 맞지 않는 노드 | **리더** | SDN |
| `--condition sdn_physnet_not_ready` | 설정한 physnet(옮기기 다리 포함)이 준비 안 됨 | **리더** | 노드 |
| `--condition sdn_lb_member_down` | 부하 분산기의 건강 검사 멤버 가운데 죽은 것이 있음(LB마다 하나) | **리더** | 부하 분산기 |
| `node_link_down`(웹·API) | 노드의 랜 포트 링크가 끊김(계획 75, [링크 경보](#링크-경보)) | **리더** | 노드의 포트 |
| `bond_degraded`(웹·API) | 본딩이 저하(구성 포트 일부만 살아 이중화를 잃음)되거나 끊김 | **리더** | 노드의 본딩 |

- **칸.** `--for`(조건이 이만큼 이어지면 알림, `0`~`24h`, 기본 0), `--severity info|warning|critical`(기본 warning), `--target`
  (알림 대상 이름 1~16개, 있어야 함), `--repeat`(계속 firing이면 이 간격으로 다시, 기본 `4h`, `0`이면 다시 보내지 않음,
  `1m`~`7d`), `--send-resolved true|false`(기본 true), `--node`·`--vm`(이 노드·VM만, 이름 256개까지, 비면 모두), `--disabled`.
  `--vm`은 VM 조건(`vm-metric`·`vm-failed`·`ha-recovery`)에만, `--node`는 `backup-job-failed`에 쓰지 못한다. 이벤트 조건은
  `--for 0`만 받고 해제 알림이 없다. SDN 조건의 `--node`는 대상이 노드인 셋(`sdn_central_down`·`sdn_chassis_down`·
  `sdn_physnet_not_ready`)에만 쓴다. API 본문은 `POST /alert-rules {"name","spec":{"condition":{"kind":"node_metric","metric":"cpu","op":"gt","value":90},"for_secs":300,"severity":"critical","targets":["ops-mail"]}}`다.
- **유지보수.** 유지보수 노드(`node maintenance NODE on`·`node drain`)의 노드 지표 규칙과 `node-offline`은 평가하지 않고, 들어갈 때 그 노드의
  활성 노드 경보는 알림 없이 풀린다. 그 노드에서 도는 VM의 규칙은 그대로 본다. 기록은 유지보수 중에도 계속한다.

### 평가와 상태

- **노드 경보.** 지표·`vm-failed`·`task-failed`·`ha-recovery`·`pool-usage` 규칙은 그 일이 일어난 노드가 자기 메모리 지표와
  자기 VM·Task로 5초마다 평가하고, 그 노드가 보낸다. 일이 노드마다 나뉘므로 노드가 늘어도 한 노드의 일이 늘지 않고,
  Raft에 쓰지 않는다. 상태는 `{data_dir}/alerts/state.json`(0600, 바뀔 때만 임시 파일 → fsync → rename)에 두어 orbivisord를
  다시 시작해도 같은 경보를 다시 보내지 않고, 멈췄던 동안의 Task 이벤트는 처리한 watch 리비전부터 이어 본다(Task id로
  7일 동안 두 번 보내지 않음). 깨진 파일은 `state.json.corrupt`로 옮기고 빈 상태로 시작한다. 장부 디렉터리를 열지 못하면
  (권한 등) 경고하고 메모리에만 두고 시작한다(다시 시작하면 상태와 보낸 기록을 잊는다). 목록을 읽지 못한 tick, 잠깐 준비되지
  않은 로컬 풀, 마이그레이션 중인 VM의 지표 경보는 알림 없이 빼지 않고 그대로 둔다.
- **리더 경보.** `node-offline`·`backup-job-failed`와 SDN 조건 여덟은 그 판단이 이미 리더의 것이므로 리더가 평가하고 리더가 보낸다
  (SDN 조건의 입력은 Raft의 SDN·노드 상태와 리더 OvnSync가 10초마다 읽는 게이트웨이 주인·LB 멤버 건강 — [SDN 경보](network.md#sdn-경보)). 상태는
  규칙의 `status.active`(Raft)에 두고, **상태를 쓴 뒤에만 보내므로** 리더가 바뀌어도 새 리더가 이어받아 다시 보내지 않는다.
  규칙을 만들기 전에 끝난 백업 실행은 알리지 않는다. 지운 노드의 `node-offline` 경보는 해제로 알린다.
- **상태 기계.** 지표·상태 조건은 `pending`(조건이 참) → `firing`(참인 채 `--for`가 지남, 알림) → `--repeat`마다 다시 알림 →
  `resolved`(거짓인 채 **60초**가 지남 — 깜박임 막기, `--send-resolved`면 알림)다. `pending` 중에 거짓이 되면 알림 없이 끝난다.
  값을 읽지 못한 점(`null`)은 상태를 바꾸지 않는다. 대상이 사라지면(VM 지움·멈춤, 노드를 지움) 알림 없이 끝난다. 이벤트
  조건은 일마다 한 번 보내고 끝이다.
- **상한.** 노드마다 활성 경보는 500개까지다. 넘으면 새 경보를 따라가지 않고 `GET /alerts`의 `overflowing`에 그 노드 이름이
  보인다(경고 로그 한 번).

### 보내기와 보낸 기록

- 경보를 가진 노드가 Raft의 대상 설정으로 보낸다: 노드마다 동시에 4개(한 대상에는 1개), 하나에 10초까지. 실패하면 30초·2분·
  10분 뒤 세 번 다시 한다. 대상마다 노드마다 **분당 20개**까지이고, 넘는 것은 그 분이 끝난 뒤 "N more notifications were held
  back by the rate limit of {대상}" 알림 하나로 묶는다. 보낼 대기열은 1000개까지이고 넘으면 버린다(기록에 `dropped: the send
  queue is full`). 바쁜 대상의 차례를 기다리는 알림도 모두 합쳐 1000개까지이고, 넘으면 가장 많이 밀린 대상의 가장 오래된
  것을 버린다 — 답하지 않는 대상 하나가 다른 대상의 알림을 늦추지 않는다. 대상의 차례는 돌아가며 오고, 한 대상의 차례를
  기다리는 알림은 20개까지(넘는 것은 "N more" 알림에 센다)다. 처음 넣은 뒤 15분 안에 보내지 못한 알림은 버린다(`dropped: not
  sent in time` — 살아난 대상에 묵은 알림을 몰아 보내지 않게). 같은 경보의 RESOLVED를 받으면 다시 하기를 기다리는 그 대상의
  FIRING은 버린다(`dropped: the alert was resolved before the retry`). 꺼진 대상(`--disable`)은 조용히 건너뛴다.
- **보낸 기록.** 보낸 노드의 `{data_dir}/alerts/history.log`(JSON Lines, 4 MiB에서 `history.log.1`로 한 번 돌림)에 마지막 결과만
  남는다(성공, 또는 다시 하기를 다 쓴 실패 — `(after N attempts)`).

```bash
orbivisorctl alert list                 # 활성 경보(firing 먼저, 심각도 높은 것 먼저)
# STATE     SEVERITY  RULE     SUBJECT       VALUE  THRESHOLD  SINCE
# firing    critical  cpu-hot  node/node1    97.26  90         2026-11-18T06:00:00Z
# clearing  warning   web-rss  vm/web@node2  -      -          2026-11-18T05:40:00Z
orbivisorctl alert history --since 1h   # 보낸 알림(새 것부터, 기본 지난 24시간, --until, --limit 1~5000 기본 200)
# TIME                  NODE   STATE     RULE     SUBJECT  TARGET    RESULT
# 2026-11-18T06:00:05Z  node1  firing    cpu-hot  node1    ops-mail  ok
```

- `STATE`의 `clearing`은 조건이 풀려 60초 해제 대기 중인 것이다. 노드 대상은 노드 이름, 그 밖은 `종류/이름@노드`로 보인다.
- **API.** `GET /api/v1/alerts`는 Raft의 리더 경보와, 모든 노드에 노드 사이 RPC로 물은 노드 경보(32개씩, 5초)를 합친다
  (`{"alerts":[…],"unreachable","unsupported","overflowing"?}`). 답하지 않는 노드의 노드 경보는 보이지 않지만 그 노드의
  `node-offline` 경보가 대신 보인다. `GET /api/v1/alerts/history?since=&until=&limit=`(유닉스 ms나 RFC 3339)은 노드마다의
  기록을 시각 순으로 합친다(`truncated`면 CLI가 `note: more records match …`). 둘 다 노드 문제로 실패하지 않는다.
  `/watch?kinds=alert-rule`로 규칙(리더 경보 포함) 변경을 받는다. 알림 대상은 watch할 수 없다.

- **SDN 경보 묶음.** `alert rule add-sdn-defaults --target T`(API `POST /api/v1/alert-rules/sdn-defaults {"targets": [...]}` → `{created,
  existing}`, 웹 SDN 보기의 "SDN 경보 켜기")가 `sdn-quorum-lost` critical 0초, `sdn-central-down`·`sdn-chassis-down` warning 60초,
  `sdn-gateway-unavailable` critical 30초, `sdn-gateway-failover` info, `sdn-sync-failed` warning 120초, `sdn-physnet-not-ready` warning 60초,
  `sdn-lb-member-down` warning 30초를 만든다. 같은 이름이 있으면 건드리지 않으므로 다시 불러도 된다. 모든 노드가 기능 `sdn-services`를
  알아야 SDN 조건을 쓸 수 있다(409 `cluster_upgrading`).

### 링크 경보

랜 포트·본딩 장애를 그 포트 이름과 영향으로 알린다(계획 75). 입력은 노드 상태의 인터페이스 목록(노드가 30초마다 `ip`·sysfs로 씀 —
[네트워크](network.md#물리-네트워크-포트본딩lldp))이고 **리더**가 다른 리더 경보처럼 평가한다(상태는 규칙의 `status.active`).

- **`node_link_down {ports?, role}`.** 링크가 끊긴 포트(carrier 없음, operstate `down`·`lowerlayerdown`·`notpresent`)마다 경보 하나다(경보
  키에 포트가 들어가 포트마다 따로). 이더넷, dummy 같은 그 밖의 장치, 아래 장치가 살아 있는 VLAN, 관리자가 내린 브리지(`ip link set vmbr0
  down`)를 센다. 본딩 구성 포트는 따로 세지 않는다: 본딩이 아직 흐르면 이중화만 잃은 것이라 `bond_degraded`가 맡고, 본딩이 통째로 끊기면
  본딩 이름(`bond0`)으로 하나다. `role`이 `uplink`(기본)면 쓰이는 포트만 본다 — 브리지·본딩·underlay 아래의 포트와 전역 주소를 가진 물리
  포트(스토리지·복제·관리·클러스터 링크망, 본딩 없이 주소만 가진 `ens1f0` 같은 것)와 그 아래. 쓰지 않는(묶이지 않고 주소도 없는) 포트는
  `any`에서만 본다. `any`도 어디에도 묶이지 않은 가상 장치(`ovs-system`·`br-int` — 늘 down)는 보지 않는다. `ports`는 `*`·`?` 글롭(예:
  `eno*`, 1~64자, 공백·`/`·`:` 없음)이고 비우면 모든 포트다.
- **`bond_degraded`.** 본딩마다 하나 — 구성 포트 일부가 끊기거나 노드에서 사라져(PCIe 장애·드라이버 unbind — 노드가 본 적 있는 구성
  포트를 `missing`으로 남김) 이중화를 잃었고 아직 흐름. 본딩이 끊기면 이 경보는 풀리고 `node_link_down`(본딩 이름)이 새로 울린다.
- **문구.** 대상은 `node/svc01/eno1`(CLI `alert list`의 SUBJECT, 알림의 이름 `svc01/eno1`), 메시지는 물리 관계로 셈한 영향이다:
  `eno1 down on svc01: bond0 degraded — vmbr0, 3 networks, 12 VMs`, 주소만 가진 포트는 `ens1f0 down on svc01 — cluster link`(노드의 클러스터
  링크 주소·API 주소면 `cluster link`·`management`). 영향 계산은 `GET /topology/physical`과 같은 함수다. 알림 본문에는 지도에서 그 포트로
  가는 경로 `Map: /cluster/map?focus=port:svc01/eno1`(웹훅 JSON은 `subject.map`)이 붙는다 — 앞에 웹 주소를 붙여 연다.
- **상태.** 지속 조건이라 `--for`·`--repeat`·해제가 있다. 다시 올라온 포트는 60초 뒤 해제다. 경보가 난 포트가 노드의 인터페이스 목록에서
  사라지면 풀지 않고 그대로 두고 되풀이 알림에 `eno2 disappeared from svc01`을 싣는다(다시 나타나 살아 있으면 풀림). 오프라인·유지보수
  노드의 상태는 낡았거나 일부러 바꾸는 중이므로 그 노드의 링크 경보는 새로 내지도·풀지도·되풀이하지도 않는다(노드 꺼짐은 `node-offline`이
  알림). 노드 거르기(`nodes`)를 받는다.
- **링크 경보 켜기.** `alert rule add-link-defaults --target T`(여러 번 줄 수 있음, API `POST /api/v1/alert-rules/link-defaults
  {"targets": [...]}` → `{created, existing}`, 웹 홈의 "링크 경보 켜기")가 `node-link-down`(업링크 포트, critical, 30초)과
  `bond-degraded`(warning, 60초)를 만든다. SDN 묶음과 따로라 SDN을 쓰지 않는 클러스터에서도 쓴다. 같은 이름이 있으면 건드리지 않고
  만든 것·그대로 둔 것을 줄마다 알린다(`alert rule node-link-down created`, `alert rule bond-degraded already exists; kept`). 만든 뒤로는
  보통 규칙이다.
- **다른 규칙.** 포트 글롭·`role: any`는 웹 규칙 폼(조건 "랜 포트 링크 끊김" — 볼 포트·포트 이름 칸)이나 API로 만든다:
  `POST /alert-rules {"name":"storage-nic","spec":{"condition":{"kind":"node_link_down","ports":"ens1f*","role":"any"},"for_secs":10,
  "severity":"critical","targets":["phone"]}}`. CLI `--condition`은 링크 조건을 받지 않는다.
- 모든 노드가 기능 `link-alerts`를 알아야 링크 조건 규칙을 만들고 묶음을 쓴다(아니면 **409** `cluster_upgrading`).

### 웹

- **경보**(활동 › 경보 탭 `/alerts`, 홈 경보 위젯이나 종 서랍의 "모든 경보 보기"): **활성** 탭(심각도·대상·단계
  울림/대기/해제 중·값과 문턱·시작 시각, 답하지 않은·옛 노드 안내), **규칙** 탭(누구나 읽음, admin만 만들기·고치기·지우기 —
  조건 종류에 따라 칸이 바뀌는 폼, 조건 고르기에 SDN 묶음과 링크 조건(볼 포트 업링크·underlay만 | 모든 랜 포트, 포트 이름 글롭) — 노드 거르기는 대상이 노드인 SDN 조건에만, 게이트웨이 넘김은 지속 시간 없는 이벤트), **보낸 알림** 탭(지난 24시간·7일, 500개까지). 경보는 화면 알림으로 띄우지 않고 머리줄 종의 숫자와 서랍에 보인다([웹 화면 둘러보기](web-ui.md#알림-센터)).
- **알림 대상**(설정 › 알림 대상 탭 `/settings/notifications`, admin — 규칙 탭의 링크로도 간다): SMTP·webhook 폼, 시험 보내기.
  저장된 비밀은 비워 두면 지키고 "지우기"로 지운다. 서버·포트·사용자·TLS나 URL·CA를 바꾸면서 비밀을 다시 넣지 않으면
  폼이 먼저 막는다.

### 권한·감사·오류

- **권한.** 규칙과 활성 경보·보낸 기록은 누구나(viewer 포함) 읽고 admin만 쓴다. 다만 보낸 기록의 대상 이름(`target`)과 오류
  문구(`error`)는 admin에게만 보인다. 알림 대상은 읽기·쓰기·시험 보내기 모두 admin만이다(사용자·인증 영역과 같은 계정 종류 —
  viewer·operator는 `/notification-targets*`에 **403**).
- **감사.** `alert-rule create|update|delete NAME`, `notification-target create|update|delete|test NAME`.
- **오류.** **400** `invalid_request`(규칙·대상 검증, 없는 대상을 쓰는 규칙, 비밀을 다시 줘야 함, 이름 바꾸기, 모르는 칸,
  잘못된 `since`·`until`·`limit`), **403** `forbidden`, **404** `not_found`, **409** `cluster_upgrading`(`notifications`를
  모르는 노드), **409** `in_use`(규칙이 쓰는 대상 지우기 — 규칙 이름), **409** `busy`(대상이 계속 바뀜), **412**
  `revision_mismatch`(`If-Match`가 낡음 — CLI `rule set`은 세 번까지 다시 읽어 한다).

### 한계

- 경보 묶기·억제(노드 경보가 그 노드 VM의 경보를 가림), 근무 일정, 개별 경보 확인(ack)·잠시 끄기(silence)는 없다. 잠시
  끄려면 규칙을 `--disable`하거나 노드를 유지보수로 둔다.
- SNMP 트랩, SMS, 스크립트 실행 대상은 없다. 메일 본문은 일반 텍스트다.
- 노드 경보 상태는 쓰기 직후·보내기 전에 orbivisord가 죽으면 그 알림 하나를 보내지 못할 수 있다(다시 보내지 않는 쪽을
  골랐다). 꺼진 노드의 노드 경보는 그 노드가 돌아와야 해제되고, 그동안은 `node-offline` 경보로만 보인다.
- 업그레이드 중 옛 노드의 지표 경보는 없다(그 노드는 평가하지 않는다).

## Prometheus

노드마다 `GET /api/v1/metrics`가 Prometheus 텍스트 형식(0.0.4)으로 **그 노드의** 지표를 내보낸다. 클러스터 값은 모든
노드가 같은 것을 낸다. Prometheus는 **모든 노드를 따로** 긁는다. 설정 `prometheus.enabled`가 꺼져 있으면(기본) **404**
`not_found`다. 켜고 끄기는 모든 노드가 기능 `metrics-history`를 알려야 한다(아니면 **409** `cluster_upgrading`).

```bash
orbivisorctl prometheus enable      # PATCH /settings {"prometheus":{"enabled":true}} — 감사 settings prometheus on
# prometheus: enabled
# scrape: https://10.0.0.11:8006/api/v1/metrics (bearer token of a viewer or higher)
orbivisorctl prometheus show        # 켜졌는지(on·off는 enable·disable의 별칭)
orbivisorctl prometheus disable
```

- **인증.** API 토큰(`Authorization: Bearer opl_…`)이나 세션이다. 노드 읽기와 VM 읽기 권한이 있어야 하므로 **viewer 역할의
  토큰**이면 된다(따로 읽기 전용 토큰 종류는 없다 — 폐기·감사도 보통 토큰과 같다). 순서는 401(인증 없음) → 403(권한) →
  404(꺼짐)이다. 응답은 노드마다 5초 동안 다시 쓴다.
- **값.** 모두 gauge이고 노드 메모리의 마지막 5초 점(15초보다 오래되면 그 줄을 뺌)이다. 누적 카운터가 아니라 비율이므로
  `rate()`를 씌우지 않는다. 읽지 못한 값의 줄은 빼고, 줄이 하나도 없는 이름은 HELP·TYPE도 쓰지 않는다.

| 이름 | 레이블 | 값 |
|---|---|---|
| `orbivisor_node_cpu_usage_ratio` | `node` | 0~1(모든 코어 합) |
| `orbivisor_node_memory_used_bytes`, `_memory_total_bytes` | `node` | 바이트 |
| `orbivisor_node_disk_read_bytes_per_second`, `_disk_write_bytes_per_second` | `node` | 바이트/초 |
| `orbivisor_node_network_receive_bytes_per_second`, `_network_transmit_bytes_per_second` | `node` | 바이트/초 |
| `orbivisor_node_fs_used_bytes`, `_fs_total_bytes` | `node` | `data_dir`의 파일 시스템 |
| `orbivisor_vm_info` | `vm_id`, `vm`(이름), `node` | 1(이 노드에서 도는 VM마다) |
| `orbivisor_vm_cpu_usage_ratio` | `vm_id` | 1 = 모든 vCPU가 바쁨(조금 넘을 수 있다) |
| `orbivisor_vm_memory_rss_bytes` | `vm_id` | QEMU RSS |
| `orbivisor_vm_disk_read_bytes_per_second`, `_disk_write_bytes_per_second`, `_disk_read_ops_per_second`, `_disk_write_ops_per_second` | `vm_id` | |
| `orbivisor_vm_network_receive_bytes_per_second`, `_network_transmit_bytes_per_second` | `vm_id` | 게스트 기준 |
| `orbivisor_pool_used_bytes`, `orbivisor_pool_total_bytes` | `pool`(이름) | 이 노드에서 준비된 풀(Raft 값, 1 GiB 단위로 갱신) |
| `orbivisor_cluster_nodes` | `state` = `online`·`offline`·`maintenance` | 노드 수(유지보수가 먼저, 서로 겹치지 않음) |
| `orbivisor_cluster_vms` | `phase` = `stopped`·`starting`·`running`·`stopping`·`failed`·`migrating` | VM 수(0도 냄) |
| `orbivisor_cluster_leader_present` | | 1이면 정족수가 있음 |
| `orbivisor_node_is_raft_leader` | `node` | 1이면 이 노드가 리더 |
| `orbivisor_alerts_firing` | `severity` = `info`·`warning`·`critical` | 이 노드의 firing 노드 경보 + (리더면) 리더 경보 |

- **카디널리티.** 레이블은 `node`·`vm_id`·`pool`·`state`·`phase`·`severity`뿐이다. VM 이름은 `orbivisor_vm_info`에만 있어 이름을
  바꿔도 시리즈가 끊기지 않는다. Task id·사용자·디스크 장치는 레이블로 쓰지 않는다. 위트니스는 클러스터 값과
  `orbivisor_node_is_raft_leader`만 내보낸다.

**Prometheus 설정 예.** viewer 사용자의 토큰과 클러스터 CA를 Prometheus 서버에 둔다(웹 설정의 "Prometheus" 칸에도 이 노드의
주소로 채운 예와 복사 단추가 있다):

```bash
orbivisorctl user create prometheus --role viewer
# 그 사용자로 로그인해 토큰을 만든다(웹: 그 사용자로 API 토큰 화면). 토큰은 다시 볼 수 없다.
orbivisorctl token create prometheus > /etc/prometheus/orbivisor.token
curl -k https://10.0.0.11:8006/api/v1/cluster/ca -o /etc/prometheus/orbivisor-ca.pem   # 지문을 cluster status와 비교한다
```

```yaml
scrape_configs:
  - job_name: orbivisor
    scheme: https
    metrics_path: /api/v1/metrics
    authorization:
      credentials_file: /etc/prometheus/orbivisor.token
    tls_config:
      ca_file: /etc/prometheus/orbivisor-ca.pem
    static_configs:
      - targets: ["10.0.0.11:8006", "10.0.0.12:8006", "10.0.0.13:8006"]   # 모든 노드(VIP 하나가 아님)
```

VIP나 프록시 하나만 긁으면 그 순간 그 주소가 가리키는 노드의 값만 모인다.

**Grafana 쿼리 예.**

```promql
100 * orbivisor_node_cpu_usage_ratio                                          # 노드 CPU %
orbivisor_node_memory_used_bytes / orbivisor_node_memory_total_bytes          # 노드 메모리 사용률
100 * orbivisor_vm_cpu_usage_ratio * on(vm_id) group_left(vm, node) orbivisor_vm_info   # VM CPU %, VM 이름·노드를 붙여
topk(10, orbivisor_vm_memory_rss_bytes * on(vm_id) group_left(vm) orbivisor_vm_info)
max without(instance) (orbivisor_cluster_nodes)                               # 클러스터 값은 노드마다 같으므로 하나로
max by(pool) (orbivisor_pool_used_bytes) / max by(pool) (orbivisor_pool_total_bytes)   # 공유 풀(노드마다 같은 값)
sum by(severity) (orbivisor_alerts_firing)                                    # 클러스터의 firing 경보
```

- Prometheus 쪽에서 오래 둘 지표와 자기 경보(Alertmanager)를 쓰려면 이 끝점을, 클러스터 안에서 바로 보려면 위 기록·알림을
  쓴다. 둘은 같은 측정 값이다.
- **한계.** 누적 카운터·OpenMetrics·원격 쓰기는 없다(값은 마지막 5초 비율). 지표를 밀어 보내기(Graphite·InfluxDB)와 내장
  Grafana는 없다. 노드마다 따로 긁어야 하고(서비스 찾기 없음 — 노드를 더하면 `targets`에 더한다), 클러스터 값은 노드 수만큼
  중복이다.
