//! agora-quy：fixture 销毁必须连同 tmux 的 socket 文件一起清理。

mod common;

use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agora::runtime::tmux::socket_path;
use common::node::TmuxNode;

#[tokio::test]
async fn node_drop_removes_live_and_stale_sockets_after_crash() {
    for stale in [false, true] {
        let mut node = TmuxNode::new();
        node.serve().await;
        let session = node.create_fake("cleanup", "sleep 300000");
        let path = socket_path(&node.socket);
        assert!(UnixStream::connect(&path).is_ok());
        node.crash();
        assert!(node.pane_alive(&session), "crash must preserve the agent");
        if stale {
            assert!(Command::new("tmux")
                .args(["-L", &node.socket, "kill-server"])
                .status()
                .unwrap()
                .success());
            // 确定性重现 server 已退出但文件残留，不依赖某个 tmux 版本是否 unlink。
            let _ = std::fs::remove_file(&path);
            drop(UnixListener::bind(&path).unwrap());
            common::isolate::wait_socket_refuses(&path);
        }
        assert!(path.exists());
        let home = node.home.clone();
        drop(node);
        assert!(!path.exists(), "node left socket {path:?} (stale={stale})");
        assert!(!home.exists());
    }
    drop(TmuxNode::new());
}

/// agora-tzje：被 SIGKILL 的测试进程留下的 `/tmp/ag-up-*` 由下一次启动补收——
/// 目录、tmux server、daemon 一个都不留。模拟方式就是造出"Drop 没跑"的现场：
/// `owner.pid` 写一个已经死掉的进程（它是不再存在的测试进程），`agora.pid` 写一个
/// argv 带着这份 home 的假 daemon。
#[test]
fn stale_upgrade_home_from_a_killed_process_is_swept() {
    use common::isolate;

    let n = isolate::nth();
    // tag 也由 isolate 的规则生成（home 与 socket 同一处构造）：这里的 tag 模仿"已死进程"
    // 的名字形状，但 sweep 真正的判据是目录里的 owner.pid，不是名字里的 pid。
    let (home, socket) = isolate::home_and_socket("up", &format!("sweep{n}"), n);
    std::fs::create_dir_all(home.join("bin")).unwrap();

    // 已死的 "测试进程"：spawn 一个 true、收尸，pid 短期内不会被复用。
    let mut dead = Command::new("true").spawn().unwrap();
    let owner = dead.id();
    dead.wait().unwrap();
    std::fs::write(home.join("owner.pid"), owner.to_string()).unwrap();

    // 遗留 tmux server：socket 与 home 同 tag（`home_and_socket` 的两半）。
    let sock_path = socket_path(&socket);
    assert!(Command::new("tmux")
        .args([
            "-L",
            &socket,
            "new-session",
            "-d",
            "-s",
            "leftover",
            "sleep",
            "300"
        ])
        .status()
        .unwrap()
        .success());
    assert!(sock_path.exists());

    // 遗留 daemon：sleep 的副本，argv[0] 就是 `<home>/bin/agora`（sweep 的 ps 检查认这个）。
    let fake_daemon = home.join("bin").join("agora");
    std::fs::copy("/bin/sleep", &fake_daemon).unwrap();
    isolate::mark_executable(&fake_daemon, &["0"]);
    let mut child = Command::new(&fake_daemon)
        .arg("300")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::fs::write(home.join("agora.pid"), child.id().to_string()).unwrap();

    isolate::sweep_stale_homes("up");

    assert!(!home.exists(), "遗留的 AGORA_HOME 没被收掉：{home:?}");
    assert!(!sock_path.exists(), "遗留 tmux server 的 socket 没被收掉");
    let deadline = Instant::now() + isolate::PROC;
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "sweep 没杀掉遗留 daemon");
        std::thread::sleep(Duration::from_millis(20));
    }
}
