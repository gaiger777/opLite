//! opLite REST API 클라이언트.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::Value;

/// Task가 이보다 오래 끝나지 않으면 기다리기를 그만둔다. Task 실행기의 제한 시간(10분)보다 길다.
const TASK_WAIT: Duration = Duration::from_secs(15 * 60);

/// Task 조회 요청 하나의 제한 시간. 서버가 응답하지 않아도 전체 기다리기 제한(`TASK_WAIT`)이 지켜지게 한다.
const TASK_POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// 검증 없이 받는 CA 인증서 본문의 최대 크기
const CA_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub(crate) http: reqwest::Client,
    pub token: Option<String>,
    /// 서버의 클러스터 CA(PEM). 있으면 서버 인증서를 이 CA로만 확인한다(콘솔 WebSocket도).
    pub ca_pem: Option<String>,
}

impl Client {
    pub fn new(base: &str, token: Option<String>, ca_pem: Option<String>) -> anyhow::Result<Self> {
        let mut http = reqwest::Client::builder();
        if let Some(pem) = &ca_pem {
            http = http.tls_certs_only([reqwest::Certificate::from_pem(pem.as_bytes())?]);
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http: http.build()?,
            token,
            ca_pem,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let r = self.http.request(method, self.url(path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    /// 성공하면 본문(JSON, 없으면 null), 실패하면 API 오류의 메시지와 코드
    pub async fn send(&self, r: RequestBuilder) -> anyhow::Result<Value> {
        let resp = r
            .send()
            .await
            .with_context(|| format!("cannot reach {}", self.base))?;
        let status = resp.status();
        let text = resp.text().await?;
        let body: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        if status.is_success() {
            return Ok(body);
        }
        if status == StatusCode::UNAUTHORIZED && body["code"] == "unauthorized" {
            bail!("not logged in or the token is no longer valid; run `oplctl login`");
        }
        match (body["message"].as_str(), body["code"].as_str()) {
            (Some(message), Some(code)) => bail!("{message} ({code})"),
            _ => bail!("{status}: {body}"),
        }
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        self.send(self.request(Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.send(self.request(Method::POST, path).json(body)).await
    }

    pub async fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.send(self.request(Method::PUT, path).json(body)).await
    }

    pub async fn delete(&self, path: &str) -> anyhow::Result<Value> {
        self.send(self.request(Method::DELETE, path)).await
    }

    /// 파일을 읽어 가며 본문으로 보낸다(이미지는 수 GB일 수 있다).
    pub async fn upload(
        &self,
        path: &str,
        query: &[(&str, &str)],
        file: &Path,
    ) -> anyhow::Result<Value> {
        let f = tokio::fs::File::open(file)
            .await
            .with_context(|| format!("cannot open {}", file.display()))?;
        let len = f.metadata().await?.len();
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(f));
        let r = self
            .request(Method::POST, path)
            .query(query)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(body);
        self.send(r).await
    }

    /// Task가 끝날 때까지 기다린다. 실패했으면 그 오류를 돌려준다.
    pub async fn wait_task(&self, task: &Value) -> anyhow::Result<Value> {
        let id = task["id"].as_str().context("response has no task id")?;
        let deadline = Instant::now() + TASK_WAIT;
        loop {
            let poll = self
                .request(Method::GET, &format!("/tasks/{id}"))
                .timeout(TASK_POLL_TIMEOUT);
            let task = self.send(poll).await?;
            match task["status"]["state"].as_str() {
                Some("succeeded") => return Ok(task),
                Some("failed") => bail!(
                    "task {id} failed: {}",
                    task["status"]["error"].as_str().unwrap_or("unknown error")
                ),
                state if Instant::now() > deadline => bail!(
                    "task {id} did not finish in {} minutes (state: {}); is opld running on node {}?",
                    TASK_WAIT.as_secs() / 60,
                    state.unwrap_or("unknown"),
                    task["spec"]["node"].as_str().unwrap_or("?")
                ),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
}

/// 서버의 CA 인증서(PEM)를 인증서 검증 없이 받는다. 받는 쪽은 지문을 확인해야 한다.
pub async fn fetch_ca(server: &str) -> anyhow::Result<String> {
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = format!("{}/api/v1/cluster/ca", server.trim_end_matches('/'));
    let mut resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        bail!(
            "cannot read the cluster CA from {server}: {}",
            resp.status()
        );
    }
    // 검증 없이 받는 본문이므로 크기를 제한한다.
    let too_big = || anyhow::anyhow!("the cluster CA from {server} is larger than 64 KiB");
    if resp
        .content_length()
        .is_some_and(|n| n > CA_MAX_BYTES as u64)
    {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > CA_MAX_BYTES {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).context("the cluster CA is not valid UTF-8")
}

/// 인증서(PEM)의 SHA-256 지문(`sha256:<16진수>`)
pub fn fingerprint(pem: &str) -> anyhow::Result<String> {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;
    use sha2::{Digest, Sha256};
    let der = CertificateDer::from_pem_slice(pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid certificate: {e}"))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(&der))))
}

/// 이 기계의 opld 제어 소켓으로 보내는 요청(`oplctl cluster init|join|leave`)
pub async fn local(socket: &Path, path: &str, body: &Value) -> anyhow::Result<Value> {
    let http = reqwest::Client::builder().unix_socket(socket).build()?;
    let resp = http
        .post(format!("http://opld/v1{path}"))
        .json(body)
        .send()
        .await
        .with_context(|| format!("cannot reach opld at {}; is it running?", socket.display()))?;
    let status = resp.status();
    if status.is_success() {
        // 일회용 비밀번호 등이 들어 있을 수 있으니 해석 오류를 숨기지 않는다.
        return resp
            .json()
            .await
            .context("cannot parse the response from opld");
    }
    let body: Value = resp.json().await.unwrap_or_default();
    match body["message"].as_str() {
        Some(message) => bail!("{message}"),
        None => bail!("request failed: {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert_pem(name: &str) -> String {
        rcgen::generate_simple_self_signed(vec![name.to_string()])
            .unwrap()
            .cert
            .pem()
    }

    #[test]
    fn fingerprint_format_and_determinism() {
        let a = cert_pem("a.example");
        let fp = fingerprint(&a).unwrap();
        let hex = fp.strip_prefix("sha256:").unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert_eq!(fp, fingerprint(&a).unwrap());
        assert_ne!(fp, fingerprint(&cert_pem("b.example")).unwrap());
    }

    #[test]
    fn fingerprint_rejects_garbage() {
        assert!(fingerprint("not a pem").is_err());
    }
}
