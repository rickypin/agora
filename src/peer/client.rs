//! 每个 peer 一个客户端任务（agora-7ku.5；MISSION §3.5；ADR-004；不变量 8）。
//!
//! 节点是 peer 的 API 客户端，用的就是浏览器那套 API，纪律也一样（docs/spec/api.md「消费纪律」）：
//!
//! 1. 连上先 `GET /api/system` 过 [`negotiate`]（MISSION §7.3）：不兼容 → 该 peer 标
//!    `incompatible_version`、不拉会话、不并入，**照常退避重试**——对方升级到同 major 后自愈。
//! 2. 兼容 → 先建 `WS /api/events` 再 `GET /api/sessions` 全量（先流后快照，中间不丢事件），
//!    全量进 [`PeerViews::replace`]，之后每帧事件进 [`PeerViews::apply`]，拿回的事件原样发进本机
//!    `EventBus`——浏览器只连本机一条 `/api/events`，peer 的变化从同一条流到达。
//! 3. 断线 → 行保留、标 stale（[`PeerViews::mark_stale`]），`PeerStates::failed` 记类型，按
//!    [`BackoffPolicy`] 退避重连（1 s 起、30 s 顶、永不放弃）。等待可被 [`Wake`] 打断：
//!    `retry_now` 缩短这一次等待（agora-7ku.6 "点开 stale 会话立即重试"），`reconnect` 在线时也
//!    强制断开重连。
//!
//! 时间一律本节点时钟（`PeerViews::now`）：`last_seen` 与行上的 `status_since` 用同一只表打。
//! 测试用 `InProcessTransport` 驱动整个循环，退避策略与时钟都可注入，不 sleep 等真实退避。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;

use super::backoff::{random_jitter, Backoff, BackoffPolicy};
use super::state::{PeerError, PeerStates};
use super::transport::{PeerTransport, WsMessage};
use super::view::{Applied, PeerViews, Wake};
use crate::api::version::{negotiate, ApiVersion, Compatibility, API_VERSION};
use crate::api::AppState;
use crate::events::{Event, EventBus};

/// 事件流 keepalive：每 20 s 发一个 `{"type":"ping"}`（服务端回 pong）；65 s 没有任何入站帧就当
/// 断了——与终端 WS 的数字一致（docs/spec/api.md）。没有它，对端整机睡眠（Mac 合盖）时 TCP
/// 不会主动断，这条流会永远"在线"却什么都收不到。
pub const KEEPALIVE: Duration = Duration::from_secs(20);
pub const DEAD_AFTER: Duration = Duration::from_secs(65);

/// 读 `/api/system` / `/api/sessions` 响应体的上限；几十个会话的列表远小于此。
const BODY_LIMIT: usize = 64 * 1024 * 1024;

/// 一次连接为什么结束。
enum Disconnect {
    /// [`Wake::reconnect`]：不算失败，不标 stale、不退避，立刻重来。
    Forced,
    /// 真的失败 / 断开：记类型、标 stale、退避。
    Failed(PeerError),
}

pub struct PeerClient {
    name: String,
    transport: Arc<dyn PeerTransport>,
    peers: PeerStates,
    views: PeerViews,
    events: EventBus,
    ours: ApiVersion,
    policy: BackoffPolicy,
    wake: Arc<Wake>,
}

impl PeerClient {
    /// 为 `state.registry` 里的一个 transport 建客户端：登记 peer 状态、把唤醒把手挂到
    /// `state.peer_views`（`retry_now(name)` / `reconnect(name)` 从那里找到它）。
    pub fn new(transport: Arc<dyn PeerTransport>, state: &AppState) -> Self {
        let name = transport.name().to_owned();
        let wake = Arc::new(Wake::default());
        state.peers.register(&name);
        state.peer_views.register_wake(&name, wake.clone());
        PeerClient {
            name,
            transport,
            peers: state.peers.clone(),
            views: state.peer_views.clone(),
            events: state.events.clone(),
            ours: API_VERSION,
            policy: BackoffPolicy::PEER,
            wake,
        }
    }

    /// 退避策略注入点：生产 `BackoffPolicy::PEER`，测试给毫秒级的。
    pub fn with_policy(mut self, policy: BackoffPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn wake(&self) -> Arc<Wake> {
        self.wake.clone()
    }

    /// 跑到进程结束为止：永不返回、永不放弃（不变量 8）。
    pub async fn run(self) {
        let mut backoff = Backoff::new(self.policy);
        loop {
            match self.connected(&mut backoff).await {
                Disconnect::Forced => {
                    tracing::info!(component = "peer", peer = %self.name, "按要求断开重连");
                }
                Disconnect::Failed(err) => {
                    self.peers.failed(&self.name, err);
                    self.publish(self.views.mark_stale(&self.name));
                    let delay = backoff.next_delay(random_jitter());
                    tracing::warn!(
                        component = "peer",
                        peer = %self.name,
                        error = ?err,
                        failures = backoff.failures(),
                        retry_in_ms = delay.as_millis() as u64,
                        "peer 断开，退避后重试"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = self.wake.wait() => {}
                    }
                }
            }
        }
    }

    /// 一次完整的连接：比版本 → 建流 → 拉全量 → 收增量，直到断开。
    async fn connected(&self, backoff: &mut Backoff) -> Disconnect {
        // ① 版本。读不出、major 不同都不读它的任何业务数据（api.md「api_version 兼容规则」）。
        let system = match self.get_json("/api/system").await {
            Ok(v) => v,
            Err(err) => return Disconnect::Failed(err),
        };
        match negotiate(self.ours, &system) {
            Ok(Compatibility::Same) => {}
            Ok(compat) => tracing::info!(
                component = "peer",
                peer = %self.name,
                ours = %self.ours,
                theirs = %system["api_version"],
                ?compat,
                "peer 的 API minor 与本机不同，同 major 照常对话"
            ),
            Err(err) => {
                tracing::warn!(component = "peer", peer = %self.name, %err, "peer API 版本不兼容，不并入它的会话");
                return Disconnect::Failed(PeerError::IncompatibleVersion);
            }
        }
        if let Some(node) = system.get("node").and_then(Value::as_str) {
            if node != self.name {
                // 行的 node 字段会全部对不上而被丢弃（view.rs 规则 1）——这是配置写错了名字。
                tracing::warn!(
                    component = "peer",
                    peer = %self.name,
                    their_node = node,
                    "peers[].name 与对方 node.id 不一致，它的会话行会被一跳规则全部丢弃"
                );
            }
        }
        // ② 先流后快照：与浏览器一样，连上（含重连）先对齐全量。
        let mut ws = match self.transport.connect_ws("/api/events").await {
            Ok(ws) => ws,
            Err(err) => return Disconnect::Failed(err.into()),
        };
        if let Err(err) = self.snapshot().await {
            return Disconnect::Failed(err);
        }
        backoff.reset();
        tracing::info!(component = "peer", peer = %self.name, rows = self.views.rows_of(&self.name).len(), "peer 已并入");
        // ③ 增量。
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
        let mut last_rx = Instant::now();
        loop {
            tokio::select! {
                msg = ws.next() => match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        last_rx = Instant::now();
                        self.peers.seen(&self.name, self.views.now());
                        if let Err(err) = self.handle_frame(text.as_str()).await {
                            return Disconnect::Failed(err);
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => {
                        return Disconnect::Failed(PeerError::Unreachable);
                    }
                    Some(Ok(_)) => last_rx = Instant::now(),
                    Some(Err(err)) => {
                        tracing::debug!(component = "peer", peer = %self.name, %err, "事件流出错");
                        return Disconnect::Failed(PeerError::Unreachable);
                    }
                },
                _ = tick.tick() => {
                    if last_rx.elapsed() > DEAD_AFTER {
                        tracing::warn!(component = "peer", peer = %self.name, "事件流 65 s 没有入站帧，当作断开");
                        return Disconnect::Failed(PeerError::Unreachable);
                    }
                    if ws.send(WsMessage::Text("{\"type\":\"ping\"}".into())).await.is_err() {
                        return Disconnect::Failed(PeerError::Unreachable);
                    }
                }
                force = self.wake.wait() => {
                    if force {
                        let _ = ws.close(None).await;
                        return Disconnect::Forced;
                    }
                    // retry_now 在线时无事：本来就连着。
                }
            }
        }
    }

    /// 一帧 = 一个事件数组（服务端攒批）。任何一条要求 resync 就整帧应用完再重拉一次。
    async fn handle_frame(&self, text: &str) -> Result<(), PeerError> {
        let batch = match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(a)) => a,
            Ok(other) => vec![other],
            Err(err) => {
                tracing::debug!(component = "peer", peer = %self.name, %err, "事件帧不是 JSON，忽略");
                return Ok(());
            }
        };
        let mut resync = false;
        for event in &batch {
            match self.views.apply(&self.name, event) {
                Applied::Publish(e) => self.events.publish(e),
                Applied::Resync => resync = true,
                Applied::Ignored => {}
            }
        }
        if resync {
            self.snapshot().await?;
        }
        Ok(())
    }

    /// `GET /api/sessions` 全量进视图；差分事件发进本机总线。
    async fn snapshot(&self) -> Result<(), PeerError> {
        let body = self.get_json("/api/sessions").await?;
        let Some(rows) = body.get("sessions").and_then(Value::as_array) else {
            tracing::warn!(component = "peer", peer = %self.name, "GET /api/sessions 的响应不是 {{ sessions: [...] }}");
            return Err(PeerError::Unreachable);
        };
        self.publish(self.views.replace(&self.name, rows.clone()));
        self.peers.seen(&self.name, self.views.now());
        Ok(())
    }

    /// 发一个 GET：传输错误按类型映射（`From<TransportError>`），401 是未授权，其它非 2xx 当
    /// 不可达；200 但不是 JSON 给 `Null`，让调用方按自己的语义判（`/api/system` 不是 JSON 就是
    /// "不是本协议的节点" → 不兼容；`/api/sessions` 不是 JSON → 不可达，保留旧视图）。
    async fn get_json(&self, path: &str) -> Result<Value, PeerError> {
        let req = Request::get(path)
            .body(Body::empty())
            .map_err(|_| PeerError::Unreachable)?;
        let resp = self.transport.request(req).await?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(PeerError::Unauthorized);
        }
        if !status.is_success() {
            tracing::warn!(component = "peer", peer = %self.name, path, %status, "peer 返回非 2xx");
            return Err(PeerError::Unreachable);
        }
        let bytes = axum::body::to_bytes(resp.into_body(), BODY_LIMIT)
            .await
            .map_err(|_| PeerError::Unreachable)?;
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn publish(&self, events: Vec<Event>) {
        for e in events {
            self.events.publish(e);
        }
    }
}

/// `main.rs`：为 `state.registry` 里的每个 peer 起一个客户端任务。
pub fn spawn_all(state: &AppState) -> Vec<tokio::task::JoinHandle<()>> {
    state
        .registry
        .peers()
        .map(|t| tokio::spawn(PeerClient::new(t.clone(), state).run()))
        .collect()
}
