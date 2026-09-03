//! 人的凭据（ADR-003 D1 / D2）：设备配对 + 按设备的 session。
//!
//! 没有 loopback 例外、没有 local-token 文件、没有 TOTP、没有限流：配对链接 256 位、单次、
//! 5 分钟，未认证者铸造不了它；session 只存 SHA-256。peer 的机器 token 随 agora-7ku.2 接入。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::clock::age_secs;
use crate::session::{Db, DbError};

pub const COOKIE_NAME: &str = "agora_session";
const TOKEN_BYTES: usize = 32;
/// `last_seen_at` 每小时至多写一次（ADR-003 D2）。
const TOUCH_INTERVAL_SECS: u64 = 3600;

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub pair_ttl: Duration,
    pub pair_pending_max: usize,
    pub session_idle: Duration,
    pub session_max: Duration,
    /// TLS 监听器发放的 cookie 加 `Secure`；明文监听器不加（`127.0.0.1` 是独立 origin）。
    pub cookie_secure: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            pair_ttl: Duration::from_secs(5 * 60),
            pair_pending_max: 4,
            session_idle: Duration::from_secs(30 * 86_400),
            session_max: Duration::from_secs(365 * 86_400),
            cookie_secure: false,
        }
    }
}

/// `authenticate_session` 的结果：认出来的 principal，外加"这次把服务端的滑动窗口往后推了没有"。
/// 后者不是给调用方看热闹的：服务端的 30 天是 `last_seen_at` 的滑动窗口，浏览器那边的 30 天
/// 是 cookie 的 `Max-Age`，两个窗口必须一起走。只在配对时发一次 `Max-Age` 的话，天天在用的人
/// 第 31 天照样被浏览器丢掉 cookie，而服务端还认——这正是 ADR-003 说"滑动"要防的情况。
/// `renewed` 跟着 `last_seen_at` 的每小时至多一次写走，所以重发 cookie 也是每小时至多一次。
#[derive(Debug, Clone)]
pub struct SessionAuth {
    pub principal: Principal,
    pub renewed: bool,
}

/// 每个请求先解析出它；V1 单人，Human 不带用户名（多用户只加字段，ADR-003 D9）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    Human { device: String },
    Peer { name: String },
}

impl Principal {
    /// 日志里的写法：`human:<device>` / `peer:<name>`。
    pub fn log_id(&self) -> String {
        match self {
            Principal::Human { device } => format!("human:{device}"),
            Principal::Peer { name } => format!("peer:{name}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairedVia {
    Socket,
    Session,
}

impl PairedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            PairedVia::Socket => "socket",
            PairedVia::Session => "session",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("unauthenticated")]
    Unauthenticated,
    /// cookie 认证的非 GET 请求 Origin 与 Host 不同源（ADR-003 D7）。
    #[error("cross-origin request rejected")]
    CrossOrigin,
    /// 明文监听器不接受 Bearer（ADR-003 D3）。
    #[error("bearer token requires the TLS listener")]
    BearerRequiresTls,
    /// 未知 / 已用 / 过期，对外不区分；日志里区分。
    #[error("pair token invalid")]
    PairInvalid,
    #[error("too many pending pair links ({0}); use or wait for one to expire")]
    PairPendingLimit(usize),
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for AuthError {
    fn from(e: rusqlite::Error) -> Self {
        AuthError::Db(DbError::Sql(e))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub paired_via: String,
    pub paired_from_addr: Option<String>,
    pub created_at: String,
    pub last_seen_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug)]
struct PendingPair {
    token: String,
    expires_at: Instant,
    via: PairedVia,
}

pub struct Auth {
    db: Arc<Db>,
    cfg: AuthConfig,
    /// 未用的配对链接只在内存：daemon 重启即作废，正好符合"5 分钟"的量级。
    pending: Mutex<Vec<PendingPair>>,
}

impl Auth {
    pub fn new(db: Arc<Db>, cfg: AuthConfig) -> Self {
        Auth {
            db,
            cfg,
            pending: Mutex::new(Vec::new()),
        }
    }

    pub fn config(&self) -> &AuthConfig {
        &self.cfg
    }

    // ---------- 配对链接 ----------

    /// 铸造一条；同时未用的最多 `pair_pending_max` 条。调用方负责"谁能调"——只有 socket 与已认证 session。
    pub fn mint_pair_token(&self, via: PairedVia) -> Result<String, AuthError> {
        let mut pending = lock(&self.pending);
        let now = Instant::now();
        pending.retain(|p| p.expires_at > now);
        if pending.len() >= self.cfg.pair_pending_max {
            return Err(AuthError::PairPendingLimit(self.cfg.pair_pending_max));
        }
        let token = random_token();
        pending.push(PendingPair {
            token: token.clone(),
            expires_at: now + self.cfg.pair_ttl,
            via,
        });
        Ok(token)
    }

    /// `<origin>/#pair=<token>`：token 在 fragment 里，不进服务器日志、不进 Referer。
    pub fn pair_link(origin: &str, token: &str) -> String {
        format!("{}/#pair={token}", origin.trim_end_matches('/'))
    }

    /// 兑换：单次使用。成功 → (device id, 明文 session token)；明文只此一次，库里只有哈希。
    pub fn redeem(
        &self,
        token: &str,
        user_agent: Option<&str>,
        from_addr: Option<&str>,
    ) -> Result<(Device, String), AuthError> {
        let via = {
            let mut pending = lock(&self.pending);
            let now = Instant::now();
            pending.retain(|p| p.expires_at > now);
            match pending.iter().position(|p| p.token == token) {
                Some(i) => pending.swap_remove(i).via,
                None => {
                    // 已用的链接再次出现是"有人偷看了链接"的信号（ADR-003 D2）。
                    tracing::warn!(component = "auth", from = ?from_addr, "配对失败：链接未知、已用或已过期");
                    return Err(AuthError::PairInvalid);
                }
            }
        };
        let session = random_token();
        let id = random_id();
        let name = device_name(user_agent.unwrap_or(""));
        self.db.conn().execute(
            "INSERT INTO devices (id, name, session_sha256, paired_via, paired_from_addr,
                created_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            params![id, name, sha256_hex(&session), via.as_str(), from_addr],
        )?;
        tracing::info!(component = "auth", device = %id, via = via.as_str(), from = ?from_addr, "设备已配对");
        let device = self
            .device(&id)?
            .ok_or_else(|| AuthError::DeviceNotFound(id.clone()))?;
        Ok((device, session))
    }

    // ---------- session ----------

    /// cookie 里的明文 → Principal。吊销即时、距最近使用 30 天、距配对 365 天。
    pub fn authenticate_session(&self, token: &str) -> Result<SessionAuth, AuthError> {
        let hash = sha256_hex(token);
        let row: Option<(String, String, String, Option<String>)> = self
            .db
            .conn()
            .query_row(
                "SELECT id, created_at, last_seen_at, revoked_at FROM devices
                 WHERE session_sha256 = ?1",
                [&hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((id, created_at, last_seen_at, revoked_at)) = row else {
            return Err(AuthError::Unauthenticated);
        };
        if revoked_at.is_some() {
            return Err(AuthError::Unauthenticated);
        }
        // 解析失败按"过期"处理：时间读不出来的 session 不该继续有效。
        let idle = age_secs(&last_seen_at).unwrap_or(u64::MAX);
        let age = age_secs(&created_at).unwrap_or(u64::MAX);
        if idle > self.cfg.session_idle.as_secs() || age > self.cfg.session_max.as_secs() {
            tracing::info!(component = "auth", device = %id, idle, age, "session 已过期");
            return Err(AuthError::Unauthenticated);
        }
        let touched = idle >= TOUCH_INTERVAL_SECS;
        if touched {
            self.db.conn().execute(
                "UPDATE devices SET last_seen_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1",
                [&id],
            )?;
        }
        Ok(SessionAuth {
            principal: Principal::Human { device: id },
            renewed: touched,
        })
    }

    // ---------- 设备管理 ----------

    pub fn list_devices(&self) -> Result<Vec<Device>, AuthError> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(&format!("{SELECT_DEVICE} ORDER BY created_at, id"))?;
        let rows = stmt.query_map([], row_to_device)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn device(&self, id: &str) -> Result<Option<Device>, AuthError> {
        Ok(self
            .db
            .conn()
            .query_row(
                &format!("{SELECT_DEVICE} WHERE id = ?1"),
                [id],
                row_to_device,
            )
            .optional()?)
    }

    /// 吊销即时生效：每次请求都查库，不缓存。
    pub fn revoke(&self, id: &str) -> Result<(), AuthError> {
        let n = self.db.conn().execute(
            "UPDATE devices SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1 AND revoked_at IS NULL",
            [id],
        )?;
        if n == 0 && self.device(id)?.is_none() {
            return Err(AuthError::DeviceNotFound(id.into()));
        }
        tracing::info!(component = "auth", device = %id, "设备已吊销");
        Ok(())
    }

    pub fn revoke_all(&self) -> Result<usize, AuthError> {
        Ok(self.db.conn().execute(
            "UPDATE devices SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE revoked_at IS NULL",
            [],
        )?)
    }

    /// 从配对时刻起算的秒数，测试与 CLI 展示用。
    pub fn pending_count(&self) -> usize {
        let mut pending = lock(&self.pending);
        let now = Instant::now();
        pending.retain(|p| p.expires_at > now);
        pending.len()
    }
}

const SELECT_DEVICE: &str = "SELECT id, name, paired_via, paired_from_addr, created_at,
    last_seen_at, revoked_at FROM devices";

fn row_to_device(r: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get(0)?,
        name: r.get(1)?,
        paired_via: r.get(2)?,
        paired_from_addr: r.get(3)?,
        created_at: r.get(4)?,
        last_seen_at: r.get(5)?,
        revoked_at: r.get(6)?,
    })
}

// ---------- 原语 ----------

/// 32 字节 CSPRNG → base64url 无填充（43 字符）。
pub fn random_token() -> String {
    let mut buf = [0u8; TOKEN_BYTES];
    // 系统随机源失败是不可恢复的环境错误：宁可 panic 也不发弱 token。
    getrandom::fill(&mut buf).expect("系统随机源不可用");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn random_id() -> String {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).expect("系统随机源不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从 User-Agent 生成一个人能认的名字；可在设备列表里改名（V2-1）。
pub fn device_name(ua: &str) -> String {
    let browser = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("OPR/") {
        "Opera"
    } else if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Chrome/") || ua.contains("CriOS/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else if ua.contains("curl/") {
        "curl"
    } else {
        "browser"
    };
    let os = if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "unknown OS"
    };
    format!("{browser} on {os}")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_256_bit_urlsafe() {
        let t = random_token();
        assert_eq!(t.len(), 43);
        assert!(t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(t, random_token());
    }

    #[test]
    fn names_from_user_agent() {
        assert_eq!(
            device_name("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36"),
            "Chrome on macOS"
        );
        assert_eq!(device_name(""), "browser on unknown OS");
    }

    #[test]
    fn pair_link_puts_token_in_fragment() {
        assert_eq!(
            Auth::pair_link("http://127.0.0.1:7680/", "abc"),
            "http://127.0.0.1:7680/#pair=abc"
        );
    }
}
