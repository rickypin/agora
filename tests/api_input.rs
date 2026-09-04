//! `POST /api/sessions/:id/input`（MISSION §7.3；ADR-002 D5；agora-dvh.9）：decision 经挂起的
//! hook 返回、text 经 PTY、终端先答 / 超时 → `decision_resolved`、NoPendingDecision、respond_via。

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use agora::adapter::Decision;
use agora::events::Event;
use agora::hook::{Delivery, Envelope, Inbox, Receiver};
use agora::local::Response;

use common::{Fx, HOST};

async fn call(
    fx: &Fx,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, cookie);
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = fx.app().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    // axum 自己的 JSON 拒绝（畸形 body）是纯文本 4xx，不是我们的错误形态。
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn create(fx: &Fx, cookie: &str, agent: &str) -> (String, String) {
    let (status, body) = call(
        fx,
        cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": agent, "agent_type": agent, "working_directory": "/tmp" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let gid = body["id"].as_str().unwrap().to_owned();
    let local = body["local_id"].as_str().unwrap().to_owned();
    (gid, local)
}

fn delivery(session: &str, payload: Value) -> Delivery {
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: Some(session.into()),
            agora_epoch: Some(1),
            agent_session_id: "agent-1".into(),
            agent_env: BTreeMap::new(),
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: 1,
        },
        payload,
    }
}

/// 一套带挂起表的 Fx：Receiver 接上 AppState 与事件总线。
fn with_hooks(hold_timeout: Duration) -> (Fx, Arc<Receiver>, tempfile::TempDir) {
    let mut fx = Fx::new();
    let home = tempfile::tempdir().unwrap();
    let receiver =
        Arc::new(Receiver::new(home.path(), fx.sessions.clone()).with_hold_timeout(hold_timeout));
    receiver.attach_events(fx.state.events.clone(), fx.state.node.clone());
    fx.state.hooks = Some(receiver.clone());
    (fx, receiver, home)
}

/// 起一个挂起的 PermissionRequest（模拟 hook 进程在 socket 上等）。
async fn hold(
    receiver: &Arc<Receiver>,
    home: &Path,
    session: &str,
) -> tokio::task::JoinHandle<Response> {
    let path = Inbox::new(home)
        .write(&delivery(
            session,
            json!({ "hook_event_name": "PermissionRequest", "session_id": "agent-1", "tool_name": "Bash", "tool_input": { "command": "rm x" } }),
        ))
        .unwrap();
    let r = receiver.clone();
    let task = tokio::spawn(async move { r.wake(&path).await });
    for _ in 0..100 {
        if !receiver.pending(session).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(receiver.pending(session), vec!["Bash".to_owned()]);
    task
}

async fn next_resolved(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> (String, &'static str) {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::DecisionResolved { id, via, .. } => return (id, via),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn dashboard_allow_reaches_the_held_hook_without_keystrokes() {
    // A15：不打开终端回答 PermissionRequest——决定经挂起的 hook 返回，PTY 上没有任何键击。
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let (gid, local) = create(&fx, &cookie, "claude").await;
    let mut events = fx.state.events.subscribe();
    let task = hold(&receiver, home.path(), &local).await;
    // 状态机同步：挂起登记时 hook 事件也进了状态机。
    fx.sessions
        .apply_hook(
            &local,
            1,
            &agora::adapter::for_host("claude")
                .unwrap()
                .parse(&json!({"hook_event_name":"PermissionRequest","tool_name":"Bash"})),
        )
        .unwrap();
    assert_eq!(
        fx.sessions.get(&local).unwrap().assessment.status,
        agora::status::Status::Waiting
    );

    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "decision", "decision": "allow" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tool_use_id"], "Bash");
    assert!(matches!(
        task.await.unwrap(),
        Response::Hook {
            decision: Decision::Allow
        }
    ));
    assert_eq!(next_resolved(&mut events).await, (gid.clone(), "dashboard"));
    assert!(fx.rt.inputs.lock().unwrap().is_empty(), "不注入键击");
    // 行立刻退出 WAITING，不等 agent 的 PostToolUse。
    let view = fx.sessions.get(&local).unwrap();
    assert_eq!(view.assessment.status, agora::status::Status::Running);
    assert_eq!(view.respond_via, "hook");

    // 再答一次：已经没有挂起了。
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "decision", "decision": "deny", "message": "no" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no_pending_decision");
}

#[tokio::test]
async fn terminal_answer_first_resolves_the_dashboard_side() {
    // 终端与 Dashboard 并存：终端先答（同工具的 PostToolUse 到达）→ hook 无决定退出，
    // Dashboard 收到 decision_resolved via=terminal，再答就是 no_pending_decision。
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let (gid, local) = create(&fx, &cookie, "claude").await;
    let mut events = fx.state.events.subscribe();
    let task = hold(&receiver, home.path(), &local).await;
    let post = Inbox::new(home.path())
        .write(&delivery(&local, json!({ "hook_event_name": "PostToolUse", "session_id": "agent-1", "tool_name": "Bash", "tool_use_id": "toolu_1" })))
        .unwrap();
    receiver.ingest(&post).unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Response::Hook {
            decision: Decision::None
        }
    ));
    assert_eq!(next_resolved(&mut events).await, (gid.clone(), "terminal"));
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "decision", "decision": "allow" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "no_pending_decision");
}

#[tokio::test]
async fn a_hold_times_out_on_the_daemon_side() {
    let (fx, receiver, home) = with_hooks(Duration::from_millis(200));
    let cookie = fx.cookie();
    let (gid, local) = create(&fx, &cookie, "claude").await;
    let mut events = fx.state.events.subscribe();
    let task = hold(&receiver, home.path(), &local).await;
    assert!(matches!(
        task.await.unwrap(),
        Response::Hook {
            decision: Decision::None
        }
    ));
    assert_eq!(next_resolved(&mut events).await, (gid, "timeout"));
    assert!(receiver.pending(&local).is_empty());
}

#[tokio::test]
async fn text_goes_to_the_pty_and_decisions_need_a_hold() {
    let fx = Fx::new(); // 没接挂起表：decision 一律 no_pending_decision。
    let cookie = fx.cookie();
    let (gid, _) = create(&fx, &cookie, "shell").await;
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "text", "data": "ls\n" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let inputs = fx.rt.inputs.lock().unwrap().clone();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].1, "ls\n");

    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "decision", "decision": "allow" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no_pending_decision");

    // respond_via：shell 没有 hook → terminal；不认识的 kind → 400。
    let (_, view) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(view["respond_via"], "terminal");
    let (status, _) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "nope" })),
    )
    .await;
    assert!(status.is_client_error());
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions/nope/input",
        Some(json!({ "kind": "text", "data": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
