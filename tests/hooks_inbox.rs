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
            env: vec![("AGORA_HOME".into(), node.home.display().to_string())],
            size: Size::default(),
        })
        .unwrap();
    let id = view.record.id.clone();
    let inbox = Inbox::new(&node.home);
    let deadline = Instant::now() + Duration::from_secs(15);
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
    // 三个文件（含被丢的）都进了 done/。
    let done: Vec<_> = walkdir(&inbox.done_dir());
    assert_eq!(done.len(), 3, "{done:?}");
    // 再重放一次什么都没有：不会重复应用。
    assert_eq!(receiver.replay().unwrap(), 0);
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
