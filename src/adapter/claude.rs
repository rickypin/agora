//! Claude Code adapter（ADR-002 D2/D4/D7；agora-dvh.5）。
//!
//! 事件表按 2.1.258 / 2.1.260 实测的 payload 键集合（agora-90t.3 注记；2026-09-04 真录）与
//! Claude Code hooks 文档：键全是 snake_case，`session_id` 是对话 id，`transcript_path` 只存
//! 路径不解析（D8）。实测反直觉处：`PermissionRequest` 没有 `tool_use_id`，只有 `tool_name`。
//! fixture 在 `testdata/claude/<version>/hooks/`，回放器见 `super::replay`。

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::status::AgoraEvent;

use super::hooks::{
    self, decision_key, event_name, generic_hold_key, generic_release, permission_output,
    release_keys, str_of, tool_use_id, Decision, Release,
};
use super::{
    program_is, Adapter, AgentFallback, AgentHooks, AgentIdentity, HookInstall, Version,
    VersionProbe,
};

pub struct Claude;

pub static CLAUDE: Claude = Claude;

/// 版本表（ADR-002 D7）：每行是"从这个版本起已知的能力"。低于表首的版本报不可解析——
/// 我们没在那些版本上验证过 `--resume` / `--session-id` 的语义，猜错比不猜更危险。
/// 新版本实测后加一行；表按版本升序。
const VERSION_TABLE: &[(Version, Capabilities)] = &[(
    Version(2, 1, 258),
    Capabilities {
        resume: true,
        pin: true,
    },
)];

#[derive(Debug, Clone, Copy)]
struct Capabilities {
    /// `--resume <session_id>`。
    resume: bool,
    /// `--session-id <uuid>`（只在自报缺席时用，D7）。
    pin: bool,
}

fn capabilities(v: Version) -> Option<Capabilities> {
    VERSION_TABLE
        .iter()
        .rev()
        .find(|(min, _)| v >= *min)
        .map(|(_, c)| *c)
}

/// `StopFailure` 的 matcher（Claude Code 文档的 error_type 枚举，11 种）。安装时全装，
/// 映射到 `turn.failed` 时 reason 原样带出。
pub const STOP_FAILURE_MATCHERS: &[&str] = &[
    "rate_limit",
    "authentication_failed",
    "billing_error",
    "invalid_request",
    "server_error",
    "max_output_tokens",
    "prompt_too_long",
    "network_error",
    "timeout",
    "overloaded",
    "unknown",
];

/// 安装的事件：`(event, matcher, timeout)`。PermissionRequest 挂起等人，timeout 3600 s，
/// agora 侧 55 min 先退（`hook::HOLD_TIMEOUT`）；SessionEnd 只有 1.5 s 预算，给 1 s；
/// 其余 20 s 够落盘 + 唤醒（ADR-002 D4）。
fn install_table() -> Vec<(&'static str, Option<String>, u64)> {
    vec![
        ("SessionStart", None, 20),
        ("UserPromptSubmit", None, 20),
        ("PreToolUse", None, 20),
        ("PostToolUse", None, 20),
        ("PostToolUseFailure", None, 20),
        ("PermissionRequest", None, 3600),
        ("PermissionDenied", None, 20),
        (
            "Notification",
            Some("permission_prompt|idle_prompt".into()),
            20,
        ),
        ("Stop", None, 20),
        ("StopFailure", Some(STOP_FAILURE_MATCHERS.join("|")), 20),
        ("SessionEnd", None, 1),
    ]
}

impl AgentIdentity for Claude {
    fn name(&self) -> &str {
        "claude"
    }

    fn default_command(&self) -> &str {
        "claude"
    }

    fn match_process(&self, argv: &[&str]) -> bool {
        program_is(argv, &["claude"])
    }

    /// 实测形态 `2.1.258 (Claude Code)`。不带 "Claude Code" 字样的不认：同名的别的程序
    /// 也会答 `--version`。
    fn version(&self, output: &str) -> VersionProbe {
        let trimmed = output.trim();
        if !trimmed.contains("Claude Code") {
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

impl AgentFallback for Claude {}

impl Adapter for Claude {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        Some(self)
    }
}

fn tool_input(payload: &Value) -> Option<&Value> {
    payload.get("tool_input")
}

/// AskUserQuestion 的第一问；Elicitation 的 message。
fn question(payload: &Value) -> String {
    tool_input(payload)
        .and_then(|i| i.get("questions"))
        .and_then(|q| q.get(0))
        .and_then(|q| q.get("question"))
        .and_then(Value::as_str)
        .or_else(|| str_of(payload, &["message", "prompt"]))
        .unwrap_or("question")
        .to_owned()
}

impl AgentHooks for Claude {
    fn host(&self) -> &str {
        "claude"
    }

    fn install_spec(&self) -> Vec<HookInstall> {
        install_table()
            .into_iter()
            .map(|(event, matcher, secs)| HookInstall {
                file: PathBuf::from(".claude/settings.json"),
                event: event.to_owned(),
                matcher,
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

    /// 实测 2.1.260（2026-09-04）：hook 环境里 `CLAUDE_PID` 就是 claude 主进程。
    fn agent_pid(
        &self,
        env: &std::collections::BTreeMap<String, String>,
        _ppid: u32,
    ) -> Option<u32> {
        env.get("CLAUDE_PID")?.trim().parse().ok()
    }

    fn parse(&self, payload: &Value) -> Vec<AgoraEvent> {
        let mut out = Vec::new();
        let tool = str_of(payload, &["tool_name"]).unwrap_or("tool");
        let id = || tool_use_id(payload);
        match event_name(payload) {
            // source: startup / clear / resume / compact——都带（可能是新的）session_id。
            Some("SessionStart") => {
                out.push(AgoraEvent::SessionStarted);
                if let Some(id) = hooks::session_id(payload) {
                    out.push(AgoraEvent::SessionId(id));
                }
            }
            Some("UserPromptSubmit") => out.push(AgoraEvent::PromptSubmitted(
                str_of(payload, &["prompt"]).unwrap_or_default().to_owned(),
            )),
            Some("PreToolUse") if tool == "AskUserQuestion" => out.push(AgoraEvent::InputNeeded {
                tool_use_id: id().unwrap_or_else(|| tool.to_owned()),
                question: question(payload),
            }),
            Some("PreToolUse") => out.push(AgoraEvent::Activity(tool.to_owned())),
            // Elicitation：MCP 服务器向用户要输入，同样只能在终端答。
            Some("Elicitation") => out.push(AgoraEvent::InputNeeded {
                tool_use_id: id().unwrap_or_else(|| "Elicitation".to_owned()),
                question: question(payload),
            }),
            Some("ElicitationResult") => out.push(AgoraEvent::DecisionResolved(Some(
                id().unwrap_or_else(|| "Elicitation".to_owned()),
            ))),
            // 解除按 id 与工具名两个键：PermissionRequest 实测（2.1.260）只带 tool_name。
            Some("PostToolUse" | "PostToolUseFailure" | "PermissionDenied") => {
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
            // permission_prompt 只是 PermissionRequest 的确认（挂起已经登记）；idle_prompt
            // 是 TURN_DONE 的补漏。
            Some("Notification") => {
                if str_of(payload, &["notification_type"]) == Some("idle_prompt") {
                    out.push(AgoraEvent::Idle);
                }
            }
            Some("Stop") => out.push(AgoraEvent::TurnEnded(
                str_of(payload, &["last_assistant_message"]).map(str::to_owned),
            )),
            Some("StopFailure") => out.push(AgoraEvent::TurnFailed(
                str_of(payload, &["error_type", "matcher"])
                    .unwrap_or("unknown")
                    .to_owned(),
            )),
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
        generic_release(payload)
    }

    fn decision_output(&self, decision: &Decision) -> Option<String> {
        permission_output(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn version_is_three_valued_and_table_bounded() {
        assert_eq!(
            CLAUDE.version("2.1.260 (Claude Code)\n"),
            VersionProbe::Available(Version(2, 1, 260))
        );
        assert!(matches!(
            CLAUDE.version("2.0.1 (Claude Code)"),
            VersionProbe::Unparsable(_)
        ));
        assert!(matches!(
            CLAUDE.version("2.1.258"),
            VersionProbe::Unparsable(_)
        ));
        assert!(matches!(CLAUDE.version(""), VersionProbe::Unparsable(_)));
    }

    #[test]
    fn resume_and_pin_use_the_reported_id_only() {
        let v = Version(2, 1, 258);
        assert_eq!(
            CLAUDE.resume_args(v, "abc").unwrap(),
            vec!["--resume", "abc"]
        );
        assert_eq!(
            CLAUDE.pin_args(v, "u1").unwrap(),
            vec!["--session-id", "u1"]
        );
        assert_eq!(CLAUDE.resume_args(Version(1, 0, 0), "abc"), None);
    }

    #[test]
    fn ask_user_question_waits_until_its_own_post_tool_use() {
        let pre = json!({"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"q1",
            "tool_input":{"questions":[{"question":"which?"}]}});
        assert_eq!(
            CLAUDE.parse(&pre),
            vec![AgoraEvent::InputNeeded {
                tool_use_id: "q1".into(),
                question: "which?".into()
            }]
        );
        let post = json!({"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion","tool_use_id":"q1"});
        assert_eq!(
            CLAUDE.parse(&post)[0],
            AgoraEvent::DecisionResolved(Some("q1".into()))
        );
        // AskUserQuestion 不是权限：不挂起。
        assert_eq!(CLAUDE.hold_key(&pre), None);
    }

    #[test]
    fn stop_failure_carries_the_error_type() {
        assert_eq!(
            CLAUDE.parse(&json!({"hook_event_name":"StopFailure","error_type":"rate_limit"})),
            vec![AgoraEvent::TurnFailed("rate_limit".into())]
        );
        assert_eq!(STOP_FAILURE_MATCHERS.len(), 11);
    }

    #[test]
    fn install_spec_matches_adr_002_d4() {
        let spec = CLAUDE.install_spec();
        let of = |e: &str| spec.iter().find(|h| h.event == e).unwrap();
        assert_eq!(of("PermissionRequest").timeout, Duration::from_secs(3600));
        assert_eq!(of("SessionEnd").timeout, Duration::from_secs(1));
        assert_eq!(of("Stop").timeout, Duration::from_secs(20));
        assert!(of("StopFailure")
            .matcher
            .as_deref()
            .unwrap()
            .contains("rate_limit"));
        assert!(spec
            .iter()
            .all(|h| h.file == PathBuf::from(".claude/settings.json")));
    }
}
