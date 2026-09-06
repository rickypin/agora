//! 并入视图：每个 peer 在本节点眼里的会话行（agora-7ku.5；MISSION §3.5；ADR-004；不变量 8）。
//!
//! 节点是 peer 的 API 客户端：客户端任务（`super::client`）拉全量、收事件流，把 peer 报的行
//! 原样存进这里；`GET /api/sessions` 给**人**看的那份 = 本机行 + 这里的全部行，给 **peer** 看的
//! 只有本机行——只导出本机会话、一跳防环（MISSION §3.5）。所以这里的行永远不会再被导出。
//!
//! 三条并入规则（守卫在 `tests/peer_view.rs` 与本文件的单测）：
//! 1. **行归属**：只收 `node == 该 peer 名`、`id` 以 `<peer>:` 开头的行。peer 违规把别人的会话
//!    导出来（多跳、或它自己的视图漏出去）就丢弃并 warn——一跳在收方也守一次，不只靠对方守。
//! 2. **时间用本节点时钟**（MISSION §3.5 原句："peer 视图里的时间（上次见到、等待时长）由本
//!    节点的时钟打，不信 peer 报的时间"；ADR-004 守卫句）。"等待时长"来自 `status_since`：这里把
//!    它改写成**本节点第一次看见该行处于当前状态的时刻**；peer 报的 `status_since` 只当"状态没
//!    变"的判据（连同 `status` 一起比），不当时间用——断线重连后同一状态的行不会被重置成 0 分钟，
//!    时钟漂移的 peer 也污染不了 attention 排序。"上次见到"是 `PeerStates::last_seen`，客户端用同
//!    一个时钟打。其余时间字段（`created_at` / `ended_at`…）是 peer 的 metadata，原样保留。
//! 3. **断线保留、标 stale**：peer 掉线后行一条不删，每行 `stale: true` 并带 `last_seen`（该 peer
//!    的"上次见到"，`PeerState::last_seen` 的 UTC 文本，与 `/api/health` peers 段同一个值、同一只
//!    本机表）；重新连上、全量对齐后 `stale` 回 `false`、`last_seen` 键消失。stale 的两次翻转都发
//!    `session_updated`，浏览器不用轮询就能看见。`last_seen` 只出现在 stale 的 peer 行上：非 stale
//!    时不管 peer 报的行里有没有这个键都剥掉，本机行从来没有（agora-7ku.6）。
//!
//! 这里没有 I/O：客户端任务把 peer 的 JSON 喂进来，拿回该发进本机 `EventBus` 的事件。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{watch, Notify};

use crate::events::Event;

/// 本节点的时钟（unix 秒）。生产是 `clock::now_secs`；测试注入固定值证明"不信 peer 报的时间"。
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// `decision_resolved.via` 的取值表（`src/events.rs` 用 `&'static str`）：peer 报的值只有在这张
/// 表里才转发，不认识的丢弃——事件形态是 API 的一部分，peer 升级加了新值我们不替它编。
const DECISION_VIA: &[&str] = &["dashboard", "terminal", "session", "exit", "timeout"];

/// 一条 peer 行：已改写过 `status_since` / `stale` 的 JSON，加上判"状态没变"的 token。
#[derive(Debug, Clone)]
struct Row {
    json: Value,
    /// peer 报的 `(status, status_since)`：变了才重新打本机时刻。
    token: (Option<String>, Option<i64>),
    /// 本节点第一次看见该行处于当前状态的时刻。
    since: i64,
}

#[derive(Debug, Clone, Default)]
struct PeerView {
    rows: BTreeMap<String, Row>,
    stale: bool,
    /// stale 期间每行 `last_seen` 的来源（unix 秒，本机时钟）；非 stale 时 None。存在这里是为了
    /// stale 期间再进来的行（理论上不会有——流已断——但 `apply` 的形态要自洽）也带同一个值。
    last_seen: Option<i64>,
}

/// 客户端任务的唤醒把手：`retry_now` 缩短一次退避等待（在线时无事），`reconnect` 让在线的
/// 连接立刻断开重连（重拉全量对齐）。两者都由 `PeerViews::retry_now` / `reconnect` 按 peer 名调。
#[derive(Debug, Default)]
pub struct Wake {
    notify: Notify,
    force: AtomicBool,
}

impl Wake {
    /// 退避等待中 → 立刻重试；已连上 → 无事。agora-7ku.6 "点开 stale 会话立即重试"用这个。
    pub fn retry_now(&self) {
        self.notify.notify_one();
    }

    /// 无论在等还是在连，都立刻断开并重新走一遍：比版本、拉全量、收事件流。
    pub fn reconnect(&self) {
        self.force.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// 等下一次唤醒；返回它是不是 `reconnect`（强制）。
    pub async fn wait(&self) -> bool {
        self.notify.notified().await;
        self.force.swap(false, Ordering::SeqCst)
    }
}

#[derive(Default)]
struct Inner {
    peers: BTreeMap<String, PeerView>,
    wakes: BTreeMap<String, Arc<Wake>>,
}

/// 客户端任务对一条 peer 事件的处理结果。
#[derive(Debug)]
pub enum Applied {
    /// 视图改了：把这条事件发进本机 EventBus。
    Publish(Event),
    /// 视图对不上了（peer 说 resync、或 status_changed 指向没见过的行）：重拉全量。
    Resync,
    /// 无事（pong、不认识的类型、被规则 1 丢弃的行）。
    Ignored,
}

/// 全部 peer 的并入视图。`AppState::peer_views` 持有一份：客户端任务写，`GET /api/sessions` 读。
#[derive(Clone)]
pub struct PeerViews {
    inner: Arc<Mutex<Inner>>,
    clock: Clock,
    /// 每次视图或 stale 变化 +1；测试与将来的推送靠它等"变了"而不 sleep。
    changed: watch::Sender<u64>,
}

impl Default for PeerViews {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerViews {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(crate::clock::now_secs))
    }

    /// 注入时钟（测试：固定一个远离当前的值，证明行上的时间是本节点打的，不是 peer 报的）。
    pub fn with_clock(clock: Clock) -> Self {
        let (changed, _) = watch::channel(0);
        PeerViews {
            inner: Arc::new(Mutex::new(Inner::default())),
            clock,
            changed,
        }
    }

    /// 本节点时钟的当下；客户端打 `last_seen` 用同一个源，"上次见到"与"等待时长"才是同一只表。
    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    /// 订阅"变了"：每次全量替换、事件应用、stale 翻转都 +1。
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    // ---------- 客户端任务写 ----------

    /// 全量替换某个 peer 的行（连上时、resync 时）：回到非 stale，返回相对旧视图的差分事件——
    /// 新行 `session_created`、变了的行 `session_updated`、没了的行 `session_removed`。
    /// 不合规则 1 的行丢弃并 warn，不算进视图。
    pub fn replace(&self, peer: &str, rows: Vec<Value>) -> Vec<Event> {
        let now = self.now();
        let mut inner = lock(&self.inner);
        let view = inner.peers.entry(peer.to_owned()).or_default();
        let old = std::mem::take(&mut view.rows);
        view.stale = false;
        view.last_seen = None;
        let mut events = Vec::new();
        for raw in rows {
            let Some(gid) = accept(peer, &raw) else {
                continue;
            };
            let row = stamp(raw, old.get(&gid), now, false, None);
            match old.get(&gid) {
                None => events.push(Event::SessionCreated {
                    id: gid.clone(),
                    session: row.json.clone(),
                }),
                Some(prev) if prev.json != row.json => events.push(Event::SessionUpdated {
                    id: gid.clone(),
                    session: row.json.clone(),
                }),
                Some(_) => {}
            }
            view.rows.insert(gid, row);
        }
        for gone in old.keys().filter(|k| !view.rows.contains_key(*k)) {
            events.push(Event::SessionRemoved { id: gone.clone() });
        }
        drop(inner);
        self.bump();
        events
    }

    /// 应用 peer 事件流里的一条（`/api/events` 一帧是数组，逐条喂）。
    pub fn apply(&self, peer: &str, event: &Value) -> Applied {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let id = event.get("id").and_then(Value::as_str);
        let owned = |id: &str| owned_by(peer, id);
        match kind {
            "session_created" | "session_updated" => {
                let Some(raw) = event.get("session") else {
                    return Applied::Ignored;
                };
                let Some(gid) = accept(peer, raw) else {
                    return Applied::Ignored;
                };
                let now = self.now();
                let mut inner = lock(&self.inner);
                let view = inner.peers.entry(peer.to_owned()).or_default();
                let row = stamp(
                    raw.clone(),
                    view.rows.get(&gid),
                    now,
                    view.stale,
                    view.last_seen,
                );
                let json = row.json.clone();
                view.rows.insert(gid.clone(), row);
                drop(inner);
                self.bump();
                Applied::Publish(if kind == "session_created" {
                    Event::SessionCreated {
                        id: gid,
                        session: json,
                    }
                } else {
                    Event::SessionUpdated {
                        id: gid,
                        session: json,
                    }
                })
            }
            "session_removed" => {
                let Some(id) = id.filter(|i| owned(i)) else {
                    return Applied::Ignored;
                };
                let mut inner = lock(&self.inner);
                let removed = inner
                    .peers
                    .get_mut(peer)
                    .is_some_and(|v| v.rows.remove(id).is_some());
                drop(inner);
                if !removed {
                    return Applied::Ignored;
                }
                self.bump();
                Applied::Publish(Event::SessionRemoved { id: id.to_owned() })
            }
            "status_changed" => {
                let Some(id) = id.filter(|i| owned(i)) else {
                    return Applied::Ignored;
                };
                let now = self.now();
                let mut inner = lock(&self.inner);
                let Some(view) = inner.peers.get_mut(peer) else {
                    return Applied::Resync;
                };
                let Some(prev) = view.rows.get(id) else {
                    // 事件指向没见过的行：我们漏了它的 created，视图已经对不上。
                    return Applied::Resync;
                };
                // 与浏览器就地 patch 的字段集相同（web/src/events.ts）：事件没带的沿用旧值。
                let mut raw = prev.json.clone();
                if let Value::Object(map) = &mut raw {
                    for k in [
                        "status",
                        "source",
                        "reason",
                        "alive",
                        "detail",
                        "prompt",
                        "progress",
                        "preview",
                        "status_since",
                        "hooks_unheard",
                    ] {
                        if let Some(v) = event.get(k) {
                            map.insert(k.to_owned(), v.clone());
                        }
                    }
                }
                let (stale, last_seen) = (view.stale, view.last_seen);
                let row = stamp(raw, Some(prev), now, stale, last_seen);
                let json = row.json.clone();
                view.rows.insert(id.to_owned(), row);
                drop(inner);
                self.bump();
                // 整行重发而不是转发 status_changed：行已经按本机时钟改写过，浏览器就地替换即可。
                Applied::Publish(Event::SessionUpdated {
                    id: id.to_owned(),
                    session: json,
                })
            }
            "decision_resolved" => {
                let (Some(id), Some(tool_use_id), Some(via)) = (
                    id.filter(|i| owned(i)),
                    event.get("tool_use_id").and_then(Value::as_str),
                    event.get("via").and_then(Value::as_str),
                ) else {
                    return Applied::Ignored;
                };
                let Some(via) = DECISION_VIA.iter().find(|v| **v == via) else {
                    tracing::debug!(component = "peer", %peer, via, "decision_resolved 的 via 不认识，丢弃");
                    return Applied::Ignored;
                };
                Applied::Publish(Event::DecisionResolved {
                    id: id.to_owned(),
                    tool_use_id: tool_use_id.to_owned(),
                    via,
                })
            }
            "notification" => {
                // 跨节点的"谁在等我"就是靠它（MISSION §0.2 一天的形态第 1 步）；只转发指向该 peer
                // 会话的（或不针对会话的系统通知），别人的会话按规则 1 丢。
                let id = match event.get("id") {
                    Some(Value::String(s)) if owned(s) => Some(s.clone()),
                    Some(Value::Null) | None => None,
                    Some(_) => return Applied::Ignored,
                };
                let text = |k: &str| {
                    event
                        .get(k)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_default()
                };
                let status = event
                    .get("status")
                    .cloned()
                    .and_then(|s| serde_json::from_value(s).ok());
                Applied::Publish(Event::Notification {
                    id,
                    title: text("title"),
                    body: text("body"),
                    status,
                })
            }
            "resync" => Applied::Resync,
            _ => Applied::Ignored,
        }
    }

    /// peer 掉线：行一条不删，每行 `stale: true` 并写上 `last_seen`（该 peer 的"上次见到"，调用方从
    /// `PeerState::last_seen` 取——与 `/api/health` peers 段同一个值；None 只可能是从没连上过，那时
    /// 也没有行可标）。已经 stale 就没有事件。返回每行一条 `session_updated`，浏览器据此立刻把行
    /// 画成 stale 并显示"上次见到 hh:mm"（agora-7ku.6）。
    pub fn mark_stale(&self, peer: &str, last_seen: Option<i64>) -> Vec<Event> {
        let mut inner = lock(&self.inner);
        let view = inner.peers.entry(peer.to_owned()).or_default();
        if view.stale {
            return Vec::new();
        }
        view.stale = true;
        view.last_seen = last_seen;
        let mut events = Vec::with_capacity(view.rows.len());
        for (gid, row) in view.rows.iter_mut() {
            mark(&mut row.json, true, last_seen);
            events.push(Event::SessionUpdated {
                id: gid.clone(),
                session: row.json.clone(),
            });
        }
        drop(inner);
        self.bump();
        events
    }

    // ---------- API 读 ----------

    /// 全部 peer 的行（按 peer 名、再按 id），每行带 `stale`；`GET /api/sessions` 给人看的那份用。
    pub fn rows(&self) -> Vec<Value> {
        lock(&self.inner)
            .peers
            .values()
            .flat_map(|v| v.rows.values().map(|r| r.json.clone()))
            .collect()
    }

    pub fn rows_of(&self, peer: &str) -> Vec<Value> {
        lock(&self.inner)
            .peers
            .get(peer)
            .map(|v| v.rows.values().map(|r| r.json.clone()).collect())
            .unwrap_or_default()
    }

    /// 按全局 id 取一行（`GET /api/sessions/:id` 对 peer 会话就是查这里，不去转发）。
    pub fn get(&self, gid: &str) -> Option<Value> {
        let peer = gid.split_once(':')?.0;
        lock(&self.inner)
            .peers
            .get(peer)?
            .rows
            .get(gid)
            .map(|r| r.json.clone())
    }

    /// 该 peer 的视图是否 stale；从没连上过（没有视图）是 None。
    pub fn is_stale(&self, peer: &str) -> Option<bool> {
        lock(&self.inner).peers.get(peer).map(|v| v.stale)
    }

    // ---------- 控制 ----------

    /// 客户端任务启动时登记自己的唤醒把手。
    pub fn register_wake(&self, peer: &str, wake: Arc<Wake>) {
        lock(&self.inner).wakes.insert(peer.to_owned(), wake);
    }

    pub fn wake(&self, peer: &str) -> Option<Arc<Wake>> {
        lock(&self.inner).wakes.get(peer).cloned()
    }

    /// 缩短一次退避等待（agora-7ku.6：用户点开 stale 会话）。没有这个 peer 的客户端 → false。
    pub fn retry_now(&self, peer: &str) -> bool {
        self.wake(peer).map(|w| w.retry_now()).is_some()
    }

    /// 让该 peer 的客户端立刻断开重连、重拉全量。
    pub fn reconnect(&self, peer: &str) -> bool {
        self.wake(peer).map(|w| w.reconnect()).is_some()
    }
}

/// `id` 是 `<peer>:<…>` 吗。
fn owned_by(peer: &str, id: &str) -> bool {
    id.strip_prefix(peer).is_some_and(|r| r.starts_with(':'))
}

/// 规则 1：行属于这个 peer 才收。返回全局 id。
fn accept(peer: &str, raw: &Value) -> Option<String> {
    let node = raw.get("node").and_then(Value::as_str);
    let id = raw.get("id").and_then(Value::as_str);
    match (node, id) {
        (Some(n), Some(i)) if n == peer && owned_by(peer, i) => Some(i.to_owned()),
        _ => {
            tracing::warn!(
                component = "peer",
                %peer,
                node = node.unwrap_or("?"),
                id = id.unwrap_or("?"),
                "peer 导出了不属于它的会话行，丢弃（MISSION §3.5 一跳）"
            );
            None
        }
    }
}

/// 规则 2 + 3：改写 `status_since` 为本机时刻（状态没变就沿用上次打的），置 `stale` / `last_seen`。
fn stamp(mut raw: Value, prev: Option<&Row>, now: i64, stale: bool, last_seen: Option<i64>) -> Row {
    let token = (
        raw.get("status").and_then(Value::as_str).map(str::to_owned),
        raw.get("status_since").and_then(Value::as_i64),
    );
    let since = match prev {
        Some(p) if p.token == token => p.since,
        _ => now,
    };
    set(&mut raw, "status_since", Value::from(since));
    mark(&mut raw, stale, last_seen);
    Row {
        json: raw,
        token,
        since,
    }
}

/// 规则 3 的两个键：`stale` 永远在；`last_seen` 只在 stale 且知道"上次见到"时在（UTC 文本，与
/// `PeerState` 序列化同形，前端可直接喂 Header 那个 `clockText`），否则**剥掉**——peer 报的行里
/// 就算带了 `last_seen`（它不该带；它导出的是本机行）也不许漏到我们的视图里。
fn mark(v: &mut Value, stale: bool, last_seen: Option<i64>) {
    let Value::Object(map) = v else { return };
    map.insert("stale".to_owned(), Value::Bool(stale));
    match last_seen.filter(|_| stale) {
        Some(t) => map.insert(
            "last_seen".to_owned(),
            Value::String(crate::clock::format_utc_secs(t)),
        ),
        None => map.remove("last_seen"),
    };
}

fn set(v: &mut Value, key: &str, val: Value) {
    if let Value::Object(map) = v {
        map.insert(key.to_owned(), val);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0: i64 = 1_788_393_600; // 2026-09-03T00:00:00Z（本机）
    const PEER_T: i64 = 1_600_000_000; // peer 报的：2020 年，明显漂移

    fn fixed(t: i64) -> PeerViews {
        PeerViews::with_clock(Arc::new(move || t))
    }

    fn row(id: &str, node: &str, status: &str, since: i64) -> Value {
        json!({ "id": id, "node": node, "status": status, "status_since": since, "alive": true })
    }

    #[test]
    fn rows_from_another_node_are_dropped_one_hop() {
        // 规则 1：peer b 把 c 的会话（或 id 前缀不对的行）导出来，收方也不收。
        let v = fixed(T0);
        let events = v.replace(
            "b",
            vec![
                row("b:1", "b", "running", PEER_T),
                row("c:9", "c", "waiting", PEER_T),
                row("c:8", "b", "waiting", PEER_T),
                json!({ "id": "b:2" }),
            ],
        );
        assert_eq!(v.rows().len(), 1, "{:?}", v.rows());
        assert_eq!(v.rows()[0]["id"], "b:1");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::SessionCreated { id, .. } if id == "b:1"));
        // 事件流里同样的行同样丢。
        assert!(matches!(
            v.apply(
                "b",
                &json!({ "type": "session_created", "id": "c:9", "session": row("c:9", "c", "waiting", 1) })
            ),
            Applied::Ignored
        ));
        assert!(matches!(
            v.apply("b", &json!({ "type": "session_removed", "id": "c:9" })),
            Applied::Ignored
        ));
        assert!(v.get("c:9").is_none());
    }

    #[test]
    fn status_since_is_stamped_by_the_local_clock_and_kept_while_the_status_holds() {
        // 规则 2：peer 说 2020 年，本机说 2026 年——行上是 2026 年。
        let v = fixed(T0);
        v.replace("b", vec![row("b:1", "b", "waiting", PEER_T)]);
        assert_eq!(v.get("b:1").unwrap()["status_since"], T0);
        // 重连后全量再来一次、状态没变（peer 报的 token 一样）：沿用第一次打的时刻，不重置成"刚刚"。
        let later = PeerViews {
            clock: Arc::new(|| T0 + 600),
            ..v.clone()
        };
        let events = later.replace("b", vec![row("b:1", "b", "waiting", PEER_T)]);
        assert_eq!(later.get("b:1").unwrap()["status_since"], T0);
        assert!(events.is_empty(), "内容没变就没有事件: {events:?}");
        // 状态变了：打新的本机时刻，而不是 peer 报的。
        let out = later.apply(
            "b",
            &json!({ "type": "status_changed", "id": "b:1", "status": "running", "status_since": PEER_T + 5, "source": "hook", "reason": null, "alive": true }),
        );
        let Applied::Publish(Event::SessionUpdated { id, session }) = out else {
            panic!("{out:?}");
        };
        assert_eq!(id, "b:1");
        assert_eq!(session["status"], "running");
        assert_eq!(session["status_since"], T0 + 600);
        assert_eq!(session["source"], "hook");
    }

    #[test]
    fn stale_flips_with_events_and_a_snapshot_clears_it() {
        // 规则 3 / 不变量 8：掉线行不删、标 stale 并带 last_seen；全量对齐回 false、last_seen 消失；
        // 每次翻转每行一条 session_updated。
        let v = fixed(T0);
        // peer 报的行里混进一个 last_seen（它不该有）：非 stale 时剥掉，不许漏进视图。
        let mut smuggled = row("b:2", "b", "idle", 2);
        smuggled["last_seen"] = json!("2020-01-01T00:00:00Z");
        v.replace("b", vec![row("b:1", "b", "running", 1), smuggled]);
        assert_eq!(v.is_stale("b"), Some(false));
        assert!(
            v.rows().iter().all(|r| r.get("last_seen").is_none()),
            "非 stale 行没有 last_seen: {:?}",
            v.rows()
        );
        let seen = T0 - 90; // 客户端从 PeerState.last_seen 取来的"上次见到"
        let events = v.mark_stale("b", Some(seen));
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| matches!(
            e,
            Event::SessionUpdated { session, .. }
                if session["stale"] == true && session["last_seen"] == "2026-09-02T23:58:30Z"
        )));
        assert_eq!(v.is_stale("b"), Some(true));
        assert!(v
            .rows()
            .iter()
            .all(|r| r["stale"] == true && r["last_seen"] == "2026-09-02T23:58:30Z"));
        assert!(
            v.mark_stale("b", Some(seen)).is_empty(),
            "已经 stale 不再发"
        );
        // 恢复：b:2 没了、b:1 还在、b:3 新来；stale 回 false、last_seen 键消失。
        let events = v.replace(
            "b",
            vec![row("b:1", "b", "running", 1), row("b:3", "b", "waiting", 3)],
        );
        assert_eq!(v.is_stale("b"), Some(false));
        assert!(
            v.rows()
                .iter()
                .all(|r| r["stale"] == false && r.get("last_seen").is_none()),
            "{:?}",
            v.rows()
        );
        let kinds: Vec<String> = events
            .iter()
            .map(|e| match e {
                Event::SessionCreated { id, .. } => format!("created {id}"),
                Event::SessionUpdated { id, session } => {
                    format!("updated {id} stale={}", session["stale"])
                }
                Event::SessionRemoved { id } => format!("removed {id}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            ["updated b:1 stale=false", "created b:3", "removed b:2"]
        );
        assert_eq!(v.is_stale("nobody"), None);
    }

    #[test]
    fn events_are_applied_and_forwarded_with_typed_fields() {
        let v = fixed(T0);
        v.replace("b", vec![row("b:1", "b", "running", 1)]);
        // 不认识的行的 status_changed → 重拉；resync → 重拉；pong → 无事。
        assert!(matches!(
            v.apply(
                "b",
                &json!({ "type": "status_changed", "id": "b:7", "status": "waiting" })
            ),
            Applied::Resync
        ));
        assert!(matches!(
            v.apply("b", &json!({ "type": "resync" })),
            Applied::Resync
        ));
        assert!(matches!(
            v.apply("b", &json!({ "type": "pong" })),
            Applied::Ignored
        ));
        // removed：视图里删掉并转发。
        let out = v.apply("b", &json!({ "type": "session_removed", "id": "b:1" }));
        assert!(matches!(out, Applied::Publish(Event::SessionRemoved { id }) if id == "b:1"));
        assert!(v.get("b:1").is_none());
        // notification：status 反序列化成枚举；不属于 b 的会话不转发。
        let out = v.apply(
            "b",
            &json!({ "type": "notification", "id": "b:1", "title": "Claude / x @ b needs input", "body": "Bash: rm", "status": "waiting" }),
        );
        let Applied::Publish(Event::Notification {
            id, title, status, ..
        }) = out
        else {
            panic!("{out:?}");
        };
        assert_eq!(id.as_deref(), Some("b:1"));
        assert_eq!(title, "Claude / x @ b needs input");
        assert_eq!(status, Some(crate::status::Status::Waiting));
        assert!(matches!(
            v.apply(
                "b",
                &json!({ "type": "notification", "id": "c:1", "title": "t", "body": "" })
            ),
            Applied::Ignored
        ));
        // decision_resolved：via 只认表里的值。
        assert!(matches!(
            v.apply(
                "b",
                &json!({ "type": "decision_resolved", "id": "b:1", "tool_use_id": "t1", "via": "dashboard" })
            ),
            Applied::Publish(Event::DecisionResolved {
                via: "dashboard",
                ..
            })
        ));
        assert!(matches!(
            v.apply(
                "b",
                &json!({ "type": "decision_resolved", "id": "b:1", "tool_use_id": "t1", "via": "telepathy" })
            ),
            Applied::Ignored
        ));
    }

    #[tokio::test]
    async fn wake_distinguishes_retry_from_forced_reconnect_and_views_signal_changes() {
        let v = fixed(T0);
        let mut rx = v.subscribe();
        assert!(!v.retry_now("b"), "还没有客户端登记");
        let w = Arc::new(Wake::default());
        v.register_wake("b", w.clone());
        assert!(v.retry_now("b"));
        assert!(!w.wait().await, "retry_now 不是强制");
        assert!(v.reconnect("b"));
        assert!(w.wait().await, "reconnect 是强制");
        // 视图每变一次 changed +1。
        let before = *rx.borrow_and_update();
        v.replace("b", vec![row("b:1", "b", "running", 1)]);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow_and_update(), before + 1);
    }
}
