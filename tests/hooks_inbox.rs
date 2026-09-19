//! 投递箱（ADR-002 D3；MISSION §3.4；A36 不变量 10 的投递部分）：daemon 不在时事件不丢，
//! 重启后按序重放；旧 epoch 丢弃；权限过宽拒绝读。fake-agent 走真实的 `agora hook` 路径。

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::hook::{Delivery, Envelope, HookError, Inbox, Receiver};
use agora::runtime::Size;
use agora::session::{Db, NewSession, SessionManager};
use agora::status::{Source, Status};

use common::node::{TmuxNode, AGORA_BIN};
use common::FakeRuntime;

fn delivery(session: &str, epoch: i64, event: &str, ms: u64) -> Delivery {
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: Some(session.into()),
            agora_epoch: Some(epoch),
            agent_session_id: "agent-1".into(),
            agent_env: BTreeMap::new(),
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: ms,
        },
        payload: serde_json::json!({ "hook_event_name": event, "session_id": "agent-1" }),
    }
}

/// daemon 不在（节点没起 socket）→ fake-agent 里的真实 `agora hook` 落盘、退出 0；
/// "重启"后的 Receiver 按文件名顺序重放，旧 epoch 的那条被丢。
#[tokio::test(flavor = "multi_thread")]
async fn events_survive_daemon_restart() {
    let node = TmuxNode::new();
    let script = node.home.join("agent.txt");
    std::fs::write(
        &script,
        [
            r#"hook claude {"hook_event_name":"SessionStart","session_id":"agent-1","source":"startup"}"#,
            "print started",
            r#"hook claude {"hook_event_name":"Stop","session_id":"agent-1","last_assistant_message":"ok"}"#,
            "print stopped",
            "sleep 60000",
        ]
        .join("\n"),
    )
    .unwrap();
    let view = node
        .sessions
        .create(&NewSession {
            display_name: "hooked".into(),
            agent_type: "fake".into(),
            working_directory: std::env::temp_dir(),
            worktree: None,
            task_ref: None,
            command: format!("{AGORA_BIN} fake-agent {}", script.display()),
            env: vec![
                ("AGORA_HOME".into(), node.home.display().to_string()),
                // 守卫（agora-7ad）：pane 环境里带上 grok CLI 会 export 的那个变量，把"执行者是
                // grok"的现场固定下来。fake-agent 不按宿主摆正环境的话，下面这两条 `hook claude`
                // 会被 hook 进程当成"不是我的事"静默丢掉，这个循环等满 PROC 才红。
                ("GROK_SESSION_ID".into(), "grok-in-pane".into()),
            ],
            size: Size::default(),
        })
        .unwrap();
    let id = view.record.id.clone();
    let inbox = Inbox::new(&node.home);
    // 等的是 tmux 里那个 fake-agent 起两次 `agora hook` 把文件落盘：起子进程的耗时随机器负载
    // 走，15 s 在三个 worktree 并行的批次里不够（agora-8tv / agora-2ok / agora-ohw）。
    let deadline = Instant::now() + common::isolate::PROC;
    loop {
        if inbox.pending().unwrap().len() == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hook 文件没落下: {}",
            node.tail(&view)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // hook 没有 daemon 也不拖累 agent：两条都写完、agent 继续跑。
    node.wait(&id, |v| v.alive);
    assert!(node.tail(&view).contains("stopped"), "{}", node.tail(&view));

    // Restart 之前那代进程的事件（epoch 0 < 当前 1）在重放时丢弃；时间戳放在最前，
    // 证明是按 epoch 丢的而不是按顺序。
    inbox.write(&delivery(&id, 0, "Stop", 1)).unwrap();
    assert_eq!(inbox.pending().unwrap().len(), 3);

    // daemon 重启：新的 Receiver 重放。
    let receiver = Receiver::new(&node.home, node.sessions.clone());
    assert_eq!(receiver.replay().unwrap(), 2);
    let got: Vec<_> = receiver
        .received_for(&id)
        .into_iter()
        .map(|r| (r.event.unwrap(), r.delivery.envelope.agora_epoch))
        .collect();
    assert_eq!(
        got,
        vec![
            ("SessionStart".to_string(), Some(1)),
            ("Stop".to_string(), Some(1))
        ]
    );
    let r = &receiver.received_for(&id)[0];
    assert_eq!(r.delivery.envelope.agent_session_id, "agent-1");
    assert!(
        r.delivery.envelope.runtime_env.contains_key("TMUX_PANE"),
        "pane 里跑的 hook 该带 TMUX_PANE: {:?}",
        r.delivery.envelope.runtime_env
    );
    assert!(inbox.pending().unwrap().is_empty());
    // A36 不变量 10：重放后的状态来自 hook 层——Stop → TURN_DONE，带来源与置信度。
    let v = node.sessions.get(&id).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::TurnDone, Source::Hook),
        "{:?}",
        v.assessment
    );
    assert!(v.assessment.confidence >= 0.95);
    assert_eq!(node.sessions.detail(&id).as_deref(), Some("ok"));
    // SessionStart 自报的对话 id 落进 agent_session_id（每次命中覆盖）。
    assert_eq!(v.record.agent_session_id.as_deref(), Some("agent-1"));
    // 三个文件（含被丢的）都进了 done/。
    let done: Vec<_> = walkdir(&inbox.done_dir());
    assert_eq!(done.len(), 3, "{done:?}");
    // 再重放一次什么都没有：不会重复应用。
    assert_eq!(receiver.replay().unwrap(), 0);

    // Restart → epoch 2（新一代 fake-agent 自己也会发 epoch 2 的事件）：上一代（epoch 1）
    // 迟到的 Stop 在重放时丢弃，账本里 epoch 1 的条目还是原来那两条。
    node.sessions.restart(&id, &[]).unwrap();
    inbox.write(&delivery(&id, 1, "Stop", 999)).unwrap();
    receiver.replay().unwrap();
    let old: Vec<_> = receiver
        .received_for(&id)
        .into_iter()
        .filter(|r| r.delivery.envelope.agora_epoch == Some(1))
        .collect();
    assert_eq!(old.len(), 2, "{old:?}");
    let v = node.wait(&id, |v| v.alive);
    assert_eq!(v.record.epoch, 2);
}

fn walkdir(d: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                v.extend(walkdir(&p));
            } else {
                v.push(p);
            }
        }
    }
    v
}

#[test]
fn rejects_wrong_permissions() {
    // ADR-002 "投递箱被其他用户写入或读取"：hooks/ 下任何一级过宽 → 拒绝读，文件原地不动。
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(dir.path());
    inbox.write(&delivery("s", 1, "Stop", 5)).unwrap();
    let sessions = Arc::new(SessionManager::new(
        Arc::new(Db::open_in_memory().unwrap()),
        Arc::new(FakeRuntime::default()),
    ));
    let receiver = Receiver::new(dir.path(), sessions);
    let session_dir = dir.path().join("hooks/inbox/claude/agent-1");
    std::fs::set_permissions(&session_dir, std::fs::Permissions::from_mode(0o750)).unwrap();
    let err = receiver.replay().unwrap_err();
    assert!(matches!(err, HookError::TooOpen { .. }), "{err}");
    assert!(err.to_string().contains("chmod"), "{err}");
    assert_eq!(inbox.pending().unwrap().len(), 1);
    assert!(receiver.received().is_empty());
    std::fs::set_permissions(&session_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(receiver.replay().unwrap(), 1);
}

#[test]
fn replay_orders_by_time_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(dir.path());
    inbox.write(&delivery("b", 1, "Stop", 20)).unwrap();
    inbox.write(&delivery("a", 1, "SessionStart", 10)).unwrap();
    inbox.write(&delivery("a", 1, "Stop", 30)).unwrap();
    let sessions = Arc::new(SessionManager::new(
        Arc::new(Db::open_in_memory().unwrap()),
        Arc::new(FakeRuntime::default()),
    ));
    let receiver = Receiver::new(dir.path(), sessions);
    assert_eq!(receiver.replay().unwrap(), 3);
    let order: Vec<_> = receiver
        .received()
        .into_iter()
        .map(|r| r.delivery.envelope.received_unix_ms)
        .collect();
    assert_eq!(order, vec![10, 20, 30]);
}

/// 一行真实存在的会话（FakeRuntime 起的，epoch 1），投递件带它的身份。
fn session_row(sessions: &Arc<SessionManager>) -> String {
    sessions
        .create(&NewSession {
            display_name: "swept".into(),
            agent_type: "claude".into(),
            working_directory: std::env::temp_dir(),
            worktree: None,
            task_ref: None,
            command: "claude".into(),
            env: vec![],
            size: Size::default(),
        })
        .unwrap()
        .record
        .id
}

/// `delivery` 同一条信封，payload 多带几个字段：PermissionRequest 要有 `tool_name` 才留得下摘要。
fn delivery_with(
    session: &str,
    epoch: i64,
    event: &str,
    ms: u64,
    extra: serde_json::Value,
) -> Delivery {
    let mut d = delivery(session, epoch, event, ms);
    if let (Some(base), Some(more)) = (d.payload.as_object_mut(), extra.as_object()) {
        base.extend(more.clone());
    }
    d
}

/// MISSION §5.1（A36 不变量 10 的另一半）：daemon 在、可它正忙着——启动那次 `replay()` 只扫一份
/// 快照，而 socket 在那之前的 reconcile / 重跑完之前就 bind 了，窗口里到达、socket 又没接住的投递件
/// 不能一直躺到下次重启（2026-09-18 Mac：replay 跑了 172 s，6 个 Pre/PostToolUse 躺到人来查）。
/// `Receiver::replay_stale_pending` 挂在 serve 之后与每 5 s 的 sweep 上兜底（agora-5gg.1）。
/// 守卫：删掉 `sweep()` 开头那一行调用，下面"一个周期之后 pending 空"红。
#[test]
fn events_landing_during_replay_are_consumed_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(dir.path());
    let sessions = Arc::new(SessionManager::new(
        Arc::new(Db::open_in_memory().unwrap()),
        Arc::new(FakeRuntime::default()),
    ));
    let id = session_row(&sessions);
    // 宽限期由下一条单独守；这条要验的是"一个 sweep 周期之内"，所以拿掉它（生产默认 30 s，
    // 见 `PENDING_REPLAY_AGE`）。
    let receiver =
        Receiver::new(dir.path(), sessions.clone()).with_pending_sweep_age(Duration::ZERO);

    // 启动重放：此刻投递箱里只有开局那两条，快照扫完 `replay` 就返回了。
    inbox.write(&delivery(&id, 1, "SessionStart", 10)).unwrap();
    inbox
        .write(&delivery_with(
            &id,
            1,
            "UserPromptSubmit",
            11,
            serde_json::json!({"prompt":"push it"}),
        ))
        .unwrap();
    assert_eq!(receiver.replay().unwrap(), 2);
    assert!(inbox.pending().unwrap().is_empty());
    assert_ne!(
        sessions.get(&id).unwrap().assessment.status,
        Status::Waiting,
        "补投之前这行还没在等人"
    );

    // 重放那份快照之后才到达的一条，而且没有 socket 唤醒它（现场：backlog 没排上队 / connect 被拒）。
    inbox
        .write(&delivery_with(
            &id,
            1,
            "PermissionRequest",
            20,
            serde_json::json!({"tool_name":"Bash","tool_input":{"command":"ls"}}),
        ))
        .unwrap();

    // 没有第二次重启、没有 socket 唤醒：一轮 sweep 就该把它消费掉。
    receiver.sweep();
    assert!(
        inbox.pending().unwrap().is_empty(),
        "兜底重放没扫投递箱：sweep 一个周期之后那条投递件还躺在 pending 里（agora-5gg.1）"
    );
    assert_eq!(
        walkdir(&inbox.done_dir()).len(),
        3,
        "补投的投递件要照常进 done/ 留排障归档"
    );
    let v = sessions.get(&id).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::Waiting, Source::Hook),
        "{:?}",
        v.assessment
    );
    assert_eq!(v.detail.as_deref(), Some("Bash: ls"));
    // 补投走 `ingest`，不登记挂起（登记在 socket 那条路的 `begin_wake`）：没有活连接可答。
    assert!(
        receiver.pending(&id).is_empty(),
        "兜底重放不该替一条已经断掉的连接登记挂起"
    );
    assert_eq!(receiver.received_for(&id).len(), 3);

    // 幂等：done/ 里的不会被应用第二次，ledger 也不长。
    receiver.sweep();
    assert_eq!(receiver.received_for(&id).len(), 3);

    // daemon 起来之后补投那一次（`main.rs`，serve 之后）走的是同一个入口：bin 的启动时序在集成
    // 测试里没法稳定插进去（要它落在 bind 与 accept 之间），至少把它的接缝钉在这里。
    inbox.write(&delivery(&id, 1, "Stop", 30)).unwrap();
    assert_eq!(
        receiver.replay_stale_pending(),
        1,
        "serve 之后补投的那个入口没消费投递件"
    );
    assert_eq!(
        sessions.get(&id).unwrap().assessment.status,
        Status::TurnDone,
        "补投的 Stop 该把行推到 TURN_DONE"
    );
}

/// 宽限期是条真边界，不是装饰：刚落盘的投递件多半正有人在 socket 上等答复，兜底重放抢下来会让那条
/// 连接拿到"路径不在投递箱里"的错误、也让这次权限请求进不了挂起表（`ingest` 不登记挂起）。
/// 守卫：把 `pending_file_is_due` 改成恒真 → 第一段红；改成恒假 → 第二段红。
#[test]
fn freshly_written_deliveries_wait_out_the_grace_period() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(dir.path());
    let sessions = Arc::new(SessionManager::new(
        Arc::new(Db::open_in_memory().unwrap()),
        Arc::new(FakeRuntime::default()),
    ));
    let id = session_row(&sessions);
    let grace = Duration::from_millis(250);
    let receiver = Receiver::new(dir.path(), sessions.clone()).with_pending_sweep_age(grace);
    inbox.write(&delivery(&id, 1, "SessionStart", 10)).unwrap();
    assert_eq!(receiver.replay().unwrap(), 1);

    inbox.write(&delivery(&id, 1, "Stop", 20)).unwrap();
    receiver.sweep();
    assert_eq!(
        inbox.pending().unwrap().len(),
        1,
        "还在宽限期内的投递件被兜底重放抢走了：socket 上正等答复的那条连接会拿不到自己的文件"
    );

    std::thread::sleep(grace * 2);
    receiver.sweep();
    assert!(
        inbox.pending().unwrap().is_empty(),
        "过了宽限期还没被兜底重放消费：滞留的投递件又得等下次重启了"
    );
    assert_eq!(
        sessions.get(&id).unwrap().assessment.status,
        Status::TurnDone,
        "补投走的是同一条 ingest，状态机看到的还是 hook 层的事实"
    );
}

/// 兜底重放那道权限门连 `hooks/` 根一起查：`hooks/` 本身过宽时，启动那次 `replay()` 拒读，
/// 5 s 一轮的兜底也不能把它静默消费掉——一道启动拦得住、运行中放过去的门等于没有，而
/// `hooks/` 过宽正是别人能自己建出整棵投递箱往里塞伪造事件的那一级（ADR-002「什么会让它变危险」）。
/// 这一条是 2026-09-19 在真 daemon 上手工代检实测到的：`chmod 775 hooks/` 之后，那份够老的投递件
/// 照旧在一个周期里进了 done/。守卫：把 `check_inbox_permissions` 改回只走 `inbox/` 那一支 → 第一段红。
#[test]
fn stale_replay_checks_the_hooks_root_too() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(dir.path());
    let sessions = Arc::new(SessionManager::new(
        Arc::new(Db::open_in_memory().unwrap()),
        Arc::new(FakeRuntime::default()),
    ));
    let id = session_row(&sessions);
    let receiver =
        Receiver::new(dir.path(), sessions.clone()).with_pending_sweep_age(Duration::ZERO);
    inbox.write(&delivery(&id, 1, "SessionStart", 10)).unwrap();
    assert_eq!(receiver.replay().unwrap(), 1);

    std::fs::set_permissions(
        dir.path().join("hooks"),
        std::fs::Permissions::from_mode(0o775),
    )
    .unwrap();
    inbox.write(&delivery(&id, 1, "Stop", 20)).unwrap();
    // 前提：启动那一支确实拒读这份投递箱（前提没了这段就守不到东西）。
    let err = receiver.replay().unwrap_err();
    assert!(matches!(err, HookError::TooOpen { .. }), "{err}");
    // 兜底那一支不能绕过它。
    assert_eq!(
        receiver.replay_stale_pending(),
        0,
        "hooks/ 根权限过宽时兜底重放还是把投递件消费了（agora-5gg.1）"
    );
    assert_eq!(
        inbox.pending().unwrap().len(),
        1,
        "被拒读的投递件该原地等人 chmod，不该进 done/"
    );
    assert_eq!(receiver.received_for(&id).len(), 1);
    assert_ne!(
        sessions.get(&id).unwrap().assessment.status,
        Status::TurnDone
    );

    // 修回去：同一个入口下一轮就补投，不需要重启。
    std::fs::set_permissions(
        dir.path().join("hooks"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert_eq!(
        receiver.replay_stale_pending(),
        1,
        "权限修回去后兜底重放该把它补投，否则滞留的投递件又得等下次重启"
    );
    assert!(inbox.pending().unwrap().is_empty());
    assert_eq!(
        sessions.get(&id).unwrap().assessment.status,
        Status::TurnDone
    );
}
