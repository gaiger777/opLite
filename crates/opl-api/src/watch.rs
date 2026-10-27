//! `GET /api/v1/watch?kinds=vm,task&since=<revision>`: 커밋된 변경을 WebSocket으로 보낸다.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_types::resource::Kind;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Query};
use crate::state::AppState;

/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 7] = [
    Kind::Vm,
    Kind::Volume,
    Kind::StoragePool,
    Kind::Node,
    Kind::Task,
    Kind::Network,
    Kind::Snapshot,
];

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct WatchQuery {
    /// 쉼표로 구분한 종류 이름. 예: `vm,task`
    pub kinds: String,
    /// 목록 응답의 `revision`. 그 뒤의 변경부터 받는다.
    pub since: u64,
}

fn parse_kinds(s: &str) -> Result<BTreeSet<Kind>, ApiError> {
    s.split(',')
        .map(|name| {
            WATCHABLE
                .into_iter()
                .find(|k| k.as_str() == name.trim())
                .ok_or_else(|| ApiError::bad_request(format!("cannot watch kind {name:?}")))
        })
        .collect()
}

/// 이벤트는 `opl_cluster::watch::WatchEvent`의 JSON이다. 이어 받을 수 없게 되면
/// `{"type":"error","code":"expired"|"lagged"|"closed"}`를 보내고 닫는다. 클라이언트는 목록을
/// 다시 읽고 새로 구독한다.
#[utoipa::path(get, path = "/watch", tag = "watch",
    params(WatchQuery),
    responses((status = 101, description = "WebSocket stream of change events"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn watch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<WatchQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let kinds = parse_kinds(&q.kinds)?;
    for kind in &kinds {
        state.authorize(&principal, Action::Read, *kind)?;
    }
    let watch = state.cluster.watch(q.since).map_err(|e| match e {
        WatchError::Expired { .. } => ApiError::conflict("expired", e.to_string()),
        other => ApiError::internal(other),
    })?;
    Ok(ws?.on_upgrade(move |socket| stream(socket, watch, kinds)))
}

fn wanted(event: &WatchEvent, kinds: &BTreeSet<Kind>) -> bool {
    match event {
        WatchEvent::Put { kind, .. } | WatchEvent::Deleted { kind, .. } => kinds.contains(kind),
        WatchEvent::Reset { .. } => true,
    }
}

async fn stream(mut socket: WebSocket, mut watch: Watch, kinds: BTreeSet<Kind>) {
    loop {
        tokio::select! {
            ev = watch.next() => {
                let text = match ev {
                    Ok(ev) if wanted(&ev, &kinds) => serde_json::to_string(&ev),
                    Ok(_) => continue,
                    Err(e) => {
                        let code = match e {
                            WatchError::Expired { .. } => "expired",
                            WatchError::Lagged => "lagged",
                            WatchError::Closed => "closed",
                        };
                        let msg = serde_json::json!({
                            "type": "error",
                            "code": code,
                            "message": e.to_string(),
                        });
                        let _ = socket.send(Message::text(msg.to_string())).await;
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                };
                let Ok(text) = text else { return };
                if socket.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_watchable() {
        let kinds = parse_kinds("vm, snapshot").unwrap();
        assert!(kinds.contains(&Kind::Snapshot));
        assert!(parse_kinds("user").is_err());
    }
}
