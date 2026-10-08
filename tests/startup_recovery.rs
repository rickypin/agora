//! A legacy backlog must not block HTTP, pairing or peer startup (agora-o1tm).
mod common;
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::hook::{Delivery, Envelope, Inbox};
use agora::session::Db;
use common::isolate;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Fixture {
    child: Option<Child>,
    home: PathBuf,
    socket: String,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        isolate::kill_tmux(&self.socket);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn get(addr: &str, path: &str, cookie: &str) -> Option<serde_json::Value> {
    let mut stream = std::net::TcpStream::connect(addr).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut bytes = String::new();
    stream.read_to_string(&mut bytes).ok()?;
    serde_json::from_str(bytes.split_once("\r\n\r\n")?.1).ok()
}

#[test]
fn health_and_peer_start_before_large_local_recovery_finishes() {
    let n = isolate::nth();
    let home = isolate::home_dir("recovery", n);
    agora::local::ensure_home(&home).unwrap();
    let socket = isolate::socket_name("recovery", n);
    let mut fx = Fixture {
        child: None,
        home,
        socket,
    };
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(fx.home.join("config.yaml"), format!(
        "server:\n  listen: '127.0.0.1:{port}'\nruntime:\n  tmux:\n    socket: '{}'\n    adopt_sockets: []\npeers:\n  - name: probe\n    url: 'https://127.0.0.1:1'\n    token_file: '{}'\n    cert_fingerprint: 'sha256:{}'\n",
        fx.socket, fx.home.join("missing-token").display(), "a".repeat(64))).unwrap();
    let db = Arc::new(Db::open(&fx.home.join("agora.db")).unwrap());
    let auth = Auth::new(db, AuthConfig::default());
    let pair = auth.mint_pair_token(PairedVia::Socket).unwrap();
    let (_, token) = auth.redeem(&pair, None, None).unwrap();
    let cookie = format!("agora_session={token}");
    let inbox = Inbox::new(&fx.home);
    let now = agora::hook::inbox::now_unix_ms();
    for i in 0..3000 {
        inbox.write(&Delivery { envelope: Envelope {
            host: "claude".into(), agora_session_id: None, agora_epoch: None,
            agent_session_id: "offline".into(), agent_env: BTreeMap::new(), runtime_env: BTreeMap::new(),
            ppid: std::process::id(), received_at: String::new(), received_unix_ms: now - 10_000 + i,
        }, payload: serde_json::json!({"hook_event_name":"UserPromptSubmit", "prompt":"queued state"}) }).unwrap();
    }
    let log = fx.home.join("startup.log");
    let out = std::fs::File::create(&log).unwrap();
    let started = Instant::now();
    fx.child = Some(
        Command::new(env!("CARGO_BIN_EXE_agora"))
            .arg("serve")
            .env("AGORA_HOME", &fx.home)
            .env("AGORA_LOG_FORMAT", "json")
            .env("AGORA_LOG", "info")
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + isolate::PROC;
    let mut addr = None;
    while Instant::now() < deadline {
        let text = std::fs::read_to_string(&log).unwrap();
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if v["fields"]["component"] == "api" {
                    if let Some(s) = v["fields"]["addr"].as_str() {
                        addr = Some(s.to_owned());
                    }
                }
            }
        }
        if let Some(addr) = &addr {
            if let Some(health) = get(addr, "/api/health", &cookie) {
                assert_eq!(
                    health["recovery"], "recovering",
                    "HTTP must start during replay: {health}"
                );
                assert!(!inbox.pending().unwrap().is_empty());
                let rows = get(addr, "/api/sessions", &cookie).unwrap();
                assert_eq!(rows["recovery"], "recovering");
                assert_eq!(rows["sessions"], serde_json::json!([]));
                // Peer config failure must already be observable while local recovery runs.
                let deadline = Instant::now() + isolate::PROC;
                loop {
                    let h = get(addr, "/api/health", &cookie).unwrap();
                    if h["peers"]["probe"]["last_error"] == "misconfigured" {
                        break;
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(10));
                }
                println!(
                    "HTTP responded during backlog recovery after {:?}",
                    started.elapsed()
                );
                return;
            }
        }
        assert!(
            fx.child.as_mut().unwrap().try_wait().unwrap().is_none(),
            "daemon exited: {text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "HTTP did not start: {}",
        std::fs::read_to_string(log).unwrap()
    );
}
