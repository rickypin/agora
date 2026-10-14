//! 同用户开发流程的一次性提醒（MISSION A54；接口契约见 docs/spec/api.md）。
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::events::{Event, EventBus};

pub const MAX_INPUT: usize = 64 * 1024;
pub const TITLE_LIMIT: usize = 200;
pub const BODY_LIMIT: usize = 4000;
pub const RATE_LIMIT: usize = 5;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    #[error("输入超过 64 KiB；请缩短通知")]
    TooLarge,
    #[error("输入不是合法 UTF-8；请使用 UTF-8 编码")]
    InvalidUtf8,
    #[error("标题为空；请在第一行填写标题")]
    EmptyTitle,
    #[error("notifications.enabled=false；请启用通知并重启 daemon")]
    Disabled,
    #[error("通知限流：连续 60 秒最多 5 条；请降低调用频率")]
    RateLimited,
    #[error("通知接收器不可用；请检查 daemon 日志")]
    Unavailable,
}

fn truncate(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        text
    } else {
        text.chars().take(limit - 1).chain(['…']).collect()
    }
}

pub fn normalize(title: &str, body: &str) -> Result<(String, String), Rejection> {
    if title.len().saturating_add(body.len()) > MAX_INPUT {
        return Err(Rejection::TooLarge);
    }
    let title: String = title.chars().filter(|c| !c.is_control()).collect();
    let title = title.trim();
    if title.is_empty() {
        return Err(Rejection::EmptyTitle);
    }
    let body: String = body
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    let start = body
        .split_inclusive('\n')
        .take_while(|line| line.trim().is_empty())
        .map(str::len)
        .sum::<usize>();
    Ok((
        truncate(title.to_owned(), TITLE_LIMIT),
        truncate(body[start..].trim_end().to_owned(), BODY_LIMIT),
    ))
}

pub fn parse(input: &[u8]) -> Result<(String, String), Rejection> {
    if input.len() > MAX_INPUT {
        return Err(Rejection::TooLarge);
    }
    let input = std::str::from_utf8(input).map_err(|_| Rejection::InvalidUtf8)?;
    let input = input.replace("\r\n", "\n");
    let (title, body) = input.split_once('\n').unwrap_or((&input, ""));
    normalize(title, body)
}

pub struct Receiver {
    events: EventBus,
    enabled: bool,
    accepted: Mutex<VecDeque<Instant>>,
}

impl Receiver {
    pub fn new(events: EventBus, enabled: bool) -> Self {
        Self {
            events,
            enabled,
            accepted: Mutex::new(VecDeque::new()),
        }
    }

    pub fn publish(&self, title: &str, body: &str) -> Result<(), Rejection> {
        self.publish_with_clock(title, body, Instant::now)
    }

    fn publish_with_clock(
        &self,
        title: &str,
        body: &str,
        clock: impl FnOnce() -> Instant,
    ) -> Result<(), Rejection> {
        let reject = |reason| {
            tracing::info!(component = "notify", ?reason, "通知被拒绝");
            reason
        };
        if !self.enabled {
            return Err(reject(Rejection::Disabled));
        }
        let (title, body) = normalize(title, body).map_err(reject)?;
        let mut accepted = self
            .accepted
            .lock()
            .map_err(|_| reject(Rejection::Unavailable))?;
        // Sample under the lock: publication order and window timestamps must agree.
        let now = clock();
        while accepted
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_WINDOW)
        {
            accepted.pop_front();
        }
        if accepted.len() >= RATE_LIMIT {
            return Err(reject(Rejection::RateLimited));
        }
        accepted.push_back(now);
        // 2026-10-14: once published, a lost socket reply is unknown, never a rejection.
        // Push readiness is deliberately absent: a peer may host the user's subscription.
        tracing::info!(
            component = "notify",
            title,
            body_chars = body.chars().count(),
            "通知已发布"
        );
        self.events.publish(Event::Notification {
            id: None,
            title,
            body,
            status: None,
            from_peer: false,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_limits_and_plain_text_normalization() {
        assert_eq!(
            parse(b"  title\r\n\r\n \t\r\n  body\x00\nnext \r\n").unwrap(),
            ("title".into(), "  body\nnext".into())
        );
        assert_eq!(parse(b"title").unwrap(), ("title".into(), "".into()));
        for bytes in [b"".as_slice(), b"\nbody", b" \x00\x1b\nbody"] {
            assert_eq!(parse(bytes), Err(Rejection::EmptyTitle));
        }
        assert_eq!(parse(&[0xff]), Err(Rejection::InvalidUtf8));
        assert_eq!(parse(&vec![b'x'; MAX_INPUT + 1]), Err(Rejection::TooLarge));
        assert!(parse(&vec![b'x'; MAX_INPUT]).is_ok());
        let (title, body) =
            parse(format!("{}\n{}", "好".repeat(201), "🙂".repeat(4001)).as_bytes()).unwrap();
        assert_eq!(title.chars().count(), TITLE_LIMIT);
        assert_eq!(body.chars().count(), BODY_LIMIT);
        assert!(title.ends_with('…') && body.ends_with('…'));
    }

    #[test]
    fn sliding_window_counts_only_acceptances_and_recovers() {
        let bus = EventBus::default();
        let mut events = bus.subscribe();
        let receiver = Receiver::new(bus.clone(), true);
        let now = Instant::now();
        assert_eq!(
            receiver.publish_with_clock("", "", || now),
            Err(Rejection::EmptyTitle)
        );
        for _ in 0..RATE_LIMIT {
            receiver.publish_with_clock("t", "b", || now).unwrap();
        }
        assert_eq!(
            receiver.publish_with_clock("t", "b", || now + RATE_WINDOW - Duration::from_nanos(1)),
            Err(Rejection::RateLimited)
        );
        receiver
            .publish_with_clock("t", "b", || now + RATE_WINDOW)
            .unwrap();
        let mut count = 0;
        while events.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, RATE_LIMIT + 1);
        assert_eq!(
            Receiver::new(bus, false).publish("t", "b"),
            Err(Rejection::Disabled)
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn sliding_window_expires_each_acceptance_instead_of_resetting_a_bucket() {
        let receiver = Receiver::new(EventBus::default(), true);
        let now = Instant::now();
        receiver.publish_with_clock("t", "", || now).unwrap();
        for _ in 1..RATE_LIMIT {
            receiver
                .publish_with_clock("t", "", || now + RATE_WINDOW / 2)
                .unwrap();
        }
        receiver
            .publish_with_clock("t", "", || now + RATE_WINDOW)
            .unwrap();
        assert_eq!(
            receiver.publish_with_clock("t", "", || now + RATE_WINDOW),
            Err(Rejection::RateLimited)
        );
        for _ in 1..RATE_LIMIT {
            receiver
                .publish_with_clock("t", "", || now + RATE_WINDOW + RATE_WINDOW / 2)
                .unwrap();
        }
        assert_eq!(
            receiver.publish_with_clock("t", "", || now + RATE_WINDOW + RATE_WINDOW / 2),
            Err(Rejection::RateLimited)
        );
    }

    #[test]
    fn concurrent_callers_share_one_budget() {
        let receiver = Receiver::new(EventBus::default(), true);
        std::thread::scope(|scope| {
            let calls: Vec<_> = (0..30)
                .map(|_| scope.spawn(|| receiver.publish("t", "")))
                .collect();
            assert_eq!(
                calls
                    .into_iter()
                    .map(|c| c.join().unwrap())
                    .filter(Result::is_ok)
                    .count(),
                RATE_LIMIT
            );
        });
    }
}
