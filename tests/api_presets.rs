//! `GET /api/presets` 与 `POST /api/sessions {preset}` 的守卫（agora-prdg.4；epic agora-hxva；
//! MISSION §6.9；docs/spec/api.md）。
//!
//! 钉住的是 S2 的边界：列表只读（没有任何写端点，POST / PUT / PATCH / DELETE 一律 405、不是 404）、
//! 需认证；创建只在本机展开（preset + node 在转发之前 400），预设的 `args` 真的接在裸命令名之后
//! 进了那一代的命令行——最后这条用真 tmux + `agora fake-agent` 的 `argv` 指令把自己收到的
//! argv 落盘来断言（只看响应里的 `launch_args` 字段证明不了它进了命令行）。

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::api::{ApiVersion, API_VERSION};
use agora::peer::registry::PeerRegistry;
use agora::peer::transport::{InProcessTransport, PeerTransport};
use agora::session::preset;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::node::{TmuxNode, AGORA_BIN};
use common::{Fx, HOST, NODE};

async fn call(
    fx: &Fx,
    cookie: Option<&str>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"));
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, cookie);
    }
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
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

/// 真 tmux 节点上的 API 调用：cookie 从它自己的 Auth 来，走同一个 router。
async fn node_call(
    node: &TmuxNode,
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
    let resp = agora::api::router(node.state.clone())
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

#[tokio::test]
async fn presets_are_read_only_and_require_a_principal() {
    let fx = Fx::new();
    // 未认证：不在白名单里，401（tests/auth.rs 的遍历式守卫也会敲这一条）。
    let (status, body) = call(&fx, None, Method::GET, "/api/presets", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthenticated");

    // 空库：200 + 空列表（"还没有预设"是数据状态，不是错误）。
    let cookie = fx.cookie();
    let (status, body) = call(&fx, Some(&cookie), Method::GET, "/api/presets", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["presets"], json!([]));

    // 两条：按名字排序（`preset::list` 的既有顺序，确定、好扫），形态是约定的六个字段。
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    let cwd = dir.to_str().unwrap();
    preset::upsert(&fx.db, "zeta", "claude", cwd, Some("--model opus"), None).unwrap();
    preset::upsert(&fx.db, "alpha", "pi", cwd, None, Some("跑一遍测试")).unwrap();

    let (status, body) = call(&fx, Some(&cookie), Method::GET, "/api/presets", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["presets"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{body}");
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["alpha", "zeta"], "按名字排序");
    assert_eq!(rows[0]["agent_type"], "pi");
    assert_eq!(rows[0]["prompt"], "跑一遍测试");
    assert_eq!(rows[0]["args"], Value::Null);
    assert_eq!(rows[1]["working_directory"], cwd);
    assert_eq!(rows[1]["args"], "--model opus");
    assert_eq!(rows[1]["prompt"], Value::Null);
    assert_eq!(
        rows[1]["updated_at"].as_str().unwrap().len(),
        20,
        "updated_at 是 SQLite 的 YYYY-MM-DDTHH:MM:SSZ"
    );

    // 写端点不存在：路由只注册 GET，其余方法由 axum 按方法不匹配回 405 METHOD_NOT_ALLOWED。
    // 不是 404——404 说的是"没有这个端点"，405 才是"端点有、这个动作没有"（S2 的只读边界）。
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let (status, _) = call(
            &fx,
            Some(&cookie),
            method.clone(),
            "/api/presets",
            Some(json!({ "name": "x" })),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
}

#[test]
fn api_version_is_1_15_with_the_presets_endpoint() {
    // 版本纪律（agora-prdg.4）：新端点 + 新错误类型 preset_unknown ⇒ minor +1；两边同改的是
    // src/api/version.rs 与 web/src/health.ts，一致性由 version.rs 单测
    // `page_is_built_against_the_same_api_version` 钉住。
    assert_eq!(API_VERSION, ApiVersion::new(1, 15));
}

#[tokio::test]
async fn unknown_preset_is_404_preset_unknown() {
    let fx = Fx::new();
    let cookie = fx.cookie();
    let (status, body) = call(
        &fx,
        Some(&cookie),
        Method::POST,
        "/api/sessions",
        Some(json!({ "preset": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "preset_unknown");
    // 空名也走同一个入口，但它是 400（拼错的请求，不是"删掉了的预设"）。
    let (status, body) = call(
        &fx,
        Some(&cookie),
        Method::POST,
        "/api/sessions",
        Some(json!({ "preset": "  " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "bad_request");
}

#[tokio::test]
async fn preset_with_explicit_fields_is_400_not_a_guess() {
    let fx = Fx::new();
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    preset::upsert(
        &fx.db,
        "pktmask",
        "claude",
        dir.to_str().unwrap(),
        None,
        None,
    )
    .unwrap();
    let cookie = fx.cookie();
    // 每一个显式字段与 preset 同给都算"不猜"；错误消息点名是哪个字段。
    let cases = [
        json!({ "preset": "pktmask", "display_name": "x" }),
        json!({ "preset": "pktmask", "agent_type": "claude" }),
        json!({ "preset": "pktmask", "working_directory": dir.to_str().unwrap() }),
        json!({ "preset": "pktmask", "worktree": "w" }),
        json!({ "preset": "pktmask", "task_ref": "agora-prdg.4" }),
        json!({ "preset": "pktmask", "command": "claude" }),
        json!({ "preset": "pktmask", "cols": 80 }),
        json!({ "preset": "pktmask", "rows": 24 }),
        json!({ "preset": "pktmask", "prompt": "hi" }),
    ];
    for case in cases {
        let (status, body) = call(
            &fx,
            Some(&cookie),
            Method::POST,
            "/api/sessions",
            Some(case.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{case} → {body}");
        assert_eq!(body["error"], "bad_request", "{case}");
        assert!(
            body["message"].as_str().unwrap().contains("preset"),
            "{case} → {body}"
        );
    }
    // 一条会话都没起（被拒的请求没有任何副作用）。
    let (status, body) = call(&fx, Some(&cookie), Method::GET, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"], json!([]));
}

#[tokio::test]
async fn preset_with_node_is_rejected_before_any_forward() {
    // A 把 B 配成 peer：若 preset 的检查在 forward::route_node 之后，A 就会把预设名转发给 B，
    // B 的库里没有这条预设、回一个误导性的 preset_unknown。正确行为是 A 当场 400，B 一个字节
    // 都没收到（attempts() == 0）。
    let mut a = Fx::new();
    let b = Fx::new();
    let transport = Arc::new(InProcessTransport::new(
        "peer",
        NODE,
        b.app(),
        Duration::from_secs(1),
    ));
    let mut reg = PeerRegistry::new(NODE);
    reg.insert(transport.clone() as Arc<dyn PeerTransport>)
        .unwrap();
    a.state.registry = Arc::new(reg);

    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    preset::upsert(&a.db, "pktmask", "shell", dir.to_str().unwrap(), None, None).unwrap();
    // 对端的库里也有这条预设：万一被转发过去，对端可能真起会话——这个测试因此能区分
    // "被拒绝"与"碰巧在那边也失败了"。
    preset::upsert(&b.db, "pktmask", "shell", dir.to_str().unwrap(), None, None).unwrap();

    let cookie = a.cookie();
    for node in ["peer", NODE] {
        let (status, body) = call(
            &a,
            Some(&cookie),
            Method::POST,
            "/api/sessions",
            Some(json!({ "preset": "pktmask", "node": node })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "node={node} → {body}");
        assert_eq!(body["error"], "bad_request", "node={node}");
    }
    assert_eq!(transport.attempts(), 0, "preset + node 必须在转发之前拒");
    assert!(a.rt.sessions.lock().unwrap().is_empty());
    assert!(b.rt.sessions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn preset_with_a_missing_directory_is_400_at_create_time() {
    // CLI 在 add 时校验目录存在（S1），但目录之后可能被删 / 移走。起会话时才发现的话
    // 运行时给的是 502 runtime（tmux 的 can't chdir）；预设这条路先判，给 400 与一句话。
    let fx = Fx::new();
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().canonicalize().unwrap();
    std::fs::remove_dir_all(&gone).unwrap();
    preset::upsert(&fx.db, "gone", "shell", gone.to_str().unwrap(), None, None).unwrap();
    let cookie = fx.cookie();
    let (status, body) = call(
        &fx,
        Some(&cookie),
        Method::POST,
        "/api/sessions",
        Some(json!({ "preset": "gone" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "bad_request");
    assert!(body["message"].as_str().unwrap().contains("gone"), "{body}");
    assert!(fx.rt.sessions.lock().unwrap().is_empty());
}

/// 最硬的一条（验收 ②）：预设展开成的那一代命令行里真的有 `launch_args` 与首句——真 tmux 起
/// `agora fake-agent`，它的 `argv` 指令把自己收到的 argv 落盘，测试读文件断言。
#[tokio::test]
async fn create_from_preset_puts_launch_args_and_prompt_on_the_command_line() {
    let mut node = TmuxNode::new();
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    let argv_file = dir.join("argv.txt");
    // 扮演 claude：agent 类型用来选 Adapter（claude 吃首句 prompt），命令换成 fake-agent 的
    // argv 落盘器（`agents.<type>.command` 覆盖链，与生产同一条）。
    let dumper = format!("{AGORA_BIN} fake-agent -e \"argv {}\"", argv_file.display());
    let mut agents = std::collections::BTreeMap::new();
    agents.insert(
        "claude".to_owned(),
        agora::config::AgentOverride {
            command: Some(dumper),
        },
    );
    node.state.agents = Arc::new(agents);
    preset::upsert(
        &node.db,
        "pktmask-claude",
        "claude",
        dir.to_str().unwrap(),
        Some("--model opus -c 'a b'"),
        Some("跑一遍测试并总结失败"),
    )
    .unwrap();

    let cookie = node.cookie();
    let (status, body) = node_call(
        &node,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({ "preset": "pktmask-claude" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 展开正确：display_name 缺省是预设名，agent / 目录 / 命令行快照都从预设来。
    assert_eq!(body["display_name"], "pktmask-claude");
    assert_eq!(body["agent_type"], "claude");
    assert_eq!(body["working_directory"], dir.display().to_string());
    assert_eq!(body["launch_args"], "--model opus -c 'a b'");
    // 透明：跟着会话行一起出现在 SessionView 里，手机上也能看到"这条会跑什么"。
    let (status, one) = node_call(
        &node,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{}", body["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["launch_args"], "--model opus -c 'a b'");

    // fake-agent 收到的 argv（不含 argv[0]）：`fake-agent` 子命令与脚本参数之后就是预设的 args 与首句。
    let deadline = Instant::now() + Duration::from_secs(10);
    let lines = loop {
        if let Ok(text) = std::fs::read_to_string(&argv_file) {
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            if lines.len() == 8 {
                break lines;
            }
        }
        assert!(
            Instant::now() < deadline,
            "fake-agent 没写出完整的 argv 文件: {argv_file:?}; file={:?}",
            std::fs::read_to_string(&argv_file)
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(lines[0], "fake-agent");
    assert_eq!(lines[1], "-e");
    assert_eq!(lines[2], format!("argv {}", argv_file.display()));
    assert_eq!(
        &lines[3..],
        ["--model", "opus", "-c", "a b", "跑一遍测试并总结失败"],
        "预设的 args 与首句必须在那一代的命令行上（原样经 shell）"
    );
}
