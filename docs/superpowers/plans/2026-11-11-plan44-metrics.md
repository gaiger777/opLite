# Orbivisor 계획 44: 지표 수집 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드마다 5초마다 노드(CPU·메모리·디스크 I/O·네트워크·데이터 파일 시스템)와 VM(CPU·메모리·디스크 읽기·쓰기와 IOPS·네트워크)의 사용량을 재서 메모리에 1시간 두고, 어느 노드에서든 REST API와 `orbivisorctl`로 모든 노드의 지표를 본다.

**Architecture:**
- **호스트 읽기:** Linux는 `/proc`·`/sys`를 직접 읽고(파서는 순수 함수), macOS는 libc(`host_processor_info`·`host_statistics64`·`proc_pidinfo`·`getifaddrs`)로 읽는다. 새 크레이트는 없다.
- **VM 값:** CPU·RSS는 QEMU 프로세스, 디스크는 QMP `query-blockstats`, 네트워크는 Linux tap 카운터(게스트 기준으로 바꿔 적음).
- **측정기와 저장소:** opld 안의 `MetricsSampler`가 5초마다 재고 카운터 차이를 비율로 바꿔 `MetricsStore`(시리즈마다 720점)에 둔다. Raft에는 쓰지 않는다.
- **읽기:** 새 피어 RPC `ReadMetrics`로 다른 노드의 점을 읽고, API가 감사 로그처럼 노드마다 모은다(닿지 않는 노드는 `unreachable`, 옛 노드는 `unsupported`). 웹(계획 45)은 `since`를 단 5초 폴링으로 새 점만 받는다.

**Tech Stack:** Rust 2024(MSRV 1.88), tokio, tonic(JSON 코덱), axum·utoipa, libc, clap. 웹은 `schema.d.ts`만 바뀐다. 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-11-monitoring-ui-design.md` (§1~§6, §10, §11 "구현(계획 44)")
**선행 계획:** 계획 1~43 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 노드·VM 지표 측정(5초)과 메모리 1시간 보관 | 디스크에 오래 두기, Prometheus 내보내기, 경보 |
| 피어 RPC `ReadMetrics`, REST API 세 경로, 노드 기능 `metrics` | 웹 차트(계획 45), 3D 보기(46), 끌어 놓는 VM 만들기(47) |
| `orbivisorctl node stats`·`vm stats`(`--watch`, `-o json`) | 게스트 안 지표, 디스크·NIC마다 나눈 값 |
| README "지표" 절, 설계 §11 | 측정 간격 설정 |

## 실행 환경

- **이 Mac에서 하는 것:** Rust 변경과 시험(Linux 파서와 `LinuxProbe`도 시험 빌드에서는 컴파일되어 가짜 트리로 돈다), opl-qemu `qemu-it`, opld `qemu-it` E2E(Homebrew QEMU).
- **Linux CI만:** `#[cfg(target_os = "linux")]` 코드의 컴파일·clippy, 실제 `/proc`·`/sys` 읽기, `bridge.rs`(`net-it`, root)의 tap 네트워크 값, QEMU 8.2의 `query-blockstats`.
- **도구 버전:** CI는 최신 stable Rust로 `clippy`를 돌린다. 이 Mac의 Homebrew Rust에 없는 린트가 있을 수 있다.

## 설계 결정

- **메모리 링 버퍼:** 지표는 5초마다 노드·VM 수만큼 생긴다. Raft에 쓰면 모든 노드가 복제하고 스냅샷이 커진다. 재시작하면 그 노드의 기록이 비는 것은 받아들인다.
- **읽을 때 모으기:** 감사 로그(`ReadAudit`)와 같은 틀이다. 리더에 모으면 리더가 바뀔 때 기록이 비고 짐이 몰린다.
- **디스크 I/O는 장치 경로로:** `-blockdev` 구성에서 `query-blockstats`의 `device`는 비고, 블록 노드 이름은 백업·미러 중 바뀐다. 장치 경로(`/machine/peripheral/disk{i}-dev/…`)가 늘 같다.
- **시각:** 비율은 단조 시계로, 점의 `t`는 그 노드의 벽시계(유닉스 ms)로 적는다. `since`는 그 노드 시계 기준이다.

## 파일 구조

```
crates/opl-types/src/{lib.rs, metrics.rs, node.rs}
crates/opl-qemu/src/{lib.rs, qmp.rs}, crates/opl-qemu/tests/qemu_boot.rs
crates/opl-driver/src/{hypervisor.rs, lib.rs, mock.rs, qemu.rs}
crates/opld/{Cargo.toml, src/main.rs, src/member.rs, src/bootstrap.rs, src/metrics.rs, src/metrics/{host.rs, sampler.rs, store.rs}, tests/{e2e.rs, bridge.rs}}
crates/opl-peer/{build.rs, src/{lib.rs, client.rs, server.rs, metrics.rs}, tests/grpc.rs}
crates/opl-api/src/{lib.rs, state.rs, metrics.rs}, crates/opl-api/tests/{metrics.rs, openapi.rs, two_nodes.rs}
crates/oplctl/src/{cli.rs, client.rs}
web/src/api/schema.d.ts, README.md, docs/superpowers/specs/2026-11-11-monitoring-ui-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

---

### Task 1: 지표 타입과 호스트 읽기

`opl-types`의 지표 타입(`NodeSample`·`VmSample`·`MetricsQuery`·`MetricsScope`·`NodeSeries`·`VmSeries`, 읽지 못한 값은 `null`)과 opld `metrics/host.rs`: `HostProbe` 트레이트, Linux `/proc`·`/sys` 파서(순수 함수)와 루트를 받는 `LinuxProbe`, libc로 읽는 `MacProbe`, 그 밖 OS의 `NoProbe`. 파서는 고정 문자열로, `LinuxProbe`는 가짜 `/proc`·`/sys` 트리로 시험한다(이 Mac에서도 돈다). 측정기가 쓰기 시작하는 Task 3까지 `mod metrics`는 시험에서만 컴파일한다(opld는 바이너리 크레이트라 부르지 않는 `pub` 항목도 dead_code다).

**Files:**
- Create: `crates/opl-types/src/metrics.rs`
- Create: `crates/opld/src/metrics.rs`
- Create: `crates/opld/src/metrics/host.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/src/main.rs`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-types/src/lib.rs`에서

```rust
pub mod cluster;
pub mod network;
```

을 다음으로 바꾼다:

```rust
pub mod cluster;
pub mod metrics;
pub mod network;
```

`crates/opl-types/src/metrics.rs`를 만든다:

```rust
//! 사용량 지표. 노드마다 5초마다 노드와 그 노드에서 도는 VM을 재고 메모리에 1시간 둔다(Raft에 쓰지 않는다).
//! 비율은 직전 측정과의 차이를 걸린 시간으로 나눈 값이고, 읽지 못한 값은 `null`이다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 노드 점 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeSample {
    /// 측정 시각(그 노드 벽시계의 유닉스 밀리초)
    pub t: i64,
    /// CPU 사용률 %(모든 코어를 합해 100)
    pub cpu: Option<f64>,
    pub mem_used_mib: Option<u64>,
    pub mem_total_mib: Option<u64>,
    /// 실제 디스크의 읽기 바이트/초(macOS는 `null`)
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    /// 실제 NIC의 받기 바이트/초
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
    /// 데이터 디렉터리가 있는 파일 시스템
    pub fs_used_mib: Option<u64>,
    pub fs_total_mib: Option<u64>,
}

/// VM 점 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmSample {
    /// 측정 시각(그 노드 벽시계의 유닉스 밀리초)
    pub t: i64,
    /// vCPU 대비 사용률 %(vCPU가 모두 바쁘면 100). QEMU 프로세스 전체라 100을 조금 넘을 수 있다.
    pub cpu: Option<f64>,
    /// QEMU 프로세스의 RSS
    pub mem_rss_mib: Option<u64>,
    /// 모든 디스크를 합한 값
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    pub disk_read_iops: Option<f64>,
    pub disk_write_iops: Option<f64>,
    /// 게스트 기준 받기 바이트/초(tap이 없으면 `null`)
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
}

/// 한 노드에 묻는 지표
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MetricsQuery {
    pub scope: MetricsScope,
    /// 이 `t`보다 뒤의 점만(그 노드 시계 기준). 없으면 남은 것 모두.
    pub since: Option<i64>,
}

/// 무엇의 지표인가
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricsScope {
    /// 그 노드 자신
    Nodes,
    /// 그 노드에서 잰 VM 하나
    Vm { id: Uuid },
}

/// 한 노드의 점들(시각 순)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeSeries {
    pub node: Uuid,
    pub samples: Vec<NodeSample>,
}

/// 한 노드에서 잰 VM 하나의 점들(시각 순)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmSeries {
    pub vm: Uuid,
    /// 잰 노드
    pub node: Uuid,
    pub samples: Vec<VmSample>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_tagged_by_kind() {
        let id = Uuid::now_v7();
        let q = MetricsQuery {
            scope: MetricsScope::Vm { id },
            since: Some(5),
        };
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"scope": {"kind": "vm", "id": id}, "since": 5})
        );
        assert_eq!(serde_json::from_value::<MetricsQuery>(json).unwrap(), q);
        let nodes = serde_json::json!({"scope": {"kind": "nodes"}, "since": null});
        assert_eq!(
            serde_json::from_value::<MetricsQuery>(nodes).unwrap(),
            MetricsQuery {
                scope: MetricsScope::Nodes,
                since: None
            }
        );
    }

    /// 읽지 못한 값은 빼지 않고 `null`로 보낸다(웹이 차트를 끊어 그린다).
    #[test]
    fn missing_values_are_null() {
        let series = VmSeries {
            vm: Uuid::now_v7(),
            node: Uuid::now_v7(),
            samples: vec![VmSample {
                t: 1_000,
                cpu: Some(12.5),
                mem_rss_mib: Some(512),
                disk_read_bps: None,
                disk_write_bps: None,
                disk_read_iops: None,
                disk_write_iops: None,
                net_rx_bps: Some(0.0),
                net_tx_bps: None,
            }],
        };
        let json = serde_json::to_value(&series).unwrap();
        assert_eq!(json["samples"][0]["cpu"], 12.5);
        assert_eq!(json["samples"][0]["disk_read_bps"], serde_json::Value::Null);
        assert_eq!(json["samples"][0]["net_rx_bps"], 0.0);
        assert_eq!(serde_json::from_value::<VmSeries>(json).unwrap(), series);
    }
}
```

`crates/opld/Cargo.toml`에서

```toml
hex.workspace = true
openraft.workspace = true
```

을 다음으로 바꾼다:

```toml
hex.workspace = true
libc.workspace = true
openraft.workspace = true
```

`crates/opld/src/main.rs`에서

```rust
mod member;
mod overlay;
```

을 다음으로 바꾼다:

```rust
mod member;
// 3단계에서 측정기가 부를 때까지는 시험에서만 컴파일한다(바이너리 크레이트라 pub이어도 쓰지 않으면 dead_code다).
#[cfg(test)]
mod metrics;
mod overlay;
```

`crates/opld/src/metrics.rs`를 만든다:

```rust
//! 사용량 지표 수집. 노드와 그 노드의 VM을 5초마다 재어 메모리에 1시간 둔다.

pub mod host;
```

`crates/opld/src/metrics/host.rs`를 만든다:

```rust
//! 호스트 값 읽기. 측정기가 5초마다 부르는 누적 카운터와 지금 값이다(비율은 측정기가 계산한다).
//!
//! Linux는 `/proc`·`/sys`를 읽고(파서는 글을 받는 순수 함수), macOS는 libc의 Mach·`proc_pidinfo`·
//! `getifaddrs`를 부른다. 그 밖의 OS는 모두 `None`이다.

use std::path::Path;
#[cfg(any(target_os = "linux", test))]
use std::path::PathBuf;
use std::sync::Arc;

/// CPU 누적 시간(단위는 구현마다 다르고 비율만 쓴다)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

/// 프로세스 하나의 누적 CPU 시간과 RSS
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// 사용자 + 커널 CPU 시간(나노초)
    pub cpu_ns: u64,
    pub rss_mib: u64,
}

/// 호스트 값을 읽는다. 읽지 못하면 `None`이다.
pub trait HostProbe: Send + Sync {
    /// 모든 코어를 합한 CPU 시간
    fn cpu_times(&self) -> Option<CpuTimes>;
    /// (쓰는 MiB, 전체 MiB)
    fn memory(&self) -> Option<(u64, u64)>;
    /// 실제 디스크에서 (읽은, 쓴) 누적 바이트
    fn disk_bytes(&self) -> Option<(u64, u64)>;
    /// 실제 NIC의 (받은, 보낸) 누적 바이트
    fn nic_bytes(&self) -> Option<(u64, u64)>;
    fn process(&self, pid: u32) -> Option<ProcStat>;
    /// tap 장치의 호스트 쪽 (받은, 보낸) 누적 바이트. 게스트 기준으로는 거꾸로다.
    fn tap_bytes(&self, name: &str) -> Option<(u64, u64)>;
    /// `path`가 있는 파일 시스템의 (쓰는 MiB, 전체 MiB)
    fn fs(&self, path: &Path) -> Option<(u64, u64)>;
}

/// 이 OS의 [`HostProbe`]
pub fn native() -> Arc<dyn HostProbe> {
    #[cfg(target_os = "linux")]
    let probe = Arc::new(LinuxProbe::default());
    #[cfg(target_os = "macos")]
    let probe = Arc::new(MacProbe::default());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let probe = Arc::new(NoProbe);
    probe
}

/// `statvfs`로 (쓰는 MiB, 전체 MiB)
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn statvfs_mib(path: &Path) -> Option<(u64, u64)> {
    let st = rustix::fs::statvfs(path).ok()?;
    let total = st.f_blocks * st.f_frsize;
    let free = st.f_bfree * st.f_frsize;
    Some((total.saturating_sub(free) >> 20, total >> 20))
}

/// `/proc/stat` 첫 줄(`cpu  user nice system idle iowait irq softirq steal …`). guest는 user에 이미
/// 들어 있어 앞의 여덟 값만 더하고, 바쁜 시간은 idle과 iowait를 뺀 것이다.
#[cfg(any(target_os = "linux", test))]
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.len() < 5 {
        return None;
    }
    let total: u64 = values.iter().sum();
    Some(CpuTimes {
        busy: total - values[3] - values[4],
        total,
    })
}

/// `/proc/meminfo`의 (MemTotal − MemAvailable, MemTotal) MiB
#[cfg(any(target_os = "linux", test))]
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let total = kib_field(text, "MemTotal:")?;
    let available = kib_field(text, "MemAvailable:")?;
    Some((total.saturating_sub(available) >> 10, total >> 10))
}

/// `Name:   123 kB` 줄의 값(KiB)
#[cfg(any(target_os = "linux", test))]
fn kib_field(text: &str, name: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with(name))?;
    line[name.len()..].split_whitespace().next()?.parse().ok()
}

/// 디스크 I/O를 셀 때 빼는 장치. 위에 쌓인 장치(LVM·RAID·ZFS zvol 등)는 아래 디스크와 두 번 세이고,
/// loop·램 디스크·네트워크 블록 장치는 물리 디스크가 아니다.
#[cfg(any(target_os = "linux", test))]
fn is_stacked_or_virtual(name: &str) -> bool {
    ["loop", "ram", "zram", "dm-", "md", "nbd", "zd", "rbd"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// `/proc/diskstats`에서 실제 디스크의 (읽은, 쓴) 바이트. `is_disk(name)`은 `/sys/block/<name>`이 있는지
/// (파티션이 아닌지)다. 섹터는 늘 512바이트다.
#[cfg(any(target_os = "linux", test))]
pub fn parse_diskstats(text: &str, is_disk: impl Fn(&str) -> bool) -> (u64, u64) {
    let mut read = 0;
    let mut written = 0;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // major minor name reads merged sectors_read ms writes merged sectors_written …
        if fields.len() < 10 {
            continue;
        }
        let name = fields[2];
        if is_stacked_or_virtual(name) || !is_disk(name) {
            continue;
        }
        let (Ok(r), Ok(w)) = (fields[5].parse::<u64>(), fields[9].parse::<u64>()) else {
            continue;
        };
        read += r * 512;
        written += w * 512;
    }
    (read, written)
}

/// `/proc/<pid>/stat`의 utime + stime(클록 틱). comm(둘째 값)에 공백·괄호가 들 수 있어 마지막 `)` 뒤를 센다.
#[cfg(any(target_os = "linux", test))]
pub fn parse_pid_stat(text: &str) -> Option<u64> {
    let rest = &text[text.rfind(')')? + 1..];
    // rest의 첫 값이 state(3번째 필드)이고 utime·stime은 14·15번째 필드다.
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// `/proc/<pid>/status`의 `VmRSS`(MiB)
#[cfg(any(target_os = "linux", test))]
pub fn parse_status_rss(text: &str) -> Option<u64> {
    kib_field(text, "VmRSS:").map(|kib| kib >> 10)
}

/// Linux: `/proc`·`/sys`를 읽는다. 시험은 `root`에 가짜 트리를 둔다.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone)]
pub struct LinuxProbe {
    root: PathBuf,
    clock_ticks: u64,
}

#[cfg(any(target_os = "linux", test))]
impl Default for LinuxProbe {
    fn default() -> Self {
        Self::with_root("/")
    }
}

#[cfg(any(target_os = "linux", test))]
impl LinuxProbe {
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        // SAFETY: sysconf는 인자만 읽는다.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            root: root.into(),
            clock_ticks: u64::try_from(ticks).ok().filter(|&t| t > 0).unwrap_or(100),
        }
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(path)).ok()
    }

    /// `/sys/class/net/<name>/statistics`의 (rx_bytes, tx_bytes)
    fn net_counters(&self, name: &str) -> Option<(u64, u64)> {
        let counter = |c: &str| -> Option<u64> {
            self.read(&format!("sys/class/net/{name}/statistics/{c}"))?
                .trim()
                .parse()
                .ok()
        };
        Some((counter("rx_bytes")?, counter("tx_bytes")?))
    }
}

#[cfg(any(target_os = "linux", test))]
impl HostProbe for LinuxProbe {
    fn cpu_times(&self) -> Option<CpuTimes> {
        parse_proc_stat(&self.read("proc/stat")?)
    }

    fn memory(&self) -> Option<(u64, u64)> {
        parse_meminfo(&self.read("proc/meminfo")?)
    }

    fn disk_bytes(&self) -> Option<(u64, u64)> {
        let text = self.read("proc/diskstats")?;
        let sys_block = self.root.join("sys/block");
        // `/sys/block`에서는 이름의 `/`가 `!`다(예: cciss!c0d0).
        Some(parse_diskstats(&text, |name| {
            sys_block.join(name.replace('/', "!")).exists()
        }))
    }

    /// `device` 링크가 있는 장치(실제·가상 하드웨어 NIC)만 더한다. 브리지·tap·VXLAN·본드·VLAN은 없다.
    fn nic_bytes(&self) -> Option<(u64, u64)> {
        let dir = std::fs::read_dir(self.root.join("sys/class/net")).ok()?;
        let mut total = (0, 0);
        for entry in dir.flatten() {
            if !entry.path().join("device").exists() {
                continue;
            }
            let Some((rx, tx)) = self.net_counters(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            total.0 += rx;
            total.1 += tx;
        }
        Some(total)
    }

    fn process(&self, pid: u32) -> Option<ProcStat> {
        let ticks = parse_pid_stat(&self.read(&format!("proc/{pid}/stat"))?)?;
        let rss_mib = parse_status_rss(&self.read(&format!("proc/{pid}/status"))?)?;
        Some(ProcStat {
            // tick × 10⁹은 u64를 넘을 수 있다(16 vCPU가 꽉 차면 약 133일).
            cpu_ns: (u128::from(ticks) * 1_000_000_000 / u128::from(self.clock_ticks)) as u64,
            rss_mib,
        })
    }

    fn tap_bytes(&self, name: &str) -> Option<(u64, u64)> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return None;
        }
        self.net_counters(name)
    }

    fn fs(&self, path: &Path) -> Option<(u64, u64)> {
        statvfs_mib(path)
    }
}

/// macOS: Mach 호스트 통계, `proc_pidinfo`, `getifaddrs`. 디스크 I/O와 tap은 없다.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct MacProbe {
    /// `mach_host_self()`는 부를 때마다 포트 참조를 하나 늘리므로 한 번만 얻는다.
    host: libc::mach_port_t,
    /// `proc_pidinfo`의 CPU 시간(Mach 시간 단위) → 나노초
    timebase: (u64, u64),
    /// `hw.memsize`(MiB). 알 수 없으면 0이다.
    memory_total_mib: u64,
}

#[cfg(target_os = "macos")]
impl Default for MacProbe {
    #[allow(deprecated)] // libc는 mach2 크레이트를 권하지만 새 의존성을 더하지 않는다.
    fn default() -> Self {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: 두 함수는 인자만 읽고 `info`에 쓴다.
        let (host, ok) = unsafe { (libc::mach_host_self(), libc::mach_timebase_info(&mut info)) };
        let timebase = if ok == 0 && info.numer > 0 && info.denom > 0 {
            (u64::from(info.numer), u64::from(info.denom))
        } else {
            (1, 1)
        };
        let mut bytes: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: hw.memsize는 u64 하나이고 `len`이 그 크기다.
        let ret = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                &mut bytes as *mut u64 as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        Self {
            host,
            timebase,
            memory_total_mib: if ret == 0 { bytes >> 20 } else { 0 },
        }
    }
}

#[cfg(target_os = "macos")]
impl HostProbe for MacProbe {
    fn cpu_times(&self) -> Option<CpuTimes> {
        let mut cpus: libc::natural_t = 0;
        let mut info: libc::processor_info_array_t = std::ptr::null_mut();
        let mut count: libc::mach_msg_type_number_t = 0;
        // SAFETY: 성공하면 커널이 코어마다 CPU_STATE_MAX개의 틱을 담은 배열을 우리 주소 공간에 만들어 준다.
        let ret = unsafe {
            libc::host_processor_info(
                self.host,
                libc::PROCESSOR_CPU_LOAD_INFO,
                &mut cpus,
                &mut info,
                &mut count,
            )
        };
        if ret != libc::KERN_SUCCESS || info.is_null() {
            return None;
        }
        // SAFETY: `info`는 `count`개의 integer_t다.
        let ticks = unsafe { std::slice::from_raw_parts(info, count as usize) };
        let states = libc::CPU_STATE_MAX as usize;
        let mut times = CpuTimes { busy: 0, total: 0 };
        for cpu in ticks.chunks_exact(states) {
            let tick = |s: libc::c_int| u64::from(cpu[s as usize] as u32);
            let busy = tick(libc::CPU_STATE_USER)
                + tick(libc::CPU_STATE_SYSTEM)
                + tick(libc::CPU_STATE_NICE);
            times.busy += busy;
            times.total += busy + tick(libc::CPU_STATE_IDLE);
        }
        // SAFETY: 커널이 준 배열을 돌려준다.
        #[allow(deprecated)]
        unsafe {
            libc::vm_deallocate(
                libc::mach_task_self(),
                info as libc::vm_address_t,
                count as libc::vm_size_t * std::mem::size_of::<libc::integer_t>(),
            );
        }
        Some(times)
    }

    /// 쓰는 메모리는 active + wired + 압축 페이지다(활동 모니터의 "사용된 메모리"에 가깝다).
    fn memory(&self) -> Option<(u64, u64)> {
        // SAFETY: vm_statistics64는 정수만 든 C 구조체다.
        let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `count`는 `stats`의 크기(integer_t 개수)이고 커널은 그만큼까지만 쓴다.
        let ret = unsafe {
            libc::host_statistics64(
                self.host,
                libc::HOST_VM_INFO64,
                &mut stats as *mut _ as libc::host_info64_t,
                &mut count,
            )
        };
        if ret != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: sysconf는 인자만 읽는다.
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let pages = u64::from(stats.active_count)
            + u64::from(stats.wire_count)
            + u64::from(stats.compressor_page_count);
        let total = self.memory_total_mib;
        (total > 0).then(|| ((pages * page) >> 20, total))
    }

    fn disk_bytes(&self) -> Option<(u64, u64)> {
        None
    }

    /// 이름이 `en`으로 시작하는 링크 계층 장치의 `if_data` 카운터. 32비트라 4 GiB마다 돌아간다(측정기가
    /// 줄어든 카운터를 기준으로 다시 잡는다).
    fn nic_bytes(&self) -> Option<(u64, u64)> {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: 성공하면 목록을 만들어 `head`에 둔다. 끝에서 freeifaddrs로 돌려준다.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return None;
        }
        let mut total = (0, 0);
        let mut cur = head;
        while !cur.is_null() {
            // SAFETY: 목록의 항목은 freeifaddrs 전까지 유효하다.
            let ifa = unsafe { &*cur };
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || ifa.ifa_data.is_null() || ifa.ifa_name.is_null() {
                continue;
            }
            // SAFETY: 위에서 null이 아님을 보았다. AF_LINK 항목의 ifa_data는 if_data다.
            let (family, name) = unsafe {
                (
                    i32::from((*ifa.ifa_addr).sa_family),
                    std::ffi::CStr::from_ptr(ifa.ifa_name),
                )
            };
            if family != libc::AF_LINK || !name.to_bytes().starts_with(b"en") {
                continue;
            }
            // SAFETY: 같은 이유
            let data = unsafe { &*(ifa.ifa_data as *const libc::if_data) };
            total.0 += u64::from(data.ifi_ibytes);
            total.1 += u64::from(data.ifi_obytes);
        }
        // SAFETY: getifaddrs가 준 목록을 한 번 돌려준다.
        unsafe { libc::freeifaddrs(head) };
        Some(total)
    }

    fn process(&self, pid: u32) -> Option<ProcStat> {
        // SAFETY: proc_taskinfo는 정수만 든 C 구조체다.
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: 커널은 `size`바이트까지만 쓴다.
        let got = unsafe {
            libc::proc_pidinfo(
                libc::c_int::try_from(pid).ok()?,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if got != size {
            return None;
        }
        // Apple Silicon에서 이 값은 나노초가 아니라 Mach 시간 단위다.
        let ticks = u128::from(info.pti_total_user + info.pti_total_system);
        let (numer, denom) = self.timebase;
        Some(ProcStat {
            cpu_ns: (ticks * u128::from(numer) / u128::from(denom)) as u64,
            rss_mib: info.pti_resident_size >> 20,
        })
    }

    fn tap_bytes(&self, _name: &str) -> Option<(u64, u64)> {
        None
    }

    fn fs(&self, path: &Path) -> Option<(u64, u64)> {
        statvfs_mib(path)
    }
}

/// 그 밖의 OS: 아무것도 읽지 않는다.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug)]
pub struct NoProbe;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl HostProbe for NoProbe {
    fn cpu_times(&self) -> Option<CpuTimes> {
        None
    }
    fn memory(&self) -> Option<(u64, u64)> {
        None
    }
    fn disk_bytes(&self) -> Option<(u64, u64)> {
        None
    }
    fn nic_bytes(&self) -> Option<(u64, u64)> {
        None
    }
    fn process(&self, _pid: u32) -> Option<ProcStat> {
        None
    }
    fn tap_bytes(&self, _name: &str) -> Option<(u64, u64)> {
        None
    }
    fn fs(&self, _path: &Path) -> Option<(u64, u64)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const PROC_STAT: &str = "\
cpu  100 20 30 400 50 6 7 8 90 0
cpu0 50 10 15 200 25 3 3 4 45 0
intr 12345
";

    #[test]
    fn proc_stat_counts_busy_without_idle_and_iowait() {
        // guest(90)는 user에 이미 들어 있어 더하지 않는다.
        assert_eq!(
            parse_proc_stat(PROC_STAT),
            Some(CpuTimes {
                busy: 100 + 20 + 30 + 6 + 7 + 8,
                total: 621,
            })
        );
        assert_eq!(parse_proc_stat("intr 1\n"), None);
        assert_eq!(parse_proc_stat("cpu  1 x 3 4 5\n"), None);
    }

    #[test]
    fn meminfo_uses_available_memory() {
        let text = "MemTotal:       16384000 kB\nMemFree:          100000 kB\n\
                    MemAvailable:    8192000 kB\nBuffers:            1000 kB\n";
        assert_eq!(parse_meminfo(text), Some((8000, 16000)));
        assert_eq!(parse_meminfo("MemTotal: 1024 kB\n"), None);
    }

    #[test]
    fn diskstats_counts_only_real_disks() {
        let text = "\
   8       0 sda 100 0 1000 0 50 0 2000 0 0 0 0
   8       1 sda1 90 0 900 0 40 0 1800 0 0 0 0
 259       0 nvme0n1 10 0 10 0 5 0 20 0 0 0 0 0 0 0 0 0 0
 253       0 dm-0 70 0 700 0 30 0 1500 0 0 0 0
   7       0 loop0 5 0 50 0 0 0 0 0 0 0 0
   9       0 md0 1 0 100 0 1 0 100 0 0 0 0
 230       0 zd0 1 0 100 0 1 0 100 0 0 0 0
 252       0 rbd0 1 0 100 0 1 0 100 0 0 0 0
  43       0 nbd0 1 0 100 0 1 0 100 0 0 0 0
   1       0 ram0 1 0 100 0 1 0 100 0 0 0 0
 254       0 zram0 1 0 100 0 1 0 100 0 0 0 0
";
        // `/sys/block`에 있는 것: 디스크와 쌓인 장치(파티션 sda1은 없다)
        let in_sys_block = [
            "sda", "nvme0n1", "dm-0", "loop0", "md0", "zd0", "rbd0", "nbd0", "ram0", "zram0",
        ];
        let (read, written) = parse_diskstats(text, |n| in_sys_block.contains(&n));
        assert_eq!(read, (1000 + 10) * 512);
        assert_eq!(written, (2000 + 20) * 512);
        assert_eq!(parse_diskstats("garbage\n", |_| true), (0, 0));
    }

    #[test]
    fn pid_stat_skips_a_comm_with_spaces_and_parens() {
        let text = "4242 (qemu (vm) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 \
                    700 300 0 0 20 0 5 0 1000 2000000 3000 18446744073709551615\n";
        assert_eq!(parse_pid_stat(text), Some(1000));
        assert_eq!(parse_pid_stat("4242 (qemu) S 1"), None);
    }

    #[test]
    fn status_rss_is_in_mib() {
        let text = "Name:\tqemu-system-x86\nVmPeak:\t 9000000 kB\nVmRSS:\t  2097152 kB\n";
        assert_eq!(parse_status_rss(text), Some(2048));
        assert_eq!(parse_status_rss("Name:\tkthreadd\n"), None);
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// 가짜 `/proc`·`/sys` 트리로 LinuxProbe가 파일을 맞게 고르는지 본다.
    #[test]
    fn linux_probe_reads_a_proc_and_sys_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "proc/stat", PROC_STAT);
        write(
            root,
            "proc/meminfo",
            "MemTotal: 2048 kB\nMemAvailable: 1024 kB\n",
        );
        write(
            root,
            "proc/diskstats",
            "8 0 sda 1 0 2 0 1 0 4 0 0 0 0\n8 1 sda1 1 0 2 0 1 0 4 0 0 0 0\n",
        );
        std::fs::create_dir_all(root.join("sys/block/sda")).unwrap();
        let stat = "7 (qemu) S 1 7 7 0 -1 0 0 0 0 0 250 50 0 0 20 0 1 0 1 1 1\n";
        write(root, "proc/7/stat", stat);
        write(root, "proc/7/status", "VmRSS:\t 3072 kB\n");
        // 오래 돈 큰 VM: tick × 10⁹이 u64를 넘는다.
        let stat = "9 (qemu) S 1 9 9 0 -1 0 0 0 0 0 60000000000 40000000000 0 0 20 0 1 0 1 1 1\n";
        write(root, "proc/9/stat", stat);
        write(root, "proc/9/status", "VmRSS:\t 1024 kB\n");
        // eth0은 실제 NIC, br0은 브리지(device 없음), opabcn0은 tap
        for (name, rx, tx, device) in [
            ("eth0", 1000, 2000, true),
            ("br0", 5, 5, false),
            ("opabcn0", 30, 40, false),
        ] {
            write(
                root,
                &format!("sys/class/net/{name}/statistics/rx_bytes"),
                &format!("{rx}\n"),
            );
            write(
                root,
                &format!("sys/class/net/{name}/statistics/tx_bytes"),
                &format!("{tx}\n"),
            );
            if device {
                std::fs::create_dir_all(root.join(format!("sys/class/net/{name}/device"))).unwrap();
            }
        }
        let probe = LinuxProbe::with_root(root);
        assert_eq!(probe.cpu_times().unwrap().total, 621);
        assert_eq!(probe.memory(), Some((1, 2)));
        assert_eq!(probe.disk_bytes(), Some((2 * 512, 4 * 512)));
        assert_eq!(probe.nic_bytes(), Some((1000, 2000)));
        assert_eq!(
            probe.process(7),
            Some(ProcStat {
                cpu_ns: 300 * 1_000_000_000 / probe.clock_ticks,
                rss_mib: 3,
            })
        );
        assert_eq!(
            probe.process(9).unwrap().cpu_ns,
            (100_000_000_000u128 * 1_000_000_000 / u128::from(probe.clock_ticks)) as u64
        );
        assert_eq!(probe.process(8), None);
        assert_eq!(probe.tap_bytes("opabcn0"), Some((30, 40)));
        assert_eq!(probe.tap_bytes("../../proc"), None);
        assert_eq!(probe.tap_bytes("nope"), None);
        let (used, total) = probe.fs(root).unwrap();
        assert!(total > 0 && used <= total);
    }

    /// 이 Mac에서 실제 값을 읽는다(값의 범위만 본다).
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_probe_reads_this_host() {
        let probe = MacProbe::default();
        let cpu = probe.cpu_times().unwrap();
        assert!(cpu.total > 0 && cpu.busy <= cpu.total);
        let (used, total) = probe.memory().unwrap();
        assert_eq!(total, crate::host::memory_mib());
        assert!(used > 0 && used <= total);
        assert!(probe.nic_bytes().is_some());
        // 이 시험 프로세스: CPU를 조금 쓰고 RSS가 있다.
        let me = probe.process(std::process::id()).unwrap();
        assert!(me.cpu_ns > 0);
        assert!(me.rss_mib > 0 && me.rss_mib < total);
        assert_eq!(probe.process(u32::MAX), None);
        assert_eq!(probe.disk_bytes(), None);
        assert_eq!(probe.tap_bytes("en0"), None);
        let (used, total) = probe.fs(Path::new("/")).unwrap();
        assert!(total > 0 && used <= total);
    }

    /// Linux(CI)에서 실제 `/proc`·`/sys`를 읽는다(값의 범위만 본다).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_probe_reads_this_host() {
        let probe = LinuxProbe::default();
        let cpu = probe.cpu_times().unwrap();
        assert!(cpu.total > 0 && cpu.busy <= cpu.total);
        let (used, total) = probe.memory().unwrap();
        assert_eq!(total, crate::host::memory_mib());
        assert!(used <= total);
        assert!(probe.disk_bytes().is_some());
        assert!(probe.nic_bytes().is_some());
        let me = probe.process(std::process::id()).unwrap();
        assert!(me.rss_mib > 0 && me.rss_mib < total);
        let (used, total) = probe.fs(Path::new("/")).unwrap();
        assert!(total > 0 && used <= total);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn native_probe_reads_memory_here() {
        assert!(native().memory().is_some());
    }
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo check -p opld`
Expected: `Cargo.lock`의 opld 의존성 목록이 바뀐다(새 패키지는 없다, 이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1653개, 실패 0

- [ ] **Step 4: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(metrics): 지표 타입과 호스트 읽기 — opl-types metrics(NodeSample·VmSample·MetricsQuery·MetricsScope·NodeSeries·VmSeries, openapi 스키마, 읽지 못한 값은 null), opld metrics/host.rs(HostProbe 트레이트와 CpuTimes·ProcStat, Linux 파서 parse_proc_stat·parse_meminfo·parse_diskstats(파티션·dm·loop·md·zd·rbd·nbd·ram·zram 뺌)·parse_pid_stat·parse_status_rss, /proc·/sys 루트를 받는 LinuxProbe, libc로 읽는 MacProbe(host_processor_info, host_statistics64, proc_pidinfo의 Mach 시간 → ns, en* getifaddrs, hw.memsize), 그 밖 OS NoProbe, native()), 고정 문자열·가짜 트리 시험과 이 호스트의 실제 값 범위 시험; 측정기가 부르는 3단계까지 metrics 모듈은 시험에서만 컴파일, opld 의존성에 libc"
```

---

### Task 2: VM 디스크 I/O 카운터

opl-qemu `QmpClient::query_blockstats`와 `BlockStats`(장치 경로 `qdev` 포함), opl-driver `DiskCounters`와 `Hypervisor::disk_stats`(QEMU는 장치 경로가 `disk{i}-dev`인 블록만 합한다 — 블록 노드 이름은 백업·미러 중 바뀐다, pflash는 빠진다; Mock은 `set_disk_stats`). 가짜 QMP 시험과 실제 QEMU 시험(모니터의 `qemu-io`로 디스크에 쓰고 `wr_bytes`가 느는지).

**Files:**
- Modify: `crates/opl-driver/src/hypervisor.rs`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-qemu/src/lib.rs`
- Modify: `crates/opl-qemu/src/qmp.rs`
- Modify: `crates/opl-qemu/tests/qemu_boot.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/hypervisor.rs`에서

```rust
    pub size: u64,
}
```

을 다음으로 바꾼다:

```rust
    pub size: u64,
}

/// 실행 중인 VM의 모든 디스크(`disk{i}`)를 합한 I/O 누적 카운터(게스트가 본 것, QMP `query-blockstats`).
/// QEMU를 다시 켜면 0부터 다시 센다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskCounters {
    pub rd_bytes: u64,
    pub wr_bytes: u64,
    pub rd_operations: u64,
    pub wr_operations: u64,
}
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust

    /// 라이브 마이그레이션을 받을 QEMU를 `-incoming defer`로 띄우고, 받는 중 표시(마이그레이션 Task
```

을 다음으로 바꾼다:

```rust

    /// 실행 중인 VM의 모든 디스크(`disk{i}`)를 합한 I/O 누적 카운터. 지표 측정기가 직전 값과의
    /// 차이로 비율을 낸다.
    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError>;

    /// 라이브 마이그레이션을 받을 QEMU를 `-incoming defer`로 띄우고, 받는 중 표시(마이그레이션 Task
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub use hypervisor::{
    ActualState, BackupTarget, BitmapInfo, DiskBitmaps, EventKind, Hypervisor, HypervisorEvent,
    RuntimeStatus, StopMode,
};
```

을 다음으로 바꾼다:

```rust
pub use hypervisor::{
    ActualState, BackupTarget, BitmapInfo, DiskBitmaps, DiskCounters, EventKind, Hypervisor,
    HypervisorEvent, RuntimeStatus, StopMode,
};
```

`crates/opl-driver/src/mock.rs`에서

```rust
use crate::hypervisor::{
    ActualState, BackupTarget, BitmapInfo, DiskBitmaps, EventKind, Hypervisor, HypervisorEvent,
    RuntimeStatus, StopMode,
};
```

을 다음으로 바꾼다:

```rust
use crate::hypervisor::{
    ActualState, BackupTarget, BitmapInfo, DiskBitmaps, DiskCounters, EventKind, Hypervisor,
    HypervisorEvent, RuntimeStatus, StopMode,
};
```

`crates/opl-driver/src/mock.rs`에서

```rust
    fail_next_cancel: VecDeque<DriverError>,
}
```

을 다음으로 바꾼다:

```rust
    fail_next_cancel: VecDeque<DriverError>,
    /// `set_disk_stats`로 정한 VM별 디스크 카운터. VM이 꺼지면 사라진다(다시 켜면 0).
    disk_stats: HashMap<Uuid, DiskCounters>,
}
```

`crates/opl-driver/src/mock.rs`에서

```rust
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }
```

을 다음으로 바꾼다:

```rust
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// 실행 중인 VM의 `disk_stats`가 돌려줄 카운터를 정한다(정하지 않으면 0).
    pub fn set_disk_stats(&self, id: Uuid, counters: DiskCounters) {
        self.state.lock().unwrap().disk_stats.insert(id, counters);
    }
```

`crates/opl-driver/src/mock.rs`에서

```rust
        st.mirroring.remove(&id);
        if st.vms.remove(&id).is_some() {
```

을 다음으로 바꾼다:

```rust
        st.mirroring.remove(&id);
        st.disk_stats.remove(&id);
        if st.vms.remove(&id).is_some() {
```

`crates/opl-driver/src/mock.rs`에서

```rust

    /// 범위의 첫 포트(내보낼 디스크가 있으면 NBD에 그다음 포트)를 돌려준다. `fail_next_start`로 주입한
```

을 다음으로 바꾼다:

```rust

    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Ok(st.disk_stats.get(&id).copied().unwrap_or_default())
    }

    /// 범위의 첫 포트(내보낼 디스크가 있으면 NBD에 그다음 포트)를 돌려준다. `fail_next_start`로 주입한
```

`crates/opl-driver/src/mock.rs`에서

```rust
            Err(DriverError::NotRunning(_))
        ));
    }
```

을 다음으로 바꾼다:

```rust
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn disk_stats_return_counters_set_by_the_test() {
        let hv = MockHypervisor::default();
        let vm = spec();
        assert!(matches!(
            hv.disk_stats(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
        hv.start(&vm, None).await.unwrap();
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), DiskCounters::default());

        let counters = DiskCounters {
            rd_bytes: 4096,
            wr_bytes: 512,
            rd_operations: 2,
            wr_operations: 1,
        };
        hv.set_disk_stats(vm.id, counters);
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), counters);
        hv.set_unresponsive(vm.id, true);
        assert!(matches!(
            hv.disk_stats(vm.id).await,
            Err(DriverError::NotAttached(_))
        ));
        hv.set_unresponsive(vm.id, false);

        // 실제 QEMU처럼 다시 켜면 0부터 센다.
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), DiskCounters::default());
    }
```

`crates/opl-driver/src/qemu.rs`에서

```rust
use crate::hypervisor::{
    ActualState, BackupTarget, DiskBitmaps, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus,
    StopMode,
};
```

을 다음으로 바꾼다:

```rust
use crate::hypervisor::{
    ActualState, BackupTarget, DiskBitmaps, DiskCounters, EventKind, Hypervisor, HypervisorEvent,
    RuntimeStatus, StopMode,
};
```

`crates/opl-driver/src/qemu.rs`에서

```rust

    async fn start_incoming(
```

을 다음으로 바꾼다:

```rust

    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError> {
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_blockstats()).await;
        let stats = flatten(r, "QMP query-blockstats")?;
        // 디스크는 장치 id `disk{i}-dev`로 고른다. 맨 위 노드는 블록 작업 중 필터 노드로 바뀐다.
        Ok(stats
            .iter()
            .filter(|s| s.qdev.as_deref().and_then(disk_device_index).is_some())
            .fold(DiskCounters::default(), |sum, s| DiskCounters {
                rd_bytes: sum.rd_bytes + s.rd_bytes,
                wr_bytes: sum.wr_bytes + s.wr_bytes,
                rd_operations: sum.rd_operations + s.rd_operations,
                wr_operations: sum.wr_operations + s.wr_operations,
            }))
    }

    async fn start_incoming(
```

`crates/opl-driver/src/qemu.rs`에서

```rust
    name.strip_prefix("disk")?.parse().ok()
}
```

을 다음으로 바꾼다:

```rust
    name.strip_prefix("disk")?.parse().ok()
}

/// qdev 경로(`/machine/peripheral/disk{i}-dev`, virtio는 그 아래 `/virtio-backend`)가 디스크 장치면 i
fn disk_device_index(qdev: &str) -> Option<usize> {
    let id = qdev
        .strip_prefix("/machine/peripheral/")?
        .split('/')
        .next()?;
    disk_index(id.strip_suffix("-dev")?)
}
```

`crates/opl-driver/src/qemu.rs`에서

```rust
                        nodes
                    }
```

을 다음으로 바꾼다:

```rust
                        nodes
                    }
                    // 같은 VM의 블록 백엔드: pflash, virtio 디스크, 미러 중이라 맨 위가 필터 노드인
                    // SATA 디스크
                    "query-blockstats" => {
                        let stats = |rd: u64, wr: u64, rd_ops: u64, wr_ops: u64| {
                            serde_json::json!({ "rd_bytes": rd, "wr_bytes": wr,
                                "rd_operations": rd_ops, "wr_operations": wr_ops })
                        };
                        serde_json::json!([
                            { "device": "pflash0", "node-name": "#block101",
                              "stats": stats(1000, 1000, 10, 10) },
                            { "device": "", "qdev": "/machine/peripheral/disk0-dev/virtio-backend",
                              "node-name": "disk0", "stats": stats(4096, 1024, 3, 2) },
                            { "device": "", "qdev": "/machine/peripheral/disk1-dev",
                              "node-name": "#block202", "stats": stats(100, 200, 1, 4) },
                        ])
                    }
```

`crates/opl-driver/src/qemu.rs`에서

```rust
    #[tokio::test]
    async fn bitmap_ops_use_disk_nodes_and_disk_formats() {
```

을 다음으로 바꾼다:

```rust
    #[tokio::test]
    async fn disk_stats_sum_only_the_vm_disks() {
        let tmp = short_tempdir();
        let (hv, id, fake, _qemu) = running_vm(tmp.path()).await;

        // pflash는 빼고 qdev가 `disk{i}-dev`인 백엔드만 합한다(맨 위 노드 이름과 상관없이).
        assert_eq!(
            hv.disk_stats(id).await.unwrap(),
            DiskCounters {
                rd_bytes: 4196,
                wr_bytes: 1224,
                rd_operations: 4,
                wr_operations: 6,
            }
        );
        assert_eq!(sent(&fake, &["query-blockstats"]).len(), 1);
        assert!(matches!(
            hv.disk_stats(Uuid::now_v7()).await,
            Err(DriverError::NotRunning(_))
        ));

        fake.failing.lock().unwrap().push("query-blockstats");
        assert!(matches!(
            hv.disk_stats(id).await,
            Err(DriverError::Qmp(QmpError::Command { .. }))
        ));
    }

    #[test]
    fn disk_device_index_reads_the_qdev_id() {
        for (qdev, want) in [
            ("/machine/peripheral/disk0-dev/virtio-backend", Some(0)),
            ("/machine/peripheral/disk12-dev", Some(12)),
            ("/machine/peripheral/nic0", None),
            ("/machine/peripheral/disk0", None),
            ("/machine/system.flash0", None),
        ] {
            assert_eq!(disk_device_index(qdev), want, "{qdev}");
        }
    }

    #[tokio::test]
    async fn bitmap_ops_use_disk_nodes_and_disk_formats() {
```

`crates/opl-qemu/src/lib.rs`에서

```rust
pub use qmp::{
    BitmapInfo, BlockNode, JobInfo, MigrationInfo, MigrationParameters, MigrationState,
    NbdBlockdev, QmpClient, QmpError, QmpEvent, RunState, TlsEndpoint, TransactionAction,
```

을 다음으로 바꾼다:

```rust
pub use qmp::{
    BitmapInfo, BlockNode, BlockStats, JobInfo, MigrationInfo, MigrationParameters, MigrationState,
    NbdBlockdev, QmpClient, QmpError, QmpEvent, RunState, TlsEndpoint, TransactionAction,
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
            bitmaps,
        })
```

을 다음으로 바꾼다:

```rust
            bitmaps,
        })
    }
}

/// `query-blockstats`의 블록 백엔드 하나와 게스트가 본 I/O 누적 카운터. QEMU를 다시 켜면 0부터다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockStats {
    /// 백엔드 이름(`-drive`의 이름). `-blockdev`로 붙인 디스크는 빈 문자열이다.
    pub device: String,
    /// 붙은 장치의 qdev 경로(`/machine/peripheral/disk0-dev/virtio-backend`). 없으면 None
    pub qdev: Option<String>,
    /// 백엔드에 붙은 맨 위 블록 노드(`disk{i}`. 블록 작업 중에는 필터 노드일 수 있다)
    pub node_name: Option<String>,
    pub rd_bytes: u64,
    pub wr_bytes: u64,
    pub rd_operations: u64,
    pub wr_operations: u64,
}

impl BlockStats {
    fn parse(v: &Value) -> Result<Self, QmpError> {
        let text = |k: &str| v[k].as_str().map(str::to_string);
        let count = |k: &str| {
            v["stats"][k]
                .as_u64()
                .ok_or_else(|| QmpError::Protocol(format!("blockstats without {k}: {v}")))
        };
        Ok(Self {
            device: text("device").unwrap_or_default(),
            qdev: text("qdev"),
            node_name: text("node-name"),
            rd_bytes: count("rd_bytes")?,
            wr_bytes: count("wr_bytes")?,
            rd_operations: count("rd_operations")?,
            wr_operations: count("wr_operations")?,
        })
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
            .map(BlockNode::parse)
            .collect()
```

을 다음으로 바꾼다:

```rust
            .map(BlockNode::parse)
            .collect()
    }

    /// 블록 백엔드마다의 I/O 카운터(`query-blockstats`, 아래 노드는 펼치지 않음)
    pub async fn query_blockstats(&self) -> Result<Vec<BlockStats>, QmpError> {
        let ret = self.execute("query-blockstats", None).await?;
        ret.as_array()
            .ok_or_else(|| QmpError::Protocol(format!("query-blockstats: {ret}")))?
            .iter()
            .map(BlockStats::parse)
            .collect()
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
    }

    #[tokio::test]
    async fn block_resize_sends_node_and_size() {
```

을 다음으로 바꾼다:

```rust
    }

    /// QEMU 8.2·11의 `query-blockstats` 모양: `-blockdev`로 붙인 디스크는 `device`가 비고 `qdev`가
    /// 장치 경로다. `-drive`로 붙인 pflash는 `device` 이름만 있고 `qdev`가 없다.
    #[tokio::test]
    async fn blockstats_read_counters_of_each_backend() {
        let (client, mut fake, _dir) = setup().await;
        let stats = |rd: u64, wr: u64, rd_ops: u64, wr_ops: u64| {
            json!({ "rd_bytes": rd, "wr_bytes": wr, "rd_operations": rd_ops,
                    "wr_operations": wr_ops, "flush_operations": 0, "idle_time_ns": 1,
                    "rd_merged": 0, "wr_merged": 0, "account_invalid": true })
        };
        let ret = json!([
            { "device": "pflash0", "node-name": "#block123", "stats": stats(1, 0, 1, 0) },
            { "device": "", "qdev": "/machine/peripheral/disk0-dev/virtio-backend",
              "node-name": "disk0", "stats": stats(4096, 512, 8, 1),
              "parent": { "node-name": "disk0-file", "stats": stats(0, 0, 0, 0) } },
            { "device": "", "qdev": "/machine/peripheral/disk1-dev", "stats": stats(2, 3, 4, 5) }
        ]);
        let (got, cmd) = tokio::join!(client.query_blockstats(), reply(&mut fake, ret));
        assert_eq!(cmd["execute"], "query-blockstats");
        let entry =
            |device: &str, qdev: Option<&str>, node: Option<&str>, c: [u64; 4]| BlockStats {
                device: device.into(),
                qdev: qdev.map(Into::into),
                node_name: node.map(Into::into),
                rd_bytes: c[0],
                wr_bytes: c[1],
                rd_operations: c[2],
                wr_operations: c[3],
            };
        assert_eq!(
            got.unwrap(),
            [
                entry("pflash0", None, Some("#block123"), [1, 0, 1, 0]),
                entry(
                    "",
                    Some("/machine/peripheral/disk0-dev/virtio-backend"),
                    Some("disk0"),
                    [4096, 512, 8, 1]
                ),
                entry(
                    "",
                    Some("/machine/peripheral/disk1-dev"),
                    None,
                    [2, 3, 4, 5]
                ),
            ]
        );

        // 카운터가 없으면 프로토콜 오류다.
        let (got, _) = tokio::join!(
            client.query_blockstats(),
            reply(&mut fake, json!([{ "device": "", "stats": {} }]))
        );
        assert!(matches!(got, Err(QmpError::Protocol(_))), "{got:?}");
    }

    #[tokio::test]
    async fn block_resize_sends_node_and_size() {
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust
    assert_eq!(disk0(&qmp).await.1, 2 << 30);

    qmp.quit().await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("qemu did not exit within 10s")
        .unwrap();
    assert!(exit.success(), "qemu exited with {exit}");
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(disk0(&qmp).await.1, 2 << 30);

    qmp.quit().await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("qemu did not exit within 10s")
        .unwrap();
    assert!(exit.success(), "qemu exited with {exit}");
}

/// `disk0` 장치(qdev `disk0-dev`)의 blockstats
async fn disk0_stats(qmp: &QmpClient) -> opl_qemu::BlockStats {
    let stats = qmp.query_blockstats().await.unwrap();
    stats
        .iter()
        .find(|s| {
            s.qdev
                .as_deref()
                .is_some_and(|q| q.starts_with("/machine/peripheral/disk0-dev"))
        })
        .unwrap_or_else(|| panic!("no disk0-dev in {stats:?}"))
        .clone()
}

#[tokio::test]
async fn blockstats_count_writes_to_the_disk() {
    let host = host();
    let (_tmp, mut child, paths) = launch(&host).await;
    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    qmp.cont().await.unwrap();

    let before = disk0_stats(&qmp).await;
    // -blockdev로 붙인 디스크는 device 이름이 없고 맨 위 노드가 disk0이다.
    assert_eq!(before.device, "");
    assert_eq!(before.node_name.as_deref(), Some("disk0"));

    // 게스트 대신 장치의 블록 백엔드에 직접 쓴다. qemu-io의 aio_write는 장치처럼 I/O를 센다.
    // virtio-blk-pci는 백엔드가 자식 장치 virtio-backend에 있어 그 qdev 경로를 준다.
    for io in ["aio_write -q 0 64k", "aio_flush"] {
        let cmd = format!(r#"qemu-io -d /machine/peripheral/disk0-dev/virtio-backend "{io}""#);
        let out = qmp
            .execute(
                "human-monitor-command",
                Some(serde_json::json!({ "command-line": cmd })),
            )
            .await
            .unwrap();
        // HMP 오류는 QMP 오류가 아닌 출력 문자열로 온다.
        assert_eq!(out, "", "{cmd}");
    }
    let after = disk0_stats(&qmp).await;
    assert!(
        after.wr_bytes >= before.wr_bytes + 65536,
        "{before:?} -> {after:?}"
    );
    assert!(after.wr_operations > before.wr_operations, "{after:?}");

    qmp.quit().await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("qemu did not exit within 10s")
        .unwrap();
    assert!(exit.success(), "qemu exited with {exit}");
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1657개, 실패 0

Run: `cargo test -p opl-qemu --features qemu-it --test qemu_boot`
Expected: PASS (실제 QEMU가 필요하다 — macOS Homebrew QEMU 또는 Linux CI `linux-kvm`)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): VM 디스크 I/O 카운터 — opl-qemu QmpClient::query_blockstats와 BlockStats{device, qdev, node_name, rd_bytes, wr_bytes, rd_operations, wr_operations}(카운터가 없으면 프로토콜 오류), Hypervisor::disk_stats와 DiskCounters(모든 디스크 합), QemuHypervisor는 qdev 장치 id가 disk{i}-dev인 백엔드만 합함(pflash 제외, 블록 작업 중 맨 위 노드가 필터여도 셈), MockHypervisor는 set_disk_stats로 시험이 정한 카운터(VM이 꺼지면 0); 가짜 QMP 시험과 qemu-it에서 HMP qemu-io aio_write 뒤 wr_bytes가 느는지"
```

---

### Task 3: 측정기와 저장소

opld `metrics/store.rs` `MetricsStore`(시리즈마다 720점, 1시간 지난 VM 시리즈 지움, `since` 거르기)와 `metrics/sampler.rs` `MetricsSampler`(5초마다 노드와 `Running`·`Paused` VM을 잼, 첫 측정·pid 바뀜·카운터 줄어듦은 기준만, 프로세스·tap은 측정 시작 시각·디스크는 QMP 응답 시각으로 비율 계산, QMP 1초·VM마다 4초 — 마감은 `list()` 뒤부터, 경고 10분에 한 번, 사라진 VM 카운터 지움, tap은 호스트 기준이라 게스트 기준으로 바꿔 적음). `member.rs`의 `Services` 연결과 끄기, 노드 기능 `metrics`, Mock의 `fail_disk_stats`·`delay_lists`·`delay_disk_stats`. 시험은 `tokio::time::pause`로.

**Files:**
- Create: `crates/opld/src/metrics/sampler.rs`
- Create: `crates/opld/src/metrics/store.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/src/metrics.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/mock.rs`에서

```rust
    disk_stats: HashMap<Uuid, DiskCounters>,
}
```

을 다음으로 바꾼다:

```rust
    disk_stats: HashMap<Uuid, DiskCounters>,
    /// `disk_stats`가 시간 초과로 실패하는 VM. VM이 꺼지면 사라진다.
    failing_disk_stats: HashSet<Uuid>,
    /// `list`가 이만큼 기다린 뒤에 답한다.
    list_delay: Option<Duration>,
    /// `disk_stats`가 이만큼 기다린 뒤에 답한다.
    disk_stats_delay: Option<Duration>,
}
```

`crates/opl-driver/src/mock.rs`에서

```rust
        self.state.lock().unwrap().disk_stats.insert(id, counters);
    }
```

을 다음으로 바꾼다:

```rust
        self.state.lock().unwrap().disk_stats.insert(id, counters);
    }

    /// 실행 중인 VM의 `disk_stats`만 시간 초과로 실패하게 하거나(true) 되돌린다(false). 다른 메서드와
    /// `list()`의 상태는 그대로다(QMP가 `query-blockstats`에만 늦게 답한 것처럼).
    pub fn fail_disk_stats(&self, id: Uuid, fail: bool) {
        let mut st = self.state.lock().unwrap();
        if fail {
            st.failing_disk_stats.insert(id);
        } else {
            st.failing_disk_stats.remove(&id);
        }
    }

    /// 이후 `list`가 이만큼 기다린 뒤에 답한다(응답 없는 VM 때문에 조회가 늦은 것처럼).
    pub fn delay_lists(&self, delay: Duration) {
        self.state.lock().unwrap().list_delay = Some(delay);
    }

    /// 이후 `disk_stats`가 이만큼 기다린 뒤에 답한다(QMP가 늦게 답한 것처럼).
    pub fn delay_disk_stats(&self, delay: Duration) {
        self.state.lock().unwrap().disk_stats_delay = Some(delay);
    }
```

`crates/opl-driver/src/mock.rs`에서

```rust
        st.disk_stats.remove(&id);
        if st.vms.remove(&id).is_some() {
```

을 다음으로 바꾼다:

```rust
        st.disk_stats.remove(&id);
        st.failing_disk_stats.remove(&id);
        if st.vms.remove(&id).is_some() {
```

`crates/opl-driver/src/mock.rs`에서

```rust
    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
```

을 다음으로 바꾼다:

```rust
    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let delay = self.state.lock().unwrap().list_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let st = self.state.lock().unwrap();
```

`crates/opl-driver/src/mock.rs`에서

```rust
    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Ok(st.disk_stats.get(&id).copied().unwrap_or_default())
```

을 다음으로 바꾼다:

```rust
    async fn disk_stats(&self, id: Uuid) -> Result<DiskCounters, DriverError> {
        let delay = self.state.lock().unwrap().disk_stats_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if st.failing_disk_stats.contains(&id) {
            return Err(DriverError::Timeout("query-blockstats"));
        }
        Ok(st.disk_stats.get(&id).copied().unwrap_or_default())
```

`crates/opl-driver/src/mock.rs`에서

```rust
        hv.set_unresponsive(vm.id, false);

        // 실제 QEMU처럼 다시 켜면 0부터 센다.
        hv.stop(vm.id, StopMode::Force).await.unwrap();
```

을 다음으로 바꾼다:

```rust
        hv.set_unresponsive(vm.id, false);
        hv.fail_disk_stats(vm.id, true);
        assert!(matches!(
            hv.disk_stats(vm.id).await,
            Err(DriverError::Timeout(_))
        ));
        assert_eq!(hv.list().await.unwrap()[0].status, RuntimeStatus::Running);
        hv.fail_disk_stats(vm.id, false);
        assert_eq!(hv.disk_stats(vm.id).await.unwrap(), counters);
        hv.fail_disk_stats(vm.id, true);

        // 실제 QEMU처럼 다시 켜면 0부터 센다(실패시킨 것도 지워진다).
        hv.stop(vm.id, StopMode::Force).await.unwrap();
```

`crates/opl-types/src/node.rs`에서

```rust
    "storage-migration",
];
```

을 다음으로 바꾼다:

```rust
    "storage-migration",
    "metrics",
];
```

`crates/opl-types/src/node.rs`에서

```rust
                "incremental-replication",
                "storage-migration"
            ])
```

을 다음으로 바꾼다:

```rust
                "incremental-replication",
                "storage-migration",
                "metrics"
            ])
```

`crates/opld/Cargo.toml`에서

```toml
clap.workspace = true
hex.workspace = true
```

을 다음으로 바꾼다:

```toml
clap.workspace = true
futures-util.workspace = true
hex.workspace = true
```

`crates/opld/Cargo.toml`에서

```toml
tempfile.workspace = true
```

을 다음으로 바꾼다:

```toml
tempfile.workspace = true
tokio = { workspace = true, features = ["test-util"] }
```

`crates/opld/src/bootstrap.rs`에서

```rust
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration"
            ]
        );
        update_node_status(&cluster, |status| {
```

을 다음으로 바꾼다:

```rust
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics"
            ]
        );
        update_node_status(&cluster, |status| {
```

`crates/opld/src/bootstrap.rs`에서

```rust
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration"
            ]
        );
        cluster.shutdown().await.unwrap();
    }
```

을 다음으로 바꾼다:

```rust
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics"
            ]
        );
        cluster.shutdown().await.unwrap();
    }
```

`crates/opld/src/bootstrap.rs`에서

```rust
            node().status.features,
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration"
            ]
        );
        cluster.shutdown().await.unwrap();
    }
```

을 다음으로 바꾼다:

```rust
            node().status.features,
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics"
            ]
        );
        cluster.shutdown().await.unwrap();
    }
```

`crates/opld/src/bootstrap.rs`에서

```rust
                    "storage-migration".into(),
                ],
```

을 다음으로 바꾼다:

```rust
                    "storage-migration".into(),
                    "metrics".into(),
                ],
```

`crates/opld/src/main.rs`에서

```rust
mod member;
// 3단계에서 측정기가 부를 때까지는 시험에서만 컴파일한다(바이너리 크레이트라 pub이어도 쓰지 않으면 dead_code다).
#[cfg(test)]
mod metrics;
```

을 다음으로 바꾼다:

```rust
mod member;
mod metrics;
```

`crates/opld/src/member.rs`에서

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, VXLAN 장치, VM 방화벽, APT
//! 업데이트. 위트니스는 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.
```

을 다음으로 바꾼다:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, 사용량 지표, VXLAN 장치, VM 방화벽, APT
//! 업데이트. 위트니스는 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.
```

`crates/opld/src/member.rs`에서

```rust
use crate::host::Host;
use crate::overlay::{OverlayAgent, cluster_tag};
```

을 다음으로 바꾼다:

```rust
use crate::host::Host;
use crate::metrics::host as metrics_host;
use crate::metrics::sampler::MetricsSampler;
use crate::metrics::store::MetricsStore;
use crate::overlay::{OverlayAgent, cluster_tag};
```

`crates/opld/src/member.rs`에서

```rust
    pools: PoolMounter,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
```

을 다음으로 바꾼다:

```rust
    pools: PoolMounter,
    /// 이 노드와 그 VM의 사용량 지표를 잰다.
    metrics: MetricsSampler,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
```

`crates/opld/src/member.rs`에서

```rust
        self.pools.shutdown().await;
        if let Some(updates) = self.updates {
```

을 다음으로 바꾼다:

```rust
        self.pools.shutdown().await;
        self.metrics.shutdown().await;
        if let Some(updates) = self.updates {
```

`crates/opld/src/member.rs`에서

```rust
        );
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
```

을 다음으로 바꾼다:

```rust
        );
        // 지표는 메모리에만 둔다(Raft에 쓰지 않는다).
        let metrics = MetricsSampler::spawn(
            hv.clone(),
            metrics_host::native(),
            cluster.clone(),
            config.data_dir.clone(),
            Arc::new(MetricsStore::default()),
        );
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
```

`crates/opld/src/member.rs`에서

```rust
                pools,
                overlay,
```

을 다음으로 바꾼다:

```rust
                pools,
                metrics,
                overlay,
```

`crates/opld/src/metrics.rs`에서

```rust
pub mod host;
```

을 다음으로 바꾼다:

```rust
pub mod host;
pub mod sampler;
pub mod store;
```

`crates/opld/src/metrics/sampler.rs`를 만든다:

```rust
//! 측정기. 5초마다 이 노드와 이 노드에서 도는 VM을 재어 [`MetricsStore`]에 넣는다.
//!
//! 비율은 직전 측정과의 차이를 걸린 시간(단조 시계)으로 나눈다. 첫 측정(시작, VM 시작·붙기, pid
//! 바뀜, 카운터가 줄어듦)은 기준만 잡고 점을 만들지 않는다.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use opl_cluster::Cluster;
use opl_driver::{DiskCounters, Hypervisor, RuntimeStatus};
use opl_types::metrics::{NodeSample, VmSample};
use opl_types::network::tap_name;
use opl_types::vm::VmSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;

use super::host::{CpuTimes, HostProbe};
use super::store::MetricsStore;

/// 측정 간격(고정)
pub const INTERVAL: Duration = Duration::from_secs(5);
/// VM 하나의 `query-blockstats`를 기다리는 시간. 넘으면 그 VM의 디스크 필드만 `null`이다.
pub const DISK_STATS_TIMEOUT: Duration = Duration::from_secs(1);
/// 한 번의 측정에서 VM들을 재는 시간. 넘은 VM은 이번 점이 없다.
pub const VM_TIMEOUT: Duration = Duration::from_secs(4);
/// 같은 VM의 실패 경고를 다시 남기는 간격
pub const WARN_EVERY: Duration = Duration::from_secs(600);

pub struct MetricsSampler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl MetricsSampler {
    /// `data_dir`은 노드 점의 파일 시스템 값을 읽는 데이터 디렉터리다.
    pub fn spawn(
        hv: Arc<dyn Hypervisor>,
        probe: Arc<dyn HostProbe>,
        cluster: Arc<Cluster>,
        data_dir: PathBuf,
        store: Arc<MetricsStore>,
    ) -> Self {
        let sampler = Sampler::new(hv, probe, cluster, data_dir, store);
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_sampler(sampler, rx)),
        }
    }

    /// 멈춘다. 저장소의 점은 그대로 둔다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_sampler(mut sampler: Sampler, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = sampler.tick() => {}
        }
    }
}

/// 노드의 직전 누적 카운터
struct NodeCounters {
    at: Instant,
    cpu: Option<CpuTimes>,
    disk: Option<(u64, u64)>,
    nic: Option<(u64, u64)>,
}

/// VM의 직전 누적 카운터. pid가 바뀌면 다시 기준을 잡는다.
#[derive(Debug, Clone, Copy)]
struct VmCounters {
    pid: u32,
    at: Instant,
    cpu_ns: Option<u64>,
    disk: Option<DiskCounters>,
    /// 모든 tap의 호스트 쪽 (받은, 보낸) 합
    tap: Option<(u64, u64)>,
}

/// VM 하나를 잰 것
struct VmReading {
    counters: VmCounters,
    rss_mib: Option<u64>,
    /// `query-blockstats`가 실패했으면 그 오류
    disk_error: Option<String>,
}

/// 카운터가 직전보다 줄었다(다시 기준을 잡는다).
struct Decreased;

/// 누적 값의 초당 증가. 어느 한쪽이라도 없으면 `None`이다.
fn rate(prev: Option<u64>, now: Option<u64>, secs: f64) -> Result<Option<f64>, Decreased> {
    match (prev, now) {
        (Some(prev), Some(now)) => {
            let delta = now.checked_sub(prev).ok_or(Decreased)?;
            Ok(Some(delta as f64 / secs))
        }
        _ => Ok(None),
    }
}

fn node_sample(
    prev: &NodeCounters,
    now: &NodeCounters,
    t: i64,
    memory: Option<(u64, u64)>,
    fs: Option<(u64, u64)>,
) -> Result<NodeSample, Decreased> {
    let secs = now.at.duration_since(prev.at).as_secs_f64();
    let cpu = match (prev.cpu, now.cpu) {
        (Some(prev), Some(now)) => {
            let busy = now.busy.checked_sub(prev.busy).ok_or(Decreased)?;
            let total = now.total.checked_sub(prev.total).ok_or(Decreased)?;
            (total > 0).then(|| busy as f64 * 100.0 / total as f64)
        }
        _ => None,
    };
    let first = |c: Option<(u64, u64)>| c.map(|c| c.0);
    let second = |c: Option<(u64, u64)>| c.map(|c| c.1);
    Ok(NodeSample {
        t,
        cpu,
        mem_used_mib: memory.map(|m| m.0),
        mem_total_mib: memory.map(|m| m.1),
        disk_read_bps: rate(first(prev.disk), first(now.disk), secs)?,
        disk_write_bps: rate(second(prev.disk), second(now.disk), secs)?,
        net_rx_bps: rate(first(prev.nic), first(now.nic), secs)?,
        net_tx_bps: rate(second(prev.nic), second(now.nic), secs)?,
        fs_used_mib: fs.map(|f| f.0),
        fs_total_mib: fs.map(|f| f.1),
    })
}

fn vm_sample(
    prev: &VmCounters,
    now: &VmCounters,
    t: i64,
    vcpus: Option<u32>,
    rss_mib: Option<u64>,
) -> Result<VmSample, Decreased> {
    let secs = now.at.duration_since(prev.at).as_secs_f64();
    // 바쁜 vCPU 수 × 100. QEMU 프로세스 전체라 100을 조금 넘을 수 있고 자르지 않는다.
    let cpu = rate(prev.cpu_ns, now.cpu_ns, secs)?
        .zip(vcpus.filter(|&n| n > 0))
        .map(|(ns, n)| ns / 1e9 * 100.0 / f64::from(n));
    let disk = |f: fn(&DiskCounters) -> u64| {
        rate(prev.disk.as_ref().map(f), now.disk.as_ref().map(f), secs)
    };
    let tap =
        |f: fn(&(u64, u64)) -> u64| rate(prev.tap.as_ref().map(f), now.tap.as_ref().map(f), secs);
    Ok(VmSample {
        t,
        cpu,
        mem_rss_mib: rss_mib,
        disk_read_bps: disk(|d| d.rd_bytes)?,
        disk_write_bps: disk(|d| d.wr_bytes)?,
        disk_read_iops: disk(|d| d.rd_operations)?,
        disk_write_iops: disk(|d| d.wr_operations)?,
        // tap은 호스트 쪽 방향이다. 게스트가 받은 것은 호스트가 tap으로 보낸 것이다.
        net_rx_bps: tap(|t| t.1)?,
        net_tx_bps: tap(|t| t.0)?,
    })
}

/// 지금 벽시계(유닉스 밀리초)
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 마지막 경고에서 [`WARN_EVERY`]가 지났으면 지금을 적고 true
fn warn_due(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.duration_since(last) < WARN_EVERY) {
        return false;
    }
    *last = Some(now);
    true
}

struct Sampler {
    hv: Arc<dyn Hypervisor>,
    probe: Arc<dyn HostProbe>,
    cluster: Arc<Cluster>,
    data_dir: PathBuf,
    store: Arc<MetricsStore>,
    node: Option<NodeCounters>,
    vms: HashMap<Uuid, VmCounters>,
    /// VM마다 마지막 디스크 실패 경고 시각
    disk_warned: HashMap<Uuid, Option<Instant>>,
    list_warned: Option<Instant>,
    /// 이번 측정의 VM 점 시각. VM마다 `disk_stats`를 부르기 전이라 QMP 응답이 늦어도 카운터와 맞다(프로세스·tap
    /// 값은 빨리 읽으므로 같은 시각으로 본다).
    vm_at: Instant,
}

impl Sampler {
    fn new(
        hv: Arc<dyn Hypervisor>,
        probe: Arc<dyn HostProbe>,
        cluster: Arc<Cluster>,
        data_dir: PathBuf,
        store: Arc<MetricsStore>,
    ) -> Self {
        Self {
            hv,
            probe,
            cluster,
            data_dir,
            store,
            node: None,
            vms: HashMap::new(),
            disk_warned: HashMap::new(),
            list_warned: None,
            vm_at: Instant::now(),
        }
    }

    /// 한 번 잰다.
    async fn tick(&mut self) {
        let started = Instant::now();
        let t = now_ms();
        self.sample_node(started, t);
        self.sample_vms(started).await;
        self.store.prune(t);
    }

    fn sample_node(&mut self, at: Instant, t: i64) {
        let now = NodeCounters {
            at,
            cpu: self.probe.cpu_times(),
            disk: self.probe.disk_bytes(),
            nic: self.probe.nic_bytes(),
        };
        if let Some(prev) = &self.node
            && let Ok(sample) = node_sample(
                prev,
                &now,
                t,
                self.probe.memory(),
                self.probe.fs(&self.data_dir),
            )
        {
            self.store.push_node(sample);
        }
        self.node = Some(now);
    }

    async fn sample_vms(&mut self, started: Instant) {
        let running = match self.hv.list().await {
            Ok(vms) => vms,
            Err(e) => {
                if warn_due(&mut self.list_warned, started) {
                    tracing::warn!(error = %e, "cannot list VMs for metrics");
                }
                return;
            }
        };
        let running: Vec<_> = running
            .into_iter()
            .filter(|vm| matches!(vm.status, RuntimeStatus::Running | RuntimeStatus::Paused))
            .collect();
        // vCPU 수와 NIC 수는 클러스터의 VM 스펙에서 온다(없으면 CPU·네트워크가 `null`).
        let specs: HashMap<Uuid, (u32, usize)> = self
            .cluster
            .list::<VmSpec>()
            .map(|(_, vms)| {
                vms.into_iter()
                    .map(|vm| {
                        let cpu = &vm.spec.cpu;
                        let vcpus = u32::from(cpu.sockets) * u32::from(cpu.cores);
                        (vm.id, (vcpus, vm.spec.nics.len()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // 시각(단조·벽시계)과 마감은 `list()`가 끝난 뒤에 잡는다(QEMU의 `list()`는 VM마다 QMP를 차례로
        // 불러 응답 없는 VM 하나로 늦어질 수 있다).
        self.vm_at = Instant::now();
        let t = now_ms();
        let deadline = self.vm_at + VM_TIMEOUT;
        let this = &*self;
        let readings = join_all(running.iter().map(|vm| {
            let nics = specs.get(&vm.id).map_or(0, |s| s.1);
            async move {
                let reading =
                    tokio::time::timeout_at(deadline, this.measure_vm(vm.id, vm.pid, nics)).await;
                (vm.id, reading.ok())
            }
        }))
        .await;

        // 사라진 VM의 카운터는 지운다(다시 나타나면 기준부터).
        let live: HashSet<Uuid> = running.iter().map(|vm| vm.id).collect();
        self.vms.retain(|id, _| live.contains(id));
        self.disk_warned.retain(|id, _| live.contains(id));
        for (id, reading) in readings {
            // 측정 시간을 넘었다. 직전 카운터를 두고 다음에 잰다.
            let Some(reading) = reading else { continue };
            if let Some(error) = &reading.disk_error
                && warn_due(self.disk_warned.entry(id).or_default(), started)
            {
                tracing::warn!(vm = %id, error, "cannot read VM disk counters for metrics");
            }
            let now = reading.counters;
            if let Some(prev) = self.vms.get(&id)
                && prev.pid == now.pid
                && let Ok(sample) =
                    vm_sample(prev, &now, t, specs.get(&id).map(|s| s.0), reading.rss_mib)
            {
                self.store.push_vm(id, sample);
            }
            self.vms.insert(id, now);
        }
    }

    async fn measure_vm(&self, id: Uuid, pid: u32, nics: usize) -> VmReading {
        let (disk, disk_error) =
            match tokio::time::timeout(DISK_STATS_TIMEOUT, self.hv.disk_stats(id)).await {
                Ok(Ok(counters)) => (Some(counters), None),
                Ok(Err(e)) => (None, Some(e.to_string())),
                Err(_) => (None, Some("query-blockstats timed out".to_string())),
            };
        let process = self.probe.process(pid);
        // tap이 하나라도 없으면(macOS 사용자 모드 NIC, NIC 없음) 네트워크는 `null`이다.
        let tap = (nics > 0)
            .then(|| {
                (0..nics).try_fold((0u64, 0u64), |(rx, tx), i| {
                    let (r, t) = self.probe.tap_bytes(&tap_name(id, i))?;
                    Some((rx + r, tx + t))
                })
            })
            .flatten();
        VmReading {
            counters: VmCounters {
                pid,
                at: self.vm_at,
                cpu_ns: process.map(|p| p.cpu_ns),
                disk,
                tap,
            },
            rss_mib: process.map(|p| p.rss_mib),
            disk_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use opl_driver::{MockHypervisor, StopMode};
    use opl_types::resource::Resource;
    use opl_types::runtime::VmRuntimeSpec;
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType, NicModel, NicSpec, PowerState};

    use super::super::host::ProcStat;
    use super::*;
    use crate::bootstrap::tests::single_node;
    use opl_types::metrics::{MetricsQuery, MetricsScope};

    /// 시험이 정한 값을 돌려주는 [`HostProbe`]
    #[derive(Default)]
    struct FakeProbe(Mutex<FakeValues>);

    #[derive(Default, Clone)]
    struct FakeValues {
        cpu: Option<CpuTimes>,
        memory: Option<(u64, u64)>,
        disk: Option<(u64, u64)>,
        nic: Option<(u64, u64)>,
        processes: HashMap<u32, ProcStat>,
        taps: HashMap<String, (u64, u64)>,
        fs: Option<(u64, u64)>,
    }

    impl FakeProbe {
        fn set(&self, f: impl FnOnce(&mut FakeValues)) {
            f(&mut self.0.lock().unwrap());
        }
    }

    impl HostProbe for FakeProbe {
        fn cpu_times(&self) -> Option<CpuTimes> {
            self.0.lock().unwrap().cpu
        }
        fn memory(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().memory
        }
        fn disk_bytes(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().disk
        }
        fn nic_bytes(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().nic
        }
        fn process(&self, pid: u32) -> Option<ProcStat> {
            self.0.lock().unwrap().processes.get(&pid).copied()
        }
        fn tap_bytes(&self, name: &str) -> Option<(u64, u64)> {
            self.0.lock().unwrap().taps.get(name).copied()
        }
        fn fs(&self, _path: &Path) -> Option<(u64, u64)> {
            self.0.lock().unwrap().fs
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        cluster: Arc<Cluster>,
        hv: Arc<MockHypervisor>,
        probe: Arc<FakeProbe>,
        store: Arc<MetricsStore>,
        sampler: Sampler,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let hv = Arc::new(MockHypervisor::default());
        let probe = Arc::new(FakeProbe::default());
        let store = Arc::new(MetricsStore::default());
        let sampler = Sampler::new(
            hv.clone(),
            probe.clone(),
            cluster.clone(),
            dir.path().to_path_buf(),
            store.clone(),
        );
        Fixture {
            _dir: dir,
            cluster,
            hv,
            probe,
            store,
            sampler,
        }
    }

    fn node_samples(store: &MetricsStore) -> Vec<NodeSample> {
        store
            .read(&MetricsQuery {
                scope: MetricsScope::Nodes,
                since: None,
            })
            .node
            .unwrap()
    }

    fn vm_samples(store: &MetricsStore, id: Uuid) -> Vec<VmSample> {
        store
            .read(&MetricsQuery {
                scope: MetricsScope::Vm { id },
                since: None,
            })
            .vm
            .unwrap_or_default()
    }

    /// 클러스터에 vCPU 2개, NIC 2개인 VM을 만들고 하이퍼바이저에서 켠다.
    async fn running_vm(f: &Fixture) -> Uuid {
        let cpu = CpuSpec {
            sockets: 1,
            cores: 2,
            model: CpuModel::Host,
        };
        let nic = NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac: None,
            security_groups: vec![],
        };
        let vm = Resource::new(
            "web",
            VmSpec {
                cpu: cpu.clone(),
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics: vec![nic.clone(), nic],
                power: PowerState::Running,
                restart: Default::default(),
                node: None,
                snapshot_schedule: None,
                ha: false,
            },
        );
        f.cluster.create(&vm).await.unwrap();
        start(f, vm.id).await;
        vm.id
    }

    async fn start(f: &Fixture, id: Uuid) {
        f.hv.start(
            &VmRuntimeSpec {
                id,
                name: "web".into(),
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 2,
                    model: CpuModel::Host,
                },
                memory_mib: 256,
                firmware: Firmware::Uefi,
                machine: MachineType::Virt,
                disks: vec![],
                nics: vec![],
            },
            None,
        )
        .await
        .unwrap();
    }

    fn pid_of(hv: &MockHypervisor, id: Uuid) -> u32 {
        futures_util::FutureExt::now_or_never(hv.list())
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|vm| vm.id == id)
            .unwrap()
            .pid
    }

    /// VM 카운터를 정한다: 프로세스 CPU 시간·RSS, 디스크 카운터, 두 tap의 호스트 쪽 (받은, 보낸).
    fn set_vm(f: &Fixture, id: Uuid, cpu_ns: u64, disk: DiskCounters, tap: (u64, u64)) {
        let pid = pid_of(&f.hv, id);
        f.probe.set(|v| {
            v.processes.insert(
                pid,
                ProcStat {
                    cpu_ns,
                    rss_mib: 300,
                },
            );
            v.taps.insert(tap_name(id, 0), tap);
            v.taps.insert(tap_name(id, 1), tap);
        });
        f.hv.set_disk_stats(id, disk);
    }

    fn disk(bytes: u64, ops: u64) -> DiskCounters {
        DiskCounters {
            rd_bytes: bytes,
            wr_bytes: bytes * 2,
            rd_operations: ops,
            wr_operations: ops * 2,
        }
    }

    #[tokio::test]
    async fn node_rates_start_on_the_second_tick() {
        let mut f = fixture().await;
        tokio::time::pause();
        f.probe.set(|v| {
            v.cpu = Some(CpuTimes {
                busy: 100,
                total: 1_000,
            });
            v.memory = Some((1_000, 4_000));
            v.disk = Some((0, 0));
            v.nic = Some((1_000, 2_000));
            v.fs = Some((10, 100));
        });
        f.sampler.tick().await;
        // 첫 측정은 기준만 잡는다.
        assert!(node_samples(&f.store).is_empty());

        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| {
            v.cpu = Some(CpuTimes {
                busy: 600,
                total: 2_000,
            });
            v.disk = Some((5_000, 10_000));
            v.nic = Some((6_000, 2_000));
        });
        f.sampler.tick().await;
        let samples = node_samples(&f.store);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(
            (s.mem_used_mib, s.mem_total_mib),
            (Some(1_000), Some(4_000))
        );
        assert_eq!(
            (s.disk_read_bps, s.disk_write_bps),
            (Some(1_000.0), Some(2_000.0))
        );
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(1_000.0), Some(0.0)));
        assert_eq!((s.fs_used_mib, s.fs_total_mib), (Some(10), Some(100)));
        assert!((s.t - now_ms()).abs() < 60_000);

        // NIC 카운터가 줄면(32비트 카운터가 넘침) 이번 점 없이 기준을 다시 잡는다.
        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| v.nic = Some((10, 2_000)));
        f.sampler.tick().await;
        assert_eq!(node_samples(&f.store).len(), 1);
        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| v.nic = Some((5_010, 2_000)));
        f.sampler.tick().await;
        let samples = node_samples(&f.store);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].net_rx_bps, Some(1_000.0));
        // 못 읽은 값은 `null`이다.
        assert_eq!(samples[1].disk_read_bps, Some(0.0));
        f.probe.set(|v| v.disk = None);
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert_eq!(node_samples(&f.store)[2].disk_read_bps, None);
    }

    #[tokio::test]
    async fn vm_rates_start_on_the_second_tick() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        assert!(vm_samples(&f.store, id).is_empty());

        tokio::time::advance(INTERVAL).await;
        // vCPU 2개 중 하나가 5초 내내 바빴다.
        set_vm(&f, id, 5_000_000_000, disk(50_000, 50), (1_000, 3_000));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(
            (s.disk_read_bps, s.disk_write_bps),
            (Some(10_000.0), Some(20_000.0))
        );
        assert_eq!(
            (s.disk_read_iops, s.disk_write_iops),
            (Some(10.0), Some(20.0))
        );
        // 게스트 기준: 받기는 tap의 보내기(두 tap 합)다.
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(1_200.0), Some(400.0)));
    }

    #[tokio::test]
    async fn vm_baseline_is_taken_again_when_the_pid_changes_or_a_counter_drops() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 1_000_000_000, disk(1_000, 1), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);

        // 다시 켜면 pid가 바뀐다. 카운터가 줄지 않았어도 이번 점은 없다(다른 프로세스의 값이다).
        f.hv.stop(id, StopMode::Force).await.unwrap();
        start(&f, id).await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 10_000_000_000, disk(2_000, 2), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 15_000_000_000, disk(2_500, 7), (0, 0));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].cpu, Some(50.0));
        assert_eq!(samples[1].disk_read_bps, Some(100.0));

        // 카운터가 줄면 이번 점 없이 기준을 다시 잡는다.
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 16_000_000_000, disk(100, 1), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 2);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 17_000_000_000, disk(600, 2), (0, 0));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[2].disk_read_bps, Some(100.0));
    }

    #[tokio::test]
    async fn a_disk_stats_failure_nulls_only_the_disk_fields() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        f.hv.fail_disk_stats(id, true);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(1_000, 1), (500, 500));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(s.net_rx_bps, Some(200.0));
        assert_eq!(
            (
                s.disk_read_bps,
                s.disk_write_bps,
                s.disk_read_iops,
                s.disk_write_iops
            ),
            (None, None, None, None)
        );
        // 경고는 10분에 한 번이다.
        let warned = f.sampler.disk_warned[&id];
        assert!(warned.is_some());
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert_eq!(f.sampler.disk_warned[&id], warned);
        tokio::time::advance(WARN_EVERY).await;
        f.sampler.tick().await;
        assert!(f.sampler.disk_warned[&id] > warned);

        // 다시 읽히면 그다음 점부터 디스크 값이 있다.
        f.hv.fail_disk_stats(id, false);
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(6_000, 1), (500, 500));
        f.sampler.tick().await;
        let last = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!(last.disk_read_bps, Some(1_000.0));
    }

    /// `list()`가 오래 걸려도(응답 없는 VM) 마감은 그 뒤부터라 다른 VM의 점이 생긴다.
    #[tokio::test]
    async fn a_slow_list_does_not_use_up_the_vm_deadline() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(1_000, 1), (0, 0));
        f.hv.delay_lists(VM_TIMEOUT + Duration::from_secs(1));
        f.hv.delay_disk_stats(Duration::from_millis(10));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);
    }

    /// VM 점의 벽시계 `t`는 `list()`가 끝난 뒤의 시각이다(노드 점은 측정 시작 시각).
    #[tokio::test]
    async fn vm_sample_time_is_taken_after_the_list() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        f.probe.set(|v| v.memory = Some((1, 2)));
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        // 실제 시간으로 기다린다(벽시계는 `tokio::time::pause`로 멈추지 않는다).
        f.hv.delay_lists(Duration::from_millis(200));
        f.sampler.tick().await;
        f.sampler.tick().await;
        let node_t = node_samples(&f.store).pop().unwrap().t;
        let vm_t = vm_samples(&f.store, id).pop().unwrap().t;
        assert!(vm_t - node_t >= 200, "node {node_t}, vm {vm_t}");
    }

    /// 점의 시각은 `disk_stats`를 부르기 전이다. QMP가 늦게 답해도 비율은 측정 간격으로 나눈다.
    #[tokio::test]
    async fn a_slow_disk_stats_answer_does_not_skew_the_rates() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(5_000, 5), (0, 0));
        f.hv.delay_disk_stats(Duration::from_millis(900));
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!(s.disk_read_bps, Some(1_000.0));
        assert_eq!(s.cpu, Some(50.0));
    }

    #[tokio::test]
    async fn counters_of_vanished_vms_are_dropped() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        assert!(f.sampler.vms.contains_key(&id));
        f.hv.stop(id, StopMode::Force).await.unwrap();
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert!(f.sampler.vms.is_empty());
        assert!(f.sampler.disk_warned.is_empty());
    }

    /// VM이 클러스터에 없으면(스펙을 모름) vCPU·NIC 수를 몰라 CPU와 네트워크가 `null`이다.
    #[tokio::test]
    async fn a_vm_without_a_spec_has_no_cpu_or_network() {
        let mut f = fixture().await;
        let id = Uuid::now_v7();
        start(&f, id).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 1_000_000_000, disk(5_000, 0), (10, 10));
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!((s.cpu, s.net_rx_bps, s.net_tx_bps), (None, None, None));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(s.disk_read_bps, Some(1_000.0));
    }

    #[tokio::test]
    async fn spawned_sampler_samples_every_five_seconds_until_shut_down() {
        let f = fixture().await;
        tokio::time::pause();
        f.probe.set(|v| v.memory = Some((1, 2)));
        let sampler = MetricsSampler::spawn(
            f.hv.clone(),
            f.probe.clone(),
            f.cluster.clone(),
            PathBuf::from("/"),
            f.store.clone(),
        );
        // 첫 측정은 곧바로(기준만), 그다음은 5초 뒤다.
        tokio::time::sleep(INTERVAL / 2).await;
        assert!(node_samples(&f.store).is_empty());
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(node_samples(&f.store).len(), 1);
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(node_samples(&f.store).len(), 2);
        sampler.shutdown().await;
        tokio::time::sleep(INTERVAL * 2).await;
        assert_eq!(node_samples(&f.store).len(), 2);
    }
}
```

`crates/opld/src/metrics/store.rs`를 만든다:

```rust
//! 지표 저장소. 이 노드의 시리즈 하나와 이 노드에서 잰 VM마다 시리즈 하나를 메모리에 둔다.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

#[cfg(test)]
use opl_types::metrics::{MetricsQuery, MetricsScope};
use opl_types::metrics::{NodeSample, VmSample};
use uuid::Uuid;

/// 시리즈마다 두는 점 수(5초 × 1시간). 넘으면 앞을 버린다.
pub const MAX_SAMPLES: usize = 720;
/// 마지막 점이 이만큼(밀리초) 지난 VM 시리즈는 지운다(옮겨 나간 VM, 지운 VM).
pub const VM_SERIES_TTL_MS: i64 = 3_600_000;

#[derive(Default)]
pub struct MetricsStore {
    inner: Mutex<Series>,
}

#[derive(Default)]
struct Series {
    node: VecDeque<NodeSample>,
    vms: HashMap<Uuid, VecDeque<VmSample>>,
}

/// 저장소에서 읽은 점들(시각 순). 노드 id는 읽는 쪽이 붙인다.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsLocal {
    /// `Nodes`를 물었을 때 이 노드의 점들
    pub node: Option<Vec<NodeSample>>,
    /// `Vm`을 물었을 때 그 VM의 점들(이 노드에서 잰 적이 없으면 `None`)
    pub vm: Option<Vec<VmSample>>,
}

fn push<T>(series: &mut VecDeque<T>, sample: T) {
    if series.len() == MAX_SAMPLES {
        series.pop_front();
    }
    series.push_back(sample);
}

impl MetricsStore {
    pub fn push_node(&self, sample: NodeSample) {
        push(&mut self.inner.lock().unwrap().node, sample);
    }

    pub fn push_vm(&self, vm: Uuid, sample: VmSample) {
        push(
            self.inner.lock().unwrap().vms.entry(vm).or_default(),
            sample,
        );
    }

    /// 마지막 점이 `now`(유닉스 밀리초)보다 [`VM_SERIES_TTL_MS`] 넘게 앞선 VM 시리즈를 지운다.
    pub fn prune(&self, now: i64) {
        self.inner.lock().unwrap().vms.retain(|_, series| {
            series
                .back()
                .is_some_and(|last| now - last.t <= VM_SERIES_TTL_MS)
        });
    }

    /// `since`보다 `t`가 큰 점만 돌려준다.
    // 4단계에서 피어 RPC가 부를 때까지 시험에서만 컴파일한다(바이너리 크레이트라 쓰지 않으면 dead_code다).
    #[cfg(test)]
    pub fn read(&self, q: &MetricsQuery) -> MetricsLocal {
        let after = |t: i64| q.since.is_none_or(|since| t > since);
        let inner = self.inner.lock().unwrap();
        match &q.scope {
            MetricsScope::Nodes => MetricsLocal {
                node: Some(inner.node.iter().filter(|s| after(s.t)).cloned().collect()),
                vm: None,
            },
            MetricsScope::Vm { id } => MetricsLocal {
                node: None,
                vm: inner
                    .vms
                    .get(id)
                    .map(|series| series.iter().filter(|s| after(s.t)).cloned().collect()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(t: i64) -> NodeSample {
        NodeSample {
            t,
            cpu: Some(1.0),
            mem_used_mib: None,
            mem_total_mib: None,
            disk_read_bps: None,
            disk_write_bps: None,
            net_rx_bps: None,
            net_tx_bps: None,
            fs_used_mib: None,
            fs_total_mib: None,
        }
    }

    fn vm(t: i64) -> VmSample {
        VmSample {
            t,
            cpu: Some(1.0),
            mem_rss_mib: None,
            disk_read_bps: None,
            disk_write_bps: None,
            disk_read_iops: None,
            disk_write_iops: None,
            net_rx_bps: None,
            net_tx_bps: None,
        }
    }

    fn query(scope: MetricsScope, since: Option<i64>) -> MetricsQuery {
        MetricsQuery { scope, since }
    }

    fn times<T>(samples: &[T], t: impl Fn(&T) -> i64) -> Vec<i64> {
        samples.iter().map(t).collect()
    }

    #[test]
    fn series_keep_the_last_720_samples() {
        let store = MetricsStore::default();
        let id = Uuid::now_v7();
        for t in 0..MAX_SAMPLES as i64 + 5 {
            store.push_node(node(t));
            store.push_vm(id, vm(t));
        }
        let nodes = store.read(&query(MetricsScope::Nodes, None)).node.unwrap();
        assert_eq!(nodes.len(), MAX_SAMPLES);
        assert_eq!(nodes[0].t, 5);
        assert_eq!(nodes.last().unwrap().t, MAX_SAMPLES as i64 + 4);
        let vms = store
            .read(&query(MetricsScope::Vm { id }, None))
            .vm
            .unwrap();
        assert_eq!(vms.len(), MAX_SAMPLES);
        assert_eq!(vms[0].t, 5);
    }

    #[test]
    fn read_returns_samples_after_since() {
        let store = MetricsStore::default();
        let id = Uuid::now_v7();
        for t in [1_000, 2_000, 3_000] {
            store.push_node(node(t));
            store.push_vm(id, vm(t));
        }
        let all = store.read(&query(MetricsScope::Nodes, None));
        assert_eq!(times(&all.node.unwrap(), |s| s.t), [1_000, 2_000, 3_000]);
        assert_eq!(all.vm, None);
        let after = store.read(&query(MetricsScope::Nodes, Some(2_000)));
        assert_eq!(times(&after.node.unwrap(), |s| s.t), [3_000]);
        let none = store.read(&query(MetricsScope::Nodes, Some(3_000)));
        assert_eq!(none.node, Some(vec![]));

        let vm_after = store.read(&query(MetricsScope::Vm { id }, Some(1_000)));
        assert_eq!(vm_after.node, None);
        assert_eq!(times(&vm_after.vm.unwrap(), |s| s.t), [2_000, 3_000]);
        // 이 노드에서 잰 적이 없는 VM
        let other = store.read(&query(MetricsScope::Vm { id: Uuid::now_v7() }, None));
        assert_eq!(other.vm, None);
    }

    #[test]
    fn prune_drops_vm_series_older_than_an_hour() {
        let store = MetricsStore::default();
        let (old, fresh) = (Uuid::now_v7(), Uuid::now_v7());
        let now = 10_000_000;
        store.push_vm(old, vm(now - VM_SERIES_TTL_MS - 1));
        store.push_vm(fresh, vm(now - VM_SERIES_TTL_MS - 1));
        store.push_vm(fresh, vm(now - VM_SERIES_TTL_MS));
        store.push_node(node(0));
        store.prune(now);
        let read = |id| store.read(&query(MetricsScope::Vm { id }, None)).vm;
        assert_eq!(read(old), None);
        assert_eq!(read(fresh).unwrap().len(), 2);
        // 노드 시리즈는 상한으로만 줄어든다.
        let nodes = store.read(&query(MetricsScope::Nodes, None)).node.unwrap();
        assert_eq!(nodes.len(), 1);
    }
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo check -p opld`
Expected: `Cargo.lock`의 opld 의존성 목록이 바뀐다(새 패키지는 없다, 이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1670개, 실패 0

Run: `cargo test -p opld --features qemu-it --test e2e`
Expected: PASS (실제 QEMU가 필요하다)

- [ ] **Step 4: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(metrics): 측정기와 저장소 — opld metrics/store.rs MetricsStore(노드 시리즈와 VM별 시리즈, 시리즈마다 720점, prune으로 마지막 점이 1시간 지난 VM 시리즈 지움, read는 since보다 뒤의 점만 MetricsLocal{node, vm}로 — 4단계 피어 RPC가 부를 때까지 시험에서만 컴파일), metrics/sampler.rs MetricsSampler::spawn(hv, probe, cluster, data_dir, store)(5초 interval·Skip, 첫 측정은 기준만, pid 바뀜·카운터 줄어듦이면 점 없이 다시 기준, 실행·일시정지 VM마다 프로세스 CPU·RSS와 disk_stats(1초 제한, 실패하면 디스크 필드만 null)와 tap 합(게스트 기준으로 뒤집음, vCPU·NIC 수는 클러스터 VM 스펙), VM 측정 전체 4초, 사라진 VM 카운터 지움, 경고는 VM마다 10분에 한 번), member.rs Services에서 띄우고 끔(위트니스 없음), 노드 기능 metrics, MockHypervisor::fail_disk_stats; tokio::time::pause 시험"
```

---

### Task 4: 지표 피어 RPC와 API

opl-peer `ReadMetrics`(`MetricsReply`, `MetricsSource`, `PeerService::with_metrics`, `Peers::read_metrics` — `Unimplemented`은 `None`), opl-api `GET /metrics/nodes`·`/nodes/{key}/metrics`·`/vms/{key}/metrics`(내 노드는 직접, 다른 노드는 3초 제한 RPC, 꺼진 노드는 묻지 않고 `unreachable`, 옛 노드는 `unsupported`, 위트니스 제외, VM 점은 시각 순으로 합침), `AppState::metrics`, opld `StoreSource`와 `member.rs`에서 저장소 하나를 측정기·피어·API가 나눠 씀. OpenAPI와 `schema.d.ts`. 단일 노드·두 노드·gRPC 시험.

**Files:**
- Create: `crates/opl-api/src/metrics.rs`
- Create: `crates/opl-api/tests/metrics.rs`
- Create: `crates/opl-peer/src/metrics.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/state.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-peer/build.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/lib.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-types/src/metrics.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/src/metrics/store.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
pub mod limit;
mod migration_disks;
```

을 다음으로 바꾼다:

```rust
pub mod limit;
mod metrics;
mod migration_disks;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(drain::drain_node))
        .routes(routes!(cluster::get_cluster))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(drain::drain_node))
        .routes(routes!(metrics::get_node_metrics))
        .routes(routes!(metrics::list_node_metrics))
        .routes(routes!(cluster::get_cluster))
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(vms::migrate_vm))
        .routes(routes!(snapshots::list_snapshots))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(vms::migrate_vm))
        .routes(routes!(metrics::get_vm_metrics))
        .routes(routes!(snapshots::list_snapshots))
```

`crates/opl-api/src/metrics.rs`를 만든다:

```rust
//! 사용량 지표 읽기(설계 §3.3). 지표는 노드마다 메모리에 있으므로, 이 노드는 [`AppState::metrics`]로
//! 직접, 다른 노드는 노드 사이 RPC(`ReadMetrics`)로 읽어 합친다. 위트니스는 VM을 돌리지 않아 묻지 않는다.
//! 답하지 않은 노드는 `unreachable`, 지표를 모르는 옛 노드는 `unsupported`다. 꺼진 것으로 기록된 다른
//! 노드는 묻지 않고(연결 제한을 매번 기다리지 않게) `unreachable`이다.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::types::NodeId;
use opl_peer::MetricsReply;
use opl_types::metrics::{
    MetricsQuery, MetricsScope, NodeMetricsResponse, NodeSeries, VmMetricsResponse,
};
use opl_types::node::NodeSpec;
use opl_types::resource::ResourceSpec;
use opl_types::vm::VmSpec;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::lookup;
use crate::state::AppState;

/// 다른 노드 하나의 지표를 기다리는 최대 시간
const PEER_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// 지표 읽기의 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct MetricsParams {
    /// 이 `t`(그 노드 시계의 유닉스 밀리초)보다 뒤의 점만. 없으면 남은 것 모두(1시간)
    pub since: Option<i64>,
}

/// 노드 하나를 읽은 결과
enum NodeRead {
    Reply(MetricsReply),
    /// 지표를 모르는 노드(옛 노드, 지표 소스가 없는 노드)
    Unsupported,
    /// 답하지 않은 노드와 그 이유
    Unreachable(String),
}

/// 노드 하나의 지표. 이 노드면 직접, 다른 노드면 노드 사이 RPC로 읽는다.
async fn read_node(state: &AppState, id: NodeId, addr: &str, query: MetricsQuery) -> NodeRead {
    if id.0 == state.node {
        return match &state.metrics {
            Some(source) => NodeRead::Reply(source.read(&query)),
            None => NodeRead::Unsupported,
        };
    }
    let Some(peers) = &state.peers else {
        return NodeRead::Unreachable("no node-to-node connection".into());
    };
    let read = tokio::time::timeout(
        PEER_READ_TIMEOUT,
        peers.read_metrics(addr, query, PEER_READ_TIMEOUT),
    )
    .await;
    match read {
        Ok(Ok(Some(reply))) => NodeRead::Reply(reply),
        Ok(Ok(None)) => NodeRead::Unsupported,
        Ok(Err(status)) => NodeRead::Unreachable(status.message().to_string()),
        Err(_) => NodeRead::Unreachable("timed out".into()),
    }
}

/// 위트니스를 뺀 모든 멤버에게 `query`를 묻는다. (노드 id, 이름, 결과)를 돌려준다. Node 리소스가 없는
/// 멤버는 id를 이름으로 쓴다. 꺼진 것으로 기록된 다른 노드는 묻지 않고 닿지 않는 노드로 둔다.
async fn read_all(
    state: &AppState,
    query: MetricsQuery,
) -> Result<Vec<(NodeId, String, NodeRead)>, ApiError> {
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let members = state.cluster.members();
    let reads = members
        .voters
        .iter()
        .chain(&members.learners)
        .filter_map(|(id, addr)| {
            let node = nodes.iter().find(|n| n.id == id.0);
            if node.is_some_and(|n| n.spec.witness) {
                return None;
            }
            let name = node.map_or_else(|| id.0.to_string(), |n| n.name.clone());
            let offline = id.0 != state.node && node.is_some_and(|n| !n.status.online);
            let query = query.clone();
            Some(async move {
                let read = if offline {
                    NodeRead::Unreachable("offline".into())
                } else {
                    read_node(state, *id, addr, query).await
                };
                (*id, name, read)
            })
        });
    Ok(futures_util::future::join_all(reads).await)
}

/// 닿지 않거나 옛 노드면 이름을 목록에 넣고 None, 읽었으면 그 응답
fn sort_out(
    name: String,
    read: NodeRead,
    unreachable: &mut Vec<String>,
    unsupported: &mut Vec<String>,
) -> Option<MetricsReply> {
    match read {
        NodeRead::Reply(reply) => Some(reply),
        NodeRead::Unsupported => {
            unsupported.push(name);
            None
        }
        NodeRead::Unreachable(e) => {
            // 폴링마다 부르므로 경고를 쌓지 않는다.
            tracing::debug!(node = %name, error = %e, "cannot read the metrics of a node");
            unreachable.push(name);
            None
        }
    }
}

/// 모든 노드(위트니스 제외)의 노드 지표. 답하지 않은 노드는 `unreachable`, 지표를 모르는 옛 노드는
/// `unsupported`에 이름이 들어간다.
#[utoipa::path(get, path = "/metrics/nodes", tag = "metrics",
    params(MetricsParams),
    responses((status = 200, body = NodeMetricsResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_node_metrics(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(params): Query<MetricsParams>,
) -> Result<Json<NodeMetricsResponse>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: params.since,
    };
    let mut nodes = Vec::new();
    let mut unreachable = Vec::new();
    let mut unsupported = Vec::new();
    for (id, name, read) in read_all(&state, query).await? {
        if let Some(reply) = sort_out(name.clone(), read, &mut unreachable, &mut unsupported) {
            let samples = reply.node.map(|s| s.samples).unwrap_or_default();
            nodes.push((
                name,
                NodeSeries {
                    node: id.0,
                    samples,
                },
            ));
        }
    }
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    unreachable.sort();
    unsupported.sort();
    Ok(Json(NodeMetricsResponse {
        nodes: nodes.into_iter().map(|(_, series)| series).collect(),
        unreachable,
        unsupported,
    }))
}

/// 노드 하나의 지표. 위트니스면 409 `node_unavailable`, 꺼진 것으로 기록됐거나 닿지 않으면 503
/// `node_unreachable`, 지표를 모르는 옛 노드면 409 `cluster_upgrading`.
#[utoipa::path(get, path = "/nodes/{key}/metrics", tag = "metrics",
    params(("key" = String, Path, description = "node id or name"), MetricsParams),
    responses((status = 200, body = NodeSeries), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_node_metrics(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<MetricsParams>,
) -> Result<Json<NodeSeries>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!(
                "node {:?} is a witness node and runs no workloads",
                node.name
            ),
        ));
    }
    let unreachable = |why: &str| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("node {:?} did not answer: {why}", node.name),
        )
    };
    let members = state.cluster.members();
    let Some((id, addr)) = members
        .voters
        .iter()
        .chain(&members.learners)
        .find(|(id, _)| id.0 == node.id)
    else {
        return Err(unreachable("not a cluster member"));
    };
    if node.id != state.node && !node.status.online {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("node {:?} is offline", node.name),
        ));
    }
    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: params.since,
    };
    match read_node(&state, *id, addr, query).await {
        NodeRead::Reply(reply) => Ok(Json(NodeSeries {
            node: node.id,
            samples: reply.node.map(|s| s.samples).unwrap_or_default(),
        })),
        NodeRead::Unsupported => Err(ApiError::conflict(
            "cluster_upgrading",
            format!("node {:?} does not report metrics yet", node.name),
        )),
        NodeRead::Unreachable(e) => Err(unreachable(&e)),
    }
}

/// VM 하나의 지표. 모든 노드(위트니스 제외)에 물어 시각 순으로 합치므로 마이그레이션 앞뒤 기록이
/// 이어진다. 같은 `t`가 두 노드에서 오면 둘 다 둔다.
#[utoipa::path(get, path = "/vms/{key}/metrics", tag = "metrics",
    params(("key" = String, Path, description = "VM id or name"), MetricsParams),
    responses((status = 200, body = VmMetricsResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_vm_metrics(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<MetricsParams>,
) -> Result<Json<VmMetricsResponse>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let query = MetricsQuery {
        scope: MetricsScope::Vm { id: vm.id },
        since: params.since,
    };
    let mut samples = Vec::new();
    let mut unreachable = Vec::new();
    let mut unsupported = Vec::new();
    for (_, name, read) in read_all(&state, query).await? {
        if let Some(reply) = sort_out(name, read, &mut unreachable, &mut unsupported) {
            samples.extend(
                reply
                    .vms
                    .into_iter()
                    .filter(|s| s.vm == vm.id)
                    .flat_map(|s| s.samples),
            );
        }
    }
    // 안정 정렬이라 같은 `t`는 둘 다 남는다.
    samples.sort_by_key(|s| s.t);
    unreachable.sort();
    unsupported.sort();
    Ok(Json(VmMetricsResponse {
        vm: vm.id,
        samples,
        unreachable,
        unsupported,
    }))
}
```

`crates/opl-api/src/state.rs`에서

```rust
use opl_driver::Hypervisor;
use opl_peer::Peers;
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_driver::Hypervisor;
use opl_peer::{MetricsSource, Peers};
use uuid::Uuid;
```

`crates/opl-api/src/state.rs`에서

```rust
    pub audit: Arc<AuditLog>,
    /// OIDC 영역의 discovery·JWKS(노드 메모리)
```

을 다음으로 바꾼다:

```rust
    pub audit: Arc<AuditLog>,
    /// 이 노드의 지표(opld의 측정기 저장소). 없으면 지표 API가 이 노드를 `unsupported`로 보인다.
    pub metrics: Option<Arc<dyn MetricsSource>>,
    /// OIDC 영역의 discovery·JWKS(노드 메모리)
```

`crates/opl-api/src/state.rs`에서

```rust
            audit: Arc::new(AuditLog::disabled()),
            oidc: OidcCache::default(),
```

을 다음으로 바꾼다:

```rust
            audit: Arc::new(AuditLog::disabled()),
            metrics: None,
            oidc: OidcCache::default(),
```

`crates/opl-api/tests/metrics.rs`를 만든다:

```rust
//! 지표 읽기 API(한 노드): `GET /metrics/nodes`·`/nodes/{key}/metrics`·`/vms/{key}/metrics`의 응답,
//! `since`, 지표 소스가 없는 노드(`unsupported`·409), 위트니스·멤버가 아닌 노드, 권한과 404·400.
//! 다른 노드에서 읽기는 `two_nodes.rs`가 본다.

mod common;

use std::sync::Arc;

use common::{ADMIN_PASSWORD, Env, login_at, vm_request};
use opl_api::{Action, AppState, Authorizer, Principal};
use opl_peer::{MetricsReply, MetricsSource};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Kind, Resource};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// 점은 `t` 1000·2000. `Vm`은 어떤 VM이든 그 VM의 점으로 돌려준다.
struct FakeMetrics {
    node: Uuid,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let times = || {
            [1_000, 2_000]
                .into_iter()
                .filter(|t| q.since.is_none_or(|since| *t > since))
        };
        match q.scope {
            MetricsScope::Nodes => MetricsReply {
                node: Some(NodeSeries {
                    node: self.node,
                    samples: times().map(node_sample).collect(),
                }),
                vms: vec![],
            },
            MetricsScope::Vm { id } => MetricsReply {
                node: None,
                vms: vec![VmSeries {
                    vm: id,
                    node: self.node,
                    samples: times().map(vm_sample).collect(),
                }],
            },
        }
    }
}

fn node_sample(t: i64) -> NodeSample {
    NodeSample {
        t,
        cpu: Some(25.0),
        mem_used_mib: Some(1024),
        mem_total_mib: Some(8192),
        disk_read_bps: None,
        disk_write_bps: None,
        net_rx_bps: Some(10.0),
        net_tx_bps: Some(20.0),
        fs_used_mib: Some(1),
        fs_total_mib: Some(2),
    }
}

fn vm_sample(t: i64) -> VmSample {
    VmSample {
        t,
        cpu: Some(50.0),
        mem_rss_mib: Some(300),
        disk_read_bps: None,
        disk_write_bps: None,
        disk_read_iops: None,
        disk_write_iops: None,
        net_rx_bps: None,
        net_tx_bps: None,
    }
}

/// 모든 것을 거부하는 권한 검사
struct DenyAll;

impl Authorizer for DenyAll {
    fn check(&self, _: &Principal, _: Action, _: Kind) -> bool {
        false
    }
}

/// 같은 노드의 API를 `tweak`한 상태로 하나 더 띄우고 그 주소를 돌려준다.
async fn serve(env: &Env, tweak: impl FnOnce(&mut AppState)) -> String {
    let hv: Arc<dyn opl_driver::Hypervisor> = env.hv.clone();
    let mut state = AppState::new(
        env.cluster.clone(),
        env.node,
        hv,
        env.dir.path().join("uploads2"),
    );
    tweak(&mut state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(Arc::new(state));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn with_metrics(env: &Env) -> String {
    let source = Arc::new(FakeMetrics { node: env.node });
    serve(env, |s| s.metrics = Some(source)).await
}

/// 이름이 `name`인 Node 리소스를 클러스터 멤버로 두지 않고 만든다.
async fn add_node(env: &Env, name: &str, witness: bool) {
    let r: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness,
        },
    );
    env.cluster.create(&r).await.unwrap();
}

fn times(samples: &serde_json::Value) -> Vec<i64> {
    samples
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["t"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn a_viewer_reads_node_metrics() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    env.user("vera", "viewer").await;
    let viewer = login_at(&base, "vera", "password-1").await;

    let (status, body) = viewer.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unreachable"], json!([]));
    assert_eq!(body["unsupported"], json!([]));
    let nodes = body["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1, "{body:#}");
    assert_eq!(nodes[0]["node"], json!(env.node));
    assert_eq!(times(&nodes[0]["samples"]), [1_000, 2_000]);
    assert_eq!(nodes[0]["samples"][0]["cpu"], 25.0);
    assert_eq!(
        nodes[0]["samples"][0]["disk_read_bps"],
        serde_json::Value::Null
    );
    let (_, body) = viewer.get("/metrics/nodes?since=1000").await;
    assert_eq!(times(&body["nodes"][0]["samples"]), [2_000]);

    // 노드 하나: 이름이나 id로
    let (status, body) = viewer.get("/nodes/node1/metrics?since=1000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["node"], json!(env.node));
    assert_eq!(times(&body["samples"]), [2_000]);
    let (status, body) = viewer.get(&format!("/nodes/{}/metrics", env.node)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(times(&body["samples"]), [1_000, 2_000]);
}

#[tokio::test]
async fn vm_metrics_are_read_by_vm_key() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    let admin = login_at(&base, "admin", ADMIN_PASSWORD).await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");

    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["vm"], vm["id"]);
    assert_eq!(times(&body["samples"]), [1_000, 2_000]);
    assert_eq!(body["samples"][0]["mem_rss_mib"], 300);
    assert_eq!(body["unreachable"], json!([]));
    assert_eq!(body["unsupported"], json!([]));
    let id = vm["id"].as_str().unwrap();
    let (status, body) = admin.get(&format!("/vms/{id}/metrics?since=1000")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(times(&body["samples"]), [2_000]);
}

#[tokio::test]
async fn a_node_without_metrics_is_unsupported() {
    // 기본 Env의 API에는 지표 소스가 없다(옛 노드처럼).
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["nodes"], json!([]));
    assert_eq!(body["unsupported"], json!(["node1"]));
    assert_eq!(body["unreachable"], json!([]));

    let (status, body) = admin.get("/nodes/node1/metrics").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert_eq!(
        body["message"],
        "node \"node1\" does not report metrics yet"
    );

    admin.post("/vms", vm_request("web")).await;
    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["samples"], json!([]));
    assert_eq!(body["unsupported"], json!(["node1"]));
}

#[tokio::test]
async fn witness_and_non_member_nodes_are_not_read() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    let admin = login_at(&base, "admin", ADMIN_PASSWORD).await;
    add_node(&env, "w1", true).await;
    add_node(&env, "ghost", false).await;

    let (status, body) = admin.get("/nodes/w1/metrics").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"w1\" is a witness node and runs no workloads"
    );
    let (status, body) = admin.get("/nodes/ghost/metrics").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "node_unreachable");
    assert_eq!(
        body["message"],
        "node \"ghost\" did not answer: not a cluster member"
    );
    // 목록은 클러스터 멤버만 묻는다.
    let (_, body) = admin.get("/metrics/nodes").await;
    assert_eq!(body["nodes"].as_array().unwrap().len(), 1, "{body:#}");
    assert_eq!(body["unreachable"], json!([]));
}

#[tokio::test]
async fn bad_requests_and_missing_resources_are_rejected() {
    let env = Env::start().await;
    let base = with_metrics(&env).await;
    let admin = login_at(&base, "admin", ADMIN_PASSWORD).await;
    for path in [
        "/metrics/nodes?since=yesterday",
        "/nodes/node1/metrics?since=1.5",
        "/vms/web/metrics?since=x",
        "/metrics/nodes?until=5",
    ] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(body["code"], "invalid_request", "{path}");
    }
    for path in ["/nodes/nope/metrics", "/vms/nope/metrics"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        assert_eq!(body["code"], "not_found", "{path}");
    }

    // 로그인하지 않으면 401, 권한이 없으면 403
    let (status, _) = env.anonymous().get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let denied = serve(&env, |s| s.authorizer = Arc::new(DenyAll)).await;
    let admin = login_at(&denied, "admin", ADMIN_PASSWORD).await;
    for path in ["/metrics/nodes", "/nodes/node1/metrics", "/vms/web/metrics"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/nodes/{key}/drain",
        "/vms",
```

을 다음으로 바꾼다:

```rust
        "/nodes/{key}/drain",
        "/nodes/{key}/metrics",
        "/metrics/nodes",
        "/vms/{key}/metrics",
        "/vms",
```

`crates/opl-api/tests/two_nodes.rs` 전체:

```rust
//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기(HA VM 포함), 공유 디스크의 잠금 바꾸기,
//! 로컬 디스크 복사(대상에 만들기, 볼륨 기록 옮기기, 원본 지우기, 실패 때 대상 사본 지우기, 되돌아오는 VM).
//! 노드 비우기: VM을 다른 노드로 옮기고(들어오던 VM도, 로컬 디스크 VM은 디스크를 복사해) 스냅샷이 있는 로컬
//! 디스크 VM은 남기며, 유지보수가 끝나면 멈춘다.
//! 감사 로그: 한 노드의 `GET /audit`가 두 노드의 항목을 합치고, 닿지 않는 노드를 알린다.
//! 지표: 한 노드의 지표 API가 다른 노드의 지표를 읽고, 지표를 모르는 노드와 닿지 않는 노드를 알린다.
//! OIDC: 한 노드에서 시작한 로그인을 다른 노드가 끝낸다(상태 쿠키의 키는 클러스터 CA 키에서).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{
    AppState, AuditConfig, AuditLog, IncomingConfig, IncomingMigrations, TaskRunner,
    UefiVarsFetcher, hash_password,
};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::mock::MOCK_MIRROR_BYTES;
use opl_driver::{
    DriverError, Hypervisor, MigrateTarget, MirrorTarget, MockHypervisor, RuntimeStatus, Storage,
    uefi_vars_path,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, MetricsReply, MetricsSource, PeerService, Peers};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{FirmwareFetcher, HaConfig, HaGuard, Reconciler};
use opl_types::auth::{Role, UserSpec};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use opl_types::node::{FEATURES, Fencing, NodeHa, NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Arch, DiskMove, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use common::{
    ADMIN_PASSWORD, FakeStorage, MOUNT_ROOT, WAIT, login_at, reconciler_config, runner_config,
    vm_request,
};

/// 받는 노드가 마이그레이션을 받는 포트 범위. Mock 하이퍼바이저는 첫 포트를 연다.
const MIGRATION_PORTS: std::ops::RangeInclusive<u16> = 61000..=61009;

struct Node {
    id: Uuid,
    /// Node 리소스와 감사 로그의 노드 이름
    name: String,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 볼륨 호출을 적는 저장소(reconciler, Task 실행기, 마이그레이션 받기가 함께 쓴다)
    storage: Arc<FakeStorage>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    /// 감사 로그를 끈 노드(opld의 `[audit] enabled = false`)
    audit_off: bool,
    /// 지표 소스가 없는 노드(지표를 모르는 옛 노드처럼)
    metrics_off: bool,
    /// 노드 간 서버를 멈춘다.
    peer_stop: Option<tokio::sync::oneshot::Sender<()>>,
    peer_server: Option<tokio::task::JoinHandle<()>>,
    base: String,
    /// reconciler의 HA guard. None이면 펜싱이 꺼진 노드다.
    ha: Option<Arc<HaGuard>>,
    reconciler: Option<Reconciler>,
    runner: Option<TaskRunner>,
    /// Task 실행기를 다시 띄울 때 쓰는 설정
    runner_config: Option<opl_api::TaskRunnerConfig>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir, name: &str) -> Node {
        let id = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(NodeId(id), root.path().join(id.to_string()));
        // 테스트가 동시에 많이 돌면 첫 선거가 몇 초씩 걸린다.
        config.write_timeout = Duration::from_secs(30);
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        Node {
            id,
            name: name.into(),
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            storage: Arc::new(FakeStorage::default()),
            tls,
            listener: Some(listener),
            audit_off: false,
            metrics_off: false,
            peer_stop: None,
            peer_server: None,
            base: String::new(),
            ha: None,
            reconciler: None,
            runner: None,
            runner_config: None,
        }
    }

    /// 노드 간 서버(마이그레이션 받기 포함), reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = self.storage.clone();
        let work = root.path().join(format!("{}-work", self.id));
        let incoming = IncomingMigrations::new(
            self.cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node: self.id,
                mount_root: MOUNT_ROOT.into(),
                storage: storage.clone(),
                listen_host: "127.0.0.1".into(),
                ports: MIGRATION_PORTS,
                // Task 실행기(`runner_config`)와 같다.
                migration_disks: work.join("migration-disks"),
            },
        );
        // opld처럼 API와 노드 간 서버가 같은 감사 로그를 쓴다.
        let audit = Arc::new(if self.audit_off {
            AuditLog::disabled()
        } else {
            AuditLog::open(AuditConfig {
                dir: work.join("audit"),
                node: self.name.clone(),
                max_file_size: 1 << 20,
                max_files: 2,
            })
            .unwrap()
        });
        // opld처럼 API와 노드 간 서버가 같은 지표 저장소를 읽는다.
        let metrics: Option<Arc<dyn MetricsSource>> = (!self.metrics_off).then(|| {
            // 두 노드의 VM 점 시각이 엇갈려야 합친 뒤의 정렬이 보인다.
            let vm_times = if self.name == "node1" {
                vec![1_000, 3_000]
            } else {
                vec![2_000]
            };
            Arc::new(FakeMetrics {
                node: self.id,
                vm_times,
            }) as Arc<dyn MetricsSource>
        });
        let mut service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(work.join("state"))
            .with_audit(audit.clone());
        if let Some(metrics) = &metrics {
            service = service.with_metrics(metrics.clone());
        }
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        self.peer_stop = Some(stop);
        self.peer_server = Some(tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        }));
        // UEFI 변수 파일을 가진 노드에서 받아 온다(opld처럼 reconciler와 Task 실행기가 함께 쓴다).
        // 시험의 변수 파일은 모두 템플릿과 같은 6바이트다.
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(template(root, self), b"vars 0").unwrap();
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            self.cluster.clone(),
            self.peers.clone(),
            work.join("state"),
            template(root, self),
        ));
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        reconciler_config.firmware = Some(firmware.clone());
        reconciler_config.ha = self.ha.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut state = AppState::new(
            self.cluster.clone(),
            self.id,
            hv.clone(),
            work.join("uploads"),
        );
        state.peers = Some(self.peers.clone());
        state.audit = audit;
        state.metrics = metrics;
        let state = Arc::new(state);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.firmware = Some(firmware);
        // opld처럼 노드 비우기 Task가 API 상태를 쓴다.
        config.api = Some(state.clone());
        self.reconciler = Some(reconciler);
        self.runner_config = Some(config);
        self.start_runner().await;
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(state);
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
    }

    /// Task 실행기를 멈춘다(opld가 내려간 것처럼 실행 중인 Task도 멈춘다).
    async fn stop_runner(&mut self) {
        self.runner.take().unwrap().shutdown().await;
    }

    /// Task 실행기를 (다시) 띄운다. 시작할 때 남은 마이그레이션을 정리한다.
    async fn start_runner(&mut self) {
        self.runner = Some(
            TaskRunner::spawn(
                self.cluster.clone(),
                self.storage.clone(),
                self.hv.clone(),
                self.reconciler.as_ref().unwrap().handle(),
                self.runner_config.clone().unwrap(),
            )
            .await,
        );
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    two_nodes_with(None, true, true).await
}

/// `ha`가 있으면 A의 reconciler에 넣고 두 노드를 HA 가능 노드(`process`)로 등록한다.
/// `b_audit`가 거짓이면 B는 감사 로그를 끈 노드, `b_metrics`가 거짓이면 지표 소스가 없는 노드다.
async fn two_nodes_with(
    ha: Option<Arc<HaGuard>>,
    b_audit: bool,
    b_metrics: bool,
) -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root, "node1").await;
    let mut b = Node::open(&ca, &root, "node2").await;
    let node_ha = ha.as_ref().map(|_| NodeHa {
        fencing: Fencing::Process,
        recover_after_secs: 60,
    });
    a.ha = ha;
    b.audit_off = !b_audit;
    b.metrics_off = !b_metrics;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for node in [&a, &b] {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            node.name.clone(),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id;
        r.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
            ha: node_ha,
            underlay: None,
            firewall: false,
            version: None,
            updates: None,
            // 노드가 등록할 때처럼 이 빌드의 기능을 적는다(노드 비우기).
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
        };
        // 리더가 뽑힐 때까지 쓰기가 기다린다.
        a.cluster.create(&r).await.unwrap();
    }
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: Role::Admin,
            session_epoch: 0,
            realm: None,
            subject: None,
        },
    );
    a.cluster.create(&admin).await.unwrap();
    (root, a, b)
}

#[tokio::test]
async fn console_of_a_vm_on_another_node_is_relayed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    b.hv.set_console_dir(tmp.path());

    // 노드 A의 API로 노드 B에 VM을 만들고 켠다. Task는 B의 실행기가 끝낸다.
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(b.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = b
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert!(a.hv.inspect(vm.id).await.unwrap().is_none());

    let listener =
        tokio::net::UnixListener::bind(tmp.path().join(format!("{}.vnc", vm.id))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });
    let url = format!("{}/api/v1/vms/web/console", a.base).replacen("http", "ws", 1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", admin.token.as_ref().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let greeting = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    let echo = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::binary(b"ping".to_vec()));

    // A의 API로 만든 VM은 B에서도 곧 보인다(같은 저장소).
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let deadline = Instant::now() + WAIT;
    while admin_b.get("/vms/web").await.0 != StatusCode::OK {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 노드의 드라이버 `state_dir`(Task 실행기 설정과 노드 간 서버가 같은 곳을 쓴다)
fn state_dir(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path().join(format!("{}-work", node.id)).join("state")
}

/// 노드의 UEFI 변수 템플릿(받은 변수 파일의 크기를 이와 견준다)
fn template(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path()
        .join(format!("{}-work", node.id))
        .join("vars-template.fd")
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
    let (root, a, b) = two_nodes().await;
    let vm = Uuid::now_v7();
    let src = uefi_vars_path(&state_dir(&root, &b), vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars b").unwrap();
    let fetcher = UefiVarsFetcher::new(
        a.cluster.clone(),
        a.peers.clone(),
        state_dir(&root, &a),
        template(&root, &a),
    );
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 크기가 이 노드의 템플릿과 다르면(다른 펌웨어 빌드) 받은 것을 버린다.
    std::fs::write(&src, b"other firmware").unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert!(err.0.contains("14 bytes"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 파일이 없거나 멤버가 아닌 노드면 실패하고 이 노드의 파일은 그대로다.
    let err = fetcher.fetch(Uuid::now_v7(), b.id).await.unwrap_err();
    assert!(err.0.contains("no UEFI variables"), "{err}");
    let err = fetcher.fetch(vm, Uuid::now_v7()).await.unwrap_err();
    assert!(err.0.contains("not a cluster member"), "{err}");

    // 오프라인으로 기록된 노드에서는 받지 않는다.
    std::fs::write(&src, b"newer").unwrap();
    let mut node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    node.status.online = false;
    a.cluster
        .update_status::<NodeSpec>(b.id, &node.status)
        .await
        .unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert_eq!(err.0, "node \"node2\" is offline");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");
}

/// 노드의 VM `vm` UEFI 변수 파일에 쓴다.
fn write_vars(root: &TempDir, node: &Node, vm: Uuid, data: &[u8]) {
    let path = uefi_vars_path(&state_dir(root, node), vm);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

fn read_vars(root: &TempDir, node: &Node, vm: Uuid) -> Vec<u8> {
    std::fs::read(uefi_vars_path(&state_dir(root, node), vm)).unwrap()
}

/// 멈춘 UEFI VM을 다른 노드로 옮기면, 그 노드의 스냅샷·복제·시작이 앞 노드의 변수를 받아 쓴다.
/// 되돌린 변수는 그 노드의 것이 된다(다시 받지 않는다).
#[tokio::test]
async fn variables_follow_a_stopped_vm_to_another_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    write_vars(&root, &a, vm.id, b"vars 1");

    // 멈춘 채 B로 옮긴다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = serde_json::json!(b.id);
    let (status, body) = admin
        .put("/vms/web", serde_json::json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 스냅샷은 B에서 찍고 A의 변수를 받아 담는다.
    let (status, task) = admin
        .post("/vms/web/snapshots", serde_json::json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    let s1 = b
        .cluster
        .list::<opl_types::snapshot::SnapshotSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|s| s.spec.vm == vm.id)
        .unwrap();
    let copy = state_dir(&root, &b).join(format!("vm/{}/snapshots/{}.vars", vm.id, s1.id));
    assert_eq!(std::fs::read(copy).unwrap(), b"vars 1");

    // 복제도 B에서 A의 변수를 받아 복사하고, 새 VM의 변수는 B에 있다.
    write_vars(&root, &a, vm.id, b"vars 2");
    let (status, task) = admin
        .post("/vms/web/clone", serde_json::json!({ "name": "web-2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = b.cluster.get_by_name::<VmSpec>("web-2").unwrap().unwrap();
    assert_eq!(read_vars(&root, &b, copy.id), b"vars 2");
    assert_eq!(copy.status.firmware_node, Some(b.id));

    // 켜면 B의 reconciler가 A의 변수를 받아 켜고 B를 변수 노드로 남긴다.
    write_vars(&root, &a, vm.id, b"vars 3");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 3");
    wait_vm(&b, vm.id, "variables on B", |v| {
        v.status.firmware_node == Some(b.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // 되돌리기는 스냅샷 때의 변수를 B에 쓰고 B를 변수 노드로 삼는다. 다음 시작은 A에서 받지 않는다.
    let mut moved = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    moved.status.firmware_node = Some(a.id);
    b.cluster
        .update_status::<VmSpec>(vm.id, &moved.status)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reverted = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(reverted.status.firmware_node, Some(b.id));
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 1");
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
async fn running_vm(a: &Node, admin: &common::Client) -> opl_types::vm::Vm {
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap()
}

async fn migrate(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

async fn runtime_status(node: &Node, id: Uuid) -> Option<RuntimeStatus> {
    node.hv.inspect(id).await.unwrap().map(|s| s.status)
}

/// 두 노드에서 VM이 원하는 대로 보일 때까지 기다린다.
async fn wait_vm(node: &Node, id: Uuid, what: &str, pred: impl Fn(&opl_types::vm::Vm) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let vm = node.cluster.get::<VmSpec>(id).unwrap().unwrap();
        if pred(&vm) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {:?}", vm.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 확정 전에 실패한 마이그레이션 뒤: VM은 A에 남아 돌고, B에는 받는 QEMU가 없다.
async fn assert_still_on_a(a: &Node, b: &Node, id: Uuid) {
    let vm = a.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(a.id));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.node, Some(a.id));
    assert_eq!(vm.status.migration, None);
    assert_eq!(runtime_status(a, id).await, Some(RuntimeStatus::Running));
    assert_eq!(runtime_status(b, id).await, None);
    assert!(!b.hv.is_incoming(id));
}

#[tokio::test]
async fn running_vm_migrates_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    // B의 reconciler가 넘겨받으며 `firmware_node`를 쓰기 전에, 확정 뒤 기록이 B를 쓰는지 본다.
    let b_reconciler = b.reconciler.as_ref().unwrap().handle();
    b_reconciler.begin_migration(vm.id).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = migrate(&admin_b, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], a.id.to_string());

    // A의 QEMU는 B의 클러스터 주소의 호스트, B가 연 포트로 보냈다.
    assert_eq!(
        a.hv.migrations(),
        vec![(
            vm.id,
            MigrateTarget {
                host: "127.0.0.1".into(),
                port: *MIGRATION_PORTS.start(),
                tls_hostname: "127.0.0.1".into(),
            }
        )]
    );
    let vm_now = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(vm_now.spec.node, Some(b.id));
    assert_eq!(vm_now.status.node, Some(b.id));
    assert_eq!(vm_now.status.migration, None);
    // QEMU가 변수를 B로 옮겼다.
    assert_eq!(vm_now.status.firmware_node, Some(b.id));
    assert_eq!(runtime_status(&a, vm.id).await, None);
    b_reconciler.end_migration(vm.id);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id) && v.status.pid.is_some()
    })
    .await;
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(b.hv.launches().is_empty(), "{:?}", b.hv.launches());

    // 옮긴 VM은 B에서 다룬다: 끄면 B의 QEMU가 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

#[tokio::test]
async fn failed_send_keeps_the_vm_running_on_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration(
        "Failed to connect to '127.0.0.1:61000': Connection refused".into(),
    ));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("Connection refused"), "{error}");
    assert_still_on_a(&a, &b, vm.id).await;
    // 다른 동작이 다시 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 원본은 다 보냈지만(`postmigrate`) 대상이 받기에 실패했다: 확정하지 않고 원본을 이어서 실행한다.
#[tokio::test]
async fn failed_receive_resumes_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("TLS handshake failed"), "{error}");
    assert_eq!(a.hv.migrations().len(), 1);
    assert_still_on_a(&a, &b, vm.id).await;
}

/// 정해 둔 대로 곧바로 갱신에 성공하거나 실패한다.
struct FakeRenewer(std::sync::Mutex<bool>);

#[async_trait::async_trait]
impl LeaseRenewer for FakeRenewer {
    async fn renew_lease(&self, _timeout: Duration) -> Result<(), opl_cluster::ClusterError> {
        if *self.0.lock().unwrap() {
            Ok(())
        } else {
            Err(opl_cluster::ClusterError::Timeout)
        }
    }
}

/// A에 가짜 리스의 HA guard(`process`, 리스는 `fence_after` 뒤에 끝난다)를 두고 HA VM `web`을 켠다.
async fn ha_vm_on_a(
    fence_after: Duration,
) -> (TempDir, Node, Node, Arc<FakeRenewer>, opl_types::vm::Vm) {
    let renewer = Arc::new(FakeRenewer(std::sync::Mutex::new(true)));
    let guard = HaGuard::spawn(
        renewer.clone(),
        HaConfig {
            fence_after,
            renew_interval: Duration::from_millis(20),
            watchdog: None,
        },
    );
    let (root, a, b) = two_nodes_with(Some(guard), true, true).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["ha"] = serde_json::json!(true);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    wait_vm(&a, vm.id, "running", |v| v.status.phase == VmPhase::Running).await;
    (root, a, b, renewer, vm)
}

/// 되돌린 HA VM의 원본은 reconciler가 리스를 확인한 뒤 이어서 실행한다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_resumes_the_source_with_the_lease() {
    let (_root, a, b, _renewer, vm) = ha_vm_on_a(Duration::from_secs(60)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let deadline = Instant::now() + WAIT;
    while runtime_status(&a, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_still_on_a(&a, &b, vm.id).await;
    assert_eq!(a.hv.launches().len(), 1);
}

/// 마이그레이션하는 동안 리스가 끝났으면 되돌려도 원본을 이어서 실행하지 않고 끈다. 리스가 돌아오면
/// 다시 켠다.
#[tokio::test]
async fn failed_receive_of_an_ha_vm_without_the_lease_stops_the_source() {
    let (_root, a, b, renewer, vm) = ha_vm_on_a(Duration::from_millis(500)).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    a.hv.delay_migrations(Duration::from_millis(1500));
    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    *renewer.0.lock().unwrap() = false;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    wait_vm(&a, vm.id, "fenced", |v| {
        v.status.last_error.as_deref() == Some("fenced: lost the cluster majority")
    })
    .await;
    assert_eq!(runtime_status(&a, vm.id).await, None);
    assert!(
        !a.hv.pause_calls(vm.id).contains(&"resume"),
        "{:?}",
        a.hv.pause_calls(vm.id)
    );

    *renewer.0.lock().unwrap() = true;
    wait_vm(&a, vm.id, "running again", |v| {
        v.status.phase == VmPhase::Running && v.status.last_error.is_none()
    })
    .await;
    assert_eq!(a.hv.launches().len(), 2);
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

/// 원본 QEMU가 보내는 중인데 기다리기가 실패했다: 되돌리며 그 마이그레이션을 취소한다. 나중에 끝나
/// 원본을 멈추지 않는다.
#[tokio::test]
async fn unfinished_send_is_cancelled_by_the_rollback() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!a.hv.is_outgoing(vm.id));
    a.hv.complete_outgoing(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_still_on_a(&a, &b, vm.id).await;
}

/// API 검사를 거치지 않고 Task를 만든다(검사와 마이그레이션 기록 사이에 들어온 요청).
async fn put_task(node: &Node, op: TaskOp, target: Uuid, on: Uuid) -> Uuid {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: on,
        },
    );
    task.name = task.id.to_string();
    node.cluster.create(&task).await.unwrap();
    task.id
}

/// Task가 끝날 때까지 기다린다.
async fn finished_task(node: &Node, id: Uuid) -> Task {
    let deadline = Instant::now() + WAIT;
    loop {
        let task = node.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        if task.status.state.is_finished() {
            return task;
        }
        assert!(Instant::now() < deadline, "task {id}: {:?}", task.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// API가 마이그레이션을 받은 뒤 같은 VM의 스냅샷·디스크 크기 Task가 들어왔다: 원본 Task가 기록한 뒤
/// 다시 보고 되돌린다.
#[tokio::test]
async fn task_started_alongside_stops_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    // 실행하는 노드가 없어 끝나지 않는다.
    let elsewhere = Uuid::now_v7();
    let others = [
        (
            TaskOp::SnapshotCreate {
                snapshot: Uuid::now_v7(),
            },
            vm.id,
            "snapshotted",
        ),
        (
            TaskOp::VolumeResize {
                size_bytes: 2 << 20,
            },
            volume,
            "resized",
        ),
    ];
    for (op, target, what) in others {
        let other = put_task(&a, op, target, elsewhere).await;
        let migrate = put_task(
            &a,
            TaskOp::VmMigrate {
                node: b.id,
                pool: None,
            },
            vm.id,
            a.id,
        )
        .await;
        let task = finished_task(&a, migrate).await;
        assert_eq!(task.status.state, TaskState::Failed, "{:?}", task.status);
        let error = task.status.error.unwrap();
        assert!(error.contains(what), "{error}");
        assert!(a.hv.migrations().is_empty());
        assert_still_on_a(&a, &b, vm.id).await;
        a.cluster.delete::<TaskSpec>(other, None).await.unwrap();
    }
}

/// 보내는 동안 VM을 껐다(검사와 기록 사이에 들어온 정지): 확정하지 않는다.
#[tokio::test]
async fn vm_stopped_during_the_migration_is_not_committed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    wait_vm(&a, vm.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let current = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.power = PowerState::Stopped;
    a.cluster
        .update_spec(vm.id, None, current.labels, &spec)
        .await
        .unwrap();

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("stopped"), "{error}");
    let got = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(got.spec.node, Some(a.id));
    assert_eq!(runtime_status(&b, vm.id).await, None);
    // A의 reconciler가 원하는 대로 끈다.
    wait_vm(&a, vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped).await;
}

#[tokio::test]
async fn different_qemu_versions_do_not_migrate() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.set_qemu_version("9.0.0");
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("QEMU 9.0.0"), "{error}");
    assert!(a.hv.migrations().is_empty());
    assert_still_on_a(&a, &b, vm.id).await;
}

/// A에서 도는 VM `web`. 디스크는 공유 dir 풀 `nas`의 볼륨 `root`다. 볼륨 id를 함께 돌려준다.
async fn running_vm_with_disk(a: &Node, admin: &common::Client) -> (opl_types::vm::Vm, Uuid) {
    let pool: StoragePool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/nonexistent/opl-nas".into(),
            },
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
    a.cluster.create(&pool).await.unwrap();
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["disks"] = serde_json::json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    (vm, volume.id)
}

/// 노드의 볼륨 `id`에 대한 공유·배타·끄기 호출(차례대로)
fn lock_calls(node: &Node, id: Uuid) -> Vec<&'static str> {
    node.storage
        .calls()
        .into_iter()
        .filter(|(op, v)| *v == id && ["share", "exclusive", "deactivate"].contains(op))
        .map(|(op, _)| op)
        .collect()
}

/// 공유 디스크(공유 LVM이면 LV 잠금): 원본은 보내기 전에 공유로 바꾸고 확정 뒤 끈다. 대상은 공유로
/// 켜서 받고, 넘겨받은 뒤 원본이 끌 때까지(일시 오류) 다시 시도해 배타로 바꾼다.
#[tokio::test]
async fn shared_disks_are_shared_during_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    assert!(lock_calls(&a, volume).is_empty());

    b.storage.hold_exclusives(1);
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(lock_calls(&a, volume), ["share", "deactivate"]);
    let deadline = Instant::now() + WAIT;
    while lock_calls(&b, volume).len() < 3 {
        assert!(
            Instant::now() < deadline,
            "B did not make the disk exclusive: {:?}",
            lock_calls(&b, volume)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(lock_calls(&b, volume), ["share", "exclusive", "exclusive"]);
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    // 한 번 배타로 바꾼 뒤에는 다시 하지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&b, volume).len(), 3);
}

/// 확정 전 실패: 대상은 받으려고 켠 디스크를 끄고, 원본은(보류가 풀린 reconciler가) 다시 배타로
/// 바꾼다.
#[tokio::test]
async fn failed_migration_makes_shared_disks_exclusive_again() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration("Connection refused".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_still_on_a(&a, &b, vm.id).await;
    let deadline = Instant::now() + WAIT;
    while lock_calls(&a, volume).len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", lock_calls(&a, volume));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&a, volume), ["share", "exclusive"]);
    assert_eq!(lock_calls(&b, volume), ["share", "deactivate"]);
}

/// 노드마다 따로인 로컬 디스크의 크기
const LOCAL_SIZE: u64 = 1 << 20;

/// A에서 도는 VM `web`. 디스크는 로컬 디렉터리 풀 `local`(노드마다 따로 있는 같은 이름의 풀)의 볼륨
/// `root`(qcow2)이고 그 파일은 A에 있다. 볼륨 id를 함께 돌려준다.
async fn running_vm_with_local_disk(a: &Node, admin: &common::Client) -> (opl_types::vm::Vm, Uuid) {
    dir_pool(a, "local", false).await;
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: LOCAL_SIZE,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    a.storage.place(volume.id, LOCAL_SIZE);
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["disks"] = serde_json::json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    (vm, volume.id)
}

/// 노드의 마이그레이션 디스크 기록 파일(`runner_config`의 디렉터리)
fn disk_record(root: &TempDir, node: &Node, task: &serde_json::Value) -> std::path::PathBuf {
    root.path()
        .join(format!("{}-work", node.id))
        .join("migration-disks")
        .join(format!("{}.json", task["id"].as_str().unwrap()))
}

fn volume_of(a: &Node, id: Uuid) -> Volume {
    a.cluster.get::<VolumeSpec>(id).unwrap().unwrap()
}

/// 로컬 디스크는 B에 같은 id로 새로 만들어 미러로 복사하고(NBD 포트, 내보내기 `disk0`), 스위치오버에서
/// 미러를 끝낸다. 확정 뒤 볼륨 기록이 B로 옮겨지고 A의 원본 파일과 기록이 지워진다. 메모리 단계에
/// 들어가면 Task에 진행률이 보이고 끝나면 지운다.
#[tokio::test]
async fn local_disks_are_copied_to_the_other_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    let deadline = Instant::now() + WAIT;
    let progress = loop {
        let t = a.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        if let Some(p) = t
            .status
            .progress
            .clone()
            .filter(|p| p.stage == "migrating-memory")
        {
            break p;
        }
        assert!(Instant::now() < deadline, "no progress: {:?}", t.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(progress.done_bytes, MOCK_MIRROR_BYTES);
    assert_eq!(progress.total_bytes, MOCK_MIRROR_BYTES);
    // 복사하는 동안 VM 기록에 계획이 있다.
    let migration = a
        .cluster
        .get::<VmSpec>(vm.id)
        .unwrap()
        .unwrap()
        .status
        .migration
        .unwrap();
    assert_eq!(
        migration.disks,
        [DiskMove {
            volume,
            pool: "local".into(),
            format: ImageFormat::Qcow2,
        }]
    );
    assert!(disk_record(&root, &a, &task).exists());
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(task["status"].get("progress").is_none(), "{task}");

    assert_eq!(
        a.hv.mirrors(),
        [(
            vm.id,
            vec![MirrorTarget {
                disk: 0,
                host: "127.0.0.1".into(),
                port: MIGRATION_PORTS.start() + 1,
                export: "disk0".into(),
                tls_hostname: "127.0.0.1".into(),
                format: ImageFormat::Qcow2,
            }]
        )]
    );
    assert_eq!(a.hv.switchovers(), [vm.id]);
    let moved = volume_of(&a, volume);
    assert_eq!(moved.status.node, Some(b.id));
    assert_eq!(moved.spec.pool, "local");
    assert_eq!(moved.spec.format, ImageFormat::Qcow2);
    assert!(b.storage.has_file(volume));
    assert!(b.storage.deleted().is_empty());
    assert!(!a.storage.has_file(volume));
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(!disk_record(&root, &a, &task).exists());
    // B의 reconciler가 넘겨받아(NBD 내보내기를 멈추고) 이어서 실행한다.
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(b.hv.exports(vm.id).is_empty());
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
}

/// `pool`을 주면 로컬 디스크를 B의 그 풀에 만든다. 디스크를 끌 때는 그 노드가 실제로 가진 풀을 쓴다:
/// B는 계획의 풀, 확정 뒤 A는 원래 풀(볼륨 기록은 이미 B의 풀이다).
#[tokio::test]
async fn local_disks_go_to_the_given_pool() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;
    dir_pool(&a, "fast", false).await;
    let pool_of = |node: &Node| -> Vec<String> {
        let calls = node.storage.deactivations().into_iter();
        calls
            .filter(|(_, id)| *id == volume)
            .map(|(p, _)| p)
            .collect()
    };

    // 받기 확인이 실패하면 B는 받으려고 켠 사본을 계획의 풀에서 끈다.
    b.hv.fail_next_incoming_wait(DriverError::Migration("load of migration failed".into()));
    let (status, task) = admin
        .post(
            "/vms/web/migrate",
            serde_json::json!({ "node": "node2", "pool": "fast" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!pool_of(&b).is_empty());
    assert!(pool_of(&b).iter().all(|p| p == "fast"), "{:?}", pool_of(&b));
    assert!(pool_of(&a).is_empty());

    let (status, task) = admin
        .post(
            "/vms/web/migrate",
            serde_json::json!({ "node": "node2", "pool": "fast" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["pool"], "fast");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let moved = volume_of(&a, volume);
    assert_eq!(moved.spec.pool, "fast");
    assert_eq!(moved.status.node, Some(b.id));
    assert!(b.storage.has_file(volume));
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(!pool_of(&a).is_empty());
    assert!(
        pool_of(&a).iter().all(|p| p == "local"),
        "{:?}",
        pool_of(&a)
    );
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
    })
    .await;
}

/// 다음 마이그레이션 하나를 실패하게 한다.
type Inject<'a> = Box<dyn Fn() + 'a>;

/// 복사하는 마이그레이션이 단계마다 실패한다: VM은 A에서 계속 돌고, 볼륨 기록은 그대로이며, B에
/// 만든 디스크는 지워진다(B에 이미 있던 같은 볼륨은 지우지 않는다). A의 기록 파일도 지워진다.
#[tokio::test]
async fn failed_disk_copies_are_deleted_on_the_target() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;
    let local = a
        .cluster
        .get_by_name::<StoragePoolSpec>("local")
        .unwrap()
        .unwrap();
    let set_space = |available: Option<u64>| {
        let cluster = a.cluster.clone();
        let (b_id, local_id) = (b.id, local.id);
        async move {
            let mut node = cluster.get::<NodeSpec>(b_id).unwrap().unwrap();
            match available {
                Some(available_bytes) => node.status.pools.insert(
                    "local".into(),
                    PoolState {
                        pool_id: local_id,
                        ready: true,
                        error: None,
                        total_bytes: 1 << 40,
                        available_bytes,
                    },
                ),
                None => node.status.pools.remove("local"),
            };
            cluster
                .update_status::<NodeSpec>(b_id, &node.status)
                .await
                .unwrap();
        }
    };

    // 공간: B가 알린 남은 공간보다 할당량이 크다. B를 부르지 않는다.
    a.storage.set_allocated(volume, LOCAL_SIZE);
    set_space(Some(1000)).await;
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "not enough space in storage pool \"local\" on node \"node2\": need 1048576, available 1000"
    );
    assert!(b.storage.deleted().is_empty());
    assert!(!disk_record(&root, &a, &task).exists());
    set_space(None).await;

    let failures: [(&str, Inject); 4] = [
        (
            "would change size on pool \"local\" (1048576 → 4194304 bytes)",
            Box::new(|| b.storage.round_creates(4 << 20)),
        ),
        (
            "NBD server closed the connection",
            Box::new(|| {
                a.hv.fail_next_mirror(DriverError::Migration(
                    "disk mirror opl-mirror-0 failed: NBD server closed the connection".into(),
                ))
            }),
        ),
        (
            "disk mirror opl-mirror-0 failed: Input/output error",
            Box::new(|| a.hv.fail_next_switchover("Input/output error")),
        ),
        (
            "load of migration failed",
            Box::new(|| {
                b.hv.fail_next_incoming_wait(DriverError::Migration(
                    "load of migration failed".into(),
                ))
            }),
        ),
    ];
    for (n, (why, inject)) in failures.iter().enumerate() {
        inject();
        let task = migrate(&admin, "node2").await;
        b.storage.round_creates(0);
        assert_eq!(task["status"]["state"], "failed", "{task}");
        let error = task["status"]["error"].as_str().unwrap();
        assert!(error.contains(why), "{error}");
        assert_still_on_a(&a, &b, vm.id).await;
        assert!(!b.storage.has_file(volume), "{why}");
        assert_eq!(b.storage.deleted().len(), n + 1, "{why}");
        assert!(a.storage.has_file(volume));
        assert!(a.storage.deleted().is_empty());
        assert!(!disk_record(&root, &a, &task).exists(), "{why}");
        assert!(!disk_record(&root, &b, &task).exists(), "{why}");
        let current = volume_of(&a, volume);
        assert_eq!(current.status.node, Some(a.id));
        assert_eq!(current.spec.pool, "local");
        assert!(a.hv.mirroring(vm.id).is_empty());
    }

    // B에 같은 볼륨이 이미 있다(공유인 풀을 로컬로 설정했다): 그 파일은 지우지 않는다.
    b.storage.place(volume, LOCAL_SIZE);
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.contains(
            "volume \"root\" already exists on node \"node2\" in pool \"local\"; is the pool really node-local?"
        ),
        "{error}"
    );
    assert!(b.storage.has_file(volume));
    assert_eq!(b.storage.deleted().len(), failures.len());
    assert_still_on_a(&a, &b, vm.id).await;
    // 다시 하면 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// A → B로 옮긴 뒤 A의 원본을 지우지 못했다(남은 사본과 기록). VM이 A로 되돌아오면 A가 받기 전에
/// 기록대로 남은 사본을 지우고 새로 만든다. B는 자기 사본을 지운다.
#[tokio::test]
async fn a_vm_coming_back_replaces_the_copy_left_on_its_old_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    a.storage.fail_deletes_unmounted(1);
    let first = migrate(&admin, "node2").await;
    assert_eq!(first["status"]["state"], "succeeded", "{first}");
    assert!(a.storage.has_file(volume));
    assert!(disk_record(&root, &a, &first).exists());
    assert_eq!(volume_of(&a, volume).status.node, Some(b.id));
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
    })
    .await;

    let back = migrate(&admin, "node1").await;
    assert_eq!(back["status"]["state"], "succeeded", "{back}");
    assert_eq!(back["spec"]["node"], b.id.to_string());
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(a.storage.has_file(volume));
    assert!(!disk_record(&root, &a, &first).exists());
    assert!(!b.storage.has_file(volume));
    assert!(b.storage.deleted().contains(&volume));
    assert_eq!(volume_of(&a, volume).status.node, Some(a.id));
    wait_vm(&a, vm.id, "running on A", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(a.id)
    })
    .await;
}

/// 원본이 확정한 뒤 마무리 전에 A의 실행기가 멈췄다(opld가 내려감): B의 reconciler가 넘겨받으며
/// 볼륨 기록을 B로 고치고 표시를 지운다. A의 실행기가 다시 뜨면 정리가 남은 원본 QEMU를 끄고 기록
/// 파일대로 원본 볼륨을 지운다. B의 사본은 그대로다.
#[tokio::test]
async fn source_stopping_after_the_commit_is_finished_by_both_nodes() {
    let (root, mut a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    b.hv.hold_next_incoming_wait();
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    b.hv.incoming_wait_held().await;
    // 받기를 확인한 원본 Task의 확정 쓰기(`commit`)와 같다. 그 직후 마무리 전에 실행기가 멈춘다.
    let current = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.node = Some(b.id);
    a.cluster
        .update_spec(vm.id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    a.stop_runner().await;
    b.hv.release_incoming_wait();

    wait_vm(&b, vm.id, "taken over by B", |v| {
        v.status.migration.is_none()
            && v.status.phase == VmPhase::Running
            && v.status.node == Some(b.id)
    })
    .await;
    let moved = volume_of(&a, volume);
    assert_eq!(moved.status.node, Some(b.id));
    assert_eq!(moved.spec.pool, "local");
    // A는 아직 원본을 지우지 않았다.
    assert!(a.storage.has_file(volume));
    assert!(disk_record(&root, &a, &task).exists());

    a.start_runner().await;
    until("source volume deleted", || !a.storage.has_file(volume)).await;
    assert_eq!(a.storage.deleted(), [volume]);
    assert!(!disk_record(&root, &a, &task).exists());
    assert_eq!(runtime_status(&a, vm.id).await, None);
    assert!(b.storage.has_file(volume));
    assert!(b.storage.deleted().is_empty());
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("interrupted"), "{error}");
}

/// 디스크를 복사하는 중에 A의 실행기가 다시 시작했다: Task는 끝나지 않은 것으로 실패하고, A의
/// reconciler가 남은 미러를 치우고 표시를 지우며 원본은 계속 돈다. B의 정리(시작할 때와 30분마다)가
/// 받는 QEMU를 끄고 기록 파일대로 만든 사본을 지운다. A의 원본 파일은 그대로다.
#[tokio::test]
async fn restarting_the_source_while_copying_rolls_the_migration_back() {
    let (root, mut a, mut b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_local_disk(&a, &admin).await;

    a.hv.hold_next_mirror();
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    a.hv.mirror_held().await;
    assert_eq!(a.hv.mirroring(vm.id), [0]);
    assert!(b.storage.has_file(volume));
    assert!(b.hv.is_incoming(vm.id));

    a.stop_runner().await;
    a.start_runner().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("interrupted"), "{error}");
    wait_vm(&a, vm.id, "rolled back on A", |v| {
        v.status.migration.is_none() && v.status.phase == VmPhase::Running
    })
    .await;
    until("mirror cleared", || a.hv.mirroring(vm.id).is_empty()).await;
    assert_eq!(
        runtime_status(&a, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    let current = volume_of(&a, volume);
    assert_eq!(current.status.node, Some(a.id));
    assert!(a.storage.has_file(volume));
    assert!(a.storage.deleted().is_empty());

    b.stop_runner().await;
    b.start_runner().await;
    until("copy deleted on B", || !b.storage.has_file(volume)).await;
    assert_eq!(b.storage.deleted(), [volume]);
    assert!(!disk_record(&root, &b, &task).exists());
    assert!(!b.hv.is_incoming(vm.id));
    assert_eq!(runtime_status(&b, vm.id).await, None);
    assert!(a.storage.has_file(volume));
}

/// 복사하는 동안 다른 쪽이 Task를 끝냈으면(실패로 기록) 진행률 쓰기가 Task를 `running`으로 되살리지
/// 않는다.
#[tokio::test]
async fn progress_does_not_revive_a_finished_task() {
    let (_root, a, _b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    running_vm_with_local_disk(&a, &admin).await;

    a.hv.hold_next_mirror();
    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    a.hv.mirror_held().await;
    until("copy progress", || {
        let t = a.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        t.status
            .progress
            .is_some_and(|p| p.stage == "copying-disks")
    })
    .await;
    let failed = TaskStatus {
        state: TaskState::Failed,
        error: Some("ended elsewhere".into()),
        progress: None,
    };
    a.cluster
        .update_status::<TaskSpec>(id, &failed)
        .await
        .unwrap();
    a.hv.release_mirror();
    // 복사가 끝나 메모리 단계에 들어가도(보내기는 500ms 걸린다) 쓰지 않는다.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(400) {
        let t = a.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        assert_eq!(t.status, failed);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 디렉터리 풀 `pool`을 만든다. `shared`면 두 노드가 함께 쓰는 풀이다.
async fn dir_pool(a: &Node, pool: &str, shared: bool) {
    let pool: StoragePool = Resource::new(
        pool,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: format!("/nonexistent/opl-{pool}").into(),
            },
            nodes: vec![],
            shared,
            backups: false,
        },
    );
    a.cluster.create(&pool).await.unwrap();
}

/// 노드 `on`에서 도는 VM `name`. 디스크는 풀 `pool`의 볼륨 `{name}-root`다.
async fn running_vm_on_pool(
    a: &Node,
    admin: &common::Client,
    name: &str,
    pool: &str,
    on: Uuid,
) -> opl_types::vm::Vm {
    let mut volume: Volume = Resource::new(
        format!("{name}-root"),
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(on);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request(name);
    req["spec"]["node"] = serde_json::json!(on);
    req["spec"]["disks"] = serde_json::json!([{ "volume": volume.name, "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap()
}

/// 노드 `node`를 비우는 Task를 만든다.
async fn drain(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post(&format!("/nodes/{node}/drain"), serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    task
}

fn node_of(a: &Node, id: Uuid) -> Option<Uuid> {
    a.cluster.get::<VmSpec>(id).unwrap().unwrap().spec.node
}

#[tokio::test]
async fn drain_moves_running_vms_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app1 = running_vm_on_pool(&a, &admin, "app1", "nas", a.id).await;
    let app2 = running_vm_on_pool(&a, &admin, "app2", "nas", a.id).await;

    // B의 API로 요청해도 비우기 Task는 비울 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = drain(&admin_b, "node1").await;
    assert_eq!(task["spec"]["node"], a.id.to_string());
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(node_of(&a, app1.id), Some(b.id));
    assert_eq!(node_of(&a, app2.id), Some(b.id));
    // 이름 순으로 하나씩 옮겼다.
    let moved: Vec<Uuid> = a.hv.migrations().into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, [app1.id, app2.id]);
    for vm in [&app1, &app2] {
        wait_vm(&b, vm.id, "running on B", |v| {
            v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
        })
        .await;
    }
    let node = a.cluster.get::<NodeSpec>(a.id).unwrap().unwrap();
    assert!(node.spec.maintenance);
}

/// 로컬 디스크 VM도 B의 같은 이름 풀로 디스크를 복사해 옮긴다(볼륨 기록이 B로 가고 A의 원본이
/// 지워진다). 스냅샷이 있는 로컬 디스크 VM은 이유와 함께 남아 A에서 계속 돈다.
#[tokio::test]
async fn drain_moves_vms_with_local_disks() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    dir_pool(&a, "local", false).await;
    let app = running_vm_on_pool(&a, &admin, "app", "nas", a.id).await;
    let db = running_vm_on_pool(&a, &admin, "db", "local", a.id).await;
    let snap = running_vm_on_pool(&a, &admin, "snap", "local", a.id).await;
    let mut volumes = Vec::new();
    for name in ["db-root", "snap-root"] {
        let volume = a.cluster.get_by_name::<VolumeSpec>(name).unwrap().unwrap();
        a.storage.place(volume.id, 1 << 20);
        volumes.push(volume.id);
    }
    let mut snapshot: opl_types::snapshot::Snapshot = Resource::new(
        "",
        opl_types::snapshot::SnapshotSpec {
            vm: snap.id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: snap.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    a.cluster.create(&snapshot).await.unwrap();

    let task = admin.wait_task(&drain(&admin, "node1").await).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "1 VM(s) were not moved: snap (disks on a local pool and snapshots)"
    );
    assert_eq!(node_of(&a, app.id), Some(b.id));
    assert_eq!(node_of(&a, db.id), Some(b.id));
    // 같은 이름의 풀로 옮겼다(`pool` 없음).
    let (_, tasks) = a.cluster.list::<TaskSpec>().unwrap();
    assert!(tasks.iter().any(|t| t.spec.target == db.id
        && t.spec.op
            == TaskOp::VmMigrate {
                node: b.id,
                pool: None
            }));
    let moved = volume_of(&a, volumes[0]);
    assert_eq!(moved.status.node, Some(b.id));
    assert_eq!(moved.spec.pool, "local");
    assert!(b.storage.has_file(volumes[0]));
    assert_eq!(a.storage.deleted(), [volumes[0]]);
    wait_vm(&b, db.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
    })
    .await;
    // 옮기지 못한 VM은 A에서 계속 돈다.
    let snap = a.cluster.get::<VmSpec>(snap.id).unwrap().unwrap();
    assert_eq!(snap.spec.node, Some(a.id));
    assert_eq!(snap.status.phase, VmPhase::Running);
    assert_eq!(
        runtime_status(&a, snap.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(a.storage.has_file(volumes[1]));
}

/// 비우기를 시작할 때 이 노드로 들어오던 VM도 들어온 뒤 옮긴다. 성공하면 노드가 비어 있다.
#[tokio::test]
async fn drain_also_moves_a_vm_that_was_coming_in() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app = running_vm_on_pool(&a, &admin, "app", "nas", b.id).await;

    b.hv.delay_migrations(Duration::from_millis(500));
    let (status, migrate) = admin
        .post("/vms/app/migrate", serde_json::json!({ "node": "node1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{migrate}");
    let task = drain(&admin, "node1").await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let migrate = admin.wait_task(&migrate).await;
    assert_eq!(migrate["status"]["state"], "succeeded", "{migrate}");
    // B → A로 들어온 뒤 비우기가 다시 B로 옮겼다.
    let moved: Vec<Uuid> = a.hv.migrations().into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, [app.id]);
    assert_eq!(node_of(&a, app.id), Some(b.id));
}

/// 옮기는 동안 유지보수를 끝내면 그 마이그레이션이 끝난 뒤 멈춘다.
#[tokio::test]
async fn ending_maintenance_stops_the_drain() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app1 = running_vm_on_pool(&a, &admin, "app1", "nas", a.id).await;
    let app2 = running_vm_on_pool(&a, &admin, "app2", "nas", a.id).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let task = drain(&admin, "node1").await;
    wait_vm(&a, app1.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let (status, node) = admin.get("/nodes/node1").await;
    assert_eq!(status, StatusCode::OK, "{node}");
    let mut spec = node["spec"].clone();
    spec["maintenance"] = serde_json::json!(false);
    let (status, body) = admin
        .put(
            "/nodes/node1",
            serde_json::json!({ "spec": spec }),
            node["revision"].as_u64(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "drain stopped: maintenance ended");
    assert_eq!(node_of(&a, app1.id), Some(b.id));
    assert_eq!(node_of(&a, app2.id), Some(a.id));
}

/// `found`가 참이 될 때까지 `GET /audit?{query}`를 다시 부른다(쓰기 스레드와 복제를 기다린다).
async fn audit_until(
    admin: &common::Client,
    query: &str,
    found: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let (status, body) = admin.get(&format!("/audit?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if found(&body) {
            return body;
        }
        assert!(start.elapsed() < WAIT, "audit: {body:#}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 응답 항목의 노드 이름
fn audit_nodes(body: &serde_json::Value) -> Vec<&str> {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["node"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn audit_entries_of_both_nodes_are_merged() {
    let (_root, a, mut b) = two_nodes().await;
    // 로그인(세션과 토큰 만들기)이 각 노드의 감사 로그에 적힌다.
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let both = |body: &serde_json::Value| {
        let nodes = audit_nodes(body);
        nodes.contains(&"node1") && nodes.contains(&"node2")
    };
    let body = audit_until(&admin, "action=/tokens", both).await;
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["disabled"], serde_json::json!([]));
    assert_eq!(body["truncated"], false);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{body:#}");
    // 두 노드의 항목을 합쳐 새 것부터
    let times: Vec<u64> = entries
        .iter()
        .map(|e| e["time"].as_u64().unwrap())
        .collect();
    assert!(times[0] >= times[1], "{body:#}");
    // 노드로 거르면 그 노드만 묻는다. 한 노드가 limit을 넘기면 잘렸다고 알린다.
    let body = audit_until(&admin, "node=node2", |_| true).await;
    assert!(
        !audit_nodes(&body).is_empty() && audit_nodes(&body).iter().all(|n| *n == "node2"),
        "{body:#}"
    );
    let body = audit_until(&admin, "node=node2&limit=1", |_| true).await;
    assert_eq!(audit_nodes(&body), ["node2"]);
    assert_eq!(body["truncated"], true);

    // 위트니스는 API가 없어 묻지 않는다(닿지 않는 노드에도 넣지 않는다).
    let node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    let witness = NodeSpec {
        witness: true,
        ..node.spec.clone()
    };
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &witness)
        .await
        .unwrap();
    let body = audit_until(&admin, "", |body| !audit_nodes(body).contains(&"node2")).await;
    assert_eq!(body["unreachable"], serde_json::json!([]));
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    audit_until(&admin, "", |body| audit_nodes(body).contains(&"node2")).await;

    // B의 노드 간 서버를 멈추면 B는 닿지 않는 노드다. A의 항목은 그대로 온다.
    let _ = b.peer_stop.take().unwrap().send(());
    b.peer_server.take().unwrap().await.unwrap();
    let body = audit_until(&admin, "", |body| {
        body["unreachable"] != serde_json::json!([])
    })
    .await;
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    let nodes = audit_nodes(&body);
    assert!(
        !nodes.is_empty() && nodes.iter().all(|n| *n == "node1"),
        "{body:#}"
    );
}

#[tokio::test]
async fn a_node_with_the_audit_log_off_is_disabled_not_unreachable() {
    let (_root, a, _b) = two_nodes_with(None, false, true).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let body = audit_until(&admin, "", |body| {
        audit_nodes(body).contains(&"node1") && body["disabled"] != serde_json::json!([])
    })
    .await;
    assert_eq!(body["disabled"], serde_json::json!(["node2"]));
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert!(audit_nodes(&body).iter().all(|n| *n == "node1"), "{body:#}");
}

/// 노드 점은 `t` 1000·2000(CPU는 노드 id의 첫 바이트), VM 점은 어떤 VM이든 `vm_times`.
struct FakeMetrics {
    node: Uuid,
    vm_times: Vec<i64>,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let after = |t: &i64| q.since.is_none_or(|since| *t > since);
        match q.scope {
            MetricsScope::Nodes => MetricsReply {
                node: Some(NodeSeries {
                    node: self.node,
                    samples: [1_000, 2_000]
                        .into_iter()
                        .filter(after)
                        .map(|t| NodeSample {
                            t,
                            cpu: Some(f64::from(self.node.as_bytes()[0])),
                            mem_used_mib: None,
                            mem_total_mib: None,
                            disk_read_bps: None,
                            disk_write_bps: None,
                            net_rx_bps: None,
                            net_tx_bps: None,
                            fs_used_mib: None,
                            fs_total_mib: None,
                        })
                        .collect(),
                }),
                vms: vec![],
            },
            MetricsScope::Vm { id } => MetricsReply {
                node: None,
                vms: vec![VmSeries {
                    vm: id,
                    node: self.node,
                    samples: (self.vm_times.iter().copied())
                        .filter(after)
                        .map(|t| VmSample {
                            t,
                            cpu: None,
                            mem_rss_mib: None,
                            disk_read_bps: None,
                            disk_write_bps: None,
                            disk_read_iops: None,
                            disk_write_iops: None,
                            net_rx_bps: None,
                            net_tx_bps: None,
                        })
                        .collect(),
                }],
            },
        }
    }
}

/// 응답 노드 시리즈의 노드 id
fn metrics_nodes(body: &serde_json::Value) -> Vec<Uuid> {
    body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| serde_json::from_value(n["node"].clone()).unwrap())
        .collect()
}

#[tokio::test]
async fn metrics_of_the_other_node_are_read_over_rpc() {
    let (_root, a, mut b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin.get("/metrics/nodes?since=1000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 노드 이름 순
    assert_eq!(metrics_nodes(&body), [a.id, b.id], "{body:#}");
    assert_eq!(body["nodes"][1]["samples"][0]["t"], 2_000);
    assert_eq!(
        body["nodes"][1]["samples"][0]["cpu"],
        f64::from(b.id.as_bytes()[0])
    );
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["unsupported"], serde_json::json!([]));

    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["node"], serde_json::json!(b.id));
    assert_eq!(body["samples"].as_array().unwrap().len(), 2);

    // VM 지표는 두 노드의 점(A 1000·3000, B 2000)을 시각 순으로 합친다.
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["vm"], vm["id"]);
    let times: Vec<_> = body["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["t"].as_i64().unwrap())
        .collect();
    assert_eq!(times, [1_000, 2_000, 3_000]);
    let (_, body) = admin.get("/vms/web/metrics?since=3000").await;
    assert_eq!(body["samples"], serde_json::json!([]));

    // B의 노드 간 서버를 멈추면 B는 닿지 않는 노드다. A의 점은 그대로 온다.
    let _ = b.peer_stop.take().unwrap().send(());
    b.peer_server.take().unwrap().await.unwrap();
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id]);
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
    let (status, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["samples"].as_array().unwrap().len(), 2);
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "node_unreachable");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("node \"node2\" did not answer: "),
        "{body}"
    );
}

/// B의 Node 리소스를 `name`으로 다시 만든다(이름을 바꾸는 명령이 없다). A가 볼 때까지 기다린다.
async fn rename_node(a: &Node, id: Uuid, name: &str) {
    let mut node = a.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    a.cluster.delete::<NodeSpec>(id, None).await.unwrap();
    node.name = name.into();
    a.cluster.create(&node).await.unwrap();
    until("renamed node", || {
        a.cluster
            .get::<NodeSpec>(id)
            .unwrap()
            .is_some_and(|n| n.name == name)
    })
    .await;
}

/// 노드 목록은 id가 아니라 이름 순이고, 위트니스로 표시한 구성원은 어디에도 넣지 않는다.
#[tokio::test]
async fn node_metrics_are_sorted_by_name_and_witnesses_are_left_out() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    // 나중에 만든 B(id가 더 큼)의 이름이 앞으로 오게 한다.
    assert!(a.id < b.id);
    rename_node(&a, b.id, "node0").await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [b.id, a.id], "{body:#}");

    // 위트니스는 VM을 돌리지 않아 묻지 않는다(Raft 구성원이어도 `nodes`·목록 어디에도 없다).
    let node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    let witness = NodeSpec {
        witness: true,
        ..node.spec.clone()
    };
    a.cluster
        .update_spec(b.id, None, node.labels.clone(), &witness)
        .await
        .unwrap();
    until("witness", || {
        a.cluster
            .get::<NodeSpec>(b.id)
            .unwrap()
            .is_some_and(|n| n.spec.witness)
    })
    .await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id], "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
    admin.post("/vms", vm_request("web")).await;
    let (_, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(body["samples"].as_array().unwrap().len(), 2, "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!([]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
}

/// 꺼진 것으로 기록된 노드는 묻지 않고(연결 제한을 기다리지 않게) 닿지 않는 노드다. B의 서버는 떠 있으므로
/// 물었다면 B의 점이 왔을 것이다.
#[tokio::test]
async fn an_offline_node_is_unreachable_without_being_asked() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let mut node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    node.status.online = false;
    a.cluster
        .update_status::<NodeSpec>(b.id, &node.status)
        .await
        .unwrap();
    until("offline", || {
        a.cluster
            .get::<NodeSpec>(b.id)
            .unwrap()
            .is_some_and(|n| !n.status.online)
    })
    .await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id], "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    assert_eq!(body["unsupported"], serde_json::json!([]));
    admin.post("/vms", vm_request("web")).await;
    let (_, body) = admin.get("/vms/web/metrics").await;
    assert_eq!(body["samples"].as_array().unwrap().len(), 2, "{body:#}");
    assert_eq!(body["unreachable"], serde_json::json!(["node2"]));
    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "node_unreachable");
    assert_eq!(body["message"], "node \"node2\" is offline");
}

#[tokio::test]
async fn a_node_without_metrics_is_unsupported_not_unreachable() {
    let (_root, a, _b) = two_nodes_with(None, true, false).await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin.get("/metrics/nodes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(metrics_nodes(&body), [a.id]);
    assert_eq!(body["unsupported"], serde_json::json!(["node2"]));
    assert_eq!(body["unreachable"], serde_json::json!([]));
    let (status, body) = admin.get("/nodes/node2/metrics").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert_eq!(
        body["message"],
        "node \"node2\" does not report metrics yet"
    );
}

/// `found`가 참이 될 때까지 기다린다(복제를 기다린다).
async fn until(what: &str, found: impl Fn() -> bool) {
    let start = Instant::now();
    while !found() {
        assert!(start.elapsed() < WAIT, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 한 노드에서 시작한 OIDC 로그인의 콜백이 다른 노드로 와도(같은 주소 뒤의 다른 노드) 끝난다.
#[tokio::test]
async fn an_oidc_login_started_on_one_node_finishes_on_the_other() {
    use opl_types::realm::RealmSpec;
    let (_root, a, b) = two_nodes().await;
    // opld가 클러스터를 만들 때처럼 클러스터 정보(CA 키)를 두고, 외부 사용자를 쓰도록 두 노드가 이 빌드를 돈다.
    common::seed_cluster_info(&a.cluster).await;
    for node in [&a, &b] {
        let mut status = a.cluster.get::<NodeSpec>(node.id).unwrap().unwrap().status;
        status.version = Some(opl_types::version::BUILD.into());
        a.cluster
            .update_status::<NodeSpec>(node.id, &status)
            .await
            .unwrap();
    }
    let idp = common::idp::Idp::start().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin
        .post(
            "/realms",
            serde_json::json!({
                "name": "okta",
                "backend": idp.backend(),
                "roles": { "groups": [{ "group": "opl-admins", "role": "admin" }] }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    until("B has the realm", || {
        b.cluster
            .get_by_name::<RealmSpec>("okta")
            .unwrap()
            .is_some()
    })
    .await;

    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let started = browser
        .get(format!("{}/api/v1/auth/oidc/okta/start", a.base))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::FOUND);
    let location = started.headers()["location"].to_str().unwrap().to_string();
    let cookie = started.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let callback = idp.follow(&location).await;
    assert!(callback.starts_with(&a.base), "{callback}");
    // A에서 시작했지만 B가 받는다. 토큰 교환에는 상태 쿠키의 리디렉트 URI(A의 주소)를 쓴다.
    let resp = browser
        .get(callback.replacen(&a.base, &b.base, 1))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()["location"], "/");
    let session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|c| c.starts_with("opl_session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    until("B has the user", || {
        b.cluster
            .get_by_name::<UserSpec>("alice@okta")
            .unwrap()
            .is_some()
    })
    .await;
    let me: serde_json::Value = browser
        .get(format!("{}/api/v1/auth/whoami", b.base))
        .header("cookie", &session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["name"], "alice@okta", "{me}");
    assert_eq!(me["role"], "admin", "{me}");
}
```

`crates/opl-peer/build.rs`에서

```rust
        ))
        .method(
```

을 다음으로 바꾼다:

```rust
        ))
        .method(json(
            "read_metrics",
            "ReadMetrics",
            "opl_types::metrics::MetricsQuery",
            "crate::metrics::MetricsReply",
        ))
        .method(
```

`crates/opl-peer/src/client.rs`에서

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기,
//! UEFI 변수 받기, 감사 로그 읽기.
```

을 다음으로 바꾼다:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기,
//! UEFI 변수 받기, 감사 로그 읽기, 지표 읽기.
```

`crates/opl-peer/src/client.rs`에서

```rust
use opl_types::audit::{AuditEntry, AuditQuery};
use tokio_stream::wrappers::ReceiverStream;
```

을 다음으로 바꾼다:

```rust
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::metrics::MetricsQuery;
use tokio_stream::wrappers::ReceiverStream;
```

`crates/opl-peer/src/client.rs`에서

```rust
use crate::firmware::{UefiVarsError, UefiVarsRequest};
use crate::migration::{
```

을 다음으로 바꾼다:

```rust
use crate::firmware::{UefiVarsError, UefiVarsRequest};
use crate::metrics::MetricsReply;
use crate::migration::{
```

`crates/opl-peer/src/client.rs`에서

```rust

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
```

을 다음으로 바꾼다:

```rust

impl Peers {
    /// `addr`의 노드의 메모리에 있는 지표를 읽는다. 그 노드가 지표를 모르면(옛 노드, 지표 소스가 없는
    /// 노드) None이다.
    pub async fn read_metrics(
        &self,
        addr: &str,
        query: MetricsQuery,
        timeout: Duration,
    ) -> Result<Option<MetricsReply>, Status> {
        let mut req = Request::new(query);
        req.set_timeout(timeout);
        match self.client(addr)?.read_metrics(req).await {
            Ok(reply) => Ok(Some(reply.into_inner())),
            Err(s) if s.code() == Code::Unimplemented => Ok(None),
            Err(s) => Err(s),
        }
    }
}

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
```

`crates/opl-peer/src/lib.rs`에서

```rust
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기, UEFI 변수
//! 파일 보내기, 감사 로그 읽기다.
```

을 다음으로 바꾼다:

```rust
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기, UEFI 변수
//! 파일 보내기, 감사 로그 읽기, 지표 읽기다.
```

`crates/opl-peer/src/lib.rs`에서

```rust
pub mod firmware;
pub mod migration;
```

을 다음으로 바꾼다:

```rust
pub mod firmware;
pub mod metrics;
pub mod migration;
```

`crates/opl-peer/src/lib.rs`에서

```rust
pub use firmware::UefiVarsError;
pub use migration::{
```

을 다음으로 바꾼다:

```rust
pub use firmware::UefiVarsError;
pub use metrics::{MetricsReply, MetricsSource};
pub use migration::{
```

`crates/opl-peer/src/metrics.rs`를 만든다:

```rust
//! 지표 읽기의 노드 사이 RPC: 한 노드의 API가 다른 노드의 메모리에 있는 지표를 읽는다(`ReadMetrics`).
//! 읽는 쪽 처리는 opld가 [`MetricsSource`]로 넣는다.

use opl_types::metrics::{MetricsQuery, NodeSeries, VmSeries};
use serde::{Deserialize, Serialize};

/// 한 노드가 돌려준 지표
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsReply {
    /// `Nodes`를 물었을 때 그 노드의 점들
    pub node: Option<NodeSeries>,
    /// `Vm`을 물었을 때 그 노드에서 잰 VM의 점들(잰 적이 없으면 비어 있다)
    pub vms: Vec<VmSeries>,
}

/// 이 노드의 지표를 읽는 처리
pub trait MetricsSource: Send + Sync {
    /// `since`보다 뒤의 점을 돌려준다.
    fn read(&self, q: &MetricsQuery) -> MetricsReply;
}
```

`crates/opl-peer/src/server.rs`에서

```rust
use opl_types::audit::AuditQuery;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
```

을 다음으로 바꾼다:

```rust
use opl_types::audit::AuditQuery;
use opl_types::metrics::MetricsQuery;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
```

`crates/opl-peer/src/server.rs`에서

```rust
use crate::firmware::{UefiVarsChunk, UefiVarsRequest};
use crate::migration::{IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
```

을 다음으로 바꾼다:

```rust
use crate::firmware::{UefiVarsChunk, UefiVarsRequest};
use crate::metrics::{MetricsReply, MetricsSource};
use crate::migration::{IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
```

`crates/opl-peer/src/server.rs`에서

```rust
    audit: Option<Arc<dyn AuditSource>>,
    /// 드라이버의 `state_dir`. UEFI 변수 파일을 여기서 찾아 보낸다.
```

을 다음으로 바꾼다:

```rust
    audit: Option<Arc<dyn AuditSource>>,
    metrics: Option<Arc<dyn MetricsSource>>,
    /// 드라이버의 `state_dir`. UEFI 변수 파일을 여기서 찾아 보낸다.
```

`crates/opl-peer/src/server.rs`에서

```rust
            audit: None,
            state_dir: None,
```

을 다음으로 바꾼다:

```rust
            audit: None,
            metrics: None,
            state_dir: None,
```

`crates/opl-peer/src/server.rs`에서

```rust
        self.audit = Some(source);
        self
```

을 다음으로 바꾼다:

```rust
        self.audit = Some(source);
        self
    }

    /// 이 노드의 지표를 `source`로 읽어 준다. 없으면 읽기 요청을 `unimplemented`로 거부한다(옛 노드와 같다).
    pub fn with_metrics(mut self, source: Arc<dyn MetricsSource>) -> Self {
        self.metrics = Some(source);
        self
```

`crates/opl-peer/src/server.rs`에서

```rust
        Ok(Response::new(AuditReply { entries }))
    }
```

을 다음으로 바꾼다:

```rust
        Ok(Response::new(AuditReply { entries }))
    }

    async fn read_metrics(
        &self,
        req: Request<MetricsQuery>,
    ) -> Result<Response<MetricsReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .metrics
            .as_ref()
            .ok_or_else(|| Status::unimplemented("this node does not keep metrics"))?;
        Ok(Response::new(source.read(req.get_ref())))
    }
```

`crates/opl-peer/tests/grpc.rs` 전체:

```rust
//! 실제 gRPC + mTLS로 잇는 노드들(한 프로세스 안, 127.0.0.1의 빈 포트).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{
    AuditSource, GrpcNetwork, IncomingDisk, IncomingError, IncomingMigration, MetricsReply,
    MetricsSource, MigrateIncomingRequest, MigrateIncomingResponse, MigrationReceiver, NodeMonitor,
    NodeMonitorConfig, PeerService, Peers, UefiVarsError,
};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

/// 테스트가 동시에 많이 돌아 CPU가 바쁘면 선거가 몇 초씩 걸린다(openraft는 리더 임기 동안 다른
/// 노드에 투표하지 않으므로 리더를 잃으면 보통 선거 제한 시간의 두 배쯤 걸린다).
const WAIT: Duration = Duration::from_secs(30);

fn tls_for(ca: &ClusterCa, id: NodeId) -> NodeTls {
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca.sign_node(&csr, id.0, &["127.0.0.1".into()]).unwrap(),
        key_pem,
    }
}

fn volume(name: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

struct Node {
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 이 노드의 드라이버 `state_dir`(UEFI 변수 파일을 보내는 곳)
    state_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Node {
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        Self::start_with(ca, root, None, None, None).await
    }

    /// `migrations`가 있으면 마이그레이션을 그것으로 받고, `audit`이 있으면 감사 로그를, `metrics`가
    /// 있으면 지표를 그것으로 읽는다.
    async fn start_with(
        ca: &ClusterCa,
        root: &TempDir,
        migrations: Option<Arc<dyn MigrationReceiver>>,
        audit: Option<Arc<dyn AuditSource>>,
        metrics: Option<Arc<dyn MetricsSource>>,
    ) -> Node {
        let id = NodeId(Uuid::now_v7());
        let tls = tls_for(ca, id);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
        config.write_timeout = WAIT;
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        let hv = Arc::new(MockHypervisor::default());
        let state_dir = root.path().join(format!("{id}-state"));
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>))
                .with_uefi_vars(&state_dir);
        if let Some(migrations) = migrations {
            service = service.with_migrations(migrations);
        }
        if let Some(audit) = audit {
            service = service.with_audit(audit);
        }
        if let Some(metrics) = metrics {
            service = service.with_metrics(metrics);
        }
        let (stop, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        Node {
            addr,
            cluster,
            peers,
            hv,
            state_dir,
            stop: Some(stop),
            server: Some(server),
        }
    }

    fn id(&self) -> NodeId {
        self.cluster.id()
    }

    async fn stop(&mut self) {
        // opld처럼 노드 간 서버가 끝난 뒤 Raft를 멈춘다. 먼저 멈추면 서버가 아직 받는 전달 요청에
        // 다시 시도하지 않는 "raft stopped"로 답한다.
        let _ = self.stop.take().unwrap().send(());
        self.server.take().unwrap().await.unwrap();
        self.cluster.shutdown().await.unwrap();
    }
}

/// 받은 요청을 적고 정해 둔 결과를 돌려주는 받는 쪽
#[derive(Default)]
struct FakeReceiver {
    calls: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl MigrationReceiver for FakeReceiver {
    async fn incoming(
        &self,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        self.calls.lock().unwrap().push(format!(
            "incoming {} {} {} {:?}",
            req.vm, req.task, req.qemu_version, req.disks
        ));
        if req.qemu_version != "8.2.2" {
            return Err(IncomingError::Incompatible(format!(
                "QEMU {} differs from 8.2.2",
                req.qemu_version
            )));
        }
        // 복사할 디스크가 있으면 NBD 포트도 연다.
        Ok(MigrateIncomingResponse {
            port: 60007,
            nbd_port: (!req.disks.is_empty()).then_some(60008),
        })
    }

    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("wait {} {}", req.vm, req.task));
        Err(IncomingError::Failed("the receiving QEMU exited".into()))
    }

    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("abort {} {}", req.vm, req.task));
        Ok(())
    }
}

/// 받은 조건을 적고 항목 하나를 돌려주는 감사 로그
#[derive(Default)]
struct FakeAudit {
    queries: std::sync::Mutex<Vec<AuditQuery>>,
}

#[async_trait::async_trait]
impl AuditSource for FakeAudit {
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String> {
        if query.user.as_deref() == Some("broken") {
            return Err("cannot read the audit log".into());
        }
        self.queries.lock().unwrap().push(query);
        Ok(vec![AuditEntry {
            id: Uuid::now_v7(),
            time: 1_000,
            node: "n2".into(),
            user: Some("alice".into()),
            user_id: None,
            role: None,
            auth: AuditAuth::Session,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms".into(),
            action: "POST /vms".into(),
            target: None,
            status: 201,
            duration_ms: 2,
            detail: None,
        }])
    }
}

/// 받은 조건을 적고 `since`보다 뒤의 점을 돌려주는 지표(노드 점은 `t` 1000·2000)
#[derive(Default)]
struct FakeMetrics {
    queries: std::sync::Mutex<Vec<MetricsQuery>>,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        self.queries.lock().unwrap().push(q.clone());
        let samples = [1_000, 2_000]
            .into_iter()
            .filter(|t| q.since.is_none_or(|since| *t > since))
            .map(|t| NodeSample {
                t,
                cpu: Some(12.5),
                mem_used_mib: Some(100),
                mem_total_mib: Some(200),
                disk_read_bps: None,
                disk_write_bps: None,
                net_rx_bps: None,
                net_tx_bps: None,
                fs_used_mib: None,
                fs_total_mib: None,
            })
            .collect();
        MetricsReply {
            node: Some(NodeSeries {
                node: Uuid::nil(),
                samples,
            }),
            vms: vec![],
        }
    }
}

/// 노드 `n`개를 띄우고 첫 노드에서 클러스터를 초기화한 뒤 모두가 같은 리더를 알 때까지 기다린다.
async fn start(n: usize) -> (ClusterCa, TempDir, Vec<Node>) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut nodes = Vec::new();
    for _ in 0..n {
        nodes.push(Node::start(&ca, &root).await);
    }
    let members: BTreeMap<_, _> = nodes
        .iter()
        .map(|n| (n.id(), BasicNode::new(&n.addr)))
        .collect();
    nodes[0].cluster.initialize(members).await.unwrap();
    wait_leader(&nodes).await;
    (ca, root, nodes)
}

async fn wait_leader(nodes: &[Node]) -> NodeId {
    let deadline = Instant::now() + WAIT;
    loop {
        let leaders: Vec<_> = nodes
            .iter()
            .map(|n| n.cluster.raft().metrics().borrow().current_leader)
            .collect();
        if let Some(Some(leader)) = leaders.first()
            && leaders.iter().all(|l| *l == Some(*leader))
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().metrics().borrow().state == ServerState::Leader
        {
            return *leader;
        }
        assert!(Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn nodes_replicate_and_forward_over_mtls() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();

    let v = volume("disk-a");
    nodes[follower].cluster.create(&v).await.unwrap();
    assert!(
        nodes[follower]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );

    // 리더가 죽어도 남은 두 노드로 쓰기가 이어진다.
    let leader = nodes.iter().position(|n| n.id() == leader).unwrap();
    nodes[leader].stop().await;
    let rest: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
    let v = volume("disk-b");
    nodes[rest[0]].cluster.create(&v).await.unwrap();
    assert!(
        nodes[rest[0]]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn strangers_and_non_members_are_refused() {
    let (ca, root, nodes) = start(1).await;
    // 다른 CA의 인증서로는 TLS 연결이 맺어지지 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let stranger = Peers::new(&tls_for(&other, NodeId(Uuid::now_v7())));
    assert!(stranger.ping(&nodes[0].addr, WAIT).await.is_err());
    // 같은 CA라도 멤버가 아니면 거부한다(제거된 노드).
    let outsider = Node::start(&ca, &root).await;
    let err = outsider.peers.ping(&nodes[0].addr, WAIT).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err = outsider
        .peers
        .leader_request(&nodes[0].addr, LeaderRequest::Remove { id: nodes[0].id() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");
    // 멤버는 된다.
    assert_eq!(
        nodes[0].peers.ping(&nodes[0].addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
}

#[tokio::test]
async fn a_new_node_joins_as_learner_then_voter() {
    let (ca, root, mut nodes) = start(1).await;
    let rev = nodes[0].cluster.create(&volume("before")).await.unwrap();

    let new = Node::start(&ca, &root).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: new.id(),
            addr: new.addr.clone(),
        })
        .await
        .unwrap();
    new.cluster
        .raft()
        .wait(Some(WAIT))
        .applied_index_at_least(Some(rev), "learner caught up")
        .await
        .unwrap();
    // 새 노드가 스스로 승격을 요청한다(리더로 전달된다).
    new.cluster
        .request(LeaderRequest::AddVoter { id: new.id() })
        .await
        .unwrap();
    let members = new.cluster.members();
    assert_eq!(members.voters.len(), 2);
    assert!(members.voters.contains_key(&new.id()));
    assert!(
        new.cluster
            .get_by_name::<VolumeSpec>("before")
            .unwrap()
            .is_some()
    );
    nodes.push(new);
    // 새 노드에서 한 쓰기도 커밋된다(이제 과반수는 2).
    nodes[1].cluster.create(&volume("after")).await.unwrap();
}

#[tokio::test]
async fn console_is_relayed_to_the_node_running_the_vm() {
    let (_ca, _root, nodes) = start(2).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let target = &nodes[1];
    target.hv.set_console_dir(tmp.path());
    let vm = Uuid::now_v7();
    let spec: VmRuntimeSpec = serde_json::from_value(serde_json::json!({
        "id": vm, "name": "web", "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256,
        "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
    }))
    .unwrap();
    target.hv.start(&spec, None).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    let mut console = nodes[0].peers.console(&target.addr, vm).await.unwrap();
    assert_eq!(&console.recv().await.unwrap()[..], b"RFB 003.008\n");
    assert!(console.send(bytes::Bytes::from_static(b"ping")).await);
    assert_eq!(&console.recv().await.unwrap()[..], b"ping");

    // 그 노드에서 돌지 않는 VM은 거부한다.
    let err = nodes[0]
        .peers
        .console(&nodes[0].addr, vm)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, opl_peer::ConsoleError::NotRunning), "{err}");
}

#[tokio::test]
async fn leader_records_when_nodes_go_offline_and_come_back() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
        r.status.online = node.id() == leader;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let online = |id: NodeId| {
        leader_cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };
    let wait_for = |want: bool, id: NodeId| {
        let online = &online;
        async move {
            let deadline = Instant::now() + WAIT;
            while online(id) != want {
                assert!(
                    Instant::now() < deadline,
                    "node {id} never became online={want}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    wait_for(true, follower_id).await;
    nodes[follower].stop().await;
    wait_for(false, follower_id).await;
    assert!(online(leader));
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn new_leader_does_not_revive_nodes_recorded_offline() {
    let (_ca, _root, mut nodes) = start(5).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        r.status.online = true;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let online = |cluster: &Cluster, id: NodeId| {
        cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };

    // 팔로워 하나를 멈추고 리더가 오프라인으로 기록할 때까지 기다린다.
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let mut dead = nodes.remove(follower);
    let dead_id = dead.id();
    dead.stop().await;
    let old = nodes.iter().position(|n| n.id() == leader).unwrap();
    let deadline = Instant::now() + WAIT;
    while online(&nodes[old].cluster, dead_id) {
        assert!(
            Instant::now() < deadline,
            "dead follower never went offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 리더도 멈추면 새 리더가 그 기록을 다시 온라인으로 바꾸지 않아야 한다.
    let mut old = nodes.remove(old);
    old.stop().await;
    let new_leader = wait_leader(&nodes).await;
    let new_leader = nodes.iter().find(|n| n.id() == new_leader).unwrap();
    // 새 리더가 이전 커밋을 모두 적용한 뒤부터 본다. openraft는 리더 확인 RPC를 하트비트 간격
    // (50ms)만 기다리고, 노드 둘이 죽어 남는 여유가 없어 CPU가 바쁘면 한 번에 실패할 수 있다.
    // 운영 코드(노드 감시, 예약 작업)처럼 다시 한다.
    let deadline = Instant::now() + WAIT;
    while let Err(e) = new_leader.cluster.read_barrier().await {
        assert!(
            Instant::now() < deadline,
            "read barrier never succeeded: {e}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 감시 간격보다 길고 `offline_after`보다 짧게 지켜본다.
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            !online(&new_leader.cluster, dead_id),
            "new leader revived a node recorded offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn migration_requests_reach_the_receiver_of_a_member() {
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(FakeReceiver::default());
    let target = Node::start_with(&ca, &root, Some(receiver.clone()), None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    // 대상 노드가 멤버십을 받아 원본 노드를 멤버로 알 때까지 기다린다.
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (vm, task) = (Uuid::now_v7(), Uuid::now_v7());
    let peers = &nodes[0].peers;
    let req = |version: &str| MigrateIncomingRequest {
        vm,
        task,
        qemu_version: version.into(),
        disks: vec![],
    };
    assert_eq!(
        peers
            .migrate_incoming(&target.addr, req("8.2.2"))
            .await
            .unwrap(),
        MigrateIncomingResponse {
            port: 60007,
            nbd_port: None
        }
    );
    // 복사할 디스크는 요청에 실려 가고 NBD 포트가 돌아온다.
    let disk = IncomingDisk {
        volume: Uuid::now_v7(),
        size_bytes: 1 << 30,
    };
    let reply = peers
        .migrate_incoming(
            &target.addr,
            MigrateIncomingRequest {
                disks: vec![disk],
                ..req("8.2.2")
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.nbd_port, Some(60008));
    // 받는 쪽의 거절과 실패는 그 종류 그대로 온다.
    let err = peers
        .migrate_incoming(&target.addr, req("9.0.0"))
        .await
        .unwrap_err();
    assert!(matches!(err, IncomingError::Incompatible(_)), "{err:?}");
    let migration = IncomingMigration { vm, task };
    let err = peers
        .migrate_incoming_wait(&target.addr, migration)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        IncomingError::Failed("the receiving QEMU exited".into())
    );
    peers
        .migrate_incoming_abort(&target.addr, migration)
        .await
        .unwrap();
    assert_eq!(
        *receiver.calls.lock().unwrap(),
        [
            format!("incoming {vm} {task} 8.2.2 []"),
            format!("incoming {vm} {task} 8.2.2 {:?}", [disk]),
            format!("incoming {vm} {task} 9.0.0 []"),
            format!("wait {vm} {task}"),
            format!("abort {vm} {task}"),
        ]
    );

    // 받는 처리가 없는 노드는 거부한다.
    let err = target
        .peers
        .migrate_incoming_abort(&nodes[0].addr, migration)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("does not receive migrations")),
        "{err:?}"
    );
    // 멤버가 아닌 노드의 요청은 받는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .migrate_incoming(&target.addr, req("8.2.2"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert_eq!(receiver.calls.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_a_member() {
    let (ca, root, nodes) = start(2).await;
    let (from, to) = (&nodes[1], &nodes[0]);
    let vm = Uuid::now_v7();
    // 데이터 조각, 0만 있는 조각 둘, 끝이 짧은 조각(마지막 바이트만 0이 아님)
    let chunk = opl_peer::firmware::CHUNK_BYTES;
    let mut vars: Vec<u8> = (0..chunk).map(|i| (i % 251) as u8 + 1).collect();
    vars.resize(3 * chunk + 100, 0);
    *vars.last_mut().unwrap() = 0xaa;
    let src = uefi_vars_path(&from.state_dir, vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, &vars).unwrap();

    // 받는 노드의 옛 파일을 바꾼다.
    let dest = uefi_vars_path(&to.state_dir, vm);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"stale").unwrap();
    let len = to
        .peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(len, vars.len() as u64);
    assert!(std::fs::read(&dest).unwrap() == vars, "content differs");

    // 0만 있는 파일, 빈 파일도 크기 그대로 온다.
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(
        to.peers
            .uefi_vars(&from.addr, vm, &dest, None)
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::read(&dest).unwrap().is_empty());

    // 그 노드에 파일이 없으면 NotFound이고 받는 쪽 파일은 그대로다.
    std::fs::write(&dest, b"kept").unwrap();
    let err = to
        .peers
        .uefi_vars(&from.addr, Uuid::now_v7(), &dest, None)
        .await
        .unwrap_err();
    assert_eq!(err, UefiVarsError::NotFound);
    assert_eq!(std::fs::read(&dest).unwrap(), b"kept");

    // 멤버가 아닌 노드에는 보내지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .uefi_vars(&from.addr, vm, &root.path().join("outsider.fd"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, UefiVarsError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert!(!root.path().join("outsider.fd").exists());
}

#[tokio::test]
async fn audit_logs_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let audit = Arc::new(FakeAudit::default());
    let target = Node::start_with(&ca, &root, None, Some(audit.clone()), None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |limit, user: Option<&str>| AuditQuery {
        since: 10,
        until: 20_000,
        user: user.map(Into::into),
        action: Some("/vms".into()),
        limit,
    };
    let peers = &nodes[0].peers;
    let entries = peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "POST /vms");
    // 너무 큰 limit은 받는 쪽이 줄인다.
    peers
        .read_audit(&target.addr, query(1_000_000, None), WAIT)
        .await
        .unwrap();
    assert_eq!(
        *audit.queries.lock().unwrap(),
        [query(7, None), query(AuditQuery::MAX_LIMIT, None)]
    );
    // 읽기 실패는 오류로 온다.
    let err = peers
        .read_audit(&target.addr, query(7, Some("broken")), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Internal, "{err:?}");

    // 감사 로그가 없는 노드는 거부한다.
    let err = target
        .peers
        .read_audit(&nodes[0].addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented, "{err:?}");
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(audit.queries.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn metrics_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let metrics = Arc::new(FakeMetrics::default());
    let target = Node::start_with(&ca, &root, None, None, Some(metrics.clone())).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |since| MetricsQuery {
        scope: MetricsScope::Nodes,
        since,
    };
    let peers = &nodes[0].peers;
    let reply = peers
        .read_metrics(&target.addr, query(Some(1_000)), WAIT)
        .await
        .unwrap()
        .unwrap();
    let samples = reply.node.unwrap().samples;
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].t, 2_000);
    assert_eq!(samples[0].cpu, Some(12.5));
    assert_eq!(*metrics.queries.lock().unwrap(), [query(Some(1_000))]);

    // 지표 소스가 없는 노드(옛 노드처럼)는 None이다.
    let none = target
        .peers
        .read_metrics(&nodes[0].addr, query(None), WAIT)
        .await
        .unwrap();
    assert_eq!(none, None);
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_metrics(&target.addr, query(None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(metrics.queries.lock().unwrap().len(), 1);
}
```

`crates/opl-types/src/metrics.rs`에서

```rust

#[cfg(test)]
```

을 다음으로 바꾼다:

```rust

/// `GET /metrics/nodes`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeMetricsResponse {
    /// 읽은 노드마다 하나(노드 이름 순)
    pub nodes: Vec<NodeSeries>,
    /// 답하지 않은 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 지표를 모르는 옛 노드의 이름(정렬)
    pub unsupported: Vec<String>,
}

/// `GET /vms/{key}/metrics`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmMetricsResponse {
    pub vm: Uuid,
    /// 모든 노드에서 잰 점을 시각 순으로 합친 것(마이그레이션 앞뒤가 이어진다)
    pub samples: Vec<VmSample>,
    /// 답하지 않은 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 지표를 모르는 옛 노드의 이름(정렬)
    pub unsupported: Vec<String>,
}

#[cfg(test)]
```

`crates/opld/src/member.rs`에서

```rust
use crate::metrics::sampler::MetricsSampler;
use crate::metrics::store::MetricsStore;
use crate::overlay::{OverlayAgent, cluster_tag};
```

을 다음으로 바꾼다:

```rust
use crate::metrics::sampler::MetricsSampler;
use crate::metrics::store::{MetricsStore, StoreSource};
use crate::overlay::{OverlayAgent, cluster_tag};
```

`crates/opld/src/member.rs`에서

```rust
        };
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
```

을 다음으로 바꾼다:

```rust
        };
        // 측정기가 적고, API와 다른 노드(노드 간 서버)가 읽는다.
        let metrics_store = Arc::new(MetricsStore::default());
        let metrics_source = Arc::new(StoreSource::new(node, metrics_store.clone()));
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
```

`crates/opld/src/member.rs`에서

```rust
            .with_uefi_vars(config.data_dir.clone())
            .with_audit(audit.clone());
        let peer = serve_peer(peer_listener, &tls, service, exited_tx.clone());
```

을 다음으로 바꾼다:

```rust
            .with_uefi_vars(config.data_dir.clone())
            .with_audit(audit.clone())
            .with_metrics(metrics_source.clone());
        let peer = serve_peer(peer_listener, &tls, service, exited_tx.clone());
```

`crates/opld/src/member.rs`에서

```rust
            state.audit = audit;
            let created = match password {
```

을 다음으로 바꾼다:

```rust
            state.audit = audit;
            state.metrics = Some(metrics_source);
            let created = match password {
```

`crates/opld/src/member.rs`에서

```rust
            config.data_dir.clone(),
            Arc::new(MetricsStore::default()),
        );
```

을 다음으로 바꾼다:

```rust
            config.data_dir.clone(),
            metrics_store,
        );
```

`crates/opld/src/metrics/store.rs`에서

```rust
//! 지표 저장소. 이 노드의 시리즈 하나와 이 노드에서 잰 VM마다 시리즈 하나를 메모리에 둔다.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

#[cfg(test)]
use opl_types::metrics::{MetricsQuery, MetricsScope};
use opl_types::metrics::{NodeSample, VmSample};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
//! 지표 저장소. 이 노드의 시리즈 하나와 이 노드에서 잰 VM마다 시리즈 하나를 메모리에 둔다.
//! API와 노드 간 서버는 [`StoreSource`]로 읽는다.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use opl_peer::{MetricsReply, MetricsSource};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use uuid::Uuid;
```

`crates/opld/src/metrics/store.rs`에서

```rust
/// 저장소에서 읽은 점들(시각 순). 노드 id는 읽는 쪽이 붙인다.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
```

을 다음으로 바꾼다:

```rust
/// 저장소에서 읽은 점들(시각 순). 노드 id는 읽는 쪽이 붙인다.
#[derive(Debug, Clone, PartialEq)]
```

`crates/opld/src/metrics/store.rs`에서

```rust
    /// `since`보다 `t`가 큰 점만 돌려준다.
    // 4단계에서 피어 RPC가 부를 때까지 시험에서만 컴파일한다(바이너리 크레이트라 쓰지 않으면 dead_code다).
    #[cfg(test)]
    pub fn read(&self, q: &MetricsQuery) -> MetricsLocal {
```

을 다음으로 바꾼다:

```rust
    /// `since`보다 `t`가 큰 점만 돌려준다.
    pub fn read(&self, q: &MetricsQuery) -> MetricsLocal {
```

`crates/opld/src/metrics/store.rs`에서

```rust
            },
        }
```

을 다음으로 바꾼다:

```rust
            },
        }
    }
}

/// 이 노드(`node`)의 저장소를 지표 API와 노드 간 서버(`ReadMetrics`)에 읽어 준다.
pub struct StoreSource {
    node: Uuid,
    store: Arc<MetricsStore>,
}

impl StoreSource {
    pub fn new(node: Uuid, store: Arc<MetricsStore>) -> Self {
        Self { node, store }
    }
}

impl MetricsSource for StoreSource {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let local = self.store.read(q);
        let vms = match (&q.scope, local.vm) {
            (MetricsScope::Vm { id }, Some(samples)) => vec![VmSeries {
                vm: *id,
                node: self.node,
                samples,
            }],
            _ => vec![],
        };
        MetricsReply {
            node: local.node.map(|samples| NodeSeries {
                node: self.node,
                samples,
            }),
            vms,
        }
```

`crates/opld/src/metrics/store.rs`에서

```rust
        assert_eq!(nodes.len(), 1);
    }
}
```

을 다음으로 바꾼다:

```rust
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn source_names_this_node_and_the_vm() {
        let store = Arc::new(MetricsStore::default());
        let (me, id) = (Uuid::now_v7(), Uuid::now_v7());
        store.push_node(node(1_000));
        store.push_node(node(2_000));
        store.push_vm(id, vm(1_000));
        let source = StoreSource::new(me, store);

        let reply = source.read(&query(MetricsScope::Nodes, Some(1_000)));
        let series = reply.node.unwrap();
        assert_eq!(series.node, me);
        assert_eq!(times(&series.samples, |s| s.t), [2_000]);
        assert!(reply.vms.is_empty());

        let reply = source.read(&query(MetricsScope::Vm { id }, None));
        assert_eq!(reply.node, None);
        assert_eq!(reply.vms.len(), 1);
        assert_eq!((reply.vms[0].vm, reply.vms[0].node), (id, me));
        assert_eq!(times(&reply.vms[0].samples, |s| s.t), [1_000]);
        // 이 노드에서 잰 적이 없는 VM은 시리즈가 없다.
        let other = source.read(&query(MetricsScope::Vm { id: Uuid::now_v7() }, None));
        assert!(other.vms.is_empty());
    }
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1681개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(metrics): 지표 피어 RPC와 API — opl-peer ReadMetrics(MetricsQuery → MetricsReply{node, vms}), MetricsSource 트레이트, PeerService::with_metrics(소스가 없으면 unimplemented), Peers::read_metrics(모르는 노드는 None); opl-api AppState::metrics, GET /metrics/nodes(NodeMetricsResponse{nodes, unreachable, unsupported}, 노드 이름 순)·/nodes/{key}/metrics(NodeSeries, 위트니스 409 node_unavailable, 닿지 않으면 503 node_unreachable, 옛 노드 409 cluster_upgrading)·/vms/{key}/metrics(VmMetricsResponse{vm, samples, unreachable, unsupported}, 모든 노드의 점을 시각 순으로 합침), 위트니스 제외·내 노드는 직접·남은 노드는 RPC 3초, 권한 Read Node·Vm, since가 숫자가 아니면 400; opld StoreSource(노드 id를 붙여 저장소를 읽음)로 member.rs에서 저장소 하나를 측정기·노드 간 서버·API가 나눠 씀, MetricsStore::read·MetricsLocal을 시험 밖에서도 컴파일; OpenAPI와 schema.d.ts; gRPC·한 노드·두 노드 시험"
```

---

### Task 5: oplctl 지표 보기와 문서

`orbivisorctl node stats`·`vm stats <VM>`(마지막 점의 표, `--watch`는 5초마다 새 표와 `since` 이어 받기 — 일시 오류는 stderr에 찍고 계속, 401·403·404만 끝냄, `-o json --watch`는 회차마다 한 줄 JSON), `client.rs`의 `NotLoggedIn`. 측정기에서 `user` NIC를 네트워크 합에서 빼기. opld E2E(VM 점과 `node stats`), Linux `bridge.rs`(tap 네트워크 값). README "지표" 절과 설계 문서 §11 "구현(계획 44)".

**Files:**
- Modify: `README.md`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`
- Modify: `crates/opld/src/metrics/sampler.rs`
- Modify: `crates/opld/tests/bridge.rs`
- Modify: `crates/opld/tests/e2e.rs`
- Modify: `docs/superpowers/specs/2026-11-11-monitoring-ui-design.md`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown

## 백업과 복원
```

을 다음으로 바꾼다:

````markdown

## 지표

노드마다 5초마다 그 노드와 그 노드에서 도는 VM의 사용량을 재고 메모리에 1시간(시리즈마다 720점) 둔다. Raft에 쓰지
않고 디스크에도 쓰지 않는다. 다른 노드의 지표는 읽을 때 노드 사이 RPC로 모은다(감사 로그와 같은 방식).

```bash
orbivisorctl node stats            # 노드마다 마지막 점
# NODE   TIME                  CPU    MEMORY          DISK READ  DISK WRITE  NET RX     NET TX    DATA FS
# node1  2026-11-11T06:30:05Z  12.3%  6.1GiB/31.2GiB  0B/s       1.2MiB/s    15.4KiB/s  3.1KiB/s  41.0GiB/468.0GiB
orbivisorctl vm stats web          # VM의 마지막 점(마이그레이션 앞뒤 노드의 점을 합친 것)
# TIME                  CPU    MEMORY  DISK READ  DISK WRITE  READ IOPS  WRITE IOPS  NET RX    NET TX
# 2026-11-11T06:30:05Z  48.7%  1.1GiB  0B/s       512.0KiB/s  0.0        12.4        2.0KiB/s  1.1KiB/s
orbivisorctl node stats --watch    # 5초마다 새 점만 다시 읽어 새 표를 이어 찍는다(Ctrl-C로 끝냄, vm stats도)
orbivisorctl -o json vm stats web  # 응답 그대로(남은 모든 점)
orbivisorctl -o json node stats --watch  # 회차마다 응답 한 줄(JSON Lines)
```

- **노드 값.** CPU %(모든 코어를 합해 100), 쓰는 메모리/전체(Linux `MemTotal − MemAvailable`, macOS active + wired +
  compressor), 실제 디스크의 읽기·쓰기 바이트/초(Linux `/proc/diskstats`에서 파티션과 `loop`·`ram`·`zram`·`dm-`·`md`·`nbd`·
  `zd`·`rbd` 장치를 뺀 합 — LVM·RAID·ZFS 위의 쓰기를 두 번 세지 않는다, macOS는 없음), 실제 NIC의 받기·보내기
  바이트/초(Linux는 `/sys/class/net/<if>/device`가 있는 장치, macOS는 `en*`), 데이터 디렉터리(`data_dir`)의 파일 시스템
  사용/전체.
- **VM 값.** CPU %는 QEMU 프로세스의 CPU 시간을 vCPU 수(sockets × cores)로 나눈 것이라 vCPU가 모두 바쁘면 100이고
  I/O 스레드 때문에 조금 넘을 수 있다. 메모리는 QEMU 프로세스의 RSS(게스트가 쓰는 양이 아님), 디스크는 QMP
  `query-blockstats`의 모든 VM 디스크 합(바이트/초와 IOPS), 네트워크는 tap NIC 카운터의 합을 게스트 기준으로 바꾼
  것이다(받기 = tap의 보내기). 사용자 모드 NIC(`user`)는 tap이 없어 세지 않고, tap NIC가 없으면(macOS) 네트워크는
  없다. `Running`·`Paused` VM만 잰다.
- **비율과 빈 값.** 비율은 직전 측정과의 차이를 걸린 시간(단조 시계)으로 나눈다. 첫 측정(orbivisord 시작, VM 시작·붙기,
  QEMU pid 바뀜, 카운터가 줄어듦)은 기준만 잡으므로 VM의 첫 점은 켠 뒤 5~10초에 생긴다. 읽지 못한 값은 `null`(표는
  `-`)이다. `query-blockstats`가 1초 안에 답하지 않으면 그 VM의 디스크 값만 `null`이고, 실패 경고는 VM마다 10분에 한 번
  남는다.
- **API.** `GET /api/v1/metrics/nodes?since=`(모든 노드, `{"nodes":[{"node","samples"}],"unreachable","unsupported"}`,
  노드 이름 순), `GET /api/v1/nodes/{key}/metrics?since=`(노드 하나, `{"node","samples"}`), `GET /api/v1/vms/{key}/metrics?since=`
  (`{"vm","samples","unreachable","unsupported"}` — 위트니스를 뺀 모든 노드에 물어 시각 순으로 합친다). 노드 읽기는 노드
  읽기 권한, VM 읽기는 VM 읽기 권한(viewer도 된다)이다. `since`는 그 노드 시계의 유닉스 밀리초 `t`이고 그보다 뒤의
  점만 온다(숫자가 아니면 **400** `invalid_request`). 점의 `t`는 그 노드의 벽시계라 노드 사이 시계 차이가 그대로 보인다.
  다른 노드는 노드 사이 RPC로 3초까지 기다리고, 꺼진 것으로 기록된 노드는 묻지 않는다. 답하지 않거나 꺼진 노드는
  `unreachable`, 지표를 모르는 옛 orbivisord는 `unsupported`에 이름이 들어간다(orbivisorctl은 표 뒤 stderr에
  `warning: no metrics from: …`, `note: these nodes do not report metrics yet: …`를 쓴다). 노드 하나 읽기는 그 노드가
  꺼졌거나(`node "…" is offline`) 닿지 않으면 **503** `node_unreachable`, 옛 노드면 **409** `cluster_upgrading`,
  위트니스면 **409** `node_unavailable`이다. 노드 기능 목록(`status.features`)에 `metrics`가 있다.
- **한계.** orbivisord를 다시 시작하면 그 노드의 기록이 비고, 꺼진 노드의 기록은 읽을 수 없다. 측정 간격(5초)과
  보관 시간(1시간)은 바꿀 수 없다. Prometheus 내보내기, 경보, 게스트 안의 지표, 디스크·NIC마다 나눈 값은 없다. 웹 차트는
  다음 계획이다. vCPU 수와 NIC 목록은 지금 VM 스펙에서 읽으므로, 다시 시작해야 적용되는 변경(코어 수, NIC 더하기) 뒤
  VM을 다시 시작하기 전까지는 CPU %가 새 vCPU 수로 나뉘고 네트워크가 `null`일 수 있다.

## 백업과 복원
````

`README.md`에서

```markdown
- [제품 이름 Orbivisor로 바꾸기 설계 (계획 43)](docs/superpowers/specs/2026-11-10-rename-orbivisor-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [제품 이름 Orbivisor로 바꾸기 설계 (계획 43)](docs/superpowers/specs/2026-11-10-rename-orbivisor-design.md)
- [실시간 지표와 쉬운 화면 설계 (하위 프로젝트 14: 계획 44~47)](docs/superpowers/specs/2026-11-11-monitoring-ui-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{
    ApiError, BACKUP_WAIT, Client, DRAIN_WAIT, IMPORT_WAIT, MIGRATE_WAIT, NotLoggedIn,
    UPGRADE_WAIT, fetch_ca, fingerprint, local, seg,
};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(
    name = "orbivisorctl",
    version = opl_types::version::BUILD,
    about = "Orbivisor command-line client"
)]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(login이 만든 토큰 `oplctl-…`이면 서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지나 orbivisord가 만드는 VXLAN 네트워크)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// VM NIC에 거는 방화벽 규칙 묶음. nftables가 있는 노드가 적용한다.
    SecurityGroup {
        #[command(subcommand)]
        cmd: SecurityGroupCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    /// VM 백업(백업 풀에 담은 디스크 복사본과 VM 설정)과 복원
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// 백업 작업: 정한 시각(UTC)에 고른 VM들을 백업하고 보존 규칙대로 옛 백업을 지운다.
    BackupJob {
        #[command(subcommand)]
        cmd: BackupJobCmd,
    },
    /// 다른 하이퍼바이저(VMware)의 VM 가져오기(OVA, ESXi·vCenter)
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// 인증 영역(admin만): 바깥 LDAP 디렉터리나 OIDC IdP로 로그인하게 한다.
    Realm {
        #[command(subcommand)]
        cmd: RealmCmd,
    },
    /// 클러스터 설정(업데이트 채널)
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// 구독 키와 상태. 구독은 기능을 막지 않는다.
    Subscription {
        #[command(subcommand)]
        cmd: SubscriptionCmd,
    },
    /// 옵트인 텔레메트리(기본 꺼짐). 켜면 리더가 하루 한 번 클러스터 규모를 보낸다.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },
    /// 감사 로그(admin만): 모든 노드의 바꾸는 요청·로그인·콘솔 열기를 새 것부터
    Audit(AuditArgs),
    /// 원격 클러스터: 다른 Orbivisor 클러스터를 등록해 상태를 모아 보고, 그 클러스터의 API를 이 클러스터를
    /// 거쳐 부른다(저장한 그 클러스터의 API 토큰으로).
    Remote {
        #[command(subcommand)]
        cmd: RemoteCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin", conflicts_with = "token_file")]
    user: String,
    /// 인증 영역. `local`이 아니면 그 영역의 사용자 이름(`@영역` 없이)으로 로그인한다.
    #[arg(long, default_value = "local", conflicts_with = "token_file")]
    realm: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long, conflicts_with = "token_file")]
    password_file: Option<PathBuf>,
    /// 웹 UI에서 만든 API 토큰이 든 파일(`-`면 표준 입력). 비밀번호 대신 이 토큰을 확인해 저장한다
    /// (SSO(OIDC) 사용자).
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `orbivisorctl cluster init`의 출력이나 다른 노드의 `orbivisorctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// orbivisord 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get {
        node: String,
    },
    /// 노드의 업데이트 확인 결과(올릴 수 있는 패키지)를 보인다.
    Updates {
        node: String,
        /// 먼저 노드에서 다시 확인하고(Task가 끝날 때까지 기다린다) 결과를 보인다.
        #[arg(long)]
        refresh: bool,
    },
    /// 노드의 패키지를 올린다(`apt-get dist-upgrade`). VM은 계속 돌고 orbivisord가 잠깐 다시 시작한다. 한 번에
    /// 한 노드만 한다.
    Upgrade {
        node: String,
        /// 업그레이드 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
    /// 노드를 유지보수로 바꾸거나(`on` — 새 VM을 놓지 않는다) 끝낸다(`off`). 도는 VM은 그대로다.
    Maintenance {
        node: String,
        state: Switch,
    },
    /// 노드를 유지보수로 바꾸고 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션한다. 로컬 풀
    /// 디스크 VM은 옮기지 못한다.
    Drain {
        node: String,
        /// 비우기 Task가 끝날 때까지 기다린다. 옮기지 못한 VM이 있으면 실패한다.
        #[arg(long)]
        wait: bool,
    },
    /// 노드마다 마지막 사용량 점(CPU, 메모리, 디스크, 네트워크, 데이터 파일 시스템). `-o json`은 남은 모든
    /// 점(1시간)이다.
    Stats(StatsArgs),
}

/// `node stats`·`vm stats`의 공통 인자
#[derive(Args)]
struct StatsArgs {
    /// 5초마다 다시 읽어 새 표를 이어 찍는다(Ctrl-C로 끝낸다).
    #[arg(long)]
    watch: bool,
}

/// `node maintenance`의 켜기·끄기
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Switch {
    On,
    Off,
}

/// `node maintenance`: 노드를 읽어 라벨과 spec은 그대로 두고 `maintenance`만 바꿔 그 revision으로
/// (`If-Match`) 다시 쓴다. 읽은 뒤 status만 바뀌어도 revision이 달라지므로 `revision_mismatch`면 다시 한다.
/// 끄면 그 노드의 끝나지 않은 비우기가 멈춘다고 `err`(stderr)에 알린다(Task 목록을 읽지 못하면 그 경고).
async fn node_maintenance(
    client: &Client,
    node: &str,
    on: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let path = format!("/nodes/{}", seg(node));
    let mut retries = SpecRetries::default();
    let id = loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&spec_update(&current, "maintenance", json!(on)));
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => break cell(&current["id"]),
        }
    };
    let state = if on { "on" } else { "off" };
    writeln!(out, "node {node}: maintenance {state}")?;
    if !on {
        // 비우기 Task는 유지보수가 끝나면 멈춘다. 유지보수는 이미 꺼졌으므로 찾아보지 못해도 경고만 한다.
        let tasks = match client.get("/tasks").await {
            Ok(tasks) => tasks,
            Err(e) => {
                writeln!(err, "warning: cannot check for a running drain: {e}")?;
                return Ok(());
            }
        };
        let draining = tasks["items"].as_array().into_iter().flatten().find(|t| {
            t["spec"]["op"]["kind"] == "node-drain"
                && cell(&t["spec"]["node"]) == id
                && !matches!(t["status"]["state"].as_str(), Some("succeeded" | "failed"))
        });
        if let Some(task) = draining {
            writeln!(
                err,
                "warning: the running drain of {node} stops (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

/// `node drain`: 비우기 Task를 만든다(노드는 곧바로 유지보수가 된다). `wait`이면 끝날 때까지 기다리고,
/// 아니면 Task를 따라가는 방법을 알린다.
async fn node_drain(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{}/drain", seg(node)), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, DRAIN_WAIT).await?;
        writeln!(out, "node {node} drained")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "drain of {node} started (task {id}); follow it with `orbivisorctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node upgrade`: 업그레이드 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn node_upgrade(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{}/upgrade", seg(node)), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, UPGRADE_WAIT).await?;
        writeln!(out, "node {node} upgraded")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "upgrade of {node} started (task {id}); follow it with `orbivisorctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node updates`의 패키지 표 열
const UPDATE_HEADERS: &[&str] = &["PACKAGE", "CURRENT", "CANDIDATE"];

/// `node updates`: `--refresh`면 다시 확인 Task를 기다린 뒤, 노드의 `status.updates`를 보인다. 확인 시각,
/// 오류, 패키지 표(새로 설치할 패키지는 CURRENT가 `-`), 잘렸는지. `-o json`이면 `status.updates` 그대로.
async fn node_updates(
    client: &Client,
    node: &str,
    refresh: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if refresh {
        let task = client
            .post(&format!("/nodes/{}/updates/refresh", seg(node)), &json!({}))
            .await?;
        client.wait_task(&task).await?;
    }
    let found = client.get(&format!("/nodes/{}", seg(node))).await?;
    let updates = &found["status"]["updates"];
    if json_out {
        return Ok(print_json(out, updates)?);
    }
    if !updates.is_object() {
        bail!("node {node} does not check for updates");
    }
    let checked = updates["checked_at"]
        .as_i64()
        .map_or_else(|| "never".to_string(), unix_time);
    writeln!(out, "checked: {checked}")?;
    if let Some(error) = updates["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let packages = updates["packages"].as_array().cloned().unwrap_or_default();
    if packages.is_empty() {
        if updates["checked_at"].is_u64() && updates["error"].is_null() {
            writeln!(out, "no updates available")?;
        }
        return Ok(());
    }
    let rows: Vec<_> = packages
        .iter()
        .map(|p| vec![cell(&p["name"]), cell(&p["current"]), cell(&p["candidate"])])
        .collect();
    table(out, UPDATE_HEADERS, &rows)?;
    if updates["truncated"] == true {
        writeln!(out, "(only the first {} packages are shown)", rows.len())?;
    }
    Ok(())
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다. 백업을 담는 풀로 표시하거나 끈다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args(["shared", "no_shared", "backups"])))]
struct PoolSet {
    pool: String,
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long, conflicts_with = "no_shared")]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
    /// VM 백업을 담는 풀로 표시한다(true, 디렉터리·NFS 풀만)거나 끈다(false, 백업이 남아 있으면 거절한다).
    #[arg(long, value_name = "true|false")]
    backups: Option<bool>,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`). 주지 않은 값은 보내지 않는다.
fn pool_set_body(set: &PoolSet) -> Value {
    let mut body = json!({});
    if set.shared || set.no_shared {
        body["shared"] = json!(set.shared);
    }
    if let Some(backups) = set.backups {
        body["backups"] = json!(backups);
    }
    body
}

/// `pool set`이 바꾼 것: `shared, backups on`
fn pool_set_summary(set: &PoolSet) -> String {
    let mut parts = Vec::new();
    if set.shared {
        parts.push("shared");
    } else if set.no_shared {
        parts.push("node-local");
    }
    match set.backups {
        Some(true) => parts.push("backups on"),
        Some(false) => parts.push("backups off"),
        None => {}
    }
    parts.join(", ")
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
    /// VM 백업을 담는 풀로 표시한다. 디렉터리·NFS 풀만 된다.
    #[arg(long, conflicts_with_all = ["lvm_thin", "zfs", "rbd", "lvm_shared"])]
    backups: bool,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
        backups,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    if backups {
        spec["backups"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크나 VXLAN 네트워크를 만든다.
    Create(NetworkCreate),
    Delete {
        network: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["bridge", "vxlan"])))]
struct NetworkCreate {
    name: String,
    /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
    #[arg(long)]
    bridge: Option<String>,
    /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
    #[arg(long, conflicts_with = "vxlan")]
    vlan: Option<u16>,
    /// VXLAN 네트워크 식별자(VNI, 1-16777215). orbivisord가 모든 노드에 브리지 oplv<VNI>를 만든다.
    #[arg(long, value_name = "VNI")]
    vxlan: Option<u32>,
    /// VXLAN 네트워크의 MTU(1280-9000, 기본 1450)
    #[arg(long, conflicts_with = "bridge")]
    mtu: Option<u16>,
}

/// `network create`의 요청 본문(`POST /networks`). 주지 않은 값은 보내지 않는다.
fn network_create_body(c: &NetworkCreate) -> Value {
    let mut spec = serde_json::Map::new();
    if let Some(bridge) = &c.bridge {
        spec.insert("bridge".into(), json!(bridge));
    }
    if let Some(vlan) = c.vlan {
        spec.insert("vlan".into(), json!(vlan));
    }
    if let Some(vni) = c.vxlan {
        spec.insert("vxlan".into(), json!({ "vni": vni }));
    }
    if let Some(mtu) = c.mtu {
        spec.insert("mtu".into(), json!(mtu));
    }
    json!({ "name": c.name, "spec": spec })
}

#[derive(Subcommand)]
enum SecurityGroupCmd {
    List,
    Get {
        group: String,
    },
    /// 보안 그룹을 만든다. 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. 맞는 규칙이 없으면
    /// 들어오는 것은 막고 나가는 것은 보낸다.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 규칙 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`
        /// (예: "in accept tcp 22 10.0.0.0/8"). 여러 번 줄 수 있다.
        #[arg(long = "rule", value_name = "RULE", value_parser = parse_rule)]
        rules: Vec<Value>,
    },
    /// 보안 그룹을 지운다. 그룹을 쓰는 VM NIC가 없어야 한다.
    Delete {
        group: String,
    },
    /// 규칙을 더하거나 뺀다. 그룹을 쓰는 노드가 곧바로 적용한다.
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// 규칙 하나를 더한다(RULE은 `security-group create --rule`과 같다).
    Add {
        group: String,
        #[arg(value_parser = parse_rule)]
        rule: Value,
        /// 이 번호(1부터) 자리에 넣는다. 생략하면 맨 끝이다.
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// N번 규칙(1부터, `security-group get`의 순서)을 뺀다.
    Rm { group: String, n: usize },
}

/// `in accept tcp 22 10.0.0.0/8`처럼 쓴다: 방향과 동작, 그 뒤로 프로토콜·포트·원격 CIDR을 이 차례로
/// (모두 생략할 수 있다). 값의 나머지 검사(포트 범위, 프로토콜과 포트·주소의 짝)는 API가 한다.
fn parse_rule(s: &str) -> Result<Value, String> {
    let bad = || {
        format!("invalid rule {s:?}: use in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]")
    };
    let mut words = s.split_whitespace().peekable();
    let direction = words
        .next()
        .filter(|w| matches!(*w, "in" | "out"))
        .ok_or_else(bad)?;
    let action = words
        .next()
        .filter(|w| matches!(*w, "accept" | "drop"))
        .ok_or_else(bad)?;
    let mut rule = json!({ "direction": direction, "action": action });
    if let Some(protocol) = words.next_if(|w| matches!(*w, "tcp" | "udp" | "icmp" | "icmpv6")) {
        rule["protocol"] = json!(protocol);
    }
    if let Some(ports) = words.next_if(|w| is_ports(w)) {
        rule["ports"] = json!(ports);
    }
    if let Some(remote) = words.next_if(|w| w.contains(['.', ':'])) {
        rule["remote"] = json!(remote);
    }
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(rule)
}

/// `22`나 `8000-8080` 모양인지
fn is_ports(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match word.split_once('-') {
        Some((start, end)) => digits(start) && digits(end),
        None => digits(word),
    }
}

/// `security-group create`의 요청 본문(`POST /security-groups`)
fn security_group_create_body(name: &str, description: Option<&str>, rules: &[Value]) -> Value {
    let mut spec = json!({ "rules": rules });
    if let Some(description) = description {
        spec["description"] = json!(description);
    }
    json!({ "name": name, "spec": spec })
}

/// 그룹의 규칙에 `rule`을 `at`번(1부터, 생략하면 맨 끝) 자리에 넣은 목록
fn rules_with(group: &Value, rule: Value, at: Option<usize>) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = at.unwrap_or(rules.len() + 1);
    if at == 0 || at > rules.len() + 1 {
        bail!(
            "cannot add a rule at {at}: security group {} has {} rules (use 1 to {})",
            cell(&group["name"]),
            rules.len(),
            rules.len() + 1
        );
    }
    rules.insert(at - 1, rule);
    Ok(rules)
}

/// 그룹의 규칙에서 `n`번(1부터)을 뺀 목록
fn rules_without(group: &Value, n: usize) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if n == 0 || n > rules.len() {
        bail!(
            "security group {} has no rule {n} ({} rules, numbered from 1)",
            cell(&group["name"]),
            rules.len()
        );
    }
    rules.remove(n - 1);
    Ok(rules)
}

/// 그룹을 읽어 `edit`으로 만든 규칙 목록으로 바꾼다. 읽은 revision을 `If-Match`로 보내므로 그 사이
/// 다른 사람이 바꿨으면 실패한다(규칙 번호가 달라졌을 수 있다).
async fn patch_rules(
    client: &Client,
    group: &str,
    edit: impl FnOnce(&Value) -> anyhow::Result<Vec<Value>>,
) -> anyhow::Result<()> {
    let path = format!("/security-groups/{}", seg(group));
    let current = client.get(&path).await?;
    let rules = edit(&current)?;
    let r = client
        .request(Method::PATCH, &path)
        .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
        .json(&json!({ "rules": rules }));
    client.send(r).await?;
    Ok(())
}

/// `security-group list`의 열
const SECURITY_GROUP_HEADERS: &[&str] = &["NAME", "RULES", "DESCRIPTION"];

/// `security-group list`의 한 행
fn security_group_row(g: &Value) -> Vec<String> {
    vec![
        cell(&g["name"]),
        g["spec"]["rules"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        cell(&g["spec"]["description"]),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    /// NIC의 보안 그룹을 바꾼다. 실행 중이어도 곧바로 적용된다. 그룹을 주지 않으면 모두 뺀다.
    NicSg {
        vm: String,
        /// NIC 번호(0부터, `vm get`의 nics 순서)
        index: usize,
        groups: Vec<String>,
    },
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다. 노드 로컬 풀의 디스크는 대상 노드에 복사한다
    /// (로컬 디스크가 있는 VM에 스냅샷이 있으면 옮기지 못한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
        /// 노드 로컬 풀의 디스크를 둘 대상 노드의 풀. 없으면 디스크마다 같은 이름의 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// VM의 마지막 사용량 점(QEMU 프로세스의 CPU·RSS, 디스크, 네트워크). `-o json`은 남은 모든 점(1시간)이다.
    Stats {
        vm: String,
        #[command(flatten)]
        stats: StatsArgs,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다. `,sg=A+B`를 붙이면 그 NIC에
    /// 보안 그룹을 건다(예: `lan,sg=web+db`).
    #[arg(long = "nic", value_name = "NETWORK[,sg=GROUP+...]", value_parser = parse_nic)]
    nics: Vec<NicArg>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

/// `--nic`으로 받은 네트워크와 보안 그룹
#[derive(Debug, Clone, PartialEq, Eq)]
struct NicArg {
    network: String,
    security_groups: Vec<String>,
}

/// `lan`, `lan,sg=web+db`처럼 쓴다. 그룹이 있는지는 API가 본다.
fn parse_nic(s: &str) -> Result<NicArg, String> {
    let bad = || format!("invalid nic {s:?}: use NETWORK or NETWORK,sg=GROUP+GROUP");
    let (network, groups) = match s.split_once(',') {
        Some((network, option)) => {
            let groups = option.strip_prefix("sg=").ok_or_else(bad)?;
            let groups: Vec<String> = groups.split('+').map(str::to_string).collect();
            if groups.iter().any(|g| g.is_empty() || g.contains(',')) {
                return Err(bad());
            }
            (network, groups)
        }
        None => (s, Vec::new()),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NicArg {
        network: network.into(),
        security_groups: groups,
    })
}

/// `vm create`의 NIC 하나. 그룹이 없으면 `security_groups`를 보내지 않는다.
fn nic_body(nic: &NicArg) -> Value {
    let mut body = json!({ "network": nic.network, "model": "virtio" });
    if !nic.security_groups.is_empty() {
        body["security_groups"] = json!(nic.security_groups);
    }
    body
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum BackupCmd {
    /// VM을 백업한다. 실행 중이면 디스크를 잠깐 스냅샷한 때의 상태(crash-consistent)를 담는다.
    Create {
        vm: String,
        /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
        #[arg(long)]
        pool: String,
        /// 생략하면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
        #[arg(long)]
        name: Option<String>,
        /// 할 수 있으면 증분으로(같은 풀의 바로 앞 백업을 부모로 바뀐 블록만) 백업한다. 할 수 없으면
        /// 전체로 하고 그 이유를 보인다.
        #[arg(long)]
        incremental: bool,
    },
    /// 백업 목록. VM을 지운 백업도 보인다.
    List {
        /// 이 VM(이름이나 id)의 백업만. 지운 VM은 백업에 남은 이름으로 찾는다.
        #[arg(long)]
        vm: Option<String>,
    },
    /// 백업(JSON). 증분 백업은 `chain`에 전체 백업부터 이 백업까지의 이름과 체인 파일 크기의 합, 원격
    /// 사본마다 체인의 몇 백업이 원격에 있는지(`replicas`의 "k of n copied")를 더한다.
    Get { backup: String },
    /// 백업 파일과 기록을 지운다. 실패한 백업도 지운다.
    Delete {
        backup: String,
        /// 파일을 지울 노드가 없으면 기록만 지운다(파일은 백업 풀에 남는다).
        #[arg(long)]
        force: bool,
    },
    /// 백업에서 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Restore {
        backup: String,
        /// 새 VM 이름
        #[arg(long)]
        name: String,
        /// 새 디스크를 둘 풀
        #[arg(long)]
        pool: String,
        /// 복원을 실행할 노드(이름이나 id). 생략하면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
        #[arg(long)]
        node: Option<String>,
    },
    /// 백업을 원격 클러스터(`remote add`로 등록)의 백업 풀에 사본으로 보낸다. 원격 쪽 토큰 주인은
    /// operator 이상이어야 한다.
    Replicate {
        backup: String,
        /// 원격 이름
        #[arg(long)]
        remote: String,
        /// 원격의 백업 풀
        #[arg(long)]
        pool: String,
        /// 복제 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Subcommand)]
enum BackupJobCmd {
    /// 백업 작업을 만든다. 정한 요일의 시각(UTC)에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛
    /// 백업을 지운다.
    Create(BackupJobCreate),
    List,
    Get {
        job: String,
    },
    /// 준 값만 바꾼다. 일정이나 보존 규칙의 일부만 주면 나머지는 지금 값을 그대로 둔다.
    Set(BackupJobSet),
    /// 작업만 지운다. 작업이 만든 백업은 남는다.
    Delete {
        job: String,
    },
    /// 지금 실행한다. 리더의 스케줄러가 다음 차례에 시작하며 끝나기를 기다리지 않는다.
    Run {
        job: String,
    },
}

/// 백업할 VM 고르기. 셋 중 하나만 준다.
#[derive(Args)]
struct VmsArgs {
    /// 모든 VM
    #[arg(long)]
    all: bool,
    /// 이 이름의 VM(여러 번 줄 수 있다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
    /// 이 라벨을 모두 가진 VM(여러 번 줄 수 있다)
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    labels: Vec<(String, String)>,
}

/// 보존 규칙. 모두 0이면 지우지 않는다.
#[derive(Args)]
struct KeepArgs {
    /// 최신 N개를 남긴다.
    #[arg(long, value_name = "N")]
    keep_last: Option<u32>,
    /// 그다음 하루(UTC)마다 가장 새 것 하나씩 N일
    #[arg(long, value_name = "N")]
    keep_daily: Option<u32>,
    /// 그다음 ISO 주마다 가장 새 것 하나씩 N주
    #[arg(long, value_name = "N")]
    keep_weekly: Option<u32>,
    /// 그다음 달마다 가장 새 것 하나씩 N달
    #[arg(long, value_name = "N")]
    keep_monthly: Option<u32>,
}

impl KeepArgs {
    fn given(&self) -> bool {
        self.keep_last.is_some()
            || self.keep_daily.is_some()
            || self.keep_weekly.is_some()
            || self.keep_monthly.is_some()
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").required(true).args(["all", "vms", "labels"])))]
struct BackupJobCreate {
    name: String,
    /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
    #[arg(long)]
    pool: String,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
    /// 작업이 만든 백업을 원격의 백업 풀에 사본으로 보낸다.
    #[arg(long, value_name = "REMOTE/POOL", value_parser = parse_replicate)]
    replicate: Option<Value>,
    /// 1 이상이면 할 수 있을 때 증분으로 백업하고, 체인의 증분이 N개가 되면 다음은 전체다. 0이면 늘 전체.
    #[arg(long, value_name = "N")]
    full_every: Option<u32>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").args(["all", "vms", "labels"])))]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "all", "vms", "labels", "pool", "at", "days",
    "keep_last", "keep_daily", "keep_weekly", "keep_monthly", "enabled",
    "replicate", "no_replicate", "full_every",
])))]
struct BackupJobSet {
    job: String,
    #[arg(long)]
    pool: Option<String>,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 모든 요일을 주면 매일이다.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 켜거나(다음 실행 시각을 지금부터 다시 정한다) 끈다. 진행 중인 실행은 끝까지 간다.
    #[arg(long, value_name = "true|false")]
    enabled: Option<bool>,
    /// 작업이 만든 백업을 원격의 백업 풀에 사본으로 보낸다.
    #[arg(long, value_name = "REMOTE/POOL", value_parser = parse_replicate, conflicts_with = "no_replicate")]
    replicate: Option<Value>,
    /// 원격 복제를 끈다. 이미 보낸 사본은 남는다.
    #[arg(long)]
    no_replicate: bool,
    /// 1 이상이면 할 수 있을 때 증분으로 백업하고, 체인의 증분이 N개가 되면 다음은 전체다. 0이면 늘 전체.
    #[arg(long, value_name = "N")]
    full_every: Option<u32>,
}

/// `--replicate`의 `REMOTE/POOL`을 `replicate` 값(`{ remote, pool }`)으로
fn parse_replicate(s: &str) -> Result<Value, String> {
    match s.split_once('/') {
        Some((remote, pool)) if !remote.is_empty() && !pool.is_empty() && !pool.contains('/') => {
            Ok(json!({ "remote": remote, "pool": pool }))
        }
        _ => Err(format!("invalid target {s:?}: use REMOTE/POOL")),
    }
}

/// `--label`의 `KEY=VALUE`. 값에는 `=`가 있어도 된다.
fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err(format!("invalid label {s:?}: use KEY=VALUE")),
    }
}

/// VM 고르기(`vms` 값). 주지 않았으면 `None`이다.
fn vms_value(args: &VmsArgs) -> Option<Value> {
    if args.all {
        Some(json!({ "all": true }))
    } else if !args.vms.is_empty() {
        Some(json!({ "names": args.vms }))
    } else if !args.labels.is_empty() {
        let labels: serde_json::Map<String, Value> = args
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Some(json!({ "labels": labels }))
    } else {
        None
    }
}

fn days_value(days: &[Day]) -> Value {
    json!(days.iter().map(|d| value_name(*d)).collect::<Vec<_>>())
}

/// 보존 규칙(`keep` 값). 주지 않은 단위는 `current`의 값, 거기도 없으면 0이다.
fn keep_value(args: &KeepArgs, current: &Value) -> Value {
    let pick = |arg: Option<u32>, field: &str| {
        arg.map_or_else(|| current[field].as_u64().unwrap_or(0), u64::from)
    };
    json!({
        "last": pick(args.keep_last, "last"),
        "daily": pick(args.keep_daily, "daily"),
        "weekly": pick(args.keep_weekly, "weekly"),
        "monthly": pick(args.keep_monthly, "monthly"),
    })
}

/// `backup-job create`의 요청 본문
fn backup_job_create_body(args: &BackupJobCreate) -> Value {
    let mut body = json!({
        "name": args.name,
        "spec": {
            "vms": vms_value(&args.vms),
            "pool": args.pool,
            "schedule": { "at": args.at, "days": days_value(&args.days) },
            "keep": keep_value(&args.keep, &Value::Null),
            "enabled": !args.disabled,
        }
    });
    // 복제하지 않으면 보내지 않는다(옛 서버는 모르는 필드를 거절한다).
    if let Some(replicate) = &args.replicate {
        body["spec"]["replicate"] = replicate.clone();
    }
    // 주지 않으면 보내지 않는다(옛 서버는 모르는 필드를 거절한다).
    if let Some(full_every) = args.full_every {
        body["spec"]["full_every"] = json!(full_every);
    }
    body
}

/// `backup-job set`의 요청 본문(`PATCH /backup-jobs/{key}`). 준 값만 보낸다. 서버는 일정과 보존
/// 규칙을 통째로 바꾸므로 그 일부만 주었으면 나머지는 읽은 작업(`current`)의 값으로 채운다.
fn backup_job_set_body(args: &BackupJobSet, current: &Value) -> Value {
    let mut body = json!({});
    if let Some(vms) = vms_value(&args.vms) {
        body["vms"] = vms;
    }
    if let Some(pool) = &args.pool {
        body["pool"] = json!(pool);
    }
    let schedule = &current["spec"]["schedule"];
    if args.at.is_some() || !args.days.is_empty() {
        body["schedule"] = json!({
            "at": args.at.as_ref().map_or_else(|| schedule["at"].clone(), |at| json!(at)),
            "days": if args.days.is_empty() {
                schedule["days"].as_array().map_or_else(|| json!([]), |d| json!(d))
            } else {
                days_value(&args.days)
            },
        });
    }
    if args.keep.given() {
        body["keep"] = keep_value(&args.keep, &current["spec"]["keep"]);
    }
    if let Some(enabled) = args.enabled {
        body["enabled"] = json!(enabled);
    }
    // `null`은 복제를 끈다(생략하면 그대로).
    if let Some(replicate) = &args.replicate {
        body["replicate"] = replicate.clone();
    } else if args.no_replicate {
        body["replicate"] = Value::Null;
    }
    if let Some(full_every) = args.full_every {
        body["full_every"] = json!(full_every);
    }
    body
}

#[derive(Subcommand)]
enum ImportCmd {
    /// VMware 등에서 내보낸 OVA를 이 서버의 노드에 올리고, 원본 VM을 읽어 VM 설정을 제안받는다.
    /// 원본 파일은 그 노드에 남고 실행도 그 노드에서 한다.
    Ova {
        file: PathBuf,
        /// 가져오기 이름
        #[arg(long)]
        name: String,
    },
    /// ESXi·vCenter의 데이터스토어에 있는 VM을 보인다. 비밀번호는 `--password-file`이나
    /// `OPL_ESXI_PASSWORD`에서, 없으면 터미널에서 묻는다. 지문을 주지 않으면 서버 인증서의 지문을 알려 준다.
    EsxiBrowse {
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// ESXi·vCenter의 꺼진 VM을 읽어 VM 설정을 제안받는다. 비밀번호는 `esxi-browse`처럼 받고 연결을
    /// 받은 노드의 메모리에만 남으므로, 그 orbivisord를 다시 시작하면 가져오기를 다시 만들어야 한다.
    Esxi {
        /// 가져오기 이름
        name: String,
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문(`import esxi-browse`가 알려 준다)
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        datastore: String,
        /// 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`)
        #[arg(long)]
        vmx: String,
    },
    List,
    /// 원본 VM, 제안, 경고, 마지막 실행의 오류
    Get {
        import: String,
    },
    /// 원본 디스크를 볼륨으로 옮겨 새 VM(꺼짐)을 만든다. 주지 않은 값은 제안 값이다.
    Run(Box<ImportRunArgs>),
    /// 가져오기와 원본 파일을 지운다(만든 VM은 남는다).
    Delete {
        import: String,
        /// 원본 노드가 없거나 오프라인이면 기록만 지운다(원본 파일은 그 노드에 남는다).
        #[arg(long)]
        force: bool,
    },
}

/// ESXi·vCenter 연결. 비밀번호는 인자로 받지 않는다.
#[derive(Args)]
struct EsxiArgs {
    /// ESXi 호스트나 vCenter 주소
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u16,
    #[arg(long)]
    user: String,
    /// vCenter면 데이터센터 이름
    #[arg(long, default_value = "ha-datacenter")]
    datacenter: String,
    /// 비밀번호가 든 파일. 없으면 `OPL_ESXI_PASSWORD`, 그것도 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
}

/// ESXi 비밀번호. `--password-file`, `OPL_ESXI_PASSWORD` 순서로 보고, 둘 다 없으면 터미널에서 묻는다.
fn esxi_password(conn: &EsxiArgs) -> anyhow::Result<String> {
    if let Some(file) = &conn.password_file {
        return read_password(Some(file), "");
    }
    match std::env::var("OPL_ESXI_PASSWORD") {
        Ok(password) => Ok(password),
        Err(_) => Ok(rpassword::prompt_password(format!(
            "password for {}@{}: ",
            conn.user, conn.host
        ))?),
    }
}

/// API의 `EsxiConnection`
fn esxi_connection(conn: &EsxiArgs, fingerprint: Option<&str>, password: &str) -> Value {
    json!({
        "host": conn.host,
        "port": conn.port,
        "user": conn.user,
        "password": password,
        "datacenter": conn.datacenter,
        "fingerprint": fingerprint,
    })
}

/// `import esxi`의 요청 본문
fn esxi_import_body(name: &str, connection: &Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": connection, "datastore": datastore, "vmx": vmx })
}

#[derive(Args)]
struct ImportRunArgs {
    import: String,
    /// 새 VM 이름
    #[arg(long)]
    name: String,
    /// 디스크 볼륨을 만들 풀
    #[arg(long)]
    pool: String,
    /// 원본 네트워크를 Orbivisor 네트워크에 잇는다(예: `"VM Network=lan"`). 원본 NIC의 네트워크마다
    /// 준다. 원본 네트워크가 하나면 `NETWORK`만 줘도 된다.
    #[arg(long = "net", value_name = "SOURCE=NETWORK", value_parser = parse_net)]
    nets: Vec<NetArg>,
    #[arg(long, value_enum)]
    disk_bus: Option<DiskBus>,
    #[arg(long, value_enum)]
    nic_model: Option<NicModel>,
    #[arg(long, value_enum)]
    firmware: Option<Firmware>,
    #[arg(long)]
    sockets: Option<u16>,
    #[arg(long)]
    cores: Option<u16>,
    /// 메모리(MiB)
    #[arg(long)]
    memory: Option<u32>,
    /// 원본 MAC 주소 대신 새 MAC을 받는다.
    #[arg(long)]
    new_macs: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DiskBus {
    VirtioBlk,
    Sata,
}

#[derive(Clone, Copy, ValueEnum)]
enum NicModel {
    Virtio,
    E1000e,
}

/// `--net`으로 받은 매핑. `source`가 없으면 하나뿐인 원본 네트워크다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NetArg {
    source: Option<String>,
    network: String,
}

fn parse_net(s: &str) -> Result<NetArg, String> {
    let bad = || format!("invalid network mapping {s:?}: use SOURCE=NETWORK or NETWORK");
    // 원본 네트워크 이름에 `=`가 있을 수 있다. Orbivisor 네트워크 이름에는 없다.
    let (source, network) = match s.rsplit_once('=') {
        Some(("", _)) => return Err(bad()),
        Some((source, network)) => (Some(source.to_string()), network),
        None => (None, s),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NetArg {
        source,
        network: network.into(),
    })
}

/// 가져오기의 원본 네트워크(제안의 네트워크 매핑 키)
fn source_networks(import: &Value) -> Vec<String> {
    import["status"]["proposal"]["networks"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `import run`의 요청 본문. `sources`는 `=` 없는 `--net`을 이을 원본 네트워크다.
fn import_run_body(args: &ImportRunArgs, sources: &[String]) -> anyhow::Result<Value> {
    let mut networks = serde_json::Map::new();
    for net in &args.nets {
        let source = match (&net.source, sources) {
            (Some(source), _) => source.clone(),
            (None, [only]) => only.clone(),
            (None, _) => bail!(
                "--net {} needs SOURCE=NETWORK: vm-import {} has {} source networks ({})",
                net.network,
                args.import,
                sources.len(),
                sources
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        networks.insert(source, json!(net.network));
    }
    Ok(json!({
        "name": args.name,
        "pool": args.pool,
        "networks": networks,
        "disk_bus": args.disk_bus.map(value_name),
        "nic_model": args.nic_model.map(value_name),
        "firmware": args.firmware.map(value_name),
        "sockets": args.sockets,
        "cores": args.cores,
        "memory_mib": args.memory,
        "keep_macs": !args.new_macs,
    }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// 설정을 `키: 값` 줄로 보인다.
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing|enterprise)
    Set {
        /// 예: `updates.channel=testing`
        #[arg(required = true, value_name = "KEY=VALUE")]
        values: Vec<String>,
    },
}

/// `settings set`이 아는 키
const SETTING_KEYS: &[&str] = &["updates.channel"];

/// `settings set`의 요청 본문(`PATCH /settings`). 준 키만 보낸다.
fn settings_set_body(values: &[String]) -> anyhow::Result<Value> {
    let mut body = json!({});
    for value in values {
        let Some((key, v)) = value.split_once('=') else {
            bail!("expected KEY=VALUE, got {value:?}");
        };
        match key {
            "updates.channel" => body["updates"]["channel"] = json!(v),
            _ => bail!("unknown setting {key}; known: {}", SETTING_KEYS.join(", ")),
        }
    }
    Ok(body)
}

/// 객체의 값들을 `a.b: 값` 줄로 펼친다.
fn setting_lines(prefix: &str, value: &Value, lines: &mut Vec<String>) {
    match value.as_object() {
        Some(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                setting_lines(&key, v, lines);
            }
        }
        None => lines.push(format!("{prefix}: {}", cell(value))),
    }
}

async fn settings(
    client: &Client,
    cmd: SettingsCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SettingsCmd::Get => {
            let settings = client.get("/settings").await?;
            if json_out {
                print_json(out, &settings)?;
            } else {
                let mut lines = Vec::new();
                setting_lines("", &settings["spec"], &mut lines);
                for line in lines {
                    writeln!(out, "{line}")?;
                }
            }
        }
        SettingsCmd::Set { values } => {
            client
                .patch("/settings", &settings_set_body(&values)?)
                .await?;
            writeln!(out, "settings updated")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum SubscriptionCmd {
    /// 구독 상태를 보인다.
    Show,
    /// 구독 키를 넣는다(있던 키는 바꾼다).
    #[command(group(ArgGroup::new("source").required(true).args(["key", "file"])))]
    Set {
        /// `opl1.<payload>.<signature>`
        key: Option<String>,
        /// 키가 든 파일(`-`면 표준 입력)
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 구독 키를 지운다.
    Remove,
}

/// 구독 상태(`SubscriptionInfo`)를 `키: 값` 줄로. 없는 값은 줄을 쓰지 않는다.
fn subscription_lines(info: &Value) -> Vec<String> {
    let mut state = cell(&info["state"]);
    if info["development"] == true {
        state.push_str(" (development key)");
    }
    let mut lines = vec![format!("state: {state}")];
    for key in ["customer", "level"] {
        if !info[key].is_null() {
            lines.push(format!("{key}: {}", cell(&info[key])));
        }
    }
    let used = cell(&info["used_nodes"]);
    lines.push(match info["nodes"].as_u64() {
        Some(limit) => format!("nodes: {used}/{limit}"),
        None => format!("nodes: {used}"),
    });
    if let Some(at) = info["expires_at"].as_u64() {
        lines.push(format!("expires: {}", opl_types::subscription::date(at)));
    }
    lines
}

fn print_subscription(
    out: &mut (dyn Write + Send),
    json_out: bool,
    info: &Value,
) -> anyhow::Result<()> {
    if json_out {
        print_json(out, info)?;
    } else {
        for line in subscription_lines(info) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

async fn subscription(
    client: &Client,
    cmd: SubscriptionCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SubscriptionCmd::Show => {
            let info = client.get("/subscription").await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Set { key, file } => {
            let key = match key {
                Some(key) => key,
                None => match file {
                    Some(path) if path.as_os_str() == "-" => {
                        read_stdin_key(&mut std::io::stdin().lock())?
                    }
                    file => read_password(file.as_ref(), "")?,
                },
            };
            let info = client
                .put("/subscription", &json!({ "key": key.trim() }))
                .await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Remove => {
            client.delete("/subscription").await?;
            writeln!(out, "subscription removed")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum TelemetryCmd {
    /// 텔레메트리 설정과 마지막 전송을 보인다.
    Show,
    /// 텔레메트리를 켠다. 무엇을 보내는지는 `preview`로 먼저 본다.
    Enable {
        /// 보낼 `https://` 주소. 없으면 지금 끝점(설정 또는 빌드 기본값)
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// 텔레메트리를 끈다(id는 남는다).
    Disable,
    /// 지금 보낼 보고를 JSON 그대로 보인다(admin).
    Preview,
    /// 보고에 붙는 id를 새로 만든다.
    ResetId,
}

/// `telemetry enable|disable`의 요청 본문(`PATCH /settings`)
fn telemetry_body(enabled: bool, endpoint: Option<&str>) -> Value {
    let mut body = json!({ "telemetry": { "enabled": enabled } });
    if let Some(endpoint) = endpoint {
        body["telemetry"]["endpoint"] = json!(endpoint);
    }
    body
}

/// 설정 리소스의 텔레메트리 부분을 `키: 값` 줄로. 끝점이 없으면 이 oplctl 빌드의 기본 끝점을 보인다.
fn telemetry_lines(settings: &Value) -> Vec<String> {
    let spec = &settings["spec"]["telemetry"];
    let status = &settings["status"]["telemetry"];
    let endpoint = match spec["endpoint"].as_str() {
        Some(endpoint) => endpoint.to_string(),
        None if !opl_types::version::TELEMETRY_URL.is_empty() => {
            format!("{} (build default)", opl_types::version::TELEMETRY_URL)
        }
        None => "none".into(),
    };
    let last_sent = match status["last_sent_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    vec![
        format!("enabled: {}", spec["enabled"] == true),
        format!("endpoint: {endpoint}"),
        format!("id: {}", status["id"].as_str().unwrap_or("none")),
        format!("last sent: {last_sent}"),
        format!(
            "last error: {}",
            status["last_error"].as_str().unwrap_or("none")
        ),
    ]
}

async fn telemetry(
    client: &Client,
    cmd: TelemetryCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let settings = match cmd {
        TelemetryCmd::Show => client.get("/settings").await?,
        TelemetryCmd::Enable { endpoint } => {
            client
                .patch("/settings", &telemetry_body(true, endpoint.as_deref()))
                .await?
        }
        TelemetryCmd::Disable => {
            client
                .patch("/settings", &telemetry_body(false, None))
                .await?
        }
        TelemetryCmd::Preview => {
            // 보낼 JSON 그대로 보인다.
            let report = client.get("/telemetry/preview").await?;
            print_json(out, &report)?;
            return Ok(());
        }
        TelemetryCmd::ResetId => client.post("/telemetry/reset-id", &json!({})).await?,
    };
    if json_out {
        print_json(out, &settings)?;
    } else {
        for line in telemetry_lines(&settings) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// 원격 클러스터들과 마지막으로 모은 상태(이름, 클러스터, 버전, 노드 online/total, VM running/total)
    List,
    /// 원격 하나의 설정과 상태를 보인다(토큰은 id만).
    Show { name: String },
    /// 원격을 등록한다(admin). 서버가 그 클러스터에 연결해 CA 지문과 토큰을 확인한다.
    Add(RemoteAdd),
    /// 원격을 바꾼다(admin). 준 것만 바꾼다. 주소나 지문을 바꾸면 토큰도 다시 준다.
    Set(RemoteSet),
    /// 원격을 지운다(admin). 원격 클러스터의 토큰은 그대로 남는다.
    Remove { name: String },
    /// 원격 상태를 곧 다시 모아 보인다(admin).
    Refresh { name: String },
    /// 원격 클러스터의 VM 목록(이 클러스터를 거쳐 읽는다)
    Vms { name: String },
    /// 원격 클러스터의 VM을 시작하거나(start) 정지한다(stop — 로컬 `vm stop`처럼 ACPI, `--force`면
    /// 곧바로 끄기). 원격 쪽 권한은 토큰 주인의 역할이 정한다. Task를 기다리지 않는다.
    Vm {
        name: String,
        #[arg(value_enum)]
        action: RemoteVmAction,
        vm: String,
        /// stop에서 ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct RemoteAdd {
    name: String,
    /// 원격 클러스터 노드의 API 주소(`https://<호스트>[:포트]`)
    #[arg(long)]
    url: String,
    /// 원격 클러스터 CA의 지문(`sha256:...`, 원격 노드의 `orbivisorctl cluster status`). 없으면 CA를 받아
    /// 지문을 보이고 확인을 묻는다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 원격 클러스터에서 만든 API 토큰이 든 파일(`-`면 표준 입력 — 그때는 확인을 물을 수 없으므로
    /// `--ca-fingerprint`나 `--yes`가 필요하다)
    #[arg(long)]
    token_file: PathBuf,
    /// 받은 CA 지문을 묻지 않고 믿는다.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct RemoteSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 새 API 토큰이 든 파일(`-`면 표준 입력). 주지 않으면 저장된 토큰을 그대로 둔다.
    #[arg(long)]
    token_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum RemoteVmAction {
    Start,
    Stop,
}

/// 원격 VM 동작이 부르는 원격 API의 동작(`/vms/<VM>/actions/<동작>`)과 보일 이름
fn remote_vm_action(
    action: RemoteVmAction,
    force: bool,
) -> anyhow::Result<(&'static str, &'static str)> {
    Ok(match (action, force) {
        (RemoteVmAction::Start, false) => ("start", "start"),
        (RemoteVmAction::Start, true) => bail!("--force is only for stop"),
        (RemoteVmAction::Stop, false) => ("stop", "stop"),
        (RemoteVmAction::Stop, true) => ("stop?mode=force", "force stop"),
    })
}

/// 토큰 파일(`-`면 표준 입력)의 토큰. 앞뒤 공백을 뗀다.
fn read_remote_token(file: &PathBuf) -> anyhow::Result<String> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    Ok(token)
}

/// 받은 CA 지문을 보이고 확인을 받는다. `--yes`면 묻지 않고, 터미널이 아니면 묻지 못하므로
/// `--ca-fingerprint`로 다시 실행하게 한다.
fn confirm_fingerprint(
    actual: &str,
    yes: bool,
    terminal: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    writeln!(out, "the remote cluster CA fingerprint is {actual}")?;
    if yes {
        return Ok(());
    }
    if !terminal {
        bail!(
            "check the fingerprint (`orbivisorctl cluster status` on a node of the remote cluster) and \
             run again with --ca-fingerprint {actual}"
        );
    }
    write!(out, "Is this the CA of the remote cluster? [y/N] ")?;
    out.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        bail!("cancelled");
    }
    Ok(())
}

/// `<a>/<b>` 모양의 수(`online/total` 등). 모르면 `-`.
fn remote_counts(v: &Value, part: &str) -> String {
    if v.is_null() {
        return "-".into();
    }
    format!("{}/{}", cell(&v[part]), cell(&v["total"]))
}

/// 원격의 상태 칸: 닿으면 `reachable`, 아니면 `unreachable: <이유>`
fn remote_state(status: &Value) -> String {
    if status["reachable"] == true {
        "reachable".into()
    } else {
        format!("unreachable: {}", cell(&status["error"]))
    }
}

/// `remote list`의 열
const REMOTE_HEADERS: &[&str] = &["NAME", "CLUSTER", "VERSION", "NODES", "VMS", "STATUS"];

/// `remote list`의 한 행
fn remote_row(r: &Value) -> Vec<String> {
    let s = &r["status"];
    vec![
        cell(&r["name"]),
        cell(&s["cluster_name"]),
        cell(&s["version"]),
        remote_counts(&s["nodes"], "online"),
        remote_counts(&s["vms"], "running"),
        remote_state(s),
    ]
}

/// `remote show`의 `키: 값` 줄
fn remote_lines(r: &Value) -> Vec<String> {
    let s = &r["status"];
    let checked = match s["checked_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    let memory = match s["memory_gib"].as_u64() {
        Some(gib) => format!("{gib} GiB"),
        None => "-".into(),
    };
    vec![
        format!("name: {}", cell(&r["name"])),
        format!("url: {}", cell(&r["url"])),
        format!("ca fingerprint: {}", cell(&r["ca_fingerprint"])),
        format!("token id: {}", cell(&r["token_id"])),
        format!("status: {}", remote_state(s)),
        format!("changed at: {checked}"),
        format!("cluster: {}", cell(&s["cluster_name"])),
        format!("version: {}", cell(&s["version"])),
        format!("nodes online: {}", remote_counts(&s["nodes"], "online")),
        format!("vms running: {}", remote_counts(&s["vms"], "running")),
        format!("cpus: {}", cell(&s["cpus"])),
        format!("memory: {memory}"),
    ]
}

/// `remote set`의 본문: 지금 원격에 준 것만 바꾼다. 토큰을 주지 않으면 빼서 저장된 토큰을 둔다.
fn remote_set_body(current: &Value, args: &RemoteSet, token: Option<String>) -> Value {
    let mut body = json!({
        "url": args.url.as_deref().map_or_else(|| current["url"].clone(), |u| json!(u)),
        "ca_fingerprint": args
            .ca_fingerprint
            .as_deref()
            .map_or_else(|| current["ca_fingerprint"].clone(), |f| json!(f)),
    });
    if let Some(token) = token {
        body["token"] = json!(token);
    }
    body
}

async fn remote(
    client: &Client,
    cmd: RemoteCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let show = |out: &mut (dyn Write + Send), r: &Value| -> anyhow::Result<()> {
        if json_out {
            print_json(out, r)?;
        } else {
            for line in remote_lines(r) {
                writeln!(out, "{line}")?;
            }
        }
        Ok(())
    };
    match cmd {
        RemoteCmd::List => {
            let list = client.get("/remotes").await?;
            print_list(out, json_out, &list, REMOTE_HEADERS, remote_row)?;
        }
        RemoteCmd::Show { name } => {
            show(out, &client.get(&format!("/remotes/{}", seg(&name))).await?)?
        }
        RemoteCmd::Add(args) => {
            // 표준 입력은 토큰을 읽느라 다 썼으므로 지문 확인을 물을 수 없다.
            if args.token_file.as_os_str() == "-" && args.ca_fingerprint.is_none() && !args.yes {
                bail!("reading the token from stdin needs --ca-fingerprint or --yes");
            }
            let token = read_remote_token(&args.token_file)?;
            let fingerprint = match args.ca_fingerprint {
                Some(f) => f,
                None => {
                    let actual = fingerprint(&fetch_ca(&args.url).await?)?;
                    use std::io::IsTerminal;
                    let stdin = std::io::stdin();
                    let terminal = stdin.is_terminal();
                    confirm_fingerprint(&actual, args.yes, terminal, &mut stdin.lock(), out)?;
                    actual
                }
            };
            let body = json!({
                "name": args.name,
                "url": args.url,
                "ca_fingerprint": fingerprint,
                "token": token,
            });
            let created = client.post("/remotes", &body).await?;
            writeln!(
                out,
                "remote {} added ({})",
                args.name,
                remote_state(&created["status"])
            )?;
        }
        RemoteCmd::Set(args) => {
            let token = args
                .token_file
                .as_ref()
                .map(read_remote_token)
                .transpose()?;
            let path = format!("/remotes/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&remote_set_body(&current, &args, token));
            client.send(r).await?;
            writeln!(out, "remote {} updated", args.name)?;
        }
        RemoteCmd::Remove { name } => {
            client.delete(&format!("/remotes/{}", seg(&name))).await?;
            writeln!(out, "remote {name} removed")?;
        }
        RemoteCmd::Refresh { name } => {
            let r = client
                .post(
                    &format!("/remotes/{}/actions/refresh", seg(&name)),
                    &json!({}),
                )
                .await?;
            show(out, &r)?;
        }
        RemoteCmd::Vms { name } => {
            let list = client
                .get(&format!("/remotes/{}/api/vms", seg(&name)))
                .await?;
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
        RemoteCmd::Vm {
            name,
            action,
            vm,
            force,
        } => {
            let (action, shown) = remote_vm_action(action, force)?;
            let task = client
                .send(client.request(
                    Method::POST,
                    // 동작은 이 파일이 정한 값이고 쿼리를 담으므로 인코딩하지 않는다.
                    &format!(
                        "/remotes/{}/api/vms/{}/actions/{}",
                        seg(&name),
                        seg(&vm),
                        action
                    ),
                ))
                .await?;
            writeln!(
                out,
                "vm {vm} on remote {name}: {shown} requested (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

#[derive(Args)]
struct AuditArgs {
    /// 이 시각부터: `30m`·`1h`·`7d`(지금에서 뺌)나 RFC 3339. 기본은 `--until`의 24시간 전
    #[arg(long)]
    since: Option<String>,
    /// 이 시각까지(RFC 3339). 기본은 지금
    #[arg(long)]
    until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    #[arg(long)]
    user: Option<String>,
    /// 노드 이름
    #[arg(long)]
    node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 든 문자열
    #[arg(long)]
    action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    #[arg(long)]
    limit: Option<u32>,
}

/// `--since`: 기간(`30m`·`1h`·`7d`, 초 `s`도)이면 지금(`now_ms`)에서 뺀 유닉스 밀리초, 아니면
/// 그대로(RFC 3339나 밀리초) 보내 서버가 푼다.
fn audit_since(s: &str, now_ms: u64) -> String {
    match audit_duration_ms(s) {
        Some(ms) => now_ms.saturating_sub(ms).to_string(),
        None => s.to_string(),
    }
}

/// `30m`·`1h`·`7d`(초 `s`도) 같은 기간의 밀리초. 기간이 아니면 None
fn audit_duration_ms(s: &str) -> Option<u64> {
    let unit = match s.chars().last()? {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let n = s[..s.len() - 1].parse::<u64>().ok()?;
    Some(n.saturating_mul(unit))
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn unix_ms_time(ms: u64) -> String {
    match time::OffsetDateTime::from_unix_timestamp((ms / 1000) as i64) {
        Ok(t) => format!(
            "{}T{:02}:{:02}:{:02}Z",
            t.date(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => ms.to_string(),
    }
}

/// `audit`의 표 한 줄
fn audit_row(e: &Value) -> Vec<String> {
    // 대상이 없으면 detail(예: `audit.dropped`의 수, 로그인의 `username=…`)
    let target = match (e["target"]["kind"].as_str(), e["target"]["key"].as_str()) {
        (Some(kind), Some(key)) => format!("{kind}/{key}"),
        _ => e["detail"].as_str().unwrap_or("-").to_string(),
    };
    vec![
        e["time"].as_u64().map_or_else(|| "-".into(), unix_ms_time),
        cell(&e["node"]),
        cell(&e["user"]),
        cell(&e["ip"]),
        cell(&e["action"]),
        target,
        cell(&e["status"]),
    ]
}

/// `GET /audit`를 부르고 표로 보인다. 읽지 못한 노드, 감사 로그를 끈 노드와 잘림은 표 뒤
/// `err`(stderr)에 알린다.
async fn audit(
    client: &Client,
    args: AuditArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    if args.until.is_some()
        && args
            .since
            .as_deref()
            .is_some_and(|s| audit_duration_ms(s).is_some())
    {
        bail!("a relative --since cannot be combined with --until; give a time");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let query: Vec<(&str, String)> = [
        ("since", args.since.map(|s| audit_since(&s, now_ms))),
        ("until", args.until),
        ("user", args.user),
        ("node", args.node),
        ("action", args.action),
        ("limit", args.limit.map(|n| n.to_string())),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();
    let mut r = client.request(Method::GET, "/audit");
    if !query.is_empty() {
        r = r.query(&query);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = resp["entries"]
        .as_array()
        .map(|a| a.iter().map(audit_row).collect())
        .unwrap_or_default();
    table(
        out,
        &["TIME", "NODE", "USER", "IP", "ACTION", "TARGET", "STATUS"],
        &rows,
    )?;
    let unreachable: Vec<&str> = resp["unreachable"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !unreachable.is_empty() {
        writeln!(
            err,
            "warning: no audit records from: {}",
            unreachable.join(", ")
        )?;
    }
    let disabled: Vec<&str> = resp["disabled"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !disabled.is_empty() {
        writeln!(
            err,
            "note: the audit log is disabled on: {}",
            disabled.join(", ")
        )?;
    }
    if resp["truncated"] == true {
        writeln!(
            err,
            "note: more entries match; narrow the range or raise --limit"
        )?;
    }
    Ok(())
}

/// `--watch`가 다시 읽는 간격(측정 간격과 같다)
const STATS_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// CPU 사용률 칸(`12.3%`)
fn percent_cell(v: &Value) -> String {
    v.as_f64()
        .map_or_else(|| "-".into(), |n| format!("{n:.1}%"))
}

/// MiB 값을 사람이 읽기 쉽게
fn mib_cell(v: &Value) -> String {
    v.as_u64()
        .map_or_else(|| "-".into(), |n| bytes(&json!(n.saturating_mul(1 << 20))))
}

/// 사용/전체 MiB 칸(`1.0GiB/4.0GiB`). 둘 중 하나라도 없으면 `-`
fn mib_pair_cell(used: &Value, total: &Value) -> String {
    if used.is_null() || total.is_null() {
        return "-".into();
    }
    format!("{}/{}", mib_cell(used), mib_cell(total))
}

/// 바이트/초 칸(`1.5KiB/s`)
fn rate_cell(v: &Value) -> String {
    v.as_f64().map_or_else(
        || "-".into(),
        |n| format!("{}/s", bytes(&json!(n.max(0.0).round() as u64))),
    )
}

/// 초당 횟수 칸
fn iops_cell(v: &Value) -> String {
    v.as_f64().map_or_else(|| "-".into(), |n| format!("{n:.1}"))
}

/// 점의 측정 시각 칸
fn sample_time(s: &Value) -> String {
    s["t"].as_u64().map_or_else(|| "-".into(), unix_ms_time)
}

/// 지표 요청을 보낸다. `--watch`면 일시 오류는 stderr에 찍고 `None`이다(다음 회차에 다시 읽는다).
/// 401·403·404(로그인·권한·없는 대상)는 다시 읽어도 같으므로 끝낸다.
async fn send_stats_request(
    client: &Client,
    r: reqwest::RequestBuilder,
    watch: bool,
    err: &mut dyn Write,
) -> anyhow::Result<Option<Value>> {
    match client.send(r).await {
        Ok(resp) => Ok(Some(resp)),
        Err(e) if watch && !ends_stats_watch(&e) => {
            writeln!(err, "error: {e:#}")?;
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// `--watch`를 끝내는 오류(401·403·404)
fn ends_stats_watch(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NotLoggedIn>().is_some()
        || e.downcast_ref::<ApiError>().is_some_and(|api| {
            matches!(
                api.code.as_str(),
                "unauthorized" | "forbidden" | "not_found"
            )
        })
}

/// `-o json`: 응답 그대로. `--watch`면 회차마다 한 줄(JSON Lines)이다.
fn print_stats_json(out: &mut dyn Write, resp: &Value, watch: bool) -> anyhow::Result<()> {
    if watch {
        writeln!(out, "{}", serde_json::to_string(resp)?)?;
    } else {
        print_json(out, resp)?;
    }
    Ok(())
}

/// 지표 응답의 `unreachable`(답하지 않은 노드)·`unsupported`(옛 노드)를 `err`(stderr)에 알린다.
fn metrics_warnings(resp: &Value, err: &mut dyn Write) -> std::io::Result<()> {
    let names = |key: &str| -> Vec<&str> {
        resp[key]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    let unreachable = names("unreachable");
    if !unreachable.is_empty() {
        writeln!(err, "warning: no metrics from: {}", unreachable.join(", "))?;
    }
    let unsupported = names("unsupported");
    if !unsupported.is_empty() {
        writeln!(
            err,
            "note: these nodes do not report metrics yet: {}",
            unsupported.join(", ")
        )?;
    }
    Ok(())
}

/// `node stats`의 열
const NODE_STATS_HEADERS: &[&str] = &[
    "NODE",
    "TIME",
    "CPU",
    "MEMORY",
    "DISK READ",
    "DISK WRITE",
    "NET RX",
    "NET TX",
    "DATA FS",
];

/// `node stats`의 한 행. 점이 없으면(`null`) 이름 말고는 모두 `-`다.
fn node_stats_row(name: &str, s: &Value) -> Vec<String> {
    vec![
        name.to_string(),
        sample_time(s),
        percent_cell(&s["cpu"]),
        mib_pair_cell(&s["mem_used_mib"], &s["mem_total_mib"]),
        rate_cell(&s["disk_read_bps"]),
        rate_cell(&s["disk_write_bps"]),
        rate_cell(&s["net_rx_bps"]),
        rate_cell(&s["net_tx_bps"]),
        mib_pair_cell(&s["fs_used_mib"], &s["fs_total_mib"]),
    ]
}

/// `node stats`가 읽기마다 이어 쓰는 것
#[derive(Default)]
struct NodeStatsState {
    /// 노드 id → 이름
    names: std::collections::HashMap<String, String>,
    /// 노드 id → 받은 마지막 점
    last: std::collections::HashMap<String, Value>,
    /// 지금까지 응답에 나온 노드 id
    seen: std::collections::HashSet<String>,
    /// 지금까지 본 노드 가운데 직전 응답에 빠진 노드 id
    missing: std::collections::HashSet<String>,
    /// 다음 읽기의 `since`: 응답에 나온 노드의 마지막 `t` 가운데 가장 이른 것(빠진 노드는 셈에서 뺀다).
    /// 점이 없는 노드가 있거나 빠졌던 노드가 이번에 돌아왔으면 없음 — 돌아온 노드의 시계가 늦어도 그 줄이
    /// 멈추지 않게 한 번은 모든 점을 받는다.
    since: Option<i64>,
}

/// `node stats`: `GET /metrics/nodes`를 읽어 보인다. `watch`면 5초마다 새 점만 다시 읽어 새 표를 이어 찍는다.
async fn node_stats(
    client: &Client,
    watch: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let mut state = NodeStatsState::default();
    if !json_out {
        for n in items(&client.get("/nodes").await?) {
            state.names.insert(cell(&n["id"]), cell(&n["name"]));
        }
    }
    loop {
        node_stats_round(client, &mut state, watch, json_out, out, err).await?;
        if !watch {
            return Ok(());
        }
        tokio::time::sleep(STATS_EVERY).await;
        if !json_out {
            writeln!(out)?;
        }
    }
}

/// `node stats`의 읽기 한 번. 표는 응답의 노드마다 그 노드의 마지막 점이다(이번에 새 점이 없으면 전에 받은
/// 점). `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
async fn node_stats_round(
    client: &Client,
    state: &mut NodeStatsState,
    watch: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let mut r = client.request(Method::GET, "/metrics/nodes");
    if let Some(since) = state.since {
        r = r.query(&[("since", since)]);
    }
    let Some(resp) = send_stats_request(client, r, watch, err).await? else {
        return Ok(());
    };
    let nodes = resp["nodes"].as_array().cloned().unwrap_or_default();
    let ids: std::collections::HashSet<String> = nodes.iter().map(|n| cell(&n["node"])).collect();
    let returned = state.missing.iter().any(|id| ids.contains(id));
    state.missing = state.seen.difference(&ids).cloned().collect();
    state.seen.extend(ids);
    let mut since: Option<Option<i64>> = returned.then_some(None);
    for n in &nodes {
        let id = cell(&n["node"]);
        if let Some(s) = n["samples"].as_array().and_then(|a| a.last()) {
            state.last.insert(id.clone(), s.clone());
        }
        let t = state.last.get(&id).and_then(|s| s["t"].as_i64());
        since = Some(match since {
            None => t,
            Some(m) => m.zip(t).map(|(m, t)| m.min(t)),
        });
    }
    state.since = since.flatten();
    if json_out {
        return print_stats_json(out, &resp, watch);
    }
    let rows: Vec<Vec<String>> = nodes
        .iter()
        .map(|n| {
            let id = cell(&n["node"]);
            let name = state.names.get(&id).unwrap_or(&id);
            node_stats_row(name, state.last.get(&id).unwrap_or(&Value::Null))
        })
        .collect();
    table(out, NODE_STATS_HEADERS, &rows)?;
    metrics_warnings(&resp, err)?;
    Ok(())
}

/// `vm stats`의 열
const VM_STATS_HEADERS: &[&str] = &[
    "TIME",
    "CPU",
    "MEMORY",
    "DISK READ",
    "DISK WRITE",
    "READ IOPS",
    "WRITE IOPS",
    "NET RX",
    "NET TX",
];

/// `vm stats`의 한 행
fn vm_stats_row(s: &Value) -> Vec<String> {
    vec![
        sample_time(s),
        percent_cell(&s["cpu"]),
        mib_cell(&s["mem_rss_mib"]),
        rate_cell(&s["disk_read_bps"]),
        rate_cell(&s["disk_write_bps"]),
        iops_cell(&s["disk_read_iops"]),
        iops_cell(&s["disk_write_iops"]),
        rate_cell(&s["net_rx_bps"]),
        rate_cell(&s["net_tx_bps"]),
    ]
}

/// `vm stats`가 읽기마다 이어 쓰는 것
#[derive(Default)]
struct VmStatsState {
    /// 받은 마지막 점
    last: Option<Value>,
    /// 다음 읽기의 `since`(마지막 점의 `t`)
    since: Option<i64>,
}

/// `vm stats`: `GET /vms/{key}/metrics`를 읽어 보인다. `watch`면 5초마다 새 점만 다시 읽어 새 표를 이어 찍는다.
async fn vm_stats(
    client: &Client,
    vm: &str,
    watch: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let mut state = VmStatsState::default();
    loop {
        vm_stats_round(client, vm, &mut state, watch, json_out, out, err).await?;
        if !watch {
            return Ok(());
        }
        tokio::time::sleep(STATS_EVERY).await;
        if !json_out {
            writeln!(out)?;
        }
    }
}

/// `vm stats`의 읽기 한 번. 표는 마지막 점 한 줄이다(이번에 새 점이 없으면 전에 받은 점). 점이 하나도 없으면
/// 머리만 찍고 stderr에 알린다. `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
async fn vm_stats_round(
    client: &Client,
    vm: &str,
    state: &mut VmStatsState,
    watch: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let mut r = client.request(Method::GET, &format!("/vms/{}/metrics", seg(vm)));
    if let Some(since) = state.since {
        r = r.query(&[("since", since)]);
    }
    let Some(resp) = send_stats_request(client, r, watch, err).await? else {
        return Ok(());
    };
    if let Some(s) = resp["samples"].as_array().and_then(|a| a.last()) {
        state.since = s["t"].as_i64();
        state.last = Some(s.clone());
    }
    if json_out {
        return print_stats_json(out, &resp, watch);
    }
    let rows: Vec<Vec<String>> = state.last.iter().map(vm_stats_row).collect();
    table(out, VM_STATS_HEADERS, &rows)?;
    metrics_warnings(&resp, err)?;
    if state.last.is_none() {
        writeln!(
            err,
            "note: no metrics yet; the VM is off or started less than 10 seconds ago"
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

#[derive(Subcommand)]
enum RealmCmd {
    List,
    /// 영역 설정을 보인다(서비스 계정 비밀번호·클라이언트 비밀은 있는지만).
    Show {
        name: String,
    },
    /// LDAP 영역을 만든다.
    AddLdap(RealmAdd),
    /// LDAP 영역을 바꾼다. 준 것만 바꾼다.
    SetLdap(RealmSet),
    /// OIDC 영역을 만든다. IdP에는 `<노드 주소>/api/v1/auth/oidc/callback`을 리디렉트 URI로 등록한다.
    AddOidc(RealmAddOidc),
    /// OIDC 영역을 바꾼다. 준 것만 바꾼다.
    SetOidc(RealmSetOidc),
    /// 영역을 지운다. 그 영역 사용자들의 세션과 토큰은 멈춘다.
    Remove {
        name: String,
    },
    /// 영역을 시험한다(사용자를 만들지 않는다). LDAP은 사용자 이름과 비밀번호로 디렉터리를,
    /// OIDC는 입력 없이 IdP의 discovery 문서와 키를 확인한다.
    Test {
        name: String,
        /// 영역 안의 사용자 이름(`@영역` 없이). LDAP 영역에는 꼭 필요하고, OIDC 영역이면 무시한다(경고).
        #[arg(long)]
        user: Option<String>,
        /// 비밀번호가 든 파일(`-`면 표준 입력). 없으면 터미널에서 묻는다.
        #[arg(long, requires = "user")]
        password_file: Option<PathBuf>,
    },
}

#[derive(Args)]
struct RealmAdd {
    name: String,
    /// `ldap://host[:port]`(StartTLS) 또는 `ldaps://host[:port]`
    #[arg(long)]
    url: String,
    /// 사용자를 찾을 기준 DN
    #[arg(long)]
    base_dn: String,
    #[command(flatten)]
    ldap: LdapArgs,
}

#[derive(Args)]
struct RealmSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    base_dn: Option<String>,
    #[command(flatten)]
    ldap: LdapArgs,
    /// 저장된 서비스 계정 비밀번호를 지운다. 서비스 계정 DN이 남으면 안 된다(`--clear-bind-dn`).
    #[arg(long, conflicts_with = "bind_password_file")]
    clear_bind_password: bool,
    /// 서비스 계정 DN과 비밀번호를 지운다(익명 검색).
    #[arg(long, conflicts_with_all = ["bind_dn", "bind_password_file"])]
    clear_bind_dn: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

#[derive(Args)]
struct RealmAddOidc {
    name: String,
    /// IdP의 issuer(`https://...`). discovery 문서의 `issuer`와 글자 그대로 같아야 한다.
    #[arg(long)]
    issuer: String,
    /// IdP에 등록한 클라이언트 ID
    #[arg(long)]
    client_id: String,
    #[command(flatten)]
    oidc: OidcArgs,
}

#[derive(Args)]
struct RealmSetOidc {
    name: String,
    #[arg(long)]
    issuer: Option<String>,
    #[arg(long)]
    client_id: Option<String>,
    #[command(flatten)]
    oidc: OidcArgs,
    /// 저장된 클라이언트 비밀을 지운다(공개 클라이언트).
    #[arg(long, conflicts_with = "client_secret_file")]
    clear_client_secret: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

/// `realm add-oidc`와 `set-oidc`의 공통 선택
#[derive(Args)]
struct OidcArgs {
    /// 클라이언트 비밀이 든 파일(`-`면 표준 입력). 없으면 공개 클라이언트(PKCE만)
    #[arg(long)]
    client_secret_file: Option<PathBuf>,
    /// 요청할 scope(여러 번, `openid`가 있어야 함). set-oidc에서 주면 목록 전체를 바꾼다.
    #[arg(long = "scope", value_name = "SCOPE")]
    scopes: Vec<String>,
    /// 사용자 이름을 담은 클레임(기본 preferred_username)
    #[arg(long)]
    username_claim: Option<String>,
    /// 그룹(문자열 배열)을 담은 클레임(기본 groups)
    #[arg(long)]
    groups_claim: Option<String>,
    /// IdP 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// IdP 요청마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹 클레임 값을 가진 사용자의 역할(`<그룹>=<role>`, 여러 번). set-oidc에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "GROUP=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `realm add-ldap`과 `set-ldap`의 공통 선택
#[derive(Args)]
struct LdapArgs {
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[arg(long)]
    bind_dn: Option<String>,
    /// 서비스 계정 비밀번호가 든 파일(`-`면 표준 입력)
    #[arg(long)]
    bind_password_file: Option<PathBuf>,
    /// 사용자 찾기 필터. `{username}`이 로그인 이름으로 바뀐다.
    #[arg(long)]
    user_filter: Option<String>,
    /// 그룹 DN을 담은 사용자 속성(기본 memberOf)
    #[arg(long)]
    group_attr: Option<String>,
    /// 서버 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// 연결과 작업마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹에 든 사용자의 역할(`<그룹 DN>=<role>`, 여러 번). set-ldap에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "DN=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `--group`의 `<그룹 DN>=<role>`. DN에 `=`가 있으므로 마지막 `=`에서 나눈다.
fn parse_group_role(s: &str) -> Result<(String, Role), String> {
    let (group, role) = s
        .rsplit_once('=')
        .ok_or_else(|| format!("invalid group {s:?}: use <group DN>=<role>"))?;
    let role = Role::from_str(role, true)
        .map_err(|_| format!("invalid role {role:?} in {s:?}: use admin, operator or viewer"))?;
    if group.trim().is_empty() {
        return Err(format!("invalid group {s:?}: use <group DN>=<role>"));
    }
    Ok((group.to_string(), role))
}

/// 파일에서 읽은 영역 값들(서비스 계정 비밀번호, CA)
#[derive(Default)]
struct LdapFiles {
    bind_password: Option<String>,
    ca_pem: Option<String>,
}

impl LdapFiles {
    fn read(args: &LdapArgs) -> anyhow::Result<Self> {
        Ok(Self {
            bind_password: args
                .bind_password_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// 비밀번호 파일(`-`면 표준 입력). 끝의 줄바꿈만 뗀다.
fn read_secret_file(path: &PathBuf) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .context("cannot read standard input")?;
        return Ok(s.trim_end_matches(['\r', '\n']).to_string());
    }
    read_password(Some(path), "")
}

fn group_roles(groups: &[(String, Role)]) -> Value {
    groups
        .iter()
        .map(|(group, role)| json!({ "group": group, "role": value_name(*role) }))
        .collect()
}

/// `realm add-ldap`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_body(args: &RealmAdd, files: LdapFiles) -> Value {
    let l = &args.ldap;
    let mut backend = json!({ "kind": "ldap", "url": args.url, "base_dn": args.base_dn });
    let optional = [
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("bind_password", files.bind_password.map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&l.groups, l.default_role)
    });
    if let Some(display) = &l.display {
        body["display"] = json!(display);
    }
    body
}

/// 만들 영역의 그룹 → 역할 표와 기본 역할
fn new_roles(groups: &[(String, Role)], default: Option<Role>) -> Value {
    let mut roles = json!({ "groups": group_roles(groups) });
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    roles
}

/// 바꿀 영역의 역할: 지금 표(`current`)에 준 것만 바꾼다.
fn changed_roles(
    current: &Value,
    groups: &[(String, Role)],
    default: Option<Role>,
    clear_groups: bool,
    clear_default_role: bool,
) -> Value {
    let mut roles = current.clone();
    if !groups.is_empty() {
        roles["groups"] = group_roles(groups);
    }
    if clear_groups {
        roles["groups"] = json!([]);
    }
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    if clear_default_role && let Some(r) = roles.as_object_mut() {
        r.remove("default");
    }
    roles
}

/// `set-ldap`·`set-oidc`가 바꿀 영역의 종류가 맞는지
fn ensure_kind(current: &Value, name: &str, kind: &str) -> anyhow::Result<()> {
    if current["backend"]["kind"] != kind {
        anyhow::bail!(
            "realm {name} is not an {} realm (its kind is {})",
            kind.to_uppercase(),
            cell(&current["backend"]["kind"])
        );
    }
    Ok(())
}

/// 파일에서 읽은 OIDC 영역 값들(클라이언트 비밀, CA)
#[derive(Default)]
struct OidcFiles {
    client_secret: Option<String>,
    ca_pem: Option<String>,
}

impl OidcFiles {
    fn read(args: &OidcArgs) -> anyhow::Result<Self> {
        Ok(Self {
            client_secret: args
                .client_secret_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// `realm add-oidc`·`set-oidc`가 준 선택 값들(이름과 값)
fn oidc_options(o: &OidcArgs, files: OidcFiles) -> [(&'static str, Option<Value>); 6] {
    [
        ("client_secret", files.client_secret.map(Value::from)),
        (
            "scopes",
            (!o.scopes.is_empty()).then(|| Value::from(o.scopes.clone())),
        ),
        ("username_claim", o.username_claim.clone().map(Value::from)),
        ("groups_claim", o.groups_claim.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", o.timeout.map(Value::from)),
    ]
}

/// `realm add-oidc`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_oidc_body(args: &RealmAddOidc, files: OidcFiles) -> Value {
    let o = &args.oidc;
    let mut backend = json!({ "kind": "oidc", "issuer": args.issuer, "client_id": args.client_id });
    for (key, value) in oidc_options(o, files) {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&o.groups, o.default_role)
    });
    if let Some(display) = &o.display {
        body["display"] = json!(display);
    }
    body
}

/// `realm set-oidc`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 클라이언트 비밀은 주지 않으면 빼서(서버가 저장된 값을 둔다 — issuer·client_id·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_oidc_body(
    current: &Value,
    args: &RealmSetOidc,
    files: OidcFiles,
) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "oidc")?;
    let o = &args.oidc;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_client_secret");
    }
    let mut changes = vec![
        ("issuer", args.issuer.clone().map(Value::from)),
        ("client_id", args.client_id.clone().map(Value::from)),
    ];
    changes.extend(oidc_options(o, files));
    if args.clear_client_secret {
        changes.push(("client_secret", Some(json!(""))));
    }
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if args.clear_ca
        && let Some(b) = backend.as_object_mut()
    {
        b.remove("ca_pem");
    }
    let roles = changed_roles(
        &current["roles"],
        &o.groups,
        o.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = o
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// `realm set-ldap`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 서비스 계정 비밀번호는 주지 않으면 빼서(서버가 저장된 값을 둔다 — url·bind_dn·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_body(current: &Value, args: &RealmSet, files: LdapFiles) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "ldap")?;
    let l = &args.ldap;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_bind_password");
    }
    let changes = [
        ("url", args.url.clone().map(Value::from)),
        ("base_dn", args.base_dn.clone().map(Value::from)),
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
        (
            "bind_password",
            if args.clear_bind_password || args.clear_bind_dn {
                Some(json!(""))
            } else {
                files.bind_password.map(Value::from)
            },
        ),
    ];
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if let Some(b) = backend.as_object_mut() {
        if args.clear_bind_dn {
            b.remove("bind_dn");
        }
        if args.clear_ca {
            b.remove("ca_pem");
        }
        // 서비스 계정 DN에는 비밀번호가 있어야 한다.
        if args.clear_bind_password && b.get("bind_dn").is_some_and(Value::is_string) {
            anyhow::bail!(
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
    }
    let roles = changed_roles(
        &current["roles"],
        &l.groups,
        l.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = l
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// OIDC 영역의 콜백 경로. IdP에는 노드 주소 뒤에 붙여 리디렉트 URI로 등록한다.
const OIDC_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";

/// `realm show`의 `키: 값` 줄
fn realm_lines(realm: &Value) -> Vec<String> {
    let b = &realm["backend"];
    let set = |v: bool| if v { "set" } else { "none" };
    let ca = if b["ca_pem"].is_string() {
        "custom (PEM)"
    } else {
        "system roots"
    };
    let mut lines = vec![
        format!("name: {}", cell(&realm["name"])),
        format!("display: {}", cell(&realm["display"])),
        format!("kind: {}", cell(&b["kind"])),
    ];
    if b["kind"] == "oidc" {
        let scopes: Vec<String> = b["scopes"]
            .as_array()
            .map(|a| a.iter().map(cell).collect())
            .unwrap_or_default();
        lines.extend([
            format!("issuer: {}", cell(&b["issuer"])),
            format!("client id: {}", cell(&b["client_id"])),
            format!("client secret: {}", set(b["has_client_secret"] == true)),
            format!("scopes: {}", scopes.join(" ")),
            format!("username claim: {}", cell(&b["username_claim"])),
            format!("groups claim: {}", cell(&b["groups_claim"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
            // IdP에는 노드 주소 뒤에 이 경로를 붙여 리디렉트 URI로 등록한다.
            format!("callback path: {OIDC_CALLBACK_PATH}"),
        ]);
    } else {
        lines.extend([
            format!("url: {}", cell(&b["url"])),
            format!("base dn: {}", cell(&b["base_dn"])),
            format!("bind dn: {}", cell(&b["bind_dn"])),
            format!("bind password: {}", set(b["has_bind_password"] == true)),
            format!("user filter: {}", cell(&b["user_filter"])),
            format!("group attribute: {}", cell(&b["group_attr"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
        ]);
    }
    let groups = realm["roles"]["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if groups.is_empty() {
        lines.push("groups: none".into());
    } else {
        lines.push("groups:".into());
        for g in &groups {
            lines.push(format!("  {} = {}", cell(&g["group"]), cell(&g["role"])));
        }
    }
    lines.push(format!(
        "default role: {}",
        realm["roles"]["default"].as_str().unwrap_or("none")
    ));
    lines
}

async fn realm(
    client: &Client,
    cmd: RealmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        RealmCmd::List => {
            let list = client.get("/realms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "DISPLAY", "KIND", "URL"],
                |r| {
                    vec![
                        cell(&r["name"]),
                        cell(&r["display"]),
                        cell(&r["backend"]["kind"]),
                        // OIDC 영역은 issuer
                        cell(if r["backend"]["kind"] == "oidc" {
                            &r["backend"]["issuer"]
                        } else {
                            &r["backend"]["url"]
                        }),
                    ]
                },
            )?;
        }
        RealmCmd::Show { name } => {
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            if json_out {
                print_json(out, &realm)?;
            } else {
                for line in realm_lines(&realm) {
                    writeln!(out, "{line}")?;
                }
            }
        }
        RealmCmd::AddLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            client
                .post("/realms", &realm_add_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::AddOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            client
                .post("/realms", &realm_add_oidc_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_oidc_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::Remove { name } => {
            client.delete(&format!("/realms/{}", seg(&name))).await?;
            writeln!(out, "realm {name} removed")?;
        }
        RealmCmd::Test {
            name,
            user,
            password_file,
        } => {
            // OIDC 영역은 입력 없이 시험한다(LDAP 영역이면 서버가 사용자를 달라고 한다).
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            let kind = realm["backend"]["kind"].as_str().unwrap_or_default();
            if kind == "oidc" && user.is_some() {
                eprintln!("warning: --user is ignored for an OIDC realm");
            }
            let body = match &user {
                Some(user) if kind != "oidc" => {
                    let password = match &password_file {
                        Some(f) => read_secret_file(f)?,
                        None => read_password(None, "Password: ")?,
                    };
                    json!({ "username": user, "password": password })
                }
                _ => json!({}),
            };
            let result = client
                .post(&format!("/realms/{}/actions/test", seg(&name)), &body)
                .await?;
            let steps = result["steps"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &result)?;
            } else {
                let rows: Vec<Vec<String>> = steps
                    .iter()
                    .map(|s| {
                        let ok = if s["ok"] == true { "ok" } else { "failed" };
                        vec![cell(&s["name"]), ok.into(), cell(&s["message"])]
                    })
                    .collect();
                table(out, &["STEP", "RESULT", "MESSAGE"], &rows)?;
            }
            // 사용자 결과(DN·그룹·역할)는 LDAP 시험에만 있다.
            if !json_out && kind == "ldap" {
                writeln!(out, "dn: {}", cell(&result["dn"]))?;
                let groups: Vec<String> = result["groups"]
                    .as_array()
                    .map(|a| a.iter().map(cell).collect())
                    .unwrap_or_default();
                writeln!(
                    out,
                    "groups: {}",
                    if groups.is_empty() {
                        "-".into()
                    } else {
                        groups.join(", ")
                    }
                )?;
                writeln!(out, "role: {}", result["role"].as_str().unwrap_or("none"))?;
            }
            if let Some(failed) = steps.iter().find(|s| s["ok"] != true) {
                bail!("realm {name}: the {} step failed", cell(&failed["name"]));
            }
        }
    }
    Ok(())
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

/// 표준 입력의 구독 키(앞뒤 공백을 다듬음). 키를 명령줄에 두지 않으려고 쓴다.
fn read_stdin_key(input: &mut dyn std::io::Read) -> anyhow::Result<String> {
    let mut key = String::new();
    input
        .read_to_string(&mut key)
        .context("cannot read the key from standard input")?;
    Ok(key.trim().to_string())
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{}", seg(pool))).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let (token, name) = match &args.token_file {
            Some(file) => token_login(&server, file, ca_pem.clone()).await?,
            None => login(&server, args, ca_pem.clone()).await?,
        };
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        // 새 토큰과 같으면 두고, 이 CLI의 login이 만든 토큰(`oplctl-…`)만 지운다(`--token-file`로 넣은
        // 토큰은 사용자가 웹 UI에서 만든 것이다).
        if let Some((old, old_server)) = previous
            && old != token
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(old), ca_pem)?;
            remove_login_token(&client, id).await;
        }
        writeln!(out, "logged in to {server} as {name}")?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `orbivisorctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            // login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 이미 지워진 토큰이어도 로컬에서는 지운다.
            let kept = match config.token.as_deref().and_then(token_id) {
                Some(id) => remove_login_token(&client, id).await == Some(false),
                None => false,
            };
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
            if kept {
                writeln!(
                    out,
                    "the token stays valid on the server; delete it in the web UI"
                )?;
            }
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{}", seg(&node))).await?)?
            }
            NodeCmd::Updates { node, refresh } => {
                node_updates(&client, &node, refresh, json_out, out).await?
            }
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
            NodeCmd::Maintenance { node, state } => {
                node_maintenance(
                    &client,
                    &node,
                    state == Switch::On,
                    out,
                    &mut std::io::stderr(),
                )
                .await?
            }
            NodeCmd::Drain { node, wait } => node_drain(&client, &node, wait, out).await?,
            NodeCmd::Stats(stats) => {
                node_stats(&client, stats.watch, json_out, out, &mut std::io::stderr()).await?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client
                    .get(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(set) => {
                client
                    .patch(
                        &format!("/storage-pools/{}", seg(&set.pool)),
                        &pool_set_body(&set),
                    )
                    .await?;
                writeln!(out, "storage pool {}: {}", set.pool, pool_set_summary(&set))?;
            }
            PoolCmd::Delete { pool } => {
                client
                    .delete(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => print_json(
                out,
                &client.get(&format!("/networks/{}", seg(&network))).await?,
            )?,
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client
                    .delete(&format!("/networks/{}", seg(&network)))
                    .await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::SecurityGroup { cmd } => security_group(&client, cmd, json_out, out).await?,
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => print_json(
                out,
                &client.get(&format!("/volumes/{}", seg(&volume))).await?,
            )?,
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{}/resize", seg(&volume)), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{}", seg(&volume))).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Import { cmd } => import(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{}", seg(&task))).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{}", seg(&name)), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{}", seg(&name))).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{}", seg(&token))).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
        Cmd::Realm { cmd } => realm(&client, cmd, json_out, out).await?,
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
        Cmd::Remote { cmd } => remote(&client, cmd, json_out, out).await?,
    }
    Ok(())
}

async fn security_group(
    client: &Client,
    cmd: SecurityGroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SecurityGroupCmd::List => {
            let list = client.get("/security-groups").await?;
            print_list(
                out,
                json_out,
                &list,
                SECURITY_GROUP_HEADERS,
                security_group_row,
            )?;
        }
        SecurityGroupCmd::Get { group } => print_json(
            out,
            &client
                .get(&format!("/security-groups/{}", seg(&group)))
                .await?,
        )?,
        SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } => {
            let body = security_group_create_body(&name, description.as_deref(), &rules);
            client.post("/security-groups", &body).await?;
            writeln!(out, "security group {name} created")?;
        }
        SecurityGroupCmd::Delete { group } => {
            client
                .delete(&format!("/security-groups/{}", seg(&group)))
                .await?;
            writeln!(out, "security group {group} deleted")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Add { group, rule, at },
        } => {
            patch_rules(client, &group, |g| rules_with(g, rule, at)).await?;
            writeln!(out, "security group {group}: rule added")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Rm { group, n },
        } => {
            patch_rules(client, &group, |g| rules_without(g, n)).await?;
            writeln!(out, "security group {group}: rule {n} removed")?;
        }
    }
    Ok(())
}

async fn backup(
    client: &Client,
    cmd: BackupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupCmd::Create {
            vm,
            pool,
            name,
            incremental,
        } => backup_create(client, &vm, &pool, name, incremental, out).await?,
        BackupCmd::List { vm } => {
            let mut r = client.request(Method::GET, "/backups");
            if let Some(vm) = &vm {
                r = r.query(&[("vm", vm)]);
            }
            let list = client.send(r).await?;
            print_list(out, json_out, &list, BACKUP_HEADERS, backup_row)?;
        }
        BackupCmd::Get { backup } => {
            let mut b = client.get(&format!("/backups/{}", seg(&backup))).await?;
            if b["status"]["parent"].is_string() {
                b["chain"] = backup_chain(&b, &items(&client.get("/backups").await?));
            }
            print_json(out, &b)?
        }
        BackupCmd::Delete { backup, force } => backup_delete(client, &backup, force, out).await?,
        BackupCmd::Restore {
            backup,
            name,
            pool,
            node,
        } => {
            let body = json!({ "name": name, "pool": pool, "node": node });
            let task = client
                .post(&format!("/backups/{}/restore", seg(&backup)), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            writeln!(out, "vm {name} restored from backup {backup}")?;
        }
        BackupCmd::Replicate {
            backup,
            remote,
            pool,
            wait,
        } => backup_replicate(client, &backup, &remote, &pool, wait, out).await?,
    }
    Ok(())
}

/// `backup create`: 백업 Task를 기다린다. 증분을 청했으면 증분이 됐는지, 아니면 전체로 한 이유를 보인다.
async fn backup_create(
    client: &Client,
    vm: &str,
    pool: &str,
    name: Option<String>,
    incremental: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut body = json!({ "pool": pool, "name": name });
    // 청하지 않으면 보내지 않는다(옛 서버는 모르는 필드를 거절한다).
    if incremental {
        body["incremental"] = json!(true);
    }
    let task = client
        .post(&format!("/vms/{}/backups", seg(vm)), &body)
        .await?;
    client.wait_task_for(&task, BACKUP_WAIT).await?;
    // 이름을 주지 않았거나 증분을 청했으면 만든 백업을 읽는다.
    let backup = if name.is_none() || incremental {
        let id = cell(&task["spec"]["op"]["backup"]);
        client.get(&format!("/backups/{}", seg(&id))).await?
    } else {
        Value::Null
    };
    let name = name.unwrap_or_else(|| cell(&backup["name"]));
    let kind = if !incremental {
        String::new()
    } else if backup["status"]["parent"].is_string() {
        " (incremental)".into()
    } else {
        format!(" (full: {})", cell(&backup["status"]["full_reason"]))
    };
    writeln!(out, "backup {name} of vm {vm} created{kind}")?;
    Ok(())
}

/// 증분 백업의 체인: 전체 백업부터 이 백업까지의 이름과 그 파일 크기의 합. `backups`는 백업 목록이고,
/// 거기 없는 조상은 id로 적고 거기서 멈춘다(이미 본 백업이 다시 나와도 멈춘다). 체인을 끝까지 따라가지
/// 못했거나 다 만들지 않은(`ready`가 아닌) 백업이 있으면 크기는 모르므로(부분 합은 보이지 않는다) 이름만
/// 적는다. 체인을 끝까지 따라갔으면 `replicas`에 체인 백업들의 원격 사본 (원격, 풀)마다 체인 n개 가운데
/// 사본이 ready로 적힌(사본 id가 있는) 백업 k개를 "k of n copied"로 적는다(복제 중이 아니어도 보이는
/// 상태).
fn backup_chain(backup: &Value, backups: &[Value]) -> Value {
    let stored = |b: &Value| -> u64 {
        b["status"]["disks"].as_array().map_or(0, |disks| {
            disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum()
        })
    };
    let ready = |b: &Value| b["status"]["phase"] == "ready";
    let mut names = vec![cell(&backup["name"])];
    let mut members = vec![backup];
    let mut total = stored(backup);
    let mut sized = ready(backup);
    let mut whole = true;
    let mut seen = std::collections::HashSet::from([backup["id"].clone()]);
    let mut parent = backup["status"]["parent"].clone();
    while let Some(id) = parent.as_str() {
        if !seen.insert(parent.clone()) {
            sized = false;
            whole = false;
            break;
        }
        let Some(b) = backups.iter().find(|b| b["id"] == id) else {
            names.push(id.to_string());
            sized = false;
            whole = false;
            break;
        };
        names.push(cell(&b["name"]));
        members.push(b);
        total += stored(b);
        sized &= ready(b);
        parent = b["status"]["parent"].clone();
    }
    names.reverse();
    members.reverse();
    let mut chain = json!({ "backups": names });
    if sized {
        chain["stored_bytes"] = total.into();
    }
    // 체인 백업들의 사본 (원격, 풀)을 전체 백업부터 처음 나온 차례로
    let mut targets: Vec<(&Value, &Value)> = Vec::new();
    for b in &members {
        for r in b["status"]["replicas"].as_array().into_iter().flatten() {
            let target = (&r["remote"], &r["pool"]);
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
    if whole && !targets.is_empty() {
        let sent = |remote: &Value, pool: &Value| {
            members
                .iter()
                .filter(|b| {
                    b["status"]["replicas"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|r| {
                            r["remote"] == *remote
                                && r["pool"] == *pool
                                && r["state"] == "ready"
                                && r["backup"].is_string()
                        })
                })
                .count()
        };
        chain["replicas"] = targets
            .iter()
            .map(|&(remote, pool)| {
                json!({
                    "remote": remote,
                    "pool": pool,
                    "progress": format!("{} of {} copied", sent(remote, pool), members.len()),
                })
            })
            .collect();
    }
    chain
}

/// `backup replicate`: 복제 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn backup_replicate(
    client: &Client,
    backup: &str,
    remote: &str,
    pool: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(
            &format!("/backups/{}/actions/replicate", seg(backup)),
            &json!({ "remote": remote, "pool": pool }),
        )
        .await?;
    if wait {
        client.wait_task_for(&task, BACKUP_WAIT).await?;
        writeln!(
            out,
            "backup {backup} replicated to pool {pool} of remote {remote}"
        )?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "replication of backup {backup} to remote {remote} started (task {id}); follow it with `orbivisorctl task get {id}`"
        )?;
    }
    Ok(())
}

/// 백업을 지운다. `force`로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
async fn backup_delete(
    client: &Client,
    backup: &str,
    force: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut r = client.request(Method::DELETE, &format!("/backups/{}", seg(backup)));
    if force {
        r = r.query(&[("force", "true")]);
    }
    let got = client.send(r).await?;
    if let Some(message) = got["message"].as_str() {
        writeln!(out, "{message}")?;
    } else {
        let task = client.wait_task(&got).await?;
        writeln!(out, "backup {backup} deleted")?;
        // 원격 사본을 지우지 못했으면 성공한 Task에 경고가 남는다.
        if let Some(warning) = task["status"]["error"].as_str() {
            writeln!(out, "warning: {warning}")?;
        }
    }
    Ok(())
}

/// `backup list`의 열
const BACKUP_HEADERS: &[&str] = &[
    "NAME", "VM", "POOL", "PHASE", "CREATED", "SIZE", "TYPE", "ORIGIN", "REPLICAS",
];

/// `backup list`의 한 행. VM은 만든 때의 이름이고(VM을 지웠어도 남는다), SIZE는 백업 파일 크기의 합이다
/// (증분 백업은 증분 파일만). TYPE은 부모가 있으면 `incr`, 아니면 `full`이다. ORIGIN은 원격에서 받은
/// 사본의 `<보낸 클러스터>/<원본 id 앞 8자>`, REPLICAS는 원격 사본마다 `<원격>:<상태>`다.
fn backup_row(b: &Value) -> Vec<String> {
    let status = &b["status"];
    let created = status["created_at"]
        .as_i64()
        .map_or_else(|| "-".to_string(), unix_time);
    let size = match status["disks"].as_array() {
        Some(disks) if status["phase"] == "ready" => bytes(
            &disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum::<u64>()
                .into(),
        ),
        _ => "-".into(),
    };
    let origin = &b["spec"]["origin"];
    let origin = match origin["backup"].as_str() {
        Some(id) => format!("{}/{}", cell(&origin["cluster"]), &id[..id.len().min(8)]),
        None => "-".into(),
    };
    let replicas = match status["replicas"].as_array() {
        Some(r) if !r.is_empty() => r
            .iter()
            .map(|r| format!("{}:{}", cell(&r["remote"]), cell(&r["state"])))
            .collect::<Vec<_>>()
            .join(","),
        _ => "-".into(),
    };
    vec![
        cell(&b["name"]),
        cell(&b["spec"]["vm_name"]),
        cell(&b["spec"]["pool"]),
        cell(&status["phase"]),
        created,
        size,
        if status["parent"].is_string() {
            "incr"
        } else {
            "full"
        }
        .into(),
        origin,
        replicas,
    ]
}

async fn backup_job(
    client: &Client,
    cmd: BackupJobCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupJobCmd::Create(args) => {
            let job = client
                .post("/backup-jobs", &backup_job_create_body(&args))
                .await?;
            if args.disabled {
                writeln!(out, "backup job {} created (disabled)", args.name)?;
            } else {
                let next = job["status"]["next_run"]
                    .as_i64()
                    .map_or_else(|| "-".to_string(), unix_time);
                writeln!(out, "backup job {} created; next run {next}", args.name)?;
            }
        }
        BackupJobCmd::List => {
            let list = client.get("/backup-jobs").await?;
            print_list(out, json_out, &list, BACKUP_JOB_HEADERS, backup_job_row)?;
        }
        BackupJobCmd::Get { job } => print_json(
            out,
            &client.get(&format!("/backup-jobs/{}", seg(&job))).await?,
        )?,
        BackupJobCmd::Set(args) => {
            // If-Match 없이 보낸다: 스케줄러가 status를 자주 써서 revision이 곧잘 바뀐다.
            let path = format!("/backup-jobs/{}", seg(&args.job));
            let current = client.get(&path).await?;
            client
                .patch(&path, &backup_job_set_body(&args, &current))
                .await?;
            writeln!(out, "backup job {} updated", args.job)?;
        }
        BackupJobCmd::Delete { job } => {
            client
                .delete(&format!("/backup-jobs/{}", seg(&job)))
                .await?;
            writeln!(out, "backup job {job} deleted; its backups are kept")?;
        }
        BackupJobCmd::Run { job } => {
            client
                .send(client.request(Method::POST, &format!("/backup-jobs/{}/run", seg(&job))))
                .await?;
            writeln!(
                out,
                "backup job {job} will run at the scheduler's next check"
            )?;
        }
    }
    Ok(())
}

/// `backup-job list`의 열
const BACKUP_JOB_HEADERS: &[&str] = &[
    "NAME", "POOL", "VMS", "SCHEDULE", "KEEP", "ENABLED", "NEXT", "LAST",
];

/// `backup-job list`의 한 행. 시각은 모두 UTC다. 끈 작업은 다음 실행이 없다.
fn backup_job_row(j: &Value) -> Vec<String> {
    let spec = &j["spec"];
    let status = &j["status"];
    let vms = &spec["vms"];
    let vms = if vms["all"] == true {
        "all".to_string()
    } else if let Some(names) = vms["names"].as_array() {
        names.iter().map(cell).collect::<Vec<_>>().join(",")
    } else if let Some(labels) = vms["labels"].as_object() {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "-".to_string()
    };
    let days = match spec["schedule"]["days"].as_array() {
        Some(days) if !days.is_empty() => days.iter().map(cell).collect::<Vec<_>>().join(","),
        _ => "daily".to_string(),
    };
    let mut schedule = format!("{days} {} UTC", cell(&spec["schedule"]["at"]));
    if let Some(n) = spec["full_every"].as_u64().filter(|n| *n > 0) {
        schedule.push_str(&format!(", full every {n}"));
    }
    let keep: Vec<String> = ["last", "daily", "weekly", "monthly"]
        .iter()
        .filter_map(|unit| match spec["keep"][unit].as_u64() {
            Some(n) if n > 0 => Some(format!("{unit}={n}")),
            _ => None,
        })
        .collect();
    let keep = if keep.is_empty() {
        "all".to_string()
    } else {
        keep.join(",")
    };
    let enabled = spec["enabled"] != false;
    let next = match status["next_run"].as_i64() {
        Some(t) if enabled => unix_time(t),
        _ => "-".to_string(),
    };
    let last = if !status["run"].is_null() {
        "running".to_string()
    } else {
        let r = &status["last_run"];
        match r["finished_at"].as_i64() {
            Some(t) => format!(
                "{} ({} ok, {} failed)",
                unix_time(t),
                cell(&r["ok"]),
                cell(&r["failed"])
            ),
            None => "-".to_string(),
        }
    };
    vec![
        cell(&j["name"]),
        cell(&spec["pool"]),
        vms,
        schedule,
        keep,
        enabled.to_string(),
        next,
        last,
    ]
}

async fn import(
    client: &Client,
    cmd: ImportCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ImportCmd::Ova { file, name } => {
            // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
            let size = tokio::fs::metadata(&file)
                .await
                .with_context(|| format!("cannot read {}", file.display()))?
                .len()
                .to_string();
            client
                .send(
                    client
                        .request(Method::POST, "/vm-imports/ova/check")
                        .query(&[("name", name.as_str()), ("size", size.as_str())]),
                )
                .await?;
            let file_name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{name}.ova"));
            let query = [("name", name.as_str()), ("file_name", file_name.as_str())];
            client.upload("/vm-imports/ova", &query, &file).await?;
            writeln!(
                out,
                "vm-import {name} uploaded; review it with `orbivisorctl import get {name}`"
            )?;
        }
        ImportCmd::EsxiBrowse { conn, fingerprint } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, fingerprint.as_deref(), &password);
            esxi_browse(client, &connection, json_out, out).await?;
        }
        ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, Some(&fingerprint), &password);
            let body = esxi_import_body(&name, &connection, &datastore, &vmx);
            esxi_create(client, &body, out).await?;
        }
        ImportCmd::List => {
            let list = client.get("/vm-imports").await?;
            let (nodes, vms) = if json_out {
                (Vec::new(), Vec::new())
            } else {
                (
                    items(&client.get("/nodes").await?),
                    items(&client.get("/vms").await?),
                )
            };
            print_list(out, json_out, &list, IMPORT_HEADERS, |i| {
                import_row(i, &nodes, &vms)
            })?;
        }
        ImportCmd::Get { import } => {
            let import = client.get(&format!("/vm-imports/{}", seg(&import))).await?;
            if json_out {
                print_json(out, &import)?;
            } else {
                let nodes = items(&client.get("/nodes").await?);
                let vms = if import["status"]["vm"].is_null() {
                    Vec::new()
                } else {
                    items(&client.get("/vms").await?)
                };
                print_import(out, &import, &nodes, &vms)?;
            }
        }
        ImportCmd::Run(args) => {
            // `=` 없는 --net이 있을 때만 원본 네트워크를 읽는다.
            let sources = if args.nets.iter().any(|n| n.source.is_none()) {
                source_networks(
                    &client
                        .get(&format!("/vm-imports/{}", seg(&args.import)))
                        .await?,
                )
            } else {
                Vec::new()
            };
            let body = import_run_body(&args, &sources)?;
            let task = client
                .post(&format!("/vm-imports/{}/run", seg(&args.import)), &body)
                .await?;
            client.wait_task_for(&task, IMPORT_WAIT).await?;
            writeln!(out, "vm {} imported", args.name)?;
        }
        ImportCmd::Delete { import, force } => {
            let mut r = client.request(Method::DELETE, &format!("/vm-imports/{}", seg(&import)));
            if force {
                r = r.query(&[("force", "true")]);
            }
            let got = client.send(r).await?;
            // force로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
            if let Some(message) = got["message"].as_str() {
                writeln!(out, "{message}")?;
            } else {
                client.wait_task(&got).await?;
                writeln!(out, "vm-import {import} deleted")?;
            }
        }
    }
    Ok(())
}

/// ESXi 연결 오류. 믿을 지문이 없으면 서버 인증서의 지문을 확인하고 다시 하라고 알린다.
fn esxi_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.code == "untrusted_certificate" => {
            let fingerprint = cell(&api.details["fingerprint"]);
            anyhow::anyhow!(
                "the ESXi certificate fingerprint is {fingerprint}; check it and rerun with --fingerprint {fingerprint}"
            )
        }
        _ => e,
    }
}

/// `import esxi-browse`: 데이터스토어마다 VM 표
async fn esxi_browse(
    client: &Client,
    connection: &Value,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let got = client
        .post("/vm-imports/esxi/browse", connection)
        .await
        .map_err(esxi_error)?;
    if json_out {
        return Ok(print_json(out, &got)?);
    }
    let mut rows = Vec::new();
    for ds in got["datastores"].as_array().into_iter().flatten() {
        for vm in ds["vms"].as_array().into_iter().flatten() {
            rows.push(esxi_vm_row(&ds["name"], vm));
        }
    }
    table(out, ESXI_VM_HEADERS, &rows)?;
    Ok(())
}

/// `import esxi-browse`의 열
const ESXI_VM_HEADERS: &[&str] = &[
    "DATASTORE",
    "VMX",
    "NAME",
    "GUEST",
    "CPU",
    "MEMORY",
    "DISKS",
    "LOCKED",
];

/// `import esxi-browse`의 한 행. `.vmx`를 읽지 못한 VM은 NAME 자리에 오류를 보인다.
fn esxi_vm_row(datastore: &Value, vm: &Value) -> Vec<String> {
    let (name, guest, cpu, memory, disks) = match vm["error"].as_str() {
        Some(error) => (
            format!("error: {error}"),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ),
        None => (
            cell(&vm["name"]),
            cell(&vm["guest_os"]),
            cell(&vm["vcpus"]),
            bytes(&vm["memory_mib"].as_u64().map(|m| m << 20).into()),
            cell(&vm["disks"]),
        ),
    };
    vec![
        cell(datastore),
        cell(&vm["vmx"]),
        name,
        guest,
        cpu,
        memory,
        disks,
        cell(&vm["locked"]),
    ]
}

/// `import esxi`: 가져오기를 만든다.
async fn esxi_create(
    client: &Client,
    body: &Value,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    client
        .post("/vm-imports/esxi", body)
        .await
        .map_err(esxi_error)?;
    writeln!(
        out,
        "vm-import {name} created; review it with `orbivisorctl import get {name}`",
        name = cell(&body["name"])
    )?;
    Ok(())
}

/// ESXi 원본의 표시(`esxi://호스트/데이터스토어/.vmx`). OVA 원본이면 `None`이다.
fn esxi_source(i: &Value) -> Option<String> {
    let esxi = &i["spec"]["source"]["esxi"];
    esxi.is_object().then(|| {
        format!(
            "esxi://{}/{}/{}",
            cell(&esxi["host"]),
            cell(&esxi["datastore"]),
            cell(&esxi["vmx"])
        )
    })
}

/// 목록에서 id가 `id`인 항목의 이름. 없으면 id다.
fn name_of(list: &[Value], id: &Value) -> String {
    list.iter()
        .find(|v| v["id"] == *id)
        .map_or_else(|| cell(id), |v| cell(&v["name"]))
}

/// `import list`의 열
const IMPORT_HEADERS: &[&str] = &["NAME", "NODE", "SOURCE", "PHASE", "VM"];

/// `import list`의 한 행. SOURCE는 원본 VM 이름이고, 원본을 읽지 못했으면 올린 파일 이름이다.
/// ESXi 원본이면 `esxi://호스트/데이터스토어/.vmx`다.
fn import_row(i: &Value, nodes: &[Value], vms: &[Value]) -> Vec<String> {
    let status = &i["status"];
    let source = match (esxi_source(i), status["source_vm"]["name"].as_str()) {
        (Some(esxi), _) => esxi,
        (None, Some(name)) => name.to_string(),
        (None, None) => cell(&i["spec"]["source"]["ova"]["file_name"]),
    };
    vec![
        cell(&i["name"]),
        name_of(nodes, &i["spec"]["node"]),
        source,
        cell(&status["phase"]),
        name_of(vms, &status["vm"]),
    ]
}

/// `import get`: 머리 줄들, 원본 디스크와 NIC 표, 경고
fn print_import(
    out: &mut (dyn Write + Send),
    i: &Value,
    nodes: &[Value],
    vms: &[Value],
) -> anyhow::Result<()> {
    let ova = &i["spec"]["source"]["ova"];
    let status = &i["status"];
    writeln!(out, "vm-import: {}", cell(&i["name"]))?;
    writeln!(out, "node: {}", name_of(nodes, &i["spec"]["node"]))?;
    match esxi_source(i) {
        Some(esxi) => writeln!(out, "source: {esxi}")?,
        None => writeln!(
            out,
            "source: {} ({})",
            cell(&ova["file_name"]),
            bytes(&ova["size_bytes"])
        )?,
    }
    writeln!(out, "phase: {}", cell(&status["phase"]))?;
    if !status["vm"].is_null() {
        writeln!(out, "vm: {}", name_of(vms, &status["vm"]))?;
    }
    if let Some(error) = status["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let src = &status["source_vm"];
    if src.is_object() {
        writeln!(
            out,
            "source vm: {} ({}, {}, {} vCPUs, {} cores per socket, {} MiB)",
            cell(&src["name"]),
            cell(&src["guest_os"]),
            cell(&src["firmware"]),
            cell(&src["vcpus"]),
            cell(&src["cores_per_socket"]),
            cell(&src["memory_mib"])
        )?;
    }
    let p = &status["proposal"];
    if p.is_object() {
        writeln!(
            out,
            "proposal: {} sockets x {} cores, {} MiB, {}, {} disks, {} NICs",
            cell(&p["sockets"]),
            cell(&p["cores"]),
            cell(&p["memory_mib"]),
            cell(&p["firmware"]),
            cell(&p["disk_bus"]),
            cell(&p["nic_model"])
        )?;
    }
    if src.is_object() {
        let disks: Vec<_> = src["disks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                vec![
                    cell(&d["name"]),
                    cell(&d["file"]),
                    bytes(&d["capacity_bytes"]),
                    cell(&d["controller"]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["DISK", "FILE", "SIZE", "CONTROLLER"], &disks)?;
        // PROPOSED는 이름이 같아 제안한 Orbivisor 네트워크다.
        let nics: Vec<_> = src["nics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                vec![
                    cell(&n["network"]),
                    cell(&n["adapter"]),
                    cell(&n["mac"]),
                    cell(&p["networks"][n["network"].as_str().unwrap_or_default()]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["NETWORK", "ADAPTER", "MAC", "PROPOSED"], &nics)?;
    }
    if let Some(warnings) = status["warnings"].as_array().filter(|w| !w.is_empty()) {
        writeln!(out, "\nwarnings:")?;
        for w in warnings {
            writeln!(out, "  {}", cell(w))?;
        }
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  orbivisorctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            print_cluster_status(out, &view)?;
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{}", seg(&node))).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

/// `vm migrate`: 마이그레이션 Task를 만들고 끝나기를 기다린다. `pool`은 줄 때만 보낸다(옛 서버는 모르는
/// 필드를 거절한다). 기다리는 동안 디스크 복사 진행률과 메모리 단계를 `err`(stderr)에 알린다.
async fn vm_migrate(
    client: &Client,
    vm: &str,
    node: &str,
    pool: Option<String>,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut body = json!({ "node": node });
    if let Some(pool) = pool {
        body["pool"] = json!(pool);
    }
    let task = client
        .post(&format!("/vms/{}/migrate", seg(vm)), &body)
        .await?;
    // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
    let mut shown = None;
    client
        .wait_task_watch(&task, MIGRATE_WAIT, |t| {
            // 진행 줄을 쓰지 못해도(stderr가 닫힘) 마이그레이션은 계속 기다린다.
            if let Some(line) = migrate_progress(vm, &t["status"]["progress"], &mut shown) {
                let _ = writeln!(err, "{line}");
            }
            Ok(())
        })
        .await?;
    writeln!(out, "vm {vm}: migrated to {node}")?;
    Ok(())
}

/// 마지막으로 알린 마이그레이션 단계
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrateShown {
    /// 디스크 복사(퍼센트)
    Copying(u64),
    Memory,
}

/// 마이그레이션 Task의 `progress`에서 알릴 줄. 디스크 복사는 처음과 지난번보다 5퍼센트 포인트 넘게
/// 바뀌었을 때, 메모리 단계는 들어갈 때 한 번이다.
fn migrate_progress(
    vm: &str,
    progress: &Value,
    shown: &mut Option<MigrateShown>,
) -> Option<String> {
    match progress["stage"].as_str()? {
        "copying-disks" => {
            let done = progress["done_bytes"].as_u64().unwrap_or(0);
            let total = progress["total_bytes"].as_u64().unwrap_or(0);
            let percent = if total == 0 {
                0
            } else {
                (u128::from(done.min(total)) * 100 / u128::from(total)) as u64
            };
            let show = match *shown {
                None => true,
                Some(MigrateShown::Copying(last)) => percent.abs_diff(last) > 5,
                Some(MigrateShown::Memory) => false,
            };
            if !show {
                return None;
            }
            *shown = Some(MigrateShown::Copying(percent));
            Some(format!(
                "vm {vm}: copying disks {percent}% ({} of {})",
                gib(done),
                gib(total)
            ))
        }
        "migrating-memory" if *shown != Some(MigrateShown::Memory) => {
            *shown = Some(MigrateShown::Memory);
            Some(format!("vm {vm}: migrating memory"))
        }
        _ => None,
    }
}

/// 바이트를 소수 한 자리 GiB로(`14.8 GiB`)
fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{}", seg(&vm))).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{}", seg(key))).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args.nics.iter().map(nic_body).collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::NicSg { vm, index, groups } => {
            put_spec(client, &vm, |current| {
                nic_groups_update(current, index, &groups)
            })
            .await?;
            if groups.is_empty() {
                writeln!(out, "vm {vm}: NIC {index} has no security groups")?;
            } else {
                writeln!(
                    out,
                    "vm {vm}: NIC {index} security groups {}",
                    groups.join(", ")
                )?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Migrate { vm, node, pool } => {
            vm_migrate(client, &vm, &node, pool, out, &mut std::io::stderr()).await?
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                    let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{}/snapshots/{}", seg(&vm), seg(&cell(&s["id"])));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{}", seg(&vm))).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client
                .post(&format!("/vms/{}/clone", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Stats { vm, stats } => {
            vm_stats(
                client,
                &vm,
                stats.watch,
                json_out,
                out,
                &mut std::io::stderr(),
            )
            .await?
        }
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{}", seg(&vm))).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 열
const VM_HEADERS: &[&str] = &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"];

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client
                .post(&format!("/vms/{}/snapshots", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current =
                client.get(&format!("/vms/{}", seg(&vm))).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{}/snapshots/{}/revert", seg(&vm), seg(&name));
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{}/snapshots/{}", seg(&vm), seg(&name)))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", seg(&args.vm));
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    put_spec(client, vm, |current| {
        Ok(spec_update(current, field, value.clone()))
    })
    .await
}

/// VM `vm`을 읽어 `body`(읽은 VM → 수정 본문)로 만든 본문을 그 revision으로(`If-Match`) 다시 쓴다.
async fn put_spec(
    client: &Client,
    vm: &str,
    body: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", seg(vm));
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body(&current)?);
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// VM 수정 본문: NIC `index`(0부터)의 보안 그룹만 `groups`로 바꾼다.
fn nic_groups_update(vm: &Value, index: usize, groups: &[String]) -> anyhow::Result<Value> {
    let mut nics = vm["spec"]["nics"].as_array().cloned().unwrap_or_default();
    let count = nics.len();
    let Some(nic) = nics.get_mut(index) else {
        bail!(
            "VM {} has no NIC {index} ({count} NICs, numbered from 0)",
            cell(&vm["name"])
        );
    };
    nic["security_groups"] = json!(groups);
    Ok(spec_update(vm, "nics", Value::Array(nics)))
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(
            Method::POST,
            // 동작은 이 파일이 정한 값이고 `stop?mode=force`처럼 쿼리를 담으므로 인코딩하지 않는다.
            &format!("/vms/{}/actions/{action}", seg(vm)),
        ))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "VERSION",
    "UNDERLAY",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        // 옛 버전 노드는 기록하지 않는다.
        cell(&n["status"]["version"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
        cell(&n["status"]["underlay"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `network list`의 열
const NETWORK_HEADERS: &[&str] = &["NAME", "TYPE", "BRIDGE", "VLAN/VNI", "MTU"];

/// VXLAN 네트워크의 기본 MTU(서버의 `DEFAULT_VXLAN_MTU`)
const DEFAULT_VXLAN_MTU: u64 = 1450;

/// `network list`의 한 행. VXLAN 네트워크의 브리지는 opld가 만드는 `oplv<vni>`다.
fn network_row(n: &Value) -> Vec<String> {
    let spec = &n["spec"];
    match spec["vxlan"]["vni"].as_u64() {
        Some(vni) => vec![
            cell(&n["name"]),
            "vxlan".into(),
            format!("oplv{vni}"),
            vni.to_string(),
            spec["mtu"]
                .as_u64()
                .unwrap_or(DEFAULT_VXLAN_MTU)
                .to_string(),
        ],
        None => vec![
            cell(&n["name"]),
            "bridge".into(),
            cell(&spec["bridge"]),
            cell(&spec["vlan"]),
            "-".into(),
        ],
    }
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|m| {
            vec![
                cell(&m["name"]),
                cell(&m["id"]),
                cell(&m["address"]),
                if m["voter"] == true {
                    "voter"
                } else {
                    "learner"
                }
                .into(),
                cell(&m["online"]),
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
    // 버전을 모르는 멤버(옛 버전 노드, 합류 중)는 세지 않는다.
    let mut versions: Vec<&str> = members
        .iter()
        .filter_map(|m| m["version"].as_str())
        .collect();
    versions.sort_by(|a, b| opl_types::version::compare(a, b));
    versions.dedup_by(|a, b| opl_types::version::compare(a, b).is_eq());
    if versions.len() > 1 {
        writeln!(
            out,
            "warning: nodes run different Orbivisor versions ({}); finish the upgrade on every node",
            versions.join(", ")
        )?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    // 백업 풀만 적는다(옛 서버와 끈 풀은 보내지 않는다).
    if pool["spec"]["backups"] == true {
        text.push_str("backups: true\n");
    }
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `orbivisorctl cluster init` \
             or `orbivisorctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 이 CLI의 login이 만든 토큰(이름 `oplctl-…`)이면 지운다. 토큰 자신으로 읽고 지운다(실패는 무시).
/// 지웠으면 `Some(true)`, 다른 토큰(웹 UI에서 만든 것)이라 두었으면 `Some(false)`, 목록을 읽지 못했거나
/// 목록에 없으면(이미 지워짐) `None`.
async fn remove_login_token(client: &Client, id: uuid::Uuid) -> Option<bool> {
    let tokens = client.get("/tokens").await.ok()?;
    let token = tokens["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["id"].as_str() == Some(id.to_string().as_str()))?;
    let made_by_login = token["name"]
        .as_str()
        .is_some_and(|n| n.starts_with(LOGIN_TOKEN_PREFIX));
    if made_by_login {
        let _ = client.delete(&format!("/tokens/{id}")).await;
    }
    Some(made_by_login)
}

/// 웹 UI에서 만든 API 토큰으로 로그인한다: 그 토큰으로 `GET /auth/whoami`를 불러 확인하고 사용자 이름을
/// 얻는다. 토큰과 사용자 이름을 돌려준다.
async fn token_login(
    server: &str,
    file: &PathBuf,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    let client = Client::new(server, Some(token.clone()), ca_pem)?;
    let me = client
        .get("/auth/whoami")
        .await
        .context("the API token was not accepted")?;
    let name = me["name"]
        .as_str()
        .context("server did not return the user")?
        .to_string();
    Ok((token, name))
}

/// OIDC 영역은 브라우저로만 로그인하므로 비밀번호를 묻기 전에 거절한다. 로그인 화면의
/// 영역 목록(`GET /auth/realms`)을 읽지 못하면 그냥 지나간다(서버가 로그인에서 판단한다).
async fn oidc_login_refused(anon: &Client, server: &str, realm: &str) -> anyhow::Result<()> {
    let resp = anon
        .http
        .get(anon.url("/auth/realms"))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    let list: Value = if resp.status().is_success() {
        resp.json().await.unwrap_or_default()
    } else {
        Value::Null
    };
    let oidc = list
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| r["name"] == realm && r["kind"] == "oidc");
    if oidc {
        bail!(
            "sign in to {realm} in the web UI, create an API token there and run `orbivisorctl login --token-file`"
        );
    }
    Ok(())
}

/// `login`이 만드는 API 토큰 이름의 앞부분. 비교되는 문자열 — 바꾸지 않는다(`logout`이 옛 CLI가 만든 토큰도 알아보고 지운다).
const LOGIN_TOKEN_PREFIX: &str = "oplctl-";

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다. 토큰과 서버가 정한
/// 사용자 이름(영역 사용자는 `<사용자>@<영역>`)을 돌려준다.
async fn login(
    server: &str,
    args: &LoginArgs,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let anon = Client::new(server, None, ca_pem)?;
    if args.realm != opl_types::realm::LOCAL_REALM {
        oidc_login_refused(&anon, server, &args.realm).await?;
    }
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let mut body = json!({ "username": args.user, "password": password });
    if args.realm != opl_types::realm::LOCAL_REALM {
        body["realm"] = json!(args.realm);
    }
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let user: Value = resp.json().await.unwrap_or_default();
    let user = user["name"].as_str().unwrap_or(&args.user).to_string();
    let name = format!(
        "{LOGIN_TOKEN_PREFIX}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    let token = created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")?;
    Ok((token, user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node, pool: None } } if vm == "web" && node == "n2"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2", "--pool", "fast"])
            .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { pool: Some(pool), .. } } if pool == "fast"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 디스크 복사 진행률은 처음과 5퍼센트 포인트 넘게 바뀔 때, 메모리 단계는 한 번 알린다.
    #[test]
    fn migrate_progress_lines() {
        let progress = |stage: &str, done: u64| json!({ "stage": stage, "done_bytes": done, "total_bytes": 40u64 << 30 });
        let copying = |done: u64| progress("copying-disks", done);
        let mut shown = None;
        let mut lines = Vec::new();
        for p in [
            json!(null),
            copying(0),
            copying(1 << 30),
            copying(14_800 << 20),
            copying(15 << 30),
            copying(40 << 30),
            progress("migrating-memory", 40 << 30),
            json!({ "stage": "migrating-memory" }),
            copying(40 << 30),
        ] {
            lines.extend(migrate_progress("web", &p, &mut shown));
        }
        assert_eq!(
            lines,
            [
                "vm web: copying disks 0% (0.0 GiB of 40.0 GiB)",
                "vm web: copying disks 36% (14.5 GiB of 40.0 GiB)",
                "vm web: copying disks 100% (40.0 GiB of 40.0 GiB)",
                "vm web: migrating memory",
            ]
        );
    }

    /// `vm migrate`는 `--pool`을 줄 때만 본문에 넣고, 기다리는 동안 진행률을 stderr에 알린다.
    #[tokio::test]
    async fn vm_migrate_sends_the_pool_and_shows_progress() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static POLLS: AtomicUsize = AtomicUsize::new(0);
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/tasks/t1" => {
                let (stage, done) = match POLLS.fetch_add(1, Ordering::SeqCst) {
                    0 => ("copying-disks", 0),
                    1 => ("copying-disks", 1u64 << 30),
                    2 => ("migrating-memory", 2 << 30),
                    _ => return json!({ "id": "t1", "status": { "state": "succeeded" } }),
                };
                let progress =
                    json!({ "stage": stage, "done_bytes": done, "total_bytes": 2u64 << 30 });
                json!({ "id": "t1", "status": { "state": "running", "progress": progress } })
            }
            _ => json!({ "id": "t1", "status": { "state": "pending" } }),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        vm_migrate(
            &client,
            "web",
            "n2",
            Some("fast".into()),
            &mut out,
            &mut err,
        )
        .await
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "vm web: migrated to n2\n");
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "vm web: copying disks 0% (0.0 GiB of 2.0 GiB)\n\
             vm web: copying disks 50% (1.0 GiB of 2.0 GiB)\n\
             vm web: migrating memory\n"
        );
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen[0].line, "POST /api/v1/vms/web/migrate HTTP/1.1");
            assert_eq!(seen[0].body, json!({ "node": "n2", "pool": "fast" }));
        }

        // 풀을 주지 않으면 본문에 `pool`이 없다.
        seen.lock().unwrap().clear();
        let mut out = Vec::new();
        vm_migrate(&client, "web", "n2", None, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap()[0].body, json!({ "node": "n2" }));
    }

    /// 진행 줄을 쓰지 못해도(stderr가 닫힘) 마이그레이션이 끝나기를 기다린다.
    #[tokio::test]
    async fn vm_migrate_ignores_a_broken_stderr() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static POLLS: AtomicUsize = AtomicUsize::new(0);
        let (base, _) = fake_api_bodies(|path| match path {
            "/api/v1/tasks/t1" => match POLLS.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    let progress =
                        json!({ "stage": "migrating-memory", "done_bytes": 0, "total_bytes": 0 });
                    json!({ "id": "t1", "status": { "state": "running", "progress": progress } })
                }
                _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
            },
            _ => json!({ "id": "t1", "status": { "state": "pending" } }),
        })
        .await;
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        vm_migrate(&client, "web", "n2", None, &mut out, &mut Broken)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "vm web: migrated to n2\n");
        assert!(POLLS.load(Ordering::SeqCst) >= 2);
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool.clone(), pool_set_body(&s), pool_set_summary(&s)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }), "shared".into())
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            (
                "nas".into(),
                json!({ "shared": false }),
                "node-local".into()
            )
        );
        // 백업 표시는 따로 바꾸거나 공유 여부와 함께 바꾼다. 주지 않은 값은 보내지 않는다.
        assert_eq!(
            set(&["--backups", "true"]).unwrap(),
            (
                "nas".into(),
                json!({ "backups": true }),
                "backups on".into()
            )
        );
        assert_eq!(
            set(&["--no-shared", "--backups", "false"]).unwrap().1,
            json!({ "shared": false, "backups": false })
        );
        assert_eq!(
            set(&["--shared", "--backups", "false"]).unwrap().2,
            "shared, backups off"
        );
        // 하나는 꼭, 공유와 노드 로컬은 함께 못 준다, 백업은 true나 false
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
        assert!(set(&["--backups"]).is_err());
        assert!(set(&["--backups", "yes"]).is_err());
    }

    #[test]
    fn pool_create_marks_backup_pools() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "bk"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            body(&["--path", "/srv/bk", "--backups"]).unwrap()["spec"],
            json!({ "backend": { "kind": "dir", "path": "/srv/bk" }, "nodes": [], "backups": true })
        );
        assert_eq!(
            body(&["--nfs", "nas:/bk", "--backups"]).unwrap()["spec"]["backups"],
            json!(true)
        );
        // 끈 풀은 보내지 않는다.
        assert!(body(&["--path", "/srv/bk"]).unwrap()["spec"]["backups"].is_null());
        // 블록 풀은 백업을 담지 못한다.
        for args in [
            &["--lvm-thin", "vg0/tp", "--backups"][..],
            &["--zfs", "tank/opl", "--backups"],
            &["--rbd", "rbd", "--backups"],
            &["--lvm-shared", "san", "--backups"],
        ] {
            assert!(body(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn describes_a_backup_pool() {
        let pool = json!({
            "name": "bk",
            "spec": { "backend": { "kind": "dir", "path": "/srv/bk" }, "backups": true }
        });
        assert_eq!(
            describe_pool(&pool),
            "pool: bk\nkind: dir\nlocation: /srv/bk\nshared: -\nbackups: true\n"
        );
    }

    #[test]
    fn parses_backup_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Backup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk"]).unwrap(),
            BackupCmd::Create { vm, pool, name: None, incremental: false } if vm == "web" && pool == "bk"
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--incremental"]).unwrap(),
            BackupCmd::Create {
                incremental: true,
                ..
            }
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--name", "before-upgrade"]).unwrap(),
            BackupCmd::Create { name: Some(n), .. } if n == "before-upgrade"
        ));
        assert!(matches!(
            parse(&["list", "--vm", "web"]).unwrap(),
            BackupCmd::List { vm: Some(v) } if v == "web"
        ));
        assert!(matches!(
            parse(&["restore", "web-1", "--name", "web2", "--pool", "local", "--node", "n1"])
                .unwrap(),
            BackupCmd::Restore { backup, name, pool, node: Some(node) }
                if backup == "web-1" && name == "web2" && pool == "local" && node == "n1"
        ));
        assert!(matches!(
            parse(&["replicate", "web-1", "--remote", "dr", "--pool", "bk"]).unwrap(),
            BackupCmd::Replicate { backup, remote, pool, wait: false }
                if backup == "web-1" && remote == "dr" && pool == "bk"
        ));
        assert!(matches!(
            parse(&[
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk",
                "--wait"
            ])
            .unwrap(),
            BackupCmd::Replicate { wait: true, .. }
        ));
        // 백업 풀과 복원의 새 이름·풀, 복제할 원격·풀은 꼭 준다.
        for args in [
            &["create", "web"][..],
            &["restore", "web-1", "--pool", "local"],
            &["restore", "web-1", "--name", "web2"],
            &["replicate", "web-1", "--pool", "bk"],
            &["replicate", "web-1", "--remote", "dr"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_list_shows_the_vm_name_and_stored_size() {
        let ready = json!({
            "name": "web-20261104-030000",
            "spec": { "vm": "x", "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "disks": [
                    { "name": "disk0", "stored_bytes": 3u64 << 30 },
                    { "name": "disk1", "stored_bytes": 1u64 << 29 }
                ]
            }
        });
        assert_eq!(
            backup_row(&ready),
            [
                "web-20261104-030000",
                "web",
                "bk",
                "ready",
                "2026-11-04 03:00 UTC",
                "3.5GiB",
                "full",
                "-",
                "-"
            ]
        );
        // 만드는 중이거나 실패한 백업은 크기가 없다.
        let pending = json!({
            "name": "b", "spec": { "vm_name": "web", "pool": "bk" },
            "status": { "phase": "pending", "created_at": null, "disks": [] }
        });
        assert_eq!(backup_row(&pending)[3..6], ["pending", "-", "-"]);
        // 부모가 있으면 증분이고, 크기는 증분 파일만이다.
        let incr = json!({
            "name": "web-2", "spec": { "vm_name": "web", "pool": "bk", "incremental": true },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "parent": "019a4f2c-1111-7000-8000-000000000000",
                "disks": [{ "name": "disk0", "stored_bytes": 1u64 << 20 }]
            }
        });
        assert_eq!(backup_row(&incr)[5..7], ["1.0MiB", "incr"]);
        // 증분을 청했어도 전체로 했으면 전체다.
        let full = json!({
            "name": "web-1", "spec": { "vm_name": "web", "pool": "bk", "incremental": true },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "full_reason": "VM is stopped", "disks": []
            }
        });
        assert_eq!(backup_row(&full)[6], "full");
    }

    #[test]
    fn backup_chain_names_the_backups_from_the_full_one() {
        let backup = |id: &str, name: &str, parent: Option<&str>, bytes: u64| {
            json!({
                "id": id, "name": name,
                "status": { "phase": "ready", "parent": parent, "disks": [
                    { "name": "disk0", "stored_bytes": bytes },
                    { "name": "disk1", "stored_bytes": 1 }
                ] }
            })
        };
        let all = [
            backup("a", "full", None, 1000),
            backup("b", "incr-1", Some("a"), 100),
            backup("c", "incr-2", Some("b"), 10),
            backup("x", "other", None, 5),
        ];
        assert_eq!(
            backup_chain(&all[2], &all),
            json!({ "backups": ["full", "incr-1", "incr-2"], "stored_bytes": 1113 })
        );
        // 원격 사본: (원격, 풀)마다 체인에서 ready이고 사본 id가 있는 백업의 수
        let copy = |pool: &str, state: &str, backup: Option<&str>| json!({ "remote": "dr", "pool": pool, "state": state, "backup": backup, "at": 1 });
        let mut sent = all.clone();
        sent[0]["status"]["replicas"] = json!([
            copy("bk", "ready", Some("ra")),
            copy("other", "ready", Some("oa"))
        ]);
        sent[1]["status"]["replicas"] = json!([copy("bk", "ready", Some("rb"))]);
        sent[2]["status"]["replicas"] = json!([
            copy("bk", "copying", Some("rc")),
            copy("other", "ready", None)
        ]);
        assert_eq!(
            backup_chain(&sent[2], &sent),
            json!({
                "backups": ["full", "incr-1", "incr-2"], "stored_bytes": 1113,
                "replicas": [
                    { "remote": "dr", "pool": "bk", "progress": "2 of 3 copied" },
                    { "remote": "dr", "pool": "other", "progress": "1 of 3 copied" }
                ]
            })
        );
        // 기록이 이 백업의 실패 기록 하나뿐이어도(진행 중이 아니어도) 상태로 보인다.
        let mut failed = all.clone();
        failed[2]["status"]["replicas"] = json!([copy("bk", "failed", Some("rc"))]);
        assert_eq!(
            backup_chain(&failed[2], &failed),
            json!({
                "backups": ["full", "incr-1", "incr-2"], "stored_bytes": 1113,
                "replicas": [{ "remote": "dr", "pool": "bk", "progress": "0 of 3 copied" }]
            })
        );
        // 체인을 끝까지 따라가지 못하면 진행은 모른다.
        assert_eq!(
            backup_chain(&sent[2], &sent[1..]),
            json!({ "backups": ["a", "incr-1", "incr-2"] })
        );
        // 목록에 없는 조상은 id로 적고 멈춘다. 체인 크기는 모른다.
        assert_eq!(
            backup_chain(&all[2], &all[1..]),
            json!({ "backups": ["a", "incr-1", "incr-2"] })
        );
        // 다 만들지 않은 백업이 있으면 크기는 적지 않는다.
        let mut pending = all.clone();
        pending[0]["status"]["phase"] = json!("failed");
        assert_eq!(
            backup_chain(&pending[2], &pending),
            json!({ "backups": ["full", "incr-1", "incr-2"] })
        );
        // 잘못된 목록에서 고리를 이루어도 멈춘다. 전체 백업에 닿지 못했으므로 크기는 모른다.
        let mut ring = all.clone();
        ring[0]["status"]["parent"] = json!("c");
        assert_eq!(
            backup_chain(&ring[2], &ring),
            json!({ "backups": ["full", "incr-1", "incr-2"] })
        );
        // 고리면 사본 기록이 있어도 진행은 모른다.
        let mut ring = sent.clone();
        ring[0]["status"]["parent"] = json!("c");
        assert_eq!(
            backup_chain(&ring[2], &ring),
            json!({ "backups": ["full", "incr-1", "incr-2"] })
        );
    }

    /// `backup get`은 증분 백업에만 백업 목록으로 만든 `chain`을 붙인다.
    #[tokio::test]
    async fn backup_get_adds_the_chain_of_incremental_backups() {
        fn reply(path: &str) -> Value {
            let full = json!({ "id": "a", "name": "full", "status": { "phase": "ready",
                "disks": [{ "name": "disk0", "stored_bytes": 1000 }] } });
            let incr = json!({ "id": "b", "name": "incr", "status": { "phase": "ready",
                "parent": "a", "disks": [{ "name": "disk0", "stored_bytes": 10 }] } });
            match path {
                "/api/v1/backups/incr" => incr,
                "/api/v1/backups/full" => full,
                "/api/v1/backups" => json!({ "items": [full, incr] }),
                _ => json!({}),
            }
        }
        let get = |name: &'static str| async move {
            let (client, seen) = fake_api(reply).await;
            let mut out = Vec::new();
            backup(
                &client,
                BackupCmd::Get {
                    backup: name.into(),
                },
                false,
                &mut out,
            )
            .await
            .unwrap();
            let paths = seen.lock().unwrap().clone();
            (serde_json::from_slice::<Value>(&out).unwrap(), paths)
        };
        let (incr, paths) = get("incr").await;
        assert_eq!(
            incr["chain"],
            json!({ "backups": ["full", "incr"], "stored_bytes": 1010 })
        );
        assert_eq!(
            paths,
            [
                "GET /api/v1/backups/incr HTTP/1.1",
                "GET /api/v1/backups HTTP/1.1"
            ]
        );
        // 전체 백업에는 체인이 없고 목록도 읽지 않는다.
        let (full, paths) = get("full").await;
        assert!(full.get("chain").is_none(), "{full}");
        assert_eq!(paths, ["GET /api/v1/backups/full HTTP/1.1"]);
    }

    /// `backup create --incremental`은 `incremental`을 보내고 증분이 됐는지, 아니면 전체로 한 이유를
    /// 보인다. 청하지 않으면 보내지 않는다.
    #[tokio::test]
    async fn backup_create_reports_incremental_or_why_full() {
        let run = |reply: fn(&str) -> Value, name: Option<&'static str>, incremental: bool| async move {
            let (base, seen) = fake_api_bodies(reply).await;
            let client = Client::new(&base, None, None).unwrap();
            let mut out = Vec::new();
            backup_create(
                &client,
                "web",
                "bk",
                name.map(str::to_string),
                incremental,
                &mut out,
            )
            .await
            .unwrap();
            let seen = seen.lock().unwrap();
            let lines: Vec<String> = seen.iter().map(|r| r.line.clone()).collect();
            (String::from_utf8(out).unwrap(), lines, seen[0].body.clone())
        };
        fn task(path: &str) -> Option<Value> {
            match path {
                "/api/v1/vms/web/backups" => {
                    Some(json!({ "id": "t1", "spec": { "op": { "backup": "b1" } } }))
                }
                "/api/v1/tasks/t1" => {
                    Some(json!({ "id": "t1", "status": { "state": "succeeded" } }))
                }
                _ => None,
            }
        }

        let (out, lines, body) = run(
            |path| {
                task(path).unwrap_or_else(
                    || json!({ "id": "b1", "name": "web-1", "status": { "parent": "b0" } }),
                )
            },
            None,
            true,
        )
        .await;
        assert_eq!(out, "backup web-1 of vm web created (incremental)\n");
        assert_eq!(
            body,
            json!({ "pool": "bk", "name": null, "incremental": true })
        );
        assert_eq!(
            lines,
            [
                "POST /api/v1/vms/web/backups HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "GET /api/v1/backups/b1 HTTP/1.1"
            ]
        );

        let (out, _, _) = run(
            |path| {
                task(path).unwrap_or_else(|| {
                    json!({ "id": "b1", "name": "nightly", "status": { "full_reason": "VM is stopped" } })
                })
            },
            Some("nightly"),
            true,
        )
        .await;
        assert_eq!(
            out,
            "backup nightly of vm web created (full: VM is stopped)\n"
        );

        // 청하지 않고 이름을 주면 백업을 다시 읽지 않는다.
        let (out, lines, body) = run(|path| task(path).unwrap(), Some("nightly"), false).await;
        assert_eq!(out, "backup nightly of vm web created\n");
        assert_eq!(body, json!({ "pool": "bk", "name": "nightly" }));
        assert_eq!(lines.len(), 2, "{lines:?}");
    }

    #[test]
    fn backup_list_shows_the_origin_and_replicas() {
        // 원격에서 받은 사본은 보낸 클러스터와 원본 id 앞 8자
        let copy = json!({
            "name": "web-20261104-030000",
            "spec": {
                "vm_name": "web", "pool": "bk",
                "origin": {
                    "cluster": "prod", "ca_fingerprint": "sha256:aa",
                    "backup": "019a4f2c-1111-7000-8000-000000000000"
                }
            },
            "status": { "phase": "receiving", "created_at": 1_793_761_200, "disks": [] }
        });
        assert_eq!(backup_row(&copy)[7..], ["prod/019a4f2c", "-"]);
        // 원격 사본마다 원격:상태
        let original = json!({
            "name": "web-20261104-030000",
            "spec": { "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200, "disks": [],
                "replicas": [
                    { "remote": "dr", "pool": "bk", "backup": "019a4f2c-1111-7000-8000-000000000000", "state": "ready", "at": 1 },
                    { "remote": "far", "pool": "bk", "state": "failed", "error": "boom", "at": 2 }
                ]
            }
        });
        assert_eq!(backup_row(&original)[7..], ["-", "dr:ready,far:failed"]);
    }

    /// `backup-job create`의 인자를 요청 본문으로
    fn backup_job_create(args: &[&str]) -> Result<Value, clap::Error> {
        let mut argv = vec!["oplctl", "backup-job", "create", "nightly"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::BackupJob {
                cmd: BackupJobCmd::Create(c),
            } => backup_job_create_body(&c),
            _ => unreachable!(),
        })
    }

    #[test]
    fn backup_job_create_builds_the_spec() {
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--label",
                "tier=prod",
                "--label",
                "env=a=b",
                "--at",
                "03:00",
                "--days",
                "mon,wed",
                "--keep-last",
                "2",
                "--keep-weekly",
                "4",
            ])
            .unwrap(),
            json!({
                "name": "nightly",
                "spec": {
                    "vms": { "labels": { "tier": "prod", "env": "a=b" } },
                    "pool": "bk",
                    "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                    "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                    "enabled": true
                }
            })
        );
        // 요일을 주지 않으면 매일, 보존 규칙을 주지 않으면 모두 0(지우지 않음)
        let all =
            backup_job_create(&["--pool", "bk", "--all", "--at", "00:00", "--disabled"]).unwrap();
        assert_eq!(all["spec"]["vms"], json!({ "all": true }));
        assert_eq!(
            all["spec"]["schedule"],
            json!({ "at": "00:00", "days": [] })
        );
        assert_eq!(
            all["spec"]["keep"],
            json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 })
        );
        assert_eq!(all["spec"]["enabled"], json!(false));
        // 복제하지 않으면 `replicate`를 보내지 않는다.
        assert!(all["spec"].get("replicate").is_none(), "{all}");
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/bk"
            ])
            .unwrap()["spec"]["replicate"],
            json!({ "remote": "dr", "pool": "bk" })
        );
        assert_eq!(
            backup_job_create(&["--pool", "bk", "--vm", "web", "--vm", "db", "--at", "03:00"])
                .unwrap()["spec"]["vms"],
            json!({ "names": ["web", "db"] })
        );
        // 전체 백업 주기는 주었을 때만 보낸다.
        assert!(all["spec"].get("full_every").is_none(), "{all}");
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--full-every",
                "6"
            ])
            .unwrap()["spec"]["full_every"],
            json!(6)
        );
        // VM 고르기는 셋 중 꼭 하나, 풀과 시각은 꼭, 라벨은 KEY=VALUE, 요일은 mon..sun
        for args in [
            &["--pool", "bk", "--at", "03:00"][..],
            &["--pool", "bk", "--all", "--vm", "web", "--at", "03:00"],
            &[
                "--pool", "bk", "--vm", "web", "--label", "a=b", "--at", "03:00",
            ],
            &["--all", "--at", "03:00"],
            &["--pool", "bk", "--all"],
            &["--pool", "bk", "--label", "tier", "--at", "03:00"],
            &["--pool", "bk", "--label", "=prod", "--at", "03:00"],
            &["--pool", "bk", "--all", "--at", "03:00", "--days", "monday"],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--keep-last",
                "-1",
            ],
            // 복제 대상은 REMOTE/POOL
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "/bk",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/bk/x",
            ],
        ] {
            assert!(backup_job_create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_set_changes_only_what_is_given() {
        let current = json!({
            "name": "nightly",
            "revision": 9,
            "spec": {
                "vms": { "all": true }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["sat"] },
                "keep": { "last": 2, "daily": 7, "weekly": 0, "monthly": 0 },
                "enabled": true
            }
        });
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup-job", "set", "nightly"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::BackupJob {
                    cmd: BackupJobCmd::Set(s),
                } => backup_job_set_body(&s, &current),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--enabled", "false"]).unwrap(),
            json!({ "enabled": false })
        );
        assert_eq!(
            set(&["--vm", "web", "--pool", "bk2"]).unwrap(),
            json!({ "vms": { "names": ["web"] }, "pool": "bk2" })
        );
        // 일정과 보존 규칙은 통째로 바뀌므로 주지 않은 부분은 지금 값을 보낸다.
        assert_eq!(
            set(&["--at", "04:30"]).unwrap(),
            json!({ "schedule": { "at": "04:30", "days": ["sat"] } })
        );
        assert_eq!(
            set(&["--days", "sun,mon"]).unwrap(),
            json!({ "schedule": { "at": "03:00", "days": ["sun", "mon"] } })
        );
        assert_eq!(
            set(&["--keep-monthly", "6", "--keep-last", "1"]).unwrap(),
            json!({ "keep": { "last": 1, "daily": 7, "weekly": 0, "monthly": 6 } })
        );
        // 복제는 대상을 주면 바꾸고 --no-replicate면 null(끔)
        assert_eq!(
            set(&["--replicate", "dr/bk"]).unwrap(),
            json!({ "replicate": { "remote": "dr", "pool": "bk" } })
        );
        assert_eq!(
            set(&["--no-replicate"]).unwrap(),
            json!({ "replicate": null })
        );
        // 0이면 늘 전체로 돌아간다.
        assert_eq!(
            set(&["--full-every", "6"]).unwrap(),
            json!({ "full_every": 6 })
        );
        assert_eq!(
            set(&["--full-every", "0"]).unwrap(),
            json!({ "full_every": 0 })
        );
        // 바꿀 것은 꼭 하나, VM 고르기는 하나만, 사용 여부는 true나 false, 복제는 켜거나 끄거나
        for args in [
            &[][..],
            &["--all", "--vm", "web"],
            &["--enabled"],
            &["--enabled", "yes"],
            &["--replicate", "dr/bk", "--no-replicate"],
            &["--replicate", "dr"],
            &["--full-every", "-1"],
        ] {
            assert!(set(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_list_shows_the_schedule_and_runs() {
        let job = json!({
            "name": "nightly",
            "spec": {
                "vms": { "labels": { "env": "a", "tier": "prod" } }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                "enabled": true
            },
            "status": {
                "next_run": 1_793_761_200, "run": null,
                "last_run": { "started_at": 1_793_674_800, "finished_at": 1_793_675_100, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "db", "error": "boom" }]
            }
        });
        assert_eq!(
            backup_job_row(&job),
            [
                "nightly",
                "bk",
                "env=a,tier=prod",
                "mon,wed 03:00 UTC",
                "last=2,weekly=4",
                "true",
                "2026-11-04 03:00 UTC",
                "2026-11-03 03:05 UTC (3 ok, 1 failed)"
            ]
        );
        // 매일, 보존 규칙 없음, 진행 중
        let mut running = job.clone();
        running["spec"]["vms"] = json!({ "names": ["web", "db"] });
        running["spec"]["schedule"]["days"] = json!([]);
        running["spec"]["keep"] = json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 });
        running["status"]["run"] = json!({ "started_at": 1, "queue": [], "backups": [] });
        assert_eq!(
            backup_job_row(&running)[2..],
            [
                "web,db",
                "daily 03:00 UTC",
                "all",
                "true",
                "2026-11-04 03:00 UTC",
                "running"
            ]
        );
        // 끈 작업은 다음 실행이 없고, 한 번도 돌지 않았으면 마지막 실행도 없다.
        let mut off = job.clone();
        off["spec"]["vms"] = json!({ "all": true });
        off["spec"]["enabled"] = json!(false);
        off["status"]["last_run"] = Value::Null;
        assert_eq!(backup_job_row(&off)[2], "all");
        assert_eq!(backup_job_row(&off)[5..], ["false", "-", "-"]);
        // 증분 백업 작업은 전체 백업 주기를 함께 보인다(0은 늘 전체).
        let mut incremental = job.clone();
        incremental["spec"]["full_every"] = json!(6);
        assert_eq!(
            backup_job_row(&incremental)[3],
            "mon,wed 03:00 UTC, full every 6"
        );
        incremental["spec"]["full_every"] = json!(0);
        assert_eq!(backup_job_row(&incremental)[3], "mon,wed 03:00 UTC");
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 브리지와 VXLAN 중 정확히 하나. VLAN은 브리지와, MTU는 VXLAN과만. 주지 않은 값은 보내지 않는다.
    #[test]
    fn parses_network_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "network", "create", "lan"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Network {
                    cmd: NetworkCmd::Create(c),
                } => network_create_body(&c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--bridge", "vmbr0"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } })
        );
        assert_eq!(
            create(&["--bridge", "vmbr0", "--vlan", "10"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0", "vlan": 10 } })
        );
        assert_eq!(
            create(&["--vxlan", "100"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 } } })
        );
        assert_eq!(
            create(&["--vxlan", "100", "--mtu", "1400"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 }, "mtu": 1400 } })
        );
        for args in [
            &[][..],
            &["--bridge", "vmbr0", "--vxlan", "100"],
            &["--vxlan", "100", "--vlan", "10"],
            &["--bridge", "vmbr0", "--mtu", "1400"],
            &["--vlan", "10"],
            &["--mtu", "1400"],
            &["--vxlan", "-1"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 종류, 붙는 브리지(VXLAN은 관리 이름), VLAN이나 VNI, VXLAN MTU(없으면 기본값)
    #[test]
    fn network_list_shows_kind_bridge_and_mtu() {
        let rows: Vec<_> = [
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
            json!({ "name": "dmz", "spec": { "bridge": "vmbr0", "vlan": 20 } }),
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 100 } } }),
            json!({ "name": "jumbo", "spec": { "vxlan": { "vni": 7 }, "mtu": 8950 } }),
        ]
        .iter()
        .map(network_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NETWORK_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   TYPE    BRIDGE   VLAN/VNI  MTU\n\
             lan    bridge  vmbr0    -         -\n\
             dmz    bridge  vmbr0    20        -\n\
             ov     vxlan   oplv100  100       1450\n\
             jumbo  vxlan   oplv7    7         8950\n"
        );
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 받은 요청 줄을 `seen`에 남기고 경로에 맞는 JSON을 200으로 돌려주는 작은 HTTP 서버
    async fn fake_api(
        reply: fn(&str) -> Value,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        fake_api_status(move |path| (200, reply(path))).await
    }

    /// `fake_api`와 같고 응답 상태 코드도 정한다.
    async fn fake_api_status(
        reply: impl Fn(&str) -> (u16, Value) + Send + 'static,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(line);
                let (status, body) = reply(&path);
                let body = body.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        (client, seen)
    }

    /// `backup delete --force`는 `?force=true`로 보내고, 기록만 지웠으면 서버의 알림을 보인다.
    /// 아니면 Task를 기다린다.
    #[tokio::test]
    async fn backup_delete_force_shows_the_server_message() {
        let cli = Cli::try_parse_from(["oplctl", "backup", "delete", "b1", "--force"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                cmd: BackupCmd::Delete { force: true, .. }
            }
        ));

        let (client, seen) =
            fake_api(|_| json!({ "message": "backup \"b1\" deleted; its files are left" })).await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", true, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/backups/b1?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup \"b1\" deleted; its files are left\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/backups/b1 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");

        // 원격 사본을 지우지 못했으면 Task의 경고를 보인다.
        let (client, _) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": {
                    "state": "succeeded",
                    "error": "backup deleted, but remote copies may be left: dr: unreachable"
                } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup b1 deleted\nwarning: backup deleted, but remote copies may be left: dr: unreachable\n"
        );
    }

    /// `backup replicate`는 원격·풀을 본문으로 복제 Task를 만들고 따라가는 방법을 알린다. `--wait`이면
    /// Task를 기다린다.
    #[tokio::test]
    async fn backup_replicate_starts_the_task() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/backups/web-1/actions/replicate" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Backup {
                    cmd:
                        BackupCmd::Replicate {
                            backup,
                            remote,
                            pool,
                            wait,
                        },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                backup_replicate(&client, &backup, &remote, &pool, wait, &mut out)
                    .await
                    .unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&[
                "oplctl",
                "backup",
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk"
            ])
            .await,
            "replication of backup web-1 to remote dr started (task t1); follow it with `orbivisorctl task get t1`\n"
        );
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "{seen:?}");
            assert_eq!(
                seen[0].line,
                "POST /api/v1/backups/web-1/actions/replicate HTTP/1.1"
            );
            assert_eq!(seen[0].body, json!({ "remote": "dr", "pool": "bk" }));
        }
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&[
                "oplctl",
                "backup",
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk",
                "--wait"
            ])
            .await,
            "backup web-1 replicated to pool bk of remote dr\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "POST /api/v1/backups/web-1/actions/replicate HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `backup-job set`은 작업을 읽어 그 값에 맞춰 PATCH하고, `run`은 본문 없이 POST한다.
    #[tokio::test]
    async fn backup_job_set_reads_then_patches_and_run_posts() {
        let (client, seen) = fake_api(|_| {
            json!({
                "name": "nightly", "revision": 3,
                "spec": { "schedule": { "at": "03:00", "days": [] }, "keep": {} },
                "status": {}
            })
        })
        .await;
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "set", "nightly", "--at", "04:00"])
            .unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        let mut out = Vec::new();
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "run", "nightly"]).unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/backup-jobs/nightly HTTP/1.1",
                "PATCH /api/v1/backup-jobs/nightly HTTP/1.1",
                "POST /api/v1/backup-jobs/nightly/run HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup job nightly updated\nbackup job nightly will run at the scheduler's next check\n"
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        a["status"]["underlay"] = json!("10.0.0.1");
        a["status"]["version"] = json!("0.2.0");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        // underlay가 없는 노드(위트니스, 옛 서버)는 `-`다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        VERSION  UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  0.2.0    10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -        -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -        -         true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 버전을 아는 멤버의 버전이 둘 이상이면 낮은 것부터 한 번씩 적어 경고한다. 모르는 멤버는 세지 않는다.
    #[test]
    fn cluster_status_warns_about_mixed_versions() {
        let member = |name: &str, version: Option<&str>| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": true,
                "online": true,
                "witness": false,
                "version": version,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.9.0")),
            member("c", Some("0.10.0")),
            member("d", Some("0.10.0~rc1")),
            member("e", None),
        ]);
        assert!(
            out.ends_with(
                "warning: nodes run different Orbivisor versions (0.9.0, 0.10.0~rc1, 0.10.0); \
                 finish the upgrade on every node\n"
            ),
            "{out}"
        );
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.10.0")),
            member("c", None),
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 방향과 동작은 꼭, 프로토콜·포트·CIDR은 이 차례로 생략할 수 있다.
    #[test]
    fn parses_rules() {
        assert_eq!(
            parse_rule("in accept tcp 22 10.0.0.0/8"),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "tcp",
                "ports": "22", "remote": "10.0.0.0/8"
            }))
        );
        assert_eq!(
            parse_rule("out drop tcp 25"),
            Ok(json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }))
        );
        assert_eq!(
            parse_rule("in accept icmp"),
            Ok(json!({ "direction": "in", "action": "accept", "protocol": "icmp" }))
        );
        assert_eq!(
            parse_rule("  in   accept udp 8000-8080 "),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "udp", "ports": "8000-8080"
            }))
        );
        assert_eq!(
            parse_rule("out accept 2001:db8::/32"),
            Ok(json!({ "direction": "out", "action": "accept", "remote": "2001:db8::/32" }))
        );
        assert_eq!(
            parse_rule("in drop"),
            Ok(json!({ "direction": "in", "action": "drop" }))
        );
        // 포트 범위와 프로토콜·포트의 짝은 서버가 본다.
        assert_eq!(
            parse_rule("in accept 99999"),
            Ok(json!({ "direction": "in", "action": "accept", "ports": "99999" }))
        );
        for bad in [
            "",
            "in",
            "accept in",
            "up accept",
            "in allow",
            "in accept sctp",
            "in accept tcp 22 10.0.0.0/8 extra",
            "in accept 10.0.0.0/8 tcp",
            "in accept tcp 10.0.0.0/8 22",
            "in accept tcp ssh",
            "in accept tcp 22-",
            "in accept tcp -22",
            "IN accept",
        ] {
            let err = parse_rule(bad).unwrap_err();
            assert!(err.contains("in|out accept|drop"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_security_group_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "security-group"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::SecurityGroup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&[
            "create",
            "web",
            "--description",
            "web servers",
            "--rule",
            "in accept tcp 443",
            "--rule",
            "in accept icmp",
        ])
        .unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [
                    { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" },
                    { "direction": "in", "action": "accept", "protocol": "icmp" },
                ]
            }})
        );
        // 설명이 없으면 보내지 않는다. 규칙 없는 그룹도 된다.
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&["create", "empty"]).unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "empty", "spec": { "rules": [] } })
        );
        assert!(parse(&["create", "web", "--rule", "in maybe"]).is_err());
        assert!(matches!(
            parse(&["rule", "add", "web", "out drop tcp 25", "--at", "2"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Add { group, rule, at: Some(2) } }
                if group == "web" && rule["ports"] == "25"
        ));
        assert!(matches!(
            parse(&["rule", "rm", "web", "3"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Rm { group, n: 3 } } if group == "web"
        ));
        assert!(parse(&["rule", "add", "web", "sideways accept"]).is_err());
        assert!(parse(&["rule", "rm", "web", "first"]).is_err());
        assert!(matches!(
            parse(&["get", "web"]).unwrap(),
            SecurityGroupCmd::Get { group } if group == "web"
        ));
        assert!(matches!(
            parse(&["delete", "web"]).unwrap(),
            SecurityGroupCmd::Delete { group } if group == "web"
        ));
    }

    /// 규칙 번호는 1부터다. 범위 밖이면 아무것도 바꾸지 않고 실패한다.
    #[test]
    fn adds_and_removes_rules_by_number() {
        let rule =
            |n: u16| json!({ "direction": "in", "action": "accept", "ports": n.to_string() });
        let group = json!({ "name": "web", "spec": { "rules": [rule(1), rule(2)] } });
        assert_eq!(
            rules_with(&group, rule(9), None).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(1)).unwrap(),
            vec![rule(9), rule(1), rule(2)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(3)).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        for at in [0, 4] {
            let err = rules_with(&group, rule(9), Some(at))
                .unwrap_err()
                .to_string();
            assert!(err.contains("use 1 to 3"), "{err}");
        }
        assert_eq!(rules_without(&group, 1).unwrap(), vec![rule(2)]);
        assert_eq!(rules_without(&group, 2).unwrap(), vec![rule(1)]);
        for n in [0, 3] {
            let err = rules_without(&group, n).unwrap_err().to_string();
            assert!(err.contains(&format!("has no rule {n}")), "{err}");
        }
        // 규칙이 없는 그룹은 응답에 rules가 비어 있다.
        let empty = json!({ "name": "e", "spec": { "rules": [] } });
        assert_eq!(rules_with(&empty, rule(1), None).unwrap(), vec![rule(1)]);
        assert!(rules_without(&empty, 1).is_err());
    }

    #[test]
    fn security_group_list_shows_rule_counts() {
        let rows: Vec<_> = [
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [{ "direction": "in", "action": "accept" }, { "direction": "out", "action": "drop" }]
            }}),
            json!({ "name": "empty", "spec": { "rules": [] } }),
        ]
        .iter()
        .map(security_group_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, SECURITY_GROUP_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   RULES  DESCRIPTION\n\
             web    2      web servers\n\
             empty  0      -\n"
        );
    }

    /// `--nic NETWORK[,sg=A+B]`. 그룹이 없으면 `security_groups`를 보내지 않는다.
    #[test]
    fn parses_nics_with_security_groups() {
        let nics = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "create", "web"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Vm {
                    cmd: VmCmd::Create(c),
                } => c.nics.iter().map(nic_body).collect::<Vec<_>>(),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            nics(&[
                "--nic",
                "user",
                "--nic",
                "lan,sg=web+db",
                "--nic",
                "dmz,sg=web"
            ])
            .unwrap(),
            vec![
                json!({ "network": "user", "model": "virtio" }),
                json!({ "network": "lan", "model": "virtio", "security_groups": ["web", "db"] }),
                json!({ "network": "dmz", "model": "virtio", "security_groups": ["web"] }),
            ]
        );
        for bad in [
            "",
            ",sg=web",
            "lan,",
            "lan,sg=",
            "lan,sg=web+",
            "lan,sg=+web",
            "lan,group=web",
            "lan,sg=web,sg=db",
        ] {
            assert!(nics(&["--nic", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_vm_nic_sg() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "1", "a", "b"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { vm, index: 1, groups } }
                if vm == "web" && groups == ["a", "b"]
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "0"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { index: 0, groups, .. } } if groups.is_empty()
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "-1"]).is_err());
    }

    /// 그 NIC의 그룹만 바꾸고 나머지(다른 NIC, 다른 필드, 라벨)는 그대로 둔다.
    #[test]
    fn nic_groups_update_changes_one_nic() {
        let vm = json!({
            "name": "web",
            "revision": 4,
            "labels": { "team": "a" },
            "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["old"] },
                ],
            },
        });
        let groups = ["web".to_string(), "db".to_string()];
        assert_eq!(
            nic_groups_update(&vm, 1, &groups).unwrap(),
            json!({ "labels": { "team": "a" }, "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["web", "db"] },
                ],
            }})
        );
        assert_eq!(
            nic_groups_update(&vm, 1, &[]).unwrap()["spec"]["nics"][1]["security_groups"],
            json!([])
        );
        let err = nic_groups_update(&vm, 2, &groups).unwrap_err().to_string();
        assert_eq!(err, "VM web has no NIC 2 (2 NICs, numbered from 0)");
    }

    /// `import` 명령의 인자
    fn parse_import(args: &[&str]) -> Result<ImportCmd, clap::Error> {
        let mut argv = vec!["oplctl", "import"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::Import { cmd } => cmd,
            _ => unreachable!(),
        })
    }

    /// `import run`의 인자를 요청 본문으로. `sources`는 가져오기의 원본 네트워크다.
    fn import_run(args: &[&str], sources: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["run", "web01"];
        argv.extend_from_slice(args);
        let ImportCmd::Run(run) = parse_import(&argv)? else {
            unreachable!()
        };
        let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
        import_run_body(&run, &sources)
    }

    #[test]
    fn parses_import_commands() {
        assert!(matches!(
            parse_import(&["ova", "/tmp/web01.ova", "--name", "web01"]).unwrap(),
            ImportCmd::Ova { file, name } if file == std::path::Path::new("/tmp/web01.ova") && name == "web01"
        ));
        assert!(matches!(parse_import(&["list"]).unwrap(), ImportCmd::List));
        assert!(matches!(
            parse_import(&["get", "web01"]).unwrap(),
            ImportCmd::Get { import } if import == "web01"
        ));
        assert!(matches!(
            parse_import(&["delete", "web01", "--force"]).unwrap(),
            ImportCmd::Delete { import, force: true } if import == "web01"
        ));
        // 가져오기 이름, 새 VM 이름과 풀은 꼭 준다.
        for args in [
            &["ova", "/tmp/web01.ova"][..],
            &["run", "web01", "--pool", "local"],
            &["run", "web01", "--name", "web"],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--disk-bus",
                "ide",
            ],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--net",
                "VM Network=",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "=lan",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "",
            ],
        ] {
            assert!(parse_import(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn import_run_builds_the_body() {
        // 주지 않은 값은 비워 두어 서버가 제안 값을 쓴다.
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "VM Network=lan",
                    "--net",
                    "DMZ=dmz",
                    "--net",
                    "a=b=lan"
                ],
                &["DMZ", "VM Network", "a=b"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan", "DMZ": "dmz", "a=b": "lan" },
                "disk_bus": null, "nic_model": null, "firmware": null,
                "sockets": null, "cores": null, "memory_mib": null,
                "keep_macs": true
            })
        );
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "lan",
                    "--disk-bus",
                    "sata",
                    "--nic-model",
                    "e1000e",
                    "--firmware",
                    "bios",
                    "--sockets",
                    "2",
                    "--cores",
                    "4",
                    "--memory",
                    "8192",
                    "--new-macs"
                ],
                &["VM Network"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan" },
                "disk_bus": "sata", "nic_model": "e1000e", "firmware": "bios",
                "sockets": 2, "cores": 4, "memory_mib": 8192,
                "keep_macs": false
            })
        );
        assert_eq!(
            import_run(
                &["--name", "web", "--pool", "p", "--disk-bus", "virtio-blk"],
                &[]
            )
            .unwrap()["disk_bus"],
            "virtio-blk"
        );
        // `=` 없는 --net은 원본 네트워크가 하나일 때만 쓴다.
        let err = import_run(
            &["--name", "web", "--pool", "p", "--net", "lan"],
            &["A", "B"],
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 2 source networks (\"A\", \"B\")"
        );
        let err = import_run(&["--name", "web", "--pool", "p", "--net", "lan"], &[]);
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 0 source networks ()"
        );
    }

    fn sample_import() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "web01",
            "spec": {
                "node": "n-1",
                "source": { "ova": { "file_name": "web01.ova", "size_bytes": 3u64 << 29 } }
            },
            "status": {
                "phase": "ready",
                "source_vm": {
                    "name": "web01-src", "guest_os": "otherLinux64Guest", "firmware": "uefi",
                    "vcpus": 2, "cores_per_socket": 2, "memory_mib": 1024,
                    "disks": [
                        { "name": "disk0", "file": "web01-disk1.vmdk", "capacity_bytes": 10u64 << 30, "controller": "scsi" }
                    ],
                    "nics": [
                        { "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" },
                        { "network": "DMZ", "adapter": "e1000" }
                    ]
                },
                "proposal": {
                    "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
                    "disk_bus": "virtio-blk", "nic_model": "virtio",
                    "networks": { "DMZ": null, "VM Network": "VM Network" }
                },
                "warnings": ["first warning", "second warning"],
                "error": "checksum mismatch for web01-disk1.vmdk"
            }
        })
    }

    #[test]
    fn source_networks_are_the_proposal_keys() {
        assert_eq!(source_networks(&sample_import()), ["DMZ", "VM Network"]);
        assert!(source_networks(&json!({ "status": {} })).is_empty());
    }

    #[test]
    fn import_list_shows_node_source_and_vm_names() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let vms = [json!({ "id": "v-1", "name": "web" })];
        assert_eq!(
            import_row(&sample_import(), &nodes, &vms),
            ["web01", "node1", "web01-src", "ready", "-"]
        );
        // 만든 VM은 이름으로, 지운 VM·빠진 노드는 id로 보인다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        assert_eq!(import_row(&done, &nodes, &vms)[3..], ["done", "web"]);
        assert_eq!(
            import_row(&done, &[], &[])[1..],
            ["n-1", "web01-src", "done", "v-1"]
        );
        // 원본을 읽지 못한 가져오기는 파일 이름을 보인다.
        let mut failed = sample_import();
        failed["status"] = json!({ "phase": "failed" });
        assert_eq!(import_row(&failed, &nodes, &vms)[2], "web01.ova");
    }

    #[test]
    fn import_get_shows_the_source_proposal_and_warnings() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let mut out = Vec::new();
        print_import(&mut out, &sample_import(), &nodes, &[]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
vm-import: web01
node: node1
source: web01.ova (1.5GiB)
phase: ready
error: checksum mismatch for web01-disk1.vmdk
source vm: web01-src (otherLinux64Guest, uefi, 2 vCPUs, 2 cores per socket, 1024 MiB)
proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs

DISK   FILE              SIZE     CONTROLLER
disk0  web01-disk1.vmdk  10.0GiB  scsi

NETWORK     ADAPTER  MAC                PROPOSED
VM Network  vmxnet3  00:50:56:aa:bb:cc  VM Network
DMZ         e1000    -                  -

warnings:
  first warning
  second warning
"
        );
        // 끝난 가져오기는 만든 VM을, 경고가 없으면 경고 줄을 뺀다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        done["status"]["warnings"] = json!([]);
        done["status"]["error"] = Value::Null;
        let mut out = Vec::new();
        print_import(
            &mut out,
            &done,
            &nodes,
            &[json!({ "id": "v-1", "name": "web" })],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("phase: done\nvm: web\nsource vm: "), "{text}");
        assert!(
            !text.contains("warnings") && !text.contains("error"),
            "{text}"
        );
    }

    /// `import ova`는 크기로 먼저 확인하고 실제 파일 이름과 함께 올린다.
    #[tokio::test]
    async fn import_ova_checks_then_uploads_with_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        // 본문이 없어야 작은 시험 서버가 요청 줄만 읽고 답할 수 있다.
        let file = dir.path().join("web01-export.ova");
        std::fs::write(&file, b"").unwrap();
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let cmd = ImportCmd::Ova {
            file,
            name: "web01".into(),
        };
        let mut out = Vec::new();
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/vm-imports/ova/check?name=web01&size=0 HTTP/1.1",
                "POST /api/v1/vm-imports/ova?name=web01&file_name=web01-export.ova HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 uploaded; review it with `orbivisorctl import get web01`\n"
        );
    }

    /// `=` 없는 `--net`이면 가져오기를 읽어 원본 네트워크를 찾고, 실행 Task를 기다린다.
    #[tokio::test]
    async fn import_run_reads_the_import_for_a_bare_net_and_waits() {
        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else if path.ends_with("/run") {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            } else {
                json!({ "status": { "proposal": { "networks": { "VM Network": null } } } })
            }
        })
        .await;
        let run = |args: &[&str]| {
            let mut argv = vec![
                "oplctl", "import", "run", "web01", "--name", "web", "--pool", "p",
            ];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Import { cmd } => cmd,
                _ => unreachable!(),
            }
        };
        let mut out = Vec::new();
        import(&client, run(&["--net", "lan"]), false, &mut out)
            .await
            .unwrap();
        // SOURCE=NETWORK만 주면 읽지 않는다.
        import(&client, run(&["--net", "VM Network=lan"]), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/vm-imports/web01 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm web imported\nvm web imported\n"
        );
    }

    /// `import delete --force`는 `?force=true`로 보내고 서버의 알림을 보인다. 아니면 Task를 기다린다.
    #[tokio::test]
    async fn import_delete_force_shows_the_server_message() {
        let (client, seen) = fake_api(|_| {
            json!({ "message": "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)" })
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: true,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/vm-imports/web01?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: false,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/vm-imports/web01 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "vm-import web01 deleted\n");
    }

    #[test]
    fn parses_import_esxi_commands() {
        let ImportCmd::EsxiBrowse { conn, fingerprint } =
            parse_import(&["esxi-browse", "--host", "esx1", "--user", "root"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fingerprint, None);
        // 포트와 데이터센터는 ESXi 기본값이다.
        assert_eq!(
            esxi_connection(&conn, fingerprint.as_deref(), "secret"),
            json!({
                "host": "esx1", "port": 443, "user": "root", "password": "secret",
                "datacenter": "ha-datacenter", "fingerprint": null
            })
        );
        let ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } = parse_import(&[
            "esxi",
            "web01",
            "--host",
            "vc1",
            "--port",
            "8443",
            "--user",
            "admin@vsphere.local",
            "--datacenter",
            "DC1",
            "--datastore",
            "datastore1",
            "--vmx",
            "web01/web01.vmx",
            "--fingerprint",
            "AB:CD",
        ])
        .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            esxi_import_body(
                &name,
                &esxi_connection(&conn, Some(&fingerprint), "pw"),
                &datastore,
                &vmx
            ),
            json!({
                "name": "web01",
                "connection": {
                    "host": "vc1", "port": 8443, "user": "admin@vsphere.local", "password": "pw",
                    "datacenter": "DC1", "fingerprint": "AB:CD"
                },
                "datastore": "datastore1",
                "vmx": "web01/web01.vmx"
            })
        );
        // 만들기에는 지문·데이터스토어·.vmx가 꼭 있어야 하고, 비밀번호는 인자로 받지 않는다.
        let full = [
            "esxi",
            "web01",
            "--host",
            "esx1",
            "--user",
            "root",
            "--datastore",
            "ds",
            "--vmx",
            "a/a.vmx",
            "--fingerprint",
            "AB",
        ];
        assert!(parse_import(&full).is_ok());
        for skip in ["--datastore", "--vmx", "--fingerprint", "--host", "--user"] {
            let i = full.iter().position(|a| *a == skip).unwrap();
            let args: Vec<&str> = full[..i].iter().chain(&full[i + 2..]).copied().collect();
            assert!(parse_import(&args).is_err(), "{args:?}");
        }
        let mut with_password = full.to_vec();
        with_password.extend(["--password", "x"]);
        assert!(parse_import(&with_password).is_err());
        assert!(parse_import(&["esxi-browse", "--host", "esx1"]).is_err());

        // 비밀번호 파일(끝의 줄바꿈은 뺀다)
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("esxi-password");
        std::fs::write(&file, "VMware1!\n").unwrap();
        let mut with_file = full.to_vec();
        with_file.extend(["--password-file", file.to_str().unwrap()]);
        let ImportCmd::Esxi { conn, .. } = parse_import(&with_file).unwrap() else {
            unreachable!()
        };
        assert_eq!(esxi_password(&conn).unwrap(), "VMware1!");
    }

    fn sample_browse() -> Value {
        json!({
            "datastores": [
                {
                    "name": "datastore1",
                    "vms": [
                        {
                            "vmx": "web01/web01.vmx", "name": "web01", "guest_os": "ubuntu-64",
                            "vcpus": 2, "memory_mib": 1024, "disks": 1, "locked": false, "error": null
                        },
                        {
                            "vmx": "db/db.vmx", "name": "db", "guest_os": "", "vcpus": 0,
                            "memory_mib": 0, "disks": 0, "locked": true,
                            "error": "memsize is missing"
                        }
                    ]
                },
                { "name": "empty", "vms": [] }
            ]
        })
    }

    /// `import esxi-browse`는 연결을 보내고 데이터스토어의 VM을 표로 보인다. 읽지 못한 VM은 NAME
    /// 자리에 오류를 보인다.
    #[tokio::test]
    async fn import_esxi_browse_shows_the_vms() {
        let (client, seen) = fake_api(|_| sample_browse()).await;
        let mut out = Vec::new();
        let conn = json!({ "host": "esx1" });
        esxi_browse(&client, &conn, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi/browse HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
DATASTORE   VMX              NAME                       GUEST      CPU  MEMORY  DISKS  LOCKED
datastore1  web01/web01.vmx  web01                      ubuntu-64  2    1.0GiB  1      false
datastore1  db/db.vmx        error: memsize is missing  -          -    -       -      true
"
        );
        // --json이면 응답 그대로
        let mut out = Vec::new();
        esxi_browse(&client, &conn, true, &mut out).await.unwrap();
        let printed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed, sample_browse());
    }

    /// 지문을 주지 않았거나(409 `untrusted_certificate`) 틀리면 서버 인증서의 지문으로 다시 하라고 알린다.
    #[tokio::test]
    async fn import_esxi_explains_an_untrusted_certificate() {
        let (client, _) = fake_api_status(|_| {
            (
                409,
                json!({
                    "code": "untrusted_certificate",
                    "message": "verify the ESXi certificate fingerprint",
                    "details": { "fingerprint": "AB:CD:EF" }
                }),
            )
        })
        .await;
        let conn = json!({ "host": "esx1" });
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        let body = esxi_import_body("web01", &conn, "ds", "a/a.vmx");
        let err = esxi_create(&client, &body, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        // 다른 오류는 서버의 메시지와 코드 그대로
        let (client, _) = fake_api_status(|_| {
            (
                400,
                json!({ "code": "esxi_auth", "message": "ESXi rejected the user name or password" }),
            )
        })
        .await;
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ESXi rejected the user name or password (esxi_auth)"
        );
    }

    /// `import esxi`는 가져오기를 만들고 검토 명령을 알린다.
    #[tokio::test]
    async fn import_esxi_creates_the_import() {
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let mut out = Vec::new();
        let body = esxi_import_body("web01", &json!({}), "ds", "a/a.vmx");
        esxi_create(&client, &body, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 created; review it with `orbivisorctl import get web01`\n"
        );
    }

    fn sample_esxi_import() -> Value {
        let mut i = sample_import();
        i["spec"]["source"] = json!({ "esxi": {
            "host": "esx1.example.com", "port": 443, "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": "root",
            "fingerprint": "AB:CD"
        } });
        i
    }

    /// ESXi에서 가져오면 SOURCE는 `esxi://호스트/데이터스토어/.vmx`다.
    #[test]
    fn import_list_and_get_show_an_esxi_source() {
        let url = "esxi://esx1.example.com/datastore1/web01/web01.vmx";
        assert_eq!(import_row(&sample_esxi_import(), &[], &[])[2], url);
        let mut out = Vec::new();
        print_import(&mut out, &sample_esxi_import(), &[], &[]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!(
                "vm-import: web01\nnode: n-1\nsource: {url}\nphase: ready\n"
            )),
            "{text}"
        );
    }

    /// `node updates`는 확인 시각, 오류, 패키지 표, 잘렸는지를 보인다. `--refresh`면 다시 확인 Task를
    /// 기다린 뒤 읽는다.
    #[tokio::test]
    async fn node_updates_shows_the_check() {
        fn reply(path: &str) -> Value {
            match path {
                "/api/v1/nodes/n1/updates/refresh" => json!({ "id": "t1" }),
                "/api/v1/tasks/t1" => json!({ "id": "t1", "status": { "state": "succeeded" } }),
                "/api/v1/nodes/n1" => json!({
                    "name": "n1",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000,
                        "packages": [
                            { "name": "orbivisor", "current": "0.2.0", "candidate": "0.2.1" },
                            { "name": "linux-image-6.12.48+deb13-amd64", "current": null, "candidate": "6.12.48-1" }
                        ],
                        "truncated": true,
                        "error": null
                    } }
                }),
                "/api/v1/nodes/n2" => json!({
                    "name": "n2",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000, "packages": [],
                        "error": "apt-get update -q failed (exit status: 100): E: no network"
                    } }
                }),
                "/api/v1/nodes/n3" => json!({
                    "name": "n3",
                    "status": { "updates": { "checked_at": 1_793_000_000, "packages": [], "error": null } }
                }),
                "/api/v1/nodes/n4" => json!({
                    "name": "n4",
                    "status": { "updates": { "checked_at": null, "packages": [], "error": null } }
                }),
                _ => json!({ "name": "w", "status": { "online": true } }),
            }
        }
        let (client, seen) = fake_api(reply).await;
        let run = |args: &'static [&'static str], json_out: bool| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Updates { node, refresh },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_updates(&client, &node, refresh, json_out, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        let text = run(&["oplctl", "node", "updates", "n1", "--refresh"], false)
            .await
            .unwrap();
        assert_eq!(
            text,
            "checked: 2026-10-26 07:33 UTC\n\
             PACKAGE                          CURRENT  CANDIDATE\n\
             orbivisor                        0.2.0    0.2.1\n\
             linux-image-6.12.48+deb13-amd64  -        6.12.48-1\n\
             (only the first 2 packages are shown)\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/updates/refresh HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1"
            ]
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n2"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\n\
             error: apt-get update -q failed (exit status: 100): E: no network\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n3"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\nno updates available\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n4"], false)
                .await
                .unwrap(),
            "checked: never\n"
        );
        let err = run(&["oplctl", "node", "updates", "w"], false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "node w does not check for updates");
        let shown: Value = serde_json::from_str(
            &run(&["oplctl", "node", "updates", "n3"], true)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(shown["packages"], json!([]));
    }

    /// `node upgrade`는 업그레이드 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다린다.
    #[tokio::test]
    async fn node_upgrade_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/upgrade" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Upgrade { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_upgrade(&client, &node, wait, &mut out).await.unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1"]).await,
            "upgrade of n1 started (task t1); follow it with `orbivisorctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/upgrade HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1", "--wait"]).await,
            "node n1 upgraded\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/upgrade HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `node maintenance`는 on·off만 받는다. 노드를 읽어 라벨과 spec은 그대로 두고 `maintenance`만 바꿔
    /// 읽은 revision으로(`If-Match`) 다시 쓴다.
    #[tokio::test]
    async fn node_maintenance_puts_the_spec() {
        let run = |args: &'static [&'static str], base: String| async move {
            let Cmd::Node {
                cmd: NodeCmd::Maintenance { node, state },
            } = Cli::try_parse_from(args).unwrap().cmd
            else {
                unreachable!()
            };
            let client = Client::new(&base, None, None).unwrap();
            let mut out = Vec::new();
            let mut err = Vec::new();
            node_maintenance(&client, &node, state == Switch::On, &mut out, &mut err)
                .await
                .unwrap();
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
            String::from_utf8(out).unwrap()
        };
        for args in [
            &["oplctl", "node", "maintenance", "n1"][..],
            &["oplctl", "node", "maintenance", "n1", "yes"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }

        let (base, seen) = fake_api_bodies(|_| {
            json!({
                "name": "n1",
                "revision": 7,
                "labels": { "rack": "a" },
                "spec": { "address": "10.0.0.1:7443", "maintenance": false, "witness": false },
                "status": { "online": true }
            })
        })
        .await;
        assert_eq!(
            run(&["oplctl", "node", "maintenance", "n1", "on"], base.clone()).await,
            "node n1: maintenance on\n"
        );
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2, "{seen:?}");
            assert_eq!(seen[0].line, "GET /api/v1/nodes/n1 HTTP/1.1");
            assert_eq!(seen[1].line, "PUT /api/v1/nodes/n1 HTTP/1.1");
            assert_eq!(seen[1].if_match.as_deref(), Some("7"));
            assert_eq!(
                seen[1].body,
                json!({
                    "labels": { "rack": "a" },
                    "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false }
                })
            );
        }
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "maintenance", "n1", "off"], base).await,
            "node n1: maintenance off\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].body["spec"]["maintenance"], false);
        // 끄면 그 노드의 비우기 Task를 찾아본다(없으면 경고하지 않는다).
        assert_eq!(seen[2].line, "GET /api/v1/tasks HTTP/1.1");
    }

    /// 유지보수를 끄면 그 노드의 끝나지 않은 비우기가 멈춘다고 stderr에 알린다.
    #[tokio::test]
    async fn node_maintenance_off_warns_about_a_running_drain() {
        let (base, _) = fake_api_bodies(|path| {
            if path == "/api/v1/tasks" {
                let drain = |id: &str, node: &str, state: &str| {
                    json!({
                        "id": id,
                        "spec": { "op": { "kind": "node-drain" }, "target": node, "node": node },
                        "status": { "state": state }
                    })
                };
                json!({ "items": [
                    drain("t1", "id-1", "failed"),
                    drain("t2", "id-2", "running"),
                    drain("t3", "id-1", "running"),
                ] })
            } else {
                json!({
                    "id": "id-1",
                    "name": "n1",
                    "revision": 7,
                    "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false },
                    "status": { "online": true }
                })
            }
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        node_maintenance(&client, "n1", false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "node n1: maintenance off\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: the running drain of n1 stops (task t3)\n"
        );
    }

    /// 유지보수는 이미 꺼졌으므로, 비우기 Task를 찾아보지 못해도 경고만 하고 성공한다.
    #[tokio::test]
    async fn node_maintenance_off_succeeds_when_tasks_cannot_be_read() {
        let (client, _) = fake_api_status(|path| {
            if path == "/api/v1/tasks" {
                (
                    500,
                    json!({ "code": "internal", "message": "tasks unavailable" }),
                )
            } else {
                (
                    200,
                    json!({
                        "id": "id-1",
                        "name": "n1",
                        "revision": 7,
                        "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false },
                        "status": { "online": true }
                    }),
                )
            }
        })
        .await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        node_maintenance(&client, "n1", false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "node n1: maintenance off\n"
        );
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.starts_with("warning: cannot check for a running drain: ")
                && err.contains("tasks unavailable"),
            "{err}"
        );
    }

    /// 읽은 뒤 노드가 바뀌어 PUT이 `revision_mismatch`면 다시 읽어 쓴다.
    #[tokio::test]
    async fn node_maintenance_retries_a_revision_mismatch() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            // GET, PUT(409), GET, PUT 차례다.
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                1 => (
                    409,
                    json!({ "code": "revision_mismatch", "message": "node n1 changed" }),
                ),
                _ => (
                    200,
                    json!({
                        "id": "id-1",
                        "name": "n1",
                        "revision": 7,
                        "spec": { "address": "10.0.0.1:7443", "maintenance": false, "witness": false },
                        "status": { "online": true }
                    }),
                ),
            }
        })
        .await;
        let mut out = Vec::new();
        node_maintenance(&client, "n1", true, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "node n1: maintenance on\n");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/nodes/n1 HTTP/1.1",
                "PUT /api/v1/nodes/n1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1",
                "PUT /api/v1/nodes/n1 HTTP/1.1",
            ]
        );
    }

    /// `node drain`은 비우기 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다리고, Task가
    /// 실패하면(옮기지 못한 VM) 그 오류로 끝난다.
    #[tokio::test]
    async fn node_drain_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/drain" => json!({ "id": "t1" }),
            "/api/v1/nodes/n2/drain" => json!({ "id": "t2" }),
            "/api/v1/tasks/t2" => json!({ "id": "t2", "status": {
                "state": "failed",
                "error": "1 VM(s) were not moved: db (disks on a local pool)"
            } }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Drain { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_drain(&client, &node, wait, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "drain", "n1"]).await.unwrap(),
            "drain of n1 started (task t1); follow it with `orbivisorctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/drain HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "drain", "n1", "--wait"])
                .await
                .unwrap(),
            "node n1 drained\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/drain HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        let err = run(&["oplctl", "node", "drain", "n2", "--wait"])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "task t2 failed: 1 VM(s) were not moved: db (disks on a local pool)"
        );
    }

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
    #[test]
    fn settings_set_body_maps_known_keys() {
        assert_eq!(
            settings_set_body(&["updates.channel=testing".into()]).unwrap(),
            json!({ "updates": { "channel": "testing" } })
        );
        let err = settings_set_body(&["updates.mirror=x".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown setting updates.mirror; known: updates.channel"
        );
        let err = settings_set_body(&["updates.channel".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected KEY=VALUE, got \"updates.channel\""
        );
    }

    /// `settings get`은 spec을 `키: 값` 줄로, `-o json`이면 리소스 그대로 보인다. `set`은 PATCH한다.
    #[tokio::test]
    async fn settings_get_and_set() {
        let (client, seen) = fake_api(|_| {
            json!({
                "id": "00000000-0000-0000-0000-000000000000", "name": "cluster", "revision": 0,
                "spec": { "updates": { "channel": "stable" } }, "status": {}
            })
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "settings", "get"][..],
            &["oplctl", "settings", "set", "updates.channel=testing"][..],
        ] {
            let Cmd::Settings { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            settings(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "updates.channel: stable\nsettings updated\n"
        );

        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "get"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let mut out = Vec::new();
        settings(&client, cmd, true, &mut out).await.unwrap();
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["spec"]["updates"]["channel"], "stable");

        // 모르는 키는 요청을 보내지 않는다.
        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "set", "x=1"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        assert!(
            settings(&client, cmd, false, &mut Vec::new())
                .await
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    /// `subscription show`는 상태를 줄로 보인다. 한도가 없으면 쓰는 노드 수만.
    #[test]
    fn subscription_lines_show_the_state() {
        let active = json!({
            "state": "active", "id": "sub-1", "customer": "Example Corp", "level": "premium",
            "nodes": 3, "used_nodes": 2, "expires_at": 1_700_000_000u64, "development": true
        });
        assert_eq!(
            subscription_lines(&active),
            [
                "state: active (development key)",
                "customer: Example Corp",
                "level: premium",
                "nodes: 2/3",
                "expires: 2023-11-14",
            ]
        );
        let expired = json!({
            "state": "expired", "id": "sub-1", "customer": "C", "level": "standard",
            "nodes": 1, "used_nodes": 1, "expires_at": 0, "development": false
        });
        assert_eq!(subscription_lines(&expired)[0], "state: expired");
        assert_eq!(
            subscription_lines(&json!({ "state": "none", "used_nodes": 1 })),
            ["state: none", "nodes: 1"]
        );
    }

    /// `subscription set --file -`은 표준 입력에서 키를 읽는다(앞뒤 공백·줄바꿈을 다듬음).
    #[test]
    fn subscription_key_can_come_from_stdin() {
        let args = ["oplctl", "subscription", "set", "--file", "-"];
        let Cmd::Subscription {
            cmd: SubscriptionCmd::Set { key: None, file },
        } = Cli::try_parse_from(args).unwrap().cmd
        else {
            panic!("parse --file -")
        };
        assert_eq!(file.unwrap().to_str(), Some("-"));
        let mut input = std::io::Cursor::new(" opl1.a.b\r\n");
        assert_eq!(read_stdin_key(&mut input).unwrap(), "opl1.a.b");
    }

    /// `subscription set`은 키(인자나 파일)를 PUT하고 상태를 보인다. `remove`는 DELETE한다.
    #[tokio::test]
    async fn subscription_set_show_and_remove() {
        let (client, seen) = fake_api(|_| {
            json!({ "state": "active", "customer": "C", "level": "standard", "nodes": 2,
                    "used_nodes": 1, "expires_at": 1_700_000_000u64, "development": false })
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "opl1.a.b\r\n").unwrap();
        let file = file.to_str().unwrap();
        let mut out = Vec::new();
        for args in [
            &["oplctl", "subscription", "show"][..],
            &["oplctl", "subscription", "set", "opl1.a.b"][..],
            &["oplctl", "subscription", "set", "--file", file][..],
            &["oplctl", "subscription", "remove"][..],
        ] {
            let Cmd::Subscription { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            subscription(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "DELETE /api/v1/subscription HTTP/1.1",
            ]
        );
        let shown =
            "state: active\ncustomer: C\nlevel: standard\nnodes: 1/2\nexpires: 2023-11-14\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}subscription removed\n")
        );

        // 키나 파일 중 하나만 준다.
        for args in [
            &["oplctl", "subscription", "set"][..],
            &["oplctl", "subscription", "set", "k", "--file", "f"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    /// `telemetry show`는 켜짐·끝점·id·마지막 전송·오류를 줄로 보인다.
    #[test]
    fn telemetry_lines_show_the_state() {
        let settings = json!({
            "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
            "status": { "telemetry": {
                "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last_sent_at": 1_761_471_000, "last_error": "HTTP 500"
            } }
        });
        assert_eq!(
            telemetry_lines(&settings),
            [
                "enabled: true",
                "endpoint: https://t.example/r",
                "id: 6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last sent: 2025-10-26 09:30 UTC",
                "last error: HTTP 500",
            ]
        );
        let default_endpoint = if opl_types::version::TELEMETRY_URL.is_empty() {
            "endpoint: none".to_string()
        } else {
            format!(
                "endpoint: {} (build default)",
                opl_types::version::TELEMETRY_URL
            )
        };
        assert_eq!(
            telemetry_lines(&json!({ "spec": { "updates": {} }, "status": {} })),
            [
                "enabled: false".to_string(),
                default_endpoint,
                "id: none".into(),
                "last sent: never".into(),
                "last error: none".into(),
            ]
        );
    }

    #[test]
    fn telemetry_bodies_send_only_what_was_given() {
        assert_eq!(
            telemetry_body(true, None),
            json!({ "telemetry": { "enabled": true } })
        );
        assert_eq!(
            telemetry_body(true, Some("https://t.example/r")),
            json!({ "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } })
        );
        assert_eq!(
            telemetry_body(false, None),
            json!({ "telemetry": { "enabled": false } })
        );
    }

    /// `telemetry`의 하위 명령이 부르는 API. preview는 받은 JSON을 그대로 예쁘게 보인다.
    #[tokio::test]
    async fn telemetry_commands_call_the_api() {
        let (client, seen) = fake_api(|path| {
            if path == "/api/v1/telemetry/preview" {
                json!({ "schema": 1, "vms": { "total": 2 } })
            } else {
                json!({
                    "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
                    "status": { "telemetry": { "id": "abc" } }
                })
            }
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "telemetry", "show"][..],
            &[
                "oplctl",
                "telemetry",
                "enable",
                "--endpoint",
                "https://t.example/r",
            ][..],
            &["oplctl", "telemetry", "disable"][..],
            &["oplctl", "telemetry", "reset-id"][..],
            &["oplctl", "telemetry", "preview"][..],
        ] {
            let Cmd::Telemetry { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            telemetry(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "POST /api/v1/telemetry/reset-id HTTP/1.1",
                "GET /api/v1/telemetry/preview HTTP/1.1",
            ]
        );
        let shown = "enabled: true\nendpoint: https://t.example/r\nid: abc\nlast sent: never\nlast error: none\n";
        let preview = "{\n  \"schema\": 1,\n  \"vms\": {\n    \"total\": 2\n  }\n}\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}{shown}{preview}")
        );
    }

    /// `node stats`의 노드 점 하나
    fn node_sample(t: u64, cpu: f64) -> Value {
        json!({
            "t": t, "cpu": cpu, "mem_used_mib": 1024, "mem_total_mib": 4096,
            "disk_read_bps": 1536.0, "disk_write_bps": null, "net_rx_bps": 2048.5,
            "net_tx_bps": 0.0, "fs_used_mib": 10240, "fs_total_mib": 102400
        })
    }

    /// `node stats`는 노드마다 마지막 점을 노드 이름으로 보이고, 점이 없는 노드는 `-`다. 읽지 못한 노드와
    /// 옛 노드는 stderr에 알린다. `-o json`은 응답 그대로(모든 점)이고 노드 목록을 읽지 않는다.
    #[tokio::test]
    async fn node_stats_shows_the_last_sample_of_each_node() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes" => json!({ "items": [
                { "id": "id-a", "name": "node-a" }, { "id": "id-b", "name": "node-b" }
            ] }),
            _ => json!({
                "nodes": [
                    { "node": "id-a", "samples": [
                        node_sample(1_762_387_200_123, 99.0),
                        node_sample(1_762_387_205_123, 12.345)
                    ] },
                    { "node": "id-b", "samples": [] }
                ],
                "unreachable": ["node-c"],
                "unsupported": ["node-d"]
            }),
        })
        .await;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        node_stats(&client, false, false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/nodes HTTP/1.1",
                "GET /api/v1/metrics/nodes HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE    TIME                  CPU    MEMORY         DISK READ  DISK WRITE  NET RX    NET TX  DATA FS\n\
             node-a  2025-11-06T00:00:05Z  12.3%  1.0GiB/4.0GiB  1.5KiB/s   -           2.0KiB/s  0B/s    10.0GiB/100.0GiB\n\
             node-b  -                     -      -              -          -           -         -       -\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no metrics from: node-c\n\
             note: these nodes do not report metrics yet: node-d\n"
        );

        let (mut out, mut err) = (Vec::new(), Vec::new());
        node_stats(&client, false, true, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap()[2],
            "GET /api/v1/metrics/nodes HTTP/1.1"
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["nodes"][0]["samples"].as_array().unwrap().len(), 2);
        assert!(err.is_empty());
    }

    /// `node stats --watch`의 다음 읽기는 모든 노드의 마지막 점 가운데 가장 이른 `t`를 `since`로 보내고,
    /// 새 점이 없는 노드는 전에 받은 마지막 점을 보인다.
    #[tokio::test]
    async fn node_stats_watch_asks_only_for_new_samples() {
        let (client, seen) = fake_api_status(|path| {
            let body = if path.contains("since=") {
                json!({
                    "nodes": [
                        { "node": "id-a", "samples": [] },
                        { "node": "id-b", "samples": [node_sample(1_762_387_215_000, 3.0)] }
                    ],
                    "unreachable": [], "unsupported": []
                })
            } else {
                json!({
                    "nodes": [
                        { "node": "id-a", "samples": [node_sample(1_762_387_205_000, 1.0)] },
                        { "node": "id-b", "samples": [node_sample(1_762_387_210_000, 2.0)] }
                    ],
                    "unreachable": [], "unsupported": []
                })
            };
            (200, body)
        })
        .await;
        let mut state = NodeStatsState::default();
        state.names.insert("id-a".into(), "node-a".into());
        let mut out = Vec::new();
        node_stats_round(&client, &mut state, true, false, &mut out, &mut Vec::new())
            .await
            .unwrap();
        let mut out = Vec::new();
        node_stats_round(&client, &mut state, true, false, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/metrics/nodes HTTP/1.1",
                "GET /api/v1/metrics/nodes?since=1762387205000 HTTP/1.1"
            ]
        );
        let out = String::from_utf8(out).unwrap();
        let rows: Vec<&str> = out.lines().skip(1).collect();
        // 이름을 모르는 노드는 id로 보인다.
        assert!(
            rows[0].starts_with("node-a  2025-11-06T00:00:05Z  1.0%"),
            "{out}"
        );
        assert!(
            rows[1].starts_with("id-b    2025-11-06T00:00:15Z  3.0%"),
            "{out}"
        );
        assert_eq!(state.since, Some(1_762_387_205_000));
    }

    /// `vm stats`는 VM의 마지막 점을 보이고, `--watch`의 다음 읽기는 그 `t`를 `since`로 보낸다. 점이 없으면
    /// 그 까닭을 stderr에 알린다. `-o json`은 응답 그대로다.
    #[tokio::test]
    async fn vm_stats_shows_the_last_sample() {
        let (client, seen) = fake_api_status(|path| {
            let samples = if path.contains("since=") {
                json!([])
            } else {
                json!([
                    { "t": 1_762_387_200_000u64, "cpu": 1.0, "mem_rss_mib": 1, "disk_read_bps": null,
                      "disk_write_bps": null, "disk_read_iops": null, "disk_write_iops": null,
                      "net_rx_bps": null, "net_tx_bps": null },
                    { "t": 1_762_387_205_123u64, "cpu": 104.25, "mem_rss_mib": 300,
                      "disk_read_bps": 10240.0, "disk_write_bps": 1048576.0, "disk_read_iops": 2.5,
                      "disk_write_iops": 40.0, "net_rx_bps": null, "net_tx_bps": null }
                ])
            };
            let body = json!({
                "vm": "019a0000-0000-7000-8000-000000000001",
                "samples": samples,
                "unreachable": ["node-c"],
                "unsupported": []
            });
            (200, body)
        })
        .await;
        let mut state = VmStatsState::default();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        vm_stats_round(
            &client, "web 1", &mut state, true, false, &mut out, &mut err,
        )
        .await
        .unwrap();
        let table = "TIME                  CPU     MEMORY    DISK READ  DISK WRITE  READ IOPS  WRITE IOPS  NET RX  NET TX\n\
                     2025-11-06T00:00:05Z  104.2%  300.0MiB  10.0KiB/s  1.0MiB/s    2.5        40.0        -       -\n";
        assert_eq!(String::from_utf8(out).unwrap(), table);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no metrics from: node-c\n"
        );
        // 다음 읽기에 새 점이 없으면 마지막 점을 다시 보인다.
        let mut out = Vec::new();
        vm_stats_round(
            &client,
            "web 1",
            &mut state,
            true,
            false,
            &mut out,
            &mut Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), table);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/vms/web%201/metrics HTTP/1.1",
                "GET /api/v1/vms/web%201/metrics?since=1762387205123 HTTP/1.1"
            ]
        );

        // 점이 하나도 없다.
        let mut state = VmStatsState {
            since: Some(1),
            ..Default::default()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        vm_stats_round(&client, "web", &mut state, true, false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(out.iter().filter(|&&b| b == b'\n').count(), 1);
        assert!(String::from_utf8(err).unwrap().ends_with(
            "note: no metrics yet; the VM is off or started less than 10 seconds ago\n"
        ));

        let (mut out, mut err) = (Vec::new(), Vec::new());
        vm_stats(&client, "web", false, true, &mut out, &mut err)
            .await
            .unwrap();
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["samples"].as_array().unwrap().len(), 2);
        assert!(err.is_empty());
    }

    /// 지금까지 본 노드가 응답에 빠진(닿지 않음·옛 버전) 동안은 남은 노드로 `since`를 셈하고, 그 노드가
    /// 돌아온 다음 읽기 한 번만 `since`를 보내지 않는다(돌아온 노드의 시계가 늦으면 그 줄이 멈춘다).
    #[tokio::test]
    async fn node_stats_watch_drops_since_once_after_a_missing_node_returns() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            let a = json!({ "node": "id-a", "samples": [node_sample(1_762_387_210_000, 1.0)] });
            let b = json!({ "node": "id-b", "samples": [node_sample(1_762_387_205_000, 2.0)] });
            let nodes = match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                1 | 2 => json!([a]),
                _ => json!([a, b]),
            };
            (
                200,
                json!({ "nodes": nodes, "unreachable": ["node-b"], "unsupported": [] }),
            )
        })
        .await;
        let mut state = NodeStatsState::default();
        for _ in 0..5 {
            node_stats_round(
                &client,
                &mut state,
                true,
                false,
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/metrics/nodes HTTP/1.1",
                "GET /api/v1/metrics/nodes?since=1762387205000 HTTP/1.1",
                // id-b가 빠진 동안은 id-a의 마지막 `t`다.
                "GET /api/v1/metrics/nodes?since=1762387210000 HTTP/1.1",
                "GET /api/v1/metrics/nodes?since=1762387210000 HTTP/1.1",
                // id-b가 돌아온 다음 한 번만 `since`가 없다.
                "GET /api/v1/metrics/nodes HTTP/1.1",
            ]
        );
        assert_eq!(state.since, Some(1_762_387_205_000));
    }

    /// `--watch`에서 일시 오류는 stderr에 찍고 다음 회차를 계속한다(점은 그대로). 401·403·404는 끝낸다.
    /// `--watch`가 아니면 오류로 끝난다.
    #[tokio::test]
    async fn stats_watch_survives_transient_errors() {
        let (client, _) = fake_api_status(|path| {
            let status = path
                .rsplit_once("since=")
                .map_or(200, |(_, code)| code.parse().unwrap());
            let body = match status {
                200 => json!({
                    "vm": "019a0000-0000-7000-8000-000000000001",
                    "samples": [{ "t": 500u64, "cpu": 1.0 }],
                    "unreachable": [], "unsupported": []
                }),
                401 => json!({ "code": "unauthorized", "message": "authentication required" }),
                403 => json!({ "code": "forbidden", "message": "permission denied" }),
                404 => json!({ "code": "not_found", "message": "VM web not found" }),
                _ => json!({ "code": "internal", "message": "boom" }),
            };
            (status, body)
        })
        .await;
        // `since`에 상태 코드를 실어 시험 서버가 그 상태로 답하게 한다.
        let mut state = VmStatsState::default();
        vm_stats_round(
            &client,
            "web",
            &mut state,
            true,
            false,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .await
        .unwrap();
        state.since = Some(503);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        vm_stats_round(&client, "web", &mut state, true, false, &mut out, &mut err)
            .await
            .unwrap();
        assert!(out.is_empty());
        assert_eq!(String::from_utf8(err).unwrap(), "error: boom (internal)\n");
        assert_eq!(state.last.as_ref().unwrap()["t"], 500);
        assert_eq!(state.since, Some(503));
        for code in [401, 403, 404] {
            state.since = Some(code);
            let e = vm_stats_round(
                &client,
                "web",
                &mut state,
                true,
                false,
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .await
            .unwrap_err();
            assert!(ends_stats_watch(&e), "{code}: {e:#}");
        }
        state.since = Some(503);
        vm_stats_round(
            &client,
            "web",
            &mut state,
            false,
            false,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        let mut state = NodeStatsState {
            since: Some(500),
            ..Default::default()
        };
        let mut err = Vec::new();
        node_stats_round(&client, &mut state, true, false, &mut Vec::new(), &mut err)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(err).unwrap(), "error: boom (internal)\n");
    }

    /// `-o json --watch`는 회차마다 한 줄 compact JSON(JSON Lines)이다.
    #[tokio::test]
    async fn stats_json_watch_prints_one_line_per_round() {
        let (client, _) = fake_api(|_| {
            json!({ "nodes": [{ "node": "id-a", "samples": [node_sample(1_000, 1.0)] }],
                    "unreachable": [], "unsupported": [] })
        })
        .await;
        let mut state = NodeStatsState::default();
        let mut out = Vec::new();
        for _ in 0..2 {
            node_stats_round(&client, &mut state, true, true, &mut out, &mut Vec::new())
                .await
                .unwrap();
        }
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        for line in lines {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["nodes"][0]["node"], "id-a");
        }
    }

    /// `node stats`·`vm stats`는 `--watch`를 받는다.
    #[test]
    fn parses_stats_commands() {
        let cli = Cli::try_parse_from(["oplctl", "node", "stats", "--watch"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Node {
                cmd: NodeCmd::Stats(StatsArgs { watch: true })
            }
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "stats", "web"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Stats { vm, stats: StatsArgs { watch: false } } } if vm == "web"
        ));
    }

    /// `--since`의 기간(`30m`·`1h`·`7d`)은 지금에서 뺀 유닉스 밀리초로, 나머지는 그대로 보낸다.
    #[test]
    fn audit_since_turns_durations_into_milliseconds() {
        let now = 1_762_400_000_000;
        assert_eq!(audit_since("30m", now), (now - 30 * 60_000).to_string());
        assert_eq!(audit_since("1h", now), (now - 3_600_000).to_string());
        assert_eq!(audit_since("7d", now), (now - 7 * 86_400_000).to_string());
        assert_eq!(audit_since("90s", now), (now - 90_000).to_string());
        assert_eq!(
            audit_since("2026-11-06T00:00:00Z", now),
            "2026-11-06T00:00:00Z"
        );
        assert_eq!(audit_since("1762400000000", now), "1762400000000");
        assert_eq!(audit_since("xd", now), "xd");
    }

    /// `audit`는 거르기를 쿼리로 보내고 표를 보인다. 읽지 못한 노드와 잘림은 stderr에 알린다.
    /// `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
    #[tokio::test]
    async fn audit_shows_a_table_and_warnings() {
        let (client, seen) = fake_api(|_| {
            json!({
                "entries": [
                    { "id": "019a0000-0000-7000-8000-000000000002", "time": 1_762_387_205_123u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "token", "ip": "10.0.0.5", "method": "POST",
                      "path": "/vms/web-1/actions/start", "action": "POST /vms/{key}/actions/{action}",
                      "target": { "kind": "vms", "key": "web-1" }, "status": 202,
                      "duration_ms": 12, "detail": null },
                    { "id": "019a0000-0000-7000-8000-000000000001", "time": 1_762_387_200_000u64,
                      "node": "node-b", "user": null, "user_id": null, "role": null,
                      "auth": "none", "ip": "10.0.0.9", "method": "POST", "path": "/auth/login",
                      "action": "POST /auth/login", "target": null, "status": 401,
                      "duration_ms": 3, "detail": "username=bob" },
                    { "id": "019a0000-0000-7000-8000-000000000000", "time": 1_762_387_100_000u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "session", "ip": "10.0.0.5", "method": "POST", "path": "/auth/logout",
                      "action": "POST /auth/logout", "target": null, "status": 204,
                      "duration_ms": 1, "detail": null }
                ],
                "unreachable": ["node-c", "node-d"],
                "disabled": ["node-e"],
                "truncated": true
            })
        })
        .await;
        let args = [
            "oplctl",
            "audit",
            "--since",
            "2026-11-06T00:00:00Z",
            "--until",
            "1762400000000",
            "--user",
            "alice",
            "--node",
            "node-a",
            "--action",
            "start",
            "--limit",
            "5",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, false, &mut out, &mut err).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/audit?since=2026-11-06T00%3A00%3A00Z&until=1762400000000\
              &user=alice&node=node-a&action=start&limit=5 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "TIME                  NODE    USER   IP        ACTION                            TARGET        STATUS\n\
             2025-11-06T00:00:05Z  node-a  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202\n\
             2025-11-06T00:00:00Z  node-b  -      10.0.0.9  POST /auth/login                  username=bob  401\n\
             2025-11-05T23:58:20Z  node-a  alice  10.0.0.5  POST /auth/logout                 -             204\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no audit records from: node-c, node-d\n\
             note: the audit log is disabled on: node-e\n\
             note: more entries match; narrow the range or raise --limit\n"
        );

        // 거르기가 없으면 쿼리도 없다(서버 기본값: 지난 24시간, 200개).
        let Cmd::Audit(a) = Cli::try_parse_from(["oplctl", "audit"]).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, true, &mut out, &mut err).await.unwrap();
        assert_eq!(seen.lock().unwrap()[1], "GET /api/v1/audit HTTP/1.1");
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["unreachable"], json!(["node-c", "node-d"]));
        assert_eq!(shown["entries"][1]["detail"], "username=bob");
        assert!(err.is_empty());

        // 기간 --since는 --until과 함께 줄 수 없다(서버에 보내지 않는다).
        let args = [
            "oplctl",
            "audit",
            "--since",
            "1h",
            "--until",
            "1762400000000",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let e = audit(&client, a, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "a relative --since cannot be combined with --until; give a time"
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// `fake_api_bodies`가 남긴 요청
    #[derive(Debug)]
    struct SeenRequest {
        line: String,
        if_match: Option<String>,
        body: Value,
    }

    /// `fake_api`와 같고 요청의 `if-match`와 JSON 본문도 남긴다. 응답에는 세션 쿠키를 붙인다.
    async fn fake_api_bodies(
        reply: fn(&str) -> Value,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<SeenRequest>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = vec![0; 4096];
                // 머리와 content-length만큼의 본문을 다 읽는다.
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "connection closed mid-request");
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let length = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break (head.to_string(), body.to_string());
                    }
                };
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let if_match = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("if-match")
                        .then(|| v.trim().to_string())
                });
                log.lock().unwrap().push(SeenRequest {
                    line,
                    if_match,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let body = reply(&path).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nset-cookie: opl_session=s1; Path=/\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        (base, seen)
    }

    fn realm_cmd(args: &[&str]) -> Result<RealmCmd, clap::Error> {
        let mut all = vec!["oplctl", "realm"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Realm { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `realm add-ldap`은 url·base DN이 필요하고, `--group`은 마지막 `=`에서 나눈다.
    #[test]
    fn parses_realm_commands() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--group",
            "cn=admins,dc=example,dc=com=admin",
            "--group",
            "cn=staff,dc=example,dc=com=Viewer",
        ])
        .unwrap() else {
            panic!("add-ldap")
        };
        assert_eq!(add.ldap.groups.len(), 2);
        assert_eq!(add.ldap.groups[0].0, "cn=admins,dc=example,dc=com");
        assert!(matches!(add.ldap.groups[0].1, Role::Admin));
        assert!(matches!(add.ldap.groups[1].1, Role::Viewer));

        assert_eq!(
            parse_group_role("cn=a,dc=x=root").unwrap_err(),
            "invalid role \"root\" in \"cn=a,dc=x=root\": use admin, operator or viewer"
        );
        assert!(parse_group_role("admins").is_err());
        assert!(parse_group_role("=admin").is_err());

        for args in [
            // url·base DN이 없다.
            &["add-ldap", "corp", "--url", "ldaps://h"][..],
            &["add-ldap", "corp", "--base-dn", "dc=x"][..],
            // 비밀번호를 주면서 지울 수는 없다.
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-password-file",
                "f",
            ][..],
            // 지우기와 설정은 함께 줄 수 없다.
            &["set-ldap", "corp", "--clear-bind-dn", "--bind-dn", "cn=x"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-dn",
                "--bind-password-file",
                "f",
            ][..],
            &["set-ldap", "corp", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &[
                "set-ldap",
                "corp",
                "--clear-groups",
                "--group",
                "cn=x=admin",
            ][..],
            // 비밀번호 파일은 사용자와 함께
            &["test", "corp", "--password-file", "f"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "corp", "--user", "alice"]).unwrap()
        else {
            panic!("test")
        };
        assert_eq!(user.as_deref(), Some("alice"));
    }

    /// `add-ldap`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_body_sends_only_what_is_given() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldap://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_body(&add, LdapFiles::default()),
            json!({
                "name": "corp",
                "backend": { "kind": "ldap", "url": "ldap://ldap.example.com", "base_dn": "dc=example,dc=com" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--bind-dn",
            "cn=svc,dc=example,dc=com",
            "--user-filter",
            "(sAMAccountName={username})",
            "--group-attr",
            "isMemberOf",
            "--timeout",
            "5",
            "--display",
            "Corp",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("svc-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_body(&add, files),
            json!({
                "name": "corp",
                "display": "Corp",
                "backend": {
                    "kind": "ldap", "url": "ldaps://ldap.example.com", "base_dn": "dc=example,dc=com",
                    "bind_dn": "cn=svc,dc=example,dc=com", "bind_password": "svc-pw",
                    "user_filter": "(sAMAccountName={username})", "group_attr": "isMemberOf",
                    "ca_pem": "PEM", "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000001", "name": "corp", "revision": 4,
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 10, "has_bind_password": true
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-ldap`은 지금 영역에 준 것만 바꾼다. 비밀번호는 주지 않으면 빼고(서버가 둔다),
    /// `--clear-bind-password`면 빈 글이다. `--group`을 주면 표 전체를 바꾼다.
    #[test]
    fn realm_set_body_changes_only_what_is_given() {
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        let kept = json!({
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 3
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        });
        assert_eq!(
            realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap(),
            kept
        );

        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--url",
            "ldap://other.example.com",
            "--clear-bind-dn",
            "--clear-ca",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "admin",
            "--display",
            "Corporate",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        let body = realm_set_body(&with_ca, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["display"], "Corporate");
        assert_eq!(body["backend"]["url"], "ldap://other.example.com");
        // --clear-bind-dn은 저장된 비밀번호도 지운다.
        assert_eq!(body["backend"]["bind_password"], "");
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
        assert!(body["backend"].get("ca_pem").is_none(), "{body}");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                "default": "admin"
            })
        );

        // 새 비밀번호와 CA
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("new-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_body(&sample_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["bind_password"], "new-pw");
        assert_eq!(body["backend"]["ca_pem"], "PEM");

        // 그룹 표와 기본 역할 지우기
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-groups", "--clear-default-role"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert_eq!(body["roles"], json!({ "groups": [] }));

        // 서비스 계정 DN이 남는데 비밀번호만 지울 수는 없다(새 DN을 줘도).
        for args in [
            &["set-ldap", "corp", "--clear-bind-password"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-dn",
                "cn=other",
            ][..],
        ] {
            let RealmCmd::SetLdap(set) = realm_cmd(args).unwrap() else {
                unreachable!()
            };
            let e = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap_err();
            assert_eq!(
                e.to_string(),
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
        // DN이 없는 영역이나 DN도 지우면 된다.
        let mut anon = sample_realm();
        anon["backend"].as_object_mut().unwrap().remove("bind_dn");
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-bind-password"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&anon, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["backend"]["bind_password"], "");
        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--clear-bind-password",
            "--clear-bind-dn",
        ])
        .unwrap() else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
    }

    /// url·bind_dn·CA를 바꾸면서 비밀번호를 주지 않으면 서버의 400을 그대로 보인다.
    #[tokio::test]
    async fn realm_set_shows_the_server_refusal() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                (200, sample_realm())
            } else {
                (
                    400,
                    json!({
                        "code": "invalid_request",
                        "message": "bind_password must be given again when url, bind_dn or ca_pem changes"
                    }),
                )
            }
        })
        .await;
        let e = realm(
            &client,
            realm_cmd(&["set-ldap", "corp", "--url", "ldaps://other.example.com"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("bind_password must be given again when url, bind_dn or ca_pem changes"),
            "{e}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1"
            ]
        );
    }

    #[test]
    fn realm_show_lines() {
        assert_eq!(
            realm_lines(&sample_realm()).join("\n"),
            "name: corp\n\
             display: Corp\n\
             kind: ldap\n\
             url: ldaps://ldap.example.com\n\
             base dn: dc=example,dc=com\n\
             bind dn: cn=svc,dc=example,dc=com\n\
             bind password: set\n\
             user filter: (uid={username})\n\
             group attribute: memberOf\n\
             ca: system roots\n\
             timeout: 10s\n\
             groups:\n  cn=admins,dc=example,dc=com = admin\n\
             default role: viewer"
        );
        let mut bare = sample_realm();
        bare["backend"]["has_bind_password"] = json!(false);
        bare["backend"]["ca_pem"] = json!("PEM");
        bare["roles"] = json!({ "groups": [] });
        let lines = realm_lines(&bare);
        for line in [
            "bind password: none",
            "ca: custom (PEM)",
            "groups: none",
            "default role: none",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// 영역 명령이 부르는 API: 파일의 비밀번호·CA는 본문에, `set-ldap`은 읽은 revision을
    /// If-Match로, `test`는 단계 표를 보이고 실패한 단계가 있으면 오류다.
    #[tokio::test]
    async fn realm_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_realm()] }),
            "/api/v1/realms/corp/actions/test" => json!({
                "steps": [
                    { "name": "connect", "ok": true, "message": "connected to ldaps://ldap.example.com" },
                    { "name": "service_bind", "ok": true, "message": "bound as cn=svc,dc=example,dc=com" },
                    { "name": "search", "ok": true, "message": "found uid=alice,dc=example,dc=com" },
                    { "name": "user_bind", "ok": true, "message": "the password is correct" },
                    { "name": "groups", "ok": true, "message": "2 groups" }
                ],
                "dn": "uid=alice,dc=example,dc=com",
                "groups": ["cn=admins,dc=example,dc=com", "cn=staff,dc=example,dc=com"],
                "role": "admin"
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "connect", "ok": false, "message": "connection refused" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("pw");
        std::fs::write(&secret, "svc-pw\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-ldap",
                "corp",
                "--url",
                "ldaps://ldap.example.com",
                "--base-dn",
                "dc=example,dc=com",
                "--bind-dn",
                "cn=svc,dc=example,dc=com",
                "--bind-password-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-ldap", "corp", "--bind-password-file", secret][..],
            &["remove", "corp"][..],
            &["test", "corp", "--user", "alice", "--password-file", secret][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken", "--user", "a", "--password-file", secret]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the connect step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1",
                "DELETE /api/v1/realms/corp HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "POST /api/v1/realms/corp/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["bind_password"], "svc-pw");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("4"));
        assert_eq!(seen[3].body["backend"]["bind_password"], "svc-pw");
        assert!(seen[3].body["backend"].get("has_bind_password").is_none());
        assert_eq!(
            seen[6].body,
            json!({ "username": "alice", "password": "svc-pw" })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             corp  Corp     ldap  ldaps://ldap.example.com\n\
             realm corp created\n\
             realm corp updated\n\
             realm corp removed\n\
             STEP          RESULT  MESSAGE\n\
             connect       ok      connected to ldaps://ldap.example.com\n\
             service_bind  ok      bound as cn=svc,dc=example,dc=com\n\
             search        ok      found uid=alice,dc=example,dc=com\n\
             user_bind     ok      the password is correct\n\
             groups        ok      2 groups\n\
             dn: uid=alice,dc=example,dc=com\n\
             groups: cn=admins,dc=example,dc=com, cn=staff,dc=example,dc=com\n\
             role: admin\n"
        );
    }

    /// `login --realm`은 로그인 본문에 영역을 넣고(local이면 넣지 않음) 서버가 정한 이름을 돌려준다.
    #[tokio::test]
    async fn login_sends_the_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/login" => json!({ "name": "alice@corp", "role": "admin" }),
            "/api/v1/tokens" => json!({ "secret": "opl_t_s" }),
            _ => json!({}),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "alice-pw\n").unwrap();
        let pw = pw.to_str().unwrap();
        for (realm, expected) in [(Some("corp"), Some("corp")), (None, None)] {
            let mut args = vec!["oplctl", "login", "--user", "Alice", "--password-file", pw];
            if let Some(realm) = realm {
                args.extend(["--realm", realm]);
            }
            let Cmd::Login(args) = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            assert_eq!(
                login(&base, &args, None).await.unwrap(),
                ("opl_t_s".to_string(), "alice@corp".to_string())
            );
            let seen = seen.lock().unwrap();
            let sent = &seen[seen.len() - 3];
            assert_eq!(sent.line, "POST /api/v1/auth/login HTTP/1.1");
            assert_eq!(sent.body["username"], "Alice");
            assert_eq!(sent.body["password"], "alice-pw");
            assert_eq!(sent.body.get("realm").and_then(Value::as_str), expected);
        }
    }

    /// `realm add-oidc`는 issuer·client ID가 필요하고, 지우기와 설정은 함께 줄 수 없다.
    /// `test`는 사용자 없이도 된다(OIDC).
    #[test]
    fn parses_oidc_realm_commands() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--group",
            "opl-admins=admin",
        ])
        .unwrap() else {
            panic!("add-oidc")
        };
        assert_eq!(add.oidc.scopes, ["openid", "groups"]);
        assert_eq!(add.oidc.groups[0].0, "opl-admins");
        assert!(matches!(add.oidc.groups[0].1, Role::Admin));

        for args in [
            &["add-oidc", "okta", "--issuer", "https://idp"][..],
            &["add-oidc", "okta", "--client-id", "oplite"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-client-secret",
                "--client-secret-file",
                "f",
            ][..],
            &["set-oidc", "okta", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &["set-oidc", "okta", "--clear-groups", "--group", "g=admin"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "okta"]).unwrap() else {
            panic!("test")
        };
        assert_eq!(user, None);
    }

    /// `add-oidc`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_oidc_body_sends_only_what_is_given() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_oidc_body(&add, OidcFiles::default()),
            json!({
                "name": "okta",
                "backend": { "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com/oauth2/default",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--timeout",
            "5",
            "--display",
            "Okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("s3cret".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_oidc_body(&add, files),
            json!({
                "name": "okta",
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com/oauth2/default",
                    "client_id": "oplite", "client_secret": "s3cret", "scopes": ["openid", "groups"],
                    "username_claim": "email", "groups_claim": "roles", "ca_pem": "PEM",
                    "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "opl-ops", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_oidc_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000002", "name": "okta", "revision": 9,
            "display": "Okta",
            "backend": {
                "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                "groups_claim": "groups", "timeout_secs": 10, "has_client_secret": true
            },
            "roles": {
                "groups": [{ "group": "opl-admins", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-oidc`는 지금 영역에 준 것만 바꾼다. 비밀은 주지 않으면 빼고(서버가 둔다),
    /// `--clear-client-secret`면 빈 글이다. 다른 종류의 영역은 바꾸지 않는다.
    #[test]
    fn realm_set_oidc_body_changes_only_what_is_given() {
        let RealmCmd::SetOidc(set) = realm_cmd(&["set-oidc", "okta", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            realm_set_oidc_body(&sample_oidc_realm(), &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                    "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                    "groups_claim": "groups", "timeout_secs": 3
                },
                "roles": {
                    "groups": [{ "group": "opl-admins", "role": "admin" }],
                    "default": "viewer"
                }
            })
        );

        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--issuer",
            "https://other.example.com",
            "--client-id",
            "opl2",
            "--scope",
            "openid",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--clear-client-secret",
            "--clear-ca",
            "--clear-groups",
            "--clear-default-role",
            "--display",
            "Other",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_oidc_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        assert_eq!(
            realm_set_oidc_body(&with_ca, &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Other",
                "backend": {
                    "kind": "oidc", "issuer": "https://other.example.com", "client_id": "opl2",
                    "client_secret": "", "scopes": ["openid"], "username_claim": "email",
                    "groups_claim": "roles", "timeout_secs": 10
                },
                "roles": { "groups": [] }
            })
        );

        // 새 비밀·CA와 그룹 표
        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "admin",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("new-secret".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_oidc_body(&sample_oidc_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["client_secret"], "new-secret");
        assert_eq!(body["backend"]["ca_pem"], "PEM");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "opl-ops", "role": "operator" }],
                "default": "admin"
            })
        );

        // 종류가 다른 영역
        let e = realm_set_oidc_body(&sample_realm(), &set, OidcFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm okta is not an OIDC realm (its kind is ldap)"
        );
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let e = realm_set_body(&sample_oidc_realm(), &set, LdapFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm corp is not an LDAP realm (its kind is oidc)"
        );
    }

    #[test]
    fn realm_show_lines_for_oidc() {
        assert_eq!(
            realm_lines(&sample_oidc_realm()).join("\n"),
            "name: okta\n\
             display: Okta\n\
             kind: oidc\n\
             issuer: https://idp.example.com\n\
             client id: oplite\n\
             client secret: set\n\
             scopes: openid profile email\n\
             username claim: preferred_username\n\
             groups claim: groups\n\
             ca: system roots\n\
             timeout: 10s\n\
             callback path: /api/v1/auth/oidc/callback\n\
             groups:\n  opl-admins = admin\n\
             default role: viewer"
        );
        let mut public = sample_oidc_realm();
        public["backend"]["has_client_secret"] = json!(false);
        public["backend"]["ca_pem"] = json!("PEM");
        let lines = realm_lines(&public);
        for line in ["client secret: none", "ca: custom (PEM)"] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// OIDC 영역 명령이 부르는 API: 목록의 URL 칸은 issuer, 파일의 비밀·CA는 본문에,
    /// `set-oidc`는 읽은 revision을 If-Match로, `test`는 영역 종류를 읽고 입력 없이 단계 표만 보인다
    /// (`--user`를 줘도 비밀번호를 묻지 않고 DN·그룹·역할 줄이 없다).
    #[tokio::test]
    async fn realm_oidc_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_oidc_realm()] }),
            "/api/v1/realms/okta/actions/test" => json!({
                "steps": [
                    { "name": "discovery", "ok": true, "message": "read https://idp.example.com/.well-known/openid-configuration" },
                    { "name": "jwks", "ok": true, "message": "2 signing keys (RS256 1, ES256 1)" }
                ],
                "dn": null, "groups": [], "role": null
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "discovery", "ok": false, "message": "issuer differs" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_oidc_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-oidc",
                "okta",
                "--issuer",
                "https://idp.example.com",
                "--client-id",
                "oplite",
                "--client-secret-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-oidc", "okta", "--client-secret-file", secret][..],
            &["test", "okta", "--user", "alice"][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the discovery step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "PUT /api/v1/realms/okta HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "POST /api/v1/realms/okta/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["kind"], "oidc");
        assert_eq!(seen[1].body["backend"]["client_secret"], "s3cret");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("9"));
        assert_eq!(seen[3].body["backend"]["client_secret"], "s3cret");
        assert!(seen[3].body["backend"].get("has_client_secret").is_none());
        assert_eq!(seen[5].body, json!({}));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             okta  Okta     oidc  https://idp.example.com\n\
             realm okta created\n\
             realm okta updated\n\
             STEP       RESULT  MESSAGE\n\
             discovery  ok      read https://idp.example.com/.well-known/openid-configuration\n\
             jwks       ok      2 signing keys (RS256 1, ES256 1)\n"
        );
    }

    /// `login --realm <OIDC 영역>`은 비밀번호를 묻기 전에 웹 UI로 보낸다.
    #[tokio::test]
    async fn login_refuses_an_oidc_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/realms" => json!([
                { "name": "local", "display": null, "kind": "local" },
                { "name": "corp", "display": null, "kind": "ldap" },
                { "name": "okta", "display": "Okta", "kind": "oidc" }
            ]),
            _ => json!({}),
        })
        .await;
        // 비밀번호 파일이 없으므로 묻는다면 터미널이 없어 다른 오류가 난다.
        let Cmd::Login(args) = Cli::try_parse_from(["oplctl", "login", "--realm", "okta"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let e = login(&base, &args, None).await.unwrap_err();
        assert_eq!(
            e.to_string(),
            "sign in to okta in the web UI, create an API token there and run `orbivisorctl login --token-file`"
        );
        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["GET /api/v1/auth/realms HTTP/1.1"]);
    }

    /// `login --token-file`은 비밀번호 로그인 인자와 함께 줄 수 없다.
    #[test]
    fn token_file_login_conflicts_with_password_login() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["oplctl", "login", "--token-file", "t"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args).map(|_| ())
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--ca-fingerprint", "sha256:ab"]).is_ok());
        for extra in [
            &["--user", "bob"][..],
            &["--realm", "okta"][..],
            &["--password-file", "pw"][..],
        ] {
            let e = parse(extra).unwrap_err();
            assert_eq!(
                e.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{extra:?}"
            );
        }
    }

    /// `login --token-file`: 토큰으로 whoami를 불러 확인하고 저장한다. 같은 토큰을 다시 넣으면 지우지 않고,
    /// 이전 토큰은 login이 만든 것(`oplctl-…`)만 지운다. 받지 않는 토큰은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_checks_and_saves_the_token() {
        let ids = [
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
        ];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[2] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[2] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/auth/whoami" => (
                200,
                json!({ "name": "alice@corp.com@okta", "role": "admin" }),
            ),
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        let login_with = async |token: &str| {
            std::fs::write(&file, format!("{token}\n")).unwrap();
            let mut out = Vec::new();
            let args = [
                "oplctl",
                "--config",
                config.to_str().unwrap(),
                "--server",
                client.base.as_str(),
                "login",
                "--token-file",
                file.to_str().unwrap(),
            ];
            run(args, &mut out)
                .await
                .map(|()| String::from_utf8(out).unwrap())
        };
        let saved = || CliConfig::load(&config).unwrap().token;
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)
        assert_eq!(
            login_with(&tokens[0]).await.unwrap(),
            format!("logged in to {} as alice@corp.com@okta\n", client.base)
        );
        assert_eq!(saved().as_deref(), Some(tokens[0].as_str()));
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 같은 토큰을 다시 넣어도 지우지 않는다.
        login_with(&tokens[0]).await.unwrap();
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 다른 토큰: 이전 토큰(laptop)은 login이 만든 것이 아니므로 두다.
        login_with(&tokens[1]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1",
                "GET /api/v1/tokens HTTP/1.1"
            ]
        );
        // 이전 토큰(oplctl-…)은 지운다.
        login_with(&tokens[2]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1".to_string(),
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
        assert_eq!(saved().as_deref(), Some(tokens[2].as_str()));
    }

    /// 서버가 받지 않는 토큰(401)은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_does_not_save_a_refused_token() {
        let (client, seen) = fake_api_status(|_| {
            (
                401,
                json!({ "code": "unauthorized", "message": "invalid token" }),
            )
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        std::fs::write(&file, "opl_bad_token\n").unwrap();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "login",
            "--token-file",
            file.to_str().unwrap(),
        ];
        let e = run(args, &mut Vec::new()).await.unwrap_err();
        assert_eq!(e.to_string(), "the API token was not accepted");
        assert!(!config.exists());
        assert_eq!(*seen.lock().unwrap(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
    }

    /// 시험 서버 쪽에서 경로 조각의 퍼센트 인코딩을 푼다.
    fn percent_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    /// `vm stop --force`는 동작 뒤의 쿼리를 그대로 보낸다(VM 이름만 인코딩).
    #[tokio::test]
    async fn vm_stop_force_keeps_the_query() {
        let (client, seen) =
            fake_api(|_| json!({ "id": "t1", "status": { "state": "succeeded" } })).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "vm",
            "stop",
            "web 1",
            "--force",
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "vm web 1: stop done\n");
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines[0],
            "POST /api/v1/vms/web%201/actions/stop?mode=force HTTP/1.1"
        );
    }

    /// 이름을 경로에 넣을 때 퍼센트 인코딩해, `+`·`@`가 든 OIDC 사용자 이름도 서버에서 같은 이름으로 풀린다.
    #[tokio::test]
    async fn user_delete_encodes_the_name_in_the_path() {
        let (client, seen) = fake_api(|_| json!({})).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let name = "alice+ops@corp.com@okta";
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "user",
            "delete",
            name,
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("user {name} deleted\n")
        );
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines,
            ["DELETE /api/v1/users/alice%2Bops%40corp.com%40okta HTTP/1.1"]
        );
        let path = lines[0].split(' ').nth(1).unwrap();
        assert_eq!(
            percent_decode(path.strip_prefix("/api/v1/users/").unwrap()),
            name
        );
    }

    /// `logout`은 login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 웹 UI에서 만든 토큰은 로컬 설정에서만
    /// 지우고 서버에서는 그대로라고 알린다.
    #[tokio::test]
    async fn logout_deletes_only_tokens_made_by_login() {
        let ids = [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[1] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[1] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let logout_with = async |token: &str| {
            CliConfig {
                server: Some(client.base.clone()),
                token: Some(token.into()),
                ca_pem: None,
            }
            .save(&config)
            .unwrap();
            let mut out = Vec::new();
            let args = ["oplctl", "--config", config.to_str().unwrap(), "logout"];
            run(args, &mut out).await.unwrap();
            assert_eq!(CliConfig::load(&config).unwrap().token, None);
            String::from_utf8(out).unwrap()
        };
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)은 서버에서 지우지 않는다.
        assert_eq!(
            logout_with(&tokens[0]).await,
            "logged out\nthe token stays valid on the server; delete it in the web UI\n"
        );
        assert_eq!(lines(), ["GET /api/v1/tokens HTTP/1.1"]);
        // login이 만든 토큰은 지운다.
        assert_eq!(logout_with(&tokens[1]).await, "logged out\n");
        assert_eq!(
            lines(),
            [
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    fn remote_cmd(args: &[&str]) -> Result<RemoteCmd, clap::Error> {
        let mut all = vec!["oplctl", "remote"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Remote { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `remote add`는 주소와 토큰 파일이 필요하다. `remote vm`의 stop은 로컬 `vm stop`처럼 ACPI이고
    /// `--force`면 곧바로 끈다.
    #[test]
    fn parses_remote_commands() {
        let RemoteCmd::Add(add) = remote_cmd(&[
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
            "--yes",
        ])
        .unwrap() else {
            panic!("not add");
        };
        assert_eq!(add.url, "https://10.0.0.5:8443");
        assert_eq!(add.token_file, PathBuf::from("-"));
        assert!(add.yes && add.ca_fingerprint.is_none());
        assert!(remote_cmd(&["add", "dr", "--url", "https://h"]).is_err());
        assert!(remote_cmd(&["add", "dr", "--token-file", "t"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "reboot", "web"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "shutdown", "web"]).is_err());
        let actions: Vec<_> = [
            &["start"][..],
            &["stop"][..],
            &["stop", "--force"][..],
            &["start", "--force"][..],
        ]
        .iter()
        .map(|a| {
            let mut args = vec!["vm", "dr"];
            args.extend_from_slice(&a[..1]);
            args.push("web");
            args.extend_from_slice(&a[1..]);
            match remote_cmd(&args).unwrap() {
                RemoteCmd::Vm { action, force, .. } => {
                    remote_vm_action(action, force).map_err(|e| e.to_string())
                }
                _ => unreachable!(),
            }
        })
        .collect();
        assert_eq!(
            actions,
            [
                Ok(("start", "start")),
                Ok(("stop", "stop")),
                Ok(("stop?mode=force", "force stop")),
                Err("--force is only for stop".to_string()),
            ]
        );
    }

    /// 토큰을 표준 입력으로 받으면 지문 확인을 물을 수 없으므로 `--ca-fingerprint`나 `--yes`가 없으면 곧바로
    /// 실패한다(표준 입력을 읽거나 원격에 연결하지 않고).
    #[tokio::test]
    async fn remote_add_from_stdin_needs_a_fingerprint_or_yes() {
        let client = Client::new("http://127.0.0.1:1", None, None).unwrap();
        let args = [
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
        ];
        let err = remote(&client, remote_cmd(&args).unwrap(), false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "reading the token from stdin needs --ca-fingerprint or --yes"
        );
    }

    /// 원격이 저장한 토큰을 거절하면 중계가 502 `remote_unauthorized`를 주므로 그 메시지를 보이고, 이
    /// 클러스터에 다시 로그인하라고 하지 않는다.
    #[tokio::test]
    async fn a_refused_remote_token_is_not_a_login_problem() {
        let message = "the remote cluster refused the stored token; update it with `orbivisorctl remote set --token-file`";
        let (client, _) = fake_api_status(move |_| {
            (
                502,
                json!({ "code": "remote_unauthorized", "message": message }),
            )
        })
        .await;
        let err = remote(
            &client,
            remote_cmd(&["vms", "dr"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(err, format!("{message} (remote_unauthorized)"));
        assert!(!err.contains("orbivisorctl login"), "{err}");
    }

    /// 받은 CA 지문은 보이고, `--yes`가 아니면 터미널에서 묻는다. 터미널이 아니면 지문을 달라고 한다.
    #[test]
    fn remote_fingerprint_needs_a_confirmation() {
        let fp = "sha256:ab";
        let confirm = |yes: bool, terminal: bool, answer: &str| {
            let mut out = Vec::new();
            let r = confirm_fingerprint(fp, yes, terminal, &mut answer.as_bytes(), &mut out);
            (
                r.map_err(|e| e.to_string()),
                String::from_utf8(out).unwrap(),
            )
        };
        let shown = "the remote cluster CA fingerprint is sha256:ab\n";
        assert_eq!(confirm(true, false, ""), (Ok(()), shown.to_string()));
        let (r, out) = confirm(false, false, "y\n");
        assert_eq!(out, shown);
        assert!(
            r.unwrap_err()
                .contains("run again with --ca-fingerprint sha256:ab"),
            "not a terminal"
        );
        let prompt = format!("{shown}Is this the CA of the remote cluster? [y/N] ");
        assert_eq!(confirm(false, true, "Yes\n"), (Ok(()), prompt.clone()));
        assert_eq!(
            confirm(false, true, "\n"),
            (Err("cancelled".to_string()), prompt)
        );
    }

    fn sample_remote() -> Value {
        json!({
            "id": "0190b9d4-0000-7000-8000-000000000001",
            "name": "dr",
            "revision": 9,
            "url": "https://10.0.0.5:8443",
            "ca_fingerprint": "sha256:aa",
            "token_id": "0190b9d4-0000-7000-8000-000000000002",
            "status": {
                "reachable": true,
                "checked_at": 1_793_000_000,
                "error": null,
                "cluster_name": "dr-site",
                "version": "0.37.0",
                "nodes": { "total": 3, "online": 2 },
                "vms": { "total": 5, "running": 4 },
                "cpus": 48,
                "memory_gib": 192
            }
        })
    }

    /// 목록의 노드·VM 칸은 `online/total`·`running/total`이고, 닿지 않는 원격은 이유를 보인다.
    #[test]
    fn remote_rows_and_lines() {
        assert_eq!(
            remote_row(&sample_remote()),
            ["dr", "dr-site", "0.37.0", "2/3", "4/5", "reachable"]
        );
        let down = json!({
            "name": "old",
            "status": { "reachable": false, "error": "connection refused" }
        });
        assert_eq!(
            remote_row(&down),
            ["old", "-", "-", "-", "-", "unreachable: connection refused"]
        );
        let lines = remote_lines(&sample_remote());
        for line in [
            "url: https://10.0.0.5:8443",
            "ca fingerprint: sha256:aa",
            "token id: 0190b9d4-0000-7000-8000-000000000002",
            "status: reachable",
            "nodes online: 2/3",
            "vms running: 4/5",
            "cpus: 48",
            "memory: 192 GiB",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
        assert!(
            remote_lines(&down).contains(&"changed at: never".to_string()),
            "{:?}",
            remote_lines(&down)
        );
    }

    /// 원격 명령이 부르는 API: `add`는 토큰 파일과 준 지문을, `set`은 읽은 revision을 If-Match로 하고
    /// 토큰을 주지 않으면 빼서 보낸다. `vms`·`vm`은 중계 경로로 원격 API를 부른다.
    #[tokio::test]
    async fn remote_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes/dr/api/vms" => json!({
                "revision": 3,
                "items": [{
                    "name": "web",
                    "spec": { "power": "running", "ha": false },
                    "status": { "phase": "running", "node": "n1", "pid": 42 }
                }]
            }),
            "/api/v1/remotes/dr/api/vms/web/actions/stop?mode=force" => {
                json!({ "id": "t1", "status": { "state": "pending" } })
            }
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        let secret = format!("opl_{}_s3cret", uuid::Uuid::now_v7().simple());
        std::fs::write(&token, format!("{secret}\n")).unwrap();
        let token = token.to_str().unwrap();

        let mut out = Vec::new();
        for args in [
            &[
                "add",
                "dr",
                "--url",
                "https://10.0.0.5:8443",
                "--ca-fingerprint",
                "sha256:aa",
                "--token-file",
                token,
            ][..],
            &["set", "dr", "--url", "https://10.0.0.6:8443"][..],
            &["set", "dr", "--token-file", token][..],
            &["remove", "dr"][..],
            &["vms", "dr"][..],
            &["vm", "dr", "stop", "web", "--force"][..],
        ] {
            remote(&client, remote_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "POST /api/v1/remotes HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "DELETE /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr/api/vms HTTP/1.1",
                "POST /api/v1/remotes/dr/api/vms/web/actions/stop?mode=force HTTP/1.1",
            ]
        );
        assert_eq!(
            seen[0].body,
            json!({
                "name": "dr",
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(seen[2].if_match.as_deref(), Some("9"));
        assert_eq!(
            seen[2].body,
            json!({ "url": "https://10.0.0.6:8443", "ca_fingerprint": "sha256:aa" })
        );
        assert_eq!(
            seen[4].body,
            json!({
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "remote dr added (reachable)\n\
             remote dr updated\n\
             remote dr updated\n\
             remote dr removed\n\
             NAME  POWER    PHASE    NODE  HA     PID  ERROR\n\
             web   running  running  n1    false  42   -\n\
             vm web on remote dr: force stop requested (task t1)\n"
        );
    }

    /// `remote list`는 원격마다 한 줄, `show`·`refresh`는 `키: 값` 줄이다.
    #[tokio::test]
    async fn remote_list_and_refresh() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes" => json!({ "revision": 9, "items": [sample_remote()] }),
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        remote(&client, remote_cmd(&["list"]).unwrap(), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  CLUSTER  VERSION  NODES  VMS  STATUS\n\
             dr    dr-site  0.37.0   2/3    4/5  reachable\n"
        );
        let mut out = Vec::new();
        remote(
            &client,
            remote_cmd(&["refresh", "dr"]).unwrap(),
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            remote_lines(&sample_remote()).join("\n") + "\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/remotes HTTP/1.1",
                "POST /api/v1/remotes/dr/actions/refresh HTTP/1.1",
            ]
        );
    }
}
```

`crates/oplctl/src/client.rs`에서

```rust
impl std::error::Error for ApiError {}
```

을 다음으로 바꾼다:

```rust
impl std::error::Error for ApiError {}

/// 로그인하지 않았거나 토큰이 더 이상 맞지 않다(401 `unauthorized`). 명령이 오류 타입으로 알아본다.
#[derive(Debug)]
pub struct NotLoggedIn;

impl std::fmt::Display for NotLoggedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not logged in or the token is no longer valid; run `orbivisorctl login`")
    }
}

impl std::error::Error for NotLoggedIn {}
```

`crates/oplctl/src/client.rs`에서

```rust
        if status == StatusCode::UNAUTHORIZED && body["code"] == "unauthorized" {
            bail!("not logged in or the token is no longer valid; run `orbivisorctl login`");
        }
```

을 다음으로 바꾼다:

```rust
        if status == StatusCode::UNAUTHORIZED && body["code"] == "unauthorized" {
            return Err(NotLoggedIn.into());
        }
```

`crates/opld/src/metrics/sampler.rs` 전체:

```rust
//! 측정기. 5초마다 이 노드와 이 노드에서 도는 VM을 재어 [`MetricsStore`]에 넣는다.
//!
//! 비율은 직전 측정과의 차이를 걸린 시간(단조 시계)으로 나눈다. 첫 측정(시작, VM 시작·붙기, pid
//! 바뀜, 카운터가 줄어듦)은 기준만 잡고 점을 만들지 않는다.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use opl_cluster::Cluster;
use opl_driver::{DiskCounters, Hypervisor, RuntimeStatus};
use opl_types::metrics::{NodeSample, VmSample};
use opl_types::network::{USER_NETWORK, tap_name};
use opl_types::vm::VmSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;

use super::host::{CpuTimes, HostProbe};
use super::store::MetricsStore;

/// 측정 간격(고정)
pub const INTERVAL: Duration = Duration::from_secs(5);
/// VM 하나의 `query-blockstats`를 기다리는 시간. 넘으면 그 VM의 디스크 필드만 `null`이다.
pub const DISK_STATS_TIMEOUT: Duration = Duration::from_secs(1);
/// 한 번의 측정에서 VM들을 재는 시간. 넘은 VM은 이번 점이 없다.
pub const VM_TIMEOUT: Duration = Duration::from_secs(4);
/// 같은 VM의 실패 경고를 다시 남기는 간격
pub const WARN_EVERY: Duration = Duration::from_secs(600);

pub struct MetricsSampler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl MetricsSampler {
    /// `data_dir`은 노드 점의 파일 시스템 값을 읽는 데이터 디렉터리다.
    pub fn spawn(
        hv: Arc<dyn Hypervisor>,
        probe: Arc<dyn HostProbe>,
        cluster: Arc<Cluster>,
        data_dir: PathBuf,
        store: Arc<MetricsStore>,
    ) -> Self {
        let sampler = Sampler::new(hv, probe, cluster, data_dir, store);
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_sampler(sampler, rx)),
        }
    }

    /// 멈춘다. 저장소의 점은 그대로 둔다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_sampler(mut sampler: Sampler, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = sampler.tick() => {}
        }
    }
}

/// 노드의 직전 누적 카운터
struct NodeCounters {
    at: Instant,
    cpu: Option<CpuTimes>,
    disk: Option<(u64, u64)>,
    nic: Option<(u64, u64)>,
}

/// VM의 직전 누적 카운터. pid가 바뀌면 다시 기준을 잡는다.
#[derive(Debug, Clone, Copy)]
struct VmCounters {
    pid: u32,
    /// 프로세스·tap을 읽은 시각(이번 측정의 VM 측정 시작, `disk_stats`를 기다리기 전)
    at: Instant,
    cpu_ns: Option<u64>,
    disk: Option<DiskCounters>,
    /// `disk_stats`가 답한 시각. QMP 응답은 늦을 수 있어(최대 1초) 디스크 비율은 이 시각의 차이로 나눈다.
    disk_at: Instant,
    /// 모든 tap의 호스트 쪽 (받은, 보낸) 합
    tap: Option<(u64, u64)>,
}

/// VM 하나를 잰 것
struct VmReading {
    counters: VmCounters,
    rss_mib: Option<u64>,
    /// `query-blockstats`가 실패했으면 그 오류
    disk_error: Option<String>,
}

/// 카운터가 직전보다 줄었다(다시 기준을 잡는다).
struct Decreased;

/// 누적 값의 초당 증가. 어느 한쪽이라도 없으면 `None`이다.
fn rate(prev: Option<u64>, now: Option<u64>, secs: f64) -> Result<Option<f64>, Decreased> {
    match (prev, now) {
        (Some(prev), Some(now)) => {
            let delta = now.checked_sub(prev).ok_or(Decreased)?;
            Ok(Some(delta as f64 / secs))
        }
        _ => Ok(None),
    }
}

fn node_sample(
    prev: &NodeCounters,
    now: &NodeCounters,
    t: i64,
    memory: Option<(u64, u64)>,
    fs: Option<(u64, u64)>,
) -> Result<NodeSample, Decreased> {
    let secs = now.at.duration_since(prev.at).as_secs_f64();
    let cpu = match (prev.cpu, now.cpu) {
        (Some(prev), Some(now)) => {
            let busy = now.busy.checked_sub(prev.busy).ok_or(Decreased)?;
            let total = now.total.checked_sub(prev.total).ok_or(Decreased)?;
            (total > 0).then(|| busy as f64 * 100.0 / total as f64)
        }
        _ => None,
    };
    let first = |c: Option<(u64, u64)>| c.map(|c| c.0);
    let second = |c: Option<(u64, u64)>| c.map(|c| c.1);
    Ok(NodeSample {
        t,
        cpu,
        mem_used_mib: memory.map(|m| m.0),
        mem_total_mib: memory.map(|m| m.1),
        disk_read_bps: rate(first(prev.disk), first(now.disk), secs)?,
        disk_write_bps: rate(second(prev.disk), second(now.disk), secs)?,
        net_rx_bps: rate(first(prev.nic), first(now.nic), secs)?,
        net_tx_bps: rate(second(prev.nic), second(now.nic), secs)?,
        fs_used_mib: fs.map(|f| f.0),
        fs_total_mib: fs.map(|f| f.1),
    })
}

fn vm_sample(
    prev: &VmCounters,
    now: &VmCounters,
    t: i64,
    vcpus: Option<u32>,
    rss_mib: Option<u64>,
) -> Result<VmSample, Decreased> {
    let secs = now.at.duration_since(prev.at).as_secs_f64();
    let disk_secs = now.disk_at.duration_since(prev.disk_at).as_secs_f64();
    // 바쁜 vCPU 수 × 100. QEMU 프로세스 전체라 100을 조금 넘을 수 있고 자르지 않는다.
    let cpu = rate(prev.cpu_ns, now.cpu_ns, secs)?
        .zip(vcpus.filter(|&n| n > 0))
        .map(|(ns, n)| ns / 1e9 * 100.0 / f64::from(n));
    let disk = |f: fn(&DiskCounters) -> u64| {
        rate(
            prev.disk.as_ref().map(f),
            now.disk.as_ref().map(f),
            disk_secs,
        )
    };
    let tap =
        |f: fn(&(u64, u64)) -> u64| rate(prev.tap.as_ref().map(f), now.tap.as_ref().map(f), secs);
    Ok(VmSample {
        t,
        cpu,
        mem_rss_mib: rss_mib,
        disk_read_bps: disk(|d| d.rd_bytes)?,
        disk_write_bps: disk(|d| d.wr_bytes)?,
        disk_read_iops: disk(|d| d.rd_operations)?,
        disk_write_iops: disk(|d| d.wr_operations)?,
        // tap은 호스트 쪽 방향이다. 게스트가 받은 것은 호스트가 tap으로 보낸 것이다.
        net_rx_bps: tap(|t| t.1)?,
        net_tx_bps: tap(|t| t.0)?,
    })
}

/// 지금 벽시계(유닉스 밀리초)
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 마지막 경고에서 [`WARN_EVERY`]가 지났으면 지금을 적고 true
fn warn_due(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.duration_since(last) < WARN_EVERY) {
        return false;
    }
    *last = Some(now);
    true
}

struct Sampler {
    hv: Arc<dyn Hypervisor>,
    probe: Arc<dyn HostProbe>,
    cluster: Arc<Cluster>,
    data_dir: PathBuf,
    store: Arc<MetricsStore>,
    node: Option<NodeCounters>,
    vms: HashMap<Uuid, VmCounters>,
    /// VM마다 마지막 디스크 실패 경고 시각
    disk_warned: HashMap<Uuid, Option<Instant>>,
    list_warned: Option<Instant>,
}

impl Sampler {
    fn new(
        hv: Arc<dyn Hypervisor>,
        probe: Arc<dyn HostProbe>,
        cluster: Arc<Cluster>,
        data_dir: PathBuf,
        store: Arc<MetricsStore>,
    ) -> Self {
        Self {
            hv,
            probe,
            cluster,
            data_dir,
            store,
            node: None,
            vms: HashMap::new(),
            disk_warned: HashMap::new(),
            list_warned: None,
        }
    }

    /// 한 번 잰다.
    async fn tick(&mut self) {
        let started = Instant::now();
        let t = now_ms();
        self.sample_node(started, t);
        self.sample_vms(started).await;
        self.store.prune(t);
    }

    fn sample_node(&mut self, at: Instant, t: i64) {
        let now = NodeCounters {
            at,
            cpu: self.probe.cpu_times(),
            disk: self.probe.disk_bytes(),
            nic: self.probe.nic_bytes(),
        };
        if let Some(prev) = &self.node
            && let Ok(sample) = node_sample(
                prev,
                &now,
                t,
                self.probe.memory(),
                self.probe.fs(&self.data_dir),
            )
        {
            self.store.push_node(sample);
        }
        self.node = Some(now);
    }

    async fn sample_vms(&mut self, started: Instant) {
        let running = match self.hv.list().await {
            Ok(vms) => vms,
            Err(e) => {
                if warn_due(&mut self.list_warned, started) {
                    tracing::warn!(error = %e, "cannot list VMs for metrics");
                }
                return;
            }
        };
        let running: Vec<_> = running
            .into_iter()
            .filter(|vm| matches!(vm.status, RuntimeStatus::Running | RuntimeStatus::Paused))
            .collect();
        // vCPU 수와 tap NIC 번호는 클러스터의 VM 스펙에서 온다(없으면 CPU·네트워크가 `null`). 사용자
        // 모드 NIC(`user`)는 tap이 없어 뺀다.
        let specs: HashMap<Uuid, (u32, Vec<usize>)> = self
            .cluster
            .list::<VmSpec>()
            .map(|(_, vms)| {
                vms.into_iter()
                    .map(|vm| {
                        let cpu = &vm.spec.cpu;
                        let vcpus = u32::from(cpu.sockets) * u32::from(cpu.cores);
                        let taps = (vm.spec.nics.iter().enumerate())
                            .filter(|(_, nic)| nic.network != USER_NETWORK)
                            .map(|(i, _)| i)
                            .collect();
                        (vm.id, (vcpus, taps))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // 시각(단조·벽시계)과 마감은 `list()`가 끝난 뒤에 잡는다(QEMU의 `list()`는 VM마다 QMP를 차례로
        // 불러 응답 없는 VM 하나로 늦어질 수 있다).
        let at = Instant::now();
        let t = now_ms();
        let deadline = at + VM_TIMEOUT;
        let this = &*self;
        let readings = join_all(running.iter().map(|vm| {
            let taps = specs.get(&vm.id).map_or(&[][..], |s| &s.1[..]);
            async move {
                let reading =
                    tokio::time::timeout_at(deadline, this.measure_vm(vm.id, vm.pid, taps, at))
                        .await;
                (vm.id, reading.ok())
            }
        }))
        .await;

        // 사라진 VM의 카운터는 지운다(다시 나타나면 기준부터).
        let live: HashSet<Uuid> = running.iter().map(|vm| vm.id).collect();
        self.vms.retain(|id, _| live.contains(id));
        self.disk_warned.retain(|id, _| live.contains(id));
        for (id, reading) in readings {
            // 측정 시간을 넘었다. 직전 카운터를 두고 다음에 잰다.
            let Some(reading) = reading else { continue };
            if let Some(error) = &reading.disk_error
                && warn_due(self.disk_warned.entry(id).or_default(), started)
            {
                tracing::warn!(vm = %id, error, "cannot read VM disk counters for metrics");
            }
            let now = reading.counters;
            if let Some(prev) = self.vms.get(&id)
                && prev.pid == now.pid
                && let Ok(sample) =
                    vm_sample(prev, &now, t, specs.get(&id).map(|s| s.0), reading.rss_mib)
            {
                self.store.push_vm(id, sample);
            }
            self.vms.insert(id, now);
        }
    }

    /// `at`은 이번 측정의 VM 측정 시작 시각이다. 프로세스·tap은 기다리지 않고 곧바로 읽으므로 그 시각으로
    /// 적고, 디스크는 `disk_stats`의 응답을 기다린 뒤 응답 시각으로 따로 적는다.
    async fn measure_vm(&self, id: Uuid, pid: u32, taps: &[usize], at: Instant) -> VmReading {
        let process = self.probe.process(pid);
        // tap NIC가 없거나(사용자 모드 NIC뿐, NIC 없음) tap 하나라도 읽지 못하면 네트워크는 `null`이다.
        let tap = (!taps.is_empty())
            .then(|| {
                taps.iter().try_fold((0u64, 0u64), |(rx, tx), &i| {
                    let (r, t) = self.probe.tap_bytes(&tap_name(id, i))?;
                    Some((rx + r, tx + t))
                })
            })
            .flatten();
        let (disk, disk_error) =
            match tokio::time::timeout(DISK_STATS_TIMEOUT, self.hv.disk_stats(id)).await {
                Ok(Ok(counters)) => (Some(counters), None),
                Ok(Err(e)) => (None, Some(e.to_string())),
                Err(_) => (None, Some("query-blockstats timed out".to_string())),
            };
        VmReading {
            counters: VmCounters {
                pid,
                at,
                cpu_ns: process.map(|p| p.cpu_ns),
                disk,
                disk_at: Instant::now(),
                tap,
            },
            rss_mib: process.map(|p| p.rss_mib),
            disk_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use opl_driver::{MockHypervisor, StopMode};
    use opl_types::resource::Resource;
    use opl_types::runtime::VmRuntimeSpec;
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType, NicModel, NicSpec, PowerState};

    use super::super::host::ProcStat;
    use super::*;
    use crate::bootstrap::tests::single_node;
    use opl_types::metrics::{MetricsQuery, MetricsScope};

    /// 시험이 정한 값을 돌려주는 [`HostProbe`]
    #[derive(Default)]
    struct FakeProbe(Mutex<FakeValues>);

    #[derive(Default, Clone)]
    struct FakeValues {
        cpu: Option<CpuTimes>,
        memory: Option<(u64, u64)>,
        disk: Option<(u64, u64)>,
        nic: Option<(u64, u64)>,
        processes: HashMap<u32, ProcStat>,
        taps: HashMap<String, (u64, u64)>,
        fs: Option<(u64, u64)>,
        /// `process()`가 불린 시각들
        process_calls: Vec<Instant>,
    }

    impl FakeProbe {
        fn set(&self, f: impl FnOnce(&mut FakeValues)) {
            f(&mut self.0.lock().unwrap());
        }
    }

    impl HostProbe for FakeProbe {
        fn cpu_times(&self) -> Option<CpuTimes> {
            self.0.lock().unwrap().cpu
        }
        fn memory(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().memory
        }
        fn disk_bytes(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().disk
        }
        fn nic_bytes(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().nic
        }
        fn process(&self, pid: u32) -> Option<ProcStat> {
            let mut v = self.0.lock().unwrap();
            v.process_calls.push(Instant::now());
            v.processes.get(&pid).copied()
        }
        fn tap_bytes(&self, name: &str) -> Option<(u64, u64)> {
            self.0.lock().unwrap().taps.get(name).copied()
        }
        fn fs(&self, _path: &Path) -> Option<(u64, u64)> {
            self.0.lock().unwrap().fs
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        cluster: Arc<Cluster>,
        hv: Arc<MockHypervisor>,
        probe: Arc<FakeProbe>,
        store: Arc<MetricsStore>,
        sampler: Sampler,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let hv = Arc::new(MockHypervisor::default());
        let probe = Arc::new(FakeProbe::default());
        let store = Arc::new(MetricsStore::default());
        let sampler = Sampler::new(
            hv.clone(),
            probe.clone(),
            cluster.clone(),
            dir.path().to_path_buf(),
            store.clone(),
        );
        Fixture {
            _dir: dir,
            cluster,
            hv,
            probe,
            store,
            sampler,
        }
    }

    fn node_samples(store: &MetricsStore) -> Vec<NodeSample> {
        store
            .read(&MetricsQuery {
                scope: MetricsScope::Nodes,
                since: None,
            })
            .node
            .unwrap()
    }

    fn vm_samples(store: &MetricsStore, id: Uuid) -> Vec<VmSample> {
        store
            .read(&MetricsQuery {
                scope: MetricsScope::Vm { id },
                since: None,
            })
            .vm
            .unwrap_or_default()
    }

    /// 클러스터에 vCPU 2개, NIC 2개(`lan`)인 VM을 만들고 하이퍼바이저에서 켠다.
    async fn running_vm(f: &Fixture) -> Uuid {
        running_vm_on(f, &["lan", "lan"]).await
    }

    /// [`running_vm`]과 같고 NIC마다 네트워크를 정한다.
    async fn running_vm_on(f: &Fixture, networks: &[&str]) -> Uuid {
        let cpu = CpuSpec {
            sockets: 1,
            cores: 2,
            model: CpuModel::Host,
        };
        let nics = networks
            .iter()
            .map(|network| NicSpec {
                network: network.to_string(),
                model: NicModel::Virtio,
                mac: None,
                security_groups: vec![],
            })
            .collect();
        let vm = Resource::new(
            "web",
            VmSpec {
                cpu: cpu.clone(),
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics,
                power: PowerState::Running,
                restart: Default::default(),
                node: None,
                snapshot_schedule: None,
                ha: false,
            },
        );
        f.cluster.create(&vm).await.unwrap();
        start(f, vm.id).await;
        vm.id
    }

    async fn start(f: &Fixture, id: Uuid) {
        f.hv.start(
            &VmRuntimeSpec {
                id,
                name: "web".into(),
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 2,
                    model: CpuModel::Host,
                },
                memory_mib: 256,
                firmware: Firmware::Uefi,
                machine: MachineType::Virt,
                disks: vec![],
                nics: vec![],
            },
            None,
        )
        .await
        .unwrap();
    }

    fn pid_of(hv: &MockHypervisor, id: Uuid) -> u32 {
        futures_util::FutureExt::now_or_never(hv.list())
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|vm| vm.id == id)
            .unwrap()
            .pid
    }

    /// VM 카운터를 정한다: 프로세스 CPU 시간·RSS, 디스크 카운터, 두 tap의 호스트 쪽 (받은, 보낸).
    fn set_vm(f: &Fixture, id: Uuid, cpu_ns: u64, disk: DiskCounters, tap: (u64, u64)) {
        let pid = pid_of(&f.hv, id);
        f.probe.set(|v| {
            v.processes.insert(
                pid,
                ProcStat {
                    cpu_ns,
                    rss_mib: 300,
                },
            );
            v.taps.insert(tap_name(id, 0), tap);
            v.taps.insert(tap_name(id, 1), tap);
        });
        f.hv.set_disk_stats(id, disk);
    }

    fn disk(bytes: u64, ops: u64) -> DiskCounters {
        DiskCounters {
            rd_bytes: bytes,
            wr_bytes: bytes * 2,
            rd_operations: ops,
            wr_operations: ops * 2,
        }
    }

    #[tokio::test]
    async fn node_rates_start_on_the_second_tick() {
        let mut f = fixture().await;
        tokio::time::pause();
        f.probe.set(|v| {
            v.cpu = Some(CpuTimes {
                busy: 100,
                total: 1_000,
            });
            v.memory = Some((1_000, 4_000));
            v.disk = Some((0, 0));
            v.nic = Some((1_000, 2_000));
            v.fs = Some((10, 100));
        });
        f.sampler.tick().await;
        // 첫 측정은 기준만 잡는다.
        assert!(node_samples(&f.store).is_empty());

        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| {
            v.cpu = Some(CpuTimes {
                busy: 600,
                total: 2_000,
            });
            v.disk = Some((5_000, 10_000));
            v.nic = Some((6_000, 2_000));
        });
        f.sampler.tick().await;
        let samples = node_samples(&f.store);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(
            (s.mem_used_mib, s.mem_total_mib),
            (Some(1_000), Some(4_000))
        );
        assert_eq!(
            (s.disk_read_bps, s.disk_write_bps),
            (Some(1_000.0), Some(2_000.0))
        );
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(1_000.0), Some(0.0)));
        assert_eq!((s.fs_used_mib, s.fs_total_mib), (Some(10), Some(100)));
        assert!((s.t - now_ms()).abs() < 60_000);

        // NIC 카운터가 줄면(32비트 카운터가 넘침) 이번 점 없이 기준을 다시 잡는다.
        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| v.nic = Some((10, 2_000)));
        f.sampler.tick().await;
        assert_eq!(node_samples(&f.store).len(), 1);
        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| v.nic = Some((5_010, 2_000)));
        f.sampler.tick().await;
        let samples = node_samples(&f.store);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].net_rx_bps, Some(1_000.0));
        // 못 읽은 값은 `null`이다.
        assert_eq!(samples[1].disk_read_bps, Some(0.0));
        f.probe.set(|v| v.disk = None);
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert_eq!(node_samples(&f.store)[2].disk_read_bps, None);
    }

    #[tokio::test]
    async fn vm_rates_start_on_the_second_tick() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        assert!(vm_samples(&f.store, id).is_empty());

        tokio::time::advance(INTERVAL).await;
        // vCPU 2개 중 하나가 5초 내내 바빴다.
        set_vm(&f, id, 5_000_000_000, disk(50_000, 50), (1_000, 3_000));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(
            (s.disk_read_bps, s.disk_write_bps),
            (Some(10_000.0), Some(20_000.0))
        );
        assert_eq!(
            (s.disk_read_iops, s.disk_write_iops),
            (Some(10.0), Some(20.0))
        );
        // 게스트 기준: 받기는 tap의 보내기(두 tap 합)다.
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(1_200.0), Some(400.0)));
    }

    /// 사용자 모드 NIC(`user`)는 tap이 없으므로 빼고 tap NIC만 합한다.
    #[tokio::test]
    async fn user_mode_nics_are_left_out_of_the_network_rates() {
        let mut f = fixture().await;
        let id = running_vm_on(&f, &["lan", "user"]).await;
        let only_tap0 = |f: &Fixture| {
            f.probe.set(|v| {
                v.taps.remove(&tap_name(id, 1));
            })
        };
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        only_tap0(&f);
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 0, disk(0, 0), (1_000, 3_000));
        only_tap0(&f);
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(600.0), Some(200.0)));
    }

    #[tokio::test]
    async fn vm_baseline_is_taken_again_when_the_pid_changes_or_a_counter_drops() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 1_000_000_000, disk(1_000, 1), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);

        // 다시 켜면 pid가 바뀐다. 카운터가 줄지 않았어도 이번 점은 없다(다른 프로세스의 값이다).
        f.hv.stop(id, StopMode::Force).await.unwrap();
        start(&f, id).await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 10_000_000_000, disk(2_000, 2), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 15_000_000_000, disk(2_500, 7), (0, 0));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].cpu, Some(50.0));
        assert_eq!(samples[1].disk_read_bps, Some(100.0));

        // 카운터가 줄면 이번 점 없이 기준을 다시 잡는다.
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 16_000_000_000, disk(100, 1), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 2);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 17_000_000_000, disk(600, 2), (0, 0));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[2].disk_read_bps, Some(100.0));
    }

    #[tokio::test]
    async fn a_disk_stats_failure_nulls_only_the_disk_fields() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        f.hv.fail_disk_stats(id, true);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(1_000, 1), (500, 500));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(s.net_rx_bps, Some(200.0));
        assert_eq!(
            (
                s.disk_read_bps,
                s.disk_write_bps,
                s.disk_read_iops,
                s.disk_write_iops
            ),
            (None, None, None, None)
        );
        // 경고는 10분에 한 번이다.
        let warned = f.sampler.disk_warned[&id];
        assert!(warned.is_some());
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert_eq!(f.sampler.disk_warned[&id], warned);
        tokio::time::advance(WARN_EVERY).await;
        f.sampler.tick().await;
        assert!(f.sampler.disk_warned[&id] > warned);

        // 다시 읽히면 그다음 점부터 디스크 값이 있다.
        f.hv.fail_disk_stats(id, false);
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(6_000, 1), (500, 500));
        f.sampler.tick().await;
        let last = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!(last.disk_read_bps, Some(1_000.0));
    }

    /// `list()`가 오래 걸려도(응답 없는 VM) 마감은 그 뒤부터라 다른 VM의 점이 생긴다.
    #[tokio::test]
    async fn a_slow_list_does_not_use_up_the_vm_deadline() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(1_000, 1), (0, 0));
        f.hv.delay_lists(VM_TIMEOUT + Duration::from_secs(1));
        f.hv.delay_disk_stats(Duration::from_millis(10));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);
    }

    /// VM 점의 벽시계 `t`는 `list()`가 끝난 뒤의 시각이다(노드 점은 측정 시작 시각).
    #[tokio::test]
    async fn vm_sample_time_is_taken_after_the_list() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        f.probe.set(|v| v.memory = Some((1, 2)));
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        // 실제 시간으로 기다린다(벽시계는 `tokio::time::pause`로 멈추지 않는다).
        f.hv.delay_lists(Duration::from_millis(200));
        f.sampler.tick().await;
        f.sampler.tick().await;
        let node_t = node_samples(&f.store).pop().unwrap().t;
        let vm_t = vm_samples(&f.store, id).pop().unwrap().t;
        assert!(vm_t - node_t >= 200, "node {node_t}, vm {vm_t}");
    }

    /// 프로세스·tap은 `disk_stats`를 기다리기 전에 읽는다. QMP가 늦게 답해도 CPU 비율은 측정 간격으로,
    /// 디스크 비율은 응답 시각의 간격으로 나눈다.
    #[tokio::test]
    async fn a_slow_disk_stats_answer_does_not_skew_the_rates() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(5_900, 5), (0, 0));
        f.hv.delay_disk_stats(Duration::from_millis(900));
        let started = Instant::now();
        f.sampler.tick().await;
        assert_eq!(
            f.probe.0.lock().unwrap().process_calls.last(),
            Some(&started)
        );
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!(s.cpu, Some(50.0));
        // 디스크 응답은 0초 뒤와 0.9초 뒤라 5.9초 사이의 증가다.
        assert_near(s.disk_read_bps, 1_000.0);
    }

    /// QMP 응답 지연이 측정마다 다르면 디스크 비율은 응답 시각의 차이로 나눈다(CPU는 측정 시작의 차이).
    #[tokio::test]
    async fn disk_rates_use_the_disk_answer_times() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.hv.delay_disk_stats(Duration::from_millis(900));
        f.sampler.tick().await;
        // 다음 측정은 처음 측정을 시작하고 5초 뒤다.
        tokio::time::advance(INTERVAL - Duration::from_millis(900)).await;
        set_vm(&f, id, 5_000_000_000, disk(4_200, 0), (0, 0));
        f.hv.delay_disk_stats(Duration::from_millis(100));
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_near(s.cpu, 50.0);
        // 디스크 응답은 0.9초 뒤와 5.1초 뒤다.
        assert_near(s.disk_read_bps, 1_000.0);
    }

    /// `want`와 0.1% 안이다(tokio 타이머가 기다림을 밀리초로 올려 시각이 1ms쯤 밀린다).
    fn assert_near(got: Option<f64>, want: f64) {
        let got = got.unwrap();
        assert!((got - want).abs() < want * 1e-3, "{got} != {want}");
    }

    #[tokio::test]
    async fn counters_of_vanished_vms_are_dropped() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        assert!(f.sampler.vms.contains_key(&id));
        f.hv.stop(id, StopMode::Force).await.unwrap();
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert!(f.sampler.vms.is_empty());
        assert!(f.sampler.disk_warned.is_empty());
    }

    /// VM이 클러스터에 없으면(스펙을 모름) vCPU·NIC 수를 몰라 CPU와 네트워크가 `null`이다.
    #[tokio::test]
    async fn a_vm_without_a_spec_has_no_cpu_or_network() {
        let mut f = fixture().await;
        let id = Uuid::now_v7();
        start(&f, id).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 1_000_000_000, disk(5_000, 0), (10, 10));
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!((s.cpu, s.net_rx_bps, s.net_tx_bps), (None, None, None));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(s.disk_read_bps, Some(1_000.0));
    }

    #[tokio::test]
    async fn spawned_sampler_samples_every_five_seconds_until_shut_down() {
        let f = fixture().await;
        tokio::time::pause();
        f.probe.set(|v| v.memory = Some((1, 2)));
        let sampler = MetricsSampler::spawn(
            f.hv.clone(),
            f.probe.clone(),
            f.cluster.clone(),
            PathBuf::from("/"),
            f.store.clone(),
        );
        // 첫 측정은 곧바로(기준만), 그다음은 5초 뒤다.
        tokio::time::sleep(INTERVAL / 2).await;
        assert!(node_samples(&f.store).is_empty());
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(node_samples(&f.store).len(), 1);
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(node_samples(&f.store).len(), 2);
        sampler.shutdown().await;
        tokio::time::sleep(INTERVAL * 2).await;
        assert_eq!(node_samples(&f.store).len(), 2);
    }
}
```

`crates/opld/tests/bridge.rs`에서

```rust

    // 쓰고 있는 네트워크는 지울 수 없다.
```

을 다음으로 바꾼다:

```rust

    // 지표: tap NIC의 카운터로 VM 네트워크 값이 있다(사용자 모드 NIC는 빼고 센다).
    common::eventually("VM network metrics", || async {
        let m = d.ctl_json(&["vm", "stats", "web"]).await;
        m["samples"]
            .as_array()
            .and_then(|s| s.last())
            .is_some_and(|s| {
                s["net_rx_bps"].as_f64().is_some() && s["net_tx_bps"].as_f64().is_some()
            })
    })
    .await;

    // 쓰고 있는 네트워크는 지울 수 없다.
```

`crates/opld/tests/e2e.rs`에서

```rust

use common::{Daemon, free_port};
use rustix::process::{Pid, test_kill_process};
```

을 다음으로 바꾼다:

```rust

use common::{Daemon, eventually, free_port};
use rustix::process::{Pid, test_kill_process};
```

`crates/opld/tests/e2e.rs`에서

```rust

    // 콘솔: oplctl이 연 로컬 포트로 VNC 서버의 인사말이 온다. 블록을 나가면 콘솔을 닫는다.
```

을 다음으로 바꾼다:

```rust

    // 지표: 측정기가 5초마다 재므로 켠 뒤 10초 남짓이면 VM 점이 생긴다(첫 측정은 기준만).
    eventually("VM metrics", || async {
        let m = d.ctl_json(&["vm", "stats", "web"]).await;
        m["samples"].as_array().is_some_and(|s| !s.is_empty())
    })
    .await;
    let m = d.ctl_json(&["vm", "stats", "web"]).await;
    let s = m["samples"].as_array().unwrap().last().unwrap().clone();
    assert!(s["cpu"].as_f64().is_some(), "{m}");
    assert!(s["mem_rss_mib"].as_u64().is_some_and(|n| n > 0), "{m}");
    assert!(s["disk_write_bps"].as_f64().is_some(), "{m}");
    assert_eq!(m["unreachable"], serde_json::json!([]), "{m}");
    let table = d.ctl(&["vm", "stats", "web"]).await.unwrap();
    assert!(table.starts_with("TIME "), "{table}");
    assert_eq!(table.lines().count(), 2, "{table}");
    let nodes = d.ctl_json(&["node", "stats"]).await;
    let node = &nodes["nodes"][0]["samples"];
    assert!(node.as_array().is_some_and(|s| !s.is_empty()), "{nodes}");
    let table = d.ctl(&["node", "stats"]).await.unwrap();
    assert!(
        table.lines().nth(1).unwrap().starts_with("node1 "),
        "{table}"
    );

    // 콘솔: oplctl이 연 로컬 포트로 VNC 서버의 인사말이 온다. 블록을 나가면 콘솔을 닫는다.
```

`docs/superpowers/specs/2026-11-11-monitoring-ui-design.md`에서

```markdown
5. **oplctl과 문서:** `node stats`·`vm stats`(`--watch`, `-o json`), opld E2E, README "지표" 절, 이 문서 §11
```

을 다음으로 바꾼다:

```markdown
5. **oplctl과 문서:** `node stats`·`vm stats`(`--watch`, `-o json`), opld E2E, README "지표" 절, 이 문서 §11

## 11. 구현(계획 44)

- **타입(`opl-types/src/metrics.rs`).** §3.3대로 `NodeSample`·`VmSample`(값은 모두 `Option` — 비율 `f64`, MiB `u64`, 없으면 `null`로 보냄), `MetricsQuery`, `MetricsScope`(`kind` 태그: `{"kind":"nodes"}`·`{"kind":"vm","id":…}`), `NodeSeries`·`VmSeries`. API 응답 타입 `NodeMetricsResponse { nodes, unreachable, unsupported }`(노드 이름 순)·`VmMetricsResponse { vm, samples, unreachable, unsupported }`를 더했다.
- **호스트 읽기(`opld/src/metrics/host.rs`).** `HostProbe`는 동기 트레이트(`Send + Sync`)이고 `process(pid: u32)`, `native() -> Arc<dyn HostProbe>`가 OS에 맞는 구현을 준다. `LinuxProbe::with_root(path)`는 `/proc`·`/sys` 루트를 받아 Linux가 아니어도 시험에서 컴파일되므로 이 Mac에서 가짜 트리로 시험한다. Linux NIC는 `device` 링크가 있는 장치만 더한다(브리지·tap·VXLAN·본드·VLAN은 없다). `MacProbe`는 `proc_pidinfo`의 Mach 시간 단위를 `mach_timebase_info`로 ns로 바꾸고, 메모리는 active + wired + 압축 페이지다. 그 밖 OS는 `NoProbe`. `tap_bytes`는 호스트 기준 `(rx, tx)`이고 측정기가 게스트 기준으로 뒤집는다. macOS `getifaddrs`의 `if_data` 카운터는 32비트라 4 GiB마다 넘치는데, "카운터가 줄면 기준을 다시 잡는다"는 규칙으로 처리한다. opld 의존성에 워크스페이스의 `libc`를 더했다(새 크레이트 없음).
- **QMP·Hypervisor.** `opl_qemu::BlockStats { device, qdev, node_name, rd_bytes, wr_bytes, rd_operations, wr_operations }`(카운터가 없으면 프로토콜 오류), `QmpClient::query_blockstats()`. `opl_driver::DiskCounters`(Copy·Default, QEMU를 다시 켜면 0부터), `Hypervisor::disk_stats(id)`. QEMU는 장치 경로 `qdev`의 장치 id가 `disk{i}-dev`인 백엔드만 합한다 — 맨 위 노드 이름은 블록 작업(미러·백업 필터) 중 바뀌므로 쓰지 않고, pflash는 빠진다. `disk_stats` 자체는 QMP 기본 제한(5초)이고 1초 제한은 측정기가 겉에 둔다. 실행 중이 아니면 `NotRunning`. `MockHypervisor::set_disk_stats`·`fail_disk_stats`(디스크 읽기만 실패).
- **측정기·저장소.** `MetricsStore`(`push_node`, `push_vm`, `prune(now_ms)`, `read`, `MAX_SAMPLES = 720`, `VM_SERIES_TTL_MS`), `MetricsSampler::spawn(hv, probe, cluster, data_dir, store)`·`shutdown()` — 설계의 `node_id` 인자는 쓰지 않아 뺐다(점에는 노드 id가 없고 `StoreSource`가 붙인다). 상수 `INTERVAL` 5초, `DISK_STATS_TIMEOUT` 1초, `VM_TIMEOUT` 4초(한 번의 측정의 VM 마감), `WARN_EVERY` 10분. VM 마감은 `hv.list()`가 끝난 뒤에 잡는다 — QEMU의 `list()`는 VM마다 QMP `query-status`(5초 제한)를 차례로 불러 응답 없는 VM 하나로 늦어질 수 있고, 그 전에 잡으면 그 VM 하나 때문에 노드의 모든 VM이 점을 잃는다. VM마다 프로세스 CPU·RSS와 tap 카운터는 `disk_stats`를 기다리기 전에 곧바로 읽어 VM 측정 시작의 단조 시각(`list()` 뒤, 측정마다 지역 값)으로 적고, 디스크 카운터는 응답이 온 시각으로 따로 적어 디스크 비율은 응답 시각의 차이로 나눈다 — QMP 응답이 최대 1초 늦고 그 늦음이 측정마다 달라도 CPU·네트워크·디스크 비율이 틀어지지 않는다. VM 점의 벽시계 `t`도 VM 측정 시작 때 잡는다(노드 점의 `t`는 측정 시작). 시험용 `MockHypervisor::delay_lists`·`delay_disk_stats`. 카운터 하나라도 줄면 그 점 전체를 건너뛰고 기준을 다시 잡는다. `Running`·`Paused` VM만 잰다. vCPU = sockets × cores, tap NIC 번호는 클러스터의 VM 스펙에서 오고(스펙이 없으면 CPU·네트워크 `null`), 사용자 모드 NIC(`user`)는 tap이 없으므로 빼고 센다(아래 "설계와 다른 점"). `member.rs`는 저장소 하나를 만들어 측정기·노드 사이 서버·API가 나눠 쓰고, 위트니스에는 측정기가 없다. `FEATURES` 끝에 `metrics`(위트니스도 목록은 같다).
- **피어 RPC·API.** §3.3대로 `ReadMetrics`, `opl_peer::MetricsReply`·`MetricsSource`, `PeerService::with_metrics`, `Peers::read_metrics(addr, query, timeout) -> Result<Option<MetricsReply>, Status>`(`Unimplemented`는 `Ok(None)`). opld `StoreSource::new(node, store)`. 경로 세 개는 OpenAPI 태그 `metrics`, 피어 제한 3초. 내 노드에 소스가 없으면 `unsupported`(`/nodes/{key}/metrics`는 409), Raft 구성원이 아닌 노드는 503 `node_unreachable` "did not answer: not a cluster member". 꺼진 것으로 기록된 다른 노드(Node `status.online == false`)는 묻지 않고 `unreachable`에 넣는다(`/nodes/{key}/metrics`는 503 `node_unreachable` "node … is offline") — 계획 45가 5초마다 부르므로 꺼진 노드의 연결 제한 3초를 매번 기다리지 않게 한다. 내 노드는 기록과 상관없이 직접 읽는다. RPC 실패 로그는 폴링마다 쌓이지 않게 `debug`다.
- **oplctl.** `orbivisorctl node stats`(열 `NODE TIME CPU MEMORY DISK READ DISK WRITE NET RX NET TX DATA FS` — 메모리·파일 시스템은 `사용/전체`, 점이 없는 노드는 `-`, 노드 이름은 `GET /nodes`로 찾고 모르면 id), `orbivisorctl vm stats <VM>`(열 `TIME CPU MEMORY DISK READ DISK WRITE READ IOPS WRITE IOPS NET RX NET TX`, 점이 없으면 머리만 찍고 stderr에 `note: no metrics yet; the VM is off or started less than 10 seconds ago`). 둘 다 `--watch`면 5초마다 다시 읽고 빈 줄 뒤에 새 표를 이어 찍는다. 다시 읽을 때는 `since`를 보낸다: VM은 마지막 점의 `t`, 노드는 응답에 나온 노드의 마지막 `t` 가운데 가장 이른 것(지금까지 본 노드가 응답에 빠진(닿지 않음·옛 버전) 동안은 그 노드를 빼고 셈한다. 점이 없는 노드가 있거나 빠졌던 노드가 이번에 돌아왔으면 다음 읽기 한 번은 보내지 않음 — 돌아온 노드의 시계가 늦으면 그 줄이 멈추므로). `--watch`의 요청 오류는 stderr에 `error: …`를 찍고 다음 회차를 계속하고, 401·403·404(`NotLoggedIn`, API 오류 코드 `unauthorized`·`forbidden`·`not_found`)만 끝낸다. 새 점이 없는 시리즈는 전에 받은 마지막 점을 다시 보인다. `unreachable`·`unsupported`는 표 뒤 stderr에 `warning: no metrics from: …`, `note: these nodes do not report metrics yet: …`. `-o json`은 응답 그대로(첫 읽기는 남은 모든 점, `--watch`의 다음 읽기는 새 점만)이고 stderr에 쓰지 않는다. `-o json --watch`는 회차마다 한 줄 compact JSON(JSON Lines)이고 빈 줄이 없다.
- **실제 시험.** 이 Mac(QEMU 11.1): opl-qemu `qemu-it`(`tests/qemu_boot.rs`)에서 HMP `qemu-io -d /machine/peripheral/disk0-dev/virtio-backend "aio_write -q 0 64k"`와 `aio_flush` 뒤 `wr_bytes`가 는다. opld `qemu-it` `tests/e2e.rs`: VM을 켜고 `vm stats`에 점이 생길 때까지 기다린 뒤 CPU·RSS·디스크 쓰기 값이 있고 `node stats`의 노드 점과 표가 있다. Linux(`net-it`, root) `tests/bridge.rs`: 브리지 NIC와 사용자 모드 NIC를 단 VM의 `net_rx_bps`·`net_tx_bps`가 `null`이 아니다 — 이것과 Linux 실제 `/proc`·`/sys` 읽기는 Linux CI(linux-kvm, check)만 본다.
- **벽시계 `t`의 한계.** 점의 `t`와 `since`, 1시간 지난 VM 시리즈 지우기는 그 노드의 벽시계다(비율은 단조 시계라 맞다). NTP가 시계를 뒤로 돌리면 `since`로 이어 받기가 시계가 따라잡을 때까지 비고(새 점의 `t`가 `since`보다 작다), 한 시간 넘게 앞으로 뛰면 마지막 점이 1시간 지난 것으로 보여 VM 시리즈가 한 번 지워진다.
- **스펙 변경과 다시 시작 사이.** vCPU 수와 tap NIC 목록은 지금 클러스터의 VM 스펙에서 읽는다. 그래서 다시 시작해야 적용되는 스펙 변경(코어 수, NIC 더하기) 뒤 VM을 다시 시작하기 전까지는 CPU %가 새 vCPU 수로 나뉘고, 새 NIC의 tap이 없어 네트워크가 `null`이다.
- **1차 리뷰에서 고친 것.** `LinuxProbe::process`의 CPU 시간을 `u128`로 계산(tick × 10⁹이 u64를 넘어 debug 빌드에서 측정기가 panic하던 것), 측정기 VM 마감을 `list()` 뒤에, VM 점 시각을 `disk_stats` 전에, 꺼진 노드는 묻지 않음과 RPC 실패 로그 `debug`, API 시험 보강(노드마다 엇갈린 VM 점 시각의 시각 순 합치기, id 순서와 다른 노드 이름 순, 위트니스로 표시한 구성원이 어느 목록에도 없음 — 이름은 Node 기록을 다시 만들어, 위트니스는 구성원 노드의 Node 기록을 `witness = true`로 고쳐 본다), `node stats --watch`의 빠진 노드와 `since`, `--watch`의 일시 오류, `-o json --watch`의 JSON Lines.
- **2차 리뷰에서 고친 것.** 1차 수정의 "VM 점 시각을 `disk_stats` 전에"는 프로세스·tap을 `disk_stats`를 기다린 뒤 읽으면서 기다리기 전 시각으로 적어 QMP 응답이 늦으면 CPU·네트워크 비율이 틀어졌다 — 프로세스·tap을 먼저 읽고 디스크는 응답 시각을 따로 둔다(위, `Sampler`의 시각 필드를 없앰). VM 점의 벽시계 `t`를 `list()` 뒤에. `node stats --watch`가 노드가 빠진 내내 `since` 없이 받던 것을 돌아온 다음 한 번만으로. 이 측정기 수정 가운데 측정 순서는 스테이지 5가 바꾼 `measure_vm` 서명 줄과 겹쳐 스테이지 5에서 고쳤다.
- **설계와 다른 점.** `MetricsSampler::spawn`에 `node_id`가 없다(위). §3.1의 VM 네트워크 "tap이 없으면 `null`"은 "tap NIC(사용자 모드 NIC가 아닌 NIC)가 없거나 그 tap 하나라도 읽지 못하면 `null`"이다: 처음 구현은 스펙의 모든 NIC의 tap을 찾아 브리지 NIC와 `user` NIC를 함께 단 VM의 네트워크가 늘 `null`이었고, opld E2E를 쓰며 찾아 고쳤다. QEMU 디스크 합은 노드 이름이 아니라 `qdev` 장치 id로 고른다(위). §5의 "Linux CI linux-kvm에서 `net_rx_bps`"는 tap이 생기는 브리지 시험(`net-it`)에서 본다 — 사용자 모드 NIC만 단 E2E VM에는 tap이 없다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1690개, 실패 0

Run: `cargo test -p opld --features qemu-it --test e2e`
Expected: PASS (실제 QEMU가 필요하다)

Run: `bash packaging/tests/name-test.sh`
Expected: ok

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md crates docs
git commit -m "feat(oplctl): 지표 보기 — orbivisorctl node stats(노드마다 마지막 점: CPU·메모리·디스크·네트워크·데이터 파일 시스템, 점이 없는 노드는 -)·vm stats <VM>(마지막 점, 점이 없으면 stderr 안내), 둘 다 --watch(5초마다 since로 새 점만 다시 읽어 새 표를 이어 찍음, 새 점이 없으면 전에 받은 점)와 -o json(응답 그대로), unreachable·unsupported는 stderr; 측정기는 사용자 모드 NIC(user)를 빼고 tap NIC만 합함(브리지+user NIC VM의 네트워크가 늘 null이던 것); opld E2E에서 VM을 켠 뒤 vm stats·node stats에 점, Linux 브리지 시험에서 tap 네트워크 값; README \"지표\" 절, 설계 §11 구현(계획 44)"
```

---

## 완료 기준 (계획 44)

- 데몬이 뜬 뒤 10초 남짓이면 `orbivisorctl node stats`가 노드마다 CPU·메모리·데이터 파일 시스템(Linux는 디스크·네트워크도)을 보인다.
- 실행 중인 VM의 `orbivisorctl vm stats <VM>`이 CPU·RSS·디스크 읽기·쓰기와 IOPS(Linux 브리지 NIC는 네트워크도)를 보인다.
- `GET /api/v1/metrics/nodes`·`/nodes/{key}/metrics`·`/vms/{key}/metrics`가 `since` 뒤의 점만 돌려주고, 꺼진 노드는 `unreachable`, 옛 노드는 `unsupported`, 위트니스는 빠진다. Viewer도 읽는다.
- VM을 마이그레이션해도 `/vms/{key}/metrics`의 점이 시각 순으로 이어진다.
- 응답하지 않는 VM이 있어도 같은 노드의 다른 VM 점은 계속 생긴다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, opl-qemu·opld `qemu-it`, 웹 `typecheck`가 통과한다. Linux CI의 `linux-kvm`(`net-it` 포함)이 통과한다.

## 알고 받아들인 제한

- 재시작하면 그 노드의 1시간 기록이 비고, 꺼진 노드의 기록은 읽을 수 없다.
- macOS 노드는 노드 디스크 I/O와 VM 네트워크가 `null`이다.
- VM CPU·메모리는 QEMU 프로세스 전체 기준이다(게스트가 보는 값이 아님). vCPU 수·NIC 목록은 지금 스펙에서 읽으므로 재시작을 기다리는 스펙 변경 사이에는 어긋날 수 있다.
- 벽시계가 뒤로 가면 `since` 이어 받기가 잠시 비고, 한 시간 넘게 앞으로 뛰면 VM 시리즈가 한 번 지워진다.
- 그 밖의 제한은 설계 문서 §6·§11과 README "지표" 절에 있다.

## 실행 후 기록 (2026-11-11)

- **Task 1~5:** `feat/plan44-metrics`에서 계획대로 적용했다. Task마다 검증한 스테이지 커밋과 트리가 같았다(생성 파일 제외). 다섯 Task를 마친 트리는 리뷰와 CI 프로브를 거친 트리와 생성 파일까지 같다(이 계획 문서만 다름). Task 1만 구현 에이전트가 전체 시험을 다시 돌렸고, Task 2~5는 스테이지 검증과 CI 프로브를 믿고 적용·비교만 했다.
- **1차 리뷰에서 고친 것:**
  - 응답 없는 VM 하나가 노드의 모든 VM 점을 없애던 것: VM 마감을 `list()`가 끝난 뒤에 잡는다.
  - 꺼진 노드가 모든 지표 요청을 연결 제한(3초)만큼 늦추던 것: `online == false`인 노드는 부르지 않고 `unreachable`에 넣는다(로그는 debug로).
  - 프로세스 CPU 시간을 `u128`로 계산한다(오래 돈 큰 VM의 넘침).
  - 정렬·이름 순서·위트니스 제외 시험, 빠진 노드가 있을 때의 `since` 처리.
- **2차 리뷰에서 고친 것:** 1차 수정의 시각 때문에 CPU·네트워크 비율이 틀어지던 것. 프로세스·tap은 틱의 시각 `at`으로, 디스크는 `disk_stats` 직전 시각 `disk_at`으로 따로 적는다. VM 점의 `t`는 VM 측정 시작 때 잡는다. `node stats --watch`는 돌아온 노드가 있는 다음 요청 한 번만 `since`를 뺀다. 재시작을 기다리는 스펙 변경 사이의 한계를 문서에 적었다.

**테스트:**
- macOS `cargo test --workspace` 1690개(스테이지별 1653·1657·1670·1681·1690)
- 웹 시험 572개와 `typecheck`·`lint`
- `fmt`·`clippy -D warnings`(모든 기능)
- 패키징 셸 시험(`maint-test`·`build-deb-test`·`build-repo-test`·`setup-test`·`iso-test`·`name-test`)
- 실제 QEMU: `opl-qemu` qemu-it(`query-blockstats`와 HMP `qemu-io` 쓰기 뒤 `wr_bytes`), opld e2e

Linux CI(프로브 3회)에서 같은 시험과 linux-kvm(브리지 tap 카운터 시험 포함)·ldap·package·shell 작업이 통과했다.

## 다음 계획으로 넘길 사항

- 지표 수집(계획 44)이 끝났다. 차트·3D·끌어 놓는 VM 만들기(지표 설계 §7~9)는 가용성 뒤로 미루고 번호를 다시 매긴다.
- 다음: 하위 프로젝트 15 제어부 가용성과 대규모 클러스터(계획 45부터, 실행기 격리와 systemd watchdog). 사용자 요청으로 빠른 HA, 노드 전원 끄기 펜싱, HA 상태 검사(네트워크·스토리지·본딩 포트), 배치 규칙(affinity·anti-affinity와 soft 형태), 최적 배치, VM 무중단 유형 변경이 들어간다.
- 그 다음: 하위 프로젝트 16 SDN(OVN 백엔드).
- 테스트 VM에 계획 44를 배포하면 `orbivisorctl node stats`로 실제 Linux 값을 확인한다.
