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
    delivery_for("claude", agent_session, payload, agent_env, runtime_env)
}

fn delivery_for(
    host: &str,
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
            host: host.into(),
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

/// Claude 与 Codex 的 SessionEnd 同形（CamelCase 事件名、snake_case 键）；Codex 退出 / 结束线程的
/// reason 是 `other`，Claude 在提示符上退出是 `prompt_input_exit`。
fn session_end(agent_session: &str, reason: &str) -> Value {
    json!({ "hook_event_name": "SessionEnd", "session_id": agent_session, "cwd": "/work/devcenter", "reason": reason })
}

/// Codex Desktop 线程的信封形态（2026-09-05 真投递件）：agent_env 带 `CODEX_INTERNAL_ORIGINATOR_OVERRIDE`，
/// ppid 是所有线程共用的 app-server。测试用自己的进程号当那个"永远活着"的父进程。
fn codex_desktop(agent_session: &str, payload: Value) -> Delivery {
    let env = [(
        "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
        "Codex Desktop".to_owned(),
    )];
    let mut d = delivery_for("codex", agent_session, payload, &env, &[]);
    d.envelope.ppid = std::process::id();
    d
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
async fn session_end_for_an_unseen_session_registers_nothing() {
    // agora-vfi 验收第一条：hook 装好之前起的会话退出、Codex Desktop 结束一个线程——agora 收到的唯一
    // 一条是 SessionEnd。登记只会造一行永远没有后续事件的僵尸（2026-09-05 侧栏那行 devcenter）。
    // 关掉 locate_external 的"只有 SessionEnded 不登记"→ 行数断言红。
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    let (_, before) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert_eq!(before["sessions"].as_array().unwrap().len(), 0, "{before}");

    let d = codex_desktop("ghost-1", session_end("ghost-1", "other"));
    let path = Inbox::new(home.path()).write(&d).unwrap();
    let received = receiver
        .ingest(&path)
        .unwrap()
        .expect("不是旧 epoch，要记账");
    assert_eq!(
        received.session_key, "codex:ghost-1",
        "没登记就没有 agora id：挂起键退回 <host>:<agent_session_id>"
    );
    assert_eq!(received.event.as_deref(), Some("SessionEnd"));
    let (_, after) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert_eq!(after["sessions"].as_array().unwrap().len(), 0, "{after}");
    assert!(fx
        .sessions
        .find_by_agent_session("codex", "ghost-1")
        .unwrap()
        .is_none());
    // 文件照常进 done，账本里有这条。
    assert!(!path.exists(), "应用完要移出 inbox");
    let done = Inbox::new(home.path()).completed().unwrap();
    assert!(
        done.iter().any(|p| p.file_name() == path.file_name()),
        "{done:?}"
    );
    assert_eq!(receiver.received_for("codex:ghost-1").len(), 1);

    // Claude 同一规则（Terminal.app 里在装 hook 之前起的会话，此刻退出）。
    let d = delivery(
        "ghost-2",
        session_end("ghost-2", "prompt_input_exit"),
        &[],
        &[],
    );
    assert_eq!(
        ingest(&receiver, home.path(), &d).as_deref(),
        Some("claude:ghost-2")
    );
    assert_eq!(fx.sessions.list().unwrap().len(), 0);

    // 反面：同一个会话随后要是来了 SessionStart，照常登记——别把登记整个关掉。
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("ghost-2", session_start("ghost-2"), &[], &[]),
    )
    .unwrap();
    assert_eq!(fx.sessions.record(&id).unwrap().origin, Origin::External);
    let (_, after) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert_eq!(after["sessions"].as_array().unwrap().len(), 1, "{after}");
}

#[tokio::test]
async fn external_session_ends_on_session_end_hook() {
    // agora-vfi 验收第二条：agora 见过的 Codex Desktop 线程结束时，行在 ≤ 1 tick 内变 FINISHED。
    // Desktop 线程没有可信 pid（ppid 是共用 app-server，永远活着），行只跟 hook 走。
    // 关掉 Machine::apply 的 SessionEnded → FINISHED → 最后一段断言红（停在 turn_done）；
    // 关掉 Codex::agent_pid 的 Desktop 判定 → `alive` 断言红（把测试进程当成了 agent）。
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    let id = ingest(
        &receiver,
        home.path(),
        &codex_desktop("thread-1", session_start("thread-1")),
    )
    .unwrap();
    let rec = fx.sessions.record(&id).unwrap();
    assert_eq!(rec.origin, Origin::External);
    assert_eq!(rec.agent_type, "codex");
    assert_eq!(rec.display_name, "agora");
    let path = format!("/api/sessions/{}:{}", common::NODE, id);
    let (status, body) = call(&fx, &cookie, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "starting", "{body}");
    assert_eq!(
        body["alive"], false,
        "Desktop 的 ppid 是共用 app-server，不能当 alive：{body}"
    );

    // 一轮做完：TURN_DONE（hook 说什么就是什么）。
    ingest(
        &receiver,
        home.path(),
        &codex_desktop(
            "thread-1",
            json!({ "hook_event_name": "Stop", "session_id": "thread-1", "last_assistant_message": "done" }),
        ),
    );
    let (_, body) = call(&fx, &cookie, Method::GET, &path, None).await;
    assert_eq!(body["status"], "turn_done", "{body}");

    // 用户在 Desktop 里结束这个线程：SessionEnd(reason=other)。下一 tick 就是 FINISHED。
    ingest(
        &receiver,
        home.path(),
        &codex_desktop("thread-1", session_end("thread-1", "other")),
    );
    let (_, body) = call(&fx, &cookie, Method::GET, &path, None).await;
    assert_eq!(body["status"], "finished", "{body}");
    assert_eq!(body["source"], "hook", "{body}");
    assert!(
        body["reason"].as_str().unwrap_or_default().contains("hook"),
        "{body}"
    );
    assert_eq!(fx.sessions.list().unwrap().len(), 1, "结束不造新行");
}

#[tokio::test]
async fn external_row_ends_when_claude_clears_to_a_new_id() {
    // agora-s3r（2026-09-07 现场）：Terminal.app 裸跑的 Claude 里 /clear，旧 id 发 SessionEnd(reason=clear)，
    // 新 id 的 SessionStart 另起一行，旧行没有 pane、进程还活着（同一个 claude 在跑新会话），从此钉在
    // RUNNING（侧栏 working 16 min+）。receiver 对无句柄的 external 行把 `clear` 当普通结束。
    // 关掉 `clear_ends_external_row` 的改写 → 第一段 finished 断言红（停在 running）。
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    // 与现场一致：CLAUDE_PID 是一个活着的进程（测试进程自己），进程层给不出"结束"。
    let env = [("CLAUDE_PID", std::process::id().to_string())];
    let old = ingest(
        &receiver,
        home.path(),
        &delivery("old-id", session_start("old-id"), &env, &[]),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "old-id",
            json!({ "hook_event_name": "UserPromptSubmit", "session_id": "old-id", "prompt": "/simplify" }),
            &env,
            &[],
        ),
    );
    let old_path = format!("/api/sessions/{}:{}", common::NODE, old);
    let (_, body) = call(&fx, &cookie, Method::GET, &old_path, None).await;
    assert_eq!(body["status"], "running", "{body}");

    ingest(
        &receiver,
        home.path(),
        &delivery("old-id", session_end("old-id", "clear"), &env, &[]),
    );
    let (_, body) = call(&fx, &cookie, Method::GET, &old_path, None).await;
    assert_eq!(body["status"], "finished", "旧行到此为止：{body}");
    assert_eq!(body["source"], "hook", "{body}");

    // 同一秒到的新 id：另一行，从 STARTING 起；旧行不被它救活。
    let new = ingest(
        &receiver,
        home.path(),
        &delivery(
            "new-id",
            json!({ "hook_event_name": "SessionStart", "session_id": "new-id", "cwd": "/work/agora", "source": "clear" }),
            &env,
            &[],
        ),
    )
    .unwrap();
    assert_ne!(
        new, old,
        "无句柄的 external 行以 agent id 为身份，新 id 是新行"
    );
    let new_path = format!("/api/sessions/{}:{}", common::NODE, new);
    let (_, body) = call(&fx, &cookie, Method::GET, &new_path, None).await;
    assert_eq!(body["status"], "starting", "{body}");
    let (_, body) = call(&fx, &cookie, Method::GET, &old_path, None).await;
    assert_eq!(body["status"], "finished", "旧行仍是结束：{body}");
    assert_eq!(fx.sessions.list().unwrap().len(), 2);
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
