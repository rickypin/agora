//! 采纳 socket 扫描失败的状态化日志（agora-abg8）。
//!
//! `TmuxRuntime::list()` 每 2 s 扫一次采纳 socket。socket 持续不可读时（例如
//! `/tmp/tmux-*` 被清理工具扫掉而 tmux server 仍持有原来的 inode），旧代码每 tick
//! 无条件 warn 一条，日志风暴会淹没真正的新故障。这里只做**日志状态转换**，不改变
//! 扫描结论：
//!
//! - 第一次不可读 / reason 变化：立即发 WARN；
//! - 同一个 socket 同一个 reason 持续不可读：最多每 5 分钟发一条 heartbeat；
//! - 恢复可读：发一条 INFO，带不可用时长与期间抑制的次数。
//!
//! 时间由调用方传入，转换全是纯函数，单测不依赖 tracing subscriber 或真实时钟。
//! 多 socket 各记各的：一个采纳 socket 的故障不改变另一个 socket 的计时。

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// heartbeat 间隔（agora-abg8：持续不可读时最多每 5 分钟一条）。
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(5 * 60);

/// 一次状态转换要做的事。调用方按 variant 选 WARN / INFO 与字段；helper 自己不碰日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanLogEvent {
    /// 某个 socket 从可读（或未观测）进入不可读：立即 WARN，带首次时间。
    BecameUnavailable {
        socket: String,
        reason: String,
        since: SystemTime,
    },
    /// 同一个 socket 的失败原因变了：重置 heartbeat 计时并立即 WARN。
    /// `since` 是本轮不可用从第一次失败算起的时刻，`reason_since` 是新原因出现的时刻。
    ReasonChanged {
        socket: String,
        previous_reason: String,
        reason: String,
        since: SystemTime,
        reason_since: SystemTime,
    },
    /// 持续不可读期间的心跳：最多每 [`HEARTBEAT_EVERY`] 一条。
    /// `suppressed` 是自上一次日志以来被抑制的扫描次数。
    Heartbeat {
        socket: String,
        reason: String,
        since: SystemTime,
        last_seen: SystemTime,
        suppressed: u64,
    },
    /// 恢复可读：一条 INFO。`unavailable_for` 与 `suppressed` 覆盖整轮不可用。
    Recovered {
        socket: String,
        reason: String,
        unavailable_for: Duration,
        suppressed: u64,
    },
}

#[derive(Debug, Clone)]
struct Unavailable {
    /// 这一轮不可用的第一次失败时刻；reason 变化不重置它。
    since: SystemTime,
    reason: String,
    reason_since: SystemTime,
    last_seen: SystemTime,
    last_log: SystemTime,
    /// 自上一次日志以来的抑制次数，heartbeat 报它然后清零。
    suppressed_since_log: u64,
    /// 整轮不可用期间的抑制总数，恢复时报它。
    suppressed_total: u64,
}

/// per-socket + reason 的失败日志状态机（agora-abg8）。
pub struct AdoptScanFailureLog {
    heartbeat_every: Duration,
    sockets: HashMap<String, Unavailable>,
}

impl Default for AdoptScanFailureLog {
    fn default() -> Self {
        AdoptScanFailureLog {
            heartbeat_every: HEARTBEAT_EVERY,
            sockets: HashMap::new(),
        }
    }
}

impl AdoptScanFailureLog {
    /// 记一次失败的扫描。返回 `None` 表示这一次应当完全安静（被节流）。
    pub fn failure(&mut self, socket: &str, reason: &str, now: SystemTime) -> Option<ScanLogEvent> {
        let Some(state) = self.sockets.get_mut(socket) else {
            self.sockets.insert(
                socket.to_owned(),
                Unavailable {
                    since: now,
                    reason: reason.to_owned(),
                    reason_since: now,
                    last_seen: now,
                    last_log: now,
                    suppressed_since_log: 0,
                    suppressed_total: 0,
                },
            );
            return Some(ScanLogEvent::BecameUnavailable {
                socket: socket.to_owned(),
                reason: reason.to_owned(),
                since: now,
            });
        };

        state.last_seen = now;

        // 原因变化先于 heartbeat：马上告警，并从这一刻重新计 heartbeat。
        if state.reason != reason {
            let previous_reason = std::mem::replace(&mut state.reason, reason.to_owned());
            state.reason_since = now;
            state.last_log = now;
            state.suppressed_since_log = 0;
            return Some(ScanLogEvent::ReasonChanged {
                socket: socket.to_owned(),
                previous_reason,
                reason: reason.to_owned(),
                since: state.since,
                reason_since: now,
            });
        }

        if due(now, state.last_log, self.heartbeat_every) {
            let suppressed = state.suppressed_since_log;
            state.suppressed_since_log = 0;
            state.last_log = now;
            return Some(ScanLogEvent::Heartbeat {
                socket: socket.to_owned(),
                reason: state.reason.clone(),
                since: state.since,
                last_seen: now,
                suppressed,
            });
        }

        state.suppressed_since_log += 1;
        state.suppressed_total += 1;
        None
    }

    /// 记一次成功的扫描。第一次成功（或从不可用恢复）返回 INFO 事件；之后一直安静。
    pub fn success(&mut self, socket: &str, now: SystemTime) -> Option<ScanLogEvent> {
        let state = self.sockets.remove(socket)?;
        Some(ScanLogEvent::Recovered {
            socket: socket.to_owned(),
            reason: state.reason,
            unavailable_for: now.duration_since(state.since).unwrap_or_default(),
            suppressed: state.suppressed_total,
        })
    }
}

/// 到点才发 heartbeat。时钟回拨时宁可立刻发一条，也不无限沉默。
fn due(now: SystemTime, last: SystemTime, every: Duration) -> bool {
    match now.duration_since(last) {
        Ok(elapsed) => elapsed >= every,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn became(socket: &str, reason: &str, secs: u64) -> ScanLogEvent {
        ScanLogEvent::BecameUnavailable {
            socket: socket.into(),
            reason: reason.into(),
            since: at(secs),
        }
    }

    #[test]
    fn first_failure_warns_then_every_tick_is_quiet() {
        let mut log = AdoptScanFailureLog::default();
        assert_eq!(
            log.failure("default", "socket 文件不在", at(1_000)),
            Some(became("default", "socket 文件不在", 1_000)),
            "第一次进入 unavailable 要打一条 WARN"
        );
        assert_eq!(log.failure("default", "socket 文件不在", at(1_002)), None);
        assert_eq!(log.failure("default", "socket 文件不在", at(1_004)), None);
        assert_eq!(
            log.failure("default", "socket 文件不在", at(1_298)),
            None,
            "同一个 reason 未满 heartbeat 间隔时不许重复"
        );
    }

    #[test]
    fn heartbeat_is_at_most_once_per_five_minutes() {
        let mut log = AdoptScanFailureLog::default();
        assert_eq!(
            log.failure("default", "socket 文件不在", at(0)),
            Some(became("default", "socket 文件不在", 0))
        );
        // 每 2 s 一次的 tick：t=2..298 都被抑制（149 条）。
        for secs in (2..300).step_by(2) {
            assert_eq!(
                log.failure("default", "socket 文件不在", at(secs)),
                None,
                "t={secs} 不该打日志"
            );
        }
        assert_eq!(
            log.failure("default", "socket 文件不在", at(300)),
            Some(ScanLogEvent::Heartbeat {
                socket: "default".into(),
                reason: "socket 文件不在".into(),
                since: at(0),
                last_seen: at(300),
                suppressed: 149,
            }),
            "满 5 分钟一条 heartbeat，带 suppressed_count / last_seen / 首次时间"
        );
        // 下一个周期同样：t=302..598 被抑制，t=600 再来一条。
        for secs in (302..600).step_by(2) {
            assert_eq!(
                log.failure("default", "socket 文件不在", at(secs)),
                None,
                "t={secs} 不该打日志"
            );
        }
        assert_eq!(
            log.failure("default", "socket 文件不在", at(600)),
            Some(ScanLogEvent::Heartbeat {
                socket: "default".into(),
                reason: "socket 文件不在".into(),
                since: at(0),
                last_seen: at(600),
                suppressed: 149,
            })
        );
    }

    #[test]
    fn changed_reason_warns_immediately_and_resets_the_heartbeat() {
        let mut log = AdoptScanFailureLog::default();
        assert_eq!(
            log.failure("default", "socket 文件不在", at(0)),
            Some(became("default", "socket 文件不在", 0))
        );
        assert_eq!(log.failure("default", "socket 文件不在", at(2)), None);
        assert_eq!(log.failure("default", "socket 文件不在", at(4)), None);
        assert_eq!(
            log.failure("default", "权限拒绝", at(6)),
            Some(ScanLogEvent::ReasonChanged {
                socket: "default".into(),
                previous_reason: "socket 文件不在".into(),
                reason: "权限拒绝".into(),
                since: at(0),
                reason_since: at(6),
            }),
            "reason 变了要立即重新 WARN，不能等 heartbeat"
        );
        // 新 reason 从 t=6 重新计时：紧接着的 tick 仍被抑制。
        assert_eq!(log.failure("default", "权限拒绝", at(8)), None);
        for secs in (10..306).step_by(2) {
            assert_eq!(log.failure("default", "权限拒绝", at(secs)), None);
        }
        assert_eq!(
            log.failure("default", "权限拒绝", at(306)),
            Some(ScanLogEvent::Heartbeat {
                socket: "default".into(),
                reason: "权限拒绝".into(),
                since: at(0),
                last_seen: at(306),
                suppressed: 149,
            }),
            "reason 变化重置 heartbeat 计时，但首次时间仍是整轮不可用的起点"
        );
    }

    #[test]
    fn recovery_reports_duration_and_total_suppressed_count() {
        let mut log = AdoptScanFailureLog::default();
        assert_eq!(
            log.failure("default", "socket 文件不在", at(1_000)),
            Some(became("default", "socket 文件不在", 1_000))
        );
        assert_eq!(log.failure("default", "socket 文件不在", at(1_002)), None);
        assert_eq!(log.failure("default", "socket 文件不在", at(1_004)), None);
        assert_eq!(
            log.success("default", at(1_007)),
            Some(ScanLogEvent::Recovered {
                socket: "default".into(),
                reason: "socket 文件不在".into(),
                unavailable_for: Duration::from_secs(7),
                suppressed: 2,
            }),
            "恢复 INFO 要带不可用持续时长与期间被抑制的次数"
        );
        assert_eq!(
            log.success("default", at(1_008)),
            None,
            "恢复之后的每次成功都不再打日志"
        );
        assert_eq!(
            log.failure("default", "socket 文件不在", at(2_000)),
            Some(became("default", "socket 文件不在", 2_000)),
            "下一次不可用是新一轮，首次时间重新算"
        );
    }

    #[test]
    fn sockets_do_not_share_state_or_timers() {
        let mut log = AdoptScanFailureLog::default();
        assert_eq!(
            log.failure("default", "socket 文件不在", at(0)),
            Some(became("default", "socket 文件不在", 0))
        );
        assert_eq!(
            log.failure("work", "socket 文件不在", at(1)),
            Some(became("work", "socket 文件不在", 1)),
            "另一个 socket 第一次不可读也要自己 WARN"
        );
        assert_eq!(log.failure("default", "socket 文件不在", at(2)), None);
        assert_eq!(log.failure("work", "socket 文件不在", at(3)), None);

        assert_eq!(
            log.success("default", at(4)),
            Some(ScanLogEvent::Recovered {
                socket: "default".into(),
                reason: "socket 文件不在".into(),
                unavailable_for: Duration::from_secs(4),
                suppressed: 1,
            })
        );
        // default 恢复 / 再次不可用，都不影响 work 的计时与抑制计数。
        assert_eq!(
            log.failure("default", "socket 文件不在", at(6)),
            Some(became("default", "socket 文件不在", 6))
        );
        assert_eq!(log.failure("work", "socket 文件不在", at(7)), None);
        assert_eq!(
            log.success("work", at(302)),
            Some(ScanLogEvent::Recovered {
                socket: "work".into(),
                reason: "socket 文件不在".into(),
                unavailable_for: Duration::from_secs(301),
                suppressed: 2,
            }),
            "work 的持续时长从 t=1 算起，抑制数只算 work 自己的两次"
        );
    }

    #[test]
    fn clock_rollback_sends_a_heartbeat_instead_of_going_silent() {
        let mut log = AdoptScanFailureLog::default();
        assert_eq!(
            log.failure("default", "socket 文件不在", at(10_000)),
            Some(became("default", "socket 文件不在", 10_000))
        );
        assert_eq!(
            log.failure("default", "socket 文件不在", at(9_000)),
            Some(ScanLogEvent::Heartbeat {
                socket: "default".into(),
                reason: "socket 文件不在".into(),
                since: at(10_000),
                last_seen: at(9_000),
                suppressed: 0,
            }),
            "时钟回拨视为到期：宁可多一条 heartbeat，也不无限沉默"
        );
    }
}
