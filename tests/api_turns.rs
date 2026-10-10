//! `GET /api/sessions/:id/turns`（agora-2mff；MISSION §6.9 / A53；docs/spec/api.md「轮次」）：hook 事件
//! 落进 `<AGORA_HOME>/turns/` 的有界轮次日志，按时间先后交出最近几轮；daemon 重启（新的
//! SessionManager、同一个 home）后仍在；重放不重复记；行删了日志跟着走。peer 一跳转发见
//! `tests/forward.rs::turns_forwarded_to_owner`。

mod common;

use std::path::Path;
use std::sync::Arc;

use agora::runtime::Runtime;
use agora::session::manager::SessionManager;
use agora::status::AgoraEvent;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{Fx, HOST};

async fn call(fx: &Fx, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, fx.cookie());
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

/// 建一行、给它的 SessionManager 装上 home；返回 (gid, 本机 id)。
async fn session(fx: &Fx, home: &Path) -> (String, String) {
    fx.sessions.enable_hook_checkpoints(home);
    let wd = home.join("wd");
    std::fs::create_dir_all(&wd).unwrap();
    let (status, body) = call(
        fx,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": "w", "agent_type": "shell", "working_directory": wd })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let gid = body["id"].as_str().unwrap().to_owned();
    let id = gid.rsplit(':').next().unwrap().to_owned();
    (gid, id)
}

/// 三轮：两轮做完、第三轮还在跑。投递件名带毫秒（真投递件就是这样），同一秒里的先后照样分得清。
fn conversation() -> Vec<(String, AgoraEvent)> {
    let t0 = 1_791_600_000_000u64;
    [
        (0, AgoraEvent::PromptSubmitted("先看看 README".into())),
        (
            400,
            AgoraEvent::TurnEnded(Some("README 说这是一个 agent 控制台。".into())),
        ),
        (900, AgoraEvent::PromptSubmitted("那测试怎么跑？".into())),
        (
            950,
            AgoraEvent::TurnEnded(Some("cargo test 与 npm --prefix web test。".into())),
        ),
        (2_000, AgoraEvent::PromptSubmitted("跑一下".into())),
    ]
    .into_iter()
    .map(|(dt, e)| (format!("{}-4242.json", t0 + dt), e))
    .collect()
}

fn deliver(sessions: &SessionManager, id: &str, events: &[(String, AgoraEvent)]) {
    for (name, e) in events {
        sessions
            .apply_delivered_hook(id, 1, std::slice::from_ref(e), name, 0)
            .unwrap();
    }
}

fn prompts(body: &Value) -> Vec<String> {
    body["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["prompt"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[tokio::test]
async fn turns_come_back_oldest_first_with_limit_and_survive_a_restart() {
    let fx = Fx::new();
    let home = tempfile::tempdir().unwrap();
    let (gid, id) = session(&fx, home.path()).await;
    deliver(&fx.sessions, &id, &conversation());

    let (status, body) = call(
        &fx,
        Method::GET,
        &format!("/api/sessions/{gid}/turns"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["keep"], 20);
    assert_eq!(
        prompts(&body),
        ["先看看 README", "那测试怎么跑？", "跑一下"]
    );
    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns[0]["reply"], "README 说这是一个 agent 控制台。");
    assert_eq!(turns[0]["outcome"], "done");
    assert_eq!(turns[0]["started_at"], 1_791_600_000);
    assert_eq!(turns[2]["outcome"], "open");
    assert_eq!(turns[2]["reply"], Value::Null);

    let (_, two) = call(
        &fx,
        Method::GET,
        &format!("/api/sessions/{gid}/turns?limit=2"),
        None,
    )
    .await;
    assert_eq!(prompts(&two), ["那测试怎么跑？", "跑一下"]);
    let (_, zero) = call(
        &fx,
        Method::GET,
        &format!("/api/sessions/{gid}/turns?limit=0"),
        None,
    )
    .await;
    assert_eq!(prompts(&zero), ["跑一下"], "limit 夹到 1..=20");

    // daemon 重启：新的 SessionManager 对着同一个库与 home，内存里什么都没有。
    let restarted = SessionManager::new(fx.db.clone(), fx.rt.clone() as Arc<dyn Runtime>);
    restarted.enable_hook_checkpoints(home.path());
    let turns = restarted.turns(&id, 20).unwrap();
    assert_eq!(turns.len(), 3);
    assert_eq!(
        turns[1].reply.as_deref(),
        Some("cargo test 与 npm --prefix web test。")
    );
}

/// 崩在「日志已追加、检查点还没落」之间：重启后没有检查点挡，同一批投递件会再应用一遍
/// （归档重建也是这样）——日志不能因此多出一倍。
#[tokio::test]
async fn reapplied_deliveries_do_not_duplicate_turns() {
    let fx = Fx::new();
    let home = tempfile::tempdir().unwrap();
    let (_gid, id) = session(&fx, home.path()).await;
    deliver(&fx.sessions, &id, &conversation());

    let fresh = SessionManager::new(fx.db.clone(), fx.rt.clone() as Arc<dyn Runtime>);
    fresh.enable_hook_checkpoints(home.path());
    deliver(&fresh, &id, &conversation());
    assert_eq!(fresh.turns(&id, 20).unwrap().len(), 3);
}

#[tokio::test]
async fn deleting_the_row_deletes_its_turns() {
    let fx = Fx::new();
    let home = tempfile::tempdir().unwrap();
    let (gid, id) = session(&fx, home.path()).await;
    deliver(&fx.sessions, &id, &conversation());
    let dir = home.path().join("turns");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

    fx.sessions.delete_metadata(&id).unwrap();
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "对话正文随行删除"
    );
    let (status, body) = call(
        &fx,
        Method::GET,
        &format!("/api/sessions/{gid}/turns"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not_found");
}

/// 还没记过的行是空数组；老节点不认识 `/turns`，GET 落到 SPA fallback 的空 body 404——页面对两种
/// 非 2xx 一样按「没有上文」处理。
#[tokio::test]
async fn empty_rows_and_the_older_node_shape() {
    let fx = Fx::new();
    let home = tempfile::tempdir().unwrap();
    let (gid, _id) = session(&fx, home.path()).await;
    let (status, body) = call(
        &fx,
        Method::GET,
        &format!("/api/sessions/{gid}/turns"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["turns"], json!([]));

    let (status, body) = call(
        &fx,
        Method::GET,
        &format!("/api/sessions/{gid}/no-such-thing"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, Value::Null, "SPA fallback 不回 JSON");
}

/// 没装配 home 的 SessionManager（库与 API 层的其它测试）什么都不记，也不报错。
#[tokio::test]
async fn a_manager_without_a_home_records_nothing() {
    let fx = Fx::new();
    let wd = tempfile::tempdir().unwrap();
    let (status, body) = call(
        &fx,
        Method::POST,
        "/api/sessions",
        Some(json!({ "display_name": "w", "agent_type": "shell", "working_directory": wd.path() })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let gid = body["id"].as_str().unwrap().to_owned();
    let id = gid.rsplit(':').next().unwrap().to_owned();
    deliver(&fx.sessions, &id, &conversation());
    let (status, body) = call(
        &fx,
        Method::GET,
        &format!("/api/sessions/{gid}/turns"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["turns"], json!([]));
}
