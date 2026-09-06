//! peer 离线的用户可见行为与不变量 8 的 fake 集成测试（agora-7ku.6；A29；A36；MISSION §3.5；ADR-004）。
//!
//! 五个守卫，全部用 agora-7ku.11 的进程内 fake（`InProcessTransport`）驱动真的客户端循环
//! （`PeerClient::run`：比版本 → 建流 → 拉全量 → 收事件 → 断线退避重连），只把退避策略换成毫秒级
//! ——不 sleep 等真实退避，用 `wait_for` / 事件总线等事实：
//!
//! - `offline_peer_is_stale_with_last_seen_not_removed`：断线 → 行一条不少、每行 `stale: true` 且
//!   `last_seen` 与 `/api/health` peers 段同一个值（同一只本机表）。
//! - `local_sessions_unaffected_by_broken_peer`：**不变量 8 的 fake 版**（A36）——peer 连不上 / 活着但
//!   坏了，本机的 create / list / get / input / kill 照常且本机行永远没有 stale / last_seen。
//! - `reconnect_backoff_is_capped_and_never_gives_up`：agora-7ku.12 纯函数单测的集成版——退避封顶、
//!   一直 `retrying`、恢复后自愈。
//! - `opening_stale_peer_triggers_immediate_retry`：退避很长时人点开 stale 会话 → 立刻重试；不点开就
//!   一直 stale（反例先做）。
//! - `recovery_resyncs_full_snapshot`：断线期间 peer 增一删一改一 → 恢复后视图等于 peer 当前列表、
//!   事件流有对应 created / removed / updated、stale 与 last_seen 都清掉。
//!
//! 给 agora-7ku.9：前两个是不变量 8 的 fake 版，收进 tests/invariants_peer.rs 时换成真 TLS 两实例。
//! Node / peer / json / wait_for 从 tests/peer_view.rs 复制（本批 tests/common/ 谁都
//! 不改，合并进 common 留给 agora-7ku.9）。

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

/// 测试用的退避：5 ms 起、40 ms 顶。生产是 1 s / 30 s（`BackoffPolicy::PEER`），这里只要"会重试"。
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
    /// `set_offline` / `set_broken`。保留已配的其它 peer。
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

/// 一直收事件直到 `done` 说够了（上限 WAIT）；顺序无关的多条断言用它——tests/peer_view.rs 那个
/// 逐条等的 `expect_event` 会把跳过的那条丢掉，而它可能正是下一条要找的。
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

fn names(rows: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = rows
        .iter()
        .map(|r| r["display_name"].as_str().unwrap().to_owned())
        .collect();
    v.sort();
    v
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

/// 掉线：transport 拒连，并让客户端断开当前连接去重连（fake 里已建好的 WS 不会自己死；生产里是
/// TCP 断或 65 s keepalive 超时——都走同一条 Disconnect::Failed 路径）。等到视图标 stale。
async fn go_offline(a: &Node, t: &InProcessTransport, wake: &Wake) {
    t.set_offline(true);
    wake.reconnect();
    wait_for("b 的视图标 stale", || {
        a.state.peer_views.is_stale("b") == Some(true)
    })
    .await;
}

#[tokio::test]
async fn offline_peer_is_stale_with_last_seen_not_removed() {
    // A29 / MISSION §3.5 / 不变量 8：peer 离线 → 它的会话显示 stale（含上次见到时间）而非消失。
    // "上次见到"就是 /api/health peers 段的 last_seen：一只表、一个值，侧栏行与 Header 不会各说各话。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b1 = b.create_session("on-b-1").await;
    let on_b2 = b.create_session("on-b-2").await;
    a.create_session("on-a").await;
    let (t, wake) = merged(&mut a, &b, 2, FAST).await;
    // 在线时没有 last_seen 键。
    for row in a.state.peer_views.rows_of("b") {
        assert_eq!(row["stale"], false);
        assert!(
            row.get("last_seen").is_none(),
            "在线的 peer 行没有 last_seen: {row}"
        );
    }
    let mut rx = a.state.events.subscribe();

    go_offline(&a, &t, &wake).await;

    let h = a.health().await;
    let p = &h["peers"]["b"];
    assert_eq!(p["online"], false, "{h}");
    assert_eq!(p["retrying"], true, "{h}");
    assert_eq!(p["last_error"], "unreachable", "{h}");
    let seen = p["last_seen"]
        .as_str()
        .expect("health 的 last_seen 是 UTC 文本")
        .to_owned();
    assert!(
        seen.ends_with('Z') && seen.len() == 20,
        "ISO 8601 UTC: {seen}"
    );

    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    assert_eq!(
        names(rows),
        ["on-a", "on-b-1", "on-b-2"],
        "行一条不少: {body}"
    );
    for name in ["on-b-1", "on-b-2"] {
        let row = find(rows, name);
        assert_eq!(row["stale"], true, "{row}");
        assert_eq!(
            row["last_seen"], seen,
            "行上的上次见到 = health 的 last_seen: {row}"
        );
        assert_eq!(row["node"], "b");
    }
    assert_local_row(find(rows, "on-a"));
    // 单条读给的是最后一眼，同样带 stale / last_seen。
    let (status, row) = a.get_as_human(gid(&on_b2)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(row["stale"], true);
    assert_eq!(row["last_seen"], seen);
    // 浏览器从事件流得知：每行一条 session_updated，带 stale 与 last_seen，不用重拉。
    let got = collect_until(&mut rx, "两行的 stale session_updated", |got| {
        [gid(&on_b1), gid(&on_b2)].iter().all(|want| {
            got.iter().any(|e| matches!(e, Event::SessionUpdated { id, session } if id == want && session["stale"] == true))
        })
    })
    .await;
    for e in &got {
        if let Event::SessionUpdated { session, .. } = e {
            if session["stale"] == true {
                assert_eq!(session["last_seen"], seen, "{session}");
            }
        }
    }
    // 在线的 peer 状态模型：last_seen 保留（PeerState::failed 不动它）。
    let p = a.state.peers.get("b").unwrap();
    assert_eq!(p.last_error, Some(PeerError::Unreachable));
    assert_eq!(agora::clock::format_utc_secs(p.last_seen.unwrap()), seen);
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
    // peer 的行还在、是 stale——本机行与它并列却互不牵连。
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
async fn local_sessions_unaffected_by_broken_peer() {
    // 不变量 8（A36）的 fake 版：一个坏节点不影响另一个节点。两种坏法——连不上（offline）与
    // 连得上却答非所问（broken：HTTP 500 / WS 502）——期间 A 本机的 create / list / get / input / kill
    // 全部正常、本机行上永远没有 stale / last_seen，整个流程不因 peer 的状况变慢。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    let (t, wake) = merged(&mut a, &b, 1, FAST).await;
    go_offline(&a, &t, &wake).await;

    let started = Instant::now();

    // ① 连不上。
    local_flow(&a, "local-while-offline").await;
    assert_eq!(
        a.state.peer_views.is_stale("b"),
        Some(true),
        "peer 仍 stale"
    );

    // ② 活着但坏了：客户端重试真的碰到了 500，仍是 unreachable、仍 stale。
    t.set_offline(false);
    t.set_broken(true);
    let before = t.attempts();
    wait_for("客户端在 broken 模式下又试了一次", || {
        t.attempts() > before
    })
    .await;
    let p = a.state.peers.get("b").unwrap();
    assert!(!p.online && p.retrying, "{p:?}");
    assert_eq!(p.last_error, Some(PeerError::Unreachable));
    assert_eq!(a.state.peer_views.is_stale("b"), Some(true));
    local_flow(&a, "local-while-broken").await;

    // 响应时间不受 peer 影响：两轮本机流程（各 5 个请求）加起来远小于 2 s——peer 每次尝试是 5 s
    // 超时的话，只要本机路径碰了它一下就会超。
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "本机流程被 peer 拖慢了: {elapsed:?}"
    );

    // peer 的行一直在。
    let body = a.list_as_human().await;
    assert_eq!(find(rows_of(&body), "on-b")["id"], on_b["id"]);
    assert_eq!(find(rows_of(&body), "on-b")["stale"], true);

    // 修好：自愈，不需要人干预。
    t.set_broken(false);
    wait_for("peer 修好后自愈", || {
        a.state.peer_views.is_stale("b") == Some(false)
    })
    .await;
    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    assert_eq!(
        names(rows),
        ["local-while-broken", "local-while-offline", "on-b"],
        "{body}"
    );
    assert_eq!(find(rows, "on-b")["stale"], false);
    assert!(find(rows, "on-b").get("last_seen").is_none());
    assert_local_row(find(rows, "local-while-offline"));
    assert_local_row(find(rows, "local-while-broken"));
}

#[tokio::test]
async fn reconnect_backoff_is_capped_and_never_gives_up() {
    // agora-7ku.12 纯函数单测（src/peer/backoff.rs::never_gives_up）的集成版：退避策略 1 ms 起 4 ms 顶，
    // 离线维持几百毫秒——尝试次数说明等待真的封了顶（不封顶的指数退避 1, 2, 4, …, 128 ms 在 300 ms
    // 里只到得了 9 次），retrying 一直为 true 说明没有"重试 N 次后标 dead"；随后恢复在线自愈。
    const TIGHT: BackoffPolicy =
        BackoffPolicy::new(Duration::from_millis(1), Duration::from_millis(4));
    let mut a = Node::new("a");
    let b = Node::new("b");
    b.create_session("on-b").await;
    let (t, wake) = merged(&mut a, &b, 1, TIGHT).await;
    go_offline(&a, &t, &wake).await;

    let base = t.attempts();
    let window = Duration::from_millis(300);
    let deadline = Instant::now() + window;
    let mut samples = 0;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let p = a.state.peers.get("b").unwrap();
        assert!(p.retrying, "第 {samples} 次采样：退避不许终止: {p:?}");
        assert!(!p.online, "{p:?}");
        assert_eq!(p.last_error, Some(PeerError::Unreachable));
        assert_eq!(a.state.peer_views.is_stale("b"), Some(true));
        assert_eq!(a.state.peer_views.rows_of("b").len(), 1, "stale 行一条不少");
        samples += 1;
    }
    let made = t.attempts() - base;
    // 顶 4 ms（jitter 只往下抖）→ 300 ms 里 ≥ 75 次；20 给 CI 慢机留足余量，又远高于不封顶的 9 次。
    assert!(made >= 20, "300 ms 里只试了 {made} 次：退避没封顶或放弃了");
    assert!(samples >= 10, "采样 {samples} 次");

    // 永不放弃的另一半：恢复在线就连上，不需要人干预。
    t.set_offline(false);
    wait_for("恢复后自愈", || {
        a.state.peer_views.is_stale("b") == Some(false)
            && a.state.peer_views.rows_of("b").len() == 1
    })
    .await;
    let p = a.state.peers.get("b").unwrap();
    assert!(p.online && !p.retrying && p.last_error.is_none(), "{p:?}");
    let h = a.health().await;
    assert_eq!(h["peers"]["b"]["online"], true, "{h}");
}

#[tokio::test]
async fn opening_stale_peer_triggers_immediate_retry() {
    // agora-7ku.6 ②：人点开 stale peer 的会话 → 立刻插一次重试，不等退避。退避策略故意给 60 s
    // （jitter 后 ≥ 30 s）：不点开就一直 stale（反例先做），点开 2 s 内就回到在线——只有 retry_now
    // 能解释这次恢复。两个入口都验：sessions::get（GET /api/sessions/b:<id>）与 forward::hop
    // （写操作 / 终端 WS 的路由闸，这里用 POST input）。
    const SLOW: BackoffPolicy =
        BackoffPolicy::new(Duration::from_secs(60), Duration::from_secs(60));
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    let (t, wake) = merged(&mut a, &b, 1, SLOW).await;

    // —— 入口 1：GET /api/sessions/:id ——
    go_offline(&a, &t, &wake).await;
    let at_stale = t.attempts();
    // 反例：peer 回来了，但客户端在 ≥ 30 s 的退避里；500 ms 后仍 stale、一次新尝试都没有。
    t.set_offline(false);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        a.state.peer_views.is_stale("b"),
        Some(true),
        "不点开就还在等退避"
    );
    assert_eq!(t.attempts(), at_stale, "退避期间没有新尝试");
    // 点开：读到的是最后一眼（stale），但重试已经插进去了。
    let clicked = Instant::now();
    let (status, row) = a.get_as_human(gid(&on_b)).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["stale"], true, "读给的是最后一眼: {row}");
    assert!(row["last_seen"].is_string());
    wait_for("点开后立刻重连", || {
        a.state.peer_views.is_stale("b") == Some(false)
    })
    .await;
    let took = clicked.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "点开到恢复用了 {took:?}，不像是立即重试"
    );
    assert!(t.attempts() > at_stale, "恢复来自一次新的连接尝试");
    let (_, row) = a.get_as_human(gid(&on_b)).await;
    assert_eq!(row["stale"], false);
    assert!(
        row.get("last_seen").is_none(),
        "恢复后 last_seen 键消失: {row}"
    );
    // 在线时点开无事：不多一次尝试（retry_now 不是轮询）。
    let online_attempts = t.attempts();
    let _ = a.get_as_human(gid(&on_b)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(t.attempts(), online_attempts, "在线时点开不该碰 transport");

    // —— 入口 2：forward::hop（写操作 / 终端 WS 都从它过；这里 POST input）——
    go_offline(&a, &t, &wake).await;
    t.set_offline(false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        a.state.peer_views.is_stale("b"),
        Some(true),
        "反例：不点开就还在等退避"
    );
    let clicked = Instant::now();
    let (status, out) = a
        .human_send(
            Method::POST,
            &format!("/api/sessions/{}/input", gid(&on_b)),
            Some(json!({ "kind": "text", "data": "hi\n" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "转发本身照常（peer 已在线）: {out}");
    wait_for("经 hop 点开后立刻重连", || {
        a.state.peer_views.is_stale("b") == Some(false)
    })
    .await;
    let took = clicked.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "经 hop 点开到恢复用了 {took:?}"
    );
    let p = a.state.peers.get("b").unwrap();
    assert!(p.online && !p.retrying && p.last_error.is_none(), "{p:?}");
}

#[tokio::test]
async fn recovery_resyncs_full_snapshot() {
    // agora-7ku.6 ③：断线期间 peer 那边增一删一改一 → 重连拉到全量后视图等于 peer 当前列表，
    // 本机事件流里有对应的 created / removed / updated，stale 与 last_seen 都清掉。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let keep = b.create_session("keep").await;
    let gone = b.create_session("gone").await;
    let renamed = b.create_session("to-rename").await;
    let (t, wake) = merged(&mut a, &b, 3, FAST).await;
    go_offline(&a, &t, &wake).await;

    // 断线期间 B 上的变化，A 一条都看不见。
    let born = b.create_session("born-while-offline").await;
    let (status, _) = b
        .human_send(
            Method::DELETE,
            &format!("/api/sessions/{}", gid(&gone)),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, out) = b
        .human_send(
            Method::PATCH,
            &format!("/api/sessions/{}", gid(&renamed)),
            Some(json!({ "display_name": "renamed" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let stale_rows = a.state.peer_views.rows_of("b");
    assert_eq!(stale_rows.len(), 3, "断线期间视图冻在最后一眼");
    assert!(stale_rows
        .iter()
        .all(|r| r["stale"] == true && r["last_seen"].is_string()));
    assert_eq!(names(&stale_rows), ["gone", "keep", "to-rename"]);

    let mut rx = a.state.events.subscribe();
    t.set_offline(false);
    wait_for("恢复后对齐", || {
        a.state.peer_views.is_stale("b") == Some(false)
            && a.state.peer_views.rows_of("b").len() == 3
    })
    .await;

    // 视图 == peer 当前列表。
    let theirs = names(rows_of(&b.list_as_human().await));
    assert_eq!(theirs, ["born-while-offline", "keep", "renamed"]);
    let body = a.list_as_human().await;
    let ours: Vec<Value> = rows_of(&body)
        .iter()
        .filter(|r| r["node"] == "b")
        .cloned()
        .collect();
    assert_eq!(names(&ours), theirs, "{body}");
    for row in &ours {
        assert_eq!(row["stale"], false, "{row}");
        assert!(
            row.get("last_seen").is_none(),
            "last_seen 随 stale 一起清掉: {row}"
        );
    }
    assert_eq!(find(&ours, "keep")["id"], keep["id"]);
    assert_eq!(find(&ours, "born-while-offline")["id"], born["id"]);
    assert_eq!(find(&ours, "renamed")["id"], renamed["id"]);

    // 事件流：新来的 created、没了的 removed、改了的 updated，以及 keep——内容没变但 stale 翻回
    // false，也有一条 updated（浏览器据此去掉淡显）。顺序无关，一起收。
    let got = collect_until(&mut rx, "对齐的差分事件", |got| {
        got.iter().any(|e| matches!(e, Event::SessionCreated { id, .. } if id == gid(&born)))
            && got.iter().any(|e| matches!(e, Event::SessionRemoved { id } if id == gid(&gone)))
            && got.iter().any(|e| matches!(e, Event::SessionUpdated { id, session } if id == gid(&renamed) && session["display_name"] == "renamed"))
            && got.iter().any(|e| matches!(e, Event::SessionUpdated { id, session } if id == gid(&keep) && session["stale"] == false))
    })
    .await;
    for e in &got {
        match e {
            Event::SessionCreated { session, .. } | Event::SessionUpdated { session, .. } => {
                assert_eq!(
                    session["stale"], false,
                    "对齐后发出的行都不 stale: {session}"
                );
                assert!(session.get("last_seen").is_none(), "{session}");
            }
            _ => {}
        }
    }
    // 没有多余的 removed：keep / renamed / born 都还在。
    assert!(
        !got.iter()
            .any(|e| matches!(e, Event::SessionRemoved { id } if id != gid(&gone))),
        "{got:?}"
    );
}
