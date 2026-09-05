//! peer 传输接缝的守卫（agora-7ku.11）：两个 daemon 实例在同一个测试进程里，A 经进程内 fake
//! transport 以 Peer principal 调到 B——不开 socket、不碰 TLS（MISSION §2.3 规则 9 的"单进程
//! 多节点"骨架；agora-7ku.5 / agora-7ku.7 的 fake 多节点测试都以此为底）。

mod common;

use std::sync::Arc;

use agora::api::{self, AppState};
use agora::auth::{Auth, AuthConfig, PairedVia, Principal};
use agora::peer::registry::{PeerRegistry, Route};
use agora::peer::transport::{InProcessTransport, PeerTransport, DEFAULT_TIMEOUT};
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

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
}

/// `caller` 眼里指向 `target` 的 fake transport。
fn peer(target: &Node, caller: &Node) -> Arc<dyn PeerTransport> {
    Arc::new(InProcessTransport::new(
        target.name,
        caller.name,
        target.router(),
        DEFAULT_TIMEOUT,
    ))
}

async fn json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn get_sessions() -> Request<Body> {
    Request::get("/api/sessions").body(Body::empty()).unwrap()
}

#[tokio::test]
async fn two_in_process_daemons_can_call_each_other_as_peer() {
    let a = Node::new("a");
    let b = Node::new("b");
    b.create_session("on-b").await;
    a.create_session("on-a").await;

    // A 的注册表里 b 是 peer；B 的注册表里 a 是 peer（两边各一行配置，ADR-004）。
    let mut reg_a = PeerRegistry::new(a.name);
    reg_a.insert(peer(&b, &a)).unwrap();
    let mut reg_b = PeerRegistry::new(b.name);
    reg_b.insert(peer(&a, &b)).unwrap();

    // A → B：GET /api/sessions 以 Peer principal 到达 B，看到的是 B 的会话、标 B 的节点名。
    let Route::Peer(to_b) = reg_a.route("b") else {
        panic!("b 应是 a 的 peer: {:?}", reg_a.route("b"));
    };
    let resp = to_b.request(get_sessions()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let rows = body["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["node"], "b");
    assert_eq!(rows[0]["display_name"], "on-b");
    assert!(rows[0]["id"].as_str().unwrap().starts_with("b:"));

    // B → A 同样成立："互为 peer 不是新机制"。
    let Route::Peer(to_a) = reg_b.route("a") else {
        panic!("a 应是 b 的 peer");
    };
    let body = json(to_a.request(get_sessions()).await.unwrap()).await;
    assert_eq!(body["sessions"][0]["node"], "a");
    assert_eq!(body["sessions"][0]["display_name"], "on-a");

    // 同一个 Router 不经 fake 直接敲，没有凭据照样 401：Peer 身份只来自 fake 的进程内注入。
    let bare = b.router().oneshot(get_sessions()).await.unwrap();
    assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn callee_sees_the_caller_as_peer_principal() {
    // 用 B 的 AppState 装一个回声路由：Principal 提取器走的是与真 API 完全相同的那条。
    async fn whoami(principal: Principal) -> Json<Principal> {
        Json(principal)
    }
    let a = Node::new("a");
    let b = Node::new("b");
    let echo = Router::new()
        .route("/whoami", get(whoami))
        .with_state(b.state.clone());
    let to_b = InProcessTransport::new(b.name, a.name, echo, DEFAULT_TIMEOUT);
    let resp = to_b
        .request(Request::get("/whoami").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await, json!({ "kind": "peer", "name": "a" }));
}
