//! 고정 인덱스 `disk{i}.fidx`(설계 §7.3). 디스크의 청크 id를 차례로 담는다. 평문이다(암호화 저장소의 id는 키가 든 HMAC).
//!
//! ```text
//! 0  "OPF1"   4 version u32 = 1   8 backup uuid(16)   24 virtual_bytes u64
//! 32 chunk_bytes u32   36 flags u32 (bit0 암호화 id)   40 count u64   48..64 예약
//! 64 청크 id × count (32바이트씩, 0 청크는 모두 0)
//! 끝 SHA-256(앞의 모든 바이트) 32
//! ```

use orbivisor_types::backup_store::{ChunkId, ZERO_CHUNK, chunk_count};
use uuid::Uuid;

use super::FormatError;
use super::chunk::sha256;

pub const MAGIC: &[u8; 4] = b"OPF1";
pub const VERSION: u32 = 1;
pub const HEADER_BYTES: usize = 64;
pub const FLAG_KEYED_IDS: u32 = 1;

/// 고정 인덱스 하나
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedIndex {
    pub backup: Uuid,
    pub virtual_bytes: u64,
    pub chunk_bytes: u32,
    /// 청크 id가 HMAC인가(암호화 저장소)
    pub keyed: bool,
    pub ids: Vec<ChunkId>,
}

impl FixedIndex {
    /// 0 청크로 채운 인덱스(청크 수는 가상 크기에서)
    pub fn new(backup: Uuid, virtual_bytes: u64, chunk_bytes: u32, keyed: bool) -> FixedIndex {
        let n = chunk_count(virtual_bytes, chunk_bytes as u64) as usize;
        FixedIndex {
            backup,
            virtual_bytes,
            chunk_bytes,
            keyed,
            ids: vec![ZERO_CHUNK; n],
        }
    }

    /// 0이 아닌 청크 수
    pub fn nonzero(&self) -> usize {
        self.ids.iter().filter(|id| **id != ZERO_CHUNK).count()
    }

    /// 오프셋이 든 청크 번호
    pub fn chunk_at(&self, offset: u64) -> usize {
        (offset / self.chunk_bytes as u64) as usize
    }

    /// 청크 i의 길이(마지막 청크는 짧을 수 있다)
    pub fn chunk_len(&self, i: usize) -> u64 {
        let start = i as u64 * self.chunk_bytes as u64;
        (self.virtual_bytes - start).min(self.chunk_bytes as u64)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_BYTES + self.ids.len() * 32 + 32);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(self.backup.as_bytes());
        out.extend_from_slice(&self.virtual_bytes.to_le_bytes());
        out.extend_from_slice(&self.chunk_bytes.to_le_bytes());
        let flags = if self.keyed { FLAG_KEYED_IDS } else { 0 };
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&(self.ids.len() as u64).to_le_bytes());
        out.resize(HEADER_BYTES, 0);
        for id in &self.ids {
            out.extend_from_slice(id);
        }
        let tail = sha256(&out);
        out.extend_from_slice(&tail);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<FixedIndex, FormatError> {
        let bad = |s: &str| FormatError::Corrupt(format!("index: {s}"));
        if b.len() < HEADER_BYTES + 32 || &b[..4] != MAGIC {
            return Err(bad("not an index file"));
        }
        let (body, tail) = b.split_at(b.len() - 32);
        if sha256(body) != *tail {
            return Err(bad("checksum does not match"));
        }
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"));
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"));
        if u32_at(4) != VERSION {
            return Err(bad("unknown version"));
        }
        let backup = Uuid::from_slice(&b[8..24]).expect("16 bytes");
        let virtual_bytes = u64_at(24);
        let chunk_bytes = u32_at(32);
        let flags = u32_at(36);
        let count = u64_at(40);
        if chunk_bytes == 0 || flags & !FLAG_KEYED_IDS != 0 {
            return Err(bad("bad header"));
        }
        if count != chunk_count(virtual_bytes, chunk_bytes as u64)
            || (body.len() - HEADER_BYTES) as u64 != count.saturating_mul(32)
        {
            return Err(bad("chunk count does not match the size"));
        }
        // 길이는 위에서 32의 배수로 확인했다(남는 바이트 없음).
        let ids = body[HEADER_BYTES..].as_chunks::<32>().0.to_vec();
        Ok(FixedIndex {
            backup,
            virtual_bytes,
            chunk_bytes,
            keyed: flags & FLAG_KEYED_IDS != 0,
            ids,
        })
    }
}

/// 인덱스 파일 크기에서 청크 수(GC의 Bloom 크기 어림 — 목록의 크기만 보고)
pub fn entries_from_size(size: u64) -> u64 {
    size.saturating_sub((HEADER_BYTES + 32) as u64) / 32
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbivisor_types::backup_store::CHUNK_BYTES;

    #[test]
    fn roundtrip_and_tail_hash() {
        let mut ix = FixedIndex::new(
            Uuid::new_v4(),
            3 * CHUNK_BYTES + 5,
            CHUNK_BYTES as u32,
            true,
        );
        assert_eq!(ix.ids.len(), 4);
        ix.ids[1] = [9; 32];
        ix.ids[3] = [7; 32];
        assert_eq!(ix.nonzero(), 2);
        assert_eq!(ix.chunk_len(3), 5);
        assert_eq!(ix.chunk_len(0), CHUNK_BYTES);
        assert_eq!(ix.chunk_at(CHUNK_BYTES * 2 + 1), 2);
        let b = ix.to_bytes();
        assert_eq!(b.len(), 64 + 4 * 32 + 32);
        assert_eq!(entries_from_size(b.len() as u64), 4);
        assert_eq!(FixedIndex::from_bytes(&b).unwrap(), ix);
        // 한 바이트라도 바뀌면 꼬리 해시가 잡는다
        for pos in [0, 30, 64 + 33, b.len() - 1] {
            let mut t = b.clone();
            t[pos] ^= 1;
            assert!(FixedIndex::from_bytes(&t).is_err(), "pos {pos}");
        }
        assert!(FixedIndex::from_bytes(&b[..50]).is_err());
    }

    #[test]
    fn count_must_match_size() {
        let ix = FixedIndex::new(Uuid::nil(), 10, 4, false);
        assert_eq!(ix.ids.len(), 3);
        let mut b = ix.to_bytes();
        // 크기를 바꾸고 꼬리를 다시 계산해도 청크 수가 맞지 않으면 거절
        b[24..32].copy_from_slice(&100u64.to_le_bytes());
        b.truncate(b.len() - 32);
        let tail = sha256(&b);
        b.extend_from_slice(&tail);
        assert!(FixedIndex::from_bytes(&b).is_err());
        let empty = FixedIndex::new(Uuid::nil(), 0, 4, false);
        assert_eq!(FixedIndex::from_bytes(&empty.to_bytes()).unwrap(), empty);
    }
}
