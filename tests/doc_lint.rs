//! `scripts/doc-lint.sh` ③ 段的守卫（agora-yuk）：FOREIGN=[devcenter] 例外不覆盖
//! `docs/analysis/` 下的路径。2026-09-06 写 architecture.md 时把
//! `docs/analysis/devcenter/appendix-b-multihost.md` 误写成 appendix-b.md，同一行前面有
//! "devcenter 教训"，整行被放过、脚本报 0 问题——这里在 tempdir 里造一个最小仓库把这条
//! 例外的例外钉死，同时钉住两条不能退化的边：devcenter 自己的路径仍放过（哪怕离名字很远、
//! 隔着"；"），没提别的项目的行里缺失路径仍报。
//!
//! 仓库以前没有 doc-lint 的测试，CI 只是对真仓库跑一遍脚本；本文件不依赖 tests/common。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// 假 `bd`：脚本里 `shutil.which("bd")` 不为 None 就会 `bd list -t epic --all --json` 且
/// check=True，所以给它一个只会回答这一句的壳。认领 A2 而不是 A1——A1 在脚本的 SHARED_A
/// 里（MISSION §12 写明它横跨阶段），只有一个 epic 认领会被 ② 段报"已经不再跨 epic"
/// （2026-09-06 实测），那样三个测试的 exit code 就都混进 ② 的噪音。
const FAKE_BD: &str =
    "#!/bin/sh\nprintf '%s' '[{\"id\":\"x-1\",\"acceptance_criteria\":\"A2\"}]'\n";

/// 一个 tempdir 里的最小仓库：脚本、MISSION、四篇 OWN_DOCS、一份存在的 devcenter 分析报告、
/// `src/lib.rs`（让 TOP 里有 src），全部提交（脚本用 `git ls-files` 列 md）。
/// `spec` 是 docs/spec/x.md 的内容——三个测试只在这一处不同。
struct Repo {
    dir: tempfile::TempDir,
    bin: PathBuf,
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// 在 `dir` 里跑一条 git；全局 / 系统配置指到 /dev/null，本机的 init.templateDir、hooks
/// 之类干扰不进来（与 tests/worktree_create.rs 同一写法）。
fn git(dir: &Path, args: &[&str]) {
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
}

/// 本机没有 git 或 python3 就跳过（CI 的 ubuntu runner 两个都有，但不赌）。
fn tools_present() -> bool {
    ["git", "python3"].iter().all(|t| {
        Command::new(t)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn repo(spec: &str) -> Repo {
    let dir = tempfile::tempdir().unwrap();
    // tempdir/repo 是被检查的仓库，tempdir/bin 放假 bd——不进仓库，git ls-files 看不见它。
    let root = &dir.path().join("repo");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/doc-lint.sh");
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::copy(script, root.join("scripts/doc-lint.sh")).unwrap();
    write(
        root,
        "MISSION.md",
        "# MISSION\n\n## 12. 验收\n\n- [ ] **A2** 最小仓库里唯一的一条验收\n",
    );
    write(root, "README.md", "# readme\n");
    write(root, "AGENTS.md", "# agents\n");
    write(root, "ROADMAP.md", "# roadmap\n");
    write(root, "docs/spec/x.md", spec);
    write(root, "docs/adr/.gitkeep", "");
    write(
        root,
        "docs/analysis/devcenter/appendix-b-multihost.md",
        "# 附录 B（存在的报告）\n",
    );
    write(root, "src/lib.rs", "");
    // 假 bd 前置到 PATH；不整个换掉 PATH，python3 / git 还要找得到。
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("bd"), FAKE_BD).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("bd"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
    Repo { dir, bin }
}

impl Repo {
    fn lint(&self) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new("sh")
            .arg("scripts/doc-lint.sh")
            .current_dir(self.dir.path().join("repo"))
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("起 sh scripts/doc-lint.sh")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn context(out: &Output) -> String {
    format!(
        "exit={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        stdout(out),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn foreign_project_exception_does_not_cover_our_analysis_directory() {
    if !tools_present() {
        eprintln!("skip: 本机没有 git 或 python3");
        return;
    }
    let repo = repo(
        "devcenter 教训：见 `docs/analysis/devcenter/appendix-b.md`\n\
         devcenter 教训：见 `docs/analysis/devcenter/appendix-b-multihost.md`\n",
    );
    let out = repo.lint();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{}", context(&out));
    assert!(
        text.contains("docs/spec/x.md:1:")
            && text.contains("docs/analysis/devcenter/appendix-b.md"),
        "{}",
        context(&out)
    );
    // 存在的那份报告不报——句子里也有 devcenter，路径也在 docs/analysis/ 下，只是它真的在。
    assert!(!text.contains("docs/spec/x.md:2:"), "{}", context(&out));
    assert!(text.trim_end().ends_with("1 个问题"), "{}", context(&out));
}

#[test]
fn foreign_project_paths_are_still_exempt() {
    if !tools_present() {
        eprintln!("skip: 本机没有 git 或 python3");
        return;
    }
    // ADR-001:207 的形态：`src/tmux.rs` 离 devcenter 一词很远、中间隔着"；"，两个都不在仓里。
    let repo = repo("devcenter `src/which.rs` 的注释；`src/tmux.rs` 的 attach_args\n");
    let out = repo.lint();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{}", context(&out));
    assert!(text.trim_end().ends_with("0 个问题"), "{}", context(&out));
}

#[test]
fn our_own_missing_paths_are_still_reported() {
    if !tools_present() {
        eprintln!("skip: 本机没有 git 或 python3");
        return;
    }
    let repo = repo("见 `src/nope.rs`\n");
    let out = repo.lint();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{}", context(&out));
    assert!(
        text.contains("docs/spec/x.md:1:") && text.contains("src/nope.rs"),
        "{}",
        context(&out)
    );
}
