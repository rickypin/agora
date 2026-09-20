//! hook 观测检查点：一个会话一份，0700 目录 / 0600 文件，先 sync 再原子替换。
//! 不进 SQLite，也不保存存活结论（信封里报来的 agent 进程号是 hook 事实，v2 起随之落盘，活没活
//! 每 tick 现算；agora-tql）；done 的 24 h 排障保留期不影响长期等待的会话。
use crate::status::machine::HookSnapshot;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// 一个 id 在 `state/` 里的两个文件名：检查点 `<hex>.json` 与 `save` 先写、再 rename 的
/// `<hex>.part`（崩在中途就留在目录里）。`hex` 是 id 的逐字节 hex（`901051` →
/// `393031303531`）：数据库 id 不参与路径语义，所以 `state/` 里看到的从来不是 id 本身。
/// 写路径与孤儿清理共用这一份命名，两边不会各自拼一遍后缀。
fn names(id: &str) -> [String; 2] {
    let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    [format!("{key}.json"), format!("{key}.part")]
}

fn path(dir: &Path, id: &str) -> PathBuf {
    dir.join(&names(id)[0])
}

/// `save` 的临时文件：它也归「有没有对应行」这一判，见 `prune_orphans`。
fn part_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(&names(id)[1])
}

pub fn save(dir: &Path, id: &str, snapshot: &HookSnapshot) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let target = path(dir, id);
    let part = part_path(dir, id);
    let bytes = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&part)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    fs::rename(part, target)?;
    fs::File::open(dir)?.sync_all()
}

pub fn load(dir: &Path, id: &str) -> io::Result<Option<HookSnapshot>> {
    match fs::read(path(dir, id)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn exists(dir: &Path, id: &str) -> bool {
    path(dir, id).exists()
}

pub fn remove(dir: &Path, id: &str) -> io::Result<()> {
    match fs::remove_file(path(dir, id)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// 删掉 `dir` 里不属于这些 id 的检查点文件（含 `save` 崩在中途留下的 `.part`），返回删掉的
/// 文件名（排序过，日志与断言拿到的顺序稳定）。清理挂在 sweep 的节流上，见
/// `Receiver::maybe_prune_done`（agora-5gg.14）。
///
/// 判「属不属于」是正向算出该留的名字，不是从文件名反解 id：`names()` 只会生成纯小写 hex，
/// 所以任何别的名字（手写的、更早版本写的）都必然对不上任何一行，反解那条路对它们无意义，
/// 而正向这条路不需要相信目录里的任何文件名。`.part` 一起判是同一条规则——`save` 只给有行的
/// 会话写临时文件（`apply_hook_inner` 开头就 `record(id)?`），所以在途的 `.part` 天然在 keep 里，
/// 不会被这一轮删走。
pub fn prune_orphans(dir: &Path, ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let keep: HashSet<String> = ids.into_iter().flat_map(|id| names(&id)).collect();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // 没有 `state/` = 这个节点还没落过检查点，不算故障；别的错（权限）才值得说一声。
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(component = "hook", dir = %dir.display(), %err, "扫 hook 检查点目录失败");
            return Vec::new();
        }
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) {
            continue;
        }
        // 只删文件：谁在 `state/` 里建了子目录都不轮到这次清理去动（`remove_file` 对目录只会报错）。
        if !entry.path().is_file() {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed.push(name),
            // 单个文件删不掉（权限、正被人拿着）不该让整轮停下来：留下等下一轮。
            Err(err) => {
                tracing::warn!(component = "hook", file = %entry.path().display(), %err, "删无行的 hook 检查点失败")
            }
        }
    }
    removed.sort();
    removed
}
