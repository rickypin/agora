//! A11 / A12 / A20 / A21 的后端语义，真实 tmux 隔离 socket 上的 Session Manager。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::runtime::tmux::{TmuxConfig, TmuxRuntime};
use agora::runtime::{Runtime, Size};
use agora::session::{Db, NewSession, SessionError, SessionManager};
use agora::status::Status;

static N: AtomicU32 = AtomicU32::new(0);

struct Fixture {
    socket: String,
    db: Arc<Db>,
    rt: Arc<TmuxRuntime>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let socket = format!("agora-sm-{}-{n}", std::process::id());
        let dir = tempfile::tempdir().unwrap();
        let rt = Arc::new(
            TmuxRuntime::new(TmuxConfig {
                socket: socket.clone(),
                adopt_sockets: vec![],
                conf_path: dir.path().join("tmux.conf"),
                ..Default::default()
            })
            .unwrap(),
        );
        let db = Arc::new(Db::open(&dir.path().join("agora.db")).unwrap());
        Fixture {
            socket,
            db,
            rt,
            _dir: dir,
        }
    }

    fn manager(&self) -> SessionManager {
        SessionManager::new(self.db.clone(), self.rt.clone() as Arc<dyn Runtime>)
    }

    fn wait(
        &self,
        m: &SessionManager,
        id: &str,
        pred: impl Fn(&agora::session::SessionView) -> bool,
    ) -> agora::session::SessionView {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let v = m.get(id).unwrap();
            if pred(&v) {
                return v;
            }
            assert!(Instant::now() < deadline, "timeout: {v:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn spec(name: &str, command: &str) -> NewSession {
    NewSession {
        display_name: name.into(),
        agent_type: "shell".into(),
        working_directory: std::env::temp_dir(),
        worktree: None,
        task_ref: None,
        command: command.into(),
        env: vec![],
        size: Size::default(),
    }
}

#[test]
fn daemon_restart_rediscovers_sessions_with_exit_codes() {
    // A11 A12：kill -9 daemon 再起 → 列表重新发现全部会话；exit 7 的显示 FAILED 且退出码可读。
    let f = Fixture::new();
    let m = f.manager();
    let live = m.create(&spec("live", "sleep 300")).unwrap();
    let failed = m.create(&spec("failed", "exit 7")).unwrap();
    f.wait(&m, &failed.record.id, |v| v.exit.is_some());
    drop(m);

    let m2 = f.manager();
    let report = m2.reconcile().unwrap();
    assert_eq!(report.known_alive, vec![live.record.id.clone()]);
    assert_eq!(report.known_dead, vec![failed.record.id.clone()]);
    let v = m2.get(&failed.record.id).unwrap();
    assert_eq!(v.assessment.status, Status::Failed);
    assert_eq!(v.exit, Some(agora::runtime::Exit::Code(7)));
    assert!(v.record.ended_at.is_some());
    let lv = f.wait(&m2, &live.record.id, |v| v.alive);
    assert!(matches!(
        lv.assessment.status,
        Status::Starting | Status::Running
    ));
}

#[test]
fn kill_keeps_scrollback_and_restart_reuses_session() {
    // A21：Kill 后行仍在、输出可看；Restart 同一会话内重生，前一轮输出仍在。
    let f = Fixture::new();
    let m = f.manager();
    let s = m
        .create(&spec("k", "echo ROUND_ONE EPOCH=$AGORA_EPOCH; sleep 300"))
        .unwrap();
    let id = s.record.id.clone();
    f.wait(&m, &id, |v| v.alive);
    let killed = m.kill(&id).unwrap();
    assert!(!killed.alive);
    let r = agora::runtime::RuntimeRef(killed.record.runtime_ref.clone().unwrap());
    let tail = String::from_utf8_lossy(&f.rt.capture_tail(&r, 50).unwrap()).into_owned();
    assert!(tail.contains("ROUND_ONE"), "{tail}");

    let restarted = m.restart(&id, &[]).unwrap();
    assert_eq!(restarted.record.epoch, 2);
    f.wait(&m, &id, |v| v.alive);
    // AGORA_EPOCH 经 LaunchSpec.env 交给了 agent。respawn-pane -e 只作用于新进程、
    // 不改会话环境，所以看进程自己打印的值而不是 show-environment（实测 2026-09-03）。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let tail = String::from_utf8_lossy(&f.rt.capture_tail(&r, 50).unwrap()).into_owned();
        if tail.contains("ROUND_ONE EPOCH=1") && tail.contains("ROUND_ONE EPOCH=2") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Restart 后前一轮输出应仍在且新一轮 epoch=2: {tail}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn delete_metadata_does_not_kill_and_cleanup_needs_dead() {
    // A20：Delete ≠ kill；清理只对已退出会话。
    let f = Fixture::new();
    let m = f.manager();
    let s = m.create(&spec("d", "sleep 300")).unwrap();
    let id = s.record.id.clone();
    f.wait(&m, &id, |v| v.alive);
    assert!(matches!(m.cleanup(&id), Err(SessionError::StillAlive(_))));

    let r = agora::runtime::RuntimeRef(s.record.runtime_ref.clone().unwrap());
    m.delete_metadata(&id).unwrap();
    let still = f.rt.inspect(&r).unwrap();
    assert!(still.alive, "进程仍活，会话留在 socket 上成未注册");
    let report = m.reconcile().unwrap();
    assert_eq!(report.unregistered_managed, vec![r.clone()]);
}

#[test]
fn working_directory_and_command_are_portable() {
    let f = Fixture::new();
    let m = f.manager();
    let mut n = spec("cwd", "pwd; sleep 300");
    n.working_directory = PathBuf::from("/");
    let s = m.create(&n).unwrap();
    assert_eq!(s.record.working_directory.as_deref(), Some("/"));
    assert_eq!(s.record.command.as_deref(), Some("pwd; sleep 300"));
}
