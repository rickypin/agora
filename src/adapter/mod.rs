//! Adapter：agent 特定代码唯一允许的目录（ADR-002 D2/D7/D9；agora-dvh.5）。
//!
//! 三个 trait（D9）：[`AgentIdentity`]（身份与启动、版本表、resume / pin 参数）、
//! [`AgentHooks`]（有 hook 的 agent 才实现：安装规格、事件映射、挂起与解除、决定写回）、
//! [`AgentFallback`]（文本兜底，默认什么都不认）。核心层只经 [`Adapter`] 注册表拿到它们，
//! 不知道任何 agent 的 payload 键（规则 5，`tests/arch_boundary.rs`）。
//!
//! 版本只解析 `--version` 这一个文档化输出，不解析帮助输出（规则 10）；flag 有无由各
//! adapter 的版本表回答，表以下的版本报"不可解析"——这就是三值（可用 / 不可解析 / 缺失）
//! 与规则 10 的边界（D7）。Restart 绝不用"继续上一次"类参数，只用自报的对话 id（D7）。

pub mod claude;
pub mod codex;
pub mod grok;
pub mod hooks;
pub mod replay;
pub mod shell;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::runtime::exec::{exec, ExecError, ExecOptions};
use crate::status::{AgoraEvent, DetectionResult};

pub use hooks::{Decision, Release};

/// `x.y.z`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// 从一段文本里取第一个 `x.y.z`。
    pub fn find_in(text: &str) -> Option<Version> {
        for token in text.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
            let mut it = token.split('.');
            let (Some(a), Some(b), Some(c)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            if it.next().is_some() {
                continue;
            }
            if let (Ok(a), Ok(b), Ok(c)) = (a.parse(), b.parse(), c.parse()) {
                return Some(Version(a, b, c));
            }
        }
        None
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// agent 可用性三值（ADR-002 D7）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionProbe {
    Available(Version),
    /// 命令在，但 `--version` 的输出不是这个 adapter 认得的形态，或版本低于版本表。
    Unparsable(String),
    Missing,
}

/// 对话身份与启动（ADR-002 D9 的 `AgentIdentity`）。
pub trait AgentIdentity: Send + Sync {
    /// `sessions.agent_type` 里存的名字。核心层只当它是自由字符串（ADR-002 D2）。
    fn name(&self) -> &str;

    /// 起会话时的默认命令。**可移植的裸命令名**，不写绝对路径（ADR-001 D7）：
    /// 库里存的命令会在 daemon 重启、换机器后被再次执行，绝对路径会跟着 brew 前缀漂走。
    fn default_command(&self) -> &str;

    /// 在进程树里认出自己：`argv` 是一个进程的命令行。
    fn match_process(&self, argv: &[&str]) -> bool;

    /// 解析 `<command> --version` 的输出。默认：认不出任何形态。
    fn version(&self, output: &str) -> VersionProbe {
        VersionProbe::Unparsable(output.trim().to_owned())
    }

    /// Restart 时按自报的对话 id 续上的参数；None = 这个版本不支持（或没有对话 id 概念）。
    fn resume_args(&self, _version: Version, _agent_session_id: &str) -> Option<Vec<String>> {
        None
    }

    /// 起会话时钉死对话 id 的参数（只在自报缺席时用，D7）。
    fn pin_args(&self, _version: Version, _new_id: &str) -> Option<Vec<String>> {
        None
    }
}

/// 安装到 agent 配置里的一条 hook（ADR-002 D4）；写文件的是 `agora hooks install`（dvh.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInstall {
    /// 相对用户 HOME 的配置文件路径。
    pub file: PathBuf,
    pub event: String,
    pub matcher: Option<String>,
    pub timeout: Duration,
}

/// 有 hook 的 agent（ADR-002 D9 的 `AgentHooks`）。
pub trait AgentHooks: Send + Sync {
    /// `--host` 的取值，也是投递箱的第一层目录。
    fn host(&self) -> &str;

    fn install_spec(&self) -> Vec<HookInstall>;

    /// hook 能不能替用户批准（D2 "没有的能力就写没有"）。
    fn decision_via_hook(&self) -> bool;

    /// 宿主自认（D4）：`has_grok_session` 是环境里有没有 `GROK_SESSION_ID`。
    fn host_matches_env(&self, has_grok_session: bool) -> bool {
        !has_grok_session
    }

    /// payload 里 agent 自报的对话 id（投递箱目录名与 `session.id`）。
    fn agent_session_id(&self, payload: &serde_json::Value) -> Option<String>;

    /// 一条原始事件 → 零或多条 agora 事件（D2 的映射表）。
    fn parse(&self, payload: &serde_json::Value) -> Vec<AgoraEvent>;

    /// 这条事件要不要挂起等决定；返回挂起键里的 `tool_use_id`。
    fn hold_key(&self, payload: &serde_json::Value) -> Option<String>;

    /// 这条事件到达时解除哪些挂起（D5）。
    fn release_for(&self, payload: &serde_json::Value) -> Release;

    /// 决定写回 stdout 的形态；None = 不输出（fail-open）。
    fn decision_output(&self, decision: &Decision) -> Option<String>;

    /// payload 里 agent 自报的工作目录：外部会话（无 AGORA_*）登记时的项目与显示名来源。
    fn working_directory(&self, _payload: &serde_json::Value) -> Option<std::path::PathBuf> {
        None
    }

    /// 信封 `agent_env` 里 agent 自己的进程号（如 `CLAUDE_PID`）：无运行时句柄的外部会话
    /// 靠它判断存活（ADR-002 D4）。不认识就 None → 活性 UNKNOWN。
    fn agent_pid(&self, _agent_env: &std::collections::BTreeMap<String, String>) -> Option<u32> {
        None
    }
}

/// 文本兜底（ADR-002 D6）：只服务无 hook 的会话；默认什么都不认。
pub trait AgentFallback: Send + Sync {
    /// `tail` 是屏幕末尾的非空行（已 strip ANSI）。
    fn detect(&self, _tail: &[&str]) -> Option<DetectionResult> {
        None
    }
}

pub trait Adapter: AgentIdentity + AgentFallback {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        None
    }
}

/// 内置 agent。名字同时是 `agents.<name>.command` 的配置键（docs/spec/config.md）。
pub const ADAPTERS: &[&dyn Adapter] = &[&claude::CLAUDE, &codex::CODEX, &grok::GROK, &shell::SHELL];

/// 前端 Agent 下拉里的"自己填命令"那一项：没有 Adapter，命令必须由用户给。
pub const CUSTOM: &str = "custom";

/// 按名字取内置 Adapter。
pub fn find(name: &str) -> Option<&'static dyn Adapter> {
    ADAPTERS.iter().copied().find(|a| a.name() == name)
}

/// 命令行 → agent；都不认就是 None（Unknown Agent）。
pub fn identify(argv: &[&str]) -> Option<&'static dyn Adapter> {
    ADAPTERS.iter().copied().find(|a| a.match_process(argv))
}

/// 这个 agent 类型有没有 hook（ADR-002 D1 "有 hook 的 agent"）。名字不认识（custom、fake）
/// 按没有算——收到 hook 事件状态机会自动升级。
pub fn has_hooks(agent_type: &str) -> bool {
    find(agent_type).is_some_and(|a| a.hooks().is_some())
}

/// `--host` 的合法取值。
pub fn hosts() -> Vec<&'static str> {
    ADAPTERS
        .iter()
        .filter_map(|a| a.hooks().map(|h| h.host()))
        .collect()
}

pub fn for_host(host: &str) -> Option<&'static dyn AgentHooks> {
    ADAPTERS
        .iter()
        .filter_map(|a| a.hooks())
        .find(|h| h.host() == host)
}

/// 探测可用性：跑 `<command> --version`（唯一允许解析的输出，规则 10）。调用方在 blocking 线程。
pub fn probe(adapter: &dyn Adapter, command: &str, timeout: Duration) -> VersionProbe {
    let opts = ExecOptions {
        timeout: Some(timeout),
        ..ExecOptions::default()
    };
    match exec(&[command, "--version"], &opts) {
        Ok(out) if out.status.success() => adapter.version(&String::from_utf8_lossy(&out.stdout)),
        Ok(out) => {
            VersionProbe::Unparsable(String::from_utf8_lossy(&out.stderr_tail).trim().to_owned())
        }
        Err(e) if e.is_not_found() => VersionProbe::Missing,
        Err(ExecError::Timeout { .. }) => VersionProbe::Unparsable("--version timed out".into()),
        Err(e) => VersionProbe::Unparsable(e.to_string()),
    }
}

/// `sh -c "claude --resume x"` → `["claude", "--resume", "x"]`。
///
/// 运行时记下的启动命令多半是这个形态（运行时用 `sh -c` 执行会话命令），不解开
/// 的话每个会话都会被认成 shell。只解一层：`sh -c "sh -c …"` 现实里不存在，多解一层只是
/// 多一个能被构造出来的误判面。分词按空白，不处理引号——引号里的命令名不是我们要认的东西。
pub(crate) fn unwrap_shell_c<'a>(argv: &'a [&'a str]) -> Vec<&'a str> {
    let is_shell = argv
        .first()
        .is_some_and(|p| matches!(basename(p), "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh"));
    if is_shell {
        if let Some(script) = argv.iter().skip_while(|a| **a != "-c").nth(1) {
            return script.split_whitespace().collect();
        }
    }
    argv.to_vec()
}

pub(crate) fn basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
}

/// argv[0] 的基名在名单里就是自己。
pub(crate) fn program_is(argv: &[&str], programs: &[&str]) -> bool {
    let argv = unwrap_shell_c(argv);
    argv.first()
        .is_some_and(|p| programs.contains(&basename(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(argv: &[&str]) -> Option<&'static str> {
        identify(argv).map(|b| b.name())
    }

    #[test]
    fn builtin_names_and_default_commands_are_portable() {
        for b in ADAPTERS {
            assert!(!b.default_command().contains('/'), "{}", b.name());
        }
        assert_eq!(find("claude").unwrap().default_command(), "claude");
        assert!(find("nope").is_none());
        assert_eq!(hosts(), vec!["claude", "codex", "grok"]);
        assert!(has_hooks("claude") && !has_hooks("shell") && !has_hooks("fake"));
    }

    #[test]
    fn match_process_reads_the_basename_not_the_path() {
        // brew 前缀、nix store 路径、直接裸名，认的都是同一个 agent。
        assert_eq!(name_of(&["claude"]), Some("claude"));
        assert_eq!(
            name_of(&["/opt/homebrew/bin/claude", "--foo"]),
            Some("claude")
        );
        assert_eq!(name_of(&["/usr/local/bin/codex"]), Some("codex"));
        assert_eq!(name_of(&["grok", "--resume", "abc"]), Some("grok"));
    }

    #[test]
    fn shell_c_is_unwrapped_before_matching() {
        // 运行时记下的启动命令是 `sh -c "<command>"`：不解开的话全都是 shell。
        assert_eq!(name_of(&["sh", "-c", "claude --resume x"]), Some("claude"));
        assert_eq!(name_of(&["/bin/zsh", "-c", "codex"]), Some("codex"));
        // 没有 -c 的 shell 就是 shell 会话本身。
        assert_eq!(name_of(&["zsh"]), Some("shell"));
        assert_eq!(name_of(&["/bin/bash", "-l"]), Some("shell"));
    }

    #[test]
    fn arguments_that_merely_mention_an_agent_do_not_match() {
        // 参数里出现 agent 名不算：认的是 argv[0]，不是"命令行里有没有这个词"。
        assert_eq!(name_of(&["vim", "claude.md"]), None);
        assert_eq!(name_of(&["git", "commit", "-m", "codex"]), None);
        assert_eq!(name_of(&[]), None);
    }

    #[test]
    fn version_finds_first_triplet() {
        assert_eq!(
            Version::find_in("2.1.260 (Claude Code)"),
            Some(Version(2, 1, 260))
        );
        assert_eq!(
            Version::find_in("codex-cli 0.152.1"),
            Some(Version(0, 152, 1))
        );
        assert_eq!(
            Version::find_in("grok 1.0.13 (abc)"),
            Some(Version(1, 0, 13))
        );
        assert_eq!(Version::find_in("usage: foo"), None);
        assert!(Version(2, 1, 260) > Version(2, 1, 258));
    }

    #[test]
    fn probe_reports_missing_for_absent_commands() {
        assert_eq!(
            probe(
                &claude::CLAUDE,
                "/nonexistent/agora-no-such-agent",
                Duration::from_secs(2)
            ),
            VersionProbe::Missing
        );
        // 命令在但输出不是 Claude 的形态：不可解析（规则 10 的边界）。
        assert!(matches!(
            probe(&claude::CLAUDE, "true", Duration::from_secs(2)),
            VersionProbe::Unparsable(_)
        ));
    }
}
