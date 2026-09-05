//! Codex CLI adapter（ADR-002 D2/D4/D5/D7；agora-dvh.7）。
//!
//! 事件表按 2026-09-05 本机 0.152.1 交互式实测（ADR-002 附录 A；agora-dvh.2）：事件名 CamelCase、
//! 键 snake_case（`hook_event_name`、`tool_use_id`、`turn_id`），fixture 在 `testdata/codex/<version>/hooks/`。
//! 实测反直觉处，别好心改回去：
//! - **挂起期间 TUI 不显示审批提示**，只显示 "Running PermissionRequest hook"；hook 退出后提示
//!   才弹。所以 Codex 的挂起是独占而非并存（Claude 是并存），上限只能秒级（[`HOLD_TIMEOUT`]），
//!   超时 fail-open 把提示交回终端。
//! - `PermissionRequest` 不带 `tool_use_id`（`PreToolUse` 带 `exec-<uuid>`），只能按 `tool_name` 键。
//! - 未在 TUI `/hooks` 信任的 hook **静默跳过**，无任何提示；信任按条 sha256 存
//!   `~/.codex/config.toml [hooks.state]`，改条目（含 timeout）该条失效、改脚本内容不失效。
//!   所以安装条目必须稳定，装后要提醒用户跑 `/hooks`（[`INSTALL_HINT`]）。
//! - 没有 Notification / idle_prompt / StopFailure；Esc 中断发 `Interrupt`，没有 PostToolUse。
//! - hook 环境没有 `CODEX_*` 进程号变量；安装命令 `exec` 进 agora，ppid 就是 codex 本体。
//! - `SessionEnd` / `Interrupt` 的 hook timeout 上限 3 s（超过自动 clamp 并警告）。
//! - `/clear` 只发新 id 的 `SessionStart(source=clear)`；退出时每个 session 各发一次 SessionEnd。
//! - `SessionStart` 在**第一条 prompt 提交时**才 fire（与 UserPromptSubmit 相隔几十毫秒），不是
//!   TUI 启动时（2026-09-05 真 daemon 端到端）：起会话后到用户第一次提问之前状态停在进程层，
//!   自报 id 也要等到那时——Restart 在此之前只能退化为原命令（"还没自报过对话 id"）。

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::status::AgoraEvent;

use super::hooks::{
    self, decision_key, event_name, generic_hold_key, generic_release, permission_output,
    release_keys, str_of, Decision, Release,
};
use super::{
    program_is, Adapter, AgentFallback, AgentHooks, AgentIdentity, HookInstall, Version,
    VersionProbe,
};

pub struct Codex;

pub static CODEX: Codex = Codex;

/// 版本表（ADR-002 D7）：`codex resume <id>` 子命令（0.152.1 实测；全局 `-c` 放子命令前合法）。
/// 没有钉 id 的参数：Codex 的对话 id 由它自己生成，Restart 只能靠自报。
const VERSION_TABLE: &[(Version, Capabilities)] =
    &[(Version(0, 152, 1), Capabilities { resume: true })];

#[derive(Debug, Clone, Copy)]
struct Capabilities {
    resume: bool,
}

fn capabilities(v: Version) -> Option<Capabilities> {
    VERSION_TABLE
        .iter()
        .rev()
        .find(|(min, _)| v >= *min)
        .map(|(_, c)| *c)
}

/// 挂起上限：挂起期间终端答不了，这就是终端用户看到 "Running PermissionRequest hook" 干等的
/// 最长时间。20 s 够 Dashboard 上的人点一下，不够让人在终端焦虑。
pub const HOLD_TIMEOUT: Duration = Duration::from_secs(20);

/// 装完要说的话：不信任就一条事件都收不到，且没有任何报错可查。
pub const INSTALL_HINT: &str = "Codex 未信任的 hook 会被静默跳过：请在 Codex TUI 里输入 /hooks，按 t 信任 agora 的条目（升级 agora 不用重做，条目不变）。";

/// 安装的事件：`(event, timeout)`。PermissionRequest 的 timeout 要盖住挂起上限 + 客户端 30 s 余量；
/// SessionEnd / Interrupt 上限 3 s（实测超过会被 clamp 并在 stderr 警告——每次启动都刷一行）；
/// 其余 20 s 够落盘 + 唤醒。没装 PreCompact / PostCompact / Subagent*：状态机用不上。
fn install_table() -> Vec<(&'static str, u64)> {
    vec![
        ("SessionStart", 20),
        ("UserPromptSubmit", 20),
        ("PreToolUse", 20),
        ("PostToolUse", 20),
        ("PermissionRequest", HOLD_TIMEOUT.as_secs() + 40),
        ("Stop", 20),
        ("Interrupt", 3),
        ("SessionEnd", 3),
    ]
}

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

    /// 实测形态 `codex-cli 0.152.1`。不带 "codex" 字样的不认。
    fn version(&self, output: &str) -> VersionProbe {
        let trimmed = output.trim();
        if !trimmed.contains("codex") {
            return VersionProbe::Unparsable(trimmed.to_owned());
        }
        match Version::find_in(trimmed) {
            Some(v) if capabilities(v).is_some() => VersionProbe::Available(v),
            Some(v) => VersionProbe::Unparsable(format!(
                "{v} 低于版本表首项 {}，能力未验证",
                VERSION_TABLE[0].0
            )),
            None => VersionProbe::Unparsable(trimmed.to_owned()),
        }
    }

    fn resume_args(&self, version: Version, agent_session_id: &str) -> Option<Vec<String>> {
        capabilities(version)
            .filter(|c| c.resume)
            .map(|_| vec!["resume".to_owned(), agent_session_id.to_owned()])
    }

    /// `codex exec <prompt>`。两个 flag 都是为隔离 HOME 里的冒烟准备的：临时目录不是 git 仓库
    /// （`--skip-git-repo-check`），也没有 `/hooks` 信任状态——信任只能在 TUI 里按，无头没法做，
    /// 只好 `--dangerously-bypass-hook-trust`；跑的 hook 是冒烟自己刚装进临时 HOME 的，没有别人的。
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec![
            "exec".to_owned(),
            "--skip-git-repo-check".to_owned(),
            "--dangerously-bypass-hook-trust".to_owned(),
            prompt.to_owned(),
        ])
    }
}

impl AgentFallback for Codex {}

impl Adapter for Codex {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        Some(self)
    }
}

impl AgentHooks for Codex {
    fn host(&self) -> &str {
        "codex"
    }

    fn install_spec(&self) -> Vec<HookInstall> {
        install_table()
            .into_iter()
            .map(|(event, secs)| HookInstall {
                file: PathBuf::from(".codex/hooks.json"),
                event: event.to_owned(),
                matcher: None,
                timeout: Duration::from_secs(secs),
            })
            .collect()
    }

    fn decision_via_hook(&self) -> bool {
        true
    }

    fn agent_session_id(&self, payload: &Value) -> Option<String> {
        hooks::session_id(payload)
    }

    fn working_directory(&self, payload: &Value) -> Option<std::path::PathBuf> {
        hooks::cwd(payload)
    }

    /// 实测 0.152.1：环境里没有 CODEX_* 进程号；安装命令 `exec` 进 agora，ppid 就是 codex。
    fn agent_pid(
        &self,
        _env: &std::collections::BTreeMap<String, String>,
        ppid: u32,
    ) -> Option<u32> {
        (ppid > 1).then_some(ppid)
    }

    fn parse(&self, payload: &Value) -> Vec<AgoraEvent> {
        let mut out = Vec::new();
        let tool = str_of(payload, &["tool_name"]).unwrap_or("tool");
        match event_name(payload) {
            // source: startup / resume / clear / compact——都带（可能是新的）session_id。
            Some("SessionStart") => {
                out.push(AgoraEvent::SessionStarted);
                if let Some(id) = hooks::session_id(payload) {
                    out.push(AgoraEvent::SessionId(id));
                }
            }
            Some("UserPromptSubmit") => out.push(AgoraEvent::PromptSubmitted(
                str_of(payload, &["prompt"]).unwrap_or_default().to_owned(),
            )),
            Some("PreToolUse") => out.push(AgoraEvent::Activity(tool.to_owned())),
            // 解除按 id 与工具名两个键：PermissionRequest 只带 tool_name。
            Some("PostToolUse") => {
                out.extend(
                    release_keys(payload)
                        .into_iter()
                        .map(|k| AgoraEvent::DecisionResolved(Some(k))),
                );
                out.push(AgoraEvent::Activity(tool.to_owned()));
            }
            Some("PermissionRequest") => out.push(AgoraEvent::DecisionNeeded {
                tool_use_id: decision_key(payload),
                summary: tool.to_owned(),
            }),
            Some("Stop") => out.push(AgoraEvent::TurnEnded(
                str_of(payload, &["last_assistant_message"]).map(str::to_owned),
            )),
            // Esc：这一轮没有正常收尾，也没有 PostToolUse 来解除挂起。
            Some("Interrupt") => out.push(AgoraEvent::TurnFailed("interrupt".to_owned())),
            Some("SessionEnd") => out.push(AgoraEvent::SessionEnded(
                str_of(payload, &["reason"]).map(str::to_owned),
            )),
            _ => {}
        }
        out
    }

    fn hold_key(&self, payload: &Value) -> Option<String> {
        generic_hold_key(true, payload)
    }

    fn release_for(&self, payload: &Value) -> Release {
        match event_name(payload) {
            Some("Interrupt") => Release::Session,
            _ => generic_release(payload),
        }
    }

    fn decision_output(&self, decision: &Decision) -> Option<String> {
        permission_output(decision)
    }

    fn hold_timeout(&self) -> Duration {
        HOLD_TIMEOUT
    }

    fn install_hint(&self) -> Option<String> {
        Some(INSTALL_HINT.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn version_is_three_valued_and_table_bounded() {
        assert_eq!(
            CODEX.version("codex-cli 0.152.1\n"),
            VersionProbe::Available(Version(0, 152, 1))
        );
        assert!(matches!(
            CODEX.version("codex-cli 0.145.0"),
            VersionProbe::Unparsable(_)
        ));
        assert!(matches!(
            CODEX.version("0.152.1"),
            VersionProbe::Unparsable(_)
        ));
    }

    #[test]
    fn resume_is_a_subcommand_and_there_is_no_pin() {
        let v = Version(0, 152, 1);
        assert_eq!(CODEX.resume_args(v, "abc").unwrap(), vec!["resume", "abc"]);
        assert_eq!(CODEX.pin_args(v, "u1"), None);
        assert_eq!(CODEX.resume_args(Version(0, 1, 0), "abc"), None);
    }

    #[test]
    fn permission_request_holds_on_tool_name_and_interrupt_releases_everything() {
        let pr = json!({"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch x"},"turn_id":"t"});
        assert_eq!(CODEX.hold_key(&pr).as_deref(), Some("Bash"));
        assert_eq!(
            CODEX.parse(&pr),
            vec![AgoraEvent::DecisionNeeded {
                tool_use_id: "Bash".into(),
                summary: "Bash".into()
            }]
        );
        let post = json!({"hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"exec-1","tool_response":""});
        assert_eq!(
            CODEX.release_for(&post),
            Release::ToolUse(vec!["exec-1".into(), "Bash".into()])
        );
        let esc = json!({"hook_event_name":"Interrupt","turn_id":"t"});
        assert_eq!(
            CODEX.parse(&esc),
            vec![AgoraEvent::TurnFailed("interrupt".into())]
        );
        assert_eq!(CODEX.release_for(&esc), Release::Session);
        assert_eq!(CODEX.hold_key(&esc), None);
    }

    #[test]
    fn hold_is_seconds_because_the_terminal_cannot_answer_meanwhile() {
        assert!(CODEX.hold_timeout() <= Duration::from_secs(60));
        let spec = CODEX.install_spec();
        let of = |e: &str| spec.iter().find(|h| h.event == e).unwrap();
        assert!(of("PermissionRequest").timeout > CODEX.hold_timeout() + Duration::from_secs(30));
        assert!(of("SessionEnd").timeout <= Duration::from_secs(3));
        assert!(of("Interrupt").timeout <= Duration::from_secs(3));
        assert!(spec
            .iter()
            .all(|h| h.file == PathBuf::from(".codex/hooks.json")));
        assert!(spec.iter().all(|h| h.matcher.is_none()));
        assert!(CODEX.install_hint().unwrap().contains("/hooks"));
    }

    #[test]
    fn agent_pid_is_the_hook_parent() {
        let env = std::collections::BTreeMap::from([("CLAUDE_PID".to_owned(), "42".to_owned())]);
        assert_eq!(CODEX.agent_pid(&env, 777), Some(777));
        assert_eq!(CODEX.agent_pid(&env, 1), None);
    }
}
