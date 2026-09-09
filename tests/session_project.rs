//! 会话行 `project` 字段（agora-uvd.1，A49）：按 working_directory 现算、不落库。
//!
//! 照 `tests/task_info.rs` 的搭法：临时目录、真 git 仓库 + linked worktree，
//! SessionManager 用同步的 [`ProjectIndex`]。

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use agora::project::{ProjectIndex, ProjectInfo};
use agora::runtime::{Runtime, Size};
use agora::session::{Db, NewSession, SessionManager};
use common::FakeRuntime;

/// 在 `dir` 里跑一条 git，返回 trim 过的 stdout；失败就 panic 带 stderr。
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("起 git");
    assert!(
        out.status.success(),
        "git {args:?} 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// `git init -b main` + 首个 commit；本机没有 git → false，调用方跳过。
fn git_init(dir: &Path) -> bool {
    std::fs::create_dir_all(dir).unwrap();
    let probe = Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        return false;
    }
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    true
}

fn session(cwd: &Path) -> NewSession {
    NewSession {
        display_name: "s".into(),
        agent_type: "shell".into(),
        working_directory: cwd.to_path_buf(),
        worktree: None,
        task_ref: None,
        command: "sleep 300".into(),
        env: vec![],
        size: Size::default(),
    }
}

fn mgr_sync() -> (SessionManager, Arc<FakeRuntime>, Arc<Db>) {
    mgr_with(ProjectIndex::default().synchronous())
}

fn mgr_with(index: ProjectIndex) -> (SessionManager, Arc<FakeRuntime>, Arc<Db>) {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(db.clone(), rt.clone() as Arc<dyn Runtime>)
        .with_project_index(Arc::new(index));
    (m, rt, db)
}

/// 以 git 自己报的路径为准（macOS 上 /tmp → /private/tmp）。
fn expected(cwd: &Path) -> ProjectInfo {
    let worktree = git(cwd, &["rev-parse", "--show-toplevel"]);
    let common = match Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        _ => git(cwd, &["rev-parse", "--git-common-dir"]),
    };
    let git_dir = {
        let p = PathBuf::from(&common);
        if p.is_absolute() {
            p
        } else {
            cwd.join(p)
        }
    };
    let repo = git_dir
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap_or_else(|_| git_dir.parent().unwrap().to_path_buf());
    let worktree = PathBuf::from(worktree)
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize worktree: {e}"));
    let branch = {
        let out = Command::new("git")
            .args(["symbolic-ref", "--short", "-q", "HEAD"])
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        } else {
            None
        }
    };
    ProjectInfo {
        name: repo.file_name().unwrap().to_string_lossy().into_owned(),
        main: worktree == repo,
        repo: repo.to_string_lossy().into_owned(),
        worktree: worktree.to_string_lossy().into_owned(),
        branch,
    }
}

fn skip_no_git() -> bool {
    let ok = Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("跳过：本机没有可用的 git");
    }
    !ok
}

#[test]
fn main_worktree_session_reports_repo_branch_and_main_true() {
    if skip_no_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("agora");
    assert!(git_init(&repo));
    let (m, _rt, _db) = mgr_sync();
    let v = m.create(&session(&repo)).unwrap();
    let want = expected(&repo);
    assert_eq!(v.project.as_ref(), Some(&want));
    assert!(want.main, "主 worktree 的 main 应为 true");
    assert_eq!(want.branch.as_deref(), Some("main"));
    let wire = serde_json::to_value(&v).unwrap();
    assert_eq!(wire["project"]["repo"], want.repo);
    assert_eq!(wire["project"]["name"], "agora");
    assert_eq!(wire["project"]["worktree"], want.worktree);
    assert_eq!(wire["project"]["branch"], "main");
    assert_eq!(wire["project"]["main"], true);
}

#[test]
fn linked_worktree_session_points_repo_to_main_worktree_and_main_false() {
    if skip_no_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("agora");
    assert!(git_init(&repo));
    let linked = tmp.path().join("agora-wt").join("feat");
    std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat",
            linked.to_str().unwrap(),
        ],
    );
    let (m, _rt, _db) = mgr_sync();
    let v = m.create(&session(&linked)).unwrap();
    let want = expected(&linked);
    assert_eq!(v.project.as_ref(), Some(&want));
    assert!(!want.main);
    assert_eq!(want.branch.as_deref(), Some("feat"));
    assert_eq!(
        PathBuf::from(&want.repo).canonicalize().unwrap(),
        repo.canonicalize().unwrap(),
        "linked worktree 的 repo 应指向主 worktree"
    );
    assert_eq!(
        PathBuf::from(&want.worktree).canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
}

#[test]
fn session_in_a_subdirectory_still_resolves_its_worktree_root() {
    if skip_no_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("agora");
    assert!(git_init(&repo));
    let sub = repo.join("src").join("lib");
    std::fs::create_dir_all(&sub).unwrap();
    let (m, _rt, _db) = mgr_sync();
    let v = m.create(&session(&sub)).unwrap();
    let want = expected(&sub);
    assert_eq!(v.project.as_ref(), Some(&want));
    assert!(want.main);
    assert_eq!(
        PathBuf::from(&want.worktree).canonicalize().unwrap(),
        repo.canonicalize().unwrap()
    );
}

#[test]
fn non_git_directory_and_missing_directory_yield_project_null() {
    let tmp = tempfile::tempdir().unwrap();
    let not_git = tmp.path().join("notes");
    std::fs::create_dir_all(&not_git).unwrap();
    let missing = tmp.path().join("no-such");
    let (m, _rt, _db) = mgr_sync();

    let a = m.create(&session(&not_git)).unwrap();
    assert_eq!(a.project, None);
    let ja = serde_json::to_string(&a).unwrap();
    assert!(
        ja.contains("\"project\":null"),
        "不是 git 仓库应显式 null，不是缺键: {ja}"
    );

    let b = m.create(&session(&missing)).unwrap();
    assert_eq!(b.project, None);
    let jb = serde_json::to_string(&b).unwrap();
    assert!(
        jb.contains("\"project\":null"),
        "目录不存在应显式 null，不是缺键: {jb}"
    );
}

#[test]
fn detached_head_yields_branch_null() {
    if skip_no_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("agora");
    assert!(git_init(&repo));
    git(&repo, &["checkout", "--detach", "-q"]);
    let (m, _rt, _db) = mgr_sync();
    let v = m.create(&session(&repo)).unwrap();
    let p = v.project.as_ref().expect("detached HEAD 仍是仓库");
    assert_eq!(p.branch, None);
    let wire = serde_json::to_value(&v).unwrap();
    assert!(wire["project"]["branch"].is_null());
    assert_eq!(wire["project"]["main"], true);
}

#[test]
fn branch_change_is_visible_after_ttl() {
    if skip_no_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("agora");
    assert!(git_init(&repo));
    let index = Arc::new(
        ProjectIndex::default()
            .synchronous()
            .with_ttl(Duration::ZERO),
    );
    let first = index.get(&repo).expect("主 worktree 查得到");
    assert_eq!(first.branch.as_deref(), Some("main"));
    git(&repo, &["checkout", "-q", "-b", "other"]);
    let second = index.get(&repo).expect("TTL 过期后重查");
    assert_eq!(second.branch.as_deref(), Some("other"));
}

#[test]
fn project_is_not_stored_in_sqlite() {
    if skip_no_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("agora");
    assert!(git_init(&repo));
    let db_path = tmp.path().join("agora.db");
    let db = Arc::new(Db::open(&db_path).unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(db.clone(), rt as Arc<dyn Runtime>)
        .with_project_index(Arc::new(ProjectIndex::default().synchronous()));
    let v = m.create(&session(&repo)).unwrap();
    let p = v.project.as_ref().expect("API 有 project");
    assert_eq!(p.branch.as_deref(), Some("main"));

    let conn = db.conn();
    let mut stmt = conn.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for forbidden in ["branch", "repo", "main"] {
        assert!(
            !cols.iter().any(|c| c == forbidden),
            "sessions 长出了 project 内容列 {forbidden}: {cols:?}（不变量 7：只存 working_directory）"
        );
    }
    drop(stmt);
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(conn);

    let dump = std::fs::read(&db_path).unwrap();
    let text = String::from_utf8_lossy(&dump);
    let repo_path = p.repo.as_str();
    // dump 里可以出现 repo 路径（working_directory），但字段名 "branch" 不得作为独立词出现。
    if let Some(i) = text.find("branch") {
        let around = &text[i.saturating_sub(80)..(i + 80).min(text.len())];
        assert!(
            repo_path.contains("branch"),
            "库文件里出现了字段名 branch（不在 repo 路径里）: {around:?}"
        );
    }
}
