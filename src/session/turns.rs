//! 会话的轮次日志（agora-2mff；MISSION §6.9 / A53；docs/spec/api.md「轮次」；ADR-002 D12）。
//!
//! 手机会话卡要看得懂「这一轮之前聊过什么」。宿主自己的 transcript 最全，但私有格式随版本漂移、
//! 体量大（2026-10-10 zuan 一个 Claude 会话 7.7 MB，人打的话 17 条），节点也从来没存它的路径
//! （ADR-002 Non-Goals）。这里只记 hook 层**已经**交到节点手里的东西：人说的话（`prompt.submitted`，
//! 完整文本）、这一轮的最终回复（`turn.ended`），外加失败与宿主注入两种轮边界。
//!
//! 一个会话一个追加型 JSONL：`<AGORA_HOME>/turns/<hex(id)>.jsonl`（目录 0700、文件 0600；命名同
//! hook 检查点，库里的 id 不参与路径语义）。不进 SQLite：session store 不变、不升 schema，升级后
//! 照样能回滚到旧二进制。
//!
//! - **有界**：读时只交最近 [`KEEP`] 轮；条目数超过 [`COMPACT_AT`] 时原子重写成最近 KEEP 轮。
//!   单条文本超过 [`TEXT_MAX`] 字节截在字符边界并注明。
//! - **幂等**：条目带事件自己的毫秒时刻（投递件文件名里的那个）。早于末条的、或与末条同刻同内容的
//!   不再追加——启动重放与归档重建按落盘顺序把事件再走一遍，不会重复记。
//! - **写不进不算失败**：这是给人回看的副本，状态机与检查点不依赖它；调用方 warn 一句继续。

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::status::AgoraEvent;

/// 每个会话交出去的最多轮数，也是压缩后留下的轮数。
pub const KEEP: usize = 20;

/// 文件里的条目数到这个数就压缩：一轮通常两条（人话 + 回复），留出一倍余量，压缩不至于每轮都发生。
pub const COMPACT_AT: usize = KEEP * 4;

/// 单条文本（人话或回复）落盘的上限，字节。长回复截断后尾部注明，不静默丢。
pub const TEXT_MAX: usize = 16 * 1024;

const TRUNCATED_NOTE: &str = "\n…（超过 16 KB 的部分没有保存）";

/// 文件里的一行。`at` 是事件自己的毫秒时刻。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// 人话：开一轮。
    Prompt { at: u64, text: String },
    /// 宿主自己注入的 prompt（agora-3s5）：一轮照样开始，但没有人话。
    Injected { at: u64 },
    /// 最终回复：合上当前轮；没有开着的轮（日志在一轮中间才开始记）就自成半轮。
    Reply {
        at: u64,
        #[serde(default)]
        text: Option<String>,
    },
    /// 以错误 / 中断结束：合上当前轮。
    Failed { at: u64, reason: String },
}

impl Entry {
    fn at(&self) -> u64 {
        match self {
            Entry::Prompt { at, .. }
            | Entry::Injected { at }
            | Entry::Reply { at, .. }
            | Entry::Failed { at, .. } => *at,
        }
    }

    /// hook 事件 → 条目；与轮无关的事件（工具活动、权限、会话起止）不记。
    pub fn from_event(event: &AgoraEvent, at: u64) -> Option<Entry> {
        match event {
            AgoraEvent::PromptSubmitted(p) => Some(Entry::Prompt { at, text: cap(p) }),
            AgoraEvent::PromptInjected => Some(Entry::Injected { at }),
            AgoraEvent::TurnEnded(reply) => Some(Entry::Reply {
                at,
                text: reply.as_deref().map(cap),
            }),
            AgoraEvent::TurnFailed(reason) => Some(Entry::Failed {
                at,
                reason: reason.clone(),
            }),
            _ => None,
        }
    }
}

/// 一轮怎么收尾的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 最新的一轮，还没收到结束。
    Open,
    /// 收到了最终回复（回复文本可能为空：宿主没带）。
    Done,
    /// 以错误 / 中断结束。
    Failed,
    /// 下一轮已经开始，这一轮的结束没记下来（pi 排队交付的中间轮，agora-cdjb.1 之前就是这样）。
    NoReply,
}

/// `GET /api/sessions/:id/turns` 里的一轮（docs/spec/api.md「轮次」）。时刻是 unix 秒。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Turn {
    /// 人说的话；宿主注入的轮与「日志从一轮中间开始」的半轮为 null。
    pub prompt: Option<String>,
    /// 这一轮是宿主注入的 prompt 开的（不是人说的）。
    pub injected: bool,
    pub reply: Option<String>,
    pub outcome: Outcome,
    /// `outcome = failed` 时的错误类型。
    pub failure: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

fn secs(ms: u64) -> i64 {
    i64::try_from(ms / 1000).unwrap_or(i64::MAX)
}

/// 截在字符边界：`TEXT_MAX` 落在一个多字节字符中间时往前退到边界。
pub fn cap(text: &str) -> String {
    if text.len() <= TEXT_MAX {
        return text.to_owned();
    }
    let mut end = TEXT_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED_NOTE}", &text[..end])
}

/// 条目 → 轮，连同每一轮起始条目的下标（压缩要按轮切）。
fn fold(entries: &[Entry]) -> (Vec<Turn>, Vec<usize>) {
    let mut turns: Vec<Turn> = Vec::new();
    let mut starts = Vec::new();
    let open = |turns: &Vec<Turn>| turns.last().is_some_and(|t| t.outcome == Outcome::Open);
    for (i, e) in entries.iter().enumerate() {
        match e {
            Entry::Prompt { .. } | Entry::Injected { .. } => {
                if let Some(prev) = turns.last_mut().filter(|t| t.outcome == Outcome::Open) {
                    prev.outcome = Outcome::NoReply;
                }
                let (prompt, injected) = match e {
                    Entry::Prompt { text, .. } => (Some(text.clone()), false),
                    _ => (None, true),
                };
                turns.push(Turn {
                    prompt,
                    injected,
                    reply: None,
                    outcome: Outcome::Open,
                    failure: None,
                    started_at: Some(secs(e.at())),
                    ended_at: None,
                });
                starts.push(i);
            }
            Entry::Reply { at, text } => {
                if open(&turns) {
                    let t = turns.last_mut().expect("open() 看过有最后一轮");
                    t.reply = text.clone();
                    t.outcome = Outcome::Done;
                    t.ended_at = Some(secs(*at));
                } else {
                    turns.push(Turn {
                        prompt: None,
                        injected: false,
                        reply: text.clone(),
                        outcome: Outcome::Done,
                        failure: None,
                        started_at: None,
                        ended_at: Some(secs(*at)),
                    });
                    starts.push(i);
                }
            }
            // 没有开着的轮时的失败无处可挂（比如回复之后又报了一次错）：留在文件里，不成轮。
            Entry::Failed { at, reason } => {
                if open(&turns) {
                    let t = turns.last_mut().expect("open() 看过有最后一轮");
                    t.outcome = Outcome::Failed;
                    t.failure = Some(reason.clone());
                    t.ended_at = Some(secs(*at));
                }
            }
        }
    }
    (turns, starts)
}

fn names(id: &str) -> [String; 2] {
    let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    [format!("{key}.jsonl"), format!("{key}.part")]
}

fn path(dir: &Path, id: &str) -> PathBuf {
    dir.join(&names(id)[0])
}

/// 读出全部条目；坏行跳过（追加写崩在半行时留下的尾巴，不该让整份日志读不出来）。
fn read_entries(dir: &Path, id: &str) -> io::Result<Vec<Entry>> {
    let file = match fs::File::open(path(dir, id)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut entries = Vec::new();
    for line in io::BufReader::new(file).lines() {
        let line = line?;
        if let Ok(e) = serde_json::from_str::<Entry>(&line) {
            entries.push(e);
        }
    }
    Ok(entries)
}

/// 最近 `limit` 轮（≤ [`KEEP`]），按时间先后。
pub fn load(dir: &Path, id: &str, limit: usize) -> io::Result<Vec<Turn>> {
    let (turns, _) = fold(&read_entries(dir, id)?);
    let take = limit.min(KEEP);
    Ok(turns[turns.len().saturating_sub(take)..].to_vec())
}

/// 追加一条；返回是否真的写了（重放来的旧条目返回 false）。
pub fn append(dir: &Path, id: &str, entry: &Entry) -> io::Result<bool> {
    let entries = read_entries(dir, id)?;
    if let Some(last) = entries.last() {
        if entry.at() < last.at() || entry == last {
            return Ok(false);
        }
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
    line.push(b'\n');
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path(dir, id))?;
    f.write_all(&line)?;
    if entries.len() + 1 >= COMPACT_AT {
        let mut all = entries;
        all.push(entry.clone());
        compact(dir, id, &all)?;
    }
    Ok(true)
}

/// 只留最近 [`KEEP`] 轮的条目，先写 `.part` 再原子替换。
fn compact(dir: &Path, id: &str, entries: &[Entry]) -> io::Result<()> {
    let (turns, starts) = fold(entries);
    let from = if turns.len() > KEEP {
        starts[turns.len() - KEEP]
    } else {
        0
    };
    let mut bytes = Vec::new();
    for e in &entries[from..] {
        serde_json::to_writer(&mut bytes, e).map_err(io::Error::other)?;
        bytes.push(b'\n');
    }
    let part = dir.join(&names(id)[1]);
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&part)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    fs::rename(part, path(dir, id))
}

pub fn remove(dir: &Path, id: &str) -> io::Result<()> {
    for name in names(id) {
        match fs::remove_file(dir.join(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// 删掉 `dir` 里不属于这些 id 的文件，返回删掉的文件名（排序过）。判据与 hook 检查点同一条：
/// 正向算出该留的名字，不从文件名反解 id。
pub fn prune_orphans(dir: &Path, ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let keep: HashSet<String> = ids.into_iter().flat_map(|id| names(&id)).collect();
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) || !entry.path().is_file() {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed.push(name),
            Err(err) => {
                tracing::warn!(component = "session", file = %entry.path().display(), %err, "删无行的轮次日志失败")
            }
        }
    }
    removed.sort();
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(at: u64, text: &str) -> Entry {
        Entry::Prompt {
            at,
            text: text.into(),
        }
    }

    fn reply(at: u64, text: &str) -> Entry {
        Entry::Reply {
            at,
            text: Some(text.into()),
        }
    }

    #[test]
    fn folds_prompts_and_replies_into_turns() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for e in [
            reply(1_000, "日志开始前那一轮的回复"),
            prompt(2_000, "第一句"),
            reply(3_000, "第一句的回复"),
            Entry::Injected { at: 4_000 },
            reply(5_000, "后台任务做完了"),
            prompt(6_000, "排队的第一条"),
            prompt(7_000, "排队的第二条"),
            Entry::Failed {
                at: 8_000,
                reason: "api_error".into(),
            },
            prompt(9_000, "还在跑的这一句"),
        ] {
            assert!(append(d, "abc123", &e).unwrap());
        }
        let turns = load(d, "abc123", KEEP).unwrap();
        let shape: Vec<_> = turns
            .iter()
            .map(|t| {
                (
                    t.prompt.as_deref(),
                    t.injected,
                    t.reply.as_deref(),
                    t.outcome,
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (None, false, Some("日志开始前那一轮的回复"), Outcome::Done),
                (Some("第一句"), false, Some("第一句的回复"), Outcome::Done),
                (None, true, Some("后台任务做完了"), Outcome::Done),
                (Some("排队的第一条"), false, None, Outcome::NoReply),
                (Some("排队的第二条"), false, None, Outcome::Failed),
                (Some("还在跑的这一句"), false, None, Outcome::Open),
            ]
        );
        assert_eq!(turns[1].started_at, Some(2));
        assert_eq!(turns[1].ended_at, Some(3));
        assert_eq!(turns[4].failure.as_deref(), Some("api_error"));
        assert_eq!(load(d, "abc123", 2).unwrap(), turns[4..].to_vec());
    }

    #[test]
    fn replayed_events_are_not_recorded_twice() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let events = [prompt(1_000, "a"), reply(2_000, "b"), prompt(3_000, "c")];
        for e in &events {
            assert!(append(d, "x", e).unwrap());
        }
        // 启动重放 / 归档重建把同一串事件再走一遍。
        for e in &events {
            assert!(!append(d, "x", e).unwrap());
        }
        // 同一毫秒里不同的条目照记（一封投递件里的两个事件）。
        assert!(append(d, "x", &reply(3_000, "d")).unwrap());
        let turns = load(d, "x", KEEP).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].reply.as_deref(), Some("d"));
    }

    #[test]
    fn keeps_the_newest_turns_and_compacts_without_changing_them() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let total = KEEP * 3;
        for i in 0..total as u64 {
            append(d, "s", &prompt(i * 10, &format!("p{i}"))).unwrap();
            append(d, "s", &reply(i * 10 + 1, &format!("r{i}"))).unwrap();
        }
        let turns = load(d, "s", usize::MAX).unwrap();
        assert_eq!(turns.len(), KEEP);
        assert_eq!(
            turns[0].prompt.as_deref(),
            Some(&*format!("p{}", total - KEEP))
        );
        assert_eq!(
            turns[KEEP - 1].reply.as_deref(),
            Some(&*format!("r{}", total - 1))
        );
        let lines = fs::read_to_string(path(d, "s")).unwrap().lines().count();
        assert!(lines < COMPACT_AT, "压缩过：{lines} 行");
        assert!(!d.join(&names("s")[1]).exists(), "不留 .part");
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary_and_says_so() {
        let long = "汉".repeat(TEXT_MAX); // 每个 3 字节
        let capped = cap(&long);
        assert!(capped.ends_with(TRUNCATED_NOTE));
        let body = capped.strip_suffix(TRUNCATED_NOTE).unwrap();
        assert!(body.len() <= TEXT_MAX && body.chars().all(|c| c == '汉'));
        assert_eq!(cap("短"), "短");
    }

    #[test]
    fn files_are_private_and_removed_with_the_row() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let d = home.path().join("turns");
        append(&d, "keep", &prompt(1, "a")).unwrap();
        append(&d, "gone", &prompt(1, "b")).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&d), 0o700);
        assert_eq!(mode(&path(&d, "keep")), 0o600);
        remove(&d, "gone").unwrap();
        remove(&d, "never-existed").unwrap();
        assert!(load(&d, "gone", KEEP).unwrap().is_empty());
        fs::write(d.join("stray.jsonl"), "{}").unwrap();
        assert_eq!(prune_orphans(&d, ["keep".to_owned()]), vec!["stray.jsonl"]);
        assert_eq!(load(&d, "keep", KEEP).unwrap().len(), 1);
    }

    #[test]
    fn a_torn_last_line_does_not_hide_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        append(d, "t", &prompt(1, "a")).unwrap();
        let mut f = OpenOptions::new().append(true).open(path(d, "t")).unwrap();
        f.write_all(b"{\"kind\":\"reply\",\"at\":2,\"te").unwrap();
        assert_eq!(load(d, "t", KEEP).unwrap().len(), 1);
    }
}
