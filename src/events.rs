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

use crate::session::{Origin, SessionManager, SessionView};
use crate::status::{EndCause, ProcessState, Source, Status, UnknownCause};

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
        /// 结束 / 说不清的**类型**（封闭枚举，agora-5gg.6）：`reason` 是给人看的那一句，
        /// 措辞可以随版本变，调用方按这两个字段分支而不做字符串匹配（MISSION §2.3 规则 10）。
        /// 与 `status` 一致：FINISHED / FAILED 带 `end_cause`、UNKNOWN 带 `unknown_cause`，其余两个皆 null
        /// （修复前写的 hook 检查点恢复出来的结束行除外，见 `Assessment::end_cause`）。
        end_cause: Option<EndCause>,
        unknown_cause: Option<UnknownCause>,
        /// 进程三态（Q4，agora-5gg.18）。`alive` 是它的布尔投影、只保留一版：两个都带，
        /// 未升级的页面读 `alive`、升级了的读 `process`。
        process: ProcessState,
        alive: bool,
        /// hook 给的问题 / 最后一条回复；变了也算状态变化（就地回答要显示它）。
        detail: Option<String>,
        /// 两行预览与 pane 兜底（MISSION §6.3）：变了也算状态变化。
        prompt: Option<String>,
        progress: Option<String>,
        preview: Option<String>,
        /// 当前状态的起点（unix 秒）。
        status_since: i64,
        /// "hook 没接上"提示（agora-dvh.15）：出现 / 消失都算状态变化。
        hooks_unheard: Option<String>,
    },
    /// 挂起的决定被解除（ADR-002 D5）：`via` 是 dashboard / terminal / session / exit / timeout。
    DecisionResolved {
        id: String,
        tool_use_id: String,
        via: &'static str,
    },
    /// 浏览器通知（MISSION §6.6；A18）：只在 RUNNING → WAITING / TURN_DONE / FINISHED / FAILED
    /// 这四种转换上发一条；`status` 是转换后的状态，前端据此决定点击落到哪（WAITING →
    /// 就地回答）。`id` 为 None 是不针对会话的系统通知（暂未使用，形态保留）。
    Notification {
        id: Option<String>,
        title: String,
        body: String,
        status: Option<Status>,
    },
    /// 一个 peer 在本节点眼里的面貌变了（agora-c8h；模型 `crate::peer::state`）：`peer` 就是
    /// `/api/health` peers 段里该节点的那一项，Header 据此立刻改点，不等 health 的 60 s 轮询。
    /// 只在 `online` / `retrying` / `last_error` 变时发；在线期间 `last_seen` 的刷新不发。
    PeerChanged {
        name: String,
        peer: crate::peer::state::PeerState,
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
            // 同一 peer 一批里上上下下只留最后一眼（断线即重连那种抖动）。
            Event::PeerChanged { name, .. } => Some(("peer", name)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
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
    /// 结束 / 说不清的类型：与 reason 一起进比对（同一句话被改成另一种 end_cause 是要人看一眼的）。
    end_cause: Option<EndCause>,
    unknown_cause: Option<UnknownCause>,
    /// 进程三值与布尔投影一起记：只带 `alive` 的旧进程看不出 `gone` 与 `unknown` 的区别，
    /// 而那一切换了是要发事件的（盘点 B2）。
    process: ProcessState,
    alive: bool,
    detail: Option<String>,
    prompt: Option<String>,
    progress: Option<String>,
    preview: Option<String>,
    status_since: i64,
    hooks_unheard: Option<String>,
    /// 任务标签是异步补齐的（`task/`）：到了要整行重发。
    task: Option<crate::task::TaskInfo>,
    /// 项目身份同样异步补齐（`project/index`）：到了要整行重发，否则浏览器永远看不到。
    project: Option<crate::project::ProjectInfo>,
    pending_decision: Option<crate::session::PendingDecision>,
    /// agent 自报的对话 id（`/clear` 后会换）：Settings 里"当前对话"要跟着变（dvh.13）。
    agent_session_id: Option<String>,
}

fn seen(v: &SessionView) -> Seen {
    Seen {
        status: v.assessment.status,
        source: v.assessment.source,
        reason: v.assessment.reason.clone(),
        end_cause: v.assessment.end_cause.clone(),
        unknown_cause: v.assessment.unknown_cause,
        process: v.process,
        alive: v.alive,
        detail: v.detail.clone(),
        prompt: v.prompt.clone(),
        progress: v.progress.clone(),
        preview: v.preview.clone(),
        status_since: v.status_since,
        hooks_unheard: v.hooks_unheard.clone(),
        task: v.task.clone(),
        project: v.project.clone(),
        pending_decision: v.pending_decision.clone(),
        agent_session_id: v.record.agent_session_id.clone(),
    }
}

/// 求差器：喂它每一轮的列表，吐出该发的事件。与 tokio 无关，便于单测。
#[derive(Debug)]
pub struct Differ {
    last: HashMap<String, Seen>,
    primed: bool,
    /// `notifications.enabled`：关掉后只是不发 `notification`，状态事件照发。
    notifications: bool,
}

impl Default for Differ {
    fn default() -> Self {
        Differ::new(true)
    }
}

/// 四种值得打扰人的转换（MISSION §6.6 的表）：从 RUNNING 出发，IDLE 出发也算——IDLE 只是"安静了
/// 一阵"（无 hook 会话 idle_after 后就是它），之后再死或再提问同样需要人；2026-09-04 人眼验收时
/// `sleep 70; exit 1` 因为先过了 60 s 的 IDLE 而一条没弹。RUNNING → IDLE / UNKNOWN 不算
/// （那是 agora 没把握，不是 agent 需要人）；STARTING → FAILED 也不算（起不来会立刻在列表里看到）；
/// TURN_DONE → FINISHED 也不算（2026-09-04 实测：Claude 的 /exit 与 Kill 都是从 TURN_DONE 退出，
/// 人刚收过 "finished its turn"，再来一条 "finished" 只是噪音）。用户自己按的 Kill
/// （`end_cause = killed_by_user`）即使从 RUNNING 落到 FINISHED 也不通知：是他自己干的。
/// 驻留时间在状态层已经处理（ADR-002 D1），这里看到的转换都是稳定的；同一会话每次真的从
/// RUNNING 落到这四态各发一条，不去重——每次权限请求都需要人。
/// external 行人自己在终端里结束的（宿主的 `host_session_end`、换对话的 `superseded`）
/// 同 Kill 一个道理——是他自己干的，不通知（agora-j4w.4；MISSION §4.6 证据 ②）；只有进程消失
/// （`end_cause = process_gone`：崩溃、被杀、关窗口不发 SessionEnd 的那一家——bd memories
/// `external-exit-hooks-ctrlc-vs-hup` 2026-09-08 实测，接受它被当成意外通知一次）才通知。
/// agora / adopted 行的 FINISHED 通知不变。
///
/// 上面两条判据从 agora-5gg.6 起读 [`EndCause`] 而不读 `reason` 文本：以前第一条款是
/// `reason.starts_with("killed by user")`、第二条是 `source != process`，前者拿一句人话做判断
/// （MISSION §2.3 规则 10 的形状），后者分不出"宿主说了结束"与"只是没人报结束"。
/// 结论形状一字未改，守卫见 `notification_for_branches_on_end_cause_not_on_reason_text`。
/// `origin = headless` 的行一种转换都不通知（裁决 agora-5gg.7 选 B，agora-5gg.20）：宿主自己起的
/// 无头一轮与内部子代理不是等人的会话。
/// 一次状态转换的事实（求差器已比对过的那一行）。
#[derive(Debug, Clone, Copy)]
pub struct Transition<'a> {
    pub id: &'a str,
    pub agent_type: &'a str,
    /// 用户起的名字（display_name），不是 pane title。
    pub name: &'a str,
    pub node: &'a str,
    pub origin: Origin,
    pub prev: Status,
    pub next: Status,
    /// 结束的类型（封闭枚举）。只有落到 FINISHED / FAILED 的转换上才有值，其余转换是 None。
    /// 这里故意**不带** `reason`：通知该不该发是一个判断，判断只能拿类型做（规则 10）。
    pub end_cause: Option<&'a EndCause>,
    pub detail: Option<&'a str>,
}

pub fn notification_for(t: Transition<'_>) -> Option<Event> {
    let Transition {
        id,
        agent_type,
        name,
        node,
        origin,
        prev,
        next,
        end_cause,
        detail,
    } = t;
    if !matches!(prev, Status::Running | Status::Idle)
        || matches!(end_cause, Some(EndCause::KilledByUser))
    {
        return None;
    }
    if origin == Origin::Headless {
        // 宿主自己起的无头一轮 / 子代理（裁决 agora-5gg.7 选 B）：任何转换都不弹。
        // 它们不是一条等人的会话，Mac 现场一次 handoff 就能堆七行（agora-5gg.7 盘点），
        // 每一条 WAITING / TURN_DONE 都弹一下，人的通知会被它们淹没。
        return None;
    }
    if origin == Origin::External
        && next == Status::Finished
        && !matches!(end_cause, Some(EndCause::ProcessGone))
    {
        return None;
    }
    let verb = match next {
        Status::Waiting => "needs input",
        Status::TurnDone => "finished its turn",
        Status::Finished => "finished",
        Status::Failed => "failed",
        _ => return None,
    };
    Some(Event::Notification {
        id: Some(id.to_owned()),
        title: format!("{} / {name} @ {node} {verb}", agent_label(agent_type)),
        body: detail
            .map(|d| d.lines().next().unwrap_or("").trim().to_owned())
            .unwrap_or_default(),
        status: Some(next),
    })
}

/// 通知标题里的 agent 名首字母大写（§6.6 的表：agent_type 小写，标题里是专名）。
fn agent_label(agent_type: &str) -> String {
    let mut c = agent_type.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

impl Differ {
    pub fn new(notifications: bool) -> Self {
        Differ {
            last: HashMap::new(),
            primed: false,
            notifications,
        }
    }

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
                // task_ref 是 metadata，标签又是异步查回来的：整行重发最省事也最不会漏字段。
                // project 同理（agora-uvd.1）。对话 id 也是（metadata 不是状态，/clear 后会换，dvh.13）。
                Some(prev)
                    if prev.task != s.task
                        || prev.project != s.project
                        || prev.agent_session_id != s.agent_session_id
                        || prev.pending_decision != s.pending_decision =>
                {
                    out.push(Event::SessionUpdated {
                        id: gid.clone(),
                        session: export(node, v),
                    })
                }
                Some(prev) if *prev != s => {
                    out.push(Event::StatusChanged {
                        id: gid.clone(),
                        status: s.status,
                        source: s.source,
                        reason: s.reason.clone(),
                        end_cause: s.end_cause.clone(),
                        unknown_cause: s.unknown_cause,
                        process: s.process,
                        alive: s.alive,
                        detail: s.detail.clone(),
                        prompt: s.prompt.clone(),
                        progress: s.progress.clone(),
                        preview: s.preview.clone(),
                        status_since: s.status_since,
                        hooks_unheard: s.hooks_unheard.clone(),
                    });
                }
                _ => {}
            }
            if let Some(prev) = self.last.get(&gid) {
                if self.notifications {
                    // 标题里用用户起的名字而不是 pane title：后者是 agent 随手改的
                    // （"✳ 创建文件 x.txt"），2026-09-04 实测通知里读起来像任务不像会话。
                    let name = if v.record.display_name.is_empty() {
                        v.name.as_str()
                    } else {
                        v.record.display_name.as_str()
                    };
                    out.extend(notification_for(Transition {
                        id: &gid,
                        agent_type: &v.record.agent_type,
                        name,
                        node,
                        origin: v.record.origin,
                        prev: prev.status,
                        next: s.status,
                        end_cause: s.end_cause.as_ref(),
                        detail: s.detail.as_deref(),
                    }));
                }
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
    notifications: bool,
) {
    let mut differ = Differ::new(notifications);
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let s = sessions.clone();
        // list 会起运行时子进程：放 blocking 线程（ADR-001 D8）。先 sweep 再 list：sweep 删掉的行
        // 在同一 tick 的求差里就消失，求差器发的 `session_removed` 与 DELETE /api/sessions/:id
        // 之后客户端看到的是同一条事件，这里不另发一条（agora-j4w.3）。
        match tokio::task::spawn_blocking(move || {
            match s.sweep(crate::clock::now_secs()) {
                Ok(removed) if !removed.is_empty() => {
                    tracing::info!(
                        component = "session",
                        removed = removed.len(),
                        "过期 external 行已删"
                    )
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(component = "session", %err, "过期扫描失败，下个周期再试")
                }
            }
            s.list()
        })
        .await
        {
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
    use crate::status::HostEndReason;

    fn status(id: &str, st: Status) -> Event {
        Event::StatusChanged {
            id: id.into(),
            status: st,
            source: Source::Process,
            reason: None,
            end_cause: None,
            unknown_cause: None,
            process: ProcessState::Alive,
            alive: true,
            detail: None,
            prompt: None,
            progress: None,
            preview: None,
            status_since: 0,
            hooks_unheard: None,
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
    fn notifications_only_on_the_four_transitions_out_of_running() {
        use Status::*;
        let n = |prev, next| {
            notification_for(Transition {
                id: "n:1",
                agent_type: "myagent",
                name: "frontend",
                node: "zuan",
                origin: Origin::Agora,
                prev,
                next,
                end_cause: None,
                detail: Some("Bash: rm -rf x\n第二行"),
            })
        };
        let title = |e: Option<Event>| match e {
            Some(Event::Notification {
                title,
                body,
                status,
                ..
            }) => (title, body, status),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            title(n(Running, Waiting)),
            (
                "Myagent / frontend @ zuan needs input".to_owned(),
                "Bash: rm -rf x".to_owned(),
                Some(Waiting)
            )
        );
        assert_eq!(
            title(n(Running, TurnDone)).0,
            "Myagent / frontend @ zuan finished its turn"
        );
        assert_eq!(
            title(n(Running, Finished)).0,
            "Myagent / frontend @ zuan finished"
        );
        assert_eq!(
            title(n(Running, Failed)).0,
            "Myagent / frontend @ zuan failed"
        );
        for (p, q) in [
            (Running, Idle),
            (Running, Unknown),
            (Running, Running),
            (Starting, Failed),
            (Waiting, TurnDone),
            (TurnDone, Finished),
            (Idle, Running),
        ] {
            assert!(n(p, q).is_none(), "{p:?} → {q:?} 不该通知");
        }
        // IDLE 出发也通知：安静一阵之后再提问 / 死掉，人同样需要知道。
        assert_eq!(title(n(Idle, Failed)).0, "Myagent / frontend @ zuan failed");
        assert_eq!(title(n(Idle, Waiting)).2, Some(Waiting));
        assert!(
            notification_for(Transition {
                id: "n:1",
                agent_type: "myagent",
                name: "f",
                node: "z",
                origin: Origin::Agora,
                prev: Running,
                next: Finished,
                // 用户在 Dashboard 按过 Kill：process_layer / runtime_gone 都会给这一档（agora-5gg.6）。
                end_cause: Some(&EndCause::KilledByUser),
                detail: None,
            })
            .is_none(),
            "用户自己 Kill 的不通知"
        );
    }

    #[test]
    fn external_rows_ended_by_hook_do_not_notify_but_process_gone_does() {
        // agora-j4w.4：人在终端里自己结束的 external 会话（宿主的 SessionEnd、换对话的 superseded）
        // 不弹 "finished"；只有进程消失才弹。agora / adopted 行的 FINISHED 照旧。
        // 守卫：关掉 notification_for 里 origin + end_cause 那一块 → 前两段 is_none 断言红。
        use Status::*;
        let n = |origin, cause| {
            notification_for(Transition {
                id: "n:1",
                agent_type: "myagent",
                name: "ext",
                node: "mac",
                origin,
                prev: Running,
                next: Finished,
                end_cause: Some(cause),
                detail: None,
            })
        };
        // 数组 + 引用：避开创循环变量又要在断言消息里用它时的生命周期抹不平。
        const ENDED_BY_HOST: &[EndCause] = &[
            EndCause::HostSessionEnd(HostEndReason::Exit),
            EndCause::HostSessionEnd(HostEndReason::Clear),
            EndCause::Superseded,
        ];
        for cause in ENDED_BY_HOST {
            let muted = n(Origin::External, cause).is_none();
            assert!(muted, "external 行由宿主说的结束是人自己干的：{cause:?}");
        }
        assert!(
            n(Origin::External, &EndCause::ProcessGone).is_some(),
            "进程消失（崩溃 / 宿主不发 SessionEnd 的那一家关窗口）才是意外，要通知"
        );
        const ANY_END: &[EndCause] = &[
            EndCause::HostSessionEnd(HostEndReason::Exit),
            EndCause::ExitCode(0),
            EndCause::ProcessGone,
        ];
        for cause in ANY_END {
            let agora = n(Origin::Agora, cause).is_some();
            let adopted = n(Origin::Adopted, cause).is_some();
            assert!(agora, "agora 起的行不受影响：{cause:?}");
            assert!(adopted, "adopted 行同：{cause:?}");
        }
    }

    #[test]
    fn notification_for_branches_on_end_cause_not_on_the_reason_text() {
        // agora-5gg.6（MISSION §2.3 规则 10）：通知的两条判断以前是 `reason.starts_with("killed by
        // user")` 与 `source != process`——前者拿给人看的一句话做判断，后者靠来源层猜"人说了没说"。
        // 现在 Transition 里根本没有 reason 这一格（编译期就拿不进去），判断只看 end_cause：
        // - 同样一句人话（现场上是同一行 killed by user，`reason` 一字不变）end_cause 不是
        //   killed_by_user → 照通知；
        // - 同样一句人话换成 killed_by_user → 不通知。
        // 守卫：把两条 end_cause 判断退回成 reason 文本匹配 → 编译不过（没有 reason 这一格）；
        // 把 external 那一块退回成 `source != Process` → 第二段 is_none 断言红（hook 说的
        // process_gone 与人自己退出的行长得一样）。
        let n = |origin, cause, detail| {
            notification_for(Transition {
                id: "n:1",
                agent_type: "myagent",
                name: "row",
                node: "mac",
                origin,
                prev: Status::Running,
                next: Status::Finished,
                end_cause: Some(cause),
                detail,
            })
        };
        // 人话一模一样的两行（同一行 Kill 后运行时报 143）：唯一区别是 end_cause。
        let the_same_sentence = "killed by user (exit code 143)";
        assert!(
            n(
                Origin::Agora,
                &EndCause::KilledByUser,
                Some(the_same_sentence)
            )
            .is_none(),
            "killed_by_user 不通知"
        );
        assert!(
            n(
                Origin::Agora,
                &EndCause::ExitCode(143),
                Some(the_same_sentence)
            )
            .is_some(),
            "同一句人话，但类型不是用户杀的 → 照通知：判的是枚举不是文本"
        );
        // external 行的分档同理不再看来源层：hook 报的进程消失要通知，process 报的宿主结束不通知。
        assert!(
            n(Origin::External, &EndCause::ProcessGone, None).is_some(),
            "hook 还是 process 报的都不相干，process_gone 就通知"
        );
        assert!(
            n(
                Origin::External,
                &EndCause::HostSessionEnd(HostEndReason::Exit),
                None,
            )
            .is_none(),
            "宿主说了结束就是人自己干的，哪怕这一条结论由哪一层说不清"
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

    /// 无 hook 会话的一行视图：状态、reason 与起点都取自真实的状态机，不手写——手写两份一样的行
    /// 什么都测不出来，这条守卫要抓的正是状态机吐出的 reason 每 tick 不同（agora-385）。
    fn shell_view(m: &crate::status::Machine, process: ProcessState) -> SessionView {
        SessionView {
            record: crate::session::SessionRecord {
                id: "s1".into(),
                runtime_ref: Some("fake:agora:s1".into()),
                display_name: "shell-01".into(),
                name_locked: false,
                agent_type: "shell".into(),
                working_directory: None,
                worktree: None,
                task_ref: None,
                command: None,
                agent_session_id: None,
                epoch: 1,
                transcript_path: None,
                created_at: String::new(),
                spawned_at: None,
                ended_at: None,
                ended_at_approximate: false,
                killed_at: None,
                updated_at: String::new(),
                origin: crate::session::Origin::Agora,
            },
            name: "shell-01".into(),
            process,
            // 旧布尔永远是三值的投影（agora-5gg.18）：造行也不手写两个独立的事实。
            alive: process == ProcessState::Alive,
            exit: None,
            pid: Some(1),
            managed: true,
            assessment: m.current().clone(),
            detail: None,
            pending_decision: None,
            respond_via: "terminal",
            respond_within_secs: None,
            prompt: None,
            progress: None,
            preview: None,
            status_since: m.status_since(),
            hooks_unheard: None,
            task: None,
            project: None,
        }
    }

    #[test]
    fn idle_row_emits_no_status_changed_on_later_ticks() {
        // 守卫（agora-385）：IDLE 行之后的 tick 什么都没变（reason 固定、status_since 钉在进入 IDLE 那一刻）
        // → 求差器一条 status_changed 都不发；前端不必每 2 s 重渲染那一行。对照：输出恢复、状态真变了才发。
        // 关掉：machine.rs 里 IDLE 的 reason 改回 `no output for {n}s` → 第二轮 step 就有事件，断言红。
        use crate::runtime::{RuntimeRef, RuntimeSession, Size};
        use crate::status::{Assessment, Liveness, Machine, MachineConfig, Observation};
        let mut rt = RuntimeSession {
            r#ref: RuntimeRef("fake:agora:s1".into()),
            name: "s1".into(),
            pid: Some(1),
            alive: true,
            exit: None,
            exited_at: None,
            title: String::new(),
            cwd: std::path::PathBuf::from("/"),
            attached: false,
            size: Size { cols: 80, rows: 24 },
            managed: true,
            output_at: Some(0),
        };
        let mut m = Machine::new(MachineConfig::default(), false, 1, 0);
        let tick = |m: &mut Machine, rt: &RuntimeSession, now: i64| {
            m.observe(Observation {
                process: Assessment::new(Status::Running, Source::Process, 1.0, None),
                liveness: Liveness::Alive,
                text: None,
                runtime: Some(rt),
                epoch: 1,
                now,
            })
        };
        tick(&mut m, &rt, 0);
        assert_eq!(tick(&mut m, &rt, 60).status, Status::Idle);
        let mut differ = Differ::default();
        assert!(
            differ
                .step("n", &[shell_view(&m, ProcessState::Alive)])
                .is_empty(),
            "第一轮只建基线"
        );
        for now in [62, 64, 66] {
            tick(&mut m, &rt, now);
            let events = differ.step("n", &[shell_view(&m, ProcessState::Alive)]);
            assert!(
                events.is_empty(),
                "t={now}: IDLE 行没变，不该有事件: {events:?}"
            );
        }
        // 输出恢复 → RUNNING：这才是一条 status_changed，起点是恢复那一刻。
        rt.output_at = Some(70);
        tick(&mut m, &rt, 70);
        let events = differ.step("n", &[shell_view(&m, ProcessState::Alive)]);
        assert!(
            matches!(
                &events[..],
                [Event::StatusChanged {
                    status: Status::Running,
                    status_since: 70,
                    ..
                }]
            ),
            "{events:?}"
        );
    }

    #[test]
    fn project_change_emits_session_updated() {
        // agora-uvd.1：异步补齐的 project 从 None → Some 必须走 SessionUpdated，否则浏览器
        // 永远看不到。相同时不发。关掉 Differ::step 里 prev.project != s.project 那一截，
        // 这条就红（会变成 StatusChanged 或什么都不发）。
        use crate::project::ProjectInfo;
        use crate::status::{Machine, MachineConfig};
        let m = Machine::new(MachineConfig::default(), false, 1, 0);
        let none = shell_view(&m, ProcessState::Alive);
        let mut differ = Differ::default();
        assert!(
            differ.step("n", std::slice::from_ref(&none)).is_empty(),
            "第一轮只建基线"
        );

        let mut some = none.clone();
        some.project = Some(ProjectInfo {
            repo: "/repo".into(),
            name: "repo".into(),
            worktree: "/repo".into(),
            branch: Some("main".into()),
            main: true,
        });
        let events = differ.step("n", &[some.clone()]);
        assert!(
            matches!(&events[..], [Event::SessionUpdated { .. }]),
            "project 到了该整行重发: {events:?}"
        );
        let events = differ.step("n", &[some]);
        assert!(events.is_empty(), "project 没变不该再发: {events:?}");
    }

    #[test]
    fn a_process_tri_state_change_is_a_status_change_even_when_nothing_else_moves() {
        // agora-5gg.18（裁决 Q4）：`process` 进了求差器的 Seen。现场形状（盘点 B2）：同一行从
        // "探到进程号还在"换成"这个宿主没给可信进程号"（daemon 重启后检查点里没回填上 pid），
        // status / source / reason 一个字都没变，只有进程事实从 alive 变成 unknown——那是一条
        // 人要看一眼的变化，不能吐掉。从 Seen 里去掉 process → 第二轮什么都不发，第一条断言红。
        use crate::status::{Machine, MachineConfig};
        let m = Machine::new(MachineConfig::default(), false, 1, 0);
        let alive = shell_view(&m, ProcessState::Alive);
        let mut differ = Differ::default();
        assert!(
            differ.step("n", std::slice::from_ref(&alive)).is_empty(),
            "第一轮只建基线"
        );
        let unknown = shell_view(&m, ProcessState::Unknown);
        let events = differ.step("n", std::slice::from_ref(&unknown));
        match &events[..] {
            [Event::StatusChanged {
                process,
                alive: alive_flag,
                ..
            }] => {
                assert_eq!(*process, ProcessState::Unknown);
                assert!(!alive_flag, "旧布尔是 process == alive 的投影");
            }
            other => panic!("只换了 process，应发一条 status_changed: {other:?}"),
        }
        // 发到线上的形态（docs/spec/api.md 事件流）：两个字段都在，老页面读 alive、新页面读 process。
        let j = serde_json::to_value(&events[0]).unwrap();
        assert_eq!(j["process"], "unknown", "{j}");
        assert_eq!(j["alive"], false, "{j}");
        assert!(
            differ.step("n", &[unknown]).is_empty(),
            "同一眼不该再发第二条"
        );
    }
}
