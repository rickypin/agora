//! 宿主文本通道的投递队列（agora-t5kf.1；ADR-002 D11）。
//!
//! 无句柄的行（external / headless）本来只能看：`POST /api/sessions/:id/input {kind:text}` 落到
//! 它们身上是 409 `no_runtime`（MISSION §5.5「没有终端与文本输入」）。宿主自己会执行文本的
//! （目前只有 pi 的扩展：`pi.sendUserMessage` 文档写「Always triggers a turn」，2026-10-07 实测）
//! 走这一格：daemon 把这件事写进
//! `<home>/input/<session hex>/<id>.json`（0700 目录 / 0600 文件，`.part` + rename），
//! 宿主的扩展取件、注入、改名回报。四态都是一次 rename：
//!
//! ```text
//! <id>.json     待取（daemon 写）
//! <id>.claimed  扩展已取走（先改名再注入：daemon 的超时清理因此不会把在途的那件删掉）
//! <id>.done     注入成功（API 的 ack）
//! <id>.failed   注入抛错（文件内容是给排障的一句话；API 报 host_rejected）
//! ```
//!
//! 目录名是会话 id 的逐字节 hex（与 `hooks/state` 同一习惯：数据库 id 不参与路径语义）。权限跟
//! 投递箱一个理由——同一台机器上的其他用户能往里塞 prompt 就等于能指挥别人的 agent。
//!
//! 超时删 `.json` 的竞态：扩展可能刚读到、还没 rename（几百微秒的窗口），daemon 这时删掉 `.json`，
//! 扩展的 rename 失败就放掉。真正的坏情形是扩展 rename 成功前的最后一步被删（概率更低），那一条
//! 会照常执行、用户按失败重试就会跑两遍——代价小、概率小，v1 接受，写在这里防后来者当 bug。

use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 队列的根：`<AGORA_HOME>/input`。
pub fn home_dir(agora_home: &Path) -> PathBuf {
    agora_home.join("input")
}

/// 一个会话的队列目录：`<home>/input/<session hex>`。
pub fn session_dir(home: &Path, session: &str) -> PathBuf {
    let key: String = session
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    home.join(key)
}

/// 队列条目的 id：`<ms>-<pid>-<seq>`。与投递件同一形态（毫秒 + 进程号），加一个进程内序号防同毫秒。
pub fn new_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "{:013}-{}-{}",
        ms,
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn path_of(home: &Path, session: &str, id: &str, suffix: &str) -> PathBuf {
    session_dir(home, session).join(format!("{id}.{suffix}"))
}

/// 写一件待取的文本。目录 0700、文件 0600，先 `.part` 再 rename（扩展看到的永远不完整）。
pub fn enqueue(home: &Path, session: &str, text: &str) -> io::Result<String> {
    let dir = session_dir(home, session);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let id = new_id();
    let part = path_of(home, session, &id, "json.part");
    let target = path_of(home, session, &id, "json");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&part)?;
        use std::io::Write;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&part, &target)?;
    Ok(id)
}

/// ack 等待的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `.done` 出现：宿主注入成功。
    Acked,
    /// `.failed` 出现：宿主拒绝了（内容是它留的一句话）。
    Rejected(String),
    /// 等到上限还没结果；还没被取走的 `.json` 已经删掉（`claimed` 留着让扩展跑完）。
    TimedOut,
}

/// 等宿主取件的结果：每 50 ms 看一次目录，最多 `wait`。async 是因为 API 在请求上下文里等。
pub async fn wait_acked(home: &Path, session: &str, id: &str, wait: Duration) -> Outcome {
    let deadline = Instant::now() + wait;
    loop {
        if path_of(home, session, id, "done").exists() {
            return Outcome::Acked;
        }
        if let Ok(reason) = std::fs::read_to_string(path_of(home, session, id, "failed")) {
            let reason = reason.trim();
            return Outcome::Rejected(if reason.is_empty() {
                "宿主拒绝了这条输入".to_owned()
            } else {
                reason.to_owned()
            });
        }
        if Instant::now() >= deadline {
            // 还没被取走的删掉：留着的话用户重试一次会跑两遍（已经被取走的 `.claimed` 不动，
            // 扩展会在注入完成后自己改名，`sweep` 再回收）。
            let _ = std::fs::remove_file(path_of(home, session, id, "json"));
            return Outcome::TimedOut;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 清理：`input/*/` 里超过 `older_than` 的 `.done` / `.failed` / `.claimed` 与残留的 `.part`
/// 全删；空目录也删。挂在 receiver 那条节流清理上（与归档、孤儿检查点同一趟扫目录）。
/// 不碰还在等取件的 `.json`（它属于某个正在等的请求，或对端进程已经不在——那种情况由下一次
/// 第 1 节里的超时清理掉）。
pub fn sweep(home: &Path, older_than: Duration) {
    let Ok(dirs) = std::fs::read_dir(home) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for dir in dirs.flatten() {
        let path = dir.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&path) else {
            continue;
        };
        let mut left = 0usize;
        for f in files.flatten() {
            let name = f.file_name();
            let name = name.to_string_lossy();
            let stale = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age >= older_than);
            let removable = name.ends_with(".done")
                || name.ends_with(".failed")
                || name.ends_with(".claimed")
                || name.ends_with(".part");
            if stale && removable {
                let _ = std::fs::remove_file(f.path());
            } else if !name.ends_with(".part") {
                left += 1;
            }
        }
        if left == 0 && std::fs::read_dir(&path).is_ok_and(|mut it| it.next().is_none()) {
            let _ = std::fs::remove_dir(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_is_private_and_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let root = home_dir(home.path());
        let id = enqueue(&root, "zuan:aa2462", "只回一个字：好").unwrap();
        let file = path_of(&root, "zuan:aa2462", &id, "json");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "只回一个字：好");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "队列文件必须只有自己可读（里面是 prompt）");
        let dir_mode = std::fs::metadata(session_dir(&root, "zuan:aa2462"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[tokio::test]
    async fn wait_acked_sees_done_and_rejected_and_times_out() {
        let home = tempfile::tempdir().unwrap();
        let root = home_dir(home.path());
        let session = "zuan:x";
        // ack：扩展的 rename 就是"把 json 变成 done"。
        let id = enqueue(&root, session, "go").unwrap();
        std::fs::rename(
            path_of(&root, session, &id, "json"),
            path_of(&root, session, &id, "done"),
        )
        .unwrap();
        assert_eq!(
            wait_acked(&root, session, &id, Duration::from_millis(200)).await,
            Outcome::Acked
        );
        // 拒绝：`.failed` 的内容是给人看的一句话。
        let id = enqueue(&root, session, "go").unwrap();
        std::fs::write(path_of(&root, session, &id, "failed"), "pi 不在提示符上\n").unwrap();
        assert_eq!(
            wait_acked(&root, session, &id, Duration::from_millis(200)).await,
            Outcome::Rejected("pi 不在提示符上".to_owned())
        );
        // 超时：没人取件，`.json` 被清掉，重试不会跑两遍；`.claimed` 的留在原地。
        let id = enqueue(&root, session, "go").unwrap();
        assert_eq!(
            wait_acked(&root, session, &id, Duration::from_millis(60)).await,
            Outcome::TimedOut
        );
        assert!(!path_of(&root, session, &id, "json").exists());
        let id2 = enqueue(&root, session, "go").unwrap();
        std::fs::rename(
            path_of(&root, session, &id2, "json"),
            path_of(&root, session, &id2, "claimed"),
        )
        .unwrap();
        assert_eq!(
            wait_acked(&root, session, &id2, Duration::from_millis(60)).await,
            Outcome::TimedOut
        );
        assert!(
            path_of(&root, session, &id2, "claimed").exists(),
            "在途的那件不能被超时删掉"
        );
    }

    #[test]
    fn sweep_removes_settled_and_keeps_pending() {
        let home = tempfile::tempdir().unwrap();
        let root = home_dir(home.path());
        let session = "zuan:x";
        let done = enqueue(&root, session, "a").unwrap();
        std::fs::rename(
            path_of(&root, session, &done, "json"),
            path_of(&root, session, &done, "done"),
        )
        .unwrap();
        let pending = enqueue(&root, session, "b").unwrap();
        // 0 秒的保留期：上面那条已结算的会被扫掉，待取的 `.json` 不碰。
        sweep(&root, Duration::ZERO);
        assert!(!path_of(&root, session, &done, "done").exists());
        assert!(path_of(&root, session, &pending, "json").exists());
    }
}
