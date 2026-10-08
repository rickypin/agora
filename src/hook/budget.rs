//! Bounded durable spool admission. Existing deliveries are never evicted to admit a new one.
//! Payload fields unused by adapters are trimmed before admission; resource exhaustion is
//! recorded separately so a healthy HTTP connection cannot hide incomplete observations.
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{Delivery, HookError, Inbox};

pub const MAX_PENDING_FILES: usize = 8192;
pub const MAX_PENDING_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_DELIVERY_BYTES: usize = 1024 * 1024;
pub const MAX_STDIN_BYTES: u64 = 16 * 1024 * 1024;

impl Inbox {
    pub fn gap_path(&self) -> PathBuf {
        self.inbox_dir()
            .parent()
            .expect("hooks parent")
            .join("observation-gap.json")
    }

    /// Sticky until an operator acknowledges the lost observations. Never contains payloads.
    pub fn note_gap(&self, reason: &str) {
        let target = self.gap_path();
        let Some(parent) = target.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        let part = target.with_extension(format!("{}.part", std::process::id()));
        let body = serde_json::json!({"reason": reason, "at": super::inbox::now_unix_ms()});
        if let Ok(mut f) = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&part)
        {
            use std::io::Write;
            if f.write_all(body.to_string().as_bytes()).is_ok() && f.sync_all().is_ok() {
                let _ = fs::rename(&part, &target);
            }
        }
        let _ = fs::remove_file(part);
    }

    pub fn observation_gap(&self) -> Option<serde_json::Value> {
        match fs::read(self.gap_path()) {
            Ok(bytes) => Some(
                serde_json::from_slice(&bytes)
                    .unwrap_or_else(|_| serde_json::json!({"reason": "unreadable_gap"})),
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(serde_json::json!({"reason": "unreadable_gap"})),
        }
    }

    /// All CLI writers share a short admission lock. The receiver only removes files, so
    /// concurrent consumption can overestimate usage but can never over-admit writers.
    pub fn write_bounded(&self, delivery: &Delivery) -> Result<PathBuf, HookError> {
        let result = self.write_with_limits(delivery, MAX_PENDING_FILES, MAX_PENDING_BYTES);
        if let Err(err) = &result {
            self.note_gap(match err {
                HookError::Capacity => "inbox_capacity",
                _ => "inbox_write_failed",
            });
        }
        result
    }

    pub fn write_with_limits(
        &self,
        delivery: &Delivery,
        files_limit: usize,
        bytes_limit: u64,
    ) -> Result<PathBuf, HookError> {
        let io = |source| HookError::Io {
            path: self.inbox_dir().display().to_string(),
            source,
        };
        let root = self.inbox_dir();
        fs::create_dir_all(&root).map_err(io)?;
        // Same privacy boundary as Inbox::write; validate before inspecting existing data.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(io)?;
        let hooks = root.parent().expect("hooks parent");
        fs::set_permissions(hooks, fs::Permissions::from_mode(0o700)).map_err(io)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(hooks.join("admission.lock"))
            .map_err(io)?;
        let until = Instant::now() + Duration::from_millis(250);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Err(HookError::Capacity),
                Err(std::fs::TryLockError::Error(e)) => return Err(io(e)),
            }
        }
        let mut compact = delivery.clone();
        super::inbox::trim_unused_results(&mut compact);
        let bytes = serde_json::to_vec(&compact)
            .map_err(|e| io(io::Error::other(e)))?
            .len();
        if files_limit == 0 || bytes > MAX_DELIVERY_BYTES {
            return Err(HookError::Capacity);
        }
        let mut count = 0;
        let mut total = bytes as u64;
        // Stream the directory tree: legacy oversized inboxes must not be allocated/sorted
        // in every hook process just to discover that admission is already over budget.
        for host in fs::read_dir(&root).map_err(io)? {
            let host = host.map_err(io)?.path();
            for session in fs::read_dir(&host).map_err(io)? {
                let session = session.map_err(io)?.path();
                for file in fs::read_dir(&session).map_err(io)? {
                    let file = file.map_err(io)?;
                    match file.metadata() {
                        Ok(m) if m.is_file() => {
                            count += 1;
                            total += m.len();
                        }
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(io(e)),
                    }
                    if count >= files_limit || total > bytes_limit {
                        return Err(HookError::Capacity);
                    }
                }
                // Admission lock excludes writers; receiver only moves files out.
                // Empty conversation directories must not grow forever either.
                let _ = fs::remove_dir(session);
            }
            let _ = fs::remove_dir(host);
        }
        if total > bytes_limit {
            return Err(HookError::Capacity);
        }
        self.write(&compact)
    }
}
