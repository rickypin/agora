//! 机器 token：节点作为 peer 客户端的凭据（ADR-003 D3；agora-7ku.2）。
//!
//! 形态 `apt_<name>_<base64url(32 字节随机)>`：前缀让日志与 secret scanner 认得出它，name 段让
//! 服务端按名字查一条记录再做常量时间比对。被访问的节点只存整串的 SHA-256（`peer_tokens` 表，
//! schema v4）；明文只在 `create` 返回值里出现一次，由人写进对方节点的 `peers[].token_file`。
//!
//! 校验（[`authenticate`]）：查 name → 常量时间比对 → 未吊销 → `Peer { name }`。没有签发过任何
//! token 的节点自然拒绝一切 Bearer（表里没有行就没有能匹配的哈希，不需要额外开关，A31）；吊销
//! 即时（每请求查一次库，不缓存）。"Bearer 只在 TLS 监听器上被接受"不在这里——那是
//! `api::auth` 的提取器按 [`crate::api::TlsListener`] 标记判的，本模块只认 token 本身。
//!
//! 持有方那一半（[`load_token_file`]）：`token_file` 明文、`0600`、属于当前用户；任何一条不满足
//! 都是**配置错误**，不是"离线"，也不该进退避重试——重试不会让文件权限变对。

use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::{random_token, sha256_hex, AuthError};
use crate::clock::age_secs;
use crate::session::{Db, DbError};

/// token 前缀（"agora peer token"）。
pub const PREFIX: &str = "apt_";
/// 32 字节 base64url 无填充恰好 43 字符；解析靠这个定长，见 [`parse`]。
const SECRET_LEN: usize = 43;
/// peer 名最长多少字符；字符集与 `node.id` 相同（字母、数字、`-`、`_`）。
const NAME_MAX: usize = 64;
/// `last_used_at` 每小时至多写一次，与 `devices.last_seen_at` 同一节拍（ADR-003 D2）。
const LAST_USED_INTERVAL_SECS: u64 = 3600;

/// 签发 / 吊销 / 列表的失败；HTTP 校验的失败是 [`AuthError`]，对外一律 401 不区分原因。
#[derive(Debug, thiserror::Error)]
pub enum PeerTokenError {
    #[error("peer 名 {0:?} 不合法：1–64 个字母、数字、- 或 _")]
    InvalidName(String),
    #[error("peer {0} 已有有效的 token；要换新的用 --rotate（旧的立即失效）")]
    Exists(String),
    #[error("没有为 peer {0} 签发过 token")]
    NotFound(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for PeerTokenError {
    fn from(e: rusqlite::Error) -> Self {
        PeerTokenError::Db(DbError::Sql(e))
    }
}

/// `peer_tokens` 一行，给 `agora peer token list` 与测试看；没有哈希列——列表不需要它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerToken {
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

// ---------- 形态 ----------

/// 与 `node.id` 同一字符集：token 里 name 段两侧都是 `_`，放开字符集只会让解析更难。
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `apt_<name>_<43 字符>` → name。
///
/// base64url 字母表本身含 `_`，peer 名也允许 `_`（`node.id` 就允许），所以不能按 `_` 切：
/// `apt_a_b_XXXX…` 既可以是 name=`a_b`，也可以是 name=`a` 而 secret 以 `b_` 开头。定长的
/// 随机段解决它——最后 43 字符永远是 secret，再前一个字符必须是 `_`，剩下的是 name
/// （2026-09-06；反例：按 `rsplit_once('_')` 切，name=`a` 的 token 若 secret 含 `_` 就会切错）。
pub fn parse(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(PREFIX)?;
    if rest.len() <= SECRET_LEN + 1 {
        return None;
    }
    let cut = rest.len() - SECRET_LEN;
    let (head, secret) = rest.split_at(cut);
    let name = head.strip_suffix('_')?;
    let b64url = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    (is_valid_name(name) && secret.chars().all(b64url)).then_some(name)
}

fn mint(name: &str) -> String {
    format!("{PREFIX}{name}_{}", random_token())
}

/// 常量时间比对两串哈希：长度不同直接假（长度不是秘密），否则逐字节 XOR 累加，不提前返回。
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

// ---------- 签发 / 吊销 / 列表（CLI 直接操作库，不需要 daemon 在跑，ADR-003 D6） ----------

/// 签发。返回明文，只此一次；库里只有哈希。已有有效 token 时不加 `rotate` 拒绝——防止手滑把
/// 正在用的 peer 换掉；加了就换新并让旧的立即失效（同一行换哈希，旧的从此匹配不上）。
/// 已吊销的 name 直接重发，不需要 `rotate`：吊销后再签就是正常流程。
pub fn create(db: &Db, name: &str, rotate: bool) -> Result<String, PeerTokenError> {
    if !is_valid_name(name) {
        return Err(PeerTokenError::InvalidName(name.to_owned()));
    }
    let conn = db.conn();
    let existing: Option<Option<String>> = conn
        .query_row(
            "SELECT revoked_at FROM peer_tokens WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    if matches!(existing, Some(None)) && !rotate {
        return Err(PeerTokenError::Exists(name.to_owned()));
    }
    let token = mint(name);
    conn.execute(
        "INSERT OR REPLACE INTO peer_tokens (name, token_sha256, created_at, last_used_at, revoked_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ','now'), NULL, NULL)",
        params![name, sha256_hex(&token)],
    )?;
    tracing::info!(
        component = "auth",
        peer = name,
        replaced = existing.is_some(),
        "已签发机器 token"
    );
    Ok(token)
}

/// 吊销即时生效：下一次请求查库就查不到未吊销的行。已吊销的再吊销是 no-op；没签过 → NotFound。
pub fn revoke(db: &Db, name: &str) -> Result<(), PeerTokenError> {
    let conn = db.conn();
    let n = conn.execute(
        "UPDATE peer_tokens SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
         WHERE name = ?1 AND revoked_at IS NULL",
        [name],
    )?;
    if n == 0 {
        let exists: Option<i64> = conn
            .query_row("SELECT 1 FROM peer_tokens WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(PeerTokenError::NotFound(name.to_owned()));
        }
    }
    tracing::info!(component = "auth", peer = name, "机器 token 已吊销");
    Ok(())
}

pub fn list(db: &Db) -> Result<Vec<PeerToken>, PeerTokenError> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT name, created_at, last_used_at, revoked_at FROM peer_tokens ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PeerToken {
            name: r.get(0)?,
            created_at: r.get(1)?,
            last_used_at: r.get(2)?,
            revoked_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------- 校验（每请求一次） ----------

/// `Authorization` 头的值 → peer 名。scheme 必须是 Bearer（大小写不敏感，RFC 7235）；其余任何
/// 失败对外都是 `Unauthenticated`，原因只进日志。已吊销的 token 再出现是"持有方被攻破或换了
/// 机器没清文件"的信号，日志级别 warn。
pub fn authenticate(db: &Db, authorization: &str) -> Result<String, AuthError> {
    let token = match authorization.trim().split_once(char::is_whitespace) {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => rest.trim(),
        _ => {
            tracing::warn!(component = "auth", "Authorization 不是 Bearer，拒绝");
            return Err(AuthError::Unauthenticated);
        }
    };
    let Some(name) = parse(token) else {
        tracing::warn!(component = "auth", "Bearer 不是机器 token 的形态，拒绝");
        return Err(AuthError::Unauthenticated);
    };
    let conn = db.conn();
    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT token_sha256, last_used_at, revoked_at FROM peer_tokens WHERE name = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((hash, last_used_at, revoked_at)) = row else {
        tracing::warn!(
            component = "auth",
            peer = name,
            "没有为这个 peer 签发过 token，拒绝"
        );
        return Err(AuthError::Unauthenticated);
    };
    if !ct_eq(&sha256_hex(token), &hash) {
        tracing::warn!(component = "auth", peer = name, "机器 token 不匹配，拒绝");
        return Err(AuthError::Unauthenticated);
    }
    if revoked_at.is_some() {
        tracing::warn!(
            component = "auth",
            peer = name,
            "已吊销的机器 token 被使用，拒绝"
        );
        return Err(AuthError::Unauthenticated);
    }
    // 解析不出的时间按"很久没用"处理：多写一次无害。
    let stale = last_used_at
        .as_deref()
        .and_then(age_secs)
        .is_none_or(|age| age >= LAST_USED_INTERVAL_SECS);
    if stale {
        conn.execute(
            "UPDATE peer_tokens SET last_used_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE name = ?1",
            [name],
        )?;
    }
    Ok(name.to_owned())
}

// ---------- 持有方：token_file ----------

/// 读 `peers[].token_file` 的失败。**每一种都是配置错误**：该 peer 在状态模型里应显示为
/// 「配置错误」而不是"离线 / 未授权"，也不进退避（重试改不了文件）。改好文件后重载才会好。
#[derive(Debug, thiserror::Error)]
pub enum TokenFileError {
    #[error("token_file {path} 读不到: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("token_file {path} 属于 uid {owner}，不是当前用户 {me}")]
    WrongOwner { path: String, owner: u32, me: u32 },
    #[error("token_file {path} 权限过宽（{mode:03o}）：group / other 不得有任何位；请执行 chmod 600 {path}")]
    TooOpen { path: String, mode: u32 },
    #[error("token_file {path} 的内容不是机器 token（应为一行 apt_<name>_<43 字符>）")]
    Malformed { path: String },
}

/// 校验权限再读明文：属于当前 uid、group / other 没有任何位（与 `AGORA_HOME` 自检同一尺度，
/// ADR-003 D6），内容去掉首尾空白后必须是合法 token 形态。返回明文 token；调用方只拿它填
/// `Authorization` 头，不落日志。
pub fn load_token_file(path: &Path) -> Result<String, TokenFileError> {
    use std::os::unix::fs::MetadataExt;
    let display = path.display().to_string();
    let meta = std::fs::metadata(path).map_err(|source| TokenFileError::Read {
        path: display.clone(),
        source,
    })?;
    // SAFETY: getuid 没有前置条件、不会失败。
    let me = unsafe { libc::getuid() };
    if meta.uid() != me {
        return Err(TokenFileError::WrongOwner {
            path: display,
            owner: meta.uid(),
            me,
        });
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(TokenFileError::TooOpen {
            path: display,
            mode,
        });
    }
    let text = std::fs::read_to_string(path).map_err(|source| TokenFileError::Read {
        path: display.clone(),
        source,
    })?;
    let token = text.trim();
    if parse(token).is_none() {
        return Err(TokenFileError::Malformed { path: display });
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_is_prefix_name_underscore_43() {
        let t = mint("zuan");
        assert!(t.starts_with("apt_zuan_"));
        assert_eq!(t.len(), PREFIX.len() + "zuan".len() + 1 + SECRET_LEN);
        assert_eq!(parse(&t), Some("zuan"));
        assert_ne!(mint("zuan"), t);
    }

    #[test]
    fn names_with_underscores_parse_back_exactly() {
        // 定长切分：name 含 `_`、secret 含 `_` 都不会切错。
        for name in ["a_b", "a", "_", "zuan-2_x", "A_"] {
            let t = mint(name);
            assert_eq!(parse(&t), Some(name), "{t}");
        }
        let secret_with_underscores = "_".repeat(SECRET_LEN);
        assert_eq!(
            parse(&format!("apt_a_{secret_with_underscores}")),
            Some("a")
        );
    }

    #[test]
    fn malformed_tokens_do_not_parse() {
        let secret = "x".repeat(SECRET_LEN);
        let short = &secret[1..];
        for bad in [
            String::new(),
            "apt_".into(),
            "apt__".into(),
            format!("apt_{secret}"),       // 没有 name
            format!("apt__{secret}"),      // 空 name
            format!("apt_zuan{secret}"),   // name 与 secret 之间没有 `_`
            format!("apt_zuan_{short}"),   // secret 短一位
            format!("apt_zu an_{secret}"), // name 含空格
            format!("apt_zuan_{short}="),  // 带填充
            format!("APT_zuan_{secret}"),  // 前缀大小写
        ] {
            assert_eq!(parse(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn constant_time_eq_is_plain_equality() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        assert!(ct_eq("", ""));
    }
}
