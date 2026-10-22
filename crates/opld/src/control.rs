//! 로컬 제어 소켓. `oplctl cluster init|join|leave`가 이 노드의 opld에 요청한다. 소켓은 opld를
//! 실행한 사용자만 쓸 수 있으므로(0600) 따로 인증하지 않는다.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::bail;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub type Reply = oneshot::Sender<Result<Value, ControlError>>;

/// 제어 요청. 노드의 상태를 바꾸므로 main 루프가 하나씩 처리한다.
pub enum Request {
    Init { cluster_name: String, reply: Reply },
    Join { url: String, reply: Reply },
    Leave { reply: Reply },
}

#[derive(Debug)]
pub struct ControlError {
    pub status: StatusCode,
    pub message: String,
}

impl ControlError {
    /// 노드의 지금 상태에서는 할 수 없는 요청
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    pub fn failed(e: anyhow::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("{e:#}"),
        }
    }
}

/// `GET /v1/status`의 내용
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub node: Uuid,
    pub name: String,
    /// `uninitialized` | `member`
    pub state: &'static str,
    pub cluster: Option<String>,
}

#[derive(Clone)]
struct Ctx {
    requests: mpsc::Sender<Request>,
    status: Arc<Mutex<Status>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitBody {
    cluster_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinBody {
    url: String,
}

type Answer = Result<Json<Value>, (StatusCode, Json<Value>)>;

async fn ask(ctx: &Ctx, make: impl FnOnce(Reply) -> Request) -> Answer {
    let (reply, rx) = oneshot::channel();
    let gone = || {
        let body = json!({ "message": "opld is shutting down" });
        (StatusCode::SERVICE_UNAVAILABLE, Json(body))
    };
    ctx.requests.send(make(reply)).await.map_err(|_| gone())?;
    match rx.await.map_err(|_| gone())? {
        Ok(v) => Ok(Json(v)),
        Err(e) => Err((e.status, Json(json!({ "message": e.message })))),
    }
}

fn router(ctx: Ctx) -> Router {
    Router::new()
        .route(
            "/v1/status",
            get(|State(ctx): State<Ctx>| async move {
                Json(serde_json::to_value(ctx.status.lock().unwrap().clone()).unwrap())
            }),
        )
        .route(
            "/v1/init",
            post(
                |State(ctx): State<Ctx>, Json(b): Json<InitBody>| async move {
                    ask(&ctx, |reply| Request::Init {
                        cluster_name: b.cluster_name,
                        reply,
                    })
                    .await
                },
            ),
        )
        .route(
            "/v1/join",
            post(
                |State(ctx): State<Ctx>, Json(b): Json<JoinBody>| async move {
                    ask(&ctx, |reply| Request::Join { url: b.url, reply }).await
                },
            ),
        )
        .route(
            "/v1/leave",
            post(|State(ctx): State<Ctx>| async move {
                ask(&ctx, |reply| Request::Leave { reply }).await
            }),
        )
        .with_state(ctx)
}

/// 제어 소켓을 연다. 다른 opld가 이미 쓰고 있으면 실패한다. 이전 opld가 남긴 소켓 파일은 지운다.
/// 권한을 바꾸기 전에 다른 사용자가 연결하지 못하도록, 옆에 만든 주인만 들어갈 수 있는(0700) 임시
/// 디렉터리에서 소켓을 묶고 0600으로 바꾼 뒤 제자리로 옮긴다.
pub async fn serve(
    path: &Path,
    requests: mpsc::Sender<Request>,
    status: Arc<Mutex<Status>>,
) -> anyhow::Result<JoinHandle<()>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if tokio::net::UnixStream::connect(path).await.is_ok() {
            bail!("{} is in use; is another opld running?", path.display());
        }
        std::fs::remove_file(path)?;
    }
    // 유닉스 소켓 경로 길이 제한(104바이트) 때문에 임시 경로를 짧게 둔다.
    let tmp = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!(".opl{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::DirBuilder::new().mode(0o700).create(&tmp)?;
    let bound = (|| {
        let sock = tmp.join("s");
        let listener = UnixListener::bind(&sock)?;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&sock, path)?;
        std::io::Result::Ok(listener)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    let listener = bound?;
    let app = router(Ctx { requests, status });
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "control socket stopped");
        }
    }))
}
