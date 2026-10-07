//! Web Push 服务端（MISSION §6.6 / §10.3；agora-thc.6）：订阅、加密、投递与可达性。
//!
//! 只做 Apple 端点（iPhone-only，agora-thc.9）。推送源是承载节点的事件总线：
//! [`crate::events::Event::Notification`]——本机与 peer 的通知已同源（`src/peer/view.rs`
//! 把 peer 事件流里的 notification 转发进本机总线），所以这里不需要合并视图求差，
//! peer 会话的推送也由承载节点直接发出、不经 peer 中转。

pub mod crypto;
pub mod sender;
pub mod store;
pub mod vapid;

use std::sync::{Arc, Mutex};

pub use sender::{PushSender, SenderConfig};
pub use store::{PushStore, PushSubscription};
pub use vapid::Vapid;

/// `GET /api/health` 的 `push` 段（MISSION §10.3）。`apple` 三值：
/// `None` = 没发过也没探过（未启用 / 未评估）、`Some(true)` = 最近一次投递到得了 Apple、
/// `Some(false)` = 连不上（`reason` 说明是哪一类失败）。`fcm` 恒 `null`——Android 延后
/// （agora-w9ki）。
#[derive(Debug, Clone, Default)]
pub struct PushHealth {
    inner: Arc<Mutex<HealthInner>>,
}

#[derive(Debug, Clone, Default)]
struct HealthInner {
    apple: Option<bool>,
    reason: Option<String>,
}

impl PushHealth {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_reachable(&self) {
        self.put(Some(true), None);
    }

    /// 不可达：`reason` 给排障看，不带正文与订阅秘密。
    pub fn set_unreachable(&self, reason: impl Into<String>) {
        self.put(Some(false), Some(reason.into()));
    }

    /// 后端能连上但这次投递被拒（不是可达性问题）：不改可达性，只清掉旧的原因。
    pub fn note_delivery_error(&self, reason: impl Into<String>) {
        if let Ok(mut g) = self.inner.lock() {
            if g.apple.is_none() {
                return;
            }
            g.reason = Some(reason.into());
        }
    }

    fn put(&self, apple: Option<bool>, reason: Option<String>) {
        if let Ok(mut g) = self.inner.lock() {
            g.apple = apple;
            g.reason = reason;
        }
    }

    /// `{ "apple": true|false|null, "fcm": null, "reason": "…"|null }`。
    /// `reason` 只在 apple=false 时有值；字段恒在，客户端按类型读。
    pub fn snapshot(&self) -> serde_json::Value {
        let g = self.inner.lock().ok();
        let (apple, reason) = match g.as_ref() {
            Some(g) => (g.apple, g.reason.clone()),
            None => (None, None),
        };
        serde_json::json!({ "apple": apple, "fcm": null, "reason": reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_defaults_to_unknown_and_never_reports_fcm() {
        let h = PushHealth::new();
        assert_eq!(
            h.snapshot(),
            serde_json::json!({ "apple": null, "fcm": null, "reason": null })
        );
        h.set_unreachable("连不上 web.push.apple.com");
        assert_eq!(
            h.snapshot(),
            serde_json::json!({ "apple": false, "fcm": null, "reason": "连不上 web.push.apple.com" })
        );
        h.set_reachable();
        assert_eq!(
            h.snapshot(),
            serde_json::json!({ "apple": true, "fcm": null, "reason": null })
        );
    }

    #[test]
    fn delivery_error_does_not_invent_reachability() {
        let h = PushHealth::new();
        h.note_delivery_error("401");
        assert_eq!(
            h.snapshot()["apple"],
            serde_json::Value::Null,
            "没发过就不下结论"
        );
        h.set_reachable();
        h.note_delivery_error("429");
        assert_eq!(h.snapshot()["apple"], true, "拿到应答就是可达");
        assert_eq!(h.snapshot()["reason"], "429");
    }
}
