//! 로그인과 현재 비밀번호 확인의 시도 제한(설계 2절). 노드 메모리에만 두고 노드마다 따로 센다.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Mutex, Once};
use std::time::Duration;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use tokio::time::Instant;

use crate::error::ApiError;

/// (사용자 이름, IP) 키가 막히기까지의 연속 실패 수
pub const USER_FAILURES: u32 = 5;
/// IP 키가 막히기까지의 연속 실패 수
pub const IP_FAILURES: u32 = 20;
/// 처음 막는 시간. 이어진 실패마다 두 배가 된다.
pub const FIRST_LOCK: Duration = Duration::from_secs(30);
pub const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// 마지막 시도 뒤 이만큼이 넘게 지난 기록은 지운다. 최대 잠금과 같아서, 막 풀린 뒤의 실패는 이어서 센다.
pub const IDLE: Duration = Duration::from_secs(15 * 60);
pub const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    User(String, IpAddr),
    Ip(IpAddr),
}

impl Key {
    fn limit(&self) -> u32 {
        match self {
            Key::User(..) => USER_FAILURES,
            Key::Ip(_) => IP_FAILURES,
        }
    }
}

struct Entry {
    failures: u32,
    locked_until: Option<Instant>,
    last: Instant,
}

/// 실패한 시도를 세어 막는다. 시계는 `tokio::time::Instant`라 테스트가 시간을 멈추고 당길 수 있다.
#[derive(Default)]
pub struct AttemptLimiter {
    entries: Mutex<HashMap<Key, Entry>>,
}

fn keys(user: &str, ip: IpAddr) -> [Key; 2] {
    let ip = counted_ip(ip);
    [Key::User(user.to_lowercase(), ip), Key::Ip(ip)]
}

/// 세는 단위의 IP. IPv4-mapped IPv6는 IPv4로 보고, IPv6는 /64 네트워크 하나로 센다(한 사용자가
/// 쉽게 바꿔 쓸 수 있는 주소 묶음이다).
fn counted_ip(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
        v4 => v4,
    }
}

/// 막힌 키가 있으면 풀릴 때까지 남은 시간(가장 긴 것)
fn wait(map: &HashMap<Key, Entry>, keys: &[Key], now: Instant) -> Option<Duration> {
    keys.iter()
        .filter_map(|k| map.get(k)?.locked_until)
        .filter(|until| *until > now)
        .map(|until| until - now)
        .max()
}

impl AttemptLimiter {
    /// 막혀 있으면 풀릴 때까지 남은 시간
    pub fn blocked(&self, user: &str, ip: IpAddr) -> Option<Duration> {
        wait(
            &self.entries.lock().unwrap(),
            &keys(user, ip),
            Instant::now(),
        )
    }

    /// 시도를 시작한다. 막혀 있으면 기록하지 않고 429 `too_many_attempts`. 아니면 이 시도를 미리
    /// 실패로 센다(한도에 닿으면 잠근다). 확인을 기다리는 동안 들어온 다른 시도가 이 실패를 보고
    /// 막히도록 확인과 세기를 한 번에 한다. 실패하면 따로 기록하지 않고, 성공하면 `success`를 부른다.
    pub fn begin(&self, user: &str, ip: IpAddr) -> Result<(), ApiError> {
        self.begin_keys(&keys(user, ip))
    }

    /// 사용자 이름 없이 IP 키만 세는 `begin`. 있을 수 없는 이름의 로그인에 쓴다(기록에 그 이름을 넣지 않는다).
    pub fn begin_ip(&self, ip: IpAddr) -> Result<(), ApiError> {
        self.begin_keys(&[Key::Ip(counted_ip(ip))])
    }

    fn begin_keys(&self, keys: &[Key]) -> Result<(), ApiError> {
        let now = Instant::now();
        let mut map = self.entries.lock().unwrap();
        if let Some(wait) = wait(&map, keys, now) {
            return Err(too_many_attempts(wait));
        }
        map.retain(|_, e| now.duration_since(e.last) <= IDLE);
        for key in keys {
            let limit = key.limit();
            let e = map.entry(key.clone()).or_insert(Entry {
                failures: 0,
                locked_until: None,
                last: now,
            });
            e.failures += 1;
            e.last = now;
            if e.failures >= limit {
                // 한도를 넘은 실패마다 두 배. 2^6 * 30초가 이미 15분을 넘는다.
                let lock = FIRST_LOCK * 2u32.pow((e.failures - limit).min(6));
                e.locked_until = Some(now + lock.min(MAX_LOCK));
            }
        }
        // 넘치면 잠기지 않은 것 가운데 마지막 시도가 가장 오래된 것부터 버린다. 잠긴 것은 그 다음이다
        // (기록을 채워 잠금을 풀지 못하게).
        while map.len() > MAX_ENTRIES {
            let oldest = map
                .iter()
                .min_by_key(|(_, e)| (e.locked_until.is_some_and(|until| until > now), e.last))
                .map(|(k, _)| k.clone())
                .expect("map is not empty");
            map.remove(&oldest);
        }
        Ok(())
    }

    /// 성공한 시도. 그 (사용자 이름, IP) 기록은 지우고, IP 기록에서는 `begin`이 미리 센 이 시도 하나만
    /// 뺀다. IP 기록을 지우지 않는 것은 자기 계정 로그인으로 다른 이름의 실패를 지우지 못하게 하려는 것이다.
    pub fn success(&self, user: &str, ip: IpAddr) {
        let [user_key, ip_key] = keys(user, ip);
        let mut map = self.entries.lock().unwrap();
        map.remove(&user_key);
        if let Some(e) = map.get_mut(&ip_key) {
            e.failures = e.failures.saturating_sub(1);
            // 이 시도로 한도에 닿아 걸린 잠금은 푼다. 한도 아래에서는 잠금이 생기지 않는다.
            if e.failures < IP_FAILURES {
                e.locked_until = None;
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

fn too_many_attempts(wait: Duration) -> ApiError {
    // 남은 시간을 올림한 초. 0초로 알려 주지 않는다.
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut e = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_attempts",
        format!("too many failed attempts; try again in {secs} seconds"),
    );
    e.retry_after = Some(secs);
    e
}

/// 요청한 클라이언트의 IP(연결 주소). 연결 주소가 없으면(라우터를 바로 부를 때) `0.0.0.0` 하나로 센다.
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip())
                .unwrap_or_else(|| {
                    // 테스트 라우터를 바로 부를 때는 괜찮다. opld에서 보이면 리스너 설정이 빠진 것이다.
                    static WARNED: Once = Once::new();
                    WARNED.call_once(|| {
                        tracing::warn!("no client address; counting login attempts as 0.0.0.0")
                    });
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

    /// 실패한 시도 n번(막혀 있으면 안 된다)
    fn fail(l: &AttemptLimiter, user: &str, ip: IpAddr, n: u32) {
        for _ in 0..n {
            l.begin(user, ip).unwrap();
        }
    }

    /// i마다 다른 IPv4 주소
    fn nth_ip(i: usize) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0b00_0000 + i as u32))
    }

    #[tokio::test(start_paused = true)]
    async fn locks_after_five_failures_for_thirty_seconds() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 4);
        assert_eq!(l.blocked("alice", IP), None);
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("alice", IP), Some(FIRST_LOCK));
        // 사용자 이름은 대소문자를 가리지 않고, 다른 사용자·다른 IP는 막히지 않는다.
        assert_eq!(l.blocked("ALICE", IP), Some(FIRST_LOCK));
        assert_eq!(l.blocked("bob", IP), None);
        assert_eq!(l.blocked("alice", OTHER_IP), None);

        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(l.blocked("alice", IP), Some(Duration::from_secs(1)));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(l.blocked("alice", IP), None);
    }

    #[tokio::test(start_paused = true)]
    async fn attempts_count_as_failures_when_they_begin() {
        // 확인이 끝나기 전의 시도도 센다. 동시에 들어온 시도는 다섯 번째까지만 통과한다.
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 5);
        let e = l.begin("alice", IP).unwrap_err();
        assert_eq!(e.code, "too_many_attempts");
    }

    #[tokio::test(start_paused = true)]
    async fn continued_failures_double_the_lock_up_to_fifteen_minutes() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 5);
        let mut expected = FIRST_LOCK;
        for _ in 0..8 {
            assert_eq!(l.blocked("alice", IP), Some(expected));
            tokio::time::advance(expected).await;
            assert_eq!(l.blocked("alice", IP), None);
            fail(&l, "alice", IP, 1);
            expected = (expected * 2).min(MAX_LOCK);
        }
        assert_eq!(l.blocked("alice", IP), Some(MAX_LOCK));
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_attempts_are_not_recorded() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 5);
        tokio::time::advance(Duration::from_secs(10)).await;
        // 막힌 동안 두드려도 잠금이 늘지 않는다.
        for _ in 0..10 {
            let e = l.begin("alice", IP).unwrap_err();
            assert_eq!(e.retry_after, Some(20));
        }
        tokio::time::advance(Duration::from_secs(20)).await;
        // 풀린 뒤의 실패는 여섯 번째다.
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("alice", IP), Some(FIRST_LOCK * 2));
    }

    #[tokio::test(start_paused = true)]
    async fn success_resets_the_user_record() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 4);
        fail(&l, "alice", IP, 1);
        l.success("alice", IP);
        // 처음부터 다시 센다.
        fail(&l, "alice", IP, 4);
        assert_eq!(l.blocked("alice", IP), None);
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("alice", IP), Some(FIRST_LOCK));
    }

    #[tokio::test(start_paused = true)]
    async fn success_takes_back_only_its_own_attempt_from_the_ip_record() {
        let l = AttemptLimiter::default();
        for i in 0..IP_FAILURES - 1 {
            fail(&l, &format!("user{i}"), IP, 1);
        }
        // 스무 번째 시도는 성공한다. 미리 센 실패와 그로 걸린 잠금을 되돌린다.
        fail(&l, "alice", IP, 1);
        l.success("alice", IP);
        assert_eq!(l.blocked("new-user", IP), None);
        // 앞의 실패 19번은 남아 있어 한 번 더 실패하면 막힌다.
        fail(&l, "other", IP, 1);
        assert_eq!(l.blocked("new-user", IP), Some(FIRST_LOCK));
    }

    #[tokio::test(start_paused = true)]
    async fn ip_is_locked_after_twenty_failures_across_users() {
        let l = AttemptLimiter::default();
        for i in 0..IP_FAILURES {
            assert_eq!(l.blocked("new-user", IP), None, "{i}");
            fail(&l, &format!("user{i}"), IP, 1);
        }
        assert_eq!(l.blocked("new-user", IP), Some(FIRST_LOCK));
        assert_eq!(l.blocked("new-user", OTHER_IP), None);
    }

    #[tokio::test(start_paused = true)]
    async fn begin_ip_counts_only_the_ip() {
        let l = AttemptLimiter::default();
        for _ in 0..IP_FAILURES - 1 {
            l.begin_ip(IP).unwrap();
        }
        assert_eq!(l.len(), 1);
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("bob", IP), Some(FIRST_LOCK));
        assert!(l.begin_ip(IP).is_err());
    }

    #[test]
    fn ipv6_is_counted_per_slash_64_and_mapped_ipv4_as_ipv4() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::2".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(counted_ip(a), "2001:db8:1:2::".parse::<IpAddr>().unwrap());
        assert_eq!(counted_ip(a), counted_ip(b));
        assert_ne!(counted_ip(a), counted_ip(c));
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert_eq!(counted_ip(mapped), IP);
        assert_eq!(counted_ip(IP), IP);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_records_are_dropped() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 4);
        tokio::time::advance(IDLE + Duration::from_millis(1)).await;
        // 기록할 때 오래된 기록을 지우므로 alice의 실패는 1번부터 다시 센다.
        fail(&l, "bob", OTHER_IP, 1);
        assert_eq!(l.len(), 2);
        fail(&l, "alice", IP, 4);
        assert_eq!(l.blocked("alice", IP), None);
    }

    #[tokio::test(start_paused = true)]
    async fn record_count_is_capped_by_dropping_the_oldest_unlocked() {
        let l = AttemptLimiter::default();
        // (locked, IP)는 잠기고 IP 키는 실패 5번으로 잠기지 않았다. 둘 다 가장 오래된 기록이다.
        fail(&l, "locked", IP, USER_FAILURES);
        tokio::time::advance(Duration::from_millis(1)).await;
        // 사용자마다 IP가 달라 한 시도가 기록 두 개를 만든다.
        for i in 0..MAX_ENTRIES / 2 - 1 {
            fail(&l, &format!("user{i}"), nth_ip(i), 1);
        }
        assert_eq!(l.len(), MAX_ENTRIES);
        tokio::time::advance(Duration::from_millis(1)).await;
        fail(&l, "last", OTHER_IP, 1);
        assert_eq!(l.len(), MAX_ENTRIES);
        let map = l.entries.lock().unwrap();
        // 잠긴 기록은 남고, 잠기지 않은 것 가운데 가장 오래된 IP 키가 빠졌다.
        assert!(map.contains_key(&Key::User("locked".into(), IP)));
        assert!(!map.contains_key(&Key::Ip(IP)));
        assert!(map.contains_key(&Key::Ip(OTHER_IP)));
    }

    #[test]
    fn error_carries_retry_after_in_whole_seconds() {
        let e = too_many_attempts(Duration::from_millis(29_500));
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.code, "too_many_attempts");
        assert_eq!(e.retry_after, Some(30));
        assert!(
            e.message.contains("try again in 30 seconds"),
            "{}",
            e.message
        );
    }
}
