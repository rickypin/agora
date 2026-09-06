//! 新建 worktree（MISSION §6.4；A44，agora-h1k.1）：`POST /api/projects/worktrees` 的后端。
//!
//! 「Worktree 跟着任务生灭，agora 只管"生"」——这是 §1.4 Git GUI 边界的唯一例外，且只增不改：
//! 这里唯一的写操作是 `git worktree add -b <name> <path> <base>`，合并与销毁归人。守卫
//! `tests/arch_boundary.rs::git_subprocesses_are_read_only_or_worktree_add` 把 `src/` 里的 git
//! 子命令钉在 status / diff / rev-parse / symbolic-ref / worktree list / worktree add。
//!
//! 路径按 `worktree_root`（docs/spec/config.md）：相对路径相对**主 worktree** 所在目录解析，
//! `{repo}` 换成主 worktree 的目录名，worktree 名接在其下。按主 worktree 而不是请求里的 `path`
//! 解析，是因为 linked worktree 也是已知项目（用过一次就进了 `projects` 表）：从
//! `agora-wt/x` 发起新建也该落到 `agora-wt/`，而不是 `agora-wt/x-wt/`。
//!
//! base 缺省链（docs/spec/api.md）：主 worktree 当前 checked-out 的分支 → detached 时
//! `refs/remotes/origin/HEAD` 指向的远端分支（`origin/main` 那种，作为起点它一定存在）→ 再兜底
//! `main`（连远端都没有的本地仓库；分支不存在时 git 失败 → 502 `git`）。

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::runtime::exec::{self, ExecOptions};

use super::{ProjectError, Projects, Worktree};

/// `worktree_root` 的缺省值（与 `config::Config::default` 一致）。
pub const DEFAULT_WORKTREE_ROOT: &str = "../{repo}-wt";

/// `git worktree add` 要把工作区 checkout 一遍，大仓库上远超 exec 的 5 s 缺省；其余 git
/// 调用仍用缺省。
const ADD_TIMEOUT: Duration = Duration::from_secs(120);

/// 名字合法性：既是一个路径分段又是一个分支名。这里拒的都是 400 `bad_request`，不等 git
/// 用文本报错再猜（§2.3 规则 10）。规则取 `git check-ref-format` 对单个分段的子集，多出的一条是
/// 不许以 `-` 开头——那会被 `git worktree add` 当成选项。
pub fn validate_name(name: &str) -> Result<(), ProjectError> {
    let bad = |why: &str| -> Result<(), ProjectError> {
        Err(ProjectError::InvalidName(format!("{name:?}: {why}")))
    };
    if name.is_empty() {
        return bad("不能为空");
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("不能含空白或控制字符");
    }
    if name.contains('/') || name.contains('\\') {
        return bad("不能含路径分隔符");
    }
    if name.contains("..") || name.contains("@{") {
        return bad("不能含 .. 或 @{");
    }
    if name
        .chars()
        .any(|c| matches!(c, '~' | '^' | ':' | '?' | '*' | '['))
    {
        return bad("含 git 引用名不允许的字符 ~ ^ : ? * [");
    }
    if name.starts_with('-') || name.starts_with('.') {
        return bad("不能以 - 或 . 开头");
    }
    if name.ends_with('.') || name.ends_with(".lock") {
        return bad("不能以 . 或 .lock 结尾");
    }
    Ok(())
}

/// `<worktree_root>/<name>` 按主 worktree 展开成绝对路径。不 canonicalize：目标目录本来就
/// 不该存在，canonicalize 会失败；`..` 靠词法归一吃掉。
pub fn worktree_path(worktree_root: &str, main_worktree: &Path, name: &str) -> PathBuf {
    let repo_name = main_worktree
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = PathBuf::from(worktree_root.replace("{repo}", &repo_name));
    normalize(&main_worktree.join(root).join(name))
}

/// 词法归一：吃掉 `.` 与 `..`，不碰文件系统。
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl Projects {
    /// 配置里的 `worktree_root`（docs/spec/config.md）。
    pub fn worktree_root(&self) -> &str {
        &self.worktree_root
    }

    /// `git worktree add -b <name> <worktree_root>/<name> <base>`；返回新项，形态与 [`Projects::worktrees`]
    /// 的每项一致（`path` 是 git 自己登记的规范路径，`head` 是真的）。
    ///
    /// 三种冲突在起 git 之前各按类型判：同名 worktree 已登记 → [`ProjectError::WorktreeExists`]，
    /// 分支已存在 → [`ProjectError::BranchExists`]，目录已存在 → [`ProjectError::PathExists`]。
    /// git 对这些的报错只有文本，先判才能按类型分支（§2.3 规则 10）；判完仍有竞态漏网的，
    /// 由 git 失败兜成 [`ProjectError::Git`]。
    pub fn create_worktree(
        &self,
        repo: &Path,
        name: &str,
        base: Option<&str>,
    ) -> Result<Worktree, ProjectError> {
        validate_name(name)?;
        // 已知项目校验与现有 worktree 一次拿到；主 worktree 的路径与分支都在第一项里。
        let existing = self.worktrees(repo)?;
        let main = existing
            .first()
            .ok_or_else(|| ProjectError::Git("git worktree list 没有返回主 worktree".to_owned()))?;
        let main_path = PathBuf::from(&main.path);
        let target = worktree_path(&self.worktree_root, &main_path, name);

        if existing.iter().any(|w| {
            let p = Path::new(&w.path);
            p.file_name().is_some_and(|n| n == name)
                || w.branch.as_deref() == Some(name)
                || p == target
        }) {
            return Err(ProjectError::WorktreeExists(name.to_owned()));
        }
        if branch_exists(&main_path, name)? {
            return Err(ProjectError::BranchExists(name.to_owned()));
        }
        if target.exists() {
            return Err(ProjectError::PathExists(
                target.to_string_lossy().into_owned(),
            ));
        }

        let base = match base.map(str::trim) {
            Some(b) if !b.is_empty() => b.to_owned(),
            _ => default_base(main, &main_path)?,
        };
        let repo_arg = main_path.to_string_lossy().into_owned();
        let target_arg = target.to_string_lossy().into_owned();
        let argv = [
            "git",
            "-C",
            &repo_arg,
            "worktree",
            "add",
            "-b",
            name,
            &target_arg,
            &base,
        ]
        .map(str::to_owned);
        let opts = ExecOptions {
            timeout: Some(ADD_TIMEOUT),
            ..ExecOptions::default()
        };
        let out = exec::exec(&argv, &opts)?;
        if !out.status.success() {
            return Err(ProjectError::Git(
                String::from_utf8_lossy(&out.stderr_tail).trim().to_owned(),
            ));
        }
        tracing::info!(component = "project", repo = %repo_arg, worktree = %target_arg, %base, "新建 worktree");

        // 回读 git 自己的登记而不是拼一个：path 要与 GET 列出来的同一形态（macOS 上 /tmp →
        // /private/tmp 那种规范化），head 也得是真的。
        let after = self.worktrees(repo)?;
        Ok(after
            .into_iter()
            .find(|w| w.branch.as_deref() == Some(name))
            .unwrap_or(Worktree {
                path: target_arg,
                branch: Some(name.to_owned()),
                head: None,
                main: false,
                locked: false,
            }))
    }
}

/// `git rev-parse --verify --quiet refs/heads/<name>`：退出码 0 就是分支存在。
fn branch_exists(repo: &Path, name: &str) -> Result<bool, ProjectError> {
    let repo_arg = repo.to_string_lossy().into_owned();
    let refname = format!("refs/heads/{name}");
    let argv = [
        "git",
        "-C",
        &repo_arg,
        "rev-parse",
        "--verify",
        "--quiet",
        &refname,
    ]
    .map(str::to_owned);
    let out = exec::exec(&argv, &ExecOptions::default())?;
    Ok(out.status.success())
}

/// base 缺省链：主 worktree 当前分支 → `origin/HEAD` 指向的远端分支 → `main`。
fn default_base(main: &Worktree, main_path: &Path) -> Result<String, ProjectError> {
    if let Some(b) = &main.branch {
        return Ok(b.clone());
    }
    let repo_arg = main_path.to_string_lossy().into_owned();
    let argv = [
        "git",
        "-C",
        &repo_arg,
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ]
    .map(str::to_owned);
    let out = exec::exec(&argv, &ExecOptions::default())?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            return Ok(s);
        }
    }
    Ok("main".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_path_segment_and_one_branch_name() {
        for ok in ["feat-x", "agora-h1k.1", "wf_1", "a.b"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", " ", "a b", "a/b", "..", "a..b", "-x", ".hidden", "x.lock", "a:b", "a~1", "a@{1}",
        ] {
            assert!(
                matches!(validate_name(bad), Err(ProjectError::InvalidName(_))),
                "{bad:?} 该被拒"
            );
        }
    }

    #[test]
    fn worktree_root_resolves_against_the_main_worktree() {
        let main = Path::new("/Users/r/code/agora");
        assert_eq!(
            worktree_path("../{repo}-wt", main, "h1k"),
            PathBuf::from("/Users/r/code/agora-wt/h1k")
        );
        // 绝对路径的 worktree_root 原样用。
        assert_eq!(
            worktree_path("/srv/wt/{repo}", main, "h1k"),
            PathBuf::from("/srv/wt/agora/h1k")
        );
        // 没有 {repo} 也行：所有仓库的 worktree 堆一处。
        assert_eq!(
            worktree_path("../wt", main, "h1k"),
            PathBuf::from("/Users/r/code/wt/h1k")
        );
    }
}
