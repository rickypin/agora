//! 每个 peer 一个客户端任务（agora-7ku.5；MISSION §3.5；ADR-004；不变量 8）。
//!
//! 节点是 peer 的 API 客户端，用的就是浏览器那套 API，纪律也一样（docs/spec/api.md「消费纪律」）：
//!
//! 1. 连上先 `GET /api/system` 过 [`negotiate`]（MISSION §7.3）：不兼容 → 该 peer 标
//!    `incompatible_version`、不拉会话、不并入，**照常退避重试**——对方升级到同 major 后自愈。
//! 2. 兼容 → 先建 `WS /api/events` 再 `GET /api/sessions` 全量（先流后快照，中间不丢事件），
//!    全量进 [`PeerViews::replace`]，之后每帧事件进 [`PeerViews::apply`]，拿回的事件原样发进本机
//!    `EventBus`——浏览器只连本机一条 `/api/events`，peer 的变化从同一条流到达。
//! 3. 断线 → 行保留、标 stale 并写上"上次见到"（[`PeerViews::mark_stale`]，值取 `PeerState::last_seen`），
//!    `PeerStates::failed` 记类型，按 [`BackoffPolicy`] 退避重连（1 s 起、30 s 顶、永不放弃）。
//!    等待可被 [`Wake`] 打断：`retry_now` 缩短这一次等待（agora-7ku.6 "点开 stale 会话立即重试"：
//!    `api::forward::hop` 与 `api::sessions::get` 看到 Human 碰 stale peer 的会话就调一次），
//!    `reconnect` 在线时也强制断开重连。
//! 4. 配置错误（`TransportError::Config` → [`PeerError::Misconfigured`]，ADR-003 D3；agora-41e）
//!    **不进退避**：行照样标 stale，但 `retrying: false`，等一个固定的
//!    `BackoffPolicy::misconfigured_recheck`（生产 10 s）再走一遍 `connected`——`HttpsTransport`
//!    的三项配置检查在拨号之前，文件没修好就不上网；修好了自然进正常流程、`seen` 清错误。
//!    `retry_now` / `reconnect` 同样能打断这个等待。转入配置错误时 warn 一条（正文是
//!    `TokenFileError` 的原话，含 chmod 600 提示），之后每次重读仍失败只 debug，不刷屏。
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
use super::transport::{PeerTransport, TransportError, WsMessage};
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
    Failed(Failure),
}

/// 一次失败：给状态模型的类型，加上**只有日志需要**的细节。`PeerError` 是 `/api/health` 的枚举，
/// 故意不带数据（`Copy` + 序列化成 snake_case 字符串），所以 `TransportError::FingerprintMismatch
/// { expected, actual }` 经 `From` 映射后两个指纹就没了——2026-09-07 实机代检（agora-7ku.10.1 ④）
/// 改错 cert_fingerprint 后 daemon.log 只有 `error=FingerprintMismatch`，人得另外 openssl 才知道
/// 对端真实指纹（agora-btf）。于是 [`PeerClient::classify`] 在映射之前把它们留在这里，跟着
/// `Disconnect::Failed` 走到 `run` 的那条「断开，退避后重试」WARN 打成 `expected=` / `actual=`。
/// 指纹是公开的（`agora node fingerprint` 就是给人抄的），不是秘密，进日志没有顾虑。
struct Failure {
    kind: PeerError,
    /// 只有指纹不匹配带：配置里写的（expected）与对端证书真实的（actual）。
    fingerprints: Option<Fingerprints>,
}

struct Fingerprints {
    expected: String,
    actual: String,
}

impl From<PeerError> for Failure {
    fn from(kind: PeerError) -> Self {
        Failure {
            kind,
            fingerprints: None,
        }
    }
}

/// 传输错误 → 状态类型（`PeerError: From<TransportError>`，state.rs）+ 指纹不匹配时把两个指纹
/// 留下。配置错误的分档日志不在这里，在 [`PeerClient::classify`]（要看前一个状态）。
impl From<TransportError> for Failure {
    fn from(err: TransportError) -> Self {
        let fingerprints = match &err {
            TransportError::FingerprintMismatch { expected, actual } => Some(Fingerprints {
                expected: expected.clone(),
                actual: actual.clone(),
            }),
            _ => None,
        };
        Failure {
            kind: err.into(),
            fingerprints,
        }
    }
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
                Disconnect::Failed(failure) => {
                    let err = failure.kind;
                    self.peers.failed(&self.name, err);
                    // "上次见到"写到每条 stale 行上：就是 PeerState.last_seen（/api/health peers 段
                    // 给 Header 的那个值），一只表、一个数字，侧栏行与 Header 不会各说各话。
                    let last_seen = self.peers.get(&self.name).and_then(|p| p.last_seen);
                    self.publish(self.views.mark_stale(&self.name, last_seen));
                    let delay = if err == PeerError::Misconfigured {
                        // 配置错误不进退避（ADR-003 D3）：固定间隔重读配置文件，不翻倍、不抖、
                        // 不累计 failures。计数归零是有意的：配置错误不是网络失败，文件修好后若
                        // 真的连不上，退避该从 1 s 起步，而不是接着配置错误期间攒下的次数直接
                        // 跳到 30 s 顶（2026-09-06；反例：不归零时 chmod 600 之后 peer 恰好在
                        // 重启，人要多等半分钟才看到它回来）。转入时的 warn 在 `classify` 里。
                        backoff.reset();
                        let delay = self.policy.misconfigured_recheck;
                        tracing::debug!(
                            component = "peer",
                            peer = %self.name,
                            recheck_in_ms = delay.as_millis() as u64,
                            "peer 配置错误，稍后重读配置"
                        );
                        delay
                    } else {
                        let delay = backoff.next_delay(random_jitter());
                        warn_disconnected(&self.name, &failure, backoff.failures(), delay);
                        delay
                    };
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
                return Disconnect::Failed(PeerError::IncompatibleVersion.into());
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
            Err(err) => return Disconnect::Failed(self.classify(err)),
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
                        return Disconnect::Failed(PeerError::Unreachable.into());
                    }
                    Some(Ok(_)) => last_rx = Instant::now(),
                    Some(Err(err)) => {
                        tracing::debug!(component = "peer", peer = %self.name, %err, "事件流出错");
                        return Disconnect::Failed(PeerError::Unreachable.into());
                    }
                },
                _ = tick.tick() => {
                    if last_rx.elapsed() > DEAD_AFTER {
                        tracing::warn!(component = "peer", peer = %self.name, "事件流 65 s 没有入站帧，当作断开");
                        return Disconnect::Failed(PeerError::Unreachable.into());
                    }
                    if ws.send(WsMessage::Text("{\"type\":\"ping\"}".into())).await.is_err() {
                        return Disconnect::Failed(PeerError::Unreachable.into());
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
    async fn handle_frame(&self, text: &str) -> Result<(), Failure> {
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
    async fn snapshot(&self) -> Result<(), Failure> {
        let body = self.get_json("/api/sessions").await?;
        let Some(rows) = body.get("sessions").and_then(Value::as_array) else {
            tracing::warn!(component = "peer", peer = %self.name, "GET /api/sessions 的响应不是 {{ sessions: [...] }}");
            return Err(PeerError::Unreachable.into());
        };
        self.publish(self.views.replace(&self.name, rows.clone()));
        self.peers.seen(&self.name, self.views.now());
        Ok(())
    }

    /// 传输错误 → 状态类型（`From<TransportError>`），顺手打配置错误的日志：**转入**「配置错误」
    /// （前一个状态不是它）warn 一条，正文是 `TransportError` 的 Display——token_file 那类就是
    /// `TokenFileError` 的原话，含 `chmod 600 <path>`；仍在配置错误里的每次重读只 debug，10 s 一条
    /// 也不该刷屏。要在 `PeerStates::failed` 之前调，那时 `last_error` 还是前一个状态。
    /// 指纹不匹配不在这里打日志：两个指纹留在 [`Failure`] 里（`From<TransportError>`），由 `run`
    /// 的断开 WARN 一并打出。
    fn classify(&self, err: TransportError) -> Failure {
        if let TransportError::Config(detail) = &err {
            let was = self.peers.get(&self.name).and_then(|p| p.last_error);
            if was == Some(PeerError::Misconfigured) {
                tracing::debug!(component = "peer", peer = %self.name, %detail, "peer 配置仍然错误");
            } else {
                tracing::warn!(
                    component = "peer",
                    peer = %self.name,
                    %detail,
                    recheck_secs = self.policy.misconfigured_recheck.as_secs_f64(),
                    "peer 配置错误：不重试，改好文件后按固定间隔重读即恢复，不需要重启"
                );
            }
        }
        err.into()
    }

    /// 发一个 GET：传输错误按类型映射（`classify`），401 是未授权，其它非 2xx 当
    /// 不可达；200 但不是 JSON 给 `Null`，让调用方按自己的语义判（`/api/system` 不是 JSON 就是
    /// "不是本协议的节点" → 不兼容；`/api/sessions` 不是 JSON → 不可达，保留旧视图）。
    async fn get_json(&self, path: &str) -> Result<Value, Failure> {
        let req = Request::get(path)
            .body(Body::empty())
            .map_err(|_| Failure::from(PeerError::Unreachable))?;
        let resp = self
            .transport
            .request(req)
            .await
            .map_err(|e| self.classify(e))?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(PeerError::Unauthorized.into());
        }
        if !status.is_success() {
            tracing::warn!(component = "peer", peer = %self.name, path, %status, "peer 返回非 2xx");
            return Err(PeerError::Unreachable.into());
        }
        let bytes = axum::body::to_bytes(resp.into_body(), BODY_LIMIT)
            .await
            .map_err(|_| Failure::from(PeerError::Unreachable))?;
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn publish(&self, events: Vec<Event>) {
        for e in events {
            self.events.publish(e);
        }
    }
}

/// 断开进退避时的那条 WARN。指纹不匹配时多两个字段 `expected=`（配置里写的）/ `actual=`（对端证书
/// 真实的），其它类型这两个字段不出现（`Option` 为 `None` 时 tracing 不记该字段）——人排障时
/// `grep actual= daemon.log` 就能拿到该抄进 `cert_fingerprint` 的值，不必另外 openssl（agora-btf）。
/// 抽成自由函数是为了让单测能在自己的 subscriber 下捕获这一行。
fn warn_disconnected(peer: &str, failure: &Failure, failures: u32, retry_in: Duration) {
    tracing::warn!(
        component = "peer",
        peer,
        error = ?failure.kind,
        expected = failure.fingerprints.as_ref().map(|f| f.expected.as_str()),
        actual = failure.fingerprints.as_ref().map(|f| f.actual.as_str()),
        failures,
        retry_in_ms = retry_in.as_millis() as u64,
        "peer 断开，退避后重试"
    );
}

/// `main.rs`：为 `state.registry` 里的每个 peer 起一个客户端任务。
pub fn spawn_all(state: &AppState) -> Vec<tokio::task::JoinHandle<()>> {
    state
        .registry
        .peers()
        .map(|t| tokio::spawn(PeerClient::new(t.clone(), state).run()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    /// 把一个 tracing fmt subscriber 的输出攒进内存，跑完 `f` 后整段交回。只装在当前线程
    /// （`set_default`），不影响别的测试。
    fn capture_logs(f: impl FnOnce()) -> String {
        let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Sink(sink.clone()))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        f();
        let bytes = buf.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    /// 守卫（agora-btf）：指纹不匹配的断开 WARN 必须把配置里的与对端真实的两个指纹都打出来，
    /// 人不用另外 openssl。改坏法：`warn_disconnected` 去掉 `expected` / `actual` 两个字段，或
    /// `From<TransportError> for Failure` 不再填 `fingerprints`——两种都红。
    #[test]
    fn fingerprint_mismatch_warn_carries_both_fingerprints() {
        let expected = format!("sha256:{}", "ab".repeat(32));
        let actual = format!("sha256:{}", "cd".repeat(32));
        // 不经 PeerClient（要一整个 AppState）：`classify` 除了配置错误的日志就只是这一个 From。
        let failure = Failure::from(TransportError::FingerprintMismatch {
            expected: expected.clone(),
            actual: actual.clone(),
        });
        assert_eq!(failure.kind, PeerError::FingerprintMismatch);
        let out =
            capture_logs(|| warn_disconnected("zuan", &failure, 3, Duration::from_millis(4000)));
        assert!(out.contains("WARN"), "{out}");
        assert!(out.contains("peer 断开，退避后重试"), "{out}");
        assert!(out.contains("peer=\"zuan\""), "{out}");
        assert!(out.contains("error=FingerprintMismatch"), "{out}");
        assert!(
            out.contains(&format!("expected=\"{expected}\"")),
            "缺 expected：{out}"
        );
        assert!(
            out.contains(&format!("actual=\"{actual}\"")),
            "缺 actual：{out}"
        );
        assert!(out.contains("failures=3"), "{out}");
        assert!(out.contains("retry_in_ms=4000"), "{out}");

        // 其它类型的断开：两个字段根本不出现，不是打成 expected=None 之类的噪音。
        let plain = Failure::from(PeerError::Unreachable);
        let out = capture_logs(|| warn_disconnected("zuan", &plain, 1, Duration::from_secs(1)));
        assert!(out.contains("error=Unreachable"), "{out}");
        assert!(!out.contains("expected"), "{out}");
        assert!(!out.contains("actual"), "{out}");
    }
}
