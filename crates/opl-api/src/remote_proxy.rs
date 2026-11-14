//! 원격 클러스터 API 중계 `/api/v1/remotes/{key}/api/{*path}`. 그 원격의 `/api/v1/{path}`로 메서드·
//! 쿼리·본문과 몇 헤더만 저장한 토큰으로 보내고, 상태·본문과 몇 헤더를 흘려 돌려준다. 허용 목록의 자원을
//! GET·HEAD로 읽는 것은 원격을 읽을 수 있는 누구나, 그 밖은 admin만(원격 쪽 권한은 토큰 주인의 역할이
//! 다시 정한다). watch와 콘솔(WebSocket)은 중계하지 않는다.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{
    ACCEPT, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG,
    HeaderName, HeaderValue, IF_MATCH, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use opl_types::remote::RemoteSpec;
use opl_types::resource::ResourceSpec;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::error::ApiError;
use crate::remote_client::RemoteClient;
use crate::remotes::remote_unreachable;
use crate::resources::lookup;
use crate::state::AppState;
use crate::vm_imports::esxi::percent_decode;

/// 원격으로 보내는 요청 헤더(`Authorization`·`Host`는 클라이언트가 채운다 — 쿠키·`Origin` 등은 보내지
/// 않는다)
const REQUEST_HEADERS: [HeaderName; 4] = [CONTENT_TYPE, CONTENT_LENGTH, IF_MATCH, ACCEPT];
/// 돌려주는 응답 헤더(`Content-Type`은 JSON일 때만 그대로)
const RESPONSE_HEADERS: [HeaderName; 4] = [CONTENT_LENGTH, ETAG, RETRY_AFTER, CONTENT_DISPOSITION];
/// admin이 아니어도 GET·HEAD로 읽을 수 있는 첫 경로 조각(그 밖에는 `auth/whoami`만). 사용자·토큰·영역·
/// 감사·텔레메트리와 원격의 원격(중계 사슬)은 원격 토큰이 읽을 수 있어도 이 클러스터의 admin만 읽는다.
const READABLE: [&str; 19] = [
    "vms",
    "volumes",
    "storage-pools",
    "networks",
    "security-groups",
    "snapshots",
    "backups",
    "backup-jobs",
    "vm-imports",
    "ha-groups",
    "ha-checks",
    "placement-rules",
    "placement",
    "nodes",
    "tasks",
    "cluster",
    "version",
    "settings",
    "subscription",
];

/// 원격 API 중계. 경로에 `.`·`..`·빈 조각이 있으면(퍼센트 인코딩 포함) 400, watch·콘솔은 404
/// `remote_path_unsupported`, 원격에 닿지 못하면 502 `remote_unreachable`, 원격이 저장한 토큰을
/// 거절하면(401) 502 `remote_unauthorized`.
pub async fn proxy(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    note: AuditNote,
    Path((key, _)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let (parts, body) = request.into_parts();
    let (method, uri) = (parts.method, parts.uri);
    let path = remote_path(uri.path());
    let segments = segments(path);
    let action = match method {
        Method::GET | Method::HEAD if readable(&segments) => Action::Read,
        _ => Action::Write,
    };
    state.authorize(&principal, action, RemoteSpec::KIND)?;
    check_path(&segments)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
    note.detail(format!("remote={} {method} /{path}", remote.name));
    let path_and_query = match uri.query() {
        Some(query) => format!("/{path}?{query}"),
        None => format!("/{path}"),
    };
    // 등록할 때 확인해 저장한 CA만 믿는다(연결할 때 CA를 다시 받지 않는다).
    let client = RemoteClient::new(&remote.spec.url, &remote.spec.ca_pem, &remote.spec.token)
        .map_err(remote_unreachable)?;
    let response = client
        .request(
            method,
            &path_and_query,
            copy(&parts.headers, &REQUEST_HEADERS),
            body,
        )
        .await
        .map_err(remote_unreachable)?;
    let (parts, body) = response.into_parts();
    // 원격의 401은 이 클러스터의 로그인 문제가 아니다(본문은 버린다).
    if parts.status == StatusCode::UNAUTHORIZED {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "remote_unauthorized",
            "the remote cluster refused the stored token; update it with `orbivisorctl remote set --token-file`",
        ));
    }
    let mut out = Response::new(Body::new(body));
    *out.status_mut() = parts.status;
    *out.headers_mut() = response_headers(&parts.headers);
    Ok(out)
}

/// 돌려줄 응답 헤더. 원격의 본문을 브라우저가 이 클러스터의 페이지로 실행하지 못하게 JSON이 아니면
/// `application/octet-stream`으로 하고, 늘 `nosniff`와 아무것도 허락하지 않는 CSP를 붙인다.
fn response_headers(from: &HeaderMap) -> HeaderMap {
    let mut to = copy(from, &RESPONSE_HEADERS);
    let json = from
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    let content_type = match from.get(CONTENT_TYPE) {
        Some(v) if json => v.clone(),
        _ => HeaderValue::from_static("application/octet-stream"),
    };
    to.insert(CONTENT_TYPE, content_type);
    to.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    to.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    to
}

/// 요청 경로에서 `/remotes/{key}/api/` 뒤(퍼센트 인코딩 그대로)
fn remote_path(path: &str) -> &str {
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    path.splitn(5, '/').nth(4).unwrap_or_default()
}

/// 디코딩한 경로 조각
fn segments(path: &str) -> Vec<String> {
    path.split('/').map(percent_decode).collect()
}

/// admin이 아니어도 읽을 수 있는 경로인지([`READABLE`]와 `auth/whoami`)
fn readable(segments: &[String]) -> bool {
    match segments {
        [auth, whoami] if auth == "auth" && whoami == "whoami" => true,
        [first, ..] => READABLE.contains(&first.as_str()),
        [] => false,
    }
}

fn check_path(segments: &[String]) -> Result<(), ApiError> {
    if segments
        .iter()
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(ApiError::bad_request(
            "the remote path must not have empty, \".\" or \"..\" segments",
        ));
    }
    let unsupported = match segments {
        [first, ..] if first == "watch" => true,
        [vms, _, console] => vms == "vms" && console == "console",
        _ => false,
    };
    if unsupported {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "remote_path_unsupported",
            "watch and console are not proxied to remote clusters",
        ));
    }
    Ok(())
}

/// `names`의 헤더만 옮긴다.
fn copy(from: &HeaderMap, names: &[HeaderName]) -> HeaderMap {
    let mut to = HeaderMap::new();
    for name in names {
        for value in from.get_all(name) {
            to.append(name.clone(), value.clone());
        }
    }
    to
}

#[cfg(test)]
mod tests {
    use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN, SET_COOKIE};

    use super::*;

    #[test]
    fn only_listed_headers_cross() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            (AUTHORIZATION, "Bearer opl_local"),
            (COOKIE, "opl_session=s3cret"),
            (ORIGIN, "https://here"),
            (HOST, "here"),
            (CONTENT_TYPE, "application/json"),
            (CONTENT_LENGTH, "2"),
            (IF_MATCH, "7"),
            (ACCEPT, "application/json"),
            (SET_COOKIE, "x=y"),
            (ETAG, "\"1\""),
        ] {
            headers.insert(name, HeaderValue::from_static(value));
        }
        let sent = copy(&headers, &REQUEST_HEADERS);
        let mut names: Vec<&str> = sent.keys().map(HeaderName::as_str).collect();
        names.sort();
        assert_eq!(
            names,
            ["accept", "content-length", "content-type", "if-match"]
        );
        let returned = response_headers(&headers);
        let mut names: Vec<&str> = returned.keys().map(HeaderName::as_str).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "content-length",
                "content-security-policy",
                "content-type",
                "etag",
                "x-content-type-options"
            ]
        );
    }

    /// JSON만 그 Content-Type 그대로, 그 밖(없음 포함)은 octet-stream
    #[test]
    fn only_json_keeps_its_content_type() {
        for (given, sent) in [
            (Some("application/json"), "application/json"),
            (
                Some("Application/JSON; charset=utf-8"),
                "Application/JSON; charset=utf-8",
            ),
            (Some("text/html"), "application/octet-stream"),
            (Some("application/json+x"), "application/octet-stream"),
            (Some("image/svg+xml"), "application/octet-stream"),
            (None, "application/octet-stream"),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(given) = given {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(given));
            }
            let out = response_headers(&headers);
            assert_eq!(out[CONTENT_TYPE], sent, "{given:?}");
            assert_eq!(out[X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert_eq!(out[CONTENT_SECURITY_POLICY], "default-src 'none'; sandbox");
        }
    }

    #[test]
    fn non_admins_read_only_listed_paths() {
        for ok in [
            "vms",
            "vms/web",
            "storage-pools/p/volumes",
            "tasks",
            "auth/whoami",
            "%76ms",
            "subscription",
            "ha-groups/g",
            "ha-checks/stor",
            "placement-rules",
            "placement",
            "placement/recommendations",
        ] {
            assert!(readable(&segments(ok)), "{ok}");
        }
        for admin in [
            "users",
            "tokens/x",
            "realms",
            "audit",
            "telemetry",
            "remotes",
            "remotes/dr/api/vms",
            "auth",
            "auth/sessions",
            "auth/whoami/x",
            "%75sers",
            "watch",
            "",
            "unknown",
        ] {
            assert!(!readable(&segments(admin)), "{admin}");
        }
    }

    #[test]
    fn paths_follow_the_api_prefix() {
        for uri in ["/api/v1/remotes/dr/api/vms/web", "/remotes/dr/api/vms/web"] {
            assert_eq!(remote_path(uri), "vms/web", "{uri}");
        }
        assert_eq!(remote_path("/remotes/dr/api/a%2Fb"), "a%2Fb");
    }

    #[test]
    fn dot_empty_watch_and_console_paths_are_refused() {
        for ok in [
            "vms",
            "vms/web",
            "vms/a.b/actions/start",
            "vms/web/console/x",
            "nodes/watch",
        ] {
            assert!(check_path(&segments(ok)).is_ok(), "{ok}");
        }
        for bad in ["vms/..", "%2e%2E/vms", ".", "vms//web", "vms/", ""] {
            assert_eq!(
                check_path(&segments(bad)).unwrap_err().code,
                "invalid_request",
                "{bad}"
            );
        }
        for unsupported in [
            "watch",
            "%77atch",
            "watch/x",
            "vms/web/console",
            "vms/%77eb/console",
        ] {
            assert_eq!(
                check_path(&segments(unsupported)).unwrap_err().code,
                "remote_path_unsupported",
                "{unsupported}"
            );
        }
    }
}
