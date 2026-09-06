//! 投递箱重放的时间语义（A42；agora-h1k.4；MISSION §3.4 / §6.3）：daemon 停机期间攒下的事件，
//! 重放后状态起点（`status_since`）是事件自己的时刻——文件名里的 ts——不是重放的当下。
//! 关掉 `Machine::apply_at` 里对 `at` 的使用（回到 `now`）这里就红。

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use agora::clock;
use agora::hook::{inbox::now_unix_ms, Delivery, Envelope, Inbox, Receiver};
use agora::runtime::Size;
use agora::session::{Db, NewSession, SessionManager};
use agora::status::{Source, Status};
use serde_json::json;

use common::FakeRuntime;

fn create(s: &SessionManager) -> String {
    s.create(&NewSession {
        display_name: "replayed".into(),
        agent_type: "claude".into(),
        working_directory: "/tmp".into(),
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

fn delivery(id: &str, ms: u64, payload: serde_json::Value) -> Delivery {
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: Some(id.into()),
            agora_epoch: Some(1),
            agent_session_id: "conversation".into(),
            agent_env: BTreeMap::new(),
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: ms,
        },
        payload,
    }
}

#[test]
fn status_since_uses_event_time_on_replay() {
    // M3 剧本第 4 步：停 daemon → 会话在停机期间进入 WAITING → 起 daemon 重放 → "waiting Nm"
    // 从事件时刻算，不是从 daemon 起来那一刻算成 0。
    let home = tempfile::tempdir().unwrap();
    let rt = Arc::new(FakeRuntime::default());
    let id = {
        let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
        let s = SessionManager::new(db, rt.clone());
        create(&s)
    };
    // daemon 停了。三分钟前 agent 收到 prompt，两分钟前它停下来要权限：hook 进程照常落盘。
    let now = clock::now_secs();
    let inbox = Inbox::new(home.path());
    inbox
        .write(&delivery(
            &id,
            (now - 180) as u64 * 1000,
            json!({"hook_event_name":"UserPromptSubmit", "prompt":"do x"}),
        ))
        .unwrap();
    inbox
        .write(&delivery(
            &id,
            (now - 120) as u64 * 1000,
            json!({"hook_event_name":"PermissionRequest", "tool_name":"Bash"}),
        ))
        .unwrap();

    // daemon 起来：reconcile + 重放。
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let s = Arc::new(SessionManager::new(db, rt.clone()));
    s.reconcile().unwrap();
    let r = Receiver::new(home.path(), s.clone());
    assert_eq!(r.replay().unwrap(), 2);
    let v = s.get(&id).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::Waiting, Source::Hook),
        "{v:?}"
    );
    let since = v.status_since;
    assert!(
        (since - (now - 120)).abs() <= 1,
        "waiting 从事件时刻（两分钟前）算: status_since={since} now={now}"
    );
    assert_eq!(v.prompt.as_deref(), Some("do x"));

    // 再重启一次：检查点里存的也是事件时刻，不会被恢复那一刻覆盖。
    drop(r);
    drop(s);
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let s = Arc::new(SessionManager::new(db, rt.clone()));
    s.reconcile().unwrap();
    let r = Receiver::new(home.path(), s.clone());
    assert_eq!(r.replay().unwrap(), 0);
    assert_eq!(s.get(&id).unwrap().status_since, since);

    // 实时到达的事件：文件名的 ts 就是此刻，起点 = 收到的时刻。
    let path = inbox
        .write(&delivery(
            &id,
            now_unix_ms(),
            json!({"hook_event_name":"Stop", "last_assistant_message":"ok"}),
        ))
        .unwrap();
    let before = clock::now_secs();
    r.ingest(&path).unwrap();
    let v = s.get(&id).unwrap();
    assert_eq!(v.assessment.status, Status::TurnDone);
    assert!(
        v.status_since >= before && v.status_since <= clock::now_secs(),
        "实时事件的起点就是现在: {} vs {before}",
        v.status_since
    );
}
