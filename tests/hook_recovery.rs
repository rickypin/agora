//! M1 review 守卫：已消费的 hook 仍可恢复；检查点从不替代进程事实。
mod common;
use agora::{
    hook::{Delivery, Envelope, Inbox, Receiver},
    runtime::{Exit, Size},
    session::{Db, NewSession, SessionManager},
    status::{AgoraEvent, MachineConfig, Source, Status},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn create(s: &SessionManager, agent: &str) -> String {
    s.create(&NewSession {
        display_name: "review".into(),
        agent_type: agent.into(),
        working_directory: "/tmp".into(),
        worktree: None,
        task_ref: None,
        command: agent.into(),
        env: vec![],
        size: Size::default(),
    })
    .unwrap()
    .record
    .id
}
fn delivery(id: &str, epoch: i64, ms: u64, payload: serde_json::Value) -> Delivery {
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: Some(id.into()),
            agora_epoch: Some(epoch),
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
fn consumed_hooks_survive_two_restarts_and_done_pruning() {
    for (payload, status, detail) in [
        (
            json!({"hook_event_name":"Stop", "last_assistant_message":"finished the change"}),
            Status::TurnDone,
            "finished the change",
        ),
        (
            json!({"hook_event_name":"PermissionRequest", "tool_name":"Write"}),
            Status::Waiting,
            "Write",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
        let rt = Arc::new(common::FakeRuntime::default());
        let s = Arc::new(SessionManager::new(db.clone(), rt.clone()));
        let id = create(&s, "claude");
        let r = Receiver::new(home.path(), s.clone());
        let inbox = Inbox::new(home.path());
        let path = inbox
            .write(&delivery(
                &id,
                1,
                1,
                json!({"hook_event_name":"UserPromptSubmit", "prompt":"implement feature"}),
            ))
            .unwrap();
        r.ingest(&path).unwrap();
        let path = inbox.write(&delivery(&id, 1, 2, payload)).unwrap();
        r.ingest(&path).unwrap();
        let since = s.get(&id).unwrap().status_since;
        drop(r);
        drop(s);
        drop(db);
        assert!(inbox.pending().unwrap().is_empty());
        inbox.prune_done(Duration::ZERO);
        for _ in 0..2 {
            let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
            let s = Arc::new(SessionManager::new(db, rt.clone()));
            s.reconcile().unwrap();
            let r = Receiver::new(home.path(), s.clone());
            assert_eq!(r.replay().unwrap(), 0);
            let v = s.get(&id).unwrap();
            assert_eq!(
                (v.assessment.status, v.assessment.source),
                (status, Source::Hook)
            );
            assert_eq!(v.detail.as_deref(), Some(detail));
            assert_eq!(v.prompt.as_deref(), Some("implement feature"));
            assert_eq!(v.status_since, since);
            assert!(
                v.pending_decision.is_none(),
                "恢复状态不恢复已经断掉的挂起连接"
            );
            assert!(r
                .respond(&id, None, agora::adapter::Decision::Allow)
                .is_err());
        }
    }
}

#[test]
fn recovered_hook_cannot_override_exit_or_a_new_epoch() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db.clone(), rt.clone()));
    let id = create(&s, "claude");
    let r = Receiver::new(home.path(), s.clone());
    let inbox = Inbox::new(home.path());
    r.ingest(
        &inbox
            .write(&delivery(
                &id,
                1,
                1,
                json!({"hook_event_name":"Stop", "last_assistant_message":"old"}),
            ))
            .unwrap(),
    )
    .unwrap();
    let reference = s.record(&id).unwrap().runtime_ref.unwrap();
    rt.set_dead(&reference, Exit::Code(7));
    let recovered = Arc::new(SessionManager::new(db.clone(), rt.clone()));
    Receiver::new(home.path(), recovered.clone())
        .replay()
        .unwrap();
    assert_eq!(
        recovered.get(&id).unwrap().assessment.status,
        Status::Failed
    );
    recovered.restart(&id, &[]).unwrap();
    let restarted = Arc::new(SessionManager::new(db, rt));
    let receiver = Receiver::new(home.path(), restarted.clone());
    inbox
        .write(&delivery(&id, 1, 2, json!({"hook_event_name":"Stop"})))
        .unwrap();
    receiver.replay().unwrap();
    let v = restarted.get(&id).unwrap();
    assert_eq!(v.record.epoch, 2);
    assert_ne!(v.assessment.status, Status::TurnDone);
    assert_ne!(v.detail.as_deref(), Some("old"));
    // 新事件仍能正常覆盖。
    receiver
        .ingest(
            &inbox
                .write(&delivery(
                    &id,
                    2,
                    3,
                    json!({"hook_event_name":"Stop", "last_assistant_message":"new"}),
                ))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(restarted.get(&id).unwrap().detail.as_deref(), Some("new"));
}

#[test]
fn failed_checkpoint_keeps_delivery_pending() {
    let home = tempfile::tempdir().unwrap();
    let s = Arc::new(SessionManager::new(
        Arc::new(Db::open_in_memory().unwrap()),
        Arc::new(common::FakeRuntime::default()),
    ));
    let id = create(&s, "claude");
    let r = Receiver::new(home.path(), s.clone());
    let inbox = Inbox::new(home.path());
    let path = inbox
        .write(&delivery(&id, 1, 1, json!({"hook_event_name":"Stop"})))
        .unwrap();
    // 用同名文件模拟检查点目录无法创建。
    std::fs::write(home.path().join("hooks/state"), "blocked").unwrap();
    assert!(r.ingest(&path).is_err());
    assert!(path.exists(), "未持久化不能移到 done");
    std::fs::rename(
        home.path().join("hooks/state"),
        home.path().join("blocked-state"),
    )
    .unwrap();
    assert!(
        r.ingest(&path).unwrap().is_some(),
        "写盘恢复后必须能重试，不能被内存水位去重"
    );
    assert_eq!(s.get(&id).unwrap().assessment.status, Status::TurnDone);
}

#[test]
fn silent_hook_fallback_runs_through_session_manager_and_recovers() {
    for agent in ["claude", "codex", "grok"] {
        let rt = Arc::new(common::FakeRuntime::default());
        let s = SessionManager::new(Arc::new(Db::open_in_memory().unwrap()), rt.clone())
            .with_status_config(MachineConfig {
                silence_after: Duration::from_secs(1),
                ..Default::default()
            });
        let id = create(&s, agent);
        let reference = s.record(&id).unwrap().runtime_ref.unwrap();
        rt.tails
            .lock()
            .unwrap()
            .insert(reference, "Do you want to proceed?\n1. Yes\n2. No".into());
        s.apply_hook(&id, 1, &[AgoraEvent::Activity("working".into())])
            .unwrap();
        assert_eq!(s.get(&id).unwrap().assessment.status, Status::Running);
        std::thread::sleep(Duration::from_millis(1100));
        let v = s.get(&id).unwrap();
        assert_eq!(v.assessment.status, Status::Unknown, "{agent}");
        assert!(v.assessment.reason.unwrap().contains("hooks silent"));
        assert!(v.preview.is_none(), "hook 预览不被屏幕代替");
        s.apply_hook(&id, 1, &[AgoraEvent::Activity("resumed".into())])
            .unwrap();
        assert_eq!(s.get(&id).unwrap().assessment.status, Status::Running);
    }
}

#[test]
fn upgrade_bootstraps_archive_and_checkpoint_watermark_rejects_old_inbox() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db.clone(), rt.clone()));
    let id = create(&s, "claude");
    let inbox = Inbox::new(home.path());
    // 模拟旧版 daemon 已消费的文件，尚无检查点。
    let prompt = delivery(
        &id,
        1,
        1,
        json!({"hook_event_name":"UserPromptSubmit", "prompt":"task"}),
    );
    let stop = delivery(
        &id,
        1,
        3,
        json!({"hook_event_name":"Stop", "last_assistant_message":"done"}),
    );
    for event in [&prompt, &stop] {
        inbox.done(&inbox.write(event).unwrap()).unwrap();
    }
    let receiver = Receiver::new(home.path(), s.clone());
    receiver.replay().unwrap();
    assert_eq!(s.get(&id).unwrap().assessment.status, Status::TurnDone);
    assert_eq!(s.get(&id).unwrap().prompt.as_deref(), Some("task"));
    // 崩溃在写检查点与移 done 之间，或旧投递迟到：不能覆盖更新的 Stop。
    inbox
        .write(&delivery(
            &id,
            1,
            2,
            json!({"hook_event_name":"PermissionRequest", "tool_name":"Write"}),
        ))
        .unwrap();
    inbox.write(&stop).unwrap();
    let restarted = Arc::new(SessionManager::new(db, rt));
    let receiver = Receiver::new(home.path(), restarted.clone());
    assert_eq!(receiver.replay().unwrap(), 0);
    assert_eq!(
        restarted.get(&id).unwrap().assessment.status,
        Status::TurnDone
    );
    assert!(inbox.pending().unwrap().is_empty());
    assert!(receiver.pending(&id).is_empty());
}

#[test]
fn corrupt_checkpoint_does_not_block_other_sessions_or_new_inbox() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db.clone(), rt.clone()));
    let bad = create(&s, "claude");
    let good = create(&s, "claude");
    let receiver = Receiver::new(home.path(), s);
    let inbox = Inbox::new(home.path());
    for id in [&bad, &good] {
        receiver
            .ingest(
                &inbox
                    .write(&delivery(
                        id,
                        1,
                        1,
                        json!({"hook_event_name":"Stop", "last_assistant_message":"ok"}),
                    ))
                    .unwrap(),
            )
            .unwrap();
    }
    let dir = home.path().join("hooks/state");
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let key: String = bad.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let file = dir.join(format!("{key}.json"));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved["version"], 1);
    assert!(saved.get("alive").is_none() && saved.get("exit").is_none());
    std::fs::write(&file, "broken").unwrap();
    inbox
        .write(&delivery(
            &bad,
            1,
            2,
            json!({"hook_event_name":"UserPromptSubmit", "prompt":"recover"}),
        ))
        .unwrap();
    let s = Arc::new(SessionManager::new(db, rt));
    let receiver = Receiver::new(home.path(), s.clone());
    assert_eq!(receiver.replay().unwrap(), 1);
    assert_eq!(s.get(&good).unwrap().assessment.status, Status::TurnDone);
    assert_eq!(s.get(&bad).unwrap().assessment.status, Status::Running);
}
