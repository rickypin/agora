//! `POST /api/sessions/:id/input`（MISSION §7.3；ADR-002 D5；agora-dvh.9）：decision 经挂起的
//! hook 返回、text 经 PTY、终端先答 / 超时 → `decision_resolved`、NoPendingDecision、respond_via。

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// 按事实等到上限：条件成立即退出，最多等 5 s（5 ms 一轮）。
///
/// 2026-09-06：之前是固定 100 × 5 ms / 100 × 10 ms 轮询，三路 agent 并行编译的机器与 CI
/// 单核（GitHub Actions run 34001122947）上 receiver 的 wake 还没跑到就超时，随后的断言假阴性
/// （agora-e8s）。等待上限放宽不等于放宽断言：断言原样保留，超时了仍然会红。
/// 2026-09-10 又从 5 s 提到 `isolate::PROC`：同一条病在 2026-09-09 那批打红了
/// `old_epoch_approval_cannot_answer_a_restarted_session`（agora-d0p）。
async fn wait_until(mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + common::isolate::PROC;
    while !cond() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

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

/// 无句柄的 external 行（`host:agent_session_id` 为身份；agora-t5kf.1 的输入通道就长在它们上）。
fn external_delivery(host: &str, agent_session: &str, payload: Value) -> Delivery {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    Delivery {
        envelope: Envelope {
            host: host.into(),
            agora_session_id: None,
            agora_epoch: None,
            agent_session_id: agent_session.into(),
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
    wait_until(|| !receiver.pending(session).is_empty()).await;
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
    assert_eq!(view["text_via"], "runtime", "有句柄的行：text 走 PTY");
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
    // 等两件事都到位：账本证明**本条**已处理（只看 pending 会被上一条挂起骗过），pending 证明
    // hold 已登记。`wake` 里"ingest（账本在这里写）→ begin_wake 登记 hold"是两步，中间有个窗口：
    // 2026-10-07 ubuntu-24.04 CI 上等账本单条件跑进了窗口里，pending 还是空的（本地快跑不出来）。
    wait_until(|| {
        receiver
            .received_for(session)
            .iter()
            .any(|r| r.delivery.envelope.received_unix_ms == ms)
            && !receiver.pending(session).is_empty()
    })
    .await;
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

#[tokio::test]
async fn an_interrupted_permission_is_released_via_terminal_when_the_prompt_disappears() {
    // agora-9cd：PermissionRequest 挂起 → 终端按 1 放行 → 按 Esc，Claude 2.1.261 一个事件都不发
    // （testdata/claude/2.1.261/hooks/interrupted.jsonl）。唯一可观测的事实是屏幕上的权限提示没了：
    // 状态机见过提示、之后连续 text_ticks 个 tick 见不到 → 清挂起、UNKNOWN(text)；receiver 的 sweep 把还在
    // socket 上等的 hook 以 none 放掉（fail-open）、pending_decision 移除、decision_resolved via=terminal。
    // 守卫：从 SessionManager / API 入口覆盖接线（ADR-002 D1 例外——有挂起时 hooked 会话也 capture）。
    // 关掉：manager 的 capture 条件去掉 `|| pending` → 一直 waiting，第一处 unknown 断言超时红；
    //       sweep 去掉孤儿 hold 那段 → pending 不空、hold 不返回，红。
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let (gid, local) = create(&fx, &cookie, "claude").await;
    let mut events = fx.state.events.subscribe();
    let reference = fx.sessions.record(&local).unwrap().runtime_ref.unwrap();
    let task = hold(&receiver, home.path(), &local).await;
    let endpoint = format!("/api/sessions/{gid}");

    // 对照：提示一直在屏幕上（跨过至少两个 tick，默认 tick = 2 s）→ 一直 waiting、hold 仍在。
    fx.rt.tails.lock().unwrap().insert(
        reference.clone(),
        "Do you want to proceed?\n❯ 1. Yes\n  2. No".into(),
    );
    for i in 0..3 {
        let (_, row) = call(&fx, &cookie, Method::GET, &endpoint, None).await;
        assert_eq!(row["status"], "waiting", "{i}: {row}");
        assert!(row["pending_decision"].is_object(), "{i}: {row}");
        assert!(
            row["preview"].is_null(),
            "hooked 会话的 preview 仍为 null：{row}"
        );
        receiver.sweep();
        assert_eq!(receiver.pending(&local), vec!["Bash".to_owned()], "{i}");
        if i < 2 {
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
    }

    // 终端放行 / Esc：提示消失，屏幕只剩空提示符。连续 GET（每次都是一次 observe）直到翻 unknown。
    fx.rt.tails.lock().unwrap().insert(reference, "❯ ".into());
    let deadline = Instant::now() + common::isolate::PROC;
    let mut row = Value::Null;
    while Instant::now() < deadline {
        row = call(&fx, &cookie, Method::GET, &endpoint, None).await.1;
        if row["status"] == "unknown" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(row["status"], "unknown", "{row}");
    assert_eq!(row["source"], "text", "{row}");
    assert!(
        row["reason"].as_str().unwrap().contains("prompt gone"),
        "{row}"
    );

    // sweep 放掉还在等的 hook（生产里每 5 s 一次；hold 登记 ≥ 2 s 后才算，所以这里重试到它放）。
    let deadline = Instant::now() + common::isolate::PROC;
    while !receiver.pending(&local).is_empty() && Instant::now() < deadline {
        receiver.sweep();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(receiver.pending(&local).is_empty());
    assert!(matches!(
        task.await.unwrap(),
        Response::Hook {
            decision: Decision::None
        }
    ));
    assert_eq!(next_resolved(&mut events).await, (gid.clone(), "terminal"));
    let (_, row) = call(&fx, &cookie, Method::GET, &endpoint, None).await;
    assert!(row["pending_decision"].is_null(), "{row}");
    assert_eq!(row["status"], "unknown", "放 hold 不改状态：{row}");
    assert!(fx.rt.inputs.lock().unwrap().is_empty(), "不注入键击");
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("{endpoint}/input"),
        Some(json!({ "kind": "decision", "decision": "allow" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "no_pending_decision");
}

// ── 宿主文本通道（agora-t5kf.1；`src/hook/input.rs`）──────────────────────────────────────

/// 登记一条 external 的 pi 行：载荷里带 `input_channel: 1`（扩展的自报，ADR-002 D11）。
fn register_pi_with_channel(receiver: &Arc<Receiver>, home: &Path, agent: &str) -> String {
    let path = Inbox::new(home)
        .write(&external_delivery(
            "pi",
            agent,
            json!({
                "hook_event_name": "session_start",
                "session_id": agent,
                "cwd": "/work/agora",
                "mode": "tui",
                "reason": "startup",
                "idle": true,
                "input_channel": 1
            }),
        ))
        .unwrap();
    receiver.ingest(&path).unwrap();
    agent.to_owned()
}

#[tokio::test]
async fn host_text_channel_queues_the_prompt_and_waits_for_the_extension_ack() {
    // 无句柄的 pi 行自报输入通道 → text 走 input/ 队列；扩展（测试里是消费者）把
    // `<id>.json` 改名 `<id>.done` 就是 ack，API 到那时才回 200。
    let (mut fx, receiver, home) = with_hooks(Duration::from_secs(30));
    fx.state.input_ack_wait = Duration::from_secs(3);
    let cookie = fx.cookie();
    register_pi_with_channel(&receiver, home.path(), "pi-1");
    let row = fx
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.record.agent_type == "pi")
        .unwrap();
    let id = row.record.id.clone();
    assert_eq!(
        row.text_via, "host",
        "扩展自报 input_channel → text_via = host"
    );
    assert_eq!(row.respond_via, "terminal", "决策那一栏不受文本通道影响");

    // 扩展的替身：轮询自己那个队列目录，见到 `.json` 就改名 `.done`，把文本带回来。
    // 目录键是**宿主自己的**会话 id（`agent_session_id` = pi 载荷里的 session_id），不是 agora id
    // ——扩展只知道自己那个（2026-10-07 真机代检：daemon 拿 agora id 当键时两边永远碰不上）。
    let dir = fx.sessions.input_dir().unwrap();
    let sid = "pi-1".to_owned();
    let consumer = tokio::spawn(async move {
        let session_dir = agora::hook::input::session_dir(&dir, &sid);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(entries) = std::fs::read_dir(&session_dir) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.ends_with(".json") {
                        let text = std::fs::read_to_string(e.path()).unwrap();
                        std::fs::rename(e.path(), e.path().with_extension("done")).unwrap();
                        return text;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("队列里没等到 .json");
    });

    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{id}/input"),
        Some(json!({ "kind": "text", "data": "只回一个字：好" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        consumer.await.unwrap(),
        "只回一个字：好",
        "队列文件里就是手机发的那句"
    );
}

#[tokio::test]
async fn host_text_channel_times_out_when_the_extension_does_not_take_it() {
    let (mut fx, receiver, home) = with_hooks(Duration::from_secs(30));
    fx.state.input_ack_wait = Duration::from_millis(80);
    let cookie = fx.cookie();
    register_pi_with_channel(&receiver, home.path(), "pi-2");
    let id = fx
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.record.agent_type == "pi")
        .unwrap()
        .record
        .id
        .clone();
    let dir = agora::hook::input::session_dir(&fx.sessions.input_dir().unwrap(), "pi-2");

    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{id}/input"),
        Some(json!({ "kind": "text", "data": "go" })),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(body["error"], "host_timeout");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "没人取走的那件要清掉：用户重试不该跑两遍"
    );
}

#[tokio::test]
async fn a_handleless_row_without_an_input_channel_still_says_no_runtime() {
    // Claude / Codex / Grok 没有宿主注入能力（旧扩展也一样）：维持 MISSION §5.5 的只读口径。
    let (fx, receiver, home) = with_hooks(Duration::from_secs(30));
    let cookie = fx.cookie();
    let path = Inbox::new(home.path())
        .write(&external_delivery(
            "claude",
            "claude-1",
            json!({ "hook_event_name": "SessionStart", "session_id": "claude-1" }),
        ))
        .unwrap();
    receiver.ingest(&path).unwrap();
    let row = fx
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.record.agent_type == "claude")
        .unwrap();
    assert_eq!(row.text_via, "none");
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{}/input", row.record.id),
        Some(json!({ "kind": "text", "data": "go" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "no_runtime");
}
