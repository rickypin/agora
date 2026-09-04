//! 每个会话一台状态机：四层来源仲裁、驻留时间、hook 沉默、epoch（ADR-002 D1）。
//!
//! 两个入口：[`Machine::apply`] 吃 hook 事件（立即生效），[`Machine::observe`] 每 tick 吃
//! 进程事实 + 文本判定 + 活动样本，吐出当前 [`Assessment`]。裁决顺序：
//!
//! 1. 进程退出压倒一切：FINISHED / FAILED 之后 hook 事件只当 metadata。
//! 2. 有 hook 的会话：WAITING / TURN_DONE 只来自 hook；文本层永远抬不上去，活动层不产生 IDLE。
//!    hook 沉默（`silence_after` 无事件）而屏幕像在等人 → UNKNOWN `hooks silent`，不猜 WAITING。
//! 3. 无 hook 的会话：文本 WAITING 要连续 2 tick 一致（conf ≤ 0.8）；`idle_after` 无输出 → IDLE
//!    （conf 0.6）；输出恢复 → RUNNING。
//! 4. 驻留：低层来源在 `high_hold`（30 s）内不覆盖高层写入的状态；resize / attach / detach
//!    引起的重绘不算活动。
//!
//! "有 hook"不只看 Adapter 的声明：只要收到过一条 hook 事件就按有 hook 处理——采纳的
//! 未知会话装了 hook 的话，事件一到就自动升级，而不是永远靠文本猜。
//!
//! 状态不落库（不变量 7）；daemon 重启后从进程层重新长出来，hook 侧靠投递箱重放补上。

use std::collections::BTreeSet;
use std::time::Duration;

use crate::runtime::{RuntimeSession, Size};

use super::{AgoraEvent, Assessment, DetectionResult, Source, Status};

#[derive(Debug, Clone)]
pub struct MachineConfig {
    /// 无输出多久算 IDLE（`status.idle_after`）。
    pub idle_after: Duration,
    /// hook 多久没声音算沉默（`hooks.silence_after`）。
    pub silence_after: Duration,
    /// 高层写入后多久内低层不得覆盖。
    pub high_hold: Duration,
    /// 文本层 WAITING 需要连续几个 tick 一致。
    pub text_ticks: u32,
    /// 两次文本观测至少隔多久才算两个 tick（`status.detector_interval`）：同一秒内多次读不算。
    pub tick: Duration,
}

impl Default for MachineConfig {
    fn default() -> Self {
        MachineConfig {
            idle_after: Duration::from_secs(60),
            silence_after: Duration::from_secs(600),
            high_hold: Duration::from_secs(30),
            text_ticks: 2,
            tick: Duration::from_secs(2),
        }
    }
}

/// 进程活着与否的三值：外部会话（无运行时）是 Unknown，只有 hook 能说话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

/// 一个 tick 的全部输入。
pub struct Observation<'a> {
    /// 进程层的结论（`process_layer` 或"运行时不可用"的 UNKNOWN）。
    pub process: Assessment,
    pub liveness: Liveness,
    /// 文本层（Adapter 兜底）的判定；没有检测器就是 None。
    pub text: Option<DetectionResult>,
    /// 活动样本来源。
    pub runtime: Option<&'a RuntimeSession>,
    /// 库里的当前 epoch；比状态机记的大就是 Restart 过了。
    pub epoch: i64,
    pub now: i64,
}

/// 来源的层级：只用来比"谁能覆盖谁"。
fn layer(source: Source) -> u8 {
    match source {
        Source::Hook => 3,
        Source::Process => 2,
        Source::Text => 1,
        Source::Activity | Source::None => 0,
    }
}

/// 这个结论享不享受驻留保护：进程层的 RUNNING 是"活着"的默认值而不是一次写入，
/// "还没观测到"/"运行时不可用"的 UNKNOWN 更不是谁写的结论——都不保护。
fn protected(a: &Assessment) -> bool {
    a.source != Source::None && !(a.source == Source::Process && a.status == Status::Running)
}

#[derive(Debug)]
pub struct Machine {
    cfg: MachineConfig,
    declared_hooks: bool,
    heard_hooks: bool,
    epoch: i64,
    current: Assessment,
    set_at: i64,
    last_hook_at: Option<i64>,
    /// 本代进程起始时刻：hook 沉默与 IDLE 的起点（没有任何事件 / 输出时）。
    since: i64,
    text_streak: Option<(Status, u32)>,
    last_text_at: i64,
    /// 最近一次真正的活动（IDLE 的起点）。
    last_output_at: Option<i64>,
    /// 最近看到的输出时刻，含重绘：只用来判断"前进了没有"。
    seen_output_at: Option<i64>,
    last_size: Option<Size>,
    last_attached: Option<bool>,
    /// 最近一次 hook 给的"正在做什么"/ 最后一条回复，给两行预览（dvh.10）。
    detail: Option<String>,
    /// 还没答的权限 / 提问（并行工具各一个）：清空才回 RUNNING。
    pending: BTreeSet<String>,
}

impl Machine {
    pub fn new(cfg: MachineConfig, declared_hooks: bool, epoch: i64, now: i64) -> Self {
        Machine {
            cfg,
            declared_hooks,
            heard_hooks: false,
            epoch,
            current: Assessment::unknown("no observation yet"),
            set_at: now,
            last_hook_at: None,
            since: now,
            text_streak: None,
            last_text_at: 0,
            last_output_at: None,
            seen_output_at: None,
            last_size: None,
            last_attached: None,
            detail: None,
            pending: BTreeSet::new(),
        }
    }

    pub fn has_hooks(&self) -> bool {
        self.declared_hooks || self.heard_hooks
    }

    pub fn current(&self) -> &Assessment {
        &self.current
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub fn epoch(&self) -> i64 {
        self.epoch
    }

    /// Restart：新一代进程，旧状态、旧驻留全部作废。
    fn reset(&mut self, epoch: i64, now: i64) {
        let cfg = self.cfg.clone();
        let declared = self.declared_hooks;
        *self = Machine::new(cfg, declared, epoch, now);
    }

    fn set(&mut self, a: Assessment, now: i64) {
        if a != self.current {
            self.set_at = now;
        }
        self.current = a;
    }

    /// hook 事件立即生效。旧 epoch 的丢弃（返回 false）；进程已退出的只当 metadata。
    pub fn apply(&mut self, event: &AgoraEvent, epoch: i64, now: i64) -> bool {
        if epoch < self.epoch {
            return false;
        }
        if epoch > self.epoch {
            self.reset(epoch, now);
        }
        self.heard_hooks = true;
        self.last_hook_at = Some(now);
        if matches!(self.current.status, Status::Finished | Status::Failed)
            && self.current.source == Source::Process
        {
            return true;
        }
        let hook = |status, conf, reason: Option<&str>| {
            Assessment::new(status, Source::Hook, conf, reason)
        };
        let waiting_on_others = |pending: &BTreeSet<String>, current: &Assessment| {
            current.status == Status::Waiting && !pending.is_empty()
        };
        let next = match event {
            AgoraEvent::SessionStarted => {
                self.pending.clear();
                Some(hook(Status::Starting, 1.0, Some("session started")))
            }
            AgoraEvent::SessionId(_) => None,
            AgoraEvent::PromptSubmitted(p) => {
                self.pending.clear();
                self.detail = Some(p.clone());
                Some(hook(Status::Running, 1.0, Some("prompt submitted")))
            }
            // 并行工具：一个在等权限时另一个的 PreToolUse / PostToolUse 照样到，不算"人答了"。
            AgoraEvent::Activity(_) if waiting_on_others(&self.pending, &self.current) => None,
            AgoraEvent::Activity(what) => {
                self.detail = Some(what.clone());
                Some(hook(Status::Running, 1.0, Some("activity")))
            }
            AgoraEvent::InputNeeded {
                tool_use_id,
                question,
            } => {
                self.pending.insert(tool_use_id.clone());
                self.detail = Some(question.clone());
                Some(hook(Status::Waiting, 0.95, Some("question")))
            }
            AgoraEvent::DecisionNeeded {
                tool_use_id,
                summary,
            } => {
                self.pending.insert(tool_use_id.clone());
                self.detail = Some(summary.clone());
                Some(hook(Status::Waiting, 1.0, Some("permission")))
            }
            AgoraEvent::DecisionResolved(id) => {
                match id {
                    Some(id) => {
                        self.pending.remove(id);
                    }
                    None => self.pending.clear(),
                }
                (self.current.status == Status::Waiting && self.pending.is_empty())
                    .then(|| hook(Status::Running, 0.95, Some("decision resolved")))
            }
            AgoraEvent::TurnEnded(last) => {
                self.pending.clear();
                if last.is_some() {
                    self.detail = last.clone();
                }
                Some(hook(Status::TurnDone, 1.0, Some("turn ended")))
            }
            AgoraEvent::TurnFailed(reason) => {
                self.pending.clear();
                Some(hook(Status::TurnDone, 0.95, Some(reason)))
            }
            // 空闲通知只是 TURN_DONE 的确认 / 补漏：RUNNING 里听到它才改，WAITING 不动。
            AgoraEvent::Idle => (self.current.status == Status::Running)
                .then(|| hook(Status::TurnDone, 0.9, Some("idle"))),
            AgoraEvent::SessionEnded(_) => {
                self.pending.clear();
                None
            }
        };
        if let Some(a) = next {
            self.set(a, now);
        }
        true
    }

    /// 每 tick 一次。返回当前结论。
    pub fn observe(&mut self, obs: Observation<'_>) -> Assessment {
        let now = obs.now;
        if obs.epoch > self.epoch {
            self.reset(obs.epoch, now);
        }
        // 1. 进程退出 / 运行时不可信：压倒一切。
        if obs.liveness == Liveness::Dead || obs.process.source == Source::None {
            if obs.liveness == Liveness::Unknown && self.current.source == Source::Hook {
                // 外部会话：没有进程事实，hook 说什么就是什么。
                return self.current.clone();
            }
            self.set(obs.process, now);
            return self.current.clone();
        }
        let output_changed = self.sample_activity(obs.runtime, now);

        if self.has_hooks() {
            return self.observe_hooked(obs.process, obs.text.as_ref(), now);
        }
        self.observe_unhooked(obs.process, obs.text.as_ref(), output_changed, now)
    }

    /// 活动样本：输出时刻前进才算活动；尺寸或 attach 状态同时变了的那一 tick 不算
    /// （重绘不是 agent 在干活，devcenter 的 `(width,height)` 教训）。
    fn sample_activity(&mut self, rt: Option<&RuntimeSession>, now: i64) -> bool {
        let Some(rt) = rt else {
            return false;
        };
        let redraw = self.last_size.is_some_and(|s| s != rt.size)
            || self.last_attached.is_some_and(|a| a != rt.attached);
        self.last_size = Some(rt.size);
        self.last_attached = Some(rt.attached);
        let Some(at) = rt.output_at else {
            return false;
        };
        let advanced = self.seen_output_at.is_none_or(|prev| at > prev);
        let first = self.seen_output_at.is_none();
        self.seen_output_at = Some(at);
        if first {
            // 第一次看到：记下来，但"从现在起"才开始计 IDLE，不追溯。
            self.last_output_at = Some(at.max(self.since).min(now));
            return false;
        }
        // 重绘：看到的时刻记下（免得下一 tick 又当成新活动），但不是活动，IDLE 起点不动。
        if advanced && !redraw {
            self.last_output_at = Some(at);
            return true;
        }
        false
    }

    fn observe_hooked(
        &mut self,
        process: Assessment,
        text: Option<&DetectionResult>,
        now: i64,
    ) -> Assessment {
        let quiet_since = self.last_hook_at.unwrap_or(self.since);
        let silent = now - quiet_since >= self.cfg.silence_after.as_secs() as i64;
        let screen_waits = text
            .is_some_and(|t| matches!(t.status, Status::Waiting | Status::TurnDone | Status::Idle));
        if silent && screen_waits {
            let reason = format!(
                "hooks silent for {}s; screen: {}",
                now - quiet_since,
                text.map(|t| t.reason.as_str()).unwrap_or_default()
            );
            self.set(
                Assessment::new(Status::Unknown, Source::Text, 0.5, Some(&reason)),
                now,
            );
            return self.current.clone();
        }
        if self.current.source == Source::Hook {
            return self.current.clone();
        }
        // 还没有 hook 事件（或沉默解除）：进程层的 STARTING / RUNNING。
        self.set(process, now);
        self.current.clone()
    }

    fn observe_unhooked(
        &mut self,
        process: Assessment,
        text: Option<&DetectionResult>,
        output_changed: bool,
        now: i64,
    ) -> Assessment {
        // 文本层：只认 WAITING，且要连续 text_ticks 个 tick 一致。
        let text_waiting = match text {
            Some(t) if t.status == Status::Waiting => {
                let counted = now - self.last_text_at >= self.cfg.tick.as_secs() as i64;
                let n = match self.text_streak {
                    Some((Status::Waiting, n)) if counted => n + 1,
                    Some((Status::Waiting, n)) => n,
                    _ => 1,
                };
                if counted || self.text_streak.is_none() {
                    self.last_text_at = now;
                }
                self.text_streak = Some((Status::Waiting, n));
                (n >= self.cfg.text_ticks).then(|| {
                    Assessment::new(
                        Status::Waiting,
                        Source::Text,
                        t.confidence.min(0.8),
                        Some(&t.reason),
                    )
                })
            }
            _ => {
                self.text_streak = None;
                None
            }
        };
        let idle = if output_changed {
            None
        } else {
            let last = self.last_output_at.unwrap_or(self.since);
            (now - last >= self.cfg.idle_after.as_secs() as i64).then(|| {
                Assessment::new(
                    Status::Idle,
                    Source::Activity,
                    0.6,
                    Some(&format!("no output for {}s", now - last)),
                )
            })
        };
        let candidate = text_waiting.or(idle).unwrap_or(process);
        // 驻留：低层不覆盖 high_hold 内高层写的状态；同层推进（STARTING → RUNNING）不算覆盖。
        let held = protected(&self.current)
            && layer(candidate.source) < layer(self.current.source)
            && now - self.set_at < self.cfg.high_hold.as_secs() as i64;
        if !held {
            self.set(candidate, now);
        }
        self.current.clone()
    }
}
