//! daemon 侧：读投递箱、重放、挂起表（ADR-002 D3/D5）。
//!
//! 挂起的决定以 `(session, tool_use_id)` 为键。session 是 agora 的会话 id；外部会话
//! （没有 `AGORA_SESSION_ID`）用 `<host>:<agent_session_id>`，采纳（agora-dvh.12）时再对上。
//! 上限、超时、解除规则都在这里；哪些事件算解除、映射成什么事件，问宿主的 `AgentHooks`。

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use tokio::sync::oneshot;

use crate::adapter::{self, AgentHooks, Decision, Release};
use crate::events::{global_id, Event, EventBus};
use crate::local::Response;
use crate::session::{ExternalSession, Origin, PendingDecision, SessionManager};
use crate::status::{AgoraEvent, HOST_END_CLEAR};

use super::inbox::{delivery_time_secs, now_unix_ms, Delivery, Inbox, DONE_RETENTION};
use super::HookError;

pub const MAX_HOLDS_PER_SESSION: usize = 8;
pub const MAX_HOLDS_PER_NODE: usize = 256;
/// 默认挂起上限；每个宿主可以更短（`AgentHooks::hold_timeout`，Codex 是秒级），永远先于
/// agent 的 hook timeout 退出。
pub const HOLD_TIMEOUT: Duration = crate::adapter::hooks::DEFAULT_HOLD_TIMEOUT;
/// hold 登记后多久内 sweep 不拿"状态机里没这个键"当解除依据（agora-9cd）。`begin_wake` 先 ingest
/// （事件进状态机、`pending` 插入键）再 hold，正常顺序下 hold 一登记状态机里就有键；这 2 s 只是保险，
/// 免得把刚登记、状态机那边还没来得及看见的 hold 当孤儿放掉。
pub const HOLD_SETTLE: Duration = Duration::from_secs(2);
/// 两次归档清理之间至少隔多久。sweep 每 5 s 一轮，扫目录不该跟着这个频率走；保留期本身是
/// 24 h（`DONE_RETENTION`），迟一个小时删对排障没有影响（agora-t36）。
pub const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);
/// 兜底重放（[`Receiver::replay_stale_pending`]）只碰落盘早于 N 秒的投递件。为什么要留这道宽限：
/// 投递件刚写下的那几毫秒里，它的主人往往已经连上 socket、正等 daemon 读它（`hook::cmd::deliver`）。
/// 兜底这条路走的是 `ingest`，而 `ingest` 不登记挂起（登记在 `begin_wake`）：抢先把文件挪进
/// `done/`，那条连接会拿到"路径不在投递箱里"的错误、那次权限请求也永远不会出现在 Dashboard 上
/// （hook fail-open 退 0，人只在终端里被问，所以是降级不是崩）。N 要盖住"活 daemon 从落盘到 ingest"
/// 的最坏延迟：非挂起事件的客户端只等 2 s（`cmd::ACK_TIMEOUT`），挂起事件在 accept 之后立刻 ingest
/// 并把文件挪走，所以 30 s 是十倍以上的余量；真滞留下来的（2026-09-18 现场是几分钟到几天）等得起。
pub const PENDING_REPLAY_AGE: Duration = Duration::from_secs(30);

/// 一条已应用的事件：状态机（agora-dvh.4）的输入，现阶段只进账本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub session_key: String,
    pub event: Option<String>,
    pub delivery: Delivery,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RespondError {
    #[error("保存 hook 答复失败: {0}")]
    Checkpoint(String),
    #[error("会话 {session} 没有挂起的决定 {tool_use_id}")]
    NoPendingDecision {
        session: String,
        tool_use_id: String,
    },
}

struct Hold {
    request_id: String,
    epoch: i64,
    tx: oneshot::Sender<Decision>,
    since: Instant,
    /// 这一条的上限：宿主给的与 daemon 配置里较短的那个。
    timeout: Duration,
}

struct HeldWake {
    session: String,
    tool_use_id: String,
    request_id: String,
    timeout: Duration,
    rx: oneshot::Receiver<Decision>,
}

pub struct Receiver {
    inbox: Inbox,
    processing: Mutex<()>,
    sessions: Arc<SessionManager>,
    holds: Mutex<HashMap<(String, String), Hold>>,
    ledger: Mutex<Vec<Received>>,
    hold_timeout: Duration,
    /// `/api/events` 的 `decision_resolved` 从这里发；没接总线就不发。
    events: Mutex<Option<(EventBus, Arc<str>)>>,
    /// 归档清理（agora-t36）：保留期、两次清理之间的最小间隔、上次跑完的时刻。
    /// `last_prune = None` = 还没在本进程里跑过。
    done_retention: Duration,
    prune_interval: Duration,
    last_prune: Mutex<Option<Instant>>,
    /// 兜底重放的宽限期（[`PENDING_REPLAY_AGE`]）；测试用它把它缩到毫秒级。
    pending_sweep_age: Duration,
    /// 兜底重放已经 warn 过的读不动 / 应用不上的投递件：同一个坏文件每 5 s 撞一次，
    /// 不该刷 5 s 一条日志。只在内存里，文件消失就忘掉。
    pending_bad: Mutex<BTreeSet<PathBuf>>,
    /// 投递箱权限是否已经 warn 过：第一轮 warn，之后降到 debug，直到它被修好。
    inbox_perm_warned: AtomicBool,
}

/// 账本里记的事件名：只给排障看，所以键名风格两种都认，不算核心层懂 payload。
/// 无句柄的 external 行（`runtime_ref` NULL）以 (host, agent_session_id) 为身份：Claude 的 /clear 发
/// 旧 id 的 SessionEnd(reason=clear) 后紧接着**新 id** 的 SessionStart，新 id 在 `locate_external` 里
/// 查不到就另起一行，旧行从此再没有任何事件。状态机对 `clear` 的"不改状态"例外（agora-vfi）是给
/// 同一行继续的会话设计的——agora 起的会话 id 是 agora 的，有 pane 的行 `register_external` 按
/// runtime_ref 复用同一行——对这种行它就错了：旧行钉在最后一个状态，没有 pane 所以沉默规则够不着，
/// CLAUDE_PID 还在跑新会话所以进程层也不会说结束（2026-09-07 实测：/clear 两次，旧行 working 16 min+，
/// agora-s3r）。所以在这里把 `clear` 改写成别的 reason，让状态机按普通结束处理：这一行到此为止，
/// 新行由新 id 的 SessionStart 登记。
const CLEAR_ENDS_EXTERNAL_ROW: &str = "clear (external row: the new id lands on a new row)";

fn clear_ends_external_row(events: &[AgoraEvent]) -> Vec<AgoraEvent> {
    events
        .iter()
        .map(|e| match e {
            AgoraEvent::SessionEnded(Some(r)) if r == HOST_END_CLEAR => {
                AgoraEvent::SessionEnded(Some(CLEAR_ENDS_EXTERNAL_ROW.to_owned()))
            }
            e => e.clone(),
        })
        .collect()
}

/// 这两个 reason 说的是"进程把身份换成了另一个对话 id"，不是"这一行的对话跑完了"：Claude 的
/// `/clear`、`/resume` 与 CLI 带 `--resume` 起进程（2.1.x 实测，换对话 / 退法的事件表在
/// `src/adapter/` 里 Claude 那一支的模块文档）。
const IDENTITY_HANDOFF_REASONS: [&str; 2] = ["resume", "clear"];

/// 这条投递件里的 SessionEnd 是不是身份交接（reason ∈ `IDENTITY_HANDOFF_REASONS`）。
fn ends_on_identity_handoff(events: &[AgoraEvent]) -> bool {
    events.iter().any(|e| {
        matches!(e, AgoraEvent::SessionEnded(Some(r))
            if IDENTITY_HANDOFF_REASONS.contains(&r.as_str()))
    })
}

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
        sessions.enable_hook_checkpoints(home);
        Receiver {
            processing: Mutex::new(()),
            inbox: Inbox::new(home),
            sessions,
            holds: Mutex::new(HashMap::new()),
            ledger: Mutex::new(Vec::new()),
            hold_timeout: HOLD_TIMEOUT,
            events: Mutex::new(None),
            done_retention: DONE_RETENTION,
            prune_interval: PRUNE_INTERVAL,
            last_prune: Mutex::new(None),
            pending_sweep_age: PENDING_REPLAY_AGE,
            pending_bad: Mutex::new(BTreeSet::new()),
            inbox_perm_warned: AtomicBool::new(false),
        }
    }

    /// 测试用：缩短挂起超时。
    pub fn with_hold_timeout(mut self, timeout: Duration) -> Self {
        self.hold_timeout = timeout;
        self
    }

    /// 归档清理的两个旋钮（`hooks.inbox_retention` / `hooks.prune_interval`）；
    /// 间隔 `Duration::ZERO` = 每次 sweep 都扫。
    pub fn with_pruning(mut self, retention: Duration, interval: Duration) -> Self {
        self.done_retention = retention;
        self.prune_interval = interval;
        self
    }

    /// 测试用：只改节流间隔，保留期仍是默认的 24 h。
    pub fn with_prune_interval(mut self, interval: Duration) -> Self {
        self.prune_interval = interval;
        self
    }

    /// 测试用：只改兜底重放的宽限期（默认 [`PENDING_REPLAY_AGE`]）。
    /// `Duration::ZERO` = 每轮 sweep 都把投递箱里的东西全拿了应用。
    pub fn with_pending_sweep_age(mut self, age: Duration) -> Self {
        self.pending_sweep_age = age;
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
        self.sessions
            .restore_hook_checkpoints()
            .map_err(|e| HookError::Io {
                path: "hooks/state".into(),
                source: std::io::Error::other(e),
            })?;
        self.restore_archive()?;
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
        match self.sessions.supersede_external_rows() {
            Ok(ended) if !ended.is_empty() => {
                tracing::info!(component = "hook", rows = ?ended, "同一进程换了对话，旧行结束");
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(component = "hook", %err, "取代旧行失败"),
        }
        // 走同一条节流：启动这次一定跑（还没记过时刻），之后一小时内的 sweep 不重复扫。
        self.maybe_prune_done();
        Ok(n)
    }

    /// 兜底重放：把投递箱里落盘已够久、还没人消费的投递件按时间序应用一遍，返回应用条数。
    ///
    /// 启动时 [`Self::replay`] 只看那一刻的快照，而 socket 在那之前的 reconcile / 重放（同步等、
    /// 2026-09-18 现场 172 s）之前就 bind 了：bind 与 accept 之间到达的 hook 连接排内核 backlog
    /// （std UnixListener 是 128），任何一个没排上队的（backlog 满、connect 被拒、客户端被宿主按
    /// timeout 杀掉）投递件就再没人读，一直等到下次重启——2026-09-18 Mac 上 6 个 Pre/PostToolUse
    /// 躺在投递箱那个会话的目录里至今没进 done（MISSION §5.1「hook 事件在 daemon 不在时不得
    /// 丢失」不等价于「daemon 忙着的时候也不得丢失」）。两个入口共用这一条：serve 起来之后补扫一次
    /// （`main.rs`），之后 sweep 每周期兜底一次。
    ///
    /// 可重复跑：`ingest` 应用成功就把文件挪进 `done/`；就算挪之前崩了，检查点里的"已处理文件名"
    /// 会让状态机在同名文件再应用时返回 `Ok(false)`（`apply_delivered_hook`）。只碰落盘早于
    /// [`PENDING_REPLAY_AGE`] 的文件，理由见那个常量。
    pub fn replay_stale_pending(&self) -> usize {
        if let Err(err) = self.inbox.check_inbox_permissions() {
            // 启动那道权限门在这条路上也得拦着（只查 `hooks/` 根与 `inbox/` 那一支，不陪着扫 `done/`）：
            // 不拦就等于 `replay()` 拒绝读的投递箱会在下一个 sweep 周期被这里悄悄消费掉。
            if self.inbox_perm_warned.swap(true, Ordering::Relaxed) {
                tracing::debug!(component = "hook", %err, "投递箱权限仍然过宽，兜底重放跳过");
            } else {
                tracing::warn!(component = "hook", %err, "投递箱权限过宽，兜底重放跳过");
            }
            return 0;
        }
        self.inbox_perm_warned.store(false, Ordering::Relaxed);
        let pending = match self.inbox.pending() {
            Ok(p) => p,
            Err(err) => {
                tracing::debug!(component = "hook", %err, "扫投递箱失败，兜底重放这一轮跳过");
                return 0;
            }
        };
        let mut applied = 0;
        for path in pending {
            if !self.pending_file_is_due(&path) {
                continue;
            }
            match self.ingest(&path) {
                // Ok(None)：旧 epoch、或检查点已处理过同名文件——ingest 已把文件挪进 done/。
                Ok(Some(_)) => applied += 1,
                Ok(None) => {}
                Err(err) => {
                    // 读不动 / 解析不了 / 库出错的：留在原地等下一轮，但同一个路径只 warn 一次
                    // （sweep 每 5 s 一轮，坏文件会一直撞在这里）。
                    let mut bad = self.pending_bad.lock().unwrap_or_else(|p| p.into_inner());
                    if bad.insert(path.clone()) {
                        tracing::warn!(component = "hook", path = %path.display(), %err, "兜底重放跳过");
                    } else {
                        tracing::debug!(component = "hook", path = %path.display(), %err, "兜底重放仍然跳过");
                    }
                }
            }
        }
        // 已经被消费掉 / 被 prune 掉的坏文件不该在本进程里记一辈子。
        {
            let mut bad = self.pending_bad.lock().unwrap_or_else(|p| p.into_inner());
            bad.retain(|p| p.exists());
        }
        if applied > 0 {
            tracing::info!(
                component = "hook",
                replayed = applied,
                "兜底重放消费了滞留的投递件"
            );
            // 与 `replay` 尾部同一条：补进来的事件里可能有新的 external 行，把同一进程上一行的
            // 对话结束掉。
            match self.sessions.supersede_external_rows() {
                Ok(ended) if !ended.is_empty() => {
                    tracing::info!(component = "hook", rows = ?ended, "同一进程换了对话，旧行结束");
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(component = "hook", %err, "取代旧行失败"),
            }
        }
        applied
    }

    /// 这份投递件够不够老、可以被兜底重放拿走（宽限期的理由见 [`PENDING_REPLAY_AGE`]）。
    /// mtime 读不动或晚于现在（时钟回拨）→ 退回文件名里的 ts（事件自己的时刻）；连 ts 都没有
    /// （手写的、名字不是 `<ts>-<seq>.json`）就当够老：真实 hook 写的那两份时间戳都在，落这一档的
    /// 不会是正有人在 socket 上等答复的东西。
    fn pending_file_is_due(&self, path: &Path) -> bool {
        if self.pending_sweep_age.is_zero() {
            return true; // 测试用：不设宽限期
        }
        let now = now_unix_ms();
        let limit = self.pending_sweep_age.as_millis() as u64;
        let written = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .or_else(|| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                delivery_time_secs(&name).map(|s| s.max(0) as u64 * 1000)
            });
        match written {
            // 未来的时间戳（机器时钟往回拨过）也当够老：它不是刚写下等人拿的东西。
            Some(ts) if ts <= now => now - ts >= limit,
            _ => true,
        }
    }

    /// 首次升级没有检查点时，利用仍保留的 done 文件补建；之后不依赖这份排障归档。
    fn restore_archive(&self) -> Result<(), HookError> {
        let mut restore = HashMap::new();
        for path in self.inbox.completed()? {
            let delivery = match self.inbox.read(&path) {
                Ok(d) => d,
                Err(err) => {
                    tracing::warn!(component = "hook", %err, "跳过损坏的归档事件");
                    continue;
                }
            };
            let env = &delivery.envelope;
            let Some(hooks) = adapter::for_host(&env.host) else {
                continue;
            };
            let rec = match &env.agora_session_id {
                Some(id) => self.sessions.record(id).ok(),
                None => self
                    .sessions
                    .find_by_agent_session(&env.host, &env.agent_session_id)
                    .ok()
                    .flatten(),
            };
            let Some(rec) = rec else {
                continue;
            };
            let handleless_external = rec.origin.is_handleless() && rec.runtime_ref.is_none();
            let selected = *restore.entry(rec.id.clone()).or_insert_with(|| {
                if !self.sessions.has_hook_checkpoint(&rec.id) {
                    return true;
                }
                // agora-tql：无句柄 external 行的 v1 检查点可能停在 s3r 修复前的结论（SessionEnd(clear)
                // 当时不改状态，2026-09-08 现场 8 行僵尸），归档里还有它的投递就丢掉检查点、
                // 从归档按序重建——重建走下面同一条路，`clear` 同样被改写成普通结束。
                let legacy = handleless_external && self.sessions.hook_checkpoint_is_legacy(&rec.id);
                if legacy {
                    tracing::info!(component = "hook", session = %rec.id, "v1 检查点的 external 行从归档重建");
                    self.sessions.forget_hook_state(&rec.id);
                }
                legacy
            });
            if !selected || env.agora_epoch.is_some_and(|epoch| epoch != rec.epoch) {
                continue;
            }
            if env.agora_session_id.is_none() {
                if let Some(pid) = hooks.agent_pid(&env.agent_env, env.ppid) {
                    self.sessions
                        .note_external_pid(&rec.id, pid, env.received_unix_ms as i64);
                }
            }
            let events = hooks.parse(&delivery.payload);
            let events: Cow<'_, [AgoraEvent]> = if handleless_external {
                Cow::Owned(clear_ends_external_row(&events))
            } else {
                Cow::Borrowed(&events)
            };
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if let Err(err) = self
                .sessions
                .apply_delivered_hook(&rec.id, rec.epoch, &events, &name)
            {
                tracing::warn!(component = "hook", %err, "归档恢复失败");
            }
        }
        Ok(())
    }

    /// 读一个文件并应用：epoch 旧的丢弃（返回 None），其余记账、解除挂起、移 done/。
    pub fn ingest(&self, path: &Path) -> Result<Option<Received>, HookError> {
        let _guard = self.processing.lock().unwrap_or_else(|p| p.into_inner());
        self.ingest_inner(path)
    }

    fn ingest_inner(&self, path: &Path) -> Result<Option<Received>, HookError> {
        if !self.inbox.contains(path) {
            return Err(HookError::Outside(path.display().to_string()));
        }
        let delivery = self.inbox.read(path)?;
        let stale = self.is_stale(&delivery);
        if stale {
            self.inbox.done(path)?;
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
        // 事件只 parse 一次：要不要登记外部会话得先看它（locate_external），应用也用它。
        let events = hooks.parse(&delivery.payload);
        // agora 起的会话带 AGORA_SESSION_ID；其余按 (host, agent_session_id) 找或登记（MISSION §5.4），
        // 找到了挂起键就用 agora 的 id——这样它的 allow / deny 与 agora 起的会话走同一条路。
        let (id, epoch) = match &delivery.envelope.agora_session_id {
            Some(id) => (Some(id.clone()), delivery.envelope.agora_epoch.unwrap_or(0)),
            None => (self.locate_external(hooks, &delivery, &events), 0),
        };
        let key = id.clone().unwrap_or_else(|| session_key(&delivery));
        if let Some(id) = &id {
            // 外部会话没有 epoch 概念：按库里那代算，永不因 epoch 被丢。
            let rec = if delivery.envelope.agora_session_id.is_some() {
                None
            } else {
                self.sessions.record(id).ok()
            };
            let epoch = rec.as_ref().map(|r| r.epoch).unwrap_or(epoch);
            // 身份交接不是结束（agora-29n，2026-09-18 zuan 现场 e3cd17）：Claude 的 `--resume` 先以**新 id**
            // 发 SessionStart(source=startup)，`locate_external` 查不到就登记出一行，12 s 后
            // SessionEnd(reason=resume) 落到这一行上——它没有 prompt、没有输出，从此钉成一行谁也没起过的
            // FINISHED。无句柄 external 行的身份就是 agent id，resume / clear 换 id 时这个新行注定是空壳；
            // 删行，让它像从没被登记过（求差器下一 tick 发 `session_removed`，已经见过这一行的页面跟着掉）。
            // 判据是状态机没见过第一条 PromptSubmitted（`hook_prompt_seen`），不是"刚登记"：现场那一行
            // 也可能在 SessionStart 与 SessionEnd 之间挂几小时（zuan 的 ef0e50 钉过 180 h STARTING）。
            // 有过对话的行不走这一支，仍是 `clear_ends_external_row` 改写 reason 后按普通结束
            // （s3r 那条守卫在 `tests/hooks_external.rs` 的 clear 用例里，它红就说明越界了）。
            // 只碰无句柄的 external 行：有 pane 的行身份是 runtime_ref，SessionEnd 之后同一进程再发的
            // SessionStart 落回同一行（`src/status/machine.rs` 的 `SessionEnded` 注释），那是真会话的
            // resume，删掉就是把人正在用的行抹了。
            let handoff = rec.as_ref().is_some_and(|r| r.runtime_ref.is_none())
                && ends_on_identity_handoff(&events)
                && !self.sessions.hook_prompt_seen(id);
            if handoff {
                match self.sessions.delete_metadata(id) {
                    Ok(()) => {
                        tracing::info!(component = "hook", session = %id,
                            host = %delivery.envelope.host,
                            agent_session = %delivery.envelope.agent_session_id,
                            "external 行在第一条 prompt 之前遇到 SessionEnd(resume|clear)：身份交接，删行而不是 FINISHED");
                        self.inbox.done(path)?;
                        return Ok(None);
                    }
                    // 删不动（库出错）就别吞事件：退回原来的路，行停在 FINISHED 比丢掉这条投递件好。
                    Err(err) => tracing::warn!(component = "hook", session = %id, %err,
                        "删身份交接的 external 行失败，按普通结束处理"),
                }
            }
            let events: Cow<'_, [AgoraEvent]> = match &rec {
                Some(r) if r.runtime_ref.is_none() => Cow::Owned(clear_ends_external_row(&events)),
                _ => Cow::Borrowed(&events),
            };
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let applied = self
                .sessions
                .apply_delivered_hook(id, epoch, &events, &name);
            if matches!(applied, Ok(false)) {
                self.inbox.done(path)?;
                return Ok(None);
            }
            if let Err(err) = applied {
                if matches!(err, crate::session::SessionError::NotFound(_)) {
                    tracing::debug!(component = "hook", session = %id, %err, "hook 事件没有对应会话");
                } else {
                    return Err(HookError::Io {
                        path: path.display().to_string(),
                        source: std::io::Error::other(err),
                    });
                }
            }
        }
        match hooks.release_for(&delivery.payload) {
            Release::ToolUse(keys) => {
                for k in keys {
                    self.resolve(&key, &k, Decision::None, "terminal");
                }
            }
            Release::Session => self.resolve_session(&key, "session"),
            Release::None => {}
        }
        // 2026-09-05 agora-9dj：检查点成功之后才消费文件，写盘失败留在 inbox 供重试。
        self.inbox.done(path)?;
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
    /// 环境能定位到可采纳 socket 上的 pane 就有终端，否则是无句柄的 external。两种投递件不登记：
    /// agent 没自报 id 的（`unknown`），那会每条事件造一行垃圾；`events` 为空或只有 SessionEnded 的，
    /// 那是 agora 没见过的会话在结束——hook 装好之前起的会话退出、Codex Desktop 结束一个线程——
    /// 登记只会造一行永远没有后续事件的僵尸（2026-09-05 侧栏那行 devcenter，agora-vfi）。三家宿主
    /// 同一规则，不按 host 分叉。不登记的投递件后面照常走：按 `<host>:<agent_session_id>` 解挂起、
    /// 进 done、记账本。
    fn locate_external(
        &self,
        hooks: &dyn AgentHooks,
        delivery: &Delivery,
        events: &[AgoraEvent],
    ) -> Option<String> {
        let env = &delivery.envelope;
        if env.agent_session_id == "unknown" {
            return None;
        }
        let mut registered = false;
        let found = match self
            .sessions
            .find_by_agent_session(&env.host, &env.agent_session_id)
        {
            Ok(Some(rec)) => Some(rec.id),
            Ok(None)
                if events
                    .iter()
                    .all(|e| matches!(e, AgoraEvent::SessionEnded(_))) =>
            {
                tracing::debug!(component = "hook", host = %env.host,
                    agent_session = %env.agent_session_id, events = events.len(),
                    "没见过的会话的 SessionEnd（或不产生事件的投递件），不登记");
                None
            }
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
                    // 这一行从那条事件起算，不是从 daemon 把它写进库起算：停机期间攒下的投递
                    // 重放时，写库那一刻全落在重放那两分钟里（agora-5gg.3，2026-09-19 Mac 实测）。
                    created_at: Some(env.received_unix_ms as i64 / 1000),
                    // 宿主自己起的一次性会话 / 子代理登记成 `headless`（裁决 agora-5gg.7 选 B）：
                    // 判据只看这一条投递件的载荷结构，录不出差异的宿主（Codex exec / Desktop：
                    // SessionStart 与交互逐键相同）就照常落 `external`。
                    origin: if hooks.is_headless(&delivery.payload) {
                        Origin::Headless
                    } else {
                        Origin::External
                    },
                };
                match self.sessions.register_external(&spec) {
                    // `session_created` 由事件监视器的下一 tick 差分发出，这里不重复广播。
                    Ok(id) => {
                        tracing::info!(component = "hook", session = %id, host = %env.host,
                            terminal = spec.runtime_ref.is_some(), "登记外部会话");
                        registered = true;
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
            self.sessions
                .note_external_pid(id, pid, env.received_unix_ms as i64);
            if registered {
                match self.sessions.supersede_external_rows() {
                    Ok(ended) if !ended.is_empty() => {
                        tracing::info!(component = "hook", session = %id, rows = ?ended,
                            "同一进程换了对话，旧行结束");
                    }
                    Ok(_) => {}
                    Err(err) => tracing::warn!(component = "hook", %err, "取代旧行失败"),
                }
            }
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

    /// 文件 I/O 与运行时查询在 blocking 线程，挂起等待留在 async 侧。
    pub async fn wake(self: &Arc<Self>, path: &Path) -> Response {
        let me = self.clone();
        let path = path.to_owned();
        let held = match tokio::task::spawn_blocking(move || me.begin_wake(&path)).await {
            Ok(Ok(held)) => held,
            Ok(Err(response)) => return response,
            Err(err) => {
                return Response::Error {
                    message: format!("hook worker: {err}"),
                }
            }
        };
        let decision = match tokio::time::timeout(held.timeout, held.rx).await {
            Ok(Ok(d)) => d,
            Ok(Err(_)) => Decision::None,
            Err(_) => {
                self.resolve_request(
                    &held.session,
                    &held.tool_use_id,
                    Some(&held.request_id),
                    Decision::None,
                    "timeout",
                );
                Decision::None
            }
        };
        Response::Hook { decision }
    }

    fn begin_wake(&self, path: &Path) -> Result<HeldWake, Response> {
        // 2026-09-05：应用事件与登记挂起不可被解除事件插入；锁不跨 await。
        let _guard = self.processing.lock().unwrap_or_else(|p| p.into_inner());
        let none = || Response::Hook {
            decision: Decision::None,
        };
        let received = self
            .ingest_inner(path)
            .map_err(|err| {
                tracing::warn!(component = "hook", path = %path.display(), %err, "唤醒被拒");
                Response::Error {
                    message: err.to_string(),
                }
            })?
            .ok_or_else(none)?;
        let hooks = adapter::for_host(&received.delivery.envelope.host).ok_or_else(none)?;
        let tool_use_id = hooks
            .hold_key(&received.delivery.payload)
            .ok_or_else(none)?;
        let timeout = hooks.hold_timeout().min(self.hold_timeout);
        let summary = hooks
            .parse(&received.delivery.payload)
            .into_iter()
            .find_map(|e| match e {
                AgoraEvent::DecisionNeeded { summary, .. } => Some(summary),
                _ => None,
            })
            .unwrap_or_else(|| tool_use_id.clone());
        let (request_id, rx) = self
            .hold(
                &received.session_key,
                hooks.host(),
                &tool_use_id,
                &summary,
                timeout,
            )
            .ok_or_else(none)?;
        Ok(HeldWake {
            session: received.session_key,
            tool_use_id,
            timeout,
            request_id,
            rx,
        })
    }

    /// 登记一个挂起；超上限返回 None（hook 立即 exit 0）。
    fn hold(
        &self,
        session: &str,
        host: &str,
        tool_use_id: &str,
        summary: &str,
        timeout: Duration,
    ) -> Option<(String, oneshot::Receiver<Decision>)> {
        let mut holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
        let per_session = holds.keys().filter(|(s, _)| s == session).count();
        if holds.len() >= MAX_HOLDS_PER_NODE || per_session >= MAX_HOLDS_PER_SESSION {
            tracing::warn!(component = "hook", session, "挂起超上限，放行给终端");
            return None;
        }
        let (tx, rx) = oneshot::channel();
        let request_id = crate::adapter::resume::new_conversation_id();
        let epoch = self.sessions.record(session).map(|r| r.epoch).unwrap_or(0);
        // 同键重复到达（重试）：旧的那个解除，新的接替。
        if let Some(old) = holds.insert(
            (session.to_owned(), tool_use_id.to_owned()),
            Hold {
                request_id: request_id.clone(),
                epoch,
                tx,
                since: Instant::now(),
                timeout,
            },
        ) {
            self.sessions
                .remove_pending_decision(session, &old.request_id);
        }
        self.sessions.add_pending_decision(
            session,
            PendingDecision {
                request_id: request_id.clone(),
                summary: summary.into(),
                epoch,
                host: host.into(),
            },
        );
        Some((request_id, rx))
    }

    fn resolve(
        &self,
        session: &str,
        tool_use_id: &str,
        decision: Decision,
        via: &'static str,
    ) -> bool {
        self.resolve_request(session, tool_use_id, None, decision, via)
    }

    fn resolve_request(
        &self,
        session: &str,
        tool_use_id: &str,
        request_id: Option<&str>,
        decision: Decision,
        via: &'static str,
    ) -> bool {
        let hold = {
            let mut holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            let key = (session.to_owned(), tool_use_id.to_owned());
            if request_id.is_some_and(|id| holds.get(&key).is_none_or(|h| h.request_id != id)) {
                return false;
            }
            let hold = holds.remove(&key);
            if let Some(h) = &hold {
                self.sessions
                    .remove_pending_decision(session, &h.request_id);
            }
            hold
        };
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
            self.sessions
                .remove_pending_decision(session, &h.request_id);
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
        self.respond_inner(session, tool_use_id, None, decision)
    }

    pub fn respond_request(
        &self,
        session: &str,
        request_id: &str,
        decision: Decision,
    ) -> Result<String, RespondError> {
        self.respond_inner(session, None, Some(request_id), decision)
    }

    fn respond_inner(
        &self,
        session: &str,
        tool_use_id: Option<&str>,
        request_id: Option<&str>,
        decision: Decision,
    ) -> Result<String, RespondError> {
        let _guard = self.processing.lock().unwrap_or_else(|p| p.into_inner());
        let epoch = self.sessions.record(session).map(|r| r.epoch).unwrap_or(0);
        let key = if let Some(request_id) = request_id {
            self.holds
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .find(|((s, _), h)| s == session && h.request_id == request_id && h.epoch == epoch)
                .map(|((_, k), _)| k.clone())
                .ok_or_else(|| RespondError::NoPendingDecision {
                    session: session.into(),
                    tool_use_id: request_id.into(),
                })?
        } else {
            match tool_use_id {
                Some(k) => k.to_owned(),
                None => self.pending(session).into_iter().next().unwrap_or_default(),
            }
        };
        let held = {
            let mut holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            let map_key = (session.to_owned(), key.clone());
            let valid = holds.get(&map_key).is_some_and(|h| {
                h.epoch == epoch
                    && request_id.is_none_or(|r| r == h.request_id)
                    && h.since.elapsed() < h.timeout
                    && !h.tx.is_closed()
            });
            if !valid {
                return Err(RespondError::NoPendingDecision {
                    session: session.into(),
                    tool_use_id: key,
                });
            }
            if let Ok(rec) = self.sessions.record(session) {
                self.sessions
                    .apply_hook(
                        session,
                        rec.epoch,
                        &[AgoraEvent::DecisionResolved(Some(key.clone()))],
                    )
                    .map_err(|e| RespondError::Checkpoint(e.to_string()))?;
            }
            let held = holds
                .remove(&map_key)
                .expect("validated hold under same lock");
            self.sessions
                .remove_pending_decision(session, &held.request_id);
            held
        };
        let _ = held.tx.send(decision);
        self.announce(session, &key, "dashboard");
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

    /// 定期扫：先把落盘够久却没人消费的投递件补投一遍（`replay_stale_pending`，socket 那条路
    /// 漏掉的东西在这儿兜底），再解超时与进程已退出的挂起。`wake` 自己也有超时，这里是双保险，
    /// 主要为进程退出——没有事件会替死掉的 agent 发 SessionEnd。顺带按 `PRUNE_INTERVAL`
    /// 节流清一次归档与无行的 hook 检查点：原先 `prune_done` 只在 `replay()` 尾部跑，而 replay 只在
    /// 启动时跑一次，于是"保留 24 h"实际成了"下次重启时清掉 24 h 以前的"，daemon 不重启就无界增长
    /// （2026-09-10 现场：开发机 done/ 87 MB / 12274 文件，靠频繁重启才没露馅；agora-t36）。
    /// 无行检查点的清理是同一只手顺带扫到的另一堆文件（agora-5gg.14，见 `maybe_prune_done`）。
    pub fn sweep(&self) {
        // 先补投递箱再处理挂起：兜底重放可能正把解除事件（PostToolUse / Stop / SessionEnd）补进来，
        // 后面那几段以"状态机里没这个键"放掉 hold，读到已经更新的挂起表才不会误放。
        self.replay_stale_pending();
        self.maybe_prune_done();
        let expired: Vec<((String, String), String)> = {
            let holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            holds
                .iter()
                .filter(|(_, h)| h.since.elapsed() >= h.timeout)
                .map(|(k, h)| (k.clone(), h.request_id.clone()))
                .collect()
        };
        for ((s, t), request_id) in expired {
            self.resolve_request(&s, &t, Some(&request_id), Decision::None, "timeout");
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
        // 状态机已经没有这个挂起、hook 进程却还在 socket 上等（agora-9cd，2026-09-06）：终端里放行后
        // 按 Esc，Claude 2.1.261 一个事件都不发，状态机靠"屏幕上的提示消失"清掉挂起（machine.rs
        // observe_hooked）；下一条 prompt 的 PromptSubmitted 也会清挂起而 `release_for` 不解 hold。
        // 两种情况下 hold 都会挂到 55 min 超时，Dashboard 的 Allow / Deny 按钮一直在。这里以
        // `Decision::None` 放掉：hook fail-open 退出（Claude 早已走自己的路，无害）、`pending_decision`
        // 移除、`decision_resolved` via=terminal——人是在终端动的手。只看同一 epoch 的 hold：Restart
        // 之后旧代的 hold 本来就答不了（`respond_inner` 按 epoch 拒），交给超时，不冒充"终端答了"。
        // 带 request_id 解除：sweep 读到的快照与解除之间若同键被新请求接替，不误放新的那个。
        let settled: Vec<((String, String), String, i64)> = {
            let holds = self.holds.lock().unwrap_or_else(|p| p.into_inner());
            holds
                .iter()
                .filter(|(_, h)| h.since.elapsed() >= HOLD_SETTLE)
                .map(|(k, h)| (k.clone(), h.request_id.clone(), h.epoch))
                .collect()
        };
        for ((s, t), request_id, epoch) in settled {
            let Ok(rec) = self.sessions.record(&s) else {
                continue;
            };
            if rec.epoch != epoch || self.sessions.pending_hook_keys(&s).contains(&t) {
                continue;
            }
            if self.resolve_request(&s, &t, Some(&request_id), Decision::None, "terminal") {
                tracing::info!(component = "hook", session = %s, tool_use_id = %t,
                    "状态机已无此挂起（终端里答过或中断了），放掉还在等的 hook");
            }
        }
    }

    /// 距上次清理够久了才真的扫目录；时刻先记后扫，扫得慢也不会两轮叠在一起。
    /// 两条清理共用这一条节流（名字只说了归档，事实是两条，agora-5gg.14）：归档与孤儿检查点都是
    /// 「扫一次目录」，都不该跟着 5 s 一轮的 sweep 走（agora-t36 同一条理由）。
    fn maybe_prune_done(&self) {
        {
            let mut last = self.last_prune.lock().unwrap_or_else(|p| p.into_inner());
            if last.is_some_and(|t| t.elapsed() < self.prune_interval) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.inbox.prune_done(self.done_retention);
        // 无行的 hook 检查点：没有行就没人会加载它们（`restore_hook_checkpoints` 按行迭代），删了
        // 不丢东西——它们已经不可恢复。只在内存里判不出来源，所以这一行要能把文件名交给排障的人。
        // 失败只 warn：下一轮 sweep 还走这条路，不值得报事故。
        match self.sessions.prune_orphan_hook_checkpoints() {
            Ok(files) if !files.is_empty() => {
                tracing::info!(
                    component = "hook",
                    files = ?files,
                    "hooks/state/ 里没有对应行的检查点已删（文件名是 id 的逐字节 hex）"
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(component = "hook", %err, "清理无行的 hook 检查点失败，下一轮再试")
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
