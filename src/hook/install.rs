//! `agora hooks install | uninstall <agent>`（ADR-002 D4；agora-dvh.1）。
//!
//! hook 装进用户自己的 agent 配置：文件与事件表由宿主的 `AgentHooks::install_spec` 给，
//! 这里只会"把一组条目拼进一份 JSON"，不认识任何 agent。规则：
//! - 命令形态 `if [ -x <AGORA_HOME>/bin/agora ]; then exec <AGORA_HOME>/bin/agora hook --host <h> --home <AGORA_HOME>; fi`：
//!   稳定路径 + 显式 `--home`；`bin/agora` 是指向当前二进制的符号链接，**每次** install 都建 / 修，
//!   配置条目"无需改动"也不例外（agora-vkt：2026-09-05 清空 ~/.agora 后三家条目都还在、链接没了，
//!   `[ -x ]` 守卫让 hook 静默失效，用户只看到 hooks_unheard）；uninstall 不碰它——别的 agent 的
//!   条目可能还在用。`exec` 让 agora 顶替 `sh -c` 那层，hook 进程的 ppid 就是 agent 本体——实测
//!   2026-09-04 Grok 1.0.13 不带 `exec` 时 ppid 是 sh，而 Grok 的环境里没有进程号变量，外部会话的
//!   存活只能靠 ppid。
//! - 幂等：命令里的 `<AGORA_HOME>/bin/agora hook` 是自己条目的标记——先删自己的再加，重复装不重复；
//!   卸载只删自己的，别人的条目与文件里其它键原样。
//! - 装前显示 diff（stderr），`--dry-run` 只看不写（链接也只报告"将建立 / 将重指"）；写文件先
//!   `.part` 再 rename。
//!
//! 配置文件的形态是三家共同的 `{"hooks": {"<Event>": [{"matcher"?, "hooks": [{"type":
//! "command", "command", "timeout"}]}]}}`（Claude settings.json、Grok agora.json、Codex
//! hooks.json，后者 2026-09-05 实测 0.152.1 照此加载）。装完宿主要用户再做一步的（Codex 的
//! `/hooks` 信任）由 `AgentHooks::install_hint` 说，这里只负责打印。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::adapter::{self, AgentHooks, HookInstall};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{0}")]
    Usage(String),
    #[error("{agent} 没有 hook 安装规格")]
    NoSpec { agent: String },
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} 不是 JSON 对象: {reason}")]
    NotAnObject { path: String, reason: String },
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// 稳定路径 `<AGORA_HOME>/bin/agora`。
pub fn bin_path(agora_home: &Path) -> PathBuf {
    agora_home.join("bin").join("agora")
}

/// daemon 启动时的自检（agora-vkt）：链接在、指向的二进制没了 → `Some((link, target))`，让 main
/// 警告一句怎么修。链接根本不存在不算——装 hooks 之前它本来就不存在；指向别处但目标存在也不算，
/// 那可能是用户有意让 hook 用另一份二进制。
pub fn dangling_bin_link(agora_home: &Path) -> Option<(PathBuf, PathBuf)> {
    let link = bin_path(agora_home);
    let target = std::fs::read_link(&link).ok()?;
    let resolved = resolve_link_target(&link, &target);
    (!resolved.exists()).then_some((link, target))
}

/// `read_link` 读出的目标按链接所在目录解析（符号链接的语义）；`join` 遇到绝对路径就是绝对路径本身。
fn resolve_link_target(link: &Path, target: &Path) -> PathBuf {
    link.parent()
        .map(|dir| dir.join(target))
        .unwrap_or_else(|| target.to_path_buf())
}

/// 两个路径是不是同一份二进制：两边都 canonicalize 得出来就比真路径，任一失败（悬空链接的目标、
/// 还不存在的路径）就退回原样比较——悬空的旧链接仍要判 Repoint，不能因为比不出来就放过。
///
/// 为什么不能原样比（agora-78f，2026-09-06 实测）：macOS 的 `current_exe()` 走
/// `_NSGetExecutablePath`，不解析符号链接，经 `<AGORA_HOME>/bin/agora` 链接跑 `hooks install`
/// 拿到的"当前二进制"就是链接本身；原样比"链接目标 == exe"永远不等，判成 Repoint，真跑会先删链接
/// 再 `symlink(link, link)`，链接指成自环，此后 hooks.json 里 `if [ -x <AGORA_HOME>/bin/agora ]`
/// 对三家 host 都为假，hook 静默失效、agora 收不到任何事件。Linux 走 /proc/self/exe 会解析，所以
/// CI 上复现不出来。tmp 在 macOS 上是 /var → /private/var，正好覆盖"两边 canonical 才相等"的分支。
fn same_binary(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `<AGORA_HOME>/bin/agora` 该怎么处置（[`Installer::bin_link_plan`]）。三态之外没有"跳过"：每次
/// install 都要走到这里，不看配置条目要不要改（agora-vkt）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPlan {
    /// 已经是指向当前二进制的符号链接。
    Ok(PathBuf),
    /// 路径上什么都没有：首装，或 AGORA_HOME 被清空重置过。
    Create(PathBuf),
    /// 路径上有东西但不对。`from` 是它现在指向的地方——旧版本 / 别处的二进制，或已经悬空；
    /// `None` 是被普通文件（或目录）占了位、根本不是符号链接。
    Repoint {
        link: PathBuf,
        from: Option<PathBuf>,
    },
}

impl LinkPlan {
    pub fn link(&self) -> &Path {
        match self {
            LinkPlan::Ok(link) | LinkPlan::Create(link) => link,
            LinkPlan::Repoint { link, .. } => link,
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, LinkPlan::Ok(_))
    }

    /// 给用户看的那一行：总含 `<link> -> <exe>`，让人看得见链接指到了哪。`applied` 是"已经落盘"
    /// （`--dry-run` 传 false，说"将…"）。
    pub fn describe(&self, exe: &Path, applied: bool) -> String {
        let link = self.link().display();
        let exe = exe.display();
        match self {
            LinkPlan::Ok(_) => format!("{link} -> {exe}（已指向）"),
            LinkPlan::Create(_) if applied => format!("{link} -> {exe}（已建立）"),
            LinkPlan::Create(_) => format!("将建立 {link} -> {exe}（--dry-run，未建）"),
            LinkPlan::Repoint { from, .. } => {
                let why = match from {
                    Some(t) if t.exists() => format!("原指向 {}", t.display()),
                    Some(t) => format!("原指向 {}，已不存在", t.display()),
                    None => "原是普通文件，不是符号链接".to_owned(),
                };
                if applied {
                    format!("{link} -> {exe}（已重指；{why}）")
                } else {
                    format!("将重指 {link} -> {exe}（{why}；--dry-run，未改）")
                }
            }
        }
    }
}

fn sh_quote(p: &Path) -> String {
    let s = p.display().to_string();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c))
    {
        s
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// 写进 agent 配置的命令（ADR-002 D4 的形态）。
pub fn command(agora_home: &Path, host: &str) -> String {
    let bin = sh_quote(&bin_path(agora_home));
    let home = sh_quote(agora_home);
    format!("if [ -x {bin} ]; then exec {bin} hook --host {host} --home {home}; fi")
}

/// 自己条目的标记：稳定路径 + `hook` 子命令。
fn is_ours(agora_home: &Path, command: &str) -> bool {
    command.contains(&format!("{} hook ", sh_quote(&bin_path(agora_home))))
}

/// 一次安装 / 卸载要做的事：改哪个文件、改前改后。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub file: PathBuf,
    pub before: Value,
    pub after: Value,
}

impl Plan {
    pub fn is_noop(&self) -> bool {
        self.before == self.after
    }

    pub fn diff(&self) -> String {
        diff(&pretty(&self.before), &pretty(&self.after))
    }
}

pub struct Installer {
    pub agora_home: PathBuf,
    pub user_home: PathBuf,
}

impl Installer {
    fn read(&self, file: &Path) -> Result<Value, InstallError> {
        if !file.exists() {
            return Ok(json!({}));
        }
        let text = std::fs::read_to_string(file).map_err(io(file))?;
        let v: Value = if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&text).map_err(|e| InstallError::NotAnObject {
                path: file.display().to_string(),
                reason: e.to_string(),
            })?
        };
        if !v.is_object() {
            return Err(InstallError::NotAnObject {
                path: file.display().to_string(),
                reason: "顶层不是对象".into(),
            });
        }
        Ok(v)
    }

    fn file_of(&self, spec: &[HookInstall], agent: &str) -> Result<PathBuf, InstallError> {
        let Some(first) = spec.first() else {
            return Err(InstallError::NoSpec {
                agent: agent.to_owned(),
            });
        };
        Ok(self.user_home.join(&first.file))
    }

    pub fn plan_install(&self, hooks: &dyn AgentHooks) -> Result<Plan, InstallError> {
        let spec = hooks.install_spec();
        let file = self.file_of(&spec, hooks.host())?;
        let before = self.read(&file)?;
        let mut after = before.clone();
        remove_ours(&mut after, &self.agora_home);
        let cmd = command(&self.agora_home, hooks.host());
        let table = hooks_table(&mut after);
        for h in &spec {
            let mut group = Map::new();
            if let Some(m) = &h.matcher {
                group.insert("matcher".into(), Value::String(m.clone()));
            }
            group.insert(
                "hooks".into(),
                json!([{ "type": "command", "command": cmd, "timeout": h.timeout.as_secs() }]),
            );
            if let Some(groups) = table
                .entry(h.event.clone())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
            {
                groups.push(Value::Object(group));
            }
        }
        Ok(Plan {
            file,
            before,
            after,
        })
    }

    pub fn plan_uninstall(&self, hooks: &dyn AgentHooks) -> Result<Plan, InstallError> {
        let spec = hooks.install_spec();
        let file = self.file_of(&spec, hooks.host())?;
        let before = self.read(&file)?;
        let mut after = before.clone();
        remove_ours(&mut after, &self.agora_home);
        Ok(Plan {
            file,
            before,
            after,
        })
    }

    /// 先 `.part` 再 rename：agent 的 file watcher 看到的永远是完整文件。
    pub fn write(&self, plan: &Plan) -> Result<(), InstallError> {
        if let Some(dir) = plan.file.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        let part = plan.file.with_extension("json.part");
        std::fs::write(&part, pretty(&plan.after) + "\n").map_err(io(&part))?;
        std::fs::rename(&part, &plan.file).map_err(io(&plan.file))
    }

    /// 只看不动：`<AGORA_HOME>/bin/agora` 现在是什么、该怎么办。`--dry-run` 靠它报告，真跑先用它
    /// 组出给用户看的那一行再 [`Self::ensure_bin_link`]。
    pub fn bin_link_plan(&self, exe: &Path) -> LinkPlan {
        let link = bin_path(&self.agora_home);
        match std::fs::read_link(&link) {
            // 目标就是当前二进制（按真路径比，[`same_binary`]），或者 exe 就是这条链接本身——经链接
            // 运行、current_exe 没解析（agora-78f）——都算已指向：链接解析到的就是正在跑的这份。
            Ok(target)
                if same_binary(&resolve_link_target(&link, &target), exe)
                    || same_binary(&link, exe) =>
            {
                LinkPlan::Ok(link)
            }
            // 悬空也走这支：read_link 读的是链接本身，目标在不在无所谓。
            Ok(target) => LinkPlan::Repoint {
                link,
                from: Some(target),
            },
            // 不是符号链接却有东西：普通文件 / 目录占位。symlink_metadata 不跟随链接。
            Err(_) if link.symlink_metadata().is_ok() => LinkPlan::Repoint { link, from: None },
            Err(_) => LinkPlan::Create(link),
        }
    }

    /// `<AGORA_HOME>/bin/agora` → 当前二进制；指错了就重指。实现是自由函数 [`ensure_bin_link`]
    /// （`agora upgrade` 也调它，agora-7ku.8），这里只是委托。
    pub fn ensure_bin_link(&self, exe: &Path) -> Result<PathBuf, InstallError> {
        ensure_bin_link(&self.agora_home, exe)
    }
}

/// `<AGORA_HOME>/bin/agora` → `exe`；指错了就重指。symlink 用 `exe` 原样（[`run`] 与 `agora upgrade`
/// 都已 canonicalize 过；测试传 tmp 路径并断言 `read_link(link) == exe`）。安装（`hooks install`）与
/// 升级（`agora upgrade`）维护的是同一条链接，所以只有这一个实现（agora-7ku.8）。
pub fn ensure_bin_link(agora_home: &Path, exe: &Path) -> Result<PathBuf, InstallError> {
    let link = bin_path(agora_home);
    // 硬守卫：exe 就是链接本身（原样或真路径相等）→ 什么都不动，直接当"已指向"。任何情况下都
    // 不能 remove + symlink 造出 link -> link：2026-09-06 开发机上 `~/.agora/bin/agora hooks
    // install <agent>` 就是这么把链接指成自环、三家 hook 全哑的（agora-78f）。放在 create_dir_all
    // 之前——exe 是链接本身时目录必然已在，不该有任何副作用。
    if same_binary(&link, exe) {
        return Ok(link);
    }
    let dir = link.parent().unwrap_or(agora_home);
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    if let Ok(target) = std::fs::read_link(&link) {
        if same_binary(&resolve_link_target(&link, &target), exe) {
            return Ok(link);
        }
    }
    if link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link).map_err(io(&link))?;
    }
    std::os::unix::fs::symlink(exe, &link).map_err(io(&link))?;
    Ok(link)
}

fn hooks_table(root: &mut Value) -> &mut Map<String, Value> {
    let obj = root.as_object_mut().expect("read() 保证顶层是对象");
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    hooks.as_object_mut().expect("刚设成对象")
}

/// 删自己的条目：组里只剩别人的留下，组空了删组，事件空了删键；`hooks` 空了也留着——
/// 那是用户文件里的键，不是我们的。
fn remove_ours(root: &mut Value, agora_home: &Path) {
    let Some(table) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    let events: Vec<String> = table.keys().cloned().collect();
    for ev in events {
        let Some(groups) = table.get_mut(&ev).and_then(Value::as_array_mut) else {
            continue;
        };
        for g in groups.iter_mut() {
            if let Some(entries) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                entries.retain(|e| {
                    !e.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| is_ours(agora_home, c))
                });
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|a| !a.is_empty())
        });
        if groups.is_empty() {
            table.remove(&ev);
        }
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// 最小的行 diff（LCS）：文件只有几十到几百行，不值得引 crate。
pub fn diff(before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = String::new();
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            out.push_str(&format!("  {}\n", a[i]));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push_str(&format!("+ {}\n", b[j]));
            j += 1;
        } else {
            out.push_str(&format!("- {}\n", a[i]));
            i += 1;
        }
    }
    out
}

fn usage() -> String {
    format!(
        "用法: agora hooks install|uninstall <{}> [--dry-run] [--home <AGORA_HOME>] [--user-home <HOME>]",
        adapter::hosts().join("|")
    )
}

pub struct Args {
    pub action: String,
    pub agent: String,
    pub dry_run: bool,
    pub agora_home: PathBuf,
    pub user_home: PathBuf,
}

pub fn parse_args(argv: &[&str]) -> Result<Args, InstallError> {
    let mut it = argv.iter();
    let action = it
        .next()
        .filter(|a| matches!(**a, "install" | "uninstall"))
        .ok_or_else(|| InstallError::Usage(usage()))?;
    let agent = it
        .next()
        .ok_or_else(|| InstallError::Usage(usage()))?
        .to_string();
    let mut dry_run = false;
    let mut agora_home = None;
    let mut user_home = None;
    while let Some(a) = it.next() {
        match *a {
            "--dry-run" => dry_run = true,
            "--home" => agora_home = it.next().map(PathBuf::from),
            "--user-home" => user_home = it.next().map(PathBuf::from),
            other => {
                return Err(InstallError::Usage(format!(
                    "未知参数 {other}\n{}",
                    usage()
                )))
            }
        }
    }
    Ok(Args {
        action: action.to_string(),
        agent,
        dry_run,
        agora_home: agora_home.unwrap_or_else(crate::local::resolve_home),
        user_home: user_home
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(".")),
    })
}

/// 入口。成功 0；用法错误 2；写不了 1。给用户看的话全在 stderr（stdout 留给将来机器可读的形态），
/// "当前二进制"是 `current_exe()` 经 canonicalize 的真路径；可测的核在 [`run_with`]。
pub fn run(argv: &[&str]) -> i32 {
    let exe = match std::env::current_exe() {
        // macOS 的 current_exe 不解析符号链接：经 <AGORA_HOME>/bin/agora 跑到这里拿到的是链接本身，
        // 不解析就会把链接重指成自环（agora-78f，2026-09-06）。canonicalize 失败（二进制在进程起来
        // 之后被删 / 搬走）退回原路径继续，[`same_binary`] 与 `ensure_bin_link` 的硬守卫兜底。
        Ok(exe) => exe.canonicalize().unwrap_or(exe),
        Err(err) => {
            eprintln!("agora hooks: current_exe: {err}");
            return 1;
        }
    };
    match run_with(argv, &exe, &mut std::io::stderr()) {
        Ok(()) => 0,
        Err(InstallError::Usage(msg)) => {
            eprintln!("{msg}");
            2
        }
        Err(err) => {
            eprintln!("agora hooks: {err}");
            1
        }
    }
}

/// 一行输出。`out` 在生产里就是 stderr，写不进去（EPIPE）按 IO 错误报而不是 panic。
fn say(out: &mut dyn Write, line: impl AsRef<str>) -> Result<(), InstallError> {
    writeln!(out, "{}", line.as_ref()).map_err(io(Path::new("stderr")))
}

/// `agora hooks <action> <agent> …` 的全部逻辑，`exe` 是"当前二进制"（[`run`] 传 `current_exe()`，
/// 测试传 tmp 里的假文件），给用户看的话全写进 `out`。
///
/// install 的顺序固定是先链接、再配置：条目里的 `[ -x bin/agora ]` 守卫要在条目生效的那一刻就
/// 通得过。链接那一步不看配置是否 noop（agora-vkt，2026-09-05 实测：清空 ~/.agora 之后三家条目
/// 都还在、链接没了，旧实现只说"无需改动"就返回，hook 被守卫静默跳过、只剩 hooks_unheard）。
/// `--dry-run` 两样都只报告不落盘；uninstall 不碰链接——别的 agent 的条目可能还在用。
pub fn run_with(argv: &[&str], exe: &Path, out: &mut dyn Write) -> Result<(), InstallError> {
    let args = parse_args(argv)?;
    let hooks = adapter::for_host(&args.agent).ok_or_else(|| {
        InstallError::Usage(format!("{} 没有 hook 可装\n{}", args.agent, usage()))
    })?;
    let installer = Installer {
        agora_home: args.agora_home.clone(),
        user_home: args.user_home.clone(),
    };
    let install = args.action == "install";
    let plan = if install {
        installer.plan_install(hooks)?
    } else {
        installer.plan_uninstall(hooks)?
    };
    if install {
        let link = installer.bin_link_plan(exe);
        if !args.dry_run && !link.is_ok() {
            installer.ensure_bin_link(exe)?;
        }
        say(out, link.describe(exe, !args.dry_run))?;
    }
    if plan.is_noop() {
        say(out, format!("{} 无需改动", plan.file.display()))?;
    } else {
        say(out, format!("--- {}", plan.file.display()))?;
        out.write_all(plan.diff().as_bytes())
            .map_err(io(Path::new("stderr")))?;
        if args.dry_run {
            return say(out, "(--dry-run，未写入)");
        }
        installer.write(&plan)?;
        say(out, format!("已写入 {}", plan.file.display()))?;
    }
    if install {
        if let Some(hint) = hooks.install_hint() {
            say(out, hint)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_added_and_removed_lines() {
        let d = diff("a\nb\nc", "a\nc\nd");
        assert_eq!(d, "  a\n- b\n  c\n+ d\n");
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let c = command(Path::new("/Users/x y/.agora"), "h");
        assert!(c.starts_with("if [ -x '/Users/x y/.agora/bin/agora' ]; then"));
        assert!(is_ours(Path::new("/Users/x y/.agora"), &c));
        assert!(!is_ours(Path::new("/Users/z/.agora"), &c));
    }

    #[test]
    fn bin_link_plan_distinguishes_ok_create_and_repoint() {
        let tmp = tempfile::tempdir().unwrap();
        let inst = Installer {
            agora_home: tmp.path().join("agora"),
            user_home: tmp.path().join("home"),
        };
        let exe = tmp.path().join("agora-current");
        std::fs::write(&exe, "").unwrap();
        let link = bin_path(&inst.agora_home);
        // 什么都没有 → Create；只看不动。
        assert_eq!(inst.bin_link_plan(&exe), LinkPlan::Create(link.clone()));
        assert!(!link.parent().unwrap().exists());
        assert!(inst
            .bin_link_plan(&exe)
            .describe(&exe, false)
            .starts_with("将建立 "));
        // 指对了 → Ok。
        inst.ensure_bin_link(&exe).unwrap();
        assert_eq!(inst.bin_link_plan(&exe), LinkPlan::Ok(link.clone()));
        // 指向别处（目标在） / 悬空（目标不在） / 普通文件占位 → Repoint，理由各不同。
        let old = tmp.path().join("agora-old");
        std::fs::write(&old, "").unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&old, &link).unwrap();
        let plan = inst.bin_link_plan(&exe);
        assert_eq!(
            plan,
            LinkPlan::Repoint {
                link: link.clone(),
                from: Some(old.clone())
            }
        );
        assert!(plan.describe(&exe, true).contains("原指向"));
        assert!(!plan.describe(&exe, true).contains("已不存在"));
        std::fs::remove_file(&old).unwrap();
        assert!(inst
            .bin_link_plan(&exe)
            .describe(&exe, true)
            .contains("已不存在"));
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "占位").unwrap();
        let plan = inst.bin_link_plan(&exe);
        assert_eq!(
            plan,
            LinkPlan::Repoint {
                link: link.clone(),
                from: None
            }
        );
        assert!(plan.describe(&exe, false).contains("普通文件"));
        // 三种都能修回来。
        inst.ensure_bin_link(&exe).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    }

    #[test]
    fn bin_link_plan_treats_the_link_itself_as_already_pointing() {
        // agora-78f（2026-09-06 开发机实测）：macOS 的 current_exe 不解析符号链接，经
        // <AGORA_HOME>/bin/agora 跑 install 时 exe 就是链接本身；旧实现原样比"目标 == exe"判成
        // Repoint，真跑 remove + symlink(link, link) 造出自环，三家 hook 的 `[ -x ]` 守卫全假。
        // tmp 在 macOS 上是 /var → /private/var：read_link 读出的 X 与 canonicalize(link) 不同串，
        // 只有两边都 canonical 才相等，正好覆盖 same_binary 的那条分支。
        let tmp = tempfile::tempdir().unwrap();
        let inst = Installer {
            agora_home: tmp.path().join("agora"),
            user_home: tmp.path().join("home"),
        };
        let x = tmp.path().join("agora-current");
        std::fs::write(&x, "").unwrap();
        let link = bin_path(&inst.agora_home);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&x, &link).unwrap();

        // 以链接自己的路径当 exe：计划是"已指向"，不是"将重指 link -> link"。
        let plan = inst.bin_link_plan(&link);
        assert_eq!(plan, LinkPlan::Ok(link.clone()));
        assert!(plan.describe(&link, false).contains("已指向"));
        // 真跑也不动文件系统：链接仍指向 X，且不是自环（metadata 跟随链接，自环会 ELOOP）。
        assert_eq!(inst.ensure_bin_link(&link).unwrap(), link);
        assert_eq!(std::fs::read_link(&link).unwrap(), x);
        assert!(std::fs::metadata(&link).is_ok());
        // 换一个真二进制仍照常重指：硬守卫只挡"exe 就是链接本身"。
        let y = tmp.path().join("agora-next");
        std::fs::write(&y, "").unwrap();
        assert!(matches!(inst.bin_link_plan(&y), LinkPlan::Repoint { .. }));
        inst.ensure_bin_link(&y).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), y);
        // 悬空的旧链接照旧判 Repoint：目标 canonicalize 不了，退回原样比较，不能因此放过。
        std::fs::remove_file(&y).unwrap();
        assert!(matches!(
            inst.bin_link_plan(&x),
            LinkPlan::Repoint { from: Some(_), .. }
        ));
    }

    #[test]
    fn dangling_bin_link_only_flags_a_link_whose_target_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agora");
        // 没装过 hooks：链接不存在，不告。
        assert_eq!(dangling_bin_link(&home), None);
        let exe = tmp.path().join("agora-current");
        std::fs::write(&exe, "").unwrap();
        let inst = Installer {
            agora_home: home.clone(),
            user_home: tmp.path().join("h"),
        };
        let link = inst.ensure_bin_link(&exe).unwrap();
        assert_eq!(dangling_bin_link(&home), None);
        // 二进制搬了家 / 被删：告。
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(dangling_bin_link(&home), Some((link.clone(), exe)));
        // 相对目标按链接所在目录解析。
        std::fs::remove_file(&link).unwrap();
        std::fs::write(home.join("bin/real"), "").unwrap();
        std::os::unix::fs::symlink("real", &link).unwrap();
        assert_eq!(dangling_bin_link(&home), None);
    }
}
