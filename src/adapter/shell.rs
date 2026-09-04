//! 登录 shell：没有 hook、没有版本、没有对话身份，只有"起一个 shell"。

use super::{program_is, Adapter, AgentFallback, AgentIdentity};

pub struct Shell;

pub static SHELL: Shell = Shell;

/// 认自己用的程序名（argv[0] 的基名）。
pub const PROGRAMS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ksh"];

impl AgentIdentity for Shell {
    fn name(&self) -> &str {
        "shell"
    }

    /// 用户的登录 shell 由运行时的 `sh -c` 展开——存 `$SHELL` 而不是 `/bin/zsh`，
    /// 同一份库换台机器仍然对（ADR-001 D7）。
    fn default_command(&self) -> &str {
        "$SHELL"
    }

    fn match_process(&self, argv: &[&str]) -> bool {
        program_is(argv, PROGRAMS)
    }
}

impl AgentFallback for Shell {}
impl Adapter for Shell {}
