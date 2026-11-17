//! 이미지 라이브러리(계획 58): 잘 알려진 클라우드 이미지의 내장 목록과 관리자 항목, 체크섬 파일 해석.
//!
//! 항목은 `(id, arch)`로 하나다 — 같은 id가 아키텍처마다 한 줄씩 있다(`ubuntu-24.04` x86_64·aarch64).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::storage::SourceFormat;
use crate::vm::{Arch, Firmware};

/// 관리자 항목의 최대 수
pub const MAX_CUSTOM_IMAGES: usize = 200;

/// 받는 체크섬 파일의 최대 크기
pub const MAX_CHECKSUM_FILE_BYTES: usize = 1024 * 1024;

/// 항목 id의 최대 길이: aarch64 볼륨 이름(`-arm64`를 붙임)도 리소스 이름의 63자에 들게 한다.
pub const MAX_ID_LEN: usize = 57;

/// 제목·배포판·버전·기본 사용자의 최대 글자 수
const MAX_TITLE_CHARS: usize = 128;
const MAX_LABEL_CHARS: usize = 64;
const MAX_USER_CHARS: usize = 32;

/// 카탈로그 이미지 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CatalogImage {
    /// 항목 id(소문자·숫자·`.`·`-`, 57자까지, 소문자·숫자로 시작하고 끝남). 아키텍처마다 같은 id를 쓴다.
    pub id: String,
    pub title: String,
    /// 배포판(예: `ubuntu`)
    pub os: String,
    pub version: String,
    pub arch: Arch,
    /// 이미지 URL(`http`·`https`, 사용자 정보 없음)
    pub url: String,
    pub checksum: Checksum,
    /// `qcow2` 또는 `raw`
    pub source_format: SourceFormat,
    pub firmware: Firmware,
    /// cloud-init 기본 사용자 이름(예: `ubuntu`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub default_user: Option<String>,
    /// 이미지의 디스크 크기(가상 크기, 바이트). 모르면 0이다.
    #[serde(default)]
    pub min_size_bytes: u64,
}

/// 받은 이미지를 확인하는 방법
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum Checksum {
    /// 고정 SHA-256(16진 64자)
    Sha256 { value: String },
    /// 고정 SHA-512(16진 128자)
    Sha512 { value: String },
    /// 이미지와 같은 곳의 체크섬 파일(`SHA256SUMS`·`SHA512SUMS`·`CHECKSUM`·`.sha512`). 받을 때 이미지 파일 이름의
    /// 줄을 찾는다([`parse_checksums`]).
    Sums { url: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error(
        "image id {0:?} must have 1-57 lowercase letters, digits, '.' and '-', starting and ending with a letter or digit"
    )]
    Id(String),
    #[error("image {0}: title, os and version must not be empty")]
    Empty(String),
    #[error("image {0}: {1} must have at most {2} characters")]
    TooLong(String, &'static str, usize),
    #[error("image {0}: url {1:?} must be an http or https URL ending in a file name")]
    Url(String, String),
    #[error("image {0}: urls must not contain a user name or password")]
    UserInfo(String),
    #[error("image {0}: checksum {1:?} must be {2} hexadecimal digits")]
    Digest(String, String, usize),
    #[error("image {0}: checksum url {1:?} must be an http or https URL")]
    SumsUrl(String, String),
    #[error("image {0}: source_format must be qcow2 or raw")]
    SourceFormat(String),
    #[error("image {0}: aarch64 images need uefi firmware")]
    Aarch64Bios(String),
    #[error("image id {0:?} is a built-in image")]
    Builtin(String),
    #[error("image {0} ({1:?}) is listed twice")]
    Duplicate(String, Arch),
    #[error("the image catalog can have at most {MAX_CUSTOM_IMAGES} custom images")]
    TooMany,
}

impl CatalogImage {
    pub fn validate(&self) -> Result<(), CatalogError> {
        let id = &self.id;
        if !valid_id(id) {
            return Err(CatalogError::Id(id.clone()));
        }
        if [&self.title, &self.os, &self.version]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err(CatalogError::Empty(id.clone()));
        }
        for (field, value, max) in [
            ("title", Some(&self.title), MAX_TITLE_CHARS),
            ("os", Some(&self.os), MAX_LABEL_CHARS),
            ("version", Some(&self.version), MAX_LABEL_CHARS),
            ("default_user", self.default_user.as_ref(), MAX_USER_CHARS),
        ] {
            if value.is_some_and(|v| v.chars().count() > max) {
                return Err(CatalogError::TooLong(id.clone(), field, max));
            }
        }
        // 카탈로그는 로그인한 누구나 읽으므로 URL에 비밀(사용자 정보)을 담지 않는다(쿼리는 API가 관리자 아니면 가린다).
        let sums = match &self.checksum {
            Checksum::Sums { url } => Some(url),
            _ => None,
        };
        for url in std::iter::once(&self.url).chain(sums) {
            if parse_http(url).is_some_and(|u| !u.username().is_empty() || u.password().is_some()) {
                return Err(CatalogError::UserInfo(id.clone()));
            }
        }
        if parse_http(&self.url).is_none() || file_name_of(&self.url).is_none() {
            return Err(CatalogError::Url(id.clone(), self.url.clone()));
        }
        match &self.checksum {
            Checksum::Sha256 { value } if !is_hex(value, 64) => {
                return Err(CatalogError::Digest(id.clone(), value.clone(), 64));
            }
            Checksum::Sha512 { value } if !is_hex(value, 128) => {
                return Err(CatalogError::Digest(id.clone(), value.clone(), 128));
            }
            Checksum::Sums { url } if parse_http(url).is_none() => {
                return Err(CatalogError::SumsUrl(id.clone(), url.clone()));
            }
            _ => {}
        }
        if self.source_format == SourceFormat::Vmdk {
            return Err(CatalogError::SourceFormat(id.clone()));
        }
        if self.arch == Arch::Aarch64 && self.firmware == Firmware::Bios {
            return Err(CatalogError::Aarch64Bios(id.clone()));
        }
        Ok(())
    }

    /// 받은 볼륨의 기본 이름: id의 `.`을 `-`로, aarch64면 `-arm64`를 붙인다(`ubuntu-24-04-arm64`).
    pub fn default_volume_name(&self) -> String {
        let name = self.id.replace('.', "-");
        match self.arch {
            Arch::X86_64 => name,
            Arch::Aarch64 => format!("{name}-arm64"),
        }
    }
}

/// 관리자 항목(`SettingsSpec.image_catalog`) 검사: 각 항목, 개수, 내장 id와 겹침, `(id, arch)` 겹침
pub fn validate_custom(images: &[CatalogImage]) -> Result<(), CatalogError> {
    if images.len() > MAX_CUSTOM_IMAGES {
        return Err(CatalogError::TooMany);
    }
    let builtin: BTreeSet<&str> = BUILTIN.iter().map(|b| b.id).collect();
    let mut seen = BTreeSet::new();
    for image in images {
        image.validate()?;
        if builtin.contains(image.id.as_str()) {
            return Err(CatalogError::Builtin(image.id.clone()));
        }
        if !seen.insert((image.id.as_str(), image.arch == Arch::X86_64)) {
            return Err(CatalogError::Duplicate(image.id.clone(), image.arch));
        }
    }
    Ok(())
}

/// [`CatalogImage::default_volume_name`]이 늘 리소스 이름이 되는 id: `.`을 `-`로 바꾸면 리소스 이름이고(소문자·숫자·`-`,
/// `-`로 시작하거나 끝나지 않음) `-arm64`를 붙여도 63자 안이며 id처럼 보이지 않는다.
fn valid_id(id: &str) -> bool {
    let name = id.replace('.', "-");
    id.len() <= MAX_ID_LEN
        && crate::resource::validate_name(&name).is_ok()
        && uuid::Uuid::try_parse(&name).is_err()
}

/// 호스트가 있는 `http`·`https` URL이면 그 해석. 제어 문자·공백과 `https:///x`처럼 호스트 자리가 빈 모양은 URL
/// 해석기가 조용히 고치므로 먼저 거절한다.
fn parse_http(url: &str) -> Option<url::Url> {
    let (_, rest) = url.split_once("://")?;
    if rest.is_empty()
        || rest.starts_with('/')
        || url.bytes().any(|b| b.is_ascii_control() || b == b' ')
    {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    (matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|h| !h.is_empty()))
    .then_some(parsed)
}

/// `s`가 16진 `len`자인지(대소문자 모두)
pub fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// URL 경로의 마지막 조각(퍼센트 디코딩). 체크섬 파일에서 찾을 이미지 파일 이름이다. 경로가 없거나 `/`로 끝나면
/// 없음이다.
pub fn file_name_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (_, path) = rest.split_once('/')?;
    let last = path.rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    percent_decode(last)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let name = String::from_utf8(out).ok()?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

/// 체크섬 파일 `text`에서 `file_name`의 해시를 찾는다. 받는 모양: GNU `해시  이름`·`해시 *이름`, BSD
/// `SHA256 (이름) = 해시`·`SHA512 (이름) = 해시`, 해시 하나만 있는 파일(Alpine `.sha512`). OpenPGP clearsign
/// 머리·서명은 건너뛰고 `#` 줄은 주석이다. 길이 64는 SHA-256, 128은 SHA-512이고 값은 소문자로 돌려준다.
pub fn parse_checksums(text: &str, file_name: &str) -> Option<Checksum> {
    let mut words = text.split_whitespace();
    if let (Some(only), None) = (words.next(), words.next()) {
        return digest(None, only);
    }
    let mut in_signature = false;
    let mut in_header = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") {
            in_header = true;
            continue;
        }
        if in_header {
            // 머리(`Hash: SHA256`)는 빈 줄에서 끝난다.
            in_header = !line.trim().is_empty();
            continue;
        }
        if line.starts_with("-----BEGIN PGP SIGNATURE-----") {
            in_signature = true;
            continue;
        }
        if line.starts_with("-----END PGP SIGNATURE-----") {
            in_signature = false;
            continue;
        }
        if in_signature {
            continue;
        }
        // clearsign은 `-`로 시작하는 줄 앞에 `- `를 붙인다.
        let line = line.strip_prefix("- ").unwrap_or(line).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((head, hash)) = line.rsplit_once(" = ") {
            let Some((algo, rest)) = head.split_once(" (") else {
                continue;
            };
            if rest.strip_suffix(')') == Some(file_name) {
                return digest(Some(algo.trim()), hash.trim());
            }
            continue;
        }
        let Some((hash, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim_start();
        if name.strip_prefix('*').unwrap_or(name) == file_name {
            return digest(None, hash);
        }
    }
    None
}

/// 16진 해시를 길이(와 BSD 줄의 알고리즘 이름)로 가린다.
fn digest(algo: Option<&str>, hash: &str) -> Option<Checksum> {
    let value = hash.to_ascii_lowercase();
    match (algo, hash.len()) {
        (None | Some("SHA256"), 64) if is_hex(hash, 64) => Some(Checksum::Sha256 { value }),
        (None | Some("SHA512"), 128) if is_hex(hash, 128) => Some(Checksum::Sha512 { value }),
        _ => None,
    }
}

/// 내장 항목 한 줄(정적 데이터). 늘 같은 디렉터리의 체크섬 파일로 확인한다.
pub struct Builtin {
    pub id: &'static str,
    pub title: &'static str,
    pub os: &'static str,
    pub version: &'static str,
    pub arch: Arch,
    pub url: &'static str,
    pub sums: &'static str,
    pub firmware: Firmware,
    pub default_user: &'static str,
    pub min_size_bytes: u64,
}

impl Builtin {
    pub fn image(&self) -> CatalogImage {
        CatalogImage {
            id: self.id.into(),
            title: self.title.into(),
            os: self.os.into(),
            version: self.version.into(),
            arch: self.arch,
            url: self.url.into(),
            checksum: Checksum::Sums {
                url: self.sums.into(),
            },
            source_format: SourceFormat::Qcow2,
            firmware: self.firmware,
            default_user: Some(self.default_user.into()),
            min_size_bytes: self.min_size_bytes,
        }
    }
}

/// 내장 목록을 [`CatalogImage`]로
pub fn builtin() -> Vec<CatalogImage> {
    BUILTIN.iter().map(Builtin::image).collect()
}

/// `images`(내장 + 관리자)에서 `(id, arch)` 항목을 찾는다.
pub fn find<'a>(images: &'a [CatalogImage], id: &str, arch: Arch) -> Option<&'a CatalogImage> {
    images.iter().find(|i| i.id == id && i.arch == arch)
}

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

macro_rules! builtin {
    ($id:literal, $title:literal, $os:literal, $version:literal, $arch:ident, $firmware:ident,
     $user:literal, $size:expr, $url:expr, $sums:expr) => {
        Builtin {
            id: $id,
            title: $title,
            os: $os,
            version: $version,
            arch: Arch::$arch,
            url: $url,
            sums: $sums,
            firmware: Firmware::$firmware,
            default_user: $user,
            min_size_bytes: $size,
        }
    };
}

/// 내장 목록. "release·latest" 경로와 같은 디렉터리의 체크섬 파일을 써서 고정 해시가 낡지 않는다. 판 번호가 URL에
/// 있는 Fedora·Alpine은 Orbivisor 판마다 고친다(`.github/workflows/catalog.yml`이 주마다 링크를 본다).
pub const BUILTIN: &[Builtin] = &[
    builtin!(
        "ubuntu-24.04",
        "Ubuntu 24.04 LTS",
        "ubuntu",
        "24.04",
        X86_64,
        Uefi,
        "ubuntu",
        3584 * MIB,
        "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img",
        "https://cloud-images.ubuntu.com/releases/24.04/release/SHA256SUMS"
    ),
    builtin!(
        "ubuntu-24.04",
        "Ubuntu 24.04 LTS",
        "ubuntu",
        "24.04",
        Aarch64,
        Uefi,
        "ubuntu",
        3584 * MIB,
        "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-arm64.img",
        "https://cloud-images.ubuntu.com/releases/24.04/release/SHA256SUMS"
    ),
    builtin!(
        "ubuntu-22.04",
        "Ubuntu 22.04 LTS",
        "ubuntu",
        "22.04",
        X86_64,
        Uefi,
        "ubuntu",
        2252 * MIB,
        "https://cloud-images.ubuntu.com/releases/22.04/release/ubuntu-22.04-server-cloudimg-amd64.img",
        "https://cloud-images.ubuntu.com/releases/22.04/release/SHA256SUMS"
    ),
    builtin!(
        "ubuntu-22.04",
        "Ubuntu 22.04 LTS",
        "ubuntu",
        "22.04",
        Aarch64,
        Uefi,
        "ubuntu",
        2252 * MIB,
        "https://cloud-images.ubuntu.com/releases/22.04/release/ubuntu-22.04-server-cloudimg-arm64.img",
        "https://cloud-images.ubuntu.com/releases/22.04/release/SHA256SUMS"
    ),
    builtin!(
        "debian-12",
        "Debian 12 (bookworm)",
        "debian",
        "12",
        X86_64,
        Uefi,
        "debian",
        3 * GIB,
        "https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-genericcloud-amd64.qcow2",
        "https://cloud.debian.org/images/cloud/bookworm/latest/SHA512SUMS"
    ),
    builtin!(
        "debian-12",
        "Debian 12 (bookworm)",
        "debian",
        "12",
        Aarch64,
        Uefi,
        "debian",
        3 * GIB,
        "https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-genericcloud-arm64.qcow2",
        "https://cloud.debian.org/images/cloud/bookworm/latest/SHA512SUMS"
    ),
    builtin!(
        "rocky-9",
        "Rocky Linux 9",
        "rocky",
        "9",
        X86_64,
        Uefi,
        "rocky",
        10 * GIB,
        "https://dl.rockylinux.org/pub/rocky/9/images/x86_64/Rocky-9-GenericCloud-Base.latest.x86_64.qcow2",
        "https://dl.rockylinux.org/pub/rocky/9/images/x86_64/CHECKSUM"
    ),
    builtin!(
        "rocky-9",
        "Rocky Linux 9",
        "rocky",
        "9",
        Aarch64,
        Uefi,
        "rocky",
        10 * GIB,
        "https://dl.rockylinux.org/pub/rocky/9/images/aarch64/Rocky-9-GenericCloud-Base.latest.aarch64.qcow2",
        "https://dl.rockylinux.org/pub/rocky/9/images/aarch64/CHECKSUM"
    ),
    builtin!(
        "alma-9",
        "AlmaLinux 9",
        "almalinux",
        "9",
        X86_64,
        Uefi,
        "almalinux",
        10 * GIB,
        "https://repo.almalinux.org/almalinux/9/cloud/x86_64/images/AlmaLinux-9-GenericCloud-latest.x86_64.qcow2",
        "https://repo.almalinux.org/almalinux/9/cloud/x86_64/images/CHECKSUM"
    ),
    builtin!(
        "alma-9",
        "AlmaLinux 9",
        "almalinux",
        "9",
        Aarch64,
        Uefi,
        "almalinux",
        10 * GIB,
        "https://repo.almalinux.org/almalinux/9/cloud/aarch64/images/AlmaLinux-9-GenericCloud-latest.aarch64.qcow2",
        "https://repo.almalinux.org/almalinux/9/cloud/aarch64/images/CHECKSUM"
    ),
    builtin!(
        "fedora-45",
        "Fedora Cloud 45",
        "fedora",
        "45",
        X86_64,
        Uefi,
        "fedora",
        5 * GIB,
        "https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Cloud/x86_64/images/Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2",
        "https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Cloud/x86_64/images/Fedora-Cloud-45-1.4-x86_64-CHECKSUM"
    ),
    builtin!(
        "fedora-45",
        "Fedora Cloud 45",
        "fedora",
        "45",
        Aarch64,
        Uefi,
        "fedora",
        5 * GIB,
        "https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Cloud/aarch64/images/Fedora-Cloud-Base-Generic-45-1.4.aarch64.qcow2",
        "https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Cloud/aarch64/images/Fedora-Cloud-45-1.4-aarch64-CHECKSUM"
    ),
    builtin!(
        "alpine-3.20",
        "Alpine Linux 3.20",
        "alpine",
        "3.20.10",
        X86_64,
        Bios,
        "alpine",
        200 * MIB,
        "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/cloud/nocloud_alpine-3.20.10-x86_64-bios-cloudinit-r0.qcow2",
        "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/cloud/nocloud_alpine-3.20.10-x86_64-bios-cloudinit-r0.qcow2.sha512"
    ),
    builtin!(
        "alpine-3.20",
        "Alpine Linux 3.20",
        "alpine",
        "3.20.10",
        Aarch64,
        Uefi,
        "alpine",
        246 * MIB,
        "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/cloud/nocloud_alpine-3.20.10-aarch64-uefi-cloudinit-r0.qcow2",
        "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/cloud/nocloud_alpine-3.20.10-aarch64-uefi-cloudinit-r0.qcow2.sha512"
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    const H64: &str = "04e8f2461a16a44e78a370284471f29616cdd9baf1893df25807b71f985b6c94";

    fn h128(c: char) -> String {
        c.to_string().repeat(128)
    }

    fn image() -> CatalogImage {
        CatalogImage {
            id: "corp-ubuntu".into(),
            title: "Corp Ubuntu".into(),
            os: "ubuntu".into(),
            version: "24.04".into(),
            arch: Arch::X86_64,
            url: "https://mirror.corp/img/corp-ubuntu.qcow2".into(),
            checksum: Checksum::Sha256 { value: H64.into() },
            source_format: SourceFormat::Qcow2,
            firmware: Firmware::Uefi,
            default_user: None,
            min_size_bytes: 0,
        }
    }

    #[test]
    fn ubuntu_sha256sums_have_star_names() {
        let text = format!(
            "00b9709d33192c8cfb987fd9522e9a035c73fd125c5a479b125b5d26ffafec67 *ubuntu-24.04-server-cloudimg-amd64.squashfs\n\
             {H64} *ubuntu-24.04-server-cloudimg-amd64.img\n\
             50e24e1fb79e89116e939d90aa7738f7af48c442fa1bc2510131e0b39c50348e *ubuntu-24.04-server-cloudimg-arm64.img\n"
        );
        assert_eq!(
            parse_checksums(&text, "ubuntu-24.04-server-cloudimg-amd64.img"),
            Some(Checksum::Sha256 { value: H64.into() })
        );
        assert_eq!(
            parse_checksums(&text, "ubuntu-24.04-server-cloudimg-amd64"),
            None
        );
        assert_eq!(parse_checksums(&text, "cloudimg-amd64.img"), None);
    }

    #[test]
    fn debian_sha512sums_have_two_spaces() {
        let text = format!(
            "{}  debian-12-genericcloud-amd64.json\n{}  debian-12-genericcloud-amd64.qcow2\n",
            h128('a'),
            h128('B')
        );
        assert_eq!(
            parse_checksums(&text, "debian-12-genericcloud-amd64.qcow2"),
            Some(Checksum::Sha512 { value: h128('b') })
        );
    }

    #[test]
    fn rocky_checksum_is_bsd_style_with_comments() {
        let text = format!(
            "# Rocky-9-GenericCloud-Base-9.9-20261112.0.x86_64.qcow2: 649199616 bytes\n\
             SHA256 (Rocky-9-GenericCloud-Base-9.9-20261112.0.x86_64.qcow2) = {}\n\
             # Rocky-9-GenericCloud-Base.latest.x86_64.qcow2: 649199616 bytes\n\
             SHA256 (Rocky-9-GenericCloud-Base.latest.x86_64.qcow2) = {H64}\n",
            "1".repeat(64)
        );
        assert_eq!(
            parse_checksums(&text, "Rocky-9-GenericCloud-Base.latest.x86_64.qcow2"),
            Some(Checksum::Sha256 { value: H64.into() })
        );
        // 알고리즘 이름과 길이가 맞지 않으면 없다.
        let wrong = format!("SHA512 (a.qcow2) = {H64}\n");
        assert_eq!(parse_checksums(&wrong, "a.qcow2"), None);
        let sha512 = format!("SHA512 (a.qcow2) = {}\n", h128('c'));
        assert_eq!(
            parse_checksums(&sha512, "a.qcow2"),
            Some(Checksum::Sha512 { value: h128('c') })
        );
    }

    #[test]
    fn fedora_clearsigned_checksum_skips_the_signature() {
        let text = format!(
            "-----BEGIN PGP SIGNED MESSAGE-----\n\
             Hash: SHA256\n\
             \n\
             # Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2: 583335936 bytes\n\
             SHA256 (Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2) = {H64}\n\
             - --dash-escaped line\n\
             -----BEGIN PGP SIGNATURE-----\n\
             \n\
             iQIzBAEBCAAdFiEExufwgc+A4TFGZ26IgptgZjFkVTEFAmj9DpIACgkQgptgZjFk\n\
             =hjFD\n\
             -----END PGP SIGNATURE-----\n"
        );
        assert_eq!(
            parse_checksums(&text, "Fedora-Cloud-Base-Generic-45-1.4.x86_64.qcow2"),
            Some(Checksum::Sha256 { value: H64.into() })
        );
        assert_eq!(parse_checksums(&text, "Hash:"), None);
    }

    #[test]
    fn alpine_sha512_file_is_a_single_hash() {
        let text = format!("{}\n", h128('d'));
        assert_eq!(
            parse_checksums(
                &text,
                "nocloud_alpine-3.20.10-x86_64-bios-cloudinit-r0.qcow2"
            ),
            Some(Checksum::Sha512 { value: h128('d') })
        );
        assert_eq!(parse_checksums("not-a-hash\n", "a"), None);
        assert_eq!(parse_checksums("", "a"), None);
    }

    #[test]
    fn crlf_lines_and_bad_hashes() {
        let text = format!("{H64}  a.img\r\nzz{}  b.img\r\n", &H64[2..]);
        assert_eq!(
            parse_checksums(&text, "a.img"),
            Some(Checksum::Sha256 { value: H64.into() })
        );
        assert_eq!(parse_checksums(&text, "b.img"), None);
    }

    #[test]
    fn file_name_is_the_last_decoded_path_segment() {
        assert_eq!(
            file_name_of("https://x.org/a/b/disk%20one.qcow2?sig=1#frag").as_deref(),
            Some("disk one.qcow2")
        );
        assert_eq!(file_name_of("https://x.org/img").as_deref(), Some("img"));
        assert_eq!(file_name_of("https://x.org/a/"), None);
        assert_eq!(file_name_of("https://x.org"), None);
        assert_eq!(file_name_of("https://x.org/a%2Fb"), None);
        assert_eq!(file_name_of("https://x.org/a%zz"), None);
    }

    #[test]
    fn builtin_images_are_valid_unique_and_cover_both_arches() {
        let images = builtin();
        let mut keys = BTreeSet::new();
        for image in &images {
            assert_eq!(image.validate(), Ok(()), "{}", image.id);
            assert!(image.url.starts_with("https://"), "{}", image.url);
            let Checksum::Sums { url } = &image.checksum else {
                panic!("{} must use a checksum file", image.id);
            };
            assert!(url.starts_with("https://"), "{url}");
            // 체크섬 파일은 이미지와 같은 디렉터리에 있다.
            let dir = |u: &str| u.rsplit_once('/').unwrap().0.to_string();
            assert_eq!(dir(url), dir(&image.url), "{}", image.id);
            assert!(image.min_size_bytes > 0);
            assert!(keys.insert((image.id.clone(), image.arch == Arch::X86_64)));
        }
        let ids: BTreeSet<&str> = BUILTIN.iter().map(|b| b.id).collect();
        for id in &ids {
            for arch in [Arch::X86_64, Arch::Aarch64] {
                assert!(find(&images, id, arch).is_some(), "{id} {arch:?}");
            }
        }
        for id in [
            "ubuntu-24.04",
            "ubuntu-22.04",
            "debian-12",
            "rocky-9",
            "alma-9",
            "alpine-3.20",
        ] {
            assert!(ids.contains(id), "{id}");
        }
        assert!(ids.iter().any(|id| id.starts_with("fedora-")));
        let alpine = find(&images, "alpine-3.20", Arch::Aarch64).unwrap();
        assert_eq!(alpine.firmware, Firmware::Uefi);
    }

    #[test]
    fn default_volume_names() {
        let images = builtin();
        let name = |arch| {
            find(&images, "ubuntu-24.04", arch)
                .unwrap()
                .default_volume_name()
        };
        assert_eq!(name(Arch::X86_64), "ubuntu-24-04");
        assert_eq!(name(Arch::Aarch64), "ubuntu-24-04-arm64");
    }

    #[test]
    fn checksum_is_tagged_by_kind_and_image_round_trips() {
        let json = serde_json::json!({ "kind": "sums", "url": "https://x/SHA256SUMS" });
        let sum: Checksum = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            sum,
            Checksum::Sums {
                url: "https://x/SHA256SUMS".into()
            }
        );
        assert_eq!(serde_json::to_value(&sum).unwrap(), json);

        let image = image();
        let json = serde_json::to_value(&image).unwrap();
        assert_eq!(json["checksum"]["kind"], "sha256");
        assert_eq!(json["arch"], "x86_64");
        assert!(json.get("default_user").is_none());
        assert_eq!(
            serde_json::from_value::<CatalogImage>(json.clone()).unwrap(),
            image
        );
        let mut typo = json;
        typo["checksumm"] = "x".into();
        assert!(serde_json::from_value::<CatalogImage>(typo).is_err());
    }

    fn with_id(id: &str) -> Result<(), CatalogError> {
        CatalogImage {
            id: id.into(),
            ..image()
        }
        .validate()
    }

    fn with_url(url: &str) -> Result<(), CatalogError> {
        CatalogImage {
            url: url.into(),
            ..image()
        }
        .validate()
    }

    #[test]
    fn custom_images_are_checked() {
        assert_eq!(validate_custom(&[image()]), Ok(()));
        let with = |f: fn(&mut CatalogImage)| {
            let mut i = image();
            f(&mut i);
            i.validate()
        };
        assert!(matches!(
            with(|i| i.id = "Ubuntu".into()),
            Err(CatalogError::Id(_))
        ));
        assert!(matches!(
            with(|i| i.id = "-x".into()),
            Err(CatalogError::Id(_))
        ));
        assert!(matches!(
            with(|i| i.id = "x".repeat(58)),
            Err(CatalogError::Id(_))
        ));
        assert_eq!(with(|i| i.id = "x".repeat(57)), Ok(()));
        // 기본 볼륨 이름이 리소스 이름이 되지 않는 id
        for id in [
            "my_os",
            "myos.",
            "myos-",
            "a..b-",
            "01234567-89ab-cdef-0123-456789abcdef",
        ] {
            assert!(
                matches!(with_id(id), Err(CatalogError::Id(_))),
                "{id} was accepted"
            );
        }
        assert_eq!(with_id("myos-1.0"), Ok(()));
        assert!(matches!(
            with(|i| i.title = "t".repeat(129)),
            Err(CatalogError::TooLong(_, "title", 128))
        ));
        assert!(matches!(
            with(|i| i.default_user = Some("u".repeat(33))),
            Err(CatalogError::TooLong(_, "default_user", 32))
        ));
        // 사용자 정보는 이미지·체크섬 파일 URL 어디에도 둘 수 없다. 쿼리는 된다(API가 가린다).
        assert!(matches!(
            with(|i| i.url = "https://user:pw@x/a.img".into()),
            Err(CatalogError::UserInfo(_))
        ));
        assert!(matches!(
            with(|i| i.checksum = Checksum::Sums {
                url: "https://token@x/SHA256SUMS".into()
            }),
            Err(CatalogError::UserInfo(_))
        ));
        assert_eq!(with(|i| i.url = "https://x/a.img?token=1".into()), Ok(()));
        // 제어 문자·공백·호스트 없음
        for url in [
            "https://x/a\nb.img",
            "https://x/a\tb.img",
            "https://x/a b.img",
            "https:///a.img",
            "https://[::1/a.img",
        ] {
            assert!(
                matches!(with_url(url), Err(CatalogError::Url(..))),
                "{url:?} was accepted"
            );
        }
        assert!(matches!(
            with(|i| i.title = " ".into()),
            Err(CatalogError::Empty(_))
        ));
        assert!(matches!(
            with(|i| i.url = "ftp://x/a.img".into()),
            Err(CatalogError::Url(..))
        ));
        assert!(matches!(
            with(|i| i.url = "https://x/dir/".into()),
            Err(CatalogError::Url(..))
        ));
        assert!(matches!(
            with(|i| i.checksum = Checksum::Sha256 {
                value: "abc".into()
            }),
            Err(CatalogError::Digest(_, _, 64))
        ));
        assert!(matches!(
            with(|i| i.checksum = Checksum::Sha512 { value: H64.into() }),
            Err(CatalogError::Digest(_, _, 128))
        ));
        assert!(matches!(
            with(|i| i.checksum = Checksum::Sums {
                url: "file:///etc/passwd".into()
            }),
            Err(CatalogError::SumsUrl(..))
        ));
        assert!(matches!(
            with(|i| i.source_format = SourceFormat::Vmdk),
            Err(CatalogError::SourceFormat(_))
        ));
        assert!(matches!(
            with(|i| {
                i.arch = Arch::Aarch64;
                i.firmware = Firmware::Bios;
            }),
            Err(CatalogError::Aarch64Bios(_))
        ));

        let builtin = CatalogImage {
            id: "ubuntu-24.04".into(),
            ..image()
        };
        assert_eq!(
            validate_custom(&[builtin]),
            Err(CatalogError::Builtin("ubuntu-24.04".into()))
        );
        let arm = CatalogImage {
            arch: Arch::Aarch64,
            ..image()
        };
        assert_eq!(validate_custom(&[image(), arm]), Ok(()));
        assert_eq!(
            validate_custom(&[image(), image()]),
            Err(CatalogError::Duplicate("corp-ubuntu".into(), Arch::X86_64))
        );
        let many: Vec<_> = (0..=MAX_CUSTOM_IMAGES)
            .map(|n| CatalogImage {
                id: format!("img-{n}"),
                ..image()
            })
            .collect();
        assert_eq!(validate_custom(&many), Err(CatalogError::TooMany));
        assert_eq!(validate_custom(&many[1..]), Ok(()));
    }
}
