//! 集成测试之间的资源隔离（agora-eny）。
//!
//! 这些测试抢三样**全机器共享**的资源：tmux 的 socket 名字空间（`/tmp/tmux-<uid>/`）、
//! AGORA_HOME 的路径、监听端口。端口哪里都用 `:0` 让 OS 挑，剩下两样只能靠名字挑开。
//!
//! 名字以前是 `<前缀>-<pid>-<序号>`。**pid 不够**：pid 是顺序分配后回绕的，一次并行批次
//! （三个 worktree 各自 cargo build + cargo test，rustc / tmux / sh 加起来十万量级的进程）
//! 足够让两个测试二进制先后拿到同一个 pid，于是算出同一个 tmux socket 名与同一个
//! AGORA_HOME。撞上不会报错退出，而是安静的假红：后一个进程的 tmux 命令连上前一个留下的
//! server（agora-eny 描述里的"会话被别的进程列成 unregistered"），或两个 AGORA_HOME 互相
//! 覆盖。所以标签在 pid 之后再接一段本进程的启动时刻——同一个 pid 在不同时刻取到的标签不同。
//!
//! 清理是同一件事的另一半：2026-09-10 实测，开发机 `/tmp/tmux-501/` 里躺着 1361 个测试留下的
//! 死 socket 文件，其中 480 个是 `tests/runtime_tmux.rs` 的 `agora-test-*`——它的 Drop 只
//! kill-server、不删文件（agora-n15）。死文件本身不致命，但它把"名字撞了"从"tmux 说 no
//! server"变成"tmux 认领一个陌生 socket"，正是要防的那一类。所以 [`kill_tmux`] 两件事一起做，
//! 每个 fixture 的 Drop 都走它。
//!
//! 还有一样容易看漏的共享资源是**内核的"文件正被写"判定**：任何要被子进程 exec 的 fixture（假
//! tmux、假 bd、假 launchctl……）都不能裸 `fs::write` + `chmod 0755` 自己造，要走 [`exec_script`]
//! / [`mark_executable`]，否则满载并行时下一次 exec 按概率报 ETXTBSY（os error 26）；守卫是
//! `tests/test_isolation.rs::fixtures_never_chmod_by_hand`，成因与实测数字写在 [`mark_executable`]。

#![allow(dead_code)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agora::runtime::tmux::socket_path;

/// 造一个要 exec 的 fixture 时用的空转 argv（[`exec_script`] 注入的守卫认它）。
pub const PROBE_ARG: &str = "--agora-isolate-exec-probe";

/// 写一个**要被子进程 exec 的脚本** fixture：落盘 → 显式关句柄 → chmod 0755 → 空转 exec 一次。
///
/// 这是仓里造可执行 fixture 的唯一入口，配套的守卫是
/// `tests/test_isolation.rs::fixtures_never_chmod_by_hand`（任一测试文件里出现裸
/// `from_mode(0o755)` 就红）。
///
/// `body` 必须以 `#!` 开头；helper 会在 shebang 之后插一行守卫，收到 [`PROBE_ARG`] 就
/// `exit 0`，于是探针 exec 一定不进正文、不产生副作用（假 `bd` 会先把 argv 追加进
/// `calls.log`，正文里那行记账不能被探针踩到）。
pub fn exec_script(path: &Path, body: &str) -> PathBuf {
    let (shebang, rest) = body.split_once('\n').unwrap_or((body, ""));
    assert!(
        shebang.starts_with("#!"),
        "exec_script 要的是带 shebang 的脚本，{path:?} 给的是：{shebang}"
    );
    let guarded = format!("{shebang}\nif [ \"$1\" = \"{PROBE_ARG}\" ]; then exit 0; fi\n{rest}");
    // 显式写、显式关：句柄还活着的时候 exec 就是 ETXTBSY（os error 26）。
    let mut f = std::fs::File::create(path).unwrap_or_else(|e| panic!("写 fixture {path:?}: {e}"));
    f.write_all(guarded.as_bytes())
        .unwrap_or_else(|e| panic!("写 fixture {path:?}: {e}"));
    f.flush()
        .unwrap_or_else(|e| panic!("flush fixture {path:?}: {e}"));
    drop(f);
    mark_executable(path, &[PROBE_ARG]);
    path.to_path_buf()
}

/// chmod 0755，然后**真的 exec 一次**确认内核已经肯让这份文件被 exec。
///
/// 为什么非要 exec 一次：`std::fs::write` / `fs::copy` 写完就关句柄，可这台机器上跑并行
/// 测试时，同一测试二进制里**别的线程**正在 fork+exec，fork 那一刻如果我们的写句柄还开着，
/// 子进程会继承一份它、一直攥到自己 exec 为止；在那之前 inode 的写计数 > 0，我们的
/// `execve` 就返回 ETXTBSY。实测（zuan，112 核，8 线程反复「写 `#!/bin/sh` 脚本 + chmod +
/// exec」）：3200 份 fixture 里 130 次红在第一次 exec 上（约 4%，与 issue 记的 7% 同形），
/// 而 `File::create` + 显式 flush/drop **一次都治不好**（148/3200，同概率）——因为问题不在
/// 我们这边关得晚，在别人的子进程攥得久。
///
/// 所以 helper 自己先 exec 一次：ETXTBSY 就退避重试，以 [`PROC`] 为上限。探针成功的那一刻，
/// inode 的写计数一定已经归零，且 fixture 之后再也没人写它，于是被测代码那一次 exec 不可能
/// 再撞 ETXTBSY——同一负载下 3200 份 fixture，探针最多重试 2 次就过，被测 exec 0 红。
///
/// 重试次数一律 `println!` 进测试输出（0 次也打），跑统计时 grep `exec 探针` 就能数出
/// 这一轮里 ETXTBSY 真发生过几次。
///
/// `probe` 是调用方保证不产生副作用的一次 argv；脚本 fixture 由 [`exec_script`] 传
/// [`PROBE_ARG`]，复制来的真二进制由调用方自己给一个空转子命令。
pub fn mark_executable(path: &Path, probe: &[&str]) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {path:?}: {e}"));
    let deadline = Instant::now() + PROC;
    let mut retries = 0u32;
    loop {
        let mut cmd = Command::new(path);
        cmd.args(probe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.output() {
            Ok(_) => break,
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline => {
                retries += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("fixture {path:?} 写完 exec 不了（探针重试 {retries} 次）: {e}"),
        }
    }
    println!(
        "[isolate] fixture {} exec 探针重试 {retries} 次",
        path.display()
    );
}

/// 等一个**外部进程**把事做完的预算：起 tmux、起 `agora` / `sh` 子进程、hook 把文件落盘。
///
/// 为什么这么长：绿的路径一等到条件就返回，长预算一秒都不多花；它只在真坏的时候多等一会儿。
/// 而短预算在满载时会把好代码判红——2026-09-06 三个 worktree 并行 `cargo test --all-targets`
/// （8 核、CARGO_BUILD_JOBS=3 ×3、load average 36–58）时，满载下 fork+exec 一次就超过 5 s，
/// 一批打红了 `tests/tmux_dead_window.rs` 4/4、`tests/runtime_degraded.rs` 3 条
/// （agora-z62 / agora-74s），2026-09-09 那批又打红了 hook / inbox / upgrade 一串
/// （agora-pea / agora-8tv / agora-2ok / agora-p3l / agora-d0p）。
///
/// 放宽等待上限**不等于**放宽断言：断言原样保留，超时了照样红（同一条理由的先例见
/// `tests/api_input.rs::wait_until` 的 agora-e8s 注记与 `tests/task_beads.rs` 的
/// `FAKE_BD_TIMEOUT`）。这个常量只给"等外部进程"用；断言"必须很快"的地方（如
/// `tests/peer_stale.rs` 的 `took < …`）不要用它。
pub const PROC: Duration = Duration::from_secs(60);

/// 本测试进程独有的短标签：`<pid><启动时刻的 base36>`。
pub fn tag() -> &'static str {
    static TAG: OnceLock<String> = OnceLock::new();
    TAG.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        format!("{}{}", std::process::id(), base36(nanos))
    })
}

/// 进程内自增的 fixture 序号：同一个进程里的两个 fixture 也不共享名字。
pub fn nth() -> u32 {
    static N: AtomicU32 = AtomicU32::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}

/// 一个 fixture 的 tmux socket 名。`prefix` 标出是哪个测试文件，出事时 `ls /tmp/tmux-<uid>/`
/// 能认出人来。
pub fn socket_name(prefix: &str, n: u32) -> String {
    format!("agora-{prefix}-{}-{n}", tag())
}

/// 一个 fixture 的 AGORA_HOME。短路径是硬要求：macOS 的 unix socket 路径上限 104 字节，
/// tempfile 的默认目录太深会让 `<home>/agora.sock` 绑不上（agora-3la）。
pub fn home_dir(prefix: &str, n: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/ag-{prefix}-{}-{n}", tag()))
}

/// 杀掉这个 socket 上的 tmux server，并把 socket 文件一起删掉。
pub fn kill_tmux(socket: &str) {
    let _ = Command::new("tmux")
        .args(["-L", socket, "kill-server"])
        .stderr(Stdio::null())
        .status();
    // 2026-09-07 macOS 实测（agora-quy）：kill-server 之后 socket 文件仍可能留着。
    let _ = std::fs::remove_file(socket_path(socket));
}

/// 等到 `path` 上的 socket 真的连不上为止（上限 [`PROC`]）。
///
/// 造"server 已退出但 socket 文件还在"这个状态的写法是 `bind` 一个 `UnixListener` 再 `drop`。
/// drop 之后它**不一定立刻**拒连：本进程别的线程在那一瞬 fork 出来的子进程会带走一份继承来的
/// 监听 fd（Rust 的 socket 都是 CLOEXEC，但 CLOEXEC 只在 exec 那一刻生效，fork 与 exec 之间那
/// 一段 fd 还活着），只要还有一个 fd 活着，内核就照样让 connect 排进 backlog——测试于是看到
/// "已经关掉的监听还能连上"。
///
/// 2026-09-10 实测（agora-eny，解释 agora-743 / agora-as3）：整个 `tests/session_tmux.rs` 二进制
/// 多线程跑、机器 16 路满载时 10 轮红 3 次，全红在这一条断言上；同一条测试单独 `--exact` 跑
/// 10 轮 0 红——差别正是"同进程里有没有别的线程在起 tmux 子进程"。
/// 别把这里改回一次性断言：它在开发机上单跑永远是绿的。
pub fn wait_socket_refuses(path: &std::path::Path) {
    use std::os::unix::net::UnixStream;
    let deadline = std::time::Instant::now() + PROC;
    while UnixStream::connect(path).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "{path:?} 上仍有人监听（{PROC:?} 内没等到拒连）"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn base36(mut v: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = [b'0'; 8];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(v % 36) as usize];
        v /= 36;
    }
    String::from_utf8(out.to_vec()).expect("base36 只产 ASCII")
}
