//! 不变量 8 与不变量 11 的整栈集成测试（agora-7ku.9；A36；MISSION §2.2；ADR-003；ADR-004）。
//!
//! 同一进程里两个 daemon 实例（`TmuxNode`：各自 AGORA_HOME、真 tmux socket、SQLite、TLS 监听器），
//! 互配 peer 走**真 TLS socket + Bearer + SPKI 指纹钉住**：客户端是生产的 `HttpsTransport` 与
//! `PeerClient`（main.rs 为每个 `peers[]` 起的那个），服务端是生产的 `api::serve_tls_router`。
//! tests/peer_stale.rs / tests/peer_tls.rs / tests/peer_token.rs 各钉一半（进程内 fake 的客户端
//! 循环、单独的 TLS 半边、单独的 token 半边）；这里是它们第一次合起来过 socket。
//!
//! 每条测试的 doc 注明它靠哪些守卫成立、把哪个守卫关掉会怎样；逐条关掉守卫、看对应断言变红的
//! 记录在提交信息里（ADR-003「什么会让它变危险」末句，A36）。
//!
//! 都跑在真 tmux 上、各自的 socket 与 AGORA_HOME 里；agent 是 `agora fake-agent`。

mod common;

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::api;
use agora::auth::peer_token;
use agora::config::PeerSection;
use agora::peer::backoff::BackoffPolicy;
use agora::peer::client::PeerClient;
use agora::peer::registry::PeerRegistry;
use agora::peer::state::PeerError;
use agora::peer::transport::{HttpsTransport, PeerTransport, TransportError, DEFAULT_TIMEOUT};
use agora::peer::view::Wake;
use agora::tls::{self, Fingerprint};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use common::node::TmuxNode;

/// 测试用的退避：5 ms 起、40 ms 顶（生产 1 s / 30 s，`BackoffPolicy::PEER`），只要"会重试"。
const FAST: BackoffPolicy = BackoffPolicy::new(Duration::from_millis(5), Duration::from_millis(40));
/// 等一个条件成立的上限；正常几十毫秒就到，10 s 是 CI 慢机的余量。
const WAIT: Duration = Duration::from_secs(10);
/// 运行时子进程（tmux）的超时。默认 5 s 在 CARGO_BUILD_JOBS=2、几个 agent 同时编译的机器上会
/// 假阴性（第八批 agora-z62 的教训），这里显式给足。
const EXEC_TIMEOUT: Duration = Duration::from_secs(20);
/// 不变量 8 的"不被拖慢"上限：peer 每次尝试是 5 s 超时（`DEFAULT_TIMEOUT`），本机流程只要在任何
/// 一步碰了 peer 一下就会超过它；真 tmux 的 create / kill 各要几十到几百毫秒，5 s 留足余量。
const LOCAL_FLOW_BUDGET: Duration = DEFAULT_TIMEOUT;

fn node() -> TmuxNode {
    TmuxNode::with_runtime("tmux", EXEC_TIMEOUT)
}

// ---------- 人的一侧：cookie + 进程内 router（浏览器打本机 API 的等价物） ----------

fn router(n: &TmuxNode) -> axum::Router {
    api::router(n.state.clone())
}

fn human_get(n: &TmuxNode, path: &str) -> Request {
    Request::get(path)
        .header(header::HOST, common::HOST)
        .header(header::COOKIE, n.cookie())
        .body(Body::empty())
        .unwrap()
}

/// 以人的身份发一个带 body 的写请求（同源头齐全），返回 `(status, body)`。
async fn human_send(
    n: &TmuxNode,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, common::HOST)
        .header(header::ORIGIN, format!("http://{}", common::HOST))
        .header(header::COOKIE, n.cookie());
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = router(n).oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

/// 在本节点上以人的身份起一个会话（真 tmux 里跑 `sleep 300`）。
async fn create_session(n: &TmuxNode, display_name: &str) -> Value {
    let body = json!({
        "display_name": display_name,
        "agent_type": "shell",
        "working_directory": "/tmp",
        "command": "sleep 300",
    });
    let (status, created) = human_send(n, Method::POST, "/api/sessions", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created
}

/// 人看到的 `GET /api/sessions`。
async fn list_as_human(n: &TmuxNode) -> Value {
    let resp = router(n)
        .oneshot(human_get(n, "/api/sessions"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json(resp).await
}

/// 人看到的 `GET /api/sessions/:id`：`(status, body)`。
async fn get_as_human(n: &TmuxNode, gid: &str) -> (StatusCode, Value) {
    let resp = router(n)
        .oneshot(human_get(n, &format!("/api/sessions/{gid}")))
        .await
        .unwrap();
    let status = resp.status();
    (status, json(resp).await)
}

/// 带凭据的 `GET /api/health`：完整报告（peers 段）。
async fn health(n: &TmuxNode) -> Value {
    let resp = router(n)
        .oneshot(human_get(n, "/api/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json(resp).await
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

/// 本机的一整套：create → list → get → input → kill（真 tmux），每一步都断言本机行没有 stale /
/// last_seen，且 peer 的 `peer_row` 一直 stale 地并列在旁。
async fn local_flow(n: &TmuxNode, name: &str, peer_row: &str) {
    let created = create_session(n, name).await;
    assert_local_row(&created);
    assert_eq!(created["node"], n.name.as_str());
    let id = gid(&created).to_owned();
    let body = list_as_human(n).await;
    let rows = rows_of(&body);
    assert_local_row(find(rows, name));
    assert_eq!(find(rows, peer_row)["stale"], true, "{body}");
    let (status, row) = get_as_human(n, &id).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_local_row(&row);
    let (status, out) = human_send(
        n,
        Method::POST,
        &format!("/api/sessions/{id}/input"),
        Some(json!({ "kind": "text", "data": "ls\n" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (status, out) = human_send(
        n,
        Method::POST,
        &format!("/api/sessions/{id}/kill"),
        Some(json!({ "confirmed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_local_row(&out);
    assert_eq!(out["alive"], false, "kill 真的落到了本机运行时: {out}");
}

// ---------- peer 的一侧：机器 token、peers[] 一行、客户端任务 ----------

/// 在被访问节点 `issuer` 上给 `holder` 签机器 token（ADR-003 D3：`agora peer token create <name>`
/// 的等价物），明文写进 holder 的 home（0600）；返回 `token_file` 路径。
fn issue_token(issuer: &TmuxNode, holder: &TmuxNode) -> PathBuf {
    let token = peer_token::create(&issuer.db, &holder.name, false).unwrap();
    write_token(holder, &issuer.name, &token)
}

/// 0600 的 token 文件（D3 的持有方形态），路径按签发方命名；同一路径重写即换 token
/// （`HttpsTransport` 每次请求重读）。
fn write_token(holder: &TmuxNode, issuer: &str, token: &str) -> PathBuf {
    let p = holder.home.join(format!("{issuer}.token"));
    std::fs::write(&p, format!("{token}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    p
}

/// `caller` 眼里指向 `target` 的 `peers[]` 一行：url 是 target 的 TLS 监听器。
fn section(target: &TmuxNode, pin: &str, token_file: &Path) -> PeerSection {
    PeerSection {
        name: target.name.clone(),
        url: format!("https://{}", target.tls_addr.expect("target 已 serve_tls")),
        token_file: token_file.to_path_buf(),
        cert_fingerprint: pin.into(),
    }
}

/// 一个跑着的 peer 客户端任务（main.rs 为每个 peer spawn 的那个）与它的唤醒把手；drop 即停。
struct Client {
    task: tokio::task::JoinHandle<()>,
    wake: Arc<Wake>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 把 `transport` 装进 `caller` 的注册表（同名的换掉、其它保留）并起客户端任务，退避策略由测试给。
fn start_client(
    caller: &mut TmuxNode,
    transport: Arc<HttpsTransport>,
    policy: BackoffPolicy,
) -> Client {
    let transport: Arc<dyn PeerTransport> = transport;
    let mut reg = PeerRegistry::new(&caller.name);
    for existing in caller.state.registry.peers() {
        if existing.name() != transport.name() {
            reg.insert(existing.clone()).unwrap();
        }
    }
    reg.insert(transport.clone()).unwrap();
    caller.state.registry = Arc::new(reg);
    let client = PeerClient::new(transport, &caller.state).with_policy(policy);
    let wake = client.wake();
    let task = tokio::spawn(client.run());
    Client { task, wake }
}

/// 把 `target` 配成 `caller` 的 peer（正确指纹 + 给定 token 文件）并起客户端。
fn link(
    caller: &mut TmuxNode,
    target: &TmuxNode,
    pin: &str,
    token_file: &Path,
    policy: BackoffPolicy,
) -> Client {
    let t = Arc::new(HttpsTransport::new(
        section(target, pin, token_file),
        DEFAULT_TIMEOUT,
    ));
    start_client(caller, t, policy)
}

fn online(n: &TmuxNode, peer: &str) -> bool {
    n.state.peers.get(peer).is_some_and(|p| p.online)
}

fn last_error(n: &TmuxNode, peer: &str) -> Option<PeerError> {
    n.state.peers.get(peer).and_then(|p| p.last_error)
}

/// `sha256:<64 hex>` 的最后一位改掉：格式合法、值不对——中间人拿任何证书都长这样。
fn flip_last_hex(pin: &str) -> String {
    let mut s = pin.to_owned();
    let last = s.pop().unwrap();
    s.push(if last == '0' { '1' } else { '0' });
    s
}

// ---------- 裸的网络客户端：证明"网络可达" ----------

type TlsWs =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// 人从远端设备（V2-1 的手机）经 TLS 监听器订阅 `/api/events`：cookie + 同源 Origin。
async fn events_ws_tls(n: &TmuxNode) -> TlsWs {
    let addr = n.tls_addr.expect("已 serve_tls");
    let connector = tls::client::connector(n.fingerprint.unwrap()).unwrap();
    let io = tls::client::connect(&connector, "127.0.0.1", addr.port())
        .await
        .unwrap();
    let mut req = format!("wss://{addr}/api/events")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert(header::COOKIE, n.cookie().parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, format!("https://{addr}").parse().unwrap());
    tokio_tungstenite::client_async(req, io).await.unwrap().0
}

/// 一直收事件（每帧一个数组，摊平）直到 `done` 说够了（上限 WAIT）。
async fn ws_collect_until(
    ws: &mut TlsWs,
    what: &str,
    mut done: impl FnMut(&[Value]) -> bool,
) -> Vec<Value> {
    let mut got: Vec<Value> = Vec::new();
    let deadline = Instant::now() + WAIT;
    while !done(&got) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let v: Value = serde_json::from_str(&text).unwrap();
                match v {
                    Value::Array(batch) => got.extend(batch),
                    other => got.push(other),
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(other) => panic!("事件流结束: {other:?}; 收到的: {got:?}"),
            Err(_) => panic!("等事件超时: {what}; 收到的: {got:?}"),
        }
    }
    got
}

/// 一次裸的 HTTP/1.1 GET 走 TLS（钉住 `pin`），返回 `(状态码, 响应体)`。不经任何 agora 客户端代码：
/// 这是"网络可达、握手成功、请求到了对方"的最低限度证明。
async fn https_raw(
    addr: SocketAddr,
    pin: Fingerprint,
    path: &str,
    extra_headers: &str,
) -> (u16, String) {
    let connector = tls::client::connector(pin).unwrap();
    let mut io = tls::client::connect(&connector, "127.0.0.1", addr.port())
        .await
        .unwrap();
    io.write_all(raw_get(addr, path, extra_headers).as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), io.read_to_end(&mut buf)).await;
    split_response(&buf)
}

/// 同上，明文 TCP。
async fn http_raw(addr: &str, path: &str, extra_headers: &str) -> (u16, String) {
    let mut io = tokio::net::TcpStream::connect(addr).await.unwrap();
    io.write_all(raw_get(addr.parse().unwrap(), path, extra_headers).as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), io.read_to_end(&mut buf)).await;
    split_response(&buf)
}

fn raw_get(addr: SocketAddr, path: &str, extra_headers: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{extra_headers}\r\n")
}

fn split_response(buf: &[u8]) -> (u16, String) {
    let text = String::from_utf8_lossy(buf);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("不是 HTTP 应答: {text:?}"));
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(
        !head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked"),
        "响应用了 chunked，这里没解码: {head}"
    );
    (status, body.trim().to_owned())
}

fn error_kind(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body:?}"));
    v["error"].as_str().unwrap_or_default().to_owned()
}

/// 不变量 8：一个坏掉的节点不能影响另一个；peer 离线在视图里是 stale（保留最后一眼与时间），
/// 绝不静默消失。
///
/// 两个真 daemon 实例互为 peer（真 TLS + Bearer + 指纹钉住），B 的 fake-agent 会话并入 A 的视图后
/// B 整个死掉（监听器、已建的连接、轮询一起没了；tmux 里的 agent 还活着——不变量 3），再在原端口
/// 重启；然后反过来 A 死一次。
/// 守卫：
/// - `PeerClient::run` 永不返回、失败只写 `PeerStates::failed` + `PeerViews::mark_stale`（行留着、
///   翻 stale、记 last_seen），退避封顶并一直重试。把 `mark_stale` 改成删行，"行仍在"红；让 `run`
///   在几次失败后返回，重连那一段红（online 回不来、retrying 变 false）。
/// - `PeerState::failed` 不动 `last_seen`。让它清掉，health 与行上的 last_seen 断言红。
/// - 本机的 create / list / get / input / kill 只碰本机运行时，peer 的行从内存视图并入
///   （`PeerViews::rows`），不去问 peer。让 list 先同步问一遍 peer，本机流程的时间上限红。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_8_broken_peer_isolated() {
    let mut a = node();
    let mut b = node();
    let (a_addr, fp_a) = a.serve_tls().await;
    let (b_addr, fp_b) = b.serve_tls().await;

    // B 给 A 签机器 token，A 把 B 配成 peer：真 TLS、真 Bearer、指纹钉住。
    let a_holds = issue_token(&b, &a);
    let _a_to_b = link(&mut a, &b, &fp_b.to_string(), &a_holds, FAST);
    wait_for("A 并入 B", || online(&a, &b.name)).await;
    let h = health(&a).await;
    assert_eq!(h["peers"][&b.name]["online"], true, "{h}");
    assert!(h["peers"][&b.name]["last_error"].is_null(), "{h}");

    // B 上起一个真的 fake-agent：经 B 的轮询 → B 的 /api/events（TLS 上的 WS）→ A 的视图。
    let on_b = b.create_fake("on-b", "ignore-hup; print READY; read; print AFTER; exit 5");
    let on_b_id = on_b.record.id.clone();
    let on_b_gid = b.gid(&on_b_id);
    b.wait(&on_b_id, |v| v.alive);
    wait_for("on-b 出现在 A 的视图里", || {
        a.state.peer_views.get(&on_b_gid).is_some()
    })
    .await;
    let body = list_as_human(&a).await;
    let row = find(rows_of(&body), "on-b");
    assert_eq!(row["node"], b.name.as_str(), "{row}");
    assert_eq!(row["id"], on_b_gid.as_str());
    assert_eq!(row["stale"], false);
    assert!(
        row.get("last_seen").is_none(),
        "在线的 peer 行没有 last_seen"
    );

    // 反向：A 也是 B 的 peer（A 给 B 签 token）。
    let b_holds = issue_token(&a, &b);
    let _b_to_a = link(&mut b, &a, &fp_a.to_string(), &b_holds, FAST);
    wait_for("B 并入 A", || online(&b, &a.name)).await;

    // 人在远端设备上订阅 A 的事件流（TLS 上的 WS），全程挂着。
    let mut ws = events_ws_tls(&a).await;

    // —— B 整个死掉 ——
    b.crash();
    wait_for("A 把 B 标成 stale", || {
        a.state.peer_views.is_stale(&b.name) == Some(true)
    })
    .await;

    // A 的本机流程全通、不被拖慢。
    let started = Instant::now();
    local_flow(&a, "local-while-b-down", "on-b").await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < LOCAL_FLOW_BUDGET,
        "本机流程被 peer 拖慢了: {elapsed:?}"
    );

    // health：离线、退避中、原因是不可达、last_seen 保留。
    let h = health(&a).await;
    let p = &h["peers"][&b.name];
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
    assert_eq!(last_error(&a, &b.name), Some(PeerError::Unreachable));

    // B 的行仍在且 stale，last_seen 与 health 同一个值；单条读同样。
    let body = list_as_human(&a).await;
    let row = find(rows_of(&body), "on-b");
    assert_eq!(row["stale"], true, "{row}");
    assert_eq!(row["last_seen"], seen, "{row}");
    assert_eq!(row["node"], b.name.as_str());
    let (status, row) = get_as_human(&a, &on_b_gid).await;
    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["stale"], true);
    assert_eq!(row["last_seen"], seen);

    // A 的 /api/events 照常：B 的行翻 stale 的 session_updated、本机新会话的 session_created
    // 都到了远端订阅者手里。
    let got = ws_collect_until(
        &mut ws,
        "stale 的 session_updated 与本机的 session_created",
        |got| {
            got.iter().any(|e| {
                e["type"] == "session_updated"
                    && e["id"] == on_b_gid.as_str()
                    && e["session"]["stale"] == true
            }) && got.iter().any(|e| {
                e["type"] == "session_created"
                    && e["session"]["display_name"] == "local-while-b-down"
            })
        },
    )
    .await;
    for e in &got {
        if e["type"] == "session_updated"
            && e["id"] == on_b_gid.as_str()
            && e["session"]["stale"] == true
        {
            assert_eq!(e["session"]["last_seen"], seen, "{e}");
        }
    }

    // B 的 agent 没死（不变量 3 的旁证）：daemon 死了，tmux 里的 pane 还在。
    assert!(b.pane_alive(&on_b), "B 的 daemon 死后它的 agent 不能退出");

    // —— B 原端口重启：同一 home，证书复用、指纹不变；A 在退避上限内重连，行恢复 ——
    let m = b.rebuild();
    assert!(m.get(&on_b_id).unwrap().alive);
    let (addr2, fp2) = b.serve_tls_at(b_addr.port()).await;
    assert_eq!(addr2, b_addr);
    assert_eq!(fp2, fp_b, "同一 home 的自签证书复用，指纹不变");
    wait_for("A 重连 B", || {
        a.state.peer_views.is_stale(&b.name) == Some(false)
    })
    .await;
    let h = health(&a).await;
    let p = &h["peers"][&b.name];
    assert_eq!(p["online"], true, "{h}");
    assert_eq!(p["retrying"], false, "{h}");
    assert!(p["last_error"].is_null(), "{h}");
    let body = list_as_human(&a).await;
    let rows = rows_of(&body);
    let row = find(rows, "on-b");
    assert_eq!(row["stale"], false, "{row}");
    assert!(
        row.get("last_seen").is_none(),
        "恢复后 last_seen 键消失: {row}"
    );
    assert_eq!(row["id"], on_b_gid.as_str());
    assert_local_row(find(rows, "local-while-b-down"));

    // —— 反向：A 死掉，B 照常 ——
    drop(ws);
    a.crash();
    wait_for("B 把 A 标成 stale", || {
        b.state.peer_views.is_stale(&a.name) == Some(true)
    })
    .await;
    let started = Instant::now();
    local_flow(&b, "local-while-a-down", "local-while-b-down").await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < LOCAL_FLOW_BUDGET,
        "B 的本机流程被 peer 拖慢了: {elapsed:?}"
    );
    let h = health(&b).await;
    assert_eq!(h["peers"][&a.name]["online"], false, "{h}");
    assert_eq!(h["peers"][&a.name]["last_error"], "unreachable", "{h}");
    assert!(h["peers"][&a.name]["last_seen"].is_string(), "{h}");
    // A 原端口重启，B 自愈。
    a.rebuild();
    let (addr2, fp2) = a.serve_tls_at(a_addr.port()).await;
    assert_eq!((addr2, fp2), (a_addr, fp_a));
    wait_for("B 重连 A", || {
        b.state.peer_views.is_stale(&a.name) == Some(false)
    })
    .await;
    let body = list_as_human(&b).await;
    let rows = rows_of(&body);
    assert_eq!(find(rows, "local-while-b-down")["stale"], false);
    assert_local_row(find(rows, "local-while-a-down"));
    assert_local_row(find(rows, "on-b"));
}

/// 不变量 11：节点互不信任——每个节点独立认证 principal；网络可达 ≠ 授权。
///
/// 全程两个真 daemon 实例、真 TLS socket；每一步都是"连得上、握手成、请求到了对方"之后被拒——
/// 不是离线。守卫（ADR-003）与关掉之后：
/// - D4 指纹钉住（`tls::client` 的 `PinnedVerifier`）：③ 错指纹是 `fingerprint_mismatch`（独立状态，
///   不并进 unreachable），错误里带对端真实指纹。让 verifier 接受任何证书，③ 红。
/// - D3 机器 token（`auth::peer_token::authenticate`：查 name → 常量时间比对 → 未吊销）：① 未签发
///   401、② 吊销即时 401。让 `authenticate` 只看 `apt_` 前缀不查库，①② 红。
/// - D3 / D5「Bearer 只上 TLS」（`Principal` 提取器看 `TlsListener` 标记）：⑥ 明文监听器带 Bearer
///   是 `bearer_requires_tls`。去掉那个判断，⑥ 红。
/// - D1 每个 handler 都要 `Principal`，白名单只有 health 的公开子集：④ 无凭据的 GET /api/sessions
///   401、GET /api/health 恰为 `{"status":"ok"}`。让 health 对无凭据也给完整报告，④ 红。
/// - D2 设备吊销即时（`Auth::authenticate_session` 查 `revoked_at`）：⑤ 已吊销设备的 cookie 401。
///   忽略 `revoked_at`，⑤ 红。
#[tokio::test(flavor = "multi_thread")]
async fn invariant_11_unauthenticated_peer_and_device_rejected() {
    let mut a = node();
    let mut b = node();
    let (b_addr, fp_b) = b.serve_tls().await;
    let on_b = b.create_fake("on-b", "ignore-hup; print READY; read; print AFTER; exit 5");
    let on_b_gid = b.gid(&on_b.record.id);

    // ③ 错指纹（最后一位 hex 改掉）：TCP 连上了、握手在证书校验处被拒——独立状态
    // fingerprint_mismatch，不是 unreachable；错误里的 actual 是对端真实指纹（D4，无 TOFU）。
    let unissued = write_token(
        &a,
        &b.name,
        &format!("apt_{}_{}", a.name, agora::auth::random_token()),
    );
    let wrong = flip_last_hex(&fp_b.to_string());
    let mitm = Arc::new(HttpsTransport::new(
        section(&b, &wrong, &unissued),
        DEFAULT_TIMEOUT,
    ));
    let err = mitm
        .request(Request::get("/api/sessions").body(Body::empty()).unwrap())
        .await
        .unwrap_err();
    match &err {
        TransportError::FingerprintMismatch { expected, actual } => {
            assert_eq!(expected, &wrong);
            assert_eq!(
                actual,
                &fp_b.to_string(),
                "错误里带对端真实指纹，人能据此核对"
            );
        }
        other => panic!("应是 FingerprintMismatch，而不是 {other:?}"),
    }
    let client = start_client(&mut a, mitm, FAST);
    wait_for("A 的 health 说 B 指纹不匹配", || {
        last_error(&a, &b.name) == Some(PeerError::FingerprintMismatch)
    })
    .await;
    let h = health(&a).await;
    assert_eq!(
        h["peers"][&b.name]["last_error"], "fingerprint_mismatch",
        "{h}"
    );
    assert_eq!(h["peers"][&b.name]["online"], false, "{h}");
    assert!(
        a.state.peer_views.rows_of(&b.name).is_empty(),
        "指纹不匹配时一行都不读"
    );
    drop(client);

    // ① 指纹对了、token 没人签过：握手成功、请求到达 B、B 回 401；A 记 unauthorized
    // （不是 unreachable，也不是 misconfigured——文件本身是合法形态的）。
    let right = Arc::new(HttpsTransport::new(
        section(&b, &fp_b.to_string(), &unissued),
        DEFAULT_TIMEOUT,
    ));
    let resp = right
        .request(Request::get("/api/sessions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json(resp).await["error"], "unauthenticated");
    let client = start_client(&mut a, right.clone(), FAST);
    wait_for("A 的 health 说 B 未授权", || {
        last_error(&a, &b.name) == Some(PeerError::Unauthorized)
    })
    .await;
    let h = health(&a).await;
    let p = &h["peers"][&b.name];
    assert_eq!(p["last_error"], "unauthorized", "{h}");
    assert_eq!(p["online"], false, "{h}");
    assert_eq!(
        p["retrying"], true,
        "token 随时可能签出来，照常退避重试: {h}"
    );
    assert!(
        a.state.peer_views.rows_of(&b.name).is_empty(),
        "未授权时一行都不读"
    );

    // ② 签发 → 在线（同一路径重写 token 文件，transport 每次请求重读，客户端不重启）；
    // 吊销 → 下一次请求 401、又回 unauthorized；再签 → 又在线。
    let token = peer_token::create(&b.db, &a.name, false).unwrap();
    write_token(&a, &b.name, &token);
    wait_for("签发后在线", || online(&a, &b.name)).await;
    wait_for("on-b 并入 A", || {
        a.state.peer_views.get(&on_b_gid).is_some()
    })
    .await;
    let h = health(&a).await;
    assert_eq!(h["peers"][&b.name]["online"], true, "{h}");
    assert!(h["peers"][&b.name]["last_error"].is_null(), "{h}");
    peer_token::revoke(&b.db, &a.name).unwrap();
    // 吊销挡的是下一次请求（每请求查一次库，不缓存）；已建好的事件流不会自己断，先直接发一个。
    let resp = right
        .request(Request::get("/api/sessions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "吊销即时生效");
    client.wake.reconnect();
    wait_for("吊销后 unauthorized", || {
        last_error(&a, &b.name) == Some(PeerError::Unauthorized)
    })
    .await;
    assert_eq!(
        a.state.peer_views.is_stale(&b.name),
        Some(true),
        "被拒之后行是 stale，不是消失"
    );
    assert_eq!(a.state.peer_views.rows_of(&b.name).len(), 1);
    let token = peer_token::create(&b.db, &a.name, false).unwrap();
    write_token(&a, &b.name, &token);
    wait_for("再签后在线", || {
        online(&a, &b.name) && a.state.peer_views.is_stale(&b.name) == Some(false)
    })
    .await;
    drop(client);

    // ④ 无凭据直连 B 的 TLS 端口（D1）：每条路由都要 principal；health 只给公开子集。
    let (status, body) = https_raw(b_addr, fp_b, "/api/sessions", "").await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(error_kind(&body), "unauthenticated");
    let (status, body) = https_raw(b_addr, fp_b, "/api/health", "").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, r#"{"status":"ok"}"#, "公开子集只有 status");
    // 对照：带 Bearer 的 health 才是完整形态。
    let bearer = format!("Authorization: Bearer {token}\r\n");
    let (status, body) = https_raw(b_addr, fp_b, "/api/health", &bearer).await;
    assert_eq!(status, 200, "{body}");
    let full: Value = serde_json::from_str(&body).unwrap();
    assert!(
        full.get("peers").is_some() && full.get("runtime").is_some(),
        "{full}"
    );

    // ⑤ 已吊销设备的 cookie（D2）：同一个 cookie，吊销前 200、吊销后 401。
    let cookie = b.cookie();
    let cookie_header = format!("Cookie: {cookie}\r\n");
    let (status, body) = https_raw(b_addr, fp_b, "/api/sessions", &cookie_header).await;
    assert_eq!(status, 200, "{body}");
    let device_id: String =
        b.db.conn()
            .query_row(
                "SELECT id FROM devices WHERE session_sha256 = ?1",
                [agora::auth::sha256_hex(cookie.split_once('=').unwrap().1)],
                |r| r.get(0),
            )
            .unwrap();
    b.auth.revoke(&device_id).unwrap();
    let (status, body) = https_raw(b_addr, fp_b, "/api/sessions", &cookie_header).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(error_kind(&body), "unauthenticated");

    // ⑥ 明文监听器上带 Bearer（D3 / D5）：结构性拒绝，连 token 都不看——任何节点的明文监听器都
    // 一样，这里用 A 的。对照：同一 token 在 B 的 TLS 监听器上可用。
    let plain = a.serve().await;
    let (status, body) = http_raw(&plain, "/api/sessions", &bearer).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(error_kind(&body), "bearer_requires_tls");
    let (status, body) = http_raw(&plain, "/api/health", &bearer).await;
    assert_eq!(
        status, 401,
        "明文上的 Bearer 不能降级成公开子集蒙混过去: {body}"
    );
    let (status, body) = https_raw(b_addr, fp_b, "/api/sessions", &bearer).await;
    assert_eq!(status, 200, "{body}");
    let listed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(find(rows_of(&listed), "on-b")["id"], on_b_gid.as_str());
}

/// agora-284：经 peer 转发的 Kill 不能被节点自己的宽限拖到转发超时。
///
/// B 上的 fake-agent 忽略 SIGTERM（交互式 shell 的样子），人在 A 上对它按 Kill：A 把请求转发给 B，
/// B 的 kill 只同步等 `KILL_QUICK_WAIT`（1 s），没退就立即返回仍 alive 的行（killed_at 已写），
/// 宽限在 B 的后台线程走完再 SIGKILL。此前 B 同步等满 `KILL_GRACE`（5 s）== 转发超时
/// `DEFAULT_TIMEOUT`（5 s），A 报 502 peer_unreachable 而 B 其实正在杀（2026-09-07 zuan 实测）。
///
/// 让它红的方法：把 kill 改回 `runtime.terminate(&ref, KILL_GRACE)` 同步等满——A 这边 502。
#[tokio::test(flavor = "multi_thread")]
async fn forwarded_kill_returns_before_the_grace_period() {
    let mut a = node();
    let mut b = node();
    let (_a_addr, _fp_a) = a.serve_tls().await;
    let (_b_addr, fp_b) = b.serve_tls().await;
    let a_holds = issue_token(&b, &a);
    let _a_to_b = link(&mut a, &b, &fp_b.to_string(), &a_holds, FAST);
    wait_for("A 并入 B", || online(&a, &b.name)).await;

    let on_b = b.create_fake("on-b", "ignore-term; print READY; read");
    let on_b_id = on_b.record.id.clone();
    let on_b_gid = b.gid(&on_b_id);
    b.wait(&on_b_id, |v| v.alive && b.tail(v).contains("READY"));
    wait_for("on-b 出现在 A 的视图里", || {
        a.state.peer_views.get(&on_b_gid).is_some()
    })
    .await;

    // 人在 A 上 Kill（已确认）：A → B 转发，必须在转发超时内 200 回来，行仍 alive 且 killed_at 已写。
    let t0 = Instant::now();
    let (status, out) = human_send(
        &a,
        Method::POST,
        &format!("/api/sessions/{on_b_gid}/kill"),
        Some(json!({ "confirmed": true })),
    )
    .await;
    let took = t0.elapsed();
    assert_eq!(status, StatusCode::OK, "转发的 kill 用了 {took:?}: {out}");
    assert!(
        took < DEFAULT_TIMEOUT,
        "转发的 kill 用了 {took:?}，不能逼近转发超时 {DEFAULT_TIMEOUT:?}"
    );
    assert_eq!(out["id"], on_b_gid.as_str(), "{out}");
    assert_eq!(out["alive"], true, "TERM 被忽略，返回时进程应还活着: {out}");
    assert!(out["killed_at"].is_string(), "{out}");
    assert!(
        b.pane_alive(&b.sessions.get(&on_b_id).unwrap()),
        "tmux 独立作证：还活着"
    );

    // 宽限满后 B 自己 SIGKILL：agora 与 tmux 都说死了，且是"用户杀的"。等到退出码也收集到了再
    // 断言：pane 刚死、SIGCHLD 还没到的那一两个 tick 是设计内的瞬时 UNKNOWN，只等 !alive 在
    // Linux CI 上会踩进去（2026-09-07 ubuntu-22.04 红过，macOS 撞不上）。
    let deadline = Instant::now() + agora::session::manager::KILL_GRACE + Duration::from_secs(5);
    let dead = loop {
        let v = b.sessions.get(&on_b_id).unwrap();
        if !v.alive && v.exit.is_some() {
            break v;
        }
        assert!(
            Instant::now() < deadline,
            "宽限满后没被 SIGKILL（或退出码一直没收集到）: {v:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(!b.pane_alive(&dead));
    assert_eq!(
        dead.assessment.status,
        agora::status::Status::Finished,
        "{dead:?}"
    );
    wait_for("A 的视图里 on-b 也变成不 alive", || {
        a.state
            .peer_views
            .get(&on_b_gid)
            .is_some_and(|row| row["alive"] == false)
    })
    .await;
}
