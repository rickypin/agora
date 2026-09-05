//! `/api/sessions*` 的 A1 / A20 / A21 语义（agora-xqa.9），假运行时 + 内存库。

mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{Fx, HOST, NODE};

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
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

async fn create(fx: &Fx, cookie: &str, name: &str) -> Value {
    let (status, body) = call(
        fx,
        cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({
            "display_name": name,
            "agent_type": "shell",
            "working_directory": "/tmp",
            "command": "sleep 300",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

fn local(gid: &str) -> &str {
    gid.split_once(':').unwrap().1
}

#[tokio::test]
async fn ids_are_node_prefixed_and_list_includes_unregistered_runtime_sessions() {
    // A1：列表含全部运行时会话——已登记的与运行时里未登记的（Unknown Agent）。
    let fx = Fx::new();
    let cookie = fx.cookie();
    let created = create(&fx, &cookie, "one").await;
    let gid = created["id"].as_str().unwrap();
    assert!(gid.starts_with(&format!("{NODE}:")), "{gid}");
    assert_eq!(created["node"], NODE);
    assert_eq!(created["local_id"], local(gid));
    assert_eq!(created["status"], "starting");

    fx.rt.insert("fake:default:stray", true, None, false);
    let (status, body) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], gid);
    let unregistered = body["unregistered"].as_array().unwrap();
    assert_eq!(unregistered.len(), 1);
    assert_eq!(unregistered[0]["runtime_ref"], "fake:default:stray");
    assert_eq!(unregistered[0]["managed"], false);

    // 单条：全局 id 与裸 id 都行；别的节点 → node_unknown。
    let (status, one) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["id"], gid);
    let (status, _) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{}", local(gid)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, err) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/othernode:{}", local(gid)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(err["error"], "node_unknown");
    let (status, err) = call(&fx, &cookie, Method::GET, "/api/sessions/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(err["error"], "not_found");
}

#[tokio::test]
async fn adopt_registers_an_unregistered_session() {
    let fx = Fx::new();
    let cookie = fx.cookie();
    fx.rt.insert("fake:default:manual", true, None, false);
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions/adopt",
        Some(json!({ "runtime_ref": "fake:default:manual", "display_name": "手动起的" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["origin"], "adopted");
    assert_eq!(body["name"], "手动起的");
    assert_eq!(body["runtime_ref"], "fake:default:manual");
    assert_eq!(body["managed"], false);
    // 采纳过的不再出现在 unregistered；再采纳一次 → already_registered。
    let (_, list) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert!(list["unregistered"].as_array().unwrap().is_empty());
    let (status, err) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions/adopt",
        Some(json!({ "runtime_ref": "fake:default:manual" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"], "already_registered");
}

#[tokio::test]
async fn delete_metadata_never_kills() {
    // A20：DELETE 只删 metadata，活着的进程留在运行时成未注册。
    let fx = Fx::new();
    let cookie = fx.cookie();
    let created = create(&fx, &cookie, "keep-running").await;
    let gid = created["id"].as_str().unwrap().to_owned();
    let r#ref = created["runtime_ref"].as_str().unwrap().to_owned();
    let (status, _) = call(
        &fx,
        &cookie,
        Method::DELETE,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let still = fx.rt.sessions.lock().unwrap().get(&r#ref).cloned().unwrap();
    assert!(still.alive, "DELETE 不得杀进程");
    let (_, list) = call(&fx, &cookie, Method::GET, "/api/sessions", None).await;
    assert!(list["sessions"].as_array().unwrap().is_empty());
    assert_eq!(list["unregistered"][0]["runtime_ref"], r#ref);
}

#[tokio::test]
async fn kill_and_restart_need_confirmation_only_when_they_would_kill() {
    // A21 + MISSION §8：会杀 → needs_confirmation；不会杀 → 直接执行。
    let fx = Fx::new();
    let cookie = fx.cookie();
    let created = create(&fx, &cookie, "k").await;
    let gid = created["id"].as_str().unwrap().to_owned();
    let kill = format!("/api/sessions/{gid}/kill");
    let restart = format!("/api/sessions/{gid}/restart");

    // 没带 body / confirmed:false 都拦住，且不动进程。
    let (status, err) = call(&fx, &cookie, Method::POST, &kill, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"], "needs_confirmation");
    let (status, _) = call(
        &fx,
        &cookie,
        Method::POST,
        &restart,
        Some(json!({ "confirmed": false })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, v) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(v["alive"], true);

    // 确认后执行：kill 成 FINISHED（killed by user）。
    let (status, v) = call(
        &fx,
        &cookie,
        Method::POST,
        &kill,
        Some(json!({ "confirmed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["alive"], false);
    assert_eq!(v["status"], "finished");

    // 已退出：不会杀，restart 不需要确认，epoch +1。
    let (status, v) = call(&fx, &cookie, Method::POST, &restart, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["epoch"], 2);
    assert_eq!(v["alive"], true);

    // 跑起来后 restart 又需要确认。
    let (status, _) = call(&fx, &cookie, Method::POST, &restart, None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 进程自己 FAILED：kill 不需要确认，直接执行（空操作）。
    let r#ref = created["runtime_ref"].as_str().unwrap();
    fx.rt.set_dead(r#ref, agora::runtime::Exit::Code(3));
    let (status, v) = call(&fx, &cookie, Method::POST, &kill, None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn cleanup_refuses_alive_and_patch_renames() {
    let fx = Fx::new();
    let cookie = fx.cookie();
    let created = create(&fx, &cookie, "c").await;
    let gid = created["id"].as_str().unwrap().to_owned();
    let (status, err) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/cleanup"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"], "still_alive");

    let (status, v) = call(
        &fx,
        &cookie,
        Method::PATCH,
        &format!("/api/sessions/{gid}"),
        Some(json!({ "display_name": "改过名" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "改过名");
    assert_eq!(v["name_locked"], true);
    // 改名要发 session_updated：不然侧栏到下一次全量前一直是旧名字（agora-xqa.11）。
    let mut rx = fx.state.events.subscribe();
    let (_, v2) = call(
        &fx,
        &cookie,
        Method::PATCH,
        &format!("/api/sessions/{gid}"),
        Some(json!({ "display_name": "再改" })),
    )
    .await;
    match rx.try_recv() {
        Ok(agora::events::Event::SessionUpdated { id, session }) => {
            assert_eq!(id, gid);
            assert_eq!(session, v2);
        }
        other => panic!("期待 SessionUpdated，得到 {other:?}"),
    }

    fx.rt.set_dead(
        created["runtime_ref"].as_str().unwrap(),
        agora::runtime::Exit::Code(0),
    );
    let (status, _) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/cleanup"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(fx.rt.removed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn create_falls_back_to_agent_type_as_command_and_validates_body() {
    let fx = Fx::new();
    let cookie = fx.cookie();
    let (status, v) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": "x", "agent_type": "myagent", "working_directory": "/tmp" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(
        v["command"], "myagent",
        "缺省命令 = agent_type 本身（可移植裸命令名）"
    );
    let (status, err) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": "", "agent_type": "a", "working_directory": "/tmp" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"], "bad_request");
}

#[tokio::test]
async fn restart_resumes_self_reported_conversation_and_create_pins_one() {
    // agora-dvh.13 / A32：Restart 用 agent 自报的对话 id resume；起会话时先钉一个 id 兜底。
    let fx = Fx::new();
    let cookie = fx.cookie();
    fx.probe_available("claude", agora::adapter::Version(2, 1, 260));
    let (status, created) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": "c", "agent_type": "claude", "working_directory": "/tmp" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let pinned = created["agent_session_id"].as_str().unwrap().to_owned();
    assert_eq!(
        created["command"].as_str().unwrap(),
        format!("claude --session-id {pinned}")
    );
    let gid = created["id"].as_str().unwrap().to_owned();
    let local = created["local_id"].as_str().unwrap().to_owned();
    let restart = format!("/api/sessions/{gid}/restart");

    // 没有别的自报 → 用钉的 id resume。
    let (status, v) = call(
        &fx,
        &cookie,
        Method::POST,
        &restart,
        Some(json!({ "confirmed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["restart"]["resumed"], true);
    assert_eq!(v["restart"]["agent_session_id"], pinned);
    assert_eq!(
        fx.rt.respawns.lock().unwrap().last().unwrap(),
        &format!("claude --resume {pinned}"),
        "钉的 --session-id 换成 --resume，不叠加"
    );
    assert_eq!(
        v["command"],
        format!("claude --session-id {pinned}"),
        "库里的原命令不动"
    );

    // agent 自报了新对话（/clear 后 SessionStart 带新 id）→ 覆盖，Restart 跟着新 id 走。
    fx.db
        .conn()
        .execute(
            "UPDATE sessions SET agent_session_id = 'conv-after-clear' WHERE id = ?1",
            [&local],
        )
        .unwrap();
    let (status, v) = call(
        &fx,
        &cookie,
        Method::POST,
        &restart,
        Some(json!({ "confirmed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["restart"]["agent_session_id"], "conv-after-clear");
    assert_eq!(
        fx.rt.respawns.lock().unwrap().last().unwrap(),
        "claude --resume conv-after-clear"
    );

    // 版本表外：不猜参数，原命令 + 原因。
    // why 里带整段 stderr（agora-k9r 的现场是 fake 命令的 usage）：API 只给首行、截到 120 字符。
    fx.probe_unparsable(
        "claude",
        &format!(
            "1.0.0 低于版本表首项{}\n用法: agora [serve | url | open]\n第三行",
            "x".repeat(200)
        ),
    );
    let (status, v) = call(
        &fx,
        &cookie,
        Method::POST,
        &restart,
        Some(json!({ "confirmed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["restart"]["resumed"], false);
    let reason = v["restart"]["reason"].as_str().unwrap();
    assert!(reason.contains("不可解析"), "{v}");
    assert!(!reason.contains('\n'), "只留首行: {reason}");
    assert!(
        reason.chars().count() <= 121 && reason.ends_with('…'),
        "截到 120 字符加省略号: {reason}"
    );
    assert_eq!(
        fx.rt.respawns.lock().unwrap().last().unwrap(),
        &format!("claude --session-id {pinned}"),
        "退化为库里的原命令"
    );

    // 没有 Adapter 的类型：不钉、不 resume。
    let (_, custom) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": "m", "agent_type": "myagent", "working_directory": "/tmp" })),
    )
    .await;
    assert!(custom["agent_session_id"].is_null(), "{custom}");
    assert_eq!(custom["command"], "myagent");
}
