//! Codex CLI。事件表、安装规格、`resume <id>` 子命令随 agora-dvh.7 按实测落地；本阶段
//! 只有身份与通用映射表（`hooks::GenericHooks`），hook 能替用户批准（文档说法，未实测）。

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
