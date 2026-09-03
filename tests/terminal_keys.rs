//! 键位到得了 pane 吗（agora-xqa.14 验收 / agora-xqa.3；MISSION §6.5）。
//!
//! 前端那一层"不吞 Ctrl 键、Shift+Enter 发 ESC CR、Cmd+←/→ 发 Home/End"由
//! `web/src/keys.test.ts` 与 `web/src/keyboard.test.tsx` 守；这里守另一半：这些字节
//! 经 `WS /api/sessions/:id/terminal` 原封不动落进 pane，中间没人替它做主。
//!
//! pane 里跑的是 `stty raw -echo; cat -v`：raw 模式下 Ctrl+C/Z 不再是信号而是普通字节，
//! `cat -v` 把每个控制字符回显成 `^X`——于是"到了没到"变成一句可断言的文本。

mod common;

use std::time::Duration;

use axum::http::header;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use agora::runtime::Size;
use agora::session::NewSession;
use common::node::TmuxNode;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn terminal(node: &TmuxNode, gid: &str) -> Ws {
    let addr = node.addr.clone().expect("节点已 serve");
    let mut req = format!("ws://{addr}/api/sessions/{gid}/terminal")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert(header::COOKIE, node.cookie().parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, format!("http://{addr}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn send_input(ws: &mut Ws, data: &str) {
    let frame = serde_json::json!({ "type": "input", "data": data }).to_string();
    ws.send(Message::Text(frame.into())).await.unwrap();
}

/// 攒输出直到出现 `needle`。
async fn output_until(ws: &mut Ws, needle: &str) -> String {
    let mut got = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !got.contains(needle) {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(left, ws.next())
            .await
            .unwrap_or_else(|_| panic!("等 {needle:?} 超时，已收到: {got:?}"))
            .expect("流未结束")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "output" {
                got.push_str(v["data"].as_str().unwrap());
            }
        }
    }
    got
}

/// 同上，命中任一 needle 即止。
async fn output_until_any(ws: &mut Ws, needles: &[&str]) -> String {
    let mut got = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !needles.iter().any(|n| got.contains(n)) {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(left, ws.next())
            .await
            .unwrap_or_else(|_| panic!("等 {needles:?} 超时，已收到: {got:?}"))
            .expect("流未结束")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "output" {
                got.push_str(v["data"].as_str().unwrap());
            }
        }
    }
    got
}

/// 起一个"把收到的每个控制字符念出来"的会话。
fn create_raw_cat(node: &TmuxNode) -> String {
    let script = node.home.join("rawcat.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nstty raw -echo\nprintf 'RAWCAT-READY\\r\\n'\nexec cat -v\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    node.sessions
        .create(&NewSession {
            display_name: "rawcat".into(),
            agent_type: "shell".into(),
            working_directory: node.home.clone(),
            worktree: None,
            task_ref: None,
            command: script.to_string_lossy().into_owned(),
            env: vec![],
            size: Size::default(),
        })
        .unwrap()
        .record
        .id
}

#[tokio::test(flavor = "multi_thread")]
async fn control_keys_and_the_keys_agora_remaps_all_reach_the_pane() {
    let mut node = TmuxNode::new();
    node.serve().await;
    let id = create_raw_cat(&node);
    node.wait(&id, |v| v.alive);
    let mut ws = terminal(&node, &node.gid(&id)).await;
    output_until(&mut ws, "RAWCAT-READY").await;

    // MISSION §6.5 硬约束点名的六个键：Ctrl+C/D/Z/R/A/E。
    send_input(&mut ws, "\u{3}\u{4}\u{1a}\u{12}\u{1}\u{5}").await;
    let got = output_until(&mut ws, "^C^D^Z^R^A^E").await;
    assert!(
        got.contains("^C^D^Z^R^A^E"),
        "六个 Ctrl 键要原样到 pane: {got:?}"
    );

    // agora 替浏览器代发的两组（web/src/keys.ts）：Shift+Enter 与 Cmd+←/→。
    // 断言的是"字节没被谁改写"，TUI 拿它做什么是 TUI 的事。
    send_input(&mut ws, "\u{1b}\r").await;
    let got = output_until(&mut ws, "^[^M").await;
    assert!(got.contains("^[^M"), "Shift+Enter 的 ESC CR: {got:?}");

    // Home/End 会被中间那个 attach 客户端**重新编码**：它认得 `ESC[H` / `ESC[F` 是 Home /
    // End，于是照 pane 的终端类型改发 `ESC[1~` / `ESC[4~`（macOS tmux 3.7c 实测 2026-09-03）。
    // 键义没丢——这正是我们要的"和真键盘按下 Home 一模一样"，所以两种编码都算到。
    send_input(&mut ws, "\u{1b}[H\u{1b}[F").await;
    let got = output_until_any(&mut ws, &["^[[H^[[F", "^[[1~^[[4~"]).await;
    assert!(
        got.contains("^[[H^[[F") || got.contains("^[[1~^[[4~"),
        "Cmd+←/→ 要以 Home/End 的身份到 pane: {got:?}"
    );
}
