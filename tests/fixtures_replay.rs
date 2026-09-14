//! fixture 回放（ADR-002 D10；agora-dvh.5）：`testdata/<agent>/<version>/hooks/*.jsonl` 每个文件
//! 喂给对应 adapter + 状态机，`expect` 行全部命中才绿。七个场景缺一个也红——**冒烟-only 目录**除外，
//! 规则与它的理由见 `the_seven_scenarios_exist_for_every_recorded_version_dir`。

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

/// 版本目录只准长两种样子，第三种一律红：
///
/// - **实测目录**：七个场景一个不缺（真交互录，或按实测键集合合成），`expect` 覆盖状态机各条路。
/// - **冒烟-only 目录**：除 `headless.jsonl` 外一条没有。它是 `tests/hook_smoke.rs` 按**精确版本号**
///   要的键集合基线——agent 一升级冒烟就红着等这个文件，而补录七个真交互场景是一个 epic 的量
///   （agora-3la.1 那么贵是有原因的）。不给这一档，"消红灯"和"补齐全套实测"就被绑成同一件事，
///   结果就是红灯长红（2026-09-14 实测：claude 2.1.270 / codex 0.153.4 / grok 1.0.30 三条冒烟全红，
///   而键集合与上一版逐键相同——codex 只换了 model 名，grok 只把别名键 `hook_event_name` 的值从
///   小写蛇形改成 PascalCase，`src/adapter/grok.rs` 的 `event_key` 去下划线再小写，两种形态归一，
///   新 fixture 的 expect 行就是这件事的守卫）。
///
/// 豁免只认"除 headless 外一条都没有"：录了部分场景的目录仍按七个的场景数红，防"补了两个就当齐了"。
/// 删光七个场景来钻豁免也不行，兜底见 `every_hooked_agent_keeps_one_fully_recorded_version_dir`。
#[test]
fn the_seven_scenarios_exist_for_every_recorded_version_dir() {
    let all = fixtures();
    let mut dirs: Vec<PathBuf> = all
        .iter()
        .map(|(_, p)| p.parent().unwrap().to_path_buf())
        .collect();
    dirs.sort();
    dirs.dedup();
    assert!(!dirs.is_empty());
    for dir in dirs {
        let present: Vec<String> = SCENARIOS
            .iter()
            .filter(|s| dir.join(format!("{s}.jsonl")).exists())
            .map(|s| (*s).to_owned())
            .collect();
        if present.is_empty() {
            let extra: Vec<String> = fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".jsonl") && *n != "headless.jsonl")
                .collect();
            assert!(
                extra.is_empty() && dir.join("headless.jsonl").exists(),
                "{} 既不是七场景齐全的实测目录，也不是干净的冒烟-only 目录（已录 {:?}，多余 {:?}）",
                dir.display(),
                present,
                extra
            );
            continue;
        }
        for s in SCENARIOS {
            assert!(
                dir.join(format!("{s}.jsonl")).exists(),
                "{} 缺场景 {s}（已录 {:?}；要么补齐七个，要么一个都不留按冒烟-only 目录走）",
                dir.display(),
                present
            );
        }
    }
}

/// 每个有 hook 的 agent 至少留一个七场景齐全的实测目录。这是上面那条豁免的兜底：豁免允许"只有
/// headless"的版本目录存在，但不许某个 agent 的全套实测被悄悄删光、只剩冒烟。
#[test]
fn every_hooked_agent_keeps_one_fully_recorded_version_dir() {
    let all = fixtures();
    let mut agents: Vec<String> = all.iter().map(|(a, _)| a.clone()).collect();
    agents.sort();
    agents.dedup();
    assert!(!agents.is_empty());
    for agent in agents {
        let mut dirs: Vec<PathBuf> = all
            .iter()
            .filter(|(a, _)| *a == agent)
            .map(|(_, p)| p.parent().unwrap().to_path_buf())
            .collect();
        dirs.sort();
        dirs.dedup();
        let complete = dirs.iter().any(|dir| {
            SCENARIOS
                .iter()
                .all(|s| dir.join(format!("{s}.jsonl")).exists())
        });
        assert!(
            complete,
            "{agent} 没有任何一个版本目录录齐七个场景：冒烟-only 的豁免不能被用来删掉全套实测"
        );
    }
}
