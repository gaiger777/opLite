//! 웹 UI 정적 파일. `web-ui` 기능으로 빌드하면 `web/dist`(`pnpm -C web build`의 결과)를 바이너리에
//! 넣고, API와 같은 주소에서 준다. 쿠키로 인증한 요청은 Origin이 서버와 같아야 하므로 UI는 같은
//! 출처에서 나가야 한다.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use opl_api::error::ApiError;
use rust_embed::EmbeddedFile;
#[cfg(feature = "web-ui")]
use rust_embed::RustEmbed;

/// 웹 UI가 붙는 응답의 보안 헤더. 스크립트와 연결은 같은 출처로만, 다른 사이트의 frame 안에는 뜨지 않는다.
/// 일부 UI 컴포넌트가 style 속성을 쓰므로 인라인 스타일은 허용하고, noVNC 커서는 `data:` 이미지다.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; \
                   base-uri 'none'; form-action 'self'";

/// 파일 이름에 내용 해시가 들어간 Vite 빌드 결과
const ASSETS: &str = "assets/";

#[cfg(feature = "web-ui")]
#[derive(RustEmbed)]
#[folder = "$OPL_WEB_DIST"]
// 디버그 빌드(테스트, clippy)는 UI를 빌드하지 않아도 되고 안내 페이지를 준다. 릴리스 빌드에서
// 빠뜨리면 컴파일 오류다. 디버그 빌드는 파일을 실행 중에 디스크에서 읽으므로, 나중에 만든 dist도
// 보인다. 경로는 build.rs가 `..` 없이 넘긴다.
#[cfg_attr(debug_assertions, allow_missing = true)]
struct Dist;

/// 경로로 빌드 결과 파일을 찾는다.
type Files = fn(&str) -> Option<EmbeddedFile>;

/// `/api/` 밖의 모든 경로를 웹 UI로 보낸다. API 라우터와 합친다.
pub fn router() -> Router {
    #[cfg(feature = "web-ui")]
    let files: Files = Dist::get;
    #[cfg(not(feature = "web-ui"))]
    let files: Files = |_| None;
    serve(files)
}

fn serve(files: Files) -> Router {
    Router::new().fallback(move |method: Method, uri: Uri, req: HeaderMap| async move {
        if let Some(r) = reject(&method, &uri) {
            return r;
        }
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        let mut r = match files(path) {
            Some(file) => file_response(path, file, &req),
            // 없는 빌드 결과를 index.html로 주면 브라우저가 HTML을 스크립트로 읽는다.
            None if path.starts_with(ASSETS) => StatusCode::NOT_FOUND.into_response(),
            // 화면 주소(`/vms/web-01`)는 index.html이 받아 브라우저에서 고른다.
            None => match files("index.html") {
                Some(file) => file_response("index.html", file, &req),
                None => return not_built(),
            },
        };
        secure(&mut r);
        r
    })
}

/// 웹 UI 없이 빌드한 opld의 안내
fn not_built() -> Response {
    let page = "<!doctype html><meta charset=\"utf-8\"><title>opLite</title>\
                <p>This opld was built without the web UI. Build it with \
                <code>pnpm -C web build</code> and the <code>web-ui</code> feature. \
                The API is at <code>/api/v1</code>.</p>";
    let mut r = (StatusCode::NOT_FOUND, axum::response::Html(page)).into_response();
    secure(&mut r);
    r
}

/// API 경로는 웹 UI로 대체하지 않고 API 오류(JSON)로 답한다. 정적 파일은 읽기만 한다.
fn reject(method: &Method, uri: &Uri) -> Option<Response> {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return Some(ApiError::not_found(format!("no API at {path}")).into_response());
    }
    if method != Method::GET && method != Method::HEAD {
        return Some(
            (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "GET, HEAD")],
            )
                .into_response(),
        );
    }
    None
}

/// 파일 응답. 내용 해시로 강한 ETag를 붙이고, `If-None-Match`가 맞으면 본문 없이 304로 답한다.
fn file_response(path: &str, file: EmbeddedFile, req: &HeaderMap) -> Response {
    let cache = if path.starts_with(ASSETS) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let etag = format!("\"{}\"", hex::encode(file.metadata.sha256_hash()));
    let fresh = matches(req, &etag);
    let etag = HeaderValue::from_str(&etag).expect("16진수 ETag");
    if fresh {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        let h = r.headers_mut();
        h.insert(header::ETAG, etag);
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        return r;
    }
    let mime = match file.metadata.mimetype() {
        "text/html" => "text/html; charset=utf-8".to_string(),
        other => other.to_string(),
    };
    let mut r = Response::new(Body::from(file.data.into_owned()));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::ETAG, etag);
    r
}

/// `If-None-Match`의 쉼표 목록 가운데 하나가 ETag와 같거나 `*`인가. 약한 비교(RFC 9110)라
/// 앞의 `W/`는 떼고 비교한다.
fn matches(req: &HeaderMap, etag: &str) -> bool {
    req.get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|v| v == "*" || v.strip_prefix("W/").unwrap_or(v) == etag)
}

fn secure(r: &mut Response) {
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

#[cfg(test)]
mod tests {
    use rust_embed::RustEmbed;

    use super::*;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/web"]
    struct Fixture;

    async fn get(
        app: Router,
        method: &str,
        path: &str,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        get_if(app, method, path, None).await
    }

    /// `If-None-Match`를 붙여 요청한다.
    async fn get_if(
        app: Router,
        method: &str,
        path: &str,
        if_none_match: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut req =
            reqwest::Client::new().request(method.parse().unwrap(), format!("http://{addr}{path}"));
        if let Some(v) = if_none_match {
            req = req.header(header::IF_NONE_MATCH, v);
        }
        let resp = req.send().await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        (status, headers, resp.text().await.unwrap())
    }

    #[tokio::test]
    async fn serves_files_and_falls_back_to_index_for_screens() {
        let (status, h, body) = get(serve(Fixture::get), "GET", "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture index"), "{body}");
        assert_eq!(h["content-type"], "text/html; charset=utf-8");
        assert_eq!(h["cache-control"], "no-cache");
        assert_eq!(h["content-security-policy"], CSP);
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["referrer-policy"], "no-referrer");

        let (status, h, body) = get(serve(Fixture::get), "GET", "/assets/app-1a2b3c.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture script"), "{body}");
        assert_eq!(h["content-type"], "text/javascript");
        assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");

        // 화면 주소는 index.html
        let (status, _, body) = get(serve(Fixture::get), "GET", "/vms/web-01").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture index"), "{body}");
        // 없는 빌드 결과는 index.html이 아니라 404
        let (status, _, _) = get(serve(Fixture::get), "GET", "/assets/gone-000.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn etag_revalidation_answers_not_modified() {
        // 모든 파일 응답에 내용 해시의 강한 ETag
        let (_, h, _) = get(serve(Fixture::get), "GET", "/").await;
        let etag = h["etag"].to_str().unwrap().to_string();
        let hash = hex::encode(Fixture::get("index.html").unwrap().metadata.sha256_hash());
        assert_eq!(etag, format!("\"{hash}\""));
        let (_, h, _) = get(serve(Fixture::get), "GET", "/assets/app-1a2b3c.js").await;
        assert_ne!(h["etag"], etag.as_str());
        // SPA 폴백은 index.html의 ETag
        let (_, h, _) = get(serve(Fixture::get), "GET", "/vms/web-01").await;
        assert_eq!(h["etag"], etag.as_str());

        // 같으면 본문 없이 304, 헤더는 그대로
        for path in ["/", "/vms/web-01"] {
            let (status, h, body) = get_if(serve(Fixture::get), "GET", path, Some(&etag)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{path}");
            assert!(body.is_empty(), "{body}");
            assert_eq!(h["etag"], etag.as_str());
            assert_eq!(h["cache-control"], "no-cache");
            assert_eq!(h["content-security-policy"], CSP);
            assert_eq!(h["x-content-type-options"], "nosniff");
            assert_eq!(h["referrer-policy"], "no-referrer");
        }
        // 쉼표 목록 가운데 하나, `*`, 약한 비교
        let list = format!("\"other\", {etag}");
        let weak = format!("W/{etag}");
        for v in [list.as_str(), "*", weak.as_str()] {
            let (status, _, _) = get_if(serve(Fixture::get), "GET", "/", Some(v)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{v}");
        }
        // 다르면 200과 본문
        let (status, h, body) = get_if(serve(Fixture::get), "GET", "/", Some("\"other\"")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture index"), "{body}");
        assert_eq!(h["etag"], etag.as_str());
    }

    #[tokio::test]
    async fn api_paths_and_writes_are_not_served_as_the_ui() {
        for (method, path) in [
            ("GET", "/api/v1/nope"),
            ("POST", "/api/v2/x"),
            ("GET", "/api"),
        ] {
            let (status, h, body) = get(serve(Fixture::get), method, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(h["content-type"], "application/json", "{method} {path}");
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["code"], "not_found");
        }
        let (status, h, _) = get(serve(Fixture::get), "POST", "/vms").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(h["allow"], "GET, HEAD");
        let (status, h, _) = get(serve(Fixture::get), "HEAD", "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(h["content-security-policy"], CSP);
    }

    #[tokio::test]
    async fn without_a_built_ui_explains_the_build() {
        let (status, h, body) = get(serve(|_| None), "GET", "/vms").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("without the web UI"), "{body}");
        assert_eq!(h["content-security-policy"], CSP);
        let (_, h, _) = get(serve(|_| None), "GET", "/api/v1/nope").await;
        assert_eq!(h["content-type"], "application/json");
    }
}
