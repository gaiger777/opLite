//! URL에서 이미지를 받아 볼륨을 채우는 Task(계획 57). 받은 파일은 업로드 디렉터리에 두었다가 올린 이미지처럼
//! 가져오고([`super::import_volume`] — 원본 검사·ISO 검사·변환) 지운다. 실패하면 Task 정리가 받은 파일과 볼륨을 지운다.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opl_driver::MirrorProgress;
use opl_types::storage::SourceFormat;
use opl_types::task::Task;
use opl_types::version::BUILD;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::migrate::write_progress;
use super::{Ctx, import_volume, volume_and_pool};
use crate::volumes::{UPLOAD_RESERVE_BYTES, redact_url};

/// 연결 한도
pub(crate) const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 본문이 이만큼 오지 않으면 실패한다(읽기 쉼 한도). 전체 시간 한도는 없다.
pub(crate) const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(120);
/// 진행률을 쓰는 간격
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
/// 이만큼 받을 때마다 여유 공간을 다시 본다(길이를 알아도 — 받는 동안 다른 것이 디스크를 쓸 수 있다).
const SPACE_CHECK_BYTES: u64 = 64 << 20;

/// 볼륨 `volume`을 채울 받은 파일의 자리(업로드 디렉터리 `dir` 안)
pub(super) fn path(dir: &Path, volume: Uuid) -> PathBuf {
    dir.join(format!("{volume}.download"))
}

pub(super) async fn download(
    ctx: &Ctx,
    task: &Task,
    url: &str,
    sha256: Option<&str>,
    source_format: SourceFormat,
) -> Result<(), String> {
    let volume = task.spec.target;
    volume_and_pool(ctx, volume)?;
    let dir = ctx
        .config
        .upload_dir
        .as_ref()
        .ok_or("this node has no upload directory")?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| e.to_string())?;
    let file = path(dir, volume);
    let (size, sum) = fetch(ctx, task.id, url, dir, &file).await?;
    let done = MirrorProgress {
        done_bytes: size,
        total_bytes: size,
    };
    write_progress(ctx, task.id, "verifying", done).await;
    if let Some(want) = sha256
        && sum != want
    {
        return Err(format!("checksum mismatch: expected {want}, got {sum}"));
    }
    write_progress(ctx, task.id, "importing", done).await;
    import_volume(ctx, volume, &file, source_format).await
}

/// `url`의 본문을 `file`에 받는다. 받은 크기와 SHA-256(소문자 16진)을 돌려준다.
async fn fetch(
    ctx: &Ctx,
    task: Uuid,
    url: &str,
    dir: &Path,
    file: &Path,
) -> Result<(u64, String), String> {
    let shown = redact_url(url);
    let allow_private = ctx.config.download_allow_private;
    let mut builder = reqwest::Client::builder()
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .read_timeout(DOWNLOAD_READ_TIMEOUT)
        .user_agent(format!("orbivisor/{BUILD}"));
    if !allow_private {
        // 이름은 해석한 주소를, IP로 적은 주소는 처음과 리디렉션의 각 단계에서 본다(SSRF).
        builder = builder
            .dns_resolver(Arc::new(PublicResolver {
                proxies: proxy_hosts(),
            }))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    attempt.error("too many redirects")
                } else if let Some(ip) = literal_ip(attempt.url()).filter(|ip| !public_ip(*ip)) {
                    attempt.error(Blocked(ip))
                } else {
                    attempt.follow()
                }
            }));
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    // reqwest 오류 문구의 URL은 사용자 정보를 가리지 않으므로 빼고 가린 URL을 적는다. 막은 주소는 그 까닭을 적는다.
    let failed = |e: reqwest::Error| match blocked(&e) {
        Some(ip) => format!("GET {shown}: {}", Blocked(ip)),
        None => format!("GET {shown}: {}", e.without_url()),
    };
    if !allow_private
        && let Some(ip) = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| literal_ip(&u))
            .filter(|ip| !public_ip(*ip))
    {
        return Err(format!("GET {shown}: {}", Blocked(ip)));
    }
    let mut response = client.get(url).send().await.map_err(failed)?;
    if !response.status().is_success() {
        return Err(format!("GET {shown}: HTTP {}", response.status()));
    }
    let total = response.content_length().unwrap_or(0);
    if total > 0 {
        check_space(dir, total, || {
            format!("not enough free space for a {total}-byte download")
        })?;
    }
    let mut out = tokio::fs::File::create(file)
        .await
        .map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    let mut next_check = SPACE_CHECK_BYTES;
    let mut shown_at = Instant::now();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        hash.update(&chunk);
        done += chunk.len() as u64;
        if let Some(need) = space_to_check(total, done, next_check) {
            check_space(dir, need, || {
                format!("not enough free space to go on with the download after {done} bytes")
            })?;
            next_check = done + SPACE_CHECK_BYTES;
        }
        if shown_at.elapsed() >= PROGRESS_INTERVAL {
            let progress = MirrorProgress {
                done_bytes: done,
                total_bytes: total,
            };
            if !write_progress(ctx, task, "downloading", progress).await {
                return Err("the task is no longer running".into());
            }
            shown_at = Instant::now();
        }
    }
    out.sync_all().await.map_err(|e| e.to_string())?;
    Ok((done, hex::encode(hash.finalize())))
}

/// `[download] allow_private`가 꺼져 있을 때 받지 않는 주소(루프백·링크 로컬·사설망·메타데이터 등)
#[derive(Debug)]
struct Blocked(IpAddr);

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "address {} is loopback, link-local or private; set [download] allow_private = true to download from it",
            self.0
        )
    }
}

impl std::error::Error for Blocked {}

/// reqwest 오류의 원인 사슬에 [`Blocked`]가 있으면 그 주소
fn blocked(e: &reqwest::Error) -> Option<IpAddr> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = source {
        if let Some(b) = err.downcast_ref::<Blocked>() {
            return Some(b.0);
        }
        source = err.source();
    }
    None
}

/// 밖의(공인) 주소인지: 루프백, 지정 안 함(0.0.0.0/8·::), 사설망(10/8·172.16/12·192.168/16·fc00::/7), 링크 로컬
/// (169.254/16 — 클라우드 메타데이터 169.254.169.254 포함 — ·fe80::/10), 공유 주소(100.64/10), 시험망(198.18/15),
/// 예약(240/4 — 브로드캐스트 포함), 멀티캐스트(224/4·ff00::/8)가 아니면 참. IPv4를 담은 IPv6(사상 `::ffff:a.b.c.d`,
/// 호환 `::a.b.c.d`, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`)는 그 IPv4로 본다.
fn public_ip(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => embedded_ipv4(v6).map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || a == 0
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && (b & 0xfe) == 18))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// IPv6 주소가 담은 IPv4(사상·호환·NAT64·6to4). `::`·`::1`은 IPv6 그대로 본다.
fn embedded_ipv4(v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let s = v6.segments();
    let low = |hi: u16, lo: u16| std::net::Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    if let Some(v4) = v6.to_ipv4_mapped() {
        return Some(v4);
    }
    match s {
        // IPv4 호환(::a.b.c.d)
        [0, 0, 0, 0, 0, 0, hi, lo] if !(hi == 0 && lo <= 1) => Some(low(hi, lo)),
        // NAT64 잘 알려진 접두(64:ff9b::/96)
        [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] => Some(low(hi, lo)),
        // 6to4(2002:AABB:CCDD::/48)
        [0x2002, hi, lo, ..] => Some(low(hi, lo)),
        _ => None,
    }
}

/// URL의 호스트가 IP로 적혀 있으면 그 주소(이름이면 None — 해석은 [`PublicResolver`]가 본다)
fn literal_ip(url: &reqwest::Url) -> Option<IpAddr> {
    let host = url.host_str()?;
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

/// 환경 변수로 준 프록시의 호스트 이름(그 이름은 사설 주소여도 해석한다 — 프록시가 대상에 닿는다)
fn proxy_hosts() -> Vec<String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok())
    .filter_map(|v| {
        let v = if v.contains("://") {
            v
        } else {
            format!("http://{v}")
        };
        reqwest::Url::parse(&v)
            .ok()?
            .host_str()
            .map(str::to_ascii_lowercase)
    })
    .collect()
}

/// 해석한 주소 가운데 밖의 주소만 돌려주는 이름 해석(모두 막히면 [`Blocked`] 오류)
struct PublicResolver {
    proxies: Vec<String>,
}

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let proxy = self.proxies.contains(&host);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> = addrs
                .iter()
                .copied()
                .filter(|a| proxy || public_ip(a.ip()))
                .collect();
            match (public.is_empty(), addrs.first()) {
                (true, Some(a)) => Err(Box::new(Blocked(a.ip())) as _),
                _ => Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs),
            }
        })
    }
}

/// `done` 바이트를 받아 다시 볼 때(`next_check`)가 됐으면 더 남아야 할 크기: 남은 길이(모르면 0)와
/// [`SPACE_CHECK_BYTES`] 중 큰 것
fn space_to_check(total: u64, done: u64, next_check: u64) -> Option<u64> {
    (done >= next_check).then(|| total.saturating_sub(done).max(SPACE_CHECK_BYTES))
}

/// 업로드 디렉터리에 `size` 바이트를 더 쓰고도 [`UPLOAD_RESERVE_BYTES`]가 남는지 본다.
fn check_space(dir: &Path, size: u64, message: impl FnOnce() -> String) -> Result<(), String> {
    let available = fs4::available_space(dir).map_err(|e| e.to_string())?;
    if size.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(message());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 루프백·링크 로컬(메타데이터)·사설망·공유 주소는 밖의 주소가 아니다.
    #[test]
    fn private_and_local_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "::1",
            "::",
            "fe80::1",
            "fd00:ec2::254",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "0.1.2.3",
            "198.18.0.1",
            "198.19.255.255",
            "240.0.0.1",
            "255.255.255.255",
            "224.0.0.1",
            "239.1.2.3",
            "ff02::1",
            "::127.0.0.1",
            "::10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::7f00:1",
            "2002:a9fe:a9fe::1",
            "2002:c0a8:101::",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "2001:db8::1",
            "2606:4700::1111",
            "198.20.0.1",
            "223.255.255.1",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        assert_eq!(
            literal_ip(&url("http://127.0.0.1:8080/a.iso")),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(
            literal_ip(&url("http://[fe80::1]/a")),
            Some("fe80::1".parse().unwrap())
        );
        assert_eq!(literal_ip(&url("https://cloud-images.ubuntu.com/a")), None);
    }

    /// 이름을 해석한 주소가 모두 사설이면 오류(그 주소), 프록시 이름은 사설이어도 해석한다.
    #[tokio::test]
    async fn the_resolver_drops_private_addresses() {
        use reqwest::dns::Resolve;
        let name = |s: &str| s.parse::<reqwest::dns::Name>().unwrap();
        let guard = PublicResolver { proxies: vec![] };
        let err = match guard.resolve(name("localhost")).await {
            Err(e) => e,
            Ok(_) => panic!("localhost resolved"),
        };
        assert!(err.downcast_ref::<Blocked>().is_some(), "{err}");
        let proxy = PublicResolver {
            proxies: vec!["localhost".into()],
        };
        assert!(
            proxy
                .resolve(name("localhost"))
                .await
                .unwrap()
                .next()
                .is_some()
        );
    }

    /// 길이를 알든 모르든 64 MiB마다 여유 공간을 다시 본다.
    #[test]
    fn space_is_checked_every_64_mib() {
        const M: u64 = 1 << 20;
        assert_eq!(space_to_check(0, 10 * M, SPACE_CHECK_BYTES), None);
        assert_eq!(
            space_to_check(0, 64 * M, SPACE_CHECK_BYTES),
            Some(SPACE_CHECK_BYTES)
        );
        // 길이를 알면 남은 길이(64 MiB보다 작으면 64 MiB)
        assert_eq!(
            space_to_check(1024 * M, 64 * M, SPACE_CHECK_BYTES),
            Some(960 * M)
        );
        assert_eq!(
            space_to_check(100 * M, 64 * M, SPACE_CHECK_BYTES),
            Some(SPACE_CHECK_BYTES)
        );
        assert_eq!(space_to_check(1024 * M, 10 * M, SPACE_CHECK_BYTES), None);
    }
}
