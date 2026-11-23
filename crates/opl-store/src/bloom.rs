//! GC 표시 집합의 Bloom 필터(설계 §10.4). 청크 id가 이미 균일한 해시이므로 id의 두 조각(8바이트씩)을 씨앗과 섞어
//! 이중 해싱(`h1 + i·h2`)으로 위치 k개를 낸다. 오탐은 쓰레기를 남길 뿐 지우지 않는다. 실행마다 씨앗을 바꿔 같은 쓰레기가
//! 계속 남지 않게 한다.

use opl_types::backup_store::ChunkId;

/// 기본 오탐률 1%
pub const FALSE_POSITIVE: f64 = 0.01;
/// 가장 작은 용량(아주 작은 저장소에서도 오탐이 거의 없게)
const MIN_CAPACITY: u64 = 1024;

#[derive(Clone, Debug)]
pub struct Bloom {
    bits: Vec<u64>,
    m: u64,
    k: u32,
    seed: u64,
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Bloom {
    /// `capacity`개를 넣을 때 오탐률이 `fp`가 되게 크기를 정한다.
    pub fn new(capacity: u64, fp: f64, seed: u64) -> Bloom {
        let n = capacity.max(MIN_CAPACITY) as f64;
        let ln2 = std::f64::consts::LN_2;
        let m = (-(n * fp.ln()) / (ln2 * ln2)).ceil().max(64.0) as u64;
        let k = ((m as f64 / n) * ln2).round().clamp(1.0, 16.0) as u32;
        Bloom {
            bits: vec![0; m.div_ceil(64) as usize],
            m,
            k,
            seed,
        }
    }

    /// 비트 배열 크기(바이트)
    pub fn bytes(&self) -> usize {
        self.bits.len() * 8
    }

    fn hashes(&self, id: &ChunkId) -> (u64, u64) {
        let w = |o: usize| u64::from_le_bytes(id[o..o + 8].try_into().expect("8 bytes"));
        let h1 = splitmix(w(0) ^ self.seed);
        let h2 = splitmix(w(8) ^ self.seed.rotate_left(32)) | 1;
        (h1, h2)
    }

    fn position(&self, h: (u64, u64), i: u64) -> u64 {
        h.0.wrapping_add(i.wrapping_mul(h.1)) % self.m
    }

    pub fn insert(&mut self, id: &ChunkId) {
        let h = self.hashes(id);
        for i in 0..self.k as u64 {
            let p = self.position(h, i);
            self.bits[(p / 64) as usize] |= 1 << (p % 64);
        }
    }

    /// 있을 수도 있음(참) / 확실히 없음(거짓)
    pub fn contains(&self, id: &ChunkId) -> bool {
        let h = self.hashes(id);
        (0..self.k as u64).all(|i| {
            let p = self.position(h, i);
            self.bits[(p / 64) as usize] & (1 << (p % 64)) != 0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::chunk::sha256;

    fn id(i: u64, salt: u8) -> ChunkId {
        let mut b = i.to_le_bytes().to_vec();
        b.push(salt);
        sha256(&b)
    }

    #[test]
    fn no_false_negatives_and_fp_rate() {
        let n = 1_000_000u64;
        let mut b = Bloom::new(n, FALSE_POSITIVE, 42);
        assert!(
            b.bytes() < 1_300_000,
            "about 10 bits per entry: {}",
            b.bytes()
        );
        for i in 0..n {
            b.insert(&id(i, 0));
        }
        for i in (0..n).step_by(97) {
            assert!(b.contains(&id(i, 0)));
        }
        let fps = (0..n).filter(|i| b.contains(&id(*i, 1))).count();
        let rate = fps as f64 / n as f64;
        assert!(rate <= 0.012, "false positive rate {rate}");
    }

    #[test]
    fn seed_changes_false_positives() {
        let n = 20_000u64;
        let mut a = Bloom::new(n, FALSE_POSITIVE, 1);
        let mut b = Bloom::new(n, FALSE_POSITIVE, 2);
        for i in 0..n {
            a.insert(&id(i, 0));
            b.insert(&id(i, 0));
        }
        let fa: Vec<u64> = (0..n).filter(|i| a.contains(&id(*i, 9))).collect();
        let both = fa.iter().filter(|i| b.contains(&id(**i, 9))).count();
        assert!(!fa.is_empty());
        assert!(both * 10 < fa.len(), "{both} of {}", fa.len());
    }
}
