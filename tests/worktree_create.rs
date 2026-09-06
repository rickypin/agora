//! `POST /api/projects/worktrees`（MISSION §6.4「Worktree 跟着任务生灭，agora 只管"生"」；
//! A44，agora-h1k.1）：在 tmp 下 `git init` 一个真仓库，经 Router 走一遍新建 worktree 的
//! 三个验收点——路径按 `worktree_root`、base 缺省链、冲突按类型报错。
//!
//! 本批不改 tests/common（合并归 agora-7ku.9），仓库搭建的辅助放在本文件里。

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use agora::project::Projects;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{Fx, HOST};

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

/// `git init -b main` + 首个 commit；返回 false 表示本机没有可用的 git（调用方跳过，
/// 与 tests/projects.rs 同一取舍：CI 有 git，但不赌）。
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

/// tmp/alpha 是已知项目（`Projects::new` 的 roots 就是 tmp），`worktree_root` 用缺省的
/// `../{repo}-wt`，所以新 worktree 落在 tmp/alpha-wt/<name>。
struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    fx: Fx,
    cookie: String,
}

impl Repo {
    fn new() -> Option<Self> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let repo = root.join("alpha");
        if !git_init(&repo) {
            eprintln!("跳过：本机没有可用的 git");
            return None;
        }
        let mut fx = Fx::new();
        fx.state.projects = Arc::new(Projects::new(fx.db.clone(), vec![root.clone()]));
        let cookie = fx.cookie();
        Some(Repo {
            _tmp: tmp,
            root,
            repo,
            fx,
            cookie,
        })
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, format!("http://{HOST}"))
            .header(header::COOKIE, &self.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
            .unwrap();
        let resp = self.fx.app().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    async fn create(&self, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, "/api/projects/worktrees", Some(body))
            .await
    }

    async fn list(&self) -> Vec<Value> {
        let (status, body) = self
            .call(
                Method::GET,
                &format!(
                    "/api/projects/worktrees?path={}",
                    urlencode(&self.repo.to_string_lossy())
                ),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["worktrees"].as_array().unwrap().clone()
    }

    fn body(&self, name: &str) -> Value {
        json!({ "path": self.repo.to_string_lossy(), "name": name })
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn expected_path(&self, name: &str) -> PathBuf {
        self.root.join("alpha-wt").join(name)
    }
}

/// git 报的是规范路径（macOS 上 /tmp → /private/tmp），比对前两边都 canonicalize。
fn same_path(a: &str, b: &Path) -> bool {
    let a = Path::new(a)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(a));
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a == b
}

#[tokio::test]
async fn creates_branch_and_dir_under_worktree_root() {
    let Some(r) = Repo::new() else { return };
    let (status, body) = r.create(r.body("feat-x")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 应答形态与 GET 的每项一致：{ path, branch, head, main: false, locked: false }。
    assert_eq!(body["branch"], "feat-x", "{body}");
    assert_eq!(body["main"], false, "{body}");
    assert_eq!(body["locked"], false, "{body}");
    assert_eq!(body["head"], r.head(), "从主 worktree 当前提交起: {body}");
    let path = body["path"].as_str().unwrap();
    assert!(
        same_path(path, &r.expected_path("feat-x")),
        "路径按 worktree_root 缺省 ../{{repo}}-wt/<name>，得到 {path}"
    );

    // 目录真在、是 linked worktree（.git 是文件不是目录）、分支真在。
    assert!(r.expected_path("feat-x").join(".git").is_file());
    assert_eq!(
        git(&r.repo, &["rev-parse", "--verify", "refs/heads/feat-x"]),
        r.head()
    );

    // GET 现在列得出它，且与 POST 的应答一字不差。
    let list = r.list().await;
    assert_eq!(list.len(), 2, "{list:?}");
    assert_eq!(list[1], body);
}

#[tokio::test]
async fn base_defaults_to_main_worktrees_checked_out_branch() {
    let Some(r) = Repo::new() else { return };
    let main_head = r.head();
    // 主 worktree 切到 dev 并多一个提交：缺省 base 该是 dev，不是 main。
    git(&r.repo, &["checkout", "-q", "-b", "dev"]);
    git(&r.repo, &["commit", "-q", "--allow-empty", "-m", "dev"]);
    let dev_head = r.head();
    assert_ne!(main_head, dev_head);

    let (status, body) = r.create(r.body("from-dev")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["head"], dev_head,
        "缺省 base = 主 worktree 当前分支 dev: {body}"
    );

    // 显式 base 优先于缺省链。
    let (status, body) = r
        .create(json!({ "path": r.repo.to_string_lossy(), "name": "from-main", "base": "main" }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["head"], main_head, "{body}");

    // detached 且没有远端：兜底 main（不是 detached 那个 HEAD）。
    git(&r.repo, &["checkout", "-q", "--detach", "dev"]);
    let (status, body) = r.create(r.body("from-detached")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["head"], main_head,
        "detached 且无 origin/HEAD → 兜底 main: {body}"
    );

    // detached 但有 origin/HEAD：跟它走（链的中间一环）。远端跟踪引用手工造，不真 clone。
    git(&r.repo, &["update-ref", "refs/remotes/origin/dev", "dev"]);
    git(
        &r.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/dev",
        ],
    );
    let (status, body) = r.create(r.body("from-origin-head")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["head"], dev_head,
        "detached → origin/HEAD 指向的分支: {body}"
    );
}

#[tokio::test]
async fn name_collision_is_typed_error() {
    let Some(r) = Repo::new() else { return };
    let (status, body) = r.create(r.body("feat-x")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 同名 worktree 已登记。
    let (status, body) = r.create(r.body("feat-x")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "worktree_exists");

    // 分支已存在但没挂 worktree。
    git(&r.repo, &["branch", "orphan"]);
    let (status, body) = r.create(r.body("orphan")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "branch_exists");

    // 目录已存在但不是 worktree。
    std::fs::create_dir_all(r.expected_path("taken")).unwrap();
    let (status, body) = r.create(r.body("taken")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "path_exists");

    // 名字不合法：与 GET 的未知路径同一类 400 bad_request。
    for bad in ["", "a/b", "..", "a b", "-x", "a..b"] {
        let (status, body) = r.create(r.body(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}: {body}");
        assert_eq!(body["error"], "bad_request", "{bad:?}: {body}");
    }
    // 不是已知项目：不回答任意路径的存在性问题（与 GET 同）。
    let (status, body) = r.create(json!({ "path": "/etc", "name": "x" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "bad_request");

    // 起点分支不存在：git 自己失败 → 502 git，按类型不按文本。
    let (status, body) = r
        .create(json!({
            "path": r.repo.to_string_lossy(),
            "name": "nobase",
            "base": "no-such-branch",
        }))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"], "git");

    // 以上每一次拒绝都没有留下东西：worktree 仍只有主 + feat-x，分支也没多。
    assert_eq!(r.list().await.len(), 2);
    assert!(
        !r.expected_path("nobase").exists() && !r.expected_path("x").exists(),
        "被拒的请求不该留目录"
    );
    let branches = git(&r.repo, &["branch", "--list", "--format=%(refname:short)"]);
    let mut names: Vec<&str> = branches.lines().collect();
    names.sort_unstable();
    assert_eq!(names, vec!["feat-x", "main", "orphan"], "{branches}");
}

/// 只编码路径里会出现的字符；测试用不着完整的 percent-encoding。
fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' => c.to_string(),
            c => format!("%{:02X}", c as u32),
        })
        .collect()
}
