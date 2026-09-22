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
use agora::clock;
use agora::events::{Differ, Event};
use agora::hook::{Delivery, Envelope, Inbox, Receiver};
use agora::local::Response;
use agora::runtime::Runtime;
use agora::session::{Db, Origin, SessionManager};
use agora::status::{ProcessState, Source, Status};

use common::{FakeRuntime, Fx, HOST};

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
    // 每条投递须有不同文件名；真实 hook 每进程只写一条，测试用递增时间模拟。时刻从"现在"起算：
    // external 行的进程号要与报来它的 hook 时刻对一下（进程不得晚于 hook，agora-tql），从 1 ms 起算
    // 会把测试进程当成比 hook 还新的复用者。
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
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
            received_unix_ms: now_ms + SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        },
        payload,
    }
}

/// 交互会话的 SessionStart：带 `model` + `scratchpad_dir`（testdata/claude/2.1.261/hooks/
/// turn_complete.jsonl 逐键）。这两个键是无头判据的一部分（agora-5gg.20），不写的合成载荷会被
/// 登记成 `headless` 行——要验无头就明写 `headless_session_start`，别拿漏了两个键当无头。
fn session_start(agent_session: &str) -> Value {
    json!({ "hook_event_name": "SessionStart", "session_id": agent_session, "cwd": "/work/agora",
        "source": "startup", "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch" })
}

/// 无头一轮（`claude -p`）的 SessionStart：只剩公共键（testdata/claude/2.1.270/hooks/headless.jsonl）。
fn headless_session_start(agent_session: &str) -> Value {
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
        body["process"], "unknown",
        "Desktop 没有可信进程号，三值要能说'不知道'（agora-5gg.18）：{body}"
    );
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
    assert_eq!(
        body["process"], "unknown",
        "TURN_DONE 而进程在不在没人知道（盘点 B2 那一格）不能写成没了：{body}"
    );

    // 用户在 Desktop 里结束这个线程：SessionEnd(reason=other)。下一 tick 就是 FINISHED。
    ingest(
        &receiver,
        home.path(),
        &codex_desktop("thread-1", session_end("thread-1", "other")),
    );
    let (_, body) = call(&fx, &cookie, Method::GET, &path, None).await;
    assert_eq!(body["status"], "finished", "{body}");
    assert_eq!(body["source"], "hook", "{body}");
    assert_eq!(
        body["process"], "gone",
        "对话结束即不再谈进程（Q4）：{body}"
    );
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
            json!({ "hook_event_name": "SessionStart", "session_id": "new-id", "cwd": "/work/agora", "source": "clear",
                "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch" }),
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

#[tokio::test]
async fn legacy_checkpoint_of_a_handleless_external_row_is_rebuilt_from_the_archive() {
    // agora-tql：agora-s3r（2026-09-07）之前写下的检查点停在修复前的结论——无句柄 external 行的
    // SessionEnd(reason=clear) 当时不改状态，行钉在 TURN_DONE；s3r 只管之后到达的事件，去重水位又
    // 拒掉归档里那条 SessionEnd。守卫：v1 检查点 + 归档里还有这一行的投递 → 丢掉检查点、从归档
    // 按序重建（clear 同样改写成普通结束）→ FINISHED；v1 检查点但归档里什么都没有 → 原样恢复。
    // 关掉 restore_archive 的 legacy 分支 → 第一段 finished 断言红（停在 turn_done）；
    // 关掉重建路上的 clear_ends_external_row → 同一断言红。
    let (fx, receiver, home) = with_hooks();
    let env = [("CLAUDE_PID", std::process::id().to_string())];
    let inbox = Inbox::new(home.path());
    let stop = |sid: &str| json!({ "hook_event_name": "Stop", "session_id": sid, "last_assistant_message": "done" });
    let old = ingest(
        &receiver,
        home.path(),
        &delivery("old-id", session_start("old-id"), &env, &[]),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery("old-id", stop("old-id"), &env, &[]),
    );
    // lone-id 是另一个进程的对话：同一进程报来第二个对话会把 old-id 取代掉（superseded），那是另一条规则。
    let mut lone_proc = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let lone_env = [("CLAUDE_PID", lone_proc.id().to_string())];
    let lone = ingest(
        &receiver,
        home.path(),
        &delivery("lone-id", session_start("lone-id"), &lone_env, &[]),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery("lone-id", stop("lone-id"), &lone_env, &[]),
    );
    // 修复前的 daemon 消费了 SessionEnd(clear)：文件进了 done/，检查点没变。这里不经 receiver，直接落
    // 到 done/ 模拟；再把两份检查点降成 v1（去掉 agent_process）。
    let clear = inbox
        .write(&delivery(
            "old-id",
            session_end("old-id", "clear"),
            &env,
            &[],
        ))
        .unwrap();
    inbox.done(&clear).unwrap();
    for id in [&old, &lone] {
        let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let path = home.path().join("hooks/state").join(format!("{key}.json"));
        let mut cp: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(cp["status"], Value::Null);
        assert_eq!(cp["current"]["status"], "turn_done", "{cp}");
        cp["version"] = json!(1);
        cp.as_object_mut().unwrap().remove("agent_process");
        std::fs::write(&path, cp.to_string()).unwrap();
    }
    // lone-id 的归档清掉：只剩检查点。
    for path in inbox.completed().unwrap() {
        if path.to_string_lossy().contains("lone-id") {
            std::fs::remove_file(path).unwrap();
        }
    }

    // 重启。
    let restarted = Arc::new(agora::session::SessionManager::new(
        fx.db.clone(),
        fx.rt.clone() as Arc<dyn agora::runtime::Runtime>,
    ));
    Receiver::new(home.path(), restarted.clone())
        .replay()
        .unwrap();
    let v = restarted.get(&old).unwrap();
    assert_eq!(
        v.assessment.status,
        Status::Finished,
        "从归档重建，clear 当普通结束：{:?}",
        v.assessment
    );
    assert_eq!(v.assessment.source, Source::Hook);
    let v = restarted.get(&lone).unwrap();
    assert_eq!(
        v.assessment.status,
        Status::TurnDone,
        "没有归档的 v1 检查点原样恢复：{:?}",
        v.assessment
    );
    // 重建后的检查点是当前版本（v3）且带回了进程号。
    let key: String = old.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let cp: Value = serde_json::from_slice(
        &std::fs::read(home.path().join("hooks/state").join(format!("{key}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(cp["version"], 3, "{cp}");
    assert_eq!(cp["agent_process"]["pid"], std::process::id(), "{cp}");
    lone_proc.kill().unwrap();
    lone_proc.wait().unwrap();
}

#[tokio::test]
async fn a_new_conversation_on_the_same_process_supersedes_the_old_external_row() {
    // agora-tql（2026-09-08 现场）：一个 grok 进程先后跑了三个对话 id（/clear 不发 session_end），
    // 三行都 TURN_DONE 且 alive——进程确实活着，但它早就在跑别的对话了。守卫：同一宿主、同一进程号
    // 的两个无句柄 external 行，后到的取代先到的（FINISHED，reason superseded）；daemon 重启恢复后
    // 同样收敛。关掉 supersede_external_rows 的两个调用点 → 两段 finished 断言各自红。
    // 两条投递只差 1–2 ms：`seen_at` 若是秒级，同秒平局落到 HashMap 顺序，本用例稳定红（agora-2nh；
    // 把 note_external_pid 传的时刻改回 `received_unix_ms / 1000` 就能看见）。
    let (fx, receiver, home) = with_hooks();
    let env = [("CLAUDE_PID", std::process::id().to_string())];
    let stop = |sid: &str| json!({ "hook_event_name": "Stop", "session_id": sid, "last_assistant_message": "done" });
    let first = ingest(
        &receiver,
        home.path(),
        &delivery("conv-1", session_start("conv-1"), &env, &[]),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery("conv-1", stop("conv-1"), &env, &[]),
    );
    assert_eq!(
        fx.sessions.get(&first).unwrap().assessment.status,
        Status::TurnDone
    );
    // 同一进程报来第二个对话：旧行到此为止，新行照常。
    let second = ingest(
        &receiver,
        home.path(),
        &delivery("conv-2", session_start("conv-2"), &env, &[]),
    )
    .unwrap();
    assert_ne!(first, second);
    let v = fx.sessions.get(&first).unwrap();
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    assert_eq!(v.assessment.source, Source::Hook);
    // Q4（agora-5gg.18）：这个 pid 还活着（它正跑着 second 那条对话），旧行也一律报 gone。
    // 旧代码在这里是 finished + alive:true（盘点 B1，Mac 现场 10 行）。去掉 FINISHED/FAILED
    // 那一支的提前返回 → 这两条各自红。
    assert_eq!(v.process, ProcessState::Gone, "{:?}", v.assessment);
    assert!(!v.alive, "旧布尔是 process == alive 的投影");
    assert!(
        v.assessment
            .reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("superseded"),
        "{:?}",
        v.assessment
    );
    assert_eq!(
        fx.sessions.get(&second).unwrap().assessment.status,
        Status::Starting
    );
    // 另一个进程的对话不受影响。
    let mut other = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let other_env = [("CLAUDE_PID", other.id().to_string())];
    let third = ingest(
        &receiver,
        home.path(),
        &delivery("conv-3", session_start("conv-3"), &other_env, &[]),
    )
    .unwrap();
    assert_eq!(
        fx.sessions.get(&second).unwrap().assessment.status,
        Status::Starting,
        "别的进程的新对话不取代它"
    );
    assert_eq!(
        fx.sessions.get(&third).unwrap().assessment.status,
        Status::Starting
    );

    // 重启恢复：把 conv-2 的检查点改成 turn_done 之前的老样子——直接构造"两行共享一个进程号、都没结束"
    // 的检查点局面：conv-1 的 current 改回 turn_done。重放完 supersede 再跑一遍 → conv-1 仍 FINISHED。
    let key: String = first
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = home.path().join("hooks/state").join(format!("{key}.json"));
    let mut cp: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cp["current"]["status"] = json!("turn_done");
    std::fs::write(&path, cp.to_string()).unwrap();
    let restarted = Arc::new(agora::session::SessionManager::new(
        fx.db.clone(),
        fx.rt.clone() as Arc<dyn agora::runtime::Runtime>,
    ));
    Receiver::new(home.path(), restarted.clone())
        .replay()
        .unwrap();
    let v = restarted.get(&first).unwrap();
    assert_eq!(
        v.assessment.status,
        Status::Finished,
        "重启后同样收敛：{:?}",
        v.assessment
    );
    assert_eq!(
        restarted.get(&second).unwrap().assessment.status,
        Status::Starting
    );
    other.kill().unwrap();
    other.wait().unwrap();
}

#[tokio::test]
async fn hook_session_end_survives_the_agent_process_going_away() {
    // agora-rzh（2026-09-08 现场：6 行 external 探针里 5 行宿主发了 SessionEnd，GET /api/sessions 却
    // 6 行全是 `external process gone (no exit status)`，与唯一没发 SessionEnd 的那行——Codex 关窗口——
    // 分不开）。守卫：有进程号的 external 行先收 SessionEnd(hook) 变 FINISHED，随后探活报进程消失，
    // 行的 source 仍 hook、reason 仍 `session ended (hook)`、alive 变假；对照：从没收到 SessionEnd、
    // 只有进程消失的行 reason 是 `external process gone`、source process。
    // 关掉 Machine::observe 第 1 步的 process_fact_is_no_better 判断 → 第一行的 reason 断言红。
    // 关掉 agora-5gg.18 的「FINISHED / FAILED 一律 gone」→ SessionEnd 后那一组 process/alive 红
    // （那一瞬进程还活着，旧代码报的是 finished + alive:true）。
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    let spawn = || {
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap()
    };
    let mut by_hand = spawn();
    let mut by_hup = spawn();
    let env_of = |child: &std::process::Child| [("CLAUDE_PID", child.id().to_string())];
    let stop = |sid: &str| json!({ "hook_event_name": "Stop", "session_id": sid, "last_assistant_message": "done" });

    let hand_env = env_of(&by_hand);
    let hand = ingest(
        &receiver,
        home.path(),
        &delivery("by-hand", session_start("by-hand"), &hand_env, &[]),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery("by-hand", stop("by-hand"), &hand_env, &[]),
    );
    let hup_env = env_of(&by_hup);
    let hup = ingest(
        &receiver,
        home.path(),
        &delivery("by-hup", session_start("by-hup"), &hup_env, &[]),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery("by-hup", stop("by-hup"), &hup_env, &[]),
    );
    let path_of = |id: &str| format!("/api/sessions/{}:{}", common::NODE, id);
    for id in [&hand, &hup] {
        let (status, body) = call(&fx, &cookie, Method::GET, &path_of(id), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "turn_done", "{body}");
        assert_eq!(body["alive"], true, "{body}");
        assert_eq!(body["process"], "alive", "探活拿到了进程号：{body}");
    }

    // 人在提示符上两次 Ctrl+C：Claude 发 SessionEnd(prompt_input_exit)，进程随即退出。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "by-hand",
            session_end("by-hand", "prompt_input_exit"),
            &hand_env,
            &[],
        ),
    );
    let (_, body) = call(&fx, &cookie, Method::GET, &path_of(&hand), None).await;
    assert_eq!(body["status"], "finished", "{body}");
    assert_eq!(body["reason"], "session ended (hook)", "{body}");
    // 进程号照报（agora-5gg.5）：`process: gone` 说的是"对话结束了不再谈进程"，`pid` 说的是
    // "这个对话落在哪个进程上"——排障时要拿它去 `ps` 对，不能因为行结束了就抹成 null。
    assert_eq!(body["pid"], by_hand.id(), "号还探得到就照报：{body}");
    // Q4（agora-5gg.18）：这一秒 by_hand 还活着，行上却要说 gone——对话结束就不再谈进程。
    // 旧代码在这里是 finished + alive:true（盘点 B1，Mac 2026-09-18 现场 10 行）。
    assert_eq!(body["process"], "gone", "进程还在也要说结束：{body}");
    assert_eq!(body["alive"], false, "旧布尔跟着三值走：{body}");
    let since = body["status_since"].as_i64().unwrap();
    by_hand.kill().unwrap();
    by_hand.wait().unwrap();
    let (_, body) = call(&fx, &cookie, Method::GET, &path_of(&hand), None).await;
    assert_eq!(body["status"], "finished", "{body}");
    assert_eq!(body["alive"], false, "进程确实没了：{body}");
    assert_eq!(body["process"], "gone", "{body}");
    assert_eq!(body["source"], "hook", "进程消失不换掉 hook 的说法：{body}");
    assert_eq!(body["reason"], "session ended (hook)", "{body}");
    assert_eq!(
        body["status_since"], since,
        "结束的起点仍是 SessionEnd 那一刻：{body}"
    );

    // 对照：窗口被关、宿主一个事件都没发（Codex 的行为），只有探活能说它结束了。
    by_hup.kill().unwrap();
    by_hup.wait().unwrap();
    let (_, body) = call(&fx, &cookie, Method::GET, &path_of(&hup), None).await;
    assert_eq!(body["status"], "finished", "{body}");
    assert_eq!(body["alive"], false, "{body}");
    assert_eq!(body["process"], "gone", "{body}");
    assert_eq!(body["source"], "process", "{body}");
    assert_eq!(
        body["reason"], "external process gone (no exit status)",
        "{body}"
    );
    // 探活失败 → pid 回 null（agora-5gg.5）：那个号在 `ps` 里已经属于别人（或空着），
    // 写进行里就是再一次对不上号。
    assert_eq!(body["pid"], Value::Null, "号没了不报一个假号：{body}");
}

#[tokio::test]
async fn an_external_row_reports_the_agent_process_it_probes_as_its_pid() {
    // agora-5gg.5（2026-09-18 盘点）：external 行的 `pid` 在 API 里恒 null——`SessionView::pid` 只从
    // 运行时 pane 取号，而无句柄 external 行的探活恰恰拿的是 hook 检查点里那个 `agent_process.pid`。
    // 现场排障时 `GET /api/sessions` 上这行"没有进程"，`ps` 里那个 claude 活得好好的，两边对不上号。
    // 守卫三格：号活着 → `pid` == 信封报来的号（与探活同源）；号没了 → null（不报已被复用的号）；
    // 没有可信进程号（Codex Desktop）→ null 且 `process: unknown`。
    // 改坏一次看红：把 manager 的 `pid: rt.and_then(|s| s.pid).or(external_pid)` 退回
    // `rt.and_then(|s| s.pid)` → 第一格红（`hook_session_end_survives_the_agent_process_going_away` 里
    // 那条 pid 断言同红）；把探活失败那一支的第三值改成 Some(p.pid) → 第二格红。
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let env = [("CLAUDE_PID", child.id().to_string())];
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("pid-1", session_start("pid-1"), &env, &[]),
    )
    .unwrap();
    let row_of = |body: &Value| -> Value {
        body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["local_id"] == json!(&id))
            .cloned()
            .unwrap_or_else(|| panic!("列表里没有 {id}: {body}"))
    };

    // 第一格：无句柄、探活拿到的就是信封里那个号，行上要说得出号。
    let (status, body) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = row_of(&body);
    assert_eq!(row["origin"], "external", "{row}");
    assert_eq!(row["runtime_ref"], Value::Null, "无句柄：{row}");
    assert_eq!(
        row["pid"],
        child.id(),
        "pid 要等于信封报来的 agent 进程号：{row}"
    );
    assert_eq!(row["process"], "alive", "{row}");

    // 第二格：宿主自己结束了对话（hook 先说），进程随后消失（探活失败）。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "pid-1",
            session_end("pid-1", "prompt_input_exit"),
            &env,
            &[],
        ),
    );
    child.kill().unwrap();
    child.wait().unwrap();
    let (_, body) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    let row = row_of(&body);
    assert_eq!(row["status"], "finished", "{row}");
    assert_eq!(row["process"], "gone", "{row}");
    assert_eq!(
        row["pid"],
        Value::Null,
        "探活失败填 null，不报一个可能已被复用的号：{row}"
    );

    // 第三格：没有可信进程号的行（Desktop 的 ppid 是所有线程共用的 app-server）不编一个号出来。
    let desk = ingest(
        &receiver,
        home.path(),
        &codex_desktop("thread-1", session_start("thread-1")),
    )
    .unwrap();
    let (_, body) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    let row = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["local_id"] == json!(&desk))
        .cloned()
        .unwrap_or_else(|| panic!("列表里没有 {desk}: {body}"));
    assert_eq!(row["process"], "unknown", "{row}");
    assert_eq!(row["pid"], Value::Null, "无可信进程号就是 null：{row}");
}

/// 把信封时刻拨到 `secs_ago` 秒以前，并返回换算后的 unix 秒（整秒，`ms % 1000 == 0`，断言
/// `created_at` / `ended_at` 时可以逐字比）。投递件的文件名跟着信封时刻走（`Inbox::write`），
/// 所以 `apply_delivered_hook` 取的事件时刻也就是它——重放场景的真实形状。
fn backdate(mut d: Delivery, secs_ago: i64) -> (Delivery, i64) {
    let at = clock::now_secs() - secs_ago;
    d.envelope.received_unix_ms = (at * 1000) as u64;
    (d, at)
}

#[tokio::test]
async fn session_end_stamps_the_external_row_with_the_event_time() {
    // agora-5gg.3（2026-09-19 Mac 实测：库里 35 行 FINISHED 的 external 行 ended_at 全空）：
    // ended_at 是「该行结束的时刻」。external 行没有进程退出，但对话有结束——SessionEnd 那一刻，
    // 而且是**事件**的那一刻，不是 daemon 读到它的时刻（A42 对 status_since 已是这个口径）。
    // 用无进程号的行（Codex Desktop 那一类）：进程层说不出结束，结束只能由 hook 写。
    // 改坏一次看红：删掉 `apply_hook_inner` 里 `if rec.runtime_ref.is_none() { … }` 那一段 →
    // `ended_at` 断言红（None）。
    let (fx, receiver, home) = with_hooks();
    let (start, started) = backdate(
        delivery("by-hook", session_start("by-hook"), &[], &[]),
        3600,
    );
    let (end, ended) = backdate(
        delivery(
            "by-hook",
            session_end("by-hook", "prompt_input_exit"),
            &[],
            &[],
        ),
        1800,
    );
    let id = ingest(&receiver, home.path(), &start).unwrap();
    ingest(&receiver, home.path(), &end);

    let rec = fx.sessions.record(&id).unwrap();
    // created_at 同一条规矩：这一行从它的第一条事件起算，不从 daemon 写库那一刻起算。
    assert_eq!(
        rec.created_at,
        clock::format_utc_secs(started),
        "登记用的信封时刻，不是重放时刻"
    );
    assert_eq!(
        fx.sessions.get(&id).unwrap().assessment.status,
        Status::Finished
    );
    assert_eq!(
        rec.ended_at.as_deref(),
        Some(clock::format_utc_secs(ended).as_str()),
        "SessionEnd 的事件时刻"
    );
    assert!(
        !rec.ended_at_approximate,
        "hook 报得出的时刻是准的，不标近似"
    );
}

#[tokio::test]
async fn superseded_external_row_ends_at_the_new_conversation_first_event() {
    // agora-5gg.3：superseded 的旧行没有自己的结束事件，它的结束时刻 = 新对话首条事件的时刻
    // （同一进程换对话的那一刻）。重放两小时前的投递不能把旧行的 ended_at 写成重放那一刻。
    // 共用一个进程号要拿一个**比 hook 老**的活进程：pid 1（本机 EPERM 也算存在）。随手用一个
    // 刚起的进程会被判成号复用（`agent_process_alive`），两行都变 process gone，测的就不是这一格。
    // 改坏一次看红：把 `supersede_external_rows` 的 `apply_hook_at(..., at)` 换回 `apply_hook` →
    // `ended_at` 与 `status_since` 两条断言红（会变成 ingest 跑到那一刻）。
    let (fx, receiver, home) = with_hooks();
    let pid1 = [("CLAUDE_PID", "1".to_owned())];
    let (first, _first_at) = backdate(
        delivery("sup-old", session_start("sup-old"), &pid1, &[]),
        2 * 3600,
    );
    let (second, second_at) = backdate(
        delivery("sup-new", session_start("sup-new"), &pid1, &[]),
        3600,
    );
    let old = ingest(&receiver, home.path(), &first).unwrap();
    let new = ingest(&receiver, home.path(), &second).unwrap();
    assert_ne!(old, new);

    let rec = fx.sessions.record(&old).unwrap();
    let v = fx.sessions.get(&old).unwrap();
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    assert!(
        v.assessment
            .reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("superseded"),
        "{:?}",
        v.assessment
    );
    assert_eq!(
        rec.ended_at.as_deref(),
        Some(clock::format_utc_secs(second_at).as_str()),
        "新对话首条事件的时刻"
    );
    assert!(!rec.ended_at_approximate, "{rec:?}");
    assert_eq!(
        v.status_since, second_at,
        "状态起点跟着同一时刻，两小时前的旧行到得了 ttl"
    );
    // 新行自己还在干活：没有结束时刻。
    assert_eq!(
        fx.sessions.record(&new).unwrap().ended_at,
        None,
        "被取代的是旧行"
    );
}

#[tokio::test]
async fn process_gone_external_row_gets_an_approximate_ended_at() {
    // agora-5gg.3 第三档：宿主一个 hook 都不发（Codex 关窗口），行只靠探活结束。谁也报不出
    // 它几点退的，只能记发现它的那个 tick 并标 `ended_at_approximate`（同 agora-u5p 对
    // 「运行时会话没了」的处理）。
    // 改坏一次看红：删掉 `view` 里 `if external_gone { self.note_external_process_gone(..) }` →
    // `ended_at` 断言红（None）。
    let (fx, receiver, home) = with_hooks();
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let env = [("CLAUDE_PID", child.id().to_string())];
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("by-hup", session_start("by-hup"), &env, &[]),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "by-hup",
            json!({ "hook_event_name": "Stop", "session_id": "by-hup", "last_assistant_message": "done" }),
            &env,
            &[],
        ),
    );
    assert_eq!(
        fx.sessions.record(&id).unwrap().ended_at,
        None,
        "还在干活就没有结束时刻"
    );

    child.kill().unwrap();
    child.wait().unwrap();
    let tick = clock::now_secs();
    let v = fx.sessions.get(&id).unwrap();
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    let rec = fx.sessions.record(&id).unwrap();
    let ended = clock::parse_utc_secs(
        rec.ended_at
            .as_deref()
            .expect("探到进程没了就补上结束时刻（库里 35 行空着就是缺这一步）"),
    )
    .expect("ended_at 是 SQLite 那种 UTC 文本");
    assert!(
        (tick..=tick + 1).contains(&ended),
        "结束时刻 = 发现它没了的那个 tick：tick={tick} ended={ended}"
    );
    assert!(rec.ended_at_approximate, "没人报得出退出时刻，只能近似");

    // 幂等：再读一轮不许把结束时刻一路往前漂（否则按结束时间的淘汰永远等不到 24 h）。
    let again = fx.sessions.record(&id).unwrap();
    assert_eq!(again.ended_at, rec.ended_at);
    assert_eq!(again.ended_at_approximate, rec.ended_at_approximate);
}

#[tokio::test]
async fn external_row_that_moves_on_again_loses_its_ended_at() {
    // ended_at 说的是「这一行结束了」，行又活了就得清掉——与 Restart 清空 ended_at 同一条规则
    // （`restart_with`）。hook 的 FINISHED 不是终态：Claude 的 /resume、Codex TUI 的 /new 之后
    // 同一行回 STARTING，留着一个结束时刻的 RUNNING 行，按结束时间的保留 / 折叠 / 淘汰会把它
    // 当已结束的。改坏：删掉 `apply_hook_inner` 里 ended 分支的 else（`clear_ended_at`）→ 红。
    let (fx, receiver, home) = with_hooks();
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("resumed", session_start("resumed"), &[], &[]),
    )
    .unwrap();
    // 先收一条 prompt 再结束：agora-29n 之后，无句柄 external 行在第一条 PromptSubmitted 之前就遇到
    // SessionEnd(resume|clear) 会被当成身份交接删行（`ingest_inner`），本用例要验的是「已结束的行又活了
    // → ended_at 跟着清」，那得是一行真的结束过的对话，不能是一个空壳 id。2026-09-20 rebase 到 5gg.3
    // 时实测：不补这一条，下面 `record(&id)` 直接 NotFound。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "resumed",
            json!({ "hook_event_name": "UserPromptSubmit", "session_id": "resumed", "prompt": "接着做" }),
            &[],
            &[],
        ),
    );
    ingest(
        &receiver,
        home.path(),
        &delivery("resumed", session_end("resumed", "resume"), &[], &[]),
    );
    assert!(
        fx.sessions.record(&id).unwrap().ended_at.is_some(),
        "SessionEnd(resume) 先把它钉成结束的"
    );

    // 同一进程、同一个自报 id 再来一条 SessionStart：还是那一行，状态回到 STARTING。
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "resumed",
            json!({ "hook_event_name": "SessionStart", "session_id": "resumed", "cwd": "/work/agora", "source": "resume",
                "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch" }),
            &[],
            &[],
        ),
    );
    let rec = fx.sessions.record(&id).unwrap();
    assert_eq!(
        fx.sessions.get(&id).unwrap().assessment.status,
        Status::Starting
    );
    assert_eq!(rec.ended_at, None, "行又活了，结束时刻跟着清掉");
    assert!(!rec.ended_at_approximate);
}

#[tokio::test]
async fn replayed_external_row_is_created_at_the_envelope_time() {
    // agora-5gg.3：Mac 2026-09-18 实测 46 行 external 的 created_at 全落在重启重放的那两分钟里，
    // 比它们自己的第一条事件晚 62 h——按创建时间的排序与分组（侧栏树 A48）看到的都是重启时刻。
    // created_at 取登记它的那条信封的 `received_unix_ms`。
    // 改坏：把 `register_external` 的 `COALESCE(?6, strftime('now'))` 换回
    // `strftime('%Y-%m-%dT%H:%M:%SZ','now')` → 第一条断言红。
    let (fx, receiver, home) = with_hooks();
    let (d, at) = backdate(
        delivery(
            "from-the-quiet-days",
            session_start("from-the-quiet-days"),
            &[],
            &[],
        ),
        26 * 3600,
    );
    let path = Inbox::new(home.path()).write(&d).unwrap();
    // 没有 hook 进程在 socket 上等它：走启动重放这条路。
    assert_eq!(receiver.replay().unwrap(), 1, "{path:?}");
    let rec = fx
        .sessions
        .find_by_agent_session("claude", "from-the-quiet-days")
        .unwrap()
        .expect("登记了");
    assert_eq!(rec.created_at, clock::format_utc_secs(at));
    assert!(
        clock::age_secs(&rec.created_at).unwrap() >= 26 * 3600,
        "不是重放那一刻：{:?}",
        rec.created_at
    );
}

#[tokio::test]
async fn a_resume_handoff_before_the_first_prompt_leaves_no_row() {
    // agora-29n（2026-09-18 zuan 现场 e3cd17，盘点 §3.4）：`claude --resume` 先以**新 id** 发
    // SessionStart(source=startup)，locate_external 查不到就登记出一行；12 s 后 SessionEnd(reason=resume)
    // 落到这一行上。它没有 prompt、没有输出，旧代码把它钉成一行谁也没起过的 FINISHED——无句柄 external
    // 行的身份就是 agent id，换 id 之后这个新行注定是空壳（`clear` 早在 s3r 有了出口，`resume` 漏了）。
    // 守卫：第一条 PromptSubmitted 之前的 SessionEnd(resume|clear) 是身份交接 → 删行（检查点一起删、
    // 求差器下一 tick 发 session_removed、重启后 done/ 里那两条不再把它建回来）。
    // 关掉 ingest_inner 的 handoff 分支 → 第一段"行没了"断言红（停在 finished）；
    // 去掉 `!hook_prompt_seen` 那道判据 → 对照一红，连带 s3r 那条旧守卫（clear 换 id）各自红；
    // 去掉"无句柄"那一半（对 adopted 行也删）→ 对照三红（有 pane 的行 GET 到 404）。
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();
    // 与现场一致：CLAUDE_PID 是一个活着的进程，进程层给不出"结束"，行上只剩 hook 的说法。
    let env = [("CLAUDE_PID", std::process::id().to_string())];
    let mut differ = Differ::default();
    assert!(
        differ
            .step(common::NODE, &fx.sessions.list().unwrap())
            .is_empty(),
        "第一轮只建基线"
    );

    let ghost = ingest(
        &receiver,
        home.path(),
        &delivery("resume-new-id", session_start("resume-new-id"), &env, &[]),
    )
    .unwrap();
    let gid = format!("{}:{}", common::NODE, ghost);
    let path = format!("/api/sessions/{gid}");
    let (status, body) = call(&fx, &cookie, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK, "GET {path} → {body:?}");
    assert_eq!(body["status"], "starting", "GET {path} → {body:?}");
    assert!(fx.sessions.has_hook_checkpoint(&ghost));
    // 现场隔了 12 s：这一行已经进过一次求差，页面确实见过它，所以删掉要看得见一条 session_removed。
    let created = differ.step(common::NODE, &fx.sessions.list().unwrap());
    assert!(
        created
            .iter()
            .any(|e| matches!(e, Event::SessionCreated { id, .. } if id == &gid)),
        "{created:?}"
    );

    let end = delivery(
        "resume-new-id",
        session_end("resume-new-id", "resume"),
        &env,
        &[],
    );
    let end_path = Inbox::new(home.path()).write(&end).unwrap();
    assert_eq!(
        receiver.ingest(&end_path).unwrap(),
        None,
        "身份交接的投递件不算应用出一条会话事件：行都被删了"
    );
    assert!(
        fx.sessions.record(&ghost).is_err(),
        "身份交接不留行，也不留一行无 prompt 无输出的 FINISHED"
    );
    assert_eq!(fx.sessions.list().unwrap().len(), 0);
    assert!(
        !fx.sessions.has_hook_checkpoint(&ghost),
        "检查点随 metadata 一起删，否则重启又把它建回来"
    );
    assert!(!end_path.exists(), "投递件照常移出 inbox");
    assert!(
        Inbox::new(home.path())
            .completed()
            .unwrap()
            .iter()
            .any(|p| p.file_name() == end_path.file_name()),
        "排障归档里还在"
    );
    let removed = differ.step(common::NODE, &fx.sessions.list().unwrap());
    assert!(
        removed
            .iter()
            .any(|e| matches!(e, Event::SessionRemoved { id } if id == &gid)),
        "已见过这一行的页面收到 session_removed：{removed:?}"
    );

    // 重启：done/ 里那两条（SessionStart + SessionEnd）不能把这一行重建出来。
    let restarted = Arc::new(agora::session::SessionManager::new(
        fx.db.clone(),
        fx.rt.clone() as Arc<dyn agora::runtime::Runtime>,
    ));
    Receiver::new(home.path(), restarted.clone())
        .replay()
        .unwrap();
    assert!(
        restarted.record(&ghost).is_err(),
        "库里没有这一行，归档也不该重建它"
    );
    assert!(
        restarted.list().unwrap().is_empty(),
        "{:?}",
        restarted.list()
    );

    // 对照一：有过 prompt 的行仍按普通结束——那是真结束，删掉就抹掉了人做过的一轮
    // （同一条判据的另一侧：去掉 `!hook_prompt_seen` 这里红）。
    let mut kept = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let kept_env = [("CLAUDE_PID", kept.id().to_string())];
    let survivor = ingest(
        &receiver,
        home.path(),
        &delivery(
            "resume-old-id",
            session_start("resume-old-id"),
            &kept_env,
            &[],
        ),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "resume-old-id",
            json!({ "hook_event_name": "UserPromptSubmit", "session_id": "resume-old-id", "prompt": "把测试跑完" }),
            &kept_env,
            &[],
        ),
    );
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "resume-old-id",
            session_end("resume-old-id", "resume"),
            &kept_env,
            &[],
        ),
    );
    let (_, body) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{}:{}", common::NODE, survivor),
        None,
    )
    .await;
    assert_eq!(body["status"], "finished", "有过对话的行仍是结束：{body}");
    assert_eq!(body["source"], "hook", "{body}");
    assert_eq!(body["reason"], "session ended (hook)", "{body}");
    // 那半件事（Q4（agora-5gg.18）：结束即不再谈进程）在这个形状上同样成立：pid 还活着（它正跑别的对话）。
    assert_eq!(body["process"], "gone", "{body}");
    assert_eq!(body["alive"], false, "旧布尔是 process 的投影：{body}");
    assert_eq!(fx.sessions.list().unwrap().len(), 1);
    kept.kill().unwrap();
    kept.wait().unwrap();

    // 对照二：`clear` 同一个 reason 家族、同样没 prompt → 也删行（s3r 的改写只服务于有过对话的行）。
    let mut cleared = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let cleared_env = [("CLAUDE_PID", cleared.id().to_string())];
    let ghost2 = ingest(
        &receiver,
        home.path(),
        &delivery(
            "clear-new-id",
            json!({ "hook_event_name": "SessionStart", "session_id": "clear-new-id", "cwd": "/work/agora", "source": "clear",
                "scratchpad_dir": "/tmp/claude-scratch" }),
            &cleared_env,
            &[],
        ),
    )
    .unwrap();
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "clear-new-id",
            session_end("clear-new-id", "clear"),
            &cleared_env,
            &[],
        ),
    );
    assert!(
        fx.sessions.record(&ghost2).is_err(),
        "/clear 的空行同样不留"
    );
    assert_eq!(
        fx.sessions
            .list()
            .unwrap()
            .iter()
            .map(|v| v.record.id.clone())
            .collect::<Vec<_>>(),
        vec![survivor.clone()],
        "只有那条有过对话的还在"
    );
    cleared.kill().unwrap();
    cleared.wait().unwrap();

    // 对照三：有 pane 的行不走这一支。它的身份是 runtime_ref，SessionEnd 之后同一进程再发的
    // SessionStart 落回同一行（`src/status/machine.rs` 的 `SessionEnded` 注释），那是真会话的 resume；
    // 删掉等于把人正在用的行抹了，即使它一条 prompt 都还没收到。
    fx.rt.insert("fake:default:pane", true, None, false);
    fx.rt
        .panes
        .lock()
        .unwrap()
        .insert("%9".into(), "fake:default:pane".into());
    let pane_env = [
        ("TMUX", "/tmp/tmux-501/default,123,0".to_owned()),
        ("TMUX_PANE", "%9".to_owned()),
    ];
    let adopted = ingest(
        &receiver,
        home.path(),
        &delivery(
            "resume-pane-id",
            session_start("resume-pane-id"),
            &[],
            &pane_env,
        ),
    )
    .unwrap();
    assert_eq!(
        fx.sessions.record(&adopted).unwrap().origin,
        Origin::Adopted,
        "信封能在采纳 socket 上定位到 pane → 有终端的一行"
    );
    ingest(
        &receiver,
        home.path(),
        &delivery(
            "resume-pane-id",
            session_end("resume-pane-id", "resume"),
            &[],
            &pane_env,
        ),
    );
    let (status, body) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{}:{}", common::NODE, adopted),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["status"], "finished",
        "有 pane 的行按普通结束，不删：{body}"
    );
    assert_eq!(body["origin"], "adopted", "{body}");
    assert_eq!(
        {
            let mut left: Vec<String> = fx
                .sessions
                .list()
                .unwrap()
                .into_iter()
                .map(|v| v.record.id)
                .collect();
            left.sort();
            left
        },
        {
            let mut want = vec![survivor.clone(), adopted.clone()];
            want.sort();
            want
        },
        "留下的还是那两条"
    );
}

/// 无头一次性会话登记成 `origin = headless`，满 24 h 不论什么状态都被 sweep 删掉，
/// 客户端看到的是与 `DELETE /api/sessions/:id` 同一条 `session_removed`。
#[tokio::test]
async fn a_headless_session_registers_as_headless_and_expires_whatever_its_status_is() {
    // 裁决 agora-5gg.7 选 B（agora-5gg.20）：宿主自己起的 `claude -p` / 子代理照常登记，但它不是一条
    // 等人回看的会话：默认折叠（`web/src/attention.ts`）、不通知（`tests/events.rs`）、
    // 满 24 h 不论状态即删（它常常连 SessionEnd 都没有，拿状态当门槛就永远清不掉）。
    // 判据只有载荷结构：无头的 SessionStart 只剩公共键（testdata/claude/2.1.270/hooks/headless.jsonl）。
    // 改坏：`expire_external_finished` 里 headless 那一格写回 `matches!(status, Finished)` →
    // 下面「停在 turn_done 的无头行也被删」红；`locate_external` 不判 is_headless → 第一段 origin 红。
    const DAY: u64 = 86_400;
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let sessions = Arc::new(
        SessionManager::new(db, rt as Arc<dyn Runtime>)
            .with_external_finished_ttl(Duration::from_secs(DAY)),
    );
    let receiver = Arc::new(Receiver::new(home.path(), sessions.clone()));

    // 同一批投递里放一条真交互会话：两边只差 SessionStart 的那几个键，另每一格都跟着翻。
    let one_shot = ingest(
        &receiver,
        home.path(),
        &delivery("headless-1", headless_session_start("headless-1"), &[], &[]),
    )
    .unwrap();
    let interactive = ingest(
        &receiver,
        home.path(),
        &delivery("talkative-1", session_start("talkative-1"), &[], &[]),
    )
    .unwrap();
    assert_ne!(one_shot, interactive);
    assert_eq!(
        sessions.record(&one_shot).unwrap().origin,
        Origin::Headless,
        "无头的 SessionStart 登记成 headless"
    );
    assert_eq!(
        sessions.record(&interactive).unwrap().origin,
        Origin::External,
        "交互会话不受影响"
    );
    // 线上形态：`origin` 直接是 "headless"（`Origin` 的 serde 是小写名，无迁移）。老页面读不懂
    // 这个值只会把它当有句柄的行去 attach 终端，不会读错别的字段（`docs/spec/api.md`「版本」）。
    let wire = serde_json::to_value(sessions.get(&one_shot).unwrap()).unwrap();
    assert_eq!(wire["origin"], "headless", "{wire}");

    // 两边都停在 TURN_DONE（`claude -p` 答完就退，Stop 之后什么都没有了）。
    for (id, name) in [(&one_shot, "headless-1"), (&interactive, "talkative-1")] {
        let path = Inbox::new(home.path())
            .write(&delivery(
                name,
                json!({"hook_event_name": "Stop", "session_id": name, "last_assistant_message": "pong"}),
                &[],
                &[],
            ))
            .unwrap();
        receiver.ingest(&path).unwrap();
        assert_eq!(
            sessions.get(id).unwrap().assessment.status,
            Status::TurnDone,
            "{name} 停在 turn_done"
        );
    }

    let mut differ = agora::events::Differ::default();
    assert!(
        differ.step("n", &sessions.list().unwrap()).is_empty(),
        "第一轮只建基线"
    );

    // 25 h 后：只有无头那一行到期（external 的 TURN_DONE 还在等人瞟一眼，不动）。
    assert_eq!(
        sessions
            .sweep(agora::clock::now_secs() + 25 * 3600)
            .unwrap(),
        vec![one_shot.clone()],
        "停在 turn_done 的无头行也被删"
    );
    assert!(sessions.get(&one_shot).is_err(), "行已删");
    assert!(
        sessions.get(&interactive).is_ok(),
        "同样停在 turn_done 的 external 行不动"
    );
    let events = differ.step("n", &sessions.list().unwrap());
    assert!(
        events
            .iter()
            .any(|e| matches!(e, agora::events::Event::SessionRemoved { id } if id == &format!("n:{one_shot}"))),
        "客户端看到的还是那一条 session_removed：{events:?}"
    );
}

// ==================== sweep 的「进程已退出」判据（agora-qmi）====================
//
// receiver 每 5 s 的 sweep 里有一条兜底：agent 进程退了，不会有事件替它发 SessionEnd，
// 把还在 socket 上等答复的 hook 以 none 放掉（`decision_resolved via=exit`）。这条判据原先
// 写的是 `!view.alive`。三态之后 `alive` 恒等于 `process == alive`（agora-5gg.18 的 Q4），
// `false` 就混装了三种事实——真没了、无可信进程号（unknown）、对话结束但 pid 还在跑别的对话，
// 只有第一种还是「进程退了」。下面三条各钉一格；把判据改回 `!view.alive`，② ③ 各红一次
// （② 是 unknown 被当成退出；③ 是行刚钉成 FINISHED 就把还在等的 hook 放掉，旧口径交给超时）。

/// 起一个还在 socket 上等答复的 PermissionRequest：`wake` 登记挂起之后就一直等。
/// 登记在 blocking 线程里跑，轮询到它出现在挂起表为止（同
/// `external_session_answers_permission_via_the_hook_and_refuses_text` 的做法）。
async fn hold_permission(
    receiver: &Arc<Receiver>,
    home: &std::path::Path,
    agent_session: &str,
    session: &str,
    env: &[(&str, String)],
) -> tokio::task::JoinHandle<Response> {
    let path = Inbox::new(home)
        .write(&delivery(
            agent_session,
            json!({ "hook_event_name": "PermissionRequest", "session_id": agent_session,
                    "tool_name": "Bash", "tool_input": { "command": "ls" } }),
            env,
            &[],
        ))
        .unwrap();
    let r = receiver.clone();
    let task = tokio::spawn(async move { r.wake(&path).await });
    for _ in 0..200 {
        if !receiver.pending(session).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        receiver.pending(session),
        vec!["Bash".to_owned()],
        "挂起登记上了才有得测"
    );
    task
}

/// 验收 ①：进程真的退掉了 → sweep 立刻以 `exit` 解除挂起。两种「进程退了」各钉一行：
/// 有句柄的行由运行时报来退出码（remain-on-exit 保住 dead pane，`pid` 照报，所以任何拿
/// `view.pid.is_none()` 划「退干净了」的写法都会在这一格漏掉），external 行由探活探不到号。
/// 改坏法：判据换成 `view.process == Gone && view.pid.is_none()` → 前一段红（dead pane 的号还在，
/// 挂起不解除）；整条判据拿掉（永不按退出解除）→ 两段各红一次（交给 30 s 的超时）。
#[tokio::test]
async fn the_sweep_releases_a_held_hook_when_the_agent_process_is_really_gone() {
    let (fx, receiver, home) = with_hooks();
    let cookie = fx.cookie();

    // 有句柄的行：agent 自己退出，运行时报来退出码 0。
    let (status, created) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(
            json!({ "display_name": "pane", "agent_type": "claude", "working_directory": "/tmp" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let pane = created["local_id"].as_str().unwrap().to_owned();
    let pane_ref = created["runtime_ref"].as_str().unwrap().to_owned();
    let mut pane_delivery = delivery_for(
        "claude",
        "pane-agent",
        json!({ "hook_event_name": "PermissionRequest", "session_id": "pane-agent",
                "tool_name": "Bash", "tool_input": { "command": "ls" } }),
        &[],
        &[],
    );
    pane_delivery.envelope.agora_session_id = Some(pane.clone());
    pane_delivery.envelope.agora_epoch = Some(1);
    let pane_path = Inbox::new(home.path()).write(&pane_delivery).unwrap();
    let r = receiver.clone();
    let pane_task = tokio::spawn(async move { r.wake(&pane_path).await });
    for _ in 0..200 {
        if !receiver.pending(&pane).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(receiver.pending(&pane), vec!["Bash".to_owned()]);

    // external 的行：hook 报来的进程号还在。
    let mut agent = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let env = [("CLAUDE_PID", agent.id().to_string())];
    let ext = ingest(
        &receiver,
        home.path(),
        &delivery("exit-ext", session_start("exit-ext"), &env, &[]),
    )
    .unwrap();
    let ext_task = hold_permission(&receiver, home.path(), "exit-ext", &ext, &env).await;
    let mut events = fx.state.events.subscribe();

    // 对照：两行的进程都还在 → sweep 没有理由动它们的挂起。
    receiver.sweep();
    assert_eq!(
        receiver.pending(&pane),
        vec!["Bash".to_owned()],
        "pane 还活着"
    );
    assert_eq!(
        receiver.pending(&ext),
        vec!["Bash".to_owned()],
        "external 的号还活着"
    );

    // 运行时报来退出码（dead pane 的 pid 照报）；探活探不到号了。
    fx.rt.set_dead(&pane_ref, agora::runtime::Exit::Code(0));
    agent.kill().unwrap();
    agent.wait().unwrap();
    let v = fx.sessions.get(&ext).unwrap();
    assert_eq!(v.process, ProcessState::Gone, "{:?}", v.assessment);

    receiver.sweep();
    assert!(
        receiver.pending(&pane).is_empty(),
        "运行时报来退出码 → 立刻解除，不等 55 min"
    );
    assert!(
        receiver.pending(&ext).is_empty(),
        "探活探不到号 → 立刻解除：{:?}",
        v.assessment
    );
    for task in [pane_task, ext_task] {
        assert!(
            matches!(
                task.await.unwrap(),
                Response::Hook {
                    decision: Decision::None
                }
            ),
            "sweep 放掉的是 fail-open（hook 侧没有决定）"
        );
    }
    // 两条解除不分先后（sweep 按会话 id 排序，id 是随机的），逐条收到齐为止。
    let mut via_by: BTreeMap<String, &'static str> = BTreeMap::new();
    let want: Vec<String> = [pane.as_str(), ext.as_str()]
        .iter()
        .map(|id| format!("{}:{}", common::NODE, id))
        .collect();
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    while via_by.len() < want.len() && tokio::time::Instant::now() < until {
        if let Ok(Ok(Event::DecisionResolved { id, via, .. })) =
            tokio::time::timeout(Duration::from_millis(200), events.recv()).await
        {
            via_by.insert(id, via);
        }
    }
    for id in want {
        assert_eq!(
            via_by.get(&id).copied(),
            Some("exit"),
            "{id} 的解除理由要是 exit（进程退了），实际：{via_by:?}"
        );
    }
}

/// 验收 ②：无可信进程号的 external 行（`process=unknown`，Codex Desktop 的共用 app-server、
/// 宿主不给进程号变量那一类）不是「进程退了」。改坏法：判据换回 `!view.alive`（unknown 投影
/// 出的旧布尔是 false）→ 这条红，sweep 一跑挂起就没了、答它报 no_pending_decision。
#[tokio::test]
async fn the_sweep_leaves_a_held_hook_alone_when_agora_cannot_name_the_process() {
    let (fx, receiver, home) = with_hooks();
    // 明写不带 CLAUDE_PID：Adapter 的 agent_pid 给 None，这一行的进程在不在 agora 说不上。
    let id = ingest(
        &receiver,
        home.path(),
        &delivery("no-pid", session_start("no-pid"), &[], &[]),
    )
    .unwrap();
    let task = hold_permission(&receiver, home.path(), "no-pid", &id, &[]).await;
    let v = fx.sessions.get(&id).unwrap();
    assert_eq!(v.process, ProcessState::Unknown, "{:?}", v.assessment);
    assert!(
        !v.alive,
        "旧布尔把「不知道」压成「没了」——这正是换三值时要分开的那一格"
    );

    receiver.sweep();
    assert_eq!(
        receiver.pending(&id),
        vec!["Bash".to_owned()],
        "「说不上在不在」不等于「退了」：挂起交给超时，不由 sweep 放掉"
    );
    // 挂起还是活的、还答得动（sweep 没把它以 none 放掉）。
    receiver.respond(&id, None, Decision::Allow).unwrap();
    assert!(
        matches!(
            task.await.unwrap(),
            Response::Hook {
                decision: Decision::Allow
            }
        ),
        "答得动才说明它没被 sweep 放掉"
    );
}

/// 验收 ③：行已经是 FINISHED 而那个 pid 还在跑别的对话 → 这一轮 sweep 不以 exit 解除。这一格
/// 是 Q4 的裁决压过进程事实的地方：`process` 一律 `gone`，进程却没退。改坏法：判据换回
/// `!view.alive`（FINISHED 行的旧布尔恒为 false），或只看 `view.process == ProcessState::Gone`
/// → 两条各红一次。
#[tokio::test]
async fn the_sweep_leaves_a_held_hook_alone_when_only_the_conversation_ended() {
    let (fx, receiver, home) = with_hooks();
    // 用自己的进程号当那个「还活着、只是换了对话」的 agent 进程（同 supersede 那条守卫）。
    let env = [("CLAUDE_PID", std::process::id().to_string())];
    let old = ingest(
        &receiver,
        home.path(),
        &delivery("conv-old", session_start("conv-old"), &env, &[]),
    )
    .unwrap();
    let task = hold_permission(&receiver, home.path(), "conv-old", &old, &env).await;

    // 同一个进程报来第二个对话：旧行到此为止（Grok / Codex 换对话不发 SessionEnd）。
    let new = ingest(
        &receiver,
        home.path(),
        &delivery("conv-new", session_start("conv-new"), &env, &[]),
    )
    .unwrap();
    assert_ne!(old, new);
    let v = fx.sessions.get(&old).unwrap();
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    assert_eq!(v.process, ProcessState::Gone, "Q4：{:?}", v.assessment);
    assert_eq!(
        v.pid,
        Some(std::process::id()),
        "号还探得到就照报——它就是「进程没退」的证据"
    );

    // 这一段要在挂起登记后 2 s 内跑完：再晚，sweep 的另一条兜底（agora-9cd：状态机里已经
    // 没这个键、hold 又跨过 HOLD_SETTLE → via=terminal）会替它放掉，测的就不是这条判据了；
    // supersede 会把旧行的挂起清掉，所以那个 hook 事实上是走 via=terminal 而非 55 min 超时。
    receiver.sweep();
    assert_eq!(
        receiver.pending(&old),
        vec!["Bash".to_owned()],
        "对话结束不等于进程退出：这一轮 sweep 不能以 exit 放掉它"
    );
    receiver.respond(&old, None, Decision::Allow).unwrap();
    assert!(
        matches!(
            task.await.unwrap(),
            Response::Hook {
                decision: Decision::Allow
            }
        ),
        "答得动才说明它没被 sweep 放掉"
    );
}
