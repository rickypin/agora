//! 单实例（agora-apr）：同一 AGORA_HOME 再起一个 daemon 不能伤到活着的那个。
//!
//! 2026-09-05 实测的伤法：第二个实例先跑完运行时 / 库 / reconcile、打印"daemon 就绪"，再死于
//! TCP 端口占用，退出路径无条件删掉 `agora.sock`——活实例的 HTTP 照常，`agora url` 与 hook
//! 唤醒却全部失联。修法是先绑两个监听器、失败就退，退出只删自己绑的文件（比对 inode）。
//!
//! 真二进制 + 隔离 AGORA_HOME（短路径：macOS unix socket 路径上限 104 字节）+ 隔离 tmux
//! socket（config.yaml 的 runtime.tmux.socket），免得 reconcile 摸到开发机上真的 agora server。

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use agora::local::{self, Request, Response, SOCKET_FILE};

const AGORA_BIN: &str = env!("CARGO_BIN_EXE_agora");

struct Home {
    path: PathBuf,
    tmux_socket: String,
    port: u16,
}

impl Home {
    fn new() -> Self {
        let pid = std::process::id();
        let path = PathBuf::from(format!("/tmp/agsi-{pid}"));
        let _ = std::fs::remove_dir_all(&path);
        local::ensure_home(&path).unwrap();
        let tmux_socket = format!("agora-si-{pid}");
        // 拿一个空闲端口再放掉：绑定与 daemon 启动之间有个小窗口，够用。
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        std::fs::write(
            path.join("config.yaml"),
            format!(
                "server:\n  listen: \"127.0.0.1:{port}\"\nruntime:\n  tmux:\n    socket: \"{tmux_socket}\"\n    adopt_sockets: []\n"
            ),
        )
        .unwrap();
        Home {
            path,
            tmux_socket,
            port,
        }
    }

    fn socket(&self) -> PathBuf {
        self.path.join(SOCKET_FILE)
    }

    fn spawn_serve(&self) -> Daemon {
        Daemon(Some(
            Command::new(AGORA_BIN)
                .arg("serve")
                .env("AGORA_HOME", &self.path)
                .env("AGORA_LOG", "info")
                .env_remove("AGORA_LOG_FORMAT")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }

    fn url(&self) -> std::process::Output {
        Command::new(AGORA_BIN)
            .arg("url")
            .env("AGORA_HOME", &self.path)
            .output()
            .unwrap()
    }
}

/// 起出去的 daemon：断言失败 panic 时也要收掉，否则留下一个孤儿 daemon 攥着端口与 /tmp 目录
/// （2026-09-05 关守卫变红那一跑就留了一个，pgrep 才发现）。
struct Daemon(Option<Child>);

impl Daemon {
    fn pid(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }

    /// 等它自己退出；超时就 kill 并返回 None。
    fn wait_exit(&mut self, limit: Duration) -> Option<ExitStatus> {
        let child = self.0.as_mut().unwrap();
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// 收尸并拿 (stdout, stderr)。
    fn output(mut self) -> (String, String) {
        let out = self.0.take().unwrap().wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.tmux_socket, "kill-server"])
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 等 daemon 在 socket 上答 Pong；启动含 PATH 探测（最长 5 s）与 tmux 版本探测。
async fn wait_pong(sock: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let reply =
            tokio::time::timeout(Duration::from_secs(2), local::request(sock, &Request::Ping))
                .await;
        if matches!(reply, Ok(Ok(Response::Pong))) {
            return;
        }
        assert!(Instant::now() < deadline, "daemon 没起来: {reply:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 第一行同时含所有 needle 的行在 `log` 里的位置。
fn line_pos(log: &str, needles: &[&str]) -> Option<usize> {
    let mut pos = 0;
    for line in log.split_inclusive('\n') {
        if needles.iter().all(|n| line.contains(n)) {
            return Some(pos);
        }
        pos += line.len();
    }
    None
}

#[tokio::test]
async fn second_instance_in_same_home_exits_without_touching_the_first() {
    let home = Home::new();
    let sock = home.socket();
    let mut a = home.spawn_serve();
    wait_pong(&sock).await;
    let ino_before = std::fs::metadata(&sock).unwrap().ino();
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", home.port)).is_ok(),
        "A 的 HTTP 应已在监听"
    );

    // 同一 AGORA_HOME、同一端口再起一个：该在碰运行时之前就死，且说人话。
    let mut b = home.spawn_serve();
    let status = b.wait_exit(Duration::from_secs(15));
    let (b_out, b_err) = b.output();
    let status =
        status.unwrap_or_else(|| panic!("B 15 s 内没退出\nstdout: {b_out}\nstderr: {b_err}"));
    assert!(!status.success(), "B 应以非 0 退出: {status}");
    assert!(
        b_err.contains("已有实例"),
        "B 的 stderr 应说明已有实例\nstderr: {b_err}\nstdout: {b_out}"
    );
    for forbidden in ["PATH 探测", "listening", "daemon 就绪"] {
        assert!(
            !b_out.contains(forbidden) && !b_err.contains(forbidden),
            "B 不该走到 {forbidden:?}\nstdout: {b_out}\nstderr: {b_err}"
        );
    }

    // A 毫发无损：socket 文件还是原来那个 inode，仍答 Pong，agora url 仍能铸链接。
    assert_eq!(
        std::fs::metadata(&sock).map(|m| m.ino()).ok(),
        Some(ino_before),
        "A 的 socket 文件被动过"
    );
    assert_eq!(
        local::request(&sock, &Request::Ping).await.unwrap(),
        Response::Pong
    );
    let url = home.url();
    let url_out = String::from_utf8_lossy(&url.stdout);
    assert!(
        url.status.success() && url_out.contains(&format!("http://127.0.0.1:{}/#pair=", home.port)),
        "agora url 应仍可用: status={} stdout={url_out} stderr={}",
        url.status,
        String::from_utf8_lossy(&url.stderr)
    );

    // A 自己正常退出（SIGTERM）时删掉的是自己的文件；日志里两个 listening 都在"daemon 就绪"之前。
    // SAFETY: kill 只发信号，pid 是我们自己起的子进程。
    unsafe { libc::kill(a.pid() as libc::pid_t, libc::SIGTERM) };
    let status = a.wait_exit(Duration::from_secs(15));
    let (a_out, a_err) = a.output();
    assert_eq!(
        status.map(|s| s.success()),
        Some(true),
        "A 应在 SIGTERM 后正常退出\nstdout: {a_out}\nstderr: {a_err}"
    );
    assert!(!sock.exists(), "A 退出后该删掉自己的 socket 文件");
    let log = if a_out.contains("daemon 就绪") {
        &a_out
    } else {
        &a_err
    };
    let http = line_pos(log, &["listening", "\"api\""]).expect("api listening");
    let unix = line_pos(log, &["listening", "\"local\""]).expect("local listening");
    let ready = line_pos(log, &["daemon 就绪"]).expect("daemon 就绪");
    assert!(
        http < ready && unix < ready,
        "daemon 就绪 应在两个 listening 之后\n{log}"
    );
}
