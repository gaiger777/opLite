# DR 오케스트레이션 — 복구 계획·부팅 그룹·네트워크/IP 바꾸기·격리 시험·RPO/RTO (설계)

상위 설계: 원격 클러스터·원격 복제(`docs/guide/backup.md` "원격 클러스터"·"원격 복제(DR)"·"S3로 DR"), 계획 65~67 SDN, 계획 71 VMware 이주 계획(리더 스케줄러 본보기).

로드맵(2026-11-25 차별화 분석): "DR orchestration (SRM-like recovery plans, ordered boot groups, IP remap, isolated test failover in an SDN bubble,
RPO/RTO report)". VMware는 SRM이 따로 팔리고, Proxmox VE는 복구 계획이 없다(복제·백업만). Orbivisor는 원격 복제까지 있지만 장애 때는
사람이 백업을 하나씩 골라 복원·시작한다(`backup.md` 한계: "자동 장애 조치·복구 계획 실행·연속 복제 없음").

목표: DR 클러스터에서 **복구 계획** 하나로 (1) 지켜야 할 VM과 그 복구 지점이 지금 얼마나 오래됐는지(RPO)를 늘 보고, (2) 운영을 건드리지 않고 격리망에서
**시험 장애 조치**를 단추 하나로 돌려 실제 복구 시간(RTO)을 재고 뒷정리하며, (3) 실제 장애 때 같은 계획을 **장애 조치**로 돌려 그룹 순서대로 복원·시작하고
네트워크·IP를 DR 쪽에 맞추며, (4) 운영 쪽이 살아 있으면 마지막 백업을 떠서 보낸 뒤 넘기는 **계획된 이전**을 한다.

## 1. 지금 있는 것(조사 2026-11-27)

- 원격 클러스터 `Remote{url, ca_fingerprint, token}`(`orbivisor-types/src/remote.rs`), 리더가 30초마다 상태를 모음(`RemotePoller`), 프록시
  `/remotes/{key}/api/{*path}`(`remote_proxy.rs`, `remote_client.rs` `get_json`·`request`).
- 원격 복제: `backup-replicate` Task, 원본 `status.replicas[{remote, pool, backup, state, at}]`, 사본 `spec.origin{cluster, ca_fingerprint, backup, job}`,
  작업 복제(`replicate: {remote, pool}`), 증분 체인. S3 읽기 전용 붙이기 + `backup-store scan`도 사본(origin 있음)을 만든다.
- 복원 `POST /backups/{name}/restore {name, pool, node?, folder?, live}`(`backups.rs`) — **네트워크 바꾸기 없음**(백업의 네트워크 이름이 DR에 있어야 함),
  MAC 새로, `ha` 뺌, SDN 주소는 비어 있으면 그대로.
- 게스트 IP: cloud-init `CloudInit.networks[]`(NIC별 고정·DHCP, `instance_id`가 내용 해시라 바꾸면 다시 적용). 게스트 에이전트에 IP 바꾸기 없음.
- SDN 네트워크(OVN 논리 스위치)는 라우터가 없으면 L3 격리이고 CIDR이 겹쳐도 된다 — 격리 "거품"으로 쓸 수 있다.
- VM 시작 순서는 HA `priority`뿐. 부팅 그룹·기다림·지연 없음. 백업을 띄워 보는 시험 없음.
- 오래 걸리는 여러 단계 작업의 본보기: VMware 이주 계획 스케줄러(`vmware_scheduler.rs` — 리더가 15초마다, 진행을 Raft에, 새 리더가 이어 감, 취소·뒷정리).

## 2. 고른 것과 견준 길

- **계획은 DR 쪽 리소스(채택) vs 운영 쪽 vs 둘 다.** 진짜 장애 때 운영 클러스터는 없다. 그래서 계획과 실행은 **복구하는 클러스터**에 있고, 운영 쪽은
  원격(`source`)으로만 가리킨다. 운영 쪽이 살아 있으면 프록시로 상태·마지막 백업·정지를 부른다(계획된 이전). 양방향은 각 쪽에 계획을 하나씩 둔다(되돌리기 §3.6).
- **복구 지점 = DR에 있는 사본 백업(채택) vs 연속 복제(블록 단위).** 연속 복제는 새 데이터 경로(저널·쓰기 순서)라 큰 계획 하나다 — 범위 밖. 지금 백업 복제의
  RPO(백업 간격, 증분이면 짧게 가능)를 정직하게 보이고, 계획마다 RPO 목표를 넘으면 경보한다.
- **복원 때 네트워크 바꾸기: 복원 API에 `networks` 맵을 더함(채택) vs 복원 뒤 VM 고치기.** 복원 Task가 NIC를 만들기 전에 바꿔야 "네트워크가 없음" 409를
  피하고, 다른 복원(사람이 하는 것)에도 쓸모 있다(VMware 가져오기 `overrides.networks`와 같은 모양). 보안 그룹도 같은 맵(`security_groups`).
- **IP 바꾸기: cloud-init 덮어쓰기(채택) vs 게스트 에이전트 exec vs 안 함.** 게스트 안을 바꾸는 표준 길은 cloud-init이고 Orbivisor가 이미 seed를 만든다.
  계획의 VM별 `ip` 규칙(`{nic, address/prefix, gateway, dns}` 또는 "DHCP")을 복원된 VM의 `cloud_init.networks`로 넣는다 — 게스트에 cloud-init이 있어야 하고
  (없으면 경고 "IP not changed — no cloud-init"), 원본 VM에 cloud-init이 없었으면 seed를 새로 붙인다. SDN 네트워크면 IPAM 주소도 그 주소로 잡는다.
  게스트 에이전트 exec은 OS마다 명령이 달라 고르지 않는다.
- **격리 시험: 계획이 SDN 거품 네트워크를 만들고 지움(채택) vs 운영자가 시험 네트워크를 미리 만듦.** 시험 때 계획의 `test_networks` 맵이 없는 원본 네트워크마다
  `drt-<plan>-<n>` SDN 네트워크(라우터·업링크 없음, 원본의 CIDR 그대로 — 같은 IP로 서로 닿게)를 만들고 뒷정리에서 지운다. SDN이 없는 클러스터는
  `test_networks`를 모두 정해야 한다(아니면 시험 거부 400 `dr_test_network_required`). 시험 VM은 `<vm>-drt` 이름, 폴더 `dr-test/<plan>`, 태그로 표시.
- **실행 엔진: 리더 스케줄러 + 실행 기록을 Raft에(채택) vs Task 하나가 끝까지.** 그룹 기다림이 수십 분일 수 있고 리더가 바뀌어도 이어 가야 한다 — VMware 이주
  계획과 같은 방식. 각 VM의 복원·시작은 지금 Task(`backup-restore`, `vm-start`)를 그대로 쓴다.
- **기다림: 그룹마다 `wait`(채택).** `running`(QEMU 시작), `guest`(게스트 에이전트가 답함), `ip`(게스트 에이전트가 기대 IP를 보고함 — 없으면 아무 전역 주소),
  그리고 `delay_secs`(그다음 그룹 앞 쉼)와 `timeout_secs`(넘으면 그 VM `timed_out` — 계획의 `on_timeout: continue|stop`).

## 3. 서버

### 3.1 리소스 `RecoveryPlan` (`/dr-plans`, 권한 읽기 `dr.read`=viewer, 쓰기·실행 admin, 기능 `dr-plans`)

```
spec:
  source: <원격 이름>             # 운영 클러스터(원격). 사본의 origin.cluster로 VM을 맞춘다.
  rpo_target_secs: 86400          # 넘으면 경보 dr_rpo_exceeded (0 = 끔)
  pool: <DR 풀>                    # 기본 복원 풀(VM별로 덮어씀)
  folder?: <폴더>                  # 장애 조치로 만든 VM의 폴더
  networks: {<원본 네트워크>: <DR 네트워크>}       # 없으면 같은 이름
  security_groups: {<원본>: <DR>}
  test_networks: {<원본 네트워크>: <DR 네트워크>}  # 없으면 시험 때 거품 SDN 네트워크
  on_timeout: continue | stop     # 기본 continue
  groups:                          # 차례대로
    - name: db
      wait: running | guest | ip   # 기본 running
      timeout_secs: 600
      delay_secs: 0                # 이 그룹이 끝난 뒤 다음 그룹 앞 쉼
      vms:
        - vm: <원본 VM 이름>
          pool?, node?, live?: false
          ip?: [{nic: 0, address: "10.20.0.5/24", gateway?, dns?: []} | {nic: 0, dhcp: true}]
status:
  protection: [{vm, backup?(DR 사본 이름), point_at?(백업 시각), age_secs?, ok}]   # 리더가 1분마다 다시 셈
  last_test?: {run, at, rto_secs, result}
  run?: <지금 실행 id>
```

- 검사(400): 그룹 이름·VM 겹침, VM 1~500, 그룹 1~32, `ip`의 nic 범위는 복원 때 확인(경고), 원격·풀·네트워크 이름 모양. 원격이 없으면 409 `dr_source_missing`.
- **보호 상태**(`status.protection`): DR의 백업 중 `origin.cluster`가 원격의 클러스터(원격 상태의 `cluster_name`·CA 지문)이고 VM 이름이 같은 `ready` 백업의
  가장 새 것. 없으면 `ok: false`(`no recovery point`). `age_secs > rpo_target_secs`면 `ok: false`. 원격이 닿지 않아도 셈(DR 쪽 자료만 씀).

### 3.2 실행 `DrRun` (`POST /dr-plans/{key}/actions/{test|failover|cleanup|cancel}`)

- `test`: 시험 장애 조치. 운영 쪽을 건드리지 않는다. 계획당 시험 실행 하나(이미 있으면 409 `dr_run_active`), 이전 시험의 뒷정리가 안 됐으면 409
  `dr_cleanup_required`.
- `failover {mode: planned|disaster, confirm: <계획 이름>}`: 실제. `planned`는 원격이 닿아야 하고 먼저 운영 쪽 VM을 그룹 역순으로 정지(프록시
  `POST /vms/{n}/actions/shutdown`, 5분 뒤 `stop`), 그 VM들의 새 백업을 떠서 이 클러스터로 복제한 뒤(운영 쪽 백업 작업이 아니라 `POST /vms/{n}/backup` +
  `POST /backups/{b}/actions/replicate`를 프록시로 — 끝나기를 기다림), 그 사본으로 복원. `disaster`는 지금 있는 가장 새 사본으로 곧장.
- `cleanup`: 마지막 시험이 만든 VM(정지·지움)과 거품 네트워크를 지움. 실패한 장애 조치의 반쯤 만든 VM은 지우지 않는다(사람이 고름 — 문서).
- `cancel`: 지금 단계 뒤 멈춤(진행 중 Task는 끝까지). 시험이면 그 뒤 `cleanup`.
- 실행 기록 `dr_runs`(Raft, 계획마다 최근 20개): `{id, plan, kind(test|failover-planned|failover-disaster|cleanup), state(running|succeeded|partial|failed|cancelled),
  started_at, finished_at, rto_secs, steps: [{group, vm, backup, phase(pending|preparing|restoring|starting|waiting|ready|failed|timed_out|skipped), new_vm, task, error, at}]}`.
  `GET /dr-plans/{key}/runs`, `GET /dr-runs/{id}`.
- **엔진**(`dr_scheduler.rs`, 리더 5초마다): 그룹 차례. 그룹 안은 VM을 동시에(`max_parallel` 설정 기본 4, 노드당 2) 복원 Task → 끝나면 IP 덮어쓰기(cloud-init) →
  `vm-start` Task → 기다림. 그룹의 모든 VM이 `ready`·`failed`·`timed_out`이면 `delay_secs` 뒤 다음 그룹. `on_timeout: stop`이면 실패·시간 초과가 하나라도 있으면
  남은 VM은 `skipped`, 실행 `partial`. 새 리더는 기록에서 이어 감(Task id로 결과 확인).
- **RTO**: 실행 시작부터 마지막 그룹의 마지막 `ready`까지(planned면 운영 쪽 정지·마지막 복제 시간도 따로 `prepare_secs`로). 계획 `status.last_test`에 둠.
- 장애 조치로 만든 VM: 원본 이름(이미 있으면 409 그 VM `dr_name_taken` → 그 VM만 `failed`), `ha`는 원본 사본의 설정 그대로 켜지 않음(문서 — 장애 조치 뒤 켬),
  태그 `dr:<plan>`. 시험 VM: `<vm>-drt`, 폴더 `dr-test/<plan>`, NIC는 거품 네트워크, 태그 `dr-test:<plan>`.

### 3.3 복원 API 넓히기 (`RestoreRequest`, 기능 `restore-remap`)

- `networks?: {<백업의 네트워크>: <이 클러스터 네트워크>}`, `security_groups?: {..}`, `cloud_init_networks?: [IpConfig…]`(NIC별 — 주면 VM의 cloud-init seed를 이것으로,
  없던 seed는 새로 만듦). 맵에 없는 이름은 지금처럼 같은 이름이어야 한다. CLI `backup restore --network old=new --security-group old=new --ip 0=10.0.0.5/24,gw=10.0.0.1`.
- SDN 네트워크로 바뀐 NIC의 주소: `cloud_init_networks`가 그 NIC에 고정 주소를 주면 IPAM에 그 주소로 잡고(겹치면 실패), 아니면 새로 받음.

### 3.4 경보

- `dr_rpo_exceeded {plans?}`(리더 경보, 계획마다 하나): `status.protection`에 `ok: false`가 있으면. 문구 `plan web-tier: 2 of 12 VMs over RPO 24h (web3 34h, db1 no recovery point)`.
- `dr_run_failed`: 실행이 `failed`·`partial`로 끝나면 한 번(info가 아니라 warning). 알림 본문에 실행 링크.
- `POST /alert-rules/dr-defaults {targets}`(링크 경보 묶음과 같은 모양).

### 3.5 보고

- `GET /dr-plans/{key}/report`: 계획, VM별 보호 상태(복구 지점·나이·RPO 목표), 최근 시험 셋의 RTO·결과·실패 VM, 지난 장애 조치. 웹이 보이고 CLI
  `dr report <plan> [--json]`이 표로. (PDF 없음.)

### 3.6 되돌리기(failback)

- 자동 되돌리기는 하지 않는다. 장애 조치 뒤: DR에서 복구된 VM에 백업 작업 + `replicate`를 운영 쪽(원격으로 등록)으로 걸고, 운영 쪽에 거꾸로 된 계획
  (`source` = DR)을 만들어 `failover planned`로 돌아온다. `POST /dr-plans/{key}/actions/reprotect {remote, pool, schedule}`가 그 백업 작업을 만들어 준다
  (복구된 VM들, 작업 이름 `dr-reprotect-<plan>`). 문서에 순서.

## 4. CLI·웹

- CLI `orbivisorctl dr plan create|get|list|set|delete`(YAML 파일 `-f`도), `dr test <plan> [--wait]`, `dr failover <plan> --mode planned|disaster --confirm <plan>`,
  `dr cleanup <plan>`, `dr cancel <plan>`, `dr runs <plan>`, `dr run <id>`, `dr report <plan>`, `dr reprotect …`, `backup restore --network/--security-group/--ip`,
  `alert rule add-dr-defaults`.
- 웹: 데이터 보호 › **DR**(새 화면 — 백업 메뉴 아래 탭, 사이드바 새 항목 없음). 계획 목록(보호 VM 수·RPO 지킴 막대·마지막 시험 RTO·상태), 계획 화면
  (그룹 순서 편집 — 끌어 놓기 없이 위·아래 단추, VM 고르기는 원격 VM 목록 + DR 사본에서, 네트워크 맵 표, IP 규칙), 실행 화면(그룹 띠·VM마다 단계 점·Task 링크·
  RTO 시계), "시험" 단추, "장애 조치"(계획 이름을 쳐야 하는 확인), "뒷정리". 홈 경보에 RPO 경보가 그대로 보임.

## 5. 섞인 판·호환

- 기능 `dr-plans`(계획·실행)와 `restore-remap`(복원 맵): 모든 온라인 노드가 알아야 쓰기 허용(409 `cluster_upgrading`). 운영 쪽(원격)은 옛 판이어도 된다 — 프록시로
  부르는 것은 지금 있는 API(`vms`, `backup`, `replicate`, `shutdown`)뿐. 복원 맵이 없는 옛 판으로는 계획된 이전의 "운영 쪽" 일만 한다.

## 6. 시험

- 타입: 계획 검사, 보호 상태 계산(origin 맞추기, 나이, 원격 없음), 실행 상태 기계(그룹 차례, 시간 초과, `on_timeout`, 취소, 새 리더 이어 감)를 순수 함수로.
- API(FakeStorage·가짜 원격): 계획 CRUD·권한, 시험 실행 끝까지(복원·시작 Task 흉내, 거품 네트워크 만들고 지움), 장애 조치 disaster·planned(가짜 원격이
  정지·백업·복제를 받음), 이름 겹침, 409들, 복원 맵(네트워크·보안 그룹·cloud-init).
- 웹: 목록·계획 편집·실행 화면·확인 대화상자.
- 실험실: svc01 → svc02를 원격으로(이미 있으면 그대로), svc01의 작은 VM 백업을 svc02로 복제, svc02에 계획(그룹 둘), 시험 → 거품 네트워크에서 시작·RTO →
  뒷정리, disaster 장애 조치(운영 VM은 그대로 두고 이름 충돌을 피하려 다른 폴더) — 실험실은 TCG라 느림.

## 7. 범위 밖

- 연속(블록) 복제·저널, 동기 복제(RPO 0), 스토리지 배열 복제 연동.
- 자동 장애 조치(사이트 감시로 스스로 넘김 — 분할 위험; 사람이 누름).
- 게스트 안 스크립트(복구 뒤 명령), Windows IP 바꾸기(cloud-init이 없는 게스트는 IP 그대로).
- DNS 바꾸기·외부 로드밸런서 연동.
- 보고서 PDF.
