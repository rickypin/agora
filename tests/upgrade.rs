//! A39（agora-7ku.8）：一条命令升级节点——换二进制路径、重指 `<AGORA_HOME>/bin/agora`、重启 daemon，
//! agent 不死、会话与 metadata 完整；probe 守卫拒绝不认识现有库的"新版本"（MISSION §2.3 规则 10）。
//!
//! 真二进制 + 隔离 AGORA_HOME（短路径：macOS unix socket 路径上限 104 字节）+ 隔离 tmux socket。
//! 样板抄自 tests/single_instance.rs（本批 tests/common 归别的任务，不改）。测试里没有 systemd /
//! launchd，`upgrade` 走的是 pid 文件那一支：SIGTERM 旧 daemon、经链接起新的。

#[path = "common/isolate.rs"]
mod isolate;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use agora::api::version::API_VERSION;
use agora::cli::upgrade::{read_pid, Probe, VERSIONS_DIR};
use agora::local::{self, Request, Response, SOCKET_FILE};
use agora::session::db::SCHEMA_VERSION;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const AGORA_BIN: &str = env!("CARGO_BIN_EXE_agora");
struct Home {
    path: PathBuf,
    tmux_socket: String,
    port: u16,
}

impl Home {
    fn new() -> Self {
        let n = isolate::nth();
        let path = isolate::home_dir("up", n);
        let _ = std::fs::remove_dir_all(&path);
        local::ensure_home(&path).unwrap();
        let tmux_socket = isolate::socket_name("up", n);
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

    fn bin_link(&self) -> PathBuf {
        self.path.join("bin").join("agora")
    }

    /// 测试自己起的 daemon；日志进 `<home>/<name>.log`，免得 stderr 管道没人读把它卡死。
    fn spawn_serve(&self, name: &str) -> Daemon {
        let log = std::fs::File::create(self.path.join(format!("{name}.log"))).unwrap();
        Daemon(Some(
            Command::new(AGORA_BIN)
                .arg("serve")
                .env("AGORA_HOME", &self.path)
                .env("AGORA_LOG", "info")
                .env_remove("AGORA_LOG_FORMAT")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        ))
    }

    fn agora(&self, args: &[&str]) -> Output {
        Command::new(AGORA_BIN)
            .args(args)
            .env("AGORA_HOME", &self.path)
            .env("AGORA_LOG", "info")
            .env_remove("AGORA_LOG_FORMAT")
            .output()
            .unwrap()
    }

    fn upgrade(&self, from: &Path, extra: &[&str]) -> Output {
        let from = from.display().to_string();
        let mut args = vec!["upgrade", "--from", from.as_str()];
        args.extend_from_slice(extra);
        self.agora(&args)
    }

    /// 把当前二进制复制到 home 里另一个路径当"新版本"（内容相同也算换路径）。
    fn new_binary(&self, name: &str) -> PathBuf {
        let dst = self.path.join(name);
        std::fs::copy(AGORA_BIN, &dst).unwrap();
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).unwrap();
        dst
    }

    /// `versions/<sha256 前 12 位>/agora`，与 upgrade 的放置约定同源。
    fn staged_path_for(&self, binary: &Path) -> PathBuf {
        let bytes = std::fs::read(binary).unwrap();
        let sha: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.path.join(VERSIONS_DIR).join(&sha[..12]).join("agora")
    }

    fn pane_pids(&self) -> Vec<String> {
        let out = Command::new("tmux")
            .args([
                "-L",
                &self.tmux_socket,
                "list-panes",
                "-a",
                "-F",
                "#{pane_pid}",
            ])
            .output()
            .unwrap();
        let mut pids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        pids.sort();
        pids
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        isolate::kill_tmux(&self.tmux_socket);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 测试自己起的 daemon：断言失败 panic 时也要收掉。
struct Daemon(Option<Child>);

impl Daemon {
    fn pid(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }

    /// 交给 upgrade 去停：另起线程 wait 它，退出后立刻被收尸——`kill(pid, 0)` 对僵尸也成功，
    /// 不收尸 upgrade 会一直等到超时（src/cli/upgrade.rs `alive` 的注记）。
    fn reap_in_background(mut self) -> Reaper {
        let mut child = self.0.take().unwrap();
        let pid = child.id();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Reaper { pid }
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

/// 只认 pid 的守卫：测试失败在 upgrade 停掉它之前时仍能收掉。
struct Reaper {
    pid: u32,
}

impl Drop for Reaper {
    fn drop(&mut self) {
        term_and_wait(self.pid, Duration::from_secs(5));
    }
}

/// 由 upgrade 起出去的 daemon：只能经 pid 文件认识它。声明在 `Home` 之后——先于它 Drop，
/// tmux server 与目录还在时就把 daemon 停掉。
struct Spawned {
    home: PathBuf,
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if let Some(pid) = read_pid(&self.home) {
            term_and_wait(pid, Duration::from_secs(10));
        }
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: 信号 0 只做存在性检查。
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn term_and_wait(pid: u32, limit: Duration) {
    if !alive(pid) {
        return;
    }
    // SAFETY: pid 是本测试起的（或本测试的 upgrade 起的）daemon。
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + limit;
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 等 daemon 在 socket 上答 Pong；启动含 PATH 探测（最长 5 s）与 tmux 版本探测。
/// 等的是一个真起来的 `target/debug/agora` 子进程，上限按 `isolate::PROC` 走（agora-pea）。
async fn wait_pong(sock: &Path) {
    let deadline = Instant::now() + isolate::PROC;
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

// ---------- 最小 HTTP 客户端（明文监听器，Connection: close） ----------

struct Reply {
    status: u16,
    head: String,
    body: String,
}

fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Reply {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("不是 HTTP 应答: {raw:?}"));
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    Reply {
        status,
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

/// `agora url` 铸链接 → `POST /api/auth/pair` 换 cookie（`agora_session=<token>`）。
fn pair(home: &Home) -> String {
    let url = home.agora(&["url"]);
    let link = String::from_utf8_lossy(&url.stdout).trim().to_owned();
    let token = link
        .split_once("#pair=")
        .unwrap_or_else(|| {
            panic!(
                "agora url: {link:?} / {}",
                String::from_utf8_lossy(&url.stderr)
            )
        })
        .1
        .to_owned();
    let reply = http(
        home.port,
        "POST",
        "/api/auth/pair",
        &[],
        Some(&json!({ "token": token }).to_string()),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let cookie = reply
        .head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().split(';').next().unwrap().to_owned())
        .expect("Set-Cookie");
    assert!(cookie.starts_with("agora_session="), "{cookie}");
    cookie
}

fn create_session(home: &Home, cookie: &str, name: &str, script: &str) -> Value {
    let origin = format!("http://127.0.0.1:{}", home.port);
    let body = json!({
        "display_name": name,
        "agent_type": "fake",
        "working_directory": home.path,
        "command": format!("{AGORA_BIN} fake-agent -e \"{script}\""),
    });
    let reply = http(
        home.port,
        "POST",
        "/api/sessions",
        &[("Cookie", cookie), ("Origin", origin.as_str())],
        Some(&body.to_string()),
    );
    assert_eq!(reply.status, 201, "{}", reply.body);
    serde_json::from_str(&reply.body).unwrap()
}

fn list_sessions(home: &Home, cookie: &str) -> Vec<Value> {
    let reply = http(
        home.port,
        "GET",
        "/api/sessions",
        &[("Cookie", cookie)],
        None,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    let v: Value = serde_json::from_str(&reply.body).unwrap();
    v["sessions"].as_array().cloned().unwrap_or_default()
}

/// `(id, name, status)` 按 id 排序——升级前后要逐字相等的那三样。
fn rows(sessions: &[Value]) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = sessions
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["name"].as_str().unwrap().to_owned(),
                s["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// 上限一律给 `isolate::PROC`：这里等的都是"另一个进程做完事"（daemon 起来、会话列出来、
/// hook 落到 external 行），满载时争抢 CPU，30 s / 15 s 在 2026-09-08 那批就不够（agora-pea
/// 点名 upgrade 的三条）。断言原样保留，超时照样红。
fn wait_until<T>(limit: Duration, mut probe: impl FnMut() -> Result<T, String>) -> T {
    let deadline = Instant::now() + limit;
    loop {
        match probe() {
            Ok(v) => return v,
            Err(why) => {
                assert!(Instant::now() < deadline, "超时: {why}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn stderr_of(out: &Output) -> String {
    format!(
        "status={} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[tokio::test]
async fn daemon_restart_keeps_agents_sessions_and_metadata() {
    let home = Home::new();
    let spawned = Spawned {
        home: home.path.clone(),
    };
    let old = home.spawn_serve("old-daemon");
    wait_pong(&home.socket()).await;
    let old_pid = old.pid();
    assert_eq!(
        read_pid(&home.path),
        Some(old_pid),
        "serve 绑定后要写 agora.pid"
    );
    let cookie = pair(&home);

    // 十个 fake-agent 会话，都 RUNNING 并挂着。
    for i in 0..10 {
        create_session(
            &home,
            &cookie,
            &format!("upg-{i}"),
            &format!("print READY-{i}; sleep 60000"),
        );
    }
    let before = wait_until(isolate::PROC, || {
        let rows = rows(&list_sessions(&home, &cookie));
        if rows.len() == 10 && rows.iter().all(|(_, _, st)| st == "running") {
            Ok(rows)
        } else {
            Err(format!("{rows:?}"))
        }
    });
    let panes_before = home.pane_pids();
    assert_eq!(panes_before.len(), 10, "{panes_before:?}");

    // "新版本"= 同一份二进制换个路径；此时没有 systemd / launchd，走 pid 文件那一支。
    let new_bin = home.new_binary("agora-new");
    let _old = old.reap_in_background();
    let out = home.upgrade(&new_bin, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    // 链接指向 versions/<sha12>/agora（真路径比）。
    let link = home.bin_link();
    assert_eq!(
        link.canonicalize().unwrap(),
        home.staged_path_for(&new_bin).canonicalize().unwrap(),
        "{}",
        stderr_of(&out)
    );
    // 新 pid ≠ 旧 pid，且活着。
    let new_pid = read_pid(&home.path).expect("新 daemon 的 agora.pid");
    assert_ne!(new_pid, old_pid);
    assert!(alive(new_pid));
    assert!(!alive(old_pid), "旧 daemon 应已退出");

    // 同样十行：id、name、status 仍 running；pane 一个没换。
    wait_pong(&home.socket()).await;
    let after = wait_until(isolate::PROC, || {
        let rows = rows(&list_sessions(&home, &cookie));
        if rows == before {
            Ok(rows)
        } else {
            Err(format!("before={before:?}\nafter={rows:?}"))
        }
    });
    assert_eq!(after, before);
    assert_eq!(home.pane_pids(), panes_before, "升级不得动任何 agent 进程");
    drop(spawned);
}

#[tokio::test]
async fn bin_link_repointed_and_hooks_still_deliver() {
    let home = Home::new();
    let spawned = Spawned {
        home: home.path.clone(),
    };
    let old = home.spawn_serve("old-daemon");
    wait_pong(&home.socket()).await;
    let cookie = pair(&home);

    let new_bin = home.new_binary("agora-new");
    let _old = old.reap_in_background();
    let out = home.upgrade(&new_bin, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    wait_pong(&home.socket()).await;

    // 升级后经 <AGORA_HOME>/bin/agora 这条**链接**跑 hook（安装进 agent 配置的命令就是这个路径），
    // stdin 喂一个 SessionStart 信封（形态照 testdata/claude/2.1.261/hooks/turn_complete.jsonl 首行）。
    let link = home.bin_link();
    // tmp 在 macOS 上是 /var → /private/var：两边都 canonicalize 再比。
    assert!(link
        .canonicalize()
        .unwrap()
        .starts_with(home.path.join(VERSIONS_DIR).canonicalize().unwrap()));
    let payload = json!({
        "session_id": "upg-ext-1",
        "transcript_path": "/tmp/upg-ext-1.jsonl",
        "cwd": home.path,
        "hook_event_name": "SessionStart",
        "source": "startup",
        "model": "<model>"
    });
    let mut hook = Command::new(&link)
        .args([
            "hook",
            "--host",
            "claude",
            "--home",
            &home.path.display().to_string(),
        ])
        .env_remove("GROK_SESSION_ID")
        .env_remove("AGORA_SESSION_ID")
        .env_remove("AGORA_EPOCH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    hook.stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let hook = hook.wait_with_output().unwrap();
    assert!(hook.status.success(), "{}", stderr_of(&hook));

    // 事实：GET /api/sessions 里出现该 agent_session_id 的 external 行。
    wait_until(isolate::PROC, || {
        let sessions = list_sessions(&home, &cookie);
        sessions
            .iter()
            .find(|s| s["agent_session_id"] == "upg-ext-1" && s["origin"] == "external")
            .cloned()
            .ok_or_else(|| format!("{sessions:?}"))
    });
    drop(spawned);
}

#[tokio::test]
async fn migration_versioned_and_older_daemon_refuses() {
    let home = Home::new();
    let daemon = home.spawn_serve("daemon");
    wait_pong(&home.socket()).await;
    let pid = daemon.pid();

    // 迁移带版本号：daemon 打开过的库 user_version == SCHEMA_VERSION；`--probe` 自报的也是它。
    let conn = rusqlite::Connection::open_with_flags(
        home.path.join("agora.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let user_version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(user_version, SCHEMA_VERSION);
    let probe = home.agora(&["upgrade", "--probe"]);
    assert!(probe.status.success(), "{}", stderr_of(&probe));
    let probe: Probe = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(probe.schema_version, SCHEMA_VERSION);
    assert_eq!(probe.api_version, API_VERSION.to_string());

    // 先做一次不重启的正常升级把链接建起来，好断言"拒绝时链接不动"。
    let new_bin = home.new_binary("agora-new");
    let out = home.upgrade(&new_bin, &["--no-restart"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let link = home.bin_link();
    let target_before = std::fs::read_link(&link).unwrap();
    assert_eq!(read_pid(&home.path), Some(pid), "--no-restart 不动 daemon");

    // 一个 sh 脚本当"新二进制"：对 `--probe` 说自己只认识 schema 1（低于现有库），其余退出 0。
    let script = home.path.join("fake-new.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nif [ \"$1\" = upgrade ] && [ \"$2\" = --probe ]; then\n  printf '{\"schema_version\": 1, \"api_version\": \"1.4\"}\\n'\nfi\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = home.upgrade(&script, &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        target_before,
        "拒绝时链接不动"
    );
    assert_eq!(read_pid(&home.path), Some(pid), "拒绝时 daemon 不动");
    assert!(alive(pid));
    assert_eq!(
        local::request(&home.socket(), &Request::Ping)
            .await
            .unwrap(),
        Response::Pong
    );
    assert!(
        !home.staged_path_for(&script).exists(),
        "被拒绝的那份不该留在 versions/ 里"
    );
    // 运行期的后向守卫——旧程序打开更新的库要明确拒绝（DbError::TooNew）——已由
    // tests/schema.rs::newer_database_is_refused_not_downgraded 钉死，这里不重复实现。
}
