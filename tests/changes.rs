//! 只读产出的守卫（MISSION §6.3「看结果」；A41，agora-h1k.5）：`GET /api/sessions/:id/changes` 列改动
//! 文件，`WS /api/sessions/:id/diff` 是个只读终端；整条链路对仓库零写操作。
//!
//! 在 tmp 下 `git init` 真仓库，会话跑在 FakeRuntime 上（不起进程），`working_directory` 指向仓库——
//! 对话框选 linked worktree 时填的就是它的路径（`worktree` 字段是分支名）。git / WS 的辅助抄自
//! tests/worktree_create.rs 与 tests/forward.rs：本批 tests/common 谁都不改（合并归 agora-7ku.9）。

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use common::{Fx, HOST};

/// 在 `dir` 里跑一条 git，返回原样 stdout；失败就 panic 带 stderr。
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("起 git");
    assert!(
        out.status.success(),
        "git {args:?} 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `git init -b main` + 首个 commit（写进 `files`）；返回 false 表示本机没有可用的 git
/// （调用方跳过，与 tests/worktree_create.rs 同一取舍）。
fn git_init(dir: &Path, files: &[(&str, &str)]) -> bool {
    std::fs::create_dir_all(dir).unwrap();
    let probe = Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        eprintln!("跳过：本机没有可用的 git");
        return false;
    }
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "t"]);
    for (name, body) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "init"]);
    true
}

/// 去掉 `ESC [ … m` 一类 CSI 序列：diff 终端带 `--color=always`，`+` 与 `b` 之间夹着颜色码。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 仓库的"没被写过"的证据：工作区状态 + reflog 一字不差。
fn snapshot(repo: &Path) -> (String, String) {
    (
        git(repo, &["status", "--porcelain"]),
        git(repo, &["reflog", "--no-color"]),
    )
}

async fn call(
    fx: &Fx,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
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

/// 以人的身份在 `cwd` 起一个假运行时会话，返回全局 id。
async fn create_session(fx: &Fx, cookie: &str, cwd: &Path) -> String {
    let (status, body) = call(
        fx,
        cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({
            "display_name": "worker",
            "agent_type": "shell",
            "working_directory": cwd,
            "worktree": "main",
            "command": "sleep 300",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

async fn changes(fx: &Fx, cookie: &str, gid: &str) -> (StatusCode, Value) {
    call(
        fx,
        cookie,
        Method::GET,
        &format!("/api/sessions/{gid}/changes"),
        None,
    )
    .await
}

async fn session_count(fx: &Fx, cookie: &str) -> usize {
    let (status, body) = call(fx, cookie, Method::GET, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["sessions"].as_array().unwrap().len()
}

// ---------- GET /changes ----------

/// 改动、新文件、已 add 的新文件、删除、重命名各一，列表按路径排序、status 按类型、branch 是 main；
/// 再看一眼仓库：列改动没有写过任何东西。
#[tokio::test]
async fn lists_modified_files_from_porcelain() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    if !git_init(
        &repo,
        &[
            ("a.txt", "a\n"),
            ("b.txt", "b\n"),
            ("del.txt", "gone\n"),
            ("sub/keep.txt", ""),
        ],
    ) {
        return;
    }
    // 干净树：空列表、reason null、branch main。
    let fx = Fx::new();
    let cookie = fx.cookie();
    let gid = create_session(&fx, &cookie, &repo).await;
    let (status, body) = changes(&fx, &cookie, &gid).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "files": [], "branch": "main", "reason": null })
    );

    std::fs::write(repo.join("a.txt"), "a\nchanged\n").unwrap();
    std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
    std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
    git(&repo, &["add", "staged.txt"]);
    std::fs::remove_file(repo.join("del.txt")).unwrap();
    git(&repo, &["mv", "b.txt", "c.txt"]);
    let before = snapshot(&repo);

    let (status, body) = changes(&fx, &cookie, &gid).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["branch"], "main");
    assert_eq!(body["reason"], Value::Null);
    assert_eq!(
        body["files"],
        json!([
            { "path": "a.txt", "status": "modified" },
            { "path": "c.txt", "status": "renamed" },
            { "path": "del.txt", "status": "deleted" },
            { "path": "new.txt", "status": "untracked" },
            { "path": "staged.txt", "status": "added" },
        ]),
        "{}",
        body["files"]
    );
    assert_eq!(snapshot(&repo), before, "列改动不得写仓库");

    // 裸 id 也行（与其它会话端点同一约定）。
    let bare = gid.split_once(':').unwrap().1;
    let (status, bare_body) = changes(&fx, &cookie, bare).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bare_body, body);
}

/// 不是仓库 / 目录不在 都是 200 + 空列表 + 类型原因（不是错误横幅）；会话不存在才 404。
#[tokio::test]
async fn non_repo_yields_typed_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    // 没 git 就跑不出 not_a_repo；no_directory 与 404 不需要 git，照样验。
    let has_git = Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let fx = Fx::new();
    let cookie = fx.cookie();
    if has_git {
        let gid = create_session(&fx, &cookie, &plain).await;
        let (status, body) = changes(&fx, &cookie, &gid).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({ "files": [], "branch": null, "reason": "not_a_repo" })
        );
    }

    let missing = tmp.path().join("does-not-exist");
    let gid = create_session(&fx, &cookie, &missing).await;
    let (status, body) = changes(&fx, &cookie, &gid).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "files": [], "branch": null, "reason": "no_directory" })
    );

    let (status, body) = changes(&fx, &cookie, "testnode:nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not_found");
}

// ---------- WS /diff（真 WS：同源校验要 Origin 与 Host 对得上） ----------

async fn listen(fx: &Fx) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = fx.app();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr.to_string()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: &str, path: &str, cookie: &str) -> Ws {
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, format!("http://{addr}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("同源 + cookie 应升级成功");
    ws
}

/// 下一条文本帧（Ping / Pong 跳过）；5 s 没有就 panic。
async fn next_text(ws: &mut Ws) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(left, ws.next())
            .await
            .expect("5 s 内应有帧")
            .expect("流未结束")
            .unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// diff 终端：第一帧 status read_only；输出里有 diff 文本；input 帧丢弃不出错、不回显；跑完给 exit 帧
/// 然后关闭；全程侧栏不多一行、仓库的 status 与 reflog 一字不差。
#[tokio::test]
async fn diff_terminal_is_read_only_and_ephemeral() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    if !git_init(&repo, &[("a.txt", "a\n")]) {
        return;
    }
    std::fs::write(repo.join("a.txt"), "a\nb\n").unwrap();
    let before = snapshot(&repo);

    let fx = Fx::new();
    let cookie = fx.cookie();
    let gid = create_session(&fx, &cookie, &repo).await;
    let rows_before = session_count(&fx, &cookie).await;
    let addr = listen(&fx).await;
    let mut ws = connect(
        &addr,
        &format!("/api/sessions/{gid}/diff?cols=100&rows=30"),
        &cookie,
    )
    .await;

    let first = next_text(&mut ws).await;
    assert_eq!(
        first,
        json!({ "type": "status", "status": "read_only" }),
        "第一帧必须声明只读"
    );
    // 敲键盘：桥直接丢弃。要是它进了 PTY，tty 会把它回显到输出里。
    ws.send(Message::Text(
        r#"{"type":"input","data":"SHOULD_NOT_ECHO\n"}"#.into(),
    ))
    .await
    .expect("只读终端收到 input 不该断连");

    let mut out = String::new();
    let exit = loop {
        let v = next_text(&mut ws).await;
        match v["type"].as_str() {
            Some("output") => out.push_str(v["data"].as_str().unwrap()),
            Some("exit") => break v,
            Some("pong") => {}
            other => panic!("不该有的帧 {other:?}: {v}"),
        }
    };
    let plain = strip_ansi(&out);
    assert!(plain.contains("+b"), "输出里应有 diff 的新增行: {plain:?}");
    assert!(plain.contains("a.txt"), "输出里应有文件名: {plain:?}");
    assert!(
        !plain.contains("SHOULD_NOT_ECHO"),
        "input 进了 PTY（被回显）: {plain:?}"
    );
    assert_eq!(
        exit["exit"],
        json!({ "kind": "code", "value": 0 }),
        "{exit}"
    );

    // exit 之后只剩 Close / 流结束。
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => panic!("exit 之后不该再有帧: {t}"),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "exit 之后 5 s 内应关闭");

    // 不进 sessions 表、不碰运行时；仓库没被写过。
    assert_eq!(session_count(&fx, &cookie).await, rows_before);
    assert_eq!(fx.rt.sessions.lock().unwrap().len(), 1);
    assert!(
        fx.rt.inputs.lock().unwrap().is_empty(),
        "input 不得到达运行时"
    );
    assert_eq!(snapshot(&repo), before, "看 diff 不得写仓库");
}
