//! daemon 侧：读投递箱、重放、挂起表（ADR-002 D3/D5）。
//!
//! 挂起的决定以 `(session, tool_use_id)` 为键。session 是 agora 的会话 id；外部会话
//! （没有 `AGORA_SESSION_ID`）用 `<host>:<agent_session_id>`，采纳（agora-dvh.12）时再对上。
//! 上限、超时、解除规则都在这里；哪些事件算解除、映射成什么事件，问宿主的 `AgentHooks`。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::adapter::{self, AgentHooks, Decision, Release};
use crate::events::{global_id, Event, EventBus};
use crate::local::Response;
use crate::session::{ExternalSession, SessionManager};
use crate::status::AgoraEvent;

use super::inbox::{Delivery, Inbox, DONE_RETENTION};
use super::HookError;

pub const MAX_HOLDS_PER_SESSION: usize = 8;
pub const MAX_HOLDS_PER_NODE: usize = 256;
/// 安装的 hook timeout 是 3600 s（ADR-002 D4）；agora 永远先于 agent 超时退出。
pub const HOLD_TIMEOUT: Duration = Duration::from_secs(55 * 60);

/// 一条已应用的事件：状态机（agora-dvh.4）的输入，现阶段只进账本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub session_key: String,
    pub event: Option<String>,
    pub delivery: Delivery,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RespondError {
    #[error("会话 {session} 没有挂起的决定 {tool_use_id}")]
    NoPendingDecision {
        session: String,
        tool_use_id: String,
    },
}

struct Hold {
    tx: oneshot::Sender<Decision>,
    since: Instant,
}

pub struct Receiver {
    inbox: Inbox,
    sessions: Arc<SessionManager>,
    holds: Mutex<HashMap<(String, String), Hold>>,
    ledger: Mutex<Vec<Received>>,
    hold_timeout: Duration,
    /// `/api/events` 的 `decision_resolved` 从这里发；没接总线就不发。
    events: Mutex<Option<(EventBus, Arc<str>)>>,
}

/// 账本里记的事件名：只给排障看，所以键名风格两种都认，不算核心层懂 payload。
fn event_name(payload: &serde_json::Value) -> Option<String> {
    ["hook_event_name", "hookEventName"]
        .iter()
        .find_map(|k| payload.get(k).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

/// 挂起键：agora 会话 id，或外部会话的 `<host>:<agent_session_id>`。
pub fn session_key(delivery: &Delivery) -> String {
    let env = &delivery.envelope;
    env.agora_session_id
        .clone()
        .unwrap_or_else(|| format!("{}:{}", env.host, env.agent_session_id))
}

impl Receiver {
    pub fn new(home: &Path, sessions: Arc<SessionManager>) -> Self {
        Receiver {
            inbox: Inbox::new(home),
            sessions,
            holds: Mutex::new(HashMap::new()),
            ledger: Mutex::new(Vec::new()),
            hold_timeout: HOLD_TIMEOUT,
            events: Mutex::new(None),
        }
    }

    /// 测试用：缩短挂起超时。
    pub fn with_hold_timeout(mut self, timeout: Duration) -> Self {
        self.hold_timeout = timeout;
        self
    }

    /// 接上事件总线：解除挂起时发 `decision_resolved`（会话 id 转成 `<node>:<id>`）。
    pub fn attach_events(&self, bus: EventBus, node: Arc<str>) {
        *self.events.lock().unwrap_or_else(|p| p.into_inner()) = Some((bus, node));
    }

    fn announce(&self, session: &str, tool_use_id: &str, via: &'static str) {
        let guard = self.events.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((bus, node)) = guard.as_ref() {
            bus.publish(Event::DecisionResolved {
                id: global_id(node, session),
                tool_use_id: tool_use_id.to_owned(),
                via,
            });
        }
    }

    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// 启动时重放（MISSION §3.4）：先验权限，再按文件名顺序应用全部待处理文件。
    /// 重放阶段没有 hook 进程在等，PermissionRequest 也只记账不挂起。返回应用的条数。
    pub fn replay(&self) -> Result<usize, HookError> {
        self.inbox.check_permissions()?;
        let mut n = 0;
        for path in self.inbox.pending()? {
            match self.ingest(&path) {
                Ok(Some(_)) => n += 1,
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(component = "hook", path = %path.display(), %err, "重放跳过");
                }
            }
        }
        self.inbox.prune_done(DONE_RETENTION);
        Ok(n)
    }

    /// 读一个文件并应用：epoch 旧的丢弃（返回 None），其余记账、解除挂起、移 done/。
    pub fn ingest(&self, path: &Path) -> Result<Option<Received>, HookError> {
        if !self.inbox.contains(path) {
            return Err(HookError::Outside(path.display().to_string()));
        }
        let delivery = self.inbox.read(path)?;
        let stale = self.is_stale(&delivery);
        self.inbox.done(path)?;
        if stale {
            tracing::debug!(component = "hook", session = %session_key(&delivery), "旧 epoch 的事件丢弃");
            return Ok(None);
        }
        // 宿主名来自 `--host`，`parse_args` 已校验；文件是别人手写的才会走到 Outside。
        let Some(hooks) = adapter::for_host(&delivery.envelope.host) else {
            return Err(HookError::Outside(format!(
                "{}（宿主 {} 不认识）",
                path.display(),
                delivery.envelope.host
            )));
        };
        // agora 起的会话带 AGORA_SESSION_ID；其余按 (host, agent_session_id) 找或登记（MISSION §5.4），
        // 找到了挂起键就用 agora 的 id——这样它的 allow / deny 与 agora 起的会话走同一条路。
        let (id, epoch) = match &delivery.envelope.agora_session_id {
            Some(id) => (Some(id.clone()), delivery.envelope.agora_epoch.unwrap_or(0)),
            None => (self.locate_external(hooks, &delivery), 0),
        };
        let key = id.clone().unwrap_or_else(|| session_key(&delivery));
        match hooks.release_for(&delivery.payload) {
            Release::ToolUse(keys) => {
                for k in keys {
                    self.resolve(&key, &k, Decision::None, "terminal");
                }
            }
            Release::Session => self.resolve_session(&key, "session"),
            Release::None => {}
        }
        if let Some(id) = &id {
            let events = hooks.parse(&delivery.payload);
            // 外部会话没有 epoch 概念：按库里那代算，永不因 epoch 被丢。
            let epoch = if delivery.envelope.agora_session_id.is_some() {
                epoch
            } else {
                self.sessions.record(id).map(|r| r.epoch).unwrap_or(epoch)
            };
            if let Err(err) = self.sessions.apply_hook(id, epoch, &events) {
                tracing::debug!(component = "hook", session = %id, %err, "hook 事件没有对应会话");
            }
        }
        let received = Received {
            session_key: key,
            event: event_name(&delivery.payload),
            delivery,
        };
        self.ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(received.clone());
        Ok(Some(received))
    }

    /// 无 AGORA_* 的事件找它的会话：先按 (host, agent 自报 id) 查；没有就登记——信封里的运行时
    /// 环境能定位到可采纳 socket 上的 pane 就有终端，否则是无句柄的 external。agent 没自报 id
    /// 的（`unknown`）不登记：那会每条事件造一行垃圾。
    fn locate_external(&self, hooks: &dyn AgentHooks, delivery: &Delivery) -> Option<String> {
        let env = &delivery.envelope;
        if env.agent_session_id == "unknown" {
            return None;
        }
        let found = match self
            .sessions
            .find_by_agent_session(&env.host, &env.agent_session_id)
        {
            Ok(Some(rec)) => Some(rec.id),
            Ok(None) => {
                let runtime_ref = match self.sessions.runtime().locate(&env.runtime_env) {
                    Ok(r) => r.map(|r| r.0),
                    Err(err) => {
                        tracing::debug!(component = "hook", %err, "按运行时环境定位 pane 失败");
                        None
                    }
                };
                let spec = ExternalSession {
                    agent_type: env.host.clone(),
                    agent_session_id: env.agent_session_id.clone(),
                    runtime_ref,
                    working_directory: hooks.working_directory(&delivery.payload),
                };
                match self.sessions.register_external(&spec) {
                    // `session_created` 由事件监视器的下一 tick 差分发出，这里不重复广播。
                    Ok(id) => {
                        tracing::info!(component = "hook", session = %id, host = %env.host,
                            terminal = spec.runtime_ref.is_some(), "登记外部会话");
                        Some(id)
                    }
                    Err(err) => {
                        tracing::warn!(component = "hook", %err, "登记外部会话失败");
                        None
                    }
                }
            }
            Err(err) => {
                tracing::warn!(component = "hook", %err, "查外部会话失败");
                None
            }
        };
        if let (Some(id), Some(pid)) = (&found, hooks.agent_pid(&env.agent_env, env.ppid)) {
            self.sessions.note_external_pid(id, pid);
        }
        found
    }

    /// 旧 epoch（Restart 之前那代进程发的）不得污染新进程（ADR-002 "什么会让它变危险"）。
    /// 会话不在库里的不按 epoch 过滤——外部会话没有 epoch 概念。
    fn is_stale(&self, delivery: &Delivery) -> bool {
        let env = &delivery.envelope;
        let (Some(id), Some(epoch)) = (&env.agora_session_id, env.agora_epoch) else {
            return false;
        };
        self.sessions
            .record(id)
            .map(|rec| epoch < rec.epoch)
            .unwrap_or(false)
    }

    /// socket 唤醒的处理：应用文件；要挂起的事件等决定直到解除或超时。
    pub async fn wake(self: &Arc<Self>, path: &Path) -> Response {
        let received = match self.ingest(path) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Response::Hook {
                    decision: Decision::None,
                }
            }
            Err(err) => {
                tracing::warn!(component = "hook", path = %path.display(), %err, "唤醒被拒");
                return Response::Error {
                    message: err.to_string(),
                };
            }
        };
        let Some(tool_use_id) = adapter::for_host(&received.delivery.envelope.host)
            .and_then(|h| h.hold_key(&received.delivery.payload))
        else {
            return Response::Hook {
                decision: Decision::None,
            };
        };
        let Some(rx) = self.hold(&received.session_key, &tool_use_id) else {
            return Response::Hook {
                decision: Decision::None,
            };
        };
        let decision = match tokio::time::timeout(self.hold_timeout, rx).await {
            Ok(Ok(d)) => d,
            // 发送端被丢（同键被新挂起接替）：那边已经宣布过了。
            Ok(Err(_)) => Decision::None,
            Err(_) => {
                self.resolve(
                    &received.session_key,
                    &tool_use_id,
                    Decision::None,
                    "timeout",
                );
                Decision::None
            }
        };
        Response::Hook { decision }
    }

    /// 登记一个挂起；超上限返回 None（hook 立即 exit 0）。
    fn hold(&self, session: &str, tool_use_id: &str) -> Option<oneshot::Receiver<Decision>> {
        let mut holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
        let per_session = holds.keys().filter(|(s, _)| s == session).count();
        if holds.len() >= MAX_HOLDS_PER_NODE || per_session >= MAX_HOLDS_PER_SESSION {
            tracing::warn!(component = "hook", session, "挂起超上限，放行给终端");
            return None;
        }
        let (tx, rx) = oneshot::channel();
        // 同键重复到达（重试）：旧的那个解除，新的接替。
        holds.insert(
            (session.to_owned(), tool_use_id.to_owned()),
            Hold {
                tx,
                since: Instant::now(),
            },
        );
        Some(rx)
    }

    fn resolve(
        &self,
        session: &str,
        tool_use_id: &str,
        decision: Decision,
        via: &'static str,
    ) -> bool {
        let hold = self
            .holds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(session.to_owned(), tool_use_id.to_owned()));
        match hold {
            Some(h) => {
                let _ = h.tx.send(decision);
                self.announce(session, tool_use_id, via);
                true
            }
            None => false,
        }
    }

    /// Stop / SessionEnd / 进程退出：该会话全部挂起解除（`decision.resolved`）。
    pub fn resolve_session(&self, session: &str, via: &'static str) {
        let drained: Vec<_> = {
            let mut holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            let keys: Vec<_> = holds
                .keys()
                .filter(|(s, _)| s == session)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| holds.remove(&k).map(|h| (k.1, h)))
                .collect()
        };
        for (tool_use_id, h) in drained {
            let _ = h.tx.send(Decision::None);
            self.announce(session, &tool_use_id, via);
        }
    }

    /// Dashboard 的 allow / deny（agora-dvh.9）：`tool_use_id` 缺省取最早登记的那个。
    /// 解除后顺手告诉状态机——hook 侧的 PostToolUse 稍后才到，行不该多等那一拍。
    pub fn respond(
        &self,
        session: &str,
        tool_use_id: Option<&str>,
        decision: Decision,
    ) -> Result<String, RespondError> {
        let key = match tool_use_id {
            Some(k) => k.to_owned(),
            None => self.pending(session).into_iter().next().unwrap_or_default(),
        };
        if !self.resolve(session, &key, decision, "dashboard") {
            return Err(RespondError::NoPendingDecision {
                session: session.to_owned(),
                tool_use_id: key,
            });
        }
        if let Ok(rec) = self.sessions.record(session) {
            let _ = self.sessions.apply_hook(
                session,
                rec.epoch,
                &[AgoraEvent::DecisionResolved(Some(key.clone()))],
            );
        }
        Ok(key)
    }

    /// 某会话当前挂起的 tool_use_id，按登记时间排序。
    pub fn pending(&self, session: &str) -> Vec<String> {
        let holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<_> = holds
            .iter()
            .filter(|((s, _), _)| s == session)
            .map(|((_, t), h)| (h.since, t.clone()))
            .collect();
        v.sort();
        v.into_iter().map(|(_, t)| t).collect()
    }

    pub fn hold_count(&self) -> usize {
        self.holds.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// 定期扫：超时的与进程已退出的会话解除挂起。`wake` 自己也有超时，这里是双保险，
    /// 主要为进程退出——没有事件会替死掉的 agent 发 SessionEnd。
    pub fn sweep(&self) {
        let expired: Vec<(String, String)> = {
            let holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            holds
                .iter()
                .filter(|(_, h)| h.since.elapsed() >= self.hold_timeout)
                .map(|(k, _)| k.clone())
                .collect()
        };
        for (s, t) in expired {
            self.resolve(&s, &t, Decision::None, "timeout");
        }
        let sessions: Vec<String> = {
            let holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            let mut v: Vec<_> = holds.keys().map(|(s, _)| s.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        for s in sessions {
            // 外部会话（key 带宿主前缀）不在库里，get 报 NotFound：不当作退出。
            if let Ok(view) = self.sessions.get(&s) {
                if !view.alive {
                    self.resolve_session(&s, "exit");
                }
            }
        }
    }

    pub async fn run_sweeper(self: Arc<Self>, every: Duration) {
        loop {
            tokio::time::sleep(every).await;
            let me = self.clone();
            let _ = tokio::task::spawn_blocking(move || me.sweep()).await;
        }
    }

    /// 已应用的事件（测试与 agora-dvh.4 之前的排障用）。
    pub fn received(&self) -> Vec<Received> {
        self.ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn received_for(&self, session: &str) -> Vec<Received> {
        self.received()
            .into_iter()
            .filter(|r| r.session_key == session)
            .collect()
    }
}
