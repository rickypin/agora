//! `WS /api/events`：全局事件流（docs/spec/api.md）。
//!
//! 升级请求先过 `Principal`（GET，cookie），再校验 `Origin` 与 `Host` 同源——浏览器的 WS
//! 不受同源策略限制，任何站点都能发起带 cookie 的升级（ADR-003 D7）。
//!
//! 服务端合并突发：收到第一条事件后最多再等 [`BATCH_WINDOW`] 攒同批，同一会话的连续
//! 状态变化只留最后一条，一帧发一个 JSON 数组。客户端落后超过总线容量时收到
//! `[{"type":"resync"}]`，必须重拉 `GET /api/sessions`。

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use tokio::sync::broadcast::error::RecvError;

use super::auth::same_origin;
use super::{ApiError, AppState};
use crate::auth::{AuthError, Principal};
use crate::events::{coalesce, Event};

/// 攒批窗口。前端还会再合并 ~300 ms（api.md 消费纪律），这里只挡住毫秒级的突发。
pub const BATCH_WINDOW: Duration = Duration::from_millis(50);

pub async fn upgrade(
    principal: Principal,
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    if !same_origin(&headers) {
        tracing::warn!(component = "api", principal = %principal.log_id(), "拒绝跨站 WS 升级");
        return Err(AuthError::CrossOrigin.into());
    }
    let log_id = principal.log_id();
    Ok(ws.on_upgrade(move |socket| async move {
        tracing::info!(component = "api", principal = %log_id, "events 订阅开始");
        run(socket, state).await;
        tracing::info!(component = "api", principal = %log_id, "events 订阅结束");
    }))
}

async fn run(mut socket: WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            first = rx.recv() => {
                let mut batch = match first {
                    Ok(e) => vec![e],
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(component = "api", dropped = n, "events 客户端落后，要求重同步");
                        vec![Event::Resync]
                    }
                    Err(RecvError::Closed) => break,
                };
                // 攒同批：窗口内到达的事件一起发；落后也并进同一帧。
                let deadline = tokio::time::sleep(BATCH_WINDOW);
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        _ = &mut deadline => break,
                        more = rx.recv() => match more {
                            Ok(e) => batch.push(e),
                            Err(RecvError::Lagged(_)) => batch.push(Event::Resync),
                            Err(RecvError::Closed) => break,
                        },
                    }
                }
                let batch = coalesce(batch);
                let Ok(text) = serde_json::to_string(&batch) else { continue };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(t))) if t.contains("\"ping\"") => {
                    if socket.send(Message::Text("{\"type\":\"pong\"}".into())).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}
