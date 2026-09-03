//! 真实 tmux + fake-agent 的端到端不变量（agora-3la 的骨架；A8 A9 A10 的网关侧）。
//! 不变量 1–5、7 的逐条守卫在 agora-xqa.13 补全，这里先钉住"浏览器怎么折腾 agent 都不死"。

mod common;

use std::time::Duration;

use agora::runtime::Exit;
use agora::status::Status;
use axum::http::header;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

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

async fn send_input(ws: &mut Ws, data: &str) {
    let frame = serde_json::json!({ "type": "input", "data": data }).to_string();
    ws.send(Message::Text(frame.into())).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_in_one_process_are_isolated() {
    let mut a = TmuxNode::new();
    let mut b = TmuxNode::new();
    a.serve().await;
    b.serve().await;
    let sa = a.create_fake("a", "print A-READY; sleep 60000");
    let sb = b.create_fake("b", "print B-READY; read; exit 3");
    a.wait(&sa.record.id, |v| v.alive);
    b.wait(&sb.record.id, |v| v.alive);

    // 各看各的。
    let ids_a: Vec<String> = a
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .map(|v| v.record.id)
        .collect();
    let ids_b: Vec<String> = b
        .sessions
        .list()
        .unwrap()
        .into_iter()
        .map(|v| v.record.id)
        .collect();
    assert_eq!(ids_a, vec![sa.record.id.clone()]);
    assert_eq!(ids_b, vec![sb.record.id.clone()]);
    assert!(
        a.sessions.unregistered().unwrap().is_empty(),
        "A 不该看到 B 的运行时会话"
    );

    // 杀 A 的会话，B 的纹丝不动；B 的退出码经它自己的节点可读。
    a.sessions.kill(&sa.record.id).unwrap();
    a.wait(&sa.record.id, |v| !v.alive);
    assert!(b.pane_alive(&sb), "B 的 agent 不受 A 的 kill 影响");
    let mut ws = terminal(&b, &b.gid(&sb.record.id)).await;
    output_until(&mut ws, "B-READY").await;
    send_input(&mut ws, "bye\r").await;
    output_until(&mut ws, "read:bye").await;
    let v = b.wait(&sb.record.id, |v| v.exit.is_some());
    assert_eq!(v.exit, Some(Exit::Code(3)));
    assert_eq!(v.assessment.status, Status::Failed);
}

/// A8 刷新、A9 关浏览器、A10 daemon 死掉：三种折腾之后 agent 都还在，且输入仍能送达。
#[tokio::test(flavor = "multi_thread")]
async fn refresh_close_and_daemon_crash_keep_agent_alive() {
    let mut node = TmuxNode::new();
    node.serve().await;
    let s = node.create_fake(
        "agent",
        "ignore-hup; print READY; read; print AFTER; exit 5",
    );
    let id = s.record.id.clone();
    let gid = node.gid(&id);
    node.wait(&id, |v| v.alive);

    // A9：连上再直接断开（关浏览器）。
    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    drop(ws);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(node.pane_alive(&s), "关浏览器后 agent 不能退出");

    // A8：刷新 = 再连一次；两个客户端同看也行。
    let mut ws1 = terminal(&node, &gid).await;
    let mut ws2 = terminal(&node, &gid).await;
    output_until(&mut ws1, "READY").await;
    output_until(&mut ws2, "READY").await;
    drop(ws2);

    // A10：daemon 死掉。这里的 crash 只取消监听器：axum 把升级后的连接交给独立任务，
    // 它们要等对端断开才结束（真实进程死亡时 PTY master 随进程关闭，attach 自行退出）。
    // 所以再把还开着的 ws1 断掉，模拟浏览器那边看到 WS 失败。
    node.crash();
    drop(ws1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        node.pane_alive(&s),
        "daemon 死后 agent 不能退出（不变量 3）"
    );
    let clients = std::process::Command::new("tmux")
        .args(["-L", &node.socket, "list-clients"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&clients.stdout).trim().is_empty(),
        "daemon 死后 attach 客户端应被收走: {}",
        String::from_utf8_lossy(&clients.stdout)
    );

    // daemon 重启：重建对象、reconcile，会话仍在且活着；重新 attach 输入照样送达。
    let m = node.rebuild();
    assert!(m.get(&id).unwrap().alive);
    node.serve().await;
    let mut ws = terminal(&node, &gid).await;
    output_until(&mut ws, "READY").await;
    send_input(&mut ws, "go\r").await;
    output_until(&mut ws, "AFTER").await;
    let v = node.wait(&id, |v| v.exit.is_some());
    assert_eq!(v.exit, Some(Exit::Code(5)));
    assert!(
        node.tail(&v).contains("READY"),
        "scrollback 还在: {}",
        node.tail(&v)
    );
}
