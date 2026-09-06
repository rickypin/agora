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

use crate::runtime::exec::{exec, ExecError, ExecOptions};

/// 允许对 beads 执行的子命令。改这里等于改不变量 12，先改 MISSION。
///
/// `show`：会话行的任务标签（§6.3）；`ready`：New Agent 对话框从就绪任务起会话（§6.4，A43，
/// agora-h1k.2）。两个都只读；claim 是 agent 开工的纪律（AGENTS.md），agora 不替它做——
/// `bd ready` 的 `--claim` 会认领第一条，这个字面量在 src/task 里被 `tests/arch_boundary.rs`
/// 禁掉。
pub const READ_ONLY: &[&str] = &["show", "ready"];

/// `bd show --json` 里我们用的那几个字段。只在内存缓存里活着，随 `TaskIndex::ttl` 过期重查；
/// 一个字都不进 SQLite（不变量 12；`tests/task_info.rs::acceptance_is_read_not_stored`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub title: String,
    /// bd 的 P0–P4。
    pub priority: u8,
    pub status: String,
    /// `acceptance_criteria` 全文——人看一行时要知道"做完算什么"（MISSION §6.3 看结果；A40；
    /// agora-h1k.3）。beads 里没写或只有空白 → None，前端不占位。
    pub acceptance: Option<String>,
}

/// `bd ready --json` 里一条可以起会话的任务（MISSION §6.4 从就绪任务起会话；A43）。
/// 与 [`TaskInfo`] 一样只在响应里活着，一个字不进 SQLite（不变量 12）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadyTask {
    pub id: String,
    pub title: String,
    /// bd 的 P0–P4。
    pub priority: u8,
    /// bd 的 `issue_type`（task / bug / feature / chore…）；epic 在解析时就被滤掉——阶段不是
    /// 可起会话的任务。
    #[serde(rename = "type")]
    pub issue_type: String,
}

/// `bd ready` 为什么没给出列表——按类型分（MISSION §2.3 规则 10），前端按它给文案。
/// 都不是错误：没装 bd、目录没有 beads 的仓库照样能起会话，Task 退回一句话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyError {
    /// 命令不存在（没装 bd，或配置的命令路径不对）。
    NoBd,
    /// bd 退出非零：目录没有 beads 库，或 dolt 报错。
    NoBeads,
    /// 超过 [`TaskIndex`] 的超时（10 s；embedded dolt 冷启动慢）。
    Timeout,
    /// 退出 0 但 stdout 不是 JSON 数组。
    BadOutput,
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

    /// 在 `cwd` 里跑 `bd ready --json`，给 New Agent 对话框列可选的就绪任务（`GET
    /// /api/projects/tasks`）。同步、不缓存：对话框打开一次拉一次，用户就等这一下。
    /// 调用方在 blocking 线程。
    ///
    /// 只有 `ready --json` 两个参数：**没有 `--claim`**——那会把第一条认领到当前用户名下，
    /// 而 claim 是 agent 自己开工时做的事（MISSION §6.4；`tests/task_pick.rs` 用假 bd 录下 argv
    /// 核对）。
    pub fn ready(&self, cwd: &Path) -> Result<Vec<ReadyTask>, ReadyError> {
        let sub = READ_ONLY[1];
        let opts = ExecOptions {
            timeout: Some(self.timeout),
            cwd: Some(cwd.to_path_buf()),
            ..ExecOptions::default()
        };
        let out = match exec(&[self.command.as_str(), sub, "--json"], &opts) {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                tracing::debug!(component = "task", cwd = %cwd.display(), stderr = %String::from_utf8_lossy(&o.stderr_tail).trim(), "bd ready 失败");
                return Err(ReadyError::NoBeads);
            }
            Err(ExecError::Timeout { .. }) => return Err(ReadyError::Timeout),
            Err(err) if err.is_not_found() => {
                tracing::debug!(component = "task", %err, "bd 不可用");
                return Err(ReadyError::NoBd);
            }
            Err(err) => {
                // 起不来（权限、不是可执行文件）与读输出失败：对用户来说都是"这台机器上的 bd
                // 用不了"，与没装同一档。
                tracing::debug!(component = "task", %err, "bd ready 起不来");
                return Err(ReadyError::NoBd);
            }
        };
        parse_ready(&out.stdout).ok_or(ReadyError::BadOutput)
    }
}

/// `bd ready --json` 是一个数组；epic 滤掉，其余按 bd 给的顺序。缺 id 或 title 的条目跳过。
fn parse_ready(stdout: &[u8]) -> Option<Vec<ReadyTask>> {
    let v: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let items = v.as_array()?;
    Some(
        items
            .iter()
            .filter_map(|item| {
                let issue_type = item
                    .get("issue_type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("task");
                if issue_type == "epic" {
                    return None;
                }
                Some(ReadyTask {
                    id: item.get("id")?.as_str()?.to_owned(),
                    title: item.get("title")?.as_str()?.to_owned(),
                    priority: item
                        .get("priority")
                        .and_then(|p| p.as_u64())
                        .map(|p| p.min(4) as u8)
                        .unwrap_or(2),
                    issue_type: issue_type.to_owned(),
                })
            })
            .collect(),
    )
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
        acceptance: item
            .get("acceptance_criteria")
            .and_then(|a| a.as_str())
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned),
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
        // 字节串字面量不许非 ASCII：先写成 str 再取字节。
        let body = r#"[{"id":"x-1","title":"one","priority":9,"status":"open","acceptance_criteria":"  "},{"id":"x-2","title":"two","priority":1,"status":"closed","acceptance_criteria":"tests/x.rs::guard 绿\n第二行"}]"#.as_bytes();
        let two = parse_show(body, "x-2").unwrap();
        assert_eq!(
            (two.title.as_str(), two.priority, two.status.as_str()),
            ("two", 1, "closed")
        );
        assert_eq!(
            two.acceptance.as_deref(),
            Some("tests/x.rs::guard 绿\n第二行"),
            "验收标准全文原样，多行保留"
        );
        let one = parse_show(body, "x-1").unwrap();
        assert_eq!(one.priority, 4);
        assert_eq!(one.acceptance, None, "只有空白 = 没写");
        assert!(parse_show(b"Error: no beads database found", "x-1").is_none());
        assert!(parse_show(br#"{"error":"nope"}"#, "x-1").is_none());
    }

    #[test]
    fn parse_ready_drops_epics_and_keeps_bd_order() {
        let body = r#"[{"id":"x-e","title":"M9: 阶段","priority":1,"issue_type":"epic","status":"open"},{"id":"x-2","title":"two","priority":3,"issue_type":"task","status":"open"},{"id":"x-1","title":"one","priority":9,"issue_type":"bug","status":"open"},{"title":"没有 id"}]"#.as_bytes();
        let tasks = parse_ready(body).unwrap();
        assert_eq!(
            tasks
                .iter()
                .map(|t| (t.id.as_str(), t.issue_type.as_str(), t.priority))
                .collect::<Vec<_>>(),
            vec![("x-2", "task", 3), ("x-1", "bug", 4)],
            "epic 滤掉、缺 id 的跳过、顺序照 bd 给的、优先级夹到 4"
        );
        assert_eq!(parse_ready(b"[]").unwrap(), vec![]);
        assert!(parse_ready(b"Error: no beads database found").is_none());
        assert!(parse_ready(br#"{"error":"nope"}"#).is_none(), "不是数组");
        assert_eq!(
            serde_json::to_value(&tasks[0]).unwrap(),
            serde_json::json!({"id":"x-2","title":"two","priority":3,"type":"task"}),
            "线上的键叫 type"
        );
        assert_eq!(
            serde_json::to_value(ReadyError::NoBd).unwrap(),
            serde_json::json!("no_bd")
        );
    }
}
