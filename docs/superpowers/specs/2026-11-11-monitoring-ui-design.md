# Orbivisor 실시간 지표와 쉬운 화면 설계 (하위 프로젝트 14: 계획 44~47)

작성일: 2026-11-11
상위 문서: `2026-10-22-web-ui-design.md`(웹 UI), `2026-11-06-enterprise-design.md`(감사 로그 — 노드마다 읽어 모으기 `ReadAudit`), `2026-11-09-node-maintenance-design.md`(노드 기능 목록 `features`)

## 1. 목표와 범위

지금 Orbivisor에는 사용량 지표가 하나도 없다. 노드 목록의 CPU·메모리는 등록 때 적은 용량이고, 대시보드의 "사용량"은 VM 스펙의 vCPU·메모리 합이다. 사용자가 바란 것(2026-11-10):

- 대시보드에서 노드별 CPU·메모리·디스크를 실시간 차트로 보기
- VM별로도 같은 차트 보기
- 가상 머신 목록을 화려한 3D로 보기
- 초보자가 마우스로 그림을 맞추듯 가상 머신을 만들기

계획 넷으로 나눈다.

| 계획 | 내용 |
|---|---|
| 44 (A) | 지표 수집: 노드마다 5초마다 노드·VM 지표를 재고 메모리에 1시간 둠, 피어 RPC로 다른 노드의 지표 읽기, REST API, 노드 기능 `metrics`, oplctl `node stats`·`vm stats` |
| 45 (B) | 대시보드·노드·VM 차트(uPlot): 대시보드에 노드별 작은 차트, 새 노드 상세 화면, VM 상세의 "지표" 탭, 5초마다 이어 받기 |
| 46 (C) | 3D 클러스터 보기(three.js·react-three-fiber): 노드는 받침, VM은 그 위의 블록, 색은 상태와 사용률, 표 보기와 전환, WebGL이 없으면 표 |
| 47 (D) | 끌어 놓는 VM 만들기: 부품 팔레트(CPU·메모리·디스크·NIC·ISO)를 VM 그림에 끌어 놓기, 크기 슬라이더, 작게·보통·크게 견본, 노드에 들어가는지 바로 보이기, 키보드로도, 지금 양식은 그대로 |

이 문서는 계획 44를 자세히, 45~47은 화면과 경계까지 정한다. 45~47은 계획을 쓸 때 이 문서의 해당 절을 고친다.

넣지 않는 것:

- 오래 두는 지표(1시간 넘게, 디스크에 쓰기, RRD·시계열 DB)
- Prometheus 내보내기·외부 InfluxDB/Graphite 보내기(Proxmox의 "metric server")
- 경보·임계값 알림
- 게스트 안의 지표(게스트 에이전트, 풍선 메모리)
- 디스크·NIC마다 나눈 차트(VM 합계만)
- 풀별 사용량 차트(풀 목록의 용량은 지금 그대로)

필요가 보이면 하위 프로젝트 14 다음의 "빠진 기능" 단계에서 다룬다.

### 견준 길

- **지표 보관: 노드마다 메모리 링 버퍼(채택) vs Raft에 쓰기 vs 파일(RRD).**
  - 지표는 5초마다 노드·VM 수만큼 생긴다. Raft에 쓰면 모든 노드가 복제하고 스냅샷이 커진다.
  - 파일은 재시작 뒤에도 남지만 형식·회전·디스크 쓰기가 생긴다. 사용자가 바란 것은 "실시간"이다.
  - 메모리 1시간이면 노드 하나에 VM 100개여도 수 MB다(§3.4). 재시작하면 그 노드의 기록이 비는 것은 받아들인다.
- **다른 노드의 지표: 읽을 때 피어 RPC로 모음(채택) vs 리더가 모아 둠 vs 노드가 서로 밀어 보냄.**
  - 감사 로그(`ReadAudit`)가 이미 읽을 때 모으는 방식이고 같은 틀(`join_all`, `unreachable` 목록)을 쓸 수 있다.
  - 리더에 모으면 리더가 바뀔 때 기록이 비고 리더에 짐이 몰린다.
- **웹으로 보내기: `since`를 단 5초 폴링(채택) vs WebSocket 스트림 vs SSE.**
  - 폴링은 지금의 openapi-fetch·TanStack Query·권한 검사를 그대로 쓰고, 어느 노드에 붙어도 모으기가 같다. `since`를 주면 새 점만 오므로 5초에 노드 하나당 점 하나다.
  - WebSocket은 노드 사이 구독·끊김 복구·모으기를 새로 만들어야 하는데 얻는 것은 지연 몇 초다.
  - 지금의 `/watch` WebSocket은 리소스 변경용이고 그대로 둔다.
- **호스트 값 읽기: 직접 읽기 — Linux `/proc`·`/sys`, macOS `libc`(채택) vs `sysinfo` 크레이트.**
  - 필요한 값이 적다: CPU 시간, 메모리, 디스크 I/O 합, NIC 바이트, 프로세스 CPU·RSS.
  - Linux 파서는 텍스트를 받는 순수 함수라 고정 문자열로 시험할 수 있다.
  - `sysinfo`는 의존성이 크고(Windows용 포함), 디스크 I/O를 장치마다 따로 거른다(파티션 중복).
  - macOS 호출(`host_statistics64`·`host_processor_info`·`proc_pidinfo`·`getifaddrs`)은 libc 0.2.191에 있다.
- **VM 디스크 I/O: QMP `query-blockstats`(채택) vs `/proc/<pid>/io`.**
  - `/proc/<pid>/io`는 QEMU 전체(로그·UEFI 변수 포함)이고 macOS에 없다.
  - `query-blockstats`는 게스트가 본 디스크 장치마다의 바이트·횟수이고 두 플랫폼에서 같다.
- **VM 네트워크: Linux tap 장치 카운터(채택) vs QMP.**
  - QMP에는 NIC 바이트 카운터가 없다(`query-rx-filter`는 필터뿐).
  - tap(`op<uuid 끝 10자>n<i>`)의 `/sys/class/net/<tap>/statistics/{rx,tx}_bytes`는 호스트 쪽 방향이라, 게스트 기준으로 받기·보내기를 바꿔 적는다.
  - macOS는 사용자 모드 NIC라 tap이 없어 값이 없다(`null`).

## 2. 지금 있는 것 (계획 43 끝)

- `NodeStatus { online, cpus, memory_mib, … , features }`(`opl-types/src/node.rs`)는 등록 때(`opld/src/bootstrap.rs` `record_node`) 적는 용량이다.
  - `cpus`는 `available_parallelism`에서 온다.
  - `memory_mib`는 `opld/src/host.rs` `memory_mib`에서 온다(Linux `/proc/meminfo` `MemTotal`, macOS `sysctl hw.memsize`).
- QEMU: `QemuHypervisor`(`opl-driver/src/qemu.rs`)가 `VmHandle { qmp, pid, paths, … }`를 둔다. `Hypervisor` 트레이트(`hypervisor.rs`)에 지표 메서드는 없다.
  - 가짜는 `MockHypervisor`(`mock.rs`)다.
  - QMP `execute`는 일반 메서드라 새 명령을 감싸기 쉽다.
  - 디스크 장치는 `disk{i}-dev`(블록 노드 `disk{i}`)다.
- 피어 RPC: `.proto` 없이 `opl-peer/build.rs`의 `json(...)`으로 serde 타입을 보낸다.
  - 노드 사이 읽기의 본은 `read_audit`다: `AuditSource` 트레이트, `PeerService::with_audit`, `Peers::read_audit(addr, query, timeout)`.
  - API 모으기는 `opl-api/src/audit.rs`가 한다: 위트니스를 빼고, 내 노드는 직접, 남은 노드는 RPC, `join_all`, `unreachable`.
  - 서버는 클라이언트 인증서의 노드가 클러스터 구성원인지 본다.
- API: axum + `utoipa_axum`(`opl-api/src/lib.rs` `routes!`)이다.
  - 권한은 `Principal`과 `state.authorize(&principal, Action, Kind)`로 본다. Viewer는 계정이 아닌 종류를 모두 읽는다.
  - 웹 타입은 `pnpm gen:api`로 `web/src/api/schema.d.ts`에 만든다.
- 백그라운드 루프: `{ shutdown: oneshot::Sender, task: JoinHandle }` + `spawn` + `async fn shutdown`(본: `opld/src/pools.rs` `PoolMounter`)이고, `member.rs` `Services`가 모아 끈다.
- 노드 기능: `FEATURES`(`node-drain`·`incremental-backup`·`incremental-replication`·`storage-migration`)와 `cluster::require_feature`가 있다.
- 웹: React 19, react-router 7, TanStack Query 5, shadcn·Tailwind 4, i18next(en·ko), vitest + testing-library + msw.
  - 대시보드는 `web/src/pages/DashboardPage.tsx`, 노드는 목록만(`web/src/nodes/NodesPage.tsx`), VM 상세는 `web/src/vms/VmDetailPage.tsx`다.
  - 차트 라이브러리는 없다.

## 3. 설계 — 계획 44 지표 수집

### 3.1 재는 값

모든 비율은 직전 측정과의 차이를 걸린 시간으로 나눈다. 첫 측정(재시작, VM 시작·붙기, pid 바뀜, 카운터가 줄어듦)은 기준만 잡고 점을 만들지 않는다. 그래서 VM의 첫 점은 시작 뒤 약 5초에 생긴다.

노드 점 `NodeSample`:

| 필드 | 뜻 | Linux | macOS |
|---|---|---|---|
| `t` | 측정 시각(유닉스 ms) | | |
| `cpu` | CPU 사용률 %(모든 코어 합 100) | `/proc/stat` 첫 줄의 (전체 − idle − iowait) / 전체 | `host_processor_info(PROCESSOR_CPU_LOAD_INFO)` 합 |
| `mem_used_mib`, `mem_total_mib` | 쓰는 메모리와 전체 | `/proc/meminfo` `MemTotal − MemAvailable` | `host_statistics64(HOST_VM_INFO64)`(active + wired + compressor) × 페이지 크기, 전체는 `hw.memsize` |
| `disk_read_bps`, `disk_write_bps` | 디스크 읽기·쓰기 바이트/초 | `/proc/diskstats`에서 실제 디스크만 합(§3.2) | `null` |
| `net_rx_bps`, `net_tx_bps` | 실제 NIC 받기·보내기 바이트/초 | `/sys/class/net/<if>/statistics`, `device` 링크가 있는 장치만 | `getifaddrs`의 `if_data`, 이름이 `en`으로 시작하는 장치 |
| `fs_used_mib`, `fs_total_mib` | 데이터 디렉터리(설정 `data_dir`, 기본 `/var/lib/opl`)의 파일 시스템 | `statvfs` | `statvfs` |

VM 점 `VmSample`:

| 필드 | 뜻 | 출처 |
|---|---|---|
| `t` | 측정 시각 | |
| `cpu` | VM의 vCPU 대비 사용률 %(vCPU 모두 바쁘면 100) | QEMU 프로세스 CPU 시간 ÷ (경과 × vCPU 수). Linux `/proc/<pid>/stat` utime+stime(÷ `sysconf(_SC_CLK_TCK)`), macOS `proc_pidinfo(PROC_PIDTASKINFO)` `pti_total_user + pti_total_system`(ns) |
| `mem_rss_mib` | QEMU 프로세스 RSS | Linux `/proc/<pid>/status` `VmRSS`, macOS `pti_resident_size` |
| `disk_read_bps`, `disk_write_bps`, `disk_read_iops`, `disk_write_iops` | 모든 디스크 합 | QMP `query-blockstats`의 `disk{i}-dev`(또는 노드 `disk{i}`) `rd_bytes`·`wr_bytes`·`rd_operations`·`wr_operations` 합 |
| `net_rx_bps`, `net_tx_bps` | 게스트 기준 받기·보내기 | Linux tap `tx_bytes`(게스트 받기)·`rx_bytes`(게스트 보내기) 합. tap이 없으면 `null` |

- `cpu`는 0~100으로 자르지 않는다. 에뮬레이션 스레드 때문에 100을 조금 넘을 수 있고, 그대로 보여 준다.
- 값을 못 읽은 필드는 `null`이다(Option). 예: macOS 디스크, tap 없음, QMP 시간 초과.
- QMP가 1초 안에 답하지 않으면 그 VM의 디스크 필드만 `null`이고 나머지는 적는다. 실패 경고는 VM마다 10분에 한 번 남긴다.

### 3.2 "실제 디스크" 고르기 (Linux)

- `/proc/diskstats`의 장치 가운데 `/sys/block/<name>`이 있는 것(파티션 제외)만 쓴다.
- 그중 `loop*`·`ram*`·`zram*`·`dm-*`·`md*`·`nbd*`·`zd*`(ZFS zvol)·`rbd*`는 뺀다. 그래야 LVM·RAID·ZFS 위의 쓰기가 아래 디스크와 두 번 세이지 않는다.
- 남은 장치의 읽은 섹터(필드 3)·쓴 섹터(필드 7) × 512를 합한다.
- 그래서 노드 디스크 I/O는 "물리 디스크에 실제로 간 바이트"다. iSCSI 디스크(`sd*`)는 들어가고 NFS·RBD는 들어가지 않는다(네트워크로 셈).

### 3.3 구성

- **타입(`opl-types/src/metrics.rs`):**
  - `NodeSample`, `VmSample`
  - `MetricsQuery { scope: MetricsScope, since: Option<i64> }`, `MetricsScope = Nodes | Vm { id: Uuid }`
  - 응답 `NodeSeries { node: Uuid, samples: Vec<NodeSample> }`, `VmSeries { vm: Uuid, node: Uuid, samples }`
  - 모두 `serde`와 `cfg_attr(feature = "openapi", ToSchema)`이다.
- **호스트 읽기(`opld/src/metrics/host.rs`):**
  - 트레이트 `HostProbe`
    - `cpu_times() -> Option<CpuTimes{busy, total}>`
    - `memory() -> Option<(used_mib, total_mib)>`
    - `disk_bytes() -> Option<(read, written)>`
    - `nic_bytes() -> Option<(rx, tx)>`
    - `process(pid) -> Option<ProcStat{cpu_ns, rss_mib}>`
    - `tap_bytes(name) -> Option<(rx, tx)>`
    - `fs(path) -> Option<(used_mib, total_mib)>`
  - 구현은 `LinuxProbe`(`#[cfg(target_os = "linux")]`), `MacProbe`(`#[cfg(target_os = "macos")]`), 그 밖의 OS는 모두 `None`이다.
  - Linux 파서(`parse_proc_stat`, `parse_meminfo`, `parse_diskstats`, `parse_pid_stat`, `parse_status_rss`)는 `&str`을 받는 순수 함수다.
- **QMP:**
  - `QmpClient::query_blockstats() -> Vec<BlockStats{device, node_name, rd_bytes, wr_bytes, rd_operations, wr_operations}>`
  - `Hypervisor` 트레이트에 `async fn disk_stats(&self, id) -> Result<DiskCounters>`(모든 `disk{i}` 합)를 더한다.
  - `MockHypervisor`는 시험이 정한 카운터를 돌려준다.
- **측정기(`opld/src/metrics/sampler.rs`):**
  - `MetricsSampler::spawn(hv, probe, cluster, node_id, data_dir, store)`. 틀은 `PoolMounter`와 같다(`interval` 5초, `MissedTickBehavior::Skip`, `select!` 끄기).
  - 한 번 잴 때:
    1. 노드 값을 잰다.
    2. `hv.list()`의 실행 중 VM마다 pid로 `probe.process`, `hv.disk_stats`(1초 제한), tap 이름(`tap_name(vm, i)`, 클러스터의 VM 스펙 NIC 수)으로 `probe.tap_bytes`를 잰다.
    3. VM마다 직전 카운터(`HashMap<Uuid, VmCounters{pid, …}>`)와 비교해 점을 만든다.
  - VM 측정은 `join_all`로 동시에 하되 한 번의 측정 전체를 4초로 제한한다. 넘으면 그 VM은 이번 점이 없다.
  - 사라진 VM의 카운터는 지운다.
- **저장소(`opld/src/metrics/store.rs`):**
  - `MetricsStore`(`Mutex` 안에 노드 `VecDeque<NodeSample>`, VM마다 `HashMap<Uuid, VecDeque<VmSample>>`)
  - 시리즈마다 최대 720점(5초 × 1시간)을 두고, 넘으면 앞을 버린다.
  - 마지막 점이 1시간보다 오래된 VM 시리즈는 측정 때 지운다(마이그레이션해 나간 VM, 지운 VM).
  - `read(&MetricsQuery) -> MetricsLocal`: `since`보다 큰 `t`만 돌려준다.
- **피어 RPC:**
  - `build.rs`에 `json("read_metrics", "ReadMetrics", "opl_types::metrics::MetricsQuery", "crate::metrics::MetricsReply")`
  - `opl-peer/src/metrics.rs`에 `MetricsReply { node: Option<NodeSeries>, vms: Vec<VmSeries> }`와 `trait MetricsSource { fn read(&self, q: &MetricsQuery) -> MetricsReply }`
  - `PeerService::with_metrics`, `Peers::read_metrics(addr, query, timeout)`
  - 서버는 `authorize` 뒤 소스가 없으면 `unimplemented`다.
- **API(`opl-api/src/metrics.rs`):** `ApiState`에 `metrics: Option<Arc<dyn MetricsSource>>`를 둔다.
  - `GET /metrics/nodes?since=`
    - 응답: `{ nodes: [NodeSeries], unreachable: [노드 이름], unsupported: [노드 이름] }`
    - 권한: `Read` `Kind::Node`
  - `GET /nodes/{key}/metrics?since=`
    - 그 노드 하나다. 응답은 `NodeSeries`다.
    - 닿지 않으면 503 `node_unreachable`, 옛 노드면 409 `cluster_upgrading`이다.
  - `GET /vms/{key}/metrics?since=`
    - 응답: `{ vm, samples, unreachable, unsupported }`
    - 모든 노드(위트니스 제외)에 `Vm { id }`로 묻고 시각 순으로 합친다. 그래서 마이그레이션 앞뒤 기록이 이어진다.
    - 같은 `t`가 두 노드에서 오면(시계가 같은 경우) 둘 다 둔다.
    - 권한: `Read` `Kind::Vm`
  - 모으기는 감사 로그와 같다: 내 노드는 직접 읽고, 남은 노드는 RPC(제한 3초)다. `Unimplemented`는 `unsupported`, 그 밖의 오류는 `unreachable`이다.
  - `since`는 그 노드 시계 기준의 `t`다. 웹은 받은 마지막 `t`를 노드마다 기억해 보낸다(§3.6).
- **노드 기능:** `FEATURES`에 `metrics`를 더한다. API는 막지 않고 옛 노드를 `unsupported`로 보인다(웹이 "이 노드는 지표를 지원하지 않습니다"로 보임).
- **oplctl:**
  - `orbivisorctl node stats`: 노드마다 마지막 점을 표로 보인다(CPU %, 메모리 사용/전체, 디스크 읽기·쓰기, 네트워크 받기·보내기, 데이터 파일 시스템).
  - `orbivisorctl vm stats <VM>`: 마지막 점이다.
  - 둘 다 `--watch`로 5초마다 다시 그린다(화면 지우기 없이 새 표를 이어 찍음).
  - `-o json`은 모든 점이다.

### 3.4 크기와 부하

- 점 하나는 메모리에서 약 100바이트다. 노드 시리즈 720점은 약 70 KB, VM 100개는 약 7 MB다.
- 5초마다 VM마다 QMP 명령 하나, `/proc` 파일 두 개, tap 카운터 두 개를 읽는다.
- `/metrics/nodes?since=`의 응답은 노드마다 점 하나 남짓이다. 처음(since 없음)은 노드마다 720점(JSON 약 100 KB)이다.

### 3.5 시간

- `t`는 그 노드의 벽시계(`SystemTime`)다. 간격 계산은 `Instant`(단조)로 한다. 벽시계가 뒤로 가도 비율은 맞고, `t`만 겹칠 수 있다.
- 노드 사이 시계 차이는 그대로 보인다. 지금도 Raft lease·인증서 검증이 NTP를 전제한다.

### 3.6 웹 사용 방식 (계획 45가 따름)

- 처음은 `since` 없이 받는다(1시간). 그 뒤 5초마다 시리즈마다 마지막 `t`를 `since`로 보낸다. `/metrics/nodes`는 노드마다 since가 다르므로 가장 작은 값을 보낸다.
- 받은 점은 `t`로 걸러 덧붙이고, 1시간보다 오래된 점은 버린다.
- 화면이 보이지 않으면(`document.visibilityState`) 폴링을 멈춘다(TanStack Query `refetchIntervalInBackground: false`).

### 3.7 혼합 버전

- 옛 노드는 `ReadMetrics`를 모르므로 `Unimplemented`다. API는 `unsupported` 목록에 넣는다.
- 새 노드의 `FEATURES`에 `metrics`가 있어 웹이 미리 알 수 있지만, 응답의 `unsupported`가 기준이다.
- 지표는 Raft에 쓰지 않으므로 옛 노드가 모르는 리소스 필드가 없다.

## 4. 오류 (계획 44)

| 경우 | 응답 |
|---|---|
| 노드·VM 없음 | 404 `not_found` |
| 권한 없음 | 403 `forbidden` |
| `since`가 숫자가 아님 | 400 `invalid_request` |
| `/nodes/{key}/metrics`에서 그 노드가 닿지 않음 | 503 `node_unreachable` "node … did not answer: …" |
| `/nodes/{key}/metrics`에서 그 노드가 옛 버전 | 409 `cluster_upgrading` "node … does not report metrics yet" |
| 위트니스 노드의 `/nodes/{key}/metrics` | 409 `node_unavailable` "node … is a witness node and runs no workloads" |
| 목록 응답(`/metrics/nodes`, `/vms/{key}/metrics`)의 닿지 않거나 옛 노드 | 200 + `unreachable`·`unsupported` |

## 5. 시험 (계획 44)

- **파서 단위 시험:** `/proc/stat`·`meminfo`·`diskstats`(파티션·dm·loop·zd 걸러짐)·`/proc/<pid>/stat`(comm에 공백·괄호가 든 줄)·`status`의 고정 문자열.
- **측정기 시험:** 가짜 `HostProbe`와 `MockHypervisor`로 다음을 본다.
  - 첫 측정은 점이 없고 두 번째에 비율이 맞다.
  - pid가 바뀌거나 카운터가 줄면 다시 기준을 잡는다.
  - QMP 오류는 디스크 필드만 `null`이다.
  - 사라진 VM의 카운터를 지운다.
  - 시간은 `tokio::time::pause`로 다룬다.
- **저장소 시험:** 720점 상한, `since` 거르기, 1시간 지난 VM 시리즈 지우기.
- **QMP 시험:** `query_blockstats`를 가짜 QMP 서버로 시험한다. opl-qemu `qemu-it`에서는 실제 QEMU로 디스크에 쓰고 `wr_bytes`가 느는지 본다.
- **피어·API 시험:**
  - 감사 로그 시험처럼 두 노드(피어 서버 하나를 더 띄움)에서 다른 노드의 점을 읽는다.
  - 소스 없는 노드는 `unsupported`, 멈춘 노드는 `unreachable`이다.
  - 권한(Viewer 읽기 가능)과 404·400·503·409를 본다.
- **opld 시험:** `opld` E2E(macOS·Linux)에서 VM을 켜고 10초 남짓 뒤 `/vms/{key}/metrics`에 점이 있는지 본다. Linux CI linux-kvm 작업에서는 `net_rx_bps`가 `null`이 아님(tap)도 본다.
- **oplctl 시험:** 표 출력과 `-o json`.

## 6. 알려진 한계 (계획 44)

- 재시작하면 그 노드의 1시간 기록이 빈다.
- 꺼진 노드의 기록은 읽을 수 없다(그 노드의 메모리에 있다).
- macOS 노드는 노드 디스크 I/O와 VM 네트워크가 `null`이다.
- VM CPU는 QEMU 프로세스 전체(I/O 스레드 포함)의 CPU다. VM 메모리는 QEMU RSS라 게스트가 쓰는 양이 아니다. 처음 건드린 뒤 돌려주지 않은 메모리도 잡힌다.
- 노드 네트워크는 실제 NIC만 센다. 본드·VLAN 위의 트래픽은 아래 NIC로 센다. 가상 NIC만 있는 VM 안 노드(테스트 VM의 virtio NIC)는 `device` 링크가 있어 잡힌다.
- 측정 간격 5초는 고정이다(설정 없음).

## 7. 설계 — 계획 45 대시보드·차트

- **라이브러리:** uPlot(MIT, 약 50 KB)이다. 작은 리액트 래퍼 `web/src/charts/TimeChart.tsx`를 직접 두고, 의존성은 `uplot` 하나다. 다크 모드 색은 CSS 토큰에서 읽는다.
- **훅 `useNodeMetrics()`·`useNodeMetricsOne(node)`·`useVmMetrics(vm)`:** TanStack Query + `refetchInterval: 5000` + §3.6의 since 이어 붙이기다. 데이터는 쿼리 캐시가 아닌 훅 안의 상태(`useRef` 버퍼)에 두고 1시간으로 자른다.
- **대시보드:** 지금 카드 아래에 "노드" 줄을 둔다. 노드마다 카드 하나에 CPU·메모리 작은 차트(스파크라인, 15분)와 지금 값, 디스크·네트워크 지금 값을 보이고, 누르면 노드 상세로 간다. `unsupported`·`unreachable` 노드는 카드에 그 문구를 보인다.
- **노드 상세(새 경로 `nodes/:name`):** 머리는 이름·주소·상태·버전·유지보수다. 그 아래 차트 넷(CPU %, 메모리 MiB, 디스크 B/s 읽기·쓰기, 네트워크 B/s 받기·보내기)을 15분·1시간 범위로 고른다. 그 노드의 VM 목록(지금 VM 표를 노드로 거름)도 보인다. 노드 목록의 이름은 이 화면으로 가는 링크다.
- **VM 상세:** "지표" 탭에 차트 넷(CPU %, 메모리 RSS와 스펙 메모리 선, 디스크 B/s와 IOPS, 네트워크 B/s)을 둔다. `null` 구간은 끊어 그린다. VM이 꺼져 있으면 "VM이 꺼져 있습니다"와 남은 기록을 보인다.
- **시험:** msw로 지표 응답을 주고 차트 컨테이너·지금 값·문구·since 이어 붙이기를 본다. uPlot은 jsdom에서 캔버스가 없으므로 `TimeChart`를 시험에서 가짜로 바꾸거나 `matchMedia`·캔버스를 흉내 낸다.

## 8. 설계 — 계획 46 3D 클러스터 보기

- **라이브러리:** `three`, `@react-three/fiber`, `@react-three/drei`(모두 MIT)다. `React.lazy`로 따로 묶어 3D 화면을 열 때만 받는다. 첫 화면 묶음 크기는 그대로다.
- **장면:** 노드마다 둥근 받침(이름표, 온라인은 밝게, 오프라인·유지보수는 흐리게·줄무늬)이 원이나 격자로 놓인다. 받침 위에 VM마다 블록을 쌓는다.
  - 블록 크기: vCPU·메모리에 비례한다(로그 눈금).
  - 블록 색: 상태다(실행 초록, 꺼짐 회색, 오류 빨강, 마이그레이션 중 파랑 깜박임).
  - 블록 높이 띠: 지표 CPU %.
- **조작:** 마우스로 돌리기·확대(`OrbitControls`), 블록에 올리면 이름·노드·CPU·메모리, 누르면 VM 상세다. 마이그레이션 중인 VM은 두 받침 사이에 호를 그린다.
- **자리:** VM 목록 화면 머리에 "표 | 3D" 전환을 두고, 고른 것은 `localStorage`에 둔다. 대시보드에도 작은 3D 미리보기를 둔다(누르면 VM 목록 3D).
- **대체:** WebGL을 만들 수 없으면(`WebGLRenderingContext` 실패) 전환을 숨기고 표를 보인다. `prefers-reduced-motion`이면 자동 회전·깜박임을 끈다.
- **접근성:** 3D는 보조 보기이고 모든 동작은 표에도 있다.
- **시험:** 장면 구성 함수(노드·VM → 위치·크기·색)를 순수 함수로 두고 단위 시험한다. 캔버스는 jsdom에서 그리지 않고 대체 경로를 시험한다. Playwright e2e로 3D 화면이 열리고 오류가 없는지 본다.

## 9. 설계 — 계획 47 끌어 놓는 VM 만들기

- **라이브러리:** `@dnd-kit/core`(MIT)다. 키보드 센서·스크린 리더 안내를 기본으로 준다.
- **화면:** VM 만들기 대화상자에 "그림으로 | 양식" 전환을 두고, 그림이 기본이다. 지금 양식은 그대로 두고, 두 보기가 같은 상태(`VmDraft`)를 쓴다.
  - 왼쪽은 부품 팔레트다: CPU 칩, 메모리 막대, 디스크(풀마다), 네트워크 카드(네트워크마다), 설치 이미지(ISO 볼륨).
  - 가운데는 VM 그림(본체 상자 안의 슬롯)이다. 부품을 끌어 넣으면 슬롯에 들어가고, 넣은 부품을 누르면 크기 슬라이더가 열린다. vCPU는 1~노드 최대, 메모리는 512 MiB 단위, 디스크 크기는 GiB다.
  - 오른쪽은 요약과 들어맞음이다. 고른 노드(또는 자동)에 vCPU·메모리·풀 공간이 들어가는지 지금 지표와 용량으로 초록·노랑·빨강으로 보인다.
- **견본:** 작게(1 vCPU, 1 GiB, 10 GiB), 보통(2, 4 GiB, 40 GiB), 크게(4, 8 GiB, 100 GiB)다. 누르면 그림이 채워진다.
- **만들기:** 지금 양식과 같은 `POST /vms` 요청이다. 서버 검사와 오류 문구도 같다.
- **접근성:** 모든 끌기는 키보드로도 된다(부품에 초점 → 스페이스 → 화살표 → 스페이스). 슬라이더는 radix Slider다.
- **시험:** `VmDraft` 리듀서 단위 시험, 끌어 놓기(dnd-kit 키보드 경로를 user-event로)와 견본·들어맞음 문구·요청 본문(msw)을 시험한다.

## 10. 구현 단계 (계획 44)

1. **타입·파서:** `opl-types` metrics 타입, opld `metrics/host.rs`의 Linux 파서와 `HostProbe`·`LinuxProbe`·`MacProbe`, 파서 시험
2. **QMP·Hypervisor:** `query_blockstats`, `Hypervisor::disk_stats`(QEMU·Mock), 시험과 `qemu-it`
3. **측정기·저장소:** `MetricsStore`, `MetricsSampler`, `member.rs` 연결, 노드 기능 `metrics`, 시험
4. **피어 RPC와 API:** `ReadMetrics`, `MetricsSource`, `opl-api/src/metrics.rs` 세 경로, OpenAPI·`schema.d.ts`, 두 노드 시험
5. **oplctl과 문서:** `node stats`·`vm stats`(`--watch`, `-o json`), opld E2E, README "지표" 절, 이 문서 §11
