//! `agora preset` CLI 的往返守卫（agora-prdg.3；epic agora-hxva）：真二进制 + 隔离 AGORA_HOME，
//! 直接操作 `AGORA_HOME/agora.db`（不起 daemon，ADR-003 D6 的先例同 `agora peer token`）——
//! CLI 写的、daemon 读的是同一张表。所有断言都从库里读回来，不解析 stdout 的排版细节。

#[path = "common/isolate.rs"]
mod isolate;

use std::path::PathBuf;
use std::process::{Command, Output};

use agora::session::preset;
use agora::session::Db;

struct Home {
    path: PathBuf,
}

impl Home {
    fn new(prefix: &str) -> Self {
        let path = isolate::home_dir(prefix, isolate::nth());
        let _ = std::fs::remove_dir_all(&path);
        Home { path }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_agora"))
            .args(["preset"])
            .args(args)
            .env("AGORA_HOME", &self.path)
            .output()
            .unwrap()
    }

    fn open(&self) -> Db {
        // CLI 在不合法取值上是操作失败（退出 1），此时可能还没建 AGORA_HOME；测试自己补上
        // （走 daemon/CLI 同一个 0700 自检），好把"库里没有这一行"读出来。
        agora::local::ensure_home(&self.path).unwrap();
        Db::open(&self.path.join("agora.db")).unwrap()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// add → list → show → rm → list 空；同名 upsert 整行覆盖；--args 原样落库与取出。
#[test]
fn cli_round_trip_and_upsert_overwrite() {
    let home = Home::new("preset");
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();

    let add = home.run(&[
        "add",
        "pktmask",
        "--agent",
        "claude",
        "--dir",
        dir.to_str().unwrap(),
        "--args",
        "--model opus -c 'a b'",
        "--prompt",
        "跑一遍测试",
    ]);
    assert_eq!(add.status.code(), Some(0), "{}", stderr(&add));
    assert!(stdout(&add).contains("pktmask"));

    // 库里的行：agent / 目录（canonical 绝对路径）/ args 原样 / prompt。
    let db = home.open();
    let p = preset::get(&db, "pktmask").unwrap();
    assert_eq!(p.agent_type, "claude");
    assert_eq!(p.working_directory, dir.display().to_string());
    assert_eq!(p.args.as_deref(), Some("--model opus -c 'a b'"));
    assert_eq!(p.prompt.as_deref(), Some("跑一遍测试"));

    let listed = home.run(&["list"]);
    assert_eq!(listed.status.code(), Some(0));
    let text = stdout(&listed);
    assert!(
        text.contains("pktmask") && text.contains("claude"),
        "{text}"
    );
    assert!(text.contains("--model opus -c 'a b'"), "{text}");

    let shown = home.run(&["show", "pktmask"]);
    assert_eq!(shown.status.code(), Some(0));
    let text = stdout(&shown);
    for needle in [
        "pktmask",
        "claude",
        &dir.display().to_string(),
        "--model opus -c 'a b'",
        "跑一遍测试",
    ] {
        assert!(text.contains(needle), "show 缺 {needle:?}: {text}");
    }

    // 同名 upsert：整行覆盖（agent / 参数 / 首句都换），仍只有一行。
    let upsert = home.run(&[
        "add",
        "pktmask",
        "--agent",
        "codex",
        "--dir",
        std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap(),
        "--args",
        "--continue",
    ]);
    assert_eq!(upsert.status.code(), Some(0), "{}", stderr(&upsert));
    let db = home.open();
    let overwritten = preset::get(&db, "pktmask").unwrap();
    assert_eq!(overwritten.agent_type, "codex");
    assert_eq!(overwritten.args.as_deref(), Some("--continue"));
    assert_eq!(overwritten.prompt, None, "整行覆盖：没给的字段清空");
    assert_eq!(preset::list(&db).unwrap().len(), 1, "同名覆盖不是新增");

    let rm = home.run(&["rm", "pktmask"]);
    assert_eq!(rm.status.code(), Some(0), "{}", stderr(&rm));
    assert_eq!(preset::list(&home.open()).unwrap(), vec![]);

    let empty = home.run(&["list"]);
    assert_eq!(empty.status.code(), Some(0));
    assert!(
        stdout(&empty).contains("还没有预设") && stdout(&empty).contains("agora preset add"),
        "空态给可照做的指引: {}",
        stdout(&empty)
    );
}

/// 未知 agent / 目录不存在是操作失败（退出 1），且不落库；未知预设 show / rm 也是 1。
#[test]
fn cli_rejects_unknown_agent_and_missing_directory_with_exit_1() {
    let home = Home::new("presetbad");
    let dir = tempfile::tempdir().unwrap();

    let agent = home.run(&[
        "add",
        "x",
        "--agent",
        "not-an-agent",
        "--dir",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(agent.status.code(), Some(1), "{}", stderr(&agent));
    assert!(stderr(&agent).contains("未知 agent"), "{}", stderr(&agent));

    let missing = home.run(&[
        "add",
        "x",
        "--agent",
        "claude",
        "--dir",
        "/nonexistent/agora-prdg3",
    ]);
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
    assert!(
        stderr(&missing).contains("目录不存在"),
        "{}",
        stderr(&missing)
    );

    // 两种失败都不该写出预设行（库可能已经被打开，但表里必须空）。
    assert_eq!(preset::list(&home.open()).unwrap(), vec![]);

    let shown = home.run(&["show", "nope"]);
    assert_eq!(
        shown.status.code(),
        Some(1),
        "stdout={} stderr={}",
        stdout(&shown),
        stderr(&shown)
    );
    assert_eq!(home.run(&["rm", "nope"]).status.code(), Some(1));
}

/// 用法错误（2）在建库之前就返回：缺必填项、多给位置参数、未知旗标。
#[test]
fn cli_usage_errors_exit_2_before_touching_the_database() {
    let home = Home::new("presetusage");
    let dir = tempfile::tempdir().unwrap();

    assert_eq!(home.run(&[]).status.code(), Some(2));
    assert_eq!(home.run(&["bogus"]).status.code(), Some(2));
    assert_eq!(
        home.run(&["add", "x", "--agent", "claude"]).status.code(),
        Some(2),
        "缺 --dir"
    );
    assert_eq!(
        home.run(&[
            "add",
            "--agent",
            "claude",
            "--dir",
            dir.path().to_str().unwrap()
        ])
        .status
        .code(),
        Some(2),
        "缺 <name>"
    );
    assert_eq!(
        home.run(&[
            "add",
            "a",
            "b",
            "--agent",
            "claude",
            "--dir",
            dir.path().to_str().unwrap()
        ])
        .status
        .code(),
        Some(2),
        "两个位置参数"
    );
    assert_eq!(
        home.run(&[
            "add",
            "x",
            "--agent",
            "claude",
            "--dir",
            dir.path().to_str().unwrap(),
            "--wat"
        ])
        .status
        .code(),
        Some(2),
        "未知旗标"
    );
    assert!(
        !home.path.join("agora.db").exists(),
        "用法错误不该建库: {}",
        home.path.display()
    );
}

/// 不吃首句的 agent 给了 --prompt：警告但不算错，值保留（将来 agent 变了就能用）。
#[test]
fn prompt_for_an_agent_that_does_not_take_it_is_warned_and_kept() {
    let home = Home::new("presetprompt");
    let dir = tempfile::tempdir().unwrap();
    let out = home.run(&[
        "add",
        "shellrun",
        "--agent",
        "shell",
        "--dir",
        dir.path().to_str().unwrap(),
        "--prompt",
        "echo hi",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).contains("警告"), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("不算错") || stderr(&out).contains("照存"),
        "警告要说清不是错误: {}",
        stderr(&out)
    );
    let p = preset::get(&home.open(), "shellrun").unwrap();
    assert_eq!(p.prompt.as_deref(), Some("echo hi"), "警告不等于丢弃");
}
