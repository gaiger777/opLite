# 고가용성(HA)

HA VM과 펜싱, 위트니스, 배치 규칙, 상태 검사, 최적 배치, 로컬 디스크 복제. 처음 보는 사람은 [README](../../README.md)의 빠른 시작부터.

- [HA VM과 자기 펜싱](#ha-vm과-자기-펜싱)
- [위트니스 노드](#위트니스-노드)
- [배치 규칙과 HA 정책](#배치-규칙과-ha-정책)
- [HA 상태 검사](#ha-상태-검사)
- [최적 배치](#최적-배치)
- [로컬 디스크 복제와 HA](#로컬-디스크-복제와-ha)

## HA VM과 자기 펜싱

HA VM은 노드가 클러스터 과반수와 끊기면 그 노드가 스스로 끈다(자기 펜싱, Proxmox HA의 watchdog 펜싱과 같은
자리). 노드는 리더에게 몇 초마다 **리스**를 갱신하고, 마지막으로 성공한 갱신 뒤 `fence_after_secs`가 지나면 그
노드의 HA VM을 강제로 끈다. orbivisord 자신이 멈추거나 멈춰 서도 끄도록 Linux watchdog 장치를 쥐고 있다가, 리스를 잃은
채 HA VM이 돌면 쓰다듬기를 멈춰 노드를 재시작시킨다. 리더는 노드가 스스로 펜싱을 마쳤을 시간(`recover_after_secs`)이
지나도록 리스가 돌아오지 않으면 그 노드의 HA VM을 다른 노드로 옮겨 켠다(**복구**, 아래). 디스크가 노드 로컬 풀에 있는
HA VM은 복제가 있어야 다른 노드에서 켤 수 있다(아래 "로컬 디스크 복제와 HA").

```bash
orbivisorctl vm create web --memory 1024 --disk root --ha --restart on-failure --start
orbivisorctl vm set web --no-ha      # 끄기. 실행 중이어도 된다.
orbivisorctl vm set web --ha         # 켜기. VM이 멈춰 있을 때만 된다(실행 중이면 409 vm_running).
orbivisorctl vm list                 # HA 열(true/false)
orbivisorctl node list               # FENCING 열: watchdog · process · -(꺼짐), FENCE 열: ok · unreachable · -
orbivisorctl cluster status          # 멤버마다 FENCING 열
orbivisorctl task list               # 복구 기록: OP "vm-ha-recover (from <죽은 노드 id>)", 전원 끄기 "node-power-off"
orbivisorctl node fence set node2 -f fence.json   # 전원 끄기 펜싱(아래, admin). -f - 이면 표준 입력
orbivisorctl node fence get node2                 # 방법들(비밀번호는 저장되어 있는지만)
orbivisorctl node fence delete node2
orbivisorctl node fence-test node2                # 방법마다 전원 읽기(전원은 건드리지 않음)
orbivisorctl vm set web --ha-priority 80 --guest-watch 60   # 복구 우선순위, 게스트 멈춤 감지(--no-guest-watch)
orbivisorctl settings set ha_reserve=nodes:1      # HA 용량 예약: none | nodes:N | percent:P
orbivisorctl settings set ha_start_concurrency=8  # 복구 때 대상 노드당 동시 시작(기본 4)
```

웹은 VM 만들기·편집의 "고가용성(HA)" 스위치(그 노드의 펜싱이 꺼져 있거나, 실행 중이고 HA가 꺼진 VM이면 이유와
함께 켜기를 막는다)와 그 아래 "복구 우선순위"·"게스트가 멈추면 다시 켜기", VM 목록·상세의 HA 배지(상세 요약에 우선순위·게스트
에이전트 감시), 노드 목록·클러스터 보기의 "펜싱" 열(노드 화면은 그 아래 "N초 뒤 복구"와 "전원 펜싱 닿지 않음" 배지),
노드 메뉴의 "전원 끄기 펜싱…"(admin, 방법 더하기·빼기·순서, 저장, "시험", "펜싱 지우기"), 관리 › 일반·업데이트의 "HA 용량", 작업 목록의
"web HA 복구 (노드 node-a에서)"·"node-b 전원 끄기(펜싱)"다. API는 VM `spec.ha`(생성·`PUT /api/v1/vms/{key}`),
Node `status.ha`(`{"fencing": "watchdog"|"process", "recover_after_secs": …, "fence_after_secs": …}`, 펜싱이 꺼져 있으면
없다)·`status.fence_check`, `GET|PUT|DELETE /api/v1/nodes/{key}/fence`·`POST /api/v1/nodes/{key}/fence-test`, 설정
`ha_reserve`·`ha_start_concurrency`, `GET /api/v1/cluster`의 멤버 `fencing`(꺼져 있으면 null)이다. HA 그룹·상태·재시작과 옮기기 횟수·배치 규칙·호스트 종료
정책은 아래 "배치 규칙과 HA 정책"이다(같은 노드의 크래시 재시작도 거기서 정한다 — 옛 노드가 남은 동안은 `restart`가 정하므로
`on-failure`를 권한다). 리스는 관리 네트워크만 본다 — 스토리지·VM 네트워크·본딩 포트의 장애는 아래 "HA 상태 검사"다.

```toml
# /etc/orbivisor/orbivisord.toml (노드마다, 생략하면 아래 기본값)
[ha]
fencing = "watchdog"              # watchdog | process | off (기본: Linux watchdog, macOS off)
profile = "fast"                  # fast(10·10초) | safe(30·30초, 계획 51 전의 값)
watchdog_device = "/dev/watchdog" # Linux watchdog 장치(하드웨어 또는 softdog)
# watchdog_timeout_secs = 10      # 장치에 정하는 제한 시간(5~3600초, WDIOC_SETTIMEOUT). 적으면 프로필을 이긴다.
# fence_after_secs = 10           # 마지막 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다(1초 이상). 적으면 프로필을 이긴다.
ipmitool = "ipmitool"             # 전원 끄기 펜싱의 IPMI 방법이 부르는 실행 파일(아래)
ping = "ping"                     # HA 검사의 ping 대상이 부르는 실행 파일(아래 "HA 상태 검사")
```

- **프로필과 기본값.** `profile`은 `fence_after_secs`·`watchdog_timeout_secs`를 함께 정하고, 둘 중 적은 값은 프로필을
  이긴다. 리더가 기다리는 `recover_after_secs`는 `fence_after_secs + watchdog_timeout_secs(watchdog일 때) + 2`다.

  | 프로필 | `fence_after_secs` | `watchdog_timeout_secs` | 리스 갱신 | `recover_after_secs`(watchdog) | (process) |
  |---|---|---|---|---|---|
  | `fast`(기본) | 10 | 10 | 1초 | 22초 | 12초 |
  | `safe` | 30 | 30 | 2초 | 62초 | 32초 |

  **업그레이드하면 `[ha]`에 `profile`도 두 값도 적지 않은 노드는 `fast`가 된다**(계획 51 전에는 30·30초, 복구 65초).
  짧은 리스는 잠깐의 망 끊김이나 멈칫한 데몬에도 펜싱한다. 클러스터 망이 불안하거나 아래 경고가 뜨면 옛 동작
  `profile = "safe"`를 둔다. 실제로 잰 복구 시간(이 Mac, `fast` + process 펜싱, 디스크 없는 VM): kill -9 약 12초, 망 끊기 약
  12초, SIGSTOP 약 13초(리스 끊김부터 다른 노드의 QEMU 시작까지, 게스트 부팅은 따로). 값을 키워(`fast` → `safe` 등) 다시 띄운
  노드는 새 값을 노드 기록(`status.ha`)에 쓰기 전에는 HA VM을 켜거나 도는 HA VM에 붙지 않는다(리더가 옛 짧은
  `recover_after_secs`로 옮기지 않게). 이 기록은 전체 등록(이름 풀기 등)을 기다리지 않고 `status.ha`만 먼저 쓰며, 쓰지
  못하면 0.5초마다 다시 한다(한 번에 2초까지) — 이전 프로세스가 켜 둔 watchdog이 그 사이 노드를 재시작하지 않게.
- **시작 경고.** 펜싱이 켜졌고 실제 `fence_after_secs`가 30초 미만인데 (1) 클러스터 링크가 하나뿐이거나(`cluster_addresses`가
  둘 이상이 아님, 아래 "클러스터 통신 다중 링크") (2) `fencing = "watchdog"`인데 systemd 밖에서 돌면(`NOTIFY_SOCKET` 없음 —
  `Type=notify`·`WatchdogSec`의 데몬 감시가 없음) 시작 로그에 경고하고 `profile = "safe"`를 권한다. 시작은 막지 않는다.
  실행기 격리(위 "제어부 가용성")는 끌 수 없어 늘 켜진 것으로 본다.

- **펜싱 방식.** `watchdog`: orbivisord가 HA VM을 끄고, orbivisord가 멈추면 watchdog가 노드를 재시작한다(운영용). `process`:
  orbivisord가 끄는 것만 한다 — orbivisord가 죽거나 멈추면 아무것도 끄지 않으므로 **시험·개발용**이다(macOS 노드도 쓸 수 있다).
  `process`의 여유는 2초다(`recover_after_secs` = `fence_after_secs + 2`, 계획 51 전에는 5초): 리스를 잃은 노드가 그 2초 안에
  HA VM을 끄지 못하면 리더가 옮긴 VM과 함께 돌 수 있다.
  `off`: 이 노드는 HA VM을 돌리지 않는다. Linux가 아니면 `watchdog`는 설정 오류다. 위트니스는 `[ha]`를 쓰지 않는다.
- **HA 가능 노드.** 위트니스가 아니고 펜싱이 켜진 노드만 HA VM을 돌린다. HA VM의 노드(`--node`, 편집)나
  마이그레이션 대상이 펜싱이 꺼진 노드면 **409** `ha_unsupported`, 노드를 정하지 않은 HA VM은 스케줄러가 HA 가능
  노드만 고른다(없으면 **409** `no_node`). 노드는 등록할 때마다 `[ha]`를 `status.ha`에 쓴다. `recover_after_secs`는
  `fence_after_secs + watchdog_timeout_secs(watchdog일 때) + 2`(기본 22초)로, 리더가 이만큼 기다린 뒤 옮긴다.
- **HA는 멈춘 VM에서만 켠다.** 만들 때 `--ha`는 그대로 되지만, 이미 있는 VM의 HA를 켜는 수정(`vm set --ha`, 웹
  편집, `PUT`의 `ha` false → true)은 VM이 멈춰 있을 때만 된다(원하는 상태가 stopped이고 phase가 stopped·failed).
  아니면 **409** `vm_running`("cannot turn on HA for VM "web" while it is running; turn on HA while the VM is
  stopped"). 끄기는 언제든 된다. 노드가 과반수와 끊긴 동안 실행 중인 VM의 HA를 켜면 그 노드는 HA인 줄 모르고
  끄지 않는데 리더는 그 VM을 다른 노드에서 켜 둘이 함께 돌 수 있기 때문이다. 실행 중인 VM은 정지하고 HA를 켠 뒤
  다시 시작한다.
- **리스.** 갱신 간격은 `fence_after_secs`의 10분의 1(0.2초 이상 2초 이하, 기본 1초, `safe`는 2초)이다. 리더는 아직 과반수의
  리더인지 확인한 뒤에만 갱신을 받아 준다. 그래서 과반수와 끊긴 노드는 물론, 고립된 리더 자신도 갱신하지 못한다.
  리스는 orbivisord가 시작한 때부터 센다(다시 시작한 직후 곧바로 끄지 않는다).
- **리스를 잃으면.** `fence_after_secs`가 지나면 그 노드의 HA VM을 강제로 끈다(실패로 세지 않고 원하는 상태는
  running 그대로, status "fenced: lost the cluster majority"). HA 아닌 VM은 그대로 돈다. 리더가 옮기기 전에 리스가
  돌아오면 그 노드에서 다시 켠다. 리스 없이는 HA VM을 켜지도, 멈춘 채 받은 마이그레이션을 이어서 실행하지도
  않는다("waiting for the cluster majority before starting an HA VM", 실패로 세지 않고 다시 본다). 켜기 바로 앞에서 리스와 VM을 다시 보아,
  그 사이 다른 노드로 배정됐거나 멈추라고 했으면 켜지 않는다. 짧은 네트워크 장애라도 `fence_after_secs`
  보다 길면 HA VM이 꺼진다.
- **watchdog.** 이 노드에서 HA VM을 켜거나 이미 도는 HA VM에 붙을 때 장치를 처음 열고(그 전에는 열지 않는다 —
  HA VM이 없는 Linux 노드는 장치를 건드리지 않는다), 그 뒤로 orbivisord가 끝날 때까지 쥔 채 1초마다 쓰다듬는다. 리스가
  끝났는데 HA VM이 돌면 쓰다듬기를 멈춘다. orbivisord가 제한 시간 안에 HA VM을 끄면 다시 쓰다듬으므로 노드는 재시작되지
  않고, orbivisord가 멈춰 끄지 못하면 노드가 재시작된다. 장치를 열지 못하면(없음, 다른 프로세스가 쥠) HA VM을 켜지 않고
  status에 "cannot open the watchdog device: …"를 남긴다. 장치가 요청보다 긴 제한 시간을 고르거나 제한 시간을 정하지
  못하면 HA VM에 쓰지 않는다(status "the watchdog device is unusable: …", 짧은 값을 고르면 경고만 남기고 쓴다). 그런
  장치도 이미 열어 타이머가 돌므로 닫지 않고 쥔 채 같은 규칙으로 쓰다듬고, 정상 종료 때 magic close로 멈춘다.
- **orbivisord를 멈추면 노드가 재시작될 수 있다.** 정상 종료(SIGTERM) 때 이 노드에 도는 HA VM이 없으면 watchdog를 멈추고
  닫는다(magic close). **도는 HA VM이 있으면 그대로 둔다**(경고 "HA VMs are running; leaving the watchdog armed …") —
  QEMU는 orbivisord 없이도 계속 돌므로, orbivisord가 `watchdog_timeout_secs` 안에 다시 떠 리스를 갱신하지 못하면 노드가
  재시작된다(호스트 재부팅·전원 끄기 때는 종료 정책이 HA VM을 먼저 끈다 — 아래 "배치 규칙과 HA 정책"). 업그레이드·점검으로 orbivisord를 오래 멈출 때는 먼저 HA VM을 끄거나 다른 노드로 옮기거나(`node drain`, 아래 "노드
  유지보수와 비우기") `vm set --no-ha`를 한다. 펜싱을 `off`로 바꿔 다시 띄운 노드의 이미 도는 HA VM은 끄지 않고
  status에 "HA is not available on this node"만 남긴다(멈춘 HA VM은 켜지 않는다).
- **watchdog 준비(Linux).** 하드웨어 watchdog(`iTCO_wdt`, IPMI 등)가 없으면 `softdog`를 올린다:
  `echo softdog | sudo tee /etc/modules-load.d/softdog.conf && sudo modprobe softdog`.
  `ls /sys/class/watchdog/`와 `cat /sys/class/watchdog/watchdog0/identity`로 장치를 확인한다. watchdog 장치는 한
  프로세스만 열 수 있다. **공유 LVM 풀의 sanlock `wdmd`가 이미 쥔 장치는 쓸 수 없으므로** 그 노드에는 다른 장치(예:
  하드웨어 watchdog를 `watchdog_device = "/dev/watchdog1"`로)가 있어야 한다. softdog는 장치를 하나만 만든다.
  **`nowayout` 장치는 쓰지 않는다** — magic close가 듣지 않아 HA VM이 없어도 orbivisord를 멈추면 노드가 재시작된다
  (`cat /sys/class/watchdog/watchdog0/nowayout`이 0이어야 한다. 커널 `CONFIG_WATCHDOG_NOWAYOUT`이나 softdog의
  `nowayout=1`이면 1이다).
- **한계.** 라이브 마이그레이션이 진행 중인(보류 중인) HA VM은 리스를 잃어도 마이그레이션이 끝나거나 풀릴 때까지
  끄지 않는다(`process`에서는 긴 마이그레이션이 `fence_after_secs`를 넘길 수 있다. `watchdog`면 쓰다듬기를 멈추므로
  노드가 재시작된다). 실패한 마이그레이션이 원본을 멈춘(`paused`) 채로 남기면 HA VM은 이어서 실행되지 않는다(드묾 —
  끄고 다시 켠다).

**복구.** 리더(위트니스가 아닌 노드)가 1초마다 노드의 리스를 보고 다음 노드의 HA VM을 옮긴다.

- **언제.** 리스를 마지막으로 갱신한 뒤 그 노드의 `recover_after_secs`(기본 22초, process 12초)가 지난 노드, 또는 리더가
  전원을 끄고 꺼진 것을 확인한 노드(아래 "전원 끄기 펜싱"). 리스는 리더의
  메모리에만 있으므로 리더가 바뀌면 새 리더는 리더가 된 때부터 다시 센다. 그래서 노드가 죽고 VM이 다른 노드에서
  켜지기까지는 대략 `recover_after_secs` + 1초에 부팅 시간이다. 리더도 함께 죽었으면 선거 시간이 더해지고, 디스크
  잠금이 남아 있으면(아래) 그만큼 더 걸린다. `fencing = "process"`, `fence_after_secs = 2`(recover_after 4초)인
  자동 시험에서는 수십 초다. 펜싱이 꺼진 노드(`status.ha` 없음)의 VM은 옮기지 않는다.
- **무엇을.** 그 노드에 배정되고(`spec.node`) HA이고 원하는 상태가 running인 VM을 복구 우선순위(`ha.priority`, 0~100,
  기본 50)가 높은 것부터, 같으면 이름 순서로 옮긴다. 게스트가
  스스로 끄거나 사용자가 정지한 VM, HA 아닌 VM은 그 노드에 남는다. 옮기는 동안 리더는 그 노드의 리스 갱신을
  거부한다(펜싱 중 표시) — 그 노드가 돌아와 낡은 배정을 보고 VM을 켜지 않게 하려는 것이다. 복구 기록의 결과를
  모르면(시간 초과 — 나중에 커밋될 수 있다) 결과를 알 때까지 같은 기록을 다시 보내며 거부를 이어 가고, 리더가
  바뀌면 푼다.
- **어디로.** 스케줄러와 같은 규칙(온라인, 유지보수 아님, 위트니스 아님, 아키텍처·디스크 풀·브리지, 부하 점수가 가장
  좋은 노드 — HA 그룹 우선순위와 배치 규칙이 먼저다, 아래 "배치 규칙과 HA 정책"·"최적 배치")에 더해 HA 가능 노드이고 리스가
  그 노드의 `fence_after_secs`(계획 51 전 노드는 `recover_after_secs`)보다 새로운 노드다 — 함께 죽어 아직 옮길 때가 되지
  않은 노드로는 옮기지 않는다. 죽은 노드와 진행 중이던 마이그레이션의 대상 노드, 전원을 끄는 중이거나 끈 노드는 뺀다. 여러 VM을 옮기면 앞서 옮긴 VM의 메모리도 센다. 그런 노드가 없으면 VM은 그대로 두고 status에
  "HA cannot find a node for this VM"만 남긴 뒤 1초마다 다시 본다.
- **미리 고른 노드.** 복구할 노드가 없을 때 리더가 10초마다 노드마다 "이 노드가 죽으면" 그 노드의 HA VM을 같은 순서·규칙으로
  어디에 둘지 미리 계산해 둔다(리더 메모리, 30초 넘은 계산은 쓰지 않음). 복구 때 미리 고른 노드가 지금도 리스가 살아 있고 조건·
  hard 규칙에 맞으면 그대로 쓰고, 아니면 그 자리에서 다시 고른다(로그 debug에 쓴 수·다시 고른 수).
- **동시 시작.** 한 대상 노드에 시작이 몰려 디스크가 막히지 않게, 이 리더가 그 노드로 옮겼고 아직 켜지지 않은 VM이 클러스터
  설정 `ha_start_concurrency`(1~32, 기본 4)개면 그 노드로는 더 옮기지 않는다. 미리 고른 노드가 상한이면 그 자리에서 다시
  고르고(다른 노드가 나오면 그리로 옮긴다), 고른 노드가 상한이면 다음 VM은 죽은 노드에 남았다가 다음 틱에 다시 본다(그
  노드에서 우선순위 순서가 지켜지게 다음 후보로 돌리지 않는다). 옮긴 VM은 그 노드에서 켜지거나 다른 노드로 가거나 꺼지거나
  120초가 지나면 세지 않는다. 리더가 바뀌면 처음부터 센다.
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
  원래 노드로 되돌아가지 않는다(필요하면 마이그레이션한다). HA 그룹의 VM은 우선순위가 더 높은 그룹 노드로 되돌아간다
  (아래 "배치 규칙과 HA 정책"의 되돌아가기).
- **한계.** 죽은 노드에만 있던 UEFI 변수는 잃는다 — 대상 노드는 변수를 그 노드에서 받으려다 실패하면 경고를 남기고
  자기 노드의 옛 파일(없으면 템플릿)로 켠다. TPM VM은 TPM 상태의 사본이 있는 노드에서만 복구하고, 없으면 켜지 않는다
  (사본은 60초 시점 — [vTPM과 Secure Boot](vms.md#vtpm과-secure-boot)). HA 아닌 VM은 노드가 돌아올 때까지 그 노드에 묶여 있다. Ceph RBD에는
  디스크 잠금이 없어 펜싱만 믿는다(Orbivisor는 RBD 배타 잠금·blocklist를 쓰지 않는다). NFSv3에서는 죽은 노드의 잠금이
  그 노드가 돌아올 때까지 남을 수 있고, 그동안 VM은 "disk image is locked …; retrying"으로 기다린다. `process`
  펜싱 노드는 orbivisord만 멈추면(QEMU는 남음) HA VM을 끄지 못하므로 복구가 VM을 둘 돌게 할 수 있다(시험·개발용).
  복구 도중 리더가 다른 노드로 바뀌면 죽은 노드에 남은 그 VM의 작업 실패 처리와 기록 작업 `vm-ha-recover`가 빠질 수
  있다(VM은 옮겨진다). 끝나지 않은 작업은 그 노드가 돌아오거나 제거될 때 정리된다.

**전원 끄기 펜싱(선택).** 노드마다 BMC나 명령으로 전원을 끄는 방법(1~4개)을 두면, 리더는 그 노드의 리스가 그 노드의
`fence_after_secs`(기본 10초)를 넘게 끊겼을 때 방법을 차례로 써서 전원을 끄고, 꺼진 것을 확인하면 watchdog 제한을
기다리지 않고 곧바로 HA VM을 옮긴다(Proxmox·Pacemaker의 대역 외 펜싱). 노드가 죽고 다른 노드에서 VM이 켜지기까지 약
10초 + 끄기·확인 시간이다. 방법 하나에 최대 약 45초(끄기 15초, 확인 15초, 마지막 상태 읽기 15초)가 걸릴 수 있고, 그동안에도
watchdog 기다림(`recover_after_secs`)은 나란히 흐른다 — 끄기가 그보다 늦으면 watchdog 기다림으로 먼저 옮긴다.

```json
{"methods": [
  {"kind": "ipmi", "address": "10.0.10.12", "user": "admin", "password": "…"},
  {"kind": "redfish", "url": "https://bmc-node2.example.com", "user": "root", "password": "…", "system_id": "1"},
  {"kind": "command", "off": "pdu-ctl off \"$ORBIVISOR_NODE_NAME\"", "status": "pdu-ctl status \"$ORBIVISOR_NODE_NAME\""}
]}
```

- **종류.** `ipmi { address, port = 623, user, password, lanplus = true }`는 `ipmitool [-I lanplus] -H … -p … -U … -E chassis
  power off`로 끄고 `chassis power status`가 "Chassis Power is off"면 꺼진 것이다(비밀번호는 명령 인자가 아니라
  `IPMI_PASSWORD` 환경 변수로 넘긴다). **IPMI를 쓰면 모든 노드에 `ipmitool`이 있어야 한다**(어느 노드가 리더가 될지 모른다,
  `apt install ipmitool`, 경로는 `[ha] ipmitool`). `redfish { url, user, password, system_id?, ca_pem?, insecure = false }`는 Basic
  인증으로 `ComputerSystem.Reset`(`ForceOff`)을 부르고 `PowerState`가 `Off`면 꺼진 것이다. `system_id`가 없으면
  `/redfish/v1/Systems`의 시스템이 하나일 때 그것을 쓴다(여럿이면 오류, 시스템이 `url`과 다른 scheme·호스트·포트를 가리켜도
  오류). `http://`도 받는다. `url`은 scheme·호스트·포트뿐이다 — 사용자·비밀번호(`https://u:pw@bmc`, `user`·`password`에
  둔다)나 경로·쿼리·조각을 넣으면 **400**이다. `system_id`는 영문자·숫자·`.`·`_`·`-`만(`.`·`..`는 안 됨, **400**). Redfish
  요청은 프록시 환경 변수(`HTTPS_PROXY` 등)를 따르지 않는다. `command { off, status }`는 `/bin/sh -c`로 돌리고(`off`는 리더, `status`는 이웃 노드의 정기 확인·`fence-test`를 받은 API 노드·리더) 환경 변수
  `ORBIVISOR_NODE_NAME`·`ORBIVISOR_NODE_ID`·`ORBIVISOR_NODE_ADDRESS`(노드 API 주소의 호스트)로 끌 노드를 알린다. 명령의
  환경은 비우고 `PATH`와 이 셋만 넘긴다(orbivisord의 환경 변수는 넘기지 않는다). `off`는 0으로 끝나야 하고 `status`는 첫 줄에
  `off`나 `on`을 찍는다(그 밖은 모름). 명령은 자기 프로세스 그룹에서 돌고 제한 시간을 넘기면 띄운 자식까지 끝낸다. 명령이
  끝나면 백그라운드로 남긴 자식은 기다리지 않는다. PDU의 SNMP, iLO·iDRAC 전용 도구, 가상 환경의 API를 이것으로 부른다.
- **`command`는 root 명령이다.** `command` 방법은 리더·이웃 노드 등 **모든 노드에서 orbivisord 권한(root)으로 도는 셸 명령**이다.
  펜싱을 쓸 수 있는 admin은 모든 노드에서 root로 명령을 실행할 수 있는 셈이므로 admin 계정을 그만큼 아낀다.
- **오류 문구.** 모든 뷰어가 읽는 오류(작업 `node-power-off`, `status.fence_check`)에는 방법 종류와 일반 이유만 남긴다 —
  `ipmitool`·`command`는 종료 상태, Redfish는 HTTP 상태나 "connection failed"·"timed out", 시간 초과는 "timed out after
  15s". BMC 주소·호스트와 명령 출력(비밀을 담을 수 있다)은 그 노드의 경고 로그에만 남는다(`fence-test`도 같은 문구).
- **순서와 실패.** 앞 방법부터: 끄기(명령·HTTP 하나 15초 제한) → 1초마다 상태를 15초까지 → 꺼졌으면 끝, 아니면 다음 방법.
  끄기 요청이 실패해도(이미 꺼진 노드에 409를 주는 BMC 등) 그 방법으로 상태를 한 번 읽어 꺼져 있으면 꺼진 것으로 본다.
  모두 실패하면 경고 로그와 실패한 작업 `node-power-off`(이유는 작업 오류에)를 남기고 이 리더는 그 노드를 다시 끄지 않으며
  평소처럼 `recover_after_secs`(watchdog)를 기다린다 — 안전 쪽으로 물러난다. 실패가 그 기다림으로 이미 옮긴 뒤에 오면 오류가
  "power off failed; the node's VMs were already recovered after the watchdog wait: …"다. 성공하면 성공한 작업 `node-power-off`(방법)를
  남긴다. 리더는 꺼진 것을 확인한 뒤 그 노드에서 받은 리스 갱신이 없을 때만 복구하고, 그 노드의 리스가 다시 살아나면 기록을
  지운다(다시 장애가 나면 다시 끈다). 켜기로 된 HA VM이 없는 노드(꺼진 `state: stopped` VM만 있으면 `recover_after_secs` 뒤
  자리만 옮긴다), 얼린 노드, 계획 51 전 버전의 노드는 끄지 않는다. 리더가 바뀌면
  새 리더가 처음부터 다시 본다.
- **리더 쪽 문제에 대비.** 리더는 자기 리스도 그 노드의 `fence_after_secs`보다 새로울 때만 리스가 끝난 노드를 끈다(리더의 갱신이
  멈추면 모든 노드의 리스가 함께 끝나 보이므로). 또 한 임기에 끄기를 시작하는 노드는 HA 가능 노드의 소수(`(n - 1) / 2`, 적어도
  1대 — 3·4대면 1대, 5대면 2대)까지다(요청받은 끄기도 센다). 넘는 노드는 경고 로그를 남기고 watchdog 기다림으로 옮긴다.
- **다른 노드가 BMC에 닿아야 한다.** 끄는 것은 리더(위트니스가 아닌, 그 노드가 아닌 노드)이고, 전원 읽기는 이웃 노드(정기
  확인)·`fence-test`를 받은 API 노드·리더가 한다. 어느 노드가 리더가 될지 모르므로 모든 노드가 BMC에 닿아야 한다. BMC
  관리망이 노드마다 따로라 다른 노드에서 닿지 않으면 쓸 수 없다.
- **이웃 확인과 `fence_unreachable`.** 각 노드는 등록한 뒤 곧바로, 그 뒤 1시간마다, 자기 펜싱 설정이 바뀌면(1분마다 본다)
  이웃 노드 하나(온라인·위트니스 아님·이 버전인 다른 노드 가운데 id 순으로 다음)에게 노드 간 RPC로 자기 방법들의 전원 읽기를
  부탁해 결과를 `status.fence_check { at, by, error }`에 쓴다. 모든 방법이 읽지 못했으면 `error`가 있고 이것이
  `fence_unreachable` 경고다(`node list`의 FENCE `unreachable`, 웹의 "전원 펜싱 닿지 않음" 배지 — 마우스를 올리면 이유).
  이웃이 답하지 않으면 기록하지 않고 1분 뒤 다시 하고, 3번 이어 답하지 않으면 `error`에 "no neighbour could check the fence:
  …"를 써 오래된 결과가 남지 않게 한다. 펜싱이 없으면 `fence_check`도 없다.
- **`fence-test`.** `orbivisorctl node fence-test <노드>`(웹 "시험", admin)는 저장된 방법마다 전원을 읽기만 한다(끄지 않는다).
  연결한 API 노드가 그 노드가 아니면 스스로, 그 노드면 이웃에게 부탁해 읽는다(이웃이 없으면 **409** `no_fence_checker`, 답하지
  않으면 **502** `fence_check_failed`).
- **설정과 비밀번호.** 펜싱은 노드마다의 리소스(`node-fence`, 이름 = 노드 이름)이고 읽기·쓰기 모두 admin만, 위트니스는
  **400**, 쓰기는 모든 노드가 이 버전이어야 한다(**409** `cluster_upgrading`). `PUT`은 목록을 통째로 둔다. 비밀번호는 클러스터
  저장소에 그대로 두되 API·watch·로그가 내보내지 않고 `has_password`만 보인다. `PUT`에서 `password`를 빼면 같은 종류에 IPMI는
  주소·포트·사용자, Redfish는 `url`·`user`·`ca_pem`·`insecure`가 모두 같은 저장된 방법의 값을 두고(하나라도 바꾸면 다시 줘야
  한다 — **400**), 빈 글이면 지운다(웹 "저장된 비밀번호 지우기"). `node fence set -f`는 `node fence get --json`의 출력도
  받는다(`node`·`revision`·`has_password`는 빼고 보내고, `revision`이 있으면 `If-Match`로 보내 그 사이 바뀌었으면 **412**).
  노드를 제거하면 함께 지워진다.

**HA 용량 예약.** 클러스터 설정 `ha_reserve`를 두면 HA VM을 켤 때 장애 뒤 복구할 자리가 남는지 본다(VMware HA admission
control의 백분율 방식).

- **셈(메모리).** 대상 노드 = HA 가능·온라인·유지보수 아님·종료 표시 없음. 용량 = 그 노드들의 메모리 합, 예약 = `nodes:N`이면
  메모리가 가장 큰 N대의 합(1~8, 노드 수보다 크면 전부), `percent:P`면 용량의 P%(1~90), 사용 = 그 노드들에 자리 잡은
  **모든** VM(켜진 VM, HA 아닌 VM 포함, 마이그레이션 중이면 원본 한 번)과 유지보수(비우기)·종료 중(얼리기 빼고)인 HA 가능
  노드의 켜진 `started` HA VM(곧 대상 노드로 온다)의 메모리 합. 노드 조합을 다 따지지 않는다 — 실제 복구는
  노드마다의 남은 메모리로 놓으므로 조각나면 합으로는 남아도 못 놓을 수 있다.
- **언제 거절.** `started` HA VM을 켜는 요청 — 켠 채 만들기, 꺼진 VM을 켜짐으로 고치기, 켜진 VM의 HA를 `started`로 바꾸기
  (`stopped`·`ignored`에서), 켜진 HA VM의 메모리 늘리기, 꺼진 VM 시작 — 에서 사용 + 그 VM > 용량 − 예약이면 **409** `ha_capacity`(`details { capacity_mib, reserve_mib, used_mib, need_mib }`,
  웹은 "이 HA VM을 켜면 HA 복구용으로 비워 둔 메모리를 씁니다. …"). 마이그레이션은 합이 바뀌지 않아 보지 않고, HA 복구·
  되돌아가기도 보지 않는다. 이미 넘친 클러스터에서 도는 VM을 끄지는 않는다.
- **설정.** `orbivisorctl settings set ha_reserve=none|nodes:N|percent:P`, 웹 관리 › 일반·업데이트의 "HA 용량". 기본(`none`)은 저장하지 않고,
  바꾸려면 모든 노드가 이 버전이어야 한다.

**같은 노드 빠른 재시작과 게스트 감시.**

- **빠른 재시작.** `started` HA VM의 QEMU가 죽으면(크래시, kill) 그 노드의 reconciler가 재시작 창(10분) 안의 첫 크래시는
  기다림 없이 곧바로 다시 켠다(둘째부터는 지금의 백오프, `max_restart`를 넘으면 다른 노드로 — 위 "배치 규칙과 HA 정책").
- **게스트 감시(`ha.guest_watch`, VMware VM Monitoring).** `vm set --guest-watch SECS`(30~3600, 웹 "게스트가 멈추면 다시 켜기")를 둔
  VM은 QEMU 게스트 에이전트 채널(virtio-serial `org.qemu.guest_agent.0`)을 달고, VM이 돌 때 5초마다 `guest-ping`을 보낸다.
  이 QEMU에서 에이전트가 **한 번이라도 답한 뒤** 그 초 넘게 답하지 않으면 last_error "guest agent did not answer for Ns;
  restarting"을 남기고 강제로 끈 뒤 다시 켠다(크래시 한 번으로 센다). 게스트에 `qemu-guest-agent`가 돌아야 하고, 답한 적 없는
  VM은 감시하지 않는다. 일시 정지·마이그레이션 중에는 세지 않고, 일시 정지·재개 이벤트(QMP `STOP`·`RESUME`)마다 다시 센다.
- **재부팅.** 게스트가 다시 부팅하거나(QMP `RESET`) 꺼지기 시작하면(`SHUTDOWN`) 에이전트가 다시 답할 때까지 감시하지 않는다
  (부팅 중의 침묵을 멈춤으로 보지 않는다). 이벤트를 놓쳐도 부팅을 기다리도록 최솟값을 30초로 둔다.
- **메모리 스냅샷.** 감시 채널은 장치이므로 메모리를 담은 스냅샷은 찍을 때와 감시 있음/없음이 같아야 되돌린다(아니면 **409**
  `snapshot_mismatch` "set guest_watch to match the snapshot (on|off) before reverting" — VM을 멈추고 감시를 맞춘 뒤 되돌린다).
- **켜고 끄기는 멈춘 VM에서.** 채널은 감시를 켠 VM에만 단다(모든 VM에 달면 장치가 바뀌어 옛 노드와의 마이그레이션이 깨진다).
  그래서 실행 중인 VM의 수정에서 감시의 있음/없음이 바뀌면(감시를 켜거나 끄기, 감시를 둔 VM의 HA 끄기) **409** `vm_running`
  ("cannot turn guest_watch on or off for VM "web" while it is running; guest_watch can be turned on or off only while the VM
  is stopped")이다 — 도는 QEMU와 장치가 달라지면 비우기·종료·되돌아가기·옮기기의 라이브 마이그레이션이 실패한다. 시간만
  바꾸기는 실행 중에도 된다. 감시를 둔 채 HA만 멈추려면 `state: ignored`를 쓴다. 우선순위·감시를 기본이 아닌 값으로 두려면
  모든 노드가 이 버전이어야 한다(**409** `cluster_upgrading`).

**수동 watchdog 시험(Linux, root, 직접 할 때만 — 자동 시험·CI는 진짜 watchdog를 쓰지 않는다).** 버려도 되는 시험
노드에서 한다. `soft_noboot=1`이면 softdog는 제한 시간이 지나도 재시작하지 않고 커널 로그에
"softdog: Triggered - Reboot ignored"만 남긴다. 다른 프로세스(`wdmd` 등)가 watchdog를 쓰지 않는지 먼저 본다.

```bash
sudo modprobe -r softdog; sudo modprobe softdog soft_noboot=1   # 이미 올라와 있으면 내렸다가 다시
cat /sys/class/watchdog/watchdog0/identity                      # Software Watchdog
# orbivisord.toml: [ha] fencing = "watchdog", watchdog_timeout_secs = 10, fence_after_secs = 10
sudo orbivisord --config /etc/orbivisor/orbivisord.toml &
orbivisorctl vm create hatest --memory 256 --ha --start
cat /sys/class/watchdog/watchdog0/state                         # active (orbivisord가 열었다)
# 1) orbivisord가 멈춰 서면: 쓰다듬기가 멈춰 약 10초 뒤 "Triggered - Reboot ignored"
sudo kill -STOP "$(pidof orbivisord)"; sleep 15; sudo dmesg | grep softdog; sudo kill -CONT "$(pidof orbivisord)"
# 2) HA VM이 돌 때 정상 종료: 경고 로그와 "watchdog did not stop!", 약 10초 뒤 Triggered
sudo kill -TERM "$(pidof orbivisord)"; sleep 15; sudo dmesg | tail
# 3) HA VM이 없을 때 정상 종료: magic close로 state가 inactive, Triggered가 없다
sudo orbivisord --config /etc/orbivisor/orbivisord.toml &                   # 다시 띄우면 도는 hatest에 붙어 장치를 다시 연다
orbivisorctl vm stop hatest; orbivisorctl vm list               # PHASE stopped를 확인한 뒤
sudo kill -TERM "$(pidof orbivisord)"; sleep 15; cat /sys/class/watchdog/watchdog0/state
# 4) (셋 이상의 클러스터) 이 노드와 다른 멤버 사이의 cluster_listen(8007)을 막으면 약 10초 뒤 HA VM이
#    "fenced: lost the cluster majority"로 꺼지고 Triggered는 없다. 풀면 리스가 돌아와 다시 켜진다.
sudo modprobe -r softdog                                        # 끝나면
```

## 위트니스 노드

2노드 클러스터는 한 노드만 죽어도 과반수를 잃어 아무것도 바꿀 수 없다. 위트니스는 VM을 돌리지 않고 Raft 투표만
하는 orbivisord다(vSAN 위트니스, corosync-qnetd와 같은 자리). **일반 노드 둘과 위트니스 하나**를 권한다. 투표 멤버가
셋이므로 어느 한 노드가 죽어도 클러스터가 계속 돈다. QEMU가 필요 없으므로 작은 기계나 VM(이 클러스터 밖)에 둔다.

```bash
sudo tee /etc/orbivisor/orbivisord.toml >/dev/null <<'TOML'
name = "wit"
address = "10.0.0.13"
witness = true
TOML
sudo orbivisord --config /etc/orbivisor/orbivisord.toml &
orbivisorctl cluster join-token                  # 일반 노드의 API에 로그인한 곳에서
sudo orbivisorctl cluster join 'opl-join://…'    # 위트니스에서
orbivisorctl cluster status                      # WITNESS 열이 true, 경고 없음
```

- **합류만 된다.** 위트니스에서 `cluster init`은 거절된다("a witness cannot start a cluster; join an existing
  cluster instead"). 역할은 합류할 때 정해지고 바뀌지 않는다. 바꾸려면 `cluster leave` 뒤 설정을 고치고 orbivisord를
  다시 띄워 다시 합류한다. 멤버인 채 `witness` 값만 바꿔 다시 띄우면 orbivisord는 시작하지 않는다("this node joined
  as a witness but orbivisord.toml says witness = false; leave the cluster and join again to change the role", 반대도
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
- **표시.** `orbivisorctl node list`·`cluster status`에 `WITNESS` 열(`true`/`false`), `node get`에 `spec.witness`가
  있다. 웹은 노드 목록·클러스터 멤버에 "위트니스" 배지를 붙인다. `cluster status`는 투표 멤버가 둘이면
  2노드 경고("warning: a two-node cluster stops accepting changes when either node fails; add a third node")를,
  투표 멤버가 셋 이상인데 위트니스가 아닌 노드가 하나뿐이면 "warning: HA needs at least two non-witness
  nodes"를 보인다.
  위트니스의 `ADDRESS`(`node list`·`cluster status`)는 설정의 API 주소(`address:listen 포트`)이지만 위트니스는
  그 포트를 열지 않는다.

## 배치 규칙과 HA 정책

VM을 어느 노드에 둘지 정하는 **배치 규칙**과, HA VM을 어디로 복구하고 몇 번까지 다시 켤지 정하는 **HA 정책**(HA 그룹,
상태, 재시작·옮기기 횟수)이다(Proxmox HA의 그룹·규칙·`max_restart`/`max_relocate`, vSphere DRS의 VM-VM·VM-호스트 규칙과
같은 자리). 처음 놓기(만들기·시작), 마이그레이션, 비우기, HA 복구가 같은 조건·점수 함수를 쓴다(점수가 노드의 실제 CPU·메모리
부하를 보는 것과 켠 뒤 옮기는 균형 조정기는 아래 "최적 배치"). 객체 모양의 `ha`,
HA 그룹·배치 규칙 만들기·고치기, 종료 정책 바꾸기는 **모든 노드(위트니스·관찰 노드 포함)가 이 버전**이어야 한다(아니면
**409** `cluster_upgrading` — 노드가 알리는 기능 `ha-placement`를 본다).

```bash
# 배치 규칙: hard(affinity, anti-affinity, node-affinity, node-anti-affinity)와 soft-(벌점)
orbivisorctl placement-rule create spread --kind anti-affinity --vm web1 --vm web2
orbivisorctl placement-rule create db-rack --kind node-affinity --vm-tag app=db --node-tag zone=rack-a --ha-may-violate
orbivisorctl placement-rule create near --kind soft-affinity --vm app --vm cache --weight 80
orbivisorctl placement-rule list | get | set | enable | disable | delete
orbivisorctl node label node1 zone=rack-a ssd=true [--rm KEY]   # 노드 태그 = 노드 레이블
# HA 그룹과 VM의 HA 정책
orbivisorctl ha-group create rack-a --node node1:2 --node node2:1 [--restricted] [--nofailback]
orbivisorctl vm set db --ha-group rack-a --ha-state started --max-restart 2 --max-relocate 1
orbivisorctl vm migrate db node3 --force          # 관리자만: hard 규칙을 넘어 옮김(감사 기록)
orbivisorctl vm get db                            # status.rule_violations, status.ha
# 호스트 종료 정책
orbivisorctl settings set shutdown_policy=conditional|freeze|failover|migrate
orbivisorctl node unfreeze node2                  # 돌아오지 않는 얼린 노드
```

웹은 클러스터·가용성 › 자동 복구(HA) 탭과 클러스터·가용성 › 배치 규칙·자동 균형 › 배치 규칙 보기(목록·만들기·편집·삭제, 누구나 읽고 admin만 바꿈), VM 만들기·편집의 HA 절(상태·그룹·횟수),
VM 상세 요약의 고가용성·HA 그룹·어긴 배치 규칙·HA 오류, 마이그레이션 대화상자의 "배치 규칙 무시"(admin), 노드 화면의
"레이블" 열과 "레이블 편집"(연 사이 다른 곳에서 레이블이 바뀌었으면 저장하지 않고 다시 열라고 알림)·"얼림" 배지·"얼림 풀기", 설정의 "호스트 종료 때 HA VM"이다. API는 `/api/v1/ha-groups`,
`/api/v1/placement-rules`(목록·조회·만들기 `POST`·통째로 바꾸기 `PUT`(If-Match)·`DELETE`, watch 종류 `ha-group`·`placement-rule`),
VM `spec.ha`, `status.rule_violations`·`status.ha`, `POST /api/v1/vms/{key}/migrate`의 `force`, 설정 `shutdown_policy`,
Node `status.shutdown`과 `POST /api/v1/nodes/{key}/unfreeze`다.

**배치 규칙.** 이름(리소스 이름)과 `kind`, VM(`vms` 이름 목록 또는 `vm_tag`), VM과 노드 규칙이면 노드(`nodes` 또는
`node_tag`), soft 규칙의 `weight`(1~100, 없으면 50), `enabled`(기본 켬, 끈 규칙은 없는 것으로 봄), hard 규칙의
`ha_may_violate`다. 모든 VM(HA 아닌 VM 포함)에 적용된다.

- **종류.** `affinity`(반드시 같은 노드), `anti-affinity`(반드시 서로 다른 노드), `node-affinity`(반드시 이 노드들 중),
  `node-anti-affinity`(이 노드들은 안 됨), 그리고 각각의 `soft-` 형태(되도록). VM끼리 규칙은 VM이 둘 이상이고 노드를 주지
  않는다. 이름으로 적은 VM은 아직 없어도 된다(나중에 만들 VM). 나쁜 모양은 **400**.
- **태그는 레이블이다.** `vm_tag`·`node_tag`는 `key=value` 하나이고 그 레이블 값이 같은 VM·노드를 고른다. 노드에는 따로
  `tags`가 없고 노드 레이블을 쓴다 — `[node] zone`(대규모 클러스터)은 레이블 `zone`이 되므로 `--node-tag zone=rack-a`로
  장애 영역을 고른다. 노드 레이블은 `orbivisorctl node label`, 웹 노드 화면, `PUT /api/v1/nodes/{key}`의 `labels`로 고친다
  (`zone`은 설정에 있으면 노드가 등록할 때 설정 값으로 되돌린다). 같은 `zone`·`rack` 레이블이 웹 클러스터 지도의 판·줄이 되고,
  지도의 "추천" 레이어가 최적 배치 추천을 화살표로 보인다([클러스터 지도 화면](web-ui.md#클러스터-지도-화면)).
- **자리.** 원하는 상태가 running이고 노드가 정해진 VM이 그 노드를 차지한다(라이브 마이그레이션 중이면 원본과 대상 둘
  다). 꺼진 VM은 세지 않는다.
- **hard 규칙은 조건이다.** 만들기(켠 채)·수정·시작·마이그레이션이 노드 조건(온라인·유지보수·아키텍처·풀·브리지·방화벽·HA
  가능)은 맞는데 hard 규칙 때문에 놓을 노드가 없으면 **409** `placement_rule`(`details.rules`에 규칙 이름), 노드 조건부터
  맞는 노드가 없으면 지금처럼 **409** `no_node`다. 노드를 직접 준 요청(`--node`, 마이그레이션 대상)이 규칙을 어기면 409,
  규칙을 어기는 노드에 붙어 있는 꺼진 VM을 켜면 다른 노드를 고른다(없으면 409). 규칙을 새로 만들어도 이미 놓인 VM은
  옮기지 않는다 — 어긴 규칙이 VM 상태에 보인다.
- **soft 규칙은 벌점이다.** `soft-affinity`는 다른 노드에 있는 구성원마다, `soft-anti-affinity`는 같은 노드의 구성원마다,
  soft 노드 규칙은 어기면 `weight`. 노드 고르기 순서는 hard 규칙 통과 → HA 그룹 우선순위(그룹 VM만) → 벌점이 적은 노드 →
  부하 점수가 좋은 노드(아래 "최적 배치") → 미할당 메모리가 많은 노드 → id가 작은 노드다. 어길 수밖에 없으면 어기고 놓는다.
- **모순은 422.** 켜진 hard 규칙끼리 같은 두 VM이 `affinity`(이어진 묶음, 추이적)와 `anti-affinity`에 함께 있거나 한
  `affinity` 묶음이 허용하는 노드(`node-affinity`의 교집합 − `node-anti-affinity`)가 비면 만들기·고치기를 **422**
  `rule_conflict`(`details.rules`)로 거절한다. 태그는 지금의 레이블로 푼다 — 그 뒤 레이블을 바꿔 생긴 모순은 놓을 곳
  없음(409)이나 위반으로 보인다.
- **affinity 묶음은 함께 옮긴다.** 자리 잡은 hard `affinity` 상대가 있는 VM 하나만 옮기는 마이그레이션은 **409**
  `placement_rule`이다. 비우기는 옮길 VM과 같은 노드의 hard affinity 상대를 한 묶음으로 보고 묶음 전체가 들어가는 노드(메모리는
  합)를 골라 차례로 옮긴다(중간의 잠깐 위반은 받아들인다). 규칙 때문에 받을 곳이 없으면 그 VM들은 `placement rule …`로
  남는다(규칙에 맞는 노드가 메모리 때문에만 막혔으면 — 앞 구성원이 옮겨 간 노드에 자리가 없는 짝 등 — `no node with capacity`).
  한 구성원이라도 디스크를 옮길 수 없으면(로컬 풀 디스크와 스냅샷, 디스크 복사를 모르는 노드) 묶음 전체를 남긴다 — 막은 구성원은
  그 이유로, 나머지는 `affinity partner X: <이유>`로. 먼저 옮기던 구성원이 옮겨지지 못하면 나머지는 혼자 옮기지 않는다
  (`affinity partner X could not move`). 앞 구성원이 옮겨진 뒤 마이그레이션이 실패한 구성원은 그 노드에 남고(앞 구성원을
  되돌리지 않는다) 갈라진 묶음은 `rule_violations`에 보인다.
- **`--force`.** 관리자(배치 규칙 쓰기 권한)는 마이그레이션의 `force`로 hard 규칙(restricted 그룹 포함)을 넘을 수 있다.
  노드 조건과 메모리는 그대로 본다. 다른 역할은 **403**이고, 넘은 규칙은 감사 기록 detail `force rules=<이름들>`에 남는다.
- **`rule_violations`.** 리더가 1초마다 자리 잡은 VM의 hard·soft 위반을 계산해 바뀐 VM의 `status.rule_violations`(규칙
  이름, restricted 그룹 밖이면 `ha-group:<그룹>`)만 쓴다. 꺼진 VM은 비운다. 위반을 저절로 바로잡는 균형 조정은 아직 없다.

**HA 그룹.** `nodes: [{ node, priority }]`(0~1000, 클수록 먼저), `restricted`, `nofailback`. HA VM은 `ha.group`으로 그룹을
쓴다(없는 그룹이면 **400**, VM이 쓰는 그룹 삭제는 **409** `group_in_use`).

- 그룹 VM은 처음 놓기·복구·비우기에서 우선순위가 높은 그룹 노드를 먼저 고르고, 그룹 밖 노드는 가장 낮다. `restricted`면
  그룹 밖 노드에 놓지 않는다(놓을 곳이 없으면 켜지 않는다).
- **되돌아가기(failback).** 그룹이 있고 `nofailback`이 아닌 실행 중 HA VM이 지금 노드보다 우선순위가 높은 그룹 노드(리더가
  30초 넘게 리스가 살아 있는 것을 본, 조건·hard 규칙에 맞는 노드)를 가지면 리더가 그 노드로 라이브 마이그레이션 작업을 만든다.
  클러스터에 한 번에 하나이고 끝나기를 기다린다. 거절되면(유지보수 노드, macOS(HVF), 로컬 디스크 등) 그 VM은 60초 뒤에 다시
  보고(로그는 한 번), 마이그레이션 작업이 실패한 VM은 이 리더가 다시 하지 않는다. 재시작 횟수를 넘겨 옮겨 온 VM은 옮기기 창(10분)
  안에서는 되돌아가지 않는다(옮겨 온 노드로 돌려보내면 다시 크래시해 `error`가 될 수 있다). 창이 지나 옮겨 떠난 노드
  (`status.ha.relocated_from`)로 되돌아가면 옮기기 한 번으로 센다 — 그 노드에서 다시 크래시하면 기본값(`max_relocate` 1)에서
  `error`로 끝난다(되돌아가기와 옮기기가 10분마다 끝없이 돌지 않게). 다른 노드로의 되돌아가기(노드 장애 복구 뒤 등)는 세지
  않는다. 그룹 없는 HA VM은 되돌아가지 않는다.

**HA 정책(`ha`).** `ha`는 옛 bool(`true`/`false`)이나 객체 `{ "state": "started"|"stopped"|"ignored", "group": …,
"max_restart": 1, "max_relocate": 1, "priority": 50, "guest_watch": { "timeout_secs": 60 } }`(빠진 값은 기본, 횟수는 0~10,
`priority`·`guest_watch`는 위 "HA VM과 자기 펜싱")다. 정책이 기본과 같으면 저장할 때 `true`로 쓰므로 단순
HA VM과 이 버전 전의 VM은 바이트까지 옛 모양이고, `--ha`/`--no-ha`와 웹의 HA 스위치는 그대로다. 기본과 다른 객체는 모든
노드를 올린 뒤에만 쓸 수 있다.

- **상태.** 켜고 끄기는 지금처럼 `spec.power` 하나다. `started`: 지금의 HA(노드 장애 때 다른 노드에서 켬, 횟수).
  `stopped`: 노드 장애 때 다른 노드로 옮기기만 하고 켜지 않는다(꺼진 VM도 옮긴다). `ignored`: HA를 잠시 끈다 — 펜싱·복구·횟수를
  하지 않고 HA 가능 노드 조건도 없다(설정은 남는다). 없음·`ignored`에서 `started`·`stopped`로 바꾸는 것은 HA 켜기와 같아 VM이
  멈춰 있을 때만 된다(**409** `vm_running`).
- **재시작·옮기기 횟수.** `started` HA VM의 크래시는 `restart` 정책과 관계없이 다시 켠다(게스트가 스스로 끄면 꺼진 채 둔다).
  같은 노드에서 10분 안의 크래시가 `max_restart`를 넘으면 그 노드는 더 켜지 않고 `status.ha.relocate_from`으로 옮겨 달라고
  하며, 리더가 그 노드를 뺀 노드로 옮기고 `status.ha.relocations`(옮긴 시각, 10분 창)에 더한다. 창 안에서 이미
  `max_relocate`번 옮겼으면 원하는 상태를 stopped로, phase를 failed로 두고 `status.ha.error`("gave up after N relocations")를
  남긴다(`error` 상태) — 원인을 고친 뒤 VM을 다시 시작하면 지워진다. 노드 장애 복구는 횟수에 세지 않는다. 옛 노드가 남아
  있으면 이 동작 대신 지금의 재시작 정책·백오프다. 옮기기 창은 리더의 고정 600초이고, 재시작 창은 각 노드 reconciler의
  재시작 백오프 창(기본 600초)이다.
- **복구가 막히면.** hard 규칙 때문에 놓을 곳이 없으면 켜지 않고 `status.ha.error`와 last_error
  "HA cannot place this VM: placement rule …"을 남긴 채 1초마다 다시 본다(자리가 나면 옮기고 지운다). 막는 규칙이 모두
  `ha_may_violate`면 위반이 가장 적은 노드에 켜고 경고 로그와 `rule_violations`를 남긴다. 같은 죽은 노드의 VM은 앞에서
  옮긴 VM을 다음 VM의 규칙·메모리에 센다(affinity 상대는 첫 VM을 따라간다).

**노드 정상 종료 정책.** 클러스터 설정 `shutdown_policy`(기본 `conditional`)는 **호스트가 꺼지는 중(재부팅·전원 끄기)일
때만** 움직인다. `systemctl stop orbivisord`·업그레이드 재시작은 지금 그대로(VM 계속 돎, 위 watchdog 경고)다.

- **판별.** orbivisord가 SIGTERM을 받으면 `systemctl list-jobs`를 보아 `reboot.target`·`kexec.target`·`soft-reboot.target`의
  시작 작업이면 재부팅, `poweroff.target`·`halt.target`(또는 `shutdown.target`만)이면 끄기, 없거나 명령이 실패하면(macOS 포함)
  호스트 종료가 아니다(Proxmox `pve-ha-lrm`과 같은 방식).
- **정책.** `conditional` = 재부팅이면 `freeze`, 끄기면 `failover`. `freeze`: 노드가 `status.shutdown` 표시를 남기고 이
  노드의 실행 중 HA VM을 ACPI(`[reconciler] acpi_timeout_secs`, 기본 60초) 뒤 강제로 끈다(원하는 상태는 running 그대로라
  돌아오면 그 노드에서 다시 켠다). 리더는 이 표시가 있고 오프라인인 **얼린** 노드의 HA VM을 옮기지 않는다. 표시가 있는
  노드는 꺼지기 전에도 복구·옮기기·되돌아가기·비우기·처음 놓기의 대상이 아니다(다시 등록하면 표시가 지워진다). `failover`:
  같은 방법으로 끄고 끝난다 — 리더가 평소처럼 `recover_after_secs` 뒤 다른 노드에서 켠다. `migrate`: 노드를 유지보수로 바꾸고
  비우기를 시작해 끝나거나 10분이 될 때까지 기다린 뒤 남은 HA VM을 failover처럼 끈다. 표시를 쓰지 못하면(과반 없음) 경고하고
  failover처럼 끈다.
- **돌아오면.** 다시 등록할 때 표시를 지우고, `migrate`가 켠 유지보수만 끈다 — 노드가 꺼진 동안 관리자가 유지보수를 다시
  켰어도 표시가 `migrate`가 유지보수를 켠 것이면 등록이 끄므로, 계속 유지보수로 두려면 돌아온 뒤 다시 켠다. 돌아오지 않을 얼린 노드는
  `orbivisorctl node unfreeze`(웹 "얼림 풀기", admin, 오프라인일 때만 — 아니면 **409** `node_online`)로 표시를 지우면 리더가
  죽은 노드처럼 복구한다.
- **systemd.** 정책이 VM이 살아 있을 때 돌도록 QEMU 스코프가 `Before=orbivisord.service`를 갖고(멈출 때는 역순이라 orbivisord가 먼저
  끝난다) 망·원격 파일시스템보다는 먼저 멈추도록 `After=network-online.target remote-fs.target`도 갖는다. 유닛은
  `TimeoutStopSec=15min`이다 — 이 한도는 보통의 `systemctl stop`·`restart`에도 적용되어, 멈추지 않는 orbivisord를 systemd가
  강제로 끝내기까지 기다리는 시간도 15분이다. **이 버전 전에 켠 VM의 스코프에는 이 순서가 없으므로** 호스트 종료 때 orbivisord와
  함께 SIGTERM을 받는다 — VM을 한 번 껐다 켜야 순서가 붙는다(라이브 마이그레이션으로 다른 노드에 옮겨도 된다).
- **업그레이드 뒤 동작 변화.** 기본 `conditional` 때문에 업그레이드 뒤에는 **노드를 재부팅하면 그 노드의 HA VM이 다른 노드로
  옮겨지지 않고 꺼졌다가 노드가 돌아오면 그 노드에서 다시 켜진다**(전에는 리더가 `recover_after_secs` 뒤 다른 노드로 복구했다).
  재부팅 동안 HA VM을 계속 돌리려면 먼저 `node drain`하거나 `shutdown_policy=migrate`, 예전처럼 옮기려면 `failover`로 둔다.

**한계.** 위반을 바로잡는 균형 조정과 묶음 옮기기 API(뒤 계획), 종료 failover의 빠른 복구,
꺼진 `started` HA VM의 노드 옮기기, 호스트 종료 때 HA 아닌 VM의 ACPI 종료는 없다. 리스·`failback` 30초는 리더의 메모리에만
있으므로 리더가 바뀌면 다시 센다. 스토리지·VM 네트워크 검사가 실패한 노드는 복구·옮기기·되돌아가기의 대상에서 빠진다(아래
"HA 상태 검사").

## HA 상태 검사

리스는 노드가 관리 네트워크로 리더에 닿는지만 본다. 스토리지 네트워크(NFS, iSCSI·FC SAN의 공유 LVM, Ceph RBD)가 끊겨 VM
디스크 I/O가 멈추거나 VM 네트워크·본딩 포트가 죽어도 리스는 살아 있다. **HA 검사**는 노드가 링크·핑 대상·스토리지 풀 경로를
5초마다 검사해 결과를 리스 갱신에 실어 보내고, 리더가 `grace_secs`보다 오래 실패한 검사에 정한 대로 대응하는 것이다
(VMware의 VM Component Protection·네트워크 격리 검사, Pacemaker `ethmonitor`와 같은 자리, Proxmox VE에는 없다). 검사 만들기·
바꾸기와 리더의 경고 아닌 대응은 **모든 노드가 이 버전**이어야 한다(아니면 **409** `cluster_upgrading` — 노드가 알리는 기능
`ha-checks`를 본다).

```bash
# 스토리지: NFS 풀 경로가 멈추면 그 풀에 디스크가 있는 HA VM을 다른 노드에서 다시 켬(역할 기본)
orbivisorctl ha-check create nas --role storage --pool nas
# VM 네트워크: bond1의 링크가 끊기면 그 뒤 브리지에 NIC가 있는 HA VM을 라이브 마이그레이션(역할 기본)
orbivisorctl ha-check create vmnet --role vm --link bond1 --grace 15 --node node3=link:bond2
# 관리 네트워크: 게이트웨이 둘 가운데 하나도 답하지 않으면 노드 전원 끄기(전원 끄기 펜싱이 있어야 함)
orbivisorctl ha-check create mgmt --role management --ping 10.0.0.1,10.0.0.2@eno1 --need 1 --on-fail fence
orbivisorctl ha-check list | get | delete
orbivisorctl ha-check set vmnet --on-fail restart-elsewhere --no-node-overrides --disable
orbivisorctl node interfaces node1     # NAME KIND STATE CARRIER SPEED MASTER ADDRESSES, 본딩은 아래 줄에 포트
orbivisorctl node checks node1         # NAME STATE SINCE ERROR(-o json이면 status.checks)
orbivisorctl node list                 # CHECKS 열: - · ok · degraded · failing:<이름,…>
orbivisorctl task list                 # 리더의 대응 "ha-check", 본딩 알림 "bond-degraded"
```

웹은 클러스터·가용성 › 장애 감지(HA 검사) 탭(목록·만들기·편집·삭제, 누구나 읽고 admin만 바꿈 — 인터페이스는 모든 노드가 보고한
이름에서 고르거나 직접 쓰고, 풀은 공유 풀에서 고르며, 노드별 대상 줄을 더한다), 노드 화면의 "상태 검사" 열(실패한 검사 수 배지,
제목에 검사 이름과 오류, 누르면 대화상자)과 노드 메뉴의 "네트워크 인터페이스…"(종류·상태·링크·속도·소속·주소, 본딩은 모드와
`degraded`·`down` 배지, 포트는 아래 줄이고 `up`이 아니면 빨간 글자와 상태 문구, 그 아래 이 노드의 검사 결과·시작 시각·오류),
작업 목록의 "node-b의 HA 검사 nas: 다른 노드에서 다시 켜기"(설명과 옮긴 VM은 아래 줄)·"node-b의 본딩 bond0 포트 이상"이다. API는
`/api/v1/ha-checks`(목록·조회·만들기 `POST`·통째로 바꾸기 `PUT`(If-Match)·`DELETE`; watch 종류가 아니어서 웹은 바꾼 뒤 목록을
다시 읽는다), Node `status.interfaces`·`status.checks`, VM `status.ha.evict`, 기록 Task `ha-check`·`bond-degraded`다.

**검사(`HaCheck`).** 이름(리소스 이름)과 `role`, `target`, `on_fail`(없으면 역할 기본), `grace_secs`(10~600, 기본 10 — 검사
회차 둘보다 짧으면 한 번의 실패로 대응하므로 받지 않는다),
`nodes`(노드 이름 → 그 노드의 대상, 종류는 `target`과 같아야 함), `enabled`(기본 켬)다. 클러스터에 64개까지다.

- **역할.** `storage`(공유 스토리지로 가는 길), `vm`(VM이 쓰는 네트워크), `management`(노드의 관리 네트워크). 역할이 기본
  대응과 영향받는 VM을 정한다.
- **대상.** `link { iface }`: 인터페이스(본딩이면 본딩)의 링크. `ping { addresses, via?, need = 1 }`: 주소(1~8개)마다 ICMP 하나,
  `need`개 이상 답하면 통과. `pool { name }`: 공유 스토리지 풀(NFS·공유 LVM·RBD·`shared` 디렉터리)의 경로 — `storage`만 쓰고,
  로컬 풀은 **400** "pool checks need a shared pool (nfs, lvm-shared, rbd or shared dir)".
- **노드별 대상.** 노드마다 인터페이스 이름이 다르면 `nodes`로 덮는다(CLI `--node NAME=link:IFACE|ping:ADDR[,ADDR][@IFACE]|pool:NAME`).
  없는 노드 이름은 **400**. 위트니스는 검사하지 않는다. API는 인터페이스가 노드에 있는지 보지 않는다: 그 노드에 없는
  인터페이스의 검사는 `unknown`이므로 이름이 다른 노드는 덮어 써야 검사된다.
- **자동 관리 검사.** 각 노드는 클러스터 링크마다(`cluster_addresses`의 호스트, 없으면 `address` — 이름이면 IP로 풀어서; 아래 "클러스터 통신 다중 링크")
  `cluster-link-1`, `cluster-link-2` … 검사를 스스로 만든다: 그 주소를 가진 인터페이스의 `link`, 역할 `management`, 늘 경고만.
  리소스가 아니라 노드의 보고에만 있고(`cluster-link-`로 시작하는 이름은 **400**), 그 주소를 가진 인터페이스가 없으면
  `unknown`("no interface has address …")이다. 한 번 짝지은 인터페이스는 기억해 주소를 잃어도 계속 검사한다.

**판정.** 결과는 `ok`, `degraded`(통과 — 본딩 일부 포트, multipath 일부 경로), `failing`, `unknown`(이 노드에 해당 없음:
풀을 쓰지 않는 노드, 없는 풀, 지원하지 않는 OS — 실패도 통과도 아니다)이다.

- **링크.** sysfs `carrier`가 1이고 operstate가 `up`·`unknown`(dummy 등)이면 통과. 인터페이스가 없으면, 그 orbivisord가 이
  노드에서 한 번도 본 적 없는 이름은 `unknown`("interface X not found on this node" — 이름이 다른 노드가 혼자 실패해 과반 보호
  없이 VM을 옮기지 않게), 본 뒤에 사라졌으면 `failing`이다(본 이름은 저장된 노드 상태의 인터페이스 목록에서 시작하므로 다시
  시작해도 남는다). 본딩은 아래 포트 판정으로 본딩이 `down`이거나 본딩 자체의 carrier가 0이면 `failing`, `degraded`면
  `degraded`다. 브리지는 VM의 tap 포트가 carrier를 지키므로 업링크로 본다: 브리지 포트(`brif`) 가운데 물리 NIC·본딩·VLAN(다른
  장치 위의 장치)만 세고(tap·veth·`oplx*`는 뺌) 링크가 있는 것이 하나도 없으면 `failing`("bridge br0 has no uplink with a link
  (eno1 down)"), 그런 포트가 없는 내부 브리지는 브리지 자신의 carrier다.
- **핑.** 시스템 `ping`을 주소마다 동시에 부른다(Linux `ping -n -q -c 2 -i 0.2 -W 2 [-I via]` — 0.2초 간격 둘 가운데 하나라도
  답하면 그 주소는 통과, 한 번의 손실로 실패하지 않는다). 경로는 `[ha] ping`(기본 PATH의
  `ping`, ICMP 권한은 그 실행 파일의 것, 패키지는 `iputils-ping`에 기댄다). `ping`이 없거나 실행할 수 없으면 대상의 실패가 아니라
  `unknown`("ping not found: install iputils-ping")이다. `via` 인터페이스가 이 노드에 없으면 링크 검사와 같게 본 적이 없으면
  `unknown`, 본 뒤 사라졌으면 `failing`이다(macOS는 sysfs가 없어 `via`가 있는 핑이 `unknown`). macOS의 IPv6는 `failing` "not
  supported on this OS"다.
- **풀.** NFS(`{data_dir}/pools/<풀 id>`가 마운트돼 있지 않으면 "nfs pool is not mounted")·공유 디렉터리: 디렉터리 `stat`과
  `.orbivisor-health-<노드 id>` 파일을 새로 열어 64바이트 읽기(NFS의 close-to-open이 서버에 묻게)를 3초 안에 — 넘으면 `failing`
  "not responding"이고 그 작업이 끝날 때까지 새로 띄우지 않는다. 공유 LVM: iSCSI 타깃이면 `LOGGED_IN` 세션이 있어야 하고, VG의
  PV(`pvs`, 60초마다 — `/dev/disk/by-id/…` 같은 링크 이름은 따라간 장치로)가 multipath(dm) 장치면 `/sys/block/dm-N/slaves`의
  SCSI 장치 `state == running`(NVMe는 `live`)을 센다(슬레이브가 또 dm이면
  — multipath 위의 파티션 — 4단까지 따라 내려간다) — 0이면 `failing`, 일부면 `degraded`("2 of 4 paths running"). RBD: `rados …
  --client_mount_timeout 3 --rados_osd_op_timeout 3 stat orbivisor-health-probe`(8초)가 끝나거나 `stat`이 객체가 없다고 답하면
  (`error stat-ing …: (2) …` — OSD가 답함) 통과다. 그 밖의 오류는 ENOENT여도(지운 풀, 없는 keyring) `failing`이고 문구는 stderr의
  마지막 줄에서 앞의 시각·스레드 id를 뗀 것이다. `rados`가 없으면 `unknown`("rados not found: install ceph-common"). NFS·공유
  LVM·RBD 검사는 Linux만이다(macOS는 `unknown`). Orbivisor가 세운 Ceph의 풀도 같은 검사다(`client.orbivisor`·관리 conf — [Ceph 자동
  구성](storage.md#ceph-자동-구성): Ceph 전체가 멈추면 모든 노드의 검사가 함께 실패해 과반 보호가 VM을 옮기지 않는다).
- **본딩 포트.** `/sys/class/net/<bond>/bonding/slaves`·`mode`와 포트마다 `operstate`·`carrier`·`speed`, 802.3ad면
  `/proc/net/bonding/<bond>`의 활성 aggregator와 포트별 aggregator·상대 MAC을 본다. 포트 상태: `down`(carrier 0·MII down·
  operstate down), `lacp_mismatch`(활성 aggregator가 아님, 또는 상대 MAC이 `00:00:00:00:00:00`), `speed_mismatch`(올라온 포트
  가운데 가장 빠른 것보다 느림), `up`. 본딩은 모든 포트가 `up`이면 `up`, 일부만이면 `degraded`, 링크가 있는(`down`이 아닌) 포트가
  하나도 없을 때만 `down`이다 — LACP 상대가 없는 802.3ad(스위치에 LACP 설정이 없음)는 모든 포트가 `lacp_mismatch`여도 트래픽이
  지나가므로 `degraded`(통과)다. 뒤 버전이 더한 본딩·포트 상태는 `unknown`으로 읽고 이상으로 보이지 않는다.
- **인터페이스 보고.** 노드가 30초마다 `ip -j -d addr show`로 목록(이름·종류·operstate·소속·MAC·주소, `lo`·tap·`oplv*`/`oplx*`
  빼고)을 읽고, 5초마다 sysfs로 링크·본딩을 새로 고쳐 `status.interfaces`에 64개까지(넘치면 주소가 있는 인터페이스를 먼저) 쓴다.
  자동 검사의 인터페이스 찾기는 자르기 전 목록에서 한다. 검사 결과(`status.checks`)와 함께 바뀌었을 때만 쓴다. `ip`가 없거나
  (macOS) 실패하면 기록된 목록을 지우지 않는다.

**대응.** 리더가 1초마다 리스에 실린 결과를 본다(Raft 쓰기 없음). grace는 리더 시계로 재고(노드의 `since`는 화면용), 리더가
바뀌면 처음부터 다시 잰다. 한 실패(같은 노드·같은 검사)에 한 번 대응하고, 회복한 뒤 다시 실패하면 또 한다.

- **과반이면 경고만.** 같은 검사를 `ok`·`degraded`·`failing`으로 보고한 노드 가운데 `failing`이 과반이면 노드 문제가 아니라
  공유 장비(스토리지 자체, 스위치) 문제로 보고 모든 노드에서 경고만 한다(기록 Task "failing on k of n nodes; treating it as a
  shared failure"). 두 노드 가운데 하나만 실패하면 과반이 아니고, 보고한 노드가 하나면 늘 과반이다.
- **역할 기본과 허용.** `storage` → `restart_elsewhere`(warn·fence도 됨, `migrate`는 **400** — 디스크 I/O가 멈춘 VM은 라이브
  마이그레이션할 수 없다), `vm` → `migrate`(넷 다), `management` → `warn`(warn·fence만). 자동 검사(`cluster-link-*`)는 늘 경고다.
- **영향받는 VM.** 그 노드에 자리 잡은 켜진 VM 가운데 — `storage` + `pool`: 그 풀에 디스크가 있음, `storage` + 링크·핑: 공유
  풀에 디스크가 하나라도 있음, `vm` + 링크: NIC의 브리지가 그 인터페이스 뒤에 있음(인터페이스 자신이 브리지이거나 소속(`master`)을
  따라 올라간 브리지, 그 인터페이스나 그 위가 노드의 underlay 주소를 가졌으면 VXLAN 네트워크 모두), `vm` + 핑: `via`가 있으면
  링크와 같고 없으면 NIC가 있는 모든 VM, `management`: 그 노드의 HA VM(전원 끄기 기록용).
- **자동 대응은 HA VM만**(`ha` 상태 `started`·`stopped`). 나머지 영향받는 VM은 기록 Task 문구("also affects N VMs without HA")에만
  남는다.
- `warn`: 경고 로그와 기록 Task `ha-check`(succeeded) 한 번.
- `migrate`: 영향받는 HA VM을 이름 순으로 라이브 마이그레이션한다(원본 노드마다 진행 중 작업 `ha_start_concurrency`개까지, 나머지는
  다음 틱). 대상은 그 노드가 아니고 리스가 살아 있으며 같은 검사가 실패하지 않은 노드다. 옮기기 창(10분) 안에 이미
  `max_relocate`번 옮긴 VM은 옮기지 않고 `status.ha.error`("HA check … failed; not moving the VM: gave up after N relocations")만
  남긴다(그 노드에서 검사 실패가 끝나면 리더가 지운다). 옮기면 옮기기 한 번으로 센다. 거절된 마이그레이션은 그 VM만 60초 뒤
  다시, 받을 노드가 없으면 경고.
- `restart_elsewhere`: (1) 리더가 영향받는 HA VM의 status에 끄기 요청 `ha.evict`를 쓴다. `max_relocate`에 닿은 VM은 끄지 않고
  `ha.error`만 남기고(스토리지가 돌아오면 그대로 산다), 받을 노드(그 노드가 아니고 리스가 살아 있으며 같은 검사가 막지 않는 노드 —
  앞 VM과 앞서 요청한 VM이 갈 곳을 셈)가 없는 VM도 끄지 않는다(경고 한 번, 받을 노드가 생기면 그때 요청). 한 노드의 요청을 한
  번에 몇 개까지 내는 상한은 없다(영향받는 HA VM 모두). 받을 노드가 없어 아무것도 요청하지 않았으면 기록 Task도 쓰지 않는다.
  (2) 그 노드의 reconciler가 QEMU를 QMP 없이 곧바로 SIGKILL하고(10초 안에
  끝나지 않으면 — I/O에 멈춘 D 상태 프로세스 — 실패), 파일 볼륨(디렉터리·NFS 풀)을 열어 `fsync`해 이 노드 캐시에 남은 쓰기를
  내보내고(5초 안에 — SIGKILL 뒤 커널에 남은 쓰기가 나중에 다른 노드의 VM이 쓴 이미지를 덮지 않게, 끝나지 않은 작업은 끝날
  때까지 새로 띄우지 않음), 디스크 볼륨을 끈 뒤(공유 LVM의 배타 활성을 풀어야 다른 노드가 켠다) phase
  `failed`·"HA check … failed on this node; restarting elsewhere"와 옮겨 달라는 표시(`relocate_from`)를 쓰고 요청을 지운다. 끄기나
  내보내기·볼륨 끄기가 실패하면 오류만 남기고(껐으면 phase `failed`) 다시 해 본다. 그래서 **NFS에서 서버에 닿지 못하면
  `restart_elsewhere`는 사실상 전원 끄기로 끝난다**(아래 (4) — 이 노드의 캐시가 나중에 덮어쓰지 않게). 크래시로 세지 않고 원하는
  상태는 그대로다. (3) 리더가 재시작 옮기기와 같은
  길로 다른 노드에서 켠다(`max_relocate`에 셈). (4) 노드가 20초 안에 요청을 거두지 않으면(답 없음, 끄기 실패) 이유를 다시 본다:
  검사가 아직 있고 켜져 있으며 대응이 아직 `restart_elsewhere`이고 그 노드에서 실패하고 공유 실패가 아니면 노드마다 한 번(리더
  임기 안 — 같은 노드에 뒤에 낸 요청은 다시 넘기지 않는다), 그 노드에 전원 끄기 펜싱이 있으면 전원
  끄기를 요청하고(기록 Task "the node did not stop VMs a, b within 20s; requested a power-off of the node") 확인한 뒤 노드 장애처럼
  모든 HA VM을 복구하며, 없으면 경고와 실패 기록 Task "the node did not stop the VM and has no power fencing; leaving it"만 남긴다
  (요청은 남아 노드가 나중에 끄면 이어서 옮긴다). 이유가 사라졌으면(회복, 공유 실패로 바뀜, 검사를 지우거나 끄거나 대응을 바꿈)
  리더가 요청을 거둔다 — 넘긴 뒤에도 틱마다 본다. VM을 끄거나(`spec.power = stopped`) HA를 끄면 요청이 지워진다. 요청이 있는 동안은 되돌아가기가 그 VM을 건드리지 않고
  사용자 마이그레이션은 **409** `vm_busy`("the VM is being restarted elsewhere by an HA check")다. 이 Mac의 `ha-it`에서 carrier를
  끊고 다른 노드에서 VM이 켜지기까지 약 17초였다(grace 10초).
- `fence`: 그 노드에 전원 끄기 펜싱이 있으면 리스 만료 때와 같은 길(임기마다 노드 수의 소수까지, 리더 자신은 실패 Task)로 끄기를
  요청하고(기록 Task "…; requested a power-off of the node"), 확인되면 노드 장애처럼 모든 HA VM을 복구한다 — `max_relocate`에
  세지 않는다. 없으면 실패 기록 Task "no power fencing configured".
- **대상 거르기.** 노드 장애 복구·재시작 옮기기·되돌아가기·`migrate`는 그 VM에 영향을 주는 켜진 `storage`·`vm` 검사를
  `failing`으로 보고한 노드로 옮기지 않는다(grace와 상관없이, 과반 실패여도). 보고하지 않은 노드(옛 버전)·`unknown`은 막지
  않는다. 노드 장애를 복구하는 틱에는 검사 대응을 하지 않는다(다음 틱).

**본딩 알림.** 노드의 본딩 상태가 바뀌어 세 회차(15초) 이어지면 그 노드의 감사 로그에 `node.bond-degraded`·`node.bond-down`·
`node.bond-recovered`(target 노드, detail "bond0: eno2 down, eno3 lacp_mismatch")를 적는다 — 감사 로그는 API 노드만 있으므로 API를
끈 노드는 적지 않는다. `degraded`·`down`이 되면 기록 Task `bond-degraded`(포트와 상태)도 쓴다(모든 노드가 `ha-checks`일 때만).
저하·끊김의 감사·Task는 본딩마다 10분에 한 번까지이고(흔들리는 포트가 알림을 쏟아내지 않게, 그 사이는 로그만 — 다만
`degraded`에서 `down`으로 나빠지면 간격과 상관없이 알린다), 회복은 감사와 로그뿐이며 그 저하·끊김을 알렸을 때만 남긴다. 노드가 시작할 때 이미 `up`이 아닌 본딩도 같은 규칙으로 한 번 알린다. 본딩이 일부 포트로 살아 있는 동안은
경고만 하고 대응하지 않는다.

**업그레이드 중.** 옛 노드는 결과를 보내지 않고(리더에게는 결과 없음 = 대응 없음) 옛 리더는 결과를 버린다. 모든 노드가
`ha-checks`를 알기 전에는 검사를 만들 수 없고, 이미 있는 검사가 실패해도 리더는 대응 대신 경고 로그만 남긴다(임기마다 한 번).

**한계.** VXLAN 네트워크는 underlay 주소를 가진 인터페이스(또는 그 위)의 검사로만 이어진다. VLAN 하위 인터페이스의 부모는
따라가지 않는다(`master`만). `vm` + 핑은 `via`가 없으면 NIC가 있는 모든 VM에 영향을 준다. multipath는 sysfs의 SCSI 장치 상태라 경로
실패 감지가 SCSI 제한 시간만큼 늦다. 공유 LVM 검사의 `pvs`(5초 제한)가 한 회차를 늦출 수 있다. 클러스터 링크의 호스트 이름은
처음 풀린 IP를 기억하므로 DNS가 바뀌면 orbivisord를 다시 시작해야 자동 검사가 새 주소를 본다. 실제 본딩·LACP 어긋남·NFS 서버
멈춤·iSCSI 경로 끊기는 자동 시험 없이 손으로 확인한다(Linux CI는 veth·dummy·본딩으로 링크 판정만 본다). 공유 디렉터리 풀의
마운트가 사라지면 probe가 마운트 지점(루트 파일시스템)에 써서 통과하고, 읽기 전용 export는 probe 파일을 만들지 못해 늘 실패한다.
`restart_elsewhere`로 끈 VM은 받을 노드가 사라지면 원래 노드가 회복해도 다른 노드를 기다린다(재시작 옮기기와 같다).

## 최적 배치

노드를 고를 때 할당한 메모리만이 아니라 노드의 **실제 부하**(CPU·메모리)를 보고, 켠 뒤에도 노드 사이 부하가 기울면
**균형 조정기**가 라이브 마이그레이션을 추천하거나 스스로 옮긴다(VMware DRS, Proxmox VE의 CRS·로드 밸런서와 같은 자리).
처음 놓기·HA 복구·비우기 등 모든 노드 고르기가 같은 점수 함수를 쓰고, 노드 조건과 hard 배치 규칙은 그대로 거르는 쪽이다
(점수는 순서만 바꾼다). 설정 `placement`, VM 고정, 균형 조정기의 옮기기는 **모든 노드가 이 버전**이어야 한다(아니면
**409** `cluster_upgrading` — 노드가 알리는 기능 `placement`를 본다).

```bash
orbivisorctl placement status
# mode: recommend  policy: balance  threshold: 15%
# imbalance: 21.0% (cpu 18.2%, memory 23.8%)
# last run: 2026-11-14 09:30 UTC  next run: 2026-11-14 09:35 UTC
#
# NODE   CPU1   CPU5   MEM_USED  MEM_ALLOC  MEM_TOTAL  CPU%   MEM%   VMS  AGE
# node1  90.5%  88.0%  12.0GiB   10.0GiB    16.0GiB    88.0%  75.0%  5    3s
# node2  -      -      -         2.0GiB     16.0GiB    88.0%  12.5%  1    -     # 요약 없음: CPU는 다른 노드 평균, 메모리는 할당
orbivisorctl placement recommendations       # ID VM FROM TO REASON GAIN COST(ID는 id 끝 12자)
orbivisorctl placement apply ID              # 추천 적용(표의 ID, 또는 id 전체·앞부분): "vm web: moving from node1 to node2 (task …)"
orbivisorctl placement set --mode auto --policy balance --threshold 20   # admin, 준 값만 바꾼다
orbivisorctl vm set db --pinned               # 균형 조정기가 옮기지 않음(--no-pinned로 풂, vm create --pinned도)
orbivisorctl settings get                     # placement.mode 등(기본값이면 줄이 없다)
```

웹은 클러스터·가용성 › 배치 규칙·자동 균형 › 최적 배치 보기(누구나 읽음 — 15초마다 다시 읽는다): 머리에 모드·정책·문턱·불균형·마지막/다음
실행·쉬는 이유, 노드 부하 표(CPU·메모리 사용률 막대, 1분·5분 CPU, 사용·할당·전체 메모리, VM 수, 요약 나이 — 요약이 없으면
"요약 없음"), 추천 표(VM·원래 노드·옮길 노드·이유(균형이면 줄어드는 불균형, 규칙이면 어긴 규칙)·비용과 "적용" 단추 — VM을
옮길 권한(operator 이상)이 있어야 보이고, 누르면 Task 알림, 추천이 바뀌었으면 "추천이 바뀌었습니다"), auto면 "시작한
옮기기"와 그 Task, admin의 균형 조정기 설정 카드(모드·정책·문턱 1~50)다. 리더에 묻지 못하면 "클러스터 리더에서 배치 상태를
읽지 못했습니다"를 보인다. VM 만들기·편집의 "균형 조정기가 옮기지 않음" 스위치(실행 중에도 바꿈), VM 상세 요약의 "자동
옮기기: 제외(고정)", 감사 화면의 `placement.move`("균형 조정기의 자동 옮기기"와 무엇을 옮겼는지)도 있다. API는
`GET /api/v1/placement`(상태), `GET /api/v1/placement/recommendations`(추천 목록 — 둘 다 로그인한 누구나, 원격 클러스터 중계로도
읽는다), `POST /api/v1/placement/recommendations/{id}/apply`(**202** 마이그레이션 Task, 권한은 VM 마이그레이션과 같음),
설정 `placement`(`PATCH /api/v1/settings`의 `{"placement": {"mode", "policy", "threshold"}}` — 준 값만 바꿈), VM
`spec.placement.pinned`다. watch 종류는 아니다.

**부하 요약.** 노드의 측정기(아래 "지표")가 5초마다 요약을 만든다: 지난 1분·5분 노드 CPU 사용률(모든 코어 합 기준, 0.1%
단위), 사용 메모리(마지막 점), VM마다 지난 5분 CPU 사용률(vCPU 대비 — 100%를 넘을 수 있다)과 RSS. 마지막 점이 15초보다 오래된
VM(다른 노드로 옮겨 간 VM)은 빼고, VM은 RSS가 큰 것부터 256개까지다. 노드 CPU도 사용 메모리도 없는 요약(시작 직후·`/proc`
읽기 실패)은 보내지 않는다(리더도 그런 노드는 "요약 없음"으로 본다). 요약은 **새로 만들었을 때만** 리스 갱신(1초)에 한 번
실려 리더에게 가고(리더가 받지 못하면 다음 갱신이 다시 싣는다), 리더는 노드마다 마지막 요약을 메모리에만 둔다(Raft에 쓰지
않음). 리더는 30초보다 오래된 요약을 쓰지 않는다(그 노드는 "요약 없음"). VM 20개인 노드의 요약은 약 1.2 KB이고, 규모
시험에서 노드 300대의 리더가 받는 리스 갱신은 초당 약 10 KB에서 79 KB로 늘었으며 리더 RPC 수와 프로세스 CPU는 그대로였다.
위트니스도 보내지만 점수·균형 조정 어디에도 쓰지 않는다(리더와 다른 노드의 계산이 같게). 옛 리더는 요약을 버린다.

**점수.** 놓을 VM을 그 노드에 둔 뒤의 사용률로 매긴다.

- **CPU** = 노드의 5분 CPU 사용률 + 놓을 VM의 몫(VM의 5분 CPU 사용률 × vCPU 수 / 노드 CPU 수, VM 부하를 모르면 0 — VM
  부하는 5분까지의 요약에서도 가져오므로 리스가 끊긴 노드의 VM을 다시 켤 때도 센다). 한 번에
  여러 VM을 놓으면(HA 복구 등) 앞서 이 노드에 놓은 VM의 몫도 더한다. 요약이 없는 노드는 요약이 있는 노드들의 평균 CPU를 쓴다.
  후보 노드 가운데 요약이 있는 노드가 하나도 없으면 `balance`는 점수를 쓰지 않는다(예전처럼 미할당 메모리 순 — `pack`은 할당
  메모리로 모은다).
- **메모리** = max(실제 사용, 할당) / 노드 메모리. 실제 사용은 요약의 사용 메모리 ± VM RSS(요약이 없으면 할당만), 할당은 자리
  잡은 VM의 `memory_mib` 합 ± 놓을 VM의 메모리다.
- **`balance`**(기본): CPU 여유 + 메모리 여유(가중치 1:1, 설정으로 바꾸지 않음). **`pack`**: CPU 80%·메모리 90%를 넘지 않는
  노드 가운데 많이 쓴 노드가 앞이고(적은 노드에 모음), 넘치는 노드는 모두 그 뒤에 덜 쓴 것이 앞이다. 부하를 모르면 `pack`은
  할당 메모리로 노드 메모리의 90%까지 한 노드에 쌓는다.
- **순서**: hard 규칙·노드 조건 통과 → HA 그룹 우선순위(그룹 VM만) → soft 규칙 벌점이 적은 노드 → **점수**가 좋은 노드 →
  미할당 메모리가 많은 노드 → id. soft 벌점이 점수보다 앞이다(벌점 1~100이 사용률 점수보다 늘 크다). 노드 조건·hard 규칙·
  `ha_reserve`·HA 검사가 막은 노드는 점수와 상관없이 고르지 않는다.

**쓰는 곳.** 처음 놓기(만들기·시작 — `spec.node`가 비어 있거나 규칙을 어기는 노드에 붙은 꺼진 VM), HA 복구와 미리 고른
복구 대상, 재시작 옮기기, HA 검사의 `migrate`·`restart_elsewhere` 대상, 되돌아가기(그룹 우선순위가 먼저라 같은 우선순위끼리만
점수), 비우기 대상(옮기는 VM 자신의 부하는 모름 — 0과 할당 메모리)이다. 정책은 늘 이 노드가 읽은 설정이다. 리더가 아닌
API 노드는 노드를 고를 수 있을 때만(노드 없이 켠 채 만들기·고치기, 노드가 없거나 유지보수·규칙을 어기는 노드에 붙은 VM
시작, 비우기) 리더에 노드 사이 RPC로 부하를 묻고(1초 — 리더는 같은 임기에서 1초 동안 만든 답을 다시 쓴다), 묻지 못하면(옛
리더 포함) 할당 메모리만 보고 5초 동안 다시 묻지 않는다. 노드가 정해진 VM의 시작·레이블 바꾸기 등은 묻지 않는다. 스냅샷
되돌리기의 노드 고르기는 부하를 보지 않고, 되돌려도 고정은 지금 것을 지킨다(HA처럼).

**균형 조정기.** 리더가 5분마다(리더가 된 뒤 첫 회차는 1분 뒤) 돈다.

- **대상 노드**: 위트니스가 아니고 온라인이며 유지보수·종료 표시가 없고 macOS(HVF)가 아니며 신선한 요약이 있는 노드. 둘보다
  적으면 아무것도 하지 않는다. 끝나지 않은 마이그레이션의 VM은 옮겨 갈 노드의 메모리·CPU로 센다(같은 자리를 다시 약속하지
  않게).
- **불균형** = (노드 CPU 사용률의 표준편차 + 노드 메모리 사용률의 표준편차) / 2. 이것이 문턱(`threshold`, 백분율 1~50, 기본
  15)을 넘으면 균형 옮기기를 시작하고, 한 번 시작하면 문턱의 0.8배 아래가 될 때까지 계속한다(문턱 근처의 잡음으로 시작과 멈춤을
  되풀이하지 않게 — 리더 메모리). 노드가 많으면 한 노드만 뜨거워도 표준편차가 작으므로, 한 노드의 CPU나 메모리 사용률이
  평균보다 문턱의 2배 넘게 높으면 불균형이 문턱 아래여도 그 노드의 VM을 옮긴다.
- **욕심쟁이 고르기**: 사용률이 큰 노드 5대(뜨거운 노드만 넘으면 그 노드)의 옮길 수 있는 VM × 사용률이 작은 노드 10대 가운데
  (둘 다 지난 5분 안에 만든 마이그레이션이 끝난 원본·대상 노드는 뺀다 — 5분 평균 CPU가 옮긴 VM을 아직 원본에 센다)
  불균형을 가장 많이 줄이는 옮기기를 하나 고르고, 모델에 반영한 뒤 멈출 때가 되거나 받을 것이 없을 때까지 되풀이한다
  (recommend는 5개까지). 원본·대상의 사용률 차이(CPU·메모리 가운데 큰 것)가 문턱 이상인 것만 본다. 비용은 `max(RSS, 메모리)`이고,
  효과(줄어드는 불균형)가 0.5%포인트 이상이며 비용 GiB당 0.2%포인트 이상인 것만 받는다(노드 n대에서는 한 옮기기가 두 노드만
  바꾸므로 효과 × n / 2를 견준다). 같은 효과면 비용이 작은 것 → VM 이름 → 노드 id.
- **규칙 바로잡기**: 균형 옮기기보다 먼저, 살아 있는 지금 노드에서 hard 배치 규칙을 어기는 VM(`ha_may_violate`로 놓였거나
  레이블·규칙이 바뀐 것)을 규칙에 맞는 가장 나은 살아 있는 노드(HA 노드는 리스가 `fence_after`보다 새로움, 아니면 리스 30초 안)로 옮긴다
  (불균형·문턱과 상관없이, 아래 VM 조건은 같음). soft 위반은 바로잡지 않는다.
- **모드**: `off`(돌지 않음 — 점수는 그대로 쓴다), `recommend`(기본, 추천만 — 회차마다 새로 계산하므로 지난 추천 id는
  사라진다), `auto`(곧바로 보통의 `vm-migrate` Task를 만든다 — 진행 중 마이그레이션과 합쳐 2개까지, 거절되면 VM마다 info 로그
  한 번 뒤 다음 회차에 다시 본다). `pack` 정책이면 균형 옮기기를 하지 않고(모으기와 맞선다) 규칙 바로잡기만 한다(상태
  `paused: policy is pack`).
- **옮기지 않는 VM**: 실행 중이 아니거나 마이그레이션 중, 고정(`placement.pinned`), 노드 로컬 풀에 디스크가 있음(디스크 복사가
  필요), 끝나지 않은 스냅샷·되돌리기·복제·백업·전원·디스크 크기 늘리기·마이그레이션 Task가 있음, HA 끄기 요청·옮겨 달라는
  표시가 있음, **지난 1시간** 안에 만든 마이그레이션 Task·HA 복구 기록·HA 옮기기·이 리더의 옮기기가 있음, 자리 잡은 hard
  `affinity` 상대가 있음(묶음은 옮기지 않는다).
- **받는 노드**: 같은 노드·macOS 아님, 이 리더가 살아 있음을 계속 본 지 HA 되돌아가기 지연(30초) + 60초가 지남(돌아온 노드는
  HA가 먼저 되돌린다 — 리더가 바뀌면 다시 센다), 미할당 메모리가 VM 메모리 이상, 노드 조건, 이 VM에 영향을 주는 HA 검사가 실패하지 않음,
  계획한 옮기기를 넣고도 hard 규칙을 어기지 않음, soft 벌점이 지금보다 늘지 않음, HA 그룹 VM은 그룹 우선순위가 지금 노드
  이상(되돌아가기와 핑퐁하지 않게). `ha_reserve`는 메모리 합이라 옮겨도 바뀌지 않아 보지 않는다(마이그레이션과 같다).
- **쉬는 때**(상태 `paused`에 이유, 차례대로 첫 것): 모드 `off`, `placement`를 모르는 노드가 있음, 유지보수(비우기) 중이거나
  종료 표시가 있는 노드가 있음, HA VM이 있는 HA 노드의 리스가 그 노드의 `fence_after`보다 오래됨(복구 중일 수 있다), 켜진 HA
  그룹 VM을 HA가 더 우선인 살아 있는 노드로 곧 되돌릴 수 있음(`waiting for HA failback of VM "…"` — HA 되돌아가기처럼 미할당
  메모리·HA 검사·HVF·끄기 요청·affinity 상대도 본다. 리더가 그 노드가 살아 있음을 본 지 10분이 넘으면 HA가 되돌리지 못하는
  것으로 보고 기다리지 않는다), 요약이 신선한 대상 노드가 둘보다 적음, 클러스터에 끝나지 않은 마이그레이션이 2개 이상(누가
  만들었든).
- 추천 적용(`apply`)은 **리더 노드의 API에서만** 한다(HA 검사 결과와 균형 조정기의 상태가 리더 메모리에 있다). 리더가 아닌
  노드에 보내면 **409** `recommendation_stale`("recommendations are applied on the leader node …; send the request to node
  \"…\"")이고, 웹·CLI는 리더로 대신 보내지 않으므로 리더 노드의 웹이나 `--server <리더 API 주소>`로 다시 적용한다. 리더는 상태를 다시 읽어 그
  id가 없으면 **404** `recommendation_not_found`, VM이 고정이면 **409** `vm_pinned`, 끝나지 않은 마이그레이션이 2개 이상이면
  **409** `placement_busy`다. 추천은 회차 때의 상태로 계산했으므로 지금 상태로 위의 쉬는 때·VM·받는 노드 조건(살아 있음을 본
  시간 포함)을 다시 보고, 모드가 `off`이거나 쉴 이유가 있거나("balancer paused: …") 조건이 맞지 않으면 **409**
  `recommendation_stale`(문구에 이유 — 예: 같은 추천을 다시 적용하면 "busy with a task")이다. 적용은 한 번에 하나씩 다시 본다.
  그 밖은 마이그레이션과 같다(VM이 이미 다른 노드면 **409** `vm_moved`, `no_capacity`, `placement_rule`). 최적 배치 설정(모드·
  정책·문턱)을 바꾸면 리더가 게시판의 추천을 곧 비운다(다른 설정은 그대로).

**기록.** 옮기기는 보통의 `vm-migrate` Task로 보인다. auto는 리더의 감사 로그에 `placement.move`(대상 `vms/<이름>`, detail
`auto: web node1 -> node2 (balance), imbalance 21.0% -> 12.5%, cost 4096 MiB` — 규칙 바로잡기는 `(rule <규칙>)`)를 남긴다 — 감사 로그는 API 노드만 있으므로 API를 끈
리더는 적지 않는다. 적용 API는 보통의 감사(누가, detail `placement recommendation <id>`)다. 회차 요약(불균형·옮기기 수·쉬는
이유)은 바뀔 때만 info 로그 한 줄이다.

**업그레이드 중.** 모든 노드가 `placement`를 알아야 설정 `placement`·VM 고정을 쓸 수 있고, 균형 조정기는 그때까지 쉰다
("some nodes do not support placement yet (…)"). 기능이 모자란 클러스터가 받은 백업의 `placement`는 버리고 저장하고, 복제·
복원·가져오기로 만든 VM도 고정을 버린다(모든 노드가 알면 복제본은 원본의 고정을 받는다). 점수와
부하 요약은 기능과 상관없다(옛 노드는 요약을 보내지 않아 "요약 없음", 옛 리더는 요약을 버린다).

**한계.** affinity 묶음은 함께 옮기지 않는다(묶음의 VM은 균형 조정기가 건드리지 않음). `pack`은 넘치는 노드를 덜지 않는다.
네트워크·디스크 I/O는 보지 않는다. 사용 메모리는 호스트 전체라 ZFS ARC 같은 호스트 캐시가 큰 노드는 늘 바빠 보일 수 있다.
동시 2개는 부드러운 상한이다(auto와 적용, 사용자 마이그레이션이 겹치면 잠깐 넘을 수 있다). 새 리더는 다음 요약까지(5초쯤) 부하를 모르고, 리더가 바뀌면 받는 노드의 "오래 살아 있음"을
다시 세므로 1분 반쯤 균형 옮기기를 받는 노드가 없다. 끝난 Task가 1000개를 넘어 지워지면 "1시간 안에 다시 옮기지 않음"은 리더
메모리(리더가 바뀌면 사라짐)와 HA 옮기기 기록으로만 지킨다. 뒤 버전이 설정 `placement`에 키를 더하면 그 설정을 쓰는 동안 옛
노드는 리더의 상태를 읽지 못한다(부하 없이 놓고 `GET /placement`는 503). 문턱·효과 상수는 실제 다중 노드 부하로 다시 볼
것이다.

## 로컬 디스크 복제와 HA

노드 로컬 풀(공유가 아닌 디렉터리 풀, LVM-thin, ZFS)의 VM 디스크를 다른 노드에 주기적으로 복제해 두고, VM의 노드가
죽으면 HA가 **마지막 복제본으로** 다른 노드에서 VM을 켠다(Proxmox VE의 스토리지 복제 `pvesr`, VMware vSphere Replication과
같은 자리). 공유 스토리지 없이 노드 디스크만으로 HA를 쓸 수 있고, 복제본이 있는 노드로의 마이그레이션은 마지막 복제 뒤
바뀐 것만 보낸다. 복제는 VM 스펙의 `replication`(대상 노드마다 주기와 대상 풀, 3개까지)으로 켠다. 모든 노드가 이 버전이어야
한다(노드가 알리는 기능 `node-replication`, 아래 "업그레이드").

```bash
orbivisorctl vm create web --disk root --ha --replicate node2 --start   # 기본 주기 */15(15분)
orbivisorctl vm set web --replicate node2,*/5,pool=fast --replicate node3  # 목록을 통째로 바꾼다(되풀이)
# vm web: replication targets node2, node3
orbivisorctl vm set web --no-replicate                                  # vm web: replication off
orbivisorctl vm replicate web --node node2                              # 지금 한 번(대상이 하나면 --node 생략)
# vm web: replicated to node2 (12.0 MiB, incremental)                    처음이나 다시 전체면 (… , full)
orbivisorctl vm replication web                                         # -o json이면 상태 항목 그대로
# NODE   STATE    SYNCED                AGE    LAST                 SIZE     ERROR
# node2  ready    2026-11-15 06:00 UTC  4m12s  incremental 12.0MiB  16.0GiB  -
# node3  syncing  -                     -      -                    -        -
orbivisorctl vm get web                                                 # 표준 오류에 복제 줄
# replication: node2 ready 4m ago, node3 syncing
```

- **무엇을.** VM의 노드 로컬 풀 디스크를 대상 노드의 **같은 id 볼륨**(`{id}.qcow2`·`opl-{id}` — 계획 42 마이그레이션과 같은
  이름)으로 복제한다. 대상은 `NODE[,*/N][,pool=P]`: 주기 `*/N`은 **앞 시도(복제 Task를 만든 때)에서 N분 뒤**(1~1440, 기본 15 —
  시계에 맞추지 않는다, Proxmox의 달력 이벤트와 다름 — 시도가 곧바로 실패해도 주기 전에는 다시 하지 않는다), `pool`은 그
  노드에서 이 VM의 로컬 디스크를 모두 둘 노드 로컬 풀(없으면 디스크마다 같은
  이름의 풀, 형식은 마이그레이션처럼 대상 풀 종류가 받으면 그대로 아니면 raw). 공유 풀의 디스크는 복제하지 않는다(어느
  노드에서나 열린다 — 섞인 VM은 로컬 디스크만). 복제본은 클러스터 리소스가 아니라 볼륨 목록에 따로 보이지 않고, 지우기·
  붙이기 API로 닿지 않는다. 검사(**400**): 대상 3개까지, 겹치는 노드, `*/N` 모양, VM 자신의 노드("replication target cannot
  be the VM's own node" — 스케줄러가 고른 노드가 대상이어도), 없는 노드, 위트니스("node \"w\" is a witness and cannot hold
  replicas"), 로컬 디스크 없음("VM \"web\" has no disks on node-local pools; replication applies only to them"), 대상 노드에서
  쓸 수 없는 풀("node \"node2\" cannot use storage pool \"fast\" for the replica of disk \"root\""), 공유 풀("replica pool
  \"nfs\" is shared; replication needs a node-local pool"). 이미 저장된 대상(노드·풀이 같음)은 다시 보지 않아 대상 노드가 없어져도
  VM의 다른 고치기가 막히지 않고, 죽은 노드를 지우면(`DELETE /api/v1/nodes/{key}`) 모든 VM의 복제 대상과 상태에서 그 노드가 빠진다.
- **흐름.** 리더의 스케줄러가 `[replication] tick_secs`(기본 60초)마다 주기가 지난 대상을 골라(오래 기다린 것 먼저, 원본
  노드마다 **도는** 복제 Task가 `max_concurrent` 2개까지 — VM 줄에서 기다리는 Task는 세지 않고, 끝나지 않은 백업·마이그레이션·
  핫플러그·복제 Task가 있는 VM은 그 틱에 건너뛴다) VM의 노드에 **복제 Task**(`vm-replicate`)를 만든다. 처음은 전체, 다음부터는
  대상마다 따로 둔 dirty bitmap(`opl-repl-{대상}-{회차}`)으로 **바뀐 블록만** 보낸다. 원본 QEMU가 직접 대상의 NBD 서버에
  노드 인증서 TLS로 밀어 넣는다(`blockdev-backup` — 마이그레이션의 디스크 복사와 같은 길). 원본은 **플리싱**(fleecing)으로
  보낸다: 회차를 시작할 때 게스트를 잠깐(밀리초) 멈춘 채 디스크마다 copy-before-write 필터를 끼우고 그 시점의 디스크를
  snapshot-access 노드에서 읽어 보낸다. 게스트가 덮어쓰려는 옛 블록은 먼저 그 노드의 VM 상태 디렉터리에 둔 성긴 플리싱 이미지
  (`{data_dir}/vm/{id}/fleece-{i}.img`, 회차가 끝나면 지운다)로 옮긴다 — 그래서 **네트워크가 끊기거나 대상 노드가 죽어도 게스트
  쓰기는 실패하지 않고 그 회차만 실패한다**(필터가 30초 안에 옮기지 못하거나 공간이 모자라도 게스트 쓰기는 그대로 하고 회차를
  깬다). 플리싱 이미지는 `{data_dir}`에 놓이고 회차 동안 게스트가 처음 덮어쓰는 블록만큼, 최악에는 **디스크 크기까지** 자라므로
  그 파일 시스템에 여유를 둔다. 그 디렉터리의 남은 공간이 복제할 디스크 크기 합의 10%보다 적으면 회차를 시작하지 않고("not
  enough space in … for the fleecing images"), 보내는 동안에도 남은 공간이 1 GiB와 그 파일 시스템 크기의 5% 가운데 큰 쪽보다
  줄면 회차를 멈춘다("not enough space left in … for the fleecing images" — 플리싱을 떼고 지운다). 실패하거나 orbivisord가 다시 시작해
  끊긴 회차의 플리싱은 그 Task의 정리와 orbivisord가 도는 VM에 다시 붙을 때 뗀다. VM이 그 노드에서 돌지 않게 된 뒤(옮겨 갔거나 꺼짐)
  시작한 회차는 실패가 아니라 건너뛴다(Task는 성공이되 경고 "replication run skipped: the VM is not running on this node"를
  남기고 `vm replicate`가 그 경고를 알린다 — 스케줄러가 VM이 도는 노드에서 다시 만든다). 대상 노드는 회차마다 짧은 받기
  QEMU(`-machine none`)를 `[migration] ports`의 포트 하나로 띄우므로 **노드 사이에 그 포트 범위를 연다**(마이그레이션과 나눠
  쓴다). 증분은 대상의 **겹층**(`{data_dir}/replicas/overlays/`의 qcow2, backing = 복제본)으로 받고, 회차가 끝까지 온 뒤에만
  `qemu-img commit`으로 복제본에 합친다 — 회차가 중간에 끊겨도 복제본은 앞 회차 그대로 쓸 수 있다. 받는 쪽은 끝까지 받은
  회차를 기록하고 곧바로 답하며 합치기는 그 뒤에 따로 한다(그 VM의 다음 받기·승격만 기다리고 다른 VM의 받기는 막지 않는다 —
  합치기가 끊기면 다음 받기·청소·승격이 먼저 마저 합친다). 모든 디스크를 게스트를 멈춘 한 시점으로 떠 **crash-consistent**다(백업과
  같다 — `guest_agent.freeze_fs` VM은 그 직전에 파일 시스템을 얼린다, [게스트 에이전트](vms.md#게스트-에이전트)). 대역폭 상한은 `[replication] bandwidth_mib`(MiB/s, 디스크 수로 나눈다, 0이면 없음).
- **잃는 데이터.** HA 복구는 **마지막으로 끝난 회차의 시점**으로 켠다. 그 뒤의 쓰기는 잃는다 — 최악은 "주기 + 회차가 걸린
  시간"이다(15분 주기면 대략 15분 남짓). 복구 문구가 그 시점을 말한다: `recovering: node node1 failed; restarting from the
  replica on node node2 taken at 2026-11-15 06:00:00 UTC (writes after that are lost)`(VM `status.last_error`). 이 문구는 VM이 켜지면
  사라지지만 그 복구는 VM `status.last_recovery`(`from` 죽은 노드, `node` 켠 노드, `synced_at` 복제본의 시점, `at` 복구한 때)에
  남는다 — 다음 복제가 성공해도 남고 다음 복구가 덮는다. `vm get`(표준 오류)·`vm replication`이 `last recovery: 2026-11-15
  06:20 UTC — node1 failed; restarted on node2 from the replica taken at 2026-11-15 06:00 UTC (writes after that were lost)`로,
  웹 복제 카드가 같은 문장으로 보인다.
- **복제하지 않는 때.** **꺼진 VM은 복제하지 않는다**(예약은 건너뛰고 `vm replicate`는 **409** `vm_not_running`). 끄기 전
  마지막 회차 뒤 바뀐 것은 다시 켜서 도는 다음 회차가 담는다(qcow2 비트맵은 끄고 켜도 남는다). **raw 디스크**(LVM-thin·ZFS,
  dir 풀의 raw)의 비트맵은 QEMU 안에만 있어 **VM을 다시 켜면 다음 회차가 전체**다(증분 백업과 같은 한계). 비정상 종료 뒤의
  비트맵, 크기를 늘린 디스크, 실행 중 붙인 디스크도 그 디스크만 다음이 전체다. 전체 복사가 복제본 위에 쓰는 동안 그 대상은
  `syncing`이라 HA·마이그레이션이 쓰지 않는다.
- **스냅샷과 함께 쓰지 않는다.** 복제본은 디스크의 지금 층만 담는다. 그래서 복제하는 VM의 스냅샷 만들기(수동·예약)는
  **409** `snapshot_unsupported`("VM \"web\" is replicated to other nodes; replicas do not keep snapshots, remove the replication
  first"), 스냅샷이 있거나 예약 스냅샷을 둔 VM에 복제를 켜기는 **409** `replication_unsupported`다. 백업·증분 백업은 함께
  된다(비트맵이 따로다).
- **HA 복구.** 리더는 로컬 디스크 HA VM을 **쓸 수 있는 복제본이 있는 노드**(상태 `ready`, VM의 지금 로컬 디스크를 모두 같은
  크기(볼륨의 `size_bytes`)로 담음)로만 옮기고, 그 가운데 시점이 새로운 노드를 먼저 고른다(잃는 데이터가 적은 쪽). 꺼진 채 두는
  HA VM(`ha.state: stopped`)은 복제본으로 옮기지 않는다 — 꺼진 동안 복제가 멈춰 복제본이 낡았을 수 있고, 거기로 옮기면 옛 원본이
  돌아온 뒤 그 좋은 디스크가 낡은 복제본으로 덮이기 때문이다. 그런 VM은 그 노드를 기다리며 까닭이 남는다("HA cannot find a node
  for this VM: VM is stopped; its newest data is on node \"node1\""). 노드 로컬 풀 볼륨인데 어느 노드에 있는지 모르면(옛 기록) 그
  풀이 한 노드에만 있을 때만 그 노드에서 켜고 복제본으로 옮기거나 승격하지 않는다(복제본이 없을 때와 같은 까닭을 남긴다). 복제본
  없이(로컬 디스크가 그 노드에 있다) 복제 대상 노드로 옮기면 그 노드는 VM의 노드가 되어 복제 대상에서 빠진다. 펜싱·기다림·우선순위는 지금과
  같다 — 복제본 승격도 리스 만료나 전원 끄기 확인 뒤에만 일어난다. 대상 노드는 VM을 켜기 전에 복제본을 **승격**한다(받다 만
  겹층을 버리고 볼륨의 위치를 자기로 고침 — 쓸 복제본이 없으면 켜지 않고 "disk \"root\" has no complete replica on node
  \"node2\""; 받기 QEMU 끄기·겹층 합치기·클러스터 쓰기 같은 일시 오류면 끄지 않고 잠시 뒤 다시 해 본다). 그리고 **복제 방향을 뒤집는다**: `spec.replication`에서 node2가 옛 원본 node1로 바뀌고 node1은 `stale`(옛
  원본) 대상이 된다. node1이 돌아오면 청소가 그 디스크를 복제본으로 바꾸고 다음 node2→node1 회차가 **전체**를 복사한다
  (갈라진 쓰기를 비트맵으로 알 수 없다). node1이 돌아오기 전에는 그 회차가 "node \"node1\" is offline"으로 남는다(대상을 바꿔도
  된다). 복제본이 없는 로컬 디스크 HA VM은 지금처럼 복구되지 않고 그 까닭이 남는다("HA cannot find a node for this VM: disk
  \"root\" is on node-local pool \"local\" and has no ready replica on a live node"). HA의 옮기기(재시작 횟수를 넘김)와 HA 검사의
  `restart_elsewhere`는 원본이 살아 있는 차가운 재시작이라 복제본을 쓰지 않는다(최근 쓰기를 잃으므로) — 그런 VM은 지금처럼
  옮길 노드가 없다. HA 검사의 `migrate`(라이브)는 복제본 노드로 간다.
- **마이그레이션.** 대상 노드가 VM의 복제 대상이면 전체를 복사하지 않고 **마지막 복제 뒤 바뀐 블록만** 복제본 위에 보낸다:
  남은 변경이 64 MiB 이하가 될 때까지(최대 5번) 되풀이하고 전환할 때(게스트가 멈춘 동안) 마지막 변경을 보낸다(`vm migrate`가
  `vm web: copying the changes since the replica`·`vm web: sent 3.0 MiB of changes since the replica`를 알린다, 웹 대화상자는
  "이 노드에는 복제본이 있어…" 안내). 그 노드의 복제본 풀과 다른 `--pool`은 **409** `migration_unsupported`("node \"node2\"
  holds the replica of VM \"web\" in pool \"b\"; migrate without --pool or remove the node from the replication targets").
  옮긴 뒤 복제 방향이 뒤집혀 옛 원본의 디스크가 곧바로 `ready` 복제본이 된다(옛 원본에 디스크가 남는다). 이 따라잡기는 QEMU의
  비트맵 옮기기를 끄므로 **다른 복제 대상과 증분 백업은 다음 한 번이 전체**다. 마이그레이션이 되돌려지면 그 대상의 복제본은
  `syncing`으로 남아 다음 회차(전체)가 끝날 때까지 HA에 쓰지 않는다. 복제 대상이 아닌 노드로는 지금처럼 전체를 복사한다(복제
  비트맵은 따라간다). 복제 Task와 마이그레이션은 VM 줄에서 차례로 돈다. 따라잡기도 복제 회차처럼 플리싱으로 보내 대상이 끊겨도
  게스트 쓰기는 실패하지 않는다(그 패스와 마이그레이션만 되돌려진다). 따라잡기는 `[migration] disk_timeout`(기본 12h) 안에
  끝나야 하고, 넘기면 작업을 취소하고 플리싱을 뗀 뒤 되돌린다(되돌리기도 남은 플리싱을 치운다). 옛 원본은 확정하기 전에 남길 디스크를 자기 기록에 적어 두므로
  확정 뒤 orbivisord가 죽어도 그 디스크는 옛 복제본으로 알아보고 다음 역방향 회차가 그 위에 보낸다.
- **배치.** 균형 조정기는 복제가 있는 로컬 디스크 VM을 옮길 수 있고 그 대상은 복제본이 준비된 노드뿐이다. 비우기는 로컬 디스크
  VM을 점수가 같으면 복제본 노드로 먼저 옮기고(그러면 바뀐 것만 보낸다), HA 되돌아가기(failback)도 복제본이 준비된 노드로만
  한다.
- **공간.** 새 복제본(전체)은 원본 디스크의 할당 크기 합이 대상 풀의 남은 공간(노드가 알린 `available_bytes`) 이하여야 하고,
  증분은 대상 `{data_dir}` 파일 시스템에 바뀐 양 + 1 GiB가, 대상 풀에 합칠 바뀐 양이 남아야 한다(아니면 회차 실패 "not enough
  space in storage pool
  \"local\" on node \"node2\": need …, available …"). 복제본은 대상 풀의 공간을 디스크만큼 쓴다. 노드는 자기 복제본을
  `{data_dir}/replicas/{vm}.json` 기록으로만 알고 지운다(남의 파일은 지우지 않는다 — 같은 id 볼륨이 기록 없이 있으면 그 디스크를
  거절한다): VM을 지우거나 대상에서 빼거나 디스크를 떼면 대상 노드의 청소(시작할 때와 10분마다)가 복제본을 지운다. 돌아온 옛
  원본의 기록에 아직 그 노드의 볼륨이거나 다른 VM이 쓰는 디스크가 있으면(원본이 떼어 다른 VM에 붙였다) 그 디스크는 지우지 않고
  기록에서 뺀다. 원본은 회차마다 VM 상태 디렉터리에 플리싱 이미지를 둔다(위).
- **상태 보기.** `vm replication`(위), `vm get`의 복제 줄, VM `status.replication`(대상마다 `state` pending·ready·syncing·
  stale, `synced_at` 시점, `volumes`(볼륨 id와 `size_bytes`), 마지막 회차의 `last_bytes`·`last_secs`·`last_full`, `last_try`(마지막
  복제 Task를 만든 때), `error`)와 `status.last_recovery`(위). 준비된 복제본이라도 그 뒤 VM의 로컬 디스크를 더했거나 크기를 바꿨으면
  다음 회차가 끝날 때까지 HA·마이그레이션이 쓰지 않으므로 CLI는 `node2 ready 4m ago (HA cannot use it: disk "data" changed)`(표는
  ERROR 칸), 웹 카드는 같은 안내를 붙인다. 웹은 VM 만들기·
  편집의 "복제" 절(대상 노드 — VM 노드·위트니스 빼고, 주기(분), 대상 풀, 3개까지)과 일반 절의 HA 안내("이 VM은 다른 노드에서
  다시 켤 수 없습니다 — 복제를 더하세요"), VM 요약 탭의 "복제" 카드(대상마다 노드·상태·마지막 시점과 나이·보낸 양과 증분/
  전체·다음 회차·오류, "지금 복제" 단추), 스냅샷 탭의 막힌 만들기 단추와 까닭, 노드 표의 "복제본" 칸(그 노드가 대상인 VM 수,
  누르면 VM과 상태)을 보인다. API는 `POST /api/v1/vms/{key}/replicate`(본문 `{"node": "<이름이나 id>"}` — 대상이 하나면
  생략, **202** Task, 운영자 권한, **409** `vm_busy` "VM \"web\" is already being replicated to node \"node2\" (task …)",
  **400** "node \"node3\" is not a replication target of VM \"web\""·"VM \"web\" has several replication targets; choose one
  with node")다. 실패한 회차의 까닭은 그 대상의 `error`에 남고 다음 성공 때 지워진다.
- **설정.** `orbivisord.toml`의 `[replication]`: `tick_secs`(60), `max_concurrent`(2), `bandwidth_mib`(0). 리더가 바뀌어도
  `last_try`·`synced_at`이 클러스터 기록에 있어 일정이 이어진다.
- **업그레이드.** 모든 노드가 `node-replication`을 알아야 복제를 켤 수 있고(아니면 **409** `cluster_upgrading`) 복제 Task·
  복제본 승격·따라잡기 마이그레이션을 한다. 받은 백업·복제(clone)·복원·가져오기로 만든 VM은 `replication`을 비운다.
- **한계.** 꺼진 VM 복제, 스냅샷까지 복제, HA가 아닌 VM을 손으로 복제본에서
  켜기, 갈라진 옛 원본을 증분으로 되돌리기(늘 전체), raw 디스크 비트맵을 재시작 뒤에도 지키기, 시계에 맞춘 일정은 아직 없다.
