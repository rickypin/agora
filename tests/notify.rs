//! A54：真实 CLI / daemon、本机鉴权与不确定提交；所有实例独立 home / socket。
mod common;

use agora::{
    cli::notify::submit,
    events::{Event, EventBus},
    local::{self, Request, Response},
    notify::Receiver,
};
use common::isolate;
use futures_util::{SinkExt, StreamExt};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const BIN: &str = env!("CARGO_BIN_EXE_agora");

struct Daemon {
    home: PathBuf,
    socket: String,
    child: std::process::Child,
}
impl Daemon {
    async fn start(enabled: bool, broken_vapid: bool) -> (Self, String, String) {
        let n = isolate::nth();
        let home = isolate::home_dir("notify", n);
        local::ensure_home(&home).unwrap();
        let socket = isolate::socket_name("notify", n);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        std::fs::write(home.join("config.yaml"), format!("node:\n  id: notify\nserver:\n  listen: {addr}\nruntime:\n  tmux:\n    socket: {socket}\n    adopt_sockets: []\nnotifications:\n  enabled: {enabled}\n")).unwrap();
        if broken_vapid {
            std::fs::create_dir(home.join("push")).unwrap();
            std::fs::write(home.join("push/vapid.json"), "broken").unwrap();
        }
        let db = Arc::new(agora::session::Db::open(&home.join("agora.db")).unwrap());
        let auth = agora::auth::Auth::new(db, Default::default());
        let token = auth
            .mint_pair_token(agora::auth::PairedVia::Socket)
            .unwrap();
        let (_, plain) = auth.redeem(&token, None, None).unwrap();
        let log = std::fs::File::create(home.join("test.log")).unwrap();
        let child = std::process::Command::new(BIN)
            .arg("serve")
            .env("AGORA_HOME", &home)
            .env("AGORA_LOG_FORMAT", "json")
            .env("AGORA_LOG", "info")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut fixture = Self {
            home,
            socket,
            child,
        };
        tokio::time::timeout(isolate::PROC, async {
            loop {
                assert!(
                    fixture.child.try_wait().unwrap().is_none(),
                    "daemon stopped: {}",
                    std::fs::read_to_string(fixture.home.join("test.log")).unwrap()
                );
                if matches!(
                    local::request(&fixture.home.join(local::SOCKET_FILE), &Request::Ping).await,
                    Ok(Response::Pong)
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("daemon ready");
        (fixture, addr, format!("agora_session={plain}"))
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        isolate::kill_tmux(&self.socket);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

async fn cli(home: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = tokio::process::Command::new(BIN)
        .arg("notify")
        .args(args)
        .env("AGORA_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).await.unwrap();
    tokio::time::timeout(isolate::PROC, child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn real_daemon_publishes_without_push_and_limits_calls() {
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    for broken_vapid in [false, true] {
        let (daemon, addr, cookie) = Daemon::start(true, broken_vapid).await;
        let mut req = format!("ws://{addr}/api/events")
            .into_client_request()
            .unwrap();
        req.headers_mut().insert("cookie", cookie.parse().unwrap());
        req.headers_mut()
            .insert("origin", format!("http://{addr}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
            .await
            .unwrap();
        while ws.next().await.unwrap().unwrap().to_text().unwrap() != r#"{"type":"pong"}"# {}
        // No subscriptions. Broken VAPID also disables the sender entirely.
        let out = cli(
            &daemon.home,
            &[],
            "  提醒\r\n\r\n  私有正文\x00\r\n".as_bytes(),
        )
        .await;
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert_eq!(out.stdout, "已发布通知\n".as_bytes());
        assert!(out.stderr.is_empty());
        // Bypassing the CLI cannot bypass daemon validation, and rejected input spends no quota.
        for (title, reason) in [
            ("\u{001b}".into(), agora::notify::Rejection::EmptyTitle),
            ("x".repeat(65537), agora::notify::Rejection::TooLarge),
        ] {
            assert_eq!(
                local::request(
                    &daemon.home.join(local::SOCKET_FILE),
                    &Request::Notify {
                        title,
                        body: "".into()
                    }
                )
                .await
                .unwrap(),
                Response::NotifyRejected { reason }
            );
        }
        let event = tokio::time::timeout(isolate::PROC, async {
            loop {
                let text = ws.next().await.unwrap().unwrap().into_text().unwrap();
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                if let Some(e) = v
                    .as_array()
                    .and_then(|a| a.iter().find(|e| e["type"] == "notification"))
                {
                    break e.clone();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            event,
            serde_json::json!({"type":"notification","id":null,"title":"提醒","body":"  私有正文","status":null})
        );
        for _ in 1..5 {
            assert_eq!(
                cli(&daemon.home, &[], b"same title\nbody")
                    .await
                    .status
                    .code(),
                Some(0)
            );
        }
        let out = cli(&daemon.home, &[], b"sixth").await;
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("限流"));
        let logs = std::fs::read_to_string(daemon.home.join("test.log")).unwrap();
        assert!(
            !logs.contains("私有正文"),
            "body must not enter daemon logs"
        );
    }
}

#[tokio::test]
async fn cli_rejects_disabled_invalid_missing_daemon_and_usage() {
    let (daemon, _, _) = Daemon::start(false, false).await;
    let out = cli(&daemon.home, &[], b"t\nb").await;
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("enabled=false"));
    for input in [b"\nbody".to_vec(), vec![0xff], vec![b'x'; 65537]] {
        assert_eq!(cli(&daemon.home, &[], &input).await.status.code(), Some(1));
    }
    assert_eq!(
        cli(&daemon.home, &["--invalid"], b"").await.status.code(),
        Some(2)
    );
    let missing = daemon.home.join("absent");
    assert_eq!(
        cli(&daemon.home, &["--home", missing.to_str().unwrap()], b"t")
            .await
            .status
            .code(),
        Some(1)
    );
    assert!(
        !missing.exists(),
        "CLI does not create a home or start a daemon"
    );
}

#[tokio::test]
async fn cli_whole_deadline_covers_stdin_without_eof() {
    let home = isolate::home_dir("notify", isolate::nth());
    let mut child = tokio::process::Command::new(BIN)
        .arg("notify")
        .env("AGORA_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"title\nbody").await.unwrap();
    let start = std::time::Instant::now();
    let out = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .expect("CLI must exit despite blocked stdin")
        .unwrap();
    drop(stdin);
    assert_eq!(out.status.code(), Some(1));
    assert!(start.elapsed() >= Duration::from_secs(9));
    assert!(String::from_utf8_lossy(&out.stderr).contains("超时"));
}

#[tokio::test]
async fn cli_works_with_only_home_and_explicit_home_takes_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".agora");
    local::ensure_home(&home).unwrap();
    let path = home.join(local::SOCKET_FILE);
    let p = path.clone();
    let task = tokio::spawn(async move {
        local::serve(
            &p,
            local::sync_handler(|r| {
                assert!(matches!(r, Request::Notify { .. }));
                Response::Notified
            }),
        )
        .await
    });
    while !path.exists() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut child = tokio::process::Command::new(BIN)
        .arg("notify")
        .env_clear()
        .env("HOME", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"only HOME\nworks")
        .await
        .unwrap();
    let out = tokio::time::timeout(isolate::PROC, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let out = cli(
        &dir.path().join("missing"),
        &["--home", home.to_str().unwrap()],
        b"explicit",
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    task.abort();
}

#[tokio::test]
async fn reading_input_and_waiting_for_response_share_one_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::UnixListener::bind(dir.path().join(local::SOCKET_FILE)).unwrap();
    let (mut write, read) = tokio::io::duplex(64);
    let input_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(180)).await;
        write.write_all(b"title\nbody").await.unwrap();
    });
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut String::new()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(180)).await;
        let _ = reader
            .get_mut()
            .write_all(b"{\"result\":\"notified\"}\n")
            .await;
    });
    assert_eq!(
        submit(read, dir.path(), Duration::from_millis(300))
            .await
            .unwrap_err()
            .code,
        4
    );
    input_task.await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn publication_with_lost_reply_is_unknown_and_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::UnixListener::bind(dir.path().join(local::SOCKET_FILE)).unwrap();
    let bus = EventBus::default();
    let mut events = bus.subscribe();
    let receiver = Receiver::new(bus, true);
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        let Request::Notify { title, body } = serde_json::from_str(&line).unwrap() else {
            panic!("notify")
        };
        receiver.publish(&title, &body).unwrap();
        // The reply is lost after publication; no retry may follow.
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
    });
    assert_eq!(
        submit(
            b"title\nbody".as_slice(),
            dir.path(),
            Duration::from_secs(2)
        )
        .await
        .unwrap_err()
        .code,
        4
    );
    assert!(matches!(
        events.recv().await.unwrap(),
        Event::Notification { id: None, .. }
    ));
    task.await.unwrap();
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn response_timeout_and_malformed_reply_are_unknown_but_rejection_is_not() {
    for (reply, code) in [
        (None, 4),
        (Some("not-json\n"), 4),
        (
            Some("{\"result\":\"notify_rejected\",\"reason\":\"rate_limited\"}\n"),
            1,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::UnixListener::bind(dir.path().join(local::SOCKET_FILE)).unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            reader.read_line(&mut String::new()).await.unwrap();
            if let Some(reply) = reply {
                reader.get_mut().write_all(reply.as_bytes()).await.unwrap();
            } else {
                std::future::pending::<()>().await;
            }
        });
        assert_eq!(
            submit(b"t\nb".as_slice(), dir.path(), Duration::from_millis(100))
                .await
                .unwrap_err()
                .code,
            code
        );
        task.abort();
    }
}

#[tokio::test]
async fn notify_other_uid_never_reaches_receiver() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(local::SOCKET_FILE);
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let handler = local::sync_handler(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Response::Notified
    });
    let p = path.clone();
    let uid = unsafe { libc::getuid() };
    let task = tokio::spawn(async move { local::serve_for_uid(&p, handler, uid + 1).await });
    while !path.exists() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let _ = submit(b"t\nb".as_slice(), dir.path(), Duration::from_secs(1)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    task.abort();
}
