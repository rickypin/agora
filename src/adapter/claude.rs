//! Claude Code adapter（ADR-002 D2/D4/D7；agora-dvh.5）。
//!
//! 事件表按 2.1.258 / 2.1.260 实测的 payload 键集合（agora-90t.3 注记；2026-09-04 真录）与
//! Claude Code hooks 文档：键全是 snake_case，`session_id` 是对话 id，`transcript_path` 只存
//! 路径不解析（D8）。实测反直觉处：`PermissionRequest` 没有 `tool_use_id`，只有 `tool_name`。
//! fixture 在 `testdata/claude/<version>/hooks/`，回放器见 `super::replay`。
//!
//! 宿主退出时发不发 `SessionEnd`（2026-09-08 本机 2.1.x，在独立终端里起 CLI 实测；三家对照见
//! `codex.rs` / `grok.rs`，原始记录 `bd memories external-exit-hooks-ctrlc-vs-hup`）：
//!
//! | 怎么退 | 事件 | agora 上的结果 |
//! |---|---|---|
//! | 提示符上两次 Ctrl+C | `SessionEnd(reason = prompt_input_exit)` | FINISHED（hook，`session ended (hook)`）|
//! | 关窗口（终端给 SIGHUP） | `SessionEnd(reason = other)` | 同上 |
//! | `/clear` | `SessionEnd(reason = clear)` + 新 id 的 `SessionStart(source = clear)` | 同一行不改状态；无句柄 external 行另起一行、旧行结束（agora-s3r）|
//! | `/resume`、`/logout` | `SessionEnd(reason = resume / logout)` | FINISHED，resume 之后同一进程再发 SessionStart 回 STARTING |
//!
//! 两种退法 hook 都到，所以 Claude 的 external 行 reason 是 `session ended (hook)` 就说明 SessionEnd
//! 到过、只是分不出人按的还是窗口关的；`external process gone` 才是 hook 没来、只剩探活（agora-rzh）。

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::status::AgoraEvent;

use super::hooks::{
    self, decision_key, event_name, generic_hold_key, generic_release, permission_output,
    permission_summary, release_keys, str_of, tool_use_id, Decision, Release,
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

    /// `claude -p <prompt>`：实测 2.1.261（2026-09-05）无头一轮照样 fire SessionStart /
    /// UserPromptSubmit / Stop / SessionEnd。
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["-p".to_owned(), prompt.to_owned()])
    }

    /// `claude '<prompt>'`：位置参数起交互会话并把它当第一条指令（2.1.261 实测，2026-09-06）。
    /// 不是 `-p`——那是无头单轮，答完就退出，会话就没了。
    fn initial_prompt_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec![prompt.to_owned()])
    }
}

impl AgentFallback for Claude {}

impl Adapter for Claude {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        Some(self)
    }
}

/// 交互模式的 SessionStart 会带、无头一轮不带的键（实测名单见 [`AgentHooks::is_headless`]）。
/// 这是减法判据：载荷里出现名单中任何一个键就当它是交互会话，认不出来的形状一律不判成无头——
/// 漏判只少一个折叠与一次早删，误判会把一条人的会话推进"不通知 + 满 24 h 不论状态即删"。
const INTERACTION_ONLY_KEYS: &[&str] = &["model", "scratchpad_dir", "permission_mode", "effort"];

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

    /// 无头一次性会话（`claude -p`）的 SessionStart 只剩公共键，交互模式才带的四个键一个都没有：
    /// `model` / `scratchpad_dir` / `permission_mode` / `effort`（裁决 agora-5gg.7 选 B；判据只用载荷结构）。
    ///
    /// 实录（`testdata/claude/<版本>/hooks/`）：
    /// - 无头 2.1.261 / 2.1.270：SessionStart = `session_id / transcript_path / cwd / hook_event_name / source`
    ///   五个键（`headless.jsonl` 头注；`task_notification.jsonl` 也是 `claude -p` 录的，同形）。
    /// - 交互 2.1.261 全套：`model` + `scratchpad_dir`；2.1.260 真录（`ask_user_question.jsonl`）：`model`。
    ///
    /// 为什么不只问 `model` / `scratchpad_dir`（决策原文那两个键）：`testdata/claude/2.1.260/hooks/`
    /// 里 `turn_complete.jsonl` / `clear.jsonl` 两条合成的七场景 fixture 也缺这两个键，却带着
    /// `permission_mode` / `effort`（`docs/adr/ADR-002-state-source-layering.md` 的 2.1.258 无头实测把
    /// 它们列为公共键）。只看那两个键会把这类形状判成无头，而误判的代价不对称：headless 行不通知、
    /// 满 24 h 不论状态就删（把一条人的会话当工具体静默抹掉）。四个键都在就当作交互会话，判不出来
    /// 的照常按 `external` 登记——漏判只少一个折叠与早删，误判会吞掉一行。新版本真录到无头 SessionStart
    /// 带 `permission_mode` / `effort` 时，把这两个键从名单里摘掉（回退到只看 model / scratchpad_dir）。
    fn is_headless(&self, payload: &Value) -> bool {
        // 只认 SessionStart：其余事件两种模式逐键一致（2.1.270 头注：Stop 照样带
        // background_tasks / session_crons），拿它们判就是猜。
        event_name(payload) == Some("SessionStart")
            && INTERACTION_ONLY_KEYS
                .iter()
                .all(|k| payload.get(*k).is_none())
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
            Some("UserPromptSubmit") => out.push(hooks::prompt_event(
                str_of(payload, &["prompt"]).unwrap_or_default(),
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
            // 摘要带 tool_input 的主参数（"Bash: git push"），不是光秃秃的工具名：Dashboard 上
            // 要看得出 agent 想干什么才能答 Allow / Deny（agora-pzi）。
            Some("PermissionRequest") => out.push(AgoraEvent::DecisionNeeded {
                tool_use_id: decision_key(payload),
                summary: permission_summary(payload),
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
            // 文档写的是 error_type，2.1.261 实测（2026-09-05，testdata/claude/2.1.261/hooks/
            // api_error.jsonl）发来的键叫 error（值仍是那 11 个枚举），两个都认。
            Some("StopFailure") => out.push(AgoraEvent::TurnFailed(
                str_of(payload, &["error_type", "error", "matcher"])
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
    fn injected_prompts_are_not_the_users_words() {
        // agora-3s5：2.1.261 真录（testdata/claude/2.1.261/hooks/task_notification.jsonl）——后台任务
        // 完成通知以 UserPromptSubmit 发出，prompt 以 <task-notification> 开头；它不是人敲的。
        let injected = json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "<task-notification>\n<task-id>x</task-id>\n</task-notification>"
        });
        assert_eq!(CLAUDE.parse(&injected), vec![AgoraEvent::PromptInjected]);
        let reminder =
            json!({ "hook_event_name": "UserPromptSubmit", "prompt": "  <system-reminder>\nfoo" });
        assert_eq!(CLAUDE.parse(&reminder), vec![AgoraEvent::PromptInjected]);
        let slash = json!({ "hook_event_name": "UserPromptSubmit", "prompt": "<command-name>/clear</command-name>" });
        assert_eq!(CLAUDE.parse(&slash), vec![AgoraEvent::PromptInjected]);
        let human =
            json!({ "hook_event_name": "UserPromptSubmit", "prompt": "把 config 迁到 yaml" });
        assert_eq!(
            CLAUDE.parse(&human),
            vec![AgoraEvent::PromptSubmitted("把 config 迁到 yaml".into())]
        );
        // 人贴的 HTML / 尖括号不算：只认已知标签。
        let html = json!({ "hook_event_name": "UserPromptSubmit", "prompt": "<div> 这个怎么居中" });
        assert!(matches!(
            CLAUDE.parse(&html)[0],
            AgoraEvent::PromptSubmitted(_)
        ));
    }

    /// 逐条 fixture 的第一条 SessionStart：无头录制判 headless，交互录制不判。
    #[test]
    fn headless_shape_is_read_from_every_recorded_session_start() {
        // 守卫：把 INTERACTION_ONLY_KEYS 掏成空名单 → 所有 SessionStart 都判 headless，
        // interactive 那一批红；去掉 `event_name == SessionStart` 那道门 → 无头 fixture 里的
        // UserPromptSubmit / Stop（与交互逐键同形）也判 headless，下面的“只认 SessionStart”红。
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/claude");
        let mut headless: Vec<String> = Vec::new();
        let mut interactive: Vec<String> = Vec::new();
        for path in fixture_paths(&root) {
            let rel = path
                .strip_prefix(root.parent().unwrap())
                .unwrap()
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            match first_session_start(&path).as_ref() {
                Some(p) if CLAUDE.is_headless(p) => headless.push(rel),
                Some(_) => interactive.push(rel),
                // 没录 SessionStart 的文件（2.1.260 的 api_error / interrupted …）不进门。
                None => {}
            }
        }
        headless.sort();
        interactive.sort();
        assert_eq!(
            headless,
            vec![
                // 三条真录的无头会话：2.1.270 冒烟、2.1.261 冒烟，以及 2.1.261 用 `claude -p`
                // 录的后台任务通知（头注：“无头模式 payload 没有 scratchpad_dir”）。
                "claude/2.1.261/hooks/headless.jsonl",
                "claude/2.1.261/hooks/task_notification.jsonl",
                "claude/2.1.270/hooks/headless.jsonl",
            ],
            "被判成无头的 fixture 名单变了：新录了无头场景就补进来；如果是真交互录制被判成了\
             无头，那是误判——headless 行不通知、满 24 h 不论状态即删"
        );
        // 交互 fixture 全部落在另一边：2.1.261 真录带 model + scratchpad_dir，2.1.260 真录
        // （ask_user_question）只带 model，2.1.260 合成的两条带 permission_mode + effort。
        assert!(
            interactive.len() >= 10,
            "交互 fixture 一条都没判出来？{interactive:?}"
        );
        for name in [
            "claude/2.1.261/hooks/turn_complete.jsonl",
            "claude/2.1.261/hooks/clear.jsonl",
            "claude/2.1.260/hooks/ask_user_question.jsonl",
            "claude/2.1.260/hooks/turn_complete.jsonl",
            "claude/2.1.260/hooks/clear.jsonl",
        ] {
            assert!(
                interactive.iter().any(|f| f == name),
                "{name} 是交互会话，不能判成 headless"
            );
        }
    }

    #[test]
    fn only_session_start_carries_the_headless_shape() {
        // 无头的其它事件与交互逐键同形，拿它们判就会猜：2.1.270 真录的 UserPromptSubmit 带
        // permission_mode（交互也有），不是无头信号。
        let prompt = json!({"hook_event_name":"UserPromptSubmit","session_id":"s",
            "prompt":"<prompt>","permission_mode":"default"});
        assert!(
            !CLAUDE.is_headless(&prompt),
            "UserPromptSubmit 不是 SessionStart：再像无头也不判"
        );
        let stop = json!({"hook_event_name":"Stop","session_id":"s"});
        assert!(
            !CLAUDE.is_headless(&stop),
            "缺全部交互键的 Stop 也不算：判据只认 SessionStart"
        );
        // 形状完整但缺交互键的 SessionStart 才算。
        let bare = json!({"hook_event_name":"SessionStart","session_id":"s","source":"startup"});
        assert!(CLAUDE.is_headless(&bare));
        // 四个交互键中任一个在→ 不判（宁漏不误）。
        for key in INTERACTION_ONLY_KEYS {
            let mut p = bare.clone();
            p[key] = json!("x");
            assert!(!CLAUDE.is_headless(&p), "{key} 在载荷里：这不能算无头");
        }
    }

    fn fixture_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for version in std::fs::read_dir(root).unwrap().flatten() {
            let hooks = version.path().join("hooks");
            if !hooks.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&hooks).unwrap().flatten() {
                if f.path().extension().is_some_and(|e| e == "jsonl") {
                    out.push(f.path());
                }
            }
        }
        out.sort();
        out
    }

    /// 一个 fixture 里第一条 SessionStart 的 payload（没录到就 None）。
    fn first_session_start(path: &std::path::Path) -> Option<Value> {
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let v: Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let payload = v.get("payload")?;
            if event_name(payload) == Some("SessionStart") {
                return Some(payload.clone());
            }
        }
        None
    }

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
    fn permission_request_summary_carries_the_tool_input() {
        // agora-pzi：2.1.260 的 PermissionRequest 没有 tool_use_id 但有 tool_input，摘要要用上。
        let bash = json!({"hook_event_name":"PermissionRequest","tool_name":"Bash",
            "tool_input":{"command":"git push origin main"},"permission_suggestions":[]});
        assert_eq!(
            CLAUDE.parse(&bash),
            vec![AgoraEvent::DecisionNeeded {
                tool_use_id: "Bash".into(),
                summary: "Bash: git push origin main".into()
            }]
        );
        let write = json!({"hook_event_name":"PermissionRequest","tool_name":"Write",
            "tool_input":{"file_path":"/work/agora/README.md","content":"..."}});
        assert_eq!(
            CLAUDE.parse(&write),
            vec![AgoraEvent::DecisionNeeded {
                tool_use_id: "Write".into(),
                summary: "Write: /work/agora/README.md".into()
            }]
        );
        // 没有 tool_input 退回工具名——挂起键不变，Dashboard 的 respond 仍按 tool_name 对上。
        let bare = json!({"hook_event_name":"PermissionRequest","tool_name":"Bash"});
        assert_eq!(
            CLAUDE.parse(&bare),
            vec![AgoraEvent::DecisionNeeded {
                tool_use_id: "Bash".into(),
                summary: "Bash".into()
            }]
        );
        assert_eq!(CLAUDE.hold_key(&bash).as_deref(), Some("Bash"));
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
        // 2.1.261 真录的键名。
        assert_eq!(
            CLAUDE.parse(&json!({"hook_event_name":"StopFailure","error":"server_error"})),
            vec![AgoraEvent::TurnFailed("server_error".into())]
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
            .all(|h| h.file.as_os_str() == ".claude/settings.json"));
    }
}
