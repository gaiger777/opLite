//! CBT(changed block tracking) 바뀐 구역(설계 §4·§9.2). `QueryChangedDiskAreas`는 디스크 일부만 답하므로
//! `startOffset + length`로 이어 부른다. `changeId = "*"`이면 할당된 구역 전체다. 구역을 합치고 읽기 조각
//! (4 MiB)으로 나누는 순수 함수도 여기 있다.

use super::VsphereError;
use super::client::Session;
use super::soap::MoRef;

/// 할당된 구역 전체를 묻는 `changeId`
pub const ALL_AREAS: &str = "*";
/// 읽기·쓰기 조각(설계 §7.4)
pub const CHUNK: u64 = 4 << 20;
/// 이어 부르는 횟수 한도(서버가 앞으로 나아가지 않을 때 멈춘다)
const MAX_CALLS: usize = 1 << 20;

/// 디스크 하나의 바뀐 구역(시작·길이, 합치고 차례대로)을 모두 묻는다. 결함(`FileFault` 등 — `changeId`가 무효)은
/// 그대로 돌려준다: 부르는 쪽이 그 디스크를 `"*"`로 다시 복사한다.
pub async fn changed_areas(
    session: &Session,
    vm: &MoRef,
    snapshot: &MoRef,
    device_key: i32,
    capacity: u64,
    change_id: &str,
) -> Result<Vec<(u64, u64)>, VsphereError> {
    let mut areas = Vec::new();
    let mut offset = 0u64;
    for _ in 0..MAX_CALLS {
        if offset >= capacity {
            break;
        }
        let info = session
            .query_changed_disk_areas(vm, snapshot, device_key, offset, change_id)
            .await?;
        areas.extend(info.areas);
        let next = info.start_offset.saturating_add(info.length);
        if next <= offset {
            return Err(VsphereError::Invalid(format!(
                "QueryChangedDiskAreas did not advance past offset {offset}"
            )));
        }
        offset = next;
    }
    Ok(merge(areas, capacity))
}

/// 구역을 차례대로 놓고 겹치거나 맞닿은 것을 합친다. 디스크 끝을 넘는 부분은 자른다.
pub fn merge(mut areas: Vec<(u64, u64)>, capacity: u64) -> Vec<(u64, u64)> {
    areas.retain(|(s, l)| *l > 0 && *s < capacity);
    areas.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(areas.len());
    for (start, len) in areas {
        let end = start.saturating_add(len).min(capacity);
        match out.last_mut() {
            Some((s, l)) if start <= *s + *l => {
                *l = (*l).max(end - *s);
            }
            _ => out.push((start, end - start)),
        }
    }
    out
}

/// 구역을 `chunk` 바이트를 넘지 않는 조각으로 나눈다(조각은 구역 경계를 넘지 않는다).
pub fn chunks(areas: &[(u64, u64)], chunk: u64) -> Vec<(u64, u32)> {
    let chunk = chunk.clamp(1, u64::from(u32::MAX));
    let mut out = Vec::new();
    for &(start, len) in areas {
        let mut at = start;
        let end = start + len;
        while at < end {
            let n = (end - at).min(chunk);
            out.push((at, n as u32));
            at += n;
        }
    }
    out
}

/// 사이가 `gap` 바이트 이하인 구역을 하나로 잇는다(작은 범위 읽기 — 요청·티켓 — 를 줄인다). 사이는 얼어 있는 기반의 같은
/// 내용이라 다시 써도 대상이 바뀌지 않는다(설계 §9.2 — 바뀌지 않은 곳은 앞 회차가 이미 같은 바이트로 썼다).
pub fn coalesce(areas: &[(u64, u64)], gap: u64) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(areas.len());
    for &(start, len) in areas {
        match out.last_mut() {
            Some((s, l)) if start <= (*s + *l).saturating_add(gap) => {
                *l = (*l).max(start + len - *s);
            }
            _ => out.push((start, len)),
        }
    }
    out
}

/// 구역들의 바이트 합
pub fn total(areas: &[(u64, u64)]) -> u64 {
    areas.iter().map(|(_, l)| l).sum()
}

/// 조각이 모두 0인가(0 조각은 쓰지 않고 `write_zeroes`로 — 씬 대상이 씬으로 남는다)
pub fn is_zero(data: &[u8]) -> bool {
    let (words, rest) = data.as_chunks::<8>();
    words.iter().all(|w| u64::from_ne_bytes(*w) == 0) && rest.iter().all(|b| *b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_and_adjacent_areas() {
        assert_eq!(
            merge(vec![(100, 10), (0, 10), (10, 5), (105, 20), (200, 0)], 1000),
            vec![(0, 15), (100, 25)]
        );
        // 끝을 넘으면 자르고, 밖이면 뺀다
        assert_eq!(merge(vec![(90, 50), (2000, 1)], 100), vec![(90, 10)]);
        // 안에 든 구역
        assert_eq!(merge(vec![(0, 100), (10, 5)], 1000), vec![(0, 100)]);
        assert!(merge(Vec::new(), 10).is_empty());
    }

    #[test]
    fn splits_areas_into_chunks() {
        let mib = 1 << 20;
        assert_eq!(
            chunks(&[(0, 9 * mib), (20 * mib, 1)], 4 * mib),
            vec![
                (0, (4 * mib) as u32),
                (4 * mib, (4 * mib) as u32),
                (8 * mib, mib as u32),
                (20 * mib, 1)
            ]
        );
        assert_eq!(total(&[(0, 5), (10, 7)]), 12);
    }

    #[test]
    fn coalesces_areas_with_small_gaps() {
        let k = 1 << 10;
        assert_eq!(
            coalesce(&[(0, 128 * k), (192 * k, 128 * k), (2048 * k, k)], 64 * k),
            vec![(0, 320 * k), (2048 * k, k)]
        );
        assert_eq!(coalesce(&[(0, 10), (20, 5)], 9), vec![(0, 10), (20, 5)]);
        assert_eq!(coalesce(&[(0, 10), (20, 5)], 10), vec![(0, 25)]);
        assert!(coalesce(&[], 1).is_empty());
    }

    #[test]
    fn detects_zero_chunks() {
        assert!(is_zero(&[0; 4099]));
        let mut d = vec![0u8; 4099];
        d[4098] = 1;
        assert!(!is_zero(&d));
        d[4098] = 0;
        d[7] = 1;
        assert!(!is_zero(&d));
        assert!(is_zero(&[]));
    }
}
