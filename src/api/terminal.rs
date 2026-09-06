//! `WS /api/sessions/:id/terminal`：终端流（docs/spec/api.md；MISSION §3.2）。
//!
//! 升级前就把会话解析成 `AttachSpec`——会话不存在 / 没有运行时 的错误以 HTTP 状态返回，
//! 客户端不用先建好 WS 再从帧里读错误。升级本身先过 `Principal`，再校验 `Origin` 与 `Host`
//! 同源（ADR-003 D7）。桥接循环在 `gateway`，这里只做 JSON 帧与 keepalive。会话属于 peer 时
//! 不在这里 attach：`forward::terminal` 向所属节点建同一条 WS，两边帧原样互转（agora-7ku.7）。

use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use serde::Deserialize;

use super::auth::same_origin;
use super::{forward, ApiError, AppState};
use crate::auth::{AuthError, Principal};
use crate::gateway::{
    AttachedPty, ClientMessage, ServerMessage, Utf8Stream, IDLE_TIMEOUT, PING_INTERVAL,
};
use crate::runtime::Size;

#[derive(Deserialize)]
pub struct TermQuery {
    cols: Option<u16>,
    rows: Option<u16>,
}

impl TermQuery {
    /// `?cols=&rows=` 作为初始尺寸，缺省 160×48（docs/spec/api.md）；0 与缺省同义。
    /// `/diff` 的升级（agora-h1k.5）也用它。
    pub(super) fn size(&self) -> Size {
        let default = Size::default();
        Size {
            cols: self.cols.filter(|c| *c > 0).unwrap_or(default.cols),
            rows: self.rows.filter(|r| *r > 0).unwrap_or(default.rows),
        }
    }
}

pub async fn upgrade(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    Query(q): Query<TermQuery>,
    uri: Uri,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // 同源校验挡的是浏览器带 cookie 的跨站升级；peer 不是浏览器，Bearer / 进程内注入的
    // Peer principal 跳过（ADR-003 D7 "Bearer 跳过"；agora-7ku.11 让 Peer 第一次真的到了这里）。
    if matches!(principal, Principal::Human { .. }) && !same_origin(&headers) {
        tracing::warn!(component = "api", principal = %principal.log_id(), "拒绝跨站 WS 升级");
        return Err(AuthError::CrossOrigin.into());
    }
    // 同源校验在前、路由在后：cookie 认证的跨站升级不管目标是谁都先拒。
    let id = match forward::hop(&state, &principal, &gid)? {
        forward::Hop::Local(id) => id,
        // 查询串原样带过去（cols / rows 由所属节点解释），与本机路径同一个 URL。
        forward::Hop::Peer(t) => {
            return forward::terminal(&principal, t, &gid, "/terminal", uri.query(), ws).await;
        }
    };
    let size = q.size();
    let spec = {
        let sessions = state.sessions.clone();
        let id = id.clone();
        super::sessions::blocking(&state.sessions, move |_| sessions.attach(&id, size)).await?
    };
    let log_id = principal.log_id();
    Ok(ws.on_upgrade(move |socket| async move {
        tracing::info!(component = "gateway", principal = %log_id, session = %id, "terminal attach");
        let pty = match AttachedPty::spawn(&spec, size) {
            Ok(p) => p,
            Err(err) => {
                tracing::error!(component = "gateway", session = %id, %err, "attach 启动失败");
                let _ = send(
                    &mut { socket },
                    &ServerMessage::Output {
                        data: format!("\r\n[agora] terminal error: {err}\r\n"),
                    },
                )
                .await;
                return;
            }
        };
        let pid = pty.pid();
        let confirmed = bridge(socket, pty, true).await;
        tracing::info!(
            component = "gateway",
            principal = %log_id,
            session = %id,
            pid,
            released = confirmed,
            "terminal detach"
        );
    }))
}

pub(super) async fn send(socket: &mut WebSocket, msg: &ServerMessage) -> Result<(), ()> {
    let text = serde_json::to_string(msg).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

/// WS ↔ PTY 桥接循环；返回值同 [`AttachedPty::detach`]。
///
/// `accept_input = false` 是只读终端（agora-h1k.5 的 `/diff`）：第一帧 status 报 `read_only` 而不是
/// `attached`，之后 `input` 帧一律丢弃、不进 PTY；resize / ping 照常。只读不另写一个循环——PTY 的
/// 释放顺序（SIGHUP → 确认退出 → 才 drop writer，src/gateway/mod.rs 顶部）是雷区，复制一份等于
/// 埋第二颗雷（2026-09-06）。
pub(super) async fn bridge(
    mut socket: WebSocket,
    mut pty: AttachedPty,
    accept_input: bool,
) -> bool {
    let status = if accept_input {
        "attached"
    } else {
        "read_only"
    };
    let _ = send(&mut socket, &ServerMessage::Status { status }).await;
    let mut decoder = Utf8Stream::default();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // 第一个 tick 立即完成，跳过。
    let mut last_seen = Instant::now();
    let mut exit = pty.exit_signal();
    loop {
        tokio::select! {
            biased;

            exit = exit.wait() => {
                // 读线程可能还有尾巴没送到；先把 channel 里的排空再报 exit。
                while let Ok(chunk) = tokio::time::timeout(std::time::Duration::from_millis(50), pty.read()).await {
                    match chunk {
                        Some(bytes) => {
                            let data = decoder.push(&bytes);
                            if !data.is_empty() && send(&mut socket, &ServerMessage::Output { data }).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                let _ = send(&mut socket, &ServerMessage::Exit { exit }).await;
                break;
            }

            chunk = pty.read() => {
                let Some(bytes) = chunk else { continue };
                let data = decoder.push(&bytes);
                if data.is_empty() {
                    continue;
                }
                if send(&mut socket, &ServerMessage::Output { data }).await.is_err() {
                    break;
                }
            }

            _ = ping.tick() => {
                if last_seen.elapsed() > IDLE_TIMEOUT {
                    tracing::info!(component = "gateway", "keepalive 超时，断开这一条 attach");
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }

            frame = socket.recv() => {
                let Some(Ok(frame)) = frame else { break };
                last_seen = Instant::now();
                let Message::Text(payload) = frame else { continue };
                match serde_json::from_str::<ClientMessage>(&payload) {
                    Ok(ClientMessage::Input { data }) => {
                        if accept_input {
                            pty.write(data.into_bytes()).await;
                        }
                    }
                    Ok(ClientMessage::Resize { cols, rows }) => {
                        if cols > 0 && rows > 0 {
                            pty.resize(Size { cols, rows });
                        }
                    }
                    Ok(ClientMessage::Ping) => {
                        if send(&mut socket, &ServerMessage::Pong).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => {} // 畸形帧忽略
                }
            }
        }
    }
    let confirmed = pty.detach().await;
    let _ = socket.send(Message::Close(None)).await;
    confirmed
}
