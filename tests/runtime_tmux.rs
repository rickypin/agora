//! 真实 tmux、隔离 socket 上的运行时守卫（ADR-001 D2 / D3 / D6）。
//! 每个测试自己一个 socket `agora-test-<pid>-<n>`，结束时直接杀掉那个 server。

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use agora::runtime::tmux::{socket_path, TmuxConfig, TmuxRuntime};
use agora::runtime::{Exit, LaunchSpec, Runtime, RuntimeError, Size};

static N: AtomicU32 = AtomicU32::new(0);

struct Fixture {
    rt: TmuxRuntime,
    socket: String,
    foreign: String,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let socket = format!("agora-test-{pid}-{n}");
        let foreign = format!("agora-test-{pid}-{n}-foreign");
        let dir = tempfile::tempdir().unwrap();
        let rt = TmuxRuntime::new(TmuxConfig {
            socket: socket.clone(),
            adopt_sockets: vec![foreign.clone()],
            conf_path: dir.path().join("tmux.conf"),
            history_limit: 12345,
            record: true,
            ..Default::default()
        })
        .unwrap();
        rt.check_version().unwrap();
        rt.take_recorded();
        Fixture {
            rt,
            socket,
            foreign,
            _dir: dir,
        }
    }

    fn spec(&self, name: &str, command: &str) -> LaunchSpec {
        LaunchSpec {
            name: name.into(),
            command: command.into(),
            cwd: std::env::temp_dir(),
            env: vec![("AGORA_TEST".into(), "1".into())],
            size: Size::default(),
        }
    }

    /// 测试侧直接用 tmux：在"用户的" socket 上起一个会话，agora 只能读它。
    fn foreign_session(&self, name: &str) {
        let st = Command::new("tmux")
            .args([
                "-L",
                &self.foreign,
                "new-session",
                "-d",
                "-s",
                name,
                "sleep 300",
            ])
            .status()
            .unwrap();
        assert!(st.success());
    }

    fn wait_until(
        &self,
        r: &agora::runtime::RuntimeRef,
        pred: impl Fn(&agora::runtime::RuntimeSession) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(s) = self.rt.inspect(r) {
                if pred(&s) {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "timeout waiting on {r}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for s in [&self.socket, &self.foreign] {
            let _ = Command::new("tmux")
                .args(["-L", s, "kill-server"])
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[test]
fn quick_exit_keeps_exit_code() {
    // A11/A12 的发现层：秒退 exit 7 的会话保住退出码（一次调用含 remain-on-exit）。
    let f = Fixture::new();
    let r = f.rt.create(&f.spec("ag-quick", "exit 7")).unwrap();
    // pane_dead 先于退出码被收集（3.4 实测），所以等的是 exit 而不是 alive。
    f.wait_until(&r, |s| s.exit.is_some());
    let s = f.rt.inspect(&r).unwrap();
    assert_eq!(s.exit, Some(Exit::Code(7)));
    assert!(s.managed);
    assert!(s.exited_at.is_some());
}

#[test]
fn history_limit_applies_to_first_pane() {
    // ADR-001 D3：history-limit 必须在 server 级 conf 里给，否则 3.7 前首 pane 拿默认 2000。
    let f = Fixture::new();
    f.rt.create(&f.spec("ag-hist", "sleep 300")).unwrap();
    let out = Command::new("tmux")
        .args([
            "-L",
            &f.socket,
            "display-message",
            "-p",
            "-t",
            "=ag-hist:",
            "#{history_limit}",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "12345");
}

#[test]
fn respawn_keeps_previous_scrollback() {
    let f = Fixture::new();
    let r =
        f.rt.create(&f.spec("ag-resp", "echo FIRST_ROUND; exit 0"))
            .unwrap();
    f.wait_until(&r, |s| !s.alive);
    f.rt.respawn(&r, &f.spec("ag-resp", "echo SECOND_ROUND; sleep 300"))
        .unwrap();
    f.wait_until(&r, |s| s.alive);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = String::from_utf8_lossy(&f.rt.capture_tail(&r, 200).unwrap()).into_owned();
        if text.contains("FIRST_ROUND") && text.contains("SECOND_ROUND") {
            break;
        }
        assert!(Instant::now() < deadline, "capture = {text:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn terminate_leaves_no_orphans() {
    // kill(-pgid)：子孙一起死，不经 tmux 的 SIGHUP。
    let f = Fixture::new();
    let r =
        f.rt.create(&f.spec("ag-term", "sh -c 'sleep 300' & sleep 300"))
            .unwrap();
    f.wait_until(&r, |s| s.alive);
    let pid = f.rt.inspect(&r).unwrap().pid.unwrap();
    f.rt.terminate(&r, Duration::from_secs(2)).unwrap();
    f.wait_until(&r, |s| s.exit.is_some());
    let s = f.rt.inspect(&r).unwrap();
    assert!(!s.alive);
    assert!(matches!(s.exit, Some(Exit::Signal(_))), "{:?}", s.exit);
    // 进程组里没人了：kill(-pgid, 0) 报 ESRCH。
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // SAFETY: 信号 0 只做存在性检查。
        let rc = unsafe { libc::kill(-(pid as i32), 0) };
        if rc == -1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "process group {pid} still has members"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn remove_refuses_alive_pane() {
    let f = Fixture::new();
    let r = f.rt.create(&f.spec("ag-rm", "sleep 300")).unwrap();
    f.wait_until(&r, |s| s.alive);
    assert!(matches!(f.rt.remove(&r), Err(RuntimeError::StillAlive(_))));
    f.rt.terminate(&r, Duration::from_secs(2)).unwrap();
    f.rt.remove(&r).unwrap();
    assert!(matches!(f.rt.inspect(&r), Err(RuntimeError::NotFound(_))));
}

#[test]
fn foreign_socket_is_read_only() {
    // ADR-001 D3：默认 socket 只收到 list / attach / capture，写操作在进程起来前就被拒。
    let f = Fixture::new();
    f.foreign_session("mywork");
    let list = f.rt.list().unwrap();
    let s = list
        .iter()
        .find(|s| s.name == "mywork")
        .expect("adopted session visible");
    assert!(!s.managed);
    let r = s.r#ref.clone();
    f.rt.inspect(&r).unwrap();
    f.rt.attach(&r, Size::default()).unwrap();
    f.rt.capture_tail(&r, 10).unwrap();
    f.rt.take_recorded();

    assert!(matches!(
        f.rt.respawn(&r, &f.spec("mywork", "true")),
        Err(RuntimeError::ReadOnly(_))
    ));
    assert!(matches!(f.rt.remove(&r), Err(RuntimeError::ReadOnly(_))));
    assert!(matches!(
        f.rt.terminate(&r, Duration::ZERO),
        Err(RuntimeError::ReadOnly(_))
    ));
    assert!(
        f.rt.take_recorded().is_empty(),
        "write attempts must not spawn tmux"
    );

    // 全程发往 foreign socket 的命令只有这三种。
    let _ = f.rt.list();
    let _ = f.rt.capture_tail(&r, 5);
    let _ = f.rt.attach(&r, Size::default());
    for argv in f.rt.take_recorded() {
        let on_foreign = argv.windows(2).any(|w| w[0] == "-L" && w[1] == f.foreign);
        if on_foreign {
            let verb = argv
                .iter()
                .find(|a| a.ends_with("-panes") || a.ends_with("-session") || a.ends_with("-pane"))
                .cloned()
                .unwrap_or_default();
            assert!(
                ["list-panes", "attach-session", "capture-pane"].contains(&verb.as_str()),
                "foreign socket received {argv:?}"
            );
            assert!(!argv.iter().any(|a| a == "set-option"));
        }
    }
}

#[test]
fn one_list_call_per_socket_per_tick() {
    // ADR-001 D2 / D6：子进程数 O(socket) 不 O(会话)。
    let f = Fixture::new();
    for i in 0..3 {
        f.rt.create(&f.spec(&format!("ag-l{i}"), "sleep 300"))
            .unwrap();
    }
    f.foreign_session("fw");
    f.rt.take_recorded();
    let sessions = f.rt.list().unwrap();
    assert!(sessions.len() >= 4);
    let recorded = f.rt.take_recorded();
    let list_calls = recorded
        .iter()
        .filter(|a| a.iter().any(|x| x == "list-panes"))
        .count();
    assert_eq!(list_calls, 2, "{recorded:?}");
    assert_eq!(
        recorded.len(),
        2,
        "list() must spawn nothing but list-panes: {recorded:?}"
    );
}

#[test]
fn absent_server_lists_empty_without_error() {
    let f = Fixture::new();
    assert!(!socket_path(&f.socket).exists());
    assert!(f.rt.list().unwrap().is_empty());
    assert!(f.rt.take_recorded().is_empty(), "no server → no subprocess");
}
