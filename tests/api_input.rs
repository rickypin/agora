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
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
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
            received_unix_ms: SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
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

/// 每次挂起生成独立身份：同一个 tool_name 被下一条请求复用也不能串批。
async fn hold_named(
    receiver: &Arc<Receiver>,
    home: &Path,
    session: &str,
    tool: &str,
    ms: u64,
) -> (String, tokio::task::JoinHandle<Response>) {
    let mut d = delivery(
        session,
        json!({ "hook_event_name": "PermissionRequest", "tool_name": tool }),
    );
    d.envelope.received_unix_ms = ms;
    let path = Inbox::new(home).write(&d).unwrap();
    let previous = receiver.pending(session);
    let r = receiver.clone();
    let task = tokio::spawn(async move { r.wake(&path).await });
    // 这里用账本证明本条已处理；是否挂起由后面的 API 快照校验。
    for _ in 0..100 {
        if receiver
            .received_for(session)
            .iter()
            .any(|r| r.delivery.envelope.received_unix_ms == ms)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!receiver.pending(session).is_empty(), "{previous:?}");
    (tool.into(), task)
}

#[tokio::test]
async fn parallel_decisions_bind_snapshot_summary_to_request_and_reject_stale_ids() {
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let (gid, local) = create(&fx, &cookie, "claude").await;
    fx.sessions
        .apply_hook(
            &local,
            1,
            &[agora::status::AgoraEvent::Activity("working".into())],
        )
        .unwrap();
    let mut differ = agora::events::Differ::default();
    differ.step("testnode", &fx.sessions.list().unwrap());
    let (_, bash) = hold_named(&receiver, home.path(), &local, "Bash", 10).await;
    let changes = differ.step("testnode", &fx.sessions.list().unwrap());
    assert!(
        changes
            .iter()
            .any(|e| matches!(e, Event::Notification { .. })),
        "带请求对象的整行事件也须通知"
    );
    assert!(changes.iter().any(|e| matches!(e, Event::SessionUpdated { session, .. } if session["pending_decision"]["summary"] == "Bash")));
    let (_, write) = hold_named(&receiver, home.path(), &local, "Write", 20).await;
    let endpoint = format!("/api/sessions/{gid}");
    let (_, row) = call(&fx, &cookie, Method::GET, &endpoint, None).await;
    assert_eq!(row["pending_decision"]["summary"], "Write");
    let request_id = row["pending_decision"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = json!({"kind":"decision", "decision":"allow", "request_id":request_id});
    let (status, result) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("{endpoint}/input"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["tool_use_id"], "Write");
    assert!(matches!(
        write.await.unwrap(),
        Response::Hook {
            decision: Decision::Allow
        }
    ));
    assert!(!bash.is_finished(), "显示 Write 时不能批准 Bash");
    let (_, row) = call(&fx, &cookie, Method::GET, &endpoint, None).await;
    assert_eq!(row["pending_decision"]["summary"], "Bash");
    // 同工具的新一条请求，旧按钮不得放行它。
    let (_, write2) = hold_named(&receiver, home.path(), &local, "Write", 30).await;
    let (status, result) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("{endpoint}/input"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(result["error"], "no_pending_decision");
    assert!(!write2.is_finished());
    assert!(!bash.is_finished());
    receiver.resolve_session(&local, "test");
    bash.await.unwrap();
    write2.await.unwrap();
    assert!(fx.sessions.get(&local).unwrap().pending_decision.is_none());
}

#[tokio::test]
async fn old_epoch_approval_cannot_answer_a_restarted_session() {
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let (_, local) = create(&fx, &cookie, "claude").await;
    let (_, held) = hold_named(&receiver, home.path(), &local, "Bash", 10).await;
    let request = fx.sessions.get(&local).unwrap().pending_decision.unwrap();
    fx.sessions.restart(&local, &[]).unwrap();
    assert!(receiver
        .respond_request(&local, &request.request_id, Decision::Allow)
        .is_err());
    assert!(fx.sessions.get(&local).unwrap().pending_decision.is_none());
    receiver.resolve_session(&local, "test");
    held.await.unwrap();
}

#[tokio::test]
async fn failed_decision_checkpoint_does_not_send_approval_and_can_retry() {
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let (gid, local) = create(&fx, &cookie, "claude").await;
    let (_, held) = hold_named(&receiver, home.path(), &local, "Write", 10).await;
    let request = fx.sessions.get(&local).unwrap().pending_decision.unwrap();
    let state_dir = home.path().join("hooks/state");
    let backup = home.path().join("checkpoint-backup");
    std::fs::rename(&state_dir, &backup).unwrap();
    std::fs::write(&state_dir, "blocked").unwrap();
    let body = json!({"kind":"decision", "decision":"allow", "request_id":request.request_id});
    let endpoint = format!("/api/sessions/{gid}/input");
    let (status, result) = call(&fx, &cookie, Method::POST, &endpoint, Some(body.clone())).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(result["error"], "hook_state");
    assert!(!held.is_finished(), "持久化失败不能把批准交给 agent");
    assert_eq!(
        fx.sessions.get(&local).unwrap().assessment.status,
        agora::status::Status::Waiting
    );
    std::fs::rename(&state_dir, home.path().join("obstruction")).unwrap();
    std::fs::rename(&backup, &state_dir).unwrap();
    assert_eq!(
        call(&fx, &cookie, Method::POST, &endpoint, Some(body))
            .await
            .0,
        StatusCode::OK
    );
    assert!(matches!(
        held.await.unwrap(),
        Response::Hook {
            decision: Decision::Allow
        }
    ));
}

#[tokio::test]
async fn a_custom_typed_session_with_a_held_claude_hook_takes_dashboard_decisions() {
    // agora-1dr：agent_type=fake（用户在 New Agent 里选 custom 填自己的命令）里跑的其实是
    // claude，它的 hook 挂起了——Dashboard 必须给 Allow / Deny，且答复真能回到那个 hook。
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let (gid, local) = create(&fx, &cookie, "fake").await;
    let (_, before) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(before["respond_via"], "terminal");
    assert!(before["respond_within_secs"].is_null());

    let task = hold(&receiver, home.path(), &local).await;
    let (_, view) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(view["respond_via"], "hook", "{view}");
    assert_eq!(view["respond_within_secs"], 55 * 60);
    assert_eq!(view["pending_decision"]["host"], "claude");
    let request_id = view["pending_decision"]["request_id"].as_str().unwrap();

    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "decision", "decision": "allow", "request_id": request_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(matches!(
        task.await.unwrap(),
        Response::Hook {
            decision: Decision::Allow
        }
    ));
    assert!(fx.rt.inputs.lock().unwrap().is_empty(), "不注入键击");
    let (_, after) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(after["respond_via"], "terminal", "挂起解除后退回声明类型");
}
