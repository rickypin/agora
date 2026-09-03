//! `GET /api/projects` / `/api/projects/worktrees` / `/api/agents`：New Agent 对话框的
//! 三个数据源（MISSION §6.4；agora-xqa.12）。

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use agora::project::Projects;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{Fx, HOST};

async fn get(fx: &Fx, cookie: &str, path: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = fx.app().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

/// 建一个 git 仓库；`git` 不在时返回 false，调用方跳过（CI 有 git，开发机也有，但不赌）。
fn git_init(dir: &Path) -> bool {
    std::fs::create_dir_all(dir).unwrap();
    let ok = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok(&["init", "-q", "-b", "main"])
        && ok(&["config", "user.email", "t@example.com"])
        && ok(&["config", "user.name", "t"])
        && ok(&["commit", "-q", "--allow-empty", "-m", "init"])
}

fn with_roots(fx: &mut Fx, roots: Vec<std::path::PathBuf>) {
    fx.state.projects = Arc::new(Projects::new(fx.db.clone(), roots));
}

#[tokio::test]
async fn projects_are_scanned_not_configured_and_sort_by_most_recently_used() {
    // §6.4：项目列表不靠手写；排序的唯一来源是"起过会话"。
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    if !git_init(&root.join("alpha")) {
        eprintln!("跳过：本机没有可用的 git");
        return;
    }
    assert!(git_init(&root.join("beta")));
    // 深一层的仓库也要扫到（~/code/<org>/<repo> 的放法）。
    assert!(git_init(&root.join("org/gamma")));
    // 不是仓库的目录不进列表。
    std::fs::create_dir_all(root.join("notes")).unwrap();

    let mut fx = Fx::new();
    with_roots(&mut fx, vec![root.to_path_buf()]);
    let cookie = fx.cookie();

    let (status, body) = get(&fx, &cookie, "/api/projects").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<&str> = body["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alpha", "beta", "gamma"], "{body}");
    assert!(body["projects"][0]["last_used_at"].is_null());

    // 在 beta 里起一个会话 → 它排到最前，其余按名字。
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/sessions")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "display_name": "s",
                "agent_type": "shell",
                "working_directory": root.join("beta").to_string_lossy(),
                "command": "sleep 300",
            })
            .to_string(),
        ))
        .unwrap();
    let resp = fx.app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let (_, body) = get(&fx, &cookie, "/api/projects").await;
    let names: Vec<&str> = body["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["beta", "alpha", "gamma"], "{body}");
    assert!(body["projects"][0]["last_used_at"].is_string());
}

#[tokio::test]
async fn worktrees_list_the_repo_and_refuse_paths_outside_known_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let repo = root.join("alpha");
    if !git_init(&repo) {
        eprintln!("跳过：本机没有可用的 git");
        return;
    }
    let wt = root.join("alpha-wt/feature");
    let added = Command::new("git")
        .args(["worktree", "add", "-q", "-b", "feature"])
        .arg(&wt)
        .current_dir(&repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let mut fx = Fx::new();
    with_roots(&mut fx, vec![root.to_path_buf()]);
    let cookie = fx.cookie();

    let (status, body) = get(
        &fx,
        &cookie,
        &format!(
            "/api/projects/worktrees?path={}",
            urlencode(&repo.to_string_lossy())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body["worktrees"].as_array().unwrap();
    assert_eq!(list[0]["branch"], "main");
    assert_eq!(list[0]["main"], true);
    if added {
        assert_eq!(list.len(), 2, "{body}");
        assert_eq!(list[1]["branch"], "feature");
        assert_eq!(list[1]["main"], false);
    }

    // 已知项目之外的路径一律拒绝：这个端点不是任意目录的探测器。
    let (status, body) = get(&fx, &cookie, "/api/projects/worktrees?path=/etc").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "bad_request");
}

#[tokio::test]
async fn agents_come_from_the_adapter_and_config_overrides_the_default_command() {
    // 前端的 Agent 下拉不写死任何 agent 名：名字与默认命令都从这里来（ADR-002 D2）。
    let mut fx = Fx::new();
    let cookie = fx.cookie();
    let (status, body) = get(&fx, &cookie, "/api/agents").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let agents = body["agents"].as_array().unwrap();
    assert!(agents.len() >= 4, "{body}");
    let shell = agents.iter().find(|a| a["name"] == "shell").unwrap();
    // 登录 shell 由运行时展开，不是写死的 /bin/zsh（ADR-001 D7）。
    assert_eq!(shell["command"], "$SHELL");
    for a in agents {
        assert!(
            !a["command"].as_str().unwrap().contains('/'),
            "默认命令必须可移植: {a}"
        );
    }

    let first = agents[0]["name"].as_str().unwrap().to_owned();
    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert(
        first.clone(),
        agora::config::AgentOverride {
            command: Some("my-wrapper".into()),
        },
    );
    fx.state.agents = Arc::new(overrides);
    let (_, body) = get(&fx, &cookie, "/api/agents").await;
    let a = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == first.as_str())
        .unwrap()
        .clone();
    assert_eq!(a["command"], "my-wrapper");
}

#[tokio::test]
async fn create_falls_back_to_the_adapter_default_command() {
    // 不给 command 时落到 Adapter 的 default_command，而不是 agent_type 本身
    // （agent_type "shell" 当命令跑，得到的是 "shell: command not found"）。
    let fx = Fx::new();
    let cookie = fx.cookie();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/sessions")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "display_name": "s", "agent_type": "shell", "working_directory": "/tmp" })
                .to_string(),
        ))
        .unwrap();
    let resp = fx.app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["command"], "$SHELL", "{body}");
}

/// 只编码路径里会出现的字符；测试用不着完整的 percent-encoding。
fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' => c.to_string(),
            c => format!("%{:02X}", c as u32),
        })
        .collect()
}
