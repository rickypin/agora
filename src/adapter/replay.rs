//! fixture 回放器（ADR-002 D10）：把 `testdata/<agent>/<version>/hooks/<scenario>.jsonl` 喂给
//! Adapter + 状态机，断言状态序列与 `source` / `confidence`。
//!
//! 每行一个 JSON 对象，字段：
//! - `payload`：agent 发来的原始 hook payload（`agora hook --record` 录下的，agora-3la.1）；
//! - `respond`：合成的 Dashboard 答复 `{"tool_use_id": "...", "behavior": "allow"|"deny"}`
//!   ——不是 agent 发的，所以不会出现在录制里，回放时按 `decision.resolved` 应用；
//! - `at`：相对秒数（缺省沿用上一行）；
//! - `hold`：这条 payload 该不该挂起（`AgentHooks::hold_key`）；
//! - `expect`：这一行应用后的 `{"status", "source"?, "min_confidence"?, "detail"?, "prompt"?}`
//!   （`prompt` 是 `❯` 行，写 `null` 断言它为空）。
//!
//! 只走 hook 路径：进程层 / 文本层不在此（那是 `tests/state_machine.rs` 的事）。

use serde::Deserialize;

use crate::status::{AgoraEvent, Assessment, Machine, MachineConfig, Source, Status};

use super::AgentHooks;

#[derive(Debug, Deserialize)]
struct Line {
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    respond: Option<Respond>,
    #[serde(default)]
    at: Option<i64>,
    #[serde(default)]
    hold: Option<bool>,
    #[serde(default)]
    expect: Option<Expect>,
}

#[derive(Debug, Deserialize)]
struct Respond {
    tool_use_id: String,
}

#[derive(Debug, Deserialize)]
pub struct Expect {
    pub status: Status,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub min_confidence: Option<f32>,
    #[serde(default)]
    pub detail: Option<String>,
    /// `❯` 行（`Machine::prompt`）：外层 None = 不检查，`Some(None)` = 必须为空。
    #[serde(default, deserialize_with = "deserialize_some")]
    pub prompt: Option<Option<String>>,
}

/// serde 对 `Option<Option<T>>` 的缺省是"缺键与 null 都是 None"；这里要把 `"prompt": null`
/// 与"没写 prompt"分开，所以只要键在就包一层 Some。
fn deserialize_some<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("第 {line} 行不是合法 JSON: {source}")]
    Parse {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("第 {line} 行既没有 payload 也没有 respond")]
    Empty { line: usize },
    #[error("第 {line} 行挂起判定不符：期望 {expected}，实际 {actual}")]
    Hold {
        line: usize,
        expected: bool,
        actual: bool,
    },
    #[error("第 {line} 行状态不符（{what}）：期望 {expected}，实际 {actual:?}")]
    Mismatch {
        line: usize,
        what: &'static str,
        expected: String,
        actual: Assessment,
    },
}

/// 回放一份 fixture。返回每一行应用后的结论（含没写 `expect` 的行）。
pub fn replay(hooks: &dyn AgentHooks, text: &str) -> Result<Vec<Assessment>, ReplayError> {
    let mut machine = Machine::new(MachineConfig::default(), true, 1, 0);
    let mut now = 0;
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        if raw.trim().is_empty() || raw.trim_start().starts_with('#') {
            continue;
        }
        let line: Line = serde_json::from_str(raw).map_err(|source| ReplayError::Parse {
            line: line_no,
            source,
        })?;
        now = line.at.unwrap_or(now);
        let events: Vec<AgoraEvent> = match (&line.payload, &line.respond) {
            (Some(payload), _) => {
                let held = hooks.hold_key(payload).is_some();
                if let Some(expected) = line.hold {
                    if expected != held {
                        return Err(ReplayError::Hold {
                            line: line_no,
                            expected,
                            actual: held,
                        });
                    }
                }
                hooks.parse(payload)
            }
            (None, Some(r)) => vec![AgoraEvent::DecisionResolved(Some(r.tool_use_id.clone()))],
            (None, None) => return Err(ReplayError::Empty { line: line_no }),
        };
        for e in &events {
            machine.apply(e, 1, now);
        }
        let current = machine.current().clone();
        if let Some(exp) = &line.expect {
            let mismatch = |what, expected: String| ReplayError::Mismatch {
                line: line_no,
                what,
                expected,
                actual: current.clone(),
            };
            if current.status != exp.status {
                return Err(mismatch("status", format!("{:?}", exp.status)));
            }
            if let Some(src) = exp.source {
                if current.source != src {
                    return Err(mismatch("source", format!("{src:?}")));
                }
            }
            if let Some(min) = exp.min_confidence {
                if current.confidence < min {
                    return Err(mismatch("confidence", format!("≥ {min}")));
                }
            }
            if let Some(d) = &exp.detail {
                if machine.detail() != Some(d.as_str()) {
                    return Err(mismatch(
                        "detail",
                        format!("{d:?} (got {:?})", machine.detail()),
                    ));
                }
            }
            if let Some(p) = &exp.prompt {
                if machine.prompt() != p.as_deref() {
                    return Err(mismatch(
                        "prompt",
                        format!("{p:?} (got {:?})", machine.prompt()),
                    ));
                }
            }
        }
        out.push(current);
    }
    Ok(out)
}
