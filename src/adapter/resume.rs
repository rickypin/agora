//! Restart 的 resume 与起会话时的钉 id（ADR-002 D7；agora-dvh.13）。
//!
//! 规划是纯函数：拿 `(agent_type, 原命令, 自报的对话 id, 版本探测结果)` 算出这次真正要跑的命令。
//! 识别顺序 agent 自报 > 启动时钉死的 id > 用户挑；表外版本不猜参数、退化为原命令并说明原因。
//! 绝不用"继续上一次"类参数（`tests/arch_boundary.rs::restart_never_uses_continue_or_last`）。

use super::{find, Adapter, VersionProbe};

/// Restart 到底怎么起。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPlan {
    /// 续上自报的对话。
    Resume {
        command: String,
        agent_session_id: String,
    },
    /// 退化为原命令；`reason` 给人看（API 响应与日志）。
    Original { command: String, reason: String },
}

impl RestartPlan {
    pub fn command(&self) -> &str {
        match self {
            RestartPlan::Resume { command, .. } | RestartPlan::Original { command, .. } => command,
        }
    }
}

/// 命令行的程序名：`claude --permission-mode default` → `claude`。探版本用它。
pub fn program_of(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

/// 算 Restart 的命令。`probe` 由调用方注入（真实探测跑 `<program> --version`，测试给定值）。
pub fn plan_restart(
    agent_type: &str,
    command: &str,
    agent_session_id: Option<&str>,
    probe: impl FnOnce(&dyn Adapter, &str) -> VersionProbe,
) -> RestartPlan {
    let original = |reason: String| RestartPlan::Original {
        command: command.to_owned(),
        reason,
    };
    let Some(adapter) = find(agent_type) else {
        return original(format!("{agent_type} 没有 Adapter，不知道怎么 resume"));
    };
    let id = match agent_session_id.map(str::trim) {
        Some(id) if !id.is_empty() && id != "unknown" => id,
        _ => return original("agent 还没自报过对话 id".into()),
    };
    let version = match probe(adapter, program_of(command)) {
        VersionProbe::Available(v) => v,
        VersionProbe::Unparsable(why) => {
            return original(format!("版本不可解析，不猜 resume 参数：{why}"))
        }
        VersionProbe::Missing => return original("命令不在 PATH 里".into()),
    };
    // 起会话时钉的 --session-id 这一代要拆掉：钉与 resume 指的是同一个对话，留着会打架。
    let pin = adapter.pin_args(version, id).unwrap_or_default();
    let pin_flags: Vec<&str> = flags_of(&pin).collect();
    match adapter.resume_args(version, id) {
        Some(args) => RestartPlan::Resume {
            command: splice_args(command, &args, &pin_flags),
            agent_session_id: id.to_owned(),
        },
        None => original(format!("{agent_type} {version} 不支持按对话 id resume")),
    }
}

/// 起会话时钉死对话 id（D7：只在自报缺席时才用得上，hook 一自报就覆盖）。
/// 返回 `(命令, 钉的 id)`；adapter 不支持、版本表外、或用户已经自己写了 resume / pin 参数 → None。
pub fn plan_pin(
    agent_type: &str,
    command: &str,
    probe: impl FnOnce(&dyn Adapter, &str) -> VersionProbe,
) -> Option<(String, String)> {
    let adapter = find(agent_type)?;
    let VersionProbe::Available(version) = probe(adapter, program_of(command)) else {
        return None;
    };
    let id = new_conversation_id();
    let pin = adapter.pin_args(version, &id)?;
    let resume = adapter.resume_args(version, &id).unwrap_or_default();
    // 用户自己指定了对话（命令里已有 pin 或 resume 的 flag）→ 尊重他，不钉。
    let flags: Vec<&str> = flags_of(&pin).chain(flags_of(&resume)).collect();
    if command.split_whitespace().any(|tok| flags.contains(&tok)) {
        return None;
    }
    Some((splice_args(command, &pin, &[]), id))
}

/// 把 adapter 给的参数接到命令尾上；命令里已有同名键（连同它的值）先拆掉，免得重复；
/// `also_strip` 里的键同样拆掉（如 resume 时拆 pin）。
/// 参数形态是 `键 值` 对：`--resume <id>`（Claude / Grok）或子命令 `resume <id>`（Codex，
/// 全局 `-c` 之类放在子命令前照样合法，2026-09-05 实测 0.152.1）。
pub fn splice_args(command: &str, args: &[String], also_strip: &[&str]) -> String {
    let mut flags: Vec<&str> = flags_of(args).collect();
    flags.extend_from_slice(also_strip);
    let mut out: Vec<String> = Vec::new();
    let mut toks = command.split_whitespace().peekable();
    while let Some(tok) = toks.next() {
        if flags.contains(&tok) {
            toks.next();
            continue;
        }
        out.push(tok.to_owned());
    }
    out.extend(args.iter().map(|a| shell_quote(a)));
    out.join(" ")
}

/// 参数里的键：偶数位（`--resume`、`resume`、`--session-id`）。
fn flags_of(args: &[String]) -> impl Iterator<Item = &str> {
    args.iter().step_by(2).map(String::as_str)
}

/// 对话 id 是 uuid，不需要引号；别的形态保守地单引号包住（命令经 `sh -c` 执行）。
fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// uuid v4（Claude `--session-id` 要求合法 uuid）。
pub fn new_conversation_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("系统随机源不可用");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
