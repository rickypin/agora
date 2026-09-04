//! 全局事件总线（docs/spec/api.md `/api/events`；MISSION §3.5 "快照 + 增量"）。
//!
//! 事件是**增量**：客户端先拉 `GET /api/sessions` 全量，再按事件就地 patch。总线是 tokio
//! broadcast，慢客户端读不过来就丢它的旧事件并塞一条 [`Event::Resync`]——它必须重拉全量，
//! 而不是服务端替它攒无限长的队列。事件里的会话 id 已经是对外形态 `<node>:<id>`。
//!
//! 事件来源两处：API handler 自己知道的（create / adopt / delete）立即发；进程状态的变化
//! 没有人来通知，由 [`watch`] 按 `status.detector_interval` 轮询 Session Manager 求差发出。
//! 轮询也会发现 handler 之外的增删（daemon 重启 reconcile、别的进程改库），所以 handler
//! 发过的事件轮询不会再发第二遍：它只比较自己上一轮的快照。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::session::{SessionManager, SessionView};
use crate::status::{Source, Status};

/// 每个订阅者能落后的事件数；超过就 Resync。
pub const CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    SessionCreated {
        id: String,
        session: serde_json::Value,
    },
    SessionRemoved {
        id: String,
    },
    /// metadata 改了（改名等）：整行重发，客户端就地替换（agora-xqa.11：改名后列表不刷新）。
    SessionUpdated {
        id: String,
        session: serde_json::Value,
    },
    StatusChanged {
        id: String,
        status: Status,
        source: Source,
        reason: Option<String>,
        alive: bool,
        /// hook 给的问题 / 最后一条回复；变了也算状态变化（就地回答要显示它）。
        detail: Option<String>,
    },
    /// 挂起的决定被解除（ADR-002 D5）：`via` 是 dashboard / terminal / session / exit / timeout。
    DecisionResolved {
        id: String,
        tool_use_id: String,
        via: &'static str,
    },
    /// 通知（WAITING 等）在 M1b 的状态层接上；形态先定下来。
    Notification {
        id: Option<String>,
        title: String,
        body: String,
    },
    /// 服务端丢了该客户端的事件：客户端必须重拉全量快照。
    Resync,
}

impl Event {
    /// 同一批里可合并的键：同一会话的连续状态变化只留最后一条。
    pub fn coalesce_key(&self) -> Option<(&'static str, &str)> {
        match self {
            Event::StatusChanged { id, .. } => Some(("status", id)),
            Event::SessionUpdated { id, .. } => Some(("updated", id)),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::with_capacity(CAPACITY)
    }
}

impl EventBus {
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        EventBus { tx }
    }

    /// 没有订阅者时静默丢弃：事件是增量，没人在听就没有人需要补。
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

/// 把一批事件合并：同键只留最后一条，位置取最后一次出现；其余原序保留。
pub fn coalesce(events: Vec<Event>) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    for e in events {
        if let Some(key) = e.coalesce_key() {
            let key = (key.0, key.1.to_owned());
            out.retain(|x| x.coalesce_key().map(|k| (k.0, k.1.to_owned())).as_ref() != Some(&key));
        }
        out.push(e);
    }
    out
}

/// 一轮快照里每个会话记什么，用来求差。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    status: Status,
    source: Source,
    reason: Option<String>,
    alive: bool,
    detail: Option<String>,
}

fn seen(v: &SessionView) -> Seen {
    Seen {
        status: v.assessment.status,
        source: v.assessment.source,
        reason: v.assessment.reason.clone(),
        alive: v.alive,
        detail: v.detail.clone(),
    }
}

/// 求差器：喂它每一轮的列表，吐出该发的事件。与 tokio 无关，便于单测。
#[derive(Debug, Default)]
pub struct Differ {
    last: HashMap<String, Seen>,
    primed: bool,
}

impl Differ {
    /// 第一轮只建立基线不发事件：daemon 刚起来时客户端本来就要拉全量。
    pub fn step(&mut self, node: &str, views: &[SessionView]) -> Vec<Event> {
        let mut now: HashMap<String, Seen> = HashMap::with_capacity(views.len());
        let mut out = Vec::new();
        for v in views {
            let gid = global_id(node, &v.record.id);
            let s = seen(v);
            match self.last.get(&gid) {
                None if self.primed => out.push(Event::SessionCreated {
                    id: gid.clone(),
                    session: export(node, v),
                }),
                Some(prev) if *prev != s => out.push(Event::StatusChanged {
                    id: gid.clone(),
                    status: s.status,
                    source: s.source,
                    reason: s.reason.clone(),
                    alive: s.alive,
                    detail: s.detail.clone(),
                }),
                _ => {}
            }
            now.insert(gid, s);
        }
        if self.primed {
            for gone in self.last.keys().filter(|k| !now.contains_key(*k)) {
                out.push(Event::SessionRemoved { id: gone.clone() });
            }
        }
        self.last = now;
        self.primed = true;
        out
    }
}

/// 对外 id：`<node>:<id>`（MISSION §3.5）。
pub fn global_id(node: &str, id: &str) -> String {
    format!("{node}:{id}")
}

/// 一条会话的对外形态：`id` 换成全局 id，加 `node`，本机 id 留在 `local_id`。
pub fn export(node: &str, view: &SessionView) -> serde_json::Value {
    let mut v = serde_json::to_value(view).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(map) = &mut v {
        map.insert(
            "local_id".into(),
            serde_json::Value::String(view.record.id.clone()),
        );
        map.insert(
            "id".into(),
            serde_json::Value::String(global_id(node, &view.record.id)),
        );
        map.insert("node".into(), serde_json::Value::String(node.to_owned()));
    }
    v
}

/// 轮询任务：每 `interval` 列一次会话求差发事件；列表失败只记日志，下一轮再来。
pub async fn watch(
    sessions: Arc<SessionManager>,
    bus: EventBus,
    node: Arc<str>,
    interval: Duration,
) {
    let mut differ = Differ::default();
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let s = sessions.clone();
        // list 会起运行时子进程：放 blocking 线程（ADR-001 D8）。
        match tokio::task::spawn_blocking(move || s.list()).await {
            Ok(Ok(views)) => {
                for e in differ.step(&node, &views) {
                    bus.publish(e);
                }
            }
            Ok(Err(err)) => tracing::warn!(component = "events", %err, "列会话失败，跳过本轮"),
            Err(err) => tracing::warn!(component = "events", %err, "轮询任务异常"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: &str, st: Status) -> Event {
        Event::StatusChanged {
            id: id.into(),
            status: st,
            source: Source::Process,
            reason: None,
            alive: true,
            detail: None,
        }
    }

    #[test]
    fn coalesce_keeps_last_status_per_session_and_order_of_the_rest() {
        let batch = vec![
            status("a", Status::Starting),
            Event::SessionRemoved { id: "b".into() },
            status("a", Status::Running),
            status("c", Status::Running),
        ];
        let out = coalesce(batch);
        assert_eq!(
            out,
            vec![
                Event::SessionRemoved { id: "b".into() },
                status("a", Status::Running),
                status("c", Status::Running),
            ]
        );
    }

    #[test]
    fn slow_subscriber_gets_lagged_not_a_backlog() {
        let bus = EventBus::with_capacity(2);
        let mut rx = bus.subscribe();
        for _ in 0..5 {
            bus.publish(Event::Resync);
        }
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
    }
}
