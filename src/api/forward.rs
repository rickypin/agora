//! 一跳转发（agora-7ku.7；MISSION §3.5 / §7.3；ADR-003 D8；ADR-004）。
//!
//! 会话 id `<node>:<id>` 的前缀决定一个写请求 / 终端流在哪执行：本机 → 交本地 handler；
//! 已配置的 peer → 同方法、同路径、同 body 经它的 [`PeerTransport`] 送过去，响应原样回
//! （状态码、content-type、body）；既不是本机也不是 peer → 404 `node_unknown`。
//!
//! **一跳**：请求本身来自 `Principal::Peer` 时不再转发——B 收到 A 对 `c:<id>` 的请求就到此为止，
//! 哪怕 B 自己配了 C 也答 `node_unknown`（ADR-004 "只导出本机会话，一跳"，同时防环）。
//!
//! 本模块不碰认证：浏览器的 cookie / Origin / Host / Authorization / Content-Length 一个都不带过去；
//! Bearer 由 `HttpsTransport` 自己加，进程内 fake 靠请求扩展注入 Peer 身份。也不碰确认
//! （ADR-003 D8）：`confirmed` 随 body 原样过去，所属节点按自己会话的 agent 状态判断，
//! 409 `needs_confirmation` 原样回到浏览器；这里没有任何一条代码路径能替所属节点把它置 true，
//! 也没有"调用方是 peer 就免确认"的分支——所属节点看到的就是一个普通的 Peer principal。
//! 守卫：`tests/forward.rs::kill_confirmation_enforced_at_owner`。

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite;

use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::peer::registry::Route;
use crate::peer::transport::{PeerTransport, PeerWs, TransportError, WsMessage};

/// 会话 id 的节点前缀该往哪走。
pub(super) enum Hop {
    /// 本机（含裸 id）：交本地 handler，附本机 id。
    Local(String),
    /// 已配置的 peer：经它的 transport 转发。
    Peer(Arc<dyn PeerTransport>),
}

/// `<node>:<id>` → 本机 id 或 peer transport。Unknown 与第二跳都是 404 `node_unknown`。
pub(super) fn hop(state: &AppState, principal: &Principal, gid: &str) -> Result<Hop, ApiError> {
    // 裸 id 视为本机：curl 手敲时少打一段（与 `sessions::local_id` 同一约定）。
    let Some((node, id)) = gid.split_once(':') else {
        return Ok(Hop::Local(gid.to_owned()));
    };
    match state.registry.route(node) {
        Route::Local => Ok(Hop::Local(id.to_owned())),
        Route::Peer(t) => match principal {
            // 一跳：来自 peer 的请求只能落在本机会话上。
            Principal::Peer { name } => Err(second_hop(state, node, name)),
            Principal::Human { .. } => {
                // 人点开了 stale peer 的会话（浏览器建终端 WS、或对它做任何写操作都从这里过）：
                // 插一次重试（agora-7ku.6；MISSION §3.5）。retry_now 只缩短客户端这一次退避等待，
                // 在线时无事、离线时一次点开只多一次尝试——不是轮询。转发本身照常进行：peer 真不可达
                // 就由 transport 报 peer_unreachable，重试成功与否由事件流告诉浏览器。
                if state.peer_views.is_stale(node) == Some(true) {
                    state.peer_views.retry_now(node);
                }
                Ok(Hop::Peer(t))
            }
        },
        Route::Unknown => Err(node_unknown(state, node)),
    }
}

/// 写 handler 的入口：本机 → `Local(id)`，handler 自己做；peer → 已转发，`Forwarded(resp)` 原样回。
pub(super) enum Routed {
    Local(String),
    Forwarded(Response),
}

/// 没有 body 的写请求（cleanup / DELETE）传这个。
pub(super) const NO_BODY: Option<&()> = None;

/// 六个写 handler 各调一次：`suffix` 是 `/api/sessions/<gid>` 之后的那段（`/kill`、`""`…），
/// `body` 是 handler 已解析的请求体——转发的是它再序列化的形态，所以 `confirmed` 之类只能
/// 原样过去、不可能被这里改写；代价是本节点不认识的字段到不了所属节点。2026-09-06 判断可接受：
/// 写端点的 body 都是本节点自己定义的三个小结构，浏览器页面又是本节点发的，不会带本节点
/// 不认识的字段。
pub(super) async fn route<B: Serialize>(
    state: &AppState,
    principal: &Principal,
    gid: &str,
    method: Method,
    suffix: &str,
    body: Option<&B>,
) -> Result<Routed, ApiError> {
    match hop(state, principal, gid)? {
        Hop::Local(id) => Ok(Routed::Local(id)),
        Hop::Peer(t) => {
            let path = format!("/api/sessions/{gid}{suffix}");
            relay(principal, &t, method, &path, body)
                .await
                .map(Routed::Forwarded)
        }
    }
}

/// 同方法同路径同 body 经 transport 送到所属节点，响应原样回。请求上只有 content-type：
/// 主机、TLS、Bearer 归 transport；浏览器带来的头一个都不转。
async fn relay<B: Serialize>(
    principal: &Principal,
    t: &Arc<dyn PeerTransport>,
    method: Method,
    path: &str,
    body: Option<&B>,
) -> Result<Response, ApiError> {
    let mut req = Request::builder().method(method.clone()).uri(path);
    let body = match body {
        Some(b) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(b).map_err(|e| internal(&format!("序列化请求体: {e}")))?)
        }
        None => Body::empty(),
    };
    let req = req
        .body(body)
        .map_err(|e| internal(&format!("组装转发请求: {e}")))?;
    let resp = t
        .request(req)
        .await
        .map_err(|err| transport_failed(t.name(), err))?;
    tracing::info!(
        component = "api",
        principal = %principal.log_id(),
        peer = %t.name(),
        %method,
        path,
        status = resp.status().as_u16(),
        "forwarded"
    );
    Ok(passthrough(resp))
}

/// 响应原样回：状态码 + content-type + body（流式，不落地）。其它头一律不带——hop-by-hop 头
/// （connection / transfer-encoding）不该跨连接；Set-Cookie 之类是所属节点对这条 Bearer 连接
/// 说的话，与浏览器无关。
fn passthrough(resp: Response) -> Response {
    let (parts, body) = resp.into_parts();
    let mut out = Response::new(body);
    *out.status_mut() = parts.status;
    if let Some(ct) = parts.headers.get(header::CONTENT_TYPE) {
        out.headers_mut().insert(header::CONTENT_TYPE, ct.clone());
    }
    out
}

// ---------- 终端流 ----------

/// peer 会话的 `WS /api/sessions/:id<suffix>`（`/terminal`，以及 agora-h1k.5 的只读 `/diff`）：先向
/// 所属节点建好 WS（会话不存在 / 没有运行时 / peer 不可达都以 HTTP 状态回，与本机路径一致——
/// 客户端不用先建好 WS 再从关闭码里猜），再升级浏览器这条，然后两边帧原样互转。`suffix` 是
/// `/api/sessions/<gid>` 之后那段，与 [`route`] 同一约定。
pub(super) async fn terminal(
    principal: &Principal,
    t: Arc<dyn PeerTransport>,
    gid: &str,
    suffix: &str,
    query: Option<&str>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let path = match query {
        Some(q) => format!("/api/sessions/{gid}{suffix}?{q}"),
        None => format!("/api/sessions/{gid}{suffix}"),
    };
    let upstream = t
        .connect_ws(&path)
        .await
        .map_err(|err| transport_failed(t.name(), err))?;
    let log_id = principal.log_id();
    let peer = t.name().to_owned();
    let gid = gid.to_owned();
    let suffix = suffix.to_owned();
    Ok(ws.on_upgrade(move |socket| async move {
        tracing::info!(component = "gateway", principal = %log_id, peer = %peer, session = %gid, path = %suffix, "terminal attach (forwarded)");
        let exited = bridge(socket, upstream).await;
        tracing::info!(component = "gateway", principal = %log_id, peer = %peer, session = %gid, path = %suffix, exited, "terminal detach (forwarded)");
    }))
}

/// 浏览器 ↔ 所属节点的双向桥：字节（output / input）、resize、exit、Ping / Pong、Close 全部
/// 原样过，不解释、不合并、不补 keepalive——所属节点的 20 s Ping / 65 s idle 经这里到达浏览器，
/// 浏览器的 Pong 经这里回去，活性判断仍是端到端的。任一侧断（None / Err / Close）就关另一侧；
/// exit 帧转过去后主动关两边——所属节点在 exit 之后还要等 attach 退出（最多 3 s）才关，
/// 浏览器不必陪它等。返回值：是否见到 exit。
async fn bridge(mut browser: WebSocket, mut peer: PeerWs) -> bool {
    let mut exited = false;
    loop {
        tokio::select! {
            from_browser = browser.recv() => {
                let Some(Ok(msg)) = from_browser else { break };
                let closing = matches!(msg, Message::Close(_));
                if peer.send(to_peer(msg)).await.is_err() || closing {
                    break;
                }
            }
            from_peer = peer.next() => {
                let Some(Ok(msg)) = from_peer else { break };
                let Some(msg) = to_browser(msg) else { continue };
                let closing = matches!(msg, Message::Close(_));
                let is_exit = matches!(&msg, Message::Text(t) if is_exit_frame(t));
                if browser.send(msg).await.is_err() || closing {
                    break;
                }
                if is_exit {
                    exited = true;
                    break;
                }
            }
        }
    }
    let _ = peer.close(None).await;
    let _ = browser.send(Message::Close(None)).await;
    exited
}

/// 只看顶层 `type`：output 帧的 data 里就算写着 `"type":"exit"` 也是字符串内容，解析不会认错。
#[derive(Deserialize)]
struct Tagged {
    #[serde(rename = "type")]
    kind: String,
}

fn is_exit_frame(text: &str) -> bool {
    serde_json::from_str::<Tagged>(text).is_ok_and(|t| t.kind == "exit")
}

/// axum 与 tungstenite 的 `Message` 同形不同型（axum 抄了 tungstenite 的枚举但没公开转换），
/// 两个方向各写一遍；`Frame`（原始帧）按 tungstenite 维护者的建议忽略。
fn to_peer(msg: Message) -> WsMessage {
    match msg {
        Message::Text(t) => WsMessage::Text(t.as_str().into()),
        Message::Binary(b) => WsMessage::Binary(b),
        Message::Ping(b) => WsMessage::Ping(b),
        Message::Pong(b) => WsMessage::Pong(b),
        Message::Close(f) => WsMessage::Close(f.map(|f| tungstenite::protocol::CloseFrame {
            code: f.code.into(),
            reason: f.reason.as_str().into(),
        })),
    }
}

fn to_browser(msg: WsMessage) -> Option<Message> {
    Some(match msg {
        WsMessage::Text(t) => Message::Text(t.as_str().into()),
        WsMessage::Binary(b) => Message::Binary(b),
        WsMessage::Ping(b) => Message::Ping(b),
        WsMessage::Pong(b) => Message::Pong(b),
        WsMessage::Close(f) => Message::Close(f.map(|f| CloseFrame {
            code: f.code.into(),
            reason: f.reason.as_str().into(),
        })),
        WsMessage::Frame(_) => return None,
    })
}

// ---------- 错误 ----------

fn node_unknown(state: &AppState, node: &str) -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        kind: "node_unknown",
        message: format!(
            "会话属于节点 {node}，本节点是 {}，它也不是已配置的 peer",
            state.registry.local()
        ),
    }
}

fn second_hop(state: &AppState, node: &str, from: &str) -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        kind: "node_unknown",
        message: format!(
            "会话属于节点 {node}；这个请求来自 peer {from}，本节点 {} 不做第二跳（ADR-004 一跳）",
            state.registry.local()
        ),
    }
}

/// 传输层失败按类型映射（MISSION §2.3 规则 10；类型清单见 docs/spec/api.md「一跳转发」）：
/// 指纹不匹配独立成类、绝不并进"不可达"（ADR-003 D4）；所属节点对 WS 升级的非 101 拒绝把它的
/// 状态码原样带回（握手失败没有 body，只有状态码可传）。所属节点对 HTTP 请求的非 2xx 不是
/// 传输错误，`relay` 已经原样回了。
fn transport_failed(peer: &str, err: TransportError) -> ApiError {
    let (status, kind) = match &err {
        TransportError::FingerprintMismatch { .. } => {
            (StatusCode::BAD_GATEWAY, "peer_fingerprint_mismatch")
        }
        TransportError::Config(_) | TransportError::Tls(_) => {
            (StatusCode::BAD_GATEWAY, "peer_config")
        }
        TransportError::WsRejected(status) => (*status, "peer_rejected"),
        TransportError::Timeout(_)
        | TransportError::Unreachable(_)
        | TransportError::Protocol(_) => (StatusCode::BAD_GATEWAY, "peer_unreachable"),
    };
    tracing::warn!(component = "api", %peer, error = %err, "转发到 peer 失败");
    ApiError {
        status,
        kind,
        message: format!("转发到 peer {peer} 失败: {err}"),
    }
}

fn internal(message: &str) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        kind: "internal",
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_frame_is_recognized_by_top_level_type_only() {
        assert!(is_exit_frame(
            r#"{"type":"exit","exit":{"kind":"code","value":0}}"#
        ));
        assert!(!is_exit_frame(
            r#"{"type":"output","data":"{\"type\":\"exit\"}"}"#
        ));
        assert!(!is_exit_frame(r#"{"type":"status","status":"attached"}"#));
        assert!(!is_exit_frame("not json"));
    }

    #[test]
    fn transport_errors_map_by_type_not_text() {
        let e = transport_failed(
            "zuan",
            TransportError::Timeout(std::time::Duration::from_secs(5)),
        );
        assert_eq!(
            (e.status, e.kind),
            (StatusCode::BAD_GATEWAY, "peer_unreachable")
        );
        let e = transport_failed(
            "zuan",
            TransportError::FingerprintMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
        );
        assert_eq!(
            (e.status, e.kind),
            (StatusCode::BAD_GATEWAY, "peer_fingerprint_mismatch")
        );
        let e = transport_failed("zuan", TransportError::WsRejected(StatusCode::NOT_FOUND));
        assert_eq!((e.status, e.kind), (StatusCode::NOT_FOUND, "peer_rejected"));
    }
}
