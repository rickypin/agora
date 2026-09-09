//! 会话行上的项目身份（agora-uvd.1，A49 数据半边）。
//!
//! 侧栏要按「节点 → 仓库 → worktree」分组，行上要显示「仓库 · 分支」。这些不能靠前端
//! 拼 `/api/projects`：目录可能不在 `project_roots`、可能在 peer 上、分支会变。按会话的
//! `working_directory` 在服务端现算一次，与 [`crate::task::TaskIndex`] 同一套机制——内存缓存、
//! 异步补齐、TTL 重查、**不落库**（不变量 7：库里只有 `working_directory`）。
//!
//! 本模块只读 git，不碰 [`super::Projects`] 的库表，也不判断「已知项目」：会话在哪个目录
//! 就算哪个。

use serde::Serialize;

/// 会话所在 git 仓库 / worktree / 分支。只在内存缓存里活着，随 ProjectIndex 的 TTL 过期重查。
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
