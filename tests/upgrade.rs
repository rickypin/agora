//! A39（agora-7ku.8）：一条命令升级节点——换二进制路径、重指 `<AGORA_HOME>/bin/agora`、重启 daemon，
//! agent 不死、会话与 metadata 完整；probe 守卫拒绝不认识现有库的"新版本"（MISSION §2.3 规则 10）。
//!
//! 真二进制 + 隔离 AGORA_HOME（短路径：macOS unix socket 路径上限 104 字节）+ 隔离 tmux socket
//! + 隔离自启单元。样板抄自 tests/single_instance.rs（本批 tests/common 归别的任务，不改）。
//!
//! ## 单元隔离（agora-t90q）
//!
//! `upgrade` 的重启探测要问 systemd / launchd，问的是宿主当前用户的**真**单元（`agora.service` /
//! `dev.agora.daemon`）：单元名不随 `AGORA_HOME` 走。装了真 daemon 的开发机上，这两条重启用例与
//! 开发者真在跑的单元之间原本没有屏障——现在不重启它，只是因为被测代码比对了单元的 ExecStart /
//! program 路径（agora-wyk）；那个判断一退化（只看 is-active、`systemctl show` 跑不起来、输出格式
//! 认不出——三种都落在 `None => true` 那一支）就又去 restart 真 daemon，测试自己跟着假红（2026-09-19
//! 上一批：5 个 worktree 各跑一次门禁，宿主 MainPID 连续被换）。测试的隔离不该寄托在被测代码的判断上。
//!
//! 屏障在这里造：`StandIns::new` 做两个只记 argv 的替身（假 systemctl、假 launchctl），经 src/cli/upgrade.rs
//! 的测试注入口 `AGORA_UPGRADE_SYSTEMCTL` / `AGORA_UPGRADE_LAUNCHCTL` 递给子进程。替身扮成「这台机器装了
//! 单元、单元也 active，但单元跑的是另一个人的 home」（开发机的形状），两条重启用例共用的
//! `assert_restart_went_through_the_pid_file` 据此钉三件事：① 走的是 pid 文件那一支（stderr 含「已停掉
//! pid」、不含「已 systemctl --user restart」）；② 探测真的发生了（替身被问过 is-active，也被问过
//! show -p ExecStart——只看 active 就不会问 show）；③ 谁都没被叫去 restart / kickstart。宿主那个真单元
//! 全程不在场，两条用例还各自断言一次宿主单元的状态指纹前后逐字不变（读的是测试进程自己 PATH 上的
//! 真 systemctl，不经替身）。`FOREIGN_UNIT_PROGRAM` 是守卫的开关：换成 `Home::bin_link()` 之后单元就
//! "属于本 home"了，两条用例必须红——那一次改坏正是 agora-wyk 修掉的东西。

#[path = "common/isolate.rs"]
mod isolate;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use agora::api::version::API_VERSION;
use agora::cli::upgrade::{read_pid, Probe, LAUNCHCTL_ENV, SYSTEMCTL_ENV, VERSIONS_DIR};
use agora::local::{self, Request, Response, SOCKET_FILE};
use agora::session::db::SCHEMA_VERSION;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const AGORA_BIN: &str = env!("CARGO_BIN_EXE_agora");

/// 替身扮的"这台机器上的单元"到底在跑谁的 agora：一个与本 fixture 无关的主人。
/// 路径不存在也是有意的：`same_binary_path` 只把两边的**父目录**各自 canonicalize 再比，本 home 之外
/// 的写法比不出同一个位置，就判成"不归这里"。改坏守卫就是把它换成 `Home::bin_link()`（同一处路径、
/// 且父目录解析得到）——那一次单元真就"属于本 home"了（agora-wyk 修的是同一个判据）。
const FOREIGN_UNIT_PROGRAM: &str = "/home/host-owner/.agora/bin/agora";

/// 重启探测的替身：假 `systemctl` + 假 `launchctl`，各自把收到的 argv 记一行到自己的日志。
/// 只钉"upgrade 怎么叫它们"，不模拟 service manager 的行为（同一写法先例：tests/install_script.rs
/// 的假 launchctl，agora-5gg.15）。
struct StandIns {
    systemctl: PathBuf,
    launchctl: PathBuf,
    systemctl_log: PathBuf,
    launchctl_log: PathBuf,
}

impl StandIns {
    /// 在 `<home>/standins/` 里造两个替身；`unit_program` 是替身嘴里"单元真正在跑的那份二进制"。
    fn new(home: &Path, unit_program: &Path) -> Self {
        let dir = home.join("standins");
        std::fs::create_dir_all(&dir).unwrap();
        let stand_in = |name: &str, script: &str| {
            let path = dir.join(name);
            // 要 exec 的替身走 isolate::exec_script（ETXTBSY 守卫，agora-pmm2）；探空 argv 被
            // 注入的守卫在 printf 之前挡掉，不会往替身自己的日志里多记一行。
            isolate::exec_script(&path, script)
        };
        // 日志路径写进脚本、不是经环境变量递给替身：替身可能被任意一层子进程拉起来
        // （upgrade 还会经 `sh` 起 daemon），要它一定记得到同一个地方就别依赖环境往下传。
        let systemctl_log = dir.join("systemctl.log");
        let launchctl_log = dir.join("launchctl.log");
        let program = unit_program.display().to_string();
        let systemctl = stand_in(
            "systemctl",
            &r#"#!/bin/sh
# 假 systemctl（tests/upgrade.rs，agora-t90q）：只记 argv，不碰任何真进程。
# 扮的是"装了真 daemon 的开发机"：单元 active，但单元跑的是另一个人的 home。
printf '%s\n' "$*" >> '__LOG__'
for a in "$@"; do
  case "$a" in
    is-active) printf 'active\n'; exit 0 ;;
    show) printf 'ExecStart={ path=__PROG__ ; argv[]=__PROG__ serve ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }\n'; exit 0 ;;
    restart) printf '假 systemctl：记下了一次 restart，不碰任何真进程\n'; exit 0 ;;
  esac
done
exit 0
"#
            .replace("__LOG__", &systemctl_log.display().to_string())
            .replace("__PROG__", &program),
        );
        let launchctl = stand_in(
            "launchctl",
            &r#"#!/bin/sh
# 假 launchctl（同上）：print 答"单元装载了，program 是别人的 home"，kickstart 只记账。
printf '%s\n' "$*" >> '__LOG__'
for a in "$@"; do
  case "$a" in
    print) printf 'gui/501/dev.agora.daemon = {\n\tactive count = 1\n\tstate = running\n\tprogram = __PROG__\n}\n'; exit 0 ;;
    kickstart) printf '假 launchctl：记下了一次 kickstart，不碰任何真进程\n'; exit 0 ;;
  esac
done
exit 0
"#
            .replace("__LOG__", &launchctl_log.display().to_string())
            .replace("__PROG__", &program),
        );
        StandIns {
            systemctl,
            launchctl,
            systemctl_log,
            launchctl_log,
        }
    }

    /// 要递给 `agora` 子进程的两个注入口（变量名取自被测代码，不在这份文件里重复字面值）。
    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            (SYSTEMCTL_ENV, self.systemctl.clone()),
            (LAUNCHCTL_ENV, self.launchctl.clone()),
        ]
    }

    fn calls(&self, log: &Path) -> Vec<String> {
        match std::fs::read_to_string(log) {
            Ok(text) => text.lines().map(str::to_owned).collect(),
            // 一次都没被叫到：空列表（"探测到底发生了没有"由下面的断言自己钉）。
            Err(_) => Vec::new(),
        }
    }

    fn systemd_calls(&self) -> Vec<String> {
        self.calls(&self.systemctl_log)
    }

    fn launchd_calls(&self) -> Vec<String> {
        self.calls(&self.launchctl_log)
    }
}

/// 宿主上真实自启单元的状态指纹（`systemctl --user show agora.service -p MainPID -p
/// ActiveEnterTimestamp` 的原文）。读不到 → None：没有 systemctl（macOS）、没有用户 systemd、没装
/// 单元，还有验收时把 systemctl 换成替身的那一轮——都是"没有东西可以被重启"。
/// 这里走测试进程自己 PATH 上的 systemctl，不经替身：注入口换掉的只是被测子进程要调用的那一个。
fn host_unit_state() -> Option<String> {
    let out = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "agora.service",
            "-p",
            "MainPID",
            "-p",
            "ActiveEnterTimestamp",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    // 单元没装时 systemctl 照样退 0，只是 MainPID=0、ActiveEnterTimestamp 为空——同样没东西可重启。
    if !text.contains("MainPID=") || text.contains("MainPID=0") {
        return None;
    }
    Some(text)
}

struct Home {
    path: PathBuf,
    tmux_socket: String,
    port: u16,
    stand_ins: StandIns,
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
            path: path.clone(),
            tmux_socket,
            port,
            // 替身装在 <home>/standins/ 里，随 Drop 一起删；单元路径见 FOREIGN_UNIT_PROGRAM。
            stand_ins: StandIns::new(&path, Path::new(FOREIGN_UNIT_PROGRAM)),
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

    /// 一律带上单元替身（不只 upgrade）：`agora` 子进程里任何一个去探测重启方式，都撞不到宿主的真单元。
    fn agora(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(AGORA_BIN);
        cmd.args(args)
            .env("AGORA_HOME", &self.path)
            .env("AGORA_LOG", "info")
            .env_remove("AGORA_LOG_FORMAT");
        for (k, v) in self.stand_ins.env() {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
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
        // 复制完的产物也要 exec 得起：写句柄被别的线程 fork 走就是 ETXTBSY（agora-pmm2）。
        // 探针 argv 用 `upgrade --probe`：被测代码自己就会这么叫它，而 --probe 不读配置、不碰
        // AGORA_HOME（src/main.rs），探一次不留痕迹。
        isolate::mark_executable(&dst, &["upgrade", "--probe"]);
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

/// 两条重启用例共用的隔离断言（agora-t90q，缘由见文件头「单元隔离」一段）：
/// 走的必须是 pid 文件那一支，systemctl / launchctl 只被问过、没被叫去重启任何东西。
///
/// 放在 `assert!(out.status.success())` 前面：守卫被改坏时先红的那句得是"隔离没了"，
/// 而不是退化成 15 s 后等不到 `/api/health`——那种红认不出根因（替身里的 restart 是空的，
/// 测试自己那个 daemon 没被重起，只会卡到健康超时）。
fn assert_restart_went_through_the_pid_file(home: &Home, out: &Output, old_pid: u32) {
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let systemd = home.stand_ins.systemd_calls();
    let launchd = home.stand_ins.launchd_calls();
    let context = format!(
        "\nstderr：{stderr}\n假 systemctl 收到的 argv：{systemd:?}\n假 launchctl 收到的 argv：{launchd:?}"
    );

    assert!(
        stderr.contains(&format!("已停掉 pid {old_pid}")),
        "upgrade 没走 pid 文件那一支（没停掉测试自己起的 pid {old_pid}）——它去碰 service manager 了？{context}"
    );
    assert!(
        !stderr.contains("已 systemctl --user restart"),
        "upgrade 去 systemctl --user restart 了：{context}"
    );
    assert!(
        !stderr.contains("已 launchctl kickstart"),
        "upgrade 去 launchctl kickstart 了：{context}"
    );

    // 探测得真的发生过：替身被问过 is-active，也被问过 show -p ExecStart（退化成只看 active 就不会问 show）。
    assert!(
        systemd.iter().any(|c| c.contains("is-active")),
        "重启探测一次都没调用 systemctl（注入口没生效，或探测顺序被改了）：{context}"
    );
    assert!(
        systemd.iter().any(|c| c.contains("show")),
        "探测只看了 is-active，没核对单元属于哪个 home（agora-wyk 回潮）：{context}"
    );
    assert!(
        !systemd.iter().any(|c| c.contains("restart")),
        "假 systemctl 被叫去了 restart（宿主的真单元就是这么被测试重启的）：{context}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            launchd.iter().any(|c| c.contains("print")),
            "macOS 上重启探测没查过 launchd（注入口没生效？）：{context}"
        );
    }
    assert!(
        !launchd.iter().any(|c| c.contains("kickstart")),
        "假 launchctl 被叫去了 kickstart：{context}"
    );
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

    // "新版本"= 同一份二进制换个路径；单元探测撞上的是替身（见文件头），走 pid 文件那一支。
    let new_bin = home.new_binary("agora-new");
    let _old = old.reap_in_background();
    let host_before = host_unit_state();
    let out = home.upgrade(&new_bin, &[]);
    assert_restart_went_through_the_pid_file(&home, &out, old_pid);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        host_unit_state(),
        host_before,
        "宿主的 agora.service 被动过：测试又去重启真 daemon 了"
    );

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
    let old_pid = old.pid();
    let _old = old.reap_in_background();
    let host_before = host_unit_state();
    let out = home.upgrade(&new_bin, &[]);
    assert_restart_went_through_the_pid_file(&home, &out, old_pid);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        host_unit_state(),
        host_before,
        "宿主的 agora.service 被动过：测试又去重启真 daemon 了"
    );
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
    // 要 exec 的假新二进制走 isolate::exec_script（ETXTBSY 守卫，agora-pmm2）。
    isolate::exec_script(
        &script,
        "#!/bin/sh\nif [ \"$1\" = upgrade ] && [ \"$2\" = --probe ]; then\n  printf '{\"schema_version\": 1, \"api_version\": \"1.4\"}\\n'\nfi\nexit 0\n",
    );
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
