//! 各宿主 `AgentHooks` 实现共用的零件（ADR-002 D2/D3/D5）。
//!
//! 决定的形态（[`Decision`]）、解除的形态（[`Release`]）、payload 取键的小工具，以及一张
//! **三家共用的通用映射表** [`GenericHooks`]：Codex / Grok 在各自的 adapter（agora-dvh.7 /
//! dvh.6）按实测分叉之前先用它；Claude 已经有自己的表（`claude.rs`）。键名双写 camel / snake
//! 都认，因为 Grok 双写、Codex 文档用 snake。

use serde_json::Value;

use crate::status::AgoraEvent;

use super::{AgentHooks, HookInstall};

/// 环境里哪些变量名前缀属于 agent 自己、值得进信封（`CLAUDE_PID`、`GROK_*`、`CODEX_*`）。
pub fn agent_env_prefixes() -> &'static [&'static str] {
    &["CLAUDE_", "GROK_", "CODEX_"]
}

/// Dashboard 的决定（`decision.behavior`），经挂起的 hook 同步返回给 agent。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny {
        message: Option<String>,
    },
    /// 没有决定（超时、上限、终端答了、进程退出）：hook 静默退出，TUI 的提示还在。
    None,
}

/// 一条事件到达时会解除哪些挂起（ADR-002 D5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// 同工具的 PostToolUse / PostToolUseFailure / PermissionDenied：终端里答了。列表是候选键：
    /// `tool_use_id` 与 `tool_name`——实测 2026-09-04 Claude 2.1.260 的 PermissionRequest **不带**
    /// `tool_use_id`，挂起只能按 `tool_name` 键，解除时两个键都试。
    ToolUse(Vec<String>),
    /// Stop / SessionEnd：这一轮结束，所有挂起都过期。
    Session,
    None,
}

pub(crate) fn str_of<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| payload.get(k).and_then(Value::as_str))
}

pub(crate) fn event_name(payload: &Value) -> Option<&str> {
    str_of(payload, &["hook_event_name", "hookEventName"])
}

/// payload 的 `cwd`（Claude / Codex / Grok 三家的事件都带）。
pub(crate) fn cwd(payload: &Value) -> Option<std::path::PathBuf> {
    str_of(payload, &["cwd"]).map(std::path::PathBuf::from)
}

pub(crate) fn session_id(payload: &Value) -> Option<String> {
    str_of(payload, &["session_id", "sessionId"]).map(str::to_owned)
}

pub(crate) fn tool_use_id(payload: &Value) -> Option<String> {
    str_of(payload, &["tool_use_id", "toolUseId", "call_id"]).map(str::to_owned)
}

/// `PermissionRequest` 决定写回 stdout 的形态：Claude Code 文档的
/// `hookSpecificOutput.decision.behavior`；Codex 文档同为 `decision.behavior`，本机未实测
/// （agora-dvh.7 核实后若形态不同在它的 adapter 里分叉）。`None` → 不输出（fail-open）。
pub(crate) fn permission_output(decision: &Decision) -> Option<String> {
    let decision = match decision {
        Decision::Allow => serde_json::json!({ "behavior": "allow" }),
        Decision::Deny { message } => match message {
            Some(m) => serde_json::json!({ "behavior": "deny", "message": m }),
            None => serde_json::json!({ "behavior": "deny" }),
        },
        Decision::None => return None,
    };
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": decision,
            }
        })
        .to_string(),
    )
}

/// 挂起 / 待决的键：`tool_use_id`，没有就 `tool_name`（Claude 2.1.260 的 PermissionRequest
/// 实测只有后者），再没有用事件名占位，免得一个会话里只能挂一个。按 tool_name 键的代价：
/// 并行的两个同名工具分不开，后到的接替先到的（`Receiver::hold` 同键接替）。
pub(crate) fn decision_key(payload: &Value) -> String {
    tool_use_id(payload)
        .or_else(|| str_of(payload, &["tool_name", "toolName"]).map(str::to_owned))
        .unwrap_or_else(|| "PermissionRequest".into())
}

/// 通用的挂起判定：只有能替用户批准的宿主的 `PermissionRequest`。
pub(crate) fn generic_hold_key(decision_via_hook: bool, payload: &Value) -> Option<String> {
    if !decision_via_hook || event_name(payload) != Some("PermissionRequest") {
        return None;
    }
    Some(decision_key(payload))
}

/// PostToolUse 等解除时的候选键：id 与工具名都算。
pub(crate) fn release_keys(payload: &Value) -> Vec<String> {
    let mut keys: Vec<String> = tool_use_id(payload).into_iter().collect();
    if let Some(name) = str_of(payload, &["tool_name", "toolName"]) {
        keys.push(name.to_owned());
    }
    keys
}

pub(crate) fn generic_release(payload: &Value) -> Release {
    match event_name(payload) {
        Some("PostToolUse" | "PostToolUseFailure" | "PermissionDenied") => {
            let keys = release_keys(payload);
            if keys.is_empty() {
                Release::None
            } else {
                Release::ToolUse(keys)
            }
        }
        Some("Stop" | "SessionEnd") => Release::Session,
        _ => Release::None,
    }
}

/// 三家共用的一张映射表（ADR-002 D2）。Grok 的 Stop 要按 `reason == end_turn` 过滤已在此处。
pub struct GenericHooks {
    pub host: &'static str,
    pub decision_via_hook: bool,
    /// 宿主自认（ADR-002 D4）：`GROK_SESSION_ID` 在环境里的那个才是 Grok。
    pub is_grok: bool,
}

impl AgentHooks for GenericHooks {
    fn host(&self) -> &str {
        self.host
    }

    /// 安装规格随各自的 adapter 落地（dvh.6 / dvh.7）：现在没有就写没有。
    fn install_spec(&self) -> Vec<HookInstall> {
        Vec::new()
    }

    fn decision_via_hook(&self) -> bool {
        self.decision_via_hook
    }

    /// Grok 默认兼容加载 `~/.claude/settings.json` 里的 hooks，agora 装给 Claude 的条目会被
    /// Grok 以另一个 `--host` 跑一遍。判定只看环境，不按 payload 键名风格猜。
    fn host_matches_env(&self, has_grok_session: bool) -> bool {
        self.is_grok == has_grok_session
    }

    fn agent_session_id(&self, payload: &Value) -> Option<String> {
        session_id(payload)
    }

    fn working_directory(&self, payload: &Value) -> Option<std::path::PathBuf> {
        cwd(payload)
    }

    fn parse(&self, payload: &Value) -> Vec<AgoraEvent> {
        let mut out = Vec::new();
        let tool = str_of(payload, &["tool_name", "toolName"]).unwrap_or("tool");
        match event_name(payload) {
            Some("SessionStart") => {
                out.push(AgoraEvent::SessionStarted);
                if let Some(id) = session_id(payload) {
                    out.push(AgoraEvent::SessionId(id));
                }
            }
            Some("UserPromptSubmit") => out.push(AgoraEvent::PromptSubmitted(
                str_of(payload, &["prompt"]).unwrap_or_default().to_owned(),
            )),
            Some("PreToolUse") => out.push(AgoraEvent::Activity(tool.to_owned())),
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
            Some("Notification") => {
                let kind = str_of(
                    payload,
                    &["notification_type", "notificationType", "matcher"],
                );
                match kind {
                    Some("idle_prompt") => out.push(AgoraEvent::Idle),
                    // permission_prompt 只是 PermissionRequest 的确认；能挂起的宿主已经有了，
                    // 不能的（Grok）它就是 WAITING(decision) 的唯一来源。
                    Some("permission_prompt") if !self.decision_via_hook => {
                        out.push(AgoraEvent::DecisionNeeded {
                            tool_use_id: decision_key(payload),
                            summary: str_of(payload, &["message"]).unwrap_or(tool).to_owned(),
                        })
                    }
                    _ => {}
                }
            }
            Some("Stop") => {
                let reason = str_of(payload, &["reason"]);
                if reason.is_none_or(|r| r == "end_turn") {
                    out.push(AgoraEvent::TurnEnded(
                        str_of(payload, &["last_assistant_message", "lastAssistantMessage"])
                            .map(str::to_owned),
                    ));
                }
            }
            Some("StopFailure" | "StopCancelled" | "Interrupt") => {
                out.push(AgoraEvent::TurnFailed(
                    str_of(payload, &["matcher", "reason", "error_type", "errorType"])
                        .unwrap_or("failed")
                        .to_owned(),
                ))
            }
            Some("SessionEnd") => out.push(AgoraEvent::SessionEnded(
                str_of(payload, &["reason"]).map(str::to_owned),
            )),
            _ => {}
        }
        out
    }

    fn hold_key(&self, payload: &Value) -> Option<String> {
        generic_hold_key(self.decision_via_hook, payload)
    }

    fn release_for(&self, payload: &Value) -> Release {
        generic_release(payload)
    }

    fn decision_output(&self, decision: &Decision) -> Option<String> {
        if !self.decision_via_hook {
            return None;
        }
        permission_output(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CAN: GenericHooks = GenericHooks {
        host: "can",
        decision_via_hook: true,
        is_grok: false,
    };
    const CANNOT: GenericHooks = GenericHooks {
        host: "cannot",
        decision_via_hook: false,
        is_grok: true,
    };

    #[test]
    fn generic_table_maps_both_key_styles() {
        assert_eq!(
            CAN.parse(&json!({"hook_event_name":"SessionStart","session_id":"s1"})),
            vec![
                AgoraEvent::SessionStarted,
                AgoraEvent::SessionId("s1".into())
            ]
        );
        assert_eq!(
            CAN.parse(&json!({"hookEventName":"PostToolUse","toolUseId":"t","toolName":"Bash"})),
            vec![
                AgoraEvent::DecisionResolved(Some("t".into())),
                AgoraEvent::DecisionResolved(Some("Bash".into())),
                AgoraEvent::Activity("Bash".into())
            ]
        );
        // session 结束与续跑也 fire Stop 的宿主：只认 end_turn。
        assert_eq!(
            CANNOT.parse(&json!({"hookEventName":"Stop","reason":"resume"})),
            vec![]
        );
        assert_eq!(
            CANNOT.parse(
                &json!({"hookEventName":"Notification","notificationType":"permission_prompt","message":"needs you"})
            ),
            vec![AgoraEvent::DecisionNeeded {
                tool_use_id: "PermissionRequest".into(),
                summary: "needs you".into()
            }]
        );
        // 能挂起的宿主：permission_prompt 只是确认。
        assert_eq!(
            CAN.parse(
                &json!({"hook_event_name":"Notification","notification_type":"permission_prompt"})
            ),
            vec![]
        );
        assert_eq!(CAN.parse(&json!({"hook_event_name":"Nope"})), vec![]);
    }

    #[test]
    fn host_self_recognition_follows_grok_env_only() {
        assert!(CAN.host_matches_env(false));
        assert!(!CAN.host_matches_env(true));
        assert!(CANNOT.host_matches_env(true));
        assert!(!CANNOT.host_matches_env(false));
    }

    #[test]
    fn only_permission_requests_of_capable_hosts_hold() {
        let pr = json!({ "hook_event_name": "PermissionRequest", "tool_use_id": "t1" });
        assert_eq!(CAN.hold_key(&pr).as_deref(), Some("t1"));
        assert_eq!(CANNOT.hold_key(&pr), None);
        assert_eq!(CAN.hold_key(&json!({ "hook_event_name": "Stop" })), None);
        // 2.1.260 实测：没有 tool_use_id，按 tool_name 键；什么都没有才用事件名占位。
        assert_eq!(
            CAN.hold_key(&json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" }))
                .as_deref(),
            Some("Bash")
        );
        assert_eq!(
            CAN.hold_key(&json!({ "hook_event_name": "PermissionRequest" }))
                .as_deref(),
            Some("PermissionRequest")
        );
    }

    #[test]
    fn releases_follow_adr_002_d5() {
        assert_eq!(
            generic_release(
                &json!({ "hook_event_name": "PostToolUse", "tool_use_id": "t1", "tool_name": "Bash" })
            ),
            Release::ToolUse(vec!["t1".into(), "Bash".into()])
        );
        assert_eq!(
            generic_release(&json!({ "hookEventName": "PermissionDenied", "toolUseId": "t2" })),
            Release::ToolUse(vec!["t2".into()])
        );
        assert_eq!(
            generic_release(&json!({ "hook_event_name": "Stop" })),
            Release::Session
        );
        assert_eq!(
            generic_release(&json!({ "hook_event_name": "SessionEnd" })),
            Release::Session
        );
        assert_eq!(
            generic_release(&json!({ "hook_event_name": "PreToolUse" })),
            Release::None
        );
    }

    #[test]
    fn decision_output_is_the_documented_shape_or_nothing() {
        let out = CAN.decision_output(&Decision::Allow).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let out = CAN
            .decision_output(&Decision::Deny {
                message: Some("no".into()),
            })
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["message"], "no");
        assert_eq!(CAN.decision_output(&Decision::None), None);
        assert_eq!(CANNOT.decision_output(&Decision::Allow), None);
    }
}
