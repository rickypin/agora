//! A11 / A12 / A20 / A21 的后端语义，真实 tmux 隔离 socket 上的 Session Manager。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::runtime::tmux::{TmuxConfig, TmuxRuntime};
use agora::runtime::{Runtime, Size};
use agora::session::manager::KILL_GRACE;
use agora::session::{Db, NewSession, SessionError, SessionManager};
use agora::status::Status;

// 轮询 tmux 不能太密：每次 inspect/capture 都要新起一个 tmux client 进程。生产的
// status.detector_interval 是 2 s。（曾以为密集轮询会和 server 抢 SIGCHLD 导致 pane"没有
// 退出码"——真因是 tmux < 3.6 链 libutempter 时 SIGCHLD 被吞，运行时现在会补发，见 agora-tc4。）
const POLL: Duration = Duration::from_millis(200);

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
            std::thread::sleep(POLL);
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
    // sleep 收到 TERM 就退，落在 kill 的 1 s 同步等待里；万一机器慢到超过它，kill 会先返回
    // alive 的行、后台补 KILL（agora-284），所以这里等而不是断言。
    m.kill(&id).unwrap();
    let killed = f.wait(&m, &id, |v| !v.alive);
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
        std::thread::sleep(POLL);
    }
}

#[test]
fn kill_returns_before_grace_when_process_ignores_term() {
    // agora-284：进程吃掉 TERM（交互式 shell 的样子）时，kill 不能同步等满 5 s 宽限——经 peer
    // 转发的 Kill 会撞上 5 s 转发超时。要求：1 s 出头就返回、行仍 alive 但 killed_at 已写；
    // 宽限满后后台 SIGKILL，行变成 FINISHED "killed by user"。
    let f = Fixture::new();
    let m = f.manager();
    let s = m
        .create(&spec("k", "trap '' TERM; exec sleep 300"))
        .unwrap();
    let id = s.record.id.clone();
    f.wait(&m, &id, |v| v.alive);
    let t0 = Instant::now();
    let after = m.kill(&id).unwrap();
    let took = t0.elapsed();
    assert!(
        took < KILL_GRACE,
        "kill 同步等了 {took:?}，应远短于宽限 {KILL_GRACE:?}"
    );
    assert!(after.alive, "TERM 被忽略，返回时进程应还活着: {after:?}");
    assert!(after.record.killed_at.is_some(), "{after:?}");
    // 等到"死了且退出码已收集"：tmux 先报 pane 死、退出码要等 SIGCHLD 才有，中间那一两个 tick
    // 是设计内的瞬时 UNKNOWN（"exit status not yet collected"）。只等 !alive 就断言 FINISHED 在
    // Linux CI 上会踩进这个窗口（2026-09-07 ubuntu-24.04 红过，macOS 撞不上）。
    let deadline = Instant::now() + KILL_GRACE + Duration::from_secs(5);
    let dead = loop {
        let v = m.get(&id).unwrap();
        if !v.alive && v.exit.is_some() {
            break v;
        }
        assert!(
            Instant::now() < deadline,
            "宽限满后没被 SIGKILL（或退出码一直没收集到）: {v:?}"
        );
        std::thread::sleep(POLL);
    };
    assert_eq!(dead.assessment.status, Status::Finished, "{dead:?}");
    assert!(
        dead.assessment
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("killed by user"),
        "{dead:?}"
    );
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

#[test]
fn default_pane_title_does_not_shadow_display_name() {
    // agora-gky（MISSION §4.5）：没改过名时 agent 自设的 title 赢，但 tmux 的 pane title
    // 缺省是**主机名**而不是空串——shell 和任何不发 OSC 2 的 agent 都停在那个值上。
    // 放过去侧栏就显示主机名，Session Settings 里还是用户填的名字，同一个会话两个名字。
    let f = Fixture::new();
    let m = f.manager();

    let quiet = m.create(&spec("剧本-shell", "sleep 300")).unwrap();
    let v = f.wait(&m, &quiet.record.id, |v| v.alive);
    assert_eq!(
        v.name, "剧本-shell",
        "不发 OSC 2 的会话必须显示 display_name，不是主机名"
    );
    assert!(!v.record.name_locked);

    // 主动设过标题的才算数（OSC 2 = ESC ] 2 ; <text> BEL）。
    let loud = m
        .create(&spec(
            "另一个",
            "printf '\\033]2;agent-set-title\\007'; sleep 300",
        ))
        .unwrap();
    let v = f.wait(&m, &loud.record.id, |v| v.name == "agent-set-title");
    assert!(!v.record.name_locked, "title 赢不等于落锁");
}
