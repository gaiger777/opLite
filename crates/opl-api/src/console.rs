//! `GET /api/v1/vms/{id}/console`: WebSocket 바이너리 메시지를 VM의 VNC 유닉스 소켓으로 잇는다.
//! VM이 다른 노드에서 돌면 노드 간 gRPC로 그 노드에 중계한다.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use opl_cluster::types::NodeId;
use opl_driver::DriverError;
use opl_peer::{ConsoleError, ConsoleStream};
use opl_types::resource::ResourceSpec;
use opl_types::vm::VmSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::auth::{Action, Principal};
use crate::error::ApiError;
use crate::resources::lookup;
use crate::state::AppState;

/// 다른 노드에 콘솔 스트림을 여는 데 허용하는 시간. TCP 연결 뒤 TLS/HTTP2/헤더 단계에서 멈추는
/// 노드 때문에 요청이 매달리지 않게 한다. 열린 뒤의 스트림에는 적용하지 않는다.
const CONSOLE_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

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
    let Some(node) = vm.status.node else {
        return Err(not_running());
    };
    if node != state.node {
        let Some(peers) = &state.peers else {
            return Err(not_running());
        };
        let Some(addr) = state
            .cluster
            .members()
            .addr(NodeId(node))
            .map(str::to_string)
        else {
            return Err(not_running());
        };
        let ws = ws?;
        let unavailable = || {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "console_unavailable",
                format!("cannot reach the node running VM {:?}", vm.name),
            )
        };
        let stream = tokio::time::timeout(CONSOLE_OPEN_TIMEOUT, peers.console(&addr, vm.id))
            .await
            .map_err(|_| unavailable())?
            .map_err(|e| match e {
                ConsoleError::NotRunning => {
                    ApiError::conflict("vm_not_running", format!("VM {:?} is not running", vm.name))
                }
                ConsoleError::Unavailable(_) => unavailable(),
            })?;
        return Ok(ws.on_upgrade(move |socket| relay(socket, stream)));
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

/// WebSocket과 다른 노드의 콘솔 스트림을 잇는다. 한쪽이 막혀도 다른 쪽이 멈추지 않게 두 방향을
/// 따로 돌리고, 어느 한 방향이 끝나면 끝낸다.
async fn relay(socket: WebSocket, stream: ConsoleStream) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (sender, mut receiver) = stream.into_split();
    let to_vm = async {
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    if !sender.send(data).await {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    };
    let from_vm = async {
        while let Some(data) = receiver.recv().await {
            if ws_tx.send(Message::binary(data)).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = to_vm => {}
        _ = from_vm => {}
    }
    let _ = ws_tx.send(Message::Close(None)).await;
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
