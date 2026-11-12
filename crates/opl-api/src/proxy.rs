//! 앞단 프록시와 따로 띄운 웹 포털(설계 §4): 믿는 프록시(`AppState::trusted_proxies`)가 보낸
//! `X-Forwarded-*` 헤더와 허용 출처(클러스터 설정 `web_origins`)의 CORS.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_HEADERS,
    ACCESS_CONTROL_REQUEST_METHOD, HOST, ORIGIN, UPGRADE, VARY,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use opl_types::security_group::Cidr;

use crate::auth::same_origin;
use crate::limit::ClientIp;
use crate::state::AppState;

const X_FORWARDED_FOR: &str = "x-forwarded-for";
const X_FORWARDED_HOST: &str = "x-forwarded-host";
const X_FORWARDED_PROTO: &str = "x-forwarded-proto";
const SEC_FETCH_SITE: &str = "sec-fetch-site";

/// 허용 출처의 preflight가 받는 메서드
const CORS_METHODS: &str = "GET, POST, PUT, PATCH, DELETE";
/// 브라우저가 preflight 답을 다시 쓰는 시간(초)
const CORS_MAX_AGE: &str = "600";

/// 믿는 프록시가 알린 요청의 scheme(`http`·`https`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForwardedProto(pub &'static str);

/// 요청의 `Origin`이 허용 출처다. 쿠키 요청을 받는다.
#[derive(Debug, Clone, Copy)]
struct AllowedOrigin;

/// 요청이 허용 출처(같은 출처가 아닌)에서 왔다. 세션 쿠키를 `SameSite=None`으로 준다.
pub struct FromWebOrigin(pub bool);

impl<S: Send + Sync> FromRequestParts<S> for FromWebOrigin {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(from_web_origin(parts)))
    }
}

/// 요청이 허용 출처에서 왔다([`layer`]가 표시한다).
pub(crate) fn from_web_origin(parts: &Parts) -> bool {
    parts.extensions.get::<AllowedOrigin>().is_some()
}

/// 믿는 프록시가 알린 scheme. 없으면 None
pub(crate) fn forwarded_proto(parts: &Parts) -> Option<&'static str> {
    parts.extensions.get::<ForwardedProto>().map(|p| p.0)
}

/// `Origin`이 없는 쓰기 요청(GET·HEAD가 아님)이나 WebSocket 업그레이드이고, `Sec-Fetch-Site`가 있는데
/// 같은 출처·같은 사이트·사용자가 직접 연 것(`none`)이 아니다. 다른 사이트의 페이지가 보냈을 수 있다.
/// `Sec-Fetch-Site`를 보내는 브라우저는 쓰기에 `Origin`도 보내므로, 둘 다 없으면 브라우저 밖의
/// 클라이언트(orbivisorctl, curl)로 보고 받는다.
pub(crate) fn unsafe_without_origin(parts: &Parts) -> bool {
    let headers = &parts.headers;
    if headers.contains_key(ORIGIN) {
        return false;
    }
    let upgrade = headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if matches!(parts.method, Method::GET | Method::HEAD) && !upgrade {
        return false;
    }
    headers
        .get(SEC_FETCH_SITE)
        .is_some_and(|v| !matches!(v.to_str(), Ok("same-origin" | "same-site" | "none")))
}

fn trusted(proxies: &[Cidr], ip: IpAddr) -> bool {
    proxies.iter().any(|c| c.contains(ip))
}

/// `X-Forwarded-For` 항목 하나(`a.b.c.d`, `a.b.c.d:port`, `v6`, `[v6]`, `[v6]:port`)의 주소
fn forwarded_ip(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse::<IpAddr>()
        .ok()
        .or_else(|| entry.parse::<SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| {
            entry
                .strip_prefix('[')?
                .strip_suffix(']')?
                .parse::<IpAddr>()
                .ok()
        })
}

/// 요청한 클라이언트의 주소. 연결 주소 `peer`가 믿는 프록시면 `X-Forwarded-For`(왼쪽부터 `forwarded_for`)를
/// 오른쪽부터 보며 믿는 프록시가 적은 주소만 믿는다: 오른쪽에서 믿지 않는 첫 주소다. 모두 믿는 프록시면 맨
/// 왼쪽 주소, 읽지 못하는 항목을 만나면 그 바로 오른쪽 주소다.
pub(crate) fn client_ip(proxies: &[Cidr], peer: IpAddr, forwarded_for: &[&str]) -> IpAddr {
    let mut client = peer;
    for entry in forwarded_for.iter().rev() {
        if !trusted(proxies, client) {
            break;
        }
        match forwarded_ip(entry) {
            Some(ip) => client = ip,
            None => break,
        }
    }
    client
}

/// 헤더 `name`의 맨 오른쪽 값(쉼표로 이은 것 포함). 바로 앞 프록시가 적은 값이다.
fn last_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let value = headers.get_all(name).iter().next_back()?.to_str().ok()?;
    let last = value.rsplit(',').next()?.trim();
    (!last.is_empty()).then_some(last)
}

/// 호스트·포트 글자(`host`, `host:port`, `[v6]:port`)만 있다.
pub(crate) fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

/// 믿는 프록시 `peer`가 보낸 요청: 클라이언트 주소(`X-Forwarded-For`)를 표시하고, `X-Forwarded-Host`로
/// `Host`를 바꾸고, `X-Forwarded-Proto`를 표시한다.
fn apply_forwarded(proxies: &[Cidr], peer: IpAddr, req: &mut Request) {
    let headers = req.headers();
    let forwarded_for: Vec<&str> = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        // 글자가 아닌 값은 읽지 못하는 항목으로 둔다(그 왼쪽은 믿지 않는다).
        .flat_map(|v| v.to_str().unwrap_or("?").split(','))
        .collect();
    let client = client_ip(proxies, peer, &forwarded_for);
    let host = last_value(headers, X_FORWARDED_HOST)
        .filter(|h| valid_host(h))
        .and_then(|h| HeaderValue::from_str(h).ok());
    let proto = match last_value(headers, X_FORWARDED_PROTO).map(str::to_ascii_lowercase) {
        Some(p) if p == "https" => Some(ForwardedProto("https")),
        Some(p) if p == "http" => Some(ForwardedProto("http")),
        _ => None,
    };
    if let Some(host) = host {
        req.headers_mut().insert(HOST, host);
    }
    let extensions = req.extensions_mut();
    extensions.insert(ClientIp(client));
    if let Some(proto) = proto {
        extensions.insert(proto);
    }
}

/// 요청의 `Origin`이 같은 출처가 아니고 허용 출처면 그 값
fn allowed_origin(state: &AppState, req: &Request) -> Option<HeaderValue> {
    let origin = req.headers().get(ORIGIN)?;
    let proto = req.extensions().get::<ForwardedProto>().map(|p| p.0);
    if same_origin(req.headers(), proto) {
        return None;
    }
    let normalized = crate::settings::normalize_origin(origin.to_str().ok()?)?;
    let settings = match crate::settings::current(&state.cluster) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the allowed web origins");
            return None;
        }
    };
    settings
        .spec
        .web_origins
        .contains(&normalized)
        .then(|| origin.clone())
}

fn cors_headers(headers: &mut HeaderMap, origin: HeaderValue) {
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    headers.append(VARY, HeaderValue::from_static("origin"));
}

/// 모든 API 요청이 먼저 거친다. 믿는 프록시의 헤더를 반영하고, 허용 출처의 요청이면 CORS 응답 헤더를 붙인다
/// (preflight는 여기서 답한다).
pub(crate) async fn layer(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    if !state.trusted_proxies.is_empty()
        && let Some(peer) = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
        && trusted(&state.trusted_proxies, peer)
    {
        apply_forwarded(&state.trusted_proxies, peer, &mut req);
    }
    let Some(origin) = allowed_origin(&state, &req) else {
        return next.run(req).await;
    };
    if req.method() == Method::OPTIONS && req.headers().contains_key(ACCESS_CONTROL_REQUEST_METHOD)
    {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let headers = response.headers_mut();
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(CORS_METHODS),
        );
        if let Some(asked) = req.headers().get(ACCESS_CONTROL_REQUEST_HEADERS) {
            headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, asked.clone());
        }
        headers.insert(
            ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static(CORS_MAX_AGE),
        );
        cors_headers(headers, origin);
        return response;
    }
    req.extensions_mut().insert(AllowedOrigin);
    let mut response = next.run(req).await;
    cors_headers(response.headers_mut(), origin);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidrs(list: &[&str]) -> Vec<Cidr> {
        list.iter().map(|c| c.parse().unwrap()).collect()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn client_is_the_rightmost_untrusted_forwarded_address() {
        let proxies = cidrs(&["10.0.0.0/8", "fd00::/8"]);
        let peer = ip("10.0.0.2");
        // 프록시 하나
        assert_eq!(
            client_ip(&proxies, peer, &["203.0.113.7"]),
            ip("203.0.113.7")
        );
        // 클라이언트가 지어낸 왼쪽 항목은 믿지 않는다.
        assert_eq!(
            client_ip(&proxies, peer, &["1.1.1.1", "203.0.113.7"]),
            ip("203.0.113.7")
        );
        // 믿는 프록시 둘을 거쳤다.
        assert_eq!(
            client_ip(&proxies, peer, &["1.1.1.1", "203.0.113.7", "10.0.0.9"]),
            ip("203.0.113.7")
        );
        // 모두 믿는 프록시면 맨 왼쪽
        assert_eq!(
            client_ip(&proxies, peer, &["10.0.0.7", "10.0.0.8"]),
            ip("10.0.0.7")
        );
        // 헤더가 없으면 연결 주소
        assert_eq!(client_ip(&proxies, peer, &[]), peer);
        // 읽지 못하는 항목에서 멈춘다.
        assert_eq!(
            client_ip(&proxies, peer, &["203.0.113.7", "garbage", "10.0.0.9"]),
            ip("10.0.0.9")
        );
        // 포트와 IPv6 모양
        assert_eq!(
            client_ip(&proxies, peer, &[" 203.0.113.7:5555 "]),
            ip("203.0.113.7")
        );
        assert_eq!(
            client_ip(&proxies, peer, &["[2001:db8::1]:443"]),
            ip("2001:db8::1")
        );
        assert_eq!(
            client_ip(&proxies, peer, &["[2001:db8::1]", "fd00::5"]),
            ip("2001:db8::1")
        );
        // 믿지 않는 연결 주소는 헤더를 보지 않는다.
        assert_eq!(
            client_ip(&proxies, ip("192.0.2.1"), &["203.0.113.7"]),
            ip("192.0.2.1")
        );
    }

    #[test]
    fn writes_without_origin_need_a_same_site_fetch() {
        fn parts(method: Method, headers: &[(&'static str, &str)]) -> Parts {
            let mut req = Request::builder().method(method).uri("/api/v1/vms");
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            req.body(axum::body::Body::empty()).unwrap().into_parts().0
        }
        assert!(!unsafe_without_origin(&parts(Method::GET, &[])));
        assert!(!unsafe_without_origin(&parts(Method::HEAD, &[])));
        assert!(!unsafe_without_origin(&parts(Method::POST, &[])));
        assert!(unsafe_without_origin(&parts(
            Method::DELETE,
            &[(SEC_FETCH_SITE, "cross-site")]
        )));
        for site in ["same-origin", "same-site", "none"] {
            assert!(!unsafe_without_origin(&parts(
                Method::POST,
                &[(SEC_FETCH_SITE, site)]
            )));
        }
        assert!(!unsafe_without_origin(&parts(
            Method::POST,
            &[("origin", "https://a.example")]
        )));
        assert!(!unsafe_without_origin(&parts(
            Method::GET,
            &[("upgrade", "WebSocket")]
        )));
        assert!(unsafe_without_origin(&parts(
            Method::GET,
            &[("upgrade", "WebSocket"), (SEC_FETCH_SITE, "cross-site")]
        )));
    }

    #[test]
    fn last_forwarded_value_is_used() {
        let mut h = HeaderMap::new();
        assert_eq!(last_value(&h, X_FORWARDED_HOST), None);
        h.append(X_FORWARDED_HOST, "a.example, b.example".parse().unwrap());
        assert_eq!(last_value(&h, X_FORWARDED_HOST), Some("b.example"));
        h.append(X_FORWARDED_HOST, "c.example".parse().unwrap());
        assert_eq!(last_value(&h, X_FORWARDED_HOST), Some("c.example"));
        assert!(valid_host("[fd00::1]:8443"));
        assert!(!valid_host("a.example/x"));
        assert!(!valid_host(""));
    }
}
