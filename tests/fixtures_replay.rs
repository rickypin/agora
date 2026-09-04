//! fixture 回放（ADR-002 D10；agora-dvh.5）：`testdata/<agent>/<version>/hooks/*.jsonl` 每个文件
//! 喂给对应 adapter + 状态机，`expect` 行全部命中才绿。七个场景缺一个也红。

use std::fs;
use std::path::{Path, PathBuf};

use agora::adapter;

const SCENARIOS: &[&str] = &[
    "turn_complete",
    "permission_terminal",
    "permission_dashboard",
    "clear",
    "api_error",
    "interrupted",
    "parallel_tools",
];

fn fixtures() -> Vec<(String, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut out = Vec::new();
    for agent in fs::read_dir(&root).unwrap().flatten() {
        if !agent.path().is_dir() {
            continue;
        }
        let name = agent.file_name().to_string_lossy().into_owned();
        for version in fs::read_dir(agent.path()).unwrap().flatten() {
            let hooks = version.path().join("hooks");
            let Ok(files) = fs::read_dir(&hooks) else {
                continue;
            };
            for f in files.flatten() {
                if f.path().extension().is_some_and(|e| e == "jsonl") {
                    out.push((name.clone(), f.path()));
                }
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_fixture_replays_to_its_expected_states() {
    let all = fixtures();
    assert!(!all.is_empty(), "testdata 里没有 fixture");
    for (agent, path) in &all {
        let hooks = adapter::for_host(agent).unwrap_or_else(|| {
            panic!(
                "{agent} 没有 AgentHooks，fixture {} 无处回放",
                path.display()
            )
        });
        let text = fs::read_to_string(path).unwrap();
        assert!(
            text.contains("\"expect\""),
            "{} 一条断言都没有",
            path.display()
        );
        if let Err(err) = adapter::replay::replay(hooks, &text) {
            panic!("{}: {err}", path.display());
        }
    }
}

#[test]
fn the_seven_scenarios_exist_for_every_hooked_agent_version() {
    let all = fixtures();
    let mut dirs: Vec<PathBuf> = all
        .iter()
        .map(|(_, p)| p.parent().unwrap().to_path_buf())
        .collect();
    dirs.dedup();
    assert!(!dirs.is_empty());
    for dir in dirs {
        for s in SCENARIOS {
            assert!(
                dir.join(format!("{s}.jsonl")).exists(),
                "{} 缺场景 {s}",
                dir.display()
            );
        }
    }
}
