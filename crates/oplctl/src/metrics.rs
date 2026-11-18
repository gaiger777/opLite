//! 지표 기록 요약(계획 60): `node stats`·`vm stats`·`pool stats`의 `--range`·`--from`·`--until`, 클러스터 모으기 요약,
//! `prometheus enable|disable|show`. 표는 칸마다 범위의 AVG·MAX·MIN과 p50·p95이고(근사면 머리가 `P50~`·`P95~`),
//! `-o json`은 응답 그대로다.

use std::io::Write;

use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use opl_types::metrics::{ClusterHistoryResponse, HistoryRange, HistoryReply};
use reqwest::Method;
use serde_json::json;

use crate::client::Client;
use crate::output::{bytes, json as print_json, table};

/// 기록 조회 범위. `--range`이거나 `--from`(과 `--until`)이다.
#[derive(Args, Debug, Default, Clone)]
pub(crate) struct RangeArgs {
    /// 지금에서 끝나는 범위(기록 요약 표). 없으면 마지막 점이다.
    #[arg(long, value_enum, conflicts_with_all = ["from", "until"])]
    pub range: Option<RangeArg>,
    /// 범위 시작: `2h`·`7d`(지금에서 뺌), 유닉스 밀리초나 RFC 3339
    #[arg(long)]
    pub from: Option<String>,
    /// 범위 끝(`--from`과 함께, 없으면 지금): `--from`과 같은 형식
    #[arg(long, requires = "from")]
    pub until: Option<String>,
    /// 돌려받을 점 수의 상한(1~1000, 기본 300, 클러스터 120). 표의 요약 값은 점 수와 상관없다.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub points: Option<u16>,
}

impl RangeArgs {
    /// 기록 조회를 하는지(`--range`·`--from`·`--points` 가운데 하나. `--points`만이면 지난 1시간)
    pub fn given(&self) -> bool {
        self.range.is_some() || self.from.is_some() || self.points.is_some()
    }

    /// API 질의(`range`·`from`·`until`·`points`). 시각은 유닉스 밀리초로 바꿔 보낸다. `--until` 없는 `--from`이
    /// 기간이면 서버 시계로 세도록 같은 길이의 `range`(`1h`·`1d`·`7d`·`30d`·`365d`)로 보내고, 그 밖의 기간이면 `until`도
    /// 이 시계로 보내 범위 길이가 두 시계의 차이로 바뀌지 않게 한다.
    pub fn query(&self, now_ms: i64) -> anyhow::Result<Vec<(&'static str, String)>> {
        let mut q = Vec::new();
        let relative = (self.from.as_deref())
            .filter(|_| self.until.is_none())
            .and_then(duration_ms);
        let range = self.range.map(HistoryRange::from).or_else(|| {
            let ranges = [
                HistoryRange::Hour,
                HistoryRange::Day,
                HistoryRange::Week,
                HistoryRange::Month,
                HistoryRange::Year,
            ];
            ranges.into_iter().find(|r| Some(r.ms()) == relative)
        });
        if let Some(range) = range {
            q.push(("range", range.as_str().to_string()));
        } else {
            if let Some(from) = &self.from {
                q.push(("from", time_ms(from, now_ms)?.to_string()));
            }
            match &self.until {
                Some(until) => q.push(("until", time_ms(until, now_ms)?.to_string())),
                None if relative.is_some() => q.push(("until", now_ms.to_string())),
                None => {}
            }
        }
        if let Some(points) = self.points {
            q.push(("points", points.to_string()));
        }
        Ok(q)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RangeArg {
    Hour,
    Day,
    Week,
    /// 30일
    Month,
    /// 365일
    Year,
}

impl From<RangeArg> for HistoryRange {
    fn from(r: RangeArg) -> Self {
        match r {
            RangeArg::Hour => Self::Hour,
            RangeArg::Day => Self::Day,
            RangeArg::Week => Self::Week,
            RangeArg::Month => Self::Month,
            RangeArg::Year => Self::Year,
        }
    }
}

/// 지금(유닉스 밀리초)
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `30s`·`5m`·`2h`·`7d` 같은 기간의 밀리초. 단위가 없거나 수가 숫자만이 아니면(`-2h`·`+2h`) None
pub(crate) fn duration_ms(s: &str) -> Option<i64> {
    let unit = match s.chars().last()? {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i64>().ok()?.checked_mul(unit)
}

/// 시각 인자: 기간(지금에서 뺌), 유닉스 밀리초, RFC 3339
pub(crate) fn time_ms(s: &str, now_ms: i64) -> anyhow::Result<i64> {
    if let Some(ms) = duration_ms(s) {
        return Ok(now_ms - ms);
    }
    if let Ok(ms) = s.parse::<i64>() {
        return Ok(ms);
    }
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .with_context(|| {
            format!("{s:?} is not a duration (2h), unix milliseconds or an RFC 3339 time")
        })?;
    Ok((t.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn ms_time(ms: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000)) {
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

/// 밀리초 간격을 `5s`·`1m`·`30m`·`6h`처럼
fn step_text(ms: i64) -> String {
    match ms {
        ms if ms % 86_400_000 == 0 => format!("{}d", ms / 86_400_000),
        ms if ms % 3_600_000 == 0 => format!("{}h", ms / 3_600_000),
        ms if ms % 60_000 == 0 => format!("{}m", ms / 60_000),
        ms if ms % 1_000 == 0 => format!("{}s", ms / 1_000),
        ms => format!("{ms}ms"),
    }
}

/// 칸 표시 순서(노드·VM·풀 칸)
const FIELD_ORDER: &[&str] = &[
    "cpu",
    "mem_used",
    "mem_total",
    "mem_rss",
    "disk_read",
    "disk_write",
    "disk_read_iops",
    "disk_write_iops",
    "net_rx",
    "net_tx",
    "fs_used",
    "fs_total",
    "pool_used",
    "pool_total",
];

/// 칸 이름을 표시 순서로. 모르는 칸은 뒤에 이름 순으로
fn ordered<'a>(names: impl Iterator<Item = &'a String>) -> Vec<&'a str> {
    let mut names: Vec<&str> = names.map(String::as_str).collect();
    names.sort_by_key(|n| {
        (
            FIELD_ORDER
                .iter()
                .position(|f| f == n)
                .unwrap_or(usize::MAX),
            n.to_string(),
        )
    });
    names
}

/// 칸 값 하나를 단위에 맞게: cpu `%`, MiB 칸은 바이트, 비율은 `/s`, IOPS는 소수 한 자리
pub(crate) fn value_cell(field: &str, v: Option<f32>) -> String {
    let Some(v) = v.map(f64::from).filter(|v| v.is_finite()) else {
        return "-".into();
    };
    match field {
        "cpu" => format!("{v:.1}%"),
        "mem_used" | "mem_total" | "mem_rss" | "fs_used" | "fs_total" | "pool_used"
        | "pool_total" => bytes(&json!((v.max(0.0) * 1_048_576.0).round() as u64)),
        "disk_read" | "disk_write" | "net_rx" | "net_tx" => {
            format!("{}/s", bytes(&json!(v.max(0.0).round() as u64)))
        }
        _ => format!("{v:.1}"),
    }
}

/// 기록 답의 범위 줄: `2026-…Z .. 2026-…Z (tier min1, step 1m, 60 points)`
fn range_line(tier: &str, start: i64, step_ms: i64, points: usize) -> String {
    format!(
        "{} .. {} (tier {tier}, step {}, {points} points)",
        ms_time(start),
        ms_time(start + step_ms * points as i64),
        step_text(step_ms)
    )
}

/// 노드·VM·풀 기록 답의 요약 표. 칸마다 AVG·MAX·MIN과 p50·p95(근사면 머리에 `~`)
pub(crate) fn history_table(out: &mut dyn Write, reply: &HistoryReply) -> std::io::Result<()> {
    let points = reply
        .columns
        .values()
        .map(|c| c.avg.len())
        .max()
        .unwrap_or(0);
    writeln!(
        out,
        "{}",
        range_line(reply.tier.as_str(), reply.start, reply.step_ms, points)
    )?;
    let approx = reply.pct.values().any(|p| p.approx);
    let (p50, p95) = if approx {
        ("P50~", "P95~")
    } else {
        ("P50", "P95")
    };
    let rows: Vec<Vec<String>> = ordered(reply.columns.keys())
        .into_iter()
        .map(|f| {
            let stat = reply.stats.get(f).copied().unwrap_or_default();
            let pct = reply.pct.get(f).copied().unwrap_or_default();
            vec![
                f.to_string(),
                value_cell(f, stat.avg),
                value_cell(f, stat.max),
                value_cell(f, stat.min),
                value_cell(f, pct.p50),
                value_cell(f, pct.p95),
            ]
        })
        .collect();
    table(out, &["FIELD", "AVG", "MAX", "MIN", p50, p95], &rows)?;
    if let Some(from) = reply.covered_from {
        writeln!(out, "note: no history before {}", ms_time(from))?;
    }
    Ok(())
}

/// `GET {path}`를 범위 질의로 부르고 요약 표나(`-o json`) 응답 그대로 보인다.
pub(crate) async fn history(
    client: &Client,
    path: &str,
    range: &RangeArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let q = range.query(now_ms())?;
    let mut r = client.request(Method::GET, path);
    if !q.is_empty() {
        r = r.query(&q);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let reply: HistoryReply =
        serde_json::from_value(resp).context("unexpected metrics history reply")?;
    history_table(out, &reply)?;
    Ok(())
}

/// 열(AVG)의 아는 값의 평균과 최대
fn mean_peak(col: &[Option<f32>]) -> (Option<f32>, Option<f32>) {
    let known: Vec<f32> = col
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if known.is_empty() {
        return (None, None);
    }
    let mean = known.iter().sum::<f32>() / known.len() as f32;
    let peak = known.iter().copied().fold(f32::MIN, f32::max);
    (Some(mean), Some(peak))
}

/// 클러스터 모으기 요약: 클러스터 열(평균·봉우리), 노드 줄, 상위 VM, 풀 사용량. 답하지 않은 노드는 `err`에 알린다.
pub(crate) fn cluster_tables(
    out: &mut dyn Write,
    err: &mut dyn Write,
    c: &ClusterHistoryResponse,
) -> std::io::Result<()> {
    let points = c.columns.values().map(|c| c.avg.len()).max().unwrap_or(0);
    writeln!(
        out,
        "cluster ({} nodes): {}",
        c.node_count,
        range_line(c.tier.as_str(), c.start, c.step_ms, points)
    )?;
    let rows: Vec<Vec<String>> = ordered(c.columns.keys())
        .into_iter()
        .map(|f| {
            let (mean, peak) = mean_peak(&c.columns[f].avg);
            vec![f.to_string(), value_cell(f, mean), value_cell(f, peak)]
        })
        .collect();
    table(out, &["FIELD", "AVG", "PEAK"], &rows)?;

    writeln!(out)?;
    let rows: Vec<Vec<String>> = c
        .nodes
        .iter()
        .map(|n| {
            let avg = |f: &str| value_cell(f, n.avg.get(f).copied().flatten());
            vec![
                n.name.clone(),
                n.cpus.to_string(),
                avg("cpu"),
                format!("{}/{}", avg("mem_used"), avg("mem_total")),
                avg("disk_read"),
                avg("disk_write"),
                avg("net_rx"),
                avg("net_tx"),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NODE",
            "CPUS",
            "CPU",
            "MEMORY",
            "DISK READ",
            "DISK WRITE",
            "NET RX",
            "NET TX",
        ],
        &rows,
    )?;

    if !c.top.is_empty() {
        writeln!(out)?;
        let rows: Vec<Vec<String>> = c
            .top
            .iter()
            .map(|v| {
                vec![
                    v.name.clone(),
                    v.node.clone().unwrap_or_else(|| "-".into()),
                    value_cell("cpu", v.cpu),
                    value_cell("mem_rss", v.mem_rss),
                ]
            })
            .collect();
        table(out, &["TOP VM", "NODE", "CPU", "MEMORY"], &rows)?;
    }

    if !c.pools.is_empty() {
        writeln!(out)?;
        let rows: Vec<Vec<String>> = c
            .pools
            .iter()
            .map(|p| {
                let used = p.used_bytes as f64 / p.total_bytes.max(1) as f64 * 100.0;
                vec![
                    p.name.clone(),
                    if p.shared { "shared" } else { "local" }.into(),
                    bytes(&json!(p.used_bytes)),
                    bytes(&json!(p.total_bytes)),
                    if p.total_bytes == 0 {
                        "-".into()
                    } else {
                        format!("{used:.1}%")
                    },
                ]
            })
            .collect();
        table(out, &["POOL", "KIND", "USED", "TOTAL", "USE"], &rows)?;
    }

    for (names, what) in [
        (&c.unreachable, "warning: no metrics history from"),
        (
            &c.unsupported,
            "note: these nodes do not keep metrics history yet",
        ),
        (
            &c.skewed,
            "warning: left out because their clock is far off",
        ),
    ] {
        if !names.is_empty() {
            writeln!(err, "{what}: {}", names.join(", "))?;
        }
    }
    Ok(())
}

/// `GET /metrics/cluster`(클러스터 모으기)를 부르고 요약하거나(`-o json`) 응답 그대로 보인다.
pub(crate) async fn cluster_history(
    client: &Client,
    range: &RangeArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let q = range.query(now_ms())?;
    let mut r = client.request(Method::GET, "/metrics/cluster");
    if !q.is_empty() {
        r = r.query(&q);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let c: ClusterHistoryResponse =
        serde_json::from_value(resp).context("unexpected cluster metrics reply")?;
    cluster_tables(out, err, &c)?;
    Ok(())
}

#[derive(Subcommand)]
pub(crate) enum PrometheusCmd {
    /// Prometheus 끝점(`GET /api/v1/metrics`, viewer 이상의 API 토큰)이 켜졌는지 보인다.
    Show,
    /// 끝점을 켠다. 꺼져 있으면 404다.
    #[command(alias = "on")]
    Enable,
    /// 끝점을 끈다.
    #[command(alias = "off")]
    Disable,
}

/// `prometheus …`: `PATCH /settings {"prometheus":{"enabled":…}}`(show는 `GET /settings`)
pub(crate) async fn prometheus(
    client: &Client,
    cmd: PrometheusCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let settings = match cmd {
        PrometheusCmd::Show => client.get("/settings").await?,
        PrometheusCmd::Enable | PrometheusCmd::Disable => {
            let on = matches!(cmd, PrometheusCmd::Enable);
            client
                .patch("/settings", &json!({ "prometheus": { "enabled": on } }))
                .await?
        }
    };
    if json_out {
        print_json(out, &settings)?;
        return Ok(());
    }
    let on = settings["spec"]["prometheus"]["enabled"] == true;
    writeln!(
        out,
        "prometheus: {}",
        if on { "enabled" } else { "disabled" }
    )?;
    if on {
        writeln!(
            out,
            "scrape: {} (bearer token of a viewer or higher)",
            client.url("/metrics")
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;
    use serde_json::Value;

    fn reply(approx: bool) -> Value {
        json!({
            "tier": "min1", "step_ms": 60_000, "start": 1_762_387_200_000i64,
            "columns": {
                "net_rx": { "avg": [1024.0, null], "max": [2048.0, null], "min": [0.0, null] },
                "cpu": { "avg": [10.0, 30.0], "max": [50.0, 90.0], "min": [1.0, 2.0] },
                "mem_used": { "avg": [1024.0, 2048.0], "max": [1024.0, 2048.0], "min": [1024.0, 2048.0] }
            },
            "stats": {
                "cpu": { "avg": 20.0, "max": 90.0, "min": 1.0 },
                "mem_used": { "avg": 1536.0, "max": 2048.0, "min": 1024.0 },
                "net_rx": { "avg": 1024.0, "max": 2048.0, "min": 0.0 }
            },
            "pct": {
                "cpu": { "p50": 10.0, "p95": 30.0, "approx": approx },
                "mem_used": { "p50": 1024.0, "p95": 2048.0, "approx": approx },
                "net_rx": { "p50": 1024.0, "p95": 1024.0, "approx": approx }
            },
            "covered_from": 1_762_387_260_000i64
        })
    }

    /// 요약 표는 칸을 정한 순서(cpu, 메모리, 네트워크)로, 단위에 맞게, 근사면 `P50~`·`P95~`로 보인다.
    #[test]
    fn history_tables_show_each_field_with_units() {
        let mut out = Vec::new();
        history_table(&mut out, &serde_json::from_value(reply(true)).unwrap()).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2025-11-06T00:00:00Z .. 2025-11-06T00:02:00Z (tier min1, step 1m, 2 points)\n\
             FIELD     AVG       MAX       MIN     P50~      P95~\n\
             cpu       20.0%     90.0%     1.0%    10.0%     30.0%\n\
             mem_used  1.5GiB    2.0GiB    1.0GiB  1.0GiB    2.0GiB\n\
             net_rx    1.0KiB/s  2.0KiB/s  0B/s    1.0KiB/s  1.0KiB/s\n\
             note: no history before 2025-11-06T00:01:00Z\n"
        );
        let mut out = Vec::new();
        history_table(&mut out, &serde_json::from_value(reply(false)).unwrap()).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("P50       P95\n"), "{text}");
    }

    /// 시각 인자는 기간(지금에서 뺌)·밀리초·RFC 3339이고, 그 밖은 오류다.
    #[test]
    fn time_arguments_are_durations_milliseconds_or_rfc3339() {
        let now = 1_762_387_200_000;
        assert_eq!(time_ms("2h", now).unwrap(), now - 7_200_000);
        assert_eq!(time_ms("30s", now).unwrap(), now - 30_000);
        assert_eq!(time_ms("1700000000000", now).unwrap(), 1_700_000_000_000);
        assert_eq!(
            time_ms("2025-11-06T00:00:00Z", now).unwrap(),
            1_762_387_200_000
        );
        assert_eq!(
            time_ms("2025-11-06T09:00:00+09:00", now).unwrap(),
            1_762_387_200_000
        );
        assert!(time_ms("yesterday", now).is_err());
        assert_eq!(duration_ms("5m"), Some(300_000));
        assert_eq!(duration_ms("5"), None);
        // 앞의 부호는 기간이 아니다(`-2h`가 지금 + 2시간이 되지 않게, 리뷰 2 #9).
        assert_eq!(duration_ms("-2h"), None);
        assert_eq!(duration_ms("+2h"), None);
        assert_eq!(duration_ms("h"), None);
        assert!(time_ms("-2h", now).is_err());
    }

    /// 범위 인자는 API 질의가 된다.
    #[test]
    fn range_arguments_become_the_query() {
        let now = 1_762_387_200_000;
        let r = RangeArgs {
            range: Some(RangeArg::Month),
            points: Some(50),
            ..Default::default()
        };
        assert_eq!(
            r.query(now).unwrap(),
            [("range", "month".into()), ("points", "50".into())]
        );
        let r = RangeArgs {
            from: Some("1d".into()),
            until: Some("1h".into()),
            ..Default::default()
        };
        assert_eq!(
            r.query(now).unwrap(),
            [
                ("from", (now - 86_400_000).to_string()),
                ("until", (now - 3_600_000).to_string())
            ]
        );
        assert!(!RangeArgs::default().given());
        // `--until` 없는 기간은 같은 길이의 `range`로(서버 시계), 아니면 `until`도 이 시계로 보낸다(리뷰 2 #9).
        let from = |f: &str| RangeArgs {
            from: Some(f.into()),
            ..Default::default()
        };
        assert_eq!(from("24h").query(now).unwrap(), [("range", "day".into())]);
        assert_eq!(from("7d").query(now).unwrap(), [("range", "week".into())]);
        assert_eq!(
            from("2h").query(now).unwrap(),
            [
                ("from", (now - 7_200_000).to_string()),
                ("until", now.to_string())
            ]
        );
        assert_eq!(
            from("1700000000000").query(now).unwrap(),
            [("from", "1700000000000".to_string())]
        );
    }

    /// 기록 요약은 경로에 범위 질의를 붙여 부르고, `-o json`은 응답 그대로다.
    #[tokio::test]
    async fn history_calls_the_route_with_the_range() {
        let (client, seen) = fake_api(|_, _| (200, reply(false))).await;
        let range = RangeArgs {
            range: Some(RangeArg::Week),
            ..Default::default()
        };
        let mut out = Vec::new();
        history(&client, "/vms/web/metrics/history", &range, true, &mut out)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].line,
            "GET /api/v1/vms/web/metrics/history?range=week HTTP/1.1"
        );
        let got: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(got, reply(false));
    }

    fn cluster() -> Value {
        json!({
            "tier": "min1", "step_ms": 60_000, "start": 1_762_387_200_000i64,
            "columns": {
                "cpu": { "avg": [10.0, 30.0, null] },
                "mem_used": { "avg": [1024.0, 3072.0, null] }
            },
            "nodes": [{
                "node": "00000000-0000-0000-0000-000000000001", "name": "node-a", "cpus": 4,
                "avg": { "cpu": 20.0, "mem_used": 2048.0, "mem_total": 4096.0, "net_rx": 512.0 },
                "cpu": [10.0, 30.0, null]
            }],
            "node_count": 1,
            "top": [{ "vm": "00000000-0000-0000-0000-000000000002", "name": "web", "node": "node-a",
                      "cpu": 55.5, "mem_rss": 512.0 }],
            "pools": [{ "pool": "00000000-0000-0000-0000-000000000003", "name": "local", "shared": false,
                        "used_bytes": 1073741824u64, "total_bytes": 4294967296u64 }],
            "unreachable": ["node-b"],
            "unsupported": ["node-c"],
            "skewed": ["node-d"]
        })
    }

    /// 클러스터 요약은 클러스터 열의 평균·봉우리, 노드 줄, 상위 VM, 풀을 보이고, 빠진 노드는 stderr에 알린다.
    #[tokio::test]
    async fn the_cluster_digest_is_summarised() {
        let (client, seen) = fake_api(|_, _| (200, cluster())).await;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let range = RangeArgs {
            range: Some(RangeArg::Day),
            ..Default::default()
        };
        cluster_history(&client, &range, false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].line,
            "GET /api/v1/metrics/cluster?range=day HTTP/1.1"
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "cluster (1 nodes): 2025-11-06T00:00:00Z .. 2025-11-06T00:03:00Z (tier min1, step 1m, 3 points)\n\
             FIELD     AVG     PEAK\n\
             cpu       20.0%   30.0%\n\
             mem_used  2.0GiB  3.0GiB\n\
             \n\
             NODE    CPUS  CPU    MEMORY         DISK READ  DISK WRITE  NET RX  NET TX\n\
             node-a  4     20.0%  2.0GiB/4.0GiB  -          -           512B/s  -\n\
             \n\
             TOP VM  NODE    CPU    MEMORY\n\
             web     node-a  55.5%  512.0MiB\n\
             \n\
             POOL   KIND   USED    TOTAL   USE\n\
             local  local  1.0GiB  4.0GiB  25.0%\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no metrics history from: node-b\n\
             note: these nodes do not keep metrics history yet: node-c\n\
             warning: left out because their clock is far off: node-d\n"
        );
    }

    /// `prometheus enable|disable`(별칭 on·off)는 설정을 PATCH하고, show는 읽기만 한다.
    #[tokio::test]
    async fn prometheus_switches_the_setting() {
        let (client, seen) = fake_api(|_, _| {
            (
                200,
                json!({ "spec": { "prometheus": { "enabled": true } }, "status": {} }),
            )
        })
        .await;
        let mut out = Vec::new();
        for cmd in [
            PrometheusCmd::Enable,
            PrometheusCmd::Disable,
            PrometheusCmd::Show,
        ] {
            prometheus(&client, cmd, false, &mut out).await.unwrap();
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].line, "PATCH /api/v1/settings HTTP/1.1");
        assert_eq!(seen[0].body, json!({ "prometheus": { "enabled": true } }));
        assert_eq!(seen[1].body, json!({ "prometheus": { "enabled": false } }));
        assert_eq!(seen[2].line, "GET /api/v1/settings HTTP/1.1");
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!(
                "prometheus: enabled\nscrape: {}/api/v1/metrics (bearer token of a viewer or higher)\n",
                client.base
            )),
            "{text}"
        );
    }
}
