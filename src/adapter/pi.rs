//! pi adapter（agora-c3i；MISSION §0.1「备选一等」；ADR-002 D2/D4/D7）。
//!
//! pi 没有"往 JSON 里拼 hook 条目"的配置形态：它的挂点是扩展（TypeScript，`~/.pi/agent/extensions/`
//! 下的文件由 pi 在启动时加载）。本仓的 [`agora.ts`](agora) 由 `agora hooks install pi` 写进那个
//! 目录，扩展把生命周期事件按本模块认得的形态投给 `agora hook --host pi`——事件表按 1.0.4 实测
//! （2026-10-07 本机真录，`testdata/pi/1.0.4/hooks/`），与 Claude / Codex / Grok 三家一样走
//! 投递箱、receivers 与外部会话登记，核心层不知道 pi 的存在。
//!
//! 实测要点（2026-10-07，pi 1.0.4）：
//! - 事件序（`pi -p` 一轮）：`session_start` → `before_agent_start` → `agent_start` → `turn_start`
//!   →（工具轮）`tool_execution_start/end` → `turn_end` → …→ `agent_end` → `agent_before_settle`
//!   → `agent_settled` → `session_shutdown(reason=quit)`。`turn_end` 每个模型轮都发（一轮里多次），
//!   **`agent_settled`** 才是"pi 不会再自动继续"的轮次边界（重试 / 压缩 / 排队都结束了）。
//! - `--version` 输出 `1.0.4`（裸 `x.y.z`）；`--session-id <uuid>` 钉住对话 id（实测起的会话 id
//!   逐字相同），`--session <id>` 续已有会话（部分 uuid 也认）。无头一轮用 `-p`，交互会话把首条
//!   prompt 当位置参数（`pi '<prompt>'`）。
//! - 扩展自报 `mode` ∈ `tui | rpc | json | print`：`print` / `json` 是无头一次性（判 `headless`，
//!   收进折叠区、不通知、满 24 h 删），`tui` / `rpc` 照常按 `external` 登记。
//! - 扩展是**旁观者**：没有权限代答的能力（pi 的工具调用默认不问人），`decision_via_hook()` 为
//!   false（ADR-002 D2「没有的能力就写没有」）；无运行时句柄，手机侧的深链与状态照常，回复要回终端。
//!
//! 进程号：扩展 spawn hook 时显式带 `PI_PID`（`process.pid`），信封按 `PI_` 前缀收进 `agent_env`，
//! 本 adapter 只认它——不拿 `ppid` 兜底，免得手工敲的 `agora hook --host pi` 把 shell 当成会话本体。

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::status::AgoraEvent;

use super::hooks::{self, event_name, str_of, Decision, Release};
use super::{
    Adapter, AgentFallback, AgentHooks, AgentIdentity, FileInstall, HookInstall, Version,
    VersionProbe,
};

pub struct Pi;

pub static PI: Pi = Pi;

/// 安装到 pi 的扩展文件名（相对用户 HOME）。
pub const EXTENSION_FILE: &str = ".pi/agent/extensions/agora.ts";
/// 文件里属于 agora 的标记：`agora hooks install pi` 生成的内容带它，uninstall 只删带它的文件。
pub const EXTENSION_MARKER: &str = "agora hooks install pi";
/// 扩展模板；安装时把 `{{AGORA}}` / `{{HOME}}` 替换成绝对路径。
const EXTENSION_TEMPLATE: &str = include_str!("pi/agora.ts");

/// 版本表（ADR-002 D7）：每行是"从这个版本起已知的能力"。1.0.4 是实测过 resume / pin 与事件表的
/// 版本；更低的版本我们没验证过，报不可解析。
const VERSION_TABLE: &[(Version, Capabilities)] = &[(
    Version(1, 0, 4),
    Capabilities {
        resume: true,
        pin: true,
    },
)];

#[derive(Debug, Clone, Copy)]
struct Capabilities {
    /// `--session <id>`：按自报的对话 id 续上。
    resume: bool,
    /// `--session-id <uuid>`：起会话时钉死对话 id。
    pin: bool,
}

fn capabilities(v: Version) -> Option<Capabilities> {
    VERSION_TABLE
        .iter()
        .rev()
        .find(|(min, _)| v >= *min)
        .map(|(_, c)| *c)
}

/// JS 字符串字面量（安装时把路径嵌进扩展）。路径里出现引号 / 反斜杠 / 换行的可能性极低，
/// 但嵌入式生成最忌讳"几乎不可能"。
fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl AgentIdentity for Pi {
    fn name(&self) -> &str {
        "pi"
    }

    fn default_command(&self) -> &str {
        "pi"
    }

    fn match_process(&self, argv: &[&str]) -> bool {
        super::program_is(argv, &["pi"])
    }

    /// 实测形态 `1.0.4`（裸 `x.y.z`，不带产品名）。
    fn version(&self, output: &str) -> VersionProbe {
        match Version::find_in(output.trim()) {
            Some(v) if capabilities(v).is_some() => VersionProbe::Available(v),
            Some(v) => VersionProbe::Unparsable(format!(
                "{v} 低于版本表首项 {}，能力未验证",
                VERSION_TABLE[0].0
            )),
            None => VersionProbe::Unparsable(output.trim().to_owned()),
        }
    }

    fn resume_args(&self, version: Version, agent_session_id: &str) -> Option<Vec<String>> {
        capabilities(version)
            .filter(|c| c.resume)
            .map(|_| vec!["--session".to_owned(), agent_session_id.to_owned()])
    }

    fn pin_args(&self, version: Version, new_id: &str) -> Option<Vec<String>> {
        capabilities(version)
            .filter(|c| c.pin)
            .map(|_| vec!["--session-id".to_owned(), new_id.to_owned()])
    }

    // headless_args 暂不实现：`pi -p` 确实是无头单轮（上面实测），但 tests/hook_smoke.rs 的版本
    // 漂移守卫要求 testdata/pi/<版本>/hooks/headless.jsonl，而 tests/fixtures_replay.rs 的目录规则
    // 不允许"只录冒烟"的版本目录长期存在（每个 agent 至少留一个七场景齐全的实测目录）。真录七个
    // 交互场景是独立的一批活（agora-c3i.3），在那之前不把冒烟守卫点着——否则开发机上 `--ignored`
    // 一跑就是红。

    /// 交互会话把首条 prompt 当位置参数（`pi [options] [--] [@files...] [messages...]`）。
    fn initial_prompt_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec![prompt.to_owned()])
    }
}

impl AgentFallback for Pi {}

impl Adapter for Pi {
    fn hooks(&self) -> Option<&dyn AgentHooks> {
        Some(self)
    }
}

impl AgentHooks for Pi {
    fn host(&self) -> &str {
        "pi"
    }

    /// pi 不吃 JSON 条目（走 [`Self::file_install`]），这里交空表——`hosts()` / `for_host()` 按
    /// `host()` 认它，安装器见到 `file_install` 有值就走文件式那条路。
    fn install_spec(&self) -> Vec<HookInstall> {
        Vec::new()
    }

    fn file_install(&self, agora_home: &Path) -> Option<FileInstall> {
        let bin = agora_home.join("bin").join("agora");
        let content = EXTENSION_TEMPLATE
            .replace("{{AGORA}}", &js_string(&bin.to_string_lossy()))
            .replace("{{HOME}}", &js_string(&agora_home.to_string_lossy()));
        Some(FileInstall {
            file: Path::new(EXTENSION_FILE).to_path_buf(),
            marker: EXTENSION_MARKER.to_owned(),
            content,
        })
    }

    /// 没有权限代答：pi 的工具调用默认不问人，扩展也不替用户批准（ADR-002 D2）。
    fn decision_via_hook(&self) -> bool {
        false
    }

    /// pi 的 hook 只由本仓的扩展发起（`--host pi` 是它写死的），不靠环境猜宿主——不像 Grok 会
    /// 重放 Claude 的条目。返回 true 让继承来的 `GROK_SESSION_ID`（在 Grok 里起 pi 时会传下来）
    /// 不至于把事件丢掉。
    fn host_matches_env(&self, _has_grok_session: bool) -> bool {
        true
    }

    fn agent_session_id(&self, payload: &Value) -> Option<String> {
        hooks::session_id(payload)
    }

    fn working_directory(&self, payload: &Value) -> Option<std::path::PathBuf> {
        hooks::cwd(payload)
    }

    /// 扩展 spawn 时写进环境的 `PI_PID`。只认它，不拿 `ppid` 兜底（见模块头）。
    fn agent_pid(&self, env: &BTreeMap<String, String>, _ppid: u32) -> Option<u32> {
        env.get("PI_PID")?.trim().parse().ok()
    }

    /// `-p` / `--mode json` 是无头一次性（脚本、冒烟、宿主子任务）：照常登记成 `headless`——不通知、
    /// 收进折叠区、满 24 h 不论状态即删。`tui` / `rpc` 是人的会话（或长期被外部驱动的会话），按
    /// `external` 登记。判据只看载荷结构（`mode`），缺字段一律不判无头。
    fn is_headless(&self, payload: &Value) -> bool {
        event_name(payload) == Some("session_start")
            && matches!(str_of(payload, &["mode"]), Some("print" | "json"))
    }

    fn parse(&self, payload: &Value) -> Vec<AgoraEvent> {
        let mut out = Vec::new();
        match event_name(payload) {
            Some("session_start") => {
                out.push(AgoraEvent::SessionStarted);
                if let Some(id) = hooks::session_id(payload) {
                    out.push(AgoraEvent::SessionId(id));
                }
            }
            Some("before_agent_start") => {
                out.push(hooks::prompt_event(
                    str_of(payload, &["prompt"]).unwrap_or_default(),
                ));
            }
            Some("tool_execution_start") => {
                out.push(AgoraEvent::Activity(
                    str_of(payload, &["tool_name"]).unwrap_or("tool").to_owned(),
                ));
            }
            // agent_settled = 这一轮做完了（pi 不会再自动继续）。
            Some("agent_settled") => out.push(AgoraEvent::TurnEnded(
                str_of(payload, &["last_assistant_message"]).map(str::to_owned),
            )),
            // 只有真的退出才算结束；reload / new / resume / fork 之后这个进程还在跑（会话换了或
            // 扩展热重载），结束那条旧行是 supersede / 进程探活的事。
            Some("session_shutdown") if str_of(payload, &["reason"]) == Some("quit") => {
                out.push(AgoraEvent::SessionEnded(Some("quit".to_owned())));
            }
            _ => {}
        }
        out
    }

    /// 没有挂起：pi 的工具调用不问人（见 [`Self::decision_via_hook`]）。三个写回方法都交"不做"。
    fn hold_key(&self, _payload: &Value) -> Option<String> {
        None
    }

    fn release_for(&self, _payload: &Value) -> Release {
        Release::None
    }

    fn decision_output(&self, _decision: &Decision) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(event: &str) -> Value {
        json!({
            "hook_event_name": event,
            "session_id": "01a1180a-cdef-77db-84bf-ce5bc2c24aa9",
            "session_file": "/home/u/.pi/agent/sessions/x.jsonl",
            "cwd": "/home/u/code/agora",
            "mode": "tui",
        })
    }

    #[test]
    fn event_table_maps_lifecycle_to_agora_events() {
        assert_eq!(
            PI.parse(&payload("session_start")),
            vec![
                AgoraEvent::SessionStarted,
                AgoraEvent::SessionId("01a1180a-cdef-77db-84bf-ce5bc2c24aa9".into())
            ]
        );
        let mut p = payload("before_agent_start");
        p["prompt"] = json!("把 config 迁到 yaml");
        assert_eq!(
            PI.parse(&p),
            vec![AgoraEvent::PromptSubmitted("把 config 迁到 yaml".into())]
        );
        let mut p = payload("tool_execution_start");
        p["tool_name"] = json!("bash");
        assert_eq!(PI.parse(&p), vec![AgoraEvent::Activity("bash".into())]);
        let mut p = payload("agent_settled");
        p["last_assistant_message"] = json!("好了");
        assert_eq!(
            PI.parse(&p),
            vec![AgoraEvent::TurnEnded(Some("好了".into()))]
        );
        let mut p = payload("session_shutdown");
        p["reason"] = json!("quit");
        assert_eq!(
            PI.parse(&p),
            vec![AgoraEvent::SessionEnded(Some("quit".into()))]
        );
        // reload / new / resume / fork 不算结束（同一进程还在跑）。
        for reason in ["reload", "new", "resume", "fork"] {
            let mut p = payload("session_shutdown");
            p["reason"] = json!(reason);
            assert_eq!(PI.parse(&p), Vec::<AgoraEvent>::new(), "{reason}");
        }
        // 不认识的（agent_end / turn_end 等）不出事件：turn_end 每个模型轮都发。
        assert_eq!(PI.parse(&payload("turn_end")), Vec::<AgoraEvent>::new());
        assert_eq!(PI.parse(&payload("agent_end")), Vec::<AgoraEvent>::new());
    }

    #[test]
    fn headless_is_print_and_json_only_and_only_on_session_start() {
        for mode in ["print", "json"] {
            let mut p = payload("session_start");
            p["mode"] = json!(mode);
            assert!(PI.is_headless(&p), "{mode}");
        }
        for mode in ["tui", "rpc"] {
            let mut p = payload("session_start");
            p["mode"] = json!(mode);
            assert!(!PI.is_headless(&p), "{mode}");
        }
        // 缺 mode 不判无头（误判的代价不对称：headless 行不通知、满 24 h 即删）。
        let mut p = payload("session_start");
        p.as_object_mut().unwrap().remove("mode");
        assert!(!PI.is_headless(&p));
        // 只在 SessionStart 上问。
        let mut p = payload("agent_settled");
        p["mode"] = json!("print");
        assert!(!PI.is_headless(&p));
    }

    #[test]
    fn pid_comes_from_pi_pid_env_only() {
        let mut env = BTreeMap::new();
        env.insert("PI_PID".to_owned(), "3500346".to_owned());
        assert_eq!(PI.agent_pid(&env, 1), Some(3500346));
        // 没有 PI_PID：不拿 ppid 兜底（手工敲的 hook 的父进程是 shell，不能当会话本体）。
        assert_eq!(PI.agent_pid(&BTreeMap::new(), 999), None);
    }

    #[test]
    fn version_table_gates_resume_and_pin() {
        assert_eq!(
            PI.version("1.0.4\n"),
            VersionProbe::Available(Version(1, 0, 4))
        );
        assert!(matches!(PI.version("1.0.3"), VersionProbe::Unparsable(_)));
        assert!(matches!(PI.version("nope"), VersionProbe::Unparsable(_)));
        assert_eq!(
            PI.resume_args(Version(1, 0, 4), "abc"),
            Some(vec!["--session".into(), "abc".into()])
        );
        assert_eq!(
            PI.pin_args(Version(1, 0, 4), "abc"),
            Some(vec!["--session-id".into(), "abc".into()])
        );
        // headless_args 暂缺（见上），只钉首条 prompt 这类真在用的能力。
        assert_eq!(PI.initial_prompt_args("hi"), Some(vec!["hi".into()]));
        assert!(PI.accepts_initial_prompt());
    }

    #[test]
    fn extension_template_is_installed_with_absolute_paths_and_a_marker() {
        let fi = PI.file_install(Path::new("/home/u/.agora")).unwrap();
        assert_eq!(fi.file, Path::new(".pi/agent/extensions/agora.ts"));
        assert!(fi.content.contains(EXTENSION_MARKER));
        assert!(fi.content.contains("\"/home/u/.agora/bin/agora\""));
        assert!(fi.content.contains("\"/home/u/.agora\""));
        assert!(!fi.content.contains("{{AGORA}}") && !fi.content.contains("{{HOME}}"));
        assert!(PI.install_spec().is_empty(), "pi 不走 JSON 条目安装");
    }

    #[test]
    fn js_string_escapes_quotes_and_backslashes() {
        assert_eq!(js_string(r#"/a"b\c"#), r#""/a\"b\\c""#);
        assert_eq!(js_string("/plain/path"), "\"/plain/path\"");
    }
}
