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
    assert_eq!(
        saved["version"], 3,
        "v2 = 带 agent_process（agora-tql）；v3 = seen_at 毫秒（agora-2nh）"
    );
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

#[test]
fn starting_decays_through_session_manager_and_after_checkpoint_restore() {
    // agora-okr：起好了、还没给第一条指令的 Claude 会话不能永远 STARTING。从 SessionManager 入口
    // 覆盖（agora-uez 的教训：只喂 Machine 会漏掉接线错误），再走 agora-9dj 的检查点恢复路径。
    // 检查点只在 hook 事件时写，衰减不写盘，恢复出来的仍是 STARTING。
    //
    // 宽限 3 s，不能再缩（agora-q8x，2026-09-06）：machine 用整秒时钟比较 now - since >= grace，
    // 之前设 1 s 时 ingest 落在第 N 秒末、紧接着的 get 落在第 N+1 秒初就已经满 1 s 而衰减，
    // "ingest 后立刻 get 仍是 STARTING"这条断言在三路并行编译的机器上假阴性（跨秒概率与两次
    // 调用之间的负载延迟成正比）。3 s 能容 ≥ 2 s 的抖动；衰减那头不再固定睡，按事实等到上限。
    const GRACE: Duration = Duration::from_secs(3);
    let grace = MachineConfig {
        startup_grace: GRACE,
        ..Default::default()
    };
    // 等到状态离开 STARTING（最多宽限 + 3 s），返回最后一次看到的视图；断言留给调用方原样做。
    let wait_decay = |s: &SessionManager, id: &str| {
        let deadline = std::time::Instant::now() + GRACE + common::isolate::PROC;
        loop {
            let v = s.get(id).unwrap();
            if v.assessment.status != Status::Starting || std::time::Instant::now() >= deadline {
                return v;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db.clone(), rt.clone()).with_status_config(grace.clone()));
    let id = create(&s, "claude");
    let r = Receiver::new(home.path(), s.clone());
    let inbox = Inbox::new(home.path());
    let started = |id: &str, ms: u64, source: &str| {
        delivery(
            id,
            1,
            ms,
            json!({"hook_event_name":"SessionStart", "source": source}),
        )
    };
    r.ingest(&inbox.write(&started(&id, 1, "startup")).unwrap())
        .unwrap();
    let v = s.get(&id).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::Starting, Source::Hook)
    );
    let v = wait_decay(&s, &id);
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::TurnDone, Source::Hook),
        "{v:?}"
    );
    assert!(v
        .assessment
        .reason
        .as_deref()
        .unwrap()
        .contains("awaiting first prompt"));
    s.apply_hook(&id, 1, &[AgoraEvent::PromptSubmitted("first".into())])
        .unwrap();
    assert_eq!(s.get(&id).unwrap().assessment.status, Status::Running);

    // 重启路径：检查点里是 STARTING，恢复后照样衰减。
    let id2 = create(&s, "claude");
    r.ingest(&inbox.write(&started(&id2, 2, "resume")).unwrap())
        .unwrap();
    drop(r);
    drop(s);
    drop(db);
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let restarted = Arc::new(SessionManager::new(db, rt).with_status_config(grace));
    restarted.reconcile().unwrap();
    Receiver::new(home.path(), restarted.clone())
        .replay()
        .unwrap();
    let v = wait_decay(&restarted, &id2);
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::TurnDone, Source::Hook),
        "{v:?}"
    );
    assert!(v
        .assessment
        .reason
        .as_deref()
        .unwrap()
        .contains("awaiting first prompt"));
}

/// 无句柄 external 行的投递件：没有 AGORA_*，身份是 (host, agent_session_id)，进程号在 CLAUDE_PID。
/// 时刻从"现在"起算（`ms` 只是序号）：进程号要与报来它的 hook 时刻对一下，进程不得晚于 hook。
fn external_delivery(
    agent_session: &str,
    pid: u32,
    ms: u64,
    payload: serde_json::Value,
) -> Delivery {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let ms = now_ms + ms;
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: None,
            agora_epoch: None,
            agent_session_id: agent_session.into(),
            agent_env: BTreeMap::from([("CLAUDE_PID".to_owned(), pid.to_string())]),
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: ms,
        },
        payload,
    }
}

fn checkpoint_path(home: &std::path::Path, id: &str) -> std::path::PathBuf {
    let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    home.join("hooks/state").join(format!("{key}.json"))
}

#[test]
fn external_agent_pid_survives_daemon_restart_and_guards_pid_reuse() {
    // agora-tql（2026-09-08 现场）：external 行的存活靠 hook 报来的进程号，原来只在内存里，daemon 一
    // 重启就丢，agent 早退了的行永远钉在 TURN_DONE。守卫：进程号随检查点落盘，重启后 alive 仍可判、
    // 进程一没就 FINISHED；检查点里的启动时刻对不上（号被复用）也算没了。
    // 关掉 HookSnapshot.agent_process 的恢复 → 第一段 alive 断言红；关掉 agent_process_alive 的启动
    // 时刻比对 → 最后一段 finished 断言红。
    let home = tempfile::tempdir().unwrap();
    let rt = Arc::new(common::FakeRuntime::default());
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let id = {
        let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
        let s = Arc::new(SessionManager::new(db, rt.clone()));
        let r = Receiver::new(home.path(), s.clone());
        let inbox = Inbox::new(home.path());
        let start = json!({"hook_event_name":"SessionStart","session_id":"ext-1","cwd":"/work/agora","source":"startup"});
        let path = inbox
            .write(&external_delivery("ext-1", child.id(), 1, start))
            .unwrap();
        let id = r.ingest(&path).unwrap().unwrap().session_key;
        let stop =
            json!({"hook_event_name":"Stop","session_id":"ext-1","last_assistant_message":"done"});
        let path = inbox
            .write(&external_delivery("ext-1", child.id(), 2, stop))
            .unwrap();
        r.ingest(&path).unwrap();
        let v = s.get(&id).unwrap();
        assert_eq!((v.assessment.status, v.alive), (Status::TurnDone, true));
        inbox.prune_done(Duration::ZERO);
        id
    };
    // 重启：归档已清，只有检查点。进程号从检查点回填 → alive 照旧可判。
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let s = Arc::new(SessionManager::new(db, rt.clone()));
    Receiver::new(home.path(), s.clone()).replay().unwrap();
    let v = s.get(&id).unwrap();
    assert_eq!(v.assessment.status, Status::TurnDone, "{:?}", v.assessment);
    assert!(v.alive, "重启后进程号从检查点回填，alive 仍可判");
    child.kill().unwrap();
    child.wait().unwrap();
    let v = s.get(&id).unwrap();
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    assert_eq!(v.assessment.source, Source::Process);
    assert!(
        v.assessment
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("process gone"),
        "{:?}",
        v.assessment
    );
    drop(s);

    // 号被复用：检查点里的启动时刻改成别的值，进程虽活着也不是当初那个 → FINISHED。
    let mut other = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let id2 = {
        let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
        let s = Arc::new(SessionManager::new(db, rt.clone()));
        let r = Receiver::new(home.path(), s.clone());
        let inbox = Inbox::new(home.path());
        let start = json!({"hook_event_name":"SessionStart","session_id":"ext-2","cwd":"/work/agora","source":"startup"});
        let path = inbox
            .write(&external_delivery("ext-2", other.id(), 3, start))
            .unwrap();
        let id2 = r.ingest(&path).unwrap().unwrap().session_key;
        assert!(s.get(&id2).unwrap().alive);
        inbox.prune_done(Duration::ZERO);
        id2
    };
    let path = checkpoint_path(home.path(), &id2);
    let mut cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(cp["version"], 3, "{cp}");
    assert_eq!(cp["agent_process"]["pid"], other.id(), "{cp}");
    let started = cp["agent_process"]["started_at"]
        .as_i64()
        .expect("本机能读到进程启动时刻");
    cp["agent_process"]["started_at"] = json!(started - 1000);
    std::fs::write(&path, cp.to_string()).unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let s = Arc::new(SessionManager::new(db, rt));
    Receiver::new(home.path(), s.clone()).replay().unwrap();
    let v = s.get(&id2).unwrap();
    assert!(!v.alive, "启动时刻对不上 = 号被复用，不算活着");
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    drop(s);

    // 从归档重建时读到的启动时刻可能是复用者的：hook 时刻早于号上进程的启动时刻 → 也不算活着。
    // 模拟：检查点里 seen_at（v3 起是毫秒）改成进程启动之前。
    let mut cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cp["agent_process"]["started_at"] = json!(started);
    cp["agent_process"]["seen_at"] = json!((started - 60) * 1000);
    std::fs::write(&path, cp.to_string()).unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let s = Arc::new(SessionManager::new(
        db,
        Arc::new(common::FakeRuntime::default()),
    ));
    Receiver::new(home.path(), s.clone()).replay().unwrap();
    let v = s.get(&id2).unwrap();
    assert!(!v.alive, "进程比报来它的 hook 还新 = 号被复用");
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    other.kill().unwrap();
    other.wait().unwrap();
}

#[test]
fn v2_checkpoint_seen_at_in_seconds_is_upgraded_to_millis_on_restore() {
    // agora-2nh：v3 起 `agent_process.seen_at` 是毫秒（同一进程两个对话谁新谁旧靠它分）。升级前写下
    // 的 v2 检查点里是秒；不换算就被当成 1970 年的毫秒，与启动时刻一比就成了"进程比 hook 还新 = 号被
    // 复用"，daemon 升级后一重启，现存的 external 行全判 FINISHED。守卫：检查点降成 v2（seen_at 改回
    // 秒），重启后行仍 alive；下一条 hook 写出的检查点是 v3、seen_at 是换算后的毫秒。
    // 关掉 Machine::restore_hook 里的 ×1000 → alive 断言红。
    let home = tempfile::tempdir().unwrap();
    let rt = Arc::new(common::FakeRuntime::default());
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let id = {
        let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
        let s = Arc::new(SessionManager::new(db, rt.clone()));
        let r = Receiver::new(home.path(), s.clone());
        let inbox = Inbox::new(home.path());
        let start = json!({"hook_event_name":"SessionStart","session_id":"ext-v2","cwd":"/work/agora","source":"startup"});
        let path = inbox
            .write(&external_delivery("ext-v2", child.id(), 1, start))
            .unwrap();
        let id = r.ingest(&path).unwrap().unwrap().session_key;
        assert!(s.get(&id).unwrap().alive);
        inbox.prune_done(Duration::ZERO);
        id
    };
    let path = checkpoint_path(home.path(), &id);
    let mut cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(cp["version"], 3, "{cp}");
    let seen_ms = cp["agent_process"]["seen_at"].as_i64().unwrap();
    assert!(
        seen_ms > 1_000_000_000_000,
        "v3 的 seen_at 是毫秒：{seen_ms}"
    );
    cp["version"] = json!(2);
    cp["agent_process"]["seen_at"] = json!(seen_ms / 1000);
    std::fs::write(&path, cp.to_string()).unwrap();

    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let s = Arc::new(SessionManager::new(db, rt));
    let r = Receiver::new(home.path(), s.clone());
    r.replay().unwrap();
    let v = s.get(&id).unwrap();
    assert!(
        v.alive,
        "v2 检查点的秒级 seen_at 换算后进程仍算活着：{:?}",
        v.assessment
    );
    assert_ne!(v.assessment.status, Status::Finished, "{:?}", v.assessment);

    // 下一条 hook 把检查点写成 v3，seen_at 已是毫秒（秒 ×1000，毫秒尾数丢了是预期）。
    let stop =
        json!({"hook_event_name":"Stop","session_id":"ext-v2","last_assistant_message":"done"});
    let path_stop = Inbox::new(home.path())
        .write(&external_delivery("ext-v2", child.id(), 2, stop))
        .unwrap();
    r.ingest(&path_stop).unwrap();
    let cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(cp["version"], 3, "{cp}");
    assert_eq!(
        cp["agent_process"]["seen_at"],
        seen_ms / 1000 * 1000,
        "{cp}"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}
