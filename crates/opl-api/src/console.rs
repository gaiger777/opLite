//! `GET /api/v1/vms/{id}/console`: WebSocket 바이너리 메시지를 VM의 VNC 유닉스 소켓으로 잇는다.

use std::sync::Arc;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use opl_driver::DriverError;
use opl_types::resource::ResourceSpec;
use opl_types::vm::VmSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::auth::{Action, Principal};
use crate::error::ApiError;
use crate::resources::lookup;
use crate::state::AppState;

/// VM이 이 노드에서 돌고 있어야 한다. 다른 노드로 중계하는 것은 노드 간 통신이 생긴 뒤에 한다.
#[utoipa::path(get, path = "/vms/{key}/console", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 101, description = "WebSocket carrying the VNC (RFB) stream"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn console(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let not_running = || {
        ApiError::conflict(
            "vm_not_running",
            format!("VM {:?} is not running on this node", vm.name),
        )
    };
    if vm.status.node != Some(state.node) {
        return Err(not_running());
    }
    let path = match state.hv.console_endpoint(vm.id).await {
        Ok(path) => path,
        Err(DriverError::NotRunning(_)) => return Err(not_running()),
        Err(e) => return Err(ApiError::internal(e)),
    };
    let ws = ws?;
    let vnc = UnixStream::connect(&path)
        .await
        .map_err(|_| not_running())?;
    Ok(ws.on_upgrade(move |socket| bridge(socket, vnc)))
}

async fn bridge(mut socket: WebSocket, mut vnc: UnixStream) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        tokio::select! {
            n = vnc.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if socket.send(Message::binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(data))) => {
                    if vnc.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}
