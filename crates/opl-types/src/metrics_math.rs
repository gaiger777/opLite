//! 지표 기록(계획 60)의 순수 계산: 층 고르기, 칸 범위, 합치기(xff), 점 수 줄이기, 요약, 백분위, 클러스터 합.
//! IO가 없어 시험이 쉽다. 저장 쪽(RRD)은 모르는 값을 NaN으로, 조회 쪽([`Cf`])은 `None`으로 다룬다.

use std::collections::BTreeMap;

use crate::metrics::{Cf, MetricField, Pct, Stat, TIERS, Tier, TopVm};

/// 층 고르기의 여유(ms). 요청 노드와 기록 노드의 시계 차이·RPC 지연 때문에 `from`이 층의 남은 기간보다 조금
/// 이르더라도 그 층을 고른다(앞의 몇 칸이 `null`이고 `covered_from`이 알린다).
pub const TIER_SLACK_MS: i64 = 60_000;

/// 합칠 때 알아야 하는 값의 몫(rrdtool xff 0.5): 아는 값이 절반 미만이면 모름(NaN)이다.
pub const XFF: f32 = 0.5;

/// 범위가 그 층의 기간 안에 들고 `from`이 그 층의 남은 기간 안(`now − span − 여유` 이후)인 가장 고운 층. 없으면
/// 가장 거친 층이다.
pub fn choose_tier(from: i64, until: i64, now: i64) -> Tier {
    for tier in TIERS {
        let span = tier.span_ms();
        if until.saturating_sub(from) <= span && from >= now - span - TIER_SLACK_MS {
            return tier;
        }
    }
    Tier::Hour6
}

/// 범위 `[from, until]`이 걸치는 층의 칸: `(첫 칸 번호, 칸 수)`. 칸 `b`의 시작은 `b × step`이다.
pub fn bucket_window(tier: Tier, from: i64, until: i64) -> (i64, usize) {
    let step = tier.step_ms();
    let first = from.div_euclid(step);
    let last = until.div_euclid(step);
    let count = (last - first + 1).max(0);
    (first, usize::try_from(count).unwrap_or(usize::MAX))
}

/// 칸 `cells`개를 `points`개 안으로 줄일 때 한 점에 묶는 칸 수(1 이상)
pub fn group_size(cells: usize, points: u16) -> usize {
    let points = usize::from(points.max(1));
    cells.div_ceil(points).max(1)
}

/// NaN을 `None`으로
pub fn known(v: f32) -> Option<f32> {
    (!v.is_nan()).then_some(v)
}

/// 저장 칸 하나의 AVG·MAX·MIN(모르면 NaN)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Agg {
    pub avg: f32,
    pub max: f32,
    pub min: f32,
}

impl Agg {
    /// 모르는 칸
    pub const UNKNOWN: Agg = Agg {
        avg: f32::NAN,
        max: f32::NAN,
        min: f32::NAN,
    };

    /// 값 하나(`Raw5s` 칸)
    pub fn single(v: f32) -> Agg {
        Agg {
            avg: v,
            max: v,
            min: v,
        }
    }

    pub fn is_unknown(&self) -> bool {
        self.avg.is_nan()
    }
}

fn enough(known: usize, total: usize) -> bool {
    known > 0 && known as f32 >= total as f32 * XFF
}

/// 5초 값들(분이면 12개)을 칸 하나로 합친다. 아는 값이 절반 미만이면 모름이다.
pub fn consolidate(values: &[f32]) -> Agg {
    let mut n = 0usize;
    let (mut sum, mut max, mut min) = (0f64, f32::NEG_INFINITY, f32::INFINITY);
    for &v in values.iter().filter(|v| !v.is_nan()) {
        n += 1;
        sum += f64::from(v);
        max = max.max(v);
        min = min.min(v);
    }
    if !enough(n, values.len()) {
        return Agg::UNKNOWN;
    }
    Agg {
        avg: (sum / n as f64) as f32,
        max,
        min,
    }
}

/// 고운 층의 칸들(`Min30`이면 `Min1` 30칸)을 거친 칸 하나로 합친다. 평균은 아는 칸 수로 가중하고, 최대의 최대,
/// 최소의 최소다. 아는 칸이 절반 미만이면 모름이다.
pub fn consolidate_aggs(cells: &[Agg]) -> Agg {
    let mut n = 0usize;
    let (mut sum, mut max, mut min) = (0f64, f32::NEG_INFINITY, f32::INFINITY);
    for c in cells.iter().filter(|c| !c.is_unknown()) {
        n += 1;
        sum += f64::from(c.avg);
        max = max.max(if c.max.is_nan() { c.avg } else { c.max });
        min = min.min(if c.min.is_nan() { c.avg } else { c.min });
    }
    if !enough(n, cells.len()) {
        return Agg::UNKNOWN;
    }
    Agg {
        avg: (sum / n as f64) as f32,
        max,
        min,
    }
}

/// 저장 칸들을 조회 열로(NaN → `None`)
pub fn to_cf(cells: &[Agg]) -> Cf {
    Cf {
        avg: cells.iter().map(|c| known(c.avg)).collect(),
        max: cells.iter().map(|c| known(c.max)).collect(),
        min: cells.iter().map(|c| known(c.min)).collect(),
    }
}

fn mean(values: impl Iterator<Item = Option<f32>>) -> Option<f32> {
    let (mut n, mut sum) = (0usize, 0f64);
    for v in values.flatten() {
        n += 1;
        sum += f64::from(v);
    }
    (n > 0).then(|| (sum / n as f64) as f32)
}

fn fold(values: &[Option<f32>], pick: fn(f32, f32) -> f32) -> Option<f32> {
    values.iter().flatten().copied().reduce(pick)
}

/// 이웃한 칸 `group`개를 점 하나로 묶는다(마지막 묶음은 짧을 수 있다). 평균은 아는 칸 수로 가중하고, 최대의 최대,
/// 최소의 최소다. 하나라도 알면 값이 있다. 비어 있는 MAX·MIN 열(`Digest`)은 그대로 비워 둔다.
pub fn downsample(cf: &Cf, group: usize) -> Cf {
    let group = group.max(1);
    Cf {
        avg: cf
            .avg
            .chunks(group)
            .map(|c| mean(c.iter().copied()))
            .collect(),
        max: cf.max.chunks(group).map(|c| fold(c, f32::max)).collect(),
        min: cf.min.chunks(group).map(|c| fold(c, f32::min)).collect(),
    }
}

/// 열 전체의 요약: 아는 칸 수로 가중한 평균, MAX 열의 최대, MIN 열의 최소(MAX·MIN 열이 비면 AVG 열로)
pub fn stats(cf: &Cf) -> Stat {
    let max = if cf.max.is_empty() { &cf.avg } else { &cf.max };
    let min = if cf.min.is_empty() { &cf.avg } else { &cf.min };
    Stat {
        avg: mean(cf.avg.iter().copied()),
        max: fold(max, f32::max),
        min: fold(min, f32::min),
    }
}

/// 최근순위(nearest-rank) 백분위 `p`(0~100]: 아는 값을 정렬해 `ceil(p/100 × n)`번째. 아는 값이 없으면 `None`.
pub fn percentile_nearest_rank(values: &[Option<f32>], p: f32) -> Option<f32> {
    let mut v: Vec<f32> = values
        .iter()
        .flatten()
        .copied()
        .filter(|v| !v.is_nan())
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    let rank = ((f64::from(p.clamp(0.0, 100.0)) / 100.0) * n as f64).ceil() as usize;
    Some(v[rank.clamp(1, n) - 1])
}

/// p50·p95. `approx`는 5초 원본이 아니라 층의 AVG로 셈했는지다.
pub fn percentiles(values: &[Option<f32>], approx: bool) -> Pct {
    Pct {
        p50: percentile_nearest_rank(values, 50.0),
        p95: percentile_nearest_rank(values, 95.0),
        approx,
    }
}

/// 노드 열들을 시각 칸끼리 합친 클러스터 열(AVG만). 노드마다 `(CPU 코어 수, 열)`을 받고, 열은 같은 `start`·
/// `step_ms`로 맞춰져 있어야 한다(같은 질의의 답). CPU %는 그 칸을 아는 노드들의 코어 수로 가중한 평균이고,
/// 나머지는 아는 노드 값의 합이다. 그 칸을 아는 노드가 없으면 `None`이다.
pub fn cluster_sum<'a>(
    nodes: impl IntoIterator<Item = (u32, &'a BTreeMap<String, Cf>)>,
) -> BTreeMap<String, Cf> {
    // 칸 이름 → 칸마다 (가중 합, 무게 합, 아는 노드가 있는지)
    let mut acc: BTreeMap<&str, Vec<(f64, f64, bool)>> = BTreeMap::new();
    for (cpus, columns) in nodes {
        for (name, cf) in columns {
            let cpu = name == MetricField::Cpu.as_str();
            let cells = acc.entry(name.as_str()).or_default();
            if cells.len() < cf.avg.len() {
                cells.resize(cf.avg.len(), (0.0, 0.0, false));
            }
            for (cell, v) in cells.iter_mut().zip(&cf.avg) {
                let Some(v) = v.filter(|v| !v.is_nan()) else {
                    continue;
                };
                let w = if cpu { f64::from(cpus) } else { 1.0 };
                cell.0 += f64::from(v) * w;
                cell.1 += w;
                cell.2 = true;
            }
        }
    }
    acc.into_iter()
        .map(|(name, cells)| {
            let cpu = name == MetricField::Cpu.as_str();
            let avg = cells
                .into_iter()
                .map(|(sum, weight, any)| match (any, cpu) {
                    (false, _) => None,
                    (true, true) if weight > 0.0 => Some((sum / weight) as f32),
                    // 코어 수를 모르는(0) 노드뿐이면 무게 없이는 평균을 낼 수 없다.
                    (true, true) => None,
                    (true, false) => Some(sum as f32),
                })
                .collect();
            (
                name.to_string(),
                Cf {
                    avg,
                    ..Cf::default()
                },
            )
        })
        .collect()
}

/// 노드마다의 상위 VM 목록을 합쳐 범위 평균 CPU가 큰 `n`개(CPU 모름은 뒤, 같으면 VM id 순). 같은 VM이 여러 노드에
/// 있으면(마이그레이션 중) CPU가 큰 것 하나만 둔다.
pub fn merge_top(lists: impl IntoIterator<Item = Vec<TopVm>>, n: usize) -> Vec<TopVm> {
    let mut best: BTreeMap<uuid::Uuid, TopVm> = BTreeMap::new();
    for vm in lists.into_iter().flatten() {
        match best.get(&vm.vm) {
            Some(old) if cpu_key(old) >= cpu_key(&vm) => {}
            _ => {
                best.insert(vm.vm, vm);
            }
        }
    }
    let mut all: Vec<TopVm> = best.into_values().collect();
    all.sort_by(|a, b| {
        cpu_key(b)
            .partial_cmp(&cpu_key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.vm.cmp(&b.vm))
    });
    all.truncate(n);
    all
}

fn cpu_key(vm: &TopVm) -> f32 {
    vm.cpu.filter(|c| !c.is_nan()).unwrap_or(f32::NEG_INFINITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn chooses_the_finest_tier_that_holds_the_range() {
        assert_eq!(choose_tier(NOW - HOUR, NOW, NOW), Tier::Raw5s);
        assert_eq!(choose_tier(NOW - 10 * MIN, NOW, NOW), Tier::Raw5s);
        assert_eq!(choose_tier(NOW - DAY, NOW, NOW), Tier::Min1);
        assert_eq!(choose_tier(NOW - 2 * HOUR, NOW, NOW), Tier::Min1);
        assert_eq!(choose_tier(NOW - 7 * DAY, NOW, NOW), Tier::Min30);
        assert_eq!(choose_tier(NOW - 30 * DAY, NOW, NOW), Tier::Hour6);
        assert_eq!(choose_tier(NOW - 365 * DAY, NOW, NOW), Tier::Hour6);
        // 범위는 짧아도 시작이 그 층의 남은 기간보다 이르면 한 층 거칠게
        assert_eq!(
            choose_tier(NOW - 3 * HOUR, NOW - 150 * MIN, NOW),
            Tier::Min1
        );
        assert_eq!(
            choose_tier(NOW - 2 * DAY, NOW - 2 * DAY + HOUR, NOW),
            Tier::Min30
        );
        assert_eq!(
            choose_tier(NOW - 400 * DAY, NOW - 399 * DAY, NOW),
            Tier::Hour6
        );
    }

    /// API 노드가 셈한 `from`이 기록 노드에서 조금 지났어도(시계 차이·지연) 같은 층이다.
    #[test]
    fn tier_choice_tolerates_a_little_clock_skew() {
        assert_eq!(choose_tier(NOW - HOUR, NOW, NOW + 30_000), Tier::Raw5s);
        assert_eq!(choose_tier(NOW - HOUR, NOW, NOW + 2 * MIN), Tier::Min1);
        assert_eq!(choose_tier(NOW - DAY, NOW, NOW + 59_000), Tier::Min1);
    }

    #[test]
    fn bucket_window_covers_both_ends() {
        assert_eq!(bucket_window(Tier::Min1, 0, 59_999), (0, 1));
        assert_eq!(bucket_window(Tier::Min1, 0, 60_000), (0, 2));
        assert_eq!(bucket_window(Tier::Min1, 30_000, 150_000), (0, 3));
        assert_eq!(bucket_window(Tier::Raw5s, NOW - HOUR, NOW), {
            let first = (NOW - HOUR) / 5_000;
            (first, 721)
        });
        // 유닉스 시각 이전도 칸 경계가 같다(내림)
        assert_eq!(bucket_window(Tier::Min1, -1, 0), (-1, 2));
    }

    #[test]
    fn group_size_fits_points() {
        assert_eq!(group_size(1440, 300), 5);
        assert_eq!(group_size(720, 300), 3);
        assert_eq!(group_size(100, 300), 1);
        assert_eq!(group_size(0, 300), 1);
        assert_eq!(group_size(1001, 1000), 2);
        assert_eq!(group_size(10, 0), 10);
    }

    #[test]
    fn consolidation_needs_half_the_values() {
        let nan = f32::NAN;
        let a = consolidate(&[1.0, 2.0, 3.0, nan, nan, nan]);
        assert_eq!(
            a,
            Agg {
                avg: 2.0,
                max: 3.0,
                min: 1.0
            }
        );
        assert!(consolidate(&[1.0, nan, nan, nan, nan]).is_unknown());
        assert!(consolidate(&[]).is_unknown());
        assert!(consolidate(&[nan; 12]).is_unknown());
        let mut minute = [nan; 12];
        minute[..6].copy_from_slice(&[5.0; 6]);
        assert_eq!(consolidate(&minute), Agg::single(5.0));
        minute[5] = nan;
        assert!(consolidate(&minute).is_unknown());
    }

    #[test]
    fn coarse_cells_weight_by_known_cells() {
        let cells = [
            Agg {
                avg: 10.0,
                max: 20.0,
                min: 5.0,
            },
            Agg::UNKNOWN,
            Agg {
                avg: 30.0,
                max: 40.0,
                min: 1.0,
            },
            Agg::single(20.0),
        ];
        assert_eq!(
            consolidate_aggs(&cells),
            Agg {
                avg: 20.0,
                max: 40.0,
                min: 1.0
            }
        );
        assert!(consolidate_aggs(&[Agg::UNKNOWN, Agg::UNKNOWN, Agg::single(1.0)]).is_unknown());
        assert!(consolidate_aggs(&[]).is_unknown());
    }

    #[test]
    fn to_cf_turns_nan_into_none() {
        let cf = to_cf(&[Agg::single(1.0), Agg::UNKNOWN]);
        assert_eq!(cf.avg, vec![Some(1.0), None]);
        assert_eq!(cf.max, vec![Some(1.0), None]);
        assert_eq!(cf.min, vec![Some(1.0), None]);
    }

    #[test]
    fn downsample_weights_known_cells() {
        let cf = Cf {
            avg: vec![Some(1.0), None, Some(3.0), None, None, Some(6.0), Some(8.0)],
            max: vec![Some(2.0), None, Some(9.0), None, None, Some(6.0), Some(8.0)],
            min: vec![
                Some(0.5),
                None,
                Some(3.0),
                None,
                None,
                Some(-1.0),
                Some(8.0),
            ],
        };
        let d = downsample(&cf, 3);
        assert_eq!(d.avg, vec![Some(2.0), Some(6.0), Some(8.0)]);
        assert_eq!(d.max, vec![Some(9.0), Some(6.0), Some(8.0)]);
        assert_eq!(d.min, vec![Some(0.5), Some(-1.0), Some(8.0)]);
        assert_eq!(downsample(&cf, 1), cf);
        assert_eq!(downsample(&cf, 0), cf);
        // 모르는 칸뿐인 묶음은 모름, AVG만 있는 열은 MAX·MIN을 비워 둔다
        let only = Cf {
            avg: vec![None, None, Some(4.0)],
            ..Cf::default()
        };
        let d = downsample(&only, 2);
        assert_eq!(d.avg, vec![None, Some(4.0)]);
        assert!(d.max.is_empty() && d.min.is_empty());
        assert_eq!(downsample(&Cf::default(), 5), Cf::default());
    }

    #[test]
    fn stats_of_empty_nan_and_one_point() {
        assert_eq!(stats(&Cf::default()), Stat::default());
        let none = Cf {
            avg: vec![None, None],
            max: vec![None, None],
            min: vec![None, None],
        };
        assert_eq!(stats(&none), Stat::default());
        let one = to_cf(&[Agg::single(7.0)]);
        assert_eq!(
            stats(&one),
            Stat {
                avg: Some(7.0),
                max: Some(7.0),
                min: Some(7.0)
            }
        );
        let cf = Cf {
            avg: vec![Some(2.0), None, Some(4.0)],
            max: vec![Some(10.0), None, Some(5.0)],
            min: vec![Some(1.0), None, Some(0.0)],
        };
        assert_eq!(
            stats(&cf),
            Stat {
                avg: Some(3.0),
                max: Some(10.0),
                min: Some(0.0)
            }
        );
        // AVG만 있으면 AVG로
        let avg_only = Cf {
            avg: vec![Some(2.0), Some(9.0)],
            ..Cf::default()
        };
        assert_eq!(stats(&avg_only).max, Some(9.0));
        assert_eq!(stats(&avg_only).min, Some(2.0));
    }

    #[test]
    fn nearest_rank_percentiles() {
        assert_eq!(percentile_nearest_rank(&[], 95.0), None);
        assert_eq!(percentile_nearest_rank(&[None, Some(f32::NAN)], 95.0), None);
        assert_eq!(percentile_nearest_rank(&[Some(3.0)], 50.0), Some(3.0));
        assert_eq!(percentile_nearest_rank(&[Some(3.0)], 95.0), Some(3.0));
        // 1..=100: p50 = 50번째, p95 = 95번째
        let v: Vec<Option<f32>> = (1..=100).rev().map(|i| Some(i as f32)).collect();
        assert_eq!(percentile_nearest_rank(&v, 50.0), Some(50.0));
        assert_eq!(percentile_nearest_rank(&v, 95.0), Some(95.0));
        assert_eq!(percentile_nearest_rank(&v, 100.0), Some(100.0));
        assert_eq!(percentile_nearest_rank(&v, 0.0), Some(1.0));
        // 아는 값 4개: p50 = 2번째, p95 = ceil(3.8) = 4번째
        let v = [Some(40.0), None, Some(10.0), Some(30.0), Some(20.0)];
        assert_eq!(
            percentiles(&v, true),
            Pct {
                p50: Some(20.0),
                p95: Some(40.0),
                approx: true
            }
        );
    }

    fn cols(pairs: &[(&str, Vec<Option<f32>>)]) -> BTreeMap<String, Cf> {
        pairs
            .iter()
            .map(|(n, v)| {
                (
                    n.to_string(),
                    Cf {
                        avg: v.clone(),
                        ..Cf::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn cluster_sum_weights_cpu_by_cores_and_sums_the_rest() {
        let a = cols(&[
            ("cpu", vec![Some(100.0), Some(50.0), None]),
            ("mem_used", vec![Some(1000.0), None, None]),
        ]);
        let b = cols(&[
            ("cpu", vec![Some(0.0), None, None]),
            ("mem_used", vec![Some(24.0), Some(8.0), None]),
        ]);
        let sum = cluster_sum([(4, &a), (12, &b)]);
        assert_eq!(sum["cpu"].avg, vec![Some(25.0), Some(50.0), None]);
        assert_eq!(sum["mem_used"].avg, vec![Some(1024.0), Some(8.0), None]);
        assert!(sum["cpu"].max.is_empty());
        // 코어 수를 모르는 노드뿐이면 CPU는 모름, 다른 칸은 합
        let c = cols(&[("cpu", vec![Some(10.0)]), ("net_rx", vec![Some(1.0)])]);
        let sum = cluster_sum([(0, &c), (0, &c)]);
        assert_eq!(sum["cpu"].avg, vec![None]);
        assert_eq!(sum["net_rx"].avg, vec![Some(2.0)]);
        assert!(cluster_sum(std::iter::empty()).is_empty());
    }

    #[test]
    fn cluster_sum_takes_the_longest_column() {
        let a = cols(&[("net_rx", vec![Some(1.0)])]);
        let b = cols(&[("net_rx", vec![Some(1.0), Some(2.0)])]);
        assert_eq!(
            cluster_sum([(1, &a), (1, &b)])["net_rx"].avg,
            vec![Some(2.0), Some(2.0)]
        );
    }

    #[test]
    fn merge_top_keeps_the_busiest() {
        let id = |n: u128| uuid::Uuid::from_u128(n);
        let vm = |n, cpu| TopVm {
            vm: id(n),
            cpu,
            mem_rss: None,
        };
        let top = merge_top(
            [
                vec![vm(1, Some(10.0)), vm(2, Some(50.0)), vm(5, None)],
                vec![vm(3, Some(30.0)), vm(1, Some(70.0)), vm(4, Some(30.0))],
            ],
            4,
        );
        let ids: Vec<_> = top.iter().map(|v| (v.vm, v.cpu)).collect();
        assert_eq!(
            ids,
            vec![
                (id(1), Some(70.0)),
                (id(2), Some(50.0)),
                (id(3), Some(30.0)),
                (id(4), Some(30.0)),
            ]
        );
        assert_eq!(merge_top([vec![vm(5, None)]], 10).len(), 1);
    }
}
