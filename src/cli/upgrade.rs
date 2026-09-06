//! `agora upgrade --from <新二进制> [--no-restart]` 与 `agora upgrade --probe`（A39；MISSION §2.3
//! 规则 10；agora-7ku.8）。升级只换 agora 自己——运行时与自启单元归安装脚本（ADR-001 D7，agora-7ku.1）。
//!
//! `--from` 的步骤（docs/spec/config.md「升级」）：
//! ① 读 `<from>` 算 SHA-256，放进 `<AGORA_HOME>/versions/<sha 前 12 位>/agora`（0755；同一份已在就复用）；
//! ② 以那一份跑 `upgrade --probe`，让**新**二进制自报能力——stdout 一行 JSON
//!    `{"schema_version", "api_version"}`。新二进制认识的库版本低于现有库的 `user_version` → 退出码 2、
//!    链接不动：这是规则 10 的前向守卫（旧程序打开新库的后向守卫是 `DbError::TooNew`）。
//!    probe 跑不起来 / 非 0 / 不是 JSON → 退出码 1。程序读 JSON，不读人话。
//! ③ 重指 `<AGORA_HOME>/bin/agora`——与 `hooks install` 同一个 [`ensure_bin_link`]，exe 传 canonicalize
//!    后的真路径。经 `<AGORA_HOME>/bin/agora upgrade …` 被调用时 `current_exe` 是链接本身（macOS 不解析，
//!    agora-78f）：这里根本不看 `current_exe`，新旧两边都按真路径比，造不出 link → link。
//! ④ 重启 daemon（`--no-restart` 跳过），按顺序探测：systemd 用户单元 `agora.service` 活着 →
//!    `systemctl --user restart`；macOS 上 launchd 里有 `dev.agora.daemon` → `launchctl kickstart -k`；
//!    都不是 → 读 `<AGORA_HOME>/agora.pid`，进程活着就 SIGTERM、等它退出（≤ 10 s，超时报错不 SIGKILL），
//!    再以 `<AGORA_HOME>/bin/agora serve` 脱离终端起新的（stdout+stderr 追加到 `<AGORA_HOME>/daemon.log`）；
//!    pid 文件不在或进程不在 → 只重指链接，"daemon 未在运行，下次启动即新版本"。
//! ⑤ 轮询 `GET http://<server.listen>/api/health`（公开子集）到 200 `{"status":"ok"}` 且 pid 文件里
//!    换了新 pid（≤ 15 s）。
//!
//! 人话全走 stderr；退出码：0 完成，2 拒绝（用法错误、新版本不认识这个库——什么都没动），
//! 1 中途失败（错误文本说明链接是否已重指）。

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::api::version::API_VERSION;
use crate::hook::install::{bin_path, ensure_bin_link, InstallError};
use crate::runtime::exec::{exec, ExecError, ExecOptions, ExitStatus};
use crate::session::db::SCHEMA_VERSION;

/// daemon 的 pid 文件：`serve` 绑定成功后写（0600，十进制 pid + 换行），退出时删。只给 upgrade 与人看；
/// 单实例检查另有 socket 探活（agora-apr），不靠它。
pub const PID_FILE: &str = "agora.pid";
/// 按内容哈希存放的二进制：`<AGORA_HOME>/versions/<sha256 前 12 位>/agora`。
pub const VERSIONS_DIR: &str = "versions";
/// pid 文件那一支起的 daemon 的 stdout + stderr。
pub const DAEMON_LOG: &str = "daemon.log";
/// 安装脚本写的 systemd 用户单元与 launchd label（agora-7ku.1；两边的约定见 config.md「升级」）。
pub const SYSTEMD_UNIT: &str = "agora.service";
pub const LAUNCHD_LABEL: &str = "dev.agora.daemon";

/// 旧 daemon 收到 SIGTERM 后最多等这么久；超时报错、不 SIGKILL——它可能正在收尾。
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// 重启后等 `/api/health` 的上限（启动含 PATH 探测最长 5 s 与 reconcile）。
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);
/// 新二进制跑 `--probe` 的上限。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// `agora upgrade --probe` 的输出：新二进制自报它认识的库版本与实现的 API 版本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub schema_version: i64,
    pub api_version: String,
}

impl Probe {
    pub fn current() -> Self {
        Probe {
            schema_version: SCHEMA_VERSION,
            api_version: API_VERSION.to_string(),
        }
    }
}

/// `agora upgrade --probe`：stdout 一行 JSON，退出 0。不读配置、不碰 AGORA_HOME——被问的是二进制本身。
pub fn probe() -> i32 {
    match serde_json::to_string(&Probe::current()) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(err) => {
            eprintln!("agora upgrade --probe: {err}");
            1
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("{0}")]
    Usage(String),
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("新二进制 {path} 跑不起 `upgrade --probe`: {reason}")]
    ProbeFailed { path: String, reason: String },
    #[error("新二进制 {path} 的 probe 输出不是能读的 JSON: {reason}")]
    ProbeUnreadable { path: String, reason: String },
    #[error(
        "新版本不认识这个库：库的 user_version 是 {found}，新二进制只认识到 {supported}；链接未动"
    )]
    SchemaTooOld { found: i64, supported: i64 },
    #[error("读库版本失败: {0}")]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Link(#[from] InstallError),
    #[error("{0}")]
    Exec(#[from] ExecError),
    #[error("{what} 失败（{status:?}）: {stderr}；链接已重指，daemon 仍是旧版本")]
    Restart {
        what: String,
        status: ExitStatus,
        stderr: String,
    },
    #[error("daemon（pid {pid}）在 {waited:?} 内没有退出；链接已重指，daemon 仍是旧版本，手动停掉它再起 `agora serve`")]
    DaemonStuck { pid: u32, waited: Duration },
    #[error("重启后 daemon 在 {waited:?} 内没有在 {addr} 上以新 pid 答 /api/health；链接已重指，看 {log}")]
    HealthTimeout {
        addr: SocketAddr,
        waited: Duration,
        log: String,
    },
}

impl UpgradeError {
    /// 2 = 拒绝、什么都没动（用法错误、新版本不认识这个库）；1 = 中途失败。
    pub fn exit_code(&self) -> i32 {
        match self {
            UpgradeError::Usage(_) | UpgradeError::SchemaTooOld { .. } => 2,
            _ => 1,
        }
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> UpgradeError + '_ {
    move |source| UpgradeError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn usage() -> String {
    "用法: agora upgrade --from <新二进制路径> [--no-restart] | agora upgrade --probe".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub from: PathBuf,
    pub restart: bool,
}

pub fn parse_args(argv: &[&str]) -> Result<Options, UpgradeError> {
    let mut from = None;
    let mut restart = true;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match *a {
            "--from" => from = it.next().map(PathBuf::from),
            "--no-restart" => restart = false,
            other => {
                return Err(UpgradeError::Usage(format!(
                    "未知参数 {other}\n{}",
                    usage()
                )))
            }
        }
    }
    let from = from.ok_or_else(|| UpgradeError::Usage(format!("缺 --from\n{}", usage())))?;
    Ok(Options { from, restart })
}

/// daemon 是怎么重启的（或为什么没重启）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartOutcome {
    /// `--no-restart`。
    Skipped,
    /// pid 文件不在或进程不在：只重指了链接。
    NotRunning,
    Systemd,
    Launchd,
    /// pid 文件那一支：停掉 `old_pid`，以链接重新起了一个。
    Spawned {
        old_pid: u32,
    },
}

#[derive(Debug, Clone)]
pub struct Report {
    pub probe: Probe,
    /// `versions/<sha12>/agora` 的真路径。
    pub staged: PathBuf,
    pub link: PathBuf,
    /// 重指前链接指向的地方（没有链接 → None）。
    pub previous: Option<PathBuf>,
    pub restart: RestartOutcome,
    /// 重启后新 daemon 的 pid（没重启 → None）。
    pub pid: Option<u32>,
}

/// 入口。`home` 与 `listen` 由 main 用 daemon 同一套解析给出。
pub fn run(argv: &[&str], home: &Path, listen: SocketAddr) -> i32 {
    let opts = match parse_args(argv) {
        Ok(o) => o,
        Err(err) => {
            eprintln!("{err}");
            return err.exit_code();
        }
    };
    match run_with(&opts, home, listen, &mut std::io::stderr()) {
        Ok(report) => {
            let previous = report
                .previous
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(无链接)".to_owned());
            let pid = report
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "未重启".to_owned());
            eprintln!("{previous} → {}，daemon pid {pid}", report.staged.display());
            0
        }
        Err(err) => {
            eprintln!("agora upgrade: {err}");
            err.exit_code()
        }
    }
}

/// 全部步骤；给人看的进度写进 `out`。
pub fn run_with(
    opts: &Options,
    home: &Path,
    listen: SocketAddr,
    out: &mut dyn Write,
) -> Result<Report, UpgradeError> {
    let staged = stage(home, &opts.from)?;
    // probe 或版本守卫拒绝时把这一次放进去的副本收走，versions/ 里不留没用过的东西；
    // 本来就在的那份（同 sha 复用）不动。
    let discard = |staged: &Staged| {
        if staged.created {
            if let Some(dir) = staged.path.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    };
    let probe = match probe_binary(&staged.path) {
        Ok(p) => p,
        Err(err) => {
            discard(&staged);
            return Err(err);
        }
    };
    let found = current_user_version(home)?;
    if probe.schema_version < found {
        discard(&staged);
        return Err(UpgradeError::SchemaTooOld {
            found,
            supported: probe.schema_version,
        });
    }
    say(
        out,
        format!(
            "新二进制就位：{}（schema {}，api {}）",
            staged.path.display(),
            probe.schema_version,
            probe.api_version
        ),
    )?;

    let exe = staged.path.canonicalize().map_err(io(&staged.path))?;
    let link_path = bin_path(home);
    let previous = std::fs::read_link(&link_path).ok();
    let link = ensure_bin_link(home, &exe)?;
    say(
        out,
        format!(
            "{}: {} → {}",
            link.display(),
            previous
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(无)".to_owned()),
            exe.display()
        ),
    )?;

    let old_pid = read_pid(home);
    let restart = if opts.restart {
        restart_daemon(home, &link, old_pid, out)?
    } else {
        say(out, "未重启（--no-restart）；daemon 下次启动即新版本")?;
        RestartOutcome::Skipped
    };
    let pid = match restart {
        RestartOutcome::Skipped | RestartOutcome::NotRunning => None,
        _ => {
            let pid = wait_healthy(home, listen, old_pid, HEALTH_TIMEOUT)?;
            say(out, format!("daemon 就绪：pid {pid}（/api/health 200）"))?;
            Some(pid)
        }
    };
    Ok(Report {
        probe,
        staged: exe,
        link,
        previous,
        restart,
        pid,
    })
}

fn say(out: &mut dyn Write, line: impl AsRef<str>) -> Result<(), UpgradeError> {
    writeln!(out, "{}", line.as_ref()).map_err(io(Path::new("stderr")))
}

// ---------- ① 放置 ----------

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Staged {
    path: PathBuf,
    /// 这一次才复制进去的（同 sha 已在则 false）。
    created: bool,
}

/// `<from>` → `<AGORA_HOME>/versions/<sha256 前 12 位>/agora`，0755；先写 `.part` 再 rename，
/// 半个文件永远不会顶着正式名字。
fn stage(home: &Path, from: &Path) -> Result<Staged, UpgradeError> {
    let bytes = std::fs::read(from).map_err(io(from))?;
    let sha = sha256_hex(&bytes);
    let dir = home.join(VERSIONS_DIR).join(&sha[..12]);
    let path = dir.join("agora");
    if path.is_file() {
        return Ok(Staged {
            path,
            created: false,
        });
    }
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    let part = dir.join("agora.part");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&part)
            .map_err(io(&part))?;
        f.write_all(&bytes).map_err(io(&part))?;
        // umask 可能吞掉 x 位；显式设一次。
        f.set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(io(&part))?;
    }
    std::fs::rename(&part, &path).map_err(io(&path))?;
    Ok(Staged {
        path,
        created: true,
    })
}

// ---------- ② probe 与库版本 ----------

/// 以 `<path> upgrade --probe` 问新二进制认识什么。跑不起来 / 非 0 → `ProbeFailed`，
/// stdout 不是 [`Probe`] 形态 → `ProbeUnreadable`。
fn probe_binary(path: &Path) -> Result<Probe, UpgradeError> {
    let display = path.display().to_string();
    let argv: [&OsStr; 3] = [
        path.as_os_str(),
        OsStr::new("upgrade"),
        OsStr::new("--probe"),
    ];
    let out = exec(
        &argv,
        &ExecOptions {
            timeout: Some(PROBE_TIMEOUT),
            ..ExecOptions::default()
        },
    )
    .map_err(|err| UpgradeError::ProbeFailed {
        path: display.clone(),
        reason: err.to_string(),
    })?;
    if !out.status.success() {
        return Err(UpgradeError::ProbeFailed {
            path: display,
            reason: format!(
                "{:?}，stderr: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr_tail).trim()
            ),
        });
    }
    serde_json::from_slice(&out.stdout).map_err(|err| UpgradeError::ProbeUnreadable {
        path: display,
        reason: format!(
            "{err}（stdout: {:?}）",
            String::from_utf8_lossy(&out.stdout).trim()
        ),
    })
}

/// 现有库的 `PRAGMA user_version`，**只读打开、不迁移**——用 `Db::open` 会先把库迁到本程序的版本，
/// 那正是这里要防的事。库还不存在 → 0（没有东西可以不认识）。
fn current_user_version(home: &Path) -> Result<i64, UpgradeError> {
    let path = home.join("agora.db");
    if !path.exists() {
        return Ok(0);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

// ---------- ④ 重启 ----------

fn restart_daemon(
    home: &Path,
    link: &Path,
    old_pid: Option<u32>,
    out: &mut dyn Write,
) -> Result<RestartOutcome, UpgradeError> {
    if systemd_unit_active() {
        run_checked(
            &["systemctl", "--user", "restart", SYSTEMD_UNIT],
            "systemctl --user restart",
        )?;
        say(out, format!("已 systemctl --user restart {SYSTEMD_UNIT}"))?;
        return Ok(RestartOutcome::Systemd);
    }
    if cfg!(target_os = "macos") {
        let target = launchd_target();
        if launchd_loaded(&target) {
            run_checked(
                &["launchctl", "kickstart", "-k", &target],
                "launchctl kickstart -k",
            )?;
            say(out, format!("已 launchctl kickstart -k {target}"))?;
            return Ok(RestartOutcome::Launchd);
        }
    }
    let Some(pid) = old_pid.filter(|p| alive(*p)) else {
        say(out, "daemon 未在运行，下次启动即新版本")?;
        return Ok(RestartOutcome::NotRunning);
    };
    terminate(pid)?;
    wait_gone(pid, STOP_TIMEOUT)?;
    let log = home.join(DAEMON_LOG);
    spawn_detached(home, link, &log)?;
    say(
        out,
        format!(
            "已停掉 pid {pid}，以 {} serve 重新启动（日志 {}）",
            link.display(),
            log.display()
        ),
    )?;
    Ok(RestartOutcome::Spawned { old_pid: pid })
}

/// `systemctl --user is-active agora.service` 答 `active` 才算；systemctl 不存在（macOS）、
/// 没有用户 systemd、单元没装都算不是。
fn systemd_unit_active() -> bool {
    exec(
        &["systemctl", "--user", "is-active", SYSTEMD_UNIT],
        &ExecOptions::default(),
    )
    .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "active")
    .unwrap_or(false)
}

fn launchd_target() -> String {
    // SAFETY: getuid 没有前置条件、不会失败。
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}/{LAUNCHD_LABEL}")
}

/// `launchctl print gui/<uid>/dev.agora.daemon` 成功 = 单元已装载（没装载是退出码非 0）。
fn launchd_loaded(target: &str) -> bool {
    exec(&["launchctl", "print", target], &ExecOptions::default())
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_checked(argv: &[&str], what: &str) -> Result<(), UpgradeError> {
    let out = exec(argv, &ExecOptions::default())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(UpgradeError::Restart {
            what: what.to_owned(),
            status: out.status,
            stderr: String::from_utf8_lossy(&out.stderr_tail).trim().to_owned(),
        })
    }
}

/// `kill(pid, 0)`：成功或 EPERM 都是"有这个进程"。
///
/// 已知盲点（2026-09-06）：僵尸也算活着——旧 daemon 退出后父进程不收尸（例如从一个不理 SIGCHLD 的
/// 进程里手工起的），这里会一直等到 [`STOP_TIMEOUT`] 再报 `DaemonStuck`。launchd / systemd /
/// 交互 shell / sshd 都会立刻收尸；`tests/upgrade.rs` 起的 daemon 由测试自己另起线程 wait。
pub fn alive(pid: u32) -> bool {
    // SAFETY: 信号 0 只做存在性检查，不发任何信号。
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn terminate(pid: u32) -> Result<(), UpgradeError> {
    // SAFETY: pid 来自我们自己的 pid 文件，且刚验过活着；SIGTERM 是 daemon 的正常退出信号。
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(UpgradeError::Io {
        path: format!("kill -TERM {pid}"),
        source: err,
    })
}

fn wait_gone(pid: u32, limit: Duration) -> Result<(), UpgradeError> {
    let deadline = Instant::now() + limit;
    while alive(pid) {
        if Instant::now() >= deadline {
            return Err(UpgradeError::DaemonStuck { pid, waited: limit });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// 经 `sh` 起 `<link> serve` 并立刻返回：stdin `/dev/null`、stdout+stderr 追加到日志；有 `setsid`
/// （util-linux，Linux）就开新会话彻底脱离终端，没有（macOS 默认没有）就只放后台——macOS 上正常路径是
/// launchd，这一支是开发机与测试的兜底。子进程只经 `runtime::exec` 一个入口（ADR-001 D8 施工约束 2），
/// 而 exec 要等子进程 stdout EOF：daemon 的三个标准流在脚本里就已换掉，不会攥着 sh 的管道，
/// 所以 sh 一退 exec 就返回。`$1` 是链接、`$2` 是日志，都不经 shell 拼接。
fn spawn_detached(home: &Path, link: &Path, log: &Path) -> Result<(), UpgradeError> {
    const SCRIPT: &str = r#"cd / || exit 1
if command -v setsid >/dev/null 2>&1; then
  setsid "$1" serve </dev/null >>"$2" 2>&1 &
else
  "$1" serve </dev/null >>"$2" 2>&1 &
fi
"#;
    let argv: [&OsStr; 5] = [
        OsStr::new("sh"),
        OsStr::new("-c"),
        OsStr::new(SCRIPT),
        OsStr::new("agora-upgrade"),
        link.as_os_str(),
    ];
    let mut argv = argv.to_vec();
    argv.push(log.as_os_str());
    let out = exec(
        &argv,
        &ExecOptions {
            // 新 daemon 要看到与这次升级同一个 AGORA_HOME；用户没设环境变量时 main 解析出的默认值也传过去。
            env: vec![("AGORA_HOME".to_owned(), home.display().to_string())],
            ..ExecOptions::default()
        },
    )?;
    if out.status.success() {
        Ok(())
    } else {
        Err(UpgradeError::Restart {
            what: format!("{} serve", link.display()),
            status: out.status,
            stderr: String::from_utf8_lossy(&out.stderr_tail).trim().to_owned(),
        })
    }
}

// ---------- ⑤ 健康 ----------

/// 等新 daemon：`/api/health` 答 200 `{"status":"ok"}`，且 pid 文件里的 pid 不是旧的那个
/// （systemd / launchd 重启时旧进程可能还在答最后几个请求）。返回新 pid。
fn wait_healthy(
    home: &Path,
    addr: SocketAddr,
    old_pid: Option<u32>,
    limit: Duration,
) -> Result<u32, UpgradeError> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(pid) = read_pid(home).filter(|p| Some(*p) != old_pid) {
            if health_ok(addr) {
                return Ok(pid);
            }
        }
        if Instant::now() >= deadline {
            return Err(UpgradeError::HealthTimeout {
                addr,
                waited: limit,
                log: home.join(DAEMON_LOG).display().to_string(),
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn health_ok(addr: SocketAddr) -> bool {
    match http_get(addr, "/api/health") {
        Ok((200, body)) => serde_json::from_str::<serde_json::Value>(&body)
            .map(|v| v["status"] == "ok")
            .unwrap_or(false),
        _ => false,
    }
}

/// 最小的 HTTP/1.1 GET（`Connection: close`）：返回状态码与 body。只对本机明文监听器用。
fn http_get(addr: SocketAddr, path: &str) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = String::new();
    s.read_to_string(&mut raw)?;
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| std::io::Error::other("不是 HTTP 应答"))?;
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    Ok((status, body))
}

// ---------- pid 文件 ----------

/// `<AGORA_HOME>/agora.pid` 里的 pid（没有 / 读不出 → None）。
pub fn read_pid(home: &Path) -> Option<u32> {
    read_pid_at(&home.join(PID_FILE))
}

fn read_pid_at(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// daemon 持有的 pid 文件：`serve` 绑定成功后写，Drop（正常退出与 SIGTERM 收尾）时删。
/// 只删内容仍是自己 pid 的文件——别的实例已经写了它就不动。
pub struct PidFile {
    path: PathBuf,
    pid: u32,
}

impl PidFile {
    pub fn write(home: &Path) -> std::io::Result<PidFile> {
        let path = home.join(PID_FILE);
        let pid = std::process::id();
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        // 文件早就在（上一代没删干净）时 mode 不会被 open 重设。
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        writeln!(f, "{pid}")?;
        Ok(PidFile { path, pid })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if read_pid_at(&self.path) == Some(self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_this_binarys_schema_and_api_version() {
        let p = Probe::current();
        assert_eq!(p.schema_version, SCHEMA_VERSION);
        assert_eq!(p.api_version, API_VERSION.to_string());
        let line = serde_json::to_string(&p).unwrap();
        let back: Probe = serde_json::from_str(&line).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn args_need_from_and_accept_no_restart() {
        assert!(matches!(parse_args(&[]), Err(UpgradeError::Usage(_))));
        assert!(matches!(
            parse_args(&["--bogus"]),
            Err(UpgradeError::Usage(_))
        ));
        let o = parse_args(&["--from", "/x/agora"]).unwrap();
        assert_eq!(o.from, PathBuf::from("/x/agora"));
        assert!(o.restart);
        let o = parse_args(&["--no-restart", "--from", "/x/agora"]).unwrap();
        assert!(!o.restart);
    }

    #[test]
    fn refusals_exit_2_and_failures_exit_1() {
        assert_eq!(UpgradeError::Usage(String::new()).exit_code(), 2);
        assert_eq!(
            UpgradeError::SchemaTooOld {
                found: 5,
                supported: 1
            }
            .exit_code(),
            2
        );
        assert_eq!(
            UpgradeError::ProbeFailed {
                path: String::new(),
                reason: String::new()
            }
            .exit_code(),
            1
        );
    }

    #[test]
    fn staging_is_content_addressed_and_reused() {
        let home = tempfile::tempdir().unwrap();
        let src = home.path().join("new-agora");
        std::fs::write(&src, b"#!/bin/sh\nexit 0\n").unwrap();
        let a = stage(home.path(), &src).unwrap();
        assert!(a.created);
        let sha = sha256_hex(b"#!/bin/sh\nexit 0\n");
        assert_eq!(
            a.path,
            home.path()
                .join(VERSIONS_DIR)
                .join(&sha[..12])
                .join("agora")
        );
        assert_eq!(
            std::fs::metadata(&a.path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let b = stage(home.path(), &src).unwrap();
        assert!(!b.created, "同 sha 复用");
        assert_eq!(b.path, a.path);
    }

    #[test]
    fn user_version_is_read_without_migrating() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(current_user_version(home.path()).unwrap(), 0);
        let raw = rusqlite::Connection::open(home.path().join("agora.db")).unwrap();
        raw.pragma_update(None, "user_version", 3).unwrap();
        drop(raw);
        assert_eq!(current_user_version(home.path()).unwrap(), 3);
        let raw = rusqlite::Connection::open(home.path().join("agora.db")).unwrap();
        let v: i64 = raw
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 3, "读版本不得顺手迁移");
    }

    #[test]
    fn pid_file_is_private_and_removed_only_by_its_owner() {
        let home = tempfile::tempdir().unwrap();
        let pf = PidFile::write(home.path()).unwrap();
        assert_eq!(read_pid(home.path()), Some(std::process::id()));
        assert_eq!(
            std::fs::metadata(pf.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // 别的实例已经覆盖了它：Drop 不动。
        std::fs::write(pf.path(), "424242\n").unwrap();
        drop(pf);
        assert_eq!(read_pid(home.path()), Some(424242));
        let pf = PidFile::write(home.path()).unwrap();
        drop(pf);
        assert_eq!(read_pid(home.path()), None);
    }
}
