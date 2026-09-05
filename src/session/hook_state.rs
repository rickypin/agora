//! hook 观测检查点：一个会话一份，0700 目录 / 0600 文件，先 sync 再原子替换。
//! 不进 SQLite，也不保存进程事实；done 的 24 h 排障保留期不影响长期等待的会话。
use crate::status::machine::HookSnapshot;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

fn path(dir: &Path, id: &str) -> PathBuf {
    // 数据库 id 不参与路径语义。
    let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{key}.json"))
}

pub fn save(dir: &Path, id: &str, snapshot: &HookSnapshot) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let target = path(dir, id);
    let part = target.with_extension("part");
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
