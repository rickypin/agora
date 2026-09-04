//! `agora hook` 命令的行为（ADR-002 D3/D4/D5；agora-dvh.3）：真实子进程 + 进程内 daemon 侧。
//!
//! 每个测试自己的 AGORA_HOME（短路径：macOS unix socket 路径上限 104 字节）；daemon 侧
//! 跑在一个独立的 tokio runtime 上，"daemon 崩了"就是把那个 runtime 整个 drop 掉——
//! 监听器与每个连接的任务一起消失，和真进程被 kill 一样。

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::adapter::hooks::Decision;
use agora::hook::{Receiver, MAX_HOLDS_PER_SESSION};
use agora::local::{self, Request, SOCKET_FILE};
use agora::session::{Db, SessionManager};

use common::FakeRuntime;

const AGORA_BIN: &str = env!("CARGO_BIN_EXE_agora");
static N: AtomicU32 = AtomicU32::new(0);

fn home() -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let home = PathBuf::from(format!("/tmp/agh-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    local::ensure_home(&home).unwrap();
    home
}

struct Hook {
    child: Child,
}

impl Hook {
    /// 起 `agora hook --host <host> --home <home>`，payload 进 stdin。`env` 追加；`GROK_SESSION_ID`
    /// 先清掉，免得开发机上恰好有。
    fn spawn(home: &Path, host: &str, payload: &str, env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(AGORA_BIN);
        cmd.args([
            "hook",
            "--host",
            host,
            "--home",
            &home.display().to_string(),
        ])
        .env_remove("GROK_SESSION_ID")
        .env_remove("AGORA_SESSION_ID")
        .env_remove("AGORA_EPOCH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        Hook { child }
    }

    fn wait(self) -> (i32, String, String) {
        let out = self.child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn wait_within(self, limit: Duration) -> (i32, String, String) {
        let start = Instant::now();
        let mut child = self.child;
        loop {
            if child.try_wait().unwrap().is_some() {
                let out = child.wait_with_output().unwrap();
                return (
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                );
            }
            assert!(start.elapsed() < limit, "hook 进程 {limit:?} 内没有退出");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

fn inbox_files(home: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    fn walk(d: &Path, v: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, v);
                } else {
                    v.push(p);
                }
            }
        }
    }
    walk(&home.join("hooks"), &mut v);
    v
}

/// 进程内的 daemon 侧：Receiver + socket，跑在自己的 runtime 上。
struct Daemon {
    rt: Option<tokio::runtime::Runtime>,
    receiver: Arc<Receiver>,
}

impl Daemon {
    fn start(home: &Path) -> Self {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let sessions = Arc::new(SessionManager::new(db, Arc::new(FakeRuntime::default())));
        let receiver = Arc::new(Receiver::new(home, sessions));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let socket = home.join(SOCKET_FILE);
        let r = receiver.clone();
        let handler: local::Handler = Arc::new(move |req| {
            let r = r.clone();
            Box::pin(async move {
                match req {
                    Request::Hook { path } => r.wake(Path::new(&path)).await,
                    _ => local::Response::Pong,
                }
            })
        });
        let sock = socket.clone();
        rt.spawn(async move {
            let _ = local::serve(&sock, handler).await;
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "socket 没起来");
            std::thread::sleep(Duration::from_millis(10));
        }
        Daemon {
            rt: Some(rt),
            receiver,
        }
    }

    fn wait_holds(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.receiver.hold_count() != n {
            assert!(
                Instant::now() < deadline,
                "挂起数 {} ≠ {n}",
                self.receiver.hold_count()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// daemon 崩：runtime 连同监听器、所有连接任务一起没了。
    fn crash(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.crash();
    }
}

fn permission_request(tool_use_id: &str) -> String {
    format!(
        r#"{{"hook_event_name":"PermissionRequest","session_id":"agent-1","tool_use_id":"{tool_use_id}","tool_name":"Bash"}}"#
    )
}

#[test]
fn host_mismatch_exits_silently() {
    // ADR-002 D4：Grok 兼容加载会把装给 Claude 的条目以 --host claude 跑一遍。
    let home = home();
    let (code, out, err) = Hook::spawn(
        &home,
        "claude",
        r#"{"hook_event_name":"Stop","session_id":"x"}"#,
        &[("GROK_SESSION_ID", "grok-1")],
    )
    .wait();
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    assert!(
        inbox_files(&home).is_empty(),
        "不该落盘: {:?}",
        inbox_files(&home)
    );
    // 反之亦然：--host grok 而环境里没有 GROK_SESSION_ID。
    let (code, out, _) = Hook::spawn(&home, "grok", r#"{"hookEventName":"Stop"}"#, &[]).wait();
    assert_eq!((code, out.as_str()), (0, ""));
    assert!(inbox_files(&home).is_empty());
    // 用法错误才出声：那是安装写错了。
    let (code, _, err) = Hook::spawn(&home, "nope", "{}", &[]).wait();
    assert_eq!(code, 2);
    assert!(err.contains("nope"), "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn without_daemon_the_event_is_kept_on_disk() {
    // ADR-002 D3 ②：socket 不在 → 落盘后 exit 0；信封带上身份与 tmux 线索。
    let home = home();
    let (code, out, err) = Hook::spawn(
        &home,
        "claude",
        r#"{"hook_event_name":"Stop","session_id":"agent-9","last_assistant_message":"done"}"#,
        &[
            ("AGORA_SESSION_ID", "s-1"),
            ("AGORA_EPOCH", "3"),
            ("TMUX_PANE", "%7"),
            ("CLAUDE_PID", "4242"),
        ],
    )
    .wait();
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
    let files = inbox_files(&home);
    assert_eq!(files.len(), 1, "{files:?}");
    let path = &files[0];
    assert!(
        path.starts_with(home.join("hooks/inbox/claude/agent-9")),
        "{}",
        path.display()
    );
    assert!(path.extension().is_some_and(|e| e == "json"));
    let d = agora::hook::Inbox::new(&home).read(path).unwrap();
    assert_eq!(d.envelope.agora_session_id.as_deref(), Some("s-1"));
    assert_eq!(d.envelope.agora_epoch, Some(3));
    assert_eq!(
        d.envelope.runtime_env.get("TMUX_PANE").map(String::as_str),
        Some("%7")
    );
    assert_eq!(
        d.envelope.agent_env.get("CLAUDE_PID").map(String::as_str),
        Some("4242")
    );
    assert_eq!(d.payload["last_assistant_message"], "done");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn hold_releases_on_daemon_death() {
    // ADR-002 "什么会让它变危险"：daemon 崩了 hook 不能还在等——1 s 内退出、不输出。
    let home = home();
    let mut daemon = Daemon::start(&home);
    let mut hook = Hook::spawn(
        &home,
        "claude",
        &permission_request("t-1"),
        &[("AGORA_SESSION_ID", "s-1")],
    );
    daemon.wait_holds(1);
    assert!(hook.running(), "挂起中的 hook 不该退出");
    daemon.crash();
    let (code, out, _) = hook.wait_within(Duration::from_secs(1));
    assert_eq!((code, out.as_str()), (0, ""));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn hold_cap() {
    // ADR-002 D5：每会话 8 个挂起，第 9 个立即 exit 0 放行给终端；Dashboard 的 allow 只到它答的那一个。
    let home = home();
    let daemon = Daemon::start(&home);
    let env = [("AGORA_SESSION_ID", "s-cap")];
    let mut held: Vec<Hook> = (0..MAX_HOLDS_PER_SESSION)
        .map(|i| {
            Hook::spawn(
                &home,
                "claude",
                &permission_request(&format!("t-{i}")),
                &env,
            )
        })
        .collect();
    daemon.wait_holds(MAX_HOLDS_PER_SESSION);
    let (code, out, _) = Hook::spawn(&home, "claude", &permission_request("t-overflow"), &env)
        .wait_within(Duration::from_secs(5));
    assert_eq!((code, out.as_str()), (0, ""));
    assert_eq!(daemon.receiver.hold_count(), MAX_HOLDS_PER_SESSION);
    assert!(held.iter_mut().all(Hook::running));

    // 另一个会话不受这个会话的上限影响（节点上限 256 远没到）。
    let other = Hook::spawn(
        &home,
        "claude",
        &permission_request("t-x"),
        &[("AGORA_SESSION_ID", "s-other")],
    );
    daemon.wait_holds(MAX_HOLDS_PER_SESSION + 1);

    // Dashboard 答 allow：只有 t-3 那个 hook 拿到决定并写回文档形态。
    daemon
        .receiver
        .respond("s-cap", Some("t-3"), Decision::Allow)
        .unwrap();
    let (code, out, _) = held.remove(3).wait_within(Duration::from_secs(5));
    assert_eq!(code, 0);
    let v: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("{out:?}"));
    assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
    assert_eq!(
        daemon
            .receiver
            .respond("s-cap", Some("t-3"), Decision::Allow),
        Err(agora::hook::RespondError::NoPendingDecision {
            session: "s-cap".into(),
            tool_use_id: "t-3".into()
        })
    );

    // 终端里答了 t-0：同 tool_use_id 的 PostToolUse 到达 → 那个挂起解除，hook 静默退出。
    let (code, out, _) = Hook::spawn(
        &home,
        "claude",
        r#"{"hook_event_name":"PostToolUse","session_id":"agent-1","tool_use_id":"t-0"}"#,
        &env,
    )
    .wait_within(Duration::from_secs(5));
    assert_eq!((code, out.as_str()), (0, ""));
    let (code, out, _) = held.remove(0).wait_within(Duration::from_secs(5));
    assert_eq!((code, out.as_str()), (0, ""));

    // Stop 到达：这个会话剩下的全部解除；另一个会话的还挂着。
    Hook::spawn(
        &home,
        "claude",
        r#"{"hook_event_name":"Stop","session_id":"agent-1"}"#,
        &env,
    )
    .wait_within(Duration::from_secs(5));
    for h in held {
        let (code, out, _) = h.wait_within(Duration::from_secs(5));
        assert_eq!((code, out.as_str()), (0, ""));
    }
    daemon.wait_holds(1);
    assert_eq!(daemon.receiver.pending("s-other"), vec!["t-x".to_string()]);
    daemon.receiver.resolve_session("s-other", "exit");
    let (code, out, _) = other.wait_within(Duration::from_secs(5));
    assert_eq!((code, out.as_str()), (0, ""));
    let _ = std::fs::remove_dir_all(&home);
}
