//! Codex CLI。事件表、安装规格、`resume <id>` 子命令随 agora-dvh.7 落地；本阶段只有身份与
//! 通用映射表（`hooks::GenericHooks`）。2026-09-05 实测 0.152.1（ADR-002 附录 A，agora-dvh.2）：
//! 事件名 CamelCase、键 snake_case，通用表拼法对；hook 返回 allow 确实放行，但**挂起期间 TUI
//! 不显示审批提示**，终端里的人答不了——hook 退出后提示才弹。所以 Codex 的挂起上限必须是
//! 秒级（dvh.7 定），不能照搬 Claude 的长挂起。PermissionRequest 不带 tool_use_id；未在 `/hooks`
//! 信任的 hook 被静默跳过，信任哈希按条存在 `~/.codex/config.toml [hooks.state]`。

use super::hooks::GenericHooks;
use super::{program_is, Adapter, AgentFallback, AgentHooks, AgentIdentity, Version, VersionProbe};

pub struct Codex;

pub static CODEX: Codex = Codex;

static HOOKS: GenericHooks = GenericHooks {
    host: "codex",
    decision_via_hook: true,
    is_grok: false,
};

impl AgentIdentity for Codex {
    fn name(&self) -> &str {
        "codex"
    }

    fn default_command(&self) -> &str {
        "codex"
    }

    fn match_process(&self, argv: &[&str]) -> bool {
        program_is(argv, &["codex"])
    }

    /// 实测 `codex-cli 0.152.1`（ADR-002 D7）。版本表随 dvh.7；表没有 → 只报版本号，不给能力。
    fn version(&self, output: &str) -> VersionProbe {
        match (output.contains("codex"), Version::find_in(output)) {
            (true, Some(v)) => VersionProbe::Available(v),
            _ => VersionProbe::Unparsable(output.trim().to_owned()),
        }
    }
}

impl AgentFallback for Codex {}

impl Adapter for Codex {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        Some(&HOOKS)
    }
}
