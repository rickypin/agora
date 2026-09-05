//! dvh.12：agora 没起过的会话经 hook 自己出现（MISSION §5.4）。
//! 无运行时句柄 → external 行，存活看 agent 进程号；信封里的 pane 能定位到采纳 socket → adopted 行，有终端。

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use agora::adapter::Decision;
use agora::hook::{Delivery, Envelope, Inbox, Receiver};
use agora::local::Response;
use agora::session::Origin;
use agora::status::{Source, Status};

use common::{Fx, HOST};

fn delivery(
    agent_session: &str,
    payload: Value,
    agent_env: &[(&str, String)],
    runtime_env: &[(&str, String)],
) -> Delivery {
    // 每条投递须有不同文件名；真实 hook 每进程只写一条，测试用递增时间模拟。
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let to_map = |kv: &[(&str, String)]| -> BTreeMap<String, String> {
        kv.iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    };
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: None,
            agora_epoch: None,
            agent_session_id: agent_session.into(),
            agent_env: to_map(agent_env),
            runtime_env: to_map(runtime_env),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        },
        payload,
    }
}

fn session_start(agent_session: &str) -> Value {
    json!({ "hook_event_name": "SessionStart", "session_id": agent_session, "cwd": "/work/agora", "source": "startup" })
}

fn with_hooks() -> (Fx, Arc<Receiver>, tempfile::TempDir) {
    let mut fx = Fx::new();
    let home = tempfile::tempdir().unwrap();
    let receiver = Arc::new(
        Receiver::new(home.path(), fx.sessions.clone()).with_hold_timeout(Duration::from_secs(30)),
    );
    receiver.attach_events(fx.state.events.clone(), fx.state.node.clone());
    fx.state.hooks = Some(receiver.clone());
    (fx, receiver, home)
}

fn ingest(receiver: &Receiver, home: &std::path::Path, d: &Delivery) -> Option<String> {
    let path = Inbox::new(home).write(d).unwrap();
    receiver.ingest(&path).unwrap().map(|r| r.session_key)
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn hook_without_terminal_registers_an_external_session() {
    let (fx, receiver, home) = with_hooks();
    // 存活线索：一个真进程当"agent"。
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    let env = [("CLAUDE_PID", pid.to_string())];

    let id = ingest(
        &receiver,
        home.path(),
        &delivery("ext-1", session_start("ext-1"), &env, &[]),
    )
    .unwrap();
    let rec = fx.sessions.record(&id).unwrap();
    assert_eq!(rec.origin, Origin::External);
    assert_eq!(rec.runtime_ref, None);
    assert_eq!(rec.agent_type, "claude");
    assert_eq!(rec.agent_session_id.as_deref(), Some("ext-1"));
    assert_eq!(rec.working_directory.as_deref(), Some("/work/agora"));
    assert_eq!(rec.display_name, "agora");

    // 同一 agent 会话的后续事件落到同一行，且进状态机（hook 来源）。
    let again = ingest(
        &receiver,
        home.path(),
        &delivery(
            "ext-1",
            json!({ "hook_event_name": "UserPromptSubmit", "session_id": "ext-1", "prompt": "hi" }),
            &env,
            &[],
        ),
    )
    .unwrap();
    assert_eq!(again, id);
    let view = fx.sessions.get(&id).unwrap();
    assert_eq!(
        view.assessment.status,
        Status::Running,
        "{:?}",
        view.assessment
    );
    assert_eq!(view.assessment.source, Source::Hook);
    assert!(view.alive, "CLAUDE_PID 活着 → alive");
    assert_eq!(fx.sessions.list().unwrap().len(), 1);

    // agent 进程没了：没有退出码，只报结束。
    child.kill().unwrap();
    child.wait().unwrap();
    let view = fx.sessions.get(&id).unwrap();
    assert!(!view.alive);
    assert_eq!(
        view.assessment.status,
        Status::Finished,
        "{:?}",
        view.assessment
    );

    // 没自报 id 的事件不造行。
    let none = ingest(
        &receiver,
        home.path(),
        &delivery(
            "unknown",
            json!({ "hook_event_name": "SessionStart", "source": "startup" }),
            &[],
            &[],
        ),
    )
    .unwrap();
    assert_eq!(none, "claude:unknown");
    assert_eq!(fx.sessions.list().unwrap().len(), 1);
}

#[tokio::test]
async fn external_session_answers_permission_via_the_hook_and_refuses_text() {
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("ext-2", session_start("ext-2"), &[], &[]),
    )
    .unwrap();
    let path = Inbox::new(home.path())
        .write(&delivery(
            "ext-2",
            json!({ "hook_event_name": "PermissionRequest", "session_id": "ext-2", "tool_name": "Bash", "tool_input": { "command": "ls" } }),
            &[],
            &[],
        ))
        .unwrap();
    let r = receiver.clone();
    let task = tokio::spawn(async move { r.wake(&path).await });
    for _ in 0..100 {
        if !receiver.pending(&id).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        receiver.pending(&id),
        vec!["Bash".to_owned()],
        "挂起键是 agora 的会话 id"
    );
    let gid = format!("{}:{}", common::NODE, id);
    let (status, body) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "waiting");
    assert_eq!(body["origin"], "external");
    assert_eq!(body["respond_via"], "hook");

    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "decision", "decision": "allow" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        task.await.unwrap(),
        Response::Hook {
            decision: Decision::Allow
        }
    );

    // 没有运行时句柄：text 走不了 PTY。
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/input"),
        Some(json!({ "kind": "text", "data": "next\n" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "no_runtime");
}

#[tokio::test]
async fn hook_from_an_adoptable_pane_adopts_the_runtime_session() {
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    fx.rt.insert("fake:default:manual", true, None, false);
    fx.rt
        .panes
        .lock()
        .unwrap()
        .insert("%7".into(), "fake:default:manual".into());
    let (_, list) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert_eq!(list["unregistered"].as_array().unwrap().len(), 1);
    assert!(list["unregistered"][0]["agent_hint"].is_null());

    let env = [
        ("TMUX", "/tmp/tmux-501/default,123,0".to_owned()),
        ("TMUX_PANE", "%7".to_owned()),
    ];
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("ext-3", session_start("ext-3"), &[], &env),
    )
    .unwrap();
    let rec = fx.sessions.record(&id).unwrap();
    assert_eq!(rec.origin, Origin::Adopted);
    assert_eq!(rec.runtime_ref.as_deref(), Some("fake:default:manual"));
    assert_eq!(rec.agent_type, "claude", "hook 证明了里面是谁");
    assert_eq!(rec.agent_session_id.as_deref(), Some("ext-3"));

    let (_, list) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert!(list["unregistered"].as_array().unwrap().is_empty());
    assert_eq!(list["sessions"][0]["respond_via"], "hook");
    assert_eq!(list["sessions"][0]["managed"], false);

    // pane 不在可采纳 socket 上（locate 答 None）→ 退回 external。
    let env = [
        ("TMUX", "/tmp/tmux-501/other,1,0".to_owned()),
        ("TMUX_PANE", "%8".to_owned()),
    ];
    let id2 = ingest(
        &receiver,
        home.path(),
        &delivery("ext-4", session_start("ext-4"), &[], &env),
    )
    .unwrap();
    assert_eq!(fx.sessions.record(&id2).unwrap().origin, Origin::External);
}

#[tokio::test]
async fn user_chosen_agent_type_beats_the_process_hint() {
    let fx = Fx::new();
    let cookie = fx.cookie();
    fx.rt.insert("fake:default:manual", true, None, false);
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions/adopt",
        Some(json!({ "runtime_ref": "fake:default:manual", "agent_type": "codex" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["agent_type"], "codex");
}
