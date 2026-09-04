//! 任务标签层（MISSION §6.3 L2"做到标签层"；ADR-002 D8；agora-dvh.10）。
//!
//! 每个会话关联到"它在做的那件事"：`sessions.task_ref` 存 issue id 或首条 prompt 的摘要。
//! 像 beads id 的 `task_ref` 在会话的工作目录里跑 `bd show <id> --json` 取标题与优先级，
//! 给 Dashboard 的第一列与同分排序用。**agora 对 beads 零写入**（不变量 12）：这里是全仓
//! 唯一允许起 `bd` 子进程的地方（`tests/arch_boundary.rs`），而且只有 [`READ_ONLY`] 里的
//! 子命令（`tests/task_beads.rs` 用假 `bd` 录下每一次调用核对）。
//!
//! 查询是异步补齐的：`list()` 每 2 s 跑一次，不能在里面同步等 Dolt（`bd show` 实测 0.4 s，
//! 2026-09-04）；缓存缺失时记一个占位、起线程去查，本轮先没有标签，下一轮就有了。结果
//! （含"没有 beads"/"没这个 issue"的否定答案）按 [`TaskIndex::ttl`] 缓存，免得每个 tick
//! 都去敲 Dolt。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::runtime::exec::{exec, ExecOptions};

/// 允许对 beads 执行的子命令。改这里等于改不变量 12，先改 MISSION。
pub const READ_ONLY: &[&str] = &["show"];

/// `bd show --json` 里我们用的那几个字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub title: String,
    /// bd 的 P0–P4。
    pub priority: u8,
    pub status: String,
}

#[derive(Debug, Clone)]
enum Entry {
    Pending(Instant),
    Done(Instant, Option<TaskInfo>),
}

pub struct TaskIndex {
    /// 键：(工作目录, task_ref)。同一 issue 在两个 worktree 里是同一个 issue，但 beads 库
    /// 跟仓库走，仍按目录分开查——不同目录可能是不同项目的同名前缀。
    cache: Mutex<HashMap<(PathBuf, String), Entry>>,
    command: String,
    ttl: Duration,
    timeout: Duration,
    /// 测试用：同步查，不起线程。
    sync: bool,
}

impl Default for TaskIndex {
    fn default() -> Self {
        TaskIndex::new("bd")
    }
}

impl TaskIndex {
    pub fn new(command: &str) -> Self {
        TaskIndex {
            cache: Mutex::new(HashMap::new()),
            command: command.to_owned(),
            ttl: Duration::from_secs(300),
            timeout: Duration::from_secs(10),
            sync: false,
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// 同步模式：`get` 缺失时就地查完再返回（测试与 CLI 用）。
    pub fn synchronous(mut self) -> Self {
        self.sync = true;
        self
    }

    /// 像不像 beads 的 issue id：`<prefix>-<hash>` 加可选的 `.n` 层级（`agora-dvh.10`）。
    /// 不像的（首条 prompt 摘要、随手写的一句话）不去敲 bd。
    pub fn looks_like_issue_id(s: &str) -> bool {
        let s = s.trim();
        if s.len() > 64 || s.contains(char::is_whitespace) {
            return false;
        }
        let Some((prefix, rest)) = s.split_once('-') else {
            return false;
        };
        let ok_ident =
            |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok_ident(prefix) {
            return false;
        }
        let mut parts = rest.split('.');
        let Some(hash) = parts.next() else {
            return false;
        };
        ok_ident(hash) && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    }

    /// 缓存里的答案；缺失或过期时触发一次查询（异步模式下本次返回 None）。
    pub fn get(self: &Arc<Self>, cwd: &Path, task_ref: &str) -> Option<TaskInfo> {
        if !Self::looks_like_issue_id(task_ref) {
            return None;
        }
        let key = (cwd.to_path_buf(), task_ref.trim().to_owned());
        let now = Instant::now();
        {
            let mut cache = lock(&self.cache);
            match cache.get(&key) {
                Some(Entry::Done(at, info)) if now.duration_since(*at) < self.ttl => {
                    return info.clone();
                }
                // 查询线程卡住（Dolt 假死）也不能让 list 每轮都再起一个：占位到 timeout 后才重试。
                Some(Entry::Pending(at)) if now.duration_since(*at) < self.timeout * 2 => {
                    return None;
                }
                _ => {}
            }
            cache.insert(key.clone(), Entry::Pending(now));
        }
        if self.sync {
            let info = self.fetch(&key.0, &key.1);
            lock(&self.cache).insert(key, Entry::Done(Instant::now(), info.clone()));
            return info;
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name("agora-task-lookup".into())
            .spawn(move || {
                let info = me.fetch(&key.0, &key.1);
                lock(&me.cache).insert(key, Entry::Done(Instant::now(), info));
            })
            .ok();
        None
    }

    /// 起 `bd show <id> --json`；任何失败（没装 bd、目录没有 beads、没这个 issue、输出不是
    /// JSON）都是 None——标签是锦上添花，缺了退回摘要 / 名字，不报错。
    pub fn fetch(&self, cwd: &Path, id: &str) -> Option<TaskInfo> {
        let sub = READ_ONLY[0];
        let opts = ExecOptions {
            timeout: Some(self.timeout),
            cwd: Some(cwd.to_path_buf()),
            ..ExecOptions::default()
        };
        let out = match exec(&[self.command.as_str(), sub, id, "--json"], &opts) {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                tracing::debug!(component = "task", id, cwd = %cwd.display(), stderr = %String::from_utf8_lossy(&o.stderr_tail).trim(), "bd show 失败");
                return None;
            }
            Err(err) => {
                tracing::debug!(component = "task", id, %err, "bd 不可用");
                return None;
            }
        };
        parse_show(&out.stdout, id)
    }
}

/// `bd show --json` 是一个数组（可以一次问多个 id）；取 id 相同的那条。
fn parse_show(stdout: &[u8], id: &str) -> Option<TaskInfo> {
    let v: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        o @ serde_json::Value::Object(_) => vec![o],
        _ => return None,
    };
    let item = items
        .iter()
        .find(|i| i.get("id").and_then(|x| x.as_str()) == Some(id))
        .or_else(|| items.first())?;
    Some(TaskInfo {
        id: item.get("id")?.as_str()?.to_owned(),
        title: item.get("title")?.as_str()?.to_owned(),
        priority: item
            .get("priority")
            .and_then(|p| p.as_u64())
            .map(|p| p.min(4) as u8)
            .unwrap_or(2),
        status: item
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_owned(),
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_id_shape() {
        for ok in [
            "agora-dvh",
            "agora-dvh.10",
            "agora-7ku.2",
            "beads-xyz1.3.4",
            "a_b-c1",
        ] {
            assert!(TaskIndex::looks_like_issue_id(ok), "{ok}");
        }
        for no in [
            "",
            "把 sidebar 换掉",
            "agora",
            "agora-",
            "-dvh",
            "agora-dvh.",
            "agora-dvh.x",
            "fix the bug-now please",
        ] {
            assert!(!TaskIndex::looks_like_issue_id(no), "{no}");
        }
    }

    #[test]
    fn parse_show_picks_the_matching_item_and_clamps_priority() {
        let body = br#"[{"id":"x-1","title":"one","priority":9,"status":"open"},{"id":"x-2","title":"two","priority":1,"status":"closed"}]"#;
        let two = parse_show(body, "x-2").unwrap();
        assert_eq!(
            (two.title.as_str(), two.priority, two.status.as_str()),
            ("two", 1, "closed")
        );
        assert_eq!(parse_show(body, "x-1").unwrap().priority, 4);
        assert!(parse_show(b"Error: no beads database found", "x-1").is_none());
        assert!(parse_show(br#"{"error":"nope"}"#, "x-1").is_none());
    }
}
