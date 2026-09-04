//! Grok Build adapter（ADR-002 D2/D4/D7；agora-dvh.6）。
//!
//! 事件表按 1.0.13 实测（2026-09-04，终端内交互式 `--permission-mode default` + `-p` 无头；
//! 原始录制见 ADR-002 附录 A）。反直觉处，改表前先看：
//! - `hookEventName` 是**小写蛇形**（`session_start` / `pre_tool_use` / `stop` / `notification`），
//!   不是配置文件里的 `SessionStart`；通用表按 CamelCase 匹配会一条都认不出。这里把两种写法
//!   归一后再比（去下划线、小写），配置文件那种拼法也认。
//! - payload 每个键双写 camel + snake（`sessionId` / `session_id`），取键两种都试。
//! - `Stop` 在会话结束时也 fire 一次（`reason = shutdown`、无 `promptId`），只有
//!   `reason == end_turn` 才是一轮结束。
//! - `Notification(permission_prompt)` **不带工具身份**（只有 `level` / `message` /
//!   `notificationType`），而且 PreToolUse 的 `allow` 不能替用户批准（文档："only not blocked"）
//!   → `decision_via_hook = false`，WAITING(decision) 只能在终端答；任何工具结束
//!   （PostToolUse / PostToolUseFailure / PermissionDenied）或 StopCancelled 都当"人答了"。
//! - 终端里拒绝 → `permission_denied` + `stop_cancelled(reason = permission_rejected)`；
//!   Esc 中断 → `stop_cancelled(reason = user_interrupt)`，没有 PostToolUseFailure。
//! - `/clear` → 新 id 的 `session_start(source = new)`，旧会话**不发** `session_end`。
//! - hook 进程的父进程：安装命令用 `exec`，所以 ppid 就是 grok 本体（`agent_pid`）；
//!   环境里的 `CLAUDE_PID` 是从起 grok 的 shell 继承来的（本机在 Claude Code 里起过），不能用。
//!
//! fixture 在 `testdata/grok/1.0.13/hooks/`，回放器见 `super::replay`。

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::status::AgoraEvent;

use super::hooks::{self, str_of, Decision, Release};
use super::{
    program_is, Adapter, AgentFallback, AgentHooks, AgentIdentity, HookInstall, Version,
    VersionProbe,
};

pub struct Grok;

pub static GROK: Grok = Grok;

/// 版本表（ADR-002 D7）：1.0.13 实测 `--resume <id>`（id ≡ `GROK_SESSION_ID` ≡ payload
/// `sessionId`）与 `--session-id <uuid>`（只对新对话，须是合法 uuid）。更早版本没验证过，报不可解析。
const VERSION_TABLE: &[(Version, Capabilities)] = &[(
    Version(1, 0, 13),
    Capabilities {
        resume: true,
        pin: true,
    },
)];

#[derive(Debug, Clone, Copy)]
struct Capabilities {
    resume: bool,
    pin: bool,
}

fn capabilities(v: Version) -> Option<Capabilities> {
    VERSION_TABLE
        .iter()
        .rev()
        .find(|(min, _)| v >= *min)
        .map(|(_, c)| *c)
}

/// `StopFailure` 的 matcher（Grok 1.0.13 文档的 `error` 枚举，6 种）。
pub const STOP_FAILURE_MATCHERS: &[&str] = &[
    "rate_limit",
    "authentication_failed",
    "invalid_request",
    "server_error",
    "max_output_tokens",
    "unknown",
];

/// 挂起键：Grok 的权限提示不带工具身份，一个会话同时只有一个提示在等人，用固定键。
const PERMISSION_KEY: &str = "permission_prompt";

/// 安装的事件：`(event, matcher, timeout)`。没有 PermissionRequest（Grok 没有这个事件，也没有
/// 能替用户批准的 hook）；`Stop` 是闸门（exit 2 会拦住 agent），20 s 足够落盘 + 唤醒；SessionEnd
/// 有 10 s 退出预算，给 5 s 让 daemon 读完（不像 Claude 只有 1.5 s）。
fn install_table() -> Vec<(&'static str, Option<String>, u64)> {
    vec![
        ("SessionStart", None, 20),
        ("UserPromptSubmit", None, 20),
        ("PreToolUse", None, 20),
        ("PostToolUse", None, 20),
        ("PostToolUseFailure", None, 20),
        ("PermissionDenied", None, 20),
        (
            "Notification",
            Some("permission_prompt|idle_prompt".into()),
            20,
        ),
        ("Stop", None, 20),
        ("StopFailure", Some(STOP_FAILURE_MATCHERS.join("|")), 20),
        ("StopCancelled", None, 20),
        ("SessionEnd", None, 5),
    ]
}

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

    /// 实测 `grok 1.0.13 (5e9a58528b76)`（ADR-002 D7）。不带 "grok" 字样的不认。
    fn version(&self, output: &str) -> VersionProbe {
        let trimmed = output.trim();
        if !trimmed.starts_with("grok") {
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
            .map(|_| vec!["--resume".to_owned(), agent_session_id.to_owned()])
    }

    fn pin_args(&self, version: Version, new_id: &str) -> Option<Vec<String>> {
        capabilities(version)
            .filter(|c| c.pin)
            .map(|_| vec!["--session-id".to_owned(), new_id.to_owned()])
    }
}

impl AgentFallback for Grok {}

impl Adapter for Grok {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        Some(self)
    }
}

/// 事件名归一：`pre_tool_use`（实测）与 `PreToolUse`（配置拼法）都变成 `pretooluse`。
fn event_key(payload: &Value) -> Option<String> {
    hooks::event_name(payload).map(|n| {
        n.chars()
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect()
    })
}

fn tool_name(payload: &Value) -> &str {
    str_of(payload, &["toolName", "tool_name"]).unwrap_or("tool")
}

impl AgentHooks for Grok {
    fn host(&self) -> &str {
        "grok"
    }

    /// 写 `~/.grok/hooks/agora.json`（全局 hooks 目录，永远受信，不用 `/hooks-trust`）。
    /// 文件形态与 Claude 的 settings.json 相同（文档 "The Hook JSON Format"）。
    fn install_spec(&self) -> Vec<HookInstall> {
        install_table()
            .into_iter()
            .map(|(event, matcher, secs)| HookInstall {
                file: PathBuf::from(".grok/hooks/agora.json"),
                event: event.to_owned(),
                matcher,
                timeout: Duration::from_secs(secs),
            })
            .collect()
    }

    fn decision_via_hook(&self) -> bool {
        false
    }

    /// 宿主自认（ADR-002 D4）：Grok 默认兼容加载 `~/.claude/settings.json` 里的 hooks，装给
    /// Claude 的条目会被它以 `--host claude` 跑一遍；环境里有 `GROK_SESSION_ID` 的才是 Grok。
    fn host_matches_env(&self, has_grok_session: bool) -> bool {
        has_grok_session
    }

    fn agent_session_id(&self, payload: &Value) -> Option<String> {
        hooks::session_id(payload)
    }

    fn working_directory(&self, payload: &Value) -> Option<std::path::PathBuf> {
        hooks::cwd(payload)
    }

    /// 实测 1.0.13：`GROK_*` 里没有进程号；安装命令 `exec` 进 agora，hook 的 ppid 就是 grok。
    /// 不看 `CLAUDE_PID`——那是从起 grok 的终端继承来的别家进程。
    fn agent_pid(
        &self,
        _env: &std::collections::BTreeMap<String, String>,
        ppid: u32,
    ) -> Option<u32> {
        (ppid > 1).then_some(ppid)
    }

    fn parse(&self, payload: &Value) -> Vec<AgoraEvent> {
        let mut out = Vec::new();
        // 子 agent 的事件带 subagentType（文档），不是这个会话的状态。
        if str_of(payload, &["subagentType", "subagent_type"]).is_some() {
            return out;
        }
        let tool = tool_name(payload);
        match event_key(payload).as_deref() {
            // source: new（实测；/clear 后同样是 new 带新 id）。
            Some("sessionstart") => {
                out.push(AgoraEvent::SessionStarted);
                if let Some(id) = hooks::session_id(payload) {
                    out.push(AgoraEvent::SessionId(id));
                }
            }
            Some("userpromptsubmit") => out.push(AgoraEvent::PromptSubmitted(
                str_of(payload, &["prompt"]).unwrap_or_default().to_owned(),
            )),
            Some("pretooluse") => out.push(AgoraEvent::Activity(tool.to_owned())),
            // 工具有了结果 / 被拒：权限提示（若有）已在终端答了。
            Some("posttooluse" | "posttoolusefailure" | "permissiondenied") => {
                out.push(AgoraEvent::DecisionResolved(Some(PERMISSION_KEY.into())));
                out.push(AgoraEvent::Activity(tool.to_owned()));
            }
            Some("notification") => {
                match str_of(payload, &["notificationType", "notification_type"]) {
                    Some("permission_prompt") => out.push(AgoraEvent::DecisionNeeded {
                        tool_use_id: PERMISSION_KEY.into(),
                        summary: str_of(payload, &["message"])
                            .unwrap_or("permission requested")
                            .to_owned(),
                    }),
                    Some("idle_prompt") => out.push(AgoraEvent::Idle),
                    _ => {}
                }
            }
            Some("stop") => {
                if str_of(payload, &["reason"]) == Some("end_turn") {
                    out.push(AgoraEvent::TurnEnded(
                        str_of(payload, &["lastAssistantMessage", "last_assistant_message"])
                            .map(str::to_owned),
                    ));
                }
            }
            Some("stopfailure") => out.push(AgoraEvent::TurnFailed(
                str_of(payload, &["error", "errorType", "error_type"])
                    .unwrap_or("unknown")
                    .to_owned(),
            )),
            Some("stopcancelled") => out.push(AgoraEvent::TurnFailed(
                str_of(payload, &["reason"])
                    .unwrap_or("cancelled")
                    .to_owned(),
            )),
            Some("sessionend") => out.push(AgoraEvent::SessionEnded(
                str_of(payload, &["reason"]).map(str::to_owned),
            )),
            _ => {}
        }
        out
    }

    /// 没有能替用户批准的事件：什么都不挂起。
    fn hold_key(&self, _payload: &Value) -> Option<String> {
        None
    }

    fn release_for(&self, payload: &Value) -> Release {
        match event_key(payload).as_deref() {
            Some("posttooluse" | "posttoolusefailure" | "permissiondenied") => {
                Release::ToolUse(vec![PERMISSION_KEY.into()])
            }
            Some("stop" | "stopcancelled" | "stopfailure" | "sessionend") => Release::Session,
            _ => Release::None,
        }
    }

    fn decision_output(&self, _decision: &Decision) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn version_is_three_valued_and_table_bounded() {
        assert_eq!(
            GROK.version("grok 1.0.13 (5e9a58528b76)\n"),
            VersionProbe::Available(Version(1, 0, 13))
        );
        assert!(matches!(
            GROK.version("grok 1.0.2 (abc)"),
            VersionProbe::Unparsable(_)
        ));
        assert!(matches!(
            GROK.version("1.0.13 (Claude Code)"),
            VersionProbe::Unparsable(_)
        ));
        assert!(matches!(GROK.version(""), VersionProbe::Unparsable(_)));
    }

    #[test]
    fn resume_and_pin_use_the_reported_id_only() {
        let v = Version(1, 0, 13);
        assert_eq!(GROK.resume_args(v, "abc").unwrap(), vec!["--resume", "abc"]);
        assert_eq!(GROK.pin_args(v, "u1").unwrap(), vec!["--session-id", "u1"]);
        assert_eq!(GROK.resume_args(Version(1, 0, 0), "abc"), None);
    }

    #[test]
    fn snake_case_event_names_are_the_real_ones() {
        // 实测：hookEventName 是 session_start，不是 SessionStart；两种都认。
        assert_eq!(
            GROK.parse(&json!({"hookEventName":"session_start","sessionId":"s1"})),
            vec![
                AgoraEvent::SessionStarted,
                AgoraEvent::SessionId("s1".into())
            ]
        );
        assert_eq!(
            GROK.parse(&json!({"hook_event_name":"SessionStart","session_id":"s1"})),
            vec![
                AgoraEvent::SessionStarted,
                AgoraEvent::SessionId("s1".into())
            ]
        );
        assert_eq!(
            GROK.parse(&json!({"hookEventName":"pre_tool_use","toolName":"run_terminal_command"})),
            vec![AgoraEvent::Activity("run_terminal_command".into())]
        );
    }

    #[test]
    fn stop_counts_only_on_end_turn() {
        assert_eq!(
            GROK.parse(
                &json!({"hookEventName":"stop","reason":"end_turn","lastAssistantMessage":"Done."})
            ),
            vec![AgoraEvent::TurnEnded(Some("Done.".into()))]
        );
        // 会话结束时的 Stop（reason=shutdown，无 promptId）不是一轮结束。
        assert_eq!(
            GROK.parse(&json!({"hookEventName":"stop","reason":"shutdown"})),
            vec![]
        );
        assert_eq!(GROK.parse(&json!({"hookEventName":"stop"})), vec![]);
        // 子 agent 的 Stop 不算。
        assert_eq!(
            GROK.parse(
                &json!({"hookEventName":"stop","reason":"end_turn","subagentType":"explore"})
            ),
            vec![]
        );
    }

    #[test]
    fn permission_prompt_waits_and_only_the_terminal_can_answer() {
        let n = json!({"hookEventName":"notification","notificationType":"permission_prompt",
            "level":"info","message":"Tool permission requested"});
        assert_eq!(
            GROK.parse(&n),
            vec![AgoraEvent::DecisionNeeded {
                tool_use_id: PERMISSION_KEY.into(),
                summary: "Tool permission requested".into()
            }]
        );
        assert_eq!(GROK.hold_key(&n), None);
        assert!(!GROK.decision_via_hook());
        assert_eq!(GROK.decision_output(&Decision::Allow), None);
        // 终端拒绝：permission_denied 解除，stop_cancelled 带原因。
        let denied = json!({"hookEventName":"permission_denied","toolName":"run_terminal_command","toolUseId":"call-1"});
        assert_eq!(
            GROK.parse(&denied),
            vec![
                AgoraEvent::DecisionResolved(Some(PERMISSION_KEY.into())),
                AgoraEvent::Activity("run_terminal_command".into())
            ]
        );
        assert_eq!(
            GROK.release_for(&denied),
            Release::ToolUse(vec![PERMISSION_KEY.into()])
        );
        assert_eq!(
            GROK.parse(&json!({"hookEventName":"stop_cancelled","reason":"permission_rejected"})),
            vec![AgoraEvent::TurnFailed("permission_rejected".into())]
        );
        assert_eq!(
            GROK.parse(&json!({"hookEventName":"notification","notificationType":"idle_prompt"})),
            vec![AgoraEvent::Idle]
        );
    }

    #[test]
    fn failures_carry_their_classification() {
        assert_eq!(
            GROK.parse(&json!({"hookEventName":"stop_failure","error":"rate_limit"})),
            vec![AgoraEvent::TurnFailed("rate_limit".into())]
        );
        assert_eq!(
            GROK.parse(&json!({"hookEventName":"stop_cancelled","reason":"user_interrupt"})),
            vec![AgoraEvent::TurnFailed("user_interrupt".into())]
        );
        assert_eq!(STOP_FAILURE_MATCHERS.len(), 6);
    }

    #[test]
    fn install_spec_writes_the_global_hooks_file() {
        let spec = GROK.install_spec();
        assert!(spec
            .iter()
            .all(|h| h.file == PathBuf::from(".grok/hooks/agora.json")));
        assert!(spec.iter().all(|h| h.event != "PermissionRequest"));
        let of = |e: &str| spec.iter().find(|h| h.event == e).unwrap();
        assert_eq!(
            of("Notification").matcher.as_deref(),
            Some("permission_prompt|idle_prompt")
        );
        assert!(of("StopFailure")
            .matcher
            .as_deref()
            .unwrap()
            .contains("rate_limit"));
        assert_eq!(of("StopCancelled").matcher, None);
    }

    #[test]
    fn agent_pid_is_the_hook_parent_not_an_inherited_claude_pid() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("CLAUDE_PID".to_owned(), "51219".to_owned());
        assert_eq!(GROK.agent_pid(&env, 63370), Some(63370));
        assert_eq!(GROK.agent_pid(&env, 1), None);
        assert!(GROK.host_matches_env(true) && !GROK.host_matches_env(false));
    }
}
