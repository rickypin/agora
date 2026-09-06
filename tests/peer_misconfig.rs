//! peer 「配置错误」的用户可见行为（agora-41e；ADR-003 D3；MISSION §3.5；不变量 8）。
//!
//! ADR-003 D3：持有方的 `peers[].token_file` 权限过宽 / 不属于自己 / 读不到 / 内容不合形态（以及
//! url 不是 https、指纹不合法）时"该 peer 显示为「配置错误」而非离线"。状态模型是
//! `PeerError::Misconfigured`，与其它四类的区别是**不进退避**：`retrying: false`（重试改不了文件），
//! 客户端按 `BackoffPolicy::misconfigured_recheck` 的固定间隔重读配置（生产 10 s），改好即恢复，
//! 不需要重启。五个守卫全部用进程内 fake（`InProcessTransport::set_misconfigured`）驱动真的客户端
//! 循环，策略换成毫秒级——不 sleep 等真实间隔，用 `wait_for` / 事件总线等事实：
//!
//! - `misconfigured_peer_is_flagged_and_not_retrying`：在线 → 配置错 → health 里 `misconfigured`、
//!   `retrying: false`、`last_seen` 保留且等于每条 stale 行的 `last_seen`。
//! - `misconfigured_peer_does_not_back_off`：重读间隔 100 ms 观察 400 ms——尝试次数 ≈ 4，不是
//!   1 ms 退避会给的 ≥ 20；每次采样 `retrying` 都是 false。
//! - `fixing_the_config_recovers_without_restart`：修好文件 → 2 × 间隔内在线、错误清掉、行不再 stale、
//!   事件流有对应 `session_updated`。
//! - `retry_now_wakes_a_misconfigured_peer`：间隔 60 s 时人点开 stale 会话 → 立刻重读（反例先做）。
//! - `local_sessions_unaffected_by_misconfigured_peer`：不变量 8 的又一种坏 peer——本机一切照常。
//!
//! 真文件（0644 / 内容不合形态）经 `HttpsTransport` 的那一测在 tests/peer_token.rs。
//! Node / peer / json / wait_for / collect_until 从 tests/peer_stale.rs 复制（本批 tests/common/
//! 谁都不改，合并进 common 留给 agora-7ku.9）。

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::api::{self, AppState};
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::events::Event;
use agora::peer::backoff::BackoffPolicy;
use agora::peer::client::PeerClient;
use agora::peer::registry::PeerRegistry;
use agora::peer::state::PeerError;
use agora::peer::transport::{InProcessTransport, PeerTransport, DEFAULT_TIMEOUT};
use agora::peer::view::Wake;
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// 测试用的退避：5 ms 起、40 ms 顶；配置错误的重读间隔各测自己给。
const FAST: BackoffPolicy = BackoffPolicy::new(Duration::from_millis(5), Duration::from_millis(40));
/// 等一个条件成立的上限；正常几十毫秒就到，10 s 是 CI 慢机的余量。
const WAIT: Duration = Duration::from_secs(10);

/// 一个节点实例：内存库 + 假运行时 + 自己的 node.id。
struct Node {
    name: &'static str,
    state: AppState,
    auth: Arc<Auth>,
}

impl Node {
    fn new(name: &'static str) -> Self {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let auth = Arc::new(Auth::new(db.clone(), AuthConfig::default()));
        let rt = Arc::new(common::FakeRuntime::default());
        let sessions = Arc::new(SessionManager::new(db, rt as Arc<dyn Runtime>));
        let state = AppState::new(auth.clone(), sessions, name);
        Node { name, state, auth }
    }

    fn router(&self) -> Router {
        api::router(self.state.clone())
    }

    /// 本机浏览器的凭据：经"socket"铸造并兑换的 session cookie。
    fn cookie(&self) -> String {
        let token = self.auth.mint_pair_token(PairedVia::Socket).unwrap();
        let (_, plain) = self.auth.redeem(&token, None, None).unwrap();
        format!("agora_session={plain}")
    }

    fn human_get(&self, path: &str) -> Request {
        Request::get(path)
            .header(header::HOST, common::HOST)
            .header(header::COOKIE, self.cookie())
            .body(Body::empty())
            .unwrap()
    }

    /// 以人的身份发一个带 body 的写请求（同源头齐全），返回 `(status, body)`。
    async fn human_send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, common::HOST)
            .header(header::ORIGIN, format!("http://{}", common::HOST))
            .header(header::COOKIE, self.cookie());
        let body = match body {
            Some(v) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self
            .router()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, body)
    }

    /// 在本节点上以人的身份起一个会话（假运行时，不跑进程）。
    async fn create_session(&self, display_name: &str) -> Value {
        let body = json!({
            "display_name": display_name,
            "agent_type": "shell",
            "working_directory": "/tmp",
            "command": "sleep 300",
        });
        let (status, created) = self
            .human_send(Method::POST, "/api/sessions", Some(body))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created
    }

    /// 人看到的 `GET /api/sessions`。
    async fn list_as_human(&self) -> Value {
        let resp = self
            .router()
            .oneshot(self.human_get("/api/sessions"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        json(resp).await
    }

    /// 人看到的 `GET /api/sessions/:id`：`(status, body)`。
    async fn get_as_human(&self, gid: &str) -> (StatusCode, Value) {
        let resp = self
            .router()
            .oneshot(self.human_get(&format!("/api/sessions/{gid}")))
            .await
            .unwrap();
        let status = resp.status();
        (status, json(resp).await)
    }

    async fn health(&self) -> Value {
        let resp = self
            .router()
            .oneshot(self.human_get("/api/health"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        json(resp).await
    }

    /// 把 `target` 配成本节点的 peer（一行 `peers[]` 配置的等价物）；返回 fake transport 好拨
    /// `set_misconfigured`。保留已配的其它 peer。
    fn link(&mut self, target: &Node) -> Arc<InProcessTransport> {
        let t = Arc::new(peer(target, self));
        let mut reg = PeerRegistry::new(self.name);
        for existing in self.state.registry.peers() {
            reg.insert(existing.clone()).unwrap();
        }
        reg.insert(t.clone()).unwrap();
        self.state.registry = Arc::new(reg);
        t
    }

    /// 起该 peer 的客户端任务（就是 main.rs 为每个 peer spawn 的那个），退避策略由测试给。
    fn start_client_with(
        &self,
        transport: Arc<dyn PeerTransport>,
        policy: BackoffPolicy,
    ) -> Arc<Wake> {
        let client = PeerClient::new(transport, &self.state).with_policy(policy);
        let wake = client.wake();
        tokio::spawn(client.run());
        wake
    }
}

/// `caller` 眼里指向 `target` 的 fake transport。
fn peer(target: &Node, caller: &Node) -> InProcessTransport {
    InProcessTransport::new(target.name, caller.name, target.router(), DEFAULT_TIMEOUT)
}

async fn json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// 等条件成立（5 ms 一探，上限 WAIT）；超时把 `what` 打出来。
async fn wait_for(what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !pred() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "等待超时（{WAIT:?}）: {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 一直收事件直到 `done` 说够了（上限 WAIT）；顺序无关的多条断言用它。
async fn collect_until(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    what: &str,
    mut done: impl FnMut(&[Event]) -> bool,
) -> Vec<Event> {
    let mut got: Vec<Event> = Vec::new();
    let deadline = Instant::now() + WAIT;
    while !done(&got) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(e)) => got.push(e),
            Ok(Err(err)) => panic!("总线: {err}"),
            Err(_) => panic!("等事件超时: {what}; 收到的: {got:?}"),
        }
    }
    got
}

fn rows_of(body: &Value) -> &Vec<Value> {
    body["sessions"].as_array().unwrap()
}

fn find<'a>(rows: &'a [Value], display_name: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["display_name"] == display_name)
        .unwrap_or_else(|| panic!("没有 {display_name}: {rows:?}"))
}

fn gid(row: &Value) -> &str {
    row["id"].as_str().unwrap()
}

/// 本机行的两个"永远没有"：不变量 8 的另一半是本机会话在视图里与 peer 毫无牵连。
fn assert_local_row(row: &Value) {
    assert!(
        row.get("stale").is_none() && row.get("last_seen").is_none(),
        "本机行不许带 stale / last_seen: {row}"
    );
}

/// 让 `a` 并入 `b`（含 `b` 已有的 `rows` 条会话）并等到在线；返回 transport 与唤醒把手。
async fn merged(
    a: &mut Node,
    b: &Node,
    rows: usize,
    policy: BackoffPolicy,
) -> (Arc<InProcessTransport>, Arc<Wake>) {
    let t = a.link(b);
    let wake = a.start_client_with(t.clone(), policy);
    wait_for("a 并入 b", || {
        a.state.peer_views.rows_of("b").len() == rows
            && a.state.peer_views.is_stale("b") == Some(false)
    })
    .await;
    (t, wake)
}

/// 配置坏掉：transport 从此每次尝试都是 `Config`，并让客户端断开当前连接去重连（生产里是
/// 人改坏了文件 / daemon 带着坏配置重连；fake 里已建好的 WS 不会自己死）。等到状态模型记下
/// `Misconfigured`。
async fn go_misconfigured(a: &Node, t: &InProcessTransport, wake: &Wake) {
    t.set_misconfigured(true);
    wake.reconnect();
    wait_for("b 记为配置错误", || {
        a.state.peers.get("b").map(|p| p.last_error) == Some(Some(PeerError::Misconfigured))
    })
    .await;
}

#[tokio::test]
async fn misconfigured_peer_is_flagged_and_not_retrying() {
    // ADR-003 D3："显示为「配置错误」而非离线"。先在线（seen 过、有行），配置坏掉后 1 s 内
    // /api/health 的 peers.b：online false、last_error misconfigured、retrying false（重试改不了
    // 文件）、last_seen 保留且与每条 stale 行的"上次见到"是同一个值（不变量 8：行不消失）。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b1 = b.create_session("on-b-1").await;
    let on_b2 = b.create_session("on-b-2").await;
    a.create_session("on-a").await;
    let (t, wake) = merged(&mut a, &b, 2, FAST.with_recheck(Duration::from_secs(60))).await;
    let mut rx = a.state.events.subscribe();

    let broke = Instant::now();
    go_misconfigured(&a, &t, &wake).await;
    wait_for("视图标 stale", || {
        a.state.peer_views.is_stale("b") == Some(true)
    })
    .await;
    let h = a.health().await;
    let p = &h["peers"]["b"];
    assert!(
        broke.elapsed() < Duration::from_secs(1),
        "{:?}",
        broke.elapsed()
    );
    assert_eq!(p["online"], false, "{h}");
    assert_eq!(p["last_error"], "misconfigured", "{h}");
    assert_eq!(p["retrying"], false, "配置错误不进退避: {h}");
    let seen = p["last_seen"]
        .as_str()
        .expect("last_seen 保留（不变量 8）")
        .to_owned();
    assert!(
        seen.ends_with('Z') && seen.len() == 20,
        "ISO 8601 UTC: {seen}"
    );

    // 行一条不少、每条 stale 且"上次见到"= health 的 last_seen。
    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    for name in ["on-b-1", "on-b-2"] {
        let row = find(rows, name);
        assert_eq!(row["stale"], true, "{row}");
        assert_eq!(row["last_seen"], seen, "{row}");
        assert_eq!(row["node"], "b");
    }
    assert_local_row(find(rows, "on-a"));
    let (status, row) = a.get_as_human(gid(&on_b2)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(row["stale"], true);
    assert_eq!(row["last_seen"], seen);
    // 浏览器从事件流得知：每行一条 session_updated，带 stale 与 last_seen。
    collect_until(&mut rx, "两行的 stale session_updated", |got| {
        [gid(&on_b1), gid(&on_b2)].iter().all(|want| {
            got.iter().any(|e| matches!(e, Event::SessionUpdated { id, session } if id == want && session["stale"] == true && session["last_seen"] == seen))
        })
    })
    .await;
    // 状态模型本体：与 health 一致。
    let p = a.state.peers.get("b").unwrap();
    assert!(!p.online && !p.retrying, "{p:?}");
    assert_eq!(p.last_error, Some(PeerError::Misconfigured));
    assert_eq!(agora::clock::format_utc_secs(p.last_seen.unwrap()), seen);
}

#[tokio::test]
async fn misconfigured_peer_does_not_back_off() {
    // 配置错误不是网络失败：不进指数退避，按固定间隔重读配置。策略给 1 ms 起 4 ms 顶（同
    // tests/peer_stale.rs::reconnect_backoff_is_capped_and_never_gives_up——它在 300 ms 里试 ≥ 20 次）、
    // 重读间隔 100 ms；观察 400 ms：尝试次数 ≈ 4（≤ 6），每次采样 retrying 都是 false。
    const TIGHT: BackoffPolicy =
        BackoffPolicy::new(Duration::from_millis(1), Duration::from_millis(4))
            .with_recheck(Duration::from_millis(100));
    let mut a = Node::new("a");
    let b = Node::new("b");
    b.create_session("on-b").await;
    let (t, wake) = merged(&mut a, &b, 1, TIGHT).await;
    go_misconfigured(&a, &t, &wake).await;

    let base = t.attempts();
    let window = Duration::from_millis(400);
    let deadline = Instant::now() + window;
    let mut samples = 0;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let p = a.state.peers.get("b").unwrap();
        assert!(
            !p.retrying,
            "第 {samples} 次采样：配置错误不许 retrying: {p:?}"
        );
        assert!(!p.online, "{p:?}");
        assert_eq!(p.last_error, Some(PeerError::Misconfigured));
        assert_eq!(a.state.peer_views.is_stale("b"), Some(true));
        assert_eq!(a.state.peer_views.rows_of("b").len(), 1, "stale 行一条不少");
        samples += 1;
    }
    let made = t.attempts() - base;
    // 100 ms 一次 → 400 ms 里约 4 次；6 给调度抖动留余量，又远低于 1 ms 退避会给的 ≥ 20。
    assert!(made <= 6, "400 ms 里试了 {made} 次：配置错误进了退避");
    assert!(made >= 2, "400 ms 里只试了 {made} 次：没有按间隔重读配置");
    assert!(samples >= 10, "采样 {samples} 次");
}

#[tokio::test]
async fn fixing_the_config_recovers_without_restart() {
    // "改文件后 reload 才恢复"的落地形态：daemon 按固定间隔重读配置（生产 10 s），文件修好后
    // 2 × 间隔内在线、last_error 清掉、行不再 stale（last_seen 键消失）、事件流有对应 session_updated。
    const RECHECK: Duration = Duration::from_millis(250);
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    let (t, wake) = merged(&mut a, &b, 1, FAST.with_recheck(RECHECK)).await;
    go_misconfigured(&a, &t, &wake).await;
    wait_for("视图标 stale", || {
        a.state.peer_views.is_stale("b") == Some(true)
    })
    .await;
    let mut rx = a.state.events.subscribe();

    let fixed = Instant::now();
    t.set_misconfigured(false);
    wait_for("修好后自愈", || {
        a.state.peers.get("b").is_some_and(|p| p.online)
            && a.state.peer_views.is_stale("b") == Some(false)
    })
    .await;
    let took = fixed.elapsed();
    assert!(
        took < RECHECK * 2,
        "修好到恢复用了 {took:?}，超过两个重读间隔 {RECHECK:?}"
    );
    let h = a.health().await;
    let p = &h["peers"]["b"];
    assert_eq!(p["online"], true, "{h}");
    assert_eq!(p["last_error"], Value::Null, "{h}");
    assert_eq!(p["retrying"], false, "{h}");
    assert!(p["last_seen"].is_string());
    let body = a.list_as_human().await;
    let row = find(rows_of(&body), "on-b");
    assert_eq!(row["stale"], false, "{row}");
    assert!(
        row.get("last_seen").is_none(),
        "恢复后 last_seen 键消失: {row}"
    );
    collect_until(&mut rx, "恢复的 session_updated", |got| {
        got.iter().any(|e| matches!(e, Event::SessionUpdated { id, session } if id == gid(&on_b) && session["stale"] == false && session.get("last_seen").is_none()))
    })
    .await;
    // 一个进程内从头到尾：没有重启 daemon、没有重建客户端。
    assert!(!t.is_misconfigured());
}

#[tokio::test]
async fn retry_now_wakes_a_misconfigured_peer() {
    // 重读间隔故意给 60 s：关掉开关后**不**等——反例先做，300 ms 后仍是配置错误、一次新尝试都
    // 没有；再以 Human 身份 GET /api/sessions/b:<id>（sessions::get 对 stale peer 调 retry_now，
    // agora-7ku.6）→ 2 s 内在线。只有 retry_now 能解释这次恢复。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    let (t, wake) = merged(&mut a, &b, 1, FAST.with_recheck(Duration::from_secs(60))).await;
    go_misconfigured(&a, &t, &wake).await;
    wait_for("视图标 stale", || {
        a.state.peer_views.is_stale("b") == Some(true)
    })
    .await;
    let at_broken = t.attempts();

    t.set_misconfigured(false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let p = a.state.peers.get("b").unwrap();
    assert_eq!(
        p.last_error,
        Some(PeerError::Misconfigured),
        "不点开就等着下一次重读: {p:?}"
    );
    assert!(!p.online && !p.retrying, "{p:?}");
    assert_eq!(t.attempts(), at_broken, "重读间隔内没有新尝试");

    let clicked = Instant::now();
    let (status, row) = a.get_as_human(gid(&on_b)).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["stale"], true, "读给的是最后一眼: {row}");
    wait_for("点开后立刻重读配置并恢复", || {
        a.state.peer_views.is_stale("b") == Some(false)
    })
    .await;
    let took = clicked.elapsed();
    assert!(took < Duration::from_secs(2), "点开到恢复用了 {took:?}");
    assert!(t.attempts() > at_broken, "恢复来自一次新的连接尝试");
    let p = a.state.peers.get("b").unwrap();
    assert!(p.online && !p.retrying && p.last_error.is_none(), "{p:?}");
}

/// A 本机的一整套：create → list → get → input → kill，每一步都断言本机行没有 stale / last_seen，
/// 且 peer 的 stale 行一直并列在旁。
async fn local_flow(a: &Node, name: &str) {
    let created = a.create_session(name).await;
    assert_local_row(&created);
    assert_eq!(created["node"], "a");
    let id = gid(&created).to_owned();
    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    assert_local_row(find(rows, name));
    assert_eq!(find(rows, "on-b")["stale"], true, "{body}");
    let (status, row) = a.get_as_human(&id).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_local_row(&row);
    let (status, out) = a
        .human_send(
            Method::POST,
            &format!("/api/sessions/{id}/input"),
            Some(json!({ "kind": "text", "data": "ls\n" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (status, out) = a
        .human_send(
            Method::POST,
            &format!("/api/sessions/{id}/kill"),
            Some(json!({ "confirmed": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_local_row(&out);
    assert_eq!(out["alive"], false, "kill 真的落到了本机运行时: {out}");
}

#[tokio::test]
async fn local_sessions_unaffected_by_misconfigured_peer() {
    // 不变量 8（A36）：一个配置错了的 peer 不影响本机——create / list / get / input / kill 全部正常、
    // 本机行永远没有 stale / last_seen、整个流程 < 2 s（配置检查在拨号之前，本机路径碰都不碰它）。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    let (t, wake) = merged(&mut a, &b, 1, FAST.with_recheck(Duration::from_millis(50))).await;
    go_misconfigured(&a, &t, &wake).await;
    wait_for("视图标 stale", || {
        a.state.peer_views.is_stale("b") == Some(true)
    })
    .await;

    let started = Instant::now();
    local_flow(&a, "local-while-misconfigured").await;
    local_flow(&a, "local-while-misconfigured-2").await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "本机流程被配置错误的 peer 拖慢了: {elapsed:?}"
    );
    // 期间 peer 一直是配置错误、不 retrying，行还在。
    let p = a.state.peers.get("b").unwrap();
    assert_eq!(p.last_error, Some(PeerError::Misconfigured));
    assert!(!p.online && !p.retrying, "{p:?}");
    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    assert_eq!(find(rows, "on-b")["id"], on_b["id"]);
    assert_eq!(find(rows, "on-b")["stale"], true);
    assert_local_row(find(rows, "local-while-misconfigured"));
    assert_local_row(find(rows, "local-while-misconfigured-2"));
}
