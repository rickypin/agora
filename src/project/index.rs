//! 会话行上的项目身份（agora-uvd.1，A49 数据半边）。
//!
//! 侧栏要按「节点 → 仓库 → worktree」分组，行上要显示「仓库 · 分支」。这些不能靠前端
//! 拼 `/api/projects`：目录可能不在 `project_roots`、可能在 peer 上、分支会变。按会话的
//! `working_directory` 在服务端现算一次，与 [`crate::task::TaskIndex`] 同一套机制——内存缓存、
//! 异步补齐、TTL 重查、**不落库**（不变量 7：库里只有 `working_directory`）。
//!
//! 本模块只读 git，不碰 [`super::Projects`] 的库表，也不判断「已知项目」：会话在哪个目录
//! 就算哪个。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::runtime::exec::{exec, ExecOptions};

/// 会话所在 git 仓库 / worktree / 分支。只在内存缓存里活着，随 [`ProjectIndex::ttl`] 过期重查。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectInfo {
    /// 主 worktree 的绝对路径（= git common dir 的父目录）。
    pub repo: String,
    /// `repo` 的最后一段目录名。
    pub name: String,
    /// 该会话所在 worktree 的根（`git rev-parse --show-toplevel`）。
    pub worktree: String,
    /// `git symbolic-ref --short HEAD`；detached HEAD 为 None。
    pub branch: Option<String>,
    /// `worktree == repo`。
    pub main: bool,
}

#[derive(Debug, Clone)]
enum Entry {
    Pending(Instant),
    Done(Instant, Option<ProjectInfo>),
}

pub struct ProjectIndex {
    /// 键是会话的 working_directory。同一 worktree 下两个子目录会各查一次，结果里的
    /// `worktree` / `repo` 相同；按 cwd 分开是因为 list 每 tick 用的就是那条路径。
    cache: Mutex<HashMap<PathBuf, Entry>>,
    ttl: Duration,
    timeout: Duration,
    /// 测试用：同步查，不起线程。
    sync: bool,
}

impl Default for ProjectIndex {
    fn default() -> Self {
        ProjectIndex {
            cache: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(60),
            timeout: Duration::from_secs(3),
            sync: false,
        }
    }
}

impl ProjectIndex {
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 同步模式：`get` 缺失时就地查完再返回（测试用）。
    pub fn synchronous(mut self) -> Self {
        self.sync = true;
        self
    }

    /// 缓存里的答案；缺失或过期时触发一次查询（异步模式下本次返回 None）。
    pub fn get(self: &Arc<Self>, cwd: &Path) -> Option<ProjectInfo> {
        let key = cwd.to_path_buf();
        let now = Instant::now();
        {
            let mut cache = lock(&self.cache);
            match cache.get(&key) {
                Some(Entry::Done(at, info)) if now.duration_since(*at) < self.ttl => {
                    return info.clone();
                }
                Some(Entry::Pending(at)) if now.duration_since(*at) < self.timeout * 2 => {
                    return None;
                }
                _ => {}
            }
            cache.insert(key.clone(), Entry::Pending(now));
        }
        if self.sync {
            let info = self.fetch(&key);
            lock(&self.cache).insert(key, Entry::Done(Instant::now(), info.clone()));
            return info;
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name("agora-project-lookup".into())
            .spawn(move || {
                let info = me.fetch(&key);
                lock(&me.cache).insert(key, Entry::Done(Instant::now(), info));
            })
            .ok();
        None
    }

    /// 三条只读 git：show-toplevel / git-common-dir / symbolic-ref。任何一步超时、目录不存在、
    /// 不是仓库、git 不可用 → None。detached HEAD 只让 `branch` 为 None，不算失败。
    pub fn fetch(&self, cwd: &Path) -> Option<ProjectInfo> {
        let opts = ExecOptions {
            timeout: Some(self.timeout),
            cwd: Some(cwd.to_path_buf()),
            ..ExecOptions::default()
        };
        let worktree = git_ok(&["git", "rev-parse", "--show-toplevel"], &opts)?;
        let common = match git_ok(
            &[
                "git",
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ],
            &opts,
        ) {
            Some(p) => p,
            None => git_ok(&["git", "rev-parse", "--git-common-dir"], &opts)?,
        };
        let worktree = abs_path(cwd, &worktree);
        let git_dir = abs_path(cwd, &common);
        let repo = git_dir.parent()?.to_path_buf();
        // macOS 上 /tmp 与 /private/tmp 是同一处：不 canonicalize 的话 main 会判错。
        let worktree = canonicalize_or(worktree);
        let repo = canonicalize_or(repo);
        let name = repo.file_name()?.to_string_lossy().into_owned();
        let branch = match exec(&["git", "symbolic-ref", "--short", "-q", "HEAD"], &opts) {
            Ok(o) if o.status.success() => stdout_line(&o),
            // 退出码 1 + `-q`：detached HEAD，不算失败。
            Ok(_) => None,
            Err(err) => {
                tracing::debug!(component = "project", cwd = %cwd.display(), %err, "symbolic-ref 失败");
                None
            }
        };
        Some(ProjectInfo {
            main: worktree == repo,
            repo: repo.to_string_lossy().into_owned(),
            name,
            worktree: worktree.to_string_lossy().into_owned(),
            branch,
        })
    }
}

fn git_ok(argv: &[&str], opts: &ExecOptions) -> Option<String> {
    match exec(argv, opts) {
        Ok(o) if o.status.success() => stdout_line(&o),
        Ok(o) => {
            tracing::debug!(
                component = "project",
                argv = ?argv,
                status = ?o.status,
                stderr = %String::from_utf8_lossy(&o.stderr_tail).trim(),
                "git 非零退出"
            );
            None
        }
        Err(err) => {
            tracing::debug!(component = "project", argv = ?argv, %err, "git 不可用");
            None
        }
    }
}

fn stdout_line(out: &crate::runtime::exec::Output) -> Option<String> {
    let s = String::from_utf8_lossy(&out.stdout);
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}

fn abs_path(cwd: &Path, p: &str) -> PathBuf {
    let p = PathBuf::from(p);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn canonicalize_or(p: PathBuf) -> PathBuf {
    p.canonicalize().unwrap_or(p)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
