//! 真实 tmux、隔离 socket 上的运行时守卫（ADR-001 D2 / D3 / D6）。
//! 每个测试自己一个 socket `agora-test-<pid>-<n>`，结束时直接杀掉那个 server。

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use agora::runtime::tmux::{socket_path, TmuxConfig, TmuxRuntime};
use agora::runtime::{Exit, LaunchSpec, Runtime, RuntimeError, Size};

// 轮询 tmux 不能太密：每次 inspect/capture 都要新起一个 tmux client 进程，密集轮询会和
// server 收集子进程退出状态抢 SIGCHLD，pane 会死得"没有退出码"（agora-tc4；实测 2026-09-03
// 3.2a 上密集轮询丢 5/6、200 ms 轮询丢 1/6）。生产的 status.detector_interval 是 2 s。
const POLL: Duration = Duration::from_millis(200);

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
            std::thread::sleep(POLL);
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
    // agora-6bo：前一轮的输出——包括还在可见屏上、没滚进 history 的那些——Restart 后仍在。
    // 48 行的窗口里 32 行全在可见屏；respawn-pane 的 screen_reinit 会清掉它们，
    // 靠 respawn 前后的缩放窗口把它们先推进 history 再拉回来（TmuxRuntime::respawn）。
    let f = Fixture::new();
    let r =
        f.rt.create(&f.spec(
            "ag-resp",
            "echo FIRST_ROUND; seq 1 30; echo LAST_VISIBLE; exit 0",
        ))
        .unwrap();
    f.wait_until(&r, |s| !s.alive);
    // 死了立刻 respawn：3.2a 上此时退出码可能还没收集、"Pane is dead" 行还没打印，最容易丢
    // （ubuntu-22.04 实测 2026-09-03）。
    f.rt.respawn(&r, &f.spec("ag-resp", "echo SECOND_ROUND; sleep 300"))
        .unwrap();
    f.wait_until(&r, |s| s.alive);
    wait_capture(&f, &r, &["FIRST_ROUND", "LAST_VISIBLE", "SECOND_ROUND"]);

    // 活着时 Restart：先杀再重生，前两轮同样都在。
    f.rt.respawn(&r, &f.spec("ag-resp", "echo THIRD_ROUND; sleep 300"))
        .unwrap();
    f.wait_until(&r, |s| s.alive);
    wait_capture(
        &f,
        &r,
        &["FIRST_ROUND", "LAST_VISIBLE", "SECOND_ROUND", "THIRD_ROUND"],
    );
}

fn wait_capture(f: &Fixture, r: &agora::runtime::RuntimeRef, needles: &[&str]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = String::from_utf8_lossy(&f.rt.capture_tail(r, 200).unwrap()).into_owned();
        if needles.iter().all(|n| text.contains(n)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "期待 {needles:?} 都在，capture = {text:?}"
        );
        std::thread::sleep(POLL);
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
    // 进程组里没有**活着**的成员了。这里不能用 kill(-pgid, 0)：僵尸也还在进程组里，
    // 信号 0 对它同样返回 0，于是"孤儿已经被杀干净、只是还没被 init 收尸"会被误判成失败
    // （CI ubuntu-22.04 实测 2026-09-03 run 33734626320；本机容器约 1/6 复现，agora-ty7）。
    // 后台子进程的父进程（pane 里的 shell）先死，它要等重新挂到 init 名下才被回收，
    // 这段时间长短不归我们管。改成问 ps 要状态，只把非 Z 的成员算数。
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let alive = live_group_members(pid);
        if alive.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "process group {pid} still has live members: {alive:?}"
        );
        std::thread::sleep(POLL);
    }
}

/// 进程组里非僵尸的成员（`pid stat`）。macOS 与 Linux 的 ps 都认这三个 -o 字段。
fn live_group_members(pgid: u32) -> Vec<String> {
    let out = Command::new("ps")
        .args(["-e", "-o", "pid=,pgid=,stat="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let pid = f.next()?;
            let g = f.next()?.parse::<u32>().ok()?;
            let stat = f.next()?;
            (g == pgid && !stat.starts_with('Z')).then(|| format!("{pid} {stat}"))
        })
        .collect()
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
