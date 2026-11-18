# 지표 기록·대시보드·알림·Prometheus 설계 (계획 60)

작성일: 2026-11-18
상위 문서: `2026-11-11-monitoring-ui-design.md`(계획 44 지표 수집, 그 문서 §7 대시보드 스케치), `2026-11-11-availability-scale-design.md`(§5 계획 47 리더 짐 덜기·모으기 상한, §8.3 계획 53 부하 요약), `2026-11-06-enterprise-design.md`(감사 로그 `ReadAudit`), `2026-11-18-vtpm-secure-boot-guest-agent-design.md`(계획 59 — 상태 디렉터리 정리와 `read_index`)
기준 트리: 계획 59 끝(`5ebfa20`). 이 문서는 구현 전 설계다. 스테이지 보고는 구현하면서 덧붙인다.

## 1. 목적

계획 44 지표는 노드 메모리에 1시간(`MAX_SAMPLES = 720`, 5초)만 있다(`opld/src/metrics/store.rs`). opld를 다시 시작하면 비고, 하루·주·년 추세가 없다. 웹에는 차트가 하나도 없다. 모니터링 설계 §7의 계획 45~47(uPlot 차트·3D·끌어 놓기)은 번호가 가용성·규모 계획으로 넘어가 만들지 않았다(`web/package.json`에 차트 라이브러리 없음, `nodes/:name` 경로 없음). 대시보드(`DashboardPage.tsx`)는 노드 수·VM 상태·스펙 vCPU/메모리 합 표다. 경보·알림·Prometheus 내보내기도 없다(`smtp`·`webhook` grep 0건, 텔레메트리 보고만 있음). 이 계획은 넷을 채운다: 디스크에 남는 지표 기록, 대시보드와 차트, 규칙 경보와 알림, Prometheus 끝점.

| | Proxmox VE 8 | vSphere 8 | Orbivisor 계획 60 |
|---|---|---|---|
| 기록 | rrdcached RRD, 시간·일·주·월·년, AVERAGE·MAX | vCenter DB: 실시간 20초·일 5분·주 30분·월 2시간·년 1일 | 노드마다 Rust RRD 파일, 5초·1분·30분·6시간, AVG·MAX·MIN(§5) |
| 백분위 | 없음 | 없음 | 지난 1시간 정확한 p50·p95, 그 밖은 근사(§7) |
| 알림 | 대상(sendmail·SMTP·Gotify·webhook) + 매처, 이벤트 위주 | 알람(임계값·지속 시간) + 동작(메일·SNMP·스크립트) | 규칙(지표 임계값·지속 시간, 이벤트) + 대상(SMTP·webhook·Slack·Teams·ntfy·Gotify)(§8·§9) |
| 외부 지표 | 지표 서버로 밀기(Graphite·InfluxDB) | 없음(외부 수집기) | Prometheus 끌어가기 `/api/v1/metrics`(§10) |

## 2. 범위

| 넣는 것 | 넣지 않는 것(§15) |
|---|---|
| 노드·VM·풀의 RRD 파일(노드 로컬, 고정 크기, 분마다 제자리 쓰기) | 외부 DB(InfluxDB·rrdtool), Raft에 지표 쓰기 |
| 범위 조회(시간~년, `from`·`until`), 점 수 줄이기, 평균·최대·최소, p50·p95 | 디스크·NIC마다 나눈 기록, 게스트 안 지표 |
| VM 기록이 VM을 따라가기(§6 밀어 넘기기) | 지표 밀어 보내기(Graphite·InfluxDB) |
| 클러스터 대시보드, 노드 상세 화면, VM "지표" 탭(uPlot) | 3D 보기, 끌어 놓는 VM 만들기(모니터링 설계 §8·§9) |
| 경보 규칙(지표 임계값 + 지속 시간, 이벤트), 반복·해제 알림, 보낸 기록 | 경보 묶기·억제 규칙·근무 일정, 개별 경보 확인(ack) |
| 대상: SMTP, webhook(일반 JSON·Slack·Teams·ntfy·Gotify), 시험 보내기 | SNMP 트랩, SMS, 스크립트 실행 |
| Prometheus 텍스트 형식 `/api/v1/metrics`(노드마다), Grafana·스크레이프 예 | OpenMetrics·원격 쓰기, 내장 Grafana |

**기능 둘**(`opl_types::node`, `FEATURES` 끝): **`metrics-history`**(`METRICS_HISTORY`)는 peer RPC `ReadMetricsHistory`·`PushMetricsHistory`, 기록 조회 API, 설정 `prometheus`를 가린다. **`notifications`**(`NOTIFICATIONS`)는 새 종류 `AlertRule`·`NotificationTarget`, peer RPC `ReadAlerts`를 가린다. 쓰기(규칙·대상 만들기, 설정 `prometheus`)는 모든 노드가 그 기능을 알릴 때만 받고 아니면 409 `cluster_upgrading`이다. `SettingsSpec`은 `deny_unknown_fields`라서 옛 노드는 새 칸이 든 설정을 풀지 못하기 때문이다. 읽기는 막지 않고 옛 노드를 계획 44처럼 `unsupported`로 보인다. 계획 58·59처럼 기능 검사는 검증보다 먼저 한다.

## 3. 지금 있는 것 (계획 59 끝)

- 측정기 `MetricsSampler`(`sampler.rs`, `INTERVAL` 5초)가 `NodeSample`(9칸)·`VmSample`(8칸, `opl-types/src/metrics.rs`)을 만들어 `MetricsStore`에 넣는다. 저장소는 `Mutex` 안의 `VecDeque` 720점이고, `prune`은 1시간 지난 VM 시리즈를 지운다. 점의 `t`는 노드 벽시계 ms이고, 비율은 단조 시계로 셈한다.
- `MetricsStore::summary`가 부하 요약(`LoadSummary`: `cpu1`·`cpu5` ‰, `mem`, VM 256개까지)을 만들고, 측정기가 `cluster.set_lease_load`로 리스 갱신에 실어 리더에 보낸다(계획 53).
- 읽기: `StoreSource`가 `opl_peer::MetricsSource`를 구현하고, peer RPC `ReadMetrics`가 다른 노드에서 읽는다. API `opl-api/src/metrics.rs`의 `GET /metrics/nodes`, `/nodes/{key}/metrics`, `/vms/{key}/metrics`는 `since`를 받는다. 모으기는 `fan_out::gather`(`FAN_OUT = 32`, 피어 제한 3초)를 쓴다. `/vms/{key}/metrics`는 위트니스를 뺀 **모든 노드**에 묻는다. 지표용 WebSocket은 없다. `/watch`는 리소스 변경만 다루고, 웹·CLI는 `since` 폴링을 쓴다.
- 풀 용량: `PoolMounter`가 `NodeStatus.pools`(`PoolState { total_bytes, available_bytes }`)에 쓴다. 1 GiB 넘게 바뀔 때만 쓴다(`keep_capacity`).
- 이벤트 원천: Task(`TaskSpec.node`, `TaskStatus { state, error }`, HA 복구는 `TaskOp::VmHaRecover`), VM `phase: failed`, `NodeStatus.online`(리더가 리스로 정함), 백업 작업 `BackupJobStatus.last_run: JobRunResult { failed, .. }`(리더가 돌림, `backup_jobs.rs`), 감사 로그(노드 파일, `ReadAudit`). 변경 흐름은 `Cluster::watch(since)`로 받는다.
- 비밀 다루기의 본: 영역(`realms.rs`)은 admin만 읽는다(`auth.rs` `check`의 `account` 목록). 비밀은 응답에서 빼고 `has_bind_password`만 보이며, 바꾸기에서 빼면 저장된 값을 지킨다. `/watch` 목록에도 넣지 않는다. 원격 토큰은 `Debug`에서 `***`로 가린다.

## 4. 타입 (opl-types)

- `metrics.rs`에 더한다(모두 `cfg_attr(feature = "openapi", ToSchema)`).
  - `MetricField`: 노드 `cpu, mem_used, mem_total, disk_read, disk_write, net_rx, net_tx, fs_used, fs_total`, VM `cpu, mem_rss, disk_read, disk_write, disk_read_iops, disk_write_iops, net_rx, net_tx`, 풀 `pool_used, pool_total`. snake_case 이름이 API 열 이름이다. MiB 칸은 기록에서도 MiB다.
  - `HistoryQuery { scope: HistoryScope, from: i64, until: i64, points: u16 }`, `HistoryScope = Node | Vm { id } | Pool { id } | Digest { top: u8 }`(`kind` 태그).
  - `HistoryReply { tier: Tier, step_ms, start, columns: BTreeMap<String, Cf { avg, max, min: Vec<Option<f32>> }>, stats: BTreeMap<String, Stat { avg, max, min }>, pct: BTreeMap<String, Pct { p50, p95, approx }>, covered_from: Option<i64> }`. 열 모양은 웹 uPlot이 그대로 받는 배열이다. NaN은 `null`로 보낸다.
  - `Tier = Raw5s | Min1 | Min30 | Hour6`.
- 순수 함수(`metrics_math.rs`): `choose_tier(from, until, now)`, `downsample(&Cf, group)`(평균은 아는 점 수로 가중, 최대의 최대, 최소의 최소), `stats`, `percentile_nearest_rank`, `cluster_sum`(노드 열을 시각 칸끼리 더함, CPU는 `NodeStatus.cpus`로 가중). 시험이 쉽게 하려고 IO가 없다.
- `alert.rs`(새 파일):
  - `AlertRuleSpec { enabled = true, condition: AlertCondition, for_secs: u32 (0~86400), severity: Info|Warning|Critical, targets: Vec<String>, repeat_secs: u32 = 14400 (0이면 안 함), send_resolved: bool = true, nodes: Vec<String>, vms: Vec<String> }`. `nodes`·`vms`가 비면 모두이고, `deny_unknown_fields`다.
  - `AlertCondition`(`kind` 태그): 지표 `NodeMetric { metric, op: Gt|Lt, value }`·`VmMetric { .. }`(`metric`은 `cpu`, `mem_percent`, `fs_percent`, `disk_read`, `disk_write`, `net_rx`, `net_tx`, VM은 `mem_rss`·IOPS), 상태 `PoolUsage { percent }`·`NodeOffline`·`VmFailed`, 이벤트 `TaskFailed { ops: Vec<String> }`(비면 모두)·`HaRecovery`(`vm-ha-recover` Task가 끝남)·`BackupJobFailed`. 검증: 대상 1~16개, 값은 유한, 이벤트 조건은 `for_secs = 0`이고 해제 알림이 없다.
  - `AlertRuleStatus { active: Vec<ActiveAlert> }`: **리더가 평가하는 규칙의 활성 경보만** 둔다(§8). `ActiveAlert { key, subject, since, value, last_sent }`.
  - `NotificationTargetSpec { kind: Smtp(SmtpTarget) | Webhook(WebhookTarget), enabled }`.
    - `SmtpTarget { host, port, tls: StartTls|Tls|None, user?, password?, from, to: Vec<String> (1~20) }`.
    - `WebhookTarget { url, format: Json|Slack|Teams|Ntfy|Gotify, headers: BTreeMap<String,String>, token?, ca_pem? }`.
    - 비밀(`password`, `url`의 경로·질의, `headers` 값, `token`)은 `Debug`에서 가린다(`RemoteSpec`과 같음).
  - `NotificationRecord { at, rule, subject, state: Firing|Resolved|Event|Test, target, ok, error? }`. `error`는 보낸 쪽이 비밀을 지운 문구다.
- `Kind::AlertRule`("alert-rule")와 `Kind::NotificationTarget`("notification-target")를 더한다. `NotificationTarget`은 `auth.rs`의 `account` 목록에 넣어 admin만 읽는다. `AlertRule`은 모두 읽고 admin만 쓴다. Operator 쓰기 목록에는 넣지 않으므로 지금 규칙 그대로 된다.
- `SettingsSpec.prometheus: Option<PrometheusSettings { enabled: bool }>`. 없으면 꺼짐이고 쓰지 않는다(`skip_serializing_if`).

## 5. 기록 파일 (RRD, opld `metrics/rrd.rs`)

- **자리**: `{data_dir}/metrics/node.rrd`, `metrics/vm/{id}.rrd`, `metrics/pool/{pool_id}.rrd`(풀 id라서 이름을 바꿔도 이어진다). 디렉터리는 0700, 파일은 0600이다. 바이너리이고 리틀 엔디언이다.
- **층**:

| 층 | 간격 × 칸 | 기간 | 칸 하나(VM 8칸) |
|---|---|---|---|
| `Raw5s` | 5초 × 720 | 1시간 | 값 하나 f32 × 8 = 32 B |
| `Min1` | 1분 × 1440 | 1일 | AVG·MAX·MIN f32 × 8 = 96 B |
| `Min30` | 30분 × 336 | 1주 | 96 B |
| `Hour6` | 6시간 × 1460 | 1년 | 96 B |

- **30분 층은 넣는다.** 32 KiB(10%)로 주 차트가 28점(6시간)에서 336점이 된다. 월은 6시간 층의 120점이다.
- **크기**: 층마다 4 KiB에 맞춘다. 머리 4 KiB + 24 + 136 + 32 + 140 KiB = **VM 336 KiB**, 노드 약 380 KiB, 풀 약 90 KiB. 노드에 VM이 100개면 약 34 MB다. 만들 때 `fallocate`로 크기를 잡고 NaN으로 채운다.
- **칸 주소**: rrdtool처럼 시각으로 정한다. 칸 `b = floor(t / step)`(유닉스 기준이라 모든 노드의 칸 경계가 같다)는 자리 `b mod rows`에 들어간다. 머리는 층마다 `last: i64`(마지막으로 쓴 칸)를 둔다. 칸 `b`는 `last − rows < b ≤ last`일 때만 읽는다. 칸 안 배치는 `[AVG f0..fn][MAX f0..fn][MIN f0..fn]`이라서, 칸 하나를 쓸 때 한 자리만 건드린다.
- **합치기**: `Min1`은 그 분의 5초 점 12개로 만든다. 아는 값이 절반 미만이면 NaN이다(rrdtool xff 0.5). `Min30`·`Hour6`은 칸이 닫힐 때 `Min1` 층에서 그 구간(30·360칸, `Min1`이 1440칸이라 늘 남아 있음)을 다시 읽어 만든다. 따로 누산기를 두지 않으므로 재시작 중간에 잃는 것이 없다. 다시 시작하면 머리의 `last`보다 뒤에 닫힌 칸을 `Min1`에서 메운다.
- **쓰기(분마다, 제자리 `pwrite`)**: (1) 모든 시리즈 파일에 새 칸(`Raw5s` 12칸, 닫힌 `Min1` 칸, 닫혔으면 `Min30`·`Hour6`, 건너뛴 칸은 NaN)을 `FileExt::write_at`으로 쓰고 (2) 모두 `fdatasync`(`rustix::fs::fdatasync`)한 뒤 (3) 머리를 쓰고 다시 `fdatasync`한다. 머리는 512 B 사본 둘(A/B)을 번갈아 쓰며 `seq: u64`와 SHA-256 앞 8바이트를 단다. 열 때는 검사합이 맞고 `seq`가 큰 사본을 쓴다.
  - 머리는 데이터가 디스크에 닿은 뒤에만 그 칸을 가리키므로 전원이 나가도 잃는 것은 마지막 1분이고, 가리키지 않은 칸의 쓰레기는 다음 쓰기가 덮는다. 두 사본이 다 깨졌으면 `{name}.corrupt`로 옮기고 새로 만든다(경고).
  - 파일 IO는 `spawn_blocking`에서 하고 핸들은 연 채 둔다(4096개 넘으면 LRU로 닫음).
- **빈 칸**: 노드가 꺼졌거나 opld가 멈췄던 구간, 값을 읽지 못한 칸(`None`)은 NaN이다. 0과 다르게 "모름"으로 그린다. 공백이 층 기간보다 길면 그 층 전체를 NaN으로 지운다.
- **시계 뛰기**: 점의 `t`는 벽시계다(계획 44 §3.5).
  - 앞으로 뛰면 공백과 같다.
  - 1시간 미만 뒤로 가면, 칸이 `last` 이하인 점은 파일에 쓰지 않는다. 메모리 1시간 기록은 그대로 보인다. 시계가 따라잡으면 이어 쓴다. 경고는 한 번만 남긴다.
  - 1시간 넘게 뒤로 가면, 앞선 시계(RTC가 틀렸던 부팅)로 쓴 칸이 미래 시각이다. 층마다 `(새 칸, last]`의 자리를 NaN으로 지우고 `last`를 되돌린다. 그 미래 칸은 버린다.
  - 시험은 `t`를 인자로 받는 순수 API(`Rrd::append(t, values)`)로 한다.
- **형식 버전**: 머리에 `magic = "OPLRRD\0\0"`, `version: u16 = 1`, 종류(node·vm·pool), 칸 id 목록, 층 표(간격·칸 수·오프셋)를 둔다.
  - 칸을 더하면(새 `VmSample` 필드) 버전을 올리고, 열 때 옮긴다. 옮기기는 `{name}.rrd.tmp`에 새 배치로 만들어 같은 칸 id를 복사하고 새 칸은 NaN으로 둔 뒤, fsync → rename → 디렉터리 fsync 순으로 한다. 칸 목록이 머리에 있으므로 버전 사이 옮기기는 한 함수다.
  - 더 새 버전(내려 깐 opld)이면 `{name}.rrd.v{n}`으로 옮기고 새로 시작한다. 내려 깔면 기록을 잃는다(README 한계).

## 6. 기록 루프와 노드 사이 RPC (opld)

- `MetricsStore`는 그대로 1시간 원본이자 API `since`·부하 요약의 원천이다.
- 새 `HistoryWriter::spawn(store, cluster, dir, pools)`(`metrics/history.rs`, `PoolMounter` 틀, `work` 실행기)가 분 경계마다 저장소의 지난 분 점을 파일로 옮긴다. 위트니스에는 없다.
- 풀: `PoolMounter`가 이미 재는 `PoolState`를 분마다 넘긴다(Raft 쓰기 기준인 1 GiB 거르기 전 값). 그 노드에서 `ready`인 풀만 기록한다. 공유 풀은 여러 노드가 같은 값을 따로 기록한다(파일 90 KiB라 받아들임).
- **다시 시작**: 열면서 `Raw5s` 층을 `MetricsStore`에 되돌려 넣는다. 그래서 1시간 차트와 정확한 p95가 재시작 뒤에도 남는다. 부하 요약은 `LOAD_VM_FRESH_MS`(15초) 거르기 때문에 옛 점을 쓰지 않는다. 비율 기준은 계획 44대로 다시 잡는다.
- **기록이 VM을 따라가기(밀어 넘기기)**:
  - VM 기록의 주인은 VM의 노드다(`status.node`, 없으면 `spec.node`).
  - 각 노드의 `HistoryWriter`는 10분마다와 이 노드에서 VM이 떠난 것을 본 때(watch) 자기 `vm/*.rrd`를 훑는다.
  - Raft에 VM이 있는데 주인이 다른 노드면, 새 peer RPC `PushMetricsHistory { vm, bytes }`(파일 하나, 1 MiB 미만)로 보낸다. 받은 쪽은 자기 파일과 **칸 단위로 합친다**: 자기 칸이 NaN이면 받은 값을 쓰고, 둘 다 있으면 자기 것을 둔다. `last`는 큰 쪽이다. 받은 쪽이 성공하면 보낸 쪽이 지운다.
  - 주인이 오프라인이거나 기능이 없으면 다음 차례에 다시 한다. 합치기는 같은 입력이면 결과가 같으므로 두 번 받아도 된다.
  - 그래서 라이브·꺼진 마이그레이션, 비우기, HA 복구 뒤 옛 노드가 돌아온 경우, 정해진 노드 바꾸기가 모두 같은 길이다.
- **지우기**: Raft에 VM이 없는 `vm/{id}.rrd`는 계획 59 상태 디렉터리 정리와 같은 규칙으로 지운다. 두 차례 연속 후보여야 하고, `Cluster::read_index`가 성공해야 하며, 그 VM을 대상으로 하는 끝나지 않은 Task가 없어야 한다. 풀 파일도 풀이 없어지면 같은 규칙이다.
- **peer RPC**(`opl-peer`, `build.rs`의 `json(..)`):
  - `ReadMetricsHistory(HistoryQuery) -> HistoryReply`: `MetricsSource`에 `history()` 메서드를 더하고, 기본 구현은 `Unimplemented`다.
  - `PushMetricsHistory`: 바이트 메시지다.
  - `ReadAlerts(AlertsQuery { history: Option<HistoryWindow> }) -> AlertsReply`(§8).
  - 서버는 지금처럼 구성원 인증서를 확인하고, 소스가 없으면 `unimplemented`다.
- 설정 `[metrics] history = true`(끄면 파일을 쓰지 않고 1시간 메모리만), `flush_secs = 60`(60~600)을 둔다. 시험 빌드에만 `[test] metrics_flush_secs`·`alert_tick_ms`가 있다.

## 7. 조회: 범위·통계·백분위·클러스터 모으기

- **범위**: `range=hour|day|week|month|year`이거나 `from`·`until`(유닉스 ms)이다. `points`는 기본 300, 최대 1000이다.
  - 층 고르기(`choose_tier`): 범위가 그 층의 기간 안에 드는 가장 고운 층을 고른다(1시간 → `Raw5s`, 1일 → `Min1`, 1주 → `Min30`, 그 밖 → `Hour6`).
  - `from`이 그 층의 남은 기간보다 이르면 한 층 거칠게 고른다.
  - 그 뒤 `points` 안으로 묶어 줄인다(`downsample`). `Raw5s`는 원본을 AVG=MAX=MIN으로 보낸다.
- **통계**: 고른 층의 범위 전체로 `avg`(아는 칸 수로 가중), `max`(MAX의 최대), `min`(MIN의 최소)을 셈한다.
- **백분위**:
  - 범위가 지난 1시간 안이면 메모리 5초 원본으로 최근순위(nearest-rank) p50·p95를 정확히 셈한다(`approx: false`).
  - 더 긴 범위는 고른 층의 AVG 값으로 셈하고 `approx: true`다. 평균으로 셈한 p95는 짧은 봉우리를 낮게 본다(웹·CLI가 "근사"라고 적는다).
  - 재시작 직후처럼 1시간이 다 차지 않았으면 `covered_from`이 실제 시작이다.
- **한 노드·VM·풀**: 노드 기록은 그 노드에, VM 기록은 VM 주인 노드에만 RPC 하나로 묻는다(계획 44 `/vms/{key}/metrics`처럼 모든 노드에 묻지 않음 — 마이그레이션 직후 밀어 넘기기 전 10분 안에는 앞 기록이 비어 보일 수 있고 `covered_from`이 알린다). 풀 기록은 그 풀이 `ready`인 노드 하나(요청 노드 먼저, 없으면 이름순)에 묻는다. 닿지 않으면 503 `node_unreachable`, 기능이 없으면 409 `cluster_upgrading`이다.
- **클러스터 모으기** `GET /metrics/cluster?range=&points=120`:
  - 요청 노드가 위트니스를 뺀 온라인 노드에 `ReadMetricsHistory { scope: Digest { top: 10 } }`을 `fan_out::gather`(32개씩)로 묻는다. 각 노드는 자기 노드 열(AVG만, 120점)과 범위 평균 CPU 상위 VM 10개를 돌려준다(노드마다 약 5 KB).
  - 요청 노드가 클러스터 합(`cluster_sum`: CPU는 코어 수로 가중한 %, 나머지는 합), 전체 상위 VM 10개, 노드 줄(20개 넘으면 평균 CPU 상위 20개)을 만들고, 풀 사용량은 Raft `NodeStatus.pools`(지금 값)로 붙인다. 응답에 `unreachable`·`unsupported`가 붙는다.
  - 리더를 거치지 않고, 같은 `(range, points)`는 그 API 노드에서 10초 캐시한다. 부하 요약(리더 메모리)은 쓰지 않는다 — 리더에 읽기가 몰리고 VM이 256개에서 잘린다. 칸 경계가 유닉스 기준이라 노드 사이에서 맞는다(시계 차이는 계획 44처럼 그대로 보인다).

## 8. 경보 규칙과 평가

- **누가 평가하나**(규모 때문에 둘로 나눈다):
  - **노드 경보**: 지표 규칙, `VmFailed`, `TaskFailed`·`HaRecovery`, 로컬 풀의 `PoolUsage`는 그 일이 일어난 노드가 평가한다.
    - 지표는 이 노드의 `MetricsStore`를, VM은 `status.node`(없으면 `spec.node`)가 이 노드인 것을, Task는 `TaskSpec.node`가 이 노드인 것을 본다.
    - 공유 풀은 그 풀이 `ready`인 온라인 노드 가운데 id가 가장 작은 노드가 평가한다. 모두 Raft에서 정해지므로 맞춰 볼 필요가 없다.
    - 일이 데이터 크기와 함께 노드마다 나뉘므로, 노드 수가 늘어도 한 노드의 일은 늘지 않는다.
  - **리더 경보**: `NodeOffline`·`BackupJobFailed`는 리더가 평가한다. 그 판단(`online`, 백업 작업 실행)이 이미 리더의 것이고, 바뀔 때만 일이 생긴다.
- **엔진**: `opl-api/src/alerts.rs`의 `AlertEngine::spawn(state)`(janitor 틀)이다. 5초마다 지표 규칙을 보고, `Cluster::watch`로 Task·VM·노드·백업 작업 변경을 받는다. 지표는 `ApiState.metrics`(`StoreSource`)로 읽는다.
- **상태 기계**(키 = 규칙 id + 대상 종류·id): 조건이 참이 되면 `pending`, 참인 채 `for_secs`가 지나면 `firing`(알림), `firing`인 동안 `repeat_secs`마다 다시 보내고, 거짓인 채 60초가 지나면 `resolved`(`send_resolved`면 알림, 깜박임 막기)다. 지표 규칙은 5초 점마다 보고, 값이 `None`이면 상태를 바꾸지 않는다. 대상이 사라지면(VM 지움, 노드를 떠남) 알림 없이 해제한다. 이벤트 조건은 한 번 보내고 끝이며 같은 Task id로 두 번 보내지 않는다.
- **상태 두는 곳**:
  - 노드 경보는 노드 파일 `{data_dir}/alerts/state.json`(0600, 임시 파일 → fsync → rename)에 둔다. 활성 경보, `last_sent`, 처리한 watch 리비전을 담는다. 그래서 opld를 다시 시작해도 같은 경보를 다시 보내지 않고, 놓친 이벤트를 리비전부터 이어 본다. 리비전이 압축돼 사라졌으면 지금부터 본다(경고).
  - 리더 경보는 `AlertRule.status.active`(Raft)에 둔다. 바뀔 때만 쓰므로 수가 적다. 리더가 바뀌어도 새 리더가 이어받아 다시 보내지 않는다.
  - 노드 경보를 Raft에 쓰지 않는 까닭: 지표 폭주 때(노드 300 × VM 경보) 모든 쓰기가 리더로 몰린다.
- **상한**: 노드마다 활성 경보는 500개까지다. 넘으면 경보 하나(`too many alerts on node X`)로 묶는다. 보내기 대기열은 1000개까지이고, 넘으면 버리고 기록에 `dropped`를 남긴다.
- **유지 보수**: `NodeSpec.maintenance` 노드의 노드 지표 규칙과 `NodeOffline`은 평가하지 않는다. 그 노드의 VM 규칙은 그대로 본다. 들어갈 때 활성 노드 경보는 알림 없이 해제한다.
- **읽기**: `GET /alerts`는 Raft의 리더 경보와, 모든 노드에 `ReadAlerts`를 `fan_out::gather`로 물은 노드 경보를 합친다. 계획 44 지표와 같은 비용이고, 웹은 30초마다 읽는다. 닿지 않는 노드의 경보는 보이지 않지만, 그 노드의 `NodeOffline` 경보가 대신 보인다.

## 9. 알림 대상과 보내기

- **보내는 노드**: 경보를 가진 노드가 보낸다. 노드 경보는 그 노드, 리더 경보는 리더다. 대상의 연결 정보는 Raft에서 읽는다.
  - 보내기는 노드마다 동시에 4개까지, 제한 10초다. 실패하면 30초·2분·10분 뒤 세 번 다시 한다.
  - 대상마다 노드마다 분당 20개까지 보낸다. 넘는 것은 다음 분에 "N건 더"라는 알림 하나로 묶는다.
- **SMTP**: 새 의존성 `lettre` 0.11(MIT)을 기본 기능 없이 `smtp-transport`·`builder`·`tokio1-rustls`·`ring`으로 쓴다(`deny` 작업에서 확인). 본문은 일반 텍스트이고, STARTTLS·TLS·평문(127.0.0.1 중계용)을 고른다. 제목은 `[Orbivisor {클러스터}] FIRING critical: {규칙} on {대상}`이다.
- **webhook**: 지금 있는 `reqwest`를 쓴다. `ca_pem`이 있으면 그 CA만 믿는다.
  - 형식: `json`(`{version: 1, cluster, state, severity, rule, subject: {kind, id, name, node}, value, threshold, since, at}`), `slack`(`{"text": …}`, Mattermost·Rocket.Chat과도 맞음), `teams`(Workflows Adaptive Card `{"type":"message","attachments":[…]}`), `ntfy`(텍스트 본문 + `Title`·`Priority`·`Tags` 헤더, `token`이면 `Authorization: Bearer`), `gotify`(`{title, message, priority}`, `X-Gotify-Key`).
  - 사설 주소도 받는다(온프레미스 대상). 대상은 admin만 만든다.
- **비밀**:
  - API 응답에서 `password`·`token`·헤더 값을 빼고 `has_password`·`has_token`·`header_names`만 보인다. `url`은 `scheme://host[:port]/…`로 줄인다(Slack·Teams URL 경로가 비밀이다).
  - 바꾸기 본문에서 비밀 칸을 빼면 저장된 값을 지킨다(`realms.rs`의 `bind_password` 규칙). `url`의 호스트가 바뀌면 비밀을 다시 줘야 한다.
  - `/watch` 목록에 넣지 않는다. 오류 문구·로그·기록에는 줄인 URL만 쓴다. reqwest 오류의 URL은 `without_url()`로 뗀다.
- **시험 보내기**: `POST /notification-targets/{key}/test`는 요청 노드에서 바로 보낸다. 200 `{ok, error?}`를 돌려주고, 기록에 `Test`로 남기고, 감사한다.
- **보낸 기록**: 보낸 노드의 `{data_dir}/alerts/history.log`(JSON Lines, 4 MiB에서 한 번 돌림)에 둔다. `GET /alerts/history?since=&limit=`은 `ReadAlerts`로 모아 시각 순으로 합친다(감사 로그 `ReadAudit`과 같음).

## 10. Prometheus 끝점

- `GET /api/v1/metrics`(text/plain; version=0.0.4)는 **그 노드의 것만** 내보낸다. 각 노드를 따로 긁는다. 위트니스는 클러스터 값만 내보낸다.
- **인증**: 지금의 API 토큰(`Authorization: Bearer opl_<id>_<secret>`, SHA-256 비교라 싸다)이나 세션을 쓴다. `Read` Node와 `Read` Vm 권한이 있어야 하므로 viewer 토큰으로 된다. 따로 읽기 전용 토큰 종류는 만들지 않는다. viewer 역할이 이미 그것이고, 폐기·감사도 같다.
- **켜기**: 설정 `prometheus.enabled`가 꺼져 있으면 404 `not_found`다. 응답은 5초 캐시한다.
- **이름**(마지막 5초 점, gauge): 노드 `orbivisor_node_cpu_usage_ratio`(0~1)·`_memory_used_bytes`·`_memory_total_bytes`·`_disk_read_bytes_per_second`·`_disk_write_…`·`_network_receive_bytes_per_second`·`_network_transmit_…`·`_fs_used_bytes`·`_fs_total_bytes`, VM `orbivisor_vm_cpu_usage_ratio`·`_memory_rss_bytes`·디스크 바이트·IOPS·네트워크, 풀 `orbivisor_pool_used_bytes`·`_total_bytes`, 클러스터 `orbivisor_cluster_nodes{state="online|offline|maintenance"}`·`orbivisor_cluster_vms{phase}`·`orbivisor_cluster_leader_present`·`orbivisor_node_is_raft_leader`·`orbivisor_alerts_firing{severity}`(이 노드 것).
- **카디널리티**: 레이블은 `node`(이름)·`vm_id`·`pool`·`state`·`phase`·`severity`뿐이다. VM 이름은 `orbivisor_vm_info{vm_id, vm, node} 1` 하나로만 붙인다(이름을 바꿔도 시리즈가 끊기지 않음). Task id·사용자·디스크 장치는 레이블로 쓰지 않는다. 값이 `None`이면 그 줄을 빼고 내보낸다.
- 문서(가이드 "지표")에 `scrape_configs`(`scheme: https`, `metrics_path: /api/v1/metrics`, `authorization.credentials_file`, `tls_config.ca_file` = 클러스터 CA, 노드마다 대상)와 Grafana 예 쿼리를 싣는다. 클러스터 값은 노드마다 같으므로 `max without(instance)`로 묶는다.

## 11. 다른 기능과의 관계

- **라이브 마이그레이션**: 기록은 따라간다(§6 밀어 넘기기). 마이그레이션 중에는 원본과 대상이 모두 그 VM을 잰다(계획 44와 같음). 합칠 때 대상 칸이 이기므로 겹친 몇 초가 두 번 세이지 않는다. 그 사이 조회는 주인(확정 뒤 대상)만 보므로 최대 10분 동안 앞 기록이 빈다. 그동안 1시간 원본은 계획 44 `/vms/{key}/metrics`(모든 노드)로 이어 보인다.
- **HA**: 죽은 노드의 VM 기록은 그 노드가 돌아오면 새 주인에게 밀려간다. 돌아오지 않으면 잃는다(README 한계). HA 복구는 `HaRecovery` 이벤트 규칙으로 알릴 수 있다. 펜싱된 노드의 경보는 그 노드 파일에 남았다가, 다시 켜질 때 대상이 이 노드에 없으면 알림 없이 해제한다.
- **노드 지우기**: 비우기를 먼저 권하므로 VM 기록은 밀려 나간다. 지운 노드의 노드 기록과 경보는 함께 사라진다. 리더는 그 노드의 `NodeOffline` 경보를 해제로 보낸다(대상 사라짐).
- **VM 지우기**: §6 정리 규칙으로 주인 노드와 남은 노드의 파일을 지운다. 활성 VM 경보는 알림 없이 해제한다.
- **유지 보수**: 기록은 계속하고, 노드 경보만 쉰다(§8).
- **버전이 섞인 클러스터**: §2의 두 기능으로 가린다.
  - 옛 노드는 RRD를 쓰지 않는다. 그 노드의 기록 조회는 409 `cluster_upgrading`, 클러스터 모으기에서는 `unsupported`이고, 밀어 넘기기는 받는 쪽이 `metrics-history`를 알릴 때만 한다.
  - `AlertRule`·`NotificationTarget`은 모든 노드가 `notifications`를 알린 뒤에만 쓸 수 있으므로 옛 노드는 모르는 종류를 받지 않는다. 업그레이드 중 옛 노드의 지표 경보는 빈다.
- **규모(수백 노드)**: 리더가 하는 새 일은 `NodeOffline`·`BackupJobFailed` 평가와 그 보내기뿐이다(바뀔 때만).
  - 지표 기록·평가·보내기는 노드마다 제 몫만 하고 Raft에 쓰지 않는다. 모으기 읽기(`/metrics/cluster`, `/alerts`)는 요청 API 노드가 32개씩 묻고, 클러스터 모으기는 10초 캐시한다(계획 47 §5와 같은 틀).
  - 리더의 Raft 일이 늘지 않으므로 규모 시험(`scale.rs`)은 바꾸지 않는다. 대신 API 시험이 노드 300개 가짜 소스로 `/metrics/cluster` 응답 크기와 동시성 상한을 본다.
- **디스크 닳음**:
  - VM 파일 하나가 분마다 쓰는 것은 `Raw5s` 384 B, `Min1` 96 B, 머리 512 B로 페이지 단위 약 3 × 4 KiB와 `fdatasync` 둘이다. VM 100개 노드는 하루 약 1.7 GB, 해마다 약 0.6 TB다. 보통 SSD(수백 TBW)에는 작고, 파일 크기·메타데이터가 바뀌지 않아 저널 쓰기는 거의 없다.
  - SD 카드·USB 부팅 노드는 `flush_secs = 600`(약 1/8, 대가는 전원이 나가면 10분을 잃음)이나 `history = false`를 쓴다. README에 적는다.
- **위트니스**: 기록하지 않는다. Prometheus는 클러스터 값만 내보낸다.

## 12. API·CLI·웹

- **API**(OpenAPI 태그 `metrics`·`alerts`):
  - 기록: `GET /nodes/{key}/metrics/history`, `/vms/{key}/metrics/history`, `/storage-pools/{key}/metrics/history`(`range|from,until`, `points`; `Read` 그 종류), `GET /metrics/cluster`(`Read` Node). Prometheus `GET /metrics`(§10), 설정 `PATCH /settings {prometheus}`.
  - 경보·대상: `/alert-rules[/{key}]`(읽기 모두, 쓰기 admin), `GET /alerts`·`/alerts/history`(`Read` AlertRule), `/notification-targets[/{key}]`와 `POST …/{key}/test`(admin).
  - 오류는 400 `invalid_request`(범위·`points`·조건), 404, 409 `cluster_upgrading`, 503 `node_unreachable`. 감사 설명은 `alert-rule create cpu-hot`, `notification-target test ops-mail`, `settings prometheus on`.
- **CLI**:
  - 기록: `node stats --range day|week|… [NODE]`, `vm stats VM --range week`, `pool stats POOL --range month`는 칸마다 AVG·MAX·MIN·p95 요약 표(근사면 `p95~`)이고 `-o json`은 응답 그대로다. `--range` 없이는 지금처럼 마지막 점이다.
  - `alert rule list|get|create|set|delete`(`--node-metric cpu --gt 90 --for 5m --target ops-mail`), `alert list`, `alert history [--since 1h]`, `notify target list|get|create|set|delete|test`(비밀번호·토큰은 묻거나 표준 입력, 인자로는 받지 않음), `prometheus enable|disable`(`telemetry enable|disable`처럼 `PATCH /settings`).
- **웹**:
  - 차트: 새 의존성 `uplot`(MIT, 약 50 KB, 모니터링 설계 §7 결정). 래퍼 `web/src/charts/TimeChart.tsx`는 색을 CSS 토큰에서 읽어 `.dark`를 따르고, `null`은 끊어 그린다. MAX·MIN은 옅은 띠로 그린다. 범례에 AVG·MAX·MIN을 둔다. 지난 1시간이면 p50·p95를 붙이고, 근사면 "≈"를 붙인다. 접근성을 위해 차트마다 `aria-label` 요약과 "표로 보기"(같은 열의 표)를 둔다.
  - 범위 고르기는 1시간·1일·1주·1개월·1년이다. 1시간은 지금처럼 5초 `since` 폴링으로 이어 붙이고, 나머지는 60초마다 다시 읽는다.
  - 대시보드(지금 카드 아래): 클러스터 CPU·메모리·디스크·네트워크 차트(범위 고르기), 노드 줄(20개 넘으면 상위 20개), 상위 VM 10개, 풀 사용률 막대(85% 넘으면 경고색), 활성 경보 목록(심각도 순, 30초).
  - 새 노드 상세 `nodes/:name`(차트 넷, 그 노드 VM 표, 노드 목록 이름이 링크), VM 상세 "지표" 탭(네 차트), 새 화면 `alerts`(활성 경보·규칙·보낸 기록, admin에게 "알림 대상" 탭과 시험 보내기), 설정의 "Prometheus" 칸(켜기, 스크레이프 예 복사). i18n은 en·ko(`metrics.*`, `alerts.*`).

## 13. 시험과 CI

- **순수(어디서나)**: serde(옛 설정·노드 JSON 바이트 그대로, 새 종류의 `deny_unknown_fields`), `choose_tier`·`downsample`·`stats`·`percentile`(빈 값, NaN만, 1점)과 `cluster_sum` 가중, 규칙 검증과 상태 기계 표(pending → firing → 반복 → resolved, `None` 값, 대상 사라짐), Prometheus 텍스트 출력(`insta` 고정 값).
- **RRD(이 Mac·svc01, tempfile)**: `t`를 직접 주어 합치기(xff), 층 닫기와 재시작 뒤 메우기, 앞·뒤 시계 뛰기(1시간 경계 양쪽), 공백 NaN, 층 기간보다 긴 공백을 본다. A/B 머리는 한 사본 깨짐과 쓰기 단계마다 끊는 "죽음"(데이터만 쓰고 머리 못 씀)을 흉내 낸다. 버전 옮기기(v1 → 칸 더한 가짜 v2), 더 새 버전 옆으로 옮기기, 칸 단위 합치기의 멱등, 고정 크기 336 KiB도 본다.
- **opld·API(이 Mac, `tokio::time::pause` + 시계 주입)**:
  - `HistoryWriter`·`AlertEngine`은 `Clock` 트레이트(`now_ms`)를 받는다. 시험은 `FakeClock`으로 벽시계를 앞뒤로 뛰게 한다.
  - API 시험(`MockHypervisor`, 피어 서버 하나 더): 기록 조회·주인 노드 하나만 묻기, `Digest` 모으기와 10초 캐시, 노드 300개 가짜 소스의 동시성 상한(`FAN_OUT`).
  - 경보·알림: 리더 경보가 리더 바뀜 뒤 다시 가지 않음, 노드 경보 `state.json`과 재시작, 기능 게이트 409, RBAC(viewer는 대상을 읽지 못함), 비밀 가림(응답·`Debug`·기록·오류 문구에 비밀 문자열이 없음을 grep으로 확인).
  - 보내기: 127.0.0.1 HTTP 수신기로 다섯 형식의 본문을, 가짜 SMTP 서버(`lettre` 대상, 시험 안 TCP 서버)로 메일 본문을 본다. 다시 하기·분당 상한도 본다.
- **opld E2E(새 `tests/metrics_history.rs`·`tests/alerts.rs`)**: QEMU가 필요 없는 것은 기능 없이 이 Mac·svc01에서 돈다(`audit.rs`처럼).
  1. **재시작 뒤 기록**: 노드 하나를 `[test] metrics_flush_secs = 1`로 띄워 점이 쌓이면 SIGKILL로 끝내고 다시 띄운다. `node stats --range hour -o json`에 재시작 전 점이 있고, 공백이 `null`이다.
  2. **마이그레이션 뒤 기록**(`qemu-it`, 두 opld): 디스크 없는 VM을 A에서 켜고 기록이 생기면 끈 뒤 B로 옮긴다(정해진 노드 바꾸기). B에서 켜고 짧게 둔 `[test]` 밀어 넘기기 주기 뒤, B의 `vm/{id}.rrd`에 A 시절 칸이 있고 A 파일이 없어지는지 본다. GitHub `linux-kvm`의 `migration-it`에는 같은 확인을 라이브 마이그레이션으로 하나 더한다.
  3. **경보 발생·해제**: 시험 안 webhook 수신기를 127.0.0.1에 띄운다. 대상(`json`)과 규칙 `node cpu > -1 for 0s`로 `firing`을 받는다. 규칙을 `> 1000`으로 바꿔 60초 해제 대기(`[test] alert_tick_ms`로 줄임) 뒤 `resolved`를 받는다. `alert history`에 둘 다 있다.
  4. **Prometheus 긁기**: viewer 토큰으로 `/api/v1/metrics`를 받아 시험 안 텍스트 형식 파서(`tests/common/promtext.rs`: HELP/TYPE, 레이블 이스케이프, 이름 규칙, 같은 이름·레이블 중복 없음)로 푼다. 꺼진 설정이면 404, 토큰이 없으면 401이다. `qemu-it`면 `orbivisor_vm_info`와 VM 줄을 본다. GitHub `linux-kvm`에서는 apt `prometheus`의 `promtool check metrics`도 돈다.
- **svc01(aarch64)**: 일반 묶음 전체(fmt·clippy·test·scale·storage)와 `qemu-it`의 위 E2E 1·2·4를 돈다. 디스크 닳음 추정(§11)은 svc01에서 `/proc/{pid}/io` `write_bytes`로 한 시간 재어 실행 기록에 적는다.
- **GitHub**: 새 작업은 없다. `linux-kvm`에 `migration-it` 시험 하나와 `promtool` 단계를 더한다. `web` 작업은 그대로 vitest를 돈다(msw 지표·경보 응답, `TimeChart`는 시험에서 가짜). `deny`는 `lettre`·`uplot` 라이선스를 본다.

## 14. 견준 길

- **rrdtool·librrd vs Rust 파일 형식(채택).** C 의존성과 rrdcached 데몬이 없어진다. 형식이 작아서(층 넷, 고정 칸) 시험이 쉽고, 칸 목록으로 옮기기를 직접 정한다. 대가로 도구(`rrdtool dump`)를 쓸 수 없다. `-o json` 조회가 대신한다.
- **임베디드 TSDB(sled·redb 테이블)·Raft vs 고정 크기 파일(채택).** 크기가 늘지 않고, 압축·정리가 없고, 쓰기 양이 정해져 있다. Raft는 모든 노드가 복제한다(계획 44 견준 길).
- **마이그레이션: 옛 노드에 계속 묻기 vs 파일 밀어 넘기기(채택).** 묻기로 하면 조회가 VM이 거쳐 간 모든 노드에 퍼지고(지금 1시간 조회가 그렇다), 그 노드를 지우면 기록을 잃는다. 밀어 넘기기는 한 번 336 KiB이고, 조회는 늘 한 노드다.
- **경보 평가: 리더 하나 vs 노드마다(채택) + 리더는 노드 상태만.** 리더 하나는 중복이 없고 쉽다. 하지만 지표가 노드에 있으므로 리더가 모든 노드의 5초 지표를 받아야 한다. 부하 요약은 칸이 적고 VM이 256개에서 잘린다. 노드마다 하면 데이터가 있는 자리에서 하고, 리더 짐이 노드 수와 상관없다.
- **경보 상태: 전부 Raft vs 노드 파일 + 리더 경보만 Raft(채택).** 폭주 때 쓰기가 리더에 몰리지 않는다. 대가로 활성 경보 목록은 모아 읽는다.
- **Prometheus 인증: 따로 읽기 토큰 종류 vs 지금 API 토큰 + viewer(채택).** 새 비밀 종류와 화면이 없다. 끝점은 설정으로 꺼 둔다.
- **Prometheus 값: 누적 카운터 vs 마지막 5초 비율 gauge(채택).** 저장소에 있는 것이 비율이다. 카운터는 측정기 안의 기준값이고 QEMU를 다시 켜면 0이 된다. 카운터 내보내기는 §15로 미룬다.
- **SMTP: 손으로 짠 클라이언트 vs `lettre`(채택).** AUTH·STARTTLS·점 처리를 다시 짜지 않는다.
- **알림 설정: 하나의 설정 칸 vs 리소스 둘(채택).** 규칙과 대상을 따로 RBAC·감사·watch 하고, 대상의 비밀은 admin 종류로 가둔다.

## 15. 나중 할 일

Prometheus 누적 카운터·OpenMetrics, Graphite·InfluxDB 밀어 보내기, 경보 묶기·억제(노드 경보가 그 VM 경보를 가림)·일정·개별 확인(ack), SNMP 트랩, 디스크·NIC마다 나눈 기록, 게스트 안 지표(에이전트 `guest-get-fsinfo` 사용량 기록), 기록 내보내기(CSV), 웹의 사용자 정의 대시보드, 측정 간격 설정, 지운 노드 기록 옮기기, 3D 보기·끌어 놓는 VM 만들기(모니터링 설계 §8·§9).

## 16. 스테이지 나누기

각 스테이지는 커밋 하나이고, 혼자 컴파일되고 시험된다. 앞 스테이지의 새 RPC는 `Unimplemented`이고, API는 그 스테이지에서 연다.

1. **feat(types): 지표 기록·경보·알림 타입과 기능, 순수 계산(opl-types)**
   - `HistoryQuery`·`HistoryReply`·`Tier`·`MetricField`, `metrics_math`(`choose_tier`·`downsample`·`stats`·`percentile`·`cluster_sum`).
   - `alert.rs`(규칙·조건·대상·기록, 비밀 가린 `Debug`), `Kind::AlertRule`·`NotificationTarget`, `SettingsSpec.prometheus`, `METRICS_HISTORY`·`NOTIFICATIONS`, serde 하위 호환 시험.
2. **feat(opld): RRD 파일 형식(opld `metrics/rrd.rs`)**
   - 층·칸 주소·합치기·A/B 머리·`fdatasync` 순서·시계 뛰기·버전 옮기기·칸 단위 합치기, 끊긴 쓰기 시험.
3. **feat(opld,peer): 분마다 기록·재시작 되살리기·밀어 넘기기·정리, 기록 RPC(opld·opl-peer)**
   - `HistoryWriter`, 풀 기록, `Raw5s` 되살리기, `PushMetricsHistory`·`ReadMetricsHistory`(`Digest` 포함), 지운 VM·풀 정리, `[metrics]` 설정과 `[test]` 고리, `Clock`.
4. **feat(api): 기록 조회·클러스터 모으기·Prometheus(opl-api)**
   - §7·§10 경로, 기능 게이트, 10초·5초 캐시, OpenAPI·`schema.d.ts`.
5. **feat(api): 경보 엔진·알림 대상·보내기(opl-api·opl-peer)**
   - `AlertEngine`(노드·리더 평가, `state.json`, watch 리비전), `ReadAlerts`, SMTP(`lettre`)·webhook 다섯 형식, 시험 보내기, 보낸 기록, 규칙·대상 API·RBAC·감사·비밀 가림.
6. **feat(cli): stats `--range`·alert·notify·prometheus 명령, opld E2E와 CI(oplctl·opld·CI)**
   - CLI 명령과 표시, `tests/metrics_history.rs`·`tests/alerts.rs`·`promtext`, `migration-it` 라이브 확인, `linux-kvm`의 `promtool` 단계.
7. **feat(web): uPlot 차트, 대시보드, 노드 상세, VM 지표 탭, 경보 화면, Prometheus 설정(web)**.
8. **docs: README·가이드 "지표"(기록·경보·알림·Prometheus·Grafana 예·디스크 닳음·한계), 설계 문서 스테이지 보고, 계획 60 실행 기록**.

## 17. 스테이지 보고 (설계와 다른 것·결정)

### 스테이지 1 (opl-types)

- **시각 단위**: 경보·알림의 시각(`since`·`last_sent`·`at` 등)은 지표 점처럼 유닉스 ms(`i64`)다.
- **풀 칸 단위**: `pool_used`·`pool_total`은 MiB다(`fs_*`와 같고 f32 칸에 맞음). `metrics::pool_values(total, available)`가 바꾼다. Prometheus가 바이트로 되돌린다.
- **`HistoryReply.top`**: `Digest`의 상위 VM(`TopVm { vm, cpu, mem_rss }`, 범위 평균)을 담을 칸이 설계에 없어 더했다(비면 쓰지 않음). `Digest`의 열은 AVG만이라 `Cf.max`·`min`은 비면 쓰지 않는다.
- **층 고르기 여유**: API 노드가 셈한 `from`이 기록 노드에 닿을 때 시계 차이·지연으로 층의 남은 기간보다 조금 이를 수 있어, `choose_tier`는 60초(`TIER_SLACK_MS`) 여유를 둔다. 앞 몇 칸이 `null`이 되고 `covered_from`이 알린다.
- **조회 검증**: `HistoryQuery::validate`는 `from < until`, 범위 366일 이하(`MAX_HISTORY_RANGE_MS`), `points` 1~1000, `Digest.top` 1~50을 본다.
- **순수 함수 더함**: 설계의 다섯 함수 말고 `bucket_window`(범위가 걸치는 칸), `group_size`, `consolidate`(5초 → 1분, xff 0.5)·`consolidate_aggs`(1분 → 30분·6시간), `to_cf`, `percentiles`, `merge_top`(노드마다의 상위 VM 합치기)을 `metrics_math`에 두었다. 경보 상태 기계도 순수 함수 `alert::advance`로 두어 표 시험을 했다.
- **`ActiveAlert`**: `key`는 칸이 아니라 메서드(`{rule}/{kind}/{id}`)다. 대상은 `AlertSubject { kind, id, name, node }`(webhook `json`의 `subject`와 같음)이고, 단계 `state: pending|firing`과 해제 대기 시작 `clear_since`를 더했다(깜박임 막기 60초를 재시작·리더 바뀜 뒤에도 잇기 위함).
- **`NotificationTargetSpec`**: 칸 이름이 `kind`면 JSON이 `{"kind":{"kind":"smtp"}}`가 되어 `target: TargetConfig`(`kind` 태그, `RealmSpec.backend`와 같은 꼴)로 했다: `{"target":{"kind":"smtp",…},"enabled":false}`.
- **규칙 검증 더함**: `vms`는 VM 조건(`vm_metric`·`vm_failed`·`ha_recovery`)에만, `nodes`는 `backup_job_failed`에 받지 않는다. `nodes`·`vms`는 256개까지, `TaskFailed.ops`는 64개까지 이름 형식이다. `repeat_secs`는 0이나 60~7일이다. 이벤트 조건은 `for_secs = 0`만 받고, `repeat_secs`·`send_resolved`는 받되 쓰지 않는다.
- **대상 검증 더함**: webhook `token`은 `ntfy`·`gotify`에만(다른 형식은 `headers`에), 헤더는 16개까지이고 `Host`·`Content-Length`·`Transfer-Encoding`·`Connection`은 받지 않는다. 메일 주소는 `local@domain` 모양만 보고 CR/LF를 막는다. 오류 문구의 URL은 `redact_url`로 줄인다.
- **`NotificationRecord`**: 여러 노드의 기록을 합치므로 보낸 노드 `node`를 더했다. 대기열이 넘쳐 버린 것은 `ok: false`, `error: "dropped…"`로 남긴다(따로 상태를 두지 않음). 반복은 `firing`이다.
- **serde 한계**: `AlertCondition`의 칸 없는 변형(`node_offline` 등)은 serde가 남는 칸을 막지 못한다. 칸이 있는 변형과 규칙·대상 struct는 `deny_unknown_fields`로 막는다.
- **뒤 스테이지로**: `auth.rs`의 `account` 목록에 `NotificationTarget` 넣기(스테이지 5, 지금은 끝점이 없고 `/watch`는 명시 목록이라 새지 않음), `AlertsQuery`·`AlertsReply`·`HistoryWindow`(스테이지 5), 대상 응답의 비밀 뺀 모양(스테이지 5, `realms.rs`처럼 opl-api).

### 스테이지 2 (opld `metrics/rrd.rs`)

- **API 모양**: 한 파일이 `Rrd`다. `append(t, values)`는 5초 점 하나를 쓰고(데이터만), `close(now)`는 `now`까지 끝난 거친 층 칸을 닫고, `sync_data()` → `commit()`이 §5 쓰기 순서다(파일 하나면 `flush()`). 여러 파일은 쓰는 쪽(스테이지 3)이 모두 `sync_data` 한 뒤 모두 `commit` 한다. `fdatasync`는 rustix 대신 `File::sync_data`(Linux는 `fdatasync`, macOS는 `F_FULLFSYNC`)를 쓴다 — rustix의 `fdatasync`는 macOS에 없다.
- **만들기**: `fallocate` 대신 NaN으로 채운 전체 내용을 `{name}.tmp`에 쓰고 fsync → rename → 디렉터리 fsync 한다(크기 336 KiB라 비용이 작고 macOS에서도 같은 길). 두 머리 사본(seq 0·1)을 같게 쓴다. 사본 자리는 `seq mod 2`라서 다음 commit은 늘 옛 사본을 덮는다.
- **크기**: VM 336 KiB(설계대로), 노드 376 KiB, 풀 92 KiB.
- **머리 배치(512 B)**: magic 8, version u16, 종류 u8(node 1·vm 2·pool 3), 칸 수 u8, seq u64(16), 칸 id 64바이트(24, 한 번 정한 번호는 바꾸지 않음: cpu 1 … pool_total 14), 층 표 4 × 32바이트(88: 층 id·rows·step_ms·offset·last, 칸 없음 `i64::MIN`), 504..512에 SHA-256 앞 8바이트. 층 표는 열 때 이 빌드의 배치와 맞는지 본다.
- **닫기 순서**: 큰 공백을 건너뛸 때 `Min1`이 먼저 밀려 나가면 닫히지 않은 30분·6시간 칸의 재료를 잃으므로, 점을 쓰기 전에 닫고 원본 끝을 6시간 경계로 올린 시각까지 먼저 닫는다. 처음 쓰는 파일은 거친 층의 `last`를 지금 칸 앞으로 둔다(앞 칸을 셈하지 않음).
- **시계 뒤로**: `last` 칸 시작 − `t` ≥ 1시간(`REWIND_MS`)이면 되돌린다. 층마다 `t`가 든 칸부터 `last`까지 지우고 `last`를 그 앞 칸으로 둔다(설계의 `(새 칸, last]`보다 한 칸 더 — 열린 칸은 닫힐 때 다시 만든다). 같은 칸·1시간 미만이면 `Append::Behind`이고, 경고 한 번은 쓰는 쪽이 남긴다.
- **버전 옮기기**: 칸 목록이 이 빌드와 다르면(버전이 같아도) 옮긴다. 다른 종류이거나 파일이 짧으면 깨진 것으로 보고 옆으로 옮긴다. 사본 하나라도 더 새 버전이면 새 형식으로 본다(내려 깐 빌드가 새 파일을 덮지 않게). 옆으로 옮길 이름이 이미 있으면 `.1`, `.2`…를 붙여 지우지 않는다.
- **조회**: `read(tier, from, until)`은 `bucket_window` 칸을 칸(필드)마다 `Agg`로 돌려준다(`Raw5s`는 AVG=MAX=MIN, 모르면 NaN). 칸이 층 칸 수의 두 배보다 많으면 거절한다(366일 `Hour6`은 됨).
- **합치기**: `merge(bytes)`는 받은 파일 바이트(머리 포함)와 층마다 새 `last`의 창 전체를 칸 단위로 합쳐 쓰고 commit 한다. 칸 목록이 다르면 같은 칸 id만 합친다. 종류가 다르거나 머리가 깨졌거나 짧으면 거절한다.
- **로그**: 이 모듈은 로그를 남기지 않는다. `Opened`(Existing·Created·Migrated·Corrupt·Newer)와 `Append`(Written·Behind·Rewound)를 돌려주고 쓰는 쪽이 남긴다. opld가 바이너리 크레이트라 스테이지 3이 쓰기 전까지 `metrics.rs`에 `#[allow(dead_code)]`를 두었다.

### 스테이지 3 (opld `metrics/history.rs`·opl-peer)

- **구조**: `History`(파일 핸들·조회·받기, 막히는 호출)와 `HistoryWriter`(분마다 기록, 정리·밀어 넘기기 주기)로 나눴다. `History`는 위트니스가 아닌 노드에 늘 있고, `[metrics] history = false`면 파일 없이 메모리 1시간으로 조회에 답한다(밀어 넘기기는 `failed_precondition`으로 거절해 보낸 쪽이 파일을 지키게 함). 디렉터리를 만들지 못하면 경고하고 기록을 끈다(노드 시작을 막지 않음).
- **가져갈 점 찾기**: `MetricsStore`에 시리즈마다 넣은 점 번호를 두고 기록 쓰기가 앞서 가져간 번호 뒤의 점만 가져간다(`take_pending`). 시각이 아니라 넣은 순서로 세므로 벽시계가 뒤로 가도 `Append::Behind`·`Rewound` 길로 간다. 새 VM 시리즈는 전체 점 수부터 세어 지워졌다 다시 생긴 시리즈도 놓치지 않는다.
- **풀 점**: 풀은 5초마다 재지 않으므로 기록할 때 지난 기록 뒤의 5초 칸마다 `PoolMounter`가 마지막으로 잰 값(`PoolLevels`, 1 GiB 거르기 전, `ready`만)을 쓴다(처음은 지금 칸 하나, 이미 쓴 칸은 건너뜀). 그래야 1분 층이 xff 0.5를 넘는다.
- **쉬는 파일 닫기**: 기록마다 열린 모든 파일의 거친 층을 닫되, 마지막 원본 칸이 든 6시간 경계까지만 닫는다(`close_until`). 멈춘 VM의 칸이 제때 닫히고, 그 뒤로는 분마다 NaN을 쓰고 `fdatasync` 하지 않는다.
- **기록 시각**: 기록은 벽시계 주기 경계 + 1초에 한다. 멈출 때(`Services::shutdown`, 측정기 다음) 한 번 더 기록한다.
- **5초 층 조회**: 파일의 빈 칸을 아직 쓰지 않은 메모리 점으로 메운다(파일 값 우선). 그래서 1시간 조회는 지금까지 이어지고 p50·p95가 정확하다(`approx: false`). `covered_from`은 앞 칸을 모를 때만 아는 첫 칸 시각이고, 아무 칸도 모르면 없다.
- **`Digest`**: 노드 열은 AVG만(MAX·MIN 비움), `stats`·`pct`는 비운다. 상위 VM은 파일·메모리에 있는 VM 가운데 Raft에서 주인이 이 노드인 것만 범위 평균 CPU로 고른다(마이그레이션 직후 두 노드가 같은 VM을 내지 않게).
- **밀어 넘기기·정리 주기**: 10분마다, 시작 1분 뒤 첫 차례(후보만 모음), 이 노드에 파일이 있는 VM이 바뀌거나 지워지면(watch) 30초 뒤로 앞당긴다. 마지막 점이 30초 안인 VM(아직 이 노드에서 잼)은 밀지도 지우지도 않는다. 받는 쪽이 온라인이고 `metrics-history`를 알릴 때만 보내고, `unimplemented`(옛 노드)·`failed_precondition`(기록 끔)이면 파일을 두고 다음 차례에 다시 본다. 받은 쪽이 합쳐 디스크에 쓴 뒤에만 지운다.
- **정리 규칙**: 계획 59와 같다(두 차례 연속 후보, `read_index` 성공, 그 id를 대상으로 하는 끝나지 않은 Task 없음, 지우기 직전 다시 읽어 없음). 상태 디렉터리 정리의 "1시간 지난 것" 조건은 두지 않았다(기록 파일은 Task가 VM을 만들기 전에 쓰지 않는다). `node.rrd`는 지우지 않는다. 옆으로 옮긴 파일(`.corrupt`·`.v{n}`)과 임시 파일은 시리즈가 아니라 건드리지 않는다.
- **peer RPC**: `ReadMetricsHistory(HistoryQuery) -> HistoryReply`(JSON, 서버가 `validate` 뒤 `spawn_blocking`), `PushMetricsHistory`(이진: VM id 16바이트 + 파일 바이트, 1 MiB 상한, 답은 빈 메시지). `MetricsSource`에 `history()`·`push_history()`를 기본 `Unsupported`로 더했다. 오류는 `HistoryError { Unsupported, Disabled, Invalid, Io, Unreachable }`이고 `unimplemented`·`failed_precondition`·`invalid_argument`·`internal`·그 밖으로 오간다. 클라이언트는 `Peers::read_metrics_history`·`push_metrics_history`.
- **`Clock`**: `opl_types::clock::{Clock, SystemClock, ManualClock}`(스테이지 5 `AlertEngine`도 씀). 쓰기 작업 시험은 `ManualClock`과 실제 짧은 타이머로 한다(Raft가 있는 시험에서 `tokio::time::pause`는 쓰지 않음).
- **설정**: `[metrics] history = true`, `flush_secs = 60`(60~600, 검증). 시험 고리 `[test] metrics_flush_secs`·`metrics_push_secs`(1 이상, 정리의 첫 차례·앞당김도 같은 값)는 새 기능 `metrics-it`(QEMU 없이 재시작 시험용)와 `qemu-it` 빌드에만 있다. `alert_tick_ms`는 경보 엔진과 함께 스테이지 5로 미뤘다.
- **열린 파일 상한**: 4096개(`MAX_OPEN`)를 넘으면 가장 오래 쓰지 않은 VM·풀 파일을 닫는다(노드 파일은 둔다).

### 스테이지 4 (opl-api 기록 조회·클러스터 모으기·Prometheus)

- **경로**: `GET /nodes/{key}/metrics/history`, `/vms/{key}/metrics/history`, `/storage-pools/{key}/metrics/history`(`HistoryReply` 그대로), `GET /metrics/cluster`(새 `ClusterHistoryResponse`), `GET /metrics`(Prometheus), `PATCH /settings {prometheus: {enabled}}`. 조건은 `range`이거나 `from`(+`until`, 없으면 지금)이고 둘 다 없으면 지난 1시간이다. 둘을 함께 주거나 `until`만 주면 400이다. `points` 기본은 300, 클러스터는 120이다. 조회는 API 노드에서 `HistoryQuery::validate`로 먼저 본다.
- **묻는 노드**: 대상 노드의 `status.features`에 `metrics-history`가 없으면 묻지 않고 409 `cluster_upgrading`(모으기는 `unsupported`), 꺼진 것으로 기록된 다른 노드는 묻지 않고 503 `node_unreachable`이다. VM의 주인(`status.node` → `spec.node`)이 둘 다 없으면(놓인 적 없는 VM) 이 노드에 묻는다(모두 `null` 열). 풀은 그 풀이 `ready`이고 `pool_id`가 맞는 노드 가운데 이 노드 먼저, 그다음 이름 순으로 기능을 알리고 켜진 첫 노드에 묻고, 준비된 노드가 없으면 409 `pool_unavailable`이다. 노드 로컬 풀은 그 한 노드의 기록이다(노드를 고르는 매개변수는 두지 않았다). 다른 노드 읽기 한도는 5초다(파일을 읽으므로 1시간 지표 3초보다 길게).
- **오류 짝**: `Unsupported` → 409 `cluster_upgrading`, `Unreachable` → 503 `node_unreachable`, `Invalid` → 400 `invalid_request`, `Io`·`Disabled` → 500 `internal`(문구는 로그에만).
- **층 맞추기**: 같은 조회라 칸 경계가 같으므로 노드 답의 `(tier, start, step_ms)`는 시계가 크게 어긋난 노드만 다르다. 가장 많은 노드가 쓴 것(같으면 이 노드의 것, 그다음 고운 층)을 기준으로 하고, 다른 노드는 다시 맞추지 않고 새 칸 `skewed`(비면 쓰지 않음)에 이름을 넣고 열·줄에서 뺀다. 그 노드의 상위 VM은 범위 평균이라 그대로 합친다. 아무 노드도 답하지 않으면 이 노드 시계로 셈한 층·칸과 빈 열이다.
- **모으기 답 모양**: `nodes`는 `ClusterNodeRow { node, name, cpus, avg(칸마다 범위 평균), cpu(CPU 열) }`이고 20개 넘으면 범위 평균 CPU 상위 20개를 이름 순으로 둔다. 합친 노드 수 `node_count`를 더했다. `top`은 `ClusterTopVm { vm, name, node?, cpu, mem_rss }`로 Raft에 없는 VM을 뺀 뒤 10개다. `pools`는 `ClusterPoolUsage { pool, name, shared, used_bytes, total_bytes }`(Raft `NodeStatus.pools`, 공유 풀은 이름 순 첫 준비 노드의 값, 노드 로컬 풀은 준비된 노드의 합, 준비된 노드가 없는 풀은 뺌)다.
- **10초 캐시**: 열쇠는 받은 조건 그대로 `(range, from, until, points)`이고 64개까지다. 캐시는 `tokio::sync::Mutex` 안에 있고 채우는 동안 잡고 있어, 한 API 노드의 모으기는 한 번에 하나다(겹친 대시보드 요청이 노드에 묻는 수를 늘리지 않음, 동시 연결은 `FAN_OUT` 32개). 300노드 시험은 가짜 읽기로 순수 합치기 함수(`merge_digest`)와 `fan_out::gather`를 본다(gRPC 서버 300개 대신).
- **Prometheus**: 인증(401)을 먼저 보고, 그다음 Node·Vm 읽기 권한(403), 그다음 꺼짐(404 `not_found`)이다. 캐시는 꺼짐 검사 뒤라 끄면 바로 404다. VM 줄은 `status.node`가 이 노드인 VM의 마지막 점이 15초 안일 때만, `orbivisor_vm_info{vm_id, vm, node}`는 `status.node`가 이 노드인 모든 VM이다. VM 지표 줄의 레이블은 `vm_id` 하나다. 풀은 이 노드에서 `ready`인 풀의 Raft 값(1 GiB 단위로 쓰인 값, 레이블 `pool` = 이름)이다. IOPS 이름은 `orbivisor_vm_disk_read_ops_per_second`·`_write_ops_per_second`다. `orbivisor_cluster_nodes{state}`는 유지 보수가 먼저인 서로 겹치지 않는 셋(online·offline·maintenance)이고, `orbivisor_cluster_vms{phase}`는 없는 단계도 0으로 낸다. `orbivisor_cluster_leader_present`는 `Cluster::health().quorum`이다. `orbivisor_alerts_firing{severity}`는 경보 엔진(스테이지 5)과 함께 낸다. 줄이 없는 지표는 HELP·TYPE도 쓰지 않는다. 출력 고정 시험은 `insta` 대신 기대 문자열이다.
- **설정**: `PATCH /settings`의 `prometheus: {enabled: bool?}`는 모든 노드가 `metrics-history`를 알아야 하고(다른 칸 검증보다 먼저), 켜면 `{"enabled": true}`, 끄면 칸을 지운다(옛 설정 JSON과 같게). 감사 설명은 `settings prometheus on|off`다.
- **엄격한 파서**: `opl-api/tests/common/promtext.rs`(HELP·TYPE 한 번, TYPE이 줄보다 앞, 같은 이름 줄이 이어짐, 이름·레이블 규칙, 이스케이프, 중복·빈 줄·끝 줄바꿈)를 두고 API 시험이 쓴다. 스테이지 6의 opld E2E는 `#[path]`로 같은 파일을 쓴다.

### 스테이지 5 (opl-api 경보 엔진·알림 대상·보내기, opl-peer `ReadAlerts`)

- **구조**: `opl-api/src/alerts/{mod.rs(API), book.rs(장부), engine.rs(엔진), send.rs(보내기)}`. opld는 노드 간 서버를 `AppState`보다 먼저 만들므로, 노드 상태 파일과 보낸 기록을 담는 `AlertBook`(복제하면 같은 장부)을 따로 두어 노드 간 서버(`with_alerts`, peer `AlertsSource`)와 `AppState.alerts`가 함께 쓴다. `AlertEngine::spawn(state, config)`는 장부가 없으면(위트니스) 아무것도 하지 않는다. `AlertsQuery`·`AlertsReply`·`HistoryWindow`와 API 응답 타입은 opl-types `alert.rs`에 두었다(CLI가 쓴다).
- **노드 상태 파일**: 단계·`last_sent`·`clear_since`·이벤트 키가 바뀔 때만 쓴다. 지표 값만 바뀐 것은 쓰지 않고, 처리한 watch 리비전만 바뀐 것은 60초마다 쓴다. 다시 시작하면 늦은 리비전부터 다시 보지만 이벤트 키(`{규칙}/task/{Task id}`, 7일 보관)로 거른다. 파일에 먼저 쓰고 보낸다(재시작 뒤 다시 보내지 않음 — 쓰기 직후 죽으면 한 번 못 보낼 수 있음). 깨진 파일은 `state.json.corrupt`로 옮기고 빈 상태로 시작한다.
- **리더 경보**: 리더가 된 뒤 첫 tick은 `read_barrier`만 한다. `update_status_checked`(읽은 revision)로 상태를 쓴 뒤에만 보내므로 리더가 바뀌어도 다시 보내지 않는다. `BackupJobFailed`는 watch 대신 tick마다 백업 작업 목록의 `last_run`을 보고, 이미 알린 실행을 새 칸 `AlertRuleStatus.notified`(작업 id → 알린 실행의 `finished_at` ms, 비면 쓰지 않음)에 둔다. 규칙을 만들기(UUIDv7 시각) 전에 끝난 실행은 알리지 않는다. 지운 노드의 `firing` `NodeOffline` 경보는 해제로 알린다(§11).
- **상한**: 활성 경보가 500개를 넘으면 설계처럼 경보 하나로 묶는 대신 새 경보를 따라가지 않고 장부에 `overflow`를 두어 `GET /alerts`의 `overflowing`(노드 이름)에 보이고 경고 로그를 한 번 남긴다. 규칙이 없는 묶음 경보는 심각도·대상이 없어 알릴 곳이 없기 때문이다.
- **평가 세부**: VM 지표 규칙은 `running` VM만 본다(멈춘 VM의 지표 경보는 알림 없이 뺀다). `VmFailed`는 이 노드가 주인(`status.node` → `spec.node`)인 모든 VM이다. `HaRecovery`는 `vm-ha-recover` Task가 성공하든 실패하든 끝나면 알린다(결과는 `detail`). `TaskFailed`·`HaRecovery`의 대상 이름은 `{op} {VM 이름 또는 대상 id}`다. 공유 풀은 그 풀이 `ready`인 온라인 노드 가운데 id가 가장 작은 노드가 본다.
- **보내기**: 기록에는 마지막 결과만 남긴다(성공, 또는 다시 하기를 다 쓴 실패 — 문구 끝에 `(after N attempts)`). 꺼진 대상은 조용히 건너뛰고, 없어진 대상은 실패로 기록한다. 분당 상한을 넘은 것은 창(60초)이 끝난 뒤 "N more notifications were held back by the rate limit of {대상}" 알림 하나(심각도는 모은 것 가운데 가장 높은 것, 규칙 없음)로 보낸다.
- **형식**: `json`은 설계의 칸에 `title`·`detail`을 더했다. `slack`은 `{"text": "*제목*\n본문"}`, ntfy 우선순위는 firing·event critical 5·warning 4·그 밖 3(태그 `rotating_light`·`warning`·`information_source`·`white_check_mark`·`test_tube`), Gotify는 8·5·2다. 제목에 헤더로 쓸 수 없는 글자가 있으면 그 헤더만 뺀다.
- **SMTP**: `lettre` 0.11.24를 기본 기능 없이 `smtp-transport`·`builder`·`tokio1-rustls`·`ring`·`rustls-native-certs`(시스템 루트, ldap3의 rustls와 같음)로 쓴다. `builder`가 끌어오는 `quoted_printable`은 0BSD라서 `deny.toml`에 그 크레이트만 예외로 더했다.
- **대상 비밀 지키기**(PUT): SMTP `password`를 빼면 `host`·`port`·`user`가 그대로일 때만 지키고(아니면 400), 빈 글이면 지운다. webhook `url`이 응답의 줄인 URL과 같으면 저장된 URL을 지킨다. `token`을 빼면 URL 출처(scheme·host·port)와 `ca_pem`이 그대로일 때만 지키고, 헤더 값이 빈 글이면 그 이름의 저장된 값을 같은 조건으로 지킨다(뺀 이름은 지운다).
- **API 더함**: 규칙이 쓰는 대상 지우기는 409 `in_use`, 없는 대상을 쓰는 규칙은 400. 대상 이름은 바꾸지 못한다(400). 시험 보내기는 `Operate`(대상은 `account` 종류라 admin만)이고 꺼진 대상에도 보낸다. `/watch`에 `alert-rule`을 더했다(대상은 넣지 않음). `GET /alerts/history`는 `since`·`until`(유닉스 ms나 RFC 3339, 기본 24시간)·`limit`(1~5000, 기본 200)을 받는다.
- **시험 고리**: `[test] alert_tick_ms`(`metrics-it`·`qemu-it`)는 평가 간격을 바꾸고 해제 대기도 같은 비율(12 tick)로 줄인다.
- **Prometheus**: `orbivisor_alerts_firing{severity}`는 이 노드의 `firing` 노드 경보와, 리더면 리더 경보를 센다. 세 심각도를 모두(0 포함) 내고, 장부가 없는 노드(위트니스)는 내지 않는다.

### 스테이지 6 (CLI·opld E2E·CI)

- **CLI 모듈**: 18,000줄인 `cli.rs`에 더하지 않고 `oplctl/src/metrics.rs`(기록 요약·클러스터 요약·`prometheus`)와 `alerts.rs`(`alert`·`notify`)로 나눴다. 두 모듈의 시험은 새 `#[cfg(test)] fake_api.rs`(메서드·경로마다 상태와 JSON, 요청 줄·`If-Match`·본문을 남김)를 쓴다. `time`에 `parsing` 기능을 켰다(`--from`·`--until`의 RFC 3339).
- **stats**: `node stats NODE`(범위 없으면 지난 1시간), `node stats --range …`(NODE 없음)는 설계에 CLI가 없던 클러스터 모으기 요약(클러스터 열의 평균·봉우리, 노드 줄, 상위 VM, 풀)이다. `NODE` 없이 범위도 없으면 지금처럼 마지막 점이다. `--from`·`--until`은 기간(`2h`, 지금에서 뺌)·유닉스 ms·RFC 3339를 받아 ms로 보낸다. `--points`만 줘도 기록 요약이다. `--watch`는 범위와 함께 쓰지 않는다. 표의 근사 표시는 머리 `P50~`·`P95~`다.
- **prometheus**: `enable|disable` 외에 `show`와 별칭 `on|off`를 두었다. 켜면 긁을 URL을 보인다.
- **비밀 입력**: `--password`·`--token`(값 없는 깃발), 값 없는 `--header NAME`, `--url -`(Slack·Teams처럼 URL이 비밀일 때)는 터미널이면 보이지 않게 묻고 아니면 표준 입력에서 한 줄씩 읽는다. `notify target set`은 읽은 보기에서 본문을 만들어 비밀을 보내지 않는다(SMTP `password` 뺌, webhook URL은 줄인 URL 그대로, 헤더는 빈 값). `--no-password`·`--no-token`·`--no-user`·`--no-ca`·`--rm-header`로 지운다.
- **alert rule set**: 읽은 spec에 준 것만 바꾸고, `--gt`·`--lt`만 주면 지금 지표 조건의 문턱만 바꾼다. 리더 규칙은 status가 자주 바뀌므로 `revision_mismatch`면 세 번까지 다시 읽어 쓴다. 노드 대상 경보는 `node/NAME`, 그 밖은 `kind/NAME@NODE`로 보인다.
- **E2E**: `tests/metrics_history.rs`(재시작 뒤 기록, Prometheus 긁기, `qemu-it`면 멈춘 채 옮긴 VM의 기록)와 `tests/alerts.rs`(webhook으로 시험·발생·해제·기록)는 `metrics-it` 또는 `qemu-it` 빌드에서 돈다(설계는 "기능 없이"였지만 `[test]` 고리가 시험 빌드에만 있다). 라이브 마이그레이션 확인은 `migration.rs`의 `metrics_history_follows_a_live_migration`이고 `[test]` 고리 때문에 `qemu-it`도 켜야 한다(GitHub `linux-kvm`은 둘 다 켠다). 이 Mac에서 모두 통과한다(TCG).
- **리더 경보 E2E는 뺐다**: 노드 셋으로는 노드 하나를 끈 채(`node_offline` 발생) 리더를 바꾸면 정족수가 없다. 다섯 노드 시험은 비싸서 opl-api `a_leader_change_does_not_resend_leader_alerts`에 맡긴다.
- **CI**: `check`에 `cargo test -p opld --features metrics-it --test metrics_history --test alerts`(Linux·macOS)를 더했다. `linux-kvm`은 apt `prometheus`(promtool)를 깔고 서비스를 멈춘 뒤 E2E 단계에 `OPL_TEST_PROMTOOL=promtool`을 주어 긁기 시험이 `promtool check metrics`(린트 포함)도 돌린다. 이 Mac에서 promtool 2.53.3으로 VM 줄까지 통과를 확인했다.
- **스테이지 5 시험 고침**: opl-api `sends_are_retried_and_rate_limited`는 창이 500ms라 작업공간 전체 시험의 부하에서 뒤의 Task들이 새 창에 들어 가끔 실패했다. 창을 5초로 늘렸다(시험만, 동작은 그대로).
