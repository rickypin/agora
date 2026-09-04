//! `agora hooks install | uninstall <agent>`（ADR-002 D4；agora-dvh.1）。
//!
//! hook 装进用户自己的 agent 配置：文件与事件表由宿主的 `AgentHooks::install_spec` 给，
//! 这里只会"把一组条目拼进一份 JSON"，不认识任何 agent。规则：
//! - 命令形态 `if [ -x <AGORA_HOME>/bin/agora ]; then <AGORA_HOME>/bin/agora hook --host <h> --home <AGORA_HOME>; fi`：
//!   稳定路径 + 显式 `--home`；`bin/agora` 是指向当前二进制的符号链接，安装时建 / 修。
//! - 幂等：命令里的 `<AGORA_HOME>/bin/agora hook` 是自己条目的标记——先删自己的再加，重复装不重复；
//!   卸载只删自己的，别人的条目与文件里其它键原样。
//! - 装前显示 diff（stderr），`--dry-run` 只看不写；写文件先 `.part` 再 rename。
//!
//! 配置文件的形态是三家共同的 `{"hooks": {"<Event>": [{"matcher"?, "hooks": [{"type":
//! "command", "command", "timeout"}]}]}}`（Claude settings.json、Grok agora.json 文档一致；
//! Codex 的 hooks.json 若不同在 dvh.7 分叉）。

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
    format!("if [ -x {bin} ]; then {bin} hook --host {host} --home {home}; fi")
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

    /// `<AGORA_HOME>/bin/agora` → 当前二进制；指错了就重指。
    pub fn ensure_bin_link(&self, exe: &Path) -> Result<PathBuf, InstallError> {
        let link = bin_path(&self.agora_home);
        let dir = link.parent().unwrap_or(&self.agora_home);
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        if std::fs::read_link(&link).ok().as_deref() == Some(exe) {
            return Ok(link);
        }
        if link.symlink_metadata().is_ok() {
            std::fs::remove_file(&link).map_err(io(&link))?;
        }
        std::os::unix::fs::symlink(exe, &link).map_err(io(&link))?;
        Ok(link)
    }
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

/// 入口。成功 0；用法错误 2；写不了 1。
pub fn run(argv: &[&str]) -> i32 {
    match run_inner(argv) {
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

fn run_inner(argv: &[&str]) -> Result<(), InstallError> {
    let args = parse_args(argv)?;
    let hooks = adapter::for_host(&args.agent).ok_or_else(|| {
        InstallError::Usage(format!("{} 没有 hook 可装\n{}", args.agent, usage()))
    })?;
    let installer = Installer {
        agora_home: args.agora_home.clone(),
        user_home: args.user_home.clone(),
    };
    let plan = if args.action == "install" {
        installer.plan_install(hooks)?
    } else {
        installer.plan_uninstall(hooks)?
    };
    if plan.is_noop() {
        eprintln!("{} 无需改动", plan.file.display());
        return Ok(());
    }
    eprintln!("--- {}", plan.file.display());
    eprint!("{}", plan.diff());
    if args.dry_run {
        eprintln!("(--dry-run，未写入)");
        return Ok(());
    }
    if args.action == "install" {
        let exe = std::env::current_exe().map_err(io(Path::new("current_exe")))?;
        let link = installer.ensure_bin_link(&exe)?;
        eprintln!("{} -> {}", link.display(), exe.display());
    }
    installer.write(&plan)?;
    eprintln!("已写入 {}", plan.file.display());
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
}
