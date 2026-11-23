//! 백업 저장소(계획 68 설계 §6). 중복 제거 청크 저장소 하나 — 백업 풀 안(`{풀 루트}/store/{저장소 id}/`) 또는 S3 호환
//! 객체 저장소 — 의 설정과 상태다. 데이터(청크·인덱스·매니페스트 v3)는 저장소 안에만 있고 Raft에는 이 리소스와 백업마다의
//! `Backup` 리소스만 둔다. 루트 종류다(폴더 없음). 비밀(S3 비밀 키, 암호화 주 키)은 Raft 스펙에만 있고 `Debug`에서 뺀다
//! (API 보기·watch·감사에서 빼는 것은 API가 한다).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::backup::{Backup, BackupPhase, BackupTarget};
use crate::backup_job::{BackupJobError, JobSchedule};
use crate::resource::{Kind, Resource, ResourceSpec, validate_name};

pub type BackupStore = Resource<BackupStoreSpec, BackupStoreStatus>;

/// 청크 크기(고정 4 MiB, §7.1). 저장소의 `store.json`에 적고 바꾸지 않는다.
pub const CHUNK_BYTES: u64 = 4 << 20;

/// 청크 id(평문 저장소는 SHA-256, 암호화 저장소는 HMAC-SHA256 — §7.2)
pub type ChunkId = [u8; 32];

/// 내용이 모두 0인 청크의 인덱스 자리(저장하지 않는다 — §7.1)
pub const ZERO_CHUNK: ChunkId = [0; 32];

/// S3 동시 요청 수의 한도(§6)
pub const MAX_STREAMS: u8 = 64;

/// 다시 검증할 날 수의 한도
pub const MAX_REVERIFY_DAYS: u32 = 3650;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupStoreSpec {
    pub backend: StoreBackend,
    /// 클라이언트 쪽 암호화(§9). 없으면 평문 저장소다. 만든 뒤 바꾸지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub encryption: Option<StoreEncryption>,
    /// 다른 클러스터가 주인인 저장소를 복원용으로 붙였다(§12.6). 백업·지우기·GC를 하지 않는다.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub read_only: bool,
    /// 가비지 수집 일정(UTC). 기본은 매일 05:00.
    #[serde(default = "default_gc")]
    #[cfg_attr(feature = "openapi", schema(default = default_gc))]
    pub gc: JobSchedule,
    /// 예약 검증. 없으면 예약 검증을 하지 않는다(손으로·새 백업 검증만).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub verify: Option<VerifyPolicy>,
    /// S3 동시 요청 수(1~64). 로컬 백엔드는 쓰지 않는다.
    #[serde(default = "default_max_streams")]
    #[cfg_attr(feature = "openapi", schema(default = 8, minimum = 1, maximum = 64))]
    pub max_streams: u8,
}

impl std::fmt::Debug for BackupStoreSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupStoreSpec")
            .field("backend", &self.backend)
            .field("encryption", &self.encryption)
            .field("read_only", &self.read_only)
            .field("gc", &self.gc)
            .field("verify", &self.verify)
            .field("max_streams", &self.max_streams)
            .finish()
    }
}

fn default_gc() -> JobSchedule {
    JobSchedule {
        at: "05:00".into(),
        days: Vec::new(),
    }
}

fn default_max_streams() -> u8 {
    8
}

fn default_reverify_days() -> u32 {
    30
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl ResourceSpec for BackupStoreSpec {
    const KIND: Kind = Kind::BackupStore;
    type Status = BackupStoreStatus;
}

/// 저장소가 사는 곳. JSON은 `{"pool": "bk"}` 또는 `{"s3": {...}}`이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreBackend {
    /// 백업 풀(`backups`를 켠 dir·NFS 풀)의 이름. 경로는 `{풀 루트}/store/{저장소 id}/`.
    Pool(String),
    S3(S3Target),
}

/// S3 호환 객체 저장소의 버킷과 접두사(§7.7)
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct S3Target {
    /// 끝점 URL(`https://host[:port]`). 시험만 `http`(`insecure_http`).
    pub endpoint: String,
    #[serde(default = "default_region")]
    #[cfg_attr(feature = "openapi", schema(default = "us-east-1"))]
    pub region: String,
    pub bucket: String,
    /// 버킷 안 접두사. 끝에 `/`를 붙여 쓴다. 없으면 버킷 맨 위다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub prefix: Option<String>,
    /// 경로 방식 주소(`{끝점}/{버킷}/…` — MinIO·Ceph RGW). 거짓이면 가상 호스트 방식(`{버킷}.{호스트}`).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub path_style: bool,
    /// 끝점 인증서를 확인할 사용자 CA(PEM). 없으면 시스템 루트.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ca_pem: Option<String>,
    pub access_key_id: String,
    /// 비밀 키. Raft에만 있고 API 보기·watch·감사·`Debug`에서 뺀다.
    pub secret_access_key: String,
    /// `http` 끝점을 허락한다(시험용 — 자격 증명과 데이터가 평문으로 오간다).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub insecure_http: bool,
}

fn default_region() -> String {
    "us-east-1".into()
}

impl std::fmt::Debug for S3Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Target")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .field("ca_pem", &self.ca_pem.as_ref().map(|_| "…"))
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("insecure_http", &self.insecure_http)
            .finish()
    }
}

/// 저장소의 암호화 키(§9). `key`는 주 키(base64 32바이트)이고 API 보기·watch·감사·`Debug`에서 뺀다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct StoreEncryption {
    /// 키 지문(`ab:cd:…`, 16바이트). 키를 알려 주지 않는다.
    pub fingerprint: String,
    pub key: String,
}

impl std::fmt::Debug for StoreEncryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreEncryption")
            .field("fingerprint", &self.fingerprint)
            .field("key", &"<redacted>")
            .finish()
    }
}

/// 예약 검증(§10.5)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VerifyPolicy {
    pub schedule: JobSchedule,
    /// 이 날 수보다 오래전에 검증한 백업을 다시 검증한다(1~3650).
    #[serde(default = "default_reverify_days")]
    #[cfg_attr(feature = "openapi", schema(default = 30))]
    pub reverify_days: u32,
    #[serde(default)]
    pub mode: VerifyMode,
    /// 백업이 `ready`가 되면 곧바로 그 백업을 검증한다.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub verify_new: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VerifyMode {
    /// 청크를 모두 읽어 인증 태그·압축·id를 다시 맞춘다.
    #[default]
    Full,
    /// 매니페스트·인덱스와 청크가 있는지(S3 `HEAD`)만 본다.
    Metadata,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BackupStoreError {
    #[error("backend.pool {0:?} is not a valid storage pool name")]
    Pool(String),
    #[error("s3.endpoint {0:?} must be an https URL with only a host and port")]
    Endpoint(String),
    #[error("s3.endpoint uses http: set s3.insecure_http to allow it (tests only)")]
    InsecureHttp,
    #[error("s3.region {0:?} must be 1-32 lowercase letters, digits and '-'")]
    Region(String),
    #[error(
        "s3.bucket {0:?} must be 3-63 lowercase letters, digits, '.' and '-', starting and ending with a letter or digit"
    )]
    Bucket(String),
    #[error(
        "s3.prefix {0:?} must be 1-512 characters without control characters, a leading '/', empty, '.' or '..' parts"
    )]
    Prefix(String),
    #[error("s3.ca_pem must hold a PEM certificate of at most 64 KiB")]
    CaPem,
    #[error("s3.access_key_id must be 1-128 printable characters")]
    AccessKeyId,
    #[error("s3.secret_access_key must be 1-256 printable characters")]
    SecretAccessKey,
    #[error("encryption.fingerprint {0:?} must be 16 bytes as lowercase hex pairs joined by ':'")]
    Fingerprint(String),
    #[error("encryption.key must be 32 bytes in base64")]
    Key,
    #[error("gc: {0}")]
    Gc(BackupJobError),
    #[error("verify.schedule: {0}")]
    VerifySchedule(BackupJobError),
    #[error("verify.reverify_days {0} is out of range: use 1-{MAX_REVERIFY_DAYS}")]
    ReverifyDays(u32),
    #[error("max_streams {0} is out of range: use 1-{MAX_STREAMS}")]
    MaxStreams(u8),
}

impl BackupStoreSpec {
    /// 형식만 본다(풀이 있는지, 끝점에 닿는지, 키가 저장소와 맞는지는 API·Task가 본다).
    pub fn validate(&self) -> Result<(), BackupStoreError> {
        match &self.backend {
            StoreBackend::Pool(pool) => {
                validate_name(pool).map_err(|_| BackupStoreError::Pool(pool.clone()))?
            }
            StoreBackend::S3(s3) => s3.validate()?,
        }
        if let Some(e) = &self.encryption {
            e.validate()?;
        }
        self.gc.validate().map_err(BackupStoreError::Gc)?;
        if let Some(v) = &self.verify {
            v.schedule
                .validate()
                .map_err(BackupStoreError::VerifySchedule)?;
            if !(1..=MAX_REVERIFY_DAYS).contains(&v.reverify_days) {
                return Err(BackupStoreError::ReverifyDays(v.reverify_days));
            }
        }
        if !(1..=MAX_STREAMS).contains(&self.max_streams) {
            return Err(BackupStoreError::MaxStreams(self.max_streams));
        }
        Ok(())
    }
}

fn printable(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_graphic())
}

/// 끝점 URL의 `host[:port]`(소문자, 기본 포트는 뺀다). 경로·질의·사용자 정보가 있으면 None.
fn endpoint_authority(endpoint: &str) -> Option<(String, String)> {
    let url = url::Url::parse(endpoint).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let authority = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    Some((url.scheme().to_string(), authority))
}

fn valid_bucket(b: &str) -> bool {
    let edge = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    (3..=63).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'-')
        && edge(b.bytes().next())
        && edge(b.bytes().last())
        && !b.contains("..")
}

fn valid_prefix(p: &str) -> bool {
    if p.is_empty() || p.len() > 512 || p.starts_with('/') || p.chars().any(char::is_control) {
        return false;
    }
    p.strip_suffix('/')
        .unwrap_or(p)
        .split('/')
        .all(|part| !matches!(part, "" | "." | ".."))
}

impl S3Target {
    pub fn validate(&self) -> Result<(), BackupStoreError> {
        let (scheme, _) = endpoint_authority(&self.endpoint)
            .ok_or_else(|| BackupStoreError::Endpoint(self.endpoint.clone()))?;
        if scheme == "http" && !self.insecure_http {
            return Err(BackupStoreError::InsecureHttp);
        }
        let region_ok = (1..=32).contains(&self.region.len())
            && self
                .region
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
        if !region_ok {
            return Err(BackupStoreError::Region(self.region.clone()));
        }
        if !valid_bucket(&self.bucket) {
            return Err(BackupStoreError::Bucket(self.bucket.clone()));
        }
        if let Some(p) = &self.prefix
            && !valid_prefix(p)
        {
            return Err(BackupStoreError::Prefix(p.clone()));
        }
        if let Some(ca) = &self.ca_pem
            && (ca.len() > 64 << 10 || !ca.contains("-----BEGIN CERTIFICATE-----"))
        {
            return Err(BackupStoreError::CaPem);
        }
        if !printable(&self.access_key_id, 128) {
            return Err(BackupStoreError::AccessKeyId);
        }
        if !printable(&self.secret_access_key, 256) {
            return Err(BackupStoreError::SecretAccessKey);
        }
        Ok(())
    }
}

impl StoreEncryption {
    pub fn validate(&self) -> Result<(), BackupStoreError> {
        if !valid_fingerprint(&self.fingerprint) {
            return Err(BackupStoreError::Fingerprint(self.fingerprint.clone()));
        }
        match STANDARD.decode(&self.key) {
            Ok(k) if k.len() == 32 => Ok(()),
            _ => Err(BackupStoreError::Key),
        }
    }
}

/// 키 지문 모양: 소문자 16진 두 자리 16개를 `:`로 이음(§9.1)
pub fn valid_fingerprint(fp: &str) -> bool {
    let parts: Vec<&str> = fp.split(':').collect();
    parts.len() == 16
        && parts.iter().all(|p| {
            p.len() == 2
                && p.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
}

/// Raft 겹침 키 `backup-store-target`(한 곳에 저장소 하나 — §6): `pool:<풀 이름>` 또는
/// `s3:<끝점 host[:port]>/<버킷>/<접두사>`(접두사 끝의 `/`는 뺀다). 풀 이름은 클러스터에서 고유하고 바뀌지 않는다.
pub fn store_target_key(backend: &StoreBackend) -> Option<String> {
    match backend {
        StoreBackend::Pool(pool) => Some(format!("pool:{pool}")),
        StoreBackend::S3(s3) => s3_key(&s3.endpoint, &s3.bucket, s3.prefix.as_deref()),
    }
}

/// [`store_target_key`]의 JSON 판(상태 머신 — 스펙 JSON의 `backend`를 본다)
pub fn store_target_key_value(spec: &Value) -> Option<String> {
    let backend = &spec["backend"];
    if let Some(pool) = backend["pool"].as_str() {
        return Some(format!("pool:{pool}"));
    }
    let s3 = &backend["s3"];
    s3_key(
        s3["endpoint"].as_str()?,
        s3["bucket"].as_str()?,
        s3["prefix"].as_str(),
    )
}

fn s3_key(endpoint: &str, bucket: &str, prefix: Option<&str>) -> Option<String> {
    let (_, authority) = endpoint_authority(endpoint)?;
    let prefix = prefix.unwrap_or("").trim_end_matches('/');
    Some(format!("s3:{authority}/{bucket}/{prefix}"))
}

/// 저장소의 실제 상태. 초기화·GC·검증 Task와 리더의 스케줄러가 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct BackupStoreStatus {
    pub phase: StorePhase,
    /// 저장소 `store.json`의 id. 이 클러스터가 만든 저장소는 리소스 id, 붙인 저장소는 남의 것이다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub store_id: Option<Uuid>,
    /// 공유가 아닌 백업 풀 안 저장소를 만든 노드(그 노드에만 있다 — §7.6). 공유 풀·S3면 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<Uuid>,
    /// 저장소 `owner.json`의 주인 클러스터(§12.6)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub owner: Option<StoreOwner>,
    /// 마지막 GC가 센 사용량
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub usage: Option<StoreUsage>,
    /// 진행 중인 GC(§10.2)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub gc: Option<GcRun>,
    /// GC가 청크를 지우는 중(쓰기 잠금 — `committing`이 비었을 때만 참이 된다)
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub sweeping: bool,
    /// 매니페스트를 쓰는 중인 백업(쓰기 잠금 — `sweeping`이 거짓일 때만 더한다, §10.3)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub committing: Vec<Uuid>,
    /// 쓸기를 시작할 때마다 1씩 오른다. 백업이 시작과 커밋 때 견준다.
    #[serde(skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub sweep_epoch: u64,
    /// 검증이 깨진 청크를 `bad/`로 옮기는 중(리뷰 1 I5 — 쓸기 잠금처럼 `committing`이 비었을 때만 참이 되고 그동안 커밋
    /// 구간에 새로 들지 않는다)
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub quarantining: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_gc: Option<GcResult>,
    /// 다음 예약 GC 시각(유닉스 초)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub next_gc: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_verify: Option<VerifyResult>,
    /// 다음 예약 검증 시각(유닉스 초)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub next_verify: Option<u64>,
    /// 검증이 찾아 `bad/`로 옮긴 청크 수
    #[serde(skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub bad_chunks: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum StorePhase {
    /// 초기화 Task가 `store.json`을 쓰는 중
    #[default]
    Initializing,
    Ready,
    Failed,
}

/// 저장소의 주인 클러스터(`owner.json`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StoreOwner {
    pub cluster: String,
    /// 주인 클러스터 CA의 지문(`sha256:…`)
    pub ca_fingerprint: String,
}

/// 저장소 사용량(GC가 셈)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct StoreUsage {
    /// 스냅샷(완성된 백업) 수
    pub snapshots: u64,
    /// 참조되는 청크 수
    pub chunks: u64,
    /// 청크가 차지한 바이트(압축·암호화 뒤)
    pub stored_bytes: u64,
    /// 스냅샷 디스크 가상 크기 합(dedup 배율 = logical / stored)
    pub logical_bytes: u64,
    /// 센 시각(유닉스 초)
    pub measured_at: u64,
}

/// 진행 중인 GC(§10.2)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GcRun {
    pub task: Uuid,
    /// 시작한 시각(유닉스 초, 노드 시계)
    pub started_at: u64,
    /// GC 표식의 저장소 시각 T0(유닉스 초, 저장소 시계)
    pub marker_at: u64,
    pub phase: GcPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum GcPhase {
    /// 인덱스를 읽어 쓰는 청크를 표시
    Mark,
    /// 시작 때 진행 중이던 백업이 끝나기를 기다림
    Wait,
    /// 표시되지 않은 오래된 청크를 지움
    Sweep,
}

/// 끝난 GC의 결과
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GcResult {
    pub started_at: u64,
    pub finished_at: u64,
    pub removed_chunks: u64,
    pub removed_bytes: u64,
    /// 표시되지 않았지만 GC 시작 뒤에 올라와 남긴 청크 수
    pub kept_young: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

/// 끝난 검증 실행의 결과
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VerifyResult {
    pub started_at: u64,
    pub finished_at: u64,
    /// 검증한 백업 수
    pub snapshots: u64,
    /// 확인한 청크 수
    pub chunks: u64,
    /// 깨진 청크 수
    pub bad: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

/// 노드 하나가 본 저장소(§6 — `NodeStatus.backup_stores`). 이 노드가 그 저장소를 쓸 수 있을 때만 60초마다
/// `store.json`을 읽어 적는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeStoreState {
    /// 저장소 리소스 id
    pub store: Uuid,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 확인한 시각(유닉스 초)
    pub checked_at: u64,
}

/// 라이브 복원 중인 VM의 상태(§11 — `VmStatus.live_restore`). 라이브 복원 Task가 쓰고, 갈아타기가 끝나면 지운다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LiveRestore {
    /// 되살리는 백업
    pub backup: Uuid,
    /// 라이브 복원 Task
    pub task: Uuid,
    pub phase: LiveRestorePhase,
    /// 대상 볼륨으로 옮긴 바이트(mirror 진행)
    #[serde(default)]
    pub done_bytes: u64,
    #[serde(default)]
    pub total_bytes: u64,
    /// 시작한 시각(유닉스 초)
    pub started_at: u64,
    /// 멈춘 까닭이나 실패한 이유
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 취소 요청(§11.4 `live-restore/cancel { discard: true }`): Task가 VM을 끄고 VM·대상 볼륨·오버레이를 지운다. 그동안
    /// reconciler는 VM을 켜지 않는다.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub cancel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum LiveRestorePhase {
    /// 볼륨·헬퍼·오버레이를 준비하고 VM을 켜는 중
    Starting,
    /// VM이 돌고 뒤에서 대상 볼륨으로 옮기는 중
    Copying,
    /// 다 옮겨 대상 볼륨으로 갈아타는 중
    Pivoting,
    /// 백업을 읽지 못해 VM이 멈춤(`rerror=stop`) — 저장소·헬퍼가 돌아오면 이어 감
    Paused,
    /// 강제로 끔(오버레이·대상 유지) — `vm start`가 이어 감
    Stopped,
    Failed,
}

/// 바뀐 범위(`(오프셋, 길이)` 바이트) → 그 범위가 닿는 청크 번호(오름차순, 겹치지 않음). 길이 0인 범위는 뺀다.
/// `chunk_bytes`는 0이 아니다.
pub fn chunk_ranges(extents: &[(u64, u64)], chunk_bytes: u64) -> Vec<u64> {
    let chunk_bytes = chunk_bytes.max(1);
    let mut spans: Vec<(u64, u64)> = extents
        .iter()
        .filter(|(_, len)| *len > 0)
        .map(|(off, len)| {
            let end = off.saturating_add(*len).saturating_sub(1);
            (off / chunk_bytes, end / chunk_bytes)
        })
        .collect();
    spans.sort_unstable();
    let mut out: Vec<u64> = Vec::new();
    for (first, last) in spans {
        let start = match out.last() {
            Some(&l) if l >= first => l + 1,
            _ => first,
        };
        out.extend(start..=last);
    }
    out
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MergeIndexError {
    #[error("the base index has {base} chunks but the disk has {count}")]
    SizeChanged { base: u64, count: u64 },
    #[error("{dirty} dirty chunks but {new} new ids")]
    Mismatch { dirty: usize, new: usize },
    #[error("dirty chunk {0} is outside the disk")]
    OutOfRange(u64),
}

/// 증분 백업의 완전한 인덱스(§8.3): 바뀐 청크(`dirty[j]`)는 새 id(`new[j]`), 나머지는 base 인덱스의 id를 그대로
/// 옮긴다. base와 이번 디스크의 청크 수(`count`)가 다르면(디스크 크기가 바뀜) 오류다 — 부모 고르기가 그런 base를 주지
/// 않는다.
pub fn merge_index(
    base: &[ChunkId],
    count: u64,
    dirty: &[u64],
    new: &[ChunkId],
) -> Result<Vec<ChunkId>, MergeIndexError> {
    if base.len() as u64 != count {
        return Err(MergeIndexError::SizeChanged {
            base: base.len() as u64,
            count,
        });
    }
    if dirty.len() != new.len() {
        return Err(MergeIndexError::Mismatch {
            dirty: dirty.len(),
            new: new.len(),
        });
    }
    let mut out = base.to_vec();
    for (&i, id) in dirty.iter().zip(new) {
        let slot = out.get_mut(i as usize).filter(|_| i < count);
        *slot.ok_or(MergeIndexError::OutOfRange(i))? = *id;
    }
    Ok(out)
}

/// 디스크 `virtual_bytes`의 청크 수(마지막 청크는 짧을 수 있다)
pub fn chunk_count(virtual_bytes: u64, chunk_bytes: u64) -> u64 {
    virtual_bytes.div_ceil(chunk_bytes.max(1))
}

pub mod verify {
    //! 검증 고르기(§10.5)

    use super::*;

    /// 저장소 `store`의 예약 검증 대상: `ready`인 그 저장소 백업 가운데 검증한 적이 없거나 마지막 검증이
    /// `reverify_days`보다 오래된 것. 검증한 적 없는 것부터, 그다음 오래 검증하지 않은 차례(같으면 id 차례)다.
    pub fn due(backups: &[Backup], store: &str, policy: &VerifyPolicy, now: u64) -> Vec<Uuid> {
        let cutoff = now.saturating_sub(u64::from(policy.reverify_days) * 86_400);
        let mut picked: Vec<(Option<u64>, Uuid)> = backups
            .iter()
            .filter(|b| b.status.phase == BackupPhase::Ready)
            .filter(|b| b.spec.target() == Ok(BackupTarget::Store(store)))
            .map(|b| (b.status.verify.as_ref().map(|v| v.at), b.id))
            .filter(|(at, _)| at.is_none_or(|at| at < cutoff))
            .collect();
        picked.sort();
        picked.into_iter().map(|(_, id)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::{BackupSpec, BackupStatus, BackupVerify, VerifyState};
    use crate::backup_job::Weekday;
    use serde_json::json;

    type Change = dyn Fn(&mut S3Target);
    type Extents = Vec<(u64, u64)>;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    const FP: &str = "ab:cd:ef:01:23:45:67:89:ab:cd:ef:01:23:45:67:89";

    fn s3() -> S3Target {
        S3Target {
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            bucket: "orbi-bk".into(),
            prefix: Some("orbi/".into()),
            path_style: true,
            ca_pem: None,
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            insecure_http: false,
        }
    }

    fn pool_spec() -> BackupStoreSpec {
        serde_json::from_value(json!({ "backend": { "pool": "bk" } })).unwrap()
    }

    #[test]
    fn spec_defaults_and_round_trips() {
        let s = pool_spec();
        assert_eq!(s.backend, StoreBackend::Pool("bk".into()));
        assert_eq!(s.gc, default_gc());
        assert_eq!(s.max_streams, 8);
        assert_eq!(s.encryption, None);
        assert!(!s.read_only);
        assert_eq!(s.validate(), Ok(()));
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({ "backend": { "pool": "bk" }, "gc": { "at": "05:00", "days": [] }, "max_streams": 8 })
        );

        let full = BackupStoreSpec {
            backend: StoreBackend::S3(s3()),
            encryption: Some(StoreEncryption {
                fingerprint: FP.into(),
                key: KEY.into(),
            }),
            read_only: true,
            gc: JobSchedule {
                at: "01:30".into(),
                days: vec![Weekday::Sat],
            },
            verify: Some(VerifyPolicy {
                schedule: JobSchedule {
                    at: "06:00".into(),
                    days: vec![Weekday::Sun],
                },
                reverify_days: 30,
                mode: VerifyMode::Metadata,
                verify_new: true,
            }),
            max_streams: 16,
        };
        assert_eq!(full.validate(), Ok(()));
        let j = serde_json::to_value(&full).unwrap();
        assert_eq!(j["backend"]["s3"]["bucket"], "orbi-bk");
        assert_eq!(j["backend"]["s3"]["path_style"], true);
        assert!(j["backend"]["s3"].get("insecure_http").is_none(), "{j}");
        assert_eq!(j["verify"]["mode"], "metadata");
        assert_eq!(serde_json::from_value::<BackupStoreSpec>(j).unwrap(), full);

        let v: VerifyPolicy =
            serde_json::from_value(json!({ "schedule": { "at": "06:00" } })).unwrap();
        assert_eq!(v.reverify_days, 30);
        assert_eq!(v.mode, VerifyMode::Full);
        assert!(!v.verify_new);
        let s: S3Target = serde_json::from_value(json!({
            "endpoint": "https://s3.example.com", "bucket": "bk",
            "access_key_id": "a", "secret_access_key": "b"
        }))
        .unwrap();
        assert_eq!(s.region, "us-east-1");
        for bad in [
            json!({ "backend": { "pool": "bk" }, "gcc": {} }),
            json!({ "backend": { "pool": "bk", "s3": s3() } }),
            json!({ "backend": { "nfs": "x" } }),
        ] {
            assert!(
                serde_json::from_value::<BackupStoreSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    /// 비밀(S3 비밀 키·주 키)은 `Debug`에 나오지 않는다.
    #[test]
    fn debug_hides_secrets() {
        let spec = BackupStoreSpec {
            backend: StoreBackend::S3(s3()),
            encryption: Some(StoreEncryption {
                fingerprint: FP.into(),
                key: KEY.into(),
            }),
            ..pool_spec()
        };
        let text = format!("{spec:?}");
        assert!(!text.contains("wJalrXUtnFEMI"), "{text}");
        assert!(!text.contains(KEY), "{text}");
        assert!(text.contains(FP) && text.contains("AKIDEXAMPLE"), "{text}");
    }

    #[test]
    fn validates_specs() {
        let with_s3 = |f: &dyn Fn(&mut S3Target)| {
            let mut t = s3();
            f(&mut t);
            BackupStoreSpec {
                backend: StoreBackend::S3(t),
                ..pool_spec()
            }
            .validate()
        };
        assert_eq!(with_s3(&|_| {}), Ok(()));
        let cases: Vec<(Box<Change>, BackupStoreError)> = vec![
            (
                Box::new(|t| t.endpoint = "s3.example.com".into()),
                BackupStoreError::Endpoint("s3.example.com".into()),
            ),
            (
                Box::new(|t| t.endpoint = "https://s3.example.com/bk".into()),
                BackupStoreError::Endpoint("https://s3.example.com/bk".into()),
            ),
            (
                Box::new(|t| t.endpoint = "https://u:p@s3.example.com".into()),
                BackupStoreError::Endpoint("https://u:p@s3.example.com".into()),
            ),
            (
                Box::new(|t| t.endpoint = "ftp://s3.example.com".into()),
                BackupStoreError::Endpoint("ftp://s3.example.com".into()),
            ),
            (
                Box::new(|t| t.endpoint = "http://minio:9000".into()),
                BackupStoreError::InsecureHttp,
            ),
            (
                Box::new(|t| t.region = "US East".into()),
                BackupStoreError::Region("US East".into()),
            ),
            (
                Box::new(|t| t.bucket = "Bk".into()),
                BackupStoreError::Bucket("Bk".into()),
            ),
            (
                Box::new(|t| t.bucket = "ab".into()),
                BackupStoreError::Bucket("ab".into()),
            ),
            (
                Box::new(|t| t.bucket = "-bk".into()),
                BackupStoreError::Bucket("-bk".into()),
            ),
            (
                Box::new(|t| t.prefix = Some("/a".into())),
                BackupStoreError::Prefix("/a".into()),
            ),
            (
                Box::new(|t| t.prefix = Some("a//b".into())),
                BackupStoreError::Prefix("a//b".into()),
            ),
            (
                Box::new(|t| t.prefix = Some("a/../b".into())),
                BackupStoreError::Prefix("a/../b".into()),
            ),
            (
                Box::new(|t| t.prefix = Some(String::new())),
                BackupStoreError::Prefix(String::new()),
            ),
            (
                Box::new(|t| t.ca_pem = Some("not a cert".into())),
                BackupStoreError::CaPem,
            ),
            (
                Box::new(|t| t.access_key_id = String::new()),
                BackupStoreError::AccessKeyId,
            ),
            (
                Box::new(|t| t.secret_access_key = "a b".into()),
                BackupStoreError::SecretAccessKey,
            ),
        ];
        for (f, want) in cases {
            assert_eq!(with_s3(&*f), Err(want));
        }
        assert_eq!(
            with_s3(&|t| {
                t.endpoint = "http://127.0.0.1:9000".into();
                t.insecure_http = true;
                t.prefix = Some("a/b/".into());
                t.ca_pem = Some("-----BEGIN CERTIFICATE-----\nx\n".into());
            }),
            Ok(())
        );

        let base = pool_spec();
        let check = |s: BackupStoreSpec| s.validate();
        assert_eq!(
            check(BackupStoreSpec {
                backend: StoreBackend::Pool("../bk".into()),
                ..base.clone()
            }),
            Err(BackupStoreError::Pool("../bk".into()))
        );
        for (fp, key, want) in [
            ("ab:cd", KEY, BackupStoreError::Fingerprint("ab:cd".into())),
            (
                &FP.to_uppercase(),
                KEY,
                BackupStoreError::Fingerprint(FP.to_uppercase()),
            ),
            (FP, "AAEC", BackupStoreError::Key),
            (FP, "not base64!", BackupStoreError::Key),
        ] {
            assert_eq!(
                check(BackupStoreSpec {
                    encryption: Some(StoreEncryption {
                        fingerprint: fp.into(),
                        key: key.into()
                    }),
                    ..base.clone()
                }),
                Err(want)
            );
        }
        assert_eq!(
            check(BackupStoreSpec {
                gc: JobSchedule {
                    at: "5:00".into(),
                    days: vec![]
                },
                ..base.clone()
            }),
            Err(BackupStoreError::Gc(BackupJobError::At))
        );
        let policy = |days: u32, at: &str| VerifyPolicy {
            schedule: JobSchedule {
                at: at.into(),
                days: vec![],
            },
            reverify_days: days,
            mode: VerifyMode::Full,
            verify_new: false,
        };
        assert_eq!(
            check(BackupStoreSpec {
                verify: Some(policy(0, "06:00")),
                ..base.clone()
            }),
            Err(BackupStoreError::ReverifyDays(0))
        );
        assert_eq!(
            check(BackupStoreSpec {
                verify: Some(policy(30, "6")),
                ..base.clone()
            }),
            Err(BackupStoreError::VerifySchedule(BackupJobError::At))
        );
        for n in [0, 65] {
            assert_eq!(
                check(BackupStoreSpec {
                    max_streams: n,
                    ..base.clone()
                }),
                Err(BackupStoreError::MaxStreams(n))
            );
        }
    }

    #[test]
    fn target_keys_name_one_place() {
        assert_eq!(
            store_target_key(&StoreBackend::Pool("bk".into())).as_deref(),
            Some("pool:bk")
        );
        assert_eq!(
            store_target_key(&StoreBackend::S3(s3())).as_deref(),
            Some("s3:s3.example.com/orbi-bk/orbi")
        );
        // 같은 곳의 다른 표기(대문자 호스트, 기본 포트, 끝 `/`)는 같은 키다.
        let mut t = s3();
        t.endpoint = "https://S3.Example.com:443/".into();
        t.prefix = Some("orbi".into());
        assert_eq!(
            store_target_key(&StoreBackend::S3(t.clone())).as_deref(),
            Some("s3:s3.example.com/orbi-bk/orbi")
        );
        t.endpoint = "http://minio:9000".into();
        t.prefix = None;
        assert_eq!(
            store_target_key(&StoreBackend::S3(t)).as_deref(),
            Some("s3:minio:9000/orbi-bk/")
        );
        for spec in [
            pool_spec(),
            BackupStoreSpec {
                backend: StoreBackend::S3(s3()),
                ..pool_spec()
            },
        ] {
            let v = serde_json::to_value(&spec).unwrap();
            assert_eq!(store_target_key_value(&v), store_target_key(&spec.backend));
        }
        assert_eq!(store_target_key_value(&json!({})), None);
    }

    #[test]
    fn status_omits_empty_fields_and_round_trips() {
        assert_eq!(
            serde_json::to_value(BackupStoreStatus::default()).unwrap(),
            json!({ "phase": "initializing" })
        );
        let task = Uuid::now_v7();
        let backup = Uuid::now_v7();
        let status = BackupStoreStatus {
            phase: StorePhase::Ready,
            store_id: Some(Uuid::now_v7()),
            node: Some(Uuid::now_v7()),
            owner: Some(StoreOwner {
                cluster: "prod".into(),
                ca_fingerprint: format!("sha256:{}", "ab".repeat(32)),
            }),
            usage: Some(StoreUsage {
                snapshots: 3,
                chunks: 100,
                stored_bytes: 300 << 20,
                logical_bytes: 60 << 30,
                measured_at: 1_790_000_000,
            }),
            gc: Some(GcRun {
                task,
                started_at: 1_790_000_000,
                marker_at: 1_790_000_001,
                phase: GcPhase::Wait,
            }),
            sweeping: true,
            committing: vec![backup],
            sweep_epoch: 4,
            quarantining: true,
            last_gc: Some(GcResult {
                started_at: 1,
                finished_at: 2,
                removed_chunks: 1203,
                removed_bytes: 4 << 30,
                kept_young: 2,
                error: None,
            }),
            next_gc: Some(1_790_086_400),
            last_verify: Some(VerifyResult {
                started_at: 1,
                finished_at: 2,
                snapshots: 3,
                chunks: 100,
                bad: 1,
                error: None,
            }),
            next_verify: Some(1_790_500_000),
            bad_chunks: 1,
            error: None,
        };
        let j = serde_json::to_value(&status).unwrap();
        assert_eq!(j["gc"]["phase"], "wait");
        assert_eq!(j["committing"], json!([backup]));
        assert!(j["last_gc"].get("error").is_none(), "{j}");
        assert_eq!(
            serde_json::from_value::<BackupStoreStatus>(j).unwrap(),
            status
        );
        assert_eq!(Kind::BackupStore.as_str(), "backup-store");
        assert_eq!(
            serde_json::to_value(Kind::BackupStore).unwrap(),
            "backup-store"
        );
    }

    #[test]
    fn live_restore_round_trips() {
        let lr = LiveRestore {
            backup: Uuid::now_v7(),
            task: Uuid::now_v7(),
            phase: LiveRestorePhase::Paused,
            done_bytes: 1 << 30,
            total_bytes: 20 << 30,
            started_at: 1_790_000_000,
            error: Some("backup store unreachable".into()),
            cancel: false,
        };
        let j = serde_json::to_value(&lr).unwrap();
        assert_eq!(j["phase"], "paused");
        // 취소 요청은 있을 때만 쓴다.
        assert!(j.get("cancel").is_none(), "{j}");
        assert_eq!(serde_json::from_value::<LiveRestore>(j).unwrap(), lr);
        let lr = LiveRestore { cancel: true, ..lr };
        let j = serde_json::to_value(&lr).unwrap();
        assert_eq!(j["cancel"], true);
        assert_eq!(serde_json::from_value::<LiveRestore>(j).unwrap(), lr);
        for (p, s) in [
            (LiveRestorePhase::Starting, "starting"),
            (LiveRestorePhase::Copying, "copying"),
            (LiveRestorePhase::Pivoting, "pivoting"),
            (LiveRestorePhase::Stopped, "stopped"),
            (LiveRestorePhase::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(p).unwrap(), s);
        }
    }

    #[test]
    fn chunk_ranges_map_extents_to_chunks() {
        const C: u64 = CHUNK_BYTES;
        let cases: Vec<(Extents, Vec<u64>)> = vec![
            (vec![], vec![]),
            (vec![(0, 0)], vec![]),
            (vec![(0, 1)], vec![0]),
            (vec![(0, C)], vec![0]),
            (vec![(0, C + 1)], vec![0, 1]),
            (vec![(C - 1, 2)], vec![0, 1]),
            (vec![(3 * C, 64 << 10), (C, 64 << 10)], vec![1, 3]),
            // 겹치고 이어지는 범위는 한 번씩
            (vec![(0, 2 * C), (C, 3 * C), (C + 5, 1)], vec![0, 1, 2, 3]),
            (vec![(10 * C + 7, 1), (10 * C, 1)], vec![10]),
        ];
        for (extents, want) in cases {
            assert_eq!(chunk_ranges(&extents, C), want, "{extents:?}");
        }
        assert_eq!(chunk_ranges(&[(u64::MAX - 1, 10)], C).len(), 1);
        assert_eq!(chunk_count(0, C), 0);
        assert_eq!(chunk_count(1, C), 1);
        assert_eq!(chunk_count(C, C), 1);
        assert_eq!(chunk_count(20 << 30, C), 5120);
    }

    #[test]
    fn merge_index_keeps_base_ids_for_clean_chunks() {
        let id = |n: u8| [n; 32];
        let base = vec![id(1), ZERO_CHUNK, id(3), id(4)];
        assert_eq!(
            merge_index(&base, 4, &[1, 3], &[id(9), ZERO_CHUNK]),
            Ok(vec![id(1), id(9), id(3), ZERO_CHUNK])
        );
        assert_eq!(merge_index(&base, 4, &[], &[]), Ok(base.clone()));
        assert_eq!(
            merge_index(&base, 5, &[4], &[id(5)]),
            Err(MergeIndexError::SizeChanged { base: 4, count: 5 })
        );
        assert_eq!(
            merge_index(&base, 4, &[1], &[]),
            Err(MergeIndexError::Mismatch { dirty: 1, new: 0 })
        );
        assert_eq!(
            merge_index(&base, 4, &[4], &[id(5)]),
            Err(MergeIndexError::OutOfRange(4))
        );
    }

    #[test]
    fn verify_due_picks_unverified_and_stale_ready_backups() {
        const DAY: u64 = 86_400;
        let now = 1_790_000_000;
        let policy = VerifyPolicy {
            schedule: default_gc(),
            reverify_days: 30,
            mode: VerifyMode::Full,
            verify_new: false,
        };
        let vm_spec: crate::vm::VmSpec = serde_json::from_value(json!({
            "cpu": { "cores": 1 }, "memory_mib": 512, "machine": "virt"
        }))
        .unwrap();
        let mk = |store: Option<&str>, phase: BackupPhase, at: Option<u64>| -> Backup {
            let mut b: Backup = Resource::new(
                format!("b-{}", Uuid::now_v7().simple()),
                BackupSpec {
                    vm: Uuid::now_v7(),
                    vm_name: "web".into(),
                    pool: if store.is_some() {
                        String::new()
                    } else {
                        "bk".into()
                    },
                    store: store.map(Into::into),
                    source: None,
                    vm_spec: vm_spec.clone(),
                    job: None,
                    origin: None,
                    incremental: false,
                    max_chain: None,
                },
            );
            b.status = BackupStatus {
                phase,
                verify: at.map(|at| BackupVerify {
                    at,
                    state: VerifyState::Ok,
                    bad_chunks: 0,
                    error: None,
                }),
                ..Default::default()
            };
            b
        };
        let stale = mk(Some("st"), BackupPhase::Ready, Some(now - 31 * DAY));
        let staler = mk(Some("st"), BackupPhase::Ready, Some(now - 40 * DAY));
        let never = mk(Some("st"), BackupPhase::Ready, None);
        let fresh = mk(Some("st"), BackupPhase::Ready, Some(now - DAY));
        let pending = mk(Some("st"), BackupPhase::Pending, None);
        let other = mk(Some("s3"), BackupPhase::Ready, None);
        let pool = mk(None, BackupPhase::Ready, None);
        let all = vec![
            stale.clone(),
            fresh,
            staler.clone(),
            pending,
            other,
            pool,
            never.clone(),
        ];
        assert_eq!(
            verify::due(&all, "st", &policy, now),
            vec![never.id, staler.id, stale.id]
        );
        assert!(verify::due(&all, "none", &policy, now).is_empty());
    }
}
