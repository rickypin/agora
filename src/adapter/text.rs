//! 文本兜底与预览的共用零件（ADR-002 D6；MISSION §6.7；agora-dvh.8）。
//!
//! 只服务无 hook 的会话（generic shell、采纳的未知会话、custom）；有 hook 的 agent 走这条路径
//! 的唯一出口是"hook 沉默 → UNKNOWN"（D1）。规则：
//! - 只看屏幕末尾 **8 个非空行**，不是整段 tail——devcenter 的反例：scrollback 里 `cat` 出来的
//!   源码含 `[y/N]` 把会话钉在 WAITING（`testdata/generic/pane/scrollback_source.txt`）。
//! - WAITING 模式大小写不敏感、**行尾锚定**；密码提示同样 WAITING 但 reason 是 `secret`，
//!   提示行内容不进 reason（UI 不回显）。conf 0.8；只以 `?` 结尾的问句 conf 0.7，靠状态机的
//!   连续 tick 驻留（`text_ticks`）补足"随后无输出"。
//! - RUNNING / IDLE 不在这里：那是活动层（状态机按 `output_at` 判）。

use crate::status::{DetectionResult, Status};

/// 末尾几个非空行参与判定（D6）。
pub const TAIL_LINES: usize = 8;

/// 去掉 CSI / OSC 序列与其它控制字符。`capture-pane -p` 不带 `-e` 本就不含颜色，这里是
/// 防御：pane 里 agent 自己打出的裸 ESC（进度条、光标控制）不能进侧栏，也不能骗过模式匹配。
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI：参数 0x30–0x3F，中间 0x20–0x2F，终结 0x40–0x7E。
                    for t in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&t) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC：到 BEL 或 ESC \。
                    let mut prev = '\0';
                    for t in chars.by_ref() {
                        if t == '\u{7}' || (prev == '\u{1b}' && t == '\\') {
                            break;
                        }
                        prev = t;
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if c == '\n' || c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// 屏幕末尾最后 `n` 个非空行（已 strip ANSI、去首尾空白），按屏幕顺序。
pub fn tail_lines(screen: &str, n: usize) -> Vec<String> {
    let clean = strip_ansi(screen);
    let mut lines: Vec<String> = clean
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(n)
        .map(str::to_owned)
        .collect();
    lines.reverse();
    lines
}

/// 屏幕末尾最后一个非空行，截断（预览用；MISSION §6.7 不显示大段内容）。
pub fn last_line(screen: &str, max_chars: usize) -> Option<String> {
    tail_lines(screen, 1)
        .pop()
        .map(|l| l.chars().take(max_chars).collect())
}

/// 行尾锚定的 WAITING 模式（devcenter v1 启发式，D6）。比较时行已小写。
const WAITING_SUFFIXES: &[&str] = &[
    "[y/n]",
    "(y/n)",
    "[y/n]:",
    "(y/n):",
    "do you want to proceed?",
    "would you like",
    "approve",
    "allow",
    "continue?",
    "press enter",
    "press enter to continue",
    "press enter to continue...",
];

/// 密码提示：`password:`、`[sudo] password for ricky:`、`Enter passphrase for key '…':`——
/// 提示词在行内任意位置、行以冒号结尾。内容不回显（reason 只写 `secret`）。
const SECRET_WORDS: &[&str] = &["password", "passphrase", "passcode"];

fn is_secret_prompt(lower: &str) -> bool {
    lower.trim_end().ends_with(':') && SECRET_WORDS.iter().any(|w| lower.contains(w))
}

fn ends_with_any(line: &str, suffixes: &[&str]) -> bool {
    let l = line.trim_end_matches([' ', ':', '\u{a0}']);
    suffixes
        .iter()
        .any(|s| l.ends_with(s.trim_end_matches(':')) || line.ends_with(s))
}

/// 通用检测：`tail` 是末尾 ≤ 8 个非空行（[`tail_lines`]）。只认 WAITING；其余交给活动层。
pub fn detect(tail: &[&str]) -> Option<DetectionResult> {
    let last = tail.last()?;
    let lower = last.to_lowercase();
    if is_secret_prompt(&lower) {
        return Some(DetectionResult {
            status: Status::Waiting,
            confidence: 0.8,
            reason: "secret".into(),
        });
    }
    // 提示可以在末尾几行里（TUI 的选项列表常排在问句下面），但它下面只能是选项行——
    // 下面已经有普通输出或新的 shell 提示符，说明那句早答过了（scrollback 污染，D6）。
    for (i, line) in tail.iter().enumerate().rev() {
        let lower = line.to_lowercase();
        if ends_with_any(&lower, WAITING_SUFFIXES)
            && tail[i + 1..].iter().all(|l| is_option_line(l))
        {
            return Some(DetectionResult {
                status: Status::Waiting,
                confidence: 0.8,
                reason: format!("prompt: {}", clip(line, 80)),
            });
        }
    }
    if lower.ends_with('?') {
        return Some(DetectionResult {
            status: Status::Waiting,
            confidence: 0.7,
            reason: format!("question: {}", clip(last, 80)),
        });
    }
    None
}

/// 选项行：`❯ 1. Yes`、`2) No`、`[ ] foo`、`○ option`、`> yes`。
fn is_option_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with(|c: char| c.is_ascii_digit())
        || t.starts_with(['❯', '>', '○', '●', '◯', '◉', '[', '(', '-', '*', '•'])
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(screen: &str) -> Option<DetectionResult> {
        let tail = tail_lines(screen, TAIL_LINES);
        let refs: Vec<&str> = tail.iter().map(String::as_str).collect();
        detect(&refs)
    }

    #[test]
    fn only_the_last_eight_nonempty_lines_count() {
        let mut screen = String::from("Proceed? [y/N]\n");
        for i in 0..8 {
            screen.push_str(&format!("line {i}\n\n"));
        }
        assert_eq!(d(&screen), None);
        assert_eq!(tail_lines(&screen, TAIL_LINES).len(), 8);
    }

    #[test]
    fn prompts_are_anchored_at_line_end_and_case_insensitive() {
        assert_eq!(d("Overwrite file? (Y/N)").unwrap().status, Status::Waiting);
        assert!(d("$ echo 'Do you want to proceed? no'\n$ ").is_none());
        assert_eq!(
            d("Do you want to proceed?\n❯ 1. Yes\n  2. No")
                .unwrap()
                .confidence,
            0.8
        );
        assert_eq!(d("What is your name?").unwrap().confidence, 0.7);
        assert!(d("ricky@mac agora % ").is_none());
    }

    #[test]
    fn password_prompts_do_not_echo_the_line() {
        let r = d("sudo: unable to resolve host\n[sudo] password for ricky:").unwrap();
        assert_eq!((r.status, r.reason.as_str()), (Status::Waiting, "secret"));
        assert_eq!(d("Enter passphrase:").unwrap().reason, "secret");
    }

    #[test]
    fn ansi_is_stripped_before_matching() {
        let r = d("\u{1b}[1;32mContinue?\u{1b}[0m").unwrap();
        assert_eq!(r.reason, "prompt: Continue?");
        assert_eq!(
            last_line("a\n\u{1b}]0;title\u{7}b\u{1b}[K\n\n", 10).as_deref(),
            Some("b")
        );
    }
}
