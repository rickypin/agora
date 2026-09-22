//! 集成测试自身的隔离守卫（agora-eny）。
//!
//! `tests/arch_boundary.rs` 扫的是 `src/` 与 `web/src/`，这里扫的是 `tests/` 自己：测试与 daemon
//! 一样会抢 tmux 的 socket 名字空间、AGORA_HOME 路径与监听端口，抢起来不报错，只是变成"某台
//! 机器上偶发红"。规则都写在 `tests/common/isolate.rs` 的文件注释里，这里让它们变得能红。
//!
//! 每条规则一个测试，关掉哪条哪条变红。

use std::fs;
use std::path::{Path, PathBuf};

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// `tests/` 下所有 `.rs`，除了隔离助手自己与本文件（它们按定义会提到这些字样）。
fn test_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("读 tests/").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = tests_dir();
    let mut files = Vec::new();
    walk(&root, &mut files);
    files
        .into_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(&root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "common/isolate.rs" || rel == "test_isolation.rs" {
                return None;
            }
            fs::read_to_string(&p).ok().map(|body| (rel, body))
        })
        .collect()
}

/// 从 `from` 处的 `{` 开始按花括号配对取出块体（含首尾花括号）。
fn block_at(body: &str, from: usize) -> &str {
    let bytes = body.as_bytes();
    let start = from + body[from..].find('{').expect("块里应有 {");
    let mut depth = 0usize;
    for (i, b) in bytes[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[start..start + i + 1];
                }
            }
            _ => {}
        }
    }
    &body[start..]
}

fn offenders(needle: &str) -> Vec<String> {
    test_sources()
        .into_iter()
        .filter(|(_, body)| body.contains(needle))
        .map(|(rel, _)| rel)
        .collect()
}

#[test]
fn fixture_names_come_from_isolate() {
    // tmux socket 名与 AGORA_HOME 路径只能由 isolate::socket_name / isolate::home_dir 生成：
    // 自己拼 `agora-<pid>-<n>` 会在 pid 回绕后与另一个测试进程撞名，撞上的表现是安静的假红
    // （连上别人的 tmux server / 两个进程共用一个 AGORA_HOME），不是报错。
    for (needle, what) in [
        (r#"format!("agora-"#, "tmux socket 名"),
        (r#"format!("/tmp/"#, "AGORA_HOME 路径"),
    ] {
        let bad = offenders(needle);
        assert!(
            bad.is_empty(),
            "{bad:?} 自己拼了{what}（{needle}…）；改用 isolate::socket_name / isolate::home_dir，\
             理由见 tests/common/isolate.rs"
        );
    }
}

#[test]
fn drop_impls_clean_up_through_kill_tmux() {
    // fixture 的 Drop 里只 kill-server 不删 socket 文件，死文件会在 /tmp/tmux-<uid>/ 越堆越多
    // （2026-09-10 开发机上 1361 个，其中 480 个来自一个 Drop，agora-n15），而死文件正是让下
    // 一个进程认领陌生 socket 的那一步。isolate::kill_tmux 把两件事一起做。
    let mut bad = Vec::new();
    for (rel, body) in test_sources() {
        let mut at = 0;
        while let Some(i) = body[at..].find("impl Drop for") {
            let start = at + i;
            let block = block_at(&body, start);
            if block.contains("kill-server") && !block.contains("isolate::kill_tmux") {
                bad.push(rel.clone());
            }
            at = start + block.len();
        }
    }
    assert!(
        bad.is_empty(),
        "{bad:?} 的 Drop 里直接跑了 tmux kill-server；改用 isolate::kill_tmux（它还会删掉 socket 文件）"
    );
}

#[test]
fn tests_never_bind_a_fixed_port() {
    // 端口靠 :0 让 OS 挑；写死端口的测试在并行 worktree 里会互相占位，也会撞上开发机上真的
    // daemon（:7680）。要一个"先占再放"的空闲端口，用 TcpListener::bind("127.0.0.1:0") 取。
    let mut bad = Vec::new();
    for (rel, body) in test_sources() {
        let mut at = 0;
        while let Some(i) = body[at..].find(r#"bind("127.0.0.1:"#) {
            let start = at + i;
            let rest = &body[start..];
            let port: String = rest
                .trim_start_matches(r#"bind("127.0.0.1:"#)
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if port != "0" {
                bad.push(format!("{rel}:{port}"));
            }
            at = start + 1;
        }
    }
    assert!(bad.is_empty(), "{bad:?} 绑了写死的端口；用 127.0.0.1:0");
}

#[test]
fn tmux_configs_in_tests_never_adopt_a_foreign_socket() {
    // adopt_sockets 必须显式写出来：默认值是 ["default"]，即用户自己终端里那个 tmux server。
    // 测试一旦采纳它，就会把开发机上真的会话列成自己的（2026-09-06 agora-oir 踩过），
    // 也会被别人的会话污染。要一个"外来 socket"就自己造一个（见 tests/runtime_tmux.rs 的 foreign）。
    let mut bad = Vec::new();
    for (rel, body) in test_sources() {
        let mut at = 0;
        while let Some(i) = body[at..].find("TmuxConfig {") {
            let start = at + i;
            let block = block_at(&body, start);
            if !block.contains("adopt_sockets") {
                bad.push(format!("{rel}（没写 adopt_sockets）"));
            } else if block.contains(r#""default""#) {
                bad.push(format!("{rel}（adopt_sockets 里有 default）"));
            }
            at = start + block.len();
        }
    }
    assert!(
        bad.is_empty(),
        "{bad:?}：测试里的 TmuxConfig 必须写 adopt_sockets 且不得采纳用户默认的 tmux server"
    );
}

#[test]
fn fixtures_never_chmod_by_hand() {
    // 造一个要被子进程 exec 的 fixture 不能裸 `fs::write` + `chmod 0755`：刚写完那份文件时，
    // 同一个测试二进制里别的线程正在 fork+exec，子进程会继承一份还开着的写句柄、攥到自己
    // exec 为止；在那之前 inode 的写计数 > 0，我们自己的 `execve` 就返回 ETXTBSY（os error 26）。
    // 实测（zuan，112 核）：`cargo test --test tmux_dead_window --test task_beads` 带负载连跑
    // 100 轮红 36 轮，红的形状是 tmux 那侧「无法启动 /tmp/.tmpXXXX/tmux: Text file busy」与
    // task_beads 那侧 `get` 得 None（exec 失败被 `TaskIndex::fetch` 当成"没这个 issue"，
    // 就是 agora-9zz 那条同二进制并行间歇假红）。显式 flush + drop 治不好（问题不在我们这边
    // 关得晚），要 exec 一次等到写计数归零；isolate::exec_script / isolate::mark_executable
    // 把这件事做了，理由与实测数字写在 tests/common/isolate.rs。
    // 扫 `from_mode(0o755)` 这个字面而不是"fs::write 附近有 chmod"：它是造可执行文件的唯一
    // 写法，而 0o700（tmux sockets 目录）/ 0o600（token、TLS 私钥）这类正当权限设置不会被误伤；
    // 助手自己与本文件按定义不扫（见 test_sources）。
    let bad = offenders("from_mode(0o755)");
    assert!(
        bad.is_empty(),
        "{bad:?} 自己 chmod 0755 造要 exec 的 fixture（裸 fs::write + chmod 会让下一次 exec 撞 \
         ETXTBSY）；改用 isolate::exec_script（脚本）或 isolate::mark_executable（复制来的二进制），\n\
         理由见 tests/common/isolate.rs"
    );
}

#[test]
fn isolate_is_included_once_per_test_binary() {
    // isolate::tag() 的唯一性来自模块里的 OnceLock：同一个二进制里把 isolate 包含两次
    // （`mod common;` 一份 + `#[path]` 一份）会得到两个模块、两个 OnceLock、两个不同的标签，
    // 同一个进程里的两套 fixture 就又开始各叫各的名字。有 common 的用 common::isolate。
    let bad: Vec<String> = test_sources()
        .into_iter()
        .filter(|(_, body)| {
            body.contains("mod common;") && body.contains(r#"#[path = "common/isolate.rs"]"#)
        })
        .map(|(rel, _)| rel)
        .collect();
    assert!(
        bad.is_empty(),
        "{bad:?} 同时包含了 common 与 isolate 两份；有 mod common 就用 common::isolate"
    );
}
