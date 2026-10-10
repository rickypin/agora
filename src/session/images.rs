//! 会话附图（agora-lmz2；MISSION §6.9；docs/spec/api.md「附图」）：手机 composer 贴进来的截图
//! 落到**会话自己的工作目录**里，节点把绝对路径交回；客户端把 `[image: <路径>]` 接在下一条文本后面
//! 走原来的 `input`，由 agent 用它自己的读文件工具去看。agora 不认识任何宿主的图片协议，
//! 也不往 PTY 里塞二进制——路径是所有宿主都读得懂的那一种形态。
//!
//! 为什么是工作目录、不是 `<AGORA_HOME>`：2026-10-10 zuan 实测（隔离的运行时 + 真宿主 TUI，2.1.296），
//! 图放在工作目录**外**，宿主读它前先弹一道「读工作目录之外的文件」的四选项审批框，选了 Block
//! 就永远读不到；放在工作目录**内**直接读到、不打断。目录自带一个内容为 `*` 的 `.gitignore`，
//! 连它自己一起被忽略：同一次实测 `git status --porcelain` 为空、`git worktree remove` 不被挡——
//! agora 不碰仓库的任何 git 状态（MISSION §1.4）。
//!
//! 这里只管落盘与校验；哪一行能收图（`text_via`）、错误码与转发是 `api` 层的事。

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 工作目录下的附图目录名。不用 `.agora/`：那个名字更可能已经被别的东西占了，而这一层会在里面
/// 放一个忽略一切的 `.gitignore`。
pub const DIR: &str = ".agora-uploads";

/// 一张图解码后的上限。手机端先把大图缩到长边 2000、转 JPEG（通常几百 KB），这个数只挡异常请求；
/// 它还得能在 peer 一跳转发的 5 s 总时限里传完（`backoff::CONNECT_TIMEOUT`）。
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

/// 本层写下的图留多久：每次上传顺手删掉更早的，不另起清理任务。
pub const RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("图片是空的")]
    Empty,
    #[error("图片 {0} 字节，超过上限 {MAX_BYTES}")]
    TooLarge(usize),
    #[error("只收 PNG / JPEG / GIF / WebP 图片")]
    NotAnImage,
    #[error("工作目录 {0} 不存在或不是绝对路径的目录")]
    NoWorkingDirectory(PathBuf),
    #[error("{0} 不是普通目录（符号链接或文件），不往里写")]
    UnsafeDir(PathBuf),
    #[error("写附图失败: {0}")]
    Io(#[from] std::io::Error),
}

/// 按魔数认出来的图片格式；扩展名跟着它走，不信客户端报的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Kind {
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::Gif => "gif",
            Kind::Webp => "webp",
        }
    }
}

/// 只看文件头：PNG 8 字节签名、JPEG 的 SOI + 标记、GIF87a/89a、RIFF....WEBP。
pub fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Kind::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Kind::Webp)
    } else {
        None
    }
}

/// 把一张图写进 `<working_directory>/.agora-uploads/`，返回它的绝对路径。
///
/// 目录不存在就建（0700）并放 `.gitignore`；已存在则必须是真目录——仓库里提交了一个同名符号链接
/// 时不顺着它写到别处去。`.gitignore` 用 `create_new` 写，已经有了（包括被换成符号链接）一律不动。
/// 文件名是 `<毫秒>-<6 位随机 hex>.<ext>`，同样 `create_new`（0600：截图常带私人内容）。
pub fn save(
    working_directory: &Path,
    bytes: &[u8],
    now: SystemTime,
) -> Result<PathBuf, ImageError> {
    if bytes.is_empty() {
        return Err(ImageError::Empty);
    }
    if bytes.len() > MAX_BYTES {
        return Err(ImageError::TooLarge(bytes.len()));
    }
    let kind = sniff(bytes).ok_or(ImageError::NotAnImage)?;
    if !working_directory.is_absolute() || !working_directory.is_dir() {
        return Err(ImageError::NoWorkingDirectory(working_directory.to_owned()));
    }
    let dir = working_directory.join(DIR);
    ensure_dir(&dir)?;
    prune(&dir, now);

    let ms = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut tag = [0u8; 3];
    getrandom::fill(&mut tag).expect("系统随机源不可用");
    let name = format!(
        "{ms}-{:02x}{:02x}{:02x}.{}",
        tag[0],
        tag[1],
        tag[2],
        kind.ext()
    );
    let path = dir.join(name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(bytes)?;
    Ok(path)
}

fn ensure_dir(dir: &Path) -> Result<(), ImageError> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return Err(ImageError::UnsafeDir(dir.to_owned())),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            match fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                // 并发的两次上传同时建：另一边赢了，再看一眼它是不是真目录。
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if !fs::symlink_metadata(dir)?.file_type().is_dir() {
                        return Err(ImageError::UnsafeDir(dir.to_owned()));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".gitignore"))
    {
        Ok(mut f) => f.write_all(b"*\n")?,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// 只删本层的文件名形态（`<数字>-<hex>.<ext>`）、只删普通文件、只删早于 [`RETENTION`] 的；
/// 别人放进来的东西不动。删不掉不算失败——这一步只是顺手。
fn prune(dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !ours(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > RETENTION);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn ours(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let Some((ms, tag)) = stem.split_once('-') else {
        return false;
    };
    ["png", "jpg", "gif", "webp"].contains(&ext)
        && !ms.is_empty()
        && ms.bytes().all(|b| b.is_ascii_digit())
        && tag.len() == 6
        && tag.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn sniff_knows_the_four_formats_and_nothing_else() {
        assert_eq!(sniff(PNG), Some(Kind::Png));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]), Some(Kind::Jpeg));
        assert_eq!(sniff(b"GIF89a...."), Some(Kind::Gif));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Kind::Webp));
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(sniff(b"<svg xmlns=..."), None);
        assert_eq!(sniff(b"#!/bin/sh\n"), None);
    }

    #[test]
    fn saves_under_a_self_ignoring_directory() {
        let wd = tempfile::tempdir().unwrap();
        let path = save(wd.path(), PNG, SystemTime::now()).unwrap();
        assert!(path.is_absolute());
        assert_eq!(path.parent().unwrap(), wd.path().join(DIR));
        assert_eq!(path.extension().unwrap(), "png");
        assert!(ours(path.file_name().unwrap().to_str().unwrap()));
        assert_eq!(fs::read(&path).unwrap(), PNG);
        assert_eq!(
            fs::read_to_string(wd.path().join(DIR).join(".gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn refuses_a_symlinked_upload_dir() {
        let wd = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), wd.path().join(DIR)).unwrap();
        let err = save(wd.path(), PNG, SystemTime::now()).unwrap_err();
        assert!(matches!(err, ImageError::UnsafeDir(_)), "{err}");
        assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_empty_oversized_and_non_images() {
        let wd = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert!(matches!(save(wd.path(), b"", now), Err(ImageError::Empty)));
        assert!(matches!(
            save(wd.path(), b"hello", now),
            Err(ImageError::NotAnImage)
        ));
        let mut big = PNG.to_vec();
        big.resize(MAX_BYTES + 1, 0);
        assert!(matches!(
            save(wd.path(), &big, now),
            Err(ImageError::TooLarge(_))
        ));
        assert!(matches!(
            save(Path::new("relative/dir"), PNG, now),
            Err(ImageError::NoWorkingDirectory(_))
        ));
        assert!(!wd.path().join(DIR).exists(), "校验没过不该建目录");
    }

    #[test]
    fn prune_removes_only_our_old_files() {
        let wd = tempfile::tempdir().unwrap();
        let first = save(wd.path(), PNG, SystemTime::now()).unwrap();
        let dir = wd.path().join(DIR);
        fs::write(dir.join("notes.txt"), "mine").unwrap();
        // 八天之后再传一张：第一张过期被删，别人的文件与 .gitignore 不动。
        let later = SystemTime::now() + RETENTION + Duration::from_secs(24 * 3600);
        let second = save(wd.path(), PNG, later).unwrap();
        assert!(!first.exists());
        assert!(second.exists());
        assert!(dir.join("notes.txt").exists());
        assert!(dir.join(".gitignore").exists());
    }
}
