//! tmux 3.2a 的"pane 死了但退出码还没收集"窗口（agora-5nu），不依赖本机 tmux 版本：
//! 用一个假 tmux 脚本回放 `list-panes` 输出——先给空 status，再给 7——断言窗口内不得报
//! `Signal("unknown")`。真 tmux 上这个窗口只有几毫秒，ubuntu-22.04 容器紧密轮询 30 次撞上 2 次
//! （2026-09-03），CI 上表现为 `quick_exit_keeps_exit_code` 偶红。

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::time::Duration;

use agora::runtime::tmux::{socket_path, TmuxConfig, TmuxRuntime};
use agora::runtime::{Exit, Runtime};

/// 与 `src/runtime/tmux/mod.rs` 的 `SEP` 一致。
const SEP: &str = "|#|";

fn pane_line(status: &str) -> String {
    [
        "ag-w", "123", "1", status, "", "", "0", "160", "48", "/tmp", "t",
    ]
    .join(SEP)
}

struct Fake {
    rt: TmuxRuntime,
    socket: String,
    _listener: UnixListener,
    _dir: tempfile::TempDir,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(socket_path(&self.socket));
    }
}

/// `empty_polls = Some(n)`：前 n 次 `list-panes` 返回空 status，之后返回 7；`None`：一直为空
/// （3.2a 上信号退出就是这样，没有 `pane_dead_signal` 可看）。
fn fake(tag: &str, empty_polls: Option<u32>, grace: Duration) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("count");
    let script = dir.path().join("tmux");
    let reply = match empty_polls {
        Some(n) => format!(
            "if [ \"$n\" -le {n} ]; then printf '%s\\n' '{}'; else printf '%s\\n' '{}'; fi",
            pane_line(""),
            pane_line("7")
        ),
        None => format!("printf '%s\\n' '{}'", pane_line("")),
    };
    let body = format!(
        "#!/bin/sh\n# 假 tmux 3.2a（tests/tmux_dead_window.rs）\ncase \"$*\" in\n  \
         *list-panes*) n=$(cat '{c}' 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > '{c}'; {reply} ;;\n  \
         *) echo 'tmux 3.2a' ;;\nesac\n",
        c = counter.display()
    );
    write_fake(dir, script, body, grace, tag)
}

/// 假 tmux（版本自选，都 ≥ 3.3，所以不会走 unknown-signal 兜底）：**只有**在收到过一次
/// `run-shell` 之后才肯给出 status——libutempter 吃掉 SIGCHLD 后 zombie 一直没人收，
/// 直到 server 再收到一次 SIGCHLD 才补上（agora-tc4）。
fn fake_needs_nudge(tag: &str, version: &str) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("tmux");
    let marker = dir.path().join("nudged");
    let body = format!(
        "#!/bin/sh\n# 假 tmux 3.4 + libutempter（tests/tmux_dead_window.rs）\ncase \"$*\" in\n  \
         *run-shell*) : > '{m}' ;;\n  \
         *list-panes*) if [ -e '{m}' ]; then printf '%s\\n' '{yes}'; else printf '%s\\n' '{no}'; fi ;;\n  \
         *) echo 'tmux {version}' ;;\nesac\n",
        m = marker.display(),
        yes = pane_line("3"),
        no = pane_line("")
    );
    write_fake(dir, script, body, Duration::from_secs(60), tag)
}

fn write_fake(
    dir: tempfile::TempDir,
    script: std::path::PathBuf,
    body: String,
    grace: Duration,
    tag: &str,
) -> Fake {
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // `server_running` 靠能不能 connect 上 socket 判断：这里自己 bind 一个。
    let socket = format!("agora-fake-{}-{tag}", std::process::id());
    let path = socket_path(&socket);
    let dir_of_sockets = path.parent().unwrap();
    if !dir_of_sockets.exists() {
        // 真 tmux 要求这个目录是 0700，否则拒绝启动；别给用户留一个权限不对的目录。
        std::fs::create_dir_all(dir_of_sockets).unwrap();
        std::fs::set_permissions(dir_of_sockets, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();

    let rt = TmuxRuntime::new(TmuxConfig {
        bin: script.to_string_lossy().into_owned(),
        socket: socket.clone(),
        adopt_sockets: vec![],
        conf_path: dir.path().join("tmux.conf"),
        unknown_signal_grace: grace,
        record: true,
        ..Default::default()
    })
    .unwrap();
    Fake {
        rt,
        socket,
        _listener: listener,
        _dir: dir,
    }
}

#[test]
fn empty_status_window_keeps_exit_none_until_collected() {
    let f = fake("window", Some(2), Duration::from_secs(60));
    assert_eq!(f.rt.version().unwrap(), (3, 2));
    let r = f.rt.make_ref(&f.socket, "ag-w");

    let s1 = f.rt.inspect(&r).unwrap();
    assert!(!s1.alive);
    assert_eq!(
        s1.exit, None,
        "退出码尚未收集的窗口内不得报 Signal(unknown)"
    );
    assert!(
        s1.exited_at.is_some(),
        "3.2a 没有 pane_dead_time，用首次观测替代"
    );

    let s2 = f.rt.inspect(&r).unwrap();
    assert_eq!(s2.exit, None);
    assert_eq!(s2.exited_at, s1.exited_at, "首次观测时刻要跨 tick 稳定");

    let s3 = f.rt.inspect(&r).unwrap();
    assert_eq!(s3.exit, Some(Exit::Code(7)));
    assert_eq!(s3.exited_at, s1.exited_at);
}

#[test]
fn empty_status_past_grace_is_reported_as_unknown_signal() {
    let f = fake("signal", None, Duration::from_millis(200));
    let r = f.rt.make_ref(&f.socket, "ag-w");
    assert_eq!(f.rt.inspect(&r).unwrap().exit, None);
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        f.rt.inspect(&r).unwrap().exit,
        Some(Exit::Signal("unknown".into())),
        "宽限过后仍无 status：3.2a 上只能当信号退出"
    );
}

/// 守卫（agora-tc4）：tmux < 3.6 且 libutempter 吃掉 SIGCHLD 时，pane 死了但退出状态永远不来，
/// 会话卡在 UNKNOWN。agora 看到"已死却没有退出信息"必须让 server 自己再产生一次 SIGCHLD
/// （`run-shell -b true`），zombie 才会被收掉、status 才会出现。
/// 关掉 `nudge_sigchld` 这条路，这里就一直是 None → 红。
#[test]
fn dead_without_status_is_recovered_by_a_sigchld_nudge() {
    let f = fake_needs_nudge("nudge", "3.4");
    assert_eq!(f.rt.version().unwrap(), (3, 4));
    let r = f.rt.make_ref(&f.socket, "ag-w");

    let s1 = f.rt.inspect(&r).unwrap();
    assert!(!s1.alive);
    assert_eq!(s1.exit, None, "第一次看见时状态还没补上");

    // nudge 是每 socket 200 ms 一次；等过这个间隔再看，退出码必须已经到手。
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        f.rt.inspect(&r).unwrap().exit,
        Some(Exit::Code(3)),
        "补发 SIGCHLD 之后 tmux 才收得到退出码"
    );
    assert!(
        f.rt.take_recorded()
            .iter()
            .any(|argv| argv.iter().any(|a| a == "run-shell")),
        "补救走的是 run-shell"
    );
}

/// 反向守卫：3.6+ 上游已自己补 SIGCHLD（fa5f3cef3），agora 不该再每 tick 多 fork 一个进程。
#[test]
fn modern_tmux_is_not_nudged() {
    // 同一个假 tmux，只是自称 3.7：没人 run-shell，它就一直不给 status。
    let f = fake_needs_nudge("modern", "3.7");
    let r = f.rt.make_ref(&f.socket, "ag-w");
    for _ in 0..3 {
        assert_eq!(f.rt.inspect(&r).unwrap().exit, None);
        std::thread::sleep(Duration::from_millis(120));
    }
    assert!(
        !f.rt
            .take_recorded()
            .iter()
            .any(|argv| argv.iter().any(|a| a == "run-shell")),
        "3.6+ 不该补发"
    );
}
