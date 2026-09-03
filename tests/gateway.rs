//! Terminal Gateway 守卫（ADR-001 D5；A6–A10 的机械部分）。
//!
//! 核心不变量：WS 断开只能带走 attach 客户端，绝不能把 `^D` 送进 pane。用 `sh` 扮演 attach，
//! 不需要 tmux：网关本来就不认识 tmux。

mod common;

use std::time::Duration;

use agora::gateway::{AttachedPty, DETACH_GRACE};
use agora::runtime::{AttachSpec, Exit, Size};
use axum::http::header;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use common::Fx;

fn sh(script: &str) -> AttachSpec {
    AttachSpec {
        argv: vec!["sh".into(), "-c".into(), script.into()],
        env: vec![],
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) 只探测，不发信号。
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

async fn read_until(pty: &mut AttachedPty, needle: &str, within: Duration) -> String {
    let mut got = String::new();
    let deadline = tokio::time::Instant::now() + within;
    while !got.contains(needle) {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !left.is_zero(),
            "{within:?} 内没等到 {needle:?}，已收到: {got:?}"
        );
        match tokio::time::timeout(left, pty.read()).await {
            Ok(Some(bytes)) => got.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(None) => panic!("PTY 读端提前结束，已收到: {got:?}"),
            Err(_) => panic!("{within:?} 内没等到 {needle:?}，已收到: {got:?}"),
        }
    }
    got
}

/// 守卫（ADR-001 D5）：attach 忽略 SIGHUP、SIGHUP 后不退出 → 网关必须泄漏 writer，
/// 而不是 drop 它写入 `\n^D`。假 attach 用 `read` 等一行：^D 到达就退出并留下证据文件。
#[tokio::test]
async fn never_writes_eof_to_live_attach() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("eof-received");
    let script = format!(
        "trap '' HUP; echo READY; read -r line; echo \"$line\" > {}; exit 0",
        marker.display()
    );
    let mut pty = AttachedPty::spawn(&sh(&script), Size::default()).unwrap();
    let pid = pty.pid().expect("有 pid");
    read_until(&mut pty, "READY", Duration::from_secs(5)).await;

    let started = std::time::Instant::now();
    let confirmed = pty.detach().await;
    assert!(!confirmed, "忽略 SIGHUP 的 attach 不可能被确认退出");
    assert!(
        started.elapsed() >= DETACH_GRACE,
        "必须等满宽限期才放弃，不能提前释放"
    );
    // 泄漏 writer 之后再等一会：如果 ^D 曾被写入，read 会返回、marker 会出现。
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !marker.exists(),
        "PTY 里写入了 EOF：attach 还活着时 ^D 会打进 pane 杀掉 agent"
    );
    assert!(alive(pid), "假 attach 应仍在运行（它只是忽略了 SIGHUP）");
    // SAFETY: 收尾杀掉本测试自己起的进程。
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

/// 正常路径：attach 尊重 SIGHUP → 退出被确认 → writer 释放，进程不残留。
#[tokio::test]
async fn sighup_detaches_and_releases_after_exit_confirmed() {
    let mut pty = AttachedPty::spawn(&sh("echo READY; sleep 30"), Size::default()).unwrap();
    let pid = pty.pid().unwrap();
    read_until(&mut pty, "READY", Duration::from_secs(5)).await;
    let confirmed = pty.detach().await;
    assert!(confirmed);
    // 已被 wait 回收；kill(pid,0) 对已回收的 pid 应失败（除非 pid 被复用，极小概率）。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!alive(pid), "attach 进程应已退出并被回收");
}

/// 输入到达 PTY、输出回到 channel、resize 改变 tty 尺寸；attach 自己退出时 exit 有值。
#[tokio::test]
async fn input_output_resize_and_exit_round_trip() {
    let mut pty = AttachedPty::spawn(
        &sh("stty -echo; read -r l; echo \"got:$l\"; stty size; exit 7"),
        Size { cols: 80, rows: 24 },
    )
    .unwrap();
    assert!(pty.resize(Size {
        cols: 100,
        rows: 30
    }));
    pty.write(b"hello\n".to_vec()).await;
    let out = read_until(&mut pty, "30 100", Duration::from_secs(5)).await;
    assert!(out.contains("got:hello"), "{out:?}");
    let mut exit = pty.exit_signal();
    let code = tokio::time::timeout(Duration::from_secs(5), exit.wait())
        .await
        .unwrap();
    assert_eq!(code, Exit::Code(7));
    assert!(pty.has_exited());
    assert!(pty.detach().await, "已退出的 attach 直接释放，不用再等");
}

// ---------- WS 端点 ----------

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

async fn connect(
    addr: &str,
    path: &str,
    cookie: &str,
    origin: &str,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, origin.parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

/// 收文本帧直到某条满足 `pred`；Ping 等控制帧跳过。
async fn next_json(ws: &mut Ws, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(left, ws.next())
            .await
            .expect("5 s 内应有帧")
            .expect("流未结束")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if pred(&v) {
                return v;
            }
        }
    }
}

fn fx_with_attach(script: &str) -> (Fx, String) {
    let fx = Fx::new();
    *fx.rt.attach_argv.lock().unwrap() = vec!["sh".into(), "-c".into(), script.into()];
    fx.rt.insert("fake:agora:s1", true, None, true);
    let view = fx
        .sessions
        .adopt(&agora::session::AdoptSession {
            runtime_ref: "fake:agora:s1".into(),
            display_name: Some("s1".into()),
            agent_type: Some("shell".into()),
            working_directory: None,
        })
        .unwrap();
    let gid = format!("{}:{}", common::NODE, view.record.id);
    (fx, gid)
}

#[tokio::test]
async fn terminal_ws_streams_input_output_resize_and_exit() {
    let (fx, gid) = fx_with_attach("stty -echo; read -r l; echo \"got:$l\"; stty size; exit 3");
    let cookie = fx.cookie();
    let addr = listen(&fx).await;
    let mut ws = connect(
        &addr,
        &format!("/api/sessions/{gid}/terminal?cols=80&rows=24"),
        &cookie,
        &format!("http://{addr}"),
    )
    .await
    .expect("同源 + cookie 应升级成功");

    let st = next_json(&mut ws, |v| v["type"] == "status").await;
    assert_eq!(st["status"], "attached");

    ws.send(Message::Text(
        r#"{"type":"resize","cols":100,"rows":30}"#.into(),
    ))
    .await
    .unwrap();
    ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
        .await
        .unwrap();
    next_json(&mut ws, |v| v["type"] == "pong").await;
    ws.send(Message::Text(r#"{"type":"input","data":"hi\n"}"#.into()))
        .await
        .unwrap();

    let mut out = String::new();
    let exit = loop {
        let v = next_json(&mut ws, |v| v["type"] == "output" || v["type"] == "exit").await;
        if v["type"] == "exit" {
            break v;
        }
        out.push_str(v["data"].as_str().unwrap());
    };
    assert!(out.contains("got:hi"), "{out:?}");
    assert!(out.contains("30 100"), "resize 应到达 tty: {out:?}");
    assert_eq!(exit["exit"], serde_json::json!({"kind":"code","value":3}));
}

/// 两个客户端各自一条 attach：互不干扰，各自独立退出。
#[tokio::test]
async fn two_clients_get_independent_streams() {
    let (fx, gid) = fx_with_attach("echo READY-$$; read -r l; echo bye-$l; exit 0");
    let cookie = fx.cookie();
    let addr = listen(&fx).await;
    let path = format!("/api/sessions/{gid}/terminal");
    let origin = format!("http://{addr}");
    let mut a = connect(&addr, &path, &cookie, &origin).await.unwrap();
    let mut b = connect(&addr, &path, &cookie, &origin).await.unwrap();
    let ra = next_json(&mut a, |v| v["type"] == "output").await;
    let rb = next_json(&mut b, |v| v["type"] == "output").await;
    assert_ne!(ra["data"], rb["data"], "两条流应是两个 attach 进程");
    a.send(Message::Text(r#"{"type":"input","data":"A\n"}"#.into()))
        .await
        .unwrap();
    let ea = next_json(&mut a, |v| v["type"] == "exit").await;
    assert_eq!(ea["exit"]["value"], 0);
    // b 没收到 A 的输入，也没退出。
    b.send(Message::Text(r#"{"type":"ping"}"#.into()))
        .await
        .unwrap();
    let pb = next_json(&mut b, |v| v["type"] != "status").await;
    assert_eq!(pb["type"], "pong", "b 应仍然活着: {pb:?}");
}

/// A8/A9 的网关侧：客户端断开后 attach 进程被 SIGHUP 收走，且没有 ^D 进入 PTY。
#[tokio::test]
async fn client_disconnect_hangs_up_attach_without_eof() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("eof");
    let pidfile = dir.path().join("pid");
    // 尊重 HUP（默认），但若在 HUP 之前先读到 EOF 就留证据。
    let (fx, gid) = fx_with_attach(&format!(
        "echo $$ > {}; echo READY; read -r l; echo eof > {}",
        pidfile.display(),
        marker.display()
    ));
    let cookie = fx.cookie();
    let addr = listen(&fx).await;
    let mut ws = connect(
        &addr,
        &format!("/api/sessions/{gid}/terminal"),
        &cookie,
        &format!("http://{addr}"),
    )
    .await
    .unwrap();
    next_json(&mut ws, |v| {
        v["type"] == "output" && v["data"].as_str().unwrap().contains("READY")
    })
    .await;
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(pid));
    drop(ws);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while alive(pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(pid), "WS 断开后 attach 进程应被 SIGHUP 收走");
    assert!(!marker.exists(), "attach 在退出前读到了 EOF：^D 进了 PTY");
}

#[tokio::test]
async fn terminal_ws_rejects_cross_origin_and_unknown_session() {
    let (fx, gid) = fx_with_attach("cat");
    let cookie = fx.cookie();
    let addr = listen(&fx).await;
    let err = connect(
        &addr,
        &format!("/api/sessions/{gid}/terminal"),
        &cookie,
        "http://evil.example",
    )
    .await
    .expect_err("跨站应被拒绝");
    assert!(err.to_string().contains("403"), "{err}");

    let err = connect(
        &addr,
        &format!("/api/sessions/{}:nope/terminal", common::NODE),
        &cookie,
        &format!("http://{addr}"),
    )
    .await
    .expect_err("不存在的会话应在升级前 404");
    assert!(err.to_string().contains("404"), "{err}");
}
