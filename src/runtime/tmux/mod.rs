//! tmux 运行时（ADR-001 D2 / D3 / D6 / D7）。**唯一知道 tmux 的地方。**
//!
//! - 专用 socket `-L <socket>`，server 级选项由 agora 生成的 conf 给（`-f`，每次都带）。
//! - `adopt_sockets` 只读：只 list / attach / capture，写操作一律 [`RuntimeError::ReadOnly`]。
//! - 没有"杀整个 server"的方法；remove 只碰 `pane_dead=1` 的会话。
//! - terminate 不经 tmux：`kill(-pane_pid)` 整组杀（pgid == pane_pid，实测 2026-09-02）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use super::exec::{exec, ExecOptions, Output};
use super::{
    AttachSpec, Exit, LaunchSpec, Runtime, RuntimeError, RuntimeRef, RuntimeSession, Size,
    TAIL_BUFFER_MAX,
};

const KIND: &str = "tmux";
/// 字段分隔符。必须是可打印字符：tmux 3.2a 会把格式输出里的控制字符（`\x1f`、`\t`）
/// 替换成 `_`，整行就切不开了（ubuntu-22.04 CI 实测 2026-09-03；3.7c 不替换，所以本机没发现）。
/// 自由文本字段（cwd、title）放在最后用 `splitn` 切，title 里出现它也无妨。
const SEP: &str = "|#|";
/// `new-session -e`（3.2）/ `window-size latest`（3.1）/ `respawn-pane -e`（3.0）的下限。
pub const MIN_VERSION: (u32, u32) = (3, 2);
/// `pane_dead_signal` / `pane_dead_time` 从 3.3 起才有（ubuntu-22.04 的 3.2a 实测两者皆无，
/// 2026-09-03）。低于它：信号退出报 `Signal("unknown")`，`exited_at` 取首次观测到死亡的时间。
pub const DEAD_DETAIL_VERSION: (u32, u32) = (3, 3);
/// 3.3 以前 pane 死后 `pane_dead_status` 为空有两种含义：退出码尚未收集（pty EOF 先于 waitpid，
/// ubuntu-22.04 容器紧密轮询 30 次撞上 2 次，CI run 33705306776 / 33704139125，2026-09-03），
/// 或者信号退出（3.2a 没有 `pane_dead_signal`）。只能按时间分：首次观测到死亡后这么久之内
/// `exit` 保持 `None`（上层显示"退出码尚未收集"，与 3.3+ 同一形态），之后才报 `Signal("unknown")`。
/// 否则正常 `exit 7` 会先闪一次 FAILED "signal unknown"（agora-5nu）。
pub const UNKNOWN_SIGNAL_GRACE: Duration = Duration::from_secs(2);
/// Restart 一个活着的会话时先按 Kill 同一条路（进程组 SIGTERM → SIGKILL）杀干净再重生，
/// 这是 SIGTERM 之后等多久升级 SIGKILL。
const RESPAWN_KILL_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct TmuxConfig {
    pub bin: String,
    pub socket: String,
    pub adopt_sockets: Vec<String>,
    /// agora 生成的 server 级 conf 的落点（`AGORA_HOME/tmux.conf`）。
    pub conf_path: PathBuf,
    pub history_limit: u32,
    pub exec_timeout: Duration,
    /// 探测到的用户 PATH（D7）；`None` 就用 daemon 自己的。
    pub path: Option<String>,
    /// 记录每次 tmux 调用的 argv，供测试断言（`take_recorded`）。
    pub record: bool,
    /// 3.3 以前"死了但 status 为空"多久之后才当信号退出（见 [`UNKNOWN_SIGNAL_GRACE`]）；测试可缩短。
    pub unknown_signal_grace: Duration,
}

/// `config.yaml` 里 `runtime.tmux` 段的形态（docs/spec/config.md）；core 层只把它当不透明
/// 子段传进来（ADR-001 D2），在这里才知道键名。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TmuxSection {
    pub socket: String,
    pub adopt_sockets: Vec<String>,
    pub prefix: String,
    pub history_limit: u32,
    pub exec_timeout: String,
    pub min_version: String,
}

impl Default for TmuxSection {
    fn default() -> Self {
        TmuxSection {
            socket: "agora".into(),
            adopt_sockets: vec!["default".into()],
            prefix: "ag-".into(),
            history_limit: 10_000,
            exec_timeout: "5s".into(),
            min_version: format!("{}.{}", MIN_VERSION.0, MIN_VERSION.1),
        }
    }
}

impl TmuxConfig {
    /// 用配置段覆盖默认值；`exec_timeout` 的时长语法由 config 层统一解析后传进来。
    pub fn from_section(section: &TmuxSection, exec_timeout: Duration) -> Self {
        TmuxConfig {
            socket: section.socket.clone(),
            adopt_sockets: section.adopt_sockets.clone(),
            history_limit: section.history_limit,
            exec_timeout,
            ..Default::default()
        }
    }
}

impl Default for TmuxConfig {
    fn default() -> Self {
        TmuxConfig {
            bin: "tmux".into(),
            socket: "agora".into(),
            adopt_sockets: vec!["default".into()],
            conf_path: PathBuf::from("tmux.conf"),
            history_limit: 10_000,
            exec_timeout: super::exec::DEFAULT_TIMEOUT,
            path: None,
            record: false,
            unknown_signal_grace: UNKNOWN_SIGNAL_GRACE,
        }
    }
}

pub struct TmuxRuntime {
    cfg: TmuxConfig,
    recorded: Mutex<Vec<Vec<String>>>,
    version: OnceLock<Option<(u32, u32)>>,
    /// 老 tmux 没有 pane_dead_time：记下每个会话第一次被看到已死的时刻，保持各 tick 稳定。
    first_dead_seen: Mutex<HashMap<String, SystemTime>>,
}

/// 解析后的 ref；只在本模块存在。
struct ParsedRef<'a> {
    socket: &'a str,
    session: &'a str,
}

impl TmuxRuntime {
    /// 写出 conf（幂等）；不起 server，server 在第一次 create 时随 `-f` 起来。
    pub fn new(cfg: TmuxConfig) -> Result<Self, RuntimeError> {
        let conf = render_conf(cfg.history_limit);
        if std::fs::read_to_string(&cfg.conf_path).ok().as_deref() != Some(conf.as_str()) {
            if let Some(dir) = cfg.conf_path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| RuntimeError::Failed {
                    stderr_tail: format!("创建 {} 失败: {e}", dir.display()),
                })?;
            }
            std::fs::write(&cfg.conf_path, conf).map_err(|e| RuntimeError::Failed {
                stderr_tail: format!("写 {} 失败: {e}", cfg.conf_path.display()),
            })?;
        }
        Ok(TmuxRuntime {
            cfg,
            recorded: Mutex::new(Vec::new()),
            version: OnceLock::new(),
            first_dead_seen: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &TmuxConfig {
        &self.cfg
    }

    /// 取走录制的调用（每项是完整 argv，不含 bin）。
    pub fn take_recorded(&self) -> Vec<Vec<String>> {
        std::mem::take(&mut *lock(&self.recorded))
    }

    /// `tmux -V`（唯一解析的文档化输出）→ (major, minor)。结果缓存：版本在 daemon
    /// 生命周期内不变，list 的解析分支也要用它而不能每 tick 多起一个进程。
    pub fn version(&self) -> Result<(u32, u32), RuntimeError> {
        if let Some(v) = self.version.get() {
            return v.ok_or_else(|| RuntimeError::VersionMismatch {
                reason: "无法解析 `tmux -V` 输出".into(),
            });
        }
        let out = self.run(None, &["-V"])?;
        let text = String::from_utf8_lossy(&out.stdout);
        let parsed = parse_version(&text);
        let _ = self.version.set(parsed);
        parsed.ok_or_else(|| RuntimeError::VersionMismatch {
            reason: format!("无法解析 `tmux -V` 输出: {}", text.trim()),
        })
    }

    fn dead_detail_supported(&self) -> bool {
        self.version()
            .map(|v| v >= DEAD_DETAIL_VERSION)
            .unwrap_or(false)
    }

    /// 低于 [`MIN_VERSION`] 报 VersionMismatch；health 的呈现归 agora-xqa.4。
    pub fn check_version(&self) -> Result<(u32, u32), RuntimeError> {
        let v = self.version()?;
        if v < MIN_VERSION {
            return Err(RuntimeError::VersionMismatch {
                reason: format!(
                    "tmux {}.{} 低于下限 {}.{}",
                    v.0, v.1, MIN_VERSION.0, MIN_VERSION.1
                ),
            });
        }
        Ok(v)
    }

    /// 显式拉起专用 server（daemon 启动时调用；create 也会顺带起）。
    pub fn start_server(&self) -> Result<(), RuntimeError> {
        let conf = self.conf_arg();
        self.run(
            Some(&self.cfg.socket),
            &["-f", conf.as_str(), "start-server"],
        )?;
        Ok(())
    }

    pub fn make_ref(&self, socket: &str, session: &str) -> RuntimeRef {
        RuntimeRef(format!("{KIND}:{socket}:{session}"))
    }

    fn parse_ref<'a>(&self, r: &'a RuntimeRef) -> Result<ParsedRef<'a>, RuntimeError> {
        let rest =
            r.0.strip_prefix(KIND)
                .and_then(|s| s.strip_prefix(':'))
                .ok_or_else(|| RuntimeError::NotFound(r.clone()))?;
        // 会话名不能含 ':'（tmux 自己的限制），所以第一个 ':' 就是边界。
        let (socket, session) = rest
            .split_once(':')
            .filter(|(s, n)| !s.is_empty() && !n.is_empty())
            .ok_or_else(|| RuntimeError::NotFound(r.clone()))?;
        Ok(ParsedRef { socket, session })
    }

    fn is_managed(&self, socket: &str) -> bool {
        socket == self.cfg.socket
    }

    fn conf_arg(&self) -> String {
        self.cfg.conf_path.to_string_lossy().into_owned()
    }

    fn base_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Some(p) = &self.cfg.path {
            env.push(("PATH".into(), p.clone()));
        }
        env
    }

    /// 全部 tmux 子进程都从这里走：一处加 `-L`、一处录制、一处映射错误。
    fn run(&self, socket: Option<&str>, args: &[&str]) -> Result<Output, RuntimeError> {
        let mut argv: Vec<String> = Vec::with_capacity(args.len() + 3);
        argv.push(self.cfg.bin.clone());
        if let Some(s) = socket {
            argv.push("-L".into());
            argv.push(s.into());
        }
        argv.extend(args.iter().map(|s| s.to_string()));
        if self.cfg.record {
            lock(&self.recorded).push(argv[1..].to_vec());
        }
        let opts = ExecOptions {
            timeout: Some(self.cfg.exec_timeout),
            env: self.base_env(),
            ..Default::default()
        };
        let out = exec(&argv, &opts)?;
        Ok(out)
    }

    fn run_ok(&self, socket: Option<&str>, args: &[&str]) -> Result<Output, RuntimeError> {
        let out = self.run(socket, args)?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(RuntimeError::Failed {
                stderr_tail: String::from_utf8_lossy(&out.stderr_tail).trim().to_owned(),
            })
        }
    }

    /// "server 在不在"用结构事实判断：能 connect 上 unix socket 才算在。
    /// 只看文件存在不够——server 退出与 unlink 之间有窗口（remove 最后一个会话时实测
    /// 2026-09-03 撞上过）；不匹配 stderr 文本（ADR-002 规则 10）。
    fn server_running(&self, socket: &str) -> bool {
        std::os::unix::net::UnixStream::connect(socket_path(socket)).is_ok()
    }

    fn list_socket(&self, socket: &str) -> Result<Vec<RuntimeSession>, RuntimeError> {
        if !self.server_running(socket) {
            return Ok(Vec::new());
        }
        let format = [
            "#{session_name}",
            "#{pane_pid}",
            "#{pane_dead}",
            "#{pane_dead_status}",
            "#{pane_dead_signal}",
            "#{pane_dead_time}",
            "#{session_attached}",
            "#{pane_width}",
            "#{pane_height}",
            "#{pane_current_path}",
            "#{pane_title}",
        ]
        .join(SEP);
        let out = self.run(Some(socket), &["list-panes", "-a", "-F", &format])?;
        if !out.status.success() {
            if !self.server_running(socket) {
                // 调用期间 server 正好退出了：等价于没有会话。
                return Ok(Vec::new());
            }
            // server 应答但拒绝：多半是 client/server 协议不匹配（D7），
            // 不杀 server、不退出，交给 health 呈现。
            return Err(RuntimeError::ServerUnavailable {
                reason: String::from_utf8_lossy(&out.stderr_tail).trim().to_owned(),
            });
        }
        let managed = self.is_managed(socket);
        let text = String::from_utf8_lossy(&out.stdout);
        let mut sessions: Vec<RuntimeSession> = Vec::new();
        let mut detail = None;
        for line in text.lines() {
            let Some(mut s) = parse_pane_line(line, socket, managed) else {
                continue;
            };
            // 一个会话可能有多个 pane（采纳的用户会话）：只取第一个。
            if sessions.iter().any(|x| x.name == s.name) {
                continue;
            }
            if !s.alive {
                // 只在真有死 pane 时才问版本，避免无谓的子进程。
                let detail = *detail.get_or_insert_with(|| self.dead_detail_supported());
                self.fill_dead_fallbacks(&mut s, detail);
            } else {
                lock(&self.first_dead_seen).remove(&s.r#ref.0);
            }
            sessions.push(s);
        }
        Ok(sessions)
    }

    /// 3.3 以前没有 pane_dead_signal / pane_dead_time：退出时刻用首次观测替代；"死了但 status 为空"
    /// 在首次观测后 [`UNKNOWN_SIGNAL_GRACE`] 内保持 None（退出码可能只是还没收集），之后才报
    /// unknown 信号。3.3+ 上"死了但两字段皆空"就是退出码尚未收集，一直保持 None。
    fn fill_dead_fallbacks(&self, s: &mut RuntimeSession, detail_supported: bool) {
        let first_seen = *lock(&self.first_dead_seen)
            .entry(s.r#ref.0.clone())
            .or_insert_with(SystemTime::now);
        if s.exited_at.is_none() {
            s.exited_at = Some(first_seen);
        }
        if s.exit.is_none() && !detail_supported {
            let dead_for = SystemTime::now()
                .duration_since(first_seen)
                .unwrap_or_default();
            if dead_for >= self.cfg.unknown_signal_grace {
                s.exit = Some(Exit::Signal("unknown".into()));
            }
        }
    }

    fn require_managed(&self, r: &RuntimeRef) -> Result<String, RuntimeError> {
        let p = self.parse_ref(r)?;
        if !self.is_managed(p.socket) {
            return Err(RuntimeError::ReadOnly(r.clone()));
        }
        Ok(p.session.to_owned())
    }

    fn env_args(&self, extra: &[(String, String)]) -> Vec<String> {
        let mut args = Vec::new();
        let mut push = |k: &str, v: &str| {
            args.push("-e".to_string());
            args.push(format!("{k}={v}"));
        };
        if let Some((k, v)) = super::env_probe::locale_injection() {
            if !extra.iter().any(|(ek, _)| ek == &k) {
                push(&k, &v);
            }
        }
        for (k, v) in extra {
            push(k, v);
        }
        args
    }
}

impl Runtime for TmuxRuntime {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn create(&self, spec: &LaunchSpec) -> Result<RuntimeRef, RuntimeError> {
        let conf = self.conf_arg();
        let target = format!("={}:", spec.name);
        let cwd = spec.cwd.to_string_lossy().into_owned();
        let cols = spec.size.cols.to_string();
        let rows = spec.size.rows.to_string();
        let env_args = self.env_args(&spec.env);

        // 一次调用：new-session … ; set-option remain-on-exit —— 消除秒退竞态（保住 exit 7）。
        let mut args: Vec<&str> = vec![
            "-f",
            &conf,
            "new-session",
            "-d",
            "-s",
            &spec.name,
            "-c",
            &cwd,
            "-x",
            &cols,
            "-y",
            &rows,
        ];
        args.extend(env_args.iter().map(String::as_str));
        args.extend([
            spec.command.as_str(),
            ";",
            "set-option",
            "-t",
            &target,
            "remain-on-exit",
            "on",
        ]);
        self.run_ok(Some(&self.cfg.socket), &args)?;
        Ok(self.make_ref(&self.cfg.socket, &spec.name))
    }

    fn list(&self) -> Result<Vec<RuntimeSession>, RuntimeError> {
        let mut all = self.list_socket(&self.cfg.socket)?;
        for s in &self.cfg.adopt_sockets {
            if s == &self.cfg.socket {
                continue;
            }
            match self.list_socket(s) {
                Ok(v) => all.extend(v),
                // 用户 socket 出问题不该拖垮 agora 自己的会话列表。
                Err(err) => {
                    tracing::warn!(component = "runtime", socket = %s, %err, "扫描采纳 socket 失败")
                }
            }
        }
        Ok(all)
    }

    fn inspect(&self, r: &RuntimeRef) -> Result<RuntimeSession, RuntimeError> {
        let p = self.parse_ref(r)?;
        self.list_socket(p.socket)?
            .into_iter()
            .find(|s| s.name == p.session)
            .ok_or_else(|| RuntimeError::NotFound(r.clone()))
    }

    fn attach(&self, r: &RuntimeRef, _size: Size) -> Result<AttachSpec, RuntimeError> {
        let p = self.parse_ref(r)?;
        // -u 强制 UTF-8；`=` 精确匹配；不带 -d，允许多客户端同看（§6.8）。
        let argv = vec![
            self.cfg.bin.clone(),
            "-u".into(),
            "-L".into(),
            p.socket.to_owned(),
            "attach-session".into(),
            "-t".into(),
            format!("={}", p.session),
        ];
        if self.cfg.record {
            lock(&self.recorded).push(argv[1..].to_vec());
        }
        Ok(AttachSpec {
            argv,
            env: self.base_env(),
        })
    }

    fn capture_tail(&self, r: &RuntimeRef, lines: u32) -> Result<Vec<u8>, RuntimeError> {
        let p = self.parse_ref(r)?;
        let start = format!("-{lines}");
        let target = format!("={}:", p.session);
        let out = self.run(
            Some(p.socket),
            &["capture-pane", "-p", "-J", "-S", &start, "-t", &target],
        )?;
        if !out.status.success() {
            return Err(RuntimeError::NotFound(r.clone()));
        }
        let mut data = out.stdout;
        if data.len() > TAIL_BUFFER_MAX {
            let cut = data.len() - TAIL_BUFFER_MAX;
            data.drain(..cut);
        }
        Ok(data)
    }

    fn terminate(&self, r: &RuntimeRef, grace: Duration) -> Result<(), RuntimeError> {
        self.require_managed(r)?;
        let s = self.inspect(r)?;
        if !s.alive {
            return Ok(());
        }
        let Some(pid) = s.pid else {
            return Ok(());
        };
        signal_group(pid, libc::SIGTERM);
        if self.wait_dead(r, grace) {
            return Ok(());
        }
        signal_group(pid, libc::SIGKILL);
        if self.wait_dead(r, Duration::from_secs(2)) {
            Ok(())
        } else {
            Err(RuntimeError::StillAlive(r.clone()))
        }
    }

    fn respawn(&self, r: &RuntimeRef, spec: &LaunchSpec) -> Result<(), RuntimeError> {
        let session = self.require_managed(r)?;
        let target = format!("={session}:");
        // 活着先按 Kill 同一条路杀干净，不靠 respawn-pane -k 的 SIGHUP：下面的缩窗会先发
        // SIGWINCH，活着的 TUI 收到后按 1 行重画会把要保的内容冲掉。
        match self.inspect(r) {
            Ok(s) if s.alive => self.terminate(r, RESPAWN_KILL_GRACE)?,
            Ok(_) => {}
            Err(e) => return Err(e),
        }
        // respawn-pane 会 screen_reinit 清掉可见屏，只留已滚入 history 的行（agora-6bo）。
        // tmux 自己在 pane 死时打印 "Pane is dead" 前换一次行、把顶行推进 history，但 3.2a
        // 信号退出不打印这一行，且 pty EOF 到 status 收到之间 respawn 也赶不上它
        // （ubuntu-22.04 实测 2026-09-03：pane_dead=1 后立刻 respawn，6 次丢 2 次；macOS 3.7c
        // 从没丢过只是时序不同）。所以自己动手：缩到 1 行，tmux 把光标以上的行推进 history；
        // respawn 后放大回原高度，它们又被拉回可见屏（3.2a / 3.4 / 3.7c 实测 2026-09-03）。
        // 光标以下的行会被 tmux 丢掉（screen_resize_y 先删光标下方），比整屏丢好。
        // resize-window 会把该窗口的 window-size 切成 manual，完事 -u 恢复继承全局的 latest。
        let sock = Some(self.cfg.socket.as_str());
        let height = self
            .run(
                sock,
                &["display-message", "-p", "-t", &target, "#{window_height}"],
            )
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|h| h.parse::<u32>().is_ok_and(|h| h > 1));
        if height.is_some() {
            let _ = self.run(sock, &["resize-window", "-t", &target, "-y", "1"]);
        }
        let cwd = spec.cwd.to_string_lossy().into_owned();
        let env_args = self.env_args(&spec.env);
        let mut args: Vec<&str> = vec!["respawn-pane", "-k", "-t", &target, "-c", &cwd];
        args.extend(env_args.iter().map(String::as_str));
        args.push(&spec.command);
        let respawned = self.run_ok(sock, &args);
        if let Some(h) = &height {
            let _ = self.run(sock, &["resize-window", "-t", &target, "-y", h]);
            let _ = self.run(
                sock,
                &["set-option", "-w", "-u", "-t", &target, "window-size"],
            );
        }
        match respawned {
            Ok(_) => Ok(()),
            Err(RuntimeError::Failed { .. }) if self.inspect(r).is_err() => {
                Err(RuntimeError::NotFound(r.clone()))
            }
            Err(e) => Err(e),
        }
    }

    fn remove(&self, r: &RuntimeRef) -> Result<(), RuntimeError> {
        let session = self.require_managed(r)?;
        let s = self.inspect(r)?;
        if s.alive {
            return Err(RuntimeError::StillAlive(r.clone()));
        }
        let target = format!("={session}:");
        self.run_ok(Some(&self.cfg.socket), &["kill-session", "-t", &target])?;
        lock(&self.first_dead_seen).remove(&r.0);
        Ok(())
    }
}

impl TmuxRuntime {
    fn wait_dead(&self, r: &RuntimeRef, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            match self.inspect(r) {
                Ok(s) if !s.alive => return true,
                Err(RuntimeError::NotFound(_)) => return true,
                _ => {}
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn signal_group(pid: u32, sig: libc::c_int) {
    // pgid == pane_pid：tmux 给每个 pane 起新进程组。负 pid 即整组。
    // SAFETY: kill(2) 对任意 pid 都是安全调用，失败只返回 -1。
    unsafe {
        libc::kill(-(pid as libc::pid_t), sig);
    }
}

pub fn socket_path(socket: &str) -> PathBuf {
    let dir = std::env::var_os("TMUX_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    // SAFETY: getuid 无副作用。
    let uid = unsafe { libc::getuid() };
    dir.join(format!("tmux-{uid}")).join(socket)
}

pub fn render_conf(history_limit: u32) -> String {
    format!(
        "# 由 agora 生成，不要手改（ADR-001 D3）。\n\
         set -g history-limit {history_limit}\n\
         set -g remain-on-exit on\n\
         set -g status off\n\
         set -g window-size latest\n\
         set -g destroy-unattached off\n\
         set -g exit-unattached off\n"
    )
}

pub fn parse_version(text: &str) -> Option<(u32, u32)> {
    // `tmux 3.7c` / `tmux 3.2a` / `tmux next-3.5`
    let token = text.split_whitespace().nth(1)?;
    let token = token.strip_prefix("next-").unwrap_or(token);
    let mut parts = token.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor_raw = parts.next()?;
    let minor: u32 = minor_raw
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    Some((major, minor))
}

fn parse_pane_line(line: &str, socket: &str, managed: bool) -> Option<RuntimeSession> {
    // 11 段：最后一段 title 吞掉剩余全部，分隔符出现在 title 里也不会错位。
    let f: Vec<&str> = line.splitn(11, SEP).collect();
    if f.len() < 11 {
        return None;
    }
    let name = f[0];
    let pid = f[1].parse::<u32>().ok();
    let dead = f[2] == "1";
    // pane_dead=1 到退出状态被 tmux 收集之间有窗口（3.4 实测），此时 exit 为 None：
    // "已死、退出码未到"是数据，交给上层等下一 tick。
    let exit = if dead {
        match (f[3].parse::<i32>().ok(), f[4]) {
            (_, sig) if !sig.is_empty() && sig != "0" => Some(Exit::Signal(sig.to_owned())),
            (Some(code), _) => Some(Exit::Code(code)),
            _ => None,
        }
    } else {
        None
    };
    let exited_at = f[5]
        .parse::<u64>()
        .ok()
        .filter(|t| dead && *t > 0)
        .map(|t| SystemTime::UNIX_EPOCH + Duration::from_secs(t));
    Some(RuntimeSession {
        r#ref: RuntimeRef(format!("{KIND}:{socket}:{name}")),
        name: name.to_owned(),
        pid,
        alive: !dead,
        exit,
        exited_at,
        attached: f[6] != "0",
        size: Size {
            cols: f[7].parse().unwrap_or(0),
            rows: f[8].parse().unwrap_or(0),
        },
        cwd: PathBuf::from(f[9]),
        title: f[10].to_owned(),
        managed,
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl std::fmt::Debug for TmuxRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TmuxRuntime")
            .field("socket", &self.cfg.socket)
            .finish()
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn version_parses_brew_and_apt_forms() {
        assert_eq!(parse_version("tmux 3.7c\n"), Some((3, 7)));
        assert_eq!(parse_version("tmux 3.2a"), Some((3, 2)));
        assert_eq!(parse_version("tmux next-3.5"), Some((3, 5)));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn pane_line_dead_with_code() {
        let line = [
            "ag-1",
            "123",
            "1",
            "7",
            "",
            "1700000000",
            "0",
            "160",
            "48",
            "/x",
            "t",
        ]
        .join(SEP);
        let s = parse_pane_line(&line, "agora", true).unwrap();
        assert!(!s.alive);
        assert_eq!(s.exit, Some(Exit::Code(7)));
        assert_eq!(s.r#ref.0, "tmux:agora:ag-1");
    }

    #[test]
    fn pane_line_dead_by_signal() {
        let line = [
            "s", "1", "1", "0", "TERM", "0", "1", "80", "24", "/", "a|#|b",
        ]
        .join(SEP);
        let s = parse_pane_line(&line, "default", false).unwrap();
        assert_eq!(s.exit, Some(Exit::Signal("TERM".into())));
        assert!(s.attached);
        assert!(!s.managed);
        assert_eq!(s.title, "a|#|b");
    }

    fn rt() -> TmuxRuntime {
        let dir = std::env::temp_dir().join(format!("agora-unit-{}", std::process::id()));
        TmuxRuntime::new(TmuxConfig {
            conf_path: dir.join("tmux.conf"),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn ref_roundtrip_and_rejects_garbage() {
        let rt = rt();
        let r = rt.make_ref("agora", "ag-x");
        let p = rt.parse_ref(&r).unwrap();
        assert_eq!((p.socket, p.session), ("agora", "ag-x"));
        assert!(matches!(
            rt.parse_ref(&RuntimeRef("native:1".into())),
            Err(RuntimeError::NotFound(_))
        ));
        assert!(matches!(
            rt.parse_ref(&RuntimeRef("tmux:agora:".into())),
            Err(RuntimeError::NotFound(_))
        ));
    }

    #[test]
    fn foreign_ref_is_read_only_before_any_subprocess() {
        let rt = rt();
        let r = rt.make_ref("default", "mywork");
        assert!(matches!(
            rt.require_managed(&r),
            Err(RuntimeError::ReadOnly(_))
        ));
    }
}
