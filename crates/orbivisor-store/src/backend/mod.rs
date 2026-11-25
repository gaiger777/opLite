//! 저장소 백엔드(설계 §7.5~§7.7). 객체 이름은 `/`로 나눈 상대 경로이고, 백엔드는 객체 쓰기·읽기·있음 확인·지우기·
//! 나열·옮기기·만지기만 한다. 형식은 모른다.
//!
//! - [`local::LocalBackend`]: 백업 풀 안 디렉터리(`{풀 루트}/store/{저장소 id}/`). 임시 파일 → `sync_all` → `rename`
//!   → 디렉터리 fsync.
//! - [`s3::S3Backend`]: S3 호환 객체 저장소(SigV4, hyper + rustls).
//! - [`mem::MemBackend`]: 메모리(시험·가짜 S3의 바탕, 시계를 조종한다).

pub mod local;
pub mod mem;
pub mod s3;
pub mod sigv4;
#[cfg(test)]
mod suite;

use async_trait::async_trait;
use thiserror::Error;

use crate::crypto::KeyError;
use crate::format::FormatError;

/// 한 번에 나열하는 객체 수(S3 `ListObjectsV2`의 쪽 크기와 같다)
pub const LIST_PAGE: usize = 1000;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("{0} was not found in the backup store")]
    NotFound(String),
    /// 닿지 않음(다시 하기를 다 쓴 뒤의 5xx·연결 오류·시간 넘김)
    #[error("the backup store is unreachable: {0}")]
    Unavailable(String),
    /// 403 따위
    #[error("the backup store denied access (check the credentials or the bucket policy): {0}")]
    Denied(String),
    /// 그 밖의 4xx
    #[error("the backup store rejected the request: {0}")]
    Rejected(String),
    #[error("{0}")]
    Corrupt(String),
    #[error("the key does not match the store: {0}")]
    KeyMismatch(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Invalid(String),
}

impl From<FormatError> for StoreError {
    fn from(e: FormatError) -> Self {
        match e {
            FormatError::Corrupt(m) => StoreError::Corrupt(m),
            e @ FormatError::KeyMismatch { .. } => StoreError::KeyMismatch(e.to_string()),
        }
    }
}

impl From<KeyError> for StoreError {
    fn from(e: KeyError) -> Self {
        match e {
            KeyError::Mismatch { .. } => StoreError::KeyMismatch(e.to_string()),
            e => StoreError::Invalid(e.to_string()),
        }
    }
}

/// 객체의 크기와 마지막으로 쓴 시각(저장소 시계, 유닉스 초)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    pub size: u64,
    pub modified: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub modified: u64,
}

/// 나열 한 쪽. `next`가 있으면 그것을 주고 다시 부른다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListPage {
    pub objects: Vec<ObjectInfo>,
    pub next: Option<String>,
}

#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// 오류·로그에 쓸 설명(비밀 없음)
    fn describe(&self) -> String;
    /// 동시 요청 수(S3 `max_streams`, 로컬은 고정)
    fn max_streams(&self) -> usize;
    /// 처음 만들 때 한 번(로컬: `chunks/0000`~`ffff` 디렉터리)
    async fn prepare(&self) -> Result<(), StoreError> {
        Ok(())
    }
    /// 쓴다(있으면 덮어씀 — 원자적으로 바뀐다).
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<(), StoreError>;
    /// 없을 때만 쓴다. 썼으면 참, 이미 있으면 거짓. 조건부 쓰기를 모르는 S3 구현에서는 있음 확인 뒤 쓰므로 부르는
    /// 쪽이 다시 읽어 견줘야 한다.
    async fn put_if_absent(&self, key: &str, data: Vec<u8>) -> Result<bool, StoreError>;
    /// 읽는다. 없으면 [`StoreError::NotFound`].
    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError>;
    /// 있음 확인
    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError>;
    /// 지운다(없어도 성공).
    async fn delete(&self, key: &str) -> Result<(), StoreError>;
    /// `prefix`로 시작하는 객체를 이름 차례로 한 쪽. `token`은 앞 쪽의 `next`.
    async fn list(&self, prefix: &str, token: Option<String>) -> Result<ListPage, StoreError>;
    /// 옮긴다(로컬 `rename`, S3 복사 + 지우기). 원본이 없으면 [`StoreError::NotFound`].
    async fn rename(&self, from: &str, to: &str) -> Result<(), StoreError>;
    /// 마지막으로 쓴 시각을 지금으로(로컬 `utimensat`, S3 같은 키로 복사)
    async fn touch(&self, key: &str) -> Result<(), StoreError>;
}

/// 모든 쪽을 모아 나열한다(작은 접두사에만 — `snapshots/`·`gc/`·`keys/`).
pub async fn list_all(b: &dyn Backend, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
    let mut out = Vec::new();
    let mut token = None;
    loop {
        let page = b.list(prefix, token).await?;
        out.extend(page.objects);
        match page.next {
            Some(t) => token = Some(t),
            None => return Ok(out),
        }
    }
}

/// 객체 이름 검사: 비지 않고, `/`로 시작·끝나지 않고, 빈·`.`·`..` 조각과 제어 문자가 없다.
pub fn check_key(key: &str) -> Result<(), StoreError> {
    let ok = !key.is_empty()
        && key.len() <= 1024
        && !key.chars().any(|c| c.is_control() || c == '\\')
        && key
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..");
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!("invalid object name {key:?}")))
    }
}

/// 나열 접두사 검사(빈 접두사나 `/`로 끝나는 것도 됨)
pub fn check_prefix(prefix: &str) -> Result<(), StoreError> {
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() && prefix.len() <= 1 {
        return Ok(());
    }
    check_key(trimmed)
}

/// 지금 시각(유닉스 초)
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_rules() {
        for k in ["a", "chunks/0000/ab", "keys/ab:cd.json", "gc/x.start"] {
            check_key(k).unwrap();
        }
        for k in ["", "/a", "a/", "a//b", "a/../b", "./a", "a\nb", "a\\b"] {
            assert!(check_key(k).is_err(), "{k:?}");
        }
        check_prefix("").unwrap();
        check_prefix("chunks/").unwrap();
        check_prefix("chunks/00").unwrap();
        assert!(check_prefix("../").is_err());
        assert!(check_prefix("//").is_err());
    }
}
