//! Grok CLI。事件表（Stop 按 `reason == end_turn`、Notification 双路）与安装规格随
//! agora-dvh.6 按实测落地；本阶段是身份与通用映射表，`decision_via_hook = false`：它的 allow
//! 不能替用户批准（ADR-002 D2 "没有的能力就写没有"），Dashboard 只给"打开终端"。

use super::hooks::GenericHooks;
use super::{program_is, Adapter, AgentFallback, AgentHooks, AgentIdentity, Version, VersionProbe};

pub struct Grok;

pub static GROK: Grok = Grok;

static HOOKS: GenericHooks = GenericHooks {
    host: "grok",
    decision_via_hook: false,
    is_grok: true,
};

impl AgentIdentity for Grok {
    fn name(&self) -> &str {
        "grok"
    }

    fn default_command(&self) -> &str {
        "grok"
    }

    fn match_process(&self, argv: &[&str]) -> bool {
        program_is(argv, &["grok"])
    }

    /// 实测 `grok 1.0.13 (…)`（ADR-002 D7）。版本表随 dvh.6。
    fn version(&self, output: &str) -> VersionProbe {
        match (output.contains("grok"), Version::find_in(output)) {
            (true, Some(v)) => VersionProbe::Available(v),
            _ => VersionProbe::Unparsable(output.trim().to_owned()),
        }
    }
}

impl AgentFallback for Grok {}

impl Adapter for Grok {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        Some(&HOOKS)
    }
}
