//! Adapter：agent 特定代码唯一允许的目录（ADR-002 D2/D7/D9；agora-dvh.5）。
//!
//! 只解析 `--version`，不解析帮助输出与错误文本（ADR-002 规则 10）；
//! Restart 绝不用"继续上一次"类参数，只用自报的对话 id（ADR-002 D7）。
//!
//! 本阶段（agora-xqa.12）只落地 [`AgentIdentity`] 的**启动侧**三个方法：`name`、
//! `default_command`、`match_process`。ADR-002 D9 的其余方法（`version`、`resume_args`、
//! `pin_args`）与 `AgentHooks` / `AgentFallback` 随 M1b 的 hook 侧与版本表补齐。

pub mod hooks;

use std::path::Path;

/// 对话身份与启动（ADR-002 D9 的 `AgentIdentity`）。
pub trait AgentIdentity: Send + Sync {
    /// `sessions.agent_type` 里存的名字。核心层只当它是自由字符串（ADR-002 D2）。
    fn name(&self) -> &str;

    /// 起会话时的默认命令。**可移植的裸命令名**，不写绝对路径（ADR-001 D7）：
    /// 库里存的命令会在 daemon 重启、换机器后被再次执行，绝对路径会跟着 brew 前缀漂走。
    fn default_command(&self) -> &str;

    /// 在进程树里认出自己：`argv` 是一个进程的命令行。
    ///
    /// 消费者是"未登记会话是哪个 agent"的判断（A16 external / A22 采纳），在 M1b 接上；
    /// 这里先把判定本身钉死，免得那时顺手写成核心层里的 if-else。
    fn match_process(&self, argv: &[&str]) -> bool;
}

/// V1 的四个 agent 在启动侧只有数据的差别，所以共用一个实现体。M1b 的 hook 侧各有各的
/// 事件映射，那时各 agent 会有自己的类型去实现 `AgentHooks`——`AgentIdentity` 是 trait
/// 而不是一张表，就是为了那时不用推翻这里。
pub struct Builtin {
    name: &'static str,
    default_command: &'static str,
    /// 认自己用的程序名（argv[0] 的基名）。
    programs: &'static [&'static str],
}

/// 内置 agent。名字同时是 `agents.<name>.command` 的配置键（docs/spec/config.md）。
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "claude",
        default_command: "claude",
        programs: &["claude"],
    },
    Builtin {
        name: "codex",
        default_command: "codex",
        programs: &["codex"],
    },
    Builtin {
        name: "grok",
        default_command: "grok",
        programs: &["grok"],
    },
    // 用户的登录 shell 由运行时的 `sh -c` 展开——存 `$SHELL` 而不是 `/bin/zsh`，
    // 同一份库换台机器仍然对（ADR-001 D7）。
    Builtin {
        name: "shell",
        default_command: "$SHELL",
        programs: &["sh", "bash", "zsh", "fish", "dash", "ksh"],
    },
];

/// 前端 Agent 下拉里的"自己填命令"那一项：没有 Adapter，命令必须由用户给。
pub const CUSTOM: &str = "custom";

impl AgentIdentity for Builtin {
    fn name(&self) -> &str {
        self.name
    }

    fn default_command(&self) -> &str {
        self.default_command
    }

    fn match_process(&self, argv: &[&str]) -> bool {
        let argv = unwrap_shell_c(argv);
        let Some(program) = argv.first() else {
            return false;
        };
        let base = basename(program);
        self.programs.contains(&base)
    }
}

/// 按名字取内置 Adapter。
pub fn find(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// 命令行 → agent；都不认就是 None（Unknown Agent）。
pub fn identify(argv: &[&str]) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.match_process(argv))
}

/// `sh -c "claude --resume x"` → `["claude", "--resume", "x"]`。
///
/// 运行时记下的启动命令多半是这个形态（运行时用 `sh -c` 执行会话命令），不解开
/// 的话每个会话都会被认成 shell。只解一层：`sh -c "sh -c …"` 现实里不存在，多解一层只是
/// 多一个能被构造出来的误判面。分词按空白，不处理引号——引号里的命令名不是我们要认的东西。
fn unwrap_shell_c<'a>(argv: &'a [&'a str]) -> Vec<&'a str> {
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

fn basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(argv: &[&str]) -> Option<&'static str> {
        identify(argv).map(|b| b.name())
    }

    #[test]
    fn builtin_names_and_default_commands_are_portable() {
        for b in BUILTINS {
            assert!(!b.default_command().contains('/'), "{}", b.name());
        }
        assert_eq!(find("claude").unwrap().default_command(), "claude");
        assert!(find("nope").is_none());
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
}
