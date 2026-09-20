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
        // silence_after 从 1 s 提到 3 s（sleep 跟着走）：apply_hook 与紧接着的 get 之间只要超过
        // silence_after，"刚收到 hook 应该是 RUNNING"这条断言就会读到 UNKNOWN——满载的机器上线程
        // 被调度器晾一秒是常事（agora-p3l 那类偶发）。3 s 是本文件已经选过的抖动容限（agora-q8x
        // 的 GRACE 同一条理由）。代价是这条测试从 3.3 s 变成 9.3 s，买的是它不再随负载翻脸。
        const SILENCE: Duration = Duration::from_secs(3);
        let s = SessionManager::new(Arc::new(Db::open_in_memory().unwrap()), rt.clone())
            .with_status_config(MachineConfig {
                silence_after: SILENCE,
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
        std::thread::sleep(SILENCE + Duration::from_millis(100));
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

#[test]
fn handleless_external_starting_decays_through_session_manager_and_after_checkpoint_restore() {
    // agora-rkl（2026-09-18 现场：zuan 的 ef0e50 钉在 starting 180 h、a3a2a0 10 h，Mac 的
    // trion 2 d、dbs-operator 15 h）：只收到过 SessionStart 的 external 行永远 "… starting"——
    // 它的进程层恒为 Source::None，Machine::observe 第 1 步提前 return，observe_hooked 里
    // 那条衰减（agora-okr）对它们不可达，STARTING 就没有出口（这行要人做的事明明是"给它
    // 指令"）。从 SessionManager 入口覆盖（agora-uez 的教训：只喂 Machine 会漏掉接线错误），
    // 再走 agora-9dj 的检查点恢复路径。无句柄（信封不带 CLAUDE_PID）是现场里最常见的一档：
    // Codex Desktop、丢了进程号的旧检查点；它落 Liveness::Unknown，2 h 沉默兜底也接不住
    // STARTING（兜底只说"看不清"，不说"等指令"）。
    // 检查点只在 hook 事件时写、衰减不写盘，所以重启恢复出来的仍是 STARTING。
    // 宽限 3 s 的理由见上一个测试（agora-q8x）：整秒时钟比较，1 s 宽限会在跨秒那一瞬假阴性。
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
    let r = Receiver::new(home.path(), s.clone());
    let inbox = Inbox::new(home.path());
    let start = |session: &str| {
        json!({"hook_event_name":"SessionStart","session_id": session, "cwd": "/work/trion", "source": "startup",
        // 交互会话的形状：缺了交互模式才带的键就会被登记成 headless（判据见 `src/adapter/claude.rs`）。
        "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch"})
    };
    let register =
        |session: &str| external_delivery_with_env(session, BTreeMap::new(), 1, start(session));
    let id = r
        .ingest(&inbox.write(&register("ext-stuck")).unwrap())
        .unwrap()
        .unwrap()
        .session_key;
    let v = s.get(&id).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::Starting, Source::Hook)
    );
    assert!(!v.alive, "没有可信进程号：不说活着，也不说死了");
    let v = wait_decay(&s, &id);
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::TurnDone, Source::Hook),
        "无句柄 external 行的 STARTING 必须衰减：{:?}",
        v.assessment
    );
    assert!(v
        .assessment
        .reason
        .as_deref()
        .unwrap()
        .contains("awaiting first prompt"));
    // 出口照常：人给了第一条指令就开一轮。
    s.apply_hook(&id, 1, &[AgoraEvent::PromptSubmitted("first".into())])
        .unwrap();
    assert_eq!(s.get(&id).unwrap().assessment.status, Status::Running);

    // 重启路径：另起一行只收到 SessionStart 的，检查点里停在 STARTING，恢复后照样衰减。
    let id2 = r
        .ingest(&inbox.write(&register("ext-restart")).unwrap())
        .unwrap()
        .unwrap()
        .session_key;
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
        "从检查点恢复的 STARTING 同样衰减：{:?}",
        v.assessment
    );
    assert!(v
        .assessment
        .reason
        .as_deref()
        .unwrap()
        .contains("awaiting first prompt"));
    // 恢复不靠进程事实：这一行从来没有可信进程号，alive 仍为假。
    assert!(!v.alive);
}

#[test]
fn handleless_external_silence_is_not_cleared_by_replay_or_restart() {
    // agora-5gg.2（Mac 0e26ad / 14d791 / eb129d / b939bb：Codex Desktop 行沉默 62 h 仍 turn_done）：
    // 无句柄 external 行的 `external_silent_after` 以最近一条 hook 事件自己的时刻计（ADR-002 D1 修订），
    // daemon 停机 + 重放不把「沉默了多久」清零。旧写法拿 `last_hook_at`（daemon 收到的时刻）算：
    // 停机 3.5 天后重启，重放过的那批 TURN_DONE 要再等 2 h 才 UNKNOWN，没被重放、从检查点恢复的
    // 4 行却当场 UNKNOWN——同一批行两种结果（`docs/analysis/session-status-audit-2026-09-18.md`
    // §4 A4，判 (b)）。
    // 从 SessionManager 入口覆盖（agora-uez 的教训：只喂 Machine 会漏掉接线错误——事件时刻要从
    // 投递件文件名一路走到 `apply_at` 的 `at`、再随检查点落盘、重启时回填，中间断一环就回到旧行为）。
    // 三段：① 重放一条三小时前的 TurnEnded，重启后取一次视图就是 UNKNOWN，不再等 2 h；
    // ② 事件时刻随检查点落盘，所以那一行在任何一次重启后都还是 UNKNOWN（本段刻意在重启之前不取
    // 视图：不落盘的话重启后会退回 TURN_DONE，只有这一环能抓出来）；③ 修复前写的检查点（没这个键）
    // 退回按收到时刻算：收到时刻也旧了就照旧 UNKNOWN，刚写过就照旧 TURN_DONE——不因为缺键就提前
    // 把还在等的行打成“看不清”；④ 下一条 hook 出声即恢复。
    // 把兜底的 quiet 时钟改回 `last_hook_at`（不读 last_event_at）→ ①②红；不把 last_event_at 写进
    // 检查点 → ①里的 JSON 断言与②红；把读不出时的回退改成本代起始 / 零 → ③的第一段红。
    let home = tempfile::tempdir().unwrap();
    let rt = Arc::new(common::FakeRuntime::default());
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    // 三小时前落盘的投递件（daemon 停机期间写的）；external_silent_after 用生产默认的 2 h。
    let old_ms = now_ms - 3 * 3600 * 1000;
    // `external_delivery_with_env` 的 `ms` 是相对“现在”的偏移，摆不到过去；本测试要的正是
    // “落盘时刻比 daemon 这次启动早三小时”，所以信封里直接给绝对时刻。
    let old_delivery = |session: &str, ms: u64, payload: serde_json::Value| Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: None,
            agora_epoch: None,
            agent_session_id: session.into(),
            agent_env: BTreeMap::new(), // 不带 CLAUDE_PID：无句柄 → Liveness::Unknown
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: ms,
        },
        payload,
    };
    let stop = |session: &str| json!({"hook_event_name":"Stop","session_id":session,"cwd":"/work/codex-desktop","last_assistant_message":"done"});

    let (id1, id2, id3) = {
        let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
        let s = Arc::new(SessionManager::new(db.clone(), rt.clone()));
        let r = Receiver::new(home.path(), s.clone());
        let inbox = Inbox::new(home.path());
        let id1 = r
            .ingest(
                &inbox
                    .write(&old_delivery("ext-replay", old_ms, stop("ext-replay")))
                    .unwrap(),
            )
            .unwrap()
            .unwrap()
            .session_key;
        let id2 = r
            .ingest(
                &inbox
                    .write(&old_delivery("ext-legacy", old_ms, stop("ext-legacy")))
                    .unwrap(),
            )
            .unwrap()
            .unwrap()
            .session_key;
        let id3 = r
            .ingest(
                &inbox
                    .write(&old_delivery("ext-fresh", now_ms, stop("ext-fresh")))
                    .unwrap(),
            )
            .unwrap()
            .unwrap()
            .session_key;
        // 归档清掉：重启后的结论只能来自检查点，不准从 `done/` 重建（其他用例同写法）。
        inbox.prune_done(Duration::ZERO);
        assert!(inbox.pending().unwrap().is_empty());
        let path1 = checkpoint_path(home.path(), &id1);
        let cp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path1).unwrap()).unwrap();
        assert_eq!(
            cp["last_event_at"],
            json!(old_ms / 1000),
            "事件自己的时刻随检查点落盘：{cp}"
        );
        // 第二、三行假装是 agora-5gg.2 之前写的 v3 检查点：同一个版本号，但没有 last_event_at
        // 这个键（= 没记过），沉默兜底只能退回按收到时刻算。第二行的收到时刻也摆到三小时前
        // （长时间停机后写下的旧检查点），第三行保持刚刚（快速重启）：两者结论相反，正好把
        // 回退目标钉在 `last_hook_at` 而不是本代起始。
        let path2 = checkpoint_path(home.path(), &id2);
        let mut cp2: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path2).unwrap()).unwrap();
        cp2.as_object_mut().unwrap().remove("last_event_at");
        cp2["last_hook_at"] = json!(old_ms / 1000);
        std::fs::write(&path2, cp2.to_string()).unwrap();
        let path3 = checkpoint_path(home.path(), &id3);
        let mut cp3: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path3).unwrap()).unwrap();
        cp3.as_object_mut().unwrap().remove("last_event_at");
        std::fs::write(&path3, cp3.to_string()).unwrap();
        (id1, id2, id3)
    };

    // 重启。注意：上面那一次启动从没取过视图，所以三行都还是 TURN_DONE——沉默兜底只在 observe
    // 里落、不写进检查点；第一行重启后就被判 UNKNOWN，只能是因为检查点里带了事件时刻。
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let restarted = Arc::new(SessionManager::new(db, rt));
    restarted.reconcile().unwrap();
    let r2 = Receiver::new(home.path(), restarted.clone());
    assert_eq!(r2.replay().unwrap(), 0, "投递箱已空：恢复只靠检查点");
    let v = restarted.get(&id1).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::Unknown, Source::Hook),
        "重放三小时前的 TurnEnded 后不需再等 2 h：{:?}",
        v.assessment
    );
    assert_eq!(
        v.assessment.reason.as_deref(),
        Some("hooks silent; no process handle")
    );
    assert!(!v.alive, "无句柄行：不说活着也不说死了");

    // 修复前的旧检查点（没这个键）且收到时刻也已满 2 h：按收到时刻算，结论与修复前一致。
    let v2 = restarted.get(&id2).unwrap();
    assert_eq!(
        (v2.assessment.status, v2.assessment.source),
        (Status::Unknown, Source::Hook),
        "旧检查点里收到时刻已满阈值：照旧 UNKNOWN：{:?}",
        v2.assessment
    );
    assert_eq!(
        v2.assessment.reason.as_deref(),
        Some("hooks silent; no process handle")
    );

    // 同样是旧检查点（读不出事件时刻），但刚刚写过（快速重启）：不因为缺键就当场判定“看不清”。
    let v3 = restarted.get(&id3).unwrap();
    assert_eq!(
        (v3.assessment.status, v3.assessment.source),
        (Status::TurnDone, Source::Hook),
        "旧检查点 + 刚写过：保持旧行为，不提前打 UNKNOWN：{:?}",
        v3.assessment
    );

    // 沉默不是终态：下一条 hook 出声即恢复。
    restarted.apply_hook(&id1, 1, &[AgoraEvent::Idle]).unwrap();
    let v = restarted.get(&id1).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::TurnDone, Source::Hook),
        "{:?}",
        v.assessment
    );
}

/// 无句柄 external 行的投递件：没有 AGORA_*，身份是 (host, agent_session_id)，进程号在 CLAUDE_PID。
/// 时刻从"现在"起算（`ms` 只是序号）：进程号要与报来它的 hook 时刻对一下，进程不得晚于 hook。
fn external_delivery(
    agent_session: &str,
    pid: u32,
    ms: u64,
    payload: serde_json::Value,
) -> Delivery {
    external_delivery_with_env(
        agent_session,
        BTreeMap::from([("CLAUDE_PID".to_owned(), pid.to_string())]),
        ms,
        payload,
    )
}

/// 同上，但 `agent_env` 由调用方给：空表 = 宿主没报进程号（Codex Desktop 的共用 app-server、
/// 没有 `CLAUDE_PID` 的宿主）→ `SessionManager.external_pids` 里没有这一行 → `Liveness::Unknown`。
fn external_delivery_with_env(
    agent_session: &str,
    agent_env: BTreeMap<String, String>,
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
            agent_env,
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
        let start = json!({"hook_event_name":"SessionStart","session_id":"ext-1","cwd":"/work/agora","source":"startup", "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch"});
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
        let start = json!({"hook_event_name":"SessionStart","session_id":"ext-2","cwd":"/work/agora","source":"startup", "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch"});
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
        let start = json!({"hook_event_name":"SessionStart","session_id":"ext-v2","cwd":"/work/agora","source":"startup", "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch"});
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

/// agora-t36 的守卫之一：清理挂在 sweep 周期上，daemon 不重启也回收。
/// 关掉 `Receiver::sweep` 里的 `maybe_prune_done()` 这条红。
#[test]
fn the_sweep_prunes_the_done_archive_instead_of_waiting_for_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db, rt));
    let id = create(&s, "claude");
    let inbox = Inbox::new(home.path());
    // 节流间隔 0：每轮 sweep 都真的扫，先验"扫了会删"。
    // 保留期仍是默认的 24 h：这条要验的是"按 mtime 判超期"，所以下面靠 age_file 造老文件。
    let r = Receiver::new(home.path(), s.clone()).with_prune_interval(Duration::ZERO);

    let path = inbox
        .write(&delivery(
            &id,
            1,
            1,
            json!({"hook_event_name":"UserPromptSubmit","prompt":"implement"}),
        ))
        .unwrap();
    r.ingest(&path).unwrap();
    let archived = inbox.completed().unwrap();
    assert_eq!(archived.len(), 1, "ingest 之后应留在 done/ 供排障");

    age_file(&archived[0], Duration::from_secs(48 * 3600));
    r.sweep();
    assert!(
        inbox.completed().unwrap().is_empty(),
        "sweep 应该清掉超过保留期的归档，而不是等下次 daemon 启动重放时才清（agora-t36）"
    );
}

/// agora-t36 的守卫之二：节流住，别每 5 s 一轮 sweep 都去扫目录。
#[test]
fn pruning_is_throttled_so_the_five_second_sweep_does_not_scan_every_round() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db, rt));
    let id = create(&s, "claude");
    let inbox = Inbox::new(home.path());
    let r = Receiver::new(home.path(), s.clone()).with_prune_interval(Duration::from_secs(3600));

    // 第一轮 sweep 记下时刻（本进程里还没扫过，一定跑）。
    r.sweep();

    // 之后才出现的超期文件：还在节流窗口里，第二轮不该扫到它。
    let path = inbox
        .write(&delivery(
            &id,
            1,
            2,
            json!({"hook_event_name":"UserPromptSubmit","prompt":"again"}),
        ))
        .unwrap();
    r.ingest(&path).unwrap();
    let archived = inbox.completed().unwrap();
    age_file(&archived[0], Duration::from_secs(48 * 3600));

    r.sweep();
    assert_eq!(
        inbox.completed().unwrap().len(),
        1,
        "距上次清理不到 PRUNE_INTERVAL，这一轮 sweep 不该再扫目录（agora-t36）"
    );
}

/// agora-5gg.14：`hooks/state/` 里没有对应行的检查点由 sweep 顺手清掉。
/// 现场（2026-09-18 Mac）：`state/393031303531.json` 躺在那儿，local_id `901051` 在库与 API 里
/// 都没有行——`restore_hook_checkpoints` 只按行迭代，这种文件既不会被加载也不会复活，没人清就
/// 一辈子占着那个位置。
///
/// 改坏一次看见红：
/// - 去掉 `Receiver::maybe_prune_done` 末尾对 `prune_orphan_hook_checkpoints` 的调用 → 前两条断言红（文件还在）；
/// - `hook_state::prune_orphans` 的 keep 集合塞成空（等价于「本轮没读到库」）→ 「有行的一个字节都不该动」红；
/// - 把那一条 info 改成逐文件一行、或降成 debug → 日志那三条断言各红一条。
#[test]
fn the_sweep_deletes_hook_checkpoints_that_have_no_row() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db, rt));
    let id = create(&s, "claude");
    let inbox = Inbox::new(home.path());
    // 节流间隔 0：每轮 sweep 都真扫目录（同上面 done/ 那条守卫的用法）。
    let r = Receiver::new(home.path(), s.clone()).with_prune_interval(Duration::ZERO);

    // 有行的那一份：走真投递链路、由 ingest 写出，不手写。
    let path = inbox
        .write(&delivery(
            &id,
            1,
            1,
            json!({"hook_event_name":"UserPromptSubmit","prompt":"implement"}),
        ))
        .unwrap();
    r.ingest(&path).unwrap();
    let live = checkpoint_path(home.path(), &id);
    let live_before = std::fs::read(&live).unwrap();
    let live_name = live.file_name().unwrap().to_string_lossy().into_owned();

    // 无行的那两份：`901051` 逐字节 hex 就是 `393031303531`（Mac 现场那个文件名）；`.part` 是
    // `save` 崩在中途留下的形状，同一条规则。
    let state = home.path().join("hooks/state");
    let orphan = state.join("393031303531.json");
    let orphan_part = state.join("393031303531.part");
    std::fs::write(&orphan, b"{\"version\":3}").unwrap();
    std::fs::write(&orphan_part, b"{\"version\":3").unwrap();

    let logs = common::capture_logs(|| r.sweep());

    assert!(
        !orphan.exists(),
        "sweep 应该删掉无行的检查点（agora-5gg.14）"
    );
    assert!(!orphan_part.exists(), "无行的 `.part` 同一条规则");
    assert_eq!(
        std::fs::read(&live).unwrap(),
        live_before,
        "有行的检查点一个字节都不该动"
    );
    assert!(s.get(&id).is_ok(), "这一轮只清文件，不碰库里的行");

    // 验收点：日志一条（不是一文件一条），正文点名被删的文件，不牵连有行的那个。
    let lines: Vec<&str> = logs.lines().filter(|l| l.contains("没有对应行")).collect();
    assert_eq!(lines.len(), 1, "应该只有一行 info，实际:\n{logs}");
    assert!(
        lines[0].contains("INFO"),
        "记的是 info 而不是 warn: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("393031303531.json") && lines[0].contains("393031303531.part"),
        "文件名要交给排障的人（它是 id 的 hex）: {}",
        lines[0]
    );
    assert!(
        !lines[0].contains(&live_name),
        "有行的不该出现在被删的名单里: {}",
        lines[0]
    );
}

/// 无行检查点的清理挂在 `PRUNE_INTERVAL` 那条节流上，不跟 5 s 一轮的 sweep 走（扫目录的代价与
/// done/ 同一条理由）。改坏法：把 `prune_orphan_hook_checkpoints` 的调用从 `maybe_prune_done`
/// 里挪到 `sweep` 开头（绕开节流）→ 这条红。
#[test]
fn orphan_checkpoint_pruning_rides_the_same_throttle_as_the_archive() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db, rt));
    let r = Receiver::new(home.path(), s).with_prune_interval(Duration::from_secs(3600));

    // 第一轮 sweep 记下时刻（本进程里还没扫过，一定跑）。
    r.sweep();

    // 之后才出现的孤儿：还在节流窗口里，第二轮不该去扫 `state/`。
    let state = home.path().join("hooks/state");
    std::fs::create_dir_all(&state).unwrap();
    let orphan = state.join("393031303531.json");
    std::fs::write(&orphan, b"{\"version\":3}").unwrap();
    r.sweep();
    assert!(
        orphan.exists(),
        "距上次清理不到 PRUNE_INTERVAL，这一轮 sweep 不该再扫一次目录（agora-5gg.14）"
    );
}

/// agora-t36 的守卫之三：归档里的大结果被截断，状态机看到的仍是完整 payload；`tool_input` 不截。
#[test]
fn a_big_tool_result_is_truncated_in_the_archive_but_reaches_the_state_machine_whole() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db, rt));
    let id = create(&s, "claude");
    let inbox = Inbox::new(home.path());
    let r = Receiver::new(home.path(), s.clone());

    // 现场的形状：结果是对象不是字符串（claude 4511 条里 4173 条如此），所以按紧凑 JSON 的字节数算。
    let big = "x".repeat(60 * 1024);
    let long_command = format!("echo {}", "y".repeat(20 * 1024));
    let path = inbox
        .write(&delivery(
            &id,
            1,
            3,
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_use_id": "tool-1",
                "tool_input": { "command": long_command },
                "tool_response": { "stdout": big },
            }),
        ))
        .unwrap();
    r.ingest(&path).unwrap();

    // 状态机侧（账本记的就是 ingest 读到的那份）：一个字没少。
    let seen = r.received();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].delivery.payload["tool_response"]["stdout"]
            .as_str()
            .unwrap()
            .len(),
        big.len(),
        "截断只发生在挪进 done/ 那一步，状态机判 hold 读的仍是完整 payload"
    );

    // 归档侧：截到上限、带标记。
    let archived = inbox.completed().unwrap();
    assert_eq!(archived.len(), 1);
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archived[0]).unwrap()).unwrap();
    let cut = on_disk["payload"]["tool_response"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "超限的结果应被换成带标记的字符串，实际：{}",
                on_disk["payload"]["tool_response"]
            )
        });
    assert!(
        cut.ends_with(" bytes]") && cut.contains("[truncated "),
        "截断要留可辨认的标记：{}",
        &cut[cut.len().saturating_sub(80)..]
    );
    assert!(
        cut.len() < 9 * 1024,
        "截断后不该还有 {} 字节（上限 8 KB + 标记）",
        cut.len()
    );

    // 但 tool_input 一个字不动：agora-pzi 的权限摘要要从它取主参数，归档重建也靠它。
    assert_eq!(
        on_disk["payload"]["tool_input"]["command"]
            .as_str()
            .unwrap()
            .len(),
        long_command.len(),
        "tool_input 不在截断名单里（见 ARCHIVE_TRUNCATED_KEYS 的注释）"
    );
}

/// 把文件的 mtime 往前推，模拟"躺了很久的归档"。
fn age_file(path: &std::path::Path, age: Duration) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    let when = std::time::SystemTime::now() - age;
    f.set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
}

/// agora-t36 的守卫之四：保留期取自 `hooks.inbox_retention` 而不是硬编码的 24 h。
/// 把 `with_pruning` 的 retention 换回常量 `DONE_RETENTION` 这条红。
#[test]
fn the_retention_comes_from_config_not_from_the_hardcoded_twenty_four_hours() {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let s = Arc::new(SessionManager::new(db, rt));
    let id = create(&s, "claude");
    let inbox = Inbox::new(home.path());
    // 保留期 0：刚归档的文件（mtime 就是现在）也该被清，不必等 24 h。
    let r = Receiver::new(home.path(), s.clone()).with_pruning(Duration::ZERO, Duration::ZERO);

    let path = inbox
        .write(&delivery(
            &id,
            1,
            4,
            json!({"hook_event_name":"UserPromptSubmit","prompt":"now"}),
        ))
        .unwrap();
    r.ingest(&path).unwrap();
    assert_eq!(inbox.completed().unwrap().len(), 1);

    r.sweep();
    assert!(
        inbox.completed().unwrap().is_empty(),
        "retention = 0 时刚归档的文件也该清掉：保留期要跟着 hooks.inbox_retention 走（agora-t36）"
    );
}
