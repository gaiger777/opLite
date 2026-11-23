//! NBD(네트워크 블록 장치) 클라이언트와 서버(계획 68 설계 §8·§11.1). 프로토콜은 `nbd/doc/proto.md`의 fixed
//! newstyle 협상과 구조화 응답만 쓴다(TLS·확장 머리는 쓰지 않는다 — 유닉스 소켓 0600).
//!
//! - [`client`]: QEMU의 fleecing 내보내기·`qemu-nbd`를 읽는다(`base:allocation`·`qemu:dirty-bitmap:*` 문맥).
//! - [`server`]: 읽기 전용 내보내기를 여는 서버(`base:allocation` 문맥). [`export`]가 저장소 백업 디스크를 원본으로 준다.
//! - [`serve`]: `orbivisord backup-serve` 헬퍼의 몸통(표준 입력 설정 → 저장소 열기 → 디스크마다 내보내기).

pub mod client;
pub mod export;
pub mod serve;
pub mod server;

pub use client::{Extent, NbdClient, NbdError};
pub use server::{ExportSource, NbdServer};

/// 협상 첫 매직 `NBDMAGIC`
pub(crate) const NBDMAGIC: u64 = 0x4e42_444d_4147_4943;
/// newstyle 매직 `IHAVEOPT`(서버 인사와 클라이언트 옵션 머리)
pub(crate) const IHAVEOPT: u64 = 0x4948_4156_454f_5054;
/// 옵션 응답 매직
pub(crate) const REPLY_MAGIC: u64 = 0x0003_e889_0455_65a9;
/// 요청 매직
pub(crate) const REQUEST_MAGIC: u32 = 0x2560_9513;
/// 단순 응답 매직
pub(crate) const SIMPLE_REPLY_MAGIC: u32 = 0x6744_6698;
/// 구조화 응답 조각 매직
pub(crate) const STRUCTURED_REPLY_MAGIC: u32 = 0x668e_33ef;

// 서버 인사 깃발
pub(crate) const FLAG_FIXED_NEWSTYLE: u16 = 1 << 0;
pub(crate) const FLAG_NO_ZEROES: u16 = 1 << 1;
// 클라이언트 깃발
pub(crate) const FLAG_C_FIXED_NEWSTYLE: u32 = 1 << 0;
pub(crate) const FLAG_C_NO_ZEROES: u32 = 1 << 1;

// 옵션
pub(crate) const OPT_EXPORT_NAME: u32 = 1;
pub(crate) const OPT_ABORT: u32 = 2;
pub(crate) const OPT_LIST: u32 = 3;
pub(crate) const OPT_INFO: u32 = 6;
pub(crate) const OPT_GO: u32 = 7;
pub(crate) const OPT_STRUCTURED_REPLY: u32 = 8;
pub(crate) const OPT_LIST_META_CONTEXT: u32 = 9;
pub(crate) const OPT_SET_META_CONTEXT: u32 = 10;

// 옵션 응답 종류
pub(crate) const REP_ACK: u32 = 1;
pub(crate) const REP_SERVER: u32 = 2;
pub(crate) const REP_INFO: u32 = 3;
pub(crate) const REP_META_CONTEXT: u32 = 4;
pub(crate) const REP_ERR_UNSUP: u32 = (1 << 31) | 1;
pub(crate) const REP_ERR_INVALID: u32 = (1 << 31) | 3;
pub(crate) const REP_ERR_UNKNOWN: u32 = (1 << 31) | 6;
pub(crate) const REP_ERR_TOO_BIG: u32 = (1 << 31) | 9;

// 정보 종류(OPT_INFO·OPT_GO)
pub(crate) const INFO_EXPORT: u16 = 0;
pub(crate) const INFO_BLOCK_SIZE: u16 = 3;

// 전송 깃발
pub(crate) const TFLAG_HAS_FLAGS: u16 = 1 << 0;
pub(crate) const TFLAG_READ_ONLY: u16 = 1 << 1;
pub(crate) const TFLAG_SEND_FLUSH: u16 = 1 << 2;
pub(crate) const TFLAG_SEND_DF: u16 = 1 << 7;
pub(crate) const TFLAG_CAN_MULTI_CONN: u16 = 1 << 8;

// 명령
pub(crate) const CMD_READ: u16 = 0;
pub(crate) const CMD_WRITE: u16 = 1;
pub(crate) const CMD_DISC: u16 = 2;
pub(crate) const CMD_FLUSH: u16 = 3;
pub(crate) const CMD_BLOCK_STATUS: u16 = 7;
// 명령 깃발
pub(crate) const CMD_FLAG_REQ_ONE: u16 = 1 << 3;

// 구조화 응답
pub(crate) const REPLY_FLAG_DONE: u16 = 1 << 0;
pub(crate) const REPLY_TYPE_NONE: u16 = 0;
pub(crate) const REPLY_TYPE_OFFSET_DATA: u16 = 1;
pub(crate) const REPLY_TYPE_OFFSET_HOLE: u16 = 2;
pub(crate) const REPLY_TYPE_BLOCK_STATUS: u16 = 5;
pub(crate) const REPLY_TYPE_ERROR: u16 = (1 << 15) | 1;
pub(crate) const REPLY_TYPE_ERROR_OFFSET: u16 = (1 << 15) | 2;

// 오류 값(errno와 같은 수)
pub const EPERM: u32 = 1;
pub const EIO: u32 = 5;
pub const EINVAL: u32 = 22;
pub const ENOTSUP: u32 = 95;
pub const ESHUTDOWN: u32 = 108;

/// `base:allocation` 문맥의 상태 비트: 할당되지 않음
pub const STATE_HOLE: u32 = 1 << 0;
/// `base:allocation` 문맥의 상태 비트: 0으로 읽힘
pub const STATE_ZERO: u32 = 1 << 1;
/// `qemu:dirty-bitmap:*` 문맥의 상태 비트: 바뀜
pub const STATE_DIRTY: u32 = 1 << 0;

/// 구멍·0을 알리는 문맥
pub const BASE_ALLOCATION: &str = "base:allocation";

/// QEMU dirty bitmap 문맥 이름(`qemu:dirty-bitmap:<이름>`)
pub fn dirty_bitmap_context(bitmap: &str) -> String {
    format!("qemu:dirty-bitmap:{bitmap}")
}

/// 한 요청의 최대 길이(읽기). 서버는 `INFO_BLOCK_SIZE`로 알리고 넘으면 거절한다.
pub const MAX_REQUEST: u32 = 32 << 20;
/// 옵션 데이터의 최대 길이(이름·질의)
pub(crate) const MAX_OPTION: u32 = 64 << 10;

#[cfg(test)]
mod tests;
