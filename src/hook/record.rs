//! 录制器（ADR-002 D10；agora-3la.1）：`agora hook --record <file>`（或环境变量
//! `AGORA_HOOK_RECORD`，agent 起 hook 时把自己的环境原样传下来，所以不用重装 hook 就能开）把
//! 每条 hook 事件追加成一行 fixture——`testdata/<agent>/<version>/hooks/` 的行格式
//! （`crate::adapter::replay`）：`payload` 已脱敏（`crate::adapter::scrub`），`at` 是相对录制
//! 零点的秒数，`hold` 是宿主对这条事件的挂起判定。`expect` 不写：期望什么状态是人核过才写进
//! fixture 的断言，录制器只记事实。
//!
//! 每条事件一个 hook 进程，所以时间零点（同时是脱敏的盐）存在文件的头注释
//! `# recorded host=<h> t0=<unix ms> …` 里，后来的进程读它；追加前 flock，并行工具的两个 hook
//! 同时写也不交错。文件 0600：脱敏是尽力而为，仍按私有文件对待。任何失败只在 stderr 提一句，
//! 投递照常——录制是建 fixture / 排障用的旁路，永远不拖累 agent。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::adapter::{scrub, AgentHooks};

const HEADER_PREFIX: &str = "# recorded";

#[derive(Serialize)]
struct Line<'a> {
    at: u64,
    hold: bool,
    payload: &'a Value,
}

/// 环境变量开关：值是录制文件路径。
pub const ENV: &str = "AGORA_HOOK_RECORD";

/// 追加一条。`now_ms` 与投递箱信封用同一个时刻。
pub fn append(
    path: &Path,
    hooks: &dyn AgentHooks,
    payload: &Value,
    now_ms: u64,
) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(path)?;
    lock(&file)?;
    let t0 = match read_t0(&mut file)? {
        Some(t0) => t0,
        None => {
            // 新文件（或人手写的没头的文件）：现在就是零点。头注释也是 fixture 合法的一行。
            writeln!(
                file,
                "{HEADER_PREFIX} host={} t0={now_ms} agora={} local={}",
                hooks.host(),
                env!("CARGO_PKG_VERSION"),
                super::inbox::local_time_string()
            )?;
            now_ms
        }
    };
    let scrubbed = scrub::scrub(payload, &t0.to_string());
    let line = Line {
        at: now_ms.saturating_sub(t0) / 1000,
        hold: hooks.hold_key(payload).is_some(),
        payload: &scrubbed,
    };
    let mut body = serde_json::to_vec(&line).map_err(std::io::Error::other)?;
    body.push(b'\n');
    // 一次 write：配合 flock，一行永远完整。
    file.write_all(&body)
}

/// 找头注释里的 `t0=`：扫全文件的注释行（文件只有几十行），不只首行——人可能在前面加过说明。
fn read_t0(file: &mut File) -> std::io::Result<Option<u64>> {
    file.seek(SeekFrom::Start(0))?;
    let reader = BufReader::new(&*file);
    for line in reader.lines() {
        let line = line?;
        if !line.starts_with(HEADER_PREFIX) {
            continue;
        }
        if let Some(t0) = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("t0="))
            .and_then(|v| v.parse().ok())
        {
            return Ok(Some(t0));
        }
    }
    Ok(None)
}

fn lock(file: &File) -> std::io::Result<()> {
    // SAFETY: fd 有效；LOCK_EX 阻塞到拿到锁，进程退出或 file 关闭时自动释放。
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter;
    use serde_json::json;

    #[test]
    fn lines_are_relative_to_the_header_and_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.jsonl");
        let hooks = adapter::for_host(adapter::hosts()[0]).unwrap();
        let sid = "11111111-2222-4333-8444-555555555555";
        let first = json!({"hook_event_name": "SessionStart", "session_id": sid, "cwd": "/Users/x/y", "source": "startup"});
        let second = json!({"hook_event_name": "PermissionRequest", "session_id": sid, "tool_name": "Bash", "tool_input": {"command": "rm -rf /"}});
        append(&path, hooks, &first, 1_000_000).unwrap();
        append(&path, hooks, &second, 1_003_500).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines = text.lines();
        let header = lines.next().unwrap();
        assert!(header.starts_with("# recorded host="), "{header}");
        assert!(header.contains("t0=1000000"), "{header}");
        let a: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let b: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(a["at"], 0);
        assert_eq!(b["at"], 3);
        assert_eq!(a["hold"], false);
        assert_eq!(b["hold"], hooks.hold_key(&second).is_some());
        // 两个进程（这里是两次调用）看到同一个盐：会话 id 一致且已脱敏。
        assert_eq!(a["payload"]["session_id"], b["payload"]["session_id"]);
        assert_ne!(a["payload"]["session_id"], json!(sid));
        assert!(!text.contains("rm -rf"));
        assert!(!text.contains("/Users/x"));
        // 录下来的文件是合法 fixture（没有 expect 也能回放）。
        adapter::replay::replay(hooks, &text).unwrap();
        // 权限 0600。
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
