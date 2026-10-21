//! opLite REST API 클라이언트.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::Value;

#[derive(Clone)]
pub struct Client {
    pub base: String,
    http: reqwest::Client,
    pub token: Option<String>,
}

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            token,
        }
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
        loop {
            let task = self.get(&format!("/tasks/{id}")).await?;
            match task["status"]["state"].as_str() {
                Some("succeeded") => return Ok(task),
                Some("failed") => bail!(
                    "task {id} failed: {}",
                    task["status"]["error"].as_str().unwrap_or("unknown error")
                ),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
}
