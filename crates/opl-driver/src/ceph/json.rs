//! Ceph 명령(`ceph … --format json`, `ceph-volume lvm list --format json`, `ceph --version`)의 출력 해석과
//! 상태 보고서([`CephReport`]) 합치기. 모양은 ceph 19.2(squid)의 실제 출력을 따른다. 모르는 필드는
//! 무시하고 빠진 필드는 기본값이다(판이 바뀌어도 화면이 죽지 않게).

use std::collections::{BTreeMap, BTreeSet};

use opl_types::ceph::{
    CephHealth, CephHealthCheck, CephReport, MgrReport, MonReport, OsdReport, PgReport, PoolReport,
    Usage,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn parse<T: DeserializeOwned>(out: &[u8], what: &str) -> Result<T, String> {
    serde_json::from_slice(out).map_err(|e| format!("unexpected {what} output: {e}"))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StatusJson {
    health: HealthJson,
    quorum_names: Vec<String>,
    mgrmap: MgrJson,
    osdmap: OsdmapJson,
    pgmap: PgmapJson,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HealthJson {
    status: String,
    checks: BTreeMap<String, CheckJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CheckJson {
    severity: String,
    summary: SummaryJson,
    muted: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SummaryJson {
    message: String,
    count: u64,
}

/// `ceph mgr dump`·`ceph mgr stat`·`ceph status`의 `mgrmap`(19.2의 `status`는 `available`만 준다)
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MgrJson {
    available: bool,
    active_name: String,
    standbys: Vec<NamedJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NamedJson {
    name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OsdmapJson {
    num_osds: u32,
    num_up_osds: u32,
    num_in_osds: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PgmapJson {
    pgs_by_state: Vec<PgStateJson>,
    num_pgs: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PgStateJson {
    state_name: String,
    count: u64,
}

/// `ceph status`에서 읽은 것
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatusSummary {
    pub health: CephHealth,
    /// 정족수에 든 mon 이름
    pub quorum: Vec<String>,
    /// `mgrmap`(19.2는 `available`만 — 이름은 [`parse_mgr_dump`])
    pub mgr: MgrReport,
    pub num_osds: u32,
    pub num_up_osds: u32,
    pub num_in_osds: u32,
    pub pgs: PgReport,
}

/// `ceph status --format json`
pub fn parse_status(out: &[u8]) -> Result<StatusSummary, String> {
    let s: StatusJson = parse(out, "ceph status")?;
    let checks = s
        .health
        .checks
        .into_iter()
        .map(|(code, c)| CephHealthCheck {
            code,
            severity: c.severity,
            summary: c.summary.message,
            count: c.summary.count,
            muted: c.muted,
        })
        .collect();
    Ok(StatusSummary {
        health: CephHealth {
            status: s.health.status,
            checks,
        },
        quorum: s.quorum_names,
        mgr: mgr_report(s.mgrmap),
        num_osds: s.osdmap.num_osds,
        num_up_osds: s.osdmap.num_up_osds,
        num_in_osds: s.osdmap.num_in_osds,
        pgs: PgReport {
            total: s.pgmap.num_pgs,
            states: s
                .pgmap
                .pgs_by_state
                .into_iter()
                .map(|p| (p.state_name, p.count))
                .collect(),
        },
    })
}

fn mgr_report(m: MgrJson) -> MgrReport {
    MgrReport {
        active: Some(m.active_name).filter(|n| !n.is_empty()),
        standbys: m.standbys.into_iter().map(|s| s.name).collect(),
        available: m.available,
    }
}

/// `ceph mgr dump --format json`(또는 `mgr stat` — 대기 mgr 이름은 없다)
pub fn parse_mgr_dump(out: &[u8]) -> Result<MgrReport, String> {
    Ok(mgr_report(parse(out, "ceph mgr dump")?))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct QuorumJson {
    quorum_names: Vec<String>,
    monmap: MonmapJson,
    /// `ceph mon dump`는 monmap이 맨 위다.
    mons: Vec<MonJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MonmapJson {
    mons: Vec<MonJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MonJson {
    name: String,
    public_addrs: AddrVecJson,
    /// 옛 모양(`10.0.0.1:6789/0`)
    public_addr: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AddrVecJson {
    addrvec: Vec<AddrJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AddrJson {
    #[serde(rename = "type")]
    kind: String,
    addr: String,
}

/// `ceph quorum_status --format json`의 정족수에 든 mon 이름
pub fn parse_quorum(out: &[u8]) -> Result<Vec<String>, String> {
    Ok(parse::<QuorumJson>(out, "ceph quorum_status")?.quorum_names)
}

/// `ceph quorum_status`(또는 `ceph mon dump`)의 mon들. 주소는 v2 주소(없으면 첫 주소)다. 정족수는 같은
/// 출력의 `quorum_names`에서 읽는다(`mon dump`면 모두 정족수 밖으로 나온다).
pub fn parse_monmap(out: &[u8]) -> Result<Vec<MonReport>, String> {
    let q: QuorumJson = parse(out, "ceph quorum_status")?;
    let mons = if q.monmap.mons.is_empty() {
        q.mons
    } else {
        q.monmap.mons
    };
    Ok(mons
        .into_iter()
        .map(|m| {
            let addrs = &m.public_addrs.addrvec;
            let addr = addrs
                .iter()
                .find(|a| a.kind == "v2")
                .or(addrs.first())
                .map(|a| a.addr.clone())
                .unwrap_or_else(|| {
                    let a = &m.public_addr;
                    a.split_once('/').map_or(a.as_str(), |(a, _)| a).to_string()
                });
            MonReport {
                in_quorum: q.quorum_names.contains(&m.name),
                name: m.name,
                addr,
            }
        })
        .collect())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TreeJson {
    nodes: Vec<TreeNodeJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TreeNodeJson {
    id: i64,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    children: Vec<i64>,
    status: String,
    reweight: f64,
    device_class: String,
    crush_weight: f64,
}

/// `ceph osd tree --format json`의 OSD들(id 순): 호스트, up(`status`), in(`reweight > 0`), 클래스, CRUSH
/// 무게. 장치와 바이트는 비어 있다([`report`]가 채운다).
pub fn parse_osd_tree(out: &[u8]) -> Result<Vec<OsdReport>, String> {
    let tree: TreeJson = parse(out, "ceph osd tree")?;
    let hosts: BTreeMap<i64, &str> = tree
        .nodes
        .iter()
        .filter(|n| n.kind == "host")
        .flat_map(|h| h.children.iter().map(|c| (*c, h.name.as_str())))
        .collect();
    let mut osds: Vec<OsdReport> = tree
        .nodes
        .iter()
        .filter(|n| n.kind == "osd" && n.id >= 0)
        .map(|n| OsdReport {
            id: n.id as u32,
            host: hosts.get(&n.id).copied().unwrap_or_default().to_string(),
            up: n.status == "up",
            in_: n.reweight > 0.0,
            class: Some(n.device_class.clone()).filter(|c| !c.is_empty()),
            weight: n.crush_weight,
            device: None,
            total_bytes: 0,
            used_bytes: 0,
        })
        .collect();
    osds.sort_by_key(|o| o.id);
    Ok(osds)
}

/// OSD 하나의 크기와 쓴 바이트(`ceph osd df`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsdUsage {
    pub id: u32,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OsdDfJson {
    nodes: Vec<OsdDfNodeJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OsdDfNodeJson {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
    kb: u64,
    kb_used: u64,
}

/// `ceph osd df --format json`(KiB 단위)
pub fn parse_osd_df(out: &[u8]) -> Result<Vec<OsdUsage>, String> {
    let df: OsdDfJson = parse(out, "ceph osd df")?;
    Ok(df
        .nodes
        .into_iter()
        .filter(|n| n.kind == "osd" && n.id >= 0)
        .map(|n| OsdUsage {
            id: n.id as u32,
            total_bytes: n.kb.saturating_mul(1024),
            used_bytes: n.kb_used.saturating_mul(1024),
        })
        .collect())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DfJson {
    stats: DfStatsJson,
    pools: Vec<DfPoolJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DfStatsJson {
    total_bytes: u64,
    total_avail_bytes: u64,
    total_used_raw_bytes: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DfPoolJson {
    name: String,
    id: u64,
    stats: DfPoolStatsJson,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DfPoolStatsJson {
    stored: u64,
    max_avail: u64,
    percent_used: f64,
}

/// `ceph df`의 전체 사용량과 풀들
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DfReport {
    pub usage: Usage,
    /// `size`·`orbivisor_pool`은 비어 있다([`report`]가 채운다).
    pub pools: Vec<PoolReport>,
}

/// `ceph df --format json`
pub fn parse_df(out: &[u8]) -> Result<DfReport, String> {
    let df: DfJson = parse(out, "ceph df")?;
    Ok(DfReport {
        usage: Usage {
            total_bytes: df.stats.total_bytes,
            used_bytes: df.stats.total_used_raw_bytes,
            available_bytes: df.stats.total_avail_bytes,
        },
        pools: df
            .pools
            .into_iter()
            .map(|p| PoolReport {
                name: p.name,
                id: p.id,
                size: None,
                stored_bytes: p.stats.stored,
                max_available_bytes: p.stats.max_avail,
                percent_used: p.stats.percent_used,
                orbivisor_pool: None,
            })
            .collect(),
    })
}

/// OSD 하나의 메타데이터(`ceph osd metadata`)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OsdMetadata {
    pub id: u32,
    pub hostname: String,
    /// 장치(`/dev/sdb`). ceph-volume LVM OSD면 LV 아래의 디스크다.
    pub devices: Vec<String>,
    /// 장치 식별자(`sdb=QEMU_HARDDISK_drive-scsi1`의 `=` 뒤)
    pub device_ids: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MetadataJson {
    id: i64,
    hostname: String,
    devices: String,
    device_ids: String,
}

/// 쉼표로 이은 값(빈 값은 뺀다)
fn comma_list(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// `ceph osd metadata --format json`(모든 OSD의 배열)
pub fn parse_osd_metadata(out: &[u8]) -> Result<Vec<OsdMetadata>, String> {
    let all: Vec<MetadataJson> = parse(out, "ceph osd metadata")?;
    Ok(all
        .into_iter()
        .filter(|m| m.id >= 0)
        .map(|m| OsdMetadata {
            id: m.id as u32,
            hostname: m.hostname,
            devices: comma_list(&m.devices)
                .map(|d| format!("/dev/{d}"))
                .collect(),
            device_ids: comma_list(&m.device_ids)
                .map(|d| d.split_once('=').map_or(d, |(_, id)| id).to_string())
                .collect(),
        })
        .collect())
}

/// Ceph 풀 하나(`ceph osd pool ls`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLs {
    pub name: String,
    /// 복제 수(`ls detail`일 때만)
    pub size: Option<u32>,
    /// 풀을 쓰는 application(`rbd`, `cephfs`, `rgw`, `mgr` — `ls detail`의 `application_metadata`, 이름 순)
    pub applications: Vec<String>,
    /// 이 풀을 만든 Orbivisor RBD 풀 이름(`application_metadata.rbd` 의 [`ORBIVISOR_POOL_KEY`] 표시, `ls detail`일
    /// 때만)
    pub orbivisor_pool: Option<String>,
}

/// Orbivisor가 만든 Ceph 풀에 다는 rbd application 메타데이터 키(값은 Orbivisor RBD 풀 이름)
pub const ORBIVISOR_POOL_KEY: &str = "orbivisor-pool";

/// `ceph osd pool ls --format json`(이름 배열) 또는 `ceph osd pool ls detail --format json`(객체 배열)
pub fn parse_pool_ls(out: &[u8]) -> Result<Vec<PoolLs>, String> {
    let all: Vec<Value> = parse(out, "ceph osd pool ls")?;
    Ok(all
        .iter()
        .filter_map(|v| match v {
            Value::String(name) => Some(PoolLs {
                name: name.clone(),
                size: None,
                applications: Vec::new(),
                orbivisor_pool: None,
            }),
            Value::Object(o) => Some(PoolLs {
                name: o.get("pool_name")?.as_str()?.to_string(),
                size: o
                    .get("size")
                    .and_then(Value::as_u64)
                    .and_then(|s| u32::try_from(s).ok()),
                applications: o
                    .get("application_metadata")
                    .and_then(Value::as_object)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default(),
                orbivisor_pool: o
                    .get("application_metadata")
                    .and_then(|m| m.get("rbd"))
                    .and_then(|m| m.get(ORBIVISOR_POOL_KEY))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
            _ => None,
        })
        .collect())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PgStatJson {
    pg_summary: PgSummaryJson,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PgSummaryJson {
    num_pg_by_state: Vec<PgByStateJson>,
    num_pgs: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PgByStateJson {
    name: String,
    num: u64,
}

/// `ceph pg stat --format json`의 (모든 PG 수, `active`인 PG 수)
pub fn parse_pg_stat(out: &[u8]) -> Result<(u64, u64), String> {
    let s: PgStatJson = parse(out, "ceph pg stat")?;
    let active = s
        .pg_summary
        .num_pg_by_state
        .iter()
        .filter(|p| p.name.split('+').any(|s| s == "active"))
        .map(|p| p.num)
        .sum();
    Ok((s.pg_summary.num_pgs, active))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PgLsJson {
    pg_stats: Vec<PgLsEntryJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PgLsEntryJson {
    state: String,
}

/// `ceph pg ls-by-pool {풀} --format json`의 (그 풀의 PG 수, `active`인 PG 수)
pub fn parse_pg_ls(out: &[u8]) -> Result<(u64, u64), String> {
    let ls: PgLsJson = parse(out, "ceph pg ls-by-pool")?;
    let active = ls
        .pg_stats
        .iter()
        .filter(|p| p.state.split('+').any(|s| s == "active"))
        .count();
    Ok((ls.pg_stats.len() as u64, active as u64))
}

/// ceph-volume이 만든 OSD 하나(`ceph-volume lvm list`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeOsd {
    pub id: u32,
    /// OSD의 fsid
    pub fsid: String,
    /// OSD가 속한 Ceph 클러스터의 fsid
    pub cluster_fsid: String,
    /// LV 아래의 장치(`/dev/sdb`)
    pub devices: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct VolumeJson {
    #[serde(rename = "type")]
    kind: String,
    devices: Vec<String>,
    tags: BTreeMap<String, String>,
}

/// `ceph-volume lvm list --format json`(OSD id → LV들)의 OSD들((id, fsid) 순). 같은 id 아래 LV라도 다른 클러스터의
/// 다른 OSD일 수 있으므로 `ceph.osd_fsid`로 묶고, 묶음마다 `block` LV의 클러스터와 그 묶음의 장치를 본다.
pub fn parse_volume_list(out: &[u8]) -> Result<Vec<VolumeOsd>, String> {
    let all: BTreeMap<String, Vec<VolumeJson>> = parse(out, "ceph-volume lvm list")?;
    let mut osds = Vec::new();
    for (id, lvs) in all {
        let Ok(id) = id.parse() else {
            continue;
        };
        let mut groups: BTreeMap<String, Vec<&VolumeJson>> = BTreeMap::new();
        for lv in &lvs {
            let fsid = lv.tags.get("ceph.osd_fsid").cloned().unwrap_or_default();
            groups.entry(fsid).or_default().push(lv);
        }
        for (fsid, lvs) in groups {
            let Some(block) = lvs.iter().find(|lv| lv.kind == "block").or(lvs.first()) else {
                continue;
            };
            let devices: BTreeSet<String> = lvs
                .iter()
                .flat_map(|lv| lv.devices.iter().cloned())
                .collect();
            osds.push(VolumeOsd {
                id,
                fsid,
                cluster_fsid: block
                    .tags
                    .get("ceph.cluster_fsid")
                    .cloned()
                    .unwrap_or_default(),
                devices: devices.into_iter().collect(),
            });
        }
    }
    osds.sort_by(|a, b| (a.id, &a.fsid).cmp(&(b.id, &b.fsid)));
    Ok(osds)
}

/// 설치된 Ceph의 판(`ceph --version`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CephVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// `squid`, `reef`(모르면 빈 문자열)
    pub codename: String,
}

impl CephVersion {
    /// `CephStatus.release`에 적는 모양(`19.2.1 squid`)
    pub fn release(&self) -> String {
        let numbers = format!("{}.{}.{}", self.major, self.minor, self.patch);
        if self.codename.is_empty() {
            numbers
        } else {
            format!("{numbers} {}", self.codename)
        }
    }
}

/// 앞의 숫자들(`3~git2024` → 3)
fn leading_number(s: &str) -> Option<u32> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s[..end].parse().ok()
}

/// `ceph version 19.2.1 (58a7fab8be0a062d730ad7da874972fd3fba59fb) squid (stable)`
pub fn parse_version(line: &str) -> Option<CephVersion> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let at = words.windows(2).position(|w| w == ["ceph", "version"])?;
    let mut parts = words.get(at + 2)?.splitn(3, '.');
    let major = leading_number(parts.next()?)?;
    let minor = leading_number(parts.next()?)?;
    let patch = leading_number(parts.next()?)?;
    // 이름은 커밋 해시(`(…)`) 다음 낱말이다.
    let codename = words[at + 3..]
        .iter()
        .position(|w| w.starts_with('(') && w.ends_with(')'))
        .and_then(|i| words.get(at + 3 + i + 1))
        .filter(|w| !w.starts_with('('))
        .map_or_else(String::new, |w| w.to_string());
    Some(CephVersion {
        major,
        minor,
        patch,
        codename,
    })
}

/// 상태 보고서에 쓰는 명령 출력들(모두 `--format json`)
#[derive(Debug, Clone, Copy)]
pub struct ReportInput<'a> {
    /// `ceph status`
    pub status: &'a [u8],
    /// `ceph quorum_status`(mon과 주소 — 19.2의 `status`에는 mon 목록이 없다)
    pub quorum_status: &'a [u8],
    /// `ceph mgr dump`(active·standby 이름 — 19.2의 `status`에는 없다)
    pub mgr_dump: &'a [u8],
    /// `ceph osd tree`
    pub osd_tree: &'a [u8],
    /// `ceph osd df`
    pub osd_df: &'a [u8],
    /// `ceph df`
    pub df: &'a [u8],
    /// `ceph osd metadata`
    pub osd_metadata: &'a [u8],
    /// `ceph osd pool ls detail`
    pub pool_ls: &'a [u8],
}

/// 명령 출력들을 상태 보고서로 합친다. `pools`는 (Orbivisor 풀 이름, Ceph 풀 이름)이다. `read_at`·`via`는
/// 부르는 쪽이 채운다.
pub fn report(input: &ReportInput<'_>, pools: &[(String, String)]) -> Result<CephReport, String> {
    let status = parse_status(input.status)?;
    let mut mons = parse_monmap(input.quorum_status)?;
    for mon in &mut mons {
        mon.in_quorum = status.quorum.contains(&mon.name);
    }
    let mut mgr = parse_mgr_dump(input.mgr_dump)?;
    mgr.available |= status.mgr.available;

    let usage: BTreeMap<u32, OsdUsage> = parse_osd_df(input.osd_df)?
        .into_iter()
        .map(|u| (u.id, u))
        .collect();
    let metadata: BTreeMap<u32, OsdMetadata> = parse_osd_metadata(input.osd_metadata)?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    let mut osds = parse_osd_tree(input.osd_tree)?;
    for osd in &mut osds {
        if let Some(u) = usage.get(&osd.id) {
            osd.total_bytes = u.total_bytes;
            osd.used_bytes = u.used_bytes;
        }
        if let Some(m) = metadata.get(&osd.id).filter(|m| !m.devices.is_empty()) {
            osd.device = Some(m.devices.join(","));
        }
    }
    let osd_hosts_up = osds
        .iter()
        .filter(|o| o.up)
        .map(|o| o.host.as_str())
        .collect::<BTreeSet<_>>()
        .len() as u32;

    let sizes: BTreeMap<String, Option<u32>> = parse_pool_ls(input.pool_ls)?
        .into_iter()
        .map(|p| (p.name, p.size))
        .collect();
    let df = parse_df(input.df)?;
    let pools = df
        .pools
        .into_iter()
        .map(|mut p| {
            p.size = sizes.get(&p.name).copied().flatten();
            p.orbivisor_pool = pools
                .iter()
                .find(|(_, ceph)| *ceph == p.name)
                .map(|(orbivisor, _)| orbivisor.clone());
            p
        })
        .collect();

    Ok(CephReport {
        health: status.health,
        mons,
        quorum: status.quorum,
        mgr,
        osds,
        osd_hosts_up,
        pgs: status.pgs,
        usage: df.usage,
        pools,
        read_at: 0,
        via: uuid::Uuid::nil(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS_OK: &[u8] = include_bytes!("fixtures/status-ok.json");
    const STATUS_WARN: &[u8] = include_bytes!("fixtures/status-warn.json");
    const QUORUM: &[u8] = include_bytes!("fixtures/quorum-status.json");
    const MGR_DUMP: &[u8] = include_bytes!("fixtures/mgr-dump.json");
    const MGR_STAT: &[u8] = include_bytes!("fixtures/mgr-stat.json");
    const OSD_TREE: &[u8] = include_bytes!("fixtures/osd-tree.json");
    const OSD_DF: &[u8] = include_bytes!("fixtures/osd-df.json");
    const DF: &[u8] = include_bytes!("fixtures/df.json");
    const METADATA: &[u8] = include_bytes!("fixtures/osd-metadata.json");
    const POOL_LS: &[u8] = include_bytes!("fixtures/pool-ls.json");
    const POOL_LS_DETAIL: &[u8] = include_bytes!("fixtures/pool-ls-detail.json");
    const PG_STAT: &[u8] = include_bytes!("fixtures/pg-stat.json");
    const PG_LS: &[u8] = include_bytes!("fixtures/pg-ls-by-pool.json");
    const VOLUME_LIST: &[u8] = include_bytes!("fixtures/ceph-volume-list.json");

    #[test]
    fn parses_a_healthy_status() {
        let s = parse_status(STATUS_OK).unwrap();
        assert_eq!(s.health.status, "HEALTH_OK");
        assert!(s.health.checks.is_empty());
        assert_eq!(s.quorum, ["node1", "node2", "node3"]);
        assert_eq!((s.num_osds, s.num_up_osds, s.num_in_osds), (4, 4, 4));
        assert_eq!(s.pgs.total, 129);
        assert_eq!(s.pgs.states, [("active+clean".to_string(), 129)]);
        // 19.2의 mgrmap에는 이름이 없다.
        assert_eq!(
            s.mgr,
            MgrReport {
                active: None,
                standbys: vec![],
                available: true
            }
        );
    }

    /// 검사는 코드 순이고 요약 문구·수·muted를 그대로 담는다.
    #[test]
    fn parses_health_checks_of_a_warning_status() {
        let s = parse_status(STATUS_WARN).unwrap();
        assert_eq!(s.health.status, "HEALTH_WARN");
        let check = |code: &str, summary: &str, count: u64, muted: bool| CephHealthCheck {
            code: code.into(),
            severity: "HEALTH_WARN".into(),
            summary: summary.into(),
            count,
            muted,
        };
        assert_eq!(
            s.health.checks,
            [
                check("MON_DOWN", "1/3 mons down, quorum node1,node2", 1, true),
                check(
                    "POOL_NO_REDUNDANCY",
                    "2 pool(s) have no replicas configured",
                    2,
                    false
                ),
                check(
                    "TOO_FEW_OSDS",
                    "OSD count 1 < osd_pool_default_size 3",
                    1,
                    false
                ),
            ]
        );
        assert_eq!(s.quorum, ["node1", "node2"]);
        assert_eq!(
            s.pgs.states,
            [
                ("active+clean".to_string(), 30),
                ("active+undersized".to_string(), 2),
                ("peering".to_string(), 1),
            ]
        );
    }

    /// 빠진 필드는 기본값이고, JSON이 아니면 오류다.
    #[test]
    fn missing_fields_default_and_bad_output_fails() {
        let s = parse_status(br#"{"health":{"status":"HEALTH_ERR","checks":{"X":{}}}}"#).unwrap();
        assert_eq!(s.health.status, "HEALTH_ERR");
        assert_eq!(
            s.health.checks,
            [CephHealthCheck {
                code: "X".into(),
                ..Default::default()
            }]
        );
        assert_eq!(s.pgs, PgReport::default());
        assert_eq!(parse_status(b"{}").unwrap(), StatusSummary::default());
        let err = parse_status(b"Error initializing cluster client").unwrap_err();
        assert!(err.starts_with("unexpected ceph status output: "), "{err}");
        assert!(parse_osd_tree(b"").is_err());
        assert!(parse_volume_list(b"[]").is_err());
        assert_eq!(parse_mgr_dump(b"{}").unwrap(), MgrReport::default());
        assert_eq!(parse_df(b"{}").unwrap(), DfReport::default());
        assert_eq!(parse_pg_stat(b"{}").unwrap(), (0, 0));
    }

    #[test]
    fn parses_mgr_dump_and_stat() {
        assert_eq!(
            parse_mgr_dump(MGR_DUMP).unwrap(),
            MgrReport {
                active: Some("node1".into()),
                standbys: vec!["node2".into()],
                available: true
            }
        );
        let stat = parse_mgr_dump(MGR_STAT).unwrap();
        assert_eq!(stat.active.as_deref(), Some("ip-10-0-136-232"));
        assert!(stat.available && stat.standbys.is_empty());
        let none = parse_mgr_dump(br#"{"available":false,"active_name":""}"#).unwrap();
        assert_eq!(none, MgrReport::default());
    }

    #[test]
    fn parses_quorum_and_monitors() {
        assert_eq!(parse_quorum(QUORUM).unwrap(), ["node1", "node2"]);
        assert!(parse_quorum(br#"{"quorum_names":[]}"#).unwrap().is_empty());
        let mon = |name: &str, addr: &str, in_quorum| MonReport {
            name: name.into(),
            addr: addr.into(),
            in_quorum,
        };
        assert_eq!(
            parse_monmap(QUORUM).unwrap(),
            [
                mon("node1", "10.0.0.1:3300", true),
                mon("node2", "10.0.0.2:3300", true),
                mon("node3", "[fd00::3]:3300", false),
            ]
        );
        // `mon dump`(monmap이 맨 위), v2가 없는 mon과 옛 주소만 있는 mon
        let dump = br#"{"epoch":1,"mons":[
            {"name":"a","public_addrs":{"addrvec":[{"type":"v1","addr":"10.0.0.9:6789","nonce":0}]}},
            {"name":"b","public_addr":"10.0.0.8:6789/0"}],"quorum":[0]}"#;
        assert_eq!(
            parse_monmap(dump).unwrap(),
            [
                mon("a", "10.0.0.9:6789", false),
                mon("b", "10.0.0.8:6789", false)
            ]
        );
    }

    /// 호스트 둘·OSD 넷: osd.1은 down, osd.3은 out(`reweight` 0)이다.
    #[test]
    fn parses_the_osd_tree() {
        let osds = parse_osd_tree(OSD_TREE).unwrap();
        let summary: Vec<_> = osds
            .iter()
            .map(|o| (o.id, o.host.as_str(), o.up, o.in_, o.class.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                (0, "node1", true, true, Some("hdd")),
                (1, "node1", false, true, Some("hdd")),
                (2, "node2", true, true, Some("ssd")),
                (3, "node2", true, false, Some("ssd")),
            ]
        );
        assert!((osds[0].weight - 0.009796142578125).abs() < 1e-12);
        assert_eq!((osds[0].device.as_ref(), osds[0].total_bytes), (None, 0));
        // 호스트 밖 OSD·클래스 없음
        let bare = parse_osd_tree(
            br#"{"nodes":[{"id":5,"type":"osd","status":"up","reweight":1,"device_class":""}]}"#,
        )
        .unwrap();
        assert_eq!((bare[0].host.as_str(), bare[0].class.as_ref()), ("", None));
    }

    #[test]
    fn parses_osd_df_in_bytes() {
        let usage = parse_osd_df(OSD_DF).unwrap();
        assert_eq!(usage.len(), 4);
        assert_eq!(
            usage[0],
            OsdUsage {
                id: 0,
                total_bytes: 10 << 30,
                used_bytes: 314572 * 1024
            }
        );
        assert_eq!(usage[1].total_bytes, 0);
    }

    #[test]
    fn parses_df() {
        let df = parse_df(DF).unwrap();
        assert_eq!(
            df.usage,
            Usage {
                total_bytes: 42949672960,
                used_bytes: 1287952384,
                available_bytes: 41661720576
            }
        );
        let names: Vec<_> = df.pools.iter().map(|p| (p.name.as_str(), p.id)).collect();
        assert_eq!(names, [(".mgr", 1), ("vmpool", 2)]);
        let vm = &df.pools[1];
        assert_eq!(
            (vm.stored_bytes, vm.max_available_bytes),
            (831522816, 13186596864)
        );
        assert!((vm.percent_used - 0.059316).abs() < 1e-6);
        assert_eq!((vm.size, vm.orbivisor_pool.as_ref()), (None, None));
    }

    #[test]
    fn parses_osd_metadata() {
        let all = parse_osd_metadata(METADATA).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(
            all[0],
            OsdMetadata {
                id: 0,
                hostname: "node1".into(),
                devices: vec!["/dev/sdb".into()],
                device_ids: vec!["QEMU_HARDDISK_drive-scsi1".into()],
            }
        );
        assert!(all[1].device_ids.is_empty());
        assert_eq!(all[3].devices, ["/dev/loop2"]);
        let two = parse_osd_metadata(br#"[{"id":9,"devices":"sdb,sdc","device_ids":""}]"#).unwrap();
        assert_eq!(two[0].devices, ["/dev/sdb", "/dev/sdc"]);
    }

    #[test]
    fn parses_pool_lists_with_and_without_detail() {
        let pool = |name: &str, size, apps: &[&str]| PoolLs {
            name: name.into(),
            size,
            applications: apps.iter().map(|a| a.to_string()).collect(),
            orbivisor_pool: None,
        };
        assert_eq!(
            parse_pool_ls(POOL_LS).unwrap(),
            [pool(".mgr", None, &[]), pool("vmpool", None, &[])]
        );
        assert_eq!(
            parse_pool_ls(POOL_LS_DETAIL).unwrap(),
            [
                pool(".mgr", Some(2), &["mgr"]),
                pool("vmpool", Some(3), &["rbd"])
            ]
        );
        // application이 없는(막 만든) 풀과 여럿인 풀
        assert_eq!(
            parse_pool_ls(
                br#"[{"pool_name":"a","size":1,"application_metadata":{}},{"pool_name":"b","application_metadata":{"rgw":{},"cephfs":{}}}]"#
            )
            .unwrap(),
            [pool("a", Some(1), &[]), pool("b", None, &["cephfs", "rgw"])]
        );
        // Orbivisor가 만든 풀의 표시
        let marked = parse_pool_ls(
            br#"[{"pool_name":"vms","application_metadata":{"rbd":{"orbivisor-pool":"vms"}}}]"#,
        )
        .unwrap();
        assert_eq!(
            marked,
            [PoolLs {
                orbivisor_pool: Some("vms".into()),
                ..pool("vms", None, &["rbd"])
            }]
        );
        assert!(parse_pool_ls(b"[]").unwrap().is_empty());
    }

    /// `active`가 든 상태(`active+undersized+degraded` 포함)만 센다.
    #[test]
    fn parses_pg_stat() {
        assert_eq!(parse_pg_stat(PG_STAT).unwrap(), (33, 32));
        let all = br#"{"pg_ready":true,"pg_summary":{"num_pg_by_state":[{"name":"active+clean","num":5}],"num_pgs":5}}"#;
        assert_eq!(parse_pg_stat(all).unwrap(), (5, 5));
        let inactive =
            br#"{"pg_summary":{"num_pg_by_state":[{"name":"inactive","num":2}],"num_pgs":2}}"#;
        assert_eq!(parse_pg_stat(inactive).unwrap(), (2, 0));
    }

    /// 그 풀의 PG만 센다. mgr가 아직 풀을 모르면 빈 목록이다.
    #[test]
    fn parses_pg_ls_by_pool() {
        assert_eq!(parse_pg_ls(PG_LS).unwrap(), (3, 2));
        assert_eq!(
            parse_pg_ls(br#"{"pg_ready":true,"pg_stats":[]}"#).unwrap(),
            (0, 0)
        );
        assert!(parse_pg_ls(b"Error ENOENT: pool vmpool does not exist").is_err());
    }

    #[test]
    fn parses_the_ceph_volume_list() {
        assert_eq!(
            parse_volume_list(VOLUME_LIST).unwrap(),
            [
                VolumeOsd {
                    id: 0,
                    fsid: "0851efa7-c274-4d7d-a734-b3a881914253".into(),
                    cluster_fsid: "18796e2f-112c-44cf-9d48-09f825f0a14b".into(),
                    devices: vec!["/dev/sdn".into()],
                },
                VolumeOsd {
                    id: 7,
                    fsid: "5d1c0f8e-6a3b-4c1e-9f0a-2b7d8e9c1a24".into(),
                    cluster_fsid: "0b5c6f39-0000-4000-8000-00000000c0de".into(),
                    devices: vec!["/dev/sdm".into()],
                },
            ]
        );
        // OSD가 없으면 빈 객체다.
        assert!(parse_volume_list(b"{}").unwrap().is_empty());

        // 두 클러스터가 같은 id(osd.0)를 쓴다: OSD fsid로 나누고 저마다 자기 클러스터·장치를 갖는다. 같은 OSD의
        // db LV(다른 장치)는 그 OSD에 묶인다.
        let lv = |kind: &str, dev: &str, osd: &str, cluster: &str| {
            format!(
                r#"{{"devices":["{dev}"],"type":"{kind}","tags":{{"ceph.cluster_fsid":"{cluster}","ceph.osd_fsid":"{osd}","ceph.osd_id":"0"}}}}"#
            )
        };
        let out = format!(
            r#"{{"0":[{},{},{}]}}"#,
            lv("block", "/dev/sdb", "bbbb", "c1"),
            lv("db", "/dev/nvme0n1", "bbbb", "c1"),
            lv("block", "/dev/sdc", "aaaa", "c2"),
        );
        let osd = |fsid: &str, cluster: &str, devices: &[&str]| VolumeOsd {
            id: 0,
            fsid: fsid.into(),
            cluster_fsid: cluster.into(),
            devices: devices.iter().map(|d| d.to_string()).collect(),
        };
        assert_eq!(
            parse_volume_list(out.as_bytes()).unwrap(),
            [
                osd("aaaa", "c2", &["/dev/sdc"]),
                osd("bbbb", "c1", &["/dev/nvme0n1", "/dev/sdb"]),
            ]
        );
    }

    #[test]
    fn parses_ceph_versions() {
        let v = parse_version(
            "ceph version 19.2.1 (58a7fab8be0a062d730ad7da874972fd3fba59fb) squid (stable)",
        )
        .unwrap();
        assert_eq!(
            v,
            CephVersion {
                major: 19,
                minor: 2,
                patch: 1,
                codename: "squid".into()
            }
        );
        assert_eq!(v.release(), "19.2.1 squid");
        let reef = parse_version(
            "ceph version 18.2.7 (6b0e988052ec84cf2d4a54ff9bbbc5e720b621ad) reef (stable)\n",
        )
        .unwrap();
        assert_eq!((reef.major, reef.minor, reef.patch), (18, 2, 7));
        assert_eq!(reef.release(), "18.2.7 reef");
        // 배포판의 git 판, 해시·이름이 없는 줄
        let git = parse_version("ceph version 19.2.0~git20240301.4c76c50 (4c76c50) squid (stable)")
            .unwrap();
        assert_eq!(git.release(), "19.2.0 squid");
        let bare = parse_version("ceph version 17.2.6").unwrap();
        assert_eq!(bare.release(), "17.2.6");
        for bad in [
            "",
            "ceph version",
            "ceph version x.y.z (h) squid",
            "rbd 1.2.3",
        ] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    fn input() -> ReportInput<'static> {
        ReportInput {
            status: STATUS_OK,
            quorum_status: QUORUM,
            mgr_dump: MGR_DUMP,
            osd_tree: OSD_TREE,
            osd_df: OSD_DF,
            df: DF,
            osd_metadata: METADATA,
            pool_ls: POOL_LS_DETAIL,
        }
    }

    /// OSD는 호스트·장치·바이트를 함께 담고, up OSD가 있는 호스트만 센다. Ceph 풀은 그것을 쓰는
    /// Orbivisor 풀과 잇는다.
    #[test]
    fn report_combines_the_outputs() {
        let pools = [("vms".to_string(), "vmpool".to_string())];
        let r = report(&input(), &pools).unwrap();
        assert_eq!(r.health.status, "HEALTH_OK");
        assert_eq!(r.quorum, ["node1", "node2", "node3"]);
        // 정족수는 `status`의 것이다(`quorum_status`보다 나중에 읽혔을 수 있다).
        assert!(r.mons.iter().all(|m| m.in_quorum), "{:?}", r.mons);
        assert_eq!(r.mgr.active.as_deref(), Some("node1"));
        assert_eq!(r.mgr.standbys, ["node2"]);

        let osds: Vec<_> = r
            .osds
            .iter()
            .map(|o| (o.id, o.host.as_str(), o.device.as_deref(), o.total_bytes))
            .collect();
        assert_eq!(
            osds,
            [
                (0, "node1", Some("/dev/sdb"), 10 << 30),
                (1, "node1", Some("/dev/sdc"), 0),
                (2, "node2", Some("/dev/nvme0n1"), 10 << 30),
                (3, "node2", Some("/dev/loop2"), 10 << 30),
            ]
        );
        assert_eq!(r.osd_hosts_up, 2);
        assert_eq!(r.pgs.total, 129);
        assert_eq!(r.usage.total_bytes, 42949672960);

        let p: Vec<_> = r
            .pools
            .iter()
            .map(|p| (p.name.as_str(), p.size, p.orbivisor_pool.as_deref()))
            .collect();
        assert_eq!(
            p,
            [(".mgr", Some(2), None), ("vmpool", Some(3), Some("vms"))]
        );
        assert_eq!((r.read_at, r.via), (0, uuid::Uuid::nil()));
    }

    /// down OSD만 있는 호스트는 세지 않는다.
    #[test]
    fn hosts_with_only_down_osds_do_not_count() {
        let tree = br#"{"nodes":[
            {"id":-2,"name":"node1","type":"host","children":[0]},
            {"id":-3,"name":"node2","type":"host","children":[1,2]},
            {"id":0,"name":"osd.0","type":"osd","status":"up","reweight":1},
            {"id":1,"name":"osd.1","type":"osd","status":"down","reweight":1},
            {"id":2,"name":"osd.2","type":"osd","status":"down","reweight":0}]}"#;
        let r = report(
            &ReportInput {
                osd_tree: tree,
                ..input()
            },
            &[],
        )
        .unwrap();
        assert_eq!(r.osds.len(), 3);
        assert_eq!(r.osd_hosts_up, 1);
        assert!(r.pools.iter().all(|p| p.orbivisor_pool.is_none()));
    }

    #[test]
    fn report_fails_when_an_output_is_not_json() {
        let err = report(
            &ReportInput {
                df: b"timed out",
                ..input()
            },
            &[],
        )
        .unwrap_err();
        assert!(err.starts_with("unexpected ceph df output"), "{err}");
    }
}
