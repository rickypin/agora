//! `agora preset add <name> --agent <agent> --dir <path> [--args "…"] [--prompt "…"] | list | show | rm`
//! （agora-prdg.3；epic agora-hxva）。
//!
//! 预设在**桌面 / 终端**里定义（手机只消费，S2 的 `GET /api/presets` 只读）；本命令直接操作
//! `AGORA_HOME/agora.db`，不需要 daemon 在跑，daemon 若在跑它每次请求都查库，所以这里的增删改
//! 对它即时生效——与 `agora peer token` 同一条路（ADR-003 D6）。
//!
//! `--args` 是**原样字符串**（与你在终端里敲的完全一致，经 shell，见 `session::preset` 头注释）；
//! `--dir` 必须存在（别让手机点了才失败），存进库的是 canonicalize 过的绝对路径——预设可能在别的
//! 工作目录下被 daemon 使用，相对路径没有意义。`--prompt` 对不吃首句的 agent（grok / shell）
//! **警告并保留**（不算错：将来这个 agent 接了就自动生效）。
//!
//! 退出码与 `agora hooks` / `agora peer token` 一致：0 成功、1 操作失败（未知 agent、目录不存在、
//! 预设不存在）、2 用法错误。

use std::path::Path;

use crate::adapter::{self, AgentIdentity};
use crate::local;
use crate::session::preset;
use crate::session::Db;

const USAGE: &str = "\
用法: agora preset add <name> --agent <claude|codex|grok|pi|shell> --dir <path> [--args \"…\"] [--prompt \"…\"]
      agora preset list
      agora preset show <name>
      agora preset rm <name>";

/// 退出码：0 成功、1 操作失败、2 用法错误（与 `agora hooks` 一致）。
pub fn run(args: &[&str]) -> i32 {
    match args {
        ["add", rest @ ..] => add(rest),
        ["list"] => with_db(list),
        ["show", name] => with_db(|db| show(db, name)),
        ["rm", name] => with_db(|db| remove(db, name)),
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

/// 目录自检（0700、属主）与打开库，与 `agora peer token` 同一条路。库文件若由这里首次创建，
/// 也只能属主可读：先收 umask（进程级，安全）。
fn with_db(f: impl FnOnce(&Db) -> Result<(), String>) -> i32 {
    // SAFETY: umask 没有前置条件。
    unsafe { libc::umask(0o077) };
    let home = local::resolve_home();
    if let Err(err) = local::ensure_home(&home) {
        eprintln!("{err}");
        return 2;
    }
    let db = match Db::open(&home.join("agora.db")) {
        Ok(db) => db,
        Err(err) => {
            eprintln!("打开 metadata 库失败: {err}");
            return 1;
        }
    };
    match f(&db) {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("{msg}");
            1
        }
    }
}

/// `add` 的解析结果；字段都是字面值，取值校验在 [`add`]。
#[derive(Debug, Default)]
struct AddArgs {
    name: Option<String>,
    agent: Option<String>,
    dir: Option<String>,
    args: Option<String>,
    prompt: Option<String>,
}

fn parse_add(argv: &[&str]) -> Result<AddArgs, String> {
    let mut out = AddArgs::default();
    let mut it = argv.iter();
    while let Some(arg) = it.next().copied() {
        match arg {
            "--agent" => out.agent = Some(take(&mut it, "--agent")?.to_owned()),
            "--dir" => out.dir = Some(take(&mut it, "--dir")?.to_owned()),
            "--args" => out.args = Some(take(&mut it, "--args")?.to_owned()),
            "--prompt" => out.prompt = Some(take(&mut it, "--prompt")?.to_owned()),
            other if other.starts_with('-') => return Err(format!("未知参数: {other}")),
            other if out.name.is_none() => out.name = Some(other.to_owned()),
            other => return Err(format!("多余的参数: {other}")),
        }
    }
    Ok(out)
}

fn take<'a>(it: &mut std::slice::Iter<'a, &'a str>, flag: &str) -> Result<&'a str, String> {
    it.next().copied().ok_or_else(|| format!("{flag} 缺少取值"))
}

fn add(argv: &[&str]) -> i32 {
    let parsed = match parse_add(argv) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return usage();
        }
    };
    // 缺失必填项是用法错误（2）；取值不合法是操作失败（1）——与 `agora peer token` 同档。
    let Some(name) = parsed.name.filter(|n| !n.trim().is_empty()) else {
        eprintln!("缺少 <name>（预设名不能为空）");
        return usage();
    };
    let Some(agent) = parsed.agent else {
        eprintln!("缺少 --agent");
        return usage();
    };
    let Some(dir) = parsed.dir else {
        eprintln!("缺少 --dir");
        return usage();
    };
    if adapter::find(&agent).is_none() {
        let known: Vec<&str> = adapter::ADAPTERS.iter().map(|a| a.name()).collect();
        eprintln!("未知 agent 类型: {agent}（已知: {}）", known.join(", "));
        return 1;
    }
    let path = Path::new(&dir);
    if !path.is_dir() {
        eprintln!("目录不存在: {dir}");
        return 1;
    }
    // 存绝对路径：预设会被 daemon 在它自己的工作目录下使用，相对路径没有意义。
    let dir = match path.canonicalize() {
        Ok(p) => p.display().to_string(),
        Err(err) => {
            eprintln!("目录无法解析: {dir}: {err}");
            return 1;
        }
    };
    // 不吃首句的 agent 也保留 prompt：这里只是预设的定义，起会话时按 agent 当时的能力用
    // （现在 grok / shell 用不上，将来接了就生效）——警告而不拒绝。
    if parsed
        .prompt
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty())
        && !adapter::find(&agent).is_some_and(AgentIdentity::accepts_initial_prompt)
    {
        eprintln!(
            "警告: {agent} 当前不吃首句，--prompt 照存；将来该 agent 支持后自动生效（起会话时不会重发）"
        );
    }
    let prompt = parsed.prompt.as_deref();
    with_db(|db| {
        preset::upsert(db, &name, &agent, &dir, parsed.args.as_deref(), prompt)
            .map_err(|e| e.to_string())?;
        println!("已保存预设 {name}（{agent} @ {dir}）");
        Ok(())
    })
}

fn list(db: &Db) -> Result<(), String> {
    let rows = preset::list(db).map_err(|e| e.to_string())?;
    if rows.is_empty() {
        println!("还没有预设；`agora preset add <name> --agent claude --dir <目录>` 加一条");
        return Ok(());
    }
    let row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str| {
        println!("{a:<20} {b:<8} {c:<40} {d:<24} {e:<24} {f}");
    };
    row("NAME", "AGENT", "DIR", "ARGS", "PROMPT", "UPDATED");
    for p in rows {
        row(
            &p.name,
            &p.agent_type,
            &p.working_directory,
            p.args.as_deref().unwrap_or("-"),
            p.prompt.as_deref().unwrap_or("-"),
            &p.updated_at,
        );
    }
    Ok(())
}

fn show(db: &Db, name: &str) -> Result<(), String> {
    let p = preset::get(db, name).map_err(|e| e.to_string())?;
    println!("name:              {}", p.name);
    println!("agent_type:        {}", p.agent_type);
    println!("working_directory: {}", p.working_directory);
    println!("args:              {}", p.args.as_deref().unwrap_or("-"));
    println!("prompt:            {}", p.prompt.as_deref().unwrap_or("-"));
    println!("created_at:        {}", p.created_at);
    println!("updated_at:        {}", p.updated_at);
    Ok(())
}

fn remove(db: &Db, name: &str) -> Result<(), String> {
    preset::remove(db, name).map_err(|e| e.to_string())?;
    println!("已删除预设 {name}");
    Ok(())
}
