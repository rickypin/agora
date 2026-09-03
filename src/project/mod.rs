//! 项目列表与 worktree（MISSION §6.4；`GET /api/projects` 与 `/api/projects/worktrees`）。
//!
//! 项目列表**不靠手写配置**：扫描 `project_roots` 下的 git 仓库，与库里的"最近使用"
//! 合并后按最近使用排序。上百个仓库的手写列表会立刻过期，目标是常用项目 2–3 次操作起会话。
//!
//! 新建 worktree（`git worktree add`）归 M3 A44，这里只列现有的——只读、不写工作区
//! （MISSION §1.4 的 Git GUI 边界）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::params;
use serde::Serialize;

use crate::runtime::exec::{self, ExecError, ExecOptions};
use crate::session::{Db, DbError};

/// 扫描深度：`project_roots` 的直接子目录，仓库不在那一层时再下探一层
/// （`~/code/<org>/<repo>` 这种放法很常见）。命中即不再下探——仓库里的
/// `vendor/` 之类不是项目。
pub const MAX_DEPTH: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// worktree 查询只接受已知项目：否则这个端点就成了任意目录的探测器。
    #[error("不是已知项目: {0}")]
    Unknown(String),
    #[error("git 失败: {0}")]
    Git(String),
    #[error(transparent)]
    Exec(#[from] ExecError),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for ProjectError {
    fn from(e: rusqlite::Error) -> Self {
        ProjectError::Db(DbError::Sql(e))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    pub path: String,
    pub name: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Worktree {
    pub path: String,
    /// 去掉 `refs/heads/` 前缀；detached HEAD 时为 None。
    pub branch: Option<String>,
    pub head: Option<String>,
    /// `git worktree list` 的第一条是主 worktree。
    pub main: bool,
    pub locked: bool,
}

/// 全部方法**同步阻塞**（扫目录、起 git 子进程），调用方在 `spawn_blocking` 里跑
/// ——与 `Runtime` / `SessionManager` 同一个并发模型（ADR-001 D8）。
pub struct Projects {
    db: Arc<Db>,
    roots: Vec<PathBuf>,
}

impl Projects {
    pub fn new(db: Arc<Db>, roots: Vec<PathBuf>) -> Self {
        Projects { db, roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// 扫描 + 合并 + 排序。每次调用都重扫：新 clone 的仓库不该等到重启 daemon 才出现，
    /// 而扫描是几十次 `read_dir`，比"缓存 + 失效"简单得多。
    pub fn list(&self) -> Result<Vec<Project>, ProjectError> {
        let found = self.scan();
        let conn = self.db.conn();
        for (path, name) in &found {
            // 已有行只补名字，绝不碰 last_used_at——重扫不是使用。
            conn.execute(
                "INSERT INTO projects (path, name, last_used_at) VALUES (?1, ?2, NULL)
                 ON CONFLICT(path) DO UPDATE SET name = excluded.name",
                params![path, name],
            )?;
        }
        let mut stmt = conn.prepare(
            "SELECT path, name, last_used_at FROM projects
             ORDER BY last_used_at IS NULL, last_used_at DESC, name",
        )?;
        let rows: Vec<Project> = stmt
            .query_map([], |r| {
                Ok(Project {
                    path: r.get(0)?,
                    name: r.get(1)?,
                    last_used_at: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        // 目录没了就从表里删：仓库删掉之后还留在下拉里，选中它只会得到一个起不来的会话。
        let (alive, gone): (Vec<Project>, Vec<Project>) =
            rows.into_iter().partition(|p| Path::new(&p.path).is_dir());
        for p in &gone {
            conn.execute("DELETE FROM projects WHERE path = ?1", params![p.path])?;
        }
        Ok(alive)
    }

    /// 起会话时记一次"刚用过"，这是列表排序的唯一来源。
    ///
    /// 表里没有的路径只在它自己是 git 仓库时才插入：worktree 通常在 `project_roots`
    /// 之外（`../<repo>-wt/<name>`），用过一次之后应该能在列表里直接选到；而随手指到
    /// `/tmp` 的会话不该把 `/tmp` 变成一个项目。
    pub fn touch(&self, path: &Path) -> Result<(), ProjectError> {
        let path_str = path.to_string_lossy().into_owned();
        let name = dir_name(path);
        let conn = self.db.conn();
        let updated = conn.execute(
            "UPDATE projects SET last_used_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE path = ?1",
            params![path_str],
        )?;
        if updated == 0 && is_repo(path) {
            conn.execute(
                "INSERT INTO projects (path, name, last_used_at)
                 VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ','now'))
                 ON CONFLICT(path) DO UPDATE SET last_used_at = excluded.last_used_at",
                params![path_str, name],
            )?;
        }
        Ok(())
    }

    /// 该仓库现有的 worktree（含主 worktree）。
    pub fn worktrees(&self, repo: &Path) -> Result<Vec<Worktree>, ProjectError> {
        if !self.is_known(repo) {
            return Err(ProjectError::Unknown(repo.to_string_lossy().into_owned()));
        }
        let repo_arg = repo.to_string_lossy().into_owned();
        let argv = ["git", "-C", &repo_arg, "worktree", "list", "--porcelain"].map(str::to_owned);
        let out = exec::exec(&argv, &ExecOptions::default())?;
        if !out.status.success() {
            return Err(ProjectError::Git(
                String::from_utf8_lossy(&out.stderr_tail).trim().to_owned(),
            ));
        }
        Ok(parse_worktrees(&String::from_utf8_lossy(&out.stdout)))
    }

    /// 已知 = 扫描得到过（在库里），或字面上位于某个 `project_roots` 之下。
    /// 前者让用过的 worktree 也能查，后者让刚 clone 还没进过库的仓库不必先走一次 list。
    fn is_known(&self, path: &Path) -> bool {
        if path.components().any(|c| c.as_os_str() == "..") {
            return false;
        }
        let path_str = path.to_string_lossy().into_owned();
        let in_db: bool = self
            .db
            .conn()
            .query_row(
                "SELECT 1 FROM projects WHERE path = ?1",
                params![path_str],
                |_| Ok(true),
            )
            .unwrap_or(false);
        in_db || self.roots.iter().any(|r| path.starts_with(r))
    }

    fn scan(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for root in &self.roots {
            scan_dir(root, 1, &mut out);
        }
        out.sort();
        out.dedup();
        out
    }
}

fn scan_dir(dir: &Path, depth: usize, out: &mut Vec<(String, String)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || dir_name(&path).starts_with('.') {
            continue;
        }
        if is_repo(&path) {
            out.push((path.to_string_lossy().into_owned(), dir_name(&path)));
        } else {
            scan_dir(&path, depth + 1, out);
        }
    }
}

/// 主 worktree 是 `.git` 目录，linked worktree 是 `.git` 文件——两者都算仓库。
fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// `git worktree list --porcelain`：空行分段，每段首行 `worktree <path>`。
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        let (key, value) = match line.split_once(' ') {
            Some((k, v)) => (k, v),
            None => (line, ""),
        };
        match key {
            "worktree" => out.push(Worktree {
                path: value.to_owned(),
                branch: None,
                head: None,
                main: out.is_empty(),
                locked: false,
            }),
            "HEAD" => {
                if let Some(w) = out.last_mut() {
                    w.head = Some(value.to_owned());
                }
            }
            "branch" => {
                if let Some(w) = out.last_mut() {
                    w.branch = Some(value.trim_start_matches("refs/heads/").to_owned());
                }
            }
            "locked" => {
                if let Some(w) = out.last_mut() {
                    w.locked = true;
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_is_parsed_with_the_first_entry_as_main() {
        let text = "worktree /Users/r/code/agora\nHEAD abc123\nbranch refs/heads/main\n\n\
                    worktree /Users/r/code/agora-wt/xqa\nHEAD def456\nbranch refs/heads/feat/x\nlocked\n\n\
                    worktree /Users/r/code/agora-wt/detached\nHEAD 999\ndetached\n";
        let w = parse_worktrees(text);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert!(w[0].main && !w[0].locked);
        assert_eq!(w[1].branch.as_deref(), Some("feat/x"));
        assert!(!w[1].main && w[1].locked);
        // detached HEAD 没有分支名，不能显示成一个假分支。
        assert_eq!(w[2].branch, None);
        assert_eq!(w[2].head.as_deref(), Some("999"));
    }
}
