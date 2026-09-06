//! peer 客户端与并入视图的守卫（agora-7ku.5；MISSION §3.5；ADR-004；不变量 8）。
//!
//! 全部用 agora-7ku.11 的进程内 fake：几个 daemon 实例在同一个测试进程里，A 经
//! `InProcessTransport` 以 Peer principal 调到 B（MISSION §2.3 规则 9 "单进程多节点"）。
//! 客户端循环整个真跑（比版本 → 建流 → 拉全量 → 收事件 → 断线退避重连），只把退避策略换成
//! 毫秒级、时钟换成可注入的——不 sleep 等真实退避。
//!
//! Node / peer / json 辅助从 tests/peer_fake.rs 复制（本批 tests/common/ 谁都不改，合并进
//! common 留给 agora-7ku.9）。

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agora::api::{self, AppState, API_VERSION};
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::events::Event;
use agora::peer::backoff::BackoffPolicy;
use agora::peer::client::PeerClient;
use agora::peer::registry::PeerRegistry;
use agora::peer::state::PeerError;
use agora::peer::transport::{InProcessTransport, PeerTransport, DEFAULT_TIMEOUT};
use agora::peer::view::{PeerViews, Wake};
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
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

    /// 在本节点上以人的身份起一个会话（假运行时，不跑进程）。
    async fn create_session(&self, display_name: &str) -> Value {
        let body = json!({
            "display_name": display_name,
            "agent_type": "shell",
            "working_directory": "/tmp",
            "command": "sleep 300",
        });
        let resp = self
            .router()
            .oneshot(
                Request::post("/api/sessions")
                    .header(header::HOST, common::HOST)
                    .header(header::ORIGIN, format!("http://{}", common::HOST))
                    .header(header::COOKIE, self.cookie())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        json(resp).await
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
    /// `set_offline`。保留已配的其它 peer。
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

    /// 起该 peer 的客户端任务（就是 main.rs 为每个 peer spawn 的那个），退避换成毫秒级。
    fn start_client(&self, transport: Arc<dyn PeerTransport>) -> Arc<Wake> {
        let client = PeerClient::new(transport, &self.state).with_policy(FAST);
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

/// 从总线上等一条满足条件的事件（其余跳过），上限 WAIT。
async fn expect_event(
    rx: &mut tokio::sync::broadcast::Receiver<Event>,
    what: &str,
    mut pred: impl FnMut(&Event) -> bool,
) -> Event {
    tokio::time::timeout(WAIT, async {
        loop {
            let e = rx.recv().await.expect("总线关闭");
            if pred(&e) {
                return e;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("等事件超时: {what}"))
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

#[tokio::test]
async fn peer_sessions_are_merged_with_node_label() {
    // MISSION §3.5：连上时拉全量，之后收事件流，把 peer 的会话并入自己的视图，每行标明节点；
    // id 用 peer 报的 `<peer>:<local_id>`（§7.3 会话 id 一律 <node>:<id>）。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    a.create_session("on-a").await;
    let t = a.link(&b);
    let mut rx = a.state.events.subscribe();
    a.start_client(t.clone());
    wait_for("a 并入 b 的一行", || {
        a.state.peer_views.rows_of("b").len() == 1
    })
    .await;

    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    assert_eq!(names(rows), ["on-a", "on-b"], "{body}");
    let local = find(rows, "on-a");
    assert_eq!(local["node"], "a");
    assert!(
        local.get("stale").is_none(),
        "本机行没有 stale 字段: {local}"
    );
    let merged = find(rows, "on-b");
    assert_eq!(merged["node"], "b");
    assert_eq!(
        merged["id"], on_b["id"],
        "id 是 peer 报的 <peer>:<local_id>"
    );
    assert!(merged["id"].as_str().unwrap().starts_with("b:"));
    assert_eq!(merged["local_id"], on_b["local_id"]);
    assert_eq!(merged["stale"], false);
    // 全量并入时本机总线上就有 session_created，浏览器不用重拉。
    let e = expect_event(
        &mut rx,
        "on-b 的 session_created",
        |e| matches!(e, Event::SessionCreated { id, .. } if *id == on_b["id"]),
    )
    .await;
    let Event::SessionCreated { session, .. } = e else {
        unreachable!()
    };
    assert_eq!(session["node"], "b");

    // 之后的增量经 B 的 /api/events 到达 A、原样进 A 的总线（浏览器只连 A 这一条流）。
    let on_b2 = b.create_session("on-b-2").await;
    wait_for("事件流带来 on-b-2", || {
        a.state.peer_views.rows_of("b").len() == 2
    })
    .await;
    let e = expect_event(
        &mut rx,
        "on-b-2 的 session_created",
        |e| matches!(e, Event::SessionCreated { id, .. } if *id == on_b2["id"]),
    )
    .await;
    let Event::SessionCreated { session, .. } = e else {
        unreachable!()
    };
    assert_eq!(session["node"], "b");
    assert_eq!(session["display_name"], "on-b-2");
    assert_eq!(session["stale"], false);

    // 单条读也从视图来（不为一次读去转发）。
    let resp = a
        .router()
        .oneshot(a.human_get(&format!("/api/sessions/{}", on_b2["id"].as_str().unwrap())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let row = json(resp).await;
    assert_eq!(row["display_name"], "on-b-2");
    assert_eq!(row["node"], "b");

    // Header 那一枚：在线、见过。
    let h = a.health().await;
    assert_eq!(h["peers"]["b"]["online"], true, "{h}");
    assert!(h["peers"]["b"]["last_seen"].is_string());
    assert_eq!(h["peers"]["b"]["last_error"], Value::Null);
}

#[tokio::test]
async fn only_local_sessions_are_exported_one_hop() {
    // MISSION §3.5 "只导出本机会话，一跳"：A 配 B、B 配 C。B 给人看的有 C 的会话，给 A（peer）
    // 看的只有自己的；A 于是看不到 C 的——这同时防环。
    let mut a = Node::new("a");
    let mut b = Node::new("b");
    let c = Node::new("c");
    let on_c = c.create_session("on-c").await;
    b.create_session("on-b").await;
    a.create_session("on-a").await;

    let tb_c = b.link(&c);
    b.start_client(tb_c);
    wait_for("b 并入 c", || b.state.peer_views.rows_of("c").len() == 1).await;
    assert_eq!(names(rows_of(&b.list_as_human().await)), ["on-b", "on-c"]);

    // B 对 peer principal 只给本机行：直接以 A 的身份拉一次。
    let ta_b = a.link(&b);
    let resp = ta_b
        .request(Request::get("/api/sessions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let exported = json(resp).await;
    assert_eq!(names(rows_of(&exported)), ["on-b"], "{exported}");
    assert!(exported["unregistered"]
        .as_array()
        .unwrap()
        .iter()
        .all(|u| u["node"] == "b"));
    // 单条读同样一跳：peer 来问 B "C 的会话"是 node_unknown，B 不替它转发。
    let resp = ta_b
        .request(
            Request::get(format!("/api/sessions/{}", on_c["id"].as_str().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(resp).await["error"], "node_unknown");

    // A 跑起客户端：视图里只有 B 的行，永远没有 C 的。
    a.start_client(ta_b.clone());
    wait_for("a 并入 b", || a.state.peer_views.rows_of("b").len() == 1).await;
    let body = a.list_as_human().await;
    assert_eq!(names(rows_of(&body)), ["on-a", "on-b"], "{body}");
    assert!(rows_of(&body).iter().all(|r| r["node"] != "c"));
    assert!(a.state.peer_views.rows_of("c").is_empty());
    // A 上问 C 的会话：C 不是 A 的 peer → node_unknown。
    let resp = a
        .router()
        .oneshot(a.human_get(&format!("/api/sessions/{}", on_c["id"].as_str().unwrap())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(resp).await["error"], "node_unknown");
}

/// 计 `/api/sessions` 被敲了几次：不兼容时应该是 0。
async fn count_sessions(
    State(hits): State<Arc<AtomicUsize>>,
    req: Request,
    next: Next,
) -> Response {
    if req.uri().path() == "/api/sessions" {
        hits.fetch_add(1, Ordering::SeqCst);
    }
    next.run(req).await
}

#[tokio::test]
async fn incompatible_peer_is_flagged_not_merged() {
    // MISSION §7.3 / A33：连 peer 的第一步是比 api_version；major 不同 → 标 incompatible_version、
    // 不拉 /api/sessions、不并入；按退避照常重试，对方升级后自愈（api.md「调用方的义务」）。
    let mut a = Node::new("a");
    let b = Node::new("b");
    b.create_session("on-b").await;
    a.create_session("on-a").await;

    let upgraded = Arc::new(AtomicBool::new(false));
    let system_hits = Arc::new(AtomicUsize::new(0));
    let session_hits = Arc::new(AtomicUsize::new(0));
    // B 的 /api/system 先报 major+1（"下一代"），其余端点是 B 真的 Router（计 /api/sessions 的次数）。
    let fake_system = {
        let upgraded = upgraded.clone();
        let hits = system_hits.clone();
        move || {
            let upgraded = upgraded.clone();
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let v = if upgraded.load(Ordering::SeqCst) {
                    json!({ "major": API_VERSION.major, "minor": API_VERSION.minor })
                } else {
                    json!({ "major": API_VERSION.major + 1, "minor": 0 })
                };
                Json(json!({ "api_version": v, "version": "fake", "node": "b" }))
            }
        }
    };
    let counted = b.router().layer(middleware::from_fn_with_state(
        session_hits.clone(),
        count_sessions,
    ));
    let app = Router::new()
        .route("/api/system", get(fake_system))
        .fallback_service(counted);
    let t = Arc::new(InProcessTransport::new("b", "a", app, DEFAULT_TIMEOUT));
    let mut reg = PeerRegistry::new("a");
    reg.insert(t.clone()).unwrap();
    a.state.registry = Arc::new(reg);
    a.start_client(t.clone());

    // 试了好几轮（在重试），每轮都停在版本这一步。
    wait_for("a 反复比版本", || {
        system_hits.load(Ordering::SeqCst) >= 3
    })
    .await;
    let p = a.state.peers.get("b").unwrap();
    assert_eq!(p.last_error, Some(PeerError::IncompatibleVersion), "{p:?}");
    assert!(!p.online && p.retrying, "{p:?}");
    assert_eq!(p.last_seen, None, "从没成功交互过");
    assert_eq!(
        session_hits.load(Ordering::SeqCst),
        0,
        "不兼容就不该去拉 /api/sessions"
    );
    assert!(a.state.peer_views.rows().is_empty());
    let body = a.list_as_human().await;
    assert_eq!(names(rows_of(&body)), ["on-a"], "{body}");
    let h = a.health().await;
    assert_eq!(h["peers"]["b"]["last_error"], "incompatible_version", "{h}");
    assert_eq!(h["peers"]["b"]["online"], false);
    assert_eq!(h["peers"]["b"]["retrying"], true);

    // B "升级"到同 major：下一次重试就并入，不需要人干预。
    upgraded.store(true, Ordering::SeqCst);
    wait_for("升级后自愈并入", || {
        a.state.peer_views.rows_of("b").len() == 1
    })
    .await;
    let p = a.state.peers.get("b").unwrap();
    assert!(p.online && p.last_error.is_none() && !p.retrying, "{p:?}");
    assert!(p.last_seen.is_some());
    assert_eq!(names(rows_of(&a.list_as_human().await)), ["on-a", "on-b"]);
    assert!(session_hits.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn peer_timestamps_use_local_clock() {
    // MISSION §3.5 "peer 视图里的时间（上次见到、等待时长）由本节点的时钟打，不信 peer 报的时间"；
    // ADR-004 守卫句。A 的时钟钉在 2023 年（peer 的真实时钟不可能报出它），B 报的是真实当下：
    // A 视图里 B 的行 status_since（等待时长的起点）与 health 的 last_seen（上次见到）都是 2023 年。
    const T_LOCAL: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z
    let mut a = Node::new("a");
    a.state.peer_views = PeerViews::with_clock(Arc::new(|| T_LOCAL));
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    let peer_since = on_b["status_since"].as_i64().unwrap();
    let real_now = agora::clock::now_secs();
    assert!(
        (real_now - peer_since).abs() < 600,
        "B 报的是它自己的当下: {peer_since} vs {real_now}"
    );

    let t = a.link(&b);
    a.start_client(t);
    wait_for("a 并入 b", || a.state.peer_views.rows_of("b").len() == 1).await;
    let body = a.list_as_human().await;
    let row = find(rows_of(&body), "on-b");
    assert_eq!(row["status_since"], T_LOCAL, "等待时长按本机时钟打: {row}");
    assert_ne!(row["status_since"], peer_since, "不是 peer 报的时间");
    // B 自己看自己仍是它报的时间——改写只发生在并入方。
    assert_eq!(
        find(rows_of(&b.list_as_human().await), "on-b")["status_since"],
        peer_since
    );
    // "上次见到"同一只表。
    let h = a.health().await;
    assert_eq!(
        h["peers"]["b"]["last_seen"],
        agora::clock::format_utc_secs(T_LOCAL),
        "{h}"
    );
    assert_eq!(h["peers"]["b"]["last_seen"], "2023-11-14T22:13:20Z");

    // 事件流带来的行同样：B 上改名 → A 收到 session_updated，状态没变，起点沿用本机打的那一刻。
    let resp = b
        .router()
        .oneshot(
            Request::patch(format!("/api/sessions/{}", on_b["id"].as_str().unwrap()))
                .header(header::HOST, common::HOST)
                .header(header::ORIGIN, format!("http://{}", common::HOST))
                .header(header::COOKIE, b.cookie())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "display_name": "renamed" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    wait_for("改名经事件流到达 a", || {
        a.state
            .peer_views
            .get(on_b["id"].as_str().unwrap())
            .is_some_and(|r| r["display_name"] == "renamed")
    })
    .await;
    let row = a
        .state
        .peer_views
        .get(on_b["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(row["status_since"], T_LOCAL);
    assert_eq!(row["node"], "b");
}

#[tokio::test]
async fn disconnect_keeps_rows_marked_stale() {
    // 不变量 8 / MISSION §3.5：peer 断线 → 保留最后视图、每行 stale: true、"上次见到"不动、
    // 本机行不受影响；恢复 → 自动重连、重拉全量对齐、stale 回 false。
    let mut a = Node::new("a");
    let b = Node::new("b");
    let on_b = b.create_session("on-b").await;
    a.create_session("on-a").await;
    let t = a.link(&b);
    let wake = a.start_client(t.clone());
    wait_for("a 并入 b", || {
        a.state.peer_views.rows_of("b").len() == 1
            && a.state.peer_views.is_stale("b") == Some(false)
    })
    .await;
    let seen_before = a.state.peers.get("b").unwrap().last_seen.unwrap();
    let mut rx = a.state.events.subscribe();

    // 掉线：transport 拒连，并让客户端断开当前连接去重连（fake 里已建好的 WS 不会自己死；生产里
    // 是 TCP 断或 65 s keepalive 超时——都走同一条 Disconnect::Failed 路径）。
    t.set_offline(true);
    wake.reconnect();
    wait_for("b 的视图标 stale", || {
        a.state.peer_views.is_stale("b") == Some(true)
    })
    .await;
    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    assert_eq!(names(rows), ["on-a", "on-b"], "行一条不少: {body}");
    let stale_row = find(rows, "on-b");
    assert_eq!(stale_row["stale"], true);
    assert_eq!(stale_row["node"], "b");
    assert_eq!(stale_row["id"], on_b["id"]);
    assert!(find(rows, "on-a").get("stale").is_none(), "本机行不受影响");
    // 浏览器从事件流得知 stale，不用重拉。
    expect_event(&mut rx, "on-b 的 stale session_updated", |e| {
        matches!(e, Event::SessionUpdated { id, session } if *id == on_b["id"] && session["stale"] == true)
    })
    .await;
    // Header 那一枚：离线、"上次见到"保留、在重试、原因是不可达。
    let h = a.health().await;
    let p = &h["peers"]["b"];
    assert_eq!(p["online"], false, "{h}");
    assert_eq!(p["last_seen"], agora::clock::format_utc_secs(seen_before));
    assert_eq!(p["retrying"], true);
    assert_eq!(p["last_error"], "unreachable");
    // 单条读给的是最后一眼（带 stale）。
    let resp = a
        .router()
        .oneshot(a.human_get(&format!("/api/sessions/{}", on_b["id"].as_str().unwrap())))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["stale"], true);

    // 掉线期间 B 起了新会话，A 看不见；恢复后自动重连、全量对齐。
    let on_b2 = b.create_session("on-b-2").await;
    assert_eq!(a.state.peer_views.rows_of("b").len(), 1);
    t.set_offline(false);
    wait_for("恢复后对齐", || {
        a.state.peer_views.is_stale("b") == Some(false)
            && a.state.peer_views.rows_of("b").len() == 2
    })
    .await;
    let body = a.list_as_human().await;
    let rows = rows_of(&body);
    assert_eq!(names(rows), ["on-a", "on-b", "on-b-2"], "{body}");
    assert_eq!(find(rows, "on-b")["stale"], false);
    assert_eq!(find(rows, "on-b-2")["id"], on_b2["id"]);
    let p = a.state.peers.get("b").unwrap();
    assert!(p.online && p.last_error.is_none() && !p.retrying, "{p:?}");
    assert!(p.last_seen.unwrap() >= seen_before);
}
