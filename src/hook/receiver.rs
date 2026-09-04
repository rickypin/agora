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

use crate::adapter::{self, Decision, Release};
use crate::local::Response;
use crate::session::SessionManager;

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
        let key = session_key(&delivery);
        let stale = self.is_stale(&delivery);
        self.inbox.done(path)?;
        if stale {
            tracing::debug!(component = "hook", session = %key, "旧 epoch 的事件丢弃");
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
        match hooks.release_for(&delivery.payload) {
            Release::ToolUse(keys) => {
                for k in keys {
                    self.resolve(&key, &k, Decision::None);
                }
            }
            Release::Session => self.resolve_session(&key),
            Release::None => {}
        }
        // 进状态机：只有 agora 起的 / 采纳的会话在库里；外部会话等 dvh.12 采纳后再对上。
        if let Some(id) = &delivery.envelope.agora_session_id {
            let events = hooks.parse(&delivery.payload);
            if let Err(err) =
                self.sessions
                    .apply_hook(id, delivery.envelope.agora_epoch.unwrap_or(0), &events)
            {
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
        let decision = match tokio::time::timeout(HOLD_TIMEOUT, rx).await {
            Ok(Ok(d)) => d,
            // 发送端被丢（解除）或超时：都没有决定。
            _ => {
                self.resolve(&received.session_key, &tool_use_id, Decision::None);
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

    fn resolve(&self, session: &str, tool_use_id: &str, decision: Decision) -> bool {
        let hold = self
            .holds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(session.to_owned(), tool_use_id.to_owned()));
        match hold {
            Some(h) => {
                let _ = h.tx.send(decision);
                true
            }
            None => false,
        }
    }

    /// Stop / SessionEnd / 进程退出：该会话全部挂起解除（`decision.resolved`）。
    pub fn resolve_session(&self, session: &str) {
        let drained: Vec<_> = {
            let mut holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            let keys: Vec<_> = holds
                .keys()
                .filter(|(s, _)| s == session)
                .cloned()
                .collect();
            keys.into_iter().filter_map(|k| holds.remove(&k)).collect()
        };
        for h in drained {
            let _ = h.tx.send(Decision::None);
        }
    }

    /// Dashboard 的 allow / deny（agora-dvh.9 的 respond 走这里）。
    pub fn respond(
        &self,
        session: &str,
        tool_use_id: &str,
        decision: Decision,
    ) -> Result<(), RespondError> {
        if self.resolve(session, tool_use_id, decision) {
            Ok(())
        } else {
            Err(RespondError::NoPendingDecision {
                session: session.to_owned(),
                tool_use_id: tool_use_id.to_owned(),
            })
        }
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
                .filter(|(_, h)| h.since.elapsed() >= HOLD_TIMEOUT)
                .map(|(k, _)| k.clone())
                .collect()
        };
        for (s, t) in expired {
            self.resolve(&s, &t, Decision::None);
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
                    self.resolve_session(&s);
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
