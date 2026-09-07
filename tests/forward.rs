//! 一跳转发的守卫（agora-7ku.7；ADR-003 D8；ADR-004；MISSION §3.5 / §7.3）。
//!
//! 三个 daemon 实例在同一个测试进程里：A 配了 B 为 peer，B 配了 C 为 peer，C 谁都不配。
//! 浏览器（cookie）只连 A；A 对 `b:<id>` 的写操作与终端流经进程内 fake transport 转到 B，
//! B 看到的是 `Peer { name: "a" }`。`c:<id>` 从 A 看是未知节点，从 B 看是第二跳——都不许转。
//!
//! Node / peer / json 辅助抄自 tests/peer_fake.rs（本批 tests/common 谁都不改，合并进 common
//! 留给 agora-7ku.9）。

mod common;

use std::sync::Arc;
use std::time::Duration;

use agora::api::{self, AppState};
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::peer::registry::PeerRegistry;
use agora::peer::transport::{InProcessTransport, PeerTransport, DEFAULT_TIMEOUT};
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tower::ServiceExt;

/// 一个节点实例：内存库 + 假运行时 + 自己的 node.id。
struct Node {
    name: &'static str,
    state: AppState,
    auth: Arc<Auth>,
    rt: Arc<common::FakeRuntime>,
}

impl Node {
    fn new(name: &'static str) -> Self {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let auth = Arc::new(Auth::new(db.clone(), AuthConfig::default()));
        let rt = Arc::new(common::FakeRuntime::default());
        let sessions = Arc::new(SessionManager::new(db, rt.clone() as Arc<dyn Runtime>));
        let state = AppState::new(auth.clone(), sessions, name);
        Node {
            name,
            state,
            auth,
            rt,
        }
    }

    /// 把 `targets` 配成本节点的 peer（各一行配置，ADR-004）。返回 fake transport 好让测试拨
    /// `set_offline`。对端 Router 在此刻快照：先 link 下游、再 link 上游。
    fn link(&mut self, targets: &[&Node]) -> Vec<Arc<InProcessTransport>> {
        let mut reg = PeerRegistry::new(self.name);
        let mut out = Vec::new();
        for t in targets {
            let transport = Arc::new(InProcessTransport::new(
                t.name,
                self.name,
                t.router(),
                DEFAULT_TIMEOUT,
            ));
            reg.insert(transport.clone() as Arc<dyn PeerTransport>)
                .unwrap();
            out.push(transport);
        }
        self.state.registry = Arc::new(reg);
        out
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

    /// 浏览器视角：带 cookie 与同源 Origin 敲本节点。
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Reply {
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
        Reply::from(resp).await
    }

    /// 在本节点上以人的身份起一个会话（假运行时，不跑进程）。
    async fn create_session(&self, display_name: &str) -> Value {
        let r = self
            .call(
                Method::POST,
                "/api/sessions",
                Some(json!({
                    "display_name": display_name,
                    "agent_type": "shell",
                    "working_directory": "/tmp",
                    "command": "sleep 300",
                })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body
    }

    /// 运行时里这个会话还活着吗（独立证人，不经 API）。
    fn alive(&self, r#ref: &str) -> bool {
        self.rt.sessions.lock().unwrap()[r#ref].alive
    }
}

struct Reply {
    status: StatusCode,
    content_type: Option<String>,
    body: Value,
}

impl Reply {
    async fn from(resp: axum::response::Response) -> Self {
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        // 不是 JSON 的 body（提取器的纯文本拒绝）原文保留，断言失败时看得见。
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        Reply {
            status,
            content_type,
            body,
        }
    }
}

/// `caller` 眼里指向 `target` 的 fake transport（对端看到 `Peer { name: caller }`）。
fn peer(target: &Node, caller: &Node) -> Arc<dyn PeerTransport> {
    Arc::new(InProcessTransport::new(
        target.name,
        caller.name,
        target.router(),
        DEFAULT_TIMEOUT,
    ))
}

/// A → B → C 的链：A 只认识 B，B 只认识 C。返回 A→B 的 transport 供拨离线。
fn chain() -> (Node, Node, Node, Arc<InProcessTransport>) {
    let c = Node::new("c");
    let mut b = Node::new("b");
    b.link(&[&c]);
    let mut a = Node::new("a");
    let to_b = a.link(&[&b]).remove(0);
    (a, b, c, to_b)
}

fn gid_of(session: &Value) -> String {
    session["id"].as_str().unwrap().to_owned()
}

fn ref_of(session: &Value) -> String {
    session["runtime_ref"].as_str().unwrap().to_owned()
}

// ---------- 真 WS（浏览器那一侧必须是 cookie 连接，不能用 fake transport 顶替：那会被当成
// peer 而触发一跳规则） ----------

async fn listen(node: &Node) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = node.router();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr.to_string()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: &str, path: &str, cookie: &str) -> Result<Ws, WsError> {
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, format!("http://{addr}").parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

/// 收文本帧直到某条满足 `pred`；Ping 等控制帧跳过。
async fn next_json(ws: &mut Ws, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(left, ws.next())
            .await
            .expect("5 s 内应有帧")
            .expect("流未结束")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if pred(&v) {
                return v;
            }
        }
    }
}

/// 握手被 HTTP 状态拒绝时，取状态码与错误 body（tungstenite 把整个响应带回来）。
fn rejected(err: WsError) -> (StatusCode, Value) {
    match err {
        WsError::Http(resp) => {
            let status = resp.status();
            let body = resp
                .body()
                .as_deref()
                .map(|b| serde_json::from_slice(b).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            (status, body)
        }
        other => panic!("期待 HTTP 拒绝，得到 {other:?}"),
    }
}

// ---------- 守卫 ----------

/// ADR-003 D8：确认在所属节点判断。A 上根本没有这个会话——A 若自己判断只能 404 not_found，
/// 409 needs_confirmation 只能出自 B；A 转发时不带 confirmed 就是不带，带 true 才执行；
/// B 看到的调用方是 Peer { a }，peer 不享有免确认。
#[tokio::test]
async fn kill_confirmation_enforced_at_owner() {
    let (a, b, _c, _) = chain();
    let s = b.create_session("victim").await;
    let gid = gid_of(&s);
    let r#ref = ref_of(&s);
    assert!(gid.starts_with("b:"));
    let kill = format!("/api/sessions/{gid}/kill");

    // 不带 body：B 判会杀 → 409，进程不动。
    let r = a.call(Method::POST, &kill, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "needs_confirmation");
    assert!(b.alive(&r#ref), "未确认的 kill 不得执行");

    // confirmed: false 原样过去，B 还是 409——A 没有替人置 true。
    let r = a
        .call(Method::POST, &kill, Some(json!({ "confirmed": false })))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "needs_confirmation");
    assert!(b.alive(&r#ref));

    // confirmed: true → B 执行；响应是 B 的会话行，原样回到 A 的浏览器（状态码、content-type、body）。
    let r = a
        .call(Method::POST, &kill, Some(json!({ "confirmed": true })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.content_type.as_deref(), Some("application/json"));
    assert_eq!(r.body["id"], gid);
    assert_eq!(r.body["node"], "b");
    assert_eq!(r.body["alive"], false);
    assert_eq!(r.body["status"], "finished");
    assert!(!b.alive(&r#ref), "确认后 B 真的杀了");

    // A 自己的库与运行时一动没动：它只是转发。
    let mine = a.call(Method::GET, "/api/sessions", None).await;
    assert_eq!(mine.body["sessions"].as_array().unwrap().len(), 0);
    assert!(a.rt.sessions.lock().unwrap().is_empty());
}

/// input（text / decision）与 restart 经 A 到 B 执行；所属节点的每一种判定（409 no_pending_decision、
/// 409 needs_confirmation、200 + restart 字段）都原样回来。
#[tokio::test]
async fn input_and_restart_forwarded_to_owner() {
    let (a, b, _c, _) = chain();
    let s = b.create_session("worker").await;
    let gid = gid_of(&s);
    let r#ref = ref_of(&s);

    // text → B 的运行时收到键击；A 的运行时没有。
    let r = a
        .call(
            Method::POST,
            &format!("/api/sessions/{gid}/input"),
            Some(json!({ "kind": "text", "data": "hello\n" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body, json!({}));
    assert_eq!(
        *b.rt.inputs.lock().unwrap(),
        vec![(r#ref.clone(), "hello\n".to_owned())]
    );
    assert!(a.rt.inputs.lock().unwrap().is_empty());

    // decision 没有挂起的 hook：B 的 409 no_pending_decision 原样回，不是 A 造的。
    let r = a
        .call(
            Method::POST,
            &format!("/api/sessions/{gid}/input"),
            Some(json!({ "kind": "decision", "decision": "allow", "request_id": "req-1" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "no_pending_decision");

    // restart：活着 → 409；确认后 → B respawn，epoch +1，响应多一个 restart 字段。
    let restart = format!("/api/sessions/{gid}/restart");
    let r = a.call(Method::POST, &restart, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "needs_confirmation");
    assert!(b.rt.respawns.lock().unwrap().is_empty());
    let r = a
        .call(Method::POST, &restart, Some(json!({ "confirmed": true })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["epoch"], 2);
    assert_eq!(r.body["node"], "b");
    assert_eq!(r.body["restart"]["resumed"], false, "{}", r.body);
    assert_eq!(b.rt.respawns.lock().unwrap().len(), 1);
    assert!(a.rt.respawns.lock().unwrap().is_empty());
}

/// PATCH / cleanup / DELETE 同样经 A 到 B；B 的校验（400 bad_request、409 still_alive、404 not_found）
/// 原样回来，B 的库与运行时是唯一被改的地方。
#[tokio::test]
async fn patch_cleanup_and_delete_forwarded_to_owner() {
    let (a, b, _c, _) = chain();
    let s = b.create_session("old-name").await;
    let gid = gid_of(&s);
    let r#ref = ref_of(&s);
    let path = format!("/api/sessions/{gid}");

    let r = a
        .call(
            Method::PATCH,
            &path,
            Some(json!({ "display_name": "renamed" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["display_name"], "renamed");
    assert_eq!(r.body["node"], "b");
    let on_b = b.call(Method::GET, &path, None).await;
    assert_eq!(on_b.body["display_name"], "renamed", "B 的库里改了名");

    // B 的校验原样回：没有可改的字段。
    let r = a.call(Method::PATCH, &path, Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert_eq!(r.body["error"], "bad_request");

    // 活着不能 cleanup。
    let r = a.call(Method::POST, &format!("{path}/cleanup"), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "still_alive");

    // 杀掉再 cleanup：204，B 的运行时会话被回收。
    let r = a
        .call(
            Method::POST,
            &format!("{path}/kill"),
            Some(json!({ "confirmed": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = a.call(Method::POST, &format!("{path}/cleanup"), None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    assert_eq!(*b.rt.removed.lock().unwrap(), vec![r#ref.clone()]);

    // DELETE metadata：204；之后 B 上 404，再 DELETE 一次也是 B 的 404 原样回。
    let r = a.call(Method::DELETE, &path, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.body);
    let on_b = b.call(Method::GET, &path, None).await;
    assert_eq!(on_b.status, StatusCode::NOT_FOUND);
    let r = a.call(Method::DELETE, &path, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert_eq!(r.body["error"], "not_found");
}

/// 终端流：浏览器连 A 的 `b:<id>/terminal`，A 向 B 建同一条 WS，B 在 PTY 里 attach（sh 扮演）。
/// status / output / exit 从 B 过来，input / resize / ping 从浏览器过去；exit 帧转完桥主动关。
#[tokio::test]
async fn terminal_ws_forwarded_bidirectionally() {
    let (a, b, _c, _) = chain();
    *b.rt.attach_argv.lock().unwrap() = vec![
        "sh".into(),
        "-c".into(),
        "stty -echo; read -r l; echo \"got:$l\"; stty size; exit 3".into(),
    ];
    let s = b.create_session("term").await;
    let gid = gid_of(&s);
    let addr = listen(&a).await;
    let mut ws = connect(
        &addr,
        &format!("/api/sessions/{gid}/terminal?cols=80&rows=24"),
        &a.cookie(),
    )
    .await
    .expect("同源 + cookie 应升级成功");

    let st = next_json(&mut ws, |v| v["type"] == "status").await;
    assert_eq!(st["status"], "attached");

    ws.send(Message::Text(
        r#"{"type":"resize","cols":100,"rows":30}"#.into(),
    ))
    .await
    .unwrap();
    ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
        .await
        .unwrap();
    next_json(&mut ws, |v| v["type"] == "pong").await;
    ws.send(Message::Text(r#"{"type":"input","data":"hi\n"}"#.into()))
        .await
        .unwrap();

    let mut out = String::new();
    let exit = loop {
        let v = next_json(&mut ws, |v| v["type"] == "output" || v["type"] == "exit").await;
        if v["type"] == "exit" {
            break v;
        }
        out.push_str(v["data"].as_str().unwrap());
    };
    assert!(out.contains("got:hi"), "input 应到达 B 的 PTY: {out:?}");
    assert!(out.contains("30 100"), "resize 应到达 B 的 tty: {out:?}");
    assert_eq!(exit["exit"], json!({ "kind": "code", "value": 3 }));

    // exit 之后桥主动关：接下来只能是 Close 或流结束，不会再有业务帧。
    let after = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => panic!("exit 之后不该再有帧: {t}"),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue, // Ping / Pong
            }
        }
    })
    .await;
    assert!(after.is_ok(), "exit 之后 5 s 内应关闭");
    // A 自己没有 attach 任何东西：它没有这个会话，PTY 在 B 那边。
    assert!(a.rt.sessions.lock().unwrap().is_empty());
}

/// 既不是本机也不是 peer 的前缀 → 404 node_unknown（HTTP 六个端点与 WS 升级）；A 不认识 C 就是不认识，
/// 哪怕 B 认识；裸 id 仍走本机。
#[tokio::test]
async fn unknown_node_is_rejected() {
    let (a, _b, c, _) = chain();
    for (method, path, body) in [
        (
            Method::POST,
            "/api/sessions/nobody:1/kill",
            Some(json!({ "confirmed": true })),
        ),
        (Method::POST, "/api/sessions/nobody:1/restart", None),
        (Method::POST, "/api/sessions/nobody:1/cleanup", None),
        (
            Method::POST,
            "/api/sessions/nobody:1/input",
            Some(json!({ "kind": "text", "data": "x" })),
        ),
        (
            Method::PATCH,
            "/api/sessions/nobody:1",
            Some(json!({ "display_name": "x" })),
        ),
        (Method::DELETE, "/api/sessions/nobody:1", None),
    ] {
        let r = a.call(method.clone(), path, body).await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "{method} {path}: {}",
            r.body
        );
        assert_eq!(r.body["error"], "node_unknown", "{method} {path}");
    }

    // A 的表里只有 B：c:<id> 对 A 是未知节点，即使 B 配了 C 也不会"帮忙问一下"。
    let far = c.create_session("far").await;
    let r = a
        .call(
            Method::POST,
            &format!("/api/sessions/{}/kill", gid_of(&far)),
            Some(json!({ "confirmed": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert_eq!(r.body["error"], "node_unknown");
    assert!(c.alive(&ref_of(&far)), "C 的会话不能被碰到");

    // 裸 id 走本机。
    let home = a.create_session("home").await;
    let bare = home["local_id"].as_str().unwrap();
    let r = a
        .call(
            Method::PATCH,
            &format!("/api/sessions/{bare}"),
            Some(json!({ "display_name": "still-home" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["node"], "a");
    assert_eq!(r.body["display_name"], "still-home");

    // WS 升级也以 HTTP 404 回，不建 WS。
    let addr = listen(&a).await;
    let err = connect(&addr, "/api/sessions/nobody:1/terminal", &a.cookie())
        .await
        .expect_err("未知节点不该升级成功");
    let (status, body) = rejected(err);
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "node_unknown", "{body}");
}

/// ADR-004 一跳：B 收到来自 peer A 的 `c:<id>` 请求，B 自己配了 C 也不转——答 node_unknown，
/// C 的会话一动不动。A→B→C 在结构上就不可能。
#[tokio::test]
async fn second_hop_is_refused_at_the_middle_node() {
    let (a, b, c, _) = chain();
    let far = c.create_session("far").await;
    let gid = gid_of(&far);
    let r#ref = ref_of(&far);

    // B 自己认识 C（对照组）：B 的浏览器能经 B 杀 C。先只看 409，证明路是通的。
    let r = b
        .call(Method::POST, &format!("/api/sessions/{gid}/kill"), None)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "needs_confirmation");

    // 同一条路，调用方换成 peer A：B 到此为止。
    let to_b = peer(&b, &a);
    for (method, path, body) in [
        (
            Method::POST,
            format!("/api/sessions/{gid}/kill"),
            Some(json!({ "confirmed": true })),
        ),
        (
            Method::POST,
            format!("/api/sessions/{gid}/input"),
            Some(json!({ "kind": "text", "data": "x" })),
        ),
        (Method::DELETE, format!("/api/sessions/{gid}"), None),
    ] {
        let mut req = Request::builder().method(method.clone()).uri(&path);
        let body = match body {
            Some(v) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = to_b.request(req.body(body).unwrap()).await.unwrap();
        let r = Reply::from(resp).await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "{method} {path}: {}",
            r.body
        );
        assert_eq!(r.body["error"], "node_unknown", "{method} {path}");
    }
    assert!(c.alive(&r#ref), "第二跳被拒，C 的会话不能被碰到");
    assert!(c.rt.inputs.lock().unwrap().is_empty());

    // peer 对 B 自己的会话照常能操作：一跳规则拦的是"再转"，不是 peer 本身。
    let own = b.create_session("mine").await;
    let resp = to_b
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/sessions/{}/kill", gid_of(&own)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let r = Reply::from(resp).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "needs_confirmation");

    // WS 也一样：B 不替 A 去 attach C 的终端——真握手（duplex 上的 hyper），被 404 拒在升级之前。
    let err = to_b
        .connect_ws(&format!("/api/sessions/{gid}/terminal"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            agora::peer::transport::TransportError::WsRejected(StatusCode::NOT_FOUND)
        ),
        "{err:?}"
    );
}

/// peer 不可达是一类独立的错误（502 peer_unreachable），不是 not_found、也不静默；恢复后同一请求就通。
#[tokio::test]
async fn unreachable_peer_is_reported_by_type() {
    let (a, b, _c, to_b) = chain();
    let s = b.create_session("remote").await;
    let gid = gid_of(&s);
    let kill = format!("/api/sessions/{gid}/kill");

    to_b.set_offline(true);
    let r = a.call(Method::POST, &kill, None).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.body);
    assert_eq!(r.body["error"], "peer_unreachable");

    let addr = listen(&a).await;
    let err = connect(&addr, &format!("/api/sessions/{gid}/terminal"), &a.cookie())
        .await
        .expect_err("peer 离线时不该升级成功");
    let (status, body) = rejected(err);
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"], "peer_unreachable", "{body}");

    to_b.set_offline(false);
    let r = a.call(Method::POST, &kill, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"], "needs_confirmation");
}

/// 转发桥的浏览器一侧同样复查（agora-0jt）：A 吊销了这台设备，A 上的桥以 `4401 revoked` 关浏览器、
/// 关到 B 的上游；B 那边的 attach 随之被收走。
#[tokio::test]
async fn revoked_browser_device_closes_forwarded_terminal() {
    let (mut a, b, _c, _) = chain();
    a.state.revoke_check = common::FAST_REVOKE_CHECK;
    *b.rt.attach_argv.lock().unwrap() =
        vec!["sh".into(), "-c".into(), "echo READY; read -r l".into()];
    let s = b.create_session("term").await;
    let gid = gid_of(&s);
    let cookie = a.cookie();
    let addr = listen(&a).await;
    let mut ws = connect(&addr, &format!("/api/sessions/{gid}/terminal"), &cookie)
        .await
        .expect("同源 + cookie 应升级成功");
    next_json(&mut ws, |v| {
        v["type"] == "output" && v["data"].as_str().unwrap().contains("READY")
    })
    .await;

    let device = a.auth.list_devices().unwrap().remove(0).id;
    a.auth.revoke(&device).unwrap();
    let reason = common::expect_close_code(
        &mut ws,
        agora::api::REVOKED_CLOSE_CODE,
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(reason, "revoked");
}
