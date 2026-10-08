use agora::hook::budget::*;
use agora::hook::Envelope;
use agora::hook::{Delivery, HookError, Inbox};
use std::collections::BTreeMap;
use std::fs;

fn delivery(t: u64) -> Delivery {
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: None,
            agora_epoch: None,
            agent_session_id: "bounded".into(),
            agent_env: BTreeMap::new(),
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: t,
        },
        payload: serde_json::json!({"hook_event_name":"PostToolUse", "tool_use_id":"parallel-a",
            "tool_response": "x".repeat(100_000)}),
    }
}

#[test]
fn admission_is_bounded_without_evicting_accepted_events() {
    let home = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(home.path());
    let first = inbox.write_with_limits(&delivery(1), 2, 32_000).unwrap();
    inbox.write_with_limits(&delivery(2), 2, 32_000).unwrap();
    assert!(matches!(
        inbox.write_with_limits(&delivery(3), 2, 32_000),
        Err(HookError::Capacity)
    ));
    assert!(first.exists());
    assert_eq!(inbox.pending().unwrap().len(), 2);
    inbox.done(&first).unwrap();
    inbox.write_with_limits(&delivery(3), 2, 32_000).unwrap();
}

#[test]
fn compact_results_preserve_adapter_semantics_and_gap_is_visible() {
    let home = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(home.path());
    let d = delivery(1);
    let path = inbox.write_bounded(&d).unwrap();
    let stored = inbox.read(&path).unwrap();
    let hooks = agora::adapter::for_host("claude").unwrap();
    assert_eq!(hooks.parse(&d.payload), hooks.parse(&stored.payload));
    assert!(fs::metadata(path).unwrap().len() < 16_000);
    let mut huge = delivery(2);
    huge.payload["prompt"] = "p".repeat(MAX_DELIVERY_BYTES + 1).into();
    assert!(inbox.write_bounded(&huge).is_err());
    assert_eq!(inbox.observation_gap().unwrap()["reason"], "inbox_capacity");
}

#[test]
fn concurrent_admission_never_exceeds_file_budget() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().to_owned();
    let threads: Vec<_> = (0..24)
        .map(|i| {
            let root = root.clone();
            std::thread::spawn(move || {
                Inbox::new(&root)
                    .write_with_limits(&delivery(i), 8, 128_000)
                    .is_ok()
            })
        })
        .collect();
    let accepted = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(accepted, 8);
    assert_eq!(Inbox::new(home.path()).pending().unwrap().len(), 8);
}

#[test]
fn all_recorded_adapters_keep_semantics_after_result_compaction() {
    fn files(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|x| x == "jsonl") {
                out.push(path);
            }
        }
    }
    let home = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(home.path());
    let mut count = 0;
    for host in ["claude", "codex", "grok", "pi"] {
        let hooks = agora::adapter::for_host(host).unwrap();
        let mut paths = Vec::new();
        files(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join(host),
            &mut paths,
        );
        for path in paths {
            for line in std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| l.starts_with('{'))
            {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                let Some(payload) = row.get("payload") else {
                    continue;
                };
                let mut d = delivery(count);
                d.envelope.host = host.into();
                d.payload = payload.clone();
                if let Some(obj) = d.payload.as_object_mut() {
                    obj.insert("tool_response".into(), "x".repeat(16_000).into());
                    obj.insert(
                        "toolResult".into(),
                        serde_json::json!({"result":"y".repeat(16_000)}),
                    );
                }
                let p = inbox.write_bounded(&d).unwrap();
                let compact = inbox.read(&p).unwrap();
                assert_eq!(
                    hooks.parse(&d.payload),
                    hooks.parse(&compact.payload),
                    "{}",
                    path.display()
                );
                assert_eq!(hooks.hold_key(&d.payload), hooks.hold_key(&compact.payload));
                assert_eq!(
                    hooks.release_for(&d.payload),
                    hooks.release_for(&compact.payload)
                );
                assert_eq!(
                    hooks.input_channel(&d.payload),
                    hooks.input_channel(&compact.payload)
                );
                assert_eq!(
                    hooks.working_directory(&d.payload),
                    hooks.working_directory(&compact.payload)
                );
                std::fs::remove_file(p).unwrap();
                count += 1;
            }
        }
    }
    assert!(
        count > 100,
        "fixture traversal must really cover the recorded hosts"
    );
}

#[test]
fn byte_budget_is_independent_of_file_count_and_empty_dirs_are_reclaimed() {
    let home = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(home.path());
    let p = inbox.write_with_limits(&delivery(1), 100, 12_000).unwrap();
    assert!(matches!(
        inbox.write_with_limits(&delivery(2), 100, 12_000),
        Err(HookError::Capacity)
    ));
    let old_dir = p.parent().unwrap().to_owned();
    inbox.done(&p).unwrap();
    let mut next = delivery(3);
    next.envelope.agent_session_id = "another".into();
    inbox.write_with_limits(&next, 100, 12_000).unwrap();
    assert!(!old_dir.exists());
}
