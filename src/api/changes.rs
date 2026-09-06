//! 只读产出（MISSION §6.3「看结果」；A41，agora-h1k.5）：`GET /api/sessions/:id/changes` 列会话工作
//! 目录的改动文件，`WS /api/sessions/:id/diff` 在同一目录开一个只读终端跑 `git --no-pager diff`。
//!
//! 整条链路对仓库零写操作：两处 git 子进程都是只读子命令（`status` / `diff`），守卫
//! `tests/arch_boundary.rs::git_subprocesses_are_read_only_or_worktree_add` 扫得到这两个字面量数组；
//! diff 终端不进 `sessions` 表、不发任何事件、不碰 SessionManager——它只是一个跑完就退的 git 进程，
//! 浏览器关掉标签 WS 一断就 detach。git 写操作是 Git GUI 的事（MISSION §1.4）。
//!
//! 工作目录取 `record.working_directory`：对话框在选 linked worktree 时把 `working_directory` 直接填成
//! 该 worktree 的路径，`worktree` 字段存的是**分支名**（web/src/NewAgentDialog.tsx，2026-09-06 核对），
//! 不是路径，拿它当目录会落空。
//!
//! peer 会话经一跳转发：`/changes` 走 `forward::route`（GET 也能走它），`/diff` 走 `forward::terminal`
//! 带 `/diff` 后缀，所属节点的应答原样回。

use std::path::{Path, PathBuf};

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use super::auth::same_origin;
use super::terminal::TermQuery;
use super::{forward, sessions, ApiError, AppState};
use crate::auth::{AuthError, Principal};
use crate::gateway::{AttachedPty, ServerMessage};
use crate::runtime::exec::{self, ExecError, ExecOptions};
use crate::runtime::AttachSpec;

/// `GET /api/sessions/:id/changes` 的应答（docs/spec/api.md「只读产出」）。
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Changes {
    pub files: Vec<ChangedFile>,
    /// `# branch.head`；detached HEAD 为 null。列不出来（不是仓库…）也是 null。
    pub branch: Option<String>,
    /// 列表为空的类型化原因；正常为 null。取值见 [`Reason`]。
    pub reason: Option<Reason>,
}

#[derive(Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangedFile {
    pub path: String,
    pub status: FileStatus,
}

/// `git status --porcelain=v2` 的 XY 压成一个词：X（暂存区）先于 Y（工作区），任一侧是 D 就是
/// deleted（文件已经不在工作区里，这是看结果的人最想知道的），否则第一个非 `.` 的字母决定。
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Typechange,
    Unmerged,
    Untracked,
}

/// 文件列表为空的原因，按类型（MISSION §2.3 规则 10，前端只按它分支）。
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// 工作目录不是 git 仓库（`git status` 退出码 128）。
    NotARepo,
    /// 会话没有工作目录，或目录已经不存在。
    NoDirectory,
    /// 本机 PATH 里没有 git。
    NoGit,
    /// `git status` 超过 5 s 没退出（巨型仓库 / 网络盘）。
    Timeout,
    /// git 以其它非零码退出，或读输出失败；stderr 尾巴在 daemon 日志里。
    Git,
}

/// 目录不存在 / 不是仓库 / 没有 git / 超时 都是 200 + 空列表 + 类型原因：这些不是请求错误，是
/// "这个会话没有可看的改动"这一事实的几种形态，前端要一行灰字而不是错误横幅。
pub async fn list(
    principal: Principal,
    State(state): State<AppState>,
    UrlPath(gid): UrlPath<String>,
) -> Result<Response, ApiError> {
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::GET,
        "/changes",
        forward::NO_BODY,
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    let record = sessions::blocking(&state.sessions, move |s| Ok(s.get(&id)?.record)).await?;
    let changes = match record.working_directory.map(PathBuf::from) {
        Some(cwd) => status(&cwd).await,
        None => Changes::empty(Reason::NoDirectory),
    };
    Ok(Json(changes).into_response())
}

/// `WS /api/sessions/:id/diff`：形态照 `terminal::upgrade`（同源校验、hop、peer 转发），本机分支不经
/// SessionManager.attach，直接在 PTY 里起 `git diff`。桥是 `terminal::bridge(accept_input = false)`：
/// 第一帧 `status: read_only`，之后 input 帧一律丢弃。
///
/// 为什么是 `diff HEAD` 而不是裸 `diff`：agent 干完活常常已经 `git add` 了一部分，裸 `diff` 只给
/// 工作区对暂存区的差异、看不到已暂存的改动；`diff HEAD` 是"自上次提交以来一共改了什么"，与
/// 改动列表（status 同时列暂存与未暂存）对得上。未跟踪的新文件两种写法都不显示，只在列表里带 `?`。
/// 还没有任何提交的仓库没有 HEAD，git 在终端里自己报错退出，不另立错误类型。
pub async fn diff(
    principal: Principal,
    State(state): State<AppState>,
    UrlPath(gid): UrlPath<String>,
    Query(q): Query<TermQuery>,
    uri: Uri,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    if matches!(principal, Principal::Human { .. }) && !same_origin(&headers) {
        tracing::warn!(component = "api", principal = %principal.log_id(), "拒绝跨站 WS 升级（diff）");
        return Err(AuthError::CrossOrigin.into());
    }
    let id = match forward::hop(&state, &principal, &gid)? {
        forward::Hop::Local(id) => id,
        forward::Hop::Peer(t) => {
            return forward::terminal(&principal, t, &gid, "/diff", uri.query(), ws).await;
        }
    };
    let size = q.size();
    let record = {
        let id = id.clone();
        sessions::blocking(&state.sessions, move |s| Ok(s.get(&id)?.record)).await?
    };
    let Some(cwd) = record.working_directory else {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            kind: "no_directory",
            message: format!("会话 {id} 没有工作目录，没有可 diff 的仓库"),
        });
    };
    // argv 写成以 "git" 起头的字面量数组：守卫按这个形状扫子命令（"-C" / "--no-pager" 是 flag 被
    // 过滤，第一个非 dash 字面量 "diff" 才是子命令）。别改成 `-c color.ui=always`：带值的 flag 会让
    // 守卫把值当子命令（2026-09-06）。
    let spec = AttachSpec {
        argv: [
            "git",
            "-C",
            cwd.as_str(),
            "--no-pager",
            "diff",
            "--color=always",
            "HEAD",
        ]
        .map(str::to_owned)
        .to_vec(),
        // 双保险：--no-pager 已经关了分页，PAGER 再指到 cat 防某些 git 配置把 core.pager 塞回来。
        env: vec![
            ("GIT_PAGER".to_owned(), "cat".to_owned()),
            ("PAGER".to_owned(), "cat".to_owned()),
        ],
    };
    let log_id = principal.log_id();
    Ok(ws.on_upgrade(move |socket| async move {
        tracing::info!(component = "gateway", principal = %log_id, session = %id, %cwd, "diff attach (read-only)");
        let pty = match AttachedPty::spawn(&spec, size) {
            Ok(p) => p,
            Err(err) => {
                tracing::error!(component = "gateway", session = %id, %err, "diff 启动失败");
                let _ = super::terminal::send(
                    &mut { socket },
                    &ServerMessage::Output {
                        data: format!("\r\n[agora] diff error: {err}\r\n"),
                    },
                )
                .await;
                return;
            }
        };
        let pid = pty.pid();
        let released = super::terminal::bridge(socket, pty, false).await;
        tracing::info!(component = "gateway", principal = %log_id, session = %id, pid, released, "diff detach");
    }))
}

impl Changes {
    fn empty(reason: Reason) -> Self {
        Changes {
            files: Vec::new(),
            branch: None,
            reason: Some(reason),
        }
    }
}

/// 在 `cwd` 跑 `git status --porcelain=v2 --branch -z`（经 `runtime::exec`，缺省 5 s 超时）并解析。
/// 目录不存在先判：git 对不存在的 `-C` 目录也报 128，与"不是仓库"分不开。
async fn status(cwd: &Path) -> Changes {
    if !cwd.is_dir() {
        return Changes::empty(Reason::NoDirectory);
    }
    let cwd_arg = cwd.to_string_lossy().into_owned();
    // `-z`：路径不做 C 风格引号转义、条目以 NUL 结尾，重命名的两个路径各占一段——比按行解析再
    // 去引号可靠。守卫看的是这个字面量数组里的第一个非 dash 字面量 "status"。
    let argv = [
        "git",
        "-C",
        &cwd_arg,
        "status",
        "--porcelain=v2",
        "--branch",
        "-z",
    ]
    .map(str::to_owned)
    .to_vec();
    let out = match exec::exec_async(argv, ExecOptions::default()).await {
        Ok(out) => out,
        Err(err) if err.is_not_found() => return Changes::empty(Reason::NoGit),
        Err(ExecError::Timeout { .. }) => {
            tracing::warn!(component = "api", cwd = %cwd_arg, "git status 超时");
            return Changes::empty(Reason::Timeout);
        }
        Err(err) => {
            tracing::warn!(component = "api", cwd = %cwd_arg, %err, "git status 失败");
            return Changes::empty(Reason::Git);
        }
    };
    if !out.status.success() {
        // 128 是 git 的"fatal"：在这里几乎只有"not a git repository"一种；其余码走 Git。
        let reason = if out.status == exec::ExitStatus::Code(128) {
            Reason::NotARepo
        } else {
            tracing::warn!(
                component = "api",
                cwd = %cwd_arg,
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr_tail).trim(),
                "git status 非零退出"
            );
            Reason::Git
        };
        return Changes::empty(reason);
    }
    parse_porcelain_v2(&String::from_utf8_lossy(&out.stdout))
}

/// `--porcelain=v2 --branch -z` 的解析（git-status(1)「Porcelain Format Version 2」）：
/// `# branch.head <name>`（`(detached)` → null）；`1 XY …  <path>`；`2 XY … <X><score> <path> NUL <orig>`；
/// `u XY …  <path>`；`? <path>`；`! <path>` 忽略。字段以单个空格分隔，路径是最后一个字段，
/// 所以按 `splitn` 取定数个字段再把剩下的整个当路径（路径里可以有空格）。
pub fn parse_porcelain_v2(text: &str) -> Changes {
    let mut files = Vec::new();
    let mut branch = None;
    let mut entries = text.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        match entry.as_bytes().first() {
            Some(b'#') => {
                if let Some(head) = entry.strip_prefix("# branch.head ") {
                    branch = (head != "(detached)").then(|| head.to_owned());
                }
            }
            Some(b'1') => {
                // 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
                let mut f = entry.splitn(9, ' ');
                let (Some(_), Some(xy)) = (f.next(), f.next()) else {
                    continue;
                };
                let Some(path) = f.nth(6) else { continue };
                files.push(ChangedFile {
                    path: path.to_owned(),
                    status: status_of(xy),
                });
            }
            Some(b'2') => {
                // 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>  NUL  <origPath>
                let mut f = entry.splitn(10, ' ');
                let (Some(_), Some(xy)) = (f.next(), f.next()) else {
                    continue;
                };
                let Some(path) = f.nth(7) else { continue };
                // 原路径是下一段，吃掉它免得被当成条目。
                let _orig = entries.next();
                files.push(ChangedFile {
                    path: path.to_owned(),
                    status: status_of(xy),
                });
            }
            Some(b'u') => {
                // u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
                let mut f = entry.splitn(11, ' ');
                let Some(_) = f.next() else { continue };
                let Some(path) = f.nth(9) else { continue };
                files.push(ChangedFile {
                    path: path.to_owned(),
                    status: FileStatus::Unmerged,
                });
            }
            Some(b'?') => {
                if let Some(path) = entry.strip_prefix("? ") {
                    files.push(ChangedFile {
                        path: path.to_owned(),
                        status: FileStatus::Untracked,
                    });
                }
            }
            _ => {} // `!` 忽略的文件，或不认识的行
        }
    }
    files.sort();
    Changes {
        files,
        branch,
        reason: None,
    }
}

fn status_of(xy: &str) -> FileStatus {
    let mut it = xy.chars();
    let (x, y) = (it.next().unwrap_or('.'), it.next().unwrap_or('.'));
    if x == 'D' || y == 'D' {
        return FileStatus::Deleted;
    }
    match if x != '.' { x } else { y } {
        'A' => FileStatus::Added,
        'R' => FileStatus::Renamed,
        'C' => FileStatus::Copied,
        'T' => FileStatus::Typechange,
        'U' => FileStatus::Unmerged,
        _ => FileStatus::Modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(lines: &[&str]) -> String {
        lines.iter().map(|l| format!("{l}\0")).collect()
    }

    #[test]
    fn porcelain_v2_entries_map_to_typed_statuses_sorted_by_path() {
        let text = z(&[
            "# branch.oid 0123",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +1 -0",
            "1 .M N... 100644 100644 100644 aaa bbb src/z.rs",
            "1 A. N... 000000 100644 100644 000 ccc staged.txt",
            "1 .D N... 100644 100644 000000 ddd ddd gone.txt",
            "1 MD N... 100644 100644 000000 eee fff both.txt",
            "2 R. N... 100644 100644 100644 ggg ggg R100 new name.txt",
            "old name.txt",
            "1 .T N... 100644 100644 120000 hhh hhh link",
            "u UU N... 100644 100644 100644 100644 iii jjj kkk conflict.txt",
            "? untracked.txt",
            "! ignored.log",
        ]);
        let c = parse_porcelain_v2(&text);
        assert_eq!(c.branch.as_deref(), Some("main"));
        assert_eq!(c.reason, None);
        let got: Vec<(&str, FileStatus)> = c
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status))
            .collect();
        assert_eq!(
            got,
            vec![
                ("both.txt", FileStatus::Deleted),
                ("conflict.txt", FileStatus::Unmerged),
                ("gone.txt", FileStatus::Deleted),
                ("link", FileStatus::Typechange),
                ("new name.txt", FileStatus::Renamed),
                ("src/z.rs", FileStatus::Modified),
                ("staged.txt", FileStatus::Added),
                ("untracked.txt", FileStatus::Untracked),
            ]
        );
    }

    #[test]
    fn detached_head_has_no_branch_and_a_clean_tree_is_an_empty_list() {
        let c = parse_porcelain_v2(&z(&["# branch.oid 0123", "# branch.head (detached)"]));
        assert_eq!(c.branch, None);
        assert!(c.files.is_empty());
        assert_eq!(c.reason, None);
    }

    #[test]
    fn reasons_and_statuses_serialize_snake_case() {
        let c = Changes::empty(Reason::NotARepo);
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::json!({ "files": [], "branch": null, "reason": "not_a_repo" })
        );
        assert_eq!(
            serde_json::to_value(FileStatus::Typechange).unwrap(),
            serde_json::json!("typechange")
        );
        assert_eq!(
            serde_json::to_value(Reason::NoDirectory).unwrap(),
            serde_json::json!("no_directory")
        );
    }
}
