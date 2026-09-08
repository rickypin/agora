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
    // 重建后的检查点是 v2 且带回了进程号。
    let key: String = old.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let cp: Value = serde_json::from_slice(
        &std::fs::read(home.path().join("hooks/state").join(format!("{key}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(cp["version"], 2, "{cp}");
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
