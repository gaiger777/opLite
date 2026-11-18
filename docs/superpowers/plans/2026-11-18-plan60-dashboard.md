# Orbivisor 계획 60: 지표 기록·대시보드·알림·Prometheus 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드 메모리 1시간뿐이던 계획 44 지표를 노드 로컬 RRD 파일(5초·1분·30분·6시간, 1년)에 남기고 VM을 따라가게 하며, 범위 조회·백분위·클러스터 요약을 API·CLI·웹 차트(대시보드, 노드 상세, VM 지표 탭)로 보이고, 규칙 경보를 메일·webhook으로 알리고, 노드마다 Prometheus 끝점을 연다.

**Architecture:**
- **기록은 노드 로컬 파일:** `{data_dir}/metrics/{node.rrd, vm/{id}.rrd, pool/{id}.rrd}`, 고정 크기(VM 336 KiB), 시각으로 정한 칸 주소, A/B 머리와 `fdatasync` 순서(전원이 나가도 마지막 주기만 잃음), 시계 뛰기 처리, 형식 버전 옮기기. `HistoryWriter`가 분마다 `MetricsStore`에서 옮기고, 다시 시작하면 `raw5s` 층을 메모리로 되돌린다. Raft에는 쓰지 않는다.
- **기록이 VM을 따라감:** 주인(`status.node` → `spec.node`)이 아닌 노드의 VM 파일을 peer `PushMetricsHistory`로 보내 칸 단위로 합치고, 받은 쪽이 쓴 뒤에만 지운다. 지운 VM·풀 파일은 계획 59 정리 규칙(두 차례, `read_index`, 끝나지 않은 Task 없음).
- **조회는 한 노드, 요약은 모으기:** peer `ReadMetricsHistory`로 노드·VM 주인·풀 준비 노드 하나에 묻고, `/metrics/cluster`는 API 노드가 `Digest`를 32개씩 모아 `cluster_sum`·`merge_top`(10초 캐시). 지난 1시간은 5초 원본의 정확한 p50·p95, 그 밖은 근사.
- **경보는 데이터가 있는 노드에서:** 지표·VM·Task·로컬 풀 규칙은 그 노드가(`{data_dir}/alerts/state.json`), `NodeOffline`·`BackupJobFailed`는 리더가(`AlertRule.status`, 쓴 뒤에만 보냄) 평가한다. 보내기는 경보를 가진 노드가 SMTP(`lettre`)·webhook 다섯 형식으로, 다시 하기·분당 상한·보낸 기록과 함께. peer `ReadAlerts`로 활성 경보와 기록을 모은다.
- **기능 둘:** `metrics-history`(기록 RPC·조회·`prometheus` 설정)와 `notifications`(`AlertRule`·`NotificationTarget`·`ReadAlerts`). 쓰기는 모든 노드가 알릴 때만, 읽기는 옛 노드를 `unsupported`로 보인다.

**Tech Stack:** Rust 2024(MSRV 1.88), 손으로 짠 RRD 파일 형식(`FileExt::write_at`, `File::sync_data`), tonic peer RPC, axum·utoipa, `lettre` 0.11(SMTP), `reqwest`(webhook), Prometheus 텍스트 형식 0.0.4, React·uPlot 1.6.32.

**설계 문서:** `docs/superpowers/specs/2026-11-18-dashboard-metrics-notifications-design.md`
**선행 계획:** 계획 1~59 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 노드·VM·풀의 RRD 파일(노드 로컬, 고정 크기, 분마다 제자리 쓰기), 재시작 되살리기 | 외부 DB(InfluxDB·rrdtool), Raft에 지표 쓰기 |
| 범위 조회(시간~년, `from`·`until`), 점 수 줄이기, 평균·최대·최소, p50·p95 | 디스크·NIC마다 나눈 기록, 게스트 안 지표 |
| VM 기록이 VM을 따라가기(밀어 넘기기), 지운 VM·풀 정리 | 지표 밀어 보내기(Graphite·InfluxDB), 기록 내보내기 |
| 클러스터 대시보드 위젯, 노드 상세, VM "지표" 탭(uPlot) | 3D 보기, 끌어 놓는 VM 만들기, 사용자 정의 대시보드 |
| 경보 규칙(지표 임계값 + 지속 시간, 상태, 이벤트), 반복·해제 알림, 보낸 기록 | 경보 묶기·억제·근무 일정, 개별 확인(ack)·잠시 끄기 |
| 대상: SMTP, webhook(JSON·Slack·Teams·ntfy·Gotify), 시험 보내기, 비밀 가림 | SNMP 트랩, SMS, 스크립트 실행 |
| Prometheus 텍스트 형식 `/api/v1/metrics`(노드마다), 스크레이프·Grafana 예 | OpenMetrics·누적 카운터·원격 쓰기, 내장 Grafana |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 변경과 단위·통합 시험(RRD는 tempfile, 엔진·쓰기는 `ManualClock`), 127.0.0.1 HTTP 수신기와 시험 안 SMTP 서버로 보내기, 노드 300개 가짜 소스의 모으기, opld E2E(`metrics-it`: 재시작 뒤 기록·Prometheus 긁기·webhook 경보, `qemu-it`: VM 줄·옮긴 VM의 기록, `migration-it,qemu-it`: 라이브 마이그레이션 뒤 기록, TCG), Homebrew `promtool` 2.53.3, 웹 시험과 vite preview·Chromium의 눈 확인.
- **배포 서버 svc01(Linux aarch64):** 같은 일반 시험 묶음과 `metrics-it` E2E.
- **GitHub(Ubuntu 24.04 x86_64):** `check`(Linux·macOS)에 `metrics-it` E2E, `linux-kvm`에 apt `prometheus`의 `promtool check metrics`와 `qemu-it`·`migration-it` 변형, `deny`(`lettre`·`uplot` 라이선스).
- **테스트 VM:** 두 노드에 배포한 뒤 기능 `metrics-history`·`notifications`, 기록 파일, 대시보드·경보 화면, Prometheus 끝점을 확인한다.

## 설계 결정

- **Rust RRD 파일:** rrdtool·rrdcached 의존성 없이 층 넷·고정 칸의 작은 형식을 직접 정하고, 칸 목록을 머리에 두어 버전 옮기기를 한 함수로 한다. 크기가 늘지 않고 정리·압축이 없다.
- **밀어 넘기기:** 옛 노드에 계속 묻지 않고 파일을 새 주인으로 옮겨, 조회는 늘 한 노드이고 옛 노드를 지워도 기록이 남는다.
- **경보는 노드마다 + 리더는 노드 상태만:** 지표가 있는 자리에서 평가해 리더 짐이 노드 수와 상관없고, 노드 경보 상태를 Raft에 쓰지 않아 폭주 때 쓰기가 리더로 몰리지 않는다.
- **리소스 둘(규칙·대상):** 따로 RBAC·감사·watch 하고, 대상의 비밀은 admin 전용 계정 종류에 가두며 응답·`Debug`·기록·오류 문구에서 뺀다.
- **Prometheus는 지금 토큰 + viewer, 마지막 5초 비율 gauge:** 새 비밀 종류와 화면이 없고, 끝점은 설정으로 꺼 둔다.

## Task

각 Task(스테이지)는 하나의 커밋이며, 앞 Task의 커밋 위에서 순서대로 한다. 단계마다 `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 바뀐 크레이트의 시험을 통과한 뒤 커밋한다. 앞 스테이지의 새 RPC는 `Unimplemented`이고 API는 그 스테이지에서 연다.

- [x] **Task 1: 지표 기록·경보·알림 타입과 기능, 순수 계산(opl-types)** — `MetricField`·`Tier`·`HistoryRange`·`HistoryQuery`(`validate`)·`HistoryScope`(`Digest` 포함)·`HistoryReply`(`top` 더함)와 `NodeSample`·`VmSample`의 `values()`, `metrics_math`의 `choose_tier`(60초 여유)·`bucket_window`·`consolidate`(xff 0.5)·`downsample`·`stats`·`percentiles`·`cluster_sum`(코어 가중)·`merge_top`, `alert.rs`의 규칙·조건·심각도·순수 상태 기계 `advance`·대상(`target` 칸의 SMTP·webhook, 비밀 가린 `Debug`, `redact_url`)·`NotificationRecord`, `Kind::AlertRule`·`NotificationTarget`, `SettingsSpec.prometheus`, 기능 `metrics-history`·`notifications`, serde 하위 호환 시험.
- [x] **Task 2: RRD 파일 형식(opld `metrics/rrd.rs`)** — 머리 4 KiB(512 B A/B 사본, seq·SHA-256), 층 넷의 칸 주소와 `[AVG][MAX][MIN]` 배치, `append`·`close`·`sync_data`·`commit` 쓰기 순서, 큰 공백 앞에 거친 층 먼저 닫기, 시계가 1시간 넘게 뒤로 가면 되돌리기와 `Behind`, 칸 목록이 다르면 옮기기·더 새 버전과 깨진 파일은 옆으로(`.corrupt`·`.v{n}`, 지우지 않음), 칸 단위 `merge`, 단계마다 끊는 "죽음"과 고정 크기(VM 336·노드 376·풀 92 KiB) 시험.
- [x] **Task 3: 분마다 기록·재시작 되살리기·밀어 넘기기·정리, 기록 RPC(opld·opl-peer)** — `History`(파일 핸들·조회·받기, 기록을 끄면 메모리 1시간으로 답함)와 `HistoryWriter`(주기 경계 + 1초, 멈출 때 한 번 더), `MetricsStore`의 `take_pending`, `PoolLevels`로 풀 점, `raw5s` 되살리기와 메모리로 빈 칸 메우기(정확한 백분위), `Digest`(주인 VM만), 10분·변경 30초 뒤 밀어 넘기기와 정리(계획 59 규칙), 4096 핸들 LRU, peer `ReadMetricsHistory`·`PushMetricsHistory`와 `HistoryError`, `opl_types::clock`, 설정 `[metrics] history`·`flush_secs`와 `[test] metrics_flush_secs`·`metrics_push_secs`(새 기능 `metrics-it`).
- [x] **Task 4: 기록 조회·클러스터 모으기·Prometheus(opl-api)** — `GET /nodes|vms|storage-pools/{key}/metrics/history`(`range` 또는 `from`·`until`, `points`, 묻는 노드 고르기와 409·503 짝), `GET /metrics/cluster`(`ClusterHistoryResponse`, 층이 다른 노드는 `skewed`, 노드 줄 20개, 상위 VM 10개, Raft 풀 값, 10초 캐시와 한 번에 하나 모으기, 300노드 시험), `GET /metrics` Prometheus(401 → 403 → 404, 5초 캐시, 이름·레이블 규칙, `orbivisor_vm_info`), `PATCH /settings {prometheus}`(기능 게이트·감사), 엄격한 텍스트 형식 파서 `tests/common/promtext.rs`, OpenAPI와 `schema.d.ts`.
- [x] **Task 5: 경보 엔진·알림 대상·보내기·보낸 기록(opl-api·opl-peer·opl-types·opld)** — `alerts/{mod, book, engine, send}.rs`, `AlertBook`(노드 `state.json`·`history.log`), 노드·리더 평가(리더는 `read_barrier` 뒤 상태를 쓴 다음에만 보냄, `BackupJobFailed`의 `notified`), 이벤트 키 7일, 유지보수, 활성 500개 상한의 `overflowing`, SMTP(`lettre` 0.11.24, `quoted_printable` 0BSD 예외)와 webhook 다섯 형식, 4개 동시·10초·30초/2분/10분 다시 하기·분당 20개와 "N more"·대기열 1000, 시험 보내기, 대상 비밀 지키기(PUT), RBAC(대상은 admin 전용), `/watch`의 `alert-rule`, peer `ReadAlerts`, `GET /alerts`·`/alerts/history`, `orbivisor_alerts_firing`, `[test] alert_tick_ms`.
- [x] **Task 6: stats `--range`·클러스터 요약·alert·notify·prometheus 명령, opld E2E와 CI(oplctl·opld·CI)** — `oplctl/src/metrics.rs`(기록 요약 표 `P50~`·`P95~`, `covered_from` 안내, 클러스터 요약, `prometheus show|enable|disable`)와 `alerts.rs`(`alert list|history|rule …`, `notify target …`, 비밀은 묻거나 표준 입력, `set`은 비밀을 보내지 않음, `revision_mismatch` 세 번), 시험용 `fake_api.rs`, opld `tests/metrics_history.rs`·`tests/alerts.rs`(`metrics-it`·`qemu-it`)와 `migration.rs`의 라이브 마이그레이션 기록 시험, CI `check`의 `metrics-it` 단계와 `linux-kvm`의 `promtool`.
- [x] **Task 7: uPlot 차트·범위 고르기, 클러스터 대시보드 위젯, 노드 상세, VM 지표 탭, 경보·알림 대상 화면, Prometheus 설정(web)** — 계획 61 지침대로 사이드바 항목 없이 `/alerts?tab=`, `/settings/notifications`, `/nodes/:name`, VM `?tab=metrics`, 설정의 Prometheus 칸과 들어가는 링크, `web/src/charts/`(지연 로딩 `TimeChart`, `RangePicker`, `MetricChart`의 범례 표·근사 표시·"표로 보기"), 대시보드 위젯 다섯, 색 토큰(`--chart-1..5`, `--success|warning|danger|info`), 경보 화면 세 탭과 규칙 폼, 비밀을 지키는 대상 폼, en·ko, 권한, 시험(가짜 `TimeChart`), 번들 +17 kB gzip.
- [x] **Task 8: 가이드·README·계획 문서** — 가이드 "클러스터 운영"의 "지표"를 기록 파일·충돌 안전·시계·마이그레이션·설정과 SD 카드·조회 API와 CLI·정확/근사 백분위·클러스터 요약·웹 화면으로 넓히고, 새 절 "알림"(규칙·조건·평가 자리·상태 기계·대상과 webhook 형식·시험 보내기·비밀·상한과 다시 하기·보낸 기록·CLI·웹·기능·오류)과 "Prometheus"(켜기·인증·이름과 레이블·`scrape_configs`·Grafana 쿼리·기능)를 더함, README 상태·기능 표, `docs/index.md`, 개발 시험 명령, 설계 문서 스테이지 보고, 이 계획 문서와 실행 기록.

## 완료 기준 (계획 60)

- orbivisord를 SIGKILL로 끝내고 다시 띄워도 `node stats NODE --range hour`에 재시작 전 점이 있고 멈춘 동안은 `null`이며, 하루·주·년 범위가 `min1`·`min30`·`hour6` 층으로 답한다(opld `metrics-it` E2E).
- 전원이 끊긴 쓰기(데이터만 쓰고 머리 못 씀), 머리 사본 하나 깨짐, 시계 앞·뒤 뛰기, 버전 옮기기, 더 새 형식에서 기록 파일이 마지막 주기 넘게 잃지 않거나 옆으로 옮기고 새로 시작한다(RRD 시험).
- VM을 꺼진 채 또는 라이브로 다른 노드로 옮기면 새 노드의 `vm/{id}.rrd`에 옛 노드 시절 칸이 있고 옛 파일은 없어진다(`qemu-it`, `migration-it`).
- `GET /metrics/cluster`가 노드 300개 가짜 소스에서 동시성 상한과 응답 크기를 지키고, 답하지 않은·옛·시계가 어긋난 노드를 목록으로 보인다(API 시험).
- webhook 대상과 `node cpu > -1` 규칙이 `firing`을, 문턱을 바꾸면 해제 대기 뒤 `resolved`를 보내고 `alert history`에 둘 다 있다. 리더가 바뀌어도 리더 경보를 다시 보내지 않고, 노드 경보는 재시작 뒤 다시 보내지 않는다(E2E, API 시험).
- 다섯 webhook 형식과 SMTP 본문, 다시 하기·분당 상한이 맞고, 응답·`Debug`·기록·오류 문구에 비밀이 없으며 viewer는 알림 대상을 읽지 못한다(API 시험).
- `prometheus enable` 뒤 viewer 토큰으로 받은 본문이 엄격한 파서와 `promtool check metrics`를 통과하고, 꺼져 있으면 404, 토큰이 없으면 401이다.
- 웹에서 대시보드 차트·위젯, 노드 상세, VM 지표 탭, 경보 세 탭, 알림 대상(시험 보내기), Prometheus 칸을 밝음·어두움 테마로 쓸 수 있다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 시험, `cargo deny`가 이 Mac과 svc01에서, linux-kvm(promtool 포함)이 GitHub에서 통과한다.

## 알고 받아들인 제한

- 마이그레이션 직후 밀어 넘기기 전(최대 약 10분)에는 VM 기록 조회의 앞쪽이 비어 보인다(`covered_from`이 알리고, 1시간은 계획 44 `/vms/{key}/metrics`로 보임).
- 죽은 노드의 VM 기록은 그 노드가 돌아와야 새 주인에게 넘어가고, 돌아오지 않으면 잃는다. 지운 노드의 노드 기록은 함께 사라진다.
- orbivisord를 내려 깔면 더 새 형식의 기록 파일을 옆으로 옮기고 새로 시작한다(기록을 잃음).
- 1시간보다 긴 범위의 p50·p95는 층의 평균으로 셈한 근사라 짧은 봉우리를 낮게 본다. 클러스터 요약은 평균 열만 있어 최대·최소가 그 평균 열의 것이다.
- 공유 풀은 노드마다 같은 기록을 따로 두고, 노드 로컬 풀의 기록 조회는 노드를 고를 수 없다.
- 시계가 크게 어긋난 노드는 클러스터 요약에서 빠진다(`skewed`, 다시 맞추지 않음).
- 노드 경보 상태는 파일에 먼저 쓰고 보내므로 그 사이에 죽으면 알림 하나를 못 보낼 수 있다. 활성 경보가 500개를 넘은 노드는 새 경보를 따라가지 않고 `overflowing`으로만 알린다.
- 업그레이드 중 옛 노드의 지표 경보는 없다. 꺼진 노드의 노드 경보는 보이지 않고 `node-offline`이 대신한다.
- 리더 경보의 리더 바뀜 E2E는 노드 셋으로는 정족수를 잃어 opld E2E에서 빼고 opl-api 시험에 맡겼다.
- SD 카드·USB 부팅 노드는 기본 1분 쓰기가 닳음을 늘린다(`flush_secs = 600`이나 `history = false`를 안내).
- Prometheus 값은 마지막 5초 비율 gauge이고 누적 카운터·OpenMetrics가 없으며, 노드마다 따로 긁어야 한다.
- 웹: 1시간 범위는 5초마다 300점 응답 하나를 다시 읽는다(이어 붙이기 없음). VM IOPS는 차트에 없다. 노드 상세 머리에 유지보수·비우기·레이블·지우기가 아직 없다(계획 61). 머리줄 경보 배지·종 서랍이 없다(계획 61).

## 실행 후 기록 (2026-11-18)

- 8개 스테이지를 단계 커밋으로 구현했다(`782dcec` 타입 … `06a432b` 웹, 이 문서 커밋). 설계와 다른 것·결정은 설계 문서 §17에 스테이지마다 남겼다.
  - 스테이지 5의 `sends_are_retried_and_rate_limited` 시험이 작업공간 전체 부하에서 가끔 실패해 스테이지 6에서 창을 500ms → 5초로 늘렸다(시험만).
  - 스테이지 6 E2E는 `[test]` 고리 때문에 설계의 "기능 없이" 대신 `metrics-it`(QEMU 없음)·`qemu-it` 빌드에서 돈다.
- 이 Mac에서 `cargo test --workspace` 3204개 통과(리뷰 2 수정 뒤), `metrics-it` 실제 시험(기록 되살리기·Prometheus·경보 웹훅) 통과, 웹 시험 856개 통과.
- `cargo deny` 통과: `lettre`의 `builder`가 끌어오는 `quoted_printable`(0BSD)만 `deny.toml`에 예외로 더했다. `uplot`은 MIT.
- 이 Mac에서 `metrics-it`·`qemu-it` E2E와 `migration-it,qemu-it`의 라이브 마이그레이션 기록 시험(TCG), Homebrew promtool 2.53.3의 `check metrics`(VM 줄 포함)가 통과했다.
- 리뷰 1(중대 0·중요 4·경미 9, 모두 고침): 보내기 큐가 허가를 잡은 뒤에만 받고 대상마다 한 건씩, 대시보드 모으기를 키별로 공유하고 잠금 밖에서 묻기, 기록 훑기·밀기를 쓰기와 떼기, SMTP TLS·웹훅 URL이 바뀌면 비밀 다시 묻기, 비관리자 이력에서 대상·오류 가림, 경보 500개 상한, 목록을 못 읽으면 경보 지키기, `Invalid` 밀기 물러서기와 옆 파일 30일 정리, 경보 상태 파일을 못 열어도 시작, 꺼 둔 규칙의 백업 실패 다시 보내지 않기.
- 리뷰 2(중대 0·중요 2·경미 8, 모두 고침, 리뷰 1 수정은 옳다고 확인): 기록 끈 노드로 밀기 물러서기(파일은 지킴), 15분 넘은 알림 버리기와 대상별 20건 상한, 대상 돌아가며 보내기, 풀린 경보의 발생 재시도 버리기, 열기 잠금 밖에서 파일 만들기, 디스크 오류 경고 묶기, 밀기 순서를 주인 노드별로 섞기, CLI의 부호 붙은 기간 거절, Prometheus 캐시 몰림 막기.
- svc01 일반 시험: 첫 회는 시험 3280개 통과였으나 clippy가 svc01의 Rust 1.99에서만 나는 두 경고(`fetch_update` 사용 중단, `chunks_exact_to_as_chunks`)에 걸렸다. 옛·새 툴체인 모두에서 깨끗한 꼴로 고친 뒤 둘째 회에서 fmt·clippy·시험 3289개·starve·규모 100·저장소 시험 모두 통과.
- GitHub 탐침: 세 번 모두 linux-kvm(KVM 마이그레이션의 기록 따라가기, promtool 2.45 검사 포함), install-media, ceph, ldap, package, iso-e2e(bios·uefi), catalog-links 통과.
- **테스트 VM 배포(svc01·svc02):** svc02 → svc01 순서로 바이너리만 바꿨다(유닛은 같아 그대로). 두 노드 모두 sha256이 빌드와 같고, 헬스 `ok`, 두 voter 모두 online, 기능에 `metrics-history`·`notifications`가 있다. 첫 쓰기 뒤 `/var/lib/opl/metrics`에 `node.rrd`(376 KiB)와 svc02의 test-vm 파일(336 KiB, 0600)이 생겼다. `node stats svc02 --range hour`(svc01에서 다른 노드 기록 읽기), `vm stats test-vm --range hour`(p50·p95 포함), 클러스터 모으기(`node stats --range hour`, 2노드 합계)가 모두 답하고, 업그레이드 앞의 시점은 "no history before"로 알린다. `alert list`는 빈 목록, Prometheus는 기본 꺼짐(`/api/v1/metrics` 404). 웹 `/`·`/alerts`·`/settings/notifications`가 새 빌드(index-DPeApXwd.js)를 돌려준다. svc02의 test-vm은 QEMU PID 916 그대로 실행 중이고 저널에 경고·오류는 없다. CLI의 `--range`는 `hour|day|week|month|year`만 받는다(`1h`는 `--from 1h`).

## 다음 계획으로 넘길 사항

- **계획 61: UI/UX 전면 정리**(최소 사이드바) — 경보·알림 대상·노드 상세가 링크로만 들어가는 지금 배치를 새 탐색에 넣기, 머리줄 경보 배지와 종 서랍(S6), 노드 상세 머리에 유지보수·비우기·레이블·지우기를 모으기(`ResourceHeader`, S5), 대시보드 다시 놓기(S7).
- 긴 범위의 정확한 p95: 층마다 히스토그램(또는 스케치)을 두어 근사를 줄이기, 클러스터 요약의 최대·최소 열.
- 경보 잠시 끄기(silence)·개별 확인(ack) API와 화면, 경보 묶기·억제(노드 경보가 그 VM 경보를 가림)·근무 일정.
- Prometheus 누적 카운터·OpenMetrics, 서비스 찾기(노드 목록 HTTP SD), Graphite·InfluxDB 밀어 보내기.
- 디스크·NIC마다 나눈 기록, 게스트 안 지표(게스트 에이전트 `guest-get-fsinfo` 사용량), 기록 내보내기(CSV), 사용자 정의 대시보드, 측정 간격 설정.
- 지운 노드의 기록 옮기기, 죽은 채 돌아오지 않는 노드의 VM 기록, 노드 로컬 풀 기록의 노드 고르기, 1시간 차트의 `since` 이어 붙이기.
- SNMP 트랩·SMS·스크립트 대상, HTML 메일, 리더 경보 리더 바뀜 E2E(노드 다섯).
- 격차 분석 다음 항목(계획 62~64 SDN …).
