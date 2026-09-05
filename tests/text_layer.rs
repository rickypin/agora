//! 文本兜底（ADR-002 D6；A5；agora-dvh.8）：`testdata/generic/pane/*.txt` 每个文件首行
//! `# expect: waiting [secret] | none`，其余是屏幕内容（可含 ANSI）；每条 WAITING 模式一个文件，
//! 外加 scrollback 污染反例。只喂文本层与预览，不碰状态机（那是 tests/state_machine.rs）。

use std::fs;
use std::path::Path;

use agora::adapter::{self, text};
use agora::status::Status;

fn fixtures() -> Vec<(String, String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/generic/pane");
    let mut out = Vec::new();
    for f in fs::read_dir(&dir).unwrap().flatten() {
        let raw = fs::read_to_string(f.path()).unwrap();
        let (head, body) = raw.split_once('\n').unwrap_or((&raw, ""));
        let expect = head
            .strip_prefix("# expect:")
            .unwrap_or_else(|| panic!("{} 首行不是 # expect:", f.path().display()))
            .trim()
            .to_owned();
        out.push((
            f.file_name().to_string_lossy().into_owned(),
            expect,
            body.to_owned(),
        ));
    }
    out.sort();
    out
}

#[test]
fn every_pane_fixture_detects_as_expected() {
    let all = fixtures();
    assert!(all.len() >= 12, "fixture 太少: {}", all.len());
    let mut secrets = 0;
    for (name, expect, screen) in &all {
        // 无 adapter 的类型（custom）与 shell 都走通用启发式。
        for agent in ["shell", "custom"] {
            let got = adapter::detect_screen(agent, screen);
            match expect.as_str() {
                "none" => assert!(got.is_none(), "{name}: 期望 none，得到 {got:?}"),
                "waiting" => {
                    let r = got.unwrap_or_else(|| panic!("{name}: 期望 waiting，得到 None"));
                    assert_eq!(r.status, Status::Waiting, "{name}");
                    assert!(r.confidence >= 0.7 && r.confidence <= 0.8, "{name}");
                    assert_ne!(r.reason, "secret", "{name}");
                }
                "waiting secret" => {
                    let r = got.unwrap_or_else(|| panic!("{name}: 期望 secret，得到 None"));
                    assert_eq!(
                        (r.status, r.reason.as_str()),
                        (Status::Waiting, "secret"),
                        "{name}"
                    );
                    // 不回显：提示行的任何内容都不进 reason。
                    let last = text::tail_lines(screen, 1).pop().unwrap();
                    assert!(!r.reason.contains(&last), "{name}");
                    secrets += 1;
                }
                other => panic!("{name}: 不认识的 expect {other}"),
            }
        }
    }
    assert!(secrets >= 2);
}

#[test]
fn preview_is_the_last_nonempty_line_stripped_and_clipped() {
    let screen = "\u{1b}[32mok\u{1b}[0m\n\n\u{1b}]0;title\u{7}ricky@mac % \n\n";
    assert_eq!(text::last_line(screen, 160).as_deref(), Some("ricky@mac %"));
    assert_eq!(text::last_line("abcdef", 3).as_deref(), Some("abc"));
    assert_eq!(text::last_line("\n\n", 10), None);
}

#[test]
fn hooked_agents_never_get_text_waiting_from_this_path() {
    // 有 hook 的 agent 走文本路径的唯一出口是 hook 沉默 → UNKNOWN（D1）：detect_screen 对它们
    // 也能算，但 session 层也在 hook 沉默时调它，但裁决只允许 UNKNOWN（tests/state_machine.rs::text_cannot_raise_hooked_session）。
    assert!(adapter::has_hooks("claude"));
    assert!(!adapter::has_hooks("shell") && !adapter::has_hooks("custom"));
}
